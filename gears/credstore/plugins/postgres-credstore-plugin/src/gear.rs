use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use credstore_sdk::{CredStorePluginClientV1, CredStorePluginSpecV1};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::contracts::DatabaseCapability;
use toolkit::gts::PluginV1;
use toolkit_db::DBProvider;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::PostgresCredStorePluginConfig;
use crate::domain::Service;
use crate::infra::storage::error::StoreError;
use crate::infra::storage::repo::ValueRepo;
use crate::infra::storage::store::PgValueStore;

/// Database-backed credstore plugin gear.
///
/// Persists the two runtime-written key classes so a secret written through
/// the credstore API survives a process or container restart.
///
/// `capabilities = [db]` puts this plugin on the platform's own DB path: the
/// runtime resolves a privileged connection from `DbManager` for the
/// `gears.postgres-credstore-plugin.database` stanza and runs
/// [`DatabaseCapability::migrations`] in the DB phase, before any `init`. The
/// two existing DB-owning plugins in this workspace instead take their own DSN
/// and drive `sqlx::migrate!` inside their `init`; that predates nothing in
/// particular and costs a second credential and a second connection pool, so
/// this plugin uses the platform mechanism — which also gives it a namespaced
/// migration-history table (`toolkit_migrations__postgres_credstore_plugin__<hash8>`)
/// and therefore lets it share the credstore gear's own database safely.
///
/// Not a `system` gear, and no `deps` on `credstore`: the credstore gear
/// resolves its backend plugin lazily on first request via the GTS
/// types-registry, so plugin `init` may (and does) run after the gear's.
#[toolkit::gear(
    name = "postgres-credstore-plugin",
    deps = [types_registry],
    capabilities = [db]
)]
pub struct PostgresCredStorePlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for PostgresCredStorePlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for PostgresCredStorePlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: PostgresCredStorePluginConfig = ctx.config_expanded_or_default()?;

        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            seed_count = cfg.secrets.len(),
            "Loaded plugin configuration"
        );

        // Fail closed: without a `database:` stanza this plugin has nowhere to
        // persist, and coming up as a silent value-losing store is the exact
        // failure it exists to end.
        let db_raw = ctx.db_required()?;
        let repo = ValueRepo::new(Arc::new(DBProvider::<StoreError>::new(db_raw.db())));
        let store = Arc::new(PgValueStore::new(repo));

        // Validate the config before anything is registered.
        let service = Arc::new(Service::from_config(store, &cfg)?);
        let seeded = service.seed().await?;

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) = PluginV1::<CredStorePluginSpecV1>::build_registration(
            "cf.core._.postgres_credstore.v1",
            cfg.vendor.clone(),
            cfg.priority,
        )?;

        // Publish to types-registry.
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // All fallible steps done — commit service to shared state.
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Register scoped client in ClientHub.
        let api: Arc<dyn CredStorePluginClientV1> = service;
        ctx.client_hub()
            .register_scoped::<dyn CredStorePluginClientV1>(ClientScope::gts_id(&instance_id), api);

        info!(instance_id = %instance_id, seeded, "postgres credstore plugin initialized");
        Ok(())
    }
}

impl DatabaseCapability for PostgresCredStorePlugin {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
}
