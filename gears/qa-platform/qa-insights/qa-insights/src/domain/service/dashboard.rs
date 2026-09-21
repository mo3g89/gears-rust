//! The dashboard aggregates — `GET /qa/v1/dashboard` and
//! `GET /qa/v1/dashboard/coverage`.
//!
//! The port of legacy's `api_dashboard` (`manager/src/routes/dashboard.rs:84`),
//! and the **first aggregate read in this gear**: everything before it either
//! wrote rows or paged them out again. Tasks 19 and 20-27 add more aggregates
//! beside it, so the shape below is deliberate rather than incidental.
//!
//! # Reads on one side, folds on the other
//!
//! One cross-gear listing gives live run state; grouped `SELECT`s plus a `COUNT`
//! plus one bounded row read give the ingested numbers; every number in the
//! response is then a pure function over those. That split is what makes the
//! rules ported here testable without a database — see [`resolve_days`],
//! [`is_active`], [`summarize_runs`], [`run_trend_points`], [`daily_points`],
//! [`kpi_of`] and [`failure_card`], each of which is one of legacy's rules and
//! has its own test.
//!
//! This section read "**Two** reads and a listing" through Task 21a, when the
//! reads were [`crate::domain::repos::ResultsRepository::run_status_counts`],
//! `run_status_counts_since` and `count_ingested_runs`. Task 21b's KPI pair adds
//! three more — two windowed status counts and the failure card list — so the
//! count is dropped from the heading rather than kept and corrected on every
//! task that adds one. `DashboardService`' own doc still records that none of
//! them needs a transaction, and that argument is unchanged by their number.
//!
//! ## The single listing is legacy's shape, and legacy says why
//!
//! The total, the active count, the queued count, the active list and the recent
//! list all come off **one** `RunsReader::list_recent_runs` page. Legacy derives
//! four of those five the same way and states the reason in a comment
//! (`manager/src/routes/dashboard.rs:143-146`):
//!
//! > Derive everything we need from the single `runs` listing — recent 10 for
//! > the trend chart, active/pending count and list, and the total. A second
//! > `list_runs_with_history` call (or a separate `list_workflows`) costs an
//! > extra kube round-trip and ~500ms of latency for no new information.
//!
//! Here the equivalent cost is a cross-gear SDK call rather than a kube
//! round-trip. In particular the queued count is **not** a `list_queue` call;
//! [`crate::domain::ports::RunsReader::list_recent_runs`] records that decision
//! in full.
//!
//! ## Live run state is read live, and is never read from `qa_test_results`
//!
//! `cpt-cf-qa-principle-db-first-state` makes qa-runs authoritative for a run's
//! state, and this gear's projection could not substitute for it even if it
//! wanted to: [`crate::domain::ports::RunsReader::list_runs_finished_since`]
//! never returns a run with a `NULL` `finished_at`, so the reconciler has **no**
//! view of an in-progress run at all — where legacy re-persisted live counts
//! every 30 seconds (`manager/src/services/run_results_poller.rs`). Nothing is
//! lost, because a run lands in full at `run.finished`; what it means is that the
//! active and queued numbers have exactly one honest source, and it is across the
//! gear boundary.
//!
//! # The `RunState` mapping, which is the one real translation here
//!
//! Legacy's active predicate is `run.phase == "Running" || run.phase ==
//! "Pending"` (`manager/src/routes/dashboard.rs:150` for the count, `:154` for
//! the list) over **Argo workflow phases**, and this architecture has no phase.
//! [`is_active`] carries the mapping and its own doc carries the citations; the
//! summary is `Pending → Dispatching`, `Running → Running`, and nothing else —
//! notably **not** `Queued`, which legacy also does not count.
//!
//! # The status fold reuses `ingest::classify`, and that was verified
//!
//! Plan ruling R5 requires this to be checked rather than assumed, because legacy
//! has **seven** disagreeing status classifications and
//! [`crate::domain::service::ingest`]'s header tabulates them. (R5 and that table
//! both said "four" until Phase A's whole-phase review: the fifth is the daily
//! fold three paragraphs down, which Task 18 measured here and did not carry back
//! to the index every Phase B task is pointed at. The **sixth** is the KPI
//! denominator this module's own `kpi_of` implements — Task 21b's finding, which
//! this paragraph did not carry back either until Task 22's fix round.) Checked,
//! row by row:
//!
//! | | dashboard (`dashboard.rs:170-173`) | five counters (`plans.rs:188-192`), i.e. [`classify`] |
//! |---|---|---|
//! | total | `COUNT(tr.id)` | `COUNT(tr.id)` |
//! | passed | `status = 'PASSED'` | `status = 'PASSED'` |
//! | failed | `status IN ('FAILED','ERROR')` | `status IN ('FAILED','ERROR')` |
//! | skipped | `status = 'SKIPPED'` | `status = 'SKIPPED'` |
//! | in progress | *no such counter* | `status IN ('PENDING','RUNNING')` |
//!
//! The two agree on every case they share; the dashboard simply has four counters
//! where the five-counter rule has five. So [`classify`] is reused and
//! [`Counters`] consumes four of its five buckets, folding `InProgress` in with
//! `Uncounted` — which is exactly what the dashboard's SQL does with a `PENDING`
//! row: it lands in `COUNT(tr.id)` and in none of the three filters.
//!
//! **The trap R5 names is a different function.** `bucketize_status`
//! (`manager/src/routes/analytics.rs:1940-1946`) buckets everything outside
//! `PASSED`/`FAILED`/`ERROR` to `NOT_RUN`, so a `SKIPPED` row is `NOT_RUN` there
//! and `StatusBucket::Skipped` here. That one belongs to Tasks 20/21/24 and must
//! not be unified with this.
//!
//! The **daily** trend has only two counters, not four (`:218-219`): `PASSED`,
//! and `IN ('FAILED','ERROR')`. A `SKIPPED` row moves neither. That is a third
//! reading of the same rows inside one legacy endpoint, and [`daily_points`]
//! reproduces it rather than projecting the four-counter fold onto two.
//!
//! # The 24-hour KPI is a fourth reading, and its *denominator* is the new part
//!
//! Task 21b, and checked under R5 exactly as the fold above was. Its two
//! counters are the daily trend's — `PASSED` alone and `FAILED` with `ERROR`
//! folded in — so nothing new there. Its **denominator** is
//! `status IN ('PASSED','FAILED','ERROR')` (`:329`), which is a partition no
//! other row of R5's table expresses:
//!
//! | | numerator | denominator |
//! |---|---|---|
//! | [`classify`]'s five-counter rule | `passed` | `total` = **every** row |
//! | the KPI rule (`:329-335`) | `passed` | `passed + failed` |
//!
//! So a `SKIPPED` row is in no KPI counter at all, where [`classify`] puts it in
//! `total`. [`kpi_of`] therefore reuses [`Counters`] for the vocabulary and reads
//! **two** of its four fields — `passed` and `failed`; `skipped` and
//! [`Counters::total`] are both unread, and `total` deliberately so. Reusing that
//! one would deflate every pass rate by however many tests a plan skips, on a
//! screen that renders either number without complaint. (This said "three of its
//! four" until Task 21b's fix round, which is one field's worth of wrong in the
//! direction that makes the omission look accidental.)
//!
//! **The partition is legacy's four times over, and all four are shipped here.**
//! This section said it "exists nowhere else in legacy"; it is at `:329` and
//! `:337` here, and at `:388` (flaky) and `:487` (quality vectors) — the two
//! folds behind `flaky_tests` and `quality_vectors_pass_rate`. Corrected in Task
//! 21b's fix round, and `crate::domain::service::ingest`'s R5 table carries the
//! consequence: **whichever task lands each of those two reads inherits this row
//! rather than deriving a seventh.** Task 23 was named for both and shipped
//! neither — it built the *analytics* folds of those names, a different grain and
//! a different partition. **Task 23b landed `:388` and Task 25a `:487`**, both of
//! them as [`PASSED_STATUSES`]/[`FAILED_STATUSES`] in SQL rather than through
//! [`kpi_of`], and neither derived a seventh rule.
//! (This sentence read "Task 23 reuses `kpi_of`" until Task 23's own fix round,
//! which is the same forward-looking-claim-about-unshipped-work defect that task
//! escalated one file over; it then said `quality_vectors_pass_rate` "is Task
//! 25's, together with the production `CatalogReader` adapter it cannot be
//! computed without", which was right about both and is now history rather than a
//! forecast.)
//!
//! **`flaky_tests` is shipped, by Task 23b, and it did *not* reuse [`kpi_of`]** —
//! which is the sharper reading of "inherits this row". `kpi_of` folds
//! `(status, rows)` groups over one window into one pair of numbers; legacy's
//! flaky query needs the same partition *per group*, with a `HAVING` on both
//! sides and a `LIMIT` after it (`:393-399`), so the partition goes into SQL
//! through [`PASSED_STATUSES`] and [`FAILED_STATUSES`] and the domain's share is
//! [`flaky_card`]. The `HAVING`, the `ORDER BY LEAST(passed, failed)` and the
//! `LIMIT 10` are pushed down with it rather than folded here, against this
//! module's usual direction;
//! [`crate::domain::repos::ResultsRepository::flaky_groups`] carries the ruling
//! and its two reasons. What is *not* forked is the vocabulary: `classify`'s two
//! arms are defined from the same two constants the SQL predicate is built from.
//!
//! **`quality_vectors_pass_rate` is shipped by Task 25a on the same reading, with
//! one clause fewer.**
//! [`crate::domain::repos::ResultsRepository::file_status_counts`] takes the same
//! two constants and groups by `test_file` (`:492`), and legacy attaches **no**
//! `HAVING` and **no** `LIMIT` to it — so what could not be folded after the fact
//! is the partition alone, and everything else stays in the domain as
//! [`quality_vector_pass_rates`]. The consequence a reader should carry: a file
//! that only ever passed is *absent* from the flaky read and *present* in this
//! one, because a vector whose files all pass is a full bar rather than a missing
//! one.
//!
//! It also carries **a third row-inclusion rule**, which is a separate axis from
//! the classification: no `phase` predicate at all and a window on
//! `COALESCE(finished_at, created_at)`, so it counts rows of runs still in
//! progress. Two consequences:
//!
//! * [`window_start`] is not the helper for it — that one drops a run with no
//!   finish instant, which is exactly the row this rule keeps. And
//!   [`crate::domain::repos::ResultsRepository::effective_status_counts`]
//!   tabulates all three rules against each other.
//! * **`created_at` is not the fallback column, and getting that wrong was a
//!   wrong rendered number.** Legacy's `rr.created_at` is the *run's* creation
//!   instant; `qa_test_results.created_at` is when the row was written, and
//!   ingest rewrites a run's rows on every result event. Task 21b shipped the
//!   second and its fix round added `run_created_at` for the first —
//!   `qa_insights_sdk::TestResultRecord::run_created_at` carries the column and
//!   `infra::storage::results_sea_repo`'s `effective_ts` the expression. (That
//!   was `kpi_ts` until Ruling C folded the two expressions back into one; the
//!   dangling name survived a round because it is not an intra-doc link.) Task
//!   23's two folds window on the same expression, so they inherit the column
//!   too, and the analytics reads share it as of Ruling C.
//!
//! # The clock is this process's, where legacy's is the database's
//!
//! Legacy evaluates these windows from `NOW()` **inside the statement**. `now()`
//! is the transaction's timestamp on Postgres, so its six `FILTER` clauses agree
//! with each other — but the card query (`:263-279`) is a *second* statement with
//! its own `NOW()`, so legacy's counter and its card list can straddle a
//! boundary. Here [`DashboardService::stats`] reads `OffsetDateTime::now_utc()`
//! once and *binds* the bounds, which is a substitution rather than a translation
//! and has two consequences worth naming:
//!
//! * **Clock skew between this process and the database now matters**, where in
//!   legacy it could not. A service clock ahead of the database's shifts every
//!   bound in the same direction, which is why the windows still partition
//!   correctly relative to each other — the argument below about a *producer*
//!   clock ahead of the database's is unchanged in substance, but the comparison
//!   is now producer-vs-this-process.
//! * **In exchange, every number in one payload shares one instant** — the three
//!   statements as well as the day axis, where legacy's two statements do not.
//!   The same argument [`DashboardService::stats`] makes for reading `today`
//!   once, and the reason the two windows here are guaranteed to partition
//!   rather than merely likely to.
//!
//! # `actions::LIST`, and why `view_dashboard` was rejected
//!
//! The decision is Task 18's to make, and it is: **reuse [`actions::LIST`]** on
//! [`resources::TEST_RESULT`]. Not because `list` is convenient, but because a
//! second action would let a policy give this aggregate a *different* row set
//! from `GET /qa/v1/test-results`, and an aggregate computed over rows the
//! subject may not list is an inference channel that nothing in this crate would
//! notice. Under one action the two are the same rows by construction: both apply
//! the whole compiled scope through `.secure().scope_with(..)`, so a policy that
//! narrows a subject to particular rows narrows the dashboard's numbers exactly
//! as it narrows the collection's page.
//!
//! It also matches the siblings, which is the convention `mod actions`' header
//! sets: qa-runs and qa-catalog split their read actions by *row addressing* —
//! `get` for a single row, `list` for an enumeration
//! (`qa-runs/src/domain/service/mod.rs:336-338`) — and not by view. A dashboard
//! addresses no row.
//!
//! **The rejected alternative, recorded because it is genuinely arguable.** A
//! `view_dashboard` action would let a deployment grant summary-only access —
//! aggregate counts without the per-test rows behind them — which is a real and
//! useful narrowing `list` cannot express. It was still rejected: that narrowing
//! is only sound if the dashboard's compiled scope is guaranteed no wider than
//! the collection's, nothing in this repository evaluates a policy that could
//! guarantee it, and the failure would be silent. If a deployment ever needs
//! summary-only access, the honest way to add it is a `view_dashboard` action
//! *plus* a check that its scope is no wider — not the action alone.
//!
//! # This is a read, so `refuse_scope_beyond_tenant` is deliberately not called
//!
//! Same rule as [`crate::domain::service::results`], whose header states it in
//! full: that guard exists for a measured asymmetry in the *write* path
//! (`upsert_run_results` filters its `DELETE`s with the full scope and inserts
//! through a documented no-op), and a read is a `SELECT` that applies every
//! mapped predicate. A row-scoped grant therefore narrows the answer here rather
//! than disabling the endpoint, which
//! `a_row_scoped_grant_zeroes_the_counters_instead_of_failing` pins.
//!
//! # `total_runs` is a *local* count, and it is not legacy's number
//!
//! Legacy's total is `runs.len()` over its whole run history
//! (`manager/src/routes/dashboard.rs:147`), including runs that produced no
//! results. There is no cross-gear read that can answer that: `list_runs`' limit
//! is mandatory precisely so no caller can materialise every run ever executed
//! (`qa-runs-sdk/src/client.rs:42-46`), so a total taken from the listing would
//! silently be `min(total, `[`RUN_PAGE`]`)`. The plan's own mapping row says the
//! run counts are read *locally*
//! (`plans/2026-08-18-qa-insights-gear.md:330`), and
//! `ResultsRepository::count_ingested_runs` is the local quantity that is exact:
//! runs whose results are ingested. A run whose results have not landed yet is
//! missing, which `cpt-cf-qa-principle-async-insights` makes a normal transient
//! state — and that method's doc carries what the count costs.
//!
//! # Neither this endpoint nor this feature discharges `cpt-cf-qa-fr-insights-dashboard`
//!
//! **Stated first, because the rest of this section is easy to read as a claim
//! that it is.** PRD §5.5, `cpt-cf-qa-fr-insights-dashboard` requires three things: this dashboard
//! aggregate, a coverage view at `GET /qa/v1/dashboard/coverage`, and the
//! analytics overview.
//!
//! Of the dashboard bullet's own list — "recent runs, run and test counts, **pass
//! rates**, active and queued runs" (`:579`) — Task 18 supplied the run
//! activity, the per-run trend and the daily pass/fail trend; **Task 21b the
//! pass-rate and failed-KPI half** (`pass_rate_24h`, `pass_rate_prev_24h`,
//! `failed_24h_count`, `failed_prev_24h_count`, `failed_recent`), which now
//! ships. **The flaky and quality-vector sections were Task 23's and are not on
//! the wire** — that task shipped the *analytics* folds of those names, which are
//! a different grain and a different classification; the section below on the five
//! unfilled fields says what each of the two still needs. So the bullet's list is
//! **not** complete, this sentence said it was, and the requirement is doubly
//! undischarged — the coverage half is the paragraph below, and
//! `environments_summary` is unowned.
//!
//! **The coverage half is a different matter, and Task 19 did not discharge it.**
//! This section read "the requirement is satisfied across Tasks 18, 19, 21 and 23
//! … Task 19 the coverage view" until Task 19's own Step 0 disproved it — see the
//! coverage section below, which is that Step 0. What Task 19 ships is the **shape**: the path is registered, it
//! takes legacy's parameter set (none) and answers legacy's entry shape, with **no
//! entries, in every deployment**, until an upstream that does not exist yet
//! lands. See "The coverage view, and why it answers an empty array" for the two
//! missing upstreams and their citations.
//!
//! **And the requirement does not describe the same quantity legacy computes,
//! which no decision reconciles.** That paragraph is on
//! `api::rest::dto::CoverageBuildDto` with the rest of the coverage transcript —
//! see the section below — because it is an argument about what a coverage point
//! *is*. It is unsettled, and nothing here settles it or discharges the clause.
//!
//! # Three fields are left at their `Default`, and each one is a missing upstream
//!
//! `qa_insights_sdk::DashboardStats` is legacy's sixteen-field payload plus
//! `queued_runs`. Task 18 filled seven of the seventeen, Task 21b five more,
//! Task 23b one and Task 25a one, so fourteen are computed. The other three are
//! not zero because they were measured as zero:
//!
//! * `total_plans` needs a qa-catalog plan listing; `total_schedules` a qa-runs
//!   schedule listing. Neither is on any port in this gear, and plan ruling R3
//!   forbids adding a port method no test here exercises. **Both are genuinely
//!   unowned** — no task in the plan claims them, and neither does
//!   `environments_summary`, which is qa-environments' data behind a port that does
//!   not exist (legacy fans out a health *check* per platform,
//!   `manager/src/routes/dashboard.rs:421-460`). **Task 25a's
//!   [`crate::domain::ports::EnvironmentReader`] is not that port** and does not
//!   shorten this list: it resolves an id to a *name*, which is what the
//!   analytics group chart needs; the footer strip needs a live health probe,
//!   which qa-environments exposes nothing for.
//!
//! **`quality_vectors_pass_rate` used to be on this list and is not.** It was
//! **Task 25a's**, together with the production `CatalogReader` adapter that task
//! pulled forward from Task 40 — the adapter it could not be computed without,
//! whatever repository read was added. It is legacy's **dashboard**
//! quality-vector query (the SQL at `dashboard.rs:483-492` grouped by
//! `tr.test_file` under the sixth classification, the join and fold at
//! `:501-538`), over
//! [`crate::domain::repos::ResultsRepository::file_status_counts`] and
//! [`quality_vector_pass_rates`]. Note the citation: this list said
//! `dashboard.rs:483-494`, which is the SQL and the two lines that close the
//! `query_as` call — the fold that turns those rows into the rendered array is
//! thirty-six lines further on and is where four of its five properties live.
//!
//! **`flaky_tests` used to be on this list and is not.** It was **Task 23**'s and
//! Task 23 did not fill it: that task
//! shipped the *analytics* tier's four folds — the flaky detector keyed on
//! `test_file`, the quality-vector counts, the grouped summaries and the group
//! filter, all in `crate::domain::analytics::aggregates` — and none of those is
//! this quantity. **Task 23b filled it**, from legacy's **dashboard** flaky query
//! (`dashboard.rs:380-400`, grouped by `tr.test_name, rr.plan_id` under the sixth
//! classification), over
//! [`crate::domain::repos::ResultsRepository::flaky_groups`] and [`flaky_card`].
//! Task 25 assembles the overview payload and computes neither of the two; the
//! plan's traceability row for this requirement names 25 where the work actually
//! is. (Task 25**a** did compute the second — but as *this* endpoint's field,
//! not as part of the overview, which is the distinction that paragraph is
//! about.)
//!
//! **`failed_recent` and the four 24-hour counters were the sixth entry on this
//! list and are no longer on it** — Task 21b filled them, over two repository
//! reads of their own rather than by folding Task 18's windowed read, because
//! legacy's KPI query (`manager/src/routes/dashboard.rs:317-348`) carries **no
//! phase restriction** at all — zero occurrences of `phase` in the statement —
//! and windows on `COALESCE(rr.finished_at, rr.created_at)`, so it counts rows of
//! runs still in progress. Folding them out of the daily read would have silently
//! given them the daily trend's rule instead, which is why they were held back
//! from Task 18 rather than approximated. The KPI section above is that Step 0.
//!
//! Recorded in three places on purpose — here, on the contract type, and in the
//! endpoint's own description — because a `0` on the wire is indistinguishable
//! from a real zero, and `the_fields_with_no_upstream_yet_are_left_untouched` is
//! the assertion that makes a later task update all three. **It has now done so
//! twice**, for Task 21b's five and Task 23b's one.
//!
//! # The coverage view answers an empty array, and the argument for it moved
//!
//! Task 19 and [`DashboardService::coverage`]. **The transcript — what legacy's
//! `api_coverage` does field by field, why an *absent* entry rather than a zeroed
//! one is legacy's own answer, the two missing upstreams with their citations,
//! and the objection to compiling a PEP decision that protects no data — is on
//! `api::rest::dto::CoverageBuildDto`**, the type whose emptiness it explains.
//!
//! Moved there by Task 21b at the request of Phase A's whole-phase review, which
//! assigned the move to plan Task 21 and named that type as the preferred home:
//! this header was 319 lines and 93 of them were about a method that reads
//! nothing. The prose is unchanged — only intra-doc links were re-pointed — and
//! this pointer is the only thing left in its place, so there is still exactly
//! one copy.
//!
//! Read it before adding anything to [`DashboardService::coverage`], and in
//! particular before filling the array from `qa_test_results`: test-status counts
//! are not code coverage.
//!
//! # Shaped for Task 19 without being built for it, and Task 19 fit
//!
//! Task 18 wrote this section as a forecast: coverage would add a second method
//! on [`DashboardService`] and a second `OperationBuilder` block, and nothing here
//! was generalised in advance to receive them — no trait, no enum of aggregates,
//! no shared "aggregate request" type — the one deliberately factored thing being
//! [`DashboardService::scope`], where a second read would otherwise copy a
//! resource type and an action string.
//!
//! It held. Task 19 added [`DashboardService::coverage`], which reuses
//! [`DashboardService::scope`] and nothing else, plus one route and one handler
//! beside Task 18's; it restructured nothing and generalised nothing. The
//! forecast is left standing rather than deleted because the *outcome* is the
//! evidence for the shape, and Tasks 20-27 add more aggregates here.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_catalog_sdk::UniverseTest;
use qa_insights_sdk::{
    CoverageBuild, DailyStatusPoint, DashboardRun, DashboardStats, FailedTestCard, FlakyTestCard,
    QualityVectorPassRate, RunTestTrendPoint, TestResultRecord,
};
use qa_runs_sdk::{Run, RunState, RunTarget};
use time::{Date, Duration, OffsetDateTime, UtcOffset};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::analytics::universe::normalize_test_path;
use crate::domain::error::DomainError;
use crate::domain::ports::{CatalogReader, RunsReader};
use crate::domain::repos::{
    FileStatusCount, FlakyGroup, ResultsRepository, RunStatusCount, StatusRowCount,
};
use crate::domain::service::ingest::{
    FAILED_STATUSES, PASSED_STATUSES, StatusBucket, classify, plan_identity,
};
use crate::domain::service::{DbProvider, actions, resources};

