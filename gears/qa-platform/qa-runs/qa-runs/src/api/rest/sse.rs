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
//!    read side, and, **as of Task 15, one adapter now pays its own way**:
//!    `argo::watch::handle_line` truncates with [`sanitize_line_for_archive`]
//!    before a line ever reaches the broadcaster, so *that* adapter's resident
//!    buffer no longer scales with an unbounded line. Nothing in this crate
//!    enforces the same of a *different* adapter (`MockRunExecutor` does not
//!    truncate; a future HTTP-push producer `fan_out_log`'s own doc
//!    anticipates would not either) — the obligation `infra::logs::broadcast`
//!    records is still real for any adapter that has not taken it up.
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

/// The other literal half of the marker [`sanitize_line`] appends, around
/// the dropped-byte count. Named so [`TRUNCATION_MARKER_MAX`] can size
/// itself off the same text `sanitize_line` builds from, rather than a
/// second copy of it.
const TRUNCATION_SUFFIX: &str = " bytes dropped; see the archived log]";

/// How many decimal digits a `usize` can ever need, on this target.
/// `usize::MAX.ilog10()` is the index of its highest digit, so `+ 1` is the
/// digit count — computed rather than written as a literal `20`, so a build
/// for a pointer width other than 64 bits gets its own correct bound instead
/// of silently inheriting this one.
const MAX_USIZE_DIGITS: usize = usize::MAX.ilog10() as usize + 1;

/// Upper bound on the total length [`sanitize_line`] appends when it
/// truncates a line, in bytes: [`TRUNCATION_PREFIX`], the dropped-byte count
/// at its widest possible decimal rendering, and [`TRUNCATION_SUFFIX`].
///
/// **Not "about seventy bytes"** (this module's own estimate, stated where
/// [`MAX_LINE_BYTES`] is defined, for a reader who wants a feel for the
/// number rather than a proof). `dropped` is `flattened.len() - cut`, and
/// `flattened.len()` is bounded only by how large a line a caller hands this
/// function — this crate does not itself cap that before `sanitize_line`
/// sees it, so the honest bound on the count's digit count is a `usize`'s
/// widest rendering, [`MAX_USIZE_DIGITS`], not the two or three digits a
/// typical over-long line would produce. A test that wants a real ceiling on
/// the emitted length needs this, not the smaller number that happens to
/// hold for every case anyone has tried.
pub const TRUNCATION_MARKER_MAX: usize =
    TRUNCATION_PREFIX.len() + MAX_USIZE_DIGITS + TRUNCATION_SUFFIX.len();

/// Upper bound this crate assumes for `"[{node}] "` — the prefix
/// `IngestService::fan_out_log` wraps every archived line in — so
/// [`WRITE_SIDE_MAX_LINE_BYTES`] can reserve room for it without a
/// write-side caller ever telling this module its actual `node.len()`.
///
/// **Not an invariant.** `ExecutionNode::name` is a plain `String`,
/// deliberately not normalised to a DNS-1123 label (`fan_out_log`'s own
/// doc), so nothing here guarantees a name never exceeds this. It is,
/// today, generous by roughly 6x: the one production source is
/// `format!("repo-{repo_id}")` over a UUID, ~41 bytes once wrapped —
/// `LogResume::from_archived_text` already carries the matching residual
/// for a node name containing `']'`, on the same unreachable-today,
/// not-unsound-if-it-happened terms. If a future producer ever grows node
/// names past this, the failure this constant exists to prevent
/// reappears: a write-side truncation marker gets re-cut on read,
/// reporting a dropped-byte count two orders of magnitude short of the
/// truth — not a panic, not data loss, a misdiagnosis.
const ASSUMED_ARCHIVE_PREFIX_BYTES: usize = 256;

