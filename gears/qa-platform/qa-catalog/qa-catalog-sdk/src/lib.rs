//! SDK for the `qa-catalog` gear.
//!
//! Public contract: client trait, transport-agnostic models, canonical errors.

mod client;
mod errors;
mod models;

pub use client::QaCatalogClientV1;
pub use errors::QaCatalogError;
pub use models::{
    BundleRequest, CustomPlan, CustomPlanEntry, ExclusiveFlag, NewCustomPlan, NewCustomPlanEntry,
    NewTestRepository, Plan, Product, SOURCE_REPO, SshKey, SyncRequest, TestBundle, TestFileMeta,
    TestRepository, TestRepositoryUpdate, UniverseTest,
};
