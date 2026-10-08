//! Tests for [`SlackWebhook::parse`] — the one gate between a credential-store
//! secret and a proxied request.
//!
//! Every refusal is also asserted **not to echo its input**: the value under
//! test is the tenant's webhook secret, and a refusal's text reaches the
//! settings page and the audit log.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{SLACK_WEBHOOK_HOST, SlackWebhook};
use crate::domain::error::DomainError;

const GOOD: &str = "https://hooks.slack.com/services/T000/B000/XXXXXXXX";

/// The fixed shape hint every refusal carries. Removed before the no-echo
/// check, because two of the refused inputs (`https://hooks.slack.com` and
/// `…/services/`) are prefixes of it and would otherwise "match" text that was
/// never derived from them.
const SHAPE_HINT: &str = "(https://hooks.slack.com/services/\u{2026})";

#[test]
fn a_slack_incoming_webhook_url_is_accepted_and_proxied_under_the_host_alias() {
    let hook = SlackWebhook::parse(GOOD).expect("valid");
    assert_eq!(
        hook.proxy_path(),
        "/hooks.slack.com/services/T000/B000/XXXXXXXX"
    );
    assert_eq!(SLACK_WEBHOOK_HOST, "hooks.slack.com");
}

#[test]
fn surrounding_whitespace_and_a_trailing_host_dot_and_case_are_tolerated() {
    let hook = SlackWebhook::parse("  https://HOOKS.slack.com./services/T/B/X \n")
        .expect("whitespace, one trailing dot and host case are all tolerated");
    assert_eq!(hook.proxy_path(), "/hooks.slack.com/services/T/B/X");
    SlackWebhook::parse("https://hooks.slack.com:443/services/T/B/X")
        .expect("an explicit default port is the same URL");
}

#[test]
fn every_non_slack_shape_is_refused_without_echoing_the_value() {
    for bad in [
        // Not HTTPS.
        "http://hooks.slack.com/services/T/B/X",
        // A host that merely starts with the Slack one.
        "https://hooks.slack.com.evil.test/services/T/B/X",
        // Another host outright.
        "https://evil.test/services/T/B/X",
        // Userinfo — a `user@host` spelling that could disguise the host.
        "https://user:pw@hooks.slack.com/services/T/B/X",
        "https://hooks.slack.com@evil.test/services/T/B/X",
        // A non-default port.
        "https://hooks.slack.com:8443/services/T/B/X",
        // Not an incoming-webhook path.
        "https://hooks.slack.com/api/T/B/X",
        "https://hooks.slack.com/services/",
        "https://hooks.slack.com",
        // A query or a fragment.
        "https://hooks.slack.com/services/T/B/X?x=1",
        "https://hooks.slack.com/services/T/B/X#f",
        // Traversal out of `/services`.
        "https://hooks.slack.com/services/../admin",
        "https://hooks.slack.com/services/T/../../admin",
        // An encoded slash, either case.
        "https://hooks.slack.com/services/T%2fB/X",
        "https://hooks.slack.com/services/T%2FB/X",
        // Two trailing dots is not the absolute-FQDN spelling.
        "https://hooks.slack.com../services/T/B/X",
        // A bracketed IPv6 host.
        "https://[::1]/services/T/B/X",
        // Whitespace inside the value.
        "https://hooks.slack.com/services/T B/X",
        "",
        "   ",
        "not a url",
    ] {
        let err = SlackWebhook::parse(bad).expect_err(bad);
        let DomainError::Validation { field, message } = &err else {
            panic!("{bad:?}: expected a Validation error, got {err:?}");
        };
        assert_eq!(field, "slack_webhook_credstore_ref", "{bad:?}");
        let rendered = err.to_string().replace(SHAPE_HINT, "");
        assert!(
            bad.trim().is_empty() || !rendered.contains(bad.trim()),
            "echoed: {bad:?} in {rendered:?}"
        );
        // Nor any fragment of the path — the path *is* the credential.
        let message = message.replace(SHAPE_HINT, "");
        assert!(!message.contains("/T/"), "echoed a path: {message:?}");
        assert!(!message.contains("evil"), "echoed a host: {message:?}");
    }
}

/// The `Debug` rendering of a parsed webhook is redacted too: a `{:?}` in a
/// future log line must not print the secret.
#[test]
fn the_debug_rendering_does_not_carry_the_path() {
    let hook = SlackWebhook::parse(GOOD).expect("valid");
    let rendered = format!("{hook:?}");
    assert!(!rendered.contains("XXXXXXXX"), "{rendered}");
    assert!(!rendered.contains("T000"), "{rendered}");
}
