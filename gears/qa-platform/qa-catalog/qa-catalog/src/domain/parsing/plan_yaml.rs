//! plan.yaml parsing — frozen contract with test authors
//! (PRD `cpt-cf-qa-fr-catalog-plan-discovery`).
//!
//! ## Legacy-truth: the frozen `plan.yaml` contract
//!
//! PRD `cpt-cf-qa-fr-migration-runner-contract` promises the format is
//! unchanged, so existing test repositories run unmodified. Four legacy
//! behaviors this module is responsible for, all mirroring
//! `manager/src/services/plans.rs` in the legacy VHP testrunner:
//!
//! * **`tests:` is optional** — legacy `PlanDocument.tests` is
//!   `#[serde(default)]` (plans.rs:25-26). An absent or empty list is valid;
//!   *directory* plans then derive their file list from
//!   `<plan_dir>/tests/**` in the discovery layer
//!   (`crate::domain::service::plans`, legacy plans.rs:685-692), while
//!   *file-based* plans (`plans/*.yaml`) keep an empty list
//!   (legacy plans.rs:501-589 derives nothing).
//! * **`timeout_seconds` defaults to 300** — legacy
//!   `#[serde(default = "default_timeout")]` (plans.rs:15-16) →
//!   `default_timeout() == 300` (plans.rs:29-31). See
//!   [`DEFAULT_TIMEOUT_SECONDS`].
//! * **Every test path is normalized** — legacy runs each entry through
//!   `normalize_test_path` (plans.rs:782-787) at both discovery flavors
//!   (plans.rs:559-563, plans.rs:695-699), dropping entries that normalize
//!   to nothing. See [`normalize_test_path`].
//! * **`validation` is a bool OR a tag** — legacy ORs the explicit
//!   `validation:` bool with a trimmed, case-insensitively matched
//!   `validation` entry in `tags:` (plans.rs:35-39). See
//!   [`ParsedPlan::validation`].

use serde::Deserialize;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// Timeout a plan runs with when `plan.yaml` omits `timeout_seconds`.
///
/// Legacy `default_timeout` (testrunner `manager/src/services/plans.rs:29-31`,
/// wired via `#[serde(default = ...)]` at plans.rs:15-16). A plan that legacy
/// ran with a five-minute ceiling must not start propagating "no timeout" to
/// qa-runs.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 300;

const fn default_timeout_seconds() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

/// Raw deserialization target. Unknown keys tolerated by serde default behavior.
#[derive(Debug, Deserialize)]
struct RawPlanYaml {
    name: String,
    /// Optional, exactly as in legacy (plans.rs:25-26) — see the module docs.
    #[serde(default)]
    tests: Vec<String>,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
    #[serde(default)]
    tags: Vec<String>,
    /// Legacy `PlanDocument.validation` (plans.rs:21-22) — see
    /// [`ParsedPlan::validation`] for how it combines with `tags`.
    #[serde(default)]
    validation: bool,
    /// Three-state: absent → None (inherit).
    #[serde(default)]
    exclusive: Option<bool>,
}

/// Parsed plan definition, pre-SDK (repo/branch/path are attached by the caller).
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedPlan {
    pub name: String,
    /// Normalized test paths, relative to the repository content root. May be
    /// **empty**: a directory plan's list is then derived from
    /// `<plan_dir>/tests/**` by the discovery layer, and a file-based plan
    /// legitimately carries none (legacy parity — see the module docs).
    pub test_files: Vec<String>,
    /// Always populated: the plan's own value, or
    /// [`DEFAULT_TIMEOUT_SECONDS`] when the key is absent. The SDK's
    /// `Plan::timeout_seconds` is `Option<u64>` (a *custom* plan may carry no
    /// timeout at all), so a discovered plan maps to `Some(_)` here and
    /// `None` is unreachable from a parsed `plan.yaml`.
    pub timeout_seconds: u64,
    pub tags: Vec<String>,
    /// Whether the plan classifies as a *validation* run.
    ///
    /// Legacy ORs the explicit `validation:` bool with a `validation` **tag**,
    /// matched case-insensitively after trimming
    /// (`manager/src/services/plans.rs:35-39`).
    pub validation: bool,
    pub exclusive: Option<bool>,
}

