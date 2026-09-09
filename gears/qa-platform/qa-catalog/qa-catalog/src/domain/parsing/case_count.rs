//! Static "collect": how many test cases a file declares, counted from its
//! text without running anything.
//!
//! Ported from the legacy VHP testrunner's `count_test_functions`
//! (`manager/src/routes/analytics.rs:1865-1873`, doc comment at `:1860-1864`),
//! whose only caller is `parse_test_meta` (`analytics.rs:1882`) — the same
//! whole-file read that extracts `TEST_META`. That is why this module sits
//! beside [`super::test_meta`]: both consume one file's content on one walk,
//! and the analytics universe projection ([`crate::domain::service`]'s
//! `list_universe`) needs both numbers per file.
//!
//! ## What the number feeds, and why it is a *lower bound*
//!
//! Legacy sums it across the filtered universe into
//! `OverviewSummary::case_expected` (`analytics.rs:770-779`), but only where
//! the collect job has no exact number for `(repo_id, test_file)` — the exact
//! count wins when present (`analytics.rs:767-777`). So this is deliberately
//! the fallback estimate, not the truth.
//!
//! ## Ported blind spots — do NOT "fix" these
//!
//! Every one of these makes this counter disagree with an exact pytest
//! collect. That disagreement is the *specified* behavior: a "corrected"
//! counter reports different numbers than the system this replaces.
//!
//! 1. **`@pytest.mark.parametrize` is not expanded** (legacy doc
//!    `analytics.rs:1863-1864`). One decorated `def test_x` counts as one
//!    case even when it generates twenty. This is exactly why legacy prefers
//!    the collect job's number and falls back here.
//! 2. **A failed regex compile yields `0`, not an error** (legacy's
//!    `.map(...).unwrap_or(0)` at `analytics.rs:1867` / `:1870`). Preserved
//!    below via `LazyLock<Option<Regex>>`; see [`count_matches`].
//! 3. **The Playwright pattern requires a quote immediately after the open
//!    paren**, so `test(myTitle, ...)` — a title held in a variable — counts
//!    as zero cases.
//! 4. **`\s*` matches newlines**, so both patterns' `(?m)^\s*` prefix can
//!    begin matching on an earlier blank line. It never changes a count (the
//!    `def`/`test` token still has to be there), so it is left verbatim.
//!
//! ## Two ecosystems, not one
//!
//! The function is two regexes summed. A pytest-only counter returns **zero**
//! for every Playwright spec, and `case_expected` then silently under-reports
//! for any repository holding `*.spec.ts` files. Both halves are load-bearing.

use std::sync::LazyLock;

use regex::Regex;

/// pytest cases: `def test_*(` and `async def test_*(`, at any indentation.
///
/// Verbatim from legacy `analytics.rs:1866`. The leading `^\s*` (with `(?m)`)
/// is what makes **methods** count as well as module-level functions, so a
/// `class TestGroup:` full of `def test_...` methods is not invisible.
/// `test\w*` also means the bare name `def test(` counts.
const PYTEST_CASE_PATTERN: &str = r"(?m)^\s*(?:async\s+)?def\s+test\w*\s*\(";

/// Playwright cases: `test(`, plus the `only` / `skip` / `fixme` / `fail`
/// modifier variants, each with a quoted title.
///
/// Verbatim from legacy `analytics.rs:1869`. The modifier list is an
/// enumeration rather than `\w+` on purpose: `test.describe(` is a **suite**,
/// and counting it would inflate every Playwright file by its suite count
/// (legacy doc `analytics.rs:1862-1863` calls this out explicitly).
///
/// The trailing quote class (double, single, or backtick) is blind spot 3
/// above: the open paren must be followed by a string literal.
const PLAYWRIGHT_CASE_PATTERN: &str = r#"(?m)^\s*test(?:\.(?:only|skip|fixme|fail))?\s*\(\s*["'`]"#;

/// `None` when the pattern fails to compile — legacy's `Regex::new(..).map(..)`
/// shape, which degrades to a count of `0` instead of erroring.
///
/// `LazyLock` rather than a per-call `Regex::new`: this runs once per test
/// file on the analytics universe walk, and legacy's per-call compile is an
/// implementation detail, not a behavior. The *observable* legacy behavior —
/// no panic, count `0` — is what is preserved, so this is deliberately NOT
/// the `LazyLock<Regex>` + `unwrap()` shape used in [`super::test_meta`],
/// which panics at first use instead.
static PYTEST_CASE_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(PYTEST_CASE_PATTERN).ok());

/// See [`PYTEST_CASE_RE`] for why this is an `Option`.
static PLAYWRIGHT_CASE_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(PLAYWRIGHT_CASE_PATTERN).ok());

/// Non-overlapping matches of `re` in `content`, or `0` when `re` is `None`.
///
/// The `None` arm is ported blind spot 2: legacy answers `0` for a pattern
/// that would not compile, so a broken pattern shows up as "this file has no
/// tests" rather than as an error. [`both_patterns_compile`] guards that the
/// arm is unreachable for the two patterns actually shipped.
fn count_matches(re: Option<&Regex>, content: &str) -> usize {
    re.map_or(0, |re| re.find_iter(content).count())
}

/// Count the test cases declared in one test file's source text.
///
/// pytest functions/methods **plus** Playwright spec cases — see the module
/// docs for the two patterns, the four ported blind spots, and why the answer
/// is a lower bound rather than an exact collect.
///
/// Legacy: `count_test_functions` (`manager/src/routes/analytics.rs:1865`).
#[must_use]
pub fn count_test_functions(content: &str) -> usize {
    let pytest_cases = count_matches(PYTEST_CASE_RE.as_ref(), content);
    let playwright_cases = count_matches(PLAYWRIGHT_CASE_RE.as_ref(), content);
    pytest_cases + playwright_cases
}

