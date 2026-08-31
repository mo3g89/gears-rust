//! REST API route definitions - `OpenAPI` and Axum routing.
//!
//! ## Architecture
//!
//! Routes are organized by resource:
//! - `platforms` - target platform endpoints (5 CRUD + 1 read-only lease view)
//! - `variables` - variable endpoints (list, upsert, delete)
//!
//! Lease acquire/release are intentionally **not** registered here — they
//! are SDK-only operations for the qa-runs dispatcher (see
//! `routes::platforms` and `handlers::platforms::get_platform_lease` for the
//! rationale).
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

mod platforms;
mod variables;

pub(super) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// Register all routes for the `qa-environments` gear.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteAppServices>,
) -> Router {
    router = platforms::register_platform_routes(router, openapi);
    router = variables::register_variable_routes(router, openapi);

    router.layer(axum::Extension(service))
}
