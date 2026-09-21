//! Pull-based metrics delivery: a Prometheus text-format scrape endpoint.
//!
//! # Why this exists
//!
//! Until this module, every metric any gear in this workspace recorded left the
//! process one way only — [`init_metrics_provider`] built an
//! `opentelemetry_otlp::MetricExporter` behind a periodic reader and *pushed*.
//! With `metrics.enabled: false`, which is the default every chart in the
//! repository ships, the global meter provider stays the built-in no-op and
//! every instrument is silent. So a default deployment recorded metrics that
//! were reachable by nothing at all: no route served them and no collector
//! received them.
//!
//! Push and pull are two readers on one meter provider, not two settings of
//! one. `metrics.enabled` still means OTLP push and still defaults off —
//! turning it on without a reachable collector logs an export failure every
//! interval forever, which is why it is not a thing to enable by default.
//! `metrics.scrape.enabled` has no such cost: it opens a socket, answers when
//! asked, and depends on nothing outside the pod.
//!
//! # The listener is deliberately its own
//!
//! The endpoint does **not** join the gear host's main HTTP router. That is a
//! security decision, not an aesthetic one: the main listener is what an
//! ingress routes to, and every route on it is either `.authenticated()` or
//! deliberately `.public()`. A scrape endpoint is neither — it is an
//! unauthenticated read of operational state that belongs to the cluster, not
//! to a tenant. Giving it a separate port keeps it off the ingress by
//! construction rather than by remembering to exclude it, and makes "who can
//! reach this" a `NetworkPolicy`/`Service` question with a visible answer.
//!
//! What the endpoint exposes is still worth knowing: instrument names, label
//! values and counts. Label values in this workspace are bounded enumerations
//! (outcome, reason, vendor) rather than tenant identifiers, but that is a
//! property of the instruments, not of this module — keep it cluster-internal.
//!
//! # Format
//!
//! Prometheus text exposition format 0.0.4. Instrument names are emitted
//! **verbatim** (modulo character sanitisation); no `_total` suffix is added
//! and no unit suffix is derived. The names a gear declares in its
//! `domain::metrics` are the names an operator queries, and a renderer that
//! silently rewrote them would make every hand-written dashboard query wrong.
//!
//! Sums render as `counter` when monotonic and `gauge` otherwise; gauges as
//! `gauge`; explicit-bucket histograms as `histogram` with cumulative
//! `_bucket`/`_sum`/`_count` series. Exponential histograms have no
//! representation in this format and are skipped — nothing in this workspace
//! configures a view that produces one.
//!
//! [`init_metrics_provider`]: super::init::init_metrics_provider

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::reader::MetricReader;
use opentelemetry_sdk::metrics::{InstrumentKind, ManualReader, Pipeline, Temporality};

use super::config::MetricsScrapeConfig;

/// The content type Prometheus expects from a text-format exposition.
const TEXT_FORMAT: &str = "text/plain; version=0.0.4; charset=utf-8";

/// The process-wide manual reader. Created on first use by [`reader`], which
/// `init_metrics_provider` calls while building the meter provider; read back
/// by [`render`] on every scrape.
static SCRAPE_READER: OnceLock<Arc<ManualReader>> = OnceLock::new();

/// A cloneable handle to the process-wide manual reader.
///
/// [`opentelemetry_sdk::metrics::MeterProviderBuilder::with_reader`] takes its
/// reader by value, so the reader itself cannot be kept alongside the provider.
/// This newtype delegates the whole [`MetricReader`] contract to a shared
/// `Arc`, letting [`render`] collect from the same reader the provider feeds.
#[derive(Debug, Clone)]
pub struct ScrapeReader(Arc<ManualReader>);

impl MetricReader for ScrapeReader {
    fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
        self.0.register_pipeline(pipeline);
    }

    fn collect(&self, rm: &mut ResourceMetrics) -> OTelSdkResult {
        self.0.collect(rm)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    fn temporality(&self, kind: InstrumentKind) -> Temporality {
        self.0.temporality(kind)
    }
}

/// Hand out the process-wide scrape reader, creating it on first call.
///
/// Cumulative temporality is explicit rather than left to the SDK default: a
/// Prometheus counter is cumulative by definition, and a delta reader behind
/// this renderer would produce counters that appear to reset on every scrape.
#[must_use]
pub fn reader() -> ScrapeReader {
    ScrapeReader(Arc::clone(SCRAPE_READER.get_or_init(|| {
        Arc::new(
            ManualReader::builder()
                .with_temporality(Temporality::Cumulative)
                .build(),
        )
    })))
}

