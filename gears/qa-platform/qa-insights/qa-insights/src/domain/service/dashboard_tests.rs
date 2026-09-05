//! Tests for the dashboard aggregate.
//!
//! Two tiers, deliberately:
//!
//! * The **folds** — the day window, the status counters, the active predicate,
//!   the day bucketing — are pure functions and are driven directly. They are
//!   where legacy's rules live, so they are where the ported rules are pinned.
//! * The **service** runs against the *real* repository on in-memory `SQLite`
//!   and the real transaction provider, with only the PDP and qa-runs doubled —
//!   the same shape as `results_tests` and `reconcile_tests`, and for the same
//!   reason: the property that matters is that the scope the PDP compiles is the
//!   scope the aggregates run under, and a repository double would absorb
//!   exactly that.
//!
//! # The three tests the plan mandates, and where they are
//!
//! Plan Task 18 Step 1 gives three tests as pseudo-code against helpers named
//! `resolve_days`, `run_trend_point`, `runs_in_phases` and `summarize`. Their
//! assertions are kept exactly;
//! [`the_day_window_defaults_to_fourteen_and_clamps_to_three_and_ninety`],
//! [`error_counts_as_failed_in_the_run_trend`] and
//! [`the_active_list_is_capped_at_ten_and_matches_the_count_predicate`] are
//! those three. Two of the four helper names could not survive as written:
//! `runs_in_phases` takes Argo *phase strings*, and this architecture has no
//! phase — [`runs_in_states`] takes `RunState`, which is the whole subject of
//! [`only_dispatching_and_running_runs_are_active`]. And `summarize(&runs, 14)`
//! took a day count the run listing does not use: legacy's listing is not
//! windowed by `days` (`manager/src/routes/dashboard.rs:129-158` derives every
//! run number before the window is used at `:216-231`), so
//! [`super::summarize_runs`] takes only the runs and the parameter is not
//! carried forward as an ignored argument.
//!
//! # The two tests Task 19 mandates, and why one of them cannot be written
//!
//! Plan Task 19 Step 1 asks for "one test asserting the grouping, one asserting a
//! build with no runs is represented the way legacy represents it". The second is
//! [`the_coverage_view_omits_every_build_because_nothing_reports_a_summary`], and
//! it answers the question the plan told this task to check: **absent**.
//!
//! The first cannot be written, and that is a finding rather than an omission.
//! Legacy groups coverage by `product_key` (`manager/src/routes/dashboard.rs:612-621`)
//! and `qa_runs_sdk::Run` carries no product key at all, so there is no grouping
//! in this port to assert over — and no coverage point to group either, for the
//! separate reason [`super::DashboardService::coverage`] gives. A test written
//! anyway would have to assert over a grouping this task invented, which is the
//! one thing the legacy-verification protocol forbids.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use qa_insights_sdk::{
    DashboardRun, DashboardStats, PlatformsSummary, QualityVectorPassRate, RunTestTrendPoint,
    TestResultRecord,
};
use qa_runs_sdk::{Run, RunState};
use time::macros::{date, datetime};
use time::{Duration, OffsetDateTime};
use toolkit_db::DBProvider;
use uuid::Uuid;

use super::{
    DashboardService, RUN_PAGE, daily_points, dashboard_run, failure_card, flaky_card,
    format_duration, is_active, kpi_of, quality_vector_pass_rates, resolve_days, run_trend_points,
    summarize_runs, window_start,
};
use crate::domain::error::DomainError;
use crate::domain::repos::{
    FileStatusCount, FlakyGroup, NewTestResult, ResultsRepository, RunStatusCount, StatusRowCount,
};
use crate::domain::service::test_support::{
    DEFAULT_BRANCH, DenyAllAuthZ, FakeCatalog, FakeRuns, RecordingAuthZ, ResourceConstrainedAuthZ,
    TenantScopedAuthZ, ctx, run_in_state, ts, universe_test,
};
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::test_db::{inmem_db, scope};

const TENANT: Uuid = Uuid::from_u128(0x0A);
const OTHER_TENANT: Uuid = Uuid::from_u128(0x0B);

// ---------------------------------------------------------------------------
// The folds
// ---------------------------------------------------------------------------

/// Legacy's window: default 14 days, clamped to `[3, 90]`
/// (`manager/src/routes/dashboard.rs:104`, the `days` binding in
/// `api_dashboard`: `query.days.unwrap_or(14).clamp(3, 90)`).
#[test]
fn the_day_window_defaults_to_fourteen_and_clamps_to_three_and_ninety() {
    assert_eq!(resolve_days(None), 14);
    assert_eq!(resolve_days(Some(1)), 3);
    assert_eq!(resolve_days(Some(365)), 90);
    assert_eq!(resolve_days(Some(30)), 30);
}

/// `failed` folds `ERROR` in; `skipped` does not. Ported from the counter SQL in
/// `api_dashboard` (`manager/src/routes/dashboard.rs:171-173`,
/// `FILTER (WHERE tr.status IN ('FAILED','ERROR'))`).
///
/// `tests_total` is a plain `COUNT` (`:170`, `COUNT(tr.id)`), so it is **not**
/// the sum of the other three: `passed + failed + skipped` here happens to be 4
/// only because every status in the fixture is one legacy counts.
#[test]
fn error_counts_as_failed_in_the_run_trend() {
    let point = run_trend_point(&["PASSED", "ERROR", "FAILED", "SKIPPED"]);
    assert_eq!(point.passed, 1);
    assert_eq!(point.failed, 2);
    assert_eq!(point.skipped, 1);
    assert_eq!(point.tests_total, 4);
}

/// The active list is capped at 10 and uses the same predicate as the count, so
/// a dashboard showing "23 active" never lists more than ten of them.
///
/// Legacy: `manager/src/routes/dashboard.rs:148-157` — the count and the list
/// are two folds over one listing with the identical `filter`, and only the list
/// carries `.take(10)`.
#[test]
fn the_active_list_is_capped_at_ten_and_matches_the_count_predicate() {
    let runs = runs_in_states(&[RunState::Running; 23]);
    let stats = summarize_runs(&runs);
    assert_eq!(stats.active_runs, 23);
    assert_eq!(stats.active_runs_list.len(), 10);
}

/// **The `RunState` → "active" mapping, over every variant.**
///
/// Legacy's predicate is `run.phase == "Running" || run.phase == "Pending"`
/// (`manager/src/routes/dashboard.rs:150`, `:154`) over Argo workflow phases,
/// and this architecture has no phase. The mapping is
/// `Pending → Dispatching`, `Running → Running`, and nothing else:
///
/// * Argo `Pending` is a Workflow that has been **submitted** and whose pods
///   have not started. Legacy stamps exactly that phase on the run object it
///   returns from a successful submit (`manager/src/services/argo.rs:306`,
///   `submitted_run`) and defaults an absent Argo phase to it
///   (`:2273-2275`). `RunState::Dispatching` is the same moment here — the
///   state machine is `created → queued? → dispatching → running → terminal`
///   (`qa-runs/src/domain/state_machine.rs:108`) and its `Dispatching` arm's
///   own comment is "The executor accepted the run" (`:234`).
/// * `Queued` is **not** active, and that is legacy's answer too rather than a
///   simplification: legacy's queue rows live in `run_queue`, whose service
///   never writes `run_results` (grep over
///   `manager/src/services/run_queue.rs`: zero occurrences), and a queued launch
///   has no Workflow — so it appears in neither half of
///   `list_runs_with_history` and is counted by nothing on that dashboard.
/// * `Created` is likewise not active: it is the instant before a run is either
///   queued or dispatched (`state_machine.rs:223-226`), and legacy has no
///   object at all at that point.
/// * The six terminal states (`state_machine.rs:52-59`) are named there rather
///   than counted, and none of them is active.
///
/// Asserted variant by variant rather than by counting the actives, because a
/// count would still pass if two variants swapped.
#[test]
fn only_dispatching_and_running_runs_are_active() {
    for (state, expected) in [
        (RunState::Created, false),
        (RunState::Queued, false),
        (RunState::Dispatching, true),
        (RunState::Running, true),
        (RunState::Succeeded, false),
        (RunState::Failed, false),
        (RunState::Canceled, false),
        (RunState::TimedOut, false),
        (RunState::Expired, false),
        (RunState::Error, false),
    ] {
        assert_eq!(
            is_active(state),
            expected,
            "{state:?} is {}active",
            if expected { "not " } else { "" },
        );
    }
}

/// **Queued runs are their own count, off the same listing.**
///
/// `cpt-cf-qa-fr-insights-dashboard` (`docs/PRD.md:579`) requires "active **and
/// queued** runs" — one clause of a requirement discharged across plan Tasks 18,
/// 19, 21 and 23, not by this endpoint alone. Legacy's dashboard does not report a
/// queued count at all. The count is
/// a `RunState::Queued` fold over the page
/// [`super::DashboardService::stats`] has already fetched — not a second
/// `list_queue` call — for the reason legacy states at
/// `manager/src/routes/dashboard.rs:143-146`.
///
/// The mixed fixture is the point: a queued run must move `queued_runs` and
/// **not** `active_runs`, and a dispatching one the other way round, so a
/// predicate that folded the two together fails here rather than looking
/// plausible.
#[test]
fn a_queued_run_is_counted_separately_from_an_active_one() {
    let runs = runs_in_states(&[
        RunState::Queued,
        RunState::Queued,
        RunState::Dispatching,
        RunState::Running,
        RunState::Created,
        RunState::Succeeded,
    ]);
    let stats = summarize_runs(&runs);

    assert_eq!(stats.queued_runs, 2);
    assert_eq!(stats.active_runs, 2, "dispatching + running");
    assert_eq!(
        stats.active_runs_list.len(),
        2,
        "the list is the same predicate as the count",
    );
    assert_eq!(
        stats.recent_runs.len(),
        6,
        "the recent list is every run on the page, whatever its state",
    );
}

