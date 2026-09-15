//! Run reads, cancellation, re-run, and the two queue operator actions
//! (`cpt-cf-qa-fr-runs-cancel-rerun`; frozen guide lines 110-124).
//!
//! # Three rules ported from the source system, each with a hazard attached
//!
//! ## Re-run inherits exclusivity **upward only**
//!
//! `run.resolved_exclusive.then_some(true)`, never `Some(run.resolved_exclusive)`.
//! `Some(true)` replays the original decision, which is the point of a re-run. A
//! stored `false` is deliberately **not** pinned: it would land on the *launch*
//! tier, which outranks every other, and would therefore suppress a `TEST_META`
//! or `plan.yaml` declaration added since — relaunching in parallel a test that
//! has been marked destructive in the meantime. Letting `false` fall through
//! re-resolves it from the current tiers, so the only drift is an unnecessary
//! exclusive run (throughput) rather than a corrupted platform.
//!
//! The source system carries that reasoning as a prose comment **three times**
//! (`../testrunner/manager/src/routes/runs.rs:958-967`, `:1008-1018`,
//! `:1071-1081`), once per intent, because it had no type barrier. This port has
//! one: the SDK field is named `resolved_exclusive` precisely so the obvious
//! transcription does not type-check into the wrong thing. Guide lines 205-208
//! state the same rule from the user's side.
//!
//! ## Cancel is two different operations, and the queue's guard is why
//!
//! A `queued` row holds nothing, so cancelling it drops the row and retires the
//! run. A `dispatching`/`running` row holds a claim an in-flight execution
//! depends on, and `QueueRepository::cancel_queued`'s `AND state = 'queued'`
//! predicate is *the safety property*: dropping such a row would let a new run be
//! admitted beside an exclusive one (`run_queue.rs:403-421`). The source system
//! keeps the two apart as separate endpoints — the queue's cancel and
//! `DELETE /api/runs/{name}` (`routes/runs.rs:1092-1106`) — and so does this
//! module. See [`RunsService::stop_execution`] for what a live run's cancel is
//! deliberately **not** allowed to release.
//!
//! `QueueRepository::mark_done` is never reachable from here. Its own doc says
//! why: it is unguarded and *"will terminate a `running` row and release its
//! claim on the platform, which is correct for a reconciler that has just
//! observed the execution end and **wrong** for anything user-facing."*
//!
//! ## Force start's asymmetry is intentional
//!
//! Guide lines 116-120: force start *"starts it now, ignoring what occupies the
//! platform, including an in-flight exclusive run. It does **not** override
//! `max_concurrent_runs`; that still answers `429` and leaves the row queued. The
//! asymmetry is intentional: overriding a platform is a testing decision you may
//! want to make, while overriding cluster capacity can wedge the whole namespace
//! for everyone."*
//!
//! # The repository read this module needs and does not have
//!
//! Both operator actions are addressed by **queue row id** and both need the
//! row's `run_id` — to retire the run behind a cancelled row, and to dispatch the
//! run behind a force-started one. `QueueRepository::row_status` is the by-id
//! read and returns `{environment_id, state}` only; nothing on the trait maps a
//! queue id to a run id directly.
//!
//! Force start does not need one: after `mark_dispatching` the row **is** a
//! claim, so `claims_for_platform` recovers it — the same recovery
//! `service::dispatch::claim_batch` performs, for the same reason.
//!
//! Cancel does, and there the only route is
//! [`QueueRepository::list_for_read`], whose window is clamped to
//! [`MAX_QUEUE_READ_LIMIT`]. **That ceiling is real, it is not bounded by
//! `queue_max_depth`, and it is not hidden.**
//!
//! The tempting reassurance is that `queue_max_depth` defaults to 20, so a
//! platform never has a thousand rows. It is wrong, and stating it would have
//! been this module's contribution to a subsystem that keeps finding exactly that
//! shape of sentence: `list_for_read` returns **every state**, newest first
//! (`:454-483`), so the window holds this platform's `done`, `failed`,
//! `cancelled` and `expired` history as well as its queue. What actually bounds
//! the lookup is *how many rows the platform has enqueued since the row being
//! cancelled* — a busy platform can push a still-`queued` row past 1,000 without
//! its queue ever exceeding a depth of 20.
//!
//! So the failure is reachable, and the behaviour on reaching it is to **refuse
//! rather than half-cancel**: a cancelled row whose run stays `Queued` forever is
//! a lost run, the exact failure `QueueRepository::requeue` was added to prevent.
//! An operator gets an error naming the ceiling instead of a silent orphan.
//!
//! Recorded for Task 16 as a repository-layer follow-up, and it is a real one
//! rather than a nicety: either `RowStatus` gains `run_id`, or the trait gains a
//! by-id row read. Either removes this paragraph entirely.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_environments_sdk::{AcquireOutcome, LeaseMode, QaEnvironmentsClientV1};
use qa_runs_sdk::{
    Exclusivity, LaunchOutcome, LaunchRequest, QueueEntry, QueueState, Run, RunResult, RunSource,
    RunState,
};
use time::OffsetDateTime;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, SecurityContext};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::admission::{AdmissionService, PlatformLocks};
use super::launch::{InlineDispatcher, LaunchService};
use super::{DbProvider, actions, resources};
use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::{ExecutionRef, RunExecutor};
use crate::domain::queue::{PositionInput, assign_positions, describe_blocker, ttl_expires_at};
use crate::domain::repos::{
    ArchivedLog, MAX_QUEUE_READ_LIMIT, QueueRepository, QueueRowRecord, RunLogsRepository,
    RunStatePatch, RunWithResult, RunsRepository, TestResultRow,
};
use crate::domain::state_machine::{can_transition, is_terminal};

/// What a queue-row cancel records on the row.
///
/// The source system's wording verbatim (`routes/run_queue.rs`, `api_cancel`),
/// because it is what an operator already reads in the queue view.
const QUEUE_CANCEL_REASON: &str = "Cancelled from the run queue by an operator";

/// What a run-level cancel records on the run.
const RUN_CANCEL_REASON: &str = "Cancelled by an operator";

/// What it means when the queue row a cancel meant to drop is no longer
/// `queued`.
///
/// Two entry points, two answers, and a named type rather than a `bool` because
/// the two are not interchangeable and a transposed flag would silently turn an
/// operator's `409` into a success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowRequirement {
    /// The operator asked to cancel **this row**, so a row that has started is a
    /// conflict and the whole cancel rolls back.
    Required,
    /// The operator asked to cancel **a run**, and the row is incidental: the
    /// dispatcher claiming it between the read and the write means the claim is
    /// reconciliation's, not that the cancel failed.
    BestEffort,
}

