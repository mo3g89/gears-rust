#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit tests for `PlansService`: discovery over fixture branch snapshots
//! (skip-and-log on invalid files), `TEST_META` path attachment, the
//! unsynced/missing-snapshot guard, and path-traversal rejection.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use uuid::Uuid;

use super::plans::PlansService;
use super::test_support::{
    MockTestReposRepository, PermissiveAuthZ, ctx, repo_fixture, test_db_provider,
};
use crate::domain::error::DomainError;
use crate::domain::parsing::plan_yaml::DEFAULT_TIMEOUT_SECONDS;

const VALID_PLAN_A: &str = "name: smoke\ntests:\n  - tests/test_a.py\n";
const VALID_PLAN_B: &str =
    "name: upgrade\ntags: [e2e]\nexclusive: true\ntests:\n  - tests/test_b.py\n";
const BROKEN_PLAN: &str = "name: broken\ntests: {not-a-list: true\n";
/// A test file the on-disk derivation must keep (it carries a `TEST_META` block).
const META: &str = "TEST_META = {'title': 'T'}\n\ndef test_x():\n    pass\n";

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
}

async fn build_service(
    repos: Arc<MockTestReposRepository>,
    repos_dir: PathBuf,
) -> PlansService<MockTestReposRepository> {
    let enforcer = PolicyEnforcer::new(Arc::new(PermissiveAuthZ));
    let db = test_db_provider().await;
    PlansService::new(db, repos, repos_dir, enforcer)
}

/// Whether mode bits actually deny access on this host.
///
/// Task 11 needs a reliable non-`NotFound` IO error (`PermissionDenied` on a
/// `chmod 000` file) to prove an unreadable-but-present file is `Internal`,
/// not "absent". Root cannot be denied by mode bits, so tests that need this
/// probe first and skip rather than looking up the uid: creating the file,
/// attempting the read, and checking whether it *succeeded* tests the
/// property the test actually depends on ("can mode bits deny this
/// process?"), not a proxy for it, and needs no `libc`/`nix` dependency.
fn permissions_are_enforced() -> bool {
    let probe = tempfile::NamedTempFile::new().unwrap();
    std::fs::set_permissions(
        probe.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    let enforced = std::fs::read(probe.path()).is_err();
    if !enforced {
        // Silent skips here would make a root CI run report e.g. "8/8
        // passed" while only 4 of the 8 permission-gated tests actually ran
        // their EACCES path -- say so on stderr so that isn't mistaken for
        // full coverage.
        eprintln!(
            "permissions_are_enforced: mode bits did not deny access (running as root?) \
             -- skipping permission-gated test"
        );
    }
    enforced
}

/// Restores a path's permissions on drop, including through a panicking
/// assertion. Without this, a test that `chmod 000`s a directory and then
/// panics on `unwrap_err()`/`assert!` (as one of these did during this
/// task's own RED run) leaves an undeletable directory behind for
/// `TempDir::drop` to trip over.
struct RestorePermsOnDrop<'a> {
    path: &'a Path,
    mode: u32,
}

impl Drop for RestorePermsOnDrop<'_> {
    fn drop(&mut self) {
        // Best-effort: there is nothing sensible to do with a failure inside
        // a destructor, and this only ever runs in tests.
        drop(std::fs::set_permissions(
            self.path,
            std::os::unix::fs::PermissionsExt::from_mode(self.mode),
        ));
    }
}

/// Tempdir + synced repo fixture whose `main` branch snapshot exists under
/// it (content reads resolve `<repos_dir>/<repo_id>/branches/<branch_dir>`).
fn synced_fixture(repo_id: Uuid) -> (tempfile::TempDir, Arc<MockTestReposRepository>, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let workdir = crate::infra::git::layout::branch_workdir(tmp.path(), repo_id, "main");
    std::fs::create_dir_all(&workdir).unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    (tmp, repos, workdir)
}

#[tokio::test]
async fn list_plans_parses_fixtures_and_skips_invalid() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    // 2 valid + 1 broken in the file-based `plans/` layout → 2 plans; the
    // broken one is skipped without hiding the rest.
    write(&workdir, "plans/smoke.yaml", VALID_PLAN_A);
    write(&workdir, "plans/upgrade.yaml", VALID_PLAN_B);
    write(&workdir, "plans/broken.yaml", BROKEN_PLAN);

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let mut plans = svc
        .list_plans(&ctx(tenant_id), repo_id, "main")
        .await
        .unwrap();
    plans.sort_by(|a, b| a.path.cmp(&b.path));

    assert_eq!(plans.len(), 2, "the broken plan must be skipped, not fatal");
    assert_eq!(plans[0].path, "plans/smoke.yaml");
    assert_eq!(plans[0].name, "smoke");
    assert_eq!(plans[0].repo_id, repo_id);
    assert_eq!(plans[0].branch, "main");
    assert_eq!(
        plans[0].exclusive, None,
        "absent exclusive stays None (inherit)"
    );
    assert!(
        !plans[0].validation,
        "a plan with neither the bool nor a `validation` tag is not a validation run"
    );
    assert_eq!(plans[1].path, "plans/upgrade.yaml");
    assert_eq!(plans[1].exclusive, Some(true));
    assert_eq!(plans[1].tags, vec!["e2e".to_owned()]);
}

