//! Schedule CRUD, and the tick that turns a due cron occurrence into a run.
//!
//! # What makes firing exactly-once, stated before anything else
//!
//! `cpt-cf-qa-nfr-scheduler-exactly-once` is discharged by
//! `idx_qa_schedule_ticks_claim` — a unique index on
//! `(tenant_id, schedule_id, due_at)` — and by nothing in this file.
//! [`SchedulesRepository::claim_tick`] is an `INSERT` against it: every replica
//! may evaluate every schedule, compute the same `due_at`
//! (`domain::cron::next_due` is a pure function of the stored expression, the
//! stored cursor and `now`), and attempt the claim; one commits and the rest
//! take a unique violation and stop.
//!
//! Two consequences this module is written around:
//!
//! * **`Ok(None)` from a claim is the normal path.** It is what every
//!   non-winning replica sees, on every tick, and what a failover produces. It
//!   is logged at DEBUG and is never an error.
//! * **Leadership is defence in depth, not the guarantee.** `gear.rs` runs this
//!   tick under [`crate::infra::leader::LeaderElector`], which reduces
//!   duplicated work; it does not reduce duplicated *runs*, because the claim
//!   already did. That ordering is what makes the multi-instance tests in
//!   `schedules_tests` writable at all: they drive two services against one
//!   store with no elector anywhere, which is the worst case the elector is
//!   supposed to make rarer.
//!
//! # The claim is durable, so a failed launch is never retried
//!
//! A won claim is **not** rolled back when the launch that follows it fails.
//! The failure is written to the tick row and the pass moves on. Retrying would
//! mean a second attempt at a suite the guide calls destructive
//! (`../testrunner/docs/guides/exclusive-runs-and-the-queue.md`, the exclusive
//! runs section) for one due time, which is precisely the outcome exactly-once
//! exists to prevent. The source system takes the same posture in its own
//! corner: a JIRA auto-rerun rejected with `429` "is dropped permanently and is
//! not retried" (guide lines 225-227).
//!
//! **What that costs, said plainly: a due time whose launch failed produces no
//! run, ever.** Not at the next tick, not at the next occurrence of the same
//! minute. The operator's recourse is to launch manually, and the record they
//! have is the tick row's `error` column plus the WARN this module writes.
//! [`Self::list_ticks`] is the read path onto that column — one schedule's
//! fire history, `run_id`/`error` intact, newest `due_at` first — see
//! `domain::repos::schedules_repo`, which closes the same gap on the
//! repository side.
//!
//! # A pass can be stalled by a launch that is not its own
//!
//! Stated beside the no-retry cost because it is the other thing an operator
//! debugging "my schedule fired late" needs, and nothing else in this crate says
//! it. A fire goes through `LaunchService::launch`, which reaches
//! `service::admission`'s per-platform mutex — **shared with inline REST
//! launches** — and inside that lock does a database read and a cross-gear
//! `lease_occupancy` call with **no timeout**. This pass is sequential and
//! `gear.rs` runs it with `MissedTickBehavior::Delay`, so a REST launch holding
//! that lock against a hung qa-environments stalls not just the schedule behind
//! it but every schedule in the fleet, and defers the following pass too.
//!
//! **A `tokio::time::timeout` here would be worse, which is why there is not
//! one.** The claim is already committed by the time the launch begins, so a
//! launch abandoned on a timeout that had in fact succeeded server-side would be
//! recorded as failed and never retried — a run that exists and that this gear
//! believes does not. [`MAX_FIRES_PER_TICK`] bounds how much of one pass a
//! single platform can consume; it does not bound the stall.
//!
//! # One creation path
//!
//! A fire calls the same [`LaunchService::launch`] a REST launch and a re-run
//! call, with the schedule's stored choice delivered as the **launch** tier of
//! exclusivity — see [`crate::domain::exclusivity::Tiers::launch`], which
//! records that a schedule's choice *is* that tier and so the two can never
//! disagree. `cpt-cf-qa-fr-runs-schedules` requires the shared path;
//! [`super::AppServices::new`] is what makes it structural, by cloning the one
//! `LaunchService` in the process into this service rather than building a
//! second.
//!
//! # The tenant travels with the schedule, and never comes from anywhere else
//!
//! The enumeration runs under `system_actor::for_schedule_tick` — nil tenant,
//! cross-tenant, **reads only** — and [`SchedulesRepository::list_enabled`]
//! returns each schedule paired with its own `tenant_id` for exactly this
//! reason. Every read and write that follows is issued under
//! `system_actor::for_schedule_fire`, minted from *that pair's* tenant.
//! [`Fire`] is the shape that keeps it so: it is the only value this module's
//! per-schedule work takes, it carries a [`TenantBound`] rather than a `Uuid`,
//! and it is built in one place, so there is no second tenant in scope to reach
//! for by mistake.
//!
//! The ownership token [`claim_tick`](SchedulesRepository::claim_tick) demands
//! is minted under the **fire** scope, not the enumeration scope, which is the
//! one thing that makes it mean what `domain::repos::OwnedScheduleId`'s doc
//! wants it to mean: that type carries no tenant, so a token resolved under a
//! multi-tenant scope can be spent under a different tenant's write scope.
//! Resolving under the same single-tenant scope the write uses closes that,
//! and it costs one scoped read per due schedule.
//!
//! # Skipped occurrences leave no record
//!
//! `domain::cron::next_due` answers with the most recent occurrence at or before
//! `now` and never back-fills, so an outage's missed due times are simply not
//! fired. Nothing here records them, no event marks them, and no query can
//! recover them.

use std::sync::{Arc, Mutex, PoisonError};

use authz_resolver_sdk::PolicyEnforcer;
use qa_runs_sdk::{
    Exclusivity, LaunchRequest, NewSchedule, RunSource, SLACK_NOTIFICATION_EVENTS, Schedule,
    ScheduleNotificationSettings,
};
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::launch::LaunchService;
use super::{DbProvider, actions, resources};
use crate::domain::cron;
use crate::domain::error::DomainError;
use crate::domain::repos::{
    OwnedScheduleId, RunsRepository, ScheduleTickRow, SchedulesRepository, Windowed,
};
use crate::domain::system_actor::{self, TenantBound};