/// Constructor arguments for [`RunsService`].
///
/// A struct rather than a positional constructor, for the reason
/// [`super::admission::AdmissionDeps`] gives: `clippy::too_many_arguments` would
/// need an `#[allow]` either way, and the fields that are `Arc<_>` of a trait
/// object are mutually assignable, so a transposition between them is not always
/// a type error. Named fields make it one.
///
/// **Named, not counted.** Every `Deps` doc in this module used to state how many
/// fields it had and how many were `Arc`s, and the code-quality review found all
/// four of them wrong on arrival — the count is a claim that goes stale the next
/// time a field lands, which is exactly what happened.
pub struct RunsDeps<R: RunsRepository, Q> {
    pub db: Arc<DbProvider>,
    pub runs: Arc<R>,
    pub queue: Arc<Q>,
    pub environments: Arc<dyn QaEnvironmentsClientV1>,
    pub executor: Arc<dyn RunExecutor>,
    /// Where a cancel releases the run's live log channel — see
    /// [`RunsService::retire`].
    pub logs: Arc<dyn super::LogFanout>,
    /// **The real launch service, not a seam.** Re-run's whole contract is that
    /// it is indistinguishable downstream from a fresh launch, so it goes through
    /// the one creation path rather than a scripted stand-in — which is also what
    /// makes it re-validate the replayed parameters, re-read the platform, and
    /// re-resolve the timeout without this module knowing any of those chains.
    pub launch: Arc<LaunchService<R>>,
    /// Force start's submit. The same seam `service::launch` uses, so a
    /// force-started run and an inline-admitted one take an identical path.
    pub dispatcher: Arc<dyn InlineDispatcher>,
    /// Force start's `max_concurrent_runs` gate. Held as the admission service
    /// itself rather than re-derived, because the source system shares exactly
    /// this function between its launch path and its force-start handler
    /// (`routes/run_queue.rs`, `api_force_start` calling
    /// `run_queue::enforce_global_cap`), and two copies of a cap check are two
    /// places for it to drift.
    pub admission: Arc<AdmissionService<R, Q>>,
    /// `queue_ttl_seconds`, needed by [`RunsService::queue_page`] and by
    /// nothing else on this service: `ttl_expires_at` is a derived column of
    /// the queue view, so the read that assembles the view has to know the
    /// setting. It is a bare `u64` rather than the whole `QueueLimits` because
    /// the other two limits are admission's to enforce and holding them here
    /// would invite a second enforcement site.
    pub queue_ttl_seconds: u64,
    /// **The same registry admission and the tick share** — see
    /// [`super::AppServices`]. Force start claims a row inside the platform's
    /// mutex, so a launch being admitted concurrently sees the row stop being
    /// `queued`; a second registry would leave that serialisation to chance.
    pub locks: PlatformLocks,
    pub policy_enforcer: PolicyEnforcer,
}

/// Reads, cancel, re-run, and the queue operator actions.
///
/// # Why the queue operator actions are not their own service
///
/// The alternative a reader reaches for — `cancel_queued` and `force_start` on a
/// `QueueOperatorService`, leaving this file with the run-shaped operations — was
/// considered and declined, and the reason is the one `service::dispatch_spec`
/// gives for **not** splitting the tick: the seam would cut through a critical
/// section rather than along a boundary.
///
/// `cancel_queued` and `cancel` both end in [`Self::retire`], which cancels a
/// queue row and retires its run **in one transaction**. Splitting them puts that
/// transaction on one side of a service boundary and one of its two callers on
/// the other, so the half-cancel this module exists to prevent would become
/// expressible again — the caller across the seam would have to re-derive the
/// pairing, which is how the first version of `cancel_queued` got it wrong.
///
/// `force_start` has the weaker version of the same tie: it shares
/// [`Self::require_queued`] and [`Self::diagnose`] with `cancel_queued`, and those
/// two are what turn "the guarded update matched nothing" into a 404 or a 409
/// consistently across both endpoints.
///
/// What that costs is real and is not hidden: this is a large file with a large
/// dependency struct, and the reads at the top share nothing with the operator
/// actions at the bottom except the service they hang off.
pub struct RunsService<R: RunsRepository, Q> {
    db: Arc<DbProvider>,
    runs: Arc<R>,
    queue: Arc<Q>,
    environments: Arc<dyn QaEnvironmentsClientV1>,
    executor: Arc<dyn RunExecutor>,
    logs: Arc<dyn super::LogFanout>,
    launch: Arc<LaunchService<R>>,
    dispatcher: Arc<dyn InlineDispatcher>,
    admission: Arc<AdmissionService<R, Q>>,
    queue_ttl_seconds: u64,
    locks: PlatformLocks,
    policy_enforcer: PolicyEnforcer,
}

