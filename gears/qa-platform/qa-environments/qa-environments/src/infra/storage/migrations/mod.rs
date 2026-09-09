use sea_orm_migration::prelude::*;

#[cfg(test)]
mod legacy_row;

mod m20260812_000001_initial;
mod m20260813_000004_observed_build;
mod m20260813_000005_tenant_scoped_variable_index;
mod m20260814_000006_platform_default_branch;
mod m20260828_000007_platform_observation;
mod m20260828_000008_platform_cluster_health;
mod m20260831_000009_platform_is_default;
mod m20260903_000010_rename_platform_tables;
mod m20260903_000011_environment_plugin_columns;
mod m20260903_000012_drop_legacy_platform_columns;
mod m20260903_000013_environment_product_required;

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
            Box::new(m20260903_000010_rename_platform_tables::Migration),
            Box::new(m20260903_000011_environment_plugin_columns::Migration),
            Box::new(m20260903_000012_drop_legacy_platform_columns::Migration),
            Box::new(m20260903_000013_environment_product_required::Migration),
        ]
    }
}
