//! Run state machine, terminal-phase derivation, and crash-recovery rules.
//!
//! Three rule families with different provenance, kept in one module because
//! they are the only three that decide what state a run is in:
//!
//! * **Transitions are net-new** — but not because the source system lacked a
//!   run table. It has one: `run_results`, read as `PersistedRunRow`
//!   (`manager/src/services/run_history.rs:10-48`) and merged with the live
//!   Argo Workflow by `overlay_persisted_with_live` (`run_history.rs:216`),
//!   because the Workflow object is garbage-collected after its TTL while
//!   re-runs happen much later (`manager/migrations/001_initial.sql:306-310`).
//!   What it lacks is a **transition guard**: that row is written by one
//!   upsert whose conflict clause is an unconditional `phase = EXCLUDED.phase`
//!   (`run_history.rs:635-644`), i.e. it records whatever phase Argo last
//!   reported with no legality check anywhere. So there is nothing to port even
//!   though there is a table. `cpt-cf-qa-principle-db-first-state` makes our
//!   row authoritative, which means it needs one.
//! * **Phase derivation** is ported from `manager/src/services/argo.rs:2154-2227`
//!   with one deliberate divergence: legacy's rule that any skipped test
//!   downgrades a successful run (plan decision D2, recorded in
//!   `DESIGN.md:242-254`) is **not** ported. The product owner overrode it on
//!   2026-08-28 — see [`derive_terminal_state`]'s "Skips no longer fail a run"
//!   for the reasoning and what replaces the signal.
//! * **Crash recovery** is ported from `manager/src/services/run_queue.rs:349-361`
//!   and `manager/src/services/run_dispatcher.rs:417-487`, including the
//!   boot-only / tick-only split that is easy to collapse and dangerous to.
//!
//! Pure: every function takes already-observed facts. The dispatcher does the
//! observing.
//!
//! Spelling: this module deals in *run* states, so cancellation is
//! [`RunState::Canceled`] with one `l`. The *queue row*'s
//! `QueueState::Cancelled` has two, ported from `run_queue.rs:413`. Different
//! vocabularies on different tables — do not unify them.

use qa_runs_sdk::{RunResult, RunState};
use time::OffsetDateTime;

/// The six states a run reaches at the end of its life.
///
/// **Not "the six states from which no transition is legal", which is what this
/// line said until 2026-08-14.** Five of them are immutable;
/// [`RunState::Succeeded`] admits four downgrade edges by user decision — see
/// [`can_transition`], "The one mutable terminal state".
///
/// A named constant so [`is_terminal`], the terminal arm of [`can_transition`]
/// and the exhaustiveness tests all read the same list.
///
/// The functions that spell this set out are compiler-checked; **this constant
/// is not** — its length is declared rather than derived, and the tests that
/// sweep the terminal set iterate it, which makes it their own oracle.
/// `the_terminal_set_is_exactly_these_six_states` is the independent check;
/// that test's doc carries the full argument.
pub const TERMINAL_STATES: [RunState; 6] = [
    RunState::Succeeded,
    RunState::Failed,
    RunState::Canceled,
    RunState::TimedOut,
    RunState::Expired,
    RunState::Error,
];

/// Whether a run has reached the end of its life.
///
/// **This is "is the run over", not "is the row frozen"** — the two stopped being
/// the same question on 2026-08-14, when [`RunState::Succeeded`] gained four
/// downgrade edges.
///
/// **Every caller in this gear wants this one**, and none wants the narrower
/// question — but they ask it about two different things, which an earlier
/// revision of this paragraph flattened into one:
///
/// * about a run's **current** state, meaning *is the run over, so is there
///   anything left to do* — `service::runs`' two cancels, `service::ingest`'s
///   late-result drop, and `service::dispatch::fail_orphan`;
/// * about a **destination** state, meaning *does this transition end the run, so
///   should its log channel be released* — the reap gates in
///   `service::dispatch::transition` and `service::launch::transition`.
///
/// Both are `is_terminal`, and correctly so. See [`is_immutable_terminal`] for the
/// question nothing asks.
///
/// A `match` rather than a `matches!` for the same reason [`can_transition`]
/// and [`reconcile_recorded_state`] are: a new [`RunState`] variant must fail
/// compilation here. This is the place that failure would otherwise be
/// *silent* — an omitted variant would fall out of a `matches!` as `false`, so
/// a new terminal state would read as non-terminal and the whole guard would
/// quietly stop applying to it.
///
/// The three spellings of the terminal set stay three; this only closes the
/// omission hole. `is_terminal_agrees_with_the_terminal_set` still earns its
/// keep, because it catches *mis*classification — a variant on the wrong arm —
/// which the compiler cannot see.
#[must_use]
pub fn is_terminal(state: RunState) -> bool {
    match state {
        RunState::Succeeded
        | RunState::Failed
        | RunState::Canceled
        | RunState::TimedOut
        | RunState::Expired
        | RunState::Error => true,
        RunState::Created | RunState::Queued | RunState::Dispatching | RunState::Running => false,
    }
}

