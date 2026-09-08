//! qa-environments observability metric catalog.
//!
//! One background path in this gear is measured here: the **observation
//! cycle** — `domain::service::EnvironmentsService`'s `run_observation_cycle`,
//! the body `crate::gear`'s observation ticker runs once per
//! `qa-environments.observation.poll_interval_seconds`. Before this module
//! existed nothing in any qa-platform gear measured it; the cycle's only
//! report was one `debug!` line per tick.
//!
//! # Metric naming
//!
//! These constants are the **full, literal Prometheus series names** — what
//! appears in Prometheus / `VictoriaMetrics`. They bake in the suffix the
//! OTel→Prometheus translation would otherwise add: counters carry `_total`,
//! histograms carry the unit word. No `.with_unit()` hint is set on any
//! instrument, so the rendered name is identical whether the collector has
//! `add_metric_suffixes` on or off. qa-runs' and qa-insights'
//! `domain::metrics` document the same mechanism in their module headers;
//! this follows them.
//!
//! `counter_names_carry_the_total_suffix_and_duration_names_carry_the_unit`
//! and `every_metric_is_namespaced_to_this_gear` are the gate. They are worth
//! having because the defect they catch is invisible: a series exported under
//! a name nobody queries looks, from inside this process, exactly like a
//! series that works.
//!
//! # Four families, at two levels, and why both levels exist
//!
//! | Level | Counter | Histogram | Label |
//! | --- | --- | --- | --- |
//! | the cycle | [`QA_ENVIRONMENTS_OBSERVATION_CYCLE`] | [`QA_ENVIRONMENTS_OBSERVATION_CYCLE_DURATION`] | [`crate::domain::ports::metrics::CycleOutcome`] |
//! | one environment | [`QA_ENVIRONMENTS_OBSERVATION`] | [`QA_ENVIRONMENTS_OBSERVATION_DURATION`] | see below |
//!
//! The cycle level is the ordinary RED shape every sibling gear uses: one
//! sample per pass, so sample volume follows the tick rate rather than the
//! workload.
//!
//! The per-environment level is the deliberate exception, and the reason is
//! arithmetic rather than preference. One cycle contacts **every** registered
//! environment's own cluster, serially, so its duration is a sum over a
//! population the platform sizes at a hundred. A cycle that took ten minutes
//! is one sample, and that sample cannot tell one unreachable cluster sitting
//! on a connect timeout from a hundred clusters each answering a little
//! slowly. Those two have completely different fixes, and only a
//! per-environment sample separates them.
//!
//! # What is deliberately *not* a label
//!
//! No environment id, no environment name, no tenant id, no cluster URL, no
//! kubeconfig-derived value. That is a **disclosure** rule before it is a
//! cardinality rule — see [`crate::domain::ports::metrics`]'s header, which
//! carries the argument, and this module's whole reason for labelling the
//! per-environment families by the plugin's own `FailureClass` instead.

/// One observation cycle: `domain::service::EnvironmentsService`'s
/// `run_observation_cycle`, from the connection acquisition to the last
/// environment (or to the early return that ended it), counted by how the
/// cycle itself ended.
///
/// **Not one environment.** A cycle visits every registered environment
/// across every tenant; how each of those went is [`QA_ENVIRONMENTS_OBSERVATION`]
/// and [`QA_ENVIRONMENTS_OBSERVATION_DURATION`].
///
/// Labelled by [`crate::domain::ports::metrics::CycleOutcome`].
pub const QA_ENVIRONMENTS_OBSERVATION_CYCLE: &str = "qa_environments_observation_cycle_total";

/// Wall-clock duration of one observation cycle. Same label set as
/// [`QA_ENVIRONMENTS_OBSERVATION_CYCLE`], so a rate and a quantile can be read
/// side by side, and so a cycle that ended early is not mixed into the
/// distribution of cycles that ran to the end.
///
/// # What it answers: is a cycle still finishing inside its tick interval
///
/// The ticker uses `tokio::time::MissedTickBehavior::Delay`, so a cycle that
/// outruns its interval does not queue a burst — it silently pushes every
/// later tick back. Nothing else in the gear reports that. `ObservationConfig`
/// floors the interval at 60 s and defaults it to 300 s, and
/// `crate::infra::metrics`' declared boundaries put an edge on both numbers so
/// the question can be asked without interpolating across a bucket.
///
/// **It is not an NFR.** See [`QA_ENVIRONMENTS_OBSERVATION_DURATION`]'s header
/// for what `cpt-cf-qa-nfr-scale` actually states and why neither family here
/// measures it.
pub const QA_ENVIRONMENTS_OBSERVATION_CYCLE_DURATION: &str =
    "qa_environments_observation_cycle_duration_seconds";

/// The environments one cycle reported on, **taken from the
/// `ObservationCycleReport` that cycle returns** rather than from a counter
/// incremented inside its loop.
///
/// That is the whole point of this family. The report is already the value the
/// ticker logs at `debug!` (`crate::gear`'s `observation_ticker`), so emitting
/// the same two fields makes the log line and the series structurally unable
/// to disagree: there is one addition site for each, and it is the report's
/// own field.
///
/// Labelled by [`crate::domain::ports::metrics::EnvironmentOutcome`], whose two
/// values *are* the report's two counters. The third,
/// `ObservationCycleReport::attempted`, is deliberately not its own series: it
/// is exactly the sum of the other two on every path through the loop, so a
/// series for it would be a second copy of an addition Prometheus can do.
///
/// # Its relationship to [`QA_ENVIRONMENTS_OBSERVATION_DURATION`]
///
/// The two carry different label dimensions on purpose, and the finer one is a
/// refinement of the coarser: every value of
/// [`crate::domain::ports::metrics::ObservationClass`] rolls up to exactly one
/// [`crate::domain::ports::metrics::EnvironmentOutcome`], through
/// `ObservationClass::counts_as_observed`. `the_two_environment_taxonomies_agree_on_what_counts_as_observed`
/// is what keeps the two partitions from drifting.
pub const QA_ENVIRONMENTS_OBSERVATION: &str = "qa_environments_observation_total";

