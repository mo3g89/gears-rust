//! Per-platform admission: the lock, the decision, and the row.
//!
//! Ported from `manager/src/services/run_queue.rs` — [`PlatformLocks`] from
//! `:566-588` and [`AdmissionService::admit`] from `:608-680`. The pure rules it
//! composes live in [`crate::domain::queue`]; this module is the part that
//! cannot be pure, because the whole point is the ordering of four I/O
//! operations under one mutex.
//!
//! # The critical section, and what is deliberately outside it
//!
//! Inside the platform lock: the depth read, the depth check, the occupancy
//! read, the decision, the ownership precheck, the lease acquisition, and the
//! row insert. Outside it: the global-cap check (before) and the submit (after).
//!
//! * **The submit is outside.** The source system says why in place: the lock is
//!   held only across "the decision and the row insert — never across the Argo
//!   submit, which includes a repo sync and a bundle build and would otherwise
//!   serialise every launch on a platform for minutes"
//!   (`manager/src/services/run_queue.rs:569-572`, mirrored at the dispatcher's
//!   own drain, `run_dispatcher.rs:618-620`). Here the caller — `launch`'s
//!   `settle`, or the tick's drain — performs the submit after `admit` returns.
//! * **The global cap is outside, and before — before the platformless bypass
//!   too.** It is cluster-wide, so serialising it per platform buys nothing, and
//!   the source system checks it at the very top of `launch`, before the platform
//!   is even read (`manager/src/services/run_dispatcher.rs:81`, with the
//!   platformless bypass not reached until `:103-106`). A launch with no platform
//!   therefore still answers 429 against a full cluster, which is what makes
//!   `max_concurrent_runs` cluster-wide rather than per-platform.
//!
//! # The one adaptation, and why it moves which line is load-bearing
//!
//! The source system claims the queue row inside the lock **because the row is
//! an occupancy source**: `merge_occupancy` unions unreleased claims with live
//! Argo workflows, so a claim written under the lock is what a concurrent launch
//! then observes (`run_dispatcher.rs:606`, "Claim inside the lock so a
//! concurrent launch sees these as occupancy").
//!
//! Here occupancy is the **platform lease** and nothing else
//! ([`crate::domain::queue`]'s header: "there is one record … so `Occupancy` is
//! derived from `LeaseState` and `merge_occupancy` has no counterpart"). So the
//! line that must be inside the lock is the **lease acquisition**, not the row
//! insert: a concurrent launch reads the lease, so a lease taken under the lock
//! is what it sees. Both are inside here, but if one ever had to move out it
//! would have to be the insert.
//!
//! # Composition rules this module honours, from the modules it consumes
//!
//! * [`crate::domain::queue`]: **the planner is advisory and the lease CAS is
//!   the source of truth.** So a `Dispatch` decision that loses its `acquire` is
//!   downgraded to a queued row rather than started — the safe direction, and the
//!   only one the CAS permits.
//! * [`crate::domain::queue::Occupancy::from_lease`] takes a *successful* read;
//!   a failed one must be mapped to [`Occupancy::Exclusive`] by the caller. That
//!   caller is [`lease_occupancy`], which is also what the dispatcher uses, so
//!   the two halves cannot disagree about the fail-safe direction.
//! * [`crate::domain::repos::OwnedRunId`]: the queue insert's `run_id` must have
//!   been resolved under the caller's own `qa.run` scope. That is structural —
//!   `NewQueueRow::run` is an `OwnedRunId` and this module physically cannot
//!   build the payload without minting one — and the reason is the tenant-blind
//!   foreign key `domain::repos`' header and `DESIGN.md:764`'s
//!   "every unique index is tenant-prefixed" paragraph both describe: an insert
//!   carrying a guessed `run_id` would answer differently for a run that exists
//!   in another tenant than for one that does not exist at all.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use qa_environments_sdk::{AcquireOutcome, LeaseMode, QaEnvironmentsClientV1};
use qa_runs_sdk::Run;
use tokio::sync::Mutex;
use toolkit_security::{AccessScope, SecurityContext};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::launch::{Admission, Admitted, Admitter};
use super::{DbProvider, QueueLimits, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::RunExecutor;
use crate::domain::queue::{
    AdmissionDecision, Occupancy, cap_reached, decide_admission, depth_limit, global_cap_status,
    queue_is_full,
};
use crate::domain::repos::{NewQueueRow, QueueRepository, RunsRepository};

/// The cluster-wide cap's read-and-reserve gate.
///
/// # The race, and why serialising the read would not have closed it
///
/// [`AdmissionService::enforce_global_cap`] reads `executor.list_active()` and
/// compares it against `max_concurrent_runs`. The run it lets through does not
/// enter that listing until it has been **submitted**, which happens after
/// admission returns — so a mutex around the read closes nothing: the second
/// caller reads the same listing the first one did and passes the same check.
/// What closes it is counting the callers that have passed and not yet reached
/// the executor, which is what [`CapSlot`] is.
///
/// A slot is counted in [`Self::outstanding`] for as long as its holder keeps it,
/// and the holder must keep it until its submit has been made —
/// `LaunchService::launch` and `RunsService::force_start` both do, by binding it
/// for the rest of the call.
///
/// **Nothing is serialised, and deliberately.** The check-and-increment is one
/// `fetch_update`, and the executor listing stays outside it: a lock held across
/// a submit would serialise every launch in the deployment for minutes, which is
/// why the platform lock does not span one either
/// (`manager/src/services/run_queue.rs:569-572`).
///
/// The listing a caller compares against is its own snapshot and may be stale by
/// the time its increment lands. That is safe in one direction only, which is the
/// direction it takes: a run that left the listing but still holds its slot is
/// counted twice and the cap is momentarily stricter, while a run that entered
/// the listing was holding a slot until it did, so there is no window in which it
/// is counted by neither.
///
/// **What this does not close: the tick.** `service::dispatch`'s drain enforces
/// its own per-tick budget from committed claims (`evaluate_cap`) and takes no
/// slot here, so a tick and a launch can still pass the cap jointly. That
/// separation predates this gate — parity, per [`AdmissionService::enforce_global_cap`]'s
/// note that the two counts are deliberately different — and unifying them is a
/// change to what the launch path refuses, not a fix to this one.
///
/// **What this does not close: a second replica.** The count is process-local, so
/// two replicas hold two counters and both can pass a cap of one. Closing that
/// needs the reservations to be durable, which is a schema change; a distributed
/// lock around the read would not do it, for the reason in the first paragraph.
/// The frozen guide scopes the promise the same way — *"admission correctness
/// assumes a **single manager replica**"*
/// (`exclusive-runs-and-the-queue.md`, Known limitations) — and
/// [`PlatformLocks`] records the same limit for the platform half.
#[derive(Default)]
pub(in crate::domain::service) struct GlobalCapGate {
    /// Slots handed out and not yet released.
    outstanding: Arc<AtomicU32>,
}

impl GlobalCapGate {
    /// Take one slot, or refuse.
    ///
    /// `max == 0` is unlimited and costs no executor call, which is the shipped
    /// default (`run_queue.rs:856-858`).
    async fn reserve(&self, executor: &dyn RunExecutor, max: u32) -> Result<CapSlot, DomainError> {
        if max == 0 {
            return Ok(CapSlot { outstanding: None });
        }
        let active = executor.list_active().await?;
        let active = u32::try_from(active.len()).unwrap_or(u32::MAX);
        let taken = self
            .outstanding
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |held| {
                if cap_reached(global_cap_status(active.saturating_add(held), max)) {
                    None
                } else {
                    Some(held.saturating_add(1))
                }
            });
        match taken {
            Ok(_) => Ok(CapSlot {
                outstanding: Some(Arc::clone(&self.outstanding)),
            }),
            Err(held) => {
                warn!(
                    active,
                    reserved = held,
                    limit = max,
                    "launch refused: the cluster-wide concurrent-run cap is reached",
                );
                Err(DomainError::ConcurrencyLimit { limit: max })
            }
        }
    }
}

