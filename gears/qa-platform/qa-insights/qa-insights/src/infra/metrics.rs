//! `OpenTelemetry`-backed adapter for the qa-insights metric ports.
//!
//! [`QaInsightsMetricsMeter`] owns one instrument per family in
//! [`crate::domain::metrics`] and implements every trait in
//! [`crate::domain::ports::metrics`]. One `Arc<QaInsightsMetricsMeter>` is
//! built at gear init by [`build_default_adapter`] and coerced into the
//! per-trait `Arc<dyn _>` views the two services hold.
//!
//! # Instrument naming: the catalog constant *is* the instrument name
//!
//! Every instrument is built from the catalog constant verbatim — the full,
//! literal Prometheus series name, with the suffix the `OTel`→Prometheus
//! translation would otherwise add already baked in — and **no `.with_unit()`
//! hint is set on any of them**. That is what makes the rendered series name
//! identical whether the downstream collector has `add_metric_suffixes` on or
//! off: the name is already literal and unit-free at the instrument level, so
//! the exporter dedups the existing `_total` and adds no unit suffix.
//!
//! # No `prefix` parameter, unlike `account-management`'s adapter
//!
//! AM's adapter takes `(meter, prefix)` and composes each name with `format!`,
//! so its catalog constants equal the rendered name only under the default
//! prefix. This adapter takes the meter alone and reads the names straight out
//! of [`crate::domain::metrics`], following qa-runs' answer to the same
//! question.
//!
//! The reason is the gate. `domain::metrics`'s naming tests assert properties
//! **of the constants**, and they are only a gate on what a dashboard finds if
//! the constant is what the instrument is named. A prefix parameter puts a
//! `format!` between the two and re-opens exactly the defect those tests exist
//! to catch — a series exported under a name nobody queries, which from inside
//! this process looks identical to one that works. AM needs the parameter
//! because its transitional stringly-typed facade dispatches on the family
//! name; this gear has no facade, so it has nothing to buy with it.
//!
//! Test isolation, the parameter's other use, is bought here by giving each
//! test its own [`opentelemetry_sdk::metrics::SdkMeterProvider`] rather than
//! sharing the process-global one.
//!
//! # Emission cannot fail
//!
//! Every method is a `Counter::add` or a `Histogram::record` on an instrument
//! this struct already holds: no `?`, no `unwrap`, no lookup that could miss,
//! and no allocation beyond the attribute array. Nothing here can return an
//! error, and nothing here panics.
//!
//! That is only half of the guarantee, because the call sites take
//! `Arc<dyn CollectMetrics>` / `Arc<dyn JiraPollMetrics>` and this is not the
//! only implementation they can be handed. The other half lives at the call
//! site: `domain::service`'s `emit` runs one emission so that a panicking
//! implementation cannot take the measured path with it, and latches off after
//! the first panic. See its doc.
//!
//! # Without a pipeline, every instrument is a no-op
//!
//! [`build_default_adapter`] reads the **process-global** meter provider.
//! `toolkit`'s `telemetry::init_metrics_provider` leaves that provider as the
//! built-in `NoopMeterProvider` when `metrics.enabled` is false, and it is
//! never replaced if a gear configures no pipeline at all — so every instrument
//! built below is a no-op, every emission is silent, and nothing in the boot
//! path can fail for want of a collector. The adapter is therefore installed
//! unconditionally at init; there is no "metrics off" branch to get wrong.

use std::sync::Arc;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};

use crate::domain::metrics::{
    QA_INSIGHTS_COLLECT, QA_INSIGHTS_COLLECT_DURATION, QA_INSIGHTS_COLLECT_REPORT,
    QA_INSIGHTS_JIRA_BUG, QA_INSIGHTS_JIRA_POLL, QA_INSIGHTS_JIRA_POLL_DURATION,
    QA_INSIGHTS_JIRA_RERUN,
};
use crate::domain::ports::metrics::{
    CollectMetrics, CollectOutcome, CollectReportOutcome, JiraBugOutcome, JiraPollMetrics,
    JiraPollOutcome,
};

