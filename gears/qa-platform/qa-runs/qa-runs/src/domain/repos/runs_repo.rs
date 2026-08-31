//! The run record and its per-test results.

use async_trait::async_trait;
use qa_runs_sdk::{
    ExclusiveTier, Run, RunParameter, RunResult, RunSource, RunState, RunTarget, RunTestResult,
};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::Windowed;

/// How many overdue runs one [`RunsRepository::list_timeout_candidates`] scan
/// will materialise, and therefore how many a single dispatcher tick can
/// reclaim.
///
/// Deliberately an order of magnitude below
/// [`MAX_CLAIM_SCAN`](crate::domain::repos::MAX_CLAIM_SCAN): reclaiming an
/// overdue run is far more expensive than classifying a claim - it cancels the
/// execution, transitions the run and releases the lease - and the sweep runs
/// first in every tick, ahead of the work that actually
/// dispatches queued runs. A window that let one tenant's deadline pileup fill
/// the whole tick would starve every other tenant's queue, which is the failure
/// the window exists to prevent, not one it should reproduce more slowly.
///
/// The window bounds cost per tick; the rotating `id` ordering on
/// [`RunsRepository::list_timeout_candidates`] is what bounds *coverage*, and
/// that method carries the argument for why both are needed.
pub const MAX_TIMEOUT_SWEEP_SCAN: u64 = 100;

/// A run paired with its five outcome counters, as [`RunsRepository::list_page`]
/// returns them.
///
/// Task 10: the run list needs the skip count visible next to the verdict
/// (product owner decision, 2026-08-28 — `domain::state_machine::derive_terminal_state`'s
/// "Skips no longer fail a run" no longer reads `skipped`, so nothing about a
/// `Succeeded` run says whether it asserted anything unless the count travels
/// with it). This is not a second query: `run` and `result` are read off the
/// *same* row — the five counters are denormalized onto `runs`, not a joined
/// table — so pairing them costs nothing beyond what [`RunsRepository::get`]
/// and [`RunsRepository::list`] already fetch and then discard, because
/// [`Run`] itself has no field for the counters.
#[derive(Clone, Debug, PartialEq)]
pub struct RunWithResult {
    pub run: Run,
    pub result: RunResult,
}

/// How many live runs one [`RunsRepository::list_watch_candidates`] scan will
/// materialise, and therefore how many watchers a single dispatcher tick can
/// re-attach.
///
/// The same value as [`MAX_TIMEOUT_SWEEP_SCAN`], and for a related but not
/// identical reason. The per-candidate cost has the same *shape* — one
/// `access_scope` round trip plus one scoped run read — but a different
/// distribution: a candidate whose watcher is already attached is skipped
/// before either, so a leader in steady state pays one windowed read per tick
/// and nothing else, while a leader that has just taken over pays the full
/// per-candidate cost for the whole window. The number is chosen for the
/// second case, which is the one that can hurt.
///
/// The window bounds cost per tick; the rotating `id` ordering on
/// [`RunsRepository::list_watch_candidates`] is what bounds *coverage*, and
/// that method carries the argument for why both are needed here.
pub const MAX_WATCH_SCAN: u64 = 100;

/// A run id that has been resolved under the caller's own access scope.
///
/// This is the structural form of the obligation the schema hands to every
/// writer (`infra/storage/migrations/m20260813_000003_initial.rs`, "The
/// obligation this puts on every writer"): resolve a caller-supplied `run_id`
/// under the caller's scope and return not-found if it is missing, *before*
/// any insert that references it. Done that way the tenant-blind foreign key
/// never gets to answer, and it degrades to what it should be — a last-resort
/// integrity check that fires only on a bug.
///
/// **Why a token rather than a doc comment.** Prose would be a guard nothing
/// enforces, and this subsystem has already shipped several of those. The only
/// way to obtain an `OwnedRunId` is [`RunsRepository::resolve_owned`], a
/// provided method whose body asks [`RunsRepository::get`] and returns
/// [`DomainError::RunNotFound`] when the answer is `None`. So a caller cannot
/// reach [`RunsRepository::upsert_test_result`] or `QueueRepository::insert`
/// without having asked that question, and cannot ask it about one run and
/// write about another, because the token carries the id it verified.
///
/// ## What the token actually proves, and what it does not
///
/// **Corrected 2026-08-13 by the security review.** An earlier version of this
/// paragraph claimed the token could not be minted outside this module,
/// reasoning about an implementor that *overrides* `resolve_owned` — which
/// indeed gains them nothing. That reasoning was true and the conclusion was
/// wrong, because it stopped one method short: the provided body delegates to
/// `get`, which is a **required** method. An implementation whose `get`
/// returns `Some` unconditionally mints tokens freely, in entirely safe Rust,
/// with no override and no access to this private field. The review built such
/// a shim and had the real `insert` accept its token.
///
/// So the honest statement of the guarantee is:
///
/// > **A token proves that *some* `RunsRepository::get` answered `Some` for
/// > this id under this scope. It is exactly as trustworthy as that repository
/// > is.**
///
/// Against the real [`crate::infra::storage::OrmRunsRepository`] that is a
/// scoped `SELECT`, which is the whole guarantee. Against a test double it is
/// whatever the double says — and the realistic risk is not an attacker, it is
/// **the service-layer double Tasks 13-16 will write**: one whose `get`
/// returns `Some` for convenience would let every service test mint tokens and
/// "prove" a precheck it never ran. That is the false-assurance failure this
/// design exists to remove, so it must not be reintroduced by the doubles.
///
/// ## The rule this places on every test double — decide it before writing one
///
/// **A `RunsRepository` double's `get` MUST apply tenant scoping to its
/// fixture**, returning `None` when the scope does not admit the stored row's
/// tenant. One line does it:
///
/// ```ignore
/// use toolkit_security::pep_properties::OWNER_TENANT_ID;
/// if !scope.contains_uuid(OWNER_TENANT_ID, row.tenant_id) {
///     return Ok(None);
/// }
/// ```
///
/// A double that skips this is not a simplification, it is a test that cannot
/// fail. Sealing the trait so only the ORM implementation satisfied it was
/// considered and rejected: it would block the service unit tests these
/// doubles exist for, and the doubles are the thing being constrained here,
/// not the trait.
///
/// The field stays private to this module — `domain::service` and
/// `infra::storage` are both outside it — so a token can never be conjured
/// from nothing, only obtained from *a* `get`. **Do not add a public
/// constructor**; a `from_unchecked` would turn this back into the doc comment
/// it replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnedRunId(Uuid);

