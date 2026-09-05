//! What VHP environments *declare*: the credential form an operator fills in,
//! and the fields observing one can yield.
//!
//! The UI renders both from these descriptors, so VHP's create/edit form and
//! its row on the environments table exist without a line of VHP-specific UI
//! code. [`FieldKind::MultilineSecret`] renders the same textarea today's
//! hand-written kubeconfig field uses, which is why the form is visually
//! unchanged despite now being generated.
//!
//! # `in_table` / `in_detail`, which the plan's table left open
//!
//! The plan fixes each field's key, kind, requiredness, role, label and
//! `in_table`. It says nothing about `in_detail`, so this module decides it,
//! once, by the rule the flag is named for:
//!
//! * **Every observed field is `in_detail`.** All six are machine-detected
//!   facts about the environment, and the detail page is where they are read.
//!   The three that are also `in_table` are the ones worth a column in a list
//!   of many environments.
//! * **Neither credential field is `in_detail` or `in_table`.** Those two
//!   flags place a field on the environments *table* and *detail page*; the
//!   credential form is rendered from this schema regardless. `kubeconfig` on
//!   a rendered page is the 2026-08-28 leak with the plugin's own blessing,
//!   and `vpadm_namespace` follows its sibling rather than splitting the rule
//!   for one field — the detected `namespace` observed field already puts the
//!   namespace actually in use on the detail page, which is the more useful
//!   of the two values anyway.

use qa_product_sdk::descriptor::{FieldDesc, FieldKind, FieldRole};
use qa_product_sdk::observation::{FailureClass, PluginFailure};
use qa_product_sdk::plugin::{CredentialClassification, CredentialInput};

/// The credential key holding the environment's kubeconfig.
pub const KUBECONFIG_KEY: &str = "kubeconfig";

/// The credential key holding the namespace vpadm installed into, when an
/// operator overrides it.
pub const VPADM_NAMESPACE_KEY: &str = "vpadm_namespace";

/// Where vpadm installs unless told otherwise.
///
/// The same default as `qa-environments`' `DEFAULT_VPADM_NAMESPACE`, which
/// mirrors legacy's `pick_vpadm_namespace`
/// (`manager/src/services/platforms.rs:1298-1313`) — including that a present
/// but *blank* override does not count as set any more than an absent one
/// does. That rule is applied in [`crate::observe`], where the configuration
/// is read.
pub const DEFAULT_VPADM_NAMESPACE: &str = "virtuozzo";

/// Observed key: the version prefix, `26.5` out of a `26.5.0`.
pub const PLATFORM_VERSION_KEY: &str = "platformVersion";
/// Observed key: the numeric build suffix, `0` out of a `26.5.0`.
pub const BUILD_KEY: &str = "build";
/// Observed key: the environment's base URL, `https://` + the domain suffix
/// shared by its externally-visible gateway hostnames.
pub const BASE_DOMAIN_KEY: &str = "baseDomain";
/// Observed key: the namespace the install metadata was actually found in.
pub const NAMESPACE_KEY: &str = "namespace";
/// Observed key: the unsplit `platformVersion` the cluster reported.
pub const RAW_VERSION_KEY: &str = "rawVersion";
/// Observed key: every externally-visible gateway hostname, comma-separated.
pub const EXTERNAL_HOSTS_KEY: &str = "externalHosts";

/// One credential field. `role` is absent by construction: roles describe
/// observations, and `validate_schemas` rejects a credential field claiming
/// one.
fn credential(
    key: &str,
    label: &str,
    kind: FieldKind,
    required: bool,
    help: Option<&str>,
) -> FieldDesc {
    FieldDesc {
        key: key.to_owned(),
        label: label.to_owned(),
        kind,
        required,
        role: None,
        in_table: false,
        in_detail: false,
        help: help.map(ToOwned::to_owned),
    }
}

/// One observed field. `role` is what the platform projects into a column;
/// `in_table` is whether it also earns one on the environments list.
fn observed(
    key: &str,
    label: &str,
    kind: FieldKind,
    role: Option<FieldRole>,
    in_table: bool,
) -> FieldDesc {
    FieldDesc {
        key: key.to_owned(),
        label: label.to_owned(),
        kind,
        required: false,
        role,
        in_table,
        in_detail: true,
        help: None,
    }
}