/// `days` when the caller names none — legacy's `unwrap_or(14)`
/// (`manager/src/routes/dashboard.rs:104`).
const DEFAULT_DAYS: u32 = 14;

/// The `days` floor — legacy's `.clamp(3, 90)`, same line.
const MIN_DAYS: u32 = 3;

/// The `days` ceiling — legacy's `.clamp(3, 90)`, same line.
const MAX_DAYS: u32 = 90;

/// How many runs the recent list and the trend chart hold — legacy's
/// `runs.into_iter().take(10)` (`manager/src/routes/dashboard.rs:158`).
const RECENT_RUNS: usize = 10;

/// How many active runs are *listed*, as against counted — legacy's `.take(10)`
/// on the active list only (`manager/src/routes/dashboard.rs:155`).
///
/// A separate constant from [`RECENT_RUNS`] even though both are ten, because
/// they are two of legacy's decisions that happen to agree: one bounds a chart's
/// x-axis and the other bounds a card. Collapsing them would make a later change
/// to either silently change both.
const ACTIVE_RUNS_LISTED: usize = 10;

/// How many recent failures the card list holds — legacy's `LIMIT 10`
/// (`manager/src/routes/dashboard.rs:278`).
///
/// A third constant that happens to be ten, for the reason
/// [`ACTIVE_RUNS_LISTED`] gives: it is a different one of legacy's decisions,
/// and collapsing them would make a change to one silently change the others.
/// `u64` rather than `usize` because it is a SQL `LIMIT` rather than a `take`.
const FAILED_RECENT_LISTED: u64 = 10;

