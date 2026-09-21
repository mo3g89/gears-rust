//! Ephemeral test-bundle service: build tar.gz bundles from synced working
//! copies, serve their bytes, and garbage-collect expired ones.
//!
//! # p1 resource limits (deliberate, see the plan's deferred-scope notes)
//!
//! * **Bundles are built and served fully in memory, with no size cap.**
//!   [`create_bundle`](BundlesService::create_bundle) tars+gzips into a
//!   `Vec<u8>` and [`get_bundle_content`](BundlesService::get_bundle_content)
//!   reads the whole blob back, so peak memory scales with the requested
//!   content — and since 2026-08-27 **every** bundle is the whole content root
//!   (see [`build_bundle`]), so a very large repository allocates
//!   proportionally on every run. Acceptable in p1 because bundle creation is not
//!   REST-exposed: only qa-runs calls it, once per run launch, over
//!   repositories an operator registered. Streaming plus a configurable cap is
//!   the follow-up when the `BundleStore` port grows a streaming variant.
//! * **No paging on the catalog's list endpoints** (bundles included):
//!   collections are small in p1, and `OData` paging lands at the qa-runs /
//!   analytics surfaces where it actually matters.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::{hkdf, hmac};
use authz_resolver_sdk::PolicyEnforcer;
use flate2::Compression;
use flate2::write::GzEncoder;
use qa_catalog_sdk::{BundleRequest, TestBundle};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use super::plans::{content_root_dir, require_synced, resolve_under_root, validate_rel_path};
use super::{DbProvider, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::ports::bundle_store::BundleStore;
use crate::domain::ports::metrics::{BundleDownloadMetrics, BundleDownloadOutcome};
use crate::domain::repos::{BundlesRepository, TestReposRepository};

/// Ephemeral bundle service.
#[domain_model]
pub struct BundlesService<B: BundlesRepository, R: TestReposRepository> {
    db: Arc<DbProvider>,
    repo: Arc<B>,
    repos_repo: Arc<R>,
    store: Arc<dyn BundleStore>,
    repos_dir: PathBuf,
    /// `QaCatalogConfig::bundle_ttl_seconds` — every created bundle expires
    /// this long after creation.
    bundle_ttl: time::Duration,
    /// `QaCatalogConfig::bundle_download_signing_secret` — the HMAC root every
    /// per-bundle download tag is derived from. See
    /// [`BundleDownloadSigningSecret`].
    download_signing_secret: BundleDownloadSigningSecret,
    /// The anonymous download route's access-control telemetry. See
    /// [`BundleDownloadMetrics`] — emission cannot fail and must not change
    /// this path's behaviour.
    metrics: Arc<dyn BundleDownloadMetrics>,
    policy_enforcer: PolicyEnforcer,
}

impl<B: BundlesRepository, R: TestReposRepository> BundlesService<B, R> {
    /// Nine constructor arguments, not a `Deps` struct: the one caller is
    /// `AppServices::new`, which already takes a `ServiceDeps` and unpacks it
    /// here, so a second struct would be one wrapper unpacked into another at
    /// the same call site. `super::repos::ReposService::new` carries the same
    /// allowance for the same reason.
    #[allow(clippy::too_many_arguments, reason = "see the doc above")]
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<B>,
        repos_repo: Arc<R>,
        store: Arc<dyn BundleStore>,
        repos_dir: PathBuf,
        bundle_ttl: time::Duration,
        download_signing_secret: BundleDownloadSigningSecret,
        metrics: Arc<dyn BundleDownloadMetrics>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            repo,
            repos_repo,
            store,
            repos_dir,
            bundle_ttl,
            download_signing_secret,
            metrics,
            policy_enforcer,
        }
    }
}

