use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::License;
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "QA Environments";

pub(super) fn register_variable_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /qa/v1/variables - List pipeline (global) + optional platform variables
    router = OperationBuilder::get("/qa/v1/variables")
        .operation_id("qa_environments.list_variables")
        .summary("List environment variables")
        .description(
            "List global pipeline variables, plus a platform's variables \
             when `platform_id` is given",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed(
            "platform_id",
            false,
            "Optional platform UUID to also include that platform's variables",
            "string",
        )
        .handler(handlers::list_variables)
        .json_array_response_with_schema::<dto::VariableDto>(
            openapi,
            StatusCode::OK,
            "List of variables",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /qa/v1/variables - Insert or update a variable by natural key
    router = OperationBuilder::put("/qa/v1/variables")
        .operation_id("qa_environments.upsert_variable")
        .summary("Create or update a variable")
        .description("Insert or update a pipeline (global) or per-platform variable by natural key")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::UpsertVariableReq>(openapi, "Variable upsert data")
        .handler(handlers::upsert_variable)
        .json_response_with_schema::<dto::VariableDto>(openapi, StatusCode::OK, "Upserted variable")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /qa/v1/variables/{id} - Delete a variable
    router = OperationBuilder::delete("/qa/v1/variables/{id}")
        .operation_id("qa_environments.delete_variable")
        .summary("Delete a variable")
        .description("Delete a pipeline or per-platform variable by UUID")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Variable UUID")
        .handler(handlers::delete_variable)
        .json_response(StatusCode::NO_CONTENT, "Variable deleted successfully")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
