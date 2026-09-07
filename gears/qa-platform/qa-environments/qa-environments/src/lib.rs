//! qa-environments gear: target environment registry, variables, and lease state.
//!
//! Part of the qa-platform subsystem (see `gears/qa-platform/docs/DESIGN.md`
//! §3.2 `cpt-cf-qa-component-environments`).

pub mod api;
pub mod config;
pub mod gear;
pub mod gts;

pub use gear::QaEnvironments;

// === `domain` and `infra` are crate-internal — review finding #38 ===
//
// Both used to be `pub mod`, which made every SeaORM entity, every repository
// trait and every service struct part of this crate's public API: a consumer
// could name `qa_environments::infra::storage::entity::*` and pin itself to this
// gear's schema. Only `gear` (and the SDK) is the contract. The precedent for
// the shape is `gears/system/oagw/oagw/src/lib.rs:16-17`.
//
// The `pub use`s below are the exceptions the compiler named, one per item an
// integration-test crate in `tests/` genuinely needs. Those tests compile as
// separate crates, so `pub(crate)` would otherwise break them — and an
// integration test is a real consumer, not a visibility inconvenience: none
// was deleted, gated away, or moved into `src/` to shrink this list.
//
// This gear needs no exceptions: `tests/` holds only fixtures, no integration
// test crate, so nothing outside names either module.
pub(crate) mod domain;
pub(crate) mod infra;

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
