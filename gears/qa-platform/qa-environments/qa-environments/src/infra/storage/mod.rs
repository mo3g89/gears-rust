//! Infrastructure storage layer - database persistence for the qa-environments gear.
//!
//! ## Architecture
//!
//! This module contains ALL `SeaORM`-specific code and database operations:
//! - `entity/` - `SeaORM` entity definitions (environments, variables, leases)
//! - `mapper.rs` - Conversions between `SeaORM` models and SDK contract types
//! - `migrations/` - Database schema migrations
//!
//! ## Layering Rules
//!
//! The infrastructure layer:
//! - **Contains**: ALL `SeaORM` imports and database-specific code
//! - **Uses**: `qa_environments_sdk` contract types as the domain model
//! - **Provides**: `Orm*Repository` implementations of the `domain::repos` traits

pub mod entity;
pub mod mapper;
pub mod migrations;

mod db;
mod environments_sea_repo;
mod leases_sea_repo;
/// `OData` field allow-lists for the paginated collection reads.
///
/// `pub` rather than private because `api::rest::routes` names the filter enums
/// directly: the route advertises the same type the repository translates with,
/// which is what stops the two drifting apart. It is still crate-internal **by
/// construction** — `crate::infra` itself is `pub(crate)` (review finding #38),
/// so nothing here widens this crate's public API, and `pub(crate)` on this line
/// would be the redundancy `clippy::redundant_pub_crate` names.
pub mod odata;
mod variables_sea_repo;

pub use environments_sea_repo::OrmEnvironmentsRepository;
pub use leases_sea_repo::OrmLeasesRepository;
pub use variables_sea_repo::OrmVariablesRepository;

#[cfg(test)]
#[path = "odata_tests.rs"]
mod odata_tests;
