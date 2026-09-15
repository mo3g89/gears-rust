//! The projection every result row lands through.
//!
//! # One shape: re-read the run, replace its projection
//!
//! A run whose results this gear projects is handled as an **invalidation**,
//! not a delta: the service re-reads that run's authoritative per-test list
//! from qa-runs and hands the whole list to
//! [`ResultsRepository::upsert_run_results`](crate::domain::repos::ResultsRepository::upsert_run_results),
//! which is a delete-then-insert per run.
//!
//! Two properties fall out of that choice, and each of them is a requirement
//! somewhere else in this subsystem:
//!
//! 1. **A re-run is a no-op.** Sweeping the same run twice, or rebuilding an
//!    overlapping window, re-runs delete-then-insert over identical rows —
//!    which is what makes the reconcile sweep safe to re-drive after a failure
//!    and `POST /qa/v1/insights/rebuild` safe to repeat over a window that was
//!    already replayed (`RebuildOutcomeDto`'s doc: "re-running the same window
//!    is idempotent and is the intended recovery").
//! 2. **A run whose earlier sweep failed self-heals on the next one.** Because
//!    each pass re-reads the *whole* run rather than applying a delta, a
//!    partial or stale write from a failed attempt does not linger — the next
//!    successful pass replaces it outright.
//!
//! ## There used to be two callers of this shape, and now there is one
//!
//! Until this was retired, a transactional broker consumer called the same
//! re-read-and-replace logic for a `run.finished` event, and this paragraph
//! argued that the event path and the reconciler's backfill path had to *be*
//! one path rather than two that could diverge after an outage. No deployment
//! ever registered the broker client that consumer needed, so it never ran in
//! practice; it and the `event-broker` dependency it needed were deleted once
//! that was established (`crate::gear`'s header, "Event ingest, and why there
//! is only one path"). The reconcile sweep was always the path that actually
//! executed, and it is now the only one there is.
//!
//! # Scoping: `AccessScope::for_tenant`, not a `PolicyEnforcer` decision
//!
//! Every request-driven caller in this subsystem compiles its scope from the
//! PEP. This path does not, and the reason is the first of these two — the
//! second only restates it from the round-trip's own side:
//!
//! * **The tenant is not caller-supplied.** [`IngestService::read_run_projection`]
//!   takes a [`TenantBound`] its caller already resolved — the reconcile
//!   sweep's own tenant enumeration, or a rebuild's caller context — and
//!   `AccessScope::for_tenant` is *the same value* a PEP compiles for a
//!   tenant-isolated subject — one `owner_tenant_id IN [tenant]` constraint —
//!   which is what `infra::storage::mod`'s test harness already records about
//!   its own use of it.
//! * **A PEP round-trip here would buy nothing.** `read_run_projection` takes
//!   a non-nil `TenantBound` by construction, and the shipped
//!   `static-authz-plugin` derives its decision purely from the resolved
//!   tenant — it grants any non-nil tenant an `owner_tenant_id IN [tenant]`
//!   clamp regardless of subject or action, the same constraint the first
//!   bullet already names. Asking would not be denied; it would compile back
//!   exactly the scope the caller already resolved, at the cost of a round
//!   trip with no additional access-control decision made.
//!
//! `AccessScope::allow_all()` still appears nowhere on this path, and this is
//! not a step toward it: the scope built here is bound to exactly the tenant
//! its caller resolved. (This crate's production code does construct one, at
//! exactly one call site unrelated to ingest —
//! `domain::elevated::enumeration_scope`, the nil-tenant ticker enumeration
//! `domain::service::tenants` runs before any of the three tickers, ingest
//! included, can bind to a tenant.)
//!
//! # The status vocabulary lives here, and nothing on this path counts
//!
//! [`classify`] and [`classify_all`] are the port of legacy's five-counter rule
//! (Task 14). They are pure functions over `&str` — no repository, no `async` —
//! and **the projection does not call them**: it stores the runner's word
//! verbatim, exactly as legacy does, and every aggregate is a `COUNT(*) FILTER`
//! in the query that needs it. They live here because ingest is where the
//! status vocabulary enters the gear; the first caller is Task 18.
//!
//! **Legacy has seven status classifications, not one, and they disagree.**
//! Recorded here so a later task does not re-derive one of them from this one.
//! This table is the gear's index of them and every Phase B task is pointed at
//! it, so it has been wrong about its own size three times:
//!
//! * **It said "four" through Task 19 and the fifth row was missing** — the
//!   daily fold, which Task 18's Step 0 measured and recorded on
//!   [`crate::domain::service::dashboard`] without carrying it back here. Added
//!   in Phase A's whole-phase fix wave.
//! * **It said "five" through Task 21a, and the KPI row below is the sixth.**
//!   Plan ruling R5 and this header both classed the dashboard's 24-hour KPI
//!   query as a *row-inclusion* rule only, and the paragraph under the table said
//!   so in as many words. Task 21b's Step 0 falsified that: the query's
//!   denominator is `status IN ('PASSED','FAILED','ERROR')`
//!   (`dashboard.rs:329`), a partition no other row here expresses —
//!   `classify`'s total is every row and `bucketize_status` has no denominator at
//!   all. It is a row filter *and* a status classification, and the retracted
//!   sentence is marked below rather than deleted.
//!
//!   **That row is used four times in legacy, not twice.** Task 21b first wrote
//!   here that the partition "exists nowhere else in legacy", which is false and
//!   was corrected in its fix round. All four uses are inside `api_dashboard`:
//!   the KPI pair at `dashboard.rs:329` and `:337`, the **flaky** fold at `:388`,
//!   and the **quality-vector** fold at `:487`. The last two are
//!   `DashboardStats::flaky_tests` and `DashboardStats::quality_vectors_pass_rate`
//!   — **Task 23b's and Task 25a's**, not Task 23's, which shipped the analytics
//!   folds of those names at a different grain. Neither derives a seventh rule:
//!   each **inherits this row**, and inherits the *row-inclusion* rule with it,
//!   because `:391` and `:490` window on
//!   `COALESCE(rr.finished_at, rr.created_at)` with no phase predicate too, over
//!   seven days instead of twenty-four hours. Three pieces were already ported
//!   when this was written, and only one of them was parameterised:
//!
//!   * [`crate::domain::service::dashboard`]'s `kpi_of` — the fold. Takes the
//!     status groups and nothing else; the denominator rule is the whole of it.
//!   * `infra::storage::results_sea_repo`'s `kpi_window(from, to)` — the
//!     predicate, and **the only one that takes a window**. A seven-day fold
//!     passes a different `from`, so reuse is an argument and not a fork.
//!   * the same module's `effective_ts()` — the sort expression. Takes **no
//!     parameters** and returns the same `COALESCE` whatever the window is;
//!     nothing about a seven-day fold changes it.
//!
//! * **It said "six" through Task 23b, and the snapshot row below is the
//!   seventh.** `latest_per_test_snapshot` (`analytics.rs:1633-1639`) — the fold
//!   behind the build distribution and the build-tests drill-down — maps `PASSED`
//!   to `PASSED`, `FAILED` and `ERROR` to `FAILED`, `SKIPPED` to `SKIPPED`, and
//!   **passes everything else through verbatim**. Every Phase B task before Task
//!   24 argued from this table and none of them needed it, which is exactly how
//!   it stayed missing: it is the only rule here that keeps `SKIPPED` *and*
//!   declines to bucket an unknown status, so a reader looking for "the analytics
//!   status rule" finds `bucketize_status` and gets a `NOT_RUN` where the runner
//!   said `XFAIL`. Task 24's Step 0 measured it and added the row.
//!
//!   **Task 23b took the second and not the first**, which is the part of this
//!   instruction that turned out to be wrong. `kpi_window(now - 7 days, None)` is
//!   exactly what
//!   [`crate::domain::repos::ResultsRepository::flaky_groups`] filters on, so the
//!   reuse held. [`crate::domain::service::dashboard`]'s `kpi_of` did **not**:
//!   that fold takes `(status, rows)` groups over a whole window and returns one
//!   pair of numbers, where the flaky query needs the same partition *per group*
//!   with a `HAVING` on both sides and a `LIMIT` after it — so the partition goes
//!   into SQL as [`PASSED_STATUSES`] and [`FAILED_STATUSES`], which is the same
//!   *rule* expressed where the reduction happens rather than a seventh rule.
//!
//!   **Task 25a took `:487` the same way, and the instruction held.**
//!   [`crate::domain::repos::ResultsRepository::file_status_counts`] is
//!   `kpi_window(now - 7 days, None)` plus the two constants, grouped by
//!   `test_file`, and its domain share is `dashboard::quality_vector_pass_rates`.
//!   Same reason as the flaky read and one clause weaker: this one needs the
//!   partition per group but no `HAVING` and no `LIMIT`, so what could not be
//!   folded after the fact was the partition alone. Neither of the two derived a
//!   seventh rule, which is what this paragraph predicted.
//!
//!   (`kpi_ts` was named here until Ruling C made it identical to `effective_ts`
//!   and deleted it, and the sentence that replaced the dangling name then
//!   claimed both functions took a window — which `effective_ts` does not. None
//!   of these is an intra-doc link, so nothing mechanical caught either error.
//!   This paragraph is an instruction to another task, so it is worth re-reading
//!   against the signatures whenever any of the three moves.)
//!
//! | Function | Rule | Ported by |
//! |---|---|---|
//! | the five counters (`plans.rs:188-192`) | `PASSED` / `FAILED`+`ERROR` / `SKIPPED` / `PENDING`+`RUNNING` / every row | [`classify`], here |
//! | the daily trend's fold (`dashboard.rs:218-219`) | **two counters only**: `PASSED` and `FAILED`+`ERROR`. There is no total and no skipped counter, so a `SKIPPED` row moves neither | `service::dashboard::daily_points`, Task 18 |
//! | `bucketize_status` (`analytics.rs:1940-1946`) | `PASSED`, `FAILED`+`ERROR`, **everything else `NOT_RUN`** | Task 20's universe core |
//! | `effective_case_status` (`analytics.rs:1270-1281`) | severity pick over `FAILED > ERROR > XPASS > XFAIL > SKIPPED > PASSED`, `ERROR` rendered as `FAILED` | Task 21 |
//! | `build_status_rank` (`analytics.rs:1948-1955`) | sort order `FAILED < PASSED < SKIPPED < other` | [`crate::domain::analytics::aggregates::build_status_rank`], Task 24 |
//! | the `PASSED`+`FAILED`+`ERROR` denominator (`dashboard.rs:329`, `:337`, `:388`, `:487`) | `PASSED` / `FAILED`+`ERROR`, over a denominator of `PASSED`+`FAILED`+`ERROR`. **Everything else is in no counter, the denominator included** | `service::dashboard::kpi_of` — Task 21b for the 24-hour pair (`:329`, `:337`); **Task 23b** for the flaky fold (`:388`) and **Task 25a** for the quality-vector fold (`:487`), both of them [`PASSED_STATUSES`] and [`FAILED_STATUSES`] in SQL rather than `kpi_of` |
//! | `latest_per_test_snapshot`'s mapping (`analytics.rs:1633-1639`) | `PASSED` / `FAILED`+`ERROR` / `SKIPPED` / **everything else passed through verbatim**. The only rule here with no catch-all bucket at all | [`crate::domain::analytics::aggregates::latest_per_test_snapshot`], Task 24 |
//!
//! Note what the second and sixth rows mean for a Phase B reader: **one legacy
//! endpoint, `api_dashboard`, contains three of these seven** — its per-run
//! counters are the five-counter rule minus the in-progress bucket, its daily
//! trend is the two-counter fold, and its 24-hour KPI query is the sixth row.
//! The analytics *overview* endpoint contains three more — `bucketize_status`
//! (`:761`), `effective_case_status` (`:1369`) and the seventh row, through the
//! build distribution at `:782` — and `api_build_tests` adds the fifth on top of
//! the seventh (`:419`, then `:444`).
//! [`crate::domain::analytics::aggregates`]' header tabulates the four that meet
//! in that file.
//! That same endpoint also holds three *row-inclusion* rules, which are a
//! separate axis: the per-run counters window on nothing, the daily trend on
//! `DATE(COALESCE(finished_at, created_at))` with `phase IN
//! ('Succeeded','Failed')`, and the KPI query on `COALESCE(finished_at,
//! created_at)` with **no phase predicate at all**.
//! [`crate::domain::repos::ResultsRepository::effective_status_counts`]
//! tabulates that axis.
//!
//! **Retracted 2026-08-21 (Task 21b's Step 0), and left visible because R5 still
//! cites it**: this paragraph read *"A third row-inclusion rule inside the same
//! endpoint (the 24-hour KPI window, which carries no phase restriction at all)
//! is Task 21's and is recorded on the plan's carried item 13 rather than here,
//! because it is a row filter rather than a status classification."* The last
//! clause is false — see the sixth row and the second bullet above. The rule is
//! both, and the half that is a classification belongs in this table.
//!
//! **The third row is the trap** — it was the second before the daily fold was
//! inserted above it, and the sentence is renumbered rather than left to point at
//! the wrong row. Under `bucketize_status` a `SKIPPED` file is `NOT_RUN`, while
//! under [`classify`] it is [`StatusBucket::Skipped`]. Both are correct for their
//! own surface. Reusing [`classify`] for the analytics universe would move every
//! skipped test out of `NOT_RUN` and change numbers the UI already renders.
//!
//! **And the seventh row is the trap's twin**, because it agrees with
//! [`classify`] about `SKIPPED` and with nothing about the rest: it has no
//! `PENDING`/`RUNNING` bucket, no total, and no catch-all — an `XFAIL` stays
//! `XFAIL` and is then counted by *neither* the build distribution's counters nor
//! its `executed_total`. Three rows of this table therefore start
//! `PASSED / FAILED+ERROR / …` and diverge only in the tail, which is the whole
//! reason ruling R5 forbids reusing one for another's surface.

