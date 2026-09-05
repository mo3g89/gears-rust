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
//! `UnsupportedMailClient.send(&message).await == Ok(SendOutcome::UnsupportedEgress)`,
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
//! # One method, no [`toolkit_security::SecurityContext`] parameter
//!
//! [`crate::domain::ports::slack_client::SlackClient::send`] gained a `ctx`
//! parameter in fix round 1 (ruling R108), because its production adapter
//! proxies through `oagw` and `oagw` requires a tenant identity. **That
//! reasoning does not carry over here.** D10's inert adapter never reaches a
//! network at all — there is no per-tenant resolution for a context to drive,
//! and a future SMTP-backed adapter would authenticate to an SMTP relay from
//! [`MailMessage`]'s own fields (`smtp_host`, `smtp_port`), not from a
//! `SecurityContext`. Adding the parameter here would be exactly the unused
//! one `domain::ports`' header warns against — a port with no caller for what
//! it declares.

use async_trait::async_trait;

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
    /// Send `message`. `Ok(SendOutcome::UnsupportedEgress)` on every
    /// deployment that ships only the inert adapter.
    async fn send(&self, message: &MailMessage) -> Result<SendOutcome, DomainError>;
}