impl OwnedRunId {
    /// The verified run id.
    #[must_use]
    pub fn get(self) -> Uuid {
        self.0
    }
}

/// The fields a state transition may also write.
///
/// `None` means "leave the column alone", which is why this is not just three
/// bare values: dispatch records `started_at` and nothing else, while a
/// terminal transition records `finished_at` and possibly `error`. A blind
/// write of all three would clear `started_at` on completion.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunStatePatch {
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub error: Option<String>,
}

/// A run to be created — every field a launch decides, and nothing the
/// repository owns.
///
/// **Why this exists rather than `create(run: Run)`.** Passing the full [`Run`]
/// meant the caller supplied `id`, and `qa_runs.id` is a global primary key, so
/// a probe carrying a victim's id collided and turned `create` into a
/// cross-tenant existence oracle. That was first patched by ignoring `id` and
/// saying so in a doc comment — a guard nothing enforced, in a layer whose
/// whole argument is that such guards do not hold. This type is the enforcing
/// version: the fields the repository owns are **absent**, so no caller can
/// supply one and no reader has to check which are honoured. Same reasoning
/// that produced [`NewTestResult`] and `NewQueueRow`; the question simply was
/// not asked of `create` until it had already produced a finding.
///
/// The five columns that are neither here nor caller-visible each have exactly
/// one writer, which is the property this split buys:
///
/// | Column | Written by |
/// |---|---|
/// | `id`, `created_at`, `updated_at` | the repository, at insert |
/// | `passed`/`failed`/`skipped`/`in_progress`/`total` | [`RunsRepository::add_result_counts`] |
/// | `execution_ref` | [`RunsRepository::set_execution_ref`] |
/// | `started_at`, `finished_at`, `error` | [`RunsRepository::update_state`] via [`RunStatePatch`] |
/// | `log_storage_ref` | **nothing, by decision** — see below |
///
/// `log_storage_ref` used to read "populated on completion with feature 2.7".
/// It is not, and will not be: design D-RLP-6 (2026-08-31) decided it stays
/// `NULL` deliberately. It is declared `VARCHAR(2048)`, published in `RunDto`
/// and shaped like a URI a consumer may fetch, so writing an internal table
/// reference into it would publish a schema detail in a public DTO and invite
/// exactly that misreading. A run's durable log lives in `qa_run_logs`, keyed
/// by `run_id`, reached through `RunLogsRepository` — never through this
/// column. So it has **no** writer rather than a future one, and that is the
/// property this table is asserting.
///
/// [`state`](Self::state) *is* here, because a launch legitimately decides it:
/// an admitted run is created already `Queued`, exactly as an admitted queue
/// row is filed already `dispatching`.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRun {
    /// `{slug}-{n}`, unique within the tenant.
    pub name: String,
    pub target: RunTarget,
    pub platform_id: Option<Uuid>,
    pub test_version: Option<String>,
    /// Snapshotted from the target platform at launch, never re-derived.
    pub app_version: Option<String>,
    pub app_build: Option<String>,
    pub state: RunState,
    pub resolved_exclusive: bool,
    pub exclusive_tier: ExclusiveTier,
    pub is_validation: bool,
    pub parameters: Vec<RunParameter>,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub source: RunSource,
    pub schedule_id: Option<Uuid>,
    pub bundle_ids: Vec<Uuid>,
    pub timeout_at: Option<OffsetDateTime>,
}

/// A signed change to the five denormalized counters.
///
/// **Signed on purpose.** A per-test row moves `PENDING`/`RUNNING` →
/// `PASSED`, which is `in_progress -1, passed +1, total 0` — so the deltas
/// cannot be counts. The source system never faced this: it stores no counters
/// at all and recomputes them per query
/// (`manager/src/routes/plans.rs:188-192`), which is also why the fold
/// `failed <- FAILED, ERROR` is baked into `failed` here rather than given a
/// counter of its own.
///
/// `i64` rather than the column's `i32`: a domain type has no business knowing
/// how wide the column is, and the narrowing is checked in
/// `infra::storage::mapper::db_i32_from_i64`, which rejects an out-of-range
/// delta as a validation failure.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunResultDelta {
    pub passed: i64,
    pub failed: i64,
    pub skipped: i64,
    pub in_progress: i64,
    pub total: i64,
}

