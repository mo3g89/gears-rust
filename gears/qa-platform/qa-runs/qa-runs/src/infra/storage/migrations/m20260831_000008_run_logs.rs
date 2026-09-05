//! `qa_run_logs` — the durable copy of a run's log output.
//!
//! # Its own table, not a column on `qa_runs`
//!
//! Legacy holds the same text in `run_results.raw_logs` and its own comment
//! records that selecting that column for every row of the full run history
//! once `OOMKilled` the manager. A separate table makes that query structurally
//! impossible rather than merely discouraged; `runs_sea_repo`'s list guard
//! (`no_list_query_reaches_the_log_table`) is what keeps it that way.
//!
//! # `run_id` is the primary key
//!
//! One log per run, so the append needs no prior lookup and a doubled flush
//! cannot produce a second row. This differs from `qa_run_test_results`, which
//! carries a surrogate `id` because a run has many results.
//!
//! # A log row's tenant is provably its run's tenant
//!
//! The foreign key is **composite** — `(run_id, tenant_id) REFERENCES
//! qa_runs(id, tenant_id)` — not `run_id` alone, and that is a security
//! control rather than a modelling preference.
//!
//! With a `run_id`-only key, nothing tied `qa_run_logs.tenant_id` to the
//! tenant that owns the run. `RunLogsRepository::append_log` does a scoped
//! `UPDATE` and falls through to `secure_insert` when it matches no row, and
//! `validate_insert_scope` (`libs/toolkit-db/src/secure/db_ops.rs`) checks the
//! `ActiveModel`'s **own** `tenant_id` against the caller's scope — which the
//! caller supplied, so it cannot fail. A foreign tenant reaching `append_log`
//! first could therefore create a run's log row under its own tenant and
//! poison it permanently: the rightful tenant's scoped `get_log` filters the
//! row out and every legitimate append then fails forever on the `run_id`
//! primary key. Probed and confirmed against the `SQLite` fixture by the
//! whole-branch review, 2026-08-31.
//!
//! **That was never reachable in production** — the only attach path reads
//! `candidate.tenant_id` off the run row (`service::dispatch`), the watcher
//! mints `for_result_ingest(target.tenant)`, and `record`/`append_log` have
//! exactly one production caller each. It is closed anyway, because
//! `qa_run_logs` has never been deployed, so the constraint costs nothing now
//! and would cost a data migration later.
//!
//! The composite key needs a unique index on the **parent**'s `(id,
//! tenant_id)`, which `qa_runs` did not have — every dialect requires the
//! referenced columns to carry a unique constraint or index. `qa_runs` **is**
//! deployed with data, so that index is a migration against a live table; it
//! is safe unconditionally because `qa_runs.id` is already the primary key, so
//! `(id, tenant_id)` is unique for any set of existing rows by implication and
//! the index build cannot find a duplicate to fail on.
//!
//! There is deliberately no `ON UPDATE` action: a run's `tenant_id` must never
//! change, and without one such an `UPDATE` is refused by the constraint
//! instead of silently re-homing the log with it.
//!
//! # There is no size cap, by decision
//!
//! User decision, 2026-08-31, with the risk stated: nothing bounds one run's
//! text, and retention is the cascade alone. `lines` exists so a consumer can
//! learn a log's size without selecting it.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

