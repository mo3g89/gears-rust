//! **One log line, flattened and capped** — the two sanitizers, the three
//! caps they are built from, and the one truncation rule they share.
//!
//! # Why this is a domain module
//!
//! All of it was declared in `api::rest::sse` until Task 21, because the SSE
//! endpoint was the first caller and, for a while, the only one. It is not any
//! more, and by review findings #15, #16 and #39 the arrangement had two
//! importers pointing the wrong way: `domain::service::ingest` reached into
//! `api::rest::sse` for [`ASSUMED_ARCHIVE_PREFIX_BYTES`], and
//! `infra::executor::argo::watch` reached into it for
//! [`sanitize_line_for_archive`], [`MAX_LINE_BYTES`] and
//! [`TRUNCATION_MARKER_MAX`]. Both are below the transport layer;
//! `api::rest::sse`'s own [`MAX_LINE_BYTES`] paragraph named the complaint and
//! deferred it to a later phase, which is this one.
//!
//! `domain::repos` rather than somewhere else in `domain`, because the second
//! role these numbers acquired is a **storage** role:
//! [`WRITE_SIDE_MAX_LINE_BYTES`] is what `argo::watch::handle_line` truncates
//! to *before the line is archived*, so it decides what the archive holds and
//! therefore what every `first_line`/`last_line` resume anchor
//! ([`LogResume`](super::LogResume)) contains. That puts it beside
//! [`flatten_log_char`](super::flatten_log_char), the other rule the archived
//! text obeys — and the two flattening definitions that had drifted apart are
//! now one function calling the other.
//!
//! **No value changed in the move.** [`MAX_LINE_BYTES`] is still `8 * 1024`,
//! [`ASSUMED_ARCHIVE_PREFIX_BYTES`] still `256`, and
//! [`TRUNCATION_MARKER_MAX`] and [`WRITE_SIDE_MAX_LINE_BYTES`] are still
//! derived from those and the marker's own text. Every test that pinned them
//! moved here with them (`log_line_tests.rs`); `api::rest::sse` keeps the one
//! test that is about its own event payload.
//!
//! # What `api::rest::sse` still owns
//!
//! The endpoint's framing: `log_event`, the stream duration and the keep-alive
//! interval, plus a re-export of [`sanitize_line`] and [`MAX_LINE_BYTES`] so a
//! handler's imports are unchanged. The three properties that module's header
//! claims — a line cannot forge a frame, a line cannot be arbitrarily long, a
//! stream ends — are still the endpoint's properties; the first two are now
//! *implemented* here and the third is entirely there.

/// Longest run of **runner output** this gear will keep in one line, in bytes.
/// The line actually emitted can exceed it by the truncation marker, which is
/// this gear's own text and about seventy bytes.
///
/// A judgement, not a ported constant - the source system polls a fixed-size
/// log per client rather than streaming and has no equivalent number. 8 KiB is
/// far above any plausible test-runner line (a stack frame, a Robot Framework
/// keyword trace) and far below a size that makes one line expensive to hold.
///
/// Truncation is **visible**: [`sanitize_line`] appends a marker naming how many
/// bytes were dropped. `infra::logs::broadcast` declined to cap there for the
/// reason that *"truncating an operator's log line silently is the same class of
/// harm as truncating a status, and this module has no way to say 'the rest of
/// this line is in the archived log'"*. The sanitizers here can say it, which is
/// what makes the cap acceptable at this seam and not at that one.
///
/// # It is no longer only a read-side number, and changing it is not only a
/// bandwidth decision
///
/// Whole-branch review I2. Everything above describes what this constant does
/// for the SSE endpoint, and that was the whole of it when it was written. It
/// is now also the input to [`WRITE_SIDE_MAX_LINE_BYTES`], which is what
/// `argo::watch::handle_line` truncates to **before the line is archived** — so
/// this number decides what is written to the database, and therefore what
/// every `first_line`/`last_line` resume anchor ([`super::LogResume`])
/// contains.
///
/// **Lowering it is a one-way migration for the runs already on disk, and it
/// fails quietly.** An anchor archived under the old value holds text longer
/// than a post-deploy re-read of the same line produces, so a node whose
/// archived text was ever truncated cannot match its own anchor again.
///
/// **What that costs depends on where the over-long line sits, and this
/// paragraph used to overstate it.** It read *"`LineSkip`'s first-line guard
/// mismatches for every affected node, suppression is disabled for that node
/// for the rest of the run, and review finding #50's unbounded `CONCAT` growth
/// returns for every live run at the moment of the deploy"*. Traced against
/// `argo::watch::handle_line` — whose own "A residual this change accepts"
/// section had already worked the three positions out, in the file this
/// constant did not then live beside — only the **first** archived line for a
/// node produces that outcome. A **middle** line diverges not at all:
/// suppression between the anchors is purely count-based, so a stale middle
/// line changes what one re-read line's bytes look like and nothing else. The
/// **last** line trips the other guard, which logs an `error!` blaming
/// pre-fix duplicate rows — a false alarm with no data effect. So the real
/// exposure is "every node whose *first* archived line was over-long, on a run
/// re-attached after the deploy", not every live run, and the signal for it is
/// a `debug!` that attributes the mismatch to log rotation, which is the other
/// thing that produces it.
///
/// Raising it is safe in that direction (a longer cap cannot shorten an
/// existing anchor) but widens what a single line may cost the broadcaster and
/// the archive row.
///
/// So: change this for SSE framing reasons alone only when no run is live, or
/// accept one run's worth of duplicated archive text.
///
/// **It lives in `domain::repos` for that second role.** It was declared in
/// `api::rest::sse` until Task 21, which is what made
/// `domain::service::ingest` and `infra::executor::argo::watch` — a domain
/// module and an infra module — import the transport layer to get at it and
/// its siblings. The write-side cap that decides what the archive holds is a
/// repository-shaped decision, so it now sits beside
/// [`flatten_log_char`](super::flatten_log_char), the other rule the archived
/// text obeys, and `api::rest::sse` re-exports what the endpoint needs.
/// **The value did not change in that move, and must not change casually:**
/// this paragraph is the only thing standing between a bandwidth tweak and the
/// outcome above.
pub const MAX_LINE_BYTES: usize = 8 * 1024;

