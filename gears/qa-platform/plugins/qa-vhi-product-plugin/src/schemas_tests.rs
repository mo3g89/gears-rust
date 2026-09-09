use std::collections::BTreeMap;

use credstore_sdk::SecretValue;
use qa_product_sdk::descriptor::{FieldKind, FieldRole};
use qa_product_sdk::observation::FailureClass;
use qa_product_sdk::plugin::CredentialInput;

use super::*;

fn submitted(pairs: &[(&str, &str)]) -> CredentialInput {
    CredentialInput {
        fields: pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), SecretValue::from((*v).to_owned())))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn complete() -> CredentialInput {
    submitted(&[
        (NODE_HOST_KEY, "10.136.31.154"),
        (
            SSH_PRIVATE_KEY_KEY,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nx\n",
        ),
        (VINFRA_PASSWORD_KEY, "hunter2"),
    ])
}

#[test]
fn exactly_two_credential_fields_are_secret() {
    let secret: Vec<_> = credential_schema()
        .into_iter()
        .filter(|f| f.kind.is_secret())
        .map(|f| f.key)
        .collect();
    assert_eq!(secret, vec![SSH_PRIVATE_KEY_KEY, VINFRA_PASSWORD_KEY]);
}

#[test]
fn the_required_fields_are_the_host_and_the_two_secrets() {
    let required: Vec<_> = credential_schema()
        .into_iter()
        .filter(|f| f.required)
        .map(|f| f.key)
        .collect();
    assert_eq!(
        required,
        vec![NODE_HOST_KEY, SSH_PRIVATE_KEY_KEY, VINFRA_PASSWORD_KEY],
        "node_host is required too, but it is configuration rather than a secret"
    );
}

#[test]
fn no_credential_field_claims_a_role() {
    assert!(
        credential_schema().iter().all(|f| f.role.is_none()),
        "roles describe observations; the SDK's validate_schemas rejects a credential field with one"
    );
}

#[test]
fn the_three_role_bearing_observed_fields_are_the_table_columns() {
    let in_table: Vec<_> = observed_schema()
        .into_iter()
        .filter(|f| f.in_table)
        .map(|f| (f.key, f.role))
        .collect();
    assert_eq!(
        in_table,
        vec![
            (PRODUCT_VERSION_KEY.to_owned(), Some(FieldRole::Version)),
            (BUILD_KEY.to_owned(), Some(FieldRole::Build)),
            (BASE_URL_KEY.to_owned(), Some(FieldRole::BaseUrl)),
        ]
    );
}

#[test]
fn every_observed_field_is_on_the_detail_page() {
    assert!(observed_schema().iter().all(|f| f.in_detail));
}

#[test]
fn the_base_url_field_is_a_url() {
    let base = observed_schema()
        .into_iter()
        .find(|f| f.key == BASE_URL_KEY)
        .expect("declared");
    assert_eq!(base.kind, FieldKind::Url);
}

#[test]
fn a_complete_form_classifies_its_two_secrets_and_nothing_else() {
    let classified = validate_credentials(&complete()).expect("accepted");
    let secrets: Vec<_> = classified
        .iter()
        .filter(|c| c.is_secret)
        .map(|c| c.key.as_str())
        .collect();
    assert_eq!(secrets, vec![SSH_PRIVATE_KEY_KEY, VINFRA_PASSWORD_KEY]);
}

#[test]
fn the_optional_fields_classify_as_configuration() {
    let mut form = complete();
    form.fields.insert(
        SSH_USER_KEY.to_owned(),
        SecretValue::from("root".to_owned()),
    );
    let classified = validate_credentials(&form).expect("accepted");
    let user = classified
        .iter()
        .find(|c| c.key == SSH_USER_KEY)
        .expect("classified");
    assert!(
        !user.is_secret,
        "the login name is not a secret and belongs in config"
    );
}

#[test]
fn a_missing_private_key_is_rejected_as_malformed() {
    let form = submitted(&[
        (NODE_HOST_KEY, "10.136.31.154"),
        (VINFRA_PASSWORD_KEY, "hunter2"),
    ]);
    let failure = validate_credentials(&form).expect_err("rejected");
    assert_eq!(failure.class, FailureClass::Malformed);
}

#[test]
fn a_blank_password_is_rejected_the_same_way_an_absent_one_is() {
    let form = submitted(&[
        (NODE_HOST_KEY, "10.136.31.154"),
        (
            SSH_PRIVATE_KEY_KEY,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nx\n",
        ),
        (VINFRA_PASSWORD_KEY, "   \n"),
    ]);
    let failure = validate_credentials(&form).expect_err("rejected");
    assert_eq!(failure.class, FailureClass::Malformed);
}

#[test]
fn no_rejection_message_repeats_a_submitted_value() {
    let form = submitted(&[(NODE_HOST_KEY, "sekrit-host.example")]);
    let failure = validate_credentials(&form).expect_err("rejected");
    let rendered = format!("{failure:?}");
    assert!(
        !rendered.contains("sekrit-host"),
        "a rejection must name the field, never the value"
    );
}

#[test]
fn an_undeclared_key_is_dropped_rather_than_rejecting_the_form() {
    let mut form = complete();
    form.fields
        .insert("stray".to_owned(), SecretValue::from("x".to_owned()));
    let classified = validate_credentials(&form).expect("accepted");
    assert!(classified.iter().all(|c| c.key != "stray"));
}

