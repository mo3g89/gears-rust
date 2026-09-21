//! Cron schedules and the exactly-once tick claim.
//!
//! Two tables behind one trait, and that pairing is deliberate rather than
//! incidental: `qa_schedule_ticks` exists only to serialize the firing of
//! `qa_schedules`, has no independent lifecycle, and is reaped by the parent's
//! `ON DELETE CASCADE`. A separate `TicksRepository` would be a trait whose
//! every method took a schedule id.
//!
//! Both tables are therefore governed by one resource type, `qa.schedule` — a
//! tick row is not a resource a caller ever addresses. That does **not** relax
//! `domain::repos`' one-scope-per-resource-type rule: it means the two tables
//! are one resource type, so one scope is the correct number, and the methods
//! below each take exactly one.
//!
//! That name comes from **the plan**, which lists three for this gear — this
//! header shipped crediting `DESIGN.md` §3.7 and that was wrong even then:
//! old §3.7 named *tables*, not resource types, and the document's only
//! `qa.schedule` was the `qa.schedule.fired` event, in old §3.3's Events
//! table. A documentation squash has since removed that table, and current
//! `DESIGN.md` has **zero** occurrences of `qa.schedule` (`grep -c`) under
//! any section number; current §3.7 is `Product SDK, Product Plugins,
//! Connectors`. `domain::service::resources` is where the vocabulary is
//! recorded, and it cites the plan correctly.
//!
//! **The `ResourceType` constant is `domain::service::resources::SCHEDULE`,**
//! and `ScheduleService::scope` is what compiles a scope from it — one
//! `policy_enforcer.access_scope` call per repository call, per this module's
//! one-scope-per-resource-type rule. This repository still takes an
//! `AccessScope` its caller built and calls no `PolicyEnforcer` itself, which
//! is the only half of the original sentence that survived: it shipped saying
//! the constant "does not exist yet" and that "nothing in this task calls a
//! `PolicyEnforcer`", and both stopped being true when the schedule service
//! landed. Neither was re-read afterwards.
//!
//! ## The tick table was write-only; [`SchedulesRepository::list_ticks`] closes half of that
//!
//! Until WS5 Task 1, nothing in this trait read a `qa_schedule_ticks` row back.
//! Two consequences were written down here because both looked like oversights
//! and neither was:
//!
//! * **`error` is recorded and cannot be retrieved.** [`SchedulesRepository::record_tick_outcome`]
//!   writes why a launch failed, and until [`Self::list_ticks`] no method returned
//!   it — a human with a SQL prompt and the log line the caller emits were the
//!   only readers. [`Self::list_ticks`] is that read: one schedule's fire
//!   history, bounded, newest-`due_at`-first, with `run_id`/`error` intact. It
//!   does not add an orphan sweep or any other write — it is a read, and the
//!   claim/outcome mechanics above are unchanged.
//! * **An orphaned claim still cannot be enumerated across schedules.** A claim
//!   won by a process that died before `record_tick_outcome` keeps `run_id` and
//!   `error` NULL forever, and nothing here finds *such rows specifically*
//!   across the fleet. That is not a stuck schedule — see
//!   [`SchedulesRepository::claim_tick`] on why the next occurrence proceeds
//!   regardless — but it is an unreconciled row. [`Self::list_ticks`] makes it
//!   *visible* to a caller who already knows which schedule to ask about; it is
//!   not the orphan sweep, which remains a deferral recorded at the plan level,
//!   not a gap this module closes on its own initiative.

use async_trait::async_trait;
use qa_runs_sdk::{NewSchedule, Schedule, ScheduleNotificationSettings, ScheduleTick};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::Windowed;