/// The recent list is the **first** ten of the listing, capped and **unsorted**.
///
/// Legacy: `recent_runs = runs.into_iter().take(10)`
/// (`manager/src/routes/dashboard.rs:158`) over a newest-first listing — so the
/// fold takes a prefix and does no ordering of its own. That is the property
/// [`super::summarize_runs`]' doc claims ("the page's order is preserved, never
/// re-sorted"), and it is qa-runs' contract to decide: re-deriving the order here
/// would make "the ten most recent" mean whatever this gear's tiebreak happened
/// to be.
///
/// **The fixture's ages are deliberately not monotonic**, which is what makes
/// this a test of order preservation rather than only of the cap. Two positions
/// are swapped, so the newest run (age 0) sits at index 11 and is *excluded*: a
/// fold that sorted newest-first would answer with a different ten, and one that
/// sorted oldest-first with the reverse. An earlier version fed strictly
/// increasing ages, where "the first ten" and "the ten newest" are the same set
/// and a re-sorting fold passed unchanged.
#[test]
fn the_recent_list_is_the_first_ten_of_the_listing_unsorted() {
    let mut runs = runs_in_states(&[RunState::Succeeded; 12]);
    // Age 0 (the newest) to the back, and age 9 forward past age 3.
    runs.swap(0, 11);
    runs.swap(3, 9);
    let stats = summarize_runs(&runs);

    assert_eq!(stats.recent_runs.len(), 10);
    assert_eq!(
        stats
            .recent_runs
            .iter()
            .map(|r| r.run_id)
            .collect::<Vec<_>>(),
        runs.iter().take(10).map(|r| r.id).collect::<Vec<_>>(),
        "the page's order is preserved, including the two swapped positions",
    );
    assert!(
        !stats.recent_runs.iter().any(|r| r.run_id == runs[11].id),
        "the newest run was swapped past the cap and must not be listed; a fold \
         that sorted by recency would put it back",
    );
}

/// A run whose results are not ingested is a **zero** point, not a missing one.
///
/// Legacy reaches the same answer from the other side: its `LEFT JOIN` returns
/// the run with `COUNT(tr.id) = 0`, and its consumer reads a missing map entry
/// as zeros anyway (`manager/src/routes/dashboard.rs:196-212`,
/// `counts.map(...).unwrap_or(0)`). So the trend has one point per recent run
/// either way, which is what keeps the chart's x-axis the run list.
#[test]
fn a_run_with_no_ingested_results_is_a_zero_point_in_the_trend() {
    let ingested = run_in_state(RunState::Succeeded, 0);
    let not_ingested = run_in_state(RunState::Succeeded, 1);
    let recent = [dashboard_run(&ingested), dashboard_run(&not_ingested)];
    let counts = [RunStatusCount {
        run_id: ingested.id,
        status: "PASSED".to_owned(),
        rows: 3,
        run_finished_at: Some(ts()),
    }];

    let points = run_trend_points(&recent, &counts);
    assert_eq!(
        points.iter().map(|p| p.run_id).collect::<Vec<_>>(),
        vec![ingested.id, not_ingested.id],
        "one point per recent run, in the recent list's order",
    );
    assert_eq!(points[0].tests_total, 3);
    assert_eq!(points[0].passed, 3);
    assert_eq!(
        points[1],
        RunTestTrendPoint {
            run_id: not_ingested.id,
            run_name: not_ingested.name.clone(),
            started_at: not_ingested.started_at,
            ..RunTestTrendPoint::default()
        }
    );
}

/// **Every day in the window gets a point, including the days with no runs.**
///
/// Legacy builds the axis with
/// `generate_series($1::date, CURRENT_DATE, INTERVAL '1 day')` and `LEFT JOIN`s
/// the runs onto it (`manager/src/routes/dashboard.rs:220`), so an idle day is a
/// zero and not a gap — a chart drawn from gaps would silently compress its own
/// x-axis. `start_day = today - (days - 1)` (`:105`), so the series holds
/// exactly `days` points, ascending, ending today.
#[test]
fn every_day_of_the_window_is_a_point_even_when_nothing_ran() {
    let points = daily_points(date!(2026 - 08 - 18), 3, &[]);

    assert_eq!(
        points.iter().map(|p| p.day).collect::<Vec<_>>(),
        vec![
            date!(2026 - 08 - 16),
            date!(2026 - 08 - 17),
            date!(2026 - 08 - 18),
        ],
        "three ascending days ending today",
    );
    assert!(points.iter().all(|p| p.passed == 0 && p.failed == 0));
}

/// The daily trend folds `ERROR` into `failed` and counts nothing else.
///
/// Legacy's daily query has **two** counters, not four
/// (`manager/src/routes/dashboard.rs:218-219`): `PASSED`, and
/// `IN ('FAILED','ERROR')`. A `SKIPPED` row moves neither, which is a different
/// answer from the per-run trend's — where `skipped` has a column of its own —
/// and is why this is a separate fold rather than a projection of that one.
///
/// The rows are attributed by their run's finish instant, and a run outside the
/// window is dropped rather than folded into the nearest day.
#[test]
fn the_daily_trend_folds_error_into_failed_and_counts_no_skips() {
    let on_the_seventeenth = OffsetDateTime::new_utc(date!(2026 - 08 - 17), ts().time());
    let before_the_window = OffsetDateTime::new_utc(date!(2026 - 08 - 01), ts().time());
    let run = Uuid::from_u128(0x51);
    let old_run = Uuid::from_u128(0x52);

    let counts: Vec<RunStatusCount> = [
        (run, "PASSED", 4, on_the_seventeenth),
        (run, "ERROR", 1, on_the_seventeenth),
        (run, "FAILED", 2, on_the_seventeenth),
        (run, "SKIPPED", 9, on_the_seventeenth),
        (run, "XPASS", 7, on_the_seventeenth),
        (old_run, "FAILED", 5, before_the_window),
    ]
    .into_iter()
    .map(|(run_id, status, rows, at)| RunStatusCount {
        run_id,
        status: status.to_owned(),
        rows,
        run_finished_at: Some(at),
    })
    .collect();

    let points = daily_points(date!(2026 - 08 - 18), 3, &counts);
    let seventeenth = points
        .iter()
        .find(|p| p.day == date!(2026 - 08 - 17))
        .unwrap();

    assert_eq!(seventeenth.passed, 4);
    assert_eq!(seventeenth.failed, 3, "FAILED + ERROR");
    assert!(
        points
            .iter()
            .filter(|p| p.day != date!(2026 - 08 - 17))
            .all(|p| p.passed == 0 && p.failed == 0),
        "the run outside the window must not land on any day: {points:?}",
    );
}

/// **The window opens at midnight UTC of its first day**, not at the current
/// instant `days - 1` days ago.
///
/// Legacy binds `start_day = Utc::now().date_naive() - Duration::days(days - 1)`
/// as a `date` (`manager/src/routes/dashboard.rs:105`) and compares it against
/// `DATE(COALESCE(...))` (`:222`), so the first day is included whole. A bound
/// taken from `now() - days` instead would silently drop the part of that day
/// before the request's clock time — a wrong number that moves through the day
/// and is right at midnight, which is the shape of bug nobody diagnoses.
///
/// The `- 1` is the other half: three days ending on the 18th opens on the 16th,
/// not the 15th, and `days = 1` would mean "today" rather than "today and
/// yesterday".
///
/// This is the one fold with no coverage from the tests above — they call
/// [`super::daily_points`] with a `today` directly, and the service tests stamp
/// their rows with `now()` — so it gets its own.
#[test]
fn the_window_opens_at_midnight_utc_of_its_first_day() {
    assert_eq!(
        window_start(date!(2026 - 08 - 18), 3),
        datetime!(2026-08-16 00:00:00 UTC),
    );
    assert_eq!(
        window_start(date!(2026 - 08 - 18), 1),
        datetime!(2026-08-18 00:00:00 UTC),
        "one day is today, from its own midnight",
    );
    // The default window, across a month boundary, so the arithmetic is date
    // arithmetic and not a subtraction of day-of-month.
    assert_eq!(
        window_start(date!(2026 - 09 - 05), 14),
        datetime!(2026-08-23 00:00:00 UTC),
    );
}

/// **The listing ceiling is 200 and the service asks the port for exactly it.**
///
/// Two claims in one test because they are one property split across two files.
///
/// `RUN_PAGE` is a **third** copy of this subsystem's page default — qa-runs'
/// queue contract fixes it, `infra::storage::db::PAGE_LIMITS` re-declares it, and
/// this re-declares it again because a ceiling on a cross-gear call is a domain
/// decision rather than a storage one. `PAGE_LIMITS`' copy is pinned by
/// `db::tests::the_page_limits_match_the_subsystem_convention` precisely because
/// it is a copy; this is that guard for the third one.
///
/// The second assertion is what turns
/// [`super::DashboardService::stats`]' documented bound — the active and queued
/// counts are exact only while fewer than `RUN_PAGE` runs are newer than the
/// oldest active one — from a doc claim into a pinned one. Nothing else in this
/// crate fails if the call passes a different ceiling, and a smaller one silently
/// shrinks both counts rather than erroring.
#[tokio::test]
async fn the_listing_ceiling_is_two_hundred_and_the_service_asks_for_it() {
    assert_eq!(
        RUN_PAGE, 200,
        "the subsystem page default; qa-runs' queue contract and \
         infra::storage::db::PAGE_LIMITS carry the same number",
    );

    let fx = Fixture::new().await;
    fx.service.stats(&ctx(TENANT), None).await.unwrap();

    assert_eq!(
        fx.runs.recent_limits(),
        vec![RUN_PAGE],
        "one listing per request, at exactly the documented ceiling",
    );
}

/// A group with no finish instant cannot be attributed to a day and is dropped.
///
/// Reachable only through [`super::DashboardService::stats`]'s *other* counter
/// read — the windowed one filters `run_finished_at >= since`, so it never
/// produces one — but the fold is reachable from this module and a caller that fed
/// it per-run counts would otherwise silently attribute an in-progress run to
/// whatever day the arithmetic produced.
#[test]
fn a_group_with_no_finish_instant_lands_on_no_day() {
    let counts = [RunStatusCount {
        run_id: Uuid::from_u128(0x53),
        status: "FAILED".to_owned(),
        rows: 6,
        run_finished_at: None,
    }];

    let points = daily_points(date!(2026 - 08 - 18), 3, &counts);
    assert_eq!(points.len(), 3);
    assert!(points.iter().all(|p| p.failed == 0), "{points:?}");
}

/// The duration text is legacy's, formatted from the two instants.
///
/// `manager/src/services/argo.rs:2406-2417` — `"{m}m {s}s"` once a minute has
/// passed and `"{s}s"` below that, and `None` unless **both** instants are
/// present (`start?`, `end?`). A run still going therefore has no duration text,
/// which is why `ActiveRunsCard` computes elapsed time from `started_at`
/// instead (`qa_insights_sdk::DashboardRun::duration`).
#[test]
fn the_run_duration_is_legacys_two_formats() {
    let start = ts();
    assert_eq!(
        format_duration(Some(start), Some(start + Duration::seconds(45))).as_deref(),
        Some("45s"),
    );
    assert_eq!(
        format_duration(Some(start), Some(start + Duration::seconds(125))).as_deref(),
        Some("2m 5s"),
    );
    assert_eq!(format_duration(Some(start), None), None);
    assert_eq!(format_duration(None, Some(start)), None);
}

