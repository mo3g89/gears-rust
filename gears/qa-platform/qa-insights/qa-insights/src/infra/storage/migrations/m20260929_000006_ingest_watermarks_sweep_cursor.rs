//! Adds the reconcile sweep's **within-window resume cursor** to
//! `qa_ingest_watermarks`: three nullable columns, and nothing else.
//!
//! # What the columns are for
//!
//! The sweep starts each tick at `floor = mark - lookback`, deliberately behind
//! the mark. A lookback window holding more runs than one pass' whole page
//! budget lets a pass spend every page without ever reaching past the mark —
//! and [`WatermarkRepository::advance`](crate::domain::repos::WatermarkRepository::advance)
//! is monotonic, so the advance is a successful no-op, the next tick derives the
//! identical floor, and the pass repeats forever. Second review, finding #122;
//! `domain::service::reconcile`'s `MAX_PAGES_PER_SWEEP` carries the whole
//! account.
//!
//! These columns are what lets the next tick resume where the last one stopped
//! instead of re-deriving the same floor:
//!
//! * `sweep_cursor_at` and `sweep_cursor_run_id` are the `(finished_at, id)`
//!   key of the last run the previous pass **fully consumed** — the same keyset
//!   the walk already pages on (`qa_runs_sdk::FinishedRunCursor`), not a second
//!   notion of position.
//! * `sweep_cursor_floor` is the floor that pass was walking from. When this
//!   migration shipped, a cursor was used only by a pass that derived the
//!   *same* floor. Finding #122's residual A relaxed that to "the reading pass'
//!   floor lies inside `[sweep_cursor_floor, sweep_cursor_at]`", so a drain that
//!   moves the mark keeps its cursor; `domain::service::reconcile`'s
//!   `first_page_of` carries the rule. The columns did not change.
//!
//! All three are `NULL` together, and `NULL` is the ordinary steady state — the
//! sweep clears them on every pass that catches up. A deployment that has never
//! wedged never holds a non-null value here.
//!
//! # Why this is not a second watermark, and why the schema cannot make it one
//!
//! The monotonicity of `last_reconciled_finished_at` is enforced in a `WHERE`
//! clause (`infra::storage::watermark_sea_repo`'s header). These columns carry
//! **no** such predicate and deliberately cannot: a resume point that could only
//! move forward would be a second mark, the lookback would never be re-walked,
//! and a run written with a `finished_at` behind the cursor would be lost —
//! which is the exact defect the lookback exists to prevent. They are written by
//! a plain scoped `UPDATE` that also has to be able to write `NULL`.
//!
//! # Append-only, so a new file rather than an edit
//!
//! `m20260818_000001_initial` has already run on every deployment that needs
//! these columns, so an edit of it would reach none of them. Same rule, same
//! reason, as `m20260921_000002_smtp_credentials` and
//! `m20260929_000005_drop_ingest_watermarks_last_swept_at`, whose header states
//! it in the other direction.
//!
//! # No value is bound here, because no value is written here
//!
//! This migration is pure DDL. `m20260929_000004_run_completed_notification_cutoff`'s
//! header records what it cost to format a `Uuid` and an instant into SQL text —
//! `sqlx` stores a `Uuid` on `SQLite` as a 16-byte BLOB, so a literal passes on
//! Postgres and fails only on the unit tier — and the lesson is discharged here
//! by writing no rows at all. The values these columns hold are written by
//! `infra::storage::watermark_sea_repo::OrmWatermarkRepository::set_sweep_cursor`,
//! which builds a `sea_query` `UPDATE` and lets the driver encode them.
//!
//! **And nothing compares them in SQL.** `sweep_cursor_floor` is compared
//! against the pass' own floor in Rust, in `ReconcileService::sweep`, precisely
//! so that no dialect's opinion about how a `TEXT` column compares to a bound
//! `TIMESTAMPTZ` — or a `BLOB` to a `TEXT` — can decide whether a cursor is
//! honoured. The columns are read back through the entity and judged in memory.
//!
//! `sweep_cursor_run_id` is declared the way `id` and `tenant_id` on this same
//! table already are: `UUID` on Postgres, `TEXT` on `SQLite`. `SQLite` column
//! types are affinities rather than constraints, and a `TEXT`-affinity column
//! stores a BLOB as a BLOB, so the driver's own encoding round-trips — which is
//! what `the_cursor_columns_round_trip_through_the_entity` asserts rather than
//! assumes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Postgres. `IF NOT EXISTS` on each column keeps a re-run harmless.
const POSTGRES_UP: &str = r"
ALTER TABLE qa_ingest_watermarks ADD COLUMN IF NOT EXISTS sweep_cursor_at TIMESTAMPTZ NULL;
ALTER TABLE qa_ingest_watermarks ADD COLUMN IF NOT EXISTS sweep_cursor_run_id UUID NULL;
ALTER TABLE qa_ingest_watermarks ADD COLUMN IF NOT EXISTS sweep_cursor_floor TIMESTAMPTZ NULL;
";

