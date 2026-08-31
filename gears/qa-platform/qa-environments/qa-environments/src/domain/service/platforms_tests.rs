//! `PlatformsService` tests for the `default_branch` override, against a REAL
//! in-memory `SQLite` database — the gear's actual migrations plus the
//! `SeaORM`-backed repositories, as `tests_tenant_scoping` does.
//!
//! # Why these are DB-backed rather than mock-backed
//!
//! Two independent reasons, both learned here rather than assumed.
//!
//! **A `SeaORM` entity's table and column names are runtime strings**, so
//! `platform::Model::default_branch` compiling proves nothing about the schema.
//! `m20260814_000006_platform_default_branch` covers the column directly; these
//! cover the *service* path onto it, which is the one an operator actually uses.
//!
//! **Task 9b's coverage hole was in this exact gear**: no DB-backed fixture ever
//! wrote a non-`NULL` `observed_version`, so a value dropped between the row and
//! the SDK model was invisible to all 46 tests then shipped — a `NULL` survives
//! almost any mistake unchanged. Every test below therefore writes a
//! **non-`NULL`** `default_branch` and asserts on what comes back out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use qa_environments_sdk::{KubeconfigMaterial, NewPlatform, PlatformPatch};

use crate::domain::error::DomainError;
use crate::domain::observation::{ClusterHealth, DetectedPlatform, NodeSummary};
use crate::domain::ports::{HealthOutcome, ObservationOutcome, PlatformObservation};
use crate::test_support::{
    ScriptedObserver, build_services_tenant_scoped, build_services_tenant_scoped_with_observer,
    ctx, inmem_db,
};
use uuid::Uuid;

fn new_platform(name: &str, default_branch: Option<&str>) -> NewPlatform {
    NewPlatform {
        name: name.to_owned(),
        product_id: None,
        description: None,
        kubeconfig_credstore_ref: Some("credstore://test".to_owned()),
        kubeconfig: None,
        default_branch: default_branch.map(str::to_owned),
        is_default: false,
    }
}

/// The value an operator sets on create must survive the whole way out — write
/// through the service, read back through a *separate* `get_platform` call so
/// the assertion cannot be satisfied by the create call's own return value.
#[tokio::test]
async fn a_default_branch_set_at_create_survives_the_round_trip() {
    let db = inmem_db().await;
    let services = build_services_tenant_scoped(db);
    let tenant = Uuid::new_v4();

    let created = services
        .platforms
        .create_platform(&ctx(tenant), new_platform("pinned", Some("release-9.0")))
        .await
        .unwrap();
    assert_eq!(created.default_branch.as_deref(), Some("release-9.0"));

    let fetched = services
        .platforms
        .get_platform(&ctx(tenant), created.id)
        .await
        .unwrap();
    assert_eq!(
        fetched.default_branch.as_deref(),
        Some("release-9.0"),
        "the override must reach the database and come back; a fixture that \
         only ever wrote NULL would not have proven this (Task 9b)"
    );
}

/// No override is the normal case and must not become an empty string.
#[tokio::test]
async fn a_platform_created_without_an_override_reads_back_as_none() {
    let db = inmem_db().await;
    let services = build_services_tenant_scoped(db);
    let tenant = Uuid::new_v4();

    let created = services
        .platforms
        .create_platform(&ctx(tenant), new_platform("unpinned", None))
        .await
        .unwrap();

    assert_eq!(created.default_branch, None);
    let fetched = services
        .platforms
        .get_platform(&ctx(tenant), created.id)
        .await
        .unwrap();
    assert_eq!(fetched.default_branch, None);
}

