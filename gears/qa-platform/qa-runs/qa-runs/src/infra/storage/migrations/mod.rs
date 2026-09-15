use sea_orm_migration::prelude::*;

mod m20260813_000003_initial;

#[cfg(test)]
mod schema_behaviour_tests;

pub struct Migrator;

/// This list is in application order, and from here on it is **append-only**.
///
/// A new table or column is a new file, never an edit to this one: an edit
/// would never be applied to a deployment that already ran the earlier version.
///
/// The chain was collapsed to a single migration before the platform's first
/// installation, when no deployment had run any of it. What were six migrations
/// declared three tables and then, in five further passes, added the two
/// schedule tables, the durable run-log table, three case-fidelity columns on
/// `qa_run_test_results`, a collect-target column on two tables, and three Slack
/// columns on `qa_schedules`. Every one of those is a column or a table a fresh
/// database can simply be created with. Collapsing cost nothing then and cannot
/// be repeated now.
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260813_000003_initial::Migration)]
    }
}
