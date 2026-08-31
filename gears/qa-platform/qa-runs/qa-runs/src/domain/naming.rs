//! Human-facing run names: `{slug}-{n}`.
//!
//! Ported from `workflow_name_base` (`manager/src/services/argo.rs:2476-2508`)
//! and the `slugify_k8s_name` it calls (`argo.rs:2510-2526`), plus
//! `next_sequence_number_from_names`
//! (`manager/src/services/run_history.rs:470-484`). The join itself is
//! `argo.rs:422` — `format!("{}-{}", run_name_base, next_num)`.
//!
//! Why a run needs a short name at all, given it has a UUID: the queue's
//! `blocked_by` text names the run holding a platform ("waiting for run
//! am-validation-smoke-17",
//! `../testrunner/docs/guides/exclusive-runs-and-the-queue.md` line 177), and every operator-facing surface in the source system refers to
//! runs this way. A UUID there would be unreadable.
//!
//! # The prefix cap is now arbitrary, and kept anyway
//!
//! The source system's 48-character cap is declared with its reason attached
//! (`argo.rs:2477-2478`): "Keep a conservative prefix length so `-{N}` always
//! fits in 63-char K8s name limit." **That constraint no longer applies** —
//! this gear creates no Kubernetes objects, so nothing here is bounded by a
//! `metadata.name` limit. The cap is retained rather than widened because run
//! names are compared, logged, and rendered in tables throughout the source
//! system's UI, and changing their length distribution is a visible parity
//! change with no requirement asking for it. Widening it is a deliberate
//! divergence, not a cleanup.
//!
//! # Composition contract — what a caller must add around these three
//!
//! The pipeline is [`name_base`] → [`next_sequence`] → [`run_name`], and two of
//! the three obligations it creates live entirely outside this module:
//!
//! 1. **[`next_sequence`] must be given every name sharing the prefix, and it
//!    re-filters what it is given.** The source system's query is
//!    `WHERE workflow_name LIKE '{prefix}-%'`
//!    (`run_history.rs:452-468`), which is a *prefilter*: `smoke-%` also
//!    matches `smoke-slow-9`. The exact-prefix re-check inside
//!    [`next_sequence`] is what stops `smoke` and `smoke-slow` from sharing a
//!    counter, and it is not redundant with the query.
//!
//!    A related trap the source system avoids by accident: in SQL `LIKE`, `_`
//!    is a single-character wildcard, and the pattern is built unescaped.
//!    `slugify` never emits `_` — it is not in the allow-predicate — so a
//!    prefix that went through it cannot over-match.
//!
//!    **But `default_base` never goes through `slugify`.** [`name_base`] takes
//!    it verbatim at step 1 and returns it verbatim at step 4, so a `_` in
//!    `default_base` reaches the `LIKE` pattern intact. The premise is
//!    therefore an **obligation on the caller**, not a property of this module:
//!    `default_base` must be an ASCII, slug-safe literal containing no `_`.
//!
//!    This is live, not hypothetical. `qa_runs_sdk::RunKind::CustomPlan.as_str()`
//!    is `"custom_plan"` (`qa-runs-sdk/src/models.rs:148`; the citation read
//!    `:77` until the collect kind's doc comment moved the arm down the file),
//!    and its three siblings are `"plan"`, `"test"` and `"collect"` — the first
//!    two exactly the source system's literals — so deriving `default_base`
//!    from the run kind is the obvious move and is right for three kinds out of
//!    four. Do not pass `RunKind::CustomPlan.as_str()`.
//!    The blast radius is small, because [`next_sequence`]'s exact-prefix
//!    re-check discards anything the widened `LIKE` lets through, but the
//!    safety argument stops being an argument. A repository that widens the
//!    slug charset must escape the pattern in the same change.
//!
//! 2. **The number is advisory, and the caller needs a uniqueness constraint
//!    plus a bounded retry.** Two concurrent launches on one prefix both read
//!    the same highest number and both compute the same next one. The source
//!    system resolves this optimistically: it recomputes the number inside a
//!    three-attempt loop (`argo.rs:416-422`) and retries on the create's 409
//!    conflict (`argo.rs:677-686`). Ported here, that means a unique index on
//!    the run name and the same bounded retry around the insert — **bounded**,
//!    because an unbounded loop turns a genuinely stuck name into a hang.
//!
//! 3. **The name must be settled before the environment is assembled.** The
//!    source system's result-callback URL embeds it
//!    (`argo.rs:438-441`, `/api/runs/{wf_name}/progress`), so
//!    [`crate::domain::env_assembly`]'s tier-1 statics depend on this module's
//!    output. Naming happens at launch; assembly happens at dispatch.

