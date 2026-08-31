//! qa-environments gear: target platform registry, variables, and lease state.
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
mod test_support;
