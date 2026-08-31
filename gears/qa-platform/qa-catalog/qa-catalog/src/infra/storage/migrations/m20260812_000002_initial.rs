//! Initial schema: products, test repositories/branches, SSH key metadata,
//! custom plans, and ephemeral test bundle descriptors.
//!
//! Follows the qa-environments migration shape: a backend match producing a
//! single `execute_unprepared` DDL blob per dialect (see DESIGN.md §3.7).
//! JSON array columns (`files`, `tags`) mirror the `holders` column pattern
//! used by qa-environments' `qa_platform_leases`. Column, index, and FK order
//! is kept identical across `POSTGRES_UP`, `MYSQL_UP`, and `SQLITE_UP`,
//! because a three-way eyeball diff is the only thing that catches a
//! forgotten dialect — tests exercise `SQLite` only.
//!
//! ## Products are created before repositories
//!
//! `qa_test_repositories.product_id` is a NOT NULL foreign key into
//! `qa_products` — every repository belongs to a product, which is how a
//! discovered plan is attributed to a product (legacy `enrich_plan_products`,
//! matching on repo ownership). The products table therefore has to be
//! declared first in each dialect blob.
//!
//! ## RESTRICT on products, CASCADE on repositories
//!
//! `qa_test_repositories.product_id` is `ON DELETE RESTRICT` while
//! `qa_repo_branches.repo_id` is `ON DELETE CASCADE` — two FKs, opposite
//! semantics, both deliberate. Deleting a product that still owns
//! repositories must fail loudly rather than silently cascade-delete a
//! repository (and its branch cache) as a side effect; deleting a repository
//! is exactly when its cached branches should go with it.
//!
//! ## `product_key`, not `key`
//!
//! The durable short code a product is known by is stored as `product_key`.
//! The obvious name, `key`, is a reserved word in `MySQL` and would need
//! backtick quoting in every hand-written DDL statement below. The SDK model
//! will expose it as `Product::key` (legacy parity), bridged in
//! `mapper::product_to_sdk`.
//!
//! ## Every unique index is tenant-prefixed
//!
//! Including the one on the child table whose parent already carries the
//! tenant (`qa_repo_branches`). A tenant-blind unique index on a child table
//! is a cross-tenant channel even when every query is scoped: resource UUIDs
//! are identifiers, not secrets, so a caller who learns another tenant's
//! `repo_id` could insert a row of its *own* tenant referencing it — the
//! insert passes tenant validation, stays invisible to both tenants' scoped
//! reads, and yet permanently collides with the owner's writes (a squatting
//! denial of service) while the unique violation reports whether the victim's
//! row exists (an existence oracle). Prefixing `tenant_id` makes such a row
//! harmless junk in the squatter's own key space.
//!
//! `MySQL` key-width budget (`InnoDB`, `utf8mb4`, 3072-byte limit): the widest
//! of these is `idx_qa_branches_unique` at
//! 36*4 + 36*4 + 512*4 = 2336 bytes, and `idx_qa_products_tenant_key` is
//! 36*4 + 255*4 = 1164 bytes — both fit, so no column had to shrink.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_products (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    product_key VARCHAR(255) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    folder VARCHAR(255) NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_name ON qa_products(tenant_id, name);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_key ON qa_products(tenant_id, product_key);

