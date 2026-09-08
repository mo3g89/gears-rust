//! What the observation cycle actually reports, read back through a real
//! `OpenTelemetry` pipeline.
//!
//! # Why these run against the real adapter and not a mock port
//!
//! The claim worth making is that a dashboard query finds the series, and a
//! mock of [`crate::domain::ports::metrics::ObservationMetrics`] can only prove
//! that this module called a method. `crate::infra::metrics::probe` gives each
//! test its own `SdkMeterProvider` and in-memory exporter, so every assertion
//! below is against what was really exported — which is also what makes the
//! label keys and values part of what is under test.
//!
//! The tier is the DB-backed one `environments_observation_tests` uses: real
//! `SeaORM` repositories over in-memory `SQLite`, real migrations, doubles only
//! for credstore, the runner-`Secret` writer and the product plugin. A cycle is
//! a loop over rows, and a mock repository cannot produce the population these
//! tests are about.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

use qa_environments_sdk::{CredentialMaterial, NewEnvironment};
use qa_product_sdk::observation::{
    FailureClass, HealthOutcome, ObservationOutcome, ObservedAttrs, PluginFailure,
    PluginObservation,
};
use tokio_util::sync::CancellationToken;
use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::metrics::{
    QA_ENVIRONMENTS_OBSERVATION, QA_ENVIRONMENTS_OBSERVATION_CYCLE,
    QA_ENVIRONMENTS_OBSERVATION_CYCLE_DURATION, QA_ENVIRONMENTS_OBSERVATION_DURATION,
};
use crate::domain::ports::metrics::{
    CycleOutcome, EnvironmentOutcome, ObservationClass, ObservationMetrics,
};
use crate::domain::ports::{NoopRunnerSecretWriter, ProductPluginPort};
use crate::domain::repos::{EnvironmentsRepository, PersistedCredentials};
use crate::infra::metrics::probe::MetricsProbe;
use crate::infra::storage::OrmEnvironmentsRepository;
use crate::test_support::{
    DenyAllAuthZ, FixedPluginPort, KeyedPlugin, RecordingCredStore, ScriptedPlugin,
    TenantScopedAuthZ, build_services_tenant_scoped_with_plugin,
    build_services_tenant_scoped_with_plugin_and_metrics,
    build_services_with_plugin_port_and_metrics, ctx, inmem_db,
};

/// Every environment in this module belongs to one product; which product it is
/// does not matter, because every port here resolves the same plugin.
fn product() -> Uuid {
    Uuid::from_u128(0x9001)
}

fn pasted(name: &str, document: &str) -> NewEnvironment {
    NewEnvironment {
        kubeconfig_credstore_ref: None,
        credentials: std::collections::BTreeMap::new(),
        name: name.to_owned(),
        product_id: product(),
        description: None,
        kubeconfig: Some(CredentialMaterial::new(document.to_owned())),
        default_branch: None,
        is_default: false,
    }
}

fn detected() -> PluginObservation {
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "1.2.3");
    PluginObservation {
        environment: ObservationOutcome::Detected(attrs),
        health: HealthOutcome::NotAttempted,
    }
}

fn failed_with(class: FailureClass) -> PluginObservation {
    PluginObservation {
        environment: ObservationOutcome::Failed(PluginFailure::bare(class)),
        health: HealthOutcome::NotAttempted,
    }
}

/// A port resolving every product to a plugin that always reports a successful
/// detection.
fn port_detecting() -> Arc<dyn ProductPluginPort> {
    Arc::new(FixedPluginPort::new(Arc::new(ScriptedPlugin::vhp_shaped(
        detected(),
    ))))
}

/// A port whose plugin answers per kubeconfig document, so one cycle can drive
/// several environments to different classes regardless of visit order.
fn port_keyed(outcomes: HashMap<Vec<u8>, PluginObservation>) -> Arc<dyn ProductPluginPort> {
    Arc::new(FixedPluginPort::new(Arc::new(KeyedPlugin::new(outcomes))))
}

