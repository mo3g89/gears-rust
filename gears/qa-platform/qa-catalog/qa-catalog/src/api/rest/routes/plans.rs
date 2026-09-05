use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::License;
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "QA Catalog";

pub(super) fn register_plan_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/plans?repo_id&branch - Discovered plans.
    //
    // Single-plan reads (`get_plan`) and TEST_META reads (`get_test_meta`)
    // are deliberately NOT registered as REST routes: they are SDK-only
    // operations consumed by the qa-runs dispatcher via
    // `qa_catalog_sdk::QaCatalogClientV1`.
    router = OperationBuilder::get("/qa/v1/plans")
        .operation_id("qa_catalog.list_plans")
        .summary("List discovered plans")
        .description(
            "Discover plans from the synced working copy of the given \
             repository and branch (plans are never persisted; they are \
             materialized on read)",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed("repo_id", true, "Repository UUID", "string")
        .query_param_typed("branch", true, "Branch name", "string")
        .handler(handlers::list_plans)
        .json_array_response_with_schema::<dto::PlanDto>(
            openapi,
            StatusCode::OK,
            "Discovered plans",
        )
        // Missing/invalid query params and RepoNotSynced both render 400.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
