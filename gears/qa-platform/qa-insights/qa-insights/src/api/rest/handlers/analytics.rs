//! The analytics overview, its build-tests drill-down, its export (Task 25b,
//! Task 26) and the three plan drill-downs (Task 27).
//!
//! **Two statements each** — one delegation and one conversion — and what is
//! *not* here is the point: no scope, no clamping of the two day counts, no
//! validation of the query string, and no status code.
//!
//! # The three plan handlers take one query parameter, and it used to be a
//! # path segment
//!
//! `analytics_plan_tests`, `analytics_plan_builds` and
//! `analytics_plan_test_history` extract `plan_id` through
//! [`AnalyticsPlanQuery`] — legacy's three plan endpoints
//! (`analytics.rs:2434-2572`) otherwise take no parameters at all: no
//! product, no version, no scope. `plan_id` reaches
//! [`AnalyticsService`](crate::domain::service::analytics::AnalyticsService)
//! as a plain `&str`, unvalidated, exactly as the shared query string reaches
//! the other three handlers unvalidated.
//!
//! **This was originally a `{plan_id}` path segment, and the fix round on
//! Task 27 moved it.** A real `plan_path` routinely contains `/` (this
//! crate's own fixtures, `plans/nightly/plan.yaml`), which a single path
//! segment cannot carry unencoded — the route 404'd for any real plan, and
//! nothing crossed the HTTP boundary to catch it. `?plan_id=` is what this
//! gear already ships for the overview's own plan scope
//! ([`AnalyticsPlanQuery`]'s header), so the query form is not a new
//! shape, only a corrected one.
//!
//! **The validation in particular belongs to the service.** Legacy runs
//! `normalize_overview_query` in its handler (`manager/src/routes/analytics.rs:364`)
//! and this gear does not, for the reason
//! [`super::dashboard`]'s header gives about `days`: the rules are a property of
//! the operation rather than of the transport, so a future non-HTTP caller has
//! to get the same six 400s. They live in
//! [`crate::domain::analytics::query`], are applied by
//! [`AnalyticsService`](crate::domain::service::analytics::AnalyticsService), and
//! reach the wire as `invalid_argument` through
//! [`crate::api::rest::error`]'s single `Validation` arm.
//!
//! The two request DTOs are `From`-converted into the domain's query types
//! rather than passed through, which keeps wire-contract `serde` out of the
//! domain — the layering [`crate::api::rest::dto`]' header states. (Not an
//! absolute "no `serde` at all": `domain::service::collect::CollectService`
//! derives `serde::Serialize` on a private struct to drive
//! `serde_urlencoded` for its own outbound query string — see that module's
//! doc for why. That is unrelated to *this* statement, which is about wire
//! DTOs crossing the REST boundary, not about every `serde` derive anywhere
//! under `domain`.)
//!
//! # `analytics_export` is a third handler, and it is not two statements
//!
//! Task 26's export reduces the same
//! [`AnalyticsOverview`](crate::domain::service::analytics::AnalyticsOverview)
//! the two handlers above already build, to one section (or all of them), as
//! CSV or JSON.
//! `domain::analytics::export` holds the `section`/`format` vocabularies, the
//! `400` for an unrecognized JSON section, and the whole CSV builder — see
//! that module's header for why CSV assembly is pure domain code and JSON
//! assembly is not. [`export_section_json`] is the second half, kept in this
//! file rather than in `domain::analytics::export` for exactly that reason:
//! it calls `serde_json::to_string_pretty` on [`AnalyticsOverviewDto`], and the
//! domain speaks no wire-contract `serde` (`domain::service::collect` derives
//! `serde::Serialize` on one private struct of its own, to build an outbound
//! query string — a different thing from decoding or encoding a wire DTO,
//! and not a counterexample to why this JSON assembly stays out here).

use std::sync::Arc;

