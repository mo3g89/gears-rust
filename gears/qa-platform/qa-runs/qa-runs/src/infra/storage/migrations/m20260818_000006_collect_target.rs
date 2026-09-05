//! Adds the one target column the `Collect` run kind needs, to both tables
//! that store a target.
//!
//! A **fourth migration file**, not an edit to an older one, for the reason
//! `mod.rs` states: an edit would never be applied to a deployment that already
//! ran the earlier version. `ALTER TABLE` in a new file is the only shape that
//! reaches a deployed schema.
//!
//! # Why a column at all, when the run kind is what is new
//!
//! `qa_runs` and `qa_schedules` store a [`qa_runs_sdk::RunTarget`] flattened
//! into four columns discriminated by `run_kind`
//! (`infra::storage::mapper::target_to_columns`). `RunTarget::Collect` carries a
//! repository and a **report URL**, and only the first of those has a column.
//!
//! Its third input, the branch, deliberately has no column here: it rides on
//! `LaunchRequest::branch` and is recorded as `qa_runs.test_version`, which is
//! parity spec §3.4 rule 6 (*"the branch label **is** the recorded test
//! version"*). `RunTarget::Collect`'s own doc argues why a second home for it
//! would be a defect rather than a convenience.
//!
//! ## Rejected: reuse `target_path`
//!
//! `target_path` is `NULL` for a collect row and is exactly the right width, so
//! putting the URL there costs no DDL. Declined. `target_path` means *"a
//! `plan.yaml` path inside the repository"* everywhere else — in the entity
//! docs, in `m20260813_000003_initial`'s column comment, and in
//! `target_from_columns`' arm for every other kind — and a column whose meaning
//! depends on a sibling column's value is the shape that makes a
//! `run_kind`-blind reader silently wrong rather than loudly wrong. The gear has
//! two such readers already: `OData` filtering (`infra::storage::odata`) and
//! the REST DTO. A distinct column makes "this row has a collect URL" answerable
//! without consulting `run_kind` at all.
//!
//! ## Why `qa_schedules` gets it too
//!
//! Not because a collect *schedule* is a feature: it is not one, legacy has
//! none (its collect cycle is an hourly poller, `collect.rs:154-179`), and
//! nothing in this gear creates one. It is because `qa_schedules` shares the
//! run's target codec — `schedule_to_sdk` calls the same
//! `target_from_columns`, and `mapper`'s own comment says why: *"a schedule's
//! whole purpose is to produce a run with this target, and two codecs could
//! drift into two readings of one column set."* One codec, one column set. The
//! alternative is a `target_to_columns` whose fifth output the schedule writer
//! silently discards, which is precisely the silent-drop shape this subsystem
//! keeps finding.
//!
//! # Additivity
//!
//! One new nullable column per table. No column changes name, type or
//! nullability, and `NULL` is what every pre-existing row already means, so
//! there is no backfill and the migration is safe against a populated table.
//!
//! `VARCHAR(1024)`, matching `target_path` — the widest thing either table
//! stores per target — rather than `TEXT`: it is a URL the control plane's
//! caller supplies, and a bound is what keeps an absurd one out of the row that
//! every listing reads.
//!
//! # What the tests below do and do not reach
//!
//! `SQLITE_UP` and `DOWN` run on every `cargo test`. `PG_UP` is executed by the
//! `integration` tier against a real Postgres; `MYSQL_UP` is executed by
//! nothing in this workspace, so for it the parity test below is the whole
//! control — it compares *declarations*, not dialect acceptance. The sibling
//! migrations argue this at length; not repeated here.
//!
//! **`MySQL` has no `ADD COLUMN IF NOT EXISTS`.** That is the single difference
//! between `PG_UP` and `MYSQL_UP`, and the parity test normalizes it away
//! rather than letting it hide a real divergence.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &str = r"
ALTER TABLE qa_runs ADD COLUMN IF NOT EXISTS target_collect_url VARCHAR(1024) NULL;
ALTER TABLE qa_schedules ADD COLUMN IF NOT EXISTS target_collect_url VARCHAR(1024) NULL;
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_runs ADD COLUMN target_collect_url VARCHAR(1024) NULL;
ALTER TABLE qa_schedules ADD COLUMN target_collect_url VARCHAR(1024) NULL;
";

const SQLITE_UP: &str = r"
ALTER TABLE qa_runs ADD COLUMN target_collect_url VARCHAR(1024) NULL;
ALTER TABLE qa_schedules ADD COLUMN target_collect_url VARCHAR(1024) NULL;
";

