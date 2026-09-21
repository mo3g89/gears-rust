//! Two columns on `qa_notification_config`: the SMTP AUTH username, and the
//! credstore **reference** to its password.
//!
//! # Why a second migration rather than an edit to the first
//!
//! `m20260818_000001_initial` has already run on every database this gear has
//! ever been deployed against; editing its DDL would change nothing on any of
//! them and would silently diverge the code's idea of the schema from the
//! schema. This is the first migration this gear has added after its initial
//! one, which is why `m20260818_000001_initial`'s
//! `TABLES_OWNED_BY_LATER_MIGRATIONS` exists — it stays empty, because this
//! migration creates no table.
//!
//! # Why the password is not a column
//!
//! It is the same divergence-from-legacy the initial migration records twice
//! already, for `slack_webhook_credstore_ref` and
//! `qa_jira_config.api_token_credstore_ref`: a value whose possession *is* the
//! authorization is held by reference, and the GET surface answers with the
//! reference. The username is a column, in the clear, for the reason
//! `qa_jira_config` keeps `email` in the clear beside its token reference — a
//! username names an account and authorizes nothing.
//!
//! **This is the first credential in this gear that the gear itself resolves.**
//! The Slack webhook and the JIRA token both ride HTTP, so `oagw` fetches and
//! injects them and no plaintext ever enters this process. SMTP is not HTTP,
//! `oagw` cannot proxy it, and so `infra::notify::mail_smtp` reads this
//! reference through `credstore_sdk::CredStoreClientV1` and holds the value for
//! the length of one `send`. ADR-0011 is the decision; ADR-0008
//! (`cpt-cf-qa-adr-credential-containment`) is the rule that still binds it —
//! nothing derived from that value is ever formatted.
//!
//! # Two dialects, not three, and why this file has no `MYSQL_UP`
//!
//! The initial migration declares one DDL blob per dialect including `MySQL`,
//! which `up()` then refuses outright (five of this schema's indexes exceed
//! `InnoDB`'s 3072-byte key limit). That blob earns its keep there because two
//! parity tests read it: they assert every dialect *declares* the same columns
//! and indexes in the same order, so a `MySQL` blob that drifted would fail a
//! test rather than sit unnoticed.
//!
//! There is nothing here for those tests to compare — two `ALTER TABLE ... ADD
//! COLUMN` statements, in a file whose `MySQL` path is unreachable by
//! construction — so a `MYSQL_UP` here would be a constant nothing reads,
//! nothing executes and nothing checks. [`up_ddl`] refuses the backend by name
//! instead, pointing at the initial migration's header for the reason.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Postgres. `VARCHAR(255)` for the username matches `email_smtp_host`'s width;
/// `VARCHAR(512)` for the reference matches `slack_webhook_credstore_ref`'s,
/// which is the other column in this table holding a value of the same kind.
const POSTGRES_UP: &str = r"
ALTER TABLE qa_notification_config
    ADD COLUMN IF NOT EXISTS email_smtp_username VARCHAR(255) NOT NULL DEFAULT '';
ALTER TABLE qa_notification_config
    ADD COLUMN IF NOT EXISTS email_smtp_credstore_ref VARCHAR(512) NOT NULL DEFAULT '';
";

/// `SQLite`. No `IF NOT EXISTS` clause exists for `ADD COLUMN` in `SQLite`, and
/// none is needed: the unit tier builds a fresh in-memory database for every
/// test, so this statement never meets a table that already has the column.
const SQLITE_UP: &str = r"
ALTER TABLE qa_notification_config
    ADD COLUMN email_smtp_username TEXT NOT NULL DEFAULT '';
ALTER TABLE qa_notification_config
    ADD COLUMN email_smtp_credstore_ref TEXT NOT NULL DEFAULT '';
";

