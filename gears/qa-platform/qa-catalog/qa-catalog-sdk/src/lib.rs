//! SDK for the `qa-catalog` gear.
//!
//! Public contract: client trait, transport-agnostic models, canonical errors.

mod client;
mod errors;
mod models;
mod plugin_resolver;

pub use client::QaCatalogClientV1;
pub use errors::QaCatalogError;
pub use models::{
    BundleRequest, CustomPlan, CustomPlanEntry, Exclusivity, NewCustomPlan, NewCustomPlanEntry,
    NewProduct, NewTestRepository, Plan, Product, ProductUpdate, SOURCE_REPO, SshKey, SyncRequest,
    TestBundle, TestFileMeta, TestRepository, TestRepositoryUpdate, UniverseTest,
};
pub use plugin_resolver::QaProductPluginResolverV1;
