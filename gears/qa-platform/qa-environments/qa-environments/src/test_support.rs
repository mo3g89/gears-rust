//! Test-only helpers for exercising the qa-environments gear against a real
//! in-memory `SQLite` database, wired with the gear's actual migrations and
//! `SeaORM`-backed repositories.
//!
//! This complements `domain::service::test_support`, which provides mock
//! repository doubles for pure service-logic unit tests. This module is the
//! DB-backed tier: [`domain::service::tests_tenant_scoping`] uses it to
//! verify that `SecureORM` row-level scoping (not just service-layer
//! plumbing) actually isolates tenants.
//!
//! Reference: `examples/toolkit/users-info/users-info/src/test_support.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverClient;
use authz_resolver_sdk::AuthZResolverError;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use credstore_sdk::{
    CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef, SecretType, SecretValue,
    SharingMode, TenantId, WriteOptions, WritePrecondition,
};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, Db, DbError, connect_db};
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use qa_product_sdk::access::{RunAccess, RunVarContract, RunnerSpec};
use qa_product_sdk::descriptor::{FieldDesc, FieldKind, FieldRole};
use qa_product_sdk::observation::{ObservedAttrs, PluginFailure, PluginObservation};
use qa_product_sdk::plugin::{
    CredentialClassification, CredentialInput, EnvironmentHandle, QaProductPluginV1,
};

use crate::domain::ports::{
    NoopRunnerSecretWriter, PluginUnavailable, ProductPluginPort, RunnerSecretWriter,
};
use crate::domain::service::AppServices;
use crate::gear::ConcreteAppServices;
use crate::infra::storage::{
    OrmEnvironmentsRepository, OrmLeasesRepository, OrmVariablesRepository,
};

/// Build a `SecurityContext` for `tenant_id` with a fresh random subject.
pub fn ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap()
}

/// Create an in-memory `SQLite` database and run the gear's REAL migrations
/// (`crate::infra::storage::migrations::Migrator`) against it.
pub async fn inmem_db() -> Db {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("failed to connect to in-memory sqlite database");

    run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .expect("failed to run qa-environments migrations");

    db
}

/// Shared decision logic for the permissive `AuthZ` test doubles used across
/// BOTH test tiers in this crate: the mock-repository unit tests
/// (`domain::service::test_support::PermissiveAuthZ`,
/// `variables_tests::RecordingAuthZ`) and this DB-backed tier
/// (`TenantScopedAuthZ` below). Always grants access, returning a tenant
/// `IN` constraint derived from the subject's tenant (mirrors a real PDP's
/// default tenant-isolation policy). Tenant is resolved from the explicit
/// PEP tenant context first, falling back to the subject's `tenant_id`
/// property (like a real PDP); nil UUIDs are treated as anonymous/unset.
///
/// The mock-repository tier's doubles ignore the resulting `AccessScope`
/// entirely — those mocks never touch a database — but the PEP flow still
/// needs a well-formed PDP response to compile one. Here, under
/// [`TenantScopedAuthZ`], the resulting scope IS applied against a REAL
/// `SQLite` database via `SecureORM`'s `.secure().scope_with(scope)`, so an
/// incorrect or overly-permissive scope shows up as a tenant-isolation test
/// failure instead of being silently absorbed by an in-memory mock.
pub fn permissive_response(request: &EvaluationRequest) -> EvaluationResponse {
    let root_id = request
        .context
        .tenant_context
        .as_ref()
        .and_then(|tc| tc.root_id)
        .or_else(|| {
            request
                .subject
                .properties
                .get("tenant_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        })
        .filter(|id| !id.is_nil());

    let constraints = match root_id {
        Some(id) => vec![Constraint {
            predicates: vec![Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [id],
            ))],
        }],
        None => vec![],
    };

    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints,
            ..Default::default()
        },
    }
}

/// `AuthZ` test double whose decisions compile to REAL tenant-scoped
/// `AccessScope`s: it always grants access and returns a PDP-shaped
/// `Constraint` restricting to the requesting subject's own tenant
/// (`owner_tenant_id IN [subject_tenant]`) — exactly the shape
/// `PolicyEnforcer::access_scope` compiles into row-level SQL filtering via
/// `SecureORM`'s `.secure().scope_with(scope)`.
///
/// Every service method in this gear calls `PolicyEnforcer::access_scope`
/// (never `access_scope_with(require_constraints(false))`), so the PDP is
/// always asked for constraints; this double always supplies them.
///
/// See [`permissive_response`] for the shared decision logic (also used by
/// the mock-repository test tier).
pub struct TenantScopedAuthZ;

#[async_trait]
impl AuthZResolverClient for TenantScopedAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(permissive_response(&request))
    }
}