/// One run's share of `max_concurrent_runs`, released on drop.
///
/// Held from the cap check until the caller's submit has been made. Between the
/// submit landing and the drop the run is counted twice — once by the executor
/// listing, once here — which makes the cap transiently stricter and never more
/// permissive.
///
/// `pub` only so it can appear in [`super::launch::InlineDispatcher`]'s
/// signature, which is what makes the hold structural. It has no public
/// constructor, so nothing outside `service::admission` can mint one.
#[derive(Debug)]
pub struct CapSlot {
    /// `None` when the cap is disabled, and after [`Self::release`].
    outstanding: Option<Arc<AtomicU32>>,
}

impl CapSlot {
    /// A slot against a disabled cap: it counts nothing and releases nothing.
    ///
    /// **Test-only, so the production paths cannot mint one.** Substituting a
    /// fresh slot for the real one is the last spelling that still drops a
    /// reservation early — everything else is refused by the borrow
    /// [`super::launch::InlineDispatcher`] takes — and gating it here means
    /// `launch` and `runs` have no constructor to reach for. [`GlobalCapGate`]
    /// builds the disabled-cap slot from the struct literal instead, which it can
    /// because it is in this module.
    #[cfg(test)]
    pub(in crate::domain::service) fn unlimited() -> Self {
        Self { outstanding: None }
    }

