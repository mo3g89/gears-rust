//! The archived-log repository port.
//!
//! # A separate trait, deliberately, and not a separate struct
//!
//! `OrmRunsRepository` implements this as well as [`RunsRepository`], because
//! `AppServices` is generic over its repository types and a fourth type
//! parameter would ripple through every service signature for no gain. What
//! matters is that the two *traits* are separate: `get_log` must not sit next
//! to `list`, or a future "list runs with their logs" convenience has
//! everything it needs to reproduce legacy's OOM.
//!
//! `no_list_query_reaches_the_log_table`
//! (`infra::storage::runs_sea_repo`) is the guard that keeps that true
//! regardless of intent.
//!
//! [`RunsRepository`]: crate::domain::repos::RunsRepository

use std::collections::BTreeMap;

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// One run's archived log, and enough metadata to describe it without
/// re-reading the text.
///
/// `Debug` is hand-written below and there is deliberately **no** `PartialEq`
/// or `Eq` — see that impl's own doc for both.
#[derive(Clone)]
pub struct ArchivedLog {
    pub text: String,
    pub lines: i64,
}

/// Reports `lines` and `text`'s **length**, never `text` itself.
///
/// This mirrors `infra::logs::archive::Pending`'s hand-written `Debug`, and
/// this type crosses a **wider** boundary than that one does: it is `pub`,
/// re-exported from `domain::repos`, returned from the `pub`
/// `RunsService::archived_log`, and held as a local inside
/// `api::rest::handlers::runs::stream_run_logs`, which **is**
/// `#[tracing::instrument]`ed. Adding `ret` to that attribute, or hoisting the
/// value into a parameter of a helper that later gets instrumented, would print
/// a tenant's entire log into a span — which is not hypothetical in this crate:
/// `stream_run_logs` needed `skip(svc, ctx, logs)` added to that exact
/// attribute because `RunLogBroadcaster`'s **derived** `Debug` walked its
/// `Mutex`-guarded channel map and enumerated every run streaming on the
/// replica, other tenants' included, into every child span.
///
/// # Why `PartialEq` and `Eq` are gone rather than kept
///
/// They had no user. A dead derive on a type holding bulk tenant text is not
/// inert, though: the next `assert_eq!` written against an `ArchivedLog` — or
/// against an `Option<ArchivedLog>` — renders both sides through `Debug` on
/// failure, which would have turned an unused derive into a printer. Without
/// `PartialEq` that comparison does not compile, so the two existing call
/// sites' shape is enforced rather than merely conventional:
///
/// * `assert!(x.is_none())`, not `assert_eq!(x, None)` — the former prints
///   nothing, the latter would have printed the log. Both call sites
///   (`infra::storage::run_logs_sea_repo`'s cross-tenant read test and
///   `api::rest::handlers::runs_handler_tests`'s scoped-read test) already
///   use the safe form; that was previously load-bearing and unstated.
/// * `Option::expect("…")` on the `Option<ArchivedLog>` — `expect` on an
///   `Option` prints only its own message, never the payload. (`Result::expect`
///   prints the *error*, which here is a `DomainError` and carries no log
///   text by this path's own rule.)
///
/// Assertions on the content assert on `log.text` or `log.lines` directly,
/// which is both narrower and what the tests actually mean.
impl std::fmt::Debug for ArchivedLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchivedLog")
            .field("lines", &self.lines)
            .field("text_len", &self.text.len())
            .finish()
    }
}

