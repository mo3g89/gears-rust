#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit tests for `ReposService`: sync bookkeeping (branch cache +
//! timestamp / sanitized error recording) and URL credential hygiene.
//!
//! The sync engine is an in-memory `RepoSyncPort` double that can be
//! programmed to succeed (optionally writing fixture files into the branch
//! snapshot, like the real gix adapter does) or fail with a given message.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::CredStoreClientV1;
use credstore_sdk::test_util::MockCredStoreClient;
use qa_catalog_sdk::{NewTestRepository, TestRepositoryUpdate};
use uuid::Uuid;

use super::repos::ReposService;
use super::sync_cache::SyncCache;
use super::test_support::{
    MockSshKeysRepository, MockTestReposRepository, PermissiveAuthZ, ctx, repo_fixture,
    test_db_provider,
};
use crate::domain::error::DomainError;
use crate::domain::ports::repo_sync::{RepoSyncPort, SyncResult};

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

enum SyncBehavior {
    /// Return these branches and write `(relative path, content)` fixture
    /// files into the branch snapshot first (mirrors a real materialization).
    Succeed {
        branches: Vec<String>,
        files: Vec<(String, String)>,
    },
    /// Fail with this message (as the gix adapter would via `DomainError`).
    Fail { message: String },
}

struct MockRepoSyncPort {
    behavior: SyncBehavior,
    seen_credentials: Mutex<Vec<Option<String>>>,
    seen_urls: Mutex<Vec<String>>,
}

impl MockRepoSyncPort {
    fn succeeding(branches: Vec<String>, files: Vec<(String, String)>) -> Self {
        Self {
            behavior: SyncBehavior::Succeed { branches, files },
            seen_credentials: Mutex::new(Vec::new()),
            seen_urls: Mutex::new(Vec::new()),
        }
    }

    fn failing(message: &str) -> Self {
        Self {
            behavior: SyncBehavior::Fail {
                message: message.to_owned(),
            },
            seen_credentials: Mutex::new(Vec::new()),
            seen_urls: Mutex::new(Vec::new()),
        }
    }

    /// Every `credential` argument the engine was handed, in call order.
    /// `Some(material)` proves the resolved credstore secret actually reached
    /// the engine; `None` is a public-repository call.
    fn seen_credentials(&self) -> Vec<Option<String>> {
        self.seen_credentials.lock().unwrap().clone()
    }

    /// Every `url` the content-sync half was handed, in call order.
    fn seen_urls(&self) -> Vec<String> {
        self.seen_urls.lock().unwrap().clone()
    }

    /// The `list_remote_branches` (ls-refs) half of the double, used by the
    /// branch-cache refresher tests. `Succeed`/`Fail` drive both halves.
    fn remote_branches(&self) -> Result<Vec<String>, DomainError> {
        match &self.behavior {
            SyncBehavior::Succeed { branches, .. } => Ok(branches.clone()),
            SyncBehavior::Fail { message } => Err(DomainError::Internal(message.clone())),
        }
    }
}

#[async_trait]
impl RepoSyncPort for MockRepoSyncPort {
    async fn sync(
        &self,
        url: &str,
        _branch: &str,
        credential: Option<&str>,
        _host_dir: &Path,
        branch_workdir: &Path,
    ) -> Result<SyncResult, DomainError> {
        self.seen_credentials
            .lock()
            .unwrap()
            .push(credential.map(ToOwned::to_owned));
        self.seen_urls.lock().unwrap().push(url.to_owned());
        match &self.behavior {
            SyncBehavior::Succeed { branches, files } => {
                for (rel, content) in files {
                    let path = branch_workdir.join(rel);
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).unwrap();
                    }
                    std::fs::write(&path, content).unwrap();
                }
                Ok(SyncResult {
                    branches: branches.clone(),
                    head_commit: "deadbeef".to_owned(),
                })
            }
            SyncBehavior::Fail { message } => Err(DomainError::Internal(message.clone())),
        }
    }

    async fn list_remote_branches(
        &self,
        _url: &str,
        credential: Option<&str>,
    ) -> Result<Vec<String>, DomainError> {
        self.seen_credentials
            .lock()
            .unwrap()
            .push(credential.map(ToOwned::to_owned));
        self.remote_branches()
    }
}

/// Credstore double for tests that never resolve a credential
/// (`credential_ref: None` fixtures): every call is a test bug.
struct UnusedCredStore;

#[async_trait]
impl CredStoreClientV1 for UnusedCredStore {
    async fn get(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _key: &credstore_sdk::SecretRef,
    ) -> Result<Option<credstore_sdk::GetSecretResponse>, credstore_sdk::CredStoreError> {
        unimplemented!("credstore must not be consulted for credential-less repositories")
    }
}

/// Credstore double whose `get` is denied.
///
/// The shared [`MockCredStoreClient`] covers the other three
/// [`ReposService::resolve_credential`] arms (a hit via `with_secrets`, the
/// `Ok(None)` miss via `empty`, and a backend fault via `always_failing`) but
/// cannot express `AccessDenied` — the one arm that must surface as
/// `Forbidden` rather than as a recorded sync failure.
struct DenyingCredStore;

