//! Infrastructure storage layer - database persistence for the qa-environments gear.
//!
//! ## Architecture
//!
//! This module contains ALL `SeaORM`-specific code and database operations:
//! - `entity/` - `SeaORM` entity definitions (platforms, variables, leases)
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
mod leases_sea_repo;
mod platforms_sea_repo;
mod variables_sea_repo;

pub use leases_sea_repo::OrmLeasesRepository;
pub use platforms_sea_repo::OrmPlatformsRepository;
pub use variables_sea_repo::OrmVariablesRepository;
