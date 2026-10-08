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
//! has, per PRD §11, no durable backend — an event delivered while this
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
//!    **This is not only a cost, it is a hazard, and the notification
//!    producer had to be built around it.** Such a run reaches
//!    [`ReconcileService::reproject`] on every pass, so it reaches the
//!    run-completed notification on every pass, and it can never be
//!    suppressed by a claim seeded from `qa_test_results` because it is not
//!    in that table. 1095 of the dev stand's 2326 finished runs were that
//!    shape on 2026-09-29. `m20260929_000004_run_completed_notification_cutoff`
//!    is what makes them silent; see `reproject`'s own doc.
//!
//!    **The pass is silent but it was not free, and that was a second
//!    hazard** (final review, finding 6): every pass repeated two cross-gear
//!    reads that could produce nothing and appended another audit row saying
//!    what the previous pass already said — unbounded, on a tenant whose
//!    sweep has wedged and whose floor therefore never moves.
//!    `NotifyService::already_decided` now ends such a pass on one indexed
//!    claim read. What is still repeated per tick is the *projection* — the
//!    `get_run`, the empty result read and the delete-nothing this item
//!    already priced — and the cutoff read for a run that is history, which
//!    deliberately claims nothing.
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
//! 1. Read the watermark row. The floor is `watermark - lookback`, or the
//!    beginning of time when the mark is unset. The row also carries a
//!    [`SweepCursor`] — the `(finished_at, id)` key the *previous* pass stopped
//!    on, if it stopped short of catching up — and the walk starts there
//!    instead, but only when that cursor vouches for this pass' window — its
//!    recorded floor at or before this pass' floor, its position at or after
//!    it (`first_page_of`'s honour rule). See step 6.
//! 2. `list_runs_finished_since(cursor, page_size)`, oldest first.
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
//! 6. Persist the key of that same run as the [`SweepCursor`], or **erase** the
//!    cursor if the pass caught up.
//!
//! **Step 6 is finding #122's fix, and it exists because step 1's floor sits
//! behind the mark.** A lookback window holding more runs than
//! `MAX_PAGES_PER_SWEEP * page_size` lets a pass spend its whole budget without
//! reaching the mark, where the step-5 advance is a monotonic no-op — so before
//! the cursor, the next tick derived the same floor and repeated the same work
//! forever. [`MAX_PAGES_PER_SWEEP`] carries the whole account, including what
//! keeps the cursor from becoming a second watermark and what it costs the
//! stall alarm.
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
//! Everything else — the keyset page walk, the listing, the per-run
//! transaction, the projection it goes through, the stop-at-the-first-failure
//! rule — is shared code or the same shape, which is the property that matters:
//! an operator reaching for a rebuild is already having a bad day, and a second
//! projection path would be the thing that diverged.
//!
//! The page walk is the most recent thing to become shared, and it was the last
//! divergence in this module: until 2026-09-21 the rebuild read **one** page and
//! truncated, which the sweep's own fix had just established as the shape that
//! strands data permanently. See [`ReconcileService::rebuild`] for what that
//! cost and for the budget that replaced it.
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
use qa_runs_sdk::{FinishedRunCursor, Run};
use time::{Duration, OffsetDateTime};
use toolkit_db::DBProvider;
use toolkit_security::{AccessScope, SecurityContext};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::{MAX_FINISHED_RUNS_PAGE, RunsReader};
use crate::domain::repos::{
    ResultsRepository, SweepCursor, WatermarkKind, WatermarkRepository, Watermarks,
};
use crate::domain::service::ingest::IngestService;
use crate::domain::service::notify::RunCompletionNotifier;
use crate::domain::service::{actions, refuse_scope_beyond_tenant, resources};
use crate::domain::system_actor::{self, SystemActorSite, TenantBound};

/// How far past the newest run in a page the "already ingested" probe reaches.
///
/// One second, because `ingested_run_ids_between`'s window is half-open and the
/// newest run in the page sits exactly on the upper bound. See this module's
/// header: over-wide is free, under-wide re-projects the newest run forever.
const DIFF_UPPER_SLACK: Duration = Duration::seconds(1);

/// How many pages one sweep will walk before leaving the rest for the next tick.
///
/// The sweep pages forward until it catches up (see [`ReconcileService::sweep`]
/// for why it must), and "until it catches up" over a gear that has been wedged
/// for days is unbounded work on a 5-minute ticker. This bounds it: at the
/// default `reconcile_page_size` of 200 a single pass still walks 10,000 runs,
/// far more than any real backlog between two ticks.
///
/// # The budget can be spent **before** the mark, which is what made this a
/// # wedge — and what the resume cursor now ends
///
/// The walk starts at `floor = mark - lookback`, deliberately *behind* the
/// mark, so a lookback window holding more than
/// `MAX_PAGES_PER_SWEEP * page_size` runs lets a pass spend every page without
/// ever reaching past the mark. Every one of those pages is real work — runs
/// read, a diff run against them — and [`WatermarkRepository::advance`] is
/// monotonic, so handing it an instant at or behind the mark succeeds and
/// changes nothing. Second review, finding #122.
///
/// **Until this change the next tick derived the same floor and did it again,
/// forever.** The window is historical data that never drains on its own, so
/// nothing in the sweep could ever clear it; the remedy was an operator's — a
/// `POST /qa/v1/insights/rebuild` over the window, or a narrower
/// `reconcile_lookback_seconds`.
///
/// It is now the sweep's. A pass that ends short of catching up persists the
/// `(finished_at, id)` key of the last run it consumed as a
/// [`SweepCursor`], and the next tick resumes from it instead of re-deriving
/// the floor — so a window of any size drains in `ceil(window / budget)` ticks.
/// Three properties keep that from quietly becoming a second watermark, and
/// each is a test:
///
/// * **It is honoured only where it vouches for the window.** The cursor
///   records the floor its pass walked from, and [`ReconcileService::sweep`]
///   honours it only when that floor is at or before the floor it derives and
///   the cursor's position is at or after it — so `[floor, at]` is a range the
///   stored pass consumed. A floor that moved forward during a drain keeps the
///   cursor; a wider-lookback reader's earlier floor discards it. The cost —
///   the effective lookback shrinks while a drain moves the mark — is on
///   `first_page_of`.
/// * **A caught-up pass erases it.** The tick after a drain finishes is back to
///   `mark - lookback` and re-walks the whole lookback, which is how a run
///   *written* with a `finished_at` behind the cursor is still picked up — the
///   reason the floor sits behind the mark in the first place.
/// * **The mark's monotonicity is untouched.** The cursor is a second, separate
///   column with the opposite rule (it may move backwards and be erased), not a
///   relaxation of [`WatermarkRepository::advance`].
///
/// # The alarm reads the cursor, and a healthy drain does not escalate
///
/// [`warn_if_budget_spent`] still fires on such a pass and
/// [`ReconcileOutcome::caught_up`] still stays `false`, because the walk really
/// did stop with more to read. A pass draining a window that sits behind the
/// mark moves neither the mark nor `caught_up`, so `crate::gear::stall_of`
/// could not tell it from a wedge and escalated a healthy tenant to `ERROR`
/// once the drain needed more than `crate::gear::WEDGED_PASSES_BEFORE_ERROR`
/// ticks. Finding #122's residual B closed that: a pass whose
/// [`ReconcileOutcome::sweep_cursor_at`] rises past the highest cursor that
/// ticker has seen under the same mark is progress, and the `ERROR` now means
/// the cursor high-water has stopped rising.
///
/// **That idea was measured and rejected twice before it shipped**, and both
/// objections are answered rather than dismissed — `crate::gear::stall_of`'s
/// doc carries the account. In one line each: the shared row is read through
/// one ticker's own consecutive outcomes and high-watered, so another
/// replica's oscillating writes count as progress at most once; and a replica
/// wedge re-walks the same ground every pass and so writes the same cursor
/// every pass, which reads as standing still.
/// [`reconcile_tests::a_replica_wedge_on_mixed_lookbacks_still_escalates`](reconcile_tests)
/// builds that wedge from real sweeps — two replicas on different lookbacks
/// whose older band is wider than a budget, so neither can honour the other's
/// cursor — and drives it through the real `stall_of` per replica.
///
/// A late-written run older than the lookback that a mark-advancing drain has
/// shrunk (see `first_page_of`, "The known cost") is not something any pass
/// reports; `POST /qa/v1/insights/rebuild` over the window is the remedy.
const MAX_PAGES_PER_SWEEP: u32 = 50;

