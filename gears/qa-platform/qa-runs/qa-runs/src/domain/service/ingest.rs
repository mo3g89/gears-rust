//! Incremental result ingestion and live log fan-out
//! (`cpt-cf-qa-fr-runs-results-ingest`).
//!
//! One [`ExecutionEvent`] stream per run, consumed in order, turned into
//! per-test rows, signed counter deltas, SSE log lines, and finally one
//! terminal state transition.
//!
//! # The composition this module exists to get right
//!
//! Three of its steps are only correct *together*, and each was found by a
//! review of a neighbouring module rather than by testing this one.
//!
//! ## 1. A duplicate `Finished` is a no-op, not a `409`
//!
//! The pipeline is `derive_terminal_state -> reconcile_recorded_state ->
//! conditional update_state`, and a zero-row conditional update becomes
//! [`DomainError::IllegalTransition`]. But `reconcile_recorded_state(Failed,
//! Succeeded) == Failed` and `can_transition(Failed, Failed) == false` — every
//! terminal state refuses its own self-edge, deliberately, so that a duplicate
//! completion cannot rewrite `finished_at`. Composed naively a retried `Finished`
//! therefore answers 409, and executors retry.
//!
//! So [`IngestService::finish`] no-ops **whenever the reconciled state equals the
//! recorded one**, before it goes anywhere near `can_transition`, and returns
//! success having written nothing. `can_transition`'s own doc says a duplicate is
//! "rejected by this guard"; that is true of the guard and is not the ingest
//! contract.
//!
//! ## 2. `reconcile_recorded_state`'s `Succeeded` arm is now reachable
//!
//! That function is documented as letting later evidence downgrade a recorded
//! `Succeeded` — *"a green phase is the one verdict later evidence may
//! overturn"*, which is what legacy's `effective_base_phase` does. Until
//! 2026-08-14 `can_transition` forbade it, because *no* terminal state admitted
//! any exit, so the arm was dead code and this module refused the downgrade. The
//! two contracts genuinely conflicted, and no test of either function alone could
//! see it.
//!
//! **The user resolved it by opening `Succeeded -> {Failed, Error, Canceled,
//! TimedOut}`**, so the downgrade is now recorded rather than refused. The
//! guard in (1) narrowed from `is_terminal(recorded)` to `reconciled == recorded`
//! at the same time — the first was a superset that also swallowed the downgrade.
//! `a_recorded_success_is_downgraded_by_a_later_finished_event` and
//! `a_duplicate_success_still_no_ops_rather_than_conflicting` pin the two halves,
//! which is the pairing that has to hold: the self-edge stays refused, so the
//! equality no-op is what keeps a retry out of the guard.
//!
//! ## 3. Trimming precedes the column truncation, **by layering**
//!
//! This module's own [`normalize_status`] trims;
//! `infra::storage::mapper::normalize_test_status` truncates to the column's 16
//! characters; and the two do not commute — at 11 leading spaces the wrong order
//! turns `PASSED` into `PASSE`, a status that matches no counter and looks like a
//! status.
//!
//! `normalize_status`' doc invites this task to close that with a composed
//! `normalize_for_storage(status) -> String`. **Declined, because the tree
//! already has the stronger version.** `infra::storage::runs_sea_repo`'s
//! `upsert_test_result` applies `normalize_test_status` itself, at the moment it
//! builds the row, so the truncation is the repository's and is not reachable
//! from here at all. This module trims and hands the repository a trimmed
//! `String`; the repository truncates whatever it is given. The ordering is
//! therefore a property of the layer boundary rather than of a function somebody
//! must remember to call — and a composed helper in the domain would have had to
//! import a storage constant, which is the direction that boundary exists to
//! forbid. `the_stored_status_is_trimmed_before_the_column_truncates_it` is the
//! seam test `normalize_status`' doc asks for.
//!
//! # What is deliberately *not* here
//!
//! * **`ExecutionEvent::Started` writes nothing.** `service::dispatch` already
//!   moved the run to `Running` the moment the executor accepted it, which is
//!   strictly earlier than this stream delivers the same transition. Acting on
//!   this event too would attempt an illegal `Running -> Running` transition.
//! * **A late result is dropped, not counted.** See
//!   [`IngestService::ingest_result`].
//! * **The columns can be transiently wrong while a run is live**, and the
//!   completion repairs them — see [`reconcile_counts`]. The repair converges
//!   under the escalation, which was measured rather than argued; the guarantee
//!   and its two residuals are stated on `RunsRepository::get_result`, where a
//!   reader of the columns will meet them.
//! * **Two producers on one run no longer corrupt what is stored.** Since Task
//!   16c a second producer is a deployment away, not a code change away:
//!   `service::watch`'s registry keeps one producer per run *per process*, and
//!   across processes the guarantee is the leader election's — of which the only
//!   elector shipped is `NoopLeaderElector`, under which every replica runs the
//!   dispatcher tick. So N replicas observe the same run N times.
//!
//!   Three things followed from that, and Task 16b step 1 closed all three by
//!   escalating [`IngestService::record_one_result`] and
//!   [`IngestService::finish`] to `SERIALIZABLE` with a bounded retry:
//!   duplicate per-test rows, a double-counted result, and a read skew between
//!   the completion's tally and its terminal write that left `Succeeded`
//!   standing beside a `FAILED` row. `record_one_result` carries the mechanism
//!   and the alternatives; `ingest_races_pg_tests` fails without the
//!   escalation.
//!
//!   **What that does not buy, and it is worse than one lost event.** Stated
//!   here because a reader auditing "what can still go wrong" stops at this
//!   list. The retry budget is bounded, and exhausting it returns the abort to
//!   [`IngestService::ingest`], which propagates with `?` and stops on the first
//!   failing event. So the whole observation ends: `service::watch`'s drain logs
//!   a WARN and returns, and every later event on that stream — **including
//!   `Finished`** — is never applied. That run then reaches a terminal state
//!   only through the timeout sweep, which is the path Task 16c existed to
//!   close. Under the previous un-configured transaction nothing aborted, so
//!   this failure mode is one the escalation introduced. What it replaced was
//!   silent corruption of the stored rows; that is the trade, and it is a trade
//!   rather than a free win.
//! * **`RunResult::in_progress` is still unread by the terminal derivation.**
//!   Parity, and a known gap `domain::state_machine::derive_terminal_state`
//!   records in full; ingest does not close it, and now that ingest exists the
//!   case is reachable — a `Succeeded` outcome arriving while `PENDING` rows
//!   remain is reported `Succeeded`.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_environments_sdk::QaEnvironmentsClientV1;
use qa_runs_sdk::{Run, RunKind, RunResult, RunState};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, SecurityContext};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::{LogArchive, LogFanout, SerializedDb, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::{
    ExecutionEvent, ExecutionStream, NodeOutcome, TestObservation,
};
use crate::domain::repos::{
    LogResume, NewTestResult, QueueRepository, RunResultDelta, RunStatePatch, RunsRepository,
    TestResultRow,
};
use crate::domain::state_machine::{
    ExecutorOutcome, can_transition, derive_terminal_state, is_terminal, reconcile_recorded_state,
};
use crate::domain::system_actor;

/// Trim a runner-supplied per-test status, and **do not change its case**.
///
/// # The decision, and why it is this one
///
/// The source system has two writers for this column and **they disagree**:
///
/// * the log-parse mapper folds everything to upper case — its fallback arm is
///   literally `other => return other.to_uppercase()`
///   (`../testrunner/manager/src/services/argo.rs:2932-2943`);
/// * the live progress endpoint binds the runner's string **entirely raw**
///   (`../testrunner/manager/src/routes/runs.rs:1174`, `.bind(&event.status)`).
///
/// The four categorised counters match upper-case spellings only —
/// `passed` on `PASSED`, `failed` on `FAILED`/`ERROR`, `skipped` on `SKIPPED`,
/// `in_progress` on `PENDING`/`RUNNING`
/// (`../testrunner/manager/src/routes/plans.rs:188-191`) — so a lower-case
/// `passed` arriving on the progress path lands in **none** of them, while
/// still counting toward `total`, which is `COUNT(tr.id)` over every joined row
/// regardless of status (`plans.rs:192`).
///
/// So there is no single legacy behaviour to port, and the parity mandate
/// cannot decide it. The two candidate calls split on one property:
///
/// * **Upper-casing can move a value between buckets.** A lower-case `passed`
///   goes from *no* counter to the `passed` counter. That is a real arithmetic
///   change on an input the source system genuinely receives, since the
///   progress endpoint is fed by the runner and `passed` is exactly the token
///   `status_of` maps *from*. Declined.
/// * **Trimming can only ever move a value from "no bucket" into the bucket its
///   own content names.** A string that already equals `PASSED` has no
///   surrounding whitespace, so trimming is a no-op on it; trimming therefore
///   never removes a value from a counter and never moves one counter's value
///   into another. Taken.
///
/// Whitespace is a transport artifact, not a status. The vocabulary is an
/// **open set of words** — see [`TestObservation::status`] — and
/// `"PASSED\n"` is the same word as `"PASSED"`.
///
/// This is a divergence, stated rather than hidden: the source system does not
/// trim on either path, so `"PASSED\n"` reaching it counts toward `total` and
/// nothing else. The divergence only ever repairs a count the source system
/// gets wrong; it cannot produce a count the source system would have spelled
/// differently on purpose.
///
/// # Composition obligation — do not get this backwards
///
/// `infra::storage::mapper::normalize_test_status` caps the status at the
/// column's 16 characters. **Trim first, truncate second.** The two do not
/// commute, and the interesting threshold is lower than it looks: at **11**
/// leading spaces `PASSED` truncates to `"           PASSE"` and then trims to
/// **`PASSE`** — a status that is silently *wrong* rather than obviously
/// missing, and that no counter matches. Only at 16 leading spaces does it
/// erase to the empty string. Trimming first yields `PASSED` at every length.
/// `trimming_must_precede_the_column_truncation` pins both cases.
/// (Corrected 2026-08-13 by the security review: an earlier revision offered
/// 17 spaces and erasure as the example, which is true but non-minimal and
/// understates the hazard.)
///
/// Ingest must call this once, before the per-test row is written — see
/// [`IngestService::ingest_result`]. This function does not itself enforce the
/// column's width: a status longer than 16 characters passes through
/// unchanged, and is truncated only when [`infra::storage::runs_sea_repo`]
/// builds the row. The 16-character cap is a storage constraint, not a fact
/// about the vocabulary, so it belongs there and not here.
///
/// # What this does not prove, and the refactor that would
///
/// Nothing here forces that call order: the repository takes a `String` and
/// this function cannot reach into it. This function lives beside
/// [`IngestService`] while its partner lives in the storage mapper, and the
/// paragraphs above spend a lot of prose defending an ordering that a single
/// composed `normalize_for_storage(status) -> String` would make
/// unrepresentable. That composition would still cross the domain/infra
/// boundary this crate's layering forbids — a domain function importing a
/// storage-column constant — so the ordering stays documented and tested here
/// instead. `the_stored_status_is_trimmed_before_the_column_truncates_it`
/// (`ingest_tests.rs`) is the seam test that pins it end to end.
#[must_use]
fn normalize_status(status: &str) -> String {
    status.trim().to_owned()
}

/// What a per-test status contributes to the four categorised counters.
///
/// The mapping is the migration's, which is its only definitive record
/// (`infra::storage::migrations`, the `qa_run_test_results.status` column
/// comment), ported from `../testrunner/manager/src/routes/plans.rs:188-192`:
///
/// | Counter | Statuses |
/// |---|---|
/// | `passed` | `PASSED` |
/// | `failed` | `FAILED`, `ERROR` — the fold; there is no `error` counter |
/// | `skipped` | `SKIPPED` |
/// | `in_progress` | `PENDING`, `RUNNING` |
///
/// [`Bucket::Uncategorised`] is not an error case. `XFAIL` and `XPASS` are two of
/// the eight **known** values and land here on purpose: they count toward `total`
/// and toward none of the four, so the four do not sum to the total on a run with
/// expected-failure results. The source system counts them as their own two
/// categories in analytics (`../testrunner/manager/src/routes/analytics.rs:1359-1360`)
/// and never folds them into `passed` — **do not correct that by adding them.**
/// A ninth, unknown value lands here too, which is the open-set rule working:
/// count what is recognised, store what arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bucket {
    Passed,
    Failed,
    Skipped,
    InProgress,
    /// Counted in `total` alone.
    Uncategorised,
}