/// The `outcome` label key, shared by every family here that carries one — one
/// word, so a dashboard that pivots the collect cycle by outcome uses the same
/// expression on the poll and on the per-bug family.
const OUTCOME: &str = "outcome";

/// Explicit second boundaries for every duration histogram.
///
/// Declared rather than left to the SDK default, and this is load-bearing. The
/// `OTel` defaults are `[0, 5, 10, 25, 50, … 10000]`, which are milliseconds in
/// all but name. Read as **seconds**, their first interval above zero is
/// `(0, 5]`, and a p95 read off that bucket is its upper edge rather than an
/// estimate of anything. A quantile query is the entire reason both duration
/// families exist, so the boundaries are part of the metric's definition rather
/// than a tuning knob.
///
/// **The two families this gear measures are not sub-second, and the set is
/// chosen for that.** A collect cycle issues one qa-catalog universe read and
/// then one qa-runs launch call per repository, serially; a JIRA poll pass
/// issues one JIRA status call per open bug, serially, across a network this
/// gear does not own. Both are naturally in the seconds-to-minutes range on a
/// real tenant, which is why the boundaries run to five minutes rather than to
/// the one minute qa-runs' set stops at — its two families are a database-bound
/// cycle and a single-row upsert. The low end still goes down to 10 ms because
/// both paths have a legitimate trivial case: an empty universe, and a tenant
/// with no open bugs.
///
/// **The 300 s ceiling is a stated limit, not a claim of adequacy.** Either
/// family can exceed it — a universe of hundreds of repositories, a tenant with
/// thousands of open bugs and a slow JIRA — and both then land in the overflow
/// bucket where a quantile is again an edge. Extending the high end is a change
/// to make with real cycle and pass data in hand rather than by guessing at one
/// now; past five minutes the counter beside each histogram and the trend are
/// what an operator reads.
///
/// **No boundary here is an NFR threshold.** qa-runs' set puts boundaries on
/// two design thresholds so an alert lands on a bucket edge; this gear's design
/// states no bound over either measured path, so there was no number to place
/// and none was invented.
const DURATION_BUCKETS: &[f64] = &[
    0.01, 0.05, 0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];

/// The instrumentation scope this gear's instruments are reported under.
const SCOPE: &str = "qa-insights";

/// Owns one `OpenTelemetry` instrument per qa-insights metric family and
/// implements every port trait over them.
///
/// **The instruments are built once, in [`Self::new`], and held.** Looking one
/// up per emission would put a map probe and a string hash on the measured
/// paths, and the per-bug family emits once per open bug per pass.
pub struct QaInsightsMetricsMeter {
    collect: Counter<u64>,
    collect_duration: Histogram<f64>,
    collect_report: Counter<u64>,
    jira_poll: Counter<u64>,
    jira_poll_duration: Histogram<f64>,
    jira_bug: Counter<u64>,
    jira_rerun: Counter<u64>,
}

impl QaInsightsMetricsMeter {
    /// Build every instrument this gear exports.
    ///
    /// Names come from [`crate::domain::metrics`] verbatim and carry no
    /// `.with_unit()` hint; see this module's header for both reasons.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            collect: meter
                .u64_counter(QA_INSIGHTS_COLLECT)
                .with_description(
                    "Collect cycles, by how the cycle ended (completed / refused / failed)",
                )
                .build(),
            collect_duration: meter
                .f64_histogram(QA_INSIGHTS_COLLECT_DURATION)
                .with_description("Wall-clock duration of one collect cycle, in seconds")
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
            collect_report: meter
                .u64_counter(QA_INSIGHTS_COLLECT_REPORT)
                .with_description(
                    "Reports on the runner's signed collect callback, by what the route did \
                     with each (recorded / secret_unconfigured / signature_malformed / \
                     signature_invalid / invalid / failed)",
                )
                .build(),
            jira_poll: meter
                .u64_counter(QA_INSIGHTS_JIRA_POLL)
                .with_description(
                    "JIRA poll passes, by how the pass ended \
                     (completed / skipped / refused / failed)",
                )
                .build(),
            jira_poll_duration: meter
                .f64_histogram(QA_INSIGHTS_JIRA_POLL_DURATION)
                .with_description("Wall-clock duration of one JIRA poll pass, in seconds")
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
            jira_bug: meter
                .u64_counter(QA_INSIGHTS_JIRA_BUG)
                .with_description(
                    "Bugs processed by the JIRA poller, by where each bug's chain stopped; \
                     the failure values are the poller's own swallowed per-bug failures, \
                     which nothing else reports",
                )
                .build(),
            jira_rerun: meter
                .u64_counter(QA_INSIGHTS_JIRA_RERUN)
                .with_description(
                    "Auto-rerun launches attempted by the JIRA poller, whether or not \
                     qa-runs accepted them",
                )
                .build(),
        }
    }
}

