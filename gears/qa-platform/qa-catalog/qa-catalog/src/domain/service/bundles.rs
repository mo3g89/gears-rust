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

use authz_resolver_sdk::PolicyEnforcer;
use flate2::Compression;
use flate2::write::GzEncoder;
use qa_catalog_sdk::{BundleRequest, TestBundle};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use super::plans::{content_root_dir, require_synced, resolve_under_root, validate_rel_path};
use super::{DbProvider, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::ports::bundle_store::BundleStore;
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
    policy_enforcer: PolicyEnforcer,
}

impl<B: BundlesRepository, R: TestReposRepository> BundlesService<B, R> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<B>,
        repos_repo: Arc<R>,
        store: Arc<dyn BundleStore>,
        repos_dir: PathBuf,
        bundle_ttl: time::Duration,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            repo,
            repos_repo,
            store,
            repos_dir,
            bundle_ttl,
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
        };

        let conn = self.db.conn()?;
        let tenant_id = ctx.subject_tenant_id();

        match self.repo.create(&conn, &scope, tenant_id, descriptor).await {
            Ok(created) => {
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

/// Lower-hex SHA-256 digest of `bytes`. Shared with `super::ssh_keys` (the
/// fingerprint) and the unit tests.
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
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
