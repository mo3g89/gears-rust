//! Database-backed `CredStore` plugin.
//!
//! Implements the three-method plugin SPI
//! [`credstore_sdk::CredStorePluginClientV1`] on top of a single relational
//! table, so the two **runtime-written** key classes — `private`
//! (`owner_id = Some`) and `tenant` (`owner_id = None`) — survive a process or
//! container restart. That is the whole point of this crate: with the
//! in-memory `static-credstore-plugin` every stored secret dies with the
//! process.
//!
//! Config-seeded `shared`/global secrets are *not* stored in the table. They
//! are rebuilt from configuration on every boot and held in memory, exactly as
//! the static plugin holds them, so the `tenant -> shared -> global` read
//! fallback chain behaves identically.
//!
//! # Values are stored in plaintext
//!
//! This plugin writes secret material to the database **unencrypted**, which
//! matches the benchmark system it replaces (`vhp-testrunner` stores SSH
//! private keys in `ssh_keys.private_key TEXT`, no encryption crate anywhere)
//! and matches the fact that this codebase has no at-rest encryption or KMS
//! integration to build on. The consequence is on the record: a `pg_dump` of
//! the credstore database is a file of secrets. See `README.md`.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod gear;
pub mod infra;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod test_support;

pub use gear::PostgresCredStorePlugin;