/// The cap a **write-side** caller must truncate to before its output is
/// wrapped in an archive prefix and read back through this module's own
/// [`sanitize_line`] — so the wrapped line still fits under
/// [`MAX_LINE_BYTES`] and [`sanitize_line`] never re-truncates it.
///
/// # Why this has to exist at all
///
/// `argo::watch::handle_line` truncates a line before it reaches the sink
/// (review finding #30), so the archive holds that line already at a cap,
/// plus a marker naming the true dropped-byte count.
/// `IngestService::fan_out_log` then wraps it as `"[{node}] {line}"` and
/// archives *that* — and every reader, live SSE and archive replay alike,
/// gets the wrapped string back through [`sanitize_line`] again
/// (`api::rest::handlers::runs::sse_event`, `lines_as_events`). A
/// write-side line truncated to plain [`MAX_LINE_BYTES`] is, once wrapped,
/// always over the cap, so the read side re-truncates it — discarding the
/// write side's own marker (whose count is correct) for a fresh one that
/// only counts what the *second* cut dropped. [`WRITE_SIDE_MAX_LINE_BYTES`]
/// is what stops that: reserve the prefix's assumed width and this
/// module's own [`TRUNCATION_MARKER_MAX`] up front, so the wrapped result
/// never crosses [`MAX_LINE_BYTES`] in the first place.
///
/// # A budget, not a text-matching detector
///
/// The alternative — have [`sanitize_line`] recognise its own marker and
/// leave an already-marked line alone — would need to parse the marker's
/// text back out, a *third* copy of its format (append it here, parse it
/// there) for exactly the kind of drift this crate keeps re-discovering
/// (see [`TRUNCATION_MARKER_MAX`]'s doc, and `LogPosition`'s
/// `flatten_log_char`). A budget avoids that: the write side simply never
/// produces a line long enough to need a second cut, so there is nothing
/// for the read side to detect.
///
/// # A fixed number, not `MAX_LINE_BYTES - node.len()`
///
/// `LineSkip::consume`'s anchor is derived from this exact write-side
/// output, so if the truncation point depended on the runtime length of
/// `node`, the archived text for a byte-identical over-long line would
/// differ depending on which node emitted it — a coupling between "how
/// long is my own name" and "where does my content get cut" with no
/// purpose behind it. Reserving a fixed [`ASSUMED_ARCHIVE_PREFIX_BYTES`]
/// instead keeps the truncation point a function of the line and the cap
/// alone, matching every other truncation decision this module makes.
pub const WRITE_SIDE_MAX_LINE_BYTES: usize =
    MAX_LINE_BYTES - ASSUMED_ARCHIVE_PREFIX_BYTES - TRUNCATION_MARKER_MAX;

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
    truncate(flatten(line), MAX_LINE_BYTES)
}

/// [`sanitize_line`] for a **write-side** caller whose output will be
/// wrapped in more text (`IngestService::fan_out_log`'s archive prefix)
/// before anything reads it — truncates to [`WRITE_SIDE_MAX_LINE_BYTES`]
/// rather than [`MAX_LINE_BYTES`], so the wrapped result never crosses this
/// module's own cap and gets re-truncated on read. See
/// [`WRITE_SIDE_MAX_LINE_BYTES`]'s doc for why this needs its own budget
/// rather than a second call to [`sanitize_line`].
///
/// Same flattening, same character-boundary handling, same marker — one
/// function ([`truncate`]) parametrized by the cap, not a second copy of
/// the rule.
#[must_use]
pub fn sanitize_line_for_archive(line: &str) -> String {
    truncate(flatten(line), WRITE_SIDE_MAX_LINE_BYTES)
}

