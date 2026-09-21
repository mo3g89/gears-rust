//! Relational persistence for the two runtime-written key classes.
//!
//! * [`entity`] — the `SeaORM` entity for the single `credstore_plugin_values`
//!   table.
//! * [`migrations`] — the `SeaORM` migration set the platform's DB phase runs
//!   for this gear, in this gear's own migration-history table.
//! * [`repo`] — the repository: the **only** place secret bytes cross into or
//!   out of the database.
//! * [`store`] — the [`ValueStore`](crate::domain::ValueStore) adapter over
//!   [`repo`]: the only place that decides what runs in a transaction, and
//!   the seam that keeps `SeaORM` out of `domain/` (DE0301).
//! * [`error`] — the storage error type.

pub mod entity;
pub mod error;
pub mod migrations;
pub mod repo;
pub mod store;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod leak_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod repo_tests;