/// How many flaky tests the card list holds — legacy's `LIMIT 10`
/// (`manager/src/routes/dashboard.rs:399`).
///
/// A **fourth** constant that happens to be ten, for the reason
/// [`ACTIVE_RUNS_LISTED`] gives: it is a different one of legacy's decisions, and
/// collapsing them would make a change to one silently change the others. `u64`
/// rather than `usize` because it is a SQL `LIMIT` rather than a `take`, and
/// unlike [`FAILED_RECENT_LISTED`] it bounds a list of *groups* rather than of
/// rows.
const FLAKY_TESTS_LISTED: u64 = 10;

/// The flaky window — legacy's `INTERVAL '7 days'`
/// (`manager/src/routes/dashboard.rs:391`).
///
/// **It does not move with `days`, and that is legacy's behaviour rather than an
/// omission.** [`resolve_days`] governs the daily trend and nothing else; the
/// 24-hour pair has [`KPI_WINDOW`] and this has its own literal, so a caller
/// asking for 90 days still gets seven days of flaky evidence.
/// `qa_insights_sdk::DashboardStats`' window section records the same split.
///
/// Seven days rather than `7 * KPI_WINDOW`: they are two unrelated decisions of
/// legacy's that happen to be expressible in each other's terms, and deriving one
/// from the other would make a change to the KPI width move the flaky window too.
const FLAKY_WINDOW: Duration = Duration::days(7);

/// The quality-vector window — legacy's `INTERVAL '7 days'`
/// (`manager/src/routes/dashboard.rs:490`).
///
/// **The same seven days as [`FLAKY_WINDOW`] and deliberately not derived from
/// it**, for the reason that constant states about `KPI_WINDOW`: they are two
/// unrelated decisions of legacy's that happen to be expressible in each other's
/// terms, spelled as two literals in two statements (`:391` and `:490`). Deriving
/// one from the other would make a change to the flaky evidence width silently
/// move the quality-vector bars.
///
/// It does not move with `days` either — [`resolve_days`] governs the daily trend
/// and nothing else, which makes this the **fourth** window on the endpoint.
const QUALITY_VECTOR_WINDOW: Duration = Duration::days(7);

/// The width of each KPI window — legacy's `INTERVAL '24 hours'`
/// (`manager/src/routes/dashboard.rs:321`).
///
/// The previous window is `[now - 2 × this, now - this)`, which is legacy's
/// `48 hours` / `24 hours` pair (`:325-326`) written as one interval rather than
/// two: a deployment that ever wanted a different KPI width would otherwise have
/// to change three literals consistently.
const KPI_WINDOW: Duration = Duration::hours(24);

/// The ceiling on the single qa-runs listing.
///
/// # It is a real bound on two of the numbers, and legacy had none
///
/// Legacy counts active runs over its **whole** run history, because that listing
/// is a local `SELECT`. Here `limit` is mandatory
/// (`qa-runs-sdk/src/client.rs:42-46`) and the counts are folds over the page, so
/// `active_runs` and `queued_runs` are exact only while fewer than 200 runs are
/// newer than the oldest active or queued one. A run that has been running longer
/// than the 200 most recent launches took would fall off.
///
/// 200 because it is the subsystem's page default — the same number qa-runs'
/// queue contract fixes and `infra::storage::db::PAGE_LIMITS` re-declares — so a
/// deployment reasoning about one listing ceiling reasons about all of them. Not
/// imported from that constant: it is `infra`, and the ceiling on a cross-gear
/// call is a domain decision that would still hold if this gear had no database.
const RUN_PAGE: u32 = 200;

/// Reads over `qa_test_results` and live run state, reduced to one payload.
///
/// Generic over the repository rather than boxed, for the reason
/// [`crate::domain::service::AppServices`] gives: [`ResultsRepository`]'s methods
/// are generic over their `DBRunner`, so the trait is not object-safe and the
/// parameter propagates to the composition root. `Clone + 'static` is not
/// required — nothing here opens a transaction, for the same reason
/// [`crate::domain::service::results::ResultsService`] does not: three
/// independent `SELECT`s on a pooled connection need no isolation guarantee a
/// dashboard would notice, and the payload is already a snapshot of two different
/// systems.
pub struct DashboardService<R> {
    db: Arc<DbProvider>,
    results: R,
    runs: Arc<dyn RunsReader>,
    /// The catalog's quality vectors, for
    /// [`DashboardStats::quality_vectors_pass_rate`]. **The only cross-gear read
    /// on this endpoint besides qa-runs'**, and it is conditional — see
    /// [`Self::stats`].
    catalog: Arc<dyn CatalogReader>,
    policy_enforcer: PolicyEnforcer,
}