CREATE TABLE IF NOT EXISTS qa_test_repositories (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    product_id UUID NOT NULL REFERENCES qa_products(id) ON DELETE RESTRICT,
    name VARCHAR(255) NOT NULL,
    url VARCHAR(1024) NOT NULL,
    default_branch VARCHAR(255) NOT NULL,
    content_root VARCHAR(1024) NOT NULL DEFAULT '',
    credential_ref VARCHAR(1024) NULL,
    last_synced_at TIMESTAMPTZ NULL,
    sync_error TEXT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_repos_tenant_name ON qa_test_repositories(tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_qa_repos_product ON qa_test_repositories(tenant_id, product_id);

CREATE TABLE IF NOT EXISTS qa_repo_branches (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    repo_id UUID NOT NULL REFERENCES qa_test_repositories(id) ON DELETE CASCADE,
    name VARCHAR(512) NOT NULL,
    refreshed_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_branches_unique ON qa_repo_branches(tenant_id, repo_id, name);

CREATE TABLE IF NOT EXISTS qa_ssh_keys (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    credstore_ref VARCHAR(1024) NOT NULL,
    fingerprint VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_ssh_keys_tenant_name ON qa_ssh_keys(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_custom_plans (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    files JSONB NOT NULL DEFAULT '[]',
    tags JSONB NOT NULL DEFAULT '[]',
    timeout_seconds BIGINT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_custom_plans_tenant_name ON qa_custom_plans(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_test_bundles (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    storage_ref VARCHAR(2048) NOT NULL,
    checksum_sha256 VARCHAR(64) NOT NULL,
    size_bytes BIGINT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_qa_bundles_expiry ON qa_test_bundles(expires_at);
";

const MYSQL_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_products (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    product_key VARCHAR(255) NOT NULL,
    description TEXT NOT NULL DEFAULT (''),
    folder VARCHAR(255) NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_products_tenant_name (tenant_id, name),
    UNIQUE KEY idx_qa_products_tenant_key (tenant_id, product_key)
);

CREATE TABLE IF NOT EXISTS qa_test_repositories (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    product_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    url VARCHAR(1024) NOT NULL,
    default_branch VARCHAR(255) NOT NULL,
    content_root VARCHAR(1024) NOT NULL DEFAULT '',
    credential_ref VARCHAR(1024) NULL,
    last_synced_at TIMESTAMP NULL,
    sync_error TEXT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_repos_tenant_name (tenant_id, name),
    KEY idx_qa_repos_product (tenant_id, product_id),
    CONSTRAINT fk_qa_repos_product FOREIGN KEY (product_id) REFERENCES qa_products(id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS qa_repo_branches (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    repo_id VARCHAR(36) NOT NULL,
    name VARCHAR(512) NOT NULL,
    refreshed_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_branches_unique (tenant_id, repo_id, name),
    CONSTRAINT fk_qa_repo_branches_repo FOREIGN KEY (repo_id) REFERENCES qa_test_repositories(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS qa_ssh_keys (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    credstore_ref VARCHAR(1024) NOT NULL,
    fingerprint VARCHAR(255) NOT NULL,
    created_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_ssh_keys_tenant_name (tenant_id, name)
);

CREATE TABLE IF NOT EXISTS qa_custom_plans (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    -- Expression defaults (MySQL 8.0.13+) for symmetry with the Postgres
    -- (`JSONB NOT NULL DEFAULT '[]'`) and SQLite (`TEXT NOT NULL DEFAULT
    -- '[]'`) definitions; application code always writes these columns
    -- explicitly on insert.
    files JSON NOT NULL DEFAULT ('[]'),
    tags JSON NOT NULL DEFAULT ('[]'),
    timeout_seconds BIGINT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_custom_plans_tenant_name (tenant_id, name)
);

CREATE TABLE IF NOT EXISTS qa_test_bundles (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    storage_ref VARCHAR(2048) NOT NULL,
    checksum_sha256 VARCHAR(64) NOT NULL,
    size_bytes BIGINT NOT NULL,
    expires_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL,
    KEY idx_qa_bundles_expiry (expires_at)
);
";

const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_products (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    product_key TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    folder TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_name ON qa_products(tenant_id, name);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_key ON qa_products(tenant_id, product_key);

CREATE TABLE IF NOT EXISTS qa_test_repositories (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    product_id TEXT NOT NULL,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    default_branch TEXT NOT NULL,
    content_root TEXT NOT NULL DEFAULT '',
    credential_ref TEXT NULL,
    last_synced_at TEXT NULL,
    sync_error TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (product_id) REFERENCES qa_products(id) ON DELETE RESTRICT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_repos_tenant_name ON qa_test_repositories(tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_qa_repos_product ON qa_test_repositories(tenant_id, product_id);

CREATE TABLE IF NOT EXISTS qa_repo_branches (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    repo_id TEXT NOT NULL,
    name TEXT NOT NULL,
    refreshed_at TEXT NOT NULL,
    FOREIGN KEY (repo_id) REFERENCES qa_test_repositories(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_branches_unique ON qa_repo_branches(tenant_id, repo_id, name);

CREATE TABLE IF NOT EXISTS qa_ssh_keys (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    credstore_ref TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_ssh_keys_tenant_name ON qa_ssh_keys(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_custom_plans (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    files TEXT NOT NULL DEFAULT '[]',
    tags TEXT NOT NULL DEFAULT '[]',
    timeout_seconds BIGINT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_custom_plans_tenant_name ON qa_custom_plans(tenant_id, name);

CREATE TABLE IF NOT EXISTS qa_test_bundles (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    storage_ref TEXT NOT NULL,
    checksum_sha256 TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_qa_bundles_expiry ON qa_test_bundles(expires_at);
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
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = r"
DROP TABLE IF EXISTS qa_test_bundles;
DROP TABLE IF EXISTS qa_custom_plans;
DROP TABLE IF EXISTS qa_ssh_keys;
DROP TABLE IF EXISTS qa_repo_branches;
DROP TABLE IF EXISTS qa_test_repositories;
DROP TABLE IF EXISTS qa_products;
        ";
        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}
