//! Observation through the product plugin: what reaches the plugin, and what
//! the plugin's answer writes into both column sets.
//!
//! Against a REAL in-memory `SQLite` database and real test doubles for
//! credstore and the plugin — the same tier `environments_observation_tests`
//! and `environments_kubeconfig_tests` use, for the same reason: the merge
//! rules under test are SQL, and a mock repository would assert against a
//! hand-written copy of them.
//!
//! # The two directions
//!
//! Half of Task 15's requirements are about what the **gear** puts on the
//! `EnvironmentHandle` — a resolved slot rather than a reference-only one,
//! `config` verbatim, the previous attributes or `None`. None of those is
//! visible from the outcome, the database or any DTO, so
//! [`ScriptedPlugin`]'s recorded handles are the only place they can be
//! asserted. The other half is about what the answer *writes*, and that is
//! asserted by reading the row back.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use qa_environments_sdk::{CredentialMaterial, NewEnvironment};
use qa_product_sdk::descriptor::{FieldKind, FieldRole};
use qa_product_sdk::observation::{
    FailureClass, HealthOutcome, HealthState, ObservationOutcome, ObservedAttrs, PluginFailure,
    PluginObservation,
};
use qa_product_sdk::testing::Canary;
use uuid::Uuid;

use crate::domain::ports::{PluginUnavailable, ProductPluginPort};
use crate::test_support::{
    FixedPluginPort, ScriptedPlugin, SequencedPlugin, UnavailablePluginPort,
    build_services_tenant_scoped, build_services_tenant_scoped_with_plugin, ctx, field, inmem_db,
    set_config,
};

/// The product every environment in this module belongs to.
fn product() -> Uuid {
    Uuid::from_u128(0x9001)
}

/// A pasted-kubeconfig create request, with a product so a plugin can be
/// resolved for it.
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

/// A full detection: all four role-claimed attributes plus one that claims no
/// role, which is what a real plugin returns.
fn full_detection() -> PluginObservation {
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");
    attrs.set("build", "1471");
    attrs.set("baseDomain", "https://sv.jele.io");
    attrs.set("namespace", "virtuozzo");
    PluginObservation {
        environment: ObservationOutcome::Detected(attrs),
        health: HealthOutcome::Checked {
            state: HealthState::Ok,
            detail: Some("Healthy"),
        },
    }
}

fn port(plugin: Arc<ScriptedPlugin>) -> Arc<dyn ProductPluginPort> {
    Arc::new(FixedPluginPort::new(plugin))
}

// ---------------------------------------------------------------------------
// What the plugin is asked, and what it is handed
// ---------------------------------------------------------------------------

/// One resolution per environment, each for that environment's own product.
#[tokio::test]
async fn the_cycle_resolves_the_plugin_for_the_environments_product() {
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(full_detection()));
    let resolver = Arc::new(FixedPluginPort::new(Arc::clone(&plugin) as Arc<_>));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        Arc::clone(&resolver) as Arc<dyn ProductPluginPort>,
    );
    let tenant = Uuid::new_v4();

    for name in ["a", "b", "c"] {
        services
            .environments
            .create_environment(&ctx(tenant), pasted(name, &format!("kubeconfig-{name}")))
            .await
            .unwrap();
    }

    // Since Task 18b, **create** resolves the plugin too — its credential
    // write path classifies the submitted form through it — so the resolutions
    // this test is about are the ones the cycle ADDS, not every resolution the
    // port has ever seen. Asserting the create half as well, because it is
    // free here and it is the only place that count is pinned.
    let after_creates = resolver.asked();
    assert_eq!(
        after_creates,
        vec![product(), product(), product()],
        "one resolution per create, each naming that environment's product"
    );

    let report = services.environments.run_observation_cycle().await;

    assert_eq!(report.attempted, 3);
    assert_eq!(report.observed, 3);
    assert_eq!(
        &resolver.asked()[after_creates.len()..],
        &[product(), product(), product()][..],
        "one resolution per environment, each naming that environment's product"
    );
    assert_eq!(
        plugin.handles().len(),
        3,
        "and one observe call per environment"
    );
}

