//! Relational persistence for the two runtime-written key classes.
//!
//! * [`entity`] — the `SeaORM` entity for the single `credstore_plugin_values`
//!   table.
//! * [`migrations`] — the `SeaORM` migration set the platform's DB phase runs
//!   for this gear, in this gear's own migration-history table.
//! * [`repo`] — the repository: the **only** place secret bytes cross into or
//!   out of the database.
//! * [`error`] — the storage error type and its mapping to
//!   [`credstore_sdk::CredStoreError`].

pub mod entity;
pub mod error;
pub mod migrations;
pub mod repo;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod leak_tests;
