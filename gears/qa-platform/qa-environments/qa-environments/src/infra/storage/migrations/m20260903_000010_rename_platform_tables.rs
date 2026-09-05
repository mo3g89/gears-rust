//! Renames three tables to match the aggregate's new name (spec D5).
//!
//! The word "platform" named the *product* — a product may be an `IaaS`, a
//! `PaaS`, an OS or an appliance — not the thing tested against, so the
//! aggregate is now `Environment`. Its three tables follow:
//!
//! | before                  | after                       |
//! | ----------------------- | --------------------------- |
//! | `qa_platforms`          | `qa_environments`           |
//! | `qa_platform_variables` | `qa_environment_variables`  |
//! | `qa_platform_leases`    | `qa_environment_leases`     |
//!
//! `RENAME TABLE` rather than create-copy-drop: the tables carry live rows and
//! foreign keys, and a copy would need a maintenance window this change does
//! not otherwise need. Reversible in `down`.
//!
//! ## No column is renamed, and that is deliberate
//!
//! `platform_id` — the FK on `qa_environment_variables` and the primary key of
//! `qa_environment_leases` — keeps its physical name here. This migration is
//! part of a phase whose contract is *zero behaviour change*, and a column
//! rename is a behaviour change: three further gears own `platform_id` columns
//! with no migration scheduled for them, and the plan's expand/contract
//! discipline puts column rewrites in the later tasks that already rewrite
//! these columns. The entities rename the Rust *field* to `environment_id` and
//! pin the physical name with `#[sea_orm(column_name = "platform_id")]`, so the
//! SQL this gear emits is byte-identical to what it emitted before.
//!
//! ## Why three literal SQL blobs and not `Table::rename()`
//!
//! Every migration in this directory is written as one `execute_unprepared`
//! blob per dialect, and this migration is precisely where the three dialects
//! diverge, so the builder could not express it:
//!
//! * **`SQLite`** keeps an index's *name* across `ALTER TABLE … RENAME TO` and
//!   has no `ALTER INDEX` at all, so a renamed index must be dropped and
//!   recreated. (The index definitions below are therefore repeated in full;
//!   they are copied from `m20260812_000001_initial` and
//!   `m20260813_000005_tenant_scoped_variable_index`, columns unchanged.)
//! * **Postgres** also keeps index names across a table rename, but renames an
//!   index in place with `ALTER INDEX … RENAME TO` — no definition is repeated,
//!   so no definition can drift.
//! * **`MySQL`** carries its unique indexes inline in `CREATE TABLE` as
//!   `UNIQUE KEY idx_…`, uses `RENAME TABLE` rather than `ALTER TABLE … RENAME
//!   TO`, and renames an index with `ALTER TABLE … RENAME INDEX … TO …`
//!   (metadata only, no table rebuild, and permitted on an index that backs a
//!   foreign key because the index object itself is unchanged).
//!
//! ## The indexes that are renamed, and the one that is not
//!
//! Renamed, because their names embed the old table name:
//!
//! | before                               | after                                  |
//! | ------------------------------------ | -------------------------------------- |
//! | `idx_qa_platforms_tenant_name`       | `idx_qa_environments_tenant_name`      |
//! | `idx_qa_platform_vars_tenant_unique` | `idx_qa_environment_vars_tenant_unique`|
//! | `idx_qa_platform_vars_platform`      | `idx_qa_environment_vars_environment`  |
//!
//! **`idx_qa_platform_vars_unique` is deliberately absent from that list, and
//! must stay absent.** It was the tenant-blind
//! `qa_platform_variables (platform_id, name)` index shipped by
//! `m20260812_000001_initial`, and `m20260813_000005_tenant_scoped_variable_index`
//! **drops** it in all three dialects as a security fix (it let anyone holding
//! another tenant's `platform_id` squat every variable name on it, and it
//! answered whether the victim already had one). Every database that can reach
//! this migration has run `000005`, so the index does not exist to be renamed —
//! `000005`'s own green test `the_tenant_blind_index_is_dropped` asserts exactly
//! that. Renaming it would fail on `MySQL`, and "renaming" it on `SQLite` (drop
//! + create) would *resurrect* the index that fix removed. There is no
//!   `idx_qa_environment_vars_unique` in this schema and there must never be
//!   one.
//!
//! `idx_qa_pipeline_vars_unique` is also left alone: `qa_pipeline_variables` is
//! not renamed by this migration.
//!
//! **`idx_qa_environment_vars_environment` indexes a column still physically
//! named `platform_id`**, so the index name is one step ahead of the column it
//! covers. That is intentional, not a mistake: it follows directly from the
//! no-column-rename rule above, and Task 14 — which rewrites these columns
//! anyway — is what closes the gap. Anyone inspecting the schema between now
//! and then will see the mismatch; this is the note that says it is expected.
//!
//! ## `MySQL`: version floor, and what a partial failure leaves behind
//!
//! **Version floor.** `ALTER TABLE … RENAME INDEX` requires **`MySQL` >= 5.7 or
//! `MariaDB` >= 10.5.2**. `MariaDB` is an engine this repository does
//! contemplate (`Makefile:627`, `bench-mariadb`), and on an older `MariaDB` the
//! statement is a syntax error rather than a no-op.
//!
//! **A partial failure is not recoverable by re-running.** `MySQL` DDL is
//! non-transactional and auto-commits per statement, so `MYSQL_UP` is not one
//! unit: if any `RENAME INDEX` fails after the `RENAME TABLE` has committed, the
//! database is left with three renamed tables carrying three old index names,
//! and the migration cannot simply be re-run — its first statement would fail
//! with "table doesn't exist". The manual recovery is to issue **only** the
//! three `RENAME INDEX` statements from `MYSQL_UP`, against the already-renamed
//! tables, and then let the runner record the migration.
//!
//! **Postgres is unaffected by both of the above.** A multi-statement
//! `execute_unprepared` there runs as one implicit transaction, so `POSTGRES_UP`
//! is all-or-nothing: a failure anywhere rolls the whole blob back and leaves
//! the schema untouched.
//!
//! **`RENAME TABLE`'s foreign-key repointing depends on `foreign_key_checks`
//! being ON** (the default; `SeaORM` does not touch it). With it off, the two
//! named foreign keys would be left naming a `qa_platforms` that no longer
//! exists — the same precondition, on a different engine, that `SQLITE_UP`'s
//! `PRAGMA foreign_keys` comment sets out below.
//!
//! ## The two named foreign keys are left alone, on purpose
//!
//! `fk_qa_platform_variables_platform` and `fk_qa_platform_leases_platform`
//! (declared inline in `m20260812_000001_initial`'s `MySQL` blob) keep their
//! names. This is a decision, not an oversight: they are `MySQL`-only metadata,
//! invisible to `SeaORM`, and renaming one means dropping and re-adding a
//! foreign key on a populated table — a rebuild-and-revalidate this rename buys
//! nothing from. `RENAME TABLE` already repoints them at `qa_environments`
//! automatically; only the constraint *names* remain historical. Postgres' and
//! `SQLite`'s equivalents are unnamed and follow their tables without any
//! statement here. The system-generated primary-key constraint names (Postgres'
//! `qa_platforms_pkey` and friends) are left for the same reason.
//!
//! ## Rolling back
//!
//! `down` is the exact inverse, statement for statement, and is exercised by
//! `the_rename_round_trips_and_keeps_its_rows` below. It must be applied before
//! any earlier migration's `down`: `m20260828_000007`'s and `m20260828_000008`'s
//! rollbacks name `qa_platforms` in raw SQL, which only exists once this one has
//! been reversed. Reverse order is what the migration runner does anyway.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE qa_platforms RENAME TO qa_environments;
ALTER TABLE qa_platform_variables RENAME TO qa_environment_variables;
ALTER TABLE qa_platform_leases RENAME TO qa_environment_leases;

