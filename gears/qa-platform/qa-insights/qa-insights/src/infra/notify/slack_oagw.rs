//! [`SlackClient`] over the Outbound API Gateway — Task 39.
//!
//! # Do not reach for `reqwest` here
//!
//! Stated first for the same reason `infra::jira::oagw_client` states it first:
//! a future reader with a webhook URL in hand will reach for `reqwest` by
//! reflex, and legacy did exactly that
//! (`manager/src/services/notifications.rs:20`, a `reqwest::Client` field).
//! This subsystem's egress contract, `cpt-cf-qa-contract-egress`, permits
//! **one** direct-HTTP exception and it belongs to qa-catalog's git transport.
//! Every Slack call in this gear goes through
//! [`ServiceGatewayClientV1::proxy_request`]. A `reqwest` dependency in this
//! crate's `Cargo.toml` is the review finding, not the code that uses it.
//!
//! # The 10-second bound is ported reasoning, not a guess
//!
//! Legacy's `NotificationService::new`
//! (`manager/src/services/notifications.rs:52-64`) builds its `reqwest::Client`
//! with a 10-second total-request timeout, and its own comment explains why:
//! the run-queue dispatcher awaits a Slack send *inside its tick*, for the
//! mandatory `expired` event, so an unbounded request against a black-holing
//! webhook host would stall the whole queue rather than losing just the one
//! notification. **It is a fixed bound, not derived from any tick interval** —
//! legacy's own comment says so explicitly of its dispatcher's tick
//! (`RUN_QUEUE_DISPATCHER_INTERVAL_SECONDS`), and that reasoning survives the
//! port even though the tick this gear will eventually have is qa-runs'
//! (Task 40's), not this one's own. See [`SlackOagwClient::REQUEST_TIMEOUT`].
//!
//! # Finding A (fix round 1) — closed by ruling R108: `send` now takes `ctx`
//!
//! Task 39's first draft proxied every message as `SecurityContext::anonymous()`,
//! because [`SlackClient::send`] then took no [`SecurityContext`] at all while
//! [`ServiceGatewayClientV1::proxy_request`] requires one for `oagw`'s own
//! per-tenant upstream resolution. Both of this gear's call sites —
//! `NotifyService::send_test` (`domain/service/notify.rs:702`) and
//! `send_run_completed_channel`/`RunCompletedChannel::send` (`:1038`, `:1252`)
//! — already held the real sending tenant's `ctx` at the point they built a
//! [`SlackMessage`]; the context was being dropped at the port boundary, not
//! missing from this gear. R108 added `ctx: &SecurityContext` to
//! [`SlackClient::send`] and this adapter now forwards it verbatim to
//! `proxy_request` — see the port's own header, "One method, and `ctx` is not
//! optional", for the ruling in full.
//! [`the_slack_client_forwards_the_callers_security_context`] (this module's
//! tests) pins that the identity reaching `oagw` is the caller's, not a fixed
//! placeholder.
//!
//! # Finding B (fix round 1) — a release-gate item, not a code change here
//!
//! Ruling R107 confirms this finding stands and explicitly keeps it out of any
//! adapter task: even with a correct tenant identity, `oagw` has no built-in
//! way to deliver a credstore secret into a URL path, which is where Slack's
//! incoming-webhook credential lives. `oagw`'s registered auth plugins —
//! `apikey`, `noop`, two `oauth2_client_cred` variants
//! (`gears/system/oagw/oagw/src/infra/plugin/registry.rs:34-66`) — all inject
//! into a **header** (`AuthContext::headers`,
//! `gears/system/oagw/oagw/src/domain/plugin/mod.rs:35-51`); none can set the
//! request **path**, even though `TransformRequestContext::path` is itself
//! mutable (`:139-140`) — no built-in `TransformPlugin` uses it for anything
//! but request-id stamping (`registry.rs:125-145`). This is exactly the
//! header-shaped-versus-path-shaped divergence between JIRA's credential
//! (R76/R77: a bearer/basic token, injected by `apikey`, and the token itself
//! never enters this gear because oagw resolves and injects it) and Slack's
//! incoming webhook, whose credential *is* the URL, mostly the path.
//!
//! **Closing this needs one of two things, neither of which belongs inside a
//! two-file adapter task:** a `credstore-sdk` dependency in *this* gear, to
//! resolve `SlackMessage::webhook_credstore_ref` itself and hand `oagw` a
//! complete URI (this crate's `Cargo.toml` declares `oagw`/`oagw-sdk` and no
//! `credstore-sdk` today — verified, not assumed); or a new `oagw` capability
//! that can carry a path-shaped secret the way its auth plugins carry a
//! header-shaped one (a change to a different gear's crate). The controller
//! has recorded this as a release-gate item and raised it to the human,
//! rather than leaving it for whichever task next touches this file to
//! rediscover.
//!
//! [`webhook_path`] is the visible placeholder for this gap: it turns the
//! credstore-ref string into a request path by stripping its scheme, with no
//! guarantee any `oagw` upstream is registered under the result, because
//! nothing in this gear resolves the reference to find out what should be
//! there. **This means `SlackOagwClient` cannot deliver a real Slack message
//! to a real tenant's webhook today** — built, bounded (the ten-second
//! timeout) and tested, but not yet deliverable. It degrades safely rather
//! than dangerously: Task 38's claim lifecycle releases the claim and writes
//! an `OUTCOME_FAILED` audit row on a failed send
//! (`domain::service::notify::NotifyService::send_run_completed_channel`), so
//! nothing is silently suppressed and an operator can see every failure.
//!
//! **Task 40 bound this adapter into `gear.rs` anyway, and that is the correct
//! action** — the alternative is a deployment that reports every Slack send as
//! `unsupported_egress` and never surfaces the credential problem at all.
//! Binding it changes *which* of Task 38's six claim outcomes a send reaches
//! (a failed send, claim released, `OUTCOME_FAILED` logged) and not how many
//! there are. Whoever picks up either candidate fix above should start reading
//! here.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use oagw_sdk::Body;
use oagw_sdk::api::ServiceGatewayClientV1;
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::ports::{CREDSTORE_REF_SCHEME, SendOutcome, SlackClient, SlackMessage};

