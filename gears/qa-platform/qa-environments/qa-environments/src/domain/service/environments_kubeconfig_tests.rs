//! `EnvironmentsService` tests for the **pasted kubeconfig** path, against a REAL
//! in-memory `SQLite` database (the gear's own migrations plus the
//! `SeaORM`-backed repositories) and an in-memory credstore double whose `get`
//! actually works.
//!
//! # What these are for
//!
//! A kubeconfig's `users[].user.client-key-data` is a client **private key**.
//! The capability being tested is "an operator may paste one"; the property
//! being defended is "having pasted it, they cannot then find it anywhere but
//! credstore". So every test here asserts on an **outcome** — what the row
//! holds, what credstore holds, what the logs contain — rather than on the
//! shape of a string that was supposed to cause that outcome.
//!
//! The fixture document carries one distinctive fragment,
//! [`SECRET_FRAGMENT`], which appears nowhere else in this repository. Any
//! future change that echoed the document — into a log line, a `Debug`
//! rendering, an error message or a response body — puts that fragment
//! somewhere it is asserted absent, and fails.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use credstore_sdk::{CredStoreClientV1, SecretRef, SharingMode};
use qa_environments_sdk::{CredentialMaterial, EnvironmentPatch, NewEnvironment};
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use crate::api::rest::dto::{CreateEnvironmentReq, EnvironmentDto, UpdateEnvironmentReq};
use crate::domain::error::DomainError;
use crate::test_support::{
    CapturedLogs, FixedPluginPort, RecordingCredStore, ScriptedPlugin,
    build_services_tenant_scoped_with_plugin_and_credstore, ctx, inmem_db,
};

/// A realistic kubeconfig, including the field that makes the document secret.
const KUBECONFIG: &str = "apiVersion: v1\n\
     kind: Config\n\
     clusters:\n\
     - name: prod\n\
     \x20 cluster:\n\
     \x20   server: https://prod.example.com:6443\n\
     users:\n\
     - name: admin\n\
     \x20 user:\n\
     \x20   client-key-data: qaenv-kubeconfig-canary-alpha-4f2c\n";

/// The rotated document used by the update tests, with its own canary.
const ROTATED_KUBECONFIG: &str = "apiVersion: v1\n\
     kind: Config\n\
     users:\n\
     - name: admin\n\
     \x20 user:\n\
     \x20   client-key-data: qaenv-kubeconfig-canary-beta-9d71\n";

/// The fragment that must never leave credstore. Unique to this file.
const SECRET_FRAGMENT: &str = "qaenv-kubeconfig-canary-alpha-4f2c";
const ROTATED_FRAGMENT: &str = "qaenv-kubeconfig-canary-beta-9d71";

/// A caller-supplied reference that is a **valid** `SecretRef`
/// (`[a-zA-Z0-9_-]{1,255}`, see `credstore_sdk::SecretRef::new`) and does not
/// carry the generated prefix.
///
/// The distinction is load-bearing and was found by break-testing: an earlier
/// version of the two "leaves a caller-supplied secret alone" tests used
/// `credstore://team-a/prod`, which `SecretRef::new` rejects outright. Removing
/// the ownership guard from `forget_owned_secret` left those tests **passing**,
/// because the delete never got as far as the guard — they were measuring
/// `SecretRef`'s character set, not ownership. A bare name reaches the guard.
const CALLER_SUPPLIED_REF: &str = "team-a-prod-cluster";

/// The prefix `EnvironmentsService` mints generated references under. Spelled out
/// literally rather than imported so that a change to the constant has to be a
/// deliberate change to this expectation too.
///
/// Task 18b changed it from `qa-environments-kubeconfig-`, which named one
/// product's credential in the gear whose whole purpose is to stop doing that.
/// This edit is the deliberate one the comment above asks for.
const GENERATED_PREFIX: &str = "qa-environments-credential-";

/// The product every environment in this file belongs to.
///
/// # Why these fixtures have a product at all, since Task 18b
///
/// They did not, and that was review finding **I-1**. Task 18b folded the
/// pre-plugin `kubeconfig`/`kubeconfig_credstore_ref` pair into the
/// plugin-driven path (ruling F-8) and claimed **this file passing unmodified
/// was the evidence the fold preserved the pair's rules**. It was not: every
/// fixture here set `product_id: None`, so every test took the *productless*
/// branch — `store_unclassified_credential` → `store_legacy_pair`, a second
/// implementation of those rules — and `desugar_legacy_credential_pair`, the
/// fold itself, was never entered. The reviewer proved it by deleting the
/// fold's empty-reference filter and then its exactly-one-of refusal: all 231
/// tests passed both times.
///
/// So the fixtures now name a product and the harness resolves a plugin for
/// it, which is what makes this file the evidence it is claimed to be — the
/// pair's rules asserted against the code that serves a real deployment, where
/// every environment has a product.
///
/// The productless branch keeps its own coverage in
/// `environments_credentials_tests`.
fn product() -> Uuid {
    Uuid::from_u128(0x9001)
}