// The tests above assert through `.is_secret()`, which is `true` for *both*
// `FieldKind::Secret` and `FieldKind::MultilineSecret` — so none of them
// would notice `ssh_private_key` quietly changing from `MultilineSecret` to
// `Secret`. That distinction is not cosmetic: it is which widget the
// operator gets on the credential form, a single-line input versus the
// textarea a multi-line PEM private key needs to be pasted into. This test
// pins the full descriptor shape — kind included — so that regression fails
// here instead of shipping silently.
#[test]
fn credential_schema_matches_the_spec_field_by_field() {
    let shape: Vec<_> = credential_schema()
        .into_iter()
        .map(|f| (f.key, f.kind, f.required))
        .collect();
    assert_eq!(
        shape,
        vec![
            (NODE_HOST_KEY.to_owned(), FieldKind::Text, true),
            (SSH_USER_KEY.to_owned(), FieldKind::Text, false),
            (SSH_PORT_KEY.to_owned(), FieldKind::Int, false),
            (
                SSH_PRIVATE_KEY_KEY.to_owned(),
                FieldKind::MultilineSecret,
                true
            ),
            (VINFRA_USERNAME_KEY.to_owned(), FieldKind::Text, false),
            (VINFRA_PASSWORD_KEY.to_owned(), FieldKind::Secret, true),
        ]
    );
}

#[test]
fn observed_schema_matches_the_spec_field_by_field() {
    let shape: Vec<_> = observed_schema()
        .into_iter()
        .map(|f| (f.key, f.kind, f.role, f.in_table))
        .collect();
    assert_eq!(
        shape,
        vec![
            (
                PRODUCT_VERSION_KEY.to_owned(),
                FieldKind::Text,
                Some(FieldRole::Version),
                true
            ),
            (
                BUILD_KEY.to_owned(),
                FieldKind::Text,
                Some(FieldRole::Build),
                true
            ),
            (
                BASE_URL_KEY.to_owned(),
                FieldKind::Url,
                Some(FieldRole::BaseUrl),
                true
            ),
            (NODE_COUNT_KEY.to_owned(), FieldKind::Int, None, false),
            (RAW_RELEASE_KEY.to_owned(), FieldKind::Text, None, false),
            (STORAGE_NAME_KEY.to_owned(), FieldKind::Text, None, false),
        ]
    );
}

// ── `ssh_port`, the first `Int` field any plugin declares ─────────────────
//
// Nothing else in the stack enforces `FieldKind::Int`: the UI renders it as a
// plain text input, `qa-environments` writes every submitted field as a JSON
// string, and `config::config_port` falls back to 22 for anything that will
// not parse. So until 2026-09-09 "twenty-two" was accepted end to end and the
// environment silently talked to the wrong port -- a wrong answer with no
// diagnostic anywhere. `validate_credentials` is the only place that can say
// so, and these are the tests that hold it there.

/// A complete form plus an `ssh_port` value.
fn complete_with_port(port: &str) -> CredentialInput {
    submitted(&[
        (NODE_HOST_KEY, "10.136.31.154"),
        (
            SSH_PRIVATE_KEY_KEY,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nx\n",
        ),
        (VINFRA_PASSWORD_KEY, "hunter2"),
        (SSH_PORT_KEY, port),
    ])
}

#[test]
fn a_non_numeric_ssh_port_is_rejected_rather_than_silently_defaulted() {
    for port in ["twenty-two", "22a", "2 2", "-1", "1.5", "0x16"] {
        let failure = validate_credentials(&complete_with_port(port))
            .expect_err("a non-numeric port must be refused");
        assert_eq!(failure.class, FailureClass::Malformed);
        assert_eq!(
            failure.detail,
            Some(SSH_PORT_MALFORMED),
            "`{port}` must be refused with the fixed port text"
        );
    }
}

#[test]
fn an_out_of_range_ssh_port_is_rejected() {
    // 65536 does not fit a `u16`, and 0 fits but is not connectable.
    for port in ["0", "65536", "99999999999999999999"] {
        let failure =
            validate_credentials(&complete_with_port(port)).expect_err("out of range is refused");
        assert_eq!(failure.detail, Some(SSH_PORT_MALFORMED), "port {port}");
    }
}

#[test]
fn a_valid_ssh_port_is_accepted_and_classified_as_configuration() {
    for port in ["22", "2222", "1", "65535", "  2222  "] {
        let classified =
            validate_credentials(&complete_with_port(port)).expect("a real port is accepted");
        let entry = classified
            .iter()
            .find(|c| c.key == SSH_PORT_KEY)
            .expect("ssh_port must be classified, or it never reaches `config`");
        assert!(
            !entry.is_secret,
            "a port is configuration, not credential material"
        );
    }
}

/// Blank still means "unset". The rule every optional field here follows is
/// that a present-but-blank value does not count as set, and the default is
/// applied where the configuration is *read*; rejecting a blank port would
/// make "leave it alone" impossible to express on the form.
#[test]
fn a_blank_ssh_port_is_unset_not_malformed() {
    for port in ["", "   ", "\n"] {
        validate_credentials(&complete_with_port(port))
            .expect("a blank optional field is unset, not an error");
    }
}