use axum::Extension;
use axum::extract::Query;
use axum::http::header;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{
    AnalyticsBuildTestsQuery, AnalyticsExportQuery, AnalyticsOverviewDto, AnalyticsOverviewQuery,
    AnalyticsPlanQuery, BuildTestDetailDto, PlanBuildDistributionDto, PlanTestAnalyticsDto,
    PlanTestHistoryDto, plan_test_analytics_list_dto,
};
use crate::domain::analytics::export::{self, ExportOverview, ExportSection};
use crate::domain::error::DomainError;
use crate::gear::ConcreteAppServices;

/// `GET /qa/v1/analytics/overview` — the eight-section overview.
///
/// Every parameter reaches the service unvalidated; see this module's header.
#[tracing::instrument(
    skip(svc, ctx, query),
    fields(
        analytics.product_id = %query.product_id,
        analytics.scope = %query.scope,
        analytics.group_by = ?query.group_by,
    )
)]
pub async fn analytics_overview(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<AnalyticsOverviewQuery>,
) -> ApiResult<Json<AnalyticsOverviewDto>> {
    let overview = svc.analytics.overview(&ctx, query.into()).await?;
    Ok(Json(AnalyticsOverviewDto::from(overview)))
}

/// `GET /qa/v1/analytics/build-tests` — the tests of one build.
///
/// `build` is refused before every other rule, which is legacy's order
/// (`analytics.rs:373-376` precedes `:379`) and is the service's to enforce.
#[tracing::instrument(
    skip(svc, ctx, query),
    fields(
        analytics.product_id = %query.product_id,
        analytics.build = %query.build,
    )
)]
pub async fn analytics_build_tests(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<AnalyticsBuildTestsQuery>,
) -> ApiResult<Json<Vec<BuildTestDetailDto>>> {
    let items = svc.analytics.build_tests(&ctx, query.into()).await?;
    Ok(Json(
        items.into_iter().map(BuildTestDetailDto::from).collect(),
    ))
}

/// `GET /qa/v1/analytics/export` — the overview, reduced to one section (or
/// all of them), as CSV or JSON.
///
/// `api_export` (`manager/src/routes/analytics.rs:457-519`). `format` and
/// `section` are read and normalized **before** the overview is built here,
/// which is the *opposite* order from legacy: `build_overview` is legacy's
/// `:473` and its `format`/`section` normalization is `:475-486`, strictly
/// after. The reordering is forced by this gear's shape rather than chosen for
/// parity — `query.into()` consumes `query` to build the overview, so `format`
/// and `section` have to be read off it first — and it has no observable
/// effect: both normalizations are infallible (no rejection reads the
/// overview), and the overview is still built before the `section` vocabulary
/// is checked, exactly as legacy checks it only after `build_overview` has
/// already run. The two response headers (`content-type`,
/// `content-disposition`) and the two filenames (`analytics-export.csv` /
/// `.json`) are legacy's own (`:490-501`, `:506-517`).
///
/// The CSV branch validates nothing about `section` and can answer `200` with
/// an empty body; the JSON branch validates it and can answer `400`. Both are
/// legacy's own behaviour — see `domain::analytics::export`'s header for the
/// full argument, which is the reason this function checks `is_csv` before
/// either the DTO conversion or the section parse.
#[tracing::instrument(
    skip(svc, ctx, query),
    fields(
        analytics.product_id = %query.product_id,
        analytics.format = ?query.format,
        analytics.section = ?query.section,
    )
)]
pub async fn analytics_export(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<AnalyticsExportQuery>,
) -> ApiResult<impl IntoResponse> {
    let is_csv = export::is_csv_format(query.format.as_deref());
    let section_raw = export::normalize_export_section(query.section.as_deref());

    let overview = svc.analytics.overview(&ctx, query.into()).await?;

    if is_csv {
        let body = export::overview_to_csv(
            section_raw.as_str(),
            &ExportOverview {
                summary: &overview.summary,
                lists: &overview.lists,
                heatmap: &overview.heatmap,
                trend: &overview.trend,
                flaky: &overview.flaky,
                platform_names: &overview.platform_names,
            },
        );
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=analytics-export.csv",
                ),
            ],
            body,
        )
            .into_response());
    }

    let section = export::parse_export_section(section_raw.as_str())?;
    let dto = AnalyticsOverviewDto::from(overview);
    let body = export_section_json(&dto, section)?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=analytics-export.json",
            ),
        ],
        body,
    )
        .into_response())
}