/// The DDL for `backend`, or an error naming why there is none.
///
/// Same shape and the same refusal as the initial migration's `up_ddl`: handing
/// `MYSQL_UP` to a `MySQL` engine would half-apply a schema this gear cannot
/// run on, so the backend is refused instead of narrated.
fn up_ddl(backend: sea_orm::DatabaseBackend) -> Result<&'static str, DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(POSTGRES_UP),
        sea_orm::DatabaseBackend::Sqlite => Ok(SQLITE_UP),
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
        let conn = manager.get_connection();
        conn.execute_unprepared(up_ddl(backend)?).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        // `DROP COLUMN` rather than a no-op: `down()` that does not undo `up()`
        // is dead weight, which is the property the initial migration's own
        // `the_down_migration_drops_every_table` pins for its half.
        //
        // `SQLite` has supported `ALTER TABLE ... DROP COLUMN` since 3.35
        // (2021-03); the bundled `libsqlite3-sys` is far newer, and the test
        // below is what proves it rather than this comment.
        conn.execute_unprepared(
            r"
ALTER TABLE qa_notification_config DROP COLUMN email_smtp_credstore_ref;
ALTER TABLE qa_notification_config DROP COLUMN email_smtp_username;
",
        )
        .await?;
        Ok(())
    }
}

/// The same argument the initial migration's test module makes: every name
/// above is a runtime string, so `cargo build` is no evidence at all about this
/// file. These run the real `Migrator` against a real in-memory `SQLite`
/// database.
#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the initial migration's test module carries the same allow and the same \
              reason: every name in this file is a runtime string, so the only evidence \
              about it comes from raw statements against a real engine"
)]
mod tests {
    use sea_orm_migration::MigratorTrait;
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

    /// The column names `infra::storage::entity::notification_config::Model`
    /// binds. A typo in either the entity or the DDL is a query-time failure,
    /// not a compile error, so it is asserted here against the live catalogue.
    async fn columns(conn: &DatabaseConnection) -> Vec<String> {
        let rows = conn
            .query_all_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT name FROM pragma_table_info('qa_notification_config')",
            ))
            .await
            .expect("pragma_table_info");
        rows.iter()
            .map(|r| r.try_get::<String>("", "name").expect("name column"))
            .collect()
    }

    #[tokio::test]
    async fn the_migration_adds_both_smtp_credential_columns() {
        let conn = migrated_db().await;
        let columns = columns(&conn).await;
        assert!(
            columns.iter().any(|c| c == "email_smtp_username"),
            "email_smtp_username is missing; columns were {columns:?}"
        );
        assert!(
            columns.iter().any(|c| c == "email_smtp_credstore_ref"),
            "email_smtp_credstore_ref is missing; columns were {columns:?}"
        );
    }

    /// Both columns default to the empty string, which is what makes this
    /// migration safe to apply to a database full of rows written before SMTP
    /// authentication existed: every one of them comes out as "no credential",
    /// which `NotifyService`'s own `mail_credentials` reads as an
    /// unauthenticated relay rather than as a broken configuration.
    #[tokio::test]
    async fn an_existing_row_gains_empty_credentials_rather_than_null() {
        let conn = migrated_db().await;
        conn.execute_unprepared(
            "INSERT INTO qa_notification_config (id, tenant_id, created_at, updated_at) \
             VALUES ('00000000-0000-0000-0000-0000000000aa', \
                     '00000000-0000-0000-0000-0000000000bb', \
                     '2026-09-21 00:00:00+00:00', '2026-09-21 00:00:00+00:00')",
        )
        .await
        .expect("insert a row carrying only the non-defaulted columns");

        let row = conn
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT email_smtp_username, email_smtp_credstore_ref \
                 FROM qa_notification_config",
            ))
            .await
            .expect("select")
            .expect("one row");

        assert_eq!(
            row.try_get::<String>("", "email_smtp_username")
                .expect("username is NOT NULL"),
            ""
        );
        assert_eq!(
            row.try_get::<String>("", "email_smtp_credstore_ref")
                .expect("reference is NOT NULL"),
            ""
        );
    }

    /// `down()` undoes `up()` and touches nothing else — the same property the
    /// initial migration pins for its own half.
    #[tokio::test]
    async fn the_down_migration_drops_exactly_its_own_two_columns() {
        let conn = migrated_db().await;
        let before = columns(&conn).await;
        let manager = sea_orm_migration::SchemaManager::new(&conn);

        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .expect("down applies");

        let after = columns(&conn).await;
        let removed: Vec<_> = before
            .iter()
            .filter(|c| !after.contains(c))
            .cloned()
            .collect();
        assert_eq!(
            removed,
            vec![
                "email_smtp_username".to_owned(),
                "email_smtp_credstore_ref".to_owned()
            ],
            "down() must remove exactly the two columns up() added"
        );
    }
}