#[async_trait]
impl CredStoreClientV1 for DenyingCredStore {
    async fn get(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _key: &credstore_sdk::SecretRef,
    ) -> Result<Option<credstore_sdk::GetSecretResponse>, credstore_sdk::CredStoreError> {
        Err(credstore_sdk::CredStoreError::AccessDenied)
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

async fn build_service(
    repos: Arc<MockTestReposRepository>,
    engine: Arc<MockRepoSyncPort>,
    repos_dir: PathBuf,
) -> ReposService<MockTestReposRepository, MockSshKeysRepository> {
    build_service_with_credstore(repos, engine, repos_dir, Arc::new(UnusedCredStore)).await
}

/// Like [`build_service`] but with a caller-supplied credstore double — for
/// the credential-bearing sync path (`credential_ref: Some(..)` fixtures).
///
/// Zero freshness TTL: the cache never short-circuits, so every `sync_repo`
/// call reaches the engine double (the pre-cache behavior these tests pin).
/// The freshness path itself is exercised by
/// [`sync_repo_honors_the_freshness_cache_until_forced`].
async fn build_service_with_credstore(
    repos: Arc<MockTestReposRepository>,
    engine: Arc<MockRepoSyncPort>,
    repos_dir: PathBuf,
    credstore: Arc<dyn CredStoreClientV1>,
) -> ReposService<MockTestReposRepository, MockSshKeysRepository> {
    build_service_full(
        repos,
        engine,
        repos_dir,
        credstore,
        std::time::Duration::ZERO,
    )
    .await
}

async fn build_service_full(
    repos: Arc<MockTestReposRepository>,
    engine: Arc<MockRepoSyncPort>,
    repos_dir: PathBuf,
    credstore: Arc<dyn CredStoreClientV1>,
    freshness_ttl: std::time::Duration,
) -> ReposService<MockTestReposRepository, MockSshKeysRepository> {
    build_service_with_ssh_keys(
        repos,
        engine,
        repos_dir,
        credstore,
        freshness_ttl,
        Arc::new(MockSshKeysRepository::none()),
    )
    .await
}

/// Like [`build_service_full`] but with a caller-supplied SSH-key
/// repository — for the ssh credential-resolution path, where
/// `credential_ref` names a `qa_ssh_keys` row rather than a credstore
/// reference.
async fn build_service_with_ssh_keys(
    repos: Arc<MockTestReposRepository>,
    engine: Arc<MockRepoSyncPort>,
    repos_dir: PathBuf,
    credstore: Arc<dyn CredStoreClientV1>,
    freshness_ttl: std::time::Duration,
    ssh_keys: Arc<MockSshKeysRepository>,
) -> ReposService<MockTestReposRepository, MockSshKeysRepository> {
    let enforcer = PolicyEnforcer::new(Arc::new(PermissiveAuthZ));
    let db = test_db_provider().await;
    ReposService::new(
        db,
        repos,
        ssh_keys,
        credstore,
        engine,
        repos_dir,
        Arc::new(SyncCache::new(freshness_ttl)),
        enforcer,
    )
}

/// A never-synced repository fixture that carries a credstore reference, so
/// the sync path has to resolve it.
fn credentialed_repo(repo_id: Uuid) -> qa_catalog_sdk::TestRepository {
    qa_catalog_sdk::TestRepository {
        credential_ref: Some(CRED_REF.to_owned()),
        ..repo_fixture(repo_id, false)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_repo_updates_branch_cache_and_timestamp() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, false,
    )));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned(), "dev".to_owned()],
        vec![(
            "plans/smoke.yaml".to_owned(),
            "name: smoke\ntests: [test_a.py]\n".to_owned(),
        )],
    ));
    let svc = build_service(Arc::clone(&repos), engine, tmp.path().to_path_buf()).await;

    let updated = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap();

    assert!(
        updated.last_synced_at.is_some(),
        "successful sync must set last_synced_at"
    );
    assert_eq!(updated.sync_error, None, "successful sync clears the error");
    assert_eq!(
        repos.recorded_branches(),
        vec!["main".to_owned(), "dev".to_owned()],
        "branch cache must be replaced with the engine's inventory"
    );
    assert!(
        crate::infra::git::layout::branch_workdir(tmp.path(), repo_id, "main")
            .join("plans/smoke.yaml")
            .is_file(),
        "an empty branch selector must materialize the DEFAULT branch's snapshot"
    );
}

/// Race regression: the engine must be handed the repository row re-read
/// UNDER the two-tier lock, not the pre-lock row. `update_repo` and
/// `delete_repo` mutate the working area while holding the repo-tier lock,
/// so a URL change landing between `sync_repo`'s pre-lock read and its lock
/// acquisition must reach the engine — otherwise the OLD remote's content
/// would be fetched and recorded as freshly synced, silently defeating the
/// update path's invalidation. The mock swaps the row's URL right after the
/// first `get`, which is exactly that interleaving, deterministically.
#[tokio::test]
async fn sync_repo_syncs_the_row_reread_under_the_lock() {
    const MOVED_URL: &str = "https://example.com/org/moved.git";

    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo_swapping_url(
        repo_fixture(repo_id, false),
        MOVED_URL,
    ));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned()],
        vec![],
    ));
    let svc = build_service(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
    )
    .await;

    svc.sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap();

    assert_eq!(
        engine.seen_urls(),
        vec![MOVED_URL.to_owned()],
        "the engine must sync the URL from the under-lock re-read"
    );
}

