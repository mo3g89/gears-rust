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
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use flate2::read::GzDecoder;
use qa_catalog_sdk::{BundleRequest, TestBundle};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, pep_properties};
use uuid::Uuid;

use super::bundles::{BundleDownloadSigningSecret, BundlesService};
use super::test_support::{
    MockTestReposRepository, PermissiveAuthZ, ctx, repo_fixture, test_db_provider,
};
use crate::domain::error::DomainError;
use crate::domain::ports::bundle_store::BundleStore;
use crate::domain::ports::metrics::NoopMetrics;
use crate::domain::repos::BundlesRepository;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

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
            return Err(DomainError::database("descriptor write failed"));
        }
        self.rows
            .lock()
            .unwrap()
            .insert(bundle.id, (tenant_id, bundle.clone()));
        Ok(bundle)
    }

    /// **Scope-aware, unlike the first cut of this double.** The real
    /// `SecureORM` read filters on `owner_tenant_id`, which is precisely what
    /// makes `get_bundle_content` unable to serve one tenant's bundle to
    /// another. A double that ignored the scope would make the cross-tenant
    /// download tests pass whatever the production code did.
    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TestBundle>, DomainError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .get(&id)
            .filter(|(tenant_id, _)| Self::scope_admits(scope, *tenant_id))
            .map(|(_, bundle)| bundle.clone()))
    }

    async fn tenant_of<C: DBRunner>(
        &self,
        _runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Uuid>, DomainError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .get(&id)
            .filter(|(tenant_id, _)| Self::scope_admits(scope, *tenant_id))
            .map(|(tenant_id, _)| *tenant_id))
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

/// The signing secret every service built here uses. Comfortably over
/// [`super::bundles::MIN_SIGNING_SECRET_LEN`]: the download path fails closed
/// under a short one, so a blank here would turn every signature assertion
/// below into "this deployment has no secret" and a broken verification would
/// still look green.
const TEST_SIGNING_SECRET: &str = "a-test-bundle-download-signing-secret";

async fn build_service(
    bundles: Arc<MockBundlesRepository>,
    repos: Arc<MockTestReposRepository>,
    store: Arc<InMemoryBundleStore>,
    repos_dir: PathBuf,
) -> BundlesService<MockBundlesRepository, MockTestReposRepository> {
    build_service_with_secret(bundles, repos, store, repos_dir, TEST_SIGNING_SECRET).await
}