// The parent unique index the composite foreign key below references. Run
// **before** the `CREATE TABLE`, because the constraint cannot be declared
// until its referenced columns are unique.
//
// `CREATE UNIQUE INDEX IF NOT EXISTS`, the form
// `m20260813_000003_initial`'s `POSTGRES_UP` and `SQLITE_UP` already use for
// every index they declare, rather than `ALTER TABLE ... ADD CONSTRAINT ...
// UNIQUE`: the `ALTER` form has no `IF NOT EXISTS` in either dialect, and
// Postgres accepts a plain unique index as a foreign key's referenced key
// (it resolves the parent key against `pg_index`, requiring only that the
// index be unique, immediate, valid and non-partial).
//
// **Safe against the deployed table.** `qa_runs.id` is already its primary
// key, so `(id, tenant_id)` is unique for any set of existing rows by
// implication -- the build has no duplicate it could fail on. It is
// redundant with that primary key as an *access path*, and it is not there
// as one; it is there because a composite foreign key needs a unique parent
// key to reference.
const PG_PARENT_UQ: &str = r"
CREATE UNIQUE INDEX IF NOT EXISTS uq_qa_runs_id_tenant ON qa_runs(id, tenant_id);
";

// `ALTER TABLE ... ADD UNIQUE KEY`, not `CREATE UNIQUE INDEX`: `qa_runs`
// already exists on a `MySQL` deployment, and `MySQL` declares its indexes
// inline in `CREATE TABLE` (`m20260813_000003_initial`'s `MYSQL_UP`:
// `UNIQUE KEY idx_qa_runs_tenant_name (tenant_id, name)`), so there is no
// house-style `CREATE INDEX` form to follow here. `MySQL` has no
// `IF NOT EXISTS` on either spelling of an index addition, so this statement
// is not idempotent -- acceptable because a migration body runs once, and
// moot in practice because `qa-runs`' `Cargo.toml` enables only
// `["sqlite", "pg"]`, which makes this whole body declaration-only.
const MYSQL_PARENT_UQ: &str = r"
ALTER TABLE qa_runs ADD UNIQUE KEY uq_qa_runs_id_tenant (id, tenant_id);
";

// `SQLite` enforces the parent-key requirement at DML time rather than at
// `CREATE TABLE` time: without this index every insert into `qa_run_logs`
// would fail with `foreign key mismatch`, not the `CREATE TABLE` below.
const SQLITE_PARENT_UQ: &str = r"
CREATE UNIQUE INDEX IF NOT EXISTS uq_qa_runs_id_tenant ON qa_runs(id, tenant_id);
";

// **The cascade is a table-level, composite `FOREIGN KEY` in every dialect,
// Postgres included.** See the module header on why the tenant column is part
// of it. A composite key cannot be written inline on a column definition in
// any dialect, so the table-level form is forced here rather than merely
// preferred -- which is also what makes the guard below able to require one
// spelling of all three bodies instead of exempting Postgres.
const PG_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_run_logs (
    run_id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    text TEXT NOT NULL DEFAULT '',
    lines BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (run_id, tenant_id) REFERENCES qa_runs(id, tenant_id) ON DELETE CASCADE
);
";

// `VARCHAR(36)` for the UUID columns and `TIMESTAMP` for the timestamp,
// matching `m20260813_000003_initial`'s `MYSQL_UP` exactly (that file is the
// authority on dialect column types, not this migration's own first guess,
// which had wrongly proposed `CHAR(36)`/`BINARY(16)`).
//
// `LONGTEXT`, not the bare `TEXT` the authority file uses for `error`: this
// column is the one place the "no size cap, by decision" rule in the module
// header actually binds, and plain `MySQL` `TEXT` caps out at 64KiB, which
// would silently reintroduce a cap the decision rejected. `DEFAULT ('')` is
// the parenthesised-expression form `m20260813_000003_initial` already uses
// for its own un-defaultable-by-literal columns (`parameters JSON NOT NULL
// DEFAULT ('[]')`) — `MySQL` (8.0.13+) accepts an expression default on
// `TEXT`/`JSON`/`BLOB` where a literal default is rejected.
//
// **The cascade is a table-level `CONSTRAINT ... FOREIGN KEY`, not inline on
// `run_id`.** Corrected in fix-round 1: `MySQL`/`InnoDB` parses and silently
// drops an inline `REFERENCES` clause written on a column definition — it
// registers no constraint at all — and only a table-level `FOREIGN KEY (...)
// REFERENCES ...` is real. `m20260813_000003_initial`'s `MYSQL_UP` already
// knows this (`CONSTRAINT fk_qa_run_queue_run FOREIGN KEY (run_id)
// REFERENCES qa_runs(id) ON DELETE CASCADE`); this migration's first version
// missed it because `run_id` is this table's primary key rather than a plain
// column, which made the inline form look load-bearing when it was inert.
const MYSQL_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_run_logs (
    run_id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    text LONGTEXT NOT NULL DEFAULT (''),
    lines BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMP NOT NULL,
    CONSTRAINT fk_qa_run_logs_run FOREIGN KEY (run_id, tenant_id) REFERENCES qa_runs(id, tenant_id) ON DELETE CASCADE
);
";