/// A successful observation, so the plugin double is a complete one. Nothing
/// in this file observes; it exists because `ScriptedPlugin` needs an outcome.
fn detection() -> qa_product_sdk::observation::PluginObservation {
    use qa_product_sdk::observation::{
        HealthOutcome, HealthState, ObservationOutcome, ObservedAttrs, PluginObservation,
    };
    PluginObservation {
        environment: ObservationOutcome::Detected(ObservedAttrs::default()),
        health: HealthOutcome::Checked {
            state: HealthState::Ok,
            detail: None,
        },
    }
}

/// Services with a real credstore double **and** a VHP-shaped plugin, so the
/// pre-plugin pair travels the fold rather than the productless branch.
fn services_with(
    db: toolkit_db::Db,
    credstore: Arc<RecordingCredStore>,
) -> Arc<crate::gear::ConcreteAppServices> {
    build_services_tenant_scoped_with_plugin_and_credstore(
        db,
        Arc::new(FixedPluginPort::new(Arc::new(ScriptedPlugin::vhp_shaped(
            detection(),
        )))),
        credstore,
    )
}

/// The one credstore reference an environment stores.
///
/// Task 19 dropped `Environment::kubeconfig_credstore_ref`; `credentials` is
/// where a reference lives now. Every fixture in this file declares a single
/// required secret, so "the sole entry" is well defined here — a plugin with
/// two would be `sole_required_secret_key() == None`, which this file does not
/// exercise (`environments_credentials_tests` does).
fn sole_ref(environment: &qa_environments_sdk::Environment) -> String {
    environment
        .credentials
        .first()
        .map(|credential| credential.credstore_ref.clone())
        .unwrap_or_default()
}

fn pasted(name: &str, document: &str) -> NewEnvironment {
    NewEnvironment {
        credentials: std::collections::BTreeMap::new(),
        kubeconfig_credstore_ref: None,
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
        credentials: std::collections::BTreeMap::new(),
        kubeconfig_credstore_ref: Some(credstore_ref.to_owned()),
        name: name.to_owned(),
        product_id: product(),
        description: None,
        kubeconfig: None,
        default_branch: None,
        is_default: false,
    }
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

/// The capability itself, plus the property that makes it safe: the paste
/// succeeds, the **row** holds a generated reference, and the **document** is
/// readable back out of credstore under exactly that reference.
#[tokio::test]
async fn a_pasted_kubeconfig_reaches_credstore_and_only_its_reference_reaches_the_row() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(&ctx(tenant), pasted("prod", KUBECONFIG))
        .await
        .unwrap();

    // Re-read through a *separate* call, so nothing below can be satisfied by
    // the create call's own return value (the rule `environments_tests` sets).
    let fetched = services
        .environments
        .get_environment(&ctx(tenant), created.id)
        .await
        .unwrap();

    assert!(
        sole_ref(&fetched).starts_with(GENERATED_PREFIX),
        "the stored value must be a generated reference, got {:?}",
        sole_ref(&fetched)
    );
    assert_eq!(sole_ref(&fetched), sole_ref(&created));

    // `infra::storage::mapper::environment_to_sdk` carries every content column of
    // `qa_environments` onto `Environment` (only `tenant_id` is dropped), so
    // scanning the round-tripped model scans the row.
    let row = format!("{fetched:?}");
    assert!(
        !row.contains(SECRET_FRAGMENT) && !row.contains("client-key-data"),
        "the document reached a database column: {row}"
    );

    // ...and it is genuinely retrievable, through the client trait rather than
    // by peeking at the double's map.
    let secret_ref = SecretRef::new(sole_ref(&fetched)).unwrap();
    let stored = credstore
        .get(&ctx(tenant), &secret_ref)
        .await
        .unwrap()
        .expect("the reference the row holds must resolve to a secret");
    assert_eq!(
        stored.value.as_bytes(),
        KUBECONFIG.as_bytes(),
        "the pasted document must come back byte-for-byte"
    );
    assert_eq!(
        credstore.sharing_of(&sole_ref(&fetched)),
        Some(SharingMode::Tenant),
        "written with the same sharing mode qa-catalog uses for SSH keys"
    );
}

/// Every paste gets its own reference — two environments must never share one
/// secret, or deleting either would break the other.
#[tokio::test]
async fn each_paste_gets_its_own_generated_reference() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let a = services
        .environments
        .create_environment(&ctx(tenant), pasted("a", KUBECONFIG))
        .await
        .unwrap();
    let b = services
        .environments
        .create_environment(&ctx(tenant), pasted("b", KUBECONFIG))
        .await
        .unwrap();

    assert_ne!(sole_ref(&a), sole_ref(&b));
    assert_eq!(credstore.len(), 2);
}