/// [`build_service`] with the signing secret named, for the fail-closed tests.
async fn build_service_with_secret(
    bundles: Arc<MockBundlesRepository>,
    repos: Arc<MockTestReposRepository>,
    store: Arc<InMemoryBundleStore>,
    repos_dir: PathBuf,
    secret: &str,
) -> BundlesService<MockBundlesRepository, MockTestReposRepository> {
    let enforcer = PolicyEnforcer::new(Arc::new(PermissiveAuthZ));
    let db = test_db_provider().await;
    BundlesService::new(
        db,
        bundles,
        repos,
        store,
        repos_dir,
        TEST_TTL,
        BundleDownloadSigningSecret(secret.to_owned()),
        Arc::new(NoopMetrics),
        enforcer,
    )
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

/// `sha256_hex` is SHA-256 — the standard digest, not merely whatever the
/// current provider computes.
///
/// Every other assertion about a checksum in this file compares `sha256_hex`
/// against `sha256_hex`, so all of them stay green under a provider that
/// computes something else entirely, and a `checksum_sha256` column full of
/// values no other tool agrees with would look exactly like this suite passing.
/// These vectors are the published SHA-256 test vectors (FIPS 180-4, and the
/// empty-input digest), so they are external to this crate and to its hasher.
///
/// This test was added when the hasher moved from `sha2` to `aws-lc-rs`
/// (DE0708). The move is a provider swap that must not change one byte of
/// output, because `qa_test_bundles.checksum_sha256` and the branch directory
/// names on disk were written by the old one — and nothing in the suite could
/// have told the difference.
#[test]
fn the_bundle_checksum_is_standard_sha256() {
    for (input, expected) in [
        (
            &b""[..],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            &b"abc"[..],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            &b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"[..],
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ] {
        assert_eq!(
            sha256_hex(input),
            expected,
            "sha256_hex must agree with the published SHA-256 vectors, not \
             merely with itself"
        );
    }
}

// ---------------------------------------------------------------------------
// The per-bundle download signature
//
// `GET /qa/v1/test-bundles/{id}?sig=...` is registered `.anonymous().exposed()`,
// so `sig` is not defence in depth -- it is the only thing between a caller and
// a tenant's test content. These tests are therefore written to fail closed in
// both directions: a tag that should work must work (or every run in every
// deployment stops fetching its tests), and a tag that should not must not.
//
// They replaced a credential rather than joining one. Before this, a runner pod
// carried the confidential secret of a `fullScopeAllowed` Keycloak client with
// a tenant_id claim hardcoded to the seed tenant -- readable by the
// tenant-authored pytest the pod exists to run, and wrong for every tenant but
// one. `a_second_tenant_can_fetch_its_own_bundle` is the test for that second
// half: it could not have passed before, whatever the code did, because the
// claim named one tenant.
// ---------------------------------------------------------------------------

/// Build a bundle for `tenant_id` and hand back the whole descriptor, tag
/// included. The one place these tests obtain a legitimately minted tag —
/// nothing here recomputes one by hand, so a test cannot agree with a broken
/// signer by copying its arithmetic.
async fn seed_signed_bundle(
    svc: &BundlesService<MockBundlesRepository, MockTestReposRepository>,
    tenant_id: Uuid,
    repo_id: Uuid,
) -> TestBundle {
    svc.create_bundle(
        &ctx(tenant_id),
        BundleRequest {
            repo_id,
            branch: "main".to_owned(),
            files: vec!["tests/test_a.py".to_owned()],
        },
    )
    .await
    .expect("the fixture bundle must build")
}

/// **A bundle built by this gear can be fetched with the tag it returned, and
/// the bytes are the bundle's own.**
///
/// The happy path, and the one that must never be allowed to fail quietly: a
/// regression here stops every run in every deployment from fetching its tests,
/// and the symptom (`no tests collected`) looks like a plan problem.
///
/// The checksum comparison is what makes this more than "a 200 came back": it
/// pins that the signed path serves the same bytes the unsigned, authenticated
/// path does, rather than some other bundle that happens to exist.
#[tokio::test]
async fn a_valid_signature_serves_the_bundles_own_bytes() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let svc = build_service(
        Arc::new(MockBundlesRepository::default()),
        repos,
        Arc::new(InMemoryBundleStore::default()),
        tmp.path().to_path_buf(),
    )
    .await;

    let bundle = seed_signed_bundle(&svc, tenant_id, repo_id).await;
    assert!(
        !bundle.download_sig.is_empty(),
        "create_bundle must return a download tag -- an empty one means the \
         dispatcher has nothing to put on TEST_BUNDLE_URL and the pod fetches \
         nothing"
    );
    assert!(
        bundle.download_sig.chars().all(|c| c.is_ascii_hexdigit()),
        "the tag must be hex: it travels in a URL query string and anything \
         else would need escaping the adapter does not apply: {}",
        bundle.download_sig
    );

    let bytes = svc
        .get_bundle_content_signed(bundle.id, &bundle.download_sig)
        .await
        .expect("a bundle's own tag must serve it");

    assert_eq!(
        sha256_hex(&bytes),
        bundle.checksum_sha256,
        "the signed path must serve THIS bundle's bytes, not merely some 200"
    );
}

/// **A tampered, an empty and a non-hex tag are all refused, with one status.**
///
/// Three refusal paths inside `verify_download_signature` — malformed,
/// mismatched, and (below) unconfigured — and one `Forbidden` out of all of
/// them. That merge is deliberate: a response that distinguished them would
/// hand a caller a free oracle for which guess was closer. The distinction
/// survives only on the metric.
///
/// The tampered case flips the FIRST hex digit rather than appending: an
/// implementation that compared prefixes, or that truncated, would survive an
/// appended byte.
#[tokio::test]
async fn a_tampered_or_missing_signature_is_refused() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let svc = build_service(
        Arc::new(MockBundlesRepository::default()),
        repos,
        Arc::new(InMemoryBundleStore::default()),
        tmp.path().to_path_buf(),
    )
    .await;

    let bundle = seed_signed_bundle(&svc, tenant_id, repo_id).await;

    let mut flipped: Vec<char> = bundle.download_sig.chars().collect();
    flipped[0] = if flipped[0] == '0' { '1' } else { '0' };
    let flipped: String = flipped.into_iter().collect();
    assert_ne!(flipped, bundle.download_sig);

    let truncated = &bundle.download_sig[..bundle.download_sig.len() - 2];

    for (label, signature) in [
        ("one flipped hex digit", flipped.as_str()),
        ("a truncated tag", truncated),
        ("an empty tag", ""),
        ("a non-hex tag", "not-a-hex-signature"),
        ("an odd-length hex tag", "abc"),
    ] {
        let err = svc
            .get_bundle_content_signed(bundle.id, signature)
            .await
            .expect_err(&format!("{label} must be refused"));
        assert!(
            matches!(err, DomainError::Forbidden),
            "{label} must answer Forbidden and nothing more specific -- every \
             refusal path shares one status so the response is not an oracle: \
             got {err:?}"
        );
    }
}

