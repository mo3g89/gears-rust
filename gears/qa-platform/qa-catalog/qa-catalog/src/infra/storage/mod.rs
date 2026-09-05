//! Infrastructure storage layer - database persistence for the qa-catalog gear.
//!
//! ## Architecture
//!
//! This module contains ALL `SeaORM`-specific code and database operations:
//! - `entity/` - `SeaORM` entity definitions (repositories, branches, ssh keys,
//!   custom plans, products, product versions, test bundles)
//! - `mapper.rs` - Conversions between `SeaORM` models and SDK contract types
//! - `migrations/` - Database schema migrations
//!
//! ## Layering Rules
//!
//! The infrastructure layer:
//! - **Contains**: ALL `SeaORM` imports and database-specific code
//! - **Uses**: `qa_catalog_sdk` contract types as the domain model
//! - **Provides**: `Orm*Repository` implementations of the `domain::repos` traits

pub mod entity;
pub mod mapper;
pub mod migrations;

mod bundles_sea_repo;
mod custom_plans_sea_repo;
mod db;
mod products_sea_repo;
mod ssh_keys_sea_repo;
mod test_repos_sea_repo;

pub use bundles_sea_repo::OrmBundlesRepository;
pub use custom_plans_sea_repo::OrmCustomPlansRepository;
pub use products_sea_repo::OrmProductsRepository;
pub use ssh_keys_sea_repo::OrmSshKeysRepository;
pub use test_repos_sea_repo::OrmTestReposRepository;