/// Classify a status, **case-sensitively**.
///
/// Uppercase only, because that is what the counters this is ported from match
/// (`plans.rs:188-191`) and because `normalize_status` deliberately does not
/// recase: upper-casing would move a lower-case `passed` from no counter into
/// `passed`, a real arithmetic change on an input the source system genuinely
/// receives. So a lower-case status counts toward `total` and nothing else, here
/// as there.
fn bucket(status: &str) -> Bucket {
    match status {
        "PASSED" => Bucket::Passed,
        "FAILED" | "ERROR" => Bucket::Failed,
        "SKIPPED" => Bucket::Skipped,
        "PENDING" | "RUNNING" => Bucket::InProgress,
        _ => Bucket::Uncategorised,
    }
}

/// The signed change one upsert makes to the five counters.
///
/// **Signed, and computed from the row being replaced**, which is why this takes
/// `previous`. A test row moves `PENDING` -> `PASSED`, which is
/// `in_progress -1, passed +1, total 0` — so the deltas cannot be counts, as
/// [`RunResultDelta`] says at its declaration. `total` moves only when the row is
/// genuinely new, because `upsert_test_result` is a delete-then-insert on
/// `(test_file, test_name)` and replaces rather than adds.
fn counter_delta(previous: Option<&str>, next: &str) -> RunResultDelta {
    let mut delta = RunResultDelta {
        total: i64::from(previous.is_none()),
        ..RunResultDelta::default()
    };
    if let Some(previous) = previous {
        apply_bucket(&mut delta, bucket(previous), -1);
    }
    apply_bucket(&mut delta, bucket(next), 1);
    delta
}

fn apply_bucket(delta: &mut RunResultDelta, bucket: Bucket, sign: i64) {
    match bucket {
        Bucket::Passed => delta.passed += sign,
        Bucket::Failed => delta.failed += sign,
        Bucket::Skipped => delta.skipped += sign,
        Bucket::InProgress => delta.in_progress += sign,
        Bucket::Uncategorised => {}
    }
}

/// What is recorded on a run the completion guard refuses to call `Succeeded`.
///
/// Disclosable by construction — it is this gear's own sentence about the
/// caller's own run and names no other system's text, so it does not need
/// `DomainError::recorded_text`, which classifies *errors*.
pub const NO_RESULTS_REASON: &str = "the execution reported success but no test result was ever recorded; \
     a run with no results cannot be reported as passing";

/// A queue row released because the execution it claimed has ended.
const COMPLETION_REASON: &str = "the execution finished and its claim was released";

