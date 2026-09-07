//! Multi-branch working-copy behavior of the gix sync engine.
//!
//! Covers the primitive the design rests on (one `gix` clone materializing
//! two branches' trees into separate plain directories — the original
//! spike, spec §3.1), the real [`RepoSyncPort`] engine driven per branch,
//! the missing-branch failure mode, and concurrent syncs through the same
//! two-tier locks `ReposService` uses. Gated behind `integration` like the
//! sibling gix suite, because it drives the real git transport:
//!
//! ```sh
//! cargo test -p qa-catalog --features integration --test multi_branch
//! ```
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

// `domain` and `infra` are `pub(crate)` (review finding #38); everything this
// test needs is re-exported at the crate root, named there one item at a time.
use qa_catalog::{GixSyncEngine, RepoSyncPort, branch_workdir, host_dir};

/// Build a local fixture repo with two branches holding different content.
/// Returns the repo path. Uses the `git` CLI (already required by the
/// sibling integration suite) purely to *author* the fixture.
fn fixture_repo(root: &Path) -> PathBuf {
    let repo = root.join("origin");
    std::fs::create_dir_all(&repo).unwrap();

    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .expect("git must be on PATH");
        assert!(status.success(), "git {args:?} failed");
    };

    git(&["init", "--initial-branch=main", "--quiet"]);
    git(&["config", "user.email", "spike@example.test"]);
    git(&["config", "user.name", "Spike"]);

    std::fs::write(repo.join("marker.txt"), "from-main\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "main content"]);

    git(&["checkout", "-q", "-b", "release/5.0"]);
    std::fs::write(repo.join("marker.txt"), "from-release\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "release content"]);

    git(&["checkout", "-q", "main"]);
    repo
}

/// `file://` URL for the fixture repo, over its **canonicalized** path.
///
/// gix canonicalizes file-URL paths when it writes the remote config (on
/// macOS `/var/...` becomes `/private/var/...`), while the engine's
/// `open_existing` compares the configured URL verbatim — a mismatch reads
/// as "different remote" and correctly discards the working area. Real
/// deployments only ever use http(s) URLs, which gix never rewrites; the
/// fixture must canonicalize so consecutive syncs recognize their own clone.
fn fixture_url(origin: &Path) -> String {
    format!("file://{}", origin.canonicalize().unwrap().display())
}

/// Materialize `branch`'s tree from `repo` into `dest`, WITHOUT writing the
/// repository index. This is the exact operation the production sync engine
/// will perform for each branch.
fn materialize(repo: &gix::Repository, branch: &str, dest: &Path) {
    let interrupt = AtomicBool::new(false);
    std::fs::create_dir_all(dest).unwrap();

    let tracking = format!("refs/remotes/origin/{branch}");
    let commit_id = repo
        .find_reference(tracking.as_str())
        .unwrap()
        .peel_to_id()
        .unwrap()
        .detach();

    let tree_id = repo
        .find_object(commit_id)
        .unwrap()
        .peel_to_tree()
        .unwrap()
        .id;
    let mut index = repo.index_from_tree(&tree_id).unwrap();
    let mut opts = repo
        .checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)
        .unwrap();
    opts.destination_is_initially_empty = true;

    let objects = repo.objects.clone().into_arc().unwrap();
    gix::worktree::state::checkout(
        &mut index,
        dest,
        objects,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &interrupt,
        opts,
    )
    .unwrap();
    // Deliberately NO `index.write(...)` — the index is shared across
    // branches, and these directories are read-only content snapshots.
}

#[test]
fn one_clone_materializes_two_branches_into_separate_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = fixture_repo(tmp.path());
    let url = fixture_url(&origin);

    // One bare host clone owning objects + refs for every branch.
    let host = tmp.path().join("host");
    let interrupt = AtomicBool::new(false);
    let mut prepare = gix::prepare_clone_bare(url.as_str(), &host).unwrap();
    let (repo, _outcome) = prepare
        .fetch_only(gix::progress::Discard, &interrupt)
        .unwrap();

    // Two branches, two destinations, one object store.
    let main_dir = tmp.path().join("branches/main");
    let release_dir = tmp.path().join("branches/release-5-0");
    materialize(&repo, "main", &main_dir);
    materialize(&repo, "release/5.0", &release_dir);

    assert_eq!(
        std::fs::read_to_string(main_dir.join("marker.txt")).unwrap(),
        "from-main\n",
        "main worktree must hold main's content"
    );
    assert_eq!(
        std::fs::read_to_string(release_dir.join("marker.txt")).unwrap(),
        "from-release\n",
        "release worktree must hold release's content, not main's"
    );
    assert!(
        !main_dir.join(".git").exists(),
        "branch worktrees are plain directories, not git worktrees"
    );
}

