//! qa-runs observability metric catalog.
//!
//! DESIGN's p95 NFRs are stated over dispatch and ingest, and until this module
//! existed nothing observed either path at all: `grep -rn 'metrics!|prometheus|
//! opentelemetry|histogram'` over all four qa-platform gears returned zero
//! non-test hits. The NFRs were not badly observed, they were unobservable.
//! Review finding #4.
//!
//! # Metric naming
//!
//! These constants are the **full, literal Prometheus series names** — what
//! appears in Prometheus / `VictoriaMetrics`. They bake in the suffix the
//! OTel→Prometheus translation would otherwise add: counters carry `_total`,
//! histograms carry the unit word. No `.with_unit()` hint is set on any
//! instrument, so the rendered name is identical whether the collector has
//! `add_metric_suffixes` on or off. `account-management`'s `domain::metrics`
//! documents this mechanism in its module header under *Metric naming*; this
//! follows it.
//!
//! `counter_names_carry_the_total_suffix_and_duration_names_carry_the_unit`
//! and `every_metric_is_namespaced_to_this_gear` are the gate. They are worth
//! having because the defect they catch is invisible: a series exported under a
//! name nobody queries looks, from inside this process, exactly like a series
//! that works.
//!
//! # Why typed ports and not an `emit_metric(&str, …)` facade
//!
//! `account-management` has both. Its facade bridge is documented there as *"a
//! transitional surface"* whose typed ports are *"the long-term API"*, and it
//! exists because that gear had call sites predating the ports. This gear has
//! none: there is nothing to transition from, so it starts where AM is going.
//! See [`crate::domain::ports::metrics`].
//!
//! # What the dispatch duration is, and what it is not
//!
//! [`QA_RUNS_DISPATCH_DURATION`] times one dispatcher **cycle**. It is the
//! dispatcher's own RED duration, not `cpt-cf-qa-nfr-dispatch-latency`: that
//! NFR bounds *platform release → execution request*, which is dominated by the
//! 5 s sweep interval, and no measurement of a cycle's wall-clock duration can
//! contain the gap between two cycles. [`QA_RUNS_QUEUE_WAIT_DURATION`] is the
//! family that goes at the NFR, and its own doc states exactly how far it gets
//! and where it falls short.
//!
//! # Five families, not two
//!
//! The two measured paths contribute a counter and a histogram each, and the
//! queue wait adds a third such pair on the dispatch side.
//!
//! [`QA_RUNS_DISPATCH_DECISION`] is a separate family rather than a
//! `decision` label on [`QA_RUNS_DISPATCH`], because the two carry different
//! label sets: one is keyed on how a dispatcher cycle ended, the other on what
//! admission decided before any cycle ran. Folding them together would
//! widen a fixed label set, which is the mistake `account-management` records
//! at its own repair-runs family — a dashboard keyed on the narrower set stops
//! being able to sum its own series.

/// One dispatcher cycle: `domain::service::dispatch`'s tick, from the TTL sweep
/// to the end of the drain, counted by how the cycle itself ended.
///
/// **Not one submission.** A tick claims and dispatches zero or more runs;
/// `dispatch_one` is inside it and is not counted here. See
/// [`crate::domain::ports::metrics::DispatchOutcome`], which labels this
/// family and enumerates the three ways a cycle ends.
pub const QA_RUNS_DISPATCH: &str = "qa_runs_dispatch_total";

/// Wall-clock duration of one dispatcher cycle. Same label set as
/// [`QA_RUNS_DISPATCH`], so a rate and a quantile can be read side by side.
///
/// **This is the dispatcher's own RED duration and it is not
/// `cpt-cf-qa-nfr-dispatch-latency`.** That NFR bounds *platform release →
/// execution request*, a quantity dominated by the 5 s sweep interval, and a
/// tick's wall-clock duration structurally cannot contain the interval between
/// ticks. What this answers is "is a cycle itself getting slower", which is the
/// question a rising [`QA_RUNS_QUEUE_WAIT_DURATION`] sends an operator to next.
pub const QA_RUNS_DISPATCH_DURATION: &str = "qa_runs_dispatch_duration_seconds";

