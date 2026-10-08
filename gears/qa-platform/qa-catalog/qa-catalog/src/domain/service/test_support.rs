//! Shared test doubles for the `domain::service` unit tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use qa_catalog_sdk::{NewTestRepository, TestRepository, TestRepositoryUpdate};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::DbProvider;
use super::authz_surface::ENFORCED;
use super::branch_snapshot::BranchSync;
use crate::domain::error::DomainError;
use crate::domain::ports::repo_sync::{RepoSyncPort, SyncResult};
use crate::domain::repos::{RefreshTarget, SshKeysRepository, TestReposRepository};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

/// Build a `SecurityContext` for `tenant_id` with a fresh random subject.
pub(super) fn ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap()
}

/// The revision [`repo_fixture`] and the mock sync engine agree a successful
/// sync left behind. A real 40-character hex object id, so a test that
/// asserts on the value is asserting on something shaped like what git
/// produces.
pub(super) const SYNCED_HEAD_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Build a `TestRepository` fixture. `synced` sets `last_synced_at` **and a
/// `head_commit`** (with a clear `sync_error`), i.e. the state after a
/// successful default-branch (`"main"`) sync. Both are set together because
/// that is the only way a real sync leaves the row, and the discovery cache
/// keys on the revision — a fixture that set only the timestamp would be a
/// state the write path cannot produce.
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
        head_commit: synced.then(|| SYNCED_HEAD_COMMIT.to_owned()),
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
/// to `replace_branches`).
pub(super) struct MockTestReposRepository {
    repo: Mutex<Option<TestRepository>>,
    tenant_id: Uuid,
    branches: Mutex<Vec<String>>,
    replace_calls: Mutex<usize>,
    /// When set, the stored repository's `url` is replaced with this value
    /// right after the FIRST `get` returns — a deterministic stand-in for a
    /// concurrent `update_repo` landing between `sync_repo`'s pre-lock read
    /// and its under-lock re-read.
    url_after_first_get: Mutex<Option<String>>,
    /// Run once, right after the next `get` has read the row: a
    /// deterministic stand-in for something landing between a caller's row
    /// read and whatever it reads next.
    after_get_hook: AfterGetHook,
}

type AfterGetHook = Mutex<
    Option<
        Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send>,
    >,