/// [`SlackClient`] over `oagw`. See this module's header for Finding A
/// (closed, R108) and Finding B (open, a release-gate item per R107) — how
/// far this can currently reach.
pub struct SlackOagwClient {
    gateway: Arc<dyn ServiceGatewayClientV1>,
    /// The bound `send` applies to one proxied request.
    /// [`Self::REQUEST_TIMEOUT`] in production, always — [`Self::with_timeout`]
    /// exists only so a test can prove the bound is *applied*, not merely
    /// declared, without waiting out ten real seconds.
    timeout: Duration,
}

impl SlackOagwClient {
    /// The fixed bound this module's header explains. `Duration::from_secs(10)`
    /// is legacy's own value
    /// (`manager/src/services/notifications.rs:58-59`, "10s is generous for a
    /// webhook - Slack's own guidance is ~3s").
    pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

    #[must_use]
    pub fn new(gateway: Arc<dyn ServiceGatewayClientV1>) -> Self {
        Self {
            gateway,
            timeout: Self::REQUEST_TIMEOUT,
        }
    }

    /// Test-only escape hatch: a client whose bound is *not*
    /// [`Self::REQUEST_TIMEOUT`], so a test can exercise the bound with a
    /// duration measured in milliseconds rather than the real ten seconds.
    /// Production code has exactly one way to build this type —
    /// [`Self::new`] — and always gets the real bound.
    #[cfg(test)]
    fn with_timeout(gateway: Arc<dyn ServiceGatewayClientV1>, timeout: Duration) -> Self {
        Self { gateway, timeout }
    }
}