/// Write an environment row carrying a **nil tenant id**, which the observation
/// cycle skips before contacting anything.
///
/// Through the repository rather than through `create_environment`, because the
/// service cannot produce this row: `TenantScopedAuthZ` compiles a nil subject
/// tenant into no constraint at all, and the PEP fails that closed
/// (`ConstraintsRequiredButAbsent` -> `Forbidden`) — measured, not assumed. The
/// row exists in real deployments as a pre-tenancy leftover, which is why
/// `run_observation_cycle` has a branch for it at all, and the branch is what
/// is under test.
///
/// This file may name `AccessScope::allow_all()`: it ends in `_tests.rs`, the
/// one exemption `unscoped_read_guard_tests`' textual ban grants, and its own
/// header explains why that exemption is granted to whole test modules.
async fn seed_nil_tenant_environment(db: &Db, name: &str) {
    let provider: crate::domain::service::DbProvider = DBProvider::new(db.clone());
    let conn = provider.conn().expect("a connection for the seed write");
    OrmEnvironmentsRepository
        .create(
            &conn,
            &AccessScope::allow_all(),
            Uuid::nil(),
            NewEnvironment {
                kubeconfig_credstore_ref: None,
                credentials: std::collections::BTreeMap::new(),
                name: name.to_owned(),
                product_id: product(),
                description: None,
                kubeconfig: None,
                default_branch: None,
                is_default: false,
            },
            PersistedCredentials {
                legacy_ref: String::new(),
                credentials: Vec::new(),
                config: serde_json::json!({}),
            },
        )
        .await
        .expect("the nil-tenant row must be writable");
}

/// **One cycle is one observation on both cycle instruments, under the outcome
/// that cycle actually had.**
///
/// The first thing a missing or misplaced emission breaks. The exported names
/// are printed on failure because an empty export is a different defect from a
/// wrong count — it says the pipeline saw nothing at all.
#[tokio::test]
async fn an_observation_cycle_records_one_observation() {
    let probe = MetricsProbe::new();
    let services = build_services_tenant_scoped_with_plugin_and_metrics(
        inmem_db().await,
        port_detecting(),
        probe.adapter(),
    );
    services
        .environments
        .create_environment(
            &ctx(Uuid::new_v4()),
            pasted("environment-a", "kubeconfig-a"),
        )
        .await
        .unwrap();

    services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    let series = probe.collect();
    assert_eq!(
        series.counter(QA_ENVIRONMENTS_OBSERVATION_CYCLE),
        1,
        "one cycle, one increment; the exported names were {:?}",
        series.names()
    );
    assert_eq!(
        series.histogram_count(QA_ENVIRONMENTS_OBSERVATION_CYCLE_DURATION),
        1,
        "and the counter and its histogram move together"
    );
    assert_eq!(
        series.counter_with(
            QA_ENVIRONMENTS_OBSERVATION_CYCLE,
            &[("outcome", CycleOutcome::Completed.as_str())]
        ),
        1,
        "a cycle that reached the end of the list is completed"
    );
}

/// **A cycle that never reached its first environment is `unstarted`, not
/// `completed`.**
///
/// This is the blind spot the cycle family exists to close, and it is invisible
/// to the report: a failed listing returns an all-zero
/// `ObservationCycleReport`, byte-identical to the one a deployment with no
/// registered environments produces. Without this label an operator cannot tell
/// "nothing has been observed for an hour because the database is unreachable"
/// from "this deployment has no environments".
///
/// The failure is produced by pointing the gear at a database whose migrations
/// were never run, which is the cheapest real listing failure available: the
/// table genuinely does not exist, so the repository's own error path is the one
/// under test rather than an injected one.
#[tokio::test]
async fn a_cycle_that_cannot_list_its_environments_is_unstarted_not_completed() {
    let probe = MetricsProbe::new();
    let services = build_services_with_plugin_port_and_metrics(
        unmigrated_db().await,
        Arc::new(TenantScopedAuthZ),
        Arc::new(RecordingCredStore::new()),
        Arc::new(NoopRunnerSecretWriter),
        port_detecting(),
        Some(probe.adapter()),
        crate::config::QaEnvironmentsConfig::default().max_variables,
    );

    let report = services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    assert_eq!(
        report.attempted, 0,
        "premise: the listing really did fail, so nothing was attempted"
    );
    let series = probe.collect();
    assert_eq!(
        series.counter_with(
            QA_ENVIRONMENTS_OBSERVATION_CYCLE,
            &[("outcome", CycleOutcome::Unstarted.as_str())]
        ),
        1,
        "a cycle that could not enumerate anything is unstarted"
    );
    assert_eq!(
        series.counter_with(
            QA_ENVIRONMENTS_OBSERVATION_CYCLE,
            &[("outcome", CycleOutcome::Completed.as_str())]
        ),
        0,
        "and it is emphatically not a completed one, which is what an all-zero report \
         alone would read as"
    );
}

/// An in-memory database with **no** migrations run, so every query against the
/// gear's tables fails the way a real outage would.
async fn unmigrated_db() -> Db {
    connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("failed to connect to in-memory sqlite database")
}