/// Parse the contents of a `plan.yaml` file.
///
/// Unknown keys are tolerated (plain serde derive without `deny_unknown_fields`).
/// `exclusive` is three-state: absent → `None` (inherit from `TEST_META`/defaults),
/// explicit `false` → `Some(false)` (overrides inheritance). `tests` is
/// optional and `timeout_seconds` defaults to [`DEFAULT_TIMEOUT_SECONDS`] —
/// both mirroring legacy, see the module docs.
///
/// # Errors
///
/// Returns [`DomainError::PlanYamlInvalid`] if `content` is not valid YAML or
/// is missing the only required key, `name`.
pub fn parse_plan_yaml(content: &str) -> Result<ParsedPlan, DomainError> {
    let raw: RawPlanYaml =
        serde_saphyr::from_str(content).map_err(|e| DomainError::PlanYamlInvalid {
            message: e.to_string(),
        })?;
    // Legacy ORs the bool with a trimmed, case-insensitive `validation` tag
    // (`manager/src/services/plans.rs:35-39`). Computed before the struct
    // literal because `raw.tags` is moved into `ParsedPlan.tags` below, so the
    // borrow has to happen first — which is why legacy hoists it too.
    let validation = raw.validation
        || raw
            .tags
            .iter()
            .any(|tag| tag.trim().eq_ignore_ascii_case("validation"));
    Ok(ParsedPlan {
        name: raw.name,
        // Normalize BEFORE anything downstream validates: legacy repaired
        // `./x`, `/x` and `a\b` silently, and the gear's `validate_rel_path`
        // hard-rejects all three. Normalization never weakens that check —
        // `..` components survive it and are still rejected.
        test_files: raw
            .tests
            .iter()
            .map(|test| normalize_test_path(test))
            .filter(|test| !test.is_empty())
            .collect(),
        timeout_seconds: raw.timeout_seconds,
        tags: raw.tags,
        validation,
        exclusive: raw.exclusive,
    })
}

