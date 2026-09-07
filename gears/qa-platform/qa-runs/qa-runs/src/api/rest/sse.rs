//! Framing for the live-log SSE endpoint.
//!
//! Everything here exists because a log line is **another system's bytes**. The
//! executor adapter hands `ExecutionEvent::Log { line }` to
//! `infra::logs::RunLogBroadcaster`, which fans it out verbatim, and this module
//! is the last thing between that string and an operator's browser.
//!
//! # The three properties this module owns
//!
//! 1. **A line cannot forge a frame.** SSE delimits events with newlines: a
//!    payload containing `\n\nevent: alert\ndata: ...\n\n` becomes *two* events
//!    on the wire, the second one entirely under the execution plane's control.
//!    [`sanitize_line`] is what makes a line one line.
//! 2. **A line cannot be arbitrarily long.** `DEFAULT_LOG_CHANNEL_CAPACITY`
//!    bounds a line *count*, not bytes - `infra::logs::broadcast` records that a
//!    runner emitting a 2 MB line holds 512 MB per subscribed run, and that the
//!    byte cap belongs to whoever writes the adapter.
//!    [`MAX_LINE_BYTES`] is that cap for the read side, and, **as of Task 15,
//!    one adapter now pays its own way**: `argo::watch::handle_line` truncates
//!    with `domain::repos::sanitize_line_for_archive` before a line ever
//!    reaches the broadcaster, so *that* adapter's resident buffer no longer
//!    scales with an unbounded line. Nothing in this crate enforces the same of
//!    a *different* adapter (`MockRunExecutor` does not truncate; a future
//!    HTTP-push producer `fan_out_log`'s own doc anticipates would not either)
//!    — the obligation `infra::logs::broadcast` records is still real for any
//!    adapter that has not taken it up.
//! 3. **A stream ends.** See [`MAX_STREAM_DURATION`].
//!
//! # Where the first two properties are implemented
//!
//! In [`domain::repos::log_line`](crate::domain::repos), since Task 21, and
//! re-exported here: the write-side cap that
//! `argo::watch::handle_line` truncates to is derived from
//! [`MAX_LINE_BYTES`] and decides what the archive holds, so a domain module
//! and an infra module both needed these and were importing them out of the
//! transport layer to get them (review findings #15, #16, #39). This module's
//! `MAX_LINE_BYTES` paragraph had named that complaint and deferred it.
//!
//! What is still declared here is the framing itself: [`log_event`], which
//! builds the endpoint's payload, and the two connection constants below.
//!
//! # What this endpoint will not be able to do, once a real elector is deployed
//!
//! `infra::logs::RunLogBroadcaster` is a **per-process** channel map, and since
//! Task 16c the only publisher into it is the observer task
//! `domain::service::watch` starts from the dispatcher tick.
//!
//! **Today every replica is the dispatcher**, because `NoopLeaderElector` is the
//! only elector this crate ships and its `run_role` runs the work
//! unconditionally — so each replica publishes into the broadcaster its own
//! router subscribes to, and this endpoint works wherever the load balancer
//! sends the request.
//!
//! **Under a real elector it would not.** Only the leader would tick, so only
//! the leader's map would receive lines, and a subscriber landing on any other
//! replica would get a **200 and silence** — not an error, not an
//! empty-stream signal, just a connection that stays open until
//! [`MAX_STREAM_DURATION`]. Nothing in this module could detect it or report it,
//! which is why it is written here rather than discovered: the failure would be
//! indistinguishable from a run that has produced no output yet.
//!
//! Stated in the conditional deliberately — the first draft stated it in the
//! present tense, which describes a deployment that does not exist. See
//! `infra::logs::broadcast`, which owns the property.

use std::time::Duration;

// The sanitizer and the read-side cap live in `domain::repos`, not here -- the
// write-side cap derived from `MAX_LINE_BYTES` decides what the archive holds,
// and `domain::service::ingest` and `infra::executor::argo::watch` must not
// import the transport layer to reach it (review findings #15, #16, #39).
// Re-exported so a handler's `use crate::api::rest::sse::{..}` is unchanged and
// this module stays the one place the endpoint's framing is imported from.
pub use crate::domain::repos::{MAX_LINE_BYTES, sanitize_line};

use crate::api::rest::dto::RunLogLineDto;

/// How long one live-log connection may stay open.
///
/// # What this bounds and what it does not
///
/// A subscription ends on its own only when the run's channel is reaped, which
/// `service::ingest` and `service::runs` do when the run reaches a terminal
/// state. A run that never reaches one - wedged mid-execution with a dead
/// executor - therefore holds its connection, and one map entry, for as long as
/// the client keeps it open. This constant is what makes "as long as" finite.
///
/// Eight hours is `cpt-cf-qa-nfr-run-duration`'s longest contemplated run, so a
/// legitimate stream is not cut short by a run merely being long. A run that
/// outlives it has its stream closed and the client must reconnect; that is the
/// accepted cost, and it is a cost, not a non-event.
///
/// **This is a ceiling, not the mechanism.** The pre-stream terminal check in
/// the handler is what stops the common case - opening the log page of a
/// finished run - from minting a channel entry at all. Without that check this
/// constant would be the only thing bounding an entry per such request.
pub const MAX_STREAM_DURATION: Duration = Duration::from_hours(8);

/// Interval between SSE keep-alive comments.
///
/// Intermediaries close idle connections, and a run can be silent for minutes
/// between phases without being stuck. Mirrors mini-chat's 30 s
/// (`gears/mini-chat/mini-chat/src/api/rest/handlers/messages.rs`, the
/// `KeepAlive::new().interval(..)` on its `Sse::new(relay)`).
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Sanitize a line and wrap it as the endpoint's event payload.
#[must_use]
pub fn log_event(line: &str) -> RunLogLineDto {
    RunLogLineDto {
        line: sanitize_line(line),
    }
}

#[cfg(test)]
mod tests {
    use super::log_event;

    /// The payload shape, which is this module's own. Every other test that
    /// once lived here moved to `domain::repos::log_line_tests` with the caps
    /// and the sanitizers it pins -- Task 21.
    #[test]
    fn the_event_payload_carries_the_sanitized_line() {
        assert_eq!(log_event("a\nb").line, "a b");
    }
}