/// The reference path is the one that already worked; it must keep working,
/// and must not write anything to credstore.
#[tokio::test]
async fn a_supplied_reference_still_works_and_writes_nothing_to_credstore() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(
            &ctx(tenant),
            by_reference("prod", "credstore://team-a/prod"),
        )
        .await
        .unwrap();

    assert_eq!(sole_ref(&created), "credstore://team-a/prod");
    assert_eq!(
        credstore.len(),
        0,
        "a caller who already holds a reference must not cause a credstore write"
    );
}

/// Both fields is a validation error, and it has to name **both** — the caller
/// needs to know which one to drop, and `DomainError::Validation` carries only
/// a single `field`.
#[tokio::test]
async fn supplying_both_a_reference_and_a_document_is_a_validation_error_naming_both() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let new = NewEnvironment {
        credentials: std::collections::BTreeMap::new(),
        kubeconfig_credstore_ref: Some("credstore://team-a/prod".to_owned()),
        ..pasted("prod", KUBECONFIG)
    };
    let err = services
        .environments
        .create_environment(&ctx(tenant), new)
        .await
        .unwrap_err();

    let DomainError::Validation { field, message } = &err else {
        panic!("expected a validation error, got {err:?}");
    };
    let rendered = format!("{field}: {message}");
    assert!(
        rendered.contains("kubeconfig_credstore_ref") && rendered.contains("kubeconfig'"),
        "the error must name both fields, got {rendered}"
    );
    assert!(
        !rendered.contains(SECRET_FRAGMENT),
        "the rejection must not echo the document: {rendered}"
    );
    assert_eq!(
        credstore.len(),
        0,
        "a rejected create must reach no credstore"
    );
    assert!(
        services
            .environments
            .list_environments(&ctx(tenant), &ODataQuery::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

/// Neither field is still refused, and the refusal no longer names one
/// product's credential.
///
/// # The message changed at Task 18b, deliberately
///
/// It used to be `kubeconfig_credstore_ref: must not be empty` — the field
/// name this endpoint always gave, kept so a caller already handling it kept
/// working. Once these fixtures gained a product (review finding I-1) the
/// request travels the plugin path, where the refusal is the platform floor:
/// an environment needs at least one credential, expressed in terms of "the
/// fields this product's plugin declares" rather than in terms of a
/// kubeconfig.
///
/// **That is an improvement, not a regression to absorb quietly.** The old
/// string put one product's field name in a message every product would see,
/// which is the coupling this whole plan exists to remove. What has to stay
/// true — and is asserted below — is that the request is still *refused*, and
/// that both spellings of "absent" reach the same refusal.
#[tokio::test]
async fn supplying_neither_on_create_is_still_refused_without_naming_a_kubeconfig() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    // Absent, and the empty string that the previously-required `String` field
    // used to carry, must both land on the same error.
    for absent in [None, Some(String::new())] {
        let new = NewEnvironment {
            credentials: std::collections::BTreeMap::new(),
            ..pasted("prod", KUBECONFIG)
        };
        let new = NewEnvironment {
            credentials: std::collections::BTreeMap::new(),
            kubeconfig: None,
            ..new
        };
        let err = services
            .environments
            .create_environment(&ctx(tenant), new)
            .await
            .unwrap_err();
        let DomainError::Validation { field, message } = &err else {
            panic!("for {absent:?}: expected a validation error, got {err:?}");
        };
        assert_eq!(
            field, "credentials",
            "for {absent:?}: the refusal is about credentials in general now, not about \
             one product's field"
        );
        assert!(
            message.contains("at least one credential"),
            "for {absent:?}: got {message}"
        );
        assert!(
            !message.contains("kubeconfig"),
            "for {absent:?}: the message must not name one product's credential -- that is \
             the coupling this plan removes: {message}"
        );
    }
}

/// A blank paste is rejected before anything is written, exactly as
/// `create_ssh_key` rejects a blank PEM.
#[tokio::test]
async fn a_blank_pasted_kubeconfig_is_rejected_before_credstore_is_touched() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    for blank in ["", "   ", "\t\n "] {
        let err = services
            .environments
            .create_environment(&ctx(tenant), pasted("prod", blank))
            .await
            .unwrap_err();
        assert!(
            matches!(
                &err,
                DomainError::Validation { field, message }
                    if field == "kubeconfig" && message == "must not be empty"
            ),
            "for {blank:?}: got {err:?}"
        );
    }
    assert_eq!(credstore.len(), 0);
}