// Business logic methods
//
// `B: 'static`: the `purge_expired` transaction closure's future captures
// `Arc<B>`, and `DBProvider::transaction` requires its captures to be
// `'static`.
impl<B: BundlesRepository + 'static, R: TestReposRepository> BundlesService<B, R> {
    /// Build a bundle for `req`: the whole content root of the synced working
    /// copy, tar.gz'd in memory, stored in the bundle store, with the descriptor
    /// persisted. `req.files` names what the run will *execute* and is validated
    /// against the branch — see [`build_bundle`] for why it is no longer what the
    /// archive contains.
    #[instrument(skip(self, ctx, req), fields(repo_id = %req.repo_id, branch = %req.branch, files = req.files.len()))]
    pub async fn create_bundle(
        &self,
        ctx: &SecurityContext,
        req: BundleRequest,
    ) -> Result<TestBundle, DomainError> {
        info!("Creating test bundle");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::BUNDLE, actions::CREATE, None)
            .await?;

        // Validate every client-supplied path before touching the filesystem.
        for file in &req.files {
            validate_rel_path("files", file)?;
        }

        let (root, content_root) = self.synced_content_root(ctx, &req).await?;

        let files = req.files.clone();
        let bytes = tokio::task::spawn_blocking(move || build_bundle(&root, &content_root, &files))
            .await
            .map_err(|e| DomainError::Internal(e.to_string()))??;

        let checksum = sha256_hex(&bytes);
        let size_bytes = u64::try_from(bytes.len())
            .map_err(|_| DomainError::Internal("bundle size overflows u64".to_owned()))?;

        // The ID is allocated before `put` so the blob and the descriptor
        // row share one identity (see `BundlesRepository::create`).
        let id = Uuid::new_v4();
        let storage_ref = self.store.put(id, bytes).await?;

        let now = OffsetDateTime::now_utc();
        let descriptor = TestBundle {
            id,
            storage_ref: storage_ref.clone(),
            checksum_sha256: checksum,
            size_bytes,
            expires_at: now + self.bundle_ttl,
            created_at: now,
            // Empty on the way IN to the repository -- there is no column for
            // it. The tag is attached to the value that comes back out, below,
            // once the row (and therefore the identity it authorises) exists.
            download_sig: String::new(),
        };

        let conn = self.db.conn()?;
        let tenant_id = ctx.subject_tenant_id();

        match self.repo.create(&conn, &scope, tenant_id, descriptor).await {
            Ok(mut created) => {
                // The tag is minted HERE, and only here: `id`, `tenant_id` and
                // the signing secret are all in hand at this one point, and
                // nothing downstream of this method can recompute it without
                // the secret. It is attached to the returned value and never
                // written to the row -- see `TestBundle::download_sig`.
                created.download_sig = self.sign_download(created.id, tenant_id);
                // The size is logged because it is now a function of the whole
                // content root rather than of a file list, and this is the only
                // place a deployment can see it grow.
                info!("Successfully created bundle with id={id} size_bytes={size_bytes}");
                Ok(created)
            }
            Err(db_err) => {
                // Don't strand the blob when the descriptor write fails.
                if let Err(cleanup_err) = self.store.delete(&storage_ref).await {
                    warn!(bundle_id = %id, error = %cleanup_err, "Failed to clean up bundle blob after DB error");
                }
                Err(db_err)
            }
        }
    }

    /// Fetch the bundle bytes. An expired bundle reads exactly like a
    /// missing one — it must never be served after `expires_at`.
    #[instrument(skip(self, ctx), fields(bundle_id = %id))]
    pub async fn get_bundle_content(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Vec<u8>, DomainError> {
        debug!("Fetching bundle content");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::BUNDLE, actions::GET, Some(id))
            .await?;

        let conn = self.db.conn()?;
        let descriptor = self
            .repo
            .get(&conn, &scope, id)
            .await?
            .ok_or(DomainError::NotFound { id })?;

        if descriptor.expires_at <= OffsetDateTime::now_utc() {
            return Err(DomainError::NotFound { id });
        }

        self.store.get(&descriptor.storage_ref).await
    }

    /// Serve one bundle's bytes to a caller that presented a **signature**
    /// rather than a session — the whole of `GET
    /// /qa/v1/test-bundles/{id}?sig=...`.
    ///
    /// # Why a workflow pod has no bearer token, and what replaced it
    ///
    /// The runner pod's job is to execute tenant-authored pytest. It used to
    /// carry a confidential OIDC client secret so it could mint a token and
    /// call this route `.authenticated()`; that credential was
    /// deployment-wide, unexpiring, `fullScopeAllowed`, hardcoded to one
    /// tenant, and sat in an environment variable of a process tree running
    /// tenant-written code. It is gone. What the pod carries now is a tag over
    /// `(bundle_id, tenant_id)` that authorises **exactly this one bundle**
    /// and nothing else — a `tar.gz` the pod is about to unpack anyway, whose
    /// contents belong to the tenant that owns it. That is the property that
    /// makes a leaked tag uninteresting, and therefore the property that makes
    /// a revocation list unnecessary.
    ///
    /// # The order of the four steps is the security argument
    ///
    /// 1. **Recover the tenant from the row.** [`BundlesRepository::tenant_of`]
    ///    under the elevated, read-only enumeration scope. The tenant is never
    ///    a second query parameter: qa-insights' collect route must accept one
    ///    because its row may not exist yet, but a bundle descriptor always
    ///    exists before a tag over it can be presented, so the weaker shape
    ///    would buy nothing.
    /// 2. **Verify.** [`Self::verify_download_signature`], constant-time,
    ///    fail-closed. All three refusal paths answer one
    ///    [`DomainError::Forbidden`]; the classification goes to the metric
    ///    only, so the response is not an oracle for which guess was closer.
    /// 3. **Mint the context**: `system_actor::for_bundle_download` bound to
    ///    the tenant *the row named*, not to anything the caller said.
    /// 4. **Delegate, unchanged**, to [`Self::get_bundle_content`] — which
    ///    still asks the PDP, still scopes the descriptor read to that tenant,
    ///    and still refuses an expired bundle as a 404. Step 4 is what keeps
    ///    the tag unable to reach past the bundle it names even if step 2 were
    ///    wrong.
    ///
    /// # A missing bundle is a 404 before a signature is ever checked
    ///
    /// Deliberate, and not a leak: an id that names no row cannot be
    /// distinguished from an expired one (the GC deletes both descriptor and
    /// blob), `get_bundle_content` already answers 404 for both, and bundle
    /// ids are v4 UUIDs — enumerating them is not an attack this ordering
    /// enables. The alternative, refusing an unknown id with 403, would make
    /// *this* route the oracle for "is this id real" that the merged refusal
    /// above exists to avoid.
    ///
    /// # No expiry of its own
    ///
    /// The tag carries none. It is valid exactly as long as the row it names,
    /// because step 4 already refuses an expired bundle before reading a byte
    /// and the GC already deletes the blob. One number to tune
    /// (`bundle_ttl_seconds`), one expiry semantics, one error message —
    /// instead of a tag that can expire while its bundle is live, or outlive a
    /// bundle that has gone.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] for every signature refusal, merged;
    /// [`DomainError::NotFound`] for an unknown, expired or purged bundle.
    #[instrument(skip(self, signature), fields(bundle_id = %id))]
    pub async fn get_bundle_content_signed(
        &self,
        id: Uuid,
        signature: &str,
    ) -> Result<Vec<u8>, DomainError> {
        debug!("Fetching bundle content under a download signature");

        let conn = self.db.conn()?;
        // Elevated, cross-tenant, and one column wide. See
        // `BundlesRepository::tenant_of` for why the download path cannot use
        // the scoped `get` for this step and why it may not read more.
        let scope = crate::domain::elevated::enumeration_scope();
        let Some(tenant_id) = self.repo.tenant_of(&conn, &scope, id).await? else {
            return Err(DomainError::NotFound { id });
        };

        if let Err(refusal) = self.verify_download_signature(id, tenant_id, signature) {
            // The operator's copy of the distinction, and the only one: a
            // deployment with no secret configured and a deployment under a
            // guessing attack are opposite incidents with opposite fixes.
            self.metrics.bundle_download(refusal.into());
            warn!(
                refusal = refusal.as_str(),
                "refusing a bundle download: the sig query parameter did not verify"
            );
            return Err(DomainError::Forbidden);
        }

        let ctx = crate::domain::system_actor::for_bundle_download(tenant_id);
        let bytes = self.get_bundle_content(&ctx, id).await?;
        self.metrics
            .bundle_download(BundleDownloadOutcome::Served);
        Ok(bytes)
    }

    /// The hex-encoded HMAC-SHA256 tag over `(bundle_id, tenant_id)` —
    /// `TEST_BUNDLE_URL`'s `sig` query parameter, and the value
    /// [`Self::create_bundle`] returns on `TestBundle::download_sig`.
    ///
    /// `aws_lc_rs::hmac` rather than `sha2`/`hmac` directly: Dylint `DE0708`
    /// bans a new non-allow-listed `sha2` import, and `aws-lc-rs` is this
    /// workspace's mandated FIPS-validated primitive — the same call this
    /// module already makes for `sha256_hex`, and the same one qa-insights'
    /// `CollectService::sign` makes.
    ///
    /// **This signs unconditionally, including under an unconfigured secret.**
    /// Verification is where fail-closed lives
    /// ([`Self::verify_download_signature`]), for the same reason qa-insights
    /// puts it there: a signer that refused would fail a *bundle build*, which
    /// is a launch-path failure with a misleading message, where the refusal
    /// belongs on the download with a diagnostic that names the secret.
    fn sign_download(&self, bundle_id: Uuid, tenant_id: Uuid) -> String {
        let key = derive_signing_key(&self.download_signing_secret.0, tenant_id);
        let tag = hmac::sign(&key, &signing_payload(bundle_id, tenant_id));
        hex::encode(tag.as_ref())
    }

    /// Verify [`Self::sign_download`]'s tag.
    ///
    /// Fails closed (never verifies) when `bundle_download_signing_secret` is
    /// empty or shorter than [`MIN_SIGNING_SECRET_LEN`] once trimmed, and on
    /// any signature that does not decode as hex or does not match.
    /// `aws_lc_rs::hmac::verify` is constant-time, so neither matching arm
    /// leaks which byte first differed.
    ///
    /// # What the tag covers, stated so the implementation cannot drift
    ///
    /// Exactly `(bundle_id, tenant_id)`, and therefore exactly one row. Not a
    /// repository's bundles, not a tenant's, not a run's: a run has N nodes and
    /// N bundles, so a run-scoped tag would have to be presented against a
    /// bundle id it does not name — which means a stored row or a second
    /// identifier in the tag, and per-bundle is strictly narrower and needs
    /// neither.
    ///
    /// It authorises **no write of any kind, no other bundle, no other route,
    /// no other gear.** It is not a `SecurityContext` and must never become
    /// convertible into one beyond
    /// `system_actor::for_bundle_download(tenant_id)`.
    ///
    /// # Errors
    ///
    /// [`SignatureRefusal`], naming which of the three paths refused. **The
    /// caller cannot distinguish them**: [`Self::get_bundle_content_signed`]
    /// answers all three with one [`DomainError::Forbidden`] and carries the
    /// classification on the metric alone.
    fn verify_download_signature(
        &self,
        bundle_id: Uuid,
        tenant_id: Uuid,
        signature: &str,
    ) -> Result<(), SignatureRefusal> {
        if !signing_secret_is_configured(&self.download_signing_secret.0) {
            return Err(SignatureRefusal::SecretUnconfigured);
        }
        let Ok(tag) = hex::decode(signature) else {
            return Err(SignatureRefusal::Malformed);
        };
        let key = derive_signing_key(&self.download_signing_secret.0, tenant_id);
        hmac::verify(&key, &signing_payload(bundle_id, tenant_id), &tag)
            .map_err(|_| SignatureRefusal::Mismatch)
    }

    /// Every tenant with at least one bundle descriptor expired at or before
    /// now, ascending, **with the nil tenant filtered out**.
    ///
    /// The enumeration half of the GC pass: [`Self::purge_expired`] runs once
    /// per tenant this answers with, each in its own transaction under a
    /// tenant-bound context minted from that tenant's own id
    /// (`domain::system_actor::for_bundle_delete`) — see `crate::gear`'s
    /// bundle GC task, which drives the loop.
    ///
    /// Elevated, not PEP-compiled: `_enumeration` is accepted only so the
    /// caller's audit-logging `system_actor::for_bundle_gc` construction
    /// still reads as feeding this read — the context it builds is never
    /// passed to `access_scope`. See `domain::elevated` for why this
    /// nil-tenant enumeration bypasses the PEP.
    ///
    /// **The nil-tenant filter.** `for_bundle_delete(Uuid::nil())` builds a
    /// context indistinguishable from [`crate::domain::system_actor::for_bundle_gc`]'s
    /// own nil-tenant, platform-scoped one — `SecurityContext` has no third
    /// state between "this tenant" and "no tenant", so a nil `tenant_id`
    /// here would silently become the platform-root sentinel rather than a
    /// row's owning tenant. Passed to the PEP it is denied (the plugin
    /// refuses a nil tenant outright), so a `qa_bundles` row that ever
    /// carries `tenant_id = 00000000-…` fails closed rather than leaking —
    /// but ascending order sorts it first, so it would also be retried,
    /// denied, and reported first on **every** GC pass forever, without ever
    /// being the fault the retry implies. qa-runs refuses this shape at
    /// construction (`TenantBound`) and qa-insights filters it at this same
    /// enumeration boundary (`TenantDirectory::known_tenants`); this mirrors
    /// the latter, since qa-catalog's factories take a plain `Uuid` rather
    /// than a checked newtype.
    #[instrument(skip(self, _enumeration))]
    pub async fn tenants_with_expired_bundles(
        &self,
        // Kept, unused, so the caller's audit-logging
        // `system_actor::for_bundle_gc` construction still reads as feeding
        // this read.
        _enumeration: &SecurityContext,
    ) -> Result<Vec<Uuid>, DomainError> {
        debug!("Enumerating tenants with expired bundles");

        // Nil-tenant enumeration: elevated here rather than authorized. See
        // `domain::elevated` for why, and for why the per-tenant delete that
        // follows (`Self::purge_expired`) is still tenant-bound.
        let scope = crate::domain::elevated::enumeration_scope();

        let conn = self.db.conn()?;
        let now = OffsetDateTime::now_utc();
        let tenants = self
            .repo
            .tenants_with_expired_bundles(&conn, &scope, now)
            .await?;

        let total = tenants.len();
        let tenants: Vec<Uuid> = tenants.into_iter().filter(|id| !id.is_nil()).collect();
        let nil_count = total - tenants.len();
        if nil_count > 0 {
            warn!(
                nil_count,
                "qa-catalog: bundle GC enumeration found expired bundles owned by the nil \
                 tenant; skipping them rather than minting a context indistinguishable from \
                 the platform-root sentinel. This indicates a data-integrity issue upstream \
                 of GC, not an authorization gap"
            );
        }
        Ok(tenants)
    }

    /// Delete one tenant's expired bundles: descriptor rows first
    /// (select+delete atomically inside one transaction), then the blobs. A
    /// blob whose delete fails is merely orphaned storage (logged, retried by
    /// later GC sweeps only if the store implementation makes that possible)
    /// — the reverse order could leave descriptors pointing at deleted blobs.
    ///
    /// `ctx` must be tenant-bound — a nil-tenant context here would ask the
    /// PEP for a platform-wide `qa.bundle`/`DELETE` scope, and the whole
    /// point of splitting this from [`Self::tenants_with_expired_bundles`] is
    /// that the delete never runs under one. `crate::gear`'s bundle GC task
    /// calls this once per tenant [`Self::tenants_with_expired_bundles`]
    /// returns, under `domain::system_actor::for_bundle_delete(tenant_id)`.
    ///
    /// Returns the number of purged descriptors.
    #[instrument(skip(self, ctx))]
    pub async fn purge_expired(&self, ctx: &SecurityContext) -> Result<usize, DomainError> {
        debug!("Purging expired bundles");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::BUNDLE, actions::DELETE, None)
            .await?;

        let now = OffsetDateTime::now_utc();
        let repo = Arc::clone(&self.repo);
        let expired = self
            .db
            .transaction(move |tx| {
                Box::pin(async move { repo.delete_expired(tx, &scope, now).await })
            })
            .await?;

        for bundle in &expired {
            if let Err(err) = self.store.delete(&bundle.storage_ref).await {
                warn!(bundle_id = %bundle.id, error = %err, "Failed to delete expired bundle blob");
            }
        }

        if !expired.is_empty() {
            info!("Purged {} expired bundles", expired.len());
        }
        Ok(expired.len())
    }

    /// Resolve the repository (tenancy precheck under its own `TEST_REPO/GET`
    /// scope), require it synced for the requested branch, and return the
    /// canonicalized content root together with the repository-relative path it
    /// was reached by.
    ///
    /// The second element is what makes the archive self-describing: it is the
    /// prefix every entry is written under, so the extracted tree keeps the
    /// depth the repository has. See [`build_bundle`] for why that matters.
    async fn synced_content_root(
        &self,
        ctx: &SecurityContext,
        req: &BundleRequest,
    ) -> Result<(PathBuf, String), DomainError> {
        let repo_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::GET, Some(req.repo_id))
            .await?;

        let conn = self.db.conn()?;
        let repo = self
            .repos_repo
            .get(&conn, &repo_scope, req.repo_id)
            .await?
            .ok_or(DomainError::NotFound { id: req.repo_id })?;

        require_synced(&repo, &req.branch)?;
        let root = content_root_dir(&self.repos_dir, &repo, &req.branch)?;
        Ok((root, repo.content_root))
    }
}