/// **A second tenant can fetch its own bundle. This is the multi-tenancy bug
/// the signature fixes, not a side benefit of it.**
///
/// Before this change the runner authenticated as a Keycloak client whose
/// `tenant_id` claim was HARDCODED to `.Values.seedTenantId`, and
/// `get_bundle_content` scopes the descriptor read to the caller's tenant. So
/// every bundle owned by any other tenant read as a 404 to every runner pod in
/// the deployment: a second tenant's runs could not fetch their tests at all,
/// and `fetch_bundle.py` even printed the symptom ("its `tenant_id` claim names a
/// tenant that does not own this bundle") without anyone drawing the
/// conclusion.
///
/// The tag carries the bundle's own tenant — recovered from the descriptor row,
/// never asserted by the caller — so there is no deployment-wide claim left to
/// be wrong. **This test could not have passed under the old design whatever
/// the code did**, which is what makes it the proof rather than a restatement.
#[tokio::test]
async fn a_second_tenant_can_fetch_its_own_bundle() {
    let seed_tenant = Uuid::new_v4();
    let other_tenant = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let svc = build_service(
        Arc::new(MockBundlesRepository::default()),
        repos,
        Arc::new(InMemoryBundleStore::default()),
        tmp.path().to_path_buf(),
    )
    .await;

    let first = seed_signed_bundle(&svc, seed_tenant, repo_id).await;
    let second = seed_signed_bundle(&svc, other_tenant, repo_id).await;
    assert_ne!(first.id, second.id);
    assert_ne!(
        first.download_sig, second.download_sig,
        "two tenants' tags over two bundles must differ -- an equal pair would \
         mean the key is not tenant-derived and the payload not bundle-bound"
    );

    for (label, bundle) in [("the seed tenant", &first), ("a second tenant", &second)] {
        let bytes = svc
            .get_bundle_content_signed(bundle.id, &bundle.download_sig)
            .await
            .unwrap_or_else(|err| panic!("{label} must be able to fetch its own bundle: {err:?}"));
        assert_eq!(sha256_hex(&bytes), bundle.checksum_sha256, "{label}");
    }
}

