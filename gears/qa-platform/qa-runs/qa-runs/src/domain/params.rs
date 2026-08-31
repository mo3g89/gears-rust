//! Launch-parameter validation and normalization.
//!
//! Ported from `manager/src/routes/settings.rs:15-195`
//! (`RESERVED_PIPELINE_VARIABLE_NAMES`, `is_valid_env_var_name`,
//! `validate_variable_list`, `validate_run_parameters`,
//! `normalize_run_parameters`). Frozen contract:
//! `cpt-cf-qa-fr-runs-params` — "Violations MUST fail the launch with a
//! message naming the offending parameter", so every error variant below
//! carries the name.
//!
//! The frozen user-facing guide states the same rules from the operator's side
//! — charset, the eleven reserved names, duplicates, and all three caps — in one
//! table (`../testrunner/docs/guides/run-parameters.md:37-43`), plus "validated
//! on every launch endpoint (and re-validated when a run is re-run)" at `:31-32`
//! and "Run parameters are stored in plain text ... Do not put tokens or
//! passwords in them" at `:45-47`, which is the rule behind [`normalize`]'s
//! dropped `secure` flag.
//!
//! Pure, and applied at **both** entry points the requirement names: launch
//! and re-run. The source system re-validates a replayed set for reasons its
//! own comment states — "cheap defense-in-depth against stale/tampered rows
//! and a reserved list that may have grown since"
//! (`manager/src/routes/runs.rs:920-923`).
//!
//! # Composition contract — read this before calling either function
//!
//! [`normalize`] and [`validate`] are a **pair applied in that order**, and
//! the source system never applies one without the other
//! (`settings.rs:175-177`: `let cleaned = normalize_run_parameters(parsed);
//! validate_run_parameters(&cleaned)?; Ok(cleaned)`).
//!
//! Three obligations fall out of that, none of which a unit test inside this
//! module can enforce:
//!
//! 1. **Validate the normalized list, never the raw one.** [`validate`] does
//!    not trim, so a padded `"  FOO  "` fails the charset check instead of
//!    passing, and a wholly blank editor row fails instead of being dropped.
//! 2. **Persist the normalized list, not the request's raw one.** The source
//!    system returns `cleaned` and stores that (`settings.rs:175-177`). The
//!    reason this matters is not re-run — it is dispatch: the environment is
//!    assembled from the run's **stored** parameters (Task 14 Step 4), so a
//!    raw-persisted `"  FOO  "` becomes an environment variable literally named
//!    `"  FOO  "`, and a row blank in both fields becomes `""=""`. That happens
//!    on the very first run, with no replay involved.
//!
//!    Re-run is the secondary case and a weaker one, because Task 15 Step 6
//!    routes re-run back through the full launch path, which re-normalizes: the
//!    "succeeds once, fails on replay" failure only bites where re-run calls
//!    [`validate`] alone, as the source system's does (`runs.rs:923`).
//! 3. **Re-run may call [`validate`] alone** (as `routes/runs.rs:923` does),
//!    because obligation 2 guarantees the stored set is already normalized.
//!    Re-normalizing first is harmless — [`normalize`] is idempotent — but it
//!    is not what makes re-run correct; obligation 2 is.
//!
//! Both functions run **before any I/O**: a launch that will be rejected must
//! not force-sync a repository first (Task 13's launch pipeline).

use std::collections::HashSet;

use qa_runs_sdk::RunParameter;

