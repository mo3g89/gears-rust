//! The one step every reader of branch content goes through to reach a
//! branch's snapshot: [`ensure_branch_snapshot`].
//!
//! ## What a read of `(repo, branch)` does
//!
//! 1. The repository has synced successfully (`last_synced_at` set,
//!    `sync_error` clear) and the branch's snapshot exists on disk: the read
//!    is served from it. No sync, no network.
//! 2. Otherwise the branch is synced first, through [`BranchSync`]:
//!    - the branch must exist on the remote, and it is always confirmed there
//!      first: the branch list is refreshed (ls-refs, no content fetch) and
//!      re-read. Neither the cached list nor a snapshot on disk is enough —
//!      the cache lags the remote both ways (a branch deleted there stays
//!      cached until the next refresh), and a snapshot outlives the branch it
//!      was taken from. The listing costs a round trip only on this path,
//!      i.e. for a branch with no snapshot or while the repository is already
//!      in the failed state (`sync_error` set). A branch absent from the
//!      remote is [`DomainError::BranchNotFound`], and the sync engine is
//!      never called for it: a failed sync records `sync_error`, which every
//!      branch of the repository reads as "not synced"
//!      ([`super::plans::require_synced`]), so syncing a mistyped or deleted
//!      branch would break every other branch until the next good sync;
//!    - a listing that fails is answered by class (DESIGN §3.3 "Branch model
//!      and the first read of a branch"): a configuration fault — the
//!      credential cannot be resolved, or the remote refuses it — is recorded
//!      in `sync_error` like an explicit sync's failure, so step 3 answers it
//!      as [`DomainError::RepoNotSynced`] with that reason; anything else is
//!      [`DomainError::SyncFailed`] (503) and records nothing. Both back the
//!      repository off for `remote_failure_backoff_seconds`: further reads
//!      answer the same — the recorded reason, or 503 — without contacting
//!      the remote, until the window ends, a listing or sync succeeds, an
//!      explicit sync is forced, or the repository's `url` or
//!      `credential_ref` changes. A credential backoff answers only while
//!      `sync_error` still holds the reason it recorded, and the branch
//!      refresher never starts one;
//!    - an existing branch is synced without `force`, so the freshness cache
//!      and the repo-then-branch sync locks apply: concurrent first reads of
//!      one branch fetch its content once. The ls-refs check runs before
//!      those locks, so concurrent first readers each list the remote; that
//!      is a ref listing, not a content fetch, and holding a lock across it
//!      would not collapse it (the lock must be released before `sync_repo`
//!      takes the same locks, and the branch is not fresh until that sync
//!      ends). While `sync_error` is set, the freshness window does not
//!      short-circuit the sync: a branch synced within it is fetched again,
//!      because only a successful sync clears the error.
//! 3. The snapshot is resolved again. Still unavailable is
//!    [`DomainError::RepoNotSynced`], carrying the repository's recorded sync
//!    failure (repository-wide, so possibly another branch's).
//!
//! ## Authorization
//!
//! The sync runs under the reader's own context, so it needs the reader to
//! hold `SYNC` on the repository, exactly as an explicit sync does. A reader
//! without it gets the refusal the sync gives ([`DomainError::Forbidden`]);
//! nothing is elevated. A reader needs no `SYNC` only when step 1 serves the
//! read: the branch has a snapshot and the repository's `sync_error` is
//! clear. While `sync_error` is set (any branch's last sync failed), every
//! read takes step 2 and needs `SYNC`.
//!
//! ## Who does not use it
//!
//! The analytics universe walk (`PlansService::list_universe`) reads every
//! repository of a product in one call and keeps skipping a repository with
//! no snapshot for the selected branch instead of syncing it: one call there
//! would otherwise fetch every repository the product has, and one
//! unreachable remote must not blank the overview for the others.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use qa_catalog_sdk::TestRepository;
use toolkit_security::SecurityContext;

use super::plans::{content_root_dir, require_synced};
use crate::domain::error::DomainError;

/// Syncs a branch that a read found without a snapshot.
///
/// Implemented by `ReposService`, which owns the sync engine, the branch
/// cache and the sync locks. The readers (`PlansService`, `BundlesService`)
/// hold it as a trait object, so they share the one sync path without
/// becoming generic over the SSH-key repository `ReposService` also needs.
#[async_trait]
pub trait BranchSync: Send + Sync {
    /// Sync `branch` of `repo` — the row the reader resolved under its own
    /// `TEST_REPO/GET` scope — for a read, under `ctx`.
    ///
    /// The branch is always confirmed against the remote (ls-refs) before
    /// the sync; the cached branch list alone never decides.
    ///
    /// [`DomainError::BranchNotFound`] when the remote has no such branch
    /// (the sync engine is not called); otherwise the repository row after a
    /// non-forced sync, with any engine failure recorded in its `sync_error`.
    /// A configuration fault found while listing is recorded the same way;
    /// any other listing failure is [`DomainError::SyncFailed`].
    async fn sync_branch_for_read(
        &self,
        ctx: &SecurityContext,
        repo: &TestRepository,
        branch: &str,
    ) -> Result<TestRepository, DomainError>;
}

/// Resolve the content root of `(repo, branch)`, syncing the branch first
/// when it has no snapshot — see this module's header for the full rule.
///
/// Returns the repository row the content root was resolved against: after a
/// sync that is the row the sync left, carrying its new `head_commit`.
pub(super) async fn ensure_branch_snapshot(
    branch_sync: &dyn BranchSync,
    repos_dir: &Path,
    ctx: &SecurityContext,
    repo: TestRepository,
    branch: &str,
) -> Result<(TestRepository, PathBuf), DomainError> {
    if require_synced(&repo, branch).is_ok() {
        match content_root_dir(repos_dir, &repo, branch) {
            Ok(root) => return Ok((repo, root)),
            // No snapshot for this branch yet: sync it below.
            Err(DomainError::RepoNotSynced { .. }) => {}
            // A snapshot that exists but cannot be resolved (EACCES, a
            // content root escaping the snapshot) is a fault a sync would
            // not fix.
            Err(error) => return Err(error),
        }
    }

    let repo = branch_sync.sync_branch_for_read(ctx, &repo, branch).await?;

    // `RepoNotSynced` from here on carries the recorded `sync_error`, so a
    // failed sync tells the caller why.
    require_synced(&repo, branch)?;
    let root = content_root_dir(repos_dir, &repo, branch)?;
    Ok((repo, root))
}
