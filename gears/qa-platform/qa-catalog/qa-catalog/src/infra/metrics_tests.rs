//! Adapter-level tests: what [`super::QaCatalogMetricsMeter`] actually
//! exports, read back through a real `OpenTelemetry` SDK pipeline with an
//! in-memory exporter.
//!
//! # What is checked here and what is checked at the call site
//!
//! These tests answer *"is the series a dashboard queries the one this adapter
//! writes"*. Whether `plugin_for` calls the port at all, and with what outcome,
//! is `domain::service::plugin_registry`'s to answer — an adapter test cannot
//! see a missing call site.
//!
//! The split matters because of the constraint that an emission is silent when
//! no adapter is installed: that makes it easy to write a metric assertion that
//! passes while measuring nothing, so each half is written against the thing it
//! can actually falsify.

use std::time::Duration;

use super::probe::MetricsProbe;
use super::{DURATION_BUCKETS, SCOPE, build_default_adapter};
use crate::domain::metrics::{
    COUNTERS, DURATIONS, QA_CATALOG_PLUGIN_RESOLUTION, QA_CATALOG_PLUGIN_RESOLUTION_DURATION,
};
use crate::domain::ports::metrics::{PluginResolutionMetrics, PluginResolutionOutcome};

/// **Every family in the catalog is exported under exactly its catalog name.**
///
/// The defect this catches is the one `domain::metrics`' header calls
/// invisible: a series exported under a name nobody queries looks, from inside
/// this process, exactly like a series that works. The catalog's own naming
/// tests cannot catch it — they assert properties of the constants, and are a
/// gate on the exported name only if the constant *is* the instrument name.
/// This is the test that ties the two together, and it is why
/// [`super::QaCatalogMetricsMeter::new`] takes no name prefix.
#[test]
fn every_catalog_family_is_exported_under_its_catalog_name() {
    let probe = MetricsProbe::new();

    // One emission, which drives every family this gear has.
    probe
        .adapter()
        .plugin_resolution(PluginResolutionOutcome::Resolved, Duration::from_millis(1));

    let series = probe.collect();
    let exported = series.names();
    for family in COUNTERS.iter().chain(DURATIONS) {
        assert!(
            exported.contains(family),
            "{family} is in the catalog but nothing exported it; exported names \
             were {exported:?}"
        );
    }
    assert_eq!(
        series.scopes(),
        vec![SCOPE],
        "every family must be reported under this gear's one instrumentation \
         scope; two scopes means two meters were built"
    );
}

/// **One `plugin_resolution` call drives both of its instruments, once each.**
///
/// The property
/// [`crate::domain::ports::metrics::PluginResolutionMetrics::plugin_resolution`]'s
/// doc states as the reason its signature takes the duration rather than
/// leaving the histogram to a second call: the rate and the quantile can then
/// never disagree about how many resolutions there were. Two methods would make
/// that a convention.
#[test]
fn one_resolution_drives_the_counter_and_its_histogram_together() {
    let probe = MetricsProbe::new();

    probe.adapter().plugin_resolution(
        PluginResolutionOutcome::Unregistered,
        Duration::from_millis(250),
    );

    let series = probe.collect();
    assert_eq!(series.counter(QA_CATALOG_PLUGIN_RESOLUTION), 1);
    assert_eq!(
        series.histogram_count(QA_CATALOG_PLUGIN_RESOLUTION_DURATION),
        1
    );
    assert_eq!(
        series.counter_with(QA_CATALOG_PLUGIN_RESOLUTION, &[("outcome", "unregistered")]),
        1,
        "the label value is the enum's own rendering, not a second spelling"
    );
}

/// **Every label value the taxonomy admits reaches the exporter as its own data
/// point, on both instruments.**
///
/// Sweeps `ALL` rather than sampling. The defect is an adapter that hard-codes
/// the attribute — every value would still export, into one merged series, and
/// a dashboard split by outcome would read a flat line for every value but the
/// hard-coded one.
///
/// The histogram half is checked with the same sweep because the two
/// instruments take their attributes from one array in the adapter but need not
/// have: a version that labelled only the counter would pass every other test
/// in this file.
#[test]
fn every_label_value_reaches_the_exporter_on_its_own_series() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    for outcome in PluginResolutionOutcome::ALL {
        adapter.plugin_resolution(outcome, Duration::from_millis(1));
    }

    let series = probe.collect();
    for outcome in PluginResolutionOutcome::ALL {
        assert_eq!(
            series.counter_with(
                QA_CATALOG_PLUGIN_RESOLUTION,
                &[("outcome", outcome.as_str())]
            ),
            1,
            "resolution outcome {} exported no counter series of its own",
            outcome.as_str()
        );
        assert_eq!(
            series.histogram_count_with(
                QA_CATALOG_PLUGIN_RESOLUTION_DURATION,
                &[("outcome", outcome.as_str())]
            ),
            1,
            "resolution outcome {} exported no histogram series of its own",
            outcome.as_str()
        );
    }
}

