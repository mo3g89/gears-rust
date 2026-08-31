//! The analytics routes — `GET /qa/v1/analytics/overview`,
//! `GET /qa/v1/analytics/build-tests` (Task 25b) and
//! `GET /qa/v1/analytics/export` (Task 26).
//!
//! # Both, and in one commit, which is controller ruling R10
//!
//! Task 24 shipped the build-tests folds
//! (`domain::analytics::aggregates::build_last_run_build_distribution` and
//! `build_test_details`) and could ship no route for them: `api_build_tests` is
//! not a thin endpoint — it calls `normalize_overview_query`,
//! `load_universe_and_rows` and `apply_universe_group_filter` before it reaches
//! the fold, and every one of those is Task 25's. So the drill-down's REST tier
//! lands here beside the overview's, over the same service and the same
//! `AccessScope`.
//!
//! # No `OData`, per D7
//!
//! `api::rest::mod`' header states it: `OData` applies to the two flat result
//! collections and to nothing else in this gear. Both operations here answer
//! with an aggregate — there is no row set for a `$filter` to narrow and no page
//! for a cursor to walk — so the nine (and eight) parameters are plain query
//! parameters, exactly as legacy's are.
//!
//! # Only the two day counts are declared `integer`; the other seven are `string`
//!
//! `days_heatmap` and `days_trend` are `Option<u32>` on the DTO and are declared
//! as `integer` here for that reason. The other seven are declared `string`
//! **including `product_id`**, which is a UUID once
//! `domain::service::analytics::product_uuid` parses it: declaring it as a UUID
//! would move the rejection into the extractor, which answers with its own
//! message and its own shape — the argument `domain::analytics::query`'s header
//! makes about why the field is not typed, and the same reason the route does not
//! advertise a format it does not enforce at that layer.
//!
//! # The descriptions carry what the schema cannot say
//!
//! Four things a caller would otherwise discover by being wrong about them, and
//! all four are behaviour rather than shape: an unknown `product_id` is an empty
//! overview rather than a 404, `case_expected` mixes an exact per-file collect
//! count with a static fallback rather than needing a collect job to be
//! non-zero (Task 29), the group chart
//! and the Quality Vector totals are **not** narrowed by `group_by`, and the read
//! is windowed to the wider of the two day counts so the counters outside the
//! charts move with them. The same duplication `routes::dashboard`' header
//! accepts, and for the same reason: `OperationBuilder::description` takes a
//! `&str` and cannot interpolate anything.

use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{AuthState, HandlerSlot, LicenseState, OperationBuilder};

use super::{API_TAG, License};
use crate::api::rest::{dto, handlers};

pub(super) fn register_analytics_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = register_overview(router, openapi);
    let router = register_build_tests(router, openapi);
    let router = register_export(router, openapi);
    let router = register_plan_tests(router, openapi);
    let router = register_plan_builds(router, openapi);
    register_plan_test_history(router, openapi)
}