/// The freshness cache: an unforced re-sync within the TTL is served from
/// the cache (no engine call); `force: true` evicts and re-syncs.
#[tokio::test]
async fn sync_repo_honors_the_freshness_cache_until_forced() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, false,
    )));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned()],
        vec![],
    ));
    let svc = build_service_full(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(UnusedCredStore),
        std::time::Duration::from_mins(5),
    )
    .await;

    svc.sync_repo(&ctx(tenant_id), repo_id, "main", false)
        .await
        .unwrap();
    assert_eq!(engine.seen_credentials().len(), 1, "first sync must fetch");

    svc.sync_repo(&ctx(tenant_id), repo_id, "main", false)
        .await
        .unwrap();
    assert_eq!(
        engine.seen_credentials().len(),
        1,
        "a fresh branch must be served from the cache without an engine call"
    );

    svc.sync_repo(&ctx(tenant_id), repo_id, "dev", false)
        .await
        .unwrap();
    assert_eq!(
        engine.seen_credentials().len(),
        2,
        "freshness is per-branch: another branch must still fetch"
    );

    svc.sync_repo(&ctx(tenant_id), repo_id, "main", true)
        .await
        .unwrap();
    assert_eq!(
        engine.seen_credentials().len(),
        3,
        "force must evict the freshness entry and re-sync"
    );
}

#[tokio::test]
async fn sync_repo_records_error_string_on_engine_failure() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, false,
    )));
    let engine = Arc::new(MockRepoSyncPort::failing(
        "fetch of https://x-access-token:supersecret@github.com/org/repo.git failed: 403",
    ));
    let svc = build_service(Arc::clone(&repos), engine, tmp.path().to_path_buf()).await;

    let updated = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap();

    let sync_error = updated.sync_error.expect("engine failure must be recorded");
    assert!(
        !sync_error.contains("supersecret"),
        "sync_error must never carry credential material: {sync_error}"
    );
    assert!(
        sync_error.contains("https://***@github.com/org/repo.git"),
        "URL userinfo must be redacted, not dropped wholesale: {sync_error}"
    );
    assert_eq!(
        updated.last_synced_at, None,
        "a failed sync must not claim a sync timestamp"
    );
    assert!(
        repos.recorded_branches().is_empty(),
        "a failed sync must not touch the branch cache"
    );
}

#[tokio::test]
async fn create_repo_rejects_userinfo_urls() {
    let tenant_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::none());
    let engine = Arc::new(MockRepoSyncPort::failing("unused"));
    let svc = build_service(repos, engine, tmp.path().to_path_buf()).await;

    for url in [
        "https://user:token@github.com/org/repo.git",
        "https://token@github.com/org/repo.git",
    ] {
        let err = svc
            .create_repo(
                &ctx(tenant_id),
                NewTestRepository {
                    product_id: Uuid::new_v4(),
                    name: "repo".to_owned(),
                    url: url.to_owned(),
                    default_branch: "main".to_owned(),
                    content_root: String::new(),
                    credential_ref: None,
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { ref field, .. } if field == "url"),
            "expected url validation error for {url}, got {err:?}"
        );
    }
}

#[tokio::test]
async fn create_repo_allows_credential_free_urls() {
    let tenant_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    for url in [
        "https://github.com/org/repo.git",
        "http://git.internal.example/org/repo.git",
    ] {
        let repos = Arc::new(MockTestReposRepository::none());
        let engine = Arc::new(MockRepoSyncPort::failing("unused"));
        let svc = build_service(repos, engine, tmp.path().to_path_buf()).await;
        let created = svc
            .create_repo(
                &ctx(tenant_id),
                NewTestRepository {
                    product_id: Uuid::new_v4(),
                    name: "repo".to_owned(),
                    url: url.to_owned(),
                    default_branch: "main".to_owned(),
                    content_root: String::new(),
                    credential_ref: None,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("expected {url} to be accepted, got {e:?}"));
        assert_eq!(created.url, url);
    }
}

/// Scheme allow-list (ADR-0005, amended 2026-08-27): http(s) and ssh
/// remotes may be registered; everything else may not. The gix engine's
/// local transport would sync a plain host path (including another tenant's
/// working copy under `repos_dir`), so `file://`, local paths, Windows
/// drive paths and bare host strings must still be rejected at create time.
///
/// `ssh://git:password@host/...` stays rejected even though ssh is now
/// supported: a password is an embedded secret under any scheme. SSH
/// acceptance is asserted separately by `create_repo_allows_ssh_urls`.
#[tokio::test]
async fn create_repo_rejects_non_http_urls() {
    let tenant_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::none());
    let engine = Arc::new(MockRepoSyncPort::failing("unused"));
    let svc = build_service(repos, engine, tmp.path().to_path_buf()).await;

    for url in [
        "file:///var/lib/gears/qa-catalog/repos/00000000-0000-0000-0000-000000000000",
        "/var/lib/gears/qa-catalog/repos/00000000-0000-0000-0000-000000000000",
        "ssh://git:password@host/org/repo.git",
        "git:password@host:org/repo.git",
        "git://host/org/repo.git",
        "github.com/org/repo.git",
        "C:\\Users\\me\\repo",
        "github.com",
    ] {
        let err = svc
            .create_repo(
                &ctx(tenant_id),
                NewTestRepository {
                    product_id: Uuid::new_v4(),
                    name: "repo".to_owned(),
                    url: url.to_owned(),
                    default_branch: "main".to_owned(),
                    content_root: String::new(),
                    credential_ref: None,
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { ref field, .. } if field == "url"),
            "expected url validation error for {url}, got {err:?}"
        );
    }
}