/// How many pages one [`ReconcileService::rebuild`] will walk before answering
/// that it did not finish.
///
/// The rebuild pages on the same keyset cursor the sweep does, so its bound is
/// a *budget* rather than a wall: whatever it does not reach is named in
/// [`ReconcileOutcome::resume_from`] and the next request starts there. It is
/// deliberately smaller than [`MAX_PAGES_PER_SWEEP`] and for a reason the sweep
/// does not have — a rebuild is a synchronous operator request, and a walk long
/// enough to hit a gateway timeout hands back no answer at all, which is the
/// silent partial this endpoint exists to stop being. Ten pages is 2,000 runs
/// at the default `reconcile_page_size`, ten times the reach of the single page
/// this used to truncate at.
const MAX_PAGES_PER_REBUILD: u32 = 10;

/// What one sweep did. Returned rather than only logged, because Task 16's
/// rebuild endpoint answers with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconcileOutcome {
    /// Runs in the page the sweep examined.
    pub scanned: usize,
    /// Runs this pass re-projected — **attempts, not writes**.
    ///
    /// # It counts attempts, and the three-day outage is why that is spelled out
    ///
    /// A run is counted here once [`ReconcileService::reproject`] returns `Ok`,
    /// and `Ok` includes the two cases that write nothing: a run qa-runs no
    /// longer has (`read_run_projection` answers `None`) and a run with no
    /// per-test rows at all (the projection is empty and the delete-then-insert
    /// inserts nothing). Neither leaves a row in `qa_test_results`, and
    /// [`ResultsRepository::ingested_run_ids_between`] answers *from that
    /// table* — so such a run is never "already ingested", is re-projected on
    /// every single pass, and is counted here every time.
    ///
    /// That is not hypothetical: through the 2026-09-18 outage this field read
    /// `116` on every pass for three days while `qa_test_results` did not grow
    /// by a row. A reader who takes it for "work that landed" reads a treadmill
    /// as health. [`Self::result_rows_written`] is the number that answers the
    /// question this one looks like it answers, and it is why that field was
    /// added rather than this one's meaning quietly changed: `replayed` on
    /// `api::rest::dto::RebuildOutcomeDto` is this field on the wire, with a
    /// documented `replayed == scanned unless it stopped early` invariant that
    /// only holds for attempts.
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
    /// Rows this pass actually wrote into `qa_test_results`.
    ///
    /// The honest counterpart to [`Self::backfilled`], which counts attempts —
    /// read that field's doc for the outage this pair exists to make legible.
    /// A pass with `backfilled > 0` and `result_rows_written == 0` is
    /// re-projecting runs that have nothing to project, forever.
    ///
    /// Counted from the projection the write was handed, so it is rows
    /// *offered* to an idempotent delete-then-insert rather than rows the
    /// database reports as new: a re-projection of an unchanged run rewrites
    /// the same rows and counts them again. That is the right number for the
    /// question here ("did this pass carry any content at all"), and it is not
    /// a row delta.
    pub result_rows_written: usize,
    /// Where this tenant's reconcile mark stands **after** this pass, whether
    /// or not this pass is what put it there.
    ///
    /// # This replaced `watermark_advanced_to`, which was the mis-wired alarm
    ///
    /// The old field was documented as "where the watermark ended up, if it
    /// moved" and was set from `Walk::advance_to` — the newest instant the walk
    /// *fully accounted for*, handed to [`WatermarkRepository::advance`]. That
    /// advance is monotonic, so handing it an instant at or behind the stored
    /// mark is a **successful no-op**, and the field was `Some(...)` all the
    /// same. It therefore reported "there was something to advance to", never
    /// "the stored row moved" — which is precisely the distinction the
    /// 2026-09-18 outage turned on, and precisely the one
    /// `crate::gear::report_reconcile_outcome` needed and could not get.
    ///
    /// This one is the stored value: `max(mark before the pass, whatever this
    /// pass advanced to)`, which is what monotonicity makes the row hold. Two
    /// consecutive passes reporting the same instant is therefore evidence
    /// about the row, not about the walk. `None` means this tenant has no mark
    /// at all — the gear has never swept it, or (for
    /// [`ReconcileService::rebuild`]) the pass deliberately neither read nor
    /// wrote one.
    pub watermark_at: Option<OffsetDateTime>,
    /// Whether the walk ended because qa-runs had **nothing further** past the
    /// cursor.
    ///
    /// `true` is the ordinary end of a sweep that has caught up: the last page
    /// came back short or empty, so every finished run this tenant has is
    /// accounted for. `false` means the walk stopped with more to read — a gap,
    /// a spent page budget, or a cursor that could not step — and is what
    /// separates "this tenant's mark has not moved because there is nothing to
    /// move it to" from "this tenant's mark has not moved and there is".
    ///
    /// From [`ReconcileService::rebuild`] it is the same fact about the
    /// operator's window rather than about the tenant: `true` means every run
    /// that finished in `[from, to)` was reached. It was `false` unconditionally
    /// until the rebuild learned to page, because a rebuild that read one page
    /// had no walk to finish.
    pub caught_up: bool,
    /// Where the next request must start to finish what this pass did not.
    ///
    /// `Some(at)` exactly when [`Self::caught_up`] is `false` on a
    /// [`ReconcileService::rebuild`] — the pass ran out of its page budget, or
    /// stopped on a gap — and it is the `from` of the request that continues it.
    /// The instant is the one of the last run this pass *consumed*, so the
    /// continuation re-replays that run and anything tied with it; a rebuild is
    /// idempotent, so re-replaying is free and a strict bound that skipped a tie
    /// group would not be.
    ///
    /// **This is on the wire** (`api::rest::dto::RebuildOutcomeDto`), and that is
    /// the whole point of the field. The truncation it reports used to be a
    /// `WARN` in a log, which the 2026-09-18 outage established is
    /// indistinguishable from success to everyone who is not reading the log at
    /// that moment.
    ///
    /// Always `None` from [`ReconcileService::reconcile_once`]: the sweep's
    /// resume points are durable rather than carried by a caller. The mark is
    /// one of them, and since finding #122 the
    /// [`SweepCursor`] is the other — which is the case the mark cannot serve,
    /// because a pass that spends its budget behind its own mark advances it
    /// nowhere. See [`MAX_PAGES_PER_SWEEP`].
    pub resume_from: Option<OffsetDateTime>,
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
    /// Where this tenant's within-window resume cursor stands **after** this
    /// pass, whether or not this pass is what put it there. `None` means the
    /// row holds no cursor — the steady state, and what a caught-up pass
    /// leaves.
    ///
    /// # An input to `crate::gear::stall_of`, through a high-water
    ///
    /// A tenant draining a lookback window wider than one pass' page budget
    /// moves neither [`Self::watermark_at`] (the runs are behind the mark) nor
    /// [`Self::caught_up`], so on those two fields alone it is a wedge. This
    /// field tells them apart: **a drain moves this instant forward pass after
    /// pass; a wedge does not.** `stall_of` counts a pass under a standing mark
    /// as progress when this instant rises past
    /// `crate::gear::TenantProgress::cursor_high_water`, and the stall `WARN`
    /// and escalation `ERROR` both carry it.
    ///
    /// It used to be documented here as something that **must not** become an
    /// input, for two reasons that still hold and that the high-water answers:
    ///
    /// 1. **It is shared state.** Two replicas sweep one tenant under
    ///    `NoopLeaderElector` and write one cursor row, so this instant can
    ///    move because the *other* replica moved it, and can alternate between
    ///    two positions. Compared against the *highest* value one ticker has
    ///    seen under the current mark rather than the previous pass', an
    ///    oscillation counts as progress at most once.
    /// 2. **A pass-local "did I advance" is `true` on every pass of a genuine
    ///    wedge** in which each pass does real work. The comparison is across
    ///    consecutive passes, not within one: a replica wedge re-walks the same
    ///    ground and writes the same cursor every pass, which reads as standing
    ///    still.
    ///    `reconcile_tests::a_replica_wedge_on_mixed_lookbacks_still_escalates`
    ///    is that wedge, built and measured, and
    ///    `crate::gear::tests::the_resume_cursor_high_water_is_an_input_to_stall_of`
    ///    is what goes red if the cursor is taken out of the decision again.
    pub sweep_cursor_at: Option<OffsetDateTime>,
}