/// "Material goes to credstore FIRST" has a corollary that is worth pinning
/// separately: if that write fails, there is no environment.
#[tokio::test]
async fn a_failed_credstore_write_creates_no_environment() {
    let credstore = Arc::new(RecordingCredStore::always_failing());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let err = services
        .environments
        .create_environment(&ctx(tenant), pasted("prod", KUBECONFIG))
        .await
        .unwrap_err();

    assert!(matches!(err, DomainError::CredStore(_)), "got {err:?}");
    assert!(
        !format!("{err:?}").contains(SECRET_FRAGMENT),
        "the failure must not carry the document: {err:?}"
    );
    assert!(
        services
            .environments
            .list_environments(&ctx(tenant), &ODataQuery::default())
            .await
            .unwrap()
            .items
            .is_empty(),
        "no row may exist when the secret could not be stored"
    );
}

/// The other half of that ordering: the secret is written before the row, so a
/// failed row write must not leave an orphan credstore could never name again.
/// A duplicate name is the realistic cause.
#[tokio::test]
async fn a_failed_row_write_deletes_the_secret_it_had_just_written() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let first = services
        .environments
        .create_environment(&ctx(tenant), pasted("dup", KUBECONFIG))
        .await
        .unwrap();
    assert_eq!(credstore.len(), 1);

    let err = services
        .environments
        .create_environment(&ctx(tenant), pasted("dup", KUBECONFIG))
        .await
        .unwrap_err();

    assert!(
        matches!(&err, DomainError::EnvironmentNameExists { name } if name == "dup"),
        "the original write error must propagate, not the cleanup outcome: {err:?}"
    );
    assert_eq!(
        credstore.references(),
        vec![sole_ref(&first)],
        "only the surviving environment's secret may remain"
    );
}

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

/// Replacing a pasted kubeconfig: new secret, new reference on the row, and the
/// superseded secret removed rather than orphaned.
#[tokio::test]
async fn pasting_a_replacement_rotates_the_reference_and_removes_the_old_secret() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(&ctx(tenant), pasted("prod", KUBECONFIG))
        .await
        .unwrap();
    let old_ref = sole_ref(&created).clone();

    services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                kubeconfig: Some(CredentialMaterial::new(ROTATED_KUBECONFIG.to_owned())),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap();

    let fetched = services
        .environments
        .get_environment(&ctx(tenant), created.id)
        .await
        .unwrap();
    let new_ref = sole_ref(&fetched).clone();

    assert!(new_ref.starts_with(GENERATED_PREFIX));
    assert_ne!(
        new_ref, old_ref,
        "the reference must rotate with the document"
    );
    assert_eq!(
        credstore.references(),
        vec![new_ref.clone()],
        "the superseded secret must be gone, leaving exactly one"
    );

    let stored = credstore
        .get(&ctx(tenant), &SecretRef::new(new_ref).unwrap())
        .await
        .unwrap()
        .expect("the new reference must resolve");
    assert_eq!(stored.value.as_bytes(), ROTATED_KUBECONFIG.as_bytes());
}

/// A reference this gear did **not** mint may be shared with other environments or
/// other systems entirely, so replacing it must not delete it.
#[tokio::test]
async fn pasting_over_a_caller_supplied_reference_leaves_that_secret_alone() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    credstore.seed(CALLER_SUPPLIED_REF, "someone else's kubeconfig");
    let created = services
        .environments
        .create_environment(&ctx(tenant), by_reference("prod", CALLER_SUPPLIED_REF))
        .await
        .unwrap();

    services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                kubeconfig: Some(CredentialMaterial::new(ROTATED_KUBECONFIG.to_owned())),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap();

    let fetched = services
        .environments
        .get_environment(&ctx(tenant), created.id)
        .await
        .unwrap();
    assert!(sole_ref(&fetched).starts_with(GENERATED_PREFIX));
    assert!(
        credstore
            .references()
            .contains(&CALLER_SUPPLIED_REF.to_owned()),
        "a secret this gear did not create is not this gear's to delete: {:?}",
        credstore.references()
    );
    assert_eq!(credstore.len(), 2);
}

/// Naming a new reference (rather than pasting) still works, and still writes
/// nothing to credstore.
#[tokio::test]
async fn updating_to_a_new_reference_still_works() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(&ctx(tenant), by_reference("prod", "credstore://old"))
        .await
        .unwrap();

    let updated = services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                kubeconfig_credstore_ref: Some("credstore://new".to_owned()),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(sole_ref(&updated), "credstore://new");
    assert_eq!(credstore.len(), 0);
}

