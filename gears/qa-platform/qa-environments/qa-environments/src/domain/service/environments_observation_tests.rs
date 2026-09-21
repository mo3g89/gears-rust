//! `EnvironmentsService::run_observation_cycle` tests (Task 8's ticker body),
//! against a REAL in-memory `SQLite` database and real test doubles for
//! credstore and the runner-`Secret` writer — the same tier
//! `environments_kubeconfig_tests`
//! and `environments_tests` use, for the same reasons.
//!
//! Task 15 replaced the observer call inside `observe_environment` with the
//! product plugin, so the doubles here are plugins
//! ([`crate::test_support::ScriptedPlugin`], [`crate::test_support::KeyedPlugin`])
//! reached through a [`crate::domain::ports::ProductPluginPort`] double. The
//! non-plugin doubles that remain are
//! [`crate::domain::ports::RunnerSecretWriter`]s, for D4's runner-`Secret`
//! write — the only half of the old `PlatformObserver` port Task 19 kept.
//!
//! # Where the three `VPADM_NAMESPACE` tests went
//!
//! This module used to cover Addition 1 (per-environment `VPADM_NAMESPACE`
//! resolution) through `pick_vpadm_namespace`/`vpadm_namespace_for`. Task 15
//! **deleted both**: a plugin reads its own non-secret credential fields out
//! of `EnvironmentHandle::config`, so the gear no longer resolves one
//! product's variable on its behalf (ruling D-9). The property those three
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

use std::sync::{Arc, Mutex};

use qa_environments_sdk::{
    CredentialMaterial, CredentialSubmission, EnvironmentPatch, NewEnvironment,
};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

use std::collections::HashMap;

use qa_product_sdk::descriptor::FieldKind;
use qa_product_sdk::observation::{
    FailureClass, HealthOutcome, ObservationOutcome, ObservedAttrs, PluginFailure,
    PluginObservation,
};