/// `observe` needs plaintext, so the slot must be `resolved` — a
/// `reference_only` slot would leave a plugin unable to authenticate and is
/// what dispatch uses instead (Task 18).
#[tokio::test]
async fn the_slot_handed_to_observe_carries_the_resolved_material() {
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(full_detection()));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
    );
    let tenant = Uuid::new_v4();
    let environment = services
        .environments
        .create_environment(&ctx(tenant), pasted("a", "the-kubeconfig-document"))
        .await
        .unwrap();

    services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();

    let handle = plugin.last_handle();
    assert_eq!(handle.slots.len(), 1, "one credential, one slot");
    let (key, credstore_ref, material) = handle.slots[0].clone();
    assert_eq!(
        key, "kubeconfig",
        "the key comes from the plugin's own required secret field, not from a \
         literal in this gear"
    );
    assert_eq!(
        credstore_ref, environment.credentials[0].credstore_ref,
        "and the reference is the environment's own"
    );
    assert_eq!(
        material.as_deref(),
        Some(b"the-kubeconfig-document".as_slice()),
        "RESOLVED, not reference_only: observe is the one method the contract \
         expects to be handed plaintext"
    );
}

/// `config` is passed **verbatim**. This is the successor to the three
/// `VPADM_NAMESPACE` tests Task 15 deleted: the property is the same — an
/// environment's own operator-set configuration reaches the thing that
/// observes it — but the gear no longer knows what any of it means.
#[tokio::test]
async fn the_config_column_reaches_the_plugin_verbatim() {
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(full_detection()));
    let db = inmem_db().await;
    let services =
        build_services_tenant_scoped_with_plugin(db.clone(), port(Arc::clone(&plugin) as Arc<_>));
    let tenant = Uuid::new_v4();
    let environment = services
        .environments
        .create_environment(&ctx(tenant), pasted("a", "kubeconfig-a"))
        .await
        .unwrap();

    // Two keys, one of which this gear has never heard of: whatever the
    // migration's backfill (or a future credential form) put there is what
    // the plugin gets.
    set_config(
        &db,
        tenant,
        environment.id,
        serde_json::json!({"vpadm_namespace": "custom-ns", "some_other_product_key": 7}),
    )
    .await;

    services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();

    assert_eq!(
        plugin.last_handle().config,
        serde_json::json!({"vpadm_namespace": "custom-ns", "some_other_product_key": 7}),
        "verbatim -- not filtered to keys this gear recognises, and not \
         synthesised from the variables table"
    );
}

/// A freshly created environment has been observed by nothing, and the
/// contract distinguishes that from "observed and found nothing" — so the
/// handle's `observed` channel is `None` the first time and `Some` after.
#[tokio::test]
async fn the_previous_attributes_reach_the_plugin_and_are_none_the_first_time() {
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(full_detection()));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
    );
    let tenant = Uuid::new_v4();
    let environment = services
        .environments
        .create_environment(&ctx(tenant), pasted("a", "kubeconfig-a"))
        .await
        .unwrap();

    services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();
    assert_eq!(
        plugin.handles()[0].observed,
        None,
        "nothing has observed this environment yet, and an empty map would \
         claim otherwise"
    );

    services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();
    let observed = plugin.handles()[1]
        .observed
        .clone()
        .expect("the second call must see the first call's attributes");
    assert_eq!(observed.get("platformVersion"), Some("9.2"));
    assert_eq!(observed.get("baseDomain"), Some("https://sv.jele.io"));
}

// ---------------------------------------------------------------------------
// What the answer writes
// ---------------------------------------------------------------------------

/// The role projections reach the plugin-shaped columns.
#[tokio::test]
async fn role_projection_writes_the_new_columns() {
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(full_detection()));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
    );
    let tenant = Uuid::new_v4();
    let environment = services
        .environments
        .create_environment(&ctx(tenant), pasted("a", "kubeconfig-a"))
        .await
        .unwrap();

    let observed = services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();

    assert_eq!(observed.observed_version.as_deref(), Some("9.2"));
    assert_eq!(observed.observed_build.as_deref(), Some("1471"));
    assert_eq!(
        observed.observed_base_url.as_deref(),
        Some("https://sv.jele.io")
    );
    assert_eq!(observed.health_state, HealthState::Ok);
    assert_eq!(observed.health_detail.as_deref(), Some("Healthy"));
    assert!(observed.health_checked_at.is_some());
    assert_eq!(
        observed.observed_attrs.get("platformVersion"),
        Some("9.2"),
        "and the whole declared map is stored alongside the projections, \
         because the UI renders a plugin's own fields from it"
    );
    assert_eq!(observed.observed_attrs.get("build"), Some("1471"));
}