/// One queued run taken from the queue to an accepted execution request.
///
/// Counts only runs that were genuinely **queued**: a launch admission decided
/// `Dispatch` for never enters the tick's FIFO, and a launch with no platform
/// is never queued at all.
///
/// **It counts departures, not arrivals.** Every increment is a row leaving the
/// queue *successfully*, so four populations are missing from it: a claimed row
/// whose dispatch failed, a queued row the TTL sweep expired before any tick
/// reached it, and — because `service::dispatch`'s `record_queue_wait` returns
/// early rather than record a span it cannot compute — a drained row with no
/// `enqueued_at` and a drained row whose stored instant is ahead of the drain's
/// clock. It therefore equals the rate at which runs are enqueued only in
/// steady state, and diverges from it in exactly the incidents an operator is
/// reading it during.
///
/// **The arrival side is [`QA_RUNS_DISPATCH_DECISION`], not
/// [`QA_RUNS_DISPATCH`].** `qa_runs_dispatch_decision_total{decision="queued"}`
/// counts admissions into the FIFO, which is the denominator this counter is
/// the numerator of; [`QA_RUNS_DISPATCH`] is per *cycle* and joins to neither.
/// Read the dispatcher's own report beside both.
///
/// It is the companion counter for [`QA_RUNS_QUEUE_WAIT_DURATION`]'s p95, which
/// observes the same events.
pub const QA_RUNS_QUEUE_WAIT: &str = "qa_runs_queue_wait_total";

/// **Queue residency**: how long a queued run sat in the queue before this
/// gear had recorded its execution as started.
///
/// It is the nearest thing to `cpt-cf-qa-nfr-dispatch-latency` the stored
/// timestamps support, and it is **not** that NFR. Read the two sections below
/// before writing an alert on it: the relationship holds in one direction on
/// the common path and in the other direction on three specific paths.
///
/// # What it measures, exactly
///
/// From the queue row's `enqueued_at` to `OffsetDateTime::now_utc()` read in
/// `service::dispatch`'s drain immediately after `dispatch_one` returned `Ok`.
/// Emitted once per queued run, by the tick that drained it.
///
/// **That end instant is later than the execution request itself.** `submit`
/// stamps the moment the executor accepted the run, and `record_started` then
/// writes the execution reference, marks the queue row running and transitions
/// the run — all before the drain reads its clock. So the tail of every
/// observation includes those three scoped writes. The accepted-at instant is
/// not visible to the drain: `dispatch_one` returns `Result<(), _>` and keeps
/// it inside itself. The error is one-signed — it can only inflate — so it
/// cannot mask a slow drain, and what it adds is three writes rather than a
/// wait on anything.
///
/// # How it relates to the NFR, in both directions
///
/// `DESIGN.md`'s row and `PRD.md` both state the requirement over *platform
/// release → execution request*. This series shares neither endpoint exactly.
///
/// **Where it over-states the NFR window** — the common case. A run enqueued
/// while its platform was occupied starts waiting before the platform is free,
/// so its residency contains the whole NFR window plus the predecessor's
/// remaining runtime. There the series is an upper bound, and an alert on it
/// cannot miss a violation, though on a busy platform the bound is loose enough
/// to be dominated by the predecessor rather than by anything the dispatcher
/// controls.
///
/// **Where it under-states it.** `queue::decide_admission` queues a launch
/// whenever the platform already has a queued row, *whatever* its occupancy —
/// strict FIFO, so a later arrival cannot overtake. A run can therefore be
/// enqueued while its platform is already free, and its residency then starts
/// *after* the release the NFR measures from. Three ways that happens, all
/// ordinary: a tick that stopped at the concurrency cap, a tick whose executor
/// listing failed, and simply the gap between sweeps. On those paths the
/// measured value is smaller than the NFR's quantity and an alert on it **can**
/// miss a violation.
///
/// Nothing in this gear can do better today, which is why the definition is
/// stated rather than fixed: the lease lives in `qa-environments`, and this gear
/// reads only free-or-held, never *since when*. Closing the gap needs a release
/// instant from that gear.
///
/// Read it as a trend, and beside [`QA_RUNS_DISPATCH_DURATION`] — a rising
/// residency with a flat cycle duration is a queue backing up, and a rising
/// cycle duration is the dispatcher itself slowing down.
pub const QA_RUNS_QUEUE_WAIT_DURATION: &str = "qa_runs_queue_wait_duration_seconds";

