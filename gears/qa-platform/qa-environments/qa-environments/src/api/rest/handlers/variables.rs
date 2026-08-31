use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{ListVariablesQuery, UpsertVariableReq, VariableDto};
use crate::gear::ConcreteAppServices;

/// List global pipeline variables, plus a platform's variables when
/// `platform_id` is given.
#[tracing::instrument(skip(svc, ctx))]
pub async fn list_variables(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<ListVariablesQuery>,
) -> ApiResult<Json<Vec<VariableDto>>> {
    let vars = svc.variables.list_for_env(&ctx, query.platform_id).await?;
    Ok(Json(vars.into_iter().map(VariableDto::from).collect()))
}

/// Insert or update a variable by natural key (`platform_id` + `name`, or
/// `name` alone for a pipeline variable).
#[tracing::instrument(skip(svc, ctx, req), fields(variable.name = %req.name))]
pub async fn upsert_variable(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<UpsertVariableReq>,
) -> ApiResult<Json<VariableDto>> {
    let var = svc.variables.upsert(&ctx, req.into()).await?;
    Ok(Json(VariableDto::from(var)))
}

/// Delete a variable by ID.
#[tracing::instrument(skip(svc, ctx), fields(variable.id = %id))]
pub async fn delete_variable(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.variables.delete(&ctx, id).await?;
    Ok(no_content().into_response())
}
