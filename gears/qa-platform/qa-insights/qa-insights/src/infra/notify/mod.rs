//! The two outbound notification adapters — Task 39.
//!
//! [`slack_oagw::SlackOagwClient`] implements
//! [`SlackClient`](crate::domain::ports::SlackClient) over `oagw`;
//! [`mail_unsupported::UnsupportedMailClient`] implements
//! [`MailClient`](crate::domain::ports::MailClient) as D10's permanent inert
//! answer. Both files carry their own reasoning in full — `slack_oagw`'s
//! header in particular records two findings about how far an oagw-backed
//! Slack adapter can currently reach, not just the "no `reqwest`" rule every
//! egress adapter in this crate states.
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
pub mod mail_unsupported;
pub mod slack_oagw;

pub use mail_unsupported::UnsupportedMailClient;
pub use slack_oagw::SlackOagwClient;
