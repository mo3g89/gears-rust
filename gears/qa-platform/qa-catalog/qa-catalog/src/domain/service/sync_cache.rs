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
//!
//! ## Failure backoff
//!
//! A repository whose remote could not be listed, or whose sync recorded a
//! fault in `sync_error` (its credential could not be used, it is over a size
//! budget, or its content sync timed out), is remembered for
//! `remote_failure_backoff_seconds` together with which of the two it was, so
//! a read that would sync it answers at once
//! instead of contacting the remote again (DESIGN §3.3 "Branch model and the
//! first read of a branch"). Same posture as the freshness cache: in memory,
//! per replica, lost on restart.
//!
//! A configuration backoff carries the `sync_error` text it recorded, and a read
//! answers from it only while the row still carries exactly that text — never
//! another branch's failure that landed on the same column since. Every clear
//! bumps a per-repository generation, so an attempt that started before the
//! repository changed (a new credential or url, a forced or successful sync)
//! cannot re-arm a backoff when it finishes after the change
//! ([`SyncCache::mark_backoff_if_current`]).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Key identifying one materialized branch snapshot.
type BranchKey = (Uuid, String);

/// Why a repository is backed off — the two answers a backed-off read gives.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteFault {
    /// The remote could not be listed (unreachable, timing out, failing):
    /// the read answers 503.
    Unreachable,
    /// A fault recorded in `sync_error` — the credential cannot be used
    /// (unresolvable, refused, unofferable), the repository is over a size
    /// budget, or its content sync timed out — and the read answers that
    /// recorded reason (400).
    Configuration,
}

/// One backed-off repository: when it failed, how, and — for a configuration
/// fault — the `sync_error` text that failure recorded.
struct BackoffEntry {
    at: Instant,
    fault: RemoteFault,
    recorded: Option<String>,
}

/// Backed-off repositories, and how many times each one's backoff was ended.
#[derive(Default)]
struct BackoffState {
    entries: HashMap<Uuid, BackoffEntry>,
    generations: HashMap<Uuid, u64>,
}

impl BackoffState {
    fn clear(&mut self, repo_id: Uuid) {
        self.entries.remove(&repo_id);
        *self.generations.entry(repo_id).or_default() += 1;
    }
}

type BackoffMap = Arc<Mutex<BackoffState>>;

/// Registry of shareable async locks keyed by `K`.
type LockMap<K> = Arc<Mutex<HashMap<K, Arc<Mutex<()>>>>>;

/// Freshness cache + lock registry for repository syncs.
#[domain_model]
pub struct SyncCache {
    ttl: Duration,
    fresh: Arc<Mutex<HashMap<BranchKey, Instant>>>,
    repo_locks: LockMap<Uuid>,
    branch_locks: LockMap<BranchKey>,
    failure_ttl: Duration,
    backoffs: BackoffMap,
}

