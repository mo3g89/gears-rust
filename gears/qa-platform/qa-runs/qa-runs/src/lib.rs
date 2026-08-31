//! qa-runs gear: launch validation, exclusivity resolution, per-platform FIFO
//! queue, dispatcher with crash recovery, run state machine, environment
//! assembly, executor invocation, event ingestion, cancellation/re-run,
//! timeout enforcement, SSE logs, and cron schedules.
//!
//! Part of the qa-platform subsystem (see `gears/qa-platform/docs/DESIGN.md`
//! §3.2 `cpt-cf-qa-component-runs`).

pub mod api;
pub mod config;
pub mod domain;
pub mod gear;
pub mod infra;

pub use gear::QaRuns;

/// Every identifier this crate's prose cites must exist. Crate-wide rather than
/// per-module, because the defect it guards has landed in several unrelated
/// files, including this crate's own guard against it.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "doc_citations_tests.rs"]
mod doc_citations_tests;

// No crate-level `test_support`: the plan's file list named one, and the two
// harnesses this crate needs already exist closer to what they serve -
// `domain::service::test_support` for the service doubles and
// `infra::storage::test_db` for the migrated in-memory database. A third,
// crate-level module would have been a re-export of those two.
