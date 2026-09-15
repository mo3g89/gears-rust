//! The analytics overview and its build-tests drill-down —
//! `GET /qa/v1/analytics/overview` and `GET /qa/v1/analytics/build-tests`.
//!
//! The port of legacy's `api_overview` (`manager/src/routes/analytics.rs:360`),
//! `build_overview` (`:728`), `load_universe_and_rows` (`:805`) and
//! `api_build_tests` (`:369`). Everything this module does is *ordering*: the
//! rules are in [`crate::domain::analytics`], all of them pure, and this is the
//! one place that reads a clock, reaches two sibling gears and a database, and
//! runs the folds in legacy's sequence.
//!
//! # The pipeline order is the whole risk, and it is not the order it looks like
//!
//! [`apply_universe_group_filter`] does **not** narrow everything downstream of
//! it. Legacy calls it once (`:749-750`) and then hands *two different
//! universes* to two different sets of folds:
//!
//! | Fold | Universe / rows it gets | Legacy line |
//! |---|---|---|
//! | [`build_grouped_summaries`] | the **unfiltered** universe and **all** rows | `:747` — *before* the filter call |
//! | [`build_quality_vector_summary`] | never narrowed at all | its map is built in `load_universe_and_rows` (`:744-745`, fold at `:871-882`) |
//! | [`build_latest_map`], [`build_stats_map`], the per-case roll-up, [`summarize`], [`build_lists`], [`build_heatmap`], [`build_trend`], the build distribution and [`build_flaky`] | `filtered_universe` + `rows_for_scope` | `:751-783` |
//!
//! Assembling it the other way collapses the group chart to a single bar — in a
//! chart whose only job is comparison — and shrinks
//! [`QualityVectorSummary::total_tests`] to the selection. Both still render, so
//! neither failure is visible in a screenshot.
//! [`crate::domain::analytics::aggregates`]' header carries the same table.
//!
//! # One clock read per request
//!
//! [`Clock::today`] is read **once** in each public method and the same [`Date`]
//! reaches the heatmap, the trend and the flaky window. Legacy calls
//! the clock inside each of the three — `build_heatmap` (`:1453`) and
//! `build_trend` (`:1497`) through `recent_days` (`:2078`), `build_flaky`
//! directly (`:1657`) — so a
//! request that crosses midnight between two of them draws a heatmap whose last
//! column is a day behind the trend's last point.
//! [`crate::domain::ports::clock`]'s header carries the argument for the port and
//! for the folds taking a `Date` rather than the port itself.
//!
//! # Four things legacy does that this schema cannot, each decided here
//!
//! This is the first task that turns an analytics request into a read, so it is
//! the first that has to answer them. Each is recorded at the code that makes the
//! decision, and named here so a reader can find all four:
//!
//! 1. **`product_key`** — legacy's all-scope predicate is
//!    `r.product_key = $2 OR (r.product_key IS NULL AND r.plan_id = ANY($3))`
//!    (`:992-995`) and **neither column exists here**. See
//!    [`universe_filter`], which resolves the second disjunct and drops the
//!    first. [`UniverseFilter`] is *not* extended: its header forbids a field
//!    with no call site, and this mapping needs none.
//! 2. **`plan_id`** — an opaque string in legacy, matched against
//!    `run_results.plan_id`; here a plan's identity is the
//!    `(repo_id, plan_path)` pair. See [`narrow_to_plan`].
//! 3. **The unknown product.** Legacy 404s when its product registry does not
//!    know `product_id` (`:742`). This gear has no registry to ask and
//!    [`CatalogReader::list_universe`] is deliberately not an existence oracle,
//!    so an unknown product is an **empty universe and a 200 of zeros**. See
//!    [`AnalyticsService::overview`]' `# Errors`.
//! 4. **The `since` bound.** Legacy's `ExecRowRaw` queries are unwindowed
//!    (`:964`, `:991`) and every window is applied in memory. See
//!    [`universe_window_start`].
//!
//! # Two upstream failures fail the whole payload, unconditionally
//!
//! [`crate::domain::service::dashboard`] made the same choice for its own
//! qa-catalog read and its 403 is *conditional on data* — the ported
//! short-circuit (`dashboard.rs:498-500`) means the catalog is read only once
//! some file forms a group. **The overview has no such gate**: the universe is
//! the denominator of every number in the payload, so it is read on every
//! request and a qa-catalog `Forbidden` or `Internal` is a `Forbidden` or
//! `Internal` on the whole response, every time. That is decided deliberately
//! rather than degraded to an empty universe, for the reason this crate applies
//! everywhere: an empty universe is indistinguishable from a deployment with
//! nothing synced, and a broken upstream must not look like an idle one.
//!
//! The qa-environments read is the second, and it *is* data-conditional — but
//! only in the trivial sense that it is skipped when nothing to resolve exists.
//! [`AnalyticsService::overview`]' `# Errors` states both.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_catalog_sdk::UniverseTest;
use qa_insights_sdk::CollectCount;
use time::{Date, OffsetDateTime};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::analytics::aggregates::{
    AnalyticsLists, BuildLastRunDistribution, BuildTestDetail, CaseData, FlakyTest, GroupBy,
    GroupedSummaries, HeatmapData, OverviewSummary, QualityVectorSummary, TrendData,
    apply_universe_group_filter, build_case_data, build_flaky, build_grouped_summaries,
    build_heatmap, build_last_run_build_distribution, build_lists, build_quality_vector_summary,
    build_stats_map, build_test_details, build_trend, heatmap_days, recent_days, summarize,
    trend_days,
};
use crate::domain::analytics::query::{
    NormalizedOverviewQuery, OverviewQuery, Scope, normalize_build, normalize_overview_query,
};
use crate::domain::analytics::universe::{build_latest_map, expected_cases, resolve_rows};
use crate::domain::analytics::{ExecRow, PlanRef, UniverseFilter};
use crate::domain::error::DomainError;
use crate::domain::ports::{CatalogReader, Clock, EnvironmentReader};
use crate::domain::repos::{CollectRepository, PlanExecRow, ResultsRepository};
use crate::domain::service::{DbProvider, actions, resources};