/// `QaCatalogConfig::bundle_download_signing_secret` — the HMAC root
/// [`BundlesService::sign_download`] and
/// [`BundlesService::verify_download_signature`] share.
///
/// A type rather than a bare `String` so it cannot be transposed with any
/// other string argument at the one construction site, and so `Debug` can be
/// written by hand: an accidental `{:?}` anywhere in this gear's own code — a
/// panic message, a stray `tracing::debug!` — cannot print the secret. That is
/// narrower than, and no substitute for, whatever a config dump does with the
/// `QaCatalogConfig` field itself, which never passes through this impl.
#[derive(Clone)]
pub struct BundleDownloadSigningSecret(pub String);

impl std::fmt::Debug for BundleDownloadSigningSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BundleDownloadSigningSecret")
            .field(&"<redacted>")
            .finish()
    }
}

/// The minimum accepted length (bytes, trimmed) of
/// `QaCatalogConfig::bundle_download_signing_secret` before
/// [`BundlesService::verify_download_signature`] treats it as configured at
/// all.
///
/// 16 bytes (128 bits) is not a proof of entropy — a 16-byte run of one
/// repeated character is exactly as weak as it looks — only a floor on
/// *length*, which is the one property this code can check; generating the
/// value randomly stays the deployment's job. The same floor, for the same
/// reason, as qa-insights' `collect::MIN_SIGNING_SECRET_LEN`: this is a
/// per-deployment operational secret an operator types into configuration, so
/// the shorter of the two common floors (128 bits) is the right one rather
/// than the hash's own 64-byte block size.
pub const MIN_SIGNING_SECRET_LEN: usize = 16;

