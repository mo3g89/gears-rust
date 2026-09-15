//! `/qa/v1/queue` operations.

use axum::Router;
use http::StatusCode;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};

use super::{API_TAG, License};
use crate::api::rest::{dto, handlers};
use crate::infra::storage::odata::QueueFilterField;

pub(super) fn register_queue_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /qa/v1/queue - paginated read
    router = OperationBuilder::get("/qa/v1/queue")
        .operation_id("qa_runs.list_queue")
        .summary("Read the run queue")
        .description(
            "One page of run-queue rows, newest first, all states. `queue_position` is \
             1-based among an environment's queued rows and is computed over the rows this \
             request returned - so a narrow page understates it, and filtering by \
             environment_id is the way to get a position you can rely on. `limit` defaults to \
             200 and is clamped to 1-500; new callers should use $top instead, which wins \
             when both are given.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param("environment_id", false, "Narrow to one environment's queue")
        .query_param_typed(
            "limit",
            false,
            "Bare page size; defaults to 200, clamped to 1-500. $top takes precedence.",
            "integer",
        )
        .handler(handlers::queue::list_queue)
        .json_response_with_schema::<toolkit_odata::Page<dto::QueueEntryDto>>(
            openapi,
            StatusCode::OK,
            "One page of run-queue rows",
        )
        .with_odata_filter::<QueueFilterField>()
        .with_odata_orderby::<QueueFilterField>()
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /qa/v1/queue/{id}
    router = OperationBuilder::delete("/qa/v1/queue/{id}")
        .operation_id("qa_runs.cancel_queued_row")
        .summary("Cancel a queued row")
        .description(
            "Drop a row that has not started, retiring its run in the same transaction. \
             Answers 409 when the row has already left `queued`: it then holds a claim on \
             its environment, and dropping it here would release an environment a live \
             execution still owns.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Queue row UUID")
        .handler(handlers::queue::cancel_queued)
        .no_content_response(StatusCode::NO_CONTENT, "Queue row cancelled")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        // `QueueRowNotQueued` maps to `aborted`, which renders 409.
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/queue/{id}/force-start
    OperationBuilder::post("/qa/v1/queue/{id}/force-start")
        .operation_id("qa_runs.force_start_queued_row")
        .summary("Force-start a queued row")
        .description(
            "Start a queued row now, bypassing the environment occupancy check. The \
             cluster-wide max_concurrent_runs cap is still enforced, so this can still \
             answer 429 - the asymmetry is deliberate: an operator may override \
             exclusivity on one environment, but not the limit that protects the whole \
             cluster. The override is logged with the run and the environment named.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Queue row UUID")
        .handler(handlers::queue::force_start)
        .json_response_with_schema::<dto::StartedRunDto>(
            openapi,
            StatusCode::OK,
            "The run that was started",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_429(openapi)
        .error_500(openapi)
        .register(router, openapi)
}