/// `AuthZ` test double that wraps `TenantScopedAuthZ`'s decision logic
/// (always grants, tenant-scoped constraint) while recording every
/// `(action, resource_id)` pair requested, so a test can assert *which* PEP
/// action a service method asked for -- not just whether it succeeded.
/// Mirrors `domain::service::variables_tests::RecordingAuthZ`, which is
/// private to that module; this one is crate-visible so REST-handler-level
/// tests (`api::rest::handlers::environments`) can use it too, via
/// `build_services_full`'s `authz` parameter.
#[derive(Default)]
pub struct RecordingAuthZ {
    requests: Mutex<Vec<(String, Option<Uuid>)>>,
}

impl RecordingAuthZ {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `action` was requested for `resource_id` at least once.
    #[must_use]
    pub fn requested(&self, action: &str, resource_id: Option<Uuid>) -> bool {
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

/// `AuthZ` test double that always denies (`decision=false`) — the canonical
/// PDP-deny path. The enforcer converts this into `EnforcerError::Denied`,
/// which `DomainError::from` maps to `DomainError::Forbidden`.
pub struct DenyAllAuthZ;

#[async_trait]
impl AuthZResolverClient for DenyAllAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// In-memory [`CredStoreClientV1`] double: a real `reference -> (bytes,
/// sharing)` store whose `get` works.
///
/// A write-only double (like `qa-catalog`'s, which leaves `get`
/// `unimplemented!`) can only prove that a write was *attempted*. Reading the
/// document back out through the client trait — under the very reference the
/// database row ended up holding — is what proves the paste is actually
/// recoverable, which is the whole point of storing it. `qa-catalog`'s
/// `ssh_keys_tests` peeks into the mock's map instead; going through `get`
/// costs ten more lines and exercises one more real edge.
pub struct RecordingCredStore {
    secrets: Mutex<HashMap<String, StoredSecret>>,
    /// When true every operation fails with `CredStoreError::Internal` — the
    /// only way to reach the "credstore write failed, so no row exists" path.
    failing: bool,
    /// When set, the first *n* **creates** succeed and every later one fails.
    ///
    /// This is how a *partial* n-ary write is reached: one credential minted,
    /// the next refused. `delete` deliberately does not count against it,
    /// because the compensating cleanup that runs afterwards is the thing
    /// under test and a store that refused to delete would hide the defect
    /// rather than prove the fix.
    fail_creates_after: Option<usize>,
    /// Creates attempted so far, against `fail_creates_after`.
    creates: Mutex<usize>,
}

/// One entry of [`RecordingCredStore`]'s store.
struct StoredSecret {
    bytes: Vec<u8>,
    sharing: SharingMode,
}

impl RecordingCredStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            secrets: Mutex::new(HashMap::new()),
            failing: false,
            fail_creates_after: None,
            creates: Mutex::new(0),
        }
    }

    /// Every operation fails. Used to prove a failed credstore write creates
    /// no environment row.
    #[must_use]
    pub fn always_failing() -> Self {
        Self {
            secrets: Mutex::new(HashMap::new()),
            failing: true,
            fail_creates_after: None,
            creates: Mutex::new(0),
        }
    }

    /// The first `n` creates succeed; every later one fails.
    ///
    /// Used to reach a write that stored some of an environment's credentials
    /// and then failed, which is the state review finding IMPORTANT-1's
    /// compensating cleanup exists to unwind.
    #[must_use]
    pub fn failing_creates_after(n: usize) -> Self {
        Self {
            secrets: Mutex::new(HashMap::new()),
            failing: false,
            fail_creates_after: Some(n),
            creates: Mutex::new(0),
        }
    }

    /// Number of secrets currently held — an orphan check.
    #[must_use]
    pub fn len(&self) -> usize {
        self.secrets.lock().unwrap().len()
    }

    /// Every reference currently held, for assertions about *which* secrets
    /// survived an update or delete.
    #[must_use]
    pub fn references(&self) -> Vec<String> {
        let mut refs: Vec<String> = self.secrets.lock().unwrap().keys().cloned().collect();
        refs.sort();
        refs
    }

    /// The sharing mode a reference was written with.
    #[must_use]
    pub fn sharing_of(&self, raw_ref: &str) -> Option<SharingMode> {
        self.secrets
            .lock()
            .unwrap()
            .get(raw_ref)
            .map(|entry| entry.sharing)
    }

    /// Seed a secret as if some other system had registered it — the
    /// caller-supplied-reference case.
    pub fn seed(&self, raw_ref: &str, value: &str) {
        self.secrets.lock().unwrap().insert(
            raw_ref.to_owned(),
            StoredSecret {
                bytes: value.as_bytes().to_vec(),
                sharing: SharingMode::Tenant,
            },
        );
    }

    fn write_result(&self) -> Result<(), CredStoreError> {
        if self.failing {
            Err(CredStoreError::Internal("backend failure".into()))
        } else {
            Ok(())
        }
    }

    /// [`Self::write_result`] plus [`Self::fail_creates_after`]'s budget.
    fn create_result(&self) -> Result<(), CredStoreError> {
        self.write_result()?;
        if let Some(budget) = self.fail_creates_after {
            let mut creates = self.creates.lock().unwrap();
            *creates += 1;
            if *creates > budget {
                return Err(CredStoreError::Internal("backend failure".into()));
            }
        }
        Ok(())
    }
}

