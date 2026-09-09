//! Collected test-case counts — the collect job's report, and the exact
//! "expected cases" number analytics prefers over a static estimate.

use async_trait::async_trait;
use qa_insights_sdk::CollectCount;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Persistence for `qa_test_case_collect`.
#[async_trait]
pub trait CollectRepository: Send + Sync {
    /// Record the exact case count for one `(repo_id, branch, test_file)`.
    ///
    /// An upsert on the natural key, which is `idx_qa_test_case_collect_target`
    /// — legacy's composite primary key `(repo_id, branch, test_file)`
    /// (`001_initial.sql:278`) demoted to a unique index by the platform's
    /// mandatory `id UUID`. Legacy's statement is `INSERT ... ON CONFLICT
    /// (repo_id, branch, test_file) DO UPDATE SET case_count = EXCLUDED.case_count,
    /// collected_at = EXCLUDED.collected_at`
    /// (`manager/src/routes/analytics.rs:2623-2628`), and this must be the same:
    /// **an implementation that inserts with a fresh `id` instead of targeting
    /// that index duplicates the count rather than replacing it**, and every
    /// "expected cases" number the analytics surface shows is then inflated,
    /// with nothing failing.
    ///
    /// `count.collected_at` is the caller's, not `now()`: it is when the collect
    /// job produced the count, which is distinct from `updated_at`.
    ///
    /// Takes the contract type whole rather than five loose parameters, because
    /// three of the five — `repo_id`, `branch`, `test_file` — are the natural
    /// key, and passing those separately invites a call site that transposes
    /// `branch` and `test_file`: both `&str`, both accepted by the compiler in
    /// either order.
    async fn upsert_count<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        count: CollectCount,
    ) -> Result<(), DomainError>;

    /// Every collected count for the given repositories on one branch.
    ///
    /// # A slice, not one repository and not the whole branch
    ///
    /// Legacy loads **every** repository's counts for a branch in one statement
    /// — `SELECT repo_id, test_file, case_count FROM test_case_collect WHERE
    /// branch = $1` (`analytics.rs:2684`) — and keys the resulting map on
    /// `(repo_id, test_file)`. Only entries whose `repo_id` belongs to a
    /// universe test are ever looked up (`:773`), so every other repository's
    /// rows are loaded and discarded.
    ///
    /// So there are two costs to trade, and the plan's `counts_for(repo_id,
    /// branch)` traded one for the other rather than avoiding both. Narrowing to
    /// a single repository does drop the wasted rows, but it makes the caller
    /// loop over the universe's distinct repositories and issue **N statements
    /// where legacy issues one** — and this trait can lean on nothing to make
    /// that cheap. (Corrected 2026-08-20 by the code review; an earlier version
    /// of this doc argued the row-set saving and did not mention the round
    /// trips.)
    ///
    /// Taking a slice gets both: `WHERE tenant_id = ? AND branch = ? AND repo_id
    /// IN (…)` is one round trip, reads exactly the rows the caller will look
    /// up, and still uses `idx_qa_test_case_collect_tenant_repo_branch`.
    /// [`CollectCount`] already carries `repo_id`, so the caller keys the result
    /// on `(repo_id, test_file)` exactly as legacy does.
    ///
    /// An empty `repo_ids` returns no rows — it is "no repositories in scope",
    /// never "every repository". A caller with an empty universe has nothing to
    /// look up.
    async fn list_counts_for<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        repo_ids: &[Uuid],
        branch: &str,
    ) -> Result<Vec<CollectCount>, DomainError>;
}
