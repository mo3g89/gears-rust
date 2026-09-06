#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit tests for `BundlesService`: tar.gz roundtrip against an in-memory
//! `BundleStore`, expiry semantics, and traversal rejection.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, PolicyEnforcer};
use flate2::read::GzDecoder;
use qa_catalog_sdk::{BundleRequest, TestBundle};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, pep_properties};
use uuid::Uuid;

use super::bundles::BundlesService;
use super::test_support::{
    MockTestReposRepository, PermissiveAuthZ, ctx, repo_fixture, test_db_provider,
};
use crate::domain::error::DomainError;
use crate::domain::ports::bundle_store::BundleStore;
use crate::domain::repos::BundlesRepository;

const FIXTURE_CONTENT: &str = "def test_a():\n    assert True\n";

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// In-memory `BundleStore` over a `HashMap<storage_ref, bytes>`.
#[derive(Default)]
struct InMemoryBundleStore {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
}

#[async_trait]
impl BundleStore for InMemoryBundleStore {
    async fn put(&self, bundle_id: Uuid, bytes: Vec<u8>) -> Result<String, DomainError> {
        let storage_ref = format!("mem:{bundle_id}");
        self.blobs
            .lock()
            .unwrap()
            .insert(storage_ref.clone(), bytes);
        Ok(storage_ref)
    }

    async fn get(&self, storage_ref: &str) -> Result<Vec<u8>, DomainError> {
        self.blobs
            .lock()
            .unwrap()
            .get(storage_ref)
            .cloned()
            .ok_or_else(|| DomainError::Internal(format!("blob missing: {storage_ref}")))
    }

    async fn delete(&self, storage_ref: &str) -> Result<(), DomainError> {
        self.blobs.lock().unwrap().remove(storage_ref);
        Ok(())
    }
}

/// In-memory `BundlesRepository`, optionally failing its descriptor write so
/// the compensating blob delete can be exercised.
///
/// Tracks each row's owning tenant alongside its descriptor (`TestBundle`
/// itself carries no `tenant_id` — that column lives only in the real
/// table), scope-aware exactly like the real `OrmBundlesRepository`: a
/// [`Self::delete_expired`] or
/// [`Self::tenants_with_expired_bundles`] call under an unconstrained
/// (elevated) scope sees every tenant, and one under a tenant-bound scope
/// sees only the rows `AccessScope::contains_uuid` admits — the property the
/// bundle-GC split (enumerate under `domain::elevated`, delete per tenant
/// under `system_actor::for_bundle_delete`) depends on.
#[derive(Default)]
struct MockBundlesRepository {
    rows: Mutex<HashMap<Uuid, (Uuid, TestBundle)>>,
    /// When set, `create` fails with this error instead of inserting.
    failing_create: bool,
}

impl MockBundlesRepository {
    fn insert(&self, tenant_id: Uuid, bundle: TestBundle) {
        self.rows
            .lock()
            .unwrap()
            .insert(bundle.id, (tenant_id, bundle));
    }

    /// A repository whose descriptor write always fails (e.g. a duplicate or
    /// a dropped connection) — the only way to reach `create_bundle`'s
    /// compensating `store.delete`.
    fn failing_create() -> Self {
        Self {
            failing_create: true,
            ..Self::default()
        }
    }

    /// Whether `scope` admits `tenant_id`: unconstrained (the elevated
    /// enumeration scope) admits every tenant, otherwise `scope` must name it
    /// explicitly — the same rule `SecureORM`'s `.scope_with` applies to the
    /// real table.
    fn scope_admits(scope: &AccessScope, tenant_id: Uuid) -> bool {
        scope.is_unconstrained() || scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant_id)
    }
}