>;

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
            replace_calls: Mutex::new(0),
            url_after_first_get: Mutex::new(None),
            after_get_hook: Mutex::new(None),
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

    /// Run `hook` once, right after the next `get` has read the row.
    pub(super) fn with_after_get_hook<F>(self, hook: impl FnOnce() -> F + Send + 'static) -> Self
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        *self.after_get_hook.lock().unwrap() = Some(Box::new(move || Box::pin(hook())));
        self
    }

    pub(super) fn none() -> Self {
        Self {
            repo: Mutex::new(None),
            tenant_id: Uuid::new_v4(),
            branches: Mutex::new(Vec::new()),
            replace_calls: Mutex::new(0),
            url_after_first_get: Mutex::new(None),
            after_get_hook: Mutex::new(None),
        }
    }

    /// Seed the branch cache, as an earlier sync or refresher pass would
    /// have left it.
    pub(super) fn set_cached_branches(&self, branches: &[&str]) {
        *self.branches.lock().unwrap() = branches.iter().map(|b| (*b).to_owned()).collect();
    }

    /// The stored repository row as it is now.
    pub(super) fn current(&self) -> Option<TestRepository> {
        self.repo.lock().unwrap().clone()
    }

    pub(super) fn recorded_branches(&self) -> Vec<String> {
        self.branches.lock().unwrap().clone()
    }

    /// How many times `replace_branches` was called.
    pub(super) fn replace_calls(&self) -> usize {
        *self.replace_calls.lock().unwrap()
    }

    /// Simulate a successful sync that found **nothing new**: bump the
    /// stored repository's `last_synced_at` and leave `head_commit` where it
    /// is, which is exactly what `record_sync_success` writes when the fetch
    /// returns the same tip. A discovery cache keyed on the revision must
    /// treat this as "the working copy is unchanged" and keep its answer.
    pub(super) fn touch_synced_at(&self, at: OffsetDateTime) {
        if let Some(repo) = self.repo.lock().unwrap().as_mut() {
            repo.last_synced_at = Some(at);
        }
    }

    /// Simulate a successful sync that **advanced the branch**: a new
    /// `head_commit` together with the new `last_synced_at` that always
    /// accompanies it. This is the pair `record_sync_success` writes, so a
    /// test that used this is exercising a state the write path can actually
    /// produce.
    pub(super) fn touch_head_commit(&self, at: OffsetDateTime, head_commit: &str) {
        if let Some(repo) = self.repo.lock().unwrap().as_mut() {
            repo.last_synced_at = Some(at);
            repo.head_commit = Some(head_commit.to_owned());
        }
    }

    /// Simulate a `content_root` change landing on the stored repository.
    ///
    /// `RepoService::update_repo` clears the synced state for this, and the
    /// re-sync that follows restores the **same** `head_commit` when the
    /// branch has not moved — so the revision alone cannot see the change.
    /// This helper reproduces the end state of that sequence: a new
    /// `content_root` under an unchanged revision.
    pub(super) fn set_content_root(&self, content_root: &str) {
        if let Some(repo) = self.repo.lock().unwrap().as_mut() {
            repo.content_root = content_root.to_owned();
        }
    }

    /// Simulate a product reassignment landing on the stored repository —
    /// `RepoService::update_repo` writes `product_id` unconditionally and
    /// does *not* advance `last_synced_at` for it (only a `url`/
    /// `content_root` change does). A discovery cache keyed only on
    /// `last_synced_at` would miss this.
    pub(super) fn set_product_id(&self, product_id: Uuid) {
        if let Some(repo) = self.repo.lock().unwrap().as_mut() {
            repo.product_id = product_id;
        }
    }

    /// Simulate an `update_repo` of the credential landing on the stored row.
    pub(super) fn set_credential_ref(&self, credential_ref: Option<&str>) {
        if let Some(repo) = self.repo.lock().unwrap().as_mut() {
            repo.credential_ref = credential_ref.map(ToOwned::to_owned);
        }
    }

    /// Simulate another branch's failed sync landing on the stored row: the
    /// repository-wide `sync_error`, nothing else touched.
    pub(super) fn set_sync_error(&self, sync_error: Option<&str>) {
        if let Some(repo) = self.repo.lock().unwrap().as_mut() {
            repo.sync_error = sync_error.map(ToOwned::to_owned);
        }
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
        let hook = self.after_get_hook.lock().unwrap().take();
        if let Some(hook) = hook {
            hook().await;
        }
        Ok(found)
    }

    async fn owner_tenant<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Uuid>, DomainError> {
        // The pinned tenant, as `list_refresh_targets` reports it: a request
        // path and the refresher must agree on the owner.
        Ok(self
            .repo
            .lock()
            .unwrap()
            .as_ref()
            .filter(|r| r.id == id)
            .map(|_| self.tenant_id))
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
            head_commit: None,
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
        head_commit: Option<String>,
        sync_error: Option<String>,
    ) -> Result<Option<TestRepository>, DomainError> {
        let mut guard = self.repo.lock().unwrap();
        let Some(repo) = guard.as_mut().filter(|r| r.id == id) else {
            return Ok(None);
        };
        repo.last_synced_at = last_synced_at;
        repo.head_commit = head_commit;
        repo.sync_error = sync_error;
        repo.updated_at = OffsetDateTime::now_utc();
        Ok(Some(repo.clone()))
    }

    async fn replace_branches<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        repo_id: Uuid,
        branches: Vec<String>,
    ) -> Result<(), DomainError> {
        // Same contract as `OrmTestReposRepository::replace_branches`: an
        // unresolvable repository is `NotFound`, and the cache ends as the
        // listing, duplicates collapsed. The owning tenant is the adapter's to
        // read off the row, so there is nothing to record here.
        if self
            .repo
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|r| r.id != repo_id)
        {
            return Err(DomainError::NotFound { id: repo_id });
        }
        let listed: std::collections::BTreeSet<String> = branches.into_iter().collect();
        *self.branches.lock().unwrap() = listed.into_iter().collect();
        *self.replace_calls.lock().unwrap() += 1;
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

