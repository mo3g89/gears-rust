#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit tests for `VariablesService::upsert`'s CREATE/UPDATE scope selection
//! and the environment tenancy precheck ordering.
//!
//! These specifically guard against a reviewed regression: the natural-key
//! existence probe in `upsert` must never run before the environment tenancy
//! precheck, and must never be able to observe rows outside the caller's own
//! tenant (see `find_by_natural_key`'s doc comment and
//! `find_environment_var`'s tenant predicate).
//!
//! `RecordingAuthZ` wraps the same permissive decision logic as
//! [`super::test_support::PermissiveAuthZ`] but additionally records every
//! `(action, resource_id)` pair the service requested, so these tests can
//! assert *which* PEP action was requested — not just the end result.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::models::{EvaluationRequest, EvaluationResponse};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, PolicyEnforcer};
use qa_environments_sdk::{NewVariable, RESERVED_VARIABLE_NAMES, Variable};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::test_support::{MockEnvironmentsRepository, ctx, permissive_response, test_db_provider};
use super::{VariablesService, actions};
use crate::domain::error::DomainError;
use crate::domain::repos::VariablesRepository;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// `VariablesRepository` double. `find_by_natural_key` returns a configurable
/// "existing" row (or `None`) and counts how many times it was called — the
/// oracle regression test asserts this count is zero when the environment
/// precheck should have short-circuited before the probe.
#[derive(Default)]
struct MockVariablesRepository {
    existing: Option<Variable>,
    find_calls: Mutex<usize>,
}

impl MockVariablesRepository {
    fn with_existing(var: Variable) -> Self {
        Self {
            existing: Some(var),
            find_calls: Mutex::new(0),
        }
    }

    fn empty() -> Self {
        Self::default()
    }

    fn find_call_count(&self) -> usize {
        *self.find_calls.lock().unwrap()
    }
}

#[async_trait]
impl VariablesRepository for MockVariablesRepository {
    async fn list_pipeline<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
    ) -> Result<Vec<Variable>, DomainError> {
        unimplemented!("not exercised by the upsert-scope unit tests")
    }

    async fn list_for_environment<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _environment_id: Uuid,
    ) -> Result<Vec<Variable>, DomainError> {
        unimplemented!("not exercised by the upsert-scope unit tests")
    }

    async fn find_by_natural_key<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        _environment_id: Option<Uuid>,
        _name: &str,
    ) -> Result<Option<Variable>, DomainError> {
        *self.find_calls.lock().unwrap() += 1;
        Ok(self.existing.clone())
    }

    async fn upsert<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        var: NewVariable,
    ) -> Result<Variable, DomainError> {
        let id = self.existing.as_ref().map_or_else(Uuid::new_v4, |v| v.id);
        Ok(Variable {
            id,
            environment_id: var.environment_id,
            name: var.name,
            value: var.value,
        })
    }

    async fn delete<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
    ) -> Result<bool, DomainError> {
        unimplemented!("not exercised by the upsert-scope unit tests")
    }
}

/// Wraps [`super::test_support::permissive_response`] (always grants access)
/// while recording every `(action, resource_id)` pair requested, so tests can
/// assert which PEP action the service asked for.
#[derive(Default)]
struct RecordingAuthZ {
    requests: Mutex<Vec<(String, Option<Uuid>)>>,
}

impl RecordingAuthZ {
    fn new() -> Self {
        Self::default()
    }

    fn requested(&self, action: &str, resource_id: Option<Uuid>) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .any(|(a, id)| a == action && *id == resource_id)
    }
}

#[async_trait]
impl AuthZResolverClient for RecordingAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        self.requests
            .lock()
            .unwrap()
            .push((request.action.name.clone(), request.resource.id));
        Ok(permissive_response(&request))
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Generous cap for tests that don't exercise the `max_variables` truncation
/// behavior (see `tests_tenant_scoping::list_for_env_caps_at_max_variables`
/// for a dedicated test of that behavior).
const UNLIMITED_FOR_TESTS: usize = usize::MAX;

