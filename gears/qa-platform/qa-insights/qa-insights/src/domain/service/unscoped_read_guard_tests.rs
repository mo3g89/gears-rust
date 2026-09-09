//! **`AccessScope::allow_all()` appears in exactly one production path.**
//!
//! # Where this ban comes from
//!
//! `qa-runs`' own `no_production_path_uses_allow_all`
//! (`domain::service::tenant_scoping_tests`) is the original: a textual walk
//! of the crate's `src/` that fails if `allow_all` appears outside a comment
//! anywhere except the one named seam this gear elevates through. This
//! module is the same guard for qa-insights, which — like qa-runs, and
//! unlike qa-environments — has no second, ratified call site:
//! `for_ticker_enumeration` reads through
//! `domain::elevated::enumeration_scope`, and that is the only place
//! `AccessScope::allow_all()` may appear in this crate's production code.
//! Every write this gear performs, including the reconcile sweep's and the
//! ingest path's, scopes with `AccessScope::for_tenant(tenant)` instead.
//!
//! # What it catches, and what it does not
//!
//! Textual, exactly like qa-runs' version: it catches the direct call
//! (`allow_all` appearing outside a comment, outside a `_tests.rs` file, and
//! outside `domain/elevated.rs`). It does not catch an alias, a re-export
//! under another name, a scope built field-by-field to be equivalent, or a
//! call assembled from string fragments. It is a tripwire on the obvious
//! spelling, not a proof.
//!
//! This file is itself named `unscoped_read_guard_tests.rs` — ending in
//! `_tests.rs`, so the walk exempts it — precisely so it can name the banned
//! call as many times as this documentation needs to without reporting
//! itself. `domain/service/test_support.rs` (`#[cfg(test)]`-gated) needs no
//! exemption of its own: unlike qa-catalog's sibling file, it builds its test
//! scopes some other way and never spells `allow_all`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

/// **`AccessScope::allow_all()` appears in no production path in this crate
/// except `domain::elevated::enumeration_scope` itself.**
///
/// See the module doc for the full argument; this test's job is only to keep
/// the claim true as the crate changes under it.
#[test]
fn no_production_path_uses_allow_all() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    walk(&src, &mut |path, contents| {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        // Whole-file test modules are exempt — including this one, which
        // names the banned call several times in its own documentation.
        // `domain/elevated.rs` is exempt too, and only that one path — see
        // the module doc for why this is the crate's one legitimate call.
        if name.ends_with("_tests.rs") || path.ends_with("domain/elevated.rs") {
            return;
        }
        for (number, line) in contents.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if line.contains("allow_all") {
                offenders.push(format!("{}:{}", path.display(), number + 1));
            }
        }
    });

    assert!(
        offenders.is_empty(),
        "AccessScope::allow_all() must not appear in a production path: {offenders:?}"
    );
}

/// Recurse over `.rs` files under `dir`.
///
/// A hand-rolled walk rather than a dependency, matching qa-runs' and
/// qa-environments' identical helper: `walkdir` is not a dev-dependency of
/// this crate and adding one for six lines would be a worse trade than the
/// six lines.
fn walk(dir: &Path, visit: &mut impl FnMut(&Path, &str)) {
    let entries = std::fs::read_dir(dir).expect("the crate's own src/ must be readable");
    for entry in entries {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            walk(&path, visit);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let contents =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        visit(&path, &contents);
    }
}
