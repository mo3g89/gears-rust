//! The outbound notification adapters — Task 39, plus the SMTP follow-up.
//!
//! [`slack_oagw::SlackOagwClient`] implements
//! [`SlackClient`](crate::domain::ports::SlackClient) over `oagw`.
//! [`MailClient`](crate::domain::ports::MailClient) has **two**:
//! [`mail_smtp::SmtpMailClient`], a real `lettre` transport, and
//! [`mail_unsupported::UnsupportedMailClient`], the explicitly-bound answer for
//! a deployment that has not configured SMTP egress — `gear::init` chooses
//! between them on `QaInsightsConfig::smtp_allowed_hosts`. Each file carries
//! its own reasoning in full: `slack_oagw`'s header records two findings about
//! how far an oagw-backed Slack adapter can currently reach, and `mail_smtp`'s
//! records why it is the one adapter in this crate that does **not** go through
//! `oagw` at all (ADR-0011: `oagw` speaks HTTP and SMTP is not HTTP).
//!
//! Neither was wired into [`crate::gear::QaInsights`] by the task that built
//! them — R90: build the adapters, do not bind them. **Task 40 bound both**,
//! replacing `gear.rs`'s two inert stand-ins (`NeverWiredSlackClient`,
//! `NeverWiredMailClient`), which no longer exist.
//!
//! Binding [`SlackOagwClient`] does **not** mean Slack notifications are
//! delivered: R107 is an open release-gate item and
//! [`slack_oagw`]'s "Finding B" carries the whole argument.

pub mod block_kit;
pub mod mail_smtp;
pub mod mail_unsupported;
pub mod slack_oagw;

pub use mail_smtp::SmtpMailClient;
pub use mail_unsupported::UnsupportedMailClient;
pub use slack_oagw::SlackOagwClient;
