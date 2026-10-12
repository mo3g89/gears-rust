//! `EnvironmentsService::run_observation_cycle` tests (Task 8's ticker body),
//! against a REAL in-memory `SQLite` database and real test doubles for
//! credstore — the same tier
//! `environments_kubeconfig_tests`
//! and `environments_tests` use, for the same reasons.
//!
//! Task 15 replaced the observer call inside `observe_environment` with the
//! product plugin, so the doubles here are plugins
//! ([`crate::test_support::ScriptedPlugin`], [`crate::test_support::KeyedPlugin`])
//! reached through a [`crate::domain::ports::ProductPluginPort`] double.
//!
//! # Where the three `VPADM_NAMESPACE` tests went
//!
//! This module used to cover Addition 1 (per-environment `VPADM_NAMESPACE`
//! resolution) through `pick_vpadm_namespace`/`vpadm_namespace_for`. Task 15
//! **deleted both**: a plugin reads its own non-secret credential fields out
//! of `EnvironmentHandle::config`, so the gear no longer resolves one
//! product's variable on its behalf. The property those three
//! tests protected — an environment's own namespace reaching the thing that
//! observes it — is now
//! `the_config_column_reaches_the_plugin_verbatim` in
//! `observation_projection_tests`, one layer out and product-neutral.
//!
//! What that costs is recorded rather than glossed over: editing the
//! `VPADM_NAMESPACE` **variable** no longer changes where the plugin looks.
//! `m20260903_000011_environment_plugin_columns` (folded into `migrations::m20260812_000001_initial` by the docs squash) captured every override that
//! existed into `config`, and the credential form that makes `config`
//! editable is Task 22's.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use qa_environments_sdk::{CredentialMaterial, NewEnvironment};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use std::collections::HashMap;

use qa_product_sdk::observation::{
    FailureClass, HealthOutcome, ObservationOutcome, ObservedAttrs, PluginFailure,
    PluginObservation,
};

use crate::domain::ports::ProductPluginPort;
use crate::test_support::{
    FixedPluginPort, HangingPlugin, KeyedPlugin, RecordingCredStore, ScriptedPlugin,
    build_services_tenant_scoped_with_plugin,
    build_services_tenant_scoped_with_plugin_and_credstore,
    build_services_tenant_scoped_with_plugin_and_observe_timeout, ctx, inmem_db,
};

/// Every environment in this module belongs to one product, because an
/// environment that names none cannot resolve a plugin at all — a case
/// `an_environment_with_no_product_records_that_it_cannot_be_observed`
/// covers on its own, in `observation_projection_tests`.
fn product() -> Uuid {
    Uuid::from_u128(0x9001)
}

