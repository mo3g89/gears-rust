//! Test-only helpers for exercising the qa-catalog gear against a real
//! in-memory `SQLite` database, wired with the gear's actual migrations and
//! `SeaORM`-backed repositories.
//!
//! This complements `domain::service::test_support`, which provides mock
//! repository doubles for pure service-logic unit tests. This module is the
//! DB-backed tier: [`domain::service::tests_tenant_scoping`] uses it to
//! verify that `SecureORM` row-level scoping (not just service-layer
//! plumbing) actually isolates tenants.
//!
//! Reference: `gears/qa-platform/qa-environments/.../src/test_support.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverClient;
use authz_resolver_sdk::AuthZResolverError;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use credstore_sdk::CredStoreClientV1;
use credstore_sdk::test_util::MockCredStoreClient;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::bundle_store::BundleStore;
use crate::domain::ports::repo_sync::{RepoSyncPort, SyncResult};
use crate::domain::service::{AppServices, ServiceDeps, SyncCache};
use crate::gear::ConcreteAppServices;
use crate::infra::storage::{
    OrmBundlesRepository, OrmCustomPlansRepository, OrmProductsRepository, OrmSshKeysRepository,
    OrmTestReposRepository,
};

/// Build a `SecurityContext` for `tenant_id` with a fresh random subject.
pub fn ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap()
}

/// Create a product in the caller's tenant and return its id. Repository
/// fixtures need one because `qa_test_repositories.product_id` is required.
pub async fn seed_product(
    services: &ConcreteAppServices,
    ctx: &SecurityContext,
    name: &str,
) -> Uuid {
    let key = name.to_uppercase();
    let product = services
        .products
        .create_product(
            ctx,
            name.to_owned(),
            key,
            format!("{name} fixture product"),
            None,
        )
        .await
        .expect("failed to seed fixture product");
    product.id
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
    .expect("failed to run qa-catalog migrations");

    db
}

/// Shared decision logic for the permissive `AuthZ` test doubles used across
/// BOTH test tiers in this crate: the mock-repository unit tests
/// (`domain::service::test_support::PermissiveAuthZ` re-uses this) and this
/// DB-backed tier (`TenantScopedAuthZ` below). Always grants access,
/// returning a tenant `IN` constraint derived from the subject's tenant
/// (mirrors a real PDP's default tenant-isolation policy). Tenant is
/// resolved from the explicit PEP tenant context first, falling back to the
/// subject's `tenant_id` property (like a real PDP).
///
/// A nil UUID is treated as anonymous/unset and yields **no constraints**,
/// which is *not* a platform-wide grant: every service method in this crate
/// calls `PolicyEnforcer::access_scope` (never
/// `access_scope_with(require_constraints(false))`), so an empty constraint
/// set fails compilation (`CompileFailed`) and lands as
/// [`DomainError::Forbidden`]. This matters for a genuinely unauthenticated
/// caller; it no longer matters for [`crate::domain::system_actor`]'s two
/// platform-scoped factories (`for_branch_refresh_enumeration`,
/// `for_bundle_gc`) — both elevate through
/// `crate::domain::elevated::enumeration_scope` instead and never call
/// `access_scope` at all, so neither reaches this double. Use
/// [`SystemActorGrantAuthZ`] to test a background task's tenant-bound
/// *write* half; see its docs for what obligation that half still carries.
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

/// `AuthZ` double modelling a deployment whose policy grants qa-catalog's
/// system actor (see [`crate::domain::system_actor`]) a scope covering
/// several tenants at once, while every other subject stays tenant-scoped as
/// under [`TenantScopedAuthZ`].
///
/// The grant is expressed the only way it can be: as an explicit
/// `owner_tenant_id IN [tenants]` constraint. Every service method in this
/// crate calls `PolicyEnforcer::access_scope` (never
/// `access_scope_with(require_constraints(false))`), so a decision with **no**
/// constraints fails compilation and lands as `Forbidden`.
///
/// **What this exercises today.** Both of the gear's lifecycle tasks split
/// into an elevated, nil-tenant *enumeration* step and a PEP-derived,
/// tenant-bound *write* step (see `domain::system_actor`'s module doc). The
/// enumeration step no longer reaches `access_scope` at all — it elevates
/// through `domain::elevated::enumeration_scope` — so it carries no
/// deployment obligation and this double's grant is never consulted for it.
/// What remains is the write step's ordinary obligation: the deployment's
/// policy must grant `qa_catalog.system` a scope covering the tenant each
/// per-tenant write names. Set `system_tenants` to those tenants to model
/// that.
pub struct SystemActorGrantAuthZ {
    /// Tenants the system actor may see.
    pub system_tenants: Vec<Uuid>,
}