use std::sync::Arc;

use qa_runs_sdk::{Run, RunTarget, RunTestResult};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::RunsReader;
use crate::domain::repos::{NewTestCaseResult, NewTestResult, ResultsRepository};
use crate::domain::system_actor::{SystemActorSite, TenantBound};

/// Projects qa-runs' lifecycle events into `qa_test_results` and
/// `qa_test_case_results`.
///
/// Generic over the repository rather than holding a `dyn` one:
/// [`ResultsRepository`]'s methods are generic over the
/// [`DBRunner`](toolkit_db::secure::DBRunner) they run on — which is what lets a
/// caller supply a transaction — and a trait with a generic method is not
/// object-safe. The [`RunsReader`] has no such constraint and is a `dyn`, so
/// the adapter can be swapped at composition time.
pub struct IngestService<R> {
    results: R,
    runs: Arc<dyn RunsReader>,
}

/// One run's projected result rows, read from qa-runs and folded by
/// [`project_rows`], ready for [`IngestService::write_run_projection`] to
/// write.
///
/// Opaque outside this module on purpose. The two fields are exactly
/// [`project_rows`]'s return value; [`ReconcileService`](crate::domain::service::reconcile::ReconcileService)'s
/// `reproject` is the one caller outside this module, and it only ever
/// carries this value from [`IngestService::read_run_projection`] to
/// [`IngestService::write_run_projection`] without inspecting it — see
/// [`IngestService::read_run_projection`]'s "Split in Task 5a's fix wave"
/// section for why it has to carry it across a gap that did not used to
/// exist.
pub struct RunProjection {
    files: Vec<NewTestResult>,
    cases: Vec<NewTestCaseResult>,
}