// ---------------------------------------------------------------------------
// The 24-hour KPI folds
// ---------------------------------------------------------------------------

/// **The denominator is `PASSED` + `FAILED` + `ERROR`, and nothing else** —
/// legacy's `total_24h` (`manager/src/routes/dashboard.rs:328-331`,
/// `status IN ('PASSED','FAILED','ERROR')`), not `COUNT(tr.id)`.
///
/// This is the R5 trap for this task and the mutation this test exists for:
/// [`super::kpi_of`] reuses [`super::Counters`], which also computes
/// `Counters::total` over **every** row, and `total` is the wrong denominator. The
/// fixture makes the two answers far apart — six skipped and in-progress rows
/// against four counted ones — so `passed / total` gives `0.3` (3 of 10 rows)
/// where the rule gives `0.75` (3 of 4). Both are plausible pass rates on a
/// chart, which is why this is asserted rather than reasoned about. (This doc
/// said `0.25` until Task 21b's fix round; the fixture and the assertion were
/// always right, and the mutation was run to get the real number.)
///
/// The five statuses also pin the numerator's vocabulary: `ERROR` is a failure
/// (`:320`), `XPASS` is **not** a pass, and `SKIPPED`, `RUNNING` and `XPASS` are
/// in neither counter nor denominator.
#[test]
fn the_kpi_denominator_counts_only_passed_and_failed_rows() {
    let kpi = kpi_of(&[
        status_rows("PASSED", 3),
        status_rows("ERROR", 1),
        status_rows("SKIPPED", 4),
        status_rows("RUNNING", 1),
        status_rows("XPASS", 1),
    ]);

    assert_eq!(kpi.failed, 1, "ERROR is a failure");
    assert_eq!(
        kpi.pass_rate,
        Some(0.75),
        "3 passed over 4 counted rows; the six skipped, running and xpass rows \
         are in neither half"
    );
}

/// **`None` is "no data" and `Some(0.0)` is "everything failed", and legacy
/// distinguishes them** — the two `if total > 0` guards at
/// `manager/src/routes/dashboard.rs:356` and `:359`.
///
/// Three mutations, one per case. Dropping the guard makes the empty window
/// `0.0/0.0` — `NaN`, which serialises as `null` in JSON and so would *look*
/// right while making the third case wrong too. Replacing `None` with
/// `Some(0.0)` makes the first two indistinguishable from the third. And a window
/// of nothing but skips is the case that separates "no rows" from "no *counted*
/// rows": it has rows, and still no rate.
#[test]
fn a_window_with_nothing_to_divide_by_has_no_pass_rate_rather_than_zero() {
    let empty = kpi_of(&[]);
    assert_eq!(empty.failed, 0);
    assert_eq!(empty.pass_rate, None, "an empty window is no data, not 0%");

    let only_skips = kpi_of(&[status_rows("SKIPPED", 9), status_rows("PENDING", 2)]);
    assert_eq!(only_skips.failed, 0);
    assert_eq!(
        only_skips.pass_rate, None,
        "eleven rows and none of them counted is still no data"
    );

    let all_failed = kpi_of(&[status_rows("FAILED", 2), status_rows("ERROR", 1)]);
    assert_eq!(all_failed.failed, 3);
    assert_eq!(
        all_failed.pass_rate,
        Some(0.0),
        "a window of nothing but failures is a measured 0%, not no data"
    );
}

/// **Every column of the row reaches its own field on the card.**
///
/// Nine fields, and the shape invites exactly one bug: three of them are
/// `Uuid`/`Option<Uuid>` (`run_id`, `repo_id`, `platform_id`) and four are
/// `Option<String>` (`plan_path`, `jira_key`, `launch_id`, and `test_file` once
/// it is wrapped), so any transposition inside those two groups compiles and
/// ships. The fixture therefore gives every field a value distinguishable from
/// every other field of its type.
///
/// `jira_key` is asserted because it is filled rather than deferred: legacy reads
/// `tr.jira_key` off the result row in this very query
/// (`manager/src/routes/dashboard.rs:271`), and `qa_test_results` carries the same
/// column — Phase C's JIRA registry is a different key on a different table.
///
/// `finished_at` is the run's finish here because the row has one; the fallback is
/// [`a_card_with_no_finish_instant_falls_back_to_the_runs_creation_instant`].
#[test]
fn a_failure_card_carries_every_column_of_its_row() {
    let card = failure_card(failed_row());

    assert_eq!(card.test_name, "test_login_rejects_expired_token");
    assert_eq!(card.test_file.as_deref(), Some("tests/regression/login.py"));
    assert_eq!(card.run_id, Uuid::from_u128(0xA1));
    assert_eq!(card.repo_id, Some(Uuid::from_u128(0xB2)));
    assert_eq!(
        card.plan_path.as_deref(),
        Some("plans/regression/plan.yaml")
    );
    assert_eq!(card.platform_id, Some(Uuid::from_u128(0xC3)));
    assert_eq!(card.finished_at, Some(datetime!(2026-08-20 11:30:00 UTC)));
    assert_eq!(card.jira_key.as_deref(), Some("VHP-4711"));
    assert_eq!(card.launch_id.as_deref(), Some("88213"));
}

/// **The two conversions on the card, each of which a plain copy gets wrong, and
/// one of which has *three* candidate columns.**
///
/// * `finished_at` is legacy's `COALESCE(rr.finished_at, rr.created_at)`
///   (`manager/src/routes/dashboard.rs:270`), where `rr.created_at` is the
///   **run's** creation instant. So a row from a run still in progress carries
///   `run_created_at`. Two mutations are covered, both of which compile:
///   copying `run_finished_at` straight through gives `null` — on precisely the
///   rows this KPI rule exists to include, so the count would say "3 failures"
///   over a list showing three cards with no time — and reaching for the row's
///   own `created_at` gives a plausible but wrong instant, which is the column
///   this task shipped before Ruling A and the reason
///   `qa_test_results.run_created_at` exists. [`failed_row`] makes `created_at`
///   the *latest* of the three so that mutation is visible rather than merely
///   untested.
/// * `test_file` is `None` rather than `Some("")`. This schema collapses "no
///   file" to `""` (`qa_insights_sdk::TestResultRecord`'s divergence 3) where
///   legacy's column is nullable, so an unconditional `Some` puts an empty path
///   on the card and a client renders a blank link.
#[test]
fn a_card_with_no_finish_instant_falls_back_to_the_runs_creation_instant() {
    let row = failed_row();
    let ingested_at = row.created_at;
    let card = failure_card(TestResultRecord {
        test_file: String::new(),
        run_finished_at: None,
        ..row
    });

    assert_eq!(
        card.finished_at,
        Some(datetime!(2026-08-20 09:15:00 UTC)),
        "no run finish, so the *run's* creation instant is the effective one"
    );
    assert_ne!(
        card.finished_at,
        Some(ingested_at),
        "and it is not the row's own created_at, which is when this row was last \
         written and moves on every re-ingest of the run"
    );
    assert_eq!(
        card.test_file, None,
        "an empty test_file column is absent, not an empty path"
    );
}

// ---------------------------------------------------------------------------
// The quality-vector pass rate
// ---------------------------------------------------------------------------

/// One counted file: `(test_file, passed, failed)`, with `total` derived as the
/// union the read derives it as.
fn file_count(test_file: &str, passed: u64, failed: u64) -> FileStatusCount {
    FileStatusCount {
        test_file: test_file.to_owned(),
        passed,
        failed,
        total: passed + failed,
    }
}

/// A universe entry for `test_file` carrying `vectors`.
fn vectored(test_file: &str, vectors: &[&str]) -> qa_catalog_sdk::UniverseTest {
    qa_catalog_sdk::UniverseTest {
        quality_vectors: vectors.iter().map(|v| (*v).to_owned()).collect(),
        ..universe_test(test_file)
    }
}

/// **A file's counters are added to every vector it declares**, and `tests`
/// counts distinct files rather than executions.
///
/// Legacy fans out inside the row loop (`dashboard.rs:519-525`): one `agg` entry
/// per vector, `+=` on all three counters, and the *file* inserted into a
/// `HashSet` whose length becomes `tests` (`:509`, `:524`, `:534`). So a file
/// carrying two vectors is counted twice in the sums and once in each vector's
/// test count — which is what makes the vectors a partition of *concerns* rather
/// than of files.
///
/// The three counters per file are deliberately distinct and the two files
/// deliberately unequal, so a fold that summed the wrong column, or divided
/// instead of adding, lands on none of these numbers.
#[test]
fn a_file_contributes_its_counters_to_each_vector_it_declares() {
    let counts = [
        file_count("tests/a.py", 3, 1),
        file_count("tests/b.py", 5, 2),
    ];
    let universe = [
        vectored("tests/a.py", &["Security", "Resilience"]),
        vectored("tests/b.py", &["Security"]),
    ];

    let rates = quality_vector_pass_rates(&counts, &universe);

    let by_vector: HashMap<&str, &QualityVectorPassRate> = rates
        .iter()
        .map(|item| (item.vector.as_str(), item))
        .collect();
    let security = by_vector["Security"];
    assert_eq!(
        (
            security.passed,
            security.failed,
            security.total,
            security.tests
        ),
        (8, 3, 11, 2),
        "both files carry Security, so their counters sum and two distinct files \
         are counted",
    );
    let resilience = by_vector["Resilience"];
    assert_eq!(
        (
            resilience.passed,
            resilience.failed,
            resilience.total,
            resilience.tests
        ),
        (3, 1, 4, 1),
        "only tests/a.py carries Resilience",
    );
}

/// **A counted file the universe does not know is dropped**, and so is a file
/// the universe knows and that declares no vector.
///
/// Legacy's two `continue`s (`dashboard.rs:516-518` for the missing file,
/// and the empty `vectors` loop for the unclassified one). Both matter and they
/// are different: the first is a row for a test that no plan lists any more —
/// deleted, renamed, or never in the catalog — and the second is a test that is
/// listed and simply carries no `TEST_META` `quality_vectors`.
///
/// Note what this does **not** do: it does not produce an "unclassified" bucket.
/// The *analytics* quality-vector fold does
/// (`crate::domain::analytics::aggregates::build_quality_vector_summary`'s
/// `unclassified_tests`) and this one has no such field — legacy's
/// `QualityVectorPassRate` has five fields and none of them is a residue.
#[test]
fn a_file_with_no_vectors_or_no_universe_entry_contributes_to_nothing() {
    let counts = [
        file_count("tests/known.py", 1, 0),
        file_count("tests/orphan.py", 9, 9),
        file_count("tests/plain.py", 7, 7),
    ];
    let universe = [
        vectored("tests/known.py", &["Security"]),
        vectored("tests/plain.py", &[]),
    ];

    let rates = quality_vector_pass_rates(&counts, &universe);

    assert_eq!(rates.len(), 1, "{rates:?}");
    assert_eq!(rates[0].vector, "Security");
    assert_eq!(
        (
            rates[0].passed,
            rates[0].failed,
            rates[0].total,
            rates[0].tests
        ),
        (1, 0, 1, 1),
        "neither the orphan's 9/9 nor the unclassified file's 7/7 is anywhere in \
         the answer",
    );
}

