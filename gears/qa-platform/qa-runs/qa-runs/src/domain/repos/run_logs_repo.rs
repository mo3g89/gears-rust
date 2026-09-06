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
/// # This holds two lines of log text, and that is why `Debug` is
/// hand-written and `PartialEq` is gone — fix-round 2
///
/// `first_line`/`last_line` exist to guard the count (see "Two anchors"
/// below), and a guard against log text is still tenant log text: this type
/// crosses exactly the boundary [`ArchivedLog`]'s own doc warns about — it is
/// `pub`, re-exported from `domain::repos`, and reaches
/// [`RunExecutor::watch`](crate::domain::ports::run_executor::RunExecutor::watch),
/// whose implementors are exactly the kind of function a future
/// `#[tracing::instrument]` gets added to. So `Debug` is hand-written below
/// to report only `lines` and whether each anchor is present, never their
/// text, mirroring `ArchivedLog`'s own impl — and `PartialEq`/`Eq` are not
/// derived, for the same reason `ArchivedLog`'s doc gives for dropping them:
/// a dead derive on a type holding tenant text is not inert, it is the next
/// `assert_eq!` waiting to be written. [`LogResume::is_empty`] is what this
/// module's own tests use instead of comparing two whole `LogResume`s.
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
/// rather than ask the log source to filter by any timestamp. Left
/// unguarded, that can only ever *under*-count relative to what was truly
/// archived — which re-duplicates a little, the direction this crate has
/// always tolerated. **It is not true, and an earlier revision of this
/// paragraph wrongly said it was, that a count can never over-count relative
/// to what actually reached the pod's log — see "Two anchors" below for the
/// two ways it can and the guard against each.**
///
/// The cost of the read itself is honest and unhidden: a re-attach re-reads
/// a pod's log from byte 0 over the network on every re-attach, paying that
/// bandwidth again for a long run. **This is not a regression** — the
/// pre-Task-13 code paid exactly the same read for exactly the same reason;
/// Finding #50 was about the `CONCAT` duplicating what came back, never
/// about the read itself. A real per-line emission timestamp —
/// `LogParams::timestamps: true`, parsing and stripping the RFC3339 prefix
/// Kubernetes prepends, and a schema change to store one per node — would
/// let a resumed read start late instead of at byte 0. That is a real
/// optimisation and it is not this task's: it needs a parser and a migration
/// neither of which exists yet, and is recorded here so the next person who
/// wants to speed up a re-attach finds this paragraph before re-deriving the
/// clock-mismatch trap the timestamp version fell into.
///
/// # Two anchors — fix-round 2
///
/// A bare count trusts that a fresh, byte-0 re-read's first `lines` entries
/// are exactly the ones the archive already has. That trust can fail in
/// both directions, and each failure needs its own guard because they look
/// nothing alike:
///
/// **The window can move forward — `first_line` guards this.** A pod's
/// container log is not retained forever: kubelet rotates it out under
/// `containerLogMaxSize` × `containerLogMaxFiles` (10Mi × 5 by default), and
/// a chatty node on an 8-hour run — `cpt-cf-qa-nfr-run-duration`'s whole
/// reason to re-attach at all — is exactly the shape that outgrows it. Once
/// rotation has dropped `D` lines from the head, a re-attach's "byte 0" is
/// really line `D+1`, while the count still expects line `1`. Suppressing
/// blindly would drop lines `lines+1 .. lines+D`, which never reached the
/// archive at all — a real loss this task exists to prevent, not merely
/// tolerate. `first_line` is the archived text of the first line this node's
/// count was built from; a resuming caller compares it against the first
/// line its fresh re-read actually produces before trusting the count at
/// all, and suppresses nothing for that node if they disagree — the safe
/// direction (duplication) rather than the unsafe one (loss).
///
/// **The count can be inflated — `last_line` guards this, but only
/// detects, it cannot recover.** An archive still carrying duplicate lines
/// from a run that hit Finding #50 *before this fix shipped*, live across
/// the upgrade, has a per-node count larger than that node's real content —
/// see [`LogResume::lines_for`]'s own doc for exactly this case. `first_line`
/// does not catch it: the window has not moved, only the count is wrong, so
/// the first line still matches. `last_line` is the archived text of the
/// last line the count was built from; once a resuming caller has consumed
/// exactly `lines` lines it can compare the last one it actually consumed
/// against this anchor. A mismatch means the count was wrong and something
/// it just suppressed was never truly archived — detected, at the point
/// it can no longer be undone, which is why the contract is "log loudly",
/// not "recover".
///
/// Both anchors hold the archived line's own text, without the `"[{node}] "`
/// prefix. **It is not true that this needs no reconstruction on either
/// side — an earlier revision of this sentence said so, and fix-round 3
/// found the counter-example**: `domain::service::ingest::fan_out_log` maps
/// every `'\n'` and `'\r'` in a line to a plain space before archiving it
/// (so one archived entry can never accidentally split into two), which
/// means an anchor is the *flattened* text, not the raw text a pod ever
/// printed. A comparison against a freshly re-read raw line — which can
/// still carry an embedded `\r`, since `futures`' `Lines` strips only the
/// trailing terminator — must flatten that raw line the same way first, or
/// a line with an embedded `\r` never matches its own anchor. [`flatten_log_char`]
/// is the one shared definition of that flattening, used by both
/// `fan_out_log` (building what gets archived) and
/// `infra::executor::argo::watch`'s `LineSkip` (normalising what it compares
/// an anchor against) — two independent copies of this rule already drifted
/// once, which is the counter-example above.
#[derive(Clone)]
pub struct LogPosition {
    pub lines: i64,
    pub first_line: String,
    pub last_line: String,
}