fn pasted(name: &str, document: &str) -> NewEnvironment {
    NewEnvironment {
        // The pre-plugin pair is still a REQUEST field until Task 22;
        // Task 19 dropped only the column.
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

fn by_reference(name: &str, credstore_ref: &str) -> NewEnvironment {
    NewEnvironment {
        // The pre-plugin pair is still a REQUEST field until Task 22;
        // Task 19 dropped only the column.
        kubeconfig_credstore_ref: Some(credstore_ref.to_owned()),
        credentials: std::collections::BTreeMap::new(),
        name: name.to_owned(),
        product_id: product(),
        description: None,
        kubeconfig: None,
        default_branch: None,
        is_default: false,
    }
}

/// A `Detected` outcome carrying just the version attribute — the shape a
/// plugin returns when detection succeeded and the gateway read was
/// inconclusive.
fn detected(version: &str) -> PluginObservation {
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", version);
    PluginObservation {
        environment: ObservationOutcome::Detected(attrs),
        health: HealthOutcome::NotAttempted,
    }
}

/// A failed environment half, carrying the remote's own text.
fn detection_failed(remote: &str) -> PluginObservation {
    PluginObservation {
        environment: ObservationOutcome::Failed(
            PluginFailure::classified(FailureClass::NotFound, "the target could not be read")
                .with_remote_message(remote.to_owned()),
        ),
        health: HealthOutcome::NotAttempted,
    }
}

/// A port resolving every product to one plugin that always reports
/// `detected(version)`.
fn port_detecting(version: &str) -> Arc<dyn ProductPluginPort> {
    Arc::new(FixedPluginPort::new(Arc::new(ScriptedPlugin::vhp_shaped(
        detected(version),
    ))))
}

// ---------------------------------------------------------------------------
// One environment's failure must never abort the cycle for the others
// ---------------------------------------------------------------------------

/// An environment whose kubeconfig cannot be resolved must not stop the cycle from
/// reaching the others. `report.attempted == 3` is the load-bearing assertion:
/// if the loop had returned early on the broken environment, this would be less
/// than 3 regardless of where in the (unordered) list that environment landed.
///
/// # Why the counts changed on 2026-08-29
///
/// This test used to assert `failed == 1` / `observed == 2`, because an
/// unresolvable kubeconfig made `observe_environment` return a genuine `Err`
/// *before* any `ObservationOutcome` existed — and nothing was persisted, so the
/// environment read "not yet observed" forever while the reason lived only in a
/// log line. Measured on the remote, where seven of eight environments sat in
/// exactly that state.
///
/// The resolution failure is now folded into the outcome instead, so all three
/// environments complete a pass and the broken one's reason is persisted where an
/// operator can read it. That makes it behave exactly like a *detection*
/// failure, which has always been a value rather than an error — so `observed`
/// here means "the cycle completed a pass over this environment", which is what it
/// already meant for every failed detection.
///
/// `an_environment_whose_kubeconfig_cannot_be_resolved_says_so_on_its_row` below is
/// what pins the surfacing itself. The cycle-does-not-abort property is still
/// asserted here, by `attempted`.
#[tokio::test]
async fn one_environments_failure_does_not_abort_the_cycle_for_the_others() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port_detecting("1.0.0"),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    services
        .environments
        .create_environment(&ctx(tenant), pasted("environment-a", "kubeconfig-a"))
        .await
        .unwrap();
    // Points at a credstore reference nothing ever seeded: `observe_environment`
    // fails to resolve the material and returns a genuine `Err`, before any
    // `ObservationOutcome` is even constructed.
    services
        .environments
        .create_environment(
            &ctx(tenant),
            by_reference("environment-broken", "never-seeded-in-credstore"),
        )
        .await
        .unwrap();
    services
        .environments
        .create_environment(&ctx(tenant), pasted("environment-c", "kubeconfig-c"))
        .await
        .unwrap();

    let report = services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    assert_eq!(
        report.attempted, 3,
        "the cycle must reach every environment regardless of where the broken one sits"
    );
    assert_eq!(
        report.failed, 0,
        "an unresolvable kubeconfig is now recorded as a failed observation, not an error"
    );
    assert_eq!(
        report.observed, 3,
        "every environment completes a pass, the broken one carrying its reason on the row"
    );
}

/// The surfacing this whole change exists for, and the leak it must not cause.
///
/// Two assertions carry the weight:
///
/// * the row says *why*, so the environment page stops reading "not yet observed"
///   for an environment nobody can observe; and
/// * the message does **not** contain the credstore reference. The `DomainError`
///   this replaces names the environment and its `credstore_ref` inline, and
///   `credstore_ref` is the one value banned
///   from `EnvironmentDto` — under `SharingMode::Tenant` the reference *is* a read
///   path to the kubeconfig. Formatting that error into `version_detect_error`
///   would publish it by the back door to every GET-authorized caller.
///
/// The cluster columns must also stay untouched: nothing contacted a cluster,
/// so claiming `Unreachable` would be the same fabrication the final
/// whole-branch review caught on the feature-off build.
#[tokio::test]
async fn an_environment_whose_kubeconfig_cannot_be_resolved_says_so_on_its_row() {
    const REF: &str = "never-seeded-canary-reference";

    let credstore = Arc::new(RecordingCredStore::new());
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port_detecting("1.0.0"),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let broken = services
        .environments
        .create_environment(&ctx(tenant), by_reference("environment-broken", REF))
        .await
        .unwrap();

    services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    let row = services
        .environments
        .get_environment(&ctx(tenant), broken.id)
        .await
        .expect("the environment must still be readable");

    let message = row
        .version_detect_error
        .as_deref()
        .expect("an unresolvable kubeconfig must be recorded, not swallowed into a log line");
    assert!(
        message.contains("credential store"),
        "an operator must be told what is wrong, got: {message}"
    );
    assert!(
        !message.contains(REF),
        "the credstore reference must never reach a published column, got: {message}"
    );
    assert!(
        row.version_detected_at.is_some(),
        "the attempt is still an attempt and must be stamped"
    );
}

/// The brief's own illustrative scenario: three environments, the middle one's
/// *observer* reports `Failed`. Per `observe_environment`'s documented contract
/// a detection failure is a **value**, not an error — so all three still come
/// back `Ok` from `observe_environment`, and this test's job is to prove each
/// one's own outcome (not some other environment's) is what actually landed in
/// its row. `KeyedPlugin` keys on the kubeconfig content specifically so this
/// holds regardless of the order `run_observation_cycle` visits them in. (It
/// replaced an observer double of the same shape when Task 15 moved
/// observation onto the plugin; the property is unchanged.)
#[tokio::test]
async fn the_middle_environments_failed_detection_still_lets_the_outer_two_be_recorded() {
    let plugin = Arc::new(KeyedPlugin::new(HashMap::from([
        (b"kubeconfig-a".to_vec(), detected("1.2.3")),
        (
            b"kubeconfig-b".to_vec(),
            detection_failed("namespace \"virtuozzo\" not found"),
        ),
        (b"kubeconfig-c".to_vec(), detected("4.5.6")),
    ])));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        Arc::new(FixedPluginPort::new(plugin)),
    );
    let tenant = Uuid::new_v4();

    let a = services
        .environments
        .create_environment(&ctx(tenant), pasted("environment-a", "kubeconfig-a"))
        .await
        .unwrap();
    let b = services
        .environments
        .create_environment(&ctx(tenant), pasted("environment-b", "kubeconfig-b"))
        .await
        .unwrap();
    let c = services
        .environments
        .create_environment(&ctx(tenant), pasted("environment-c", "kubeconfig-c"))
        .await
        .unwrap();

    let report = services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    assert_eq!(report.attempted, 3);
    assert_eq!(
        report.observed, 3,
        "a Failed *outcome* is still a successfully-persisted Ok, matching \
         observe_environment's own documented contract"
    );
    assert_eq!(report.failed, 0);

    let refreshed_a = services
        .environments
        .get_environment(&ctx(tenant), a.id)
        .await
        .unwrap();
    assert_eq!(refreshed_a.observed_version.as_deref(), Some("1.2.3"));
    assert!(refreshed_a.version_detect_error.is_none());

    let refreshed_b = services
        .environments
        .get_environment(&ctx(tenant), b.id)
        .await
        .unwrap();
    assert_eq!(
        refreshed_b.version_detect_error.as_deref(),
        Some("the target could not be read: namespace \"virtuozzo\" not found"),
        "the classified detail and the remote's own text, in that order -- \
         `PluginFailure`'s Display, which is what makes a failure fixable \
         without letting a plugin format anything of its own"
    );

    let refreshed_c = services
        .environments
        .get_environment(&ctx(tenant), c.id)
        .await
        .unwrap();
    assert_eq!(refreshed_c.observed_version.as_deref(), Some("4.5.6"));
    assert!(refreshed_c.version_detect_error.is_none());
}