impl CollectMetrics for QaInsightsMetricsMeter {
    fn collect_cycle(&self, outcome: CollectOutcome, duration: Duration) {
        let labels = [KeyValue::new(OUTCOME, outcome.as_str())];
        self.collect.add(1, &labels);
        self.collect_duration
            .record(duration.as_secs_f64(), &labels);
    }

    fn collect_report(&self, outcome: CollectReportOutcome) {
        self.collect_report
            .add(1, &[KeyValue::new(OUTCOME, outcome.as_str())]);
    }
}

impl JiraPollMetrics for QaInsightsMetricsMeter {
    fn poll_pass(&self, outcome: JiraPollOutcome, duration: Duration) {
        let labels = [KeyValue::new(OUTCOME, outcome.as_str())];
        self.jira_poll.add(1, &labels);
        self.jira_poll_duration
            .record(duration.as_secs_f64(), &labels);
    }

    fn bug(&self, outcome: JiraBugOutcome) {
        self.jira_bug
            .add(1, &[KeyValue::new(OUTCOME, outcome.as_str())]);
    }

    fn auto_rerun(&self) {
        // No attributes at all: the family is one series, and every dimension
        // that would distinguish two reruns — the tenant, the bug, the plan,
        // the platform — is either unbounded cardinality or free text out of
        // JIRA. The constant's own doc carries the argument.
        self.jira_rerun.add(1, &[]);
    }
}

/// Build the adapter against the process-global `OpenTelemetry` meter provider.
///
/// The one call site is `gear.rs`'s init. Safe to call whether or not a
/// pipeline was ever configured: with no provider installed the global one is
/// the built-in no-op, so every instrument this returns is a no-op and every
/// emission through it is silent. See this module's header.
#[must_use]
pub fn build_default_adapter() -> Arc<QaInsightsMetricsMeter> {
    let scope = opentelemetry::InstrumentationScope::builder(SCOPE).build();
    let meter = opentelemetry::global::meter_with_scope(scope);
    Arc::new(QaInsightsMetricsMeter::new(&meter))
}