impl<R: ResultsRepository> IngestService<R> {
    #[must_use]
    pub const fn new(results: R, runs: Arc<dyn RunsReader>) -> Self {
        Self { results, runs }
    }

    /// Re-read one run from qa-runs and fold it into the rows a later
    /// [`Self::write_run_projection`] will write — the two cross-gear reads
    /// and the pure fold, with **no** database runner.
    ///
    /// **This function opens no transaction of its own — that is not the same
    /// claim as "cannot trip the guard."** Whether calling it trips
    /// `toolkit_db`'s transaction-bypass guard depends on whether the *caller*
    /// already has a transaction open on this task when it calls in, because
    /// the guard is task-local (see the mechanism below). For
    /// [`ReconcileService::reproject`](crate::domain::service::reconcile::ReconcileService::reproject)
    /// that is never true — it calls this function *before* opening one — so
    /// the call is safe there, and it is the only caller left: a second one,
    /// a transactional broker consumer that called both this and
    /// [`Self::write_run_projection`] under one transaction, was deleted along
    /// with the event-broker dependency it needed (`crate::gear`'s header).
    /// That caller's own risk from this mechanism is retired with it.
    ///
    /// # Split out of one unified function in Task 5a's fix wave, and why
    ///
    /// Through Task 40 the reads and the write below were one function —
    /// [`ReconcileService::reproject`](crate::domain::service::reconcile::ReconcileService::reproject)
    /// called it *inside its own transaction*. The mechanism that made this
    /// fail for the sweep and the rebuild is a property specific to this stack
    /// rather than to this function: in a single-binary deployment the qa-runs
    /// client `ClientHub` resolves is qa-runs' **own in-process service**, and
    /// its `get`/`test_results` (`qa-runs/src/domain/service/runs.rs:321`,
    /// `:611-625`) call **qa-runs' own** `db.conn()` before they ever reach a
    /// repository. `toolkit_db`'s transaction-bypass guard is task-local
    /// rather than instance-local — set for the duration of *any* `Db`
    /// instance's transaction closure and checked by *every* `Db` instance's
    /// `conn()` on the same task (`libs/toolkit-db/src/secure/db.rs:10-21`,
    /// deliberate defence-in-depth against a captured `Arc<AppServices>`
    /// bypassing a transaction) — so qa-runs' nested `conn()` failed with
    /// `DbError::ConnRequestedInsideTx` every time this gear's own
    /// transaction was already open around it: every sweep, every rebuild,
    /// deterministically.
    ///
    /// This function and [`Self::write_run_projection`] are the split that
    /// lets `ReconcileService::reproject` read *before* it opens a
    /// transaction and open one around only the write — see that function's
    /// own doc for the race the split introduces and why it does not matter
    /// here. The unified, unsplit shape existed for a second caller that
    /// needed the reads and the write under one transaction of its own; that
    /// caller is gone and the split is now unconditional rather than one of
    /// two shapes.
    ///
    /// # `site` names the audit identity of the two reads below
    ///
    /// The qa-runs re-read runs under a system actor, and **which** system
    /// actor is the caller's to say. Through Task 19 this function minted
    /// `system_actor::for_event_ingest` unconditionally, so a reconcile sweep and
    /// an operator rebuild both emitted `site = "event_ingest"` — defeating the
    /// one purpose [`crate::domain::system_actor`] exists for, whose header now
    /// records the correction and its three named sites.
    ///
    /// The parameter is a [`SystemActorSite`] rather than a built
    /// `SecurityContext` on purpose: minting here from the same [`TenantBound`]
    /// the projection is written under keeps "the tenant we read as" and "the
    /// tenant we write to" the same value by construction *when a caller mints
    /// one [`TenantBound`] and passes it to both* [`Self::read_run_projection`]
    /// and [`Self::write_run_projection`] — which every current caller does,
    /// because both halves are always called from the same outer function
    /// over the same local variable, but the type system does not prove it.
    ///
    /// # A run that has vanished is `Ok(None)`, not an error
    ///
    /// If qa-runs answers `not_found` — the run was deleted, or is not visible
    /// to the system actor — this logs and returns `Ok(None)`: nothing to
    /// project, not a failure. See each caller's own doc for what it does
    /// with that.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] for a transport failure reaching qa-runs —
    /// which is also what a tripped transaction-bypass guard becomes, since
    /// qa-runs' own `db.conn()` failure surfaces to this gear as an opaque
    /// far-side error (see this function's own "Split..." section above).
    /// This function opens no transaction of its own, so it has nothing of
    /// its own to roll back; whether the *caller* is holding one open — and
    /// therefore rolls back on this error — is the caller's concern, covered
    /// by its own doc ([`ReconcileService::reproject`] never is at this
    /// point).
    pub async fn read_run_projection(
        &self,
        site: SystemActorSite,
        tenant: TenantBound,
        run_id: Uuid,
    ) -> Result<Option<RunProjection>, DomainError> {
        let ctx = site.context(tenant);

        let run = match self.runs.get_run(&ctx, run_id).await {
            Ok(run) => run,
            Err(DomainError::RunNotIngested { .. }) => {
                warn!(
                    %run_id,
                    tenant_id = %tenant.get(),
                    "qa-runs reports no such run; nothing to project, advancing past the event",
                );
                return Ok(None);
            }
            Err(other) => return Err(other),
        };

        let rows = self.runs.list_run_test_results(&ctx, run_id).await?;
        let (files, cases) = project_rows(&run, rows);

        debug!(
            %run_id,
            tenant_id = %tenant.get(),
            file_rows = files.len(),
            case_rows = cases.len(),
            "replacing a run's result projection",
        );

        Ok(Some(RunProjection { files, cases }))
    }

