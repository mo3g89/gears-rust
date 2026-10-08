//! Test repository service: CRUD, on-demand sync, branch cache reads.
//!
//! ## URL policy (ADR-0005 `cpt-cf-qa-adr-git-egress`, amended 2026-08-27)
//!
//! `create_repo` accepts `https://`, `http://`, `ssh://` and scp-like
//! `user@host:path` repository URLs. Local forms (`file://`, bare host
//! paths, Windows drive paths) stay rejected at create time: the git
//! engine's local transport would treat a host path as a repository to
//! sync, which would make repo registration an arbitrary host-file read
//! primitive via plan discovery.
//!
//! The classifier is [`crate::domain::git_url::classify_remote`], shared
//! with the sync engine so validation and execution cannot disagree.
//!
//! ## Credential handling
//!
//! Repository credentials live in credstore only: `create_repo` rejects URLs
//! embedding userinfo credentials, the resolved secret material goes to
//! [`RepoSyncPort::sync`] and nowhere else (never logged, never persisted),
//! and every error string recorded in `sync_error` is sanitized first
//! (userinfo in embedded URLs redacted, any occurrence of the resolved
//! material stripped).

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::{CredStoreClientV1, CredStoreError, SecretRef};
use qa_catalog_sdk::{NewTestRepository, TestRepository, TestRepositoryUpdate};
use regex::Regex;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use super::branch_snapshot::BranchSync;
use super::plans::validate_rel_path;
use super::sync_cache::SyncCache;
use super::validation::validate_name;
use super::{DbProvider, RemoteFault, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::git_url::{RemoteKind, classify_remote};
use crate::domain::ports::repo_sync::RepoSyncPort;
use crate::domain::repos::{RefreshTarget, SshKeysRepository, TestReposRepository};
use crate::domain::system_actor;

/// Userinfo (`user`, `user:token`) in any `scheme://userinfo@host` URL.
/// Used to redact engine error text before it is persisted in `sync_error`.
#[allow(clippy::unwrap_used)] // Compile-time-known regex pattern; panic in init is intentional
static URL_USERINFO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?P<scheme>[A-Za-z][A-Za-z0-9+.-]*://)[^/\s@]+@").unwrap());

/// The second half of every "credential not accessible" reason
/// [`ReposService::resolve_credential`] records. credstore answers a missing
/// secret, a secret another subject owns privately and a denied read alike, so
/// the reason cannot say which happened; it names the one an operator cannot
/// see from the secret list.
pub const UNREADABLE_CREDENTIAL_HINT: &str = "qa-catalog reads a repository's \
     credential as its system actor in the tenant that owns the repository, for a sync, a read \
     and the background branch refresh alike, never as the user who stored it, so only a \
     secret readable by the system actor in the owning tenant is found: store it there with \
     `tenant` sharing (a `private` secret is readable by its owner only, and one stored in \
     another tenant is not visible at all), or `shared` from a parent tenant";

/// Test repository service.
///
/// `K` is the SSH-key metadata repository: an SSH remote's `credential_ref`
/// names a `qa_ssh_keys` row, not a credstore reference directly, so
/// resolving one takes a lookup here first. See
/// [`resolve_credential`](Self::resolve_credential).
#[domain_model]
pub struct ReposService<R: TestReposRepository, K: SshKeysRepository> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    ssh_keys: Arc<K>,
    credstore: Arc<dyn CredStoreClientV1>,
    sync_engine: Arc<dyn RepoSyncPort>,
    repos_dir: PathBuf,
    sync_cache: Arc<SyncCache>,
    policy_enforcer: PolicyEnforcer,
}

impl<R: TestReposRepository + 'static, K: SshKeysRepository + 'static> ReposService<R, K> {
    // Eight collaborators: the seven this service already had plus the
    // SSH-key repository the ssh credential path resolves through. Grouping
    // them into a parameter struct would only move the same list one type
    // away, and `AppServices::new` is the single caller.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<R>,
        ssh_keys: Arc<K>,
        credstore: Arc<dyn CredStoreClientV1>,
        sync_engine: Arc<dyn RepoSyncPort>,
        repos_dir: PathBuf,
        sync_cache: Arc<SyncCache>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            repo,
            ssh_keys,
            credstore,
            sync_engine,
            repos_dir,
            sync_cache,
            policy_enforcer,
        }
    }
}

// Business logic methods
impl<R: TestReposRepository + 'static, K: SshKeysRepository + 'static> ReposService<R, K> {
    #[instrument(skip(self, ctx))]
    pub async fn list_repos(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<TestRepository>, DomainError> {
        debug!("Listing test repositories");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::LIST, None)
            .await?;

        let conn = self.db.conn()?;
        self.repo.list(&conn, &scope).await
    }

    #[instrument(skip(self, ctx), fields(repo_id = %id))]
    pub async fn get_repo(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<TestRepository, DomainError> {
        debug!("Getting test repository by id");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::GET, Some(id))
            .await?;

        let conn = self.db.conn()?;
        self.repo
            .get(&conn, &scope, id)
            .await?
            .ok_or(DomainError::NotFound { id })
    }

    /// Register a test repository.
    ///
    /// Scheme policy (ADR-0005, amended 2026-08-27): `new.url` must be an
    /// `https://`, `http://`, `ssh://` or scp-like `user@host:path` remote.
    /// Embedded credentials are rejected; a bare `git@` on an ssh remote is
    /// a username, not a credential — see [`validate_repo_url`].
    #[instrument(skip(self, ctx, new), fields(name = %new.name))]
    pub async fn create_repo(
        &self,
        ctx: &SecurityContext,
        new: NewTestRepository,
    ) -> Result<TestRepository, DomainError> {
        info!("Creating test repository");

        validate_name("name", &new.name)?;
        validate_repo_url(&new.url)?;
        validate_name("default_branch", &new.default_branch)?;
        if !new.content_root.is_empty() {
            validate_rel_path("content_root", &new.content_root)?;
        }
        if let Some(raw_ref) = &new.credential_ref {
            // Format-validate the credstore reference up front; existence is
            // checked at sync time (a ref may legitimately be provisioned
            // later than the repository row).
            SecretRef::new(raw_ref.clone()).map_err(|e| DomainError::Validation {
                field: "credential_ref".to_owned(),
                message: e.to_string(),
            })?;
        }

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::CREATE, None)
            .await?;

