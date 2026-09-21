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

use time::OffsetDateTime;

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

/// Where one execution node's archived log currently ends: the kubelet
/// emission instant of the most recent line archived for it.
///
/// # This replaces a count, and the count's own doc explained why a
/// # timestamp had been tried and reverted once already — Task 2 (WS5)
///
/// The type that used to live here (`lines`, `first_line`, `last_line`) was
/// itself a workaround: `infra::executor::argo::watch`'s per-node
/// suppression counter re-read a node's whole pod log from byte 0 on
/// every re-attach and suppressed the first `lines` of what came back,
/// guarded by two archived-text anchors — because a per-line emission
/// timestamp had no parser and no schema to live in yet. Both that
/// counter's own doc and this type's earlier doc named the unbounded
/// failure mode this left open: once kubelet rotates a node's log out
/// from under a long, chatty run, the first-line guard trips, "that
/// node's count is now dead for the rest of the run", and every further
/// re-attach re-archives that node's whole retained window again.
///
/// A timestamp closes exactly that hole, because it is not a claim about
/// *this process's* archive at all — it is kubelet's own claim about *when
/// it wrote the line*, handed back to Kubernetes as `LogParams::since_time`.
/// Kubernetes, not this crate, decides what counts as "at or before that
/// instant", so a rotation that has dropped old lines from the retained
/// window changes nothing here: there is no anchor to compare against and
/// nothing to detect as misaligned, because nothing is compared client-side
/// any more.
///
/// # Why the earlier timestamp attempt failed, and why this one does not
///
/// The count's own doc recorded a first, reverted attempt at exactly this:
/// using the archive row's `updated_at` — the control plane's own
/// **write-time** — as the per-node `since_time`. That compares the wrong
/// clock: a line can sit queued behind a database round trip after
/// `ExecutionStream` has already delivered it, and a flush stamping
/// `updated_at = now` for whatever *had* been archived by then would filter
/// that line out on the next `since_time`-bounded re-attach, permanently —
/// worse than the duplication bug it was meant to fix.
///
/// [`LogPosition::last_emitted_at`] is not that clock. It is kubelet's own
/// **per-line emission timestamp**
/// (`LogParams { timestamps: true, .. }`, parsed by
/// [`crate::domain::repos::log_line::split_kubelet_timestamp`]), stored
/// per node the moment a line carrying it is archived. Kubernetes filters
/// the *next* read by the same clock the line was tagged with, so the two
/// sides of the comparison agree by construction — there is no
/// control-plane write latency for either side to be skewed by.
///
/// # What "inclusive at second granularity" costs, and what it does not
///
/// Kubernetes rounds `since_time` to the second before sending it
/// (`kube-core`'s own `subresource::Request::logs`), and a re-attach may
/// therefore re-emit the handful of lines sharing the last recorded
/// second — never more, and never a line that was never archived. This is
/// the same "under-suppress, never lose" direction the deleted per-node
/// counter aimed for, reached without a client-side counter that can drift out of
/// alignment with what the pod's log retention window actually holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogPosition {
    pub last_emitted_at: OffsetDateTime,
}

/// Map one character the way a line is flattened before it is archived:
/// `'\n'` and `'\r'` become a plain space, everything else is unchanged.
///
/// The one definition of "how a log line is flattened for storage" this
/// crate has. `domain::service::ingest::fan_out_log` maps every character of
/// `node` and `line` through this before building the `"[{node}] {line}"`
/// text [`LogArchive::record`](crate::domain::service::LogArchive::record)
/// receives.
#[must_use]
pub fn flatten_log_char(c: char) -> char {
    if c == '\n' || c == '\r' { ' ' } else { c }
}

/// A run's archive read-position, per execution node — where
/// [`RunExecutor::watch`](crate::domain::ports::run_executor::RunExecutor::watch)
/// should resume rather than replay from byte 0 (Finding #50, and Task 2
/// (WS5) which replaced the byte-0-plus-suppression mechanism with this
/// one).
///
/// An empty map — [`LogResume::default`] — is the correct answer for a run
/// with no recorded position for any node yet: [`Self::last_emitted_for`]
/// then answers `None` for any node, which is what makes a **first attach**
/// read from the beginning exactly as it always has (no `since_time` at
/// all, rather than one set to the epoch). It is also what a run **live
/// across the Task 2 deploy** gets: such a run has archived text from before
/// `qa_run_log_positions` existed but no row in it yet, so its next
/// re-attach re-reads one retained kubelet window it had already archived —
/// bounded, one-time, and in the safe (duplicate, not lost) direction. See
/// [`RunLogsRepository::log_resume_positions`]'s own doc.
#[derive(Clone, Debug, Default)]
pub struct LogResume(BTreeMap<String, LogPosition>);

