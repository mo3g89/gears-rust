#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit tests for `ReposService`: sync bookkeeping (branch cache +
//! timestamp / sanitized error recording) and URL credential hygiene.
//!
//! The sync engine is an in-memory `RepoSyncPort` double that can be
//! programmed to succeed (optionally writing fixture files into the branch
//! snapshot, like the real gix adapter does) or fail with a given message.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::test_util::MockCredStoreClient;
use credstore_sdk::{CredStoreClientV1, SharingMode};
use qa_catalog_sdk::{NewTestRepository, TestRepositoryUpdate};
use uuid::Uuid;

use super::branch_snapshot::BranchSync;
use super::repos::{ReposService, UNREADABLE_CREDENTIAL_HINT};
use super::sync_cache::SyncCache;
use super::test_support::{
    LsRefsFailure, MockRepoSyncPort, MockSshKeysRepository, MockTestReposRepository,
    PermissiveAuthZ, SharingCredStore, StoredSecret, ctx, repo_fixture, test_db_provider,
};
use crate::domain::error::DomainError;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

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
/// cannot express `AccessDenied`, which must be recorded exactly as a miss is.
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

/// Credstore double that holds nothing and counts its lookups — for the
/// backoff tests that pin a credential fault is not re-resolved inside the
/// window.
#[derive(Default)]
struct CountingEmptyCredStore {
    gets: std::sync::Mutex<usize>,
}

impl CountingEmptyCredStore {
    fn gets(&self) -> usize {
        *self.gets.lock().unwrap()
    }
}