/// Every `\r` and `\n` becomes a space. Replaced rather than dropped so that
/// `a\nb` reads as `a b` and not as `ab` - joining two words that were never
/// adjacent misreports the runner's output. Shared by [`sanitize_line`] and
/// [`sanitize_line_for_archive`] - see [`truncate`]'s doc for why capping is
/// split out the same way.
fn flatten(line: &str) -> String {
    line.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

/// Truncate already-flattened text to `cap` bytes, on a character boundary,
/// appending a marker naming how many bytes were dropped.
///
/// Parametrized by `cap` rather than hard-coding [`MAX_LINE_BYTES`] so
/// [`sanitize_line`] (the read side) and [`sanitize_line_for_archive`] (a
/// write side that must leave room for text wrapped around its result
/// later) share one truncation rule instead of maintaining two copies of
/// it under different names.
fn truncate(flattened: String, cap: usize) -> String {
    if flattened.len() <= cap {
        return flattened;
    }

    // Back off to the nearest character boundary at or below the cap, so the
    // truncation never splits a multi-byte character.
    let mut cut = cap;
    while cut > 0 && !flattened.is_char_boundary(cut) {
        cut -= 1;
    }
    let dropped = flattened.len() - cut;
    let mut out = flattened[..cut].to_owned();
    out.push_str(TRUNCATION_PREFIX);
    out.push_str(&dropped.to_string());
    out.push_str(TRUNCATION_SUFFIX);
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
    use super::{
        MAX_LINE_BYTES, TRUNCATION_MARKER_MAX, WRITE_SIDE_MAX_LINE_BYTES, log_event, sanitize_line,
        sanitize_line_for_archive,
    };

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

    /// [`TRUNCATION_MARKER_MAX`] must actually bound the marker, not merely
    /// the typical case — this is what a caller outside this cap (`argo::watch`,
    /// truncating before the broadcaster) relies on to size its own assertion.
    /// A line far larger than `MAX_LINE_BYTES` pushes `dropped` into more
    /// digits than a hand-picked estimate would have covered.
    #[test]
    fn the_marker_bound_holds_for_a_line_far_larger_than_the_cap() {
        let huge = "q".repeat(MAX_LINE_BYTES * 50);
        let safe = sanitize_line(&huge);
        assert!(
            safe.len() <= MAX_LINE_BYTES + TRUNCATION_MARKER_MAX,
            "marker overran its declared bound: {} bytes",
            safe.len()
        );
    }

    /// **The end-to-end property Task 15's fix round 1 exists to guarantee.**
    ///
    /// `argo::watch::handle_line` truncates a ~2 MB line with
    /// [`sanitize_line_for_archive`] before it reaches the broadcaster.
    /// `IngestService::fan_out_log` then wraps that output as
    /// `"[{node}] {line}"` and archives it -- and every reader gets the
    /// wrapped string back through this module's [`sanitize_line`]. If the
    /// write side had used plain [`sanitize_line`] instead (its output
    /// already at `MAX_LINE_BYTES`), wrapping it in a real node prefix pushes
    /// the total over the cap, and the read side would re-truncate,
    /// discarding the true ~2 MB drop count for a tiny one left over from
    /// the second cut. This test fails under that mistake and passes under
    /// the shipped design.
    #[test]
    fn a_write_side_truncated_line_survives_the_read_side_without_a_second_cut() {
        // `ExecutionNode::name`'s one production source,
        // `format!("repo-{repo_id}")` over a UUID.
        let node = "repo-3f9c2b6e-1a2d-4e5f-9a8b-7c6d5e4f3a2b";
        let huge = "z".repeat(MAX_LINE_BYTES * 250); // ~2 MB, ASCII throughout
        let write_side = sanitize_line_for_archive(&huge);
        // `IngestService::fan_out_log`'s own construction -- see
        // `WRITE_SIDE_MAX_LINE_BYTES`'s doc, which names this exact format.
        let archived = format!("[{node}] {write_side}");

        let read_side = sanitize_line(&archived);
        assert_eq!(
            read_side, archived,
            "the read side must not re-truncate a line the write side already capped"
        );

        let true_dropped = huge.len() - WRITE_SIDE_MAX_LINE_BYTES;
        assert!(
            write_side.contains(&true_dropped.to_string()),
            "the marker must report the true ~2 MB drop, not a re-truncation's much \
             smaller one: {write_side}"
        );
    }
}
