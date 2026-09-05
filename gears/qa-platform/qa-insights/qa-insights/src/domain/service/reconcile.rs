//! The self-healing sweep, and the operator rebuild beside it.
//!
//! # Why this exists, and why legacy has no counterpart
//!
//! Legacy's analytics reads the same database its run path writes, in the same
//! process, so it has nothing to reconcile. This gear's projection lives in its
//! own database, split from qa-runs by the gear boundary, so it has to be built
//! by *some* explicit read of qa-runs — there is no shared process or
//! transaction to inherit correctness from the way legacy does. This module is
//! that read, on a timer, over an idempotent replace that makes it safe to run
//! against a run more than once.
//!
//! **A transactional broker consumer was the design's other intended ingest
//! path, and it never actually ran.** The event broker qa-platform is built for
//! has, per `PRD.md:1176`, no durable backend — an event delivered while this
//! gear was down would be simply lost, which is the row this module's own
//! idempotent-and-rebuildable design satisfies: *"insights ingestion designed
//! idempotent + rebuildable from qa-runs records until durability lands."* But
//! no deployment ever registered the client that consumer needed, so it was
//! deleted along with the event-broker dependency once that was established
//! (`crate::gear`'s header, "Event ingest, and why there is only one path").
//! This sweep was never a backstop for a live path — it has been the only path
//! that ran, from the day this gear first booted.
//!
//! # It replaces a poller, and it is far less aggressive than the one it
//! # replaces
//!
//! `manager/src/services/run_results_poller.rs` is the thing being replaced,
//! and the plan's instruction was that the replacement must not be *more*
//! aggressive. Read on 2026-08-20, its cadence and batch shape are:
//!
//! * **Every 30 seconds** (`RUN_RESULTS_SYNC_INTERVAL_SECONDS`, `main.rs:182`),
//!   floored at 5s (`run_results_poller.rs:39`), with an immediate cycle at
//!   startup (`RUN_RESULTS_SYNC_ON_STARTUP`, default true).
//! * Each cycle lists **every workflow Argo holds** — no window, no watermark,
//!   no page limit — keeps those in `Succeeded|Failed|Error|Running|Pending`,
//!   and asks the database for their persisted `(phase, test_count)` in one
//!   round trip.
//! * `needs_persist_sync` (`:254-267`) then re-persists a run when it is
//!   `Running`/`Pending` (always, so live counts advance), or when the
//!   persisted phase differs, or when the persisted `test_count` is **0**, or
//!   when there is no row at all.
//!
//! This sweep runs every 300s by default, reads a **bounded page** of runs from
//! a windowed watermark, and touches only the runs it finds missing. Strictly
//! less work, on every axis.
//!
//! ## Two behaviours of legacy's poller that this deliberately does not have
//!
//! 1. **In-progress runs are never reconciled.**
//!    `RunsReader::list_runs_finished_since` never returns a run with a `NULL`
//!    `finished_at`, so a run whose `test.result` events were all dropped shows
//!    no rows until it finishes — where legacy re-persisted live counts every
//!    30 seconds. The cost is a gap in the *in-progress* view after an outage,
//!    not lost data: the run lands in full at `run.finished`. Closing it would
//!    mean a second, unwindowed listing of active runs on every pass, which is
//!    exactly the aggression the instruction rules out. Recorded so Task 18
//!    knows the dashboard's active view must read qa-runs live rather than this
//!    projection — which is what that task already specifies.
//! 2. **A run with genuinely zero results is re-projected on every sweep.**
//!    `ResultsRepository::ingested_run_ids_between` reports the runs that have
//!    *rows*, so a run that legitimately produced none is never in that set and
//!    is always "missing". Legacy has the identical behaviour, from the
//!    `persisted.test_count == 0` arm of `needs_persist_sync`, so this is
//!    parity rather than a defect. It is bounded — one `get_run` plus one empty
//!    `list_run_test_results` plus one delete-nothing per sweep — and fixing it
//!    needs a way to record "ingested, zero rows", which is a schema change no
//!    task owns.
//!
//! ## And one behaviour legacy could not have: a tenant's backfill can wedge
//!
//! Recorded beside the two above because it is the same class of statement — a
//! consequence of a decision that is correct — and because nothing else in this
//! gear said it.
//!
//! [`Self::consume_page`] breaks at the first run it fails to backfill and
//! [`Self::advance_watermark`] never passes it. That is right, and the reason is
//! two paragraphs down. What follows from it is a **liveness** cost: a run that
//! fails *permanently* — a driver error on its own rows, a persistent qa-runs
//! 500 for that one id — is retried on every pass forever, and **no run that
//! finished after it is ever backfilled for that tenant**, because the floor can
//! never move past it. The projection is not wrong; it is frozen from that
//! instant onward, for that tenant, until someone intervenes.
//!
//! Legacy could not wedge this way, and not because it was more careful: it had
//! no watermark. Its poller re-listed every workflow Argo held on every cycle,
//! so one unpersistable run cost exactly that run.
//!
//! **What this gear offers is one WARN per pass and
//! [`ReconcileOutcome::stopped_at_run`]**, which names the offending run id so an
//! operator has something to act on — `POST /qa/v1/insights/rebuild` over a
//! narrow window is the tool, and if that fails too the run needs a look in
//! qa-runs.
//!
//! **Task 40 discharged the alerting obligation the plan stated here**, and the
//! consumer of the outcome is `crate::gear`'s `report_reconcile_outcome`, called
//! once per tenant per reconcile tick. It does two things beyond logging the
//! boolean: it raises `stopped_at_gap` to a `WARN` naming the tenant *and* the
//! run rather than folding it into the pass's `debug!` counters, and it keeps a
//! per-leadership-term count of **consecutive** wedged passes per tenant,
//! escalating to `ERROR` at `gear::WEDGED_PASSES_BEFORE_ERROR`. That second
//! half is the answer to the question the plan left open beside the obligation —
//! whether a repeatedly wedged tenant is a louder signal than a single one — and
//! that function's doc carries the reasoning and what was rejected. Without any
//! of it the failure mode is silent by construction, since a wedged tenant
//! produces no errors and no missing-data signal of any kind — only a projection
//! that stops advancing.
//!
//! *Rejected:* skipping the failed run and advancing anyway (strands it forever —
//! see step 5 below), and a retry counter that gives up after N passes (which
//! turns a frozen projection into a *silently incomplete* one, and needs a place
//! to record the abandonment that no table here has).
//!
//! # The algorithm, and the two places it must not cut a corner
//!
//! Per tenant:
//!
//! 1. Read the watermark. The floor is `watermark - lookback`, or the beginning
//!    of time when the mark is unset.
//! 2. `list_runs_finished_since(floor, page_size)`, oldest first.
//! 3. Diff the page's run ids against `ingested_run_ids_between(floor, ...)`.
//! 4. Backfill each missing run **through the same projection the rebuild
//!    endpoint uses** — [`Self::reproject`], which reads via
//!    [`IngestService::read_run_projection`] and writes via
//!    [`IngestService::write_run_projection`] (Task 5a split what used to be one
//!    function into these two; see the "Transactions" section below). Not two
//!    projections sharing an argument by convention: the sweep and the rebuild
//!    call the identical function, so a divergence between what an automatic
//!    pass writes and what an operator's replay writes cannot exist as a code
//!    path, only as a bug in the one path both take.
//! 5. Advance the watermark to the newest `finished_at` that was successfully
//!    accounted for, **and only that far**.
//!
//! **Step 5 stops at the first failure.** The page is oldest-first, so on a
//! failure the sweep advances to the last run *before* it and returns. Skipping
//! the failure and advancing to the end of the page would strand that run
//! forever — the next sweep's floor would already be past it. This is the whole
//! reason the ordering is part of the port's contract.
//!
//! **The upper bound of the diff is deliberately over-wide.** The plan says to
//! diff against `ingested_run_ids_between(floor, page_max_finished_at)`, and
//! that method's window is half-open — so the newest run in the page, whose
//! `finished_at` *is* `page_max`, would be excluded from the "already ingested"
//! set and re-projected on every single sweep. The bound used here is
//! `page_max + 1s`. An over-wide upper bound is harmless: the extra ids belong
//! to runs that are not in the page, and membership is only ever tested for
//! page members.
//!
//! # The rebuild is the same replay with the watermark taken out
//!
//! [`ReconcileService::rebuild`] is Task 16's operator endpoint, and it is in
//! this module rather than beside a service of its own because it is the sweep's
//! algorithm minus two steps: it does not read the watermark to find its floor
//! (the operator supplies both bounds) and it does not write one at the end.
//! Everything else — the listing, the per-run transaction, the projection it
//! goes through, the stop-at-the-first-failure rule — is shared code, which is
//! the property that matters: an operator reaching for a rebuild is already
//! having a bad day, and a second projection path would be the thing that
//! diverged.
//!
//! Four decisions it carries, each with what was rejected:
//!
//! 1. **It deletes nothing it does not immediately rewrite.** This is carried
//!    obligation 4 out of Task 15, stated on [`crate::infra::leader`]'s header:
//!    leader election in this gear is an *optimisation, not mutual exclusion*,
//!    and two replicas sweeping one tenant are correct only because every write
//!    is an idempotent upsert. A rebuild that truncated the range before
//!    rewriting it would make election a **correctness** requirement, which this
//!    gear does not provide — and it would do so on the one code path an
//!    operator reaches for when the projection is already suspect. So the
//!    rebuild replays run by run through [`ReconcileService::reproject`],
//!    whose write — [`IngestService::write_run_projection`]'s
//!    delete-then-insert — is scoped to the single run it is about to
//!    rewrite.
//!    *Rejected:* a range `DELETE` followed by a re-ingest, which would also
//!    have removed the rows of runs qa-runs has since deleted. Those rows are
//!    the price; they are reachable by a narrower tool, and none of the
//!    alternatives to leaving them is safe under concurrent replicas.
//!    **And it is legacy's shape as well**, which this decision was argued
//!    without noticing: `manager/src/services/argo.rs:2654`,
//!    `backfill_test_case_results`, rebuilds its projection with a per-run
//!    `DELETE … WHERE run_id = $1` and re-insert inside a loop over runs, never
//!    a range delete. See [`ReconcileService::rebuild`] for what else that
//!    citation does and does not support.
//! 2. **The window is `[from, to)` — inclusive lower, exclusive upper.** The
//!    convention is already recorded on
//!    [`ResultsRepository::ingested_run_ids_between`]: *"Half-open so that
//!    consecutive windows sharing an endpoint neither skip a run nor replay
//!    one."* An operator rebuilding a bad day in two halves splits at one
//!    instant and writes it once, which is the property that makes the tool
//!    composable. `RunsReader::list_runs_finished_since`'s lower bound is
//!    already inclusive, so only the upper bound was this task's to choose.
//!    *Rejected:* a closed `[from, to]`, which reads more naturally in an
//!    operator's head — and which would put two opposite conventions in one
//!    module, three hundred lines from [`DIFF_UPPER_SLACK`], the constant that
//!    exists because the sweep needs a *deliberately over-wide* bound to
//!    compensate for exactly this half-openness. Mixing the two is how the
//!    newest run in a page came to be re-projected forever once already.
//! 3. **A per-run failure stops the rebuild**, exactly as it stops the sweep,
//!    and reports [`ReconcileOutcome::stopped_at_gap`]. The sweep's reason —
//!    never advance a watermark past a gap — does not apply here, because there
//!    is no watermark to advance; the reason that does is that re-running the
//!    same window is free and total, so stopping loses nothing and gives the
//!    operator a single unambiguous signal.
//!    *Rejected:* pressing on and counting failures, which needs a further
//!    counter field on [`ReconcileOutcome`] and hands back a partial success an
//!    operator then has to reason about.
//! 4. **The listing runs under the caller's own `SecurityContext`**, not under a
//!    system actor. [`RunsReader`]'s header forecast this in the conditional —
//!    *"a future request-scoped caller would pass its own"*, a sentence this
//!    task made indicative — and it is what keeps the endpoint from being a
//!    privilege escalation: the set of runs a rebuild can
//!    touch is the set qa-runs is willing to show *that operator*. The
//!    subsequent per-run re-read — [`IngestService::read_run_projection`]
//!    since Task 5a split it out of `reproject_run`, still called from
//!    [`ReconcileService::reproject`] — still uses a system actor, which is a
//!    real asymmetry and a safe one — it can only re-read a run the caller's
//!    own listing already returned, and the write is scoped by the PDP
//!    decision compiled here.
//!    **It is `system_actor::for_operator_rebuild`, not another site's actor.**
//!    Through Task 19 this path minted `for_event_ingest` unconditionally, so an
//!    operator rebuild's cross-gear reads were indistinguishable in the audit
//!    log from the reconcile sweep's — the one thing
//!    [`crate::domain::system_actor`] exists to keep apart. Corrected in Phase
//!    A's fix wave by threading
//!    [`SystemActorSite`](crate::domain::system_actor::SystemActorSite) through
//!    what was then one shared function (Task 5a's split carried it forward
//!    onto `read_run_projection`); that module's header carries the whole
//!    record, including `for_event_ingest`'s later deletion.
//!
//! # Transactions: one per run, and the watermark on its own
//!
//! Each run's projection commits by itself. A page of 200 runs sharing one
//! transaction would hold locks across 200 cross-gear round trips, and a
//! failure at run 199 would discard 198 good backfills. The watermark advance
//! is its own transaction at the end; a crash between the last projection and
//! the advance costs one replayed window, which is idempotent by construction.
//!
//! **Since Task 5a, one run's transaction no longer spans its cross-gear
//! round trip at all.** [`ReconcileService::reproject`] reads via
//! [`IngestService::read_run_projection`] before opening a transaction, and
//! opens one only around [`IngestService::write_run_projection`] — see that
//! function's own doc for why: qa-runs' local client, resolved in-process in
//! a single-binary deployment, calls its own `db.conn()`, and
//! `toolkit_db`'s transaction-bypass guard is task-local, so a cross-gear
//! read nested inside this gear's own transaction tripped it deterministically.
//! The "one transaction per run, not one for the whole page" property this
//! section argues for is unaffected — each run's *write* still commits or
//! rolls back on its own — only *what the transaction spans* changed.
//!
//! # Leadership and tenancy: both wired by Task 40, and the tenancy answer is
//! # not one of the two this section forecast
//!
//! [`crate::infra::leader`] holds the elector and `crate::gear`'s
//! `reconcile_ticker` runs this sweep under `ROLE_RECONCILER`.
//! [`ReconcileService::reconcile_once`] still takes the tenant it sweeps,
//! because this gear has no tenant registry of its own — the REST path learns
//! one from its caller and a ticker has to enumerate first
//! (`domain::service::tenants`'s header).
//!
//! This section used to say: *"**Task 40 owns 'which tenants does the ticker
//! sweep?'**, and it is an open question, not an oversight: the honest options
//! are a `qa_ingest_watermarks` scan (only finds tenants already seen once) or a
//! platform tenant directory (a new cross-gear dependency)."* Task 40 answered
//! it, and **took neither option**:
//!
//! * The `qa_ingest_watermarks` scan is **circular**. The only writer of that
//!   table is [`Self::advance_watermark`], reached from `sweep`, reached from
//!   `reconcile_once(tenant)` — so on a fresh deployment the scan returns
//!   nothing, no tenant is ever swept, and nothing ever writes a first row. This
//!   section's parenthetical undersold it: not "only finds tenants already seen
//!   once" but "never finds any".
//! * The tenant directory is a new cross-gear dependency, which no task in the
//!   file map adds.
//!
//! What ships instead is `SELECT DISTINCT tenant_id FROM qa_test_results` —
//! written by the *event* path, so not circular — behind
//! [`TenantDirectory`](crate::domain::service::tenants::TenantDirectory), whose
//! header carries the design, the blind spot it leaves, and the authority split
//! that keeps its nil-tenant enumeration context out of every read that follows
//! it.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_runs_sdk::Run;
use time::{Duration, OffsetDateTime};
use toolkit_db::DBProvider;
use toolkit_security::{AccessScope, SecurityContext};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::RunsReader;
use crate::domain::repos::{ResultsRepository, WatermarkKind, WatermarkRepository};
use crate::domain::service::ingest::IngestService;
use crate::domain::service::{actions, refuse_scope_beyond_tenant, resources};
use crate::domain::system_actor::{self, SystemActorSite, TenantBound};

