//! `TEST_META` text parsing — frozen contract (PRD `cpt-cf-qa-fr-catalog-test-meta`).
//!
//! Re-implements the legacy VHP testrunner's *parsing rules* — the regexes,
//! the whole-file scan, the OR over `exclusive` occurrences, the `TEST_TITLE`
//! fallback, the tag normalization — for the subset of keys this gear's
//! contract carries. The block is read as text and NEVER executed, so both
//! Python (`True`/`False`) and JSON (`true`/`false`) literals are accepted,
//! with keys in single or double quotes.
//!
//! ## Deliberate differences from legacy
//!
//! The key set is **not** identical, so this is a re-implementation of the
//! rules rather than of the whole legacy struct:
//!
//! 1. **One legacy key is still not parsed** — the docstring-derived
//!    `description` (`tests.rs:1015`, `first_docstring`), which feeds
//!    legacy's test browser and has no p1 counterpart here.
//!
//!    **Corrected 2026-08-18 (qa-insights Task 7).** This list used to name
//!    three unparsed keys, adding `component` and `quality_vectors` on the
//!    grounds that "the catalog's contract is exclusivity plus display
//!    metadata". That stopped being true when ADR-0005 confined git egress
//!    to this gear: qa-insights has no checkout, so the analytics universe
//!    projection it consumes must carry both keys from here or lose them
//!    entirely (legacy read them off the checkout itself,
//!    `manager/src/routes/analytics.rs:869-882`). Both are now parsed below
//!    — see [`ParsedTestMeta::component`] and
//!    [`ParsedTestMeta::quality_vectors`]. They are deliberately **not**
//!    added to the SDK's `TestFileMeta`: that type is qa-runs' exclusivity
//!    input and neither key belongs to it.
//! 2. **`bugs` has no legacy counterpart** — it is PRD-required and follows
//!    the `tags` extraction conventions exactly (see [`BUGS_RE`]).
//! 3. **`exclusive` is three-state, not `bool`** — legacy
//!    `parse_test_meta_exclusive` returns `bool`
//!    (`manager/src/services/test_meta.rs:42`), collapsing "no `exclusive`
//!    key" into `false`. Here an absent key is `None` so the plan.yaml /
//!    launch tiers can supply the value instead; only that distinction makes
//!    the inheritance in PRD `cpt-cf-qa-fr-runs-exclusivity` expressible. Any
//!    occurrence still resolves the same way legacy's OR does.
//!
//! Keys are scanned over the whole file rather than scoped to a
//! `TEST_META = { ... }` block: scoping via a brace regex breaks on any nested
//! dict value, and the legacy parsers deliberately never scoped their keys.
//!
//! Exclusivity is OR'd over every occurrence in the file, rather than taking
//! the first: a stale commented-out `False` above the real declaration must
//! not disarm it. The two failure directions are not symmetric — guessing
//! "exclusive" costs platform throughput (a run waits), while guessing
//! "parallel" lets a destructive test run beside another and corrupt a live
//! stand — so ambiguity resolves toward the recoverable one. The accepted
//! cost: prose or a commented-out line that mentions an exclusive-true
//! declaration makes the file read as exclusive.

use std::sync::LazyLock;

use regex::Regex;
use toolkit_macros::domain_model;

/// Parsed metadata for one test file (pre-SDK; the path is attached by the
/// caller).
///
/// `exclusive` is three-state: `Some(true)` — any exclusive-true occurrence in
/// the file; `Some(false)` — only false occurrences; `None` — the key never
/// appears (inherit from `plan.yaml`/launch tiers).
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedTestMeta {
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub exclusive: Option<bool>,
    pub bugs: Vec<String>,
    /// `TEST_META` `component`, first occurrence only.
    ///
    /// Read by the analytics universe projection, which falls back to
    /// inferring it from the path when absent (legacy
    /// `infer_component_from_path`, `manager/src/routes/analytics.rs:1794`).
    /// The inference is the *caller's* job — it needs the path, which this
    /// parser deliberately never sees.
    pub component: Option<String>,
    /// `TEST_META` `quality_vectors`, trimmed, blanks dropped, and
    /// de-duplicated **case-insensitively while preserving the first spelling
    /// seen** — legacy `analytics.rs:1917-1935`. The case-folded dedup is not
    /// cosmetic: legacy's quality-vector summary keys on the folded form
    /// (`analytics.rs:880`), so `["Security", "security"]` is one vector, not
    /// two, and a plain `dedup` here would double-count it.
    pub quality_vectors: Vec<String>,
}

