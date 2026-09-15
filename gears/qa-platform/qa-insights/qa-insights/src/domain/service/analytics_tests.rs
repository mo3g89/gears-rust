//! Tests for the analytics overview and its build-tests drill-down.
//!
//! Two tiers, the same split [`super::super::dashboard`]'s tests use:
//!
//! * the **helpers** that turn a request into a read — the plan narrowing, the
//!   row predicate, the window bound and the id collection — are pure functions
//!   and are driven directly;
//! * the **service** runs against the *real* repository on in-memory `SQLite`
//!   and the real transaction provider, with only the PDP, qa-catalog,
//!   qa-environments and the clock doubled. A repository double would absorb
//!   exactly the property that matters — that the scope the PDP compiles is the
//!   scope the analytics read runs under.
//!
//! # What these tests are *not* for
//!
//! The folds themselves are pinned in `domain::analytics::aggregates_tests` and
//! `universe_tests`, and the six query rejections in
//! `domain::analytics::query_tests`. Re-asserting a fold's arithmetic here would
//! be testing that module through a database. What is only visible from this
//! tier is the **order** the folds run in, which universe each one gets, and the
//! four reads that surround them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use qa_catalog_sdk::UniverseTest;
use qa_insights_sdk::CollectCount;
use time::Duration;
use time::OffsetDateTime;
use time::macros::{date, datetime};
use toolkit_db::DBProvider;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{
    AnalyticsService, BuildTestsQuery, narrow_to_plan, environment_ids, universe_filter,
    universe_window_start,
};
use crate::domain::analytics::aggregates::{
    AnalyticsLists, GroupBy, GroupedSummaries, PlatformGroupSummary,
};
use crate::domain::analytics::query::{NormalizedOverviewQuery, OverviewQuery, Scope};
use crate::domain::error::DomainError;
use crate::domain::repos::{
    CollectRepository, NewTestCaseResult, NewTestResult, ResultsRepository,
};
use crate::domain::service::test_support::{
    CatalogFailure, DEFAULT_BRANCH, DenyAllAuthZ, FakeCatalog, FakePlatforms, FixedClock,
    RecordingAuthZ, TODAY, TenantScopedAuthZ, ctx, ts, universe_test_full,
};
use crate::infra::storage::collect_sea_repo::OrmCollectRepository;
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::test_db::{inmem_db, scope};

const TENANT: Uuid = Uuid::from_u128(0x0A);
const PRODUCT: Uuid = Uuid::from_u128(0xB0);
const REPO: Uuid = Uuid::from_u128(0x30);
const PLAN: &str = "plans/smoke.yaml";
const OTHER_PLAN: &str = "plans/regression.yaml";
const VERSION: &str = "8.1.2";
const LINUX: Uuid = Uuid::from_u128(0x51);
const WINDOWS: Uuid = Uuid::from_u128(0x52);

// ---------------------------------------------------------------------------
// The helpers
// ---------------------------------------------------------------------------

/// `plan_id` selects on the plan's **path**, which is the mapping this task
/// owns: legacy compares against `plan.id` (`manager/src/routes/analytics.rs:845-847`)
/// and this architecture has no plan UUID at all.
#[test]
fn a_plan_scope_narrows_the_universe_to_the_entries_on_that_path() {
    let universe = vec![
        planned("tests/a.py", REPO, PLAN),
        planned("tests/b.py", REPO, OTHER_PLAN),
    ];

    let narrowed = narrow_to_plan(universe, Scope::Plan, Some(PLAN));

    assert_eq!(narrowed.len(), 1);
    assert_eq!(narrowed[0].test_file, "tests/a.py");
}

/// A path listed by two of a product's repositories selects **both**, which is
/// wider than legacy and is the only reading a caller holding one query
/// parameter can express — `narrow_to_plan`'s doc carries the argument.
#[test]
fn a_plan_path_shared_by_two_repositories_selects_both() {
    let other_repo = Uuid::from_u128(0x31);
    let universe = vec![
        planned("tests/a.py", REPO, PLAN),
        planned("tests/b.py", other_repo, PLAN),
    ];

    let narrowed = narrow_to_plan(universe, Scope::Plan, Some(PLAN));

    assert_eq!(narrowed.len(), 2);
}

/// An `all` scope ignores a `plan_id` it was given, which is legacy's behaviour
/// (`:2412` normalizes it and nothing reads it).
#[test]
fn an_all_scope_ignores_a_plan_id_it_was_handed() {
    let universe = vec![
        planned("tests/a.py", REPO, PLAN),
        planned("tests/b.py", REPO, OTHER_PLAN),
    ];

    let narrowed = narrow_to_plan(universe, Scope::All, Some(PLAN));

    assert_eq!(narrowed.len(), 2);
}

/// The row predicate is fed from the universe's own plan set, which is exactly
/// how legacy feeds its second disjunct (`:942-947`, `plan_id = ANY($3)`) — and
/// the set is **de-duplicated**, because two universe entries routinely share a
/// plan.
#[test]
fn the_row_predicate_carries_the_universes_deduplicated_plan_set() {
    let universe = vec![
        planned("tests/a.py", REPO, PLAN),
        planned("tests/b.py", REPO, PLAN),
        planned("tests/c.py", REPO, OTHER_PLAN),
    ];

    let filter = universe_filter(&universe, &normalized(GroupBy::None, None), TODAY);

    assert_eq!(filter.plans.len(), 2);
    assert_eq!(filter.product_version.as_deref(), Some(VERSION));
    assert!(
        filter.finished_only,
        "legacy's r.phase IN ('Succeeded','Failed') is widened to run_finished_at IS NOT NULL",
    );
}

/// **The bound is the wider of the two axes, and `days_heatmap` can be the wider
/// one.** `?days_trend=7&days_heatmap=30` is a legal request, and taking the
/// trend's clamp alone would blank the last three weeks of the heatmap.
#[test]
fn the_read_window_opens_on_the_oldest_day_either_chart_can_draw() {
    // Trend at its floor, heatmap at its ceiling: 30 days ending today, so the
    // oldest column is 29 days back.
    let wide_heatmap = universe_window_start(TODAY, 30, 7);
    assert_eq!(wide_heatmap, datetime!(2026-07-20 00:00:00 UTC));

    // The other way round: 90 trend days, so 89 days back.
    let wide_trend = universe_window_start(TODAY, 7, 90);
    assert_eq!(wide_trend, datetime!(2026-05-21 00:00:00 UTC));
}

