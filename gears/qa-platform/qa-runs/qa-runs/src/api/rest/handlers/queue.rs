//! Handlers for `/qa/v1/queue`.

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::response::IntoResponse;
use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::dto::{QueueEntryDto, QueueQuery, StartedRunDto};
use crate::api::rest::error::as_queue_error;
use crate::gear::ConcreteAppServices;

// Every handler here wraps its service call in [`as_queue_error`] rather than
// using a bare `?`. The reason is one this module cannot see on its own: the
// `DomainError` -> `CanonicalError` mapping is exhaustive over *variants* and
// says nothing about *endpoints*, and three of the variants these three
// handlers can raise - `Forbidden`, `ConcurrencyLimit`, `RunNotFound` - name no
// resource, so the mapping attributes them to `cf.qa.runs.run.v1~`. These
// endpoints resolve `resources::QUEUE_ENTRY` scopes, so a caller refused here
// was being pointed at run permissions and, on the delete, at a run uuid the
// request never named. `handler_tests` below drives all three and reads the
// `gts_id` out of the rendered body, which is the only check there is: nothing
// in the type system pairs an endpoint with a resource type.

/// `GET /qa/v1/queue`
///
/// # Two ways to narrow, and they are not redundant
///
/// `environment_id` is a plain query parameter as well as being filterable via
/// `OData` (as `$filter=environment_id eq ...`, see
/// [`QueueFilterField::EnvironmentId`](crate::infra::storage::odata::QueueFilterField::EnvironmentId)
/// — the two share a name as of ruling G-3, but remain two mechanisms; see
/// [`QueueQuery::environment_id`]). The frozen guide's remedy for a
/// `queue_position` distorted by a narrow window is *the environment-filtered
/// call*, and a remedy that requires knowing `OData` syntax is not much of a
/// remedy. The parameter is the one an operator reaches for; the `OData`
/// field serves everything else.
///
/// The legacy `limit` parameter is accepted and clamped for the same reason -
/// the guide specifies it, and callers ported from the source system send it -
/// but `$top` is what a new caller should use. See [`QueueQuery`].
#[tracing::instrument(skip(svc, ctx, odata, params))]
pub async fn list_queue(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(params): Query<QueueQuery>,
    OData(odata): OData,
) -> ApiResult<JsonPage<QueueEntryDto>> {
    params
        .reject_legacy_field()
        .map_err(|e| as_queue_error(None, e))?;
    let page = svc
        .runs
        .queue_page(&ctx, params.environment_id, &params.merge_into(odata))
        .await
        .map_err(|e| as_queue_error(None, e))?;
    Ok(Json(page.map_items(QueueEntryDto::from)))
}

/// `DELETE /qa/v1/queue/{id}`
///
/// Cancels a **queued** row and retires its run in one transaction. A row that
/// has left `queued` answers 409 (`QueueRowNotQueued` maps to `aborted`), which
/// is the honest answer: the row now holds a claim on its environment and
/// dropping it here would release an environment a live execution still owns.
#[tracing::instrument(skip(svc, ctx), fields(queue.id = %id))]
pub async fn cancel_queued(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.runs
        .cancel_queued(&ctx, id)
        .await
        .map_err(|e| as_queue_error(Some(id), e))?;
    Ok(no_content().into_response())
}

/// `POST /qa/v1/queue/{id}/force-start`
///
/// The deliberate asymmetry: environment occupancy is bypassed, the
/// cluster-wide `max_concurrent_runs` cap is **not**. The service logs the
/// bypass at WARN with the environment and the run named, because an
/// operator overriding exclusivity is exactly the event an incident review
/// needs to find.
///
/// Answers 200 with the run that was started rather than 204: the caller
/// force-started something and the id of what now holds the environment is
/// the useful part of the answer.
#[tracing::instrument(skip(svc, ctx), fields(queue.id = %id))]
pub async fn force_start(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<StartedRunDto>> {
    let run_id = svc
        .runs
        .force_start(&ctx, id)
        .await
        .map_err(|e| as_queue_error(Some(id), e))?;
    Ok(Json(StartedRunDto { run_id }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "queue_handler_tests.rs"]
mod handler_tests;