/// `SQLite`. It has no `ADD COLUMN IF NOT EXISTS`; the migration table is what
/// makes a re-run not happen. `TEXT` for all three, matching the initial
/// migration's own `SQLITE_UP` for this table.
const SQLITE_UP: &str = r"
ALTER TABLE qa_ingest_watermarks ADD COLUMN sweep_cursor_at TEXT NULL;
ALTER TABLE qa_ingest_watermarks ADD COLUMN sweep_cursor_run_id TEXT NULL;
ALTER TABLE qa_ingest_watermarks ADD COLUMN sweep_cursor_floor TEXT NULL;
";

/// Dropping the three is lossless in the way that matters: a cursor is a
/// within-window resume point, so losing it costs one re-walk of a lookback
/// window and never a run.
const POSTGRES_DOWN: &str = r"
ALTER TABLE qa_ingest_watermarks DROP COLUMN IF EXISTS sweep_cursor_at;
ALTER TABLE qa_ingest_watermarks DROP COLUMN IF EXISTS sweep_cursor_run_id;
ALTER TABLE qa_ingest_watermarks DROP COLUMN IF EXISTS sweep_cursor_floor;
";

/// `SQLite`. `DROP COLUMN` needs 3.35 (2021), which the bundled
/// `libsqlite3-sys` exceeds; `m20260929_000005` relies on the same thing. None
/// of the three carries an index or a constraint that would block it.
const SQLITE_DOWN: &str = r"
ALTER TABLE qa_ingest_watermarks DROP COLUMN sweep_cursor_at;
ALTER TABLE qa_ingest_watermarks DROP COLUMN sweep_cursor_run_id;
ALTER TABLE qa_ingest_watermarks DROP COLUMN sweep_cursor_floor;
";

/// The DDL for `backend`, or an error naming why there is none.
///
/// Same shape and the same refusal as every earlier migration's `up_ddl`.
fn ddl(
    backend: sea_orm::DatabaseBackend,
    postgres: &'static str,
    sqlite: &'static str,
) -> Result<&'static str, DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(postgres),
        sea_orm::DatabaseBackend::Sqlite => Ok(sqlite),
        sea_orm::DatabaseBackend::MySql => Err(DbErr::Custom(
            "qa-insights has no MySQL schema; see m20260818_000001_initial's header".to_owned(),
        )),
        other => Err(DbErr::Migration(format!(
            "unsupported database backend: {other:?}"
        ))),
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        manager
            .get_connection()
            .execute_unprepared(ddl(backend, POSTGRES_UP, SQLITE_UP)?)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        manager
            .get_connection()
            .execute_unprepared(ddl(backend, POSTGRES_DOWN, SQLITE_DOWN)?)
            .await?;
        Ok(())
    }
}

/// Every name above is a runtime string, so `cargo build` is no evidence about
/// this file; the evidence is raw statements against a real engine.
#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the initial migration's test module carries the same allow and the same reason: \
              every name in this file is a runtime string, so the only evidence about it comes \
              from raw statements against a real engine"
)]
mod tests {
    use sea_orm::{ActiveModelTrait, ActiveValue, EntityTrait};
    use sea_orm_migration::MigrationName as _;
    use sea_orm_migration::MigratorTrait as _;
    use sea_orm_migration::sea_orm::{
        ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement,
    };
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::super::Migrator;
    use crate::infra::storage::entity::ingest_watermark;

