//! `OpenTelemetry`-backed adapter for the qa-runs metric ports.
//!
//! [`QaRunsMetricsMeter`] owns one instrument per family in
//! [`crate::domain::metrics`] and implements every trait in
//! [`crate::domain::ports::metrics`]. One `Arc<QaRunsMetricsMeter>` is built at
//! gear init by [`build_default_adapter`] and coerced into the per-trait
//! `Arc<dyn _>` views the two services hold.
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
//! AM's `AmMetricsMeter::new` takes `(meter, prefix)` and composes each name as
//! `format!("{prefix}_…")`, so its catalog constants equal the rendered name
//! only under the default prefix. This adapter takes the meter alone and reads
//! the names straight out of [`crate::domain::metrics`].
//!
//! The reason is the gate. `domain::metrics`'s naming tests
//! (`counter_names_carry_the_total_suffix_and_duration_names_carry_the_unit`,
//! `every_metric_is_namespaced_to_this_gear`) assert properties **of the
//! constants**, and they are only a gate on what a dashboard finds if the
//! constant is what the instrument is named. A prefix parameter puts a
//! `format!` between the two and re-opens exactly the defect those tests exist
//! to catch — a series exported under a name nobody queries, which from inside
//! this process looks identical to one that works. AM needs the parameter
//! because its transitional stringly-typed facade dispatches on the family
//! name; this gear has no facade (see [`crate::domain::metrics`]'s header) and
//! so has nothing to buy with it.
//!
//! Test isolation, the parameter's other use, is bought here by giving each
//! test its own [`opentelemetry_sdk::metrics::SdkMeterProvider`] rather than
//! sharing the process-global one — which AM's own tests do as well, making
//! their `TEST_PREFIX` redundant there too.
//!
//! # Emission cannot fail
//!
//! Every method is a `Counter::add` or a `Histogram::record` on an instrument
//! this struct already holds: no `?`, no `unwrap`, no lookup that could miss,
//! and no allocation beyond the attribute array. Nothing here can return an
//! error, and nothing here panics.
//!
//! That is only half of the guarantee, because the call sites take
//! `Arc<dyn DispatchMetrics>` / `Arc<dyn IngestMetrics>` and this is not the
//! only implementation they can be handed. The other half lives at the call
//! site: `domain::service`'s `emit` runs one emission so that a panicking
//! implementation cannot take the measured path with it. See its doc for why
//! the trait's written contract is not sufficient on its own.
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
    QA_RUNS_DISPATCH, QA_RUNS_DISPATCH_DECISION, QA_RUNS_DISPATCH_DURATION, QA_RUNS_INGEST,
    QA_RUNS_INGEST_DURATION, QA_RUNS_QUEUE_WAIT, QA_RUNS_QUEUE_WAIT_DURATION,
};
use crate::domain::ports::metrics::{
    DispatchDecision, DispatchMetrics, DispatchOutcome, IngestMetrics, IngestOutcome,
};

/// The `outcome` label key, shared by both counter families and both
/// histograms — one word, so a dashboard that pivots dispatch by outcome uses
/// the same expression on ingest.
const OUTCOME: &str = "outcome";

/// The `decision` label key on the admission-decision family. A second key
/// rather than a second value of [`OUTCOME`], because the two answer different
/// questions — see [`crate::domain::metrics::QA_RUNS_DISPATCH_DECISION`].
const DECISION: &str = "decision";

