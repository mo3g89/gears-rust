use sea_orm_migration::prelude::*;

mod m20260812_000002_initial;
mod m20260903_000003_product_plugin_instance;
mod m20260903_000004_plugin_instance_id_not_null;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260812_000002_initial::Migration),
            Box::new(m20260903_000003_product_plugin_instance::Migration),
            Box::new(m20260903_000004_plugin_instance_id_not_null::Migration),
        ]
    }
}