/// **One tenant's tag does not open another tenant's bundle.**
///
/// The narrowness claim, stated as an assertion. The key is HKDF-derived per
/// tenant and the payload binds the bundle id, so a tag is a credential for one
/// row: neither presenting tenant A's tag against tenant B's bundle, nor
/// presenting a tag for one of A's own bundles against another of A's bundles,
/// may work.
#[tokio::test]
async fn a_tag_opens_exactly_one_bundle() {
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let svc = build_service(
        Arc::new(MockBundlesRepository::default()),
        repos,
        Arc::new(InMemoryBundleStore::default()),
        tmp.path().to_path_buf(),
    )
    .await;

    let a_first = seed_signed_bundle(&svc, tenant_a, repo_id).await;
    let a_second = seed_signed_bundle(&svc, tenant_a, repo_id).await;
    let b_bundle = seed_signed_bundle(&svc, tenant_b, repo_id).await;

    for (label, bundle_id, signature) in [
        (
            "tenant A's tag against tenant B's bundle",
            b_bundle.id,
            &a_first.download_sig,
        ),
        (
            "tenant B's tag against tenant A's bundle",
            a_first.id,
            &b_bundle.download_sig,
        ),
        (
            "one of tenant A's own tags against another of its own bundles",
            a_second.id,
            &a_first.download_sig,
        ),
    ] {
        let err = svc
            .get_bundle_content_signed(bundle_id, signature)
            .await
            .expect_err(&format!("{label} must be refused"));
        assert!(
            matches!(err, DomainError::Forbidden),
            "{label}: got {err:?}"
        );
    }
}

/// **An expired bundle 404s even under a perfectly valid tag, and a tag carries
/// no expiry of its own.**
///
/// The lifetime decision, pinned. The tag is valid exactly as long as the row it
/// names, because `get_bundle_content` refuses an expired descriptor before
/// reading a byte and the GC deletes the blob. That is one clock, not two —
/// there is no `exp` inside the tag to skew against the row, and no way for a
/// tag to outlive its bundle or a bundle to outlive its tag.
///
/// 404, not 403: an expired bundle reads exactly like a missing one, which is
/// the pre-existing contract of this route and the message `fetch_bundle.py`
/// already diagnoses.
#[tokio::test]
async fn an_expired_bundle_is_a_404_under_a_valid_signature() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::default());
    let svc = build_service(
        Arc::clone(&bundles),
        repos,
        Arc::new(InMemoryBundleStore::default()),
        tmp.path().to_path_buf(),
    )
    .await;

    let bundle = seed_signed_bundle(&svc, tenant_id, repo_id).await;

    // Age the row past its expiry, leaving the tag untouched: the tag is a
    // function of (id, tenant_id) only, so it stays valid and the ROW is what
    // refuses.
    {
        let mut rows = bundles.rows.lock().unwrap();
        let entry = rows.get_mut(&bundle.id).expect("the row must exist");
        entry.1.expires_at = OffsetDateTime::now_utc() - time::Duration::seconds(1);
    }

    let err = svc
        .get_bundle_content_signed(bundle.id, &bundle.download_sig)
        .await
        .expect_err("an expired bundle must not be served");
    assert!(
        matches!(err, DomainError::NotFound { id } if id == bundle.id),
        "an expired bundle reads exactly like a missing one, whatever tag is \
         presented: got {err:?}"
    );
}

/// **An unknown bundle id is a 404, and the signature is never consulted.**
///
/// Deliberate ordering, and not a leak: an id naming no row cannot be told from
/// an expired-and-purged one, `get_bundle_content` already answers 404 for
/// both, and bundle ids are v4 UUIDs. Refusing an unknown id with 403 instead
/// would make this route the "is this id real" oracle that merging the
/// signature refusals exists to avoid.
#[tokio::test]
async fn an_unknown_bundle_id_is_a_404() {
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let svc = build_service(
        Arc::new(MockBundlesRepository::default()),
        repos,
        Arc::new(InMemoryBundleStore::default()),
        tmp.path().to_path_buf(),
    )
    .await;

    let missing = Uuid::new_v4();
    let err = svc
        .get_bundle_content_signed(missing, "00")
        .await
        .expect_err("an unknown bundle must not be served");
    assert!(
        matches!(err, DomainError::NotFound { id } if id == missing),
        "got {err:?}"
    );
}