// ---------------------------------------------------------------------------
// Sync engine double
// ---------------------------------------------------------------------------

enum SyncBehavior {
    /// Return these branches and write `(relative path, content)` fixture
    /// files into the branch snapshot first (mirrors a real materialization).
    Succeed {
        branches: Vec<String>,
        files: Vec<(String, String)>,
    },
    /// Fail with this message (as the gix adapter would via `DomainError`).
    Fail { message: String },
    /// The content sync fails with the error this builds — for the variants
    /// the service classifies (timeout, byte budget).
    FailWith(fn() -> DomainError),
}

/// How the double's ls-refs half fails, when told to: the two classes the lazy
/// read answers differently (DESIGN §3.3).
#[derive(Clone, Copy, Debug)]
pub(super) enum LsRefsFailure {
    /// The remote could not be reached.
    Unreachable,
    /// The remote refused the credential.
    CredentialRejected,
    /// The remote did not answer within the deadline (the adapter's
    /// `RemoteTimedOut`).
    TimedOut,
}

/// In-memory `RepoSyncPort` double: programmed to succeed (optionally
/// writing fixture files into the branch snapshot, as the real gix adapter
/// materializes one) or fail with a given message.
///
/// By default `Succeed`/`Fail` drive both halves — the content sync and the
/// ls-refs listing. [`Self::failing_sync_of`] splits them: the remote lists
/// branches, and the content sync of any of them fails.
pub(super) struct MockRepoSyncPort {
    behavior: SyncBehavior,
    /// When set, what `list_remote_branches` answers regardless of
    /// `behavior`.
    remote_branches_override: Option<Vec<String>>,
    seen_credentials: Mutex<Vec<Option<String>>>,
    seen_urls: Mutex<Vec<String>>,
    /// Every branch the content-sync half was asked to materialize.
    synced_branches: Mutex<Vec<String>>,
    /// How many times the ls-refs half was called.
    ls_refs_calls: Mutex<usize>,
    /// When set, how every call to the remote fails, ahead of everything
    /// else: ls-refs, and the content sync too — a remote that is down, or
    /// that refuses the credential, refuses both.
    ls_refs_failure: Mutex<Option<LsRefsFailure>>,
    /// Run once, inside the next ls-refs call: a deterministic stand-in for
    /// something landing while the remote is being listed.
    ls_refs_hook: LsRefsHook,
}

type LsRefsHook = Mutex<Option<Box<dyn FnOnce() + Send>>>;

impl MockRepoSyncPort {
    fn with_behavior(
        behavior: SyncBehavior,
        remote_branches_override: Option<Vec<String>>,
    ) -> Self {
        Self {
            behavior,
            remote_branches_override,
            seen_credentials: Mutex::new(Vec::new()),
            seen_urls: Mutex::new(Vec::new()),
            synced_branches: Mutex::new(Vec::new()),
            ls_refs_calls: Mutex::new(0),
            ls_refs_failure: Mutex::new(None),
            ls_refs_hook: Mutex::new(None),
        }
    }

    /// Run `hook` once, inside the next ls-refs call.
    pub(super) fn with_ls_refs_hook(self, hook: impl FnOnce() + Send + 'static) -> Self {
        *self.ls_refs_hook.lock().unwrap() = Some(Box::new(hook));
        self
    }

