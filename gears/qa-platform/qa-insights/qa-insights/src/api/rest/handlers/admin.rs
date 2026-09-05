//! Operator endpoints. One, at Task 16: the rebuild.

use std::sync::Arc;

use axum::Extension;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{RebuildOutcomeDto, RebuildReq};
use crate::gear::ConcreteAppServices;

/// `POST /qa/v1/insights/rebuild` — replay a closed window from qa-runs.
///
/// Four lines, and each of the things it does *not* do is deliberate:
///
/// * It does not validate the window. `ReconcileService::rebuild` refuses a
///   window that is not strictly forward, because that refusal is a property of
///   the operation rather than of the transport — the same rule has to hold for
///   any future non-HTTP caller.
/// * It does not resolve the tenant. The service takes it from the context,
///   through `TenantBound`, which is the type that refuses the platform-root
///   sentinel.
/// * It does not authorize. The service compiles its own scope from the PEP, as
///   every service in every sibling gear does.
/// * It does not choose a status code. `?` resolves through
///   [`crate::api::rest::error`].
#[tracing::instrument(skip(svc, ctx, req), fields(rebuild.from = %req.from, rebuild.to = %req.to))]
pub async fn rebuild(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    Json(req): Json<RebuildReq>,
) -> ApiResult<Json<RebuildOutcomeDto>> {
    let outcome = svc.reconcile.rebuild(&ctx, req.from, req.to).await?;
    Ok(Json(RebuildOutcomeDto::from(outcome)))
}