#[tokio::test]
async fn create_repo_rejects_traversing_content_root() {
    let tenant_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::none());
    let engine = Arc::new(MockRepoSyncPort::failing("unused"));
    let svc = build_service(repos, engine, tmp.path().to_path_buf()).await;

    for content_root in ["../outside", "/abs/path", "a/../../b", "a\\..\\b"] {
        let err = svc
            .create_repo(
                &ctx(tenant_id),
                NewTestRepository {
                    product_id: Uuid::new_v4(),
                    name: "repo".to_owned(),
                    url: "https://github.com/org/repo.git".to_owned(),
                    default_branch: "main".to_owned(),
                    content_root: content_root.to_owned(),
                    credential_ref: None,
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { ref field, .. } if field == "content_root"),
            "expected content_root validation error for {content_root:?}, got {err:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Credential-bearing sync path: all four `resolve_credential` arms, both
// sanitizer layers, and the one message that reaches `sync_error` WITHOUT
// passing through `sanitize_sync_error`.
// ---------------------------------------------------------------------------

const CRED_REF: &str = "qa-catalog-repo-token";
const CRED_MATERIAL: &str = "x-access-token:ghp_supersecrettoken";

/// Arm 1 — credstore hit. The resolved material must reach the engine (and
/// nothing else): this is the assertion `seen_credentials` exists for.
#[tokio::test]
async fn sync_repo_passes_the_resolved_credential_to_the_engine() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(credentialed_repo(
        repo_id,
    )));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned()],
        vec![],
    ));
    let credstore = Arc::new(MockCredStoreClient::with_secrets(vec![(
        CRED_REF.to_owned(),
        CRED_MATERIAL.to_owned(),
    )]));
    let svc = build_service_with_credstore(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        credstore,
    )
    .await;

    let updated = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap();

    assert_eq!(updated.sync_error, None);
    assert_eq!(
        engine.seen_credentials(),
        vec![Some(CRED_MATERIAL.to_owned())],
        "the credstore secret must reach the engine verbatim, exactly once"
    );

    // The branch refresher resolves the very same way.
    svc.refresh_branches(&ctx(tenant_id), repo_id)
        .await
        .unwrap();
    assert_eq!(
        engine.seen_credentials(),
        vec![
            Some(CRED_MATERIAL.to_owned()),
            Some(CRED_MATERIAL.to_owned())
        ],
        "ls-refs must be credentialed too"
    );
}

/// Arm 2 — `Ok(None)`: the reference does not resolve. That is recorded as a
/// sync failure naming the *reference* (repo-row metadata, not a secret), and
/// the engine is never called. This is also the ONE message that reaches
/// `sync_error` without passing through `sanitize_sync_error`, so it must be
/// safe by construction.
#[tokio::test]
async fn sync_repo_records_an_unresolvable_credential_without_calling_the_engine() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(credentialed_repo(
        repo_id,
    )));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned()],
        vec![],
    ));
    let svc = build_service_with_credstore(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
    )
    .await;

    let updated = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap();

    let sync_error = updated
        .sync_error
        .expect("an unresolvable credential must be recorded");
    assert!(
        sync_error.contains(CRED_REF),
        "the message must name the reference so an operator can fix it: {sync_error}"
    );
    assert!(
        !sync_error.contains("ghp_"),
        "no material may appear (there is none to leak, and the message bypasses \
         the sanitizer): {sync_error}"
    );
    assert_eq!(updated.last_synced_at, None);
    assert!(
        engine.seen_credentials().is_empty(),
        "the engine must not be called at all when the credential cannot be resolved"
    );
    assert!(
        repos.recorded_branches().is_empty(),
        "the branch cache must not be touched"
    );

    // The refresher returns the same condition as an error instead of
    // recording it (it must not clobber the content-sync error column).
    let err = svc
        .refresh_branches(&ctx(tenant_id), repo_id)
        .await
        .unwrap_err();
    let DomainError::SyncFailed { message } = err else {
        panic!("expected SyncFailed, got {err:?}");
    };
    assert!(message.contains(CRED_REF), "got {message}");
}

/// Arm 3 — credstore denies the read: `Forbidden`, not a recorded sync
/// failure. A policy problem is the caller's, not the repository's state.
#[tokio::test]
async fn sync_repo_surfaces_credstore_access_denied_as_forbidden() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(credentialed_repo(
        repo_id,
    )));
    let engine = Arc::new(MockRepoSyncPort::succeeding(vec![], vec![]));
    let svc = build_service_with_credstore(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(DenyingCredStore),
    )
    .await;

    let err = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Forbidden), "got {err:?}");

    let repo = svc.get_repo(&ctx(tenant_id), repo_id).await.unwrap();
    assert_eq!(
        repo.sync_error, None,
        "an authorization failure must not be recorded as a repository sync error"
    );
    assert!(engine.seen_credentials().is_empty());
}

/// Arm 4 — any other credstore failure is infrastructure: `CredStore`.
#[tokio::test]
async fn sync_repo_surfaces_other_credstore_failures_as_credstore_errors() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(credentialed_repo(
        repo_id,
    )));
    let engine = Arc::new(MockRepoSyncPort::succeeding(vec![], vec![]));
    let svc = build_service_with_credstore(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::always_failing()),
    )
    .await;

    let err = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::CredStore(_)), "got {err:?}");
    assert!(engine.seen_credentials().is_empty());
}

/// The SECOND sanitizer layer, which the URL-userinfo regex cannot cover: a
/// token echoed by the engine *outside* a URL (a header dump, an auth-probe
/// trace) is stripped because it equals the resolved material. This is the
/// entire reason that layer exists.
#[tokio::test]
async fn sync_error_strips_credential_material_echoed_outside_a_url() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(credentialed_repo(
        repo_id,
    )));
    // No `scheme://userinfo@host` anywhere: the regex layer cannot help here.
    let engine = Arc::new(MockRepoSyncPort::failing(&format!(
        "authentication failed for https://github.com/org/repo.git \
         (sent Authorization: Basic {CRED_MATERIAL}); retried with {CRED_MATERIAL}"
    )));
    let svc = build_service_with_credstore(
        Arc::clone(&repos),
        engine,
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::with_secrets(vec![(
            CRED_REF.to_owned(),
            CRED_MATERIAL.to_owned(),
        )])),
    )
    .await;

    let updated = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap();
    let sync_error = updated.sync_error.expect("engine failure must be recorded");

    assert!(
        !sync_error.contains("ghp_supersecrettoken") && !sync_error.contains(CRED_MATERIAL),
        "material echoed outside a URL must be stripped: {sync_error}"
    );
    assert_eq!(
        sync_error.matches("***").count(),
        2,
        "EVERY occurrence must be replaced, not just the first: {sync_error}"
    );
    assert!(
        sync_error.contains("https://github.com/org/repo.git"),
        "the diagnosable remainder must survive: {sync_error}"
    );
}