/// The dual write, and the thing that makes Phase D revertible: the same
/// observation still writes the legacy columns, with their own asymmetric
/// merge rules intact.
#[tokio::test]
async fn the_legacy_columns_are_written_identically() {
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(full_detection()));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
    );
    let tenant = Uuid::new_v4();
    let environment = services
        .environments
        .create_environment(&ctx(tenant), pasted("a", "kubeconfig-a"))
        .await
        .unwrap();

    let observed = services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();

    assert_eq!(
        observed.observed_base_url.as_deref(),
        Some("https://sv.jele.io"),
        "the BaseUrl projection fills the column. Task 19 dropped `vhp_base_url`, \
         which took the same value from the same projection; this is the one that \
         survived"
    );
    assert_eq!(
        observed.observed_attrs.get("namespace"),
        Some("virtuozzo"),
        "and the Namespace projection reaches `observed_attrs`, which is the \
         namespace's only home since `observed_namespace` was dropped (E-23)"
    );
    assert!(observed.version_detect_error.is_none());
    assert!(observed.version_detected_at.is_some());
}

/// A verdict that is neither `Ok` nor a failure, carried end to end — and the
/// one place the coarsening asymmetry is visible.
///
/// `HealthState` has three verdicts where legacy has four: `Warning` and
/// `Degraded` both coarsen to `Degraded`. The plugin contract's answer is that
/// the finer value rides along in `Checked`'s `detail`, so this asserts both
/// halves of the pair from one observation: `health_state` takes the coarse
/// verdict, `cluster_status` takes legacy's exact spelling back out of
/// `detail`. Without this, a `health_state` hardcoded to `ok` on the `Checked`
/// arm passes every other test in this module — measured, not assumed.
#[tokio::test]
async fn a_degraded_verdict_reaches_both_columns_in_each_ones_vocabulary() {
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(PluginObservation {
        environment: ObservationOutcome::Detected(attrs),
        health: HealthOutcome::Checked {
            state: HealthState::Degraded,
            // Legacy's own name for a cluster that answered and listed no
            // nodes at all -- a *different* status from `Degraded`, which
            // `HealthState` cannot express and this column can.
            detail: Some("Warning"),
        },
    }));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
    );
    let tenant = Uuid::new_v4();
    let environment = services
        .environments
        .create_environment(&ctx(tenant), pasted("a", "kubeconfig-a"))
        .await
        .unwrap();

    let observed = services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();

    assert_eq!(
        observed.health_state,
        HealthState::Degraded,
        "the plugin-shaped column takes the coarse verdict"
    );
    assert_eq!(
        observed.health_detail.as_deref(),
        Some("Warning"),
        "the plugin's own word survives in `health_detail` while `health_state` \
         carries the coarse verdict -- which is the whole reason the two columns \
         are separate. Task 19 dropped the legacy `cluster_status` that used to \
         hold the fine-grained spelling"
    );
}