/// Whether `from -> to` is a legal transition.
///
/// The legal-successor table for `DESIGN.md:240`'s state machine
/// (`created → queued? → dispatching → running → terminal`). Matched
/// exhaustively on `from` on purpose: adding a [`RunState`] variant must fail
/// compilation here rather than silently inherit a permissive default, which a
/// `_` arm would allow.
///
/// Three guards worth naming:
///
/// * **Five of the six terminal states refuse every exit, including into
///   themselves; [`RunState::Succeeded`] refuses only the self-edge.** A
///   duplicate completion event is therefore rejected by this guard rather than
///   silently rewriting `finished_at`. Note this is a statement about the
///   *guard*, not about what ingest should do with a duplicate — see the
///   composition rule on [`reconcile_recorded_state`], which must no-op before
///   ever reaching here. See "The one mutable terminal state" below.
/// * **`Queued -> Running` is illegal.** A queued row must pass through
///   `Dispatching`, because that is the state that holds the platform claim
///   while the sync and bundle build run. The source system does the same, in
///   two places: an admitted launch is inserted *directly* as `dispatching`,
///   "which is what makes the row a claim before the caller submits"
///   (`manager/src/services/run_queue.rs:159-172`), and the only promotion out
///   of `queued` is `mark_dispatching`, whose `WHERE ... AND state = 'queued'`
///   predicate is "a cheap guard against double dispatch"
///   (`run_queue.rs:279-291`).
/// * **`Dispatching -> Failed | Error` without ever running.** A submit that
///   fails leaves no execution behind, and the source system marks such a row
///   failed straight from `dispatching`
///   (`manager/src/services/run_dispatcher.rs:47-58`, the `Err` arm calling
///   `mark_failed`).
///
/// **`Queued -> Expired` is the TTL sweep's edge, and only `Queued` has it.**
/// A queued run is not bounded by `cpt-cf-qa-fr-runs-timeout` — the source
/// system bounds it with `queue_ttl_seconds`, which expires the *queue row*
/// (`run_queue.rs:370-378`, `SET state = 'expired'`) and has no run-level
/// counterpart there, because legacy has no run row for a launch that never
/// started. Here there is one, and [`RunState::Expired`] is what it becomes
/// (added 2026-08-13 by user decision, raised by this table: reusing `Canceled`
/// would make the sweep indistinguishable from an operator cancel, and reusing
/// `TimedOut` would conflate a queue-wait clock with the execution deadline).
/// No state past `Queued` gains this edge — a run that has reached
/// `Dispatching` is no longer waiting on the queue, so `queue_ttl_seconds` no
/// longer applies to it.
///
/// # The one mutable terminal state
///
/// **`Succeeded -> {Failed, Error, Canceled, TimedOut}` added 2026-08-14 by user
/// decision**, raised by Task 15. Without those four edges
/// [`reconcile_recorded_state`]'s `Succeeded` arm was **dead code**: the only
/// caller that can reach it is an ingest path which must then ask this function,
/// and this function refused. The observable consequence was a fail-open
/// divergence from the source system — later evidence that says `Failed`, which
/// `effective_base_phase` records as `Failed`
/// (`manager/src/services/argo.rs:2154-2168`, pinned there by
/// `persisted_verdict_survives_a_still_green_workflow`), left the run
/// `Succeeded` here.
///
/// The four targets are **exactly** the values [`reconcile_recorded_state`] can
/// return out of a recorded `Succeeded`, which is what makes this the minimum
/// widening rather than a general relaxation:
/// `the_reconciler_and_the_guard_compose_without_a_gap` sweeps the pairing.
///
/// **`Succeeded -> Succeeded` stays refused**, and that half is load-bearing: it
/// is the duplicate-completion guard, and `service::ingest`'s no-op path is built
/// on never being handed that pair. `Succeeded -> Expired` stays refused too — a
/// run that reached the executor was never waiting on the queue.
///
/// **This widening changes what `can_transition` protects for callers other than
/// ingest**, so it was checked against each: `service::launch` only ever moves a
/// run out of `Created`; `service::runs`' cancel returns early on
/// [`is_terminal`] and so never asks about a terminal `from`; and
/// `service::dispatch`'s `fail_orphan` — which *can* be handed an
/// already-terminal run — grew an explicit [`is_terminal`] guard in the same
/// change, because `Succeeded -> Error` becoming legal would otherwise let a
/// stale orphan sweep overwrite a real verdict.
///
/// # `Dispatching -> Succeeded`, added 2026-08-15
///
/// The other four exits from `Dispatching` were already legal - `Failed`,
/// `Error`, `Canceled`, `TimedOut` - so a run that skipped `Running` could
/// record every verdict **except** the good one. That asymmetry was reachable,
/// not theoretical: `service::dispatch`'s `record_started` deliberately swallows
/// a failed `Dispatching -> Running` write, because the execution is genuinely
/// running by then and returning an error could trigger a caller retry that
/// double-submits. A transient database failure on that one write therefore
/// leaves a live run sitting in `Dispatching`, and when it finishes green
/// `service::ingest` asks for `Dispatching -> Succeeded` and is refused. The run
/// then stays `Dispatching` until the timeout sweep reclaims it as `TimedOut` -
/// a **wrong verdict recorded for a run that passed**, which is worse than the
/// failure it came from.
///
/// **The alternative was to stop `record_started` swallowing, and it was
/// rejected.** That swallow is a documented, reasoned decision ported from
/// `manager/src/services/run_dispatcher.rs:32-44`; undoing it to fix a state
/// table would trade a wrong verdict for a double submit.
///
/// What the edge costs is that a run may reach `Succeeded` without ever having
/// been observed `Running`. What gates it is the executor's own
/// `ExecutionEvent::Finished { Succeeded }`: only a completion event reaches
/// `service::ingest`'s terminal write at all.
///
/// **Corrected 2026-08-15.** This said the gate was the ingested per-test
/// counts - "which only exist for a run the executor actually ran". That is
/// false and this file falsifies it:
/// `derive_terminal_state(ExecutorOutcome::Succeeded, RunResult::default(),
/// None)` returns `Succeeded` on zero counts, pinned by
/// `no_node_failure_and_no_results_succeeds`. The edge is sound; the reason
/// given for it was not.
///
/// One further precision: "the other four exits from `Dispatching` were already
/// legal" is loose. `Expired` was not and still is not legal from
/// `Dispatching`, correctly - a run that reached the executor was never waiting
/// on the queue.
#[must_use]
pub fn can_transition(from: RunState, to: RunState) -> bool {
    match from {
        // Admission decided: start now, or wait. Either way, cancellable.
        RunState::Created => matches!(
            to,
            RunState::Queued | RunState::Dispatching | RunState::Canceled
        ),
        // The dispatcher claims a queued row, the operator drops it, or the TTL
        // sweep expires it; see the doc note above on why there is no `Running`
        // and no `TimedOut` here.
        RunState::Queued => matches!(
            to,
            RunState::Dispatching | RunState::Canceled | RunState::Expired
        ),
        // The executor accepted the run, or the submit itself failed, or the
        // control plane gave up on it. `Succeeded` is here for the reason this
        // function's doc gives under "Dispatching -> Succeeded".
        RunState::Dispatching => matches!(
            to,
            RunState::Running
                | RunState::Succeeded
                | RunState::Failed
                | RunState::Error
                | RunState::Canceled
                | RunState::TimedOut
        ),
        // Execution finished, one way or another.
        RunState::Running => matches!(
            to,
            RunState::Succeeded
                | RunState::Failed
                | RunState::Error
                | RunState::Canceled
                | RunState::TimedOut
        ),
        // The one terminal state later evidence may overturn — see this
        // function's doc, and [`reconcile_recorded_state`], whose `Succeeded` arm
        // these four edges exist to make reachable. `Succeeded` is absent from
        // the list on purpose: the self-edge is the duplicate-completion guard.
        RunState::Succeeded => matches!(
            to,
            RunState::Failed | RunState::Error | RunState::Canceled | RunState::TimedOut
        ),
        // Nothing leaves these five. Listed as patterns rather than caught by `_`
        // so a new `RunState` variant fails compilation here; that they are
        // exactly `TERMINAL_STATES` minus `Succeeded` is pinned by
        // `no_transition_leaves_a_terminal_state`.
        RunState::Failed
        | RunState::Canceled
        | RunState::TimedOut
        | RunState::Expired
        | RunState::Error => false,
    }
}

/// The terminal states from which **no** transition is legal — the five that are
/// not [`RunState::Succeeded`].
///
/// # It exists for one test, and has no production caller
///
/// Said plainly, because two earlier lines implied otherwise: this was described
/// as "the narrower predicate the sweeps use" and as serving callers that mean
/// *no write may follow*, and there are none. Every caller in this gear asks
/// [`is_terminal`] — *is the run over* — and each is correct to.
///
/// Its two uses are in this module's own tests, where it is what lets
/// `no_transition_leaves_a_terminal_state` keep sweeping the states the property
/// still holds for after `Succeeded` stopped being one of them. A predicate that
/// exists to keep a test honest is a legitimate thing to have; claiming callers
/// it does not have is not.
///
/// Composed from [`is_terminal`] rather than from [`TERMINAL_STATES`] — an
/// earlier line said the latter. The conclusion is the same, because
/// `is_terminal`'s exhaustive `match` and the constant are pinned to each other
/// by `is_terminal_agrees_with_the_terminal_set`, but this module is otherwise
/// precise that the terminal set has three spellings and which one is being read
/// matters.
#[must_use]
pub fn is_immutable_terminal(state: RunState) -> bool {
    is_terminal(state) && state != RunState::Succeeded
}