impl Default for RecordingCredStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CredStoreClientV1 for RecordingCredStore {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        if self.failing {
            return Err(CredStoreError::Internal("backend failure".into()));
        }
        Ok(self
            .secrets
            .lock()
            .unwrap()
            .get(key.as_ref())
            .map(|entry| GetSecretResponse {
                value: SecretValue::new(entry.bytes.clone()),
                id: Uuid::nil(),
                owner_tenant_id: TenantId::nil(),
                sharing: entry.sharing,
                is_inherited: false,
                version: 1,
                secret_type: SecretType::generic().gts_id().to_owned(),
                expires_at: None,
            }))
    }

    async fn create_opts(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
        _opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        self.create_result()?;
        let mut secrets = self.secrets.lock().unwrap();
        if secrets.contains_key(key.as_ref()) {
            return Err(CredStoreError::Conflict);
        }
        secrets.insert(
            key.as_ref().to_owned(),
            StoredSecret {
                bytes: value.as_bytes().to_vec(),
                sharing,
            },
        );
        Ok(())
    }

    async fn put_opts(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
        _precondition: WritePrecondition,
        _opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        self.write_result()?;
        self.secrets.lock().unwrap().insert(
            key.as_ref().to_owned(),
            StoredSecret {
                bytes: value.as_bytes().to_vec(),
                sharing,
            },
        );
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
        _precondition: WritePrecondition,
    ) -> Result<(), CredStoreError> {
        self.write_result()?;
        match self.secrets.lock().unwrap().remove(key.as_ref()) {
            Some(_) => Ok(()),
            None => Err(CredStoreError::NotFound),
        }
    }
}

/// `RunnerSecretWriter` test double that records every
/// `ensure_kubeconfig_secret` call as `(credstore_ref, kubeconfig bytes)`, in
/// call order, and always succeeds.
///
/// Exists for D4's create/update half (the final review's I1): "the Secret was
/// written" is not observable from the returned `Environment`, from the
/// database, or from any DTO — the write goes into a *different cluster* — so
/// the only place it can be asserted is at the port.
pub struct RecordingSecretObserver {
    calls: Mutex<Vec<(String, Vec<u8>)>>,
}

impl RecordingSecretObserver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Every `ensure_kubeconfig_secret` call so far, in order.
    #[must_use]
    pub fn secret_writes(&self) -> Vec<(String, Vec<u8>)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Default for RecordingSecretObserver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RunnerSecretWriter for RecordingSecretObserver {
    async fn ensure_kubeconfig_secret(
        &self,
        credstore_ref: &str,
        kubeconfig: &SecretValue,
    ) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push((credstore_ref.to_owned(), kubeconfig.as_bytes().to_vec()));
        Ok(())
    }
}

/// `RunnerSecretWriter` test double whose `ensure_kubeconfig_secret` always
/// fails with a scripted message. Used to prove the observation ticker's
/// self-heal failure is logged with the environment attached (Task 8's
/// Addition 2).
pub struct FailingSecretObserver {
    message: String,
}

impl FailingSecretObserver {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait]
impl RunnerSecretWriter for FailingSecretObserver {
    async fn ensure_kubeconfig_secret(
        &self,
        _credstore_ref: &str,
        _kubeconfig: &SecretValue,
    ) -> Result<(), String> {
        Err(self.message.clone())
    }
}

/// Build the real `ConcreteAppServices` DI container — `SeaORM`-backed
/// repositories, not mocks — wired to `db` and `authz`, using the gear's
/// configured default `max_variables` cap (see `QaEnvironmentsConfig`).
pub fn build_services(db: Db, authz: Arc<dyn AuthZResolverClient>) -> Arc<ConcreteAppServices> {
    build_services_with_limit(
        db,
        authz,
        crate::config::QaEnvironmentsConfig::default().max_variables,
    )
}

/// Like [`build_services`], but with a caller-supplied `max_variables` cap
/// instead of the config default — used by tests exercising the
/// `list_for_env` truncation behavior.
pub fn build_services_with_limit(
    db: Db,
    authz: Arc<dyn AuthZResolverClient>,
    max_variables: usize,
) -> Arc<ConcreteAppServices> {
    build_services_full(
        db,
        authz,
        Arc::new(RecordingCredStore::new()),
        Arc::new(NoopRunnerSecretWriter),
        max_variables,
    )
}

/// The full wiring, for tests that need to hold on to the credstore double
/// and assert against it (and, now, to supply a [`RunnerSecretWriter`] double).
pub fn build_services_full(
    db: Db,
    authz: Arc<dyn AuthZResolverClient>,
    credstore: Arc<dyn CredStoreClientV1>,
    observer: Arc<dyn RunnerSecretWriter>,
    max_variables: usize,
) -> Arc<ConcreteAppServices> {
    build_services_with_plugin_port(
        db,
        authz,
        credstore,
        observer,
        vhp_shaped_plugin_port(),
        max_variables,
    )
}