/// The completion guard raised by Task 9's security review, taken as
/// recommended.
///
/// The five count columns are `NOT NULL DEFAULT 0` and `derive_terminal_state`
/// reads only `failed > 0` (`skipped` stopped voting on the verdict on
/// 2026-08-28), so an all-zero row plus a `Succeeded`
/// outcome yields `Succeeded` — **a run whose result ingest was dropped, delayed,
/// or never delivered is byte-identical to one that passed cleanly.** That sits
/// against the invariant `domain::state_machine` states in its own words: *"a run
/// whose work never ran must never read as passing."*
///
/// # Why `total == 0` is a sound trigger here, and not merely a heuristic
///
/// The plan phrases the guard as "refuse `Succeeded` for `total == 0` **against a
/// non-empty plan**", which would need a catalog read at completion to re-derive
/// what dispatch already decided. It does not: **no run can reach the executor
/// with an empty file list.** `service::dispatch_spec::submit` refuses a run
/// whose groups are empty with a `Validation` error before it builds a spec, and
/// `RunSpec::nodes` refuses an empty node list at the port. So a `Finished` event
/// exists only for a run that was submitted with at least one test file, and
/// `total == 0` at that point means no result row was ever ingested.
///
/// # What would falsify it, stated rather than left to be found
///
/// A test **file** that contains no test **cases**. The file list is non-empty,
/// the runner legitimately reports nothing, and this guard calls the run
/// `Failed` where the source system would have called it `Succeeded` — legacy
/// stores no counts at all and recomputes them per query, so "no rows" and "no
/// ingest" are one fact there and it cannot distinguish the two either. That is a
/// deliberate divergence in the fail-closed direction, and the recorded reason
/// says which case an operator is looking at.
///
/// Only a `Succeeded` derivation is touched: everything else already carries a
/// verdict that does not claim the work passed.
///
/// # `RunKind::Collect` is exempt, and it is the falsifying case above taken to
/// # its limit
///
/// A collect run records no test result **by construction**: it runs
/// `pytest --collect-only`, executes nothing, and delivers its work by posting
/// per-file exact counts to qa-insights' report route
/// (`COLLECT_ONLY`/`VHP_COLLECT_URL`, the frozen runner-facing contract). So
/// `total == 0` is that kind's SUCCESS shape, not the "result ingest was
/// dropped" evidence this guard exists to catch — and no amount of the run
/// going well could ever produce a row for it. Guarding it made every collect
/// run in every deployment terminal-`Failed`: measured on 2026-09-03, run
/// `collect-d4addf78-…-76` reported all 221 files, its workflow phase was
/// `Succeeded`, and this function recorded it `failed` with
/// [`NO_RESULTS_REASON`].
///
/// **Matched on the kind, not on `counts.total == 0 && platform_id.is_none()`
/// or any other proxy** — the same choice, for the same reason,
/// `service::launch` records where it bypasses admission for this kind: a
/// correctness property held up by a coincidence two functions away
/// disappears silently the day that coincidence does.
///
/// This exempts the KIND from the guard, not from the verdict: a collect run
/// whose executor reports `Failed`, or whose node died, still reaches `Failed`
/// through [`derive_terminal_state`], which is what
/// `ingest_tests::a_failed_collect_run_is_still_failed` pins.
fn apply_completion_guard(
    derived: RunState,
    counts: RunResult,
    kind: RunKind,
) -> (RunState, Option<&'static str>) {
    if kind == RunKind::Collect {
        return (derived, None);
    }
    if derived == RunState::Succeeded && counts.total == 0 {
        return (RunState::Failed, Some(NO_RESULTS_REASON));
    }
    (derived, None)
}

/// The five counters implied by a run's stored per-test rows.
///
/// The authority [`IngestService::reconciled_counts`] uses instead of the
/// denormalized columns, and the same projection the source system computes per
/// query (`../testrunner/manager/src/routes/plans.rs:188-192`): four filtered
/// buckets over [`bucket`], and `total` as a count of every row regardless of
/// status.
fn tally(rows: &[TestResultRow]) -> RunResult {
    let mut counts = RunResult {
        total: rows.len(),
        ..RunResult::default()
    };
    for row in rows {
        match bucket(&row.status) {
            Bucket::Passed => counts.passed += 1,
            Bucket::Failed => counts.failed += 1,
            Bucket::Skipped => counts.skipped += 1,
            Bucket::InProgress => counts.in_progress += 1,
            Bucket::Uncategorised => {}
        }
    }
    counts
}

/// The signed delta that moves `stored` onto `wanted`.
///
/// A delta rather than an absolute write because `add_result_counts` is the only
/// counter mutator and is incremental by design — the same statement the
/// per-event path uses, so a correction and an ordinary increment cannot take two
/// different code paths and drift.
///
/// `i64::try_from` cannot fail for any count a `usize` column holds on the
/// platforms this runs on, and saturating is the safe direction anyway: a
/// correction that under-shoots leaves the columns closer to the rows than they
/// were, and the *verdict* never depended on them.
fn correction(stored: RunResult, wanted: RunResult) -> RunResultDelta {
    let diff = |to: usize, from: usize| {
        i64::try_from(to).unwrap_or(i64::MAX) - i64::try_from(from).unwrap_or(i64::MAX)
    };
    RunResultDelta {
        passed: diff(wanted.passed, stored.passed),
        failed: diff(wanted.failed, stored.failed),
        skipped: diff(wanted.skipped, stored.skipped),
        in_progress: diff(wanted.in_progress, stored.in_progress),
        total: diff(wanted.total, stored.total),
    }
}

/// The scopes one completion's read-tally-correct needs, derived before its
/// transaction opens.
///
/// A struct rather than positional `AccessScope`s: they are all the same type, so
/// a transposition compiles — and they are not interchangeable, because
/// [`Self::correct`] is compiled for `dispatch` and the rest for `get`. Named
/// fields are what make handing the write scope to a read, or the reverse, a
/// visible mistake instead of an argument-order one.
///
/// `Clone` because [`SerializedDb::with_retry`] may run its body more than once
/// and each attempt needs its own copy. The scopes are *not* re-derived per
/// attempt: they are policy decision point round trips, and a retry loop that
/// re-enters the PDP would put its latency inside the contention it is
/// recovering from.
#[derive(Clone)]
struct TallyScopes {
    /// The run row, read for its recorded state — see [`IngestService::finish`]
    /// on why that read is inside the transaction.
    state: AccessScope,
    counts: AccessScope,
    owned: AccessScope,
    rows: AccessScope,
    correct: AccessScope,
}

/// What one `Finished` event settled, decided **inside** the transaction that
/// recorded it.
///
/// Returned rather than acted on in place because the side effects a completion
/// owes — releasing the platform, reaping the log channel — must not happen
/// inside a database transaction: a cross-gear lease release inside one holds
/// row locks across another system's latency.
#[derive(Clone, Copy)]
enum Completion {
    /// The reconciled state equalled the recorded one, so nothing was written.
    AlreadyRecorded,
    /// The verdict was recorded, with the counts it was derived from.
    Recorded { state: RunState, counts: RunResult },
}

/// The whole terminal decision, from an outcome and the tallied evidence.
///
/// Pure, and factored out of [`IngestService::finish`]'s transaction so the three
/// rules that compose here can be read without the plumbing around them:
/// [`derive_terminal_state`] turns the outcome plus the evidence into a verdict,
/// [`apply_completion_guard`] refuses a green one with no evidence at all, and
/// [`reconcile_recorded_state`] lets a state already recorded win.
fn decide_terminal(
    recorded: RunState,
    outcome: ExecutorOutcome,
    nodes: NodeOutcome,
    counts: RunResult,
    kind: RunKind,
) -> (RunState, Option<&'static str>) {
    let derived = derive_terminal_state(outcome, counts, nodes.node_failure());
    let (derived, guard_reason) = apply_completion_guard(derived, counts, kind);
    (reconcile_recorded_state(recorded, derived), guard_reason)
}

/// The counters, **tallied from the per-test rows**, with the stored columns
/// corrected to match.
///
/// Takes the transaction runner rather than a connection, because it runs inside
/// [`IngestService::finish`]'s transaction — the tally and the terminal write
/// have to commit together or the recorded state and the counts it was derived
/// from can disagree.
///
/// # Why the verdict is not derived from the columns
///
/// The columns are moved by signed deltas whose classification is not serialized
/// (see [`IngestService::record_one_result`]), so two concurrent retries of one
/// test can drive `failed` to `0` while a `FAILED` row is still present — and a
/// `Succeeded` outcome over those columns would then produce a `Succeeded` run
/// that demonstrably failed. The rows cannot be *fabricated* that way: an upsert
/// replaces one row, and a tally over them is whatever is actually stored.
///
/// This is also *closer* to the source system, not further: it stores no counters
/// at all and recomputes exactly these five per query
/// (`../testrunner/manager/src/routes/plans.rs:188-192`). The denormalized
/// columns exist here because `cpt-cf-qa-fr-runs-results-ingest` wants them
/// updated incrementally; nothing said the *verdict* had to trust them.
///
/// # The correction is a read-modify-write, and what actually makes it safe
///
/// `stored` is read by one statement and the delta applied by another, and an
/// exact correction would need an **absolute** write, which `RunsRepository`
/// deliberately does not offer because the incremental path needs a delta.
///
/// **Measured for Task 16b step 3, which closes it.** The write half is already
/// safe on its own: `add_result_counts` builds `col = col + n` server-side, so a
/// producer's increment landing between the read and the write is preserved by
/// the correction rather than overwritten. The half that needs the escalation is
/// the pair of **reads** — [`RunsRepository::get_result`] and
/// `list_test_results`. Under a per-statement snapshot they can disagree about
/// one producer's commit, leaving the delta one short of the rows it was computed
/// against; under [`IngestService::finish`]'s `SERIALIZABLE` transaction they
/// cannot. `ingest_races_pg_tests`'
/// `the_completions_counter_repair_survives_a_producer_committing_beside_it`
/// pins it and is red without the escalation.
///
/// So the columns converge at this transaction's commit rather than being
/// best-effort, and `RunsRepository::get_result` states that where a reader of
/// them will see it. What still does not depend on it: the *verdict* is derived
/// from `tallied`, which is returned regardless.
async fn reconcile_counts<C, R>(
    runs: &R,
    tx: &C,
    run_id: Uuid,
    scopes: &TallyScopes,
) -> Result<RunResult, DomainError>
where
    C: DBRunner,
    R: RunsRepository,
{
    let stored = runs
        .get_result(tx, &scopes.counts, run_id)
        .await?
        .ok_or(DomainError::RunNotFound { id: run_id })?;
    let owned = runs.resolve_owned(tx, &scopes.owned, run_id).await?;
    let tallied = tally(&runs.list_test_results(tx, &scopes.rows, owned).await?);
    if tallied == stored {
        return Ok(tallied);
    }
    warn!(
        %run_id,
        ?stored,
        ?tallied,
        "the denormalized result counters disagree with the per-test rows; the rows \
         are authoritative and the columns are being corrected",
    );
    runs.add_result_counts(tx, &scopes.correct, run_id, correction(stored, tallied))
        .await?;
    Ok(tallied)
}

