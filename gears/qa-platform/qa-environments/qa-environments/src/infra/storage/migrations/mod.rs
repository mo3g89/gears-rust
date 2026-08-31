use sea_orm_migration::prelude::*;

mod m20260812_000001_initial;
mod m20260813_000004_observed_build;
mod m20260813_000005_tenant_scoped_variable_index;
mod m20260814_000006_platform_default_branch;
mod m20260828_000007_platform_observation;
mod m20260828_000008_platform_cluster_health;
mod m20260831_000009_platform_is_default;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260812_000001_initial::Migration),
            Box::new(m20260813_000004_observed_build::Migration),
            Box::new(m20260813_000005_tenant_scoped_variable_index::Migration),
            Box::new(m20260814_000006_platform_default_branch::Migration),
            Box::new(m20260828_000007_platform_observation::Migration),
            Box::new(m20260828_000008_platform_cluster_health::Migration),
            Box::new(m20260831_000009_platform_is_default::Migration),
        ]
    }
}