#[tokio::test]
async fn list_plans_discovers_legacy_directory_plans() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    // Legacy layout: `<dir>/plan.yaml`, and `<dir>/<subdir>/plan.yaml` only
    // when the parent has no plan.yaml of its own.
    write(&workdir, "infra/plan.yaml", VALID_PLAN_A);
    write(&workdir, "product/upgrade/plan.yaml", VALID_PLAN_B);
    // A subdir under a dir that HAS a plan.yaml must NOT be scanned.
    write(&workdir, "infra/nested/plan.yaml", VALID_PLAN_B);

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let mut paths: Vec<String> = svc
        .list_plans(&ctx(tenant_id), repo_id, "main")
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.path)
        .collect();
    paths.sort();

    assert_eq!(
        paths,
        vec![
            "infra/plan.yaml".to_owned(),
            "product/upgrade/plan.yaml".to_owned()
        ],
        "depth-2 is only scanned when depth-1 has no plan.yaml (legacy behavior)"
    );
}

/// Legacy `load_plan_from_dir` (testrunner `manager/src/services/plans.rs:685-692`):
/// a *directory* plan whose `plan.yaml` omits `tests:` derives its file list
/// by walking `<plan_dir>/tests/**`, keeping `test_*.py` files whose contents
/// contain `TEST_META` (legacy `list_test_files` :724-729 →
/// `collect_tests_recursively` :760/:763, repo mode).
#[tokio::test]
async fn directory_plan_without_tests_derives_its_files_from_disk() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(&workdir, "infra/plan.yaml", "name: infra\n");
    write(&workdir, "infra/tests/test_b.py", META);
    write(&workdir, "infra/tests/nested/test_a.py", META);
    // Excluded: wrong name prefix, wrong extension, and no TEST_META block.
    write(&workdir, "infra/tests/helper.py", META);
    write(&workdir, "infra/tests/test_notes.txt", META);
    write(
        &workdir,
        "infra/tests/test_plain.py",
        "def test_x():\n    pass\n",
    );

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let plans = svc
        .list_plans(&ctx(tenant_id), repo_id, "main")
        .await
        .unwrap();

    assert_eq!(plans.len(), 1);
    assert_eq!(
        plans[0].test_files,
        vec![
            "infra/tests/nested/test_a.py".to_owned(),
            "infra/tests/test_b.py".to_owned(),
        ],
        "derived list must be `test_*.py` + TEST_META only, sorted, prefixed with the plan dir"
    );
}

/// Same legacy rule, second branch: a plan that *does* list `tests:` keeps its
/// own list verbatim (normalized) — no disk walk (legacy plans.rs:693-699).
#[tokio::test]
async fn directory_plan_with_tests_keeps_its_own_list() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(&workdir, "infra/plan.yaml", VALID_PLAN_A);
    write(&workdir, "infra/tests/test_ignored.py", META);

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let plans = svc
        .list_plans(&ctx(tenant_id), repo_id, "main")
        .await
        .unwrap();

    assert_eq!(
        plans[0].test_files,
        vec!["tests/test_a.py".to_owned()],
        "an explicit list must win over the on-disk derivation"
    );
}

/// Legacy `scan_file_based_plans` (plans.rs:501-589) derives nothing: a
/// `plans/*.yaml` document with no `tests:` is still returned, with an empty
/// file list. It must not vanish from discovery.
#[tokio::test]
async fn file_based_plan_without_tests_is_returned_with_an_empty_list() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(&workdir, "plans/bare.yaml", "name: bare\n");
    // A sibling `tests/` tree must NOT be picked up for the file-based flavor.
    write(&workdir, "tests/test_a.py", META);

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let plans = svc
        .list_plans(&ctx(tenant_id), repo_id, "main")
        .await
        .unwrap();

    assert_eq!(
        plans.len(),
        1,
        "a plan without `tests:` must still be listed"
    );
    assert_eq!(plans[0].path, "plans/bare.yaml");
    assert!(
        plans[0].test_files.is_empty(),
        "file-based plans derive no files (legacy parity), got {:?}",
        plans[0].test_files
    );
}

/// `plan.yaml` omitting `timeout_seconds` must carry the legacy 300s ceiling
/// all the way out to the SDK model.
#[tokio::test]
async fn discovered_plan_carries_the_legacy_timeout_default() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);
    write(&workdir, "plans/smoke.yaml", VALID_PLAN_A);

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let plans = svc
        .list_plans(&ctx(tenant_id), repo_id, "main")
        .await
        .unwrap();

    assert_eq!(
        plans[0].timeout_seconds,
        Some(DEFAULT_TIMEOUT_SECONDS),
        "a plan legacy ran with a 5-minute ceiling must not report 'no timeout'"
    );
}