/// `PATCH`ing a **reference** over a gear-generated one must not orphan the
/// generated secret.
///
/// This starts from a *pasted* environment deliberately.
/// `updating_to_a_new_reference_still_works` starts from `credstore://old`,
/// which was never gear-generated and has no secret behind it at all, so it
/// passes against code that cleans up nothing. Measured before the fix: the row
/// held `team-a-prod-cluster` while credstore still held
/// `qa-environments-kubeconfig-…` — unreachable by any caller, because the only
/// thing that ever named it was the column the patch just overwrote. That is
/// the orphan `delete_ssh_key` exists to avoid, and it was reachable from the
/// UI, whose `updatePlatformReqFromForm` emits `kubeconfig_credstore_ref` for
/// any single-line value.
#[tokio::test]
async fn patching_a_reference_over_a_generated_one_removes_the_superseded_secret() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(&ctx(tenant), pasted("prod", KUBECONFIG))
        .await
        .unwrap();
    let generated = sole_ref(&created).clone();
    assert!(
        generated.starts_with(GENERATED_PREFIX),
        "the fixture must start from a *generated* reference or this test cannot \
         see the defect: {generated}"
    );
    assert_eq!(credstore.references(), vec![generated.clone()]);

    // The replacement names a secret some other system registered.
    credstore.seed(CALLER_SUPPLIED_REF, "someone else's kubeconfig");

    let updated = services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                kubeconfig_credstore_ref: Some(CALLER_SUPPLIED_REF.to_owned()),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(sole_ref(&updated), CALLER_SUPPLIED_REF);
    assert!(
        !credstore.references().contains(&generated),
        "the superseded generated secret is orphaned — nothing names it any \
         more: {:?}",
        credstore.references()
    );
    assert_eq!(
        credstore.references(),
        vec![CALLER_SUPPLIED_REF.to_owned()],
        "and the caller's own secret must survive"
    );
}

/// The same PATCH, but naming the reference the row **already holds**.
///
/// The cleanup above removes the *superseded* reference; a no-op PATCH supplies
/// one that is not superseded. Deleting it would destroy the secret the row
/// still points at, silently breaking an environment through a request that changed
/// nothing — so the cleanup is guarded by a comparison against the reference
/// the row ended up with, not by "a reference was supplied".
#[tokio::test]
async fn patching_the_reference_the_row_already_holds_keeps_its_secret() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(&ctx(tenant), pasted("prod", KUBECONFIG))
        .await
        .unwrap();
    let generated = sole_ref(&created).clone();

    let updated = services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                kubeconfig_credstore_ref: Some(generated.clone()),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(sole_ref(&updated), generated);
    assert_eq!(
        credstore.references(),
        vec![generated.clone()],
        "a PATCH naming the reference already stored must not delete its secret"
    );
    let stored = credstore
        .get(&ctx(tenant), &SecretRef::new(generated).unwrap())
        .await
        .unwrap()
        .expect("the reference the row still holds must still resolve");
    assert_eq!(stored.value.as_bytes(), KUBECONFIG.as_bytes());
}

/// Create and update must read an empty `kubeconfig_credstore_ref` the same
/// way: as **not supplied**.
///
/// `resolve_kubeconfig_source` has always filtered it, so
/// `{"kubeconfig_credstore_ref": "", "kubeconfig": "…"}` is accepted on POST.
/// Update used to reject the identical payload as "both supplied".
#[tokio::test]
async fn an_empty_reference_alongside_a_paste_is_a_paste_on_update_as_on_create() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    // The create half, stated here so the two readings are compared in one
    // place rather than asserted apart and assumed equal.
    let mut new = pasted("prod", KUBECONFIG);
    new.kubeconfig_credstore_ref = Some(String::new());
    let created = services
        .environments
        .create_environment(&ctx(tenant), new)
        .await
        .expect("an empty reference alongside a paste is a paste on create");
    assert!(sole_ref(&created).starts_with(GENERATED_PREFIX));

    let updated = services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                kubeconfig_credstore_ref: Some(String::new()),
                kubeconfig: Some(CredentialMaterial::new(ROTATED_KUBECONFIG.to_owned())),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .expect("and must be a paste on update too");

    assert!(
        sole_ref(&updated).starts_with(GENERATED_PREFIX),
        "the paste must win, not the empty reference: {}",
        sole_ref(&updated)
    );
    assert_eq!(
        credstore.references(),
        vec![sole_ref(&updated)],
        "and the superseded secret must still be cleaned up"
    );
}

