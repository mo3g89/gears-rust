use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::License;
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "QA Environments";

pub(super) fn register_environment_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /qa/v1/environments - List target environments
    router = OperationBuilder::get("/qa/v1/environments")
        .operation_id("qa_environments.list_environments")
        .summary("List target environments")
        .description("Retrieve all target environments visible to the caller")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_environments)
        .json_array_response_with_schema::<dto::EnvironmentDto>(
            openapi,
            StatusCode::OK,
            "List of target environments",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/environments - Register a new target environment
    router = OperationBuilder::post("/qa/v1/environments")
        .operation_id("qa_environments.create_environment")
        .summary("Register a new target environment")
        .description("Create a new target environment with a credstore kubeconfig reference")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateEnvironmentReq>(openapi, "Environment creation data")
        .handler(handlers::create_environment)
        .json_response_with_schema::<dto::EnvironmentDto>(
            openapi,
            StatusCode::CREATED,
            "Created environment",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/environments/{id} - Get a target environment
    router = OperationBuilder::get("/qa/v1/environments/{id}")
        .operation_id("qa_environments.get_environment")
        .summary("Get a target environment")
        .description("Retrieve a specific target environment by UUID")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Environment UUID")
        .handler(handlers::get_environment)
        .json_response_with_schema::<dto::EnvironmentDto>(
            openapi,
            StatusCode::OK,
            "Environment found",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PATCH /qa/v1/environments/{id} - Update a target environment
    router = OperationBuilder::patch("/qa/v1/environments/{id}")
        .operation_id("qa_environments.update_environment")
        .summary("Update a target environment")
        .description("Partially update a target environment's fields")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Environment UUID")
        .json_request::<dto::UpdateEnvironmentReq>(openapi, "Environment update data")
        .handler(handlers::update_environment)
        .json_response_with_schema::<dto::EnvironmentDto>(
            openapi,
            StatusCode::OK,
            "Updated environment",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /qa/v1/environments/{id} - Delete a target environment
    router = OperationBuilder::delete("/qa/v1/environments/{id}")
        .operation_id("qa_environments.delete_environment")
        .summary("Delete a target environment")
        .description("Delete a target environment by UUID; fails if it holds an active lease")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Environment UUID")
        .handler(handlers::delete_environment)
        .json_response(StatusCode::NO_CONTENT, "Environment deleted successfully")
        // EnvironmentLeased maps to FailedPrecondition, which renders as HTTP 400.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/environments/{id}/refresh - run one on-demand detection cycle
    router = OperationBuilder::post("/qa/v1/environments/{id}/refresh")
        .operation_id("qa_environments.refresh_environment")
        .summary("Refresh a target environment's observed version")
        .description(
            "Run one detection cycle against the environment's own cluster and return the \
             refreshed environment. A detection failure (cluster unreachable, install metadata \
             missing) is a fact about the environment, reported as HTTP 200 with \
             version_detect_error populated, never as an error response. An error response is \
             reserved for a fact about the request (404: the environment does not exist) or a \
             genuine fault of this service -- never for detection itself failing.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Environment UUID")
        .handler(handlers::refresh_environment)
        .json_response_with_schema::<dto::EnvironmentDto>(
            openapi,
            StatusCode::OK,
            "Refreshed environment",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/environments/{id}/lease - Read-only lease view.
    //
    // Acquire/release are deliberately NOT registered as REST routes: they
    // are SDK-only operations consumed by the qa-runs dispatcher via
    // `qa_environments_sdk::QaEnvironmentsClientV1`. This endpoint exists
    // only so operators/engineers can see *why* a run is queued/waiting.
    router = OperationBuilder::get("/qa/v1/environments/{id}/lease")
        .operation_id("qa_environments.get_environment_lease")
        .summary("Get a target environment's lease state")
        .description("Read-only view of the current lease occupancy for an environment")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Environment UUID")
        .handler(handlers::get_environment_lease)
        .json_response_with_schema::<dto::LeaseDto>(openapi, StatusCode::OK, "Current lease state")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
