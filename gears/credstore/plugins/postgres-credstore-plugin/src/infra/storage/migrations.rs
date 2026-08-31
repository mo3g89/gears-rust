//! `SeaORM` migrations for the database-backed credstore plugin.
//!
//! Run by the platform's DB phase before `init`
//! (`libs/toolkit/src/runtime/host_runtime.rs`), against this gear's **own**
//! migration-history table `toolkit_migrations__postgres_credstore_plugin__<hash8>`.
//! That is why sharing the `credstore` database with the credstore gear is
//! safe: the gear's history lives in `toolkit_migrations__credstore__<hash8>`,
//! and the two tables (`credstore_secrets` vs `credstore_plugin_values`) do
//! not collide either.

use sea_orm_migration::prelude::*;

pub mod m0001_initial_schema;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m0001_initial_schema::Migration)]
    }
}
