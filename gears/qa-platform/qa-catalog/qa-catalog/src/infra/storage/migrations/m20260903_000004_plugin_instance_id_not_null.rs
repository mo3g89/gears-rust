//! Tightens `qa_products.plugin_instance_id` to `NOT NULL` — the **contract**
//! half of the expand/contract pair `m20260903_000003_product_plugin_instance`
//! opened, and the enforcement of decision **D6**: every product names a
//! product plugin, and there is no fallback path.
//!
//! ## The service guard lands first, and that ordering is not cosmetic
//!
//! `create_product` refused `plugin_instance_id: None` in the same task, and
//! it had to: the shipped UI cannot send that field, so until the guard landed
//! **every product the UI created carried `NULL`** — and this migration would
//! have failed on real data. That was finding FW-1 of the 2026-09-04 Phase E
//! review, and it is why Task 20 runs its Step 3 before its Step 2.
//!
//! ## `up()` checks before it alters, and says which rows are wrong
//!
//! An `ALTER` that fails on a constraint violation reports the constraint, not
//! the data — an operator learns that *some* row is null and has to go find
//! it. [`Migration::offending_ids`] runs first and puts the product ids in the
//! error, because the recovery is per-row: bind each one through
//! `PATCH /qa/v1/products/{id}` with a `plugin_instance_id` from
//! `GET /qa/v1/product-plugins`, or delete it.
//!
//! The backfill in `000003` means a deployment that has run both migrations in
//! order has no null rows to find. This check is for the deployment that
//! created products *between* the two, through a UI that could not name a
//! plugin — the FW-1 window.
//!
//! ## `SQLite` restates the table; Postgres and `MySQL` alter the column
//!
//! Deployment is Postgres (`config/qa-platform-stack.yaml`' `engine:
//! "postgres"`); `SQLite` is the in-memory backend the test suite runs on, and
//! it **cannot** `ALTER COLUMN ... SET NOT NULL` at all. So its arm is the
//! table-rebuild dance: create the replacement with the constraint, copy,
//! drop, rename, and recreate the indexes.
//!
//! Two things make that safe rather than merely plausible, and both are
//! asserted by this module's tests:
//!
//! * **the indexes are recreated, and a test checks they exist afterwards.**
//!   A rebuild that silently loses `idx_qa_products_tenant_name` would take
//!   the "at most one product per (tenant, name)" rule with it, and nothing
//!   else in this gear enforces that;
//! * **foreign-key enforcement is suspended around the rebuild, because it is
//!   ON.** `qa_test_repositories` references `qa_products(id)`, and
//!   `DROP TABLE qa_products` is refused outright while a referencing row
//!   exists.
//!
//!   **Suspending it takes two pragmas, and which one works depends on
//!   whether there is a transaction** (review finding IMPORTANT-2, which
//!   corrected this paragraph twice over):
//!
//!   * `PRAGMA foreign_keys = OFF` is documented by `SQLite` as a **no-op
//!     inside a transaction**;
//!   * `PRAGMA defer_foreign_keys = ON` works inside one, and postpones the
//!     violation from the `DROP` to the commit.
//!
//!   **What happens at that commit is not what this doc used to say.** `SQLite`
//!   does not re-evaluate deferred constraints there: it keeps a *counter* of
//!   outstanding violations, `DROP TABLE`'s implicit `DELETE` increments it, and
//!   `ALTER TABLE … RENAME TO` does **not** decrement it. Putting a satisfying
//!   table back is therefore not what lets the commit through.
//!
//!   **`PRAGMA defer_foreign_keys = OFF` in the restore is what lets it
//!   through**, because that reset zeroes the counter. It is load-bearing on
//!   the transactional path — the one production takes — and this doc used to
//!   call it a courtesy for the bare-connection path. Measured: drop that one
//!   pragma and
//!   `the_rebuild_survives_a_referencing_row_inside_the_runners_transaction`
//!   fails at `txn.commit()` with `SQLITE_CONSTRAINT_FOREIGNKEY` (787), with
//!   the rest of the module green.
//!
//!   Two consequences worth stating, because neither is guessable:
//!
//!   * the reset clears **every** deferred violation outstanding in the same
//!     transaction, not only the one this rebuild created. `run_gear_migrations`
//!     opens one transaction per migration, which bounds it;
//!   * **the child rows survive because of the schema, not because of this
//!     mechanism.** `qa_test_repositories.product_id` is `ON DELETE RESTRICT`
//!     (`m20260812_000002_initial.rs:79`, `:165`, `:243`) and is the only
//!     foreign key into `qa_products`, so the implicit `DELETE` has no cascade
//!     to fire. **A future child added with `ON DELETE CASCADE` would be
//!     silently emptied by this migration**, and would need
//!     `qa-environments`' `m20260903_000013` save-and-restore shape instead.
//!
//!   `toolkit_db::migration_runner::run_gear_migrations` — the path taken by
//!   `run_migrations_for_gear` in production **and** by
//!   `run_migrations_for_testing` — opens a transaction per migration and
//!   builds the `SchemaManager` on it, so the transactional case is the one
//!   that actually runs. An earlier version of this doc asserted the opposite
//!   ("there is no transaction here for it to defer to"), on the strength of a
//!   test that called `MigrationTrait::up` directly on a bare connection and
//!   therefore measured a path nothing uses. [`Migration::rebuild_sqlite`]
//!   now sets **both**, and `the_rebuild_survives_a_referencing_row_inside_the_runners_transaction`
//!   runs this migration through the runner, with data, so the mechanism is
//!   measured where it is used.
//!
//!   The restore reads the pragma's value first and puts **that** back, rather
//!   than hardcoding `ON` (finding m-4), and it runs even when the rebuild
//!   fails: a connection left with foreign keys off is a silent integrity hole
//!   that outlives the failure that caused it, and `toolkit-db` pools
//!   connections.
//!
//!   **Do not "tidy away" the `defer_foreign_keys = OFF` in that restore.** It
//!   reads as redundant — `SQLite` does clear the *flag* at every commit or
//!   rollback — but the flag is not what matters: the reset is what zeroes the
//!   deferred-violation counter, and without it this migration cannot commit at
//!   all. This paragraph used to say the opposite.
//!
//! ## `down()` widens the column back and cannot fail
//!
//! Dropping a `NOT NULL` needs no data change, so the reverse is safe on every
//! dialect and leaves a schema every reader that predates this migration
//! understands.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE qa_products ALTER COLUMN plugin_instance_id SET NOT NULL;
";