use crate::domain::ports::{ProductPluginPort, RunnerSecretWriter};
use crate::test_support::{
    FailingSecretObserver, FixedPluginPort, KeyedPlugin, RecordingCredStore,
    RecordingSecretObserver, ScriptedPlugin, SelectivelyFailingSecretObserver,
    TruncatingSecretObserver, build_services_tenant_scoped_with_observer,
    build_services_tenant_scoped_with_observer_and_port, build_services_tenant_scoped_with_plugin,
    build_services_tenant_scoped_with_plugin_and_credstore, ctx, field, inmem_db,
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
/// *before* any `ObservationOutcome` existed — and nothing was persisted, so
/// the environment read "not yet observed" forever while the reason lived only in
/// a log line. Cluster-health spec section 8; measured on the remote, where
/// seven of eight environments sat in exactly that state.
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
// on its behalf is the coupling this plan exists to remove (ruling D-9).
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

// ---------------------------------------------------------------------------
// Addition 2: a 409 from the self-heal names the environment and the fix
// ---------------------------------------------------------------------------

/// A shared writer that appends every byte `tracing-subscriber` emits into
/// one buffer. A raw buffer rather than `tracing-test`, for the reason
/// `Cargo.toml`'s `tracing-subscriber` entry records: `tracing-test` keeps
/// only captured lines containing the test's span name, so a multi-line field
/// value — the shape a leaked kubeconfig has — survives capture only as its
/// first line.
#[derive(Clone)]
struct RawBuffer(Arc<Mutex<Vec<u8>>>);

impl RawBuffer {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for RawBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for RawBuffer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The self-heal step's failure must reach the log **with the environment
/// attached**, and must carry whatever operator guidance the observer's own
/// error text already has (`infra::runner_secret_writer::describe_apply_failure`
/// builds that text for a real `409`; this test only proves the ticker's
/// logging does not throw the environment's identity away on the way to the
/// log — see `EnvironmentsService::self_heal_kubeconfig_secret`'s own doc for the
/// full argument).
#[tokio::test]
async fn a_self_heal_failure_is_logged_with_the_environment_attached() {
    let conflict_message = "secret/qa-platform-x in namespace argo already exists and is not \
                             owned by this writer's field manager (\"qa-environments\"), so \
                             server-side apply was refused with a 409 Conflict; delete it so \
                             qa-environments can take ownership on the next cycle";
    let observer = Arc::new(FailingSecretObserver::new(conflict_message));
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        observer as Arc<dyn RunnerSecretWriter>,
    );
    let tenant = Uuid::new_v4();

    let buffer = RawBuffer::new();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_ansi(false)
        .finish();

    // A thread-local default, held across the `.await` below — `#[tokio::test]`
    // defaults to a current-thread runtime, so the whole cycle runs on this
    // one thread and the guard sees everything it logs.
    let guard = tracing::subscriber::set_default(subscriber);

    let environment = services
        .environments
        .create_environment(
            &ctx(tenant),
            pasted("environment-with-handmade-secret", "kubeconfig-x"),
        )
        .await
        .unwrap();

    let _report = services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;
    drop(guard);

    let log = buffer.contents();
    assert!(
        log.contains("environment-with-handmade-secret"),
        "the self-heal failure must name the environment, not read as an anonymous repeating \
         error: {log}"
    );
    assert!(
        log.contains(&environment.id.to_string()),
        "must also carry the environment id: {log}"
    );
    assert!(
        log.contains("409 Conflict"),
        "must surface the observer's own operator guidance rather than swallowing it: {log}"
    );
}

// ---------------------------------------------------------------------------
// Decision D4 on create and on update, not only as a self-heal (final review I1)
// ---------------------------------------------------------------------------

/// Until 2026-08-28 what is now `ensure_runner_secret` had exactly one production
/// caller — the ticker's self-heal — so an environment created through the UI had
/// no runner `Secret` until the next cycle (default `poll_interval_seconds:
/// 300`, floor 60). That gap IS the ~5-minute `FailedMount` window D4 exists
/// to eliminate, and the spec (§4.6) said "on create, on update, and as a
/// self-heal" the whole time.
///
/// The assertion is at the port because there is nowhere else it could be:
/// the `Secret` is written into the **Argo** cluster, so it appears in no
/// row, no DTO and no response.
#[tokio::test]
async fn creating_an_environment_materialises_its_runner_secret_immediately() {
    let observer = Arc::new(RecordingSecretObserver::new());
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn RunnerSecretWriter>,
    );
    let tenant = Uuid::new_v4();

    let environment = services
        .environments
        .create_environment(
            &ctx(tenant),
            pasted("created-in-the-ui", "kubeconfig-alpha"),
        )
        .await
        .unwrap();

    let writes = observer.secret_writes();
    assert_eq!(
        writes.len(),
        1,
        "create must write the runner Secret exactly once, not wait for the ticker: {writes:?}"
    );
    assert_eq!(
        writes[0].0, environment.credentials[0].credstore_ref,
        "the Secret must be keyed on the reference the row ended up with, so its derived name \
         matches what qa-runs' executor mounts"
    );
    assert_eq!(
        writes[0].1,
        b"kubeconfig-alpha".to_vec(),
        "and must carry the material that was actually stored, not a placeholder"
    );
}

/// A registration that names an existing credstore reference instead of
/// pasting a document is the same situation from the runner's point of view:
/// the pod still mounts a `Secret` derived from that reference, and it still
/// has to exist before the first run.
#[tokio::test]
async fn creating_an_environment_by_reference_also_materialises_its_runner_secret() {
    let existing = "team-a-prod-cluster";
    let credstore = Arc::new(RecordingCredStore::new());
    credstore.seed(existing, "kubeconfig-by-reference");

    let observer = Arc::new(RecordingSecretObserver::new());
    let services = crate::test_support::build_services_full(
        inmem_db().await,
        Arc::new(crate::test_support::TenantScopedAuthZ),
        Arc::clone(&credstore) as Arc<dyn credstore_sdk::CredStoreClientV1>,
        Arc::clone(&observer) as Arc<dyn RunnerSecretWriter>,
        crate::config::QaEnvironmentsConfig::default().max_variables,
    );
    let tenant = Uuid::new_v4();

    services
        .environments
        .create_environment(
            &ctx(tenant),
            by_reference("registered-by-reference", existing),
        )
        .await
        .unwrap();

    let writes = observer.secret_writes();
    assert_eq!(
        writes.len(),
        1,
        "a reference-registered environment needs the Secret too: {writes:?}"
    );
    assert_eq!(writes[0].0, existing);
    assert_eq!(writes[0].1, b"kubeconfig-by-reference".to_vec());
}

