//! What the platform renders and what the gear stores both come off these two
//! lists, so the tests here pin the declarations themselves — keys, kinds,
//! roles and requiredness — not merely that the lists validate.

use credstore_sdk::SecretValue;

use super::*;

fn field<'a>(schema: &'a [FieldDesc], key: &str) -> &'a FieldDesc {
    schema
        .iter()
        .find(|f| f.key == key)
        .unwrap_or_else(|| panic!("the schema declares `{key}`"))
}

fn form(fields: &[(&str, &str)]) -> CredentialInput {
    CredentialInput {
        fields: fields
            .iter()
            .map(|(key, value)| ((*key).to_owned(), SecretValue::from((*value).to_owned())))
            .collect(),
    }
}

#[test]
fn the_credential_form_is_a_required_kubeconfig_and_an_optional_namespace() {
    let schema = credential_schema();

    assert_eq!(schema.len(), 2);

    let kubeconfig = field(&schema, KUBECONFIG_KEY);
    assert_eq!(kubeconfig.label, "Kubeconfig");
    assert_eq!(kubeconfig.kind, FieldKind::MultilineSecret);
    assert!(kubeconfig.required);
    assert_eq!(kubeconfig.help, None);

    let namespace = field(&schema, VPADM_NAMESPACE_KEY);
    assert_eq!(namespace.label, "vpadm namespace");
    assert_eq!(namespace.kind, FieldKind::Text);
    assert!(!namespace.required);
    assert!(
        namespace
            .help
            .as_deref()
            .is_some_and(|help| help.contains(DEFAULT_VPADM_NAMESPACE)),
        "the help text names the default, which is the whole reason the field is optional"
    );
}

/// `MultilineSecret` is what renders the textarea today's hand-written
/// kubeconfig field uses, which is why VHP's form is visually unchanged
/// despite now being generated. It is also what tells the gear to write the
/// value to credstore rather than to the environment's configuration.
#[test]
fn only_the_kubeconfig_is_credential_material() {
    let schema = credential_schema();
    let secrets: Vec<&str> = schema
        .iter()
        .filter(|f| f.kind.is_secret())
        .map(|f| f.key.as_str())
        .collect();

    assert_eq!(secrets, vec![KUBECONFIG_KEY]);
}

#[test]
fn the_observed_schema_declares_the_six_detected_fields() {
    let schema = observed_schema();

    let keys: Vec<&str> = schema.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            PLATFORM_VERSION_KEY,
            BUILD_KEY,
            BASE_DOMAIN_KEY,
            NAMESPACE_KEY,
            RAW_VERSION_KEY,
            EXTERNAL_HOSTS_KEY,
        ]
    );
    assert!(
        schema.iter().all(|f| f.in_detail),
        "every observed fact is read on the environment detail page"
    );
}

/// The claim that makes `observed_base_url` the successor to `vhp_base_url`
/// with no special case anywhere in the platform: the role, not the key, is
/// what the column and Task 10's `E2E_VHP_BASE_URL` are projected from.
#[test]
fn base_domain_claims_the_base_url_role_and_is_a_url() {
    let schema = observed_schema();
    let base = field(&schema, BASE_DOMAIN_KEY);

    assert_eq!(base.role, Some(FieldRole::BaseUrl));
    assert_eq!(base.kind, FieldKind::Url);
    assert_eq!(base.label, "Base URL");
    assert!(base.in_table);
}

#[test]
fn the_version_build_and_base_url_roles_are_the_table_columns() {
    let schema = observed_schema();

    assert_eq!(
        field(&schema, PLATFORM_VERSION_KEY).role,
        Some(FieldRole::Version)
    );
    assert_eq!(field(&schema, BUILD_KEY).role, Some(FieldRole::Build));

    let in_table: Vec<&str> = schema
        .iter()
        .filter(|f| f.in_table)
        .map(|f| f.key.as_str())
        .collect();
    assert_eq!(
        in_table,
        vec![PLATFORM_VERSION_KEY, BUILD_KEY, BASE_DOMAIN_KEY]
    );
}