impl ReconcileOutcome {
    /// Fold one page's counters into a walk's running total.
    ///
    /// The three counters and nothing else: every other field is a decision
    /// about where the walk *ended*, which only the walk can make. Shared by
    /// [`Walk::absorb`] and by [`ReconcileService::replay_window`] so that the
    /// sweep's arithmetic and the rebuild's cannot drift apart.
    fn add_counts(&mut self, page: &Self) {
        self.scanned += page.scanned;
        self.backfilled += page.backfilled;
        self.result_rows_written += page.result_rows_written;
    }
}

// `Cursor` used to be defined here, as a private `(finished_at, id)` pair with
// `floor`/`after` constructors. It is `qa_runs_sdk::FinishedRunCursor` now, and
// the move was the point rather than a tidy-up: the instant and the id used to
// travel as two positional parameters through four layers of delegation, where
// keeping the first and dropping the second compiled, read like a legitimate
// first page, and silently restored the bound that stranded three days of
// results. That type's own doc carries the argument; this module's use of it is
// unchanged — `starting_at` for the first page of a walk, `after` for every
// resume.

/// One page of [`ReconcileService::sweep`]'s walk, and where it leaves the walk.
///
/// Not public and not [`ReconcileOutcome`]: `outcome` here is *this page's*
/// contribution, which the caller accumulates, and `next_cursor` is loop control
/// that no caller of the sweep has any use for.
struct PageStep {
    /// What this page scanned and backfilled.
    outcome: ReconcileOutcome,
    /// The `(finished_at, id)` key of the last run this page **fully
    /// accounted for**, if any.
    ///
    /// Two things read it and both need the whole key rather than the instant:
    /// [`Walk::advance_to`] takes its `finished_at` for the watermark, and
    /// [`ReconcileService::sweep`] persists it whole as the
    /// [`SweepCursor`] the next tick resumes from. It used to be an
    /// `Option<OffsetDateTime>` named `advance_to`, and widening it here rather
    /// than adding a second field beside it is deliberate: the walk pages on
    /// one keyset and this module is not going to acquire a second notion of
    /// where it is.
    consumed: Option<FinishedRunCursor>,
    /// Where the next page starts, or `None` to end the walk.
    next_cursor: Option<FinishedRunCursor>,
    /// This page came back short of `page_size` with no gap in it: qa-runs has
    /// nothing further past the cursor. See [`ReconcileOutcome::caught_up`].
    caught_up: bool,
}

/// The running state of one sweep's walk across pages.
#[derive(Default)]
struct Walk {
    /// Every page's contribution so far.
    outcome: ReconcileOutcome,
    /// The key of the newest run any page fully accounted for. See
    /// [`PageStep::consumed`] for why it is the whole key.
    consumed: Option<FinishedRunCursor>,
}

impl Walk {
    /// Fold one page in, and answer where the walk goes next.
    fn absorb(&mut self, step: &PageStep) -> Option<FinishedRunCursor> {
        self.outcome.add_counts(&step.outcome);
        // The *last* page decides, not any page: a walk that read four full
        // pages and then a short one has caught up.
        self.outcome.caught_up = step.caught_up;
        if step.outcome.stopped_at_gap {
            self.outcome.stopped_at_gap = true;
            self.outcome.stopped_at_run = step.outcome.stopped_at_run;
        }
        // Pages walk forward, so a later page's key is the later one.
        if step.consumed.is_some() {
            self.consumed = step.consumed;
        }
        step.next_cursor
    }

    /// The newest instant this walk fully accounted for, for
    /// [`WatermarkRepository::advance`].
    ///
    /// Separate from [`ReconcileOutcome::watermark_at`], which is where the
    /// stored mark *ends up* — the two differ whenever this one is at or behind
    /// the mark, since that advance is monotonic. Derived from
    /// [`Self::consumed`] rather than tracked beside it, so the instant the
    /// mark moves to and the key the cursor records cannot name two different
    /// runs.
    fn advance_to(&self) -> Option<OffsetDateTime> {
        self.consumed.map(FinishedRunCursor::at)
    }

    /// The [`SweepCursor`] this pass leaves behind, given the floor it walked
    /// from, or `None` when it has nothing durable to say.
    ///
    /// `None` on a caught-up pass is the **reset** that keeps this from being a
    /// second watermark — the caller turns it into an erase. `None` on a pass
    /// that consumed nothing is a different `None`, and the caller treats it
    /// differently: see [`ReconcileService::sweep`].
    fn resume_cursor(&self, floor: OffsetDateTime) -> Option<SweepCursor> {
        if self.outcome.caught_up {
            return None;
        }
        self.consumed.and_then(|at| {
            // `after_id` is `Some` for every cursor `consume_page` builds — it
            // constructs them with `FinishedRunCursor::after` — and `and_then`
            // rather than an `expect` because a resume point standing on an
            // assumption is exactly the shape that stranded three days of data
            // once already. Answering `None` costs one re-derivation of the
            // floor, which is the sweep's ordinary behaviour.
            at.after_id().map(|run_id| SweepCursor {
                window_floor: floor,
                at: at.at(),
                run_id,
            })
        })
    }
}

/// The page budget is spent and the walk is ending short of caught up.
///
/// A WARN rather than an error, because the ordinary case of this is a large
/// backlog being worked through: the mark moves as far as this pass got and
/// the next tick resumes from there, which is exactly the property the
/// single-page sweep did not have.
///
/// **The mark is not always what carries the progress**, and this doc said it
/// was until finding #122: a lookback window wider than the whole budget lets a
/// pass spend every page *behind* its own mark, where the advance is a
/// monotonic no-op. What the next tick resumes from in that case is the
/// [`SweepCursor`] this pass persists, not the mark — see
/// [`MAX_PAGES_PER_SWEEP`]. Either way the next tick continues rather than
/// repeating, which is the property the single-page sweep did not have and
/// which the cursor is what makes true in general.
///
/// The line stays, and the walk still leaves [`ReconcileOutcome::caught_up`]
/// `false`, because "this pass did not finish" is a fact an operator should be
/// able to see whether or not the next one picks it up.
fn warn_if_budget_spent(
    tenant: TenantBound,
    page_number: u32,
    scanned: usize,
    cursor: OffsetDateTime,
) {
    if page_number < MAX_PAGES_PER_SWEEP {
        return;
    }
    warn!(
        tenant_id = %tenant.get(),
        pages = MAX_PAGES_PER_SWEEP,
        scanned,
        %cursor,
        "the sweep spent its page budget before catching up; the mark advances as far as this \
         pass got, the resume cursor records where it stopped, and the next tick continues \
         from there",
    );
}