/// Rotating a kubeconfig without re-applying the `Secret` is worse than never
/// having written one: the runner mounts the OLD material happily and then
/// fails against the environment's cluster, which reads as a broken environment
/// rather than as a stale credential.
#[tokio::test]
async fn replacing_an_environments_kubeconfig_rewrites_its_runner_secret() {
    let observer = Arc::new(RecordingSecretObserver::new());
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn RunnerSecretWriter>,
    );
    let tenant = Uuid::new_v4();

    let environment = services
        .environments
        .create_environment(
            &ctx(tenant),
            pasted("rotating-environment", "kubeconfig-old"),
        )
        .await
        .unwrap();

    let updated = services
        .environments
        .update_environment(
            &ctx(tenant),
            environment.id,
            EnvironmentPatch {
                kubeconfig: Some(CredentialMaterial::new("kubeconfig-new".to_owned())),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap();

    let writes = observer.secret_writes();
    assert_eq!(
        writes.len(),
        2,
        "create wrote one, the rotation must write the second: {writes:?}"
    );
    assert_eq!(
        writes[1].0, updated.credentials[0].credstore_ref,
        "the second write must use the NEW reference the row now carries"
    );
    assert_eq!(
        writes[1].1,
        b"kubeconfig-new".to_vec(),
        "and the NEW material - a Secret still holding the old bytes is the failure this exists \
         to prevent"
    );
}

/// The narrow half of the same rule: a patch that does not mention the
/// kubeconfig changes neither the `Secret`'s name (derived from the credstore
/// reference) nor its contents, so re-applying it would be a pointless
/// round-trip into another cluster on every rename.
#[tokio::test]
async fn a_patch_that_does_not_touch_the_kubeconfig_writes_no_secret() {
    let observer = Arc::new(RecordingSecretObserver::new());
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn RunnerSecretWriter>,
    );
    let tenant = Uuid::new_v4();

    let environment = services
        .environments
        .create_environment(
            &ctx(tenant),
            pasted("renamed-environment", "kubeconfig-alpha"),
        )
        .await
        .unwrap();

    services
        .environments
        .update_environment(
            &ctx(tenant),
            environment.id,
            EnvironmentPatch {
                name: Some("renamed-environment-v2".to_owned()),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        observer.secret_writes().len(),
        1,
        "only the create's write; a rename must not re-apply the Secret"
    );
}

/// The `Secret` write reaches a **different cluster** than the row it belongs
/// to, so it is the one step in create that can fail for reasons that say
/// nothing about the request. An environment whose row exists and whose `Secret`
/// is missing is recoverable — the ticker re-applies it next cycle — while
/// one that silently vanished is not, so the failure is logged and the create
/// still succeeds.
#[tokio::test]
async fn a_failed_secret_write_does_not_roll_back_the_create() {
    let observer = Arc::new(FailingSecretObserver::new(
        "secret/qa-platform-x in namespace argo already exists ... 409 Conflict",
    ));
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        observer as Arc<dyn RunnerSecretWriter>,
    );
    let tenant = Uuid::new_v4();

    let buffer = RawBuffer::new();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            pasted("secret-write-fails", "kubeconfig-alpha"),
        )
        .await;
    drop(guard);

    let environment = created.expect("a failed Secret write must not fail the create");

    // And the row really is there to be found afterwards.
    let fetched = services
        .environments
        .get_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();
    assert_eq!(fetched.id, environment.id);

    let log = buffer.contents();
    assert!(
        log.contains("secret-write-fails"),
        "the swallowed failure must still name the environment: {log}"
    );
    assert!(
        log.contains("409 Conflict"),
        "and must carry the writer's own operator guidance rather than swallowing it: {log}"
    );
    assert!(
        log.contains("create"),
        "and must say which of the three call sites it came from: {log}"
    );
}

// ---------------------------------------------------------------------------
// Every credential reaches the runner, not just the first (Task 8, VHI plan)
// ---------------------------------------------------------------------------