/// How many schedules one pass will actually fire.
///
/// **The enumeration is deliberately uncapped and this is not.** A read window
/// over `SchedulesRepository::list_enabled` would silently drop schedules from a
/// pass, and that repository's own doc gives the reason it must not: a schedule
/// skipped by a window is a fire that never happens, with nothing to retry it
/// until the next occurrence. A cap on *fires* is the opposite shape, and the
/// property that makes it safe is stated here rather than assumed:
///
/// **A schedule this pass does not reach keeps its cursor unmoved and writes no
/// claim, so `domain::cron::next_due` returns the same `due_at` on the next
/// pass and it claims cleanly.** Nothing is skipped; it is deferred by one tick
/// interval. That is what a read window cannot offer, because a row a window
/// never returned is a row the tick never learned was due.
///
/// # Why a bound at all
///
/// Fires are sequential and each is expensive — a scoped resolve, a claim, then
/// a whole `LaunchService::launch` with its platform read, catalog reads and
/// admission. The realistic trigger is cron alignment: `0 0 * * *` across a
/// fleet makes every schedule due in the same pass. Unbounded, that pass runs
/// for as long as the fleet is large, holding `service::admission`'s
/// per-platform lock repeatedly while it does.
///
/// # Value, and what it does not fix
///
/// Twenty, read off the shipped `queue_max_depth` default: that is how many
/// **queued** runs one (scope, platform) holds before admission answers
/// [`DomainError::QueueFull`], so a pass that fires far more than this against
/// one saturated platform is doing work that mostly cannot succeed. **It is a
/// constant, not a function of that knob** — a deployment that raises the depth
/// does not raise this.
///
/// **That is a rule of thumb and not a ceiling on useful work**, so it is not
/// stated as one: a run that dispatches instead of queueing leaves no queue row
/// for `queued_depth` to count, and a schedule with no `environment_id` is
/// admitted `Unqueued` before the depth check is reached at all
/// (`service::admission`). Either kind can exceed twenty in a pass and succeed
/// every time. The number is chosen for the case that hurts, not derived from
/// a bound that holds.
///
/// **The `QueueFull` exposure is reduced and not removed, and it is worse for a
/// schedule than for a person.** A `QueueFull` lands in the launch's `Err` arm,
/// is recorded on the tick row, and is **never retried** — so an occurrence lost
/// that way produces no run, ever. A manual launch answered 429 has a human who
/// can try again; a scheduled one has nobody. Tracked as a follow-up at the plan
/// level rather than solved here, because the fix is a retry policy and a retry
/// after a committed claim is the thing exactly-once forbids.
///
/// # The starvation this does have, and where the fix actually lives
///
/// `list_enabled` orders by `id`, so a pass always evaluates the lowest ids
/// first. A fleet with more than this many schedules due *every* pass — an
/// `* * * * *` expression on more than twenty schedules against a 60 s tick —
/// drains the low ids and never reaches the tail. Ordinary alignment drains in
/// a few passes, because a fired schedule stops being due; a permanently
/// over-subscribed fleet does not.
///
/// The remedy is a rotating cursor like `MAX_CLAIM_SCAN`'s, but **not** at
/// `SchedulesRepository::list_enabled`'s own cap alone — that cap
/// (`MAX_SCHEDULE_SCAN`, 1,000) exists to bound a pathological fleet size, and
/// a fleet under it, which is every fleet this bug was filed against, never
/// truncates that read, so a cursor advanced only on *that* truncation would
/// sit at `None` forever and fire the same twenty every pass — the starvation,
/// unfixed. What actually has to rotate is *this constant's own* cap: `Self`
/// tracks, inside the loop below, the id of the schedule at which `fires`
/// reached [`MAX_FIRES_PER_TICK`] (`fire_scan_cursor`, advanced by
/// [`Self::advance_fire_scan_cursor`]), and resumes there next pass — regardless
/// of whether `list_enabled`'s own window was truncated. Only a pass that
/// evaluates its *entire* window without exhausting the fire budget, on a
/// `list_enabled` read that was not itself truncated, wraps back to `None`
/// (see [`Self::advance_fire_scan_cursor`]): that is the one case where
/// nothing was left unvisited.
const MAX_FIRES_PER_TICK: u32 = 20;

/// Everything [`ScheduleService`] needs.
///
/// A struct rather than a positional constructor, for the reason
/// [`super::dispatch::DispatchDeps`] gives: several fields are `Arc<_>` of a
/// trait object and are mutually assignable, so a transposition between them is
/// not always a type error.
pub struct ScheduleDeps<S, R: RunsRepository> {
    pub db: Arc<DbProvider>,
    pub schedules: Arc<S>,
    /// **The launch service, not a builder for one.** See this module's header:
    /// the one-creation-path requirement is discharged by sharing the instance.
    pub launch: Arc<LaunchService<R>>,
    pub policy_enforcer: PolicyEnforcer,
}

/// What one pass of [`ScheduleService::fire_due_schedules`] did.
///
/// Returned rather than only logged so the ticker can report it and the tests
/// can assert on it without reading log output. Infallible by construction, like
/// [`super::dispatch::TickReport`]: a pass that propagated would take the ticker
/// with it, and a scheduler that has silently stopped is indistinguishable from
/// a fleet with nothing due.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScheduleTickReport {
    /// Enabled schedules this pass looked at, including any it refused to
    /// touch. Zero with a non-zero [`Self::failed`] means the enumeration itself
    /// did not run.
    pub evaluated: u32,
    /// Due times that were claimed and produced a run.
    pub fired: u32,
    /// Due times another instance had already claimed. **Not a failure** — see
    /// this module's header.
    pub lost: u32,
    /// Work this pass could not carry through: a refused schedule row, a denied
    /// scope, a repository error, or a launch that failed after its claim was
    /// won. An enumeration that never ran counts once here.
    pub failed: u32,
    /// Due schedules this pass left for the next one, having reached
    /// [`MAX_FIRES_PER_TICK`].
    ///
    /// **Not a failure and not a skip.** A deferred schedule wrote no claim and
    /// kept its cursor, so the next pass computes the same `due_at` and fires
    /// it; see that constant for why that is true and for the one case where it
    /// is not.
    pub deferred: u32,
}

/// What one pass of [`ScheduleService::check_schedule_targets`] found.
///
/// Infallible by construction, for the same reason [`ScheduleTickReport`]
/// gives: a pass that propagated would take the ticker with it, and a check
/// that has silently stopped running is indistinguishable from a fleet with
/// nothing wrong.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReferentialCheckReport {
    /// Enabled schedules this pass looked at.
    pub evaluated: u32,
    /// Schedules whose target no longer resolves, each recorded as a new
    /// `qa_schedule_ticks` row via
    /// [`crate::domain::repos::SchedulesRepository::record_referential_check`].
    pub dangling: u32,
    /// Work this pass could not carry through: a nil-tenant row, a refused
    /// enumeration, a refused per-schedule resolve, or a repository error
    /// while writing a finding. An enumeration that never ran counts once
    /// here.
    pub failed: u32,
}

/// One schedule the tick has decided to act on, with the tenant every read and
/// write about it must be bound to.
///
/// [`TenantBound`] rather than a `Uuid`, so the nil-tenant enumeration identity
/// cannot reach a write: that type's only constructor refuses nil, the refusal
/// happens once in [`outstanding`], and every scoped call the fire then makes
/// takes its context from this one value. Same shape and same reason as
/// `super::watch::WatchTarget`.
///
/// Not `#[domain_model]`: it never leaves this module and models nothing the
/// domain has a name for — it is the loop's own carrier.
struct Fire<'a> {
    schedule: &'a Schedule,
    tenant: TenantBound,
    /// The occurrence being fired, which is the claim's unique key.
    due_at: OffsetDateTime,
}

/// Schedule CRUD, and the firing tick.
pub struct ScheduleService<S, R: RunsRepository> {
    db: Arc<DbProvider>,
    schedules: Arc<S>,
    launch: Arc<LaunchService<R>>,
    policy_enforcer: PolicyEnforcer,
    /// What this process writes into `qa_schedule_ticks.claimed_by`.
    ///
    /// **Diagnostic only.** Nothing reads it to make a decision — the decision
    /// was made by the unique index — and the migration says so at the column.
    /// It is minted once per service, so it identifies the process rather than
    /// the pass, which is what makes it useful when two replicas' claims are
    /// interleaved in one table.
    claimed_by: String,
    /// Where the *next* pass should resume `list_enabled` from — see this
    /// module's doc on `MAX_FIRES_PER_TICK` for why this is not simply
    /// threaded from `list_enabled`'s own `Windowed::truncated`. `None`
    /// starts from the beginning of the id-ordered fleet.
    ///
    /// Per-process and never persisted, matching `DispatchService`'s scan
    /// cursors: losing it on a restart costs one pass reverting to the head
    /// of the fleet, not correctness — the exactly-once claim is what
    /// prevents a double fire, not this cursor.
    fire_scan_cursor: Mutex<Option<Uuid>>,
}

