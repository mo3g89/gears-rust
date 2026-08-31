//! Adds the five columns one cluster-health read writes: `cluster_status`,
//! `cluster_status_message`, `cluster_nodes`, `cluster_namespace_count` and
//! `cluster_checked_at`.
//!
//! **Added 2026-08-28, qa-platform "cluster health" spec, Task 4.** Task 3
//! (`domain::ports::platform_observer`) gave this gear a `HealthOutcome` value
//! alongside the version-detection `ObservationOutcome` it already had, but
//! nothing yet persists what a health read learned. These five columns are
//! where it lands; `OrmPlatformsRepository::record_observation` (this task) is
//! their only writer.
//!
//! ## A second migration, not an edit to the first
//!
//! Same reasoning as `m20260828_000007_platform_observation`'s module doc
//! (and `m20260814_000006` before it): every earlier migration in this
//! sequence must be assumed to have run wherever this gear is deployed, so
//! this is a new file rather than an edit to `000007`. The numbering
//! (`000008`) continues directly from `000007`.
//!
//! ## No `IF NOT EXISTS` on the `ALTER`
//!
//! For the reason `m20260813_000004_observed_build`, `m20260814_000006` and
//! `m20260828_000007` all record: `SQLite` rejects `ALTER TABLE … ADD COLUMN
//! IF NOT EXISTS` outright and `MySQL`'s grammar has no such clause either,
//! so all three dialect statements are bare rather than one defensive and two
//! not — the migration runner's apply-once guarantee is the actual guard.
//!
//! ## `JSONB` / `JSON` / `TEXT` for `cluster_nodes`
//!
//! The node list is stored as a JSON blob rather than a normalised table: it
//! is written and read whole (never queried by individual node), so a
//! dialect's native JSON type is the natural fit. Postgres gets `JSONB` (its
//! indexed, canonical binary JSON type); `MySQL` gets its own native `JSON`
//! type; `SQLite` has neither, so it gets `TEXT` and the JSON round-trips
//! through it as text, the same accommodation `version_detected_at` makes for
//! `SQLite`'s lack of a temporal type in `m20260828_000007`.
//!
//! ## All five nullable, no backfill, no default
//!
//! An existing platform has never had its cluster health checked, so `NULL` —
//! "nothing known yet" — is the only honest value for every row already in
//! the table. A default would assert a reading no cluster ever produced.
//! `NULL` here is also a distinct fact from `cluster_status = "Unreachable"`:
//! the former means never checked, the latter means checked and failed — see
//! `entity::platform::Model::cluster_status`'s own doc.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN cluster_status TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_status_message TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_nodes JSONB NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_namespace_count INTEGER NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_checked_at TIMESTAMPTZ NULL;
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN cluster_status TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_status_message TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_nodes JSON NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_namespace_count INTEGER NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_checked_at TIMESTAMP NULL;
";

const SQLITE_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN cluster_status TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_status_message TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_nodes TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_namespace_count INTEGER NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_checked_at TEXT NULL;
";

/// Pick the statement blob for a backend.
///
/// Extracted from `up()` so it can be tested directly, following
/// `m20260828_000007_platform_observation::sql_for` — inline in the `match`,
/// nothing could reach it, and swapping two adjacent arms is exactly the
/// mistake that guard was added to catch there.
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
            "ALTER TABLE qa_platforms DROP COLUMN cluster_status;
             ALTER TABLE qa_platforms DROP COLUMN cluster_status_message;
             ALTER TABLE qa_platforms DROP COLUMN cluster_nodes;
             ALTER TABLE qa_platforms DROP COLUMN cluster_namespace_count;
             ALTER TABLE qa_platforms DROP COLUMN cluster_checked_at;",
        )
        .await?;
        Ok(())
    }
}