/// A `NewEnvironment` carrying VHI's two secrets, both as pasted material.
fn with_two_credentials(name: &str) -> NewEnvironment {
    let mut credentials = std::collections::BTreeMap::new();
    credentials.insert(
        "ssh_private_key".to_owned(),
        CredentialSubmission::Material(CredentialMaterial::new(
            "-----BEGIN OPENSSH PRIVATE KEY-----\nx\n".to_owned(),
        )),
    );
    credentials.insert(
        "vinfra_password".to_owned(),
        CredentialSubmission::Material(CredentialMaterial::new("hunter2".to_owned())),
    );
    NewEnvironment {
        kubeconfig_credstore_ref: None,
        credentials,
        name: name.to_owned(),
        product_id: product(),
        description: None,
        kubeconfig: None,
        default_branch: None,
        is_default: false,
    }
}

/// A schema with no required *secret* at all -- one required, non-secret
/// field and nothing else.
///
/// **Why a create can only reach zero `credentials` this way.** Submitting
/// nothing is refused outright, unconditionally, before any plugin is
/// consulted: `environments_credentials_tests.rs`'s
/// `a_create_with_no_credential_at_all_is_refused` pins exactly that
/// ("an environment needs at least one credential"). So the only way a stored
/// row ends up with an empty `credentials` is to submit a field the plugin
/// then classifies as non-secret -- it lands in `Environment::config`
/// instead, which is the case `materialise_runner_secret`'s
/// empty-`credentials` guard exists for.
fn port_config_only() -> Arc<dyn ProductPluginPort> {
    Arc::new(FixedPluginPort::new(Arc::new(
        ScriptedPlugin::vhp_shaped(detected("7.4.0")).with_credential_schema(vec![field(
            "notes",
            FieldKind::Text,
            true,
            None,
        )]),
    )))
}

/// A create submitting only the one field [`port_config_only`]'s plugin
/// classifies as non-secret.
fn config_only(name: &str) -> NewEnvironment {
    let mut credentials = std::collections::BTreeMap::new();
    credentials.insert(
        "notes".to_owned(),
        CredentialSubmission::Material(CredentialMaterial::new("no secret here".to_owned())),
    );
    NewEnvironment {
        kubeconfig_credstore_ref: None,
        credentials,
        name: name.to_owned(),
        product_id: product(),
        description: None,
        kubeconfig: None,
        default_branch: None,
        is_default: false,
    }
}

/// A port resolving every product to a VHI-shaped plugin.
fn port_vhi_shaped() -> Arc<dyn ProductPluginPort> {
    Arc::new(FixedPluginPort::new(Arc::new(ScriptedPlugin::vhi_shaped(
        detected("7.4.0"),
    ))))
}

/// Both credentials reach the writer, **each paired with its own material**.
///
/// A count alone -- which is all this asserted until 2026-09-09 -- passes
/// against an implementation that writes `credentials[0]` twice, and against
/// one that pairs reference A with material B. Neither is hypothetical: both
/// are one transposed index away in a loop that carries a reference and a
/// resolved value side by side, and both produce a run that mounts the wrong
/// bytes at the right path with nothing to notice. `RecordingSecretObserver`
/// records the bytes, so the pairing is assertable and is what is asserted.
///
/// Sorted before comparison: the order credentials are iterated in is the
/// row's, and pinning it here would be pinning something this test is not
/// about.
#[tokio::test]
async fn an_environment_with_two_credentials_materialises_both_secrets() {
    let observer = Arc::new(RecordingSecretObserver::new());
    let services = build_services_tenant_scoped_with_observer_and_port(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn RunnerSecretWriter>,
        port_vhi_shaped(),
    );

    let environment = services
        .environments
        .create_environment(&ctx(Uuid::new_v4()), with_two_credentials("vhi-one"))
        .await
        .unwrap();

    let writes = observer.secret_writes();
    assert_eq!(
        writes.len(),
        2,
        "an environment whose plugin declares two secrets needs BOTH in the Argo \
         namespace; one of them missing is a FailedMount on every run against it: {writes:?}"
    );

    // The references written are exactly the two the row ended up with --
    // not one of them twice.
    let mut written_refs: Vec<String> = writes.iter().map(|(r, _)| r.clone()).collect();
    written_refs.sort();
    let mut row_refs: Vec<String> = environment
        .credentials
        .iter()
        .map(|c| c.credstore_ref.clone())
        .collect();
    row_refs.sort();
    assert_eq!(
        written_refs, row_refs,
        "each stored credential must be written under its own reference; a repeated \
         reference means one credential never reached the Argo cluster"
    );

    // ...and the materials written are the two documents that were submitted,
    // so no reference was paired with another's bytes.
    let mut written_materials: Vec<Vec<u8>> = writes.iter().map(|(_, m)| m.clone()).collect();
    written_materials.sort();
    let mut submitted = vec![
        b"-----BEGIN OPENSSH PRIVATE KEY-----\nx\n".to_vec(),
        b"hunter2".to_vec(),
    ];
    submitted.sort();
    assert_eq!(
        written_materials, submitted,
        "the material written under each reference must be that credential's own"
    );

    // And the pairing itself: the ssh key's reference carries the PEM, the
    // password's reference carries the password. This is the assertion the two
    // set comparisons above cannot make on their own -- both sets can be
    // right while the two are crossed.
    let reference_of = |key: &str| {
        let Some(credential) = environment.credentials.iter().find(|c| c.key == key) else {
            panic!("the row must carry a `{key}` credential");
        };
        credential.credstore_ref.clone()
    };
    let material_written_for = |reference: &str| {
        let Some((_, material)) = writes.iter().find(|(r, _)| r == reference) else {
            panic!("nothing was written for {reference}");
        };
        material.clone()
    };
    assert_eq!(
        material_written_for(&reference_of("ssh_private_key")),
        b"-----BEGIN OPENSSH PRIVATE KEY-----\nx\n".to_vec()
    );
    assert_eq!(
        material_written_for(&reference_of("vinfra_password")),
        b"hunter2".to_vec()
    );
}