// ---------------------------------------------------------------------------
// A shutdown does not wait for every environment to be observed
// ---------------------------------------------------------------------------

/// `run_observation_cycle` used to take no token, so its per-environment loop
/// could not be interrupted. Each iteration is a network round trip to that
/// environment's own cluster, so at `cpt-cf-qa-nfr-scale`'s 100 environments a
/// shutdown waited for 100 round trips -- the whole budget, spent after the
/// operator asked it to stop. The ticker's own `select!` already held a
/// token; the loop body just could not see it. Review finding #32.
///
/// Twenty environments, a plugin that cancels the token right after its first
/// `observe` call returns, and the assertion is `attempted < 20`: whatever the
/// exact count, a cancelled cycle must stop somewhere in the middle rather
/// than pay for the whole pass.
#[tokio::test]
async fn a_cancelled_observation_cycle_stops_between_environments() {
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detected("1.0.0")));
    let resolver = Arc::new(FixedPluginPort::new(Arc::clone(&plugin) as Arc<_>));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        Arc::clone(&resolver) as Arc<dyn ProductPluginPort>,
    );
    let tenant = Uuid::new_v4();

    for i in 0..20 {
        services
            .environments
            .create_environment(
                &ctx(tenant),
                pasted(&format!("environment-{i}"), &format!("kubeconfig-{i}")),
            )
            .await
            .unwrap();
    }

    let cancel = CancellationToken::new();
    plugin.cancel_after_first_observe(cancel.clone());

    let report = services.environments.run_observation_cycle(&cancel).await;

    assert!(
        report.attempted < 20,
        "a cancelled cycle must stop early; it attempted all {} of them",
        report.attempted
    );
}

