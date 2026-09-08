//! `OpenTelemetry`-backed adapter for the qa-catalog metric port.
//!
//! [`QaCatalogMetricsMeter`] owns one instrument per family in
//! [`crate::domain::metrics`] and implements
//! [`crate::domain::ports::metrics::PluginResolutionMetrics`]. One
//! `Arc<QaCatalogMetricsMeter>` is built at gear init by
//! [`build_default_adapter`] and coerced into the `Arc<dyn _>` view
//! `QaProductRegistry` holds.
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
//! of [`crate::domain::metrics`], following qa-runs', qa-insights' and
//! qa-environments' answer to the same question.
//!
//! The reason is the gate. `domain::metrics`' naming tests assert properties
//! **of the constants**, and they are only a gate on what a dashboard finds if
//! the constant is what the instrument is named. A prefix parameter puts a
//! `format!` between the two and re-opens exactly the defect those tests exist
//! to catch — a series exported under a name nobody queries, which from inside
//! this process looks identical to one that works.
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
//! That is only half of the guarantee, because the call site takes
//! `Arc<dyn PluginResolutionMetrics>` and this is not the only implementation
//! it can be handed. The other half lives at the call site:
//! `domain::service`'s `emit` runs one emission so that a panicking
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

use crate::domain::metrics::{QA_CATALOG_PLUGIN_RESOLUTION, QA_CATALOG_PLUGIN_RESOLUTION_DURATION};
use crate::domain::ports::metrics::{PluginResolutionMetrics, PluginResolutionOutcome};

/// The `outcome` label key, carried by both families so that a rate and a
/// quantile are two queries over one partition.
const OUTCOME: &str = "outcome";

/// Explicit second boundaries for the duration histogram.
///
/// Declared rather than left to the SDK default, and this is load-bearing. The
/// `OTel` defaults are `[0, 5, 10, 25, 50, … 10000]`, which are milliseconds in
/// all but name. Read as **seconds**, their first interval above zero is
/// `(0, 5]`, and every resolution this gear will ever perform lands in it — a
/// p95 read off that bucket is its upper edge rather than an estimate of
/// anything. A quantile query is the entire reason the duration family exists,
/// so the boundaries are part of the metric's definition rather than a tuning
/// knob.
///
/// **This set is two orders of magnitude lower than qa-environments', and the
/// deviation is the point.** That gear's boundaries run to five minutes because
/// its families measure round trips to customer clusters and whole cycles over
/// a hundred of them. A resolution is a policy decision plus one indexed row
/// read plus a hash probe, none of which crosses a customer network, so a
/// healthy sample is sub-millisecond to a few milliseconds and the interesting
/// question — *is the PDP or the database making resolution expensive* — is
/// asked in that range. Buckets sized for the other gear would answer it with
/// one bucket.
///
/// **No boundary here is an NFR threshold.** The qa-platform design states no
/// latency bound over plugin resolution — the one NFR that reaches this gear at
/// all is a capacity envelope allocated to qa-runs and qa-insights — so there
/// was no number to place and none was invented.
///
/// **The 10 s ceiling is a stated limit, not a claim of adequacy.** A
/// resolution that outruns it is a PDP or a database that has stopped
/// answering, and it lands in the overflow bucket where a quantile is again an
/// edge; past that point the counter beside the histogram is what an operator
/// reads. Extending the high end is a change to make with real data rather than
/// by guessing at one now.
const DURATION_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// The instrumentation scope this gear's instruments are reported under.
const SCOPE: &str = "qa-catalog";

/// Owns one `OpenTelemetry` instrument per qa-catalog metric family and
/// implements the port trait over them.
///
/// **The instruments are built once, in [`Self::new`], and held.** Looking one
/// up per emission would put a map probe and a string hash on the measured
/// path, and this path runs once per environment per observation cycle.
pub struct QaCatalogMetricsMeter {
    resolution: Counter<u64>,
    resolution_duration: Histogram<f64>,
}

