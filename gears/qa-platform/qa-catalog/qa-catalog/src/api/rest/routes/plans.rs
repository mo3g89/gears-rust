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
             materialized on read). A branch the remote has but that was \
             never synced is synced on this first read, which needs the sync \
             permission on the repository. A branch the remote does not have \
             answers 404. A repository whose credential cannot be resolved or \
             is rejected by the remote answers 400 with the reason, recorded \
             in its `sync_error`; a remote that cannot be listed (including \
             one answering HTTP 403) answers 503. For a short backoff after \
             either failure, every read that would sync that repository gives \
             the same answer without contacting the remote.",
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
        // Missing/invalid query params, RepoNotSynced (including a recorded
        // credential fault) render 400; BranchNotFound is 404; a remote that
        // cannot be listed, or one inside its backoff for that, is 503.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
}