/// A run past its control-plane deadline, with the tenant to write under.
///
/// The tenant travels with the id because the sweep enumerates across tenants
/// under one system context and then writes under a **per-tenant** one; a
/// caller that kept only the run id would have nothing to mint the second
/// context from. Named fields rather than `(Uuid, Uuid)` for the reason the
/// source system gives at `manager/src/services/run_queue.rs:136-147`: two
/// same-typed fields transpose silently.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeoutCandidate {
    pub run_id: Uuid,
    pub tenant_id: Uuid,
}

/// A live run that something ought to be observing, with the tenant to observe
/// it under.
///
/// The tenant travels with the id for the reason [`TimeoutCandidate`] gives
/// about its own: the scan enumerates across tenants under one nil-tenant
/// context and every step that follows — the candidate read, and every write
/// the ingest path then makes — runs under a **per-tenant** one, which a caller
/// holding only the run id would have nothing to mint.
///
/// # Why this is not [`TimeoutCandidate`], which has the same two fields
///
/// Deliberate duplication. The two types are structurally identical and mean
/// different things: a `TimeoutCandidate` is *"a run past its control-plane
/// deadline"* and is acted on by cancelling it, while this is a run that is
/// healthy and simply unobserved. A single shared type would let a sweep be
/// handed the other's rows with no diagnostic, and the failure — cancelling a
/// run because a watcher was missing — is not one a `Uuid` pair would show.
///
/// **The `execution_ref` is deliberately absent**, even though the scan's
/// `WHERE` clause is the only reason a row is here at all. It is read back per
/// candidate under that candidate's own tenant-bound scope, so the cross-tenant
/// enumeration returns the minimum that lets the caller bind a tenant — the
/// same shape `TimeoutCandidate` has, and the reason the per-candidate read
/// exists rather than being an avoidable round trip.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchCandidate {
    pub run_id: Uuid,
    pub tenant_id: Uuid,
}

/// One per-test result, as written by ingest.
///
/// `status` is a `String`, not an enum, and that is the deliberate opposite of
/// every other enum-shaped column in this gear. The set is **open**: the
/// source system writes unvalidated runner text into it
/// (`manager/src/routes/runs.rs:1115` → `:1168-1174`) and uppercases any
/// pytest outcome outside its six-way map
/// (`manager/src/services/argo.rs:2932-2943`). A ninth value is a runner
/// change, not corruption, and rejecting it would drop a real result. The
/// eight known values and their mapping onto the five counters live in the
/// migration's column comment, which is their only definitive record.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewTestResult {
    /// `""` when the runner reported no file — never `None`. The column is
    /// `NOT NULL DEFAULT ''` so the dedupe predicate is a plain equality
    /// instead of legacy's `COALESCE(test_file, '')`
    /// (`manager/src/routes/runs.rs:1154`).
    pub test_file: String,
    pub test_name: String,
    pub status: String,
    /// The runner's duration text **verbatim**, including extended forms like
    /// `85.06s (0:01:25)` (`manager/src/services/argo.rs:3279`).
    pub duration: Option<String>,
    pub launch_id: Option<String>,
    pub jira_key: Option<String>,
    /// The pytest node identifier, or `None` from a producer that reports no
    /// per-case detail.
    ///
    /// Collapsed to `""` by the repository, because
    /// `qa_run_test_results.nodeid` is `NOT NULL DEFAULT ''`
    /// (`m20260818_000005_case_fidelity`) for the reason
    /// [`Self::test_file`] gives about its own column. The `Option` is kept on
    /// *this* type rather than collapsed by ingest so that the collapse happens
    /// at exactly one place, the write — the same shape the source system uses,
    /// which holds `nodeid: Option<String>`
    /// (`manager/src/services/argo.rs:2923`) and calls `unwrap_or_default()`
    /// once, at `:2973`.
    ///
    /// A non-empty value means this row is case-level rather than file-level;
    /// [`crate::domain::ports::run_executor::TestObservation::nodeid`] states
    /// that convention in full, including that nothing enforces it.
    pub nodeid: Option<String>,
    /// The xfail/skip explanation, nullable in the column because for a reason
    /// absence is information rather than a spelling choice
    /// (`manager/migrations/001_initial.sql:261`).
    pub reason: Option<String>,
    /// The **case**-level bug reference — not [`Self::jira_key`], which is the
    /// **file**-level one. Two columns, deliberately
    /// (`manager/migrations/001_initial.sql:262` versus `:71`).
    pub ticket: Option<String>,
}

