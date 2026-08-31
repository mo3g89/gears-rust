use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::License;
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "QA Environments";

pub(super) fn register_platform_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /qa/v1/platforms - List target platforms
    router = OperationBuilder::get("/qa/v1/platforms")
        .operation_id("qa_environments.list_platforms")
        .summary("List target platforms")
        .description("Retrieve all target platforms visible to the caller")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_platforms)
        .json_array_response_with_schema::<dto::PlatformDto>(
            openapi,
            StatusCode::OK,
            "List of target platforms",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/platforms - Register a new target platform
    router = OperationBuilder::post("/qa/v1/platforms")
        .operation_id("qa_environments.create_platform")
        .summary("Register a new target platform")
        .description("Create a new target platform with a credstore kubeconfig reference")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreatePlatformReq>(openapi, "Platform creation data")
        .handler(handlers::create_platform)
        .json_response_with_schema::<dto::PlatformDto>(
            openapi,
            StatusCode::CREATED,
            "Created platform",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/platforms/{id} - Get a target platform
    router = OperationBuilder::get("/qa/v1/platforms/{id}")
        .operation_id("qa_environments.get_platform")
        .summary("Get a target platform")
        .description("Retrieve a specific target platform by UUID")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Platform UUID")
        .handler(handlers::get_platform)
        .json_response_with_schema::<dto::PlatformDto>(openapi, StatusCode::OK, "Platform found")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PATCH /qa/v1/platforms/{id} - Update a target platform
    router = OperationBuilder::patch("/qa/v1/platforms/{id}")
        .operation_id("qa_environments.update_platform")
        .summary("Update a target platform")
        .description("Partially update a target platform's fields")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Platform UUID")
        .json_request::<dto::UpdatePlatformReq>(openapi, "Platform update data")
        .handler(handlers::update_platform)
        .json_response_with_schema::<dto::PlatformDto>(openapi, StatusCode::OK, "Updated platform")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /qa/v1/platforms/{id} - Delete a target platform
    router = OperationBuilder::delete("/qa/v1/platforms/{id}")
        .operation_id("qa_environments.delete_platform")
        .summary("Delete a target platform")
        .description("Delete a target platform by UUID; fails if it holds an active lease")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Platform UUID")
        .handler(handlers::delete_platform)
        .json_response(StatusCode::NO_CONTENT, "Platform deleted successfully")
        // PlatformLeased maps to FailedPrecondition, which renders as HTTP 400.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/platforms/{id}/refresh - run one on-demand detection cycle
    router = OperationBuilder::post("/qa/v1/platforms/{id}/refresh")
        .operation_id("qa_environments.refresh_platform")
        .summary("Refresh a target platform's observed version")
        .description(
            "Run one detection cycle against the platform's own cluster and return the \
             refreshed platform. A detection failure (cluster unreachable, install metadata \
             missing) is a fact about the platform, reported as HTTP 200 with \
             version_detect_error populated, never as an error response. An error response is \
             reserved for a fact about the request (404: the platform does not exist) or a \
             genuine fault of this service -- never for detection itself failing.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Platform UUID")
        .handler(handlers::refresh_platform)
        .json_response_with_schema::<dto::PlatformDto>(openapi, StatusCode::OK, "Refreshed platform")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/platforms/{id}/lease - Read-only lease view.
    //
    // Acquire/release are deliberately NOT registered as REST routes: they
    // are SDK-only operations consumed by the qa-runs dispatcher via
    // `qa_environments_sdk::QaEnvironmentsClientV1`. This endpoint exists
    // only so operators/engineers can see *why* a run is queued/waiting.
    router = OperationBuilder::get("/qa/v1/platforms/{id}/lease")
        .operation_id("qa_environments.get_platform_lease")
        .summary("Get a target platform's lease state")
        .description("Read-only view of the current lease occupancy for a platform")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Platform UUID")
        .handler(handlers::get_platform_lease)
        .json_response_with_schema::<dto::LeaseDto>(openapi, StatusCode::OK, "Current lease state")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