impl<R> DashboardService<R>
where
    R: ResultsRepository,
{
    #[must_use]
    pub const fn new(
        db: Arc<DbProvider>,
        results: R,
        runs: Arc<dyn RunsReader>,
        catalog: Arc<dyn CatalogReader>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            results,
            runs,
            catalog,
            policy_enforcer,
        }
    }

    /// Everything `GET /qa/v1/dashboard` returns, for `days` of history.
    ///
    /// # The order of operations is load-bearing
    ///
    /// The PEP decision is compiled **before** qa-runs is asked anything.
    /// Otherwise a caller with no grant could still learn which runs exist, which
    /// is the same reason
    /// [`crate::domain::service::reconcile::ReconcileService::rebuild`] compiles
    /// its scope first.
    /// `a_denied_caller_reads_nothing_and_does_not_reach_qa_runs` asserts the
    /// listing count and not just the error, because asserting the error alone
    /// passes for an implementation that listed first.
    ///
    /// The window is closed over one `today`, read once. Reading the clock per
    /// query would let the day axis and the counters disagree across a midnight
    /// boundary — a once-a-day bug, and the kind that is never diagnosed.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies, or compiles a scope it
    /// cannot express — including the platform-root caller, whose nil tenant
    /// yields no constraint and so fails compilation with
    /// `ConstraintsRequiredButAbsent` (`PolicyEnforcer::access_scope` always
    /// requires constraints; `authz-resolver-sdk/src/pep/compiler.rs:84-86`) — and
    /// when **either** sibling refuses the subject: qa-runs on the run listing,
    /// or **qa-catalog on the universe read** (Task 25a).
    ///
    /// [`DomainError::Internal`] when the qa-runs listing **or the qa-catalog
    /// universe read** fails. Neither is laundered into an empty dashboard: the
    /// reconciler's listing follows the same rule and for the same reason — a
    /// broken upstream must not look like an idle one, and an empty array is
    /// indistinguishable from a measured empty window.
    ///
    /// # The qa-catalog grant is a new precondition on this endpoint, and it
    /// # bites *conditionally*
    ///
    /// A caller holding `qa.test_result/list` and **no** qa-catalog grant used to
    /// get a full dashboard and now gets a 403 on the whole payload — including
    /// the thirteen fields that need no catalog. That is deliberate, for the
    /// reason above, and it is stated here because the failure is not local to the
    /// one field it is about.
    ///
    /// **It is not deterministic from the caller's grants alone.** The
    /// quality-vector universe read is skipped entirely when no test file had
    /// **any** row in the window (legacy's own short-circuit — see
    /// [`Self::stats`]' body), so the *same* caller on the *same* deployment gets
    /// `200` while the last seven days hold no result row and `403` once one
    /// lands. One `SKIPPED` row is enough to flip it, because the read groups on
    /// the file rather than on the status. A deployment diagnosing an
    /// intermittent 403 here should look at the grant, not at qa-catalog's
    /// health. **A `product_id` is unrelated to this**: the repository listing
    /// its own filter reads is a separate, unconditional call — see the next
    /// section — so this short-circuit's behaviour is exactly what it was
    /// before `product_id` existed.
    ///
    /// # `product_id` — through the run's repository, never a test universe
    ///
    /// A run is attributed to a product **through its target's repository,
    /// never its environment** — the client's own rule
    /// (`qa-platform-ui/src/lib/productScope.ts`'s header). Concretely:
    /// [`CatalogReader::list_repos`] resolves every repository's owning
    /// product, [`run_repo_id`] reads the `repo_id` a `Plan`, `Test` or
    /// `Collect` target names directly off the target (never through
    /// [`plan_identity`], which the next paragraph says why not), and a run
    /// whose repository is not one of the product's is dropped **before** any
    /// fold runs — so the two counts, both lists and the per-run trend all
    /// narrow together, exactly as the client's filtered Runs list does. Six
    /// more reads narrow the same way, over the persisted `repo_id` column
    /// rather than a live target: [`ResultsRepository::count_ingested_runs`],
    /// `run_status_counts_since`, `effective_status_counts`, `recent_failures`,
    /// `flaky_groups` and `file_status_counts` (the last of which is what
    /// narrows `quality_vectors_pass_rate`, incidentally and for free, by
    /// dropping every counted file whose repository is not the product's).
    /// **That column is not the live target, and a collect run is where the two
    /// diverge**: ingest stamps it `NULL` for a collect run exactly as it does
    /// for a custom plan, so these six reads never counted a collect run's own
    /// rows in the first place, scoped or not —
    /// `infra::storage::results_sea_repo::repo_scope`'s doc has the full
    /// argument. Only the live-run reads in the previous paragraph see a
    /// collect run at all.
    ///
    /// **This is deliberately *not* [`crate::domain::analytics`]'s join.** That
    /// module resolves a product through [`CatalogReader::list_universe`] —
    /// a checkout walk that treats an unsynced repository exactly like one
    /// with no tests, which is the right denominator for a suite summary and
    /// the wrong test for "does this run exist": a run plainly exists in
    /// qa-runs regardless of whether its repository has ever synced. Reusing
    /// that join here made every run of an unsynced repository silently
    /// disappear from its own product's dashboard — indistinguishable from an
    /// idle product — which is exactly the failure mode
    /// `cpt-cf-qa-principle-async-insights` and this method's own "a broken
    /// upstream must not look like an idle one" both exist to prevent
    /// elsewhere on this same endpoint. [`CatalogReader::list_repos`]'s own doc
    /// carries the contrast in full.
    ///
    /// **A custom-plan run matches no product**, because [`run_repo_id`]
    /// answers `None` for one — a custom plan spans repositories, so there is
    /// no single one to test membership with, and the expansion that could
    /// resolve one (`qa-platform-ui/src/lib/productScope.ts`'s
    /// `productIdOfCustomPlan`) is a client-side read over qa-catalog's custom
    /// plan definitions that this gear has no port for. **This is a real,
    /// disclosed gap** rather than a silently narrower answer: a custom-plan
    /// run is present in the deployment-wide dashboard and absent from every
    /// product-scoped one, and the UI states this on the page rather than only
    /// in this description — see `qa-platform-ui/src/components/dashboard/ProductScopeNotice.tsx`.
    ///
    /// [`DomainError::Database`] for a driver failure.
    pub async fn stats(
        &self,
        ctx: &SecurityContext,
        days: Option<u32>,
        product_id: Option<Uuid>,
    ) -> Result<DashboardStats, DomainError> {
        let days = resolve_days(days);
        let scope = self.scope(ctx).await?;

        // A plain repository listing, not a test universe — see this method's
        // `# product_id` section for why the substitution matters. `None`
        // unless a product was actually asked for, so an unscoped request pays
        // no extra cross-gear round trip. A `Vec` rather than a `HashSet`: it
        // is both the membership set the run filter below tests against and
        // the slice every repo-scoped read below takes, and a product's own
        // repository count is nowhere near where a linear `contains` matters.
        let product_repo_ids: Option<Vec<Uuid>> = match product_id {
            Some(product_id) => {
                let repos = self.catalog.list_repos(ctx).await?;
                Some(
                    repos
                        .into_iter()
                        .filter(|repo| repo.product_id == product_id)
                        .map(|repo| repo.id)
                        .collect(),
                )
            }
            None => None,
        };

        let mut runs = self.runs.list_recent_runs(ctx, RUN_PAGE).await?;
        // Counted **before** the retain below drops them, and only when a
        // product filter is actually applied: an unscoped request drops
        // nothing, so this must stay zero rather than count every
        // unattributable run in the deployment. This is a narrower set than
        // "runs the filter removed" — a run belonging to a *different*
        // product is removed too, and removing it is the filter working, not
        // a gap `unattributable_runs` exists to surface.
        let unattributable_runs = if product_repo_ids.is_some() {
            count_of(
                runs.iter()
                    .filter(|run| run_repo_id(&run.target).is_none())
                    .count(),
            )
        } else {
            0
        };
        // Narrowed to the product **before** any fold below runs — see
        // `run_repo_id`'s doc for what "narrowed" means for a custom-plan run.
        // `retain` rather than `into_iter().filter().collect()`: the page is
        // already the `Vec` this needs, so there is nothing to rebuild.
        if let Some(repo_ids) = &product_repo_ids {
            runs.retain(|run| run_repo_id(&run.target).is_some_and(|id| repo_ids.contains(&id)));
        }
        let activity = summarize_runs(&runs);

        // One clock read for the whole payload, for the reason this doc gives —
        // and `today` is derived from it rather than read again, so the day axis
        // and the KPI windows cannot straddle a midnight between two calls.
        let now = OffsetDateTime::now_utc();
        let today = now.date();
        let since = window_start(today, days);
        let kpi_from = now - KPI_WINDOW;
        let prev_from = kpi_from - KPI_WINDOW;
        // A third window off the same clock read, and the widest of the three.
        // `FLAKY_WINDOW` records why it is not derived from `KPI_WINDOW`.
        let flaky_from = now - FLAKY_WINDOW;
        // A fourth, equal to the third and not derived from it —
        // `QUALITY_VECTOR_WINDOW` says why.
        let vectors_from = now - QUALITY_VECTOR_WINDOW;

        let conn = self.db.conn()?;
        let repo_ids = product_repo_ids.as_deref();
        let recent_ids: Vec<Uuid> = activity.recent_runs.iter().map(|run| run.run_id).collect();
        let per_run = self
            .results
            .run_status_counts(&conn, &scope, &recent_ids)
            .await?;
        let windowed = self
            .results
            .run_status_counts_since(&conn, &scope, since, repo_ids)
            .await?;
        let total_runs = self
            .results
            .count_ingested_runs(&conn, &scope, repo_ids)
            .await?;
        // `None` upper bound on the current window and `kpi_from` on the
        // previous: legacy's own bounds, and
        // `ResultsRepository::effective_status_counts` records why an upper bound
        // of "now" would be a divergence.
        let current = self
            .results
            .effective_status_counts(&conn, &scope, kpi_from, None, repo_ids)
            .await?;
        let previous = self
            .results
            .effective_status_counts(&conn, &scope, prev_from, Some(kpi_from), repo_ids)
            .await?;
        let failures = self
            .results
            .recent_failures(
                &conn,
                &scope,
                &FAILED_STATUSES,
                kpi_from,
                FAILED_RECENT_LISTED,
                repo_ids,
            )
            .await?;
        // The two status partitions are the domain's, and `classify`'s arms are
        // defined from the same two constants — so this predicate and the fold
        // beside it are one definition. `ResultsRepository::flaky_groups` carries
        // why this read's `HAVING`, `ORDER BY` and `LIMIT` are in SQL where every
        // other aggregate here folds in the domain.
        let flaky = self
            .results
            .flaky_groups(
                &conn,
                &scope,
                &PASSED_STATUSES,
                &FAILED_STATUSES,
                flaky_from,
                FLAKY_TESTS_LISTED,
                repo_ids,
            )
            .await?;

        // Ruling R5's sixth classification per **file**, over the same two
        // constants and for the same reason `flaky_groups` takes them:
        // `dashboard::kpi_of` reduces a whole window to one pair of numbers, and
        // this needs the same partition per group. `repo_ids` here is what makes
        // `quality_vectors_pass_rate` narrow with the rest of the payload — the
        // fold below drops any file whose repository is not the product's,
        // exactly as `run_repo_id` drops a run of one.
        // `ResultsRepository::file_status_counts` carries the whole argument.
        let vector_files = self
            .results
            .file_status_counts(
                &conn,
                &scope,
                &PASSED_STATUSES,
                &FAILED_STATUSES,
                vectors_from,
                repo_ids,
            )
            .await?;
        // **The catalog is read only if there is something to join it to**, which
        // is legacy's own short-circuit: `Ok(rows) if rows.is_empty() =>` with the
        // comment *"Nothing to aggregate; skip the expensive TEST_META parse
        // entirely."* (`dashboard.rs:498-500`). Here the expense is a cross-gear
        // round trip rather than hundreds of file reads, and skipping it cannot
        // change the answer: the fold iterates `vector_files`, so with none of
        // them there is no vector to emit whatever the universe holds.
        //
        // Note the exact trigger: **no group**, not "no counted row". A file
        // whose window holds nothing but `SKIPPED` rows still forms a group —
        // three zeros — so the read happens and the section renders zero-counter
        // rows for that file's vectors, which is legacy's behaviour.
        // `ResultsRepository::file_status_counts`' doc carries why those rows are
        // real output.
        //
        // `None, None` — no product filter and each repository's own default
        // branch. Both are legacy's: `build_quality_vectors_by_file` walks
        // *every* plan (`analytics.rs:2017-2021`, `list_plans_with_repos(..., None)`)
        // and resolves each one's checkout rather than selecting a branch, which
        // is what `CatalogReader::list_universe`'s `branch: None` means. Note the
        // asymmetry the port's doc calls out: `None` there is the **default**
        // branch, not every branch.
        //
        // **A failure here fails the whole payload, where legacy logs and
        // continues** (`dashboard.rs:539`, `tracing::warn!` leaving the list
        // empty). Deliberate, and the same divergence this method's `# Errors`
        // section records for the qa-runs listing: an empty array is
        // indistinguishable from a measured empty window, so a broken upstream
        // must not look like a suite with no quality vectors. The cost is that a
        // caller with no qa-catalog grant now gets a 403 on the whole dashboard;
        // that is visible and fixable, which "silently no bars" is not.
        //
        // **Note the interaction between the two decisions above, because it is
        // not obvious from either alone:** the short-circuit means the grant is
        // only exercised when a group came back, so a caller missing it sees
        // `200` on a quiet deployment and `403` on the same deployment once a
        // result row lands in the window. The 403 is therefore a function of the
        // *data* as well as of the grant. `# Errors` carries it for a caller;
        // recorded here too, because this is the line that makes it true and a
        // reader tempted to make the read unconditional would remove exactly
        // that conditionality.
        //
        // **Unaffected by `product_id`.** This is `list_universe`, not
        // `list_repos` — the checkout-derived read the `# product_id` section
        // argues against reusing for a run-existence question — and it is used
        // here only to resolve a *display name* for each already-narrowed
        // `vector_files` entry, not to decide which rows count. So it stays
        // `None, None` regardless of `product_id`, exactly as before that
        // parameter existed.
        let universe = if vector_files.is_empty() {
            Vec::new()
        } else {
            self.catalog.list_universe(ctx, None, None).await?
        };

        // Folded into locals so the struct literal below can follow field
        // declaration order (`clippy::inconsistent_struct_constructor`) while
        // still borrowing `activity.recent_runs` before it is moved.
        let trend = run_trend_points(&activity.recent_runs, &per_run);
        let daily = daily_points(today, days, &windowed);
        let current = kpi_of(&current);
        let previous = kpi_of(&previous);

        Ok(DashboardStats {
            total_runs,
            active_runs: activity.active_runs,
            queued_runs: activity.queued_runs,
            recent_runs: activity.recent_runs,
            recent_run_test_trend: trend,
            daily_test_status_trend: daily,
            active_runs_list: activity.active_runs_list,
            failed_recent: failures.into_iter().map(failure_card).collect(),
            failed_24h_count: current.failed,
            failed_prev_24h_count: previous.failed,
            pass_rate_24h: current.pass_rate,
            pass_rate_prev_24h: previous.pass_rate,
            flaky_tests: flaky.into_iter().map(flaky_card).collect(),
            // The three still unfilled, and each is a missing read or a missing
            // upstream: `total_plans`, `total_schedules` and `environments_summary`
            // need cross-gear reads no port in this gear has, and **no task in
            // the plan owns any of the three**. This module's header names them
            // one by one and `qa_insights_sdk::DashboardStats` carries what each
            // still wants; the endpoint description says so on the wire.
            //
            // **It was five until Task 23b**, which filled `flaky_tests` above,
            // and four until Task 25a, which filled
            // `quality_vectors_pass_rate` below with the `CatalogReader` adapter
            // it could not be computed without.
            quality_vectors_pass_rate: quality_vector_pass_rates(&vector_files, &universe),
            unattributable_runs,
            ..DashboardStats::default()
        })
    }

    /// Coverage per product build — `GET /qa/v1/dashboard/coverage`.
    ///
    /// **Empty today, and empty for a reason `api::rest::dto::CoverageBuildDto`
    /// states in full**: legacy parses its percentages out of workflow log text,
    /// which this architecture has no reader for until p2, and groups them by a
    /// `product_key` that `qa_runs_sdk::Run` does not carry. Read that type's doc
    /// before adding anything here, and in particular before filling the array
    /// from `qa_test_results` — test-status counts are not code coverage. (That
    /// transcript was in this module's header until Task 21b's doc split; the
    /// header's coverage section is now a pointer to it.)
    ///
    /// Registered and shaped rather than omitted or stubbed with `todo!()`: the
    /// path, the no-parameter signature, the entry shape and the empty answer are
    /// all legacy's, and only the *population* of the array waits on an upstream.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies, or compiles a scope it
    /// cannot express — the same two cases [`Self::stats`] documents, including
    /// the platform-root caller whose nil tenant fails compilation.
    pub async fn coverage(&self, ctx: &SecurityContext) -> Result<Vec<CoverageBuild>, DomainError> {
        // The compiled scope is discarded rather than bound: there is no read for
        // it to narrow yet. Compiling it at all is the decision this module's
        // header argues, along with the objection to it.
        self.scope(ctx).await?;

        Ok(Vec::new())
    }

    /// The caller's scope for an aggregate read, derived fresh on every call.
    ///
    /// Factored out for the reason
    /// [`crate::domain::service::results::ResultsService`]' equivalent gives, and
    /// Task 19's [`Self::coverage`] is the second read that now calls it: two
    /// inline copies are two places for a resource type or an action string to
    /// drift with nothing in this crate failing — which is the hazard
    /// `test_support::RecordingAuthZ` exists for, and
    /// `the_coverage_view_authorizes_under_test_result_list_once` is what would
    /// catch the drift.
    async fn scope(&self, ctx: &SecurityContext) -> Result<AccessScope, DomainError> {
        self.policy_enforcer
            .access_scope(ctx, &resources::TEST_RESULT, actions::LIST, None)
            .await
            .map_err(DomainError::from)
    }
}

