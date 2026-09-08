//! qa-catalog gear: test repositories, plan discovery, `TEST_META` parsing,
//! custom plans, products/versions, ephemeral test bundles, SSH key metadata.
//!
//! Part of the qa-platform subsystem (see `gears/qa-platform/docs/DESIGN.md`
//! §3.2 `cpt-cf-qa-component-catalog`).

pub mod api;
pub mod config;
pub mod gear;
pub mod gts;

pub use gear::QaCatalog;

// === `domain` and `infra` are crate-internal — review finding #38 ===
//
// Both used to be `pub mod`, which made every SeaORM entity, every repository
// trait and every service struct part of this crate's public API: a consumer
// could name `qa_catalog::infra::storage::entity::*` and pin itself to this
// gear's schema. Only `gear` (and the SDK) is the contract. The precedent for
// the shape is `gears/system/oagw/oagw/src/lib.rs:16-17`.
//
// The `pub use`s below are the exceptions the compiler named, one per item an
// integration-test crate in `tests/` genuinely needs. Those tests compile as
// separate crates, so `pub(crate)` would otherwise break them — and an
// integration test is a real consumer, not a visibility inconvenience: none
// was deleted, gated away, or moved into `src/` to shrink this list.
pub(crate) mod domain;
pub(crate) mod infra;

/// Needed by `tests/gix_sync_integration.rs`, which asserts the failure
/// variant the gix engine reports for an unreachable remote.
pub use domain::error::DomainError;
/// Needed by `tests/gix_sync_integration.rs`, which parses a plan out of a
/// worktree the engine has just checked out.
pub use domain::parsing::plan_yaml::parse_plan_yaml;
/// Needed by `tests/gix_sync_integration.rs` and `tests/multi_branch.rs`: the
/// port both drive [`GixSyncEngine`] through.
pub use domain::ports::repo_sync::RepoSyncPort;
/// Needed by `tests/multi_branch.rs`, which shares one cache across two
/// branch syncs.
pub use domain::service::SyncCache;
/// The gix sync engine under test in both integration test crates, and the
/// on-disk layout they check it wrote (`tests/gix_sync_integration.rs`,
/// `tests/multi_branch.rs`).
pub use infra::git::GixSyncEngine;
pub use infra::git::layout::{branch_workdir, host_dir};

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
