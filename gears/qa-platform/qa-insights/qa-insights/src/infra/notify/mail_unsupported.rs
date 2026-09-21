//! The [`MailClient`] a deployment gets when its operator has not enabled SMTP
//! egress — and the one that fails rather than pretends.
//!
//! # This type outlived the decision that created it
//!
//! It was built for D10: config, routing and dedupe shipped for email exactly
//! as for Slack, and the SMTP socket was deferred, so this adapter reported
//! `Ok(SendOutcome::UnsupportedEgress)` — never an `Err`, by a property the
//! port's own header claimed for the *port*. D10 is closed
//! ([`crate::domain::ports::mail_client`]'s header), and
//! [`crate::infra::notify::mail_smtp::SmtpMailClient`] is the adapter that
//! sends. **This type did not go away with it**, exactly as its own replacement
//! recipe predicted: it is still the correct answer for a deployment that has
//! not configured SMTP egress at all.
//!
//! # What changed is the answer, and it is the point of the whole change
//!
//! `send` now returns `Err(DomainError::UnsupportedEgress { channel:
//! "email" })`. The old `Ok(..)` was not a rounding error; it was the defect.
//! An operator filled in the SMTP columns, pressed the test button, and got a
//! success back for a message that was never sent — and on the run-completed
//! path the audit log recorded the non-failure string `"unsupported_egress"`
//! against a notification nobody received.
//!
//! Now:
//!
//! * `NotifyService::send_test` propagates the error, which
//!   `api::rest::error` renders as a `501` naming the channel. The operator is
//!   told email does not work on this deployment.
//! * `NotifyService::send_run_completed_channel` takes its `Err` arm: the
//!   dedupe claim is released (so a later deployment *with* SMTP is not
//!   permanently suppressed for that run) and the audit row is written with
//!   `outcome = "unsupported_egress"` — the same string as before, chosen now
//!   by `failure_outcome_str` from the error rather than from a `SendOutcome`,
//!   so "this deployment cannot send email" stays distinguishable from "the
//!   relay refused this message".
//!
//! # Binding
//!
//! `gear::init` chooses between this and
//! [`SmtpMailClient`](crate::infra::notify::mail_smtp::SmtpMailClient) on
//! `QaInsightsConfig::smtp_allowed_hosts` being empty. It is an explicit
//! binding of a named fallback, not an absence — which is why this type has a
//! module rather than being an `Option` on the service.

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::ports::{MailClient, MailMessage, SendOutcome};

/// The channel name this adapter reports itself under, matching the audit
/// log's `channel` column and `RunCompletedChannel::name`.
const CHANNEL: &str = "email";

/// The [`MailClient`] for a deployment with no SMTP egress configured — see
/// this module's header for why it fails rather than reporting an outcome.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnsupportedMailClient;

#[async_trait]
impl MailClient for UnsupportedMailClient {
    async fn send(
        &self,
        _ctx: &SecurityContext,
        _message: &MailMessage,
    ) -> Result<SendOutcome, DomainError> {
        Err(DomainError::UnsupportedEgress {
            channel: CHANNEL.to_owned(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// The tenant these tests send as. Not [`SecurityContext::anonymous`]:
    /// review finding #37 gave this port the `ctx` parameter so a caller never
    /// has to fall back to that, and a test that passed the anonymous context
    /// would be pinning the shape the finding removed.
    fn ctx() -> SecurityContext {
        SecurityContext::builder()
            .subject_id(uuid::Uuid::from_u128(0xDEAD))
            .subject_tenant_id(uuid::Uuid::from_u128(0x1A11))
            .build()
            .expect("subject_id and subject_tenant_id are both set")
    }

    fn message() -> MailMessage {
        MailMessage {
            smtp_host: "smtp.example.com".to_owned(),
            smtp_port: 587,
            credentials: None,
            from: "qa@example.com".to_owned(),
            recipients: "a@example.com,b@example.com".to_owned(),
            subject: "subject".to_owned(),
            body: "body".to_owned(),
        }
    }

    /// The inversion this change is about. The old assertion here was
    /// `... == Ok(SendOutcome::UnsupportedEgress)`, and it was the defect
    /// written down as a test: a deployment with no mail adapter answered
    /// "fine" to every send. It now fails, naming the channel, so both callers
    /// can tell an operator the truth.
    #[tokio::test]
    async fn the_unsupported_mail_client_fails_rather_than_reporting_an_outcome() {
        let error = UnsupportedMailClient
            .send(&ctx(), &message())
            .await
            .expect_err("a deployment with no SMTP egress cannot send mail");
        assert!(
            matches!(&error, DomainError::UnsupportedEgress { channel } if channel == CHANNEL),
            "expected UnsupportedEgress{{channel: \"email\"}}, got {error:?}"
        );
    }

    /// The port's contract is per-message, not per-adapter-instance: three
    /// *different* messages each get the same honest answer rather than the
    /// first "using up" some hidden one-shot state. Distinct subjects, not
    /// three calls with `message()`'s fixture repeated verbatim -- a fixed
    /// adapter could theoretically special-case one payload and this test
    /// would not notice.
    #[tokio::test]
    async fn every_message_is_refused() {
        let client = UnsupportedMailClient;
        for subject in [
            "first notification",
            "second notification",
            "third notification",
        ] {
            let error = client
                .send(
                    &ctx(),
                    &MailMessage {
                        subject: subject.to_owned(),
                        ..message()
                    },
                )
                .await
                .expect_err("every message is refused");
            assert!(matches!(error, DomainError::UnsupportedEgress { .. }));
        }
    }

    /// A message carrying credentials is refused the same way — this adapter
    /// never reaches a credential store, so it cannot fail for any other
    /// reason, and an operator who *has* filled the settings in must still be
    /// told the deployment cannot send.
    #[tokio::test]
    async fn a_fully_configured_message_is_refused_too() {
        let error = UnsupportedMailClient
            .send(
                &ctx(),
                &MailMessage {
                    credentials: Some(crate::domain::ports::MailCredentials {
                        username: "qa@example.com".to_owned(),
                        password_credstore_ref: "qa-smtp".to_owned(),
                    }),
                    ..message()
                },
            )
            .await
            .expect_err("configuration on the tenant's side does not make egress exist");
        assert!(matches!(error, DomainError::UnsupportedEgress { .. }));
    }
}