/// [`build_services_full`] plus the product-plugin port, for the tests that
/// observe. Every other helper here routes through it with
/// [`no_plugin_port`].
pub fn build_services_with_plugin_port(
    db: Db,
    authz: Arc<dyn AuthZResolverClient>,
    credstore: Arc<dyn CredStoreClientV1>,
    observer: Arc<dyn RunnerSecretWriter>,
    product_plugins: Arc<dyn ProductPluginPort>,
    max_variables: usize,
) -> Arc<ConcreteAppServices> {
    let db: Arc<DBProvider<DbError>> = Arc::new(DBProvider::new(db));

    Arc::new(AppServices::new(
        Arc::new(OrmEnvironmentsRepository),
        Arc::new(OrmVariablesRepository),
        Arc::new(OrmLeasesRepository),
        db,
        authz,
        credstore,
        observer,
        product_plugins,
        max_variables,
    ))
}

/// Services wired with [`TenantScopedAuthZ`], a fresh [`RecordingCredStore`]
/// (so a seeded `kubeconfig_credstore_ref` resolves), a [`NoopRunnerSecretWriter`] and a
/// caller-supplied product-plugin port — the shape every observation test
/// wants.
pub fn build_services_tenant_scoped_with_plugin(
    db: Db,
    product_plugins: Arc<dyn ProductPluginPort>,
) -> Arc<ConcreteAppServices> {
    build_services_with_plugin_port(
        db,
        Arc::new(TenantScopedAuthZ),
        Arc::new(RecordingCredStore::new()),
        Arc::new(NoopRunnerSecretWriter),
        product_plugins,
        crate::config::QaEnvironmentsConfig::default().max_variables,
    )
}

/// [`build_services_tenant_scoped_with_plugin`] with a caller-supplied
/// credstore double as well, for the tests that assert against both.
pub fn build_services_tenant_scoped_with_plugin_and_credstore(
    db: Db,
    product_plugins: Arc<dyn ProductPluginPort>,
    credstore: Arc<dyn CredStoreClientV1>,
) -> Arc<ConcreteAppServices> {
    build_services_with_plugin_port(
        db,
        Arc::new(TenantScopedAuthZ),
        credstore,
        Arc::new(NoopRunnerSecretWriter),
        product_plugins,
        crate::config::QaEnvironmentsConfig::default().max_variables,
    )
}

/// Convenience: build services with the default [`TenantScopedAuthZ`] double.
pub fn build_services_tenant_scoped(db: Db) -> Arc<ConcreteAppServices> {
    build_services(db, Arc::new(TenantScopedAuthZ))
}

/// Convenience: build services with the default [`TenantScopedAuthZ`] double
/// and a caller-supplied `max_variables` cap.
pub fn build_services_tenant_scoped_with_limit(
    db: Db,
    max_variables: usize,
) -> Arc<ConcreteAppServices> {
    build_services_with_limit(db, Arc::new(TenantScopedAuthZ), max_variables)
}

/// Convenience: build services with [`TenantScopedAuthZ`], a fresh
/// [`RecordingCredStore`] (so a seeded `kubeconfig_credstore_ref` resolves),
/// and a caller-supplied [`RunnerSecretWriter`] double — for tests exercising
/// `EnvironmentsService::observe_environment`/`refresh_environment`.
pub fn build_services_tenant_scoped_with_observer(
    db: Db,
    observer: Arc<dyn RunnerSecretWriter>,
) -> Arc<ConcreteAppServices> {
    build_services_full(
        db,
        Arc::new(TenantScopedAuthZ),
        Arc::new(RecordingCredStore::new()),
        observer,
        crate::config::QaEnvironmentsConfig::default().max_variables,
    )
}

// ---------------------------------------------------------------------------
// Product-plugin doubles (Task 15)
// ---------------------------------------------------------------------------

/// A `QaProductPluginV1` double: declared schemas a test chooses, a scripted
/// [`PluginObservation`], and a record of every [`EnvironmentHandle`] it was
/// handed.
///
/// # Why it records the handle
///
/// Three of Task 15's requirements are about what the *gear* puts on the
/// handle rather than about what the plugin does with it — the credential
/// slot is `resolved` and not `reference_only`, `config` is passed verbatim,
/// and `observed` is `None` for a never-observed environment. None of those
/// is visible from the outcome, from the database, or from any DTO, so the
/// port is the only place they can be asserted. Same reasoning as
/// [`RecordingSecretObserver`]'s.
///
/// The recorded slots keep the credential **bytes**, because
/// `an_undeclared_attribute_never_reaches_storage` and its siblings need to
/// know the plugin really was handed the canary before asserting the canary
/// went nowhere. A double that discarded them could not tell "contained" from
/// "never present".
pub struct ScriptedPlugin {
    credential_schema: Vec<FieldDesc>,
    observed_schema: Vec<FieldDesc>,
    outcome: Mutex<PluginObservation>,
    handles: Mutex<Vec<RecordedHandle>>,
    /// When set, `validate_credentials` refuses with this failure — the only
    /// way to reach Task 18b's "the plugin rejected the form" path.
    credential_rejection: Option<PluginFailure>,
    /// When set, `validate_credentials` also classifies this key, whether or
    /// not the form carried it. A misbehaving plugin, for the one refusal
    /// Task 18b makes on the gear's own initiative: acting on it would mint a
    /// secret out of nothing.
    extra_classification: Option<String>,
    /// Every `CredentialInput` `validate_credentials` was handed, as key sets.
    /// **Keys only, never values** — recording submitted credential bytes in
    /// a test double is the 2026-08-28 leak with a shorter blast radius.
    validated: Mutex<Vec<Vec<String>>>,
}

