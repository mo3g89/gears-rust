//! SDK for the `qa-environments` gear.
//!
//! Public contract: client trait, transport-agnostic models, canonical errors.

mod client;
mod errors;
mod models;

pub use client::QaEnvironmentsClientV1;
pub use errors::QaEnvironmentsError;
pub use models::{
    AcquireOutcome, ClusterHealthView, KubeconfigMaterial, LeaseMode, LeaseState, NewPlatform,
    NewVariable, NodeCounts, NodeSummary, PlatformPatch, RESERVED_VARIABLE_NAMES, TargetPlatform,
    Variable,
};
