//! The per-platform run queue.
//!
//! Each method is named after the legacy function it ports so the mapping is
//! greppable: `manager/src/services/run_queue.rs`. Two of them diverge in
//! signature, both because Task 9's schema deliberately dropped legacy's
//! `run_queue.workflow_name` column — see [`QueueRepository::mark_running`]
//! and [`QueueRepository::fail_orphaned_dispatching`], which say so where a
//! reader will hit it.
//!
//! ## Multi-field returns are named structs
//!
//! Every one of them, per the rule the source system states three times
//! (`run_queue.rs:136-147`, `:682-693`,
//! `manager/src/services/exclusivity.rs:88-95`): a pair of same-typed fields
//! is a transposition the compiler cannot catch. Where this port's types
//! already make a transposition impossible — [`RowStatus`] is the clearest,
//! since legacy's two `String`s became a `Uuid` and a `QueueState` here — the
//! struct is kept anyway, but its doc says which of the two reasons applies.
//! Recording a hazard that the port removed as though it survived would be a
//! false comment, and this subsystem treats those as defects.

use async_trait::async_trait;
use qa_runs_sdk::{QueueState, RunKind, RunSource};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::runs_repo::OwnedRunId;
use crate::domain::error::DomainError;
use crate::domain::queue::AdmissionDecision;
use crate::domain::repos::Windowed;

/// Ceiling on [`QueueRepository::list_for_read`]'s window.
///
/// Legacy takes an uncapped `i64` (`manager/src/services/run_queue.rs:457`)
/// and relies on its callers. A repository that will materialise however many
/// rows a caller names is a denial-of-service primitive one forgotten
/// validator away, and this is the layer that actually allocates them. That
/// argument stands alone and is the whole justification.
///
/// **The tension, stated rather than hidden.** A cap is truncation, and
/// truncation is exactly what the frozen guide warns about: a `limit` that
/// cuts the window makes `queue_position` *understate* how far back a row
/// really is, so a truncated row's position collapses toward 1 and
/// `describe_blocker` reports "waiting for its turn" instead of naming the
/// rows ahead (guide lines 171-184, and legacy says the same at
/// `manager/src/services/run_queue.rs:435-449`). So this constant does not
/// make the answer better — it bounds a distortion that is already unbounded,
/// and the guide's own remedy is the platform-filtered call, not a bigger
/// window.
///
/// **Corrected 2026-08-13 by the spec review.** An earlier version of this
/// comment cited those guide lines as saying a large window "answers the
/// operator's question poorly", i.e. as an argument *for* capping. They say
/// the opposite. Reading a citation as support when it is a caveat is the
/// failure mode this subsystem keeps finding; the citation stays, pointed the
/// right way.
pub const MAX_QUEUE_READ_LIMIT: u64 = 1_000;

/// How many claims one [`QueueRepository::all_claims`] scan will materialise.
///
/// Separate from [`MAX_QUEUE_READ_LIMIT`] even though the two currently agree:
/// that one bounds a *caller-supplied* window on an operator read, this one
/// bounds a fixed cross-tenant scan the dispatcher issues on every tick, and
/// the reasons to move either are unrelated.
///
/// # This bounds cost per tick, not coverage
///
/// A window alone would be a starvation primitive, and was one until
/// 2026-08-15: see [`QueueRepository::all_claims`] for the ordering that makes
/// every claim reachable within a bounded number of ticks, and for the size of
/// that bound.
pub const MAX_CLAIM_SCAN: u64 = 1_000;