/// Map one character the way a line is flattened before it is archived:
/// `'\n'` and `'\r'` become a plain space, everything else is unchanged.
///
/// The one definition of "how a log line is flattened for storage" this
/// crate has. `domain::service::ingest::fan_out_log` maps every character of
/// `node` and `line` through this before building the `"[{node}] {line}"`
/// text [`LogArchive::record`](crate::domain::service::LogArchive::record)
/// receives; `infra::executor::argo::watch`'s `LineSkip` maps every
/// character of a freshly re-read raw line through it the same way before
/// comparing against [`LogPosition`]'s `first_line`/`last_line`, which
/// `LogResume::from_archived_text` recovered from already-flattened text.
///
/// Two independent copies of this one rule already drifted once — a raw
/// `\r` survived a fresh re-read while its archived anchor had already been
/// flattened to a space, so the two could never compare equal, which
/// silently disabled the rotation guard (permanently, for that node, for
/// the rest of the run) for any node whose first archived line happened to
/// carry one. There is now exactly one definition, and both call sites are
/// it.
#[must_use]
pub fn flatten_log_char(c: char) -> char {
    if c == '\n' || c == '\r' { ' ' } else { c }
}

/// Reports `lines` and whether each anchor is present, never their text —
/// see this type's own "This holds two lines of log text" section.
impl std::fmt::Debug for LogPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogPosition")
            .field("lines", &self.lines)
            .field("has_first_line", &!self.first_line.is_empty())
            .field("has_last_line", &!self.last_line.is_empty())
            .finish()
    }
}

