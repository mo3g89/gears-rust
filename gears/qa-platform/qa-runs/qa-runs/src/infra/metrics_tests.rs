//! Adapter-level tests: what [`super::QaRunsMetricsMeter`] actually exports, read back
//! through a real `OpenTelemetry` SDK pipeline with an in-memory exporter.
//!
//! # What is checked here and what is checked at the call sites
//!
//! These tests answer *"is the series a dashboard queries the one this adapter
//! writes"*. Whether the two measured paths call the port at all, and with what
//! outcome, is `domain::service::dispatch`'s and `domain::service::ingest`'s to
//! answer — an adapter test cannot see a missing call site.
//!
//! The split matters because of the plan's own constraint that an emission is
//! silent when no adapter is installed: that makes it easy to write a metric
//! assertion that passes while measuring nothing, so each half is written
//! against the thing it can actually falsify.

use std::time::Duration;

use super::build_default_adapter;
use super::probe::MetricsProbe;
use crate::domain::metrics::{
    COUNTERS, DURATIONS, QA_RUNS_DISPATCH, QA_RUNS_DISPATCH_DECISION, QA_RUNS_DISPATCH_DURATION,
    QA_RUNS_INGEST, QA_RUNS_INGEST_DURATION,
};
use crate::domain::ports::metrics::{
    DispatchDecision, DispatchMetrics, DispatchOutcome, IngestMetrics, IngestOutcome,
};

/// **Every family in the catalog is exported under exactly its catalog name.**
///
/// The defect this catches is the one `domain::metrics`' header calls invisible:
/// a series exported under a name nobody queries looks, from inside this
/// process, exactly like a series that works. The catalog's own naming tests
/// cannot catch it — they assert properties of the constants, and are a gate on
/// the exported name only if the constant *is* the instrument name. This is the
/// test that ties the two together, and it is why
/// [`super::QaRunsMetricsMeter::new`] takes no name prefix.
#[test]
fn every_catalog_family_is_exported_under_its_catalog_name() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    // One emission into every family, so each instrument has a data point and
    // therefore appears in the export at all.
    adapter.dispatch_pass(DispatchOutcome::Started, Duration::from_millis(1));
    adapter.dispatch_decision(DispatchDecision::Queued);
    adapter.ingest_batch(IngestOutcome::Applied, Duration::from_millis(1));

    let series = probe.collect();
    let exported = series.names();
    for family in COUNTERS.iter().chain(DURATIONS) {
        assert!(
            exported.contains(family),
            "{family} is in the catalog but nothing exported it; exported names \
             were {exported:?}"
        );
    }
}

/// **One `dispatch_pass` call drives both of its instruments, once each.**
///
/// The property [`DispatchMetrics::dispatch_pass`]'s doc states as the reason
/// its signature takes the duration rather than leaving the histogram to a
/// second call: the rate and the quantile can then never disagree about how
/// many passes there were. Two methods would make that a convention.
#[test]
fn one_dispatch_pass_drives_the_counter_and_its_histogram_together() {
    let probe = MetricsProbe::new();

    probe
        .adapter()
        .dispatch_pass(DispatchOutcome::Failed, Duration::from_millis(250));

    let series = probe.collect();
    assert_eq!(series.counter(QA_RUNS_DISPATCH), 1);
    assert_eq!(series.histogram_count(QA_RUNS_DISPATCH_DURATION), 1);
    assert_eq!(
        series.counter_with(QA_RUNS_DISPATCH, &[("outcome", "failed")]),
        1,
        "the label value is the enum's own rendering, not a second spelling"
    );
}

/// The ingest half of the pairing above.
#[test]
fn one_ingest_batch_drives_the_counter_and_its_histogram_together() {
    let probe = MetricsProbe::new();

    probe
        .adapter()
        .ingest_batch(IngestOutcome::Duplicate, Duration::from_millis(3));

    let series = probe.collect();
    assert_eq!(series.counter(QA_RUNS_INGEST), 1);
    assert_eq!(series.histogram_count(QA_RUNS_INGEST_DURATION), 1);
    assert_eq!(
        series.counter_with(QA_RUNS_INGEST, &[("outcome", "duplicate")]),
        1
    );
}

/// **Every label value the taxonomy admits reaches the exporter as its own data
/// point.**
///
/// Sweeps the three `ALL` constants rather than sampling. The defect is an
/// adapter that hard-codes one attribute — every value would still export, into
/// one merged series, and a per-outcome dashboard would read a flat line for
/// every outcome but the hard-coded one.
#[test]
fn every_label_value_reaches_the_exporter_on_its_own_series() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    for outcome in DispatchOutcome::ALL {
        adapter.dispatch_pass(outcome, Duration::from_millis(1));
    }
    for decision in DispatchDecision::ALL {
        adapter.dispatch_decision(decision);
    }
    for outcome in IngestOutcome::ALL {
        adapter.ingest_batch(outcome, Duration::from_millis(1));
    }

    let series = probe.collect();
    for outcome in DispatchOutcome::ALL {
        assert_eq!(
            series.counter_with(QA_RUNS_DISPATCH, &[("outcome", outcome.as_str())]),
            1,
            "dispatch outcome {} exported no series of its own",
            outcome.as_str()
        );
    }
    for decision in DispatchDecision::ALL {
        assert_eq!(
            series.counter_with(
                QA_RUNS_DISPATCH_DECISION,
                &[("decision", decision.as_str())]
            ),
            1,
            "dispatch decision {} exported no series of its own",
            decision.as_str()
        );
    }
    for outcome in IngestOutcome::ALL {
        assert_eq!(
            series.counter_with(QA_RUNS_INGEST, &[("outcome", outcome.as_str())]),
            1,
            "ingest outcome {} exported no series of its own",
            outcome.as_str()
        );
    }
}

/// **The duration is recorded in seconds, and the buckets are the ones the NFR
/// thresholds sit on.**
///
/// Both halves are one property: a value in seconds against the `OTel` default
/// boundaries — which are milliseconds in all but name — puts every pass this
/// gear will ever do in the first bucket, and a p95 read off that histogram is
/// the bucket edge rather than an estimate. Recording six seconds and asking
/// which bucket it landed in falsifies both a millisecond conversion and the
/// default boundaries at once.
#[test]
fn a_duration_is_recorded_in_seconds_against_the_declared_buckets() {
    let probe = MetricsProbe::new();

    probe
        .adapter()
        .dispatch_pass(DispatchOutcome::Started, Duration::from_secs(6));

    let series = probe.collect();
    assert_eq!(
        series.histogram_bucket_of(QA_RUNS_DISPATCH_DURATION, 6.0),
        Some(1),
        "six seconds must land in the (5, 10] bucket: the NFR threshold is 10 s \
         and an alert is written against that edge"
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

    adapter.dispatch_pass(DispatchOutcome::Refused, Duration::from_millis(1));
    adapter.dispatch_decision(DispatchDecision::Unqueued);
    adapter.ingest_batch(IngestOutcome::Completed, Duration::from_millis(1));
}