impl<R, Q> RunsService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    pub fn new(deps: RunsDeps<R, Q>) -> Self {
        Self {
            db: deps.db,
            runs: deps.runs,
            queue: deps.queue,
            environments: deps.environments,
            executor: deps.executor,
            logs: deps.logs,
            launch: deps.launch,
            dispatcher: deps.dispatcher,
            admission: deps.admission,
            queue_ttl_seconds: deps.queue_ttl_seconds,
            locks: deps.locks,
            policy_enforcer: deps.policy_enforcer,
        }
    }

    /// The lock registry, for the composition test that proves force start
    /// contends with admission rather than with itself.
    #[cfg(test)]
    pub(in crate::domain::service) fn locks(&self) -> &PlatformLocks {
        &self.locks
    }

    /// The admission service, for the composition test that proves force start
    /// and launch reserve from one cap gate.
    #[cfg(test)]
    pub(in crate::domain::service) fn admission(&self) -> &Arc<AdmissionService<R, Q>> {
        &self.admission
    }

    /// The launch service, for the composition test that proves a re-run goes
    /// through the *same* creation path the container built.
    #[cfg(test)]
    pub(in crate::domain::service) fn launch_service(&self) -> &Arc<LaunchService<R>> {
        &self.launch
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

    /// A fresh `qa.queue_entry` scope, per call — never derived from
    /// [`resources::RUN`].
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

    /// Read a run under the action actually being performed.
    ///
    /// `action` is a parameter rather than a constant because the same read backs
    /// four different operations, and this module's rule is the one
    /// `domain::service` states: the PEP is asked about the action the code is
    /// about to perform. **A cancel is not authorized under `create`** — the
    /// action-widening argument `service::launch` makes for its own transitions
    /// is scoped to a launch recording its own admission outcome, and an operator
    /// cancel is a different request by a different principal.
    async fn read_run(
        &self,
        ctx: &SecurityContext,
        action: &str,
        run_id: Uuid,
    ) -> Result<Run, DomainError> {
        let scope = self.run_scope(ctx, action, Some(run_id)).await?;
        let conn = self.db.conn()?;
        self.runs
            .get(&conn, &scope, run_id)
            .await?
            .ok_or(DomainError::RunNotFound { id: run_id })
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

impl<R, Q> RunsService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// One run.
    ///
    /// # Errors
    ///
    /// [`DomainError::RunNotFound`] for absent and foreign alike — telling them
    /// apart is the cross-tenant existence oracle this gear closes everywhere.
    pub async fn get(&self, ctx: &SecurityContext, run_id: Uuid) -> Result<Run, DomainError> {
        self.read_run(ctx, actions::GET, run_id).await
    }

    /// One page of the runs in scope, newest first, each paired with its
    /// result counters (Task 10 — see [`RunWithResult`]).
    ///
    /// **Bounded, and this is where the obligation `RunsRepository::list`
    /// records is discharged.** That method's doc places it on the API layer —
    /// *"do not expose this method through an endpoint without pagination"* —
    /// and this is the read the endpoint uses instead: the window is the page
    /// size, not the tenant's whole run history.
    ///
    /// # The scope is resolved here and the query is passed through untouched
    ///
    /// The `AccessScope` comes from the policy enforcer before the repository
    /// is called, and `query` is handed down exactly as the caller sent it. The
    /// repository is what composes the two, and it can only do so on a scoped
    /// select — so no `$filter` can widen what this returns, whatever it says.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`], [`DomainError::Validation`] for a query the
    /// field allow-list or the cursor rejects, or [`DomainError::Database`].
    pub async fn list(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<RunWithResult>, DomainError> {
        let scope = self.run_scope(ctx, actions::LIST, None).await?;
        let conn = self.db.conn()?;
        self.runs.list_page(&conn, &scope, query).await
    }

    /// Runs finished at or after `since`, oldest first, at most `limit`.
    ///
    /// qa-insights' reconciler sweep, reached over the SDK. It is a **read of
    /// runs**, so it derives its scope exactly as [`Self::list`] does — the
    /// same `resources::RUN`, the same `actions::LIST`, the same
    /// enforcer — and the repository composes that scope with the watermark
    /// predicate on a select that cannot be unscoped. A sweep is a wider read
    /// than a page of one tenant's history in *time*, never in *tenancy*: a
    /// caller who cannot list runs cannot sweep them either, and one who can
    /// sweeps only their own.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the caller may not list runs, or
    /// [`DomainError::Database`].
    pub async fn list_runs_finished_since(
        &self,
        ctx: &SecurityContext,
        since: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<Run>, DomainError> {
        let scope = self.run_scope(ctx, actions::LIST, None).await?;
        let conn = self.db.conn()?;
        self.runs
            .list_finished_since(&conn, &scope, since, limit)
            .await
    }

    /// One page of the run queue, newest first, optionally one platform's.
    ///
    /// # The three derived fields, and what each costs
    ///
    /// `QueueEntry` carries three values that are not columns, and each is
    /// computed here because each needs something the repository does not have:
    ///
    /// * **`queue_position`** — [`assign_positions`] over *the rows this page
    ///   returned*. That is the guide's own definition and its own caveat: a
    ///   narrow page pushes older `queued` rows out of the window, collapsing
    ///   the positions behind them toward 1. The guide's remedy is the
    ///   platform-filtered call, which is why `environment_id` is a first-class
    ///   parameter here rather than something a caller has to express in
    ///   `OData`.
    /// * **`ttl_expires_at`** — needs [`Self::queue_ttl_seconds`], which is
    ///   configuration the repository has no business knowing.
    /// * **`blocked_by`** — needs the *names* of the runs currently holding each
    ///   platform, which is a second table.
    ///
    /// # `blocked_by` is resolved only for head-of-queue rows
    ///
    /// [`describe_blocker`] ignores its `holders` argument whenever the position
    /// is above 1 — a row with three rows ahead of it is told about those three,
    /// not about the platform. So the holder lookup runs **once per platform
    /// that has a position-1 row in this page**, and not at all otherwise. A
    /// page of two hundred rows across four platforms costs at most four claim
    /// reads plus one run read per claim.
    ///
    /// **A holder whose run cannot be read renders as "waiting for a run that is
    /// still starting", and here that text means something different from what
    /// it means in the source system.** There, a holder with no name is the
    /// ordinary mid-build state, read from a nullable `workflow_name` column.
    /// Here `Run::name` is a `String`, never absent, so the only way to reach
    /// that arm is a **failed read** - a denied scope or a database error. The
    /// wording is kept because it is what an operator sees in the source system
    /// for a visually identical situation, but a reader must not infer from it
    /// that the holder is mid-build.
    ///
    /// It is accepted rather than hidden: the alternative is failing the whole
    /// listing because one blocker's name was unavailable, and the field is
    /// advisory text for a human.
    ///
    /// **`blocked_by`'s holders come from a fresh read, not from the page.**
    /// `queue_position` is computed over the rows this request returned - the
    /// guide's caveat, repeated above - while the holders are read per platform
    /// outside the window. That makes `blocked_by` *more* accurate than the
    /// source system's, which derives both from the returned rows; it also
    /// means the two fields do not share a snapshot, so a holder that finished
    /// between the two reads is named as blocking a row it no longer blocks.
    ///
    /// # Errors
    ///
    /// As [`Self::list`].
    pub async fn queue_page(
        &self,
        ctx: &SecurityContext,
        environment_id: Option<Uuid>,
        query: &ODataQuery,
    ) -> Result<Page<QueueEntry>, DomainError> {
        let scope = self.queue_scope(ctx, actions::LIST, None).await?;
        let conn = self.db.conn()?;
        let page = self
            .queue
            .list_page(&conn, &scope, environment_id, query)
            .await?;

        let inputs: Vec<PositionInput> = page
            .items
            .iter()
            .map(|row| PositionInput {
                id: row.id,
                environment_id: row.environment_id,
                enqueued_at: row.enqueued_at,
                queued: row.state == QueueState::Queued,
            })
            .collect();
        let positions = assign_positions(&inputs);
        let holders = self.holders_for_heads(ctx, &page.items, &positions).await;

        let items = page
            .items
            .into_iter()
            .map(|row| {
                let position = positions.get(&row.id).copied();
                let blocked_by = position.map(|position| {
                    describe_blocker(
                        position,
                        holders.get(&row.environment_id).map_or(&[][..], Vec::as_slice),
                    )
                });
                QueueEntry {
                    id: row.id,
                    run_id: row.run_id,
                    environment_id: row.environment_id,
                    run_kind: row.run_kind,
                    source: row.source,
                    exclusive: row.exclusive,
                    state: row.state,
                    error: row.error,
                    enqueued_at: row.enqueued_at,
                    dispatched_at: row.dispatched_at,
                    finished_at: row.finished_at,
                    queue_position: position,
                    // `None` for anything that is not `queued`, which is also
                    // what `assign_positions` decides - so the two derived
                    // fields agree by construction rather than by two copies of
                    // the same `if`.
                    ttl_expires_at: position
                        .and_then(|_| ttl_expires_at(row.enqueued_at, self.queue_ttl_seconds)),
                    blocked_by,
                }
            })
            .collect();

        Ok(Page {
            items,
            page_info: page.page_info,
        })
    }

    /// The names of the runs holding each platform that has a head-of-queue row
    /// in this page.
    ///
    /// Failures are swallowed into an empty holder list for that platform,
    /// which [`describe_blocker`] renders as "waiting for its turn". A listing
    /// must not fail because one advisory sentence could not be composed - see
    /// [`Self::queue_page`].
    async fn holders_for_heads(
        &self,
        ctx: &SecurityContext,
        rows: &[QueueRowRecord],
        positions: &HashMap<Uuid, u32>,
    ) -> HashMap<Uuid, Vec<Option<String>>> {
        let heads: BTreeSet<Uuid> = rows
            .iter()
            .filter(|row| positions.get(&row.id) == Some(&1))
            .map(|row| row.environment_id)
            .collect();

        let mut holders = HashMap::new();
        for environment_id in heads {
            let claims = match self.claims_for_platform(ctx, environment_id).await {
                Ok(claims) => claims,
                Err(error) => {
                    warn!(
                        %environment_id,
                        %error,
                        "could not read a platform's claims for the queue listing's \
                         blocked_by text; reporting the generic reason instead",
                    );
                    continue;
                }
            };
            let mut names = Vec::with_capacity(claims.len());
            for claim in claims {
                names.push(
                    self.read_run(ctx, actions::GET, claim.run_id)
                        .await
                        .ok()
                        .map(|run| run.name),
                );
            }
            holders.insert(environment_id, names);
        }
        holders
    }

    /// A run's five counters.
    ///
    /// # Errors
    ///
    /// [`DomainError::RunNotFound`], or [`DomainError::CorruptState`] if a
    /// counter is negative.
    pub async fn get_result(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<RunResult, DomainError> {
        let scope = self.run_scope(ctx, actions::GET, Some(run_id)).await?;
        let conn = self.db.conn()?;
        self.runs
            .get_result(&conn, &scope, run_id)
            .await?
            .ok_or(DomainError::RunNotFound { id: run_id })
    }

    /// A run's per-test rows, for the run-detail response.
    ///
    /// # Errors
    ///
    /// [`DomainError::RunNotFound`] when the run is not visible under `ctx`.
    pub async fn test_results(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<Vec<TestResultRow>, DomainError> {
        let scope = self.run_scope(ctx, actions::GET, Some(run_id)).await?;
        let conn = self.db.conn()?;
        let owned = self.runs.resolve_owned(&conn, &scope, run_id).await?;

        let results_scope = self.run_scope(ctx, actions::GET, Some(run_id)).await?;
        let conn = self.db.conn()?;
        self.runs
            .list_test_results(&conn, &results_scope, owned)
            .await
    }
}

// ---------------------------------------------------------------------------
// The archived log
// ---------------------------------------------------------------------------

/// A separate impl block, bound to `RunLogsRepository` **in addition to**
/// `RunsRepository`, rather than folded into the "Reads" block above.
///
/// [`RunsDeps`] and every other method on this service asks only for
/// `R: RunsRepository`, and one caller — `runs_tests.rs`'s `FakeRuns` — is a
/// scripted double that implements exactly that trait and nothing more. Widening
/// the "Reads" block's bound would make every method in it, `get` included,
/// require `RunLogsRepository` too, which `FakeRuns` cannot satisfy without
/// implementing a table it has no reason to know about. A second `impl` block
/// keeps the two repository traits — `get_log` deliberately kept off
/// `RunsRepository`, see that trait's own doc — from leaking into a bound
/// `RunsService`'s other 90% of methods do not need.
///
/// `ConcreteAppServices`'s `OrmRunsRepository` implements both traits already,
/// so this costs production nothing.
impl<R, Q> RunsService<R, Q>
where
    R: RunsRepository + RunLogsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// A finished run's durable log, if `qa_run_logs` has a row for it.
    ///
    /// The **only** route from a handler to `RunLogsRepository::get_log`:
    /// `api::rest::handlers::runs::stream_run_logs` calls this rather than
    /// reaching the repository directly, exactly as [`Self::get`] is the only
    /// route to `RunsRepository::get`. Absent and foreign both answer `Ok(None)`
    /// here — `get_log`'s own scoped query is what makes them indistinguishable,
    /// not a check in this method or in the handler — so a defect in the
    /// handler's own terminal-state read cannot turn into a cross-tenant log
    /// read on its own; this method's scope resolution is a second, independent
    /// gate on the same table.
    ///
    /// # That claim covers **this arm only**, and the qualification matters
    ///
    /// The handler's terminal branch is a `match` with two arms, and only the
    /// one this method serves is independently scoped. The other —
    /// `None => logs.replay(id)` — reads
    /// [`crate::infra::logs::RunLogBroadcaster`]'s process-local retained map,
    /// which is keyed by run id and carries **no tenant scope at all**: it is
    /// infrastructure with no repository and no `SecurityContext`, and its own
    /// type doc says so. So for a run with no archived row, the handler's
    /// `svc.runs.get(&ctx, id)` precheck is the *only* thing standing between a
    /// caller and another tenant's retained tail. A defect skipping that
    /// precheck would be caught on the archive arm and not on the fallback.
    ///
    /// Pre-existing and not introduced with the archive — the fallback arm is
    /// the behaviour that was already there. Recorded here because the
    /// paragraph above reads absolute and is not: the *durable* copy has two
    /// gates, the in-memory one has one.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] from the policy decision point, or
    /// [`DomainError::Database`].
    pub async fn archived_log(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<Option<ArchivedLog>, DomainError> {
        let scope = self.run_scope(ctx, actions::GET, Some(run_id)).await?;
        let conn = self.db.conn()?;
        self.runs.get_log(&conn, &scope, run_id).await
    }
}

// ---------------------------------------------------------------------------
// Cancel
// ---------------------------------------------------------------------------

impl<R, Q> RunsService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// Cancel a run, whatever state it is in.
    ///
    /// # Idempotent on an already-terminal run
    ///
    /// [`is_terminal`] is what makes that true rather than an error: a run that
    /// has already finished, been cancelled, expired or timed out has nothing
    /// left to stop, and answering `409` to an operator clicking Cancel twice
    /// would be a fault report for a correct request. Nothing is written and the
    /// run is returned as it stands.
    ///
    /// # Errors
    ///
    /// [`DomainError::RunNotFound`], [`DomainError::ExecutorFailed`] when a live
    /// execution's cancel could not be delivered (in which case **nothing is
    /// recorded** — see [`Self::stop_execution`]), or
    /// [`DomainError::IllegalTransition`] if the run moved between the read and
    /// the write.
    ///
    /// **One of those two error paths is not clean, and saying so is the point.**
    /// On [`DomainError::IllegalTransition`] for a `Running` run,
    /// [`Self::stop_execution`] has **already** asked the execution plane to stop
    /// — that call comes first, and by design, because a cancel recorded for an
    /// execution that is still running would be a lie. So the operator receives an
    /// error for a run whose execution really was stopped, and the run is left for
    /// the control-plane timeout sweep to reclaim rather than reaching `Canceled`.
    ///
    /// Reversing the order would trade this for the worse failure — recording
    /// `Canceled` and then failing to deliver the cancel — so the window is
    /// accepted rather than closed. A retry of the cancel is safe:
    /// `RunExecutor::cancel` is idempotent and the run is still non-terminal.
    pub async fn cancel(&self, ctx: &SecurityContext, run_id: Uuid) -> Result<Run, DomainError> {
        let run = self.read_run(ctx, actions::CANCEL, run_id).await?;
        if is_terminal(run.state) {
            info!(
                %run_id,
                state = run.state.as_str(),
                "cancel requested for a run that is already terminal; nothing to do",
            );
            return Ok(run);
        }

        // A live execution is stopped through the executor and releases nothing;
        // a run that has not started drops its queue row, which holds nothing.
        //
        // The row is *located* before anything is written and *dropped* in the
        // same transaction as the run's retirement — see [`Self::retire`] on why
        // the two must not be separate writes.
        let row = if matches!(run.state, RunState::Dispatching | RunState::Running) {
            self.stop_execution(&run).await?;
            None
        } else {
            self.cancellable_row(ctx, &run)
                .await
                .map(|queue_id| (queue_id, RowRequirement::BestEffort))
        };

        let finished_at = OffsetDateTime::now_utc();
        self.retire(ctx, &run, row, finished_at, RUN_CANCEL_REASON)
            .await?;
        Ok(Run {
            state: RunState::Canceled,
            finished_at: Some(finished_at),
            error: Some(RUN_CANCEL_REASON.to_owned()),
            ..run
        })
    }

    /// Ask the execution plane to stop, and **release nothing**.
    ///
    /// # What this deliberately does not do, and why it is not an omission
    ///
    /// It does not mark the queue row done and it does not hand back the platform
    /// lease. At this instant the control plane has *asked* for a cancellation
    /// and has not *observed* the execution end — `RunExecutor::cancel` is
    /// documented as fire-and-forget, matching the source system, whose cancel
    /// handler patches the workflow and writes no run state at all
    /// (`routes/runs.rs:1092-1106` -> `argo.rs:1447-1453`). Releasing the claim
    /// now would free the platform while an execution is still winding down on
    /// it, which is precisely what lets a new run start beside an exclusive one.
    ///
    /// Both are released by whoever *observes* the end: `service::ingest`'s
    /// `Finished` branch, or the dispatcher tick's claim reconciliation, which
    /// releases the claim and the lease together once the executor stops listing
    /// the execution. The cost is that a cancelled run's platform stays held for
    /// up to one tick, which is the fail-safe direction.
    ///
    /// # The fail-safe direction on failure
    ///
    /// An undeliverable cancel returns `Err` and records nothing, mirroring
    /// `service::dispatch::reclaim_overdue`: marking a run `Canceled` whose
    /// execution is still running would report a stop that did not happen.
    ///
    /// A run in `Dispatching` with no execution reference has nothing to cancel —
    /// the submit has not happened yet. Its run row is retired here and its queue
    /// row is left `dispatching` for the orphan guard, which is the only pass that
    /// can tell a mid-build row from a lost one.
    async fn stop_execution(&self, run: &Run) -> Result<(), DomainError> {
        let Some(reference) = run.execution_ref.as_deref() else {
            warn!(
                run_id = %run.id,
                state = run.state.as_str(),
                "cancelling a run that has no execution reference yet; its queue row is \
                 left for the orphan guard",
            );
            return Ok(());
        };
        self.executor.cancel(&ExecutionRef::new(reference)).await?;
        info!(
            run_id = %run.id,
            execution_ref = reference,
            "cancellation requested of the execution plane; the claim and the lease are \
             released when the execution is observed to end",
        );
        Ok(())
    }

    /// This run's `queued` row, if it has one — the row a cancel drops.
    ///
    /// `None` for a platformless run, for a row already claimed, and for a run
    /// whose row the TTL sweep already expired. All three are ordinary, so the
    /// caller retires the run either way.
    async fn cancellable_row(&self, ctx: &SecurityContext, run: &Run) -> Option<Uuid> {
        let environment_id = run.environment_id?;
        match self.queue_row_for_run(ctx, environment_id, run.id).await {
            Ok(row) => row.map(|row| row.id),
            Err(error) => {
                warn!(
                    run_id = %run.id,
                    %error,
                    "could not read the queue row of a cancelled run; the TTL sweep owns it",
                );
                None
            }
        }
    }

    /// Whether a queue row that has stopped being `queued` is a conflict or a
    /// shrug.
    ///
    /// The distinction is the difference between the two entry points and it is
    /// named rather than expressed as a `bool`: an operator who asked to cancel a
    /// **row** must be told when that row started, while an operator who asked to
    /// cancel a **run** does not care which of the two happened to the row.
    async fn retire(
        &self,
        ctx: &SecurityContext,
        run: &Run,
        row: Option<(Uuid, RowRequirement)>,
        finished_at: OffsetDateTime,
        reason: &str,
    ) -> Result<(), DomainError> {
        let illegal = || DomainError::IllegalTransition {
            id: run.id,
            state: run.state,
            action: "be cancelled".to_owned(),
        };
        // The mandatory pre-check, asked about the state the repository returned
        // rather than about a literal — and asked **before** anything is written,
        // which is half of what closes the half-cancel below.
        if !can_transition(run.state, RunState::Canceled) {
            return Err(illegal());
        }

        // Both scopes are derived here, outside the transaction: they are policy
        // decision point round trips, and holding a database transaction open
        // across a network call to the PDP would put an unrelated system's latency
        // inside a lock. Fresh per call, and one per resource type.
        let run_scope = self.run_scope(ctx, actions::CANCEL, Some(run.id)).await?;
        let row_scope = match row {
            Some((queue_id, _)) => Some((
                queue_id,
                self.queue_scope(ctx, actions::CANCEL, Some(queue_id))
                    .await?,
            )),
            None => None,
        };

        // Everything the closure touches is **owned**, not borrowed. The
        // transaction runner is `for<'a> FnOnce(&'a DbTx<'a>) -> ... + 'a`, so a
        // captured reference would have to outlive a lifetime the caller chooses;
        // cloning three `Arc`s and two scopes is the price of running two
        // repositories' writes under one transaction.
        let queue = Arc::clone(&self.queue);
        let runs = Arc::clone(&self.runs);
        let reason = reason.to_owned();
        let run_id = run.id;
        let from = run.state;
        let patch = RunStatePatch {
            started_at: None,
            finished_at: Some(finished_at),
            // A constant of this gear's own, about the caller's own run:
            // `DomainError::recorded_text` classifies *errors*, and there is no
            // error here to classify.
            error: Some(reason.clone()),
        };
        let requirement = row.map(|(_, requirement)| requirement);

        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    if let Some((queue_id, scope)) = row_scope {
                        let cancelled = queue.cancel_queued(tx, &scope, queue_id, &reason).await?;
                        if !cancelled && requirement == Some(RowRequirement::Required) {
                            // Rolls the whole thing back, which is the point: the
                            // caller re-reads to name what the row became.
                            return Err(DomainError::QueueRowNotQueued {
                                id: queue_id,
                                state: QueueState::Queued,
                            });
                        }
                        if !cancelled {
                            warn!(
                                %queue_id,
                                "the queue row was no longer queued when the cancel reached \
                                 it; its claim belongs to reconciliation",
                            );
                        }
                    }
                    if runs
                        .update_state(tx, &run_scope, run_id, from, RunState::Canceled, patch)
                        .await?
                    {
                        Ok(())
                    } else {
                        Err(DomainError::IllegalTransition {
                            id: run_id,
                            state: from,
                            action: "be cancelled".to_owned(),
                        })
                    }
                })
            })
            .await?;

        // Only after the pair has committed. `LogSubscription::recv` answers
        // `None` only when the channel is dropped, so without this an SSE handler
        // watching a cancelled run holds its connection open forever.
        self.logs.reap(run.id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Re-run
// ---------------------------------------------------------------------------

impl<R, Q> RunsService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// Launch the stored run again, through the **one** creation path.
    ///
    /// # What going through `LaunchService::launch` buys, rather than costs
    ///
    /// Every obligation the source system discharges by hand at its re-run
    /// endpoint is discharged here by the launch path itself, which is what keeps
    /// a re-run indistinguishable downstream:
    ///
    /// * **the parameters are re-validated** — launch step 1 is
    ///   `params::normalize` then `params::validate`, before any I/O. The source
    ///   system re-checks explicitly and says why: *"cheap defense-in-depth
    ///   against stale/tampered rows and a reserved list that may have grown
    ///   since"* (`routes/runs.rs:920-923`). Here that is not an extra call, it is
    ///   the same first step every launch takes;
    /// * **the platform is re-read, not replayed.** Task 13's ownership check is a
    ///   launch-time snapshot, and a platform deleted or reassigned since would
    ///   otherwise let a re-run take the *global*, non-tenant-partitioned lease on
    ///   it. `launch`'s `resolve` reads the platform first, through the caller's
    ///   own qa-environments client, precisely as the ownership check;
    /// * **the deadline is resolved afresh** by `domain::timeout`'s three chains,
    ///   which are public for exactly this caller — a re-run makes a *new*
    ///   decision, so it reuses the function rather than reading the old row's
    ///   `timeout_at`;
    /// * **the lease's release-on-`Err` obligation** is inherited whole from
    ///   `service::admission::take_lease`, so a committed acquire whose response
    ///   was lost is handed back here as it is on any other launch.
    ///
    /// # Two fields that are **not** replayed
    ///
    /// `source` becomes [`RunSource::Manual`] and `schedule_id` becomes `None`,
    /// matching the source system, which hard-codes `run_source: Some("manual")`
    /// and `schedule_id: None` on every re-run intent: a person re-running a
    /// scheduled run is a manual launch, and attributing it to the schedule would
    /// corrupt that schedule's history.
    ///
    /// # Errors
    ///
    /// [`DomainError::RunNotFound`], [`DomainError::Validation`] when the stored
    /// run carries no recorded branch, and everything
    /// [`LaunchService::launch`] can fail with — notably
    /// [`DomainError::InvalidParameters`] for a replayed parameter set that is no
    /// longer legal, which is the point of re-validating.
    pub async fn rerun(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<LaunchOutcome, DomainError> {
        let run = self.read_run(ctx, actions::RERUN, run_id).await?;
        let request = replay(&run)?;
        info!(
            source_run = %run.id,
            exclusive = ?request.exclusive,
            branch = ?request.branch,
            "re-running a stored run through the launch path",
        );
        self.launch.launch(ctx, request).await
    }
}

/// Rebuild a [`LaunchRequest`] from a stored run.
///
/// A free function so the transcription can be read, and tested, without a
/// service: every field here is a decision, and the two that are easy to get
/// wrong are [`LaunchRequest::exclusive`] and [`LaunchRequest::branch`].
///
/// # A collect run is refused, and this is the **second** REST-reachable path
/// to the admission bypass
///
/// `LaunchRunReq::into_domain` closes `POST /qa/v1/runs`; this closes
/// `POST /qa/v1/runs/{id}/rerun`, which reaches [`LaunchService::launch`]
/// through here and would otherwise hand a caller the same bypass — no
/// `max_concurrent_runs`, no `queue_max_depth`, no 429 — repeatable at will,
/// needing only that one collect run exist, which the hourly cycle guarantees.
///
/// **`replay` itself needed no change for the collect kind**: it clones
/// `run.target`, so a collect run replayed cleanly as a collect run. That it
/// *worked* is exactly the problem — this refusal is the one place a
/// `RunTarget`-agnostic function has to stop being agnostic.
///
/// Parity, not invention: the source system cannot re-run a collect workflow
/// either. Its collect submission is annotated `vhp-tests/run-kind = "plan"`
/// (`manager/src/services/argo.rs:608`, hard-coded on the plan submit path)
/// carrying the **synthetic** plan id `collect-{repo.id}` with an empty
/// `dir_path` (`manager/src/services/collect.rs:93-94`) — a plan that was never
/// written to the plans directory. So `rerun` (`manager/src/routes/runs.rs:903`)
/// takes its plan arm, resolves that id against the plans it can see, and fails.
/// Legacy fails late and by accident; this fails early and says why.
///
/// Re-collecting is itself harmless — that is not the argument. The argument is
/// that the *route to a launch that skips admission* must not be caller-driven.
/// qa-insights re-triggers a collection by calling the trigger again, which is
/// the surface that owns the policy.
///
/// # Errors
///
/// [`DomainError::Validation`] when the run records no branch, or when the run
/// is a collect run.
fn replay(run: &Run) -> Result<LaunchRequest, DomainError> {
    if matches!(run.target.kind(), qa_runs_sdk::RunKind::Collect) {
        return Err(DomainError::Validation {
            field: "target.kind".to_owned(),
            message: format!(
                "run {} is a collect run, which bypasses admission and is re-launched by                  the collect trigger rather than by re-run",
                run.id
            ),
        });
    }

    // The branch the original run actually executed against. The source system
    // takes `source_ref` falling back to `test_version` (`routes/runs.rs:927-933`);
    // this port records one branch label in one column, so there is no fallback
    // to write.
    //
    // **A run with no recorded branch is refused rather than re-resolved.** The
    // source system answers `BadRequest` for a run missing its plan metadata
    // (`routes/runs.rs:910-918`), and while `RunTarget` here is total — it always
    // carries its ids, so "missing plan metadata" has no field-for-field
    // counterpart — the *effect* the refusal protects is exactly this one:
    // letting `None` fall through would re-resolve the branch from the platform
    // and repository defaults as they stand today, so a re-run could silently
    // execute a different branch's files than the run it claims to repeat. That
    // is the failure "reuse the original branch" exists to prevent.
    let branch = run
        .test_version
        .as_deref()
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| DomainError::Validation {
            field: "test_version".to_owned(),
            message: format!(
                "run {} records no branch, so it cannot be re-run against the branch it \
                 originally executed",
                run.id
            ),
        })?
        .to_owned();

    Ok(LaunchRequest {
        target: run.target.clone(),
        environment_id: run.environment_id,
        branch: Some(branch),
        include_tags: run.include_tags.clone(),
        exclude_tags: run.exclude_tags.clone(),
        parameters: run.parameters.clone(),
        // Upward only. See this module's header; `Exclusivity::from_option_bool
        // (Some(run.resolved_exclusive))` -- the direct transcription of the
        // old `Some(run.resolved_exclusive)` -- is the version that type-checks
        // and is wrong: it would pin `Shared` on the launch tier for a run
        // that ran parallel, suppressing a `TEST_META`/`plan.yaml` declaration
        // added since. `Exclusive` when the original ran exclusive, `Inherit`
        // (not `Shared`) otherwise, so a since-marked-destructive test is
        // still re-resolved rather than replayed parallel.
        exclusive: if run.resolved_exclusive {
            Exclusivity::Exclusive
        } else {
            Exclusivity::Inherit
        },
        // Re-resolved by `domain::timeout`'s three chains inside the launch, not
        // replayed from the old row's absolute `timeout_at`.
        timeout_seconds: None,
        source: RunSource::Manual,
        schedule_id: None,
    })
}

// ---------------------------------------------------------------------------
// Queue operator actions
// ---------------------------------------------------------------------------

impl<R, Q> RunsService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// Drop a queue row that has not started, and retire its run.
    ///
    /// # Errors
    ///
    /// [`DomainError::QueueRowNotFound`] when no such row is visible,
    /// [`DomainError::QueueRowNotQueued`] when it exists but has already left
    /// `queued`, and [`DomainError::Internal`] when the row is beyond
    /// [`MAX_QUEUE_READ_LIMIT`] and its run therefore cannot be retired — see this
    /// module's header on why that refuses rather than half-cancelling.
    pub async fn cancel_queued(
        &self,
        ctx: &SecurityContext,
        queue_id: Uuid,
    ) -> Result<(), DomainError> {
        let status = self.require_queued(ctx, queue_id).await?;
        let row = self
            .queue_row_by_id(ctx, status.environment_id, queue_id)
            .await?;
        // **Read before write.** This used to cancel the row first and read the
        // run afterwards, and the security review executed the consequence: a
        // failure of *any* kind between the two — a foreign run, a dropped
        // connection, a denied scope — left `row = cancelled` and `run = queued`.
        // That run is unrecoverable, not untidy: `list_timeout_candidates` matches
        // `dispatching | running` only, so the control-plane sweep never sees it,
        // and the TTL sweep works on `queued` **rows**, which is the one that was
        // just cancelled. It is the exact sentence this module's header wrote
        // about the case it *did* close.
        let run = self.read_run(ctx, actions::CANCEL, row.run_id).await?;
        if is_terminal(run.state) {
            return self
                .cancel_row_of_finished_run(ctx, queue_id, row.run_id)
                .await;
        }

        let finished_at = OffsetDateTime::now_utc();
        match self
            .retire(
                ctx,
                &run,
                Some((queue_id, RowRequirement::Required)),
                finished_at,
                QUEUE_CANCEL_REASON,
            )
            .await
        {
            Ok(()) => {}
            // The guarded UPDATE inside the transaction is the decision; the
            // pre-check above is advisory, and the state it carried was a guess.
            // Re-read rather than guess again, so the message cannot lie — the
            // dispatcher may have claimed the row, but the TTL sweep may equally
            // have expired it, and those mean the opposite to whoever asked.
            Err(DomainError::QueueRowNotQueued { .. }) => {
                return Err(self.diagnose(ctx, queue_id).await);
            }
            Err(error) => return Err(error),
        }
        info!(%queue_id, run_id = %row.run_id, "queue row cancelled by an operator");
        Ok(())
    }

    /// The row of a run that has already finished: there is nothing to retire, so
    /// the row cancel stands alone and cannot strand anything.
    async fn cancel_row_of_finished_run(
        &self,
        ctx: &SecurityContext,
        queue_id: Uuid,
        run_id: Uuid,
    ) -> Result<(), DomainError> {
        if self.cancel_row(ctx, queue_id).await? {
            info!(%queue_id, %run_id, "queue row of a finished run cancelled");
            return Ok(());
        }
        Err(self.diagnose(ctx, queue_id).await)
    }

    /// Start a queued row now, bypassing the platform's occupancy but **not**
    /// `max_concurrent_runs`.
    ///
    /// Guide lines 116-120: *"The asymmetry is intentional: overriding a platform
    /// is a testing decision you may want to make, while overriding cluster
    /// capacity can wedge the whole namespace for everyone."*
    ///
    /// The cap is enforced **before** the claim, so a `429` leaves the row
    /// untouched and still queued, exactly as the source system orders it
    /// (`routes/run_queue.rs`, `api_force_start`).
    ///
    /// Returns the run that was started.
    ///
    /// # Errors
    ///
    /// [`DomainError::QueueRowNotFound`], [`DomainError::QueueRowNotQueued`],
    /// [`DomainError::ConcurrencyLimit`] for the cap this does not override,
    /// [`DomainError::Internal`] when a claimed row's run id cannot be recovered,
    /// and whatever the submit failed with.
    pub async fn force_start(
        &self,
        ctx: &SecurityContext,
        queue_id: Uuid,
    ) -> Result<Uuid, DomainError> {
        let status = self.require_queued(ctx, queue_id).await?;
        // Cluster capacity first, and before the claim. The slot is handed to the
        // submit below rather than merely bound here: it is what a concurrent
        // launch or force start counts until this run reaches the executor's
        // listing, and `dispatch_inline` taking it is what stops it being dropped
        // before then.
        let capacity = self.admission.enforce_global_cap().await?;

        let claim = self
            .claim_by_force(ctx, queue_id, status.environment_id)
            .await?;
        warn!(
            %queue_id,
            run_id = %claim.run_id,
            environment_id = %status.environment_id,
            exclusive = claim.exclusive,
            "run-queue row FORCE STARTED by an operator; platform occupancy bypassed, \
             max_concurrent_runs still enforced",
        );

        // Dispatch outside the platform lock: this is where the force-sync and
        // the bundle build happen, and holding the mutex across them would
        // serialise every launch on the platform for minutes
        // (`run_queue.rs:569-572`).
        self.dispatcher
            .dispatch_inline(ctx, claim.run_id, Some(queue_id), &capacity)
            .await?;
        Ok(claim.run_id)
    }

    /// Claim the row inside the platform lock and take the lease if it is free.
    ///
    /// # Two things happen inside the mutex, and both have to
    ///
    /// The claim, so a launch being admitted concurrently sees the row stop being
    /// `queued` — the source system says exactly this
    /// (`routes/run_queue.rs`, *"Claim inside the platform lock, so a launch
    /// admitted concurrently sees this row as occupancy the instant it stops
    /// being `queued`"*) — and the lease acquisition, because in this port the
    /// lease *is* occupancy (`service::admission`'s header) and so it is the line
    /// a concurrent launch actually reads.
    ///
    /// # The lease is attempted, and a refusal is overridden
    ///
    /// This is where the guide's "ignoring what occupies the platform" becomes
    /// concrete, and it is not free. The lease is a compare-and-swap in
    /// qa-environments, so an exclusive occupant makes `acquire_lease` answer
    /// `Busy`. Force start proceeds anyway — that is the operator's decision — and
    /// the consequence is stated rather than hidden: **the force-started run then
    /// holds no lease**, so when the original occupant finishes and releases, the
    /// platform reads `Free` while the forced run is still executing on it, and a
    /// later exclusive launch can start beside it. That is the same override the
    /// operator already asked for, extended in time.
    ///
    /// When the lease *is* free the acquisition succeeds and the platform is held
    /// normally, which is the common case and costs nothing.
    ///
    /// Double-submit safety comes from `mark_dispatching`'s
    /// `AND state = 'queued'` predicate, not from the lock: whoever flips the row
    /// wins and everyone else sees `false`.
    async fn claim_by_force(
        &self,
        ctx: &SecurityContext,
        queue_id: Uuid,
        environment_id: Uuid,
    ) -> Result<crate::domain::repos::ClaimRow, DomainError> {
        let lock = self.locks.get(environment_id).await;
        let _guard = lock.lock().await;

        if !self.mark_dispatching(ctx, queue_id).await? {
            return Err(self.diagnose(ctx, queue_id).await);
        }
        // The row is a claim now, so this is where its run id lives — the same
        // recovery `service::dispatch::claim_batch` performs.
        let claim = self
            .claims_for_platform(ctx, environment_id)
            .await?
            .into_iter()
            .find(|claim| claim.id == queue_id)
            .ok_or_else(|| {
                error!(
                    %queue_id,
                    %environment_id,
                    "a force-started row was claimed but its run id could not be recovered; \
                     leaving it for the orphan guard rather than dispatching blind",
                );
                DomainError::Internal(format!(
                    "queue row {queue_id} was claimed but its run id could not be read"
                ))
            })?;

        self.take_lease_or_override(ctx, environment_id, &claim).await;
        Ok(claim)
    }

    /// Take the platform lease for a force-started run, treating a refusal as the
    /// override the operator asked for. See [`Self::claim_by_force`].
    async fn take_lease_or_override(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
        claim: &crate::domain::repos::ClaimRow,
    ) {
        let mode = if claim.exclusive {
            LeaseMode::Exclusive
        } else {
            LeaseMode::Parallel
        };
        match self
            .environments
            .acquire_lease(ctx, environment_id, claim.run_id, mode)
            .await
        {
            Ok(AcquireOutcome::Acquired) => {}
            Ok(AcquireOutcome::Busy { current }) => warn!(
                run_id = %claim.run_id,
                %environment_id,
                current = ?current,
                "force start is proceeding without the platform lease because another run \
                 holds it; this run will not register as occupancy and the platform will \
                 read free once the current holder releases",
            ),
            Err(error) => {
                error!(
                    run_id = %claim.run_id,
                    %environment_id,
                    %error,
                    "could not acquire the platform lease for a force-started run; \
                     releasing in case the acquisition landed and only the response was lost",
                );
                self.give_back_lease(ctx, environment_id, claim.run_id).await;
            }
        }
    }

    /// Hand back a lease this path may have taken but could not confirm.
    ///
    /// The obligation `service::admission::take_lease` discharges, inherited
    /// verbatim: an `acquire_lease` that **commits server-side and then fails to
    /// return** leaves the platform held by a run nothing will ever release, and
    /// no later pass can free it because a lease is not a claim and
    /// reconciliation never sees it. `decide_release` removes only a hold the run
    /// actually has, so this is a no-op when the acquisition never landed — which
    /// is what makes it safe to issue unconditionally on the error path.
    async fn give_back_lease(&self, ctx: &SecurityContext, environment_id: Uuid, run_id: Uuid) {
        if let Err(error) = self
            .environments
            .release_lease(ctx, environment_id, run_id)
            .await
        {
            error!(
                %run_id,
                %environment_id,
                %error,
                "could not release a lease that may have been taken for a force-started \
                 run; the platform will read as busy until it is cleared by hand",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Queue plumbing
// ---------------------------------------------------------------------------

impl<R, Q> RunsService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// The row's status, refusing anything that is not still `queued`.
    ///
    /// **Advisory.** The guarded write that follows is what actually decides; this
    /// exists so an operator acting on a row that has already started is told what
    /// happened instead of being handed the result of a write that matched
    /// nothing. The source system distinguishes the same two answers the same way
    /// (`routes/run_queue.rs`, *"The two are distinguished with a follow-up read
    /// so an operator clicking Cancel on a row that just started is told what
    /// happened rather than being shown 'not found'"*).
    async fn require_queued(
        &self,
        ctx: &SecurityContext,
        queue_id: Uuid,
    ) -> Result<crate::domain::repos::RowStatus, DomainError> {
        let scope = self.queue_scope(ctx, actions::GET, Some(queue_id)).await?;
        let conn = self.db.conn()?;
        let status = self
            .queue
            .row_status(&conn, &scope, queue_id)
            .await?
            .ok_or(DomainError::QueueRowNotFound { id: queue_id })?;
        if status.state == QueueState::Queued {
            Ok(status)
        } else {
            Err(DomainError::QueueRowNotQueued {
                id: queue_id,
                state: status.state,
            })
        }
    }

    /// Name what a row is *now*, after a guarded write matched nothing.
    ///
    /// Read rather than guessed: `mark_dispatching` and `cancel_queued` prove only
    /// "no longer queued", and the dispatcher claiming it, the TTL sweep expiring
    /// it and another operator cancelling it mean opposite things to whoever asked.
    async fn diagnose(&self, ctx: &SecurityContext, queue_id: Uuid) -> DomainError {
        match self.require_queued(ctx, queue_id).await {
            // It says `queued` again — a second race. Report the conflict rather
            // than retrying, so the caller re-reads.
            Ok(status) => DomainError::QueueRowNotQueued {
                id: queue_id,
                state: status.state,
            },
            Err(error) => error,
        }
    }

    async fn cancel_row(&self, ctx: &SecurityContext, queue_id: Uuid) -> Result<bool, DomainError> {
        let scope = self
            .queue_scope(ctx, actions::CANCEL, Some(queue_id))
            .await?;
        let conn = self.db.conn()?;
        self.queue
            .cancel_queued(&conn, &scope, queue_id, QUEUE_CANCEL_REASON)
            .await
    }

    async fn mark_dispatching(
        &self,
        ctx: &SecurityContext,
        queue_id: Uuid,
    ) -> Result<bool, DomainError> {
        let scope = self
            .queue_scope(ctx, actions::FORCE_START, Some(queue_id))
            .await?;
        let conn = self.db.conn()?;
        self.queue.mark_dispatching(&conn, &scope, queue_id).await
    }

    async fn claims_for_platform(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
    ) -> Result<Vec<crate::domain::repos::ClaimRow>, DomainError> {
        let scope = self.queue_scope(ctx, actions::LIST, None).await?;
        let conn = self.db.conn()?;
        self.queue
            .claims_for_platform(&conn, &scope, environment_id)
            .await
    }

    /// One platform's rows, newest first, clamped to [`MAX_QUEUE_READ_LIMIT`].
    ///
    /// The only route from a queue id to a run id on this trait — see the module
    /// header, which states the ceiling and what is done about it.
    async fn queue_window(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
    ) -> Result<Vec<QueueRowRecord>, DomainError> {
        let scope = self.queue_scope(ctx, actions::LIST, None).await?;
        let conn = self.db.conn()?;
        self.queue
            .list_for_read(&conn, &scope, Some(environment_id), MAX_QUEUE_READ_LIMIT)
            .await
    }

    /// The row a cancel is about, refusing when it is outside the window.
    async fn queue_row_by_id(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
        queue_id: Uuid,
    ) -> Result<QueueRowRecord, DomainError> {
        self.queue_window(ctx, environment_id)
            .await?
            .into_iter()
            .find(|row| row.id == queue_id)
            .ok_or_else(|| {
                error!(
                    %queue_id,
                    %environment_id,
                    limit = MAX_QUEUE_READ_LIMIT,
                    "a queue row exists but is outside the readable window, so its run \
                     cannot be retired; refusing rather than cancelling the row alone",
                );
                DomainError::Internal(format!(
                    "queue row {queue_id} is beyond the readable window of \
                     {MAX_QUEUE_READ_LIMIT} rows for its platform"
                ))
            })
    }

    /// This run's queue row, if it has one in the window.
    ///
    /// `Ok(None)` rather than an error when it is absent: a run with no row is the
    /// ordinary case for a platformless launch and for a row the TTL sweep already
    /// expired, and a cancel must not fail on either.
    async fn queue_row_for_run(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
        run_id: Uuid,
    ) -> Result<Option<QueueRowRecord>, DomainError> {
        Ok(self
            .queue_window(ctx, environment_id)
            .await?
            .into_iter()
            .find(|row| row.run_id == run_id && row.state == QueueState::Queued))
    }
}

#[cfg(test)]
#[path = "runs_tests.rs"]
mod tests;