// `TEXT` for the UUID columns and `TEXT` for the timestamp, matching
// `m20260813_000003_initial`'s `SQLITE_UP` exactly (`TIMESTAMP`, guessed
// initially, is not what that file uses for any timestamp column).
//
// The cascade is a table-level `FOREIGN KEY`, matching
// `m20260813_000003_initial`'s `SQLITE_UP` style, even though `SQLite`
// itself honors the inline form too (`deleting_a_run_deletes_its_log` passed
// against the inline version in fix-round 0). Aligned in fix-round 1 so all
// three bodies read the same way, which is what would have caught the
// `MySQL` defect by inspection instead of by a security review.
const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_run_logs (
    run_id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    text TEXT NOT NULL DEFAULT '',
    lines BIGINT NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (run_id, tenant_id) REFERENCES qa_runs(id, tenant_id) ON DELETE CASCADE
);
";

// The log table goes first, so the composite foreign key is gone before the
// parent index it references is dropped. Dialect-split only because `MySQL`
// spells an index drop as an `ALTER TABLE`.
const PG_DOWN: &str = r"
DROP TABLE IF EXISTS qa_run_logs;
DROP INDEX IF EXISTS uq_qa_runs_id_tenant;
";

const MYSQL_DOWN: &str = r"
DROP TABLE IF EXISTS qa_run_logs;
ALTER TABLE qa_runs DROP INDEX uq_qa_runs_id_tenant;
";