/// How the executor says an execution ended.
///
/// The `RunExecutor` port's terminal vocabulary, deliberately narrower than
/// [`RunState`]: the executor reports what *it* observed, and this module
/// decides what the *run* is, which is not the same answer — that gap is the
/// whole point of [`derive_terminal_state`].
///
/// There is deliberately no `Expired` here, and [`derive_terminal_state`] never
/// returns [`RunState::Expired`]: a run whose queue row outlived
/// `queue_ttl_seconds` never reached the executor, so expiry is recorded by the
/// TTL sweep, not derived from an outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutorOutcome {
    Succeeded,
    Failed,
    Errored,
    Canceled,
    TimedOut,
}

/// The run's terminal state, from the executor's outcome plus the ingested
/// results.
///
/// Ported from `manager/src/services/argo.rs:2170-2207`
/// (`derive_phase_from_result_flags` and its two wrappers), with one
/// deliberate divergence — see "Skips no longer fail a run" below. **A green
/// executor phase is not sufficient evidence that a run passed** — the
/// results get the final word — and there are two independent reasons:
///
/// 1. **Results say otherwise.** Any failed or errored result downgrades a
///    `Succeeded` outcome to `Failed` (`argo.rs:2186`). This arm exists
///    because Argo assessed a DAG solely by its *leaf* tasks, so a
///    `run_if: always` dependent that succeeded erased an ancestor's failure
///    from the workflow phase entirely (`argo.rs:2174-2178`).
/// 2. **A node died before reporting anything.** `node_failure` covers what the
///    results cannot: an execution node that could not download its bundle,
///    could not pull its image, or hit its own timeout leaves no failed rows
///    behind, and the workflow phase hid that failure whenever the node was not
///    a leaf (`argo.rs:2218-2227`, `dag_nodes_failed`). `None` means the
///    executor reports no per-node detail — the mock adapter does not — and is
///    treated as "no known node failure", matching legacy's empty node list.
///
/// # Skips no longer fail a run — a deliberate divergence, ratified 2026-08-28
///
/// Legacy is explicit and considered here, not silent: "a skipped test means
/// the run didn't fully execute, so it must never read as passing either"
/// (`manager/src/services/argo.rs:2197`). Plan decision D2 ported that rule
/// as-is, and it held until this task, including its consequence — the
/// open-bug skip-list (`SKIP_TESTS_WITH_BUGS`, `argo.rs:476-481`) makes the
/// runner skip tests, so a run that skipped only known-broken tests reported
/// `Failed`.
///
/// **The product owner overrode it on 2026-08-28.** The suites this platform
/// actually runs are full of environment-gated skips — a real run recently
/// produced 68 executed tests where a large share were gated skips — so the
/// old rule marked nearly every real run red and the colour stopped carrying
/// information. A skipped result no longer downgrades a `Succeeded` outcome;
/// only `counts.failed` does.
///
/// **This is a considered trade, not a free one: a false red is not worth
/// risking a false green.** A run where every test was gated out now reports
/// `Succeeded`, and nothing about that word says the run asserted nothing —
/// so the skip count must not go quiet along with the rule. It is surfaced in
/// the run list and the run detail view instead, with a visible marker on a
/// succeeded run whose `skipped` is non-zero
/// (`qa-platform-ui/src/components/runs/RunsTable.tsx`,
/// `qa-platform-ui/src/pages/RunDetailPage.tsx`). The state machine no longer
/// carries that signal; the UI does, so "succeeded, 68 skipped" cannot be
/// mistaken for a clean run.
///
/// `a_skipped_test_no_longer_downgrades_a_successful_execution` pins the new
/// behaviour in this module's own words, and still records the old rule this
/// diverges from.
///
/// **Where legacy's `ERROR` status went.** `has_failed` is `FAILED` *or*
/// `ERROR` in the source system (`argo.rs:2194-2196`), yet [`RunResult`] has no
/// error counter. That is not a loss: legacy folds the two statuses together in
/// *aggregation*, not in derivation. Its per-run count projection is
/// `COUNT(*) FILTER (WHERE tr.status IN ('FAILED', 'ERROR')) AS failed`, and
/// the same query produces exactly [`RunResult`]'s five numbers — passed,
/// failed, skipped, `in_progress` (`PENDING`/`RUNNING`), total
/// (`manager/src/routes/plans.rs:188-192`) — which it then feeds straight into
/// `derive_phase_from_counts(phase, failed, skipped)` (`plans.rs:214-218`). So
/// `counts.failed` here *is* legacy's `has_failed`, and a future error bucket
/// must be added to that sum rather than beside it.
///
/// **`counts.in_progress` is deliberately unread — do not "fix" this.** A run
/// whose executor reports `Succeeded` while `in_progress > 0` is reported
/// `Succeeded` on incomplete evidence. That is parity: legacy's derivation
/// never sees an in-progress case (`derive_phase_from_result_flags` reads only
/// the two flags), so a rule here would be net-new behaviour against
/// `cpt-cf-qa-principle-semantics-parity`. It is also probably unreachable — a
/// `Succeeded` outcome means every node finished, so a lingering `PENDING`
/// result indicates an ingest bug rather than a real state. Recorded as a known
/// gap by user decision on 2026-08-13; if ingest proves the case reachable,
/// that is the moment to revisit it.
///
/// The rule is idempotent: only `Succeeded` is ever downgraded, so feeding this
/// function its own output changes nothing (`argo.rs:2183-2185`). Legacy's arm
/// reads `Succeeded | Skipped` — the extra state is dropped here rather than
/// lost, for the same reason [`RunState::Expired`] has no [`ExecutorOutcome`]:
/// there is no `Skipped` outcome in this vocabulary, because a whole execution
/// being gated out is an Argo DAG concept with no counterpart behind the
/// `RunExecutor` port.
#[must_use]
pub fn derive_terminal_state(
    outcome: ExecutorOutcome,
    counts: RunResult,
    node_failure: Option<bool>,
) -> RunState {
    let base = match outcome {
        ExecutorOutcome::Succeeded => RunState::Succeeded,
        ExecutorOutcome::Failed => RunState::Failed,
        ExecutorOutcome::Errored => RunState::Error,
        ExecutorOutcome::Canceled => RunState::Canceled,
        ExecutorOutcome::TimedOut => RunState::TimedOut,
    };
    if base != RunState::Succeeded {
        return base;
    }
    // A skipped test no longer fails the run (product owner decision,
    // 2026-08-28), a deliberate divergence from `manager/src/services/argo.rs:2197`
    // — see this function's "Skips no longer fail a run" doc section. The
    // suites this platform runs are full of gated skips, so the old rule
    // reported almost every real run as failed and the colour stopped
    // carrying information. The skip count is surfaced in the run list and
    // detail views instead, so a run that asserted nothing is still legible.
    let results_say_no = counts.failed > 0;
    if results_say_no || node_failure == Some(true) {
        return RunState::Failed;
    }
    RunState::Succeeded
}