/// The nine query parameters `register_overview` and `register_export` share,
/// in this exact order — `product_id` through `group_value`, including the two
/// day counts in the middle.
///
/// Task 26's fix round found these character-for-character identical between
/// the two callers: 29 `query_param_typed` calls existed across the module's
/// three registration functions, and the shared nine could drift between
/// `register_overview` and `register_export` independently with nothing to
/// notice. Factored here so there is exactly one copy to update.
///
/// **`register_build_tests` is deliberately not folded in.** Seven of its
/// eight parameters are the same *text* as seven of these nine, but not the
/// same *shape*: it has no day counts, so its own block is a contiguous
/// `product_id..group_value` with nothing between `branch` and `group_by`,
/// where this helper's callers need the two day-count parameters spliced into
/// exactly that gap. Splitting this helper into a `branch`-then-`group_by`
/// pair plus a middle day-count block, purely so `register_build_tests` could
/// reuse the two outer pieces, would trade nine duplicated lines for a shape
/// with no second use — the "false common shape" this fix round's brief
/// warned against forcing.
///
/// Verified to leave the emitted `OpenAPI` document unchanged: this crate's
/// `every_operation_registers_and_the_document_builds` and
/// `the_openapi_document_lists_every_registered_path` both still pass, and
/// the fix report for this round records a direct diff of
/// `/qa/v1/analytics/overview`'s and `/qa/v1/analytics/export`'s serialized
/// parameter arrays from before this change against after.
fn overview_query_params<H, R, S, A, L>(
    builder: OperationBuilder<H, R, S, A, L>,
) -> OperationBuilder<H, R, S, A, L>
where
    H: HandlerSlot<S>,
    A: AuthState,
    L: LicenseState,
{
    builder
        .query_param_typed(
            "product_id",
            true,
            "The product whose plans define the universe. Required and non-blank.",
            "string",
        )
        .query_param_typed(
            "version",
            true,
            "The product version the executions were run against. Required and non-blank.",
            "string",
        )
        .query_param_typed(
            "scope",
            true,
            "all or plan, case-insensitively. Required.",
            "string",
        )
        .query_param_typed(
            "plan_id",
            false,
            "The plan's path within its repository. Required when scope=plan, ignored \
             otherwise.",
            "string",
        )
        .query_param_typed(
            "branch",
            false,
            "The branch whose plans define the universe. Absent means each repository's own \
             default branch, and leaves every branch's executions in scope.",
            "string",
        )
        .query_param_typed(
            "days_heatmap",
            false,
            "Columns on the heatmap. Defaults to 7, silently clamped to 1-30.",
            "integer",
        )
        .query_param_typed(
            "days_trend",
            false,
            "Points on the trend, and the flaky window. Defaults to 90, silently clamped to \
             7-365.",
            "integer",
        )
        .query_param_typed(
            "group_by",
            false,
            "none, component, tag or platform. Defaults to none when absent; a present but \
             empty value is rejected.",
            "string",
        )
        .query_param_typed(
            "group_value",
            false,
            "The component or tag to narrow to, matched case-insensitively. Blank narrows \
             nothing.",
            "string",
        )
}

/// `GET /qa/v1/analytics/overview`.
///
/// A function per operation, where `routes::dashboard` registers both of its own
/// in one: each of these carries nine or eight `query_param_typed` calls and a
/// description a caller actually reads, and the pair does not fit in one body
/// that `clippy::too_many_lines` will accept. Splitting on the operation is the
/// only seam there is.
fn register_overview(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/analytics/overview — the eight computed sections.
    //
    // `.authenticated()` is not the authorization: `AnalyticsService` compiles a
    // `PolicyEnforcer` decision on `qa.test_result` / `list` — the same pair the
    // dashboard and the two collections use — before it reads anything, so 403 is
    // a documented outcome for an authenticated caller. It also covers a
    // qa-catalog or qa-environments refusal, neither of which is degraded into a
    // partial payload.
    let builder = OperationBuilder::get("/qa/v1/analytics/overview")
        .operation_id("qa_insights.analytics_overview")
        .summary("Analytics overview")
        .description(
            "Eight computed sections over one product's test universe, in one payload: a \
             pass/fail/not-run summary, the same universe as three sorted lists, a per-test \
             day heatmap, a daily trend, the distribution of each test's latest build, the \
             flaky tests, the Quality Vector breakdown and three group breakdowns. The \
             universe is every test file qa-catalog resolves from the product's plans, and \
             it is the denominator of every number here - a test with no execution row is \
             not_run rather than absent, so total does not move with the data. product_id, \
             version and scope are required; scope=plan additionally requires plan_id, which \
             is the plan's path within its repository. An unknown product_id is an empty \
             overview of zeros rather than a 404, because this gear reads the universe from \
             qa-catalog and that read is not an existence oracle for a product. branch \
             selects the branch whose plans define the universe AND narrows the executions \
             to runs on that branch. Absent is the asymmetric case, and it is deliberate: the \
             universe then comes from each repository's own default branch while every \
             branch's executions stay in scope. days_heatmap defaults to 7 and is clamped \
             to 1-30; days_trend defaults to 90 and is clamped to 7-365, and the flaky \
             window is days_trend rather than a third setting. Both clamps are silent. The \
             executions read is bounded to the wider of the two windows, so pass_count, \
             fail_count, total_runs and the build distribution count that window rather than \
             all of history, and widening days_trend widens them. group_by plus group_value \
             narrow the summary, the lists, both charts, the build distribution and the \
             flaky list to one component or tag; they deliberately do NOT narrow the three \
             group breakdowns or the Quality Vector totals, which stay over the whole \
             universe so the chart remains a comparison. group_by=platform narrows nothing \
             at all, and a blank group_value narrows nothing, both of which are the \
             behaviour of the system being replaced. summary.case_expected sums, per test \
             file, the collect job's exact case count where the collect job has reported one \
             for that file on this request's branch (branch, or the configured default \
             collect branch - main unless overridden - when absent), and a static count parsed from \
             the test source otherwise; the static count does not expand \
             @pytest.mark.parametrize, so it is a lower bound wherever the exact count is \
             not available, and a deployment that has never run a collect job still renders \
             a non-zero total from the static counts alone. The platform breakdown carries a \
             platform_id and a platform name resolved from qa-environments; the name is null \
             for a platform the caller cannot see, and those bars sort last. Run identities \
             are ids rather than names - there is no bulk run-name lookup to make one \
             without a request per row. Requires the qa.test_result/list grant, the same one \
             the dashboard and the test-result collections need, plus whatever qa-catalog \
             requires to list a universe.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([]);
    overview_query_params(builder)
        .handler(handlers::analytics_overview)
        .json_response_with_schema::<dto::AnalyticsOverviewDto>(
            openapi,
            StatusCode::OK,
            "The eight computed sections, with the normalized query echoed back",
        )
        // 400 is the caller's and there are seven ways to earn it: the five rules
        // of the shared query normalizer, a product_id that is not a UUID, and a
        // days_* that is not an integer (which fails in the extractor). A
        // days_* that is merely out of range is clamped, not refused.
        //
        // There is no 404: an unknown product is an empty overview, for the
        // reason the description gives.
        .error_400(openapi)
        .error_401(openapi)
        // 403 covers this gear's PDP, qa-catalog on the universe read and
        // qa-environments on the platform names. None of the three is laundered
        // into a partial payload; `domain::service::analytics`' header carries
        // that decision.
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi)
}