/// Legacy's day window: `days.unwrap_or(14).clamp(3, 90)`
/// (`manager/src/routes/dashboard.rs:104`).
///
/// The clamp is silent, exactly as legacy's is — `days=365` is answered with 90
/// days of data rather than a 400. Kept because it is behaviour a client may
/// already depend on, and because the alternative turns a cosmetic mistake in a
/// query string into a broken page.
fn resolve_days(days: Option<u32>) -> u32 {
    days.unwrap_or(DEFAULT_DAYS).clamp(MIN_DAYS, MAX_DAYS)
}

/// The instant the day window opens: midnight UTC, `days - 1` days before
/// `today`.
///
/// Legacy binds `start_day = Utc::now().date_naive() - Duration::days(days - 1)`
/// as a `date` and compares it against `DATE(COALESCE(...))`
/// (`manager/src/routes/dashboard.rs:105`, `:222`), so the window opens at the
/// start of that day and the series holds exactly `days` points. The `- 1` is
/// what makes `days = 1` mean "today" rather than "today and yesterday" — and it
/// is why [`resolve_days`] is applied first: a `days` of `0` would open the
/// window *tomorrow*.
fn window_start(today: Date, days: u32) -> OffsetDateTime {
    (today - Duration::days(i64::from(days) - 1))
        .midnight()
        .assume_utc()
}

/// Whether a run is one of the "active" runs the dashboard counts and lists.
///
/// # The mapping from legacy's two Argo phases, with both citations
///
/// Legacy: `run.phase == "Running" || run.phase == "Pending"`
/// (`manager/src/routes/dashboard.rs:150` and `:154` — the count and the list use
/// the identical predicate, which is what makes "23 active, 10 listed" coherent).
/// There is no `phase` here, so the two phases map onto
/// [`qa_runs_sdk::RunState`]:
///
/// * **Argo `Running` → [`RunState::Running`].** Unambiguous.
/// * **Argo `Pending` → [`RunState::Dispatching`].** Argo `Pending` is a Workflow
///   that has been *submitted* and whose pods have not started: legacy stamps
///   that phase on the run it returns from a successful submit
///   (`manager/src/services/argo.rs:306`, in `submitted_run`) and defaults an
///   absent Argo phase to it (`:2273-2275`). `Dispatching` is that same moment in
///   this architecture — the state machine is
///   `created → queued? → dispatching → running → terminal`
///   (`qa-runs/src/domain/state_machine.rs:108`), and that module describes the
///   state as "the state that holds the platform claim while the sync and bundle
///   build run" (`:122-123`), during which "a row sits in `dispatching` with no
///   execution reference for the whole force-sync + bundle-build window, which is
///   minutes" (`:527`). Submitted, claimed, not yet executing — Argo `Pending`.
///   (`:234-236` was cited here until Task 18's fix round; it enumerates the
///   *exits* from `Dispatching` and sits above the arm at `:237`, so trimming it
///   to "The executor accepted the run" read as a definition it is not.)
///
/// # The three exclusions, and why each is legacy's answer rather than a
/// # simplification
///
/// * **[`RunState::Queued`] is not active.** Legacy's queue is a separate table
///   written by `manager/src/services/run_queue.rs`, which contains **zero**
///   references to `run_results` (grepped, not assumed), and a queued launch has
///   no Workflow yet — so it appears in neither half of `list_runs_with_history`
///   (`manager/src/services/run_history.rs:311-391`) and nothing on that
///   dashboard counts it. It is counted here, separately, because
///   `cpt-cf-qa-fr-insights-dashboard`'s "active and queued runs" clause names it
///   (PRD §5.5, `cpt-cf-qa-fr-insights-dashboard` — one clause of a requirement this endpoint does not
///   discharge alone; see this module's header) and because this architecture
///   *has* an authoritative row for it; see
///   [`qa_insights_sdk::DashboardStats::queued_runs`].
/// * **[`RunState::Created`] is not active** and is not queued either. It is the
///   instant before a run becomes one or the other
///   (`qa-runs/src/domain/state_machine.rs:223-226`), and legacy has no object at
///   all at that point. A run sitting there is in no counter, which is the honest
///   answer for a state whose only legal exits are the two that are counted.
/// * **The six terminal states are not active** —
///   `Succeeded, Failed, Canceled, TimedOut, Expired, Error`, named rather than
///   counted at `qa-runs/src/domain/state_machine.rs:52-59` for the reason
///   recorded there.
///
/// `matches!` on the two rather than a negation of "terminal": the negation would
/// silently make `Created` and `Queued` active the moment somebody read it as
/// "not finished", and `is_terminal` is a function in a crate this one cannot
/// call anyway.
const fn is_active(state: RunState) -> bool {
    matches!(state, RunState::Dispatching | RunState::Running)
}

