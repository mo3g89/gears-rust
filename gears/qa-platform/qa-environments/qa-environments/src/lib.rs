//! qa-environments gear: target environment registry, variables, and lease state.
//!
//! Part of the qa-platform subsystem (see `gears/qa-platform/docs/DESIGN.md`
//! §3.2 `cpt-cf-qa-component-environments`).

pub mod api;
pub mod config;
pub mod domain;
pub mod gear;
pub mod infra;

pub use gear::QaEnvironments;

#[cfg(test)]
mod containment_tests;
#[cfg(test)]
mod test_support;

/// **No `domain` module imports the `api` layer.** `api` is a transport over
/// `domain`, and the dependency may not run the other way. A structural guard
/// rather than a `cargo gears lint` rule -- see the module's own header for
/// why that CLI cannot express this one. Review findings #15, #16, #39.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "no_api_in_domain_tests.rs"]
mod no_api_in_domain_tests;