/// How far past the newest run in a page the "already ingested" probe reaches.
///
/// One second, because `ingested_run_ids_between`'s window is half-open and the
/// newest run in the page sits exactly on the upper bound. See this module's
/// header: over-wide is free, under-wide re-projects the newest run forever.
const DIFF_UPPER_SLACK: Duration = Duration::seconds(1);

/// What one sweep did. Returned rather than only logged, because Task 16's
/// rebuild endpoint answers with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconcileOutcome {
    /// Runs in the page the sweep examined.
    pub scanned: usize,
    /// Runs whose projection this pass wrote.
    ///
    /// **What that means differs between the two callers, deliberately.** For
    /// [`ReconcileService::reconcile_once`] it is the runs that had *no*
    /// projection, because a sweep skips whatever the diff says already landed.
    /// For [`ReconcileService::rebuild`] it is every run in the window, because
    /// a rebuild's whole premise is that the existing projection is the thing
    /// that is wrong. The field was named for the sweep, which arrived first;
    /// renaming it would have churned Task 15's tests for a word, and inventing
    /// a second outcome type for one differing field is what the plan's Task 16
    /// explicitly rules out.
    pub backfilled: usize,
    /// Where the watermark ended up, if it moved.
    pub watermark_advanced_to: Option<OffsetDateTime>,
    /// Whether the sweep stopped early on a failed backfill. `true` means the
    /// page was not fully consumed and the next sweep resumes at the gap.
    pub stopped_at_gap: bool,
    /// The run the pass stopped on, when it stopped on one.
    ///
    /// `Some` exactly when [`Self::stopped_at_gap`] is `true`, and it is a
    /// separate field rather than a `stopped_at_gap: Option<Uuid>` because the
    /// boolean is already on the wire (`api::rest::dto::RebuildOutcomeDto`) and
    /// this is not: it is for the log line and for the reconcile ticker's alert
    /// (`crate::gear`'s `report_reconcile_outcome`, Task 40).
    ///
    /// **Why it exists:** a permanently failing run wedges the whole tenant's
    /// backfill (see this module's header), and "which run" is the only thing an
    /// operator needs that the boolean does not carry. The per-run WARN has it,
    /// but a WARN is not a value a ticker can branch on.
    ///
    /// One case sets `stopped_at_gap` with a run id whose *failure* was the
    /// port's rather than the run's: a run returned with no `finished_at` from a
    /// finished-since listing. It is still the run to look at.
    pub stopped_at_run: Option<Uuid>,
}

