use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};

use super::License;
use crate::api::rest::{dto, handlers};
use crate::infra::storage::odata::VariableFilterField;

const API_TAG: &str = "QA Environments";

pub(super) fn register_variable_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /qa/v1/variables - List pipeline (global) + optional environment variables
    router = OperationBuilder::get("/qa/v1/variables")
        .operation_id("qa_environments.list_variables")
        .summary("List environment variables")
        .description(
            "One page of the global pipeline variables, plus an environment's variables \
             when `environment_id` is given. Supports OData $filter and $orderby; the page \
             size defaults to 200 and is clamped to 500. Cursor pagination applies only \
             when `environment_id` is absent -- with it the response is the union of two \
             tables, which a single-table cursor cannot address, so the page is bounded \
             (pipeline variables first) and `next_cursor` is null. Narrow with $filter.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed(
            "environment_id",
            false,
            "Optional environment UUID to also include that environment's variables",
            "string",
        )
        .handler(handlers::list_variables)
        .json_response_with_schema::<toolkit_odata::Page<dto::VariableDto>>(
            openapi,
            StatusCode::OK,
            "One page of variables",
        )
        // The same `VariableFilterField` both repository reads translate with.
        .with_odata_filter::<VariableFilterField>()
        .with_odata_orderby::<VariableFilterField>()
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
        .description(
            "Insert or update a pipeline (global) or per-environment variable by natural key",
        )
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
        .description("Delete a pipeline or per-environment variable by UUID")
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
