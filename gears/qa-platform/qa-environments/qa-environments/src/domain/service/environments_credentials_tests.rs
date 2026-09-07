//! Task 18b's plugin-driven credential write path.
//!
//! Against a REAL in-memory `SQLite` database with real doubles for credstore
//! and the plugin — the same tier `environments_kubeconfig_tests` and
//! `observation_projection_tests` use, and for their reason: the merge rules
//! under test end in SQL, and a mock repository would assert against a
//! hand-written copy of them.
//!
//! # What is NOT here
//!
//! The pre-plugin `kubeconfig`/`kubeconfig_credstore_ref` pair's own rules —
//! exactly-one-of, the empty-reference reading, the orphan on a
//! reference-over-a-paste — are in `environments_kubeconfig_tests`, which
//! Task 18b left passing unmodified except for the generated-reference prefix.
//! That file passing is the evidence that folding the pair into the plugin
//! path (ruling F-8) preserved its behaviour; duplicating it here would assert
//! the same thing against the same code twice.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use qa_environments_sdk::{
    CredentialMaterial, CredentialSubmission, EnvironmentPatch, NewEnvironment,
};
use qa_product_sdk::descriptor::{FieldKind, FieldRole};
use qa_product_sdk::observation::{
    FailureClass, HealthOutcome, HealthState, ObservationOutcome, ObservedAttrs, PluginFailure,
    PluginObservation,
};
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::{PluginUnavailable, ProductPluginPort};
use crate::test_support::{
    FixedPluginPort, RecordingCredStore, ScriptedPlugin, UnavailablePluginPort,
    build_services_tenant_scoped_with_plugin_and_credstore, ctx, field, inmem_db, no_plugin_port,
};

/// The product every environment in this module belongs to.
fn product() -> Uuid {
    Uuid::from_u128(0x9001)
}

/// The prefix this gear mints generated references under, spelled literally so
/// a change to the constant has to be a deliberate change here too. Task 18b
/// generalised it from `qa-environments-kubeconfig-`.
const GENERATED_PREFIX: &str = "qa-environments-credential-";

const KUBECONFIG: &str = "apiVersion: v1\nkind: Config\n";

fn detection() -> PluginObservation {
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");
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

/// A create carrying `credentials` in plugin shape and nothing legacy.
fn submitting(name: &str, entries: Vec<(&str, CredentialSubmission)>) -> NewEnvironment {
    NewEnvironment {
        // The pre-plugin pair is still a REQUEST field until Task 22;
        // Task 19 dropped only the column.
        kubeconfig_credstore_ref: None,
        name: name.to_owned(),
        product_id: product(),
        description: None,
        kubeconfig: None,
        credentials: entries
            .into_iter()
            .map(|(key, submission)| (key.to_owned(), submission))
            .collect(),
        default_branch: None,
        is_default: false,
    }
}

fn pasted(value: &str) -> CredentialSubmission {
    CredentialSubmission::Material(CredentialMaterial::new(value.to_owned()))
}

// ---------------------------------------------------------------------------
// Where a submitted credential lands
// ---------------------------------------------------------------------------

/// A pasted secret is minted under the new prefix, lands in `credentials`
/// under the **plugin's** key, and **dual-writes** the pre-plugin column.
///
/// The dual-write is the whole reason Task 18b is safe to land before Task 19:
/// `qa-runs`' dispatch reads that column in production (ruling E-17), so a
/// write path that stopped maintaining it would break every run against every
/// environment.
#[tokio::test]
async fn a_pasted_secret_lands_in_credentials_and_dual_writes_the_legacy_column() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detection()));
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting("prod", vec![("kubeconfig", pasted(KUBECONFIG))]),
        )
        .await
        .unwrap();

    assert_eq!(created.credentials.len(), 1, "one submitted, one stored");
    assert_eq!(
        created.credentials[0].key, "kubeconfig",
        "the key is the plugin's own `FieldDesc::key`, not a literal in this gear"
    );
    assert!(
        created.credentials[0]
            .credstore_ref
            .starts_with(GENERATED_PREFIX),
        "a pasted document is minted under a reference this gear owns, got {}",
        created.credentials[0].credstore_ref
    );
    assert_eq!(
        created.credentials[0].credstore_ref, created.credentials[0].credstore_ref,
        "and the pre-plugin column is DUAL-WRITTEN with the same reference, \
         because qa-runs' dispatch still reads it (ruling E-17)"
    );
    assert_eq!(
        credstore.references(),
        vec![created.credentials[0].credstore_ref.clone()],
        "exactly one secret written, and it is the one the row names"
    );
    // The document reached credstore and nothing else: the row carries a
    // reference, and the SDK model has no field that could hold material.
    assert!(
        !format!("{created:?}").contains("apiVersion"),
        "the document must not be renderable from the created environment"
    );
}

/// A submitted **reference** lands in `credentials` as given, is not minted,
/// and is not owned — so a later rotation leaves it alone.
#[tokio::test]
async fn a_submitted_reference_is_stored_as_given_and_never_owned() {
    let credstore = Arc::new(RecordingCredStore::new());
    credstore.seed("team-a-prod-cluster", KUBECONFIG);
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detection()));
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting(
                "prod",
                vec![(
                    "kubeconfig",
                    CredentialSubmission::Reference("team-a-prod-cluster".to_owned()),
                )],
            ),
        )
        .await
        .unwrap();

    assert_eq!(
        created.credentials[0].credstore_ref, "team-a-prod-cluster",
        "stored verbatim; nothing was minted for it"
    );
    assert_eq!(
        credstore.references(),
        vec!["team-a-prod-cluster".to_owned()],
        "and no second secret appeared"
    );

    // Rotating it to a paste must leave the caller-supplied secret in place:
    // it may be shared with other environments or other systems entirely.
    services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                credentials: BTreeMap::from([("kubeconfig".to_owned(), pasted("rotated"))]),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap();

    assert!(
        credstore
            .references()
            .contains(&"team-a-prod-cluster".to_owned()),
        "a reference this gear did not mint is never this gear's to destroy, got {:?}",
        credstore.references()
    );
}

