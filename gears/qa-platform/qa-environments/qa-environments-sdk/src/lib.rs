//! SDK for the `qa-environments` gear.
//!
//! Public contract: client trait, transport-agnostic models, canonical errors.

mod client;
mod errors;
mod models;

/// The two `qa-product-sdk` types [`Environment`] carries, re-exported so a
/// consumer can name its own fields' types without taking a second dependency
/// to do it. `qa-runs` and `qa-insights` both build `Environment` fixtures and
/// neither otherwise knows the plugin contract exists.
///
/// A re-export rather than a local twin: `ObservedAttrs` is the exact type
/// `project_roles` and `retain_declared` take, and a parallel map would need
/// converting at every boundary.
pub use qa_product_sdk::observation::{HealthState, ObservedAttrs};

pub use client::QaEnvironmentsClientV1;
pub use errors::QaEnvironmentsError;
pub use models::{
    AcquireOutcome, ClusterHealthView, ClusterStatus, CredentialMaterial, CredentialSubmission,
    Environment, EnvironmentCredential, EnvironmentPatch, LeaseMode, LeaseState, NewEnvironment,
    NewVariable, NodeCounts, NodeSummary, RESERVED_VARIABLE_NAMES, Variable,
};