/// Constructor arguments for [`IngestService`].
///
/// A struct rather than a positional constructor, for the reason
/// [`super::admission::AdmissionDeps`] gives: `clippy::too_many_arguments` would
/// need an `#[allow]` either way, and the fields that are `Arc<_>` of a trait
/// object are mutually assignable, so a transposition between them is not always
/// a type error. Named fields make it one.
///
/// **Named, not counted.** Every `Deps` doc in this module used to state how many
/// fields it had and how many were `Arc`s, and the code-quality review found all
/// four of them wrong on arrival — the count is a claim that goes stale the next
/// time a field lands, which is exactly what happened.
pub struct IngestDeps<R, Q> {
    /// Already wrapped, so the raw provider never enters this module.
    ///
    /// This field used to be an `Arc<DbProvider>` that [`IngestService::new`]
    /// wrapped on the way in, and that left the un-escalated `transaction`
    /// reachable from right here: `deps.db.transaction(..)` compiled clean,
    /// twelve lines above [`SerializedDb`]'s claim that it is not spellable
    /// inside this module. Wrapping at the *caller* closes it, and the callers
    /// can all do it — every construction site is inside
    /// `crate::domain::service`, where [`SerializedDb::new`] is visible.
    ///
    /// Narrower than its siblings on purpose: [`SerializedDb`] is
    /// `pub(in crate::domain::service)` and this field is not widening it. The
    /// alternative — making the wrapper `pub(crate)` so a `pub` field could
    /// carry it — would hand the rest of the crate a type whose only purpose is
    /// to restrict what this module can do.
    pub(in crate::domain::service) db: SerializedDb,
    pub runs: Arc<R>,
    pub queue: Arc<Q>,
    pub environments: Arc<dyn QaEnvironmentsClientV1>,
    pub logs: Arc<dyn LogFanout>,
    /// Where every fanned-out line is also accumulated for durable storage.
    ///
    /// Fed the same prefixed string [`Self::logs`] gets — see
    /// [`IngestService::fan_out_log`]'s doc on why an unprefixed archive would
    /// be a second divergence for the UI's marker parser to handle. Flushed at
    /// [`IngestService::finish`], best-effort; a periodic tick
    /// (`domain::service::dispatch`, a later task) drains whatever `finish`
    /// never reaches, such as a cancelled run's tail — see `finish`'s own doc.
    pub archive: Arc<dyn LogArchive>,
    pub policy_enforcer: PolicyEnforcer,
}

/// Consumes one run's execution events.
pub struct IngestService<R, Q> {
    /// Wrapped, so that the un-escalated `transaction` this module must not use
    /// is not reachable from inside it — see [`SerializedDb`].
    db: SerializedDb,
    runs: Arc<R>,
    queue: Arc<Q>,
    environments: Arc<dyn QaEnvironmentsClientV1>,
    logs: Arc<dyn LogFanout>,
    /// See [`IngestDeps::archive`].
    archive: Arc<dyn LogArchive>,
    policy_enforcer: PolicyEnforcer,
}

impl<R, Q> IngestService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    pub fn new(deps: IngestDeps<R, Q>) -> Self {
        Self {
            db: deps.db,
            runs: deps.runs,
            queue: deps.queue,
            environments: deps.environments,
            logs: deps.logs,
            archive: deps.archive,
            policy_enforcer: deps.policy_enforcer,
        }
    }

    /// A fresh `qa.run` scope, per call. See `domain::service`, "One scope per
    /// resource type".
    ///
    /// `qa_run_test_results` is governed by `qa.run` as well: it is a child table
    /// with no resource type of its own, and `RunsRepository`'s `results_scope`
    /// parameter names the *table* it governs rather than a second type.
    async fn run_scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
        resource_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, &resources::RUN, action, resource_id)
            .await?)
    }

    /// A fresh `qa.queue_entry` scope, per call — never derived from
    /// [`resources::RUN`].
    async fn queue_scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
        resource_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, &resources::QUEUE_ENTRY, action, resource_id)
            .await?)
    }

    async fn read_run(&self, ctx: &SecurityContext, run_id: Uuid) -> Result<Run, DomainError> {
        let scope = self.run_scope(ctx, actions::GET, Some(run_id)).await?;
        let conn = self.db.conn()?;
        self.runs
            .get(&conn, &scope, run_id)
            .await?
            .ok_or(DomainError::RunNotFound { id: run_id })
    }

    /// Where `run_id`'s archive currently ends, per node — Task 13, review
    /// finding #50.
    ///
    /// **Flushes before it reads, and that is not incidental.** `record`
    /// buffers a line in memory; only a periodic `flush_due` tick or
    /// `Self::finish` writes it to the row this reads. Without the flush
    /// here, a re-attach whose previous observer queued lines but never got
    /// to flush them — the ordinary case, since `gear.rs` runs `flush_due`
    /// *after* the dispatcher tick that calls this — would read a position
    /// that undercounts by up to a full tick's worth of this run's own
    /// output, and the resuming executor would re-fetch and re-archive
    /// exactly that tail. Flushing first closes the window (fix-round 1;
    /// found by review, not covered by the original commit's test, which
    /// force-flushed in its own polling loop and so could not see it).
    ///
    /// A flush failure here is not fatal to the read: `flush_due` and
    /// `Self::finish` still own eventually writing this run's buffer, so a
    /// transient failure here only means the position read is stale by
    /// whatever is still pending — which re-duplicates that pending tail on
    /// this resume, the direction this crate has always tolerated, not one
    /// this method introduces.
    ///
    /// The rest is a thin delegation to `self.archive`, which is private to
    /// this struct (`IngestDeps::archive`'s own doc: `get_log` must not sit
    /// next to `list`, and the same reach argument applies to `archive`
    /// itself — nothing outside this module should be able to call
    /// `record`/`flush` directly). `domain::service::watch`'s `drain` is the
    /// one caller: it reads this before calling
    /// [`RunExecutor::watch`](crate::domain::ports::run_executor::RunExecutor::watch)
    /// so the executor can resume rather than replay.
    pub(in crate::domain::service) async fn resume_positions(
        &self,
        tenant: system_actor::TenantBound,
        run_id: Uuid,
    ) -> Result<LogResume, DomainError> {
        if let Err(error) = self.archive.flush(run_id).await {
            warn!(
                %run_id,
                %error,
                "could not flush this run's log before reading its resume position; the \
                 position may undercount a still-pending tail, which will be re-archived",
            );
        }
        self.archive.resume_positions(tenant, run_id).await
    }
}

// ---------------------------------------------------------------------------
// The stream
// ---------------------------------------------------------------------------

