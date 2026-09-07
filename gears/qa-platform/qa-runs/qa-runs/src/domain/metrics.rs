//! qa-runs observability metric catalog.
//!
//! DESIGN's p95 NFRs are stated over dispatch and ingest, and until this module
//! existed nothing measured either: `grep -rn 'metrics!|prometheus|
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
/// is never queued at all. So this family's rate is the queued arrival rate,
/// and it is the companion failure-rate query for
/// [`QA_RUNS_QUEUE_WAIT_DURATION`]'s p95.
pub const QA_RUNS_QUEUE_WAIT: &str = "qa_runs_queue_wait_total";

/// How long a queued run waited before its execution was requested — the
/// closest measurable stand-in for `cpt-cf-qa-nfr-dispatch-latency`, and
/// deliberately **not** claimed to be it.
///
/// # What it measures, exactly
///
/// From the queue row's `enqueued_at` to the instant
/// `RunExecutor::start` returned an accepted execution for that run. Emitted
/// once per queued run, by the tick that drained it.
///
/// # Where it diverges from the NFR, and in which direction
///
/// `DESIGN.md`'s row and `PRD.md` both state the requirement as *platform
/// release → execution request*. The **end** point matches: `start` returning
/// is the execution request. The **start** point does not — `enqueued_at` is
/// when the run joined the queue, which is earlier than when its platform
/// became free by however long the run ahead of it still had to run.
///
/// Nothing in this gear can do better today: the lease lives in
/// `qa-environments` and this gear reads only free-or-held, never *since
/// when*. So the measured value is an **upper bound** on the NFR's quantity.
/// That is the safe direction for an alert — it cannot hide a violation — but
/// it is a loose bound on a busy platform, where it is dominated by the
/// predecessor's runtime rather than by anything the dispatcher controls.
///
/// Read it against a low-occupancy window, or as a trend, and pair it with
/// [`QA_RUNS_DISPATCH_DURATION`] to tell a slow cycle from a long wait.
/// `DECOMPOSITION.md` records this NFR as "implemented, unmeasured"; this
/// narrows that gap and does not close it.
pub const QA_RUNS_QUEUE_WAIT_DURATION: &str = "qa_runs_queue_wait_duration_seconds";

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

/// Wall-clock duration of one ingest pass — the second p95 NFR. Same label set
/// as [`QA_RUNS_INGEST`].
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