/// Everything `GET /qa/v1/analytics/overview` answers with.
///
/// Legacy's `AnalyticsOverviewResponse` (`analytics.rs:231-248`): **eight
/// computed sections** and the query echoed back beside them. Shipping seven of
/// the eight looks complete in a screenshot and is not, which is what
/// `the_overview_returns_all_eight_sections` exists to prevent.
///
/// # Two of legacy's sixteen fields are absent, and one is added
///
/// * **`product_key` is gone.** Legacy fills it from the product it resolved
///   (`:787`) and VHP-319 deleted that model; nothing in this subsystem carries
///   a product key. Absent rather than echoed back as the `product_id`, which
///   would be a different value under the same name.
/// * **[`Self::platform_names`] is added**, and it is not a section — it is the
///   label side of [`crate::domain::analytics::ExecRow::environment_id`]. Legacy's
///   row carried a platform *name*; this one carries an id, so a name has to be
///   read from qa-environments and the read is the service's while the rendering
///   is the DTO's. See that field.
///
/// The echoed fields are the **normalized** query rather than the raw one, which
/// is legacy's too (`:786-793` reads `query`, the `NormalizedOverviewQuery`): a
/// caller that sent `?scope=ALL&group_by=` gets `all` back, and a caller that
/// sent a blank `branch` gets `null`.
#[derive(Clone, Debug)]
pub struct AnalyticsOverview {
    /// Echoed. Still the opaque string the caller sent, trimmed — see
    /// [`crate::domain::analytics::query`]' header for why it is not a `Uuid`.
    pub product_id: String,
    /// Echoed, trimmed.
    pub version: String,
    /// Echoed, parsed. Legacy renders it back through `scope_to_str` (`:2088`).
    pub scope: Scope,
    /// Echoed, trimmed, blank dropped. Present under an `all` scope too, exactly
    /// as legacy carries it — it is simply never read there.
    pub plan_id: Option<String>,
    /// Echoed, trimmed, blank dropped. `None` is every branch.
    pub branch: Option<String>,
    /// Echoed, parsed. Legacy renders it back through `group_to_str` (`:2095`).
    pub group_by: GroupBy,
    /// Echoed, trimmed, blank dropped.
    pub group_value: Option<String>,
    /// The universe partitioned three ways, plus the six per-case counters.
    pub summary: OverviewSummary,
    /// The same universe as three sorted lists of items.
    pub lists: AnalyticsLists,
    /// One row per test, one cell per day of the heatmap window.
    pub heatmap: HeatmapData,
    /// One point per day of the trend window, each totalling the universe.
    pub trend: TrendData,
    /// One entry per build named by some test's latest run.
    pub build_distribution: Vec<BuildLastRunDistribution>,
    /// The tests that both passed and failed inside the trend window.
    pub flaky: Vec<FlakyTest>,
    /// The Quality Vectors the **unfiltered** universe declares.
    pub quality_vectors: QualityVectorSummary,
    /// The three group breakdowns, over the **unfiltered** universe and rows.
    pub grouped: GroupedSummaries,
    /// The display name of every platform id this payload mentions, for the ids
    /// qa-environments resolved.
    ///
    /// # Why the map is here rather than the names being substituted in place
    ///
    /// Two sections carry a platform id — [`GroupedSummaries::platform`], whose
    /// entries *are* platforms, and the three lists' `last_environment_id` — and the
    /// resolution is **one** cross-gear call over the distinct ids of both
    /// ([`EnvironmentReader::names`]' signature is shaped to make a per-row lookup
    /// inexpressible). Substituting in place would mean two service-tier mirrors
    /// of Task 23's fold output whose only difference is a `String` where a
    /// `Uuid` was; carrying the map instead keeps the folds' types and puts the
    /// join where the rest of this gear's label rendering already lives.
    ///
    /// **An id absent from the map resolved to nothing** — deleted since the run
    /// executed, or in another tenant, which [`EnvironmentReader::names`]
    /// deliberately makes indistinguishable. What that renders as is the DTO's
    /// decision and not this type's; the port's own header refuses to invent a
    /// label here for the same reason.
    pub platform_names: HashMap<Uuid, String>,
}

/// The build-tests drill-down's query — legacy's `AnalyticsBuildTestsQuery`
/// (`analytics.rs:51-60`), eight fields, eight fields.
///
/// **Seven of the overview's nine plus `build`.** The two it does not take are
/// the day counts, and legacy supplies them itself as `None`/`None` (`:385-386`)
/// so that one normalization covers both endpoints; [`Self::into_overview`] is
/// that construction. The drill-down draws no chart, so the two defaults are
/// applied and then unused — except by [`universe_window_start`], which is the
/// one place they still matter here and says why.
#[derive(Clone, Debug, Default)]
pub struct BuildTestsQuery {
    /// Required, non-blank.
    pub product_id: String,
    /// Required, non-blank.
    pub version: String,
    /// Required. `"all"` or `"plan"`.
    pub scope: String,
    /// Required when `scope` is `plan`.
    pub plan_id: Option<String>,
    /// Absent, blank or whitespace all mean every branch.
    pub branch: Option<String>,
    /// Defaults to `"none"`; a *present but blank* value is a 400.
    pub group_by: Option<String>,
    /// Blank narrows nothing.
    pub group_value: Option<String>,
    /// Required, non-blank — and checked **before** every rule above
    /// (`analytics.rs:373-376` precedes the `normalize_overview_query` call at
    /// `:379`), so a request that is wrong in both ways is told about the build.
    pub build: String,
}

impl BuildTestsQuery {
    /// The seven shared parameters as an [`OverviewQuery`], with the two day
    /// counts absent.
    ///
    /// `api_build_tests` (`analytics.rs:379-389`) verbatim. `build` is not
    /// carried across: it is normalized separately and earlier, which is the
    /// order that decides which message a doubly-invalid request gets.
    fn into_overview(self) -> OverviewQuery {
        OverviewQuery {
            product_id: self.product_id,
            version: self.version,
            scope: self.scope,
            plan_id: self.plan_id,
            branch: self.branch,
            days_heatmap: None,
            days_trend: None,
            group_by: self.group_by,
            group_value: self.group_value,
        }
    }
}

/// One test, as `GET /qa/v1/analytics/plan/tests?plan_id=` renders it.
///
/// Legacy's `TestAnalytics` (`manager/src/models.rs:637-647`), read from
/// `api_plan_tests` (`manager/src/routes/analytics.rs:2434-2483`). See
/// [`PlanExecRow`]'s header for why the grain is `test_name` and why
/// [`Self::last_version`] is `qa_test_results.product_version` rather than the
/// overview's `app_build`.
///
/// # `last_run_id` where legacy has `last_run_name`
///
/// The same substitution [`crate::domain::analytics::ExecRow::run_id`]'s
/// header already made for the overview: no bulk run-name read exists in this
/// gear, so the id is carried and a caller who can resolve one does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanTestAnalytics {
    pub test_name: String,
    /// The most recent row's raw status — legacy's `last_status`, defaulted to
    /// `"UNKNOWN"` there (`:2473`) for an aggregate legacy's `ARRAY_AGG` could in
    /// principle return empty. Unreachable here: a group exists only because at
    /// least one row produced it.
    pub last_status: String,
    /// The most recent row's platform, unresolved. `None` for a run that named
    /// none.
    pub last_environment_id: Option<Uuid>,
    /// The most recent row's `product_version` — legacy's `last_version`
    /// (`r.app_version`). See [`PlanExecRow::version`].
    pub last_version: Option<String>,
    /// The most recent row's run. See this type's header.
    pub last_run_id: Uuid,
    /// The most recent row's JIRA reference.
    pub jira_key: Option<String>,
    /// Every row naming this test, inside the read window — legacy's
    /// unconditional `COUNT(*)`.
    pub total_runs: u64,
    /// Rows whose status is the **literal** string `PASSED` — legacy's
    /// `COUNT(*) FILTER (WHERE t.status = 'PASSED')` (`:2448`). See
    /// [`PlanExecRow::status`] for why this is narrower than
    /// `PASSED_STATUSES`.
    pub pass_count: u64,
    /// Rows whose status is the literal string `FAILED` (`:2449`). See
    /// [`Self::pass_count`].
    pub fail_count: u64,
}

/// [`AnalyticsService::plan_tests`]' whole answer: the rows and the platform
/// names they need. The same split [`AnalyticsOverview::platform_names`] makes,
/// for the same reason — the resolution is one cross-gear call over the
/// distinct ids of the whole payload, not a field on the item.
#[derive(Clone, Debug)]
pub struct PlanTests {
    /// One entry per test name, ordered by [`PlanTestAnalytics::test_name`].
    pub items: Vec<PlanTestAnalytics>,
    /// The display name of every platform id [`Self::items`] mentions, for the
    /// ids qa-environments resolved. See [`AnalyticsOverview::platform_names`]'
    /// header for the same shape and the same reason.
    pub platform_names: HashMap<Uuid, String>,
}

/// One build, as `GET /qa/v1/analytics/plan/builds?plan_id=` renders it.
///
/// Legacy's `BuildDistribution` (`manager/src/models.rs:651-657`), read from
/// `api_plan_builds` (`manager/src/routes/analytics.rs:2486-2527`). The
/// grouping key is `qa_test_results.product_version` — see [`PlanExecRow`]'s
/// header for why that is not [`crate::domain::analytics::ExecRow::build`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanBuildDistribution {
    /// `COALESCE(r.app_version, 'unknown')` (`analytics.rs:2493`) — legacy's own
    /// literal, reproduced verbatim. **Not**
    /// [`crate::domain::analytics::universe::UNKNOWN_BUILD`]: that constant's own
    /// header refuses to serve two different fields, and this is a different
    /// column under a coincidentally identical label.
    pub build: String,
    /// Every row of the group — legacy's unconditional `COUNT(*)`
    /// (`analytics.rs:2494`), which can exceed `passed + failed + skipped` when
    /// a row's status is none of the three (`ERROR`, `RUNNING`, …). See
    /// [`PlanExecRow::status`].
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}

