//! Adapter-level tests: what [`super::QaInsightsMetricsMeter`] actually
//! exports, read back through a real `OpenTelemetry` SDK pipeline with an
//! in-memory exporter.
//!
//! # What is checked here and what is checked at the call sites
//!
//! These tests answer *"is the series a dashboard queries the one this adapter
//! writes"*. Whether the two measured paths call the port at all, and with what
//! outcome, is `domain::service::collect`'s and `domain::service::jira_poller`'s
//! to answer — an adapter test cannot see a missing call site.
//!
//! The split matters because of the plan's own constraint that an emission is
//! silent when no adapter is installed: that makes it easy to write a metric
//! assertion that passes while measuring nothing, so each half is written
//! against the thing it can actually falsify.

use std::time::Duration;

use super::probe::MetricsProbe;
use super::{DURATION_BUCKETS, SCOPE, build_default_adapter};
use crate::domain::metrics::{
    COUNTERS, DURATIONS, QA_INSIGHTS_COLLECT, QA_INSIGHTS_COLLECT_DURATION,
    QA_INSIGHTS_COLLECT_REPORT, QA_INSIGHTS_JIRA_BUG, QA_INSIGHTS_JIRA_POLL,
    QA_INSIGHTS_JIRA_POLL_DURATION, QA_INSIGHTS_JIRA_RERUN,
};
use crate::domain::ports::metrics::{
    CollectMetrics, CollectOutcome, CollectReportOutcome, JiraBugOutcome, JiraPollMetrics,
    JiraPollOutcome,
};

/// **Every family in the catalog is exported under exactly its catalog name.**
///
/// The defect this catches is the one `domain::metrics`' header calls
/// invisible: a series exported under a name nobody queries looks, from inside
/// this process, exactly like a series that works. The catalog's own naming
/// tests cannot catch it — they assert properties of the constants, and are a
/// gate on the exported name only if the constant *is* the instrument name.
/// This is the test that ties the two together, and it is why
/// [`super::QaInsightsMetricsMeter::new`] takes no name prefix.
#[test]
fn every_catalog_family_is_exported_under_its_catalog_name() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    // One emission into every family, so each instrument has a data point and
    // therefore appears in the export at all.
    adapter.collect_cycle(CollectOutcome::Completed, Duration::from_millis(1));
    adapter.collect_report(CollectReportOutcome::Recorded);
    adapter.poll_pass(JiraPollOutcome::Completed, Duration::from_millis(1));
    adapter.bug(JiraBugOutcome::Resolved);
    adapter.auto_rerun();

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

/// **One `collect_cycle` call drives both of its instruments, once each.**
///
/// The property [`CollectMetrics::collect_cycle`]'s doc states as the reason
/// its signature takes the duration rather than leaving the histogram to a
/// second call: the rate and the quantile can then never disagree about how
/// many cycles there were. Two methods would make that a convention.
#[test]
fn one_collect_cycle_drives_the_counter_and_its_histogram_together() {
    let probe = MetricsProbe::new();

    probe
        .adapter()
        .collect_cycle(CollectOutcome::Failed, Duration::from_millis(250));

    let series = probe.collect();
    assert_eq!(series.counter(QA_INSIGHTS_COLLECT), 1);
    assert_eq!(series.histogram_count(QA_INSIGHTS_COLLECT_DURATION), 1);
    assert_eq!(
        series.counter_with(QA_INSIGHTS_COLLECT, &[("outcome", "failed")]),
        1,
        "the label value is the enum's own rendering, not a second spelling"
    );
}

/// The poll half of the pairing above.
#[test]
fn one_poll_pass_drives_the_counter_and_its_histogram_together() {
    let probe = MetricsProbe::new();

    probe
        .adapter()
        .poll_pass(JiraPollOutcome::Skipped, Duration::from_millis(3));

    let series = probe.collect();
    assert_eq!(series.counter(QA_INSIGHTS_JIRA_POLL), 1);
    assert_eq!(series.histogram_count(QA_INSIGHTS_JIRA_POLL_DURATION), 1);
    assert_eq!(
        series.counter_with(QA_INSIGHTS_JIRA_POLL, &[("outcome", "skipped")]),
        1
    );
}