/// A stored per-test result: every column of `qa_run_test_results` except
/// `tenant_id`, which is the scope's business rather than a caller's.
///
/// # It was deliberately narrower until Task 4, and this records why it is not
///
/// Through Tasks 2 and 3 this type omitted `nodeid`, `reason` and `ticket`
/// (`m20260818_000005_case_fidelity`), which [`NewTestResult`] wrote and
/// nothing read back. The omission carried its own expiry date: *"surfacing
/// them belongs with the read methods that would expose them, which is a
/// separate change"*. [`RunsRepository::list_test_results`] is now that read —
/// qa-insights' reconciler consumes it through
/// [`qa_runs_sdk::QaRunsClientV1::list_run_test_results`] and needs all three
/// to rebuild the per-case analytics the event stream dropped — so the three
/// are here, and there is exactly **one** mapper from the row
/// (`infra::storage::mapper::test_result_to_row`) rather than a second,
/// wider one beside it.
///
/// Ingest is unaffected: it still reads only [`Self::status`], to compute a
/// counter delta.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestResultRow {
    pub id: Uuid,
    pub run_id: Uuid,
    pub test_file: String,
    pub test_name: String,
    /// Open set — see [`NewTestResult::status`].
    pub status: String,
    pub duration: Option<String>,
    pub launch_id: Option<String>,
    /// The **file**-level bug reference (`manager/migrations/001_initial.sql:71`),
    /// distinct from [`Self::ticket`].
    pub jira_key: Option<String>,
    /// `""` when the producer reported no node id, never `NULL` — the column is
    /// `NOT NULL DEFAULT ''`. See [`NewTestResult::nodeid`], which is where the
    /// `Option` is collapsed.
    pub nodeid: String,
    /// The xfail/skip explanation (`manager/migrations/001_initial.sql:261`).
    pub reason: Option<String>,
    /// The **case**-level bug reference
    /// (`manager/migrations/001_initial.sql:262`), distinct from
    /// [`Self::jira_key`]. Legacy's analytics renders only this one as
    /// `AnalyticsListItem::case_tickets`
    /// (`manager/src/routes/analytics.rs:132`).
    pub ticket: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Project a stored row onto the SDK's read-only [`RunTestResult`].
///
/// # Why the conversion lives here and not at the client seam
///
/// This is the **only** place the ten contract fields are named together, and
/// naming them together is the point: `RunTestResult` is built from a row by a
/// struct literal, so a dropped field compiles cleanly and produces a
/// plausible-looking answer with one column silently `None`. That is exactly
/// the defect Tasks 2 and 3 each shipped once. Keeping the literal beside the
/// row it reads from puts the two field lists on one screen, and
/// `infra::storage::runs_sea_repo`'s
/// `every_field_of_a_per_test_row_survives_the_trip_to_the_sdk` walks the whole
/// path — database, mapper, this impl — with ten distinct values.
///
/// The four columns not carried across (`id`, `tenant_id`, `created_at`,
/// `updated_at`) are dropped on purpose; [`RunTestResult`] says why.
impl From<TestResultRow> for RunTestResult {
    fn from(row: TestResultRow) -> Self {
        Self {
            run_id: row.run_id,
            test_file: row.test_file,
            test_name: row.test_name,
            status: row.status,
            duration: row.duration,
            launch_id: row.launch_id,
            jira_key: row.jira_key,
            nodeid: row.nodeid,
            reason: row.reason,
            ticket: row.ticket,
        }
    }
}

