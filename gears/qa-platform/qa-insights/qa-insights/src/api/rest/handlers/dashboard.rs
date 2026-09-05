//! The dashboard aggregates. Task 18, extended by Task 19's coverage view.
//!
//! **Two statements each** — one delegation and one conversion, however rustfmt
//! wraps them — and everything that is *not* here is deliberate: no scope, no
//! clamping of `days`, no status code.
//!
//! **The clamp in particular belongs to the service and not here.** `days` is
//! clamped to `[3, 90]` by
//! [`resolve_days`](crate::domain::service::dashboard::resolve_days), because it
//! is a property of the operation rather than of the transport — the same rule
//! has to hold for any future non-HTTP caller, exactly as
//! [`super::rebuild`]'s window validation does. A handler that clamped here would
//! be a second opinion able to disagree with the one the `OpenAPI` description
//! advertises.
//!
//! The scope is derived by
//! [`DashboardService`](crate::domain::service::dashboard::DashboardService)
//! immediately before the reads, and `?` resolves the error through
//! [`crate::api::rest::error`].

use std::sync::Arc;

use axum::Extension;
use axum::extract::Query;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{CoverageBuildDto, DashboardQuery, DashboardStatsDto};
use crate::gear::ConcreteAppServices;

/// `GET /qa/v1/dashboard` — run activity and the ingested counters behind it.
///
/// `days` is optional and reaches the service unchanged; an absent one is 14 and
/// an out-of-range one is clamped rather than refused.
#[tracing::instrument(skip(svc, ctx, query), fields(dashboard.days = ?query.days))]
pub async fn dashboard(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Query(query): Query<DashboardQuery>,
) -> ApiResult<Json<DashboardStatsDto>> {
    let stats = svc.dashboard.stats(&ctx, query.days).await?;
    Ok(Json(DashboardStatsDto::from(stats)))
}

/// `GET /qa/v1/dashboard/coverage` — coverage per product build.
///
/// **No extractor beyond the two extensions**, which is legacy's shape rather
/// than a simplification: `api_coverage` takes only `State`
/// (`manager/src/routes/dashboard.rs:573`), so there is no query struct to
/// deserialize and adding one would be a new contract.
///
/// The array is empty until an upstream for the percentages exists;
/// `domain::service::dashboard::DashboardService::coverage` carries the whole
/// argument, and this handler is not the place that decides it.
#[tracing::instrument(skip(svc, ctx))]
pub async fn dashboard_coverage(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<Vec<CoverageBuildDto>>> {
    let builds = svc.dashboard.coverage(&ctx).await?;
    Ok(Json(
        builds.into_iter().map(CoverageBuildDto::from).collect(),
    ))
}