    /// Write one run's already-projected rows —
    /// [`ResultsRepository::upsert_run_results`]'s delete-then-insert, and the
    /// only part of a reprojection that needs a database runner.
    ///
    /// # `scope` is the caller's, and it did not used to be
    ///
    /// Through Task 15 this projection write built `AccessScope::for_tenant(tenant)`
    /// itself. That was right for its background callers then — a ticker or an
    /// envelope taking their tenant with no request-scoped caller in sight —
    /// and **wrong for Task 16's rebuild**, which has a real request-scoped
    /// caller whose scope comes from a `PolicyEnforcer` decision and may be
    /// *narrower* than the whole tenant. Every background caller now passes
    /// `AccessScope::for_tenant(tenant.get())` explicitly, which is the same
    /// value it was getting.
    ///
    /// What the parameter buys is that a caller has to *say* which scope it is
    /// using instead of silently inheriting a whole-tenant one — see this
    /// module's "Scoping" section for why that distinction is the one that
    /// matters here.
    ///
    /// ## What actually fails closed, corrected
    ///
    /// This paragraph first read: *"this is about honesty at the call site, not
    /// about adding a check: `upsert_run_results` already fails closed through
    /// `validate_tenant_in_scope` if `scope` and `tenant` disagree."* That is
    /// true of **`owner_tenant_id` and of nothing else**, and Task 16's review
    /// measured what the gap costs.
    ///
    /// `validate_tenant_in_scope` (`libs/toolkit-db/src/secure/db_ops.rs:281-298`)
    /// checks one property. The repository's two `DELETE`s are filtered with the
    /// **full** scope while its two inserts go through `scope_unchecked`, a
    /// documented no-op (`db_ops.rs:376-385`). So a scope carrying any further
    /// predicate makes the delete match less than the insert writes, and a
    /// replay duplicates rows rather than replacing them — on precisely the
    /// narrower request-scoped input this parameter was added to permit.
    ///
    /// **The check that closes it is the caller's**, and there is one:
    /// [`refuse_scope_beyond_tenant`](crate::domain::service::refuse_scope_beyond_tenant),
    /// called by `ReconcileService::rebuild` before anything is read. The full
    /// measurement is recorded there and on
    /// [`ResultsRepository::upsert_run_results`](crate::domain::repos::ResultsRepository::upsert_run_results).
    /// This function does not call it itself: its two background callers satisfy
    /// it by construction, and putting it here would run a PDP-shaped check on
    /// every event of a 500-test run to no effect.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] from the repository write. The caller's own
    /// transaction is what rolls back on it; this function opens none of its
    /// own.
    pub async fn write_run_projection<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant: TenantBound,
        run_id: Uuid,
        projection: RunProjection,
    ) -> Result<(), DomainError> {
        self.results
            .upsert_run_results(
                runner,
                scope,
                tenant.get(),
                run_id,
                projection.files,
                projection.cases,
            )
            .await
    }

    // `reproject_run` — `read_run_projection` followed by `write_run_projection`
    // composed under one caller-supplied `runner` — lived here through Task 40
    // for a transactional broker consumer that needed both under its own
    // transaction, committing an offset in the same breath as the projection.
    // No deployment ever registered the `EventBrokerApi` client that consumer
    // needed, so it never ran; it was deleted along with the consumer and the
    // event-broker dependency (`crate::gear`'s header, "Event ingest, and why
    // there is only one path"). `ReconcileService::reproject` composes the
    // same two calls under its own transaction and is the only caller left —
    // see `read_run_projection`'s own doc for why the two stayed split rather
    // than being recomposed into one function again.
}

