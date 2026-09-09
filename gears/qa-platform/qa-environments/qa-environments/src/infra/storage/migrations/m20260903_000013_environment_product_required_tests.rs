//! Tests for the `product_id NOT NULL` contract migration.
//!
//! **The `SQLite` arm is the one that matters here**: it is the only dialect
//! that cannot `ALTER COLUMN … SET NOT NULL`, so it is the only one that
//! rebuilds the table — and a rebuild is where rows, indexes and referencing
//! tables get silently lost. The Postgres and `MySQL` arms are one statement
//! each and are unexercised, exactly as every other migration in this gear
//! leaves them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, TransactionTrait,
    TryGetable,
};
use sea_orm_migration::{MigrationName, MigrationTrait, MigratorTrait, SchemaManager};
use uuid::Uuid;

use super::super::legacy_row::{insert, text, uuid_lit};

const STAMP: &str = "2026-09-05 00:00:00+00:00";

async fn connect() -> DatabaseConnection {
    let mut opts = ConnectOptions::new("sqlite::memory:".to_owned());
    opts.max_connections(1).min_connections(1);
    Database::connect(opts)
        .await
        .expect("failed to connect to in-memory sqlite database")
}

/// Every migration up to but excluding this one.
async fn db_migrated_up_to_this_one() -> DatabaseConnection {
    let conn = connect().await;
    let manager = SchemaManager::new(&conn);
    let this = MigrationName::name(&super::Migration);
    for migration in super::super::Migrator::migrations() {
        if MigrationName::name(&*migration) == this {
            return conn;
        }
        migration
            .up(&manager)
            .await
            .expect("failed to run a qa-environments migration");
    }
    panic!("this migration is not registered in the Migrator");
}

/// Run this migration the way the runner does — **inside a transaction**.
///
/// `toolkit_db::migration_runner::run_gear_migrations` opens one per migration
/// (`libs/toolkit-db/src/migration_runner.rs:357-366`) in production and under
/// `run_migrations_for_testing` alike. Measuring `up()` on a bare connection
/// measures a path nothing uses, which is how `m20260903_000004` shipped with a
/// foreign-key mechanism that does nothing.
async fn run_it(conn: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    let txn = conn.begin().await.expect("failed to open a transaction");
    match MigrationTrait::up(&super::Migration, &SchemaManager::new(&txn)).await {
        Ok(()) => txn.commit().await,
        Err(error) => {
            txn.rollback().await.expect("rollback failed");
            Err(error)
        }
    }
}

async fn plant_environment(conn: &DatabaseConnection, id: u128, product: Option<u128>, name: &str) {
    let product = product.map_or_else(|| "NULL".to_owned(), |p| uuid_lit(Uuid::from_u128(p)));
    insert(
        conn,
        "qa_environments",
        &[
            ("id", uuid_lit(Uuid::from_u128(id))),
            ("tenant_id", uuid_lit(Uuid::from_u128(7))),
            ("name", text(name)),
            ("product_id", product),
            ("available", "1".to_owned()),
            ("is_default", "0".to_owned()),
            ("credentials", text("[]")),
            ("observed_attrs", text("{}")),
            ("config", text("{}")),
            ("health_state", text("unknown")),
            ("created_at", text(STAMP)),
            ("updated_at", text(STAMP)),
        ],
    )
    .await;
}

async fn scalar<T: TryGetable>(conn: &DatabaseConnection, sql: &str) -> T {
    conn.query_one_raw(Statement::from_string(
        conn.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .unwrap()
    .expect("the query returned no row")
    .try_get_by_index::<T>(0)
    .expect("unexpected column type")
}

/// The constraint is in force afterwards: a `NULL` insert is rejected.
#[tokio::test]
async fn after_the_migration_an_environment_cannot_omit_its_product() {
    let conn = db_migrated_up_to_this_one().await;
    plant_environment(&conn, 1, Some(0x10), "prod").await;
    run_it(&conn).await.expect("the migration must run");

    let unbound = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "INSERT INTO qa_environments \
                 (id, tenant_id, name, product_id, available, is_default, created_at, updated_at) \
                 VALUES ({}, {}, 'later', NULL, 1, 0, '{STAMP}', '{STAMP}');",
                uuid_lit(Uuid::from_u128(0x99)),
                uuid_lit(Uuid::from_u128(7)),
            ),
        ))
        .await;
    assert!(
        unbound.is_err(),
        "product_id must be NOT NULL after this migration"
    );
}

/// The pre-check refuses, and names the offending ids rather than letting the
/// rebuild fail opaquely.
#[tokio::test]
async fn a_productless_row_halts_the_migration_and_is_named() {
    let conn = db_migrated_up_to_this_one().await;
    plant_environment(&conn, 2, Some(0x10), "bound").await;
    plant_environment(&conn, 3, None, "unbound").await;

    let error = run_it(&conn).await.expect_err("the migration must refuse");
    let text = error.to_string();
    assert!(
        text.contains(&Uuid::from_u128(3).to_string()),
        "the refusal must name the offending environment: {text}"
    );
    assert!(
        !text.contains(&Uuid::from_u128(2).to_string()),
        "and must not name a row that is fine: {text}"
    );
}