async fn build_service(
    variables_repo: Arc<MockVariablesRepository>,
    environments_repo: Arc<MockEnvironmentsRepository>,
    authz: Arc<RecordingAuthZ>,
) -> VariablesService<MockVariablesRepository, MockEnvironmentsRepository> {
    let enforcer = PolicyEnforcer::new(authz);
    let db = test_db_provider().await;
    VariablesService::new(
        db,
        variables_repo,
        environments_repo,
        enforcer,
        UNLIMITED_FOR_TESTS,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upsert_existing_row_requests_update_scope() {
    let tenant_id = Uuid::new_v4();
    let existing_id = Uuid::new_v4();

    let variables_repo = Arc::new(MockVariablesRepository::with_existing(Variable {
        id: existing_id,
        environment_id: None,
        name: "FOO".to_owned(),
        value: "old".to_owned(),
    }));
    let environments_repo = Arc::new(MockEnvironmentsRepository::none());
    let authz = Arc::new(RecordingAuthZ::new());
    let svc = build_service(variables_repo.clone(), environments_repo, authz.clone()).await;

    let result = svc
        .upsert(
            &ctx(tenant_id),
            NewVariable {
                environment_id: None,
                name: "FOO".to_owned(),
                value: "new".to_owned(),
            },
        )
        .await
        .unwrap();

    assert_eq!(result.id, existing_id);
    assert_eq!(variables_repo.find_call_count(), 1);
    assert!(
        authz.requested(actions::UPDATE, Some(existing_id)),
        "expected an UPDATE request for the existing row's id; requests were: {:?}",
        authz.requests.lock().unwrap()
    );
    assert!(
        !authz.requested(actions::CREATE, None),
        "must not also request CREATE"
    );
}

#[tokio::test]
async fn upsert_new_row_requests_create_scope() {
    let tenant_id = Uuid::new_v4();

    let variables_repo = Arc::new(MockVariablesRepository::empty());
    let environments_repo = Arc::new(MockEnvironmentsRepository::none());
    let authz = Arc::new(RecordingAuthZ::new());
    let svc = build_service(variables_repo.clone(), environments_repo, authz.clone()).await;

    let _ = svc
        .upsert(
            &ctx(tenant_id),
            NewVariable {
                environment_id: None,
                name: "BAR".to_owned(),
                value: "value".to_owned(),
            },
        )
        .await
        .unwrap();

    assert_eq!(variables_repo.find_call_count(), 1);
    assert!(
        authz.requested(actions::CREATE, None),
        "expected a CREATE request with no resource id; requests were: {:?}",
        authz.requests.lock().unwrap()
    );
    assert!(
        !authz
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(a, _)| a == actions::UPDATE),
        "must not also request UPDATE"
    );
}

/// Regression test for the cross-tenant existence oracle: a foreign
/// `environment_id` (one the caller's environments repo can't see) must 404 from
/// the tenancy precheck *before* the natural-key probe ever runs. If the
/// probe ran first, its result (found/not-found) would leak information
/// about an `environment_id` the caller isn't authorized to know about.
#[tokio::test]
async fn upsert_foreign_environment_is_not_found_before_probe() {
    let tenant_id = Uuid::new_v4();
    let foreign_environment_id = Uuid::new_v4();

    let variables_repo = Arc::new(MockVariablesRepository::empty());
    // The environments mock has no environment registered, so any `get(id)`
    // returns `None` regardless of `id` — simulating an environment_id the
    // caller cannot see (wrong tenant, or nonexistent).
    let environments_repo = Arc::new(MockEnvironmentsRepository::none());
    let authz = Arc::new(RecordingAuthZ::new());
    let svc = build_service(variables_repo.clone(), environments_repo, authz.clone()).await;

    let err = svc
        .upsert(
            &ctx(tenant_id),
            NewVariable {
                environment_id: Some(foreign_environment_id),
                name: "BAZ".to_owned(),
                value: "value".to_owned(),
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::EnvironmentNotFound { id } if id == foreign_environment_id),
        "expected EnvironmentNotFound, got {err:?}"
    );
    assert_eq!(
        variables_repo.find_call_count(),
        0,
        "the natural-key probe must never run before the environment precheck rejects a foreign environment_id"
    );
}

// ---------------------------------------------------------------------------
// Reserved-name refusal (Task 11b)
// ---------------------------------------------------------------------------
//
// The source system refuses these names on all three environment write paths;
// this gear owns two of them (pipeline and environment variables, both reaching
// `upsert`). See `RESERVED_VARIABLE_NAMES` for why the refusal is load-bearing
// rather than tidiness: `RP_API_KEY` reaches the runner as a secret reference,
// and a variable of that name would replace it with operator-supplied text.

/// Every reserved name is refused, on both the pipeline (`environment_id: None`)
/// and the per-environment path — the two paths this gear owns.
///
/// Swept rather than spot-checked on one name: a check written as an
/// `== "RP_API_KEY"` comparison would satisfy a single-name test and leave the
/// other ten open.
///
/// The second assertion does a second job: it proves every reserved name
/// reaches the *reserved* refusal rather than being rejected earlier as
/// malformed, which is the property that makes `validate_name`'s check order
/// unobservable — see its doc.
///
/// It matches the refusal's own wording rather than relying on "the charset
/// message happens not to contain the name". That is true today and is not a
/// property anything defends: **legacy's charset message does name the
/// variable** (`../testrunner/manager/src/routes/settings.rs:73-76`), so
/// "make our charset error name it too" is a plausible and obviously-correct
/// future edit — one that would leave a name-only assertion passing while it
/// silently stopped proving anything.
#[tokio::test]
async fn upsert_refuses_every_reserved_name_on_both_paths() {
    let tenant_id = Uuid::new_v4();
    let environment_id = Uuid::new_v4();

    for reserved in RESERVED_VARIABLE_NAMES {
        for scope in [None, Some(environment_id)] {
            let variables_repo = Arc::new(MockVariablesRepository::empty());
            let environments_repo = Arc::new(MockEnvironmentsRepository::none());
            let authz = Arc::new(RecordingAuthZ::new());
            let svc = build_service(variables_repo.clone(), environments_repo, authz).await;

            let err = svc
                .upsert(
                    &ctx(tenant_id),
                    NewVariable {
                        environment_id: scope,
                        name: reserved.to_owned(),
                        value: "hijack".to_owned(),
                    },
                )
                .await
                .unwrap_err();

            assert!(
                matches!(&err, DomainError::Validation { field, message }
                    if field == "name"
                        && message.contains(reserved)
                        && message.contains("is reserved by the test runner")),
                "expected the reserved-name refusal to name {reserved}, got {err:?}"
            );
            assert_eq!(
                variables_repo.find_call_count(),
                0,
                "{reserved} must be refused before any I/O, as the source system \
                 validates the whole list up front"
            );
        }
    }
}

/// Case-insensitive, matching the source system's `eq_ignore_ascii_case`
/// (`../testrunner/manager/src/routes/settings.rs:80-88`).
///
/// A lowercase `rp_api_key` would not actually shadow anything in the runner's
/// shell, which is case-sensitive — the refusal is of the *intent*, and the
/// alternative (accepting it) leaves a variable that can never reach a run.
#[tokio::test]
async fn upsert_refuses_a_reserved_name_in_any_casing() {
    let tenant_id = Uuid::new_v4();

    for spelling in ["rp_api_key", "Rp_Api_Key", "rP_aPi_KeY"] {
        let variables_repo = Arc::new(MockVariablesRepository::empty());
        let environments_repo = Arc::new(MockEnvironmentsRepository::none());
        let authz = Arc::new(RecordingAuthZ::new());
        let svc = build_service(variables_repo, environments_repo, authz).await;

        let err = svc
            .upsert(
                &ctx(tenant_id),
                NewVariable {
                    environment_id: None,
                    name: spelling.to_owned(),
                    value: "hijack".to_owned(),
                },
            )
            .await
            .unwrap_err();

        assert!(
            matches!(&err, DomainError::Validation { field, message }
                if field == "name"
                    && message.contains(spelling)
                    && message.contains("is reserved by the test runner")),
            "expected {spelling} to be refused as reserved, got {err:?}"
        );
    }
}

/// The refusal must not swallow ordinary names. Without this, a check that
/// rejected everything would satisfy both tests above.
#[tokio::test]
async fn upsert_still_accepts_a_name_that_merely_resembles_a_reserved_one() {
    let tenant_id = Uuid::new_v4();

    for allowed in ["RP_API_KEY_2", "MY_RP_API_KEY", "TEST_FILE", "APP_BUILDS"] {
        let variables_repo = Arc::new(MockVariablesRepository::empty());
        let environments_repo = Arc::new(MockEnvironmentsRepository::none());
        let authz = Arc::new(RecordingAuthZ::new());
        let svc = build_service(variables_repo, environments_repo, authz).await;

        let result = svc
            .upsert(
                &ctx(tenant_id),
                NewVariable {
                    environment_id: None,
                    name: allowed.to_owned(),
                    value: "fine".to_owned(),
                },
            )
            .await;

        assert!(result.is_ok(), "{allowed} must be accepted, got {result:?}");
    }
}
