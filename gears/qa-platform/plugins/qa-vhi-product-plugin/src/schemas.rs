//! What VHI environments *declare*: the credential form an operator fills in,
//! and the fields observing one can yield.
//!
//! The UI renders both from these descriptors, so VHI's create/edit form and
//! its row on the environments table exist without a line of VHI-specific UI
//! code.
//!
//! # `in_table` / `in_detail`, which the plan's table left open
//!
//! The plan fixes each field's key, kind, requiredness, role, label and
//! `in_table`. It says nothing about `in_detail`, so this module decides it,
//! once, by the rule VHP's own schema module decided it by:
//!
//! * **Every observed field is `in_detail`.** All six are machine-detected
//!   facts about the environment, and the detail page is where they are read.
//!   The three that are also `in_table` are the ones worth a column in a list
//!   of many environments.
//! * **Neither credential field is `in_detail` or `in_table`.** Those two
//!   flags place a field on the environments *table* and *detail page*; the
//!   credential form is rendered from this schema regardless.

use qa_product_sdk::descriptor::{FieldDesc, FieldKind, FieldRole};
use qa_product_sdk::observation::{FailureClass, PluginFailure};
use qa_product_sdk::plugin::{CredentialClassification, CredentialInput};

/// The credential key holding the management node's address.
///
/// **One field, used twice.** It is the SSH host *and* `VINFRA_PORTAL`: in
/// `hcilib3`, the portal is literally the hostname of the backend URL
/// (`hcilib3-py3/lib/hcilib/vinfra_cli/__init__.py:71`,
/// `urlparse(self.backend_url).hostname`) and the backend URL is
/// `https://{host}:8888` (`hcilib/subsystems/subsystems.py:1044`). Asking an
/// operator for it twice would invite them to disagree with themselves.
pub const NODE_HOST_KEY: &str = "node_host";
pub const SSH_USER_KEY: &str = "ssh_user";
pub const SSH_PORT_KEY: &str = "ssh_port";
pub const SSH_PRIVATE_KEY_KEY: &str = "ssh_private_key";
pub const VINFRA_USERNAME_KEY: &str = "vinfra_username";
pub const VINFRA_PASSWORD_KEY: &str = "vinfra_password";

/// Where `ssh_user` defaults to when an operator leaves it blank: `hcilib3`'s
/// management node is administered as `root` over SSH. Applied in
/// `observe`/`run`, where the configuration is read — not here, and a present
/// but blank override does not count as set any more than an absent one does,
/// the same rule VHP's `DEFAULT_VPADM_NAMESPACE` states about its own default.
pub const DEFAULT_SSH_USER: &str = "root";
/// Where `ssh_port` defaults to when an operator leaves it blank: the
/// standard SSH port. Applied in `observe`/`run`, where the configuration is
/// read — not here.
pub const DEFAULT_SSH_PORT: u16 = 22;
/// Where `vinfra_username` defaults to when an operator leaves it blank: the
/// `vinfra` CLI's built-in administrator account. Applied in `observe`/`run`,
/// where the configuration is read — not here.
pub const DEFAULT_VINFRA_USERNAME: &str = "admin";

/// Observed key: the product version, `7.4.0` out of `/etc/hci-release`.
pub const PRODUCT_VERSION_KEY: &str = "productVersion";
/// Observed key: the product build, `82` out of the same line.
pub const BUILD_KEY: &str = "build";
/// Observed key: the admin panel's base URL, confirmed reachable.
pub const BASE_URL_KEY: &str = "baseUrl";
/// Observed key: how many nodes `vinfra node list` reported.
pub const NODE_COUNT_KEY: &str = "nodeCount";
/// Observed key: the raw `/etc/hci-release` line, on a *successful* parse.
/// When the line cannot be decomposed at all, this attribute is never
/// written -- a `Failed` observation carries no attributes -- and the raw
/// text instead survives on that failure's `remote_message`, so a human can
/// still see what the node actually said.
pub const RAW_RELEASE_KEY: &str = "rawRelease";
/// Observed key: `storage-name` from the backend's about document.
pub const STORAGE_NAME_KEY: &str = "storageName";

const NODE_HOST_REQUIRED: &str =
    "the management node's address is required: it is both the SSH host and the vinfra portal";
const SSH_KEY_REQUIRED: &str =
    "an SSH private key is required: paste a passphrase-less key authorised for this node";
const VINFRA_PASSWORD_REQUIRED: &str =
    "the vinfra admin password is required: the CLI cannot authenticate to the cluster without it";
/// `ssh_port` is `FieldKind::Int`, but nothing between the operator's browser
/// and `config_port` enforces that: the UI renders `Int` as a plain text
/// input, and `config_port` falls back to [`DEFAULT_SSH_PORT`] for anything it
/// cannot parse. Without this check, "twenty-two" is accepted end to end and
/// the environment silently talks to port 22 -- a wrong answer with no
/// diagnostic anywhere, which is worse than a refused form.
const SSH_PORT_MALFORMED: &str = "the SSH port must be a whole number between 1 and 65535: leave \
                                  it blank to use the default of 22";

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

