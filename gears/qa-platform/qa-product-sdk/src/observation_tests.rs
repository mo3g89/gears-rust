use super::*;
use crate::descriptor::{FieldDesc, FieldKind, FieldRole};

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

#[test]
fn role_projection_lifts_the_claimed_attributes_into_columns() {
    let schema = vec![
        observed("platformVersion", Some(FieldRole::Version)),
        observed("build", Some(FieldRole::Build)),
        observed("baseDomain", Some(FieldRole::BaseUrl)),
        observed("namespace", Some(FieldRole::Namespace)),
        observed("nodeCount", None),
    ];
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");
    attrs.set("build", "1471");
    attrs.set("baseDomain", "https://sv.jele.io");
    attrs.set("namespace", "virtuozzo");
    attrs.set("nodeCount", "3");

    let p = project_roles(&schema, &attrs);
    assert_eq!(p.version.as_deref(), Some("9.2"));
    assert_eq!(p.build.as_deref(), Some("1471"));
    assert_eq!(p.base_url.as_deref(), Some("https://sv.jele.io"));
    assert_eq!(p.namespace.as_deref(), Some("virtuozzo"));
}

#[test]
fn an_unclaimed_role_projects_to_none() {
    let schema = vec![observed("platformVersion", Some(FieldRole::Version))];
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");

    let p = project_roles(&schema, &attrs);
    assert_eq!(p.version.as_deref(), Some("9.2"));
    assert_eq!(
        p.build, None,
        "a plugin that declares no Build role yields no APP_BUILD"
    );
}

/// A declared role whose attribute the plugin did not return this cycle must
/// not project a stale or empty value.
#[test]
fn a_declared_role_with_no_value_projects_to_none() {
    let schema = vec![observed("build", Some(FieldRole::Build))];
    let p = project_roles(&schema, &ObservedAttrs::default());
    assert_eq!(p.build, None);
}

#[test]
fn a_blank_attribute_projects_to_none_rather_than_an_empty_string() {
    let schema = vec![observed("build", Some(FieldRole::Build))];
    let mut attrs = ObservedAttrs::default();
    attrs.set("build", "   ");
    assert_eq!(project_roles(&schema, &attrs).build, None);
}

/// `detail` is `&'static str`, so this is the compile-time guarantee that a
/// plugin cannot format credential material into a failure. The test documents
/// the intent; the type is what enforces it.
#[test]
fn a_failure_carries_a_class_and_fixed_text() {
    let f = PluginFailure::classified(FailureClass::AuthRejected, "the credential was rejected");
    assert_eq!(f.class, FailureClass::AuthRejected);
    assert_eq!(f.detail, Some("the credential was rejected"));
    assert_eq!(f.remote_message, None);
}

/// The one sanctioned exception: text the *remote* sent back. It is what keeps
/// "namespaces virtuozzo not found" visible to an operator who can act on it.
#[test]
fn a_failure_may_carry_a_remote_message() {
    let f = PluginFailure::classified(FailureClass::NotFound, "the object was not found")
        .with_remote_message("namespaces \"virtuozzo\" not found");
    assert_eq!(
        f.remote_message.as_deref(),
        Some("namespaces \"virtuozzo\" not found")
    );
}

#[test]
fn health_states_round_trip_through_their_wire_names() {
    for (state, wire) in [
        (HealthState::Ok, "ok"),
        (HealthState::Degraded, "degraded"),
        (HealthState::Down, "down"),
        (HealthState::Unknown, "unknown"),
    ] {
        assert_eq!(state.as_str(), wire);
        assert_eq!(HealthState::from_str_or_unknown(wire), state);
    }
}

/// An unrecognised persisted value must read as `Unknown`, not panic: a column
/// written by a newer build must not take down an older one.
#[test]
fn an_unrecognised_health_value_reads_as_unknown() {
    assert_eq!(
        HealthState::from_str_or_unknown("wobbly"),
        HealthState::Unknown
    );
}

/// The serialised form is a flat JSON object whose keys are in sorted order.
///
/// Asserted against the serialised **string**, not a `serde_json::Value`: a
/// `Value`'s map erases ordering, so the earlier version of this test could
/// not have demonstrated ordering with any number of keys. Ordering is the
/// property that matters here — `ObservedAttrs` is a `BTreeMap` specifically
/// so the persisted JSONB is stable and an unchanged observation is not a
/// spurious row update every cycle.
#[test]
fn observed_attrs_serialise_to_a_sorted_json_object() {
    let mut attrs = ObservedAttrs::default();
    attrs.set("z", "3");
    attrs.set("a", "1");
    attrs.set("m", "2");

    let json = serde_json::to_string(&attrs).unwrap();

    assert_eq!(json, r#"{"a":"1","m":"2","z":"3"}"#);
}

/// **Important-4.** `validate_schemas` constrains what a plugin *declares*;
/// nothing constrained what it *returns*. A plugin with a spotless schema
/// could still set `kubeconfig_echo` and have Task 15 persist it to JSONB and
/// publish it on `EnvironmentDto`.
#[test]
fn retain_declared_drops_an_undeclared_attribute() {
    let schema = vec![observed("platformVersion", Some(FieldRole::Version))];
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");
    attrs.set("kubeconfig_echo", "-----BEGIN PRIVATE KEY-----");

    let kept = retain_declared(&schema, attrs);

    assert_eq!(kept.get("platformVersion"), Some("9.2"));
    assert_eq!(kept.get("kubeconfig_echo"), None);
}

#[test]
fn retain_declared_keeps_every_declared_attribute() {
    let schema = vec![
        observed("platformVersion", Some(FieldRole::Version)),
        observed("nodeCount", None),
    ];
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");
    attrs.set("nodeCount", "3");

    let kept = retain_declared(&schema, attrs.clone());

    assert_eq!(kept, attrs);
}

/// A plugin declaring no observed schema can persist nothing — which is the
/// point: what is not declared cannot be rendered, because it is not there.
#[test]
fn retain_declared_against_an_empty_schema_keeps_nothing() {
    let mut attrs = ObservedAttrs::default();
    attrs.set("anything", "at all");

    assert!(retain_declared(&[], attrs).is_empty());
}