/// The reconcile sweep.
///
/// Generic over the two repositories for the reason [`IngestService`] is:
/// their methods are generic over the `DBRunner` they run on, so the traits are
/// not object-safe.
pub struct ReconcileService<R, W> {
    db: Arc<DBProvider<DomainError>>,
    results: R,
    watermarks: W,
    runs: Arc<dyn RunsReader>,
    ingest: Arc<IngestService<R>>,
    /// Used by [`Self::rebuild`] and by nothing else in this type.
    ///
    /// The sweep does **not** touch it, and that asymmetry is the module's
    /// scoping decision made visible in one struct: the sweep is a ticker with
    /// no caller, so it scopes with `AccessScope::for_tenant` for the reasons
    /// [`crate::domain::service::ingest`] sets out; the rebuild has a real
    /// caller, so it asks the PDP. Holding the enforcer here rather than passing
    /// it per call matches every sibling gear — it is constructed once at init
    /// and serves every resource type.
    policy_enforcer: PolicyEnforcer,
    lookback: Duration,
    page_size: u32,
}

impl<R, W> ReconcileService<R, W>
where
    // `Clone + 'static` on both, and it is the transaction signature that
    // demands it rather than taste: `DBProvider::transaction` takes a
    // `for<'a> FnOnce(&'a DbTx<'a>) -> …+ 'a`, so a closure capturing
    // `&self.watermarks` would have to outlive every `'a` — which the compiler
    // resolves to `'static`. Both repositories are unit structs, so the clone
    // is free.
    R: ResultsRepository + Clone + 'static,
    W: WatermarkRepository + Clone + 'static,
{
    /// # Eight parameters, and a params struct was the rejected alternative
    ///
    /// `clippy::too_many_arguments` fires at eight, and the fix it implies — a
    /// `ReconcileDeps` struct — would have exactly one construction site
    /// ([`crate::domain::service::AppServices::new`]) which would unpack it
    /// again immediately. What such a struct buys elsewhere is protection
    /// against transposing two arguments of the same type, and there is none to
    /// buy here: all eight types are distinct (`Arc<DBProvider<_>>`, `R`, `W`,
    /// `Arc<dyn RunsReader>`, `Arc<IngestService<R>>`, `PolicyEnforcer`,
    /// `Duration`, `u32`), so any transposition is a compile error rather than a
    /// silent swap. `expect` rather than `allow`, so that if a later task
    /// shortens the list the attribute itself goes red.
    #[expect(
        clippy::too_many_arguments,
        reason = "all eight parameter types are distinct, so a params struct would guard against \
                  nothing the compiler does not already catch"
    )]
    #[must_use]
    pub const fn new(
        db: Arc<DBProvider<DomainError>>,
        results: R,
        watermarks: W,
        runs: Arc<dyn RunsReader>,
        ingest: Arc<IngestService<R>>,
        policy_enforcer: PolicyEnforcer,
        lookback: Duration,
        page_size: u32,
    ) -> Self {
        Self {
            db,
            results,
            watermarks,
            runs,
            ingest,
            policy_enforcer,
            lookback,
            page_size,
        }
    }

    /// Sweep one tenant once.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] when qa-runs cannot be listed at all — the
    /// sweep did nothing and the watermark did not move. A *per-run* backfill
    /// failure is **not** an error: it stops the sweep, leaves the watermark
    /// short of the gap, and is reported as
    /// [`ReconcileOutcome::stopped_at_gap`], because the sweep did useful work
    /// and the caller is a ticker that should log and try again rather than
    /// treat it as a fault.
    ///
    /// [`DomainError::Database`] from the watermark read or advance.
    pub async fn reconcile_once(
        &self,
        tenant: TenantBound,
    ) -> Result<ReconcileOutcome, DomainError> {
        let outcome = self.sweep(tenant).await?;
        info!(
            tenant_id = %tenant.get(),
            scanned = outcome.scanned,
            backfilled = outcome.backfilled,
            stopped_at_gap = outcome.stopped_at_gap,
            // `Debug`, not `Display`: the field is an `Option` and a `None`
            // rendered as an empty string is indistinguishable from a missing
            // field in a structured sink.
            stopped_at_run = ?outcome.stopped_at_run,
            "reconcile sweep finished",
        );
        Ok(outcome)
    }

    /// Replay a closed window from qa-runs, on an operator's behalf.
    ///
    /// `POST /qa/v1/insights/rebuild`. **It neither reads nor writes the
    /// watermark**, which is the whole point of the endpoint rather than an
    /// omission: it is a tool for a *known-bad window*, not a reset. Rewinding
    /// the mark would make the next sweep re-walk everything since, and
    /// advancing it would strand whatever sits between the window and where the
    /// sweep had got to.
    ///
    /// See this module's header for the four decisions this carries — no
    /// truncation, the half-open `[from, to)` window, stop-at-the-first-failure,
    /// and the caller's own context on the listing.
    ///
    /// # No legacy *endpoint*, but legacy does have a projection rebuild — and
    /// # it is the precedent for the shape chosen here
    ///
    /// **Corrected in Task 16's fix round.** This section first said flatly that
    /// legacy "has nothing to rebuild *from*". That is false, and the citation
    /// that falsifies it is one that would have *supported* the design:
    /// `manager/src/services/argo.rs:2654`, `backfill_test_case_results`,
    /// read on 2026-08-20. It rebuilds `test_case_results` run by run —
    /// `DELETE FROM test_case_results WHERE run_id = $1` and then re-insert,
    /// inside a loop over runs — re-deriving each run's cases by re-parsing
    /// `run_results.raw_logs`. So legacy rebuilds a projection, and it does it
    /// with **exactly** the per-run delete-then-rewrite this module's decision 1
    /// argues for over a range `DELETE`. That decision was defended from first
    /// principles when it had a parity citation available.
    ///
    /// What is genuinely absent, and what this endpoint therefore does not port:
    ///
    /// * **It is not an operator endpoint.** It is a one-time startup migration,
    ///   called once from `manager/src/main.rs:140`.
    /// * **It has no time window.** Its selection predicate is
    ///   `WHERE raw_logs LIKE '%TEST_CASE:%' AND id NOT IN (SELECT DISTINCT
    ///   run_id FROM test_case_results)` — "runs that have no case rows yet".
    /// * **Its source is its own database**, `run_results.raw_logs`, not a
    ///   sibling service. That is the part the gear split removed, and it is why
    ///   the *shape* ports and the *plumbing* cannot.
    ///
    /// One deliberate divergence follows from the first two: legacy **skips**
    /// runs that already have rows, while this replays every run in the window.
    /// Both are right for what they are. Legacy's is a migration filling in what
    /// was never written; this is a repair for a window whose contents are known
    /// to be *wrong*, where "already has rows" is not evidence of anything —
    /// which is what `a_rebuild_re_reads_a_run_the_sweep_would_have_skipped`
    /// pins.
    ///
    /// The *reason* this endpoint exists is still gear-split-specific and has no
    /// counterpart: legacy's poller re-read Argo every 30 seconds
    /// (`manager/src/main.rs:182`) and its analytics read the same database the
    /// run path wrote (`manager/src/routes/analytics.rs`), so it had no separate
    /// projection that could go stale. This exists because this gear's
    /// projection lives in its own database, built by an explicit read of
    /// qa-runs that can fail or lag — the same reason [`Self::reconcile_once`]
    /// does.
    ///
    /// # The window is one page, and a truncated one is logged rather than
    /// # silently halved
    ///
    /// `page_size` bounds the listing, as it does for the sweep — an operator
    /// typing a year-wide window must not be able to ask this gear to
    /// materialise every run in it. When the listing comes back full, the window
    /// may hold more, and the answer is to narrow it and repeat; that is a WARN
    /// rather than an error because the runs that *were* replayed were replayed
    /// correctly.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the caller carries no tenant — the nil
    /// UUID is the platform-root sentinel and there is no cross-tenant rebuild
    /// (see [`TenantBound`]). Chosen over [`DomainError::Validation`], which
    /// would answer 400 and tell the caller their request body was the problem
    /// when the problem is who they are.
    ///
    /// [`DomainError::Validation`] when `to` is not strictly after `from`. An
    /// equal pair is the empty window under a half-open upper bound, and an
    /// operator who typed one instant twice made a mistake worth answering
    /// rather than a no-op worth performing.
    ///
    /// [`DomainError::Forbidden`] again from the PDP, through
    /// `From<EnforcerError>`.
    ///
    /// [`DomainError::UnsupportedScope`] when the PDP **grants** the action but
    /// compiles to a scope constraining more than `owner_tenant_id`. Not a
    /// denial and deliberately not reported as one: the projection write's delete
    /// is scope-filtered and its insert is not, so a replay under such a scope
    /// would duplicate every row rather than replace it. Refused before qa-runs
    /// is listed. The whole measurement is on
    /// [`refuse_scope_beyond_tenant`](crate::domain::service::refuse_scope_beyond_tenant).
    ///
    /// [`DomainError::Internal`] when qa-runs cannot be listed at all. A
    /// *per-run* failure is not an error — see [`Self::reconcile_once`], which
    /// says the same thing for the same reason.
    pub async fn rebuild(
        &self,
        ctx: &SecurityContext,
        from: OffsetDateTime,
        to: OffsetDateTime,
    ) -> Result<ReconcileOutcome, DomainError> {
        let tenant = TenantBound::new(ctx.subject_tenant_id()).ok_or_else(|| {
            warn!("a rebuild was requested by a caller with no tenant; refusing");
            DomainError::Forbidden
        })?;

        if to <= from {
            return Err(DomainError::Validation {
                field: "to".to_owned(),
                message: "must be strictly after 'from'; the window is half-open [from, to)"
                    .to_owned(),
            });
        }

        let scope = self
            .policy_enforcer
            .access_scope(ctx, &resources::TEST_RESULT, actions::REBUILD, None)
            .await?;

        // Before anything is read or written. A scope that constrains more than
        // the tenant cannot be executed by the projection write — its delete is
        // scope-filtered and its insert is not — so a replay under one would
        // duplicate every row instead of replacing it. See
        // `refuse_scope_beyond_tenant`.
        refuse_scope_beyond_tenant(&scope, resources::TEST_RESULT_NAME)?;

        let window = self.window(ctx, tenant, from, to).await?;
        let outcome = self.replay(tenant, &scope, window).await;

        info!(
            tenant_id = %tenant.get(),
            %from,
            %to,
            scanned = outcome.scanned,
            replayed = outcome.backfilled,
            stopped_at_gap = outcome.stopped_at_gap,
            stopped_at_run = ?outcome.stopped_at_run,
            "operator rebuild finished",
        );

        // `watermark_advanced_to` is `None`, and it is meaningful: this pass did
        // not move the mark, by construction.
        Ok(outcome)
    }

    /// The runs a rebuild will replay: one page from qa-runs, cut to `[from, to)`.
    ///
    /// Split out of [`Self::rebuild`] for the same reason [`Self::sweep`] is
    /// split out of [`Self::reconcile_once`] — so the shape of the operation is
    /// the body of the function rather than something to reconstruct from it.
    async fn window(
        &self,
        ctx: &SecurityContext,
        tenant: TenantBound,
        from: OffsetDateTime,
        to: OffsetDateTime,
    ) -> Result<Vec<Run>, DomainError> {
        // The caller's own context, not a system actor: see decision 4 in this
        // module's header.
        let page = self
            .runs
            .list_runs_finished_since(ctx, from, self.page_size)
            .await?;

        // Measured on the *unfiltered* page: it is the listing that was capped,
        // and the filter below can leave a truncated page looking short.
        if page.len() >= self.page_size as usize {
            warn!(
                tenant_id = %tenant.get(),
                %from,
                %to,
                page_size = self.page_size,
                "the rebuild window filled a whole page; runs past the page boundary were not \
                 replayed. Narrow the window and repeat.",
            );
        }

        // The port guarantees the lower bound (`finished_at >= from`) and that a
        // run with no finish instant is never returned. The upper bound is this
        // function's, and it is exclusive.
        Ok(page
            .into_iter()
            .filter(|run| run.finished_at.is_some_and(|at| at < to))
            .collect())
    }

    /// Re-project every run in `window`, stopping at the first that fails.
    ///
    /// No watermark and no diff, which is the whole difference from
    /// [`Self::consume_page`]: every run in the window is replayed, because the
    /// premise of a rebuild is that the existing projection is what is wrong.
    async fn replay(
        &self,
        tenant: TenantBound,
        scope: &AccessScope,
        window: Vec<Run>,
    ) -> ReconcileOutcome {
        let mut outcome = ReconcileOutcome {
            scanned: window.len(),
            ..ReconcileOutcome::default()
        };

        for run in window {
            if let Err(err) = self
                .reproject(SystemActorSite::OperatorRebuild, tenant, scope, run.id)
                .await
            {
                warn!(
                    run_id = %run.id,
                    tenant_id = %tenant.get(),
                    error = %err,
                    "rebuild failed on a run; stopping here rather than reporting a partial \
                     success. Re-running the same window is idempotent.",
                );
                outcome.stopped_at_gap = true;
                outcome.stopped_at_run = Some(run.id);
                break;
            }
            outcome.backfilled += 1;
        }

        outcome
    }

    /// The sweep itself, with the summary log lifted out to its caller.
    ///
    /// Split for no reason other than readability: every step below is one
    /// line, so the shape of the algorithm is the body of the function rather
    /// than something to be reconstructed from it.
    async fn sweep(&self, tenant: TenantBound) -> Result<ReconcileOutcome, DomainError> {
        // `for_reconcile_sweep`, not `for_event_ingest`: the `site` on the audit
        // line is the whole reason `system_actor` has one factory per flow, and
        // this call site emitted the consumer's name from Task 15 until Phase
        // A's fix wave.
        let ctx = system_actor::for_reconcile_sweep(tenant);
        let scope = AccessScope::for_tenant(tenant.get());

        let floor = self.floor(&scope).await?;
        let page = self
            .runs
            .list_runs_finished_since(&ctx, floor, self.page_size)
            .await?;

        if page.is_empty() {
            debug!(tenant_id = %tenant.get(), %floor, "reconcile sweep found no runs in the window");
            return Ok(ReconcileOutcome::default());
        }

        let already = self.already_ingested(&scope, floor, &page).await?;
        let (mut outcome, advance_to) = self.consume_page(tenant, &scope, page, &already).await;

        if let Some(at) = advance_to {
            self.advance_watermark(tenant, at).await?;
            outcome.watermark_advanced_to = Some(at);
        }

        Ok(outcome)
    }

    /// Where this sweep starts reading qa-runs from.
    ///
    /// An unset mark means this gear has never swept, and the floor is the
    /// beginning of time — `page_size` is what keeps that first pass finite.
    /// Legacy reads every workflow on every cycle, so starting from the
    /// beginning *once* is not the aggressive choice; it is the one that makes
    /// the projection complete.
    async fn floor(&self, scope: &AccessScope) -> Result<OffsetDateTime, DomainError> {
        let marks = {
            let conn = self.db.conn()?;
            self.watermarks.get(&conn, scope).await?
        };
        Ok(marks
            .last_reconciled_finished_at
            .map_or(OffsetDateTime::UNIX_EPOCH, |mark| mark - self.lookback))
    }

    /// The run ids in this window that already have a projection.
    ///
    /// The upper bound carries [`DIFF_UPPER_SLACK`] on purpose; see this
    /// module's header for why an exact `page_max` would re-project the newest
    /// run on every sweep.
    async fn already_ingested(
        &self,
        scope: &AccessScope,
        floor: OffsetDateTime,
        page: &[Run],
    ) -> Result<Vec<Uuid>, DomainError> {
        // The page is oldest-first, so its last element carries the newest
        // instant. A page member with no instant cannot widen the window, and
        // `consume_page` refuses to advance past one anyway.
        let page_max = page
            .iter()
            .filter_map(|run| run.finished_at)
            .next_back()
            .unwrap_or(floor);

        let conn = self.db.conn()?;
        self.results
            .ingested_run_ids_between(&conn, scope, floor, page_max + DIFF_UPPER_SLACK)
            .await
    }

    /// Walk the page in order, backfilling what is missing, and report how far
    /// the watermark may move.
    ///
    /// Returns the outcome and the newest instant that was fully accounted for.
    /// **It stops at the first failure**, because the page is oldest-first and
    /// everything after a gap is newer: advancing past one would put the next
    /// sweep's floor beyond a run that was never ingested.
    async fn consume_page(
        &self,
        tenant: TenantBound,
        scope: &AccessScope,
        page: Vec<Run>,
        already: &[Uuid],
    ) -> (ReconcileOutcome, Option<OffsetDateTime>) {
        let mut outcome = ReconcileOutcome {
            scanned: page.len(),
            ..ReconcileOutcome::default()
        };
        let mut advance_to = None;

        for run in page {
            let Some(finished_at) = run.finished_at else {
                // The port promises never to return one. If it does, the run is
                // un-accountable rather than ingestible: advancing the watermark
                // past a run whose instant is unknown is the one thing that
                // cannot be undone.
                warn!(
                    run_id = %run.id,
                    "qa-runs returned a run with no finish instant from a finished-since \
                     listing; stopping the sweep here rather than advancing past it",
                );
                outcome.stopped_at_gap = true;
                outcome.stopped_at_run = Some(run.id);
                break;
            };

            if !already.contains(&run.id) {
                if let Err(err) = self
                    .reproject(SystemActorSite::ReconcileSweep, tenant, scope, run.id)
                    .await
                {
                    warn!(
                        run_id = %run.id,
                        tenant_id = %tenant.get(),
                        error = %err,
                        "reconcile backfill failed; stopping the sweep short of the gap",
                    );
                    outcome.stopped_at_gap = true;
                    outcome.stopped_at_run = Some(run.id);
                    break;
                }
                outcome.backfilled += 1;
            }

            advance_to = Some(finished_at);
        }

        (outcome, advance_to)
    }

    /// Move the reconcile mark, in its own transaction.
    async fn advance_watermark(
        &self,
        tenant: TenantBound,
        at: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let tenant_id = tenant.get();
        let watermarks = self.watermarks.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    watermarks
                        .advance(
                            tx,
                            &AccessScope::for_tenant(tenant_id),
                            tenant_id,
                            WatermarkKind::ReconciledFinishedAt,
                            at,
                        )
                        .await
                })
            })
            .await
    }

    /// Re-project one run, reading before any transaction opens and writing
    /// inside one.
    ///
    /// Shared by the sweep and by [`Self::rebuild`], which is the point: two
    /// separate projections would diverge only after an outage — or only after
    /// an operator reached for the rebuild — which is the worst possible time to
    /// find out. The `scope` is the caller's, and it is the only thing that
    /// differs between the two: `AccessScope::for_tenant` for the sweep, a PDP
    /// decision for the rebuild.
    ///
    /// `site` is the second such thing, and it is a parameter for the same
    /// reason: the audit identity of the cross-gear re-read differs between the
    /// two callers, so each private helper — [`Self::consume_page`] for the
    /// sweep, [`Self::replay`] for the rebuild — names its own rather than
    /// sharing one. Neither helper serves both flows, so neither needs to thread
    /// it further.
    ///
    /// # Fixed in Task 5a: the read used to happen inside this function's own
    /// # transaction, and that was the bug
    ///
    /// Through Task 40 this function opened its transaction first and called
    /// `IngestService::reproject_run` — reads and all — inside it. In a
    /// single-binary deployment the qa-runs client `ClientHub` resolves is
    /// qa-runs' own in-process service, and its `get`/`test_results`
    /// (`qa-runs/src/domain/service/runs.rs:321`, `:611-625`) call **qa-runs'
    /// own** `db.conn()`. `toolkit_db`'s transaction-bypass guard is
    /// task-local rather than instance-local
    /// (`libs/toolkit-db/src/secure/db.rs:10-21`), so that nested call failed
    /// with `DbError::ConnRequestedInsideTx` — indistinguishable, to the
    /// guard, from the bypass it exists to catch — on every run, in every
    /// sweep and every rebuild, deterministically. `POST
    /// /qa/v1/insights/rebuild` answered
    /// `{"scanned":2,"replayed":0,"stopped_at_gap":true}` on a stack that had
    /// completed real test runs, and `sweep()` shares this function, so it
    /// had been failing identically on every tick since boot.
    ///
    /// The fix reads via [`IngestService::read_run_projection`] *before*
    /// opening a transaction at all, then opens one around only
    /// [`IngestService::write_run_projection`]. The cross-gear reads never had
    /// this gear's transaction as a consistency boundary in the first place —
    /// they touch qa-runs' database, not this gear's — so moving them outside
    /// it costs nothing and trips nothing.
    ///
    /// ## The race the split introduces, and why it does not matter here
    ///
    /// The run this reads and the run this writes can now differ: qa-runs
    /// could answer a later query differently than it answered the read a
    /// moment ago, and that "moment ago" is now the time to open a
    /// transaction plus one round trip, not zero. Three things hold that
    /// argument would otherwise need to defeat, and did not exist to defeat
    /// before this split because the window it worries about was already
    /// there:
    ///
    /// 1. **The window is not new.** `get_run` and `list_run_test_results`
    ///    were always two separate cross-gear calls, made in sequence, with no
    ///    isolation between them and none against this gear's own database —
    ///    they read a *different* gear's database, so this gear's transaction
    ///    was never a consistency boundary for them, opened before this split
    ///    or after it. The split moves *when* the transaction starts; it does
    ///    not add a read that used to be atomic with something and now is not.
    /// 2. **The write is an idempotent delete-then-insert of one run's whole
    ///    state**, not a merge. A write that lands a slightly stale snapshot
    ///    is not corrupt — it is a true state the run held a moment earlier —
    ///    and this module's header relies on exactly that property elsewhere
    ///    (idempotent re-sweep, idempotent re-rebuild, two replicas racing on
    ///    the same run). A reprojection that landed slightly behind is
    ///    corrected the next time anything re-projects that run — the next
    ///    sweep that still finds it missing, or another rebuild — not by this
    ///    function refusing to run.
    /// 3. **Two replicas racing this same function already had this
    ///    property**, and the module's header says so: leader election here
    ///    is "an optimisation, not mutual exclusion," and correctness rests on
    ///    every write being an idempotent upsert, not on any ordering between
    ///    a read and a write. Two transactions from two replicas already could
    ///    not serialize their cross-gear reads against each other before this
    ///    split; whichever commits its write last determines the final state,
    ///    exactly as it does now.
    ///
    /// What *would* matter, and does not apply here: a run whose `finished_at`
    /// is set before every one of its result rows has landed would look
    /// "ingested" (has some rows) to [`Self::already_ingested`]'s diff and
    /// never be swept again even with the read and the write back in one
    /// transaction — that hazard is about qa-runs' own write ordering, not
    /// about where this function's transaction opens, and this split neither
    /// creates it nor closes it.
    async fn reproject(
        &self,
        site: SystemActorSite,
        tenant: TenantBound,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<(), DomainError> {
        // Before any transaction opens: see this function's own doc for why
        // a cross-gear read nested inside one trips `toolkit_db`'s
        // transaction-bypass guard in a single-binary deployment.
        let Some(projection) = self
            .ingest
            .read_run_projection(site, tenant, run_id)
            .await?
        else {
            return Ok(());
        };

        let ingest = Arc::clone(&self.ingest);
        // Cloned into the closure: `DBProvider::transaction` takes a
        // `for<'a> FnOnce(&'a DbTx<'a>) -> … + 'a`, which the compiler resolves
        // to `'static`, so a borrowed `&AccessScope` cannot cross it. Only the
        // write needs this now; the read above already ran to completion.
        let scope = scope.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    ingest
                        .write_run_projection(tx, &scope, tenant, run_id, projection)
                        .await
                })
            })
            .await
    }
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod reconcile_tests;