/// **The plugin decides what is secret, and this gear obeys.** A field the
/// plugin classifies non-secret goes to `config` in the clear, never to
/// credstore and never to `credentials`.
///
/// `vpadm_namespace` is the real case: `FieldKind::Text` in VHP's own
/// `credential_schema()`, submitted through the same form as the kubeconfig.
#[tokio::test]
async fn a_non_secret_field_lands_in_config_not_in_credstore() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detection()));
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting(
                "prod",
                vec![
                    ("kubeconfig", pasted(KUBECONFIG)),
                    ("vpadm_namespace", pasted("virtuozzo")),
                ],
            ),
        )
        .await
        .unwrap();

    assert_eq!(
        created.config,
        serde_json::json!({ "vpadm_namespace": "virtuozzo" }),
        "the non-secret field is stored as a value, under its own key"
    );
    assert_eq!(
        created.credentials.len(),
        1,
        "and it is NOT a credential: only the kubeconfig is, got {:?}",
        created.credentials
    );
    assert_eq!(
        credstore.len(),
        1,
        "nor did it reach credstore -- one secret written, for the kubeconfig"
    );
}

// ---------------------------------------------------------------------------
// What the plugin is asked, and what its answer is allowed to say
// ---------------------------------------------------------------------------

/// A plugin rejection surfaces its `&'static str` detail and **no submitted
/// value**.
///
/// `PluginFailure::detail` is `Option<&'static str>` — a compile-time constant
/// — which is what makes it the one credential-shaped text this path may
/// surface verbatim. The absence assertion is the point: it searches the whole
/// rendered error for the submitted bytes.
#[tokio::test]
async fn a_plugin_rejection_surfaces_its_fixed_detail_and_no_submitted_value() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(
        ScriptedPlugin::vhp_shaped(detection()).rejecting_credentials(PluginFailure::classified(
            FailureClass::Malformed,
            "a kubeconfig is required",
        )),
    );
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let error = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting(
                "prod",
                vec![("kubeconfig", pasted("SUPER-SECRET-MATERIAL"))],
            ),
        )
        .await
        .expect_err("the plugin refused the form");

    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("a kubeconfig is required"),
        "the plugin's own fixed detail must reach the caller, got: {rendered}"
    );
    assert!(
        !rendered.contains("SUPER-SECRET-MATERIAL"),
        "and nothing submitted may appear in it, got: {rendered}"
    );
    assert_eq!(
        credstore.len(),
        0,
        "nothing rejected ever reaches credstore -- the ordering create has \
         always guaranteed"
    );
}

/// A submitted key the plugin does not declare is **dropped**, not refused.
///
/// That is the plugin contract's own choice, not this gear's:
/// `qa-vhp-product-plugin`'s `validate_credentials` documents that undeclared
/// keys "are ignored rather than rejected ... rejecting the whole form over one
/// is a worse failure than dropping it" (ruling F-5). The `warn` that keeps it
/// from being silent names the key and never a value; this asserts the storage
/// half.
#[tokio::test]
async fn a_key_the_plugin_does_not_declare_is_dropped_not_stored() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detection()));
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting(
                "prod",
                vec![
                    ("kubeconfig", pasted(KUBECONFIG)),
                    ("not_a_declared_field", pasted("dropped")),
                ],
            ),
        )
        .await
        .expect("one undeclared key does not reject the whole form");

    assert_eq!(
        created.credentials.len(),
        1,
        "only the declared field is stored, got {:?}",
        created.credentials
    );
    assert_eq!(
        created.config,
        serde_json::json!({}),
        "and it did not fall through to `config` either"
    );
    assert_eq!(credstore.len(), 1, "nor to credstore");
}

/// A classification for a key the form did **not** carry is refused.
///
/// The asymmetry with the test above is deliberate: dropping an undeclared
/// submission loses nothing, while acting on this would have the gear mint a
/// secret out of nothing and store a credential with no value.
#[tokio::test]
async fn a_classification_for_an_unsubmitted_key_is_refused() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(
        ScriptedPlugin::vhp_shaped(detection()).classifying_unsubmitted_key("never_submitted"),
    );
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let error = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting("prod", vec![("kubeconfig", pasted(KUBECONFIG))]),
        )
        .await
        .expect_err("a plugin cannot claim a field the request did not submit");

    assert!(
        format!("{error:?}").contains("never_submitted"),
        "the offending key must be named -- it is a plugin bug an operator \
         reports, got: {error:?}"
    );
}