#[tokio::test]
async fn engine_materializes_each_branch_into_its_own_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = fixture_repo(tmp.path());
    let url = fixture_url(&origin);
    let repos_dir = tmp.path().join("repos");
    let repo_id = uuid::Uuid::new_v4();
    let engine = GixSyncEngine;

    for branch in ["main", "release/5.0"] {
        let result = engine
            .sync(
                &url,
                branch,
                None,
                &host_dir(&repos_dir, repo_id),
                &branch_workdir(&repos_dir, repo_id, branch),
            )
            .await
            .unwrap_or_else(|e| panic!("sync of {branch} failed: {e}"));
        assert!(
            result.branches.iter().any(|b| b == branch),
            "advertised inventory must include {branch}"
        );
    }

    let main_marker = branch_workdir(&repos_dir, repo_id, "main").join("marker.txt");
    let release_marker = branch_workdir(&repos_dir, repo_id, "release/5.0").join("marker.txt");
    assert_eq!(std::fs::read_to_string(main_marker).unwrap(), "from-main\n");
    assert_eq!(
        std::fs::read_to_string(release_marker).unwrap(),
        "from-release\n"
    );
}

#[tokio::test]
async fn syncing_a_branch_absent_from_the_remote_fails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = fixture_repo(tmp.path());
    let url = fixture_url(&origin);
    let repos_dir = tmp.path().join("repos");
    let repo_id = uuid::Uuid::new_v4();

    let err = GixSyncEngine
        .sync(
            &url,
            "no-such-branch",
            None,
            &host_dir(&repos_dir, repo_id),
            &branch_workdir(&repos_dir, repo_id, "no-such-branch"),
        )
        .await
        .expect_err("a missing branch must not succeed");

    assert!(
        err.to_string().contains("not found on the remote"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn concurrent_syncs_of_different_branches_do_not_corrupt_each_other() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = fixture_repo(tmp.path());
    let url = fixture_url(&origin);
    let repos_dir = tmp.path().join("repos");
    let repo_id = uuid::Uuid::new_v4();

    // Serialize through the same two-tier lock the service uses; without the
    // repo tier these concurrent fetches race on the shared object store.
    let cache = std::sync::Arc::new(qa_catalog::SyncCache::new(std::time::Duration::ZERO));

    let mut handles = Vec::new();
    for branch in ["main", "release/5.0", "main", "release/5.0"] {
        let url = url.clone();
        let repos_dir = repos_dir.clone();
        let cache = std::sync::Arc::clone(&cache);
        handles.push(tokio::spawn(async move {
            let repo_lock = cache.repo_lock(repo_id).await;
            let branch_lock = cache.branch_lock(repo_id, branch).await;
            let _repo_guard = repo_lock.lock().await;
            let _branch_guard = branch_lock.lock().await;
            GixSyncEngine
                .sync(
                    &url,
                    branch,
                    None,
                    &host_dir(&repos_dir, repo_id),
                    &branch_workdir(&repos_dir, repo_id, branch),
                )
                .await
                .map(|_| ())
        }));
    }

    for handle in handles {
        handle
            .await
            .expect("task must not panic")
            .expect("every concurrent sync must succeed");
    }

    assert_eq!(
        std::fs::read_to_string(branch_workdir(&repos_dir, repo_id, "main").join("marker.txt"))
            .unwrap(),
        "from-main\n"
    );
    assert_eq!(
        std::fs::read_to_string(
            branch_workdir(&repos_dir, repo_id, "release/5.0").join("marker.txt")
        )
        .unwrap(),
        "from-release\n"
    );
}