#[async_trait]
impl AuthZResolverClient for SystemActorGrantAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let is_system =
            request.subject.id == crate::domain::system_actor::QA_CATALOG_SYSTEM_ACTOR_UUID;

        if !is_system {
            return Ok(permissive_response(&request));
        }

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        self.system_tenants.clone(),
                    ))],
                }],
                ..Default::default()
            },
        })
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

/// Inert [`RepoSyncPort`] double: the tenant-scoping tests never sync, so a
/// call reaching the engine is itself a test failure.
struct NoopSyncEngine;

#[async_trait]
impl RepoSyncPort for NoopSyncEngine {
    async fn sync(
        &self,
        _url: &str,
        _branch: &str,
        _credential: Option<&str>,
        _host_dir: &std::path::Path,
        _branch_workdir: &std::path::Path,
    ) -> Result<SyncResult, DomainError> {
        Err(DomainError::Internal(
            "NoopSyncEngine::sync must not be called by these tests".to_owned(),
        ))
    }

    async fn list_remote_branches(
        &self,
        _url: &str,
        _credential: Option<&str>,
    ) -> Result<Vec<String>, DomainError> {
        Err(DomainError::Internal(
            "NoopSyncEngine::list_remote_branches must not be called by these tests".to_owned(),
        ))
    }
}

/// [`RepoSyncPort`] double whose ls-refs half returns a fixed branch
/// inventory — used by the branch-cache refresher tenant-binding test. The
/// content-sync half stays inert (the refresher must never call it).
struct BranchListingSyncEngine {
    branches: Vec<String>,
}

#[async_trait]
impl RepoSyncPort for BranchListingSyncEngine {
    async fn sync(
        &self,
        _url: &str,
        _branch: &str,
        _credential: Option<&str>,
        _host_dir: &std::path::Path,
        _branch_workdir: &std::path::Path,
    ) -> Result<SyncResult, DomainError> {
        Err(DomainError::Internal(
            "the branch refresher must use list_remote_branches, not sync".to_owned(),
        ))
    }

    async fn list_remote_branches(
        &self,
        _url: &str,
        _credential: Option<&str>,
    ) -> Result<Vec<String>, DomainError> {
        Ok(self.branches.clone())
    }
}

/// Inert [`BundleStore`] double — same rationale as [`NoopSyncEngine`].
struct NoopBundleStore;

#[async_trait]
impl BundleStore for NoopBundleStore {
    async fn put(&self, _bundle_id: Uuid, _bytes: Vec<u8>) -> Result<String, DomainError> {
        Err(DomainError::Storage(
            "NoopBundleStore::put must not be called by these tests".to_owned(),
        ))
    }

    async fn get(&self, _storage_ref: &str) -> Result<Vec<u8>, DomainError> {
        Err(DomainError::Storage(
            "NoopBundleStore::get must not be called by these tests".to_owned(),
        ))
    }

    async fn delete(&self, _storage_ref: &str) -> Result<(), DomainError> {
        Ok(())
    }
}

/// Build the real `ConcreteAppServices` DI container — `SeaORM`-backed
/// repositories, not mocks — wired to `db` and `authz`, with inert
/// sync-engine/bundle-store ports and the in-memory credstore double (the
/// scoping tests exercise row-level DB isolation, not the git/blob planes).
pub fn build_services(db: Db, authz: Arc<dyn AuthZResolverClient>) -> Arc<ConcreteAppServices> {
    build_services_with_engine(db, authz, Arc::new(NoopSyncEngine), throwaway_repos_dir())
}

/// A fresh, never-created path under the system temp dir. Suites that never
/// touch repository content use it so no two of them share a working area.
fn throwaway_repos_dir() -> PathBuf {
    PathBuf::from(format!(
        "{}/qa-catalog-test-{}",
        std::env::temp_dir().display(),
        Uuid::new_v4()
    ))
}

