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
//!    byte cap belongs to whoever writes the adapter. This is that cap for the
//!    read side; it does **not** shrink the broadcaster's resident buffer, which
//!    is still `capacity x` the longest line the adapter accepts.
//! 3. **A stream ends.** See [`MAX_STREAM_DURATION`].
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

use crate::api::rest::dto::RunLogLineDto;

/// Longest run of **runner output** this endpoint will emit in one line, in
/// bytes. The line actually emitted can exceed it by the truncation marker,
/// which is this gear's own text and about seventy bytes.
///
/// A judgement, not a ported constant - the source system polls a fixed-size
/// log per client rather than streaming and has no equivalent number. 8 KiB is
/// far above any plausible test-runner line (a stack frame, a Robot Framework
/// keyword trace) and far below a size that makes one line expensive to hold.
///
/// Truncation is **visible**: [`sanitize_line`] appends a marker naming how many
/// bytes were dropped. `infra::logs::broadcast` declined to cap here for the
/// reason that *"truncating an operator's log line silently is the same class of
/// harm as truncating a status, and this module has no way to say 'the rest of
/// this line is in the archived log'"*. This layer can say it, which is what
/// makes the cap acceptable here and not there.
pub const MAX_LINE_BYTES: usize = 8 * 1024;

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

/// Marker appended to a line this endpoint truncated.
///
/// ASCII only - `clippy::non_ascii_literal` is denied workspace-wide - and
/// prefixed the same way `infra::logs::gap_marker` is.
///
/// **The prefix is a convention, not a guarantee.** Log lines are the execution
/// plane's bytes verbatim, so a runner printing this exact text is
/// indistinguishable from a real truncation. Telling them apart would need a
/// separate SSE event type, which the payload shape leaves room for and this
/// gear has not needed.
const TRUNCATION_PREFIX: &str = " [qa-runs: line truncated, ";

/// Make one log line safe to frame as a single SSE event.
///
/// Two transformations, in this order. The second is the one only this layer
/// provides - see the module header on why the first is defence-in-depth.
///
/// * **Every `\r` and `\n` becomes a space.** Replaced rather than dropped so
///   that `a\nb` reads as `a b` and not as `ab` - joining two words that were
///   never adjacent misreports the runner's output, which is the thing this
///   endpoint exists to show faithfully. Replacement also means the byte count
///   below is not changed by the substitution.
/// * **The result is truncated to [`MAX_LINE_BYTES`]**, on a character
///   boundary, with a marker naming how many bytes went missing.
///
/// # Why the newline handling is not "escaping"
///
/// An escape (`\n` as the two characters backslash-n) would round-trip, but it
/// would also mean every consumer has to unescape to render, and a consumer
/// that forgot would show backslashes through the whole log. The frame safety
/// is the requirement; exact reproduction of control characters is not, and the
/// archived log is the artefact that keeps the original bytes.
///
/// # What this does not do
///
/// It does not strip ANSI escapes, terminal control sequences, or anything else
/// a browser might interpret. Those are the renderer's problem: they cannot
/// forge an SSE frame, which is the boundary this function defends.
#[must_use]
pub fn sanitize_line(line: &str) -> String {
    let flattened: String = line
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();

    if flattened.len() <= MAX_LINE_BYTES {
        return flattened;
    }

    // Back off to the nearest character boundary at or below the cap, so the
    // truncation never splits a multi-byte character.
    let mut cut = MAX_LINE_BYTES;
    while cut > 0 && !flattened.is_char_boundary(cut) {
        cut -= 1;
    }
    let dropped = flattened.len() - cut;
    let mut out = flattened[..cut].to_owned();
    out.push_str(TRUNCATION_PREFIX);
    out.push_str(&dropped.to_string());
    out.push_str(" bytes dropped; see the archived log]");
    out
}

/// Sanitize a line and wrap it as the endpoint's event payload.
#[must_use]
pub fn log_event(line: &str) -> RunLogLineDto {
    RunLogLineDto {
        line: sanitize_line(line),
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_LINE_BYTES, log_event, sanitize_line};

    /// The forgery this module exists to prevent. A payload carrying a blank
    /// line followed by `event:`/`data:` is two SSE events once framed, and the
    /// second is entirely the execution plane's to compose - it could claim any
    /// event type the client handles.
    #[test]
    fn a_line_cannot_carry_a_second_sse_frame() {
        let hostile = "starting\n\nevent: run_finished\ndata: {\"state\":\"succeeded\"}\n\n";
        let safe = sanitize_line(hostile);
        assert!(!safe.contains('\n'), "no newline may survive: {safe:?}");
        assert!(
            !safe.contains('\r'),
            "no carriage return may survive: {safe:?}"
        );
        // The text is still there - this is framing safety, not censorship.
        assert!(safe.contains("run_finished"));
    }

    /// A lone `\r` is enough on its own: SSE accepts CR, LF and CRLF as line
    /// terminators, so a sanitizer that only handled `\n` would leave the
    /// forgery open on a Windows-style runner.
    #[test]
    fn a_carriage_return_alone_is_neutralised() {
        assert_eq!(sanitize_line("a\rb"), "a b");
        assert_eq!(sanitize_line("a\r\nb"), "a  b");
    }

    /// Replaced with a space rather than deleted, so two words that were on
    /// separate lines do not become one word that the runner never printed.
    #[test]
    fn a_newline_becomes_a_space_rather_than_disappearing() {
        assert_eq!(sanitize_line("first\nsecond"), "first second");
    }

    #[test]
    fn a_short_line_is_untouched() {
        assert_eq!(sanitize_line("PASS | smoke.robot"), "PASS | smoke.robot");
    }

    /// Truncation is visible and quantified. A silent cut would leave an
    /// operator reading a sentence that stops mid-word with nothing saying why.
    #[test]
    fn an_over_long_line_is_truncated_and_says_so() {
        let long = "x".repeat(MAX_LINE_BYTES * 2);
        let safe = sanitize_line(&long);
        assert!(safe.starts_with(&"x".repeat(MAX_LINE_BYTES)));
        assert!(
            safe.contains("line truncated"),
            "{}",
            &safe[safe.len() - 80..]
        );
        assert!(
            safe.contains(&MAX_LINE_BYTES.to_string()),
            "the marker must name how many bytes went missing"
        );
    }

    /// The boundary: exactly at the cap is not truncated, one byte over is.
    #[test]
    fn the_truncation_boundary_is_the_cap_itself() {
        let exact = "y".repeat(MAX_LINE_BYTES);
        assert_eq!(sanitize_line(&exact), exact);
        let over = "y".repeat(MAX_LINE_BYTES + 1);
        assert!(sanitize_line(&over).contains("line truncated"));
    }

    /// A cut that landed inside a multi-byte character would produce invalid
    /// UTF-8 and panic on the slice; the backoff is what prevents it. The
    /// fixture puts a three-byte character straddling the cap.
    #[test]
    fn truncation_never_splits_a_multibyte_character() {
        let mut line = "z".repeat(MAX_LINE_BYTES - 1);
        line.push('\u{20AC}'); // EUR sign, three bytes: 8191 + 3 = 8194
        line.push_str("tail");
        let safe = sanitize_line(&line);
        assert!(safe.contains("line truncated"));
        // Reaching here at all is the assertion: an unaligned cut panics.
        assert!(safe.is_char_boundary(0));
    }

    #[test]
    fn the_event_payload_carries_the_sanitized_line() {
        assert_eq!(log_event("a\nb").line, "a b");
    }
}