/// Same second layer on the refresher's (ls-refs) half.
#[tokio::test]
async fn refresh_branches_strips_credential_material_echoed_outside_a_url() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(credentialed_repo(
        repo_id,
    )));
    let engine = Arc::new(MockRepoSyncPort::failing(&format!(
        "ls-refs rejected the token {CRED_MATERIAL}"
    )));
    let svc = build_service_with_credstore(
        Arc::clone(&repos),
        engine,
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::with_secrets(vec![(
            CRED_REF.to_owned(),
            CRED_MATERIAL.to_owned(),
        )])),
    )
    .await;

    let err = svc
        .refresh_branches(&ctx(tenant_id), repo_id)
        .await
        .unwrap_err();
    let DomainError::SyncFailed { message } = err else {
        panic!("expected SyncFailed, got {err:?}");
    };
    assert!(
        !message.contains("ghp_supersecrettoken"),
        "the returned message must never carry material: {message}"
    );
    assert!(message.contains("***"), "got {message}");
}

/// A malformed stored reference is a validation error before credstore is
/// consulted at all.
#[tokio::test]
async fn sync_repo_rejects_a_malformed_credential_ref() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(
        qa_catalog_sdk::TestRepository {
            credential_ref: Some(String::new()),
            ..repo_fixture(repo_id, false)
        },
    ));
    let engine = Arc::new(MockRepoSyncPort::succeeding(vec![], vec![]));
    let svc = build_service_with_credstore(
        repos,
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(UnusedCredStore),
    )
    .await;

    let err = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "credential_ref"),
        "got {err:?}"
    );
    assert!(engine.seen_credentials().is_empty());
}

/// The credential-less case, pinned alongside the others: a public repository
/// hands the engine `None` and never touches credstore (`UnusedCredStore`'s
/// `unimplemented!()` is what enforces the second half).
#[tokio::test]
async fn sync_repo_passes_no_credential_for_a_public_repository() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, false,
    )));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned()],
        vec![],
    ));
    let svc = build_service(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
    )
    .await;

    svc.sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap();
    assert_eq!(engine.seen_credentials(), vec![None]);
}

// ---------------------------------------------------------------------------
// Update path (PRD `cpt-cf-qa-fr-catalog-repos`: "register, update, remove")
// ---------------------------------------------------------------------------

fn update_fixture(name: &str, url: &str) -> TestRepositoryUpdate {
    TestRepositoryUpdate {
        product_id: Uuid::new_v4(),
        name: name.to_owned(),
        url: url.to_owned(),
        default_branch: "main".to_owned(),
        content_root: String::new(),
        credential_ref: None,
    }
}

/// The mutable fields land — including `product_id` and `default_branch`,
/// both updatable (the latter only selects the branch used when a caller
/// names none; it does not identify synced content) — and an update that
/// leaves `url`/`content_root` alone keeps the working copy valid.
#[tokio::test]
async fn update_repo_replaces_mutable_fields_and_keeps_the_working_copy() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let fixture = repo_fixture(repo_id, true);
    let repos = Arc::new(MockTestReposRepository::with_repo(fixture));
    let engine = Arc::new(MockRepoSyncPort::failing("unused"));
    let svc = build_service(Arc::clone(&repos), engine, tmp.path().to_path_buf()).await;

    let new_product_id = Uuid::new_v4();
    let updated = svc
        .update_repo(
            &ctx(tenant_id),
            repo_id,
            TestRepositoryUpdate {
                product_id: new_product_id,
                name: "renamed".to_owned(),
                // Same url as the fixture: no invalidation.
                url: "https://example.com/org/repo.git".to_owned(),
                default_branch: "develop".to_owned(),
                content_root: String::new(),
                credential_ref: Some("qa-catalog-cred".to_owned()),
            },
        )
        .await
        .unwrap();

    assert_eq!(updated.name, "renamed");
    assert_eq!(updated.credential_ref.as_deref(), Some("qa-catalog-cred"));
    assert_eq!(
        updated.product_id, new_product_id,
        "product_id is a mutable field: repo ownership can be re-attributed"
    );
    assert_eq!(
        updated.default_branch, "develop",
        "default_branch is a mutable field: it selects the branch used when \
         no branch is named, and does not identify synced content"
    );
    assert!(
        updated.last_synced_at.is_some(),
        "an update that does not move the content location must keep the synced state"
    );
}