/// `GET /qa/v1/analytics/plan/tests?plan_id=` — one test's aggregated
/// analytics.
///
/// `api_plan_tests` (`manager/src/routes/analytics.rs:2434-2483`). `plan_id`
/// reaches the service unvalidated and unresolved — see
/// `domain::service::analytics::AnalyticsService::plan_tests`'s header for how
/// it is matched.
#[tracing::instrument(skip(svc, ctx), fields(analytics.plan_id = %query.plan_id))]
pub async fn analytics_plan_tests(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<AnalyticsPlanQuery>,
) -> ApiResult<Json<Vec<PlanTestAnalyticsDto>>> {
    let response = svc.analytics.plan_tests(&ctx, &query.plan_id).await?;
    Ok(Json(plan_test_analytics_list_dto(response)))
}

/// `GET /qa/v1/analytics/plan/builds?plan_id=` — the plan's build
/// distribution.
///
/// `api_plan_builds` (`manager/src/routes/analytics.rs:2486-2527`).
#[tracing::instrument(skip(svc, ctx), fields(analytics.plan_id = %query.plan_id))]
pub async fn analytics_plan_builds(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<AnalyticsPlanQuery>,
) -> ApiResult<Json<Vec<PlanBuildDistributionDto>>> {
    let dists = svc.analytics.plan_builds(&ctx, &query.plan_id).await?;
    Ok(Json(
        dists
            .into_iter()
            .map(PlanBuildDistributionDto::from)
            .collect(),
    ))
}

/// `GET /qa/v1/analytics/plan/test-history?plan_id=` — each test's run
/// history.
///
/// `api_plan_test_history` (`manager/src/routes/analytics.rs:2530-2572`). See
/// `domain::service::analytics::PlanTestHistory`'s header for why the outer
/// array's order is this port's own rather than legacy's unspecified `HashMap`
/// order.
#[tracing::instrument(skip(svc, ctx), fields(analytics.plan_id = %query.plan_id))]
pub async fn analytics_plan_test_history(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<AnalyticsPlanQuery>,
) -> ApiResult<Json<Vec<PlanTestHistoryDto>>> {
    let history = svc
        .analytics
        .plan_test_history(&ctx, &query.plan_id)
        .await?;
    Ok(Json(
        history.into_iter().map(PlanTestHistoryDto::from).collect(),
    ))
}

/// The JSON body for one export section — legacy's `overview_section_json`
/// (`:2234-2260`), minus the vocabulary check: this function's `section`
/// argument is already an [`ExportSection`], not a string, because
/// [`export::parse_export_section`] ran first and there is no `_` arm left to
/// reach.
///
/// Lives here rather than in `domain::analytics::export` because it
/// `serde_json`-serializes [`AnalyticsOverviewDto`] and five of its fields —
/// see this module's header and `domain::analytics::export`'s for the
/// layering this respects.
fn export_section_json(
    overview: &AnalyticsOverviewDto,
    section: ExportSection,
) -> ApiResult<String> {
    let value = match section {
        ExportSection::Summary => serde_json::to_string_pretty(&overview.summary),
        ExportSection::Lists => serde_json::to_string_pretty(&overview.lists),
        ExportSection::Heatmap => serde_json::to_string_pretty(&overview.heatmap),
        ExportSection::Trend => serde_json::to_string_pretty(&overview.trend),
        ExportSection::Flaky => serde_json::to_string_pretty(&overview.flaky),
        ExportSection::All => serde_json::to_string_pretty(overview),
    };
    value
        .map_err(|err| DomainError::Internal(format!("failed to serialize export payload: {err}")))
        .map_err(CanonicalError::from)
}
