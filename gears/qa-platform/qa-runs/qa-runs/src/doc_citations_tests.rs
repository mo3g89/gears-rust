//! Every identifier this crate's prose cites must exist.
//!
//! # Why this guard exists
//!
//! Repeatedly, a doc comment in this subsystem has named a test or function that
//! is not in the tree — and every instance landed in the sentence whose whole
//! purpose is to let a reader check a claim. A citation is the load-bearing part
//! of "X pins this": if the reader cannot find X, the sentence is worse than
//! silence, because it asserts coverage that does not exist.
//!
//! Nothing else catches them: `cargo clippy --all-targets` is exit 0 on every
//! one of them.
//!
//! # Why not `cargo doc`, and where the two are genuinely complementary
//!
//! `rustdoc`'s `broken_intra_doc_links` covers **intra-doc links on items it
//! documents**, which is not the same set as this crate's citations, for two
//! measured reasons:
//!
//! * **Private items are skipped by default.** Plain `cargo doc -p qa-runs
//!   --no-deps` reports none of the dangling links this subsystem has shipped,
//!   because their enclosing items are private. `--document-private-items`
//!   surfaces them — along with dozens of pre-existing unresolved links that
//!   would have to be triaged before it could gate anything.
//! * **`#[cfg(test)]` modules are not built at all**, so no rustdoc setting
//!   reaches them. Measured rather than assumed: a deliberately dangling link
//!   planted inside a `#[cfg(test)]` module is **not reported at all** by
//!   `cargo doc --no-deps --document-private-items`, while the same link on a
//!   non-test private item in the same file is reported. Setting
//!   `RUSTDOCFLAGS="--cfg test"` is not a way around it either — with that flag
//!   the documentation build **fails** (`error[E0433]: cannot find module or
//!   crate `tracing_test``), so it reports nothing at all, including the
//!   non-test link it does report without the flag. Citations inside test
//!   modules are among the instances this subsystem has shipped.
//!
//! So rustdoc is the better tool for links on public items, and this test is the
//! only thing that reaches test modules and bare-backtick prose. They are
//! complementary; neither subsumes the other.
//!
//! # What this does not check
//!
//! **File-path citations.** An instance in this subsystem was spelled
//! `` `tests/ingest_races_pg.rs` `` — a path, not an identifier — and this guard
//! would not have caught it. Paths in this crate's prose are rooted in at least
//! three different places (this crate, the SDK crate, and the legacy tree), so
//! "does this file exist" has no single answer to check against. Stated rather
//! than quietly omitted.
//!
//! Nor does it check that a cited function is *relevant* to the sentence citing
//! it — only that it exists.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Citations that deliberately name something the tree does not contain.
///
/// Each entry needs a reason, and the reasons fall into three kinds: the
/// identifier belongs to another codebase, or the prose is *about* the
/// identifier's absence, or it is not a Rust function at all.
///
/// **The list is checked in both directions.** An entry that becomes resolvable,
/// or whose citation is deleted, fails
/// [`every_identifier_this_crates_prose_cites_exists`] as stale — so this cannot
/// silently accumulate dead exemptions, which is the failure mode an allowlist
/// normally has.
const CITATIONS_THAT_INTENTIONALLY_DO_NOT_RESOLVE: &[(&str, &str)] = &[
    // ---- Belong to the legacy VHP Test Runner, which is a separate tree ----
    ("derive_phase_from_result_flags", "legacy `manager/src`"),
    ("next_sequence_number_from_names", "legacy `manager/src`"),
    ("is_valid_env_var_name", "legacy `manager/src`"),
    (
        "persisted_verdict_survives_a_still_green_workflow",
        "legacy `manager/src`",
    ),
    (
        "list_persisted_workflow_names_by_prefix",
        "legacy `manager/src`",
    ),
    // The three parity oracles the Argo adapter's marker parser re-runs. They
    // are the *legacy* tests, cited by name in `infra::executor::argo::markers`
    // so a reader can find the assertion the port's behaviour is measured
    // against; this crate's own copies carry different names because they
    // assert on `TestObservation` rather than on legacy's `TestResult`.
    (
        "parse_test_results_captures_pytest_duration",
        "legacy `manager/src/services/argo.rs:3210`",
    ),
    (
        "parse_test_results_assigns_correct_file_across_multiple_tests",
        "legacy `manager/src/services/argo.rs:3231`",
    ),
    (
        "parse_test_results_keeps_extended_duration_text",
        "legacy `manager/src/services/argo.rs:3286`",
    ),
    // ---- Belongs to the toolkit ----
    (
        "json_array_response_with_schema",
        "`toolkit::api::operation_builder::OperationBuilder`'s method, not this crate's",
    ),
    // ---- Belongs to a sibling gear ----
    (
        "plan_and_test_meta_reads_are_scoped_to_the_callers_tenant",
        "qa-catalog's own tenant-scoping suite",
    ),
    // ---- Belong to `qa-vhp-product-plugin`, cited by `domain::runvars`' test
    //      header as where five of its own tests went at Task 18. Naming them
    //      is the whole point of that paragraph: "the coverage moved" and "the
    //      coverage went" are indistinguishable from a diff of deletions. ----
    (
        "the_base_domain_accompanies_the_base_url",
        "`qa-vhp-product-plugin`'s `run_tests.rs`",
    ),
    (
        "an_unparseable_base_url_yields_the_url_without_a_domain",
        "`qa-vhp-product-plugin`'s `run_tests.rs`",
    ),
    (
        "a_scheme_less_base_url_still_yields_a_bare_domain",
        "`qa-vhp-product-plugin`'s `run_tests.rs`",
    ),
    (
        "blank_observed_attributes_contribute_no_variables",
        "`qa-vhp-product-plugin`'s `run_tests.rs`",
    ),
    (
        "the_observed_namespace_becomes_the_namespace_variable",
        "`qa-vhp-product-plugin`'s `run_tests.rs`",
    ),
    (
        "a_fully_observed_environment_yields_exactly_the_four_variables",
        "`qa-vhp-product-plugin`'s `run_tests.rs`",
    ),
    // ---- The prose is about the identifier NOT existing ----
    (
        "platform_defaults_are_not_available_yet",
        "names a test that was deleted, and says so",
    ),
    (
        "every_tenant_bound_factory_takes_a_checked_tenant",
        "records a guard that was removed as inert, and says so",
    ),
    (
        "the_claim_scan_wraps_and_reaches_a_row_below_its_cursor",
        "quotes an earlier invented identifier as the example of this defect",
    ),
    (
        "an_event_for_an_unknown_run_is_dropped_without_error",
        "names the test the plan asked for, to record diverging from it",
    ),
    // ---- Task 4's spec review renamed three tests; the prose quotes the name
    //      each one shipped under, which is the whole point of the sentence ----
    (
        "list_runs_finished_since_returns_oldest_first_within_the_limit",
        "the plan's name for a sweep test, quoted where the rename is explained",
    ),
    (
        "the_sweep_stops_at_the_limit_it_was_given",
        "the name that test shipped under, quoted where the rename is explained",
    ),
    (
        "the_sweep_is_scoped_to_the_callers_own_tenant",
        "the name that test shipped under, quoted where the rename is explained",
    ),
];