/// Changing `url` (or `content_root`) invalidates the working copy: the synced
/// state is cleared so reads fail closed with `RepoNotSynced` rather than
/// serving content fetched from the OLD url.
#[tokio::test]
async fn update_repo_invalidates_the_working_copy_when_the_content_location_changes() {
    let tenant_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    for update in [
        update_fixture("repo", "https://example.com/org/moved.git"),
        TestRepositoryUpdate {
            content_root: "tests".to_owned(),
            ..update_fixture("repo", "https://example.com/org/repo.git")
        },
    ] {
        let repo_id = Uuid::new_v4();
        let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
            repo_id, true,
        )));
        let engine = Arc::new(MockRepoSyncPort::failing("unused"));
        let svc = build_service(Arc::clone(&repos), engine, tmp.path().to_path_buf()).await;

        let updated = svc
            .update_repo(&ctx(tenant_id), repo_id, update.clone())
            .await
            .unwrap();

        assert_eq!(
            updated.last_synced_at, None,
            "{update:?} must clear the synced state"
        );
        assert_eq!(
            updated.sync_error, None,
            "the row must read as never-synced, not as failed"
        );
        // ...which is exactly what the read gate keys on.
        let err = crate::domain::service::plans::require_synced(&updated, "main").unwrap_err();
        assert!(
            matches!(err, DomainError::RepoNotSynced { .. }),
            "content reads must fail closed after invalidation, got {err:?}"
        );
    }
}

/// The update path enforces the SAME URL policy as create (ADR-0005 as
/// amended) — otherwise it would be the way around it. That cuts both ways:
/// what create rejects, update must reject, and what create accepts (ssh,
/// since 2026-08-27) update must accept.
#[tokio::test]
async fn update_repo_enforces_the_create_url_policy() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(MockRepoSyncPort::failing("unused"));
    let svc = build_service(Arc::clone(&repos), engine, tmp.path().to_path_buf()).await;

    for url in [
        "file:///var/lib/gears/qa-catalog/repos/00000000-0000-0000-0000-000000000000",
        "/var/lib/gears/qa-catalog/repos",
        "git://host/org/repo.git",
        "https://user:token@github.com/org/repo.git",
        "https://token@github.com/org/repo.git",
        // Rejected under ssh too: a password is an embedded secret.
        "ssh://git:token@host/org/repo.git",
    ] {
        let err = svc
            .update_repo(&ctx(tenant_id), repo_id, update_fixture("repo", url))
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { ref field, .. } if field == "url"),
            "expected url validation error for {url}, got {err:?}"
        );
    }

    // ...and the content_root rule, likewise.
    for content_root in ["../outside", "/abs/path", "a/../../b", "a\\..\\b"] {
        let err = svc
            .update_repo(
                &ctx(tenant_id),
                repo_id,
                TestRepositoryUpdate {
                    content_root: content_root.to_owned(),
                    ..update_fixture("repo", "https://example.com/org/repo.git")
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { ref field, .. } if field == "content_root"),
            "expected content_root validation error for {content_root:?}, got {err:?}"
        );
    }

    // Nothing may have been written by any rejected call.
    let repo = svc.get_repo(&ctx(tenant_id), repo_id).await.unwrap();
    assert_eq!(repo.name, "test-repo", "a rejected update must not persist");
    assert_eq!(repo.url, "https://example.com/org/repo.git");
}

#[tokio::test]
async fn update_repo_rejects_an_empty_name() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(MockRepoSyncPort::failing("unused"));
    let svc = build_service(repos, engine, tmp.path().to_path_buf()).await;

    let err = svc
        .update_repo(
            &ctx(tenant_id),
            repo_id,
            update_fixture("", "https://example.com/org/repo.git"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "name"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn update_repo_404s_on_a_repo_outside_the_scope() {
    let tenant_id = Uuid::new_v4();
    let missing_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::none());
    let engine = Arc::new(MockRepoSyncPort::failing("unused"));
    let svc = build_service(repos, engine, tmp.path().to_path_buf()).await;

    let err = svc
        .update_repo(
            &ctx(tenant_id),
            missing_id,
            update_fixture("repo", "https://example.com/org/repo.git"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::NotFound { id } if id == missing_id),
        "expected NotFound, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Branch-cache refresher path (`list_refresh_targets` + `refresh_branches`),
// driven by the lifecycle task in `crate::gear`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_refresh_targets_reports_each_repo_with_its_owning_tenant() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo_in_tenant(
        repo_fixture(repo_id, true),
        tenant_id,
    ));
    let engine = Arc::new(MockRepoSyncPort::succeeding(vec![], vec![]));
    let svc = build_service(Arc::clone(&repos), engine, tmp.path().to_path_buf()).await;

    let targets = svc.list_refresh_targets(&ctx(tenant_id)).await.unwrap();

    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].repo_id, repo_id);
    assert_eq!(
        targets[0].tenant_id, tenant_id,
        "the refresher binds its per-repo system context to this tenant, so it \
         must be the repository's own owner tenant"
    );
}

/// The refresher writes branch rows under the tenant of the *context* it is
/// called with (`crate::domain::system_actor::for_branch_refresh`), so a
/// wrong-tenant regression here would file another tenant's branch cache.
#[tokio::test]
async fn refresh_branches_writes_under_the_context_tenant() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo_in_tenant(
        repo_fixture(repo_id, true),
        tenant_id,
    ));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned(), "release/9.5".to_owned()],
        vec![],
    ));
    let svc = build_service(Arc::clone(&repos), engine, tmp.path().to_path_buf()).await;

    svc.refresh_branches(&ctx(tenant_id), repo_id)
        .await
        .unwrap();

    assert_eq!(
        repos.recorded_branches(),
        vec!["main".to_owned(), "release/9.5".to_owned()],
        "the cache must be replaced with the ls-refs inventory"
    );
    assert_eq!(
        repos.recorded_branch_tenant(),
        Some(tenant_id),
        "branch rows must be written under the context's tenant"
    );
}