/// Collect from the scrape reader and render the Prometheus text exposition.
///
/// Returns `None` when no scrape reader was ever attached to a meter provider
/// (metrics scraping is off, or the provider failed to build) or when the
/// reader refuses to collect because it has been shut down. Both are reported
/// to the caller as a 503 rather than an empty 200, so a scraper sees the
/// difference between "no pipeline" and "a pipeline with nothing in it yet".
#[must_use]
pub fn render() -> Option<String> {
    let reader = SCRAPE_READER.get()?;
    let mut rm = ResourceMetrics::default();
    if let Err(err) = reader.collect(&mut rm) {
        tracing::warn!(error = %err, "metrics scrape: collection failed");
        return None;
    }
    Some(encode(&rm))
}

/// Start the scrape listener on the current Tokio runtime.
///
/// A bind failure is logged and abandoned, never propagated: a process that
/// serves its traffic is more useful than one that refuses to start because
/// port 9464 was taken, and the failure is loud in the log either way.
pub fn spawn(cfg: &MetricsScrapeConfig) {
    if !cfg.enabled {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::error!(
            "metrics scrape: no Tokio runtime at init; the endpoint will not be served"
        );
        return;
    };
    if !cfg.path.starts_with('/') {
        tracing::error!(
            path = %cfg.path,
            "metrics scrape: path must start with '/'; the endpoint will not be served"
        );
        return;
    }
    let bind_addr = cfg.bind_addr.clone();
    let path = cfg.path.clone();
    handle.spawn(async move {
        let app = axum::Router::new().route(&path, axum::routing::get(handler));
        match tokio::net::TcpListener::bind(&bind_addr).await {
            Ok(listener) => {
                tracing::info!(
                    bind_addr = %bind_addr,
                    path = %path,
                    "metrics scrape endpoint listening"
                );
                if let Err(err) = axum::serve(listener, app).await {
                    tracing::error!(error = %err, "metrics scrape endpoint stopped");
                }
            }
            Err(err) => tracing::error!(
                error = %err,
                bind_addr = %bind_addr,
                "metrics scrape: bind failed; the endpoint will not be served"
            ),
        }
    });
}

#[allow(clippy::unused_async)]
async fn handler() -> axum::response::Response {
    use axum::response::IntoResponse as _;
    match render() {
        Some(body) => (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, TEXT_FORMAT)],
            body,
        )
            .into_response(),
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            [(axum::http::header::CONTENT_TYPE, TEXT_FORMAT)],
            "# no metrics pipeline is attached to this process\n",
        )
            .into_response(),
    }
}

// ===== rendering ============================================================

/// One exposition family: the `# HELP`/`# TYPE` pair and its samples.
struct Family {
    kind: &'static str,
    help: String,
    body: String,
}

/// Render a collected [`ResourceMetrics`] as text exposition.
///
/// Samples are grouped by family name across instrumentation scopes. Two gears
/// declaring the same instrument name would otherwise produce two `# TYPE`
/// lines for one family, which Prometheus rejects outright — the whole scrape,
/// not just the duplicate.
fn encode(rm: &ResourceMetrics) -> String {
    let mut families: BTreeMap<String, Family> = BTreeMap::new();

    for scope in rm.scope_metrics() {
        for metric in scope.metrics() {
            let name = sanitize_name(metric.name());
            let mut body = String::new();
            let Some(kind) = render_data(&name, metric.data(), &mut body) else {
                continue;
            };
            let entry = families.entry(name).or_insert_with(|| Family {
                kind,
                help: escape_help(metric.description()),
                body: String::new(),
            });
            if entry.kind == kind {
                entry.body.push_str(&body);
            } else {
                tracing::warn!(
                    metric = metric.name(),
                    "metrics scrape: two instruments share a name with different types; \
                     the later one is dropped"
                );
            }
        }
    }

    let mut out = String::new();
    for (name, family) in &families {
        let Family { kind, help, body } = family;
        _ = writeln!(out, "# HELP {name} {help}");
        _ = writeln!(out, "# TYPE {name} {kind}");
        out.push_str(body);
    }
    out
}

/// Dispatch on the value type, then on the aggregation.
fn render_data(name: &str, data: &AggregatedMetrics, out: &mut String) -> Option<&'static str> {
    match data {
        AggregatedMetrics::F64(d) => render_aggregation(name, d, out),
        AggregatedMetrics::U64(d) => render_aggregation(name, d, out),
        AggregatedMetrics::I64(d) => render_aggregation(name, d, out),
    }
}

