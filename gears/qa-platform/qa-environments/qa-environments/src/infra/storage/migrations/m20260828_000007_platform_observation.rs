//! Adds the four columns a Kubernetes cluster observation writes:
//! `vhp_base_url`, `observed_namespace`, `version_detect_error` and
//! `version_detected_at`.
//!
//! **Added 2026-08-28, qa-platform "platform observation" spec, Task 4.**
//! Task 3 (`domain::ports::platform_observer`) gave this gear a
//! `PlatformObserver` port and an `ObservationOutcome` value, but nothing yet
//! persists what a call to `observe()` learned. These four columns are where
//! it lands; `OrmEnvironmentsRepository::record_observation` (this task) is their
//! only writer.
//!
//! ## A second migration, not an edit to the first
//!
//! Same reasoning as `m20260814_000006_platform_default_branch`'s module doc:
//! every earlier migration in this sequence must be assumed to have run
//! wherever this gear is deployed, so it is a new file rather than an edit.
//! The numbering (`000007`) continues directly from `000006`.
//!
//! ## No `IF NOT EXISTS` on the `ALTER`
//!
//! For the reason `m20260813_000004_observed_build` and `m20260814_000006`
//! both record: `SQLite` rejects `ALTER TABLE … ADD COLUMN IF NOT EXISTS`
//! outright and `MySQL`'s grammar has no such clause either, so all three
//! dialect statements are bare rather than one defensive and two not — the
//! migration runner's apply-once guarantee is the actual guard.
//!
//! ## `TEXT`, not `VARCHAR`, for the three string columns
//!
//! `vhp_base_url`, `observed_namespace` and `version_detect_error` are all
//! declared `TEXT` on **every** dialect, matching `description`'s declaration
//! in `m20260812_000001_initial` rather than the bounded `VARCHAR` columns in
//! that same table. None of the three has a downstream sink whose width could
//! anchor a bound the way `default_branch`'s did (see that migration's module
//! doc for why a bound was worth adding there): a base URL is an
//! operator-opaque string nothing else parses to a fixed width, a namespace
//! name is a Kubernetes identifier already capped at 63 characters by the
//! cluster itself, and a detect-error message is free text meant for a human
//! to read, not a value fed onward to another gear's column. Bounding any of
//! them here would be a limit invented for its own sake.
//!
//! ## `version_detected_at`: `TIMESTAMPTZ` / `TIMESTAMP` / `TEXT`, nullable
//!
//! Mirrors `created_at`/`updated_at`'s per-dialect declaration in
//! `m20260812_000001_initial` (`TIMESTAMPTZ` on Postgres, `TIMESTAMP` on
//! `MySQL`, `TEXT` on `SQLite`, since `SQLite` has no native temporal type and
//! `time::OffsetDateTime` round-trips through it as text) — the only
//! difference is nullability, because unlike those two columns this one has
//! no value until the first observation attempt ever completes.
//!
//! ## All four nullable, no backfill, no default
//!
//! An existing platform has never been observed, so `NULL` — "nothing known
//! yet" — is the only honest value for every row already in the table. A
//! default would assert something no cluster ever reported.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN vhp_base_url TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN observed_namespace TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN version_detect_error TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN version_detected_at TIMESTAMPTZ NULL;
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN vhp_base_url TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN observed_namespace TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN version_detect_error TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN version_detected_at TIMESTAMP NULL;
";

const SQLITE_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN vhp_base_url TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN observed_namespace TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN version_detect_error TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN version_detected_at TEXT NULL;
";

/// Pick the statement blob for a backend.
///
/// Extracted from `up()` so it can be tested directly, following
/// `m20260814_000006_platform_default_branch::sql_for` — inline in the
/// `match`, nothing could reach it, and swapping two adjacent arms is exactly
/// the mistake that guard was added to catch there.
const fn sql_for(backend: sea_orm::DatabaseBackend) -> &'static str {
    match backend {
        sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
        sea_orm::DatabaseBackend::MySql => MYSQL_UP,
        sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = sql_for(manager.get_database_backend());
        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE qa_platforms DROP COLUMN vhp_base_url;
             ALTER TABLE qa_platforms DROP COLUMN observed_namespace;
             ALTER TABLE qa_platforms DROP COLUMN version_detect_error;
             ALTER TABLE qa_platforms DROP COLUMN version_detected_at;",
        )
        .await?;
        Ok(())
    }
}