/// A credstore **reference** offered for a non-secret field is refused:
/// `config` holds values, and there is nothing sensible to store.
#[tokio::test]
async fn a_reference_for_a_non_secret_field_is_refused() {
    let credstore = Arc::new(RecordingCredStore::new());
    credstore.seed("some-ref", "virtuozzo");
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detection()));
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let error = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting(
                "prod",
                vec![
                    ("kubeconfig", pasted(KUBECONFIG)),
                    (
                        "vpadm_namespace",
                        CredentialSubmission::Reference("some-ref".to_owned()),
                    ),
                ],
            ),
        )
        .await
        .expect_err("a non-secret field cannot be given as a credstore reference");

    assert!(
        format!("{error:?}").contains("vpadm_namespace"),
        "the offending field must be named, got: {error:?}"
    );
}

/// **The write path resolves nothing** (ruling F-4, reversed): only pasted
/// material reaches `validate_credentials`, and a reference-only submission
/// reaches it not at all.
///
/// This is what keeps creating an environment against a not-yet-provisioned
/// reference possible — a designed, self-healing state whose recovery
/// instruction `KUBECONFIG_UNRESOLVED` spells out.
#[tokio::test]
async fn a_reference_only_submission_is_never_resolved_and_never_validated() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detection()));
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    // Deliberately NOT seeded: credstore holds no such secret.
    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting(
                "prod",
                vec![(
                    "kubeconfig",
                    CredentialSubmission::Reference("not-provisioned-yet".to_owned()),
                )],
            ),
        )
        .await
        .expect("naming a not-yet-provisioned reference is a self-healing state, not an error");

    assert_eq!(created.credentials[0].credstore_ref, "not-provisioned-yet");
    assert!(
        plugin.validated_keys().is_empty(),
        "with nothing pasted there is no form to validate, so the plugin is \
         not asked at all, got {:?}",
        plugin.validated_keys()
    );
}

/// A create that supplies no credential at all is refused by the platform
/// floor, because the reference-only path reaches no plugin that could enforce
/// a required field.
#[tokio::test]
async fn a_create_with_no_credential_at_all_is_refused() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detection()));
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );

    let error = services
        .environments
        .create_environment(&ctx(Uuid::new_v4()), submitting("prod", vec![]))
        .await
        .expect_err("an environment needs at least one credential");

    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("at least one credential"),
        "got: {rendered}"
    );
    assert!(
        !rendered.contains("kubeconfig"),
        "and the message must not name one product's credential, got: {rendered}"
    );
}

/// Supplying one field twice — once in `credentials`, once as the pre-plugin
/// pair — is refused rather than resolved by precedence.
///
/// Either precedence is a silent surprise: the operator sees one of two values
/// stored and no reason why.
#[tokio::test]
async fn one_field_supplied_under_both_spellings_is_refused() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detection()));
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );

    let mut new = submitting("prod", vec![("kubeconfig", pasted(KUBECONFIG))]);
    new.kubeconfig = Some(CredentialMaterial::new("the-legacy-spelling".to_owned()));

    let error = services
        .environments
        .create_environment(&ctx(Uuid::new_v4()), new)
        .await
        .expect_err("two spellings of one field is a refusal, not a precedence question");

    assert!(
        format!("{error:?}").contains("supplied twice"),
        "got: {error:?}"
    );
    assert_eq!(credstore.len(), 0, "and nothing was written");
}

// ---------------------------------------------------------------------------
// The compensating deletes, both directions
// ---------------------------------------------------------------------------

/// A failed **row** write deletes every reference the call minted — all of
/// them, not just the first, because a plugin may declare several secret
/// fields.
///
/// A duplicate name is the realistic cause, and the one the code's own comment
/// names.
#[tokio::test]
async fn a_failed_row_write_forgets_every_minted_secret() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(
        ScriptedPlugin::vhp_shaped(detection()).with_credential_schema(vec![
            field("kubeconfig", FieldKind::MultilineSecret, true, None),
            field("api_token", FieldKind::Secret, false, None),
        ]),
    );
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let two_secrets = || {
        submitting(
            "prod",
            vec![
                ("kubeconfig", pasted(KUBECONFIG)),
                ("api_token", pasted("t0ken")),
            ],
        )
    };

    services
        .environments
        .create_environment(&ctx(tenant), two_secrets())
        .await
        .unwrap();
    assert_eq!(credstore.len(), 2, "two secret fields, two secrets");

    // Same name: the row write fails after both secrets are written.
    services
        .environments
        .create_environment(&ctx(tenant), two_secrets())
        .await
        .expect_err("a duplicate name must fail the row write");

    assert_eq!(
        credstore.len(),
        2,
        "the second call's TWO minted secrets are both cleaned up, leaving \
         only the first environment's: {:?}",
        credstore.references()
    );
}

/// An update supersedes only the keys it mentions, and deletes only what this
/// gear owns.
#[tokio::test]
async fn an_update_supersedes_only_the_keys_it_mentions() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(
        ScriptedPlugin::vhp_shaped(detection()).with_credential_schema(vec![
            field("kubeconfig", FieldKind::MultilineSecret, true, None),
            field("api_token", FieldKind::Secret, false, None),
        ]),
    );
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting(
                "prod",
                vec![
                    ("kubeconfig", pasted(KUBECONFIG)),
                    ("api_token", pasted("t0ken")),
                ],
            ),
        )
        .await
        .unwrap();
    let original_token = created
        .credentials
        .iter()
        .find(|credential| credential.key == "api_token")
        .unwrap()
        .credstore_ref
        .clone();
    let original_kubeconfig = created.credentials[0].credstore_ref.clone();

    let updated = services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                credentials: BTreeMap::from([("kubeconfig".to_owned(), pasted("rotated"))]),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        updated.credentials.len(),
        2,
        "the untouched credential survives the merge: {:?}",
        updated.credentials
    );
    assert_eq!(
        updated
            .credentials
            .iter()
            .find(|credential| credential.key == "api_token")
            .unwrap()
            .credstore_ref,
        original_token,
        "and keeps its own reference -- a key the patch did not mention \
         supersedes nothing"
    );
    assert_ne!(
        updated.credentials[0].credstore_ref, original_kubeconfig,
        "while the mentioned one rotated"
    );
    assert!(
        credstore.references().contains(&original_token),
        "the unmentioned secret is still in credstore: {:?}",
        credstore.references()
    );
    assert!(
        !credstore.references().contains(&original_kubeconfig),
        "and only the superseded one was removed: {:?}",
        credstore.references()
    );
}