/// Only Kubernetes products have a namespace at all, so the role has no column
/// of its own and a column on a list of mixed products would be blank for most
/// rows. It still claims the role, because the detail page and Task 10's
/// `E2E_K8S_NAMESPACE` both read it through the projection.
#[test]
fn namespace_claims_its_role_without_earning_a_column() {
    let schema = observed_schema();
    let namespace = field(&schema, NAMESPACE_KEY);

    assert_eq!(namespace.role, Some(FieldRole::Namespace));
    assert!(!namespace.in_table);
    assert!(namespace.in_detail);
}

/// A credential field on the environments table or detail page is the
/// 2026-08-28 leak with the plugin's own blessing. `validate_schemas` cannot
/// catch this one — it constrains kinds and roles, not placement — so it is
/// asserted here.
#[test]
fn no_credential_field_is_rendered_on_the_environment_page() {
    assert!(
        credential_schema()
            .iter()
            .all(|f| !f.in_table && !f.in_detail)
    );
}

#[test]
fn a_submitted_form_classifies_the_kubeconfig_as_secret_and_the_namespace_as_config() {
    let input = form(&[
        (KUBECONFIG_KEY, "apiVersion: v1"),
        (VPADM_NAMESPACE_KEY, "vhp"),
    ]);

    let classified = validate_credentials(&input).unwrap();

    assert_eq!(
        classified,
        vec![
            CredentialClassification::secret(KUBECONFIG_KEY),
            CredentialClassification::config(VPADM_NAMESPACE_KEY),
        ]
    );
}

/// The optional field really is optional: a form without it classifies only
/// what it carried, rather than inventing an empty namespace for the gear to
/// store.
#[test]
fn an_omitted_optional_field_is_not_classified() {
    let classified = validate_credentials(&form(&[(KUBECONFIG_KEY, "apiVersion: v1")])).unwrap();

    assert_eq!(
        classified,
        vec![CredentialClassification::secret(KUBECONFIG_KEY)]
    );
}

/// The gear writes only what a plugin classified, so an undeclared key reaches
/// nothing. Rejecting the whole form over one is a worse failure than dropping
/// it.
#[test]
fn an_undeclared_key_is_ignored_rather_than_rejected() {
    let input = form(&[(KUBECONFIG_KEY, "apiVersion: v1"), ("token", "hunter2")]);

    let classified = validate_credentials(&input).unwrap();

    assert_eq!(
        classified,
        vec![CredentialClassification::secret(KUBECONFIG_KEY)]
    );
}

#[test]
fn a_form_with_no_kubeconfig_is_rejected() {
    let failure = validate_credentials(&form(&[(VPADM_NAMESPACE_KEY, "vhp")])).unwrap_err();

    assert_eq!(failure.class, FailureClass::Malformed);
    assert_eq!(failure.remote_message, None);
}

/// Blankness is answered against the bytes, without decoding them: turning
/// credential material into a `&str` to call `.trim()` is one `?`-shaped
/// mistake away from a formatted error carrying the whole document.
#[test]
fn a_blank_kubeconfig_is_rejected_the_same_way_an_absent_one_is() {
    let absent = validate_credentials(&form(&[])).unwrap_err();
    let blank = validate_credentials(&form(&[(KUBECONFIG_KEY, " \n\t ")])).unwrap_err();

    assert_eq!(blank, absent);
}

/// The rejection is classified, never assembled from the submitted document.
/// This is the branch the 2026-08-28 key escaped through in the original.
#[test]
fn a_rejection_never_quotes_what_was_submitted() {
    let secret = "-----BEGIN PRIVATE KEY-----\nCANARY-schemas-rejection\n";
    // Blank the kubeconfig so the form is rejected, and put the marker in the
    // *other* field, where a careless "which field was wrong?" message would
    // pick it up.
    let input = form(&[(KUBECONFIG_KEY, "   "), (VPADM_NAMESPACE_KEY, secret)]);

    let failure = validate_credentials(&input).unwrap_err();

    let rendered = format!("{failure}");
    assert!(!rendered.contains("CANARY-schemas-rejection"), "{rendered}");
    assert!(!rendered.contains("BEGIN PRIVATE KEY"), "{rendered}");
}
