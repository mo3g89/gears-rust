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
//! # Three families, not two
//!
//! The two NFR paths contribute a counter and a histogram each. The third
//! counter, [`QA_RUNS_DISPATCH_DECISION`], is a separate family rather than a
//! `decision` label on [`QA_RUNS_DISPATCH`], because the two carry different
//! label sets: one is keyed on how a dispatch pass ended, the other on what
//! admission decided before any pass happened. Folding them together would
//! widen a fixed label set, which is the mistake `account-management` records
//! at its own repair-runs family — a dashboard keyed on the narrower set stops
//! being able to sum its own series.

/// One dispatch pass: a claimed run taken to a started execution, or retired.
///
/// Labelled by [`crate::domain::ports::metrics::DispatchOutcome`].
pub const QA_RUNS_DISPATCH: &str = "qa_runs_dispatch_total";

/// Wall-clock duration of one dispatch pass — the p95 the DESIGN NFR is stated
/// over. Same label set as [`QA_RUNS_DISPATCH`], so a rate and a quantile can
/// be read side by side.
pub const QA_RUNS_DISPATCH_DURATION: &str = "qa_runs_dispatch_duration_seconds";

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
pub const COUNTERS: &[&str] = &[QA_RUNS_DISPATCH, QA_RUNS_DISPATCH_DECISION, QA_RUNS_INGEST];

/// Every duration histogram this gear exports. See [`COUNTERS`] for why the
/// list is declared.
pub const DURATIONS: &[&str] = &[QA_RUNS_DISPATCH_DURATION, QA_RUNS_INGEST_DURATION];

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metrics_tests.rs"]
mod tests;