/// One run's outcome for one test, inside [`PlanTestHistory::results`].
///
/// Legacy's `TestHistoryEntry` (`manager/src/models.rs:661-665`). `run_id`
/// where legacy has `run_name` — see [`PlanTestAnalytics`]'s header for the
/// same substitution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanTestHistoryEntry {
    /// `product_version`, **not** coalesced to `"unknown"` — legacy's own
    /// `TestHistoryRow::build` (`analytics.rs:2600`) is assigned unchanged at
    /// `:2560` and its wire type, `TestHistoryEntry::build`
    /// (`models.rs:662`), stays `Option` too, unlike
    /// [`PlanBuildDistribution::build`]. Two different null-handling rules over
    /// the same column, both legacy's own and both preserved verbatim.
    pub build: Option<String>,
    pub status: String,
    pub run_id: Uuid,
}

/// One test's history, as `GET /qa/v1/analytics/plan/test-history?plan_id=`
/// renders it.
///
/// Legacy's `TestHistory` (`manager/src/models.rs:669-672`), read from
/// `api_plan_test_history` (`manager/src/routes/analytics.rs:2530-2572`).
///
/// # The outer order is this port's, not legacy's
///
/// Legacy folds its rows into a `HashMap<String, Vec<TestHistoryEntry>>` and
/// then collects the map into the response `Vec` (`:2553-2569`) — so which
/// test comes first is Rust's `HashMap` iteration order, seeded per process
/// and never specified by legacy's own contract. That is not a behaviour worth
/// porting: it is an accident of the data structure legacy happened to fold
/// into, not a rule a client could depend on. This orders by [`Self::test_name`]
/// ascending instead — deterministic, and cheap given the rows already arrive
/// sorted by test name within [`build_plan_test_history`]'s fold. Each test's
/// own [`Self::results`] **is** legacy's order: newest first, exactly as
/// `ORDER BY t.test_name, r.finished_at DESC NULLS LAST` (`:2540`) produces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanTestHistory {
    pub test_name: String,
    /// Newest first. See this type's header.
    pub results: Vec<PlanTestHistoryEntry>,
}

/// The analytics reads, reduced to the two payloads the SPA draws.
///
/// Generic over the repository rather than boxed, for the reason
/// [`crate::domain::service::AppServices`] gives: [`ResultsRepository`]'s methods
/// are generic over their `DBRunner`, so the trait is not object-safe and the
/// parameter propagates to the composition root. Nothing here opens a
/// transaction — two independent `SELECT`s on a pooled connection need no
/// isolation guarantee, and the payload is already a snapshot of three different
/// systems.
pub struct AnalyticsService<R, C> {
    db: Arc<DbProvider>,
    results: R,
    /// The analytics universe, read from qa-catalog. **Unconditional**, unlike
    /// [`crate::domain::service::dashboard::DashboardService`]'s — see this
    /// module's header.
    catalog: Arc<dyn CatalogReader>,
    /// The platform id → display name resolution. Task 25a shipped the port for
    /// exactly this caller; [`AnalyticsOverview::platform_names`] says what it is
    /// used for and the build-tests drill-down does not need it.
    platforms: Arc<dyn EnvironmentReader>,
    /// Today, read once per request. See this module's header.
    clock: Arc<dyn Clock>,
    policy_enforcer: PolicyEnforcer,
    /// The collect job's exact per-file case counts — Task 29's other half of
    /// [`OverviewSummary::case_expected`], read alongside [`Self::results`]
    /// rather than boxed for the same reason that field is generic:
    /// [`CollectRepository`]'s methods are generic over their `DBRunner`.
    /// [`Self::overview`]' collect read is this field's only caller;
    /// [`Self::build_tests`] renders no summary and never reads it.
    collect: C,
    /// Legacy's `DEFAULT_COLLECT_BRANCH` (`manager/src/services/collect.rs:19`),
    /// substituted whenever a request names no branch — `analytics.rs:2682`,
    /// `branch.unwrap_or(DEFAULT_COLLECT_BRANCH)`. `QaInsightsConfig::
    /// default_collect_branch`'s own doc records why it is kept `"main"`:
    /// lining the default lookup up with what the hourly collect cycle writes.
    default_collect_branch: String,
}

