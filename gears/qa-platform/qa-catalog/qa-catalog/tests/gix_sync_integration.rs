//! End-to-end integration test for the gix sync engine (Task 9, ADR-0005):
//! builds a local fixture git repository *with gix itself*, then drives the
//! real [`RepoSyncPort`] implementation through the real transport —
//! clone, re-fetch, branch listing, and plan discovery via the plan.yaml
//! parser.
//!
//! Gated behind the `integration` cargo feature (mirroring the workspace's
//! integration-suite gating) because the local `file://`-style transport
//! spawns `git upload-pack`, i.e. it needs a `git` binary on PATH:
//!
//! ```sh
//! cargo test -p qa-catalog --features integration --test gix_sync_integration
//! ```
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use gix::bstr::BString;
use qa_catalog::domain::error::DomainError;
use qa_catalog::domain::parsing::plan_yaml::parse_plan_yaml;
use qa_catalog::domain::ports::repo_sync::RepoSyncPort;
use qa_catalog::infra::git::GixSyncEngine;
use qa_catalog::infra::git::layout::{branch_workdir, host_dir};

const PLAN_YAML: &str = "name: smoke\ntags: [ci]\ntests:\n  - tests/test_login.py\n";
const PLAN_YAML_V2: &str = "name: smoke\ntags: [ci, nightly]\ntests:\n  - tests/test_login.py\n";

/// Signature for fixture commits (gix requires explicit committer/author).
fn signature() -> gix::actor::SignatureRef<'static> {
    gix::actor::SignatureRef {
        name: "qa-catalog-test".into(),
        email: "qa-catalog-test@example.com".into(),
        time: "1700000000 +0000",
    }
}

/// Write `files` as one commit on `refs/heads/<branch>` of the fixture
/// repository, replacing the whole tree. Nested paths are supported one
/// directory level deep (enough for the `plans/` discovery layout).
fn commit_tree(
    repo: &gix::Repository,
    branch: &str,
    files: &[(&str, &str)],
    parent: Option<gix::ObjectId>,
    message: &str,
) -> gix::ObjectId {
    use gix::objs::tree::{Entry, EntryKind};

    // Group files into root entries and single-level subdirectories.
    let mut dirs: std::collections::BTreeMap<&str, Vec<(&str, &str)>> =
        std::collections::BTreeMap::default();
    let mut root_files: Vec<(&str, &str)> = Vec::new();
    for (path, content) in files {
        match path.split_once('/') {
            Some((dir, rest)) => dirs.entry(dir).or_default().push((rest, content)),
            None => root_files.push((path, content)),
        }
    }

    let blob_entry = |name: &str, content: &str| Entry {
        mode: EntryKind::Blob.into(),
        filename: BString::from(name),
        oid: repo.write_blob(content).unwrap().detach(),
    };

    let mut entries: Vec<Entry> = Vec::new();
    for (dir, dir_files) in &dirs {
        let sub_entries: Vec<Entry> = dir_files
            .iter()
            .map(|(name, content)| blob_entry(name, content))
            .collect();
        let sub_tree = sorted_tree(sub_entries);
        entries.push(Entry {
            mode: EntryKind::Tree.into(),
            filename: BString::from(*dir),
            oid: repo.write_object(&sub_tree).unwrap().detach(),
        });
    }
    for (name, content) in &root_files {
        entries.push(blob_entry(name, content));
    }
    let tree_id = repo.write_object(sorted_tree(entries)).unwrap().detach();

    repo.commit_as(
        signature(),
        signature(),
        format!("refs/heads/{branch}"),
        message,
        tree_id,
        parent,
    )
    .unwrap()
    .detach()
}

/// Order entries the way git tree objects require (directories sort as if
/// their name had a trailing `/`).
fn sorted_tree(mut entries: Vec<gix::objs::tree::Entry>) -> gix::objs::Tree {
    entries.sort();
    gix::objs::Tree { entries }
}

/// Init a non-bare fixture repository with `main` (plan layout + README)
/// and a `feature` branch pointing at the same commit. Returns the repo
/// and the `main` tip.
fn fixture_repo(dir: &Path) -> (gix::Repository, gix::ObjectId) {
    let repo = gix::init(dir).unwrap();
    let main_tip = commit_tree(
        &repo,
        "main",
        &[
            ("plans/smoke.yaml", PLAN_YAML),
            ("plan.yaml", PLAN_YAML),
            ("README.md", "fixture\n"),
        ],
        None,
        "initial",
    );
    repo.reference(
        "refs/heads/feature",
        main_tip,
        gix::refs::transaction::PreviousValue::Any,
        "fixture branch",
    )
    .unwrap();
    (repo, main_tip)
}