#[async_trait]
impl BundlesRepository for MockBundlesRepository {
    async fn create<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        tenant_id: Uuid,
        bundle: TestBundle,
    ) -> Result<TestBundle, DomainError> {
        if self.failing_create {
            return Err(DomainError::Database("descriptor write failed".to_owned()));
        }
        self.rows
            .lock()
            .unwrap()
            .insert(bundle.id, (tenant_id, bundle.clone()));
        Ok(bundle)
    }

    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TestBundle>, DomainError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .get(&id)
            .map(|(_, bundle)| bundle.clone()))
    }

    async fn delete_expired<C: DBRunner>(
        &self,
        _runner: &C,
        scope: &AccessScope,
        now: OffsetDateTime,
    ) -> Result<Vec<TestBundle>, DomainError> {
        let mut rows = self.rows.lock().unwrap();
        let expired: Vec<TestBundle> = rows
            .values()
            .filter(|(tenant_id, b)| b.expires_at <= now && Self::scope_admits(scope, *tenant_id))
            .map(|(_, b)| b.clone())
            .collect();
        for bundle in &expired {
            rows.remove(&bundle.id);
        }
        Ok(expired)
    }

    async fn tenants_with_expired_bundles<C: DBRunner>(
        &self,
        _runner: &C,
        scope: &AccessScope,
        now: OffsetDateTime,
    ) -> Result<Vec<Uuid>, DomainError> {
        let rows = self.rows.lock().unwrap();
        let mut tenants: Vec<Uuid> = rows
            .values()
            .filter(|(tenant_id, b)| b.expires_at <= now && Self::scope_admits(scope, *tenant_id))
            .map(|(tenant_id, _)| *tenant_id)
            .collect();
        tenants.sort_unstable();
        tenants.dedup();
        Ok(tenants)
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const TEST_TTL: time::Duration = time::Duration::seconds(3600);

async fn build_service(
    bundles: Arc<MockBundlesRepository>,
    repos: Arc<MockTestReposRepository>,
    store: Arc<InMemoryBundleStore>,
    repos_dir: PathBuf,
) -> BundlesService<MockBundlesRepository, MockTestReposRepository> {
    let enforcer = PolicyEnforcer::new(Arc::new(PermissiveAuthZ));
    let db = test_db_provider().await;
    BundlesService::new(db, bundles, repos, store, repos_dir, TEST_TTL, enforcer)
}

/// Tempdir + synced repo fixture with one test file in its `main` branch
/// snapshot (content reads resolve
/// `<repos_dir>/<repo_id>/branches/<branch_dir>`).
fn synced_fixture(repo_id: Uuid) -> (tempfile::TempDir, Arc<MockTestReposRepository>) {
    let tmp = tempfile::tempdir().unwrap();
    let workdir = crate::infra::git::layout::branch_workdir(tmp.path(), repo_id, "main");
    std::fs::create_dir_all(workdir.join("tests")).unwrap();
    std::fs::write(workdir.join("tests/test_a.py"), FIXTURE_CONTENT).unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    (tmp, repos)
}

use super::bundles::sha256_hex;

/// Un-gzip + untar into `(entry path, bytes)` pairs.
fn untar(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut archive = tar::Archive::new(GzDecoder::new(bytes));
    let mut out = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        out.push((path, content));
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_bundle_roundtrip() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(
        Arc::clone(&bundles),
        repos,
        Arc::clone(&store),
        tmp.path().to_path_buf(),
    )
    .await;

    let descriptor = svc
        .create_bundle(
            &ctx(tenant_id),
            BundleRequest {
                repo_id,
                branch: "main".to_owned(),
                files: vec!["tests/test_a.py".to_owned()],
            },
        )
        .await
        .unwrap();

    let bytes = svc
        .get_bundle_content(&ctx(tenant_id), descriptor.id)
        .await
        .unwrap();

    assert_eq!(
        sha256_hex(&bytes),
        descriptor.checksum_sha256,
        "descriptor checksum must match the served bytes"
    );
    assert_eq!(
        u64::try_from(bytes.len()).unwrap(),
        descriptor.size_bytes,
        "descriptor size must match the served bytes"
    );
    assert!(descriptor.expires_at > descriptor.created_at);

    let entries = untar(&bytes);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "tests/test_a.py");
    assert_eq!(
        entries[0].1,
        FIXTURE_CONTENT.as_bytes(),
        "the archived bytes must equal the fixture bytes"
    );
}

