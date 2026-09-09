//! `qa_environments.product_id` becomes `NOT NULL` (**decision D9**).
//!
//! **Added 2026-09-05, product-plugins plan Task 20b.** Split from Task 20 by
//! ruling F-12 and run *after* Task 19 for one reason: `SQLite` cannot
//! `ALTER COLUMN … SET NOT NULL`, so its arm has to restate the whole table,
//! and Task 19 dropped eight of its columns. Written before the drop this
//! rebuild would have been written twice, the second replacing the first —
//! two hand-written copies of a wide table, which is the drift class this
//! branch keeps getting bitten by. Written once, after, it is 17 columns and
//! there is no second copy.
//!
//! # Why the column can be tightened at all
//!
//! Every path that could produce a productless row is gone: Task 20a made
//! `qa_products.plugin_instance_id` `NOT NULL`, and Task 19 (ruling F-13) made
//! a credential write with no resolvable plugin **refuse** rather than take a
//! pre-plugin path — so `create_environment` can no longer make one. What is
//! left is rows created before those tasks, which is what the pre-check below
//! is for.
//!
//! # The `SQLite` arm
//!
//! `qa_environment_variables` and `qa_environment_leases` both reference
//! `qa_environments(id)` `ON DELETE CASCADE`, and the 12-step rebuild `DROP`s
//! the referenced table — so foreign keys have to be suspended around it.
//!
//! **`PRAGMA foreign_keys = OFF` is not enough.** `SQLite` documents that
//! pragma as a **no-op inside a transaction**, and
//! `toolkit_db::migration_runner::run_gear_migrations` opens one per migration
//! — in production and under `run_migrations_for_testing` alike.
//!
//! `qa-catalog`'s `m20260903_000004` reached the same conclusion at the
//! pre-Task-19 review, and this paragraph used to say it "got the mechanism
//! wrong". The whole-branch review measured that claim and it is too strong:
//! that migration's own tests did only exercise `MigrationTrait::up` on a bare
//! connection, but the migration itself commits correctly — for a reason
//! neither file stated until now (see below), and its children are
//! `ON DELETE RESTRICT`, so it has no cascade to guard against in the first
//! place.
//!
//! So this sets **both**: `defer_foreign_keys = ON`, which works inside a
//! transaction, and `foreign_keys = OFF` for the bare-connection path. The
//! restore puts back the value that was actually there rather than hardcoding
//! `ON`.
//!
//! **What the deferral actually buys, stated correctly.** `SQLite` does not
//! re-evaluate deferred constraints at the commit: it keeps a *counter* of
//! outstanding violations, and `ALTER TABLE … RENAME TO` does not decrement it
//! — so "the rename has put a satisfying table back by then" is not why the
//! commit succeeds, and this doc used to say it was. The restore's
//! `PRAGMA defer_foreign_keys = OFF` **zeroes that counter**, and that reset is
//! what allows the `COMMIT`. It is load-bearing, not a courtesy; removing it as
//! redundant breaks the migration.
//!
//! It is also not what protects the child rows here — the save-and-restore
//! below is. See `qa-catalog`'s `m20260903_000004`, whose children are
//! `ON DELETE RESTRICT` and which therefore needs no such save.
//!
//! `the_rebuild_survives_referencing_rows_inside_the_runners_transaction` is
//! the test that measures the mechanism where it is used.
//!
//! # `down()` widens the column back and cannot fail
//!
//! Dropping a `NOT NULL` needs no data change.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The post-Task-19 column set, restated once.
///
/// Ordered as the table has them, because `INSERT … SELECT` below names every
/// column explicitly and a reordering would silently transpose values.
const SQLITE_UP: &str = r"
-- **THE CHILD ROWS ARE SAVED AND RESTORED, and that is not belt-and-braces.**
--
-- `DROP TABLE` performs an implicit `DELETE FROM` first, and `SQLite` fires
-- foreign-key ACTIONS for it whenever foreign keys were enabled *when the
-- statement was prepared*. `qa_environment_variables` and
-- `qa_environment_leases` both cascade off `qa_environments(id)`, so any
-- rebuild that drops the parent empties them.
--
-- Neither pragma can prevent it inside the runner's transaction:
-- `foreign_keys = OFF` is a documented no-op there, and `defer_foreign_keys`
-- defers the CHECK, not the ACTION. `legacy_alter_table` does not help either
-- -- it governs whether a RENAME rewrites child references, not whether a
-- DROP cascades. Measured, all three.
--
-- So the children are copied out before the parent is rebuilt and copied back
-- after, which needs no pragma to be honoured and works on either path.
-- `the_rebuild_survives_referencing_rows_inside_the_runners_transaction` is
-- what caught the first version: the migration committed cleanly and the
-- environment's variables were gone.
CREATE TEMP TABLE qa_environment_variables_backup AS
    SELECT * FROM qa_environment_variables;
