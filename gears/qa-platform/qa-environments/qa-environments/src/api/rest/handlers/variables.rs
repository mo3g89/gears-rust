use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{ListVariablesQuery, UpsertVariableReq, VariableDto};
use crate::gear::ConcreteAppServices;

/// `GET /qa/v1/variables`
///
/// One page of the global pipeline variables, plus an environment's variables
/// when `environment_id` is given.
///
/// Two extractors, and they are not redundant. `environment_id` stays a plain
/// query parameter because it carries an **authorization precheck** — the
/// service resolves a PLATFORM/`GET` scope for it and answers 404 for an
/// environment the caller cannot see — which an `OData` `$filter` on the same
/// concept would skip. `OData` covers the general case (`$filter`, `$orderby`,
/// `$top`, cursor). See `VariableFilterField`'s doc for the full argument, and
/// `VariablesService::list_for_env` for why the `environment_id` case is
/// bounded rather than cursored.
///
/// As on `list_environments`, the service resolves the caller's `AccessScope`
/// before the filter is applied, so a `$filter` cannot widen the result.
#[tracing::instrument(skip(svc, ctx, odata))]
pub async fn list_variables(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<ListVariablesQuery>,
    OData(odata): OData,
) -> ApiResult<JsonPage<VariableDto>> {
    let page = svc
        .variables
        .list_for_env(&ctx, query.environment_id, &odata)
        .await?;
    Ok(Json(page.map_items(VariableDto::from)))
}

/// Insert or update a variable by natural key (`environment_id` + `name`, or
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