/// Whether `secret` clears [`MIN_SIGNING_SECRET_LEN`] once trimmed.
///
/// **One predicate, two call sites, deliberately shared**:
/// [`BundlesService::verify_download_signature`] refuses on it at request
/// time, and `crate::gear`'s `init` warns on it at boot. qa-insights shipped
/// this same pair as two independent `is_empty()` checks, then raised one of
/// them and not the other, and the result was a real gap — a 1–15 character
/// secret passed the boot check silently and failed every request. There is
/// one definition here so the two cannot drift.
pub fn signing_secret_is_configured(secret: &str) -> bool {
    secret.trim().len() >= MIN_SIGNING_SECRET_LEN
}

/// The domain-separation label for [`derive_signing_key`]'s HKDF salt.
///
/// Public/non-secret by construction — RFC 5869 §3.1 never requires a secret
/// salt — and fixed rather than random because there is no per-deployment
/// random value available here that the signing secret does not already
/// provide; a fixed, versioned label is the standard fallback when none
/// exists. The secret supplies the extract step's entropy; this constant's
/// only job is binding the derivation to *this* call site, so that a future
/// second HKDF use in this crate over the same root secret cannot collide with
/// this one's output. The trailing `v1` is deliberate: changing this constant
/// changes every derived key at once, which invalidates every tag in flight.
const BUNDLE_DOWNLOAD_HKDF_SALT: &[u8] = b"qa-catalog/bundle-download/v1";