    /// Give the slot back now, for an outcome that consumes no cluster capacity.
    fn release(&mut self) {
        if let Some(outstanding) = self.outstanding.take() {
            outstanding.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for CapSlot {
    fn drop(&mut self) {
        self.release();
    }
}

/// One platform's admission mutex.
type PlatformLock = Arc<Mutex<()>>;

/// Registry of per-platform admission mutexes.
///
/// Admission must be serialised per platform, or two concurrent launches both
/// observe an idle platform and both start
/// (`manager/src/services/run_queue.rs:566-572`, whose shape this copies at
/// `:573-588`). The outer mutex is held only for the lookup, never across the
/// per-platform critical section — which is what makes different platforms
/// independent.
///
/// `tokio::sync::Mutex` for both, not `std::sync::Mutex`: the critical section
/// awaits four I/O calls, and a `std` guard held across an `await` is both
/// denied by clippy and a deadlock hazard on a single-threaded runtime.
///
/// **Keyed by `platform_id` alone, so the lock is not tenant-partitioned — and
/// that is correct here.** `qa_platform_leases` is keyed on a bare
/// `platform_id` (`domain::repos::NewQueueRow::platform_id` states the
/// consequence in full), so the resource two tenants would contend for is the
/// same row. A tenant-prefixed lock would let two tenants enter the critical
/// section for one platform simultaneously and race on that row. Ownership is
/// established upstream, by the launch path's platform read; this registry's job
/// is only mutual exclusion over the lease.
///
/// **Process-local, and under N replicas the frozen contract is broken in a way
/// worth naming.** Two replicas have two registries and the mutual exclusion is
/// gone. The lease CAS still refuses to double-grant, so the failure is **not**
/// two runs on one platform — but an earlier version of this paragraph stopped at
/// "strict FIFO makes even that ugly", which understates it. The actual failure is
/// a **FIFO inversion**: replica A's inline admission reads an idle platform and
/// starts a fresh launch while replica B's tick is mid-drain on an older queued
/// row, so a later launch overtakes an earlier one. That is a direct divergence
/// from guide lines 105-107, *"the queue is strictly FIFO … otherwise a queued
/// exclusive run would starve forever"*, and it is invisible to the CAS because
/// both runs may legitimately hold a parallel lease.
///
/// Two smaller consequences of the same non-atomicity, recorded because they are
/// easy to mistake for the big one:
///
/// * the depth read and the insert are not atomic across replicas, so
///   `queue_max_depth` can overshoot by at most N−1 rows — bounded and harmless;
/// * a row that keeps losing the lease race is **survivable rather than lost**
///   only because `requeue` preserves `enqueued_at`: it keeps its FIFO place, and
///   if it loses often enough the TTL sweep expires it with the mandatory alert
///   rather than leaving it to wait forever.
///
/// The source system says the same about itself — "admission correctness assumes a
/// single manager replica" (guide line 232) — and Task 16 owes the leader-elected
/// ticker. **Note what that does and does not fix**: leader election makes the
/// *tick* single-writer, and admission still runs inline in every replica's REST
/// handler, so the inversion above survives it. Closing it needs a distributed
/// lock on the platform, not a leader on the ticker.
#[derive(Clone, Default)]
pub struct PlatformLocks {
    locks: Arc<Mutex<HashMap<Uuid, PlatformLock>>>,
}

impl PlatformLocks {
    /// This platform's mutex, creating it on first use.
    ///
    /// The returned `Arc` is the *same* one for the same `platform_id`, which is
    /// the entire property: `the_same_platform_maps_to_the_same_lock` asserts it
    /// with `Arc::ptr_eq`, because a registry that minted a fresh mutex per call
    /// would compile, would look right, and would serialise nothing.
    pub async fn get(&self, platform_id: Uuid) -> PlatformLock {
        let mut registry = self.locks.lock().await;
        Arc::clone(
            registry
                .entry(platform_id)
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }
}

/// Read a platform's occupancy from its lease, failing **closed**.
///
/// A read that fails is [`Occupancy::Exclusive`]. This is the source system's
/// fail-safe direction ported: an unreadable Argo makes it push a synthetic
/// `exclusive: true` occupant so "we assume the platform is busy, so a run
/// queues rather than trampling an exclusive run"
/// (`manager/src/services/run_dispatcher.rs:237-252`), and it logs at ERROR
/// naming the platform, which this does too. The frozen guide promises the same
/// behaviour to operators: "If the manager cannot read Argo, every platform
/// reads as busy and launches queue rather than risk starting beside an
/// exclusive run" (guide lines 217-218 — the sentence spans both).
///
/// A free function shared by admission and the dispatcher's drain, for the
/// reason [`crate::domain::queue::platform_admits`] is shared: two views of
/// occupancy would drift, and this one carries the failure direction.
pub(in crate::domain::service) async fn lease_occupancy(
    environments: &dyn QaEnvironmentsClientV1,
    ctx: &SecurityContext,
    platform_id: Uuid,
) -> Occupancy {
    match environments.get_lease(ctx, platform_id).await {
        Ok(state) => Occupancy::from_lease(&state),
        Err(error) => {
            error!(
                %platform_id,
                %error,
                "could not read the platform lease; treating the platform as exclusively \
                 held so nothing is dispatched blind",
            );
            Occupancy::Exclusive
        }
    }
}

/// The lease mode a run's resolved exclusivity asks for.
fn lease_mode(exclusive: bool) -> LeaseMode {
    if exclusive {
        LeaseMode::Exclusive
    } else {
        LeaseMode::Parallel
    }
}

/// Constructor arguments for [`AdmissionService`].
///
/// A struct rather than a positional constructor: `clippy::too_many_arguments`
/// would need an `#[allow]` either way, and the fields that are `Arc<dyn _>`
/// trait objects are mutually assignable, so a transposition between them is not
/// always a type error. Named fields make it one.
///
/// **Corrected 2026-08-14 by the code-quality review**, which found this line
/// counting "nine positional parameters … four of the nine" over a struct with
/// eight fields and two such trait objects — and three later `Deps` docs copying
/// the shape, each wrong in its own way. A count in a doc comment is a claim; this
/// one was wrong on arrival and propagated.
pub struct AdmissionDeps<R, Q> {
    pub db: Arc<DbProvider>,
    pub runs: Arc<R>,
    pub queue: Arc<Q>,
    pub environments: Arc<dyn QaEnvironmentsClientV1>,
    pub executor: Arc<dyn RunExecutor>,
    pub locks: PlatformLocks,
    pub limits: QueueLimits,
    pub policy_enforcer: PolicyEnforcer,
}

/// Decide-and-record, under the platform's admission lock.
pub struct AdmissionService<R, Q> {
    db: Arc<DbProvider>,
    runs: Arc<R>,
    queue: Arc<Q>,
    environments: Arc<dyn QaEnvironmentsClientV1>,
    executor: Arc<dyn RunExecutor>,
    locks: PlatformLocks,
    limits: QueueLimits,
    policy_enforcer: PolicyEnforcer,
    /// Not a constructor argument: one service is one gate, and the container
    /// hands *this* service to both callers of the cap.
    cap: GlobalCapGate,
}

impl<R, Q> AdmissionService<R, Q>
where
    R: RunsRepository,
    Q: QueueRepository,
{
    pub fn new(deps: AdmissionDeps<R, Q>) -> Self {
        Self {
            db: deps.db,
            runs: deps.runs,
            queue: deps.queue,
            environments: deps.environments,
            executor: deps.executor,
            locks: deps.locks,
            limits: deps.limits,
            policy_enforcer: deps.policy_enforcer,
            cap: GlobalCapGate::default(),
        }
    }

    /// The lock registry, for the composition test that proves admission and
    /// dispatch share one.
    #[cfg(test)]
    pub(in crate::domain::service) fn locks(&self) -> &PlatformLocks {
        &self.locks
    }

    /// A fresh `qa.run` scope, per call. See `domain::service`, "One scope per
    /// resource type".
    async fn run_scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
        resource_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, &resources::RUN, action, resource_id)
            .await?)
    }

    /// A fresh `qa.queue_entry` scope, per call.
    ///
    /// **Derived from [`resources::QUEUE_ENTRY`], never from
    /// [`resources::RUN`].** A scope compiled for `qa.run` and passed to a
    /// `qa_run_queue` query would authorize the wrong resource type while
    /// looking, at the call site, exactly like this one.
    async fn queue_scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
        resource_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, &resources::QUEUE_ENTRY, action, resource_id)
            .await?)
    }

    /// The cluster-wide `max_concurrent_runs` gate, checked before the platform
    /// lock.
    ///
    /// Ported from `read_global_cap` + `enforce_global_cap`
    /// (`manager/src/services/run_queue.rs:834-891`), including two details that
    /// are easy to lose:
    ///
    /// * **A disabled cap costs no executor call.** Legacy returns `None` before
    ///   listing anything when `max_concurrent_runs == 0` (`:856-858`), which is
    ///   the shipped default — so the common path makes no cross-plane call at
    ///   all.
    /// * **An unreadable executor fails the launch**, where the depth limit fails
    ///   open. Legacy makes exactly this asymmetry and states it: over-committing
    ///   is the worse outcome for the cap, while for the depth limit the worse
    ///   outcome is rejecting a legitimate launch (`:893-900`). Legacy answers
    ///   500; here the error propagates as
    ///   [`DomainError::ExecutorFailed`] and Task 16 maps it to 500.
    ///
    /// **The count is live executions only**, not committed claims — legacy
    /// counts `list_workflows().filter(is_active)` here (`:860-872`) and folds
    /// uncommitted claims in only in the *dispatcher's* arithmetic
    /// (`committed_active_count`, `:941-960`). Keeping the two different is
    /// parity; unifying them would make admission stricter than it has ever been.
    ///
    /// **Visible to `service::runs` since Task 15, and shared rather than
    /// copied.** Force start must enforce this cap and must not override it
    /// (guide lines 116-120), and the source system shares exactly this function
    /// between its launch path and its force-start handler
    /// (`manager/src/routes/run_queue.rs`, `api_force_start` calling
    /// `crate::services::run_queue::enforce_global_cap`). A second copy would be
    /// a second place for the disabled-cap short-circuit and the
    /// unreadable-executor direction to drift.
    ///
    /// **The returned [`CapSlot`] is the coordination, and dropping it early
    /// undoes the check.** The listing this reads does not include the run being
    /// admitted until it is submitted, so the slot is what a concurrent caller
    /// sees in the meantime; [`GlobalCapGate`] carries the mechanism and the
    /// limit. Both callers bind it for the rest of their own call, which is where
    /// their submit happens.
    #[must_use = "the slot is the reservation; dropping it here re-opens the race"]
    pub(in crate::domain::service) async fn enforce_global_cap(
        &self,
    ) -> Result<CapSlot, DomainError> {
        self.cap
            .reserve(self.executor.as_ref(), self.limits.max_concurrent_runs)
            .await
    }

    /// Take the lease for a run that the planner says may start now.
    ///
    /// Returns the decision that survives the CAS. `Busy` and a failed
    /// acquisition both downgrade to [`AdmissionDecision::Queue`]: the lease is
    /// the source of truth and the planner is advisory
    /// ([`crate::domain::queue`]'s module docs), so the lease may refuse a
    /// dispatch the planner allowed. It can never do the opposite, because this
    /// is only ever called on a `Dispatch` decision.
    ///
    /// A failed acquisition queues rather than propagating, for the same reason
    /// [`lease_occupancy`] reads as busy: an unknown platform state must not
    /// produce a start.
    ///
    /// # A failed acquisition also **releases**, because a lost response wedges the
    /// platform forever
    ///
    /// An earlier version of this paragraph ended "the run keeps its place in the
    /// queue and the next tick retries", which is false for the one failure that
    /// makes this interesting: an `acquire_lease` that **commits server-side and
    /// then fails to return** — a timeout, a dropped response. For an exclusive run
    /// the lease is now `HeldExclusive { holder: run }`
    /// (`qa-environments/src/domain/lease.rs:30-33`), so the next tick reads
    /// [`Occupancy::Exclusive`], `platform_admits` refuses everything, and
    /// `plan_dispatch_batch` breaks on the first row — **the next tick never
    /// retries.** The row then TTL-expires, and the sweep releases no lease; nor
    /// can reconciliation, because a `queued` row is not a claim and `all_claims`
    /// never returns it. The platform reads busy until somebody clears the lease by
    /// hand. The parallel variant is the same shape: a phantom holder blocks every
    /// future exclusive launch.
    ///
    /// So the `Err` arm hands the lease back, exactly as
    /// [`Self::give_back_lease`] does for the neighbouring case of an insert that
    /// failed after a successful acquire. **It is safe in both worlds**:
    /// `decide_release` is idempotent and removes only a hold the run actually has
    /// (`lease.rs:57-63`), so it is a no-op when the acquisition genuinely never
    /// landed.
    async fn take_lease(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        run: &Run,
    ) -> AdmissionDecision {
        match self
            .environments
            .acquire_lease(ctx, platform_id, run.id, lease_mode(run.resolved_exclusive))
            .await
        {
            Ok(AcquireOutcome::Acquired) => AdmissionDecision::Dispatch,
            Ok(AcquireOutcome::Busy { current }) => {
                info!(
                    run_id = %run.id,
                    %platform_id,
                    current = ?current,
                    "the platform lease refused a run the planner admitted; queueing it \
                     instead - the lease is the source of truth",
                );
                AdmissionDecision::Queue
            }
            Err(error) => {
                error!(
                    run_id = %run.id,
                    %platform_id,
                    %error,
                    "could not acquire the platform lease; queueing the run rather than \
                     starting it against an unknown platform state, and releasing in \
                     case the acquisition landed and only the response was lost",
                );
                // The acquisition may have committed server-side. See this method's
                // doc: without this, a lost response leaves the platform leased by a
                // run that will never be dispatched and that no later pass can free.
                self.give_back_lease(ctx, platform_id, run.id).await;
                AdmissionDecision::Queue
            }
        }
    }

    /// Give back a lease this admission took but could not record.
    ///
    /// Without this, an insert that fails after a successful `acquire` leaves the
    /// platform leased by a run that has no queue row and will never be
    /// dispatched — and because the lease is this gear's only occupancy oracle,
    /// the platform reads busy forever. Best-effort: the caller's error is the
    /// one that matters, and a failure here is logged at ERROR because it is the
    /// one that wedges a platform.
    async fn give_back_lease(&self, ctx: &SecurityContext, platform_id: Uuid, run_id: Uuid) {
        if let Err(error) = self
            .environments
            .release_lease(ctx, platform_id, run_id)
            .await
        {
            error!(
                %run_id,
                %platform_id,
                %error,
                "could not release a lease taken for a queue row that was never written; \
                 the platform will read as busy until the lease is cleared by hand",
            );
        }
    }
}