/// Longest slug a run name's prefix may have. See the module docs for why this
/// number is 48 and why it stays 48.
///
/// A `pub const` where the source system has a function-local one
/// (`argo.rs:2478`), because the tests that pin truncation need to express
/// "one over the cap" without restating the number.
pub const MAX_PREFIX_LEN: usize = 48;

/// The three sources a run-name prefix can come from, in order of preference.
///
/// A struct rather than three positional arguments: `primary` and
/// `default_base` are both `&str`, transposing them compiles, and the two mean
/// opposite things — `primary` is what the name *should* be, `default_base` is
/// the last resort when nothing else survives slugification. The source system
/// passes them positionally (`argo.rs:420`); naming them here is the one
/// deliberate shape change in this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameSources<'a> {
    /// Preferred source — the plan's or test's human-written name.
    pub primary: &'a str,
    /// Used only when `primary` slugifies to nothing. The source system passes
    /// the target's id here (`argo.rs:420`), which always survives.
    pub fallback: Option<&'a str>,
    /// Used when both of the above slugify to nothing. A literal at both of the
    /// source system's call sites — `"plan"` (`argo.rs:420`) and `"test"`
    /// (`argo.rs:737`); there are only those two.
    ///
    /// **Must be an ASCII, slug-safe literal with no `_`.** Unlike `primary`
    /// and `fallback` this value never passes through `slugify`, so whatever is
    /// here reaches the run name, and from there the repository's unescaped
    /// `LIKE '{prefix}-%'` where `_` is a wildcard. In particular do not pass
    /// `qa_runs_sdk::RunKind::CustomPlan.as_str()`, which is `"custom_plan"`.
    /// The module docs' composition obligation 1 has the full argument.
    pub default_base: &'a str,
}

/// Lowercase, ASCII-alphanumeric only, with every run of other characters
/// collapsed to a single `-`, and leading/trailing `-` trimmed.
///
/// Ported character for character from `slugify_k8s_name`
/// (`manager/src/services/argo.rs:2510-2526`). Two details that look like
/// details and are not:
///
/// * **The lowercasing is Unicode, applied to the whole string before the
///   scan** (`value.to_lowercase()`, `argo.rs:2514`) — not `to_ascii_lowercase`
///   per character. The difference is observable: `to_lowercase` *expands*
///   some characters, so `İ` becomes `i` plus a combining mark and contributes
///   a real `i` to the slug, where a per-character ASCII fold would have
///   dropped the whole thing to a separator.
/// * **`_` is a separator, not a word character** — the allow-predicate is
///   `is_ascii_alphanumeric()` (`argo.rs:2515`) with no `_` arm, so
///   `smoke_tests` slugifies to `smoke-tests`. This module's charset is
///   therefore *narrower* than [`crate::domain::params::is_valid_name`]'s,
///   which does allow `_`; they are unrelated rules over unrelated inputs and
///   must not be unified.
fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut prev_dash = false;

    for ch in value.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }

    slug.trim_matches('-').to_owned()
}

