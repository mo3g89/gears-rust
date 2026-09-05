use std::sync::Arc;

use axum::Extension;
use axum::extract::Query;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{ListPlansQuery, PlanDto};
use crate::gear::ConcreteAppServices;

/// Discover plans in the synced working copy of `(repo_id, branch)`.
///
/// `get_plan` and `get_test_meta` are deliberately **not** exposed over
/// REST — they are SDK-only operations for the qa-runs dispatcher (plan
/// resolution and `TEST_META` exclusivity aggregation at launch); see
/// `qa_catalog_sdk::QaCatalogClientV1`.
#[tracing::instrument(skip(svc, ctx, query), fields(repo.id = %query.repo_id, branch = %query.branch))]
pub async fn list_plans(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<ListPlansQuery>,
) -> ApiResult<Json<Vec<PlanDto>>> {
    let plans = svc
        .plans
        .list_plans(&ctx, query.repo_id, &query.branch)
        .await?;
    Ok(Json(plans.into_iter().map(PlanDto::from).collect()))
}
