//! Domain repository traits.
//!
//! Every method takes the caller-prepared `AccessScope` and a `DBRunner`
//! (`&DbConn` or a transaction runner), so multi-statement operations can run
//! inside a service-owned transaction. Implementations must never widen the
//! scope they are handed: whatever scope reaches this trait — PEP-compiled,
//! or minted by the one named seam, `domain::elevated::enumeration_scope`,
//! for a nil-tenant enumerating read — is executed as given, never assembled
//! or loosened here. `AccessScope::allow_all()` itself appears in this gear's
//! production code only at that one seam; no implementation in this module
//! constructs one, and no query here is unscoped on its own account.
//!
//! ## One scope per resource type
//!
//! A scope is compiled by the PEP for a specific resource type, and reusing
//! one compiled for runs on a query against the queue table would authorize
//! the wrong thing. Every method here takes **exactly one** scope, governing
//! the one table it queries. The two methods on [`RunsRepository`] that reach
//! `qa_run_test_results` rather than `qa_runs` name their parameter
//! `results_scope` so the difference is visible at the call site; everywhere
//! else the plain name is unambiguous.
//!
//! **Corrected 2026-08-13 by the quality review.** This paragraph previously
//! described a design that was considered and then replaced: it named
//! `queue_scope` and `runs_scope` and said one method "takes two scopes". No
//! method does, and those two names appear nowhere. Where a write depends on a
//! run being visible to the caller, the proof travels as an [`OwnedRunId`]
//! token rather than as a second scope — which is the better answer, because a
//! second scope parameter would have to be trusted to be the right one whereas
//! a token can only have come from a scoped read.
//!
//! ## The foreign keys are tenant-blind
//!
//! `qa_run_queue.run_id` and `qa_run_test_results.run_id` reference
//! `qa_runs(id)` with **no tenant component**, so an insert carrying an
//! attacker-chosen `run_id` succeeds if that run exists in *any* tenant and
//! fails with a foreign-key violation if it does not. The response is then a
//! working membership test over another tenant's run identifiers.
//! `secure_insert` cannot close it: it validates the `tenant_id` column of the
//! row being written and nothing else, and it cannot know that a *referenced*
//! parent belongs to the same tenant.
//!
//! What closes it is [`OwnedRunId`] — see that type. Every write that takes a
//! caller-supplied `run_id` takes it as an `OwnedRunId`, which can only be
//! produced by resolving the run under the caller's own scope, so the foreign
//! key never gets to answer the question.
//!
//! ## Guarded writes return `bool`, and the classification is the caller's
//!
//! Four methods here are compare-and-set: [`RunsRepository::update_state`]
//! carries `WHERE state = $from`, and `QueueRepository`'s
//! [`mark_dispatching`](QueueRepository::mark_dispatching) and
//! [`cancel_queued`](QueueRepository::cancel_queued) carry
//! `AND state = 'queued'`. Each returns `bool`: `true` when the guarded
//! `UPDATE` matched, `false` when it did not.
//!
//! **`false` is not an error, and the repository deliberately does not say why
//! it happened.** It cannot: `UPDATE … RETURNING` is not portably reachable
//! through `SeaORM`'s `UpdateMany`, which reports only a count, so
//! distinguishing "the row moved on", "the row does not exist" and "the row is
//! not yours" would mean guessing. An enum that guessed would be worse than a
//! `bool` that does not.
//!
//! So the protocol is: **the guard is the safety property; the classification
//! is only the message.** A service that wants to turn `false` into
//! [`DomainError::IllegalTransition`] or
//! [`DomainError::QueueRowNotQueued`] — both of which need a state the
//! repository never returns — must issue a second scoped read to find one, and
//! **that read is inherently stale**: the row may have moved again between the
//! failed update and the read. The state it reports is therefore advisory, for
//! a human reading an error message. Nothing may branch on it, and in
//! particular no caller may re-attempt the write because the classification
//! "looked retryable". The atomic answer was the `bool`.
//!
//! [`DomainError::IllegalTransition`]: crate::domain::error::DomainError::IllegalTransition
//! [`DomainError::QueueRowNotQueued`]: crate::domain::error::DomainError::QueueRowNotQueued

mod queue_repo;
mod run_logs_repo;
mod runs_repo;
mod schedules_repo;

/// A read that carries its own window, plus whether the window filled.
///
/// # Why the flag exists rather than a bare `Vec`
///
/// Two reads in this gear are issued by the dispatcher against **every tenant
/// at once** and take no caller-supplied `limit` to clamp:
/// [`QueueRepository::all_claims`] and
/// [`RunsRepository::list_timeout_candidates`]. Both had to be bounded, and a
/// bound on a read that returns a bare `Vec` is a *silent* ceiling: the caller
/// cannot tell a cluster with exactly `window` rows from a cluster with ten
/// times that many, so a truncated answer reads as a complete one. Every
/// consequence of truncation in this gear - an unreconciled claim, an
/// under-counted concurrency total - is invisible in exactly that way.
///
/// So the window travels with the answer.
///
/// # What this type guarantees, and what is only a convention
///
/// Measured against [`OwnedRunId`], which is the other type in this module that
/// carries a claim: that one is a **token**, and its guarantee is structural -
/// the field is private, so the only way to hold one is to have called the
/// method that resolves a run under the caller's scope.
///
/// `Windowed` is not that. Both fields are `pub`, and
/// `domain::service::admission::tests::fakes` constructs one literally. So:
///
/// * **Guaranteed**: nothing. This is a data carrier, and a caller can be handed
///   `{ rows: everything, truncated: false }` by any producer.
/// * **A property of the two production call sites, not of the type**:
///   `truncated` is *exact* rather than conservative, because
///   [`Windowed::from_overread`] is fed a `window + 1` read and both `SeaORM`
///   implementations request exactly that. A result of precisely `window` rows
///   is reported complete. An implementation that passed the bare window would
///   report every full scan as truncated, and nothing in this type would object.
///
/// **A token was considered and rejected.** Making `from_overread` the only
/// constructor - private fields, like `OwnedRunId` - would make the exactness
/// structural. It was not done because the test doubles need to produce these,
/// and a `#[cfg(test)]` escape hatch on a security-shaped token is worse than an
/// honest data carrier: it would read as a guarantee while having one hole in
/// exactly the configuration where guarantees are checked.
///
/// **Read-to-exhaustion was the other alternative** - return a cursor and make
/// the caller loop until the set is drained, which removes truncation as a
/// concept. `domain::service::dispatch`'s boot recovery does exactly that and it
/// is the right answer where full coverage is affordable. It is not the answer
/// for a five-second tick, which is why the tick windows and rotates instead;
/// `QueueRepository::all_claims` carries that argument.
///
/// This type deliberately does *not* say what a caller should do about
/// truncation. The two callers want different things, and each documents its
/// own choice.
///
/// Deliberately **not** `#[domain_model]`, unlike every other type these
/// traits return. It is a generic container with no domain identity of its
/// own - the domain model is the `T` inside it - and the attribute's registry
/// is keyed by concrete type name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Windowed<T> {
    /// The rows, at most `window` of them.
    pub rows: Vec<T>,
    /// `true` when at least one further row matched and was not returned.
    pub truncated: bool,
}