impl<S, R> ScheduleService<S, R>
where
    S: SchedulesRepository,
    R: RunsRepository,
{
    pub fn new(deps: ScheduleDeps<S, R>) -> Self {
        Self {
            db: deps.db,
            schedules: deps.schedules,
            launch: deps.launch,
            policy_enforcer: deps.policy_enforcer,
            claimed_by: format!("qa-runs/{}", Uuid::new_v4()),
            fire_scan_cursor: Mutex::new(None),
        }
    }

    /// A scope over `qa.schedule`, derived fresh for every repository call.
    ///
    /// One resource type, because `qa_schedules` and `qa_schedule_ticks` are one
    /// — see [`resources::SCHEDULE`]. Never hoisted: a scope compiled for one
    /// action must not be reused for the next, and deriving per call is what
    /// stops an added call inheriting one.
    async fn scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
        resource_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .policy_enforcer
            .access_scope(ctx, &resources::SCHEDULE, action, resource_id)
            .await?)
    }

    /// Put a payload into the form it will be stored in, before it is checked.
    ///
    /// # The stored form must be the validated form
    ///
    /// [`Self::validate`] refuses a name that is empty **after trimming**, so
    /// without this a name of `"nightly "` passes the check and is then written
    /// with its trailing space. `idx_qa_schedules_tenant_name` is on the raw
    /// column, so the tenant ends up with two schedules whose names render
    /// identically everywhere a human looks, and the 409 that should have
    /// stopped the second one never fires. The mismatch between the checked form
    /// and the stored form is the whole defect; normalising here removes it.
    ///
    /// # Here and not at the REST boundary, which is where it was first put
    ///
    /// `api::rest::dto` is not a choke point: `QaRunsLocalClient` hands an
    /// `sdk::NewSchedule` from an in-process caller straight to [`Self::create`]
    /// and never passes through a DTO at all. Trimming there closed the defect
    /// for HTTP callers and left it open for every cross-gear one — which is
    /// worse than not fixing it, because the commit said it was fixed. This is
    /// the one function both entry points share.
    ///
    /// The boundary still *measures* the trimmed length, so a padded name is not
    /// refused for exceeding a column width it will not occupy; measuring and
    /// normalising are different jobs and only the second one belongs to a
    /// single owner.
    fn normalize(new: &mut NewSchedule) {
        let trimmed = new.name.trim();
        if trimmed.len() != new.name.len() {
            new.name = trimmed.to_owned();
        }
    }

    /// Reject a schedule payload before it costs a round trip.
    ///
    /// Runs after [`Self::normalize`], so `new.name` is already the string that
    /// will be stored.
    ///
    /// # Both checks are at **create and update**, not only at fire time
    ///
    /// An expression that cannot be parsed would otherwise be stored happily and
    /// then fail on every evaluation, forever, in a background pass whose only
    /// output is a log line — a schedule that silently never fires. Rejecting it
    /// at the write is what turns that into a 400 the operator sees while they
    /// still have the expression in front of them.
    ///
    /// The parsed value is discarded: `next_due` re-parses the stored string on
    /// every evaluation, deliberately (`domain::cron`), so caching it here would
    /// be a second representation that could disagree with the column.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] for a name that is empty or whitespace, and
    /// [`DomainError::InvalidCron`] — **not** a `Validation` on a `cron` field —
    /// for an expression `domain::cron::parse_cron` refuses. That variant exists
    /// precisely because the same refusal happens at evaluation time, where
    /// there is no request field to name; `crate::domain::error::DomainError::InvalidCron`
    /// carries the argument.
    fn validate(new: &NewSchedule) -> Result<(), DomainError> {
        if new.name.trim().is_empty() {
            return Err(DomainError::Validation {
                field: "name".to_owned(),
                message: "a schedule name must not be empty".to_owned(),
            });
        }
        // A collect run bypasses admission (`service::launch`;
        // `manager/src/services/argo.rs:369-372`), so a schedule holding one
        // would fire admission-bypassing launches on a cron — the same
        // starvation vector `LaunchRunReq::into_domain` and
        // `service::runs::replay` refuse, but automated and recurring.
        //
        // Parity: the source system's collection is an hourly poller
        // (`manager/src/services/collect.rs:157-179`, `run_collect_cycle`) and a
        // plain function, never a `CronWorkflow`, so no schedule of its can
        // carry one. `qa_schedules.target_collect_url` exists anyway, because
        // this table shares the run's target codec and the alternative is a
        // `target_to_columns` output the schedule writer silently discards
        // (`m20260818_000006_collect_target`). Representable, and refused here:
        // the column keeps the codec honest, this check keeps the behaviour
        // legacy's.
        if matches!(new.target.kind(), qa_runs_sdk::RunKind::Collect) {
            return Err(DomainError::Validation {
                field: "target.kind".to_owned(),
                message: "a collect run bypasses admission and cannot be scheduled; the                           collect trigger owns its cadence"
                    .to_owned(),
            });
        }
        cron::parse_cron(&new.cron)?;
        Ok(())
    }

    /// The width of `qa_schedules.slack_channel`, in bytes.
    ///
    /// `VARCHAR(255)`, per `m20260818_000007_schedule_notifications` (folded into `migrations::m20260813_000003_initial` by the docs squash). Repeated
    /// here for the reason `api::rest::dto`'s own width constants are repeated:
    /// on Postgres an over-long value raises `22001`, which surfaces as an
    /// opaque 500 naming no field, and `SQLite` does not enforce `VARCHAR`
    /// widths at all — so no test in this crate's default tier would catch it.
    const MAX_SLACK_CHANNEL_LEN: usize = 255;

    /// Reject notification settings before they cost a round trip.
    ///
    /// # Why the width check is **here** and not in `api::rest::dto`
    ///
    /// Every other column width in this gear is enforced at the REST boundary,
    /// and that module's own doc admits what it costs: *"`QaRunsLocalClient`
    /// hands an `sdk::NewSchedule` from an in-process caller straight to
    /// [the service] and never passes through a DTO at all"*. For the schedule
    /// name that leniency is bounded — the in-process caller is this gear's own
    /// test support. For **these** settings the in-process caller is the point:
    /// D9 exists so that qa-insights can read and (in principle) drive them over
    /// the SDK. A check only an HTTP caller met would be a check the primary
    /// caller skips.
    ///
    /// # Why the event vocabulary is closed
    ///
    /// Legacy's form deserializes straight into `ScheduledRunNotificationEvent`
    /// (`manager/src/models.rs:291-299`), so an unrecognised event name is a 422
    /// there — it is not stored. Storing one here would be a subscription that
    /// silently never fires, because the routing core in qa-insights (Task 36)
    /// can only act on names it knows. The accepted set is
    /// [`SLACK_NOTIFICATION_EVENTS`], which is in the SDK precisely so that both
    /// gears read one list.
    ///
    /// Case-sensitive, and matching the **serialized** spellings: `InProgress`
    /// is not an event name, `in_progress` is. See that constant.
    ///
    /// Duplicates and ordering are left alone. Legacy does not normalise them
    /// either, and a set that reordered an operator's list would be a second
    /// difference between what they typed and what they read back.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming `slack_channel` or
    /// `slack_events`.
    fn validate_notifications(settings: &ScheduleNotificationSettings) -> Result<(), DomainError> {
        if let Some(channel) = settings.slack_channel.as_deref()
            && channel.len() > Self::MAX_SLACK_CHANNEL_LEN
        {
            return Err(DomainError::Validation {
                field: "slack_channel".to_owned(),
                message: format!(
                    "is {} bytes; the maximum is {}",
                    channel.len(),
                    Self::MAX_SLACK_CHANNEL_LEN
                ),
            });
        }
        for event in &settings.slack_events {
            if !SLACK_NOTIFICATION_EVENTS.contains(&event.as_str()) {
                return Err(DomainError::Validation {
                    field: "slack_events".to_owned(),
                    message: format!(
                        "'{event}' is not a notification event; must be one of {}",
                        SLACK_NOTIFICATION_EVENTS.join(", ")
                    ),
                });
            }
        }
        Ok(())
    }
}