#[tokio::test]
async fn a_named_file_bundles_the_package_around_it_not_just_the_file() {
    // The regression test for run `94978978-fa28-4650-a14d-2ce8f72dff49`: ten
    // test modules were bundled alone and failed collection seven times on
    // `ModuleNotFoundError: No module named 'lib'`. Every file below is one the
    // old selective bundle dropped, and each one is separately fatal to a real
    // pytest suite: no `pytest.ini` is a marker-warning flood, no `conftest.py`
    // is every fixture missing, no `lib/` is the import error itself.
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let workdir = crate::infra::git::layout::branch_workdir(tmp.path(), repo_id, "main");
    std::fs::create_dir_all(workdir.join("tests/lib/monitoring")).unwrap();
    std::fs::create_dir_all(workdir.join("tests/monitoring")).unwrap();
    std::fs::write(workdir.join("tests/pytest.ini"), "[pytest]\n").unwrap();
    std::fs::write(workdir.join("tests/conftest.py"), "").unwrap();
    std::fs::write(workdir.join("tests/__init__.py"), "").unwrap();
    std::fs::write(workdir.join("tests/requirements.txt"), "pytest\n").unwrap();
    std::fs::write(workdir.join("tests/lib/monitoring/client.py"), "").unwrap();
    std::fs::write(
        workdir.join("tests/monitoring/test_b.py"),
        "from lib.monitoring.client import X\n",
    )
    .unwrap();

    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(bundles, repos, store, tmp.path().to_path_buf()).await;

    let descriptor = svc
        .create_bundle(
            &ctx(tenant_id),
            BundleRequest {
                repo_id,
                branch: "main".to_owned(),
                files: vec!["tests/monitoring/test_b.py".to_owned()],
            },
        )
        .await
        .unwrap();

    let bytes = svc
        .get_bundle_content(&ctx(tenant_id), descriptor.id)
        .await
        .unwrap();
    let paths: Vec<String> = untar(&bytes).into_iter().map(|(p, _)| p).collect();
    for required in [
        "tests/monitoring/test_b.py",
        "tests/pytest.ini",
        "tests/conftest.py",
        "tests/__init__.py",
        "tests/requirements.txt",
        "tests/lib/monitoring/client.py",
    ] {
        assert!(
            paths.iter().any(|p| p == required),
            "{required} must travel with a bundle that names only one test file; got {paths:?}"
        );
    }
}

#[tokio::test]
async fn caches_and_vcs_internals_never_travel_in_a_bundle() {
    // Not tidiness: a `.pyc` whose source has been deleted is still importable
    // and shadows the module the run meant to execute. `.git` is size.
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let workdir = crate::infra::git::layout::branch_workdir(tmp.path(), repo_id, "main");
    for dir in [".git", "tests/__pycache__", "node_modules", ".venv/lib"] {
        std::fs::create_dir_all(workdir.join(dir)).unwrap();
        std::fs::write(workdir.join(dir).join("junk"), "x").unwrap();
    }

    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(bundles, repos, store, tmp.path().to_path_buf()).await;

    let descriptor = svc
        .create_bundle(
            &ctx(tenant_id),
            BundleRequest {
                repo_id,
                branch: "main".to_owned(),
                files: vec!["tests/test_a.py".to_owned()],
            },
        )
        .await
        .unwrap();
    let bytes = svc
        .get_bundle_content(&ctx(tenant_id), descriptor.id)
        .await
        .unwrap();
    let paths: Vec<String> = untar(&bytes).into_iter().map(|(p, _)| p).collect();
    assert_eq!(
        paths,
        vec!["tests/test_a.py".to_owned()],
        "only the tracked content may travel"
    );
}

#[tokio::test]
async fn a_named_file_that_is_not_on_the_branch_is_still_rejected() {
    // `files` stopped being the archive's contents; it did NOT stop being
    // checked. A plan naming a path the branch does not have must fail here,
    // where the dispatcher can report it, rather than become a run whose pytest
    // exits 4 on a missing argument.
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(bundles, repos, store, tmp.path().to_path_buf()).await;

    let error = svc
        .create_bundle(
            &ctx(tenant_id),
            BundleRequest {
                repo_id,
                branch: "main".to_owned(),
                files: vec!["tests/does_not_exist.py".to_owned()],
            },
        )
        .await
        .expect_err("a missing named file must be rejected");
    assert!(
        matches!(error, DomainError::FileNotFound { ref path } if path == "tests/does_not_exist.py"),
        "expected FileNotFound, got {error:?}"
    );
}