/// The `validation` classification must survive discovery out to the SDK
/// model — this is the seam qa-runs reads. Legacy ORs the `validation:` bool
/// with a `validation` tag (`manager/src/services/plans.rs:35-39`); the tag
/// form is used here so the test covers the OR, not just the bool.
#[tokio::test]
async fn discovered_plan_carries_the_validation_flag() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);
    write(
        &workdir,
        "plans/smoke.yaml",
        "name: v\ntags: [validation]\ntests:\n  - tests/test_a.py\n",
    );

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let plans = svc
        .list_plans(&ctx(tenant_id), repo_id, "main")
        .await
        .unwrap();

    assert!(
        plans[0].validation,
        "a `validation` tag must reach the SDK model, not stop at the parser"
    );
}

/// Discovery attributes every plan to its repository's owning product —
/// legacy `enrich_plan_products`' repo-ownership path (the `tests_folder`
/// path is deliberately not ported; see the module docs on legacy-truth).
#[tokio::test]
async fn discovered_plans_carry_the_owning_product() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let product_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let workdir = crate::infra::git::layout::branch_workdir(tmp.path(), repo_id, "main");
    std::fs::create_dir_all(&workdir).unwrap();

    let repo = qa_catalog_sdk::TestRepository {
        product_id,
        ..repo_fixture(repo_id, true)
    };
    let repos = Arc::new(MockTestReposRepository::with_repo(repo));

    write(&workdir, "plans/smoke.yaml", VALID_PLAN_A);
    write(&workdir, "plans/upgrade.yaml", VALID_PLAN_B);

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let plans = svc
        .list_plans(&ctx(tenant_id), repo_id, "main")
        .await
        .unwrap();

    assert!(
        !plans.is_empty(),
        "the fixture must actually discover plans, or this test proves nothing"
    );
    assert!(
        plans.iter().all(|p| p.product_id == product_id),
        "every discovered plan must carry its repository's product id: {plans:?}"
    );
}

/// Legacy sorts the merged plan list (`plans.rs:117`, by plan id) and
/// de-duplicates across the two flavors (`scan_root`, plans.rs:470-499).
/// Discovery here sorts by *path* (this gear's plan identity) and keeps the
/// first of a duplicate pair — file-based before directory, legacy's order.
#[tokio::test]
async fn discovery_is_ordered_and_deduplicated() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(&workdir, "plans/zulu.yaml", VALID_PLAN_A);
    write(&workdir, "plans/alpha.yaml", VALID_PLAN_B);
    write(&workdir, "mike/plan.yaml", VALID_PLAN_A);
    // `<root>/plans/plan.yaml` is reachable by BOTH flavors (a `.yaml` file
    // inside `plans/`, and `plan.yaml` inside the `plans/` directory) — the
    // one input that makes de-duplication observable.
    write(&workdir, "plans/plan.yaml", VALID_PLAN_B);

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let paths: Vec<String> = svc
        .list_plans(&ctx(tenant_id), repo_id, "main")
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.path)
        .collect();

    assert_eq!(
        paths,
        vec![
            "mike/plan.yaml".to_owned(),
            "plans/alpha.yaml".to_owned(),
            "plans/plan.yaml".to_owned(),
            "plans/zulu.yaml".to_owned(),
        ],
        "discovery must be sorted by path and free of duplicates on every call"
    );
}

/// Parse-time normalization repairs the shapes legacy repaired, and nothing
/// more: a traversing entry survives normalization and is still rejected by
/// the path validation on the read paths that consume it.
#[tokio::test]
async fn normalized_test_paths_still_fail_traversal_validation() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);
    write(
        &workdir,
        "plans/sketchy.yaml",
        "name: sketchy\ntests:\n  - '/tests/test_a.py'\n  - './../../secret.txt'\n",
    );
    write(&workdir, "tests/test_a.py", META);
    std::fs::write(tmp.path().join("secret.txt"), "secret").unwrap();

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let plan = svc
        .get_plan(&ctx(tenant_id), repo_id, "main", "plans/sketchy.yaml")
        .await
        .unwrap();
    assert_eq!(
        plan.test_files,
        vec!["tests/test_a.py".to_owned(), "../../secret.txt".to_owned()],
        "the leading slash is repaired; the traversal is left intact for the validator"
    );

    // The repaired entry reads fine...
    svc.get_test_meta(
        &ctx(tenant_id),
        repo_id,
        "main",
        &[plan.test_files[0].clone()],
    )
    .await
    .expect("a normalized relative path must be readable");

    // ...the traversing one is still rejected.
    let err = svc
        .get_test_meta(
            &ctx(tenant_id),
            repo_id,
            "main",
            &[plan.test_files[1].clone()],
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { .. }),
        "normalization must not neutralize the traversal check, got {err:?}"
    );
}