/// Every row survives the rebuild with its values intact.
#[tokio::test]
async fn the_rebuild_carries_every_row_and_every_value_across() {
    let conn = db_migrated_up_to_this_one().await;
    plant_environment(&conn, 4, Some(0x10), "alpha").await;
    plant_environment(&conn, 5, Some(0x20), "beta").await;
    run_it(&conn).await.expect("the migration must run");

    let count: i64 = scalar(&conn, "SELECT COUNT(*) FROM qa_environments;").await;
    assert_eq!(count, 2, "no row may be lost in the rebuild");

    let name: String = scalar(
        &conn,
        &format!(
            "SELECT name FROM qa_environments WHERE id = {};",
            uuid_lit(Uuid::from_u128(5))
        ),
    )
    .await;
    assert_eq!(name, "beta", "and values must land in the right columns");

    let product: Uuid = scalar(
        &conn,
        &format!(
            "SELECT product_id FROM qa_environments WHERE id = {};",
            uuid_lit(Uuid::from_u128(4))
        ),
    )
    .await;
    assert_eq!(
        product,
        Uuid::from_u128(0x10),
        "including the one this migration is about"
    );
}

/// The unique index survives.
///
/// A rebuild that silently lost `idx_qa_environments_tenant_name` would take
/// the "at most one environment per (tenant, name)" rule with it, and nothing
/// else in this gear enforces that.
#[tokio::test]
async fn the_rebuild_recreates_the_unique_index() {
    let conn = db_migrated_up_to_this_one().await;
    plant_environment(&conn, 6, Some(0x10), "prod").await;
    run_it(&conn).await.expect("the migration must run");

    let present: i64 = scalar(
        &conn,
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
         AND name = 'idx_qa_environments_tenant_name';",
    )
    .await;
    assert_eq!(present, 1, "the index must exist in sqlite_master");

    // Present in `sqlite_master` is not the same as built on the right columns,
    // so the rule itself is exercised too.
    let duplicate = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "INSERT INTO qa_environments \
                 (id, tenant_id, name, product_id, available, is_default, created_at, updated_at) \
                 VALUES ({}, {}, 'prod', {}, 1, 0, '{STAMP}', '{STAMP}');",
                uuid_lit(Uuid::from_u128(0x98)),
                uuid_lit(Uuid::from_u128(7)),
                uuid_lit(Uuid::from_u128(0x10)),
            ),
        ))
        .await;
    assert!(
        duplicate.is_err(),
        "the index must still REJECT a duplicate (tenant, name) -- an index present \
         in sqlite_master but built on the wrong columns passes the check above and \
         fails here"
    );
}

/// **The mechanism, measured on the path that uses it.**
///
/// `qa_environment_variables` and `qa_environment_leases` both reference
/// `qa_environments(id)`, and the rebuild `DROP`s the referenced table.
/// `PRAGMA foreign_keys = OFF` is a documented no-op inside a transaction, and
/// the runner opens one — so a rebuild relying on that pragma alone fails here
/// with `FOREIGN KEY constraint failed`, which is exactly what
/// `m20260903_000004` would have done had anything tested it this way.
///
/// The deferred check runs at COMMIT, so `run_it` asserting on the commit as
/// well as on `up()` is what makes this faithful: a test that stopped at `up()`
/// would pass with the deferral never verified.
#[tokio::test]
async fn the_rebuild_survives_referencing_rows_inside_the_runners_transaction() {
    let conn = db_migrated_up_to_this_one().await;
    plant_environment(&conn, 7, Some(0x10), "prod").await;
    insert(
        &conn,
        "qa_environment_variables",
        &[
            ("id", uuid_lit(Uuid::from_u128(0x70))),
            ("tenant_id", uuid_lit(Uuid::from_u128(7))),
            ("platform_id", uuid_lit(Uuid::from_u128(7))),
            ("name", text("VPADM_NAMESPACE")),
            ("value", text("virtuozzo")),
            ("created_at", text(STAMP)),
            ("updated_at", text(STAMP)),
        ],
    )
    .await;

    // The precondition: without it the assertion below could pass on a
    // database that never had a referencing row in the first place.
    let before: i64 = scalar(&conn, "SELECT COUNT(*) FROM qa_environment_variables;").await;
    assert_eq!(
        before, 1,
        "the fixture must actually have planted a child row"
    );

    run_it(&conn).await.expect(
        "the rebuild must survive a referencing row INSIDE A TRANSACTION -- a \
         `FOREIGN KEY constraint failed` here means the suspension mechanism does not \
         work on the path the migration runner uses",
    );

    let variables: i64 = scalar(&conn, "SELECT COUNT(*) FROM qa_environment_variables;").await;
    assert_eq!(variables, 1, "the referencing row must survive too");
}

/// `down()` widens the column back, and a `NULL` is accepted again.
#[tokio::test]
async fn down_makes_the_column_nullable_again() {
    let conn = db_migrated_up_to_this_one().await;
    plant_environment(&conn, 8, Some(0x10), "prod").await;
    run_it(&conn).await.expect("the migration must run");

    let txn = conn.begin().await.unwrap();
    MigrationTrait::down(&super::Migration, &SchemaManager::new(&txn))
        .await
        .expect("down must run");
    txn.commit().await.unwrap();

    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        format!(
            "INSERT INTO qa_environments \
             (id, tenant_id, name, product_id, available, is_default, created_at, updated_at) \
             VALUES ({}, {}, 'later', NULL, 1, 0, '{STAMP}', '{STAMP}');",
            uuid_lit(Uuid::from_u128(0x97)),
            uuid_lit(Uuid::from_u128(7)),
        ),
    ))
    .await
    .expect("after down() a productless row must be insertable again");
}