        let conn = self.db.conn()?;
        let tenant_id = ctx.subject_tenant_id();

        let repo = self.repo.create(&conn, &scope, tenant_id, new).await?;

        info!("Successfully created test repository with id={}", repo.id);
        Ok(repo)
    }

    /// Replace a registered repository's mutable fields
    /// (PRD `cpt-cf-qa-fr-catalog-repos`: "register, update, and remove").
    ///
    /// Delete-and-recreate is not a substitute: `qa_custom_plans.files`
    /// embeds `repo_id` inside JSON, so a re-registered repository orphans
    /// every custom plan that referenced the old row.
    ///
    /// Validation is *identical* to [`create_repo`](Self::create_repo) — the
    /// same URL policy (http(s)/ssh, no embedded credentials; ADR-0005 as
    /// amended) and the same `content_root` containment rule. An update path
    /// that validated less than create would be the way around create's
    /// policy.
    ///
    /// # Mutable `default_branch`
    ///
    /// `default_branch` is updatable: with per-branch snapshots the column no
    /// longer *is* the identity of the working copy's contents, it is only
    /// the branch chosen when a caller names none. Changing it therefore
    /// needs no invalidation — the previously materialized branches stay
    /// valid and readable.
    ///
    /// # Working-copy invalidation
    ///
    /// Changing `url` or `content_root` makes the existing working area stale
    /// (different remote, or a content root that may not exist in it). Rather
    /// than keep serving content fetched from the *old* URL, the synced state
    /// is cleared in the same UPDATE: no read is served from the old content,
    /// and the next read of a branch syncs it from the new `url` first (see
    /// `super::branch_snapshot`), or fails closed when that sync fails. The
    /// on-disk repository directory (the
    /// shared clone plus every branch snapshot) is also removed, and the
    /// freshness cache evicted: content reads resolve snapshot directories
    /// directly — no URL check on the read path — so a stale snapshot left
    /// behind would keep serving the old remote's content. The whole
    /// destructive step holds the repo-tier sync lock, and `sync_repo`
    /// re-reads the row under that same lock, so an in-flight sync can
    /// neither rebuild old-remote content after the wipe nor record it as
    /// synced.
    #[instrument(skip(self, ctx, update), fields(repo_id = %id, name = %update.name))]
    pub async fn update_repo(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        update: TestRepositoryUpdate,
    ) -> Result<TestRepository, DomainError> {
        info!("Updating test repository");

        validate_name("name", &update.name)?;
        validate_repo_url(&update.url)?;
        validate_name("default_branch", &update.default_branch)?;
        if !update.content_root.is_empty() {
            validate_rel_path("content_root", &update.content_root)?;
        }
        if let Some(raw_ref) = &update.credential_ref {
            // Format-validated up front, existence checked at sync time —
            // same rule as create.
            SecretRef::new(raw_ref.clone()).map_err(|e| DomainError::Validation {
                field: "credential_ref".to_owned(),
                message: e.to_string(),
            })?;
        }

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::UPDATE, Some(id))
            .await?;

        // Ownership/existence precheck under its own GET scope: a foreign or
        // nonexistent id must 404 before anything is written, and the current
        // row is what decides whether the working copy is now stale.
        let get_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::GET, Some(id))
            .await?;

        let conn = self.db.conn()?;
        let existing = self
            .repo
            .get(&conn, &get_scope, id)
            .await?
            .ok_or(DomainError::NotFound { id })?;

        let invalidate = existing.url != update.url || existing.content_root != update.content_root;
        if invalidate {
            info!(
                repo_id = %id,
                "Repository url/content_root changed; clearing synced state until the next sync"
            );
        }

        // Destructive invalidation must serialize with in-flight syncs on the
        // repo-tier lock: without it, a sync that read the OLD row could
        // finish materializing old-remote content AFTER the working area is
        // cleared here, and then record it as freshly synced. `sync_repo`
        // re-reads the row under this same lock, so holding it across the
        // sync-state clear + directory removal + cache eviction closes the
        // race from this side.
        //
        // A credential change takes the same lock, so it cannot land between
        // a lazy read's re-check and its write of a credential fault
        // (`record_listing_fault`): either that fault is written first and
        // the backoff cleared below, or the read finds the new credential and
        // writes nothing.
        let credential_changed = existing.credential_ref != update.credential_ref;
        let _repo_guard = if invalidate || credential_changed {
            Some(self.sync_cache.repo_lock(id).await.lock_owned().await)
        } else {
            None
        };

        let updated = self
            .repo
            .update(&conn, &scope, id, update, invalidate)
            .await?
            .ok_or(DomainError::NotFound { id })?;

        if invalidate {
            // Best-effort removal of the whole working area (shared clone +
            // every branch snapshot): reads resolve snapshot directories
            // directly, so a leftover snapshot would serve the old content.
            // A failure is logged, not fatal — the DB row already reads as
            // never-synced, so reads fail closed regardless.
            let repo_root = self.repos_dir.join(id.to_string());
            if let Err(err) = tokio::fs::remove_dir_all(&repo_root).await
                && err.kind() != std::io::ErrorKind::NotFound
            {
                warn!(repo_id = %id, error = %err, "Failed to clear the repository working area");
            }
            self.sync_cache.invalidate_repo(id).await;
        }

        // A different credential or remote is a different answer: end the
        // failure backoff so the next read asks (DESIGN §3.3). A url change
        // also invalidates the whole repository above, which ends it too.
        if existing.url != updated.url || existing.credential_ref != updated.credential_ref {
            self.sync_cache.clear_backoff(id).await;
        }

        info!("Successfully updated test repository");
        Ok(updated)
    }

    #[instrument(skip(self, ctx), fields(repo_id = %id))]
    pub async fn delete_repo(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        info!("Deleting test repository");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::DELETE, Some(id))
            .await?;

        // Serialize with in-flight syncs (repo-tier lock, same rationale as
        // `update_repo`'s invalidation path): a sync racing this delete must
        // not rebuild the working area after it is removed and then record
        // the deleted repository's content as synced. `sync_repo` re-reads
        // the row under this lock and 404s once the row is gone.
        let repo_lock = self.sync_cache.repo_lock(id).await;
        let _repo_guard = repo_lock.lock().await;

        let conn = self.db.conn()?;
        let deleted = self.repo.delete(&conn, &scope, id).await?;
        if !deleted {
            return Err(DomainError::NotFound { id });
        }

        // Best-effort cleanup of the synced working area (shared clone +
        // branch snapshots). A leftover directory is harmless (repo IDs are
        // UUIDs, never reused), so failures are logged and swallowed.
        let repo_root = self.repos_dir.join(id.to_string());
        if let Err(err) = tokio::fs::remove_dir_all(&repo_root).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            warn!(repo_id = %id, error = %err, "Failed to remove repository working area");
        }
        self.sync_cache.invalidate_repo(id).await;

        info!("Successfully deleted test repository");
        Ok(())
    }

    /// Sync `branch` of repository `id` into its snapshot directory
    /// (`branch = ""` selects the repository's default branch). Engine
    /// failures are *recorded* (sanitized) in `sync_error` and returned as a
    /// successful call carrying the updated repository, matching the SDK
    /// contract ("returns when the sync completes or fails").
    ///
    /// `force` skips (and evicts) the freshness cache — what the explicit
    /// sync route and qa-runs' dispatch use, so a run never reads a stale
    /// snapshot. A content read that finds a branch without a snapshot syncs
    /// it through here with `false` (see `super::branch_snapshot`): a branch
    /// synced within the TTL is not fetched again while the repository's
    /// `sync_error` is clear, and concurrent first reads of one branch wait on
    /// the locks below and then find it fresh. A forced sync ends the
    /// repository's failure backoff.
    #[instrument(skip(self, ctx), fields(repo_id = %id, branch = %branch, force))]
    pub async fn sync_repo(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        branch: &str,
        force: bool,
    ) -> Result<TestRepository, DomainError> {
        info!("Syncing test repository");

        let sync_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::SYNC, Some(id))
            .await?;

        // Ownership/existence precheck under its own GET scope
        // (`replace_branches` resolves the owning tenant itself, off the row).
        // This pre-lock row serves ONLY the default-branch resolution (which
        // fixes the branch-lock identity) and the freshness fast path; the
        // row the engine syncs is re-read under the locks below.
        let get_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::GET, Some(id))
            .await?;
        let repo = {
            let conn = self.db.conn()?;
            self.repo
                .get(&conn, &get_scope, id)
                .await?
                .ok_or(DomainError::NotFound { id })?
        };

        let branch = if branch.trim().is_empty() {
            repo.default_branch.clone()
        } else {
            branch.trim().to_owned()
        };

        // A fresh branch fetches nothing — unless the row carries a recorded
        // `sync_error`. That column is repository-wide and every read refuses on
        // it (`plans::require_synced`), so handing the row back as it is would
        // refuse a reader of fresh branch X with branch Y's failure for the rest
        // of the window (DESIGN §3.3). Re-syncing X is what clears it.
        //
        // A forced sync is an operator (or a launch) asking for the remote now:
        // it ends the repository's failure backoff before it tries.
        if force {
            self.sync_cache.invalidate(id, &branch).await;
            self.sync_cache.clear_backoff(id).await;
        } else if repo.sync_error.is_none() && self.sync_cache.is_fresh(id, &branch).await {
            return Ok(repo);
        }

        // Two-tier lock: the repo tier serializes every git mutation against
        // the shared object store; the branch tier collapses duplicate syncs
        // of the same branch. Order is always repo-then-branch — the reverse
        // would deadlock against a concurrent caller.
        let repo_lock = self.sync_cache.repo_lock(id).await;
        let branch_lock = self.sync_cache.branch_lock(id, &branch).await;
        let _repo_guard = repo_lock.lock().await;
        let _branch_guard = branch_lock.lock().await;

        // Re-read the row under the lock and sync THAT row: `update_repo` /
        // `delete_repo` mutate the working area while holding the repo-tier
        // lock, so a URL/credential change (or a delete) landing between the
        // pre-lock read and lock acquisition must not let this call fetch the
        // OLD remote and record its content as freshly synced.
        let repo = {
            let conn = self.db.conn()?;
            self.repo
                .get(&conn, &get_scope, id)
                .await?
                .ok_or(DomainError::NotFound { id })?
        };

        // Re-check under the lock: a concurrent caller may have just synced
        // (and, by succeeding, cleared the error this row would carry).
        if !force && repo.sync_error.is_none() && self.sync_cache.is_fresh(id, &branch).await {
            return Ok(repo);
        }
        // Read under the lock, after the row: a credential fault this sync
        // records backs the repository off only if nothing ended the backoff
        // since (see `SyncCache::mark_backoff_if_current`).
        let generation = self.sync_cache.backoff_generation(id).await;

        let credential = match self.resolve_credential(ctx, &sync_scope, &repo).await? {
            ResolvedCredential::Available(credential) => credential,
            ResolvedCredential::Inaccessible(message) => {
                let recorded = self
                    .record_sync_failure(&sync_scope, &repo, message)
                    .await?;
                self.back_off_for_recorded_fault(&recorded, generation)
                    .await;
                return Ok(recorded);
            }
        };

        let host_dir = crate::infra::git::layout::host_dir(&self.repos_dir, repo.id);
        let branch_workdir =
            crate::infra::git::layout::branch_workdir(&self.repos_dir, repo.id, &branch);
        let outcome = self
            .sync_engine
            .sync(
                &repo.url,
                &branch,
                credential.as_deref(),
                &host_dir,
                &branch_workdir,
            )
            .await;

        match outcome {
            Ok(result) => {
                self.sync_cache.mark_synced(id, &branch).await;
                self.sync_cache.clear_backoff(id).await;
                self.record_sync_success(&sync_scope, id, result.branches, result.head_commit)
                    .await
            }
            Err(engine_err) => {
                let sanitized = sanitize_sync_error(&engine_err.to_string(), credential.as_deref());
                let recorded = self
                    .record_sync_failure(&sync_scope, &repo, sanitized)
                    .await?;
                // DESIGN §3.3 "Limits on talking to a remote": a credential
                // fault, a size fault and a content sync that timed out are all
                // recorded, and each backs the repository off for its recorded
                // reason, so reads inside the window answer that reason (400)
                // without contacting the remote. A timed-out sync is answered
                // that way rather than as an outage: the timeout is on the row,
                // and asking again inside the window would hold another
                // `sync_timeout_seconds` for the same answer. A branch listing
                // that times out is the outage case (`list_into_branch_cache`).
                match engine_err {
                    DomainError::CredentialRejected { .. }
                    | DomainError::SyncBudgetExceeded { .. }
                    | DomainError::RemoteTimedOut { .. } => {
                        self.back_off_for_recorded_fault(&recorded, generation)
                            .await;
                    }
                    _ => {}
                }
                Ok(recorded)
            }
        }
    }

    /// Cached branch names (populated by sync / the branch-cache task);
    /// reads the cache table only — never the git remote.
    #[instrument(skip(self, ctx), fields(repo_id = %id))]
    pub async fn list_branches(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Vec<String>, DomainError> {
        debug!("Listing cached branches");

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::GET, Some(id))
            .await?;

        let conn = self.db.conn()?;

        // Existence precheck (same TEST_REPO resource type and action as the
        // branch-cache read, so the one scope covers both): a foreign or
        // nonexistent repository must 404 rather than read as "no branches".
        self.repo
            .get(&conn, &scope, id)
            .await?
            .ok_or(DomainError::NotFound { id })?;

        self.repo.list_branches(&conn, &scope, id).await
    }

    /// Branch-cache refresher support (the lifecycle task in `crate::gear`):
    /// every repository, across every tenant, as `(repo_id, tenant_id)` pairs.
    ///
    /// Elevated, not PEP-compiled: `ctx` is accepted only so the caller's
    /// audit-logging `system_actor::for_branch_refresh_enumeration`
    /// construction still reads as feeding this read — the context it builds
    /// is never passed to `access_scope`. See `domain::elevated` for why this
    /// nil-tenant enumeration bypasses the PEP, and [`Self::refresh_branches`]
    /// for the tenant-bound write each returned target feeds.
    #[instrument(skip(self, _ctx))]
    pub async fn list_refresh_targets(
        &self,
        // Kept, unused, so the caller's audit-logging
        // `system_actor::for_branch_refresh_enumeration` construction still
        // reads as feeding this read.
        _ctx: &SecurityContext,
    ) -> Result<Vec<RefreshTarget>, DomainError> {
        debug!("Listing branch-cache refresh targets");

        // Nil-tenant enumeration: elevated here rather than authorized. See
        // `domain::elevated` for why, and for why the per-repository write
        // that follows (`Self::refresh_branches`) is still tenant-bound.
        let scope = crate::domain::elevated::enumeration_scope();

        let conn = self.db.conn()?;
        self.repo.list_refresh_targets(&conn, &scope).await
    }

    /// Refresh the repository's branch cache from the remote WITHOUT a full
    /// content sync (`RepoSyncPort::list_remote_branches` — a bare ls-refs).
    ///
    /// Used by the branch-cache refresher lifecycle task. `ctx` authorizes
    /// the refresh (see `crate::domain::system_actor::for_branch_refresh`);
    /// the rewritten rows carry the repository's owning tenant whatever
    /// `ctx`'s tenant is.
    ///
    /// Unlike [`sync_repo`](Self::sync_repo), failures are returned (for the
    /// caller to log), not recorded in `sync_error` — that column reports
    /// content-sync outcomes, and a transient ls-refs hiccup must not
    /// clobber it. The lazy read uses the same listing through
    /// `try_refresh_branches` and answers its failures by class — see
    /// `sync_branch_for_read`.
    #[instrument(skip(self, ctx), fields(repo_id = %id))]
    pub async fn refresh_branches(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError> {
        debug!("Refreshing branch cache");
        self.try_refresh_branches(ctx, id, Backoff::Ignore)
            .await?
            .listing
            .map_err(|failure| DomainError::SyncFailed {
                message: failure.into_message(),
            })
    }
}

