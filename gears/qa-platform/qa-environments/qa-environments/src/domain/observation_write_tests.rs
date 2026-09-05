//! [`ObservationWrite`]'s two transforms, and the legacy status mapping.
//!
//! The `retain_declared` half is the one that matters: see
//! `an_undeclared_attribute_is_dropped_before_anything_can_persist_it` and
//! the module's own header for why a constructor rather than a call the
//! writer must remember.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use qa_product_sdk::descriptor::{FieldDesc, FieldKind, FieldRole};
use qa_product_sdk::observation::{
    FailureClass, HealthOutcome, ObservationOutcome, ObservedAttrs, PluginFailure,
    PluginObservation,
};
use qa_product_sdk::testing::Canary;

use super::ObservationWrite;

/// A declared observed field. Mirrors `qa-vhp-product-plugin`'s own
/// `observed()` helper so the shapes under test are the shapes a real plugin
/// declares.
fn observed(key: &str, role: Option<FieldRole>) -> FieldDesc {
    FieldDesc {
        key: key.to_owned(),
        label: key.to_owned(),
        kind: FieldKind::Text,
        required: false,
        role,
        in_table: false,
        in_detail: true,
        help: None,
    }
}

fn schema() -> Vec<FieldDesc> {
    vec![
        observed("platformVersion", Some(FieldRole::Version)),
        observed("build", Some(FieldRole::Build)),
        observed("baseDomain", Some(FieldRole::BaseUrl)),
        observed("namespace", Some(FieldRole::Namespace)),
        observed("rawVersion", None),
    ]
}

fn detected(attrs: ObservedAttrs) -> PluginObservation {
    PluginObservation {
        environment: ObservationOutcome::Detected(attrs),
        health: HealthOutcome::NotAttempted,
    }
}

#[test]
fn the_four_role_claimed_attributes_are_projected() {
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");
    attrs.set("build", "1471");
    attrs.set("baseDomain", "https://sv.jele.io");
    attrs.set("namespace", "virtuozzo");
    attrs.set("rawVersion", "9.2.1471");

    let write = ObservationWrite::new(&schema(), detected(attrs));

    let roles = write.roles();
    assert_eq!(roles.version.as_deref(), Some("9.2"));
    assert_eq!(roles.build.as_deref(), Some("1471"));
    assert_eq!(roles.base_url.as_deref(), Some("https://sv.jele.io"));
    assert_eq!(roles.namespace.as_deref(), Some("virtuozzo"));
    assert_eq!(
        write.attrs().get("rawVersion"),
        Some("9.2.1471"),
        "a declared field claiming no role still reaches the attribute map"
    );
}

/// **The requirement this type exists for.** A plugin whose `observed_schema`
/// is spotless can still put an undeclared key in the map it returns, and
/// that map is persisted to a JSONB column and published on
/// `EnvironmentDto`. The canary here is the plugin SDK's own — the same
/// marker its leak harness plants — so both suites are testing the same
/// thing.
#[test]
fn an_undeclared_attribute_is_dropped_before_anything_can_persist_it() {
    let canary = Canary::vhp_shaped();
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");
    attrs.set("kubeconfig_echo", canary.pem.clone());

    let write = ObservationWrite::new(&schema(), detected(attrs));

    assert_eq!(
        write.attrs().get("kubeconfig_echo"),
        None,
        "an undeclared key must not survive into anything persistable"
    );
    let serialised = serde_json::to_string(write.attrs()).unwrap();
    assert!(
        !serialised.contains(&canary.pem),
        "and the canary must not be anywhere in the blob that reaches the column: {serialised}"
    );
    assert_eq!(
        write.attrs().get("platformVersion"),
        Some("9.2"),
        "while the DECLARED attributes survive alongside it -- dropping everything \
         would also satisfy a one-sided assertion"
    );
}

#[test]
fn a_failed_environment_half_carries_no_attributes_and_no_projections() {
    let write = ObservationWrite::new(
        &schema(),
        PluginObservation {
            environment: ObservationOutcome::Failed(PluginFailure::classified(
                FailureClass::Unreachable,
                "the target could not be reached",
            )),
            health: HealthOutcome::NotAttempted,
        },
    );

    assert!(write.attrs().is_empty());
    assert_eq!(write.roles().version, None);
    assert_eq!(write.roles().base_url, None);
}

/// Blank projects to `None`, never to `Some("")` — an empty `APP_VERSION`
/// reaching every test is the failure `project_roles` guards, and this pins
/// that the guard survives the trip through this type.
#[test]
fn a_blank_role_claimed_attribute_projects_to_none() {
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "   ");

    let write = ObservationWrite::new(&schema(), detected(attrs));

    assert_eq!(write.roles().version, None);
}

// Two tests were deleted here with `legacy_cluster_status`, which computed a
// value for the `cluster_status` column `m20260903_000012` dropped. They pinned
// that a plugin naming one of legacy's four statuses in its `detail` had that
// word used verbatim, and that anything else fell back to the coarse
// `HealthState` projection. There is no legacy spelling to recover any more --
// `health_state` and `health_detail` are the only health columns, and
// `the_four_role_claimed_attributes_are_projected` above covers what remains of
// the projection (whole-branch review, I-2).