/// `observed_namespace` is the one legacy column a later detection must
/// **not** overwrite — only fill from `NULL`. An accidental overwrite is
/// invisible in the happy path, so this drives two observations with
/// different namespaces and asserts the first one survives.
///
/// Its mirror image is `vhp_base_url`/`observed_base_url`, where the stored
/// value must yield to a conclusive detection and survive an inconclusive
/// one; the second half asserts that too, from the same pair of calls.
#[tokio::test]
async fn a_later_observation_overwrites_the_namespace_but_not_a_working_base_url() {
    let mut first = ObservedAttrs::default();
    first.set("platformVersion", "9.2");
    first.set("namespace", "chosen-first");
    first.set("baseDomain", "https://first.jele.io");
    // The second observation names a different namespace and NO base domain
    // at all -- an inconclusive gateway read, which is how a real plugin
    // reports one.
    let mut second = ObservedAttrs::default();
    second.set("platformVersion", "9.3");
    second.set("namespace", "detected-later");

    let plugin = Arc::new(SequencedPlugin::new([
        PluginObservation {
            environment: ObservationOutcome::Detected(first),
            health: HealthOutcome::NotAttempted,
        },
        PluginObservation {
            environment: ObservationOutcome::Detected(second),
            health: HealthOutcome::NotAttempted,
        },
    ]));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        Arc::new(FixedPluginPort::new(plugin)),
    );
    let tenant = Uuid::new_v4();
    let environment = services
        .environments
        .create_environment(&ctx(tenant), pasted("a", "kubeconfig-a"))
        .await
        .unwrap();

    let first_pass = services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();
    assert_eq!(
        first_pass.observed_attrs.get("namespace"),
        Some("chosen-first")
    );
    assert_eq!(
        first_pass.observed_base_url.as_deref(),
        Some("https://first.jele.io")
    );

    let second_pass = services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();
    // **`observed_attrs` is overwritten, not sticky** -- and since Task 19
    // dropped `observed_namespace` that is the whole rule rather than half of
    // a contradiction. Finding E-23 was exactly that contradiction: the column
    // was sticky, the map was not, so a redeployed environment ran in its new
    // namespace and displayed the old one.
    assert_eq!(
        second_pass.observed_attrs.get("namespace"),
        Some("detected-later"),
        "the map takes the LATEST observation -- which is the whole rule now \
         that the sticky column beside it is gone"
    );
    assert_eq!(
        second_pass.observed_base_url.as_deref(),
        Some("https://first.jele.io"),
        "and an inconclusive base-domain read must leave a working URL alone \
         rather than blank it"
    );
    assert_eq!(
        second_pass.observed_version.as_deref(),
        Some("9.3"),
        "while the version, whose rule IS unconditional overwrite, moved"
    );
}

/// A failed observation persists **classified** text, and nothing derived
/// from the credential material the plugin was handed.
///
/// The canary is the plugin SDK's own — planted in the kubeconfig the plugin
/// receives — so this and the leak harness are testing the same marker. The
/// remote's own message is the one sanctioned exception and must survive,
/// because "namespaces virtuozzo not found" is what makes a broken
/// environment fixable.
#[tokio::test]
async fn a_failed_observation_persists_only_classified_text() {
    let canary = Canary::vhp_shaped();
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(PluginObservation {
        environment: ObservationOutcome::Failed(
            PluginFailure::classified(FailureClass::NotFound, "the target could not be read")
                .with_remote_message("namespaces \"virtuozzo\" not found"),
        ),
        health: HealthOutcome::Failed(
            PluginFailure::classified(FailureClass::Unreachable, "the target could not be reached")
                .with_remote_message("connection refused"),
        ),
    }));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
    );
    let tenant = Uuid::new_v4();
    let environment = services
        .environments
        .create_environment(&ctx(tenant), pasted("a", &canary.pem))
        .await
        .unwrap();

    let observed = services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();

    // The precondition: the plugin really was handed the canary, so the
    // assertions below distinguish "contained" from "never present".
    assert_eq!(
        plugin.last_handle().slots[0].2.as_deref(),
        Some(canary.pem.as_bytes()),
        "the plugin must have been handed the planted material"
    );

    let detail = observed
        .health_detail
        .as_deref()
        .expect("a failed health read must say why");
    assert_eq!(
        detail,
        "the target could not be reached: connection refused"
    );
    assert_eq!(observed.health_state, HealthState::Unknown);
    let version_error = observed
        .version_detect_error
        .as_deref()
        .expect("a failed detection must say why");
    assert_eq!(
        version_error,
        "the target could not be read: namespaces \"virtuozzo\" not found"
    );

    for (label, text) in [
        ("health_detail", detail),
        ("version_detect_error", version_error),
    ] {
        for marker in [&canary.pem, &canary.token, &canary.password] {
            assert!(
                !text.contains(marker.as_str()),
                "{label} must carry no credential-derived text, got: {text}"
            );
        }
    }
    assert_eq!(
        observed.observed_attrs.iter().count(),
        0,
        "and a failed detection stores no attributes"
    );
}