/// Trimmed, and blank normalised to "no override" — on the **create** path.
///
/// The source system's create path does exactly this before binding the column
/// (`manager/src/services/platforms.rs:385-392`).
///
/// **What this test does and does not buy, stated precisely** — an earlier
/// version of this comment claimed an untrimmed `"  release-9.0\n"` would reach
/// qa-runs and be used verbatim as a git ref. That is **false**:
/// `resolve_branch` trims its middle tier too, so the branch qa-runs actually
/// syncs is correct either way. Checked rather than assumed, and the wrong
/// rationale is recorded here because it is the more tempting one to write.
///
/// The real value is twofold and smaller. First, **parity**: the stored column
/// is byte-identical to what the source system would have stored, which matters
/// because the column is read by operators and by any future direct consumer,
/// not only by `resolve_branch`. Second, the **blank** half is load-bearing on
/// its own: a stored `""` is not merely untidy, it is a value `resolve_branch`
/// must filter to reach the repository tier at all, and relying on that filter
/// alone would put the correctness of the whole chain in the other gear's hands.
/// See `resolve_branch`'s "the platform tier's filter is redundant, and stays"
/// section for why both sides keep their half.
#[tokio::test]
async fn the_create_path_trims_the_override_and_treats_blank_as_absent() {
    let db = inmem_db().await;
    let services = build_services_tenant_scoped(db);
    let tenant = Uuid::new_v4();

    let trimmed = services
        .platforms
        .create_platform(
            &ctx(tenant),
            new_platform("padded", Some("  release-9.0\n")),
        )
        .await
        .unwrap();
    assert_eq!(trimmed.default_branch.as_deref(), Some("release-9.0"));

    for (name, blank) in [("blank-empty", ""), ("blank-spaces", "   ")] {
        let created = services
            .platforms
            .create_platform(&ctx(tenant), new_platform(name, Some(blank)))
            .await
            .unwrap();
        assert_eq!(
            created.default_branch, None,
            "a blank override is not a branch name; it must be stored as NULL \
             so the repository default applies, as legacy does"
        );
    }
}

/// The length boundary, on both write paths.
///
/// This asserts the **service's** check, not the database's: on `SQLite` the
/// column is declared `TEXT` and has no width to enforce, so the database half is
/// untestable here — see `validate_default_branch`'s doc for that, and for why the
/// bound is 512 (it matches `qa_runs.test_version`, where this value ends up).
///
/// 512 characters must be accepted and 513 rejected: an off-by-one rejecting 512
/// would turn a storable value into a 400, and one accepting 513 would hand the
/// problem to a `MySQL` that truncates silently.
#[tokio::test]
async fn an_override_wider_than_the_column_is_a_validation_error_on_both_paths() {
    let db = inmem_db().await;
    let services = build_services_tenant_scoped(db);
    let tenant = Uuid::new_v4();

    let at_limit = "b".repeat(512);
    let over_limit = "b".repeat(513);

    let ok = services
        .platforms
        .create_platform(&ctx(tenant), new_platform("at-limit", Some(&at_limit)))
        .await
        .unwrap();
    assert_eq!(ok.default_branch.as_deref(), Some(at_limit.as_str()));

    let create_err = services
        .platforms
        .create_platform(&ctx(tenant), new_platform("over", Some(&over_limit)))
        .await
        .unwrap_err();
    assert!(
        matches!(
            create_err,
            DomainError::Validation { ref field, .. } if field == "default_branch"
        ),
        "an over-long override must be a named validation error, not a 500 or a \
         silent truncation; got {create_err:?}"
    );

    let update_err = services
        .platforms
        .update_platform(
            &ctx(tenant),
            ok.id,
            PlatformPatch {
                default_branch: Some(Some(over_limit)),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            update_err,
            DomainError::Validation { ref field, .. } if field == "default_branch"
        ),
        "the patch path must check too; got {update_err:?}"
    );
}