/// Names the runner owns; a launch parameter may not shadow any of them.
///
/// Exactly the source system's `RESERVED_PIPELINE_VARIABLE_NAMES`
/// (`manager/src/routes/settings.rs:15-27`), name for name and in the same
/// order, and exactly what `cpt-cf-qa-fr-runs-params` enumerates.
///
/// SECURITY NOTE — inherited parity, verified 2026-08-13 (plan decision D3).
/// This list deliberately does **not** cover the runner's result-callback URL
/// (the source system's `VHP_PROGRESS_URL`, `manager/src/services/argo.rs:438-441`)
/// or `E2E_VHP_BASE_URL` (`argo.rs:493-496`), and the source system's parameter
/// merge is a retain-then-push (`argo.rs:246-262`, reused verbatim for
/// parameters at `:267-272`), so a launch parameter of either name **replaces**
/// the platform-supplied value there. That exposure is carried forward on
/// purpose: the goal is to preserve the source system's behavior and adapt only
/// the architecture. Closing it is a deliberate divergence and belongs in the
/// PRD amendment block for `cpt-cf-qa-fr-runs-params`, not in a quiet edit here.
///
/// **This list is pinned to a constant in another crate** (added 2026-08-13 by
/// Task 11b, and recorded here because the paragraph above is exactly where a
/// reader is told not to edit it quietly). One process became two gears, so the
/// source system's single list became two: this one covers run parameters,
/// while `qa_environments_sdk::RESERVED_VARIABLE_NAMES` covers pipeline and
/// platform variables. They must stay identical, name for name and in order —
/// a name reserved on one environment write path and not the others is a name
/// an operator can use to replace a secret **reference** with a literal, which
/// is the premise
/// [`crate::domain::ports::run_executor::RunEnv::new`]'s precedence rests on.
/// `the_two_reserved_name_lists_must_stay_identical` fails on any divergence.
/// It lives beside that precedence rather than here, because that is the
/// correctness argument which breaks — but editing this array is what will
/// fail it, and nothing else would tell you where to look.
pub const RESERVED_NAMES: [&str; 11] = [
    "APP_BUILD",
    "APP_VERSION",
    "E2E_K8S_NAMESPACE",
    "KUBECONFIG",
    "PRODUCT_KEY",
    "RP_API_KEY",
    "RP_PROJECT",
    "SKIP_TESTS_WITH_BUGS",
    "TEST_BUNDLE_URL",
    "TEST_FILES",
    "TEST_VERSION",
];

/// Most parameters one launch may carry (`MAX_RUN_PARAMETERS`,
/// `manager/src/routes/settings.rs:32`). Stated by `cpt-cf-qa-fr-runs-params`.
pub const MAX_PARAMETERS: usize = 50;

/// Longest parameter name, in **bytes** (`MAX_RUN_PARAMETER_NAME_LEN`,
/// `manager/src/routes/settings.rs:33`). The source system's own rationale is
/// at `settings.rs:29-31` — the caps bound the rendered workflow spec so one
/// launch cannot bloat it, and failing early gives a clearer message than the
/// orchestrator rejecting an oversized object would.
///
/// **Correction to plan decision D3, which called the two size caps
/// "undocumented".** They are documented, in the frozen user-facing guide's
/// rules table (`../testrunner/docs/guides/run-parameters.md:41-42`). What that
/// table says is "At most **128** characters", while the code measures
/// `String::len` — bytes. The two agree for every name that survives
/// [`is_valid_name`], which is ASCII by construction; they diverge only in the
/// window where a multi-byte name is still in flight, because the size pass
/// runs before the charset pass. Bytes is what is ported, because bytes is what
/// bounds the spec.
pub const MAX_NAME_LEN: usize = 128;

/// Largest parameter value, in bytes (`MAX_RUN_PARAMETER_VALUE_LEN`,
/// `manager/src/routes/settings.rs:34`). See [`MAX_NAME_LEN`] for the rationale.
pub const MAX_VALUE_LEN: usize = 8 * 1024;

/// How much of an oversized parameter name an error message echoes.
///
/// The name that triggers [`ParamError::NameTooLong`] is over the cap by
/// definition and otherwise unbounded, so quoting it whole would let a rejected
/// launch write an arbitrarily large log line — the exact bloat the cap exists
/// to prevent (`manager/src/routes/settings.rs:29-31`). Counted in
/// **characters**, not bytes, so the excerpt is always a valid `str` boundary;
/// the length the message *reports* is still bytes, because that is the unit
/// the cap is enforced in.
const NAME_EXCERPT_CHARS: usize = 64;

