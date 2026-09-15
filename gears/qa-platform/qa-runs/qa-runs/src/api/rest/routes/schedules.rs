//! `/qa/v1/schedules` operations.

use axum::Router;
use http::StatusCode;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::{API_TAG, License};
use crate::api::rest::{dto, handlers};

pub(super) fn register_schedule_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /qa/v1/schedules - unpaginated list
    router = OperationBuilder::get("/qa/v1/schedules")
        .operation_id("qa_runs.list_schedules")
        .summary("List schedules")
        .description(
            "Every schedule visible to the caller, by name. Unpaginated, unlike the runs \
             list: schedules are operator-authored and bounded by how many an operator \
             writes, whereas qa_runs grows on its own.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::schedules::list_schedules)
        // `json_array_response_with_schema`, never `json_response_with_schema::
        // <Vec<_>>`: utoipa names every `Vec` `Vec`, so registering one as a
        // component collides with every other list response and aborts
        // registration at boot.
        .json_array_response_with_schema::<dto::ScheduleDto>(
            openapi,
            StatusCode::OK,
            "The caller's schedules",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/schedules
    router = OperationBuilder::post("/qa/v1/schedules")
        .operation_id("qa_runs.create_schedule")
        .summary("Create a schedule")
        .description(
            "Store a cron schedule under the caller's tenant. The expression is parsed \
             here, not at firing time: an expression this gear cannot read would otherwise \
             be stored happily and then fail on every evaluation, forever, in a background \
             pass whose only output is a log line. 400 covers that and the exclusive_choice \
             token; 409 means the name is already taken within the tenant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::NewScheduleReq>(openapi, "The schedule to create")
        .handler(handlers::schedules::create_schedule)
        .json_response_with_schema::<dto::ScheduleDto>(
            openapi,
            StatusCode::CREATED,
            "The stored schedule",
        )
        // 400 is three things here: an unparseable cron expression
        // (`DomainError::InvalidCron`), an `exclusive_choice` outside the three
        // tokens, and the empty name and column-width checks - all
        // `invalid_argument`, all naming their field.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        // `ScheduleNameExists` maps to `already_exists`, which renders 409. The
        // unique violation is mapped in the repository, so a duplicate name is
        // never a 500.
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /qa/v1/schedules/{id}
    router = OperationBuilder::get("/qa/v1/schedules/{id}")
        .operation_id("qa_runs.get_schedule")
        .summary("Get a schedule")
        .description(
            "One schedule. Absent and another tenant's are the same 404, which is what \
             stops this endpoint answering whether an id exists elsewhere.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Schedule UUID")
        .handler(handlers::schedules::get_schedule)
        .json_response_with_schema::<dto::ScheduleDto>(openapi, StatusCode::OK, "The schedule")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /qa/v1/schedules/{id} - full replace
    router = OperationBuilder::put("/qa/v1/schedules/{id}")
        .operation_id("qa_runs.replace_schedule")
        .summary("Replace a schedule")
        .description(
            "A full replace taking the same body as the create, which is also what the \
             source system's edit form sends. Not a PATCH: exclusive_choice is itself a \
             tri-state, so a patch would have to distinguish 'leave it' from 'set it to \
             inherit' and this workspace has no dependency that can. `enabled` is \
             therefore required - omitting it must not be able to silently pause or resume \
             a schedule whose cron expression was the only thing being edited. \
             last_fired_tick is server-owned and is not moved by an edit, so rewriting an \
             expression cannot re-fire the past.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Schedule UUID")
        .json_request::<dto::NewScheduleReq>(openapi, "The schedule's new content, in full")
        .handler(handlers::schedules::replace_schedule)
        .json_response_with_schema::<dto::ScheduleDto>(
            openapi,
            StatusCode::OK,
            "The replaced schedule",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        // Renaming onto a name another schedule in the tenant holds - the same
        // conflict as the create's, from the other side.
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /qa/v1/schedules/{id}/notifications
    router = OperationBuilder::put("/qa/v1/schedules/{id}/notifications")
        .operation_id("qa_runs.update_schedule_notifications")
        .summary("Replace a schedule's Slack notification settings")
        .description(
            "The three per-schedule Slack settings, and nothing else on the schedule: a              schedule pinned exclusive comes back pinned exclusive, because the other              columns are not in the UPDATE at all. A full, idempotent replacement of a              settings sub-resource is a PUT, and the REST surface - unlike the test-facing              contract - is not frozen. All three fields are required, for the reason the              schedule replace requires `enabled`. 400 is an event name outside the six              (`pending`, `in_progress`, `succeeded`, `failed`, `error`,              `skipped`) or a channel wider than the column. qa-runs stores these and sends              nothing; the sending is qa-insights'.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Schedule UUID")
        .json_request::<dto::UpdateScheduleNotificationsReq>(
            openapi,
            "The notification settings, in full",
        )
        .handler(handlers::schedules::update_schedule_notifications)
        .json_response_with_schema::<dto::ScheduleDto>(
            openapi,
            StatusCode::OK,
            "The schedule, with its new settings",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        // No 409: this endpoint touches no unique column, so there is nothing
        // for it to collide with.
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /qa/v1/schedules/{id}
    OperationBuilder::delete("/qa/v1/schedules/{id}")
        .operation_id("qa_runs.delete_schedule")
        .summary("Delete a schedule")
        .description(
            "Delete a schedule and, by cascade, every tick row recording what it fired. \
             Not idempotent: a second delete answers 404. Runs the schedule already \
             produced are left alone - qa_runs.schedule_id carries no foreign key.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Schedule UUID")
        .handler(handlers::schedules::delete_schedule)
        // `.no_content_response()`, never `.json_response(NO_CONTENT, ..)`,
        // which would advertise a JSON body on a bodyless response.
        .no_content_response(StatusCode::NO_CONTENT, "Schedule deleted")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi)
}
