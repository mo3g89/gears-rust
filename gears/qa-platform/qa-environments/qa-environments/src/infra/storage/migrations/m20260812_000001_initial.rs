//! Initial schema: environments, per-environment and pipeline variables, and
//! lease state.
//!
//! A backend match producing a single `execute_unprepared` DDL blob per
//! dialect. Lease holders are stored as a JSON array column alongside a
//! `version` integer for optimistic concurrency (backs
//! `DomainError::LeaseConflict`).
//!
//! # The aggregate is an `Environment`, and the tables say so
//!
//! The word "platform" names a *product* — a product may be an IaaS, a PaaS, an
//! OS or an appliance — not the thing tested against. The aggregate is
//! therefore `Environment` and its tables are `qa_environments`,
//! `qa_environment_variables` and `qa_environment_leases`.
//!
//! The foreign-key column those last two carry is still spelled `environment_id`.
//! That is the one piece of the old vocabulary still in the schema, and it is
//! deliberate rather than forgotten: the entities rename the Rust *field* to
//! `environment_id` and pin the physical name with
//! `#[sea_orm(column_name = "environment_id")]`, and the column rename is its own
//! change with its own blast radius across four gears.
//!
//! # No MySQL
//!
//! qa-insights' indexes exceed InnoDB's 3072-byte key limit and the
//! `qa-platform` feature deploys all four gears together, so no deployment can
//! reach a MySQL arm in any of them.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_environments (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    -- Required: every environment belongs to a product, and the product is
    -- what resolves the plugin that governs it. There is no fallback.
    product_id UUID NOT NULL,
    description TEXT NULL,
    available BOOLEAN NOT NULL DEFAULT TRUE,
    -- Written by an observation, never by an operator.
    observed_version VARCHAR(255) NULL,
    observed_build VARCHAR(255) NULL,
    -- Operator-set: the middle tier of the branch-resolution chain
    -- (explicit -> this -> the repository's own default_branch).
    default_branch VARCHAR(512) NULL,
    -- At most one per (tenant, product); enforced by the service in the same
    -- transaction rather than by a constraint.
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    version_detect_error TEXT NULL,
    version_detected_at TIMESTAMPTZ NULL,
    -- The plugin-shaped credential channel: one entry per credential key, each
    -- naming the credstore reference the material lives behind. Never the
    -- material itself.
    credentials JSONB NOT NULL DEFAULT '[]',
    -- Plugin-declared observed fields, and the operator-set non-secret half of
    -- the credential form. Kept apart because the two have different authors:
    -- one a machine, one a human.
    observed_attrs JSONB NOT NULL DEFAULT '{}',
    config JSONB NOT NULL DEFAULT '{}',
    observed_base_url TEXT NULL,
    -- The vocabulary every product can express. `unknown` covers both an
    -- environment nothing has looked at and one whose health read failed;
    -- health_checked_at is what separates them.
    health_state TEXT NOT NULL DEFAULT 'unknown',
    health_detail TEXT NULL,
    health_checked_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_environments_tenant_name ON qa_environments(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_environment_variables (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    environment_id UUID NOT NULL REFERENCES qa_environments(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    value TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_qa_environment_vars_environment ON qa_environment_variables(environment_id);
-- Tenant-scoped, not environment-scoped: without tenant_id in the key a
-- variable name is unique across tenants that share an environment id space.
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_environment_vars_tenant_unique
    ON qa_environment_variables(tenant_id, environment_id, name);

CREATE TABLE IF NOT EXISTS qa_pipeline_variables (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    value TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_pipeline_vars_unique ON qa_pipeline_variables(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_environment_leases (
    environment_id UUID PRIMARY KEY NOT NULL REFERENCES qa_environments(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL,
    mode VARCHAR(16) NOT NULL,
    holders JSONB NOT NULL DEFAULT '[]',
    version BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL
);
";

const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_environments (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    product_id TEXT NOT NULL,
    description TEXT NULL,
    available INTEGER NOT NULL DEFAULT 1,
    observed_version TEXT NULL,
    observed_build TEXT NULL,
    default_branch TEXT NULL,
    is_default INTEGER NOT NULL DEFAULT 0,
    version_detect_error TEXT NULL,
    version_detected_at TEXT NULL,
    credentials TEXT NOT NULL DEFAULT '[]',
    observed_attrs TEXT NOT NULL DEFAULT '{}',
    config TEXT NOT NULL DEFAULT '{}',
    observed_base_url TEXT NULL,
    health_state TEXT NOT NULL DEFAULT 'unknown',
    health_detail TEXT NULL,
    health_checked_at TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_environments_tenant_name ON qa_environments(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_environment_variables (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (environment_id) REFERENCES qa_environments(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_qa_environment_vars_environment ON qa_environment_variables(environment_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_environment_vars_tenant_unique
    ON qa_environment_variables(tenant_id, environment_id, name);

CREATE TABLE IF NOT EXISTS qa_pipeline_variables (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_pipeline_vars_unique ON qa_pipeline_variables(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_environment_leases (
    environment_id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    mode TEXT NOT NULL,
    holders TEXT NOT NULL DEFAULT '[]',
    version BIGINT NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (environment_id) REFERENCES qa_environments(id) ON DELETE CASCADE
);
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
            // See this module's header: no gear in this subsystem has a MySQL
            // schema, because they deploy together and qa-insights has none.
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
DROP TABLE IF EXISTS qa_environment_leases;
DROP TABLE IF EXISTS qa_pipeline_variables;
DROP TABLE IF EXISTS qa_environment_variables;
DROP TABLE IF EXISTS qa_environments;
        ";
        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}