/// One `CredentialSlot` as a test can compare it: the key, the reference,
/// and the resolved bytes — `None` for a `reference_only` slot, which
/// `observe` must never be given.
///
/// A named type rather than the tuple it wraps, because `clippy::type_complexity`
/// is denied workspace-wide and a three-element tuple inside a `Vec` trips it.
pub type RecordedSlot = (String, String, Option<Vec<u8>>);

/// One `observe` call's handle, flattened into owned values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedHandle {
    pub slots: Vec<RecordedSlot>,
    pub config: serde_json::Value,
    pub observed: Option<ObservedAttrs>,
}

impl ScriptedPlugin {
    /// A plugin declaring VHP's own shape: one required multiline secret
    /// (`kubeconfig`), one optional non-secret config field
    /// (`vpadm_namespace`), and the four role-claimed observed fields.
    #[must_use]
    pub fn vhp_shaped(outcome: PluginObservation) -> Self {
        Self {
            credential_schema: vhp_shaped_credential_schema(),
            observed_schema: vhp_shaped_observed_schema(),
            outcome: Mutex::new(outcome),
            handles: Mutex::new(Vec::new()),
            credential_rejection: None,
            extra_classification: None,
            validated: Mutex::new(Vec::new()),
        }
    }

    /// Make `validate_credentials` refuse with `failure`.
    #[must_use]
    pub fn rejecting_credentials(mut self, failure: PluginFailure) -> Self {
        self.credential_rejection = Some(failure);
        self
    }

    /// Make `validate_credentials` claim a field the form did not carry.
    #[must_use]
    pub fn classifying_unsubmitted_key(mut self, key: &str) -> Self {
        self.extra_classification = Some(key.to_owned());
        self
    }

    /// The **keys** of every form `validate_credentials` was handed, in call
    /// order. Never the values.
    #[must_use]
    pub fn validated_keys(&self) -> Vec<Vec<String>> {
        self.validated.lock().unwrap().clone()
    }

    /// Replace the declared credential schema — for the two cases the legacy
    /// single-reference column cannot be bound to: no required secret field,
    /// and two of them.
    #[must_use]
    pub fn with_credential_schema(mut self, schema: Vec<FieldDesc>) -> Self {
        self.credential_schema = schema;
        self
    }

    /// Replace the declared observed schema — for
    /// `an_undeclared_attribute_never_reaches_storage`, which needs a
    /// spotless schema next to an outcome that sets a key it does not
    /// declare.
    #[must_use]
    pub fn with_observed_schema(mut self, schema: Vec<FieldDesc>) -> Self {
        self.observed_schema = schema;
        self
    }

    /// Every handle this plugin was handed, in call order.
    #[must_use]
    pub fn handles(&self) -> Vec<RecordedHandle> {
        self.handles.lock().unwrap().clone()
    }

    /// The handle from the most recent `observe` call.
    #[must_use]
    pub fn last_handle(&self) -> RecordedHandle {
        self.handles
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("observe was never called")
    }
}

/// VHP's own credential shape: one required multiline secret, one optional
/// non-secret config field. Shared by the two plugin doubles so a test that
/// swaps one for the other is not also swapping the schema under it.
#[must_use]
pub fn vhp_shaped_credential_schema() -> Vec<FieldDesc> {
    vec![
        field("kubeconfig", FieldKind::MultilineSecret, true, None),
        field("vpadm_namespace", FieldKind::Text, false, None),
    ]
}

/// The four role-claimed observed fields, spelled as
/// `qa-vhp-product-plugin`'s `observed_schema()` spells them.
#[must_use]
pub fn vhp_shaped_observed_schema() -> Vec<FieldDesc> {
    vec![
        field(
            "platformVersion",
            FieldKind::Text,
            false,
            Some(FieldRole::Version),
        ),
        field("build", FieldKind::Text, false, Some(FieldRole::Build)),
        field(
            "baseDomain",
            FieldKind::Url,
            false,
            Some(FieldRole::BaseUrl),
        ),
        field(
            "namespace",
            FieldKind::Text,
            false,
            Some(FieldRole::Namespace),
        ),
    ]
}

