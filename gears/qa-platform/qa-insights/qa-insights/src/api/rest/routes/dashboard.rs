//! The dashboard routes — `GET /qa/v1/dashboard` (Task 18) and
//! `GET /qa/v1/dashboard/coverage` (Task 19).
//!
//! # No `OData`, and that is per D7 rather than an omission
//!
//! `api::rest::mod`' header states it: `OData` applies to the two flat result
//! collections and to nothing else in this gear, because those are the tables
//! `cpt-cf-qa-nfr-scale`'s 5M-row target is about. This endpoint answers with an
//! aggregate, so there is no row set for a `$filter` to narrow and no page for a
//! cursor to walk. `days` is a plain query parameter.
//!
//! # The description carries what the payload cannot say for itself
//!
//! Four things a client cannot discover from the schema and would otherwise
//! discover by being wrong about them: `total_runs` counts runs whose results are
//! *ingested* rather than every run qa-runs has, four fields of legacy's
//! payload are absent rather than zero, the 24-hour KPI block counts a
//! different row set from every other number in the payload — it includes runs
//! that have not finished, which is legacy's rule and is invisible on the wire —
//! and `flaky_tests` has a **third** window, seven days, which no more moves with
//! `days` than the 24-hour block does. It was three things and five absent fields
//! until Task 23b computed `flaky_tests`.
//!
//! `api::rest::dto::DashboardStatsDto` and `domain::service::dashboard`'s header
//! carry the same four facts, which is
//! duplication accepted for the reason
//! `routes::collections`' header records about its own page-size literals: an
//! `OperationBuilder::description` takes a `&str` and cannot interpolate anything.
//!
//! The coverage operation has the same obligation for a harder fact: its array is
//! **empty until an upstream for the percentages exists**, and a caller has no way
//! to tell that from an empty deployment. `api::rest::dto::CoverageBuildDto` is
//! where the reasons and the citations live — it was
//! `domain::service::dashboard`'s header until Task 21b's doc split; the
//! description says the fact and nothing else.
//!
//! **That description is written for an API consumer, and deliberately names
//! neither legacy's route nor a roadmap milestone.** `/api/dashboard/coverage` and
//! "p2 with 2.7" are this project's vocabulary rather than a caller's: a client
//! needs to know that the array is empty and that no number is being invented, not
//! which internal deliverable will change that. Both facts stay on that DTO,
//! where the audience is an implementer.
//!
//! # Coverage declares no 400, and that is not an oversight
//!
//! It takes no parameters — legacy's `api_coverage` takes only `State`
//! (`manager/src/routes/dashboard.rs:573`) — so there is no extractor that can
//! fail and no bad request to answer. Every error it can produce comes from the
//! PDP or from a driver.

use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::{API_TAG, License};
use crate::api::rest::{dto, handlers};