/// The operator-facing half.
///
/// # Two production callers, and the `#[allow(dead_code)]` is gone
///
/// Task 19 shipped this block behind a scoped allow, because the only thing
/// reaching it was `schedules_tests`. Both callers it was waiting for now exist:
/// `api::rest::handlers::schedules` over HTTP, and `domain::local_client`'s
/// `QaRunsClientV1` impl in process. Removing the attribute is what proves it —
/// a method here that no caller reached would fail `-D warnings` rather than
/// sitting behind a permission to be unused.
impl<S, R> ScheduleService<S, R>
where
    S: SchedulesRepository,
    R: RunsRepository,
{
    /// Store a new schedule under the caller's tenant.
    ///
    /// # The target is resolved before it is persisted
    ///
    /// A cron expression that cannot be parsed is refused here rather than
    /// left to fail forever in the firing tick's own background pass
    /// (`Self::validate`'s own doc). A `plan_path`/`repo_id`/`environment_id`
    /// that does not resolve is exactly the same shape of mistake, and until
    /// this check existed nothing caught it either — see
    /// [`LaunchService::resolve_target_exists`], which this calls with the
    /// caller's own `ctx` so the check runs under the same tenant the write
    /// will.
    ///
    /// # Errors
    ///
    /// As [`Self::validate`]; [`DomainError::Validation`] naming the field
    /// when the target does not resolve; [`DomainError::ScheduleNameExists`]
    /// when the name is taken within the tenant; [`DomainError::Forbidden`]
    /// when the policy denies; [`DomainError::Database`] on a persistence
    /// failure.
    pub async fn create(
        &self,
        ctx: &SecurityContext,
        mut new: NewSchedule,
    ) -> Result<Schedule, DomainError> {
        Self::normalize(&mut new);
        Self::validate(&new)?;
        self.launch
            .resolve_target_exists(ctx, &new.target, new.environment_id, new.branch.as_deref())
            .await?;
        let scope = self.scope(ctx, actions::CREATE, None).await?;
        let conn = self.db.conn()?;
        self.schedules
            .create(&conn, &scope, ctx.subject_tenant_id(), new)
            .await
    }

    /// Read one schedule.
    ///
    /// # Errors
    ///
    /// [`DomainError::ScheduleNotFound`] for a schedule that does not exist
    /// **or** is not visible in the caller's scope — the two are
    /// indistinguishable, which is the cross-tenant existence oracle this gear
    /// closes everywhere else too.
    pub async fn get(&self, ctx: &SecurityContext, id: Uuid) -> Result<Schedule, DomainError> {
        let scope = self.scope(ctx, actions::GET, Some(id)).await?;
        let conn = self.db.conn()?;
        self.schedules
            .get(&conn, &scope, id)
            .await?
            .ok_or(DomainError::ScheduleNotFound { id })
    }

    /// Every schedule the caller can see, by name.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the policy denies, [`DomainError::Database`]
    /// on a query failure.
    pub async fn list(&self, ctx: &SecurityContext) -> Result<Vec<Schedule>, DomainError> {
        let scope = self.scope(ctx, actions::LIST, None).await?;
        let conn = self.db.conn()?;
        self.schedules.list(&conn, &scope).await
    }

    /// One schedule's fire history — see [`SchedulesRepository::list_ticks`].
    /// Not every row returned is a fire: a
    /// `claimed_by == REFERENTIAL_CHECK_CLAIMED_BY` row is a referential
    /// check, not a claim — see that trait method's own doc.
    ///
    /// Two repository calls under one scope, deliberately, matching
    /// `RunsService::test_results`'s own `resolve_owned` + read shape: the
    /// `get` is what turns "no ticks" and "no such schedule" into distinct
    /// answers, since [`SchedulesRepository::list_ticks`] alone would answer
    /// an empty `Vec` for both.
    ///
    /// # Errors
    ///
    /// [`DomainError::ScheduleNotFound`] when the schedule does not exist or
    /// is not visible in the caller's scope.
    pub async fn list_ticks(
        &self,
        ctx: &SecurityContext,
        schedule_id: Uuid,
    ) -> Result<Vec<ScheduleTickRow>, DomainError> {
        let scope = self.scope(ctx, actions::GET, Some(schedule_id)).await?;
        let conn = self.db.conn()?;
        self.schedules
            .get(&conn, &scope, schedule_id)
            .await?
            .ok_or(DomainError::ScheduleNotFound { id: schedule_id })?;
        self.schedules.list_ticks(&conn, &scope, schedule_id).await
    }

    /// Replace every caller-decidable field of a schedule.
    ///
    /// # `enabled` survives an edit by construction
    ///
    /// It is a field of [`NewSchedule`], so an edit states it like every other
    /// field and a round trip through read-edit-write preserves it. The source
    /// system cannot do that: it edits a schedule by **deleting and recreating**
    /// the `CronWorkflow`, then re-suspending it by hand if the old one was
    /// suspended, with an error message that has to tell the operator the
    /// schedule is now running when the restore fails
    /// (`manager/src/routes/schedules.rs:708-733`). There is no delete here and
    /// nothing to restore, so that whole failure mode does not port.
    ///
    /// `last_fired_tick` is likewise untouched — the repository does not expose
    /// it to this method at all, so an operator editing a cron expression cannot
    /// rewind the cursor and re-fire the past.
    ///
    /// # Errors
    ///
    /// As [`Self::validate`] and [`Self::create`], plus
    /// [`DomainError::ScheduleNotFound`] when no schedule in scope matched.
    pub async fn update(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        mut new: NewSchedule,
    ) -> Result<Schedule, DomainError> {
        Self::normalize(&mut new);
        Self::validate(&new)?;
        self.launch
            .resolve_target_exists(ctx, &new.target, new.environment_id, new.branch.as_deref())
            .await?;
        let scope = self.scope(ctx, actions::UPDATE, Some(id)).await?;
        let conn = self.db.conn()?;
        self.schedules
            .update(&conn, &scope, id, new)
            .await?
            .ok_or(DomainError::ScheduleNotFound { id })
    }

    /// Edit the three Slack notification settings, and nothing else (D9).
    ///
    /// # Legacy is a delete-and-recreate; this is an `UPDATE`, and nothing is
    /// lost by that
    ///
    /// `manager/src/routes/schedules.rs::api_update_notifications` (the handler
    /// at `schedules.rs:833`) deletes the `CronWorkflow` and recreates it from a
    /// carried-forward `CreateScheduleForm`. Its own doc comment gives the
    /// reason and the reason is entirely about annotation storage: the values
    /// that take effect at trigger time are baked into the object's embedded
    /// trigger script, so patching the annotation left the script stale and the
    /// edited settings silently never applied. Recreating regenerates the
    /// script.
    ///
    /// qa-runs has no derived artifact to regenerate. A schedule is a row, these
    /// settings are three of its columns, and the firing path reads the row — so
    /// the port is a three-column `UPDATE`. The delete-and-recreate would buy
    /// nothing and would import legacy's disclosed failure mode, in which a
    /// recreate that fails after the delete succeeds leaves the schedule
    /// permanently gone with no rollback.
    ///
    /// What **is** behaviour in that handler is its carry-forward of every other
    /// field, `exclusive` called out by name
    /// (`manager/src/routes/schedules.rs:854-856`). That is preserved
    /// structurally: `SchedulesRepository::update_notifications` puts three
    /// columns in the `SET` list, so no other field is in the statement.
    ///
    /// # This is not a second way past the collect guard
    ///
    /// [`Self::validate`] refuses `RunKind::Collect` on create and on update.
    /// This method does not re-run it, and does not need to: its payload is
    /// [`ScheduleNotificationSettings`], which has no target and no kind, and the
    /// repository leaves `run_kind` and every `target_*` column `NotSet`. A
    /// schedule's kind cannot be reached from here at all, so there is nothing to
    /// re-validate and nothing to sneak past. Pinned by
    /// [`schedules_tests::notification_settings_cannot_smuggle_a_collect_target_past_the_guard`].
    ///
    /// Scoped under [`actions::UPDATE`] rather than an action of its own: it is
    /// an edit of an existing schedule's fields, which is exactly the authority
    /// that constant names.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] as [`Self::validate_notifications`];
    /// [`DomainError::ScheduleNotFound`] when no schedule in scope matched —
    /// absent and another tenant's being indistinguishable, as on every other
    /// read and write here; [`DomainError::Forbidden`] when the policy denies;
    /// [`DomainError::Database`] on a persistence failure.
    pub async fn update_notifications(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        settings: ScheduleNotificationSettings,
    ) -> Result<Schedule, DomainError> {
        Self::validate_notifications(&settings)?;
        let scope = self.scope(ctx, actions::UPDATE, Some(id)).await?;
        let conn = self.db.conn()?;
        self.schedules
            .update_notifications(&conn, &scope, id, settings)
            .await?
            .ok_or(DomainError::ScheduleNotFound { id })
    }

    /// Delete a schedule and, by cascade, every tick row recording what it
    /// fired.
    ///
    /// # Errors
    ///
    /// [`DomainError::ScheduleNotFound`] when no schedule in scope matched.
    pub async fn delete(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        let scope = self.scope(ctx, actions::DELETE, Some(id)).await?;
        let conn = self.db.conn()?;
        if self.schedules.delete(&conn, &scope, id).await? {
            Ok(())
        } else {
            Err(DomainError::ScheduleNotFound { id })
        }
    }
}