#[async_trait]
impl CredStoreClientV1 for CountingEmptyCredStore {
    async fn get(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _key: &credstore_sdk::SecretRef,
    ) -> Result<Option<credstore_sdk::GetSecretResponse>, credstore_sdk::CredStoreError> {
        *self.gets.lock().unwrap() += 1;
        Ok(None)
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

/// A service over a caller-built sync cache and credstore, for the backoff
/// tests.
async fn build_service_with_cache(
    repos: Arc<MockTestReposRepository>,
    engine: Arc<MockRepoSyncPort>,
    repos_dir: PathBuf,
    credstore: Arc<dyn CredStoreClientV1>,
    cache: SyncCache,
) -> ReposService<MockTestReposRepository, MockSshKeysRepository> {
    ReposService::new(
        test_db_provider().await,
        repos,
        Arc::new(MockSshKeysRepository::none()),
        credstore,
        engine,
        repos_dir,
        Arc::new(cache),
        PolicyEnforcer::new(Arc::new(PermissiveAuthZ)),
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
        // Ordered: the cache is a set, as `replace_branches` stores it.
        vec!["dev".to_owned(), "main".to_owned()],
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

/// An explicit sync reads the repository's credential as the qa-catalog system
/// actor, the identity the background branch refresher reads it as, bound to
/// the repository's owning tenant. So a secret with `private` sharing, readable
/// by its owner only, fails the owner's own sync with the reason, instead of
/// passing it and then failing every refresher pass. Authorization of the sync
/// stays the caller's.
#[tokio::test]
async fn an_explicit_sync_reads_the_credential_as_the_system_actor_and_names_sharing() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let owner = ctx(tenant_id);
    let store = |sharing| -> Arc<dyn CredStoreClientV1> {
        Arc::new(SharingCredStore::new(vec![StoredSecret {
            reference: CRED_REF,
            value: CRED_MATERIAL,
            tenant: tenant_id,
            owner: owner.subject_id(),
            sharing,
        }]))
    };
    let sync = |sharing| {
        let owner = owner.clone();
        async move {
            let tmp = tempfile::tempdir().unwrap();
            let repos = Arc::new(MockTestReposRepository::with_repo_in_tenant(
                credentialed_repo(repo_id),
                tenant_id,
            ));
            let engine = Arc::new(MockRepoSyncPort::succeeding(
                vec!["main".to_owned()],
                vec![],
            ));
            let svc = build_service_with_credstore(
                Arc::clone(&repos),
                Arc::clone(&engine),
                tmp.path().to_path_buf(),
                store(sharing),
            )
            .await;
            let row = svc.sync_repo(&owner, repo_id, "", true).await;
            (row, engine)
        }
    };

    let (row, engine) = sync(SharingMode::Private).await;
    let reason = row
        .expect("an unreadable credential is a recorded sync failure, not an error")
        .sync_error
        .expect("the failure is recorded");
    assert!(reason.contains(UNREADABLE_CREDENTIAL_HINT), "{reason}");
    assert!(
        reason.contains(CRED_REF),
        "the reason still names the reference: {reason}"
    );
    assert!(
        engine.seen_credentials().is_empty(),
        "nothing reaches the remote"
    );

    let (row, engine) = sync(SharingMode::Tenant).await;
    assert_eq!(row.expect("a tenant-shared secret syncs").sync_error, None);
    assert_eq!(
        engine.seen_credentials(),
        vec![Some(CRED_MATERIAL.to_owned())]
    );
}

/// The actor is bound to the tenant that **owns** the repository, the tenant
/// the background refresher binds to, not to the caller's. A caller of another
/// tenant whose scope reaches the repository (a parent tenant over its child)
/// reads the owner's `tenant`-shared secret, exactly as the refresher does;
/// bound to the caller's tenant, the read would miss in the sync and succeed in
/// the refresh.
#[tokio::test]
async fn an_explicit_sync_by_a_caller_of_another_tenant_reads_the_credential_in_the_owning_tenant()
{
    let owning_tenant = Uuid::new_v4();
    let caller_tenant = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo_in_tenant(
        credentialed_repo(repo_id),
        owning_tenant,
    ));
    let engine = Arc::new(MockRepoSyncPort::succeeding(
        vec!["main".to_owned()],
        vec![],
    ));
    let svc = build_service_with_credstore(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(SharingCredStore::new(vec![StoredSecret {
            reference: CRED_REF,
            value: CRED_MATERIAL,
            tenant: owning_tenant,
            owner: Uuid::new_v4(),
            sharing: SharingMode::Tenant,
        }])),
    )
    .await;

    let row = svc
        .sync_repo(&ctx(caller_tenant), repo_id, "", true)
        .await
        .expect("the sync is authorized under the caller's own context");
    assert_eq!(row.sync_error, None, "the owning tenant's secret is read");
    assert_eq!(
        engine.seen_credentials(),
        vec![Some(CRED_MATERIAL.to_owned())]
    );
}

/// Arm 3 — credstore denies the read: a recorded sync failure carrying the
/// same reason as a miss, not `Forbidden`. The caller was authorized for the
/// sync; what credstore refused is the gear's own read of a secret the
/// repository names, which is a fact about the repository's configuration.
#[tokio::test]
async fn sync_repo_records_a_credstore_access_denied_as_the_unreadable_credential_reason() {
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

    let updated = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .expect("a refused credential read is recorded, not raised");
    let sync_error = updated.sync_error.expect("the refusal is recorded");
    assert!(sync_error.contains(CRED_REF), "got {sync_error}");
    assert!(
        sync_error.contains(UNREADABLE_CREDENTIAL_HINT),
        "got {sync_error}"
    );
    assert!(engine.seen_credentials().is_empty());
}

/// credstore's `NotFound` error is the same miss as `Ok(None)`: recorded with
/// the reason, never a `CredStore` infrastructure error.
#[tokio::test]
async fn sync_repo_records_a_credstore_not_found_error_as_the_unreadable_credential_reason() {
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
        Arc::new(MockCredStoreClient::erroring_not_found()),
    )
    .await;

    let updated = svc
        .sync_repo(&ctx(tenant_id), repo_id, "", true)
        .await
        .expect("a missing secret is recorded, not raised");
    let sync_error = updated.sync_error.expect("the miss is recorded");
    assert!(
        sync_error.contains(UNREADABLE_CREDENTIAL_HINT),
        "got {sync_error}"
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
/// state is cleared so the read gate refuses what is on disk, and a read syncs
/// the branch from the new location (`branch_snapshot`) rather than serving
/// content fetched from the OLD url.
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
            "content reads must not be served from the old snapshot, got {err:?}"
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
    assert_eq!(
        repos.replace_calls(),
        0,
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

// ---------------------------------------------------------------------------
// The failure backoff (DESIGN §3.3 "Branch model and the first read of a branch")
// ---------------------------------------------------------------------------

/// DESIGN §3.3: a remote that failed to list is not asked again by every read
/// inside the backoff window; an explicit sync is not held back by it, and its
/// success ends the backoff.
#[tokio::test]
async fn a_remote_that_failed_to_list_is_not_asked_again_within_the_backoff() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::Unreachable),
    );
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;
    let repo = repos.current().unwrap();

    for _ in 0..3 {
        let err = svc
            .sync_branch_for_read(&ctx(tenant_id), &repo, "main")
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::SyncFailed { .. }), "got {err:?}");
    }
    assert_eq!(
        engine.ls_refs_calls(),
        1,
        "a down remote is asked once per backoff window"
    );

    engine.set_ls_refs_failure(None);
    svc.sync_repo(&ctx(tenant_id), repo_id, "main", true)
        .await
        .expect("the explicit sync is not behind the backoff");
    svc.sync_branch_for_read(&ctx(tenant_id), &repo, "main")
        .await
        .expect("the successful sync ended the backoff");
    assert_eq!(engine.ls_refs_calls(), 2);
}