/// Classify a submitted form the way a real plugin does: every declared field
/// the form actually carried, secret-ness taken from its `FieldKind`.
///
/// # Why the doubles share one implementation
///
/// It is `qa-vhp-product-plugin`'s `validate_credentials` minus the
/// product-specific required-field check, which is the part a double has no
/// business asserting. Before Task 18b all three doubles answered
/// `unimplemented!("qa-environments' observation path never validates a form")`
/// — true when written, because the only caller was the leak-conformance
/// harness. Task 18b's write path is the production caller that comment said
/// did not exist.
#[must_use]
pub fn classify_against_schema(
    schema: &[FieldDesc],
    input: &CredentialInput,
) -> Vec<CredentialClassification> {
    schema
        .iter()
        .filter(|field| input.contains(&field.key))
        .map(|field| CredentialClassification {
            key: field.key.clone(),
            is_secret: field.kind.is_secret(),
        })
        .collect()
}

/// One `FieldDesc`, spelled once for the doubles above.
#[must_use]
pub fn field(key: &str, kind: FieldKind, required: bool, role: Option<FieldRole>) -> FieldDesc {
    FieldDesc {
        key: key.to_owned(),
        label: key.to_owned(),
        kind,
        required,
        role,
        in_table: false,
        in_detail: true,
        help: None,
    }
}

#[async_trait]
impl QaProductPluginV1 for ScriptedPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        self.credential_schema.clone()
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        self.observed_schema.clone()
    }

    async fn validate_credentials(
        &self,
        input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        self.validated
            .lock()
            .unwrap()
            .push(input.fields.keys().cloned().collect());
        if let Some(ref failure) = self.credential_rejection {
            return Err(failure.clone());
        }
        let mut classifications = classify_against_schema(&self.credential_schema(), input);
        if let Some(ref key) = self.extra_classification {
            classifications.push(CredentialClassification::secret(key.clone()));
        }
        Ok(classifications)
    }

    async fn observe(&self, env: &EnvironmentHandle<'_>) -> PluginObservation {
        self.handles.lock().unwrap().push(RecordedHandle {
            slots: env
                .slots
                .iter()
                .map(|slot| {
                    (
                        slot.key.clone(),
                        slot.credstore_ref.clone(),
                        slot.value.as_ref().map(|v| v.as_bytes().to_vec()),
                    )
                })
                .collect(),
            config: env.config.clone(),
            observed: env.observed.cloned(),
        });
        self.outcome.lock().unwrap().clone()
    }

    async fn prepare_run_access(
        &self,
        _env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        unimplemented!("dispatch is qa-runs' side of the contract, not this gear's")
    }

    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        unimplemented!("dispatch is qa-runs' side of the contract, not this gear's")
    }

    fn env_contract(&self) -> RunVarContract {
        unimplemented!("dispatch is qa-runs' side of the contract, not this gear's")
    }
}

/// A `ProductPluginPort` double resolving **every** product to one plugin,
/// and recording the product ids it was asked about.
pub struct FixedPluginPort {
    plugin: Arc<dyn QaProductPluginV1>,
    asked: Mutex<Vec<Uuid>>,
}

impl FixedPluginPort {
    #[must_use]
    pub fn new(plugin: Arc<dyn QaProductPluginV1>) -> Self {
        Self {
            plugin,
            asked: Mutex::new(Vec::new()),
        }
    }

