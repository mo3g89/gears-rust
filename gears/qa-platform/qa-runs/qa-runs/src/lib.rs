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
pub mod gts;
pub mod infra;

pub use gear::QaRuns;

// === `domain` and `infra` stay `pub` here — review finding #38, measured ===
//
// Finding #38 asks for `pub(crate) mod domain` / `pub(crate) mod infra` in all
// four gears, so SeaORM entities and repository traits stop being part of the
// crate's public API. It landed that way in `qa-catalog` and
// `qa-environments`. **It does not land here as a visibility-only change**,
// which is what the finding is scoped to.
//
// Measured rather than guessed: the change was made, the compiler run, and
// then reverted. With both modules `pub(crate)`, this crate reports 16 groups
// of newly-dead code — items nothing outside its own `#[cfg(test)]` modules
// reaches, which `pub mod` was keeping the compiler quiet about.
//
// They are scattered rather than one subsystem: `domain::cron`'s
// skip-reporting helpers, `domain::repos::log_line`'s archive-side constants,
// `domain::state_machine`'s terminal-state helpers, four
// `infra::logs::broadcast` methods, and several unused re-exports in
// `domain::repos` and `infra::logs`.
//
// None of that is a visibility question. Each item is a decision — delete it
// and the tests that cover it, or wire the feature.
//
// **Two ways of not making that decision were weighed and rejected.** The
// blunt one is an `#[allow(dead_code)]` over a whole subsystem: it trades a
// real signal for a green build, and it goes on hiding the next dead thing to
// land there. The sharp one is per-item `#[expect(dead_code, reason = "…")]`,
// which is already how this repo records a deliberately-unused item
// (`infra::storage::entity::mod`, `api::rest::dto`) and which keeps the
// signal, because `expect` starts warning the moment the item stops being
// dead. It was rejected here for one reason only: it is not a way to *defer*
// the decision. A `reason` written on each of these items records an
// adjudication nobody has made, and reads to the next reader as though one
// had. Where the follow-up's answer turns out to be "keep, deliberately
// unused", `#[expect(dead_code, reason = "…")]` is exactly what should land —
// it is that task's likely output, not a substitute for doing it.
//
// Making the modules private also turns every `pub(crate)` item inside them
// into a `clippy::redundant_pub_crate` error (103 sites here), which is denied
// repo-wide; that part is mechanical, the dead code is not.
//
// Left as its own task, with the count above as the size estimate.

/// Every identifier this crate's prose cites must exist. Crate-wide rather than
/// per-module, because the defect it guards has landed in several unrelated
/// files, including this crate's own guard against it.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "doc_citations_tests.rs"]
mod doc_citations_tests;

/// Every `path/to/file.rs:N` citation under `gears/qa-platform` names a file
/// that exists. A second guard rather than part of the one above: that one
/// checks identifiers and explicitly not paths, and covers this crate only.
/// Hosted here because this is where the subsystem's citation guards live.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "file_citations_tests.rs"]
mod file_citations_tests;

// No crate-level `test_support`: the plan's file list named one, and the two
// harnesses this crate needs already exist closer to what they serve -
// `domain::service::test_support` for the service doubles and
// `infra::storage::test_db` for the migrated in-memory database. A third,
// crate-level module would have been a re-export of those two.

/// **No `domain` module imports the `api` layer.** `api` is a transport over
/// `domain`, and the dependency may not run the other way. A structural guard
/// rather than a `cargo gears lint` rule -- see the module's own header for
/// why that CLI cannot express this one. Review findings #15, #16, #39.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "no_api_in_domain_tests.rs"]
mod no_api_in_domain_tests;