#[tokio::test]
async fn refresh_branches_404s_on_a_repo_outside_the_scope() {
    let tenant_id = Uuid::new_v4();
    let missing_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::none());
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned()],
        vec![],
    ));
    let svc = build_service(Arc::clone(&repos), engine, tmp.path().to_path_buf()).await;

    let err = svc
        .refresh_branches(&ctx(tenant_id), missing_id)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::NotFound { id } if id == missing_id),
        "expected NotFound, got {err:?}"
    );
    assert!(
        repos.recorded_branch_tenant().is_none(),
        "no branch rows may be written for an unresolvable repository"
    );
}

/// Unlike `sync_repo`, a refresh failure is returned (for the task to log)
/// and must NOT clobber `sync_error` — that column reports content-sync
/// outcomes. The engine message is sanitized on the way out regardless.
#[tokio::test]
async fn refresh_branches_returns_sanitized_error_and_leaves_sync_error_alone() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let repos = Arc::new(MockTestReposRepository::with_repo_in_tenant(
        repo_fixture(repo_id, true),
        tenant_id,
    ));
    let engine = Arc::new(MockRepoSyncPort::failing(
        "ls-refs of https://x-access-token:supersecret@github.com/org/repo.git failed: 403",
    ));
    let svc = build_service(Arc::clone(&repos), engine, tmp.path().to_path_buf()).await;

    let err = svc
        .refresh_branches(&ctx(tenant_id), repo_id)
        .await
        .unwrap_err();

    let DomainError::SyncFailed { message } = err else {
        panic!("expected SyncFailed, got {err:?}");
    };
    assert!(
        !message.contains("supersecret"),
        "the returned message must never carry credential material: {message}"
    );
    assert!(
        message.contains("https://***@github.com/org/repo.git"),
        "URL userinfo must be redacted, not dropped wholesale: {message}"
    );

    let repo = svc.get_repo(&ctx(tenant_id), repo_id).await.unwrap();
    assert_eq!(
        repo.sync_error, None,
        "a failed branch refresh must not overwrite the content-sync error"
    );
    assert!(
        repos.recorded_branches().is_empty(),
        "a failed refresh must not touch the branch cache"
    );
}

/// SSH remotes are accepted (ADR-0005 as amended 2026-08-27, at the human
/// partner's explicit direction). Both syntaxes must work: the `ssh://` URL
/// form and the scp-like `git@host:path` form, which is what Bitbucket hands
/// out and what the deployment's repositories actually use.
///
/// `git@` here is a *username*, not a secret, which is why the embedded-
/// credential check has to be scheme-aware rather than "any `@` is a
/// credential".
#[tokio::test]
async fn create_repo_allows_ssh_urls() {
    let tenant_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    for url in [
        "ssh://git@bitbucket.org/virtuozzocore/vhp-core.git",
        "ssh://git@bitbucket.org:7999/virtuozzocore/vhp-core.git",
        "ssh://bitbucket.org/virtuozzocore/vhp-core.git",
        "git@bitbucket.org:virtuozzocore/vhp-core.git",
        "bitbucket.org:virtuozzocore/vhp-core.git",
    ] {
        let repos = Arc::new(MockTestReposRepository::none());
        let engine = Arc::new(MockRepoSyncPort::failing("unused"));
        let svc = build_service(repos, engine, tmp.path().to_path_buf()).await;
        let created = svc
            .create_repo(
                &ctx(tenant_id),
                NewTestRepository {
                    product_id: Uuid::new_v4(),
                    name: "repo".to_owned(),
                    url: url.to_owned(),
                    default_branch: "main".to_owned(),
                    content_root: String::new(),
                    credential_ref: None,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("expected {url} to be accepted, got {e:?}"));
        assert_eq!(created.url, url);
    }
}

const SSH_KEY_CREDSTORE_REF: &str = "qa-ssh-key-abc123";
const SSH_KEY_PEM: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaA==\n-----END OPENSSH PRIVATE KEY-----\n";

/// For an **SSH** remote, `credential_ref` names a `qa_ssh_keys` row — not a
/// credstore reference.
///
/// This is forced by two facts that meet here:
///
/// * the UI's repository form sends the SSH-key picker's selection straight
///   through as `credential_ref` (`qa-platform-ui/src/api/adapters.ts:950`,
///   `credential_ref: form.ssh_key_id`), and that value is `SshKeyDto.id`,
///   the `qa_ssh_keys` row id;
/// * `SshKeyDto` deliberately omits `credstore_ref` (`api/rest/dto.rs`, "Do
///   not add it back") because publishing it would hand every tenant member
///   a working read path to another member's key.
///
/// So the client *cannot* supply the credstore reference, and the gear has
/// to do the `id -> credstore_ref -> material` hop itself. Resolving an SSH
/// `credential_ref` directly against credstore — which is what the http path
/// does — fails, because a `qa_ssh_keys` UUID is not a secret reference.
#[tokio::test]
async fn sync_repo_resolves_an_ssh_credential_ref_through_the_ssh_key_row() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let key_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let ssh_repo = qa_catalog_sdk::TestRepository {
        url: "git@bitbucket.org:virtuozzocore/vhp-core.git".to_owned(),
        // What the UI sends: the ssh key's row id.
        credential_ref: Some(key_id.to_string()),
        ..repo_fixture(repo_id, false)
    };

    let repos = Arc::new(MockTestReposRepository::with_repo(ssh_repo));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned()],
        vec![],
    ));
    // The key material sits under the row's `credstore_ref`, NOT under the
    // row id the repository references.
    let credstore = Arc::new(MockCredStoreClient::with_secrets(vec![(
        SSH_KEY_CREDSTORE_REF.to_owned(),
        SSH_KEY_PEM.to_owned(),
    )]));
    let svc = build_service_with_ssh_keys(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        credstore,
        std::time::Duration::ZERO,
        Arc::new(MockSshKeysRepository::with_key(
            key_id,
            SSH_KEY_CREDSTORE_REF,
        )),
    )
    .await;

    let updated = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap();

    assert_eq!(
        updated.sync_error, None,
        "an ssh repo with a registered key must sync"
    );
    assert_eq!(
        engine.seen_credentials(),
        vec![Some(SSH_KEY_PEM.to_owned())],
        "the engine must receive the PRIVATE KEY PEM, resolved via the ssh key row"
    );
}