/// Every `.rs` file under this crate's `src/`.
///
/// Walked at runtime rather than listed with `include_str!`, because a list
/// would not cover a file added later — and a new file carrying a bad citation
/// is exactly the case this exists for. `CARGO_MANIFEST_DIR` is baked in at
/// compile time, so this does not depend on the working directory.
fn source_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("the crate's src/ is readable") {
            let path = entry.expect("a readable dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut out,
    );
    out
}

/// The text of every `///` and `//!` line, with the marker stripped.
fn doc_lines(source: &str) -> impl Iterator<Item = &str> {
    source.lines().filter_map(|line| {
        let trimmed = line.trim_start();
        trimmed
            .strip_prefix("///")
            .or_else(|| trimmed.strip_prefix("//!"))
    })
}

/// Every line that is **not** a doc comment or an ordinary comment.
///
/// The complement of [`doc_lines`], and the only thing a citation may resolve
/// against — see [`every_identifier_this_crates_prose_cites_exists`] on why
/// resolving against prose made the guard satisfiable by its own input.
fn code_lines(source: &str) -> impl Iterator<Item = &str> {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
}

/// Is this a bare, lower-snake-case identifier?
fn is_snake_ident(token: &str) -> bool {
    !token.is_empty()
        && token.starts_with(|c: char| c.is_ascii_lowercase())
        && token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Tokens the prose cites, as `(identifier, rule)` pairs.
///
/// # Two rules, and what each deliberately leaves alone
///
/// * **An intra-doc link** — ``[`x`]`` — is an explicit item reference, so an
///   *unqualified* snake-case target must resolve.
///
///   **Qualified targets are not checked, and that is the larger half.** Most
///   link targets in this crate carry a path — ``[`Self::finish`]``,
///   ``[`DomainError::Database`]``, ``[`toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS`]``
///   — and none of them is examined here. Checking one means resolving its final
///   segment against enum variants and struct fields as well as functions, which
///   this file does not model; a resolver that got that wrong would fail the
///   build for correct prose. So a renamed **method** cited as
///   ``[`Self::old_name`]`` passes unnoticed.
///
///   That gap is prospective rather than historical: the dangling links this
///   subsystem has actually shipped were unqualified, which is why the rule
///   catches its own history. It is not evidence that it catches the class.
/// * **A backticked bare token** in prose is only *probably* an identifier, and
///   the threshold of four underscores is a false-positive trade rather than a
///   principle. **What it buys is test-name citations, not function-name ones.**
///   This crate's test names are long sentences; its function names are not —
///   `decide_terminal`, `run_role`, `clamp_up` — so an invented *function* name
///   in bare backticks is under the threshold and passes. Lowering it would
///   start reporting config keys and table names, which are written the same
///   way. `idx_` prefixes are dropped as a category: database index names.
fn citations(source: &str) -> Vec<(String, &'static str)> {
    // Doc lines are joined before scanning, because this crate wraps long
    // identifiers *across* lines inside one backtick pair:
    //
    //     /// `no_production_path_uses_
    //     /// allow_all`
    //
    // Scanning line by line sees two half-identifiers and reports both as
    // dangling. Joining and then stripping whitespace inside the backticks
    // reassembles the real name. Citations in this crate are written that way,
    // and every one of them was a false positive before this.
    let joined: String = doc_lines(source).collect::<Vec<_>>().join("\n");
    let despace = |t: &str| -> String { t.split_whitespace().collect() };

    let mut found = Vec::new();

    // Intra-doc links: [`target`]
    let mut rest = joined.as_str();
    while let Some(open) = rest.find("[`") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("`]") else { break };
        let target = despace(&after[..close]);
        let target = target.trim_end_matches("()");
        if is_snake_ident(target) && target.contains('_') {
            found.push((target.to_owned(), "intra-doc link"));
        }
        rest = &after[close + 2..];
    }

    // Bare backticked tokens: `some_long_test_name`
    for chunk in joined.split('`').skip(1).step_by(2) {
        let token = despace(chunk);
        let token = token.trim_end_matches("()");
        if is_snake_ident(token) && token.matches('_').count() >= 4 && !token.starts_with("idx_") {
            found.push((token.to_owned(), "backticked identifier"));
        }
    }
    found
}

