//! The product-plugin catalogue endpoint.
//!
//! Thin delegation, like every handler here: the enumeration itself lives in
//! `QaProductRegistry::list_registered_plugins`, which is the one place in
//! this gear that knows GTS exists.

use std::sync::Arc;

use axum::Extension;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::ProductPluginDto;
use crate::gear::ConcreteAppServices;

/// List the product plugins this deployment has registered, with both of
/// each plugin's declared schemas.
///
/// An empty list is a 200 with `[]`, never a 404: "this deployment registers
/// no product plugins" is a legitimate answer about a real, reachable
/// collection. A types-registry that cannot be reached is a 500 instead —
/// see the service method for why that is not flattened into `[]`.
#[tracing::instrument(skip(svc, ctx))]
pub async fn list_product_plugins(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<Vec<ProductPluginDto>>> {
    let plugins = svc.plugin_registry.list_registered_plugins(&ctx).await?;
    Ok(Json(
        plugins.into_iter().map(ProductPluginDto::from).collect(),
    ))
}