const POSTGRES_DOWN: &str = r"
ALTER TABLE qa_products ALTER COLUMN plugin_instance_id DROP NOT NULL;
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_products MODIFY plugin_instance_id VARCHAR(512) NOT NULL;
";

const MYSQL_DOWN: &str = r"
ALTER TABLE qa_products MODIFY plugin_instance_id VARCHAR(512) NULL;
";

/// The `SQLite` table rebuild.
///
/// The column list is the initial schema's (`m20260812_000002_initial`'s
/// `SQLITE_UP`) plus `plugin_instance_id` from `000003`, in that order. The
/// `INSERT ... SELECT` names every column explicitly rather than relying on
/// positional order, so a future column added between these two migrations
/// fails the copy loudly instead of silently shifting values one field left.
const SQLITE_UP: &str = r"
CREATE TABLE qa_products_new (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    product_key TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    folder TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    plugin_instance_id TEXT NOT NULL
);
INSERT INTO qa_products_new
    (id, tenant_id, name, product_key, description, folder, created_at, updated_at, plugin_instance_id)
SELECT
    id, tenant_id, name, product_key, description, folder, created_at, updated_at, plugin_instance_id
FROM qa_products;
DROP TABLE qa_products;
ALTER TABLE qa_products_new RENAME TO qa_products;
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_name ON qa_products(tenant_id, name);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_key ON qa_products(tenant_id, product_key);
";

/// The same rebuild with the column nullable again.
const SQLITE_DOWN: &str = r"
CREATE TABLE qa_products_new (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    product_key TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    folder TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    plugin_instance_id TEXT NULL
);
INSERT INTO qa_products_new
    (id, tenant_id, name, product_key, description, folder, created_at, updated_at, plugin_instance_id)
SELECT
    id, tenant_id, name, product_key, description, folder, created_at, updated_at, plugin_instance_id
FROM qa_products;
DROP TABLE qa_products;
ALTER TABLE qa_products_new RENAME TO qa_products;
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_name ON qa_products(tenant_id, name);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_products_tenant_key ON qa_products(tenant_id, product_key);
";

/// Pick the statements for a backend.
///
/// An exhaustive `match` on purpose: `sea_orm::DatabaseBackend` is
/// `non_exhaustive`-free, so a new dialect is a compile error here rather than
/// a wildcard that hands the server dialects the wrong statement. That is the
/// argument `m20260903_000003`'s `alter_for` makes, and this follows it.
const fn statements_for(backend: sea_orm::DatabaseBackend, up: bool) -> &'static str {
    match (backend, up) {
        (sea_orm::DatabaseBackend::Postgres, true) => POSTGRES_UP,
        (sea_orm::DatabaseBackend::Postgres, false) => POSTGRES_DOWN,
        (sea_orm::DatabaseBackend::MySql, true) => MYSQL_UP,
        (sea_orm::DatabaseBackend::MySql, false) => MYSQL_DOWN,
        (sea_orm::DatabaseBackend::Sqlite, true) => SQLITE_UP,
        (sea_orm::DatabaseBackend::Sqlite, false) => SQLITE_DOWN,
    }
}