/// Split one run's per-test rows into the two tables they land in.
///
/// A free function, and pure: it is the whole of the mapping, so it can be
/// tested without a repository, a port or a database.
///
/// # `nodeid` is the discriminator, and it is a weak one
///
/// After qa-runs' Task 2 migration, a file-level row and a case-level row are
/// indistinguishable in `qa_run_test_results` — both can carry `nodeid = ''`.
/// Legacy got the distinction for free from table identity (`test_results` vs
/// `test_case_results`); this schema does not. **The rule is: a non-empty
/// `nodeid` means the row is also a case-level result.** It is a convention no
/// constraint enforces, which is why it is written here, at the one place that
/// applies it.
///
/// A row with a non-empty `nodeid` produces **both** a file-level and a
/// case-level row, not one or the other. Legacy's parser writes both: the
/// per-file outcome feeds `test_results` and each `TEST_CASE` marker feeds
/// `test_case_results` (`manager/src/services/argo.rs:2593-2615` and
/// `:2963-2975`), and the file-level counters the dashboard renders are
/// computed over the former. Dropping the file-level row for a case-carrying
/// result would silently empty every count for a runner that reports node ids.
///
/// # The order of `files` is the ingest ordinal
///
/// `upsert_run_results` derives `qa_test_results.ingest_ordinal` from each
/// row's position in this `Vec`, and that ordinal is the within-run half of the
/// latest-wins tiebreak. This function preserves the producer's order and must
/// keep doing so: sorting, deduplicating or partitioning `rows` here would
/// invert the tiebreak with nothing failing.
fn project_rows(
    run: &Run,
    rows: Vec<RunTestResult>,
) -> (Vec<NewTestResult>, Vec<NewTestCaseResult>) {
    let (repo_id, plan_path) = plan_identity(&run.target);

    let mut files = Vec::with_capacity(rows.len());
    let mut cases = Vec::new();

    for row in rows {
        if !row.nodeid.is_empty() {
            cases.push(NewTestCaseResult {
                test_file: row.test_file.clone(),
                nodeid: row.nodeid.clone(),
                // The case-level table spells the test function `name`, not
                // `test_name`; the value is the same one.
                name: row.test_name.clone(),
                status: row.status.clone(),
                duration: row.duration.clone(),
                reason: row.reason.clone(),
                ticket: row.ticket.clone(),
            });
        }

        files.push(NewTestResult {
            test_file: row.test_file,
            test_name: row.test_name,
            status: row.status,
            duration: row.duration,
            launch_id: row.launch_id,
            jira_key: row.jira_key,
            // The analytics *filter*: legacy's `WHERE r.app_version = $1`.
            product_version: run.app_version.clone(),
            // The analytics *projection*: what the build distribution groups
            // by. Carried as `None` when the run named no build, so "no build
            // reported" stays distinguishable from a run that literally
            // reported the string `unknown` — Task 24 owns that fallback.
            app_build: run.app_build.clone(),
            environment_id: run.environment_id,
            repo_id,
            plan_path: plan_path.clone(),
            // Legacy's analytics branch filter reads `source_ref` and falls
            // back to `test_version` *at query time*
            // (`manager/src/routes/analytics.rs:967-970`,
            // `COALESCE(NULLIF(r.source_ref,''), NULLIF(r.test_version,''))`),
            // because both columns sit on the row it is already selecting.
            //
            // **Here the fallback is applied once, on write — and by the time it
            // reaches this line there is nothing left to fall back from.**
            // `qa_runs_sdk::Run` has exactly one branch field: `test_version`,
            // documented as "Branch actually resolved and executed against"
            // (`qa-runs-sdk/src/models.rs:297-300`), and `source_ref` does not
            // exist anywhere in qa-runs — that gear collapsed the pair at launch
            // and records the reasoning at
            // `qa-runs/src/domain/service/runs.rs:1047-1050` ("this port records
            // one branch label in one column, so there is no fallback to
            // write"). So this assignment *is* the resolved value, and
            // `analytics::UniverseFilter::branch` is a plain equality rather
            // than a `COALESCE`.
            //
            // Two consequences, stated because neither is fixable downstream.
            // A row written while this resolution was wrong **cannot be
            // corrected by a query change** — the analytics read has no second
            // column to consult — so it needs a reprojection: the operator
            // rebuild (`ReconcileService::rebuild`, Task 16) re-reads the run
            // from qa-runs and rewrites the row. And a run that recorded no
            // branch at all stores `None`, which no branch-filtered analytics
            // read can ever match; it is visible only in the unfiltered one,
            // where an absent filter means every branch.
            branch: run.test_version.clone(),
            // `None` while the run is still going. Legacy writes RUNNING and
            // PENDING rows too, and the dashboard's KPI window coalesces to
            // `run_created_at` for them.
            run_finished_at: run.finished_at,
            // The fallback half of legacy's
            // `COALESCE(rr.finished_at, rr.created_at)`. Non-optional on
            // `qa_runs_sdk::Run`, so it is always present here; the column is
            // nullable anyway, because a stored `None` is a better record of a
            // producer that stopped sending it than an epoch would be.
            //
            // **Not the same instant as the row's `created_at`**, which the
            // repository mints on every write — and this projection runs again
            // for the whole run on every result event, so that one moves while
            // this one does not. `qa_insights_sdk::TestResultRecord::run_created_at`
            // carries what windowing on the wrong one did.
            run_created_at: Some(run.created_at),
        });
    }

    (files, cases)
}