/// Every identifier cited in this crate's prose resolves to something in it.
///
/// Failure means one of two things, and the message says which: a citation names
/// nothing (fix the citation, or add it to
/// [`CITATIONS_THAT_INTENTIONALLY_DO_NOT_RESOLVE`] with a reason), or an
/// exemption has gone stale (delete the exemption).
#[test]
fn every_identifier_this_crates_prose_cites_exists() {
    let files = source_files();
    assert!(
        files.len() > 20,
        "the source walk found {} files, which cannot be right — a broken walk \
         would make this whole test vacuous",
        files.len(),
    );

    let sources: Vec<(PathBuf, String)> = files
        .into_iter()
        .map(|p| {
            let text = fs::read_to_string(&p).expect("a source file is readable");
            (p, text)
        })
        .collect();

    // Everything the crate *declares* that a citation could legitimately mean.
    //
    // **Read from code lines only, which is load-bearing.** An earlier version
    // scanned the raw file text, so any line containing the word `fn ` followed
    // by an identifier satisfied a citation -- including the doc comment doing
    // the citing. Planting `` `a_totally_invented_name_here` `` went red, and
    // adding a prose line reading "the fn a_totally_invented_name_here does the
    // thing" turned it green again. Prose quoting a signature is common in this
    // crate, and a commented-out `// fn old_name(..)` does it too, so the guard
    // could be satisfied by the very text it is supposed to check.
    //
    // `code_lines` is the complement of `doc_lines`, minus ordinary `//`
    // comments. It is a line filter, not a parser: a `fn` inside a string
    // literal still counts, which is the conservative direction (it can only
    // make the guard accept, never falsely reject) and is not closed here.
    let code: Vec<String> = sources
        .iter()
        .map(|(_, text)| code_lines(text).collect::<Vec<_>>().join("\n"))
        .collect();
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for text in &code {
        for keyword in ["fn ", "mod ", "struct ", "enum ", "trait ", "const "] {
            for chunk in text.match_indices(keyword) {
                let tail = &text[chunk.0 + keyword.len()..];
                let name: &str = tail
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or("");
                if !name.is_empty() {
                    declared.insert(name);
                }
            }
        }
    }

    let exempt: BTreeSet<&str> = CITATIONS_THAT_INTENTIONALLY_DO_NOT_RESOLVE
        .iter()
        .map(|(name, _)| *name)
        .collect();

    // **Backtick parity, asserted rather than assumed.** `citations` pairs
    // backticks positionally, so a single unbalanced one -- or a ``` fence,
    // which contributes three -- inverts the pairing for the whole rest of the
    // file and silently flips every later citation from checked to unchecked.
    // That is the false-negative direction, and nothing else would notice it.
    // Latent today: no file is unbalanced. This turns the day one becomes
    // unbalanced into a loud failure instead of a quiet loss of coverage.
    let mut unbalanced: Vec<String> = Vec::new();
    for (path, text) in &sources {
        let ticks = doc_lines(text)
            .map(|line| line.matches('`').count())
            .sum::<usize>();
        if ticks % 2 != 0 {
            let file = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(path)
                .display();
            unbalanced.push(format!("{file}: {ticks} backticks in doc comments"));
        }
    }
    assert!(
        unbalanced.is_empty(),
        "doc comments with an odd number of backticks -- every citation after \
         the imbalance is silently unchecked:\n  {}",
        unbalanced.join("\n  "),
    );

    let mut dangling: Vec<String> = Vec::new();
    let mut exemptions_used: BTreeSet<&str> = BTreeSet::new();

    for (path, text) in &sources {
        for (token, rule) in citations(text) {
            if declared.contains(token.as_str()) {
                continue;
            }
            if let Some(hit) = exempt.get(token.as_str()) {
                exemptions_used.insert(hit);
                continue;
            }
            let file = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(path)
                .display();
            // Keyed without the rule, so one token matching both rules is one
            // finding rather than two -- a four-underscore dangling link
            // otherwise reported "2 identifiers" for a single mistake.
            let _ = rule;
            dangling.push(format!("{file}: `{token}` resolves to nothing"));
        }
    }
    dangling.sort();
    dangling.dedup();
    assert!(
        dangling.is_empty(),
        "prose cites {} identifier(s) that do not exist:\n  {}\n\nFix the \
         citation, or — if the prose is deliberately naming something absent — \
         add it to CITATIONS_THAT_INTENTIONALLY_DO_NOT_RESOLVE with a reason.",
        dangling.len(),
        dangling.join("\n  "),
    );

    // The other direction: no exemption may outlive its reason.
    for (name, reason) in CITATIONS_THAT_INTENTIONALLY_DO_NOT_RESOLVE {
        assert!(
            !declared.contains(name),
            "`{name}` is exempt as \"{reason}\", but the crate now declares it — \
             delete the exemption",
        );
        assert!(
            exemptions_used.contains(name),
            "`{name}` is exempt as \"{reason}\", but nothing cites it any more — \
             delete the exemption",
        );
    }
}
