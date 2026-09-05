use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::http::Uri;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{CustomPlanDto, UpsertCustomPlanReq};
use crate::gear::ConcreteAppServices;

/// List all custom plans visible to the caller.
#[tracing::instrument(skip(svc, ctx))]
pub async fn list_custom_plans(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<Vec<CustomPlanDto>>> {
    let plans = svc.custom_plans.list_custom_plans(&ctx).await?;
    Ok(Json(plans.into_iter().map(CustomPlanDto::from).collect()))
}

/// Get a single custom plan by ID.
#[tracing::instrument(skip(svc, ctx), fields(custom_plan.id = %id))]
pub async fn get_custom_plan(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CustomPlanDto>> {
    Ok(Json(
        svc.custom_plans.get_custom_plan(&ctx, id).await?.into(),
    ))
}

/// Create a custom plan.
#[tracing::instrument(skip(svc, ctx, req), fields(custom_plan.name = %req.name))]
pub async fn create_custom_plan(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<UpsertCustomPlanReq>,
) -> ApiResult<impl IntoResponse> {
    let plan = svc
        .custom_plans
        .create_custom_plan(&ctx, req.try_into()?)
        .await?;
    let id_str = plan.id.to_string();
    Ok(created_json(CustomPlanDto::from(plan), &uri, &id_str).into_response())
}

/// Replace a custom plan wholesale (the SDK contract is full-document
/// update, hence `PUT` — no tri-state patch semantics).
#[tracing::instrument(skip(svc, ctx, req), fields(custom_plan.id = %id))]
pub async fn update_custom_plan(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpsertCustomPlanReq>,
) -> ApiResult<Json<CustomPlanDto>> {
    let plan = svc
        .custom_plans
        .update_custom_plan(&ctx, id, req.try_into()?)
        .await?;
    Ok(Json(CustomPlanDto::from(plan)))
}

/// Delete a custom plan by ID.
#[tracing::instrument(skip(svc, ctx), fields(custom_plan.id = %id))]
pub async fn delete_custom_plan(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.custom_plans.delete_custom_plan(&ctx, id).await?;
    Ok(no_content().into_response())
}