/// `GET /qa/v1/analytics/build-tests`. See [`register_overview`] for why the two
/// are separate functions.
fn register_build_tests(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/analytics/build-tests — the overview's build bar, drilled into.
    //
    // Same grant, same service, same group filter: it is a second reduction of
    // the overview's own inputs rather than a new read path.
    OperationBuilder::get("/qa/v1/analytics/build-tests")
        .operation_id("qa_insights.analytics_build_tests")
        .summary("Tests of one build")
        .description(
            "The tests whose latest run executed against one build, with the status that run \
             reported. The drill-down behind a bar of the overview's build distribution, \
             sharing that endpoint's universe and its group filter. It does NOT share the \
             overview's executions window: it takes neither day count, so its read is always \
             bounded to the default 90 days - an overview asked for days_trend=365 can \
             therefore draw a bar carrying tests this list does not return. build is required \
             and \
             non-blank, and is refused before every other parameter is looked at. It is \
             matched case-insensitively, and the literal build unknown selects the tests \
             whose latest run named no build at all. A build nothing ran against is an empty \
             array rather than a 404. Ordering is by status - failures first, then passes, \
             then everything else - and then by test name. The status here is the runner's \
             own, so a test the overview lists as not_run because it was skipped appears \
             with SKIPPED. Takes seven of the overview's nine parameters and neither day \
             count, because it draws no chart. Requires the qa.test_result/list grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed(
            "product_id",
            true,
            "The product whose plans define the universe. Required and non-blank.",
            "string",
        )
        .query_param_typed(
            "version",
            true,
            "The product version the executions were run against. Required and non-blank.",
            "string",
        )
        .query_param_typed(
            "scope",
            true,
            "all or plan, case-insensitively. Required.",
            "string",
        )
        .query_param_typed(
            "plan_id",
            false,
            "The plan's path within its repository. Required when scope=plan, ignored \
             otherwise.",
            "string",
        )
        .query_param_typed(
            "branch",
            false,
            "The branch whose plans define the universe. Absent means each repository's own \
             default branch, and leaves every branch's executions in scope.",
            "string",
        )
        .query_param_typed(
            "group_by",
            false,
            "none, component, tag or platform. Defaults to none when absent; a present but \
             empty value is rejected.",
            "string",
        )
        .query_param_typed(
            "group_value",
            false,
            "The component or tag to narrow to, matched case-insensitively. Blank narrows \
             nothing.",
            "string",
        )
        .query_param_typed(
            "build",
            true,
            "The build to list. Required and non-blank, matched case-insensitively; unknown \
             selects the tests whose latest run named no build.",
            "string",
        )
        .handler(handlers::analytics_build_tests)
        .json_array_response_with_schema::<dto::BuildTestDetailDto>(
            openapi,
            StatusCode::OK,
            "One entry per test whose latest run executed against the build",
        )
        // 400 for the blank build and for each of the shared query rules. No 404:
        // an unknown build, like an unknown product, is an empty answer.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi)
}