/// The four values legacy derives from its single run listing.
///
/// A struct rather than a four-tuple because all four are counts or lists of
/// runs and a transposed pair would compile — the same argument
/// [`crate::domain::service::ServiceDeps`] makes about its own fields.
///
/// Private, like every fold in this module: the only callers are
/// [`DashboardService::stats`] and this module's tests, which are a child module
/// and so reach them without a wider visibility. A `pub(crate)` fold would be an
/// invitation to compute a dashboard number somewhere other than here.
struct RunActivity {
    active_runs: u64,
    queued_runs: u64,
    recent_runs: Vec<DashboardRun>,
    active_runs_list: Vec<DashboardRun>,
}

/// The repository a run's target names, read directly off the target —
/// [`DashboardService::stats`]' product filter's membership test, applied to
/// one run.
///
/// # Deliberately *not* [`plan_identity`]
///
/// `plan_identity` answers `(None, None)` for `RunTarget::Collect` as well as
/// `RunTarget::CustomPlan` — a decision that is correct for *its* callers
/// (ingest's row projection and the analytics universe filter, both of which
/// need a **plan** identity, and a collect run executes no plan) but wrong
/// for this one: a collect run still names a real `repo_id`
/// (`RunTarget::Collect { repo_id, .. }`), and the client's own attribution
/// rule (`qa-platform-ui/src/lib/productScope.ts`) uses exactly that field for
/// exactly this kind. Reusing `plan_identity` here would make every collect
/// run vanish from its own product's Active Runs card while the client's Runs
/// list still showed it — a live run listed on one product-scoped surface and
/// silently absent from the surface next to it.
///
/// So this reduction has one `None` case, not two: `RunTarget::CustomPlan`.
/// [`DashboardService::stats`]' `# product_id` section says why that gap
/// cannot be closed without a new cross-gear read this task does not add.
const fn run_repo_id(target: &RunTarget) -> Option<Uuid> {
    match target {
        RunTarget::Plan { repo_id, .. }
        | RunTarget::Test { repo_id, .. }
        | RunTarget::Collect { repo_id, .. } => Some(*repo_id),
        RunTarget::CustomPlan { .. } => None,
    }
}

/// Fold one qa-runs listing into the run half of the dashboard.
///
/// **Counted over the whole page, listed after a `take`** — that asymmetry is
/// legacy's (`manager/src/routes/dashboard.rs:148-157`) and is the point of
/// `the_active_list_is_capped_at_ten_and_matches_the_count_predicate`: the count
/// and the list must use the same predicate, or a card reading "23 active" is
/// listing something else.
///
/// The page's order is preserved, never re-sorted. It is qa-runs' newest-first
/// contract, and re-sorting here would make "the ten most recent" mean whatever
/// this gear's tiebreak happened to be — the same hazard
/// `RunsReader::list_run_test_results` spells out for the ingest ordinal.
fn summarize_runs(runs: &[Run]) -> RunActivity {
    RunActivity {
        active_runs: count_of(runs.iter().filter(|run| is_active(run.state)).count()),
        queued_runs: count_of(
            runs.iter()
                .filter(|run| run.state == RunState::Queued)
                .count(),
        ),
        recent_runs: runs.iter().take(RECENT_RUNS).map(dashboard_run).collect(),
        active_runs_list: runs
            .iter()
            .filter(|run| is_active(run.state))
            .take(ACTIVE_RUNS_LISTED)
            .map(dashboard_run)
            .collect(),
    }
}

/// Project one qa-runs run into the dashboard's run shape.
///
/// `qa_insights_sdk::DashboardRun` exists so this crate's *contract* does not
/// depend on `qa-runs-sdk`; that type's header carries the argument, and this is
/// the function it names as "the gear that projects `Run` into this on the way
/// out".
///
/// Two fields deserve a note:
///
/// * **`phase` is `RunState::as_str()`** — qa-runs' own persisted, lowercase
///   spelling (`qa-runs-sdk/src/models.rs:266-280`). Re-casing it to legacy's
///   capitalised Argo phase would invent a third vocabulary for one value, and
///   mapping it onto legacy's five phases would have to fold six terminal states
///   onto three.
/// * **`product_key` is always `None`, and that is not an omission here.**
///   `qa_runs_sdk::Run` has no product key: VHP-319 deleted legacy's
///   product-version model and qa-catalog owns products now, so a run carries
///   `app_version` and `app_build` and nothing identifying a product. Legacy
///   reads `run_results.product_key` for this card
///   (`manager-ui/src/components/dashboard/ActiveRunsCard.tsx:54` draws it), so
///   it is a real gap — in the *run* record rather than in this projection.
///   Closing it is plan open question 1, the `product_id`/`version`/`scope`
///   mapping VHP-319 left unresolved, deferred to Task 20.
fn dashboard_run(run: &Run) -> DashboardRun {
    let (repo_id, plan_path) = plan_identity(&run.target);
    DashboardRun {
        run_id: run.id,
        name: run.name.clone(),
        phase: run.state.as_str().to_owned(),
        repo_id,
        plan_path,
        environment_id: run.environment_id,
        product_key: None,
        app_version: run.app_version.clone(),
        started_at: run.started_at,
        duration: format_duration(run.started_at, run.finished_at),
    }
}

/// One trend point per recent run, in the recent list's order.
///
/// # A run with no ingested rows is a zero point, not a missing one
///
/// Legacy reaches the same answer from the other side: its per-run query is a
/// `LEFT JOIN`, so a run with no results comes back with `COUNT(tr.id) = 0`, and
/// its consumer reads a missing map entry as zeros anyway
/// (`manager/src/routes/dashboard.rs:196-212`, `counts.map(..).unwrap_or(0)`).
/// Keeping the point is what keeps the chart's x-axis the run list — a chart
/// drawn from present points only would silently drop the newest run while its
/// results were still being ingested, which
/// `cpt-cf-qa-principle-async-insights` makes a routine state.
///
/// The counts are indexed once rather than scanned per run. Not about the
/// constant — ten runs by up to ten groups is nothing — but about this staying
/// linear if a later task raises [`RECENT_RUNS`].
fn run_trend_points(recent: &[DashboardRun], counts: &[RunStatusCount]) -> Vec<RunTestTrendPoint> {
    let mut by_run: HashMap<Uuid, Counters> = HashMap::new();
    for count in counts {
        by_run
            .entry(count.run_id)
            .or_default()
            .add(&count.status, count.rows);
    }

    recent
        .iter()
        .map(|run| {
            let counters = by_run.get(&run.run_id).copied().unwrap_or_default();
            RunTestTrendPoint {
                run_id: run.run_id,
                run_name: run.name.clone(),
                started_at: run.started_at,
                tests_total: counters.total,
                passed: counters.passed,
                failed: counters.failed,
                skipped: counters.skipped,
            }
        })
        .collect()
}

/// One point per day of the window, ascending, ending on `today`.
///
/// # Every day is present, including the days nothing ran
///
/// Legacy builds the axis with
/// `generate_series($1::date, CURRENT_DATE, INTERVAL '1 day')` and `LEFT JOIN`s
/// the runs onto it (`manager/src/routes/dashboard.rs:220`), so an idle day is a
/// zero rather than a gap. The `BTreeMap` below *is* that `generate_series`: it is
/// pre-filled with every day and only then counted into, which is also what makes
/// the output sorted without a sort.
///
/// # Two counters, not four, and that is a third fold inside one legacy endpoint
///
/// `PASSED` and `IN ('FAILED','ERROR')` (`manager/src/routes/dashboard.rs:218-219`).
/// A `SKIPPED` row moves neither, unlike in [`run_trend_points`]. [`Counters`]
/// computes all four and this reads two of them, rather than there being a second
/// narrower fold: the alternative puts `FAILED`+`ERROR` in two places.
///
/// # The day is the run's finish day, in UTC
///
/// Legacy's `DATE(COALESCE(rr.finished_at, rr.created_at))` is evaluated in the
/// database session's timezone, which is a deployment setting rather than a
/// decision. UTC is the fixed choice here, made explicit rather than inherited
/// from whatever offset a driver hands back, so the same rows bucket the same way
/// on every deployment.
///
/// A group with no finish instant is dropped. The windowed read cannot produce one
/// — it filters `run_finished_at >= since` — but this function is reachable with
/// per-run counts, and attributing an in-progress run to whatever day the
/// arithmetic produced would be worse than omitting it.
fn daily_points(today: Date, days: u32, counts: &[RunStatusCount]) -> Vec<DailyStatusPoint> {
    let span = i64::from(days) - 1;
    let start = today - Duration::days(span);
    let mut by_day: BTreeMap<Date, Counters> = (0..=span)
        .map(|nth| (start + Duration::days(nth), Counters::default()))
        .collect();

    for count in counts {
        let Some(at) = count.run_finished_at else {
            continue;
        };
        if let Some(counters) = by_day.get_mut(&at.to_offset(UtcOffset::UTC).date()) {
            counters.add(&count.status, count.rows);
        }
    }

    by_day
        .into_iter()
        .map(|(day, counters)| DailyStatusPoint {
            day,
            passed: counters.passed,
            failed: counters.failed,
        })
        .collect()
}

/// One window's KPI pair — the failure count and the pass rate.
///
/// A struct rather than a `(u64, Option<f64>)` because
/// [`DashboardService::stats`] computes two of them and assigns four fields from
/// them; a transposed pair of *windows* would compile, and naming the halves is
/// what makes the assignment readable at the call site.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Kpi {
    failed: u64,
    pass_rate: Option<f64>,
}

