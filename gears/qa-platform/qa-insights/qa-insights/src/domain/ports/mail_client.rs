//! The outbound email port — Task 38 fixes the shape (R102); Task 39 supplies
//! the one production implementation, and D10 is why that implementation
//! never actually sends.
//!
//! # D10: this port exists so the rest of the feature ships, not so mail does
//!
//! Legacy's `send_email` (`manager/src/services/notifications.rs:132-177`)
//! builds a `lettre` message and hands it to an SMTP transport. This gear's
//! design defers the SMTP send itself (decision D10, recorded on
//! [`crate::domain::error::DomainError::UnsupportedEgress`]'s own doc):
//! config, routing and dedupe all ship for email exactly as they do for
//! Slack, and only the socket is missing. **Task 39's adapter for this trait
//! is the inert one** — its test asserts
//! `UnsupportedMailClient.send(&ctx, &message).await == Ok(SendOutcome::UnsupportedEgress)`,
//! never an `Err`, which is this module's second header point:
//!
//! # `send` never errors, and that is a property of the *port*, not a quirk
//! # of one adapter
//!
//! [`MailClient::send`] returns `Result<SendOutcome, DomainError>` rather
//! than `Result<(), DomainError>` precisely so "this deployment has no mail
//! adapter" can be a **value** the caller logs
//! ([`crate::domain::service::notify::NotifyService`] maps
//! [`SendOutcome::UnsupportedEgress`] to the audit log's `"unsupported_egress"`
//! outcome string — the seam this task owns end to end, since no single
//! task's tests cover the mapping from either side alone) rather than an
//! error the caller has to special-case. A future SMTP-backed adapter would
//! still answer through this same `Ok(SendOutcome::Sent)` / `Err(..)` shape
//! for an address it could not resolve or a socket it could not open; only
//! the inert adapter is guaranteed never to take the `Err` arm at all.
//!
//! # One method, and it takes a `ctx` — review finding #37
//!
//! [`MailClient::send`] takes `ctx: &SecurityContext`, the same as
//! [`crate::domain::ports::slack_client::SlackClient::send`] (which gained it
//! in fix round 1, ruling R108). **This module used to argue the opposite**,
//! and the argument is recorded here because it is the kind a future reader
//! will reconstruct and act on: D10's inert adapter never reaches a network,
//! so there is no per-tenant resolution for a context to drive, and a future
//! SMTP-backed adapter would authenticate to a relay from [`MailMessage`]'s
//! own `smtp_host`/`smtp_port` rather than from an identity — therefore the
//! parameter would be unused surface, which `domain::ports`' header warns
//! against.
//!
//! What that argument misses is that these are **two egress ports of one
//! notification service**, called from the same two methods, for the same
//! event, under the same tenant's authority, and they returned different
//! answers to "on whose behalf is this being sent?". A reader comparing them
//! had to reconstruct the whole D10-inertness story to learn that the
//! asymmetry was not an oversight — and *"currently unused"* is a property of
//! today's one adapter, not of the port: the moment any deployment ships a
//! real relay, "which tenant is this email for" is the first question an
//! adapter has to answer, for tenant-scoped relay credentials and for the
//! audit trail, and a port that never carried the identity would have to
//! change shape to answer it. The two notification ports now have one
//! authorization story. Both call sites
//! ([`crate::domain::service::notify::NotifyService::send_test`] and
//! `RunCompletedChannel::send`) already held the sending tenant's `ctx` where
//! they build a [`MailMessage`]; neither had to acquire one it lacked, which
//! is the same thing R108 found for Slack.

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::ports::SendOutcome;

/// One outbound email, resolved from the tenant's stored SMTP settings but
/// not yet sent.
///
/// Field-for-field what legacy's `send_email` reads off `NotificationsConfig`
/// (`notifications.rs:133-177`) plus the two values that method takes as
/// arguments (`subject`, `body`) rather than from config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailMessage {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub from: String,
    /// Comma-separated, stored and forwarded exactly as
    /// [`qa_insights_sdk::NotificationConfig::email_recipients`] holds it —
    /// the split into individual mailboxes is the adapter's job, matching
    /// legacy's own `split(',')` (`notifications.rs:144-153`).
    pub recipients: String,
    pub subject: String,
    pub body: String,
}

/// The outbound email egress, behind the inert adapter Task 39 ships (D10).
///
/// # Errors
///
/// This module's header states the property the inert adapter holds
/// (`send` never returns `Err`). A future SMTP-backed adapter would use
/// [`DomainError::Internal`] for a transport failure and
/// [`DomainError::Validation`] for an address `lettre` refuses to parse,
/// matching legacy's own two failure shapes at `notifications.rs:139-176`.
#[async_trait]
pub trait MailClient: Send + Sync {
    /// Send `message` as `ctx`'s subject. `ctx` is the sending tenant's
    /// identity — see this module's header, "One method, and it takes a
    /// `ctx`", for why it is required even though D10's inert adapter has
    /// nothing to do with it. `Ok(SendOutcome::UnsupportedEgress)` on every
    /// deployment that ships only that adapter.
    async fn send(
        &self,
        ctx: &SecurityContext,
        message: &MailMessage,
    ) -> Result<SendOutcome, DomainError>;
}