/// HKDF-derive tenant `tenant_id`'s own HMAC-SHA256 key from the one root
/// secret a deployment configures.
///
/// `PRK = HKDF-Extract(salt = `[`BUNDLE_DOWNLOAD_HKDF_SALT`]`, ikm =
/// root_secret)`, then `OKM = HKDF-Expand(PRK, info = tenant_id's 16 raw
/// bytes, len = HMAC-SHA256's key length)`, both HMAC-SHA256-based, so this
/// adds a key-derivation step and no new hash algorithm.
///
/// # What it buys, stated precisely
///
/// Whoever holds the root secret can still derive any tenant's key on demand —
/// unavoidable for any scheme that derives every tenant's key from one
/// configured value without an out-of-band per-tenant secret store, which this
/// design does not ask for. What the derivation buys is that a *derived* key,
/// leaked alone, verifies for the one tenant it was derived for and not for
/// every tenant at once — and, more concretely here, that the tag on a given
/// bundle's URL is bound to that bundle's owning tenant without the caller
/// having to assert a tenant at all.
///
/// `tenant_id.as_bytes()` is `Uuid`'s fixed 16-byte representation, so unlike
/// a delimited string it needs no delimiter-safety argument: `expand`'s own
/// doc warns about concatenated *variable-length* `info` fields colliding,
/// which cannot apply to a single fixed-length one.
///
/// # Why `aws_lc_rs::hkdf`, not `hkdf`/`ring`
///
/// Dylint `DE0708` bans a new non-allow-listed `sha2` import, and `aws-lc-rs`
/// is this workspace's mandated FIPS-validated primitive. Reaching for `hkdf`
/// or `ring` would reintroduce a non-FIPS-validated implementation and add a
/// dependency this workspace has already declined for this exact problem.
///
/// # Fail-closed is unaffected
///
/// This function never decides whether a download is served:
/// [`BundlesService::verify_download_signature`] checks
/// [`signing_secret_is_configured`] and returns
/// [`SignatureRefusal::SecretUnconfigured`] before this function is reached.
///
/// # Panics
///
/// Never for this call site — see the allowance.
#[allow(
    clippy::expect_used,
    reason = "the requested OKM length is HMAC-SHA256's own fixed digest length (32 bytes), \
              never more than the 255x-digest-length cap `Prk::expand` enforces, so this can \
              never fail here; a panic that cannot trigger beats a silent fallback key"
)]
fn derive_signing_key(root_secret: &str, tenant_id: Uuid) -> hmac::Key {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, BUNDLE_DOWNLOAD_HKDF_SALT);
    let prk = salt.extract(root_secret.as_bytes());
    // A named binding, not an inline literal: `Okm` borrows this slice for its
    // own lifetime, and an unnamed temporary would be dropped at the end of the
    // `let okm = ...` statement, before `hmac::Key::from(okm)` uses it (E0716).
    let info: [&[u8]; 1] = [tenant_id.as_bytes().as_slice()];
    let okm = prk
        .expand(&info, hkdf::HKDF_SHA256.hmac_algorithm())
        .expect(
            "HKDF-Expand's requested length is HMAC-SHA256's own fixed digest \
             length (32 bytes), never more than the 255x-digest-length cap \
             `expand` enforces, so this can never fail for this call site",
        );
    hmac::Key::from(okm)
}