#[tokio::test]
async fn sync_clone_fetch_and_discover_plans_end_to_end() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let (fixture, main_tip) = fixture_repo(fixture_dir.path());
    let url = fixture_dir.path().to_str().unwrap().to_owned();

    let engine = GixSyncEngine;
    let repos_root = tempfile::tempdir().unwrap();
    let repo_id = uuid::Uuid::new_v4();
    let host = host_dir(repos_root.path(), repo_id);
    let workdir = branch_workdir(repos_root.path(), repo_id, "main");

    // --- Clone path: first sync materializes the branch snapshot. ----------
    let result = engine
        .sync(&url, "main", None, &host, &workdir)
        .await
        .unwrap();
    assert_eq!(result.branches, vec!["feature", "main"]);
    assert_eq!(result.head_commit, main_tip.to_string());
    assert_eq!(
        std::fs::read_to_string(workdir.join("plans/smoke.yaml")).unwrap(),
        PLAN_YAML
    );
    assert!(workdir.join("README.md").is_file());

    // Discovery layer: the synced plan parses like PlansService would.
    let parsed = parse_plan_yaml(&std::fs::read_to_string(workdir.join("plan.yaml")).unwrap())
        .expect("synced plan.yaml must parse");
    assert_eq!(parsed.name, "smoke");
    assert_eq!(parsed.test_files, vec!["tests/test_login.py"]);

    // --- Fetch path: second sync picks up new history and prunes files. ----
    let second_tip = commit_tree(
        &fixture,
        "main",
        &[
            ("plans/smoke.yaml", PLAN_YAML_V2),
            ("plan.yaml", PLAN_YAML_V2),
            // README.md removed upstream — the re-sync must drop it.
        ],
        Some(main_tip),
        "update plans",
    );

    let result = engine
        .sync(&url, "main", None, &host, &workdir)
        .await
        .unwrap();
    assert_eq!(result.branches, vec!["feature", "main"]);
    assert_eq!(result.head_commit, second_tip.to_string());
    assert_eq!(
        std::fs::read_to_string(workdir.join("plans/smoke.yaml")).unwrap(),
        PLAN_YAML_V2
    );
    assert!(
        !workdir.join("README.md").exists(),
        "files deleted upstream must not survive a re-sync"
    );

    // --- Branch inventory without a working copy (ls-refs). ----------------
    let branches = engine.list_remote_branches(&url, None).await.unwrap();
    assert_eq!(branches, vec!["feature", "main"]);

    // --- Unknown branch fails cleanly through the fetch path. --------------
    let err = engine
        .sync(
            &url,
            "does-not-exist",
            None,
            &host,
            &branch_workdir(repos_root.path(), repo_id, "does-not-exist"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::SyncFailed { message } if message.contains("does-not-exist")),
        "unexpected error: {err}"
    );
}

/// Unknown branch through the **clone** path (no host clone yet). The bare
/// clone fetches whatever the remote advertises, and `sync` then checks the
/// requested branch against that advertised inventory before materializing
/// anything. Without that check, a nonexistent branch would either error
/// confusingly deep in materialization or — worse — silently serve another
/// branch's content under the requested branch's name. The assertion
/// deliberately pins the *behavior* (an error, and no snapshot content)
/// rather than any error wording.
#[tokio::test]
async fn clone_of_an_unknown_branch_fails_instead_of_falling_back() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let (_fixture, _main_tip) = fixture_repo(fixture_dir.path());
    let url = fixture_dir.path().to_str().unwrap().to_owned();

    let engine = GixSyncEngine;
    let repos_root = tempfile::tempdir().unwrap();
    // Never synced: `sync` takes the clone path, not the fetch path.
    let repo_id = uuid::Uuid::new_v4();
    let workdir = branch_workdir(repos_root.path(), repo_id, "does-not-exist");

    let err = engine
        .sync(
            &url,
            "does-not-exist",
            None,
            &host_dir(repos_root.path(), repo_id),
            &workdir,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(&err, DomainError::SyncFailed { .. }),
        "a clone of a nonexistent branch must fail as SyncFailed, got {err:?}"
    );
    assert!(
        !workdir.join("plan.yaml").is_file(),
        "no content may be checked out for a branch that does not exist"
    );
}