/// An empty `kubeconfig_credstore_ref` **alone** on a PATCH stays the rejection
/// it has always been.
///
/// "Empty means not supplied" is the create reading, and on create not-supplied
/// is the required-field error. On update it would mean "leave the kubeconfig
/// alone" — but the caller *named* the field, and `Some("")` reaching the
/// repository writes `''` into `kubeconfig_credstore_ref NOT NULL`, i.e. blanks
/// a live environment's only pointer to its credentials. So the field is rejected
/// rather than silently ignored.
#[tokio::test]
async fn an_empty_reference_alone_on_update_is_still_rejected() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(&ctx(tenant), pasted("prod", KUBECONFIG))
        .await
        .unwrap();

    let err = services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                kubeconfig_credstore_ref: Some(String::new()),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .expect_err("an empty reference is not a storable reference");
    match err {
        DomainError::Validation { field, message } => {
            assert_eq!(field, "kubeconfig_credstore_ref");
            assert_eq!(message, "must not be empty");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
    assert_eq!(
        credstore.references(),
        vec![sole_ref(&created)],
        "a rejected patch must neither write nor remove a secret"
    );
}

#[tokio::test]
async fn supplying_both_on_update_is_a_validation_error_naming_both() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(&ctx(tenant), pasted("prod", KUBECONFIG))
        .await
        .unwrap();
    let before = credstore.references();

    let err = services
        .environments
        .update_environment(
            &ctx(tenant),
            created.id,
            EnvironmentPatch {
                kubeconfig_credstore_ref: Some("credstore://new".to_owned()),
                kubeconfig: Some(CredentialMaterial::new(ROTATED_KUBECONFIG.to_owned())),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap_err();

    let DomainError::Validation { field, message } = &err else {
        panic!("expected a validation error, got {err:?}");
    };
    let rendered = format!("{field}: {message}");
    assert!(
        rendered.contains("kubeconfig_credstore_ref") && rendered.contains("kubeconfig'"),
        "the error must name both fields, got {rendered}"
    );
    assert_eq!(credstore.references(), before, "nothing may have changed");
}

/// A patch that does not mention the kubeconfig at all leaves it exactly where
/// it was — "neither" is only an error on create.
#[tokio::test]
async fn a_patch_that_does_not_mention_the_kubeconfig_leaves_it_untouched() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(&ctx(tenant), pasted("prod", KUBECONFIG))
        .await
        .unwrap();

    let updated = services
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
        .unwrap();

    assert_eq!(updated.name, "renamed");
    assert_eq!(sole_ref(&updated), sole_ref(&created));
    assert_eq!(credstore.references(), vec![sole_ref(&created)]);
}

/// Pasting at an environment that does not exist must not leave a secret behind:
/// the row is resolved first, so credstore is never touched.
#[tokio::test]
async fn pasting_at_a_missing_environment_writes_nothing() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let err = services
        .environments
        .update_environment(
            &ctx(tenant),
            Uuid::new_v4(),
            EnvironmentPatch {
                kubeconfig: Some(CredentialMaterial::new(KUBECONFIG.to_owned())),
                ..EnvironmentPatch::default()
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::EnvironmentNotFound { .. }),
        "got {err:?}"
    );
    assert_eq!(credstore.len(), 0);
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

/// Deleting an environment whose kubeconfig this gear stored must take the secret
/// with it — otherwise nothing can ever name that secret again, which is the
/// orphan `delete_ssh_key` exists to avoid.
#[tokio::test]
async fn deleting_an_environment_removes_a_secret_this_gear_generated() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    let created = services
        .environments
        .create_environment(&ctx(tenant), pasted("prod", KUBECONFIG))
        .await
        .unwrap();
    assert_eq!(credstore.len(), 1);

    services
        .environments
        .delete_environment(&ctx(tenant), created.id)
        .await
        .unwrap();

    assert_eq!(credstore.len(), 0, "the generated secret must be removed");
}

/// ...but a caller-supplied reference is not this gear's to delete.
#[tokio::test]
async fn deleting_an_environment_leaves_a_caller_supplied_secret_alone() {
    let credstore = Arc::new(RecordingCredStore::new());
    let services = services_with(inmem_db().await, credstore.clone());
    let tenant = Uuid::new_v4();

    credstore.seed(CALLER_SUPPLIED_REF, "someone else's kubeconfig");
    let created = services
        .environments
        .create_environment(&ctx(tenant), by_reference("prod", CALLER_SUPPLIED_REF))
        .await
        .unwrap();

    services
        .environments
        .delete_environment(&ctx(tenant), created.id)
        .await
        .unwrap();

    assert_eq!(
        credstore.references(),
        vec![CALLER_SUPPLIED_REF.to_owned()],
        "a secret this gear did not create must survive the environment"
    );
}

// ---------------------------------------------------------------------------
// The security keystone
// ---------------------------------------------------------------------------
//
// `CapturedLogs` / `CapturedLogsWriter` moved to `crate::test_support` (review
// finding #29-followup) so `environments_sea_repo`'s tests can reuse the same
// raw-byte capture instead of asserting only half of what a test's name
// promises. See the doc comment there for the two hazards that apply to every
// caller.