/// A run's archive read-position, per execution node — where
/// [`RunExecutor::watch`](crate::domain::ports::run_executor::RunExecutor::watch)
/// should resume rather than replay from byte 0 (Finding #50).
///
/// An empty map — [`LogResume::default`] — is the correct answer for a run
/// with no archived log yet: [`Self::lines_for`] then answers `0` for any
/// node, which is what makes a first attach read from the beginning exactly
/// as it always has.
///
/// No `PartialEq`/`Eq`: see [`LogPosition`]'s own doc for why. `Debug` is
/// still safe to derive here — it walks the map and calls `LogPosition`'s
/// own hand-written, redacting `Debug` for each entry.
#[derive(Clone, Debug, Default)]
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
    /// [`LogPosition`]'s doc for why a count rather than a timestamp, and for
    /// what `first_line`/`last_line` guard against.
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
                // The content after the closing bracket and its one
                // separating space — see `fan_out_log`'s construction
                // (`"[{node}] {line}"`) — is what a fresh, unprefixed
                // re-read of the real log actually produces, and so what
                // the anchors must be compared against verbatim.
                let content = rest[end + 1..].strip_prefix(' ').unwrap_or("");
                let entry = counts
                    .entry(rest[..end].to_owned())
                    .or_insert_with(|| LogPosition {
                        lines: 0,
                        first_line: String::new(),
                        last_line: String::new(),
                    });
                if entry.lines == 0 {
                    content.clone_into(&mut entry.first_line);
                }
                content.clone_into(&mut entry.last_line);
                entry.lines += 1;
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
    /// re-reads from a pod's log, from the beginning, guarded by
    /// [`Self::first_line_for`] and [`Self::last_line_for`] — see
    /// [`LogPosition`]'s "Two anchors" section.
    ///
    /// Both assume one archived line per delivered `Log` event for that
    /// node — true by construction from `fan_out_log` onward. An archive
    /// carrying pre-fix (pre-Task-13) duplicate lines from a run that hit
    /// Finding #50 *before this shipped*, still live when it does, inflates
    /// this count relative to what either resuming caller actually has left
    /// to deliver, and causes an over-skip on that run's first post-deploy
    /// re-attach — reachable exactly once per such run, bounded by that
    /// run's own pre-existing duplicate count, and **detected but not
    /// recoverable**: the Argo adapter's `LineSkip` reports it via `error!`
    /// once it has consumed as many lines as this method answered and the
    /// last one disagrees with [`Self::last_line_for`], but whatever it
    /// already suppressed by then cannot be un-suppressed. This is not
    /// modelled by any test in this module, which construct `LogResume`
    /// values directly rather than through a legacy row; it is covered by
    /// `infra::executor::argo::watch`'s own `LineSkip` tests instead.
    #[must_use]
    pub fn lines_for(&self, node: &str) -> i64 {
        self.0.get(node).map_or(0, |position| position.lines)
    }

    /// The archived text of `node`'s first line (unprefixed), or `None` if
    /// nothing is archived for it. See [`LogPosition`]'s "Two anchors"
    /// section for what a resuming caller does with it.
    #[must_use]
    pub fn first_line_for(&self, node: &str) -> Option<String> {
        self.0.get(node).map(|position| position.first_line.clone())
    }

    /// The archived text of `node`'s last line (unprefixed), or `None` if
    /// nothing is archived for it. See [`LogPosition`]'s "Two anchors"
    /// section for what a resuming caller does with it.
    #[must_use]
    pub fn last_line_for(&self, node: &str) -> Option<String> {
        self.0.get(node).map(|position| position.last_line.clone())
    }

    /// Whether this resume position carries anything for any node — the
    /// only comparison this module's own tests make against a whole
    /// `LogResume`, since there is no `PartialEq` to `assert_eq!` one
    /// against another (see [`LogPosition`]'s doc for why).
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
    ///
    /// `is_empty`, not `assert_eq!(.., LogResume::default())` — there is no
    /// `PartialEq` to compare whole `LogResume`s with; see `LogPosition`'s
    /// doc for why.
    #[test]
    fn empty_text_yields_an_empty_resume() {
        assert!(LogResume::from_archived_text("").is_empty());
        assert!(LogResume::from_archived_text("no prefix at all\nstill none\n").is_empty());
    }

    /// **The property both `run_logs_sea_repo` and `MockRunsRepository` rely
    /// on this one implementation for**: two nodes' output, interleaved in
    /// one archive exactly as two pods' drains would arrive, told apart and
    /// counted correctly, with neither contaminating the other's count —
    /// and the same for the two anchors fix-round 2 added: each node's own
    /// first and last line, unprefixed, not the other's.
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

        assert_eq!(resume.first_line_for("a").as_deref(), Some("one"));
        assert_eq!(resume.last_line_for("a").as_deref(), Some("three"));
        assert_eq!(resume.first_line_for("b").as_deref(), Some("uno"));
        assert_eq!(
            resume.last_line_for("b").as_deref(),
            Some("uno"),
            "node b has one line, so its first and last are the same one",
        );
        assert_eq!(resume.first_line_for("never-appeared"), None);
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