/// Legacy: testrunner `manager/src/services/test_meta.rs:46`.
#[allow(clippy::unwrap_used)] // Compile-time-known regex patterns; panics in init are intentional
static EXCLUSIVE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)["']exclusive["']\s*:\s*(true|false)"#).unwrap());

/// Legacy: testrunner `manager/src/routes/tests.rs:1021`.
#[allow(clippy::unwrap_used)]
static TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"["']title["']\s*:\s*["']([^"']+)["']"#).unwrap());

/// Legacy: testrunner `manager/src/routes/tests.rs:1027` (pre-`TEST_META` files).
#[allow(clippy::unwrap_used)]
static LEGACY_TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)^TEST_TITLE\s*=\s*["']([^"']+)["']"#).unwrap());

/// Legacy: testrunner `manager/src/routes/analytics.rs:1885` (the analytics
/// half of `parse_test_meta`; the test-browser half is `routes/tests.rs:1022`).
#[allow(clippy::unwrap_used)]
static COMPONENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)["']component["']\s*:\s*["']([^"']+)["']"#).unwrap());

/// Legacy: testrunner `manager/src/routes/analytics.rs:1888`.
#[allow(clippy::unwrap_used)]
static QUALITY_VECTORS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?ms)["']quality_vectors["']\s*:\s*\[([^\]]*)\]"#).unwrap());

/// Legacy: testrunner `manager/src/services/test_meta.rs:62`.
#[allow(clippy::unwrap_used)]
static TAGS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?ms)["']tags["']\s*:\s*\[([^\]]*)\]"#).unwrap());

/// No legacy counterpart; PRD-required, follows the `tags` conventions.
#[allow(clippy::unwrap_used)]
static BUGS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?ms)["']bugs["']\s*:\s*\[([^\]]*)\]"#).unwrap());

/// Legacy: testrunner `manager/src/services/test_meta.rs:65`.
#[allow(clippy::unwrap_used)]
static LIST_ITEM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"["']([^"']+)["']"#).unwrap());

/// Parse a test file's `TEST_META` metadata from its source text.
///
/// Infallible by design: absent keys read as absent, never as an error — a
/// malformed file is still a test file, it just carries no metadata.
#[must_use]
pub fn parse_test_meta(source: &str) -> ParsedTestMeta {
    ParsedTestMeta {
        title: first_capture(&TITLE_RE, source).or_else(|| first_capture(&LEGACY_TITLE_RE, source)),
        tags: capture_list(&TAGS_RE, source),
        exclusive: parse_exclusive(source),
        bugs: capture_list(&BUGS_RE, source),
        component: first_capture(&COMPONENT_RE, source),
        quality_vectors: dedup_fold_ascii_case(capture_list(&QUALITY_VECTORS_RE, source)),
    }
}

/// Keep the first spelling of each entry, comparing case-insensitively.
///
/// Legacy `analytics.rs:1918-1933`: the `HashSet` holds `to_ascii_lowercase`
/// while the pushed value keeps its original case. Order is the file's.
fn dedup_fold_ascii_case(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.to_ascii_lowercase()))
        .collect()
}

/// Whole-file scan, OR over every occurrence: any true wins over any false;
/// only false occurrences give `Some(false)`; no occurrence gives `None`.
fn parse_exclusive(source: &str) -> Option<bool> {
    let mut found = None;
    for value in EXCLUSIVE_RE
        .captures_iter(source)
        .filter_map(|caps| caps.get(1))
    {
        let is_true = value.as_str().eq_ignore_ascii_case("true");
        found = Some(found.unwrap_or(false) | is_true);
    }
    found
}