/// The 32 bytes [`BundlesService::sign_download`] and
/// [`BundlesService::verify_download_signature`] both MAC: the bundle id's 16
/// raw bytes followed by the tenant id's 16.
///
/// **Both operands are fixed-width, so there is no delimiter to argue about.**
/// qa-insights' `signing_payload` joins variable-length fields with `|` and
/// has to carry a paragraph explaining why one `branch` value cannot stand in
/// for a different triple. Two UUIDs concatenated at known offsets admit no
/// such ambiguity: the split point is a constant, not a scan for a separator.
///
/// `tenant_id` is inside the payload even though [`derive_signing_key`]
/// already binds the key to it — defense in depth that costs nothing. A
/// hypothetical derivation regression that produced the same key for two
/// tenants would still produce different tags for them, because the MAC input
/// would differ under the shared key.
fn signing_payload(bundle_id: Uuid, tenant_id: Uuid) -> [u8; 32] {
    let mut payload = [0_u8; 32];
    payload[..16].copy_from_slice(bundle_id.as_bytes());
    payload[16..].copy_from_slice(tenant_id.as_bytes());
    payload
}

/// Which of [`BundlesService::verify_download_signature`]'s three refusal
/// paths a download took.
///
/// # This exists because the response deliberately cannot say
///
/// All three become one [`DomainError::Forbidden`] at
/// [`BundlesService::get_bundle_content_signed`], which is that method's whole
/// point: a response that distinguished them would hand an attacker a free
/// oracle for which guess was closer. The distinction is real and an operator
/// needs it — a deployment with no secret configured and a deployment under a
/// guessing attack are opposite incidents with opposite fixes — so it is
/// carried in this type, out of the request path and into the metric, and
/// nowhere else.
///
/// **Effectively crate-private.** `crate::domain` is `pub(crate)` in this gear,
/// so nothing outside the crate can name this type whatever its own visibility
/// says — `pub` here rather than `pub(crate)` only because clippy's
/// `redundant_pub_crate` is denied and the enclosing module already bounds it.
/// Its whole reason for existing is that this information must not leave the
/// process through the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureRefusal {
    /// `bundle_download_signing_secret` is absent, or shorter than
    /// [`MIN_SIGNING_SECRET_LEN`] once trimmed. Fail-closed: this refuses every
    /// download, including a correctly computed one.
    SecretUnconfigured,
    /// The `sig` parameter is not hex, so there is no tag to compare.
    Malformed,
    /// The tag decoded and does not match this deployment's secret over the
    /// `(bundle_id, tenant_id)` pair the descriptor row named.
    Mismatch,
}