impl<R, Q> IngestService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// Consume one run's whole event stream.
    ///
    /// **Stops on the first failing event.** The alternative — logging and
    /// continuing — would keep ingesting results for a run whose `Finished` can
    /// no longer be recorded, and would hide a `Forbidden` behind a wall of
    /// identical log lines. The claim the run holds is not abandoned by stopping:
    /// the next tick's reconciliation observes the execution as `Gone` and
    /// releases it, and the control-plane timeout sweep reclaims the run itself.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::apply`] failed with.
    pub async fn ingest(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        stream: &mut ExecutionStream,
    ) -> Result<(), DomainError> {
        while let Some(event) = stream.recv().await {
            self.apply(ctx, run_id, event).await?;
        }
        Ok(())
    }

    /// Apply one observation.
    ///
    /// Separate from [`Self::ingest`] because a runner-facing progress endpoint
    /// would land here: the source system receives per-test results over HTTP
    /// from the runner (`../testrunner/manager/src/routes/runs.rs:1130-1187`)
    /// rather than only through a watch stream, and that path needs one event
    /// applied without a stream to drive it.
    ///
    /// **Corrected 2026-08-15 by Task 16c.** This said "the seam Task 16's
    /// runner-facing progress endpoint lands on". Task 16 shipped no such
    /// endpoint. **Corrected 2026-08-15 by Task 16b step 1**, which is the third
    /// revision of this sentence: it said shipping one was "gated ... until the
    /// completion's phantom read is closed, because a second producer is exactly
    /// what makes that read reachable". That read is closed, and a second
    /// producer no longer corrupts what is stored, so neither clause justifies
    /// anything now. `api::rest::routes` carries the reason that survives, and
    /// it is not a safety one: wiring an ingest route was never in scope, and
    /// nothing specifies how a runner would authenticate to it.
    /// Naming a delivered endpoint that does not exist is how somebody adds one.
    /// The only production caller today is `service::watch`'s drain, through
    /// [`Self::ingest`].
    ///
    /// # Errors
    ///
    /// [`DomainError::RunNotFound`] when the run is not visible under `ctx`'s own
    /// scope, [`DomainError::IllegalTransition`] when a terminal state cannot be
    /// recorded, [`DomainError::Forbidden`] from the policy decision point, and
    /// [`DomainError::Database`] from any repository call.
    pub async fn apply(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        event: ExecutionEvent,
    ) -> Result<(), DomainError> {
        match event {
            // Deliberately inert — see the module header.
            ExecutionEvent::Started => Ok(()),
            ExecutionEvent::Log { node, line } => self.fan_out_log(ctx, run_id, &node, &line).await,
            ExecutionEvent::TestResult(observation) => {
                self.ingest_result(ctx, run_id, observation).await
            }
            ExecutionEvent::Finished {
                outcome,
                nodes,
                message,
            } => self.finish(ctx, run_id, outcome, nodes, message).await,
        }
    }

    /// Prefix each line with the node that produced it, then fan it out.
    ///
    /// # This arm is gated on the same read as the other three, and it was not
    ///
    /// **Corrected 2026-08-14 by the security review, which executed both
    /// escapes.** This function used to take no `ctx` and perform no repository
    /// call, so `apply` answered `Ok` for
    /// `apply(&ctx(other_tenant), run, Log { .. })` **and delivered the line into
    /// that run's stream** — a cross-tenant write — and `Ok` for a run id that
    /// does not exist.
    ///
    /// The second half is what makes it more than a missing check: `TestResult`
    /// and `Finished` answer [`DomainError::RunNotFound`] for absent and foreign
    /// alike, so the discriminator an operation-by-operation reader would look for
    /// was present on three arms and absent on the fourth. That is this
    /// subsystem's recurring "the oracle moves" shape — the response, not the row,
    /// is where a caller learns the difference, and here one `match` arm answered
    /// differently from its neighbours.
    ///
    /// So it reads the run under a `qa.run`/`get` scope like everything else. A
    /// run in a terminal state is **not** refused: log output can legitimately
    /// arrive while a completion is being recorded, dropping it would truncate the
    /// tail of the very output an operator is watching, and a log line moves no
    /// counter and no verdict — which is the entire reason the terminal drop rule
    /// exists for [`Self::ingest_result`] and does not apply here.
    ///
    /// # The cost, stated rather than discovered
    ///
    /// This is now **one scoped repository read per log line**, and a log stream
    /// is the highest-volume thing this gear consumes. Correctness first: the
    /// alternative on the table was to authorize once per stream in
    /// [`Self::ingest`], which would leave `apply` — a public seam any future
    /// runner-facing progress endpoint would call directly — unguarded, i.e.
    /// exactly the hole being closed.
    ///
    /// **Bounding the cost is still open, and it is not Task 16's.** This said
    /// it was; Task 16 shipped without touching it and Task 16c, which built the
    /// stream driver, did not take it either — the driver is `service::watch`'s
    /// drain and the shape that would work there is a short-lived per-run
    /// authorization cache held by it, **not** a hoisted `AccessScope`, which
    /// this layer forbids. Left undone deliberately: a cache keyed on a run and
    /// a context is a security-relevant object, and adding one in the task whose
    /// job was to make the path reachable at all would be the composition
    /// mistake this subsystem keeps making.
    ///
    /// # The prefix
    ///
    /// **A divergence, and a necessary one.** The source system has exactly one
    /// log per run because it has one workflow; here a run has one execution node
    /// per repository group (parity spec §3.4 rule 5), so an unprefixed
    /// interleaving of two nodes' output is unreadable and, worse, ambiguous
    /// about which repository failed. The prefix is ASCII and bracketed so a
    /// consumer can strip it without guessing.
    ///
    /// **The archive is fed this same prefixed string**, not the runner's raw
    /// `line`. A replayed log therefore reads byte-identical to the live
    /// stream it was recorded from, and the UI's marker parser — taught to
    /// strip `[node] ` in `5538cf972` — needs no second rule for archived
    /// lines.
    ///
    /// # One ingested line is exactly one archived line (Task 6)
    ///
    /// `prefixed` never carries a `\n` or a `\r`, by construction rather than
    /// by the caller's good behaviour. That is what makes the two consumers
    /// below agree: `self.logs.publish` hands the whole string to one
    /// `broadcast` send, so a live subscriber gets it as exactly one SSE
    /// event, and `RunLogArchive::record` appends the same string plus one
    /// `\n` of its own — so the read path
    /// (`api::rest::handlers::runs::stream_run_logs`) splitting the archived
    /// text on `\n` recovers exactly the lines that were ingested, never more.
    ///
    /// Without this, a `line` carrying an embedded `\n` would silently turn
    /// one ingested line into several *replayed* ones once split, each its
    /// own SSE event — a viewer watching the run live and one opening it
    /// after it finished would see a different event count for the same
    /// output. Worse than a display difference: a later fragment of a split
    /// line carries no `[node] ` prefix
    /// (`ingest_tests::a_log_line_is_fanned_out_prefixed_with_the_node_that_produced_it`
    /// pins the prefix precisely so a consumer can tell nodes apart), and
    /// `RunLogArchive::record`'s `entry.lines += 1` counts one call as one
    /// line regardless of what it contains, so `qa_run_logs.lines` would
    /// silently disagree with `taken.text.lines().count()`.
    ///
    /// The real adapter, `infra::executor::argo::watch::Watcher::follow`,
    /// obtains `line` from `stream.lines()` (`watch.rs:400`,
    /// `futures::AsyncBufReadExt`, which never yields a trailing `\n` or `\r`
    /// — confirmed by reading `futures-util`'s own `read_until`-based
    /// implementation rather than trusting the method's name), so it could
    /// never trigger this on its own. But `apply` is `pub` and its own doc
    /// says a runner-facing HTTP progress endpoint "would land here" one day,
    /// whose `line` would be a JSON string field with no such guarantee, and
    /// `ExecutionEvent::Log { line: String }` places no constraint on it
    /// either way — `MockRunExecutor` and scripted `ExecutionEvent::Log`
    /// literals already construct one with an embedded `\n` in tests. Rather
    /// than lean on "the one caller today happens not to", this normalises
    /// unconditionally, folded into the same allocation that builds the
    /// prefix, which is already the larger cost on this path — the extra
    /// per-character check is free beside it, the broadcast send and the
    /// database write that follow.
    ///
    /// **`node` is flattened on the same terms as `line`**, and the same
    /// argument applies to it with one fact removed. A previous revision
    /// pushed `node` verbatim and justified it by naming its producer:
    /// `ExecutionNode::name`'s one production source is
    /// `format!("repo-{repo_id}")` over a UUID, which cannot contain a line
    /// terminator. That is a fact about today's producer, not an invariant —
    /// `ExecutionNode::name` is a plain `String` that is deliberately *not*
    /// normalised to a DNS-1123 label, `ExecutionEvent::Log { node: String }`
    /// constrains it no more than it constrains `line`, and a `node` carrying
    /// a `\n` splits the archived text at a point where the fragment after it
    /// has no `[` either — strictly worse than a split `line`, which at least
    /// keeps the opening bracket. So this method's invariant is now
    /// unconditional over the whole string it builds: **`prefixed` never
    /// contains a `\n` or a `\r`, by construction.** No caller has to be
    /// trusted and no producer has to be re-audited when one is added.
    async fn fan_out_log(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        node: &str,
        line: &str,
    ) -> Result<(), DomainError> {
        self.read_run(ctx, run_id).await?;
        // One line means no embedded line terminator anywhere in the string
        // this builds — `node` included, not only `line` — so both are
        // flattened here rather than assumed. See this method's doc above.
        // Built in one pass rather than `format!(..)` followed by a second
        // scan over the whole prefixed string: the allocation is sized once
        // and each half is copied exactly once.
        let flatten = |c: char| if c == '\n' || c == '\r' { ' ' } else { c };
        let mut prefixed = String::with_capacity(node.len() + line.len() + 3);
        prefixed.push('[');
        prefixed.extend(node.chars().map(flatten));
        prefixed.push_str("] ");
        prefixed.extend(line.chars().map(flatten));
        // The archive gets the **same string** the subscribers get — see this
        // method's "The prefix" doc section above.
        self.archive
            .record(ctx.subject_tenant_id(), run_id, &prefixed);
        self.logs.publish(run_id, prefixed);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Per-test results
// ---------------------------------------------------------------------------

impl<R, Q> IngestService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// Store one per-test result and move the counters by the delta it causes.
    ///
    /// # A result for a run that is already terminal is dropped
    ///
    /// `RunsRepository::add_result_counts` is **unguarded** and says so: *"it
    /// will happily bump a terminal run's counters, so a late result event
    /// arriving after completion silently changes a finished run's numbers — and
    /// `derive_terminal_state` reads `failed` (only; `skipped` stopped voting on
    /// the verdict on 2026-08-28), so a recomputation on a late `failed` delta
    /// would then disagree with the recorded verdict. Whether a late event is
    /// dropped belongs to ingest."* It is dropped, and this is the decision:
    ///
    /// * a recorded terminal state is the *verdict*, and the run's counters are
    ///   the evidence it was derived from — letting the evidence move afterwards
    ///   produces a run whose numbers no longer explain its state, which is worse
    ///   than a missing row because it is not visibly missing;
    /// * a genuinely late-but-real result is the cost, so it is logged at WARN
    ///   naming the run, the test and the recorded state.
    ///
    /// **The check runs inside the write's own transaction**, not on a separate
    /// connection before it. It used to be outside, and the code-quality review
    /// showed what that cost: the state read and the writes it authorises were
    /// two connections apart, so a completion committing in between was invisible
    /// and the result was written on the strength of a state that no longer held.
    ///
    /// How the drop actually happens is no longer what an earlier revision of
    /// this paragraph said. It claimed *"a statement issued after that commit
    /// sees the terminal state and drops"*, which was `READ COMMITTED`
    /// reasoning; [`Self::record_one_result`] now opens the transaction
    /// `SERIALIZABLE`, where the snapshot is fixed at transaction start and a
    /// concurrent commit is **not** visible to a later statement. The
    /// interleaving is refused instead: the completion's write and this
    /// transaction's read of the same row are a conflict, one of them aborts
    /// with `40001`, and the retry re-reads a state that is by then terminal and
    /// drops there.
    ///
    /// **Not established:** which of the two transactions aborts, or that the
    /// drop happens on the retry rather than on the first attempt. No test
    /// distinguishes those — `ingest_races_pg_tests` asserts the stored outcome,
    /// which is the same either way. The paragraph above is `SERIALIZABLE`
    /// semantics applied to this code, not an observation of it.
    ///
    /// The check is on the run's state read from the repository, not on a state
    /// cached from earlier in the stream, so it is not a self-evidently-true
    /// guard.
    async fn ingest_result(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        observation: TestObservation,
    ) -> Result<(), DomainError> {
        // Trimmed once, before the row is written. The repository truncates to
        // the column width on its own side — see the module header on why the
        // ordering is a layer boundary rather than a composed call.
        let status = normalize_status(&observation.status);
        if !self
            .record_one_result(ctx, run_id, &observation, status)
            .await?
        {
            warn!(
                %run_id,
                test_name = %observation.test_name,
                "a per-test result arrived after the run reached a terminal state and was \
                 dropped; recording it would move counters the recorded verdict was \
                 derived from",
            );
        }
        Ok(())
    }

    /// Classify, replace the row, and move the counters — in one
    /// `SERIALIZABLE` transaction.
    ///
    /// # What the transaction buys
    ///
    /// * **The row and the counters commit together.**
    ///   `RunsRepository::upsert_test_result`'s own doc asks for this — *"pass a
    ///   transaction runner to make the pair atomic"*. Without it, a failure
    ///   between `upsert_test_result` and `add_result_counts` left a result row
    ///   no counter reflected, a permanent skew nothing reconciles.
    /// * **Two producers cannot both act on the same stale read.** This body
    ///   reads a *set* — `list_test_results`, to find the row it is about to
    ///   replace and to decide the delta — and then writes based on what it
    ///   found. Under `READ COMMITTED` each statement re-snapshots, so two
    ///   transactions can each read `previous = None`, each insert, and each
    ///   apply `+1`: a duplicated row and a doubled counter. Under
    ///   `SERIALIZABLE` that pair is a read/write dependency cycle, so Postgres
    ///   aborts one with `40001` and [`SerializedDb::with_retry`] runs it again
    ///   against the row the winner committed.
    ///
    /// A transaction alone would not have done this. Atomicity is not
    /// serialization, and an earlier revision of this doc was wrong about the
    /// isolation it was getting — see below.
    ///
    /// # Why the isolation is set here rather than left to the server
    ///
    /// `DbProvider::transaction` sets no `TxConfig`, so it inherits the server
    /// default, which on the shipped dialect is `READ COMMITTED`.
    /// `infra::storage::isolation_pg_tests` measures exactly that against a real
    /// Postgres: two transactions reading one tuple both commit, leaving the row
    /// stored twice and its counter at two.
    ///
    /// **A retracted premise, recorded so it is not reintroduced.** This
    /// paragraph used to reason about `REPEATABLE READ` "on MySQL/InnoDB, which
    /// this gear ships (the migration's `MYSQL_UP` arm)". **The gear does not
    /// ship `MySQL`** — this crate links `toolkit-db` with
    /// `features = ["sqlite", "pg"]`. Every migration here carries three dialect
    /// blobs by convention, so the presence of a `MYSQL_UP` arm is evidence of
    /// the convention and not of a deployment. That inference is the defect; the
    /// arm's existence is not.
    ///
    /// # What is **not** proven
    ///
    /// * **Nothing here is exercised by the `SQLite` tier.** That tier admits one
    ///   writer at a time, so every test in it passes with this escalation and
    ///   without it. The falsifying tests are
    ///   `domain::service::ingest_races_pg_tests`, behind the `integration`
    ///   feature, and they were checked by removing the escalation and watching
    ///   them go red.
    /// * **The retry budget is bounded, and exhausting it drops the result.**
    ///   [`SerializedDb::with_retry`] carries the argument. Contention heavy
    ///   enough to exhaust the budget returns the abort to the caller, and
    ///   ingest's caller is a watch-stream drain with nobody to answer. What the
    ///   escalation changes is *which* failure happens — an attributable one
    ///   rather than a silently wrong row set — not that failure is impossible.
    /// * **The other writers of these rows are not serialized against this one.**
    ///   `SERIALIZABLE` is a property of a *pair* of transactions. Only this
    ///   method and [`Self::finish`] were escalated, because they are the two
    ///   that read-then-write the per-test rows. A future writer that opens a
    ///   default transaction over the same rows would not be serialized against
    ///   either, and nothing in the type system says so.
    ///
    /// # The alternatives, and what each fails to close
    ///
    /// * **A locking read on the run row**, taken by both transactions before
    ///   their reads. `RunsRepository` has no locking read — `get` is a plain
    ///   scoped select — so it is a new trait method, its `SeaORM`
    ///   implementation, and its doubles. It also has no `SQLite` spelling, so
    ///   the unit tier would exercise a different path from the shipped one.
    /// * **A unique index on `(tenant_id, run_id, test_file, test_name)`**
    ///   addresses the duplicate row directly, at the storage layer rather than
    ///   the isolation layer. **No surviving reason to omit it is recorded
    ///   here**, and an earlier revision of this bullet offered one that does not
    ///   hold: it argued that legacy's delete-then-insert means the schema must
    ///   accept a repeated tuple, but a delete followed by an unconditional
    ///   insert never leaves two rows carrying that tuple, so the pattern is
    ///   compatible with the index. The causality runs the other way — the index
    ///   is absent, which is why dedupe had to be done in code. The migration's
    ///   other reason was an `InnoDB` key-width limit, dead now that `MySQL` is
    ///   not shipped. What remains true is only that adopting it is a schema
    ///   change that contradicts `a_repeated_per_test_tuple_is_accepted`, and
    ///   that it would not on its own close the phantom read.
    /// * **A zero-delta `add_result_counts` before each read**, to take the
    ///   `qa_runs` row lock. It depends on that method being an `UPDATE`, which
    ///   is an implementation detail of another file, and issues a pointless
    ///   write per event.
    ///
    /// `a_completion_trusts_the_rows_and_not_the_skewed_columns` pins the
    /// verdict's independence from the columns.
    ///
    /// `list_test_results` is used for the classification because
    /// `RunsRepository` has no single-row read on the child table, so this
    /// materialises every result row per event. A real per-event cost on a large
    /// suite, and a repository-layer follow-up.
    async fn record_one_result(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        observation: &TestObservation,
        status: String,
    ) -> Result<bool, DomainError> {
        // Scopes are derived before the transaction opens: they are policy
        // decision point round trips, and holding a transaction across one would
        // put the PDP's latency inside a database lock. Fresh per call.
        let state_scope = self.run_scope(ctx, actions::GET, Some(run_id)).await?;
        let owned_scope = self.run_scope(ctx, actions::GET, Some(run_id)).await?;
        let read_scope = self.run_scope(ctx, actions::GET, Some(run_id)).await?;
        let write_scope = self.run_scope(ctx, actions::DISPATCH, Some(run_id)).await?;
        let counts_scope = self.run_scope(ctx, actions::DISPATCH, Some(run_id)).await?;

        let runs = Arc::clone(&self.runs);
        let tenant_id = ctx.subject_tenant_id();
        let new = NewTestResult {
            test_file: observation.test_file.clone(),
            test_name: observation.test_name.clone(),
            status,
            duration: observation.duration.clone(),
            launch_id: observation.launch_id.clone(),
            jira_key: observation.jira_key.clone(),
            // Carried, not interpreted. None of the three feeds
            // `counter_delta` — the five counters are derived from `status`
            // alone — so this widens what a row records without changing what
            // a run reports.
            //
            // The `Option`s stay `Option`s here. `nodeid`'s collapse to `''`
            // for its `NOT NULL DEFAULT ''` column happens once, in
            // `OrmRunsRepository::upsert_test_result`, so the layer that knows
            // the column's nullability is the layer that decides it; the same
            // reason `normalize_test_status` truncates there and
            // `normalize_status` trims here.
            nodeid: observation.nodeid.clone(),
            reason: observation.reason.clone(),
            ticket: observation.ticket.clone(),
        };

        // Every capture is cloned per attempt, because a contention abort
        // re-runs this body from `BEGIN` — the database has already rolled the
        // first attempt back, so a body that consumed its inputs could not be
        // re-run. `account-management`'s acquire path clones its key for the
        // same reason.
        self.db
            .with_retry(move |tx| {
                let runs = Arc::clone(&runs);
                let state_scope = state_scope.clone();
                let owned_scope = owned_scope.clone();
                let read_scope = read_scope.clone();
                let write_scope = write_scope.clone();
                let counts_scope = counts_scope.clone();
                let new = new.clone();
                Box::pin(async move {
                    // The drop guard, inside the transaction that would do the
                    // writing. A completion that committed before this statement
                    // is visible to it.
                    let state = runs
                        .get(tx, &state_scope, run_id)
                        .await?
                        .ok_or(DomainError::RunNotFound { id: run_id })?
                        .state;
                    if is_terminal(state) {
                        return Ok(false);
                    }
                    let owned = runs.resolve_owned(tx, &owned_scope, run_id).await?;
                    let previous = runs
                        .list_test_results(tx, &read_scope, owned)
                        .await?
                        .into_iter()
                        .find(|row| {
                            row.test_file == new.test_file && row.test_name == new.test_name
                        })
                        .map(|row| row.status);
                    let delta = counter_delta(previous.as_deref(), &new.status);

                    runs.upsert_test_result(tx, &write_scope, tenant_id, owned, new)
                        .await?;
                    if delta != RunResultDelta::default() {
                        runs.add_result_counts(tx, &counts_scope, run_id, delta)
                            .await?;
                    }
                    Ok(true)
                })
            })
            .await
    }
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

impl<R, Q> IngestService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// Derive the run's terminal state, record it, and hand back everything the
    /// execution was holding.
    ///
    /// # The order of the three releases
    ///
    /// The lease is released **before** the queue row is marked done, and the
    /// reason is what a crash between them costs:
    ///
    /// * row first, then a crash — the claim is gone, so `all_claims` never
    ///   returns it and **no later pass can find the lease to release**. The
    ///   platform reads busy until somebody clears it by hand, which is the
    ///   failure `service::dispatch::release_lease` calls out as the one that
    ///   wedges a platform.
    /// * lease first, then a crash — the claim survives, the next tick's
    ///   reconciliation classifies its execution as `Gone` and releases both. A
    ///   second release is a no-op, because `decide_release` removes only a hold
    ///   the run actually has.
    ///
    /// So the recoverable order is lease, then row. `a_terminal_run_no_longer_holds_its_platforms_lease`
    /// asserts the resulting invariant directly.
    ///
    /// `mark_done` and not `mark_failed`, whatever the verdict: the queue row
    /// records what happened to the *claim*, and the claim completed. `failed`
    /// there means the submit itself broke.
    ///
    /// # `message` reaches the caller verbatim, and that is an accepted asymmetry
    ///
    /// **User decision, 2026-08-15.** `ExecutionEvent::Finished { message }` is
    /// the execution plane's own text. It is written straight into
    /// `RunStatePatch::error` — hence `qa_runs.error`, which `GET /runs/{id}`
    /// serves — with **no** [`DomainError::recorded_text`] gate, because it is
    /// not a `DomainError` at all. Accepted at source-system parity
    /// (`argo.rs:2280-2283` puts the
    /// workflow's `status.message` in the same place) and because the text
    /// concerns the tenant's own run: this is infrastructure-detail disclosure to
    /// the owner, not a cross-tenant oracle.
    ///
    /// **The asymmetry is recorded rather than left to be rediscovered.**
    /// `DomainError::ExecutorFailed` is classified **non-disclosable** by
    /// `DomainError::disclosable`, on the stated grounds that it *"wraps the
    /// execution plane's vocabulary"* — so `service::dispatch::record_failed`
    /// redacts that system's text through `recorded_text` while this path passes
    /// the same system's text through untouched. Same origin, two verdicts,
    /// decided by which wrapper the text arrived in. That is this subsystem's
    /// recurring "the oracle moves" shape — the discriminator is in the payload,
    /// not in the row — and it is accepted with eyes open rather than overlooked.
    ///
    /// **Task 16c is what made this path live for the first time.** Before it
    /// nothing delivered a `Finished` event, so no `message` had ever reached
    /// the column in production.
    async fn finish(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        outcome: ExecutorOutcome,
        nodes: NodeOutcome,
        message: Option<String>,
    ) -> Result<(), DomainError> {
        let finished_at = OffsetDateTime::now_utc();

        // Scopes are derived before the transaction opens: they are policy
        // decision point round trips, and holding a transaction across one would
        // put the PDP's latency inside a database lock. Fresh per call, one per
        // repository call.
        let scopes = TallyScopes {
            state: self.run_scope(ctx, actions::GET, Some(run_id)).await?,
            counts: self.run_scope(ctx, actions::GET, Some(run_id)).await?,
            owned: self.run_scope(ctx, actions::GET, Some(run_id)).await?,
            rows: self.run_scope(ctx, actions::GET, Some(run_id)).await?,
            correct: self.run_scope(ctx, actions::DISPATCH, Some(run_id)).await?,
        };
        let write_scope = self.run_scope(ctx, actions::DISPATCH, Some(run_id)).await?;

        let runs = Arc::clone(&self.runs);
        // Cloned per attempt for the reason `record_one_result` gives: a
        // contention abort re-runs this body from `BEGIN`.
        let (run, completion) = self
            .db
            .with_retry(move |tx| {
                let runs = Arc::clone(&runs);
                let scopes = scopes.clone();
                let write_scope = write_scope.clone();
                let message = message.clone();
                Box::pin(async move {
                    // **The recorded state is read inside the transaction that
                    // acts on it.** It used to be read outside, and the
                    // consequence was a lease held for a run that had just been
                    // cancelled: an operator cancel committing between the read
                    // and `update_state` left `recorded = Running`, so the guarded
                    // update matched nothing, `finish` returned `Err` — and
                    // returned it *before* the release and the reap below, which
                    // are the two things a cancelled run most needs. Read inside,
                    // the same interleaving reconciles to `Canceled`, lands on
                    // `AlreadyRecorded`, and releases.
                    //
                    // This is the same move `record_one_result` already made for
                    // its own drop guard, for the same reason.
                    //
                    // **Still not covered, and by a narrower margin than
                    // before.** Moving this read back outside leaves the
                    // SQLite tier green, because the in-memory doubles ignore
                    // the runner they are handed and so have no isolation for
                    // inside-versus-outside to mean anything against. The
                    // falsifying input is an operator cancel committing
                    // between this read and the `update_state` below, which
                    // needs two real transactions on a real dialect. That tier
                    // now exists — `ingest_races_pg_tests` — but it drives a
                    // concurrent *result* and *completion*, not a concurrent
                    // *cancel*, so this particular read placement remains
                    // reviewed rather than tested.
                    let run = runs
                        .get(tx, &scopes.state, run_id)
                        .await?
                        .ok_or(DomainError::RunNotFound { id: run_id })?;
                    let recorded = run.state;
                    let counts = reconcile_counts(runs.as_ref(), tx, run_id, &scopes).await?;
                    let (state, guard_reason) =
                        decide_terminal(recorded, outcome, nodes, counts, run.target.kind());
                    if let Some(reason) = guard_reason {
                        warn!(
                            %run_id,
                            reason,
                            "the executor reported success for a run with no recorded \
                             results; refusing to record it as passing",
                        );
                    }
                    if state == recorded {
                        return Ok((run, Completion::AlreadyRecorded));
                    }
                    let illegal = || DomainError::IllegalTransition {
                        id: run_id,
                        state: recorded,
                        action: format!("become {}", state.as_str()),
                    };
                    if !can_transition(recorded, state) {
                        error!(
                            %run_id,
                            recorded = recorded.as_str(),
                            derived = state.as_str(),
                            "an execution finished but its run is in a state that cannot \
                             reach the derived verdict; the control-plane timeout sweep \
                             will reclaim it",
                        );
                        return Err(illegal());
                    }
                    let error = guard_reason.map(str::to_owned).or(message);
                    let moved = runs
                        .update_state(
                            tx,
                            &write_scope,
                            run_id,
                            recorded,
                            state,
                            RunStatePatch {
                                started_at: None,
                                finished_at: Some(finished_at),
                                error,
                            },
                        )
                        .await?;
                    if moved {
                        Ok((run, Completion::Recorded { state, counts }))
                    } else {
                        Err(illegal())
                    }
                })
            })
            .await?;

        // **Released and reaped on both branches, and this is not
        // belt-and-braces.** A `Finished` event is direct evidence that the
        // execution ended, which is exactly the condition under which releasing a
        // claim is correct — and the case that makes it load bearing is an
        // operator cancel: `service::runs::stop_execution` records `Canceled` on
        // the run and deliberately releases *nothing*, because at that instant the
        // execution has only been asked to stop. This is where it is observed to
        // have stopped.
        self.release(ctx, &run).await;
        self.logs.reap(run_id);

        // Best-effort, and after the commit. The recorded terminal state, the
        // lease release and the reap are what this method owes its caller;
        // the archive is beside them. A failure is logged and the text stays
        // in the buffer for the next tick — the same treatment `reap` gets,
        // and the reason `flush`'s error is not propagated with `?` here: a
        // flush failure must never fail a completion.
        //
        // **Not every terminal run reaches this line.** An operator cancel
        // (`service::runs::retire`) records `Canceled` directly and reaps its
        // own log channel without ever calling `finish` — `LogFanout::reap`'s
        // own doc names the four call sites, and adding a flush to each was
        // rejected: this port's own precondition doc records that the
        // convention "flush wherever `reap` is called" has already been got
        // wrong once. So a cancelled run's tail is left for the periodic
        // `flush_due` tick, not chased into every reap site.
        if let Err(error) = self.archive.flush(run_id).await {
            warn!(
                %run_id,
                %error,
                "archiving this run's log failed at finish; the tick will retry",
            );
        }

        Self::announce(&run, completion);
        Ok(())
    }

    /// Log the side effect a completion owes, outside its transaction.
    ///
    /// Split from [`Self::finish`] so the transaction body and what follows it
    /// read separately.
    fn announce(run: &Run, completion: Completion) {
        match completion {
            Completion::AlreadyRecorded => {
                info!(
                    run_id = %run.id,
                    state = run.state.as_str(),
                    "a duplicate or late completion event arrived for a run already in \
                     this state; nothing to write",
                );
            }
            Completion::Recorded { state, counts } => {
                info!(
                    run_id = %run.id,
                    state = state.as_str(),
                    passed = counts.passed,
                    failed = counts.failed,
                    skipped = counts.skipped,
                    total = counts.total,
                    "run finished",
                );
            }
        }
    }

    /// Hand back the platform lease and release the queue claim, in that order.
    ///
    /// Best-effort on both halves: the run is recorded terminal by the time this
    /// runs, and failing the ingest afterwards would neither un-record it nor
    /// help. A lease that could not be released is logged at ERROR because it is
    /// the failure that wedges a platform.
    async fn release(&self, ctx: &SecurityContext, run: &Run) {
        let Some(platform_id) = run.platform_id else {
            return;
        };
        if let Err(error) = self
            .environments
            .release_lease(ctx, platform_id, run.id)
            .await
        {
            error!(
                run_id = %run.id,
                %platform_id,
                %error,
                "could not release the platform lease of a finished run; the platform will \
                 read as busy until the lease is cleared",
            );
        }
        if let Err(error) = self.release_claim(ctx, run, platform_id).await {
            warn!(
                run_id = %run.id,
                %platform_id,
                %error,
                "could not release the queue claim of a finished run; the next tick's \
                 reconciliation owns it",
            );
        }
    }

    /// `QueueRepository` has no "the claim for this run" lookup, so the claim is
    /// found through `claims_for_platform` under the run's own tenant scope —
    /// the same small, tenant-correct query `service::dispatch` uses rather than
    /// the cross-tenant `all_claims`.
    async fn release_claim(
        &self,
        ctx: &SecurityContext,
        run: &Run,
        platform_id: Uuid,
    ) -> Result<(), DomainError> {
        let list_scope = self.queue_scope(ctx, actions::LIST, None).await?;
        let conn = self.db.conn()?;
        let claims = self
            .queue
            .claims_for_platform(&conn, &list_scope, platform_id)
            .await?;
        let Some(claim) = claims.into_iter().find(|claim| claim.run_id == run.id) else {
            return Ok(());
        };

        let scope = self
            .queue_scope(ctx, actions::DISPATCH, Some(claim.id))
            .await?;
        let conn = self.db.conn()?;
        self.queue.mark_done(&conn, &scope, claim.id).await?;
        info!(
            run_id = %run.id,
            queue_id = %claim.id,
            reason = COMPLETION_REASON,
            "queue claim released",
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "ingest_tests.rs"]
pub(in crate::domain::service) mod tests;