/// **Every label value the taxonomy admits reaches the exporter as its own data
/// point.**
///
/// Sweeps the four `ALL` constants rather than sampling. The defect is an
/// adapter that hard-codes one attribute — every value would still export, into
/// one merged series, and a per-outcome dashboard would read a flat line for
/// every outcome but the hard-coded one. That matters most on the per-bug
/// family, whose whole purpose is that its failure values are distinguishable.
#[test]
fn every_label_value_reaches_the_exporter_on_its_own_series() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    for outcome in CollectOutcome::ALL {
        adapter.collect_cycle(outcome, Duration::from_millis(1));
    }
    for outcome in CollectReportOutcome::ALL {
        adapter.collect_report(outcome);
    }
    for outcome in JiraPollOutcome::ALL {
        adapter.poll_pass(outcome, Duration::from_millis(1));
    }
    for outcome in JiraBugOutcome::ALL {
        adapter.bug(outcome);
    }

    let series = probe.collect();
    for outcome in CollectOutcome::ALL {
        assert_eq!(
            series.counter_with(QA_INSIGHTS_COLLECT, &[("outcome", outcome.as_str())]),
            1,
            "collect outcome {} exported no series of its own",
            outcome.as_str()
        );
    }
    for outcome in CollectReportOutcome::ALL {
        assert_eq!(
            series.counter_with(QA_INSIGHTS_COLLECT_REPORT, &[("outcome", outcome.as_str())]),
            1,
            "collect report outcome {} exported no series of its own",
            outcome.as_str()
        );
    }
    for outcome in JiraPollOutcome::ALL {
        assert_eq!(
            series.counter_with(QA_INSIGHTS_JIRA_POLL, &[("outcome", outcome.as_str())]),
            1,
            "poll outcome {} exported no series of its own",
            outcome.as_str()
        );
    }
    for outcome in JiraBugOutcome::ALL {
        assert_eq!(
            series.counter_with(QA_INSIGHTS_JIRA_BUG, &[("outcome", outcome.as_str())]),
            1,
            "per-bug outcome {} exported no series of its own",
            outcome.as_str()
        );
    }
}

/// **The auto-rerun family is one unlabelled series.**
///
/// The family carries no attributes on purpose — see
/// [`crate::domain::metrics::QA_INSIGHTS_JIRA_RERUN`] — so this pins that no
/// label crept in: a labelled data point would still be counted by `counter`,
/// but `counter_with` over an empty label set would stop describing the whole
/// family, and any label added here would be one carrying a tenant, a bug or a
/// plan, which is the disclosure rule this catalog is built around.
#[test]
fn the_auto_rerun_family_carries_no_labels() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    adapter.auto_rerun();
    adapter.auto_rerun();

    let series = probe.collect();
    assert_eq!(series.counter(QA_INSIGHTS_JIRA_RERUN), 2);
    assert_eq!(
        series.counter_with(QA_INSIGHTS_JIRA_RERUN, &[]),
        2,
        "an unlabelled family's whole total must be reachable with no label filter"
    );
}

/// **Every duration histogram is built with the declared boundaries.**
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
    let adapter = probe.adapter();

    adapter.collect_cycle(CollectOutcome::Completed, Duration::from_secs(6));
    adapter.poll_pass(JiraPollOutcome::Completed, Duration::from_secs(6));

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

/// **The duration is recorded in seconds, not milliseconds.**
///
/// A different defect from the boundaries themselves: `as_millis` instead of
/// `as_secs_f64` would put a six-second cycle in the overflow bucket above 300,
/// and no assertion about the boundary set would notice.
#[test]
fn a_duration_is_recorded_in_seconds() {
    let probe = MetricsProbe::new();

    probe
        .adapter()
        .collect_cycle(CollectOutcome::Completed, Duration::from_secs(6));

    let series = probe.collect();
    assert_eq!(
        series.histogram_bucket_of(QA_INSIGHTS_COLLECT_DURATION, 6.0),
        Some(1),
        "six seconds recorded as seconds lands in the bucket that contains 6.0; \
         recorded as 6000 it would land in the overflow bucket instead"
    );
    assert_eq!(
        series.histogram_bucket_of(QA_INSIGHTS_COLLECT_DURATION, 6000.0),
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

    adapter.collect_cycle(CollectOutcome::Refused, Duration::from_millis(1));
    adapter.collect_report(CollectReportOutcome::SignatureInvalid);
    adapter.poll_pass(JiraPollOutcome::Failed, Duration::from_millis(1));
    adapter.bug(JiraBugOutcome::StatusCheckFailed);
    adapter.auto_rerun();
}
