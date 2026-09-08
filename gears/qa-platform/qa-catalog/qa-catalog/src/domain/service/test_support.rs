//! Shared test doubles for the `domain::service` unit tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, AuthZResolverError};
use qa_catalog_sdk::{NewTestRepository, TestRepository, TestRepositoryUpdate};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::DbProvider;
use super::authz_surface::ENFORCED;
use crate::domain::error::DomainError;
use crate::domain::repos::{RefreshTarget, SshKeysRepository, TestReposRepository};
use toolkit_security::PlatformSecurityContext;
use toolkit_canonical_errors::CanonicalError;

/// Build a `SecurityContext` for `tenant_id` with a fresh random subject.
pub(super) fn ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap()
}

/// Build a `TestRepository` fixture. `synced` sets `last_synced_at` (with a
/// clear `sync_error`), i.e. the state after a successful default-branch
/// (`"main"`) sync.
pub(super) fn repo_fixture(id: Uuid, synced: bool) -> TestRepository {
    let now = OffsetDateTime::now_utc();
    TestRepository {
        id,
        product_id: Uuid::new_v4(),
        name: "test-repo".to_owned(),
        url: "https://example.com/org/repo.git".to_owned(),
        default_branch: "main".to_owned(),
        content_root: String::new(),
        credential_ref: None,
        last_synced_at: synced.then_some(now),
        sync_error: None,
        created_at: now,
        updated_at: now,
    }
}

/// The mocks in this module never touch the database — the `DBRunner`
/// argument is ignored by every mock method — so a real `Db` handle is only
/// needed to satisfy `Arc<DbProvider>` fields, produce `DbConn` values to
/// pass through, and back the (table-free) transactions `sync_repo` /
/// `purge_expired` open. No migrations are required.
pub(super) async fn test_db_provider() -> Arc<DbProvider> {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("failed to connect to in-memory sqlite");
    Arc::new(DBProvider::<DomainError>::new(db))
}

/// Shared decision logic for the permissive `AuthZ` test doubles: always
/// grants access, returning a tenant `IN` constraint derived from the
/// subject's tenant (mirrors a real PDP's default tenant-isolation policy).
/// The mocks in this test tier ignore the resulting `AccessScope` entirely,
/// but the PEP flow still needs a well-formed PDP response to compile one.
///
/// Shared with the DB-backed tenant-scoping tier — the single definition
/// lives in `crate::test_support` (mirrors qa-environments).
pub(super) use crate::test_support::permissive_response;

/// Permissive `AuthZ` resolver: always grants access. Use `RecordingAuthZ`
/// (in `products_tests`) when a test needs to assert *which* action /
/// resource-id the service requested.
pub(super) struct PermissiveAuthZ;

#[async_trait]
impl AuthZResolverApi for PermissiveAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(permissive_response(&request))
    }
}

/// The [`ENFORCED`] entry for `(resource_type, action)`, as the `&'static
/// str`s that list holds.
///
/// **Panics** when this gear enforces no such pair, which is the point: it is
/// how a test names a permission without hand-typing one. A pair that came
/// through here is a pair the permission catalog declares an
/// `AuthzPermissionV1` instance for, because
/// `crate::gts::permissions_tests::the_catalog_matches_the_enforced_surface`
/// pins the two to each other in both directions.
pub(super) fn enforced_pair(resource_type: &str, action: &str) -> (&'static str, &'static str) {
    *ENFORCED
        .iter()
        .find(|&&(r, a)| r == resource_type && a == action)
        .unwrap_or_else(|| {
            panic!(
                "qa-catalog's PEP enforces no ({resource_type}, {action}) pair, so no role \
                 could be granted it -- see `authz_surface::ENFORCED`"
            )
        })
}

/// `AuthZ` resolver granting exactly **one** `(resource_type, action)` pair
/// and denying every other.
///
/// **No `AuthZ` double in this crate varied its decision by action.** The
/// seven that existed all answer the same way whatever is asked:
/// [`PermissiveAuthZ`], [`crate::test_support::TenantScopedAuthZ`],
/// [`crate::test_support::SystemActorGrantAuthZ`] (which varies by *subject*,
/// never by action) and the three `RecordingAuthZ` copies (`products_tests`,
/// `tests_tenant_scoping`, `bundles_tests`) grant every pair;
/// [`crate::test_support::DenyAllAuthZ`] refuses every pair. So none of them
/// can express "this principal holds grants, just not *this* one", and a
/// denial any of them produces is a denial of something nothing could have
/// authorized. This double is the one that discriminates, which is what makes
/// a denial attributable to a missing grant — see
/// `products_tests::an_action_without_a_grant_is_denied`.
///
/// The granted pair answers with [`permissive_response`], so the PEP compiles
/// a real tenant-scoped `AccessScope` from it and the authorized operation
/// runs the path a granted caller runs. Every other pair answers with the
/// `decision: false` shape `DenyAllAuthZ` returns for everything, which the
/// enforcer turns into `EnforcerError::Denied` and hence
/// [`DomainError::Forbidden`].
///
/// The grant is resolved through [`enforced_pair`] rather than taken as two
/// strings, so it can only name a pair this gear actually enforces: a typo'd
/// action would otherwise build a fixture that grants *nothing*, and the
/// denial half of a test would then pass for the wrong reason.
pub(super) struct SelectiveGrantAuthZ {
    granted: (&'static str, &'static str),
}

