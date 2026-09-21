//! REST API route definitions - `OpenAPI` and Axum routing.
//!
//! ## Architecture
//!
//! Routes are organized by resource:
//! - `test_repos` - repository CRUD, sync-now, cached-branch reads
//! - `plans` - discovered-plan listing
//! - `custom_plans` - custom plan CRUD
//! - `products` - products, versions, distinct folders
//! - `product_plugins` - the registered product-plugin catalogue (read-only)
//! - `ssh_keys` - SSH key metadata (material lives in credstore only)
//! - `bundles` - bundle download
//!
//! `get_plan`, `get_test_meta`, and `create_bundle` are intentionally **not**
//! registered here — they are SDK-only operations for the qa-runs dispatcher
//! (see `qa_catalog_sdk::QaCatalogClientV1` and the handler-module docs).
//!
//! ## Layering
//!
//! Routes orchestrate but don't contain business logic: they delegate to
//! `handlers::*`, which in turn call `domain::service::AppServices`.

use std::sync::Arc;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};

use crate::gear::ConcreteAppServices;

mod bundles;
mod custom_plans;
mod plans;
mod product_plugins;
mod products;
mod ssh_keys;
mod test_repos;

pub(super) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// Register all routes for the `qa-catalog` gear.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteAppServices>,
) -> Router {
    register_operations(router, openapi).layer(axum::Extension(service))
}

/// The route definitions alone, with nothing bound to them.
///
/// Split out of [`register_routes`] so registration can be driven by a caller
/// that has no `AppServices`: `routes_tests`, which has no database, and the
/// `qa-platform-openapi` generator, which renders `docs/openapi.json` from
/// this crate instead of from a running gateway. Public for the second of
/// those — it is the only entry point outside this crate, and it binds
/// nothing, so it cannot be mistaken for a way to mount the gear.
///
/// Same shape as qa-runs' and qa-insights' `register_operations`.
pub fn register_operations(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = test_repos::register_test_repo_routes(router, openapi);
    router = plans::register_plan_routes(router, openapi);
    router = custom_plans::register_custom_plan_routes(router, openapi);
    router = products::register_product_routes(router, openapi);
    router = product_plugins::register_product_plugin_routes(router, openapi);
    router = ssh_keys::register_ssh_key_routes(router, openapi);
    bundles::register_bundle_routes(router, openapi)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "routes_tests.rs"]
mod tests;