/// Like [`build_services`] but with a caller-supplied [`RepoSyncPort`].
fn build_services_with_engine(
    db: Db,
    authz: Arc<dyn AuthZResolverClient>,
    sync_engine: Arc<dyn RepoSyncPort>,
    repos_dir: PathBuf,
) -> Arc<ConcreteAppServices> {
    let db = Arc::new(DBProvider::<DomainError>::new(db));

    Arc::new(AppServices::new(
        Arc::new(OrmTestReposRepository),
        Arc::new(OrmCustomPlansRepository),
        Arc::new(OrmProductsRepository),
        Arc::new(OrmSshKeysRepository),
        Arc::new(OrmBundlesRepository),
        ServiceDeps {
            db,
            authz,
            credstore: Arc::new(MockCredStoreClient::empty()),
            sync_engine,
            bundle_store: Arc::new(NoopBundleStore),
            repos_dir,
            bundle_ttl: time::Duration::seconds(3600),
            // Zero TTL: the freshness cache never short-circuits, so every
            // sync call in these tests reaches the (inert) engine double.
            sync_cache: Arc::new(SyncCache::new(std::time::Duration::ZERO)),
        },
    ))
}

/// Convenience: build services with the default [`TenantScopedAuthZ`] double.
pub fn build_services_tenant_scoped(db: Db) -> Arc<ConcreteAppServices> {
    build_services(db, Arc::new(TenantScopedAuthZ))
}

/// Tenant-scoped services with a caller-supplied credstore double.
///
/// Exists for the SSH credential-resolution scoping tests: those need a
/// credstore that *would* hand back key material if the row lookup were to
/// succeed, so that a tenant-scoping failure shows up as a sync that
/// wrongly **succeeds** rather than as a silent second-order error.
pub fn build_services_tenant_scoped_with_credstore(
    db: Db,
    credstore: Arc<dyn CredStoreClientV1>,
) -> Arc<ConcreteAppServices> {
    let db = Arc::new(DBProvider::<DomainError>::new(db));

    Arc::new(AppServices::new(
        Arc::new(OrmTestReposRepository),
        Arc::new(OrmCustomPlansRepository),
        Arc::new(OrmProductsRepository),
        Arc::new(OrmSshKeysRepository),
        Arc::new(OrmBundlesRepository),
        ServiceDeps {
            db,
            authz: Arc::new(TenantScopedAuthZ),
            credstore,
            sync_engine: Arc::new(MaterializingSyncEngine),
            bundle_store: Arc::new(NoopBundleStore),
            repos_dir: throwaway_repos_dir(),
            bundle_ttl: time::Duration::seconds(3600),
            sync_cache: Arc::new(SyncCache::new(std::time::Duration::ZERO)),
        },
    ))
}

/// Services for the branch-cache refresher path
/// (`ReposService::list_refresh_targets` + `refresh_branches`): the ls-refs
/// half of the sync port returns `branches`, and the `AuthZ` double grants
/// the gear's system actor a scope covering `system_tenants` (see
/// [`SystemActorGrantAuthZ`]) while ordinary subjects stay tenant-scoped.
pub fn build_services_with_branch_listing(
    db: Db,
    branches: &[&str],
    system_tenants: Vec<Uuid>,
) -> Arc<ConcreteAppServices> {
    build_services_with_engine(
        db,
        Arc::new(SystemActorGrantAuthZ { system_tenants }),
        Arc::new(BranchListingSyncEngine {
            branches: branches.iter().map(|b| (*b).to_owned()).collect(),
        }),
        throwaway_repos_dir(),
    )
}

/// Tenant-scoped services rooted at a caller-owned `repos_dir`, with a sync
/// engine that materializes an **empty** branch snapshot instead of talking to
/// git.
///
/// This is what lets a DB-backed test give a repository real on-disk content:
/// the caller drives `sync_repo` (which flips `last_synced_at`, the gate every
/// content read checks) and then writes plan and test files straight into
/// `layout::branch_workdir(repos_dir, repo_id, branch)`. The git plane is
/// deliberately not exercised — these suites are about tenant isolation over
/// real content, not about cloning.
pub fn build_services_tenant_scoped_at(db: Db, repos_dir: PathBuf) -> Arc<ConcreteAppServices> {
    build_services_with_engine(
        db,
        Arc::new(TenantScopedAuthZ),
        Arc::new(MaterializingSyncEngine),
        repos_dir,
    )
}

