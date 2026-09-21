//! The outbound email port — Task 38 fixed the shape (R102); Task 39 supplied
//! the one production implementation, and **the SMTP follow-up reversed the
//! decision that made that implementation inert**.
//!
//! # D10 is closed, and this header is what it was replaced by
//!
//! D10 said: config, routing and dedupe ship for email exactly as they do for
//! Slack, and only the socket is deferred. The cost of leaving it there was
//! measured rather than guessed — an operator fills in the SMTP columns, sends
//! a test, reads a success back, and receives nothing — and owner decision 1
//! settled it as *implement it fully*. The socket now exists:
//! [`crate::infra::notify::mail_smtp::SmtpMailClient`] is a real `lettre`
//! transport, and ADR-0011 (`cpt-cf-qa-adr-smtp-egress`) is the architectural
//! half of the decision, because SMTP cannot traverse `oagw` and so this is the
//! gear's first direct, non-gateway TCP egress.
//!
//! [`crate::infra::notify::mail_unsupported::UnsupportedMailClient`] did not go
//! away with D10: it is the explicitly-bound answer for a deployment whose
//! operator has not enabled SMTP egress at all
//! (`crate::config::QaInsightsConfig::smtp_allowed_hosts` empty). What changed
//! is what it *answers* — see the next section.
//!
//! # `send` may now error, and [`SendOutcome::UnsupportedEgress`] is gone
//!
//! This module used to state, as a property of the *port*, that `send` never
//! returns `Err`, and the inert adapter reported
//! `Ok(SendOutcome::UnsupportedEgress)` — a **value** the caller logged as the
//! audit string `"unsupported_egress"` rather than an error it had to
//! special-case.
//!
//! That was the right shape for a port whose only adapter could not fail. It is
//! the wrong shape now, and not only because a real transport has failures: the
//! "value, not an error" design is exactly what let a deployment with no mail
//! adapter write a *non-failure* audit row for a notification nobody received.
//! [`MailClient::send`] now answers `Ok(SendOutcome::Sent)` or an `Err`, and
//! `UnsupportedMailClient` takes the `Err` arm with
//! [`DomainError::UnsupportedEgress`]. An operator reading the log sees a
//! failed send, which is what happened.
//!
//! `SendOutcome` survives, with one variant, because
//! [`crate::domain::ports::slack_client::SlackClient`] shares the type and the
//! audit-log fold reads both ports through it.
//!
//! # One method, and it takes a `ctx` — review finding #37, and now load-bearing
//!
//! [`MailClient::send`] takes `ctx: &SecurityContext`, the same as
//! [`crate::domain::ports::slack_client::SlackClient::send`] (which gained it
//! in fix round 1, ruling R108). **This module used to argue the opposite**,
//! and the argument is kept here because the finding that overruled it turned
//! out to be right for a reason it only predicted:
//!
//! > D10's inert adapter never reaches a network, so there is no per-tenant
//! > resolution for a context to drive, and a future SMTP-backed adapter would
//! > authenticate to a relay from [`MailMessage`]'s own `smtp_host`/`smtp_port`
//! > rather than from an identity — therefore the parameter would be unused
//! > surface, which [`crate::domain::ports`]' header warns against.
//!
//! Finding #37 answered that these are two egress ports of one notification
//! service, called from the same two methods, under the same tenant's
//! authority, and that *"currently unused"* is a property of today's one
//! adapter rather than of the port — *"the moment any deployment ships a real
//! relay, 'which tenant is this email for' is the first question an adapter has
//! to answer, for tenant-scoped relay credentials and for the audit trail, and
//! a port that never carried the identity would have to change shape to answer
//! it."*
//!
//! That is now literally what the parameter does.
//! [`SmtpMailClient`](crate::infra::notify::mail_smtp::SmtpMailClient) forwards
//! this `ctx` to `credstore_sdk::CredStoreClientV1::get`, whose resolution is
//! tenant-scoped; without it the adapter could only read the nil tenant's
//! secrets. Had the port shipped without the parameter, adding SMTP would have
//! meant changing the trait, both call sites and every test double, which is
//! the cost the finding was about.

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::ports::SendOutcome;