/// The document must not appear in a log line, a `Debug` rendering, or a
/// response body.
///
/// The log half runs against the real emitted output at `TRACE`: every
/// `tracing` event and span field produced on this thread while the create,
/// update and delete paths run. Its reach is exactly that and no further —
/// `sqlx`/`SeaORM` emit through the `log` crate and this subscriber installs
/// no `log` bridge, so their statement logs are **not** in this buffer.
/// Whether the document reaches a database column is therefore not this
/// test's job; `a_pasted_kubeconfig_reaches_credstore_and_only_its_reference_reaches_the_row`
/// covers that by reading the row back.
///
/// An absence assertion is worthless on its own — it passes just as happily
/// when nothing was captured — so the test first proves the capture is seeing
/// this code path, and that it renders `#[instrument]` span fields (the other
/// way a document could reach a log).
/// Every type that can carry the document, rendered the way a stray
/// `debug!("{x:?}")` would render it.
///
/// Extracted from the leak test below so that test stays under
/// `clippy::too_many_lines` as channels are added — and adding a channel
/// here is exactly what review finding I-6 asked for.
fn every_document_carrying_rendering() -> Vec<String> {
    // Every type that can carry the document, rendered the way a stray
    // `debug!("{x:?}")` would render it.
    //
    // # The plugin-shaped channel is here too, and it was not (review I-6)
    //
    // Task 18b added a second way to submit a document —
    // `CredentialSubmission::Material`, the `credentials` map on both request
    // models, and `CredentialSubmissionDto::Material` on both DTOs — and this
    // array's five entries covered only the pre-plugin `kubeconfig` field.
    // Worse, the task's own diff added `credentials: None` to the two DTO
    // literals below, so the array named the new field and deliberately left
    // it empty. The reviewer replaced the redacting `Debug` arm with a leaking
    // one and all 231 tests passed: a `tracing::debug!(?req)` in any handler
    // would then have put a kubeconfig's `client-key-data` private key into
    // the logs, which is the 2026-08-28 incident's exact shape.
    let submitted_document = || {
        std::collections::BTreeMap::from([(
            "kubeconfig".to_owned(),
            qa_environments_sdk::CredentialSubmission::Material(CredentialMaterial::new(
                KUBECONFIG.to_owned(),
            )),
        )])
    };
    let submitted_dto = || {
        Some(std::collections::BTreeMap::from([(
            "kubeconfig".to_owned(),
            crate::api::rest::dto::CredentialSubmissionDto::Material(KUBECONFIG.to_owned()),
        )]))
    };
    vec![
        format!("{:?}", CredentialMaterial::new(KUBECONFIG.to_owned())),
        format!("{:?}", pasted("prod", KUBECONFIG)),
        format!(
            "{:?}",
            EnvironmentPatch {
                kubeconfig: Some(CredentialMaterial::new(KUBECONFIG.to_owned())),
                ..EnvironmentPatch::default()
            }
        ),
        format!(
            "{:?}",
            CreateEnvironmentReq {
                name: "prod".to_owned(),
                product_id: None,
                description: None,
                kubeconfig_credstore_ref: None,
                kubeconfig: Some(KUBECONFIG.to_owned()),
                credentials: None,
                default_branch: None,
                is_default: None,
            }
        ),
        format!(
            "{:?}",
            UpdateEnvironmentReq {
                kubeconfig: Some(KUBECONFIG.to_owned()),
                credentials: None,
                ..UpdateEnvironmentReq::default()
            }
        ),
        // ── the plugin-shaped channel, Task 18b's own ──
        format!(
            "{:?}",
            qa_environments_sdk::CredentialSubmission::Material(CredentialMaterial::new(
                KUBECONFIG.to_owned()
            ))
        ),
        format!(
            "{:?}",
            NewEnvironment {
                credentials: submitted_document(),
                ..pasted("prod", KUBECONFIG)
            }
        ),
        format!(
            "{:?}",
            EnvironmentPatch {
                credentials: submitted_document(),
                ..EnvironmentPatch::default()
            }
        ),
        format!(
            "{:?}",
            CreateEnvironmentReq {
                name: "prod".to_owned(),
                product_id: None,
                description: None,
                kubeconfig_credstore_ref: None,
                kubeconfig: None,
                credentials: submitted_dto(),
                default_branch: None,
                is_default: None,
            }
        ),
        format!(
            "{:?}",
            UpdateEnvironmentReq {
                credentials: submitted_dto(),
                ..UpdateEnvironmentReq::default()
            }
        ),
    ]
}