impl SelectiveGrantAuthZ {
    /// Grant `(resource_type, action)` — which must be one of [`ENFORCED`]'s
    /// pairs — and nothing else.
    pub(super) fn granting(resource_type: &str, action: &str) -> Self {
        Self {
            granted: enforced_pair(resource_type, action),
        }
    }
}

#[async_trait]
impl AuthZResolverApi for SelectiveGrantAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let asked = (
            request.resource.resource_type.as_str(),
            request.action.name.as_str(),
        );
        if asked == self.granted {
            return Ok(permissive_response(&request));
        }
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// `TestReposRepository` double holding a single configurable repository.
///
/// `get` respects the requested id; `update_sync_state` and
/// `replace_branches` mutate/record state so the sync tests can assert what
/// the service persisted. List/create/delete are implemented minimally for
/// the CRUD-path tests.
///
/// The SDK `TestRepository` model carries no `tenant_id`, so the owning
/// tenant is held alongside it: `list_refresh_targets` reports it (rather
/// than a hardcoded nil, which would mask a service passing the wrong tenant
/// to `replace_branches`), and `recorded_branch_tenant` exposes the tenant
/// the service actually wrote branch rows under.
pub(super) struct MockTestReposRepository {
    repo: Mutex<Option<TestRepository>>,
    tenant_id: Uuid,
    branches: Mutex<Vec<String>>,
    recorded_branch_tenant: Mutex<Option<Uuid>>,
    /// When set, the stored repository's `url` is replaced with this value
    /// right after the FIRST `get` returns — a deterministic stand-in for a
    /// concurrent `update_repo` landing between `sync_repo`'s pre-lock read
    /// and its under-lock re-read.
    url_after_first_get: Mutex<Option<String>>,
}

impl MockTestReposRepository {
    pub(super) fn with_repo(repo: TestRepository) -> Self {
        Self::with_repo_in_tenant(repo, Uuid::new_v4())
    }

    /// Like [`with_repo`](Self::with_repo) but pins the owning tenant, so a
    /// test can assert the service round-trips *that* tenant.
    pub(super) fn with_repo_in_tenant(repo: TestRepository, tenant_id: Uuid) -> Self {
        Self {
            repo: Mutex::new(Some(repo)),
            tenant_id,
            branches: Mutex::new(Vec::new()),
            recorded_branch_tenant: Mutex::new(None),
            url_after_first_get: Mutex::new(None),
        }
    }

    /// Like [`with_repo`](Self::with_repo), but the repository's `url`
    /// changes to `moved_url` right after the first `get` — see the
    /// `url_after_first_get` field docs.
    pub(super) fn with_repo_swapping_url(repo: TestRepository, moved_url: &str) -> Self {
        let mock = Self::with_repo(repo);
        *mock.url_after_first_get.lock().unwrap() = Some(moved_url.to_owned());
        mock
    }

    pub(super) fn none() -> Self {
        Self {
            repo: Mutex::new(None),
            tenant_id: Uuid::new_v4(),
            branches: Mutex::new(Vec::new()),
            recorded_branch_tenant: Mutex::new(None),
            url_after_first_get: Mutex::new(None),
        }
    }

    pub(super) fn recorded_branches(&self) -> Vec<String> {
        self.branches.lock().unwrap().clone()
    }

    /// The `tenant_id` the last `replace_branches` call was handed.
    pub(super) fn recorded_branch_tenant(&self) -> Option<Uuid> {
        *self.recorded_branch_tenant.lock().unwrap()
    }
}

#[async_trait]
impl TestReposRepository for MockTestReposRepository {
    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TestRepository>, DomainError> {
        let found = self
            .repo
            .lock()
            .unwrap()
            .as_ref()
            .filter(|r| r.id == id)
            .cloned();
        if found.is_some()
            && let Some(moved_url) = self.url_after_first_get.lock().unwrap().take()
            && let Some(repo) = self.repo.lock().unwrap().as_mut()
        {
            repo.url = moved_url;
        }
        Ok(found)
    }