/// Reverse order of `*_UP`, and one blob for all three dialects.
///
/// Plain `DROP COLUMN`, without `IF EXISTS`, for the reason
/// `m20260818_000005_case_fidelity`'s `DOWN` gives: Postgres accepts the guard,
/// `MySQL` and `SQLite` do not, and a per-dialect `DOWN` for a guard that only
/// matters when `down()` runs twice is not worth three blobs that can drift.
/// Neither column is indexed or part of a key, which is the condition `SQLite`'s
/// `DROP COLUMN` imposes.
const DOWN: &str = r"
ALTER TABLE qa_schedules DROP COLUMN target_collect_url;
ALTER TABLE qa_runs DROP COLUMN target_collect_url;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => PG_UP,
            sea_orm::DatabaseBackend::MySql => MYSQL_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(DOWN).await?;
        Ok(())
    }
}

/// Schema tests for the collect target column.
///
/// The sibling migrations explain why a module like this exists at all:
/// `SeaORM` entities name their table and every column as **runtime strings**,
/// so a typo in `entity/run.rs` or in a blob here compiles cleanly and fails at
/// query time. Only a query links the two.
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

    use crate::infra::storage::entity::run;

    /// Deterministic fixture UUIDs; `Uuid::new_v4` would need a cargo feature
    /// this crate does not ask for and would make a failure unreproducible.
    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 2026-08-13 00:00:00 UTC, the fixture instant the sibling modules use.
    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
    }

    /// The `(table, name, definition)` of each `ADD COLUMN` in a blob, in order,
    /// with Postgres's `IF NOT EXISTS` normalized away.
    ///
    /// That guard is the one difference the three blobs are *allowed* to have —
    /// `MySQL` has no such clause — so it is stripped here rather than being
    /// left to make the comparison always-unequal, which would turn the parity
    /// test into a test of nothing.
    fn added_columns(ddl: &str) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for line in ddl.lines() {
            let line = line.trim().trim_end_matches(';');
            let Some(rest) = line.strip_prefix("ALTER TABLE ") else {
                continue;
            };
            let (table, rest) = rest
                .split_once(" ADD COLUMN ")
                .expect("an ALTER in these blobs is an ADD COLUMN");
            let rest = rest.strip_prefix("IF NOT EXISTS ").unwrap_or(rest);
            let (name, definition) = rest
                .split_once(' ')
                .expect("an ADD COLUMN clause has a name and a type");
            out.push((
                table.to_owned(),
                name.to_owned(),
                definition.trim().to_owned(),
            ));
        }
        out
    }

    /// The three dialects add the same columns, to the same tables, in the same
    /// order, with the same types and nullability.
    ///
    /// This is the *only* control over `MYSQL_UP`, which nothing in this
    /// workspace executes: a column added to two blobs and forgotten in the
    /// third leaves every other test in this file green, because they all run
    /// against `SQLite`.
    #[test]
    fn every_dialect_adds_the_same_collect_column() {
        let pg = added_columns(super::PG_UP);
        let my = added_columns(super::MYSQL_UP);
        let sq = added_columns(super::SQLITE_UP);

        assert_eq!(
            pg.iter()
                .map(|(t, n, _)| (t.as_str(), n.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("qa_runs", "target_collect_url"),
                ("qa_schedules", "target_collect_url"),
            ],
            "the parser lost a column; every comparison below would then be \
             over the wrong set: {pg:?}"
        );
        assert_eq!(pg, my, "PG_UP and MYSQL_UP add different columns");
        assert_eq!(pg, sq, "PG_UP and SQLITE_UP add different columns");
    }

    /// `DOWN` drops exactly what the `*_UP` blobs add, in the reverse order.
    #[test]
    fn down_drops_exactly_what_up_adds() {
        let added: Vec<(String, String)> = added_columns(super::PG_UP)
            .into_iter()
            .map(|(t, n, _)| (t, n))
            .collect();
        let dropped: Vec<(String, String)> = super::DOWN
            .lines()
            .filter_map(|line| {
                let line = line.trim().trim_end_matches(';');
                let rest = line.strip_prefix("ALTER TABLE ")?;
                let (table, name) = rest.split_once(" DROP COLUMN ")?;
                Some((table.to_owned(), name.to_owned()))
            })
            .collect();

        assert_eq!(dropped.len(), 2, "DOWN parser found {dropped:?}");
        assert_eq!(
            dropped,
            added.iter().rev().cloned().collect::<Vec<_>>(),
            "DOWN must drop exactly the UP columns, in reverse order"
        );
    }

    /// In-memory `SQLite` with **every** migration this gear declares applied,
    /// through the real [`super::super::Migrator`].
    ///
    /// `max_connections(1)` is load-bearing: each `SQLite` `:memory:`
    /// connection is its own database, so a larger pool would let a query land
    /// on one where the migration never ran.
    async fn migrated_db() -> DatabaseConnection {
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

    /// Name, declared type and `NOT NULL` of each column, in declaration order.
    async fn column_specs(conn: &DatabaseConnection, table: &str) -> Vec<(String, String, bool)> {
        use sea_orm::Statement;
        conn.query_all(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            format!("SELECT name, type, \"notnull\" FROM pragma_table_info('{table}')"),
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.try_get::<String>("", "name").unwrap(),
                row.try_get::<String>("", "type").unwrap(),
                row.try_get::<i32>("", "notnull").unwrap() != 0,
            )
        })
        .collect()
    }

    /// Both tables gain the column, nullable, as the database itself reports it.
    ///
    /// Deliberately an assertion about the **tables**, not about the migration
    /// having run: `Migrator::migrations()` returning this migration proves only
    /// that it is registered.
    #[tokio::test]
    async fn both_target_tables_gain_a_nullable_collect_url() {
        let conn = migrated_db().await;
        for table in ["qa_runs", "qa_schedules"] {
            let specs = column_specs(&conn, table).await;
            let found = specs
                .iter()
                .find(|(name, _, _)| name == "target_collect_url")
                .unwrap_or_else(|| panic!("target_collect_url missing from {table}: {specs:?}"));
            assert!(
                !found.2,
                "{table}.target_collect_url must be nullable: a pre-existing row has no URL"
            );
        }
    }

    /// Every pre-existing target column still has its **name, declared type and
    /// nullability**.
    ///
    /// The whole rule for this task is *additive only*, and "additive" is a
    /// claim about what did not change. Comparing names alone would wave through
    /// a migration that retyped `target_path` or made `target_repo_id`
    /// `NOT NULL`, so this reads what the database concluded from the DDL.
    #[tokio::test]
    async fn the_pre_existing_target_columns_are_untouched() {
        let conn = migrated_db().await;
        let expected = [
            ("target_repo_id", "TEXT", false),
            ("target_path", "TEXT", false),
            ("target_test_file", "TEXT", false),
            ("target_custom_plan_id", "TEXT", false),
        ];
        for table in ["qa_runs", "qa_schedules"] {
            let specs = column_specs(&conn, table).await;
            for (name, ty, notnull) in expected {
                let found = specs
                    .iter()
                    .find(|(n, _, _)| n == name)
                    .unwrap_or_else(|| panic!("{name} missing from {table}"));
                assert_eq!(
                    (found.1.as_str(), found.2),
                    (ty, notnull),
                    "{table}.{name} changed type or nullability: no longer additive"
                );
            }
            assert_eq!(
                specs
                    .iter()
                    .filter(|(n, _, _)| n == "target_collect_url")
                    .count(),
                1,
                "{table} must gain exactly one collect column"
            );
        }
    }

    /// A run row carrying a collect target, written and read back through the
    /// entity.
    ///
    /// This is the assertion that links the entity's new field name to the
    /// column name above — the link `cargo build` does not make, because both
    /// sides are runtime strings. The struct literal names every field for the
    /// reason `m20260818_000005_case_fidelity` argues: a `..Default::default()`
    /// absorbs the next column added after it without a compile error.
    #[tokio::test]
    async fn a_collect_url_round_trips_through_the_run_entity() {
        let conn = migrated_db().await;
        let id = uuid(1);
        let url = "https://insights.example/qa/v1/collect/r/main";

        run::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(uuid(2)),
            name: ActiveValue::Set("collect-1".to_owned()),
            run_kind: ActiveValue::Set("collect".to_owned()),
            target_repo_id: ActiveValue::Set(Some(uuid(0x10))),
            target_path: ActiveValue::Set(None),
            target_test_file: ActiveValue::Set(None),
            target_custom_plan_id: ActiveValue::Set(None),
            target_collect_url: ActiveValue::Set(Some(url.to_owned())),
            environment_id: ActiveValue::Set(None),
            test_version: ActiveValue::Set(Some("main".to_owned())),
            app_version: ActiveValue::Set(None),
            app_build: ActiveValue::Set(None),
            state: ActiveValue::Set("running".to_owned()),
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
        .insert(&conn)
        .await
        .unwrap();

        let stored = run::Entity::find_by_id(id)
            .one(&conn)
            .await
            .unwrap()
            .expect("the row just inserted must be readable");
        assert_eq!(stored.target_collect_url.as_deref(), Some(url));
        assert_eq!(stored.run_kind, "collect");
    }

    /// `down()` removes the column from both tables and leaves every table
    /// standing.
    ///
    /// `super::Migration` directly, **not** a loop over `Migrator::migrations()`
    /// — that loop is in declaration order and `down()` runs in the reverse of
    /// it, so the loop would drop Phase A's tables out from under this
    /// migration.
    #[tokio::test]
    async fn the_collect_target_down_migration_drops_both_columns() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();

        for table in ["qa_runs", "qa_schedules"] {
            let specs = column_specs(&conn, table).await;
            assert!(
                !specs.iter().any(|(n, _, _)| n == "target_collect_url"),
                "target_collect_url survived down() on {table}: {specs:?}"
            );
            conn.execute_unprepared(&format!("SELECT 1 FROM {table}"))
                .await
                .expect("down() must not drop any table");
        }
    }
}