impl SyncCache {
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            fresh: Arc::new(Mutex::new(HashMap::new())),
            repo_locks: Arc::new(Mutex::new(HashMap::new())),
            branch_locks: Arc::new(Mutex::new(HashMap::new())),
            failure_ttl: Duration::ZERO,
            backoffs: Arc::new(Mutex::new(BackoffState::default())),
        }
    }

    /// Back a repository off for `backoff` after its remote failed or its
    /// sync recorded a fault. `Duration::ZERO` (what [`Self::new`] starts with) disables it.
    #[must_use]
    pub fn with_failure_backoff(mut self, backoff: Duration) -> Self {
        self.failure_ttl = backoff;
        self
    }

    /// The fault `repo_id` is backed off for, if its window is still open.
    pub async fn backoff(&self, repo_id: Uuid) -> Option<RemoteFault> {
        if self.failure_ttl.is_zero() {
            return None;
        }
        let backoffs = self.backoffs.lock().await;
        backoffs
            .entries
            .get(&repo_id)
            .filter(|entry| entry.at.elapsed() < self.failure_ttl)
            .map(|entry| entry.fault)
    }

    /// The `sync_error` text an open [`RemoteFault::Configuration`] backoff
    /// of `repo_id` recorded, if it was armed with one.
    pub async fn recorded_reason(&self, repo_id: Uuid) -> Option<String> {
        if self.failure_ttl.is_zero() {
            return None;
        }
        let backoffs = self.backoffs.lock().await;
        backoffs
            .entries
            .get(&repo_id)
            .filter(|entry| entry.at.elapsed() < self.failure_ttl)
            .filter(|entry| entry.fault == RemoteFault::Configuration)
            .and_then(|entry| entry.recorded.clone())
    }

    /// Record that `repo_id` just failed with `fault`, unconditionally and
    /// with no recorded reason.
    pub async fn mark_backoff(&self, repo_id: Uuid, fault: RemoteFault) {
        if self.failure_ttl.is_zero() {
            return;
        }
        let mut backoffs = self.backoffs.lock().await;
        self.arm(&mut backoffs, repo_id, fault, None);
    }

    /// How many times `repo_id`'s backoff has been ended so far. Read it
    /// before an attempt and hand it to [`Self::mark_backoff_if_current`].
    pub async fn backoff_generation(&self, repo_id: Uuid) -> u64 {
        self.backoffs
            .lock()
            .await
            .generations
            .get(&repo_id)
            .copied()
            .unwrap_or_default()
    }

    /// Record that `repo_id` failed with `fault` — and, for a configuration
    /// fault, the `sync_error` text it recorded — unless its backoff was ended
    /// since `generation` was read: the failure then belongs to a repository
    /// that has changed since (a new credential or url, a forced or
    /// successful sync), and must not hold the new one back. Returns whether
    /// it armed.
    pub async fn mark_backoff_if_current(
        &self,
        repo_id: Uuid,
        fault: RemoteFault,
        recorded: Option<String>,
        generation: u64,
    ) -> bool {
        if self.failure_ttl.is_zero() {
            return false;
        }
        let mut backoffs = self.backoffs.lock().await;
        if backoffs
            .generations
            .get(&repo_id)
            .copied()
            .unwrap_or_default()
            != generation
        {
            return false;
        }
        self.arm(&mut backoffs, repo_id, fault, recorded);
        true
    }

    fn arm(
        &self,
        backoffs: &mut BackoffState,
        repo_id: Uuid,
        fault: RemoteFault,
        recorded: Option<String>,
    ) {
        backoffs
            .entries
            .retain(|_, entry| entry.at.elapsed() < self.failure_ttl);
        backoffs.entries.insert(
            repo_id,
            BackoffEntry {
                at: Instant::now(),
                fault,
                recorded,
            },
        );
    }

    /// End `repo_id`'s backoff: its remote just answered, an operator forced a
    /// sync, or the credential or url it failed with changed. Any attempt
    /// still in flight from before cannot re-arm it.
    pub async fn clear_backoff(&self, repo_id: Uuid) {
        self.backoffs.lock().await.clear(repo_id);
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
    /// change, or repository deletion), and end its failure backoff.
    pub async fn invalidate_repo(&self, repo_id: Uuid) {
        {
            let mut fresh = self.fresh.lock().await;
            fresh.retain(|(id, _), _| *id != repo_id);
        }
        self.backoffs.lock().await.clear(repo_id);
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
    async fn a_failed_remote_is_backed_off_per_repository_until_the_window_ends() {
        let c = SyncCache::new(Duration::ZERO).with_failure_backoff(Duration::from_millis(40));
        let down = Uuid::new_v4();
        let refused = Uuid::new_v4();
        let other = Uuid::new_v4();
        c.mark_backoff(down, RemoteFault::Unreachable).await;
        c.mark_backoff(refused, RemoteFault::Configuration).await;
        assert_eq!(c.backoff(down).await, Some(RemoteFault::Unreachable));
        assert_eq!(
            c.backoff(refused).await,
            Some(RemoteFault::Configuration),
            "the class is remembered"
        );
        assert_eq!(
            c.backoff(other).await,
            None,
            "the backoff is per repository"
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            c.backoff(down).await,
            None,
            "the backoff ends with its window"
        );
    }

    #[tokio::test]
    async fn clearing_or_invalidating_the_repository_ends_the_backoff() {
        let c = SyncCache::new(Duration::ZERO).with_failure_backoff(Duration::from_secs(30));
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        c.mark_backoff(a, RemoteFault::Configuration).await;
        c.mark_backoff(b, RemoteFault::Unreachable).await;
        c.clear_backoff(a).await;
        c.invalidate_repo(b).await;
        assert_eq!(
            c.backoff(a).await,
            None,
            "a successful listing, a forced sync or a credential change ends it"
        );
        assert_eq!(c.backoff(b).await, None, "a url change or delete ends it");
    }

    #[tokio::test]
    async fn a_credential_backoff_remembers_the_reason_it_recorded() {
        let c = SyncCache::new(Duration::ZERO).with_failure_backoff(Duration::from_secs(30));
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let generation = c.backoff_generation(a).await;
        assert!(
            c.mark_backoff_if_current(
                a,
                RemoteFault::Configuration,
                Some("refused".to_owned()),
                generation
            )
            .await
        );
        c.mark_backoff(b, RemoteFault::Configuration).await;
        assert_eq!(c.recorded_reason(a).await.as_deref(), Some("refused"));
        assert_eq!(
            c.recorded_reason(b).await,
            None,
            "a backoff armed without recording anything has no reason to answer"
        );
    }

    #[tokio::test]
    async fn an_attempt_from_before_a_clear_does_not_re_arm_the_backoff() {
        let c = SyncCache::new(Duration::ZERO).with_failure_backoff(Duration::from_secs(30));
        let a = Uuid::new_v4();
        let before = c.backoff_generation(a).await;
        c.clear_backoff(a).await;
        assert!(
            !c.mark_backoff_if_current(a, RemoteFault::Unreachable, None, before)
                .await
        );
        assert_eq!(c.backoff(a).await, None, "cleared since the attempt began");
        let before = c.backoff_generation(a).await;
        c.invalidate_repo(a).await;
        assert!(
            !c.mark_backoff_if_current(a, RemoteFault::Unreachable, None, before)
                .await
        );
        assert_eq!(
            c.backoff(a).await,
            None,
            "invalidated since the attempt began"
        );
    }

    #[tokio::test]
    async fn a_zero_backoff_disables_the_negative_cache() {
        let c = SyncCache::new(Duration::ZERO);
        c.mark_backoff(Uuid::nil(), RemoteFault::Unreachable).await;
        assert_eq!(c.backoff(Uuid::nil()).await, None);
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