impl QaCatalogMetricsMeter {
    /// Build every instrument this gear exports.
    ///
    /// Names come from [`crate::domain::metrics`] verbatim and carry no
    /// `.with_unit()` hint; see this module's header for both reasons.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            resolution: meter
                .u64_counter(QA_CATALOG_PLUGIN_RESOLUTION)
                .with_description(
                    "Product-plugin resolutions, by how the resolution ended \
                     (resolved / unregistered / refused / failed)",
                )
                .build(),
            resolution_duration: meter
                .f64_histogram(QA_CATALOG_PLUGIN_RESOLUTION_DURATION)
                .with_description(
                    "Wall-clock duration of one product-plugin resolution, in seconds: \
                     the policy check, the product read and the ClientHub probe",
                )
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
        }
    }
}

impl PluginResolutionMetrics for QaCatalogMetricsMeter {
    fn plugin_resolution(&self, outcome: PluginResolutionOutcome, duration: Duration) {
        let labels = [KeyValue::new(OUTCOME, outcome.as_str())];
        self.resolution.add(1, &labels);
        self.resolution_duration
            .record(duration.as_secs_f64(), &labels);
    }
}

/// Build the adapter against the process-global `OpenTelemetry` meter provider.
///
/// The one call site is `gear.rs`'s init. Safe to call whether or not a
/// pipeline was ever configured: with no provider installed the global one is
/// the built-in no-op, so every instrument this returns is a no-op and every
/// emission through it is silent. See this module's header.
#[must_use]
pub fn build_default_adapter() -> Arc<QaCatalogMetricsMeter> {
    let scope = opentelemetry::InstrumentationScope::builder(SCOPE).build();
    let meter = opentelemetry::global::meter_with_scope(scope);
    Arc::new(QaCatalogMetricsMeter::new(&meter))
}

/// A private meter provider and the series it collected, for tests.
///
/// Lives here, beside the adapter, rather than in a test file, because two
/// modules read it: this module's own tests and
/// `domain::service::plugin_registry`'s. The call-site suite is the one that
/// matters most — what needs proving there is that the **rendered series** is
/// what a dashboard query will find, and a mock of the port would prove only
/// that the call site calls the port.
///
/// Copied from qa-environments' `infra::metrics::probe`, which copied it from
/// qa-insights'.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::expect_used, reason = "test-only helper")]
pub mod probe {
    use std::sync::Arc;

    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    use super::QaCatalogMetricsMeter;

    /// A meter provider private to one test, its in-memory exporter, and the
    /// adapter built over it.
    ///
    /// Private rather than global: two tests running in the same process share
    /// the global provider, and a counter is cumulative, so a shared one makes
    /// every `assert_eq!(counter, 1)` order-dependent.
    pub struct MetricsProbe {
        provider: SdkMeterProvider,
        exporter: InMemoryMetricExporter,
        adapter: Arc<QaCatalogMetricsMeter>,
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
            let adapter = Arc::new(QaCatalogMetricsMeter::new(
                &provider.meter_with_scope(scope),
            ));
            Self {
                provider,
                exporter,
                adapter,
            }
        }

        /// The adapter to hand a service under test.
        pub fn adapter(&self) -> Arc<QaCatalogMetricsMeter> {
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

    /// What one flush produced, with the questions the tests ask of it.
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
                        if Self::matches(point.attributes(), labels) {
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
            self.sum_histogram(name, None)
        }

        /// How many observations a histogram family recorded on the data points
        /// carrying every one of `labels`.
        ///
        /// This is the per-class rate: a Prometheus histogram exports a
        /// `_count` per label set, which is why the per-environment family
        /// needs no counter of its own.
        pub fn histogram_count_with(&self, name: &str, labels: &[(&str, &str)]) -> u64 {
            self.sum_histogram(name, Some(labels))
        }

        fn sum_histogram(&self, name: &str, labels: Option<&[(&str, &str)]>) -> u64 {
            let mut total = 0;
            for metric in self.named(name) {
                if let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data() {
                    for point in histogram.data_points() {
                        if Self::matches(point.attributes(), labels) {
                            total += point.count();
                        }
                    }
                }
            }
            total
        }

        fn matches<'a>(
            attributes: impl Iterator<Item = &'a opentelemetry::KeyValue>,
            labels: Option<&[(&str, &str)]>,
        ) -> bool {
            let Some(wanted) = labels else {
                return true;
            };
            let held: Vec<(&str, String)> = attributes
                .map(|kv| (kv.key.as_str(), kv.value.as_str().into_owned()))
                .collect();
            wanted
                .iter()
                .all(|(key, value)| held.iter().any(|(k, v)| k == key && v == value))
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
