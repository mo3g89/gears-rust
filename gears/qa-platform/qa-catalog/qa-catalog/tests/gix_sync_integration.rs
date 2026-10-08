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
// `domain` and `infra` are `pub(crate)` (review finding #38); everything this
// test needs is re-exported at the crate root, named there one item at a time.
use qa_catalog::{
    DomainError, GixSyncEngine, RepoSyncPort, SyncLimits, branch_workdir, host_dir, parse_plan_yaml,
};

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

    let engine = GixSyncEngine::default();
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

    let engine = GixSyncEngine::default();
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

/// DESIGN §3.3's failure split through the real transport: a remote that answers `401` is a
/// refused credential, whether none or a wrong one was configured, and a closed
/// port is an outage. (Over this plain `http://` endpoint the wrong one is
/// refused by gix itself, which never sends a credential in clear text; the
/// `401` after a credential was sent is pinned by the adapter's unit tests.) A local endpoint answering every request `401` stands in
/// for a git host rejecting a token. Bounded: it serves at most eight
/// connections, then its thread ends.
#[tokio::test]
async fn ls_refs_tells_a_refused_credential_from_an_unreachable_remote() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"git\"\r\n\
                  Content-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    let url = format!("http://127.0.0.1:{port}/org/repo.git");
    for credential in [None, Some("deploy:wrong-token")] {
        let err = GixSyncEngine::default()
            .list_remote_branches(&url, credential)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::CredentialRejected { .. }),
            "credential {credential:?}: got {err:?}"
        );
    }

    let closed = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let err = GixSyncEngine::default()
        .list_remote_branches(&format!("http://127.0.0.1:{closed}/org/repo.git"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::SyncFailed { .. }), "got {err:?}");
}

/// A remote that accepts the connection and never answers: the listing gives
/// up at its deadline instead of holding the read. Finite: the endpoint holds
/// at most four connections for at most 15 s, then its thread ends — past the
/// assertion's 10 s, so a listing without its deadline fails it, and short of
/// reqwest's 30 s stall bound, so the detached listing thread the runtime
/// waits for at shutdown ends with the endpoint.
#[tokio::test]
async fn a_listing_against_a_remote_that_never_answers_ends_at_its_deadline() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut held = Vec::new();
        while std::time::Instant::now() < until {
            match listener.accept() {
                Ok((stream, _)) if held.len() < 4 => held.push(stream),
                Ok(_) => {}
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        drop(held);
    });
    let engine = GixSyncEngine::new(SyncLimits {
        ls_refs_timeout: std::time::Duration::from_secs(1),
        ..SyncLimits::default()
    });
    let started = std::time::Instant::now();
    let err = engine
        .list_remote_branches(&format!("http://127.0.0.1:{port}/org/repo.git"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::RemoteTimedOut { .. }),
        "got {err:?}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

/// Pseudo-random printable text that deflate cannot shrink much: a pack of
/// this blob stays near its size. Finite and deterministic (an LCG).
fn incompressible(len: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            ALPHABET[(state >> 58) as usize] as char
        })
        .collect()
}

/// A fixture whose `main` carries one 1 MiB blob, against 64 KiB budgets.
fn oversized_fixture(dir: &Path) -> String {
    let repo = gix::init(dir).unwrap();
    let big = incompressible(1 << 20);
    commit_tree(
        &repo,
        "main",
        &[("data/big.txt", big.as_str()), ("plan.yaml", PLAN_YAML)],
        None,
        "big",
    );
    dir.to_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_clone_whose_pack_exceeds_the_budget_is_refused_and_leaves_nothing_behind() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let url = oversized_fixture(fixture_dir.path());
    let repos_root = tempfile::tempdir().unwrap();
    let repo_id = uuid::Uuid::new_v4();
    let host = host_dir(repos_root.path(), repo_id);
    let workdir = branch_workdir(repos_root.path(), repo_id, "main");
    let engine = GixSyncEngine::new(SyncLimits {
        max_fetch_bytes: 64 << 10,
        ..SyncLimits::default()
    });

    let err = engine
        .sync(&url, "main", None, &host, &workdir)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::SyncBudgetExceeded { .. }),
        "got {err:?}"
    );
    assert!(
        !host.parent().unwrap().exists(),
        "the oversized clone is removed"
    );
}

#[tokio::test]
async fn a_branch_whose_checkout_exceeds_the_budget_is_refused_before_anything_is_written() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let url = oversized_fixture(fixture_dir.path());
    let repos_root = tempfile::tempdir().unwrap();
    let repo_id = uuid::Uuid::new_v4();
    let host = host_dir(repos_root.path(), repo_id);
    let workdir = branch_workdir(repos_root.path(), repo_id, "main");
    let engine = GixSyncEngine::new(SyncLimits {
        max_checkout_bytes: 64 << 10,
        ..SyncLimits::default()
    });

    let err = engine
        .sync(&url, "main", None, &host, &workdir)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::SyncBudgetExceeded { .. }),
        "got {err:?}"
    );
    assert!(!workdir.exists(), "no part of the snapshot was written");
}
