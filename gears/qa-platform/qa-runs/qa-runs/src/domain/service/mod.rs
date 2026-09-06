//! Domain service layer — the composition tier.
//!
//! The four pure cores decide, the repositories persist, and the `RunExecutor`
//! port receives; this layer is what puts them in order. Per-resource
//! submodules mirror the repository layer:
//!
//! - `launch` — launch validation, branch resolution, repository grouping,
//!   exclusivity resolution, and the six-step launch contract (parity spec
//!   §3.4). Task 13.
//! - `admission` / `dispatch` — the per-platform admission lock and the
//!   dispatcher tick. Task 14.
//! - `ingest` / `runs` — result ingestion, cancel, re-run, queue operator
//!   actions. Task 15.
//! - `schedules` — schedule CRUD and the exactly-once firing tick. Task 19.
//!
//! ## Security
//!
//! All operations use the `AuthZ` Resolver PEP (Policy Enforcement Point)
//! pattern via [`PolicyEnforcer`](authz_resolver_sdk::PolicyEnforcer):
//!
//! 1. Construct a `PolicyEnforcer` once (during init) — it serves every
//!    resource type.
//! 2. Call `enforcer.access_scope(&ctx, &resource, action, resource_id)`.
//! 3. The enforcer builds the request, evaluates via the PDP, and compiles the
//!    returned constraints into an `AccessScope`.
//! 4. Pass the scope to repository methods for tenant-isolated queries.
//!
//! **One scope per resource type.** A scope compiled for resource type A is
//! never reused on a query against type B's table — see `domain::repos`, "One
//! scope per resource type". Every precondition read derives its own dedicated
//! scope, and a fresh scope is derived before **every** repository call rather
//! than once per operation, so an added call cannot inherit a scope that was
//! compiled for something else.
//!
//! **One exception, named rather than hidden.** The six nil-tenant
//! enumeration reads `domain::system_actor`'s header lists are handed to
//! `domain::elevated::enumeration_scope`, which returns
//! `AccessScope::allow_all()` directly instead of deriving one from a PEP
//! decision — the one place in this crate's production paths that call
//! appears, and the only place it is meant to. Every other code path, and
//! every write those six reads feed, still derives its scope exactly as
//! described above.
//!
//! ## Cross-gear reads are authorized on the far side
//!
//! The launch path reads a platform (qa-environments) and a plan or custom plan
//! (qa-catalog) through their SDK clients, carrying the caller's own
//! `SecurityContext`. Tenancy is enforced by those gears' PEPs. This layer adds
//! **no local existence probe** on top: a probe that answered "exists but
//! forbidden" differently from "does not exist" is exactly the cross-tenant
//! oracle qa-catalog's review found, and the SDK call already fails closed.
//!
//! ## Connection management
//!
//! Services acquire database connections internally via `DBProvider`. Callers
//! do not touch database objects — they call service methods with business
//! parameters only.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::pep::ResourceType;
use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use qa_catalog_sdk::QaCatalogClientV1;
use qa_environments_sdk::QaEnvironmentsClientV1;
use toolkit_db::DBProvider;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::RunsRepository;
use crate::domain::system_actor;

/// `pub(crate)` rather than private-plus-re-export (qa-catalog's idiom): Tasks
/// 14 and 15 name [`Admission`](launch::Admission) and the two seam traits, and
/// a `pub(crate) use` of a name *this* module never mentions is an unused
/// import until the task that needs it lands.
pub(crate) mod launch;

pub(crate) mod admission;
pub(crate) mod dispatch;
/// The run -> `RunSpec` half of dispatch — see that module's header for why the
/// tick and the translation live in two files and the tick was not split.
pub(crate) mod dispatch_spec;
/// Result ingestion.
///
/// **The module-scope `#[allow(dead_code)]` is gone**, and its removal is the
/// point of Task 16c. It was there because nothing called
/// [`ingest::IngestService::apply`] and almost every item in the module was
/// reachable only through it; `service::watch` now drains an execution's events
/// into `ingest`, so every one of them is reachable from a non-test build and
/// the lint is live again. Restoring the attribute would restore exactly the
/// blindness that let the gap survive the whole of Phase A.
pub(crate) mod ingest;
pub(crate) mod runs;
/// Schedule CRUD and the exactly-once firing tick. Phase B, Task 19.
pub(crate) mod schedules;
/// What drives [`ingest`] — see that module's header for what was missing.
pub(crate) mod watch;

/// Tenant isolation against a real database, rather than against doubles that
/// choose to honour the scope they are handed.
#[cfg(test)]
#[path = "tenant_scoping_tests.rs"]
mod tenant_scoping_tests;

#[cfg(test)]
pub(crate) mod test_support;

/// The golden `RunSpec` — product-plugin plan Task 16's hard gate. In-lib
/// rather than in `tests/`, because `build_spec` is private on a `pub(crate)`
/// service and every double it needs is `#[cfg(test)]`; see that module's
/// header.
#[cfg(test)]
#[path = "golden_run_spec_tests.rs"]
mod golden_run_spec_tests;

/// The ingest races only two database connections can reach. Gated on the
/// `integration` feature so a default `cargo test` needs no Docker.
#[cfg(all(test, feature = "integration"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod ingest_races_pg_tests;