/// `remote_failure_backoff_seconds: 0` asks the remote on every read.
#[tokio::test]
async fn a_zero_backoff_asks_the_remote_on_every_read() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::Unreachable),
    );
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO),
    )
    .await;
    let repo = repos.current().unwrap();

    for _ in 0..2 {
        svc.sync_branch_for_read(&ctx(tenant_id), &repo, "main")
            .await
            .unwrap_err();
    }
    assert_eq!(engine.ls_refs_calls(), 2);
}

/// The backoff a repository is in ends when what it failed with changes: a
/// new credential is a new answer, so the next read asks the remote.
#[tokio::test]
async fn a_credential_change_ends_the_backoff() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(
        qa_catalog_sdk::TestRepository {
            credential_ref: Some("cred-old".to_owned()),
            ..repo_fixture(repo_id, true)
        },
    ));
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::CredentialRejected),
    );
    let credstore = Arc::new(MockCredStoreClient::with_secrets(vec![
        ("cred-old".to_owned(), "deploy:old-token".to_owned()),
        ("cred-new".to_owned(), "deploy:new-token".to_owned()),
    ]));
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        credstore,
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;

    for _ in 0..2 {
        let row = svc
            .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
            .await
            .expect("a credential fault is answered as the recorded row");
        assert!(row.sync_error.is_some());
    }
    assert_eq!(engine.ls_refs_calls(), 1);

    let current = repos.current().unwrap();
    svc.update_repo(
        &ctx(tenant_id),
        repo_id,
        TestRepositoryUpdate {
            product_id: current.product_id,
            name: current.name.clone(),
            url: current.url.clone(),
            default_branch: current.default_branch.clone(),
            content_root: current.content_root.clone(),
            credential_ref: Some("cred-new".to_owned()),
        },
    )
    .await
    .unwrap();
    engine.set_ls_refs_failure(None);

    let row = svc
        .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .expect("the new credential is tried at once");
    assert_eq!(
        engine.ls_refs_calls(),
        2,
        "the credential change ended the backoff"
    );
    assert_eq!(
        row.sync_error, None,
        "and the successful sync cleared the recorded fault"
    );
}

/// A new url is a new remote: the backoff of the old one does not apply.
#[tokio::test]
async fn a_url_change_ends_the_backoff() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::Unreachable),
    );
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;

    svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .unwrap_err();
    svc.update_repo(
        &ctx(tenant_id),
        repo_id,
        update_fixture("test-repo", "https://example.com/org/moved.git"),
    )
    .await
    .unwrap();
    svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .unwrap_err();
    assert_eq!(
        engine.ls_refs_calls(),
        2,
        "the moved repository's remote is asked"
    );
}

/// A forced sync is an operator asking for the remote now: it ends the backoff
/// before it tries, whether or not it then succeeds.
#[tokio::test]
async fn a_forced_sync_ends_the_backoff_even_when_it_fails() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(
        MockRepoSyncPort::failing_sync_of(
            vec!["main".to_owned()],
            "fetch failed: connection reset",
        )
        .with_ls_refs_failure(LsRefsFailure::Unreachable),
    );
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;

    svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .unwrap_err();
    svc.sync_repo(&ctx(tenant_id), repo_id, "main", true)
        .await
        .expect("the failure is recorded on the returned row");
    svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .unwrap_err();
    assert_eq!(
        engine.ls_refs_calls(),
        2,
        "the forced sync ended the backoff"
    );
}

/// The refresher is not held back by the backoff, and its successful listing
/// ends it: the remote just answered, so the next read asks it again instead of
/// answering 503 for the rest of the window.
#[tokio::test]
async fn a_successful_refresh_ends_the_backoff() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::Unreachable),
    );
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;

    svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .unwrap_err();
    engine.set_ls_refs_failure(None);
    svc.refresh_branches(&ctx(tenant_id), repo_id)
        .await
        .expect("the refresher lists inside the backoff window");
    svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .expect("the successful refresh ended the backoff");
    assert_eq!(engine.ls_refs_calls(), 3);
}