    /// Make every ls-refs call (and content sync) fail with `failure` until
    /// told otherwise.
    pub(super) fn with_ls_refs_failure(self, failure: LsRefsFailure) -> Self {
        *self.ls_refs_failure.lock().unwrap() = Some(failure);
        self
    }

    pub(super) fn set_ls_refs_failure(&self, failure: Option<LsRefsFailure>) {
        *self.ls_refs_failure.lock().unwrap() = failure;
    }

    pub(super) fn succeeding(branches: Vec<String>, files: Vec<(String, String)>) -> Self {
        Self::with_behavior(SyncBehavior::Succeed { branches, files }, None)
    }

    pub(super) fn failing(message: &str) -> Self {
        Self::with_behavior(
            SyncBehavior::Fail {
                message: message.to_owned(),
            },
            None,
        )
    }

    /// The remote lists `remote_branches`, and every content sync fails with
    /// `message` — a branch that exists but cannot be fetched.
    pub(super) fn failing_sync_of(remote_branches: Vec<String>, message: &str) -> Self {
        Self::with_behavior(
            SyncBehavior::Fail {
                message: message.to_owned(),
            },
            Some(remote_branches),
        )
    }

    /// The remote lists `remote_branches`, and every content sync fails with
    /// the error `error` builds — for the timeout and budget classes.
    pub(super) fn failing_sync_with(
        remote_branches: Vec<String>,
        error: fn() -> DomainError,
    ) -> Self {
        Self::with_behavior(SyncBehavior::FailWith(error), Some(remote_branches))
    }

    /// Every `credential` argument the engine was handed, in call order.
    /// `Some(material)` proves the resolved credstore secret actually reached
    /// the engine; `None` is a public-repository call.
    pub(super) fn seen_credentials(&self) -> Vec<Option<String>> {
        self.seen_credentials.lock().unwrap().clone()
    }

    /// Every `url` the content-sync half was handed, in call order.
    pub(super) fn seen_urls(&self) -> Vec<String> {
        self.seen_urls.lock().unwrap().clone()
    }

    /// Every branch the content-sync half was asked for, in call order.
    pub(super) fn synced_branches(&self) -> Vec<String> {
        self.synced_branches.lock().unwrap().clone()
    }

    /// How many times the ls-refs half was called.
    pub(super) fn ls_refs_calls(&self) -> usize {
        *self.ls_refs_calls.lock().unwrap()
    }

    /// The `list_remote_branches` (ls-refs) half of the double, used by the
    /// branch-cache refresher tests. `Succeed`/`Fail` drive both halves
    /// unless an override was set.
    /// The programmed remote failure, if any, as the adapter reports it for
    /// the step named `context`.
    fn remote_failure(&self, context: &str) -> Option<DomainError> {
        match *self.ls_refs_failure.lock().unwrap() {
            Some(LsRefsFailure::Unreachable) => Some(DomainError::SyncFailed {
                message: format!(
                    "{context}: failed to connect to https://git.example/r.git: Connection refused"
                ),
            }),
            Some(LsRefsFailure::CredentialRejected) => Some(DomainError::CredentialRejected {
                message: format!(
                    "{context}: Credentials provided for \"https://git.example/r.git\" were not accepted by the remote"
                ),
            }),
            Some(LsRefsFailure::TimedOut) => Some(DomainError::RemoteTimedOut {
                message: format!("{context}: did not finish within 30 s"),
            }),
            None => None,
        }
    }

    fn remote_branches(&self) -> Result<Vec<String>, DomainError> {
        if let Some(err) = self.remote_failure("ls-refs failed") {
            return Err(err);
        }
        if let Some(branches) = &self.remote_branches_override {
            return Ok(branches.clone());
        }
        match &self.behavior {
            SyncBehavior::Succeed { branches, .. } => Ok(branches.clone()),
            SyncBehavior::Fail { message } => Err(DomainError::Internal(message.clone())),
            SyncBehavior::FailWith(error) => Err(error()),
        }
    }
}

