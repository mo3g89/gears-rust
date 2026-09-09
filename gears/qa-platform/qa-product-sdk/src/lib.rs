//! The contract every QA Platform product plugin implements.
//!
//! See `gears/qa-platform/docs/features/product-plugins.md` for why this exists
//! and what each decision cost.

pub mod access;
pub mod descriptor;
pub mod gts;
pub mod observation;
pub mod plugin;
/// The leak-conformance harness, behind the `test-util` feature so a
/// production plugin build does not compile it — only a plugin crate's own
/// test suite (a `[dev-dependencies]` consumer) needs it. The feature gates
/// this module's code, not the dependency tree: `tracing-subscriber` reaches
/// every gear in this workspace transitively through `cf-gears-toolkit`
/// anyway (see `Cargo.toml`).
#[cfg(feature = "test-util")]
pub mod testing;

pub use access::{MountSpec, RunAccess, RunVar, RunVarContract, RunnerSpec};
pub use descriptor::{FieldDesc, FieldKind, FieldRole, SchemaError, validate_schemas};
pub use gts::QaProductPluginSpecV1;
pub use observation::{
    FailureClass, HealthOutcome, HealthState, ObservationOutcome, ObservedAttrs, PluginFailure,
    PluginObservation, RoleProjection, project_roles, retain_declared,
};
pub use plugin::{
    CredentialClassification, CredentialInput, CredentialSlot, EnvironmentHandle,
    QaProductPluginV1, RegisteredPlugin,
};
#[cfg(feature = "test-util")]
pub use testing::{Canary, assert_no_leak};