/// The `(repo_id, plan_path)` pair a run's target denormalizes to.
///
/// `None`/`None` for the two kinds with no plan identity — a custom plan spans
/// repositories and has no single one, and a collect run enumerates a
/// repository rather than executing a plan in it. That is the contract
/// `qa_insights_sdk::TestResultRecord::repo_id` states, and it is what keeps a
/// plan-scoped analytics filter from matching a collect run's rows.
///
/// **Not exhaustive by accident.** The `match` is exhaustive today and will
/// stop compiling if qa-runs adds a fifth kind, which is the intended
/// behaviour here: this reads a typed SDK value already fully in memory, so
/// a new variant is a decision this function's author must make rather than
/// one silently defaulted around. (An earlier revision contrasted this with
/// a transactional broker consumer's own wire decoder, for which the
/// opposite held — a new variant there must not park the partition. That
/// consumer, and the event path it decoded, were deleted as dead code, so
/// the contrast no longer has a live second half; the reasoning for this
/// function's own exhaustiveness stands on its own.)
pub(crate) fn plan_identity(target: &RunTarget) -> (Option<Uuid>, Option<String>) {
    match target {
        RunTarget::Plan { repo_id, path } | RunTarget::Test { repo_id, path, .. } => {
            (Some(*repo_id), Some(path.clone()))
        }
        RunTarget::CustomPlan { .. } | RunTarget::Collect { .. } => (None, None),
    }
}

// ---------------------------------------------------------------------------
// Status classification
// ---------------------------------------------------------------------------

/// Which of legacy's five counters a runner status moves.
///
/// Ported from `manager/src/routes/plans.rs:188-192`, which computes exactly
/// these five in SQL:
///
/// ```sql
/// COUNT(*) FILTER (WHERE tr.status = 'PASSED')                  AS passed
/// COUNT(*) FILTER (WHERE tr.status IN ('FAILED', 'ERROR'))      AS failed
/// COUNT(*) FILTER (WHERE tr.status = 'SKIPPED')                 AS skipped
/// COUNT(*) FILTER (WHERE tr.status IN ('PENDING', 'RUNNING'))   AS in_progress
/// COUNT(tr.id)                                                  AS total
/// ```
///
/// Re-verified against the source on 2026-08-20; the citation lands exactly.
///
/// # `Uncounted` is a real bucket, not a fallthrough
///
/// `total` is `COUNT(tr.id)` over **every** row, so it is not the sum of the
/// other four, and the difference is not rounding — it is `XFAIL`, `XPASS`, and
/// any status a future runner invents. Legacy counts `XFAIL` and `XPASS`
/// separately wherever it cares (`manager/src/routes/analytics.rs:1359-1360`,
/// `"XFAIL" => xfail += 1,` / `"XPASS" => xpass += 1,`) and **never folds them
/// into `passed`**. Folding them would inflate every pass rate the dashboard
/// renders — an `XPASS` is a test that was expected to fail and did not, which
/// is a result to look at, not a success.
///
/// Naming the bucket forces a caller's `match` to say what it does with those
/// rows instead of letting them fall off the end of an `if`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusBucket {
    Passed,
    /// `FAILED` **and** `ERROR`. Legacy keeps the two apart as stored statuses
    /// and folds them together in every aggregate that produces these numbers,
    /// which is why `qa_runs_sdk::RunResult` has five fields and not six.
    Failed,
    Skipped,
    /// `PENDING` and `RUNNING` — a row written before the run reached its
    /// verdict. Legacy writes those rows (`manager/src/services/argo.rs:2603`),
    /// so a projection that dropped them would empty the in-progress column.
    InProgress,
    /// In `total` and in no other counter: `XFAIL`, `XPASS`, and anything a
    /// runner starts emitting after this was written.
    Uncounted,
}

