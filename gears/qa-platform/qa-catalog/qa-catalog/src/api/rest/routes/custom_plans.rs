use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::License;
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "QA Catalog";

pub(super) fn register_custom_plan_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /qa/v1/custom-plans - List custom plans
    router = OperationBuilder::get("/qa/v1/custom-plans")
        .operation_id("qa_catalog.list_custom_plans")
        .summary("List custom plans")
        .description("Retrieve all user-composed custom plans visible to the caller")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_custom_plans)
        .json_array_response_with_schema::<dto::CustomPlanDto>(
            openapi,
            StatusCode::OK,
            "List of custom plans",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/custom-plans - Create a custom plan
    router = OperationBuilder::post("/qa/v1/custom-plans")
        .operation_id("qa_catalog.create_custom_plan")
        .summary("Create a custom plan")
        .description("Create a user-composed plan referencing files across repositories")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::UpsertCustomPlanReq>(openapi, "Custom plan data")
        .handler(handlers::create_custom_plan)
        .json_response_with_schema::<dto::CustomPlanDto>(
            openapi,
            StatusCode::CREATED,
            "Created custom plan",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/custom-plans/{id} - Get a custom plan
    router = OperationBuilder::get("/qa/v1/custom-plans/{id}")
        .operation_id("qa_catalog.get_custom_plan")
        .summary("Get a custom plan")
        .description("Retrieve a specific custom plan by UUID")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Custom plan UUID")
        .handler(handlers::get_custom_plan)
        .json_response_with_schema::<dto::CustomPlanDto>(
            openapi,
            StatusCode::OK,
            "Custom plan found",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /qa/v1/custom-plans/{id} - Replace a custom plan
    router = OperationBuilder::put("/qa/v1/custom-plans/{id}")
        .operation_id("qa_catalog.update_custom_plan")
        .summary("Replace a custom plan")
        .description("Full-document update of a custom plan (per the SDK contract)")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Custom plan UUID")
        .json_request::<dto::UpsertCustomPlanReq>(openapi, "Replacement custom plan data")
        .handler(handlers::update_custom_plan)
        .json_response_with_schema::<dto::CustomPlanDto>(
            openapi,
            StatusCode::OK,
            "Updated custom plan",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /qa/v1/custom-plans/{id} - Delete a custom plan
    router = OperationBuilder::delete("/qa/v1/custom-plans/{id}")
        .operation_id("qa_catalog.delete_custom_plan")
        .summary("Delete a custom plan")
        .description("Delete a custom plan by UUID")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Custom plan UUID")
        .handler(handlers::delete_custom_plan)
        // 204 carries no body — see `routes::test_repos`.
        .no_content_response(StatusCode::NO_CONTENT, "Custom plan deleted successfully")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
