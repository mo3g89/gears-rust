//! Adds the three per-schedule Slack notification columns `qa_schedules` needs
//! in order to own the settings qa-insights will read (decision D9).
//!
//! A **fifth migration file**, not an edit to `m20260813_000004_schedules`, for
//! the reason `mod.rs` states: an edit would never be applied to a deployment
//! that already ran the earlier version. `ALTER TABLE` in a new file is the only
//! shape that reaches a deployed schema.
//!
//! **The ordinal is `000007`, not the `000006` the plan named.** Task 5 took
//! `m20260818_000006_collect_target`; the plan anticipated the collision and
//! said to pick the next free ordinal.
//!
//! # What is being ported
//!
//! The source system stores exactly these three values as `CronWorkflow`
//! annotations and edits them through one endpoint,
//! `manager/src/routes/schedules.rs::api_update_notifications` (the handler
//! begins at `schedules.rs:833`, registered at `routes/mod.rs:95-98`). Its form
//! is `UpdateScheduleNotificationsForm` (`manager/src/models.rs:291-299`) —
//! `enabled`, `channel`, `events` — and the values land on
//! `CreateScheduleForm::{slack_notifications_enabled, slack_channel,
//! slack_notification_events}` (`manager/src/models.rs:190-194`), which is where
//! this migration's column names come from.
//!
//! **Verified, and one of the plan's citations was wrong.** The plan lists the
//! event vocabulary as `Pending`, `InProgress`, `Succeeded`, `Failed`, `Error`,
//! `Skipped` — those are the Rust *variant* names. The enum carries
//! `#[serde(rename_all = "snake_case")]` (`manager/src/models.rs:949-959`), so
//! the strings that cross the wire and land in this column are `pending`,
//! `in_progress`, `succeeded`, `failed`, `error`, `skipped`. Legacy asserts it
//! itself: `scheduled_run_event_serializes_to_snake_case`
//! (`manager/src/models.rs:940-946`) and the UI's union type
//! (`manager-ui/src/api/types.ts:761-767`). The distinction is not cosmetic —
//! `InProgress` lowercases to `inprogress`, which legacy's own
//! `ScheduledRunNotificationEvent::parse` rejects.
//!
//! ## The delete-and-recreate does not port, and that is not a dropped behaviour
//!
//! Legacy's handler **deletes the `CronWorkflow` and recreates it** from a
//! carried-forward `CreateScheduleForm` rather than patching the annotation. Its
//! own doc comment says why, and the reason is entirely about annotation
//! storage: the values that take effect at trigger time are *baked into the
//! `CronWorkflow`'s embedded trigger script*, so a plain annotation patch left the
//! script stale and the edited settings silently never applied. Recreating the
//! object is how the script gets regenerated.
//!
//! qa-runs has no embedded script and no second copy of these values: a schedule
//! is a row in this table, the settings are three of its columns, and the run
//! path reads the row. So the port is a plain `UPDATE` of three columns. Nothing
//! is lost, because there is no derived artifact to regenerate — and the whole
//! failure mode legacy's comment then has to disclose (*"the recreate itself can
//! still fail … in which case the schedule is still gone with no rollback"*)
//! does not exist here either.
//!
//! The half of legacy's handler that **is** behaviour is its `exclusive`
//! carry-forward — *"this endpoint edits Slack settings only, so a schedule
//! pinned to exclusive (or to parallel) must come back pinned the same way"*
//! (`manager/src/routes/schedules.rs:854-856`).
//! That is preserved by `SchedulesRepository::update_notifications` touching
//! three columns and no others, and pinned by
//! `schedules_tests::updating_notification_settings_leaves_every_other_field_untouched`.
//!
//! # The JSON default: **one** strategy, stated once
//!
//! `slack_notification_events` is `NOT NULL` **with a database default of
//! `'[]'`**. It is never nullable, `NULL` is never a legal value in it, and no
//! mapper anywhere treats `NULL` as an empty list. The plan offered a second
//! option — nullable, with `NULL` read as empty — and it is **not** taken.
//!
//! `MySQL` rejects a *literal* default on a `JSON` column but accepts the
//! parenthesised *expression* form (`DEFAULT ('[]')`, 8.0.13+), which is exactly
//! what the sibling `include_tags`, `exclude_tags` and `parameters` columns on
//! this same table already do (`m20260813_000004_schedules`, `MYSQL_UP`). Doing
//! anything else here would give one table two readings of "an empty JSON array
//! column", which is the drift that migration's own comment was written to
//! prevent.
//!
//! # Additivity
//!
//! Three new columns, none of which any existing row has. Two carry a default
//! that **is** what every pre-existing row already means — notifications off,
//! no events — and the third is nullable, which is what "no channel override"
//! already means. So there is no backfill and the migration is safe against a
//! populated table. No column changes name, type or nullability.
//!
//! `VARCHAR(255)` for the channel, matching `qa_schedules.name` and legacy's own
//! `slack_channel` column width for the global setting
//! (`manager/migrations/001_initial.sql`, `notification_settings`): a Slack
//! channel name is bounded at 80 characters by Slack itself, so 255 is slack
//! (no pun) without being unbounded, and a bound is what keeps an absurd value
//! out of the row every schedule listing reads.
//!
//! # What the tests below do and do not reach
//!
//! `SQLITE_UP` and `DOWN` run on every `cargo test`. `PG_UP` is executed by the
//! `integration` tier against a real Postgres; `MYSQL_UP` is executed by
//! **nothing** in this workspace, so for it the parity test below is the whole
//! control — and, unlike `m20260818_000006_collect_target`'s, it cannot compare
//! the column *definitions* verbatim, because these three columns genuinely have
//! per-dialect types (`JSONB` / `JSON` / `TEXT`) and per-dialect defaults. It
//! therefore compares the (table, column, nullability) triples across all three
//! and asserts the per-dialect type map explicitly, so a column forgotten in one
//! blob and a type silently changed in one blob both fail.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &str = r"
ALTER TABLE qa_schedules ADD COLUMN IF NOT EXISTS slack_notifications_enabled BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE qa_schedules ADD COLUMN IF NOT EXISTS slack_channel VARCHAR(255) NULL;
ALTER TABLE qa_schedules ADD COLUMN IF NOT EXISTS slack_notification_events JSONB NOT NULL DEFAULT '[]';
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_schedules ADD COLUMN slack_notifications_enabled BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE qa_schedules ADD COLUMN slack_channel VARCHAR(255) NULL;
ALTER TABLE qa_schedules ADD COLUMN slack_notification_events JSON NOT NULL DEFAULT ('[]');
";