/// Fold one window's status groups into legacy's two KPI numbers.
///
/// # The denominator is the whole of this function, and it is legacy's fourth
/// # reading of `status`
///
/// Legacy's KPI query counts three things per window (`:319-335` for the current
/// one): failures are `status IN ('FAILED','ERROR')` (`:320`), the numerator is
/// `status = 'PASSED'` (`:333`), and the **denominator is
/// `status IN ('PASSED','FAILED','ERROR')`** (`:329`) — *not* every row.
///
/// So a `SKIPPED`, `PENDING`, `RUNNING`, `XFAIL` or `XPASS` row inside the window
/// is in none of the three counters, and in particular is **not** in the
/// denominator. That is the trap plan ruling R5 is about, and it is why
/// [`Counters::total`] is deliberately unused here:
///
/// | | numerator | denominator |
/// |---|---|---|
/// | [`classify`]'s five-counter rule | `passed` | `total` = **every** row (`COUNT(tr.id)`) |
/// | this KPI rule | `passed` | `passed + failed` |
///
/// Reusing `total` would deflate every pass rate on the dashboard by however many
/// tests a plan skips, silently, on a screen that already renders. Pinned by
/// `the_kpi_denominator_counts_only_passed_and_failed_rows`.
///
/// # What it agrees with, so the reuse is stated rather than assumed
///
/// On the two counters it *does* have, this rule is [`classify`] exactly —
/// `PASSED` alone, `FAILED` with `ERROR` folded in — which is also the daily
/// trend's pair (`:218-219`, [`daily_points`]). So [`Counters`] is reused and
/// **two** of its four fields are read, `passed` and `failed`; `skipped` is
/// simply not a KPI counter, and `total` is the one legacy deliberately does not
/// use here. `bucketize_status` (`analytics.rs:1940-1946`) would give the same two
/// counters and is still not reused: it buckets `SKIPPED` to `NOT_RUN`, which is
/// invisible while the denominator excludes both and would become a divergence
/// the moment a caller wanted a skipped count off this fold.
///
/// # `None` rather than `0.0` for an empty window
///
/// Legacy guards each rate with `if row.total_24h > 0` (`:356`, `:359`) and
/// leaves the field at its `None` otherwise, which is why
/// `DashboardStats::pass_rate_24h` is an `Option<f64>`: a deployment with no runs
/// in the last day reports "no data", not "0% passed". A window whose rows are
/// *all* failures is a different thing and does report `Some(0.0)`.
///
/// The value is a **ratio in `[0, 1]`**, not a percentage — legacy divides and
/// does not multiply (`:357`).
#[allow(
    clippy::cast_precision_loss,
    reason = "legacy divides two i64 counts as f64 (manager/src/routes/\
              dashboard.rs:357); a count above 2^53 is not a test window, and a \
              fallible conversion here has no failure a dashboard could act on. \
              Same trade `domain::analytics::aggregates::summarize` records"
)]
fn kpi_of(counts: &[StatusRowCount]) -> Kpi {
    let mut counters = Counters::default();
    for count in counts {
        counters.add(&count.status, count.rows);
    }

    // Legacy's `total_24h`, which is *not* `Counters::total`. See this
    // function's doc.
    let counted = counters.passed.saturating_add(counters.failed);
    Kpi {
        failed: counters.failed,
        pass_rate: (counted > 0).then(|| counters.passed as f64 / counted as f64),
    }
}

/// Project one result row into a recent-failure card.
///
/// Legacy selects eight columns and maps them one to one (`:264-272` for the
/// projection, `:287-296` for the mapping). Three of the nine fields here are
/// not a plain copy:
///
/// * **`test_file` is `None` when the column is `""`.** Legacy's column is
///   nullable and its card is `Option<String>`; this schema collapsed absent to
///   `""` so that "absent" has one spelling
///   (`qa_insights_sdk::TestResultRecord`'s divergence 3), and this is where the
///   two spellings meet again. Emitting `Some("")` would put an empty path on a
///   card that legacy would have left blank.
/// * **`finished_at` is the *effective* instant, not the run's finish.** Legacy
///   selects `COALESCE(rr.finished_at, rr.created_at) AS finished_at` (`:270`),
///   so the card's instant is the same expression the window filters on. A card
///   for a run still in progress carries the **run's** creation instant —
///   `run_created_at`, not the row's `created_at`, which is when this row was
///   last written and moves on every re-ingest of the run
///   (`qa_insights_sdk::TestResultRecord::run_created_at` tabulates the two).
///   Reading the wrong one would put a time on the card that the window it was
///   selected by does not contain.
/// * **`run_id` is legacy's `workflow_name`, and the plan pair is legacy's
///   `plan_id`.** Both substitutions are `qa_insights_sdk::FailedTestCard`'s and
///   that type documents them; nothing is decided here.
///
/// `jira_key` **is** this query's own column in legacy — `tr.jira_key` off
/// `test_results` (`:271`), not a join and not a later enrichment pass — and
/// `qa_test_results` carries the same column, denormalized by ingest from
/// `qa_runs_sdk::RunTestResult`. So it is filled, and it is *not* waiting on
/// Phase C's JIRA registry: `qa_insights_sdk::TestResultRecord::jira_key`
/// records that the runner-reported file-level key and `JiraBug` are two
/// different things.
///
/// Pure and taking the row by value, so the nine copies are pinned by
/// `a_failure_card_carries_every_column_of_its_row` with a distinct fixture value
/// per field — three `Uuid`s and four `String`s that a transposition would
/// otherwise pass straight through.
fn failure_card(row: TestResultRecord) -> FailedTestCard {
    FailedTestCard {
        test_name: row.test_name,
        test_file: (!row.test_file.is_empty()).then_some(row.test_file),
        run_id: row.run_id,
        repo_id: row.repo_id,
        plan_path: row.plan_path,
        environment_id: row.environment_id,
        finished_at: row.run_finished_at.or(row.run_created_at),
        jira_key: row.jira_key,
        launch_id: row.launch_id,
    }
}

/// Project one flaky group into its card.
///
/// Legacy maps its `FlakyRow` to a `FlakyTestCard` field for field
/// (`manager/src/routes/dashboard.rs:406-416`), and the three counters are copied
/// rather than recomputed: they are legacy's three selected columns
/// (`:386-388`), and `total` is a **rendered number** rather than a derived one —
/// `crate::domain::repos::FlakyGroup::total` argues that. Two of the seven fields
/// are not a plain copy:
///
/// * **`test_file` is `None` when the aggregate is `""`.** Legacy's column is
///   nullable and `MAX` skips `NULL`s, so its card carries `None` for a group no
///   row of which named a file; this schema collapsed absent to `""` so that
///   "absent" has one spelling (`qa_insights_sdk::TestResultRecord`'s divergence
///   3), and this is where the two spellings meet again — the same meeting point
///   [`failure_card`] documents, for the same column and by the same expression.
/// * **The plan pair is legacy's `plan_id`.** `qa_insights_sdk::FlakyTestCard`
///   documents the substitution and that crate's note 1 argues it; nothing is
///   decided here. Legacy's field is a `String` and non-optional because its
///   `run_results.plan_id` is `TEXT NOT NULL`
///   (`manager/migrations/001_initial.sql:53`); both halves are nullable on
///   `qa_test_results`, so a group whose run named no plan carries `None` twice
///   rather than an invented slug.
///
/// Legacy's `r.passed.max(0) as usize` clamp (`:412-414`) has no counterpart
/// here and needs none: the counters are already `u64` by the time they leave
/// `infra::storage::results_sea_repo`, whose `FlakyGroupRow::into_domain` is
/// where that clamp lives.
///
/// Pure and taking the group by value, so the seven copies are pinned by
/// `a_flaky_card_carries_every_field_of_its_group` with a distinct fixture value
/// per field. **This one struct carries five transposition hazards**, which is
/// the family this crate has now been caught by three times: the three counters
/// are all `u64`, `repo_id` is the only `Uuid` and is safe, `test_file` and
/// `plan_path` are both `Option<String>` on the card, and `test_name` and
/// `test_file` are both `String` on the group — so `test_name: group.test_file`
/// compiles too.
fn flaky_card(group: FlakyGroup) -> FlakyTestCard {
    FlakyTestCard {
        test_name: group.test_name,
        test_file: (!group.test_file.is_empty()).then_some(group.test_file),
        repo_id: group.repo_id,
        plan_path: group.plan_path,
        passed: group.passed,
        failed: group.failed,
        total: group.total,
    }
}

/// One vector's three running counters plus its distinct-file set.
///
/// A named alias because `clippy::type_complexity` denies the tuple inline, and
/// the name is worth having anyway: three of the four members are same-typed
/// counters and a reader needs their order — `passed`, `failed`, `total`, exactly
/// as legacy's `agg` tuple orders them (`manager/src/routes/dashboard.rs:509`,
/// `:521-523`).
type VectorAccumulator<'a> = (u64, u64, u64, BTreeSet<(Uuid, &'a str)>);

/// `repo_id -> normalized file -> folded vector -> display spelling`, the join
/// table [`quality_vector_pass_rates`] builds over the universe.
///
/// A named alias for the same reason [`VectorAccumulator`] is one:
/// `clippy::type_complexity` denies the three-deep map inline. Nested rather
/// than a `(Uuid, &str)` tuple key — see the comment where it is built.
type FilesByRepo<'a> = BTreeMap<Uuid, BTreeMap<&'a str, BTreeMap<String, &'a str>>>;