/// **Rotating one credential of two rewrites both, each with its own bytes.**
///
/// The update call site was untested with more than one credential: every
/// existing update test uses the one-credential VHP shape, so an
/// implementation that still took `credentials.first()` on *this* path -- the
/// exact defect Task 8 fixed on the create path -- passed the whole suite.
/// `replaced_credential` gates the call, so a patch mentioning one credential
/// converges all of them, which is correct (the writer is idempotent by name)
/// and is what this pins.
#[tokio::test]
async fn updating_one_credential_of_two_rewrites_both_with_their_own_material() {
    let observer = Arc::new(RecordingSecretObserver::new());
    let services = build_services_tenant_scoped_with_observer_and_port(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn RunnerSecretWriter>,
        port_vhi_shaped(),
    );
    let tenant = Uuid::new_v4();

    let environment = services
        .environments
        .create_environment(&ctx(tenant), with_two_credentials("vhi-rotating"))
        .await
        .unwrap();

    let mut patch_credentials = std::collections::BTreeMap::new();
    patch_credentials.insert(
        "vinfra_password".to_owned(),
        CredentialSubmission::Material(CredentialMaterial::new("hunter3-rotated".to_owned())),
    );
    let updated = services
        .environments
        .update_environment(
            &ctx(tenant),
            environment.id,
            EnvironmentPatch {
                credentials: patch_credentials,
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap();

    let writes = observer.secret_writes();
    assert_eq!(
        writes.len(),
        4,
        "two on create, two on update: an update that wrote only the rotated credential \
         would leave the other's Secret unconverged, which is the same class of bug as \
         `credentials.first()`: {writes:?}"
    );

    let reference_of = |key: &str| {
        let Some(credential) = updated.credentials.iter().find(|c| c.key == key) else {
            panic!("the row must carry a `{key}` credential");
        };
        credential.credstore_ref.clone()
    };
    let update_writes = &writes[2..];
    let material_written_for = |reference: &str| {
        let Some((_, material)) = update_writes.iter().find(|(r, _)| r == reference) else {
            panic!("the update wrote nothing for {reference}");
        };
        material.clone()
    };
    assert_eq!(
        material_written_for(&reference_of("vinfra_password")),
        b"hunter3-rotated".to_vec(),
        "the rotated credential must carry the NEW bytes"
    );
    assert_eq!(
        material_written_for(&reference_of("ssh_private_key")),
        b"-----BEGIN OPENSSH PRIVATE KEY-----\nx\n".to_vec(),
        "and the untouched one must be re-applied with its own, unchanged bytes -- not \
         with the rotated credential's"
    );
}

/// **Two credentials whose references derive one `Secret` name: the second is
/// skipped, loudly, not allowed to overwrite the first.**
///
/// The name is derived from the credstore reference by a lossy rule (see
/// `infra::runner_secret_writer::two_long_path_style_references_can_derive_one_secret_name`,
/// which pins that real, plausible references can collide under it). Before
/// this branch an environment had one credential and this was structurally
/// impossible. With two it is reachable, and the failure mode is silent: the
/// second apply overwrites the first's material and `qa-runs` mounts that one
/// object at BOTH mount paths, with no error on either gear.
///
/// [`TruncatingSecretObserver`] reproduces the collision at a length this test
/// can write references against, and records what actually reached the writer
/// -- so "skipped" is asserted as an absence at the port, which is the only
/// place it is observable.
#[tokio::test]
async fn two_credentials_deriving_one_secret_name_do_not_overwrite_each_other() {
    let db = inmem_db().await;
    let credstore = Arc::new(RecordingCredStore::new());
    let max_variables = crate::config::QaEnvironmentsConfig::default().max_variables;

    // First, learn the two references this environment actually gets.
    let recording = Arc::new(RecordingSecretObserver::new());
    let services = crate::test_support::build_services_with_plugin_port(
        db.clone(),
        Arc::new(crate::test_support::TenantScopedAuthZ),
        Arc::clone(&credstore) as Arc<dyn credstore_sdk::CredStoreClientV1>,
        Arc::clone(&recording) as Arc<dyn RunnerSecretWriter>,
        port_vhi_shaped(),
        max_variables,
    );
    let environment = services
        .environments
        .create_environment(&ctx(Uuid::new_v4()), with_two_credentials("vhi-colliding"))
        .await
        .unwrap();
    let references: Vec<String> = environment
        .credentials
        .iter()
        .map(|c| c.credstore_ref.clone())
        .collect();
    assert_eq!(references.len(), 2);

    // The references are minted UUIDs, so they share no prefix: truncating to
    // zero characters is what makes them collide. That is the collision, not a
    // property of UUIDs -- see this test's own doc for the real shape.
    let colliding = Arc::new(TruncatingSecretObserver::truncating_at(0));
    assert_eq!(
        colliding.derived_secret_name(Uuid::new_v4(), &references[0]),
        colliding.derived_secret_name(Uuid::new_v4(), &references[1]),
        "the double must actually collide these two, or this test proves nothing"
    );

    let services = crate::test_support::build_services_with_plugin_port(
        db,
        Arc::new(crate::test_support::TenantScopedAuthZ),
        credstore,
        Arc::clone(&colliding) as Arc<dyn RunnerSecretWriter>,
        port_vhi_shaped(),
        max_variables,
    );

    let buffer = RawBuffer::new();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;
    drop(guard);

    let written = colliding.written();
    assert_eq!(
        written.len(),
        1,
        "the second claimant of a derived name must be SKIPPED, not applied on top of the \
         first: {written:?}"
    );
    assert_eq!(
        written[0], references[0],
        "the first credential keeps the name it claimed"
    );

    let log = buffer.contents();
    assert!(
        log.contains("derive the same runner Secret name"),
        "a silently dropped credential is the failure this exists to prevent, so it must be \
         logged at error: {log}"
    );
    assert!(
        log.contains(&references[1]),
        "the log must name the reference that was skipped, or an operator cannot find it: \
         {log}"
    );
}

/// A one-credential environment writes exactly one `Secret`, **under the
/// reference the row carries and with the material that was stored**.
///
/// *Strengthened rather than deleted* (2026-09-09 review). As written it
/// asserted only `writes.len() == 1`, which is strictly weaker than
/// `creating_an_environment_materialises_its_runner_secret_immediately`
/// above -- that one already pins the count, the reference and the bytes for
/// the same shape. It was kept because it states a distinct rule (the loop
/// generalising to N credentials must not have changed the N=1 case) and
/// because its fixture reaches that case through the VHP-shaped plugin port
/// rather than the create path's kubeconfig sugar; deleting it would have
/// left "the VHP path is untouched" unstated. With the reference and material
/// pinned it is no longer subsumed: an implementation that wrote one Secret
/// with the wrong bytes now fails here too.
#[tokio::test]
async fn a_single_credential_environment_still_materialises_exactly_one() {
    let observer = Arc::new(RecordingSecretObserver::new());
    let services = build_services_tenant_scoped_with_observer(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn RunnerSecretWriter>,
    );

    let environment = services
        .environments
        .create_environment(&ctx(Uuid::new_v4()), pasted("vhp-one", "kubeconfig-alpha"))
        .await
        .unwrap();

    let writes = observer.secret_writes();
    assert_eq!(
        writes.len(),
        1,
        "the VHP path must be untouched by this change: {writes:?}"
    );
    assert_eq!(
        writes[0].0, environment.credentials[0].credstore_ref,
        "under the reference the row ended up with, so the derived name matches what \
         qa-runs' executor mounts"
    );
    assert_eq!(writes[0].1, b"kubeconfig-alpha".to_vec());
}

/// **A credential whose material cannot be resolved does not take its
/// siblings down with it.**
///
/// This is the `continue` in `materialise_runner_secret`'s loop -- reached
/// through `runner_secret_material` returning `None` -- and it was untested:
/// an implementation that `return`ed there instead is one keystroke away, is
/// the exact class of bug Task 8 fixed one line up (`credentials.first()`),
/// and passed the whole suite. The credential that fails is deliberately the
/// **first** in iteration order, so "the loop kept going" is the only way the
/// second one can have been written.
///
/// The failure is reached by giving a second service instance a credstore
/// holding only one of the two references the create minted: the row still
/// names both, so the resolution of the other genuinely fails the way a
/// deleted or unreadable secret would.
#[tokio::test]
async fn a_credential_whose_material_cannot_be_resolved_does_not_suppress_the_next() {
    let db = inmem_db().await;
    let first_credstore = Arc::new(RecordingCredStore::new());
    let max_variables = crate::config::QaEnvironmentsConfig::default().max_variables;

    let recording = Arc::new(RecordingSecretObserver::new());
    let services = crate::test_support::build_services_with_plugin_port(
        db.clone(),
        Arc::new(crate::test_support::TenantScopedAuthZ),
        Arc::clone(&first_credstore) as Arc<dyn credstore_sdk::CredStoreClientV1>,
        Arc::clone(&recording) as Arc<dyn RunnerSecretWriter>,
        port_vhi_shaped(),
        max_variables,
    );
    let environment = services
        .environments
        .create_environment(&ctx(Uuid::new_v4()), with_two_credentials("vhi-half-gone"))
        .await
        .unwrap();

    // The order the loop will see them in, taken from the row itself.
    let references: Vec<String> = environment
        .credentials
        .iter()
        .map(|c| c.credstore_ref.clone())
        .collect();
    let [first_ref, second_ref] = references.as_slice() else {
        panic!("this fixture stores exactly two credentials");
    };

    // A credstore that holds ONLY the second reference, so resolving the
    // first fails and the second still can be written.
    let partial_credstore = Arc::new(RecordingCredStore::new());
    partial_credstore.seed(second_ref, "hunter2");

    let observer = Arc::new(RecordingSecretObserver::new());
    let services = crate::test_support::build_services_with_plugin_port(
        db,
        Arc::new(crate::test_support::TenantScopedAuthZ),
        Arc::clone(&partial_credstore) as Arc<dyn credstore_sdk::CredStoreClientV1>,
        Arc::clone(&observer) as Arc<dyn RunnerSecretWriter>,
        port_vhi_shaped(),
        max_variables,
    );

    services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    let writes = observer.secret_writes();
    assert_eq!(
        writes.len(),
        1,
        "the resolvable credential must still have been written: an implementation that \
         returned on the first unresolvable one writes nothing here: {writes:?}"
    );
    assert_eq!(
        writes[0].0, *second_ref,
        "and it must be the one whose material was there"
    );
    assert!(
        !writes.iter().any(|(r, _)| r == first_ref),
        "the unresolvable credential must not have been written with someone else's bytes"
    );
}

/// Proves the loop does not stop at the first refusal -- **not** "two writes
/// are attempted when nothing fails" (test 1 already covers that).
///
/// # Why one environment, two service instances, sharing the DB and credstore
///
/// `credstore_ref`s are minted as `Uuid::new_v4()` (`environments.rs`'s
/// `write_generated_secret`), with no dependence on content or environment
/// name.
/// A `SelectivelyFailingSecretObserver` has to be told a reference that will
/// actually be written, or it refuses nothing and the test degenerates into
/// exactly the loose, unproven form this replaces -- one that would still
/// pass against an implementation that `break`s (or `return`s) on the first
/// `Err` instead of `continue`ing. So the failing reference and the write it
/// is meant to catch must be in the same world: this creates one environment
/// with a permissive observer to learn its two real references, then builds a
/// *second* `services` around the *same* `db` and the *same* `credstore` (so
/// the row and its already-minted credential material are both still there)
/// with a [`SelectivelyFailingSecretObserver`] targeting the first of those
/// two references, and drives D4's write path again via the self-heal
/// (`run_observation_cycle`) rather than a second, unrelated `create`.
#[tokio::test]
async fn a_failed_write_does_not_suppress_the_next_credential() {
    let db = inmem_db().await;
    let credstore = Arc::new(RecordingCredStore::new());
    let max_variables = crate::config::QaEnvironmentsConfig::default().max_variables;

    let first_pass_observer = Arc::new(RecordingSecretObserver::new());
    let services = crate::test_support::build_services_with_plugin_port(
        db.clone(),
        Arc::new(crate::test_support::TenantScopedAuthZ),
        Arc::clone(&credstore) as Arc<dyn credstore_sdk::CredStoreClientV1>,
        Arc::clone(&first_pass_observer) as Arc<dyn RunnerSecretWriter>,
        port_vhi_shaped(),
        max_variables,
    );

    let environment = services
        .environments
        .create_environment(&ctx(Uuid::new_v4()), with_two_credentials("vhi-partial"))
        .await
        .unwrap();

    let first_pass_writes = first_pass_observer.secret_writes();
    assert_eq!(
        first_pass_writes.len(),
        2,
        "the create itself must not already be dropping one, or there is nothing left to \
         prove below: {first_pass_writes:?}"
    );
    let first_ref = first_pass_writes[0].0.clone();

    // Same `db`, same `credstore`; a fresh service instance whose observer
    // refuses exactly the reference the create above actually minted, driving
    // the self-heal over the same row.
    let observer = Arc::new(SelectivelyFailingSecretObserver::failing_on(&first_ref));
    let services = crate::test_support::build_services_with_plugin_port(
        db,
        Arc::new(crate::test_support::TenantScopedAuthZ),
        credstore,
        Arc::clone(&observer) as Arc<dyn RunnerSecretWriter>,
        port_vhi_shaped(),
        max_variables,
    );

    services
        .environments
        .run_observation_cycle(&CancellationToken::new())
        .await;

    // The FULL ordered list, not a length and a membership check. Those two
    // were sound here only because the refused reference happens to be
    // attempted first: an implementation that stopped after the first failure
    // would produce a one-element list and fail the length check, but one
    // that reordered, retried, or attempted a reference twice would satisfy
    // both. Pinning the order also pins that the refusal did not consume the
    // sibling's turn.
    // The oracle is the ROW's own credential order, not the first pass's
    // write order: deriving the expectation from another run of the same loop
    // would make the two flip together, and a reordering mutation would pass.
    let attempted = observer.attempted();
    let expected: Vec<String> = environment
        .credentials
        .iter()
        .map(|c| c.credstore_ref.clone())
        .collect();
    assert_eq!(
        attempted, expected,
        "each credential is an independent decision, attempted exactly once and in the \
         row's own order; one failure must not take the others down, nor take their turn"
    );
    assert_eq!(
        attempted.first(),
        Some(&first_ref),
        "the refused reference must be the one attempted first, or this test is proving \
         the easy direction"
    );
}

/// An environment whose plugin classifies every submitted field as
/// non-secret ends up with an empty `Environment::credentials` -- see
/// [`port_config_only`] for why this is the only reachable way to get there.
/// `materialise_runner_secret` must not write anything for it.
#[tokio::test]
async fn an_environment_with_no_credentials_writes_nothing() {
    let observer = Arc::new(RecordingSecretObserver::new());
    let services = build_services_tenant_scoped_with_observer_and_port(
        inmem_db().await,
        Arc::clone(&observer) as Arc<dyn RunnerSecretWriter>,
        port_config_only(),
    );

    let environment = services
        .environments
        .create_environment(&ctx(Uuid::new_v4()), config_only("bare"))
        .await
        .unwrap();

    assert!(
        environment.credentials.is_empty(),
        "the fixture must actually reach zero stored credentials, not merely submit a field: \
         {:?}",
        environment.credentials
    );
    assert!(observer.secret_writes().is_empty());
}
