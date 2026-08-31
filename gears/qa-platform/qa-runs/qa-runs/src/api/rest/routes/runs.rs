//! `/qa/v1/runs` operations.

use axum::Router;
use http::StatusCode;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};

use super::{API_TAG, License};
use crate::api::rest::{dto, handlers};
use crate::infra::storage::odata::RunFilterField;

pub(super) fn register_run_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // POST /qa/v1/runs - launch
    router = OperationBuilder::post("/qa/v1/runs")
        .operation_id("qa_runs.launch_run")
        .summary("Launch a run")
        .description(
            "Validate, resolve exclusivity, and either start the run immediately (200) or \
             admit it to its platform's queue (202). A queued run starts on its own - no \
             further call is needed. 429 means the launch was refused by a capacity \
             setting, and the response names which one: queue_max_depth for a full \
             per-platform queue, max_concurrent_runs for the cluster-wide cap.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::LaunchRunReq>(openapi, "The run to launch")
        .handler(handlers::runs::launch_run)
        // Two success bodies at two statuses. Distinct statuses are the shape
        // that is unambiguously safe: responses are keyed by status string, so
        // two bodies at one status would collide on a single map key.
        .json_response_with_schema::<dto::RunDto>(
            openapi,
            StatusCode::OK,
            "The run started immediately",
        )
        .json_response_with_schema::<dto::QueuedRunDto>(
            openapi,
            StatusCode::ACCEPTED,
            "The run was queued and will start on its own",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_429(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/runs - paginated list
    router = OperationBuilder::get("/qa/v1/runs")
        .operation_id("qa_runs.list_runs")
        .summary("List runs")
        .description(
            "One page of the runs visible to the caller, newest first. Supports OData \
             $filter, $orderby and cursor pagination; the page size defaults to 200 and is \
             clamped to 500.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::runs::list_runs)
        // `Page<RunDto>`, not `Vec<RunDto>`: every `Vec<_>` collapses onto one
        // OpenAPI component name and aborts registration. `Page<T>` carries a
        // hand-written schema impl that registers `T` alongside it.
        .json_response_with_schema::<toolkit_odata::Page<dto::RunDto>>(
            openapi,
            StatusCode::OK,
            "One page of runs",
        )
        // The same `RunFilterField` the repository translates with. Passing one
        // type to both is what stops the advertised fields and the SQL-
        // translatable fields drifting apart.
        .with_odata_filter::<RunFilterField>()
        .with_odata_orderby::<RunFilterField>()
        // 400 is reachable here and is the caller's: a `$filter` naming a field
        // outside the allow-list, or a cursor from a different sort order.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/runs/{id} - detail
    router = OperationBuilder::get("/qa/v1/runs/{id}")
        .operation_id("qa_runs.get_run")
        .summary("Get a run")
        .description(
            "The run, its five outcome counters, and its per-test result rows. The counters \
             fold FAILED and ERROR together into `failed`, matching what the run's terminal \
             verdict is derived from.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Run UUID")
        .handler(handlers::runs::get_run)
        .json_response_with_schema::<dto::RunDetailDto>(openapi, StatusCode::OK, "The run")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/runs/{id}/cancel
    router = OperationBuilder::post("/qa/v1/runs/{id}/cancel")
        .operation_id("qa_runs.cancel_run")
        .summary("Cancel a run")
        .description(
            "Stop a run in any state. Idempotent: a run that has already finished, been \
             cancelled, expired or timed out is left as it stands and still answers 204.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Run UUID")
        .handler(handlers::runs::cancel_run)
        // `.no_content_response()`, never `.json_response(NO_CONTENT, ..)`,
        // which would advertise a JSON body on a bodyless response.
        .no_content_response(StatusCode::NO_CONTENT, "Run cancelled")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        // A live execution the executor refuses to cancel is `ExecutorFailed`,
        // which is an opaque 500 - the run, its claim and its lease are all
        // left alone and the next tick retries.
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/runs/{id}/rerun
    router = OperationBuilder::post("/qa/v1/runs/{id}/rerun")
        .operation_id("qa_runs.rerun_run")
        .summary("Re-run a run")
        .description(
            "Launch a new run with the original's target, branch, parameters and tag \
             filter. Same two-outcome shape as a launch: 200 started, 202 queued. A run \
             that records no branch is refused with 400 rather than re-resolved, so a \
             re-run cannot silently execute a different branch's files.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Run UUID to repeat")
        .handler(handlers::runs::rerun_run)
        .json_response_with_schema::<dto::RunDto>(
            openapi,
            StatusCode::OK,
            "The re-run started immediately",
        )
        .json_response_with_schema::<dto::QueuedRunDto>(
            openapi,
            StatusCode::ACCEPTED,
            "The re-run was queued and will start on its own",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_429(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/runs/{id}/logs - SSE
    OperationBuilder::get("/qa/v1/runs/{id}/logs")
        .operation_id("qa_runs.stream_run_logs")
        .summary("Stream a run's live log")
        // CORRECTED 2026-08-31. This said "its output is in the archived
        // log", full stop. That over-claims: the fallback to the
        // broadcaster's in-memory tail is not vestigial, and for the ~192
        // runs that finished on the remote before `qa_run_logs` existed there
        // is no archived copy at all — for those, this endpoint answers
        // whatever the tail still holds, which after a pod restart is
        // nothing. This is a **published** description (it reaches the UI's
        // generated `openapi.d.ts` as a `@description` comment), so it has to
        // be true of every run, not of the ones written since the migration.
        //
        // Description text only — no operation id, parameter, status code,
        // content type or schema changes, so the wire contract is untouched.
        .description(
            "Server-sent events, one per log line, for as long as the run is active. A run that has \
             already finished answers an immediately-complete stream - its live channel is gone, and \
             its output is served from the archived log when one was recorded, or from whatever the \
             in-memory tail still holds when it was not; a run that finished before log archiving \
             existed has no archived copy and may answer with no lines at all. Lines are flattened to \
             one line each and truncated past 8 KiB with a marker saying so; a gap marker is emitted \
             if a slow reader falls behind.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Run UUID")
        .handler(handlers::runs::stream_run_logs)
        // `sse_json`, not `json_response_with_schema`: it hardcodes 200 and
        // publishes `text/event-stream`, which is what this endpoint actually
        // writes. Declaring JSON here while streaming SSE is a live defect in
        // another gear in this workspace, and its published spec is wrong about
        // its own content type as a result.
        .sse_json::<dto::RunLogLineDto>(openapi, "Live log lines")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi)
}