#[tokio::test]
async fn a_content_root_repo_bundles_under_its_repository_relative_prefix() {
    // The regression test for run `1a9f0eb6-5e2f-4aa5-8eec-d577ed53a88c`.
    //
    // `vhp-core` sets `content_root = tests/e2e` and its suite locates the
    // checkout the way a suite in a checkout may: `parents[4]`. Rooting the
    // archive AT the content root put `tests/e2e/tests/vpctl/conftest.py` two
    // levels shallower, `parents[4]` ran off the end, and because that is a
    // `conftest.py` evaluated at import time pytest aborted the whole session --
    // 1465 collectable tests, zero recorded results.
    //
    // Asserted on the archive's entry names rather than on an extracted tree:
    // the entry name IS the contract with the runner, and a tree would also pass
    // if tar happened to create the parents for some unrelated reason.
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let workdir = crate::infra::git::layout::branch_workdir(tmp.path(), repo_id, "main");
    std::fs::create_dir_all(workdir.join("tests/e2e/tests/vpctl")).unwrap();
    std::fs::write(workdir.join("tests/e2e/tests/pytest.ini"), "[pytest]\n").unwrap();
    std::fs::write(workdir.join("tests/e2e/tests/vpctl/conftest.py"), "").unwrap();
    // Outside the content root: it must NOT travel just because the prefix now
    // names its parent directory.
    std::fs::write(workdir.join("tests/outside.py"), "").unwrap();

    let mut repo = repo_fixture(repo_id, true);
    repo.content_root = "tests/e2e".to_owned();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo));

    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(bundles, repos, store, tmp.path().to_path_buf()).await;

    let descriptor = svc
        .create_bundle(
            &ctx(tenant_id),
            BundleRequest {
                repo_id,
                branch: "main".to_owned(),
                // Still content-root-relative: the prefix changes what the
                // archive is called, not the coordinate a plan speaks.
                files: vec!["tests/vpctl/conftest.py".to_owned()],
            },
        )
        .await
        .unwrap();

    let bytes = svc
        .get_bundle_content(&ctx(tenant_id), descriptor.id)
        .await
        .unwrap();
    let entries = untar(&bytes);
    let paths: Vec<&str> = entries.iter().map(|(p, _)| p.as_str()).collect();

    assert!(
        paths.contains(&"tests/e2e/tests/vpctl/conftest.py"),
        "entries must carry the content_root prefix so the extracted tree keeps \
         the repository's depth; got {paths:?}"
    );
    assert!(
        paths.contains(&"tests/e2e/tests/pytest.ini"),
        "the whole content root still travels, prefixed; got {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.ends_with("outside.py")),
        "the prefix must not widen what is collected -- only what it is named; \
         got {paths:?}"
    );

    let marker = entries
        .iter()
        .find(|(p, _)| p == ".qa-content-root")
        .map(|(_, c)| String::from_utf8_lossy(c).into_owned());
    assert_eq!(
        marker.as_deref(),
        Some("tests/e2e"),
        "the archive must declare its own content root: it is the only way the \
         runner can resolve a content-root-relative TEST_FILES entry, and a \
         bundle already in the store has no marker at all"
    );
}

#[tokio::test]
async fn a_repo_without_a_content_root_bundles_exactly_as_before() {
    // The backward-compatibility half of the test above. `content_root` is empty
    // for every repository but one, and for those the archive must be unchanged
    // AND carry no marker -- absence is what tells the runner "the archive root
    // is the content root", which is also how every bundle built before the
    // marker existed still reads correctly.
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);

    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(bundles, repos, store, tmp.path().to_path_buf()).await;

    let descriptor = svc
        .create_bundle(
            &ctx(tenant_id),
            BundleRequest {
                repo_id,
                branch: "main".to_owned(),
                files: vec![],
            },
        )
        .await
        .unwrap();

    let bytes = svc
        .get_bundle_content(&ctx(tenant_id), descriptor.id)
        .await
        .unwrap();
    let paths: Vec<String> = untar(&bytes).into_iter().map(|(p, _)| p).collect();

    assert!(
        paths.contains(&"tests/test_a.py".to_owned()),
        "an empty content_root must leave entry names untouched; got {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p == ".qa-content-root"),
        "no marker may be written when there is no prefix to declare, so that a \
         missing marker keeps its single meaning; got {paths:?}"
    );
}