/// **The row's file is normalized before the lookup; the universe's is not.**
///
/// Legacy normalizes the *row* inline (`dashboard.rs:503-508` — trim, strip
/// leading `./`, strip leading `/`, backslash to slash) and looks the result up
/// in a map already keyed on `normalize_test_path`'s output
/// (`analytics.rs:2044`). This port reuses
/// [`crate::domain::analytics::universe::normalize_test_path`] rather than
/// re-spelling that closure, and the universe side needs nothing because
/// `qa_catalog_sdk::UniverseTest::test_file` arrives normalized — the SDK field's
/// own doc states it.
///
/// Two stored spellings of one file therefore land on **one** vector entry with
/// their counters summed and `tests == 1`, which is the case that would otherwise
/// double-count a rename.
#[test]
fn the_stored_path_is_normalized_before_it_is_matched() {
    let counts = [
        file_count("./tests/a.py", 2, 0),
        file_count("  tests/a.py  ", 3, 1),
        file_count("/tests/a.py", 1, 1),
    ];
    let universe = [vectored("tests/a.py", &["Security"])];

    let rates = quality_vector_pass_rates(&counts, &universe);

    assert_eq!(rates.len(), 1);
    assert_eq!(
        (
            rates[0].passed,
            rates[0].failed,
            rates[0].total,
            rates[0].tests
        ),
        (6, 2, 8, 1),
        "three stored spellings are one file",
    );
}

/// Ranked by [`QualityVectorPassRate::total`] **descending**, with ties broken
/// ascending by vector name.
///
/// Legacy sorts `b.total.cmp(&a.total)` (`dashboard.rs:537`) over a `BTreeMap`
/// drained in ascending key order (`:509`, `:527-528`), and `Vec::sort_by` is stable
/// — so the tiebreak is the vector name ascending. Legacy's order is total within
/// a dialect-free fold and this reproduces it exactly rather than refining it.
///
/// The fixture makes `Zebra` and `Alpha` tie on `total` while `Middle` outranks
/// both, so a fold that sorted ascending, or that left the map order alone, or
/// that broke the tie the other way, fails here.
#[test]
fn the_vectors_are_ranked_by_total_descending_then_by_name() {
    let counts = [
        file_count("tests/a.py", 1, 0),
        file_count("tests/b.py", 4, 0),
    ];
    let universe = [
        vectored("tests/a.py", &["Zebra", "Alpha"]),
        vectored("tests/b.py", &["Middle"]),
    ];

    let rates = quality_vector_pass_rates(&counts, &universe);

    assert_eq!(
        rates
            .iter()
            .map(|item| (item.vector.as_str(), item.total))
            .collect::<Vec<_>>(),
        vec![("Middle", 4), ("Alpha", 1), ("Zebra", 1)],
    );
}

/// One file declaring the same vector twice under different casing counts it
/// **once** — legacy's per-file case-folded dedup.
///
/// `build_quality_vectors_by_file` folds per file on
/// `trimmed.to_ascii_lowercase()` keeping the first spelling it saw
/// (`analytics.rs:2049-2055`), and blank entries are dropped (`:2049-2052`).
/// qa-catalog already applies the same rule when it builds
/// `UniverseTest::quality_vectors` (`qa-catalog/src/domain/parsing/test_meta.rs`'
/// `dedup_fold_ascii_case`, and the cross-plan union at
/// `domain/service/plans.rs:443-450`), so this fold re-applies it rather than
/// trusting it — the port's contract is a `Vec<String>`, not a set.
#[test]
fn one_files_case_variants_and_blanks_are_folded_before_counting() {
    let counts = [file_count("tests/a.py", 2, 2)];
    let universe = [vectored("tests/a.py", &["Security", "security", "  ", ""])];

    let rates = quality_vector_pass_rates(&counts, &universe);

    assert_eq!(rates.len(), 1, "one vector, not four: {rates:?}");
    assert_eq!(rates[0].vector, "Security", "the first spelling seen wins");
    assert_eq!(
        (rates[0].passed, rates[0].failed, rates[0].total),
        (2, 2, 4),
        "counted once, not twice",
    );
}

/// **Two *different files* spelling one vector differently are two rows, and
/// that is legacy's behaviour rather than a defect this port introduces.**
///
/// Legacy's dashboard fold keys `agg` on the **display string**
/// (`dashboard.rs:520`, `agg.entry(vector.clone())`), and the case-folded dedup
/// that produced that string ran *per file* (`analytics.rs:2053-2055`) — so `Security`
/// from one file and `security` from another are two independent entries with
/// their own sums and their own `tests` counts.
///
/// Note the asymmetry with the **analytics** quality-vector fold, which does the
/// opposite: `build_quality_vector_summary` folds across files
/// (`analytics.rs:1060-1062`) and reports one merged count. **Legacy has two
/// quality-vector folds and they disagree on this**, so the two ports disagree
/// too. Ported verbatim under Phase B's standing instruction, and pinned here so
/// that an implementation which "fixed" it — by folding, or by lowercasing the
/// key — changes a rendered number and fails rather than passing quietly.
#[test]
fn two_files_spelling_a_vector_differently_are_two_dashboard_rows() {
    let counts = [
        file_count("tests/a.py", 3, 0),
        file_count("tests/b.py", 0, 1),
    ];
    let universe = [
        vectored("tests/a.py", &["Security"]),
        vectored("tests/b.py", &["security"]),
    ];

    let rates = quality_vector_pass_rates(&counts, &universe);

    assert_eq!(
        rates
            .iter()
            .map(|item| (item.vector.as_str(), item.total, item.tests))
            .collect::<Vec<_>>(),
        vec![("Security", 3, 1), ("security", 1, 1)],
        "not merged: legacy's dashboard fold keys on the display string, unlike \
         its analytics fold",
    );
}

/// **A file with no counted row still counts toward its vectors' `tests`**, and
/// can render a whole row on its own.
///
/// `entry.3.insert(normalized)` (`dashboard.rs:524`) runs unconditionally inside
/// the per-vector loop, with no test on the counters — so a file whose window
/// holds nothing but `SKIPPED` rows arrives here as `(0, 0, 0)`, adds nothing to
/// its vectors' sums, and still increments each one's distinct-file count. On a
/// vector no other file carries, that is a rendered
/// `("Security", 0, 0, 0, tests: 1)`.
///
/// **This is the test `ResultsRepository::file_status_counts`' doc was missing**,
/// and its absence let that doc assert the opposite — that a
/// `HAVING passed + failed > 0` on the read would be indistinguishable from no
/// clause at all. It is not: the clause deletes this row. So the guard this test
/// provides runs in both directions — a `HAVING` added to the SQL, or a
/// counter test added to the fold's insert, each fails here.
///
/// The second file is seeded so the assertion is not only about a lone row:
/// `Resilience` gets a real 4/1 from a counted file *and* the zero-counter file's
/// `tests` increment, so a fold that skipped zero-counter files would report
/// `tests: 1` there while leaving the counters right.
#[test]
fn a_file_with_no_counted_row_still_counts_toward_its_vectors_tests() {
    let counts = [
        file_count("tests/skipped.py", 0, 0),
        file_count("tests/real.py", 4, 1),
    ];
    let universe = [
        vectored("tests/skipped.py", &["Security", "Resilience"]),
        vectored("tests/real.py", &["Resilience"]),
    ];

    let rates = quality_vector_pass_rates(&counts, &universe);

    assert_eq!(
        rates
            .iter()
            .map(|item| (
                item.vector.as_str(),
                item.passed,
                item.failed,
                item.total,
                item.tests
            ))
            .collect::<Vec<_>>(),
        vec![("Resilience", 4, 1, 5, 2), ("Security", 0, 0, 0, 1)],
        "the all-skipped file adds no counters and one test to each of its two \
         vectors, and Security is a row of pure zeros rather than absent",
    );
}