/// First [`NAME_EXCERPT_CHARS`] characters of `name`, marked with `...` when
/// anything was dropped so a reader can tell an excerpt from a whole name.
fn name_excerpt(name: &str) -> String {
    let mut excerpt: String = name.chars().take(NAME_EXCERPT_CHARS).collect();
    if name.chars().nth(NAME_EXCERPT_CHARS).is_some() {
        excerpt.push_str("...");
    }
    excerpt
}

/// Why a parameter set was rejected. Every variant names the offender, which
/// is what `cpt-cf-qa-fr-runs-params` requires of the message.
///
/// **Exactly two variants do not, and both are accounted for here.**
/// [`ParamError::EmptyName`] and [`ParamError::TooMany`] have nothing to name —
/// there is no usable name in the first case and no single offender in the
/// second — so they carry the counts instead. Same content and shape as the
/// source system's two messages, not the same string: legacy says
/// `"Variable name cannot be empty"` (`settings.rs:66`) and
/// `"Too many parameters: {} (max {})"` (`settings.rs:122-125`), where these
/// say `parameter` rather than `Variable` and start lowercase.
///
/// [`ParamError::NameTooLong`] was a third, unaccounted exception until spec
/// review found it: it *has* an offender, and a caller submitting fifty
/// parameters could not tell which one was rejected. It now carries a bounded
/// excerpt — see [`NAME_EXCERPT_CHARS`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParamError {
    #[error("parameter name cannot be empty")]
    EmptyName,

    #[error(
        "parameter '{name}' must start with a letter or '_' and contain only letters, \
         digits, or '_'"
    )]
    InvalidName { name: String },

    #[error("parameter '{name}' is reserved by the test runner and cannot be overridden")]
    Reserved { name: String },

    #[error("parameter '{name}' is defined more than once")]
    Duplicate { name: String },

    #[error("too many parameters: {count} (max {max})")]
    TooMany { count: usize, max: usize },

    /// `name` is a bounded [`name_excerpt`], not the whole name: the name is
    /// oversized by definition and otherwise unbounded, so echoing it whole
    /// would make this message its own instance of the spec bloat
    /// [`MAX_NAME_LEN`] exists to prevent. `len` is the **byte** length of the
    /// full name, which is the unit the cap is enforced in.
    #[error("parameter name '{name}' is too long ({len} bytes, max {max})")]
    NameTooLong {
        name: String,
        len: usize,
        max: usize,
    },

    #[error("value for parameter '{name}' is too large ({len} bytes, max {max})")]
    ValueTooLarge {
        name: String,
        len: usize,
        max: usize,
    },
}

/// Whether a name is a legal environment-variable identifier:
/// `^[A-Za-z_][A-Za-z0-9_]*$`.
///
/// Ported character for character from `is_valid_env_var_name`
/// (`manager/src/routes/settings.rs:36-44`). Hand-written rather than a regex —
/// it is two predicates, and the source system writes it the same way.
///
/// The checks are **ASCII-only on purpose**: `is_ascii_alphabetic` rejects
/// `FÖÖ`, which a Unicode-aware `is_alphabetic` would accept and the runner's
/// shell would then not be able to export.
#[must_use]
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Trim names, and drop rows that are blank in *both* fields.
///
/// Ported from `normalize_run_parameters`
/// (`manager/src/routes/settings.rs:180-195`). Three rules:
///
/// * **Names are trimmed; values are not.** A value's leading or trailing
///   space can be meaningful (a padded separator, an intentionally blank
///   suffix), and the source system never touches it.
/// * **A run parameter is never secret.** The source system's shared
///   `PipelineVariable` carries a `secure` flag and this function forces it to
///   `false` (`settings.rs:191`). [`RunParameter`] has no such field, so the
///   rule is expressed in the type rather than in code — which is stronger:
///   there is nothing to force off because there is nothing to set.
/// * **A row blank in both fields is dropped**, so a stray empty row in the UI
///   editor does not fail validation — while a row blank in only one field is
///   *kept*, so it fails loudly rather than being silently discarded. That
///   asymmetry is the whole point of the filter and is easy to "simplify" away.
///
/// Idempotent: trimming a trimmed name is a no-op, and a list with no
/// fully-blank rows loses nothing. See the module docs' composition contract
/// for why that matters at re-run.
#[must_use]
pub fn normalize(parameters: Vec<RunParameter>) -> Vec<RunParameter> {
    parameters
        .into_iter()
        .map(|parameter| RunParameter {
            name: parameter.name.trim().to_owned(),
            value: parameter.value,
        })
        .filter(|parameter| !(parameter.name.is_empty() && parameter.value.is_empty()))
        .collect()
}