/// Reconcile a freshly derived state against the one already recorded.
///
/// Ported from `effective_base_phase` (`manager/src/services/argo.rs:2154-2168`),
/// whose own doc gives the reason: the poller resolves failures the workflow
/// phase cannot express — a node that died without emitting results — and
/// records that verdict; re-reading the still-green workflow must not undo it,
/// "or the list view and the poller would disagree forever and the run would be
/// re-synced on every cycle". So a run already recorded `Failed`/`Error` is
/// **never upgraded** by a later re-derivation that happens to see a clean
/// outcome.
///
/// Extended past legacy in one respect, deliberately: `Canceled`, `TimedOut`
/// and `Expired` are recorded *decisions* rather than derived phases, so they
/// also win over any re-derivation. Legacy had no equivalent — an Argo
/// terminate simply produced a `Failed` workflow, and there is no run-level
/// cancel or expiry state in the source system at all — so this is a divergence
/// the new state vocabulary forces, recorded here rather than left implicit.
/// `Expired` is the strongest case of the three: such a run never reached the
/// executor, so there is no outcome to re-derive from and anything `derived`
/// says about it is noise.
///
/// `Succeeded` is the only terminal state that does *not* win, which is exactly
/// legacy's rule — a green phase is the one verdict later evidence may
/// overturn.
///
/// Matched exhaustively for the same reason as [`can_transition`]: a new
/// [`RunState`] variant must fail compilation here rather than fall through a
/// `_` arm into "just take the derived value".
///
/// # Composing with [`can_transition`]
///
/// **When the result equals `recorded` there is nothing to write.** The caller
/// must no-op rather than feed the pair to [`can_transition`], which refuses the
/// self-edge on every terminal state: a late or duplicate `Finished` event on an
/// already-terminal run is a no-op at ingest, not an illegal transition. The
/// distinction matters because executors retry — turning a retry into a `409`
/// would make a correctly-delivered duplicate look like a fault. The guard's own
/// doc says a duplicate completion is "rejected by this guard rather than
/// silently rewriting `finished_at`"; that is true of the guard in isolation and
/// is *not* the ingest contract, which is this paragraph.
///
/// **When the result differs from `recorded`, [`can_transition`] admits it, and
/// that took a change to be true.** This arm's whole purpose is the downgrade of
/// a recorded `Succeeded`, and until 2026-08-14 `can_transition` refused every
/// exit from every terminal state — so the arm was **unreachable through any
/// composition that obeys the mandatory pre-check**, and a run legacy records as
/// `Failed` stayed `Succeeded` here. Task 15 surfaced it; the user resolved it by
/// opening `Succeeded -> {Failed, Error, Canceled, TimedOut}`, which is exactly
/// the set this function can return out of a recorded `Succeeded`.
///
/// So the two functions now compose without a gap: for every `recorded` and every
/// state [`derive_terminal_state`] can produce, the pair is either equal (no-op)
/// or a legal transition. `the_reconciler_and_the_guard_compose_without_a_gap`
/// sweeps it exhaustively, which is the check that catches a future
/// [`ExecutorOutcome`] variant reopening the hole.
#[must_use]
pub fn reconcile_recorded_state(recorded: RunState, derived: RunState) -> RunState {
    match recorded {
        RunState::Failed
        | RunState::Error
        | RunState::Canceled
        | RunState::TimedOut
        | RunState::Expired => recorded,
        RunState::Created
        | RunState::Queued
        | RunState::Dispatching
        | RunState::Running
        | RunState::Succeeded => derived,
    }
}

/// What the dispatcher found when it looked up a claim row's execution.
///
/// Three states rather than two booleans, because "is it still active?" is only
/// a question at all once there *is* an execution to ask about — a
/// `{ has_execution: false, execution_active: true }` pair would be an
/// unrepresentable-in-reality combination that a struct of two `bool`s happily
/// admits. These map one-for-one onto what the source system reads: the row's
/// nullable `workflow_name`, then its membership in the set of live workflows
/// (`manager/src/services/run_dispatcher.rs:440-457`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimExecution {
    /// The row carries no execution reference yet — legacy's
    /// `workflow_name IS NULL`.
    Absent,
    /// The executor still lists the referenced execution as active.
    Active,
    /// The referenced execution is terminal or no longer known to the executor.
    /// Legacy cannot tell those apart either, and does not need to.
    Gone,
}

/// What the dispatcher observed about one claim row.
///
/// A struct rather than two positional arguments: `age_seconds` and
/// [`reconcile_claim`]'s `orphan_timeout_seconds` are both plain second counts,
/// and a caller that transposed them would compile and silently invert the
/// guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimObservation {
    /// The state of the execution this row claims a platform for.
    pub execution: ClaimExecution,
    /// Seconds since the row was claimed, or since it was enqueued if it was
    /// never dispatched — the source system's `all_claims` coalesces exactly
    /// that way, `dispatched_at.unwrap_or(enqueued_at)`
    /// (`manager/src/services/run_queue.rs:341-346`). See [`elapsed_seconds`].
    pub age_seconds: u64,
}

/// What to do with a claim row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimAction {
    /// Leave it alone.
    Keep,
    /// Its execution is terminal or gone: release the claim so the platform
    /// frees and the queue drains.
    Release,
    /// It was left mid-dispatch and will never produce an execution: fail it.
    FailOrphaned,
}

/// Per-tick claim reconciliation.
///
/// Ported from `manager/src/services/run_dispatcher.rs:427-477`. The
/// `orphan_timeout` age guard is **load-bearing**, and this is the comment to
/// read before ever removing it: a row sits in `dispatching` with no execution
/// reference for the whole force-sync + bundle-build window, which is minutes.
/// Failing such a row early abandons a launch that is still in progress — and
/// momentarily releases its claim, which is exactly when a second run could be
/// admitted alongside an exclusive one (`run_dispatcher.rs:419-426`).
///
/// Note the ordering: an execution that is demonstrably alive is kept
/// regardless of age. Age only decides the fate of a row that has *no*
/// execution to point at.
#[must_use]
pub fn reconcile_claim(observation: ClaimObservation, orphan_timeout_seconds: u64) -> ClaimAction {
    match observation.execution {
        ClaimExecution::Active => ClaimAction::Keep,
        // Terminal or deleted — either way the platform is free. This is what
        // stops a stuck execution blocking the queue forever
        // (`run_dispatcher.rs:448-456`).
        ClaimExecution::Gone => ClaimAction::Release,
        ClaimExecution::Absent => {
            if observation.age_seconds >= orphan_timeout_seconds {
                ClaimAction::FailOrphaned
            } else {
                ClaimAction::Keep
            }
        }
    }
}