#[tokio::test]
async fn the_document_never_reaches_a_log_line_a_debug_rendering_or_a_response_body() {
    let logs = CapturedLogs::default();
    let updated = {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        // `#[tokio::test]` runs on a current-thread runtime, so a
        // thread-local default subscriber covers every `.await` below.
        let _guard = tracing::subscriber::set_default(subscriber);

        let credstore = Arc::new(RecordingCredStore::new());
        let services = services_with(inmem_db().await, credstore.clone());
        let tenant = Uuid::new_v4();

        // `tracing` caches each callsite's `Interest` **globally**, decided by
        // whichever thread reaches it first. The rest of this crate's suite
        // runs concurrently with no subscriber installed, so a callsite they
        // touch first is cached as `never` and this subscriber never sees it —
        // which made this test pass alone and fail in the full run, with the
        // "capture is not seeing this code path" guard firing. Measured, not
        // theorised.
        //
        // So: drive the whole path once to force every callsite to register,
        // rebuild the cache against *this* subscriber, then discard what the
        // warm-up produced and measure the real run.
        let warmup = services
            .environments
            .create_environment(&ctx(tenant), pasted("warmup", KUBECONFIG))
            .await
            .unwrap();
        services
            .environments
            .update_environment(
                &ctx(tenant),
                warmup.id,
                EnvironmentPatch {
                    kubeconfig: Some(CredentialMaterial::new(ROTATED_KUBECONFIG.to_owned())),
                    ..EnvironmentPatch::default()
                },
            )
            .await
            .unwrap();
        services
            .environments
            .delete_environment(&ctx(tenant), warmup.id)
            .await
            .unwrap();
        tracing::callsite::rebuild_interest_cache();
        logs.clear();

        let created = services
            .environments
            .create_environment(&ctx(tenant), pasted("prod", KUBECONFIG))
            .await
            .unwrap();
        let updated = services
            .environments
            .update_environment(
                &ctx(tenant),
                created.id,
                EnvironmentPatch {
                    kubeconfig: Some(CredentialMaterial::new(ROTATED_KUBECONFIG.to_owned())),
                    ..EnvironmentPatch::default()
                },
            )
            .await
            .unwrap();
        // Exercise delete's logging too, on a throwaway environment.
        let doomed = services
            .environments
            .create_environment(&ctx(tenant), pasted("doomed", KUBECONFIG))
            .await
            .unwrap();
        services
            .environments
            .delete_environment(&ctx(tenant), doomed.id)
            .await
            .unwrap();
        updated
    };

    let emitted = logs.text();

    // Guard the guard: without these, every assertion below would pass against
    // an empty buffer.
    for expected in [
        "Successfully created environment",
        "Successfully updated environment",
        "Successfully deleted environment",
    ] {
        assert!(
            emitted.contains(expected),
            "the log capture is not seeing this code path ({expected:?} missing), \
             so the absence assertions below would be vacuous"
        );
    }
    assert!(
        emitted.contains("create_environment{name=prod}"),
        "the capture must render `#[instrument]` span fields, or a document \
         recorded as one would slip past the assertions below"
    );

    for canary in [SECRET_FRAGMENT, ROTATED_FRAGMENT, "client-key-data"] {
        assert!(
            !emitted.contains(canary),
            "the kubeconfig reached a log line (found {canary:?})"
        );
    }

    let renderings = every_document_carrying_rendering();

    for rendered in &renderings {
        assert!(
            !rendered.contains(SECRET_FRAGMENT) && !rendered.contains("client-key-data"),
            "a Debug rendering carries the document: {rendered}"
        );
        assert!(
            rendered.contains("REDACTED"),
            "a type carrying the document must say so, rather than silently \
             omitting the field: {rendered}"
        );
    }

    // The response body: what a REST caller actually gets back.
    let updated_ref = sole_ref(&updated);
    assert!(
        updated_ref.starts_with(GENERATED_PREFIX),
        "the fixture must hold a generated reference, or the absence assertion \
         below could pass against a value that was never there: {updated_ref}"
    );
    let body = serde_json::to_string(&EnvironmentDto::from(updated)).unwrap();
    assert!(
        !body.contains(SECRET_FRAGMENT)
            && !body.contains(ROTATED_FRAGMENT)
            && !body.contains("client-key-data"),
        "the response body carries the document: {body}"
    );
    assert!(
        !body.contains("\"kubeconfig\""),
        "the response DTO must carry no kubeconfig field at all: {body}"
    );
    // ...and, since 2026-08-27, not the **reference** either. `SharingMode::Tenant`
    // means whoever holds the reference can read the document back through
    // credstore's own `GET /credstore/v1/secrets/{ref}`, and `EnvironmentDto` goes
    // to every `qa.platform` GET/LIST-authorized caller — so publishing it handed
    // every tenant member a working read path to a pasted private key. This is
    // `SshKeyDto`'s convention (`qa-catalog/.../api/rest/dto.rs`), which withholds
    // its own `credstore_ref` for exactly this reason.
    //
    // The reference itself is asserted absent rather than just the field name, so
    // re-publishing it under any other key fails too.
    let reference = &updated_ref;
    assert!(
        !body.contains("kubeconfig_credstore_ref"),
        "the response DTO must not publish the credstore reference: {body}"
    );
    assert!(
        !body.contains(reference.as_str()),
        "the reference {reference} reached the response body under some other \
         name: {body}"
    );
}