#[tokio::test]
async fn create_bundle_with_empty_files_bundles_whole_content_root() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    // A second file to prove the whole tree is walked.
    let workdir = crate::infra::git::layout::branch_workdir(tmp.path(), repo_id, "main");
    std::fs::create_dir_all(workdir.join("plans")).unwrap();
    std::fs::write(
        workdir.join("plans/smoke.yaml"),
        "name: smoke\ntests: [tests/test_a.py]\n",
    )
    .unwrap();

    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(bundles, repos, store, tmp.path().to_path_buf()).await;

    let descriptor = svc
        .create_bundle(
            &ctx(tenant_id),
            BundleRequest {
                repo_id,
                branch: "main".to_owned(),
                files: vec![],
            },
        )
        .await
        .unwrap();

    let bytes = svc
        .get_bundle_content(&ctx(tenant_id), descriptor.id)
        .await
        .unwrap();
    let paths: Vec<String> = untar(&bytes).into_iter().map(|(p, _)| p).collect();
    assert_eq!(
        paths,
        vec!["plans/smoke.yaml".to_owned(), "tests/test_a.py".to_owned()],
        "empty `files` must bundle the whole content root (deterministic order)"
    );
}

#[tokio::test]
async fn expired_bundle_is_not_served() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(
        Arc::clone(&bundles),
        repos,
        Arc::clone(&store),
        tmp.path().to_path_buf(),
    )
    .await;

    // A descriptor whose blob still exists but whose TTL has passed.
    let id = Uuid::new_v4();
    let storage_ref = store.put(id, b"stale".to_vec()).await.unwrap();
    let now = OffsetDateTime::now_utc();
    bundles.insert(
        tenant_id,
        TestBundle {
            id,
            storage_ref,
            checksum_sha256: sha256_hex(b"stale"),
            size_bytes: 5,
            expires_at: now - time::Duration::seconds(1),
            created_at: now - time::Duration::hours(2),
        },
    );

    let err = svc
        .get_bundle_content(&ctx(tenant_id), id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id: e } if e == id),
        "an expired bundle must read as missing, got {err:?}"
    );
}

#[tokio::test]
async fn purge_expired_deletes_rows_and_blobs() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(
        Arc::clone(&bundles),
        repos,
        Arc::clone(&store),
        tmp.path().to_path_buf(),
    )
    .await;

    let now = OffsetDateTime::now_utc();
    let expired_id = Uuid::new_v4();
    let live_id = Uuid::new_v4();
    for (id, expires_at) in [
        (expired_id, now - time::Duration::seconds(1)),
        (live_id, now + time::Duration::hours(1)),
    ] {
        let storage_ref = store.put(id, b"blob".to_vec()).await.unwrap();
        bundles.insert(
            tenant_id,
            TestBundle {
                id,
                storage_ref,
                checksum_sha256: sha256_hex(b"blob"),
                size_bytes: 4,
                expires_at,
                created_at: now - time::Duration::hours(2),
            },
        );
    }

    let purged = svc.purge_expired(&ctx(tenant_id)).await.unwrap();

    assert_eq!(purged, 1);
    assert!(bundles.rows.lock().unwrap().contains_key(&live_id));
    assert!(!bundles.rows.lock().unwrap().contains_key(&expired_id));
    assert!(
        store
            .blobs
            .lock()
            .unwrap()
            .contains_key(&format!("mem:{live_id}"))
    );
    assert!(
        !store
            .blobs
            .lock()
            .unwrap()
            .contains_key(&format!("mem:{expired_id}"))
    );
}

/// Compensating delete: when the descriptor write fails, the blob that was
/// already `put` must not be stranded in the store (nothing would ever
/// reference it, so GC — which walks descriptor rows — could never reclaim it).
#[tokio::test]
async fn create_bundle_deletes_the_blob_when_the_descriptor_write_fails() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::failing_create());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(
        Arc::clone(&bundles),
        repos,
        Arc::clone(&store),
        tmp.path().to_path_buf(),
    )
    .await;

    let err = svc
        .create_bundle(
            &ctx(tenant_id),
            BundleRequest {
                repo_id,
                branch: "main".to_owned(),
                files: vec!["tests/test_a.py".to_owned()],
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Database(_)),
        "the original write error must propagate, not the cleanup outcome: {err:?}"
    );
    assert!(
        store.blobs.lock().unwrap().is_empty(),
        "the blob must be cleaned up, leaving no unreferenced storage: {:?}",
        store.blobs.lock().unwrap().keys().collect::<Vec<_>>()
    );
    assert!(bundles.rows.lock().unwrap().is_empty());
}