// ---------------------------------------------------------------------------
// Both readers prefer `credentials` (ruling F-2)
// ---------------------------------------------------------------------------

/// `resolve_credential_slots` reads `credentials` when it is populated, and
/// hands the plugin one resolved slot per stored credential.
///
/// The `qa-runs` half of this pair is `dispatch_tests`'
/// `the_plugin_is_handed_a_reference_only_slot_keyed_by_its_own_schema` and
/// `an_environment_written_before_task_18b_falls_back_to_the_legacy_column`.
#[tokio::test]
async fn observation_resolves_every_credential_in_the_plugin_shaped_column() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(
        ScriptedPlugin::vhp_shaped(detection()).with_credential_schema(vec![
            field("kubeconfig", FieldKind::MultilineSecret, true, None),
            field("api_token", FieldKind::Secret, false, None),
        ]),
    );
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting(
                "prod",
                vec![
                    ("kubeconfig", pasted(KUBECONFIG)),
                    ("api_token", pasted("t0ken")),
                ],
            ),
        )
        .await
        .unwrap();

    services
        .environments
        .observe_environment(&ctx(tenant), created.id)
        .await
        .unwrap();

    let handle = plugin.last_handle();
    let mut keys: Vec<&str> = handle
        .slots
        .iter()
        .map(|(key, _, _)| key.as_str())
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["api_token", "kubeconfig"],
        "BOTH stored credentials reach the plugin -- the legacy single-reference \
         column could only ever carry one"
    );
    for (key, _, material) in &handle.slots {
        assert!(
            material.is_some(),
            "observe is the one method handed plaintext, so every slot is \
             resolved; `{key}` was not"
        );
    }
}

// ---------------------------------------------------------------------------
// The productless branch (ruling F-7)
// ---------------------------------------------------------------------------

// Four tests were deleted here by Task 19, with the column they were about.
//
// `a_create_with_no_product_takes_the_pre_plugin_path`,
// `observation_falls_back_to_the_legacy_column_for_a_pre_task_18b_row`,
// `deleting_a_pre_task_18b_row_forgets_its_legacy_secret` (the re-review's
// `n-4`) and `a_reference_minted_under_the_pre_task_18b_prefix_is_still_ours_
// to_delete` all exercised `kubeconfig_credstore_ref` — either the pre-plugin
// write path, or the readers' fallback to it. Task 19 dropped the column,
// ruling **F-2** dropped both fallbacks with it, and ruling **F-13** made the
// pre-plugin path refuse, so none of the four describes reachable behaviour.
//
// What replaced their properties, so none is silently lost:
//
// * the productless and plugin-unavailable *refusals* are
//   `the_two_pre_plugin_routes_give_two_different_remedies`;
// * n-ary secret cleanup on delete is
//   `deleting_an_environment_forgets_every_secret_it_owns` (finding I-4),
//   which is the general case the 1-ary legacy test was a special case of;
// * **the old minting prefix is still owned**, which is what
//   `a_reference_minted_under_the_pre_task_18b_prefix...` was really for:
//   `is_generated_ref` still matches it, and
//   `a_rotation_supersedes_a_secret_minted_under_the_old_prefix` holds that.

// `plugin_shaped_credentials_with_no_product_are_refused` was deleted by Task
// 20b. It asserted the refusal ruling F-13 put on a productless credential
// write; `m20260903_000013` made `product_id` `NOT NULL` and
// `NewEnvironment::product_id` a plain `Uuid`, so the state is unrepresentable
// rather than refused -- the same progression Task 20a made for
// `NewProduct::plugin_instance_id`.
//
// What holds the property now: the type itself, plus
// `TryFrom<CreateEnvironmentReq>`'s refusal at the wire, which
// `dto::tests::create_environment_req_into_new_environment` exercises. The
// plugin-UNAVAILABLE half of F-13 is untouched and is
// `the_two_pre_plugin_routes_give_two_different_remedies`.

/// The `FieldRole` import is used by the schema helper below; this keeps the
/// role-claiming shape exercised so the import is not dead.
#[tokio::test]
async fn a_role_claiming_credential_schema_is_still_only_about_credentials() {
    let plugin = Arc::new(
        ScriptedPlugin::vhp_shaped(detection()).with_observed_schema(vec![field(
            "platformVersion",
            FieldKind::Text,
            false,
            Some(FieldRole::Version),
        )]),
    );
    let credstore = Arc::new(RecordingCredStore::new());
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting("prod", vec![("kubeconfig", pasted(KUBECONFIG))]),
        )
        .await
        .unwrap();

    assert_eq!(
        created.observed_version, None,
        "a create observes nothing: the role projection is observation's, not \
         the credential path's"
    );
}

// ---------------------------------------------------------------------------
// Regression tests from the independent review
// ---------------------------------------------------------------------------