/// `NotAttempted` writes no health column at all — in either set, and not
/// even a checked-at, because "never checked" is the truth when nothing
/// looked.
///
/// `health_state` is `NOT NULL DEFAULT 'unknown'`, so "writes no health
/// column" means the row keeps its existing value; that is what is asserted,
/// not that anything is `NULL`.
#[tokio::test]
async fn not_attempted_writes_no_health_columns() {
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(PluginObservation {
        environment: ObservationOutcome::Detected(attrs),
        health: HealthOutcome::NotAttempted,
    }));
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
    );
    let tenant = Uuid::new_v4();
    let environment = services
        .environments
        .create_environment(&ctx(tenant), pasted("a", "kubeconfig-a"))
        .await
        .unwrap();

    let observed = services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();

    assert_eq!(
        observed.health_state,
        HealthState::Unknown,
        "the column's own default, untouched"
    );
    assert_eq!(observed.health_detail, None);
    assert_eq!(
        observed.health_checked_at, None,
        "not even a check time: nothing checked, and a stamp would claim \
         something did"
    );
    assert_eq!(
        observed.observed_version.as_deref(),
        Some("9.2"),
        "while the environment half, which DID succeed, still wrote its own \
         columns (D-CH-4: the halves are independent)"
    );
}

/// **The requirement this task exists for.** A plugin with a spotless
/// `observed_schema()` can still put an undeclared key in the map it
/// returns; that map is persisted to a JSONB column and published on
/// `EnvironmentDto`, which is the persist → DTO → page chain of the
/// 2026-08-28 leak.
///
/// Mutation-proved: deleting `retain_declared` from
/// `ObservationWrite::new` turns this test red.
#[tokio::test]
async fn an_undeclared_attribute_never_reaches_storage() {
    let canary = Canary::vhp_shaped();
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");
    attrs.set("kubeconfig_echo", canary.pem.clone());

    let plugin = Arc::new(
        ScriptedPlugin::vhp_shaped(PluginObservation {
            environment: ObservationOutcome::Detected(attrs),
            health: HealthOutcome::NotAttempted,
        })
        // A spotless schema: it declares nothing secret, and it does not
        // declare `kubeconfig_echo` at all. Schema validation is satisfied;
        // only `retain_declared` stands between the echoed key and the
        // column.
        .with_observed_schema(vec![field(
            "platformVersion",
            FieldKind::Text,
            false,
            Some(FieldRole::Version),
        )]),
    );
    let services = build_services_tenant_scoped_with_plugin(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
    );
    let tenant = Uuid::new_v4();
    let environment = services
        .environments
        .create_environment(&ctx(tenant), pasted("a", &canary.pem))
        .await
        .unwrap();

    let observed = services
        .environments
        .observe_environment(&ctx(tenant), environment.id)
        .await
        .unwrap();

    assert_eq!(
        observed.observed_attrs.get("kubeconfig_echo"),
        None,
        "an undeclared attribute must not reach storage"
    );
    let stored = serde_json::to_string(&observed.observed_attrs).unwrap();
    assert!(
        !stored.contains(&canary.pem),
        "and the canary must be nowhere in the blob that reached the column: {stored}"
    );
    assert_eq!(
        observed.observed_attrs.get("platformVersion"),
        Some("9.2"),
        "while the DECLARED attribute survives alongside it -- dropping \
         everything would also satisfy a one-sided assertion"
    );
    assert_eq!(
        observed.observed_version.as_deref(),
        Some("9.2"),
        "and its projection still reaches the legacy column"
    );
}

// ---------------------------------------------------------------------------
// Nothing was able to look
// ---------------------------------------------------------------------------

// `an_environment_with_no_product_records_that_it_cannot_be_observed` was
// deleted by Task 20b. It planted a productless row through the repository --
// `create_environment` refused one from Task 19 on (ruling F-13) -- and
// asserted the cycle recorded why. `m20260903_000013` made `product_id`
// `NOT NULL` and the model followed, so the row is now unrepresentable and
// the test could only be kept by constructing a state no deployment can hold.
//
// `PluginUnavailable::NoProduct` itself is still reachable, from qa-catalog's
// resolver rather than from a row, and `an_unresolvable_plugin_records_why_
// and_writes_no_health_column` covers that arm.

