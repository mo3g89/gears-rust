//! Object-safe client trait for inter-gear consumption via `ClientHub`.

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::errors::QaRunsError;
use crate::models::{
    LaunchOutcome, LaunchRequest, NewSchedule, QueueEntry, Run, RunResult, RunTestResult, Schedule,
    ScheduleNotificationSettings,
};

/// Object-safe client for the qa-runs gear (Version 1).
///
/// Registered in `ClientHub`:
/// ```ignore
/// let runs = hub.get::<dyn QaRunsClientV1>()?;
/// ```
///
/// Primary consumer: qa-insights' auto-rerun (DESIGN §3.4 — the one back-edge
/// in the subsystem, taken deliberately through this public contract).
#[async_trait]
pub trait QaRunsClientV1: Send + Sync {
    // ==================== Runs ====================

    /// The single run-creation path. Manual, CI, scheduled, and auto-rerun
    /// launches all enter here, which is what keeps scheduled and manual runs
    /// indistinguishable downstream (`cpt-cf-qa-fr-runs-schedules`).
    ///
    /// Returns `LaunchOutcome::Started` or `::Queued`. The third outcome —
    /// rejected by a limit — is an `Err` carrying the limit that was hit
    /// (`resource_exhausted`), because there is no run to return.
    async fn launch(
        &self,
        ctx: &SecurityContext,
        req: LaunchRequest,
    ) -> Result<LaunchOutcome, QaRunsError>;

    async fn get_run(&self, ctx: &SecurityContext, id: Uuid) -> Result<Run, QaRunsError>;

    /// Runs, newest first. `limit` is mandatory: `qa_runs` grows strictly
    /// faster than the queue and never drains, so an unbounded inter-gear call
    /// would materialize every run ever executed. REST paging and `OData`
    /// filtering are separate and unaffected.
    async fn list_runs(&self, ctx: &SecurityContext, limit: u32) -> Result<Vec<Run>, QaRunsError>;