/// An explicit sync the remote refuses the credential for records the fault
/// and backs the repository off like the lazy read does: the next read answers
/// the recorded reason without contacting the remote.
#[tokio::test]
async fn an_explicit_sync_whose_credential_is_refused_backs_the_reads_off() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::CredentialRejected),
    );
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;

    let row = svc
        .sync_repo(&ctx(tenant_id), repo_id, "main", true)
        .await
        .expect("the failure is recorded on the returned row");
    assert!(row.sync_error.is_some());
    let row = svc
        .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .expect("a credential fault is answered as the recorded row");
    assert!(row.sync_error.is_some());
    assert_eq!(
        engine.ls_refs_calls(),
        0,
        "the read inside the backoff does not ask the remote"
    );
}

/// A non-forced explicit sync that succeeds ends the backoff too: the remote
/// just answered.
#[tokio::test]
async fn a_successful_explicit_sync_ends_the_backoff() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::Unreachable),
    );
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;

    svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .unwrap_err();
    engine.set_ls_refs_failure(None);
    svc.sync_repo(&ctx(tenant_id), repo_id, "main", false)
        .await
        .expect("the explicit sync is not behind the backoff");
    svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .expect("the successful sync ended the backoff");
    assert_eq!(engine.ls_refs_calls(), 2);
}

/// A credential the store cannot resolve backs the repository off too, whether
/// a read or an explicit sync found it: inside the window, reads answer the
/// recorded reason without looking the credential up again.
#[tokio::test]
async fn an_unresolvable_credential_is_not_looked_up_again_within_the_backoff() {
    let tenant_id = Uuid::new_v4();
    for explicit_first in [false, true] {
        let repo_id = Uuid::new_v4();
        let tmp = tempfile::tempdir().unwrap();
        let repos = Arc::new(MockTestReposRepository::with_repo(
            qa_catalog_sdk::TestRepository {
                credential_ref: Some("cred-missing".to_owned()),
                ..repo_fixture(repo_id, true)
            },
        ));
        let engine = Arc::new(MockRepoSyncPort::succeeding(
            vec!["main".to_owned()],
            vec![],
        ));
        let credstore = Arc::new(CountingEmptyCredStore::default());
        let svc = build_service_with_cache(
            Arc::clone(&repos),
            Arc::clone(&engine),
            tmp.path().to_path_buf(),
            Arc::clone(&credstore) as Arc<dyn CredStoreClientV1>,
            SyncCache::new(std::time::Duration::ZERO)
                .with_failure_backoff(std::time::Duration::from_secs(30)),
        )
        .await;

        if explicit_first {
            svc.sync_repo(&ctx(tenant_id), repo_id, "main", true)
                .await
                .expect("the failure is recorded on the returned row");
        } else {
            svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
                .await
                .expect("a credential fault is answered as the recorded row");
        }
        for _ in 0..2 {
            let row = svc
                .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
                .await
                .expect("a credential fault is answered as the recorded row");
            assert!(
                row.sync_error
                    .as_deref()
                    .is_some_and(|e| e.contains("cred-missing")),
                "{row:?}"
            );
        }
        assert_eq!(
            credstore.gets(),
            1,
            "explicit first: {explicit_first}: looked up once per backoff window"
        );
        assert_eq!(engine.ls_refs_calls(), 0);
    }
}

/// A credential fixed while a read was listing the remote with the old one:
/// the old credential's refusal is stale by the time it would be written. It is
/// not written over the fixed row and does not back the repository off — the
/// next read asks the remote with the new credential at once.
#[tokio::test]
async fn a_credential_fault_found_before_the_credential_changed_is_not_recorded() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(
        qa_catalog_sdk::TestRepository {
            credential_ref: Some("cred-old".to_owned()),
            ..repo_fixture(repo_id, true)
        },
    ));
    let repos_in_hook = Arc::clone(&repos);
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::CredentialRejected)
            .with_ls_refs_hook(move || repos_in_hook.set_credential_ref(Some("cred-new"))),
    );
    let credstore = Arc::new(MockCredStoreClient::with_secrets(vec![
        ("cred-old".to_owned(), "deploy:old-token".to_owned()),
        ("cred-new".to_owned(), "deploy:new-token".to_owned()),
    ]));
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        credstore,
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;

    let row = svc
        .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .expect("a stale fault is not an error either");
    assert_eq!(row.sync_error, None, "the stale refusal is not answered");
    assert_eq!(
        repos.current().unwrap().sync_error,
        None,
        "nor written over the fixed row"
    );

    engine.set_ls_refs_failure(None);
    let row = svc
        .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .expect("the new credential is tried at once");
    assert_eq!(
        engine.ls_refs_calls(),
        2,
        "no backoff was left for the stale attempt"
    );
    assert_eq!(row.sync_error, None);
}