    async fn list<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
    ) -> Result<Vec<TestRepository>, DomainError> {
        Ok(self.repo.lock().unwrap().iter().cloned().collect())
    }

    async fn create<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        new: NewTestRepository,
    ) -> Result<TestRepository, DomainError> {
        let now = OffsetDateTime::now_utc();
        let created = TestRepository {
            id: Uuid::new_v4(),
            product_id: new.product_id,
            name: new.name,
            url: new.url,
            default_branch: new.default_branch,
            content_root: new.content_root,
            credential_ref: new.credential_ref,
            last_synced_at: None,
            sync_error: None,
            created_at: now,
            updated_at: now,
        };
        *self.repo.lock().unwrap() = Some(created.clone());
        Ok(created)
    }

    async fn update<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
        update: TestRepositoryUpdate,
        invalidate_working_copy: bool,
    ) -> Result<Option<TestRepository>, DomainError> {
        let mut guard = self.repo.lock().unwrap();
        let Some(repo) = guard.as_mut().filter(|r| r.id == id) else {
            return Ok(None);
        };
        // `product_id` and `default_branch` ARE mutable through update — the
        // real repository sets both (see `OrmTestReposRepository::update`),
        // so a mock that left either untouched would hide a service failing
        // to pass them through.
        repo.product_id = update.product_id;
        repo.name = update.name;
        repo.url = update.url;
        repo.default_branch = update.default_branch;
        repo.content_root = update.content_root;
        repo.credential_ref = update.credential_ref;
        if invalidate_working_copy {
            repo.last_synced_at = None;
            repo.sync_error = None;
        }
        repo.updated_at = OffsetDateTime::now_utc();
        Ok(Some(repo.clone()))
    }

    async fn delete<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let mut guard = self.repo.lock().unwrap();
        if guard.as_ref().is_some_and(|r| r.id == id) {
            *guard = None;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn update_sync_state<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
        last_synced_at: Option<OffsetDateTime>,
        sync_error: Option<String>,
    ) -> Result<Option<TestRepository>, DomainError> {
        let mut guard = self.repo.lock().unwrap();
        let Some(repo) = guard.as_mut().filter(|r| r.id == id) else {
            return Ok(None);
        };
        repo.last_synced_at = last_synced_at;
        repo.sync_error = sync_error;
        repo.updated_at = OffsetDateTime::now_utc();
        Ok(Some(repo.clone()))
    }

    async fn replace_branches<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        tenant_id: Uuid,
        _repo_id: Uuid,
        branches: Vec<String>,
    ) -> Result<(), DomainError> {
        *self.branches.lock().unwrap() = branches;
        *self.recorded_branch_tenant.lock().unwrap() = Some(tenant_id);
        Ok(())
    }

    async fn list_branches<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _repo_id: Uuid,
    ) -> Result<Vec<String>, DomainError> {
        let mut branches = self.branches.lock().unwrap().clone();
        branches.sort();
        Ok(branches)
    }

    async fn list_refresh_targets<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
    ) -> Result<Vec<RefreshTarget>, DomainError> {
        // The real tenant, NOT a nil placeholder: the refresher binds its
        // per-repository system context to this value, so a hardcoded nil
        // here would hide a wrong-tenant regression.
        let tenant_id = self.tenant_id;
        Ok(self
            .repo
            .lock()
            .unwrap()
            .iter()
            .map(|r| RefreshTarget {
                repo_id: r.id,
                tenant_id,
            })
            .collect())
    }
}

/// `SshKeysRepository` double holding zero or one key row.
///
/// Only `find_by_id` is exercised: `ReposService` uses this repository for
/// exactly one thing — turning an SSH remote's `credential_ref` into the
/// `credstore_ref` that holds the key material.
pub(super) struct MockSshKeysRepository {
    key: Option<qa_catalog_sdk::SshKey>,
}

impl MockSshKeysRepository {
    pub(super) fn none() -> Self {
        Self { key: None }
    }

    /// A key row with `id` whose material lives at `credstore_ref`.
    pub(super) fn with_key(id: Uuid, credstore_ref: &str) -> Self {
        Self {
            key: Some(qa_catalog_sdk::SshKey {
                id,
                name: "deploy-key".to_owned(),
                credstore_ref: credstore_ref.to_owned(),
                fingerprint: "SHA256:test".to_owned(),
                created_at: OffsetDateTime::now_utc(),
            }),
        }
    }
}

#[async_trait]
impl SshKeysRepository for MockSshKeysRepository {
    async fn list<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
    ) -> Result<Vec<qa_catalog_sdk::SshKey>, DomainError> {
        Ok(self.key.clone().into_iter().collect())
    }

    async fn create<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _tenant_id: Uuid,
        _name: String,
        _credstore_ref: String,
        _fingerprint: String,
    ) -> Result<qa_catalog_sdk::SshKey, DomainError> {
        unimplemented!("ReposService never creates ssh keys")
    }

    async fn find_by_id<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<qa_catalog_sdk::SshKey>, DomainError> {
        Ok(self.key.clone().filter(|k| k.id == id))
    }

    async fn delete<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
    ) -> Result<bool, DomainError> {
        unimplemented!("ReposService never deletes ssh keys")
    }
}