/// Where one execution node's contribution to a run's archived log currently
/// ends.
///
/// Unlike [`ArchivedLog`], this holds no log text at all — only a count — so
/// it needs no hand-written `Debug` and no ban on `PartialEq`: there is
/// nothing here an `assert_eq!` failure could print that this crate's
/// no-log-text rule cares about.
///
/// # Why this is a count, and not a timestamp — fix-round 1
///
/// An earlier revision of this type also carried the archive row's
/// `updated_at` as a per-node `since_time`, handed to Kubernetes as
/// `LogParams::since_time`. Review found that compares two different clocks:
/// `updated_at` is stamped by the **control plane**, at the instant a flush
/// *writes* the row (`run_logs_sea_repo.rs`'s `append_log`); Kubernetes
/// filters pod log entries by each entry's own **kubelet-recorded emission
/// time**. A line can reach the executor — `ExecutionStream` delivers it, so
/// it is "observed" — and still be sitting behind a database round trip when
/// the tick's `flush_due` stamps `updated_at = now` for whatever *had*
/// reached the archive by then. Re-attaching with `since_time` set to that
/// stamp filters out every such line, because its own kubelet timestamp is
/// **earlier** than the stamp being compared against, and there is no re-read
/// path to recover it: exactly the loss direction Finding #50's fix must not
/// open, worse than the bug it replaced (which duplicated, never lost).
///
/// A count has no clock in it. [`RunExecutor::watch`](crate::domain::ports::run_executor::RunExecutor::watch)'s
/// adapters that can resume are expected to re-read a node's log from the
/// beginning and suppress the first `lines` of what they read for that node,
/// rather than ask the log source to filter by any timestamp. That can only
/// ever *under*-count relative to what was truly archived — which
/// re-duplicates a little, the direction this crate has always tolerated —
/// never over-count relative to what actually reached the pod's log, because
/// [`RunLogsRepository::log_resume_positions`] counts only lines this same
/// text-parsing pass can already see, and `IngestService::fan_out_log`
/// (`domain::service::ingest`) is the only production caller of
/// [`LogArchive::record`](crate::domain::service::LogArchive::record),
/// archiving exactly one line per delivered
/// [`ExecutionEvent::Log`](crate::domain::ports::run_executor::ExecutionEvent::Log).
///
/// The cost is honest and unhidden: a re-attach re-reads a pod's log from
/// byte 0 over the network on every re-attach, paying that bandwidth again
/// for a long run. **This is not a regression** — the pre-Task-13 code paid
/// exactly the same read for exactly the same reason; Finding #50 was about
/// the `CONCAT` duplicating what came back, never about the read itself. A
/// real per-line emission timestamp — `LogParams::timestamps: true`, parsing
/// and stripping the RFC3339 prefix Kubernetes prepends, and a schema change
/// to store one per node — would let a resumed read start late instead of at
/// byte 0. That is a real optimisation and it is not this task's: it needs a
/// parser and a migration neither of which exists yet, and is recorded here
/// so the next person who wants to speed up a re-attach finds this paragraph
/// before re-deriving the clock-mismatch trap the timestamp version fell
/// into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogPosition {
    pub lines: i64,
}

/// A run's archive read-position, per execution node — where
/// [`RunExecutor::watch`](crate::domain::ports::run_executor::RunExecutor::watch)
/// should resume rather than replay from byte 0 (Finding #50).
///
/// An empty map — [`LogResume::default`] — is the correct answer for a run
/// with no archived log yet: [`Self::lines_for`] then answers `0` for any
/// node, which is what makes a first attach read from the beginning exactly
/// as it always has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogResume(BTreeMap<String, LogPosition>);

impl LogResume {
    /// Build a resume position from one run's whole archived text.
    ///
    /// The one implementation of the per-node line count both
    /// `infra::storage::run_logs_sea_repo` (over a real database) and
    /// `domain::service::test_support::MockRunsRepository` (over an in-memory
    /// double) need — see [`RunLogsRepository::log_resume_positions`]'s own
    /// doc for why counting `"[{node}] "`-prefixed lines is how a per-node
    /// answer is recovered from a schema with no per-node column at all, and
    /// [`LogPosition`]'s doc for why a count rather than a timestamp.
    ///
    /// A node name containing `]` would truncate early here and be
    /// mis-attributed under a shortened key — unreachable today
    /// (`ExecutionNode::name`'s one production source is
    /// `format!("repo-{repo_id}")`) and merely mis-attributed, not unsound,
    /// if it ever happened: the total line count across all nodes is
    /// unaffected, only which node a given count is filed under.
    #[must_use]
    pub fn from_archived_text(text: &str) -> Self {
        let mut counts: BTreeMap<String, LogPosition> = BTreeMap::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix('[')
                && let Some(end) = rest.find(']')
            {
                counts
                    .entry(rest[..end].to_owned())
                    .or_insert(LogPosition { lines: 0 })
                    .lines += 1;
            }
        }
        Self(counts)
    }

    /// The exact count already archived for `node`, `0` if none.
    ///
    /// Every resuming caller uses this and only this:
    /// [`MockRunExecutor`](crate::infra::executor::mock::MockRunExecutor)
    /// skips exactly this many already-replayed
    /// [`ExecutionEvent::Log`](crate::domain::ports::run_executor::ExecutionEvent::Log)
    /// entries from its own scripted sequence, and the Argo adapter
    /// (`infra::executor::argo::watch`) skips exactly this many lines it
    /// re-reads from a pod's log, from the beginning. Both assume one
    /// archived line per delivered `Log` event for that node — true by
    /// construction from `fan_out_log` onward (see [`LogPosition`]'s doc) —
    /// so an archive carrying pre-fix (pre-Task-13) duplicate lines from a
    /// run that hit Finding #50 before this shipped would inflate this count
    /// relative to what either resuming caller actually has left to deliver,
    /// and cause an over-skip. Unreachable for a run created after this fix
    /// (the invariant holds from here on) and not modelled by any test here,
    /// which construct `LogResume` values directly rather than through a
    /// legacy row.
    #[must_use]
    pub fn lines_for(&self, node: &str) -> i64 {
        self.0.get(node).map_or(0, |position| position.lines)
    }
}