/// Where a live log line goes.
///
/// # Why it lives here rather than beside a consumer
///
/// **Corrected 2026-08-14 by the code-quality review.** It was declared in
/// `service::ingest` and justified as *"a seam declared beside its consumer,
/// exactly as `service::launch` declares `Admitter`"*. That analogy stopped
/// holding inside the same task: `Admitter` has one consumer, while this trait
/// now has **three services plus [`ServiceDeps`]**, and two of them were reaching
/// it as `super::ingest::LogFanout` — naming the ingest module to obtain a trait
/// that is no longer ingest's.
///
/// So it sits in the composition tier, beside the [`ServiceDeps::logs`] field
/// that carries it and the container that wires it. The reason it is a trait at
/// all is unchanged and is the real one: the implementation is
/// `infra::logs::RunLogBroadcaster`, which holds live channel handles, and no
/// domain signature may name an infrastructure type.
///
/// **Both methods are synchronous and infallible, which is the contract rather
/// than an implementation detail.** The run path must never block on, or fail
/// because of, a log consumer: there is no `Result` to swallow because there is
/// nothing an implementor may report. An implementation that needed to await
/// would have to buffer instead.
pub trait LogFanout: Send + Sync {
    /// Fan one line out to `run_id`'s current subscribers. Dropped if nobody is
    /// listening.
    fn publish(&self, run_id: Uuid, line: String);

    /// Release `run_id`'s channel, ending every live subscription.
    ///
    /// # Called from every service that records a terminal state
    ///
    /// **Named rather than counted**, because a count in a doc comment is a claim
    /// this project has been bitten by twice — and the previous revision of this
    /// list was wrong at both ends, saying "five" over six items while omitting a
    /// seventh:
    ///
    /// * `service::ingest::finish`, once, after its transaction commits — on the
    ///   recorded and the already-recorded branch alike, because a `Finished`
    ///   event is evidence the execution ended either way;
    /// * `service::runs::retire`, which both cancels share;
    /// * `service::dispatch::transition`, covering its failed submit, TTL expiry,
    ///   control-plane timeout and orphan recovery;
    /// * `service::launch::transition`, covering a refused launch's `abandon`.
    ///
    /// **This is a convention across the sites named above, not a structural
    /// guarantee.** An earlier revision claimed the `dispatch` choke point meant
    /// "a fifth cannot forget it"; the fifth path already existed in
    /// `service::launch`, and the reap there was not forgotten but *unreachable*,
    /// because `LaunchService` had no field to call it through. A sixth service
    /// can still forget, and nothing in the type system says otherwise.
    fn reap(&self, run_id: Uuid);
}

/// What a flush pass did, for the dispatcher tick's log line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushReport {
    pub runs: usize,
    pub lines: u64,
    pub failed: usize,
}

/// Accumulate a run's log lines and write them to durable storage.
///
/// # Why this is not part of `LogFanout`
///
/// `LogFanout` is a fire-and-forget fan-out port whose two methods are
/// synchronous, and `reap` is called from four services that have no business
/// acquiring a database transaction. Keeping the archive separate keeps each
/// port's purpose answerable in one sentence.
///
/// # Why there is no `read`, and the one method that is not one
///
/// The read path needs the repository, not the accumulator — the same reason
/// `LogFanout` deliberately has no `subscribe`. [`Self::resume_positions`]
/// (Task 13, Finding #50) does not reopen that: it answers counts and a
/// timestamp, never `text` — the same restraint [`crate::domain::repos::
/// ArchivedLog`]'s hand-written `Debug` argues for, applied to the signature
/// instead. The repository round trip this section is really about — a
/// connection and a resolved `AccessScope` — still happens on
/// [`crate::infra::logs::RunLogArchive`], mirroring how [`Self::flush`]
/// resolves both, not on this trait.
///
/// # One `flush` per run at a time — the hazard, and who closes it
///
/// Two overlapping `flush` calls for the **same** `run_id` are not safe unless
/// something closes the gap: naively, each would take an independent,
/// disjoint slice of whatever is buffered at the moment it runs, and issue an
/// independent write, so whichever write commits last wins regardless of
/// which slice was chronologically newer — a lost-update race, not a crash.
///
/// **This trait does not enforce single-flight per run — that is an
/// implementation's obligation, not a structural guarantee of the port.**
/// [`crate::infra::logs::RunLogArchive`] closes it internally: its `take`
/// refuses a second concurrent taker for a `run_id` already being flushed,
/// making a second overlapping `flush` call a no-op rather than a second,
/// disjoint slice — see that type's own module doc and `take`'s doc for the
/// mechanism, and
/// `infra::logs::archive::tests::a_second_concurrent_flush_for_the_same_run_is_a_no_op`
/// for the regression this closes. Concretely, that closed the race
/// [`crate::domain::service::ingest::IngestService::finish`] introduced by
/// calling `flush` beside the periodic tick's `flush_due` — the two now
/// genuinely can run concurrently for one run, and callers of `RunLogArchive`
/// need not serialize around that themselves.
///
/// **A different implementor of this trait is not covered by that internal
/// guarantee and starts from the hazard above.** Nothing in this trait's
/// signature stops a future `impl LogArchive` from taking disjoint slices the
/// naive way; such an implementor must either reproduce `RunLogArchive`'s
/// per-run single-flight internally, or document that its callers must
/// serialize `flush` per run themselves — the obligation this section used to
/// place on every caller, before `RunLogArchive` took it on instead.
#[async_trait]
pub trait LogArchive: Send + Sync {
    /// Buffer one line for `run_id`. Synchronous and infallible, like
    /// `LogFanout::publish`, so the per-line ingest path never awaits.
    ///
    /// `tenant_id` is carried on the buffer rather than looked up at flush
    /// time: `fan_out_log` already runs under a tenant-bound context, so the
    /// flush needs no read to learn where the row belongs.
    fn record(&self, tenant_id: Uuid, run_id: Uuid, line: &str);