#[tokio::test]
async fn list_plans_requires_synced_repo_and_materialized_snapshot() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();

    // Never-synced repo.
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, false,
    )));
    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let err = svc
        .list_plans(&ctx(tenant_id), repo_id, "main")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::RepoNotSynced { .. }),
        "got {err:?}"
    );

    // The repository synced successfully, but this branch was never
    // materialized, so no snapshot directory exists for it. Reading it must
    // fail, not return an empty plan list — an empty list would make a
    // mistyped branch name look like a repository with no tests.
    let (tmp, repos, _workdir) = synced_fixture(repo_id);
    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let err = svc
        .list_plans(&ctx(tenant_id), repo_id, "never-materialized")
        .await
        .expect_err("a branch with no snapshot must not read as empty");
    assert!(
        matches!(err, DomainError::RepoNotSynced { .. }),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// `require_synced` (the branch-agnostic sync gate)
// ---------------------------------------------------------------------------

/// A `TestRepository` that looks successfully synced, with `default_branch`.
fn synced_repo_fixture(default_branch: &str) -> qa_catalog_sdk::TestRepository {
    let now = time::OffsetDateTime::now_utc();
    qa_catalog_sdk::TestRepository {
        id: Uuid::new_v4(),
        product_id: Uuid::new_v4(),
        name: "fixture".to_owned(),
        url: "https://example.test/repo.git".to_owned(),
        default_branch: default_branch.to_owned(),
        content_root: String::new(),
        credential_ref: None,
        last_synced_at: Some(now),
        sync_error: None,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn require_synced_accepts_any_branch_once_the_repo_is_synced() {
    let repo = synced_repo_fixture("main");
    assert!(
        super::plans::require_synced(&repo, "release/5.0").is_ok(),
        "a non-default branch must be readable under the multi-branch model"
    );
}

#[test]
fn require_synced_rejects_a_repo_with_a_sync_error() {
    let mut repo = synced_repo_fixture("main");
    repo.sync_error = Some("boom".to_owned());
    assert!(matches!(
        super::plans::require_synced(&repo, "main"),
        Err(DomainError::RepoNotSynced { .. })
    ));
}

#[test]
fn require_synced_rejects_a_never_synced_repo() {
    let mut repo = synced_repo_fixture("main");
    repo.last_synced_at = None;
    assert!(matches!(
        super::plans::require_synced(&repo, "main"),
        Err(DomainError::RepoNotSynced { .. })
    ));
}

/// A working directory that exists but cannot be resolved is not "never
/// synced". `content_root_dir` mapped every `canonicalize` failure on the
/// workdir or the content root to `RepoNotSynced`, which is right when the
/// path is absent and wrong otherwise. Review finding #26.
///
/// `EACCES` on `canonicalize` needs a directory with its execute bit
/// removed somewhere *inside* the path being resolved (denying traversal),
/// not just on the leaf: stat-ing an entry only needs search permission on
/// its containing directory, not on the entry itself. So this chmods the
/// `branches` directory that contains the branch workdir, not the workdir
/// itself. Skipped when running as root, which cannot be denied this way.
#[test]
fn content_root_dir_reports_internal_not_not_synced_on_a_permission_failure() {
    if !permissions_are_enforced() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo_id = Uuid::new_v4();
    let repo = repo_fixture(repo_id, true);
    let workdir = crate::infra::git::layout::branch_workdir(tmp.path(), repo_id, "main");
    std::fs::create_dir_all(&workdir).unwrap();
    let branches_dir = workdir.parent().unwrap();
    std::fs::set_permissions(
        branches_dir,
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    // Runs on drop even if the assertions below panic, so a failing
    // assertion can never leave a `0o000` directory for `tmp`'s own
    // `TempDir::drop` to trip over.
    let _restore = RestorePermsOnDrop {
        path: branches_dir,
        mode: 0o755,
    };

    let err = super::plans::content_root_dir(tmp.path(), &repo, "main").unwrap_err();

    let DomainError::Internal(message) = &err else {
        panic!("an EACCES resolving the workdir must not read as RepoNotSynced; got {err:?}");
    };
    assert!(
        message.contains("working directory exists but could not be resolved"),
        "expected the working-directory message, got {message:?}"
    );
}

/// The other half: a genuinely never-materialized branch is still
/// `RepoNotSynced`.
#[test]
fn content_root_dir_still_reports_not_synced_when_genuinely_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_id = Uuid::new_v4();
    let repo = repo_fixture(repo_id, true);
    // No workdir created at all: this branch was never materialized.
    let err = super::plans::content_root_dir(tmp.path(), &repo, "main").unwrap_err();
    assert!(
        matches!(err, DomainError::RepoNotSynced { .. }),
        "got {err:?}"
    );
}

/// A path that cannot be resolved (EACCES on a directory in the middle of
/// it) is not the same as a path that does not exist. `resolve_under_root`
/// answered both with `Ok(None)`, which callers turn into a 404 (`get_plan`,
/// `get_test_meta`) or a silently dropped exclusivity vote
/// (`walk_repo_universe`). Review finding #8.
///
/// Skipped when running as root, which cannot be denied by mode bits.
#[test]
fn resolve_under_root_reports_internal_not_absent_on_a_permission_failure() {
    if !permissions_are_enforced() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let locked_dir = root.join("locked");
    std::fs::create_dir(&locked_dir).unwrap();
    std::fs::write(locked_dir.join("file.py"), "pass\n").unwrap();
    std::fs::set_permissions(
        &locked_dir,
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    // Runs on drop even if the assertions below panic, so a failing
    // assertion can never leave a `0o000` directory for `tmp`'s own
    // `TempDir::drop` to trip over.
    let _restore = RestorePermsOnDrop {
        path: &locked_dir,
        mode: 0o755,
    };

    let err = super::plans::resolve_under_root(root, "locked/file.py").unwrap_err();

    let DomainError::Internal(message) = &err else {
        panic!("an EACCES resolving a path must not silently read as absent; got {err:?}");
    };
    assert!(
        message.contains("could not be resolved"),
        "expected the path-resolution message, got {message:?}"
    );
}

/// The other half: a genuinely missing path still resolves to `Ok(None)`.
/// Without this, the fix above could regress every "does not exist" answer
/// into an `Internal` error and still pass.
#[test]
fn resolve_under_root_still_reports_ok_none_when_genuinely_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let resolved = super::plans::resolve_under_root(tmp.path(), "nope.py").unwrap();
    assert_eq!(
        resolved, None,
        "a genuinely missing path must stay Ok(None)"
    );
}

#[tokio::test]
async fn get_plan_returns_single_plan_by_path() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);
    write(&workdir, "plans/smoke.yaml", VALID_PLAN_A);

    let svc = build_service(repos, tmp.path().to_path_buf()).await;

    let plan = svc
        .get_plan(&ctx(tenant_id), repo_id, "main", "plans/smoke.yaml")
        .await
        .unwrap();
    assert_eq!(plan.name, "smoke");
    assert_eq!(plan.path, "plans/smoke.yaml");

    let err = svc
        .get_plan(&ctx(tenant_id), repo_id, "main", "plans/missing.yaml")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PlanNotFound { .. }),
        "got {err:?}"
    );
}

