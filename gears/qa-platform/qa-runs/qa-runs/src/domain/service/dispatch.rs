//! The one path from a claimed queue row to a started execution, plus the tick
//! that drains the queue.
//!
//! Ported from `manager/src/services/run_dispatcher.rs`. Two entry points and one
//! tick, in the source system's shape:
//!
//! * [`DispatchService::dispatch_one`] — the single submission path, used by
//!   **both** the inline admission path and the tick, so there is one submission
//!   path with no bypass branch (`run_dispatcher.rs:1-6`). It does the expensive
//!   work: force-sync, bundle build, environment assembly, `RunExecutor::start`.
//! * [`DispatchService::run_tick`] — one dispatcher cycle
//!   (`run_dispatcher.rs:344-415`).
//! * [`DispatchService::recover_after_boot`] — the boot-only rule
//!   (`run_queue.rs:349-361`), which is **not** the tick's rule and must never be
//!   called from one.
//!
//! # The tick's order, and the one place this diverges from the plan
//!
//! Legacy's order is: TTL sweep, executor listing (skip the tick on error), claim
//! reconciliation, re-read claims, global-cap check, enumerate platforms, drain
//! each with a threaded budget. Every step is where it is for a reason the source
//! system states in place, and the first is the sharpest: the sweep runs "first,
//! and before every early return below", because the two early returns fire in
//! exactly the situations where queued rows pile up unnoticed — a cluster at
//! `max_concurrent_runs`, and an unreadable executor, during which occupancy
//! fails safe to busy so every launch queues and nothing drains
//! (`run_dispatcher.rs:344-351`, and the same argument again at `:495-508`).
//!
//! **The control-plane timeout sweep runs second, not last.** The plan specifies
//! it as step 8, after the drain. That would put it downstream of both early
//! returns, and legacy's own argument for the TTL sweep applies to it word for
//! word: a run past its deadline is *most* in need of reclaiming when the cluster
//! is at its cap (reclaiming it is what frees capacity) or when the executor is
//! unreadable (during which nothing else can progress at all). It depends on
//! neither the listing nor the reconciliation nor the cap. Placed after the drain
//! it would be switched off in the two situations that need it most, which is the
//! defect the plan's own step 2.1 exists to prevent. Reported to the coordinator
//! rather than filed as a plan edit.
//!
//! # Tenancy: enumerate with nil, write with the row's own tenant
//!
//! Every cross-tenant enumeration here uses a nil-tenant
//! [`crate::domain::system_actor`] factory and every write that follows uses the
//! paired tenant-bound one, built through
//! [`TenantBound::new`](crate::domain::system_actor::TenantBound::new), which
//! refuses nil. A nil-tenant write is *denied* rather than mis-scoped, so getting
//! this wrong makes the dispatcher inert rather than wrong — which is worse than
//! it sounds, because it fails quietly. Every call site therefore reads
//! `let Some(tenant) = TenantBound::new(row.tenant_id) else { warn; continue; }`:
//! fail-closed but **visible**.
//!
//! **One repository method looks like it can be used cross-tenant and cannot.**
//! `QueueRepository::expire_queued_before` is a single bulk
//! `UPDATE … RETURNING` and reads as one call for the whole cluster. Issued under
//! a nil-tenant scope it is a cross-tenant *write*, which this discipline
//! forbids and a deployment's policy would deny anyway. So the sweep enumerates
//! the tenants that could have an expirable row — `platforms_with_queued_rows`,
//! a read, under the nil-tenant context — and then issues the bulk write **once
//! per tenant** under that tenant's own scope. The same applies to
//! `fail_orphaned_dispatching`, which takes a list of ids and is called once per
//! tenant with that tenant's ids.
//!
//! **Per-tenant issuing is necessary and was not sufficient, and the difference
//! is the one security finding this task shipped.** An earlier version of the
//! paragraph above stopped there, as though N calls under N contexts were N
//! single-tenant writes. They are not: `expire_queued_before` is **set-based**, so
//! its row set comes from the compiled scope and not from the loop, and issuing it
//! N times under N *possibly identical* covering scopes is N cross-tenant writes.
//! Whether the scopes differ is the PDP's decision: a deployment whose policy
//! grants `qa_runs.system` a **covering** constraint set for this write action
//! — one that is not clamped to the tenant the context names — makes them
//! identical regardless of which tenant asked. Under such a policy, one
//! statement returned two tenants' rows and the loop stamped both rows'
//! writes with the first tenant's identity.
//!
//! So the guarantee is now a property of this code: **every write for an
//! expired row is bound to `row.tenant_id`, not to the loop's tenant**, and a
//! mismatch between the two is logged at ERROR as the policy misconfiguration it
//! is. The per-tenant loop still bounds which rows the *statement* can reach; the
//! per-row context bounds which identity each row is written under.
//!
//! Refusing the foreign row outright was the first attempt and it traded one defect
//! for another: the bulk statement has already expired the row by then, so skipping
//! left its run in whichever state it already had — the frozen guide's line 96
//! promise broken for precisely the row the covering scope pulled in, since the
//! mandatory WARN in [`DispatchService::expire_one_row`] only fires for a row this
//! sweep actually returns.
//!
//! `fail_orphaned_dispatching` needs no equivalent because it drives from a
//! per-tenant id list, and neither does any other pass; `expire_queued_before` is
//! the only one whose row set the scope chooses.
//!
//! # The reference deployment denies every nil-tenant system actor, and the
//! dispatcher runs there anyway — because it never asks
//!
//! The dev stack's `static-authz` plugin derives its decision purely from the
//! resolved tenant and denies a nil tenant outright; its config is
//! `{vendor, priority}` with no per-subject grant to express. Every pass below
//! still *mints* a nil-tenant `system_actor` context for its enumerating read
//! (`for_dispatch_enumeration`, `for_claim_reconciliation`, `for_ttl_sweep`,
//! `for_timeout_sweep`, `for_watch_scan`) — but that read no longer reaches the
//! PEP at all. It elevates through `crate::domain::elevated::enumeration_scope`
//! instead (see that module's doc for why this is not the cross-tenant oracle
//! the crate otherwise bans), so the reference deployment's blanket
//! nil-tenant denial has nothing to deny. Every write the enumeration feeds is
//! still issued per row under the *tenant-bound* counterpart factory
//! (`for_dispatch`, `for_ttl_expiry`, `for_timeout_enforcement`, `for_result_ingest`,
//! `for_claim_release`), minted from the row's own resolved tenant id — a
//! context `static-authz` grants exactly as it would for any end-user request
//! naming that tenant.
//!
//! Before this module routed through the seam, every pass below failed with
//! [`DomainError::Forbidden`] in the reference deployment, and disabling the
//! dispatcher there was rejected: it makes the launch path silently one-shot,
//! because a run that queues never starts and strict FIFO then blocks every
//! later launch on that platform, with no error anywhere. The mechanism built
//! for that case is kept, because a *write* can still be denied — a
//! misconfigured policy, a tenant-scoped deny rule, anything short of the
//! blanket nil-tenant case this task closed. Every pass still folds a write
//! denial into [`TickReport::denied_passes`] through
//! [`TickReport::note_failure`], which emits **one** actionable WARN per pass
//! naming the remedy — the shape `qa-catalog/src/gear.rs:400-419`'s
//! `log_task_failure` uses, generalised so a pass that denies fifty rows still
//! logs once. Task 16 adds the `dispatcher_enabled` knob so an operator can turn
//! it off deliberately rather than by accident.
//!
//! **What is proven, and what is not.** The dedup rule's *emission* is pinned:
//! `a_pass_denied_once_per_row_logs_its_remedy_once` counts the WARNs a
//! per-row-denied pass produces, which is the number an operator sees. The
//! other lines in this module are still argued from the classification and the
//! per-pass folding they derive from rather than observed. See
//! [`TickReport::note_failure`].

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use qa_catalog_sdk::QaCatalogClientV1;
use qa_environments_sdk::QaEnvironmentsClientV1;
use qa_runs_sdk::{Run, RunState};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::admission::{PlatformLocks, lease_occupancy};
use super::launch::InlineDispatcher;
use super::watch::{RunWatcher, WatchTarget};
use super::{DbProvider, QueueLimits, actions, emit, resources};
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{DispatchMetrics, DispatchOutcome};
use crate::domain::ports::product_plugin::ProductPluginPort;
use crate::domain::ports::run_executor::{ExecutionRef, RunExecutor};
use crate::domain::queue::{
    GlobalCap, cap_reached, expiry_cutoff, global_cap_status, plan_dispatch_batch,
};
use crate::domain::repos::{
    ClaimAge, MAX_CLAIM_SCAN, MAX_TIMEOUT_SWEEP_SCAN, MAX_WATCH_SCAN, QueueRepository,
    QueuedPlatform, RunStatePatch, RunsRepository, WatchCandidate, Windowed,
};
use crate::domain::state_machine::{
    ClaimAction, ClaimExecution, ClaimObservation, boot_recovery_action, can_transition,
    elapsed_seconds, is_terminal, reconcile_claim,
};
use crate::domain::system_actor::{self, TenantBound};

/// The reason recorded on an expired row's `error`, and echoed verbatim in the
/// WARN beside it so an operator reading the log and an operator reading the row
/// see the same sentence. Ported verbatim from
/// `manager/src/services/run_dispatcher.rs:492-493`.
const EXPIRY_REASON: &str = "Waited longer than queue_ttl_seconds and was expired without starting";

/// The reason recorded on a row a restart left mid-dispatch.
///
/// Legacy says "before a workflow was created"
/// (`manager/src/services/run_dispatcher.rs:468`); ADR-0001 removes workflows,
/// so the noun changes and nothing else does.
const ORPHAN_REASON: &str = "Dispatch was interrupted before an execution was created";

/// The reason recorded on a run the control plane reclaimed at its deadline.
///
/// No legacy counterpart: the source system relies on Argo's
/// `activeDeadlineSeconds` alone (`manager/src/services/argo.rs:539`), and
/// `cpt-cf-qa-fr-runs-timeout` requires control-plane enforcement because
/// "enforcement cannot rely on the execution backend alone" (`PRD.md:404`).
const TIMEOUT_REASON: &str = "The run exceeded its timeout and was cancelled by the control plane";

/// Named passes, so [`TickReport::denied_passes`] holds stable strings a test
/// and a log line can both name.
const PASS_TTL: &str = "queue TTL sweep";
const PASS_TIMEOUT: &str = "control-plane timeout sweep";
const PASS_LIST_ACTIVE: &str = "executor listing";
const PASS_RECONCILE: &str = "claim reconciliation";
const PASS_CAP: &str = "global cap evaluation";
const PASS_ENUMERATE: &str = "queued-platform enumeration";
const PASS_DRAIN: &str = "platform drain";
const PASS_BOOT: &str = "boot claim recovery";
const PASS_WATCH: &str = "watcher re-attachment";

/// What one tick did, and what it could not do.
///
/// Returned rather than logged-and-forgotten for two reasons. Task 16's ticker
/// needs something to log at the end of a pass, and — the reason it is a struct
/// and not a `()` — the folding in [`Self::denied_passes`] is what makes "one
/// actionable WARN per pass" assertable from the data rather than only from the
/// output.
///
/// **Corrected 2026-08-15 by Task 16c.** This said the folding was *"the only
/// testable form"* of that rule, on the grounds that *"this crate has no
/// log-capturing dev-dependency"*. It has one: `tracing-test` is in
/// `Cargo.toml`'s `[dev-dependencies]` with a paragraph explaining the choice,
/// and this module's own
/// `a_pass_denied_once_per_row_logs_its_remedy_once` already counts emitted
/// WARNs with it — as the module header says in as many words. The sentence
/// survived the arrival of the thing that falsified it.
///
/// # `#[domain_model]`, and where this task drew the line
///
/// Added in the fix round on the gear's own precedent, which the spec review
/// found: `service::launch` marks [`Admission`](super::launch::Admission) — a
/// service-layer outcome value returned across a layer boundary — and leaves it
/// off `LaunchService`, `NestedPlan`, `TargetFacts` and `Resolved`. This is
/// `Admission`'s shape exactly: the value the composition tier hands back to the
/// tier that drove it. The attribute is not about serialization (this type is
/// never serialized); it rejects infrastructure types in fields at compile time,
/// which is a live guarantee for a struct Task 16 will grow log fields on.
///
/// The remaining omissions in this module and its sibling are deliberate and
/// match that precedent:
///
/// * [`DispatchDeps`] / `AdmissionDeps` — DI structs holding `Arc<dyn _>` and
///   `Arc<DbProvider>`. They *cannot* carry it, which is the attribute working.
/// * `PlatformLocks` — a live handle on a mutex registry, the class
///   `domain::ports::run_executor` exempts explicitly for `ExecutionStream` and
///   `ExecutionSink`: "live handles on an in-flight observation, which is the one
///   thing in this file that is not a value".
/// * `QueueLimits` — a configuration input consumed inside the service tier and
///   never returned or published. Borderline, and decided on direction of travel:
///   `Admission` and this type come *out*, `QueueLimits` goes *in*.
/// * `ClaimedRow`, `Started`, `QueueWrite` — module-private, never crossing any
///   boundary.
///
/// None of this is settleable without the lint: `cargo gears lint --dylint` is
/// absent, so DE0309 coverage is **unverified** either way.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Queued rows the TTL sweep expired.
    pub expired: usize,
    /// Runs the control-plane deadline reclaimed.
    pub timed_out: usize,
    /// Claims released because their execution was terminal or gone.
    pub released: usize,
    /// Claims failed because they were left mid-dispatch past the orphan
    /// timeout.
    pub failed_orphans: usize,
    /// Queue rows this tick claimed, cluster-wide.
    pub claimed: u32,
    /// Live runs this tick started observing.
    ///
    /// **Not "runs being observed"** — a run this process was already watching
    /// is skipped and does not count, so a healthy steady state reports zero
    /// here and that is the wanted reading. A number that stays high tick after
    /// tick means observers keep ending, which is what an executor that has
    /// forgotten its executions looks like from here.
    pub attached: usize,
    /// Set when the tick stopped before draining, naming which step stopped it.
    /// `None` means the tick ran to the end.
    pub stopped_at: Option<&'static str>,
    /// Passes the policy decision point refused, each recorded **once**.
    pub denied_passes: Vec<&'static str>,
}