/// [`RepoSyncPort`] double that creates the branch workdir and nothing else.
/// See [`build_services_tenant_scoped_at`].
struct MaterializingSyncEngine;

#[async_trait]
impl RepoSyncPort for MaterializingSyncEngine {
    async fn sync(
        &self,
        _url: &str,
        branch: &str,
        _credential: Option<&str>,
        _host_dir: &std::path::Path,
        branch_workdir: &std::path::Path,
    ) -> Result<SyncResult, DomainError> {
        std::fs::create_dir_all(branch_workdir)
            .map_err(|e| DomainError::Internal(e.to_string()))?;
        Ok(SyncResult {
            branches: vec![branch.to_owned()],
            head_commit: "0000000000000000000000000000000000000000".to_owned(),
        })
    }

    async fn list_remote_branches(
        &self,
        _url: &str,
        _credential: Option<&str>,
    ) -> Result<Vec<String>, DomainError> {
        Ok(Vec::new())
    }
}

/// Ground-truth read of every `qa_repo_branches` row as
/// `(repo_id, tenant_id, branch name)`, bypassing the service layer.
///
/// One of exactly **two** test-only `AccessScope::allow_all()` uses in the
/// crate (the other is [`seed_raw_custom_plan_row`]) — both ground-truth
/// reads/writes that bypass the service layer entirely, not scope narrowing
/// a production path relies on. Production code constructs `allow_all()` at
/// exactly one place, `domain::elevated::enumeration_scope`, for the two
/// nil-tenant lifecycle enumerations — see that module's doc; no other
/// production path queries unscoped. A scoped read cannot verify which
/// `tenant_id` a row actually carries, because
/// the scope filters on exactly that column and the service returns only branch
/// *names*. Reading the raw rows is what makes a wrong-tenant write visible.
pub async fn all_branch_rows(db: &Db) -> Vec<(Uuid, Uuid, String)> {
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    use toolkit_security::AccessScope;

    let conn = db.conn().expect("conn");
    let rows = crate::infra::storage::entity::repo_branch::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("read qa_repo_branches");

    let mut out: Vec<(Uuid, Uuid, String)> = rows
        .into_iter()
        .map(|m| (m.repo_id, m.tenant_id, m.name))
        .collect();
    out.sort();
    out
}

/// Insert a `qa_custom_plans` row with an **arbitrary `files` payload**, bypassing
/// the service and its conversions, and return its id.
///
/// Exists for one purpose: `qa_catalog_sdk::NewCustomPlanEntry::plan_path` is
/// mandatory, so no supported write path can any longer produce the two-element
/// array shape that rows written before that field carry. Something has to be able
/// to *create* that payload for a test to prove it still decodes, and this is it.
///
/// The insert goes through the real entity and the real column, so what is skipped
/// is only the encoder — which is exactly the point, since the payload under test
/// is one this build's encoder cannot emit. Raw SQL would be closer to the metal
/// but `toolkit_db::secure::DbConn` deliberately seals off `SeaORM`'s raw executor
/// from downstream crates, and that boundary is worth more than the last inch of
/// fidelity here.
///
/// Second of the crate's two test-only `AccessScope::allow_all()` uses; see
/// [`all_branch_rows`], including for the one place that constructor is
/// legitimate in production code.
pub async fn seed_raw_custom_plan_row(
    db: &Db,
    tenant_id: Uuid,
    name: &str,
    files: serde_json::Value,
) -> Uuid {
    use sea_orm::ActiveValue;
    use time::OffsetDateTime;
    use toolkit_db::secure::secure_insert;
    use toolkit_security::AccessScope;

    let id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();
    let am = crate::infra::storage::entity::custom_plan::ActiveModel {
        id: ActiveValue::Set(id),
        tenant_id: ActiveValue::Set(tenant_id),
        name: ActiveValue::Set(name.to_owned()),
        files: ActiveValue::Set(files),
        tags: ActiveValue::Set(serde_json::json!([])),
        timeout_seconds: ActiveValue::Set(None),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
    };

    let conn = db.conn().expect("conn");
    secure_insert::<crate::infra::storage::entity::custom_plan::Entity>(
        am,
        &AccessScope::allow_all(),
        &conn,
    )
    .await
    .expect("seed qa_custom_plans row");
    id
}