/// A schedule id that has been resolved under the caller's own access scope.
///
/// The schedules counterpart of [`OwnedRunId`](super::OwnedRunId), and
/// deliberately the *same* pattern rather than a variant of it — that type
/// carries the full argument and this one does not repeat it. In short:
/// `qa_schedule_ticks.schedule_id` references `qa_schedules(id)` with **no
/// tenant component**, so an insert carrying an attacker-chosen `schedule_id`
/// succeeds if that schedule exists in *any* tenant and fails with a
/// foreign-key violation if it does not, which is a membership test over
/// another tenant's schedule identifiers. Resolving the id under the caller's
/// scope first means the constraint never gets to answer — **when the resolving
/// scope and the writing scope name the same tenant.** When they do not, see
/// the next section; that qualifier is load-bearing and was missing from the
/// first version of this comment.
///
/// # The token carries no tenant, so the index prefix is still load-bearing
///
/// **Read this before concluding the seam is closed.** An `OwnedScheduleId` is
/// a bare `Uuid`. It records *that* some scope could see the schedule, never
/// *which tenant* that scope named, and nothing ties it to the scope or the
/// `tenant_id` later passed to [`SchedulesRepository::claim_tick`]. So a token
/// minted under a **multi-tenant** scope — precisely the shape the firing
/// ticker's nil-tenant enumeration compiles to — can be spent while writing
/// under a *different* tenant's scope, and the resulting row references one
/// tenant's schedule while belonging to another.
///
/// Two consequences, both worth having in front of you:
///
/// * **`idx_qa_schedule_ticks_claim`'s leading `tenant_id` is still doing work
///   and must not be "simplified" as redundant now that a token exists.** It is
///   what keeps such a row from colliding with — and thereby squatting, and
///   answering questions about — the owning tenant's own claims.
/// * The token narrows *who can reach* the foreign key; it does not remove the
///   reach. Under a single-tenant scope it closes the oracle outright, which is
///   every REST caller Task 20 will add. Under the ticker's enumeration scope it
///   does not, and that is not a defect to fix here: the ticker legitimately
///   needs to see every tenant's schedules.
///
/// `two_tenants_may_claim_the_same_schedule_id_and_due_at` asserts both halves —
/// that a single-tenant scope is refused, and that a multi-tenant one is not.
///
/// **Built now, before it is needed, on purpose.** Task 17's own caller — the
/// firing ticker — takes its schedule ids from its own scoped enumeration and
/// never from a request, so nothing today can reach the oracle. Task 20 adds the
/// REST layer that can. Retrofitting a token through a live call site is
/// strictly more expensive than adding it while [`SchedulesRepository`] has no
/// consumers at all, and the migration header's earlier claim that "nothing yet
/// needs one" was true only for as long as the next task did not exist.
///
/// # What the token proves, and the two limits on it
///
/// It proves that *some* [`SchedulesRepository::get`] answered `Some` for this
/// id under *some* scope, and it is exactly as trustworthy as that repository
/// is. [`OwnedRunId`](super::OwnedRunId) carries the long form of the argument.
/// Both limits, named rather than left to "the same limit as":
///
/// 1. **The tenant limit above** — the token does not bind the resolving scope
///    to the writing one. This is the schedules-specific one and it has no
///    counterpart on `OwnedRunId`, whose one caller resolves and writes under a
///    single tenant.
/// 2. **The double limit `OwnedRunId` also has** — a `get` that returns `Some`
///    unconditionally mints tokens freely, in safe Rust, with no override. **A
///    test double's `get` MUST apply tenant scoping to its fixture** or every
///    test using it mints tokens and "proves" a precheck it never ran.
///
/// The field is private to this module, so a token can never be conjured from
/// nothing. **Do not add a public constructor.**
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnedScheduleId(Uuid);

impl OwnedScheduleId {
    /// The verified schedule id.
    #[must_use]
    pub fn get(self) -> Uuid {
        self.0
    }
}

/// One row of [`SchedulesRepository::list_ticks`] — every column of
/// `qa_schedule_ticks` a caller may read back, except `tenant_id` (the scope's
/// business, not a caller's) and `created_at` (redundant with `due_at`/
/// `claimed_at`, which already order and timestamp the row for this read).
///
/// **Not every row is a fire attempt.** [`SchedulesRepository::list_ticks`]
/// returns [`SchedulesRepository::record_referential_check`]'s rows
/// unfiltered too — `claimed_by == `[`REFERENTIAL_CHECK_CLAIMED_BY`], `due_at`
/// a check instant rather than a due occurrence, `run_id` always `None`. See
/// that method's own doc, and this struct's `run_id` field below.
///
/// Same shape as `runs_repo::TestResultRow`: a `#[domain_model]` struct that
/// exists so the mapper (`infra::storage::mapper::tick_to_sdk`) has one place
/// to name every field the read needs, rather than building
/// [`qa_runs_sdk::ScheduleTick`] straight from the `SeaORM` model at the call
/// site, where a dropped field would compile silently.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleTickRow {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub due_at: OffsetDateTime,
    pub claimed_by: String,
    pub claimed_at: OffsetDateTime,
    /// `None` until `record_tick_outcome` writes it, forever for an orphaned
    /// claim (see this module's header) — or **always**, unconditionally,
    /// when `claimed_by == `[`REFERENTIAL_CHECK_CLAIMED_BY`]: that row was
    /// never a claim, so there is no launch outcome to ever write here.
    pub run_id: Option<Uuid>,
    pub error: Option<String>,
}

