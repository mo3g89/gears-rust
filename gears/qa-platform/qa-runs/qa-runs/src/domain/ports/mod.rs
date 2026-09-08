//! Domain ports — interfaces the domain owns and infrastructure satisfies.
//!
//! A port lives here, not in `infra`, because the domain defines what it needs
//! and an adapter answers it; the dependency arrow points inward. The adapter
//! itself lives under [`crate::infra`] (`infra::executor::mock` today, the
//! serverless-runtime adapter with feature 2.7).

/// Typed metric-emission ports — what this gear measures, satisfied by an
/// OpenTelemetry-backed adapter under [`crate::infra`]. The families they emit
/// into are declared in [`crate::domain::metrics`].
pub mod metrics;
/// How a run reaches its target environment — the product-plugin resolver,
/// satisfied by [`crate::infra::product_plugin`].
pub mod product_plugin;
pub mod run_executor;
