use super::*;

fn f(key: &str, kind: FieldKind, role: Option<FieldRole>) -> FieldDesc {
    FieldDesc {
        key: key.to_owned(),
        label: key.to_owned(),
        kind,
        required: false,
        role,
        in_table: false,
        in_detail: true,
        help: None,
    }
}

#[test]
fn a_schema_with_no_roles_and_no_secrets_is_valid() {
    let creds = vec![f("kubeconfig", FieldKind::MultilineSecret, None)];
    let observed = vec![f("nodes", FieldKind::Int, None)];
    assert!(validate_schemas(&creds, &observed).is_ok());
}

#[test]
fn credential_schema_may_declare_secrets() {
    let creds = vec![
        f("token", FieldKind::Secret, None),
        f("kubeconfig", FieldKind::MultilineSecret, None),
    ];
    assert!(validate_schemas(&creds, &[]).is_ok());
}

/// The rule that makes `observed_attrs` structurally safe to render. Without
/// it a plugin could put credential material on the environment page, which is
/// the 2026-08-28 leak reintroduced through a new door.
#[test]
fn observed_schema_may_not_declare_a_secret() {
    let observed = vec![f("kubeconfig_echo", FieldKind::Secret, None)];
    let err = validate_schemas(&[], &observed).unwrap_err();
    assert!(
        matches!(&err, SchemaError::SecretInObservedSchema { key } if key == "kubeconfig_echo"),
        "got {err:?}"
    );
}

#[test]
fn observed_schema_may_not_declare_a_multiline_secret() {
    let observed = vec![f("dump", FieldKind::MultilineSecret, None)];
    assert!(matches!(
        validate_schemas(&[], &observed).unwrap_err(),
        SchemaError::SecretInObservedSchema { .. }
    ));
}

/// Two fields claiming `Version` would make `APP_VERSION` depend on iteration
/// order, so it is rejected at registration rather than resolved arbitrarily.
#[test]
fn two_fields_may_not_claim_the_same_role() {
    let observed = vec![
        f("platformVersion", FieldKind::Text, Some(FieldRole::Version)),
        f("coreVersion", FieldKind::Text, Some(FieldRole::Version)),
    ];
    let err = validate_schemas(&[], &observed).unwrap_err();
    assert!(
        matches!(
            &err,
            SchemaError::DuplicateRole {
                role: FieldRole::Version,
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn distinct_roles_coexist() {
    let observed = vec![
        f("platformVersion", FieldKind::Text, Some(FieldRole::Version)),
        f("build", FieldKind::Text, Some(FieldRole::Build)),
        f("baseUrl", FieldKind::Url, Some(FieldRole::BaseUrl)),
        f("namespace", FieldKind::Text, Some(FieldRole::Namespace)),
    ];
    assert!(validate_schemas(&[], &observed).is_ok());
}

/// Roles are a property of what was *observed*, so a credential field claiming
/// one is a mistake the author should hear about at boot.
#[test]
fn credential_fields_may_not_claim_a_role() {
    let creds = vec![f("baseUrl", FieldKind::Url, Some(FieldRole::BaseUrl))];
    assert!(matches!(
        validate_schemas(&creds, &[]).unwrap_err(),
        SchemaError::RoleOnCredentialField { .. }
    ));
}

#[test]
fn duplicate_keys_within_one_schema_are_rejected() {
    let observed = vec![f("v", FieldKind::Text, None), f("v", FieldKind::Int, None)];
    assert!(matches!(
        validate_schemas(&[], &observed).unwrap_err(),
        SchemaError::DuplicateKey { .. }
    ));
}

#[test]
fn an_empty_key_is_rejected() {
    assert!(matches!(
        validate_schemas(&[], &[f("  ", FieldKind::Text, None)]).unwrap_err(),
        SchemaError::BlankKey
    ));
}

/// Tasks 21 and 22 render the credential form and the environment detail page
/// from these two descriptor lists side by side. One key declared in both,
/// differently, makes what gets rendered depend on which list a renderer
/// consulted first — and with a `MultilineSecret` credential and a `Text`
/// observation under one key, the wrong answer renders a secret.
#[test]
fn a_key_may_not_appear_in_both_schemas() {
    let creds = vec![f("namespace", FieldKind::MultilineSecret, None)];
    let observed = vec![f("namespace", FieldKind::Text, None)];

    let err = validate_schemas(&creds, &observed).unwrap_err();

    assert!(
        matches!(&err, SchemaError::KeyInBothSchemas { key } if key == "namespace"),
        "got {err:?}"
    );
}

/// Distinct keys across the two schemas are the normal case and stay valid.
#[test]
fn distinct_keys_across_the_two_schemas_are_valid() {
    let creds = vec![f("kubeconfig", FieldKind::MultilineSecret, None)];
    let observed = vec![f("namespace", FieldKind::Text, None)];
    assert!(validate_schemas(&creds, &observed).is_ok());
}

// ---------------------------------------------------------------------------
// sole_required_secret_key
// ---------------------------------------------------------------------------

/// A field that is required and secret, or neither, as each test needs.
fn field(key: &str, required: bool, kind: FieldKind) -> FieldDesc {
    FieldDesc {
        required,
        ..f(key, kind, None)
    }
}

/// The shape every plugin has today: one required secret, so the platform's
/// single-reference column has an unambiguous key.
#[test]
fn one_required_secret_is_the_sole_key() {
    let schema = vec![
        field("kubeconfig", true, FieldKind::MultilineSecret),
        field("vpadm_namespace", false, FieldKind::Text),
    ];
    assert_eq!(
        sole_required_secret_key(&schema).as_deref(),
        Some("kubeconfig")
    );
}

/// Two required secrets resolve to **nothing**, deliberately: one column holds
/// one reference and cannot say which of the two it is. A caller must refuse,
/// not pick — picking would hand a plugin the wrong material under the right
/// name, which is the one failure worse than not observing at all.
#[test]
fn two_required_secrets_resolve_to_nothing() {
    let schema = vec![
        field("kubeconfig", true, FieldKind::MultilineSecret),
        field("api_token", true, FieldKind::Secret),
    ];
    assert!(sole_required_secret_key(&schema).is_none());
}

/// An **optional** secret is not a candidate. An environment that must be
/// reachable has to have the credential it cannot work without, and that is
/// the one marked required.
#[test]
fn an_optional_secret_is_not_a_candidate() {
    let schema = vec![
        field("kubeconfig", false, FieldKind::MultilineSecret),
        field("region", true, FieldKind::Text),
    ];
    assert!(sole_required_secret_key(&schema).is_none());
}

/// A required **non-secret** is not a candidate either: the column it would
/// key holds credstore references.
#[test]
fn a_required_non_secret_is_not_a_candidate() {
    let schema = vec![field("region", true, FieldKind::Text)];
    assert!(sole_required_secret_key(&schema).is_none());
}