/// The five counters over one set of statuses.
///
/// `usize`, matching `qa_runs_sdk::RunResult` and the wire `CountsPayload`, so
/// the three can be compared without casts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResultCounts {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub in_progress: usize,
    /// Every row, whatever its status. **Not** the sum of the four above.
    pub total: usize,
}

/// Classify one runner status.
///
/// # Exact match, no trimming and no re-casing, and that is legacy's behaviour
///
/// Legacy matches the stored string exactly — `match status { "PASSED" => ... }`
/// against a value SQL handed back. Two things follow, and both are deliberate:
///
/// * A lowercase `passed` is [`StatusBucket::Uncounted`] here, exactly as it is
///   in legacy. Normalising it would be a *divergence*, and a silent one: the
///   row would start moving a counter it has never moved.
/// * The producer already trims. `qa-runs`' `normalize_status` trims and
///   deliberately does not re-case, so by the time a status reaches this gear
///   the whitespace question is answered upstream. Trimming again here would
///   hide a producer-side regression rather than fix one.
///
/// The status column is an **open set** — legacy writes unvalidated runner text
/// at `manager/src/routes/runs.rs:1115` — so this function must never fail
/// closed on an unfamiliar value. A ninth status is a runner change; refusing
/// it would drop a real result.
#[must_use]
pub fn classify(status: &str) -> StatusBucket {
    match status {
        // Not the literal `"PASSED"`, which is what this arm read until Task 23b.
        // [`PASSED_STATUSES`] needs the same set as *data*, for the second read
        // that must put a status predicate in SQL, and that constant's doc carries
        // the argument — identical in shape to [`FAILED_STATUSES`]' below.
        s if PASSED_STATUSES.contains(&s) => StatusBucket::Passed,
        // Not `"FAILED" | "ERROR"`, which is what this arm read until Task 21b's
        // fix round. [`FAILED_STATUSES`] needs the same set as *data*, for the one
        // read that must put the predicate in SQL, and a second literal spelling
        // is a second thing to keep in step. Defining the arm *from* the constant
        // makes them one definition, so the drift cannot happen rather than being
        // tested for — the test that claimed to catch it could not, and
        // `the_failed_status_set_is_legacys_pair_and_classify_is_defined_from_it`
        // now records what it does instead.
        //
        // Placed after the passed arm deliberately: an earlier arm wins, so the
        // ordering means this constant defines the *failure* bucket and cannot
        // silently capture a status named above it. Nothing is above it but the
        // passed set, and the whole vocabulary's mapping is pinned by
        // `the_whole_status_vocabulary_maps_the_way_the_legacy_sql_filters_do`.
        //
        // **This read "a literal arm wins over a later guard" until Task 23b**,
        // which is a mechanism that no longer exists here — the arm above is now a
        // guard too. The conclusion is unchanged and the two sets are disjoint by
        // `the_two_counted_status_sets_are_disjoint_so_the_denominator_is_their_sum`,
        // so nothing rests on the order; it is kept because a future widening of
        // either constant would make it rest on the order again.
        s if FAILED_STATUSES.contains(&s) => StatusBucket::Failed,
        "SKIPPED" => StatusBucket::Skipped,
        "PENDING" | "RUNNING" => StatusBucket::InProgress,
        _ => StatusBucket::Uncounted,
    }
}

/// The statuses [`classify`] puts in [`StatusBucket::Passed`], spelled as data.
///
/// # A second read had to put a status predicate in SQL, so the pass set follows
/// # the failure set out of this module
///
/// [`FAILED_STATUSES`] below carries the whole argument for why a vocabulary
/// leaves here at all, and this constant exists for the same reason one method
/// later:
/// [`crate::domain::repos::ResultsRepository::flaky_groups`] is legacy's
/// `HAVING COUNT(*) FILTER (WHERE tr.status = 'PASSED') > 0 AND … > 0 … LIMIT 10`
/// (`manager/src/routes/dashboard.rs:386-399`), and a `HAVING` plus a `LIMIT`
/// over *two* status partitions cannot be applied after a domain-side fold
/// without reading every group in the window first — the row set the aggregate
/// exists not to materialise.
///
/// **It is not a second spelling of the rule — it *is* the rule.** [`classify`]'s
/// `Passed` arm is `s if PASSED_STATUSES.contains(&s)`, exactly as its `Failed`
/// arm is defined from [`FAILED_STATUSES`], so the drift that would leave a row
/// counted by `pass_rate_24h` and missing from the flaky query's numerator is not
/// expressible. Task 21b arrived at that shape for the failure set after shipping
/// two literals and a test that could not catch the drift it named; this one is
/// written that way from the start rather than repeating the round.
///
/// **One element, and the singleton is the point rather than an accident.**
/// Legacy's numerator is `tr.status = 'PASSED'` (`dashboard.rs:386`), an equality
/// and not an `IN`, so the set has one member; a slice rather than a `&str`
/// because the read that consumes it takes both partitions the same way and
/// because widening it — a runner that starts reporting `XPASS` as a pass, say —
/// must be one edit here rather than a signature change. Widening it widens every
/// pass counter in the gear at once, which is the same intended coupling
/// [`FAILED_STATUSES`] documents.
///
/// # The denominator is not a third constant
///
/// Legacy's flaky and quality-vector folds count over `('PASSED', 'FAILED',
/// 'ERROR')` (`dashboard.rs:388`, `:487`) — ruling R5's sixth classification,
/// tabulated in this module's header. That is exactly this set unioned with
/// [`FAILED_STATUSES`], and `flaky_groups` derives it that way rather than
/// spelling a third literal that could disagree with the two above it.
pub const PASSED_STATUSES: [&str; 1] = ["PASSED"];