/// `INTEGER` and `0` rather than `BOOLEAN`/`FALSE`, matching the `enabled`
/// column this table already carries in `SQLITE_UP`
/// (`m20260813_000004_schedules`): one table, one spelling for a boolean.
const SQLITE_UP: &str = r"
ALTER TABLE qa_schedules ADD COLUMN slack_notifications_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE qa_schedules ADD COLUMN slack_channel VARCHAR(255) NULL;
ALTER TABLE qa_schedules ADD COLUMN slack_notification_events TEXT NOT NULL DEFAULT '[]';
";

/// Reverse order of `*_UP`, and one blob for all three dialects.
///
/// Plain `DROP COLUMN`, without `IF EXISTS`, for the reason
/// `m20260818_000005_case_fidelity`'s `DOWN` gives: Postgres accepts the guard,
/// `MySQL` and `SQLite` do not, and a per-dialect `DOWN` for a guard that only
/// matters when `down()` runs twice is not worth three blobs that can drift.
/// None of the three columns is indexed or part of a key, which is the condition
/// `SQLite`'s `DROP COLUMN` imposes.
const DOWN: &str = r"
ALTER TABLE qa_schedules DROP COLUMN slack_notification_events;
ALTER TABLE qa_schedules DROP COLUMN slack_channel;
ALTER TABLE qa_schedules DROP COLUMN slack_notifications_enabled;
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
        conn.execute_unprepared(DOWN).await?;
        Ok(())
    }
}

