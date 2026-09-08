//! Adapter-level tests: what [`super::QaEnvironmentsMetricsMeter`] actually
//! exports, read back through a real `OpenTelemetry` SDK pipeline with an
//! in-memory exporter.
//!
//! # What is checked here and what is checked at the call site
//!
//! These tests answer *"is the series a dashboard queries the one this adapter
//! writes"*. Whether the observation cycle calls the port at all, and with what
//! outcome, is `domain::service::environments`' to answer — an adapter test
//! cannot see a missing call site.
//!
//! The split matters because of the constraint that an emission is silent when
//! no adapter is installed: that makes it easy to write a metric assertion that
//! passes while measuring nothing, so each half is written against the thing it
//! can actually falsify.

use std::time::Duration;

use super::probe::MetricsProbe;
use super::{DURATION_BUCKETS, SCOPE, build_default_adapter};
use crate::domain::metrics::{
    COUNTERS, DURATIONS, QA_ENVIRONMENTS_OBSERVATION, QA_ENVIRONMENTS_OBSERVATION_CYCLE,
    QA_ENVIRONMENTS_OBSERVATION_CYCLE_DURATION, QA_ENVIRONMENTS_OBSERVATION_DURATION,
};
use crate::domain::ports::metrics::{
    CycleOutcome, EnvironmentOutcome, ObservationClass, ObservationMetrics,
};

/// **Every family in the catalog is exported under exactly its catalog name.**
///
/// The defect this catches is the one `domain::metrics`' header calls
/// invisible: a series exported under a name nobody queries looks, from inside
/// this process, exactly like a series that works. The catalog's own naming
/// tests cannot catch it — they assert properties of the constants, and are a
/// gate on the exported name only if the constant *is* the instrument name.
/// This is the test that ties the two together, and it is why
/// [`super::QaEnvironmentsMetricsMeter::new`] takes no name prefix.
#[test]
fn every_catalog_family_is_exported_under_its_catalog_name() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    // One emission into every family, so each instrument has a data point and
    // therefore appears in the export at all.
    adapter.observation_cycle(CycleOutcome::Completed, Duration::from_millis(1));
    adapter.cycle_environments(EnvironmentOutcome::Observed, 1);
    adapter.environment_observed(ObservationClass::Detected, Duration::from_millis(1));

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

/// **One `observation_cycle` call drives both of its instruments, once each.**
///
/// The property
/// [`crate::domain::ports::metrics::ObservationMetrics::observation_cycle`]'s
/// doc states as the reason its signature takes the duration rather than
/// leaving the histogram to a second call: the rate and the quantile can then
/// never disagree about how many cycles there were. Two methods would make that
/// a convention.
#[test]
fn one_observation_cycle_drives_the_counter_and_its_histogram_together() {
    let probe = MetricsProbe::new();

    probe
        .adapter()
        .observation_cycle(CycleOutcome::Unstarted, Duration::from_millis(250));

    let series = probe.collect();
    assert_eq!(series.counter(QA_ENVIRONMENTS_OBSERVATION_CYCLE), 1);
    assert_eq!(
        series.histogram_count(QA_ENVIRONMENTS_OBSERVATION_CYCLE_DURATION),
        1
    );
    assert_eq!(
        series.counter_with(
            QA_ENVIRONMENTS_OBSERVATION_CYCLE,
            &[("outcome", "unstarted")]
        ),
        1,
        "the label value is the enum's own rendering, not a second spelling"
    );
}

/// **The report-driven counter adds the count it was given, under its own
/// label.**
///
/// Unlike every other emission in this subsystem this one is not an increment
/// of one: it carries `ObservationCycleReport`'s field verbatim, so a cycle
/// that observed forty environments moves the series by forty. A `+= 1`
/// implementation would still export, still carry the right label, and read as
/// a deployment with one environment.
#[test]
fn the_report_driven_counter_adds_the_report_s_own_count() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    adapter.cycle_environments(EnvironmentOutcome::Observed, 40);
    adapter.cycle_environments(EnvironmentOutcome::Failed, 2);

    let series = probe.collect();
    assert_eq!(
        series.counter_with(QA_ENVIRONMENTS_OBSERVATION, &[("outcome", "observed")]),
        40
    );
    assert_eq!(
        series.counter_with(QA_ENVIRONMENTS_OBSERVATION, &[("outcome", "failed")]),
        2
    );
    assert_eq!(
        series.counter(QA_ENVIRONMENTS_OBSERVATION),
        42,
        "and the two together are the report's attempted count"
    );
}

/// **A zero count still creates its series.**
///
/// A healthy deployment reports `failed = 0` on every cycle. If that emission
/// were skipped the failure series would not exist at all until the first
/// failure, and `rate(..{outcome="failed"}[5m])` — the query an alert is
/// written as — returns *no data* rather than zero for an absent series, which
/// silently evaluates to nothing rather than to a healthy zero.
#[test]
fn a_zero_count_still_exports_its_series() {
    let probe = MetricsProbe::new();

    probe
        .adapter()
        .cycle_environments(EnvironmentOutcome::Failed, 0);

    let series = probe.collect();
    assert!(
        series.names().contains(&QA_ENVIRONMENTS_OBSERVATION),
        "a zero addition must still create the family; exported names were {:?}",
        series.names()
    );
    assert_eq!(
        series.counter_with(QA_ENVIRONMENTS_OBSERVATION, &[("outcome", "failed")]),
        0
    );
}