const SQLITE_DOWN: &str = r"
DROP TABLE IF EXISTS qa_run_logs;
DROP INDEX IF EXISTS uq_qa_runs_id_tenant;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let (parent_uq, sql) = match backend {
            sea_orm::DatabaseBackend::Postgres => (PG_PARENT_UQ, PG_UP),
            sea_orm::DatabaseBackend::MySql => (MYSQL_PARENT_UQ, MYSQL_UP),
            sea_orm::DatabaseBackend::Sqlite => (SQLITE_PARENT_UQ, SQLITE_UP),
        };
        // The parent unique index first: the composite foreign key in `sql`
        // references `qa_runs(id, tenant_id)`, and no dialect accepts a
        // foreign key onto columns that carry no unique key. Two
        // `execute_unprepared` calls rather than one concatenated script so a
        // failure names which half failed.
        conn.execute_unprepared(parent_uq).await?;
        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => PG_DOWN,
            sea_orm::DatabaseBackend::MySql => MYSQL_DOWN,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_DOWN,
        };
        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{
        ActiveModelTrait, ActiveValue, ConnectOptions, ConnectionTrait, Database,
        DatabaseConnection, EntityTrait,
    };
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{MYSQL_PARENT_UQ, MYSQL_UP, PG_PARENT_UQ, PG_UP, SQLITE_PARENT_UQ, SQLITE_UP};
    use crate::infra::storage::entity::{run, run_log};

    /// Every dialect must declare the same five columns in the same order.
    #[test]
    fn all_three_dialects_declare_the_same_columns() {
        for (name, ddl) in [
            ("postgres", PG_UP),
            ("mysql", MYSQL_UP),
            ("sqlite", SQLITE_UP),
        ] {
            let names = column_names(ddl);
            assert_eq!(
                names,
                vec!["run_id", "tenant_id", "text", "lines", "updated_at"],
                "{name} declares the wrong columns or order",
            );
        }
    }

    /// The cascade is the entire retention policy (spec D-RLP-1) **and** the
    /// only thing tying a log row's tenant to its run's tenant, so it is
    /// asserted in the DDL text of every dialect and not only behaviourally —
    /// and asserted in the *form* each dialect actually honors, not merely
    /// that the words appear somewhere.
    ///
    /// **Fix-round 1.** The original version of this test only checked
    /// `ddl.contains("REFERENCES qa_runs(id) ON DELETE CASCADE")`, which is
    /// true of an inline `REFERENCES` clause written on the `run_id` column
    /// definition — and `MySQL`/`InnoDB` parses and silently drops exactly
    /// that form, registering no constraint at all. The substring-only
    /// version passed against that broken `MYSQL_UP`, and this crate has no
    /// `MySQL` execution tier to catch it any other way.
    ///
    /// **Final fix wave.** The key is now composite (see the module header),
    /// which cannot be spelled inline in any dialect — so Postgres is no
    /// longer exempt from the table-level requirement and all three bodies
    /// are held to one spelling. Break-tested twice: dropping `tenant_id`
    /// from either side of any one dialect's `FOREIGN KEY` turns the first
    /// assertion red, and putting a dialect's key back inline on `run_id`
    /// turns the second red.
    #[test]
    fn every_dialect_cascades_from_qa_runs_on_the_composite_key() {
        for (name, ddl) in [
            ("postgres", PG_UP),
            ("mysql", MYSQL_UP),
            ("sqlite", SQLITE_UP),
        ] {
            assert!(
                ddl.contains(
                    "FOREIGN KEY (run_id, tenant_id) REFERENCES qa_runs(id, tenant_id) \
                     ON DELETE CASCADE"
                ),
                "{name} must cascade from qa_runs on (id, tenant_id): a run_id-only key \
                 leaves a log row's tenant unconstrained, which lets a foreign tenant \
                 create and permanently poison a run's log row, and dropping the cascade \
                 orphans a deleted run's log forever",
            );

            let run_id_line = ddl
                .lines()
                .find(|l| l.trim_start().starts_with("run_id "))
                .unwrap_or_else(|| panic!("{name} declares no run_id column"));
            assert!(
                !run_id_line.contains("REFERENCES"),
                "{name}'s cascade must not be declared inline on run_id — MySQL/InnoDB \
                 silently drops an inline REFERENCES clause and registers no constraint \
                 at all, and a composite key cannot be written inline in any dialect: \
                 {run_id_line}",
            );
        }
    }

    /// **The composite foreign key is unusable without a unique key on the
    /// parent's `(id, tenant_id)`**, and every dialect says so differently:
    /// Postgres and `MySQL` refuse the `CREATE TABLE`/`ALTER TABLE` outright,
    /// while `SQLite` accepts the DDL and then fails every insert with
    /// `foreign key mismatch`. So the index is asserted per dialect rather
    /// than left to `deleting_a_run_deletes_its_log` to discover on one.
    ///
    /// Break-tested: emptying any one of the three constants turns this red,
    /// and emptying `SQLITE_PARENT_UQ` additionally turns
    /// `deleting_a_run_deletes_its_log` and
    /// `a_foreign_scoped_append_is_refused` red.
    #[test]
    fn every_dialect_declares_the_parent_unique_key() {
        for (name, ddl) in [
            ("postgres", PG_PARENT_UQ),
            ("mysql", MYSQL_PARENT_UQ),
            ("sqlite", SQLITE_PARENT_UQ),
        ] {
            assert!(
                ddl.contains("uq_qa_runs_id_tenant"),
                "{name} must declare the parent unique key the composite foreign key \
                 references",
            );
            assert!(
                ddl.contains("(id, tenant_id)"),
                "{name}'s parent unique key must be on (id, tenant_id) in that order, \
                 or the foreign key resolves against nothing: {ddl}",
            );
        }
    }

    /// `run_id` is the primary key, so a doubled flush cannot make two rows.
    #[test]
    fn run_id_is_the_primary_key_in_every_dialect() {
        for (name, ddl) in [
            ("postgres", PG_UP),
            ("mysql", MYSQL_UP),
            ("sqlite", SQLITE_UP),
        ] {
            let pk_line = ddl
                .lines()
                .find(|l| l.contains("PRIMARY KEY"))
                .unwrap_or_else(|| panic!("{name} declares no primary key"));
            assert!(
                pk_line.contains("run_id"),
                "{name}'s primary key must be run_id, found: {pk_line}",
            );
        }
    }

    /// Column names of a `CREATE TABLE` body, in declaration order.
    ///
    /// Skips a leading token of `PRIMARY`/`CONSTRAINT`/`FOREIGN`/`UNIQUE`/
    /// `KEY`/`INDEX` rather than matching the literal string `"PRIMARY KEY"`,
    /// the same exclusion set `m20260813_000003_initial`'s `columns_by_table`
    /// uses. **Fix-round 1:** widened from just `"PRIMARY KEY"` because the
    /// table-level `CONSTRAINT ... FOREIGN KEY` / `FOREIGN KEY (...)` lines
    /// added to `MYSQL_UP`/`SQLITE_UP` in this round are not column
    /// definitions either, and the narrower check let `FOREIGN`/`CONSTRAINT`
    /// leak in as a spurious sixth "column".
    fn column_names(ddl: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in ddl.lines() {
            let line = line.trim().trim_end_matches(',');
            if line.is_empty()
                || line.starts_with("--")
                || line.starts_with("CREATE")
                || line.starts_with(')')
            {
                continue;
            }
            let Some(ident) = line.split_whitespace().next() else {
                continue;
            };
            if matches!(
                ident.to_uppercase().as_str(),
                "PRIMARY" | "CONSTRAINT" | "FOREIGN" | "UNIQUE" | "KEY" | "INDEX"
            ) {
                continue;
            }
            out.push(ident.to_owned());
        }
        out
    }

    /// Deterministic fixture UUIDs, matching the sibling migration test
    /// modules. `Uuid::new_v4` would depend on a cargo feature this crate does
    /// not ask for, and would make a failure unreproducible.
    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 2026-08-13 00:00:00 UTC, the same fixture instant the sibling migration
    /// test modules and `domain` use.
    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
    }

    /// In-memory `SQLite` with **every** migration this gear declares applied,
    /// through the real [`super::super::Migrator`]. Copied verbatim from the
    /// sibling migration test modules (e.g.
    /// `m20260818_000006_collect_target`'s `migrated_db`).
    ///
    /// `max_connections(1)` is load-bearing: each `SQLite` `:memory:`
    /// connection is its own database, so a larger pool would let a query land
    /// on one where the migration never ran.
    async fn sqlite_migrated() -> DatabaseConnection {
        let mut opts = ConnectOptions::new("sqlite::memory:");
        opts.max_connections(1).min_connections(1);
        let conn = Database::connect(opts)
            .await
            .expect("failed to connect to in-memory sqlite database");
        conn.execute_unprepared("PRAGMA foreign_keys = ON;")
            .await
            .expect("failed to enable sqlite foreign key enforcement");

        let manager = SchemaManager::new(&conn);
        for migration in super::super::Migrator::migrations() {
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-runs migrations");
        }
        conn
    }

    /// A minimal-but-complete `qa_runs` row, so a log row has a parent to
    /// reference. Every field is named explicitly rather than
    /// `..Default::default()`, for the reason `m20260818_000005_case_fidelity`
    /// gives: a wildcard would silently absorb the next column added to
    /// `run::Model` without a compile error.
    async fn seed_run(conn: &DatabaseConnection, run_id: Uuid, tenant: Uuid) {
        run::ActiveModel {
            id: ActiveValue::Set(run_id),
            tenant_id: ActiveValue::Set(tenant),
            name: ActiveValue::Set("smoke-1".to_owned()),
            run_kind: ActiveValue::Set("plan".to_owned()),
            target_repo_id: ActiveValue::Set(None),
            target_path: ActiveValue::Set(None),
            target_test_file: ActiveValue::Set(None),
            target_custom_plan_id: ActiveValue::Set(None),
            target_collect_url: ActiveValue::Set(None),
            environment_id: ActiveValue::Set(None),
            test_version: ActiveValue::Set(None),
            app_version: ActiveValue::Set(None),
            app_build: ActiveValue::Set(None),
            state: ActiveValue::Set("dispatching".to_owned()),
            resolved_exclusive: ActiveValue::Set(false),
            exclusive_tier: ActiveValue::Set("default".to_owned()),
            is_validation: ActiveValue::Set(false),
            parameters: ActiveValue::Set(serde_json::json!([])),
            include_tags: ActiveValue::Set(serde_json::json!([])),
            exclude_tags: ActiveValue::Set(serde_json::json!([])),
            source: ActiveValue::Set("manual".to_owned()),
            schedule_id: ActiveValue::Set(None),
            bundle_ids: ActiveValue::Set(serde_json::json!([])),
            execution_ref: ActiveValue::Set(None),
            log_storage_ref: ActiveValue::Set(None),
            timeout_at: ActiveValue::Set(None),
            started_at: ActiveValue::Set(None),
            finished_at: ActiveValue::Set(None),
            error: ActiveValue::Set(None),
            passed: ActiveValue::Set(0),
            failed: ActiveValue::Set(0),
            skipped: ActiveValue::Set(0),
            in_progress: ActiveValue::Set(0),
            total: ActiveValue::Set(0),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();
    }

    /// Deleting a run deletes its log. **The cascade is the entire retention
    /// policy** (spec D-RLP-1), so this is the test that stops an orphaned log
    /// surviving its run forever.
    ///
    /// The explicit `PRAGMA foreign_keys = ON` is **not** what turns
    /// enforcement on here — this connection goes through `sea_orm`'s own
    /// `Database::connect`, not `toolkit-db`, and `sqlx`'s `SQLite` driver
    /// already enables foreign keys on every connection it opens
    /// (`m20260813_000003_initial`'s `migrated_db`, corrected 2026-08-13,
    /// records the same for its own identical setup). Deleting this line
    /// would leave the test green. Setting it to `OFF` *does* turn the test
    /// red, because that explicit statement overrides the driver's default —
    /// which is the break-test below, and the only honest one.
    #[tokio::test]
    async fn deleting_a_run_deletes_its_log() {
        let conn = sqlite_migrated().await;
        conn.execute_unprepared("PRAGMA foreign_keys = ON;")
            .await
            .unwrap();

        let tenant = uuid(1);
        let run_id = uuid(2);
        seed_run(&conn, run_id, tenant).await;

        run_log::ActiveModel {
            run_id: ActiveValue::Set(run_id),
            tenant_id: ActiveValue::Set(tenant),
            text: ActiveValue::Set("[node] hello\n".to_owned()),
            lines: ActiveValue::Set(1),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(&conn)
        .await
        .unwrap();

        assert!(
            run_log::Entity::find_by_id(run_id)
                .one(&conn)
                .await
                .unwrap()
                .is_some(),
            "precondition: the log row must exist before the delete",
        );

        run::Entity::delete_by_id(run_id).exec(&conn).await.unwrap();

        assert!(
            run_log::Entity::find_by_id(run_id)
                .one(&conn)
                .await
                .unwrap()
                .is_none(),
            "the log must not survive its run",
        );
    }
}
