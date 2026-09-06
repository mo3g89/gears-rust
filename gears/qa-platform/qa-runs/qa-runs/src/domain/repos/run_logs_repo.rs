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
use time::OffsetDateTime;
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
/// Unlike [`ArchivedLog`], this holds no log text at all — only a count and a
/// timestamp — so it needs no hand-written `Debug` and no ban on `PartialEq`:
/// there is nothing here an `assert_eq!` failure could print that this crate's
/// no-log-text rule cares about.
///
/// # Why both fields, and why `since_time` is the one that matters
///
/// `qa_run_logs` has one row **per run**, not per node (`lines`/`text` are
/// whole-run totals; see [`RunLogsRepository::log_resume_positions`]'s own
/// doc for how a per-node count is recovered from that). So `since_time` is
/// the row's `updated_at` — the last time *anything* was flushed for the run,
/// which is always at or after this node's own last archived line, never
/// before it. Handed to Kubernetes as `since_time`, that is the *safe*
/// direction: it can re-request a few lines already archived (a bounded
/// duplicate, at most one flush period's worth), but it cannot skip past a
/// line that was never archived, so it cannot reopen Finding #50's gap.
///
/// `lines` is exact — a real count of this node's own archived lines (see
/// same doc) — but is unsafe to hand to Kubernetes as `tail_lines` for a
/// `follow: true` resume: `tail_lines` means "the last N lines of the log as
/// it stands right now", not "skip the first N", so if the pod produced more
/// than `2 * lines` new lines during the gap since the last observer ended,
/// `tail_lines = lines` would return only recent output and silently drop the
/// middle — the loss direction this whole task must not open. That is why
/// [`LogResume::tail_lines_for`] only ever answers when `since_time` does
/// not: `lines` is this type's fallback for a hypothetical implementation
/// with counts but no timestamp, not a mechanism this one relies on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogPosition {
    pub lines: i64,
    pub since_time: Option<OffsetDateTime>,
}

/// A run's archive read-position, per execution node — where
/// [`RunExecutor::watch`](crate::domain::ports::run_executor::RunExecutor::watch)
/// should resume rather than replay from byte 0 (Finding #50).
///
/// An empty map — [`LogResume::default`] — is the correct answer for a run
/// with no archived log yet: every accessor then answers `None`/`0` for any
/// node, which is what makes a first attach read from the beginning exactly
/// as it always has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogResume(BTreeMap<String, LogPosition>);

impl LogResume {
    /// The timestamp to resume `node` from, or `None` when nothing is
    /// archived for it yet — the caller's cue to read from the beginning.
    #[must_use]
    pub fn since_time_for(&self, node: &str) -> Option<OffsetDateTime> {
        self.0.get(node).and_then(|position| position.since_time)
    }

    /// The fallback line count for `node`, used only when
    /// [`Self::since_time_for`] answers `None` for the same node *and*
    /// something has still been archived for it. See [`LogPosition`]'s doc for
    /// why an implementation that always has a timestamp once `lines > 0` —
    /// `infra::storage::run_logs_sea_repo`'s does — makes this
    /// effectively unreachable there, and why that is a feature rather than
    /// dead code: it is the difference between "no mechanism happens to fire"
    /// and "no mechanism exists" for a future `RunLogsRepository` that tracks
    /// counts without a timestamp.
    #[must_use]
    pub fn tail_lines_for(&self, node: &str) -> Option<i64> {
        self.0.get(node).and_then(|position| {
            (position.since_time.is_none() && position.lines > 0).then_some(position.lines)
        })
    }

    /// The exact count already archived for `node`, `0` if none.
    ///
    /// This is the one accessor [`MockRunExecutor`](crate::infra::executor::mock::MockRunExecutor)
    /// uses: it holds its own scripted sequence rather than a real log stream,
    /// so it can skip exactly this many already-replayed
    /// [`ExecutionEvent::Log`](crate::domain::ports::run_executor::ExecutionEvent::Log)
    /// entries per node — an exact resume with no Kubernetes-shaped
    /// approximation, which is what makes the *no-duplication* direction
    /// falsifiable for the mock without weakening the *no-loss* direction it
    /// already had.
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
    /// each node's own `"[{node}] "`, which is what
    /// `infra::storage::run_logs_sea_repo`'s implementation does, and the
    /// run's single `updated_at` stands in for every node's `since_time` —
    /// see [`LogPosition`]'s doc for why that over-approximation is the safe
    /// direction rather than a shortcut.
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