/// Legacy's quality-vector pass rate — the **join** half of
/// `DashboardStats::quality_vectors_pass_rate`.
///
/// `manager/src/routes/dashboard.rs:501-538`. The SQL half is
/// [`crate::domain::repos::ResultsRepository::file_status_counts`], and the two
/// meet on the test file.
///
/// # Legacy reads `TEST_META` off disk here; this reads the catalog's projection
///
/// Legacy calls `collect_quality_vectors_by_file` (`analytics.rs:1991-2007`),
/// which walks every plan, resolves every entry to a file in a git working copy
/// and parses `TEST_META` out of it — behind a process-global 1800-second TTL
/// cache, because the walk is hundreds of small file reads. ADR-0005 confines git
/// egress to qa-catalog, so in this architecture the vectors are a field of that
/// gear's own projection ([`UniverseTest::quality_vectors`]) and arrive over
/// [`CatalogReader::list_universe`](crate::domain::ports::CatalogReader::list_universe).
/// There is nothing to cache here and nothing to invalidate.
///
/// **The other quality-vector reader is not this one.**
/// [`crate::domain::analytics::aggregates::build_quality_vector_summary`] is the
/// *analytics overview's* fold: it counts **files per vector** off the universe
/// alone, has an `unclassified_tests` residue, and folds case variants **across**
/// files. This one divides executions and does none of those three. Legacy has
/// two folds of this name and they agree on nothing but the word — the same
/// relation `flaky_card` has to `build_flaky`.
///
/// # Five properties, and three of them look like defects
///
/// 1. **A file fans out to every vector it declares** (`:519-525`), so its
///    counters are added once per vector and the sums across the returned `Vec`
///    exceed the row count. That is what makes the vectors a partition of
///    concerns rather than of files.
/// 2. **`tests` is distinct *files*, not executions** — a `HashSet` of the
///    normalized path (`:509`, `:524`, `:534`).
/// 3. **A counted file the universe does not know is dropped** (`:516-518`), and
///    a file that declares no vector contributes to nothing. Neither produces a
///    residue bucket: legacy's `QualityVectorPassRate` has five fields and none
///    of them is one.
/// 4. **The row's path is normalized and the universe's is not.** Legacy spells
///    the closure inline (`:503-508`) and it is
///    [`normalize_test_path`](crate::domain::analytics::universe::normalize_test_path)
///    character for character; the universe side needs none because
///    [`UniverseTest::test_file`] arrives normalized, which that field's own doc
///    states. Reused rather than re-spelled — a second copy of that rule is a
///    second thing to keep in step, and it carries a known quirk (a Windows-style
///    `.\tests\a.py` survives as `./tests/a.py`) that must not be fixed in one
///    copy only.
/// 5. **Two *different files* spelling one vector differently are two rows.**
///    `agg` is keyed on the **display string** (`:520`), and the case-folded dedup
///    that produced that string ran per file (`analytics.rs:2053-2055`). So `Security`
///    from one file and `security` from another sum separately — where
///    [`build_quality_vector_summary`](crate::domain::analytics::aggregates::build_quality_vector_summary)
///    merges them, because *its* legacy original folds across files
///    (`analytics.rs:1060-1062`). Ported verbatim under Phase B's standing
///    instruction and pinned by
///    `two_files_spelling_a_vector_differently_are_two_dashboard_rows`, because
///    an implementation that "fixed" it changes a rendered number.
///
/// # Determinism, where legacy has none
///
/// Legacy's `vectors_by_file` is a `HashMap` and so is each file's vector bucket,
/// so *which* spelling of a case-variant vector wins is undefined between two runs
/// over identical data. This fold walks files in path order and each file's
/// vectors in case-folded order, which fixes the winner without changing any
/// count — the same refinement
/// [`build_quality_vector_summary`](crate::domain::analytics::aggregates::build_quality_vector_summary)
/// made, recorded the same way. The output order is legacy's own: `total`
/// descending (`:537`) over a `BTreeMap` drained ascending, and `Vec::sort_by` is
/// stable, so ties break ascending by vector name.
fn quality_vector_pass_rates(
    counts: &[FileStatusCount],
    universe: &[UniverseTest],
) -> Vec<QualityVectorPassRate> {
    // `repo_id -> normalized file -> folded vector -> display spelling`, which
    // is legacy's `build_quality_vectors_by_file` (`analytics.rs:2016-2068`)
    // over the catalog's projection instead of over the disk. Re-keyed on the
    // file rather than iterating `universe`, because a file listed by two
    // plans **within one repo** is two universe entries and one vector test —
    // `a_file_in_two_universe_entries_is_one_vector_test`.
    //
    // `repo_id` is the outer key — a fix-round-2 correction. Two repositories
    // can share a `test_file`, and a bare-path key merged their buckets, so
    // one repo's untagged file inherited the other's `quality_vectors` and the
    // counted rows below folded both repositories' outcomes into the same
    // vector — `a_cross_repo_path_collision_does_not_mix_vector_pass_rates`.
    // It is a separate map level rather than a `(repo_id, &str)` tuple key:
    // the join below constructs its lookup key from a normalized `String`
    // local, and a tuple key would force that local's borrow to outlive the
    // function, which does not typecheck; nesting keeps the inner map's
    // key exactly `&str`, matched by the same `Borrow<str>` lookup the
    // pre-existing code already relied on.
    // `plan_path` is deliberately still absent: it plays no part in the
    // cross-repository collision, and adding it would split the same-repo,
    // two-plan case the comment above exists to keep collapsed.
    let mut by_file: FilesByRepo<'_> = BTreeMap::new();
    for test in universe {
        let bucket = by_file
            .entry(test.repo_id)
            .or_default()
            .entry(test.test_file.as_str())
            .or_default();
        for vector in &test.quality_vectors {
            let trimmed = vector.trim();
            if trimmed.is_empty() {
                continue;
            }
            // First spelling seen wins, case-insensitively (`:2053-2055`).
            bucket
                .entry(trimmed.to_ascii_lowercase())
                .or_insert(trimmed);
        }
    }

    // Keyed on the **display** string, which is property 5 above and not an
    // oversight: legacy's `agg.entry(vector.clone())` (`:520`).
    let mut agg: BTreeMap<&str, VectorAccumulator<'_>> = BTreeMap::new();
    for count in counts {
        // Skips every row with no `repo_id` — fix-round-3's second correction,
        // and a rendered-number change, not a tautology. `count.repo_id` is
        // `None` iff the run's target was `RunTarget::CustomPlan`
        // (`plan_identity`, `ingest.rs:597-604`, returns `(None, None)`
        // together), so before this a `CustomPlan` run's rows *did* join here:
        // `normalize_test_path(&count.test_file)` still matches a universe
        // path, and whichever repository happened to declare that path
        // absorbed the run's counters into its vectors, joined by bare path
        // alone. That is exactly the collision
        // `a_cross_repo_path_collision_does_not_mix_vector_pass_rates` exists
        // to rule out elsewhere in this fold, so it is closed here too rather
        // than left as this one join's exception. It also brings this fold in
        // line with the overview's: [`ResultsRepository::file_status_counts`]'s
        // `universe_condition` filters on `(repo_id, plan_path)` pairs
        // (`results_sea_repo.rs:397-411`), which a `CustomPlan` run likewise
        // never has one of. A row without a `repo_id` now contributes to
        // nothing, the same as a row whose file the universe does not know.
        let Some(repo_id) = count.repo_id else {
            continue;
        };
        let Some(by_repo) = by_file.get(&repo_id) else {
            continue;
        };
        let normalized = normalize_test_path(&count.test_file);
        // `get_key_value` rather than `get`: both the vector spellings and the
        // file inserted into the distinct-file set have to outlive `normalized`,
        // which is a local. Same string either way — the lookup succeeded on
        // equality — so this is a lifetime move and not a semantic one.
        let Some((file, vectors)) = by_repo.get_key_value(normalized.as_str()) else {
            continue;
        };
        for display in vectors.values() {
            let entry = agg.entry(display).or_default();
            entry.0 += count.passed;
            entry.1 += count.failed;
            entry.2 += count.total;
            // The *file*, not the row: two stored spellings of one path are one
            // `tests` — `the_stored_path_is_normalized_before_it_is_matched`.
            // Keyed on `(repo_id, file)`, not just `file`: two repositories can
            // share a path, and each is its own distinct file. A bare-path key
            // here would re-collapse the two repositories that `by_file`'s own
            // `(repo_id, file)` nesting above keeps apart, undoing that fix for
            // this one field while `passed`/`failed`/`total` stayed correct —
            // `two_repositories_sharing_a_test_file_and_vector_are_two_tests`,
            // which the analytics overview's `build_quality_vector_summary`
            // agrees with over the same universe (`aggregates.rs:1721`).
            entry.3.insert((repo_id, *file));
        }
    }

    let mut rates: Vec<QualityVectorPassRate> = agg
        .into_iter()
        .map(
            |(vector, (passed, failed, total, tests))| QualityVectorPassRate {
                vector: vector.to_owned(),
                passed,
                failed,
                total,
                // `usize` to `u64`, and the count is bounded by the universe.
                tests: u64::try_from(tests.len()).unwrap_or(u64::MAX),
            },
        )
        .collect();
    // Legacy's `sort_by(|a, b| b.total.cmp(&a.total))` (`:537`) — descending, and
    // *stable*, so the `BTreeMap`'s ascending vector order is the tiebreak.
    // `Reverse` rather than a comparator, which is the same order and what
    // `clippy::sort_by_key` asks for; `sort_by_key` is stable, so the tiebreak is
    // unaffected.
    rates.sort_by_key(|rate| Reverse(rate.total));
    rates
}

/// Legacy's rendered duration text.
///
/// `manager/src/services/argo.rs:2406-2417` exactly: `"{m}m {s}s"` once a whole
/// minute has passed and `"{s}s"` below that, and `None` unless **both** instants
/// are present (`start?`, `end?` there). So a run still going has no duration text
/// at all, which is why `ActiveRunsCard` computes elapsed time from `started_at`
/// instead — see [`qa_insights_sdk::DashboardRun::duration`].
///
/// A negative span (a clock that went backwards between the two writes) renders as
/// negative seconds, as it does in legacy. Not clamped, because a `0s` would hide
/// the skew that produced it.
#[allow(
    clippy::integer_division,
    reason = "legacy divides whole seconds by 60 and prints the remainder \
              (manager/src/services/argo.rs:2410-2411); a float here would render \
              a different string"
)]
fn format_duration(
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
) -> Option<String> {
    let secs = (finished_at? - started_at?).whole_seconds();
    let mins = secs / 60;
    if mins > 0 {
        Some(format!("{mins}m {}s", secs % 60))
    } else {
        Some(format!("{secs}s"))
    }
}

/// The dashboard's four counters over a set of `(status, rows)` groups.
///
/// [`classify`] is the fold this reuses; this module's header carries the
/// row-by-row verification that the dashboard's SQL and `classify`'s
/// five-counter rule agree, and that the analytics `bucketize_status` does not.
#[derive(Clone, Copy, Debug, Default)]
struct Counters {
    /// `COUNT(tr.id)` — **every** row, so not the sum of the other three.
    total: u64,
    passed: u64,
    /// `FAILED` and `ERROR` together.
    failed: u64,
    skipped: u64,
}

impl Counters {
    /// Add one group's rows.
    ///
    /// `saturating_add` rather than `+`: the inputs are database counts and an
    /// overflow would be a panic on a read-only endpoint. Unreachable at any
    /// plausible row count, and cheaper than being sure.
    fn add(&mut self, status: &str, rows: u64) {
        self.total = self.total.saturating_add(rows);
        match classify(status) {
            StatusBucket::Passed => self.passed = self.passed.saturating_add(rows),
            StatusBucket::Failed => self.failed = self.failed.saturating_add(rows),
            StatusBucket::Skipped => self.skipped = self.skipped.saturating_add(rows),
            // In `total` and in nothing else, which is exactly what legacy's SQL
            // does with a `PENDING`, `RUNNING`, `XFAIL` or `XPASS` row: it is in
            // `COUNT(tr.id)` and in none of the three `FILTER`s. Named rather
            // than a `_` arm so a new `StatusBucket` variant stops the build
            // here, where the decision belongs.
            StatusBucket::InProgress | StatusBucket::Uncounted => {}
        }
    }
}

/// A count off the one bounded listing, as a `u64`.
///
/// Saturating rather than fallible: the page is capped at [`RUN_PAGE`], so this
/// cannot fail on any target this ships on, and a dashboard that refused to
/// render because a counter did not fit would be worse than one showing a
/// nonsense number. A function rather than an inline conversion because the
/// workspace denies `as` casts.
fn count_of(runs: usize) -> u64 {
    u64::try_from(runs).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "dashboard_tests.rs"]
mod dashboard_tests;