/// The credential fields a VHP environment needs.
#[must_use]
pub fn credential_schema() -> Vec<FieldDesc> {
    vec![
        credential(
            KUBECONFIG_KEY,
            "Kubeconfig",
            FieldKind::MultilineSecret,
            true,
            None,
        ),
        credential(
            VPADM_NAMESPACE_KEY,
            "vpadm namespace",
            FieldKind::Text,
            false,
            Some("The namespace vpadm installed into. Defaults to `virtuozzo`."),
        ),
    ]
}

/// The fields [`crate::observe`] can yield.
///
/// `baseDomain` claiming [`FieldRole::BaseUrl`] is what makes
/// `qa_environments.observed_base_url` the successor to `vhp_base_url` with no
/// special case anywhere in the platform: the same projection feeds the
/// column, the environment page and Task 10's `E2E_VHP_BASE_URL`.
///
/// `namespace` claims [`FieldRole::Namespace`] and is deliberately *not*
/// `in_table`: only Kubernetes products have a namespace at all, so the role
/// has no column of its own and a column on a list of mixed products would be
/// empty for most rows.
#[must_use]
pub fn observed_schema() -> Vec<FieldDesc> {
    vec![
        observed(
            PLATFORM_VERSION_KEY,
            "Platform version",
            FieldKind::Text,
            Some(FieldRole::Version),
            true,
        ),
        observed(
            BUILD_KEY,
            "Build",
            FieldKind::Text,
            Some(FieldRole::Build),
            true,
        ),
        observed(
            BASE_DOMAIN_KEY,
            "Base URL",
            FieldKind::Url,
            Some(FieldRole::BaseUrl),
            true,
        ),
        observed(
            NAMESPACE_KEY,
            "Namespace",
            FieldKind::Text,
            Some(FieldRole::Namespace),
            false,
        ),
        observed(
            RAW_VERSION_KEY,
            "Raw platformVersion",
            FieldKind::Text,
            None,
            false,
        ),
        observed(
            EXTERNAL_HOSTS_KEY,
            "External gateway hostnames",
            FieldKind::Text,
            None,
            false,
        ),
    ]
}

/// Fixed rejection for a form with no kubeconfig in it.
const KUBECONFIG_REQUIRED: &str =
    "a kubeconfig is required: paste the kubeconfig for the cluster this environment runs on";

/// Classify a submitted credential form.
///
/// # What this checks, and what it deliberately does not
///
/// Presence and non-blankness of the required fields, and nothing else. It
/// does **not** parse the kubeconfig, even though doing so would reject a
/// pasted PEM private key at the form rather than at the first observation.
/// That is a behaviour change, and Phase C is a move: today's create dialog
/// accepts any non-empty text and reports a malformed document through
/// `observe`'s classified failure, which is where
/// [`qa_plugin_k8s::classify_kubeconfig`] already contains it. Adding the
/// check here would start refusing environments the platform accepts today,
/// which is a decision for a task that says so.
///
/// Keys the schema does not declare are ignored rather than rejected: the
/// gear writes only what a plugin classified, so an undeclared key reaches
/// nothing, and rejecting the whole form over one is a worse failure than
/// dropping it.
///
/// # Errors
///
/// [`FailureClass::Malformed`] when a required field is absent or blank. The
/// text is fixed and names no submitted value — the enforcement point for the
/// rule in `PRODUCT-PLUGINS-DESIGN.md` §9.
pub fn validate_credentials(
    input: &CredentialInput,
) -> Result<Vec<CredentialClassification>, PluginFailure> {
    let kubeconfig_present = input
        .get(KUBECONFIG_KEY)
        .is_some_and(|value| !is_blank(value.as_bytes()));
    if !kubeconfig_present {
        return Err(PluginFailure::classified(
            FailureClass::Malformed,
            KUBECONFIG_REQUIRED,
        ));
    }

    Ok(credential_schema()
        .iter()
        .filter(|field| input.contains(&field.key))
        .map(|field| CredentialClassification {
            key: field.key.clone(),
            is_secret: field.kind.is_secret(),
        })
        .collect())
}

/// Whether submitted bytes are empty or nothing but ASCII whitespace.
///
/// Asked of the bytes, not of a `String`: turning credential material into a
/// `&str` to call `.trim()` on it is one `?`-shaped mistake away from a
/// formatted error carrying the whole document, and emptiness is answerable
/// without decoding anything.
fn is_blank(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_whitespace)
}

#[cfg(test)]
#[path = "schemas_tests.rs"]
mod schemas_tests;