impl<R, C> AnalyticsService<R, C>
where
    R: ResultsRepository,
    C: CollectRepository,
{
    /// # Eight parameters, and a params struct was the rejected alternative
    ///
    /// `clippy::too_many_arguments` fires at eight, added by Task 29's
    /// `collect` and `default_collect_branch`. [`ReconcileService::new`](
    /// crate::domain::service::reconcile::ReconcileService::new)'s doc made
    /// the same call at the same count and for the same reason: a
    /// `AnalyticsDeps` struct would have exactly one construction site
    /// ([`crate::domain::service::AppServices::new`]) and would unpack it
    /// again immediately, and all eight types here are distinct (`Arc<DbProvider>`,
    /// `R`, `Arc<dyn CatalogReader>`, `Arc<dyn EnvironmentReader>`, `Arc<dyn Clock>`,
    /// `PolicyEnforcer`, `C`, `String`) — so a transposition is a compile
    /// error already, which is what such a struct would otherwise buy.
    /// `expect` rather than `allow`, so a later shortening of the list goes red.
    #[expect(
        clippy::too_many_arguments,
        reason = "all eight parameter types are distinct, so a params struct would guard against \
                  nothing the compiler does not already catch"
    )]
    #[must_use]
    pub const fn new(
        db: Arc<DbProvider>,
        results: R,
        catalog: Arc<dyn CatalogReader>,
        platforms: Arc<dyn EnvironmentReader>,
        clock: Arc<dyn Clock>,
        policy_enforcer: PolicyEnforcer,
        collect: C,
        default_collect_branch: String,
    ) -> Self {
        Self {
            db,
            results,
            catalog,
            platforms,
            clock,
            policy_enforcer,
            collect,
            default_collect_branch,
        }
    }

    /// The eight-section overview — `GET /qa/v1/analytics/overview`.
    ///
    /// `build_overview` (`analytics.rs:728-803`) in legacy's own order. Read this
    /// module's header before changing the sequence: two of the eight sections
    /// are computed over the **unfiltered** universe and the rest are not.
    ///
    /// # The order of operations is load-bearing
    ///
    /// * The query is normalized **first**, exactly as legacy's handler does
    ///   (`:364`). Every rejection is a 400 that names one parameter and reveals
    ///   nothing about the deployment, so refusing before the PDP costs no
    ///   information and keeps the message a client sees independent of its
    ///   grants.
    /// * The PEP decision is compiled **before** qa-catalog is asked anything,
    ///   which is
    ///   [`DashboardService::stats`](crate::domain::service::dashboard::DashboardService::stats)'
    ///   rule and for its reason: otherwise a caller with no grant could still
    ///   learn which test files a product's plans contain.
    /// * The clock is read **once**; see this module's header.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] — a **400** naming the offending query
    /// parameter — for each of the six rules in
    /// [`crate::domain::analytics::query`], plus a `product_id` that is not a
    /// UUID (see [`product_uuid`], which is the one rejection legacy spells as a
    /// 404 instead).
    ///
    /// [`DomainError::Forbidden`] when the PDP denies, or compiles a scope it
    /// cannot express, or when **either** sibling refuses the subject: qa-catalog
    /// on the universe read, or qa-environments on the platform names. Neither is
    /// degraded — this module's header carries both decisions.
    ///
    /// [`DomainError::Internal`] when either sibling read fails, and
    /// [`DomainError::Database`] for a driver failure.
    ///
    /// **There is no not-found**, and legacy has one: `product_id` naming no
    /// product is a 404 there (`:742`) and an empty universe here, because
    /// [`CatalogReader::list_universe`] is deliberately not an existence oracle
    /// for a product id — the port's own `# Errors` states it. An unknown product
    /// and a product whose repositories are all unsynced are therefore the same
    /// answer: a 200 of zeros.
    pub async fn overview(
        &self,
        ctx: &SecurityContext,
        query: OverviewQuery,
    ) -> Result<AnalyticsOverview, DomainError> {
        let query = normalize_overview_query(&query)?;
        let scope = self.scope(ctx).await?;
        let today = self.clock.today();

        let (universe, all_rows) = self
            .load_universe_and_rows(ctx, &scope, &query, today)
            .await?;

        // Over the **unfiltered** universe: the vectors are a property of the
        // suite, not of a selection, and legacy builds the map inside
        // `load_universe_and_rows` (`:744-745`) where the group filter cannot
        // reach it. Computed here rather than there only because this gear's
        // fold is pure over the universe, where legacy's accumulates during the
        // plan walk.
        let quality_vectors = build_quality_vector_summary(&universe);
        // Over the **unfiltered** universe and **all** rows, and it must stay
        // above the filter call below — legacy's `:747` precedes its `:749`.
        let grouped = build_grouped_summaries(&universe, &all_rows);

        let (filtered_universe, rows_for_scope) = narrow_to_group(
            &universe,
            all_rows,
            query.group_by,
            query.group_value.as_deref(),
        );

        let latest_map = build_latest_map(&filtered_universe, &rows_for_scope);
        let stats_map = build_stats_map(&rows_for_scope);

        let cases = self
            .case_data(&scope, &filtered_universe, &latest_map)
            .await?;
        let collect_counts = self
            .collect_counts(&scope, &filtered_universe, query.branch.as_deref())
            .await?;

        let mut summary = summarize(&filtered_universe, &latest_map, &cases);
        // Legacy's handler, not `build_summary`: `:769-779` fills this in after
        // the summary is built, over the same `filtered_universe` `summarize`
        // just consumed. `aggregates.rs`' header and
        // `OverviewSummary::case_expected`'s own doc say why the fold is not
        // inside `summarize`.
        summary.case_expected = expected_cases(&filtered_universe, &collect_counts);
        let lists = build_lists(&filtered_universe, &latest_map, &stats_map, &cases);
        let heatmap = build_heatmap(
            &filtered_universe,
            &rows_for_scope,
            query.days_heatmap,
            today,
        );
        let trend = build_trend(&filtered_universe, &rows_for_scope, query.days_trend, today);
        let build_distribution =
            build_last_run_build_distribution(&filtered_universe, &rows_for_scope);
        // `days_trend`, not a window of its own — legacy's `:783` passes the
        // trend's clamp and `aggregates::flaky_cutoff` records why.
        let flaky = build_flaky(&filtered_universe, &rows_for_scope, query.days_trend, today);

        // One call, over the distinct ids of both sections that carry one. The
        // adapter performs no round trip for an empty slice, so a deployment
        // whose runs name no platform neither pays for the call nor can be
        // refused it.
        let platform_names = self
            .platforms
            .names(ctx, &environment_ids(&grouped, &lists))
            .await?;

        Ok(AnalyticsOverview {
            product_id: query.product_id,
            version: query.version,
            scope: query.scope,
            plan_id: query.plan_id,
            branch: query.branch,
            group_by: query.group_by,
            group_value: query.group_value,
            summary,
            lists,
            heatmap,
            trend,
            build_distribution,
            flaky,
            quality_vectors,
            grouped,
            platform_names,
        })
    }

    /// One build's tests — `GET /qa/v1/analytics/build-tests`.
    ///
    /// `api_build_tests` (`analytics.rs:369-455`). The same universe and the same
    /// rows as [`Self::overview`], narrowed the same way, reduced by a different
    /// fold: [`build_test_details`] over
    /// [`latest_per_test_snapshot`](crate::domain::analytics::aggregates::latest_per_test_snapshot),
    /// which keeps the status the latest map has already bucketized away.
    ///
    /// # Its window is the **default** one, and it can be narrower than the bar
    /// # it was opened from
    ///
    /// [`BuildTestsQuery::into_overview`] supplies `None`/`None` for the two day
    /// counts, which is legacy's own construction (`:385-386`) and is forced by
    /// D7: this endpoint takes legacy's parameter set verbatim and legacy's set
    /// has no `days_trend`. So [`universe_window_start`] always resolves to the
    /// **90-day** default here, while an overview served with `?days_trend=365`
    /// computed its build distribution over 365 days.
    ///
    /// **A bar there can therefore report tests this list cannot see.** Legacy
    /// has no window at all, so its drill-down always matched its bar; the
    /// mismatch is introduced by [`universe_window_start`], whose own doc carries
    /// why a bound exists. It is stated rather than papered over, and it is
    /// stated on the wire too — the endpoint description says the window is the
    /// default one regardless of what the overview was asked for. Widening it
    /// means either accepting a ninth parameter D7 excludes or making the bound
    /// configurable, and both are product decisions rather than this task's.
    ///
    /// Neither the grouped summaries nor the quality vectors are computed —
    /// legacy computes neither either (`:405-452` runs the filter, the snapshot
    /// fold and the sort
    /// and nothing else), so the unfiltered universe has no second consumer here
    /// and the platform names have none at all.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] for `build` **first** — legacy's `:373-376`
    /// runs before the shared rules — then each of
    /// [`Self::overview`]' rejections. The remaining variants are that method's,
    /// minus the qa-environments arm: this endpoint resolves no platform name.
    pub async fn build_tests(
        &self,
        ctx: &SecurityContext,
        query: BuildTestsQuery,
    ) -> Result<Vec<BuildTestDetail>, DomainError> {
        // Before `normalize_overview_query`, which is legacy's order and is
        // observable: a request with a blank `build` *and* a blank `product_id`
        // is told about the build.
        let build = normalize_build(query.build.as_str())?;
        let query = normalize_overview_query(&query.into_overview())?;
        let scope = self.scope(ctx).await?;
        let today = self.clock.today();

        let (universe, all_rows) = self
            .load_universe_and_rows(ctx, &scope, &query, today)
            .await?;
        let (filtered_universe, rows_for_scope) = narrow_to_group(
            &universe,
            all_rows,
            query.group_by,
            query.group_value.as_deref(),
        );

        Ok(build_test_details(
            &filtered_universe,
            &rows_for_scope,
            build.as_str(),
        ))
    }

    /// Aggregated test analytics for one plan —
    /// `GET /qa/v1/analytics/plan/tests?plan_id=`.
    ///
    /// `api_plan_tests` (`analytics.rs:2434-2483`). No universe, no product, no
    /// version: unlike [`Self::overview`] and [`Self::build_tests`], this reads
    /// `qa_test_results` directly by plan and does not touch qa-catalog at all
    /// — see [`PlanExecRow`]'s header and
    /// [`ResultsRepository::list_for_plan`]'s for why `plan_id` here is legacy's
    /// literal `WHERE r.plan_id = $1` reduced to this schema's `plan_path`
    /// column, matched across every repository the caller's scope admits.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] on `plan_id`, trimmed and blank — see
    /// [`Self::plan_rows`]'s doc, "`plan_id` is trimmed and refused blank"
    /// (Phase B fix wave, Finding 9). [`DomainError::Forbidden`] when the PDP
    /// denies. [`DomainError::Forbidden`] or [`DomainError::Internal`] when
    /// qa-environments refuses or fails resolving the platform names.
    /// [`DomainError::Database`] for a driver failure. **There is no
    /// not-found for a non-blank `plan_id`**: one naming nothing produces an
    /// empty `Vec`, the same shape [`Self::overview`]'s unknown-product answer
    /// takes and for the same reason — this schema has no plan registry to ask.
    pub async fn plan_tests(
        &self,
        ctx: &SecurityContext,
        plan_id: &str,
    ) -> Result<PlanTests, DomainError> {
        let rows = self.plan_rows(ctx, plan_id).await?;
        let items = build_plan_test_analytics(&rows);
        let platform_names = self
            .platforms
            .names(ctx, &plan_test_environment_ids(&items))
            .await?;
        Ok(PlanTests {
            items,
            platform_names,
        })
    }

    /// The build distribution for one plan —
    /// `GET /qa/v1/analytics/plan/builds?plan_id=`.
    ///
    /// `api_plan_builds` (`analytics.rs:2486-2527`). Same read as
    /// [`Self::plan_tests`]; see that method's header. No platform names: legacy's
    /// `BuildDistribution` carries none.
    ///
    /// # Errors
    ///
    /// As [`Self::plan_tests`], minus the qa-environments arm.
    pub async fn plan_builds(
        &self,
        ctx: &SecurityContext,
        plan_id: &str,
    ) -> Result<Vec<PlanBuildDistribution>, DomainError> {
        let rows = self.plan_rows(ctx, plan_id).await?;
        Ok(build_plan_build_distribution(&rows))
    }

    /// Per-test run history for one plan —
    /// `GET /qa/v1/analytics/plan/test-history?plan_id=`.
    ///
    /// `api_plan_test_history` (`analytics.rs:2530-2572`). Same read as
    /// [`Self::plan_tests`]; see that method's header, and see
    /// [`PlanTestHistory`]'s header for why the outer order is this port's own
    /// rather than legacy's unspecified `HashMap` order.
    ///
    /// # Errors
    ///
    /// As [`Self::plan_builds`].
    pub async fn plan_test_history(
        &self,
        ctx: &SecurityContext,
        plan_id: &str,
    ) -> Result<Vec<PlanTestHistory>, DomainError> {
        let rows = self.plan_rows(ctx, plan_id).await?;
        Ok(build_plan_test_history(&rows))
    }

    /// The read behind all three plan drill-downs — compile the scope, read the
    /// clock once, and read `qa_test_results` for `plan_id` inside the default
    /// window.
    ///
    /// # The window is this port's, and it is R21/R22's divergence again
    ///
    /// Legacy's three statements are unwindowed — `ResultsRepository::list_for_plan`'s
    /// doc gives the NFR argument for bounding a per-plan read on a table
    /// `cpt-cf-qa-nfr-scale` sizes at 5M rows. `universe_window_start(today, 7,
    /// 90)` is the same 90-day default [`BuildTestsQuery::into_overview`]
    /// inherits under ruling R22, chosen here for the identical reason: there is
    /// no `days_trend` on this endpoint's parameter set — it has *no* parameters
    /// beside `plan_id` — so the default is the only bound expressible without
    /// inventing a query parameter legacy does not have.
    ///
    /// # `plan_id` is trimmed and refused blank — Phase B fix wave, Finding 9
    ///
    /// Every other required string query parameter on this phase's endpoints
    /// is trimmed and answered with a 400 when blank: `product_id` and
    /// `version` in `domain::analytics::query::normalize_overview_query`,
    /// `build` in `domain::analytics::query::normalize_build`, the collect
    /// route's `branch`
    /// (`domain::service::collect::CollectService::record_count`), and saved
    /// views' `name`/`scope` (`domain::service::saved_views`). `plan_id` on
    /// these three drill-downs was the one exception: it reached
    /// [`crate::domain::repos::ResultsRepository::list_for_plan`] verbatim,
    /// so `?plan_id=` was a 200 with an empty array rather than a 400 —
    /// harmless in effect, since no row can carry `plan_path = ''`, but
    /// inconsistent with a rule this phase otherwise applies uniformly, and
    /// on the parameter most likely to arrive mistyped. [`normalize_plan_id`]
    /// closes it here, once, ahead of every caller below rather than in each
    /// of [`Self::plan_tests`], [`Self::plan_builds`] and
    /// [`Self::plan_test_history`] — they all funnel through this one read.
    async fn plan_rows(
        &self,
        ctx: &SecurityContext,
        plan_id: &str,
    ) -> Result<Vec<PlanExecRow>, DomainError> {
        let plan_id = normalize_plan_id(plan_id)?;
        let scope = self.scope(ctx).await?;
        let today = self.clock.today();
        let since = universe_window_start(today, 7, 90);
        let conn = self.db.conn()?;
        self.results
            .list_for_plan(&conn, &scope, plan_id, since)
            .await
    }

    /// The universe from qa-catalog and the executed rows resolved against it.
    ///
    /// `load_universe_and_rows` (`analytics.rs:805-1045`), minus the two halves
    /// this architecture moves elsewhere: the plan walk and the `TEST_META` parse
    /// are qa-catalog's (ADR-0005 confines git egress there), and the
    /// quality-vector fold is pure over the universe so its caller runs it.
    ///
    /// The rows come back **already resolved**: [`resolve_rows`] applies the
    /// alias map and drops everything outside the universe, which is legacy's
    /// `:1020-1026` and the step every later fold assumes has happened.
    async fn load_universe_and_rows(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        query: &NormalizedOverviewQuery,
        today: Date,
    ) -> Result<(Vec<UniverseTest>, Vec<ExecRow>), DomainError> {
        let product_id = product_uuid(query.product_id.as_str())?;
        // `query.branch` on both sides, and the two `None`s mean opposite things:
        // here it is each repository's **default** branch, and on
        // `UniverseFilter::branch` below it is *every* branch. Legacy runs the
        // same pair (`:816-823` chooses the plan checkout, `:967-970` guards the
        // row predicate), and `CatalogReader::list_universe`' doc carries the
        // asymmetry.
        let universe = self
            .catalog
            .list_universe(ctx, Some(product_id), query.branch.as_deref())
            .await?;
        let universe = narrow_to_plan(universe, query.scope, query.plan_id.as_deref());

        // **The read is skipped when the universe is empty, and that is not an
        // optimization.** `UniverseFilter::plans` empty means *no plan
        // restriction*, so building the filter from an empty universe would ask
        // for every row the version predicate admits — the widest read this type
        // can express — in order to drop all of them at `resolve_rows`. Every
        // `UniverseTest` carries a `(repo_id, plan_path)`, so an empty universe
        // is the only way to reach an empty plan set.
        if universe.is_empty() {
            return Ok((universe, Vec::new()));
        }

        let conn = self.db.conn()?;
        let raw = self
            .results
            .list_for_universe(&conn, scope, &universe_filter(&universe, query, today))
            .await?;

        let rows = resolve_rows(&universe, raw);
        Ok((universe, rows))
    }

    /// The per-case roll-up, and the read behind it.
    ///
    /// `attach_case_data` (`analytics.rs:1288-1395`) split in two: the read is
    /// here and the fold is [`build_case_data`], because legacy's is `async` and
    /// therefore patches [`summarize`]'s and [`build_lists`]' outputs in place
    /// (`:766`) where this one is an input to both.
    ///
    /// # The empty branch is `CaseData::default` and the empty *read* is not
    ///
    /// Legacy returns early when no universe file names a latest run
    /// (`:1304-1306`), leaving all six counters at zero. A read that **succeeds
    /// and returns nothing** does not take that path: `by_key` is simply empty,
    /// so every `PASSED`/`FAILED` file falls to the fallback arm at `:1371-1376`
    /// and contributes one synthetic case of its file-level status. So the two
    /// answers differ, and branching on the row count instead of on the run set
    /// silently loses every synthetic case.
    ///
    /// A `SKIPPED` file contributes **nothing** there, and the reason is one
    /// level down: that arm matches on the *bucketized* status, and
    /// `bucketize_status` (`:1940-1946`) returns only `PASSED`, `FAILED` or
    /// `NOT_RUN` — so legacy's `"SKIPPED"` case at `:1374` is unreachable.
    /// `the_per_case_counters_are_read_from_the_latest_runs_case_rows` pins the
    /// resulting count.
    /// [`CaseData`]'s header carries the same distinction.
    async fn case_data(
        &self,
        scope: &AccessScope,
        universe: &[UniverseTest],
        latest: &HashMap<String, crate::domain::analytics::universe::LatestInfo>,
    ) -> Result<CaseData, DomainError> {
        // Iterating the universe rather than the map's values is legacy's shape
        // (`:1297-1301`) and is the same set: the map is keyed on universe files.
        let run_ids: Vec<Uuid> = universe
            .iter()
            .filter_map(|test| latest.get(&test.test_file).and_then(|info| info.run_id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        if run_ids.is_empty() {
            return Ok(CaseData::default());
        }

        let conn = self.db.conn()?;
        // **Not laundered into an empty roll-up**, where legacy warns and
        // continues (`:1326-1329`): this is this gear's own database, and a
        // failed read here would report a suite whose runner produced no
        // per-case data at all — which is a real and indistinguishable state.
        let rows = self
            .results
            .case_rows_for_runs(&conn, scope, &run_ids)
            .await?;

        Ok(build_case_data(universe, latest, &rows))
    }

    /// The collect job's exact per-file case counts, for [`expected_cases`] to
    /// prefer over [`UniverseTest::static_case_count`].
    ///
    /// `load_collect_counts` (`analytics.rs:2672-2693`) split the same way
    /// [`Self::case_data`] splits `attach_case_data`: the read is here, the
    /// per-file precedence is [`expected_cases`], and legacy's own `:767-768`
    /// comment on why exact wins is that fold's doc, not this method's.
    ///
    /// # Not windowed
    ///
    /// Unlike [`Self::load_universe_and_rows`]' row read, this carries no
    /// `since` bound — `qa_test_case_collect` is a snapshot table with one row
    /// per `(repo_id, branch, test_file)`, so there is no history for ruling
    /// R21's window to bound, and legacy's own read is the same: the whole
    /// table for one branch, not a time slice of it.
    ///
    /// # Scoped to the universe's own repositories, not read unconditionally
    ///
    /// An empty universe reads nothing, for [`Self::load_universe_and_rows`]'
    /// reason: [`CollectRepository::list_counts_for`] takes the *distinct*
    /// `repo_id`s the caller already holds rather than "every repository",
    /// and an empty universe has none. Legacy pays the same asymmetric cost in
    /// the other direction — it loads one branch's rows across **every**
    /// repository and only some are ever looked up — which
    /// [`CollectRepository::list_counts_for`]'s own doc records as the
    /// deliberate trade this port makes instead.
    ///
    /// `branch` is the request's own — `None` falls back to
    /// [`Self::default_collect_branch`], exactly as legacy's
    /// `branch.unwrap_or(DEFAULT_COLLECT_BRANCH)` does (`:2682`).
    ///
    /// # A failed read is not laundered, and that is a divergence from legacy
    ///
    /// **Found in this fix round's Step 0 re-check; the first pass's "no
    /// spec/legacy disagreement found" was wrong.** `load_collect_counts`
    /// itself never fails the request — its own `:2687-2689` is
    /// `.fetch_all(db).await.unwrap_or_default()`, so a failed collect query
    /// renders as an empty map and every file falls back to its static count,
    /// with nothing failing and nothing logged. This method's `?` (its only
    /// caller, [`Self::overview`], propagates it) fails the whole eight-section
    /// payload instead.
    ///
    /// **Kept, not fixed**, for [`Self::case_data`]'s own reason applied to
    /// this table: laundering a driver failure into an empty map here would
    /// render **static counts**, which are indistinguishable from "no collect
    /// job has ever run" — exactly the ambiguity this fold exists to resolve.
    /// This is this gear's own database, not a cross-gear call, so a failed
    /// read is a real and reportable failure rather than a degraded answer.
    ///
    /// # Scoped by `qa.test_result`, not a resource type of its own
    ///
    /// This read runs under the same `AccessScope` [`Self::scope`] compiles
    /// for [`resources::TEST_RESULT`] — the same one [`Self::case_data`]'s
    /// `case_rows_for_runs` and [`Self::load_universe_and_rows`]'
    /// `list_for_universe` use for this same request — rather than declaring
    /// a fourth-table resource type for `qa_test_case_collect`. The table has
    /// no `OData` collection and no writer this crate's own request path
    /// reaches (Task 30's collect job is the only writer), so there is no
    /// request this scope could be wrong *for*: every caller of this method
    /// already holds the grant that governs the rest of the payload this
    /// method fills in one field of. `resources::TEST_RESULT`'s own doc
    /// records this as a third table under one resource type and why that is
    /// not the same reasoning that covers the first two.
    ///
    /// **The hazard this rides along with, not introduces.**
    /// `resources::TEST_RESULT` declares `pep_properties::RESOURCE_ID`, and
    /// `SecureEntityExt::scope_with`
    /// (`infra::storage::collect_sea_repo::OrmCollectRepository::list_counts_for`,
    /// `:142`) applies whatever id constraint a policy compiles to *this*
    /// entity's own `id` column — a `qa_test_case_collect` row's own surrogate
    /// key, which no policy author constraining "which test results" could
    /// have meant. A policy that returns an id-list constraint for
    /// `qa.test_result` would filter this read down to nothing, not fail it:
    /// [`expected_cases`] would then render every file's *static* count with
    /// no error and no signal that a collect job's numbers were silently
    /// dropped. `case_rows_for_runs` carries the identical exposure over
    /// `qa_test_case_results`'s own id column, for the identical reason, and
    /// is not this task's to fix either — recorded here so a resource-type
    /// review can weigh both at once.
    async fn collect_counts(
        &self,
        scope: &AccessScope,
        universe: &[UniverseTest],
        branch: Option<&str>,
    ) -> Result<Vec<CollectCount>, DomainError> {
        if universe.is_empty() {
            return Ok(Vec::new());
        }

        let repo_ids: Vec<Uuid> = universe
            .iter()
            .map(|test| test.repo_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let effective_branch = branch.unwrap_or(self.default_collect_branch.as_str());

        let conn = self.db.conn()?;
        self.collect
            .list_counts_for(&conn, scope, &repo_ids, effective_branch)
            .await
    }

    /// The caller's scope for an aggregate read, derived fresh on every call.
    ///
    /// The same `(resource, action)` pair
    /// [`DashboardService`](crate::domain::service::dashboard::DashboardService)
    /// and the two collections use, and for the reason
    /// [`actions::LIST`] gives: an aggregate is a reduction of exactly the rows
    /// those collections return, so a second action would let a policy give it a
    /// *different* row set.
    async fn scope(&self, ctx: &SecurityContext) -> Result<AccessScope, DomainError> {
        self.policy_enforcer
            .access_scope(ctx, &resources::TEST_RESULT, actions::LIST, None)
            .await
            .map_err(DomainError::from)
    }
}

/// `product_id` as the `Uuid` [`CatalogReader::list_universe`] takes.
///
/// # This is the one rejection legacy spells as a different status
///
/// Legacy resolves the string through its own product registry and answers
/// **404 "Product not found"** for anything it does not know (`analytics.rs:742`,
/// and `:401` in `api_build_tests`). This gear has no registry: qa-catalog
/// owns products, and its universe read answers an unknown id with an empty
/// `Vec` rather than a not-found, deliberately, so that the endpoint cannot be
/// used to learn which product ids exist
/// ([`CatalogReader::list_universe`]' `# Errors`).
///
/// So the 404 has no equivalent and the two legacy cases split:
///
/// * a **well-formed** id naming nothing is an empty universe and a 200 of zeros;
/// * a **malformed** one is a 400 here, because it cannot be turned into the
///   argument the port takes and answering 200 to it would tell a caller with a
///   typo that their product is empty.
///
/// The rejection is not in [`crate::domain::analytics::query`] with the other
/// six, and that is that module's own decision: keeping `product_id` a `String`
/// is what preserves the *"`product_id` is required"* 400 for a blank one, and
/// turning it into an argument is *"the job of the task that turns a request
/// into a read"*.
///
/// # Errors
///
/// [`DomainError::Validation`] on `product_id`.
fn product_uuid(product_id: &str) -> Result<Uuid, DomainError> {
    Uuid::parse_str(product_id).map_err(|_| DomainError::Validation {
        field: "product_id".to_owned(),
        message: "product_id must be a UUID".to_owned(),
    })
}

/// Trim `plan_id` and refuse it blank — [`AnalyticsService::plan_rows`]'s own
/// doc, "`plan_id` is trimmed and refused blank", carries the full argument
/// for why this exists: every other required string query parameter on this
/// phase's endpoints already gets this treatment, and `plan_id` on the three
/// plan drill-downs was the one exception, reaching
/// [`crate::domain::repos::ResultsRepository::list_for_plan`] verbatim.
///
/// # Errors
///
/// [`DomainError::Validation`] on `plan_id`, when trimmed it is empty.
fn normalize_plan_id(plan_id: &str) -> Result<&str, DomainError> {
    let plan_id = plan_id.trim();
    if plan_id.is_empty() {
        return Err(DomainError::Validation {
            field: "plan_id".to_owned(),
            message: "plan_id is required".to_owned(),
        });
    }
    Ok(plan_id)
}

/// The universe narrowed to one plan, when the scope asks for one.
///
/// # `plan_id` is matched against the plan's **path**, and this is the decision
///
/// Legacy compares its `plan_id` against `plan.id` during the plan walk
/// (`analytics.rs:845-847`) and binds the same string to `r.plan_id = $2`
/// (`:965`). Neither exists here: `qa_insights_sdk`' header records that a plan
/// has **no UUID** in this architecture — it is materialized on read from
/// qa-catalog, there is no plans table, and legacy's own `plan_id` is a lossy
/// path-derived slug (`compose_repo_plan_id`,
/// `manager/src/services/plans.rs:789-801`) rather than a key. A plan's identity
/// here is the `(repo_id, plan_path)` pair, and [`PlanRef`] is its spelling.
///
/// A query string carries one value, so the half that identifies the plan
/// *within the request's product* is the path — the repository half is already
/// determined by the universe the product resolved to. A path listed by two of a
/// product's repositories therefore selects **both**, which is wider than legacy
/// and is the only reading that does not require the caller to know a repository
/// id it was never given. [`universe_filter`] then derives the row predicate from
/// whatever this leaves, so the two halves cannot disagree about which plans are
/// in scope.
///
/// Under [`Scope::All`] this is the identity, including when `plan_id` is
/// present: legacy carries a `plan_id` through an `all` scope and never reads it
/// (`:2412`, and [`NormalizedOverviewQuery::plan_id`]'s own doc).
fn narrow_to_plan(
    universe: Vec<UniverseTest>,
    scope: Scope,
    plan_id: Option<&str>,
) -> Vec<UniverseTest> {
    // `plan_id` is guaranteed `Some` under `Scope::Plan` — that is
    // `normalize_overview_query`'s fifth rule — so the `else` is unreachable
    // rather than a silent widening. Written as a `let ... else` returning the
    // whole universe because the alternative is an `expect` on a guarantee made
    // in another module.
    let (Scope::Plan, Some(plan_id)) = (scope, plan_id) else {
        return universe;
    };

    universe
        .into_iter()
        .filter(|test| test.plan_path == plan_id)
        .collect()
}

/// The row predicate for a normalized query and the universe it resolved to.
///
/// # `product_key` has no column, so the disjunction loses its first half
///
/// Legacy's all-scope predicate is
/// `r.product_key = $2 OR (r.product_key IS NULL AND r.plan_id = ANY($3))`
/// (`analytics.rs:992-995`), and its plan scope is `r.plan_id = $2` (`:965`).
/// **This schema has neither column.** VHP-319 deleted the product-version
/// model; what a row carries instead is the `(repo_id, plan_path)` pair ingest
/// denormalizes from the run's target
/// (`crate::domain::service::ingest::plan_identity`).
///
/// So both scopes reduce to the **second** disjunct, fed exactly as legacy feeds
/// it — from the universe's own plan set (`:942-947`, `universe_plan_ids`, bound
/// at `:1006`). One
/// entry under [`Scope::Plan`] because [`narrow_to_plan`] has already reduced the
/// universe to that plan; many under [`Scope::All`].
///
/// What is lost is the first disjunct, which admitted rows of a *product* whose
/// plan is outside the universe. Nothing in this subsystem records a product on
/// a result row, so there is no way to express it and no field is invented for
/// it — [`UniverseFilter`]'s header forbids extending the struct without a call
/// site, and this is the call site that decided it needs none. The practical
/// consequence is that a run whose target names no plan — a custom plan or a
/// collect run, both of which ingest stores with a `NULL` `plan_path`
/// deliberately — contributes to no analytics scope, which is what
/// `plan_identity`'s own doc says the `NULL` is *for*.
///
/// # The three predicates that carry over unchanged
///
/// `product_version` is legacy's `r.app_version = $1`, always bound (`:964`,
/// `:991`); `branch` is its `COALESCE(source_ref, test_version) = $N` guarded on
/// `NULL` (`:967-970`); `finished_only` is `r.phase IN ('Succeeded','Failed')`
/// widened to `run_finished_at IS NOT NULL`, which
/// [`UniverseFilter::finished_only`] records as *wider* than legacy because this
/// schema has no phase column.
fn universe_filter(
    universe: &[UniverseTest],
    query: &NormalizedOverviewQuery,
    today: Date,
) -> UniverseFilter {
    let plans: Vec<PlanRef> = universe
        .iter()
        .map(|test| PlanRef {
            repo_id: test.repo_id,
            plan_path: test.plan_path.clone(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    UniverseFilter {
        product_version: Some(query.version.clone()),
        since: Some(universe_window_start(
            today,
            query.days_heatmap,
            query.days_trend,
        )),
        plans,
        branch: query.branch.clone(),
        finished_only: true,
    }
}

/// The instant the analytics read opens: midnight UTC on the oldest day any
/// section of this request can render.
///
/// # Legacy has no bound at all, and this one is a deliberate divergence
///
/// Its two `ExecRowRaw` queries are bounded only by `app_version` (`:964`,
/// `:991`) and every window is applied in memory over the already-fetched `Vec`.
/// That is affordable on a single-tenant installation and is not affordable here:
/// `cpt-cf-qa-nfr-scale` targets 5M rows on `qa_test_results` and
/// [`ResultsRepository::list_for_universe`] returns them unaggregated.
/// [`UniverseFilter::since`] carries the full argument, including why
/// [`UniverseFilter::finished_only`] is what lets the bound reach an index.
///
/// # The widest of the three windows, not the one being drawn
///
/// Three sections window: the heatmap (`[1, 30]` days), the trend (`[7, 365]`)
/// and the flaky detector, whose cutoff *is* the trend's first day
/// ([`flaky_cutoff`](crate::domain::analytics::aggregates::flaky_cutoff)). So the
/// bound is the earliest of the two axes, and it is derived through
/// [`recent_days`] rather than by subtracting, so that it cannot disagree with
/// the axis the folds actually build. Neither clamp is assumed to have run:
/// [`heatmap_days`] and [`trend_days`] are applied again here, which is the same
/// "clamped twice" shape legacy has and `query`'s header describes.
///
/// **`days_heatmap` can be the wider of the two** — `?days_trend=7&days_heatmap=30`
/// — so taking `days_trend` alone would silently blank the last three weeks of
/// the heatmap.
///
/// # What the bound costs, stated rather than discovered
///
/// The sections that do **not** window — the summary, the lists, the per-file
/// tallies, the build distribution and the group breakdowns — see only the rows
/// inside it. So `pass_count`, `fail_count` and `total_runs` count the window's
/// executions rather than all of history, and a test whose only run predates the
/// window reads as `NOT_RUN` rather than as its last real status. Widening the
/// window widens all of them together, which is the knob a deployment has.
///
/// Midnight UTC of that day rather than the instant `days` ago:
/// [`ExecRow::day`](crate::domain::analytics::ExecRow::day) is a calendar day and
/// the charts bucket on it, so a bound at "now minus N days" would drop the
/// earlier hours of the oldest column.
fn universe_window_start(today: Date, days_heatmap: usize, days_trend: usize) -> OffsetDateTime {
    let days = heatmap_days(days_heatmap).max(trend_days(days_trend));
    // Both clamps have a positive floor, so the window is never empty; `today`
    // as the fallback is the narrowest safe answer rather than a claim that the
    // branch is unreachable, and matches how `flaky_cutoff` spells the same step.
    let oldest = recent_days(today, days).into_iter().next().unwrap_or(today);
    oldest.midnight().assume_utc()
}

/// The group filter applied once, and the two collections it produces.
///
/// `build_overview` (`analytics.rs:749-759`) and `api_build_tests` (`:405-417`),
/// which are the same six lines twice. The universe is narrowed by
/// [`apply_universe_group_filter`] and the rows are then narrowed to the files
/// that survived — **not** re-filtered by the group, which the rows could not
/// express anyway.
///
/// Takes the rows by value because it drops some and keeps the rest; the caller
/// has no use for the wider set afterwards, which is exactly the point of the
/// table in this module's header: everything computed over *all* rows is
/// computed before this runs.
fn narrow_to_group(
    universe: &[UniverseTest],
    rows: Vec<ExecRow>,
    group_by: GroupBy,
    group_value: Option<&str>,
) -> (Vec<UniverseTest>, Vec<ExecRow>) {
    let filtered = apply_universe_group_filter(universe, group_by, group_value);
    let allowed: HashSet<&str> = filtered
        .iter()
        .map(|test| test.test_file.as_str())
        .collect();
    let rows = rows
        .into_iter()
        .filter(|row| allowed.contains(row.test_file.as_str()))
        .collect();

    (filtered, rows)
}

/// Every distinct platform id the payload will render, sorted.
///
/// Two sections carry one: the platform breakdown, whose entries *are*
/// platforms, and the three lists' `last_environment_id`. Collected from the
/// **outputs** rather than from the rows, so the resolution asks about exactly
/// what is rendered — a row whose file the group filter dropped names a platform
/// nothing will draw.
///
/// A [`BTreeSet`] rather than a [`HashSet`]: the answer is a map and does not
/// care, but the *request* is observable — `test_support::FakePlatforms` records
/// every batch precisely because "resolved once, over the distinct ids" is a
/// property with no other witness — and an assertion over a hasher's order is a
/// flaky test.
fn environment_ids(grouped: &GroupedSummaries, lists: &AnalyticsLists) -> Vec<Uuid> {
    let mut ids: BTreeSet<Uuid> = grouped
        .platform
        .iter()
        .map(|entry| entry.environment_id)
        .collect();

    for item in lists
        .passed
        .iter()
        .chain(lists.failed.iter())
        .chain(lists.not_run.iter())
    {
        if let Some(id) = item.last_environment_id {
            ids.insert(id);
        }
    }

    ids.into_iter().collect()
}

/// One test-name group of [`PlanExecRow`]s, in the order [`Self::rows`]'
/// caller supplied — newest first, per
/// [`ResultsRepository::list_for_plan`]'s contract.
struct PlanTestGroup<'a> {
    rows: Vec<&'a PlanExecRow>,
}

/// Groups `rows` by [`PlanExecRow::test_name`], preserving the incoming
/// (newest-first) order within each group. A [`BTreeMap`] rather than a
/// [`HashMap`]: every one of the three folds below needs the groups in
/// `test_name` order — [`build_plan_test_analytics`] and
/// [`build_plan_test_history`] directly, [`build_plan_build_distribution`]
/// not at all but harmlessly — so grouping once, ordered, serves all three
/// without a second sort at each call site.
fn group_by_test_name(rows: &[PlanExecRow]) -> std::collections::BTreeMap<&str, PlanTestGroup<'_>> {
    let mut groups: std::collections::BTreeMap<&str, PlanTestGroup<'_>> =
        std::collections::BTreeMap::new();
    for row in rows {
        groups
            .entry(row.test_name.as_str())
            .or_insert_with(|| PlanTestGroup { rows: Vec::new() })
            .rows
            .push(row);
    }
    groups
}

/// `api_plan_tests`' fold (`analytics.rs:2467-2480`): one row per test name,
/// the most recent row's fields plus the literal `PASSED`/`FAILED` counts over
/// every row of the group. `rows` must already be newest-first —
/// [`ResultsRepository::list_for_plan`]'s ordering, consumed the same
/// authoritative-order way [`crate::domain::analytics::universe::build_latest_map`]
/// consumes [`ExecRow`]'s.
fn build_plan_test_analytics(rows: &[PlanExecRow]) -> Vec<PlanTestAnalytics> {
    group_by_test_name(rows)
        .into_iter()
        .map(|(test_name, group)| {
            // `group.rows` is never empty: `group_by_test_name` only creates an
            // entry when a row is pushed into it.
            let latest = group.rows[0];
            let total_runs = group.rows.len() as u64;
            let pass_count = group
                .rows
                .iter()
                .filter(|row| row.status == "PASSED")
                .count() as u64;
            let fail_count = group
                .rows
                .iter()
                .filter(|row| row.status == "FAILED")
                .count() as u64;
            PlanTestAnalytics {
                test_name: test_name.to_owned(),
                last_status: latest.status.clone(),
                last_environment_id: latest.environment_id,
                last_version: latest.version.clone(),
                last_run_id: latest.run_id,
                jira_key: latest.jira_key.clone(),
                total_runs,
                pass_count,
                fail_count,
            }
        })
        .collect()
}

/// `api_plan_builds`' fold — the `SELECT` at `analytics.rs:2492-2502`, its map
/// at `:2515-2523`: grouped by [`PlanExecRow::version`] verbatim, `None`
/// rendered as the literal `"unknown"` and sorted last — legacy's
/// `COALESCE(r.app_version, 'unknown')` grouped and `ORDER BY r.app_version` on
/// the **raw** column, which Postgres's default `ASC` order places after every
/// non-`NULL` value.
fn build_plan_build_distribution(rows: &[PlanExecRow]) -> Vec<PlanBuildDistribution> {
    let mut groups: std::collections::BTreeMap<Option<&str>, PlanBuildDistribution> =
        std::collections::BTreeMap::new();
    for row in rows {
        let entry = groups
            .entry(row.version.as_deref())
            .or_insert_with(|| PlanBuildDistribution {
                build: row.version.clone().unwrap_or_else(|| "unknown".to_owned()),
                total: 0,
                passed: 0,
                failed: 0,
                skipped: 0,
            });
        entry.total += 1;
        match row.status.as_str() {
            "PASSED" => entry.passed += 1,
            "FAILED" => entry.failed += 1,
            "SKIPPED" => entry.skipped += 1,
            _ => {}
        }
    }

    // `Option`'s derived `Ord` ranks `None` **first**, so a `BTreeMap<Option<&str>,
    // _>` iterates the `None` group before every `Some` — the opposite of
    // Postgres's default `ASC` order, which is legacy's `NULLS LAST`. The
    // `None` group is therefore moved to the end explicitly below, rather than
    // relied on to already be there.
    let mut unknown = None;
    let mut named = Vec::with_capacity(groups.len());
    for (version, dist) in groups {
        if version.is_none() {
            unknown = Some(dist);
        } else {
            named.push(dist);
        }
    }
    if let Some(dist) = unknown {
        named.push(dist);
    }
    named
}

/// `api_plan_test_history`'s fold (`analytics.rs:2553-2569`): one entry per
/// row, grouped by test name. See [`PlanTestHistory`]'s header for the outer
/// order and [`PlanTestHistoryEntry::build`] for why this does not coalesce to
/// `"unknown"` the way [`build_plan_build_distribution`] does.
fn build_plan_test_history(rows: &[PlanExecRow]) -> Vec<PlanTestHistory> {
    group_by_test_name(rows)
        .into_iter()
        .map(|(test_name, group)| PlanTestHistory {
            test_name: test_name.to_owned(),
            results: group
                .rows
                .into_iter()
                .map(|row| PlanTestHistoryEntry {
                    build: row.version.clone(),
                    status: row.status.clone(),
                    run_id: row.run_id,
                })
                .collect(),
        })
        .collect()
}

/// Every distinct platform id [`AnalyticsService::plan_tests`] will render,
/// sorted — [`environment_ids`]'s argument, over one field instead of two.
fn plan_test_environment_ids(items: &[PlanTestAnalytics]) -> Vec<Uuid> {
    items
        .iter()
        .filter_map(|item| item.last_environment_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
#[path = "analytics_tests.rs"]
mod analytics_tests;