impl TickReport {
    /// Record a pass failure at the level its cause warrants, at most one WARN
    /// per pass.
    ///
    /// [`DomainError::Forbidden`] means the deployment's policy does not grant
    /// this gear's system actor the scope the pass needs. The pass is then inert
    /// *by configuration* rather than broken, and it will be inert on every tick
    /// — so it belongs at WARN with the remedy named, not in the ERROR stream
    /// where real faults live. That is `qa-catalog/src/gear.rs:400-419`'s rule;
    /// the addition here is the deduplication, because a pass that iterates rows
    /// can be denied once per row and an operator needs the sentence once.
    ///
    /// **Only a tenant-bound write can still reach here as `Forbidden`.** Every
    /// pass's enumerating read elevates through `crate::domain::elevated` and
    /// never calls `access_scope`, so it cannot be denied; a `Forbidden` this
    /// function sees always came from one of the tenant-bound write factories
    /// in `domain::system_actor` (see that module's Authorization note).
    ///
    /// **`list` belongs in the remedy below, and it is easy to remove by the
    /// wrong reasoning.** It is not needed by any enumeration — those elevate
    /// and never reach the PDP, per the paragraph above — but `PASS_DRAIN`
    /// (`Self::drain_platform`) mints a tenant-bound `for_dispatch(tenant)`
    /// context and then calls `queued_rows` and `claims_for_platform`, both
    /// of which ask for `actions::LIST`; `IngestService::release_claim` does
    /// the same under `for_claim_release`. Either can come back `Forbidden`
    /// and land here, so `list` is a real requirement of the *write* side,
    /// not a leftover from the enumeration side this note already excludes.
    ///
    /// Everything else is a genuine fault and stays at ERROR, undeduplicated:
    /// two different rows failing for two different database reasons are two
    /// facts.
    fn note_failure(&mut self, pass: &'static str, error: &DomainError) {
        if !matches!(error, DomainError::Forbidden) {
            error!(pass, %error, "qa-runs dispatcher pass failed");
            return;
        }
        if !self.denied_passes.contains(&pass) {
            self.denied_passes.push(pass);
            warn!(
                pass,
                "the qa-runs dispatcher is not authorized for this pass; grant the \
                 qa_runs.system subject (subject_type=qa_runs.system) qa.run and \
                 qa.queue_entry get/list/dispatch (list is needed by the tenant-bound \
                 write side, e.g. drain's claims_for_platform/queued_rows, not by any \
                 enumeration), or set dispatcher_enabled: false"
            );
        }
    }
}

/// Constructor arguments for [`DispatchService`].
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
pub struct DispatchDeps<R, Q> {
    pub db: Arc<DbProvider>,
    pub runs: Arc<R>,
    pub queue: Arc<Q>,
    pub catalog: Arc<dyn QaCatalogClientV1>,
    pub environments: Arc<dyn QaEnvironmentsClientV1>,
    /// How a run reaches its target environment: the product plugin, resolved
    /// per dispatch. `infra::product_plugin::HubProductPluginResolver` in
    /// production.
    pub product_plugins: Arc<dyn ProductPluginPort>,
    pub executor: Arc<dyn RunExecutor>,
    /// Where a terminal transition releases the run's live log channel — see
    /// [`DispatchService::transition`].
    pub logs: Arc<dyn super::LogFanout>,
    /// **The same registry admission uses.** See [`super::AppServices`].
    pub locks: PlatformLocks,
    pub limits: QueueLimits,
    pub orphan_timeout_seconds: u64,
    pub policy_enforcer: PolicyEnforcer,
    /// Where [`DispatchService::reattach_watchers`] sends the runs it finds
    /// nobody is observing. The production value drains them into
    /// `service::ingest`; see [`super::watch`].
    pub watcher: Arc<dyn RunWatcher>,
    /// Where [`DispatchService::run_tick`] reports what one tick did and how
    /// long it took. `NoopMetrics` when no adapter is installed — see
    /// [`super::ServiceDeps::dispatch_metrics`].
    pub metrics: Arc<dyn DispatchMetrics>,
}

/// Submission, the dispatcher tick, and boot recovery.
pub struct DispatchService<R, Q> {
    // `pub(super)` on the six the `dispatch_spec` half reads. It is a second
    // `impl` block on this type in a sibling module, so it needs field access
    // rather than an accessor per field; `queue`, `locks`, `limits` and
    // `orphan_timeout_seconds` stay private because the translation half touches
    // no queue row, no lease and no report.
    pub(super) db: Arc<DbProvider>,
    pub(super) runs: Arc<R>,
    queue: Arc<Q>,
    pub(super) catalog: Arc<dyn QaCatalogClientV1>,
    pub(super) environments: Arc<dyn QaEnvironmentsClientV1>,
    pub(super) product_plugins: Arc<dyn ProductPluginPort>,
    pub(super) executor: Arc<dyn RunExecutor>,
    logs: Arc<dyn super::LogFanout>,
    locks: PlatformLocks,
    limits: QueueLimits,
    orphan_timeout_seconds: u64,
    policy_enforcer: PolicyEnforcer,
    /// Where the next claim scan resumes - the id of the last row the previous
    /// scan returned, or `None` to start from the beginning of the id space.
    ///
    /// **This is what turns a bounded window into bounded *coverage*.** See
    /// `QueueRepository::all_claims`: a window over a stable ordering starves,
    /// because a healthy claim is never written to and so never leaves the head
    /// of one. The cursor is per-replica and purely an optimisation of *where to
    /// resume*; losing it on an **isolated** restart costs one extra cycle of
    /// coverage, which is why it is not persisted.
    ///
    /// **The precondition is that restarts are isolated.** A replica
    /// crash-looping faster than one full cycle restarts at `None` every time,
    /// so it never advances past the first window and the tail of the id space
    /// is never visited at all. That is a crash-loop symptom rather than a
    /// design flaw - nothing else works either - but it is not "one extra
    /// cycle", and the difference matters to whoever is reading a held platform
    /// during exactly that incident.
    claim_scan_cursor: Mutex<Option<Uuid>>,
    /// The same rotation for the timeout sweep - see
    /// `RunsRepository::list_timeout_candidates`, which carries the argument
    /// for why an age ordering starves here too.
    timeout_scan_cursor: Mutex<Option<Uuid>>,
    /// And for the watcher re-attachment scan, where the starvation argument is
    /// the strongest of the three - see `RunsRepository::list_watch_candidates`,
    /// because a healthy live run never leaves that candidate set.
    watch_scan_cursor: Mutex<Option<Uuid>>,
    watcher: Arc<dyn RunWatcher>,
    /// See [`DispatchDeps::metrics`].
    metrics: Arc<dyn DispatchMetrics>,
    /// This service's emission latch — see `super::emit`. Per service rather
    /// than global, so a broken dispatch adapter cannot silence ingest.
    metrics_silenced: AtomicBool,
}

/// A queue row this tick claimed, with the run it is for.
///
/// Named fields over `(Uuid, Uuid)`: two ids of the same type, and transposing
/// them would dispatch the wrong run and mark the wrong row.
///
/// **The third field is not covered by that argument.** `exclusive` is read once,
/// in the `warn!` on a failed dispatch — the lease mode it selected was consumed
/// before this struct was built. It is carried because a log line about a run that
/// failed to start is materially less useful without it: whether the row was
/// exclusive is what decides how much of the platform's queue that failure was
/// holding up.
#[derive(Clone, Copy, Debug)]
struct ClaimedRow {
    queue_id: Uuid,
    run_id: Uuid,
    exclusive: bool,
    /// When this row joined the queue, from the FIFO snapshot the claim was
    /// planned against — what [`DispatchService::drain_platform`] measures the
    /// queue wait from.
    ///
    /// `None` when the row was not in that snapshot, which is the same
    /// fail-closed case `exclusive` handles a line above: no timestamp, no
    /// observation. Silently dropping the sample is right here — a fabricated
    /// one would be indistinguishable from a real wait in the histogram.
    enqueued_at: Option<OffsetDateTime>,
}

/// What a successful submit produced.
///
/// # Rejected alternative: `(ExecutionRef, OffsetDateTime)`
///
/// Not rejected for transposition-safety — the two types differ, so the compiler
/// already refuses a swap. Rejected because the struct is what carries **one
/// instant across two functions**: `submit` stamps `started_at` at the moment the
/// executor accepted the run, and `record_started` writes that same value to
/// `qa_runs.started_at`. A tuple would work identically; what the named type buys
/// is that the alternative — letting `record_started` read the clock itself —
/// becomes visibly a change rather than a simplification. Under that alternative
/// the column would record the moment it was written rather than the moment the
/// executor actually accepted the run, silently widening the gap between the two.
pub(super) struct Started {
    pub(super) execution_ref: ExecutionRef,
    pub(super) started_at: OffsetDateTime,
}

/// Which guarded write on a queue row is being made.
///
/// An enum rather than four near-identical methods, because the scope derivation
/// and the connection handling are identical for all of them and only the final
/// call differs — and because it keeps `mark_done` and `mark_failed`
/// distinguishable at every call site, which matters: `mark_done` releases a
/// claim as *completed* and `mark_failed` as *broken*, and they are one word
/// apart.
enum QueueWrite<'a> {
    /// Claim a queued row for this tick. The only *guarded* variant here — its
    /// `AND state = 'queued'` predicate is "a cheap guard against double dispatch"
    /// (`manager/src/services/run_queue.rs:279-291`) — which is why it returns the
    /// `bool` every variant returns rather than being special-cased.
    Claimed,
    /// The submit succeeded.
    Running,
    /// Return a claimed row to the queue, keeping its FIFO place, after the lease
    /// refused it. Guarded on `dispatching`.
    Requeued,
    /// The claim's execution is over and the platform is free.
    Done,
    /// The claim is broken; the reason is recorded on the row.
    Failed(&'a str),
}

/// How many runs are committed cluster-wide: live executions, plus claims the
/// executor cannot see yet.
///
/// Ported from `committed_active_count`
/// (`manager/src/services/run_queue.rs:941-960`), including the rule that makes
/// it more than `active.len()`: a row marked `dispatching` has not reached the
/// executor — the submit happens after the platform lock is released — so
/// counting live executions alone undercounts and would let a tick overshoot
/// `max_concurrent_runs`. A claim whose execution *is* live is skipped, because
/// the listing already counts it.
///
/// `known` is what claim reconciliation observed this tick, keyed by run.
/// **A claim absent from it counts as uncommitted**, which is the fail-closed
/// direction and the case that actually occurs: a launch admitted between the
/// reconciliation pass and the re-read of claims has a claim nothing has
/// classified. Legacy gets the same answer by re-reading the row's nullable
/// `workflow_name`; here the second read would cost one scoped run lookup per
/// claim, so the classification is carried forward instead and anything unknown
/// is assumed to need budget.
fn committed_active(
    active: &BTreeSet<ExecutionRef>,
    claims: &[ClaimAge],
    known: &HashMap<Uuid, ClaimExecution>,
) -> usize {
    let uncommitted = claims
        .iter()
        .filter(|claim| match known.get(&claim.run_id) {
            Some(ClaimExecution::Active) => false,
            Some(ClaimExecution::Absent | ClaimExecution::Gone) | None => true,
        })
        .count();
    active.len().saturating_add(uncommitted)
}

/// How to label a tick that a swallowed pass failure stopped.
///
/// Two of [`DispatchService::run_tick`]'s early returns come out of helpers
/// that fold their error into [`TickReport::note_failure`] and hand back a bare
/// `None`, so the error itself is gone by the time the tick can label it. What
/// survives is the one distinction the label needs: `note_failure` puts a
/// [`DomainError::Forbidden`] — and nothing else — into `denied_passes`, which
/// is the same "the caller's own configuration, not a fault" split
/// [`DomainError::disclosable`] makes for the stop that *does* still have its
/// error. Everything else is a genuine fault and is [`DispatchOutcome::Failed`].
///
/// `before` is the denial count read immediately ahead of the pass, not zero: a
/// denial recorded by an *earlier* sweep must not relabel this one, and the
/// sweeps run first by design.
fn stopped_outcome(report: &TickReport, before: usize) -> DispatchOutcome {
    if report.denied_passes.len() > before {
        DispatchOutcome::Refused
    } else {
        DispatchOutcome::Failed
    }
}

/// Where a claim scan stopped, so the caller can advance the cursor **after**
/// every read that must see the same window.
///
/// A two-field value rather than the `Windowed` itself: the rows are consumed
/// by the reconciliation loop, and handing them back only so the caller could
/// look at the last one would keep a whole window alive for one id.
#[derive(Clone, Copy, Debug)]
struct ScanEnd {
    /// Whether the window filled - the caller wraps when it did not.
    truncated: bool,
    /// The last row's id, which is where the next scan resumes.
    last: Option<Uuid>,
}

/// Classify a claim's execution against the executor's active set.
///
/// The three states map one-for-one onto what the source system reads: the row's
/// nullable handle, then its membership in the set of live executions
/// (`manager/src/services/run_dispatcher.rs:440-457`). **Absence from a
/// *successful* listing is the only evidence that an execution is over**; a
/// listing that failed never reaches here, because the tick skips entirely on
/// that error.
fn classify_execution(run: &Run, active: &BTreeSet<ExecutionRef>) -> ClaimExecution {
    match run.execution_ref.as_deref() {
        None => ClaimExecution::Absent,
        Some(reference) => {
            if active.contains(&ExecutionRef::new(reference)) {
                ClaimExecution::Active
            } else {
                ClaimExecution::Gone
            }
        }
    }
}

pub(super) fn environments_error(error: &qa_environments_sdk::QaEnvironmentsError) -> DomainError {
    DomainError::Environments(error.to_string())
}