/// Explicit second boundaries for every duration histogram.
///
/// Declared rather than left to the SDK default, and this is load-bearing. The
/// `OTel` defaults are `[0, 5, 10, 25, 50, … 10000]`, which are milliseconds in
/// all but name. Read as **seconds** their first interval above zero is
/// `(0, 5]`, and that single bucket swallows every healthy observation this
/// gear makes: an ordinary ingest pass is one scoped read and one write, and a
/// dispatcher cycle that finds nothing to drain is a handful of scoped queries.
/// A p95 over those is the bucket's upper edge rather than an estimate of
/// anything.
///
/// It swallows the interesting part of the third family too. Residency values
/// run from seconds to minutes, so the defaults do resolve their tail — but
/// `cpt-cf-qa-nfr-dispatch-latency`'s own design argument puts a healthy
/// dispatch below 5 s (a 5 s sweep, p95 ≈ 4.75 s), and *that* is exactly the
/// range the defaults cannot see inside. A quantile query is the entire reason
/// these families exist, so the boundaries are part of the metric's definition
/// rather than a tuning knob.
///
/// **What is not claimed: that a dispatcher cycle is short.** A cycle contains
/// its drain, and the drain dispatches each claimed row inline, so a tick can
/// run for as long as the force-syncs and bundle builds inside it —
/// `DispatchService::run_tick`'s own doc says a tenant with twenty
/// slow-building runs holds one for twenty of them. The upper boundaries here
/// serve that case; the lower ones serve the healthy one.
///
/// The low end goes down to 5 ms because the ingest path's common case is a
/// single-row upsert.
///
/// **The top boundary is 60 s, and that is a stated limit rather than a claim
/// of adequacy.** Two of the three families can legitimately exceed it — a
/// residency behind a long-running predecessor, and a cycle whose drain is
/// building bundles — and both land in the overflow bucket, where a quantile is
/// again an edge rather than an estimate. The set is sized for the range in
/// which these paths are *healthy*, which is the range an alert is written
/// against; past a minute the counter beside each histogram and the trend are
/// what an operator reads. Extending the high end is a change to make with real
/// residency and cycle data in hand rather than by guessing at one now.
///
/// **10.0 and 5.0 are boundaries because the two NFR thresholds are those
/// numbers**, so an alert can be written against a bucket edge rather than
/// interpolated across one. That is a convenience of alert *arithmetic* and
/// nothing more — whether either series actually measures its NFR is settled in
/// [`QA_RUNS_QUEUE_WAIT_DURATION`]'s and [`QA_RUNS_DISPATCH_DURATION`]'s own
/// docs, and for the dispatch duration the answer is no.
const DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];

/// The instrumentation scope this gear's instruments are reported under.
const SCOPE: &str = "qa-runs";

/// Owns one `OpenTelemetry` instrument per qa-runs metric family and implements
/// every port trait over them.
///
/// **The instruments are built once, in [`Self::new`], and held.** Looking one
/// up per emission would put a map probe and a string hash on the two paths
/// this gear measures — measuring a path must not change it, and the ingest
/// path runs per log line.
pub struct QaRunsMetricsMeter {
    dispatch: Counter<u64>,
    dispatch_decision: Counter<u64>,
    dispatch_duration: Histogram<f64>,
    queue_wait: Counter<u64>,
    queue_wait_duration: Histogram<f64>,
    ingest: Counter<u64>,
    ingest_duration: Histogram<f64>,
}

impl QaRunsMetricsMeter {
    /// Build every instrument this gear exports.
    ///
    /// Names come from [`crate::domain::metrics`] verbatim and carry no
    /// `.with_unit()` hint; see this module's header for both reasons.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            dispatch: meter
                .u64_counter(QA_RUNS_DISPATCH)
                .with_description(
                    "Dispatcher ticks, by how the tick ended (started / refused / failed)",
                )
                .build(),
            dispatch_decision: meter
                .u64_counter(QA_RUNS_DISPATCH_DECISION)
                .with_description(
                    "Launch admission decisions, by what admission decided \
                     (inline / queued / unqueued)",
                )
                .build(),
            dispatch_duration: meter
                .f64_histogram(QA_RUNS_DISPATCH_DURATION)
                .with_description("Wall-clock duration of one dispatcher tick, in seconds")
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
            queue_wait: meter
                .u64_counter(QA_RUNS_QUEUE_WAIT)
                .with_description("Queued runs taken from the queue to an execution request")
                .build(),
            queue_wait_duration: meter
                .f64_histogram(QA_RUNS_QUEUE_WAIT_DURATION)
                .with_description(
                    "Seconds a queued run waited between being enqueued and its \
                     execution being requested; an upper bound on the dispatch-latency \
                     NFR, not the NFR itself",
                )
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
            ingest: meter
                .u64_counter(QA_RUNS_INGEST)
                .with_description(
                    "Ingest passes, by how the observation landed \
                     (applied / completed / duplicate / refused / failed)",
                )
                .build(),
            ingest_duration: meter
                .f64_histogram(QA_RUNS_INGEST_DURATION)
                .with_description("Wall-clock duration of one ingest pass, in seconds")
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
        }
    }
}

