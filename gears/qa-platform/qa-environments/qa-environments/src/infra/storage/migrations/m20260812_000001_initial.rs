//! Initial schema: target platforms, per-platform/pipeline variables, and lease state.
//!
//! Follows the users-info migration shape: a backend match producing a single
//! `execute_unprepared` DDL blob per dialect. Lease holders are stored as a
//! JSON array column alongside a `version` integer for optimistic concurrency
//! (backs `DomainError::LeaseConflict`).

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_platforms (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    product_id UUID NULL,
    description TEXT NULL,
    kubeconfig_credstore_ref VARCHAR(1024) NOT NULL,
    available BOOLEAN NOT NULL DEFAULT TRUE,
    observed_version VARCHAR(255) NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_platforms_tenant_name ON qa_platforms(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_platform_variables (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    platform_id UUID NOT NULL REFERENCES qa_platforms(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    value TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_platform_vars_unique ON qa_platform_variables(platform_id, name);

CREATE TABLE IF NOT EXISTS qa_pipeline_variables (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    value TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_pipeline_vars_unique ON qa_pipeline_variables(tenant_id, name);

-- Deviates intentionally from the standard id/created_at column shape used
-- by the other tables above: a lease is 1:1 with its platform (at most one
-- lease row per platform, ever), so `platform_id` is the primary key rather
-- than a separate surrogate `id`, and there is no `created_at` since the row
-- is upserted in place for the platform's entire lifetime (`updated_at` plus
-- `version` fully capture its history for optimistic concurrency).
CREATE TABLE IF NOT EXISTS qa_platform_leases (
    platform_id UUID PRIMARY KEY NOT NULL REFERENCES qa_platforms(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL,
    mode VARCHAR(16) NOT NULL,
    holders JSONB NOT NULL DEFAULT '[]',
    version BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL
);
";

const MYSQL_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_platforms (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    product_id VARCHAR(36) NULL,
    description TEXT NULL,
    kubeconfig_credstore_ref VARCHAR(1024) NOT NULL,
    available BOOLEAN NOT NULL DEFAULT TRUE,
    observed_version VARCHAR(255) NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_platforms_tenant_name (tenant_id, name)
);

CREATE TABLE IF NOT EXISTS qa_platform_variables (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    platform_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    value TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_platform_vars_unique (platform_id, name),
    CONSTRAINT fk_qa_platform_variables_platform FOREIGN KEY (platform_id) REFERENCES qa_platforms(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS qa_pipeline_variables (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    value TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_pipeline_vars_unique (tenant_id, name)
);

CREATE TABLE IF NOT EXISTS qa_platform_leases (
    platform_id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    mode VARCHAR(16) NOT NULL,
    -- Expression default (MySQL 8.0.13+) for symmetry with the Postgres
    -- (`JSONB NOT NULL DEFAULT '[]'`) and SQLite (`TEXT NOT NULL DEFAULT
    -- '[]'`) definitions above. Every code path that writes this row (see
    -- `OrmLeasesRepository::compare_and_set`) always sets `holders`
    -- explicitly, so this default is a schema-symmetry safety net, not
    -- something application code relies on.
    holders JSON NOT NULL DEFAULT ('[]'),
    version BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMP NOT NULL,
    CONSTRAINT fk_qa_platform_leases_platform FOREIGN KEY (platform_id) REFERENCES qa_platforms(id) ON DELETE CASCADE
);
";

const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_platforms (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    product_id TEXT NULL,
    description TEXT NULL,
    kubeconfig_credstore_ref TEXT NOT NULL,
    available INTEGER NOT NULL DEFAULT 1,
    observed_version TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_platforms_tenant_name ON qa_platforms(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_platform_variables (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    platform_id TEXT NOT NULL,
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (platform_id) REFERENCES qa_platforms(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_platform_vars_unique ON qa_platform_variables(platform_id, name);

CREATE TABLE IF NOT EXISTS qa_pipeline_variables (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_pipeline_vars_unique ON qa_pipeline_variables(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_platform_leases (
    platform_id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    mode TEXT NOT NULL,
    holders TEXT NOT NULL DEFAULT '[]',
    version BIGINT NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (platform_id) REFERENCES qa_platforms(id) ON DELETE CASCADE
);
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::MySql => MYSQL_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
            other => {
                return Err(DbErr::Migration(format!(
                    "unsupported database backend: {other:?}"
                )));
            }
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = r"
DROP TABLE IF EXISTS qa_platform_leases;
DROP TABLE IF EXISTS qa_pipeline_variables;
DROP TABLE IF EXISTS qa_platform_variables;
DROP TABLE IF EXISTS qa_platforms;
        ";
        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}