impl<R, Q> DispatchService<R, Q>
where
    R: RunsRepository,
    Q: QueueRepository,
{
    pub fn new(deps: DispatchDeps<R, Q>) -> Self {
        Self {
            db: deps.db,
            runs: deps.runs,
            queue: deps.queue,
            catalog: deps.catalog,
            environments: deps.environments,
            product_plugins: deps.product_plugins,
            executor: deps.executor,
            logs: deps.logs,
            locks: deps.locks,
            limits: deps.limits,
            orphan_timeout_seconds: deps.orphan_timeout_seconds,
            policy_enforcer: deps.policy_enforcer,
            claim_scan_cursor: Mutex::new(None),
            timeout_scan_cursor: Mutex::new(None),
            watch_scan_cursor: Mutex::new(None),
            watcher: deps.watcher,
            metrics: deps.metrics,
            metrics_silenced: AtomicBool::new(false),
        }
    }

    /// The lock registry, for the composition test that proves admission and
    /// dispatch share one.
    #[cfg(test)]
    pub(in crate::domain::service) fn locks(&self) -> &PlatformLocks {
        &self.locks
    }

    pub(super) async fn run_scope(
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

    /// Move a run between states, guarded by the state machine.
    ///
    /// `can_transition` is asked about the state the repository returned, not
    /// about a literal, so it is not a self-evidently-true guard. `false` from the
    /// conditional update means the run moved first, which for a background pass
    /// is a lost race rather than a fault.
    ///
    /// # A terminal transition reaps the run's live log channel
    ///
    /// **Here rather than at the four call sites, so a fifth cannot forget it.**
    /// This module records a terminal state from `record_failed`, `expire_run`,
    /// `reclaim_overdue` and `fail_orphan`; the security review found that none of
    /// them released the run's `infra::logs` channel, because only
    /// `service::ingest` did. Two consequences, and the second is the one that
    /// bites: the channel outlived the run, and — since `LogSubscription::recv`
    /// answers `None` only when the channel is dropped — an SSE handler watching a
    /// timed-out or expired run held its connection open forever.
    ///
    /// Reaped only on success and only when `to` is terminal, so a lost race
    /// leaves a still-live run's subscribers connected.
    async fn transition(
        &self,
        ctx: &SecurityContext,
        run: &Run,
        to: RunState,
        patch: RunStatePatch,
    ) -> Result<(), DomainError> {
        if !can_transition(run.state, to) {
            return Err(DomainError::IllegalTransition {
                id: run.id,
                state: run.state,
                action: format!("become {}", to.as_str()),
            });
        }
        let scope = self.run_scope(ctx, actions::DISPATCH, Some(run.id)).await?;
        let conn = self.db.conn()?;
        if self
            .runs
            .update_state(&conn, &scope, run.id, run.state, to, patch)
            .await?
        {
            if is_terminal(to) {
                self.logs.reap(run.id);
            }
            Ok(())
        } else {
            Err(DomainError::IllegalTransition {
                id: run.id,
                state: run.state,
                action: format!("become {}", to.as_str()),
            })
        }
    }

    /// Read a run under a `qa.run`/`get` scope.
    pub(super) async fn read_run(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
    ) -> Result<Run, DomainError> {
        let scope = self.run_scope(ctx, actions::GET, Some(run_id)).await?;
        let conn = self.db.conn()?;
        self.runs
            .get(&conn, &scope, run_id)
            .await?
            .ok_or(DomainError::RunNotFound { id: run_id })
    }

    /// **Every** single-row write the dispatcher makes on `qa_run_queue`, each as
    /// one fallible step.
    ///
    /// Extracted so their callers can `if let Err(..)` once instead of nesting a
    /// scope match inside a connection match inside a call match — three levels of
    /// error plumbing that said nothing and buried the decision each caller was
    /// actually making. Every one derives a **fresh** `qa.queue_entry` scope, so
    /// none can inherit a scope compiled for `qa.run`.
    ///
    /// **"Every" is now true, and it was not.** This said "the four queue writes"
    /// while [`QueueWrite`] had three variants, and the arithmetic hid a real
    /// erosion: `mark_dispatching` was hand-rolled in a `claim_row` helper from the
    /// start, and round 3's `requeue` — a *fifth* guarded single-row write — was
    /// hand-rolled too, in eighteen lines of exactly the nesting this function was
    /// extracted to remove. Both now go through here. A count in a doc comment is
    /// a claim, and the way this one failed was by being written once and then
    /// bypassed twice.
    async fn queue_write(
        &self,
        ctx: &SecurityContext,
        queue_id: Uuid,
        write: QueueWrite<'_>,
    ) -> Result<bool, DomainError> {
        let scope = self
            .queue_scope(ctx, actions::DISPATCH, Some(queue_id))
            .await?;
        let conn = self.db.conn()?;
        match write {
            QueueWrite::Claimed => self.queue.mark_dispatching(&conn, &scope, queue_id).await,
            QueueWrite::Running => self.queue.mark_running(&conn, &scope, queue_id).await,
            QueueWrite::Requeued => self.queue.requeue(&conn, &scope, queue_id).await,
            QueueWrite::Done => self.queue.mark_done(&conn, &scope, queue_id).await,
            QueueWrite::Failed(reason) => {
                self.queue
                    .mark_failed(&conn, &scope, queue_id, reason)
                    .await
            }
        }
    }

    /// Record the executor's handle on the run.
    async fn record_execution_ref(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        reference: &str,
    ) -> Result<bool, DomainError> {
        let scope = self.run_scope(ctx, actions::DISPATCH, Some(run_id)).await?;
        let conn = self.db.conn()?;
        self.runs
            .set_execution_ref(&conn, &scope, run_id, reference)
            .await
    }

    /// Hand a platform back, best-effort.
    ///
    /// A lease that is not released is the failure that wedges a platform: the
    /// lease is this gear's only occupancy oracle, so a stale exclusive hold makes
    /// every later launch on that platform queue forever. ERROR, not WARN.
    async fn release_lease(&self, ctx: &SecurityContext, platform_id: Uuid, run_id: Uuid) {
        if let Err(error) = self
            .environments
            .release_lease(ctx, platform_id, run_id)
            .await
        {
            error!(
                %run_id,
                %platform_id,
                %error,
                "could not release the platform lease; the platform will read as busy \
                 until the lease is cleared",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// dispatch_one: the single submission path
// ---------------------------------------------------------------------------

impl<R, Q> DispatchService<R, Q>
where
    R: RunsRepository,
    Q: QueueRepository,
{
    /// Take one run from claimed to started.
    ///
    /// `queue_id` is `None` for [`super::launch::Admission::Unqueued`] — a run
    /// with no platform, which has no row to mark.
    ///
    /// # Errors
    ///
    /// Whatever submission failed with. On any failure this releases the claim
    /// and moves the run to a terminal state **before** returning, which is the
    /// obligation `launch::InlineDispatcher` states and the source system's
    /// `Err` arm discharges by calling `mark_failed`
    /// (`manager/src/services/run_dispatcher.rs:47-58`). A failed submit that
    /// left the claim held would stop the queue draining for that platform
    /// entirely.
    ///
    /// **The inverse case returns `Ok`.** If the submit *succeeded* but recording
    /// it failed, returning `Err` would be a lie and could trigger a caller retry
    /// that double-submits, so the failure is logged at ERROR and `Ok` is
    /// returned, accepting a stale claim the next tick's reconciliation releases
    /// (`run_dispatcher.rs:32-44`).
    pub async fn dispatch_one(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        queue_id: Option<Uuid>,
    ) -> Result<(), DomainError> {
        let run = self.read_run(ctx, run_id).await?;
        let run = self.ensure_dispatching(ctx, run).await?;

        match self.submit(ctx, &run).await {
            Ok(started) => {
                self.record_started(ctx, &run, queue_id, started).await;
                Ok(())
            }
            Err(error) => {
                self.record_failed(ctx, &run, queue_id, &error).await;
                Err(error)
            }
        }
    }

    /// Put the run in `dispatching` before any expensive work begins.
    ///
    /// The inline path arrives already `dispatching` (`launch`'s
    /// `dispatch_and_report` transitions first, so the 200 it eventually answers
    /// is about a run that really was claimed), while the tick arrives with a run
    /// in `queued` — or in `created`, which is the state `launch::settle`'s
    /// unrepresentable-outcome arm leaves behind and explicitly expects this tick
    /// to recover from. Both are legal predecessors
    /// (`domain::state_machine::can_transition`).
    ///
    /// It matters that this happens *before* the sync and the bundle build: the
    /// whole reason `dispatching` exists as a distinct state is that it holds the
    /// platform claim across those minutes
    /// (`manager/src/services/run_queue.rs:159-172`).
    async fn ensure_dispatching(
        &self,
        ctx: &SecurityContext,
        run: Run,
    ) -> Result<Run, DomainError> {
        if run.state == RunState::Dispatching {
            return Ok(run);
        }
        self.transition(ctx, &run, RunState::Dispatching, RunStatePatch::default())
            .await?;
        Ok(Run {
            state: RunState::Dispatching,
            ..run
        })
    }

    /// Record a submit that succeeded: reference, then claim, then state, then
    /// the event.
    ///
    /// **Every failure here is logged and swallowed.** The execution is genuinely
    /// running, so an error return would be a lie and could trigger a caller retry
    /// that double-submits (`manager/src/services/run_dispatcher.rs:32-44`). The
    /// cost is a stale claim, which the next tick's reconciliation releases
    /// against the executor.
    ///
    /// The order is `set_execution_ref` then the transition, which is the order
    /// that method's own doc requires: a crash between the two leaves a run that
    /// can be reconciled, whereas the other order leaves a `running` run with no
    /// handle to reconcile against.
    #[allow(
        clippy::cognitive_complexity,
        reason = "inflated by the `tracing` macros in each error arm: every step this \
                  function takes has its own operator-facing line, and the metric counts \
                  each expansion as a branch. Splitting further would separate a log line \
                  from the write it describes, which is the thing being documented. Same \
                  diagnosis as `chat-engine/src/infra/leader/k8s_lease.rs:387`"
    )]
    async fn record_started(
        &self,
        ctx: &SecurityContext,
        run: &Run,
        queue_id: Option<Uuid>,
        started: Started,
    ) {
        let reference = started.execution_ref.as_str().to_owned();
        if let Err(error) = self.record_execution_ref(ctx, run.id, &reference).await {
            error!(
                run_id = %run.id,
                execution_ref = %reference,
                %error,
                "the run started but its execution reference could not be recorded; the \
                 next tick's reconciliation owns the claim",
            );
        }

        if let Some(queue_id) = queue_id
            && let Err(error) = self.queue_write(ctx, queue_id, QueueWrite::Running).await
        {
            error!(%queue_id, %error, "could not mark the queue row running");
        }

        if let Err(error) = self
            .transition(
                ctx,
                run,
                RunState::Running,
                RunStatePatch {
                    started_at: Some(started.started_at),
                    ..RunStatePatch::default()
                },
            )
            .await
        {
            error!(
                run_id = %run.id,
                %error,
                "the run started but could not be recorded as running",
            );
        }

        info!(
            run_id = %run.id,
            execution_ref = %reference,
            "run started",
        );
    }

    /// Record a submit that failed: release the claim, retire the run, hand back
    /// the platform.
    ///
    /// All three matter and the middle one is easy to forget. Legacy marks the
    /// queue row `failed` and has no run row to retire
    /// (`manager/src/services/run_dispatcher.rs:47-58`); here the run is in
    /// `dispatching` and would sit there forever, and the platform lease taken at
    /// admission would keep the platform reading busy.
    ///
    /// **`Error`, not `Failed`.** Both are legal from `dispatching`
    /// (`domain::state_machine::can_transition`), and the distinction is
    /// load-bearing downstream: `Failed` is the verdict
    /// `derive_terminal_state` produces from *test results*, so recording a submit
    /// failure as `Failed` would make a control-plane fault indistinguishable
    /// from a failing test suite in any analysis that groups by state.
    ///
    /// The text written to `qa_runs.error` goes through
    /// [`DomainError::recorded_text`], because that column is served verbatim
    /// by `GET /runs/{id}`, and a [`DomainError::Database`] or
    /// [`DomainError::Catalog`] cause carries another system's raw text.
    #[allow(
        clippy::cognitive_complexity,
        reason = "inflated by the `tracing` macros in each error arm: every step this \
                  function takes has its own operator-facing line, and the metric counts \
                  each expansion as a branch. Splitting further would separate a log line \
                  from the write it describes, which is the thing being documented. Same \
                  diagnosis as `chat-engine/src/infra/leader/k8s_lease.rs:387`"
    )]
    async fn record_failed(
        &self,
        ctx: &SecurityContext,
        run: &Run,
        queue_id: Option<Uuid>,
        cause: &DomainError,
    ) {
        let finished_at = OffsetDateTime::now_utc();
        if !cause.disclosable() {
            warn!(
                run_id = %run.id,
                cause = %cause,
                "the submit failed for a reason the caller is not entitled to see; the run \
                 records an opaque message and this line carries the detail",
            );
        }
        let recorded = cause.recorded_text();

        if let Some(queue_id) = queue_id
            && let Err(error) = self
                .queue_write(ctx, queue_id, QueueWrite::Failed(&recorded))
                .await
        {
            error!(%queue_id, %error, "could not release the claim of a failed submit");
        }

        if let Err(error) = self
            .transition(
                ctx,
                run,
                RunState::Error,
                RunStatePatch {
                    started_at: None,
                    finished_at: Some(finished_at),
                    error: Some(recorded.clone()),
                },
            )
            .await
        {
            warn!(
                run_id = %run.id,
                %error,
                recorded_reason = %recorded,
                "could not retire a run whose submit failed; it stays in `dispatching` \
                 until the orphan guard reclaims it",
            );
        }

        if let Some(platform_id) = run.platform_id {
            self.release_lease(ctx, platform_id, run.id).await;
        }
    }
}

#[async_trait]
impl<R, Q> InlineDispatcher for DispatchService<R, Q>
where
    R: RunsRepository + 'static,
    Q: QueueRepository + 'static,
{
    /// `capacity` is borrowed and never touched: holding it is the caller's
    /// obligation and the borrow is what proves it is still held here. See
    /// [`InlineDispatcher`].
    async fn dispatch_inline(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        queue_id: Option<Uuid>,
        _capacity: &super::admission::CapSlot,
    ) -> Result<(), DomainError> {
        self.dispatch_one(ctx, run_id, queue_id).await
    }
}

// ---------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------

impl<R, Q> DispatchService<R, Q>
where
    R: RunsRepository,
    Q: QueueRepository,
{
    /// One dispatcher cycle (`manager/src/services/run_dispatcher.rs:344-415`).
    ///
    /// Infallible by construction: every pass handles its own failures and folds
    /// them into the report, exactly as legacy's cycle returns `()` and logs. A
    /// tick that propagated would take the ticker with it, and an unsupervised
    /// loop that dies leaves admission queueing rows nothing will ever start —
    /// which, because admission is strict FIFO, blocks every later launch on that
    /// platform indefinitely (`run_dispatcher.rs:324-328`). Task 16 runs each tick
    /// under its own task so a panic costs one tick rather than the dispatcher.
    ///
    /// # The interval is **5 s**, not legacy's 15, and the difference is a
    /// resolved spec conflict
    ///
    /// Legacy ticks every 15 s and states the reason — an admissible run
    /// dispatches inline, so tick latency only ever delays runs that are already
    /// queued (`run_dispatcher.rs:306-311`). That number does not satisfy
    /// `cpt-cf-qa-nfr-dispatch-latency`, which requires a queued run to start
    /// within 10 s of its platform freeing (p95). **Settled at 5 s by user
    /// decision, 2026-08-14** — legacy's own floor
    /// (`run_dispatcher.rs:313`, `interval_seconds.max(5)`), giving p95 ≈ 4.75 s;
    /// `DESIGN.md`'s NFR row now records it.
    ///
    /// The release-notification wake DESIGN originally prescribed is **not built**,
    /// and that is a tracked follow-up rather than an omission
    /// (DECOMPOSITION 2.3's follow-up register): the ticker is leader-elected and
    /// this lock registry is process-local, so a lease released on one replica has
    /// to wake the leader on another, which needs a broker event or a database
    /// signal rather than an in-process notify. This method is written to be
    /// callable on demand so such a wake can drive it unchanged.
    ///
    /// Task 16 owns the knob (`dispatcher_interval_seconds`) and its floor.
    ///
    /// # Drain order is bounded; per-tick time is not
    ///
    /// The threading below bounds *how many* rows a tick claims. What bounds
    /// **whose** is the ordering on `platforms_with_queued_rows`; nothing bounds
    /// how long a tick takes.
    ///
    /// * `platforms_with_queued_rows` orders by each platform's **oldest queued
    ///   row** (`ORDER BY MIN(enqueued_at) ASC`), so the platform that has
    ///   waited longest goes first - the FIFO the queue already promises within
    ///   a platform, applied across them. **Corrected 2026-08-15**: this
    ///   paragraph said the query had *no* `ORDER BY` and that drain order was
    ///   planner-defined, which was true until the ordering landed in the same
    ///   commit that left this text standing.
    /// * With the cap enabled, the budget therefore goes to the longest-waiting
    ///   platforms rather than to whichever sorted first, so a tenant cannot be
    ///   starved by another tenant's position in an arbitrary order. It can
    ///   still be *delayed*: the ordering decides who goes first, not how much
    ///   each takes.
    /// * With the cap **disabled** — the explicit unbounded opt-out, no
    ///   longer the shipped default (`crate::config::QaRunsConfig::max_concurrent_runs`) —
    ///   `plan_dispatch_batch` claims a platform's entire parallel FIFO, and each
    ///   claimed row is dispatched inline below, so one tenant with 20 slow-building
    ///   runs holds the tick for 20 × (force-sync + bundle build). Every other
    ///   tenant's queued rows wait that out. **This is the part that is still
    ///   open**, and it is what `infra::storage::queue_sea_repo`'s ordering
    ///   comment points here for.
    ///
    /// The remaining remedies — a per-platform claim ceiling, a per-tick
    /// deadline, round-robin *within* a tick — are policy this task does not
    /// invent. `the_global_budget_is_threaded_across_platforms` uses one tenant
    /// and cannot see any of it; `the_drain_order_is_oldest_queued_first` covers
    /// the half that landed.
    pub async fn run_tick(&self) -> TickReport {
        let started = Instant::now();
        let (report, outcome) = self.tick_passes().await;
        // Guarded, not called directly: see `super::emit`. The clock is read
        // once around the whole cycle because the cycle is what this family
        // counts — a per-run emission here would time `dispatch_one`'s
        // force-sync and bundle build instead, which is a different question
        // and is not what a dispatcher's RED duration answers.
        //
        // **This is not `cpt-cf-qa-nfr-dispatch-latency`**, and no reading of
        // it can be. That NFR bounds platform release -> execution request,
        // which is dominated by the interval *between* cycles; a cycle's own
        // wall-clock duration cannot contain the gap to the next one.
        // `Self::record_queue_wait` is the family that goes at the NFR.
        emit(&self.metrics_silenced, || {
            self.metrics.dispatch_pass(outcome, started.elapsed());
        });
        report
    }

    /// Every pass of one tick, with how the tick itself ended.
    ///
    /// Split from [`Self::run_tick`] so the emission wraps the whole cycle
    /// rather than being repeated at each of its four early returns — and so
    /// that the outcome is decided **where the tick stopped**, which is the only
    /// place that knows. [`TickReport`] alone cannot answer it: the two stops
    /// that record [`PASS_CAP`] are a full cluster and a failed cap read, which
    /// are opposite answers to *"is this anybody's fault?"* and are
    /// indistinguishable in the report.
    ///
    /// The three values are used as follows, and each is the label
    /// [`DispatchOutcome`]'s own doc describes:
    ///
    /// * [`DispatchOutcome::Completed`] — the cycle reached the drain. The
    ///   ordinary tick, including a tick that found nothing to do.
    /// * [`DispatchOutcome::Refused`] — a rule stopped it: the concurrency cap
    ///   this cycle, or a policy denial, both of which that doc names.
    /// * [`DispatchOutcome::Failed`] — a pass this gear owns failed: the
    ///   execution plane could not be listed, the claim window could not be
    ///   read, the queued platforms could not be enumerated.
    async fn tick_passes(&self) -> (TickReport, DispatchOutcome) {
        let mut report = TickReport::default();

        // First, and before every early return below: expiry needs neither the
        // executor nor the reconciliation, and an executor outage - during which
        // every launch queues, because unreadable occupancy deliberately reads as
        // busy - is exactly when a queued row must still be able to expire
        // (`run_dispatcher.rs:344-351`).
        self.ttl_sweep(&mut report).await;

        // Second, for the identical reason, and *not* last as the plan specifies:
        // reclaiming an overdue run is what frees capacity when the cluster is at
        // its cap, and it depends on none of the steps below. See this module's
        // header.
        self.timeout_sweep(&mut report).await;

        // Third, and ahead of both early returns for the third time. A run that
        // nothing is observing receives no results and can only ever end at its
        // deadline, which is the failure this pass exists to prevent - and the
        // situations the early returns describe are when it matters most. At the
        // cap, observing a run is what *frees* capacity, because a completion
        // releases the claim and the lease. During an executor outage the
        // listing fails and the tick returns before reconciling anything, so a
        // pass placed after it would stop re-attaching for the whole outage and
        // every run that started before it would time out. It depends on neither
        // the listing nor the cap.
        self.reattach_watchers(&mut report).await;

        let active = match self.executor.list_active().await {
            Ok(active) => active,
            Err(error) => {
                // Skipped, not treated as empty: an empty answer would release
                // every claim at once (`run_dispatcher.rs:353-359`).
                report.note_failure(PASS_LIST_ACTIVE, &error);
                report.stopped_at = Some(PASS_LIST_ACTIVE);
                // The one stop with the error still in hand, so the label comes
                // straight from `DomainError::disclosable` through Task 36's
                // bridge rather than from a second reading of the report.
                let outcome = DispatchOutcome::from(&error);
                return (report, outcome);
            }
        };

        // **Read once, here.** Both claim scans in this tick must see the same
        // window: `committed_active` looks each cap-pass row up in the
        // classification the reconciliation pass built, so two disjoint windows
        // make every lookup miss - counting live executions twice and the
        // reconciled window's uncommitted claims not at all. Advancing the
        // cursor inside `reconcile_claims` did exactly that until it was
        // measured; see `advance_claim_scan_cursor`.
        let after = self.claim_scan_cursor();

        let (known, scanned) = self.reconcile_claims(after, &active, &mut report).await;

        // Claims are re-read AFTER reconciliation so released ones are not
        // counted (`run_dispatcher.rs:376-377`), from the **same** window.
        //
        // The denial count is read *before* the pass so the two stops below can
        // tell a denial raised by this pass from one an earlier sweep already
        // recorded; both helpers swallow their error into `note_failure`, so the
        // report is all there is to read afterwards.
        let denials_before_cap = report.denied_passes.len();
        let cap = self.evaluate_cap(after, &active, &known, &mut report).await;

        // Only now: the tick is done reading claims, so moving the cursor
        // cannot make its two reads disagree.
        if let Some(scanned) = scanned {
            self.advance_claim_scan_cursor(&scanned);
        }

        let Some(cap) = cap else {
            report.stopped_at = Some(PASS_CAP);
            // The claim window could not be read. Not the same event as the cap
            // being reached below, though both record `PASS_CAP`.
            let outcome = stopped_outcome(&report, denials_before_cap);
            return (report, outcome);
        };
        if cap_reached(cap) {
            info!(cap = ?cap, "dispatcher waiting: max_concurrent_runs is reached");
            report.stopped_at = Some(PASS_CAP);
            // A configured limit doing its job. `Refused`, never `Failed`: a
            // cluster at `max_concurrent_runs` must not page anybody.
            return (report, DispatchOutcome::Refused);
        }

        let denials_before_enumerate = report.denied_passes.len();
        let Some(platforms) = self.queued_platforms(&mut report).await else {
            report.stopped_at = Some(PASS_ENUMERATE);
            let outcome = stopped_outcome(&report, denials_before_enumerate);
            return (report, outcome);
        };

        // Thread the budget across platforms: `plan_dispatch_batch` counts one
        // platform only, so passing the same cap to each would let every one of
        // them claim `max - active` (`run_dispatcher.rs:406-414`;
        // `GlobalCap::with_claimed` carries the arithmetic and the overshoot
        // formula).
        let mut claimed_so_far = 0u32;
        for platform in platforms {
            let budget = cap.map(|cap| cap.with_claimed(claimed_so_far));
            claimed_so_far = claimed_so_far
                .saturating_add(self.drain_platform(platform, budget, &mut report).await);
        }
        report.claimed = claimed_so_far;
        // The cycle reached the end. Row-level failures inside the sweeps and
        // the drain are logged and counted in the report where they happened;
        // folding them in here would make `Failed` mean "something, somewhere",
        // which is not a signal an alert can be written against.
        (report, DispatchOutcome::Completed)
    }

    /// Expire queued rows past `queue_ttl_seconds`, one tenant at a time.
    ///
    /// # The guarantee is the log line
    ///
    /// The frozen guide promises that an expired run "will never start. A Slack
    /// alert is always sent for this, so it cannot disappear silently" (guide
    /// line 96). The WARN in [`Self::expire_one_row`] is what discharges that
    /// promise: it is written **unconditionally**, before this pass attempts to
    /// move the row's run to [`RunState::Expired`], so the alert still fires on
    /// the failure path — a lost race with a cancel, or a denied scope — where
    /// the run's own row is left unreconciled.
    ///
    /// **One [`SecurityContext`] per expired row**, never one per sweep and never
    /// one per tenant — see [`Self::expire_one_row`], which owns that property.
    ///
    /// The history is worth keeping, because both wrong answers looked right. The
    /// first version claimed per-row and built per-tenant; the correction made the
    /// sentence match the code, which was the wrong direction — the *code* was the
    /// thing that needed fixing, because a covering policy scope makes "the loop's
    /// tenant" and "the row's tenant" differ.
    async fn ttl_sweep(&self, report: &mut TickReport) {
        let now = OffsetDateTime::now_utc();
        let Some(cutoff) = expiry_cutoff(now, self.limits.queue_ttl_seconds) else {
            // Expiry disabled (ttl == 0) or an unusable TTL. Both fail open, for
            // the reason `queue::expiry_cutoff` gives: a stale queued row is an
            // annoyance, a panicking tick stops the queue draining entirely.
            return;
        };

        let enumeration = system_actor::for_ttl_sweep();
        let Some(tenants) = self
            .tenants_with_queued_rows(&enumeration, PASS_TTL, report)
            .await
        else {
            return;
        };

        for tenant in tenants {
            // This context bounds the *statement*; each row is then handled under
            // its own tenant by `expire_one_row`. Same factory, different tenant
            // when a covering scope pulls in a foreign row.
            let scope_ctx = system_actor::for_ttl_expiry(tenant);
            let expired = match self.expire_for_tenant(&scope_ctx, cutoff).await {
                Ok(rows) => rows,
                Err(error) => {
                    report.note_failure(PASS_TTL, &error);
                    continue;
                }
            };
            for row in expired {
                Self::diagnose_foreign_expiry(&row, tenant);
                if self.expire_one_row(&row, now, report).await {
                    report.expired += 1;
                }
            }
        }
    }

    /// Log the deployment misconfiguration that lets a foreign row reach this
    /// sweep. **Diagnostic only — this decides nothing.**
    ///
    /// Titled that way on purpose. A previous revision put the heading *"the tenant
    /// check that makes this pass tenant-safe"* over this comparison, which was a
    /// survivor of the first (skip-the-row) fix where the `if` genuinely carried the
    /// property because it `continue`d. It no longer does: deleting this function
    /// whole leaves all tests green, because what makes the pass tenant-safe is
    /// [`Self::expire_one_row`] binding every write to `row.tenant_id`. Two ways a
    /// wrong headline bites — somebody tidying a log line believes they are deleting
    /// a safety mechanism, and somebody auditing tenant safety stops here, sees only
    /// an `error!`, and concludes the pass is unguarded.
    ///
    /// The analysis is still worth having, and this is it. `expire_queued_before` is
    /// **set-based**: its only tenant constraint is the compiled `AccessScope`, and
    /// the sweep's per-tenant loop changes which context that scope derives from
    /// without constraining the statement. So issuing it once per tenant is
    /// single-tenant only if the PDP narrows a `qa_runs.system` context to its own
    /// `subject_tenant_id`. A deployment that instead grants `qa_runs.system` a
    /// **covering** constraint set for this write action — one that spans every
    /// tenant rather than clamping to the one the context names — hands that
    /// same covering set to every tenant-bound context built from it, and one
    /// statement returns every tenant's rows.
    ///
    /// Measured, not argued: under a covering grant the sweep returned two tenants'
    /// rows in the first iteration. `report.expired` was 2 either way, so no count
    /// could have caught it.
    ///
    /// **This is the only pass whose row set comes from the scope.** The others
    /// drive from a per-row or per-id list — `reconcile_claims`, `timeout_sweep`,
    /// `recover_after_boot`, `drain_platform`, and `fail_orphans_for_tenant`, which
    /// passes only that tenant's ids — and need no equivalent.
    fn diagnose_foreign_expiry(row: &crate::domain::repos::ExpiredRow, bound: TenantBound) {
        if row.tenant_id != bound.get() {
            error!(
                queue_id = %row.id,
                row_tenant = %row.tenant_id,
                bound = %bound.get(),
                "the expiry scope admitted a foreign tenant's row; handling it under its \
                 own tenant. The deployment's policy is granting this gear's system \
                 subject a scope wider than one tenant, which makes every set-based \
                 background write cross-tenant",
            );
        }
    }

    /// Record one expired row, **under the row's own tenant**. `true` when the
    /// row was handled.
    ///
    /// # This is what makes the TTL sweep tenant-safe
    ///
    /// Not the comparison in [`Self::diagnose_foreign_expiry`], which only logs.
    /// The property is here, and it is a contract of this function: every write
    /// for this row is bound to a context minted from **`row.tenant_id`**,
    /// never from whichever tenant's scope the bulk statement happened to run
    /// under.
    ///
    /// **Skipping a foreign row was the first fix and it was wrong.** The bulk
    /// statement has *already* set the row to `expired` by the time it reaches
    /// here, so skipping leaves the queue row expired with its run left in
    /// whichever state it already had — `Created` or `Queued` — for exactly the
    /// row a covering scope pulled in, and no later pass revisits a row the bulk
    /// statement already moved out of `queued`. Found by break-testing:
    /// `expired` came back 1 where the fixture had two expirable rows.
    ///
    /// The WARN above is **unconditional**, regardless of whether the run's own
    /// transition below succeeds: it is what discharges guide line 96, not the
    /// run row's own reconciliation.
    async fn expire_one_row(
        &self,
        row: &crate::domain::repos::ExpiredRow,
        now: OffsetDateTime,
        report: &mut TickReport,
    ) -> bool {
        let Some(row_tenant) = TenantBound::new(row.tenant_id) else {
            warn!(
                queue_id = %row.id,
                "an expired row carries a nil tenant id; skipping it rather than writing \
                 under the platform-root identity",
            );
            return false;
        };
        let ctx = system_actor::for_ttl_expiry(row_tenant);

        let waited = elapsed_seconds(now, row.enqueued_at);
        warn!(
            queue_id = %row.id,
            run_id = %row.run_id,
            platform_id = %row.platform_id,
            waited_seconds = waited,
            ttl_seconds = self.limits.queue_ttl_seconds,
            exclusive = row.exclusive,
            "run-queue row EXPIRED: {EXPIRY_REASON}",
        );

        match self.read_run(&ctx, row.run_id).await {
            Ok(run) => self.expire_run(&ctx, &run, now, report).await,
            Err(error) => report.note_failure(PASS_TTL, &error),
        }

        true
    }

    /// Move an expired row's run to [`RunState::Expired`], through `Queued` if it
    /// never got there.
    ///
    /// # Why two edges rather than one
    ///
    /// `can_transition` admits `Created -> {Queued, Dispatching, Canceled}` —
    /// **not `Expired`**. And a `Created` run with a `queued` row is reachable and
    /// documented: `launch::settle`'s unrepresentable-outcome arm leaves exactly
    /// that, as does any failure of its own `Created -> Queued`, and
    /// [`Self::ensure_dispatching`] is written to recover from it. If this sweep
    /// gets there first, a single `Created -> Expired` attempt fails and the row
    /// goes `expired` while the run stays `Created` — stranded, with no queued row
    /// for any later tick to claim. It fails quietly, too: `IllegalTransition` is
    /// not `Forbidden`, so it lands in the ERROR stream while `denied_passes` stays
    /// empty and `expired` counts the row as handled. Measured before fixing:
    /// `expired=1 row=Expired run=Created denied=[]`.
    ///
    /// **Widening the state machine was the other option and was rejected.**
    /// `DESIGN.md:242` records a user decision (2026-08-13, Task 7) that `expired`
    /// is *"reachable only from `queued`"*, because reusing `canceled` would
    /// conflate the sweep with an operator cancel and reusing `timed_out` would
    /// conflate a queue-wait clock with the execution deadline. Adding
    /// `Created -> Expired` would contradict that in a fix round.
    ///
    /// So the run is walked through the edge it should already have taken. This is
    /// not a workaround: **the row is queued**, so `Queued` is the state the run
    /// was always supposed to be in, and this corrects a stale value rather than
    /// inventing one. Both edges are guarded compare-and-sets, so a concurrent
    /// cancel simply wins the second one and is logged.
    ///
    /// A partial failure — first edge taken, second refused — leaves the run
    /// `Queued` against an `expired` row, which is the same stranded shape one
    /// state over. It is strictly rarer (the second edge is legal, where the
    /// single-edge version was *guaranteed* to fail) and it is recorded at ERROR;
    /// closing it entirely needs the two writes in one transaction, which this
    /// repository shape cannot express.
    async fn expire_run(
        &self,
        ctx: &SecurityContext,
        run: &Run,
        now: OffsetDateTime,
        report: &mut TickReport,
    ) {
        let run = if run.state == RunState::Created {
            warn!(
                run_id = %run.id,
                "an expiring run was still `created`, so its launch never recorded the \
                 queue row it owns; correcting it to `queued` before expiring it",
            );
            if let Err(error) = self
                .transition(ctx, run, RunState::Queued, RunStatePatch::default())
                .await
            {
                report.note_failure(PASS_TTL, &error);
                return;
            }
            Run {
                state: RunState::Queued,
                ..run.clone()
            }
        } else {
            run.clone()
        };

        if let Err(error) = self
            .transition(
                ctx,
                &run,
                RunState::Expired,
                RunStatePatch {
                    started_at: None,
                    finished_at: Some(now),
                    error: Some(EXPIRY_REASON.to_owned()),
                },
            )
            .await
        {
            report.note_failure(PASS_TTL, &error);
        }
    }

    /// One tenant's bulk expiry, under that tenant's own scope.
    ///
    /// **Set-based, and therefore not self-enforcing.** See the tenant check in
    /// [`Self::ttl_sweep`]'s row loop: this statement's only tenant constraint is
    /// the compiled scope, so the caller — not this method — is what keeps a row
    /// from being handled under a foreign tenant. Asserting it here instead was
    /// considered and is not expressible: the method takes no `tenant_id`, only a
    /// scope, so it has nothing to compare a returned row against.
    async fn expire_for_tenant(
        &self,
        ctx: &SecurityContext,
        cutoff: OffsetDateTime,
    ) -> Result<Vec<crate::domain::repos::ExpiredRow>, DomainError> {
        let scope = self.queue_scope(ctx, actions::DISPATCH, None).await?;
        let conn = self.db.conn()?;
        self.queue
            .expire_queued_before(&conn, &scope, cutoff, EXPIRY_REASON)
            .await
    }

    /// The distinct tenants that could hold an expirable queued row.
    ///
    /// `platforms_with_queued_rows` is a cross-tenant **read**, which is what the
    /// nil-tenant enumeration factories are for. Every tenant with a queued row
    /// necessarily has a platform in this answer, so it is exactly the candidate
    /// set — and it is what lets the bulk expiry run per tenant instead of
    /// cross-tenant. See this module's header.
    async fn tenants_with_queued_rows(
        &self,
        enumeration: &SecurityContext,
        pass: &'static str,
        report: &mut TickReport,
    ) -> Option<Vec<TenantBound>> {
        let platforms = match self.list_queued_platforms(enumeration).await {
            Ok(platforms) => platforms,
            Err(error) => {
                report.note_failure(pass, &error);
                return None;
            }
        };
        let mut tenants: Vec<TenantBound> = Vec::new();
        for platform in platforms {
            let Some(tenant) = TenantBound::new(platform.tenant_id) else {
                warn!(
                    platform_id = %platform.platform_id,
                    "a queued row carries a nil tenant id; skipping it rather than writing \
                     under the platform-root identity",
                );
                continue;
            };
            if !tenants.iter().any(|seen| seen.get() == tenant.get()) {
                tenants.push(tenant);
            }
        }
        Some(tenants)
    }

    async fn list_queued_platforms(
        &self,
        // Kept, unused, so both callers' audit-logging `system_actor::for_*`
        // construction still reads as feeding this read.
        _enumeration: &SecurityContext,
    ) -> Result<Vec<QueuedPlatform>, DomainError> {
        // Nil-tenant enumeration: elevated here rather than authorized. See
        // `domain::elevated` for why, and for why the per-row writes that
        // follow are still tenant-bound.
        let scope = crate::domain::elevated::enumeration_scope();
        let conn = self.db.conn()?;
        self.queue.platforms_with_queued_rows(&conn, &scope).await
    }

    /// The platforms this tick will drain, freshly enumerated.
    ///
    /// Read again rather than reusing the TTL sweep's copy: legacy reads it after
    /// the cap check (`run_dispatcher.rs:398-404`), and a platform that gained its
    /// first queued row during the sweep should still drain this tick.
    async fn queued_platforms(&self, report: &mut TickReport) -> Option<Vec<QueuedPlatform>> {
        let enumeration = system_actor::for_dispatch_enumeration();
        match self.list_queued_platforms(&enumeration).await {
            Ok(platforms) => Some(platforms),
            Err(error) => {
                report.note_failure(PASS_ENUMERATE, &error);
                None
            }
        }
    }

    /// Cancel and retire every run past its control-plane deadline.
    ///
    /// `cpt-cf-qa-fr-runs-timeout`. There is no legacy counterpart to port: the
    /// source system relies on Argo's `activeDeadlineSeconds` alone
    /// (`manager/src/services/argo.rs:539`), and the requirement exists because
    /// "enforcement cannot rely on the execution backend alone" (`PRD.md:404`).
    ///
    /// # The claim is released only if the cancel succeeded
    ///
    /// This is the one place where the shape looks like reconciliation and the
    /// fail-safe direction is the opposite. Reconciliation releases a claim
    /// because it has *observed* the execution end; this pass releases because the
    /// control plane has *decided* to end it — and if the cancel could not be
    /// delivered, the execution may well still be running. Releasing its claim and
    /// its lease then is exactly what lets a second run start beside an exclusive
    /// one, which is the failure
    /// `domain::ports::run_executor::RunExecutor`'s trait docs spend three
    /// paragraphs on. So a failed cancel leaves the run, the claim and the lease
    /// alone, logs, and retries on the next tick.
    ///
    /// A run with no execution reference has nothing to cancel and is retired
    /// directly — that is a run whose submit never completed and whose deadline
    /// has since passed.
    async fn timeout_sweep(&self, report: &mut TickReport) {
        let now = OffsetDateTime::now_utc();
        let enumeration = system_actor::for_timeout_sweep();
        let candidates = match self.list_timeout_candidates(&enumeration, now).await {
            Ok(candidates) => candidates,
            Err(error) => {
                report.note_failure(PASS_TIMEOUT, &error);
                return;
            }
        };

        for candidate in candidates.rows {
            let Some(tenant) = TenantBound::new(candidate.tenant_id) else {
                warn!(
                    run_id = %candidate.run_id,
                    "an overdue run carries a nil tenant id; skipping it rather than \
                     writing under the platform-root identity",
                );
                continue;
            };
            let ctx = system_actor::for_timeout_enforcement(tenant);
            if self
                .reclaim_overdue(&ctx, candidate.run_id, now, report)
                .await
            {
                report.timed_out += 1;
            }
        }
    }

    /// Cancel, retire and release one overdue run. `false` means it was left
    /// alone.
    #[allow(
        clippy::cognitive_complexity,
        reason = "inflated by the `tracing` macros in each error arm: every step this \
                  function takes has its own operator-facing line, and the metric counts \
                  each expansion as a branch. Splitting further would separate a log line \
                  from the write it describes, which is the thing being documented. Same \
                  diagnosis as `chat-engine/src/infra/leader/k8s_lease.rs:387`"
    )]
    async fn reclaim_overdue(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        now: OffsetDateTime,
        report: &mut TickReport,
    ) -> bool {
        let run = match self.read_run(ctx, run_id).await {
            Ok(run) => run,
            Err(error) => {
                report.note_failure(PASS_TIMEOUT, &error);
                return false;
            }
        };

        // The fail-safe direction: an undelivered cancel leaves the claim and the
        // lease held, because the execution may still be running.
        if let Some(reference) = run.execution_ref.as_deref()
            && let Err(error) = self.executor.cancel(&ExecutionRef::new(reference)).await
        {
            error!(
                run_id = %run.id,
                execution_ref = reference,
                %error,
                "could not cancel an overdue run; leaving its claim and its lease held, \
                 because the execution may still be running",
            );
            return false;
        }

        if let Err(error) = self
            .transition(
                ctx,
                &run,
                RunState::TimedOut,
                RunStatePatch {
                    started_at: None,
                    finished_at: Some(now),
                    error: Some(TIMEOUT_REASON.to_owned()),
                },
            )
            .await
        {
            report.note_failure(PASS_TIMEOUT, &error);
            return false;
        }

        self.release_claim_for_run(ctx, &run, TIMEOUT_REASON, PASS_TIMEOUT, report)
            .await;
        warn!(
            run_id = %run.id,
            timeout_at = ?run.timeout_at,
            "run reclaimed by the control-plane timeout sweep",
        );
        true
    }

    /// The windowed sweep read, with the "window filled" case logged here
    /// rather than at the caller - the same shape [`Self::all_claims`] uses, so
    /// both windowed reads announce themselves in one place each.
    ///
    /// A filled window is latency, not loss: a run past its deadline is still
    /// past its deadline on the following tick, so the remainder is swept then.
    /// What an operator needs from the line is that overdue runs are being
    /// reclaimed no faster than [`MAX_TIMEOUT_SWEEP_SCAN`] per tick.
    async fn list_timeout_candidates(
        &self,
        // Kept, unused, so the caller's audit-logging `system_actor::for_timeout_sweep`
        // construction still reads as feeding this read.
        _enumeration: &SecurityContext,
        now: OffsetDateTime,
    ) -> Result<Windowed<crate::domain::repos::TimeoutCandidate>, DomainError> {
        // Nil-tenant enumeration: elevated here rather than authorized. See
        // `domain::elevated` for why, and for why the per-row writes that
        // follow are still tenant-bound.
        let scope = crate::domain::elevated::enumeration_scope();
        let conn = self.db.conn()?;
        let after = *self
            .timeout_scan_cursor
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let candidates = self
            .runs
            .list_timeout_candidates(&conn, &scope, now, after)
            .await?;
        // Advance, wrapping on a short window - the same rule as
        // `advance_claim_scan_cursor`, and for the same reason.
        *self
            .timeout_scan_cursor
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = if candidates.truncated {
            candidates.rows.last().map(|candidate| candidate.run_id)
        } else {
            None
        };
        if candidates.truncated {
            info!(
                scan_window = MAX_TIMEOUT_SWEEP_SCAN,
                resumed_after = ?after,
                "the timeout sweep filled its scan window; the remaining overdue runs are \
                 covered by the following sweeps, which resume where this one stopped",
            );
        }
        Ok(candidates)
    }

    /// Attach a result observer to every live run this process is not already
    /// observing.
    ///
    /// **This is the only thing that drives `service::ingest`**, and the only
    /// path by which a run reaches a terminal state from an
    /// `ExecutionEvent::Finished` rather than from its deadline. `service::watch`
    /// carries why there is one attach path and not two.
    ///
    /// # It is a tick pass, not a boot pass — an unflagged deviation, now flagged
    ///
    /// Task 16c's Step 2 says to run this *"into the leader-gated ticker beside
    /// `recover_after_boot`"*, which is a once-per-leadership-term pass. It is
    /// third in [`Self::run_tick`] instead, and that is a departure the first
    /// revision argued at length **within** the tick without ever naming as a
    /// departure at all.
    ///
    /// The reason it has to repeat: `recover_after_boot` answers a question that
    /// is only asked once — what did *this* restart leave behind — while this one
    /// answers a question that becomes true again continuously. An observer ends
    /// whenever its stream does, a run that started after the boot pass has none,
    /// and under a real elector a run dispatched inline on a non-leader replica
    /// never had one. A boot-only version would observe the runs alive at
    /// leadership acquisition and nothing afterwards, which is the timeout-only
    /// path restored for every run started since.
    ///
    /// # It is inside the leader gate because it runs inside the tick
    ///
    /// `crate::gear` runs `run_tick` through `LeaderElector::run_role`, so this
    /// pass inherits that gate — which is what keeps one observer per run when a
    /// real elector is deployed and a non-leader replica dispatches a run
    /// inline. **Under the shipped `NoopLeaderElector` the gate is vacuous**: every
    /// replica believes it leads, so N replicas attach N observers to the same
    /// run and `IngestService` gets N producers. That is the same pre-existing
    /// hazard `infra::leader` documents for `recover_after_boot` and the
    /// claim-scan cursor, with the same fix, and this pass neither creates nor
    /// worsens it. **And a deployment with `dispatcher_enabled: false` — which
    /// is the reference dev stack — observes nothing at all**, because there is
    /// no tick to carry this pass; runs there still launch and dispatch inline,
    /// and still end only at their deadline.
    ///
    /// # Enumerate cross-tenant, read and write per tenant
    ///
    /// The scan is `system_actor::for_watch_scan()`, a nil-tenant read, and
    /// **every** step after it — the candidate read here, and every repository
    /// call the observer's ingest path then makes — runs under
    /// `for_result_ingest(TenantBound)` minted from the scanned row's own
    /// `tenant_id`. Never from a caller's context: `service::ingest` stamps
    /// `ctx.subject_tenant_id()` into `qa_run_test_results.tenant_id`, so a
    /// context bound to anything but the run's own tenant writes that run's
    /// per-test rows under the wrong one.
    ///
    /// The nil check is `TenantBound::new`, fail-closed and **visible**, exactly
    /// as every other pass in this module does it.
    ///
    /// # No terminal-state guard, and why none is needed
    ///
    /// A run can go terminal between the scan and the read, and this pass will
    /// then attach an observer to a finished execution. That is harmless rather
    /// than unguarded: the replay's `Finished` reconciles to the state already
    /// recorded, and `IngestService::finish` answers `AlreadyRecorded` and writes
    /// nothing — the contract it acquired precisely so a retried completion is a
    /// no-op instead of a 409. A guard here would be one more branch no test at
    /// this tier could falsify, which is how this subsystem accumulated its
    /// inert guards.
    async fn reattach_watchers(&self, report: &mut TickReport) {
        let enumeration = system_actor::for_watch_scan();
        let candidates = match self.list_watch_candidates(&enumeration).await {
            Ok(candidates) => candidates,
            Err(error) => {
                report.note_failure(PASS_WATCH, &error);
                return;
            }
        };

        for candidate in candidates.rows {
            let Some(tenant) = TenantBound::new(candidate.tenant_id) else {
                warn!(
                    run_id = %candidate.run_id,
                    "a live run carries a nil tenant id; skipping it rather than observing \
                     it under the platform-root identity",
                );
                continue;
            };
            // Cheapest question first. A run already being observed costs
            // neither a policy decision nor a query, which is what makes a scan
            // over *every* live run affordable on a five-second cadence.
            if self.watcher.is_watching(candidate.run_id) {
                continue;
            }
            let ctx = system_actor::for_result_ingest(tenant);
            let run = match self.read_run(&ctx, candidate.run_id).await {
                Ok(run) => run,
                Err(error) => {
                    report.note_failure(PASS_WATCH, &error);
                    continue;
                }
            };
            // The scan matched on `execution_ref IS NOT NULL` and this read is
            // later, so `None` here means the row changed underneath - not a
            // fault, and nothing to watch either way.
            let Some(reference) = run.execution_ref.as_deref() else {
                continue;
            };
            self.watcher.attach(WatchTarget {
                run_id: run.id,
                tenant,
                execution_ref: ExecutionRef::new(reference),
            });
            report.attached += 1;
        }
    }

    /// The windowed re-attachment read, with the "window filled" case logged
    /// here - the same shape [`Self::list_timeout_candidates`] and
    /// [`Self::all_claims`] use.
    ///
    /// A filled window costs more here than it does there. The other two leave
    /// work that is already late a little later; this one leaves a run
    /// **unobserved** for up to a full rotation, and what the executor emitted in
    /// that window survives only if `RunExecutor::watch` genuinely resumes.
    ///
    /// **Both directions are falsifiable now, and neither was before Task 13
    /// (review finding #50).** Before it, `MockRunExecutor` satisfied
    /// re-attach by replaying every event from the beginning regardless of
    /// what `watch` was given — stronger than the port requires in the
    /// no-loss direction, but it meant nothing in this crate could falsify an
    /// adapter that dropped the gap, *and* the mock could not have caught the
    /// opposite defect either, because it had no notion of "already sent"
    /// to duplicate. `watch` now carries a `LogResume`
    /// (`domain::repos::LogResume`), the mock replays from that position
    /// exactly — it holds its own script, so there is no approximation to
    /// make — and `watch_tests::a_reattach_does_not_duplicate_the_archived_log`
    /// is what a re-attach that replayed in full, as this method's own
    /// `reattach_watchers` caller now exercises on every unwatched live run,
    /// would fail. `mock::watch_resumes_without_duplicating_or_dropping_log_lines`
    /// is the mock-level pin for the same two directions. **Fix-round 1**:
    /// the Argo adapter's first version instead asked Kubernetes to filter by
    /// `LogParams::since_time`, using the archive row's `updated_at` as a
    /// stand-in for a node's last archived line — review found that compares
    /// the control plane's write clock against each line's own kubelet
    /// emission time, two different events, and can *lose* a line queued
    /// behind a database round trip when the observer ends before it
    /// flushes, which is worse than the bug this task fixes. The shipped
    /// mechanism (`infra::executor::argo::watch`'s `LineSkip`) has no clock
    /// in it: it re-reads a node's log from byte 0, exactly as before this
    /// task, and suppresses the same count the mock does — see
    /// `domain::repos::LogPosition`'s doc for why a count can only
    /// under-suppress (re-duplicating a little, the tolerated direction)
    /// and never over-suppress relative to what actually reached the pod's
    /// log.
    async fn list_watch_candidates(
        &self,
        // Kept, unused, so the caller's audit-logging `system_actor::for_watch_scan`
        // construction still reads as feeding this read.
        _enumeration: &SecurityContext,
    ) -> Result<Windowed<WatchCandidate>, DomainError> {
        // Nil-tenant enumeration: elevated here rather than authorized. See
        // `domain::elevated` for why. This read is the elevated half of that
        // pairing; every step after the scan is tenant-bound -- see this
        // method's caller for the write side.
        let scope = crate::domain::elevated::enumeration_scope();
        let conn = self.db.conn()?;
        let after = *self
            .watch_scan_cursor
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let candidates = self
            .runs
            .list_watch_candidates(&conn, &scope, after)
            .await?;
        // Advance, wrapping on a short window - the same rule as
        // `advance_claim_scan_cursor`, and for the same reason.
        *self
            .watch_scan_cursor
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = if candidates.truncated {
            candidates.rows.last().map(|candidate| candidate.run_id)
        } else {
            None
        };
        if candidates.truncated {
            info!(
                scan_window = MAX_WATCH_SCAN,
                resumed_after = ?after,
                "the watcher re-attachment scan filled its window; the remaining live runs \
                 are covered by the following ticks, which resume where this one stopped",
            );
        }
        Ok(candidates)
    }

    /// Release the queue claim a run holds, and hand its platform back.
    ///
    /// `QueueRepository` has no "the claim for this run" lookup, so the claim is
    /// found through `claims_for_platform` under the run's own tenant scope —
    /// which is a small, tenant-correct query rather than the cross-tenant
    /// `all_claims`. A run with no platform has no claim and no lease.
    async fn release_claim_for_run(
        &self,
        ctx: &SecurityContext,
        run: &Run,
        reason: &str,
        pass: &'static str,
        report: &mut TickReport,
    ) {
        let Some(platform_id) = run.platform_id else {
            return;
        };
        let claims = match self.claims_for_platform(ctx, platform_id).await {
            Ok(claims) => claims,
            Err(error) => {
                report.note_failure(pass, &error);
                return;
            }
        };
        if let Some(claim) = claims.into_iter().find(|claim| claim.run_id == run.id)
            && let Err(error) = self
                .queue_write(ctx, claim.id, QueueWrite::Failed(reason))
                .await
        {
            report.note_failure(pass, &error);
        }
        self.release_lease(ctx, platform_id, run.id).await;
    }

    /// Unreleased claims on one platform, under the caller's own scope.
    async fn claims_for_platform(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
    ) -> Result<Vec<crate::domain::repos::ClaimRow>, DomainError> {
        let scope = self.queue_scope(ctx, actions::LIST, None).await?;
        let conn = self.db.conn()?;
        self.queue
            .claims_for_platform(&conn, &scope, platform_id)
            .await
    }

    /// Release claims whose execution is terminal or gone, and fail the ones a
    /// restart left mid-dispatch past the orphan timeout.
    ///
    /// Returns what it observed, keyed by run, so [`committed_active`] does not
    /// have to re-read every claim's execution reference.
    ///
    /// The `orphan_timeout` age guard is load-bearing and
    /// `domain::state_machine::reconcile_claim` owns the rule: a row sits in
    /// `dispatching` with no execution reference for the whole force-sync +
    /// bundle-build window, which is minutes, and failing it early abandons a
    /// launch that is still in progress — momentarily releasing its claim, which
    /// is exactly when a second run could be admitted alongside an exclusive one
    /// (`manager/src/services/run_dispatcher.rs:417-426`).
    ///
    /// **A claim whose run cannot be read is kept**, not released: releasing on no
    /// information is the dangerous direction, and it is deliberately left out of
    /// the returned map so [`committed_active`] charges it against the cap.
    ///
    /// # What a full claim scan costs this pass
    ///
    /// A claim past [`MAX_CLAIM_SCAN`] is not reconciled *this tick*, so a
    /// platform whose execution has already finished stays held until a later
    /// tick reaches it. **Which it will**: the scan resumes where the previous
    /// one stopped and wraps, so the wait is at most one full cycle -
    /// `ceil(claims / MAX_CLAIM_SCAN)` ticks, twenty-five seconds at the default
    /// cadence for five thousand claims.
    ///
    /// **Corrected 2026-08-15.** This paragraph previously said the scan was
    /// "ordered oldest first" and that "every release shortens it, so the
    /// backlog drains rather than wedging". That was false, and vacuously so:
    /// [`reconcile_claim`] answers `Keep` for a healthy claim, `Keep` performs
    /// no write, so there were no releases to shorten anything and one tenant's
    /// healthy claims excluded every other tenant's rows indefinitely. See
    /// `QueueRepository::all_claims` for the measurement and the ordering that
    /// replaced it.
    #[allow(
        clippy::cognitive_complexity,
        reason = "inflated by the `tracing` macros in each error arm: every step this \
                  function takes has its own operator-facing line, and the metric counts \
                  each expansion as a branch. Splitting further would separate a log line \
                  from the write it describes, which is the thing being documented. Same \
                  diagnosis as `chat-engine/src/infra/leader/k8s_lease.rs:387`"
    )]
    async fn reconcile_claims(
        &self,
        after: Option<Uuid>,
        active: &BTreeSet<ExecutionRef>,
        report: &mut TickReport,
    ) -> (HashMap<Uuid, ClaimExecution>, Option<ScanEnd>) {
        let mut known = HashMap::new();
        let enumeration = system_actor::for_claim_reconciliation();
        let Some(window) = self
            .all_claims(&enumeration, after, PASS_RECONCILE, report)
            .await
        else {
            return (known, None);
        };
        // Handed **back**, not applied here: the caller advances the cursor once
        // both scans have read, so the cap pass sees the same slice this
        // classification describes.
        let scanned = ScanEnd {
            truncated: window.truncated,
            last: window.rows.last().map(|claim| claim.id),
        };
        let now = OffsetDateTime::now_utc();

        for claim in window.rows {
            let Some(tenant) = TenantBound::new(claim.tenant_id) else {
                warn!(
                    queue_id = %claim.id,
                    "a claim carries a nil tenant id; skipping it rather than writing under \
                     the platform-root identity",
                );
                continue;
            };
            let ctx = system_actor::for_claim_release(tenant);
            let run = match self.read_run(&ctx, claim.run_id).await {
                Ok(run) => run,
                Err(error) => {
                    report.note_failure(PASS_RECONCILE, &error);
                    continue;
                }
            };
            let execution = classify_execution(&run, active);
            known.insert(claim.run_id, execution);

            let observation = ClaimObservation {
                execution,
                age_seconds: elapsed_seconds(now, claim.age_basis),
            };
            match reconcile_claim(observation, self.orphan_timeout_seconds) {
                ClaimAction::Keep => {}
                ClaimAction::Release => {
                    self.release_claim(&ctx, &claim, &run, report).await;
                    report.released += 1;
                }
                ClaimAction::FailOrphaned => {
                    warn!(
                        queue_id = %claim.id,
                        run_id = %claim.run_id,
                        age_seconds = observation.age_seconds,
                        orphan_timeout_seconds = self.orphan_timeout_seconds,
                        "failing an orphaned run-queue row: dispatching with no execution",
                    );
                    self.fail_orphan(&ctx, &claim, &run, report).await;
                    report.failed_orphans += 1;
                }
            }
        }
        (known, Some(scanned))
    }

    /// Terminal or deleted — either way the platform is free. This is what stops
    /// a stuck execution blocking the queue forever
    /// (`manager/src/services/run_dispatcher.rs:448-456`).
    ///
    /// The run row is **not** transitioned here. Reconciliation knows the
    /// execution is over but not how it ended, and
    /// `domain::state_machine::derive_terminal_state` needs an
    /// `ExecutorOutcome` plus the ingested counts to answer that. Ingest (Task 15)
    /// owns the run's terminal state; this pass owns the platform.
    async fn release_claim(
        &self,
        ctx: &SecurityContext,
        claim: &ClaimAge,
        run: &Run,
        report: &mut TickReport,
    ) {
        if let Err(error) = self.queue_write(ctx, claim.id, QueueWrite::Done).await {
            report.note_failure(PASS_RECONCILE, &error);
            return;
        }
        self.release_lease(ctx, claim.platform_id, run.id).await;
    }

    /// Fail a row left mid-dispatch, retire its run, and free its platform.
    ///
    /// # The run is retired only if it has not already finished
    ///
    /// **Added 2026-08-14, as the collateral of a state-machine widening.** The
    /// user's decision to open `Succeeded -> {Failed, Error, Canceled, TimedOut}`
    /// (`domain::state_machine::can_transition`) makes `Succeeded -> Error` legal,
    /// and this is the one pass in the gear that can be handed an already-terminal
    /// run: `reconcile_claim` answers `FailOrphaned` for a claim whose row carries
    /// no execution reference, and a run *can* be `Succeeded` with no reference
    /// recorded, because [`Self::record_started`] deliberately swallows a failed
    /// `set_execution_ref` (the execution is genuinely running, so an error return
    /// would be a lie).
    ///
    /// Before the widening the state machine refused that write and the run's
    /// verdict survived by accident. Refusing it here is the same protection, made
    /// deliberate: an orphan sweep must never overwrite a verdict ingest already
    /// derived from real results.
    ///
    /// The **claim** is still failed and the **lease** still released — the row is
    /// genuinely orphaned whatever the run says, and leaving it would keep the
    /// platform busy.
    async fn fail_orphan(
        &self,
        ctx: &SecurityContext,
        claim: &ClaimAge,
        run: &Run,
        report: &mut TickReport,
    ) {
        if let Err(error) = self
            .queue_write(ctx, claim.id, QueueWrite::Failed(ORPHAN_REASON))
            .await
        {
            report.note_failure(PASS_RECONCILE, &error);
            return;
        }
        if is_terminal(run.state) {
            warn!(
                run_id = %run.id,
                queue_id = %claim.id,
                state = run.state.as_str(),
                "an orphaned claim belongs to a run that has already finished; releasing \
                 the claim and the platform without touching the recorded verdict",
            );
        } else if let Err(error) = self
            .transition(
                ctx,
                run,
                RunState::Error,
                RunStatePatch {
                    started_at: None,
                    finished_at: Some(OffsetDateTime::now_utc()),
                    error: Some(ORPHAN_REASON.to_owned()),
                },
            )
            .await
        {
            report.note_failure(PASS_RECONCILE, &error);
        }
        self.release_lease(ctx, claim.platform_id, run.id).await;
    }

    /// Where the next claim scan resumes.
    ///
    /// **Read once per tick, by `run_tick`, and threaded to both scans.**
    /// Calling this a second time inside a pass is the bug it was written with:
    /// the cursor moves during the tick, so a second read hands the cap pass a
    /// different window than the classification it is about to be given.
    fn claim_scan_cursor(&self) -> Option<Uuid> {
        *self
            .claim_scan_cursor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Move the cursor past `scanned`, wrapping when the window came back short.
    ///
    /// A short window means the id space is exhausted, so the next scan starts
    /// over from the beginning; a full one means there is more, so the next scan
    /// resumes after the last row seen. **The wrap is the half that is easy to
    /// lose**: pin the cursor at the end of the id space instead of resetting
    /// it and every later scan returns zero rows forever, no claim is ever
    /// reconciled again, and every platform lease leaks permanently.
    ///
    /// Called **once per tick, by `run_tick`, after both scans have read.**
    /// Calling it from inside the reconciliation pass - which is what shipped -
    /// moves the cursor between the two reads.
    fn advance_claim_scan_cursor(&self, scanned: &ScanEnd) {
        let next = if scanned.truncated {
            scanned.last
        } else {
            None
        };
        *self
            .claim_scan_cursor
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = next;
    }

    async fn read_all_claims(
        &self,
        // Kept, unused, so every caller's audit-logging
        // `system_actor::for_claim_reconciliation` construction still reads
        // as feeding this read.
        _enumeration: &SecurityContext,
        after: Option<Uuid>,
    ) -> Result<Windowed<ClaimAge>, DomainError> {
        // Nil-tenant enumeration: elevated here rather than authorized. See
        // `domain::elevated` for why, and for why the per-row writes that
        // follow are still tenant-bound.
        let scope = crate::domain::elevated::enumeration_scope();
        let conn = self.db.conn()?;
        self.queue.all_claims(&conn, &scope, after).await
    }

    /// [`Self::read_all_claims`], with the failure folded into the report.
    ///
    /// A full window is **not** folded into the report and does not stop the
    /// pass. It is logged at INFO, once per read, naming the window and where
    /// the scan resumed: on a cluster with more claims than one scan holds, a
    /// full window is the *normal* state, not a symptom, and the rest are
    /// covered by the following scans.
    ///
    /// **Corrected 2026-08-15.** This paragraph described the mechanism the
    /// rotation replaced, in three separate ways: the log was ERROR, it named
    /// `max_concurrent_runs`, and a truncated window "still carries the stalest
    /// claims". The last was the justification for proceeding on a partial scan
    /// under the old `enqueued_at ASC` ordering, and it survived into an
    /// ordering that deliberately abandoned age - **a window is now an
    /// arbitrary slice of the id space with no age property at all.** A reader
    /// debugging a held platform would have hunted for an ERROR that is never
    /// emitted and assumed the partial window was age-prioritised.
    ///
    /// What each pass loses is documented at its own call site.
    async fn all_claims(
        &self,
        enumeration: &SecurityContext,
        after: Option<Uuid>,
        pass: &'static str,
        report: &mut TickReport,
    ) -> Option<Windowed<ClaimAge>> {
        match self.read_all_claims(enumeration, after).await {
            Ok(claims) => {
                if claims.truncated {
                    // INFO, not ERROR. A full window is the *normal* state of a
                    // cluster with more claims than one scan holds, and the
                    // rotation means it is no longer a symptom of anything: the
                    // rest are covered by the following scans. This line shipped
                    // at ERROR on 2026-08-15, when a full window really did mean
                    // rows were being permanently skipped.
                    info!(
                        pass,
                        scan_window = MAX_CLAIM_SCAN,
                        resumed_after = ?after,
                        "the dispatcher's claim scan filled its window; the remaining claims \
                         are covered by the following scans, which resume where this one \
                         stopped",
                    );
                }
                Some(claims)
            }
            Err(error) => {
                report.note_failure(pass, &error);
                None
            }
        }
    }

    /// The tick's cluster-wide budget, or `None` when it could not be evaluated.
    ///
    /// `Some(None)` is a *disabled* cap and `None` is a *failed* evaluation — the
    /// two must not collapse, because a failed evaluation must stop the tick
    /// while a disabled one must not.
    ///
    /// **It mints `for_claim_reconciliation()`, so the audit line reads
    /// `site="claim_reconciliation"` for a read the *cap* pass made** — and this is
    /// the pass whose `stopped_at` is [`PASS_CAP`], so the two names disagree in the
    /// logs. Deliberate: the factory is shared because the *query* is identical
    /// (`all_claims`, cross-tenant, read-only), and `domain::system_actor`'s
    /// one-factory-per-call-site rule exists to make elevation greppable, not to
    /// mirror the caller's name. A further factory would trip that module's
    /// classification speed bump — which is there to force a look at the
    /// enumeration/write split — for a read that makes no writes and reuses an
    /// existing enumeration identity.
    ///
    /// # `after` is the tick's, not this pass's
    ///
    /// It is a parameter rather than a cursor read, because the classification
    /// in `known` was built over exactly one window and this count is only
    /// meaningful against the same one.
    ///
    /// # What a truncated claim scan costs this pass, and why it does not stop it
    ///
    /// A claim outside this tick's window is not counted, so `committed` is an
    /// under-count and the cap is that much more permissive. The error is
    /// bounded well below the number of claims outside the window:
    /// `active.len()` comes from the executor listing, which the window does not
    /// touch, so what is dropped is only the *uncommitted* claims - those with
    /// no live execution - outside it.
    ///
    /// **This affects the dispatcher's per-tick budget and nothing else.** The
    /// launch path's `max_concurrent_runs` refusal is
    /// `admission::enforce_global_cap`, which counts `executor.list_active()`
    /// and never reads a claim, so the 429 a caller sees is exact regardless.
    ///
    /// Refusing to evaluate on a full scan was considered and rejected: it would
    /// stop the tick entirely, and the tick is what reconciles claims - so a
    /// cluster that once exceeded the window would stop making progress toward
    /// being under it. An over-permissive per-tick budget is a resource
    /// decision; a wedged dispatcher is an outage.
    async fn evaluate_cap(
        &self,
        after: Option<Uuid>,
        active: &BTreeSet<ExecutionRef>,
        known: &HashMap<Uuid, ClaimExecution>,
        report: &mut TickReport,
    ) -> Option<Option<GlobalCap>> {
        let enumeration = system_actor::for_claim_reconciliation();
        // `after` is the tick's, passed in - **not** re-read from the cursor.
        // Re-reading it here is precisely the regression that made the two
        // scans disjoint; see `DispatchService::run_tick`.
        let window = self
            .all_claims(&enumeration, after, PASS_CAP, report)
            .await?;
        let committed = committed_active(active, &window.rows, known);
        Some(global_cap_status(
            u32::try_from(committed).unwrap_or(u32::MAX),
            self.limits.max_concurrent_runs,
        ))
    }

    /// Claim and dispatch every admissible row for one platform.
    ///
    /// Returns how many rows this platform claimed, so the caller can spend them
    /// from the shared budget before draining the next platform.
    ///
    /// # What is inside the lock, and the one thing that had to move
    ///
    /// Inside: the occupancy read, the FIFO read, the plan, the row claims, and
    /// the **lease acquisitions**. Outside: the dispatch, because that is where
    /// the force-sync and the bundle build happen and holding the lock across them
    /// would serialise every launch on the platform for minutes
    /// (`manager/src/services/run_dispatcher.rs:618-620`).
    ///
    /// Legacy claims rows inside the lock "so a concurrent launch sees these as
    /// occupancy" (`:606`). Here the claim is *not* an occupancy source — the
    /// lease is — so the acquisition is what has to be inside, and it is. See
    /// `service::admission`'s header.
    ///
    /// # The extra read, and the repository shape that forces it
    ///
    /// `plan_dispatch_batch` returns queue-row ids, and acquiring a lease needs
    /// the **run** id. `queue::QueuedRow` does not carry one, so the run id is
    /// recovered from `claims_for_platform` *after* the rows are marked
    /// `dispatching` — which is when they become claims. That is one extra query
    /// inside the critical section, and it exists only because the FIFO row shape
    /// omits `run_id`. Reported to the coordinator: widening `QueuedRow` (Task 6's
    /// file) or `queued_rows` (Task 10's) would remove it.
    ///
    /// **Corrected by the observability task.** This said `QueuedRow` "carries
    /// only `{id, exclusive}`", which stopped being true in the same commit that
    /// added `enqueued_at` to it for the queue-wait metric and left this
    /// sentence standing. The premise the extra read rests on is narrower than
    /// that and is unchanged: the type has no `run_id`. Nor does widening it for
    /// the metric make the read removable — `enqueued_at` is a timestamp, not
    /// the id the lease needs.
    async fn drain_platform(
        &self,
        platform: QueuedPlatform,
        budget: Option<GlobalCap>,
        report: &mut TickReport,
    ) -> u32 {
        let Some(tenant) = TenantBound::new(platform.tenant_id) else {
            warn!(
                platform_id = %platform.platform_id,
                "a queued platform carries a nil tenant id; skipping it rather than \
                 dispatching under the platform-root identity",
            );
            return 0;
        };
        let ctx = system_actor::for_dispatch(tenant);
        let platform_id = platform.platform_id;

        let claimed = self.claim_batch(&ctx, platform_id, budget, report).await;
        let count = u32::try_from(claimed.len()).unwrap_or(u32::MAX);

        // Outside the lock.
        for row in claimed {
            if let Err(error) = self
                .dispatch_one(&ctx, row.run_id, Some(row.queue_id))
                .await
            {
                // `dispatch_one` has already released the claim and retired the
                // run; nothing further to do but record it.
                warn!(
                    queue_id = %row.queue_id,
                    run_id = %row.run_id,
                    exclusive = row.exclusive,
                    %error,
                    "a claimed row failed to dispatch",
                );
            } else {
                self.record_queue_wait(row);
            }
        }
        count
    }

    /// Report how long a queued run waited to reach an execution request.
    ///
    /// # Why this one is per run when `dispatch_pass` is per cycle
    ///
    /// Because the wait *is* a property of a run and has no other unit. The
    /// cardinality objection that keeps `dispatch_pass` at the cycle boundary
    /// does not apply: the family carries no label, so it is one series however
    /// many runs pass through it, and what grows with the run count is the
    /// sample count — which is the point of a histogram.
    ///
    /// # Emitted only on success, and only for rows that were really queued
    ///
    /// * **Only on success**, because the series measures the wait *to an
    ///   execution request*. A row whose dispatch failed never made one, and
    ///   folding its time-to-failure into the same histogram would mix two
    ///   distributions — the failure is already counted by the WARN above and
    ///   by the run's own retirement.
    /// * **Only for drained rows.** This is reached from the tick's drain and
    ///   nowhere else, so a launch that admission dispatched inline — which
    ///   never enters the FIFO — and a platformless launch — which is never
    ///   queued at all — contribute nothing. That is the wanted population:
    ///   `cpt-cf-qa-nfr-dispatch-latency` is stated over *queued* runs.
    ///
    /// The instant is read here rather than passed in because this is the
    /// first moment after `dispatch_one` returned an accepted execution that the
    /// drain can observe. **It is not the end point the NFR names**, and it is
    /// not the acceptance instant either: `submit` stamps that inside
    /// `dispatch_one`, which returns `Result<(), _>` and keeps it, so what is
    /// read here is later by three scoped writes. The error is one-signed — it
    /// can only inflate. What the measurement is, and where it diverges from
    /// `cpt-cf-qa-nfr-dispatch-latency` in both directions, is in
    /// [`crate::domain::metrics::QA_RUNS_QUEUE_WAIT_DURATION`]'s own doc.
    fn record_queue_wait(&self, row: ClaimedRow) {
        let Some(enqueued_at) = row.enqueued_at else {
            return;
        };
        // A negative span means the row's stored instant is ahead of this
        // clock — clock skew between writers, or a fixture. `try_into` refuses
        // it, and the sample is dropped rather than clamped to zero: a zero
        // would be a real-looking observation of something that did not happen.
        let Ok(waited) = std::time::Duration::try_from(OffsetDateTime::now_utc() - enqueued_at)
        else {
            return;
        };
        emit(&self.metrics_silenced, || self.metrics.queue_wait(waited));
    }

    /// The critical section: decide, claim, and take the leases.
    #[allow(
        clippy::cognitive_complexity,
        reason = "inflated by the `tracing` macros in each error arm: every step this \
                  function takes has its own operator-facing line, and the metric counts \
                  each expansion as a branch. Splitting further would separate a log line \
                  from the write it describes, which is the thing being documented. Same \
                  diagnosis as `chat-engine/src/infra/leader/k8s_lease.rs:387`"
    )]
    async fn claim_batch(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        budget: Option<GlobalCap>,
        report: &mut TickReport,
    ) -> Vec<ClaimedRow> {
        let lock = self.locks.get(platform_id).await;
        let _guard = lock.lock().await;

        let occupancy = lease_occupancy(self.environments.as_ref(), ctx, platform_id).await;
        let fifo = match self.queued_rows(ctx, platform_id).await {
            Ok(rows) => rows,
            Err(error) => {
                report.note_failure(PASS_DRAIN, &error);
                return Vec::new();
            }
        };

        let planned = plan_dispatch_batch(occupancy, &fifo, budget);
        if planned.is_empty() {
            return Vec::new();
        }

        let mut marked = Vec::with_capacity(planned.len());
        for id in planned {
            match self.queue_write(ctx, id, QueueWrite::Claimed).await {
                Ok(true) => marked.push(id),
                Ok(false) => warn!(queue_id = %id, "run-queue row was no longer queued; skipping"),
                Err(error) => report.note_failure(PASS_DRAIN, &error),
            }
        }
        if marked.is_empty() {
            return Vec::new();
        }

        // The rows are claims now, so this is where their run ids live.
        let run_ids: HashMap<Uuid, Uuid> = match self.claims_for_platform(ctx, platform_id).await {
            Ok(claims) => claims
                .into_iter()
                .map(|claim| (claim.id, claim.run_id))
                .collect(),
            Err(error) => {
                report.note_failure(PASS_DRAIN, &error);
                return Vec::new();
            }
        };

        let mut claimed = Vec::with_capacity(marked.len());
        for queue_id in marked {
            let Some(&run_id) = run_ids.get(&queue_id) else {
                // The row was claimed but its run id could not be recovered. Left
                // `dispatching` deliberately: the claim keeps the platform busy,
                // which is the fail-safe direction, and the orphan guard reclaims
                // it once `orphan_timeout_seconds` has passed.
                error!(
                    %queue_id,
                    %platform_id,
                    "a claimed run-queue row has no readable run id; leaving it for the \
                     orphan guard rather than dispatching blind",
                );
                continue;
            };
            // One lookup, two readers. Fail closed if the row somehow left the
            // FIFO snapshot: an exclusive lease is the most restrictive
            // request, and an absent enqueue instant means no queue-wait sample
            // rather than a guessed one.
            let planned_row = fifo.iter().find(|row| row.id == queue_id);
            let exclusive = planned_row.is_none_or(|row| row.exclusive);
            let enqueued_at = planned_row.map(|row| row.enqueued_at);
            if self
                .take_lease_for_claim(ctx, platform_id, run_id, exclusive, report)
                .await
            {
                claimed.push(ClaimedRow {
                    queue_id,
                    run_id,
                    exclusive,
                    enqueued_at,
                });
            } else {
                self.requeue_claim(ctx, queue_id, report).await;
            }
        }
        claimed
    }

    /// This platform's queued rows, oldest first — the FIFO planner's input.
    async fn queued_rows(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
    ) -> Result<Vec<crate::domain::queue::QueuedRow>, DomainError> {
        let scope = self.queue_scope(ctx, actions::LIST, None).await?;
        let conn = self.db.conn()?;
        self.queue.queued_rows(&conn, &scope, platform_id).await
    }

    /// Take the lease for a row this tick claimed. `false` means do not dispatch.
    ///
    /// The lease CAS is the source of truth and the planner is advisory
    /// (`domain::queue`'s module docs), so a `Busy` answer here overrides the
    /// plan. It should be unreachable while this process holds the platform lock —
    /// the occupancy read that fed the plan was taken under it — which is exactly
    /// why it must be handled rather than asserted: the lock is process-local and
    /// the lease is not.
    ///
    /// **A failed acquisition releases**, for the reason
    /// `admission::AdmissionService::take_lease` states in full: an acquire that
    /// commits and then loses its response would otherwise leave the platform leased
    /// to a run this tick immediately requeues, and no later pass can free it — a
    /// `queued` row is not a claim, so reconciliation never sees it, and the TTL
    /// sweep releases no lease.
    async fn take_lease_for_claim(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        run_id: Uuid,
        exclusive: bool,
        report: &mut TickReport,
    ) -> bool {
        let mode = if exclusive {
            qa_environments_sdk::LeaseMode::Exclusive
        } else {
            qa_environments_sdk::LeaseMode::Parallel
        };
        match self
            .environments
            .acquire_lease(ctx, platform_id, run_id, mode)
            .await
        {
            Ok(qa_environments_sdk::AcquireOutcome::Acquired) => true,
            Ok(qa_environments_sdk::AcquireOutcome::Busy { current }) => {
                warn!(
                    %run_id,
                    %platform_id,
                    current = ?current,
                    "the platform lease refused a row this tick claimed; failing the row \
                     rather than starting a run against a platform it does not hold",
                );
                false
            }
            Err(error) => {
                report.note_failure(PASS_DRAIN, &environments_error(&error));
                // The acquisition may have committed server-side and only lost its
                // response, in which case the platform is now leased to a run this
                // tick is about to requeue — and nothing would ever free it: the row
                // goes back to `queued`, so `all_claims` never sees it and
                // reconciliation cannot release it, and the TTL sweep releases no
                // lease either. `decide_release` is idempotent and removes only a
                // hold this run actually has, so this is a no-op when the acquire
                // genuinely never landed. Same reasoning as
                // `admission::AdmissionService::take_lease`'s `Err` arm.
                self.release_lease(ctx, platform_id, run_id).await;
                false
            }
        }
    }

    /// Put a claimed row back in the queue after the lease refused it.
    ///
    /// # This replaced a path that could not work
    ///
    /// The first implementation failed the row and retired the run to
    /// [`RunState::Error`], arguing that leaving it `dispatching` would block the
    /// platform's queue for `orphan_timeout_seconds` over a row that will never
    /// start. The argument was sound and the conclusion was wrong twice over.
    ///
    /// * **It ignored the rule this task consumes.** `domain::queue`'s module docs
    ///   say a row that loses its `acquire` should *"stay queued for the next
    ///   tick"*. Requeueing is not a third option invented here; it is the
    ///   specified one, and the only reason it was not taken is that
    ///   `QueueRepository` had no method for it.
    /// * **It produced a lost run.** At this point the run is still `Queued`, or
    ///   `Created` on the launch-recovery path — the tick has not reached
    ///   `dispatch_one`, so nothing has moved it to `Dispatching`. Neither state
    ///   admits `Error` (`domain::state_machine::can_transition`), so the
    ///   transition failed, the row stayed `failed`, and the run sat in `Queued`
    ///   forever with no queued row for any later tick to claim.
    ///
    /// **And it is reachable.** Task 16's leader election protects the *ticker*
    /// only; admission runs inline in every replica's REST handler, so one
    /// replica's launch can legitimately hold the lease while another replica's
    /// tick is mid-drain.
    ///
    /// The row keeps its FIFO place, because `enqueued_at` is untouched. If the
    /// requeue itself fails the row is left `dispatching` and the orphan guard
    /// reclaims it after `orphan_timeout_seconds` — the old behaviour as a
    /// fallback rather than as the design.
    async fn requeue_claim(&self, ctx: &SecurityContext, queue_id: Uuid, report: &mut TickReport) {
        match self.queue_write(ctx, queue_id, QueueWrite::Requeued).await {
            Ok(true) => info!(
                %queue_id,
                "the platform lease refused this row; it keeps its place in the queue and \
                 the next tick retries it",
            ),
            Ok(false) => warn!(
                %queue_id,
                "could not return a row to the queue because it was no longer dispatching; \
                 leaving it for the orphan guard",
            ),
            Err(error) => report.note_failure(PASS_DRAIN, &error),
        }
    }
}