// Sync internals
//
// `'static` bound: the transaction closure's future captures `Arc<R>`, and
// `DBProvider::transaction` requires its captures to be `'static`.
impl<R: TestReposRepository + 'static, K: SshKeysRepository + 'static> ReposService<R, K> {
    /// One listing of the remote's branches into the branch cache, under
    /// `ctx`'s `SYNC` and `GET` scopes, with its failure classified rather
    /// than raised (DESIGN §3.3 "Branch model and the first read of a
    /// branch"). `Err` is only what is not the listing's own failure: a
    /// refused scope, a row that is not there, a credstore fault, or a
    /// database fault reading the row or writing the cache. With
    /// [`Backoff::Honour`], a repository inside its failure backoff answers
    /// from it without contacting the remote, and an unreachable remote
    /// starts the backoff (a credential fault is backed off once recorded,
    /// by the caller). A successful listing ends it either way.
    async fn try_refresh_branches(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        backoff: Backoff,
    ) -> Result<RefreshAttempt, DomainError> {
        let sync_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::SYNC, Some(id))
            .await?;

        // Ownership/existence precheck under its own GET scope (mirrors
        // `sync_repo`).
        let get_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_REPO, actions::GET, Some(id))
            .await?;
        // Read before the row and before anything that can fail: a failure
        // found below backs the repository off only if nothing ended the
        // backoff since. Read after the row instead, a url change landing
        // between the two (it ends the backoff) would let this attempt back
        // the moved repository off with the old remote's failure.
        let generation = self.sync_cache.backoff_generation(id).await;
        let repo = {
            let conn = self.db.conn()?;
            self.repo
                .get(&conn, &get_scope, id)
                .await?
                .ok_or(DomainError::NotFound { id })?
        };

        let listing = self
            .list_into_branch_cache(ctx, &sync_scope, &repo, backoff, generation)
            .await?;
        Ok(RefreshAttempt {
            sync_scope,
            get_scope,
            repo,
            generation,
            listing,
        })
    }

    /// The body of [`Self::try_refresh_branches`] once the row is read: answer
    /// from the backoff, or resolve the credential, list the remote and write
    /// the branch cache. The outer `Err` is a fault before or after the
    /// listing (credstore, database); the inner one is the classified listing
    /// failure.
    ///
    /// Only the lazy read ([`Backoff::Honour`]) starts a backoff, and only an
    /// unreachable remote's here: a credential fault is backed off by
    /// [`Self::record_listing_fault`], once its reason is on the row. The
    /// refresher neither waits on nor starts one; its successful listing
    /// still ends one.
    async fn list_into_branch_cache(
        &self,
        ctx: &SecurityContext,
        sync_scope: &AccessScope,
        repo: &TestRepository,
        backoff: Backoff,
        generation: u64,
    ) -> Result<Result<(), ListingFailure>, DomainError> {
        let id = repo.id;
        if matches!(backoff, Backoff::Honour)
            && let Some(answer) = self.answer_from_backoff(repo).await
        {
            return Ok(Err(answer));
        }

        let credential = match self.resolve_credential(ctx, sync_scope, repo).await? {
            ResolvedCredential::Available(credential) => credential,
            ResolvedCredential::Inaccessible(message) => {
                return Ok(Err(ListingFailure::Configuration(message)));
            }
        };

        let branches = match self
            .list_remote_branches_classified(id, &repo.url, credential.as_deref())
            .await
        {
            Ok(branches) => branches,
            Err(failure) => {
                if matches!(backoff, Backoff::Honour)
                    && matches!(failure, ListingFailure::Unavailable(_))
                {
                    self.sync_cache
                        .mark_backoff_if_current(id, RemoteFault::Unreachable, None, generation)
                        .await;
                }
                return Ok(Err(failure));
            }
        };

        let repo_handle = Arc::clone(&self.repo);
        // Owned clone: the `for<'a>` transaction closure cannot capture
        // caller-lifetime references in its returned future.
        let scope = sync_scope.clone();

        // Transactional so the cache is never observed half-written (same
        // invariant as `record_sync_success`).
        self.db
            .transaction(move |tx| {
                Box::pin(
                    async move { repo_handle.replace_branches(tx, &scope, id, branches).await },
                )
            })
            .await?;

        debug!(repo_id = %id, "Branch cache refreshed");
        Ok(Ok(()))
    }

    /// What a repository inside its failure backoff answers, without the
    /// remote being contacted; `None` when the remote is to be asked.
    async fn answer_from_backoff(&self, repo: &TestRepository) -> Option<ListingFailure> {
        let id = repo.id;
        match self.sync_cache.backoff(id).await {
            Some(RemoteFault::Unreachable) => {
                debug!(repo_id = %id, "Remote failed to list within the backoff window; not asking it again");
                Some(ListingFailure::Unavailable(format!(
                    "the remote of repository {id} could not be listed a moment ago; not asked again until the backoff ends"
                )))
            }
            // Only while the row still carries the very reason this backoff
            // recorded: the column is repository-wide, and another branch's
            // failure landing on it since must not be answered as this one.
            Some(RemoteFault::Configuration) => {
                let recorded = self.sync_cache.recorded_reason(id).await?;
                (repo.sync_error.as_deref() == Some(recorded.as_str())).then(|| {
                    debug!(repo_id = %id, "Recorded fault within the backoff window; answering the recorded reason");
                    ListingFailure::Recorded(recorded)
                })
            }
            None => None,
        }
    }

    /// List the remote's branches, classifying a failure; a successful
    /// listing ends the backoff. The failure message is sanitized.
    async fn list_remote_branches_classified(
        &self,
        id: Uuid,
        url: &str,
        credential: Option<&str>,
    ) -> Result<Vec<String>, ListingFailure> {
        match self.sync_engine.list_remote_branches(url, credential).await {
            Ok(branches) => {
                self.sync_cache.clear_backoff(id).await;
                Ok(branches)
            }
            Err(engine_err) => {
                let sanitized = sanitize_sync_error(&engine_err.to_string(), credential);
                if matches!(engine_err, DomainError::CredentialRejected { .. }) {
                    Err(ListingFailure::Configuration(sanitized))
                } else {
                    Err(ListingFailure::Unavailable(sanitized))
                }
            }
        }
    }

    /// Resolve the repository's credential material from credstore. The
    /// material is returned to the caller for the engine call only — it is
    /// never logged and never persisted.
    ///
    /// # The reference means different things per scheme
    ///
    /// `qa_test_repositories` has one `credential_ref` column and no
    /// auth-mode column, so the URL scheme decides how it is read:
    ///
    /// * **http(s)** — `credential_ref` *is* a credstore reference, holding
    ///   basic-auth material. Unchanged from p1.
    /// * **ssh** — `credential_ref` is the id of a `qa_ssh_keys` row, and
    ///   the material is at that row's `credstore_ref`. It has to be this
    ///   way round: the REST `SshKeyDto` deliberately withholds
    ///   `credstore_ref` (publishing it would let any tenant member read
    ///   another member's key straight out of credstore), so the only
    ///   handle a client can send is the row id — and the UI does exactly
    ///   that (`adapters.ts:950`, `credential_ref: form.ssh_key_id`).
    ///
    /// An SSH remote with no `credential_ref` resolves to `None` and is
    /// attempted unauthenticated: public repositories over SSH exist.
    ///
    /// # Who reads the secret
    ///
    /// Read as the system actor ([`system_actor::for_credential_read`]),
    /// bound to the repository's owning tenant read off its row through
    /// `scope`; the caller's `ctx` authorizes, it does not read. That is the
    /// identity and the tenant the background branch refresher reads as, so
    /// a sync or a read cannot succeed on a secret the refresher cannot read.
    /// `ctx` still resolves an ssh remote's key row.
    async fn resolve_credential(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        repo: &TestRepository,
    ) -> Result<ResolvedCredential, DomainError> {
        let Some(raw_ref) = &repo.credential_ref else {
            return Ok(ResolvedCredential::Available(None));
        };

        // An unparseable URL cannot reach here through `create_repo`/
        // `update_repo`, but resolving is not the place to re-litigate it:
        // treat anything unrecognised as http(s), i.e. the p1 behavior.
        let credstore_ref = if classify_remote(&repo.url) == Ok(RemoteKind::Ssh) {
            match self.resolve_ssh_key_ref(ctx, raw_ref).await? {
                Ok(reference) => reference,
                Err(unresolved) => return Ok(unresolved),
            }
        } else {
            raw_ref.clone()
        };

        let key = SecretRef::new(credstore_ref.clone()).map_err(|e| DomainError::Validation {
            field: "credential_ref".to_owned(),
            message: e.to_string(),
        })?;

        // Read as the system actor every other path reads as (see
        // `system_actor::for_credential_read`), bound to the tenant that owns
        // the repository — the tenant the refresher binds to — not the
        // caller's. `ctx` still authorized the sync, and still resolves an
        // ssh remote's key row above.
        let owner = {
            let conn = self.db.conn()?;
            self.repo
                .owner_tenant(&conn, scope, repo.id)
                .await?
                .ok_or(DomainError::NotFound { id: repo.id })?
        };
        let reader = system_actor::for_credential_read(owner);
        match self.credstore.get(&reader, &key).await {
            Ok(Some(secret)) => {
                let material =
                    String::from_utf8(secret.value.as_bytes().to_vec()).map_err(|_| {
                        DomainError::CredStore("credential material is not valid UTF-8".to_owned())
                    })?;
                Ok(ResolvedCredential::Available(Some(material)))
            }
            // The single not-found surface: missing, private to someone else,
            // or denied to the actor. All three are a fact about the
            // repository's configuration, recorded as a sync failure with the
            // reason (the ref name is repo-row metadata, not a secret).
            // Names the reference that was actually queried in credstore
            // (`credstore_ref`): for an ssh remote that is the key row's
            // `credstore_ref`, not the caller's `credential_ref` (a
            // `qa_ssh_keys` row id) -- naming the row id here would read as
            // the id->credstore_ref hop being missing when it had, in fact,
            // already succeeded. For http(s), `raw_ref == credstore_ref`, so
            // only one reference is printed.
            Ok(None) | Err(CredStoreError::NotFound | CredStoreError::AccessDenied) => {
                let named = if raw_ref == &credstore_ref {
                    format!("credential '{credstore_ref}' is not accessible in credstore")
                } else {
                    format!(
                        "credential '{raw_ref}' (credstore ref '{credstore_ref}') is not \
                         accessible in credstore"
                    )
                };
                Ok(ResolvedCredential::Inaccessible(format!(
                    "{named}: {UNREADABLE_CREDENTIAL_HINT}"
                )))
            }
            Err(e) => Err(DomainError::CredStore(e.to_string())),
        }
    }

    /// Turn an SSH remote's `credential_ref` into the credstore reference
    /// holding the private key.
    ///
    /// `Ok(Ok(reference))` — resolved. `Ok(Err(..))` — not resolvable, with
    /// the operator-facing reason to record in `sync_error`; that is a
    /// recorded sync failure, not a hard error, matching how a missing
    /// credstore secret behaves on the http path.
    ///
    /// A `credential_ref` that does not parse as a UUID is passed through
    /// unchanged as a credstore reference. That keeps an operator-configured
    /// direct reference working (and keeps this from becoming a breaking
    /// change for any ssh repository configured that way), while the UI's
    /// row ids take the lookup path.
    async fn resolve_ssh_key_ref(
        &self,
        ctx: &SecurityContext,
        raw_ref: &str,
    ) -> Result<Result<String, ResolvedCredential>, DomainError> {
        let Ok(key_id) = Uuid::parse_str(raw_ref) else {
            return Ok(Ok(raw_ref.to_owned()));
        };

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::SSH_KEY, actions::GET, Some(key_id))
            .await?;
        let conn = self.db.conn()?;

        match self.ssh_keys.find_by_id(&conn, &scope, key_id).await? {
            Some(key) => Ok(Ok(key.credstore_ref)),
            // The id is repo-row metadata, not a secret, so naming it is
            // safe and is what makes this actionable.
            None => Ok(Err(ResolvedCredential::Inaccessible(format!(
                "ssh key '{raw_ref}' is not registered"
            )))),
        }
    }

    /// Persist a successful sync: replace the branch cache and clear the
    /// sync error, atomically — the cache must never be observed
    /// half-written.
    async fn record_sync_success(
        &self,
        scope: &AccessScope,
        id: Uuid,
        branches: Vec<String>,
        head_commit: String,
    ) -> Result<TestRepository, DomainError> {
        let now = OffsetDateTime::now_utc();
        let repo = Arc::clone(&self.repo);
        // Owned clone: the `for<'a>` transaction closure cannot capture
        // caller-lifetime references in its returned future.
        let scope = scope.clone();

        let updated = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    repo.replace_branches(tx, &scope, id, branches).await?;
                    repo.update_sync_state(tx, &scope, id, Some(now), Some(head_commit), None)
                        .await?
                        .ok_or(DomainError::NotFound { id })
                })
            })
            .await?;

        info!(repo_id = %id, "Repository sync succeeded");
        Ok(updated)
    }

    /// Record a failed sync attempt: keep the previous `last_synced_at` **and
    /// `head_commit`**, write the (already sanitized) error string.
    ///
    /// The revision is carried over for the same reason the timestamp is: a
    /// failed fetch did not change the working copy, so the revision it is at
    /// is still the one the last good sync left. Clearing it would make the
    /// discovery cache (`domain::service::plans`) miss on every read after a
    /// transient network failure, for content that never moved. Single statement — no
    /// transaction needed.
    async fn record_sync_failure(
        &self,
        scope: &AccessScope,
        repo: &TestRepository,
        message: String,
    ) -> Result<TestRepository, DomainError> {
        warn!(repo_id = %repo.id, error = %message, "Repository sync failed");

        let conn = self.db.conn()?;
        self.repo
            .update_sync_state(
                &conn,
                scope,
                repo.id,
                repo.last_synced_at,
                repo.head_commit.clone(),
                Some(message),
            )
            .await?
            .ok_or(DomainError::NotFound { id: repo.id })
    }

    /// Record a configuration fault the lazy read found while listing the
    /// remote, as an explicit sync records it (DESIGN §3.3 "Branch model and
    /// the first read of a branch"): the sanitized reason in `sync_error`, the
    /// last good revision kept. Under the repo-tier lock and against the row
    /// re-read under it, for the reason `sync_repo` re-reads: a sync finishing
    /// between the listing and this write must not have its `head_commit`
    /// replaced with the older one.
    ///
    /// The fault belongs to the row the listing ran against (`listed`). If
    /// the row under the lock has another `url` or `credential_ref` (an
    /// operator fixed it meanwhile; `update_repo` takes this lock for that)
    /// or another `last_synced_at` (a sync succeeded meanwhile), the fault is
    /// stale: the current row is returned unwritten and nothing is backed
    /// off. Otherwise the reason is written and the repository backed off for
    /// it.
    async fn record_listing_fault(
        &self,
        attempt: &RefreshAttempt,
        reason: String,
    ) -> Result<TestRepository, DomainError> {
        let listed = &attempt.repo;
        let id = listed.id;
        let repo_lock = self.sync_cache.repo_lock(id).await;
        let _repo_guard = repo_lock.lock().await;
        let current = {
            let conn = self.db.conn()?;
            self.repo
                .get(&conn, &attempt.get_scope, id)
                .await?
                .ok_or(DomainError::NotFound { id })?
        };
        if current.url != listed.url
            || current.credential_ref != listed.credential_ref
            || current.last_synced_at != listed.last_synced_at
        {
            debug!(repo_id = %id, "Repository changed while it was listed; not recording the credential fault");
            return Ok(current);
        }
        let recorded = self
            .record_sync_failure(&attempt.sync_scope, &current, reason)
            .await?;
        self.back_off_for_recorded_fault(&recorded, attempt.generation)
            .await;
        Ok(recorded)
    }

    /// Back `recorded`'s repository off for the configuration-class fault just
    /// written to its `sync_error` (a credential fault, a size fault, or a
    /// content sync that timed out), remembering that text, unless the backoff
    /// was ended since `generation` was read.
    async fn back_off_for_recorded_fault(&self, recorded: &TestRepository, generation: u64) {
        self.sync_cache
            .mark_backoff_if_current(
                recorded.id,
                RemoteFault::Configuration,
                recorded.sync_error.clone(),
                generation,
            )
            .await;
    }
}