    async fn migrated_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        Migrator::up(&conn, None).await.expect("migrations apply");
        conn
    }

    async fn columns(conn: &DatabaseConnection) -> Vec<String> {
        let rows = conn
            .query_all_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT name FROM pragma_table_info('qa_ingest_watermarks')",
            ))
            .await
            .expect("pragma_table_info");
        rows.iter()
            .map(|r| r.try_get::<String>("", "name").expect("name column"))
            .collect()
    }

    #[tokio::test]
    async fn the_three_columns_are_added_and_their_neighbours_are_not_disturbed() {
        let columns = columns(&migrated_db().await).await;
        for added in [
            "sweep_cursor_at",
            "sweep_cursor_run_id",
            "sweep_cursor_floor",
        ] {
            assert!(
                columns.iter().any(|c| c == added),
                "{added} is missing; columns were {columns:?}"
            );
        }
        for kept in [
            "id",
            "tenant_id",
            "last_reconciled_finished_at",
            "created_at",
            "updated_at",
        ] {
            assert!(
                columns.iter().any(|c| c == kept),
                "{kept} was disturbed; columns were {columns:?}"
            );
        }
    }

    /// **The property a `pragma_table_info` cannot prove.** A `Uuid` written
    /// through the driver lands on `SQLite` as a 16-byte BLOB, not as the
    /// 36-character text a human would write into a literal, and an instant
    /// lands in the driver's own encoding rather than `CURRENT_TIMESTAMP`'s.
    /// `m20260929_000004_run_completed_notification_cutoff`'s header records
    /// what it cost to discover that on a column that is *read*. These three
    /// are read on every sweep, so the round trip is asserted here rather than
    /// assumed from the DDL.
    #[tokio::test]
    async fn the_cursor_columns_round_trip_through_the_entity() {
        let conn = migrated_db().await;
        let row_id = Uuid::from_u128(0x5EE7);
        let run_id = Uuid::from_u128(0xC5);
        // Whole seconds: `SQLite` stores the instant as text and the format
        // carries sub-second digits, so an equality assertion on `now_utc()`
        // would be testing the codec's precision rather than the column.
        let at = OffsetDateTime::from_unix_timestamp(1_795_000_000).expect("a valid instant");
        let floor = OffsetDateTime::from_unix_timestamp(1_794_000_000).expect("a valid instant");

        ingest_watermark::ActiveModel {
            id: ActiveValue::Set(row_id),
            tenant_id: ActiveValue::Set(Uuid::from_u128(0x0A)),
            last_reconciled_finished_at: ActiveValue::Set(None),
            sweep_cursor_at: ActiveValue::Set(Some(at)),
            sweep_cursor_run_id: ActiveValue::Set(Some(run_id)),
            sweep_cursor_floor: ActiveValue::Set(Some(floor)),
            created_at: ActiveValue::Set(at),
            updated_at: ActiveValue::Set(at),
        }
        .insert(&conn)
        .await
        .expect("the row must insert through the entity");

        let row = ingest_watermark::Entity::find_by_id(row_id)
            .one(&conn)
            .await
            .expect("the row must be readable")
            .expect("the row must exist");

        assert_eq!(row.sweep_cursor_at, Some(at));
        assert_eq!(
            row.sweep_cursor_run_id,
            Some(run_id),
            "a Uuid must survive the driver's own encoding on this dialect; a column \
             written as a SQL literal would have failed exactly here"
        );
        assert_eq!(row.sweep_cursor_floor, Some(floor));
    }

    /// All three are nullable together, which is the steady state: a
    /// deployment whose sweep has never spent its budget inside its own
    /// lookback window holds `NULL` in every one of them.
    #[tokio::test]
    async fn the_columns_are_nullable() {
        let conn = migrated_db().await;
        let row_id = Uuid::from_u128(0x5EE8);
        let at = OffsetDateTime::from_unix_timestamp(1_795_000_000).expect("a valid instant");

        ingest_watermark::ActiveModel {
            id: ActiveValue::Set(row_id),
            tenant_id: ActiveValue::Set(Uuid::from_u128(0x0B)),
            last_reconciled_finished_at: ActiveValue::Set(None),
            sweep_cursor_at: ActiveValue::Set(None),
            sweep_cursor_run_id: ActiveValue::Set(None),
            sweep_cursor_floor: ActiveValue::Set(None),
            created_at: ActiveValue::Set(at),
            updated_at: ActiveValue::Set(at),
        }
        .insert(&conn)
        .await
        .expect("a row with no cursor must insert");

        let row = ingest_watermark::Entity::find_by_id(row_id)
            .one(&conn)
            .await
            .expect("readable")
            .expect("present");
        assert_eq!(row.sweep_cursor_at, None);
        assert_eq!(row.sweep_cursor_run_id, None);
        assert_eq!(row.sweep_cursor_floor, None);
    }

    #[tokio::test]
    async fn the_down_migration_removes_exactly_the_three_columns() {
        let conn = migrated_db().await;
        let before = columns(&conn).await;
        let manager = sea_orm_migration::SchemaManager::new(&conn);
        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .expect("down applies");

        let after = columns(&conn).await;
        let mut expected = before;
        expected.retain(|c| {
            !matches!(
                c.as_str(),
                "sweep_cursor_at" | "sweep_cursor_run_id" | "sweep_cursor_floor"
            )
        });
        assert_eq!(
            after, expected,
            "down must remove the three cursor columns and nothing else"
        );
    }

    /// A migration missing from `Migrator::migrations()` runs on no deployment;
    /// the tests above would fail on the column check, and this names the cause.
    #[test]
    fn the_migration_is_registered_in_the_chain() {
        let registered: Vec<String> = Migrator::migrations()
            .iter()
            .map(|m| m.name().to_owned())
            .collect();
        assert!(
            registered.contains(&super::Migration.name().to_owned()),
            "the sweep-cursor migration is not in the applied chain; registered: {registered:?}"
        );
    }
}