/// The bound counts **characters, not bytes** — the regression this test exists
/// for, and the one the length check originally had.
///
/// `validate_default_branch` compared `str::len()` until the Task 13b review
/// caught it. `VARCHAR(512)` counts *characters* on both server dialects, so a
/// multi-byte branch name well inside the column's capacity was rejected — and
/// rejected with a message naming a limit in "characters" that the value did not
/// exceed. Git refs are UTF-8 and non-ASCII branch names are legal, so this was
/// reachable, not theoretical.
///
/// 400 Cyrillic characters is 800 bytes: over the old byte bound of 255, over even
/// a byte bound of 512, and comfortably inside 512 characters. It must be stored,
/// intact, and it must survive the round trip rather than being truncated to a
/// different valid-looking ref.
#[tokio::test]
async fn a_multibyte_override_is_measured_in_characters_not_bytes() {
    let db = inmem_db().await;
    let services = build_services_tenant_scoped(db);
    let tenant = Uuid::new_v4();

    // `\u{0431}` is Cyrillic small letter BE, written as an escape rather than the
    // glyph because `clippy::non_ascii_literal` denies non-ASCII in source
    // literals. It is two bytes in UTF-8, which is the property under test.
    let cyrillic = "\u{0431}".repeat(400);
    assert_eq!(cyrillic.chars().count(), 400, "400 characters");
    assert_eq!(
        cyrillic.len(),
        800,
        "but 800 bytes, which is the whole point"
    );

    let created = services
        .platforms
        .create_platform(&ctx(tenant), new_platform("cyrillic", Some(&cyrillic)))
        .await
        .expect("a 400-character branch name fits VARCHAR(512) and must be accepted");

    let fetched = services
        .platforms
        .get_platform(&ctx(tenant), created.id)
        .await
        .unwrap();
    assert_eq!(
        fetched.default_branch.as_deref(),
        Some(cyrillic.as_str()),
        "and it must come back byte-for-byte; a truncated ref is a different, \
         valid-looking branch name"
    );

    // The bound still bites in characters: 513 characters is over, however few
    // bytes that happened to be.
    let too_many = "\u{0431}".repeat(513);
    let err = services
        .platforms
        .create_platform(&ctx(tenant), new_platform("cyrillic-over", Some(&too_many)))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::Validation { ref field, .. } if field == "default_branch"
        ),
        "513 characters must still be rejected; got {err:?}"
    );
}

