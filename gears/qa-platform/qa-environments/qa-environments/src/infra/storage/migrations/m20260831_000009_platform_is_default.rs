//! Adds `qa_platforms.is_default`, the per-product default platform flag.
//!
//! **Added 2026-08-31 by user decision.** The Run Test Plan and Create Schedule
//! dialogs offer a **"Default cluster"** option. In the source system that option
//! meant *no platform at all*: `manager/src/routes/runs.rs` resolves the platform
//! context to `(None, None, None, None)` when none is named, and
//! `manager/src/services/argo.rs` mounts a kubeconfig only `if let
//! Some(platform_name)`, so the runner fell through to its own in-cluster
//! `ServiceAccount`. That worked because the source system's manager was deployed
//! **inside** the cluster under test (`README.md`: manager in `vhp-tests`, the
//! product in `vhp-platform`).
//!
//! qa-platform is deployed *beside* the clusters it tests rather than inside one,
//! so "the cluster I am running in" holds no product and the source system's
//! meaning is dead here — a "Default cluster" run dispatched, executed and failed
//! every test on missing credentials, which is the report that prompted this.
//! **The decision was that "Default cluster" resolves to the product's default
//! platform instead.** That is a deliberate divergence from the source system and
//! is recorded as one.
//!
//! ## Why a column and not "the product's only platform"
//!
//! The first cut resolved the option to the product's platform when the product
//! had exactly one, which needed no schema at all. It is still the fallback, and
//! it answers identically while a product has one platform. It cannot answer at
//! all once a product has two — there is no defensible way to choose from data
//! alone, and choosing the first would silently run a suite against an
//! environment nobody picked. The user asked for the explicit flag for exactly
//! that case.
//!
//! ## `BOOLEAN NOT NULL DEFAULT FALSE`, and why NOT NULL is safe here
//!
//! Contrast `m20260814_000006_platform_default_branch`, which is nullable because
//! "no override" is a real third state. There is no third state for this flag: a
//! platform either is its product's default or is not, and every row that exists
//! when this migration runs is not. A `DEFAULT FALSE` therefore backfills every
//! existing row correctly, which is also what makes `NOT NULL` addable to a
//! populated table on all three dialects — `SQLite` rejects a `NOT NULL` column
//! added without a default, and this one has one.
//!
//! ## The uniqueness rule is enforced in the service, not by a partial index
//!
//! "At most one default per product" is the invariant. A partial unique index
//! (`... ON qa_platforms (tenant_id, product_id) WHERE is_default`) would pin it
//! in the database, and it was considered and rejected here for two reasons:
//!
//! * **`MySQL` has no partial indexes**, so the constraint would exist on two of
//!   the three dialects and the service would need the check anyway for the
//!   third. One enforcement point that always runs beats two that disagree by
//!   dialect.
//! * The intended operator gesture is **"make this one the default"**, not "fail
//!   because another already is". `EnvironmentsService` clears the previous holder
//!   itself, so a unique index would only ever fire on a bug in that clear — and
//!   it would fire as a `DbErr` mid-write rather than a named error.
//!
//! Note that the clear and the promotion are **two statements, not one**: this
//! gear has no transaction seam (there is no `begin()` anywhere in it). The
//! service therefore clears *first*, so a failure between the two leaves the
//! product with zero defaults — recoverable, and the dialogs fall back — rather
//! than two, which would make "Default cluster" resolve arbitrarily. See
//! `EnvironmentsRepository::clear_default_for_product`.
//!
//! The service-side rule and its test are therefore the contract; this migration
//! only provides the column. Stated explicitly because "no constraint in the
//! schema" reads as an oversight otherwise.
//!
//! ## A second migration, not an edit to an earlier one
//!
//! For the reason `m20260814_000006_platform_default_branch` records at length:
//! every earlier migration must be assumed to have run wherever this gear is
//! deployed, and the runner records it as applied, so editing one changes nothing
//! on an existing database while diverging from what that database contains. The
//! numbering (`000009`) continues the qa-platform subsystem's shared sequence
//! after `000008`.
//!
//! ## No `IF NOT EXISTS` on the `ALTER`
//!
//! Same reason as `000004` and `000006`: `SQLite` rejects `ALTER TABLE … ADD
//! COLUMN IF NOT EXISTS` outright, `MySQL`'s grammar has no such clause, and the
//! migration runner's apply-once guarantee is the one that actually holds.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN is_default BOOLEAN NOT NULL DEFAULT FALSE;
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN is_default BOOLEAN NOT NULL DEFAULT FALSE;
";

// `SQLite` has no BOOLEAN type of its own -- it stores one as an INTEGER, which
// is what `SeaORM` maps `bool` onto for this backend. `0` is the false default.
const SQLITE_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN is_default BOOLEAN NOT NULL DEFAULT 0;
";

/// Pick the statement for a backend.
///
/// Extracted from `up()` so it can be tested, for the reason
/// `m20260814_000006_platform_default_branch::sql_for` records: the gear's tests
/// only run `SQLite`, so a swapped match arm leaves every test green while
/// handing the server dialects the wrong statement.
fn sql_for(backend: sea_orm::DatabaseBackend) -> Result<&'static str, DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(POSTGRES_UP),
        sea_orm::DatabaseBackend::MySql => Ok(MYSQL_UP),
        sea_orm::DatabaseBackend::Sqlite => Ok(SQLITE_UP),
        other => Err(DbErr::Migration(format!(
            "unsupported database backend: {other:?}"
        ))),
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = sql_for(manager.get_database_backend())?;
        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("ALTER TABLE qa_platforms DROP COLUMN is_default;")
            .await?;
        Ok(())
    }
}