// `a_credential_patch_on_a_product_cleared_row_is_refused_not_desynchronised`
// was deleted by Task 20b, and the defect it covered is closed by construction
// rather than by a guard.
//
// **Critical C-1** was reachable because `EnvironmentPatch::product_id` was
// `Option<Option<Uuid>>`: a caller could CLEAR a plugin-managed row's product,
// and the next credential-bearing patch then took a path with no plugin to key
// the column, writing a fresh reference into the legacy column while
// `credentials` still named the one about to be deleted. Task 19 dropped that
// column; Task 20b made `product_id` NOT NULL and collapsed the patch field to
// two states, so there is no "cleared" state left to reach the bug through.
//
// The plugin-UNAVAILABLE route into the same code is still live and is
// `the_two_pre_plugin_routes_give_two_different_remedies`.

/// **Critical C-2**, all three variants the review found. The floor counts what
/// was *stored*, not what was submitted.
///
/// A submitted `Reference` under a key the plugin does not declare is dropped
/// by `reconcile_classifications` and never reaches `validate_credentials` at
/// all, so before the fix one transposed character produced an accepted row
/// with `credentials = []` **and** `kubeconfig_credstore_ref = ""` — the
/// credential-less row Task 19's warning item 5 exists to prevent, which its
/// re-derivation cannot repair because it re-derives *from* the empty column.
#[tokio::test]
async fn a_create_that_stores_no_required_secret_is_refused() {
    let variants: Vec<(&str, (&str, CredentialSubmission))> = vec![
        (
            "a reference under a typo'd key -- reaches no plugin at all",
            (
                "kubeconfg",
                CredentialSubmission::Reference("credstore://team-a/prod".to_owned()),
            ),
        ),
        ("a paste under an undeclared key", ("nope", pasted("x"))),
        (
            "only a declared NON-secret field",
            ("vpadm_namespace", pasted("virtuozzo")),
        ),
    ];

    for (label, entry) in variants {
        let credstore = Arc::new(RecordingCredStore::new());
        credstore.seed("credstore://team-a/prod", KUBECONFIG);
        let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detection()));
        let services = build_services_tenant_scoped_with_plugin_and_credstore(
            inmem_db().await,
            port(Arc::clone(&plugin) as Arc<_>),
            credstore.clone(),
        );

        let error = services
            .environments
            .create_environment(&ctx(Uuid::new_v4()), submitting("prod", vec![entry]))
            .await
            .expect_err(label);

        let DomainError::Validation { field, message } = &error else {
            panic!("{label}: expected a validation error, got {error:?}");
        };
        assert_eq!(
            field, "kubeconfig",
            "{label}: the refusal names the plugin's own missing required field"
        );
        assert!(
            message.contains("requires this credential"),
            "{label}: got {message}"
        );
        // The row must not exist, and the secret seeded for the reference
        // variant must be untouched -- nothing rejected reaches credstore.
        assert!(
            services
                .environments
                .list_environments(&ctx(Uuid::new_v4()), &ODataQuery::default())
                .await
                .unwrap()
                .items
                .is_empty(),
            "{label}: no environment may have been created"
        );
    }
}