/// One outbound email, resolved from the tenant's stored SMTP settings but not
/// yet sent.
///
/// Field-for-field what legacy's `send_email` reads off `NotificationsConfig`
/// (`notifications.rs:133-177`) plus the two values that method takes as
/// arguments (`subject`, `body`) rather than from config — and two this gear
/// has that legacy does not, because legacy's relay took unauthenticated
/// submission and this one does not have to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailMessage {
    pub smtp_host: String,
    /// The relay's port, and **the TLS selector**: 465 means the adapter wraps
    /// the socket in TLS before the first byte (RFC 8314 §3.3), every other
    /// port means it dials in the clear and requires `STARTTLS`. See
    /// [`crate::infra::notify::mail_smtp`] for why the rule lives on the port
    /// number rather than on a separate field a tenant could set to something
    /// that contradicts it.
    pub smtp_port: u16,
    /// The SMTP AUTH identity, or `None` for a relay that takes unauthenticated
    /// submission.
    ///
    /// Both halves or neither: [`Credentials`](MailCredentials) carries the
    /// username beside the credential-store *reference*, so a message can never
    /// name an account without saying how to prove it.
    pub credentials: Option<MailCredentials>,
    pub from: String,
    /// Comma-separated, stored and forwarded exactly as
    /// [`qa_insights_sdk::NotificationConfig::email_recipients`] holds it —
    /// the split into individual mailboxes is the adapter's job, matching
    /// legacy's own `split(',')` (`notifications.rs:144-153`).
    pub recipients: String,
    pub subject: String,
    pub body: String,
}

/// What the adapter needs to authenticate to the relay: a name, and where to
/// fetch the proof.
///
/// **The password is not in here and never crosses this port.** The reference
/// is resolved by the adapter, at send time, through
/// `credstore_sdk::CredStoreClientV1` under the sending tenant's own
/// `SecurityContext` — which is the whole reason [`MailClient::send`] takes
/// one. A struct with a `password: String` field would put plaintext into a
/// value the domain layer builds, clones and (per `#[derive(Debug)]`) can
/// format, which ADR-0008 (`cpt-cf-qa-adr-credential-containment`) forbids
/// outright.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailCredentials {
    pub username: String,
    /// A `credstore` secret reference, validated by
    /// [`validate_credstore_ref`](crate::domain::ports::validate_credstore_ref)
    /// on the write path before it can be stored.
    pub password_credstore_ref: String,
}

/// The outbound email egress.
///
/// # Errors
///
/// [`DomainError::Internal`] for a transport failure — an unreachable relay, a
/// TLS failure, an authentication rejection, or a send that did not finish
/// inside [`SmtpMailClient::SEND_TIMEOUT`](
/// crate::infra::notify::mail_smtp::SmtpMailClient::SEND_TIMEOUT).
/// [`DomainError::Validation`] for an address `lettre` refuses to parse,
/// matching legacy's own two failure shapes (`notifications.rs:139-176`).
/// [`DomainError::UnsupportedEgress`] from
/// [`UnsupportedMailClient`](crate::infra::notify::mail_unsupported::UnsupportedMailClient)
/// when this deployment has not enabled SMTP egress at all.
#[async_trait]
pub trait MailClient: Send + Sync {
    /// Send `message` as `ctx`'s subject. `ctx` is the sending tenant's
    /// identity and the authority the relay password is resolved under — see
    /// this module's header, "One method, and it takes a `ctx`".
    /// `Ok(SendOutcome::Sent)` on success.
    async fn send(
        &self,
        ctx: &SecurityContext,
        message: &MailMessage,
    ) -> Result<SendOutcome, DomainError>;
}