impl Migration {
    /// Run the `SQLite` rebuild with foreign-key enforcement suspended, and
    /// restore it afterwards whatever happens.
    ///
    /// **Both pragmas, because only one of them works on each path** — see the
    /// module doc, which is where the mechanism is written down. In short:
    /// `foreign_keys = OFF` is a no-op inside a transaction and the migration
    /// runner opens one; `defer_foreign_keys = ON` works inside a transaction
    /// and defers the *check* to the commit. **Putting a satisfying table back
    /// is not what lets the commit through** — `SQLite` counts deferred
    /// violations rather than re-checking them, and the `RENAME` does not
    /// decrement that counter. What zeroes it is the `defer_foreign_keys = OFF`
    /// in the restore below. **Do not "tidy away" that line**: without it this
    /// migration fails its own commit with `SQLITE_CONSTRAINT_FOREIGNKEY`.
    /// (An earlier version of this paragraph said the rename was what carried
    /// the commit, which the module doc now refutes — re-review, N-2.)
    ///
    /// The restore is unconditional — the rebuild's result is held and
    /// returned *after* it — because a connection left with foreign keys off
    /// is a silent integrity hole that outlives the failure that caused it.
    /// `toolkit-db` pools connections, so that hole would be handed to
    /// ordinary request traffic. It restores the value that was actually
    /// there, not a hardcoded `ON` (finding m-4).
    async fn rebuild_sqlite(
        conn: &SchemaManagerConnection<'_>,
        statements: &'static str,
    ) -> Result<(), DbErr> {
        let was_on = Self::foreign_keys_enabled(conn).await?;

        conn.execute_unprepared("PRAGMA defer_foreign_keys = ON;")
            .await?;
        conn.execute_unprepared("PRAGMA foreign_keys = OFF;")
            .await?;

        let rebuilt = conn.execute_unprepared(statements).await;

        let restored = conn
            // `defer_foreign_keys = OFF` is load-bearing on BOTH arms: it is
            // what zeroes the deferred-violation counter, and without it the
            // runner's COMMIT fails. See this method's doc.
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

    /// Whether `PRAGMA foreign_keys` is currently on, so the restore can put
    /// back what was there.
    async fn foreign_keys_enabled(conn: &SchemaManagerConnection<'_>) -> Result<bool, DbErr> {
        let row = conn
            .query_one(Statement::from_string(
                conn.get_database_backend(),
                "PRAGMA foreign_keys;".to_owned(),
            ))
            .await?;
        row.map_or(Ok(false), |row| {
            row.try_get_by_index::<i32>(0).map(|value| value != 0)
        })
    }

    /// The ids of every product that names no plugin, as a comma-separated
    /// list, or `None` when there are none.
    ///
    /// Read through a plain `SELECT` rather than the entity, because the
    /// entity's `plugin_instance_id` becomes non-`Option` in this same task
    /// and a migration must not depend on the shape of today's model — that is
    /// how a migration stops compiling three releases later.
    async fn offending_ids(conn: &SchemaManagerConnection<'_>) -> Result<Option<String>, DbErr> {
        let rows = conn
            .query_all(Statement::from_string(
                conn.get_database_backend(),
                "SELECT id FROM qa_products WHERE plugin_instance_id IS NULL;".to_owned(),
            ))
            .await?;

        if rows.is_empty() {
            return Ok(None);
        }

        // `try_get_by_index::<String>` on every dialect: Postgres hands back a
        // `Uuid` and SQLite a `TEXT`, so the id is read as whatever it is and
        // rendered, never parsed. This is an error message, not a value the
        // migration acts on.
        let ids: Vec<String> = rows
            .iter()
            .map(|row| {
                row.try_get_by_index::<String>(0)
                    .or_else(|_| {
                        row.try_get_by_index::<uuid::Uuid>(0)
                            .map(|id| id.to_string())
                    })
                    .unwrap_or_else(|_| "<unreadable id>".to_owned())
            })
            .collect();
        Ok(Some(ids.join(", ")))
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        if let Some(ids) = Self::offending_ids(conn).await? {
            return Err(DbErr::Custom(format!(
                "cannot require a product plugin per product: these products name none, so \
                 `qa_products.plugin_instance_id` cannot be set NOT NULL. Bind each one (PATCH \
                 /qa/v1/products/{{id}} with a plugin_instance_id from GET \
                 /qa/v1/product-plugins) or delete it, then re-run the migration. Product ids: \
                 [{ids}]"
            )));
        }

        let backend = manager.get_database_backend();
        let statements = statements_for(backend, true);
        if backend == sea_orm::DatabaseBackend::Sqlite {
            Self::rebuild_sqlite(conn, statements).await?;
        } else {
            conn.execute_unprepared(statements).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = manager.get_database_backend();
        let statements = statements_for(backend, false);
        if backend == sea_orm::DatabaseBackend::Sqlite {
            Self::rebuild_sqlite(conn, statements).await?;
        } else {
            conn.execute_unprepared(statements).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20260903_000004_plugin_instance_id_not_null_tests.rs"]
mod tests;
