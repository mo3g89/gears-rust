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

use crate::domain::ports::{
    HealthOutcome, NoopObserver, ObservationOutcome, PlatformObservation, PlatformObserver,
};
use crate::domain::service::AppServices;
use crate::gear::ConcreteAppServices;
use crate::infra::storage::{OrmLeasesRepository, OrmPlatformsRepository, OrmVariablesRepository};

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
/// tests (`api::rest::handlers::platforms`) can use it too, via
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
        }
    }

    /// Every operation fails. Used to prove a failed credstore write creates
    /// no platform row.
    #[must_use]
    pub fn always_failing() -> Self {
        Self {
            secrets: Mutex::new(HashMap::new()),
            failing: true,
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
        self.write_result()?;
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

/// `PlatformObserver` test double that returns a pre-scripted sequence of
/// outcomes, one per call to `observe` — so a single test can seed a platform
/// with a known `Detected` value on one call and then exercise a `Failed` one
/// on the next, without rebuilding `services` in between.
///
/// Panics if `observe` is called more times than outcomes were scripted:
/// running out is a test-setup bug (the wrong number of refresh calls), not a
/// case worth papering over with a default.
pub struct ScriptedObserver {
    outcomes: Mutex<VecDeque<PlatformObservation>>,
}

impl ScriptedObserver {
    /// Script only the version-detection half; every call's cluster-health
    /// half is [`HealthOutcome::NotAttempted`]. That is the honest default
    /// for a double that exists to exercise the version half: no cluster read
    /// was attempted, which is exactly true, whereas `Failed` would claim one
    /// was attempted and failed -- and `record_observation` would persist
    /// that claim as `cluster_status = "Unreachable"`. Most existing callers
    /// only care about `ObservationOutcome`, so this constructor stays the
    /// convenient one; a test that needs to drive `health` too should use
    /// [`Self::script`] instead.
    #[must_use]
    pub fn new(outcomes: impl IntoIterator<Item = ObservationOutcome>) -> Self {
        Self {
            outcomes: Mutex::new(
                outcomes
                    .into_iter()
                    .map(|platform| PlatformObservation {
                        platform,
                        health: HealthOutcome::NotAttempted,
                    })
                    .collect(),
            ),
        }
    }

    /// Queue one full `PlatformObservation` — both halves — to be returned by
    /// the next `observe` call. Unlike [`Self::new`], which always leaves
    /// `health` as `NotAttempted`, this is what a test needs to drive
    /// `health` through `Checked` as well as `Failed`.
    pub fn script(&self, observation: PlatformObservation) {
        self.outcomes.lock().unwrap().push_back(observation);
    }
}

#[async_trait]
impl PlatformObserver for ScriptedObserver {
    async fn observe(&self, _kubeconfig: &SecretValue, _vpadm_namespace: &str) -> PlatformObservation {
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .expect("ScriptedObserver called more times than outcomes were scripted")
    }

    async fn ensure_kubeconfig_secret(
        &self,
        _credstore_ref: &str,
        _kubeconfig: &SecretValue,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// `PlatformObserver` test double whose outcome depends on **which**
/// kubeconfig it was handed, not on call order — unlike [`ScriptedObserver`],
/// whose FIFO script only works for a test that observes exactly one platform
/// at a time. `PlatformsService::run_observation_cycle` visits every platform
/// in whatever order the database happens to return them in (no `ORDER BY`),
/// so a multi-platform ticker test that needs "platform B's observer returns
/// X" has no order to rely on; keying on the kubeconfig bytes each platform's
/// pasted document round-trips as (`RecordingCredStore`'s real `get`) sidesteps
/// that entirely.
pub struct KeyedObserver {
    outcomes: Mutex<HashMap<Vec<u8>, ObservationOutcome>>,
}

impl KeyedObserver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            outcomes: Mutex::new(HashMap::new()),
        }
    }

    /// Script `outcome` for whatever platform's kubeconfig document is
    /// exactly `kubeconfig_document`.
    pub fn script(&self, kubeconfig_document: &str, outcome: ObservationOutcome) {
        self.outcomes
            .lock()
            .unwrap()
            .insert(kubeconfig_document.as_bytes().to_vec(), outcome);
    }
}

impl Default for KeyedObserver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PlatformObserver for KeyedObserver {
    async fn observe(&self, kubeconfig: &SecretValue, _vpadm_namespace: &str) -> PlatformObservation {
        let platform = self
            .outcomes
            .lock()
            .unwrap()
            .get(kubeconfig.as_bytes())
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "KeyedObserver called with a kubeconfig document no test scripted an \
                     outcome for"
                )
            });
        PlatformObservation {
            platform,
            health: HealthOutcome::NotAttempted,
        }
    }

    async fn ensure_kubeconfig_secret(
        &self,
        _credstore_ref: &str,
        _kubeconfig: &SecretValue,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// `PlatformObserver` test double that records every `vpadm_namespace` it was
/// called with, in call order, and always returns the same scripted outcome.
/// Proves `PlatformsService::observe_cluster` actually resolves and forwards
/// a platform's own `VPADM_NAMESPACE` override (or the `virtuozzo` fallback),
/// rather than always passing a constant — the property Task 8's namespace
/// resolution (Addition 1) adds.
pub struct NamespaceRecordingObserver {
    namespaces: Mutex<Vec<String>>,
    outcome: ObservationOutcome,
}

impl NamespaceRecordingObserver {
    #[must_use]
    pub fn new(outcome: ObservationOutcome) -> Self {
        Self {
            namespaces: Mutex::new(Vec::new()),
            outcome,
        }
    }

    /// The namespace passed to the most recent `observe` call, if any.
    #[must_use]
    pub fn last_namespace(&self) -> Option<String> {
        self.namespaces.lock().unwrap().last().cloned()
    }
}

#[async_trait]
impl PlatformObserver for NamespaceRecordingObserver {
    async fn observe(&self, _kubeconfig: &SecretValue, vpadm_namespace: &str) -> PlatformObservation {
        self.namespaces.lock().unwrap().push(vpadm_namespace.to_owned());
        PlatformObservation {
            platform: self.outcome.clone(),
            health: HealthOutcome::NotAttempted,
        }
    }

    async fn ensure_kubeconfig_secret(
        &self,
        _credstore_ref: &str,
        _kubeconfig: &SecretValue,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// `PlatformObserver` test double that records every
/// `ensure_kubeconfig_secret` call as `(credstore_ref, kubeconfig bytes)`, in
/// call order, and always succeeds.
///
/// Exists for D4's create/update half (the final review's I1): "the Secret was
/// written" is not observable from the returned `TargetPlatform`, from the
/// database, or from any DTO — the write goes into a *different cluster* — so
/// the only place it can be asserted is at the port.
pub struct RecordingSecretObserver {
    calls: Mutex<Vec<(String, Vec<u8>)>>,
    outcome: ObservationOutcome,
}

impl RecordingSecretObserver {
    #[must_use]
    pub fn new(outcome: ObservationOutcome) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            outcome,
        }
    }

    /// Every `ensure_kubeconfig_secret` call so far, in order.
    #[must_use]
    pub fn secret_writes(&self) -> Vec<(String, Vec<u8>)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl PlatformObserver for RecordingSecretObserver {
    async fn observe(&self, _kubeconfig: &SecretValue, _vpadm_namespace: &str) -> PlatformObservation {
        PlatformObservation {
            platform: self.outcome.clone(),
            health: HealthOutcome::NotAttempted,
        }
    }

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

/// `PlatformObserver` test double whose `ensure_kubeconfig_secret` always
/// fails with a scripted message and whose `observe` always succeeds with a
/// fixed, uninteresting outcome. Used to prove the observation ticker's
/// self-heal failure is logged with the platform attached (Task 8's
/// Addition 2), independently of whatever `observe` itself returned.
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
impl PlatformObserver for FailingSecretObserver {
    async fn observe(&self, _kubeconfig: &SecretValue, _vpadm_namespace: &str) -> PlatformObservation {
        PlatformObservation {
            platform: ObservationOutcome::Detected(crate::domain::observation::DetectedPlatform {
                version: "1.0.0".to_owned(),
                build: None,
                raw: "1.0.0".to_owned(),
                namespace: "virtuozzo".to_owned(),
                base_domain: None,
            }),
            health: HealthOutcome::NotAttempted,
        }
    }

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
        Arc::new(NoopObserver),
        max_variables,
    )
}

/// The full wiring, for tests that need to hold on to the credstore double
/// and assert against it (and, now, to supply a `PlatformObserver` double).
pub fn build_services_full(
    db: Db,
    authz: Arc<dyn AuthZResolverClient>,
    credstore: Arc<dyn CredStoreClientV1>,
    observer: Arc<dyn PlatformObserver>,
    max_variables: usize,
) -> Arc<ConcreteAppServices> {
    let db: Arc<DBProvider<DbError>> = Arc::new(DBProvider::new(db));

    Arc::new(AppServices::new(
        Arc::new(OrmPlatformsRepository),
        Arc::new(OrmVariablesRepository),
        Arc::new(OrmLeasesRepository),
        db,
        authz,
        credstore,
        observer,
        max_variables,
    ))
}

/// Convenience: build services with the default [`TenantScopedAuthZ`] double.
pub fn build_services_tenant_scoped(db: Db) -> Arc<ConcreteAppServices> {
    build_services(db, Arc::new(TenantScopedAuthZ))
}

/// Convenience: build services with [`TenantScopedAuthZ`] and a
/// caller-supplied credstore double, returning the services only — the
/// caller already holds its own `Arc` to the double.
pub fn build_services_tenant_scoped_with_credstore(
    db: Db,
    credstore: Arc<dyn CredStoreClientV1>,
) -> Arc<ConcreteAppServices> {
    build_services_full(
        db,
        Arc::new(TenantScopedAuthZ),
        credstore,
        Arc::new(NoopObserver),
        crate::config::QaEnvironmentsConfig::default().max_variables,
    )
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
/// and a caller-supplied `PlatformObserver` double — for tests exercising
/// `PlatformsService::observe_platform`/`refresh_platform`.
pub fn build_services_tenant_scoped_with_observer(
    db: Db,
    observer: Arc<dyn PlatformObserver>,
) -> Arc<ConcreteAppServices> {
    build_services_full(
        db,
        Arc::new(TenantScopedAuthZ),
        Arc::new(RecordingCredStore::new()),
        observer,
        crate::config::QaEnvironmentsConfig::default().max_variables,
    )
}