pub(super) fn register_dashboard_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/dashboard — run activity plus the ingested counters behind it.
    //
    // `.authenticated()` is not the authorization: `DashboardService` compiles a
    // `PolicyEnforcer` decision on `qa.test_result` / `list` — the same pair the
    // two collections use, and that module's header says why a separate
    // `view_dashboard` action was rejected — immediately before the reads. 403 is
    // therefore a documented outcome for an authenticated caller, and it is also
    // what a caller qa-runs itself refuses gets.
    let router = OperationBuilder::get("/qa/v1/dashboard")
        .operation_id("qa_insights.dashboard")
        .summary("Dashboard aggregate")
        .description(
            "Run activity and the test counters behind it, in one payload. Live run state \
             (active, queued, the recent list) is read from qa-runs on every request and is \
             never cached here, because qa-runs owns it; the counters are computed over this \
             gear's ingested results. days sets the length of the daily pass/fail trend: it \
             defaults to 14 and is silently clamped to 3-90, so days=365 answers with 90 \
             points rather than an error. Every day in the window is present, including days \
             nothing ran. total_runs counts the runs this gear holds results for, which is \
             not the same as every run ever launched - a run whose results have not been \
             ingested yet is absent, and asynchronous ingest makes that a normal transient \
             state. The recent list holds ten runs and the active list holds at most ten, \
             using the same predicate as the active count, so a caller reading 23 active gets \
             ten entries. The 24-hour block - failed_recent, failed_24h_count, \
             failed_prev_24h_count, pass_rate_24h and pass_rate_prev_24h - has a window of \
             its own and does not move with days; it counts a wider row set than the trends \
             do, because a run that has not finished yet contributes to it. A pass rate is a \
             ratio between 0 and 1 over passed-plus-failed rows, so skipped tests do not \
             lower it, and it is null rather than 0 when the window held nothing to divide \
             by. flaky_tests has a third window - seven days, also fixed - and holds \
             at most ten tests that both passed and failed in it, flakiest first, where \
             flakiest means the larger count of the smaller of the two status groups; a \
             test that only passed or only failed is absent rather than listed with a \
             zero, so an empty array is the ordinary answer on a healthy suite. \
             quality_vectors_pass_rate has a fourth window, also seven days and also \
             fixed, and holds one entry per Quality Vector declared by any test file \
             with a row in that window, highest total first; a file declaring two \
             vectors contributes its executions to both, so the totals across that \
             array are not a row count, and an entry whose counters are all zero \
             means every file carrying that vector was skipped. Three fields of the legacy dashboard payload are deliberately \
             absent rather than reported as zero, because nothing computes them yet: \
             total_plans, total_schedules and platforms_summary. Requires the \
             qa.test_result/list grant, the same one the test-result collections \
             need.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed(
            "days",
            false,
            "Days of history for the daily trend. Defaults to 14, clamped to 3-90.",
            "integer",
        )
        .handler(handlers::dashboard)
        .json_response_with_schema::<dto::DashboardStatsDto>(
            openapi,
            StatusCode::OK,
            "Run activity and the ingested counters behind it",
        )
        // 400 is reachable and is the caller's: a `days` that is not an integer
        // fails deserialization in the extractor. A `days` that is merely out of
        // range is **not** a 400 — it is clamped, which is legacy's behaviour and
        // what the description says. There is no 404: an empty deployment is a
        // dashboard of zeros, not a missing resource.
        .error_400(openapi)
        .error_401(openapi)
        // 403 covers both this gear's PDP and qa-runs'. A subject-level refusal on
        // the far side is deliberately *not* laundered into an empty dashboard;
        // `infra::clients::qa_runs`' header carries that argument.
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/dashboard/coverage — coverage per product build.
    //
    // Same grant as the dashboard, deliberately: `DashboardService::coverage`
    // compiles a decision on `qa.test_result` / `list`, and that module's header
    // records why a second aggregate does not get a second action.
    OperationBuilder::get("/qa/v1/dashboard/coverage")
        .operation_id("qa_insights.dashboard_coverage")
        .summary("Coverage per product build")
        .description(
            "Code coverage per product: one point per product, from the latest completed run \
             that reported coverage, carrying product_key, version, a build label of the two \
             joined by a slash, and line, branch and function percentages. Takes no \
             parameters. A build with no measured coverage is absent from the array rather \
             than reported as zero. The array is empty in every deployment today, and not \
             only where coverage collection is switched off: nothing in this system measures \
             a coverage point yet, and no number is folded out of the ingested test results \
             to fill the gap. Requires the qa.test_result/list grant, the same one the \
             dashboard and the test-result collections need.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::dashboard_coverage)
        .json_array_response_with_schema::<dto::CoverageBuildDto>(
            openapi,
            StatusCode::OK,
            "One coverage point per product, newest reported version each",
        )
        // No 400: there is no parameter to get wrong. No 404: a deployment with
        // no coverage is an empty array, not a missing resource.
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi)
}
