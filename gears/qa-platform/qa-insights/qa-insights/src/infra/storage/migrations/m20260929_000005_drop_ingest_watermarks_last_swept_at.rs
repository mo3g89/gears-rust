//! Drops `qa_ingest_watermarks.last_swept_at`, the column of a sweep that never existed.
//!
//! The initial migration declared it for "the stale-in-progress sweep". No such
//! sweep was ever built: the reconciler explicitly never touches in-progress
//! runs (`domain::service::reconcile`'s header), and the dashboard reads active
//! runs from qa-runs live instead. The only reader of the column was this
//! gear's own tests, and the owner ruled it deleted.
//!
//! **A new migration and not an edit of `m20260818_000001_initial`**, because
//! the chain is append-only: an edit would never reach a deployment that already
//! ran the initial migration, and those deployments still carry the column.
//!
//! No value is written or bound here; the statement is pure DDL. `DROP COLUMN`
//! needs `SQLite` 3.35 (2021), which the bundled `libsqlite3-sys` exceeds, and
//! the column carries no index or constraint that would block it.
//!
//! `down()` restores the column as nullable and empty. The values it held are
//! not recoverable, which is acceptable because nothing ever wrote a non-null
//! one outside this gear's tests.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Postgres. `IF EXISTS` keeps a re-run harmless.
const POSTGRES_UP: &str = r"
ALTER TABLE qa_ingest_watermarks DROP COLUMN IF EXISTS last_swept_at;
";

/// `SQLite`. It has no `DROP COLUMN IF EXISTS`; the migration table is what
/// makes a re-run not happen.
const SQLITE_UP: &str = r"
ALTER TABLE qa_ingest_watermarks DROP COLUMN last_swept_at;
";

const POSTGRES_DOWN: &str = r"
ALTER TABLE qa_ingest_watermarks ADD COLUMN IF NOT EXISTS last_swept_at TIMESTAMPTZ NULL;
";

const SQLITE_DOWN: &str = r"
ALTER TABLE qa_ingest_watermarks ADD COLUMN last_swept_at TEXT NULL;
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
    use sea_orm_migration::MigrationName as _;
    use sea_orm_migration::MigratorTrait as _;
    use sea_orm_migration::sea_orm::{
        ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement,
    };

    use super::super::Migrator;

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
    async fn the_column_is_gone_and_its_neighbours_are_not() {
        let columns = columns(&migrated_db().await).await;
        assert!(
            !columns.iter().any(|c| c == "last_swept_at"),
            "last_swept_at survived; columns were {columns:?}"
        );
        for kept in [
            "id",
            "tenant_id",
            "last_reconciled_finished_at",
            "created_at",
            "updated_at",
        ] {
            assert!(
                columns.iter().any(|c| c == kept),
                "{kept} was dropped too; columns were {columns:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_down_migration_restores_exactly_the_column() {
        let conn = migrated_db().await;
        let before = columns(&conn).await;
        let manager = sea_orm_migration::SchemaManager::new(&conn);
        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .expect("down applies");
        let mut after = columns(&conn).await;
        assert!(
            after.iter().any(|c| c == "last_swept_at"),
            "down did not restore the column; columns were {after:?}"
        );
        after.retain(|c| c != "last_swept_at");
        assert_eq!(after, before, "down must add only last_swept_at");
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
            "the drop migration is not in the applied chain; registered: {registered:?}"
        );
    }
}
