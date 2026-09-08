//! Adds `qa_platforms.observed_build`, the build identifier alongside
//! `observed_version`.
//!
//! **Added 2026-08-13 by user decision, raised by qa-runs Task 9.** qa-runs
//! snapshots the target platform's version *and build* onto the run row at
//! launch and feeds them to the runner as `APP_VERSION` / `APP_BUILD`. The
//! source system has both — `platforms_meta.version` and `platforms_meta.build`
//! (`manager/migrations/001_initial.sql:142-143`), copied onto every run
//! (`manager/src/routes/runs.rs:579`, and a backfill at
//! `001_initial.sql:169-172`) — but this gear shipped with only
//! `observed_version`. Left alone, `APP_BUILD` would reach every test empty and
//! `PRD.md:577`'s "environment variable names consumed by tests … are
//! unchanged, so existing test repositories run unmodified" would be partially
//! false.
//!
//! ## A second migration, not an edit to the first
//!
//! `m20260812_000001_initial` has run in every environment that has this gear,
//! and the migration runner records it as applied — editing it would change
//! nothing on any existing database while silently diverging from what those
//! databases actually contain. The numbering (`000004`) continues the
//! qa-platform subsystem's shared sequence: `000001` qa-environments,
//! `000002` qa-catalog, `000003` qa-runs.
//!
//! ## No `IF NOT EXISTS` on the `ALTER`
//!
//! **`SQLite` rejects it outright** — `ALTER TABLE … ADD COLUMN IF NOT EXISTS`
//! fails with `near "EXISTS": syntax error`, confirmed by running exactly that
//! against the in-memory database the tests below use. `MySQL`'s `ALTER TABLE`
//! grammar has no `IF NOT EXISTS` either (it is a `MariaDB` extension), though
//! no test here executes `MySQL` to prove it. Postgres does accept the clause.
//!
//! Rather than write one dialect defensively and two bare, all three are bare:
//! the migration runner applies each migration exactly once, which is the
//! actual guarantee. The initial migration's `CREATE TABLE IF NOT EXISTS` is
//! belt-and-braces of the same kind and is equally not load-bearing.
//!
//! ## This column has no writer, and that is the pre-existing state
//!
//! **Superseded 2026-08-28:** `OrmEnvironmentsRepository::record_observation`
//! (`infra/storage/environments_sea_repo.rs`) is now that writer, reached from
//! `EnvironmentsService::observe_environment` on both the refresh endpoint and the
//! observation ticker. The paragraph below is kept as the record of what was
//! true when this migration was written — `entity/environment.rs` cites it — but
//! it no longer describes the code.
//!
//! Nothing writes `observed_build` — exactly as nothing writes
//! `observed_version`. The version poller was claimed by feature 2.1's scope
//! and never implemented; `DECOMPOSITION.md` 2.1 tracks that gap and now names
//! both columns, so one retrofit closes both. This migration deliberately does
//! **not** build the poller.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Mirrors `observed_version`'s declaration in
/// `m20260812_000001_initial`: nullable, `VARCHAR(255)` on the two server
/// dialects and `TEXT` on `SQLite`.
const POSTGRES_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN observed_build VARCHAR(255) NULL;
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN observed_build VARCHAR(255) NULL;
";

const SQLITE_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN observed_build TEXT NULL;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
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
        conn.execute_unprepared("ALTER TABLE qa_platforms DROP COLUMN observed_build;")
            .await?;
        Ok(())
    }
}