/// Write every sample of one aggregation, returning its exposition type.
///
/// `None` means "no representation in this format" — exponential histograms,
/// which nothing in this workspace configures.
fn render_aggregation<T: PromValue>(
    name: &str,
    data: &MetricData<T>,
    out: &mut String,
) -> Option<&'static str> {
    match data {
        MetricData::Gauge(gauge) => {
            for dp in gauge.data_points() {
                write_sample(out, name, &format_labels(dp.attributes(), None), dp.value());
            }
            Some("gauge")
        }
        MetricData::Sum(sum) => {
            for dp in sum.data_points() {
                write_sample(out, name, &format_labels(dp.attributes(), None), dp.value());
            }
            Some(if sum.is_monotonic() {
                "counter"
            } else {
                "gauge"
            })
        }
        MetricData::Histogram(hist) => {
            for dp in hist.data_points() {
                let bounds: Vec<f64> = dp.bounds().collect();
                let mut cumulative: u64 = 0;
                for (idx, count) in dp.bucket_counts().enumerate() {
                    cumulative = cumulative.saturating_add(count);
                    let le = bounds
                        .get(idx)
                        .map_or_else(|| "+Inf".to_owned(), |b| fmt_f64(*b));
                    let labels = format_labels(dp.attributes(), Some(("le", &le)));
                    let bucket = format!("{name}_bucket");
                    write_sample(out, &bucket, &labels, cumulative);
                }
                let labels = format_labels(dp.attributes(), None);
                write_sample(out, &format!("{name}_sum"), &labels, dp.sum());
                write_sample(out, &format!("{name}_count"), &labels, dp.count());
            }
            Some("histogram")
        }
        MetricData::ExponentialHistogram(_) => None,
    }
}

fn write_sample<T: PromValue>(out: &mut String, name: &str, labels: &str, value: T) {
    let value = value.prom();
    _ = writeln!(out, "{name}{labels} {value}");
}

/// Render an attribute set as an exposition label block, with an optional
/// synthetic label appended (`le` for histogram buckets).
fn format_labels<'a>(
    attrs: impl Iterator<Item = &'a KeyValue>,
    extra: Option<(&str, &str)>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for kv in attrs {
        let key = sanitize_label(kv.key.as_str());
        let value = escape_label_value(&kv.value.to_string());
        parts.push(format!("{key}=\"{value}\""));
    }
    if let Some((key, value)) = extra {
        let value = escape_label_value(value);
        parts.push(format!("{key}=\"{value}\""));
    }
    if parts.is_empty() {
        return String::new();
    }
    let joined = parts.join(",");
    format!("{{{joined}}}")
}

/// A metric value that can be written as an exposition sample.
///
/// Exists so histograms — whose counts are always `u64` while their sums follow
/// the instrument's value type — render through one code path without casting
/// integers to `f64` and losing precision above 2^53.
trait PromValue: Copy {
    fn prom(self) -> String;
}

impl PromValue for f64 {
    fn prom(self) -> String {
        fmt_f64(self)
    }
}

impl PromValue for u64 {
    fn prom(self) -> String {
        self.to_string()
    }
}

impl PromValue for i64 {
    fn prom(self) -> String {
        self.to_string()
    }
}

/// Prometheus spells the non-finite floats differently from Rust's `Display`.
fn fmt_f64(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_positive() {
            "+Inf"
        } else {
            "-Inf"
        }
        .to_owned();
    }
    format!("{value}")
}

