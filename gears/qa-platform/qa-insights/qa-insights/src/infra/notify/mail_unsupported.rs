//! [`MailClient`]'s one production implementation — the inert D10 adapter.
//!
//! # D10, in full, because a future reader will land here looking for SMTP
//!
//! [`crate::domain::ports::mail_client`]'s header records the decision this
//! type carries out: config, routing and dedupe for email ship exactly as
//! they do for Slack (Task 38's notification service treats the two channels
//! identically), and only the SMTP socket itself is deferred. This type *is*
//! that deferral. `send` never touches [`MailMessage::smtp_host`] or any other
//! field, never opens a connection, and — the property the port's own doc
//! states belongs to the port and not to any one adapter — never returns
//! `Err`. It reports [`SendOutcome::UnsupportedEgress`], the value
//! [`crate::domain::service::notify::NotifyService`] logs as
//! `"unsupported_egress"` rather than treats as a failure, so a deployment
//! with no SMTP relay configured still gets a complete, honest audit trail
//! for every notification it could not send.
//!
//! # Replacing this later
//!
//! A future SMTP adapter is a **new type**, not an edit to this one:
//!
//! 1. Implement [`MailClient`] for it — a `lettre` transport over
//!    [`MailMessage::smtp_host`]/[`MailMessage::smtp_port`], answering
//!    [`DomainError::Internal`] for a transport failure and
//!    [`DomainError::Validation`] for an address `lettre` refuses to parse,
//!    matching the port's own doc and legacy's two failure shapes
//!    (`manager/src/services/notifications.rs:139-176`).
//! 2. Bind it in `gear.rs`, replacing [`UnsupportedMailClient`] on the
//!    `mail_client:` line of `ServiceDeps`. (Task 40 put this type there,
//!    replacing Task 38's `NeverWiredMailClient` stand-in, which no longer
//!    exists.)
//! 3. **Delete nothing else.** This type does not go away: it stays the
//!    correct answer for every deployment that has not configured SMTP,
//!    never only a placeholder for the time before the SMTP adapter exists.

use async_trait::async_trait;

use crate::domain::error::DomainError;
use crate::domain::ports::{MailClient, MailMessage, SendOutcome};

/// The one production [`MailClient`] — see this module's header for why it
/// never dials out and never fails.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnsupportedMailClient;

#[async_trait]
impl MailClient for UnsupportedMailClient {
    async fn send(&self, _message: &MailMessage) -> Result<SendOutcome, DomainError> {
        Ok(SendOutcome::UnsupportedEgress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> MailMessage {
        MailMessage {
            smtp_host: "smtp.example.com".to_owned(),
            smtp_port: 587,
            from: "qa@example.com".to_owned(),
            recipients: "a@example.com,b@example.com".to_owned(),
            subject: "subject".to_owned(),
            body: "body".to_owned(),
        }
    }

    /// D10. The mail adapter is deliberately inert: it reports the outcome the
    /// log records and returns Ok, so a missing SMTP path never fails a run's
    /// notification pass. Brief: `task-39-brief.md` Step 1, verbatim.
    #[tokio::test]
    async fn the_unsupported_mail_client_reports_rather_than_fails() {
        let outcome = UnsupportedMailClient
            .send(&message())
            .await
            .expect("never errors");
        assert_eq!(outcome, SendOutcome::UnsupportedEgress);
    }

    /// The port's contract is per-message, not per-adapter-instance: two
    /// different messages both get the same honest answer rather than the
    /// first "using up" some hidden one-shot state.
    #[tokio::test]
    async fn every_message_reports_unsupported_egress() {
        let client = UnsupportedMailClient;
        for _ in 0..3 {
            let outcome = client.send(&message()).await.expect("never errors");
            assert_eq!(outcome, SendOutcome::UnsupportedEgress);
        }
    }
}