-- Postgres keeps the old index names across a table rename, and renames an
-- index in place -- so no index definition is repeated here and none can drift.
ALTER INDEX idx_qa_platforms_tenant_name RENAME TO idx_qa_environments_tenant_name;
ALTER INDEX idx_qa_platform_vars_tenant_unique RENAME TO idx_qa_environment_vars_tenant_unique;
ALTER INDEX idx_qa_platform_vars_platform RENAME TO idx_qa_environment_vars_environment;
";

const MYSQL_UP: &str = r"
-- MySQL renames tables with RENAME TABLE, not ALTER TABLE ... RENAME TO, and
-- one statement covers all three atomically. It also repoints the two named
-- foreign keys (fk_qa_platform_variables_platform,
-- fk_qa_platform_leases_platform) at qa_environments by itself; their names
-- stay historical on purpose -- see this module's header.
RENAME TABLE qa_platforms TO qa_environments,
             qa_platform_variables TO qa_environment_variables,
             qa_platform_leases TO qa_environment_leases;

-- RENAME INDEX is in-place metadata: no table rebuild, and it is permitted on
-- idx_qa_platform_vars_platform even though that index backs a foreign key,
-- because the index object survives the rename. Contrast
-- m20260813_000005's drop-and-recreate, which is where InnoDB's errno 150
-- ordering constraint bites; nothing here drops an index, so no ordering
-- constraint applies.
ALTER TABLE qa_environments RENAME INDEX idx_qa_platforms_tenant_name TO idx_qa_environments_tenant_name;
ALTER TABLE qa_environment_variables RENAME INDEX idx_qa_platform_vars_tenant_unique TO idx_qa_environment_vars_tenant_unique;
ALTER TABLE qa_environment_variables RENAME INDEX idx_qa_platform_vars_platform TO idx_qa_environment_vars_environment;
";

