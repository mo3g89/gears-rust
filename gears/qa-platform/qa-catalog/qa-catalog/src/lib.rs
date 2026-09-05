//! qa-catalog gear: test repositories, plan discovery, `TEST_META` parsing,
//! custom plans, products/versions, ephemeral test bundles, SSH key metadata.
//!
//! Part of the qa-platform subsystem (see `gears/qa-platform/docs/DESIGN.md`
//! §3.2 `cpt-cf-qa-component-catalog`).

pub mod api;
pub mod config;
pub mod domain;
pub mod gear;
pub mod infra;

pub use gear::QaCatalog;

#[cfg(test)]
mod test_support;
