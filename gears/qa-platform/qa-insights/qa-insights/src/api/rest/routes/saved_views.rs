//! The saved-view routes — `GET/POST /qa/v1/analytics/views` and
//! `PUT/DELETE /qa/v1/analytics/views/{id}`. Task 28.
//!
//! Two paths, four operations — `register_saved_view_routes` registers all
//! four in one function rather than splitting per-operation the way
//! `routes::analytics` does for the overview family: none of these four
//! descriptions is long enough on its own to trip `clippy::too_many_lines`,
//! and splitting them would be four functions for four bodies shorter than
//! `routes::analytics::register_overview`'s query-parameter block alone.
//! `routes::dashboard`'s two-operations-in-one-function precedent is the
//! closer analogue.
//!
//! # `plan_id` is two query parameters here, not one
//!
//! `### What Task 27 changed about plan identity on the wire` (the plan's own
//! section, read as this task's required Step 0 reading) moved the three plan
//! drill-downs' `plan_id` off a path segment and onto a query parameter
//! because a real `plan_path` contains `/`. That finding is about the path
//! shape and applies here too: `plan_path` is never a path or single-segment
//! value on any route this file registers. But **saved views are not those
//! three endpoints**, and the same finding does not hand this file `plan_id`
//! as a single string — `domain::service::saved_views`' header explains why a
//! saved view's plan identity is `(repo_id, plan_path)`, two query parameters
//! on `GET` and two body fields on `POST`/`PUT`, never legacy's single opaque
//! `plan_id`. The `id` path parameter on the two `{id}` routes below is a
//! saved view's own primary key and is never a plan identity.

use http::StatusCode;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::{API_TAG, License};
use crate::api::rest::{dto, handlers};

pub(super) fn register_saved_view_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /qa/v1/analytics/views
    router = OperationBuilder::get("/qa/v1/analytics/views")
        .operation_id("qa_insights.list_saved_views")
        .summary("List saved analytics views")
        .description(
            "The caller's own saved analytics filter sets at one scope. scope is required and \
             is all or plan, case-insensitively; scope=plan additionally requires repo_id and \
             plan_path together, naming the plan the list is scoped to. plan_path is the same \
             value GET /qa/v1/analytics/overview and its five siblings take as their plan_id \
             query parameter; repo_id additionally disambiguates it across repositories, which \
             those endpoints do not need to and this one does. A view is visible only \
             to the caller who created it - two callers may hold a view of the same name at the \
             same scope, and neither can see the other's. Ordered by most-recently-updated \
             first. Requires the gts.cf.qa.insights.saved_view.v1~/list grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed(
            "scope",
            true,
            "all or plan, case-insensitively. Required.",
            "string",
        )
        .query_param_typed(
            "repo_id",
            false,
            "The plan's repository. Required together with plan_path when scope=plan.",
            "string",
        )
        .query_param_typed(
            "plan_path",
            false,
            "The plan's path within its repository. Required together with repo_id when \
             scope=plan.",
            "string",
        )
        .handler(handlers::list_saved_views)
        .json_array_response_with_schema::<dto::SavedViewDto>(
            openapi,
            StatusCode::OK,
            "The caller's saved views at the requested scope, newest-updated first",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /qa/v1/analytics/views
    router = OperationBuilder::post("/qa/v1/analytics/views")
        .operation_id("qa_insights.create_saved_view")
        .summary("Save a new analytics view")
        .description(
            "Store a new analytics filter set owned by the caller. name is required and must \
             be non-blank after trimming. scope is required and is all or plan; scope=plan \
             additionally requires repo_id and plan_path together. plan_path is the same value \
             the analytics endpoints (GET /qa/v1/analytics/overview and its siblings) take as \
             plan_id; repo_id additionally disambiguates it across repositories. Note that \
             query_json is opaque and may itself carry a plan_id belonging to that separate \
             vocabulary - the two are not reconciled by this gear. A plan supplied alongside \
             scope=all is accepted but not stored - it is a no-op on the plan half, not a \
             rejection, matching the system being replaced's own permissiveness on this \
             combination. query_json is an opaque JSON document this gear never inspects. \
             409 means a view of this name already exists at this owner, scope and plan. \
             Requires the gts.cf.qa.insights.saved_view.v1~/create grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::NewSavedViewReq>(openapi, "The view to create")
        .handler(handlers::create_saved_view)
        .json_response_with_schema::<dto::SavedViewDto>(
            openapi,
            StatusCode::CREATED,
            "The stored view",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        // A duplicate (owner, scope, plan, name) is a 409, mapped from the
        // repository's own unique-constraint catch — never a 500 and never a
        // pre-check race. `domain::service::saved_views`'s header carries the
        // concurrency argument in full.
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /qa/v1/analytics/views/{id}
    router = OperationBuilder::put("/qa/v1/analytics/views/{id}")
        .operation_id("qa_insights.update_saved_view")
        .summary("Replace a saved analytics view")
        .description(
            "A full replace of a saved view's scope, plan, name and query, taking the same \
             body as the create. plan_path is the same value the analytics endpoints (GET \
             /qa/v1/analytics/overview and its siblings) take as plan_id; repo_id additionally \
             disambiguates it across repositories. Not a patch: every caller-decidable field is \
             taken from the body. 404 covers both an id nobody owns and an id owned by a caller \
             other than the caller making the request - the two are deliberately \
             indistinguishable, so this endpoint cannot be used to learn whether an id exists \
             for someone else. 409 means the new name collides with another view this caller \
             already holds at the new scope and plan. Requires the gts.cf.qa.insights.saved_view.v1~/update grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Saved view UUID")
        .json_request::<dto::NewSavedViewReq>(openapi, "The view's new content, in full")
        .handler(handlers::update_saved_view)
        .json_response_with_schema::<dto::SavedViewDto>(
            openapi,
            StatusCode::OK,
            "The replaced view",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /qa/v1/analytics/views/{id}
    OperationBuilder::delete("/qa/v1/analytics/views/{id}")
        .operation_id("qa_insights.delete_saved_view")
        .summary("Delete a saved analytics view")
        .description(
            "Delete a saved view by its own id - not by the (repo_id, plan_path) pair a \
             plan-scoped view is keyed on; that pair identifies the plan the view is about, the \
             same plan_path/plan_id an analytics endpoint would take, and is unrelated to which \
             view is deleted here. Not idempotent: a second delete of the same id answers 404. \
             404 covers both an id nobody owns and one owned by another caller, for the same \
             reason the replace endpoint's 404 does. Requires the gts.cf.qa.insights.saved_view.v1~/delete grant.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", "Saved view UUID")
        .handler(handlers::delete_saved_view)
        .no_content_response(StatusCode::NO_CONTENT, "View deleted")
        // `id` is a UUID path parameter; a non-UUID segment is a 400 from
        // axum's own `Path<Uuid>` rejection before this handler ever runs —
        // declared so the schema does not under-advertise it.
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi)
}