/// Schema tests for the five added columns.
///
/// **`cargo build` proves nothing about a `SeaORM` entity**: its table and
/// column names are runtime strings, so `platform::Model::cluster_status`
/// compiling says nothing about whether a column of that name exists in the
/// database. Only a query does — break-tested by renaming a column in
/// `SQLITE_UP` alone, leaving the entity untouched so the crate still
/// compiles, which turns every test in this module red along with the rest of
/// this gear's DB-backed suite (`OrmPlatformsRepository` names every column it
/// touches).
///
/// Every fixture below writes **non-`NULL`** values for all five columns
/// before asserting the round trip, per this subsystem's Task 9b lesson: a
/// fixture that only ever writes `NULL` cannot catch a value silently dropped
/// on the way out, because `NULL` survives almost any mistake unchanged.
///
/// `clippy::disallowed_methods` is allowed for the same narrow reason as in
/// the migrations this one follows: a schema test needs a raw connection, and
/// the `SecureORM` wrappers deliberately expose none. Nothing here is a
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
            version_detect_error: ActiveValue::Set(None),
            version_detected_at: ActiveValue::Set(Some(now())),
            cluster_status: ActiveValue::Set(Some("Healthy".to_owned())),
            cluster_status_message: ActiveValue::Set(Some("all good".to_owned())),
            cluster_nodes: ActiveValue::Set(Some(serde_json::json!([{
                "name": "node-0",
                "control_plane": true,
                "ready": true,
                "kubelet_version": "v1.33.4+k3s1",
                "os_image": "Ubuntu 24.04.3 LTS",
            }]))),
            cluster_namespace_count: ActiveValue::Set(Some(14)),
            cluster_checked_at: ActiveValue::Set(Some(now())),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    #[tokio::test]
    async fn all_five_columns_round_trip_through_the_migrated_schema() {
        let conn = migrated_db().await;

        platform_am().insert(&conn).await.unwrap();

        let stored = platform::Entity::find_by_id(uuid(1))
            .one(&conn)
            .await
            .unwrap()
            .expect("the platform row must read back");
        assert_eq!(
            stored.cluster_status.as_deref(),
            Some("Healthy"),
            "cluster_status must survive the round trip"
        );
        assert_eq!(
            stored.cluster_status_message.as_deref(),
            Some("all good"),
            "cluster_status_message must survive the round trip"
        );
        assert_eq!(
            stored.cluster_nodes,
            Some(serde_json::json!([{
                "name": "node-0",
                "control_plane": true,
                "ready": true,
                "kubelet_version": "v1.33.4+k3s1",
                "os_image": "Ubuntu 24.04.3 LTS",
            }])),
            "cluster_nodes must survive the round trip"
        );
        assert_eq!(
            stored.cluster_namespace_count,
            Some(14),
            "cluster_namespace_count must survive the round trip"
        );
        assert_eq!(
            stored.cluster_checked_at,
            Some(now()),
            "cluster_checked_at must survive the round trip"
        );
        assert_eq!(
            stored.observed_version.as_deref(),
            Some("26.5"),
            "and the columns added before it must be unaffected by the ALTER"
        );
    }

    #[tokio::test]
    async fn all_five_columns_are_nullable() {
        let conn = migrated_db().await;

        let am = platform::ActiveModel {
            cluster_status: ActiveValue::Set(None),
            cluster_status_message: ActiveValue::Set(None),
            cluster_nodes: ActiveValue::Set(None),
            cluster_namespace_count: ActiveValue::Set(None),
            cluster_checked_at: ActiveValue::Set(None),
            ..platform_am()
        };
        am.insert(&conn).await.unwrap();

        let stored = platform::Entity::find_by_id(uuid(1))
            .one(&conn)
            .await
            .unwrap()
            .expect("the platform row must read back");
        assert_eq!(stored.cluster_status, None);
        assert_eq!(stored.cluster_status_message, None);
        assert_eq!(stored.cluster_nodes, None);
        assert_eq!(stored.cluster_namespace_count, None);
        assert_eq!(stored.cluster_checked_at, None);
    }

    /// `down()` has to actually drop all five columns, or a rollback leaves a
    /// schema the previous release's entity cannot read.
    #[tokio::test]
    async fn the_down_migration_drops_all_five_columns() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();

        for column in [
            "cluster_status",
            "cluster_status_message",
            "cluster_nodes",
            "cluster_namespace_count",
            "cluster_checked_at",
        ] {
            let result = conn
                .execute_unprepared(&format!("SELECT {column} FROM qa_platforms"))
                .await;
            assert!(
                result.is_err(),
                "{column} must be gone after down(), but the SELECT succeeded"
            );
        }
        conn.execute_unprepared("SELECT observed_version FROM qa_platforms")
            .await
            .expect("down() must not touch observed_version");
    }

    /// `sql_for` maps each backend to its own statement -- nothing else calls
    /// it, and the suite only ever runs migrations against `SQLite`, so
    /// without this guard a swapped `Postgres`/`MySql` arm is invisible to
    /// every other test in this module.
    ///
    /// Unlike `m20260814_000006_platform_default_branch`'s equivalent guard,
    /// all three blobs here are genuinely distinct (that migration's Postgres
    /// and `MySQL` blobs are byte-identical, so its guard can only pin the
    /// declared type, not constant identity). Here a swap is both detectable
    /// and consequential: Postgres would silently lose the timezone on
    /// `cluster_checked_at` (`TIMESTAMPTZ` becomes `TIMESTAMP`), and `MySQL`
    /// would receive `JSONB`, a type it does not have, which fails loudly at
    /// deploy time rather than at this test -- so this guard pins constant
    /// identity directly and backs it up with the declared-type assertions a
    /// bare identity check would not explain to a future reader.
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
            postgres.contains("JSONB"),
            "Postgres must get JSONB for cluster_nodes"
        );
        assert!(
            postgres.contains("TIMESTAMPTZ"),
            "Postgres must get TIMESTAMPTZ for cluster_checked_at"
        );

        let mysql = super::sql_for(DatabaseBackend::MySql);
        assert!(
            mysql.contains("cluster_nodes JSON NULL"),
            "MySQL must get the bare JSON statement for cluster_nodes"
        );
        assert!(!mysql.contains("JSONB"), "MySQL must not get Postgres' JSONB");
        assert!(
            mysql.contains("cluster_checked_at TIMESTAMP NULL"),
            "MySQL must get the bare TIMESTAMP statement for cluster_checked_at"
        );
        assert!(
            !mysql.contains("TIMESTAMPTZ"),
            "MySQL must not get Postgres' TIMESTAMPTZ -- swapping the Postgres \
             and MySql arms would silently drop the timezone on every stored \
             check time, and this line is what catches it"
        );

        let sqlite = super::sql_for(DatabaseBackend::Sqlite);
        assert!(
            sqlite.contains("cluster_nodes TEXT"),
            "SQLite must get TEXT for cluster_nodes, having no native JSON type"
        );
        assert!(
            sqlite.contains("cluster_checked_at TEXT"),
            "SQLite must get TEXT for cluster_checked_at, having no native \
             temporal type"
        );
        assert!(!sqlite.contains("JSON"), "SQLite must not get a server JSON type");
        assert!(
            !sqlite.contains("TIMESTAMP"),
            "SQLite must not get a server temporal type"
        );
    }
}