/// The credential fields a VHI environment needs.
#[must_use]
pub fn credential_schema() -> Vec<FieldDesc> {
    vec![
        credential(
            NODE_HOST_KEY,
            "Management node address",
            FieldKind::Text,
            true,
            Some("Used both as the SSH host and as the vinfra portal."),
        ),
        credential(
            SSH_USER_KEY,
            "SSH user",
            FieldKind::Text,
            false,
            Some("Defaults to `root`."),
        ),
        credential(
            SSH_PORT_KEY,
            "SSH port",
            FieldKind::Int,
            false,
            Some("Defaults to `22`."),
        ),
        credential(
            SSH_PRIVATE_KEY_KEY,
            "SSH private key",
            FieldKind::MultilineSecret,
            true,
            None,
        ),
        credential(
            VINFRA_USERNAME_KEY,
            "vinfra username",
            FieldKind::Text,
            false,
            Some("Defaults to `admin`."),
        ),
        credential(
            VINFRA_PASSWORD_KEY,
            "vinfra password",
            FieldKind::Secret,
            true,
            None,
        ),
    ]
}

/// The fields [`crate`]'s observation can yield.
#[must_use]
pub fn observed_schema() -> Vec<FieldDesc> {
    vec![
        observed(
            PRODUCT_VERSION_KEY,
            "Product version",
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
            BASE_URL_KEY,
            "Base URL",
            FieldKind::Url,
            Some(FieldRole::BaseUrl),
            true,
        ),
        observed(NODE_COUNT_KEY, "Node count", FieldKind::Int, None, false),
        observed(
            RAW_RELEASE_KEY,
            "Raw hci-release",
            FieldKind::Text,
            None,
            false,
        ),
        observed(
            STORAGE_NAME_KEY,
            "Storage name",
            FieldKind::Text,
            None,
            false,
        ),
    ]
}

/// Classify a submitted credential form.
///
/// # What this checks, and what it deliberately does not
///
/// Presence and non-blankness of the three required fields — `node_host`,
/// `ssh_private_key`, `vinfra_password` — and nothing else. It does not parse
/// the private key or attempt to reach the node; that is what observation
/// does, and its failures are reported through the classified channel
/// `qa_connector_ssh::classify` already contains.
///
/// Keys the schema does not declare are ignored rather than rejected: the
/// gear writes only what a plugin classified, so an undeclared key reaches
/// nothing, and rejecting the whole form over one is a worse failure than
/// dropping it. (The design spec's §4.1 said the opposite until 2026-09-09;
/// the code is the side that is right, and the spec has been corrected.)
///
/// # The one optional field that *is* checked
///
/// `ssh_port`. It is the first `FieldKind::Int` field any plugin declares,
/// and nothing else in the stack enforces the kind: the UI renders `Int` as a
/// plain text input, `qa-environments` stores every submitted field as a JSON
/// string, and [`crate::config::config_port`] falls back to
/// [`DEFAULT_SSH_PORT`] for anything that will not parse. So a typo reached
/// production as a *wrong port*, silently, rather than as a rejected form.
/// Blank still means unset (and still defaults); a non-numeric or
/// out-of-range value is refused here, which is the only place it can be.
///
/// # Errors
///
/// [`FailureClass::Malformed`] when a required field is absent or blank, or
/// when `ssh_port` is present, non-blank, and not a port number. The text is
/// fixed and names no submitted value — the enforcement point for the rule in
/// `PRODUCT-PLUGINS-DESIGN.md` §9.
pub fn validate_credentials(
    input: &CredentialInput,
) -> Result<Vec<CredentialClassification>, PluginFailure> {
    let present = |key: &str| {
        input
            .get(key)
            .is_some_and(|value| !is_blank(value.as_bytes()))
    };

    if !present(NODE_HOST_KEY) {
        return Err(PluginFailure::classified(
            FailureClass::Malformed,
            NODE_HOST_REQUIRED,
        ));
    }
    if !present(SSH_PRIVATE_KEY_KEY) {
        return Err(PluginFailure::classified(
            FailureClass::Malformed,
            SSH_KEY_REQUIRED,
        ));
    }
    if !present(VINFRA_PASSWORD_KEY) {
        return Err(PluginFailure::classified(
            FailureClass::Malformed,
            VINFRA_PASSWORD_REQUIRED,
        ));
    }
    if present(SSH_PORT_KEY)
        && !input
            .get(SSH_PORT_KEY)
            .is_some_and(|value| is_a_port_number(value.as_bytes()))
    {
        return Err(PluginFailure::classified(
            FailureClass::Malformed,
            SSH_PORT_MALFORMED,
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

/// Whether submitted bytes, once trimmed, are a decimal port in `1..=65535`.
///
/// Read as bytes and parsed by `u16`, exactly the shape
/// [`crate::config::config_port`] will later apply to the stored string --
/// so a value this accepts is a value that function will not have to fall
/// back on, which is the whole point of checking here. `0` is refused: it is
/// a valid `u16` and not a connectable port.
///
/// Non-UTF-8 bytes are refused without being decoded; nothing derived from
/// them is formatted, here or by the caller.
fn is_a_port_number(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    text.trim().parse::<u16>().is_ok_and(|port| port != 0)
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