/// The branch refresher records nothing, so a credential fault it finds starts
/// no backoff: with another branch's failure in `sync_error`, a read of a fresh
/// branch must not be answered with that other branch's reason.
#[tokio::test]
async fn a_refresher_credential_fault_does_not_answer_reads_with_another_branchs_reason() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(
        qa_catalog_sdk::TestRepository {
            sync_error: Some(
                "repository sync failed: branch '26.8' not found on the remote".to_owned(),
            ),
            ..repo_fixture(repo_id, true)
        },
    ));
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::CredentialRejected),
    );
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;

    svc.refresh_branches(&ctx(tenant_id), repo_id)
        .await
        .unwrap_err();
    engine.set_ls_refs_failure(None);
    let row = svc
        .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .expect("the read asks the remote itself");
    assert_eq!(engine.ls_refs_calls(), 2);
    assert_eq!(
        row.sync_error, None,
        "and the successful sync clears 26.8's error"
    );
}

/// A credential backoff answers only the reason it recorded. Once another
/// failure has replaced that text in the repository-wide `sync_error`, the
/// backoff no longer speaks for the row: the next read asks the remote.
#[tokio::test]
async fn a_credential_backoff_does_not_answer_a_reason_it_did_not_record() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::CredentialRejected),
    );
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;

    svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .expect("a credential fault is answered as the recorded row");
    repos.set_sync_error(Some("another branch failed"));
    engine.set_ls_refs_failure(None);

    let row = svc
        .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .expect("the remote is asked, not the backoff");
    assert_eq!(engine.ls_refs_calls(), 2);
    assert_eq!(row.sync_error, None);
}

/// The service a timed-out content sync is driven through: the remote lists
/// `main`, and every content sync of it times out.
async fn service_whose_sync_times_out(
    backoff: std::time::Duration,
) -> (
    ReposService<MockTestReposRepository, MockSshKeysRepository>,
    Arc<MockTestReposRepository>,
    Arc<MockRepoSyncPort>,
    tempfile::TempDir,
) {
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(MockRepoSyncPort::failing_sync_with(
        vec!["main".to_owned()],
        || DomainError::RemoteTimedOut {
            message: "sync did not finish within 300 s and was stopped".to_owned(),
        },
    ));
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO).with_failure_backoff(backoff),
    )
    .await;
    (svc, repos, engine, tmp)
}

/// A content sync that timed out is recorded (the explicit-sync contract), so
/// the read that ran it answers that reason, and it backs the repository off
/// for that recorded reason: the next read inside the window answers the same
/// recorded row (400 through `require_synced`) at once, with no listing and no
/// sync — not another `sync_timeout_seconds` (DESIGN §3.3 "Limits on talking
/// to a remote").
#[tokio::test]
async fn a_timed_out_sync_is_recorded_and_answered_from_the_record_within_the_backoff() {
    let tenant_id = Uuid::new_v4();
    let (svc, repos, engine, _tmp) =
        service_whose_sync_times_out(std::time::Duration::from_secs(30)).await;

    let first = svc
        .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .unwrap();
    let recorded = first.sync_error.clone();
    assert!(
        recorded
            .as_deref()
            .is_some_and(|e| e.contains("did not finish")),
        "got {recorded:?}"
    );
    assert_eq!(engine.ls_refs_calls(), 1);
    assert_eq!(engine.synced_branches().len(), 1);

    let second = svc
        .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .expect("a timed-out sync is answered from its record, not as an outage");
    assert_eq!(second.sync_error, recorded);
    assert_eq!(
        engine.ls_refs_calls(),
        1,
        "the remote is not listed again inside the window"
    );
    assert_eq!(
        engine.synced_branches().len(),
        1,
        "the content is not synced again inside the window"
    );
}

