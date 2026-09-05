//! Initial schema — the single `credstore_plugin_values` table.
//!
//! Per-backend raw `SQL` (not `SeaORM`'s schema builder) for the same reason
//! the credstore gear's own `m0001_initial_schema` uses it: `CHECK`
//! constraints and **partial** unique indexes are preserved verbatim, and
//! `sea-query`'s `Index::create()` cannot express a partial index at all.
//! `MySQL` is not supported; the migration fails fast with a typed error
//! (`MySQL` has no partial indexes and treats `NULL`s the same way `Postgres`
//! does, so the uniqueness below could not be enforced there).
//!
//! # Why two partial unique indexes and not one composite index
//!
//! A single `UNIQUE (tenant_id, owner_id, secret_ref)` would **not** keep the
//! tenant key class unique. Both `Postgres` and `SQLite` treat `NULL` as
//! distinct from every other `NULL` in a unique index, so with `owner_id
//! NULL` marking the tenant key class, that one index admits unlimited
//! duplicate rows for the same `(tenant_id, secret_ref)` — a silent
//! duplicate-value bug where a `put` would insert a second row and a later
//! `get` could return either. Verified empirically against `PostgreSQL 16` and
//! against `SQLite` (see the crate's report); `Postgres 15+` can opt out with
//! `UNIQUE NULLS NOT DISTINCT`, but `SQLite` has no equivalent, and this
//! migration must build the identical shape on both.
//!
//! So the two classes get one partial unique index each:
//!
//! * `owner_id IS NULL`     -> unique on `(tenant_id, secret_ref)`
//! * `owner_id IS NOT NULL` -> unique on `(tenant_id, owner_id, secret_ref)`
//!
//! A private secret and a tenant secret may therefore coexist under one
//! reference, which is exactly what the two separate `HashMap`s in the
//! in-memory plugin allow.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const MYSQL_NOT_SUPPORTED: &str = "postgres-credstore-plugin migrations: MySQL is not supported \
    (this migration set targets PostgreSQL/SQLite)";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let table_sql = match backend {
            sea_orm::DatabaseBackend::Postgres => {
                r"
CREATE TABLE IF NOT EXISTS credstore_plugin_values (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    owner_id UUID NULL,
    secret_ref TEXT NOT NULL CHECK (length(secret_ref) BETWEEN 1 AND 255),
    secret_value BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS credstore_plugin_values (
    id BLOB PRIMARY KEY NOT NULL,
    tenant_id BLOB NOT NULL,
    owner_id BLOB NULL,
    secret_ref TEXT NOT NULL CHECK (length(secret_ref) BETWEEN 1 AND 255),
    secret_value BLOB NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
            }
        };

        let statements = [
            table_sql,
            // Tenant key class (`owner_id = None` on the SPI).
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_credstore_plugin_values_tenant \
             ON credstore_plugin_values (tenant_id, secret_ref) WHERE owner_id IS NULL;",
            // Private key class (`owner_id = Some` on the SPI).
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_credstore_plugin_values_private \
             ON credstore_plugin_values (tenant_id, owner_id, secret_ref) WHERE owner_id IS NOT NULL;",
        ];

        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if matches!(
            manager.get_database_backend(),
            sea_orm::DatabaseBackend::MySql
        ) {
            return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
        }
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS credstore_plugin_values;")
            .await?;
        Ok(())
    }
}