/// Schema tests for the added column.
///
/// **`cargo build` proves nothing about a `SeaORM` entity**: its table and
/// column names are runtime strings, so `environment::Model::observed_build`
/// compiling says nothing about whether a column of that name exists. Only a
/// query does.
///
/// The gear's existing DB-backed tests (`domain::service::tests_tenant_scoping`
/// through `test_support::inmem_db`) do catch a *missing* column, because
/// `OrmEnvironmentsRepository::create` names every column in its `INSERT` —
/// verified by renaming the column in `SQLITE_UP` alone, which turns six of
/// them red alongside all four below.
///
/// What they do **not** catch is a value silently lost on the way out, because
/// no DB-backed fixture in this gear ever writes a non-`NULL`
/// `observed_version` or `observed_build`, and a `NULL` survives almost any
/// mistake unchanged. Verified the same way: dropping the field in
/// `mapper::environment_to_sdk` (`observed_build: None`) leaves **49 of 50 tests
/// green**, and the one that fails is
/// `observed_build_reaches_the_sdk_model` below. That mutation is this
/// module's reason to exist.
///
/// `clippy::disallowed_methods` is allowed here for the same narrow reason as
/// in qa-runs' migration: a schema test needs a raw connection, and the
/// `SecureORM` wrappers deliberately expose none. Nothing here is a production
/// path.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, EntityTrait};
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    use uuid::Uuid;

    use crate::infra::storage::entity::environment;
    use crate::infra::storage::mapper::environment_to_sdk;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// In-memory `SQLite` with **both** migrations applied in order, which is
    /// also what proves `Migrator::migrations()` lists the new one: an
    /// unregistered migration leaves the column absent and every assertion
    /// below fails.
    ///
    /// `max_connections(1)` because each `SQLite` `:memory:` connection is its
    /// own database.
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

    /// Plant a row with raw SQL.
    ///
    /// **Not the entity.** Task 19's `m20260903_000012` dropped
    /// `kubeconfig_credstore_ref` and `environment::Model` lost the field in
    /// the same commit, so an entity insert omits a `NOT NULL` column that
    /// still exists at this migration's point in history. Reads below still go
    /// through the entity: this migration's own column survives Task 19, so
    /// the model can express it.
    async fn plant(conn: &DatabaseConnection, observed_build: Option<&str>) {
        super::super::legacy_row::plant_environment(
            conn,
            "qa_environments",
            uuid(1),
            uuid(2),
            "staging-a",
            &[
                ("observed_version", super::super::legacy_row::text("5.0.1")),
                (
                    "observed_build",
                    super::super::legacy_row::maybe(observed_build),
                ),
            ],
        )
        .await;
    }

    /// One column of the planted row, as text (`NULL` reads as `None`).
    ///
    /// Raw SQL on the read side too: the row is planted with raw SQL (the
    /// entity cannot express `kubeconfig_credstore_ref` any more), and mixing
    /// a raw insert with an entity read makes the test depend on the two
    /// agreeing about how a `Uuid` is bound.
    async fn col(conn: &DatabaseConnection, column: &str) -> Option<String> {
        conn.query_one_raw(sea_orm::Statement::from_string(
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

    /// The column exists under exactly the name the entity uses, and a value
    /// written to it comes back unchanged.
    ///
    /// Break-tested: renaming the column in `SQLITE_UP` alone — leaving the
    /// entity untouched, so the crate still compiles — turns this red.
    #[tokio::test]
    async fn observed_build_round_trips_through_the_migrated_schema() {
        let conn = migrated_db().await;

        plant(&conn, Some("20260813")).await;

        assert_eq!(
            col(&conn, "observed_build").await.as_deref(),
            Some("20260813"),
            "observed_build must survive the round trip; a NULL-only fixture \
             would not have proven this"
        );
        assert_eq!(
            col(&conn, "observed_version").await.as_deref(),
            Some("5.0.1"),
            "and its twin must be unaffected by the ALTER"
        );
    }

    /// Nullable, exactly like `observed_version` — an environment whose build was
    /// never observed is the normal case, since nothing writes either column
    /// yet.
    #[tokio::test]
    async fn observed_build_is_nullable_like_observed_version() {
        let conn = migrated_db().await;

        plant(&conn, None).await;

        assert_eq!(col(&conn, "observed_build").await, None);
    }

    /// The column has to reach the SDK model, not merely the database — that is
    /// the whole point of adding it, since `qa_runs` reads
    /// `Environment::observed_build` and nothing else.
    #[tokio::test]
    async fn observed_build_reaches_the_sdk_model() {
        let conn = migrated_db().await;

        plant(&conn, Some("20260813")).await;

        let stored = environment::Entity::find()
            .one(&conn)
            .await
            .unwrap()
            .expect("the environment row must read back");
        let sdk = environment_to_sdk(stored);
        assert_eq!(sdk.observed_build.as_deref(), Some("20260813"));
    }

    /// `down()` has to actually drop the column, or a rollback leaves a schema
    /// the previous release's entity cannot read.
    #[tokio::test]
    async fn the_down_migration_drops_the_column() {
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

        conn.execute_unprepared("SELECT observed_build FROM qa_platforms")
            .await
            .expect_err("observed_build must be gone after down()");
        conn.execute_unprepared("SELECT observed_version FROM qa_platforms")
            .await
            .expect("down() must not touch observed_version");
    }
}