/// Repository for `qa_runs` and its `qa_run_test_results` children.
#[async_trait]
pub trait RunsRepository: Send + Sync {
    /// Insert a run, returning it with the fields this layer owns filled in.
    ///
    /// [`NewRun`] deliberately has no `id`: `qa_runs.id` is a *global* primary
    /// key, so a caller-supplied one made this method a cross-tenant existence
    /// oracle. See that type.
    ///
    /// # Errors
    ///
    /// [`DomainError::RunNameExists`] when `(tenant_id, name)` collides — that
    /// unique index is tenant-prefixed, so the colliding row is always one
    /// this tenant can itself see. [`DomainError::Database`] otherwise.
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewRun,
    ) -> Result<Run, DomainError>;

    /// Scoped read by id.
    ///
    /// # Errors
    ///
    /// [`DomainError::CorruptState`] if the row does not decode,
    /// [`DomainError::Database`] on a query failure.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Run>, DomainError>;

    /// Every run in scope. `OData` filtering is applied at the API layer, not
    /// here.
    ///
    /// **Uncapped**, where `QueueRepository::list_for_read` clamps its window
    /// at `MAX_QUEUE_READ_LIMIT` and [`Self::list_timeout_candidates`] and
    /// `QueueRepository::all_claims` return a
    /// [`Windowed`](crate::domain::repos::Windowed). The inconsistency is real
    /// and is left deliberately: this method has no `limit` parameter to clamp,
    /// and inventing a silent ceiling here would truncate a reader's page
    /// without it knowing.
    ///
    /// # No endpoint calls this, and one production path still does
    ///
    /// **Updated 2026-08-15 when the paginated read landed.** The obligation
    /// this doc used to place on the API layer - *"do not expose this method
    /// through an endpoint without pagination"* - is discharged:
    /// [`Self::list_page`] is what `GET /qa/v1/runs` reads through, and its
    /// window is the page size.
    ///
    /// That leaves **one** production caller, and pagination was never going to
    /// bound it. `service::launch`'s `create_run` calls this on **every launch**,
    /// up to `NAME_ATTEMPTS` times, purely to compute the next name in a
    /// sequence, and then filters the answer in memory - see the heading *"The
    /// enumeration this does, and the repository method it is missing"* on that
    /// function, which records the same fact from the other side.
    ///
    /// The remedy that call site names is a prefix-filtered read
    /// (`WHERE name LIKE '{base}-%'`, the source system's own query at
    /// `manager/src/services/run_history.rs:452-468`), which is safe because
    /// `domain::naming::next_sequence` re-filters exactly what it is given. It
    /// is not done here: it changes the launch path's naming and uniqueness
    /// behaviour, which is a different review from bounding a dispatcher scan.
    ///
    /// # Errors
    ///
    /// As [`Self::get`].
    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Run>, DomainError>;

    /// One page of runs in scope, newest first, filtered and cursored by
    /// `query`.
    ///
    /// This is the endpoint's read, and the reason [`Self::list`]'s obligation
    /// — *"do not expose this method through an endpoint without pagination"* —
    /// is discharged. The window is bounded by the page size rather than by the
    /// number of runs a tenant has ever launched.
    ///
    /// # The tenancy boundary does not depend on the caller's filter
    ///
    /// The implementation must pass a **scoped** select to
    /// `toolkit_db::odata::sea_orm_filter::paginate_odata`, whose first
    /// parameter is a `SecureSelect<E, Scoped>` and is therefore uncallable on
    /// an unscoped one. Whatever `$filter` the caller supplies is applied on top
    /// of the scope predicate, never instead of it.
    ///
    /// # The advertised fields and the translatable fields are one list
    ///
    /// `infra::storage::odata::RunFilterField` is the allow-list, and the same
    /// type is handed to the route's `with_odata_filter`. A field is either in
    /// both or in neither.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] when the query names a field or operator the
    /// allow-list does not admit, or carries an unusable cursor — those are the
    /// caller's mistakes and must not read as server faults.
    /// [`DomainError::Database`] on a query failure, and
    /// [`DomainError::CorruptState`] if a returned row does not decode.
    ///
    /// # Returns [`RunWithResult`], not [`Run`], since Task 10
    ///
    /// The endpoint this backs is the run list, and a run list that cannot say
    /// whether a `Succeeded` run's `skipped` count is non-zero is exactly the
    /// false-green risk `domain::state_machine::derive_terminal_state`'s "Skips
    /// no longer fail a run" warns about. The pairing is free — see
    /// [`RunWithResult`].
    async fn list_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<RunWithResult>, DomainError>;

    /// Runs in scope whose `finished_at` is at or after `since`, **oldest
    /// first**, at most `limit` of them.
    ///
    /// The reconciler's sweep predicate, and nothing else calls it. qa-insights
    /// ingests run results through this sweep alone: the transactional broker
    /// consumer that would have been its other path never actually ran in any
    /// deployment and was deleted along with the event-broker dependency
    /// (`qa-insights/src/domain/service/reconcile.rs`'s header, "Why this
    /// exists, and why legacy has no counterpart"). This is the query that
    /// finds the runs each pass reads.
    ///
    /// # There is no legacy counterpart, and that is not an omission
    ///
    /// Legacy's analytics reads the run tables **directly** — `sqlx` against
    /// `state.db`, the same pool the run path writes
    /// (`manager/src/routes/analytics.rs:959-1008` and `:2438-2458`, both
    /// `FROM test_results t JOIN run_results r ON t.run_id = r.id`). It needed
    /// no sweep because it had no gear boundary to cross. This method exists
    /// *because* the split forbids that, so there is no `file:line` to cite for
    /// its behaviour and no legacy semantics to preserve — only the ordering
    /// contract below.
    ///
    /// # Oldest-first, with `id` breaking ties, and both halves are load-bearing
    ///
    /// The caller advances a watermark as it consumes the page. Newest-first
    /// would strand the oldest gap forever: every sweep would return the same
    /// recent runs and the watermark would never reach the hole behind them.
    ///
    /// **The `id` tiebreak is not decorative.** Two runs finishing inside the
    /// same clock tick have equal `finished_at`, so without a second sort key
    /// their relative order is whatever the plan happens to produce. A caller
    /// that took `limit = 1` and advanced its watermark past that timestamp
    /// would then skip whichever of the two the database felt like putting
    /// second — a silent, unreproducible data loss. `(finished_at, id)` is
    /// total, so the page boundary falls in the same place every time.
    ///
    /// # A `NULL` `finished_at` is excluded
    ///
    /// A run that has not reached a terminal state has nothing to backfill.
    /// SQL's three-valued logic already excludes it from `finished_at >=
    /// $since`; the implementation states the `IS NOT NULL` conjunct anyway,
    /// because a reader checking this contract against the query should not
    /// have to derive it.
    ///
    /// # `limit` is clamped, not trusted
    ///
    /// Mandatory for the reason [`Self::list`]'s doc gives — `qa_runs` never
    /// drains — and clamped again by the implementation to
    /// `infra::storage::db::PAGE_LIMITS`' ceiling, which is the same bound
    /// every other runs read in this gear allocates under. A caller asking for
    /// a million gets a page.
    ///
    /// # Errors
    ///
    /// As [`Self::get`].
    async fn list_finished_since<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        since: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<Run>, DomainError>;

    /// Resolve `run_id` under the caller's own scope, producing the
    /// [`OwnedRunId`] every write that references a run demands.
    ///
    /// This is the ownership precheck, and it is a **provided** method so that
    /// no implementation can weaken it: the token's field is private to this
    /// module, so an override can only obtain one by calling this body.
    ///
    /// # Errors
    ///
    /// [`DomainError::RunNotFound`] when the run does not exist *or* is not
    /// visible in `scope` — the two must be indistinguishable, since telling
    /// them apart is the cross-tenant existence oracle this method exists to
    /// close.
    async fn resolve_owned<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<OwnedRunId, DomainError> {
        match self.get(runner, scope, run_id).await? {
            Some(_) => Ok(OwnedRunId(run_id)),
            None => Err(DomainError::RunNotFound { id: run_id }),
        }
    }

    /// Move a run from `from` to `to`, applying `patch` in the same statement.
    ///
    /// **Conditional on the current state.** The `WHERE state = $from` guard
    /// is what makes the transition atomic: `false` means someone else moved
    /// the run first, and the service turns that into
    /// [`DomainError::IllegalTransition`]. This is the repository's half of
    /// the state machine and must never become a blind `UPDATE` — two racing
    /// completions would then both "succeed" and the later one would overwrite
    /// the earlier terminal state.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure. A no-match is `Ok(false)`,
    /// not an error: only the caller knows which of several actions it was
    /// attempting.
    async fn update_state<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        from: RunState,
        to: RunState,
        patch: RunStatePatch,
    ) -> Result<bool, DomainError>;

    /// Record the executor's opaque handle.
    ///
    /// Separate from [`Self::update_state`] because dispatch records the
    /// reference *then* transitions: a crash between the two leaves a run that
    /// can be reconciled, whereas the other order leaves a `running` run with
    /// no handle to reconcile against.
    ///
    /// **Unguarded**, like [`Self::add_result_counts`] and unlike
    /// [`Self::update_state`] — the same split `QueueRepository` documents on
    /// its `mark_*` family. These two are written by the dispatcher and the
    /// ingest path about a run whose state the caller already decided, while
    /// `update_state` answers a *transition* and must refuse a run that moved.
    /// The consequence worth knowing: **`add_result_counts` will happily bump
    /// a terminal run's counters**, so a late result event arriving after
    /// completion silently changes a finished run's numbers — and
    /// `derive_terminal_state` reads `failed` (only; `skipped` stopped voting
    /// on the verdict on 2026-08-28), so a recomputation on a late `failed`
    /// delta would then disagree with the recorded verdict. Whether a late event is
    /// dropped belongs to ingest (Tasks 12/15); this layer does not decide it,
    /// and does not pretend to.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn set_execution_ref<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        execution_ref: &str,
    ) -> Result<bool, DomainError>;

    /// Record the bundles this run's execution nodes were built from.
    ///
    /// **Added by Task 14, because rule 5 had nowhere to land.** `bundle_ids`
    /// was written only at insert (`NewRun::bundle_ids`, and
    /// `infra::storage::runs_sea_repo`'s insert model), and the launch path
    /// writes it **empty** on purpose — parity spec §3.4 rules 4 and 5, the
    /// force-sync and the per-group bundle build, belong to dispatch. So the run
    /// row could never record which bundles its nodes ran from, and
    /// `Resolved::clone_new_run`'s own doc says so: *"rule 5's 'each execution
    /// node carrying its own bundle reference' has nowhere to be recorded on the
    /// run row once dispatch builds the bundles."*
    ///
    /// **Not folded into [`Self::set_execution_ref`]**, which was the other
    /// option that doc names. The two writes happen at different instants and
    /// only one of them may be lost: the bundles are known *before* the submit
    /// and the execution reference only *after* it, and the ordering
    /// `set_execution_ref` documents — reference first, then the transition, so a
    /// crash leaves a reconcilable run — depends on that method writing exactly
    /// one column. A combined method would either delay the bundle write until
    /// after the submit, losing it whenever a submit succeeds and the recording
    /// fails, or make `execution_ref` nullable in its own setter, which is the
    /// column-with-two-writers shape the schema removed from the queue table.
    ///
    /// **Order matters relative to the submit, and dispatch calls this first.**
    /// A bundle recorded for a run that never started is a harmless dangling
    /// reference that `expires_at` reclaims (`DESIGN.md` §3.7, "bundle GC is
    /// driven purely by `expires_at`, never by run liveness"); a run that started
    /// against bundles nothing recorded is unreproducible.
    ///
    /// **Unguarded**, like [`Self::set_execution_ref`] and
    /// [`Self::add_result_counts`]: the caller is the dispatcher, writing about a
    /// run whose state it has already decided. The consequence is the same one
    /// those two carry — this will overwrite a terminal run's `bundle_ids` — and
    /// the only caller is `service::dispatch::dispatch_one`, which reaches it
    /// exactly once per submit.
    ///
    /// **Replaces the whole list rather than appending.** One dispatch builds one
    /// bundle per repository group and knows the complete set; an append would
    /// accumulate a re-dispatch's bundles beside the first attempt's with nothing
    /// to say which set the current execution actually used.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure, or
    /// [`DomainError::Validation`] if the list does not serialize.
    async fn set_bundle_ids<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        bundle_ids: &[Uuid],
    ) -> Result<bool, DomainError>;

    /// Apply a signed delta to the five counters, in SQL.
    ///
    /// Incremental and server-side so concurrent result events do not lose
    /// counts. A read-modify-write here would: two events arriving together
    /// both read `passed = 3`, both write `4`, and one result vanishes.
    ///
    /// **Each counter is floored at zero.** A delta that would drive one
    /// negative is clamped in the same statement, not rejected: a negative
    /// counter is the state [`Self::get_result`] classifies as corruption, so
    /// an unclamped write could permanently break a run's result projection
    /// through a supported call. The clamp is silent by design — see
    /// `infra::storage::runs_sea_repo::clamped_increment` for why rejecting
    /// would reopen the race this method exists to close.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn add_result_counts<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        delta: RunResultDelta,
    ) -> Result<bool, DomainError>;

    /// The five counters as the contract reports them.
    ///
    /// # What these numbers guarantee, and when
    ///
    /// **Once the run's completion has committed they equal a tally of the
    /// per-test rows.** `service::ingest`'s `reconcile_counts` corrects them
    /// inside the completion's `SERIALIZABLE` transaction, and
    /// `ingest_races_pg_tests`'
    /// `the_completions_counter_repair_survives_a_producer_committing_beside_it`
    /// pins that a producer committing beside the repair does not skew it — that
    /// test is red under the default isolation level, so the guarantee is the
    /// escalation's and not the arithmetic's.
    ///
    /// **Before that, they are a running total and can be transiently wrong.**
    /// Mid-run this is a live number, not a reconciled one. Two cases survive the
    /// completion as well: a completion whose retry budget was exhausted never
    /// reached the repair, and a run that never completes never gets one.
    ///
    /// The *verdict* does not depend on any of this — `reconcile_counts` derives
    /// it from the rows it tallied, not from these columns.
    ///
    /// # Errors
    ///
    /// [`DomainError::CorruptState`] if a counter is negative.
    async fn get_result<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<RunResult>, DomainError>;

    /// One window of the runs in `dispatching` or `running` whose `timeout_at`
    /// has passed.
    ///
    /// Returns the candidates whose id sorts **after** `after`, in `id` order,
    /// at most [`MAX_TIMEOUT_SWEEP_SCAN`] of them. `None` starts from the
    /// beginning.
    ///
    /// # Windowed for cost, rotating for coverage
    ///
    /// Cross-tenant, with no caller-supplied `limit`, issued once per tick under
    /// a nil-tenant context, and followed by one scoped run read plus one
    /// `access_scope` per candidate - so one tenant's backlog of overdue runs
    /// would otherwise set the per-tick cost for every tenant.
    ///
    /// The ordering is `id`, and **not** `timeout_at`, for the reason
    /// `QueueRepository::all_claims` gives at length: a window over a stable
    /// ordering starves whenever a row can stay in the set indefinitely, and one
    /// can here. `reclaim_overdue` returns without writing when the executor
    /// refuses the cancel - deliberately, because the execution may still be
    /// running - and such a run stays past its deadline, stays in
    /// `dispatching`/`running`, and would stay at the head of `timeout_at ASC`
    /// forever. [`MAX_TIMEOUT_SWEEP_SCAN`] runs in that state would then stop
    /// every other tenant's overdue run from ever being reclaimed.
    ///
    /// With the rotation, every candidate is visited at least once every
    /// `ceil(candidates / MAX_TIMEOUT_SWEEP_SCAN)` calls, **provided the caller
    /// advances `after` past what it saw and restarts on a short window**; the
    /// bound is the caller's, not this method's.
    ///
    /// # What losing the age ordering costs, which is not nothing
    ///
    /// It costs the same wrap-wait `QueueRepository::all_claims` states, and it
    /// matters *more* here. A claim exists from the moment a row is dispatched,
    /// so the claim scan's rotation only delays *reconciling* it. A timeout
    /// candidate materialises the instant `timeout_at` passes, and a newly
    /// overdue run whose id sorts below the cursor waits up to a full cycle
    /// **past its deadline** - which is precisely the latency this sweep exists
    /// to bound.
    ///
    /// The trade is still right: a cycle is seconds at the default cadence,
    /// while the starvation it replaced was unbounded. But an earlier version
    /// of this paragraph said the loss was "ordering within that cycle and
    /// nothing else", which was the optimism that had already been corrected
    /// one file over.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn list_timeout_candidates<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        now: OffsetDateTime,
        after: Option<Uuid>,
    ) -> Result<Windowed<TimeoutCandidate>, DomainError>;

    /// One window of the runs in `dispatching` or `running` that carry an
    /// execution reference.
    ///
    /// Returns the candidates whose id sorts **after** `after`, in `id` order,
    /// at most [`MAX_WATCH_SCAN`] of them. `None` starts from the beginning.
    ///
    /// # What this read is for
    ///
    /// A live `RunExecutor::watch` stream does not survive a process bounce, so
    /// after a restart the runs that were being observed are not. This is the
    /// read that finds them again.
    ///
    /// The two state values are the same pair
    /// [`Self::list_timeout_candidates`] uses and mean the same thing — a run
    /// that holds an execution — while the `execution_ref IS NOT NULL`
    /// predicate is what distinguishes a run there *is* something to watch from
    /// one whose submit never got that far, which is boot recovery's business
    /// and not this pass's.
    ///
    /// **A second source of unobserved runs appears only once a real elector is
    /// deployed**: a run dispatched inline on a replica that is not the
    /// dispatcher would never have been observed at all. Under the shipped
    /// `NoopLeaderElector` there is no such replica — every process believes it
    /// leads — so today the restart case is the whole of it. Written in the
    /// conditional deliberately: the first draft wrote it in the present tense,
    /// which describes a deployment nobody has.
    ///
    /// # The queue is **not** the source, and that is load-bearing
    ///
    /// The obvious alternative — ride the tick's claim reconciliation, which
    /// already walks every claim — cannot work: `AdmissionService` answers
    /// `Admission::Unqueued` for a run with no platform and writes **no queue
    /// row at all**, so a platformless run appears in no claim scan. Those are
    /// exactly the runs that would otherwise stay on the timeout-only path this
    /// read exists to close.
    ///
    /// # Windowed for cost, rotating for coverage — and the starvation is worse
    /// here than for the timeout sweep
    ///
    /// Cross-tenant, with no caller-supplied `limit`, issued once per tick under
    /// a nil-tenant context, and followed by one `access_scope` plus one scoped
    /// run read per candidate the caller is not already watching.
    ///
    /// The ordering is `id`, and **not** `started_at` or `updated_at`, for the
    /// reason `QueueRepository::all_claims` gives at length — and the reason
    /// bites harder here. A timeout candidate leaves the set as soon as it is
    /// reclaimed, and a claim leaves it as soon as it is released; a **healthy
    /// live run stays a candidate for its whole execution**, which
    /// `cpt-cf-qa-nfr-run-duration` puts at up to eight hours. So under any
    /// stable ordering, [`MAX_WATCH_SCAN`] long-running healthy runs would pin
    /// the head of the window permanently and no run behind them would ever be
    /// observed. Rotation is not an optimisation here; without it the pass has a
    /// reachable set of inputs on which it does nothing forever.
    ///
    /// With the rotation, every candidate is visited at least once every
    /// `ceil(candidates / MAX_WATCH_SCAN)` calls, **provided the caller advances
    /// `after` past what it saw and restarts on a short window**; the bound is
    /// the caller's, not this method's.
    ///
    /// # The NFR this window is the allocation for, and what it does not bound
    ///
    /// `cpt-cf-qa-nfr-run-duration` sets the threshold: *"run state and result
    /// ingestion recover within 60 s of control-plane restart"* (`PRD.md:607`),
    /// and `DESIGN.md`'s NFR allocation row assigns it to executor re-attach on
    /// startup. This scan is that re-attach.
    ///
    /// **It does not meet the threshold for an arbitrary fleet, and the
    /// arithmetic is stated rather than left implicit.** A cold leader starts
    /// with no cursor, so a run at position `N` in the id ordering is reached
    /// after `ceil(N / MAX_WATCH_SCAN)` scans, i.e. after roughly that many
    /// dispatcher intervals. At the shipped 5 s cadence the 60 s budget buys
    /// about a dozen scans, so the threshold holds up to roughly a *thousand*
    /// concurrently live runs and is exceeded above it — linearly, and with no
    /// error anywhere. (Roughly, because the first interval fires immediately;
    /// the boundary is not worth stating to the run.)
    ///
    /// Nothing here caps the fleet: `max_concurrent_runs` would, and
    /// [`crate::domain::queue::global_cap_status`] records `0` — no cap — as the
    /// shipped default. So the honest statement is that
    /// [`MAX_WATCH_SCAN`] bounds per-tick **cost** and the interval bounds
    /// **latency**, and the pair meets the NFR only below a fleet size neither
    /// of them enforces. Raising the constant or lowering the interval both
    /// move it; which to move is a deployment question, not one this method can
    /// answer.
    ///
    /// # What a rotating window costs *this* pass
    ///
    /// The other two rotating scans cost latency on work that is already late.
    /// This one can cost **observations**: a run started on a replica that is
    /// not the dispatcher, or started before a restart, is unobserved until the
    /// rotation reaches it, and everything the executor emitted in that window
    /// is recovered only if `RunExecutor::watch` genuinely resumes. The port
    /// requires exactly that — *"called again after a control-plane restart it
    /// must resume observation of a still-running execution rather than error"*
    /// — so the cost is zero against a conforming adapter and total against one
    /// that replays nothing. **This crate cannot tell which it has**:
    /// `MockRunExecutor` satisfies the contract by replaying from the
    /// beginning, which is strictly stronger than resuming, so no test here can
    /// falsify a real adapter that drops the gap.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn list_watch_candidates<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        after: Option<Uuid>,
    ) -> Result<Windowed<WatchCandidate>, DomainError>;

    /// Replace this run's row for `(test_file, test_name)`.
    ///
    /// Delete-then-insert, matching the source system
    /// (`manager/src/routes/runs.rs:1153-1185`), because the dedupe tuple
    /// carries no unique index — see the migration for the two independent
    /// reasons. Pass a transaction runner to make the pair atomic.
    ///
    /// `results_scope` governs `qa_run_test_results`; `run` was resolved under
    /// the *runs* scope by [`Self::resolve_owned`].
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn upsert_test_result<C: DBRunner>(
        &self,
        runner: &C,
        results_scope: &AccessScope,
        tenant_id: Uuid,
        run: OwnedRunId,
        result: NewTestResult,
    ) -> Result<TestResultRow, DomainError>;

    /// This run's per-test rows, ordered `(test_file, test_name)`.
    ///
    /// Two readers, one query: the run-detail response, and — through
    /// [`RunTestResult`] — qa-insights' reconciler. The second is why
    /// [`TestResultRow`] gained `nodeid`, `reason` and `ticket`; that type
    /// records the change.
    ///
    /// **Unbounded on purpose.** The row count is bounded by the run's own test
    /// count, which the launch path already caps, so there is no window here to
    /// page through. A caller wanting a page wants the `OData` collection.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] on a query failure.
    async fn list_test_results<C: DBRunner>(
        &self,
        runner: &C,
        results_scope: &AccessScope,
        run: OwnedRunId,
    ) -> Result<Vec<TestResultRow>, DomainError>;
}