/// Marker appended to a line one of these sanitizers truncated.
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
/// **Not "about seventy bytes"** (the estimate stated where
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
///
/// **Documented but unenforced was the whole of the problem** (whole-branch
/// review, stale one-liner #2): a misdiagnosis nothing announces is a
/// misdiagnosis nobody traces back to here. `IngestService::fan_out_log`
/// now measures the prefix it actually built against this number and says
/// so — `debug_assert!` in a debug build, a `warn!` once per process
/// otherwise. It is deliberately not a hard failure in release: an
/// over-long node name still archives correctly, it only makes one
/// dropped-byte count wrong, and refusing to archive the line would be a
/// worse answer than archiving it with a warning.
/// (Re-exported from `domain::repos` as `pub(crate)` only, so
/// `service::ingest::fan_out_log` — the one place the real prefix is built —
/// can check the assumption instead of restating the number. Nothing outside
/// this crate has any use for it. `pub` rather than `pub(crate)` here because
/// this module is itself private and `clippy::redundant_pub_crate` is denied;
/// the parent's re-export is what actually bounds the visibility. Until Task 21
/// that check was `domain::service::ingest` importing `api::rest::sse`, which
/// is the layering complaint that moved this constant here.)
pub const ASSUMED_ARCHIVE_PREFIX_BYTES: usize = 256;

/// The cap a **write-side** caller must truncate to before its output is
/// wrapped in an archive prefix and read back through [`sanitize_line`] — so
/// the wrapped line still fits under [`MAX_LINE_BYTES`] and [`sanitize_line`]
/// never re-truncates it.
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
/// is what stops that: reserve the prefix's assumed width and
/// [`TRUNCATION_MARKER_MAX`] up front, so the wrapped result never crosses
/// [`MAX_LINE_BYTES`] in the first place.
///
/// # A budget, not a text-matching detector
///
/// The alternative — have [`sanitize_line`] recognise its own marker and
/// leave an already-marked line alone — would need to parse the marker's
/// text back out, a *third* copy of its format (append it here, parse it
/// there) for exactly the kind of drift this crate keeps re-discovering
/// (see [`TRUNCATION_MARKER_MAX`]'s doc, and
/// [`flatten_log_char`](super::flatten_log_char), which [`LogPosition`](super::LogPosition)
/// records the same failure for). A budget avoids that: the write side simply never
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
/// alone, matching every other truncation decision in this module.
pub const WRITE_SIDE_MAX_LINE_BYTES: usize =
    MAX_LINE_BYTES - ASSUMED_ARCHIVE_PREFIX_BYTES - TRUNCATION_MARKER_MAX;

/// Make one log line safe to frame as a single SSE event.
///
/// The read side, and the reason this function is named for a transport it no
/// longer lives in: `api::rest::sse` re-exports it under this name and
/// `handlers::runs` frames its output, both for the live stream and for
/// archive replay. Task 21 moved the implementation here so that the one
/// truncation rule, both caps and every test that pins them sit together —
/// `sanitize_line_for_archive` below is the write side of the same rule, and a
/// domain module and an infra module were importing it out of the transport
/// layer.
///
/// Two transformations, in this order. See `api::rest::sse`'s module header on
/// why the first is defence-in-depth for the endpoint.
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
/// rather than [`MAX_LINE_BYTES`], so the wrapped result never crosses the
/// read-side cap and gets re-truncated on read. See
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
///
/// **Delegates to [`flatten_log_char`](super::flatten_log_char) rather than
/// repeating its two-character `match`.** The two were byte-for-byte the same
/// rule in two modules, which is precisely what `flatten_log_char`'s own doc
/// calls *"the one definition of how a log line is flattened for storage this
/// crate has"* and what its bug history is about: `fan_out_log` and
/// `LineSkip`'s copies of that rule drifted once and silently disabled a
/// node's rotation guard. Task 21 brought the second copy into the same module
/// as the first and there is now one.
fn flatten(line: &str) -> String {
    line.chars().map(super::flatten_log_char).collect()
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

/// Every test that pins a cap in this module, in the shape ADR
/// `tests/0001-tests-in-separate-files` asks for. They came from
/// `api::rest::sse`'s own inline `mod tests` with the constants they pin.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "log_line_tests.rs"]
mod log_line_tests;