/// A file that exists but cannot be read is not a missing plan.
///
/// `get_plan` mapped every `read_to_string` failure to `PlanNotFound`, which
/// the REST layer renders 404. An operator whose snapshot directory has
/// wrong permissions was told the plan does not exist, and went looking in
/// the repository instead of at the filesystem. `domain::service::repos`
/// already draws this line correctly at `:297` and `:336`; this file did
/// not. Review finding #6.
///
/// Skipped when running as root, which cannot be denied by mode bits.
#[tokio::test]
async fn an_unreadable_plan_file_is_internal_not_not_found() {
    if !permissions_are_enforced() {
        return;
    }
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);
    write(&workdir, "plans/locked.yaml", "name: x\n");
    std::fs::set_permissions(
        workdir.join("plans/locked.yaml"),
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let err = svc
        .get_plan(&ctx(tenant_id), repo_id, "main", "plans/locked.yaml")
        .await
        .unwrap_err();

    // Not just `Internal`: pin the message to `get_plan`'s own site so this
    // test cannot pass on an `Internal` raised by some other call inside it
    // (e.g. `resolve_under_root`).
    let DomainError::Internal(message) = &err else {
        panic!("an EACCES on an existing plan must not be PlanNotFound; got {err:?}");
    };
    assert!(
        message.contains("could not be read"),
        "expected get_plan's read-failure message, got {message:?}"
    );
}

/// The other half: a genuinely absent plan is still a 404. Without this,
/// the fix above could regress every 404 into a 500 and still pass.
#[tokio::test]
async fn a_genuinely_absent_plan_is_still_not_found() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, _workdir) = synced_fixture(repo_id);
    let svc = build_service(repos, tmp.path().to_path_buf()).await;

    let err = svc
        .get_plan(&ctx(tenant_id), repo_id, "main", "plans/nope.yaml")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PlanNotFound { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn get_test_meta_attaches_paths() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(
        &workdir,
        "tests/test_exclusive.py",
        "TEST_META = {\n    'title': 'Exclusive test',\n    'tags': ['ha'],\n    'exclusive': True,\n    'bugs': ['VHP-123'],\n}\n",
    );
    write(
        &workdir,
        "tests/test_plain.py",
        "def test_plain():\n    pass\n",
    );

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let files = vec![
        "tests/test_exclusive.py".to_owned(),
        "tests/test_plain.py".to_owned(),
    ];
    let metas = svc
        .get_test_meta(&ctx(tenant_id), repo_id, "main", &files)
        .await
        .unwrap();

    assert_eq!(metas.len(), 2);
    assert_eq!(
        metas[0].path, "tests/test_exclusive.py",
        "meta order must follow the requested file order"
    );
    assert_eq!(metas[0].title.as_deref(), Some("Exclusive test"));
    assert_eq!(metas[0].tags, vec!["ha".to_owned()]);
    assert_eq!(metas[0].exclusive, Some(true));
    assert_eq!(metas[0].bugs, vec!["VHP-123".to_owned()]);
    assert_eq!(metas[1].path, "tests/test_plain.py");
    assert_eq!(metas[1].exclusive, None);
}