impl DispatchMetrics for QaRunsMetricsMeter {
    fn dispatch_pass(&self, outcome: DispatchOutcome, duration: Duration) {
        let labels = [KeyValue::new(OUTCOME, outcome.as_str())];
        self.dispatch.add(1, &labels);
        self.dispatch_duration
            .record(duration.as_secs_f64(), &labels);
    }

    fn dispatch_decision(&self, decision: DispatchDecision) {
        self.dispatch_decision
            .add(1, &[KeyValue::new(DECISION, decision.as_str())]);
    }

    fn queue_wait(&self, waited: Duration) {
        // No attributes at all: the family is one series, and every dimension
        // that would distinguish two waits — the platform, the tenant, the run
        // — is per-run cardinality. Exclusivity was considered and left out for
        // the reason `DispatchDecision`'s doc gives about widening a family's
        // fixed label set after dashboards key on it.
        self.queue_wait.add(1, &[]);
        self.queue_wait_duration.record(waited.as_secs_f64(), &[]);
    }
}

impl IngestMetrics for QaRunsMetricsMeter {
    fn ingest_batch(&self, outcome: IngestOutcome, duration: Duration) {
        let labels = [KeyValue::new(OUTCOME, outcome.as_str())];
        self.ingest.add(1, &labels);
        self.ingest_duration.record(duration.as_secs_f64(), &labels);
    }
}

/// Build the adapter against the process-global `OpenTelemetry` meter provider.
///
/// The one call site is `gear.rs`'s init. Safe to call whether or not a
/// pipeline was ever configured: with no provider installed the global one is
/// the built-in no-op, so every instrument this returns is a no-op and every
/// emission through it is silent. See this module's header.
#[must_use]
pub fn build_default_adapter() -> Arc<QaRunsMetricsMeter> {
    let scope = opentelemetry::InstrumentationScope::builder(SCOPE).build();
    let meter = opentelemetry::global::meter_with_scope(scope);
    Arc::new(QaRunsMetricsMeter::new(&meter))
}

/// A private meter provider and the series it collected, for tests.
///
/// Lives here, beside the adapter, rather than in a test file, because three
/// modules read it: this module's own tests, `domain::service::dispatch`'s and
/// `domain::service::ingest`'s. The two call-site suites are the ones that
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

    use super::QaRunsMetricsMeter;

    /// A meter provider private to one test, its in-memory exporter, and the
    /// adapter built over it.
    ///
    /// Private rather than global: two tests running in the same process share
    /// the global provider, and a counter is cumulative, so a shared one makes
    /// every `assert_eq!(counter, 1)` order-dependent.
    pub struct MetricsProbe {
        provider: SdkMeterProvider,
        exporter: InMemoryMetricExporter,
        adapter: Arc<QaRunsMetricsMeter>,
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
            let adapter = Arc::new(QaRunsMetricsMeter::new(&provider.meter_with_scope(scope)));
            Self {
                provider,
                exporter,
                adapter,
            }
        }

        /// The adapter to hand a service under test.
        pub fn adapter(&self) -> Arc<QaRunsMetricsMeter> {
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

    /// What one flush produced, with the three questions the tests ask of it.
    pub struct Series {
        metrics: Vec<ResourceMetrics>,
    }

    impl Series {
        /// Total of a `u64` counter family across every label combination.
        pub fn counter(&self, name: &str) -> u64 {
            self.sum_counter(name, None)
        }

        /// Total of a `u64` counter family restricted to data points carrying
        /// every one of `labels`.
        pub fn counter_with(&self, name: &str, labels: &[(&str, &str)]) -> u64 {
            self.sum_counter(name, Some(labels))
        }

        fn sum_counter(&self, name: &str, labels: Option<&[(&str, &str)]>) -> u64 {
            let mut total = 0;
            for metric in self.named(name) {
                if let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() {
                    for point in sum.data_points() {
                        let matches = labels.is_none_or(|wanted| {
                            wanted.iter().all(|(key, value)| {
                                point.attributes().any(|kv| {
                                    kv.key.as_str() == *key && kv.value.as_str() == *value
                                })
                            })
                        });
                        if matches {
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
            let mut total = 0;
            for metric in self.named(name) {
                if let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data() {
                    for point in histogram.data_points() {
                        total += point.count();
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