    async fn get_run_result(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<RunResult, QaRunsError>;

    /// Runs finished at or after `since`, **oldest first**, capped at `limit`.
    ///
    /// Oldest-first is load-bearing for the only caller: qa-insights'
    /// reconciler advances a watermark as it consumes the page, so a
    /// newest-first page would let it skip past a gap it never filled.
    /// `limit` is mandatory for the same reason [`list_runs`](Self::list_runs)'
    /// is, and the implementation clamps it again.
    ///
    /// # Why this exists at all, and why there is no legacy citation for it
    ///
    /// The source system has **no counterpart**: its analytics reads the same
    /// database the run path writes, straight through `sqlx` against the shared
    /// pool (`manager/src/routes/analytics.rs:959-1008`, `:2438-2458` -
    /// `FROM test_results t JOIN run_results r ON t.run_id = r.id`, executed
    /// against `state.db`). This method pair exists **because** the gear split
    /// forbids that, not because there was an API to port. Do not go looking
    /// for the legacy endpoint; there is none.
    ///
    /// # Both bounds are half-open at one end only
    ///
    /// `finished_at >= since`, inclusive, and a run whose `finished_at` is
    /// `NULL` - anything not yet terminal - is never returned. An inclusive
    /// lower bound can re-deliver the run sitting exactly on the watermark;
    /// re-delivering one run is the cheap failure, and the reconciler's
    /// backfill is idempotent. An exclusive bound would drop a run that
    /// finished in the same clock tick as the watermark, which is the
    /// expensive one.
    async fn list_runs_finished_since(
        &self,
        ctx: &SecurityContext,
        since: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<Run>, QaRunsError>;

    /// Every per-test row qa-runs holds for one run.
    ///
    /// Unbounded by design: the row count is bounded by the run's own test
    /// count, which the launch path already caps. Callers that want a page want
    /// the REST collection instead - and note that this is deliberately **not**
    /// exposed over HTTP, because "every per-test row for this run" would
    /// duplicate that collection with none of its paging.
    ///
    /// # Errors
    ///
    /// `not_found` when the run does not exist *or* is not visible to `ctx` -
    /// the two are indistinguishable everywhere in this gear, which is what
    /// closes the cross-tenant existence oracle.
    async fn list_run_test_results(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<Vec<RunTestResult>, QaRunsError>;

    /// Cancel a queued or executing run. Idempotent on an already-terminal run.
    async fn cancel_run(&self, ctx: &SecurityContext, id: Uuid) -> Result<Run, QaRunsError>;

    /// Re-run a completed run with its original parameters, re-validated at
    /// re-run time (`cpt-cf-qa-fr-runs-cancel-rerun`).
    ///
    /// Exclusivity is inherited **upward only**: a run recorded exclusive
    /// re-runs exclusive, while a run recorded parallel is re-resolved from
    /// the current tiers, so a test marked destructive since the original run
    /// correctly becomes exclusive (guide lines 205-208;
    /// `manager/src/routes/runs.rs:958-967`).
    async fn rerun(&self, ctx: &SecurityContext, id: Uuid) -> Result<LaunchOutcome, QaRunsError>;

    // ==================== Queue ====================

    /// Queue rows, newest first, all states. `environment_id` filters to one
    /// platform — the reliable call, since positions and blockers are computed
    /// over the returned window (guide lines 179-184).
    async fn list_queue(
        &self,
        ctx: &SecurityContext,
        environment_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<QueueEntry>, QaRunsError>;

    /// Drop a `queued` row before it starts. Fails `Aborted` when the row is
    /// no longer `queued` — a `dispatching`/`running` row holds a claim an
    /// in-flight execution depends on, and dropping it would let a new run be
    /// admitted beside an exclusive one
    /// (`manager/src/services/run_queue.rs:403-421`). Cancel a started run
    /// through [`cancel_run`](Self::cancel_run) instead.
    async fn cancel_queued(&self, ctx: &SecurityContext, queue_id: Uuid)
    -> Result<(), QaRunsError>;

    /// Start a queued row now, ignoring what occupies the platform —
    /// including an in-flight exclusive run. Deliberately does **not**
    /// override `max_concurrent_runs`: overriding a platform is a testing
    /// decision an operator may want, while overriding cluster capacity can
    /// wedge the whole execution plane for everyone (guide lines 116-120).
    async fn force_start_queued(
        &self,
        ctx: &SecurityContext,
        queue_id: Uuid,
    ) -> Result<Run, QaRunsError>;

    // ==================== Schedules (Phase B) ====================

    async fn list_schedules(&self, ctx: &SecurityContext) -> Result<Vec<Schedule>, QaRunsError>;

    async fn get_schedule(&self, ctx: &SecurityContext, id: Uuid) -> Result<Schedule, QaRunsError>;

    async fn create_schedule(
        &self,
        ctx: &SecurityContext,
        new: NewSchedule,
    ) -> Result<Schedule, QaRunsError>;

    async fn update_schedule(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        new: NewSchedule,
    ) -> Result<Schedule, QaRunsError>;

    async fn delete_schedule(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), QaRunsError>;

    /// Update the three notification settings on a schedule (D9).
    ///
    /// **Edits those three fields and nothing else**, which is the behaviour
    /// legacy's own endpoint is written around — it rebuilds the whole
    /// `CronWorkflow` and carries every other field forward by hand, with the
    /// `exclusive` pin called out in a comment
    /// (`manager/src/routes/schedules.rs:854-856`). Here it is three columns in
    /// an `UPDATE`, so nothing else is even in the statement.
    ///
    /// Deliberately **not** three more fields on [`NewSchedule`]: see
    /// [`ScheduleNotificationSettings`]. A full replace through
    /// [`update_schedule`](Self::update_schedule) therefore leaves the
    /// notification settings alone rather than clearing them.
    ///
    /// # Errors
    ///
    /// `invalid_argument` for an event name outside
    /// [`SLACK_NOTIFICATION_EVENTS`](crate::SLACK_NOTIFICATION_EVENTS) or a
    /// channel wider than the column; `not_found` for a schedule that does not
    /// exist **or** is not visible to `ctx` — indistinguishable, as everywhere
    /// else in this gear.
    async fn update_schedule_notifications(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        settings: ScheduleNotificationSettings,
    ) -> Result<Schedule, QaRunsError>;
}