CREATE TEMP TABLE qa_environment_leases_backup AS
    SELECT * FROM qa_environment_leases;
CREATE TABLE qa_environments_new (
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

INSERT INTO qa_environments_new (
    id, tenant_id, name, product_id, description, available, observed_version,
    observed_build, default_branch, is_default, version_detect_error,
    version_detected_at, credentials, observed_attrs, config, observed_base_url,
    health_state, health_detail, health_checked_at, created_at, updated_at
)
SELECT
    id, tenant_id, name, product_id, description, available, observed_version,
    observed_build, default_branch, is_default, version_detect_error,
    version_detected_at, credentials, observed_attrs, config, observed_base_url,
    health_state, health_detail, health_checked_at, created_at, updated_at
FROM qa_environments;

DROP TABLE qa_environments;
ALTER TABLE qa_environments_new RENAME TO qa_environments;

CREATE UNIQUE INDEX idx_qa_environments_tenant_name ON qa_environments(tenant_id, name);

-- The parent rows are back under the same ids, so every reference resolves.
INSERT INTO qa_environment_variables SELECT * FROM qa_environment_variables_backup;
INSERT INTO qa_environment_leases SELECT * FROM qa_environment_leases_backup;
DROP TABLE qa_environment_variables_backup;
DROP TABLE qa_environment_leases_backup;
";

/// The same table with `product_id` nullable again.
const SQLITE_DOWN: &str = r"
-- **THE CHILD ROWS ARE SAVED AND RESTORED, and that is not belt-and-braces.**
--
-- `DROP TABLE` performs an implicit `DELETE FROM` first, and `SQLite` fires
-- foreign-key ACTIONS for it whenever foreign keys were enabled *when the
-- statement was prepared*. `qa_environment_variables` and
-- `qa_environment_leases` both cascade off `qa_environments(id)`, so any
-- rebuild that drops the parent empties them.
--
-- Neither pragma can prevent it inside the runner's transaction:
-- `foreign_keys = OFF` is a documented no-op there, and `defer_foreign_keys`
-- defers the CHECK, not the ACTION. `legacy_alter_table` does not help either
-- -- it governs whether a RENAME rewrites child references, not whether a
-- DROP cascades. Measured, all three.
--
-- So the children are copied out before the parent is rebuilt and copied back
-- after, which needs no pragma to be honoured and works on either path.
-- `the_rebuild_survives_referencing_rows_inside_the_runners_transaction` is
-- what caught the first version: the migration committed cleanly and the
-- environment's variables were gone.
CREATE TEMP TABLE qa_environment_variables_backup AS
    SELECT * FROM qa_environment_variables;
CREATE TEMP TABLE qa_environment_leases_backup AS
    SELECT * FROM qa_environment_leases;
CREATE TABLE qa_environments_new (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    product_id TEXT NULL,
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

INSERT INTO qa_environments_new (
    id, tenant_id, name, product_id, description, available, observed_version,
    observed_build, default_branch, is_default, version_detect_error,
    version_detected_at, credentials, observed_attrs, config, observed_base_url,
    health_state, health_detail, health_checked_at, created_at, updated_at
)
SELECT
    id, tenant_id, name, product_id, description, available, observed_version,
    observed_build, default_branch, is_default, version_detect_error,
    version_detected_at, credentials, observed_attrs, config, observed_base_url,
    health_state, health_detail, health_checked_at, created_at, updated_at
FROM qa_environments;

DROP TABLE qa_environments;
ALTER TABLE qa_environments_new RENAME TO qa_environments;

CREATE UNIQUE INDEX idx_qa_environments_tenant_name ON qa_environments(tenant_id, name);

-- The parent rows are back under the same ids, so every reference resolves.
INSERT INTO qa_environment_variables SELECT * FROM qa_environment_variables_backup;
INSERT INTO qa_environment_leases SELECT * FROM qa_environment_leases_backup;
DROP TABLE qa_environment_variables_backup;
DROP TABLE qa_environment_leases_backup;
";

impl Migration {
    /// Whether `PRAGMA foreign_keys` is currently on, so the restore can put
    /// back what was there rather than a hardcoded `ON`.
    async fn foreign_keys_enabled(conn: &SchemaManagerConnection<'_>) -> Result<bool, DbErr> {
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "PRAGMA foreign_keys;".to_owned(),
            ))
            .await?;
        row.map_or(Ok(false), |row| {
            row.try_get_by_index::<i32>(0).map(|value| value != 0)
        })
    }

    /// Run a `SQLite` table rebuild with foreign-key enforcement suspended.
    ///
    /// **Both pragmas** — see the module doc. The restore is unconditional
    /// (the rebuild's result is held and returned after it) because a pooled
    /// connection left with foreign keys off is a silent integrity hole that
    /// outlives the failure that caused it.
    async fn rebuild_sqlite(
        conn: &SchemaManagerConnection<'_>,
        statements: &'static str,
    ) -> Result<(), DbErr> {
        let was_on = Self::foreign_keys_enabled(conn).await?;

        // Each pragma as its OWN statement: buried inside the multi-statement
        // DDL blob below they do not take effect.
        //
        // Neither of these prevents the cascade the rebuild's own SQL works
        // around -- see its comment. They are here for the constraint CHECK at
        // commit, which `defer_foreign_keys` is exactly for, and for the
        // bare-connection path where `foreign_keys = OFF` does work.
        conn.execute_unprepared("PRAGMA defer_foreign_keys = ON;")
            .await?;
        conn.execute_unprepared("PRAGMA foreign_keys = OFF;")
            .await?;
        let rebuilt = conn.execute_unprepared(statements).await;

        let restored = conn
            .execute_unprepared(if was_on {
                "PRAGMA foreign_keys = ON; PRAGMA defer_foreign_keys = OFF;"
            } else {
                "PRAGMA foreign_keys = OFF; PRAGMA defer_foreign_keys = OFF;"
            })
            .await;

        rebuilt?;
        restored?;
        Ok(())
    }

    /// Refuse to run while any environment names no product, naming the ids.
    ///
    /// **This can genuinely fail on real data** (ruling F-7): every path that
    /// makes a productless row is gone, but rows created before Task 19 and
    /// Task 20a survive, so the message has to say what to do about them.
    async fn refuse_productless_rows(conn: &SchemaManagerConnection<'_>) -> Result<(), DbErr> {
        let rows = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT id FROM qa_environments WHERE product_id IS NULL;".to_owned(),
            ))
            .await?;
        if rows.is_empty() {
            return Ok(());
        }
        let mut ids = Vec::with_capacity(rows.len());
        for row in &rows {
            ids.push(
                row.try_get_by_index::<uuid::Uuid>(0)
                    .map(|id| id.to_string())
                    .or_else(|_| row.try_get_by_index::<String>(0))?,
            );
        }
        Err(DbErr::Custom(format!(
            "qa-environments m20260903_000013: these environments name no product, and \
             `product_id` is about to become NOT NULL: {}. Assign each one a product, or \
             delete it, then re-run. An environment with no product has no plugin, so it \
             cannot be observed and no run can dispatch against it.",
            ids.join(", ")
        )))
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        Self::refuse_productless_rows(conn).await?;

        match manager.get_database_backend() {
            DatabaseBackend::Postgres => {
                conn.execute_unprepared(
                    "ALTER TABLE qa_environments ALTER COLUMN product_id SET NOT NULL;",
                )
                .await?;
            }
            DatabaseBackend::MySql => {
                conn.execute_unprepared(
                    "ALTER TABLE qa_environments MODIFY product_id CHAR(36) NOT NULL;",
                )
                .await?;
            }
            DatabaseBackend::Sqlite => Self::rebuild_sqlite(conn, SQLITE_UP).await?,
            other => {
                return Err(DbErr::Migration(format!(
                    "unsupported database backend: {other:?}"
                )));
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        match manager.get_database_backend() {
            DatabaseBackend::Postgres => {
                conn.execute_unprepared(
                    "ALTER TABLE qa_environments ALTER COLUMN product_id DROP NOT NULL;",
                )
                .await?;
            }
            DatabaseBackend::MySql => {
                conn.execute_unprepared(
                    "ALTER TABLE qa_environments MODIFY product_id CHAR(36) NULL;",
                )
                .await?;
            }
            DatabaseBackend::Sqlite => Self::rebuild_sqlite(conn, SQLITE_DOWN).await?,
            other => {
                return Err(DbErr::Migration(format!(
                    "unsupported database backend: {other:?}"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20260903_000013_environment_product_required_tests.rs"]
mod tests;