#[tokio::test]
async fn get_test_meta_rejects_path_traversal() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);
    write(&workdir, "tests/test_a.py", "pass\n");
    // A real file OUTSIDE the workdir that traversal would reach.
    std::fs::write(tmp.path().join("secret.txt"), "secret").unwrap();

    let svc = build_service(repos, tmp.path().to_path_buf()).await;

    for escape in ["../secret.txt", "/etc/passwd", "tests/../../secret.txt"] {
        let err = svc
            .get_test_meta(&ctx(tenant_id), repo_id, "main", &[escape.to_owned()])
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "expected traversal rejection for {escape:?}, got {err:?}"
        );
    }
}

/// A file that exists but cannot be read is not a missing test file.
///
/// `get_test_meta` mapped every `read_to_string` failure to `FileNotFound`,
/// silently dropping the file's exclusivity vote instead of surfacing the
/// permission problem. Review finding #7.
///
/// Skipped when running as root, which cannot be denied by mode bits.
#[tokio::test]
async fn an_unreadable_test_file_is_internal_not_file_not_found() {
    if !permissions_are_enforced() {
        return;
    }
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);
    write(&workdir, "tests/test_locked.py", META);
    std::fs::set_permissions(
        workdir.join("tests/test_locked.py"),
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let err = svc
        .get_test_meta(
            &ctx(tenant_id),
            repo_id,
            "main",
            &["tests/test_locked.py".to_owned()],
        )
        .await
        .unwrap_err();

    // Not just `Internal`: pin the message to `get_test_meta`'s own site so
    // this test cannot pass on an `Internal` raised by some other call
    // inside it (e.g. `resolve_under_root`).
    let DomainError::Internal(message) = &err else {
        panic!("an EACCES on an existing test file must not be FileNotFound; got {err:?}");
    };
    assert!(
        message.contains("could not be read"),
        "expected get_test_meta's read-failure message, got {message:?}"
    );
}

/// The other half: a genuinely absent test file is still `FileNotFound`.
#[tokio::test]
async fn a_genuinely_absent_test_file_is_still_file_not_found() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, _workdir) = synced_fixture(repo_id);
    let svc = build_service(repos, tmp.path().to_path_buf()).await;

    let err = svc
        .get_test_meta(
            &ctx(tenant_id),
            repo_id,
            "main",
            &["tests/nope.py".to_owned()],
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::FileNotFound { .. }),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// `list_universe` — the analytics universe projection (qa-insights Task 7)
// ---------------------------------------------------------------------------

/// A plan whose tags must union into every one of its files' tags
/// (legacy `manager/src/routes/analytics.rs:891-897`).
const UNIVERSE_PLAN: &str = "\
name: infra suite
tags: [nightly, e2e]
tests:
  - tests/storage/test_failover_paths.py
";

/// A test file declaring **every** `TEST_META` key the projection reads, with
/// values chosen so nothing can pass by accident:
///
/// * `component` is `storage-engine`, deliberately different from the
///   `storage` a path inference would produce, so the test proves the
///   declared value wins rather than the inferred one;
/// * `quality_vectors` holds a case-duplicate, so the case-folded dedup is
///   exercised;
/// * the body carries two pytest functions **and** one Playwright case, so a
///   counter that lost either ecosystem gives a different number.
const UNIVERSE_TEST_FILE: &str = "\
TEST_META = {
    'title': 'Storage failover',
    'component': 'storage-engine',
    'tags': ['destructive', 'smoke'],
    'quality_vectors': ['Security', 'security', 'Resilience'],
}

def test_alpha():
    pass

async def test_beta():
    pass

test('playwright case', async () => {});
";

/// **The silent-drop guard for this task.**
///
/// `UniverseTest` is a twelve-field projection assembled inside a file walk.
/// Nothing fails to compile if the builder forgets a field — the analytics
/// that consumes it (Phase B) just degrades: a dropped `title_alias` turns
/// every execution row that names a test by its human title into `not_run`
/// (`analytics.rs:1738`), a dropped `quality_vectors` empties the overview's
/// quality-vector summary, a dropped `static_case_count` under-reports
/// `case_expected`.
///
/// So this asserts the **whole struct** rather than a field at a time: any
/// field the builder stops populating reddens it. Every value is distinct and
/// non-default except `versions`, which is empty here because it is empty in
/// legacy too — see `UniverseTest::versions` for the citations. That is
/// asserted, not skipped: a builder that started inventing versions would
/// fail this test as well.
#[tokio::test]
async fn list_universe_projects_every_field_of_a_fully_populated_test_file() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(&workdir, "infra/plan.yaml", UNIVERSE_PLAN);
    write(
        &workdir,
        "tests/storage/test_failover_paths.py",
        UNIVERSE_TEST_FILE,
    );

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let universe = svc
        .list_universe(&ctx(tenant_id), None, Some("main"))
        .await
        .unwrap();

    assert_eq!(universe.len(), 1, "one plan listing one file");
    assert_eq!(
        universe[0],
        qa_catalog_sdk::UniverseTest {
            repo_id,
            plan_path: "infra/plan.yaml".to_owned(),
            plan_name: "infra suite".to_owned(),
            test_file: "tests/storage/test_failover_paths.py".to_owned(),
            test_name: "Storage failover".to_owned(),
            title_alias: Some("Storage failover".to_owned()),
            // The declared component, NOT the `storage` the path implies.
            component: Some("storage-engine".to_owned()),
            // TEST_META tags unioned with the plan's, sorted (BTreeSet).
            tags: vec![
                "destructive".to_owned(),
                "e2e".to_owned(),
                "nightly".to_owned(),
                "smoke".to_owned(),
            ],
            // Case-folded dedup keeps the first spelling: no second "security".
            quality_vectors: vec!["Security".to_owned(), "Resilience".to_owned()],
            source: qa_catalog_sdk::SOURCE_REPO.to_owned(),
            // Legacy parity: never populated there either (analytics.rs:918).
            versions: vec![],
            // 2 pytest + 1 playwright — both ecosystems, or this is 2.
            static_case_count: 3,
        },
        "every projected field must survive the walk"
    );
}