/// The statuses [`classify`] puts in [`StatusBucket::Failed`], spelled as data.
///
/// # Why the vocabulary leaves this module at all
///
/// Every other consumer of the failure rule *folds* rows that have already been
/// read, so [`classify`] is enough and the repositories hold no status words —
/// `infra::storage::results_sea_repo::grouped_status_counts` records that as a
/// property. **Two reads cannot work that way** — this said "one read" until Task
/// 23b added the second, which is [`PASSED_STATUSES`]' whole reason for existing:
///
/// * [`crate::domain::repos::ResultsRepository::recent_failures`] is legacy's
///   `WHERE tr.status IN ('FAILED', 'ERROR') … LIMIT 10`
///   (`manager/src/routes/dashboard.rs:275-278`), and a `LIMIT` over failures
///   cannot be applied after a domain-side fold without reading every row in the
///   window first — which is the row set the aggregate exists not to materialise.
/// * [`crate::domain::repos::ResultsRepository::flaky_groups`] is the same
///   argument over *groups* rather than rows, with a `HAVING` as well as a
///   `LIMIT` (`dashboard.rs:393-399`).
///
/// So the predicate goes into SQL, and this constant is what the caller passes
/// so that the *vocabulary* still has one home.
///
/// **It is not a second spelling of the rule — it *is* the rule.** [`classify`]'s
/// `Failed` arm is `s if FAILED_STATUSES.contains(&s)`, so there is one
/// definition and the drift that would leave a failure counted by
/// `failed_24h_count` and missing from the card list beside it is not
/// expressible.
///
/// That is the fix round's shape and not the first one. Task 21b shipped the arm
/// as a literal `"FAILED" | "ERROR"` next to this constant, with a test asserting
/// the two agreed — and the review showed the test could not catch the drift its
/// own doc named, because it iterated a hardcoded vocabulary and a *new* status
/// (a runner starts emitting `CRASHED`) is by definition not in a hardcoded list.
/// Making the two one definition removes the failure mode instead of watching
/// for it.
///
/// **Widening this constant now widens every counter in the gear** — `classify`
/// and therefore the five counters, the dashboard's four, the daily trend, the
/// 24-hour KPI pair and the failure card list, together. That is the intended
/// coupling, and it is why the test above pins the set against its legacy
/// citation rather than against itself.
pub const FAILED_STATUSES: [&str; 2] = ["FAILED", "ERROR"];

/// Tally a set of statuses into the five counters.
///
/// # Still no production consumer, and Task 18 was not it
///
/// Nothing in the ingest path counts anything: the projection stores the
/// runner's word verbatim and every aggregate is computed on read, which is
/// what legacy does — its five counters are a `COUNT(*) FILTER` in the query
/// that needs them, not a stored column. This function is the port of that
/// rule, placed here by the plan because ingest is where the status vocabulary
/// enters the gear.
///
/// **This section forecast the dashboard (Task 18) as the first caller, and that
/// forecast was falsified.** Task 18 reuses [`classify`] directly and does not
/// call this: its counters are folded from
/// `domain::repos::RunStatusCount` groups, which are *weighted* — a
/// `(status, rows)` pair per `(run, status)` group — while this takes a slice of
/// statuses and counts one each. Feeding it a group would need the slice
/// expanded to one entry per row, which is the row set the aggregate exists to
/// avoid materialising. `domain::service::dashboard::Counters` is the weighted
/// equivalent, and it delegates the vocabulary to [`classify`] so there is still
/// exactly one fold.
///
/// So this remains a function whose only callers are the tests below
/// (`ingest_tests.rs:301`, `:315`, `:350`, `:380`), and they are what make it
/// more than a forecast. Task 21's summary counters are the next candidate;
/// **that is a candidate and not a claim**, which is the distinction the
/// sentence this replaced got wrong.
#[must_use]
pub fn classify_all<S: AsRef<str>>(statuses: &[S]) -> ResultCounts {
    let mut counts = ResultCounts::default();
    for status in statuses {
        counts.total += 1;
        match classify(status.as_ref()) {
            StatusBucket::Passed => counts.passed += 1,
            StatusBucket::Failed => counts.failed += 1,
            StatusBucket::Skipped => counts.skipped += 1,
            StatusBucket::InProgress => counts.in_progress += 1,
            StatusBucket::Uncounted => {}
        }
    }
    counts
}

#[cfg(test)]
#[path = "ingest_tests.rs"]
mod ingest_tests;