    /// The product ids this port was asked to resolve, in call order — which
    /// is how `the_cycle_resolves_the_plugin_for_the_environments_product`
    /// asserts one resolution per environment.
    #[must_use]
    pub fn asked(&self) -> Vec<Uuid> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl ProductPluginPort for FixedPluginPort {
    async fn plugin_for(
        &self,
        _ctx: &SecurityContext,
        product_id: Uuid,
    ) -> Result<Arc<dyn QaProductPluginV1>, PluginUnavailable> {
        self.asked.lock().unwrap().push(product_id);
        Ok(Arc::clone(&self.plugin))
    }
}

/// A `ProductPluginPort` resolving every product to a VHP-shaped plugin.
///
/// **The default since Task 19**, and the reason the default changed: an
/// environment's credentials can now only be stored under a key its product's
/// plugin declares (ruling F-13), so a services double with no plugin cannot
/// create the fixture almost every test needs. [`no_plugin_port`] used to be
/// the default on the argument that a permissive stand-in hides a gap — true
/// while the plugin was only needed to *observe*, and now inverted: with no
/// plugin the gap is that no test can create an environment at all.
///
/// [`no_plugin_port`] is still there, and is what the tests that are actually
/// about an unavailable resolver ask for by name.
#[must_use]
pub fn vhp_shaped_plugin_port() -> Arc<dyn ProductPluginPort> {
    Arc::new(FixedPluginPort::new(Arc::new(ScriptedPlugin::vhp_shaped(
        qa_product_sdk::observation::PluginObservation {
            environment: qa_product_sdk::observation::ObservationOutcome::Detected(
                qa_product_sdk::observation::ObservedAttrs::default(),
            ),
            health: qa_product_sdk::observation::HealthOutcome::Checked {
                state: qa_product_sdk::observation::HealthState::Ok,
                detail: None,
            },
        },
    ))))
}

/// A `ProductPluginPort` double that resolves nothing, with the cause a test
/// chooses.
pub struct UnavailablePluginPort(pub PluginUnavailable);

#[async_trait]
impl ProductPluginPort for UnavailablePluginPort {
    async fn plugin_for(
        &self,
        _ctx: &SecurityContext,
        _product_id: Uuid,
    ) -> Result<Arc<dyn QaProductPluginV1>, PluginUnavailable> {
        Err(self.0)
    }
}

/// The port every test that does not observe gets: asked, it reports that no
/// resolver is registered.
///
/// Deliberately not a permissive default. A test that reaches observation
/// without saying what plugin it expects has a gap, and this makes the gap
/// show up as a recorded failure on the row rather than as a silent success
/// against a stand-in nobody chose.
#[must_use]
pub fn no_plugin_port() -> Arc<dyn ProductPluginPort> {
    Arc::new(UnavailablePluginPort(PluginUnavailable::ResolverAbsent))
}

/// A `QaProductPluginV1` double whose outcome depends on the credential
/// material it was handed — rather than on call order — and what lets one
/// observation cycle script a different outcome per environment.
///
/// It replaced a `PlatformObserver` double of the same shape (`KeyedObserver`,
/// deleted by Task 15 along with the observation path it doubled; the port
/// itself is gone since Task 19) and keys on
/// the same thing, so the property its callers assert is unchanged.
///
/// Panics on a document no test scripted, rather than returning a default: a
/// silent default is how a cycle test ends up asserting against an outcome
/// nobody chose.
pub struct KeyedPlugin {
    outcomes: Mutex<HashMap<Vec<u8>, PluginObservation>>,
}

impl KeyedPlugin {
    #[must_use]
    pub fn new(outcomes: HashMap<Vec<u8>, PluginObservation>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes),
        }
    }
}

#[async_trait]
impl QaProductPluginV1 for KeyedPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        vhp_shaped_credential_schema()
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        vhp_shaped_observed_schema()
    }

    async fn validate_credentials(
        &self,
        input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        Ok(classify_against_schema(&self.credential_schema(), input))
    }

    async fn observe(&self, env: &EnvironmentHandle<'_>) -> PluginObservation {
        let material = env
            .slots
            .first()
            .and_then(|slot| slot.value.as_ref())
            .expect("observe must be handed a RESOLVED slot")
            .as_bytes()
            .to_vec();
        self.outcomes
            .lock()
            .unwrap()
            .get(&material)
            .cloned()
            .unwrap_or_else(|| {
                panic!("KeyedPlugin called with a document no test scripted an outcome for")
            })
    }

    async fn prepare_run_access(
        &self,
        _env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        unimplemented!("dispatch is qa-runs' side of the contract, not this gear's")
    }

    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        unimplemented!("dispatch is qa-runs' side of the contract, not this gear's")
    }

    fn env_contract(&self) -> RunVarContract {
        unimplemented!("dispatch is qa-runs' side of the contract, not this gear's")
    }
}

/// A `QaProductPluginV1` double returning a pre-scripted **sequence** of
/// outcomes, one per `observe` call — for the test shape that needs it: seed
/// a known value with the first observation, then exercise a failure on the
/// second, without rebuilding `services` in between. (Task 15 deleted the
/// `PlatformObserver` double of the same shape, `ScriptedObserver`, with the
/// path it doubled; Task 19 deleted what was left of that port.)
///
/// Panics if `observe` is called more often than outcomes were scripted:
/// running out is a test-setup bug (the wrong number of refresh calls), not a
/// case worth papering over with a default.
pub struct SequencedPlugin {
    outcomes: Mutex<VecDeque<PluginObservation>>,
}

impl SequencedPlugin {
    #[must_use]
    pub fn new(outcomes: impl IntoIterator<Item = PluginObservation>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }
    }
}

#[async_trait]
impl QaProductPluginV1 for SequencedPlugin {
    fn credential_schema(&self) -> Vec<FieldDesc> {
        vhp_shaped_credential_schema()
    }

    fn observed_schema(&self) -> Vec<FieldDesc> {
        vhp_shaped_observed_schema()
    }

    async fn validate_credentials(
        &self,
        input: &CredentialInput,
    ) -> Result<Vec<CredentialClassification>, PluginFailure> {
        Ok(classify_against_schema(&self.credential_schema(), input))
    }

    async fn observe(&self, _env: &EnvironmentHandle<'_>) -> PluginObservation {
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .expect("SequencedPlugin called more times than outcomes were scripted")
    }

    async fn prepare_run_access(
        &self,
        _env: &EnvironmentHandle<'_>,
    ) -> Result<RunAccess, PluginFailure> {
        unimplemented!("dispatch is qa-runs' side of the contract, not this gear's")
    }

