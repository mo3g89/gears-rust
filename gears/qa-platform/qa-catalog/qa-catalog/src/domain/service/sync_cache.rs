//! In-memory sync bookkeeping for the multi-branch working area: a freshness
//! TTL cache and the two-tier lock registry.
//!
//! Deliberately **not** persisted, matching the source system. The launch
//! path force-syncs (evicting first), so a lost cache after a restart costs
//! at most one redundant fetch on the next browse read — never correctness.
//!
//! ## Two lock tiers
//!
//! - per-`(repo_id, branch)`: serializes syncs of the same branch
//! - per-repo: serializes *every* git mutation for a repository, because all
//!   of its branch snapshots are materialized from one shared object store
//!   and concurrent fetches race on index and ref locks
//!
//! A caller takes the repo lock for the duration of a sync; the branch lock
//! additionally collapses duplicate work on the same branch.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Key identifying one materialized branch snapshot.
type BranchKey = (Uuid, String);

/// Registry of shareable async locks keyed by `K`.
type LockMap<K> = Arc<Mutex<HashMap<K, Arc<Mutex<()>>>>>;

/// Freshness cache + lock registry for repository syncs.
#[domain_model]
pub struct SyncCache {
    ttl: Duration,
    fresh: Arc<Mutex<HashMap<BranchKey, Instant>>>,
    repo_locks: LockMap<Uuid>,
    branch_locks: LockMap<BranchKey>,
}

impl SyncCache {
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            fresh: Arc::new(Mutex::new(HashMap::new())),
            repo_locks: Arc::new(Mutex::new(HashMap::new())),
            branch_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Whether `(repo_id, branch)` was synced within the TTL. A zero TTL
    /// disables the cache entirely.
    pub async fn is_fresh(&self, repo_id: Uuid, branch: &str) -> bool {
        if self.ttl.is_zero() {
            return false;
        }
        let fresh = self.fresh.lock().await;
        fresh
            .get(&(repo_id, branch.to_owned()))
            .is_some_and(|at| at.elapsed() < self.ttl)
    }

    /// Record a successful sync of `(repo_id, branch)`.
    pub async fn mark_synced(&self, repo_id: Uuid, branch: &str) {
        let mut fresh = self.fresh.lock().await;
        fresh.insert((repo_id, branch.to_owned()), Instant::now());
    }

    /// Drop the freshness entry so the next read re-syncs. This is what
    /// "force sync" means on the launch path.
    pub async fn invalidate(&self, repo_id: Uuid, branch: &str) {
        let mut fresh = self.fresh.lock().await;
        fresh.remove(&(repo_id, branch.to_owned()));
    }

    /// Drop every freshness entry for a repository (URL or content-root
    /// change, or repository deletion).
    pub async fn invalidate_repo(&self, repo_id: Uuid) {
        let mut fresh = self.fresh.lock().await;
        fresh.retain(|(id, _), _| *id != repo_id);
    }

    /// Repository-wide git mutation lock — see the two-tier note above.
    pub async fn repo_lock(&self, repo_id: Uuid) -> Arc<Mutex<()>> {
        let mut locks = self.repo_locks.lock().await;
        Arc::clone(
            locks
                .entry(repo_id)
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    /// Per-branch lock, collapsing duplicate syncs of the same branch.
    pub async fn branch_lock(&self, repo_id: Uuid, branch: &str) -> Arc<Mutex<()>> {
        let mut locks = self.branch_locks.lock().await;
        Arc::clone(
            locks
                .entry((repo_id, branch.to_owned()))
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> SyncCache {
        SyncCache::new(Duration::from_mins(5))
    }

    #[tokio::test]
    async fn unknown_branch_is_not_fresh() {
        assert!(!cache().is_fresh(Uuid::nil(), "main").await);
    }

    #[tokio::test]
    async fn marked_branch_is_fresh() {
        let c = cache();
        c.mark_synced(Uuid::nil(), "main").await;
        assert!(c.is_fresh(Uuid::nil(), "main").await);
    }

    #[tokio::test]
    async fn freshness_is_per_branch() {
        let c = cache();
        c.mark_synced(Uuid::nil(), "main").await;
        assert!(
            !c.is_fresh(Uuid::nil(), "release/5.0").await,
            "marking one branch must not make another look fresh"
        );
    }

    #[tokio::test]
    async fn invalidate_forces_a_resync() {
        let c = cache();
        c.mark_synced(Uuid::nil(), "main").await;
        c.invalidate(Uuid::nil(), "main").await;
        assert!(!c.is_fresh(Uuid::nil(), "main").await);
    }

    #[tokio::test]
    async fn invalidate_repo_clears_every_branch() {
        let c = cache();
        let repo = Uuid::new_v4();
        let other = Uuid::new_v4();
        c.mark_synced(repo, "main").await;
        c.mark_synced(repo, "dev").await;
        c.mark_synced(other, "main").await;

        c.invalidate_repo(repo).await;

        assert!(!c.is_fresh(repo, "main").await);
        assert!(!c.is_fresh(repo, "dev").await);
        assert!(
            c.is_fresh(other, "main").await,
            "another repository's freshness must survive"
        );
    }

    #[tokio::test]
    async fn zero_ttl_disables_the_cache() {
        let c = SyncCache::new(Duration::ZERO);
        c.mark_synced(Uuid::nil(), "main").await;
        assert!(!c.is_fresh(Uuid::nil(), "main").await);
    }

    #[tokio::test]
    async fn repo_lock_is_shared_per_repo_and_distinct_across_repos() {
        let c = cache();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert!(Arc::ptr_eq(&c.repo_lock(a).await, &c.repo_lock(a).await));
        assert!(!Arc::ptr_eq(&c.repo_lock(a).await, &c.repo_lock(b).await));
    }

    #[tokio::test]
    async fn branch_lock_is_distinct_per_branch() {
        let c = cache();
        let repo = Uuid::new_v4();
        assert!(Arc::ptr_eq(
            &c.branch_lock(repo, "main").await,
            &c.branch_lock(repo, "main").await
        ));
        assert!(!Arc::ptr_eq(
            &c.branch_lock(repo, "main").await,
            &c.branch_lock(repo, "dev").await
        ));
    }
}