#[async_trait]
impl<R, Q> Admitter for AdmissionService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// Admit one launch: `run_queue.rs:608-680`, in that order.
    ///
    /// # Errors
    ///
    /// [`DomainError::QueueFull`] and [`DomainError::ConcurrencyLimit`] for the
    /// frozen guide's two — and only two — 429 causes (guide lines 74, 240-242);
    /// [`DomainError::RunNotFound`] when the run is not visible under the
    /// caller's own scope; [`DomainError::ExecutorFailed`] when an enabled global
    /// cap cannot be evaluated; [`DomainError::QueueRowExists`] or
    /// [`DomainError::Database`] from the insert.
    async fn admit(&self, ctx: &SecurityContext, run: &Run) -> Result<Admitted, DomainError> {
        // The cluster-wide cap comes FIRST, before the platformless bypass and
        // before the per-platform lock, because it is cluster-wide and because
        // that is where legacy puts it: `enforce_global_cap` at
        // `manager/src/services/run_dispatcher.rs:81`, the platformless bypass not
        // until `:103-106`. So legacy answers 429 for a platformless launch
        // against a full cluster, and the guide makes `max_concurrent_runs`
        // cluster-wide with exactly two 429 causes (guide lines 74, 240-242).
        //
        // **This used to be the other way round, and it was a real behavioural
        // gap**: a platformless launch skipped the cap entirely and started, while
        // `launch` enforces no cap of its own. The comment that stood here
        // asserted the ordering was "unobservable for this arm, since a
        // platformless run still counts against `max_concurrent_runs` in legacy
        // and still does here" — the first half was true, the second was false,
        // and the sentence was written while reading the legacy call site rather
        // than this code.
        //
        // The slot it returns is held for the rest of this call and handed to the
        // caller with the outcome, because the run does not enter the listing the
        // check reads until the caller has submitted it.
        let mut slot = self.enforce_global_cap().await?;

        // A run with no platform is never queued and never occupancy (guide line
        // 76; `run_dispatcher.rs:103-106`) — no row, no lease, no lock. It still
        // occupies cluster capacity, which is why the cap is above this line and
        // why the slot travels with this outcome too.
        let Some(platform_id) = run.platform_id else {
            return Ok(Admitted {
                admission: Admission::Unqueued,
                slot,
            });
        };

        let lock = self.locks.get(platform_id).await;
        let _guard = lock.lock().await;

        let conn = self.db.conn()?;

        // Inside the lock, and before anything else: two concurrent launches
        // against a queue one slot from full must not both observe room. Checked
        // before the occupancy read because a rejection writes no row, so there
        // is nothing to decide and no reason to pay for a read we would throw
        // away (`run_queue.rs:633-655`).
        let depth_scope = self.queue_scope(ctx, actions::LIST, None).await?;
        let queued = self
            .queue
            .queued_depth(&conn, &depth_scope, platform_id)
            .await?;
        let limit = depth_limit(self.limits.queue_max_depth);
        if queue_is_full(queued, limit) {
            let limit = limit.unwrap_or(0);
            warn!(
                run_id = %run.id,
                %platform_id,
                queued,
                limit,
                exclusive = run.resolved_exclusive,
                "launch refused: this platform's queue is at queue_max_depth",
            );
            // Safe to disclose the count: `platform_id` was ownership-verified
            // at launch and `qa_run_queue` is tenant-partitioned, so `queued` is
            // this tenant's own depth. If an occupancy read ever spanned
            // tenants sharing a platform, this number would become a
            // cross-tenant oracle.
            //
            // **What the limit actually binds, which is not "per platform".**
            // `queued_depth` is a *scoped* count, and `contains_uuid` /
            // `InPredicate` admit a **set** — so the bound is per
            // (access scope, platform). Two tenants sharing a `platform_id` each
            // get a full `queue_max_depth`, making the platform-wide ceiling
            // Σ over the tenants that use it; and under a hierarchical policy whose
            // scope admits several tenants, this read counts all of them while
            // `insert` stamps `subject_tenant_id` alone, so one tenant's rows can
            // consume a sibling's budget. FIFO has the same shape: `queued_rows` is
            // scoped too, so "strictly FIFO" (guide lines 105-107) holds within a
            // scope and not across tenants sharing a platform.
            //
            // **The queries are deliberately not changed** (spec review's
            // recommendation, accepted): widening the count re-opens the
            // cross-tenant oracle this comment is about, and a platform-global count
            // would let one tenant's queued rows refuse another tenant's launch —
            // a worse denial of service than an over-generous ceiling.
            // `DESIGN.md` §3.7 carries the full analysis.
            return Err(DomainError::QueueFull {
                platform_id,
                queued,
                limit,
            });
        }

        let occupancy = lease_occupancy(self.environments.as_ref(), ctx, platform_id).await;
        let planned = decide_admission(occupancy, queued, run.resolved_exclusive);

        // The ownership precheck, before the child insert and before the lease:
        // a token is cheap to obtain and expensive to unwind, and if the run is
        // not the caller's we must not have taken a lease for it.
        let get_scope = self.run_scope(ctx, actions::GET, Some(run.id)).await?;
        let owned = self.runs.resolve_owned(&conn, &get_scope, run.id).await?;

        let decision = match planned {
            AdmissionDecision::Dispatch => self.take_lease(ctx, platform_id, run).await,
            AdmissionDecision::Queue => AdmissionDecision::Queue,
        };

        let create_scope = self.queue_scope(ctx, actions::CREATE, None).await?;
        let row = match self
            .queue
            .insert(
                &conn,
                &create_scope,
                ctx.subject_tenant_id(),
                NewQueueRow {
                    platform_id,
                    run: owned,
                    run_kind: run.target.kind(),
                    source: run.source,
                    exclusive: run.resolved_exclusive,
                    decision,
                },
            )
            .await
        {
            Ok(row) => row,
            Err(error) => {
                if decision == AdmissionDecision::Dispatch {
                    self.give_back_lease(ctx, platform_id, run.id).await;
                }
                return Err(error);
            }
        };

        info!(
            run_id = %run.id,
            queue_id = %row.id,
            %platform_id,
            exclusive = run.resolved_exclusive,
            occupancy = ?occupancy,
            queued,
            planned = ?planned,
            decision = ?decision,
            "run admitted",
        );

        let admission = match decision {
            AdmissionDecision::Dispatch => Admission::Dispatch { queue_id: row.id },
            AdmissionDecision::Queue => {
                // A queued run occupies no cluster capacity — it has not started
                // and may never start — so the slot goes back here rather than
                // travelling with the outcome.
                slot.release();
                Admission::Queued { queue_id: row.id }
            }
        };

        Ok(Admitted { admission, slot })
    }

    /// The bypass seam: an unmetered slot and [`Admission::Unqueued`].
    ///
    /// Nothing is read, nothing is locked, nothing is written, and — unlike
    /// every other outcome of [`Self::admit`] — **no cluster-capacity
    /// reservation is taken**. `enforce_global_cap` is deliberately not called.
    ///
    /// # Why the cap is skipped too, and not only the queue
    ///
    /// Because the source system skips it. Its collect job calls
    /// `ArgoService::submit_workflow` directly
    /// (`manager/src/services/collect.rs:128-149`), so it never enters
    /// `run_dispatcher::launch` and therefore never reaches
    /// `enforce_global_cap` (`run_dispatcher.rs:81`) — the same call site whose
    /// ordering `Self::admit` documents at length. Reserving here would be
    /// *stricter* than legacy, which sounds harmless and is not: it would let a
    /// cluster at `max_concurrent_runs` answer `ConcurrencyLimit` to the hourly
    /// collection, which is precisely the starvation the bypass exists to
    /// prevent (`manager/src/services/argo.rs:369-372`).
    ///
    /// The cost is bounded and stated rather than hidden: a collect run is a
    /// `pytest --collect-only` pass that executes no tests, and the trigger
    /// launches at most one per repository per cycle
    /// (`collect.rs:157-179`, `run_collect_cycle`).
    ///
    /// The slot is built from the struct literal, as [`GlobalCapGate::reserve`]
    /// builds the disabled-cap one, so [`CapSlot`] still has no constructor
    /// reachable from `service::launch` — see `Admitter::bypass`'s doc for why
    /// that matters.
    async fn bypass(&self, _ctx: &SecurityContext) -> Result<Admitted, DomainError> {
        Ok(Admitted {
            admission: Admission::Unqueued,
            slot: CapSlot { outstanding: None },
        })
    }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
pub(in crate::domain::service) mod tests;