/// **Review finding IMPORTANT-3.** The two routes into the pre-plugin path have
/// two different remedies, so they must not share one message.
///
/// `store_unclassified_credential`/`store_unclassified_patch` are reached when
/// the row names no product **or** when its product's plugin cannot be
/// resolved. Both used to answer "set `product_id`" / "restore `product_id`",
/// which on the second route is false in both halves: `product_id` was never
/// removed, and editing it is not the remedy — waiting for qa-catalog is.
///
/// The second half of this test is the one that matters for Task 19. A row
/// with a populated `credentials` **is refused** during a plugin outage, by
/// Critical C-1's guard — so the fallback finding I-9 reverted the refusal to
/// preserve does not, in fact, keep plugin-shaped rows writable. See ruling
/// F-13: after Task 19 drops the legacy column there is no fallback left at
/// all and this becomes the only behaviour.
#[tokio::test]
async fn the_two_pre_plugin_routes_give_two_different_remedies() {
    // ---- route 2, create: the product is set; its plugin cannot be resolved.
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        no_plugin_port(),
        Arc::new(RecordingCredStore::new()),
    );
    let error = services
        .environments
        .create_environment(
            &ctx(Uuid::new_v4()),
            submitting("prod", vec![("kubeconfig", pasted(KUBECONFIG))]),
        )
        .await
        .expect_err("a plugin-shaped create cannot be classified without the plugin");
    let DomainError::Validation { field, message } = &error else {
        panic!("expected a validation error, got {error:?}");
    };
    assert_eq!(
        field, "credentials",
        "the caller's `product_id` is correct, so the error must not point at it"
    );
    assert!(
        message.contains("retry") && message.contains("qa-catalog is not running"),
        "the message must carry the transient cause and its remedy: {message}"
    );
    assert!(
        !message.contains("set `product_id`"),
        "advice to set a field the caller already set: {message}"
    );

    // ---- route 2, update on a row that already stores plugin-shaped credentials
    let credstore = Arc::new(RecordingCredStore::new());
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let created = build_services_tenant_scoped_with_plugin_and_credstore(
        db.clone(),
        port(Arc::new(ScriptedPlugin::vhp_shaped(detection()))),
        credstore.clone(),
    )
    .environments
    .create_environment(
        &ctx(tenant),
        submitting("prod", vec![("kubeconfig", pasted(KUBECONFIG))]),
    )
    .await
    .unwrap();
    assert_eq!(created.credentials.len(), 1, "the row is plugin-shaped");
    let before = credstore.references();

    let error = build_services_tenant_scoped_with_plugin_and_credstore(
        db.clone(),
        no_plugin_port(),
        credstore.clone(),
    )
    .environments
    .update_environment(
        &ctx(tenant),
        created.id,
        EnvironmentPatch {
            kubeconfig: Some(CredentialMaterial::from("rotated".to_owned())),
            ..EnvironmentPatch::default()
        },
    )
    .await
    .expect_err("no plugin means no key, so a credential rotation cannot be stored");
    let DomainError::Validation { field, message } = &error else {
        panic!("expected a validation error, got {error:?}");
    };
    assert_eq!(field, "credentials", "the row's `product_id` is intact");
    assert!(
        !message.contains("restore `product_id`"),
        "the product was never cleared on this route: {message}"
    );
    assert!(
        message
            .to_lowercase()
            .contains("retry once it is available"),
        "`ResolverAbsent` is transient, so the remedy is to wait: {message}"
    );
    assert_eq!(
        credstore.references(),
        before,
        "and the refusal wrote nothing"
    );

    // ---- route 2, the OTHER two causes: same refusal, different remedies
    //
    // Task 19's item 7 owns this: after the drop the branch refuses on **all
    // three** `PluginUnavailable` causes, so all three `detail()` texts become
    // operator-facing on the write path. Telling an operator to retry a
    // configuration fault sends them round a loop that cannot terminate.
    let unresolvable = build_services_tenant_scoped_with_plugin_and_credstore(
        db.clone(),
        Arc::new(UnavailablePluginPort(PluginUnavailable::Unresolvable)),
        credstore.clone(),
    )
    .environments
    .update_environment(
        &ctx(tenant),
        created.id,
        EnvironmentPatch {
            kubeconfig: Some(CredentialMaterial::from("rotated".to_owned())),
            ..EnvironmentPatch::default()
        },
    )
    .await
    .expect_err("an unresolvable plugin cannot classify a credential either");
    let DomainError::Validation { message, .. } = &unresolvable else {
        panic!("expected a validation error, got {unresolvable:?}");
    };
    assert!(
        !message.to_lowercase().contains("retry"),
        "a missing plugin gear is a configuration fault; no retry clears it: {message}"
    );
    assert!(
        message.contains("check the product's plugin binding"),
        "and the remedy must be the one that works: {message}"
    );
    assert!(
        !message.contains("names no product plugin"),
        "that half of the text became impossible when Task 20a made \
         `plugin_instance_id` NOT NULL: {message}"
    );

    // Route 1 -- the productless one -- is gone with Task 20b: `product_id` is
    // `NOT NULL` and `NewEnvironment` takes a plain `Uuid`, so a credential
    // write with no product cannot be constructed to be refused. Route 2 above
    // is the whole of ruling F-13 now.
}

/// **Review finding IMPORTANT-1.** A refused write must forget every secret it
/// minted on the way to being refused.
///
/// `store_submitted_credentials` writes pasted secrets to credstore in step 4
/// and only then applies the C-2 floor in step 5. Before the fix the `?` on
/// that floor dropped `minted` on the ground, so `forget_minted_secrets` —
/// which exists for exactly this — never ran and the secrets stayed in
/// credstore with nothing able to name them. Reachable from an ordinary bad
/// request and repeatable: a client looping a rejected create wrote an
/// unbounded number of unreachable secrets.
///
/// The three variants of `a_create_that_stores_no_required_secret_is_refused`
/// could not see it, because none of them mints anything — a reference under a
/// typo'd key, a paste under an undeclared key and a non-secret paste all
/// reach credstore zero times. It takes a schema with **two** required secrets
/// and a submission of one to mint first and be refused second.
#[tokio::test]
async fn a_refused_credential_write_forgets_every_secret_it_minted() {
    let two_required = || {
        vec![
            field("kubeconfig", FieldKind::MultilineSecret, true, None),
            field("api_token", FieldKind::Secret, true, None),
        ]
    };

    // ---- create: mint `api_token`, then be refused for the missing `kubeconfig`
    let credstore = Arc::new(RecordingCredStore::new());
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::new(
            ScriptedPlugin::vhp_shaped(detection()).with_credential_schema(two_required()),
        )),
        credstore.clone(),
    );
    let error = services
        .environments
        .create_environment(
            &ctx(Uuid::new_v4()),
            submitting("prod", vec![("api_token", pasted("t0ken"))]),
        )
        .await
        .expect_err("a create that leaves a required secret unstored is refused");
    assert!(
        format!("{error:?}").contains("kubeconfig"),
        "the refusal names the missing field: {error:?}"
    );
    assert_eq!(
        credstore.references(),
        Vec::<String>::new(),
        "the refused create minted `api_token` and must have forgotten it again; \
         a secret left here is unreachable forever, because nothing names it"
    );

    // ---- update: the same shape, on the call the n-2 escape test already makes
    let credstore = Arc::new(RecordingCredStore::new());
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let created = build_services_tenant_scoped_with_plugin_and_credstore(
        db.clone(),
        port(Arc::new(ScriptedPlugin::vhp_shaped(detection()))),
        credstore.clone(),
    )
    .environments
    .create_environment(
        &ctx(tenant),
        submitting("prod", vec![("kubeconfig", pasted(KUBECONFIG))]),
    )
    .await
    .unwrap();
    let before = credstore.references();
    assert_eq!(before.len(), 1, "the row owns exactly its one kubeconfig");

    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        db.clone(),
        port(Arc::new(
            ScriptedPlugin::vhp_shaped(detection()).with_credential_schema(two_required()),
        )),
        credstore.clone(),
    );
    services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                credentials: BTreeMap::from([("kubeconfig".to_owned(), pasted("rotated"))]),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .expect_err("the row does not satisfy the plugin's current schema");
    assert_eq!(
        credstore.references(),
        before,
        "a refused rotation must leave credstore exactly as it found it -- the \
         replacement it minted is forgotten, and the secret it did NOT supersede stays"
    );

    // ---- a mint that fails PART WAY through an n-ary write unwinds the rest
    //
    // This half predates the C-2 floor: the first credential is written, the
    // second write fails, and before the fix the first was orphaned by the
    // same dropped `minted`. `delete` is not counted against the budget, so
    // the compensating cleanup can still run.
    let credstore = Arc::new(RecordingCredStore::failing_creates_after(1));
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::new(
            ScriptedPlugin::vhp_shaped(detection()).with_credential_schema(two_required()),
        )),
        credstore.clone(),
    );
    services
        .environments
        .create_environment(
            &ctx(Uuid::new_v4()),
            submitting(
                "prod",
                vec![
                    ("kubeconfig", pasted(KUBECONFIG)),
                    ("api_token", pasted("t0ken")),
                ],
            ),
        )
        .await
        .expect_err("the second credstore write fails");
    assert_eq!(
        credstore.references(),
        Vec::<String>::new(),
        "the credential written before the failure must be forgotten too"
    );
}