/// Where [`ReconcileService::sweep`]'s first page starts: after the stored
/// resume cursor when it vouches for this pass' window, and at `floor`
/// otherwise.
///
/// # The honour rule: `window_floor <= floor && at >= floor`
///
/// A stored cursor `{window_floor: F_w, at: K}` records that its pass consumed
/// every run in `[F_w, K]` that qa-runs listed — the keyset walk is
/// oldest-first and stops at the first failure, so there is no hole inside
/// that range. A pass whose own floor sits inside it (`F_w <= floor <= K`) is
/// about to walk `[floor, ..)`, and `[floor, K]` is a subset of what the cursor
/// vouches for, so resuming strictly after `K` skips nothing the stored pass
/// did not consume. The cursor this pass then writes carries **its own**
/// floor, which is still truthful: it names a subset of the range covered.
///
/// Both halves are load-bearing:
///
/// * **`window_floor <= floor`.** A cursor says nothing about the band before
///   its own floor. A replica carrying a *wider* lookback (an older floor)
///   must never honour a narrower replica's cursor, or it would step over
///   `[floor, F_w)` — the late-arrival band its wider lookback exists for.
/// * **`at >= floor`.** A cursor standing behind this pass' floor names no
///   position inside this window.
///
/// **This was an equality test until the third review pass (finding #122,
/// residual A).** Equality discarded the cursor on the very tick a drain moved
/// the mark, so a drain that crossed the mark re-walked the lookback from the
/// new floor — work its previous pass had done — and a window wider than the
/// budget took extra ticks to drain for no coverage gained.
///
/// # The known cost: the effective lookback shrinks during a mark-advancing drain
///
/// While a drain is resuming across a moving floor, no pass re-reads
/// `[floor, K]`. A run *written* into that band after the stored pass listed it
/// is therefore not seen until the drain catches up, erases the cursor, and the
/// next tick walks from `mark - lookback` again — by which point the mark has
/// moved on by however far the drain carried it. A late run older than
/// `mark_at_catch_up - lookback` is outside every later window and is recovered
/// only by `POST /qa/v1/insights/rebuild`. That is the price of not
/// re-walking: the equality rule re-read `[floor, K]` on every tick the mark
/// moved, and paid for that coverage with a repeated walk each time.
///
/// Without a cursor the first page takes the inclusive instant bound: the mark
/// names a run this gear has already consumed once, and re-reading it is how a
/// run written after an earlier pass' cutoff is still picked up. With one, the
/// pass resumes strictly after the last run the previous pass consumed — the
/// same `after` bound every page within a pass already uses.
fn first_page_of(
    tenant: TenantBound,
    floor: OffsetDateTime,
    stored: Option<SweepCursor>,
) -> FinishedRunCursor {
    let Some(stored) = stored.filter(|stored| stored.window_floor <= floor && stored.at >= floor)
    else {
        return FinishedRunCursor::starting_at(floor);
    };
    debug!(
        tenant_id = %tenant.get(),
        %floor,
        stored_floor = %stored.window_floor,
        resume_at = %stored.at,
        resume_run = %stored.run_id,
        "a previous pass consumed this window up to its cursor; resuming after it rather than \
         re-walking from the floor",
    );
    FinishedRunCursor::after(stored.at, stored.run_id)
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
    /// The run-completed notification producer — see [`Self::reproject`]'s
    /// "Notification, after the commit".
    ///
    /// `Arc<dyn …>` rather than a third type parameter: the notification
    /// repository is a type this module never otherwise mentions, and
    /// [`RunCompletionNotifier`]'s own doc carries why the seam is a trait.
    notifier: Arc<dyn RunCompletionNotifier>,
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
    /// The page both walks ask qa-runs for, **already bounded by
    /// [`MAX_FINISHED_RUNS_PAGE`]** — see [`Self::new`].
    ///
    /// That bound is what makes `page.len() >= self.page_size` a sound test for
    /// "the listing came back capped": the comparison is only meaningful while
    /// the number on the right is one qa-runs will actually honour.
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
    /// # Nine parameters, and a params struct was the rejected alternative
    ///
    /// `clippy::too_many_arguments` fires at eight, and the fix it implies — a
    /// `ReconcileDeps` struct — would have exactly one construction site
    /// ([`crate::domain::service::AppServices::new`]) which would unpack it
    /// again immediately. What such a struct buys elsewhere is protection
    /// against transposing two arguments of the same type, and there is none to
    /// buy here: all nine types are distinct (`Arc<DBProvider<_>>`, `R`, `W`,
    /// `Arc<dyn RunsReader>`, `Arc<IngestService<R>>`,
    /// `Arc<dyn RunCompletionNotifier>`, `PolicyEnforcer`, `Duration`, `u32`),
    /// so any transposition is a compile error rather than a silent swap.
    /// `expect` rather than `allow`, so that if a later task shortens the list
    /// the attribute itself goes red.
    ///
    /// **It said "eight" until this task added `notifier`**, and the count is
    /// corrected here rather than left to read as the reviewed number: a doc
    /// that miscounts its own signature is how the next addition gets waved
    /// through.
    #[expect(
        clippy::too_many_arguments,
        reason = "all nine parameter types are distinct, so a params struct would guard against \
                  nothing the compiler does not already catch"
    )]
    ///
    /// # `page_size` is bounded here, and this is not a duplicate of the
    /// # configuration's own clamp
    ///
    /// [`crate::config::QaInsightsConfig::effective_reconcile_page_size`] is
    /// where an **operator** is told their number was lowered: it clamps once,
    /// at `init`, and says so at `WARN`. This is where the *type* stops being
    /// able to hold a number qa-runs will not honour, silently, because the
    /// warning already happened and a second one per construction would be
    /// noise.
    ///
    /// They are one rule because they are one constant. Both walks in this
    /// module decide "was that page capped?" by comparing the rows they got
    /// against [`Self::page_size`], and that comparison is *wrong* — not
    /// approximate, wrong, and silently so — for any value above
    /// [`MAX_FINISHED_RUNS_PAGE`]: qa-runs answers with the cap and says
    /// nothing, the walk reads a full page as a short one, sets `caught_up`,
    /// and `crate::gear::stall_of` then declines to escalate because it
    /// requires `!caught_up`. Bounding the stored value rather than each
    /// comparison means a third comparison added later cannot forget.
    ///
    /// **It is floored at one for the same reason** (finding #122's residual
    /// C): a page of zero rows is empty, an empty listing is `caught_up`, and
    /// the sweep would report itself caught up on nothing, every tick, in
    /// silence. The configuration's clamp raises `0` and warns; this makes a
    /// `0` unrepresentable in a constructed service.
    #[must_use]
    pub const fn new(
        db: Arc<DBProvider<DomainError>>,
        results: R,
        watermarks: W,
        runs: Arc<dyn RunsReader>,
        ingest: Arc<IngestService<R>>,
        notifier: Arc<dyn RunCompletionNotifier>,
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
            notifier,
            policy_enforcer,
            lookback,
            // `if` rather than `u32::min`/`clamp`, which are not `const`.
            page_size: if page_size == 0 {
                1
            } else if page_size < MAX_FINISHED_RUNS_PAGE {
                page_size
            } else {
                MAX_FINISHED_RUNS_PAGE
            },
        }
    }

    /// Sweep one tenant once.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] when qa-runs cannot be listed. **The watermark
    /// still carries whatever earlier pages of the same pass earned** — the
    /// walk persists before it propagates, so an error here means "this pass
    /// did not finish", not "this pass did nothing"; see [`Self::sweep`].
    /// A failure on the *first* page is the case where those come to the same
    /// thing, and the mark is untouched.
    ///
    /// A *per-run* backfill failure is **not** an error: it stops the sweep,
    /// leaves the watermark short of the gap, and is reported as
    /// [`ReconcileOutcome::stopped_at_gap`], because the sweep did useful work
    /// and the caller is a ticker that should log and try again rather than
    /// treat it as a fault.
    ///
    /// [`DomainError::Database`] from the watermark read or advance. A failed
    /// advance takes precedence over a failed listing, which is the only way
    /// the two can be reported at once.
    ///
    /// **Every `Err` from here is still reported and still escalates**:
    /// `crate::gear::reconcile_pass` routes it through the same
    /// `report_reconcile_outcome` an `Ok` goes through, so a tenant whose
    /// sweep fails pass after pass reaches `ERROR` on the same
    /// `WEDGED_PASSES_BEFORE_ERROR` schedule a wedged one does. It handled this
    /// arm with a bare `warn!` until the second review's #123, which meant a
    /// permanent failure could never raise anything above `WARN`.
    pub async fn reconcile_once(
        &self,
        tenant: TenantBound,
    ) -> Result<ReconcileOutcome, DomainError> {
        let outcome = self.sweep(tenant).await?;
        info!(
            tenant_id = %tenant.get(),
            scanned = outcome.scanned,
            backfilled = outcome.backfilled,
            // Beside `backfilled` rather than instead of it, and that pairing
            // is the point: `backfilled=116 result_rows_written=0` is a
            // treadmill and reads as one, where `backfilled=116` alone read as
            // health for three days. See `ReconcileOutcome::backfilled`.
            result_rows_written = outcome.result_rows_written,
            stopped_at_gap = outcome.stopped_at_gap,
            caught_up = outcome.caught_up,
            watermark_at = ?outcome.watermark_at,
            // Beside `watermark_at` for the reason `result_rows_written` sits
            // beside `backfilled`: a pass with an unmoved mark and a
            // *forward-moving* cursor is a large window draining, and a pass
            // with both standing still is wedged. One of those needs an
            // operator and the other does not.
            sweep_cursor_at = ?outcome.sweep_cursor_at,
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
    /// # The window is walked in pages, under a budget the answer names
    ///
    /// **Until 2026-09-21 this read one page and stopped**, with a `WARN` and a
    /// `scanned` an operator had to compare against a `page_size` the response
    /// does not carry. It was the last page boundary in this module that was a
    /// wall rather than a step, and it was the same shape as the outage in two
    /// separate ways:
    ///
    /// * A window holding more than `page_size` runs replayed its oldest page
    ///   and reported plain success. The remedy the log line named — narrow the
    ///   window and repeat — worked, as long as an operator read the log.
    /// * A window whose first `page_size` runs shared **one** `finished_at`
    ///   could not be replayed at all, by any narrowing. Narrowing to that
    ///   single instant returns the same `page_size` runs, so the runs behind
    ///   them were unreachable through this endpoint — and a tie group that wide
    ///   is what the 2026-09-18 burst was made of.
    ///
    /// It now walks [`FinishedRunCursor`], the same `(finished_at, id)` keyset the sweep
    /// walks, so a tie group is a step rather than a wall and `page_size` is
    /// what it says it is: the size of a page, not the size of the job.
    ///
    /// **The cap stays, and it is a budget rather than a bound on the window.**
    /// [`MAX_PAGES_PER_REBUILD`] pages, after which the pass stops and reports
    /// [`ReconcileOutcome::resume_from`] — the `from` of the request that
    /// continues it. An unbounded walk was the rejected alternative: this is a
    /// synchronous request, an operator can type a year-wide window, and a walk
    /// that outlives the gateway's timeout returns *nothing at all*, which is
    /// the silent partial in its worst form. What makes a cap acceptable here
    /// and not before is that exceeding it is now in the answer rather than in
    /// a log line: `caught_up` is `false` and `resume_from` says where to pick
    /// up, both on the wire as `complete` and `resume_from`.
    ///
    /// The one shape a resume still cannot step over is a single instant
    /// holding more than `MAX_PAGES_PER_REBUILD * page_size` runs — 2,000 at
    /// the defaults, against the 54 the outage burst's widest tie group held —
    /// because `resume_from` carries the instant and not the whole key. That
    /// would report the same `resume_from` twice in a row, which is visible;
    /// the wall it replaces was not.
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
    /// [`DomainError::Internal`] when qa-runs cannot be listed at all — on the
    /// first page. A listing that fails on a later page is a partial answer
    /// carrying [`ReconcileOutcome::resume_from`], not an error; see
    /// `Self::replay_window`. A *per-run* failure is not an error either — see
    /// [`Self::reconcile_once`], which says the same thing for the same reason.
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

        let outcome = self.replay_window(ctx, tenant, &scope, from, to).await?;

        info!(
            tenant_id = %tenant.get(),
            %from,
            %to,
            scanned = outcome.scanned,
            replayed = outcome.backfilled,
            result_rows_written = outcome.result_rows_written,
            complete = outcome.caught_up,
            resume_from = ?outcome.resume_from,
            stopped_at_gap = outcome.stopped_at_gap,
            stopped_at_run = ?outcome.stopped_at_run,
            "operator rebuild finished",
        );

        // `watermark_at` is `None`, and that is meaningful: this pass neither
        // read nor wrote the mark, by construction.
        Ok(outcome)
    }

    /// Walk `[from, to)` page by page, replaying every run in it.
    ///
    /// Split out of [`Self::rebuild`] for the same reason [`Self::sweep`] is
    /// split out of [`Self::reconcile_once`] — so the shape of the operation is
    /// the body of the function rather than something to reconstruct from it.
    /// The two walks are deliberately close to each other and deliberately not
    /// one function: this one has no watermark to read or advance, no diff
    /// against what is already projected, and an upper bound the sweep has no
    /// counterpart for. What they *do* share — the cursor, the page budget, the
    /// per-run projection, the stop-at-the-first-failure rule — is shared code.
    ///
    /// # A listing failure after the first page is a partial answer
    ///
    /// Answered `Ok` with `caught_up = false` and
    /// [`ReconcileOutcome::resume_from`] on the last run fully consumed — the
    /// same shape as spending the page budget, logged at `WARN` with the
    /// error. Until finding #122's residual D it was an `Err` that discarded
    /// the report of the pages already replayed, on the grounds that re-running
    /// the window is idempotent. It is, but it is also the whole window again,
    /// and the operator was left without the one value that lets them continue
    /// instead.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] when qa-runs cannot be listed on the **first**
    /// page: nothing was observed, so there is no position to resume from and
    /// "the window held nothing" must not be what the caller reads.
    async fn replay_window(
        &self,
        ctx: &SecurityContext,
        tenant: TenantBound,
        scope: &AccessScope,
        from: OffsetDateTime,
        to: OffsetDateTime,
    ) -> Result<ReconcileOutcome, DomainError> {
        let mut cursor = FinishedRunCursor::starting_at(from);
        let mut outcome = ReconcileOutcome::default();

        for page_number in 1..=MAX_PAGES_PER_REBUILD {
            // The caller's own context, not a system actor: see decision 4 in
            // this module's header.
            let page = match self
                .runs
                .list_runs_finished_since(ctx, cursor, self.page_size)
                .await
            {
                Ok(page) => page,
                Err(error) if page_number == 1 => return Err(error),
                Err(error) => {
                    // `caught_up` stays `false`; the pages before this one are
                    // replayed and `cursor` stands on the last run they
                    // consumed. See this function's doc.
                    outcome.resume_from = Some(cursor.at());
                    warn!(
                        tenant_id = %tenant.get(),
                        page = page_number,
                        scanned = outcome.scanned,
                        resume_from = %cursor.at(),
                        %error,
                        "the rebuild could not list qa-runs part way through the window; the \
                         response carries the instant to resume from",
                    );
                    return Ok(outcome);
                }
            };

            // Measured on the page *as listed*: it is the listing that was
            // capped, and the upper-bound cut below can leave a full page
            // looking short.
            let saturated = page.len() >= self.page_size as usize;
            // The listing is oldest-first, so one run at or past the upper bound
            // means the window holds nothing further whatever the cap says.
            let crossed_upper_bound = page
                .iter()
                .any(|run| run.finished_at.is_some_and(|at| at >= to));

            // The port guarantees the lower bound (`finished_at >= cursor`) and
            // that a run with no finish instant is never returned. The upper
            // bound is this walk's, and it is exclusive.
            let window: Vec<Run> = page
                .into_iter()
                .filter(|run| run.finished_at.is_some_and(|at| at < to))
                .collect();

            let (page_outcome, consumed) = self.replay(tenant, scope, window).await;
            outcome.add_counts(&page_outcome);

            if page_outcome.stopped_at_gap {
                outcome.stopped_at_gap = true;
                outcome.stopped_at_run = page_outcome.stopped_at_run;
                // The run before the failure, or the page's own start when the
                // failure was the first run on it. Either way the continuation
                // re-reaches the run that failed rather than stepping past it.
                outcome.resume_from = Some(consumed.map_or(cursor.at(), FinishedRunCursor::at));
                return Ok(outcome);
            }

            if crossed_upper_bound || !saturated {
                outcome.caught_up = true;
                return Ok(outcome);
            }

            let Some(next) = consumed else {
                // A full page that yielded no cursor at all. Unreachable: the
                // page size is at least one (`Self::new` floors it), so a
                // saturated page that did not cross the upper bound is
                // non-empty and every run on it carries an instant. Answered
                // rather than asserted, because a walk that met it with another
                // identical listing would spin.
                outcome.resume_from = Some(cursor.at());
                return Ok(outcome);
            };
            cursor = next;
        }

        // The budget is spent with the window not exhausted. `caught_up` stays
        // `false` and the answer carries the resume point; see this type's
        // `resume_from` for why that is a field and not a `WARN`.
        outcome.resume_from = Some(cursor.at());
        warn!(
            tenant_id = %tenant.get(),
            pages = MAX_PAGES_PER_REBUILD,
            scanned = outcome.scanned,
            resume_from = %cursor.at(),
            "the rebuild spent its page budget before reaching the end of the window; the \
             response carries the instant to resume from",
        );
        Ok(outcome)
    }

    /// Re-project every run in `window`, stopping at the first that fails, and
    /// report the last run it fully accounted for.
    ///
    /// No watermark and no diff, which is the whole difference from
    /// [`Self::consume_page`]: every run in the window is replayed, because the
    /// premise of a rebuild is that the existing projection is what is wrong.
    ///
    /// The returned [`FinishedRunCursor`] is where the walk resumes, and it is the last run
    /// *consumed* rather than the last run listed — a page that stopped at a gap
    /// must not hand the walk a cursor past it.
    async fn replay(
        &self,
        tenant: TenantBound,
        scope: &AccessScope,
        window: Vec<Run>,
    ) -> (ReconcileOutcome, Option<FinishedRunCursor>) {
        let mut outcome = ReconcileOutcome {
            scanned: window.len(),
            ..ReconcileOutcome::default()
        };
        let mut consumed = None;

        for run in window {
            match self
                .reproject(SystemActorSite::OperatorRebuild, tenant, scope, run.id)
                .await
            {
                Ok(rows) => {
                    outcome.backfilled += 1;
                    outcome.result_rows_written += rows;
                }
                Err(err) => {
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
            }
            // `map`/`or` rather than an `else` arm: the caller's filter already
            // dropped every run without an instant, and a run that somehow has
            // none simply does not move the cursor.
            consumed = run
                .finished_at
                .map(|at| FinishedRunCursor::after(at, run.id))
                .or(consumed);
        }

        (outcome, consumed)
    }

    /// The sweep itself, with the summary log lifted out to its caller.
    ///
    /// # Why this pages, and what happened for three days when it did not
    ///
    /// Until 2026-09-21 this read **one** page per tick and advanced the mark
    /// to the last run in it. That is correct only while the lookback window
    /// holds fewer than `page_size` runs, and it wedges *permanently* the
    /// moment it does not:
    ///
    /// * [`Self::floor`] starts the read at `mark - lookback`, deliberately
    ///   **behind** the mark, so a run written after an earlier pass' cutoff is
    ///   still picked up.
    /// * The listing is capped at `page_size` and ordered oldest-first.
    /// * So when `page_size` runs finished inside `[mark - lookback, mark]`,
    ///   the page ends at or before the mark — and
    ///   [`WatermarkRepository::advance`] is monotonic by construction, so the
    ///   advance is a *successful no-op*.
    /// * The next tick computes the same floor from the same mark, reads the
    ///   same page, and makes the same no-op. Forever.
    ///
    /// Nothing about that is observable from the outside: no error, no
    /// `stopped_at_gap`, and a healthy-looking `scanned`/`backfilled` on every
    /// tick, while the projection stops dead at the instant it wedged. The mark
    /// re-derives the same floor after a restart, so redeploying does not clear
    /// it either.
    ///
    /// Observed on the dev stand: a 1700-run load test on 2026-09-18 put
    /// exactly 200 runs — `page_size` — into the hour behind the mark, freezing
    /// `last_reconciled_finished_at` at `2026-09-18T14:04:16.391692Z` for three
    /// days and across five redeploys. The wedging window is *historical* data,
    /// so it never drains and the sweep never recovers on its own.
    ///
    /// The fix is to keep reading: each page advances a cursor to its own
    /// newest instant, the next page starts there, and the loop ends when a
    /// page comes back short (caught up), a backfill fails (a gap — the mark
    /// must not move past it), or [`MAX_PAGES_PER_SWEEP`] is spent. The mark
    /// then lands on the newest run this pass fully accounted for, which is
    /// ahead of where it started whenever any run is.
    ///
    /// # The cursor is a keyset one, and the residual window is closed
    ///
    /// The first version of this walk carried a bare instant and relied on the
    /// port's inclusive `>=` bound, which left one window it still could not
    /// walk: `page_size` runs sharing a single identical `finished_at`. The
    /// page's newest instant is that instant, so the next page is the same
    /// page — the walk stopped, the mark stopped with it, and the best that
    /// shape could do was log an `ERROR` instead of spinning. The data stayed
    /// stranded, and the burst that caused the 2026-09-18 outage contains
    /// single instants shared by 54, 40, 31 and 29 runs.
    ///
    /// [`FinishedRunCursor`] is `(finished_at, id)` — the total order qa-runs already
    /// sorts this listing by — so a resume is strictly after the last run
    /// *consumed*, and a page always steps. Two consequences fall out: there is
    /// no de-duplication left to do, because no run is listed twice in a walk,
    /// and there is no un-pageable window left to report.
    ///
    /// # The cursor now survives the pass, and that is finding #122
    ///
    /// Everything above is about one pass. Between passes the walk restarted at
    /// `mark - lookback` and **only** there, which left one window it could not
    /// drain: a lookback holding more runs than [`MAX_PAGES_PER_SWEEP`] pages.
    /// Such a pass spends every page behind its own mark, the advance is a
    /// monotonic no-op, and the next tick derives the identical floor. Real work
    /// on every tick, no progress on any, and only the `WARN` and
    /// [`crate::gear`]'s escalation ladder to say so.
    ///
    /// So a pass that ends short of catching up persists its last consumed key
    /// as a [`SweepCursor`] and the next tick resumes from it. Three things make
    /// that a *within-window* resume rather than a second mark, and the order
    /// matters:
    ///
    /// 1. The cursor carries the floor its pass walked from, and this function
    ///    honours it only when that floor is at or before this pass' floor and
    ///    the cursor stands at or after it (`first_page_of`'s honour rule). A
    ///    mark that moved during a drain keeps the cursor — `[floor, at]` is
    ///    still a range the stored pass consumed — and a replica with a wider
    ///    lookback, whose floor is earlier, discards it.
    /// 2. A caught-up pass **erases** it, unconditionally. So the tick after a
    ///    drain finishes starts at `mark - lookback` again and re-reads the
    ///    whole lookback — which is the only reason the lookback works at all:
    ///    a run can be *written* with a `finished_at` earlier than one already
    ///    swept, and re-reading is how it is found.
    /// 3. A pass that consumed nothing writes nothing, leaving an earlier pass'
    ///    cursor standing. A listing that fails on page one has observed no
    ///    position, and erasing on no observation is the same mistake #123's
    ///    `?` made with the mark.
    ///
    /// **Two replicas share the row and neither can skip a run.** Under the
    /// shipped `NoopLeaderElector` every replica is the leader
    /// ([`crate::infra::leader`]'s header: election here is an optimisation, not
    /// mutual exclusion), so the write is last-writer-wins with no monotonic
    /// predicate — it has to be, since the cursor must be erasable. What makes
    /// that safe is *what the value is*: every cursor `{window_floor, at}`
    /// written is a claim that every run listed in `[window_floor, at]` was
    /// consumed — by its writer, or by the earlier pass whose cursor its writer
    /// honoured, which the honour rule only allows when that earlier range
    /// covers `[window_floor, ..]`. A replica can therefore only move the
    /// cursor to a position some pass genuinely walked to, or backwards to an
    /// earlier one. Backwards costs a repeated page, which every write on this
    /// path is idempotent under. There is no interleaving that leaves a run
    /// between the two positions unread.
    /// [`SweepCursor`]'s own doc carries the argument.
    async fn sweep(&self, tenant: TenantBound) -> Result<ReconcileOutcome, DomainError> {
        // `for_reconcile_sweep`, not `for_event_ingest`: the `site` on the audit
        // line is the whole reason `system_actor` has one factory per flow, and
        // this call site emitted the consumer's name from Task 15 until Phase
        // A's fix wave.
        let ctx = system_actor::for_reconcile_sweep(tenant);
        let scope = AccessScope::for_tenant(tenant.get());

        // Kept, not just turned into a floor: it is half of
        // `ReconcileOutcome::watermark_at`, and the whole point of that field
        // is that "where the mark stands" must be a value this pass observed
        // rather than one inferred from the walk.
        let marks = self.marks(&scope).await?;
        let mark_before = marks.last_reconciled_finished_at;
        let floor = mark_before.map_or(OffsetDateTime::UNIX_EPOCH, |mark| mark - self.lookback);

        let mut cursor = first_page_of(tenant, floor, marks.sweep_cursor);
        let mut walk = Walk::default();

        // **Held, not propagated with `?`.** See the section below on why a
        // page failure must not take the walk's earned progress with it.
        let mut failure: Option<DomainError> = None;

        for page_number in 1..=MAX_PAGES_PER_SWEEP {
            let step = match self.sweep_page(&ctx, tenant, &scope, cursor).await {
                Ok(Some(step)) => step,
                Ok(None) => {
                    // An empty listing carries the same fact a short page does:
                    // qa-runs has nothing past the cursor.
                    walk.outcome.caught_up = true;
                    break;
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            };
            let Some(next) = walk.absorb(&step) else {
                break;
            };
            cursor = next;
            warn_if_budget_spent(tenant, page_number, walk.outcome.scanned, cursor.at());
        }

        let mut outcome = walk.outcome;
        if outcome.scanned == 0 {
            debug!(tenant_id = %tenant.get(), %floor, "reconcile sweep found no runs in the window");
        }

        // **Before the failure is propagated, and that ordering is the fix for
        // finding #123.** `sweep_page` used to be called with `?`, so a
        // failure on page k returned from here with pages 1..k-1 already
        // written to `qa_test_results` and the mark still where the pass found
        // it. The next tick recomputed the same floor, re-walked the same
        // pages, and lost them again — for as long as qa-runs stayed
        // unreachable, and once more on the pass after it recovered.
        //
        // Advancing is safe precisely because `advance_to` is the newest
        // instant a page **fully accounted for**: `sweep_page`'s own failures
        // are both raised before `consume_page` runs, so a failed page
        // contributes nothing to it, and a page that stopped at a gap
        // contributes only the runs before the gap. There is no case where
        // this moves the mark past a run the sweep did not consume.
        if let Some(at) = walk.advance_to() {
            // `?`, so a watermark write that fails wins over a listing that
            // failed: the listing failure is a transient the next tick
            // retries, while a mark that cannot be written is the fault that
            // makes retrying pointless. Either way the pass ends in `Err`.
            self.advance_watermark(tenant, at).await?;
        }

        // Finding #122's fix. After the advance, because `advance` is what
        // creates the tenant's row; before the failure is propagated, for the
        // same reason the advance is — a pass that ended in an error still
        // earned whatever it consumed. See `Self::persist_resume_point`.
        let sweep_cursor_at = self
            .persist_resume_point(tenant, &walk, floor, marks.sweep_cursor)
            .await?;

        if let Some(error) = failure {
            return Err(error);
        }

        // `max`, because `WatermarkRepository::advance` is monotonic: handing
        // it an instant at or behind the stored mark succeeds and changes
        // nothing. This is therefore the value the row holds now, which is the
        // only thing a caller comparing consecutive passes can reason from.
        // `None` orders below every `Some`, so an unset mark and a first
        // advance both come out right.
        outcome.watermark_at = walk.advance_to().max(mark_before);
        // Where the row stands, for the operator and for
        // `crate::gear::stall_of`'s cursor high-water. See the field's own doc.
        outcome.sweep_cursor_at = sweep_cursor_at;

        Ok(outcome)
    }

    /// One page of [`Self::sweep`]'s walk: list, diff, consume, and say where
    /// the walk goes next.
    ///
    /// `Ok(None)` is an empty listing — the walk is over and nothing was
    /// consumed. Split out of `sweep` so that the loop reads as the walk and
    /// this reads as the page; they were one function until clippy's
    /// cognitive-complexity lint made the case that they are two things.
    async fn sweep_page(
        &self,
        ctx: &SecurityContext,
        tenant: TenantBound,
        scope: &AccessScope,
        cursor: FinishedRunCursor,
    ) -> Result<Option<PageStep>, DomainError> {
        let page = self
            .runs
            .list_runs_finished_since(ctx, cursor, self.page_size)
            .await?;

        if page.is_empty() {
            // Caught up, and the caller records it: `sweep` treats `None` as
            // "the walk is over with nothing further to read", which is exactly
            // `caught_up`.
            return Ok(None);
        }

        // Measured on the page as listed: it is the listing that was capped,
        // and "is there more after this" is a question about the cap.
        let saturated = page.len() >= self.page_size as usize;

        let already = self.already_ingested(scope, cursor.at(), &page).await?;

        // No `seen` set and no de-duplication. Under an instant-only cursor
        // every page re-listed its predecessor's boundary ties and those had to
        // be filtered out here; under the keyset cursor the bound is strict on
        // the whole key, so a run is listed once per walk by construction.
        let (outcome, consumed) = self.consume_page(tenant, scope, page, &already).await;

        // Resume after the last run this page *consumed*, never merely listed:
        // a page that stopped at a gap must not hand the walk a cursor past it.
        let next_cursor = (!outcome.stopped_at_gap && saturated)
            .then_some(consumed)
            .flatten();

        let caught_up = !saturated && !outcome.stopped_at_gap;

        Ok(Some(PageStep {
            outcome,
            consumed,
            next_cursor,
            caught_up,
        }))
    }

    /// This tenant's persisted sweep state as it stands before the sweep runs:
    /// the reconcile mark and the within-window resume cursor.
    ///
    /// A missing `last_reconciled_finished_at` means this gear has never swept
    /// this tenant, and [`Self::sweep`] turns that into a floor at the
    /// beginning of time — `page_size` and [`MAX_PAGES_PER_SWEEP`] are what
    /// keep that first pass finite. Legacy reads every workflow on every cycle,
    /// so starting from the beginning *once* is not the aggressive choice; it
    /// is the one that makes the projection complete.
    ///
    /// **This used to be `floor()` and return the subtracted instant**, then
    /// `mark()` and return the instant itself. It is the whole
    /// [`Watermarks`] now because the sweep
    /// needs three things out of one row — the floor, the "before" half of
    /// [`ReconcileOutcome::watermark_at`], and the resume cursor — and reading
    /// the row three times, or re-deriving one value from another, is how two
    /// spellings of one position come to disagree.
    async fn marks(&self, scope: &AccessScope) -> Result<Watermarks, DomainError> {
        let conn = self.db.conn()?;
        self.watermarks.get(&conn, scope).await
    }

    /// Write, erase, or deliberately leave alone this tenant's resume cursor,
    /// according to how the walk ended.
    ///
    /// The three arms are three different facts and collapsing any two of them
    /// reintroduces a defect this module has already shipped:
    ///
    /// * **Caught up: erase.** This is the reset. The next tick is back to
    ///   `mark - lookback` and re-walks the whole lookback, which is the only
    ///   reason a run *written* with a `finished_at` behind an earlier sweep is
    ///   ever found. A cursor that survived a caught-up pass would be a second
    ///   monotonic mark with the lookback dead behind it.
    /// * **Stopped short with a position: write it.** The next tick continues
    ///   the window instead of re-deriving the same floor and doing the same
    ///   work forever, which is finding #122.
    /// * **Stopped short with no position: write nothing.** A listing that
    ///   failed on the first page, or a gap on the first run, observed no
    ///   position at all. Erasing on no observation throws away an earlier
    ///   pass' progress — the same mistake #123's `?` made with the mark.
    ///
    /// Answers **where the row stands afterwards**, which is
    /// [`ReconcileOutcome::sweep_cursor_at`] — the third arm makes that
    /// different from "what this pass wrote", so it is returned from the one
    /// place that decides rather than reconstructed by the caller.
    async fn persist_resume_point(
        &self,
        tenant: TenantBound,
        walk: &Walk,
        floor: OffsetDateTime,
        stored: Option<SweepCursor>,
    ) -> Result<Option<OffsetDateTime>, DomainError> {
        match (walk.outcome.caught_up, walk.resume_cursor(floor)) {
            (true, _) => {
                self.write_sweep_cursor(tenant, None).await?;
                Ok(None)
            }
            (false, Some(cursor)) => {
                self.write_sweep_cursor(tenant, Some(cursor)).await?;
                Ok(Some(cursor.at))
            }
            // Nothing written, so the row still holds whatever this pass read
            // at its start — including a cursor this pass did not honour,
            // which is what the row holds and therefore what to report.
            (false, None) => Ok(stored.map(|stored| stored.at)),
        }
    }

    /// Record or erase this tenant's within-window resume cursor, in its own
    /// transaction.
    ///
    /// Its own transaction for [`Self::advance_watermark`]'s reason and not for
    /// a stronger one: the cursor is advisory, so a crash between the advance
    /// and this write costs one re-walk of a window and never a run. It is a
    /// separate statement from the advance rather than one row update because
    /// the two have opposite rules — the mark is monotonic and this must be
    /// able to move backwards and to erase. See
    /// [`WatermarkRepository::set_sweep_cursor`].
    async fn write_sweep_cursor(
        &self,
        tenant: TenantBound,
        cursor: Option<SweepCursor>,
    ) -> Result<(), DomainError> {
        let tenant_id = tenant.get();
        let watermarks = self.watermarks.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    watermarks
                        .set_sweep_cursor(tx, &AccessScope::for_tenant(tenant_id), cursor)
                        .await
                })
            })
            .await
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

    /// Walk the page in order, backfilling what is missing, and report the last
    /// run it fully accounted for.
    ///
    /// That run is two things at once and both are load-bearing: its instant is
    /// how far the watermark may move, and its whole `(finished_at, id)` key is
    /// where the next page resumes. **It stops at the first failure**, because
    /// the page is oldest-first and everything after a gap is newer: advancing
    /// past one would put the next sweep's floor beyond a run that was never
    /// ingested, and resuming past one would do the same to the walk.
    async fn consume_page(
        &self,
        tenant: TenantBound,
        scope: &AccessScope,
        page: Vec<Run>,
        already: &[Uuid],
    ) -> (ReconcileOutcome, Option<FinishedRunCursor>) {
        let mut outcome = ReconcileOutcome {
            scanned: page.len(),
            ..ReconcileOutcome::default()
        };
        let mut consumed = None;

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
                match self
                    .reproject(SystemActorSite::ReconcileSweep, tenant, scope, run.id)
                    .await
                {
                    Ok(rows) => {
                        outcome.backfilled += 1;
                        outcome.result_rows_written += rows;
                    }
                    Err(err) => {
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
                }
            }

            consumed = Some(FinishedRunCursor::after(finished_at, run.id));
        }

        (outcome, consumed)
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
    /// # Notification, after the commit — this gear's only producer for
    /// # `notify_run_completed`
    ///
    /// The run's terminal transition, as far as this gear can observe one, is
    /// the instant its projection lands. So this is where
    /// [`RunCompletionNotifier::notify_run_completed`] is called, and it is
    /// called **after** [`Self::notify_run_completed`]'s transaction has
    /// committed, never from inside it: that call ends in an SMTP conversation
    /// or a Slack webhook, and an open database transaction held across
    /// network egress pins a connection and its locks for the length of a
    /// ten-second relay timeout. It is also skipped entirely on the
    /// `read_run_projection` → `None` arm above, which returns before this
    /// point: a run qa-runs no longer has is not a run that just finished.
    ///
    /// **Both callers notify, and three things are why that is safe.**
    /// [`Self::consume_page`] reaches this only for a run the diff found
    /// missing, but [`Self::replay`] reaches it for *every* run in an
    /// operator's window — so a rebuild over a month of history would, with
    /// nothing else in place, mail a month of finished runs at once.
    ///
    /// * `m20260929_000004_run_completed_notification_cutoff` records when
    ///   this deployment began notifying, and
    ///   [`NotifyService::notify_run_completed`](crate::domain::service::notify::NotifyService::notify_run_completed)
    ///   declines any run that finished before it. That closes the whole class
    ///   at once, without depending on any table being a faithful census of
    ///   what was ingested.
    /// * `m20260929_000003_seed_run_completed_notification_claims` inserts one
    ///   claim row per already-projected run per run-completed channel, which
    ///   makes every run this gear had already ingested read as "already
    ///   sent" — the correct semantics, because `qa_run_notifications` **is**
    ///   the record of what has been notified, not a suppression list bolted
    ///   beside one.
    /// * The **claim a declined channel takes anyway**
    ///   ([`NotifyService::decline_run_completed_channel`](crate::domain::service::notify)),
    ///   which is what the first two could not cover: a run that finished
    ///   *after* the upgrade and was never sent — because routing declined it,
    ///   or no webhook or SMTP destination was configured — has no seeded claim
    ///   and is not history, so before that claim existed a rebuild announced
    ///   it as soon as the channel was turned on. The two migrations are
    ///   upgrade-time answers; this one is the standing invariant, and it is
    ///   what makes a rebuild safe to run on a stand whose notification
    ///   settings have changed since. `DESIGN.md` §3.9 states the consequence
    ///   for operators.
    ///
    /// **Neither migration replaces the other**, and the case that separates
    /// them is
    /// item 2 of this module's header: a run with genuinely zero results
    /// leaves no row in `qa_test_results`, so the claim seed can never name it
    /// and the diff never calls it ingested — it is re-projected on every
    /// single pass and reaches this function every time. Measured on the dev
    /// stand 2026-09-29, that is 1095 of 2326 finished runs. Only the cutoff
    /// covers them. Conversely the cutoff cannot judge a run whose
    /// `finished_at` lands after the migration's own clock through skew, and
    /// there the claim row is what answers.
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
    ) -> Result<usize, DomainError> {
        // Before any transaction opens: see this function's own doc for why
        // a cross-gear read nested inside one trips `toolkit_db`'s
        // transaction-bypass guard in a single-binary deployment.
        let Some(projection) = self
            .ingest
            .read_run_projection(site, tenant, run_id)
            .await?
        else {
            // A run qa-runs no longer has. `Ok`, because there is nothing to
            // project and nothing is wrong — and **zero**, because nothing was
            // written. `ReconcileOutcome::backfilled` counts this pass all the
            // same; `result_rows_written` is what does not.
            return Ok(0);
        };

        let ingest = Arc::clone(&self.ingest);
        // Cloned into the closure: `DBProvider::transaction` takes a
        // `for<'a> FnOnce(&'a DbTx<'a>) -> … + 'a`, which the compiler resolves
        // to `'static`, so a borrowed `&AccessScope` cannot cross it. Only the
        // write needs this now; the read above already ran to completion.
        let owned_scope = scope.clone();
        let rows = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    ingest
                        .write_run_projection(tx, &owned_scope, tenant, run_id, projection)
                        .await
                })
            })
            .await?;

        // **After the commit, never inside it.** See this function's
        // "Notification, after the commit" section.
        self.notify_run_completed(site, tenant, run_id).await;

        Ok(rows)
    }

    /// Tell [`RunCompletionNotifier`] that `run_id` finished, and swallow its
    /// failure.
    ///
    /// # Why the failure is swallowed rather than returned
    ///
    /// [`Self::reproject`]'s `Err` is what makes
    /// [`ReconcileOutcome::stopped_at_gap`] true, and a gap stops the sweep and
    /// pins the watermark short of the run — permanently, for that tenant,
    /// until an operator intervenes (this module's header, "a tenant's backfill
    /// can wedge"). The projection is the thing that must not advance past a
    /// hole; a notification that did not go out is not a hole in it. Letting a
    /// PDP hiccup or a notification-table failure wedge a tenant's *ingest*
    /// would trade the gear's whole purpose for its least critical side effect.
    ///
    /// [`RunCompletionNotifier::notify_run_completed`] already swallows every
    /// *send* failure into the audit log, so what reaches here is a denial or a
    /// database failure — both of which a `WARN` names and a human fixes.
    async fn notify_run_completed(&self, site: SystemActorSite, tenant: TenantBound, run_id: Uuid) {
        // The same actor the read above was issued under, minted fresh rather
        // than threaded: `site.context` is what keeps the tenant on the
        // context and the tenant of the projection provably the same value
        // (`domain::system_actor`'s `SystemActorSite`).
        let ctx = site.context(tenant);
        if let Err(error) = self.notifier.notify_run_completed(&ctx, run_id).await {
            warn!(
                run_id = %run_id,
                tenant_id = %tenant.get(),
                error = %error,
                "the run-completed notification could not be attempted; the projection is \
                 unaffected and the sweep continues",
            );
        }
    }
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod reconcile_tests;