enum ResolvedCredential {
    /// Credential material resolved (`None` = public repository).
    Available(Option<String>),
    /// The configured reference cannot be resolved; the message is safe to
    /// persist (contains the reference name only, never material).
    Inaccessible(String),
}

/// Why the remote's branch list could not be read — the split a lazy read
/// answers on (DESIGN §3.3 "Branch model and the first read of a branch").
/// Every message is sanitized and safe to persist.
enum ListingFailure {
    /// An operator has to fix it: the credential cannot be resolved, or the
    /// remote refused it (or demanded one none is configured).
    Configuration(String),
    /// A configuration fault inside its backoff window, already recorded in
    /// `sync_error` (the message is that recorded reason): answered again
    /// without contacting the remote and without writing it again.
    Recorded(String),
    /// Anything else: the remote is unreachable, timed out or failed, or it
    /// failed within the backoff window and was not asked again.
    Unavailable(String),
}

impl ListingFailure {
    fn into_message(self) -> String {
        match self {
            Self::Configuration(message) | Self::Recorded(message) | Self::Unavailable(message) => {
                message
            }
        }
    }
}

/// Whether a listing attempt honours the per-repository failure backoff.
/// Only the lazy read does; the refresher runs on its own interval.
#[derive(Clone, Copy)]
enum Backoff {
    Honour,
    Ignore,
}

