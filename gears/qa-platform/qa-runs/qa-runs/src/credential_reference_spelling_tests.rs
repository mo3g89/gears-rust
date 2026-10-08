//! No tracked file spells a credential reference with a scheme prefix, except
//! the files that test or describe its refusal.
//!
//! A credential reference is a bare name — letters, digits, `_` and `-`, 1 to
//! 255 characters, what `credstore_sdk::SecretRef::new` accepts — in every gear
//! (DESIGN §3.5 "Egress"; UPGRADING "Credential references have one spelling in
//! every gear"). A fixture spelling `credstore://name` or `cred://name` passes
//! only because the path it exercises never validates the reference, so it
//! tests a value no deployment can hold, and it teaches the refused spelling to
//! whoever copies it. The allowed files below each refuse or document the
//! prefixed shape. A file on the list that no longer contains either spelling
//! fails too, so the list cannot keep a dead entry.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const SCANNED: [&str; 2] = ["gears/qa-platform", "apps/cf-gears-example-server"];
const PREFIXES: [&str; 2] = ["credstore://", "cred://"];

/// Files that refuse a prefixed reference in a test, describe the refusal, or
/// rewrite stored prefixed values (the migration). Repository-relative.
const ALLOWED: &[&str] = &[
    "gears/qa-platform/qa-runs/qa-runs/src/credential_reference_spelling_tests.rs",
    "gears/qa-platform/docs/openapi.json",
    "gears/qa-platform/docs/DESIGN.md",
    "gears/qa-platform/deploy/helm/qa-platform/UPGRADING.md",
    "gears/qa-platform/qa-platform-ui/src/api/generated/openapi.d.ts",
    "gears/qa-platform/qa-platform-ui/src/api/adapters.ts",
    "gears/qa-platform/qa-platform-ui/src/lib/credstoreRef.test.ts",
    "gears/qa-platform/qa-environments/qa-environments/src/domain/service/environments.rs",
    "gears/qa-platform/qa-environments/qa-environments/src/domain/service/environments_kubeconfig_tests.rs",
    "gears/qa-platform/qa-environments/qa-environments/src/domain/service/environments_credentials_tests.rs",
    "gears/qa-platform/qa-insights/qa-insights/src/api/rest/routes/settings.rs",
    "gears/qa-platform/qa-insights/qa-insights/src/domain/ports/jira_client.rs",
    "gears/qa-platform/qa-insights/qa-insights/src/domain/ports/jira_client_tests.rs",
    "gears/qa-platform/qa-insights/qa-insights/src/domain/service/jira.rs",
    "gears/qa-platform/qa-insights/qa-insights/src/domain/service/jira_tests.rs",
    "gears/qa-platform/qa-insights/qa-insights/src/domain/service/notify_tests.rs",
    "gears/qa-platform/qa-insights/qa-insights/src/infra/jira/oagw_client_tests.rs",
    "gears/qa-platform/qa-insights/qa-insights/src/infra/storage/migrations/m20261007_000008_bare_credstore_refs.rs",
    "gears/qa-platform/qa-insights/qa-insights/src/infra/storage/migrations/mod.rs",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("qa-runs/qa-runs sits four directories below the repository root")
        .to_path_buf()
}

/// Tracked paths under [`SCANNED`], repository-relative. A `git` that cannot
/// run is a panic naming the cause, never an empty list.
fn tracked(repo: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["ls-files", "-z", "--"])
        .args(SCANNED)
        .current_dir(repo)
        .output()
        .expect("`git` must be runnable: this guard lists tracked files with `git ls-files`");
    assert!(
        out.status.success(),
        "`git ls-files` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

#[test]
fn no_tracked_file_outside_the_refusal_tests_spells_a_prefixed_reference() {
    let repo = repo_root();
    let mut offenders = Vec::new();
    let mut spelling_files = BTreeSet::new();
    for path in tracked(&repo) {
        let Ok(text) = fs::read_to_string(repo.join(&path)) else {
            continue; // binary
        };
        for (n, line) in text.lines().enumerate() {
            if PREFIXES.iter().any(|p| line.contains(p)) {
                spelling_files.insert(path.clone());
                if !ALLOWED.contains(&path.as_str()) {
                    offenders.push(format!("{path}:{}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a credential reference is a bare name; these spell a scheme prefix outside the \
         files that test its refusal:\n{}",
        offenders.join("\n")
    );
    let dead: Vec<_> = ALLOWED
        .iter()
        .filter(|p| !spelling_files.contains(**p))
        .collect();
    assert!(
        dead.is_empty(),
        "allowed but no longer spelling a prefix; delete: {dead:?}"
    );
}
