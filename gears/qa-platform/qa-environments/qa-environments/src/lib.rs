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