const SQLITE_UP: &str = r"
-- The parent is renamed first: SQLite rewrites the child tables' `REFERENCES
-- qa_platforms(id)` clauses to name qa_environments as part of this statement,
-- so the foreign keys follow with no statement of their own.
--
-- What gates that rewrite is `PRAGMA foreign_keys` being ON -- not the 3.25+
-- `legacy_alter_table=off` default, which governs the trigger/view rewriting and
-- is necessary but not sufficient here. sqlx turns `foreign_keys` on for every
-- SQLite connection it opens, so the gear and its tests always get the rewrite;
-- a raw `sqlite3` shell defaults the pragma OFF, and running these statements
-- there would leave both children pointing at a `qa_platforms` that no longer
-- exists.
ALTER TABLE qa_platforms RENAME TO qa_environments;
ALTER TABLE qa_platform_variables RENAME TO qa_environment_variables;
ALTER TABLE qa_platform_leases RENAME TO qa_environment_leases;

-- SQLite has no ALTER INDEX, and a table rename leaves its indexes under their
-- old names, so each renamed index is dropped and recreated. The column lists
-- are unchanged from the migrations that created them --
-- m20260812_000001_initial for the tenant/name index and
-- m20260813_000005_tenant_scoped_variable_index for the other two.
DROP INDEX idx_qa_platforms_tenant_name;
CREATE UNIQUE INDEX idx_qa_environments_tenant_name ON qa_environments(tenant_id, name);
DROP INDEX idx_qa_platform_vars_tenant_unique;
CREATE UNIQUE INDEX idx_qa_environment_vars_tenant_unique
    ON qa_environment_variables(tenant_id, platform_id, name);
DROP INDEX idx_qa_platform_vars_platform;
CREATE INDEX idx_qa_environment_vars_environment ON qa_environment_variables(platform_id);
";

const POSTGRES_DOWN: &str = r"
ALTER INDEX idx_qa_environments_tenant_name RENAME TO idx_qa_platforms_tenant_name;
ALTER INDEX idx_qa_environment_vars_tenant_unique RENAME TO idx_qa_platform_vars_tenant_unique;
ALTER INDEX idx_qa_environment_vars_environment RENAME TO idx_qa_platform_vars_platform;

ALTER TABLE qa_environment_leases RENAME TO qa_platform_leases;
ALTER TABLE qa_environment_variables RENAME TO qa_platform_variables;
ALTER TABLE qa_environments RENAME TO qa_platforms;
";

const MYSQL_DOWN: &str = r"
ALTER TABLE qa_environments RENAME INDEX idx_qa_environments_tenant_name TO idx_qa_platforms_tenant_name;
ALTER TABLE qa_environment_variables RENAME INDEX idx_qa_environment_vars_tenant_unique TO idx_qa_platform_vars_tenant_unique;
ALTER TABLE qa_environment_variables RENAME INDEX idx_qa_environment_vars_environment TO idx_qa_platform_vars_platform;

RENAME TABLE qa_environment_leases TO qa_platform_leases,
             qa_environment_variables TO qa_platform_variables,
             qa_environments TO qa_platforms;
";

const SQLITE_DOWN: &str = r"
DROP INDEX idx_qa_environment_vars_environment;
CREATE INDEX idx_qa_platform_vars_platform ON qa_environment_variables(platform_id);
DROP INDEX idx_qa_environment_vars_tenant_unique;
CREATE UNIQUE INDEX idx_qa_platform_vars_tenant_unique
    ON qa_environment_variables(tenant_id, platform_id, name);
DROP INDEX idx_qa_environments_tenant_name;
CREATE UNIQUE INDEX idx_qa_platforms_tenant_name ON qa_environments(tenant_id, name);

-- The children are renamed back first and the parent last, mirroring `up`'s
-- parent-first order: SQLite rewrites the `REFERENCES` clauses whichever
-- direction the parent moves in, under the same `PRAGMA foreign_keys` condition
-- SQLITE_UP's comment sets out.
ALTER TABLE qa_environment_leases RENAME TO qa_platform_leases;
ALTER TABLE qa_environment_variables RENAME TO qa_platform_variables;
ALTER TABLE qa_environments RENAME TO qa_platforms;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::MySql => MYSQL_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
        };
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }

    /// The exact inverse of `up`: the three indexes take their old names back
    /// and the three tables take theirs, in the reverse statement order.
    ///
    /// Unlike `m20260813_000005`'s rollback, this direction cannot fail on
    /// existing data — a rename neither reads nor constrains a single row.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_DOWN,
            sea_orm::DatabaseBackend::MySql => MYSQL_DOWN,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_DOWN,
        };
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }
}

/// Round-trip tests for the rename, on `SQLite` — the dialect this gear's test
/// suite runs, in the shape the neighbouring migration tests use
/// (`migrated_db()` against `sqlite::memory:`). The Postgres and `MySQL` blobs
/// are verified by review; there is no three-dialect harness in this gear and
/// this migration is not the place to build one.
///
/// `clippy::disallowed_methods` is allowed for the reason every schema test in
/// this directory needs it: `secure_insert` requires an `AccessScope`, which a
/// migration test has no business constructing, and the `SecureORM` wrappers
/// expose no raw connection.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
    use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

    /// Runs every migration in `Migrator`'s declared order, which is also what
    /// proves `Migrator::migrations()` lists this one: an unregistered
    /// migration leaves the tables under their old names and every assertion
    /// below fails.
    async fn migrated_db() -> DatabaseConnection {
        let mut opts = ConnectOptions::new("sqlite::memory:");
        opts.max_connections(1).min_connections(1);
        let conn = Database::connect(opts)
            .await
            .expect("failed to connect to in-memory sqlite database");
        let manager = SchemaManager::new(&conn);
        // **Every migration except the contract one.** Task 19's
        // `m20260903_000012` drops eight columns, so a test that ran the whole
        // list would assert against a schema its own subject no longer has --
        // while stopping at *this* migration would cut off the later
        // `m20260903_000010` rename these tests' table names depend on. Stop
        // immediately before the drop, which is the last schema state in which
        // the legacy columns and the modern names coexist.
        for migration in super::super::Migrator::migrations() {
            if sea_orm_migration::MigrationName::name(&*migration)
                == "m20260903_000012_drop_legacy_platform_columns"
            {
                break;
            }
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-environments migrations");
        }
        conn
    }

    async fn names_of(conn: &DatabaseConnection, kind: &str) -> Vec<String> {
        conn.query_all(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            format!("SELECT name FROM sqlite_master WHERE type='{kind}'"),
        ))
        .await
        .unwrap()
        .iter()
        .filter_map(|row| row.try_get::<String>("", "name").ok())
        .collect()
    }

    /// The schema a fully migrated database ends up with: the three tables and
    /// the three indexes under their new names, and nothing left under the old
    /// ones.
    #[tokio::test]
    async fn the_migrated_schema_names_environments_not_platforms() {
        let conn = migrated_db().await;
        let tables = names_of(&conn, "table").await;
        let indexes = names_of(&conn, "index").await;

        for expected in [
            "qa_environments",
            "qa_environment_variables",
            "qa_environment_leases",
            // Not renamed: a pipeline variable belongs to no environment.
            "qa_pipeline_variables",
        ] {
            assert!(
                tables.contains(&expected.to_owned()),
                "{expected} must exist after the rename, found: {tables:?}"
            );
        }
        for gone in [
            "qa_platforms",
            "qa_platform_variables",
            "qa_platform_leases",
        ] {
            assert!(
                !tables.contains(&gone.to_owned()),
                "{gone} must be gone after the rename, found: {tables:?}"
            );
        }

        for expected in [
            "idx_qa_environments_tenant_name",
            "idx_qa_environment_vars_tenant_unique",
            "idx_qa_environment_vars_environment",
            "idx_qa_pipeline_vars_unique",
        ] {
            assert!(
                indexes.contains(&expected.to_owned()),
                "{expected} must exist after the rename, found: {indexes:?}"
            );
        }
        for gone in [
            "idx_qa_platforms_tenant_name",
            "idx_qa_platform_vars_tenant_unique",
            "idx_qa_platform_vars_platform",
        ] {
            assert!(
                !indexes.contains(&gone.to_owned()),
                "{gone} must be gone after the rename, found: {indexes:?}"
            );
        }
    }

    /// The tenant-blind index this migration must NOT resurrect.
    ///
    /// `m20260813_000005` dropped `idx_qa_platform_vars_unique` as a security
    /// fix. Adding it to this migration's rename list would recreate it under a
    /// new name on `SQLite` — an index that has never existed in this schema —
    /// and undo that fix silently. Neither name may appear.
    #[tokio::test]
    async fn the_tenant_blind_variable_index_is_not_resurrected_under_a_new_name() {
        let conn = migrated_db().await;
        let indexes = names_of(&conn, "index").await;
        for forbidden in [
            "idx_qa_platform_vars_unique",
            "idx_qa_environment_vars_unique",
        ] {
            assert!(
                !indexes.contains(&forbidden.to_owned()),
                "the tenant-blind (platform_id, name) index must not exist under any name, \
                 found: {indexes:?}"
            );
        }
    }

    /// `down` then `up` again, with a row in each table throughout: the names
    /// come back and go forward, and the rows are still there — which is the
    /// whole argument for `RENAME TABLE` over create-copy-drop.
    #[tokio::test]
    async fn the_rename_round_trips_and_keeps_its_rows() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

        conn.execute_unprepared(
            "INSERT INTO qa_environments (
                 id, tenant_id, name, kubeconfig_credstore_ref, available, is_default,
                 created_at, updated_at
             ) VALUES (
                 'e1', 't1', 'staging-a', 'credstore://ref', 1, 0,
                 '2026-09-03T00:00:00Z', '2026-09-03T00:00:00Z'
             );
             INSERT INTO qa_environment_variables (
                 id, tenant_id, platform_id, name, value, created_at, updated_at
             ) VALUES (
                 'v1', 't1', 'e1', 'API_TOKEN', 'x',
                 '2026-09-03T00:00:00Z', '2026-09-03T00:00:00Z'
             );
             INSERT INTO qa_environment_leases (
                 platform_id, tenant_id, mode, holders, version, updated_at
             ) VALUES ('e1', 't1', 'free', '[]', 0, '2026-09-03T00:00:00Z');",
        )
        .await
        .expect("seeding the renamed tables must succeed");

        super::Migration
            .down(&manager)
            .await
            .expect("down must roll the rename back");

        let tables = names_of(&conn, "table").await;
        let indexes = names_of(&conn, "index").await;
        for restored in [
            "qa_platforms",
            "qa_platform_variables",
            "qa_platform_leases",
        ] {
            assert!(
                tables.contains(&restored.to_owned()),
                "{restored} must be back after down, found: {tables:?}"
            );
        }
        for restored in [
            "idx_qa_platforms_tenant_name",
            "idx_qa_platform_vars_tenant_unique",
            "idx_qa_platform_vars_platform",
        ] {
            assert!(
                indexes.contains(&restored.to_owned()),
                "{restored} must be back after down, found: {indexes:?}"
            );
        }
        assert_eq!(
            count(&conn, "qa_platforms").await,
            1,
            "the row must survive down"
        );
        assert_eq!(count(&conn, "qa_platform_variables").await, 1);
        assert_eq!(count(&conn, "qa_platform_leases").await, 1);

        super::Migration
            .up(&manager)
            .await
            .expect("up must apply again after a down");

        let tables = names_of(&conn, "table").await;
        for expected in [
            "qa_environments",
            "qa_environment_variables",
            "qa_environment_leases",
        ] {
            assert!(
                tables.contains(&expected.to_owned()),
                "{expected} must be back after the second up, found: {tables:?}"
            );
        }
        assert_eq!(
            count(&conn, "qa_environments").await,
            1,
            "the row must survive the round trip"
        );
        assert_eq!(count(&conn, "qa_environment_variables").await, 1);
        assert_eq!(count(&conn, "qa_environment_leases").await, 1);
    }

    async fn count(conn: &DatabaseConnection, table: &str) -> i64 {
        conn.query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            format!("SELECT COUNT(*) AS n FROM {table}"),
        ))
        .await
        .unwrap()
        .expect("COUNT(*) always returns a row")
        .try_get::<i64>("", "n")
        .unwrap()
    }

    /// Both recreated `UNIQUE` indexes really do still enforce uniqueness, in
    /// **both** directions of the rename.
    ///
    /// This is the only automated proof of an index *property* this migration
    /// has on any dialect, and the property is the one `SQLite`'s
    /// drop-and-recreate can silently lose: `the_rename_round_trips_and_keeps_its_rows`
    /// checks index *names* and row counts, so a `down` that recreated
    /// `idx_qa_platform_vars_tenant_unique` without its `UNIQUE` keyword would
    /// pass every other test in this module. Hence four checks: the two unique
    /// indexes `up` recreates, then the two `down` recreates.
    ///
    /// Each duplicate below collides on the indexed columns alone — the primary
    /// keys are always distinct — so only the unique index can reject it.
    #[tokio::test]
    async fn the_recreated_indexes_are_still_unique() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

        // --- after `up`: the two indexes under their new names ---
        insert_environment(&conn, "qa_environments", "e1", "staging-a")
            .await
            .expect("the first environment must insert");
        insert_environment(&conn, "qa_environments", "e2", "staging-a")
            .await
            .expect_err(
                "idx_qa_environments_tenant_name must still be UNIQUE after being recreated \
                 under its new name",
            );

        insert_variable(&conn, "qa_environment_variables", "v1", "e1", "API_TOKEN")
            .await
            .expect("the first variable must insert");
        insert_variable(&conn, "qa_environment_variables", "v2", "e1", "API_TOKEN")
            .await
            .expect_err(
                "idx_qa_environment_vars_tenant_unique must still be UNIQUE after being \
                 recreated under its new name",
            );

        super::Migration
            .down(&manager)
            .await
            .expect("down must roll the rename back");

        // --- after `down`: the same two, under the names they had before ---
        insert_environment(&conn, "qa_platforms", "e3", "staging-a")
            .await
            .expect_err(
                "idx_qa_platforms_tenant_name must still be UNIQUE after down recreated it",
            );
        insert_environment(&conn, "qa_platforms", "e4", "staging-b")
            .await
            .expect("a distinct name must still insert, so the index is not over-tight");

        insert_variable(&conn, "qa_platform_variables", "v3", "e1", "API_TOKEN")
            .await
            .expect_err(
                "idx_qa_platform_vars_tenant_unique must still be UNIQUE after down \
                 recreated it",
            );
        insert_variable(&conn, "qa_platform_variables", "v4", "e1", "API_URL")
            .await
            .expect("a distinct variable name must still insert");
    }

    /// One environment row in `table`, tenant `t1`. `table` is a literal from
    /// this module, never caller input.
    async fn insert_environment(
        conn: &DatabaseConnection,
        table: &str,
        id: &str,
        name: &str,
    ) -> Result<(), sea_orm::DbErr> {
        conn.execute_unprepared(&format!(
            "INSERT INTO {table} (
                 id, tenant_id, name, kubeconfig_credstore_ref, available, is_default,
                 created_at, updated_at
             ) VALUES (
                 '{id}', 't1', '{name}', 'credstore://ref', 1, 0,
                 '2026-09-03T00:00:00Z', '2026-09-03T00:00:00Z'
             );"
        ))
        .await
        .map(|_| ())
    }

    /// One variable row in `table`, tenant `t1`. The column is still physically
    /// named `platform_id` — see this module's header on why no column is
    /// renamed.
    async fn insert_variable(
        conn: &DatabaseConnection,
        table: &str,
        id: &str,
        environment_id: &str,
        name: &str,
    ) -> Result<(), sea_orm::DbErr> {
        conn.execute_unprepared(&format!(
            "INSERT INTO {table} (
                 id, tenant_id, platform_id, name, value, created_at, updated_at
             ) VALUES (
                 '{id}', 't1', '{environment_id}', '{name}', 'x',
                 '2026-09-03T00:00:00Z', '2026-09-03T00:00:00Z'
             );"
        ))
        .await
        .map(|_| ())
    }
}