impl<T> Windowed<T> {
    /// Split a `window + 1` read into the window and the truncation flag.
    ///
    /// Callers of the repositories never build this; the implementations do,
    /// and each one requests one row more than its window precisely so this
    /// can be exact.
    #[must_use]
    pub fn from_overread(mut rows: Vec<T>, window: usize) -> Self {
        let truncated = rows.len() > window;
        rows.truncate(window);
        Self { rows, truncated }
    }

    /// A complete answer - nothing was cut. Used by the test doubles, which
    /// hold their fixtures in memory and have no window to fill.
    #[cfg(test)]
    pub(crate) fn complete(rows: Vec<T>) -> Self {
        Self {
            rows,
            truncated: false,
        }
    }
}

/// The `LIMIT` a windowed read must issue: one row past the window, so
/// [`Windowed::from_overread`] can tell "exactly `window` rows exist" from
/// "at least `window + 1` do".
pub(crate) fn overread(window: u64) -> u64 {
    window.saturating_add(1)
}

/// The same window as a `usize`, for [`Windowed::from_overread`]. Saturating
/// rather than panicking on a 32-bit target, where the constants in this module
/// are still far below `usize::MAX`.
pub(crate) fn window_size(window: u64) -> usize {
    usize::try_from(window).unwrap_or(usize::MAX)
}

pub use queue_repo::{
    ClaimAge, ClaimRow, ExpiredRow, MAX_CLAIM_SCAN, MAX_QUEUE_READ_LIMIT, NewQueueRow,
    QueueRepository, QueueRowRecord, QueuedPlatform, RowStatus,
};
pub use run_logs_repo::{ArchivedLog, LogPosition, LogResume, RunLogsRepository};
pub use runs_repo::{
    MAX_TIMEOUT_SWEEP_SCAN, MAX_WATCH_SCAN, NewRun, NewTestResult, OwnedRunId, RunResultDelta,
    RunStatePatch, RunWithResult, RunsRepository, TestResultRow, TimeoutCandidate, WatchCandidate,
};
pub use schedules_repo::{OwnedScheduleId, SchedulesRepository};

#[cfg(test)]
mod window_tests {
    use super::{MAX_CLAIM_SCAN, MAX_TIMEOUT_SWEEP_SCAN, Windowed, overread, window_size};

    /// The boundary that decides whether truncation is *exact* or merely
    /// "the answer was window-sized". Exactly `window` rows must read as
    /// complete, or every full-but-not-truncated scan raises a false alarm.
    #[test]
    fn a_result_of_exactly_the_window_is_not_truncated() {
        let windowed = Windowed::from_overread(vec![1, 2, 3], 3);
        assert_eq!(windowed.rows, vec![1, 2, 3]);
        assert!(!windowed.truncated);
    }

    /// The overread row is what makes the previous assertion possible, and it
    /// must be dropped rather than returned - a caller that got `window + 1`
    /// rows would page inconsistently.
    #[test]
    fn the_overread_row_sets_the_flag_and_is_dropped() {
        let windowed = Windowed::from_overread(vec![1, 2, 3, 4], 3);
        assert_eq!(windowed.rows, vec![1, 2, 3]);
        assert!(windowed.truncated);
    }

    #[test]
    fn a_short_result_is_returned_whole_and_complete() {
        let windowed = Windowed::from_overread(vec![1], 3);
        assert_eq!(windowed.rows, vec![1]);
        assert!(!windowed.truncated);
    }

    /// `overread` is the only thing that couples the `LIMIT` to the window; an
    /// implementation that passed the bare window would report every full scan
    /// as complete.
    #[test]
    fn the_issued_limit_is_one_row_past_the_window() {
        assert_eq!(overread(MAX_CLAIM_SCAN), MAX_CLAIM_SCAN + 1);
        assert_eq!(overread(MAX_TIMEOUT_SWEEP_SCAN), MAX_TIMEOUT_SWEEP_SCAN + 1);
        assert_eq!(overread(u64::MAX), u64::MAX, "saturating, not wrapping");
    }

    #[test]
    fn the_window_size_conversion_saturates_rather_than_panicking() {
        assert_eq!(window_size(MAX_CLAIM_SCAN), 1_000);
        assert_eq!(window_size(u64::MAX), usize::MAX);
    }
}