/// No `TEST_META` title and no declared component: the display name falls
/// back to the file stem (legacy `fallback_test_name`, `analytics.rs:1805-1812`)
/// and the component is inferred from `tests/<component>/...`
/// (`infer_component_from_path`, `analytics.rs:1794-1803`).
///
/// `title_alias` must still be `Some` — legacy defaults it to the display name
/// (`analytics.rs:909`), and that default is what keeps stem-named execution
/// rows matching.
#[tokio::test]
async fn a_file_without_meta_falls_back_to_stem_name_and_inferred_component() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(
        &workdir,
        "infra/plan.yaml",
        "name: infra\ntests:\n  - tests/network/test_failover_paths.py\n",
    );
    write(
        &workdir,
        "tests/network/test_failover_paths.py",
        "def test_a():\n    pass\n",
    );

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let universe = svc
        .list_universe(&ctx(tenant_id), None, Some("main"))
        .await
        .unwrap();

    assert_eq!(universe.len(), 1);
    assert_eq!(
        universe[0].test_name, "failover paths",
        "strip the test_ prefix, underscores become spaces"
    );
    assert_eq!(
        universe[0].title_alias.as_deref(),
        Some("failover paths"),
        "legacy defaults the alias to the display name, never None"
    );
    assert_eq!(
        universe[0].component.as_deref(),
        Some("network"),
        "inferred from the tests/<component>/ path"
    );
    assert!(universe[0].quality_vectors.is_empty());
    assert_eq!(universe[0].static_case_count, 1);
}

/// Legacy keys its universe map on `(source, repo_id, test_file)`
/// (`analytics.rs:899-903`), so one file listed by two plans is **one** row
/// carrying the FIRST plan's identity — and the second occurrence still
/// contributes the metadata the first one lacked (`analytics.rs:922-934`).
///
/// Plans are walked in path order, so `a-plan` is first.
#[tokio::test]
async fn a_file_in_two_plans_collapses_to_one_row_that_absorbs_both_plans_metadata() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(
        &workdir,
        "a-plan/plan.yaml",
        "name: first\ntags: [alpha]\ntests:\n  - tests/shared/test_x.py\n",
    );
    write(
        &workdir,
        "b-plan/plan.yaml",
        "name: second\ntags: [beta]\ntests:\n  - tests/shared/test_x.py\n",
    );
    write(
        &workdir,
        "tests/shared/test_x.py",
        "def test_x():\n    pass\n",
    );

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let universe = svc
        .list_universe(&ctx(tenant_id), None, Some("main"))
        .await
        .unwrap();

    assert_eq!(universe.len(), 1, "the same file must not appear twice");
    assert_eq!(
        universe[0].plan_name, "first",
        "the first plan to name the file owns the row"
    );
    assert_eq!(universe[0].plan_path, "a-plan/plan.yaml");
    assert_eq!(
        universe[0].tags,
        vec!["alpha".to_owned(), "beta".to_owned()],
        "the later plan's tags are unioned in, not dropped"
    );
}

/// Product filtering. The mock repository holds one repo, so this asserts
/// both directions against its own product id — the filter is a *product*
/// filter and never the tenancy boundary (that is `repo_scope`, covered in
/// `tests_tenant_scoping`).
#[tokio::test]
async fn list_universe_filters_by_product() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let repo = repo_fixture(repo_id, true);
    let product_id = repo.product_id;

    let tmp = tempfile::tempdir().unwrap();
    let workdir = crate::infra::git::layout::branch_workdir(tmp.path(), repo_id, "main");
    std::fs::create_dir_all(&workdir).unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo));

    write(
        &workdir,
        "infra/plan.yaml",
        "name: infra\ntests:\n  - tests/test_a.py\n",
    );
    write(&workdir, "tests/test_a.py", "def test_a():\n    pass\n");

    let svc = build_service(repos, tmp.path().to_path_buf()).await;

    assert_eq!(
        svc.list_universe(&ctx(tenant_id), Some(product_id), Some("main"))
            .await
            .unwrap()
            .len(),
        1,
        "the owning product sees its own test"
    );
    assert!(
        svc.list_universe(&ctx(tenant_id), Some(Uuid::new_v4()), Some("main"))
            .await
            .unwrap()
            .is_empty(),
        "another product's universe must not include this repository"
    );
}

