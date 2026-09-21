use sea_orm_migration::prelude::*;

mod m20260818_000001_initial;
mod m20260921_000002_smtp_credentials;

#[cfg(test)]
mod schema_behaviour_tests;

pub struct Migrator;

/// This list is in application order, and from here on it is **append-only**.
///
/// A new table is a new file, never an edit to this one: an edit would never be
/// applied to a deployment that already ran the earlier version. `down()` runs
/// in the reverse of this order, which is why each migration's own test module
/// drives `MigrationTrait::down` on its own `Migration` rather than looping over
/// this list.
///
/// The chain was collapsed to a single migration before the platform's first
/// installation, when no deployment had run any of it: what were three
/// migrations declared the schema, added a table, and added a second table that
/// nothing ever read. Collapsing cost nothing then and cannot be repeated now.
///
/// `m20260921_000002_smtp_credentials` is the first migration added after that
/// collapse, and it is the proof of the append-only rule rather than an
/// exception to it: it adds two columns to `qa_notification_config` with
/// `ALTER TABLE`, because the initial migration has already run on the
/// deployments that need them.
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260818_000001_initial::Migration),
            Box::new(m20260921_000002_smtp_credentials::Migration),
        ]
    }
}
