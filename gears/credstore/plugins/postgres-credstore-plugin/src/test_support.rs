//! Test-only plumbing: build a service/repository over a migrated database.
//!
//! Two database shapes are used deliberately:
//!
//! * an **in-memory** `SQLite` database per test (the pattern in the credstore
//!   gear's `repo_impl/repo_tests.rs`) for behaviour that does not involve a
//!   restart. Schema comes from the migration definitions, never raw SQL.
//! * a **file-backed** `SQLite` database for the restart-survival test, because
//!   a `mode=memory` database is destroyed the moment its last connection
//!   closes — dropping the handle would destroy the very thing under test, so
//!   an in-memory database cannot evidence persistence at all.

use std::sync::Arc;

use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
use uuid::Uuid;

use crate::config::PostgresCredStorePluginConfig;
use crate::domain::{Service, ValueStore};
use crate::infra::storage::error::StoreError;
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::repo::ValueRepo;
use crate::infra::storage::store::PgValueStore;

/// DSN for a fresh, isolated in-memory `SQLite` database.
pub fn memory_dsn() -> String {
    format!(
        "sqlite:file:pg_credstore_plugin_{}?mode=memory&cache=shared",
        Uuid::new_v4()
    )
}

/// DSN for a file-backed `SQLite` database at `path`, created if missing.
pub fn file_dsn(path: &std::path::Path) -> String {
    format!("sqlite:file:{}?mode=rwc", path.display())
}

/// Connect to `dsn` and apply this plugin's migrations (idempotent).
pub async fn connect_migrated(dsn: &str) -> Db {
    let db = connect_db(
        dsn,
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect sqlite");

    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");

    db
}

/// Wrap a connected database in the plugin's DB provider.
pub fn provider_over(db: Db) -> Arc<crate::infra::storage::repo::ValueDbProvider> {
    Arc::new(DBProvider::<StoreError>::new(db))
}

/// Connect, migrate, and return both the provider (for tests that need raw
/// entity access, e.g. to prove a unique index rejects a duplicate) and the
/// repository built over it.
pub async fn provider_and_repo(
    dsn: &str,
) -> (Arc<crate::infra::storage::repo::ValueDbProvider>, ValueRepo) {
    let provider = provider_over(connect_migrated(dsn).await);
    (Arc::clone(&provider), ValueRepo::new(provider))
}

/// Wrap a connected database in the plugin's repository.
pub fn repo_over(db: Db) -> ValueRepo {
    ValueRepo::new(provider_over(db))
}

/// Wrap a connected database in the plugin's `ValueStore` adapter — the port
/// the domain service is built over.
pub fn store_over(db: Db) -> Arc<dyn ValueStore> {
    Arc::new(PgValueStore::new(repo_over(db)))
}

/// A service with no configured seeds, over a freshly migrated `dsn`.
pub async fn service_over(dsn: &str) -> Service {
    let db = connect_migrated(dsn).await;
    Service::from_config(store_over(db), &PostgresCredStorePluginConfig::default())
        .expect("empty config builds")
}

/// A service with no configured seeds, over a fresh in-memory database.
pub async fn memory_service() -> Service {
    service_over(&memory_dsn()).await
}