impl SignatureRefusal {
    /// The `tracing` field value. Not the metric label — that is
    /// [`BundleDownloadOutcome::as_str`], reached through the `From` impl
    /// below, so the series' label set stays owned by the metrics port.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SecretUnconfigured => "secret_unconfigured",
            Self::Malformed => "signature_malformed",
            Self::Mismatch => "signature_invalid",
        }
    }
}

/// The metric label for one refusal.
///
/// **Here rather than in `domain::ports::metrics`** — beside the enum it
/// projects, following the placement qa-insights settled on for the same
/// shape. [`SignatureRefusal`] is crate-private, so an impl written in the
/// ports module would attach a crate-private type to a public one; and the
/// "a fourth refusal path is a compile error here" guarantee only helps if it
/// is where the author adding one is already looking.
impl From<SignatureRefusal> for BundleDownloadOutcome {
    fn from(refusal: SignatureRefusal) -> Self {
        match refusal {
            SignatureRefusal::SecretUnconfigured => Self::SecretUnconfigured,
            SignatureRefusal::Malformed => Self::SignatureMalformed,
            SignatureRefusal::Mismatch => Self::SignatureInvalid,
        }
    }
}

/// Lower-hex SHA-256 digest of `bytes`. Shared with `super::ssh_keys` (the
/// fingerprint) and the unit tests.
///
/// The hasher is `aws-lc-rs` — the FIPS-validated provider the platform already
/// installs at bootstrap — and not `sha2`, which Dylint's `DE0708` bans outside
/// `dylint.toml`'s `hasher_allowed_paths`. Same algorithm, byte-identical
/// output: this is a provider swap, not a digest change, so every
/// `storage_ref`/checksum already in a database still matches. Precedent:
/// `types-registry`'s `domain::admission::fingerprint`, `bss/ledger`'s
/// `payload_hash`.
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = digest(&SHA256, bytes);
    let mut hex = String::with_capacity(digest.as_ref().len() * 2);
    for &byte in digest.as_ref() {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    hex
}

// ---------------------------------------------------------------------------
// Bundle assembly (blocking; run via `spawn_blocking`)
// ---------------------------------------------------------------------------