    /// Drain `run_id`'s buffer into its row.
    ///
    /// On error the drained text is **put back**, so a transient database
    /// failure delays a write instead of losing a log.
    ///
    /// See this trait's header: two concurrent `flush` calls for the same
    /// `run_id` race rather than compose.
    async fn flush(&self, run_id: Uuid) -> Result<(), DomainError>;

    /// Drain every buffer that has pending text. Never returns `Err`: one
    /// run's failure must not stop the others, so failures are counted in the
    /// report and logged.
    async fn flush_due(&self) -> FlushReport;

    /// Where `run_id`'s archive currently ends, per node. See this trait's
    /// header for why this exists despite "no read", and
    /// [`crate::domain::repos::LogResume`] for what the answer means.
    ///
    /// `tenant` rather than a raw `Uuid`: the implementation resolves its own
    /// [`AccessScope`](toolkit_security::AccessScope) for the read, the same
    /// way [`Self::flush`]'s `write` half does, and a nil tenant must not
    /// reach that resolution any more than it may reach a write — see
    /// `domain::system_actor::TenantBound`'s own doc.
    ///
    /// `domain::service::watch`'s `drain` is the one caller, and treats a
    /// failure here as "resume position unknown" rather than as a reason to
    /// abandon the attach: it falls back to
    /// [`crate::domain::repos::LogResume::default`], which is exactly what a
    /// first attach already does, so a transient failure degrades to the old
    /// replay-from-the-beginning behaviour rather than blocking observation.
    async fn resume_positions(
        &self,
        tenant: system_actor::TenantBound,
        run_id: Uuid,
    ) -> Result<crate::domain::repos::LogResume, DomainError>;
}

/// `DB` provider alias.
///
/// Parameterized with [`DomainError`] directly, like qa-catalog's and unlike
/// qa-environments': the launch and dispatch flows are multi-statement, so a
/// `transaction(...)` closure runs repository calls (which return
/// `DomainError`) as-is and any `Err` rolls the transaction back while
/// preserving the domain variant instead of flattening it to a database error.
pub(crate) type DbProvider = DBProvider<DomainError>;

/// The `SERIALIZABLE`-only provider wrapper.
///
/// A module of its own, and that placement is the mechanism rather than tidiness:
/// a private field is visible to the defining module **and all its
/// descendants**, so a wrapper declared here would leave `self.db.0` reachable
/// from every service in this tree — which is exactly the call it exists to
/// forbid. As a sibling, its field is out of reach.
mod serialized_db;

pub(in crate::domain::service) use serialized_db::SerializedDb;

/// Authorization resource types and their PEP-supported properties.
///
/// **Only the types this crate currently queries are declared.** The plan lists
/// three — `qa.run`, `qa.queue_entry` and `qa.schedule` — and an unused
/// `pub(crate)` constant is dead code under this workspace's `-D warnings`, so
/// each arrived with its first query: Task 14 added [`QUEUE_ENTRY`] with the
/// first `qa_run_queue` query, and [`SCHEDULE`] arrived with the schedule
/// service, which is the first thing to compile a scope for one.
///
/// **Corrected here.** This sentence predicted that Task 17 would add
/// `SCHEDULE`. It correctly did not: Task 17 shipped `SchedulesRepository`,
/// which takes an `AccessScope` its caller builds and calls no
/// `PolicyEnforcer`, so the constant would have been dead code on arrival.
///
/// **A scope compiled for one of these is never passed to a query against the
/// other's table.** `qa_runs` and `qa_run_queue` are two tables with two
/// resource types, and the admission path touches both in one operation
/// (`resolve_owned` on `qa.run`, then `insert` on `qa.queue_entry`), which is
/// exactly the place a hoisted scope would be reused across types. Both scopes
/// are derived immediately before their own call.
pub(crate) mod resources {
    use super::ResourceType;
    use toolkit_security::pep_properties;