#[async_trait]
impl RepoSyncPort for MockRepoSyncPort {
    async fn sync(
        &self,
        url: &str,
        branch: &str,
        credential: Option<&str>,
        _host_dir: &Path,
        branch_workdir: &Path,
    ) -> Result<SyncResult, DomainError> {
        self.seen_credentials
            .lock()
            .unwrap()
            .push(credential.map(ToOwned::to_owned));
        self.seen_urls.lock().unwrap().push(url.to_owned());
        self.synced_branches.lock().unwrap().push(branch.to_owned());
        // A real fetch suspends; yielding here lets a concurrent reader run
        // while this sync is in flight, which is the interleaving the
        // "concurrent first reads sync once" tests need to exercise.
        tokio::task::yield_now().await;
        if let Some(err) = self.remote_failure("fetch negotiation failed") {
            return Err(err);
        }
        match &self.behavior {
            SyncBehavior::Succeed { branches, files } => {
                // The snapshot directory exists after a real sync even when
                // the branch has no files.
                std::fs::create_dir_all(branch_workdir).unwrap();
                for (rel, content) in files {
                    let path = branch_workdir.join(rel);
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).unwrap();
                    }
                    std::fs::write(&path, content).unwrap();
                }
                Ok(SyncResult {
                    branches: branches.clone(),
                    head_commit: "deadbeef".to_owned(),
                })
            }
            SyncBehavior::Fail { message } => Err(DomainError::Internal(message.clone())),
            SyncBehavior::FailWith(error) => Err(error()),
        }
    }

    async fn list_remote_branches(
        &self,
        _url: &str,
        credential: Option<&str>,
    ) -> Result<Vec<String>, DomainError> {
        self.seen_credentials
            .lock()
            .unwrap()
            .push(credential.map(ToOwned::to_owned));
        *self.ls_refs_calls.lock().unwrap() += 1;
        let hook = self.ls_refs_hook.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        self.remote_branches()
    }
}

/// A `ReposService` over `repos` and `engine`, for the readers' lazy-sync
/// tests: the plan and bundle readers hold it as their `BranchSync`.
///
/// `freshness_ttl` is the sync cache's TTL; a non-zero one is what lets a
/// second concurrent reader find the branch fresh once the first has synced.
pub(super) async fn branch_sync_over(
    repos: Arc<MockTestReposRepository>,
    engine: Arc<MockRepoSyncPort>,
    repos_dir: std::path::PathBuf,
    freshness_ttl: std::time::Duration,
) -> Arc<dyn BranchSync> {
    branch_sync_with(
        repos,
        engine,
        repos_dir,
        super::sync_cache::SyncCache::new(freshness_ttl),
        Arc::new(PermissiveAuthZ),
    )
    .await
}

/// A `ReposService` over `repos` and `engine` with a caller-built sync cache
/// and `AuthZ` double — for the lazy-read tests that need a backoff window or a
/// reader who lacks `SYNC`.
pub(super) async fn branch_sync_with(
    repos: Arc<MockTestReposRepository>,
    engine: Arc<MockRepoSyncPort>,
    repos_dir: std::path::PathBuf,
    cache: super::sync_cache::SyncCache,
    authz: Arc<dyn AuthZResolverApi>,
) -> Arc<dyn BranchSync> {
    Arc::new(super::repos::ReposService::new(
        test_db_provider().await,
        repos,
        Arc::new(MockSshKeysRepository::none()),
        Arc::new(credstore_sdk::test_util::MockCredStoreClient::empty()),
        engine,
        repos_dir,
        Arc::new(cache),
        authz_resolver_sdk::PolicyEnforcer::new(authz),
    ))
}