/// Metric names may hold `[a-zA-Z0-9_:]`; everything else becomes `_`.
fn sanitize_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == ':' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Label names are like metric names but without `:`. OpenTelemetry attribute
/// keys are dotted by convention (`http.route`), so this fires routinely.
fn sanitize_label(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn escape_label_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// `# HELP` runs to end of line, so only backslash and newline need escaping.
fn escape_help(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        encode, escape_help, escape_label_value, fmt_f64, format_labels, reader, sanitize_label,
        sanitize_name,
    };
    use opentelemetry::KeyValue;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::reader::MetricReader as _;

    #[test]
    fn dotted_attribute_keys_become_underscores() {
        assert_eq!(sanitize_label("http.route"), "http_route");
        assert_eq!(sanitize_label("9lives"), "_9lives");
    }

    #[test]
    fn instrument_names_are_kept_verbatim() {
        // The whole point: `qa_runs_dispatch_total` must not become
        // `qa_runs_dispatch_total_total` or lose its suffix. Every dashboard
        // query an operator writes is against the name the gear declares.
        assert_eq!(
            sanitize_name("qa_runs_dispatch_total"),
            "qa_runs_dispatch_total"
        );
        assert_eq!(
            sanitize_name("qa_runs_dispatch_duration_seconds"),
            "qa_runs_dispatch_duration_seconds"
        );
        assert_eq!(sanitize_name("a.b-c"), "a_b_c");
    }

    #[test]
    fn non_finite_floats_use_prometheus_spelling() {
        assert_eq!(fmt_f64(f64::INFINITY), "+Inf");
        assert_eq!(fmt_f64(f64::NEG_INFINITY), "-Inf");
        assert_eq!(fmt_f64(f64::NAN), "NaN");
        assert_eq!(fmt_f64(0.5), "0.5");
    }

    #[test]
    fn label_values_and_help_text_are_escaped() {
        assert_eq!(escape_label_value("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
        // A quote inside HELP is legal and must NOT be escaped away; only the
        // two characters that would end or continue the line are.
        assert_eq!(escape_help("a\"b\\c\nd"), "a\"b\\\\c\\nd");
    }

    #[test]
    fn empty_attribute_sets_render_no_brace_block() {
        let none: [KeyValue; 0] = [];
        assert_eq!(format_labels(none.iter(), None), "");
        let attrs = [KeyValue::new("outcome", "ok")];
        assert_eq!(format_labels(attrs.iter(), None), "{outcome=\"ok\"}");
        assert_eq!(
            format_labels(attrs.iter(), Some(("le", "0.5"))),
            "{outcome=\"ok\",le=\"0.5\"}"
        );
    }

    /// The end-to-end shape, against a real `SdkMeterProvider`.
    ///
    /// This is the test that would have caught the defect this module exists
    /// to fix: it records through the ordinary instrument API and asserts the
    /// value comes back out of the scrape rendering, rather than asserting
    /// that a provider was constructed.
    ///
    /// It owns the process-wide reader for this test binary — `reader()` is a
    /// `OnceLock` and `ManualReader::register_pipeline` binds to the first
    /// pipeline only, so no second test here may build a provider from it.
    #[test]
    fn recorded_measurements_reach_the_rendered_exposition() {
        use opentelemetry::metrics::MeterProvider as _;

        let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_reader(reader())
            .build();
        let meter = provider.meter("scrape-test");

        let counter = meter.u64_counter("qa_runs_dispatch_total").build();
        counter.add(2, &[KeyValue::new("outcome", "ok")]);
        counter.add(1, &[KeyValue::new("outcome", "error")]);

        let hist = meter
            .f64_histogram("qa_runs_dispatch_duration_seconds")
            .build();
        hist.record(0.25, &[KeyValue::new("outcome", "ok")]);

        let handle = reader();
        let mut rm = ResourceMetrics::default();
        handle.collect(&mut rm).expect("manual collect");
        let text = encode(&rm);

        assert!(
            text.contains("# TYPE qa_runs_dispatch_total counter"),
            "monotonic sums must render as counters:\n{text}"
        );
        assert!(
            text.contains("qa_runs_dispatch_total{outcome=\"ok\"} 2"),
            "the recorded counter value must appear:\n{text}"
        );
        assert!(
            text.contains("qa_runs_dispatch_total{outcome=\"error\"} 1"),
            "each attribute set is its own series:\n{text}"
        );
        assert!(
            text.contains("# TYPE qa_runs_dispatch_duration_seconds histogram"),
            "histograms must be typed as such:\n{text}"
        );
        assert!(
            text.contains("qa_runs_dispatch_duration_seconds_count{outcome=\"ok\"} 1"),
            "histogram _count must appear:\n{text}"
        );
        assert!(
            text.contains("qa_runs_dispatch_duration_seconds_sum{outcome=\"ok\"} 0.25"),
            "histogram _sum must appear:\n{text}"
        );
        assert!(
            text.contains("qa_runs_dispatch_duration_seconds_bucket{outcome=\"ok\",le=\"+Inf\"} 1"),
            "the +Inf bucket must close the histogram:\n{text}"
        );

        // Buckets must be CUMULATIVE. The SDK's default explicit boundaries
        // start 0, 5, 10, 25, ... so the 0.25 sample lands in the (0, 5]
        // bucket: `le="0"` is 0 and EVERY boundary from 5 upwards is 1. A
        // renderer that emitted the raw per-bucket counts would instead show 1
        // at `le="5"` and 0 at every boundary above it, which is what the third
        // assertion here rules out.
        assert!(
            text.contains("qa_runs_dispatch_duration_seconds_bucket{outcome=\"ok\",le=\"0\"} 0"),
            "buckets below the sample must be 0:\n{text}"
        );
        assert!(
            text.contains("qa_runs_dispatch_duration_seconds_bucket{outcome=\"ok\",le=\"5\"} 1"),
            "the bucket containing the sample must be 1:\n{text}"
        );
        assert!(
            text.contains(
                "qa_runs_dispatch_duration_seconds_bucket{outcome=\"ok\",le=\"10000\"} 1"
            ),
            "buckets above the sample must stay 1, not drop back to 0:\n{text}"
        );

        // HELP/TYPE appear exactly once per family, whatever the scope count.
        assert_eq!(
            text.matches("# TYPE qa_runs_dispatch_total ").count(),
            1,
            "a duplicated TYPE line makes Prometheus reject the whole scrape:\n{text}"
        );
    }
}