/// The three states of `PlatformPatch::default_branch`, which is the reason it
/// is `Option<Option<String>>` rather than `Option<String>`.
///
/// Legacy expresses the same three in SQL via its `"__NULL__"` sentinel
/// (`manager/src/services/platforms.rs:447-496`): absent keeps, blank clears,
/// a value sets. A two-state field would collapse the first two, so an operator
/// unpinning a platform would find the old branch still in force with no error
/// and no warning — which is the same silent divergence this whole field was
/// added to fix.
#[tokio::test]
async fn the_patch_path_distinguishes_absent_from_cleared_from_set() {
    let db = inmem_db().await;
    let services = build_services_tenant_scoped(db);
    let tenant = Uuid::new_v4();

    let created = services
        .platforms
        .create_platform(&ctx(tenant), new_platform("tri-state", Some("release-9.0")))
        .await
        .unwrap();

    // State 1: absent. Another field changes; the override is untouched.
    let after_absent = services
        .platforms
        .update_platform(
            &ctx(tenant),
            created.id,
            PlatformPatch {
                description: Some(Some("touched".to_owned())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        after_absent.default_branch.as_deref(),
        Some("release-9.0"),
        "an absent default_branch must leave the stored override alone"
    );

    // State 3 before state 2, so the clear below has something to clear that
    // was not merely the create value.
    let after_set = services
        .platforms
        .update_platform(
            &ctx(tenant),
            created.id,
            PlatformPatch {
                default_branch: Some(Some("  release-10.0  ".to_owned())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        after_set.default_branch.as_deref(),
        Some("release-10.0"),
        "Some(Some(v)) must pin to a trimmed v"
    );

    // State 2: cleared.
    let after_clear = services
        .platforms
        .update_platform(
            &ctx(tenant),
            created.id,
            PlatformPatch {
                default_branch: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        after_clear.default_branch, None,
        "Some(None) must clear the override so the repository default applies \
         again; the capability a plain Option<String> would have dropped"
    );

    // And it is really gone from the row, not just from the update's return.
    let fetched = services
        .platforms
        .get_platform(&ctx(tenant), created.id)
        .await
        .unwrap();
    assert_eq!(fetched.default_branch, None);
}

/// `Some(Some(""))` — the shape an empty-string REST body produces — must clear
/// rather than store `""`.
///
/// This is the half of the REST encoding the DTO test cannot check: the DTO's
/// job is to *preserve* the outer `Some`, and the service's job is to fold the
/// blank inner value to `None`. Neither test alone proves an operator sending
/// `{"default_branch": ""}` actually unpins the platform.
#[tokio::test]
async fn a_patch_with_a_blank_value_clears_the_override_rather_than_storing_it() {
    let db = inmem_db().await;
    let services = build_services_tenant_scoped(db);
    let tenant = Uuid::new_v4();

    let created = services
        .platforms
        .create_platform(
            &ctx(tenant),
            new_platform("blank-patch", Some("release-9.0")),
        )
        .await
        .unwrap();

    let updated = services
        .platforms
        .update_platform(
            &ctx(tenant),
            created.id,
            PlatformPatch {
                default_branch: Some(Some("   ".to_owned())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        updated.default_branch, None,
        "a blank value inside the outer Some means clear, not store \"   \"; \
         a whitespace override would reach qa-runs' resolve_branch as a real \
         branch name if that side did not also filter it"
    );
}

/// The three states the UI has to tell apart, and the ones a flat set of
/// nullable fields would blur together:
///   never looked        -> cluster is None
///   looked, unreachable -> cluster is Some, status "Unreachable"
///   looked, fine        -> cluster is Some, status "Healthy"
/// A `cluster: None` and a `status: "Unreachable"` are different facts and
/// this is the test that keeps them different.
#[tokio::test]
async fn never_checked_unreachable_and_healthy_are_three_distinct_states() {
    let tenant = Uuid::new_v4();
    let observer = Arc::new(ScriptedObserver::new([]));
    let services = build_services_tenant_scoped_with_observer(inmem_db().await, observer.clone());

    // A pasted kubeconfig, not a bare reference: `observe_platform` resolves
    // the reference back through credstore before it ever reaches the
    // observer, and only a paste (which this gear writes to credstore itself
    // via `RecordingCredStore`) is guaranteed to resolve in this harness.
    let created = services
        .platforms
        .create_platform(
            &ctx(tenant),
            NewPlatform {
                name: "p1".to_owned(),
                product_id: None,
                description: None,
                kubeconfig_credstore_ref: None,
                kubeconfig: Some(KubeconfigMaterial::new(
                    "apiVersion: v1\nkind: Config\n".to_owned(),
                )),
                default_branch: None,
                is_default: false,
            },
        )
        .await
        .unwrap();
    assert!(
        created.cluster.is_none(),
        "a platform no cycle has reached has never been looked at"
    );

    observer.script(PlatformObservation {
        platform: ObservationOutcome::Failed("namespaces not found".to_owned()),
        health: HealthOutcome::Failed("the API server could not be reached".to_owned()),
    });
    let unreachable = services
        .platforms
        .observe_platform(&ctx(tenant), created.id)
        .await
        .unwrap();
    let view = unreachable.cluster.expect("checked, so a view exists");
    assert_eq!(view.status, "Unreachable");
    assert_eq!(
        view.status_message.as_deref(),
        Some("the API server could not be reached")
    );
    assert!(
        view.nodes.is_empty(),
        "a failed read stores no nodes (D-CH-3)"
    );
    assert_eq!(view.counts.total, 0);
    assert_eq!(view.namespace_count, None);

    observer.script(PlatformObservation {
        platform: ObservationOutcome::Detected(DetectedPlatform {
            version: "26.5".to_owned(),
            build: Some("0".to_owned()),
            raw: "26.5.0".to_owned(),
            namespace: "virtuozzo".to_owned(),
            base_domain: None,
        }),
        health: HealthOutcome::Checked(ClusterHealth {
            nodes: vec![NodeSummary {
                name: "sv-vhp-jele-io".to_owned(),
                control_plane: true,
                ready: true,
                kubelet_version: Some("v1.33.4+k3s1".to_owned()),
                os_image: Some("Ubuntu 24.04.3 LTS".to_owned()),
            }],
            namespace_count: Some(14),
        }),
    });
    let healthy = services
        .platforms
        .observe_platform(&ctx(tenant), created.id)
        .await
        .unwrap();
    let view = healthy.cluster.expect("checked, so a view exists");
    assert_eq!(view.status, "Healthy");
    assert_eq!(
        view.status_message, None,
        "only Unreachable stores a message; the rest are composed in the UI"
    );
    assert_eq!(view.counts.total, 1);
    assert_eq!(view.counts.control_plane, 1);
    assert_eq!(view.counts.worker, 0);
    assert_eq!(view.namespace_count, Some(14));
    assert_eq!(
        view.nodes[0].kubelet_version.as_deref(),
        Some("v1.33.4+k3s1")
    );
}
