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
    /// content reads fail closed with `RepoNotSynced` instead of serving
    /// content fetched from the old location. One statement, so a row can
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

    /// Record the outcome of a sync attempt: both `last_synced_at` and
    /// `sync_error` are written exactly as passed (so a successful sync clears
    /// the error by passing `None`), and `updated_at` is refreshed. Returns
    /// `Ok(None)` when no row with `id` is visible in `scope`.
    async fn update_sync_state<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        last_synced_at: Option<OffsetDateTime>,
        sync_error: Option<String>,
    ) -> Result<Option<TestRepository>, DomainError>;

    /// Replace the cached branch set of `repo_id` with `branches`
    /// (delete-all-then-insert, both scoped; duplicates in the input are
    /// collapsed). Not atomic on its own — callers that need the cache to
    /// never be observed empty must pass a transaction runner.
    ///
    /// Callers must have already resolved `repo_id` through [`get`](Self::get)
    /// under the same scope: this method writes rows carrying the `tenant_id`
    /// it is handed and does not re-check that the repository belongs to it.
    async fn replace_branches<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
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