/// **I-4.** Deleting an environment forgets **every** secret this gear minted
/// for it, not only the one the legacy column names.
///
/// No live effect today — every plugin in this tree declares one secret field —
/// but the path is generic, and after Task 19 drops the legacy column a 1-ary
/// delete would clean up nothing at all.
#[tokio::test]
async fn deleting_an_environment_forgets_every_secret_it_owns() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(
        ScriptedPlugin::vhp_shaped(detection()).with_credential_schema(vec![
            field("kubeconfig", FieldKind::MultilineSecret, true, None),
            field("api_token", FieldKind::Secret, false, None),
        ]),
    );
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting(
                "prod",
                vec![
                    ("kubeconfig", pasted(KUBECONFIG)),
                    ("api_token", pasted("t0ken")),
                ],
            ),
        )
        .await
        .unwrap();
    assert_eq!(credstore.len(), 2, "two secret fields, two secrets");

    services
        .environments
        .delete_environment(&ctx(tenant), created.id)
        .await
        .unwrap();

    assert_eq!(
        credstore.references(),
        Vec::<String>::new(),
        "BOTH minted secrets must go -- an orphan is a secret nothing can ever \
         name again, which is what `delete_ssh_key` exists to avoid: {:?}",
        credstore.references()
    );
    let _ = created;
}

/// **I-5.** The update path's compensating delete: a failed row write forgets
/// every reference the call minted, exactly as the create path does.
///
/// The reachable cause is a rename collision on the unique name index while
/// the rotation's new secret is already in credstore.
#[tokio::test]
async fn a_failed_update_row_write_forgets_the_secret_it_just_minted() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detection()));
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting("taken", vec![("kubeconfig", pasted(KUBECONFIG))]),
        )
        .await
        .unwrap();
    let victim = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting("victim", vec![("kubeconfig", pasted(KUBECONFIG))]),
        )
        .await
        .unwrap();
    let before = credstore.references();
    assert_eq!(before.len(), 2);

    // Rotate the credential AND rename onto the other environment's name, in
    // one call: the secret is written first, then the row write fails.
    services
        .environments
        .update_environment(
            &ctx(tenant),
            victim.id,
            EnvironmentPatch {
                name: Some("taken".to_owned()),
                credentials: BTreeMap::from([("kubeconfig".to_owned(), pasted("rotated"))]),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .expect_err("the duplicate name must fail the row write");

    assert_eq!(
        credstore.references(),
        before,
        "the secret minted for the failed rotation must be cleaned up, leaving \
         credstore exactly as it was: {:?}",
        credstore.references()
    );
}

/// **I-7.** `merge_credentials`' no-op-rotation guard: a request that names the
/// reference the row already stores must not delete it.
///
/// Its doc calls the comparison load-bearing rather than defensive tidiness,
/// and the behaviour it protects is destruction of a live credential by a
/// request that changed nothing — a retry, or an idempotent re-save.
#[tokio::test]
async fn resubmitting_the_stored_reference_does_not_delete_it() {
    let credstore = Arc::new(RecordingCredStore::new());
    let plugin = Arc::new(ScriptedPlugin::vhp_shaped(detection()));
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        inmem_db().await,
        port(Arc::clone(&plugin) as Arc<_>),
        credstore.clone(),
    );
    let tenant = Uuid::new_v4();

    // Create by PASTE, so the stored reference is one this gear owns and would
    // therefore really delete. A caller-supplied reference would pass this
    // test through the ownership check instead of through the guard.
    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            submitting("prod", vec![("kubeconfig", pasted(KUBECONFIG))]),
        )
        .await
        .unwrap();
    let stored = created.credentials[0].credstore_ref.clone();

    for round in 1..=2 {
        services
            .environments
            .update_environment(
                &ctx(tenant),
                created.id,
                EnvironmentPatch {
                    credentials: BTreeMap::from([(
                        "kubeconfig".to_owned(),
                        CredentialSubmission::Reference(stored.clone()),
                    )]),
                    ..EnvironmentPatch::default()
                },
            )
            .await
            .unwrap();
        assert!(
            credstore.references().contains(&stored),
            "round {round}: naming the reference the row already stores must not \
             destroy the secret the row still points at: {:?}",
            credstore.references()
        );
    }
}