/// The first capture group of the first match, if the regex matched.
fn first_capture(re: &Regex, source: &str) -> Option<String> {
    re.captures(source)?.get(1).map(|m| m.as_str().to_owned())
}

/// Quoted items of the first `key: [...]` list, trimmed, blanks dropped
/// (legacy `normalize_tags`, testrunner `manager/src/services/test_bundles.rs:272`).
fn capture_list(block_re: &Regex, source: &str) -> Vec<String> {
    let Some(block) = first_capture(block_re, source) else {
        return Vec::new();
    };
    LIST_ITEM_RE
        .captures_iter(&block)
        .filter_map(|caps| caps.get(1))
        .map(|m| m.as_str().trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_meta_yields_default() {
        let m = parse_test_meta("import pytest\n\ndef test_x():\n    pass\n");
        assert_eq!(m.exclusive, None);
        assert_eq!(m.title, None);
        assert!(m.tags.is_empty());
        assert!(m.bugs.is_empty());
    }

    #[test]
    fn python_literal_true() {
        let src = r#"
TEST_META = {
    "title": "Cluster upgrade",
    "tags": ["e2e", "destructive"],
    "exclusive": True,
}
"#;
        let m = parse_test_meta(src);
        assert_eq!(m.exclusive, Some(true));
        assert_eq!(m.title.as_deref(), Some("Cluster upgrade"));
        assert_eq!(m.tags, vec!["e2e", "destructive"]);
    }

    #[test]
    fn json_literal_true() {
        let m = parse_test_meta("TEST_META = { \"exclusive\": true }");
        assert_eq!(m.exclusive, Some(true));
    }

    #[test]
    fn explicit_false_only() {
        let m = parse_test_meta("TEST_META = { \"exclusive\": False }");
        assert_eq!(m.exclusive, Some(false));
    }

    #[test]
    fn any_true_occurrence_wins_over_false() {
        // A commented-out False must not disarm; a stray True anywhere arms.
        let src = r#"
# was: "exclusive": False
TEST_META = { "exclusive": False }
# docs say to use "exclusive": True for reboot tests
"#;
        let m = parse_test_meta(src);
        assert_eq!(
            m.exclusive,
            Some(true),
            "any true occurrence, even in a comment, wins"
        );
    }

    #[test]
    fn true_in_docstring_counts() {
        // Deliberate contract cost: prose mentioning it arms the file.
        let src = "\"\"\"Set \"exclusive\": True in TEST_META for destructive suites.\"\"\"";
        assert_eq!(parse_test_meta(src).exclusive, Some(true));
    }

    // Legacy truth (testrunner manager/src/services/test_meta.rs:46): the key
    // and value match with single quotes and case-insensitively.
    #[test]
    fn single_quoted_key_and_value_are_accepted() {
        let m = parse_test_meta("TEST_META = {'exclusive': True, 'title': 'Smoke'}\n");
        assert_eq!(m.exclusive, Some(true));
        assert_eq!(m.title.as_deref(), Some("Smoke"));
    }

    #[test]
    fn exclusive_matching_is_case_insensitive() {
        assert_eq!(
            parse_test_meta("TEST_META = {\"Exclusive\": TRUE}\n").exclusive,
            Some(true)
        );
        assert_eq!(
            parse_test_meta("TEST_META = {\"EXCLUSIVE\": false}\n").exclusive,
            Some(false)
        );
    }

    // Legacy truth (test_meta.rs:159-168): the key must match whole, not as a
    // substring — `"non_exclusive"` and `"exclusive_setup"` are different keys.
    #[test]
    fn a_key_that_merely_contains_exclusive_is_not_matched() {
        assert_eq!(
            parse_test_meta("TEST_META = {\"non_exclusive\": True}\n").exclusive,
            None
        );
        assert_eq!(
            parse_test_meta("TEST_META = {\"exclusive_setup\": True}\n").exclusive,
            None
        );
    }

    // Legacy truth (test_meta.rs:24-28): keys are scanned over the whole file,
    // never scoped to a `TEST_META = { ... }` block.
    #[test]
    fn keys_outside_a_meta_block_still_parse() {
        let src = "META = {\"title\": \"Anywhere\", \"tags\": [\"smoke\"]}\n";
        let m = parse_test_meta(src);
        assert_eq!(m.title.as_deref(), Some("Anywhere"));
        assert_eq!(m.tags, vec!["smoke"]);
    }

    // Legacy truth (test_bundles.rs:272-276, normalize_tags): entries are
    // trimmed and blank entries dropped.
    #[test]
    fn tags_are_trimmed_and_blanks_dropped() {
        let m = parse_test_meta("TEST_META = {\"tags\": [\" e2e \", \"  \", \"smoke\"]}\n");
        assert_eq!(m.tags, vec!["e2e", "smoke"]);
    }

    // Legacy truth (routes/tests.rs:1027-1036): a module-level TEST_TITLE
    // assignment is the fallback when no "title" key is present.
    #[test]
    fn legacy_test_title_fallback() {
        let m = parse_test_meta("TEST_TITLE = \"Old style title\"\n\ndef test_x():\n    pass\n");
        assert_eq!(m.title.as_deref(), Some("Old style title"));
    }

    #[test]
    fn title_key_wins_over_legacy_test_title() {
        let src = "TEST_TITLE = 'Old'\nTEST_META = {\"title\": \"New\"}\n";
        assert_eq!(parse_test_meta(src).title.as_deref(), Some("New"));
    }

    // Legacy truth (routes/tests.rs:1021): the title pattern is `[^"']+`,
    // so an empty title never matches and reads as absent.
    #[test]
    fn empty_title_reads_as_absent() {
        assert_eq!(
            parse_test_meta("TEST_META = {\"title\": \"\"}\n").title,
            None
        );
    }

    // Legacy truth (routes/analytics.rs:1885, :1901-1903): `component` is the
    // FIRST match only, same shape as `title`.
    #[test]
    fn extracts_component() {
        let m = parse_test_meta("TEST_META = {\"component\": \"storage\"}\n");
        assert_eq!(m.component.as_deref(), Some("storage"));
    }

    #[test]
    fn absent_component_reads_as_none() {
        assert_eq!(
            parse_test_meta("TEST_META = {\"title\": \"T\"}\n").component,
            None
        );
    }

    // Legacy truth (routes/analytics.rs:1888, :1917-1935): the list is
    // trimmed, blanks are dropped, and duplicates fold case-insensitively
    // with the FIRST spelling kept.
    #[test]
    fn extracts_quality_vectors_deduped_case_insensitively() {
        let src =
            "TEST_META = {\"quality_vectors\": [\" Security \", \"security\", \"  \", \"Perf\"]}\n";
        let m = parse_test_meta(src);
        assert_eq!(
            m.quality_vectors,
            vec!["Security", "Perf"],
            "case-folded dedup keeps the first spelling; blanks are dropped"
        );
    }

    #[test]
    fn absent_quality_vectors_read_as_empty() {
        assert!(
            parse_test_meta("TEST_META = {\"title\": \"T\"}\n")
                .quality_vectors
                .is_empty()
        );
    }

    // `bugs` has no legacy TEST_META key; the PRD requires exposing linked
    // bugs, and the key follows the legacy `tags` list conventions.
    #[test]
    fn extracts_bug_links() {
        let src = r#"
TEST_META = {
    "title": "Storage failover",
    "bugs": ["VHP-2618", "VHP-101"],
}
"#;
        let m = parse_test_meta(src);
        assert_eq!(m.bugs, vec!["VHP-2618", "VHP-101"]);
    }
}