/// Boot-time claim recovery — a **different rule** from [`reconcile_claim`],
/// and the two must never be collapsed.
///
/// Ported from `RunQueueService::fail_orphaned_dispatching`
/// (`manager/src/services/run_queue.rs:349-361`), whose `UPDATE ... WHERE state
/// = 'dispatching' AND workflow_name IS NULL` has no age predicate at all. Its
/// doc gives the reason it does not need one: the rows are those "left
/// mid-submit by a manager restart", and "with a single replica, that submit is
/// definitively gone". That is only true at boot, which is why two other
/// comments in the source system say so outright — "never call it from a tick"
/// (`run_dispatcher.rs:425-426`) and "which is boot-only and acts on
/// `dispatching` rows; keep the two separate" (`run_queue.rs:368-369`).
/// Calling this from a tick would fail every launch that is merely mid-build.
///
/// A row that *does* carry an execution reference is left to the tick
/// reconciler, because only it knows whether that execution is still alive —
/// legacy's `WHERE ... workflow_name IS NULL` leaves exactly those rows
/// untouched.
///
/// # The age gate, added 2026-08-15, and why legacy does not need one
///
/// **Legacy's premise does not hold here.** Its whole justification for having
/// no age predicate is quoted above — *"with a single replica, that submit is
/// definitively gone"* — and this deployment's single-replica constraint is
/// enforced **nowhere**: not in `Gears.toml`, not in the manifest, not by a
/// startup check, and not by configuration, while `dispatcher_enabled` defaults
/// to `true`.
///
/// Without the gate the pass was destructive rather than merely eager, and the
/// blast radius is worse than a lost row. A second replica booting while the
/// first is inside `dispatch_one` — force-sync plus bundle build, minutes, with
/// `execution_ref` still NULL — would fail the queue row, move the run to
/// `Error` and release the lease. The first replica's submit then *succeeds*,
/// and `record_started`'s writes all fail against the now-`Error` run and are
/// swallowed by design. The result is a live execution with no claim, no lease
/// and a run the operator reads as failed — and nothing reconciles it
/// afterwards, because reconciliation matches claims to executions and the
/// claim is gone. The platform is then free to be double-booked.
///
/// There is no `claimed_by` column, so this pass cannot tell its own cluster's
/// stale rows from another replica's live ones even in principle. The gate is
/// what makes it **slow rather than destructive**, and it works whatever the
/// deployment looks like.
///
/// # What the gate costs, stated plainly
///
/// A row genuinely orphaned by this replica's own restart now waits out
/// `orphan_timeout_seconds` before its platform frees, where it used to free at
/// boot. That is the same wait the tick already imposes on the same class of
/// row, so the steady state is unchanged; what is lost is boot's head start.
///
/// It also narrows what makes this pass distinct from [`reconcile_claim`]. The
/// remaining differences are real but modest: this one has no executor listing,
/// so it cannot classify [`ClaimExecution::Gone`] and treats any row with a
/// reference as `Active` purely to reach `Keep`; and its caller reads to
/// exhaustion rather than through the tick's rotating window, so one pass
/// clears the whole backlog. Recorded because a reader comparing the two should
/// know the gap narrowed rather than assume it did not.
#[must_use]
pub fn boot_recovery_action(
    observation: ClaimObservation,
    orphan_timeout_seconds: u64,
) -> ClaimAction {
    match observation.execution {
        ClaimExecution::Absent => {
            if observation.age_seconds >= orphan_timeout_seconds {
                ClaimAction::FailOrphaned
            } else {
                ClaimAction::Keep
            }
        }
        ClaimExecution::Active | ClaimExecution::Gone => ClaimAction::Keep,
    }
}