/// An admitted launch, ready to be filed.
///
/// `run` is an [`OwnedRunId`] rather than a `Uuid`: the foreign key on
/// `run_id` is tenant-blind, so a caller-supplied id must have been resolved
/// under the caller's own scope before it reaches this insert. See
/// [`OwnedRunId`].
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewQueueRow {
    /// **Unverified here, and unverifiable here.**
    ///
    /// Unlike [`Self::run`], this id carries no token, because the platform
    /// lives in qa-environments and only the launch path holds a client to
    /// check it against. There is no oracle — the column has no foreign key,
    /// so an unowned id neither succeeds nor fails informatively — but there is
    /// a real harm the schema states in full on `qa_runs.environment_id`:
    /// `qa_environment_leases` (renamed from `qa_platform_leases`) is keyed on a
    /// bare `environment_id` and is therefore **not** tenant-partitioned, so a row
    /// carrying another tenant's platform
    /// drives its dispatcher to take the *global* lease on that platform and
    /// block the owning tenant's runs.
    ///
    /// **The launch path must verify tenant ownership of this platform through
    /// a tenant-scoped qa-environments client before constructing this
    /// struct.** Nothing downstream re-checks, and no type in this layer can
    /// make it. Recorded here as well as in the migration because this is the
    /// struct someone fills in.
    pub environment_id: Uuid,
    pub run: OwnedRunId,
    /// Denormalized from the run so the FIFO planner needs no join, exactly as
    /// legacy's `run_queue` carries them
    /// (`manager/migrations/001_initial.sql:291`, `:294`, `:295`).
    pub run_kind: RunKind,
    pub source: RunSource,
    /// The run's *resolved* exclusivity. Short name here, `resolved_exclusive`
    /// on the run: different tables, different vocabularies.
    pub exclusive: bool,
    /// Which of the two admitted states the row starts in.
    pub decision: AdmissionDecision,
}

/// A queue row as stored.
///
/// Not `qa_runs_sdk::QueueEntry`: that type also carries `queue_position`,
/// `ttl_expires_at` and `blocked_by`, none of which are columns — they are
/// computed per request over the rows the request returned (guide lines
/// 171-184). Mixing stored and derived fields in one repository return would
/// force this layer to know the TTL setting.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueRowRecord {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub run_id: Uuid,
    pub environment_id: Uuid,
    pub run_kind: RunKind,
    pub source: RunSource,
    pub exclusive: bool,
    pub state: QueueState,
    pub error: Option<String>,
    pub enqueued_at: OffsetDateTime,
    pub dispatched_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}

/// A row holding a claim on its platform.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimRow {
    pub id: Uuid,
    /// The run this claim is for. The claim reconciler joins through it to
    /// reach `qa_runs.execution_ref`, because this table deliberately keeps no
    /// execution handle of its own (see the migration's module header).
    pub run_id: Uuid,
    pub exclusive: bool,
}

/// A claim plus the instant its age is measured from.
///
/// `age_basis` is `dispatched_at` falling back to `enqueued_at`, which is how
/// legacy coalesces it (`manager/src/services/run_queue.rs:341-346`). Feeds
/// `domain::state_machine::ClaimObservation::age_seconds` via
/// `elapsed_seconds`.
///
/// `tenant_id` travels with the row because claim reconciliation enumerates
/// across tenants and then writes under a per-tenant system context.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimAge {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub run_id: Uuid,
    pub environment_id: Uuid,
    pub age_basis: OffsetDateTime,
}

/// A platform with at least one queued row, and the tenant that owns it.
///
/// Two `Uuid`s: exactly the transposition hazard the named-struct rule is
/// about, and the one place in this file where the compiler genuinely cannot
/// help.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedPlatform {
    pub environment_id: Uuid,
    pub tenant_id: Uuid,
}

/// A row the TTL sweep expired, carrying everything the mandatory `expired`
/// alert needs (guide line 96).
///
/// Returned by the sweep itself so a row cannot be expired without the caller
/// holding the data its alert announces — the property legacy gets from
/// `UPDATE ... RETURNING` (`manager/src/services/run_queue.rs:370-401`).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpiredRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub run_id: Uuid,
    pub environment_id: Uuid,
    pub exclusive: bool,
    pub enqueued_at: OffsetDateTime,
}