/// Schema tests for the three notification columns.
///
/// The sibling migrations explain why a module like this exists at all:
/// `SeaORM` entities name their table and every column as **runtime strings**,
/// so a typo in `entity/schedule.rs` or in a blob here compiles cleanly and
/// fails at query time. Only a query links the two.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{
        ActiveModelTrait, ActiveValue, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
        DatabaseConnection, EntityTrait, QueryFilter,
    };
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::infra::storage::entity::schedule;

    /// Deterministic fixture UUIDs; `Uuid::new_v4` would need a cargo feature
    /// this crate does not ask for and would make a failure unreproducible.
    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 2026-08-13 00:00:00 UTC, the fixture instant the sibling modules use.
    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
    }

    /// The `(table, name, type, nullable)` of each `ADD COLUMN` in a blob, in
    /// order, with Postgres's `IF NOT EXISTS` normalized away.
    ///
    /// That guard is one of the two differences the three blobs are *allowed* to
    /// have — `MySQL` has no such clause — so it is stripped here rather than
    /// being left to make every comparison unequal, which would turn the parity
    /// test into a test of nothing. The other allowed difference is the type and
    /// default, which is why those are returned separately instead of being
    /// compared as one opaque definition string.
    fn added_columns(ddl: &str) -> Vec<(String, String, String, bool)> {
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
            let definition = definition.trim();
            let ty = definition
                .split_whitespace()
                .next()
                .expect("a definition starts with a type")
                .to_owned();
            // `NOT NULL` is spelled in every one of these blobs, and so is the
            // bare `NULL`, so nullability is read rather than inferred from an
            // omission.
            let nullable = !definition.contains("NOT NULL");
            out.push((table.to_owned(), name.to_owned(), ty, nullable));
        }
        out
    }

    /// The three dialects add the same columns, to the same table, in the same
    /// order, with the same nullability.
    ///
    /// This is the *only* control over `MYSQL_UP`, which nothing in this
    /// workspace executes: a column added to two blobs and forgotten in the
    /// third leaves every other test in this file green, because they all run
    /// against `SQLite`.
    #[test]
    fn every_dialect_adds_the_same_notification_columns() {
        let names = |ddl: &str| {
            added_columns(ddl)
                .into_iter()
                .map(|(t, n, _, nullable)| (t, n, nullable))
                .collect::<Vec<_>>()
        };

        let pg = names(super::PG_UP);
        assert_eq!(
            pg,
            vec![
                (
                    "qa_schedules".to_owned(),
                    "slack_notifications_enabled".to_owned(),
                    false
                ),
                ("qa_schedules".to_owned(), "slack_channel".to_owned(), true),
                (
                    "qa_schedules".to_owned(),
                    "slack_notification_events".to_owned(),
                    false
                ),
            ],
            "the parser lost a column, or a nullability changed; every comparison \
             below would then be over the wrong set"
        );
        assert_eq!(
            pg,
            names(super::MYSQL_UP),
            "PG_UP and MYSQL_UP add different columns"
        );
        assert_eq!(
            pg,
            names(super::SQLITE_UP),
            "PG_UP and SQLITE_UP add different columns"
        );
    }

    /// The per-dialect types are the ones intended, spelled out.
    ///
    /// The sibling `m20260818_000006_collect_target` compares whole definitions
    /// across dialects, which it can because its one column is `VARCHAR(1024)`
    /// everywhere. These three are not: `JSONB` on Postgres is `JSON` on `MySQL`
    /// and `TEXT` on `SQLite`, and the boolean is `INTEGER` on `SQLite`. Without
    /// this assertion, "the types may differ" would mean *any* type passes,
    /// including a `slack_notification_events VARCHAR(255)` that silently
    /// truncates an operator's event list on Postgres.
    #[test]
    fn each_dialect_declares_the_intended_type_for_each_column() {
        let types = |ddl: &str| {
            added_columns(ddl)
                .into_iter()
                .map(|(_, n, ty, _)| (n, ty))
                .collect::<Vec<_>>()
        };

        let expected = |b: &str, j: &str| {
            vec![
                ("slack_notifications_enabled".to_owned(), b.to_owned()),
                ("slack_channel".to_owned(), "VARCHAR(255)".to_owned()),
                ("slack_notification_events".to_owned(), j.to_owned()),
            ]
        };

        assert_eq!(types(super::PG_UP), expected("BOOLEAN", "JSONB"));
        assert_eq!(types(super::MYSQL_UP), expected("BOOLEAN", "JSON"));
        assert_eq!(types(super::SQLITE_UP), expected("INTEGER", "TEXT"));
    }

    /// The JSON column is `NOT NULL` **with a default** in every dialect, and
    /// the default is an empty array.
    ///
    /// This is the module doc's "one strategy" made checkable. The rejected
    /// alternative — a nullable column with `NULL` read as empty — would show up
    /// here as a missing `NOT NULL` or a missing `DEFAULT`, and it is precisely
    /// the shape that lets a mapper acquire a second, silent reading of "no
    /// events".
    ///
    /// `MySQL`'s parenthesised form is the reason the assertion is on
    /// `DEFAULT` + `[]` rather than on the exact literal.
    #[test]
    fn the_events_column_is_not_null_with_an_empty_array_default_everywhere() {
        for (dialect, ddl) in [
            ("PG_UP", super::PG_UP),
            ("MYSQL_UP", super::MYSQL_UP),
            ("SQLITE_UP", super::SQLITE_UP),
        ] {
            let line = ddl
                .lines()
                .find(|l| l.contains("slack_notification_events"))
                .unwrap_or_else(|| panic!("{dialect} has no events column"));
            assert!(line.contains("NOT NULL"), "{dialect}: {line}");
            assert!(line.contains("DEFAULT"), "{dialect}: {line}");
            assert!(line.contains("[]"), "{dialect}: {line}");
        }
    }

    /// `DOWN` drops exactly what the `*_UP` blobs add, in the reverse order.
    #[test]
    fn down_drops_exactly_what_up_adds() {
        let added: Vec<(String, String)> = added_columns(super::PG_UP)
            .into_iter()
            .map(|(t, n, _, _)| (t, n))
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

        assert_eq!(dropped.len(), 3, "DOWN parser found {dropped:?}");
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
        conn.query_all_raw(Statement::from_string(
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

    /// The table gains all three columns with the nullability the blobs declare,
    /// as the database itself reports it.
    ///
    /// Deliberately an assertion about the **table**, not about the migration
    /// having run: `Migrator::migrations()` returning this migration proves only
    /// that it is registered.
    #[tokio::test]
    async fn qa_schedules_gains_all_three_notification_columns() {
        let conn = migrated_db().await;
        let specs = column_specs(&conn, "qa_schedules").await;

        for (name, notnull) in [
            ("slack_notifications_enabled", true),
            ("slack_channel", false),
            ("slack_notification_events", true),
        ] {
            let found = specs
                .iter()
                .find(|(n, _, _)| n == name)
                .unwrap_or_else(|| panic!("{name} missing from qa_schedules: {specs:?}"));
            assert_eq!(
                found.2, notnull,
                "qa_schedules.{name} has the wrong nullability: {specs:?}"
            );
        }
    }

    /// A row written **before** this migration's columns existed still reads,
    /// and reads as "notifications off, no channel, no events".
    ///
    /// This is the additivity claim that matters operationally: the defaults are
    /// not decoration, they are what every schedule already in a deployed
    /// database will have on the morning after this ships. Raw SQL, because
    /// `SeaORM` names every column in its `INSERT` and there is no way through
    /// the entity to write a row that omits one.
    ///
    /// `INSERT ... SELECT` off a row `SeaORM` itself wrote, rather than a
    /// literal `VALUES` list, for the reason `m20260818_000005_case_fidelity`'s
    /// equivalent test gives: `sqlx` encodes a `Uuid` into `SQLite` as a
    /// **blob**, not as hyphenated text, so a hand-written id is inserted
    /// successfully and then never found again — a test that would pass by
    /// finding nothing if it asserted on absence, and fail confusingly here.
    /// `randomblob(16)` is a fresh primary key in the right encoding, and `name`
    /// is the handle this test reads back by.
    #[tokio::test]
    async fn a_row_predating_the_columns_reads_as_notifications_off() {
        let conn = migrated_db().await;
        seeded_schedule(&conn, uuid(0x51), "seed").await;

        conn.execute_unprepared(
            "INSERT INTO qa_schedules \
             (id, tenant_id, name, run_kind, target_repo_id, target_path, cron, \
              exclusive_choice, enabled, include_tags, exclude_tags, parameters, \
              created_at, updated_at) \
             SELECT randomblob(16), tenant_id, 'legacy-row', run_kind, target_repo_id, \
              target_path, cron, exclusive_choice, enabled, include_tags, exclude_tags, \
              parameters, created_at, updated_at FROM qa_schedules WHERE name = 'seed'",
        )
        .await
        .expect("a pre-migration-shaped INSERT must still be legal");

        let stored = schedule::Entity::find()
            .filter(schedule::Column::Name.eq("legacy-row"))
            .one(&conn)
            .await
            .unwrap()
            .expect("the row just inserted must be readable");
        assert!(
            !stored.slack_notifications_enabled,
            "an existing schedule must not start notifying anybody"
        );
        assert_eq!(stored.slack_channel, None);
        assert_eq!(stored.slack_notification_events, serde_json::json!([]));
    }

    /// Insert one schedule through the entity, with **every** column named.
    ///
    /// The struct literal names every field for the reason
    /// `m20260818_000005_case_fidelity` argues: a `..Default::default()` absorbs
    /// the next column added after it without a compile error. All three
    /// notification columns are set to distinct non-default values, so a caller
    /// asserting on them cannot pass by reading a default back.
    async fn seeded_schedule(conn: &DatabaseConnection, id: Uuid, name: &str) {
        schedule::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(uuid(2)),
            name: ActiveValue::Set(name.to_owned()),
            run_kind: ActiveValue::Set("plan".to_owned()),
            target_repo_id: ActiveValue::Set(Some(uuid(0x10))),
            target_path: ActiveValue::Set(Some("plans/smoke.yaml".to_owned())),
            target_test_file: ActiveValue::Set(None),
            target_custom_plan_id: ActiveValue::Set(None),
            target_collect_url: ActiveValue::Set(None),
            environment_id: ActiveValue::Set(None),
            branch: ActiveValue::Set(Some("main".to_owned())),
            cron: ActiveValue::Set("0 3 * * *".to_owned()),
            exclusive_choice: ActiveValue::Set("auto".to_owned()),
            enabled: ActiveValue::Set(true),
            include_tags: ActiveValue::Set(serde_json::json!([])),
            exclude_tags: ActiveValue::Set(serde_json::json!([])),
            parameters: ActiveValue::Set(serde_json::json!([])),
            slack_notifications_enabled: ActiveValue::Set(true),
            slack_channel: ActiveValue::Set(Some("#qa-alerts".to_owned())),
            slack_notification_events: ActiveValue::Set(serde_json::json!(["failed", "error"])),
            last_fired_tick: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();
    }

    /// All three columns round-trip through the entity, at **distinct
    /// non-default values**.
    ///
    /// This is the assertion that links the entity's three new field names to
    /// the column names above — the link `cargo build` does not make, because
    /// both sides are runtime strings. Non-default values in all three, because
    /// a fixture that left one at its default could not tell a written column
    /// from a dropped one.
    #[tokio::test]
    async fn the_three_notification_columns_round_trip_through_the_schedule_entity() {
        let conn = migrated_db().await;
        let id = uuid(1);
        seeded_schedule(&conn, id, "nightly").await;

        let stored = schedule::Entity::find_by_id(id)
            .one(&conn)
            .await
            .unwrap()
            .expect("the row just inserted must be readable");
        assert!(stored.slack_notifications_enabled);
        assert_eq!(stored.slack_channel.as_deref(), Some("#qa-alerts"));
        assert_eq!(
            stored.slack_notification_events,
            serde_json::json!(["failed", "error"])
        );
    }

    /// Every pre-existing column of `qa_schedules` still has its **name,
    /// declared type and nullability**.
    ///
    /// The whole rule for this task is *additive only*, and "additive" is a
    /// claim about what did not change. Comparing names alone would wave through
    /// a migration that retyped `cron` or made `platform_id` `NOT NULL`, so this
    /// reads what the database concluded from the DDL.
    #[tokio::test]
    async fn the_pre_existing_schedule_columns_are_untouched() {
        let conn = migrated_db().await;
        let specs = column_specs(&conn, "qa_schedules").await;

        let expected = [
            ("id", "TEXT", true),
            ("tenant_id", "TEXT", true),
            ("name", "TEXT", true),
            ("run_kind", "TEXT", true),
            ("target_repo_id", "TEXT", false),
            ("target_path", "TEXT", false),
            ("target_test_file", "TEXT", false),
            ("target_custom_plan_id", "TEXT", false),
            ("target_collect_url", "VARCHAR(1024)", false),
            ("platform_id", "TEXT", false),
            ("branch", "TEXT", false),
            ("cron", "TEXT", true),
            ("exclusive_choice", "TEXT", true),
            ("enabled", "INTEGER", true),
            ("include_tags", "TEXT", true),
            ("exclude_tags", "TEXT", true),
            ("parameters", "TEXT", true),
            ("last_fired_tick", "TEXT", false),
            ("created_at", "TEXT", true),
            ("updated_at", "TEXT", true),
        ];
        for (name, ty, notnull) in expected {
            let found = specs
                .iter()
                .find(|(n, _, _)| n == name)
                .unwrap_or_else(|| panic!("{name} missing from qa_schedules: {specs:?}"));
            assert_eq!(
                (found.1.as_str(), found.2),
                (ty, notnull),
                "qa_schedules.{name} changed type or nullability: no longer additive"
            );
        }
        assert_eq!(
            specs.len(),
            expected.len() + 3,
            "qa_schedules must gain exactly three columns: {specs:?}"
        );
    }

    /// `down()` removes all three columns and leaves the table standing.
    ///
    /// `super::Migration` directly, **not** a loop over `Migrator::migrations()`
    /// — that loop is in declaration order and `down()` runs in the reverse of
    /// it, so the loop would drop Phase A's tables out from under this
    /// migration.
    #[tokio::test]
    async fn the_notification_down_migration_drops_all_three_columns() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();

        let specs = column_specs(&conn, "qa_schedules").await;
        for name in [
            "slack_notifications_enabled",
            "slack_channel",
            "slack_notification_events",
        ] {
            assert!(
                !specs.iter().any(|(n, _, _)| n == name),
                "{name} survived down(): {specs:?}"
            );
        }
        conn.execute_unprepared("SELECT 1 FROM qa_schedules")
            .await
            .expect("down() must not drop the table");
    }
}
