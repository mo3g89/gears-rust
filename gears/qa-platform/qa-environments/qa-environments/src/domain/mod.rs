pub mod error;
pub mod lease;
pub mod local_client;
pub mod observation;
pub mod ports;
pub mod repos;
pub mod service;
/// The observation ticker's system-actor identity. Compiled only where it can
/// be reached: its one production caller (`PlatformsService::run_observation_cycle`)
/// exists only in a build carrying `platform-observation`, and `cfg(test)`
/// keeps it — and its tests — in a default `cargo test` run, so the ADR-0001
/// property that this gear's domain is testable with no Kubernetes in the tree
/// still covers it.
#[cfg(any(feature = "platform-observation", test))]
pub(crate) mod system_actor;
