//! The outbound Slack port — Task 38 fixes the shape (R102); Task 39 builds
//! the oagw-backed adapter, [`SlackClient::REQUEST_TIMEOUT`]'s consumer, and
//! its fix round 1 (ruling R108) adds the [`toolkit_security::SecurityContext`]
//! parameter Task 38's shape omitted — see "One method, and `ctx` is not
//! optional" below.
//!
//! # Why this exists as a port rather than a direct `reqwest` call
//!
//! Legacy's `send_slack_to_channel_with_blocks`
//! (`manager/src/services/notifications.rs:113-130`) is one `reqwest::Client`
//! posting a JSON body built by `slack_webhook_payload` (`:881-904`) directly
//! to a webhook URL held in config. This gear never holds a webhook URL at
//! all — [`SlackMessage::webhook_credstore_ref`] is a credential-store
//! *reference*, per the same divergence
//! [`crate::domain::ports::jira_client`]'s header states for JIRA's
//! `api_token_credstore_ref`: a Slack incoming-webhook URL is not an address
//! that happens to be secret, possession of it *is* the authorization to
//! post, so it gets the same credstore treatment
//! (`qa_insights_sdk::NotificationConfig::slack_webhook_credstore_ref`'s own
//! doc). Resolving the reference to a live URL is the adapter's job, over
//! oagw, exactly as [`crate::domain::ports::jira_client::JiraClient`]'s
//! adapter resolves `api_token_credstore_ref`.
//!
//! # `REQUEST_TIMEOUT` is declared on the trait, not only on Task 39's adapter
//!
//! The controller's ruling for this task's brief pins a associated constant,
//! `Duration::from_secs(10)`, on whichever concrete Slack client Task 39
//! ships (`SlackOagwClient::REQUEST_TIMEOUT`) — legacy's own reasoning
//! (`notifications.rs:53-64`) is that an unbounded request against a
//! black-holing webhook host would stall the caller for the OS TCP timeout
//! rather than losing just the one notification. That constant lives on the
//! concrete adapter type Task 39 defines, not on this trait: an associated
//! `const` on a trait cannot itself be probed through a `Arc<dyn SlackClient>`
//! (there is no way to name it without knowing the concrete type), so pinning
//! it here would be decoration nothing could read back. This module records
//! the expectation in prose so Task 39's own test
//! (`SlackOagwClient::REQUEST_TIMEOUT == Duration::from_secs(10)`) has
//! somewhere to point from.
//!
//! # One method, and `ctx` is not optional — ruling R108
//!
//! `send(&self, ctx: &SecurityContext, message: &SlackMessage) ->
//! Result<SendOutcome, DomainError>`. Task 39's first draft shipped this
//! without a [`toolkit_security::SecurityContext`] parameter, reasoning (by
//! analogy with [`crate::domain::ports::jira_client::JiraClient`]'s three
//! methods) that [`SlackMessage::webhook_credstore_ref`] already carries
//! everything the adapter needs. That reasoning was wrong in a way JIRA's
//! port never was: `oagw::ServiceGatewayClientV1::proxy_request` — the one
//! way any adapter over `oagw` can send anything — **requires** a
//! `SecurityContext` argument, because oagw's own per-tenant upstream
//! resolution rides on it. Without this parameter, Task 39's first adapter
//! could only proxy as `SecurityContext::anonymous()`, a fixed nil-tenant
//! identity, even though its only two callers
//! ([`crate::domain::service::notify::NotifyService::send_test`] and
//! `send_run_completed_channel`) already hold the real sending tenant's `ctx`
//! at the exact point they build a [`SlackMessage`] — the context was not
//! missing from this gear, it was being dropped at this port's boundary. R108
//! closes that: the parameter exists so an oagw-backed adapter authenticates
//! as the tenant that is actually sending, not as nobody. Both call sites now
//! forward the `ctx` they already have; neither had to acquire one it lacked.

use async_trait::async_trait;
use serde_json::Value;
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::ports::SendOutcome;

/// One outbound Slack message, resolved from routing and rendering but not
/// yet sent.
///
/// Carries both shapes this gear renders: [`Event::RunCompleted`](
/// crate::domain::notify::routing::Event::RunCompleted)'s plain text (
/// [`Self::blocks`] empty) and [`Event::ScheduledRun`](
/// crate::domain::notify::routing::Event::ScheduledRun)'s Block Kit layout.
/// Legacy's `slack_webhook_payload` (`notifications.rs:880-904`) is the wire
/// shape the adapter renders this into: `text` always, `channel` only when
/// non-empty, `blocks` only when non-empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlackMessage {
    /// A credential-store reference, never a URL. See this module's header.
    pub webhook_credstore_ref: String,
    /// `None` uses the webhook's own default channel — legacy's
    /// `effective_slack_channel` (`notifications.rs:840-847`) resolves this
    /// one layer up, in [`crate::domain::service::notify`], not here.
    pub channel: Option<String>,
    /// The fallback/plain text. Never empty for a message this gear builds,
    /// though the port does not enforce that.
    pub text: String,
    /// Block Kit blocks; empty for a plain-text send.
    pub blocks: Vec<Value>,
}

/// The outbound Slack egress, behind `infra::slack::SlackOagwClient` in
/// production (Task 39).
///
/// # Errors
///
/// [`DomainError::Internal`] for a transport or gateway failure. Never
/// [`DomainError::UnsupportedEgress`] — unlike
/// [`crate::domain::ports::mail_client::MailClient`], every deployment of
/// this gear ships a Slack adapter (Task 39's), so there is no "no adapter"
/// case for this port to report; `SendOutcome::UnsupportedEgress` on this
/// trait's return type exists only so the two ports share one result type,
/// per this module's header.
#[async_trait]
pub trait SlackClient: Send + Sync {
    /// Post `message` as `ctx`'s subject. `ctx` is the sending tenant's
    /// identity, forwarded to `oagw::ServiceGatewayClientV1::proxy_request` by
    /// the production adapter — see this module's header, "One method, and
    /// `ctx` is not optional", for why it is required rather than optional.
    /// `Ok(SendOutcome::Sent)` on success.
    async fn send(
        &self,
        ctx: &SecurityContext,
        message: &SlackMessage,
    ) -> Result<SendOutcome, DomainError>;
}