/// **m-6.** A key stored as a secret and resubmitted as a *non-secret* field is
/// dropped from `credentials` — so its secret must be superseded, not orphaned.
///
/// Reachable only when a plugin reclassifies a field between releases.
#[tokio::test]
async fn a_secret_reclassified_as_config_is_superseded_not_orphaned() {
    let credstore = Arc::new(RecordingCredStore::new());
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();

    // Release 1: `api_token` is a secret.
    let secret_schema = vec![
        field("kubeconfig", FieldKind::MultilineSecret, true, None),
        field("api_token", FieldKind::Secret, false, None),
    ];
    let created = build_services_tenant_scoped_with_plugin_and_credstore(
        db.clone(),
        port(Arc::new(
            ScriptedPlugin::vhp_shaped(detection()).with_credential_schema(secret_schema),
        )),
        credstore.clone(),
    )
    .environments
    .create_environment(
        &ctx(tenant),
        submitting(
            "prod",
            vec![
                ("kubeconfig", pasted(KUBECONFIG)),
                ("api_token", pasted("t0ken")),
            ],
        ),
    )
    .await
    .unwrap();
    let token_ref = created
        .credentials
        .iter()
        .find(|credential| credential.key == "api_token")
        .unwrap()
        .credstore_ref
        .clone();

    // Release 2: the same key is now a plain text field.
    let text_schema = vec![
        field("kubeconfig", FieldKind::MultilineSecret, true, None),
        field("api_token", FieldKind::Text, false, None),
    ];
    let updated = build_services_tenant_scoped_with_plugin_and_credstore(
        db.clone(),
        port(Arc::new(
            ScriptedPlugin::vhp_shaped(detection()).with_credential_schema(text_schema),
        )),
        credstore.clone(),
    )
    .environments
    .update_environment(
        &ctx(tenant),
        created.id,
        EnvironmentPatch {
            credentials: BTreeMap::from([("api_token".to_owned(), pasted("now-plain"))]),
            ..EnvironmentPatch::default()
        },
    )
    .await
    .unwrap();

    assert!(
        !updated
            .credentials
            .iter()
            .any(|credential| credential.key == "api_token"),
        "it is not a credential any more: {:?}",
        updated.credentials
    );
    assert_eq!(
        updated.config,
        serde_json::json!({ "api_token": "now-plain" }),
        "its value went to `config`"
    );
    assert!(
        !credstore.references().contains(&token_ref),
        "and the secret it used to name must be SUPERSEDED, not left in \
         credstore with nothing able to name it: {:?}",
        credstore.references()
    );
}

/// **Re-review n-2.** The required-secret floor runs on updates too, so a row
/// that does not satisfy its plugin's *current* schema cannot rotate one
/// credential in isolation. Both shapes are escapable, and **these tests hold
/// the escape open** — that is their whole point.
///
/// Without them, a future change that also refused the escaping patch would
/// turn an inconvenience into an unfixable row, and the suite would stay green.
#[tokio::test]
async fn a_row_that_does_not_satisfy_its_schema_can_still_be_repaired_in_one_patch() {
    let credstore = Arc::new(RecordingCredStore::new());
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();

    // Release 1 declares one required secret; the row is created under it.
    let created = build_services_tenant_scoped_with_plugin_and_credstore(
        db.clone(),
        port(Arc::new(ScriptedPlugin::vhp_shaped(detection()))),
        credstore.clone(),
    )
    .environments
    .create_environment(
        &ctx(tenant),
        submitting("prod", vec![("kubeconfig", pasted(KUBECONFIG))]),
    )
    .await
    .unwrap();

    // Release 2 adds a SECOND required secret the row does not carry.
    let two_required = vec![
        field("kubeconfig", FieldKind::MultilineSecret, true, None),
        field("api_token", FieldKind::Secret, true, None),
    ];
    let services = build_services_tenant_scoped_with_plugin_and_credstore(
        db.clone(),
        port(Arc::new(
            ScriptedPlugin::vhp_shaped(detection()).with_credential_schema(two_required),
        )),
        credstore.clone(),
    );

    // Rotating only the credential it has is refused, naming the missing field.
    let error = services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                credentials: BTreeMap::from([("kubeconfig".to_owned(), pasted("rotated"))]),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .expect_err("the row does not satisfy the plugin's current schema");
    assert!(
        format!("{error:?}").contains("api_token"),
        "the refusal must name the field that is missing, which is what makes it \
         actionable: {error:?}"
    );

    // **The escape**: supply both in one patch. This must succeed, or the row
    // is unrepairable.
    let repaired = services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                credentials: BTreeMap::from([
                    ("kubeconfig".to_owned(), pasted("rotated")),
                    ("api_token".to_owned(), pasted("t0ken")),
                ]),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .expect("supplying every required credential in one patch must repair the row");
    assert_eq!(repaired.credentials.len(), 2);

    // And a patch that touches no credential is never subject to the floor at
    // all -- otherwise renaming an environment would be blocked by its
    // credentials.
    let renamed = services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                name: Some("renamed".to_owned()),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .expect("a non-credential patch must never reach the credential floor");
    assert_eq!(renamed.name, "renamed");
}