/// **A cancelled cycle is not a completed one.**
///
/// `run_observation_cycle`'s doc calls a cancelled cycle a designed path rather
/// than a failure, and its partial report is deliberately ambiguous between "the
/// gear has this many environments" and "this call was cut short". Folding it
/// into `completed` would push that ambiguity into the series too: a shutdown's
/// truncated duration and truncated `attempted` would be mixed into the
/// distribution of cycles that ran to the end.
#[tokio::test]
async fn a_cancelled_cycle_is_not_a_completed_one() {
    let probe = MetricsProbe::new();
    let services = build_services_tenant_scoped_with_plugin_and_metrics(
        inmem_db().await,
        port_detecting(),
        probe.adapter(),
    );
    services
        .environments
        .create_environment(
            &ctx(Uuid::new_v4()),
            pasted("environment-a", "kubeconfig-a"),
        )
        .await
        .unwrap();

    let cancel = CancellationToken::new();
    cancel.cancel();
    let report = services.environments.run_observation_cycle(&cancel).await;

    assert_eq!(
        report.attempted, 0,
        "premise: the cancel check is at the top of the loop, so nothing was attempted"
    );
    let series = probe.collect();
    assert_eq!(
        series.counter_with(
            QA_ENVIRONMENTS_OBSERVATION_CYCLE,
            &[("outcome", CycleOutcome::Cancelled.as_str())]
        ),
        1
    );
    assert_eq!(
        series.counter_with(
            QA_ENVIRONMENTS_OBSERVATION_CYCLE,
            &[("outcome", CycleOutcome::Completed.as_str())]
        ),
        0
    );
}

/// **The environment counter carries the report's own numbers, and carries
/// them under the right labels.**
///
/// The property the plan asks for: the series and the `debug!` line the ticker
/// writes are driven from the same two fields, so they cannot disagree. It is
/// asserted against `report` itself rather than against literals, so a fixture
/// that grew an environment cannot make this test pass by coincidence.
///
/// The zero-tenant environment is what makes `failed` non-zero without needing
/// an injected fault: `run_observation_cycle` skips a row carrying a nil tenant
/// id rather than observing it under an unscoped identity.
#[tokio::test]
async fn the_environment_counter_carries_the_reports_own_counts() {
    let probe = MetricsProbe::new();
    let db = inmem_db().await;
    let services = build_services_tenant_scoped_with_plugin_and_metrics(
        db.clone(),
        port_detecting(),
        probe.adapter(),
    );
    let tenant = Uuid::new_v4();
    for name in ["environment-a", "environment-b"] {
        services
            .environments
            .create_environment(&ctx(tenant), pasted(name, name))
            .await
            .unwrap();
    }
    seed_nil_tenant_environment(&db, "environment-nil").await;

    let report = services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    assert_eq!(
        (report.observed, report.failed),
        (2, 1),
        "premise: two observable environments and one the cycle must skip"
    );
    let series = probe.collect();
    assert_eq!(
        series.counter_with(
            QA_ENVIRONMENTS_OBSERVATION,
            &[("outcome", EnvironmentOutcome::Observed.as_str())]
        ),
        u64::from(report.observed),
        "the observed series is the report's own observed count"
    );
    assert_eq!(
        series.counter_with(
            QA_ENVIRONMENTS_OBSERVATION,
            &[("outcome", EnvironmentOutcome::Failed.as_str())]
        ),
        u64::from(report.failed),
        "and the failed series is the report's own failed count, not a transposition \
         of the other one"
    );
    assert_eq!(
        series.counter(QA_ENVIRONMENTS_OBSERVATION),
        u64::from(report.attempted),
        "and the two together are attempted, which is why it has no series of its own"
    );
}

/// **An environment skipped for a nil tenant id is counted and not timed.**
///
/// The one population the two environment families deliberately differ by.
/// Nothing was contacted for that row, so there is no observation latency to
/// record and a near-zero sample would drag down the quantile the histogram
/// exists to report — the same reason qa-runs excludes its inert ingest event.
/// Asserting the *difference* rather than each count separately is what makes
/// the exclusion falsifiable: recording a zero sample would leave both families
/// at three and this assertion is the only one that would notice.
#[tokio::test]
async fn an_environment_skipped_for_a_nil_tenant_is_counted_but_not_timed() {
    let probe = MetricsProbe::new();
    let db = inmem_db().await;
    let services = build_services_tenant_scoped_with_plugin_and_metrics(
        db.clone(),
        port_detecting(),
        probe.adapter(),
    );
    services
        .environments
        .create_environment(
            &ctx(Uuid::new_v4()),
            pasted("environment-a", "kubeconfig-a"),
        )
        .await
        .unwrap();
    seed_nil_tenant_environment(&db, "environment-nil").await;

    let report = services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    assert_eq!(report.attempted, 2, "premise: both rows were reached");
    let series = probe.collect();
    assert_eq!(
        series.counter(QA_ENVIRONMENTS_OBSERVATION),
        2,
        "both are counted, the skipped one as failed"
    );
    assert_eq!(
        series.histogram_count(QA_ENVIRONMENTS_OBSERVATION_DURATION),
        1,
        "and only the one that was actually contacted is timed"
    );
}