/// The run-name prefix for a target.
///
/// Ported behaviour, in the order the source system applies it
/// (`manager/src/services/argo.rs:2476-2508`):
///
/// 1. Slugify `primary`; if that is empty, slugify `fallback`; if that is still
///    empty, take `default_base` **unslugified** (`argo.rs:2486-2488` — the
///    default is trusted, and both of the source system's call sites pass a
///    literal). That trust is what makes `default_base` an input this function
///    does not sanitise; see [`NameSources::default_base`] for what the caller
///    owes in return, and the module docs' obligation 1 for why it matters
///    beyond this function.
/// 2. Compact one common tail word, `-test-plan` **in preference to** `-plan`
///    (`argo.rs:2490-2495`). The order matters and the arms are `else if`, so
///    at most one is stripped: without the longer arm first, `upgrade-test-plan`
///    would compact to `upgrade-test`.
/// 3. Trim `-`, truncate to [`MAX_PREFIX_LEN`], trim `-` again — the second
///    trim is what stops truncation from leaving a trailing hyphen
///    (`argo.rs:2496-2501`).
/// 4. If the result is empty, fall back to `default_base`
///    (`argo.rs:2503-2507`).
///
/// **Step 4 is near-dead code, and is ported rather than dropped.** Step 1
/// guarantees a non-empty slug unless `default_base` is itself empty, and
/// neither the tail-word strip nor the truncate-then-trim can empty a slug that
/// `slugify` produced (it has no leading `-` and no `--` run). The one input
/// that reaches step 4 with an effect is a `default_base` made only of
/// separators — `"---"` slugifies to nothing at step 3's trim and is returned
/// verbatim by step 4. No call site does that; the branch is kept because
/// removing a guard on the strength of "no current caller triggers it" is how
/// guards stop applying.
///
/// This is also why there is no test asserting that stripping a whole slug
/// falls back to the default: `slugify` never emits a leading `-`, so no slug
/// can equal `-plan` or `-test-plan`, so that case cannot arise. A test
/// asserting it would be inert.
#[must_use]
pub fn name_base(sources: NameSources<'_>) -> String {
    let NameSources {
        primary,
        fallback,
        default_base,
    } = sources;

    let mut slug = slugify(primary);
    if slug.is_empty()
        && let Some(fallback) = fallback
    {
        slug = slugify(fallback);
    }
    if slug.is_empty() {
        // `push_str` onto a String already known to be empty, rather than the
        // source system's assignment: identical effect, and the workspace
        // denies `clippy::assigning_clones`.
        slug.push_str(default_base);
    }

    // Keep naming generic: compact common tail words used in human plan names.
    // `or_else`, longest arm first — the source system's `else if` chain
    // (`argo.rs:2490-2495`), which strips at most one suffix.
    let compacted = slug
        .strip_suffix("-test-plan")
        .or_else(|| slug.strip_suffix("-plan"))
        .unwrap_or(slug.as_str());
    let slug = compacted.trim_matches('-').to_owned();

    let slug = if slug.len() > MAX_PREFIX_LEN {
        let mut truncated = slug;
        // The cap is a byte cap, as the source system's `String::truncate` is.
        //
        // The source system truncates at exactly `MAX_PREFIX_LEN` and **panics**
        // if that byte is mid-codepoint. It cannot reach that on a slug, which
        // is ASCII by construction — but `default_base` bypasses `slugify`
        // entirely (step 1) and is a `pub` field of a borrowed `&str`, so
        // nothing enforces the "literal" the doc asks for. Walking back to the
        // nearest boundary is a deliberate divergence at an input the source
        // system panics on, for the reason `next_sequence` gives about the
        // identical hazard: a domain function should not panic on its input.
        // At most three iterations — UTF-8 code points are four bytes at most.
        let mut cut = MAX_PREFIX_LEN;
        while !truncated.is_char_boundary(cut) {
            cut -= 1;
        }
        truncated.truncate(cut);
        // Truncation can land mid-separator; trim again so no name ends in `-`.
        truncated.trim_matches('-').to_owned()
    } else {
        slug
    };

    if slug.is_empty() {
        default_base.to_owned()
    } else {
        slug
    }
}

/// One past the highest `{prefix}-{digits}` sequence among `existing`.
///
/// Ported from `next_sequence_number_from_names`
/// (`manager/src/services/run_history.rs:470-484`): strip `{prefix}-`, parse
/// the remainder as a `u64`, take the maximum, add one, and treat "nothing
/// matched" as zero so the first run of a prefix is `1`.
///
/// **Only an exact `{prefix}-{digits}` match counts.** A longer prefix that
/// merely starts with this one (`smoke-slow-9` against prefix `smoke`) leaves
/// `slow-9`, which does not parse, so it cannot bump this prefix's counter.
/// Without that, `smoke` and `smoke-slow` would share a sequence — and the
/// repository query cannot prevent it, because `LIKE 'smoke-%'` matches both
/// (see the module docs' composition contract).
///
/// The parse is deliberately left as a bare `u64::from_str` rather than being
/// prefixed with an all-digits guard. The guard would be *stricter* than the
/// source system — `+5` parses as `5` there — and stricter is the wrong
/// direction here: an unrecognised existing name is one this function will
/// happily reassign.
///
/// Two shape notes, neither of which changes what the function computes.
/// The bound is `IntoIterator`, not the plan's `Iterator`: it matches the
/// source system's own `I: IntoIterator<Item = &'a str>`
/// (`run_history.rs:470-472`), it is a strict superset so every `Iterator` the
/// plan's call sites pass still compiles, and it lets a caller hand over a
/// `Vec` or an array without an explicit `.into_iter()`.
///
/// `saturating_add` is the other, and it is the one intentional
/// micro-divergence: the source
/// system's `max_num + 1` panics in a debug build on a name of
/// `{prefix}-18446744073709551615`. Unreachable through [`run_name`], but the
/// input is a database row, and a domain function should not panic on one. The
/// caller's uniqueness constraint is the real defence either way (composition
/// contract, obligation 2).
#[must_use]
pub fn next_sequence<'a>(prefix: &str, existing: impl IntoIterator<Item = &'a str>) -> u64 {
    let highest = existing
        .into_iter()
        .filter_map(|name| {
            name.strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('-'))
                .and_then(|suffix| suffix.parse::<u64>().ok())
        })
        .max()
        .unwrap_or(0);
    highest.saturating_add(1)
}