/// Schema tests for the four added columns.
///
/// **`cargo build` proves nothing about a `SeaORM` entity**: its table and
/// column names are runtime strings, so `environment::Model::vhp_base_url`
/// compiling says nothing about whether a column of that name exists in the
/// database. Only a query does — break-tested by renaming a column in
/// `SQLITE_UP` alone, leaving the entity untouched so the crate still
/// compiles, which turns every test in this module red along with the rest of
/// this gear's DB-backed suite (`OrmEnvironmentsRepository` names every column it
/// touches).
///
/// Every fixture below writes **non-`NULL`** values for all four columns
/// before asserting the round trip, per this subsystem's Task 9b lesson: a
/// fixture that only ever writes `NULL` cannot catch a value silently dropped
/// on the way out, because `NULL` survives almost any mistake unchanged.
///
/// `clippy::disallowed_methods` is allowed for the same narrow reason as in
/// the two migrations this one follows: a schema test needs a raw connection,
/// and the `SecureORM` wrappers deliberately expose none. Nothing here is a
/// production path.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    use uuid::Uuid;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// In-memory `SQLite` with **every** registered migration applied in
    /// order, which is also what proves `Migrator::migrations()` lists the
    /// new one: an unregistered migration leaves the columns absent and every
    /// assertion below fails.
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

    const TIMESTAMP: &str = "2026-08-28 12:00:00+00:00";

    /// Plant a row with raw SQL.
    ///
    /// **Not the entity.** Task 19's `m20260903_000012` dropped `vhp_base_url`,
    /// `observed_namespace` and `kubeconfig_credstore_ref`, and
    /// `environment::Model` lost the matching fields in the same commit -- so it
    /// can neither set the columns this test is about nor satisfy
    /// `kubeconfig_credstore_ref NOT NULL`. The migration still runs on a real
    /// upgrade path, so its own test still has to hold; it just cannot go
    /// through today's model to do it.
    async fn plant(conn: &DatabaseConnection, observed: bool) {
        use super::super::legacy_row::{insert, text};
        let (base_url, namespace, error, detected_at) = if observed {
            (
                text("https://sv.jele.io"),
                text("virtuozzo"),
                text("namespaces \"virtuozzo\" not found"),
                text(TIMESTAMP),
            )
        } else {
            (
                "NULL".to_owned(),
                "NULL".to_owned(),
                "NULL".to_owned(),
                "NULL".to_owned(),
            )
        };
        insert(
            conn,
            "qa_environments",
            &[
                ("id", super::super::legacy_row::uuid_lit(uuid(1))),
                ("tenant_id", super::super::legacy_row::uuid_lit(uuid(2))),
                ("name", text("staging-a")),
                ("kubeconfig_credstore_ref", text("credstore://ref")),
                ("available", "1".to_owned()),
                ("observed_version", text("26.5")),
                ("observed_build", text("0")),
                ("vhp_base_url", base_url),
                ("observed_namespace", namespace),
                ("version_detect_error", error),
                ("version_detected_at", detected_at),
                ("created_at", text(TIMESTAMP)),
                ("updated_at", text(TIMESTAMP)),
            ],
        )
        .await;
    }

    /// One column of the planted row, as text (`NULL` reads as `None`).
    async fn col(conn: &DatabaseConnection, column: &str) -> Option<String> {
        conn.query_one(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "SELECT {column} FROM qa_environments WHERE id = {};",
                super::super::legacy_row::uuid_lit(uuid(1))
            ),
        ))
        .await
        .unwrap()
        .expect("the environment row must read back")
        .try_get_by_index::<Option<String>>(0)
        .expect("the column must be readable as text")
    }

    #[tokio::test]
    async fn all_four_columns_round_trip_through_the_migrated_schema() {
        let conn = migrated_db().await;
        plant(&conn, true).await;

        assert_eq!(
            col(&conn, "vhp_base_url").await.as_deref(),
            Some("https://sv.jele.io"),
            "vhp_base_url must survive the round trip"
        );
        assert_eq!(
            col(&conn, "observed_namespace").await.as_deref(),
            Some("virtuozzo"),
            "observed_namespace must survive the round trip"
        );
        assert_eq!(
            col(&conn, "version_detect_error").await.as_deref(),
            Some("namespaces \"virtuozzo\" not found"),
            "version_detect_error must survive the round trip"
        );
        assert_eq!(
            col(&conn, "version_detected_at").await.as_deref(),
            Some(TIMESTAMP),
            "version_detected_at must survive the round trip"
        );
        assert_eq!(
            col(&conn, "observed_build").await.as_deref(),
            Some("0"),
            "and the columns added before it must be unaffected by the ALTER"
        );
    }

    #[tokio::test]
    async fn all_four_columns_are_nullable() {
        let conn = migrated_db().await;
        plant(&conn, false).await;

        for column in [
            "vhp_base_url",
            "observed_namespace",
            "version_detect_error",
            "version_detected_at",
        ] {
            assert_eq!(col(&conn, column).await, None, "{column} must be nullable");
        }
    }

    /// `down()` has to actually drop all four columns, or a rollback leaves a
    /// schema the previous release's entity cannot read.
    #[tokio::test]
    async fn the_down_migration_drops_all_four_columns() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

        // The rename in `m20260903_000010_rename_platform_tables` has to come off
        // first: this migration's `down` names `qa_platforms` in raw SQL, which
        // exists only once the rename is reversed. Note this is NOT a full
        // reverse-order rollback -- the migrations between that one and this one
        // are skipped -- and it does not need to be: none of them stands in the
        // way of this `down`, and the rename is the only one that does.
        sea_orm_migration::MigrationTrait::down(
            &super::super::m20260903_000010_rename_platform_tables::Migration,
            &manager,
        )
        .await
        .unwrap();

        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();

        for column in [
            "vhp_base_url",
            "observed_namespace",
            "version_detect_error",
            "version_detected_at",
        ] {
            let result = conn
                .execute_unprepared(&format!("SELECT {column} FROM qa_platforms"))
                .await;
            assert!(
                result.is_err(),
                "{column} must be gone after down(), but the SELECT succeeded"
            );
        }
        conn.execute_unprepared("SELECT observed_build FROM qa_platforms")
            .await
            .expect("down() must not touch observed_build");
    }

    /// `sql_for` maps each backend to its own statement -- nothing else calls
    /// it, and the suite only ever runs migrations against `SQLite`, so
    /// without this guard a swapped `Postgres`/`MySql` arm is invisible to
    /// every other test in this module.
    ///
    /// **Added retroactively, 2026-08-28, scope extended from the
    /// cluster-health task that added this guard's sibling in
    /// `m20260828_000008_platform_cluster_health`.** This migration's three
    /// blobs differ only in `version_detected_at`'s type
    /// (`TIMESTAMPTZ`/`TIMESTAMP`/`TEXT`), so a swap here has the same silent
    /// hazard as `000008`'s: Postgres would lose the timezone on every
    /// observation timestamp rather than fail loudly.
    #[test]
    fn each_backend_gets_a_statement_of_the_right_column_type() {
        use sea_orm::DatabaseBackend;

        assert_eq!(
            super::sql_for(DatabaseBackend::Postgres),
            super::POSTGRES_UP,
            "Postgres must get POSTGRES_UP, not another dialect's blob"
        );
        assert_eq!(
            super::sql_for(DatabaseBackend::MySql),
            super::MYSQL_UP,
            "MySql must get MYSQL_UP, not another dialect's blob"
        );
        assert_eq!(
            super::sql_for(DatabaseBackend::Sqlite),
            super::SQLITE_UP,
            "Sqlite must get SQLITE_UP, not another dialect's blob"
        );

        let postgres = super::sql_for(DatabaseBackend::Postgres);
        assert!(
            postgres.contains("TIMESTAMPTZ"),
            "Postgres must get TIMESTAMPTZ for version_detected_at"
        );

        let mysql = super::sql_for(DatabaseBackend::MySql);
        assert!(
            mysql.contains("version_detected_at TIMESTAMP NULL"),
            "MySQL must get the bare TIMESTAMP statement for version_detected_at"
        );
        assert!(
            !mysql.contains("TIMESTAMPTZ"),
            "MySQL must not get Postgres' TIMESTAMPTZ -- swapping the Postgres \
             and MySql arms would silently drop the timezone on every stored \
             observation time, and this line is what catches it"
        );

        let sqlite = super::sql_for(DatabaseBackend::Sqlite);
        assert!(
            sqlite.contains("version_detected_at TEXT"),
            "SQLite must get TEXT for version_detected_at, having no native \
             temporal type"
        );
        assert!(
            !sqlite.contains("TIMESTAMP"),
            "SQLite must not get a server temporal type"
        );
    }
}
