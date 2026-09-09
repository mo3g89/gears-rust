//! Handlers for `GET/POST /qa/v1/analytics/views` and
//! `PUT/DELETE /qa/v1/analytics/views/{id}` — legacy's four saved-view
//! endpoints (`manager/src/routes/analytics.rs:521-726`).
//!
//! # Every service call goes through [`as_saved_view_error`]
//!
//! Not a bare `?`. [`as_saved_view_error`]'s own doc records why: the blanket
//! `From<DomainError> for CanonicalError` in `domain::error` has no
//! per-call-site information, so
//! it attributes every `Validation` and every `Forbidden` to the test-result
//! resource — right for the analytics and dashboard handlers, wrong here.
//! `domain::service::saved_views` raises `Validation` on `scope`, `name` and
//! `plan_path`, none of which is a test-result field, and its PDP denials are
//! about `qa.saved_view`. [`as_saved_view_error`] re-attributes both before
//! the blanket mapping ever sees them; `SavedViewNameExists` and
//! `SavedViewNotFound` already carry the right resource and pass through it
//! untouched.
//!
//! # No scope, no clamping, no status code
//!
//! Same discipline `handlers::analytics`' header states: every parameter
//! reaches [`crate::domain::service::saved_views::SavedViewsService`]
//! unvalidated, and the service owns the authorization decision, the field
//! rules and the 404/409 folding. Each handler here is a decode, a call and a
//! render.

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::http::Uri;
use axum::response::IntoResponse;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::dto::{NewSavedViewReq, SavedViewDto, SavedViewsListQuery};
use crate::api::rest::error::as_saved_view_error;
use crate::domain::service::saved_views::SavedViewInput;
use crate::gear::ConcreteAppServices;

/// `GET /qa/v1/analytics/views` — the caller's views at one scope.
///
/// `api_list_views` (`manager/src/routes/analytics.rs:521-574`).
///
/// # `query` is skipped, for `create_saved_view`'s reason
///
/// An earlier revision recorded `saved_view.scope = %query.scope` on this
/// span. `scope` reaches [`SavedViewsService::list`] **before** its private
/// `parse_scope` helper validates it — `#[tracing::instrument]` attaches a
/// field on entry, ahead of any call this function makes — so an attacker
/// sending an arbitrarily long `?scope=` value would have had it recorded
/// verbatim regardless of the 400 the request goes on to receive. That is the
/// same caller-controlled-text-with-no-bound reasoning
/// [`create_saved_view`]'s own doc gives for skipping `uri` and `req`;
/// applied here by skipping `query` entirely rather than recording one of its
/// fields unvalidated.
///
/// [`SavedViewsService::list`]: crate::domain::service::saved_views::SavedViewsService::list
#[tracing::instrument(skip(svc, ctx, query))]
pub async fn list_saved_views(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<SavedViewsListQuery>,
) -> ApiResult<Json<Vec<SavedViewDto>>> {
    let views = svc
        .saved_views
        .list(
            &ctx,
            &query.scope,
            query.repo_id,
            query.plan_path.as_deref(),
        )
        .await
        .map_err(as_saved_view_error)?;
    let dtos = views
        .into_iter()
        .map(SavedViewDto::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(as_saved_view_error)?;
    Ok(Json(dtos))
}

/// `POST /qa/v1/analytics/views` — store a new view owned by the caller.
///
/// `api_create_view` (`manager/src/routes/analytics.rs:576-639`). 201 with the
/// stored view and a `Location` header, matching every other create in this
/// workspace's REST tier.
///
/// # Nothing caller-supplied reaches the span
///
/// `uri` and `req` are both skipped, for `qa-runs`'
/// `create_schedule`'s reason: the full request target and an unvalidated
/// body are both caller-controlled text with no bound this handler enforces,
/// and `created_json` reads `uri.path()` rather than anything the span would
/// need.
#[tracing::instrument(skip(svc, ctx, req, uri))]
pub async fn create_saved_view(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<NewSavedViewReq>,
) -> ApiResult<impl IntoResponse> {
    let input: SavedViewInput = req.into();
    let view = svc
        .saved_views
        .create(&ctx, input)
        .await
        .map_err(as_saved_view_error)?;
    let dto = SavedViewDto::try_from(view).map_err(as_saved_view_error)?;
    let id = dto.id.to_string();
    Ok(created_json(dto, &uri, &id).into_response())
}

/// `PUT /qa/v1/analytics/views/{id}` — replace a view's scope, plan, name and
/// query in full.
///
/// `api_update_view` (`manager/src/routes/analytics.rs:641-701`). Not a
/// patch: every caller-decidable field comes from the body, matching legacy's
/// own full-replace shape.
#[tracing::instrument(skip(svc, ctx, req), fields(saved_view.id = %id))]
pub async fn update_saved_view(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Json(req): Json<NewSavedViewReq>,
) -> ApiResult<Json<SavedViewDto>> {
    let input: SavedViewInput = req.into();
    let view = svc
        .saved_views
        .update(&ctx, id, input)
        .await
        .map_err(as_saved_view_error)?;
    Ok(Json(
        SavedViewDto::try_from(view).map_err(as_saved_view_error)?,
    ))
}

/// `DELETE /qa/v1/analytics/views/{id}` — delete a view.
///
/// `api_delete_view` (`manager/src/routes/analytics.rs:703-726`). Not
/// idempotent, matching legacy: a second delete of the same id is a 404.
#[tracing::instrument(skip(svc, ctx), fields(saved_view.id = %id))]
pub async fn delete_saved_view(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.saved_views
        .delete(&ctx, id)
        .await
        .map_err(as_saved_view_error)?;
    Ok(no_content().into_response())
}

#[cfg(test)]
#[path = "saved_views_handler_tests.rs"]
mod saved_views_handler_tests;