    pub const RUN: ResourceType = ResourceType::from_static(
        "qa.run",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// `qa_run_queue`. The same two properties as [`RUN`]: rows are
    /// tenant-owned and addressed by id.
    pub const QUEUE_ENTRY: ResourceType = ResourceType::from_static(
        "qa.queue_entry",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// `qa_schedules` **and** `qa_schedule_ticks`.
    ///
    /// One resource type over two tables, which is `domain::repos::schedules_repo`'s
    /// own decision and not a relaxation of the one-scope-per-resource-type
    /// rule: a tick row has no independent lifecycle, is reaped by its
    /// schedule's `ON DELETE CASCADE`, and is never a resource a caller
    /// addresses. Every method on that repository takes exactly one scope, and
    /// this is the type it is compiled for.
    pub const SCHEDULE: ResourceType = ResourceType::from_static(
        "qa.schedule",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );
}

/// Authorization actions.
///
/// The plan's list is `create`, `get`, `list`, `cancel`, `rerun`, `dispatch`,
/// `force_start`, and every one of them is declared: Task 13 shipped the first
/// three, Task 14 added [`DISPATCH`], and Task 15 added [`CANCEL`], [`RERUN`]
/// and [`FORCE_START`].
///
/// **[`UPDATE`], [`DELETE`] and [`FIRE`] are not on that list, and they are
/// here anyway.** The plan's list was written for the runs and queue surfaces;
/// Phase B added a third resource type with a full CRUD surface and a
/// background writer, and this module's own rule is that the PEP is asked for
/// *the action actually being performed*. Replacing a schedule under `create`,
/// or deleting one under `cancel`, would ask the PDP about something the code
/// is not doing and would take away a deployment's ability to grant
/// read-and-edit without grant-delete. Called out rather than slipped in, the
/// way [`RunsRepository`]-adjacent additions have been before.
///
/// **Corrected 2026-08-14 by Task 15.** This paragraph attributed `force_start`
/// to Task 14 — *"the four this task does not use are added by the tasks that do
/// (14: `dispatch`, `force_start`; 15: `cancel`, `rerun`)"*. Task 14 added only
/// `DISPATCH`, and force start is a queue **operator action** that the plan's own
/// Task 15 Step 6 lists beside `cancel` and `rerun`, so the constant it named was
/// still missing when this task arrived. The rule the paragraph was stating —
/// declare a constant only when something queries under it — held; the roster
/// beside it did not.
///
/// **The launch path's state transitions run under [`CREATE`].** A launch
/// creates a run and then records the admission outcome on it
/// (`Created -> Queued` or `Created -> Dispatching`), and both writes belong to
/// the one operation the caller was authorized for. Giving the transition its
/// own action would let a policy grant "create a run" while denying "record
/// what happened to it", which is not a decision any policy should have to make
/// and would leave a run stranded in `Created`.
///
/// **One launch needs all three of these actions, and that coupling is stated
/// rather than discovered.** Besides `create`:
///
/// * [`LIST`] — `create_run`'s name-sequence read enumerates the tenant's runs so
///   `naming::next_sequence` can continue the prefix. **A policy that grants
///   `create` and denies `list` therefore fails every launch with `Forbidden`**,
///   before any row is written. That is the fail-closed direction and it is
///   deliberate, but it is a real coupling and nothing said so.
/// * [`GET`] — `dispatch_and_report` re-reads the run after dispatch, because
///   `LaunchOutcome::Started` carries a whole `Run` and dispatch is what writes
///   `execution_ref`. Justified at that call site too.
///
/// **Why `LIST` is not folded into `CREATE`** (the alternative, since the same
/// stranding argument would apply): this module's own rule is that the PEP is
/// asked for *the action actually about to be performed*, and that read is an
/// enumeration of existing rows, not a creation. Compiling it as `create` would
/// ask the PDP about an action the code is not performing, and would deny a
/// deployment the ability to grant read-only listing as its own grant. The
/// coupling is the price of asking honestly.
///
/// # What a deployment must actually grant, written out
///
/// Recorded here because it is a deployment obligation with no home in code and
/// the reference dev stack cannot satisfy it - see the `qa-runs:` block in
/// `config/qa-platform.yaml`, where the dispatcher is switched off for exactly
/// this reason.
///
/// The `qa_runs.system` subject (`subject_type=qa_runs.system`) needs:
///
/// * `qa.run` / [`GET`], [`LIST`], [`DISPATCH`], [`CANCEL`], [`RERUN`]
/// * `qa.queue_entry` / [`GET`], [`LIST`], [`DISPATCH`], [`CANCEL`],
///   [`FORCE_START`]
/// * `qa.schedule` / [`LIST`], [`GET`], [`FIRE`] — the firing ticker. It also
///   needs `qa.run`/[`CREATE`], because a fire goes through the same
///   `LaunchService::launch` a manual launch does and that path derives its own
///   run scopes.
///
/// **And the two halves pull opposite ways, which is the hard part.** The
/// per-tenant writes need a *narrowing* constraint - `owner_tenant_id IN
/// [that tenant]` - because each is issued under a tenant-bound context and
/// must not reach another tenant's rows. The nil-tenant enumerations
/// (`for_claim_reconciliation`, `for_ttl_sweep`, `for_timeout_sweep`,
/// `for_watch_scan`) need a
/// *covering* set, because their whole purpose is to see every tenant's rows in
/// one statement. A policy that expresses only the first makes the sweeps
/// return nothing; one that expresses only the second hands every tenant-bound
/// write a cross-tenant scope. Both must be expressible for the same subject,
/// keyed on the context's `subject_tenant_id`.
///
/// Nothing in this crate can check that a deployment got it right. What it does
/// instead is fail closed and say so once per pass - see
/// `dispatch::TickReport::note_failure`.
///
/// # The dispatcher tick asks for **one** action, and that is a decision
///
/// [`DISPATCH`] is the action every write the dispatcher tick makes is
/// authorized under — on both resource types: claiming a queue row, recording
/// an execution reference, moving a run to `running`, expiring a queued row,
/// releasing a reconciled claim, and cancelling a run past its deadline.
///
/// The alternative was one action per step (`dispatch`, `expire`, `reconcile`,
/// `timeout`). It was rejected because a policy could then grant a *partial*
/// tick — claim rows but never expire them, or start runs but never release
/// finished claims — and every one of those partial grants produces the failure
/// the queue's own design warns about most loudly: rows that nothing will ever
/// move, behind which strict FIFO blocks every later launch on the platform.
/// A deployment that wants the dispatcher off turns the dispatcher off; it does
/// not express that by withholding one of five scopes. This is the same
/// argument [`CREATE`] already makes for the launch path's transitions, and it
/// is a genuine cost: the grant is coarser than the operations it covers.
///
/// **Reads are still asked for honestly.** Enumerations are [`LIST`] and
/// single-row reads are [`GET`], because those are the actions being performed
/// — see the note on why `LIST` is not folded into `CREATE`.
///
/// **Task 16 owes the deployment note**: the `qa_runs.system` subject needs
/// `qa.run`/{`get`,`list`,`dispatch`} and
/// `qa.queue_entry`/{`get`,`list`,`dispatch`} for the tick to do anything at
/// all. See `service::dispatch`'s header on what happens when it does not have
/// them.
pub(crate) mod actions {
    pub const CREATE: &str = "create";
    pub const GET: &str = "get";
    pub const LIST: &str = "list";
    /// Every write the dispatcher tick makes — see this module's doc comment
    /// for why it is one action rather than five.
    pub const DISPATCH: &str = "dispatch";

    /// An operator stopping a run, and dropping the queue row behind it.
    ///
    /// **One action across both resource types, and it is not the same argument
    /// [`DISPATCH`] makes.** A run-level cancel writes `qa_runs.state` and, for a
    /// run that has not started, `qa_run_queue.state` — two rows, one request,
    /// and a policy that granted one without the other would leave either a
    /// cancelled run whose row still holds a platform or a dropped row whose run
    /// never leaves `queued`. Both are the lost-work failure this queue's design
    /// warns about loudest.
    ///
    /// **A cancel is deliberately not authorized under [`CREATE`].**
    /// `service::launch`'s action-widening argument is scoped to the
    /// admission-refusal path, where one launch records the outcome of its own
    /// admission; an operator cancel is a different request by a different
    /// principal about a run they did not necessarily create.
    pub const CANCEL: &str = "cancel";

    /// Reading a stored run in order to launch it again.
    ///
    /// **The launch itself is still [`CREATE`].** This action authorizes "may you
    /// re-run *this* run", and `LaunchService::launch` then derives its own
    /// `create`/`list`/`get` scopes for the run it creates — so a policy granting
    /// `rerun` while denying `create` fails the re-run with `Forbidden` before any
    /// row is written. That is the fail-closed direction and it is the same real
    /// coupling this module already records for `create` and `list`.
    pub const RERUN: &str = "rerun";

    /// An operator starting a queued row now, on `qa.queue_entry` only.
    ///
    /// **Its own action rather than [`DISPATCH`], because it is the one write in
    /// this gear that overrides a safety decision.** Force start bypasses the
    /// platform's occupancy — including an in-flight exclusive run — while
    /// leaving `max_concurrent_runs` enforced (guide lines 116-120). A deployment
    /// that wants the dispatcher to drain the queue but does not want operators
    /// overriding exclusivity by hand can express exactly that by granting
    /// `dispatch` and withholding this, which folding the two together would take
    /// away.
    ///
    /// The **submit** that follows runs under [`DISPATCH`], because it goes
    /// through the same `InlineDispatcher` seam every other dispatch uses.
    pub const FORCE_START: &str = "force_start";

    /// Replacing a schedule's caller-decidable fields.
    ///
    /// Its own action rather than [`CREATE`], because an edit and a creation
    /// are different authorities over an existing row: a policy that grants
    /// "may add schedules" should not thereby grant "may repoint an existing
    /// one at another plan".
    pub const UPDATE: &str = "update";

    /// Removing a schedule, and with it every tick row that records what it
    /// fired.
    ///
    /// Separate from [`UPDATE`] because it is the one schedule write that
    /// destroys history: `ON DELETE CASCADE` takes the tick rows, which are the
    /// only durable record that a due time was ever claimed.
    pub const DELETE: &str = "delete";

    /// Every write the schedule firing tick makes on `qa.schedule` — claiming a
    /// due tick, recording its outcome, and advancing the fired-through cursor.
    ///
    /// **One action for three writes, on exactly the argument [`DISPATCH`]
    /// makes for the dispatcher tick.** A policy granting the claim while
    /// withholding the cursor advance produces a schedule that claims a due
    /// time, launches, and then recomputes that same due time on every tick
    /// until the next occurrence — losing the claim it holds itself and
    /// reporting each loss as another instance's win. A deployment that wants
    /// schedules not to fire disables the scheduler; it does not express that by
    /// withholding one of three scopes.
    ///
    /// **Bounded by the occurrence, not permanent**, and an earlier revision of
    /// this paragraph said "forever". It is wrong for the reason
    /// `domain::cron::next_due` exists: the answer is the *most recent*
    /// occurrence and never an outstanding older one, so the next occurrence is a
    /// different `due_at` and claims cleanly. That is the same interlock
    /// `service::schedules`' `advance_cursor` states, and
    /// `service::schedules::tests::a_failover_mid_fire_does_not_produce_a_second_run`
    /// asserts it against an unmoved cursor.
    ///
    /// Reads on the firing path are still asked for honestly: the enumeration is
    /// [`LIST`] and the ownership resolve is [`GET`], because those are the
    /// actions being performed. A deployment must therefore grant all three to
    /// this gear's system subject — the same real coupling [`CREATE`] and
    /// [`LIST`] already have on the launch path.
    pub const FIRE: &str = "fire";
}

/// The three capacity settings the queue enforces, as named fields.
///
/// A struct rather than three parameters for the reason
/// [`crate::domain::queue::GlobalCap`] gives about its own two: `u32`, `u32`,
/// `u64` in a row is a transposition the compiler catches only between the two
/// widths, and `queue_max_depth`/`max_concurrent_runs` are both `u32` and both
/// "a number of runs". Swapping them turns a per-platform depth limit into a
/// cluster-wide cap and answers the wrong 429.
///
/// Every value is `0`-means-disabled, which is the frozen guide's convention
/// for all three (guide lines 125-135) and the convention
/// [`crate::domain::queue::depth_limit`], `global_cap_status` and `expiry_cutoff`
/// already implement. Nothing here re-interprets `0`; the pure helpers do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QueueLimits {
    /// `queue_max_depth`, enforced per (access scope, platform) rather than per
    /// platform — see [`crate::domain::error::DomainError::QueueFull`].
    /// `0` = unlimited.
    pub(crate) queue_max_depth: u32,
    /// Cluster-wide `max_concurrent_runs`. `0` = unlimited.
    pub(crate) max_concurrent_runs: u32,
    /// `queue_ttl_seconds` for the TTL sweep. `0` = never expire.
    pub(crate) queue_ttl_seconds: u64,
}

/// Everything [`AppServices::new`] needs beyond the repositories: shared
/// infrastructure handles plus the typed config values the services enforce.
pub(crate) struct ServiceDeps {
    pub(crate) db: Arc<DbProvider>,
    pub(crate) authz: Arc<dyn AuthZResolverClient>,
    pub(crate) catalog: Arc<dyn QaCatalogClientV1>,
    pub(crate) environments: Arc<dyn QaEnvironmentsClientV1>,
    /// The product-plugin resolver, consumed by [`dispatch::DispatchService`]
    /// alone: it is what `build_spec` asks how to reach a run's target
    /// environment. A **required** field for
    /// [`crate::infra::product_plugin::HubProductPluginResolver`]'s reason —
    /// this module never names `crate::infra`, so there is no production
    /// default it could build.
    pub(crate) product_plugins: Arc<dyn crate::domain::ports::product_plugin::ProductPluginPort>,
    /// The execution plane. Consumed by [`dispatch::DispatchService`] for
    /// `start`/`cancel`/`list_active`, and by
    /// [`admission::AdmissionService`] for the one `list_active` the global cap
    /// needs — which is why it is a field on this struct rather than on the
    /// dispatch service alone.
    pub(crate) executor: Arc<dyn crate::domain::ports::run_executor::RunExecutor>,
    /// Where live log lines go, consumed by [`ingest::IngestService`].
    /// `infra::logs::RunLogBroadcaster` in production; a recording double in
    /// tests.
    pub(crate) logs: Arc<dyn LogFanout>,
    /// Where a finished run's log lines are made durable, consumed by
    /// [`ingest::IngestDeps::archive`].
    ///
    /// **A required field, not an `Option` defaulted inside this
    /// constructor.** [`infra::logs::RunLogArchive`](crate::infra::logs::RunLogArchive)
    /// needs the database provider and a concrete `RunLogsRepository`, neither
    /// of which this module may construct — `domain::service` never names
    /// `crate::infra`, the same layering rule [`Self::logs`] follows. So
    /// unlike [`Self::admitter`]/[`Self::dispatcher`]/[`Self::watcher`], which
    /// default to the real production wiring when `None`, there is no
    /// production default this constructor can build for `archive` — the
    /// caller must always supply one. `gear.rs` passes the one real
    /// `RunLogArchive` it shares with the dispatcher tick; tests that do not
    /// exercise archiving pass `test_support::NullLogArchive` or their own
    /// double.
    ///
    /// This field used to be a `NoopLogArchive` this constructor built
    /// internally, deprecated as a compile-time tripwire for the task that
    /// was supposed to replace it. Making the field mandatory here is that
    /// replacement: omitting it is now a compile error at every call site
    /// instead of a lint an `#[allow]` could quietly re-admit.
    pub(crate) archive: Arc<dyn LogArchive>,
    /// **An override for the launch path's admission half, not the production
    /// wiring.** `None` — the production value — makes this container build the
    /// real [`admission::AdmissionService`] and hand the *same* instance to
    /// [`launch::LaunchService`], so there is exactly one admitter and exactly
    /// one [`admission::PlatformLocks`] in the process.
    ///
    /// It exists because the launch path must remain reviewable and testable
    /// without the concurrency core: `launch_tests` drives the six-step contract
    /// against a scripted admitter, and wiring the real one there would make
    /// every launch test depend on a lease, a queue repository and an executor.
    /// Task 13 expressed the same need by making these fields mandatory; making
    /// them optional is what let the production default move *into* the
    /// container, where the shared lock registry can be constructed once.
    pub(crate) admitter: Option<Arc<dyn launch::Admitter>>,
    /// The dispatch half — same reasoning as [`Self::admitter`], and `None`
    /// wires the real [`dispatch::DispatchService`].
    pub(crate) dispatcher: Option<Arc<dyn launch::InlineDispatcher>>,
    /// **An override for the result-observation seam, not the production
    /// wiring.** `None` — the production value — builds the real
    /// [`watch::SpawningRunWatcher`] over the *same* [`ingest::IngestService`]
    /// this container constructs, which is the only place both halves exist at
    /// once.
    ///
    /// It exists for the reason [`Self::dispatcher`] does: the re-attachment
    /// pass must be assertable without a real executor and without waiting on a
    /// spawned task, and a test that had to drive it through the production
    /// watcher would be asserting on a `tokio::spawn` it does not control.
    pub(crate) watcher: Option<Arc<dyn watch::RunWatcher>>,
    /// `runner_defaults.default_timeout_seconds`, honoured only when non-zero
    /// (`manager/src/services/argo.rs:168-181`).
    pub(crate) default_timeout_seconds: u64,
    /// The queue's three capacity settings.
    pub(crate) limits: QueueLimits,
    /// How long a claim may sit with no execution reference before a tick fails
    /// it — see [`dispatch::DispatchService`] and
    /// `crate::domain::state_machine::reconcile_claim`. Minutes, not seconds.
    pub(crate) orphan_timeout_seconds: u64,
}

/// # Why the services are `pub`, not `pub(crate)`
///
/// Step 10 asks for `pub(crate)`, and their **effective** visibility is already
/// crate-only: every one lives in a `pub(crate) mod` declared here, so nothing
/// outside this crate can name them. Writing `pub(crate)` on the items as well
/// is what `clippy::redundant_pub_crate` denies - it was tried, and it is a
/// hard error under this workspace's lint set.
///
/// The consequence worth knowing: **the module declarations below are the
/// enforcement**. Flipping one of them to `pub mod` would export a service
/// without touching the service, and no lint would object, because
/// `domain::service` is itself `pub mod`.
///
/// DI container aggregating the domain services.
///
/// **Generic over `R`, `Q` and `S`.** Task 13 shipped `AppServices<R>` because
/// the queue repository had no consumer and an unused type parameter is either a
/// `PhantomData` or a field nothing reads. Both halves of the concurrency core
/// consume `Q`, so it arrived with them, and `S` arrives with
/// [`schedules::ScheduleService`].
///
/// # The schedules repository is a constructor argument, not a `ServiceDeps` field
///
/// **Because `S` is a type parameter and [`ServiceDeps`] is not generic.** That
/// is the whole constraint, and an earlier revision of this paragraph gave a
/// different and false one — that a positional argument cannot be forgotten
/// where a field can. A plain required field on `ServiceDeps` is exactly as
/// unforgettable: omitting it is a compile error too, and the fields that
/// *can* be forgotten are the ones carrying `Option` defaults, which this
/// would not be. Making `ServiceDeps` generic to hold it would push a type
/// parameter onto every caller that builds one, including the tests that care
/// about none of it.
///
/// # One lock registry, one admitter, one dispatcher
///
/// This constructor is the only place [`admission::PlatformLocks`] is built,
/// and it clones the *same* registry into both services. That is not tidiness:
/// admission serialises "may this run start now?" per platform and the tick
/// serialises "which queued rows may I claim?" per platform, and the two
/// questions are the **same** critical section. Two registries would leave both
/// halves individually correct and the composition broken — a launch and a tick
/// deciding about one platform concurrently, each observing an idle platform,
/// each starting a run beside the other's exclusive one. No test inside either
/// module can see that;
/// `dispatch::tests::admission_and_dispatch_share_one_platform_lock_registry`
/// is the one that can.
///
/// **[`runs::RunsService`] takes the same registry, and for the same reason.**
/// Force start claims a queued row inside the platform's mutex so that a launch
/// being admitted concurrently observes the row stop being `queued`; built over
/// a second registry it would serialise against nothing, and the failure would
/// again be invisible to every test inside either module.
pub(crate) struct AppServices<R, Q, S>
where
    R: RunsRepository,
    Q: crate::domain::repos::QueueRepository,
    S: crate::domain::repos::SchedulesRepository,
{
    /// An `Arc` since Task 15, because [`runs::RunsService`] holds the *same*
    /// launch service: a re-run goes through the one creation path, so a second
    /// `LaunchService` would be a second name sequence, a second admitter
    /// reference and a second set of scopes for what is contractually one path.
    pub(crate) launch: Arc<launch::LaunchService<R>>,
    /// Held so the composition test above can prove the shared registry. The
    /// launch path reaches the same instance through
    /// [`launch::Admitter`](launch::Admitter).
    ///
    /// The *field* therefore has no production reader, while the *service* is
    /// very much reached - through the `Admitter` seam and through
    /// `RunsService`'s force-start cap check, both of which hold their own
    /// `Arc` to this instance.
    #[allow(
        dead_code,
        reason = "the service is reached through the Admitter seam and RunsDeps::admission; \
                  this field exists so the composition test can prove they are one instance"
    )]
    pub(crate) admission: Arc<admission::AdmissionService<R, Q>>,
    /// The dispatcher tick's home. Task 16's `serve` drives
    /// [`dispatch::DispatchService::run_tick`] from here.
    pub(crate) dispatch: Arc<dispatch::DispatchService<R, Q>>,
    /// Result ingestion.
    ///
    /// **Driven since Task 16c** by [`watch::SpawningRunWatcher`], which this
    /// constructor builds over this very instance and hands to
    /// [`dispatch::DispatchService`]'s re-attachment pass. Until then nothing
    /// drove it at all: removing this module's `dead_code` tripwire when
    /// `gear.rs` landed made the whole of [`ingest::IngestService`] report as
    /// unreachable, and a run this gear dispatched received no results and
    /// reached a terminal state only through the timeout sweep.
    ///
    /// **The field itself has no production reader**, and the shape is *not*
    /// quite [`Self::admission`]'s. That one's allow-reason is discharged by a
    /// real `Arc::ptr_eq` in
    /// `dispatch::tests::admission_and_dispatch_share_one_platform_lock_registry`.
    /// This one's first draft copied the sentence without the test:
    /// **no such test exists, and it is not constructible in that shape** —
    /// `watch::SpawningRunWatcher::ingest` and `dispatch::DispatchService::watcher`
    /// are both private with no accessor, so `Arc::ptr_eq` has nothing to
    /// compare. Adding accessors purely to compare pointers would be production
    /// surface existing for a test, which is what the end-to-end coverage below
    /// makes unnecessary.
    ///
    /// What covers the property instead is end-to-end:
    /// `watch::tests::a_run_reaches_a_terminal_state_from_its_finished_event`
    /// builds this container with `watcher: None` — the production default — and
    /// a run only reaches `Succeeded` if the watcher the container built drains
    /// into **an** ingest service wired to the same database, repositories,
    /// policy enforcer and log fan-out. That is the property the `#[allow]`
    /// claims and the one that matters.
    ///
    /// **It is not a one-*instance* property, and the first draft of this
    /// paragraph said it was** — *"two instances would leave the run
    /// `Running`"*. Break-tested by the review and reproduced here: making this
    /// constructor build a **second** `IngestService` and hand it to the watcher
    /// leaves the whole suite green. `IngestService` holds nothing but `Arc`s to
    /// shared collaborators and every effect it has lands in the database, so two
    /// instances built from the same `ServiceDeps` are observationally identical.
    ///
    /// That is precisely the *opposite* of `infra::logs::RunLogBroadcaster`,
    /// which holds per-process channel state — which is why Task 16's
    /// two-broadcaster defect was detectable at all and why `gear::LogWiring`
    /// exists. Reaching for that precedent here was the mistake: the two types
    /// do not share the property the precedent is about.
    #[allow(
        dead_code,
        reason = "the service is reached through the watcher's Arc to it, which this container \
                  constructs; the wiring is covered end to end by \
                  watch::tests::a_run_reaches_a_terminal_state_from_its_finished_event"
    )]
    pub(crate) ingest: Arc<ingest::IngestService<R, Q>>,
    /// Reads, cancel, re-run, and the queue operator actions.
    pub(crate) runs: Arc<runs::RunsService<R, Q>>,
    /// Schedule CRUD, and the firing tick `gear.rs`'s second ticker drives.
    ///
    /// Built over the **same** [`launch::LaunchService`] every other caller
    /// reaches, which is `cpt-cf-qa-fr-runs-schedules`' one-creation-path
    /// requirement discharged by construction rather than by a rule: this
    /// constructor holds the only `LaunchService` in the process and clones the
    /// `Arc` into the scheduler.
    pub(crate) schedules: Arc<schedules::ScheduleService<S, R>>,
}

