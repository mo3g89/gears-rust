use async_trait::async_trait;
use qa_catalog_sdk::{NewTestRepository, TestRepository, TestRepositoryUpdate};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// One branch-cache refresh target: a repository and its owning tenant.
///
/// The tenant id rides along ONLY for the branch-cache refresher lifecycle
/// task (`crate::gear`), which must mint a per-tenant system context for
/// each repository it refreshes so the rewritten branch rows carry the
/// correct `tenant_id`. It is deliberately not part of the SDK
/// `TestRepository` model.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshTarget {
    pub repo_id: Uuid,
    pub tenant_id: Uuid,
}

/// Repository trait for `TestRepository` persistence plus the per-repository
/// branch-name cache (`qa_repo_branches`), which has no aggregate of its own.
#[async_trait]
pub trait TestReposRepository: Send + Sync {
    /// Find a test repository by ID within the given security scope.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TestRepository>, DomainError>;

    /// The tenant that owns repository `id`, read off its row within `scope`;
    /// `Ok(None)` when no such row is visible there.
    ///
    /// The SDK `TestRepository` model carries no `tenant_id`, so this is how
    /// a request path learns the tenant the background branch refresher binds
    /// its system context to (`RefreshTarget::tenant_id`). Reading a
    /// repository's credential under that one tenant is what makes a request
    /// and the refresher read it alike, whichever tenant the request's caller
    /// belongs to (`ReposService::resolve_credential`).
    async fn owner_tenant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Uuid>, DomainError>;

    /// List all test repositories visible within the given security scope.
    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<TestRepository>, DomainError>;

    /// Create a new test repository.
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewTestRepository,
    ) -> Result<TestRepository, DomainError>;

    /// Replace the mutable fields of an existing repository. Returns
    /// `Ok(None)` when no row with `id` is visible in `scope`.
    ///
    /// `default_branch` is not among them — it is immutable after
    /// registration (see `ReposService::update_repo`).
    ///
    /// `invalidate_working_copy` additionally clears `last_synced_at` and
    /// `sync_error` in the SAME statement, leaving the row in its
    /// never-synced state. The caller sets it when the update makes the
    /// existing working copy stale (a changed `url` or `content_root`), so
    /// content reads sync from the new location (or fail closed) instead of
    /// serving content fetched from the old one. One statement, so a row can
    /// never be left advertising fresh content for a new URL.
    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        update: TestRepositoryUpdate,
        invalidate_working_copy: bool,
    ) -> Result<Option<TestRepository>, DomainError>;

    /// Delete a test repository by ID. Cascades to its cached branches.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Record the outcome of a sync attempt: `last_synced_at`, `head_commit`
    /// and `sync_error` are each written exactly as passed (so a successful
    /// sync clears the error by passing `None`), and `updated_at` is
    /// refreshed. Returns `Ok(None)` when no row with `id` is visible in
    /// `scope`.
    ///
    /// **All three are written, never merged**, which is what makes a failure
    /// path's obligation explicit: `record_sync_failure` must pass the
    /// repository's *existing* `last_synced_at` and `head_commit` back, or a
    /// failed attempt would erase the record of the last good one.
    async fn update_sync_state<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        last_synced_at: Option<OffsetDateTime>,
        head_commit: Option<String>,
        sync_error: Option<String>,
    ) -> Result<Option<TestRepository>, DomainError>;

    /// Make the cached branch set of `repo_id` equal `branches` (duplicates
    /// collapsed), as an idempotent diff (DESIGN §3.3 "Branch model and the
    /// first read of a branch"): names already cached are kept, cached names
    /// the listing lacks are deleted, new names are inserted with
    /// `ON CONFLICT DO NOTHING` on `idx_qa_branches_unique`, so two calls
    /// racing on one repository both succeed. A kept row keeps its
    /// `refreshed_at`, which is therefore when the name was first listed. Not
    /// atomic on its own: callers that need the cache never to be observed
    /// half-written pass a transaction runner.
    ///
    /// Rows are filed under the repository's **owning** tenant, read off its
    /// row through `scope` — never the caller's tenant, which for a caller
    /// whose scope spans a tenant hierarchy is not the owner (DESIGN §3.8). A
    /// row of this repository cached under any other tenant *visible in
    /// `scope`* is removed; one under a tenant outside `scope` is not seen and
    /// stays.
    /// [`DomainError::NotFound`] when the repository is not visible in `scope`.
    async fn replace_branches<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        repo_id: Uuid,
        branches: Vec<String>,
    ) -> Result<(), DomainError>;

    /// Cached branch names of `repo_id`, ascending. An empty vector means the
    /// cache has never been populated (or the repo is not visible in `scope`).
    async fn list_branches<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        repo_id: Uuid,
    ) -> Result<Vec<String>, DomainError>;

    /// Every repository visible in `scope` as a [`RefreshTarget`]
    /// (`repo_id` + owning `tenant_id`).
    ///
    /// System-task support (the branch-cache refresher): still scope-bound —
    /// this method applies exactly the `scope` it is handed and decides
    /// nothing about how far it reaches. Its actual caller,
    /// `ReposService::list_refresh_targets`, no longer compiles that scope
    /// from a PDP grant: it passes `domain::elevated::enumeration_scope`'s
    /// `AccessScope::allow_all()` directly, at one of the two call sites in
    /// this crate's production code sanctioned to do so — see that module's
    /// doc for why.
    async fn list_refresh_targets<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<RefreshTarget>, DomainError>;
}
