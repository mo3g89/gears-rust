//! Domain layer: pure rules, ports, repository traits, and services.

pub mod cron;
pub mod elevated;
pub mod error;
pub mod exclusivity;
pub mod local_client;
pub mod naming;
pub mod params;
pub mod ports;
pub mod queue;
pub mod repos;
pub mod runvars;
/// The composition tier.
///
/// The `#[cfg_attr(not(test), allow(dead_code))]` this module carried until
/// 2026-08-15 is **gone**, and its removal is the point: it was the crate's
/// single tripwire for "does anything in a non-test build actually reach
/// `AppServices`?". `gear.rs` now does, so every service, every field of the
/// container and every method they expose has to be genuinely reachable from
/// production code or the lint fires. Nothing else in this crate checks that -
/// a service wired into the container but never called compiles, tests, and
/// does nothing.
pub mod service;
pub mod state_machine;
pub mod system_actor;
/// Run timeout resolution — a pure core, and public because Task 15's re-run
/// resolves a *new* deadline and must not re-derive the chain.
pub mod timeout;
