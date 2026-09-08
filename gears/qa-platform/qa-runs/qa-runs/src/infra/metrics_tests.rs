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

use super::probe::MetricsProbe;
use super::{DURATION_BUCKETS, SCOPE, build_default_adapter};
use crate::domain::metrics::{
    COUNTERS, DURATIONS, QA_RUNS_DISPATCH, QA_RUNS_DISPATCH_DECISION, QA_RUNS_DISPATCH_DURATION,
    QA_RUNS_INGEST, QA_RUNS_INGEST_DURATION, QA_RUNS_QUEUE_WAIT, QA_RUNS_QUEUE_WAIT_DURATION,
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
    adapter.dispatch_pass(DispatchOutcome::Completed, Duration::from_millis(1));
    adapter.dispatch_decision(DispatchDecision::Queued);
    adapter.queue_wait(Duration::from_millis(1));
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
    assert_eq!(
        series.scopes(),
        vec![SCOPE],
        "every family must be reported under this gear's one instrumentation \
         scope; two scopes means two meters were built"
    );
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
/// point — on the histograms as well as the counters.**
///
/// Sweeps the three `ALL` constants rather than sampling. The defect is an
/// adapter that hard-codes one attribute — every value would still export, into
/// one merged series, and a per-outcome dashboard would read a flat line for
/// every outcome but the hard-coded one.
///
/// **The histogram half is the half that was missing**, and it is the half a
/// dashboard needs most: `record(value, &[])` beside a correctly labelled
/// `add` satisfies every count-only assertion in this file, and then every
/// `histogram_quantile(… by (outcome))` written against these families
/// collapses to one merged series with no error anywhere. qa-catalog's
/// equivalent test states the same thing about itself; this is that assertion
/// back-ported.
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
        assert_eq!(
            series
                .histogram_count_with(QA_RUNS_DISPATCH_DURATION, &[("outcome", outcome.as_str())]),
            1,
            "dispatch outcome {} exported no HISTOGRAM series of its own, so a \
             per-outcome quantile query would read one merged distribution",
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
        assert_eq!(
            series.histogram_count_with(QA_RUNS_INGEST_DURATION, &[("outcome", outcome.as_str())]),
            1,
            "ingest outcome {} exported no HISTOGRAM series of its own, so a \
             per-outcome quantile query would read one merged distribution",
            outcome.as_str()
        );
    }
}

/// **Every duration histogram is built with the declared boundaries.**
///
/// Read off the exported `bounds()`, not inferred from where a value landed.
/// The first version of this test did the latter and **was not a gate**:
/// `histogram_bucket_of` computes its index from the very bounds in force, so
/// six seconds reports "the bucket containing six seconds" under the declared
/// set and under the `OTel` defaults alike, and deleting `with_boundaries` left
/// it green. Break-verified in the fix round by deleting both calls.
#[test]
fn every_duration_histogram_carries_the_declared_boundaries() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    adapter.dispatch_pass(DispatchOutcome::Completed, Duration::from_secs(6));
    adapter.queue_wait(Duration::from_secs(6));
    adapter.ingest_batch(IngestOutcome::Applied, Duration::from_secs(6));

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

/// **The two NFR thresholds are bucket edges.**
///
/// [`super::DURATION_BUCKETS`]' doc claims 10.0 and 5.0 are boundaries *because
/// the two NFR thresholds are those numbers*: `cpt-cf-qa-nfr-dispatch-latency`
/// is *queued start <= 10 s p95* and `cpt-cf-qa-nfr-result-latency` is *result
/// visible <= 5 s p95* (DESIGN 1.2's table). Putting each on an edge is what
/// lets an alert read a bucket instead of interpolating across one. This gear's
/// is the only bucket set in the
/// subsystem that encodes a threshold, and the claim is only true of a dashboard
/// while the numbers are actually in the set — a re-tuning that moved either one
/// would leave the reasoning in the doc and take the capability away, silently,
/// with every other test in this file still green. qa-environments and
/// qa-catalog each assert the equivalent property of their own sets.
///
/// It asserts membership only. Whether either series *measures* its NFR is a
/// different question, settled in the negative in
/// [`crate::domain::metrics::QA_RUNS_DISPATCH_DURATION`]'s doc and settled
/// two-sidedly in [`crate::domain::metrics::QA_RUNS_QUEUE_WAIT_DURATION`]'s.
#[test]
fn the_declared_boundaries_put_the_nfr_thresholds_on_bucket_edges() {
    for (threshold, what) in [
        (10.0_f64, "the cpt-cf-qa-nfr-dispatch-latency p95 threshold"),
        (5.0_f64, "the cpt-cf-qa-nfr-result-latency p95 threshold"),
    ] {
        assert!(
            DURATION_BUCKETS.contains(&threshold),
            "{threshold} s is {what} and must be a bucket edge, so an alert reads a \
             bucket rather than an interpolation across one; boundaries are \
             {DURATION_BUCKETS:?}"
        );
    }
}