impl<S, R> ScheduleService<S, R>
where
    S: SchedulesRepository,
    R: RunsRepository + 'static,
{
    /// One scheduler pass: fire every schedule whose cron says a due time is
    /// outstanding.
    ///
    /// Infallible, for the reason [`ScheduleTickReport`] gives.
    pub async fn fire_due_schedules(&self) -> ScheduleTickReport {
        self.fire_due_schedules_at(OffsetDateTime::now_utc()).await
    }

    /// [`Self::fire_due_schedules`] against a supplied instant.
    ///
    /// **`now` is a parameter for the reason `domain::cron` is a pure module:**
    /// exactly-once firing cannot be asserted against a wall clock, and every
    /// test in `schedules_tests` that names a due time would otherwise be a
    /// race against the minute boundary. The public entry point above is the
    /// only thing that reads a clock.
    async fn fire_due_schedules_at(&self, now: OffsetDateTime) -> ScheduleTickReport {
        let mut report = ScheduleTickReport::default();

        // Nil tenant, cross-tenant, and reads only. Every write below is issued
        // under a *different* context, bound to the row's own tenant.
        let enumeration = system_actor::for_schedule_tick();
        let cursor = *self
            .fire_scan_cursor
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let candidates = match self.enabled_schedules(&enumeration, cursor).await {
            Ok(candidates) => candidates,
            Err(error) => {
                warn!(
                    %error,
                    "the schedule tick could not enumerate schedules; nothing fires this pass. \
                     This enumeration elevates through domain::elevated and never asks the PDP, \
                     so this is a database or repository error, not a policy denial",
                );
                report.failed += 1;
                return report;
            }
        };

        let mut fires = 0u32;
        // The id at which this pass's fire budget ran out — set exactly once,
        // the moment `fires` reaches `MAX_FIRES_PER_TICK`. See
        // `Self::advance_fire_scan_cursor` for why this, and not
        // `candidates.truncated`, is what the next pass resumes from.
        let mut budget_exhausted_at = None;
        for (schedule, tenant_id) in &candidates.rows {
            report.evaluated += 1;
            let Some((tenant, due_at)) = outstanding(schedule, *tenant_id, now, &mut report)
            else {
                continue;
            };

            // Counted before the attempt, not after: a lost claim and a failed
            // launch both cost a pass most of what a successful fire costs, so
            // a cap that only counted successes would not bound anything on the
            // pass where everything goes wrong.
            if fires >= MAX_FIRES_PER_TICK {
                report.deferred += 1;
                continue;
            }
            fires += 1;
            if fires == MAX_FIRES_PER_TICK {
                budget_exhausted_at = Some(schedule.id);
            }

            self.fire(
                &Fire {
                    schedule,
                    tenant,
                    due_at,
                },
                &mut report,
            )
            .await;
        }

        self.advance_fire_scan_cursor(&candidates, budget_exhausted_at);
        report_deferrals(&report);
        report
    }

    /// Where the *next* pass's [`SchedulesRepository::list_enabled`] should
    /// resume from — see this module's doc on `MAX_FIRES_PER_TICK`.
    ///
    /// `budget_exhausted_at` — the id at which this pass's fire budget ran
    /// out — takes priority whenever it is `Some`: that is a stronger signal
    /// than [`Windowed::truncated`], because a fleet under `MAX_SCHEDULE_SCAN`
    /// never truncates that read, yet can still exhaust `MAX_FIRES_PER_TICK`
    /// every single pass. Only when the budget was never exhausted does
    /// `truncated` matter: it means `list_enabled`'s own cap cut the read
    /// short, so the pass must resume past what it saw even though it had
    /// fire budget to spare. Neither holds: the pass visited every enabled
    /// schedule with room left over, so the next one starts over at the head
    /// of the fleet.
    fn advance_fire_scan_cursor(
        &self,
        candidates: &Windowed<(Schedule, Uuid)>,
        budget_exhausted_at: Option<Uuid>,
    ) {
        let next = budget_exhausted_at.or_else(|| {
            candidates
                .truncated
                .then(|| candidates.rows.last().map(|(schedule, _)| schedule.id))
                .flatten()
        });
        *self
            .fire_scan_cursor
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = next;
    }

    /// The enumeration step, split out so the tick body has one failure arm
    /// rather than a nested match.
    async fn enabled_schedules(
        &self,
        // Kept, unused, so the caller's audit-logging `system_actor::for_schedule_tick`
        // construction still reads as feeding this read.
        _ctx: &SecurityContext,
        after: Option<Uuid>,
    ) -> Result<Windowed<(Schedule, Uuid)>, DomainError> {
        // Nil-tenant enumeration: elevated here rather than authorized. See
        // `domain::elevated` for why, and for why every write below is issued
        // under a *different*, tenant-bound context.
        let scope = crate::domain::elevated::enumeration_scope();
        let conn = self.db.conn()?;
        self.schedules.list_enabled(&conn, &scope, after).await
    }

    /// One background pass: re-check every enabled schedule's target against
    /// qa-catalog and qa-environments, and record a synthetic
    /// `qa_schedule_ticks` row for every one that has gone dangling since it
    /// was written or last edited — see
    /// [`crate::domain::repos::SchedulesRepository::record_referential_check`].
    ///
    /// # Why this exists beside write-time validation
    ///
    /// `create`/`update` reject a dangling target before it is ever
    /// persisted (`Self::create`'s own doc). That cannot catch a reference
    /// that goes dangling **afterwards** — a plan, repository or environment
    /// deleted out from under an already-stored schedule — and until this
    /// pass existed nothing did: the schedule would simply fail, silently,
    /// the next time it actually fired, which for a nightly schedule can be a
    /// full day away.
    ///
    /// Unlike [`Self::fire_due_schedules`] this drains every page
    /// [`crate::domain::repos::SchedulesRepository::list_enabled`] has to
    /// offer in one call, rather than resuming from a persistent cursor: a
    /// referential check costs one scoped read and one cross-gear probe per
    /// schedule, not a whole admission-and-launch, so there is no
    /// `MAX_FIRES_PER_TICK`-shaped budget to protect.
    pub async fn check_schedule_targets(&self) -> ReferentialCheckReport {
        let mut report = ReferentialCheckReport::default();
        let enumeration = system_actor::for_schedule_tick();
        let mut after = None;
        loop {
            let candidates = match self.enabled_schedules(&enumeration, after).await {
                Ok(candidates) => candidates,
                Err(error) => {
                    warn!(
                        %error,
                        "the referential check could not enumerate schedules; nothing is \
                         checked this pass",
                    );
                    report.failed += 1;
                    return report;
                }
            };
            let truncated = candidates.truncated;
            let mut last_id = None;
            for (schedule, tenant_id) in &candidates.rows {
                report.evaluated += 1;
                last_id = Some(schedule.id);
                self.check_one_target(schedule, *tenant_id, &mut report)
                    .await;
            }
            if !truncated {
                break;
            }
            // An empty page that still claims truncation cannot be resumed —
            // stop rather than loop on nothing.
            let Some(next) = last_id else { break };
            after = Some(next);
        }
        report
    }

    /// One schedule's referential check, folded into `report`.
    ///
    /// Split into [`Self::dangling_target`] (the read) and
    /// [`Self::record_dangling`] (the write) rather than one long body: a
    /// single function covering the nil-tenant skip, the resolve, and every
    /// failure arm of a four-step write reads as one undifferentiated block
    /// and trips `clippy::cognitive_complexity`, which is the metric's own
    /// way of saying a reader cannot hold it in one pass either.
    async fn check_one_target(
        &self,
        schedule: &Schedule,
        tenant_id: Uuid,
        report: &mut ReferentialCheckReport,
    ) {
        let Some((tenant, error)) = self.dangling_target(schedule, tenant_id, report).await else {
            return;
        };
        report.dangling += 1;
        self.record_dangling(tenant, schedule, &error, report)
            .await;
    }

    /// Step 1: does `schedule`'s target still resolve? `None` covers three
    /// cases folded into one return so the caller has a single branch: a nil
    /// tenant (skipped, and counted as failed), a target that still
    /// resolves (nothing to do), and — implicitly, by not being reached —
    /// the dangling case, which alone returns `Some`.
    async fn dangling_target(
        &self,
        schedule: &Schedule,
        tenant_id: Uuid,
        report: &mut ReferentialCheckReport,
    ) -> Option<(TenantBound, DomainError)> {
        // Same refusal as `outstanding`'s, and for the same reason: nil is
        // the platform-root sentinel, not a tenant, so a corrupt row is
        // skipped rather than checked under the platform-root identity.
        let Some(tenant) = TenantBound::new(tenant_id) else {
            warn!(
                schedule_id = %schedule.id,
                schedule_name = %schedule.name,
                "a schedule carries a nil tenant id; its target cannot be checked under the \
                 platform-root identity",
            );
            report.failed += 1;
            return None;
        };
        let ctx = system_actor::for_schedule_fire(tenant);

        let error = match self
            .launch
            .resolve_target_exists(
                &ctx,
                &schedule.target,
                schedule.environment_id,
                schedule.branch.as_deref(),
            )
            .await
        {
            Ok(()) => return None,
            Err(error) => error,
        };

        warn!(
            schedule_id = %schedule.id,
            schedule_name = %schedule.name,
            %error,
            "a schedule's target no longer resolves; recording a referential-check tick",
        );
        Some((tenant, error))
    }

    /// Step 2, past the point [`Self::dangling_target`] already found a
    /// problem: resolve the schedule under its own tenant again — the
    /// enumeration scope cannot mint the write token — and write the
    /// finding.
    async fn record_dangling(
        &self,
        tenant: TenantBound,
        schedule: &Schedule,
        error: &DomainError,
        report: &mut ReferentialCheckReport,
    ) {
        let ctx = system_actor::for_schedule_fire(tenant);
        if let Err(write_error) = self
            .write_referential_check(&ctx, tenant, schedule.id, error)
            .await
        {
            warn!(
                schedule_id = %schedule.id,
                %write_error,
                "a schedule's target does not resolve, but the finding could not be written \
                 (resolving the schedule under its own tenant, compiling a scope, acquiring a \
                 connection, and the insert itself can each be the cause); it will be \
                 re-detected on the next pass",
            );
            report.failed += 1;
        }
    }

    /// The write half of [`Self::record_dangling`], collapsed to one `?`
    /// chain: resolve the schedule under its own tenant again (the
    /// enumeration scope cannot mint the write token), compile a scope, and
    /// insert the finding.
    async fn write_referential_check(
        &self,
        ctx: &SecurityContext,
        tenant: TenantBound,
        schedule_id: Uuid,
        error: &DomainError,
    ) -> Result<(), DomainError> {
        let owned = self.resolve_owned(ctx, schedule_id).await?;
        let scope = self.scope(ctx, actions::CHECK, Some(schedule_id)).await?;
        let conn = self.db.conn()?;
        self.schedules
            .record_referential_check(
                &conn,
                &scope,
                tenant.get(),
                owned,
                OffsetDateTime::now_utc(),
                // The disclosable half of the error — the same rule
                // `record_outcome` applies to a failed launch's text — so a
                // `Database`/`Environments` cause never lands another
                // system's raw text in this column.
                &error.recorded_text(),
            )
            .await
    }

    /// Claim one due time and, if this instance won it, launch.
    ///
    /// # The order is the contract
    ///
    /// 1. Resolve the schedule under the **fire** scope, minting the ownership
    ///    token `claim_tick` demands. Under the enumeration scope the token
    ///    would attest a different tenant's visibility than the write uses; see
    ///    this module's header.
    /// 2. Claim. `Ok(None)` — somebody else holds it — is the normal path.
    /// 3. Launch, through the shared [`LaunchService`].
    /// 4. Record the outcome on the tick row and advance the cursor.
    ///
    /// **Nothing between steps 2 and 4 is retried, and step 3 in particular is
    /// not.** The claim committed in step 2 is durable precisely so that a
    /// retry cannot produce a second destructive run for one due time.
    async fn fire(&self, fire: &Fire<'_>, report: &mut ScheduleTickReport) {
        let ctx = system_actor::for_schedule_fire(fire.tenant);
        let Some(owned) = self.owned_under_its_tenant(&ctx, fire, report).await else {
            return;
        };
        let Some(tick_id) = self.won_claim(&ctx, fire, owned, report).await else {
            return;
        };
        self.launch_and_record(&ctx, fire, tick_id, report).await;
    }

    /// Step 1: the ownership token, or `None` with the failure already recorded.
    async fn owned_under_its_tenant(
        &self,
        ctx: &SecurityContext,
        fire: &Fire<'_>,
        report: &mut ScheduleTickReport,
    ) -> Option<OwnedScheduleId> {
        match self.resolve_owned(ctx, fire.schedule.id).await {
            Ok(owned) => Some(owned),
            Err(error) => {
                warn!(
                    schedule_id = %fire.schedule.id,
                    tenant_id = %fire.tenant.get(),
                    %error,
                    "could not resolve a due schedule under its own tenant; not firing it",
                );
                report.failed += 1;
                None
            }
        }
    }

    /// Step 2: the claim. `None` covers both the lost race and a genuine
    /// failure, which are counted and logged differently and are the same
    /// instruction to the caller — stop.
    async fn won_claim(
        &self,
        ctx: &SecurityContext,
        fire: &Fire<'_>,
        owned: OwnedScheduleId,
        report: &mut ScheduleTickReport,
    ) -> Option<Uuid> {
        match self.claim(ctx, fire, owned).await {
            Ok(Some(tick_id)) => Some(tick_id),
            Ok(None) => {
                // The exactly-once mechanism working. Every replica that is not
                // the winner lands here, on every occurrence.
                debug!(
                    schedule_id = %fire.schedule.id,
                    due_at = %fire.due_at,
                    "another instance already claimed this due time; nothing to do",
                );
                report.lost += 1;
                None
            }
            Err(error) => {
                warn!(
                    schedule_id = %fire.schedule.id,
                    due_at = %fire.due_at,
                    %error,
                    "claiming a due time failed; the schedule does not fire for it",
                );
                report.failed += 1;
                None
            }
        }
    }

    /// Steps 3 and 4, past the point of no return: the claim is committed, so
    /// whatever happens here is written down and never attempted again.
    async fn launch_and_record(
        &self,
        ctx: &SecurityContext,
        fire: &Fire<'_>,
        tick_id: Uuid,
        report: &mut ScheduleTickReport,
    ) {
        // The one creation path.
        match self
            .launch
            .launch(ctx, launch_request_for(fire.schedule))
            .await
        {
            Ok(outcome) => {
                let run_id = outcome.run_id();
                info!(
                    schedule_id = %fire.schedule.id,
                    schedule_name = %fire.schedule.name,
                    due_at = %fire.due_at,
                    %run_id,
                    "a schedule fired",
                );
                self.settle(ctx, fire, tick_id, Some(run_id), None).await;
                report.fired += 1;
            }
            Err(error) => {
                warn!(
                    schedule_id = %fire.schedule.id,
                    schedule_name = %fire.schedule.name,
                    due_at = %fire.due_at,
                    %error,
                    "a schedule's launch failed; the claim is durable, so this due time will \
                     not be attempted again and no run comes from it",
                );
                // `recorded_text`, because this string is written to a column and
                // read back by a human: a `Database`, an `Environments` or an
                // `ExecutorFailed` cause carries another system's vocabulary and
                // is redacted. The WARN above keeps the full text, which is where
                // it belongs.
                self.settle(ctx, fire, tick_id, None, Some(&error.recorded_text()))
                    .await;
                report.failed += 1;
            }
        }
    }

    /// Resolve the schedule under the writing tenant's own scope.
    async fn resolve_owned(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<OwnedScheduleId, DomainError> {
        let scope = self.scope(ctx, actions::GET, Some(id)).await?;
        let conn = self.db.conn()?;
        self.schedules
            .resolve_owned_schedule(&conn, &scope, id)
            .await
    }

    /// The claim itself.
    ///
    /// **On a plain connection, never inside a caller-owned transaction**, which
    /// [`SchedulesRepository::claim_tick`] requires: a failed statement aborts an
    /// enclosing Postgres transaction, so a lost race inside one would poison
    /// every later statement. It is also what commits the claim before the
    /// launch begins, which is what makes the claim durable across a crash
    /// mid-launch.
    async fn claim(
        &self,
        ctx: &SecurityContext,
        fire: &Fire<'_>,
        owned: OwnedScheduleId,
    ) -> Result<Option<Uuid>, DomainError> {
        let scope = self
            .scope(ctx, actions::FIRE, Some(fire.schedule.id))
            .await?;
        let conn = self.db.conn()?;
        self.schedules
            .claim_tick(
                &conn,
                &scope,
                fire.tenant.get(),
                owned,
                fire.due_at,
                &self.claimed_by,
            )
            .await
    }

    /// Write what the fire produced, and move the fired-through cursor.
    ///
    /// # Both halves run on the failure path too, and the cursor is the reason
    ///
    /// Recording the outcome is obvious. Advancing `last_fired_tick` after a
    /// *failed* launch looks wrong and is not: without it the next pass computes
    /// the same `due_at`, re-attempts the claim it already holds, and reports the
    /// refusal as another instance's win — a stream of misleading DEBUG lines
    /// until the next occurrence, for a due time that is finished.
    ///
    /// **The cursor is not what prevents the re-fire, and must not be read as
    /// such.** The claim row is. Deleting this call would leave the run count
    /// unchanged and only restore the noise, which is exactly why it is
    /// described as noise removal rather than as a guard.
    ///
    /// Neither failure is retried or reported as a failed fire: the run either
    /// exists or does not, and that was settled before this ran.
    async fn settle(
        &self,
        ctx: &SecurityContext,
        fire: &Fire<'_>,
        tick_id: Uuid,
        run_id: Option<Uuid>,
        error: Option<&str>,
    ) {
        self.record_outcome(ctx, fire, tick_id, run_id, error).await;
        self.advance_cursor(ctx, fire).await;
    }

    async fn record_outcome(
        &self,
        ctx: &SecurityContext,
        fire: &Fire<'_>,
        tick_id: Uuid,
        run_id: Option<Uuid>,
        error: Option<&str>,
    ) {
        match self
            .write_tick_outcome(ctx, fire.schedule.id, tick_id, run_id, error)
            .await
        {
            Ok(true) => {}
            Ok(false) => warn!(
                %tick_id,
                schedule_id = %fire.schedule.id,
                "no tick row in scope matched the claim this pass just won",
            ),
            Err(error) => warn!(
                %tick_id,
                schedule_id = %fire.schedule.id,
                %error,
                "could not record what a claimed due time produced; the tick row keeps its \
                 NULLs and nothing reconciles it",
            ),
        }
    }

    /// # The resource id is the **schedule's**, not the tick's
    ///
    /// The scope is compiled for `qa.schedule`, and a tick id is not a
    /// `qa.schedule` id — `domain::repos::schedules_repo` is explicit that a
    /// tick row "is not a resource a caller ever addresses", which is why the
    /// two tables share one resource type in the first place. Passing
    /// `Some(tick_id)` mixed two id namespaces under one declared type: a PDP
    /// that resolves resource ids would be asked about a uuid that exists in
    /// `qa_schedule_ticks` and in no `qa_schedules` row, and would be entitled
    /// to deny. No test in this crate could see it, because the policy doubles
    /// here answer without resolving ids — a production-only denial.
    ///
    /// Narrowness is not lost by the change: `record_tick_outcome` filters on
    /// the tick id independently of the scope, so the statement still touches
    /// exactly one row. What the scope decides is *whose* rows are reachable,
    /// and the schedule is the resource that answers that.
    async fn write_tick_outcome(
        &self,
        ctx: &SecurityContext,
        schedule_id: Uuid,
        tick_id: Uuid,
        run_id: Option<Uuid>,
        error: Option<&str>,
    ) -> Result<bool, DomainError> {
        let scope = self.scope(ctx, actions::FIRE, Some(schedule_id)).await?;
        let conn = self.db.conn()?;
        self.schedules
            .record_tick_outcome(&conn, &scope, tick_id, run_id, error)
            .await
    }

    async fn advance_cursor(&self, ctx: &SecurityContext, fire: &Fire<'_>) {
        // `Ok(false)` cannot distinguish "no row in scope" from "already at or
        // past this due time", and neither is an error or retryable — the
        // repository says so at the method.
        if let Err(error) = self.write_cursor(ctx, fire).await {
            warn!(
                schedule_id = %fire.schedule.id,
                due_at = %fire.due_at,
                %error,
                "could not advance a schedule's fired-through cursor; the claim still \
                 prevents a re-fire, but later passes will recompute this due time and \
                 report the refusal as a lost race",
            );
        }
    }

    async fn write_cursor(
        &self,
        ctx: &SecurityContext,
        fire: &Fire<'_>,
    ) -> Result<bool, DomainError> {
        let scope = self
            .scope(ctx, actions::FIRE, Some(fire.schedule.id))
            .await?;
        let conn = self.db.conn()?;
        self.schedules
            .advance_last_fired_tick(&conn, &scope, fire.schedule.id, fire.due_at)
            .await
    }
}

/// Say so when a pass hit [`MAX_FIRES_PER_TICK`].
///
/// INFO rather than WARN: nothing is lost, and a fleet whose schedules align on
/// the hour is a normal deployment rather than a fault. It earns a line anyway,
/// because the operator-visible symptom — a run starting a tick interval later
/// than its cron says — has no other explanation anywhere in the log.
fn report_deferrals(report: &ScheduleTickReport) {
    if report.deferred > 0 {
        info!(
            deferred = report.deferred,
            fired = report.fired,
            cap = MAX_FIRES_PER_TICK,
            "more schedules were due than one pass fires; the rest keep their cursors and \
             claim the same due time on the next pass",
        );
    }
}

/// Whether this schedule has a due time the tick should act on, and whether it
/// may be acted on at all.
///
/// Pure, and a free function rather than a method, because it touches nothing
/// the service holds: everything it decides comes from the row, the clock
/// instant it was handed, and `domain::cron`.
///
/// `None` means "nothing to do", for one of three reasons — a refused row, an
/// unparseable stored expression, or simply no outstanding occurrence — and the
/// first two are counted as failures and logged. The third is silent, because it
/// is what most schedules answer on most ticks.
fn outstanding(
    schedule: &Schedule,
    tenant_id: Uuid,
    now: OffsetDateTime,
    report: &mut ScheduleTickReport,
) -> Option<(TenantBound, OffsetDateTime)> {
    let Some(tenant) = TenantBound::new(tenant_id) else {
        // Nil is the platform-root sentinel, not a tenant, so this row is
        // corrupt rather than platform-owned. Refusing here is what keeps the
        // enumeration identity out of every write the fire would make.
        warn!(
            schedule_id = %schedule.id,
            schedule_name = %schedule.name,
            "a schedule carries a nil tenant id; it will not fire, because firing it would \
             write under the platform-root identity",
        );
        report.failed += 1;
        return None;
    };

    match cron::next_due(&schedule.cron, schedule.last_fired_tick, now) {
        Ok(due) => due.map(|due_at| (tenant, due_at)),
        Err(error) => {
            // Reachable only for an expression written before this validation
            // existed, or by the validation changing under a stored value;
            // `create` and `update` both refuse one.
            warn!(
                schedule_id = %schedule.id,
                schedule_name = %schedule.name,
                %error,
                "a stored cron expression no longer parses; the schedule cannot fire until \
                 it is rewritten",
            );
            report.failed += 1;
            None
        }
    }
}

/// The launch a schedule asks for.
///
/// A named function rather than a literal inside [`ScheduleService::fire`],
/// because most of these fields are `Option<String>`, `Vec<String>` or
/// `Option<Uuid>` and are mutually assignable: `include_tags`/`exclude_tags`
/// transposed compiles and inverts which tests run.
///
/// Two fields are decisions rather than copies:
///
/// * **`exclusive: Exclusivity::from_option_bool(schedule.exclusive_choice)`**
///   goes into the *launch* tier. A schedule's stored choice is that tier —
///   see [`crate::domain::exclusivity::Tiers::launch`] — so `Shared`
///   suppresses an exclusive `TEST_META` declaration and `Inherit` inherits
///   from `plan.yaml` and then `TEST_META`. Delivering it as anything else
///   would either make a schedule unable to override, or make `auto` mean
///   "parallel". `schedule.exclusive_choice` itself stays `Option<bool>` —
///   it is the stored three-token (`true`/`false`/`auto`) vocabulary, a
///   distinct wire form this task does not touch (see that field's doc).
/// * **`timeout_seconds: None`**, because a schedule has no timeout of its own
///   to override with: `qa_runs_sdk::Schedule` carries no such field. Resolution
///   therefore falls to the plan's `timeout_seconds` and then to the configured
///   default, which is what a manual launch that names no timeout also gets.
fn launch_request_for(schedule: &Schedule) -> LaunchRequest {
    LaunchRequest {
        target: schedule.target.clone(),
        environment_id: schedule.environment_id,
        branch: schedule.branch.clone(),
        include_tags: schedule.include_tags.clone(),
        exclude_tags: schedule.exclude_tags.clone(),
        parameters: schedule.parameters.clone(),
        exclusive: Exclusivity::from_option_bool(schedule.exclusive_choice),
        timeout_seconds: None,
        source: RunSource::Scheduled,
        schedule_id: Some(schedule.id),
    }
}

#[cfg(test)]
#[path = "schedules_tests.rs"]
mod tests;