/// Seconds elapsed since `since`, floored at 0.
///
/// Floored because `since` comes from the database clock while `now` comes from
/// this process's: a small negative skew must read "just now", not wrap into a
/// huge age. Shared by the claim reconciler and the TTL sweep so the two report
/// ages the same way (`manager/src/services/run_dispatcher.rs:479-487`).
///
/// Returns `u64` where legacy returns `i64` and every caller writes `as u64`
/// (`run_dispatcher.rs:459`): once the value is floored at 0 the sign carries
/// no information, and the cast is one more place to get it wrong.
///
/// `max(0)` then `unsigned_abs` rather than `u64::try_from(..).unwrap_or(0)`:
/// that shape floors *twice*, so deleting either half leaves the behaviour
/// identical and no test can pin the floor. One floor, one guard.
#[must_use]
pub fn elapsed_seconds(now: OffsetDateTime, since: OffsetDateTime) -> u64 {
    (now - since).whole_seconds().max(0).unsigned_abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-08-13 00:00:00 UTC as a Unix timestamp.
    const DAY: i64 = 1_786_579_200;

    fn at(offset_seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(DAY + offset_seconds)
            .expect("fixture instant is within the supported date range")
    }

    // ---------- transitions ----------

    #[test]
    fn the_happy_path_is_legal_end_to_end() {
        for (from, to) in [
            (RunState::Created, RunState::Dispatching),
            (RunState::Dispatching, RunState::Running),
            (RunState::Running, RunState::Succeeded),
        ] {
            assert!(can_transition(from, to), "{from:?} -> {to:?} must be legal");
        }
    }

    #[test]
    fn the_queued_path_is_legal_end_to_end() {
        for (from, to) in [
            (RunState::Created, RunState::Queued),
            (RunState::Queued, RunState::Dispatching),
            (RunState::Dispatching, RunState::Running),
            (RunState::Running, RunState::Failed),
        ] {
            assert!(can_transition(from, to), "{from:?} -> {to:?} must be legal");
        }
    }

    #[test]
    fn dispatching_may_fail_without_running() {
        assert!(can_transition(RunState::Dispatching, RunState::Error));
        assert!(can_transition(RunState::Dispatching, RunState::Failed));
    }

    /// **A run that skipped `Running` may still record a green verdict.**
    ///
    /// The edge exists because `service::dispatch::record_started` swallows a
    /// failed `Dispatching -> Running` write while the execution runs on. Every
    /// other exit from `Dispatching` was already legal, so before this the only
    /// verdict such a run could *not* record was the correct one, and it was
    /// left for the timeout sweep to relabel `TimedOut`.
    ///
    /// Asserted alongside the sibling it is easy to confuse it with:
    /// `Queued -> Succeeded` stays refused, because a run that never reached
    /// the executor has nothing to have succeeded at.
    #[test]
    fn a_run_that_skipped_running_may_still_succeed_but_a_queued_one_may_not() {
        assert!(can_transition(RunState::Dispatching, RunState::Succeeded));
        assert!(!can_transition(RunState::Queued, RunState::Succeeded));
        assert!(!can_transition(RunState::Created, RunState::Succeeded));
    }

    #[test]
    fn cancel_is_reachable_from_every_non_terminal_state() {
        for from in [
            RunState::Created,
            RunState::Queued,
            RunState::Dispatching,
            RunState::Running,
        ] {
            assert!(
                can_transition(from, RunState::Canceled),
                "{from:?} must be cancellable"
            );
        }
    }

    #[test]
    fn timeout_is_reachable_only_from_dispatching_and_running() {
        assert!(can_transition(RunState::Dispatching, RunState::TimedOut));
        assert!(can_transition(RunState::Running, RunState::TimedOut));
        assert!(!can_transition(RunState::Created, RunState::TimedOut));
        assert!(!can_transition(RunState::Queued, RunState::TimedOut));
    }

    /// The TTL sweep's edge. `Queued` is the only state that has it: a run that
    /// has reached `Dispatching` is no longer waiting on the queue, so
    /// `queue_ttl_seconds` no longer applies to it.
    #[test]
    fn only_a_queued_run_may_expire() {
        assert!(can_transition(RunState::Queued, RunState::Expired));
        for from in [RunState::Created, RunState::Dispatching, RunState::Running] {
            assert!(
                !can_transition(from, RunState::Expired),
                "{from:?} is not waiting on the queue and must not expire"
            );
        }
    }

    /// An expired run never reached the executor, so there is no outcome to
    /// re-derive it from and nothing may overwrite the sweep's verdict.
    #[test]
    fn an_expired_run_is_a_recorded_fact_not_a_phase_to_re_derive() {
        assert_eq!(
            reconcile_recorded_state(RunState::Expired, RunState::Succeeded),
            RunState::Expired
        );
    }

    #[test]
    fn a_queued_run_cannot_start_running_without_being_dispatched() {
        assert!(!can_transition(RunState::Queued, RunState::Running));
    }

    /// The same invariant from the other entry state. `Created` and `Queued`
    /// are the two states a run can reach `Running` from illegally, and only
    /// the `Queued` half was pinned: a `Created` row promoted straight to
    /// `Running` would be executing without ever having held the platform claim
    /// that `Dispatching` represents (`run_queue.rs:159-172`), which is the
    /// whole reason the intermediate state exists.
    #[test]
    fn a_created_run_cannot_start_running_without_being_dispatched() {
        assert!(!can_transition(RunState::Created, RunState::Running));
    }

    /// A run that never executed cannot report an *execution* outcome.
    ///
    /// `Created` and `Queued` may still reach a terminal state — `Canceled`
    /// from either, `Expired` from `Queued` — but those are *dispositions*,
    /// recorded by the control plane about a run it dropped. `Succeeded`,
    /// `Failed` and `Error` are verdicts on an execution that, from these two
    /// states, never happened. `Created -> Succeeded` is the fail-closed
    /// invariant at its sharpest: a run whose work never ran must never read as
    /// passing.
    #[test]
    fn a_run_that_never_executed_cannot_report_an_execution_outcome() {
        for from in [RunState::Created, RunState::Queued] {
            for to in [RunState::Succeeded, RunState::Failed, RunState::Error] {
                assert!(
                    !can_transition(from, to),
                    "{from:?} never executed and must not report {to:?}"
                );
            }
        }
    }

    /// The guard must accept every state the derivation can produce, or a real
    /// terminal event would be rejected and the run would stick in `Running`
    /// forever.
    ///
    /// The target set is taken from [`derive_terminal_state`] rather than
    /// written out, so the two cannot drift: adding an [`ExecutorOutcome`]
    /// variant extends what this sweeps automatically.
    ///
    /// That coupling is also this test's weakness, so `Running -> Error` is
    /// asserted directly as well. Deriving the targets means a mutation to
    /// *derivation* — `Errored => RunState::Failed` — would take `Error` out of
    /// the swept set and leave the edge unguarded here, with only a different
    /// function's test failing. The direct assertion pins the edge itself:
    /// `the_happy_path_is_legal_end_to_end` covers `Running -> Succeeded` and
    /// `the_queued_path_is_legal_end_to_end` covers `Running -> Failed`, but an
    /// errored execution on a running row had no test at all.
    #[test]
    fn running_may_reach_every_state_the_derivation_can_produce() {
        assert!(
            can_transition(RunState::Running, RunState::Error),
            "an errored execution must be able to terminate a running run"
        );
        for outcome in [
            ExecutorOutcome::Succeeded,
            ExecutorOutcome::Failed,
            ExecutorOutcome::Errored,
            ExecutorOutcome::Canceled,
            ExecutorOutcome::TimedOut,
        ] {
            let derived = derive_terminal_state(outcome, RunResult::default(), None);
            assert!(
                can_transition(RunState::Running, derived),
                "derivation turns {outcome:?} into {derived:?}, which the guard rejects"
            );
        }
    }

    #[test]
    fn a_run_cannot_go_backwards() {
        assert!(!can_transition(RunState::Running, RunState::Queued));
        assert!(!can_transition(RunState::Dispatching, RunState::Created));
        assert!(!can_transition(RunState::Queued, RunState::Created));
    }

    /// **Added 2026-08-14 by user decision.** A recorded `Succeeded` is the one
    /// verdict later evidence may overturn, and until this decision
    /// [`can_transition`] refused the downgrade that
    /// [`reconcile_recorded_state`]'s own `Succeeded` arm exists to produce.
    #[test]
    fn a_recorded_success_may_be_downgraded_by_later_evidence() {
        for to in [
            RunState::Failed,
            RunState::Error,
            RunState::Canceled,
            RunState::TimedOut,
        ] {
            assert!(
                can_transition(RunState::Succeeded, to),
                "later evidence must be able to record {to:?} over a green verdict"
            );
        }
    }

    /// The half of the same decision that must **not** move: the self-edge is the
    /// duplicate-completion guard, and `service::ingest`'s no-op path is built on
    /// never being handed that pair. `Expired` is excluded because a run that
    /// never reached the executor has no outcome to re-derive, and forward states
    /// are excluded because a finished run cannot un-finish.
    #[test]
    fn a_recorded_success_still_refuses_the_self_edge_and_every_other_target() {
        assert!(
            !can_transition(RunState::Succeeded, RunState::Succeeded),
            "a duplicate completion must not be able to rewrite finished_at"
        );
        for to in [
            RunState::Created,
            RunState::Queued,
            RunState::Dispatching,
            RunState::Running,
            RunState::Expired,
        ] {
            assert!(
                !can_transition(RunState::Succeeded, to),
                "Succeeded must not reach {to:?}"
            );
        }
    }

    /// **The composition, swept exhaustively** — the check neither function's own
    /// tests could make, and the one that would have caught the gap the user's
    /// decision closed.
    ///
    /// For every **terminal** recorded state and every state
    /// [`derive_terminal_state`] can produce, the pair
    /// `(recorded, reconcile_recorded_state(recorded, derived))` must be either
    /// *equal* — which every caller no-ops on — or a transition
    /// [`can_transition`] admits. Anything else is a verdict the pipeline computes
    /// and then cannot record, which is precisely what happened to a recorded
    /// `Succeeded` before 2026-08-14.
    ///
    /// # Why `Running` and not the other three non-terminal states
    ///
    /// `Running` is included because a completion arriving for a running run is
    /// the ordinary case and must never be refused. `Created`, `Queued` and
    /// `Dispatching` are **excluded on purpose, and their refusals are the state
    /// machine working**: a run in one of those states has not (yet) executed, and
    /// `a_run_that_never_executed_cannot_report_an_execution_outcome` pins that
    /// `Created`/`Queued` must not reach `Succeeded`. Including them here would
    /// have asserted the opposite of an invariant this module already defends —
    /// which is what the first draft of this test did, and what running it
    /// revealed.
    ///
    /// `service::ingest` answers those three with `IllegalTransition`, and
    /// `a_completion_on_a_queued_run_is_still_reported_rather_than_forced` pins
    /// the one of them that is genuinely reachable.
    ///
    /// The derived set is taken from [`derive_terminal_state`] rather than written
    /// out, so a future [`ExecutorOutcome`] variant extends this automatically.
    #[test]
    fn the_reconciler_and_the_guard_compose_without_a_gap() {
        let derivable: Vec<RunState> = [
            ExecutorOutcome::Succeeded,
            ExecutorOutcome::Failed,
            ExecutorOutcome::Errored,
            ExecutorOutcome::Canceled,
            ExecutorOutcome::TimedOut,
        ]
        .into_iter()
        .flat_map(|outcome| {
            // Both node-failure answers and both result shapes, so the derivation
            // is sampled across every branch it has.
            [
                derive_terminal_state(outcome, RunResult::default(), None),
                derive_terminal_state(outcome, RunResult::default(), Some(true)),
                derive_terminal_state(
                    outcome,
                    RunResult {
                        failed: 1,
                        total: 1,
                        ..RunResult::default()
                    },
                    None,
                ),
            ]
        })
        .collect();

        let recordable = TERMINAL_STATES.into_iter().chain([RunState::Running]);
        for recorded in recordable {
            for derived in &derivable {
                let reconciled = reconcile_recorded_state(recorded, *derived);
                assert!(
                    reconciled == recorded || can_transition(recorded, reconciled),
                    "recorded {recorded:?} + derived {derived:?} reconciles to \
                     {reconciled:?}, which is neither a no-op nor a legal transition"
                );
            }
        }

        // And the gap really was there: before the four `Succeeded` edges, this
        // pair was the counter-example. Asserted directly so the test cannot
        // become vacuous if the reconciler's `Succeeded` arm is ever made to win.
        assert_eq!(
            reconcile_recorded_state(RunState::Succeeded, RunState::Failed),
            RunState::Failed,
            "the arm these edges exist for"
        );
        assert!(can_transition(RunState::Succeeded, RunState::Failed));
    }

    /// Every state, for the sweeps below.
    const ALL_STATES: [RunState; 10] = [
        RunState::Created,
        RunState::Queued,
        RunState::Dispatching,
        RunState::Running,
        RunState::Succeeded,
        RunState::Failed,
        RunState::Canceled,
        RunState::TimedOut,
        RunState::Expired,
        RunState::Error,
    ];

    /// **Narrowed 2026-08-14, not deleted.** This swept all six terminal states
    /// and asserted that none of them admitted anything. Five still do not;
    /// `Succeeded` gained four exits by user decision, so it is excluded here and
    /// pinned by its own two tests below. Narrowing rather than deleting is the
    /// point — the property still holds for the states it holds for, and deleting
    /// it would have removed the only sweep that catches a *sixth* state
    /// accidentally gaining an exit.
    #[test]
    fn no_transition_leaves_a_terminal_state() {
        for from in TERMINAL_STATES
            .into_iter()
            .filter(|state| is_immutable_terminal(*state))
        {
            for to in ALL_STATES {
                assert!(
                    !can_transition(from, to),
                    "{from:?} is terminal but allowed a transition to {to:?}"
                );
            }
        }
        assert_eq!(
            TERMINAL_STATES
                .into_iter()
                .filter(|state| is_immutable_terminal(*state))
                .count(),
            5,
            "exactly one terminal state is exempt; if a second becomes mutable this \
             sweep silently stops covering it"
        );
    }

    /// The independent check on [`TERMINAL_STATES`] itself, and the reason the
    /// constant carries none of this argument: an editor of the constant is
    /// unlikely to break it, and an editor of *this test* is the person who
    /// needs to know why it exists.
    ///
    /// `no_transition_leaves_a_terminal_state` and
    /// `is_terminal_agrees_with_the_terminal_set` both *iterate* the constant,
    /// which makes it their own oracle — shrinking it to a single state leaves
    /// both of them passing while sweeping almost nothing. So the six states
    /// are written out here as a literal, deliberately **not** derived from the
    /// constant, and this is the only test that would notice the shrink.
    ///
    /// Membership plus cardinality is airtight without a separate "and nothing
    /// else is in it" loop: six slots required to hold six *pairwise distinct*
    /// values leave no room for a seventh, so such a loop could never fail and
    /// is omitted rather than written inert. That reasoning depends entirely on
    /// the distinctness of the literals below — write one of them twice and the
    /// constant regains room for a non-terminal state — so `expected` must stay
    /// a set of six different variants.
    #[test]
    fn the_terminal_set_is_exactly_these_six_states() {
        let expected = [
            RunState::Succeeded,
            RunState::Failed,
            RunState::Canceled,
            RunState::TimedOut,
            RunState::Expired,
            RunState::Error,
        ];
        assert_eq!(
            TERMINAL_STATES.len(),
            expected.len(),
            "TERMINAL_STATES changed size; every test that sweeps it now sweeps \
             a different set"
        );
        for state in expected {
            assert!(
                TERMINAL_STATES.contains(&state),
                "{state:?} is terminal but missing from TERMINAL_STATES"
            );
        }
    }

    #[test]
    fn is_terminal_agrees_with_the_terminal_set() {
        for state in TERMINAL_STATES {
            assert!(is_terminal(state), "{state:?} must be terminal");
        }
        for state in [
            RunState::Created,
            RunState::Queued,
            RunState::Dispatching,
            RunState::Running,
        ] {
            assert!(!is_terminal(state), "{state:?} must not be terminal");
        }
    }

    // ---------- phase derivation (decision D2) ----------

    #[test]
    fn a_clean_run_succeeds() {
        let counts = RunResult {
            passed: 10,
            failed: 0,
            skipped: 0,
            in_progress: 0,
            total: 10,
        };
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, counts, None),
            RunState::Succeeded
        );
    }

    #[test]
    fn a_failed_test_downgrades_a_successful_execution() {
        let counts = RunResult {
            passed: 9,
            failed: 1,
            skipped: 0,
            in_progress: 0,
            total: 10,
        };
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, counts, None),
            RunState::Failed
        );
    }

    /// **Pinned the opposite verdict until 2026-08-28.** This test used to
    /// assert `RunState::Failed` here, quoting decision D2: "any skipped test
    /// makes the run Failed (argo.rs:2186)". Legacy's own rule is explicit and
    /// was faithfully ported — `manager/src/services/argo.rs:2197`: "a skipped
    /// test means the run didn't fully execute, so it must never read as
    /// passing either". The product owner overrode it on 2026-08-28: the
    /// suites this platform runs are full of environment-gated skips, so that
    /// rule marked nearly every real run red and destroyed the signal it
    /// existed to give. See [`derive_terminal_state`]'s "Skips no longer fail
    /// a run" for the full reasoning, including where the lost signal went
    /// (the run list and detail UI, not this function).
    #[test]
    fn a_skipped_test_no_longer_downgrades_a_successful_execution() {
        let counts = RunResult {
            passed: 9,
            failed: 0,
            skipped: 1,
            in_progress: 0,
            total: 10,
        };
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, counts, None),
            RunState::Succeeded,
            "ratified divergence from legacy (argo.rs:2197), product owner decision 2026-08-28"
        );
    }

    /// A run that skipped every test still asserted nothing, and the skip
    /// count — not the run's state — is what must say so.
    #[test]
    fn a_run_that_skipped_everything_still_succeeds_but_the_skip_count_survives() {
        let counts = RunResult {
            passed: 0,
            failed: 0,
            skipped: 68,
            in_progress: 0,
            total: 68,
        };
        let derived = derive_terminal_state(ExecutorOutcome::Succeeded, counts, None);
        assert_eq!(
            derived,
            RunState::Succeeded,
            "everything gated out is not a failure - it is a run that asserted \
             nothing, which the skip count must make visible instead"
        );
        assert_eq!(counts.skipped, 68, "the count itself is the signal now");
    }

    /// The brief's own worked example: skips alone never fail a run, a real
    /// failure still does, and a dead node still does even with clean counts.
    #[test]
    fn a_skip_no_longer_fails_a_run_but_a_failure_still_does() {
        let clean_with_skips = RunResult {
            passed: 10,
            failed: 0,
            skipped: 5,
            in_progress: 0,
            total: 15,
        };
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, clean_with_skips, None),
            RunState::Succeeded
        );

        let one_failure = RunResult {
            passed: 10,
            failed: 1,
            skipped: 5,
            in_progress: 0,
            total: 16,
        };
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, one_failure, None),
            RunState::Failed
        );

        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, clean_with_skips, Some(true)),
            RunState::Failed,
            "a node that died without emitting results still fails the run"
        );
    }

    /// Legacy states idempotence by feeding the function its own output
    /// (`argo.rs:3077-3082`), which is not expressible here: this one returns a
    /// [`RunState`] but consumes an [`ExecutorOutcome`], so the round trip is
    /// mapped by hand and the assertion is correspondingly weaker.
    /// `only_a_succeeded_outcome_is_ever_downgraded` states the property this
    /// one gestures at.
    #[test]
    fn derivation_is_idempotent() {
        let counts = RunResult {
            passed: 0,
            failed: 1,
            skipped: 0,
            in_progress: 0,
            total: 1,
        };
        let once = derive_terminal_state(ExecutorOutcome::Succeeded, counts, None);
        let twice = derive_terminal_state(ExecutorOutcome::Failed, counts, None);
        assert_eq!(once, RunState::Failed);
        assert_eq!(twice, RunState::Failed);
    }

    #[test]
    fn only_a_succeeded_outcome_is_ever_downgraded() {
        let dirty = RunResult {
            passed: 0,
            failed: 3,
            skipped: 2,
            in_progress: 0,
            total: 5,
        };
        for (outcome, expected) in [
            (ExecutorOutcome::Failed, RunState::Failed),
            (ExecutorOutcome::Errored, RunState::Error),
            (ExecutorOutcome::Canceled, RunState::Canceled),
            (ExecutorOutcome::TimedOut, RunState::TimedOut),
        ] {
            assert_eq!(
                derive_terminal_state(outcome, dirty, Some(true)),
                expected,
                "{outcome:?} must pass through untouched by results"
            );
        }
    }

    #[test]
    fn clean_results_never_upgrade_a_failed_execution() {
        let counts = RunResult {
            passed: 10,
            failed: 0,
            skipped: 0,
            in_progress: 0,
            total: 10,
        };
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Failed, counts, None),
            RunState::Failed
        );
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Errored, counts, None),
            RunState::Error
        );
    }

    #[test]
    fn cancel_and_timeout_outcomes_map_straight_through() {
        let counts = RunResult {
            passed: 3,
            failed: 0,
            skipped: 0,
            in_progress: 0,
            total: 3,
        };
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Canceled, counts, None),
            RunState::Canceled
        );
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::TimedOut, counts, None),
            RunState::TimedOut
        );
    }

    #[test]
    fn a_node_failure_with_no_results_at_all_is_a_failure() {
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, RunResult::default(), Some(true)),
            RunState::Failed,
            "an execution node that died before emitting any result must not read as passing"
        );
    }

    #[test]
    fn no_node_failure_and_no_results_succeeds() {
        assert_eq!(
            derive_terminal_state(
                ExecutorOutcome::Succeeded,
                RunResult::default(),
                Some(false)
            ),
            RunState::Succeeded
        );
        assert_eq!(
            derive_terminal_state(ExecutorOutcome::Succeeded, RunResult::default(), None),
            RunState::Succeeded
        );
    }

    #[test]
    fn a_recorded_failure_is_never_upgraded_by_re_derivation() {
        assert_eq!(
            reconcile_recorded_state(RunState::Failed, RunState::Succeeded),
            RunState::Failed
        );
        assert_eq!(
            reconcile_recorded_state(RunState::Error, RunState::Succeeded),
            RunState::Error
        );
    }

    #[test]
    fn re_derivation_may_downgrade_a_recorded_success() {
        assert_eq!(
            reconcile_recorded_state(RunState::Succeeded, RunState::Failed),
            RunState::Failed
        );
    }

    #[test]
    fn re_derivation_leaves_agreeing_states_alone() {
        assert_eq!(
            reconcile_recorded_state(RunState::Succeeded, RunState::Succeeded),
            RunState::Succeeded
        );
        assert_eq!(
            reconcile_recorded_state(RunState::Canceled, RunState::Succeeded),
            RunState::Canceled,
            "a cancel is a recorded fact, not a phase to re-derive"
        );
    }

    // ---------- crash recovery ----------

    #[test]
    fn a_young_claim_with_no_execution_is_left_alone() {
        assert_eq!(
            reconcile_claim(
                ClaimObservation {
                    execution: ClaimExecution::Absent,
                    age_seconds: 30,
                },
                600
            ),
            ClaimAction::Keep
        );
    }

    #[test]
    fn a_claim_with_no_execution_past_the_orphan_timeout_is_failed() {
        assert_eq!(
            reconcile_claim(
                ClaimObservation {
                    execution: ClaimExecution::Absent,
                    age_seconds: 600,
                },
                600
            ),
            ClaimAction::FailOrphaned
        );
    }

    #[test]
    fn a_claim_whose_execution_is_still_active_is_left_alone() {
        assert_eq!(
            reconcile_claim(
                ClaimObservation {
                    execution: ClaimExecution::Active,
                    age_seconds: 100_000,
                },
                600
            ),
            ClaimAction::Keep,
            "age never fails a claim whose execution is demonstrably alive"
        );
    }

    #[test]
    fn a_claim_whose_execution_is_gone_is_released() {
        assert_eq!(
            reconcile_claim(
                ClaimObservation {
                    execution: ClaimExecution::Gone,
                    age_seconds: 10,
                },
                600
            ),
            ClaimAction::Release
        );
    }

    /// **Boot recovery consults age**, which it did not until 2026-08-15.
    ///
    /// The inverted assertion this replaces ("must not consult age") was
    /// faithful to legacy, whose justification for having no age predicate is
    /// that a single replica's interrupted submit is definitively gone. Nothing
    /// enforces a single replica here, and without the gate a booting replica
    /// fails another replica's in-flight dispatch - whose submit then succeeds
    /// into a run this one has already marked `Error`. See
    /// [`boot_recovery_action`].
    #[test]
    fn boot_recovery_spares_a_dispatch_that_may_still_be_in_flight() {
        for age in [0, 1, 599] {
            assert_eq!(
                boot_recovery_action(
                    ClaimObservation {
                        execution: ClaimExecution::Absent,
                        age_seconds: age,
                    },
                    600
                ),
                ClaimAction::Keep,
                "a claim younger than the orphan timeout may be another replica's \
                 live build (age = {age})"
            );
        }
    }

    #[test]
    fn boot_recovery_fails_a_row_that_has_outlived_the_orphan_timeout() {
        for age in [600, 601, 100_000] {
            assert_eq!(
                boot_recovery_action(
                    ClaimObservation {
                        execution: ClaimExecution::Absent,
                        age_seconds: age,
                    },
                    600
                ),
                ClaimAction::FailOrphaned,
                "a claim past the timeout with no execution reference is orphaned \
                 (age = {age})"
            );
        }
    }

    #[test]
    fn boot_recovery_leaves_a_row_that_has_an_execution_to_the_tick() {
        for execution in [ClaimExecution::Active, ClaimExecution::Gone] {
            assert_eq!(
                boot_recovery_action(
                    ClaimObservation {
                        execution,
                        age_seconds: 100_000,
                    },
                    600
                ),
                ClaimAction::Keep,
                "a row with an execution reference is the tick reconciler's business, \
                 because only it knows whether that execution is still alive"
            );
        }
    }

    // ---------- elapsed_seconds ----------

    #[test]
    fn elapsed_seconds_measures_the_gap() {
        assert_eq!(elapsed_seconds(at(5400), at(0)), 5400);
    }

    #[test]
    fn elapsed_seconds_floors_clock_skew_at_zero() {
        assert_eq!(elapsed_seconds(at(0), at(5)), 0);
    }
}