    fn runner(&self, _observed: Option<&ObservedAttrs>) -> RunnerSpec {
        unimplemented!("dispatch is qa-runs' side of the contract, not this gear's")
    }

    fn env_contract(&self) -> RunVarContract {
        unimplemented!("dispatch is qa-runs' side of the contract, not this gear's")
    }
}

/// One `Detected` outcome carrying the version and build attributes, health
/// `NotAttempted` — the shape most tests want, spelled once.
#[must_use]
pub fn plugin_detected(version: &str, build: Option<&str>) -> PluginObservation {
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", version);
    if let Some(build) = build {
        attrs.set("build", build);
    }
    PluginObservation {
        environment: qa_product_sdk::observation::ObservationOutcome::Detected(attrs),
        health: qa_product_sdk::observation::HealthOutcome::NotAttempted,
    }
}

/// One failed environment half, carrying the remote's own text in the field
/// sanctioned to hold it.
#[must_use]
pub fn plugin_detection_failed(remote: &str) -> PluginObservation {
    PluginObservation {
        environment: qa_product_sdk::observation::ObservationOutcome::Failed(
            PluginFailure::classified(
                qa_product_sdk::observation::FailureClass::NotFound,
                "the target could not be read",
            )
            .with_remote_message(remote.to_owned()),
        ),
        health: qa_product_sdk::observation::HealthOutcome::NotAttempted,
    }
}

/// Write the `config` column directly, the way
/// `m20260903_000011_environment_plugin_columns`' backfill left it.
///
/// Through the entity rather than raw SQL, so the column name stays
/// compiler-checked, and under a tenant-bound scope rather than
/// `AccessScope::allow_all()`: this crate permits exactly one `allow_all()`
/// call site and `unscoped_read_guard_tests` fails the build on a second one,
/// test helper or not.
///
/// This used to say "there is no write path for `config` in this gear yet —
/// the plugin-driven credential form that adds one is Task 22's". **Task 18b
/// is that write path**: a credential field the plugin classifies non-secret
/// is merged into this column by `merge_config`. The helper is still needed
/// for a test that wants a `config` no request produced — the migration's
/// backfilled shape, which is what these fixtures model.
pub async fn set_config(db: &Db, tenant: Uuid, id: Uuid, config: serde_json::Value) {
    use sea_orm::ActiveValue;
    use toolkit_db::secure::secure_update_with_scope;

    let provider: DBProvider<DbError> = DBProvider::new(db.clone());
    let conn = provider.conn().expect("a connection");
    let am = crate::infra::storage::entity::environment::ActiveModel {
        id: ActiveValue::Unchanged(id),
        config: ActiveValue::Set(config),
        ..Default::default()
    };
    secure_update_with_scope::<crate::infra::storage::entity::environment::Entity>(
        am,
        &toolkit_security::AccessScope::for_tenant(tenant),
        id,
        &conn,
    )
    .await
    .expect("the config seed must apply");
}

// ---------------------------------------------------------------------------
// The security keystone
// ---------------------------------------------------------------------------

/// Collects the raw bytes a `tracing` subscriber writes, so a test can assert
/// against **everything that was emitted** rather than a filtered view of it.
///
/// This exists instead of `tracing-test` (used elsewhere in this workspace)
/// because of a hole that a break-test found: `tracing-test` keeps only the
/// captured lines containing the test's span name, so a **multi-line** field
/// value survives capture as its first line only. A kubeconfig is multi-line
/// and its private key is not on line one, so a deliberate
/// `info!(document = %material.expose())` planted in `write_generated_secret`
/// left a `tracing-test` assertion on the canary **passing**. Against this
/// buffer the same plant fails, which is the whole point of the test.
///
/// Originally private to `environments_kubeconfig_tests`; moved here (review
/// finding #29-followup) so `infra::storage::environments_sea_repo`'s tests
/// can assert `attrs_or_skip`'s `warn!` line too, instead of leaving the
/// `_and_warns` half of a test's name unverified. Two hazards apply to every
/// caller, not just the original one:
///
/// 1. `tracing` caches each callsite's `Interest` **globally**, decided by
///    whichever thread reaches it first. A callsite exercised only by one
///    test is safe by construction; a callsite other tests can also reach
///    needs the warm-up-then-clear dance
///    `the_document_never_reaches_a_log_line_a_debug_rendering_or_a_response_body`
///    uses below.
/// 2. A thread-local `tracing::subscriber::set_default` guard only covers
///    work done on the thread that installed it. A synchronous callsite is
///    fine on a plain `#[test]`; an `async` one needs a current-thread
///    runtime (`#[tokio::test]`'s default) so every `.await` stays on that
///    thread.
#[derive(Clone, Default)]
pub struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

impl CapturedLogs {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

pub struct CapturedLogsWriter(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLogsWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogsWriter;
    fn make_writer(&'a self) -> Self::Writer {
        CapturedLogsWriter(Arc::clone(&self.0))
    }
}
