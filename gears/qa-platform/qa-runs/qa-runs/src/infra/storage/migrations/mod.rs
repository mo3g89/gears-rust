use sea_orm_migration::prelude::*;

mod m20260813_000003_initial;
mod m20260813_000004_schedules;
mod m20260818_000005_case_fidelity;
mod m20260818_000006_collect_target;
mod m20260818_000007_schedule_notifications;
mod m20260831_000008_run_logs;

pub struct Migrator;

/// Migrations are **append-only** and this list is in application order.
///
/// A new table is a new file, never an edit to an older one: an edit would
/// never be applied to a deployment that already ran the earlier version.
/// `down()` runs in the reverse of this order, which is why each migration's
/// own test module drives `MigrationTrait::down` on its own `Migration` rather
/// than looping over this list.
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260813_000003_initial::Migration),
            Box::new(m20260813_000004_schedules::Migration),
            Box::new(m20260818_000005_case_fidelity::Migration),
            Box::new(m20260818_000006_collect_target::Migration),
            Box::new(m20260818_000007_schedule_notifications::Migration),
            Box::new(m20260831_000008_run_logs::Migration),
        ]
    }
}
