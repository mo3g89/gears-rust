//! The notification routing core: config, event and per-schedule settings in,
//! a routing decision out.
//!
//! [`routing`] — Task 36. Pure: no client, no repository, no `async`. It
//! answers exactly one question — *should this event send, and over which
//! channel(s)* — and nothing about *how* (Slack Block Kit rendering, SMTP
//! delivery, the scheduled-run template text) or *whether it already did*
//! (send-once dedupe against `qa_run_notifications` is
//! [`crate::domain::repos::notify_repo::NotifyRepository::claim_notification`],
//! a database call this module never makes). Tasks 37-40 build the pieces that
//! read this module's [`routing::Decision`] and act on it.
//!
//! [`render`] — Task 37. Also pure: turns a routed event into the Slack Block
//! Kit message or email text that actually gets sent. See its own module doc
//! for the full legacy citation set.

pub mod render;
pub mod routing;