// ---------------------------------------------------------------------------
// Boot recovery
// ---------------------------------------------------------------------------

impl<R, Q> DispatchService<R, Q>
where
    R: RunsRepository,
    Q: QueueRepository,
{
    /// Fail every claim a restart left mid-dispatch. **Boot only.**
    ///
    /// # Preconditions the caller owes, which are not checked here
    ///
    /// `gear::QaRuns::serve` calls it, once, before the dispatcher ticker's
    /// first tick. That is the only production caller, and it is what the two
    /// conditions below are conditions *on*: neither is checked here, and the
    /// second is not checkable here. (This paragraph shipped as "nothing calls
    /// this yet; Task 16 will" and stayed after Task 16 wired it, so a reader
    /// went looking for an unreached function and found a live boot path.)
    ///
    /// It is correct **only** when
    ///
    /// 1. it runs once per cluster, not once per process, and
    /// 2. no other replica is holding claims at the time.
    ///
    /// Legacy's justification is *"with a single replica, that submit is
    /// definitively gone"*, and the port carries the sentence without carrying the
    /// premise: this pass enumerates **cross-tenant**, so under N replicas each
    /// starting process would fail every *other* replica's in-flight claims, retire
    /// those runs, and release their leases — which is the "momentarily release its
    /// claim, exactly when a second run could be admitted alongside an exclusive
    /// one" hazard the paragraph below warns about, applied to every tenant at once.
    /// Stated as a precondition rather than asserted as a fact, because this code
    /// cannot tell how many replicas are booting.
    ///
    /// A different rule from [`Self::reconcile_claims`], and the two must never be
    /// collapsed: `QueueRepository::fail_orphaned_dispatching` has no age
    /// predicate and does not need one, because at boot "with a single replica,
    /// that submit is definitively gone" — which is true at boot and nowhere else.
    /// Called from a tick it would fail every launch that is merely mid-build and
    /// momentarily release its claim, which is exactly when a second run could be
    /// admitted alongside an exclusive one
    /// (`manager/src/services/run_queue.rs:349-361`,
    /// `run_dispatcher.rs:417-426`).
    ///
    /// A row that *does* carry an execution reference is left to the tick
    /// reconciler, because only it knows whether that execution is still alive —
    /// legacy's `WHERE … workflow_name IS NULL` leaves exactly those rows
    /// untouched.
    ///
    /// # Two things legacy does not have to do
    ///
    /// * **Retire the run.** Legacy has no run row for a launch that never
    ///   started; here the run is stuck in `dispatching` and would stay there.
    /// * **Release the lease.** A row claimed before the restart holds its
    ///   platform's lease, and the lease is this gear's only occupancy oracle, so
    ///   a boot that failed the rows and left the leases would leave every
    ///   affected platform reading busy forever.
    ///
    /// # `boot_recovery_action`'s argument, and what it is not claiming
    ///
    /// This pass has no aliveness evidence — it makes no executor call, exactly as
    /// legacy's boot-only `UPDATE` makes none. The rule distinguishes only
    /// `Absent` from not-`Absent`, so a row that has a reference is passed as
    /// `ClaimExecution::Active` purely to reach the `Keep` arm; that is **not** an
    /// assertion that the execution is running. `Gone` would have been the
    /// misleading choice, because it asserts the execution is over.
    #[allow(
        clippy::cognitive_complexity,
        reason = "inflated by the `tracing` macros in each error arm: every step this \
                  function takes has its own operator-facing line, and the metric counts \
                  each expansion as a branch. Splitting further would separate a log line \
                  from the write it describes, which is the thing being documented. Same \
                  diagnosis as `chat-engine/src/infra/leader/k8s_lease.rs:387`"
    )]
    pub async fn recover_after_boot(&self) -> TickReport {
        let mut report = TickReport::default();
        let enumeration = system_actor::for_claim_reconciliation();

        // **Read to exhaustion, not one window.** The tick's scans are windowed
        // and rotate, which bounds their cost and still reaches every claim
        // within a few ticks. Boot cannot accept "within a few ticks": a row
        // left mid-submit by a restart holds a platform's lease and its run is
        // stuck in `dispatching`, and every tick that passes before this pass
        // reaches it is a platform reading busy for a submit that is definitively
        // gone. This runs once, so paying for full coverage is affordable here in
        // a way it is not on a five-second cadence.
        //
        // Memory stays bounded at one window: each is classified and written
        // before the next is read.
        let mut after: Option<Uuid> = None;
        loop {
            let Some(window) = self
                .all_claims(&enumeration, after, PASS_BOOT, &mut report)
                .await
            else {
                return report;
            };
            let truncated = window.truncated;
            after = window.rows.last().map(|claim| claim.id);
            self.recover_window(&window.rows, &mut report).await;
            if !truncated {
                return report;
            }
        }
    }

    /// One window of [`Self::recover_after_boot`]'s scan.
    ///
    /// Split out so the boot pass can paginate; the classification and the
    /// per-tenant grouping are unchanged.
    #[allow(
        clippy::cognitive_complexity,
        reason = "same diagnosis as the caller: one operator-facing line per step, and the \
                  metric counts each `tracing` expansion as a branch"
    )]
    async fn recover_window(&self, claims: &[ClaimAge], report: &mut TickReport) {
        let now = OffsetDateTime::now_utc();
        // Grouped by tenant because `fail_orphaned_dispatching` is a write: one
        // bulk statement per tenant under that tenant's scope, never one
        // cross-tenant statement under the enumeration identity.
        let mut orphans: HashMap<Uuid, Vec<(ClaimAge, Run)>> = HashMap::new();
        for claim in claims.iter().cloned() {
            let Some(tenant) = TenantBound::new(claim.tenant_id) else {
                warn!(
                    queue_id = %claim.id,
                    "a claim carries a nil tenant id; skipping it rather than writing under \
                     the platform-root identity",
                );
                continue;
            };
            let ctx = system_actor::for_claim_release(tenant);
            let run = match self.read_run(&ctx, claim.run_id).await {
                Ok(run) => run,
                Err(error) => {
                    report.note_failure(PASS_BOOT, &error);
                    continue;
                }
            };
            let execution = match run.execution_ref {
                None => ClaimExecution::Absent,
                Some(_) => ClaimExecution::Active,
            };
            let observation = ClaimObservation {
                execution,
                // A **real** age, not zero. See `boot_recovery_action`: with no
                // age term this pass would fail another replica's in-flight
                // dispatch, whose submit then succeeds into a run this one has
                // already marked `Error`.
                age_seconds: elapsed_seconds(now, claim.age_basis),
            };
            if boot_recovery_action(observation, self.orphan_timeout_seconds)
                == ClaimAction::FailOrphaned
            {
                orphans
                    .entry(claim.tenant_id)
                    .or_default()
                    .push((claim, run));
            }
        }

        for (tenant_id, rows) in orphans {
            // The only `TenantBound::new` call site in this crate that does not
            // warn on `None`, and the reason is that it cannot fire: this key came
            // out of the map above, whose every entry was inserted under a tenant
            // that had *already* been through `TenantBound::new` in the loop that
            // built it. Re-validating is how the value is carried, not a second
            // check. Warning here would report a nil tenant that no row has.
            let Some(tenant) = TenantBound::new(tenant_id) else {
                continue;
            };
            let ctx = system_actor::for_claim_release(tenant);
            let ids: Vec<Uuid> = rows.iter().map(|(claim, _)| claim.id).collect();
            let failed = match self.fail_orphans_for_tenant(&ctx, &ids).await {
                Ok(failed) => failed,
                Err(error) => {
                    report.note_failure(PASS_BOOT, &error);
                    continue;
                }
            };
            warn!(
                %tenant_id,
                rows = ids.len(),
                failed,
                "boot recovery failed run-queue rows left mid-dispatch by a restart",
            );
            for (claim, run) in rows {
                if let Err(error) = self
                    .transition(
                        &ctx,
                        &run,
                        RunState::Error,
                        RunStatePatch {
                            started_at: None,
                            finished_at: Some(OffsetDateTime::now_utc()),
                            error: Some(ORPHAN_REASON.to_owned()),
                        },
                    )
                    .await
                {
                    report.note_failure(PASS_BOOT, &error);
                }
                self.release_lease(&ctx, claim.platform_id, run.id).await;
                report.failed_orphans += 1;
            }
        }
    }

    /// One tenant's boot-only bulk failure, under that tenant's own scope.
    ///
    /// Declared **after** [`Self::recover_after_boot`] deliberately. Inserted
    /// above it, this helper silently adopted that function's entire doc comment —
    /// including the "boot only … must never be called from a tick" rule — so
    /// `cargo doc` rendered the module's most safety-critical sentence on a private
    /// helper while the public entry point had none. In a subsystem where
    /// documentation is specification, an item's position relative to a doc block
    /// is load-bearing.
    async fn fail_orphans_for_tenant(
        &self,
        ctx: &SecurityContext,
        ids: &[Uuid],
    ) -> Result<u64, DomainError> {
        let scope = self.queue_scope(ctx, actions::DISPATCH, None).await?;
        let conn = self.db.conn()?;
        self.queue
            .fail_orphaned_dispatching(&conn, &scope, ids, ORPHAN_REASON)
            .await
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
