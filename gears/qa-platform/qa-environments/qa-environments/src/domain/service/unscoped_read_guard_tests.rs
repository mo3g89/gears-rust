//! **`AccessScope::allow_all()` appears in exactly one production path, and
//! it is a ratified exception, not a convenience.**
//!
//! # Where this ban comes from
//!
//! `qa-runs`' own `no_production_path_uses_allow_all` (`domain::service::tenant_scoping_tests`)
//! records the origin: *"a probe using `allow_all()` created a cross-tenant
//! existence oracle in qa-environments"* — this crate. That defect is why
//! `domain::service::variables`'s module header and `domain::repos::variables_repo`
//! (`variables_repo.rs:36`) both spell out, independently, that no code path in
//! this crate may query with `AccessScope::allow_all()` off of unauthenticated
//! or unauthorized *input*. This test is the textual enforcement of that ban,
//! modelled directly on `qa-runs`' version (same walk, same comment-skipping,
//! same `_tests.rs` self-exemption rule) — copied rather than reinvented,
//! because a second crate re-deriving the same guard from scratch is exactly
//! how the shape would drift.
//!
//! # Why this crate's version is not a flat zero, unlike `qa-runs`'
//!
//! `qa-runs` has no legitimate use for an unscoped read and tolerates none.
//! This crate has exactly one, ratified 2026-08-28 by the product owner after
//! Task 8's review flagged the original, undocumented use of `allow_all()` at
//! the observation ticker's enumeration call site
//! (`EnvironmentsService::run_observation_cycle`, `domain/service/environments.rs`):
//! a system actor with no caller, enumerating every environment across every
//! tenant on this gear's own maintenance schedule, where the alternative
//! (`qa-runs`'/`qa-insights`' nil-tenant-context-through-`PolicyEnforcer`
//! pattern) is `Forbidden` by design under both shipped `AuthZ` plugins with no
//! policy authored for this gear's system actor — which would leave the ticker
//! silently observing nothing on every deployment that exists today. The full
//! ratification (subject, reason, scope, and the prior incident it knowingly
//! stands near) is recorded at the call site itself, not just here — see that
//! comment for the complete argument.
//!
//! So this module enforces a **narrower** claim than `qa-runs`' flat ban: not
//! "zero", but "exactly the one ratified call site, and nothing else, ever."
//! A second call anywhere — including a second one in the very same file —
//! fails this test exactly as a first one appearing anywhere else would.
//!
//! # What it catches, and what it does not
//!
//! Textual, exactly like `qa-runs`' version: it catches the direct call
//! (`allow_all` appearing outside a comment, outside a `_tests.rs` file). It
//! does **not** catch an alias, a re-export under another name, a scope built
//! field-by-field to be equivalent, or a call assembled from string fragments.
//! It is a tripwire on the obvious spelling, not a proof.
//!
//! This file itself is named `unscoped_read_guard_tests.rs` — ending in
//! `_tests.rs`, the one exemption the walk grants — precisely so it can name
//! the banned call as many times as this documentation needs to without
//! reporting itself.

use std::path::{Path, PathBuf};

/// The one file this crate's ban permits `AccessScope::allow_all()` in, and
/// the only one. Not a directory, not a module, not a pattern — a single
/// path, so that adding a second file that also calls it (rather than a
/// second call in this same file) is caught exactly as loudly.
const RATIFIED_EXCEPTION_FILE: &str = "src/domain/service/environments.rs";

/// **`AccessScope::allow_all()` appears in no production path except the one
/// ratified ticker-enumeration call site.**
///
/// See the module doc for the full argument; this test's job is only to keep
/// the claim true as the crate changes under it.
#[test]
fn no_production_path_uses_allow_all_except_the_one_ratified_ticker_enumeration() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<(PathBuf, usize)> = Vec::new();
    walk(&src, &mut |path, contents| {
        // Whole-file test modules are exempt - including this one, which
        // names the banned call several times in its own documentation.
        // Files that merely *contain* a `#[cfg(test)] mod tests` are **not**
        // exempt, so a test written inline in a production file would still
        // be flagged - matches `qa-runs`' rule exactly, for the reason its
        // own doc gives: an exemption should be earned by the whole file
        // being a test module, not by four letters of its name.
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.ends_with("_tests.rs") {
            return;
        }
        for (number, line) in contents.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if line.contains("allow_all") {
                offenders.push((path.to_path_buf(), number + 1));
            }
        }
    });

    let ratified_path = Path::new(RATIFIED_EXCEPTION_FILE);

    let unexpected: Vec<String> = offenders
        .iter()
        .filter(|(path, _)| !path.ends_with(ratified_path))
        .map(|(path, line)| format!("{}:{line}", path.display()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "AccessScope::allow_all() must not appear in a production path outside the one \
         ratified exception ({RATIFIED_EXCEPTION_FILE}): {unexpected:?}"
    );

    let ratified_hits: Vec<String> = offenders
        .iter()
        .filter(|(path, _)| path.ends_with(ratified_path))
        .map(|(path, line)| format!("{}:{line}", path.display()))
        .collect();
    assert_eq!(
        ratified_hits.len(),
        1,
        "the ratified exception must be exactly one call site in {RATIFIED_EXCEPTION_FILE} - \
         zero means it moved or was renamed (update RATIFIED_EXCEPTION_FILE or this test), and \
         more than one means an unratified second use snuck in beside the ratified one: \
         {ratified_hits:?}"
    );
}

/// Recurse over `.rs` files under `dir`.
///
/// A hand-rolled walk rather than a dependency, copied from `qa-runs`' own
/// version for the same reason it gives: `walkdir` is not a dev-dependency of
/// this crate and adding one for six lines would be a worse trade than the
/// six lines.
fn walk(dir: &Path, visit: &mut impl FnMut(&Path, &str)) {
    let entries = std::fs::read_dir(dir).expect("the crate's own src/ must be readable");
    for entry in entries {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            walk(&path, visit);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let contents = std::fs::read_to_string(&path).expect("a readable source file");
            visit(&path, &contents);
        }
    }
}