/// **`cpt-cf-qa-nfr-dispatch-latency` itself** — seconds between the instant a
/// queued run's environment became free and the instant this gear recorded that
/// run's execution as started.
///
/// Named for the two instants rather than for the requirement, because the
/// requirement is the thing that might be restated and the interval is not —
/// and because the name that was nearly used, `dispatch_latency`, is what the
/// retracted measurement thought it was reporting while reporting queue
/// residency. A reader who asks "latency of what, measured from where?" gets
/// the answer from the series name.
///
/// This is the NFR's own quantity, not a proxy for it. Read it instead of
/// [`QA_RUNS_QUEUE_WAIT_DURATION`] when the question is whether the
/// requirement holds; read that one when the question is how long runs are
/// waiting overall.
///
/// # The two instants, and how each is observed
///
/// * **Start of the window: the environment transitions to free.**
///   qa-environments stamps `qa_environment_leases.freed_at` inside the
///   compare-and-swap that writes `LeaseState::Free`, which happens only on a
///   release that actually frees the environment — a parallel holder leaving
///   while others remain writes a still-held state and stamps nothing. The
///   acquisition that later takes the environment *out* of `Free` reads the
///   column back and carries it in `AcquireOutcome::Acquired::became_free_at`,
///   which is how the instant reaches this gear. Because it is a column, the
///   window may span a control-plane restart: the release can happen in one
///   process lifetime and the start in the next.
/// * **End of the window: the same instant [`QA_RUNS_QUEUE_WAIT_DURATION`]
///   ends at** — `OffsetDateTime::now_utc()` read in `service::dispatch`'s
///   drain immediately after `dispatch_one` returned `Ok`. Sharing the end
///   point is deliberate: the two series then differ only in where they start,
///   so their difference is exactly the predecessor's remaining runtime and the
///   pair can be read together.
///
/// # Why the workload cannot dominate this one
///
/// The complaint that retired the previous measurement (2026-09-18;
/// `docs/DESIGN.md` §3.11, "The dispatch-latency window, and the measurement
/// that was retracted") was that queue residency is *queue depth × run
/// duration*: a slower test suite inflates it without the dispatcher changing.
/// Neither factor can enter this window.
///
/// * **Predecessor runtime is outside it by construction.** The clock starts
///   when the predecessor released, so nothing it did before that is in the
///   sample.
/// * **Queue depth does not accumulate into a sample.** With N runs queued
///   behind one environment, the second run's window starts at the *first*
///   run's release, not at the original free transition — each run is anchored
///   to the release that admitted it. Depth multiplies the number of samples,
///   never the value of one.
///
/// What remains inside the window is exactly what the dispatcher controls: the
/// wait for the next 5 s sweep, `evaluate_cap` stopping a tick at
/// `max_concurrent_runs`, the drain's own work, and the force-sync and bundle
/// build inside `dispatch_one`.
///
/// # Read it beside its companion counter
///
/// [`QA_RUNS_FREE_TO_START_UNANCHORED`] counts the drained runs that
/// produced **no** sample here. A p95 under 10 s over a population that
/// excluded most of the drain is not evidence the requirement holds, and the
/// two families together are what say how much of the drain the quantile
/// covers.
pub const QA_RUNS_FREE_TO_START_DURATION: &str = "qa_runs_free_to_start_duration_seconds";