/// **Every label value the taxonomy admits reaches the exporter as its own data
/// point.**
///
/// Sweeps the three `ALL` constants rather than sampling. The defect is an
/// adapter that hard-codes one attribute — every value would still export, into
/// one merged series, and a per-class dashboard would read a flat line for
/// every class but the hard-coded one. That matters most on the per-environment
/// histogram, whose whole purpose is that its classes are distinguishable.
#[test]
fn every_label_value_reaches_the_exporter_on_its_own_series() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    for outcome in CycleOutcome::ALL {
        adapter.observation_cycle(outcome, Duration::from_millis(1));
    }
    for outcome in EnvironmentOutcome::ALL {
        adapter.cycle_environments(outcome, 1);
    }
    for class in ObservationClass::ALL {
        adapter.environment_observed(class, Duration::from_millis(1));
    }

    let series = probe.collect();
    for outcome in CycleOutcome::ALL {
        assert_eq!(
            series.counter_with(
                QA_ENVIRONMENTS_OBSERVATION_CYCLE,
                &[("outcome", outcome.as_str())]
            ),
            1,
            "cycle outcome {} exported no series of its own",
            outcome.as_str()
        );
    }
    for outcome in EnvironmentOutcome::ALL {
        assert_eq!(
            series.counter_with(
                QA_ENVIRONMENTS_OBSERVATION,
                &[("outcome", outcome.as_str())]
            ),
            1,
            "environment outcome {} exported no series of its own",
            outcome.as_str()
        );
    }
    for class in ObservationClass::ALL {
        assert_eq!(
            series.histogram_count_with(
                QA_ENVIRONMENTS_OBSERVATION_DURATION,
                &[("class", class.as_str())]
            ),
            1,
            "observation class {} exported no series of its own",
            class.as_str()
        );
    }
}

/// **The per-environment histogram carries `class`, and the counter beside it
/// carries `outcome`.**
///
/// The two keys are different on purpose — see [`super::CLASS`]'s doc — and
/// nothing else here would notice if the adapter used one key for both. It
/// would still export, and a dashboard summing the family by `outcome` would
/// then be adding a nine-valued partition to a two-valued partition of the same
/// events.
#[test]
fn the_two_environment_families_carry_different_label_keys() {
    let probe = MetricsProbe::new();
    let adapter = probe.adapter();

    adapter.cycle_environments(EnvironmentOutcome::Observed, 1);
    adapter.environment_observed(ObservationClass::Unreachable, Duration::from_millis(1));

    let series = probe.collect();
    assert_eq!(
        series.histogram_count_with(
            QA_ENVIRONMENTS_OBSERVATION_DURATION,
            &[("class", "unreachable")]
        ),
        1,
        "the histogram's key is `class`"
    );
    assert_eq!(
        series.histogram_count_with(
            QA_ENVIRONMENTS_OBSERVATION_DURATION,
            &[("outcome", "unreachable")]
        ),
        0,
        "and it is not `outcome`"
    );
    assert_eq!(
        series.counter_with(QA_ENVIRONMENTS_OBSERVATION, &[("outcome", "observed")]),
        1,
        "the counter's key is `outcome`"
    );
    assert_eq!(
        series.counter_with(QA_ENVIRONMENTS_OBSERVATION, &[("class", "observed")]),
        0,
        "and it is not `class`"
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

    adapter.observation_cycle(CycleOutcome::Completed, Duration::from_secs(6));
    adapter.environment_observed(ObservationClass::Detected, Duration::from_secs(6));

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

/// **The declared set puts an edge on the ticker's floor and on its default
/// interval.**
///
/// Not decoration: [`super::DURATION_BUCKETS`]' doc argues for those two
/// numbers specifically, on the grounds that a cycle outrunning its own
/// interval silently delays every later tick, and that claim is only true of a
/// dashboard if the numbers are really boundaries. A re-tuned set that dropped
/// either would leave the argument in the doc and the capability gone.
///
/// Both numbers are **read out of `ObservationConfig` rather than written as
/// literals**, so the argument survives a change to the config: a raised floor
/// or a changed default fails here instead of quietly leaving the boundary
/// behind on the old number. The floor is obtained the only way it can be from
/// outside — `MIN_POLL_INTERVAL_SECONDS` is private, and asking for a
/// zero-second interval is exactly what that function clamps.
#[test]
fn the_declared_boundaries_include_the_pollers_floor_and_default_interval() {
    let floor = crate::config::ObservationConfig {
        enabled: true,
        poll_interval_seconds: 0,
    }
    .effective_poll_interval_seconds();
    let default_interval =
        crate::config::ObservationConfig::default().effective_poll_interval_seconds();

    for (seconds, what) in [
        (floor, "the floor a poll interval is clamped to"),
        (default_interval, "the default poll interval"),
    ] {
        #[allow(
            clippy::cast_precision_loss,
            reason = "both values are two- and three-digit second counts"
        )]
        let boundary = seconds as f64;
        assert!(
            DURATION_BUCKETS.contains(&boundary),
            "{seconds} s is {what} and must be a bucket edge, so that `did this cycle \
             outrun its own interval` is a bucket rather than an interpolation; \
             boundaries are {DURATION_BUCKETS:?}"
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
        .environment_observed(ObservationClass::Detected, Duration::from_secs(6));

    let series = probe.collect();
    assert_eq!(
        series.histogram_bucket_of(QA_ENVIRONMENTS_OBSERVATION_DURATION, 6.0),
        Some(1),
        "six seconds recorded as seconds lands in the bucket that contains 6.0; \
         recorded as 6000 it would land in the overflow bucket instead"
    );
    assert_eq!(
        series.histogram_bucket_of(QA_ENVIRONMENTS_OBSERVATION_DURATION, 6000.0),
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

    adapter.observation_cycle(CycleOutcome::Cancelled, Duration::from_millis(1));
    adapter.cycle_environments(EnvironmentOutcome::Failed, 7);
    adapter.environment_observed(ObservationClass::AuthRejected, Duration::from_millis(1));
}