/// A private meter provider and the series it collected, for tests.
///
/// Lives here, beside the adapter, rather than in a test file, because three
/// modules read it: this module's own tests, `domain::service::collect`'s and
/// `domain::service::jira_poller`'s. The two call-site suites are the ones that
/// matter most — what needs proving there is that the **rendered series** is
/// what a dashboard query will find, and a mock of the port would prove only
/// that the call site calls the port.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod probe {
    use std::sync::Arc;

    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    use super::QaInsightsMetricsMeter;

    /// A meter provider private to one test, its in-memory exporter, and the
    /// adapter built over it.
    ///
    /// Private rather than global: two tests running in the same process share
    /// the global provider, and a counter is cumulative, so a shared one makes
    /// every `assert_eq!(counter, 1)` order-dependent.
    pub struct MetricsProbe {
        provider: SdkMeterProvider,
        exporter: InMemoryMetricExporter,
        adapter: Arc<QaInsightsMetricsMeter>,
    }

    impl MetricsProbe {
        pub fn new() -> Self {
            let exporter = InMemoryMetricExporter::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build();
            // The **same** scope constant `build_default_adapter` uses, not a
            // second literal: one constant with two readers is what makes a
            // change to it visible here rather than only in production.
            let scope = opentelemetry::InstrumentationScope::builder(super::SCOPE).build();
            let adapter = Arc::new(QaInsightsMetricsMeter::new(
                &provider.meter_with_scope(scope),
            ));
            Self {
                provider,
                exporter,
                adapter,
            }
        }

        /// The adapter to hand a service under test.
        pub fn adapter(&self) -> Arc<QaInsightsMetricsMeter> {
            Arc::clone(&self.adapter)
        }

        /// Flush the reader and read back everything exported so far.
        pub fn collect(&self) -> Series {
            self.provider
                .force_flush()
                .expect("the in-memory reader must flush");
            Series {
                metrics: self
                    .exporter
                    .get_finished_metrics()
                    .expect("the in-memory exporter must hand back what it was given"),
            }
        }
    }

    /// Which data points a reader below counts.
    ///
    /// A three-valued choice rather than an `Option<&[..]>` because the third
    /// value is not expressible as a filter: [`Self::Exactly`] is a statement
    /// about the labels a data point does **not** carry, and no subset
    /// predicate can make it. See `Series::counter_with_exactly`.
    #[derive(Clone, Copy)]
    enum LabelMatch<'a> {
        /// Every data point, whatever it carries.
        Any,
        /// Data points carrying at least every pair given. Vacuously true of
        /// every point when the slice is empty.
        AtLeast(&'a [(&'a str, &'a str)]),
        /// Data points whose attribute set is exactly the pairs given, and
        /// nothing else. An empty slice therefore means "unlabelled".
        Exactly(&'a [(&'a str, &'a str)]),
    }

    impl LabelMatch<'_> {
        fn admits<'a>(self, attributes: impl Iterator<Item = &'a opentelemetry::KeyValue>) -> bool {
            let wanted = match self {
                Self::Any => return true,
                Self::AtLeast(wanted) | Self::Exactly(wanted) => wanted,
            };
            let held: Vec<(&str, String)> = attributes
                .map(|kv| (kv.key.as_str(), kv.value.as_str().into_owned()))
                .collect();
            if matches!(self, Self::Exactly(_)) && held.len() != wanted.len() {
                return false;
            }
            wanted
                .iter()
                .all(|(key, value)| held.iter().any(|(k, v)| k == key && v == value))
        }
    }

    /// What one flush produced, with the questions the tests ask of it.
    pub struct Series {
        metrics: Vec<ResourceMetrics>,
    }

    impl Series {
        /// Total of a `u64` counter family across every label combination.
        pub fn counter(&self, name: &str) -> u64 {
            self.sum_counter(name, LabelMatch::Any)
        }

        /// Total of a `u64` counter family restricted to data points carrying
        /// **at least** every one of `labels`.
        pub fn counter_with(&self, name: &str, labels: &[(&str, &str)]) -> u64 {
            self.sum_counter(name, LabelMatch::AtLeast(labels))
        }

        /// Total of a `u64` counter family restricted to data points whose
        /// attribute set is **exactly** `labels` — no extra key.
        ///
        /// The reason this exists beside [`Self::counter_with`] is the empty
        /// slice. `AtLeast(&[])` is vacuously true of every data point, so
        /// `counter_with(name, &[])` is not a claim about labels at all — it
        /// returns the same number [`Self::counter`] does, and would keep
        /// returning it the day the family started attaching a tenant id. An
        /// unlabelled family has to be pinned with this one.
        pub fn counter_with_exactly(&self, name: &str, labels: &[(&str, &str)]) -> u64 {
            self.sum_counter(name, LabelMatch::Exactly(labels))
        }

        fn sum_counter(&self, name: &str, labels: LabelMatch<'_>) -> u64 {
            let mut total = 0;
            for metric in self.named(name) {
                if let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() {
                    for point in sum.data_points() {
                        if labels.admits(point.attributes()) {
                            total += point.value();
                        }
                    }
                }
            }
            total
        }

        /// How many observations a histogram family recorded, across every
        /// label combination.
        pub fn histogram_count(&self, name: &str) -> u64 {
            self.sum_histogram(name, LabelMatch::Any)
        }

        /// How many observations a histogram family recorded on the data points
        /// carrying **at least** every one of `labels`.
        ///
        /// This is the per-label-value rate, and without it a histogram's labels
        /// are unpinned: `record(value, &[])` beside a correctly labelled
        /// `add` passes every count-only assertion, and every
        /// `histogram_quantile(… by (outcome))` a dashboard writes then
        /// collapses to one merged series. qa-catalog's own adapter tests name
        /// that defect; this is the same reader, back-ported.
        pub fn histogram_count_with(&self, name: &str, labels: &[(&str, &str)]) -> u64 {
            self.sum_histogram(name, LabelMatch::AtLeast(labels))
        }

        fn sum_histogram(&self, name: &str, labels: LabelMatch<'_>) -> u64 {
            let mut total = 0;
            for metric in self.named(name) {
                if let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data() {
                    for point in histogram.data_points() {
                        if labels.admits(point.attributes()) {
                            total += point.count();
                        }
                    }
                }
            }
            total
        }

        /// The count in the bucket a given value falls into, for one histogram
        /// family, summed across label combinations.
        ///
        /// `None` when the family exported no histogram at all, which is a
        /// different failure from "the value landed in a bucket with zero
        /// observations" and must not be confused with it.
        pub fn histogram_bucket_of(&self, name: &str, value: f64) -> Option<u64> {
            let mut found = None;
            for metric in self.named(name) {
                if let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data() {
                    for point in histogram.data_points() {
                        // The bucket index is the first boundary the value does
                        // not exceed; a value above every boundary lands in the
                        // implicit overflow bucket, which is the last count.
                        let index = point
                            .bounds()
                            .position(|bound| value <= bound)
                            .unwrap_or_else(|| point.bounds().count());
                        let count = point.bucket_counts().nth(index).unwrap_or(0);
                        found = Some(found.unwrap_or(0) + count);
                    }
                }
            }
            found
        }

        /// The bucket boundaries a histogram family was **built with**, as the
        /// exporter reports them.
        ///
        /// Read directly rather than inferred from where a value landed:
        /// `histogram_bucket_of` computes its index from these same bounds, so
        /// it agrees with whatever set is in force and can never tell one set
        /// from another. This is what makes the declared boundaries assertable.
        pub fn histogram_bounds(&self, name: &str) -> Option<Vec<f64>> {
            for metric in self.named(name) {
                if let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data()
                    && let Some(point) = histogram.data_points().next()
                {
                    return Some(point.bounds().collect());
                }
            }
            None
        }

        /// Every instrumentation scope name this flush carried.
        pub fn scopes(&self) -> Vec<&str> {
            self.metrics
                .iter()
                .flat_map(ResourceMetrics::scope_metrics)
                .map(|scope| scope.scope().name())
                .collect()
        }

        /// Every exported metric under `name`, across scopes.
        fn named<'a>(
            &'a self,
            name: &'a str,
        ) -> impl Iterator<Item = &'a opentelemetry_sdk::metrics::data::Metric> {
            self.metrics
                .iter()
                .flat_map(ResourceMetrics::scope_metrics)
                .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
                .filter(move |metric| metric.name() == name)
        }

        /// Every metric name this flush carried — for the test that asserts a
        /// family is exported under exactly the catalog's name.
        pub fn names(&self) -> Vec<&str> {
            self.metrics
                .iter()
                .flat_map(ResourceMetrics::scope_metrics)
                .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
                .map(opentelemetry_sdk::metrics::data::Metric::name)
                .collect()
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metrics_tests.rs"]
mod tests;
