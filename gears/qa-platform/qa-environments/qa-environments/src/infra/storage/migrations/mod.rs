use sea_orm_migration::prelude::*;

mod m20260812_000001_initial;

#[cfg(test)]
mod schema_behaviour_tests;

pub struct Migrator;

/// This list is in application order, and from here on it is **append-only**.
///
/// A new table or column is a new file, never an edit to this one: an edit
/// would never be applied to a deployment that already ran the earlier version.
///
/// The chain was collapsed to a single migration before the platform's first
/// installation, when no deployment had run any of it. What were eleven
/// migrations built a Kubernetes-shaped `qa_platforms` table, added seven
/// columns to it one at a time, renamed three tables to the vocabulary the
/// aggregate actually uses, opened an expand/contract pair that added the
/// plugin-shaped columns and backfilled them from the Kubernetes-shaped ones,
/// dropped those eight, and finally rebuilt the table to make `product_id`
/// required. A database with no environments has nothing to rename, nothing to
/// backfill and nothing to drop, so the first migration simply declares the
/// schema that sequence arrived at. Collapsing cost nothing then and cannot be
/// repeated now.
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260812_000001_initial::Migration)]
    }
}