/// Normalize one `tests:` entry, byte-for-byte as legacy's
/// `normalize_test_path` does (testrunner
/// `manager/src/services/plans.rs:782-787`): trim surrounding whitespace,
/// strip leading `./` then leading `/`, and convert `\` to `/`.
///
/// The operation order is legacy's, including its one quirk: separators are
/// converted *after* the leading-separator strip, so a Windows-absolute
/// `\tests\x.py` normalizes to `/tests/x.py` — broken in legacy too (it
/// resolved no file there), and rejected as absolute by
/// `crate::domain::service::plans::validate_rel_path` here.
#[must_use]
pub fn normalize_test_path(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_plan() {
        let yaml = "name: smoke\ntests:\n  - test_a.py\n  - test_b.py\n";
        let p = parse_plan_yaml(yaml).unwrap();
        assert_eq!(p.name, "smoke");
        assert_eq!(p.test_files, vec!["test_a.py", "test_b.py"]);
        assert_eq!(p.timeout_seconds, DEFAULT_TIMEOUT_SECONDS);
        assert_eq!(p.tags, Vec::<String>::new());
        assert_eq!(
            p.exclusive, None,
            "absent exclusive must be None (inherit), not Some(false)"
        );
    }

    #[test]
    fn parses_full_plan() {
        let yaml = r"
name: upgrade
timeout_seconds: 7200
tags: [e2e, destructive]
exclusive: true
tests:
  - upgrade/test_upgrade.py
";
        let p = parse_plan_yaml(yaml).unwrap();
        assert_eq!(p.timeout_seconds, 7200);
        assert_eq!(p.tags, vec!["e2e", "destructive"]);
        assert_eq!(p.exclusive, Some(true));
    }

    #[test]
    fn explicit_false_is_not_inherit() {
        let p = parse_plan_yaml("name: x\nexclusive: false\ntests: [a.py]\n").unwrap();
        assert_eq!(
            p.exclusive,
            Some(false),
            "explicit false overrides TEST_META true downstream"
        );
    }

    #[test]
    fn missing_name_is_error() {
        assert!(parse_plan_yaml("tests: [a.py]\n").is_err());
    }

    /// Legacy `PlanDocument.tests` is `#[serde(default)]`
    /// (testrunner `manager/src/services/plans.rs:25-26`): a plan omitting
    /// `tests:` is valid and gets an empty list here — the *discovery* layer
    /// then derives directory plans' files from disk. Requiring the key would
    /// silently drop every legacy plan that relies on the derivation.
    #[test]
    fn missing_tests_defaults_to_empty() {
        let p = parse_plan_yaml("name: x\n").expect("legacy plans may omit `tests:`");
        assert_eq!(p.name, "x");
        assert!(p.test_files.is_empty());
    }

    #[test]
    fn empty_tests_list_is_accepted() {
        let p = parse_plan_yaml("name: x\ntests: []\n").expect("an empty list is not an error");
        assert!(p.test_files.is_empty());
    }

    /// Legacy `default_timeout` → 300 (`plans.rs:15-16` + `:29-31`).
    #[test]
    fn timeout_defaults_to_the_legacy_five_minutes() {
        let p = parse_plan_yaml("name: x\ntests: [a.py]\n").unwrap();
        assert_eq!(
            p.timeout_seconds, DEFAULT_TIMEOUT_SECONDS,
            "a plan without `timeout_seconds` ran with a 5-minute ceiling in legacy"
        );
        assert_eq!(DEFAULT_TIMEOUT_SECONDS, 300);
    }

    /// Legacy `normalize_test_path` (`plans.rs:782-787`) + the
    /// `.filter(|t| !t.is_empty())` both discovery flavors apply
    /// (`plans.rs:562`, `plans.rs:698`).
    #[test]
    fn test_paths_are_normalized_like_legacy() {
        let yaml = r"
name: x
tests:
  - '  tests/spaced.py  '
  - './tests/dot.py'
  - '/tests/absolute.py'
  - 'tests\windows\sep.py'
  - '//tests/double.py'
  - ''
  - '   '
";
        let p = parse_plan_yaml(yaml).unwrap();
        assert_eq!(
            p.test_files,
            vec![
                "tests/spaced.py",
                "tests/dot.py",
                "tests/absolute.py",
                "tests/windows/sep.py",
                "tests/double.py",
            ],
            "legacy repaired these silently; the gear's path validation must never see them raw"
        );
    }

    /// Normalization must not weaken the safety validation applied
    /// downstream: `..` survives it and is rejected later by
    /// `validate_rel_path` (pinned end-to-end in
    /// `domain::service::plans_tests::normalized_test_paths_still_fail_traversal_validation`).
    #[test]
    fn normalization_does_not_neutralize_parent_dir_escapes() {
        let p = parse_plan_yaml("name: x\ntests: ['../../etc/passwd', './../a.py']\n").unwrap();
        assert_eq!(p.test_files, vec!["../../etc/passwd", "../a.py"]);
        for path in &p.test_files {
            assert!(
                path.contains(".."),
                "a traversing entry must stay traversing (and be rejected downstream): {path}"
            );
        }
    }

    #[test]
    fn validation_flag_is_read_from_the_bool() {
        let p = parse_plan_yaml("name: v\nvalidation: true\ntests: [a.py]\n").unwrap();
        assert!(p.validation);
    }

    #[test]
    fn validation_flag_is_read_from_a_case_insensitive_tag() {
        // Legacy ORs the bool with a `validation` tag, case-insensitively
        // (testrunner `manager/src/services/plans.rs:35-39`).
        let p = parse_plan_yaml("name: v\ntags: [E2E, Validation]\ntests: [a.py]\n").unwrap();
        assert!(
            p.validation,
            "a `validation` tag must arm the flag regardless of case"
        );
    }

    /// Legacy trims each tag before comparing (`plans.rs:39`
    /// — `tag.trim().eq_ignore_ascii_case("validation")`), which is only
    /// observable for a *quoted* YAML scalar: the parser strips padding from
    /// bare scalars itself.
    #[test]
    fn validation_tag_is_trimmed_before_comparison() {
        let p = parse_plan_yaml("name: v\ntags: ['  validation  ']\ntests: [a.py]\n").unwrap();
        assert!(p.validation, "legacy trims the tag before comparing");
    }

    #[test]
    fn absent_validation_is_false() {
        let p = parse_plan_yaml("name: v\ntests: [a.py]\n").unwrap();
        assert!(!p.validation);
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        // Existing repos may carry extra keys; discovery must not reject them.
        let p = parse_plan_yaml("name: x\nowner: qa-team\ntests: [a.py]\n").unwrap();
        assert_eq!(p.name, "x");
    }
}
