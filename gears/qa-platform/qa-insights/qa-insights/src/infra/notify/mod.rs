//! The outbound notification adapters — Task 39, plus the SMTP follow-up.
//!
//! [`slack_oagw::SlackOagwClient`] implements
//! [`SlackClient`](crate::domain::ports::SlackClient) over `oagw`.
//! [`MailClient`](crate::domain::ports::MailClient) has **two**:
//! [`mail_smtp::SmtpMailClient`], a real `lettre` transport, and
//! [`mail_unsupported::UnsupportedMailClient`], the explicitly-bound answer for
//! a deployment that has not configured SMTP egress — `gear::init` chooses
//! between them on `QaInsightsConfig::smtp_allowed_hosts`. Each file carries
//! its own reasoning in full: `slack_oagw`'s header records how a Slack send is
//! delivered — the tenant's webhook URL resolved through credstore, checked by
//! [`slack_webhook`], proxied through a per-tenant oagw upstream — and why no
//! error it returns carries the webhook path; `mail_smtp`'s records why it is
//! the one adapter in this crate that does **not** go through `oagw` at all
//! (ADR-0011: `oagw` speaks HTTP and SMTP is not HTTP).
//!
//! Neither was wired into [`crate::gear::QaInsights`] by the task that built
//! them — the adapters were built first and bound later. **Task 40 bound
//! both**, replacing `gear.rs`'s two inert stand-ins (`NeverWiredSlackClient`,
//! `NeverWiredMailClient`), which no longer exist.
//!
//! Binding [`SlackOagwClient`] did not, at first, mean Slack notifications were
//! delivered ("Finding B"): it sent the credstore reference as a
//! placeholder path. The third review pass closed that; [`slack_oagw`]'s
//! "Delivery" section carries the design.

pub mod block_kit;
pub mod mail_smtp;
pub mod mail_unsupported;
pub mod slack_oagw;
mod slack_webhook;
#[cfg(test)]
pub mod test_credstores;

pub use mail_smtp::SmtpMailClient;
pub use mail_unsupported::UnsupportedMailClient;
pub use slack_oagw::SlackOagwClient;

/// The second half of every "this secret cannot be read" refusal a
/// notification adapter returns. credstore answers a missing
/// secret, a secret another subject owns privately and a denied read with the
/// same `Ok(None)`, so the refusal cannot say which happened; it names the one
/// an operator cannot see from the secret list.
pub const UNREADABLE_SECRET_HINT: &str = "notifications read this secret as the qa-insights system actor, not as the user who \
     stored it, so a secret with `private` sharing (readable by its owner only) is never found: \
     store it with `tenant` sharing, or `shared` from a parent tenant";
