//! Lease service — the CAS (compare-and-swap) retry loop around the pure
//! decision logic in [`crate::domain::lease`].
//!
//! Persistence uses optimistic concurrency (`version` column): read the
//! current state, decide the next state with the pure functions, then try to
//! write it back conditioned on the version unchanged. A concurrent writer
//! racing us fails the CAS and we retry the whole read-decide-write cycle,
//! bounded by [`CAS_MAX_RETRIES`].

use std::sync::Arc;

use toolkit_macros::domain_model;
use tracing::{debug, instrument};

use crate::domain::error::DomainError;
use crate::domain::lease::{decide_acquire, decide_release};
use crate::domain::repos::{LeasesRepository, PlatformsRepository};
use crate::domain::service::DbProvider;
use authz_resolver_sdk::PolicyEnforcer;

use super::{actions, resources};
use qa_environments_sdk::{AcquireOutcome, LeaseMode, LeaseState};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Maximum number of read-decide-write cycles before giving up on a CAS race.
///
/// `pub(super)` so the CAS-loop unit tests (`service::leases_tests`) can
/// assert the exact retry count without duplicating the magic number.
pub(super) const CAS_MAX_RETRIES: usize = 5;

/// Platform lease service.
#[domain_model]
pub struct LeasesService<L: LeasesRepository, P: PlatformsRepository> {
    db: Arc<DbProvider>,
    repo: Arc<L>,
    platforms_repo: Arc<P>,
    policy_enforcer: PolicyEnforcer,
}

impl<L: LeasesRepository, P: PlatformsRepository> LeasesService<L, P> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<L>,
        platforms_repo: Arc<P>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            repo,
            platforms_repo,
            policy_enforcer,
        }
    }
}

// Business logic methods
impl<L: LeasesRepository, P: PlatformsRepository> LeasesService<L, P> {
    /// Attempt to acquire a lease on `platform_id` for `run_id` in `mode`.
    ///
    /// Unavailability (`TargetPlatform::available == false`) blocks only
    /// *new* acquisitions — it never evicts existing holders. A platform can
    /// be flipped unavailable while runs are actively holding it; those runs
    /// keep their lease until they release it themselves.
    #[instrument(skip(self, ctx), fields(platform_id = %platform_id, run_id = %run_id))]
    pub async fn acquire(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        run_id: Uuid,
        mode: LeaseMode,
    ) -> Result<AcquireOutcome, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::LEASE, actions::ACQUIRE, Some(platform_id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        // Platform precheck: new acquisitions require the platform to exist
        // and be marked available. Existing holders are unaffected by a
        // platform going unavailable later — see doc comment above.
        // PLATFORM is a different resource type/table than LEASE, so this
        // read needs its own PEP-derived scope rather than reusing `scope`.
        let platform_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::GET, Some(platform_id))
            .await?;
        let platform = self
            .platforms_repo
            .get(&conn, &platform_scope, platform_id)
            .await?
            .ok_or(DomainError::PlatformNotFound { id: platform_id })?;
        if !platform.available {
            return Err(DomainError::PlatformUnavailable { id: platform_id });
        }

        let tenant_id = ctx.subject_tenant_id();

        for _ in 0..CAS_MAX_RETRIES {
            let current = self.repo.get(&conn, &scope, platform_id).await?;
            let (outcome, new_state) = decide_acquire(&current.state, run_id, mode);

            if matches!(outcome, AcquireOutcome::Busy { .. }) {
                // No write needed: the platform is occupied in a conflicting mode.
                return Ok(outcome);
            }

            if new_state == current.state {
                // Idempotent re-acquire (same run re-requesting its own hold):
                // the state would not change, so skip the write entirely.
                //
                // This assumes leases carry no TTL/renewal semantics today —
                // `updated_at` is not treated as a heartbeat, so skipping the
                // write here is purely a no-op optimization. If a lease
                // reaper/TTL-based expiry is ever added, re-acquire will need
                // to bump `updated_at` (i.e. still write) to refresh the
                // lease's liveness, and this early return must be revisited.
                return Ok(outcome);
            }

            match self
                .repo
                .compare_and_set(
                    &conn,
                    &scope,
                    tenant_id,
                    platform_id,
                    current.version,
                    &new_state,
                )
                .await
            {
                Ok(()) => return Ok(outcome),
                Err(DomainError::LeaseConflict) => {
                    debug!("CAS conflict acquiring lease, retrying");
                }
                Err(e) => return Err(e),
            }
        }

        Err(DomainError::LeaseConflict)
    }

    /// Release `run_id`'s hold on `platform_id`. Idempotent: releasing a run
    /// that does not currently hold the lease leaves the state unchanged and
    /// succeeds. No platform-availability precheck: release must work even
    /// on platforms that were flipped unavailable while held.
    #[instrument(skip(self, ctx), fields(platform_id = %platform_id, run_id = %run_id))]
    pub async fn release(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        run_id: Uuid,
    ) -> Result<LeaseState, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::LEASE, actions::RELEASE, Some(platform_id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let tenant_id = ctx.subject_tenant_id();

        for _ in 0..CAS_MAX_RETRIES {
            let current = self.repo.get(&conn, &scope, platform_id).await?;
            let new_state = decide_release(&current.state, run_id);

            if new_state == current.state {
                // No-op release (unknown run, or already free): skip the write.
                return Ok(new_state);
            }

            match self
                .repo
                .compare_and_set(
                    &conn,
                    &scope,
                    tenant_id,
                    platform_id,
                    current.version,
                    &new_state,
                )
                .await
            {
                Ok(()) => return Ok(new_state),
                Err(DomainError::LeaseConflict) => {
                    debug!("CAS conflict releasing lease, retrying");
                }
                Err(e) => return Err(e),
            }
        }

        Err(DomainError::LeaseConflict)
    }

    /// Read the current lease state for `platform_id`.
    ///
    /// Fails with `PlatformNotFound` for a nonexistent or foreign (wrong
    /// tenant) platform — without this precheck, a missing lease row (which
    /// is the normal, legitimate state for a never-leased platform) is
    /// indistinguishable from a missing platform, and both would silently
    /// read as `LeaseState::Free`. See `acquire`'s platform precheck above
    /// for the same rationale and the note on why this needs its own
    /// PEP-derived PLATFORM scope rather than reusing `scope`.
    #[instrument(skip(self, ctx), fields(platform_id = %platform_id))]
    pub async fn get(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
    ) -> Result<LeaseState, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::LEASE, actions::GET, Some(platform_id))
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let platform_scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::PLATFORM, actions::GET, Some(platform_id))
            .await?;
        self.platforms_repo
            .get(&conn, &platform_scope, platform_id)
            .await?
            .ok_or(DomainError::PlatformNotFound { id: platform_id })?;

        let versioned = self.repo.get(&conn, &scope, platform_id).await?;
        Ok(versioned.state)
    }
}