#[tokio::test]
async fn create_bundle_rejects_path_traversal() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(bundles, repos, store, tmp.path().to_path_buf()).await;

    let err = svc
        .create_bundle(
            &ctx(tenant_id),
            BundleRequest {
                repo_id,
                branch: "main".to_owned(),
                files: vec!["../outside.txt".to_owned()],
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
}

// ---------------------------------------------------------------------------
// The bundle GC's per-tenant split: enumerate under `domain::elevated`,
// delete per tenant under `system_actor::for_bundle_delete`
// ---------------------------------------------------------------------------

/// [`AuthZResolverClient`] double that records every request it is asked to
/// decide and always grants tenant-scoped access — the harness for
/// [`tenants_with_expired_bundles_does_not_consult_the_policy_engine`]: a
/// request that slipped through to `evaluate` is recorded here regardless of
/// what the resulting decision happened to be.
#[derive(Default)]
struct RecordingAuthZ {
    requests: Mutex<Vec<(String, String)>>,
}

impl RecordingAuthZ {
    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl AuthZResolverClient for RecordingAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        self.requests
            .lock()
            .unwrap()
            .push((request.resource.resource_type, request.action.name));
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
}

async fn build_service_with_authz(
    bundles: Arc<MockBundlesRepository>,
    repos: Arc<MockTestReposRepository>,
    store: Arc<InMemoryBundleStore>,
    repos_dir: PathBuf,
    authz: Arc<dyn AuthZResolverClient>,
) -> BundlesService<MockBundlesRepository, MockTestReposRepository> {
    let enforcer = PolicyEnforcer::new(authz);
    let db = test_db_provider().await;
    BundlesService::new(db, bundles, repos, store, repos_dir, TEST_TTL, enforcer)
}

/// Every tenant with an expired bundle is listed once, ascending, and a
/// tenant with only a live bundle is absent — the enumeration
/// [`BundlesService::purge_expired`]'s per-tenant loop drives from.
#[tokio::test]
async fn tenants_with_expired_bundles_lists_each_expired_tenant_once() {
    let tenant_a = Uuid::from_u128(0x0A);
    let tenant_b = Uuid::from_u128(0xB0);
    let tenant_live_only = Uuid::from_u128(0xC0);
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(Arc::clone(&bundles), repos, store, tmp.path().to_path_buf()).await;

    let now = OffsetDateTime::now_utc();
    // Two expired bundles for tenant_a (DISTINCT must collapse them), one
    // expired for tenant_b, one still live for tenant_live_only.
    for (tenant, expires_at) in [
        (tenant_a, now - time::Duration::seconds(1)),
        (tenant_a, now - time::Duration::seconds(2)),
        (tenant_b, now - time::Duration::seconds(1)),
        (tenant_live_only, now + time::Duration::hours(1)),
    ] {
        let id = Uuid::new_v4();
        bundles.insert(
            tenant,
            TestBundle {
                id,
                storage_ref: format!("mem:{id}"),
                checksum_sha256: sha256_hex(b"blob"),
                size_bytes: 4,
                expires_at,
                created_at: now - time::Duration::hours(2),
            },
        );
    }

    let tenants = svc
        .tenants_with_expired_bundles(&ctx(Uuid::nil()))
        .await
        .unwrap();
    assert_eq!(
        tenants,
        vec![tenant_a, tenant_b],
        "ascending, one entry per tenant with an expired row, live-only tenant absent"
    );
}

/// A `qa_bundles` row owned by the nil tenant is excluded from the
/// enumeration rather than passed through.
///
/// `for_bundle_delete(Uuid::nil())` builds a context indistinguishable from
/// `for_bundle_gc()`'s own platform-scoped one (`SecurityContext` has no
/// third state between "this tenant" and "no tenant"), so without this
/// filter a nil-tenant row would mint a delete context the PEP denies, sort
/// first under ascending order, and be retried and denied on every GC pass
/// forever without ever being the fault the retry implies. Filtering it here
/// keeps the pass from wedging on a row it can never legitimately act on and
/// still lets a real tenant's expired bundle, listed alongside it, purge
/// normally.
#[tokio::test]
async fn tenants_with_expired_bundles_excludes_the_nil_tenant() {
    let tenant_real = Uuid::from_u128(0xA11CE);
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(Arc::clone(&bundles), repos, store, tmp.path().to_path_buf()).await;

    let now = OffsetDateTime::now_utc();
    for (tenant, expires_at) in [
        (Uuid::nil(), now - time::Duration::seconds(1)),
        (tenant_real, now - time::Duration::seconds(1)),
    ] {
        let id = Uuid::new_v4();
        bundles.insert(
            tenant,
            TestBundle {
                id,
                storage_ref: format!("mem:{id}"),
                checksum_sha256: sha256_hex(b"blob"),
                size_bytes: 4,
                expires_at,
                created_at: now - time::Duration::hours(2),
            },
        );
    }

    let tenants = svc
        .tenants_with_expired_bundles(&ctx(Uuid::nil()))
        .await
        .unwrap();
    assert_eq!(
        tenants,
        vec![tenant_real],
        "the nil-tenant row must be filtered out; the real tenant's row must still be listed"
    );
}

/// The property this task exists to establish for the bundle GC: the
/// enumeration never asks the policy engine anything, even though the
/// per-tenant delete that follows still does (see
/// `purge_expired_deletes_rows_and_blobs`).
///
/// Break-tested: reverting `BundlesService::tenants_with_expired_bundles` to
/// call `self.policy_enforcer.access_scope(...)` again makes this test fail
/// with a non-empty request log.
#[tokio::test]
async fn tenants_with_expired_bundles_does_not_consult_the_policy_engine() {
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let enforcer = Arc::new(RecordingAuthZ::default());
    let svc = build_service_with_authz(
        Arc::clone(&bundles),
        repos,
        store,
        tmp.path().to_path_buf(),
        Arc::clone(&enforcer) as _,
    )
    .await;

    let now = OffsetDateTime::now_utc();
    bundles.insert(
        Uuid::new_v4(),
        TestBundle {
            id: Uuid::new_v4(),
            storage_ref: "mem:x".to_owned(),
            checksum_sha256: sha256_hex(b"blob"),
            size_bytes: 4,
            expires_at: now - time::Duration::seconds(1),
            created_at: now - time::Duration::hours(2),
        },
    );

    let _outcome = svc.tenants_with_expired_bundles(&ctx(Uuid::nil())).await;

    assert_eq!(
        enforcer.request_count(),
        0,
        "the bundle GC enumeration must elevate through domain::elevated, not the PEP"
    );
}

/// The full split, end to end: two tenants each own an expired bundle: the
/// enumeration finds both, and looping `purge_expired` under each tenant's
/// own scope (as `crate::gear::QaCatalog::run_bundle_gc` does) purges each
/// tenant's row and no other tenant's — the property that matters, since a
/// scope leak here would let one tenant's GC pass delete another's rows.
#[tokio::test]
async fn purging_each_enumerated_tenant_removes_only_that_tenants_rows() {
    let tenant_a = Uuid::from_u128(0x0A);
    let tenant_b = Uuid::from_u128(0xB0);
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::default());
    let store = Arc::new(InMemoryBundleStore::default());
    let svc = build_service(Arc::clone(&bundles), repos, store, tmp.path().to_path_buf()).await;

    let now = OffsetDateTime::now_utc();
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();
    for (tenant, id) in [(tenant_a, id_a), (tenant_b, id_b)] {
        bundles.insert(
            tenant,
            TestBundle {
                id,
                storage_ref: format!("mem:{id}"),
                checksum_sha256: sha256_hex(b"blob"),
                size_bytes: 4,
                expires_at: now - time::Duration::seconds(1),
                created_at: now - time::Duration::hours(2),
            },
        );
    }

    let tenants = svc
        .tenants_with_expired_bundles(&ctx(Uuid::nil()))
        .await
        .unwrap();
    assert_eq!(tenants, vec![tenant_a, tenant_b]);

    let mut total_purged = 0;
    for tenant in tenants {
        total_purged += svc.purge_expired(&ctx(tenant)).await.unwrap();
    }

    assert_eq!(total_purged, 2, "both tenants' expired rows were purged");
    assert!(
        bundles.rows.lock().unwrap().is_empty(),
        "no row should survive once every enumerated tenant has been purged"
    );
}