/// Arm 2's SSH sibling: the `id -> credstore_ref` hop succeeds (the row
/// exists), but credstore has no secret at the resolved `credstore_ref` —
/// the real case in this deployment, where the only credstore plugin is
/// in-memory and every secret is lost on a gears restart. The recorded
/// failure must name the reference that was actually queried
/// (`credstore_ref`), not the caller's `credential_ref` (the `qa_ssh_keys`
/// row id) — those differ here, and a message echoing the row id previously
/// read as "the id->credstore_ref hop is missing" when the hop had, in
/// fact, already succeeded.
#[tokio::test]
async fn sync_repo_names_the_queried_credstore_ref_when_the_secret_is_missing() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let key_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let ssh_repo = qa_catalog_sdk::TestRepository {
        url: "git@bitbucket.org:virtuozzocore/vhp-core.git".to_owned(),
        // What the UI sends: the ssh key's row id, not its credstore_ref.
        credential_ref: Some(key_id.to_string()),
        ..repo_fixture(repo_id, false)
    };

    let repos = Arc::new(MockTestReposRepository::with_repo(ssh_repo));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned()],
        vec![],
    ));
    let svc = build_service_with_ssh_keys(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        // The row resolves cleanly, but credstore has nothing at its
        // credstore_ref — the in-memory plugin's post-restart state.
        Arc::new(MockCredStoreClient::empty()),
        std::time::Duration::ZERO,
        Arc::new(MockSshKeysRepository::with_key(
            key_id,
            SSH_KEY_CREDSTORE_REF,
        )),
    )
    .await;

    let updated = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap();

    let sync_error = updated
        .sync_error
        .expect("a missing credstore secret must be recorded");
    assert!(
        sync_error.contains(SSH_KEY_CREDSTORE_REF),
        "the message must name the reference that was actually queried \
         (the row's credstore_ref), not just the caller's row id: {sync_error}"
    );
    assert!(
        engine.seen_credentials().is_empty(),
        "the engine must not be called when the credential cannot be resolved"
    );
}

/// An SSH remote with **no** `credential_ref` must attempt the clone
/// unauthenticated rather than failing early — public repositories over SSH
/// exist, and `ssh` may still succeed from host configuration.
#[tokio::test]
async fn sync_repo_attempts_an_ssh_clone_without_a_credential() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let ssh_repo = qa_catalog_sdk::TestRepository {
        url: "ssh://git@bitbucket.org/virtuozzocore/vhp-core.git".to_owned(),
        credential_ref: None,
        ..repo_fixture(repo_id, false)
    };
    let repos = Arc::new(MockTestReposRepository::with_repo(ssh_repo));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned()],
        vec![],
    ));
    let svc = build_service(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
    )
    .await;

    let updated = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap();

    assert_eq!(updated.sync_error, None);
    assert_eq!(
        engine.seen_credentials(),
        vec![None],
        "the engine must be reached with no credential, not short-circuited"
    );
}

/// An SSH `credential_ref` naming a key row that does not exist is a
/// recorded sync failure naming the key — not a hard error, and not an
/// opaque credstore miss.
#[tokio::test]
async fn sync_repo_records_a_clear_error_for_an_unregistered_ssh_key() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let key_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    let ssh_repo = qa_catalog_sdk::TestRepository {
        url: "git@bitbucket.org:virtuozzocore/vhp-core.git".to_owned(),
        credential_ref: Some(key_id.to_string()),
        ..repo_fixture(repo_id, false)
    };
    let repos = Arc::new(MockTestReposRepository::with_repo(ssh_repo));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned()],
        vec![],
    ));
    let svc = build_service_with_ssh_keys(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        std::time::Duration::ZERO,
        // No key row at all.
        Arc::new(MockSshKeysRepository::none()),
    )
    .await;

    let updated = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .unwrap();

    let sync_error = updated.sync_error.expect("an unregistered key must fail");
    assert!(
        sync_error.contains("ssh key") && sync_error.contains(&key_id.to_string()),
        "the error must name the unregistered key: {sync_error}"
    );
    assert!(
        engine.seen_credentials().is_empty(),
        "the engine must not be reached when the key cannot be resolved"
    );
}

/// The other half of "update enforces the same policy as create": the ssh
/// forms create accepts, update must accept too. Kept separate from
/// [`update_repo_enforces_the_create_url_policy`] because a *successful*
/// update writes, and that test ends by asserting nothing was written.
#[tokio::test]
async fn update_repo_accepts_the_ssh_urls_create_accepts() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();

    for url in ["ssh://git@host/org/repo.git", "git@github.com:org/repo.git"] {
        let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
            repo_id, true,
        )));
        let engine = Arc::new(MockRepoSyncPort::failing("unused"));
        let svc = build_service(repos, engine, tmp.path().to_path_buf()).await;

        let updated = svc
            .update_repo(&ctx(tenant_id), repo_id, update_fixture("repo", url))
            .await
            .unwrap_or_else(|e| panic!("expected {url} to be accepted on update, got {e:?}"));
        assert_eq!(updated.url, url);
    }
}