#[async_trait]
impl SlackClient for SlackOagwClient {
    async fn send(
        &self,
        ctx: &SecurityContext,
        message: &SlackMessage,
    ) -> Result<SendOutcome, DomainError> {
        let request = build_request(message)?;

        // R108: `ctx` is the caller's own security context, forwarded
        // verbatim — see the port's header and this module's "Finding A"
        // section for why this parameter exists at all.
        let sent = tokio::time::timeout(
            self.timeout,
            self.gateway.proxy_request(ctx.clone(), request),
        )
        .await
        .map_err(|_| {
            DomainError::Internal(format!(
                "the Slack request did not complete within {:?}",
                self.timeout
            ))
        })?
        .map_err(|e| DomainError::Internal(format!("the Slack request failed: {e}")))?;

        if sent.status().is_success() {
            Ok(SendOutcome::Sent)
        } else {
            Err(DomainError::Internal(format!(
                "Slack refused the message: the webhook answered {}",
                sent.status()
            )))
        }
    }
}

/// Build the one proxied HTTP request for `message`.
fn build_request(message: &SlackMessage) -> Result<http::Request<Body>, DomainError> {
    let payload = slack_webhook_payload(message);
    let uri = format!("/{}", webhook_path(&message.webhook_credstore_ref));
    http::Request::builder()
        .method(http::Method::POST)
        .uri(uri)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .map_err(|e| DomainError::Internal(format!("could not build the Slack request: {e}")))
}

/// The outgoing Slack webhook payload. Legacy's `slack_webhook_payload`
/// (`manager/src/services/notifications.rs:880-904`), field for field: `text`
/// always, `channel` only when [`SlackMessage::channel`] is present and
/// non-blank after trimming, `blocks` only when non-empty.
fn slack_webhook_payload(message: &SlackMessage) -> serde_json::Value {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "text".to_owned(),
        serde_json::Value::String(message.text.clone()),
    );
    let channel = message
        .channel
        .as_deref()
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty());
    if let Some(channel) = channel {
        payload.insert(
            "channel".to_owned(),
            serde_json::Value::String(channel.to_owned()),
        );
    }
    if !message.blocks.is_empty() {
        payload.insert(
            "blocks".to_owned(),
            serde_json::Value::Array(message.blocks.clone()),
        );
    }
    serde_json::Value::Object(payload)
}

/// Finding B (module header): a stand-in for a real destination. Strips a
/// [`CREDSTORE_REF_SCHEME`] prefix if present and sends the remainder as the
/// path `oagw` is asked to proxy — the same shape `proxy_request`'s own doc
/// describes (`/{alias}/{path_suffix}`), but with no guarantee any oagw upstream
/// is registered under it, because nothing in this gear ever resolves the
/// reference to find out what should be.
///
/// # One prefix spelling, `cred://` — Phase C's final review, Important 1
///
/// This used to strip `credstore://` as well, and this crate's own fixtures
/// used that spelling while the DTO tests used `cred://`. `credstore://` was
/// never valid: it carries a colon and slashes, which
/// [`validate_credstore_ref`](crate::domain::ports::validate_credstore_ref)
/// rejects and `SecretRef` prohibits outright
/// (`gears/credstore/credstore-sdk/src/models.rs:42-83`). Now that
/// `NotifyService::save_config` applies that check, no row this gear writes can
/// carry it, so stripping it would be code shaped around syntax the write path
/// refuses — and a row stored before the rule existed is better sent verbatim
/// and failing at oagw than silently rewritten into something that looks
/// resolvable.
fn webhook_path(webhook_credstore_ref: &str) -> String {
    webhook_credstore_ref
        .strip_prefix(CREDSTORE_REF_SCHEME)
        .unwrap_or(webhook_credstore_ref)
        .to_owned()
}

#[cfg(test)]
#[path = "slack_oagw_tests.rs"]
mod tests;