/// [`branch_sync_over`] with a caller-supplied credential store — for the
/// lazy-read tests that need to know which identity read the secret.
pub(super) async fn branch_sync_with_credstore(
    repos: Arc<MockTestReposRepository>,
    engine: Arc<MockRepoSyncPort>,
    repos_dir: std::path::PathBuf,
    freshness_ttl: std::time::Duration,
    credstore: Arc<dyn credstore_sdk::CredStoreClientV1>,
) -> Arc<dyn BranchSync> {
    Arc::new(super::repos::ReposService::new(
        test_db_provider().await,
        repos,
        Arc::new(MockSshKeysRepository::none()),
        credstore,
        engine,
        repos_dir,
        Arc::new(super::sync_cache::SyncCache::new(freshness_ttl)),
        authz_resolver_sdk::PolicyEnforcer::new(Arc::new(PermissiveAuthZ)),
    ))
}

/// A `BranchSync` over a remote with no branches at all: every read of a
/// branch without a snapshot is `BranchNotFound`, and the content-sync half
/// is never reached. For reader tests that do not exercise the lazy sync.
pub(super) async fn branch_sync_over_an_empty_remote(
    repos: Arc<MockTestReposRepository>,
    repos_dir: std::path::PathBuf,
) -> Arc<dyn BranchSync> {
    branch_sync_over(
        repos,
        Arc::new(MockRepoSyncPort::succeeding(Vec::new(), Vec::new())),
        repos_dir,
        std::time::Duration::ZERO,
    )
    .await
}

/// One secret [`SharingCredStore`] holds: who owns it, in which tenant,
/// shared how.
pub(super) struct StoredSecret {
    /// The reference, a bare name as every credential reference is.
    pub(super) reference: &'static str,
    pub(super) value: &'static str,
    pub(super) tenant: Uuid,
    pub(super) owner: Uuid,
    pub(super) sharing: credstore_sdk::SharingMode,
}

/// A credential store that applies credstore's sharing rule to the caller.
///
/// `credstore_sdk::test_util::MockCredStoreClient` answers the same value to
/// every caller, so it cannot tell a user's read from the qa-catalog system
/// actor's — the distinction a sync and a read have to make. This double
/// applies the visibility predicate of credstore's resolver (`resolve_for_get`
/// in `gears/credstore/credstore/src/infra/storage/repo_impl/reads.rs`) for a
/// one-tenant chain: `Private` is visible to its owner only, `Tenant` and
/// `Shared` to every subject of the owning tenant. A miss is `Ok(None)`, the
/// SDK's single anti-enumeration surface. Read-only: every write takes the
/// trait's default, which fails.
pub(super) struct SharingCredStore {
    secrets: Vec<StoredSecret>,
}

impl SharingCredStore {
    pub(super) const fn new(secrets: Vec<StoredSecret>) -> Self {
        Self { secrets }
    }

    fn visible(
        secret: &StoredSecret,
        ctx: &SecurityContext,
        key: &credstore_sdk::SecretRef,
    ) -> bool {
        secret.reference == key.as_ref()
            && secret.tenant == ctx.subject_tenant_id()
            && match secret.sharing {
                credstore_sdk::SharingMode::Private => secret.owner == ctx.subject_id(),
                credstore_sdk::SharingMode::Tenant | credstore_sdk::SharingMode::Shared => true,
            }
    }
}

#[async_trait]
impl credstore_sdk::CredStoreClientV1 for SharingCredStore {
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &credstore_sdk::SecretRef,
    ) -> Result<Option<credstore_sdk::GetSecretResponse>, credstore_sdk::CredStoreError> {
        Ok(self
            .secrets
            .iter()
            .find(|s| Self::visible(s, ctx, key))
            .map(|s| credstore_sdk::GetSecretResponse {
                value: credstore_sdk::SecretValue::new(s.value.as_bytes().to_vec()),
                id: Uuid::nil(),
                owner_tenant_id: credstore_sdk::TenantId(s.tenant),
                sharing: s.sharing,
                is_inherited: false,
                version: 1,
                secret_type: credstore_sdk::SecretType::generic().gts_id().to_owned(),
                expires_at: None,
            }))
    }
}