impl FromIterator<(String, LogPosition)> for LogResume {
    fn from_iter<I: IntoIterator<Item = (String, LogPosition)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[async_trait]
pub trait RunLogsRepository: Send + Sync {
    /// Concatenate `text` onto `run_id`'s archived log, creating the row if it
    /// is the first append, and add `lines` to its count.
    ///
    /// **The concatenation happens in the statement.** Reading the row,
    /// appending in Rust and writing it back would re-transfer the whole log on
    /// every flush and would lose a concurrent append.
    ///
    /// There is no size cap: user decision, 2026-08-31, risk recorded in the
    /// design's §8.
    async fn append_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
        tenant_id: Uuid,
        text: &str,
        lines: i64,
    ) -> Result<(), DomainError>;

    /// Read `run_id`'s archived log, or `None` when it has none.
    ///
    /// `None` is the ordinary answer for a run that finished before this table
    /// existed, and it is what makes the handler fall back to the broadcaster's
    /// in-memory tail rather than showing an empty pane.
    async fn get_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<Option<ArchivedLog>, DomainError>;

    /// Where this run's archive currently ends, per node — Finding #50's
    /// fix. Answering an empty [`LogResume`] is correct for a run that has
    /// archived nothing yet; the caller then reads from the beginning, which
    /// is what a first attach wants.
    ///
    /// # There is no per-node column; this recovers a per-node answer anyway
    ///
    /// `qa_run_logs` stores one row per **run**: `text` is every node's
    /// output interleaved and `lines` is the run's total, exactly like
    /// [`ArchivedLog`]. There was never a reason to split it by node before
    /// this method needed to — the archive fans every line through one
    /// `IngestService::fan_out_log`, which prefixes each with `"[{node}] "`
    /// before it ever reaches [`LogArchive::record`](crate::domain::service::LogArchive::record)
    /// or the broadcaster, and states its own invariant that the prefix
    /// never contains the line terminator that would make it ambiguous
    /// (`domain::service::ingest`, `fan_out_log`'s doc). A per-node count is
    /// therefore recoverable by counting how many stored lines start with
    /// each node's own `"[{node}] "` — [`LogResume::from_archived_text`] is
    /// that one implementation, shared by every `RunLogsRepository`
    /// implementor rather than duplicated per adapter.
    ///
    /// **Cost, named rather than hidden.** This scans the whole archived
    /// text on every call, which is the same "no size cap" tradeoff this
    /// crate already accepted for the archive itself (design §8) — paid here
    /// too, on the re-attach path rather than only on a human reading the
    /// log. A future per-node column would remove it; splitting the row is
    /// its own migration and out of this task's scope.
    async fn log_resume_positions<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<LogResume, DomainError>;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The base case both callers rely on: an empty (or entirely
    /// unprefixed) archive resumes nothing, which is what makes a first
    /// attach read from the beginning.
    #[test]
    fn empty_text_yields_an_empty_resume() {
        assert_eq!(LogResume::from_archived_text(""), LogResume::default());
        assert_eq!(
            LogResume::from_archived_text("no prefix at all\nstill none\n"),
            LogResume::default()
        );
    }

    /// **The property both `run_logs_sea_repo` and `MockRunsRepository` rely
    /// on this one implementation for**: two nodes' output, interleaved in
    /// one archive exactly as two pods' drains would arrive, told apart and
    /// counted correctly, with neither contaminating the other's count.
    #[test]
    fn two_interleaved_nodes_are_counted_independently() {
        let resume = LogResume::from_archived_text("[a] one\n[b] uno\n[a] two\n[a] three\n");

        assert_eq!(resume.lines_for("a"), 3);
        assert_eq!(resume.lines_for("b"), 1);
        assert_eq!(
            resume.lines_for("never-appeared"),
            0,
            "a node this text never mentions answers 0, not a missing-key panic",
        );
    }

    /// A line missing the closing `]` (truncated mid-write, or simply
    /// malformed) is not a prefixed line at all — it contributes to no
    /// node's count rather than panicking on the missing delimiter.
    #[test]
    fn an_unterminated_prefix_counts_toward_no_node() {
        let resume = LogResume::from_archived_text("[a incomplete\n[a] one\n");

        assert_eq!(resume.lines_for("a"), 1);
    }
}