/// Project a stored tick row onto the SDK's read-only [`ScheduleTick`].
///
/// Infallible, like `runs_repo::TestResultRow`'s conversion: every column here
/// is a scalar or an opaque string, matching this table's own entity doc —
/// "nothing here decodes".
impl From<ScheduleTickRow> for ScheduleTick {
    fn from(row: ScheduleTickRow) -> Self {
        Self {
            id: row.id,
            schedule_id: row.schedule_id,
            due_at: row.due_at,
            claimed_by: row.claimed_by,
            claimed_at: row.claimed_at,
            run_id: row.run_id,
            error: row.error,
        }
    }
}

/// Ceiling on one [`SchedulesRepository::list_enabled`] scan. Same shape as
/// `queue_repo::MAX_CLAIM_SCAN`, for a different reason: a fleet's
/// enabled-schedule count is hand-curated, not something that grows without
/// bound, so this bounds only the pathological case — see that method's doc
/// for why fairness at ordinary scale comes from rotating `after`, not from
/// this cap being tight.
pub const MAX_SCHEDULE_SCAN: u64 = 1_000;

/// Repository for `qa_schedules` and its `qa_schedule_ticks` claim rows.
#[async_trait]
pub trait SchedulesRepository: Send + Sync {
    /// Insert a schedule, returning it with the fields this layer owns filled
    /// in.
    ///
    /// [`NewSchedule`] carries no `id`, `last_fired_tick`, `created_at` or
    /// `updated_at` for the reason `NewRun`'s doc gives at length: `qa_schedules.id`
    /// is a *global* primary key, so honouring a caller-supplied one would turn
    /// this method into a cross-tenant existence oracle — a probe carrying a
    /// victim's schedule id would collide on the primary key and be reported as
    /// a *name* collision that is not even taken. The id is minted here.
    ///
    /// # Errors
    ///
    /// [`DomainError::ScheduleNameExists`] when `(tenant_id, name)` collides.
    /// That index is tenant-prefixed, so the colliding row is always one this
    /// tenant can itself see and naming it leaks nothing.
    /// [`DomainError::Database`] otherwise.
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewSchedule,
    ) -> Result<Schedule, DomainError>;

    /// Scoped read by id.
    ///
    /// # Errors
    ///
    /// [`DomainError::CorruptState`] if the row does not decode — including an
    /// `exclusive_choice` outside the three values, which is never silently
    /// `auto`. [`DomainError::Database`] on a query failure.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Schedule>, DomainError>;

    /// Scoped read by name, the tenant-unique key.
    ///
    /// Exists because a schedule's stable identity to an operator is its name:
    /// the REST layer's create path has to answer "does this tenant already
    /// have one called `nightly`?" without relying on a unique-violation error,
    /// and a caller that only knows the name would otherwise have to list.
    ///
    /// **Not an existence oracle**, because the read is scoped: a name another
    /// tenant owns reads as `None`, exactly as [`Self::get`] on a foreign id
    /// does.
    ///
    /// # Only meaningful under a single-tenant scope
    ///
    /// **`name` is unique per tenant, not globally**, and this method returns
    /// *one* row. Under a scope admitting more than one tenant — the shape
    /// `domain::system_actor::for_schedule_tick`'s nil-tenant enumeration
    /// compiles to — several rows can match and **the one returned is
    /// arbitrary**: the query has no tie-break, so the answer is whatever the
    /// database's plan yields, and it may differ between dialects and between
    /// calls. Nothing prevents that; it is a constraint on the caller.
    ///
    /// So, stated as what this does *not* guarantee: it does not guarantee the
    /// returned schedule belongs to any particular tenant, and it does not
    /// guarantee `None` means no tenant in scope owns the name. Both hold only
    /// when the scope admits exactly one tenant, which is what the name-lookup
    /// callers — a REST create checking its own tenant's names — always pass.
    ///
    /// Left as a documented constraint rather than a runtime check, deliberately:
    /// a check would need to know how many tenants a compiled `AccessScope`
    /// admits, which is the PEP's business and not this layer's, and it would
    /// turn a caller's mistake into a repository error at the wrong altitude.
    /// The enumeration path does not use this method at all —
    /// [`Self::list_enabled`] is what a cross-tenant reader calls, and it returns
    /// every row with its tenant.
    ///
    /// # Errors
    ///
    /// As [`Self::get`].
    async fn get_by_name<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        name: &str,
    ) -> Result<Option<Schedule>, DomainError>;

    /// Every schedule in scope, by name.
    ///
    /// **Uncapped**, like `RunsRepository::list` and for the same reason: there
    /// is no `limit` parameter to clamp and a silent ceiling would truncate a
    /// reader's answer without it knowing. The exposure is smaller here — a
    /// tenant's schedules are a hand-curated set, where its runs accumulate
    /// forever — but it is the same shape, and if a paginated read is ever
    /// wanted it belongs beside `RunsRepository::list_page` rather than as a
    /// hidden cap on this one.
    ///
    /// Ordered by `name`, not by `created_at`: the name is what an operator
    /// identifies a schedule by, and `idx_qa_schedules_tenant_name` already
    /// serves that order.
    ///
    /// # Errors
    ///
    /// As [`Self::get`].
    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Schedule>, DomainError>;

    /// Resolve `schedule_id` under the caller's own scope, producing the
    /// [`OwnedScheduleId`] that [`Self::claim_tick`] demands.
    ///
    /// A **provided** method, so no implementation can weaken it: the token's
    /// field is private to this module, so an override can only obtain one by
    /// calling this body. Same shape as `RunsRepository::resolve_owned`.
    ///
    /// # Errors
    ///
    /// [`DomainError::ScheduleNotFound`] when the schedule does not exist *or*
    /// is not visible in `scope` — the two must be indistinguishable, since
    /// telling them apart is the cross-tenant existence oracle this method
    /// exists to close.
    async fn resolve_owned_schedule<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        schedule_id: Uuid,
    ) -> Result<OwnedScheduleId, DomainError> {
        match self.get(runner, scope, schedule_id).await? {
            Some(_) => Ok(OwnedScheduleId(schedule_id)),
            None => Err(DomainError::ScheduleNotFound { id: schedule_id }),
        }
    }

    /// Replace every caller-decidable field of a schedule.
    ///
    /// `Ok(None)` when the schedule does not exist *or* is not visible in
    /// `scope` — the two must be indistinguishable, for the reason
    /// [`Self::resolve_owned_schedule`] states about its own not-found.
    ///
    /// # One race escapes that contract, and it is the shared convention
    ///
    /// The implementation reads the row under `scope` and then updates it, so a
    /// **delete landing between the two** makes the update find nothing. That
    /// surfaces as `ScopeError::Denied` → [`DomainError::Database`], i.e. a 500,
    /// where the same caller a moment earlier would have got `Ok(None)`. Both
    /// sibling gears' `update` methods have the identical window — it is
    /// `secure_update_with_scope`'s shape, not this method's mistake — so the
    /// behaviour is left alone rather than diverging one repository from the
    /// house pattern.
    ///
    /// Worth stating anyway, because *within qa-runs* this is the odd one out:
    /// `RunsRepository` and `QueueRepository` write through `update_many()`,
    /// which reports a count and cannot produce this error. A reader comparing
    /// the three would otherwise read the difference as unintended.
    ///
    /// **`last_fired_tick` is preserved**, which is the one thing this method
    /// must not touch: it is the cron evaluator's cursor, and resetting it on an
    /// edit would re-fire every due time since the schedule last ran. Editing a
    /// schedule's cron expression legitimately changes *which* times are due
    /// from here on; it does not un-fire the past. [`Self::advance_last_fired_tick`]
    /// is the column's only writer.
    ///
    /// # Errors
    ///
    /// [`DomainError::ScheduleNameExists`] when the new name is taken within
    /// the tenant, [`DomainError::Database`] on a query failure.
    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        new: NewSchedule,
    ) -> Result<Option<Schedule>, DomainError>;

    /// Write the three Slack notification columns of one schedule, and
    /// **nothing else**. `Ok(None)` means no row in scope matched.
    ///
    /// # Why this is not [`Self::update`] with a wider payload
    ///
    /// The source system's endpoint edits exactly these three values and
    /// reconstructs every other field to keep it — its `exclusive` carry-forward
    /// is commented in as many words
    /// (`manager/src/routes/schedules.rs:854-856`). A method that took a whole
    /// [`NewSchedule`] would make "leave the cron alone" the caller's job, on a
    /// resource where the sibling `update`'s own doc already records that two
    /// concurrent replaces are last-writer-wins. Three columns in the `SET` list
    /// is that guarantee expressed as a statement rather than as a convention.
    ///
    /// `updated_at` moves, because the row did change. `last_fired_tick`,
    /// `created_at` and `tenant_id` do not, for the reasons [`Self::update`]
    /// gives.
    ///
    /// The delete-between-read-and-update window [`Self::update`] documents
    /// applies here identically — same `secure_update_with_scope` shape, same
    /// `Ok(None)`-becomes-500 outcome — and is likewise left alone rather than
    /// diverging one method from the house pattern.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure. There is no name to
    /// collide, so no [`DomainError::ScheduleNameExists`] arm.
    async fn update_notifications<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        settings: ScheduleNotificationSettings,
    ) -> Result<Option<Schedule>, DomainError>;

    /// Delete a schedule. `false` means no row in scope matched.
    ///
    /// Its tick rows go with it, by `ON DELETE CASCADE` rather than by a second
    /// statement — and they must go, because a claim row surviving its schedule
    /// would block the same `(schedule_id, due_at)` from ever being claimed
    /// again if the id were reused.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Every enabled schedule in scope, each paired with **its own tenant id**.
    ///
    /// # Why the tenant travels with the row
    ///
    /// This is the firing ticker's enumeration, and it runs the pattern the
    /// dispatcher's sweeps run: one cross-tenant read under a nil-tenant context
    /// (`domain::system_actor::for_schedule_tick`), then a **per-tenant** context
    /// for every write that follows
    /// (`domain::system_actor::for_schedule_fire`). A caller holding only the
    /// schedule would have nothing to mint the second from. Same reasoning that
    /// produced `domain::repos::TimeoutCandidate` and
    /// `domain::repos::WatchCandidate`; a tuple suffices here because
    /// [`Schedule`] and `Uuid` are not interchangeable to the compiler, which is
    /// what made those two named structs necessary.
    ///
    /// `Uuid::nil()` is not filtered out here. A schedule whose `tenant_id` is
    /// nil is a corrupt row rather than a platform-owned one, and the caller has
    /// to refuse it — the dispatcher's sweeps make the same check at the same
    /// point, because writing under the platform-root identity is worse than
    /// skipping a row.
    ///
    /// **Windowed, but not for the reason every other cross-tenant read in
    /// this gear is.** Those are windowed because their sets grow without
    /// bound; a hand-curated fleet of schedules does not, and
    /// [`MAX_SCHEDULE_SCAN`] exists only to cap the pathological case, not to
    /// decide which schedules get a chance to fire — a schedule *dropped* by
    /// this scan's own cap would be a fire that never happens, with nothing
    /// to retry it until the next due time, which is exactly the outcome
    /// [`ScheduleService`](crate::domain::service::schedules::ScheduleService)'s
    /// own doc on `MAX_FIRES_PER_TICK` rejected for the *service's* cap. So
    /// `after` exists for a narrower purpose: it lets the caller rotate which
    /// end of the id-ordered fleet is read first, which is what closes the
    /// starvation that constant's doc names — see `Windowed::truncated`'s own
    /// distinction between "more rows exist past this cap" (this method's
    /// business) and "the fire budget ran out before the window did" (the
    /// service's, tracked and rotated on its own cursor, not derived from
    /// this method's `truncated` flag). `None` starts from the beginning, in
    /// `id` order, at most [`MAX_SCHEDULE_SCAN`] rows.
    ///
    /// # One corrupt row must not stop the fleet
    ///
    /// **A row that fails to decode is skipped, logged at WARN with its id and
    /// tenant, and the enumeration continues.** It is *not* returned and *not*
    /// an error, so a caller cannot tell a skipped row from one that does not
    /// exist — the log line is the only signal.
    ///
    /// This is still failing closed: that schedule does not fire. What it stops
    /// is one row failing *every* schedule in the fleet. The alternative — the
    /// obvious `collect::<Result<Vec<_>, _>>()` — aborts the whole cross-tenant
    /// pass on the first bad row, so every tenant's schedules stop firing until
    /// somebody repairs it, and nothing self-heals because
    /// [`Self::advance_last_fired_tick`] never runs either. The trigger that
    /// makes this more than theoretical is a **rolling upgrade**: an older
    /// replica's decoder meeting a value a newer replica wrote takes the entire
    /// scheduler dark for the duration of the deploy.
    ///
    /// Deliberately decided here rather than deferred to the caller as a
    /// `Vec<Result<..>>`: the caller has no information this layer lacks, so
    /// handing it the choice would only move the decision somewhere it is easier
    /// to get wrong.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure — but **not**
    /// [`DomainError::CorruptState`], which is what the skip above replaces.
    async fn list_enabled<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        after: Option<Uuid>,
    ) -> Result<Windowed<(Schedule, Uuid)>, DomainError>;

    /// Claim `(schedule_id, due_at)` for this instance, returning the tick id
    /// on success and **`Ok(None)` when somebody else already holds it**.
    ///
    /// # This is the exactly-once mechanism
    ///
    /// `cpt-cf-qa-nfr-scheduler-exactly-once` is discharged by
    /// `idx_qa_schedule_ticks_claim` and by nothing above it. The claim is an
    /// `INSERT` against a unique index: two instances racing for one due time
    /// both attempt it, one commits, the other takes a unique violation. No
    /// read-then-write and no advisory lock — a constraint, not a convention.
    /// (No section of `DESIGN.md` states this; an earlier revision of this
    /// comment cited "DESIGN §3.7", which is Product SDK and says nothing of
    /// the kind. The property is stated here, in the one place code actually
    /// enforces it.)
    ///
    /// # `Ok(None)` is the normal path, and must never be a 500
    ///
    /// A lost race happens on every non-leader instance and on every leader
    /// failover, so it is expected traffic rather than a fault. Letting it
    /// surface as [`DomainError::Database`] would answer 500 for correct
    /// behaviour and would put another system's text into this gear's logs at
    /// error level.
    ///
    /// The implementation classifies it with `ScopeError::is_unique_violation`
    /// rather than by matching driver text **at this call site**. That is a
    /// weaker statement than it first appears, and the difference matters:
    /// `toolkit_db::secure::is_unique_violation` prefers `SeaORM`'s parsed
    /// SQLSTATE but **falls back to lowercased substring matching** on the
    /// rendered message (`unique constraint`, `duplicate key`,
    /// `unique_violation`, `duplicate entry`, `unique constraint failed`) for
    /// drivers and proxies that strip the code. So the text matching is not
    /// absent, only centralized — and the consequence is real: a fault that is
    /// *not* a unique violation, whose message happens to contain one of those
    /// phrases, is classified as a lost race and becomes a **silently skipped
    /// fire**. Everything genuinely unrelated still reaches
    /// [`DomainError::Database`]; `a_non_unique_failure_is_an_error_not_a_lost_race`
    /// pins that direction.
    ///
    /// # The claim is durable even when the launch fails
    ///
    /// A won claim is **not** rolled back if the launch that follows it fails —
    /// see [`Self::record_tick_outcome`], which records the failure on the row
    /// instead. Deleting the row would let the next tick re-fire the same due
    /// time, and for a destructive suite one accidental extra run is a worse
    /// outcome than one missed one.
    ///
    /// # This surface cannot recover a lost fire, and depends on `next_due` for that
    ///
    /// A claim won by a process that dies before it launches is never retried:
    /// the row is durable, so the same due time can never be claimed again, and
    /// **no method here can advance the cursor past it** —
    /// [`Self::advance_last_fired_tick`] is called by the path that just died.
    ///
    /// What makes that self-healing rather than permanent is a property of the
    /// cron evaluator, not of this trait. `domain::cron`'s `next_due` is
    /// specified as *the most recent occurrence at or before `now`, never
    /// back-fill*, so the following tick simply computes the **next** occurrence
    /// and proceeds; the lost one is skipped and nothing is wedged.
    ///
    /// **Stated as a dependency, because it is one and it is now load-bearing.**
    /// `domain::cron::next_due` is implemented and
    /// `domain::service::schedules`'s `outstanding` is the caller; that module's
    /// own header states the same interlock from the other side. Were it ever
    /// changed to "the earliest outstanding occurrence" — the reading that
    /// back-fills, and a defensible one in isolation — a mid-fire crash would
    /// wedge that schedule **permanently**: every later tick recomputes the same
    /// orphaned due time, loses the claim it already holds, and never moves on.
    ///
    /// This paragraph shipped addressed to "whoever implements `next_due`", who
    /// had already implemented it. Addressed now to whoever changes it.
    ///
    /// # Do not call this inside a caller-owned transaction on Postgres
    ///
    /// A failed statement aborts the enclosing Postgres transaction, so a lost
    /// race inside one poisons every later statement in it — the `Ok(None)` this
    /// method returns would be followed by errors the caller cannot explain. The
    /// firing ticker calls it on a plain connection, one claim at a time, which
    /// is also the shape that keeps the claim committed before the launch begins.
    /// Not enforceable in the signature: `DBRunner` is implemented by both.
    ///
    /// # What the ownership token does and does not settle here
    ///
    /// `schedule` is an [`OwnedScheduleId`], so the tenant-blind foreign key
    /// cannot be the thing that answers whether a schedule exists. **That is one
    /// of two limits on the token, not the whole of what it leaves open** — read
    /// [`OwnedScheduleId`] itself before relying on it, because the token carries
    /// no tenant and one minted under a multi-tenant scope can be spent under a
    /// different tenant's write scope. The other limit is the race below.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on any failure that is not a unique violation
    /// — including the foreign-key violation an [`OwnedScheduleId`] whose
    /// schedule was deleted after the token was minted still produces. The token
    /// is a precheck, not a lock: it closes the existence oracle, and it neither
    /// keeps the referenced row alive nor binds the write to the tenant it was
    /// resolved under.
    async fn claim_tick<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        schedule: OwnedScheduleId,
        due_at: OffsetDateTime,
        claimed_by: &str,
    ) -> Result<Option<Uuid>, DomainError>;

    /// Record what a won claim produced: the run it launched, or why it did not.
    ///
    /// `false` means no tick row in scope matched. Both columns are written on
    /// every call, so passing `None` for either clears it — which is correct
    /// because this is the row's **single** post-claim write and the columns are
    /// NULL until it happens.
    ///
    /// Unguarded, like `RunsRepository::set_execution_ref`: the caller is the
    /// instance that won the claim and no other writer exists, so there is no
    /// transition to compare and set.
    ///
    /// # `false` does not portably mean "no such row"
    ///
    /// The implementation reports `rows_affected == 1`, and **`MySQL` counts
    /// *changed* rows where Postgres and `SQLite` count *matched* ones**. So a
    /// call that writes `(None, None)` against a row already holding NULLs
    /// changes nothing, and the three dialects disagree: `true` on Postgres and
    /// `SQLite`, `false` on `MySQL`, for a row that exists and is in scope. This
    /// table has no `updated_at` to force a change and mask the difference —
    /// that absence is deliberate for other reasons (see this module's header),
    /// and this is one of its consequences.
    ///
    /// Unreachable from the firing ticker, which always records either a run id
    /// or a reason, never neither. Written down because the `MySQL` tier is
    /// executed by nothing in this workspace, so no test can catch a caller who
    /// later depends on the other reading.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn record_tick_outcome<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tick_id: Uuid,
        run_id: Option<Uuid>,
        error: Option<&str>,
    ) -> Result<bool, DomainError>;

    /// Move the schedule's fired-through cursor forward to `due_at`.
    ///
    /// # Not in Task 17's method list, and why it is here anyway
    ///
    /// The firing pipeline this repository exists to serve ends with *"record
    /// the tick outcome and advance `last_fired_tick`"*, and no other method can
    /// perform the second half: `last_fired_tick` is absent from [`NewSchedule`],
    /// so [`Self::update`] cannot reach it, and it must not — an operator editing
    /// a cron expression would otherwise rewind the cursor and re-fire the past.
    /// A column with no writer would have made the ticker unimplementable, so
    /// this is the one addition to the specified surface, called out rather than
    /// slipped in.
    ///
    /// # Monotonic, and that is a guard rather than a nicety
    ///
    /// The `UPDATE` carries `last_fired_tick IS NULL OR last_fired_tick < $due_at`,
    /// so the cursor can only move forward. The contract calls this column *the
    /// latest due time this schedule has fired for*, and a blind write would
    /// make that false the first time a late tick landed after an early one — the
    /// cron evaluator would then recompute due times it had already fired, whose
    /// claims would all be refused, turning a quiet no-op into a stream of lost
    /// races.
    ///
    /// `false` therefore means one of two things — no row in scope, or the cursor
    /// was already at or past `due_at` — and the implementation cannot say which,
    /// for the reason `domain::repos`' *"Guarded writes return `bool`"* section
    /// gives. Neither is an error and neither is retryable.
    ///
    /// # It writes `updated_at` as well, and that is visible to operators
    ///
    /// Two columns move, not one. The row genuinely was updated, so bumping the
    /// house-style audit column is the consistent choice and it is kept — but the
    /// consequence is that **a schedule's "last modified" timestamp advances with
    /// no operator action**, once per fire. A frequently-firing schedule
    /// therefore sorts to the top of any `updated_at` ordering purely because it
    /// is doing its job. A UI that presents that column as "last edited" will be
    /// wrong; `created_at` and the schedule's own `last_fired_tick` are the two
    /// columns that mean what they say.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn advance_last_fired_tick<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        due_at: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// One schedule's fire history, most recent `due_at` first: what a real
    /// fire claimed, what it produced (`run_id`) or why it did not (`error`)
    /// — **plus, unfiltered, any [`REFERENTIAL_CHECK_CLAIMED_BY`] row.** That
    /// row is not a fire attempt at all (see
    /// [`Self::record_referential_check`]'s own doc): no filter here
    /// distinguishes it, so a caller that assumes every returned row was a
    /// claim will misread one as "a fire that produced no run".
    ///
    /// Closes the gap this trait's own header names — "the row explaining why
    /// is reachable by no query, endpoint or SDK method" — for a fixed claim
    /// followed by a failed launch, a full queue, or an instance death. The
    /// scope is compiled for `resources::SCHEDULE` with `schedule_id` as the
    /// resource id, exactly as [`Self::claim_tick`]/[`Self::record_tick_outcome`]
    /// already do — see this module's header; "the resource id is the
    /// schedule's, not the tick's" is `ScheduleService`'s decision, not this
    /// repository's, but the scope it is handed always carries that shape.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn list_ticks<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        schedule_id: Uuid,
    ) -> Result<Vec<ScheduleTickRow>, DomainError>;

    /// Write a synthetic `qa_schedule_ticks` row recording that `schedule`'s
    /// target no longer resolves — the background counterpart of
    /// `create`/`update`'s write-time check
    /// ([`crate::domain::service::launch::LaunchService::resolve_target_exists`]).
    ///
    /// `checked_at` is the check's own timestamp, not a real due instant, and
    /// `run_id` is always implicitly `None`: this is not a claim and must not
    /// be confused with one. `claimed_by` is fixed at the implementation to
    /// [`REFERENTIAL_CHECK_CLAIMED_BY`], so this row is visually
    /// distinguishable in the tick history from a real fire attempt. Only
    /// called when a problem is found — a clean check writes nothing, so a
    /// healthy fleet does not fill this table.
    ///
    /// # Why this does **not** reuse `claim_tick`/`record_tick_outcome`
    ///
    /// Both of those are keyed to a real due instant and exist to make firing
    /// exactly-once — `idx_qa_schedule_ticks_claim`'s whole point. Spending a
    /// claim on a synthetic timestamp would let a referential-check pass
    /// collide with, or itself win, the unique index a real fire depends on.
    ///
    /// **Being a plain `INSERT` does not exempt this row from that index.**
    /// `idx_qa_schedule_ticks_claim` covers `(tenant_id, schedule_id,
    /// due_at)` over the *whole* table, so this row occupies a slot in it
    /// exactly as a real claim would — "nothing races it" is not why a
    /// collision cannot happen. What actually keeps the two apart is
    /// `checked_at`: it is `now_utc()` at sub-second precision, while a real
    /// `due_at` is a cron occurrence on a minute boundary, so the two spaces
    /// do not overlap in practice.
    ///
    /// **If they ever did collide, the failure is not "the check fails".**
    /// Whichever of the two rows lands second takes the unique violation.
    /// If that is the real fire, `claim_tick`'s own mapping
    /// (`infra::storage::schedules_sea_repo`) classifies any unique
    /// violation on this index as a lost race and returns `Ok(None)` — the
    /// normal, silent outcome for "someone else already claimed this". The
    /// caller then does not launch, believing another instance already
    /// fired. No row records the real occurrence, and cron does not
    /// back-fill: that due instant is silently skipped and never retried.
    ///
    /// # `schedule` and `tenant_id`, not a bare `Uuid`
    ///
    /// Same shape as [`Self::claim_tick`] and for the same reason — see
    /// [`OwnedScheduleId`]'s doc: the token proves a scope could see the
    /// schedule but carries no tenant of its own, so the tenant the row is
    /// written under is a separate, explicit argument rather than derived
    /// from the token.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn record_referential_check<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        schedule: OwnedScheduleId,
        checked_at: OffsetDateTime,
        error: &str,
    ) -> Result<(), DomainError>;
}

/// `qa_schedule_ticks.claimed_by` for a row
/// [`SchedulesRepository::record_referential_check`] wrote, as opposed to one
/// a real fire attempt claimed. Public so the service layer that calls the
/// method and any test that asserts on the row agree on the literal without
/// either restating it.
pub const REFERENTIAL_CHECK_CLAIMED_BY: &str = "referential-check";

/// Ceiling on one [`SchedulesRepository::list_ticks`] read. A tick row is
/// written once per fire attempt and never deleted except by its schedule's
/// cascade, so an operator-authored, frequently-firing schedule is the one
/// case this bounds rather than a caller-controlled window — same shape as
/// `RunsRepository::MAX_TIMEOUT_SWEEP_SCAN`'s reasoning, for the same reason: a
/// repository that will materialise however many rows exist is the layer that
/// actually allocates them.
pub const MAX_TICK_READ_LIMIT: u64 = 200;