impl<R, Q, S> AppServices<R, Q, S>
where
    R: RunsRepository + 'static,
    Q: crate::domain::repos::QueueRepository + 'static,
    S: crate::domain::repos::SchedulesRepository + 'static,
{
    pub(crate) fn new(
        runs_repo: Arc<R>,
        queue_repo: Arc<Q>,
        schedules_repo: Arc<S>,
        deps: ServiceDeps,
    ) -> Self {
        let enforcer = PolicyEnforcer::new(deps.authz);
        let locks = admission::PlatformLocks::default();

        let admission = Arc::new(admission::AdmissionService::new(admission::AdmissionDeps {
            db: Arc::clone(&deps.db),
            runs: Arc::clone(&runs_repo),
            queue: Arc::clone(&queue_repo),
            environments: Arc::clone(&deps.environments),
            executor: Arc::clone(&deps.executor),
            locks: locks.clone(),
            limits: deps.limits,
            policy_enforcer: enforcer.clone(),
        }));
        // **Built before the dispatch service, and that ordering is the wiring.**
        // The watcher drains `RunExecutor::watch` into *this* ingest service and
        // the dispatch service's re-attachment pass drives the watcher, so the
        // only construction order that works is ingest, then watcher, then
        // dispatch. It is also why the watcher's default cannot live in `infra`:
        // this is the one point in the program where both halves exist and
        // neither is reachable from outside — see `service::watch`'s header.
        let ingest = Arc::new(ingest::IngestService::new(ingest::IngestDeps {
            db: SerializedDb::new(Arc::clone(&deps.db)),
            runs: Arc::clone(&runs_repo),
            queue: Arc::clone(&queue_repo),
            environments: Arc::clone(&deps.environments),
            logs: Arc::clone(&deps.logs),
            archive: Arc::clone(&deps.archive),
            policy_enforcer: enforcer.clone(),
        }));
        let watcher = deps.watcher.unwrap_or_else(|| {
            Arc::new(watch::SpawningRunWatcher::new(
                Arc::clone(&deps.executor),
                Arc::clone(&ingest),
            )) as Arc<dyn watch::RunWatcher>
        });

        let dispatch = Arc::new(dispatch::DispatchService::new(dispatch::DispatchDeps {
            db: Arc::clone(&deps.db),
            runs: Arc::clone(&runs_repo),
            queue: Arc::clone(&queue_repo),
            catalog: Arc::clone(&deps.catalog),
            environments: Arc::clone(&deps.environments),
            product_plugins: Arc::clone(&deps.product_plugins),
            executor: Arc::clone(&deps.executor),
            logs: Arc::clone(&deps.logs),
            locks: locks.clone(),
            limits: deps.limits,
            orphan_timeout_seconds: deps.orphan_timeout_seconds,
            policy_enforcer: enforcer.clone(),
            watcher,
        }));

        let admitter = deps
            .admitter
            .unwrap_or_else(|| Arc::clone(&admission) as Arc<dyn launch::Admitter>);
        let dispatcher = deps
            .dispatcher
            .unwrap_or_else(|| Arc::clone(&dispatch) as Arc<dyn launch::InlineDispatcher>);

        let launch = Arc::new(launch::LaunchService::new(
            Arc::clone(&deps.db),
            Arc::clone(&runs_repo),
            Arc::clone(&deps.catalog),
            Arc::clone(&deps.environments),
            Arc::clone(&deps.logs),
            admitter,
            Arc::clone(&dispatcher),
            deps.default_timeout_seconds,
            enforcer.clone(),
        ));
        // The same `LaunchService` the REST layer and the re-run path hold, not
        // a second one built from the same parts: one creation path is the
        // requirement, and an `Arc::clone` is what makes it true here.
        let schedules = Arc::new(schedules::ScheduleService::new(schedules::ScheduleDeps {
            db: Arc::clone(&deps.db),
            schedules: schedules_repo,
            launch: Arc::clone(&launch),
            policy_enforcer: enforcer.clone(),
        }));

        let runs = Arc::new(runs::RunsService::new(runs::RunsDeps {
            db: deps.db,
            runs: runs_repo,
            queue: queue_repo,
            environments: deps.environments,
            executor: deps.executor,
            logs: deps.logs,
            launch: Arc::clone(&launch),
            dispatcher,
            admission: Arc::clone(&admission),
            // The one limit this service needs: `ttl_expires_at` is a derived
            // column of the queue listing. The other two stay with admission.
            queue_ttl_seconds: deps.limits.queue_ttl_seconds,
            // The same registry, not a second one — see this type's header.
            locks,
            policy_enforcer: enforcer,
        }));

        Self {
            launch,
            admission,
            dispatch,
            ingest,
            runs,
            schedules,
        }
    }
}