/// Wall-clock duration of **one environment's whole observation** inside a
/// cycle — `EnvironmentsService`'s `observe_environment_classified`, measured
/// around the entire call.
///
/// # What is inside the span
///
/// Everything that method does, in order: the PEP check for `qa.platform`
/// `update`, the database connection, the row read, the plugin resolution
/// (a call into qa-catalog), the credential read out of credstore, the
/// plugin's own round trip to that environment's systems, the observation
/// write, and the re-read of the row afterwards. The clock is stopped the
/// instant that call returns, before the cycle's own logging and bookkeeping,
/// so nothing this gear does *between* environments is charged to any of them.
///
/// **Only the seven plugin-reported classes are guaranteed to contain a round
/// trip.** [`crate::domain::ports::metrics::ObservationClass::Refused`] and
/// [`crate::domain::ports::metrics::ObservationClass::Failed`] are the `Err`
/// half of that method, and an `Err` stops the span wherever it was raised.
/// Some of those points are before the plugin is reached — the PEP denial, the
/// failed connection, the row that was not there — and those samples are the
/// cost of a denial or of a broken database with no network round trip in them
/// at all. Others are after the plugin has already answered: the observation
/// write and the re-read below it can both fail, and such a sample does
/// contain the round trip plus a failed database call.
///
/// So those two values are not one distribution and are not the other seven's
/// either. That is what the label is for: a quantile taken over the whole
/// family without splitting by `class` mixes a connect timeout, a PDP denial
/// and a failed write into one number.
///
/// # The family that carries the weight, and why the cycle duration cannot
///
/// A cycle's duration is a sum over the whole registered population. At the
/// hundred environments the platform is sized for, one cluster sitting on a
/// connect timeout and a hundred clusters each answering a little slowly
/// produce the *same* cycle duration and want opposite fixes. This is the
/// family that tells them apart, and it does so without naming a single
/// environment: the label is
/// [`crate::domain::ports::metrics::ObservationClass`], whose failure half is
/// the plugin contract's own `FailureClass`.
///
/// # What `cpt-cf-qa-nfr-scale` states, measured rather than assumed
///
/// The plan that added this family cites `cpt-cf-qa-nfr-scale` as the NFR it
/// serves. **It does not measure that NFR, and the claim is written here in
/// the form the source documents support rather than in the form the plan
/// wished for:**
///
/// * PRD states the NFR as a **capacity envelope** — "100 registered target
///   platforms, 50 concurrently executing runs, 5,000 test files per catalog,
///   10,000 per-test results per run, and 5 million retained test-result rows"
///   — and its single latency clause applies to *collection endpoints* at that
///   volume. The observation cycle is a background ticker, not a collection
///   endpoint.
/// * DESIGN's NFR table allocates the row to **qa-runs + qa-insights**, with a
///   named qa-catalog carve-out. qa-environments is not in its "Allocated To"
///   column at all, and the one qa-environments surface the NFR's first number
///   does reach is `EnvironmentsService::list_environments`, whose own doc
///   already claims it.
///
/// So no threshold exists for this series to be compared against, and none is
/// invented here. What it genuinely reports is the **cost the NFR's first
/// number is paid in**: at a hundred registered environments the cycle's work
/// is a hundred network round trips, and this is the only place the shape of
/// that hundred is visible. Read it beside
/// [`QA_ENVIRONMENTS_OBSERVATION_CYCLE_DURATION`], which says whether the sum
/// still fits in a tick.
pub const QA_ENVIRONMENTS_OBSERVATION_DURATION: &str =
    "qa_environments_observation_duration_seconds";

/// Every counter family this gear exports.
///
/// Declared rather than derived, and therefore its own oracle. What makes it
/// worth having anyway is that the naming rules are properties *of the set*:
/// "every counter ends `_total`" is unstatable one constant at a time, and a
/// constant added without being listed here is a constant no rule checks.
///
/// # Read only by the tests, and that is not dead code
///
/// `domain` is `pub(crate)` in this gear, where it is `pub` in qa-runs and
/// qa-insights, so a catalog constant nothing outside the naming tests names
/// is genuinely unreachable and the compiler says so. The allowance is on the
/// two list constants only, and the alternative — deleting them — deletes the
/// gate: every naming rule in this catalog is a property *of the set*, and a
/// family absent from these lists is a family no rule checks.
#[allow(
    dead_code,
    reason = "read by `domain::metrics::tests` and `infra::metrics::tests`; `domain` is \
              pub(crate) in this gear, so a constant with no production reader is \
              unreachable and deleting it would delete the naming gate"
)]
pub const COUNTERS: &[&str] = &[
    QA_ENVIRONMENTS_OBSERVATION_CYCLE,
    QA_ENVIRONMENTS_OBSERVATION,
];

/// Every duration histogram this gear exports. See [`COUNTERS`] for why the
/// list is declared, and for why it carries a dead-code allowance.
#[allow(dead_code, reason = "see COUNTERS")]
pub const DURATIONS: &[&str] = &[
    QA_ENVIRONMENTS_OBSERVATION_CYCLE_DURATION,
    QA_ENVIRONMENTS_OBSERVATION_DURATION,
];

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metrics_tests.rs"]
mod tests;
