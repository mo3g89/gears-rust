//! The one rule for reading operator-authored JSON configuration, shared by
//! [`crate::observe`] and [`crate::run`] so the two never carry two copies of
//! the same "blank counts as unset" decision that could quietly drift apart.
//!
//! Both callers read the identical `config: &serde_json::Value` an
//! [`qa_product_sdk::plugin::EnvironmentHandle`] carries; the rule for what
//! counts as "the operator set this" must therefore live in exactly one place.

use crate::schemas::{DEFAULT_SSH_PORT, SSH_PORT_KEY};

/// A configured value, present and non-blank. A present-but-blank value does
/// not count as set any more than an absent one does -- the rule VHP applies
/// to its own `vpadm_namespace` override, applied here to `ssh_user`,
/// `ssh_port` and `vinfra_username`.
pub fn config_str<'a>(config: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    config
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// `ssh_port`, defaulted and falling back rather than panicking on a
/// configuration value of the wrong shape -- the configuration channel is
/// operator-authored JSON, so a value that will not parse as a port is a
/// shape this has to survive, not a case that cannot happen.
///
/// Reads only [`serde_json::Value::as_str`]: every submitted credential
/// field, `ssh_port` (`FieldKind::Int`) included, is written by
/// `qa-environments`' `write_classified_credentials` as a JSON string
/// (`environment_credentials.rs`), never a JSON number, so there is today no
/// writer this needs to also accept `as_u64` from. `ssh_port` is the first
/// `Int`-kind *config* field any plugin has declared; if a future writer ever
/// stores config numerically, this falls back to `DEFAULT_SSH_PORT` exactly
/// as it does for a value that fails to parse, rather than panicking.
pub fn config_port(config: &serde_json::Value) -> u16 {
    config_str(config, SSH_PORT_KEY)
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_SSH_PORT)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