/// A queue row's platform and state, for the operator endpoints.
///
/// Named fields, following `manager/src/services/run_queue.rs:136-147` — but
/// **not** for legacy's reason. There the two are both `String` and
/// transposing them takes the wrong platform lock; here they are a `Uuid` and
/// a [`QueueState`], so the compiler already refuses the swap. The struct
/// survives for the other half of the argument: the platform is the lock key
/// the force-start path needs and the state is what turns "the update matched
/// nothing" into a specific 404 or 409, and a tuple would leave that
/// documented rather than named.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowStatus {
    pub environment_id: Uuid,
    pub state: QueueState,
}

/// Repository for `qa_run_queue`.
#[async_trait]
pub trait QueueRepository: Send + Sync {
    /// File an admitted launch (`manager/src/services/run_queue.rs:162-200`).
    ///
    /// An admitted launch is inserted **directly in `dispatching`**, with
    /// `dispatched_at` set; that is what makes the row a claim *before* the
    /// caller submits to the executor. A queued one starts in `queued` with no
    /// `dispatched_at`.
    ///
    /// # Errors
    ///
    /// [`DomainError::QueueRowExists`] when the run already has a queue row in
    /// this tenant (`idx_qa_run_queue_tenant_run`).
    /// [`DomainError::Database`] otherwise.
    async fn insert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        row: NewQueueRow,
    ) -> Result<QueueRowRecord, DomainError>;

    /// How many rows are waiting for a platform (`:232-240`).
    ///
    /// **Must be read inside the platform lock, before the occupancy read.**
    /// That ordering is the admission race fix and it cannot live in
    /// `domain::queue`, which is pure: two concurrent launches against a
    /// queue one slot from full would otherwise both observe room. Legacy
    /// takes the lock, reads the depth, and rejects before anything else, and
    /// says why in place (`:633-655`).
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure, or
    /// [`DomainError::Internal`] if the count does not fit a `usize`.
    async fn queued_depth<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        environment_id: Uuid,
    ) -> Result<usize, DomainError>;

    /// Queued rows for a platform, oldest first — the dispatcher's FIFO input
    /// (`:243-257`, `ORDER BY enqueued_at ASC, id ASC`).
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn queued_rows<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        environment_id: Uuid,
    ) -> Result<Vec<crate::domain::queue::QueuedRow>, DomainError>;

    /// Unreleased claims on a platform — rows in `dispatching` or `running`
    /// (`:203-229`, `CLAIM_STATES` at `:104`).
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn claims_for_platform<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        environment_id: Uuid,
    ) -> Result<Vec<ClaimRow>, DomainError>;

    /// Platforms that currently have at least one queued row (`:260-267`),
    /// each with its tenant so the caller can mint a per-tenant system context
    /// for the writes that follow.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn platforms_with_queued_rows<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<QueuedPlatform>, DomainError>;

    /// Claim a queued row for this tick (`:281-291`).
    ///
    /// `WHERE id = $1 AND state = 'queued'`; `false` means the row was no
    /// longer queued — "a cheap guard against double dispatch".
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn mark_dispatching<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Put a claimed row back in the queue, keeping its place.
    ///
    /// **Added by Task 14, because the alternative was a lost run.** The
    /// dispatcher claims a row with [`Self::mark_dispatching`] and then acquires
    /// the platform lease; the lease CAS may refuse — `domain::queue`'s module
    /// docs state the rule this discharges: *"a row the planner selects may still
    /// lose its `acquire` and **stay queued for the next tick**."* There was no
    /// method for that, so the first implementation failed the row and tried to
    /// retire its run instead.
    ///
    /// **That path could not work, and this is the evidence.** At the moment the
    /// lease is refused the run is still `Queued` (or `Created`, on the
    /// launch-recovery path) — the tick has not called `dispatch_one` yet, so
    /// nothing has moved it to `Dispatching`. `can_transition` admits
    /// `Queued -> {Dispatching, Canceled, Expired}` and `Created ->
    /// {Queued, Dispatching, Canceled}`: **neither admits `Error`**. So the run
    /// transition failed, the queue row was left `failed`, and the run stayed
    /// `Queued` forever with no queued row for any later tick to claim. A lost
    /// run, on a path no test covered.
    ///
    /// # Why this is not a state-machine regression
    ///
    /// `dispatching -> queued` is not in the frozen guide's row-state narrative
    /// (guide lines 88-97), which reads forward only. It is nevertheless the
    /// conservative direction: the row returns to exactly the FIFO position it
    /// held, because `enqueued_at` is untouched and FIFO is
    /// `ORDER BY enqueued_at ASC, id ASC` (`:243-257`). An operator sees a row
    /// that briefly said `dispatching` and says `queued` again — which is what
    /// happened.
    ///
    /// `dispatched_at` is **cleared**, so the row does not carry a dispatch
    /// instant it no longer has. That matters beyond tidiness: [`Self::all_claims`]
    /// ages a claim from `dispatched_at` falling back to `enqueued_at`, so a
    /// re-claimed row that kept a stale value would be aged from its *first*
    /// claim and could cross `orphan_timeout_seconds` immediately.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure. `false` when the row was no
    /// longer `dispatching` — the guard, which is the safety property here: a row
    /// that reached `running` holds a live execution, and returning it to the
    /// queue would let the same run be dispatched twice.
    async fn requeue<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Record a successful submission (`:294-303`).
    ///
    /// **Unguarded, like [`Self::mark_failed`] and [`Self::mark_done`], and
    /// unlike [`Self::mark_dispatching`] and [`Self::cancel_queued`].** That
    /// asymmetry is legacy's and is ported deliberately: these three are the
    /// dispatcher's and the reconciler's, called about a row whose state they
    /// have already decided, while the two guarded ones answer a *request* and
    /// must refuse a row that moved. Note what that means for `mark_done` in
    /// particular — it will terminate a `running` row and release its claim on
    /// the platform, which is correct for a reconciler that has just observed
    /// the execution end and **wrong** for anything user-facing. A cancel
    /// endpoint must go through `cancel_queued`, never this.
    ///
    /// **Takes no execution reference**, where legacy's takes a
    /// `workflow_name`. This table has no column for one: `run_id` resolves to
    /// a `qa_runs` row that already owns `execution_ref`, and a second copy
    /// could only disagree with the first (see the migration's module header).
    /// The handle is recorded by `RunsRepository::set_execution_ref`, which
    /// dispatch calls *before* this.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn mark_running<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Record a failed submission and release the claim (`:306-317`).
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn mark_failed<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        error: &str,
    ) -> Result<bool, DomainError>;

    /// Release a claim whose execution is terminal or gone (`:320-326`).
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn mark_done<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Cancel a row that has not started yet (`:410-421`).
    ///
    /// The `AND state = 'queued'` guard is the **safety property**, not an
    /// optimisation: a `dispatching`/`running` row holds a claim an in-flight
    /// execution depends on, and dropping it here would let a new run be
    /// admitted beside an exclusive one. Stopping a started run is a different
    /// operation.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn cancel_queued<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        reason: &str,
    ) -> Result<bool, DomainError>;

    /// One window of claims across every platform, for reconciliation
    /// (`:329-347`).
    ///
    /// Returns the rows whose id sorts **after** `after`, in `id` order, at most
    /// [`MAX_CLAIM_SCAN`] of them. `None` starts from the beginning.
    ///
    /// # Why the window rotates, and what happens when it does not
    ///
    /// This read is cross-tenant and takes no caller `limit`
    /// ([`MAX_QUEUE_READ_LIMIT`] governs [`Self::list_for_read`] and does not
    /// apply here), and the dispatcher issues it twice per tick, adding one
    /// scoped run read and one fresh `access_scope` per claim returned. Nothing
    /// else bounds how many claims exist: `max_concurrent_runs` is an operator
    /// setting that can still be `0` — the explicit "unbounded" opt-out; the
    /// shipped default is now 50 (`crate::config::QaRunsConfig::max_concurrent_runs`),
    /// but an operator can always dial it back to unbounded. So the read has to
    /// be windowed regardless of which is configured.
    ///
    /// **A window over a *stable* ordering starves.** This method shipped
    /// ordered `enqueued_at ASC` on 2026-08-15 and was measured against real SQL
    /// the same day: `domain::state_machine::reconcile_claim` answers
    /// [`Keep`](crate::domain::state_machine::ClaimAction::Keep) for a healthy
    /// claim, `Keep` performs no write, so a healthy claim never leaves
    /// `dispatching`/`running` and never leaves the head of that ordering. One
    /// tenant holding [`MAX_CLAIM_SCAN`] long-running suites returned a
    /// byte-identical id list on every subsequent scan, and every other tenant's
    /// claims were never reconciled at all - so their finished executions never
    /// released a platform and their queues never drained. That is a worse
    /// failure than the unbounded cost the window was added to fix: an
    /// unbounded *cost* degrades everyone equally, an unbounded *latency*
    /// silences one tenant on another tenant's behaviour.
    ///
    /// # The guarantee this ordering does give
    ///
    /// `id` is the sort key and the caller advances `after` past the last row it
    /// saw, restarting at `None` once a window comes back short. Successive
    /// windows therefore sweep the id space in order and wrap.
    ///
    /// **What this method guarantees**: the window is the id-ordered slice
    /// strictly above `after`, and nothing a row *does* changes its id - so no
    /// claim can be excluded by another claim's behaviour.
    ///
    /// **What the caller guarantees**: that every claim is visited at least
    /// once every `ceil(claims / MAX_CLAIM_SCAN)` calls. That is a property of
    /// advancing and wrapping correctly, not of this signature - a caller
    /// passing `None` on every tick reproduces the original starvation exactly,
    /// and nothing here objects. `DispatchService::advance_claim_scan_cursor`
    /// is the implementation, and
    /// `a_scan_window_rotates_so_one_tenants_claims_cannot_starve_another`
    /// covers both halves. Same division `Windowed` makes about its own
    /// `truncated` flag.
    ///
    /// What the guarantee costs, stated rather than left to be discovered: a row
    /// **inserted** with an id below the current cursor waits for the wrap, so
    /// its worst-case wait is one full cycle rather than one tick. Ids are
    /// random (`Uuid::new_v4`), so an insert lands uniformly and the average is
    /// half a cycle. The bound is on *coverage*, not on the freshness of any
    /// individual row.
    ///
    /// This is also why the ordering is no longer "oldest first". It cannot be
    /// both age-ordered and rotating, and rotation is the property that closes a
    /// starvation; age-ordering was only ever a heuristic about which rows are
    /// most likely to need releasing.
    ///
    /// # The second read per tick was left in place deliberately
    ///
    /// An earlier revision suggested the counting pass reuse the classification
    /// reconciliation already built. It could, but it would weaken the count:
    /// admission is N-writer and commits claims inline, so a claim created
    /// between the two reads is counted today and would not be if the first
    /// read's rows were reused.
    ///
    /// **Both reads must be given the same `after`**, and it is the caller's job
    /// to ensure it: this method has no memory. `DispatchService::run_tick`
    /// reads its cursor once and threads it to both, then advances after both
    /// have run. It did not, briefly, and the two scans returned disjoint
    /// windows - which made every `committed_active` lookup miss. Pinned by
    /// `both_claim_scans_in_a_tick_read_the_same_window`.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn all_claims<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        after: Option<Uuid>,
    ) -> Result<Windowed<ClaimAge>, DomainError>;

    /// Expire every `queued` row enqueued before `cutoff`, returning what was
    /// expired so the caller can raise the mandatory alert (`:370-401`).
    ///
    /// Restricted to `state = 'queued'` on purpose: such a row holds no claim,
    /// so expiring it cannot release a platform a live execution still owns.
    /// This is **not** [`Self::fail_orphaned_dispatching`], which is boot-only
    /// and acts on `dispatching` rows; keep the two separate.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn expire_queued_before<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        cutoff: OffsetDateTime,
        reason: &str,
    ) -> Result<Vec<ExpiredRow>, DomainError>;

    /// Fail rows left mid-submit by a restart (`:351-361`).
    ///
    /// **No age predicate, boot-only.** Legacy's `UPDATE ... WHERE state =
    /// 'dispatching' AND workflow_name IS NULL` has none and does not need
    /// one: the rows are those "left mid-submit by a manager restart", and
    /// "with a single replica, that submit is definitively gone". That is true
    /// only at boot. Calling this from a dispatcher tick would fail every
    /// launch that is merely mid-build, and momentarily release its claim —
    /// which is exactly when a second run could be admitted beside an
    /// exclusive one.
    ///
    /// **Takes the ids rather than deriving them.** Legacy's
    /// `workflow_name IS NULL` predicate has no equivalent column here, and
    /// re-deriving it by joining to `qa_runs.execution_ref` would give the
    /// boot rule a second home:
    /// `domain::state_machine::boot_recovery_action` already owns it, and it
    /// is the caller that has both the claim (from [`Self::all_claims`]) and
    /// the run's execution reference. What stays here is the half only SQL can
    /// enforce — `AND state = 'dispatching'`, so a row that started running
    /// between the caller's read and this write is left alone.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn fail_orphaned_dispatching<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        ids: &[Uuid],
        reason: &str,
    ) -> Result<u64, DomainError>;

    /// Rows for the read endpoint, newest first, all states (`:454-483`).
    ///
    /// `environment_id` of `None` spans every platform in the same window, as
    /// legacy's does. Positions, TTL deadlines and blocker text are computed
    /// by the caller over the rows this returns — see
    /// `domain::queue::assign_positions`.
    ///
    /// `limit` is clamped to
    /// [`MAX_QUEUE_READ_LIMIT`](crate::domain::repos::MAX_QUEUE_READ_LIMIT).
    /// The API layer should validate it too, but a repository that will
    /// materialise however many rows a caller names is a denial-of-service
    /// primitive one forgotten validator away, and this is the layer that
    /// actually allocates them.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn list_for_read<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        environment_id: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<QueueRowRecord>, DomainError>;

    /// One page of queue rows, newest first, optionally narrowed to one
    /// platform, filtered and cursored by `query`.
    ///
    /// The paginated sibling of [`Self::list_for_read`], and the endpoint's
    /// read. The two coexist deliberately: `list_for_read` is the *service's*
    /// window — `queue_row_by_id` and `queue_row_for_run` walk it to reach a
    /// row's run — while this one answers a caller who is paging a history and
    /// must not be handed [`MAX_QUEUE_READ_LIMIT`] rows to satisfy a request
    /// for twenty.
    ///
    /// `environment_id` of `None` spans every platform. It is a separate parameter
    /// rather than something the caller expresses through `query`, because the
    /// guide's own remedy for a distorted `queue_position` is the
    /// platform-filtered call and that remedy should not require knowing
    /// `OData`.
    ///
    /// **`queue_position` is not computed here**, and cannot be: it is a
    /// property of the rows a request returned, so it belongs to the caller
    /// that has the whole page — see `domain::queue::assign_positions`, whose
    /// doc carries the consequence that a narrow page inflates every position
    /// toward 1.
    ///
    /// # Errors
    ///
    /// As [`RunsRepository::list_page`].
    async fn list_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        environment_id: Option<Uuid>,
        query: &ODataQuery,
    ) -> Result<Page<QueueRowRecord>, DomainError>;

    /// A row's platform and state (`:426-433`).
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn row_status<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<RowStatus>, DomainError>;
}
