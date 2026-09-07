//! The caps in [`super`], pinned.
//!
//! Moved here from `api::rest::sse`'s inline `mod tests` by Task 21, with the
//! constants they pin — a test that pins a value and a value that lives
//! somewhere else is how a cap gets changed without anything failing. The one
//! test left behind is `the_event_payload_carries_the_sanitized_line`, which
//! is about `RunLogLineDto` rather than about a cap.
//!
//! **`MAX_LINE_BYTES` is pinned by behaviour, not by a literal.** No test here
//! asserts `8 * 1024`; they assert what a line of `MAX_LINE_BYTES` bytes and a
//! line of `MAX_LINE_BYTES + 1` bytes do, which is the property that matters
//! and the one that survives a deliberate change to the number. What must not
//! change silently is the *value*, and `super`'s own doc is where that is
//! argued.

use super::{
    MAX_LINE_BYTES, TRUNCATION_MARKER_MAX, WRITE_SIDE_MAX_LINE_BYTES, sanitize_line,
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