/// Validate a normalized parameter set.
///
/// **"Normalized" is a precondition, not a description.** Hand this a raw
/// request list and it rejects sets the source system accepts: a padded
/// `"  FOO  "` fails [`is_valid_name`] on its leading space, and a wholly blank
/// editor row fails as [`ParamError::EmptyName`] instead of being dropped. Call
/// [`normalize`] first and persist what it returns — the module docs'
/// composition contract has the full argument, including why the stored list
/// and not the request's is what dispatch reads.
///
/// **Order is ported, and it is observable** — each pass runs to completion
/// over every parameter before the next begins, so the error a caller sees
/// depends on it:
///
/// 1. The **count** cap, before any per-parameter work
///    (`manager/src/routes/settings.rs:118-127`). Fifty-one oversized
///    parameters therefore report [`ParamError::TooMany`], not the first size
///    violation.
/// 2. The **size** caps, for every parameter (`settings.rs:129-151`). A set
///    whose first entry has an empty name and whose second has an oversized
///    value reports [`ParamError::ValueTooLarge`], because the whole size pass
///    precedes the name pass.
/// 3. The **name** rules, for every parameter — empty, then charset, then
///    reserved, then duplicate. This is the source system's separate
///    `validate_variable_list` (`settings.rs:57-101`), which
///    `validate_run_parameters` calls last (`settings.rs:153`) and which the
///    global pipeline-variables editor also calls; the two-function split is
///    what keeps the two editors' name rules identical.
///
/// Fails closed: nothing here truncates, coerces, or drops. An oversized or
/// unparseable parameter is rejected.
///
/// # Errors
/// [`ParamError`] naming the offending parameter.
pub fn validate(parameters: &[RunParameter]) -> Result<(), ParamError> {
    if parameters.len() > MAX_PARAMETERS {
        return Err(ParamError::TooMany {
            count: parameters.len(),
            max: MAX_PARAMETERS,
        });
    }

    for parameter in parameters {
        // Byte lengths, as the source system measures them: `String::len` on
        // both fields (`settings.rs:130`, `:140`). A multi-byte value is
        // therefore capped by its encoded size, which is what actually bounds
        // the rendered spec — a `chars().count()` cap would let a UTF-8 value
        // reach four times the intended limit.
        if parameter.name.len() > MAX_NAME_LEN {
            return Err(ParamError::NameTooLong {
                name: name_excerpt(&parameter.name),
                len: parameter.name.len(),
                max: MAX_NAME_LEN,
            });
        }
        if parameter.value.len() > MAX_VALUE_LEN {
            return Err(ParamError::ValueTooLarge {
                name: parameter.name.clone(),
                len: parameter.value.len(),
                max: MAX_VALUE_LEN,
            });
        }
    }

    let mut seen = HashSet::new();
    for parameter in parameters {
        // Empty first, with its own message: falling through to the charset
        // check would blame an empty name for its "first character"
        // (`settings.rs:63-68`).
        if parameter.name.is_empty() {
            return Err(ParamError::EmptyName);
        }
        if !is_valid_name(&parameter.name) {
            return Err(ParamError::InvalidName {
                name: parameter.name.clone(),
            });
        }
        // Case-insensitive, so `kubeconfig` is as reserved as `KUBECONFIG`
        // (`settings.rs:80-83`). Environment-variable lookup is case-sensitive
        // in the runner's shell, but the *intent* to shadow a control variable
        // is what is being refused, and a lowercase spelling expresses it just
        // as well.
        if RESERVED_NAMES
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(&parameter.name))
        {
            return Err(ParamError::Reserved {
                name: parameter.name.clone(),
            });
        }
        // Dedupe on the uppercased name, so `Foo` and `FOO` collide
        // (`settings.rs:93-94`). Note this is *stricter* than the environment
        // itself, where the two names are distinct variables that happily
        // coexist — see `domain::env_assembly::assemble`, which matches names
        // verbatim. The pair is deliberate: assembly preserves the source
        // system's exact-match merge, and this check is what stops a launch
        // from ever reaching that state through parameters. Neither half is
        // safe to relax alone.
        if !seen.insert(parameter.name.to_ascii_uppercase()) {
            return Err(ParamError::Duplicate {
                name: parameter.name.clone(),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, value: &str) -> RunParameter {
        RunParameter {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    #[test]
    fn a_valid_set_passes() {
        assert!(validate(&[p("FOO", "1"), p("_bar2", "x")]).is_ok());
        assert!(validate(&[]).is_ok());
    }

    #[test]
    fn an_empty_name_is_rejected() {
        let err = validate(&[p("", "1")]).unwrap_err();
        assert!(matches!(err, ParamError::EmptyName), "got {err:?}");
    }

    #[test]
    fn a_name_must_start_with_a_letter_or_underscore() {
        for bad in ["1FOO", "-FOO", " FOO", "9", "."] {
            let err = validate(&[p(bad, "1")]).unwrap_err();
            assert!(
                matches!(err, ParamError::InvalidName { .. }),
                "{bad} must be rejected on its first character, got {err:?}"
            );
        }
    }

    /// The tail predicate is a separate rule from the first-character one
    /// (`routes/settings.rs:43` versus `:38-41`), and one sweep mixing both
    /// cannot say which half is enforced — half of the previous test's inputs
    /// were in fact exercising this rule. Every input here has a legal first
    /// character, so only the tail check can reject it.
    ///
    /// `F\u{d6}\u{d6}` is `FÖÖ`, escaped because `clippy::non_ascii_literal` is
    /// denied workspace-wide. It belongs in this half because a Unicode-aware
    /// `is_alphabetic` would accept it and the ported ASCII check must not.
    #[test]
    fn a_name_may_contain_only_letters_digits_and_underscores_after_the_first() {
        for bad in ["FOO-BAR", "FOO.BAR", "F\u{d6}\u{d6}", "FOO BAR"] {
            let err = validate(&[p(bad, "1")]).unwrap_err();
            assert!(
                matches!(err, ParamError::InvalidName { .. }),
                "{bad} must be rejected on a tail character, got {err:?}"
            );
        }
    }

    /// The eleven reserved names, rejected case-insensitively. Enumerated in
    /// full rather than sampled: the list is a frozen contract with the runner
    /// (`routes/settings.rs:15-27`, matched at `:80-83`) and a missing entry is
    /// a silent hole.
    ///
    /// This test iterates [`RESERVED_NAMES`], so it cannot police the list's
    /// *contents* — shrink the constant and it still passes. That is
    /// `the_reserved_list_is_the_eleven_legacy_names`'s job, and it writes the
    /// literal.
    #[test]
    fn every_reserved_name_is_rejected_case_insensitively() {
        for reserved in RESERVED_NAMES {
            for spelling in [
                reserved.to_owned(),
                reserved.to_ascii_lowercase(),
                format!("{}{}", &reserved[..1], reserved[1..].to_ascii_lowercase()),
            ] {
                let err = validate(&[p(&spelling, "1")]).unwrap_err();
                assert!(
                    matches!(err, ParamError::Reserved { .. }),
                    "{spelling} must be rejected as reserved, got {err:?}"
                );
            }
        }
    }

    /// Exactly eleven, and exactly these, written as a literal so this test is
    /// not its own oracle. Pins the list itself so a future addition is a
    /// deliberate spec amendment rather than a quiet edit (decision D3).
    ///
    /// The other half of the pair is
    /// `every_reserved_name_is_rejected_case_insensitively`, which iterates
    /// [`RESERVED_NAMES`] and so cannot see a changed list at all — shrinking
    /// the constant to one element leaves it green. Neither test covers the
    /// other: that one proves the rule is *applied*, this one proves the list
    /// is *right*.
    #[test]
    fn the_reserved_list_is_the_eleven_legacy_names() {
        assert_eq!(
            RESERVED_NAMES,
            [
                "APP_BUILD",
                "APP_VERSION",
                "E2E_K8S_NAMESPACE",
                "KUBECONFIG",
                "PRODUCT_KEY",
                "RP_API_KEY",
                "RP_PROJECT",
                "SKIP_TESTS_WITH_BUGS",
                "TEST_BUNDLE_URL",
                "TEST_FILES",
                "TEST_VERSION",
            ]
        );
    }

    /// Duplicates collide on the uppercased name (`routes/settings.rs:93-94`),
    /// so differently-cased spellings of one variable are a conflict.
    #[test]
    fn duplicates_collide_case_insensitively() {
        let err = validate(&[p("FOO", "1"), p("foo", "2")]).unwrap_err();
        assert!(
            matches!(err, ParamError::Duplicate { .. }),
            "FOO and foo must collide on the uppercased key, got {err:?}"
        );
    }

    #[test]
    fn at_most_fifty_parameters_are_accepted() {
        let fifty: Vec<RunParameter> = (0..50).map(|i| p(&format!("P{i}"), "v")).collect();
        assert!(validate(&fifty).is_ok());

        let fifty_one: Vec<RunParameter> = (0..51).map(|i| p(&format!("P{i}"), "v")).collect();
        let err = validate(&fifty_one).unwrap_err();
        assert!(
            matches!(err, ParamError::TooMany { count: 51, .. }),
            "got {err:?}"
        );
    }

    /// The count cap is checked before the per-parameter loop, so an
    /// over-count set of oversized parameters reports "too many" and not the
    /// first size violation (`routes/settings.rs:118-127`).
    #[test]
    fn the_count_cap_is_reported_before_any_size_violation() {
        let huge = "x".repeat(MAX_VALUE_LEN + 1);
        let many_huge: Vec<RunParameter> = (0..51).map(|i| p(&format!("P{i}"), &huge)).collect();
        let err = validate(&many_huge).unwrap_err();
        assert!(
            matches!(err, ParamError::TooMany { .. }),
            "count is checked first, got {err:?}"
        );
    }

    /// The size pass covers *every* parameter before the name pass starts —
    /// the source system runs the size loop to completion (`:129-151`) and only
    /// then calls `validate_variable_list` (`:153`). So an empty name in the
    /// first slot does not pre-empt an oversized value in the second.
    ///
    /// Not in the plan's test list; added because the plan's own implementation
    /// doc claims this two-pass shape and nothing pinned it.
    #[test]
    fn size_violations_are_reported_before_any_name_violation() {
        let huge = "x".repeat(MAX_VALUE_LEN + 1);
        let err = validate(&[p("", "1"), p("BIG", &huge)]).unwrap_err();
        assert!(
            matches!(err, ParamError::ValueTooLarge { .. }),
            "the size pass covers every parameter before the name pass starts, got {err:?}"
        );
    }

    #[test]
    fn a_name_longer_than_128_chars_is_rejected() {
        let ok = "A".repeat(MAX_NAME_LEN);
        assert!(validate(&[p(&ok, "v")]).is_ok());
        let too_long = "A".repeat(MAX_NAME_LEN + 1);
        let err = validate(&[p(&too_long, "v")]).unwrap_err();
        let ParamError::NameTooLong { name, len, max } = &err else {
            panic!("expected NameTooLong, got {err:?}")
        };
        assert_eq!(*len, MAX_NAME_LEN + 1);
        assert_eq!(*max, MAX_NAME_LEN);
        assert!(
            name.starts_with("AAAA") && name.ends_with("..."),
            "the message must name the offender, bounded and marked as an excerpt, got {name:?}"
        );
        assert!(
            name.chars().count() <= NAME_EXCERPT_CHARS + 3,
            "the excerpt must be bounded, got {} chars",
            name.chars().count()
        );
    }

    #[test]
    fn a_value_larger_than_eight_kib_is_rejected() {
        let ok = "x".repeat(MAX_VALUE_LEN);
        assert!(validate(&[p("BIG", &ok)]).is_ok());
        let too_big = "x".repeat(MAX_VALUE_LEN + 1);
        let err = validate(&[p("BIG", &too_big)]).unwrap_err();
        assert!(matches!(err, ParamError::ValueTooLarge { .. }));
    }

    /// Size caps are byte counts, not char counts — a multi-byte value must be
    /// measured the way the source system measures it (`String::len`,
    /// `routes/settings.rs:140`).
    #[test]
    fn value_size_is_measured_in_bytes() {
        // `\u{e9}` is `é`, two bytes in UTF-8. `MAX_VALUE_LEN` of them is
        // exactly `MAX_VALUE_LEN` *characters* and twice `MAX_VALUE_LEN`
        // *bytes*, so a byte-measuring cap rejects it and a char-measuring one
        // accepts it. That is the single thing distinguishing pass from fail.
        let value = "\u{e9}".repeat(MAX_VALUE_LEN);
        let err = validate(&[p("BIG", &value)]).unwrap_err();
        assert!(
            matches!(err, ParamError::ValueTooLarge { .. }),
            "a {}-character, {}-byte value must exceed the byte cap, got {err:?}",
            value.chars().count(),
            value.len()
        );
    }

    /// The **name** cap is a byte count too (`String::len`,
    /// `routes/settings.rs:130`) — the mirror of
    /// `value_size_is_measured_in_bytes`, which was pinned while this one was
    /// not (found by spec review, 2026-08-13).
    ///
    /// It is reachable despite the ASCII-only charset rule precisely because
    /// the size pass runs before the name pass: `MAX_NAME_LEN` copies of a
    /// two-byte character is `MAX_NAME_LEN` *characters* and twice
    /// `MAX_NAME_LEN` *bytes*, so a byte cap answers `NameTooLong` while a char
    /// cap falls through to the charset check and answers `InvalidName`. That
    /// one substitution is the whole difference between pass and fail.
    ///
    /// It matters for the reason `settings.rs:29-31` gives: a char-based cap
    /// would accept a 512-byte name, which is exactly the workflow-spec bloat
    /// the caps exist to prevent.
    #[test]
    fn name_size_is_measured_in_bytes() {
        let name = "\u{e9}".repeat(MAX_NAME_LEN);
        let err = validate(&[p(&name, "v")]).unwrap_err();
        assert!(
            matches!(err, ParamError::NameTooLong { .. }),
            "a {}-character, {}-byte name must exceed the byte cap, got {err:?}",
            name.chars().count(),
            name.len()
        );
    }

    // ---------- normalization ----------

    #[test]
    fn normalization_trims_names_but_not_values() {
        let out = normalize(vec![p("  FOO  ", "  spaced  ")]);
        assert_eq!(out, vec![p("FOO", "  spaced  ")]);
    }

    /// A stray row that is blank in BOTH fields is dropped so an empty UI row
    /// does not fail validation; a row blank in only one is kept so it fails
    /// validation loudly (`routes/settings.rs:193`).
    #[test]
    fn normalization_drops_only_fully_blank_rows() {
        let out = normalize(vec![p("", ""), p("", "orphan-value"), p("FOO", "")]);
        assert_eq!(out, vec![p("", "orphan-value"), p("FOO", "")]);
    }

    /// Validation runs on the normalized list, so trimming is what makes a
    /// padded name acceptable — and what makes an all-whitespace name an
    /// empty-name error rather than a charset error. This is the module docs'
    /// composition obligation 1, exercised.
    #[test]
    fn an_all_whitespace_name_normalizes_to_an_empty_name_error() {
        let out = normalize(vec![p("   ", "v")]);
        let err = validate(&out).unwrap_err();
        assert!(
            matches!(err, ParamError::EmptyName),
            "an all-whitespace name trims to empty, got {err:?}"
        );
    }
}