/// The discriminating case the test above cannot cover: a token already
/// cancelled *before* the cycle is ever entered.
///
/// Cancelling from inside the first `observe` call (as the test above does)
/// cannot distinguish a check at the top of the loop from one at the bottom:
/// both see the token fire during the first iteration and both stop after
/// exactly one, so `attempted < 20` passes either way. Pre-cancelling proves
/// the placement instead -- a top-of-loop check never starts the first
/// iteration at all (`attempted == 0`, no round trip paid), while a
/// bottom-of-loop check would still pay for one full iteration before ever
/// consulting the token (`attempted == 1`). Fix round 1 confirmed the two
/// tests are not redundant by moving the check to the bottom of the loop: this
/// one went red while `a_cancelled_observation_cycle_stops_between_environments`
/// stayed green.
#[tokio::test]
async fn a_cycle_cancelled_before_it_starts_attempts_nothing() {
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detected("1.0.0")));
    let resolver = Arc::new(FixedPluginPort::new(Arc::clone(&plugin) as Arc<_>));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        Arc::clone(&resolver) as Arc<dyn ProductPluginPort>,
    );
    let tenant = Uuid::new_v4();

    services
        .environments
        .create_environment(&ctx(tenant), pasted("environment-a", "kubeconfig-a"))
        .await
        .unwrap();

    let cancel = CancellationToken::new();
    cancel.cancel();

    let report = services.environments.run_observation_cycle(&cancel).await;

    assert_eq!(
        report.attempted, 0,
        "a cycle cancelled before it starts must not attempt even the first environment"
    );
    assert!(
        plugin.handles().is_empty(),
        "and must never reach the plugin -- no round trip paid for an environment this cycle \
         never gets to"
    );
}

// ---------------------------------------------------------------------------
// Addition 1's three VPADM_NAMESPACE tests were DELETED by Task 15
// ---------------------------------------------------------------------------
//
// They asserted that `vpadm_namespace_for` resolved an environment's own
// `VPADM_NAMESPACE` variable (explicit / absent / blank) and forwarded it to
// the observer. Task 15 deleted that method and `pick_vpadm_namespace` with
// it: a plugin reads its own non-secret credential fields out of
// `EnvironmentHandle::config`, and the gear resolving one product's variable
// on its behalf is the coupling this plan exists to remove.
//
// Deleted rather than adapted, because there is nothing left in this gear for
// them to test -- and recorded here rather than silently removed, because a
// reader who remembers three green tests about namespace precedence should
// find out where they went. Their successor is
// `the_config_column_reaches_the_plugin_verbatim` in
// `observation_projection_tests`, which asserts the same fact one layer out
// and without naming any product: whatever is in `config` is what the plugin
// is handed.
//
// The blank-override rule itself did not disappear either. It is enforced in
// two places now, both tested:
// `m20260903_000011_environment_plugin_columns`'s (folded into `migrations::m20260812_000001_initial` by the docs squash) backfill (which skips a
// blank variable) and the plugin's own `observe::vpadm_namespace` (which
// re-trims and re-filters whatever it finds in `config`).

/// **An observation is bounded by the platform, whatever the plugin
/// does.** The plugin never answers; `observe_environment` (the refresh
/// route's path, and the ticker's) returns within the deadline with the
/// timeout persisted as a value for **both** halves, like every other failed
/// observation, and the abandoned call counted under the `timeout` class.
#[tokio::test]
async fn an_observation_that_outlives_its_deadline_is_recorded_as_a_timeout() {
    use crate::domain::metrics::QA_ENVIRONMENTS_PLUGIN_CALL;
    use crate::domain::ports::metrics::PluginCallClass;
    use crate::infra::metrics::probe::MetricsProbe;

    let probe = MetricsProbe::new();
    let services = build_services_tenant_scoped_with_plugin_and_observe_timeout(
        inmem_db().await,
        Arc::new(FixedPluginPort::new(Arc::new(HangingPlugin))),
        probe.adapter(),
        std::time::Duration::from_millis(50),
    );
    let tenant = Uuid::new_v4();
    let created = services
        .environments
        .create_environment(&ctx(tenant), pasted("environment-hang", "kubeconfig-hang"))
        .await
        .unwrap();

    let started = std::time::Instant::now();
    let row = services
        .environments
        .observe_environment(&ctx(tenant), created.id)
        .await
        .expect("a timed-out observation is a value, not an error");
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "took {elapsed:?}"
    );
    let message = row
        .version_detect_error
        .as_deref()
        .expect("the timeout must be recorded on the row");
    assert!(message.contains("observe_timeout_seconds"), "{message}");

    // The health half failed too: a failed read is `Unknown` with the reason,
    // and `health_checked_at` set — something did try to look.
    let health = row
        .health_detail
        .as_deref()
        .expect("the health half must record the timeout as well");
    assert!(health.contains("observe_timeout_seconds"), "{health}");
    assert_eq!(row.health_state, qa_environments_sdk::HealthState::Unknown);
    assert!(row.health_checked_at.is_some());

    let series = probe.collect();
    assert_eq!(
        series.counter_with(
            QA_ENVIRONMENTS_PLUGIN_CALL,
            &[("class", PluginCallClass::Timeout.as_str())]
        ),
        1,
        "the abandoned call is counted once, under `timeout`; the exported names were {:?}",
        series.names()
    );
}