/// What one listing attempt found, with the scopes it was authorized under
/// and the row it read, so a caller that records or answers the failure does
/// not authorize or read twice.
struct RefreshAttempt {
    sync_scope: AccessScope,
    get_scope: AccessScope,
    repo: TestRepository,
    /// The repository's backoff generation before the listing: a failure it
    /// found backs the repository off only if nothing ended the backoff since.
    generation: u64,
    listing: Result<(), ListingFailure>,
}

/// Validate a repository URL against the scheme policy (ADR-0005, amended
/// 2026-08-27) and reject embedded credentials — credentials go through
/// credstore refs only.
///
/// **Scheme allow-list (security boundary, not just a capability note):**
/// `https://`, `http://`, `ssh://` and scp-like `user@host:path` are
/// accepted. Everything else — `file://`, `git://`, bare local paths,
/// Windows drive paths, scheme-less strings — is rejected with `Validation`,
/// because the gix engine's local transport would otherwise happily "sync"
/// any host path (including another tenant's working copy under
/// `repos_dir`), turning repo registration into an arbitrary host-file read
/// via plan discovery. That refusal is the original ADR-0005 boundary and is
/// unchanged.
///
/// **Userinfo is scheme-dependent:** on http(s) any `…@host` userinfo is
/// credential material (`user:token@`, or a bare token as the username) and
/// is rejected; on ssh a bare `git@host` is the login *name* and is
/// permitted. `user:password@` is rejected under every scheme.
///
/// The policy itself lives in [`crate::domain::git_url`] so that this
/// check, credential resolution, and the sync engine cannot drift apart —
/// see that module's header for why a single classifier matters here.
fn validate_repo_url(url: &str) -> Result<(), DomainError> {
    classify_remote(url)
        .map(|_| ())
        .map_err(|e| DomainError::Validation {
            field: "url".to_owned(),
            message: e.message().to_owned(),
        })
}