/// **Every duration is recorded in seconds, not milliseconds.**
///
/// **Swept over every family in [`DURATIONS`]**, because each is a separate
/// `as_secs_f64` call site in the adapter: a version that got one right and
/// another wrong would pass a test that drove only the first. Measured in
/// qa-environments, where exactly that mutation came back green against a
/// single-family version of this test.
///
/// The other half of what the old single test claimed, kept separate because it
/// is a different defect: `as_millis` instead of `as_secs_f64` would put a
/// six-second pass in the overflow bucket above 60, and no assertion about the
/// boundaries themselves would notice.
#[test]
fn a_duration_is_recorded_in_seconds() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    // One six-second sample into every duration family this gear declares.
    adapter.dispatch_pass(DispatchOutcome::Completed, Duration::from_secs(6));
    adapter.queue_wait(Duration::from_secs(6));
    adapter.ingest_batch(IngestOutcome::Applied, Duration::from_secs(6));

    let series = probe.collect();
    for family in DURATIONS {
        assert_eq!(
            series.histogram_bucket_of(family, 6.0),
            Some(1),
            "{family}: six seconds recorded as seconds lands in the bucket that contains \
             6.0; recorded as 6000 it would land in the overflow bucket instead"
        );
        assert_eq!(
            series.histogram_bucket_of(family, 6000.0),
            Some(0),
            "{family}: and nothing may be sitting in the overflow bucket"
        );
    }
}

/// **One `queue_wait` call drives its counter and its histogram, unlabelled.**
///
/// The family carries no attributes on purpose — see the adapter's
/// implementation — so this also pins that no label crept in, and it does so
/// with `counter_with_exactly` / `histogram_count_with_exactly` rather than
/// with `counter_with`. That distinction is the whole assertion: an
/// **at-least** filter over an empty label set is vacuously true of every data
/// point, so `counter_with(name, &[])` returns exactly what `counter` returns
/// and would go on returning it the day a tenant id was attached here — which
/// is the disclosure rule `domain::ports::metrics`' header states. The exact
/// form is a statement about the labels the point does *not* carry, which no
/// subset filter can make.
#[test]
fn one_queue_wait_drives_the_counter_and_its_histogram_together() {
    let probe = MetricsProbe::new();

    probe.adapter().queue_wait(Duration::from_secs(3));

    let series = probe.collect();
    assert_eq!(series.counter(QA_RUNS_QUEUE_WAIT), 1);
    assert_eq!(series.histogram_count(QA_RUNS_QUEUE_WAIT_DURATION), 1);
    assert_eq!(
        series.counter_with_exactly(QA_RUNS_QUEUE_WAIT, &[]),
        1,
        "the queue-wait counter must carry NO attribute at all; any label here \
         would be one bearing a tenant, a platform or a run"
    );
    assert_eq!(
        series.histogram_count_with_exactly(QA_RUNS_QUEUE_WAIT_DURATION, &[]),
        1,
        "and neither may its histogram"
    );
    assert_eq!(
        series.histogram_bucket_of(QA_RUNS_QUEUE_WAIT_DURATION, 3.0),
        Some(1),
        "three seconds must land in the (2.5, 5] bucket"
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
    adapter.queue_wait(Duration::from_millis(1));
    adapter.ingest_batch(IngestOutcome::Completed, Duration::from_millis(1));
}
