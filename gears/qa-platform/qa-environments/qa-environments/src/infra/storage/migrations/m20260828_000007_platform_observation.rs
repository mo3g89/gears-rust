//! Adds the four columns a Kubernetes cluster observation writes:
//! `vhp_base_url`, `observed_namespace`, `version_detect_error` and
//! `version_detected_at`.
//!
//! **Added 2026-08-28, qa-platform "platform observation" spec, Task 4.**
//! Task 3 (`domain::ports::platform_observer`) gave this gear a
//! `PlatformObserver` port and an `ObservationOutcome` value, but nothing yet
//! persists what a call to `observe()` learned. These four columns are where
//! it lands; `OrmPlatformsRepository::record_observation` (this task) is their
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
/// column names are runtime strings, so `platform::Model::vhp_base_url`
/// compiling says nothing about whether a column of that name exists in the
/// database. Only a query does — break-tested by renaming a column in
/// `SQLITE_UP` alone, leaving the entity untouched so the crate still
/// compiles, which turns every test in this module red along with the rest of
/// this gear's DB-backed suite (`OrmPlatformsRepository` names every column it
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
    use sea_orm::{
        ActiveModelTrait, ActiveValue, ConnectOptions, ConnectionTrait, Database,
        DatabaseConnection, EntityTrait,
    };
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::infra::storage::entity::platform;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
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
        for migration in super::super::Migrator::migrations() {
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-environments migrations");
        }
        conn
    }

    fn platform_am() -> platform::ActiveModel {
        platform::ActiveModel {
            id: ActiveValue::Set(uuid(1)),
            tenant_id: ActiveValue::Set(uuid(2)),
            name: ActiveValue::Set("staging-a".to_owned()),
            product_id: ActiveValue::Set(Some(uuid(3))),
            description: ActiveValue::Set(Some("desc".to_owned())),
            kubeconfig_credstore_ref: ActiveValue::Set("credstore://ref".to_owned()),
            available: ActiveValue::Set(true),
            observed_version: ActiveValue::Set(Some("26.5".to_owned())),
            observed_build: ActiveValue::Set(Some("0".to_owned())),
            default_branch: ActiveValue::Set(None),
            is_default: ActiveValue::Set(false),
            vhp_base_url: ActiveValue::Set(Some("https://sv.jele.io".to_owned())),
            observed_namespace: ActiveValue::Set(Some("virtuozzo".to_owned())),
            version_detect_error: ActiveValue::Set(Some(
                "namespaces \"virtuozzo\" not found".to_owned(),
            )),
            version_detected_at: ActiveValue::Set(Some(now())),
            cluster_status: ActiveValue::Set(None),
            cluster_status_message: ActiveValue::Set(None),
            cluster_nodes: ActiveValue::Set(None),
            cluster_namespace_count: ActiveValue::Set(None),
            cluster_checked_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    #[tokio::test]
    async fn all_four_columns_round_trip_through_the_migrated_schema() {
        let conn = migrated_db().await;

        platform_am().insert(&conn).await.unwrap();

        let stored = platform::Entity::find_by_id(uuid(1))
            .one(&conn)
            .await
            .unwrap()
            .expect("the platform row must read back");
        assert_eq!(
            stored.vhp_base_url.as_deref(),
            Some("https://sv.jele.io"),
            "vhp_base_url must survive the round trip"
        );
        assert_eq!(
            stored.observed_namespace.as_deref(),
            Some("virtuozzo"),
            "observed_namespace must survive the round trip"
        );
        assert_eq!(
            stored.version_detect_error.as_deref(),
            Some("namespaces \"virtuozzo\" not found"),
            "version_detect_error must survive the round trip"
        );
        assert_eq!(
            stored.version_detected_at,
            Some(now()),
            "version_detected_at must survive the round trip"
        );
        assert_eq!(
            stored.observed_build.as_deref(),
            Some("0"),
            "and the columns added before it must be unaffected by the ALTER"
        );
    }

    #[tokio::test]
    async fn all_four_columns_are_nullable() {
        let conn = migrated_db().await;

        let am = platform::ActiveModel {
            vhp_base_url: ActiveValue::Set(None),
            observed_namespace: ActiveValue::Set(None),
            version_detect_error: ActiveValue::Set(None),
            version_detected_at: ActiveValue::Set(None),
            ..platform_am()
        };
        am.insert(&conn).await.unwrap();

        let stored = platform::Entity::find_by_id(uuid(1))
            .one(&conn)
            .await
            .unwrap()
            .expect("the platform row must read back");
        assert_eq!(stored.vhp_base_url, None);
        assert_eq!(stored.observed_namespace, None);
        assert_eq!(stored.version_detect_error, None);
        assert_eq!(stored.version_detected_at, None);
    }

    /// `down()` has to actually drop all four columns, or a rollback leaves a
    /// schema the previous release's entity cannot read.
    #[tokio::test]
    async fn the_down_migration_drops_all_four_columns() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

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
