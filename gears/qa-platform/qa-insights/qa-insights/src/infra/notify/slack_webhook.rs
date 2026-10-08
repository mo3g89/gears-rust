//! The Slack incoming-webhook URL a tenant's credential-store secret holds, and
//! the one check it passes before anything is dialled.
//!
//! # Why this exists
//!
//! A Slack incoming webhook's credential **is its URL path**
//! (`https://hooks.slack.com/services/T…/B…/<token>`), and `oagw`'s auth plugins
//! inject only headers. So [`super::slack_oagw::SlackOagwClient`] resolves the
//! secret itself and hands `oagw` the path — which makes this module the gate
//! between a value the tenant typed into their credential store and a request
//! this gear sends on their behalf.
//!
//! # What it refuses, and why it is strict
//!
//! The host is fixed ([`SLACK_WEBHOOK_HOST`]) rather than taken from the secret,
//! so a stored value cannot aim this gear's egress anywhere else. On top of
//! that, [`SlackWebhook::parse`] refuses anything that is not plainly an
//! incoming-webhook URL: another scheme, another host (including a host that
//! only *starts* with Slack's), userinfo, a non-default port, a path outside
//! `/services/`, a query, a fragment, and any path segment that is not a bare
//! token of ASCII letters, digits, `-` or `_`. That last rule is what closes
//! traversal (`..`, `.`), encoded separators (`%2f`, `%5c`) and encoded dots in
//! one place, rather than one rule per spelling; Slack's own webhook segments
//! (`T…`, `B…`, the token) are alphanumeric.
//!
//! # The value is never echoed
//!
//! Every refusal is a [`DomainError::Validation`] on
//! `slack_webhook_credstore_ref` with **fixed** text: the input is the tenant's
//! secret, and a validation message reaches both the settings page and the
//! audit log's `error` column. [`SlackWebhook`]'s `Debug` is redacted for the
//! same reason.

use crate::domain::error::DomainError;

/// The only host a Slack notification from this gear is ever sent to — and the
/// alias of the per-tenant `oagw` upstream it is sent through.
pub const SLACK_WEBHOOK_HOST: &str = "hooks.slack.com";

/// The `NotificationConfigDto` field a refusal names: the reference whose
/// secret did not hold a usable webhook.
pub const REF_FIELD: &str = "slack_webhook_credstore_ref";

/// The incoming-webhook path prefix every accepted URL carries — and the path
/// of the one `oagw` route it is proxied through.
pub const SERVICES_PREFIX: &str = "/services";

/// A validated Slack incoming webhook. Holds the path only; the scheme, host
/// and port are fixed by construction.
pub struct SlackWebhook {
    /// `/services/T…/B…/<token>` — the credential. Never formatted.
    path: String,
}

impl std::fmt::Debug for SlackWebhook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackWebhook")
            .field("path", &"<redacted>")
            .finish()
    }
}

impl SlackWebhook {
    /// Validate `raw` (surrounding whitespace ignored) as a Slack
    /// incoming-webhook URL.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming `slack_webhook_credstore_ref`, whose
    /// message never contains any part of `raw`.
    pub fn parse(raw: &str) -> Result<Self, DomainError> {
        let refuse = |why: &str| DomainError::Validation {
            field: REF_FIELD.to_owned(),
            message: format!(
                "the stored secret is not a Slack incoming-webhook URL \
                 (https://{SLACK_WEBHOOK_HOST}/services/\u{2026}): {why}"
            ),
        };

        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(refuse("it is empty"));
        }
        // `http::Uri` drops a fragment rather than refusing it, so it is
        // checked on the raw text.
        if trimmed.contains('#') {
            return Err(refuse("it must not carry a fragment"));
        }
        let uri: http::Uri = trimmed.parse().map_err(|_| refuse("it is not a URL"))?;

        if uri.scheme_str() != Some("https") {
            return Err(refuse("it must use https"));
        }
        let Some(authority) = uri.authority() else {
            return Err(refuse("it has no host"));
        };
        let authority = authority.as_str();
        if authority.contains('@') {
            return Err(refuse("credentials in the URL are not supported"));
        }
        if authority.starts_with('[') {
            return Err(refuse("the host must be the Slack webhook host"));
        }
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        if !matches!(port, None | Some("443")) {
            return Err(refuse("the port must be the default HTTPS port"));
        }
        let host = host.to_ascii_lowercase();
        // One trailing dot is the absolute-FQDN spelling of the same name.
        let host = host.strip_suffix('.').unwrap_or(&host);
        if host != SLACK_WEBHOOK_HOST {
            return Err(refuse("the host must be the Slack webhook host"));
        }

        if uri.query().is_some() {
            return Err(refuse("it must not carry a query"));
        }
        let path = uri.path();
        let Some(rest) = path
            .strip_prefix(SERVICES_PREFIX)
            .and_then(|rest| rest.strip_prefix('/'))
        else {
            return Err(refuse("the path must begin with /services/"));
        };
        let token_segment = |segment: &str| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        if !rest.split('/').all(token_segment) {
            return Err(refuse(
                "every path segment after /services/ must be a plain token of letters, digits, \
                 '-' or '_'",
            ));
        }

        Ok(Self {
            path: path.to_owned(),
        })
    }

    /// The webhook's own path, `/services/…` — the route-relative suffix
    /// `oagw::resolve_proxy_target` is asked about. The credential: never
    /// format it.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The path `oagw::proxy_request` is handed: the upstream alias
    /// ([`SLACK_WEBHOOK_HOST`]) followed by the webhook's own path. The result
    /// is the credential — it goes into the request and nowhere else.
    pub fn proxy_path(&self) -> String {
        format!("/{SLACK_WEBHOOK_HOST}{}", self.path)
    }
}

#[cfg(test)]
#[path = "slack_webhook_tests.rs"]
mod tests;