/// **The duration histogram is built with the declared boundaries.**
///
/// Read off the exported `bounds()`, not inferred from where a value landed.
/// The naive version of this test does the latter and **is not a gate**:
/// `histogram_bucket_of` computes its index from the very bounds in force, so
/// one value reports "the bucket containing it" under the declared set and
/// under the `OTel` defaults alike, and deleting `with_boundaries` leaves it
/// green. That is a finding qa-runs' own review measured; this file starts
/// where that ended.
#[test]
fn every_duration_histogram_carries_the_declared_boundaries() {
    let probe = MetricsProbe::new();

    probe
        .adapter()
        .plugin_resolution(PluginResolutionOutcome::Resolved, Duration::from_millis(3));

    let series = probe.collect();
    for family in DURATIONS {
        assert_eq!(
            series.histogram_bounds(family).as_deref(),
            Some(DURATION_BUCKETS),
            "{family} must carry the declared second boundaries; the OTel defaults \
             are milliseconds in all but name and would squeeze every value this \
             gear records into one interval"
        );
    }
}

/// **The declared set resolves the range a healthy resolution actually falls
/// in.**
///
/// [`super::DURATION_BUCKETS`]' doc argues that this gear's set is two orders
/// of magnitude lower than qa-environments' because a resolution is a policy
/// decision plus one indexed row read, not a round trip to a customer cluster.
/// That argument is only true of a dashboard if the low end really is
/// sub-millisecond, so it is asserted rather than described: a set re-tuned
/// upward would leave the reasoning in the doc and the capability gone, and a
/// p95 over a healthy deployment would again be a bucket edge.
///
/// The upper bound of the assertion is deliberately loose — what is being
/// pinned is that several edges exist below the millisecond mark, not any
/// particular one of them.
#[test]
fn the_declared_boundaries_resolve_the_sub_millisecond_range() {
    let below_one_millisecond = DURATION_BUCKETS
        .iter()
        .filter(|bound| **bound <= 0.001)
        .count();
    assert!(
        below_one_millisecond >= 1,
        "a resolution that never leaves this process must not land in the first \
         bucket every time; boundaries are {DURATION_BUCKETS:?}"
    );
    assert!(
        DURATION_BUCKETS.iter().filter(|b| **b <= 0.05).count() >= 4,
        "and the range a healthy resolution falls in needs more than one edge to be \
         a distribution rather than a count; boundaries are {DURATION_BUCKETS:?}"
    );
}

/// **The duration is recorded in seconds, not milliseconds.**
///
/// A different defect from the boundaries themselves: `as_millis` instead of
/// `as_secs_f64` would put a six-second resolution in the overflow bucket above
/// ten, and no assertion about the boundary set would notice.
#[test]
fn a_duration_is_recorded_in_seconds() {
    let probe = MetricsProbe::new();

    probe
        .adapter()
        .plugin_resolution(PluginResolutionOutcome::Resolved, Duration::from_secs(6));

    let series = probe.collect();
    assert_eq!(
        series.histogram_bucket_of(QA_CATALOG_PLUGIN_RESOLUTION_DURATION, 6.0),
        Some(1),
        "six seconds recorded as seconds lands in the bucket that contains 6.0; \
         recorded as 6000 it would land in the overflow bucket instead"
    );
    assert_eq!(
        series.histogram_bucket_of(QA_CATALOG_PLUGIN_RESOLUTION_DURATION, 6000.0),
        Some(0),
        "and nothing may be sitting in the overflow bucket"
    );
}

/// **The default adapter builds and emits with no pipeline configured.**
///
/// This is the boot posture the gear depends on: [`build_default_adapter`]
/// reads the process-global meter provider, which is the built-in no-op until
/// something installs one, so a deployment with `metrics.enabled: false` — or
/// with no `OpenTelemetry` configuration at all — still constructs every
/// instrument and still emits, silently.
///
/// It asserts nothing about the series, deliberately: there is no exporter to
/// read one from, and inventing one would mean installing a global provider,
/// which every other test in this process would then see. What it pins is that
/// neither construction nor emission can fail or panic on that path.
#[test]
fn the_default_adapter_emits_silently_with_no_pipeline_configured() {
    let adapter = build_default_adapter();

    for outcome in PluginResolutionOutcome::ALL {
        adapter.plugin_resolution(outcome, Duration::from_millis(1));
    }
}