/// **An absent or too-short signing secret refuses every download, including a
/// correctly computed one.**
///
/// Fail-closed, and the direction matters: an empty HMAC key is a publicly
/// known key, not "no protection". A deployment in this state runs no tests at
/// all, which is loud — and `gear::init` warns once at boot through the same
/// `signing_secret_is_configured` predicate this refusal uses, so the two
/// cannot drift the way qa-insights' equivalent pair once did.
///
/// The tag presented here is the one the SAME service minted, so this is not
/// "a wrong tag is refused" restated: under a non-fail-closed implementation it
/// would verify.
#[tokio::test]
async fn an_unconfigured_or_short_secret_refuses_every_download() {
    for (label, secret) in [
        ("an empty secret", ""),
        ("a whitespace-only secret", "                   "),
        ("a 15-character secret", "123456789012345"),
    ] {
        let tenant_id = Uuid::new_v4();
        let repo_id = Uuid::new_v4();
        let (tmp, repos) = synced_fixture(repo_id);
        let svc = build_service_with_secret(
            Arc::new(MockBundlesRepository::default()),
            repos,
            Arc::new(InMemoryBundleStore::default()),
            tmp.path().to_path_buf(),
            secret,
        )
        .await;

        let bundle = seed_signed_bundle(&svc, tenant_id, repo_id).await;
        let err = svc
            .get_bundle_content_signed(bundle.id, &bundle.download_sig)
            .await
            .expect_err(&format!("{label} must refuse even its own tag"));
        assert!(
            matches!(err, DomainError::Forbidden),
            "{label}: got {err:?}"
        );
    }
}

/// **The tag is not stored, and a descriptor read back carries none.**
///
/// `download_sig` is a transport field on the SDK model, not a column: there is
/// no migration, no row to rotate and nothing to revoke. A repository read that
/// returned a tag would mean somebody added a column, and a stored tag is a
/// credential at rest that the "nothing persists it" argument in this design
/// depends on not existing.
#[tokio::test]
async fn the_tag_is_never_persisted() {
    let tenant_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let (tmp, repos) = synced_fixture(repo_id);
    let bundles = Arc::new(MockBundlesRepository::default());
    let svc = build_service(
        Arc::clone(&bundles),
        repos,
        Arc::new(InMemoryBundleStore::default()),
        tmp.path().to_path_buf(),
    )
    .await;

    let bundle = seed_signed_bundle(&svc, tenant_id, repo_id).await;
    assert!(!bundle.download_sig.is_empty());

    let stored = bundles.rows.lock().unwrap()[&bundle.id].1.clone();
    assert!(
        stored.download_sig.is_empty(),
        "the descriptor written to the repository must carry no tag: {}",
        stored.download_sig
    );
}

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
            download_sig: String::new(),
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
                download_sig: String::new(),
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
        matches!(err, DomainError::Database { .. }),
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

/// [`AuthZResolverApi`] double that records every request it is asked to
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
impl AuthZResolverApi for RecordingAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
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
    authz: Arc<dyn AuthZResolverApi>,
) -> BundlesService<MockBundlesRepository, MockTestReposRepository> {
    let enforcer = PolicyEnforcer::new(authz);
    let db = test_db_provider().await;
    BundlesService::new(
        db,
        bundles,
        repos,
        store,
        repos_dir,
        TEST_TTL,
        BundleDownloadSigningSecret(TEST_SIGNING_SECRET.to_owned()),
        Arc::new(NoopMetrics),
        enforcer,
    )
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
                download_sig: String::new(),
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
                download_sig: String::new(),
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
            download_sig: String::new(),
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
                download_sig: String::new(),
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