impl LogResume {
    /// `node`'s most recently archived line's kubelet emission instant, or
    /// `None` if nothing is archived for it yet — the value a resuming
    /// caller hands to `LogParams::since_time`. `None` means "read from the
    /// beginning", the same as it always has.
    #[must_use]
    pub fn last_emitted_for(&self, node: &str) -> Option<OffsetDateTime> {
        self.0.get(node).map(|position| position.last_emitted_at)
    }

    /// Whether this resume position carries anything for any node.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
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

    /// Record `node`'s most recent archived line's kubelet emission instant
    /// for `run_id` — an upsert keyed `(run_id, node)`, in `qa_run_log_positions`
    /// (`migrations::m20260918_000004_run_log_positions`).
    ///
    /// Called once per node, alongside `append_log`, whenever a flush's
    /// buffer carried at least one line with a known emission instant for
    /// that node — see `infra::logs::archive::RunLogArchive::write`. A line
    /// with no emission instant (a non-Argo executor, or an archived run
    /// from before Task 2 (WS5)) never reaches this method at all, which is
    /// what makes `None` the correct steady state for such a node rather
    /// than a value this method would have to invent.
    async fn upsert_log_position<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
        tenant_id: Uuid,
        node: &str,
        last_emitted_at: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Where this run's archive currently ends, per node — Finding #50's
    /// fix, and Task 2 (WS5)'s replacement of the count this method used to
    /// recover from the archived text itself. Answering an empty
    /// [`LogResume`] is correct for a run with no recorded position for any
    /// node yet, and the caller then reads from the beginning. That is what
    /// a **first attach** wants, and it is also what a run **live across
    /// the Task 2 deploy** gets: such a run has archived text from before
    /// `qa_run_log_positions` existed but no row in it yet, so this answers
    /// empty exactly as a first attach would, and the next re-attach
    /// re-reads one retained kubelet window it had already archived —
    /// bounded, one-time, and in the safe (duplicate, not lost) direction.
    /// The first post-reattach flush writes this run's position rows, and
    /// every re-attach after that resumes from them normally.
    ///
    /// # There is now a per-node column; this reads it directly
    ///
    /// Before Task 2, `qa_run_logs` stored one row per **run** with no
    /// per-node column at all, and this method recovered a per-node answer
    /// by counting how many archived lines started with each node's own
    /// `"[{node}] "` prefix — `LogResume::from_archived_text`, deleted with
    /// this change. `qa_run_log_positions` now carries one row per
    /// `(run_id, node)`, written by [`RunLogsRepository::upsert_log_position`]
    /// on the same path that archives the text, so this is a direct,
    /// unscanned read rather than a scan of the whole archived log on every
    /// re-attach.
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
    use time::macros::datetime;

    use super::*;

    /// The base case both callers rely on: an empty resume answers `None`
    /// for every node, which is what makes a first attach read from the
    /// beginning (no `since_time` at all).
    #[test]
    fn an_empty_resume_answers_none_for_every_node() {
        let resume = LogResume::default();
        assert!(resume.is_empty());
        assert_eq!(resume.last_emitted_for("a"), None);
        assert_eq!(resume.last_emitted_for("never-appeared"), None);
    }

    /// Two nodes' positions, told apart and answered independently — the
    /// property both `run_logs_sea_repo` and `MockRunsRepository` rely on
    /// this type for.
    #[test]
    fn two_nodes_positions_are_independent() {
        let a_at = datetime!(2026-09-18 10:00:00 UTC);
        let b_at = datetime!(2026-09-18 09:00:00 UTC);
        let resume: LogResume = [
            ("a".to_owned(), LogPosition { last_emitted_at: a_at }),
            ("b".to_owned(), LogPosition { last_emitted_at: b_at }),
        ]
        .into_iter()
        .collect();

        assert_eq!(resume.last_emitted_for("a"), Some(a_at));
        assert_eq!(resume.last_emitted_for("b"), Some(b_at));
        assert_eq!(
            resume.last_emitted_for("never-appeared"),
            None,
            "a node this resume never mentions answers None, not a missing-key panic",
        );
        assert!(!resume.is_empty());
    }
}