/// Once the window ends, a read of a repository whose sync timed out asks the
/// remote again: it lists and syncs.
#[tokio::test]
async fn a_timed_out_sync_is_retried_once_the_backoff_ends() {
    let tenant_id = Uuid::new_v4();
    let (svc, repos, engine, _tmp) =
        service_whose_sync_times_out(std::time::Duration::from_millis(200)).await;

    svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let row = svc
        .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .unwrap();
    assert!(row.sync_error.is_some());
    assert_eq!(engine.ls_refs_calls(), 2);
    assert_eq!(engine.synced_branches().len(), 2);
}

/// A branch listing that times out stays an outage: `503`, nothing recorded,
/// and the next read inside the window answers `503` without listing again.
#[tokio::test]
async fn a_timed_out_listing_is_an_outage_and_records_nothing() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::TimedOut),
    );
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;

    for _ in 0..2 {
        let err = svc
            .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::SyncFailed { .. }), "got {err:?}");
    }
    assert_eq!(engine.ls_refs_calls(), 1);
    assert!(engine.synced_branches().is_empty());
    assert_eq!(repos.current().unwrap().sync_error, None);
}

/// An over-budget repository is recorded and backed off as a configuration
/// fault: the next read answers the recorded reason without fetching again.
#[tokio::test]
async fn an_over_budget_sync_is_recorded_and_answered_from_the_record_within_the_backoff() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let repos = Arc::new(MockTestReposRepository::with_repo(repo_fixture(
        repo_id, true,
    )));
    let engine = Arc::new(MockRepoSyncPort::failing_sync_with(
        vec!["main".to_owned()],
        || DomainError::SyncBudgetExceeded {
            message: "the fetched pack grew past max_fetch_bytes (1073741824 bytes)".to_owned(),
        },
    ));
    let svc = build_service_with_cache(
        Arc::clone(&repos),
        Arc::clone(&engine),
        tmp.path().to_path_buf(),
        Arc::new(MockCredStoreClient::empty()),
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    )
    .await;

    for _ in 0..2 {
        let row = svc
            .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
            .await
            .unwrap();
        assert!(
            row.sync_error
                .as_deref()
                .is_some_and(|e| e.contains("max_fetch_bytes"))
        );
    }
    assert_eq!(
        engine.synced_branches().len(),
        1,
        "a too-large repository is not fetched again inside the window"
    );
    assert_eq!(engine.ls_refs_calls(), 1);
}

/// A listing of the old remote that fails after the repository moved must not
/// back the moved repository off: the backoff generation the attempt carries
/// is read before its row, so a url change (which ends the backoff) landing
/// between the two leaves the attempt stale. Deterministic: the change lands
/// inside the attempt's own row read.
#[tokio::test]
async fn a_listing_of_a_remote_the_repository_moved_away_from_does_not_back_the_new_one_off() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let tmp = tempfile::tempdir().unwrap();
    let cache = Arc::new(
        SyncCache::new(std::time::Duration::ZERO)
            .with_failure_backoff(std::time::Duration::from_secs(30)),
    );
    let ending = Arc::clone(&cache);
    // The url swap and the backoff's end are what `update_repo` does for a
    // url change; both land right after the attempt has read the old row.
    let repos = Arc::new(
        MockTestReposRepository::with_repo_swapping_url(
            repo_fixture(repo_id, true),
            "https://example.com/org/moved.git",
        )
        .with_after_get_hook(move || async move { ending.clear_backoff(repo_id).await }),
    );
    let engine = Arc::new(
        MockRepoSyncPort::succeeding(vec!["main".to_owned()], vec![])
            .with_ls_refs_failure(LsRefsFailure::Unreachable),
    );
    let port: Arc<dyn crate::domain::ports::repo_sync::RepoSyncPort> = engine.clone();
    let svc = ReposService::new(
        test_db_provider().await,
        Arc::clone(&repos),
        Arc::new(MockSshKeysRepository::none()),
        Arc::new(MockCredStoreClient::empty()),
        port,
        tmp.path().to_path_buf(),
        Arc::clone(&cache),
        PolicyEnforcer::new(Arc::new(PermissiveAuthZ)),
    );

    let err = svc
        .sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::SyncFailed { .. }), "got {err:?}");
    assert!(
        cache.backoff(repo_id).await.is_none(),
        "the old remote's failure backed the moved repository off"
    );
    svc.sync_branch_for_read(&ctx(tenant_id), &repos.current().unwrap(), "main")
        .await
        .unwrap_err();
    assert_eq!(
        engine.ls_refs_calls(),
        2,
        "the moved repository's remote is asked"
    );
}