/// Sanitize an engine error message before persisting it in `sync_error`:
/// redact userinfo in any embedded URL and strip any literal occurrence of
/// the resolved credential material, so tokens can never persist there.
///
/// # Known p1 limitations (deliberate, hardening-reviewed)
///
/// Covered forms: `scheme://userinfo@host` userinfo (any scheme), and the
/// resolved credential material appearing verbatim anywhere in the text.
///
/// Known-uncovered forms — accepted as a narrow p1 gap, revisit in a future
/// hardening pass:
/// - a credential echoed **URL-encoded** by the engine (e.g. `p@ss` →
///   `p%40ss`) no longer matches the verbatim `replace`;
/// - tokens in a **query-string position** (e.g. `?access_token=...`)
///   rather than in userinfo, unless they equal the resolved material.
///
/// Both require the git engine to re-encode or relocate a secret into its
/// error text; the gix adapter is not known to do either.
fn sanitize_sync_error(message: &str, credential: Option<&str>) -> String {
    let mut sanitized = URL_USERINFO_RE
        .replace_all(message, "${scheme}***@")
        .into_owned();
    if let Some(material) = credential
        && !material.is_empty()
    {
        sanitized = sanitized.replace(material, "***");
    }
    sanitized
}

/// The lazy half of a branch-content read (see `super::branch_snapshot`).
///
/// Everything a read needs to sync a branch it cannot serve, and nothing an
/// explicit sync does not already do: the existence check refreshes the
/// branch cache through [`ReposService::refresh_branches`] and reads it back
/// through [`ReposService::list_branches`]; the sync is
/// [`ReposService::sync_repo`] without `force`. All of it runs under the
/// reader's `ctx`, so it is authorized exactly as those calls are — `SYNC` on
/// the repository — and a reader without that grant gets their refusal.
#[async_trait::async_trait]
impl<R: TestReposRepository + 'static, K: SshKeysRepository + 'static> BranchSync
    for ReposService<R, K>
{
    #[instrument(skip(self, ctx, repo), fields(repo_id = %repo.id, branch = %branch))]
    async fn sync_branch_for_read(
        &self,
        ctx: &SecurityContext,
        repo: &TestRepository,
        branch: &str,
    ) -> Result<TestRepository, DomainError> {
        info!("Branch cannot be served from a snapshot; syncing it for a read");
        let repo_id = repo.id;
        // The name `sync_repo` and the snapshot layout use (both trim): the
        // membership check below must compare that one, or `" main"` reads as a
        // branch the remote lacks (DESIGN §3.3).
        let branch = branch.trim();

        // Always confirmed on the remote, never on the cached list alone: the
        // cache lags the remote both ways (a branch deleted there stays cached
        // until the next refresh), a snapshot on disk outlives its branch, and
        // a row that never synced, or whose `url` change cleared its synced
        // state (`update_repo`), may carry another remote's list. Syncing a
        // branch the remote does not have records a repository-wide
        // `sync_error` — the failure this check is here to prevent. This path
        // only runs for a branch with no snapshot or while `sync_error` is
        // already set, so the extra listing is confined to those reads, and a
        // repository whose remote or credential just failed is not asked again
        // within `remote_failure_backoff_seconds`.
        //
        // DESIGN §3.3 "Branch model and the first read of a branch": a
        // configuration fault is recorded and answered as that recorded failure
        // (the reader's `require_synced` makes it 400), exactly as an explicit
        // sync records it; anything else is 503 and records nothing. Both back
        // the repository off, so neither a down remote nor one refusing the
        // credential is asked by every read.
        let mut attempt = self
            .try_refresh_branches(ctx, repo_id, Backoff::Honour)
            .await?;
        match std::mem::replace(&mut attempt.listing, Ok(())) {
            Ok(()) => {}
            Err(ListingFailure::Configuration(reason)) => {
                return self.record_listing_fault(&attempt, reason).await;
            }
            Err(ListingFailure::Recorded(_)) => return Ok(attempt.repo),
            Err(ListingFailure::Unavailable(message)) => {
                return Err(DomainError::SyncFailed { message });
            }
        }
        let on_remote = self
            .list_branches(ctx, repo_id)
            .await?
            .iter()
            .any(|listed| listed == branch);
        if !on_remote {
            debug!("Branch is not on the remote; not syncing it");
            return Err(DomainError::BranchNotFound {
                repo_id,
                branch: branch.to_owned(),
            });
        }

        // Not forced: the freshness cache and the repo-then-branch locks
        // apply, so concurrent first reads of one branch fetch it once.
        self.sync_repo(ctx, repo_id, branch, false).await
    }
}
