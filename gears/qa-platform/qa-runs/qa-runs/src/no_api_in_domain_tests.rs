//! **No module under `src/domain` may name `crate::api`.**
//!
//! The dependency runs the other way: `api` is a transport over `domain`. Two
//! local clients imported their error attribution from `api::rest::error`,
//! which the plugin rework made more visible by moving those clients from
//! `infra/` into `domain/`, and a third domain module reached into
//! `api::rest::sse` for a write-side byte cap. A local client is an in-process
//! call that never touches HTTP, and an archive truncation budget is not an
//! SSE framing decision — neither may take its rules from the transport layer.
//! Review findings #15, #16, #39.
//!
//! A structural scan rather than a `cargo gears lint` rule: that CLI runs a
//! fixed catalogue of lints from the external `cargo-gears-lints` crate, and
//! `Gears.toml`/`dylint.toml` can only enable, skip or parametrize what that
//! catalogue already ships. Its two nearest lints, `DE0301 no_infra_in_domain`
//! and `DE0308 no_http_in_domain`, both match on *foreign crates* (`sea_orm`,
//! `sqlx`, `axum`, `hyper`, `http`) reaching a domain module; neither can
//! express "this crate's own `api` module". So the rule is pinned here, in the
//! shape this crate already uses for its other structural guards
//! (`doc_citations_tests`, `file_citations_tests`), and in all four gears of
//! the subsystem rather than only the ones that had an offender — a gate
//! installed in one crate leaves the other three unguarded.
//!
//! # What is checked
//!
//! Every `.rs` file under this crate's `src/domain`, with **comments removed
//! first**. Prose legitimately cites the transport layer — qa-insights'
//! `domain::analytics` alone carries a dozen `[crate::api::rest::dto::..]`
//! intra-doc links — and a guard that flagged those would be switched off
//! within a week. Only what is still there once the comments are gone counts.
//!
//! Two forms are recognised, on whitespace-stripped code: `crate::api` (the
//! plain `use` and the inline path alike) and `crate::{api` (the brace form).
//! Each file is checked line by line *and* as one joined string, so a `use`
//! tree that rustfmt broke across lines cannot fall between two of them.
//!
//! # Limits, stated rather than discovered
//!
//! * A `//` inside a string literal truncates that line's code, so a
//!   `crate::api` written *after* one on the same line is invisible here.
//!   Nothing in this subsystem is written that way, and the alternative is a
//!   Rust lexer.
//! * Block comments are **not** stripped, so a `/* */`-commented import reads
//!   as code and fails the test. That is the safe direction: a false alarm is
//!   read, a false clearance is not.
//! * Relative forms (`super::super::api`) are not recognised at all. Every
//!   import in this subsystem is crate-absolute, and a guard that tried to
//!   resolve `super` chains would need to know each file's module depth.

use std::fs;
use std::path::{Path, PathBuf};

/// Files under `src/domain` that may name `crate::api`, each with its reason.
///
/// **Named files, never a pattern.** A `*_tests.rs` glob would exempt every
/// test this crate ever adds from the rule this file installs, which is how a
/// guard becomes decoration. An entry here is a decision someone took once and
/// wrote down, and
/// [`every_allowance_is_still_earning_it`](self::every_allowance_is_still_earning_it)
/// fails as soon as one stops being true.
const ALLOWED: &[(&str, &str)] = &[];

/// This crate's `src/domain`, derived from its own location.
fn domain_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("domain")
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("src/domain must be readable") {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// One source line with its `//` comment tail and all whitespace removed.
///
/// Whitespace goes too, so `crate :: api` — which rustfmt does not produce but
/// a hand edit could — is recognised on the same terms as `crate::api`.
fn code_only(line: &str) -> String {
    line.split("//")
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// Whether whitespace-stripped code names this crate's `api` module.
fn names_the_api_layer(code: &str) -> bool {
    code.contains("crate::api") || code.contains("crate::{api")
}

/// A path relative to the crate root, in the form the [`ALLOWED`] table uses.
fn crate_relative(path: &Path) -> PathBuf {
    path.strip_prefix(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .expect("every scanned file is under this crate")
        .to_owned()
}

/// **No `domain` module imports the `api` layer.**
///
/// Review findings #15, #16, #39 — see this module's header for the rule and
/// for what "imports" is taken to mean.
#[test]
fn no_domain_module_imports_the_api_layer() {
    let root = domain_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "scanned {} and found no Rust files, so this guard would pass vacuously",
        root.display()
    );

    let mut offenders = Vec::new();
    for file in &files {
        let relative = crate_relative(file);
        if ALLOWED
            .iter()
            .any(|(allowed, _)| Path::new(allowed) == relative)
        {
            continue;
        }
        let source = fs::read_to_string(file).expect("a readable source file");
        let mut joined = String::new();
        let mut flagged = false;
        for (index, line) in source.lines().enumerate() {
            let code = code_only(line);
            if names_the_api_layer(&code) {
                offenders.push(format!(
                    "{}:{}: {}",
                    relative.display(),
                    index + 1,
                    line.trim()
                ));
                flagged = true;
            }
            joined.push_str(&code);
        }
        if !flagged && names_the_api_layer(&joined) {
            offenders.push(format!(
                "{}: a `use` tree spanning several lines names `crate::api`",
                relative.display()
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "a domain module must not import the api layer -- api is a transport \
         over domain, not the other way round. Move what it needs into domain \
         and re-export it from api. Offenders:\n{}",
        offenders.join("\n")
    );
}

/// An allowance outlives its cause silently: the import goes away, the entry
/// stays, and the next file that wants one finds a precedent waiting. So every
/// [`ALLOWED`] entry must still name a file that exists and still contains
/// what it exempts.
#[test]
fn every_allowance_is_still_earning_it() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut stale = Vec::new();
    for (allowed, reason) in ALLOWED {
        assert!(
            !reason.is_empty(),
            "{allowed} is allowed without a reason, which is an exemption nobody can review"
        );
        let path = manifest.join(allowed);
        let Ok(source) = fs::read_to_string(&path) else {
            stale.push(format!("{allowed}: no such file"));
            continue;
        };
        let still_needs_it = source
            .lines()
            .any(|line| names_the_api_layer(&code_only(line)));
        if !still_needs_it {
            stale.push(format!("{allowed}: no longer names `crate::api`"));
        }
    }
    assert!(
        stale.is_empty(),
        "remove these allowances, they no longer exempt anything:\n{}",
        stale.join("\n")
    );
}