/// `GET /qa/v1/analytics/export`. See [`register_overview`] for why this is a
/// third function rather than a third block in one of the two above.
///
/// # One `200` response, though the body is two different shapes
///
/// `?format=csv` and the JSON default answer with different content types and
/// different bodies — a CSV string, or one of six JSON shapes depending on
/// `section`. `OperationBuilder`'s response registry keys on status code, so a
/// second `200` entry for `text/csv` would silently replace this one rather
/// than add to it (`ResponsesBuilder::response` is a map insert, not a merge);
/// registering only the JSON shape and stating the CSV alternative in the
/// description is the same trade-off `routes::dashboard` and this file's own
/// `register_overview` already make for behaviour a schema cannot express.
fn register_export(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/analytics/export — one section of the overview, or all of
    // them, as JSON or CSV.
    //
    // Same grant, same service, same universe as the overview: this is a
    // rendering of `AnalyticsService::overview`'s own output, not a second
    // read path.
    let builder = OperationBuilder::get("/qa/v1/analytics/export")
        .operation_id("qa_insights.analytics_export")
        .summary("Export the analytics overview")
        .description(
            "The overview's eight computed sections, reduced to one (or all of them) and \
             rendered as JSON (default) or CSV. Shares the overview's nine parameters, its \
             universe, its group filter and its executions window - see \
             GET /qa/v1/analytics/overview for what each one does. format selects the body: \
             the literal csv, case-insensitively, selects CSV; anything else, including an \
             absent value, answers as JSON. There is no rejection for an unrecognized format. \
             section selects the slice: summary, lists, heatmap, trend, flaky or all \
             (default). On the JSON branch an unrecognized section is a 400 naming the six \
             accepted spellings. On the CSV branch it is NOT rejected - it matches none of \
             the five renderable blocks and the response is a 200 with an empty body, which \
             is the system being replaced's own behaviour and is reproduced rather than \
             corrected. build_distribution, quality_vectors and grouped have no section name \
             of their own in either branch - all three are reachable only through \
             section=all, exactly as in the system being replaced. The CSV summary block \
             carries only total, passed, failed, not_run and their three percentages; the \
             six per-case counters and case_expected (see GET /qa/v1/analytics/overview for \
             what it computes) are on the JSON summary section and never on the \
             CSV one. CSV field quoting matches the system being \
             replaced exactly, including what it does NOT do: a bare carriage return is not a \
             quoting trigger and a value starting with =, +, - or @ is not escaped - this is \
             not a CSV-injection guard. The response carries a content-disposition header \
             naming analytics-export.csv or analytics-export.json. Requires the \
             qa.test_result/list grant, the same one the overview requires.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([]);
    overview_query_params(builder)
        .query_param_typed(
            "format",
            false,
            "json (default) or csv, case-insensitively. Anything else answers as json; there \
             is no rejection for an unrecognized value.",
            "string",
        )
        .query_param_typed(
            "section",
            false,
            "summary, lists, heatmap, trend, flaky or all (default). Rejected with 400 if \
             unrecognized and format is json; rendered as an empty CSV body if unrecognized \
             and format is csv.",
            "string",
        )
        .handler(handlers::analytics_export)
        .json_response(
            StatusCode::OK,
            "The requested section (or the whole payload for section=all) as pretty-printed \
             JSON, unless format=csv, in which case the body is CSV text with the same \
             content under a text/csv content type",
        )
        // 400 for each of the shared query rules plus, on the JSON branch only,
        // an unrecognized section. No 400 for an unrecognized format, and none
        // for an unrecognized section on the CSV branch - both render rather
        // than refuse.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi)
}

/// `GET /qa/v1/analytics/plan/tests?plan_id=`. Task 27, and the fix round on
/// it: `plan_id` was originally a path segment (`/plan/{plan_id}/tests`), and
/// a real `plan_path` routinely contains `/` (this crate's own fixtures,
/// `plans/nightly/plan.yaml`) — one path segment cannot carry that unencoded,
/// so the route 404'd for any real plan. Moved to a query parameter instead of
/// documenting an encoding requirement, because `?plan_id=` is what this gear
/// already ships: `AnalyticsListItemDto::plan_path`'s doc names it as the
/// value `?scope=plan&plan_id=` on the overview takes, so the path segment was
/// the outlier, not the query form. `dto::tests::a_plan_query_carries_a_slash_bearing_plan_id_through_the_same_decoder_the_route_uses`
/// pins the decode.
///
/// `api_plan_tests` (`manager/src/routes/analytics.rs:2434-2483`). Otherwise no
/// query parameters — see [`register_analytics_routes`]' three plan
/// registrations for why they share no helper with
/// [`overview_query_params`]: none of legacy's three plan endpoints take a
/// product, a version or a scope.
fn register_plan_tests(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    OperationBuilder::get("/qa/v1/analytics/plan/tests")
        .operation_id("qa_insights.analytics_plan_tests")
        .summary("Plan test analytics")
        .description(
            "Aggregated per-test analytics for one plan: the most recent execution's status, \
             platform, version, run and JIRA reference, plus pass/fail/total counts over every \
             execution inside the read window. plan_id is matched against the plan's path and, \
             unlike scope=plan on GET /qa/v1/analytics/overview, against every repository the \
             caller can see - there is no product_id here to fix one repository, and this is \
             the same reading a caller's one plan_id string already gets from that endpoint: \
             AnalyticsListItemDto ships repo_id too, but plan_id itself has only ever been the \
             path half. pass_count and fail_count match the runner's literal PASSED/FAILED \
             status and nothing else, so an ERROR execution counts toward the total but toward \
             neither. The read is bounded to the trailing 90 days, the same NFR-driven default \
             GET /qa/v1/analytics/build-tests uses, because the system being replaced reads \
             this table with no window at all and this one is sized in the millions of rows. \
             Ordered by test name. A plan_id naming nothing is an empty array rather than a \
             404. Requires the qa.test_result/list grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed(
            "plan_id",
            true,
            "The plan's path within its repository, matched across every repository the \
             caller can see. Required.",
            "string",
        )
        .handler(handlers::analytics_plan_tests)
        .json_array_response_with_schema::<dto::PlanTestAnalyticsDto>(
            openapi,
            StatusCode::OK,
            "One entry per test name the plan's read window admits",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi)
}

/// `GET /qa/v1/analytics/plan/builds?plan_id=`. Task 27. See
/// [`register_plan_tests`] for the shared description of `plan_id`, its fix
/// round, and the read window.
fn register_plan_builds(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    OperationBuilder::get("/qa/v1/analytics/plan/builds")
        .operation_id("qa_insights.analytics_plan_builds")
        .summary("Plan build distribution")
        .description(
            "The plan's executions grouped by version, inside the same 90-day read window and \
             the same plan_id matching as GET /qa/v1/analytics/plan/tests. total is every \
             execution of the group; passed, failed and skipped match the runner's literal \
             PASSED/FAILED/SKIPPED status and nothing else, so total can exceed their sum. A \
             version no execution named renders as the build unknown, sorted after every named \
             version. Requires the qa.test_result/list grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed(
            "plan_id",
            true,
            "The plan's path within its repository, matched across every repository the \
             caller can see. Required.",
            "string",
        )
        .handler(handlers::analytics_plan_builds)
        .json_array_response_with_schema::<dto::PlanBuildDistributionDto>(
            openapi,
            StatusCode::OK,
            "One entry per version the plan's read window admits, plus unknown",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi)
}

/// `GET /qa/v1/analytics/plan/test-history?plan_id=`. Task 27. See
/// [`register_plan_tests`] for the shared description of `plan_id`, its fix
/// round, and the read window.
fn register_plan_test_history(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    OperationBuilder::get("/qa/v1/analytics/plan/test-history")
        .operation_id("qa_insights.analytics_plan_test_history")
        .summary("Plan test history")
        .description(
            "Every test's execution history for one plan, inside the same 90-day read window \
             and the same plan_id matching as GET /qa/v1/analytics/plan/tests. Each test's own \
             results are newest first; build is the version that execution named, null rather \
             than unknown when it named none - unlike \
             GET /qa/v1/analytics/plan/builds, nothing here substitutes a label for a missing \
             version. The array itself is ordered by test name, which is this gear's own \
             choice: the system being replaced folds its rows into a hash map first and its \
             own array order is consequently unspecified. Requires the qa.test_result/list \
             grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed(
            "plan_id",
            true,
            "The plan's path within its repository, matched across every repository the \
             caller can see. Required.",
            "string",
        )
        .handler(handlers::analytics_plan_test_history)
        .json_array_response_with_schema::<dto::PlanTestHistoryDto>(
            openapi,
            StatusCode::OK,
            "One entry per test name the plan's read window admits",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi)
}