/// A file listed by two plans is **one** vector test, not two.
///
/// The counted rows are keyed on `test_file` by the read, so duplication can only
/// arrive from the universe side — and it can:
/// `crate::domain::analytics::aggregates::build_quality_vector_summary`'s header
/// records that legacy's universe is keyed on `(source, repo_id, test_file)`, so
/// one file listed by two plans is two entries there. This fold re-keys on the
/// file for the same reason that one does; without it `tests` double-counts and
/// the counters are added twice.
#[test]
fn a_file_in_two_universe_entries_is_one_vector_test() {
    let counts = [file_count("tests/a.py", 2, 1)];
    let mut second = vectored("tests/a.py", &["Security"]);
    second.plan_path = "plans/other.yaml".to_owned();
    let universe = [vectored("tests/a.py", &["Security"]), second];

    let rates = quality_vector_pass_rates(&counts, &universe);

    assert_eq!(rates.len(), 1);
    assert_eq!(
        (
            rates[0].passed,
            rates[0].failed,
            rates[0].total,
            rates[0].tests
        ),
        (2, 1, 3, 1),
    );
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

struct Fixture {
    db: toolkit_db::Db,
    runs: Arc<FakeRuns>,
    /// The universe the quality-vector fold joins against. Empty unless a test
    /// registers entries, which is what keeps every pre-Task-25a fixture's
    /// `quality_vectors_pass_rate` empty.
    catalog: Arc<FakeCatalog>,
    service: DashboardService<OrmResultsRepository>,
}

impl Fixture {
    async fn with_authz(authz: Arc<dyn AuthZResolverClient>) -> Self {
        let db = inmem_db().await;
        let provider = Arc::new(DBProvider::<DomainError>::new(db.clone()));
        let runs = Arc::new(FakeRuns::default());
        let catalog = Arc::new(FakeCatalog::default());
        let service = DashboardService::new(
            provider,
            OrmResultsRepository,
            Arc::clone(&runs) as Arc<dyn crate::domain::ports::RunsReader>,
            Arc::clone(&catalog) as Arc<dyn crate::domain::ports::CatalogReader>,
            PolicyEnforcer::new(authz),
        );
        Self {
            db,
            runs,
            catalog,
            service,
        }
    }

    async fn new() -> Self {
        Self::with_authz(Arc::new(TenantScopedAuthZ)).await
    }

    /// One run's rows with **caller-chosen group keys**, finished now.
    ///
    /// [`Self::seed`] derives `test_name` from the row index and leaves both plan
    /// halves `None`, so every row it writes is its own flaky group and no group
    /// can hold both a pass and a failure. The flaky read needs the opposite, and
    /// a `plan_path` besides — the field the card renders.
    async fn seed_named(&self, tenant: Uuid, run_id: Uuid, rows: &[(&str, &str, &str)]) {
        self.seed_named_at(tenant, run_id, OffsetDateTime::now_utc(), rows)
            .await;
    }

    /// [`Self::seed_named`] with the run's finish instant chosen by the caller.
    ///
    /// Needed because every other fixture here stamps `now`, under which the
    /// 24-hour and seven-day windows admit the same rows — so a service that
    /// windowed the flaky read on `KPI_WINDOW` would be invisible. Measured: that
    /// mutation left the whole suite green until this existed.
    async fn seed_named_at(
        &self,
        tenant: Uuid,
        run_id: Uuid,
        at: OffsetDateTime,
        rows: &[(&str, &str, &str)],
    ) {
        let conn = self.db.conn().unwrap();
        let files = rows
            .iter()
            .map(|(name, plan, status)| NewTestResult {
                test_file: format!("tests/{name}.py"),
                test_name: (*name).to_owned(),
                status: (*status).to_owned(),
                duration: None,
                launch_id: None,
                jira_key: None,
                product_version: Some("8.1.2".to_owned()),
                app_build: Some(format!("build-{run_id}")),
                platform_id: None,
                repo_id: Some(Uuid::from_u128(0xC0)),
                plan_path: Some((*plan).to_owned()),
                branch: None,
                run_finished_at: Some(at),
                run_created_at: Some(at - Duration::hours(1)),
            })
            .collect();
        OrmResultsRepository
            .upsert_run_results(&conn, &scope(tenant), tenant, run_id, files, vec![])
            .await
            .unwrap();
    }

    /// One run's file-level rows for `tenant`, finished **now**, written through
    /// the real repository under a tenant-only scope — the shape ingest produces.
    ///
    /// # `product_version` and `app_build` are non-blank on purpose
    ///
    /// Nothing in the dashboard aggregates reads either column, so they were `None`
    /// until Task 19's review. They are armed because
    /// [`the_coverage_view_omits_every_build_because_nothing_reports_a_summary`]
    /// needs them: the *most legacy-faithful* fabrication available to a coverage
    /// implementation is "group `qa_test_results` by build, skip the blanks as
    /// legacy does (`manager/src/routes/dashboard.rs:596-604`), report something
    /// under `line_pct`", and with both columns `None` that fabrication produces no
    /// points and passes. `app_build` varies per run so the fabrication would
    /// produce more than one.
    ///
    /// # Why the real clock here and a fixed date in the folds above
    ///
    /// [`super::DashboardService::stats`] takes no clock: it opens its window at
    /// `now - (days - 1)` days, which is the behaviour under test. A fixture
    /// instant like [`ts`] is inside that window only for as long as the calendar
    /// cooperates — 2026-08-18 falls out of a 14-day window on 2026-09-01, and
    /// the failure would look like a broken fold rather than a stale fixture.
    /// So the rows are stamped `now`, and the assertions **sum over the whole
    /// trend** rather than reading its last point: that is also robust to a run
    /// that starts one side of midnight and reads the other. Which *day* a row
    /// lands on is pinned by
    /// [`the_daily_trend_folds_error_into_failed_and_counts_no_skips`], against a
    /// fixed date and no clock at all.
    async fn seed(&self, tenant: Uuid, run_id: Uuid, statuses: &[&str]) {
        let at = OffsetDateTime::now_utc();
        let conn = self.db.conn().unwrap();
        let files = statuses
            .iter()
            .enumerate()
            .map(|(n, status)| NewTestResult {
                test_file: format!("tests/t{n}.py"),
                test_name: format!("test_{n}"),
                status: (*status).to_owned(),
                duration: None,
                launch_id: None,
                jira_key: None,
                product_version: Some("8.1.2".to_owned()),
                app_build: Some(format!("build-{run_id}")),
                platform_id: None,
                repo_id: None,
                plan_path: None,
                branch: None,
                run_finished_at: Some(at),
                // An hour before it finished, so the two run instants are
                // distinguishable. This fixture's rows are all finished, so the
                // KPI window takes its `run_finished_at` branch; the fallback
                // branch is the repository tier's
                // `an_unfinished_runs_rows_are_windowed_by_the_runs_age_not_the_ingest_instant`.
                run_created_at: Some(at - Duration::hours(1)),
            })
            .collect();
        OrmResultsRepository
            .upsert_run_results(&conn, &scope(tenant), tenant, run_id, files, vec![])
            .await
            .unwrap();
    }
}

/// The whole payload, end to end: live run state from qa-runs, counters from the
/// projection, one point per recent run and one per day of the window.
#[tokio::test]
async fn the_dashboard_reports_live_run_state_and_ingested_counters() {
    let fx = Fixture::new().await;

    let finished = run_in_state(RunState::Succeeded, 0);
    let running = run_in_state(RunState::Running, 10);
    let queued = run_in_state(RunState::Queued, 20);
    let finished_id = finished.id;
    fx.runs.add_run(finished, vec![]);
    fx.runs.add_run(running, vec![]);
    fx.runs.add_run(queued, vec![]);

    fx.seed(TENANT, finished_id, &["PASSED", "PASSED", "ERROR"])
        .await;

    let stats = fx.service.stats(&ctx(TENANT), Some(14)).await.unwrap();

    assert_eq!(stats.total_runs, 1, "one run has results ingested");
    assert_eq!(stats.active_runs, 1);
    assert_eq!(stats.queued_runs, 1);
    assert_eq!(stats.active_runs_list.len(), 1);
    assert_eq!(stats.recent_runs.len(), 3);

    // The newest run is the one with results; the other two are zero points.
    assert_eq!(stats.recent_run_test_trend.len(), 3);
    let point = stats
        .recent_run_test_trend
        .iter()
        .find(|p| p.run_id == finished_id)
        .unwrap();
    assert_eq!(point.tests_total, 3);
    assert_eq!(point.passed, 2);
    assert_eq!(point.failed, 1, "ERROR folds into failed");

    assert_eq!(stats.daily_test_status_trend.len(), 14);
    assert_eq!(
        trend_totals(&stats),
        (2, 1),
        "(passed, failed) over the window"
    );
}

/// **A denied caller reads nothing, and does not learn which runs exist.**
///
/// The PEP decision is compiled **before** the qa-runs listing, which is the
/// same discipline `ReconcileService::rebuild` records: otherwise the endpoint
/// is an oracle for "which runs are there" that works without the grant.
/// `FakeRuns::listings` is what makes the ordering observable — asserting only
/// on the error would pass for an implementation that listed first.
#[tokio::test]
async fn a_denied_caller_reads_nothing_and_does_not_reach_qa_runs() {
    let fx = Fixture::with_authz(Arc::new(DenyAllAuthZ)).await;
    fx.runs.add_run(run_in_state(RunState::Running, 0), vec![]);

    assert!(matches!(
        fx.service.stats(&ctx(TENANT), None).await,
        Err(DomainError::Forbidden)
    ));
    assert_eq!(
        fx.runs.listings(),
        0,
        "the scope must be compiled before qa-runs is asked anything",
    );
}

/// **Every field of a flaky group reaches its own field on the card.**
///
/// Legacy maps `FlakyRow` to `FlakyTestCard` one to one
/// (`manager/src/routes/dashboard.rs:406-416`). Seven copies, and
/// [`super::flaky_card`] names five transposition hazards on this one struct:
/// three `u64` counters, two `Option<String>`s on the card, and `test_name` and
/// `test_file` both `String` on the group. So every field gets a distinct value —
/// `4`, `9`, `13` for the counters, three unrelated strings, and a `Uuid` no
/// other field could hold.
///
/// The counters are `4 + 9 = 13` because that is the arithmetic the query
/// guarantees (`crate::domain::repos::FlakyGroup::total`); they are still three
/// distinct values, so a transposed pair is visible, and `4` before `9` means a
/// swap turns a mostly-failing test into a mostly-passing one.
#[test]
fn a_flaky_card_carries_every_field_of_its_group() {
    let card = flaky_card(FlakyGroup {
        test_name: "test_reconnects_after_a_broker_restart".to_owned(),
        test_file: "tests/integration/broker.py".to_owned(),
        repo_id: Some(Uuid::from_u128(0xE7)),
        plan_path: "plans/integration/plan.yaml".to_owned().into(),
        passed: 4,
        failed: 9,
        total: 13,
    });

    assert_eq!(card.test_name, "test_reconnects_after_a_broker_restart");
    assert_eq!(
        card.test_file.as_deref(),
        Some("tests/integration/broker.py"),
        "the group's representative file, not its name and not the plan path"
    );
    assert_eq!(card.repo_id, Some(Uuid::from_u128(0xE7)));
    assert_eq!(
        card.plan_path.as_deref(),
        Some("plans/integration/plan.yaml")
    );
    assert_eq!(card.passed, 4);
    assert_eq!(
        card.failed, 9,
        "this test mostly fails; a transposed pair reverses that, which is the \
         direction the flaky ranking is about"
    );
    assert_eq!(card.total, 13, "and the denominator is neither of the two");
}

/// **A group whose rows named no file leaves the card's file absent.**
///
/// `qa_test_results.test_file` is `NOT NULL DEFAULT ''`, so `MAX` over such a
/// group is `""`; legacy's column is nullable and its card is `Option<String>`.
/// [`super::flaky_card`] is where the two spellings meet, exactly as
/// [`super::failure_card`] does for the same column — and the mutation this
/// catches is `test_file: Some(group.test_file)`, which would put an empty path
/// on a card legacy would have left blank.
///
/// The counters are carried along so the same fixture also rules out the
/// `then_some` being applied to the wrong field.
#[test]
fn a_flaky_group_with_no_file_leaves_the_cards_file_absent() {
    let card = flaky_card(FlakyGroup {
        test_name: "test_unattributed".to_owned(),
        test_file: String::new(),
        repo_id: None,
        plan_path: None,
        passed: 3,
        failed: 8,
        total: 11,
    });

    assert_eq!(card.test_file, None, "not Some(\"\")");
    assert_eq!(card.test_name, "test_unattributed");
    assert_eq!(card.repo_id, None);
    assert_eq!(card.plan_path, None);
    assert_eq!((card.passed, card.failed, card.total), (3, 8, 11));
}

/// **The flaky list, end to end over the real repository**: the field is filled,
/// ranked, and the two status partitions reach SQL the right way round.
///
/// Legacy's query is `manager/src/routes/dashboard.rs:380-400`. The fixture is
/// three groups under two plans, seeded across two runs so that each flaky group's
/// passes and failures come from *different* runs — which is what
/// `qa_test_results` actually holds, since one run reports one status per test:
///
/// | group | passed | failed | smaller |
/// |---|---|---|---|
/// | `test_flip` | 2 | 3 | **2** |
/// | `test_blip` | 4 | 1 | **1** |
/// | `test_solid` | 3 | 0 | rejected |
///
/// Four mutations this catches that neither the pure fold nor the repository
/// tests can, because they are about the wiring:
///
/// * **The field left at `Default`** — the list would be empty.
/// * **The two status sets passed the wrong way round**: `test_flip` would report
///   `3` passed and `2` failed, and `test_blip` `1` and `4`. The **order is
///   unchanged** — the smaller counter of each group is a set-symmetric quantity,
///   so it stays 2, 1, 1 — and what fails is the counter tuple. This doc said the
///   ranking would reorder, which is wrong and was corrected in Task 23b's review
///   round; the mutation is caught either way, by the exact vector rather than by
///   its order.
/// * **`FLAKY_WINDOW` wired to `KPI_WINDOW`** — which is why `test_slow_flip` is
///   seeded **three days back**. Every other fixture in this file stamps `now`,
///   under which the 24-hour and seven-day windows admit exactly the same rows;
///   measured, that mutation left the whole suite green before this group existed.
///   The window's *expression* is still the repository tier's
///   `the_flaky_window_is_seven_days_of_the_effective_timestamp_and_scoped`; what
///   is pinned here is which constant the service hands it.
/// * **`FLAKY_TESTS_LISTED` wired to the wrong constant** — a limit of zero
///   empties the list.
/// * **`flaky_card` fed the wrong groups**, e.g. the card list built from
///   `failures` — the names would be the failure card's rows and `total` would be
///   absent.
///
/// The `test_file` assertion is the one that reaches all the way from
/// `MAX(test_file)` in SQL to `Option<String>` on the card.
#[tokio::test]
async fn the_dashboard_reports_the_seven_day_flaky_tests() {
    let fx = Fixture::new().await;
    let run = run_in_state(RunState::Succeeded, 0);
    fx.runs.add_run(run, vec![]);
    let nightly = "plans/nightly/plan.yaml";
    let smoke = "plans/smoke/plan.yaml";
    let weekly = "plans/weekly/plan.yaml";

    fx.seed_named(
        TENANT,
        Uuid::from_u128(0x70),
        &[
            ("test_flip", nightly, "PASSED"),
            ("test_flip", nightly, "PASSED"),
            ("test_blip", smoke, "PASSED"),
            ("test_blip", smoke, "PASSED"),
            ("test_blip", smoke, "PASSED"),
            ("test_blip", smoke, "PASSED"),
            ("test_solid", smoke, "PASSED"),
            ("test_solid", smoke, "PASSED"),
            ("test_solid", smoke, "PASSED"),
        ],
    )
    .await;
    fx.seed_named(
        TENANT,
        Uuid::from_u128(0x71),
        &[
            ("test_flip", nightly, "FAILED"),
            ("test_flip", nightly, "ERROR"),
            ("test_flip", nightly, "FAILED"),
            ("test_blip", smoke, "ERROR"),
            ("test_solid", smoke, "SKIPPED"),
        ],
    )
    .await;
    // Three days back: inside the seven-day flaky window and outside the 24-hour
    // one, which is the only fixture in this file that separates the two.
    let three_days_ago = OffsetDateTime::now_utc() - Duration::days(3);
    fx.seed_named_at(
        TENANT,
        Uuid::from_u128(0x72),
        three_days_ago,
        &[("test_slow_flip", weekly, "PASSED")],
    )
    .await;
    fx.seed_named_at(
        TENANT,
        Uuid::from_u128(0x73),
        three_days_ago,
        &[("test_slow_flip", weekly, "ERROR")],
    )
    .await;

    let stats = fx.service.stats(&ctx(TENANT), None).await.unwrap();

    assert_eq!(
        stats
            .flaky_tests
            .iter()
            .map(|c| (
                c.test_name.as_str(),
                c.plan_path.as_deref(),
                c.passed,
                c.failed,
                c.total
            ))
            .collect::<Vec<_>>(),
        vec![
            ("test_flip", Some(nightly), 2, 3, 5),
            ("test_blip", Some(smoke), 4, 1, 5),
            ("test_slow_flip", Some(weekly), 1, 1, 2),
        ],
        "test_flip first because its smaller counter is 2 against the other two's \
         1, even though its total ties test_blip's; test_blip beats \
         test_slow_flip on total; test_solid never failed and is absent rather \
         than present with a zero; and test_slow_flip is three days old, so a \
         24-hour window would drop it: {:?}",
        stats.flaky_tests,
    );
    assert_eq!(
        stats.flaky_tests[0].test_file.as_deref(),
        Some("tests/test_flip.py"),
        "MAX(test_file) over the group, through the card's Option<String>"
    );
}

/// **The action string is a security surface with no other witness.**
///
/// Nothing in this crate fails if the dashboard silently starts asking for
/// `rebuild`, and no test in this workspace evaluates a real policy — so this is
/// the only place the request this gear actually sends is observable. It is the
/// same argument `test_support::RecordingAuthZ`'s own doc makes.
///
/// **One decision per request**, on `qa.test_result` / `list`: the dashboard is
/// a reduction of exactly the rows `GET /qa/v1/test-results` returns, and
/// `domain::service::dashboard`'s header records why a `view_dashboard` action
/// was rejected.
#[tokio::test]
async fn the_dashboard_authorizes_under_test_result_list_once() {
    let authz = Arc::new(RecordingAuthZ::default());
    let fx = Fixture::with_authz(authz.clone()).await;

    fx.service.stats(&ctx(TENANT), None).await.unwrap();

    assert_eq!(
        authz.asked(),
        vec![("qa.test_result".to_owned(), "list".to_owned())],
        "one decision, on the resource the aggregate reads",
    );
}

/// Only the caller's tenant's rows reach any counter.
///
/// The other tenant's rows outnumber the caller's, so an aggregate that lost its
/// scope produces larger numbers rather than an error — which is the failure
/// mode a `GROUP BY` built from a raw `Select` would have.
#[tokio::test]
async fn the_counters_hold_only_the_callers_tenants_rows() {
    let fx = Fixture::new().await;
    let mine = Uuid::from_u128(0x60);
    let theirs = Uuid::from_u128(0x61);

    let run = run_in_state(RunState::Succeeded, 0);
    let run_id = run.id;
    fx.runs.add_run(run, vec![]);

    fx.seed(TENANT, mine, &["PASSED"]).await;
    fx.seed(OTHER_TENANT, theirs, &["FAILED", "FAILED", "FAILED"])
        .await;
    // The other tenant also holds rows for the *same* run the listing returned.
    fx.seed(OTHER_TENANT, run_id, &["FAILED", "FAILED"]).await;

    let stats = fx.service.stats(&ctx(TENANT), None).await.unwrap();
    assert_eq!(stats.total_runs, 1, "one run of mine, not three");
    assert_eq!(
        trend_totals(&stats),
        (1, 0),
        "the other tenant's five failures are invisible",
    );

    let point = stats
        .recent_run_test_trend
        .iter()
        .find(|p| p.run_id == run_id)
        .unwrap();
    assert_eq!(
        point.tests_total, 0,
        "the run's only rows belong to the other tenant",
    );
}

/// **A scope narrower than the tenant narrows the answer instead of refusing
/// it** — the difference between a read and this gear's projection write.
///
/// [`ResourceConstrainedAuthZ`] compiles `owner_tenant_id IN [tenant]` **and**
/// `resource_id IN [<random>]`, the shape a PDP produces for a row-scoped grant.
/// `refuse_scope_beyond_tenant` rejects it on the write path for the measured
/// reason recorded there; a read applies it, and the random id matches no stored
/// row, so the honest answer is zeros. This is the test that fails if somebody
/// copies the write path's guard into this service.
///
/// The **live** numbers are unaffected, and that is the other half of the
/// property: run state is qa-runs', so a row-scoped grant on this gear's tables
/// has nothing to narrow there.
#[tokio::test]
async fn a_row_scoped_grant_zeroes_the_counters_instead_of_failing() {
    let fx = Fixture::with_authz(Arc::new(ResourceConstrainedAuthZ)).await;
    let run = run_in_state(RunState::Running, 0);
    let run_id = run.id;
    fx.runs.add_run(run, vec![]);
    fx.seed(TENANT, run_id, &["PASSED", "FAILED"]).await;

    let stats = fx
        .service
        .stats(&ctx(TENANT), None)
        .await
        .expect("a row-scoped grant is a narrower read, not a refusal");

    assert_eq!(stats.total_runs, 0);
    assert!(
        stats
            .daily_test_status_trend
            .iter()
            .all(|p| p.passed == 0 && p.failed == 0),
    );
    assert_eq!(
        stats.active_runs, 1,
        "live run state is not this gear's rows"
    );
}

/// A qa-runs failure fails the endpoint rather than answering a dashboard with
/// no runs on it.
///
/// The same rule the reconciler's listing has: laundering a transport failure
/// into an empty page makes a broken deployment look like an idle one.
#[tokio::test]
async fn a_qa_runs_failure_is_an_error_rather_than_an_empty_dashboard() {
    let fx = Fixture::new().await;
    fx.runs.fail_listing(true);

    assert!(matches!(
        fx.service.stats(&ctx(TENANT), None).await,
        Err(DomainError::Internal(_))
    ));
}

/// The caller's `days` reaches the window, and the clamp is applied on the way.
#[tokio::test]
async fn the_callers_day_count_is_clamped_and_governs_the_trend_length() {
    let fx = Fixture::new().await;

    for (asked, expected) in [(None, 14), (Some(1), 3), (Some(365), 90), (Some(30), 30)] {
        let stats = fx.service.stats(&ctx(TENANT), asked).await.unwrap();
        assert_eq!(
            stats.daily_test_status_trend.len(),
            expected,
            "days={asked:?} must produce {expected} points",
        );
    }
}

/// The four fields no upstream in this gear can fill yet are at their `Default`,
/// and this test is the list.
///
/// It exists because a `0` on the wire is indistinguishable from a measured
/// zero: the endpoint description and
/// `qa_insights_sdk::DashboardStats`' header both say which fields are not
/// computed, and prose that duplicates a field list goes stale. When a later
/// task fills one of these, this assertion is what tells it to update both.
///
/// **It listed ten through Task 21a.** The five 24-hour fields left it when Task
/// 21b filled them, and the seeded run below is what made that visible here
/// first: a `FAILED` row stamped `now` puts `failed_24h_count` at `1`, so this
/// test could not be left alone. That is the intended mechanism, not an
/// inconvenience. Their positive assertions are
/// [`the_dashboard_reports_the_twenty_four_hour_kpis`].
///
/// **`flaky_tests` left it in Task 23b**, and this time the seeded run did *not*
/// force the issue — the fixture's two rows are one `PASSED` and one `FAILED`
/// under two different test names, so they are two groups of one, both rejected
/// by the flaky `HAVING`, and the field is computed-and-empty. Removing it from
/// this list is therefore a deliberate edit rather than a forced one, which is
/// exactly the case this test is weakest on; its positive assertion is
/// [`the_dashboard_reports_the_seven_day_flaky_tests`], whose fixture makes one
/// test both pass and fail.
///
/// **`quality_vectors_pass_rate` left it in Task 25a**, and the seeded fixture
/// did not force that one either: this `Fixture` registers no universe, so the
/// fold has nothing to join against and the field is computed-and-empty. That is
/// the *weak* case again, so it is asserted as empty **below** rather than
/// dropped — with the reason spelled out, because "no universe" and "no
/// implementation" are two different empties and this test now means the first.
/// Its positive assertion is
/// [`the_dashboard_reports_the_seven_day_quality_vector_pass_rates`].
#[tokio::test]
async fn the_fields_with_no_upstream_yet_are_left_untouched() {
    let fx = Fixture::new().await;
    let run = run_in_state(RunState::Succeeded, 0);
    let run_id = run.id;
    fx.runs.add_run(run, vec![]);
    fx.seed(TENANT, run_id, &["PASSED", "FAILED"]).await;

    let stats = fx.service.stats(&ctx(TENANT), None).await.unwrap();

    assert_eq!(stats.total_plans, 0, "needs a qa-catalog plan listing");
    assert_eq!(stats.total_schedules, 0, "needs a qa-runs schedule listing");
    assert_eq!(
        stats.platforms_summary,
        PlatformsSummary::default(),
        "needs a qa-environments port that does not exist"
    );
    assert!(
        stats.quality_vectors_pass_rate.is_empty(),
        "computed since Task 25a, and empty here because this fixture registers \
         no universe for the fold to join against, not because nothing computes it"
    );
}

/// **The quality-vector pass rate, end to end over the real repository and the
/// catalog port.**
///
/// The one test that covers the whole of `quality_vectors_pass_rate`: the
/// per-`test_file` read, the seven-day window, the sixth classification's
/// denominator, the join against `UniverseTest::quality_vectors` and the fold's
/// ranking. The pure fold's own properties are pinned by the seven tests above;
/// what this adds is the **wiring**, and four mutations it catches that none of
/// them can:
///
/// * the read given the 24-hour window instead of the seven-day one — the
///   `three_days_ago` rows would vanish from `Security`'s totals;
/// * the read given `days`' window — same, in the other direction;
/// * `list_universe` called with a narrowed `product_id` or `branch` — asserted
///   directly off `FakeCatalog::requests`, because a narrowed universe returns a
///   smaller but entirely plausible answer;
/// * the fold handed the flaky groups instead of the file counts, which would
///   compile only if the two row types were confused and is why the assertion is
///   on numbers rather than on emptiness.
///
/// The fixture: `tests/test_a.py` carries `Security` and `Resilience`,
/// `tests/test_b.py` carries `Security`, `tests/test_c.py` is in the universe
/// with no vector at all, and `tests/test_d.py` has rows but no universe entry.
/// So `Security` sums two files and `Resilience` one, and neither `test_c` nor
/// `test_d` is anywhere in the answer.
#[tokio::test]
async fn the_dashboard_reports_the_seven_day_quality_vector_pass_rates() {
    let fx = Fixture::new().await;
    let run = run_in_state(RunState::Succeeded, 0);
    fx.runs.add_run(run, vec![]);
    let plan = "plans/smoke/plan.yaml";

    // `seed_named` writes `tests/{name}.py`, which is what the universe entries
    // below are keyed on.
    fx.catalog.add(
        Uuid::new_v4(),
        DEFAULT_BRANCH,
        vectored("tests/test_a.py", &["Security", "Resilience"]),
    );
    fx.catalog.add(
        Uuid::new_v4(),
        DEFAULT_BRANCH,
        vectored("tests/test_b.py", &["Security"]),
    );
    fx.catalog.add(
        Uuid::new_v4(),
        DEFAULT_BRANCH,
        vectored("tests/test_c.py", &[]),
    );

    fx.seed_named(
        TENANT,
        Uuid::from_u128(0x80),
        &[
            ("test_a", plan, "PASSED"),
            ("test_a", plan, "FAILED"),
            ("test_b", plan, "PASSED"),
            ("test_b", plan, "PASSED"),
            // Neither of these is in any counter, the denominator included —
            // ruling R5's sixth classification.
            ("test_b", plan, "SKIPPED"),
            ("test_b", plan, "RUNNING"),
            ("test_c", plan, "PASSED"),
            ("test_d", plan, "FAILED"),
        ],
    )
    .await;
    // Three days back: inside the seven-day quality-vector window and outside the
    // 24-hour KPI one, so a read windowed on `KPI_WINDOW` loses this row.
    fx.seed_named_at(
        TENANT,
        Uuid::from_u128(0x81),
        OffsetDateTime::now_utc() - Duration::days(3),
        &[("test_a", plan, "ERROR")],
    )
    .await;

    let stats = fx.service.stats(&ctx(TENANT), None).await.unwrap();

    assert_eq!(
        stats
            .quality_vectors_pass_rate
            .iter()
            .map(|item| (
                item.vector.as_str(),
                item.passed,
                item.failed,
                item.total,
                item.tests
            ))
            .collect::<Vec<_>>(),
        vec![
            // test_a: 1 passed, 2 failed (FAILED + the three-day-old ERROR).
            // test_b: 2 passed, 0 failed; SKIPPED and RUNNING count nowhere.
            ("Security", 3, 2, 5, 2),
            ("Resilience", 1, 2, 3, 1),
        ],
        "Security sums both files and outranks Resilience on total; test_c has \
         no vector and test_d no universe entry, so neither appears",
    );
    assert_eq!(
        fx.catalog.requests(),
        vec![(None, None)],
        "one read, unnarrowed: every product, and each repository's default \
         branch, because legacy walks every plan and resolves each one's own \
         checkout"
    );
}

/// **No group at all means the catalog is never read**, which is legacy's own
/// short-circuit and is invisible in the payload.
///
/// `Ok(rows) if rows.is_empty() => { /* Nothing to aggregate; skip the expensive
/// TEST_META parse entirely. */ }` (`dashboard.rs:498-500`). Here the expense is
/// a cross-gear round trip. The section is empty either way, so the *only*
/// witness is `FakeCatalog::requests` being empty — which is why that recorder
/// exists.
///
/// The fixture seeds rows **outside** the seven-day window, which is the state a
/// quiet deployment is in. An all-`SKIPPED` run would not do: those rows are
/// inside the window, so the read still forms a group for the file — with three
/// zeros, since `SKIPPED` is in no counter — and the short-circuit does not fire.
/// The empty-input state is "no counted **group**", not "no counted row".
#[tokio::test]
async fn an_empty_window_skips_the_catalog_read_entirely() {
    let fx = Fixture::new().await;
    let run = run_in_state(RunState::Succeeded, 0);
    fx.runs.add_run(run, vec![]);
    fx.catalog.add(
        Uuid::new_v4(),
        DEFAULT_BRANCH,
        vectored("tests/test_a.py", &["Security"]),
    );
    fx.seed_named_at(
        TENANT,
        Uuid::from_u128(0x82),
        OffsetDateTime::now_utc() - Duration::days(30),
        &[("test_a", "plans/smoke/plan.yaml", "PASSED")],
    )
    .await;

    let stats = fx.service.stats(&ctx(TENANT), None).await.unwrap();

    assert!(stats.quality_vectors_pass_rate.is_empty());
    assert!(
        fx.catalog.requests().is_empty(),
        "nothing to aggregate, so no cross-gear round trip: {:?}",
        fx.catalog.requests(),
    );
}

/// A denied caller reads **nothing** and does not reach qa-catalog.
///
/// The same property `a_denied_caller_reads_nothing_and_does_not_reach_qa_runs`
/// asserts one port over, and it needs its own assertion for the same reason:
/// the scope is compiled before any cross-gear call, so a caller with no grant
/// must not be able to use this endpoint to learn which plans exist. Asserting
/// the error alone passes for an implementation that listed first.
#[tokio::test]
async fn a_denied_caller_does_not_reach_qa_catalog() {
    let fx = Fixture::with_authz(Arc::new(DenyAllAuthZ)).await;

    assert!(matches!(
        fx.service.stats(&ctx(TENANT), None).await,
        Err(DomainError::Forbidden)
    ));
    assert!(
        fx.catalog.requests().is_empty(),
        "the PDP decision is compiled before qa-catalog is asked anything"
    );
}

/// **The 24-hour block, end to end over the real repository**: both counters,
/// both rates and the card list, from rows the fixture wrote.
///
/// Legacy's KPI query (`manager/src/routes/dashboard.rs:317-348`) and its
/// recent-failures query (`:263-279`) both window on the **effective** row
/// timestamp with no phase predicate, so `Fixture::seed`'s rows — stamped `now` —
/// are inside the current window and nothing is inside the previous one.
///
/// Three mutations this catches that the pure folds cannot, because they are
/// about the wiring rather than the arithmetic:
///
/// * The two windows passed the wrong way round: the counts would land on
///   `failed_prev_24h_count` and `pass_rate_prev_24h`, and both assertions below
///   fail.
/// * The card list read with no window or no status filter: `failed_recent` would
///   carry the passed and skipped rows too, so its length and its `test_name`
///   would both be wrong.
/// * `SKIPPED` reaching the denominator: the rate would be `2/4` rather than
///   `2/3`, which the pure fold pins as a rule and this pins as the rule the
///   *service* actually gets from SQL — the repository groups by the raw status
///   column, so a `SKIPPED` group really does arrive here.
#[tokio::test]
async fn the_dashboard_reports_the_twenty_four_hour_kpis() {
    let fx = Fixture::new().await;
    let run = run_in_state(RunState::Succeeded, 0);
    let run_id = run.id;
    fx.runs.add_run(run, vec![]);
    fx.seed(TENANT, run_id, &["PASSED", "PASSED", "FAILED", "SKIPPED"])
        .await;

    let stats = fx.service.stats(&ctx(TENANT), None).await.unwrap();

    assert_eq!(stats.failed_24h_count, 1);
    assert_eq!(
        stats.pass_rate_24h,
        Some(2.0 / 3.0),
        "two passed over three counted rows; the skip is in neither half"
    );
    assert_eq!(
        stats.failed_prev_24h_count, 0,
        "nothing was stamped 24-48 hours ago"
    );
    assert_eq!(
        stats.pass_rate_prev_24h, None,
        "an empty previous window is no data rather than 0%"
    );

    assert_eq!(
        stats
            .failed_recent
            .iter()
            .map(|card| card.test_name.as_str())
            .collect::<Vec<_>>(),
        vec!["test_2"],
        "only the FAILED row, which Fixture::seed names by its index"
    );
    let card = &stats.failed_recent[0];
    assert_eq!(card.run_id, run_id);
    assert_eq!(card.test_file.as_deref(), Some("tests/t2.py"));
    assert!(
        card.finished_at.is_some(),
        "the effective instant is never absent"
    );
}

// ---------------------------------------------------------------------------
// The coverage view
// ---------------------------------------------------------------------------

/// **Every build is absent, and absent is legacy's own answer for a build with no
/// coverage summary.**
///
/// Step 0's finding, and the whole of this endpoint's behaviour today, so it is
/// asserted rather than only described.
///
/// The transcript behind it — the `COVERAGE_SUMMARY` marker, the missing `else`
/// that makes absence *legacy's* answer rather than this port's, the `product_key`
/// dedupe, the client's empty state, and the two missing upstreams — has exactly
/// one copy, on `api::rest::dto::CoverageBuildDto` — it was
/// [`super::DashboardService::coverage`]'s module header until Task 21b's doc
/// split moved it onto the type whose emptiness it explains. It is deliberately
/// not restated here: five near-copies of it existed when this task was written
/// and two had already drifted on their line ranges.
///
/// **The fixture arms the temptation rather than describing it.**
/// [`Fixture::seed`] stamps a non-blank `product_version` and a per-run
/// `app_build`, so the most legacy-faithful fabrication available — group
/// `qa_test_results` by build, skip the blanks, report something under `line_pct`
/// — produces points and fails here. With those columns `None`, as they were
/// before Task 19's review, it would have produced none and passed.
#[tokio::test]
async fn the_coverage_view_omits_every_build_because_nothing_reports_a_summary() {
    let fx = Fixture::new().await;
    let run = run_in_state(RunState::Succeeded, 0);
    let run_id = run.id;
    fx.runs.add_run(run, vec![]);
    fx.seed(TENANT, run_id, &["PASSED", "PASSED", "FAILED"])
        .await;
    fx.seed(TENANT, Uuid::from_u128(0x71), &["PASSED"]).await;

    // The arming is measured, not assumed. Nothing else in this crate reads
    // `app_build` or `product_version`, so a later tidy-up putting them back to
    // `None` would silently disarm the fabrication this test is here to catch.
    let conn = fx.db.conn().unwrap();
    let rows = OrmResultsRepository
        .list_by_run(&conn, &scope(TENANT), run_id)
        .await
        .unwrap();
    assert!(
        rows.iter()
            .all(|row| non_blank(row.app_build.as_deref())
                && non_blank(row.product_version.as_deref())),
        "the fixture must leave the build and version columns non-blank, or a \
         fabrication that skipped blanks as legacy does would pass this test: {rows:?}",
    );

    let builds = fx.service.coverage(&ctx(TENANT)).await.unwrap();

    assert!(
        builds.is_empty(),
        "a coverage point must exist only for a build with a measured summary, and \
         nothing in this gear measures one: {builds:?}",
    );
    assert_eq!(
        fx.runs.listings(),
        0,
        "the coverage view makes no cross-gear call; legacy's version listed every \
         workflow and fetched each one's logs, and a port that kept the listing would \
         pay for a page it cannot use",
    );
}

/// A caller the PDP refuses is refused, and not handed the same empty list
/// everybody else gets.
///
/// The endpoint reads no rows today, so this decision protects nothing *today* —
/// it is here because the authorization contract must not change under the
/// caller when the upstream lands. A client that works now and starts getting
/// 403 when the first real coverage point arrives is a breaking change shipped
/// silently; and the `403` this route publishes has to be reachable to be
/// honest. [`super::DashboardService::coverage`] records the counter-argument.
#[tokio::test]
async fn a_denied_caller_is_refused_rather_than_answered_with_an_empty_coverage_list() {
    let fx = Fixture::with_authz(Arc::new(DenyAllAuthZ)).await;

    assert!(matches!(
        fx.service.coverage(&ctx(TENANT)).await,
        Err(DomainError::Forbidden)
    ));
}

/// **The second aggregate asks for the same action as the first**, once.
///
/// `qa.test_result` / `list`, exactly as
/// [`the_dashboard_authorizes_under_test_result_list_once`] pins for
/// `GET /qa/v1/dashboard`. Not a duplicate of that test: what is under test here
/// is that a *second* aggregate did not acquire an action of its own, which is
/// the inference channel `domain::service::dashboard`'s header rejects
/// `view_dashboard` over — a policy able to give one aggregate a different row
/// set from the collection it aggregates. Nothing else in this crate observes the
/// string this gear actually sends.
#[tokio::test]
async fn the_coverage_view_authorizes_under_test_result_list_once() {
    let authz = Arc::new(RecordingAuthZ::default());
    let fx = Fixture::with_authz(authz.clone()).await;

    fx.service.coverage(&ctx(TENANT)).await.unwrap();

    assert_eq!(
        authz.asked(),
        vec![("qa.test_result".to_owned(), "list".to_owned())],
        "one decision, on the same resource and action as the dashboard",
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// One status group of a KPI window, as the repository returns it.
///
/// A weighted group rather than one row per status, because that is the shape
/// `effective_status_counts` produces and because a fold that summed groups
/// wrongly would otherwise be invisible at `rows: 1`.
fn status_rows(status: &str, rows: u64) -> StatusRowCount {
    StatusRowCount {
        status: status.to_owned(),
        rows,
    }
}

/// A failed result row with **every** field set to a value distinguishable from
/// every other field of its type.
///
/// That is the whole point of the fixture: `failure_card` copies nine fields, of
/// which three are `Uuid`-shaped and four are `Option<String>`, and a
/// transposition inside either group is correct Rust. `id`, `status`, `duration`,
/// `product_version`, `app_build` and `branch` are set too even though the card
/// drops them — a mapper that read `product_version` where it meant `plan_path`
/// would otherwise read a `None`.
///
/// **Three different instants**, deliberately ordered so that every candidate
/// for the card's `finished_at` is distinguishable: `run_created_at` (09:15) <
/// `run_finished_at` (11:30) < `created_at` (14:45). The card must read the first
/// two and never the third, so the `COALESCE` fallback is observable rather than
/// a tautology and the row-vs-run mix-up Ruling A closed cannot pass.
fn failed_row() -> TestResultRecord {
    TestResultRecord {
        id: Uuid::from_u128(0xF1),
        run_id: Uuid::from_u128(0xA1),
        test_file: "tests/regression/login.py".to_owned(),
        test_name: "test_login_rejects_expired_token".to_owned(),
        status: "FAILED".to_owned(),
        duration: Some("4.02s".to_owned()),
        launch_id: Some("88213".to_owned()),
        jira_key: Some("VHP-4711".to_owned()),
        product_version: Some("9.1.0".to_owned()),
        app_build: Some("9.1.0-4412".to_owned()),
        platform_id: Some(Uuid::from_u128(0xC3)),
        repo_id: Some(Uuid::from_u128(0xB2)),
        plan_path: Some("plans/regression/plan.yaml".to_owned()),
        branch: Some("release/9.1".to_owned()),
        run_finished_at: Some(datetime!(2026-08-20 11:30:00 UTC)),
        run_created_at: Some(datetime!(2026-08-20 09:15:00 UTC)),
        // Deliberately the *latest* of the three, and never the answer to
        // anything: legacy's card instant is the run's, and a `failure_card` that
        // reached for the row's own write time would read this.
        created_at: datetime!(2026-08-20 14:45:00 UTC),
    }
}

/// Legacy's own emptiness test for `app_version` and `product_key`: present and
/// not whitespace (`manager/src/routes/dashboard.rs:596-604`,
/// `Some(v) if !v.trim().is_empty()`).
fn non_blank(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}

/// `(passed, failed)` summed over every day of the daily trend.
///
/// The service tier asserts on this rather than on one day's point; `Fixture::seed`
/// says why.
fn trend_totals(stats: &DashboardStats) -> (u64, u64) {
    stats
        .daily_test_status_trend
        .iter()
        .fold((0, 0), |(passed, failed), point| {
            (passed + point.passed, failed + point.failed)
        })
}

/// One trend point folded from a run whose results carry `statuses`, one row
/// each.
///
/// Drives the **production** fold — [`super::run_trend_points`] — rather than a
/// re-implementation of it, so the assertions in
/// [`error_counts_as_failed_in_the_run_trend`] fail if the fold is wrong. One
/// `RunStatusCount` per status with `rows: 1` is the shape the repository
/// produces for a run whose statuses are all distinct.
fn run_trend_point(statuses: &[&str]) -> RunTestTrendPoint {
    let run = run_in_state(RunState::Succeeded, 0);
    let counts: Vec<RunStatusCount> = statuses
        .iter()
        .map(|status| RunStatusCount {
            run_id: run.id,
            status: (*status).to_owned(),
            rows: 1,
            run_finished_at: Some(ts()),
        })
        .collect();

    let recent: [DashboardRun; 1] = [dashboard_run(&run)];
    let mut points = run_trend_points(&recent, &counts);
    assert_eq!(points.len(), 1, "one recent run, one point");
    points.remove(0)
}

/// One run per state, newest first — ages 0, 1, 2 … seconds before the fixture
/// instant, so `list_recent_runs`' newest-first order is a fact about the
/// fixture rather than about `Uuid` bytes.
///
/// Replaces the plan's `runs_in_phases(&["Running"; 23])`: there is no `phase`
/// in this architecture, and the substitution is the subject of
/// [`only_dispatching_and_running_runs_are_active`].
fn runs_in_states(states: &[RunState]) -> Vec<Run> {
    states
        .iter()
        .enumerate()
        .map(|(nth, state)| run_in_state(*state, i64::try_from(nth).unwrap()))
        .collect()
}