/// **Every environment is timed under the class its own observation reached.**
///
/// The family the plan calls the one that matters, doing the thing it exists
/// for: three environments in one cycle, three different classes, each on its
/// own series. A single merged series — an adapter that hard-coded the label, or
/// a call site that classified once and reused the answer — would still export
/// three samples and would answer "is one cluster hanging or are a hundred
/// slow?" with a shrug.
///
/// `KeyedPlugin` answers per kubeconfig document, so the mapping holds
/// regardless of the order the cycle visits the rows in.
#[tokio::test]
async fn every_environment_is_timed_under_its_own_class() {
    let probe = MetricsProbe::new();
    let services = build_services_tenant_scoped_with_plugin_and_metrics(
        inmem_db().await,
        port_keyed(HashMap::from([
            (b"kubeconfig-ok".to_vec(), detected()),
            (
                b"kubeconfig-gone".to_vec(),
                failed_with(FailureClass::Unreachable),
            ),
            (
                b"kubeconfig-denied".to_vec(),
                failed_with(FailureClass::AuthRejected),
            ),
        ])),
        probe.adapter(),
    );
    let tenant = Uuid::new_v4();
    for document in ["kubeconfig-ok", "kubeconfig-gone", "kubeconfig-denied"] {
        services
            .environments
            .create_environment(&ctx(tenant), pasted(document, document))
            .await
            .unwrap();
    }

    let report = services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    assert_eq!(
        (report.observed, report.failed),
        (3, 0),
        "premise: a plugin-reported failure is a persisted observation, not a failed one"
    );
    let series = probe.collect();
    for class in [
        ObservationClass::Detected,
        ObservationClass::Unreachable,
        ObservationClass::AuthRejected,
    ] {
        assert_eq!(
            series.histogram_count_with(
                QA_ENVIRONMENTS_OBSERVATION_DURATION,
                &[("class", class.as_str())]
            ),
            1,
            "exactly one environment reached {}, and it must be timed under that class \
             and no other",
            class.as_str()
        );
    }
}

/// **A refused observation is told apart from a detected one, and from a
/// broken gear.**
///
/// The value this pair exists for: `refused` in the observation cycle almost
/// always means the deployment's PDP has no policy for this gear's system
/// actor, which is fixed by authoring policy. Labelling it `failed` would page
/// somebody for a configuration gap; labelling it `detected` would hide it
/// entirely, because a refused observation persists nothing and leaves the
/// environment's row saying whatever the last successful cycle wrote.
///
/// The rows are seeded through a permissive container and observed through a
/// denying one over the **same** database, because the cross-tenant enumeration
/// at the top of the cycle does not go through the PDP at all — only the
/// per-environment observation does, which is exactly the seam under test.
#[tokio::test]
async fn a_refused_observation_is_not_a_detected_one() {
    let db = inmem_db().await;
    let seeding = build_services_tenant_scoped_with_plugin(db.clone(), port_detecting());
    seeding
        .environments
        .create_environment(
            &ctx(Uuid::new_v4()),
            pasted("environment-a", "kubeconfig-a"),
        )
        .await
        .unwrap();

    let probe = MetricsProbe::new();
    let denied = build_services_with_plugin_port_and_metrics(
        db,
        Arc::new(DenyAllAuthZ),
        Arc::new(RecordingCredStore::new()),
        Arc::new(NoopRunnerSecretWriter),
        port_detecting(),
        Some(probe.adapter()),
        crate::config::QaEnvironmentsConfig::default().max_variables,
    );

    let report = denied
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    assert_eq!(
        (report.attempted, report.failed),
        (1, 1),
        "premise: the row was reached and its observation was refused"
    );
    let series = probe.collect();
    assert_eq!(
        series.histogram_count_with(
            QA_ENVIRONMENTS_OBSERVATION_DURATION,
            &[("class", ObservationClass::Refused.as_str())]
        ),
        1,
        "a PDP denial is a refusal"
    );
    assert_eq!(
        series.histogram_count_with(
            QA_ENVIRONMENTS_OBSERVATION_DURATION,
            &[("class", ObservationClass::Failed.as_str())]
        ),
        0,
        "and not this gear's own failure, which is the series an alert fires on"
    );
    assert_eq!(
        series.counter_with(
            QA_ENVIRONMENTS_OBSERVATION,
            &[("outcome", EnvironmentOutcome::Failed.as_str())]
        ),
        1,
        "and it still rolls up to the report's failed count, because nothing was persisted"
    );
}