/// Drained runs for which no [`QA_RUNS_FREE_TO_START_DURATION`] sample could
/// be taken, by why — the coverage denominator, and the reason the quantile
/// beside it can be trusted or not.
///
/// Labelled by [`crate::domain::ports::metrics::UnanchoredReason`]. This family
/// exists because the alternative is a silently self-selecting histogram: every
/// exclusion below is a case where the NFR's window is genuinely undefined for
/// that run, and a reader has no way to tell "the requirement holds" from "the
/// requirement was measured on four runs out of four hundred" unless the
/// excluded ones are counted in the open.
///
/// A rising `not_waiting_at_free` in particular is not a defect in this
/// measurement — it is the concurrency-cap and inter-sweep paths
/// [`QA_RUNS_QUEUE_WAIT_DURATION`]'s doc calls out, showing up as runs whose
/// wait began after their environment was already free.
pub const QA_RUNS_FREE_TO_START_UNANCHORED: &str = "qa_runs_free_to_start_unanchored_total";

/// What admission decided for a launch: dispatch inline, queue, or neither.
///
/// Labelled by [`crate::domain::ports::metrics::DispatchDecision`]. The signal
/// an operator reads before the queue-depth one: a queue that is growing
/// because every launch is being queued is a different incident from a queue
/// that is growing because dispatch is slow, and only this family tells them
/// apart.
pub const QA_RUNS_DISPATCH_DECISION: &str = "qa_runs_dispatch_decision_total";

/// One ingest pass: one observation from an execution applied to a run.
///
/// Labelled by [`crate::domain::ports::metrics::IngestOutcome`].
pub const QA_RUNS_INGEST: &str = "qa_runs_ingest_total";

/// Wall-clock duration of one ingest pass. Same label set as
/// [`QA_RUNS_INGEST`].
///
/// **It is the gear-side half of `cpt-cf-qa-nfr-result-latency`, not that
/// NFR.** PRD and `DESIGN.md` state the requirement over *runner event emission
/// → API visibility*. This span shares neither endpoint: it opens when the
/// event has already reached `IngestService::apply` and closes when that pass
/// returns, so it excludes the runner-to-gear transport on one side and the
/// read path on the other. What it bounds is the part this gear can act on, and
/// an alert on it can miss a violation whose whole cost was paid outside the
/// span — the same shape of gap [`QA_RUNS_QUEUE_WAIT_DURATION`] states in full
/// for the other NFR, and the same reason [`QA_RUNS_DISPATCH_DURATION`] carries
/// its own disclaimer.
pub const QA_RUNS_INGEST_DURATION: &str = "qa_runs_ingest_duration_seconds";

/// Every counter family this gear exports.
///
/// Declared rather than derived, and therefore its own oracle — the same
/// caveat `domain::state_machine`'s terminal-state constant carries. What
/// makes it worth having anyway is that the naming rules are properties *of
/// the set*: "every counter ends `_total`" is unstatable one constant at a
/// time, and a constant added without being listed here is a constant no rule
/// checks.
pub const COUNTERS: &[&str] = &[
    QA_RUNS_DISPATCH,
    QA_RUNS_DISPATCH_DECISION,
    QA_RUNS_QUEUE_WAIT,
    QA_RUNS_INGEST,
];

/// Every duration histogram this gear exports. See [`COUNTERS`] for why the
/// list is declared.
pub const DURATIONS: &[&str] = &[
    QA_RUNS_DISPATCH_DURATION,
    QA_RUNS_QUEUE_WAIT_DURATION,
    QA_RUNS_INGEST_DURATION,
];

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metrics_tests.rs"]
mod tests;