#[cfg(test)]
mod tests {
    use super::{
        PLAYWRIGHT_CASE_PATTERN, PLAYWRIGHT_CASE_RE, PYTEST_CASE_PATTERN, PYTEST_CASE_RE,
        count_matches, count_test_functions,
    };

    /// Legacy `analytics.rs:1866`: `^\s*` under `(?m)` matches at any
    /// indentation, so methods count alongside module-level functions, and
    /// `async def` counts because of the optional `(?:async\s+)?` group.
    /// `def helper()` must not count.
    #[test]
    fn counts_module_level_methods_and_async_pytest_functions() {
        let source = "\
import pytest

def test_alpha():
    pass

async def test_async():
    pass

def helper():
    pass

class TestGroup:
    def test_beta(self):
        pass
";
        assert_eq!(
            count_test_functions(source),
            3,
            "two module-level (one async) plus one method"
        );
    }

    /// Playwright specs count too (legacy `analytics.rs:1869`). A pytest-only
    /// counter answers 0 here, and `case_expected` then silently
    /// under-reports for every `*.spec.ts` file in the universe.
    #[test]
    fn counts_playwright_specs_including_modifier_variants() {
        let source = "\
import { test, expect } from '@playwright/test';

test('logs in', async ({ page }) => {});
test.only('focused', async ({ page }) => {});
test.skip('skipped', async ({ page }) => {});
test.fixme('broken', async ({ page }) => {});
test.fail('expected to fail', async ({ page }) => {});
";
        assert_eq!(count_test_functions(source), 5);
    }

    /// `test.describe(` is a **suite**, not a case. The legacy pattern
    /// excludes it by enumerating only `only|skip|fixme|fail`; counting it
    /// would inflate every Playwright file by its suite count.
    #[test]
    fn a_playwright_describe_block_is_not_a_case() {
        let source = "\
test.describe('suite', () => {
  test('a case', async () => {});
});
";
        assert_eq!(
            count_test_functions(source),
            1,
            "the describe wrapper must not count"
        );
    }

    /// The Playwright pattern requires a quote straight after the open paren,
    /// so a call whose title is a variable does not count. Blind spot 3,
    /// pinned rather than fixed.
    #[test]
    fn a_playwright_call_without_a_literal_title_does_not_count() {
        assert_eq!(count_test_functions("test(myTitle, async () => {});\n"), 0);
    }

    /// Blind spot 1, pinned: legacy's doc (`analytics.rs:1863-1864`) states
    /// parametrize is not expanded, and that gap is the whole reason
    /// qa-insights runs an exact collect job at all. A "fix" here would make
    /// the fallback disagree with the number legacy produced.
    #[test]
    fn parametrize_is_not_expanded() {
        let source = "\
import pytest

@pytest.mark.parametrize(\"value\", [1, 2, 3, 4, 5])
def test_values(value):
    pass
";
        assert_eq!(
            count_test_functions(source),
            1,
            "five generated cases still count as the one def that declares them"
        );
    }

    /// Blind spot 2, pinned: a pattern that fails to compile degrades to a
    /// count of `0` rather than to an error, exactly as legacy's
    /// `.map(..).unwrap_or(0)` does (`analytics.rs:1867`, `:1870`).
    #[test]
    fn an_uncompilable_pattern_counts_zero_instead_of_erroring() {
        // Assembled at runtime, not written as a literal: a literal broken
        // pattern is itself a clippy error (`invalid_regex`), and the point
        // here is the *runtime* fallback, not the lint.
        let broken = format!("{}[", r"(?m)^\s*def\s+test");
        assert!(
            regex::Regex::new(&broken).is_err(),
            "the stand-in for a broken pattern must actually be broken"
        );
        assert_eq!(count_matches(None, "def test_alpha():\n    pass\n"), 0);
    }

    /// The other half of blind spot 2: because a broken pattern is silent,
    /// nothing at runtime would tell us a shipped pattern stopped compiling —
    /// every file would just report fewer cases. This test is that alarm.
    #[test]
    fn both_patterns_compile() {
        assert!(
            PYTEST_CASE_RE.is_some(),
            "pytest pattern must compile, or every pytest file silently counts 0"
        );
        assert!(
            PLAYWRIGHT_CASE_RE.is_some(),
            "playwright pattern must compile, or every spec file silently counts 0"
        );
    }

    /// The patterns are the frozen contract, so pin their text: an
    /// "improvement" to either one changes `case_expected` for every
    /// repository, which is the one outcome this port must avoid.
    #[test]
    fn patterns_match_legacy_verbatim() {
        assert_eq!(
            PYTEST_CASE_PATTERN, r"(?m)^\s*(?:async\s+)?def\s+test\w*\s*\(",
            "legacy analytics.rs:1866"
        );
        assert_eq!(
            PLAYWRIGHT_CASE_PATTERN, r#"(?m)^\s*test(?:\.(?:only|skip|fixme|fail))?\s*\(\s*["'`]"#,
            "legacy analytics.rs:1869"
        );
    }

    /// A file with neither ecosystem's markers counts zero — the value the
    /// universe projection carries for a file that has no recognizable cases.
    #[test]
    fn a_file_with_no_tests_counts_zero() {
        assert_eq!(count_test_functions("TEST_META = {'title': 'T'}\n"), 0);
        assert_eq!(count_test_functions(""), 0);
    }

    /// Both halves are summed, so a file that somehow carries both shapes
    /// contributes both (legacy `analytics.rs:1872`).
    #[test]
    fn the_two_ecosystem_counts_are_summed() {
        let source = "\
def test_python():
    pass

test('javascript', async () => {});
";
        assert_eq!(count_test_functions(source), 2);
    }
}