/// An adapter that panics on every call — the shape a poisoned instrument lock
/// takes — plus a count of how many times it was reached.
struct PanickingMetrics {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl ObservationMetrics for PanickingMetrics {
    fn observation_cycle(&self, _outcome: CycleOutcome, _duration: std::time::Duration) {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        panic!("the adapter is broken");
    }

    fn cycle_environments(&self, _outcome: EnvironmentOutcome, _count: u32) {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        panic!("the adapter is broken");
    }

    fn environment_observed(&self, _class: ObservationClass, _duration: std::time::Duration) {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        panic!("the adapter is broken");
    }
}

/// **A broken adapter neither fails the cycle nor is called twice.**
///
/// Both halves of `domain::service::emit`'s contract, in one test, because they
/// fail in different ways: without `catch_unwind` the cycle panics outright,
/// and without the latch every later emission panics again and the default
/// panic hook writes a line to stderr each time — which is the log flood the
/// "never log per emission" constraint is about, arriving through the back
/// door.
///
/// The premise assertion matters: a cycle that observed nothing would satisfy
/// "did not fail" while never reaching an emission at all.
///
/// This test prints a panic backtrace even when it passes. The default hook
/// runs before `catch_unwind` returns, so the message reaches stderr; silencing
/// it would mean installing a custom hook every other test in the process would
/// then see.
#[tokio::test]
async fn a_broken_metrics_adapter_does_not_fail_an_observation_cycle() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let services = build_services_tenant_scoped_with_plugin_and_metrics(
        inmem_db().await,
        port_detecting(),
        Arc::new(PanickingMetrics {
            calls: Arc::clone(&calls),
        }),
    );
    services
        .environments
        .create_environment(
            &ctx(Uuid::new_v4()),
            pasted("environment-a", "kubeconfig-a"),
        )
        .await
        .unwrap();

    let first = services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;
    let second = services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    assert_eq!(
        (first.attempted, first.observed),
        (1, 1),
        "the cycle must do its work with a broken adapter, not merely survive"
    );
    assert_eq!(
        first, second,
        "and the second cycle, running through a latched-off adapter, must be identical"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the first panic latches the service off; a second call is the log flood the \
         latch exists to prevent"
    );
}

/// **A gear with no metrics pipeline configured behaves exactly as an unmetered
/// one.**
///
/// The constraint stated as an equality rather than as an absence: the same
/// fixture is cycled twice over two databases, once through the real
/// `build_default_adapter` (with no meter provider installed anywhere in the
/// process, which is the production boot posture when telemetry is off) and
/// once with no adapter at all, and the two reports must be equal.
///
/// A test that only asserted the metered cycle succeeded would pass against a
/// cycle that did nothing, which is why the premise is asserted too.
#[tokio::test]
async fn a_cycle_with_no_pipeline_configured_behaves_exactly_as_an_unmetered_one() {
    async fn cycle(metrics: bool) -> crate::domain::service::environments::ObservationCycleReport {
        let db = inmem_db().await;
        let services = if metrics {
            build_services_tenant_scoped_with_plugin_and_metrics(
                db,
                port_detecting(),
                crate::infra::metrics::build_default_adapter(),
            )
        } else {
            build_services_tenant_scoped_with_plugin(db, port_detecting())
        };
        let tenant = Uuid::new_v4();
        for name in ["environment-a", "environment-b"] {
            services
                .environments
                .create_environment(&ctx(tenant), pasted(name, name))
                .await
                .unwrap();
        }
        services
            .environments
            .run_observation_cycle(&CancellationToken::new())
            .await
    }

    let metered = cycle(true).await;
    let unmetered = cycle(false).await;

    assert_eq!(
        metered.observed, 2,
        "premise: the cycle really did observe both environments"
    );
    assert_eq!(
        metered, unmetered,
        "installing the production adapter with no pipeline behind it must not change \
         a single count"
    );
}