/// `{base}-{number}` — the whole run name (`manager/src/services/argo.rs:422`).
///
/// This is the value that goes under the uniqueness constraint the module
/// docs' composition obligation 2 requires, and the value
/// [`crate::domain::env_assembly`]'s result-callback URL embeds (obligation 3).
/// It is pure and cannot fail; everything that makes it *unique* lives in the
/// caller.
#[must_use]
pub fn run_name(base: &str, number: u64) -> String {
    format!("{base}-{number}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(primary: &str, fallback: Option<&str>, default_base: &str) -> String {
        name_base(NameSources {
            primary,
            fallback,
            default_base,
        })
    }

    #[test]
    fn a_plain_name_slugifies() {
        assert_eq!(base("Smoke Tests", None, "plan"), "smoke-tests");
    }

    /// Every run of non-alphanumerics becomes exactly one `-`
    /// (`argo.rs:2514-2523`).
    ///
    /// **Corrected against legacy.** The plan predicted `"a-b__c-d"`, from a
    /// reimplemented `slugify` that treated `_` as a word character. The source
    /// system's allow-predicate is `is_ascii_alphanumeric()` with no `_` arm,
    /// so the underscores are separators and collapse with everything else.
    #[test]
    fn non_alphanumerics_collapse_to_single_hyphens() {
        assert_eq!(base("A//B__C  D", None, "plan"), "a-b-c-d");
    }

    /// The underscore rule on its own, so the correction above is pinned by a
    /// test whose name says what it protects. `_` is legal in a *parameter*
    /// name ([`crate::domain::params::is_valid_name`]) and is not legal here;
    /// the two charsets are unrelated.
    #[test]
    fn an_underscore_is_a_separator_not_a_word_character() {
        assert_eq!(base("smoke_tests", None, "plan"), "smoke-tests");
    }

    #[test]
    fn a_blank_primary_falls_back_then_defaults() {
        assert_eq!(base("", Some("plan-id-7"), "plan"), "plan-id-7");
        assert_eq!(base("!!!", Some("!!!"), "plan"), "plan");
        assert_eq!(base("", None, "plan"), "plan");
    }

    /// Tail-word compaction (`argo.rs:2490-2495`).
    ///
    /// **Corrected against legacy.** The plan expected `"Test Plan"` to yield
    /// `"plan"`, commented "stripping the whole slug must fall back to the
    /// default base". It cannot: `slugify` gives `test-plan`, nine characters,
    /// which the ten-character `-test-plan` arm cannot match, so the `-plan`
    /// arm strips and leaves `test`. The fallback that comment describes is
    /// unreachable — see [`name_base`]'s doc on step 4.
    #[test]
    fn common_tail_words_are_compacted() {
        assert_eq!(base("Upgrade Test Plan", None, "plan"), "upgrade");
        assert_eq!(base("Upgrade Plan", None, "plan"), "upgrade");
        assert_eq!(
            base("Test Plan", None, "plan"),
            "test",
            "`-test-plan` needs an eleventh character to match, so a nine-character \
             `test-plan` falls to the `-plan` arm"
        );
    }

    /// The arms are `else if` and the longer one comes first, so
    /// `nightly-test-plan` loses `-test-plan` whole rather than losing `-plan`
    /// and keeping `-test`. Reversing the two arms is a one-line edit that
    /// nothing else here would catch.
    #[test]
    fn the_longer_tail_word_is_tried_first() {
        assert_eq!(base("Nightly Test Plan", None, "plan"), "nightly");
    }

    /// Truncation to [`MAX_PREFIX_LEN`], and the second `-` trim that follows
    /// it (`argo.rs:2498-2501`). The second case lands the cap exactly on a
    /// separator, which is the only input that can tell the two trims apart:
    /// 47 `b`s, a separator at index 47, then filler. Truncating to 48 keeps
    /// the separator; the trim removes it, so the result is 47 characters.
    #[test]
    fn a_long_name_is_truncated_to_the_prefix_cap_and_trimmed() {
        let long = "a".repeat(MAX_PREFIX_LEN + 20);
        assert_eq!(base(&long, None, "plan").len(), MAX_PREFIX_LEN);

        let hyphen_at_cap = format!("{}-{}", "b".repeat(MAX_PREFIX_LEN - 1), "c".repeat(10));
        let out = base(&hyphen_at_cap, None, "plan");
        assert!(!out.ends_with('-'), "got {out:?}");
        assert_eq!(out.len(), MAX_PREFIX_LEN - 1);
    }

    #[test]
    fn leading_and_trailing_hyphens_are_trimmed() {
        assert_eq!(base("--smoke--", None, "plan"), "smoke");
    }

    /// The lowercasing is `str::to_lowercase` over the whole string, applied
    /// before the scan (`argo.rs:2514`) — not a per-character ASCII fold.
    ///
    /// `U+0130` (capital I with dot above) is the discriminating input: Unicode
    /// lowercases it to `U+0069 U+0307`, so an ASCII `i` reaches the slug and
    /// the combining mark becomes a separator that the trim then removes. A
    /// per-character `to_ascii_lowercase` leaves `U+0130` untouched, it fails
    /// `is_ascii_alphanumeric`, the whole slug empties, and the name falls
    /// through to the default base. The expectation is read from Unicode's
    /// `SpecialCasing` mapping, which is what the source system's call resolves
    /// to, not from running this code.
    #[test]
    fn lowercasing_is_unicode_aware_not_a_per_character_ascii_fold() {
        assert_eq!(base("\u{130}", None, "plan"), "i");
    }

    /// The truncation walks back to a character boundary rather than panicking.
    ///
    /// Only reachable through `default_base`, which bypasses `slugify` — see
    /// [`NameSources::default_base`]. `\u{20ac}` is three bytes, so
    /// `a` + seventeen of them is 52 bytes with boundaries at `1 + 3k`: byte 48
    /// is **not** one, which is exactly the input the source system panics on.
    /// This is the documented divergence, so the expectation comes from the
    /// rule, not from legacy: cut at 46, the nearest boundary at or below the
    /// cap.
    #[test]
    fn truncation_of_a_multi_byte_default_base_lands_on_a_character_boundary() {
        let default_base = format!("a{}", "\u{20ac}".repeat(17));
        assert_eq!(default_base.len(), 52);
        assert!(!default_base.is_char_boundary(MAX_PREFIX_LEN));

        let out = base("", None, &default_base);
        assert_eq!(out.len(), 46);
        assert_eq!(out.chars().count(), 16);
    }

    // ---------- sequence numbers ----------

    #[test]
    fn the_first_run_of_a_prefix_is_one() {
        assert_eq!(next_sequence("smoke", std::iter::empty()), 1);
    }

    #[test]
    fn the_next_number_is_one_past_the_highest_seen() {
        assert_eq!(next_sequence("smoke", ["smoke-1", "smoke-3", "smoke-2"]), 4);
    }

    /// Only exact `{prefix}-{digits}` names count. A different prefix that
    /// merely starts with this one must not bump the sequence, or `smoke` and
    /// `smoke-slow` share a counter — and the repository query cannot prevent
    /// it, because `LIKE 'smoke-%'` matches both (`run_history.rs:460-463`).
    #[test]
    fn a_longer_prefix_does_not_bump_this_ones_sequence() {
        assert_eq!(next_sequence("smoke", ["smoke-slow-9", "smoke-2"]), 3);
    }

    #[test]
    fn non_numeric_suffixes_are_ignored() {
        assert_eq!(
            next_sequence(
                "smoke",
                ["smoke-abc", "smoke-", "smoke", "smoke-2x", "smoke-2"]
            ),
            3
        );
    }

    /// The saturating add, which is this module's other documented divergence:
    /// the source system's `max_num + 1` (`run_history.rs:483`) panics in a
    /// debug build on this row. The expectation comes from the rule stated on
    /// [`next_sequence`], not from legacy, which has no defined answer here.
    ///
    /// Unreachable through [`run_name`], which is the point — the input is a
    /// database row, and a domain function should not panic on one.
    #[test]
    fn a_sequence_already_at_the_maximum_saturates_rather_than_panicking() {
        let name = format!("smoke-{}", u64::MAX);
        assert_eq!(next_sequence("smoke", [name.as_str()]), u64::MAX);
    }

    /// `format!("{}-{}", run_name_base, next_num)` (`argo.rs:422`).
    #[test]
    fn run_name_joins_the_base_and_the_number() {
        assert_eq!(run_name("smoke", 7), "smoke-7");
    }
}
