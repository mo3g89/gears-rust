//! [`config_str`]'s blank-is-unset rule and [`config_port`]'s fallback.
//!
//! `config.rs` had a twelve-line doc and no tests at all until 2026-09-09.
//! Both functions are read by [`crate::observe`] *and* by [`crate::run`], so
//! a change to either silently changes what host a live observation reaches
//! and what a dispatched run is told -- which is exactly the pair of call
//! sites the module was extracted to keep in agreement.
//!
//! # Why `config_port` still has a fallback at all, now that submission is validated
//!
//! `schemas::validate_credentials` refuses a malformed `ssh_port` on the way
//! in, so a *newly submitted* one cannot reach here. Rows written before that
//! check existed can, and so can a value put in `config` by anything but the
//! form. This function is read on the observation ticker and on dispatch,
//! neither of which may panic over an operator's typo, so it falls back --
//! and these tests pin the fallback rather than assuming it is unreachable.

use super::*;

#[test]
fn a_present_non_blank_value_is_what_the_operator_set() {
    let config = serde_json::json!({ "ssh_user": "vhiadmin" });
    assert_eq!(config_str(&config, "ssh_user"), Some("vhiadmin"));
}

/// Surrounding whitespace is trimmed off the *returned* value, not merely
/// used to decide presence: an `ssh_user` of `"root "` would otherwise reach
/// a remote command with a trailing space in it.
#[test]
fn a_configured_value_comes_back_trimmed() {
    let config = serde_json::json!({ "ssh_user": "  vhiadmin\n" });
    assert_eq!(config_str(&config, "ssh_user"), Some("vhiadmin"));
}

/// The rule this module exists to state once: blank is not set. An operator
/// who cleared a field means "use the default", not "use the empty string".
#[test]
fn a_blank_value_does_not_count_as_set() {
    for blank in ["", " ", "\t\n "] {
        let config = serde_json::json!({ "ssh_user": blank });
        assert_eq!(config_str(&config, "ssh_user"), None, "blank: {blank:?}");
    }
}

#[test]
fn an_absent_key_is_unset() {
    assert_eq!(config_str(&serde_json::json!({}), "ssh_user"), None);
}

/// `config` is operator-authored JSON reached through a column, so it can be
/// any shape at all -- including not an object. None of these may panic.
#[test]
fn a_config_of_the_wrong_shape_is_unset_rather_than_a_panic() {
    for config in [
        serde_json::Value::Null,
        serde_json::json!([1, 2, 3]),
        serde_json::json!("a bare string"),
        serde_json::json!({ "ssh_user": 22 }),
        serde_json::json!({ "ssh_user": null }),
        serde_json::json!({ "ssh_user": { "nested": true } }),
    ] {
        assert_eq!(config_str(&config, "ssh_user"), None, "config: {config}");
    }
}

#[test]
fn a_configured_port_is_parsed() {
    assert_eq!(
        config_port(&serde_json::json!({ "ssh_port": "2222" })),
        2222
    );
}

#[test]
fn a_configured_port_survives_surrounding_whitespace() {
    assert_eq!(
        config_port(&serde_json::json!({ "ssh_port": " 2222 " })),
        2222
    );
}

#[test]
fn an_absent_or_blank_port_is_the_default() {
    assert_eq!(config_port(&serde_json::json!({})), DEFAULT_SSH_PORT);
    assert_eq!(
        config_port(&serde_json::json!({ "ssh_port": "   " })),
        DEFAULT_SSH_PORT
    );
}

/// A stored value that will not parse falls back rather than panicking -- see
/// this module's header for which writers can still produce one now that
/// submission is validated. `65536` is the interesting case: it is a number,
/// and it is not a `u16`.
#[test]
fn a_port_that_will_not_parse_falls_back_rather_than_panicking() {
    for stored in ["twenty-two", "65536", "-1", "22.0", "22 "] {
        assert_eq!(
            config_port(&serde_json::json!({ "ssh_port": stored })),
            DEFAULT_SSH_PORT,
            "stored: {stored}"
        );
    }
}

/// A JSON *number* is not read, deliberately: every writer today stores a
/// submitted field as a JSON string, and accepting a second shape here would
/// mean two encodings of one value that could disagree. The fallback is what
/// keeps that decision safe rather than fatal.
#[test]
fn a_numeric_json_port_falls_back_because_no_writer_produces_one() {
    assert_eq!(
        config_port(&serde_json::json!({ "ssh_port": 2222 })),
        DEFAULT_SSH_PORT
    );
}