/// Both clamps are applied here as well as in the parser and in the folds —
/// legacy's own "clamped twice" shape — so a caller that reached this function
/// with an unclamped value still gets a bounded window.
#[test]
fn the_read_window_clamps_both_day_counts_again() {
    assert_eq!(
        universe_window_start(TODAY, 0, 0),
        universe_window_start(TODAY, 1, 7),
        "a zero heatmap clamps to 1 and a zero trend to 7, so the trend wins",
    );
    assert_eq!(
        universe_window_start(TODAY, 9_000, 9_000),
        universe_window_start(TODAY, 30, 365),
    );
}

/// The window opens at **midnight** on that day, not at the current instant
/// minus N days: rows bucket on a calendar day, so an instant bound would drop
/// the earlier hours of the oldest column.
#[test]
fn the_read_window_opens_at_midnight_utc() {
    let opened = universe_window_start(date!(2026 - 08 - 18), 1, 7);
    assert_eq!(opened, datetime!(2026-08-12 00:00:00 UTC));
}

/// The ids are collected from **both** sections that carry one, de-duplicated
/// and ordered — the shape `EnvironmentReader::names` is signed for.
#[test]
fn the_platform_ids_are_the_distinct_ones_of_both_rendered_sections() {
    let grouped = GroupedSummaries {
        platform: vec![platform_bar(WINDOWS), platform_bar(LINUX)],
        ..GroupedSummaries::default()
    };
    let mut lists = AnalyticsLists::default();
    lists.passed.push(list_item_on(Some(LINUX)));
    lists.failed.push(list_item_on(None));

    assert_eq!(environment_ids(&grouped, &lists), vec![LINUX, WINDOWS]);
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// The overview is eight computed sections in one payload
/// (`AnalyticsOverviewResponse`, `manager/src/routes/analytics.rs:231-248`) —
/// `summary`, `lists`, `heatmap`, `trend`, `build_distribution`, `flaky`,
/// `quality_vectors`, `grouped`. The other eight fields on that struct echo the
/// query back and are not computed. Shipping seven of the eight looks complete
/// in a screenshot and is not.
///
/// The grouped assertion is controller ruling **R17**'s: the plan's own line was
/// `assert_eq!(response.grouped.component.len(), response.grouped.component.len())`,
/// which compares a value to itself and cannot fail. It is replaced by the
/// invariant the pipeline order is easy to get backwards about — the three group
/// keys are populated and the component bars sum to the **unfiltered** universe's
/// size, which they do because `build_grouped_summaries` puts every universe file
/// in exactly one component bucket.
#[tokio::test]
async fn the_overview_returns_all_eight_sections() {
    let f = fixture_with_seeded_results().await;
    let response = f
        .service
        .overview(&f.ctx, overview_query())
        .await
        .expect("overview");

    assert!(response.summary.total > 0);
    assert!(
        !response.lists.passed.is_empty()
            || !response.lists.failed.is_empty()
            || !response.lists.not_run.is_empty()
    );
    assert!(!response.heatmap.days.is_empty());
    assert!(!response.trend.points.is_empty());
    assert!(!response.build_distribution.is_empty());
    // flaky may legitimately be empty on this fixture; the grouped and
    // quality-vector sections are asserted to be present and well-formed rather
    // than merely non-empty.
    assert!(!response.grouped.component.is_empty());
    assert!(!response.grouped.tag.is_empty());
    assert!(!response.grouped.platform.is_empty());
    assert_eq!(
        response
            .grouped
            .component
            .iter()
            .map(|bar| bar.total)
            .sum::<usize>(),
        UNIVERSE_SIZE,
        "every universe file belongs to exactly one component bucket, so the bars sum to the \
         UNFILTERED universe",
    );
    assert!(response.quality_vectors.total_tests >= response.quality_vectors.unclassified_tests);
}

/// **The pipeline-order test, and the one that fails if the assembly is
/// backwards.**
///
/// `build_grouped_summaries` runs at `analytics.rs:747`, *before* the filter call
/// at `:749-750`, and the quality-vector map is never narrowed at all
/// (`:744-745`). So a request that selects one component narrows the summary, the
/// lists and the charts and leaves the group chart and the vector totals over the
/// whole universe.
///
/// Assembling it the other way collapses the chart to one bar and shrinks
/// `total_tests` to the selection — and both still render, which is why this is
/// asserted from the service rather than left to a reviewer.
#[tokio::test]
async fn the_group_chart_and_the_quality_vectors_are_not_narrowed_by_the_group_filter() {
    let f = fixture_with_seeded_results().await;

    let mut query = overview_query();
    query.group_by = Some("component".to_owned());
    query.group_value = Some("network".to_owned());
    let response = f.service.overview(&f.ctx, query).await.expect("overview");

    assert_eq!(
        response.summary.total, 1,
        "the summary is over the SELECTED component",
    );
    assert_eq!(
        response.grouped.component.len(),
        3,
        "the chart still compares every component: {:?}",
        response.grouped.component,
    );
    assert_eq!(
        response
            .grouped
            .component
            .iter()
            .map(|bar| bar.total)
            .sum::<usize>(),
        UNIVERSE_SIZE,
    );
    assert_eq!(
        response.quality_vectors.total_tests, UNIVERSE_SIZE,
        "the vector totals are a property of the suite, not of a selection",
    );
}

/// The other half of the same split: everything below the filter *is* narrowed —
/// the lists, both charts' row sets and the build distribution.
#[tokio::test]
async fn the_group_filter_narrows_the_lists_the_heatmap_and_the_build_distribution() {
    let f = fixture_with_seeded_results().await;

    let mut query = overview_query();
    query.group_by = Some("component".to_owned());
    query.group_value = Some("network".to_owned());
    let response = f.service.overview(&f.ctx, query).await.expect("overview");

    let listed =
        response.lists.passed.len() + response.lists.failed.len() + response.lists.not_run.len();
    assert_eq!(listed, 1);
    assert_eq!(response.heatmap.rows.len(), 1);
    for point in &response.trend.points {
        assert_eq!(point.passed + point.failed + point.not_run, 1);
    }
    assert!(
        response
            .build_distribution
            .iter()
            .all(|entry| entry.executed_total <= 1),
    );
}

/// **Resolved once per request, over the distinct ids** — the property
/// `EnvironmentReader::names`' slice signature exists for and that
/// `FakePlatforms::batches` is the only witness to. A service that resolved per
/// row would return the identical payload.
#[tokio::test]
async fn the_platform_names_are_resolved_in_one_batch_of_distinct_ids() {
    let f = fixture_with_seeded_results().await;
    f.platforms.add(LINUX, "Linux x86_64");

    let response = f
        .service
        .overview(&f.ctx, overview_query())
        .await
        .expect("overview");

    let batches = f.platforms.batches();
    assert_eq!(batches.len(), 1, "one call, not one per row: {batches:?}");
    assert_eq!(batches[0], vec![LINUX, WINDOWS]);
    assert_eq!(
        response.platform_names.get(&LINUX).map(String::as_str),
        Some("Linux x86_64"),
    );
    assert!(
        !response.platform_names.contains_key(&WINDOWS),
        "an id qa-environments does not resolve is absent from the map, not invented",
    );
}

/// **A qa-catalog failure fails the whole overview, and it does so
/// unconditionally.**
///
/// This is the divergence this module's header argues hardest for, and it is the
/// one the plan calls *unconditional*: legacy warns and continues on a catalog
/// problem, and [`DashboardService::stats`](super::super::dashboard::DashboardService::stats)
/// diverges from that only once some file forms a group — its ported
/// short-circuit (`dashboard.rs:498-500`) means a caller lacking the grant sees
/// `200` on a quiet deployment. **The overview has no such gate**: the universe
/// is the denominator of every number in the payload, so the read happens on
/// every request and both failure kinds fail the whole response.
///
/// Both arms, because they mean different things to a caller: `Forbidden` is a
/// missing grant and `Internal` is a broken sibling, and degrading either to an
/// empty universe would render a 200 of zeros indistinguishable from a
/// deployment with nothing synced.
///
/// **The fixture is deliberately seeded**, and the request count is asserted
/// beside the error: with no universe entries the read would still happen and
/// still fail, so the error alone would pass for a service that gated the read
/// on data the way the dashboard does.
#[tokio::test]
async fn a_qa_catalog_failure_fails_the_whole_overview() {
    for (failure, expected) in [
        (CatalogFailure::Forbidden, "Forbidden"),
        (CatalogFailure::Internal, "Internal"),
    ] {
        let f = fixture_with_seeded_results().await;
        f.catalog.fail_reads(failure);

        let err = f
            .service
            .overview(&f.ctx, overview_query())
            .await
            .expect_err("a refused or broken universe read must not be laundered into zeros");

        let matched = matches!(
            (&err, expected),
            (DomainError::Forbidden, "Forbidden") | (DomainError::Internal(_), "Internal")
        );
        assert!(
            matched,
            "{failure:?} must surface as {expected}, got {err:?}"
        );
        assert_eq!(
            f.catalog.requests().len(),
            1,
            "the read is attempted on every request, not gated on the data",
        );
    }
}

/// The drill-down reads the same universe and fails the same way — a separate
/// assertion because it is a separate method with a read of its own.
#[tokio::test]
async fn a_qa_catalog_refusal_fails_the_build_tests_drilldown_too() {
    let f = fixture_with_seeded_results().await;
    f.catalog.fail_reads(CatalogFailure::Forbidden);

    let err = f
        .service
        .build_tests(&f.ctx, build_tests_query(BUILD))
        .await
        .expect_err("the drill-down needs the same universe");

    assert!(matches!(err, DomainError::Forbidden), "got {err:?}");
}

/// A qa-environments refusal fails the **whole** overview rather than degrading
/// to an unlabelled chart. `EnvironmentReader::names`' own `# Errors` says the port
/// must not decide this and that a caller must; this is the caller deciding, the
/// same way the qa-catalog read is decided one line up.
#[tokio::test]
async fn a_platform_name_refusal_fails_the_whole_overview() {
    let f = fixture_with_seeded_results().await;
    f.platforms.fail_reads(true);

    let err = f
        .service
        .overview(&f.ctx, overview_query())
        .await
        .expect_err("a refused platform read must not be laundered into an empty map");

    assert!(matches!(err, DomainError::Forbidden), "got {err:?}");
}

/// The PEP decision is compiled **before** qa-catalog is asked anything, so a
/// caller with no grant cannot use the endpoint to learn which test files a
/// product's plans contain.
///
/// The catalog request log is asserted and not just the error: asserting the
/// error alone passes for an implementation that listed the universe first.
#[tokio::test]
async fn a_denied_caller_reads_nothing_and_does_not_reach_qa_catalog() {
    let f = Fixture::with_authz(Arc::new(DenyAllAuthZ)).await;
    f.seed_universe();

    let err = f
        .service
        .overview(&f.ctx, overview_query())
        .await
        .expect_err("a denied caller gets no overview");

    assert!(matches!(err, DomainError::Forbidden), "got {err:?}");
    assert!(
        f.catalog.requests().is_empty(),
        "qa-catalog was read before the PDP answered",
    );
}

/// The aggregate authorizes under `qa.test_result` / `list` — the same pair the
/// two collections and the dashboard use, and the reason
/// `domain::service::actions::LIST` gives for not minting a second one.
#[tokio::test]
async fn the_overview_authorizes_under_test_result_list() {
    let authz = Arc::new(RecordingAuthZ::default());
    let f = Fixture::with_authz(Arc::clone(&authz) as Arc<dyn AuthZResolverApi>).await;
    f.seed_universe();

    f.service
        .overview(&f.ctx, overview_query())
        .await
        .expect("overview");

    assert_eq!(
        authz.asked(),
        vec![("qa.test_result".to_owned(), "list".to_owned())],
    );
}

/// A query rejection is answered **before** the PDP and before either sibling is
/// asked, so the message a client is told to fix does not depend on its grants.
#[tokio::test]
async fn an_invalid_query_is_refused_before_anything_is_read() {
    let f = Fixture::new().await;
    f.seed_universe();

    let mut query = overview_query();
    query.version = "   ".to_owned();
    let err = f
        .service
        .overview(&f.ctx, query)
        .await
        .expect_err("a blank version is legacy's second rule");

    assert!(
        matches!(&err, DomainError::Validation { field, .. } if field == "version"),
        "got {err:?}",
    );
    assert!(f.catalog.requests().is_empty());
}

/// **Legacy answers 404 for a product it does not know (`:742`); this gear
/// answers a 200 of zeros.** `CatalogReader::list_universe` is deliberately not
/// an existence oracle for a product id, so "no such product" and "every
/// repository unsynced" are one answer.
#[tokio::test]
async fn an_unknown_product_is_an_empty_overview_rather_than_a_not_found() {
    let f = fixture_with_seeded_results().await;

    let mut query = overview_query();
    query.product_id = Uuid::from_u128(0xDEAD).to_string();
    let response = f.service.overview(&f.ctx, query).await.expect("overview");

    assert_eq!(response.summary.total, 0);
    assert!(response.lists.not_run.is_empty());
    assert!(response.build_distribution.is_empty());
    assert!(response.grouped.component.is_empty());
    assert_eq!(response.quality_vectors.total_tests, 0);
    assert!(
        !response.trend.points.is_empty(),
        "the axis is still drawn; it is the counters that are zero",
    );
}

/// A `product_id` that is not a UUID cannot be turned into the argument the port
/// takes, so it is a 400 rather than a 200 telling a caller with a typo that
/// their product is empty. The seventh rejection, and the only one not in
/// `domain::analytics::query`.
#[tokio::test]
async fn a_product_id_that_is_not_a_uuid_is_refused() {
    let f = Fixture::new().await;

    let mut query = overview_query();
    query.product_id = "vzlinux".to_owned();
    let err = f
        .service
        .overview(&f.ctx, query)
        .await
        .expect_err("an unparseable product id has no universe to read");

    assert!(
        matches!(&err, DomainError::Validation { field, message }
            if field == "product_id" && message == "product_id must be a UUID"),
        "got {err:?}",
    );
}

/// The echoed fields are the **normalized** query, which is legacy's too
/// (`:786-793` reads its `NormalizedOverviewQuery`): a caller that shouted its
/// scope gets it back in lower case, and a blank branch comes back as absent.
#[tokio::test]
async fn the_overview_echoes_the_normalized_query() {
    let f = fixture_with_seeded_results().await;

    let mut query = overview_query();
    query.scope = "ALL".to_owned();
    query.branch = Some("   ".to_owned());
    query.group_value = Some("  network  ".to_owned());
    let response = f.service.overview(&f.ctx, query).await.expect("overview");

    assert_eq!(response.scope, Scope::All);
    assert_eq!(response.branch, None);
    assert_eq!(response.group_value.as_deref(), Some("network"));
    assert_eq!(response.group_by, GroupBy::None);
    assert_eq!(response.version, VERSION);
}

/// The universe read asks for **this product** on the request's branch, and the
/// two `None`s on the two sides of the word mean opposite things: absent here is
/// each repository's default branch, absent on `UniverseFilter::branch` is every
/// branch. Legacy runs the same pair.
#[tokio::test]
async fn the_universe_is_read_for_the_requested_product_and_branch() {
    let f = fixture_with_seeded_results().await;

    f.service
        .overview(&f.ctx, overview_query())
        .await
        .expect("overview");

    assert_eq!(f.catalog.requests(), vec![(Some(PRODUCT), None)]);
}

/// The per-case counters come from the latest run's case rows, and the six of
/// them are what `attach_case_data` (`:1288-1395`) fills. A file with case rows
/// contributes them; a file with none contributes one synthetic case of its
/// file-level status, which is the fallback at `:1371-1376`.
#[tokio::test]
async fn the_per_case_counters_are_read_from_the_latest_runs_case_rows() {
    let f = fixture_with_seeded_results().await;

    let response = f
        .service
        .overview(&f.ctx, overview_query())
        .await
        .expect("overview");

    // tests/a.py reported three cases (a pass, a skip and an xfail); tests/b.py
    // reported none and falls back to one case of its file status. tests/c.py
    // contributes nothing despite having run, and tests/d.py never ran at all.
    //
    // The fallback (`:1371-1376`) matches on the **bucketized** status, and
    // `bucketize_status` (`:1940-1946`) returns only `PASSED`, `FAILED` or
    // `NOT_RUN` — so its `"SKIPPED" => skipped += 1` arm at `:1374` is dead code
    // in legacy, and a `SKIPPED` file reaches the `_` arm and contributes
    // nothing. (This comment said the fallback "fires only for a PASSED or
    // FAILED file", which is the right answer for the wrong reason: it read as
    // though the `SKIPPED` arm were reachable and simply did not add a case.)
    assert_eq!(response.summary.case_total, 4);
    assert_eq!(response.summary.case_passed, 1);
    assert_eq!(response.summary.case_failed, 1);
    assert_eq!(response.summary.case_skipped, 1);
    assert_eq!(response.summary.case_xfail, 1);
    assert_eq!(
        response.summary.case_expected, UNIVERSE_SIZE,
        "no collect data is seeded, so every one of the four universe files \
         falls back to its own static count - `universe_test_full`'s default \
         of 1 each (`domain::analytics::universe::expected_cases`, Task 29)",
    );

    let ticketed = response
        .lists
        .passed
        .iter()
        .find(|item| item.test_file == "tests/a.py")
        .expect("tests/a.py passed");
    assert_eq!(ticketed.case_tickets, vec!["VHP-1".to_owned()]);
}

/// The service-tier half of Task 29: [`AnalyticsService::overview`]'s own
/// read of `qa_test_case_collect`, not just the pure fold
/// [`domain::analytics::universe::expected_cases`] already has unit tests
/// for. Two things only a real repository read can prove: the branch
/// substitution (`analytics.rs:2682`,
/// `branch.unwrap_or(DEFAULT_COLLECT_BRANCH)`) and that a per-file collect
/// count reaches `summary.case_expected` end to end, mixed with the static
/// fallback for the files the collect job never reported.
#[tokio::test]
async fn an_exact_collect_count_reaches_the_overviews_case_expected_through_the_default_branch() {
    let f = Fixture::new().await;
    f.seed_universe();

    // `tests/a.py` is collected on "main" — the same branch `overview_query`'s
    // absent `branch` resolves to (`Fixture::with_authz`'s
    // `DEFAULT_BRANCH.to_owned()`, threaded as `default_collect_branch`) — so
    // its exact 7 must override its static 1.
    f.seed_collect_count(REPO, DEFAULT_BRANCH, "tests/a.py", 7)
        .await;
    // `tests/b.py` is collected too, but on a branch this request does not
    // name and therefore does not read: its 99 must not appear anywhere in
    // the total, and `tests/b.py` must fall back to its own static count
    // exactly as if no collect row for it existed at all.
    f.seed_collect_count(REPO, "release/2.0", "tests/b.py", 99)
        .await;

    let response = f
        .service
        .overview(&f.ctx, overview_query())
        .await
        .expect("overview");

    assert_eq!(
        response.summary.case_expected,
        7 + 1 + 1 + 1,
        "a.py's exact 7, plus b.py/c.py/d.py's static 1 each - b.py's 99 on \
         the other branch must not be counted",
    );
}

/// A universe whose files never named a latest run leaves the six counters at
/// zero — legacy's early return at `:1304-1306` — rather than folding a
/// successful read of nothing, which is a different value.
#[tokio::test]
async fn a_universe_with_no_executed_run_leaves_the_per_case_counters_at_zero() {
    let f = Fixture::new().await;
    f.seed_universe();

    let response = f
        .service
        .overview(&f.ctx, overview_query())
        .await
        .expect("overview");

    assert_eq!(response.summary.total, UNIVERSE_SIZE);
    assert_eq!(response.summary.not_run, UNIVERSE_SIZE);
    assert_eq!(response.summary.case_total, 0);
}

/// A plan-scoped overview reads only that plan's universe **and** binds only that
/// plan's rows, so a second plan's tests are neither counted nor listed.
#[tokio::test]
async fn a_plan_scoped_overview_sees_only_that_plans_tests() {
    let f = fixture_with_seeded_results().await;
    f.catalog.add(
        PRODUCT,
        DEFAULT_BRANCH,
        planned("tests/e.py", REPO, OTHER_PLAN),
    );

    let mut query = overview_query();
    query.scope = "plan".to_owned();
    query.plan_id = Some(OTHER_PLAN.to_owned());
    let response = f.service.overview(&f.ctx, query).await.expect("overview");

    assert_eq!(response.summary.total, 1);
    assert_eq!(response.lists.not_run.len(), 1);
    assert_eq!(response.lists.not_run[0].test_file, "tests/e.py");
}

// ---------------------------------------------------------------------------
// The build-tests drill-down
// ---------------------------------------------------------------------------

/// The drill-down lists the tests whose latest run executed against one build,
/// with the status that run reported — `api_build_tests` (`:425-452`).
#[tokio::test]
async fn the_build_tests_drilldown_lists_that_builds_tests() {
    let f = fixture_with_seeded_results().await;

    let items = f
        .service
        .build_tests(&f.ctx, build_tests_query(BUILD))
        .await
        .expect("build tests");

    let files: Vec<&str> = items.iter().map(|item| item.test_file.as_str()).collect();
    assert_eq!(files, vec!["tests/b.py", "tests/a.py", "tests/c.py"]);
    assert_eq!(items[0].status, "FAILED");
    assert_eq!(items[0].run_finished_at, ts());
}

/// A build nothing ran against is an empty array, not an error — the same shape
/// legacy answers with, and the reason the endpoint declares no 404.
#[tokio::test]
async fn a_build_no_run_executed_against_is_an_empty_list() {
    let f = fixture_with_seeded_results().await;

    let items = f
        .service
        .build_tests(&f.ctx, build_tests_query("9.9.9-0001"))
        .await
        .expect("build tests");

    assert!(items.is_empty());
}

/// **`build` is checked before every other rule.** Legacy's `:373-376` runs
/// before the `normalize_overview_query` call at `:379`, so a request that is
/// wrong in both ways is told about the build — the reverse of what reading
/// `normalize_overview_query` alone would suggest.
#[tokio::test]
async fn a_blank_build_is_reported_ahead_of_every_shared_rule() {
    let f = Fixture::new().await;

    let mut query = build_tests_query("   ");
    query.product_id = String::new();
    let err = f
        .service
        .build_tests(&f.ctx, query)
        .await
        .expect_err("a blank build is refused");

    assert!(
        matches!(&err, DomainError::Validation { field, message }
            if field == "build" && message == "build is required"),
        "the blank product_id must not win the race: {err:?}",
    );
}

/// The drill-down applies the same group filter the overview does (`:405-417`),
/// so opening a bar from a filtered overview does not widen the selection.
#[tokio::test]
async fn the_build_tests_drilldown_honours_the_group_filter() {
    let f = fixture_with_seeded_results().await;

    let mut query = build_tests_query(BUILD);
    query.group_by = Some("component".to_owned());
    query.group_value = Some("network".to_owned());
    let items = f
        .service
        .build_tests(&f.ctx, query)
        .await
        .expect("build tests");

    let files: Vec<&str> = items.iter().map(|item| item.test_file.as_str()).collect();
    assert_eq!(files, vec!["tests/c.py"]);
}

/// The drill-down never resolves a platform name: `BuildTestDetail` carries no
/// platform, so the cross-gear call would be a round trip for nothing.
#[tokio::test]
async fn the_build_tests_drilldown_reads_no_platform_names() {
    let f = fixture_with_seeded_results().await;

    f.service
        .build_tests(&f.ctx, build_tests_query(BUILD))
        .await
        .expect("build tests");

    assert!(f.platforms.batches().is_empty());
}

// ---------------------------------------------------------------------------
// The three plan drill-downs
// ---------------------------------------------------------------------------

/// One row of a plan-scoped fixture, built independently of
/// [`Fixture::seed_results`] so each test controls the version, the platform,
/// the timestamps and the status precisely.
fn plan_row(
    test_name: &str,
    status: &str,
    version: Option<&str>,
    platform: Option<Uuid>,
    jira_key: Option<&str>,
    finished_at: OffsetDateTime,
) -> NewTestResult {
    NewTestResult {
        test_file: test_name.to_owned(),
        test_name: test_name.to_owned(),
        status: status.to_owned(),
        duration: None,
        launch_id: None,
        jira_key: jira_key.map(str::to_owned),
        product_version: version.map(str::to_owned),
        app_build: None,
        environment_id: platform,
        repo_id: Some(REPO),
        plan_path: Some(PLAN.to_owned()),
        branch: None,
        run_finished_at: Some(finished_at),
        run_created_at: Some(finished_at - Duration::hours(1)),
    }
}

async fn seed_plan_run(f: &Fixture, run_id: Uuid, files: Vec<NewTestResult>) {
    let conn = f.db.conn().unwrap();
    OrmResultsRepository
        .upsert_run_results(&conn, &scope(TENANT), TENANT, run_id, files, vec![])
        .await
        .unwrap();
}

/// `api_plan_tests` (`analytics.rs:2434-2483`). Two runs of `test_login` prove
/// the arithmetic and the "latest wins" rule together: the older run's PASSED
/// contributes to the counters but not to `last_status`/`last_version`, and
/// `test_logout`'s lone `ERROR` row proves `total_runs` is unconditional while
/// `pass_count`/`fail_count` are the **literal** `PASSED`/`FAILED` match
/// [`crate::domain::repos::PlanExecRow::status`]'s header records — an `ERROR`
/// moves neither counter.
#[tokio::test]
async fn the_plan_tests_drilldown_aggregates_across_runs_and_picks_the_latest_row() {
    let f = Fixture::new().await;
    seed_plan_run(
        &f,
        Uuid::from_u128(0x60),
        vec![plan_row(
            "test_login",
            "PASSED",
            Some("8.1.2"),
            Some(LINUX),
            Some("VHP-1"),
            ts() - Duration::hours(2),
        )],
    )
    .await;
    seed_plan_run(
        &f,
        Uuid::from_u128(0x61),
        vec![
            plan_row(
                "test_login",
                "FAILED",
                Some("8.1.3"),
                Some(WINDOWS),
                None,
                ts(),
            ),
            plan_row("test_logout", "ERROR", Some("8.1.3"), None, None, ts()),
        ],
    )
    .await;

    let response = f
        .service
        .plan_tests(&f.ctx, PLAN)
        .await
        .expect("plan tests");

    let names: Vec<&str> = response
        .items
        .iter()
        .map(|item| item.test_name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["test_login", "test_logout"],
        "ordered by test_name, legacy's own ORDER BY"
    );

    let login = &response.items[0];
    assert_eq!(login.total_runs, 2);
    assert_eq!(login.pass_count, 1);
    assert_eq!(login.fail_count, 1);
    assert_eq!(login.last_status, "FAILED", "the newer run wins");
    assert_eq!(login.last_version.as_deref(), Some("8.1.3"));
    assert_eq!(login.last_environment_id, Some(WINDOWS));
    assert_eq!(
        login.jira_key, None,
        "the latest row's jira_key, not the older row's VHP-1"
    );

    let logout = &response.items[1];
    assert_eq!(logout.total_runs, 1);
    assert_eq!(
        (logout.pass_count, logout.fail_count),
        (0, 0),
        "ERROR is counted in total_runs but matches neither literal"
    );
}

/// Platform names are resolved for exactly the ids the response renders —
/// `last_environment_id` — in one batch, the same discipline
/// [`the_platform_names_are_resolved_in_one_batch_of_distinct_ids`] pins for
/// the overview. `LINUX` never appears as any test's *latest* platform here, so
/// it must not be asked about.
#[tokio::test]
async fn the_plan_tests_drilldown_resolves_only_the_rendered_platforms() {
    let f = Fixture::new().await;
    seed_plan_run(
        &f,
        Uuid::from_u128(0x60),
        vec![plan_row(
            "test_login",
            "PASSED",
            Some("8.1.2"),
            Some(LINUX),
            None,
            ts() - Duration::hours(2),
        )],
    )
    .await;
    seed_plan_run(
        &f,
        Uuid::from_u128(0x61),
        vec![plan_row(
            "test_login",
            "FAILED",
            Some("8.1.3"),
            Some(WINDOWS),
            None,
            ts(),
        )],
    )
    .await;

    f.service
        .plan_tests(&f.ctx, PLAN)
        .await
        .expect("plan tests");

    assert_eq!(
        f.platforms.batches(),
        vec![vec![WINDOWS]],
        "only the latest row's platform is asked about"
    );
}

/// A plan drill-down sees only its own plan's rows — the same isolation
/// `a_plan_scoped_overview_sees_only_that_plans_tests` pins for the overview,
/// checked here directly against the repository read rather than through a
/// catalog-derived universe (this endpoint reads no universe at all).
#[tokio::test]
async fn the_plan_tests_drilldown_does_not_see_another_plans_rows() {
    let f = Fixture::new().await;
    seed_plan_run(
        &f,
        Uuid::from_u128(0x60),
        vec![NewTestResult {
            plan_path: Some(OTHER_PLAN.to_owned()),
            ..plan_row("test_other", "PASSED", Some("8.1.2"), None, None, ts())
        }],
    )
    .await;

    let response = f
        .service
        .plan_tests(&f.ctx, PLAN)
        .await
        .expect("plan tests");

    assert!(response.items.is_empty());
}

/// **Phase B fix wave, Finding 9**: `plan_id` is the one required string
/// query parameter on this phase's endpoints that reached its repository
/// verbatim, unlike `product_id`, `version`, `build`, the collect route's
/// `branch` and saved views' `name`/`scope`. `?plan_id=` (and
/// whitespace-only) must now be a 400, matching that rule, on all three
/// drill-downs — they share [`AnalyticsService::plan_rows`], so one caller
/// per method is enough to prove each is wired through it.
#[tokio::test]
async fn a_blank_plan_id_is_refused_on_all_three_drilldowns() {
    let f = Fixture::new().await;

    for blank in ["", "   "] {
        let err = f
            .service
            .plan_tests(&f.ctx, blank)
            .await
            .expect_err("plan_tests must refuse a blank plan_id");
        assert!(
            matches!(&err, DomainError::Validation { field, .. } if field == "plan_id"),
            "plan_tests: got {err:?}",
        );

        let err = f
            .service
            .plan_builds(&f.ctx, blank)
            .await
            .expect_err("plan_builds must refuse a blank plan_id");
        assert!(
            matches!(&err, DomainError::Validation { field, .. } if field == "plan_id"),
            "plan_builds: got {err:?}",
        );

        let err = f
            .service
            .plan_test_history(&f.ctx, blank)
            .await
            .expect_err("plan_test_history must refuse a blank plan_id");
        assert!(
            matches!(&err, DomainError::Validation { field, .. } if field == "plan_id"),
            "plan_test_history: got {err:?}",
        );
    }
}

/// `api_plan_builds` (`analytics.rs:2486-2527`): grouped by `product_version`,
/// `None` rendered as `"unknown"` and sorted last — legacy's `COALESCE(...,
/// 'unknown')` and its `ORDER BY r.app_version` on the raw column. The `SKIPPED`
/// counter, unique to this endpoint among the three, is exercised alongside
/// `PASSED`/`FAILED`.
#[tokio::test]
async fn the_plan_builds_drilldown_groups_by_version_with_unknown_sorted_last() {
    let f = Fixture::new().await;
    seed_plan_run(
        &f,
        Uuid::from_u128(0x60),
        vec![
            plan_row("test_a", "PASSED", Some("8.1.3"), None, None, ts()),
            plan_row("test_b", "FAILED", Some("8.1.2"), None, None, ts()),
            plan_row("test_c", "SKIPPED", None, None, None, ts()),
        ],
    )
    .await;

    let builds = f
        .service
        .plan_builds(&f.ctx, PLAN)
        .await
        .expect("plan builds");

    let labels: Vec<&str> = builds.iter().map(|b| b.build.as_str()).collect();
    assert_eq!(labels, vec!["8.1.2", "8.1.3", "unknown"]);
    assert_eq!(
        (
            builds[0].total,
            builds[0].passed,
            builds[0].failed,
            builds[0].skipped
        ),
        (1, 0, 1, 0),
    );
    assert_eq!(
        (
            builds[2].total,
            builds[2].passed,
            builds[2].failed,
            builds[2].skipped
        ),
        (1, 0, 0, 1),
        "the unknown group is test_c's SKIPPED row",
    );
}

/// `api_plan_test_history` (`analytics.rs:2530-2572`): each test's own results
/// are newest first, and `build` stays an `Option` all the way to the wire —
/// unlike [`PlanBuildDistribution::build`], nothing here coalesces a missing
/// version to `"unknown"`. See [`PlanTestHistory`]'s header for why the outer
/// order (by `test_name`) is this port's rather than legacy's own unspecified
/// `HashMap` order.
#[tokio::test]
async fn the_plan_test_history_drilldown_orders_each_tests_runs_newest_first() {
    let f = Fixture::new().await;
    seed_plan_run(
        &f,
        Uuid::from_u128(0x60),
        vec![plan_row(
            "test_login",
            "PASSED",
            Some("8.1.2"),
            None,
            None,
            ts() - Duration::hours(2),
        )],
    )
    .await;
    seed_plan_run(
        &f,
        Uuid::from_u128(0x61),
        vec![plan_row("test_login", "FAILED", None, None, None, ts())],
    )
    .await;

    let history = f
        .service
        .plan_test_history(&f.ctx, PLAN)
        .await
        .expect("plan test history");

    assert_eq!(history.len(), 1);
    assert_eq!(history[0].test_name, "test_login");
    let entries: Vec<(Option<&str>, &str)> = history[0]
        .results
        .iter()
        .map(|e| (e.build.as_deref(), e.status.as_str()))
        .collect();
    assert_eq!(
        entries,
        vec![(None, "FAILED"), (Some("8.1.2"), "PASSED")],
        "newest first, and a missing version stays null rather than becoming unknown",
    );
}

/// The three drill-downs share the default 90-day window
/// [`AnalyticsService::plan_rows`]'s doc argues for — the NFR bound this task
/// adds where legacy has none. A row stamped outside it is invisible to all
/// three, exactly as [`the_read_window_opens_on_the_oldest_day_either_chart_can_draw`]
/// pins the same bound for the overview.
#[tokio::test]
async fn a_plan_drilldowns_window_excludes_a_row_older_than_ninety_days() {
    let f = Fixture::new().await;
    seed_plan_run(
        &f,
        Uuid::from_u128(0x60),
        vec![plan_row(
            "test_stale",
            "PASSED",
            Some("8.1.2"),
            None,
            None,
            datetime!(2026-05-20 12:00:00 UTC),
        )],
    )
    .await;

    let response = f
        .service
        .plan_tests(&f.ctx, PLAN)
        .await
        .expect("plan tests");
    assert!(
        response.items.is_empty(),
        "2026-05-20 is before the window's 2026-05-21 open: {response:?}"
    );
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// How many universe entries [`Fixture::seed_universe`] registers. Named because
/// three tests assert a sum against it and a literal `4` in each would be four
/// places to update.
const UNIVERSE_SIZE: usize = 4;

/// The build every seeded run executed against.
const BUILD: &str = "9.1.0-4412";

struct Fixture {
    db: toolkit_db::Db,
    catalog: Arc<FakeCatalog>,
    platforms: Arc<FakePlatforms>,
    ctx: SecurityContext,
    service: AnalyticsService<OrmResultsRepository, OrmCollectRepository>,
}

impl Fixture {
    async fn with_authz(authz: Arc<dyn AuthZResolverApi>) -> Self {
        let db = inmem_db().await;
        let provider = Arc::new(DBProvider::<DomainError>::new(db.clone()));
        let catalog = Arc::new(FakeCatalog::default());
        let platforms = Arc::new(FakePlatforms::default());
        let service = AnalyticsService::new(
            provider,
            OrmResultsRepository,
            Arc::clone(&catalog) as Arc<dyn crate::domain::ports::CatalogReader>,
            Arc::clone(&platforms) as Arc<dyn crate::domain::ports::EnvironmentReader>,
            Arc::new(FixedClock::default()) as Arc<dyn crate::domain::ports::Clock>,
            PolicyEnforcer::new(authz),
            OrmCollectRepository,
            DEFAULT_BRANCH.to_owned(),
        );
        Self {
            db,
            catalog,
            platforms,
            ctx: ctx(TENANT),
            service,
        }
    }

    /// Seed one collect-job report directly through
    /// [`CollectRepository::upsert_count`] — bypassing the service, exactly as
    /// [`Self::seed_results`] bypasses it for `qa_test_results` — so a test can
    /// prove the exact count reaches `summary.case_expected` through the real
    /// read path rather than asserting the pure fold a second time.
    async fn seed_collect_count(&self, repo_id: Uuid, branch: &str, test_file: &str, cases: u32) {
        let conn = self.db.conn().unwrap();
        OrmCollectRepository
            .upsert_count(
                &conn,
                &scope(TENANT),
                TENANT,
                CollectCount {
                    repo_id,
                    branch: branch.to_owned(),
                    test_file: test_file.to_owned(),
                    case_count: cases,
                    collected_at: ts(),
                },
            )
            .await
            .expect("seed collect count");
    }

    async fn new() -> Self {
        Self::with_authz(Arc::new(TenantScopedAuthZ)).await
    }

    /// Four universe entries under one product and one plan, chosen so that all
    /// three group breakdowns are non-empty and none of them is degenerate:
    /// two components plus a file with none (which groups under `unknown`), two
    /// tags plus a file with none (`untagged`), and one file declaring Quality
    /// Vectors so the vector section is not all-unclassified.
    fn seed_universe(&self) {
        for (file, component, tags, vectors) in [
            (
                "tests/a.py",
                Some("cluster"),
                &["smoke"][..],
                &["Security"][..],
            ),
            ("tests/b.py", Some("cluster"), &["smoke"][..], &[][..]),
            ("tests/c.py", Some("network"), &[][..], &[][..]),
            ("tests/d.py", None, &["regression"][..], &[][..]),
        ] {
            let mut test = planned(file, REPO, PLAN);
            test.component = component.map(str::to_owned);
            test.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
            test.quality_vectors = vectors.iter().map(|v| (*v).to_owned()).collect();
            self.catalog.add(PRODUCT, DEFAULT_BRANCH, test);
        }
    }

    /// One finished run covering three of the four universe files, plus the
    /// per-case rows of one of them.
    ///
    /// `tests/d.py` is deliberately left without a row, so the `not_run` list and
    /// the `NOT_RUN` bucket are exercised rather than assumed.
    async fn seed_results(&self) {
        let run_id = Uuid::from_u128(0x0F);
        let conn = self.db.conn().unwrap();
        let files = [
            ("tests/a.py", "PASSED", LINUX),
            ("tests/b.py", "FAILED", WINDOWS),
            ("tests/c.py", "SKIPPED", LINUX),
        ]
        .into_iter()
        .map(|(file, status, platform)| NewTestResult {
            test_file: file.to_owned(),
            test_name: file.to_owned(),
            status: status.to_owned(),
            duration: None,
            launch_id: None,
            jira_key: None,
            product_version: Some(VERSION.to_owned()),
            app_build: Some(BUILD.to_owned()),
            environment_id: Some(platform),
            repo_id: Some(REPO),
            plan_path: Some(PLAN.to_owned()),
            branch: None,
            run_finished_at: Some(ts()),
            run_created_at: Some(ts() - Duration::hours(1)),
        })
        .collect();

        // Three cases for one file, spanning three of the six case-level
        // statuses — the vocabulary that is wider than the file-level one.
        let cases = [
            ("test_one", "PASSED", None),
            ("test_two", "SKIPPED", Some("VHP-1")),
            ("test_three", "XFAIL", None),
        ]
        .into_iter()
        .map(|(name, status, ticket)| NewTestCaseResult {
            test_file: "tests/a.py".to_owned(),
            nodeid: format!("tests/a.py::{name}"),
            name: name.to_owned(),
            status: status.to_owned(),
            duration: None,
            reason: None,
            ticket: ticket.map(str::to_owned),
        })
        .collect();

        OrmResultsRepository
            .upsert_run_results(&conn, &scope(TENANT), TENANT, run_id, files, cases)
            .await
            .unwrap();
    }
}

/// The fixture the plan's Step 1 test names: a universe, a run over it, and the
/// per-case rows of one file.
async fn fixture_with_seeded_results() -> Fixture {
    let f = Fixture::new().await;
    f.seed_universe();
    f.seed_results().await;
    f
}

/// The smallest request the endpoint accepts: the three required parameters and
/// nothing else, so every default is exercised.
fn overview_query() -> OverviewQuery {
    OverviewQuery {
        product_id: PRODUCT.to_string(),
        version: VERSION.to_owned(),
        scope: "all".to_owned(),
        ..OverviewQuery::default()
    }
}

fn build_tests_query(build: &str) -> BuildTestsQuery {
    BuildTestsQuery {
        product_id: PRODUCT.to_string(),
        version: VERSION.to_owned(),
        scope: "all".to_owned(),
        build: build.to_owned(),
        ..BuildTestsQuery::default()
    }
}

/// [`universe_test_full`] with the plan identity chosen, which the row predicate
/// keys on.
fn planned(test_file: &str, repo_id: Uuid, plan_path: &str) -> UniverseTest {
    UniverseTest {
        repo_id,
        plan_path: plan_path.to_owned(),
        ..universe_test_full(test_file, test_file, None)
    }
}

fn normalized(group_by: GroupBy, group_value: Option<&str>) -> NormalizedOverviewQuery {
    NormalizedOverviewQuery {
        product_id: PRODUCT.to_string(),
        version: VERSION.to_owned(),
        scope: Scope::All,
        plan_id: None,
        branch: None,
        days_heatmap: 7,
        days_trend: 90,
        group_by,
        group_value: group_value.map(str::to_owned),
    }
}

fn platform_bar(environment_id: Uuid) -> PlatformGroupSummary {
    PlatformGroupSummary {
        environment_id,
        total: 1,
        passed: 1,
        failed: 0,
        not_run: 0,
    }
}

fn list_item_on(
    environment_id: Option<Uuid>,
) -> crate::domain::analytics::aggregates::AnalyticsListItem {
    crate::domain::analytics::aggregates::AnalyticsListItem {
        test_file: "tests/a.py".to_owned(),
        test_name: "tests/a.py".to_owned(),
        component: None,
        tags: Vec::new(),
        plan: crate::domain::analytics::PlanRef {
            repo_id: REPO,
            plan_path: PLAN.to_owned(),
        },
        plan_name: "Smoke".to_owned(),
        versions: Vec::new(),
        last_status: "PASSED",
        last_environment_id: environment_id,
        last_run_id: None,
        last_build: None,
        last_run_finished_at: None,
        pass_count: 0,
        fail_count: 0,
        skipped_count: 0,
        total_runs: 0,
        case_status: None,
        case_tickets: Vec::new(),
    }
}