/// `branch: None` resolves to each repository's own `default_branch`, which is
/// legacy's no-branch-selected behavior (`analytics.rs:810-816`). The fixture's
/// default is `main`, so the same content is found without naming it.
#[tokio::test]
async fn list_universe_without_a_branch_uses_the_repository_default() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(
        &workdir,
        "infra/plan.yaml",
        "name: infra\ntests:\n  - tests/test_a.py\n",
    );
    write(&workdir, "tests/test_a.py", "def test_a():\n    pass\n");

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    assert_eq!(
        svc.list_universe(&ctx(tenant_id), None, None)
            .await
            .unwrap()
            .len(),
        1
    );
    // A branch with no snapshot contributes nothing rather than erroring.
    assert!(
        svc.list_universe(&ctx(tenant_id), None, Some("release-9"))
            .await
            .unwrap()
            .is_empty()
    );
}

/// An unsynced repository is skipped, not fatal. `list_plans` on the same
/// repository *does* error (`RepoNotSynced`) — the asymmetry is deliberate:
/// one broken repository must not blank the whole-product overview, which is
/// legacy's posture too (`analytics.rs:856-865`).
#[tokio::test]
async fn an_unsynced_repository_contributes_nothing_instead_of_failing_the_call() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, false,
    )));

    let svc = build_service(Arc::clone(&repos), tmp.path().to_path_buf()).await;

    assert!(
        svc.list_universe(&ctx(tenant_id), None, Some("main"))
            .await
            .unwrap()
            .is_empty(),
        "an unsynced repository is skipped, and the call still succeeds"
    );
    assert!(
        matches!(
            svc.list_plans(&ctx(tenant_id), repo_id, "main")
                .await
                .unwrap_err(),
            DomainError::RepoNotSynced { .. }
        ),
        "the targeted read still errors; only the universe walk degrades"
    );
}

/// A plan entry naming a file that does not exist on this branch is dropped,
/// exactly as legacy's branch-scoped resolution drops it
/// (`analytics.rs:861-865`). A stale `tests:` line must not become a
/// zero-case universe row that then reads as `not_run` forever.
#[tokio::test]
async fn a_plan_entry_whose_file_is_absent_on_the_branch_is_dropped() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(
        &workdir,
        "infra/plan.yaml",
        "name: infra\ntests:\n  - tests/test_present.py\n  - tests/test_gone.py\n",
    );
    write(
        &workdir,
        "tests/test_present.py",
        "def test_present():\n    pass\n",
    );

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let universe = svc
        .list_universe(&ctx(tenant_id), None, Some("main"))
        .await
        .unwrap();

    assert_eq!(universe.len(), 1);
    assert_eq!(universe[0].test_file, "tests/test_present.py");
}

/// Rows come back ordered by `test_name`, matching legacy's sort
/// (`analytics.rs:941`).
#[tokio::test]
async fn universe_rows_are_ordered_by_test_name() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(
        &workdir,
        "infra/plan.yaml",
        "name: infra\ntests:\n  - tests/test_zulu.py\n  - tests/test_alpha.py\n",
    );
    write(&workdir, "tests/test_zulu.py", "def test_z():\n    pass\n");
    write(&workdir, "tests/test_alpha.py", "def test_a():\n    pass\n");

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let names: Vec<String> = svc
        .list_universe(&ctx(tenant_id), None, Some("main"))
        .await
        .unwrap()
        .into_iter()
        .map(|test| test.test_name)
        .collect();

    assert_eq!(names, vec!["alpha".to_owned(), "zulu".to_owned()]);
}

/// Legacy's component inference is naive and this port keeps it that way: it
/// takes the second path segment whenever the first is `tests`, with no check
/// that the segment is a directory. So `tests/test_x.py` — a file sitting
/// directly in `tests/` — infers the component `test_x.py`
/// (`analytics.rs:1794-1803`: `split('/')`, then `(Some("tests"), Some(c))`).
///
/// Pinned rather than fixed: `component` is a grouping key for the analytics
/// overview (`build_grouped_summaries`, `analytics.rs:1085`), so "improving"
/// this would silently re-bucket every such file relative to the system this
/// replaces.
#[tokio::test]
async fn a_file_directly_under_tests_infers_its_own_filename_as_the_component() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos, workdir) = synced_fixture(repo_id);

    write(
        &workdir,
        "infra/plan.yaml",
        "name: infra\ntests:\n  - tests/test_flat.py\n",
    );
    write(
        &workdir,
        "tests/test_flat.py",
        "def test_flat():\n    pass\n",
    );

    let svc = build_service(repos, tmp.path().to_path_buf()).await;
    let universe = svc
        .list_universe(&ctx(tenant_id), None, Some("main"))
        .await
        .unwrap();

    assert_eq!(
        universe[0].component.as_deref(),
        Some("test_flat.py"),
        "legacy quirk, ported verbatim: the inference does not check for a directory"
    );
}