/// Build an in-memory tar.gz of the whole content root under the canonicalized
/// `root`, after checking that every path in `files` exists there. Archive entry
/// names are the paths relative to `root`, written under `content_root` so the
/// extracted tree keeps the depth the repository gave it.
///
/// # Why entries carry the `content_root` prefix
///
/// A suite is entitled to locate things relative to its own checkout, and the
/// idiom for that is counting parents: `Path(__file__).resolve().parents[4]`.
/// That is correct in a checkout and was correct in the system this replaced,
/// which cloned the whole repository and merely `cd`-ed into the tests root
/// (`../vhp-testrunner/runner/entrypoint.sh`: `git clone` to `/tmp/test_repo`,
/// then `TESTS_BASE_DIR="${TESTS_BASE_DIR}/${TEST_REPO_ROOT}"`) — the ancestors
/// stayed on disk.
///
/// Rooting the archive AT the content root deleted those ancestors, so the same
/// expression ran off the end of the path. Measured on run
/// `1a9f0eb6-5e2f-4aa5-8eec-d577ed53a88c` against `vhp-core`, whose
/// `content_root` is `tests/e2e`: `tests/e2e/tests/vpctl/conftest.py` arrived as
/// `/work/tests/vpctl/conftest.py`, two levels shallower, and `parents[4]` raised
/// `IndexError`. Because that ran at import time in a `conftest.py`, pytest
/// aborted the whole session with a usage error — **1465 collectable tests, none
/// of them run, and the run recorded zero results**, not merely the vpctl ones
/// that wanted the path.
///
/// Prefixing costs nothing: the archive carries the same files, and the empty
/// ancestor directories tar creates are implicit in the entry names. What it buys
/// is that a repository which runs under the old system runs here unchanged.
///
/// # `files` is validated, not selected on — and this changed 2026-08-27
///
/// It used to be the archive's *contents*: a bundle held the plan's test files
/// and nothing else. **That produces a bundle that cannot run.** Measured on the
/// first real pytest suite to reach this path (run
/// `94978978-fa28-4650-a14d-2ce8f72dff49`, workflow
/// `monitoring-cms-e2e-tests-4`): ten test modules were bundled, unpacked, and
/// then failed collection seven times with `ModuleNotFoundError: No module named
/// 'lib'`, because `conftest.py`, `pytest.ini`, `__init__.py`, `requirements.txt`
/// and the `lib/` package the modules import were all outside the file list and
/// therefore outside the archive. A Python test module is not a self-contained
/// unit of execution; its package is.
///
/// **Why the whole root rather than a computed closure.** The source system
/// computes one (`../vhp-testrunner/manager/src/services/test_bundles.rs:110-201`):
/// a parent-walk for `conftest.py` / `__init__.py` / requirements variants, the
/// test's own package directory, then a scan for six *hardcoded* shared folder
/// names (`lib`, `common`, `utils`, `sdks`, `helpers`, `fixtures`) at two
/// candidate bases each, plus four config filenames at two bases. Read against
/// the repository that broke this, that heuristic finds `tests/lib/` (via the
/// `source_root/tests` base) and misses `tests/pytest.ini` (it only looks for
/// `pytest.ini` at the content root and at the checkout root) — so porting it
/// would have reproduced the `PytestUnknownMarkWarning` flood *and* left every
/// repository whose shared package is not one of those six names broken with no
/// diagnostic. The set of files a suite needs is not derivable from its test
/// paths; the content root is the smallest thing that is certainly sufficient,
/// and it is what the repository owner already chose by setting `content_root`.
///
/// **`files` still has to be checked**, and is: a plan naming a path that is not
/// on the branch stays a dispatch-time `FileNotFound` rather than becoming a run
/// that collects nothing. `TEST_FILES` in the pod carries the selection, so what
/// *executes* is still the plan's list — only what *travels* changed.
///
/// **The cost, stated rather than discovered later.** Bundles are built in memory
/// with no size cap (see this module's header), so a bundle is now proportional
/// to the content root instead of to the selection. For the repository above:
/// 6.6 MB / 457 files, 71 kB gzipped. A repository whose `content_root` is the
/// checkout root pays for the whole checkout, minus [`EXCLUDED_BUNDLE_DIRS`].
fn build_bundle(root: &Path, content_root: &str, files: &[String]) -> Result<Vec<u8>, DomainError> {
    for file in files {
        let resolved = resolve_under_root(root, file)?
            .ok_or_else(|| DomainError::FileNotFound { path: file.clone() })?;
        if !resolved.is_file() {
            return Err(DomainError::FileNotFound { path: file.clone() });
        }
    }
    let entries = collect_all_files(root)?;

    let internal = |e: std::io::Error| DomainError::Internal(format!("bundle build failed: {e}"));

    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (abs_path, rel_path) in entries {
        let name = if content_root.is_empty() {
            rel_path
        } else {
            format!("{content_root}/{rel_path}")
        };
        builder
            .append_path_with_name(&abs_path, &name)
            .map_err(internal)?;
    }
    if !content_root.is_empty() {
        let marker = content_root.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(marker.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, CONTENT_ROOT_MARKER, marker)
            .map_err(internal)?;
    }
    let encoder = builder.into_inner().map_err(internal)?;
    encoder.finish().map_err(internal)
}

/// Name of the archive-root file that records the bundle's `content_root`.
///
/// Written only when `content_root` is non-empty, so a bundle whose content root
/// already is the checkout root is byte-for-byte what it was before this file
/// existed — and, more importantly, so **absence keeps meaning "the archive root
/// is the content root"**. Every bundle already in the store predates the marker
/// and reads correctly under that rule, which is why the runner treats a missing
/// marker as a valid state rather than an error.
pub(super) const CONTENT_ROOT_MARKER: &str = ".qa-content-root";

/// Directory names that never travel in a test bundle: VCS internals, Python
/// bytecode and tool caches, and dependency trees a bundle must not carry.
///
/// Ported verbatim from the source system's `PYTEST_BUNDLE_EXCLUDED_DIRS`
/// (`../vhp-testrunner/manager/src/services/test_bundles.rs:370-378`), which
/// gives the reason: *"so we don't ship stale `.pyc` files or `.git` metadata
/// alongside the tests"*. A stale `__pycache__` is not merely waste — a `.pyc`
/// whose source is no longer in the tree can still be imported and shadow the
/// module the run meant to execute.
///
/// It became load-bearing when [`build_bundle`] stopped selecting on `files`:
/// until then the whole-tree walk was only reachable through an explicitly empty
/// request, and now it is the only path.
const EXCLUDED_BUNDLE_DIRS: &[&str] = &[
    ".git",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".venv",
    "node_modules",
];

/// Recursively collect every regular file under `root` as
/// `(absolute, relative)` pairs. Symlinks are skipped — a link inside the
/// working copy must not pull content from outside it into a bundle — and so are
/// the directories in [`EXCLUDED_BUNDLE_DIRS`].
fn collect_all_files(root: &Path) -> Result<Vec<(PathBuf, String)>, DomainError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| DomainError::Internal(format!("bundle build failed: {e}")))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let excluded = path
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|name| EXCLUDED_BUNDLE_DIRS.contains(&name));
                if !excluded {
                    stack.push(path);
                }
            } else if file_type.is_file() {
                let Ok(rel) = path.strip_prefix(root) else {
                    continue;
                };
                let Some(rel) = rel.to_str() else {
                    warn!(file = %path.display(), "Skipping non-UTF-8 path in bundle");
                    continue;
                };
                out.push((path.clone(), rel.to_owned()));
            }
        }
    }

    // Deterministic archive order (read_dir order is platform-dependent).
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}