/// Schema tests for the added column.
///
/// `cargo build` proves nothing about a `SeaORM` entity — its table and column
/// names are runtime strings — so only a query shows the column exists. Every
/// round-trip below writes a **non-default** value (`true`) rather than relying on
/// the column default, because `false` is what a dropped field also produces and
/// would survive almost any mistake unchanged.
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

    fn without_comments(sql: &str) -> String {
        sql.lines()
            .map(|line| line.split("--").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// This gear's other tables, plus one belonging to qa-catalog. Altering a
    /// table that *exists* succeeds on a server and fails only later, when the
    /// column turns out not to be on `qa_platforms`.
    const OTHER_TABLES: [&str; 4] = [
        "qa_platform_variables",
        "qa_pipeline_variables",
        "qa_platform_leases",
        "qa_products",
    ];

    #[test]
    fn every_dialect_blob_alters_qa_platforms_and_declares_the_column_not_null() {
        for (dialect, raw) in [
            ("POSTGRES_UP", super::POSTGRES_UP),
            ("MYSQL_UP", super::MYSQL_UP),
            ("SQLITE_UP", super::SQLITE_UP),
        ] {
            let sql = without_comments(raw);

            assert!(
                sql.contains("qa_platforms"),
                "{dialect} must alter qa_platforms; this file was copied from \
                 another migration, so the table name is what a copy-paste gets wrong"
            );
            for other in OTHER_TABLES {
                assert!(!sql.contains(other), "{dialect} must not touch {other}");
            }
            assert!(
                sql.contains("ADD COLUMN is_default"),
                "{dialect} must add is_default; a rewrite that patched only the \
                 first blob is how two dialects drift apart unnoticed"
            );
            // The flag has no third state, so unlike `default_branch` this column
            // is NOT NULL -- and it can only be NOT NULL on a populated table
            // because it carries a default.
            assert!(
                sql.contains("NOT NULL"),
                "{dialect} must declare is_default NOT NULL: a platform either is \
                 its product's default or is not, and there is no third state"
            );
            assert!(
                sql.contains("DEFAULT"),
                "{dialect} must carry a DEFAULT, which is what backfills existing \
                 rows and what makes NOT NULL addable to a populated table at all"
            );
        }
    }

    /// M7's shape: swapping the `Postgres` and `Sqlite` arms leaves every test
    /// green, because every executed test runs on `SQLite`.
    #[test]
    fn each_backend_gets_a_statement_with_the_right_default_literal() {
        use sea_orm::DatabaseBackend;

        for backend in [DatabaseBackend::Postgres, DatabaseBackend::MySql] {
            let sql = without_comments(
                super::sql_for(backend).expect("dispatch covers every backend this build compiles"),
            );
            assert!(
                sql.contains("DEFAULT FALSE"),
                "{backend:?} must get the server statement spelling the literal FALSE"
            );
        }

        let sqlite = without_comments(
            super::sql_for(DatabaseBackend::Sqlite)
                .expect("dispatch covers every backend this build compiles"),
        );
        assert!(
            sqlite.contains("DEFAULT 0"),
            "SQLite must get the integer-literal statement"
        );
        assert!(
            !sqlite.contains("DEFAULT FALSE"),
            "and must not get a server blob: the arms are adjacent and similar, \
             which is what makes swapping them a plausible edit"
        );
    }

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
    async fn plant(conn: &DatabaseConnection, is_default: bool) {
        super::super::legacy_row::plant_environment(
            conn,
            "qa_environments",
            uuid(1),
            uuid(2),
            "staging-a",
            &[
                ("observed_version", super::super::legacy_row::text("5.0.1")),
                ("observed_build", super::super::legacy_row::text("20260813")),
                ("is_default", i32::from(is_default).to_string()),
            ],
        )
        .await;
    }

    /// One INTEGER column of the planted row. `is_default` is a `BOOLEAN`,
    /// which `SQLite` stores as `INTEGER`, so reading it as text fails.
    async fn int_col(conn: &DatabaseConnection, column: &str) -> Option<i32> {
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
        .try_get_by_index::<Option<i32>>(0)
        .expect("the column must be readable as an integer")
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

    /// A `true` survives the round trip. Written as `true` deliberately: `false`
    /// is also what a dropped field produces, so a false-only fixture proves
    /// nothing.
    #[tokio::test]
    async fn is_default_round_trips_through_the_migrated_schema() {
        let conn = migrated_db().await;

        plant(&conn, true).await;

        assert_eq!(
            int_col(&conn, "is_default").await,
            Some(1),
            "is_default must survive the round trip; a false-only fixture would \
             not have proven this"
        );
        assert_eq!(
            col(&conn, "observed_build").await.as_deref(),
            Some("20260813"),
            "and the columns added before it must be unaffected by the ALTER"
        );
    }

    /// The flag has to reach the **SDK model**, which is what every consumer
    /// outside this gear reads. Dropping it in `mapper::environment_to_sdk`
    /// (`is_default: false`) leaves the row itself perfectly correct.
    #[tokio::test]
    async fn is_default_reaches_the_sdk_model() {
        let conn = migrated_db().await;

        plant(&conn, true).await;

        let stored = environment::Entity::find()
            .one(&conn)
            .await
            .unwrap()
            .expect("the environment row must read back");
        assert!(environment_to_sdk(stored).is_default);
    }

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

        conn.execute_unprepared("SELECT is_default FROM qa_platforms")
            .await
            .expect_err("is_default must be gone after down()");
        conn.execute_unprepared("SELECT default_branch FROM qa_platforms")
            .await
            .expect("down() must not touch default_branch");
    }
}