/// The two remaining upstream failures: the product's plugin is not
/// resolvable, and no resolver is registered in this deployment. Both record
/// their own text and neither claims a health status.
#[tokio::test]
async fn an_unresolvable_plugin_records_why_and_writes_no_health_column() {
    for (cause, expected) in [
        (PluginUnavailable::Unresolvable, "plugin binding"),
        (PluginUnavailable::ResolverAbsent, "qa-catalog"),
    ] {
        // **Created through a working plugin, then observed without one.**
        // Since Task 19 a credential can only be stored under a key the
        // product's plugin declares (ruling F-13), so an environment cannot be
        // created while its plugin is unavailable -- but one created earlier
        // can certainly be *observed* while it is, which is the state this
        // test is about and the reason `ProductPluginPort` records
        // unavailability as a fact about the row rather than failing.
        let db = inmem_db().await;
        let tenant = Uuid::new_v4();
        let environment = build_services_tenant_scoped(db.clone())
            .environments
            .create_environment(&ctx(tenant), pasted("a", "kubeconfig-a"))
            .await
            .unwrap();

        let services =
            build_services_tenant_scoped_with_plugin(db, Arc::new(UnavailablePluginPort(cause)));
        let observed = services
            .environments
            .observe_environment(&ctx(tenant), environment.id)
            .await
            .expect("an unresolvable plugin is a recorded outcome, not an error");

        let message = observed
            .version_detect_error
            .as_deref()
            .expect("the row must say why");
        assert!(
            message.contains(expected),
            "{cause:?} must be distinguishable, got: {message}"
        );
        assert_eq!(observed.health_checked_at, None);
    }
}

/// The pre-plugin `kubeconfig` field can only be bound to a plugin that
/// declares exactly one required secret. Two, or none, and the **write** is
/// refused rather than the reference being stored under a guessed key.
///
/// # This moved from the observation path to the write path at Task 19
///
/// It used to assert that *observation* recorded `LEGACY_CREDENTIAL_UNBINDABLE`
/// on a pre-Task-18b row, reached by emptying `credentials` after the create so
/// `resolve_credential_slots` fell back to the single legacy column and had to
/// derive a key for it. Task 19 dropped that column and ruling **F-2** dropped
/// the fallback with it, so there is no longer a stored reference without a key
/// for observation to be confused by.
///
/// The constant is still live, and this is where: the wire still carries the
/// pre-plugin pair until Task 22, `desugar_legacy_credential_pair` still has to
/// fold it under *some* key, and when the plugin names no single required
/// secret there is no key to fold it under. Refusing the write is strictly
/// better than what the old path did — the operator is told at the moment they
/// submit, rather than at the next observation cycle.
#[tokio::test]
async fn the_pre_plugin_pair_cannot_bind_to_a_plugin_with_no_sole_required_secret() {
    for schema in [
        // None required.
        vec![field("kubeconfig", FieldKind::MultilineSecret, false, None)],
        // Two required.
        vec![
            field("kubeconfig", FieldKind::MultilineSecret, true, None),
            field("api_token", FieldKind::Secret, true, None),
        ],
    ] {
        let plugin =
            Arc::new(ScriptedPlugin::vhp_shaped(full_detection()).with_credential_schema(schema));
        let services = build_services_tenant_scoped_with_plugin(
            inmem_db().await,
            port(Arc::clone(&plugin) as Arc<_>),
        );

        let error = services
            .environments
            .create_environment(&ctx(Uuid::new_v4()), pasted("a", "kubeconfig-a"))
            .await
            .expect_err("the pre-plugin pair has no key to fold into");

        let crate::domain::error::DomainError::Validation { message, .. } = &error else {
            panic!("expected a validation error, got {error:?}");
        };
        assert!(
            message.contains("pre-plugin column"),
            "an operator must be told what to fix, got: {message}"
        );
        assert!(
            plugin.handles().is_empty(),
            "and no observation may run against a credential the gear could not key"
        );
    }
}
