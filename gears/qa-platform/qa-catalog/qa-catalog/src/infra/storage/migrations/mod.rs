use sea_orm_migration::prelude::*;

mod m20260812_000002_initial;

#[cfg(test)]
mod schema_behaviour_tests;

pub struct Migrator;

/// This list is in application order, and from here on it is **append-only**.
///
/// A new table or column is a new file, never an edit to this one: an edit
/// would never be applied to a deployment that already ran the earlier version.
///
/// The chain was collapsed to a single migration before the platform's first
/// installation, when no deployment had run any of it. What were three
/// migrations declared the schema and then walked `qa_products.plugin_instance_id`
/// through an expand/contract pair — nullable, backfilled to the VHP plugin,
/// then tightened to `NOT NULL`. A database with no products has nothing to
/// backfill and nothing to tighten, so the column is simply `NOT NULL` where it
/// is declared. Collapsing cost nothing then and cannot be repeated now.
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260812_000002_initial::Migration)]
    }
}
