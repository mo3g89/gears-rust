//! Tests for the `plugin_instance_id NOT NULL` contract migration.
//!
//! `cargo build` proves nothing about a migration — its SQL is a runtime
//! string — so every claim here is made by running it against a real (if
//! in-memory) database and then asking the database.
//!
//! **These run on `SQLite`, which is the one dialect that cannot
//! `ALTER COLUMN ... SET NOT NULL` and therefore the one whose arm rebuilds
//! the table.** That makes this module the only executable check that the
//! rebuild preserves what it is supposed to: the rows, the indexes and the
//! referencing table. The Postgres and `MySQL` arms are one statement each and
//! are unexercised here, exactly as every other migration in this gear leaves
//! them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, TryGetable,
};
use sea_orm_migration::{MigrationName, MigrationTrait, MigratorTrait, SchemaManager};
use uuid::Uuid;

async fn connect() -> DatabaseConnection {
    let mut opts = ConnectOptions::new("sqlite::memory:".to_owned());
    opts.max_connections(1).min_connections(1);
    Database::connect(opts)
        .await
        .expect("failed to connect to in-memory sqlite database")
}

/// Every migration of this gear **up to but excluding this one**, so a row
/// that names no plugin can be planted before the constraint exists.
///
/// Keyed on this migration's own name rather than on a position, for the
/// reason `m20260903_000003`'s equivalent gives.
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
            .expect("failed to run a qa-catalog migration");
    }
    panic!("this migration is not registered in the Migrator");
}

/// Insert a product, optionally naming a plugin.
///
/// Raw SQL rather than the entity: the entity's `plugin_instance_id` is
/// non-optional as of this task, so it cannot express the unbound row this
/// migration exists to refuse.
async fn insert_product(conn: &DatabaseConnection, id: u128, name: &str, plugin: Option<&str>) {
    let plugin = plugin.map_or_else(|| "NULL".to_owned(), |value| format!("'{value}'"));
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        format!(
            "INSERT INTO qa_products \
             (id, tenant_id, name, product_key, description, folder, created_at, updated_at, \
             plugin_instance_id) \
             VALUES ('{}', '{}', '{name}', '{}', 'desc-{name}', 'folder-{name}', \
             '2026-09-03 00:00:00+00:00', '2026-09-04 00:00:00+00:00', {plugin});",
            Uuid::from_u128(id),
            Uuid::from_u128(7),
            name.to_uppercase(),
        ),
    ))
    .await
    .expect("failed to insert the product row");
}

async fn scalar<T: TryGetable>(conn: &DatabaseConnection, sql: &str) -> T {
    conn.query_one_raw(Statement::from_string(
        conn.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .expect("the query must run")
    .expect("the query must return a row")
    .try_get_by_index::<T>(0)
    .expect("the column must decode")
}

const BOUND: &str =
    "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1";

/// The point of the migration: after it, the column refuses a `NULL`.
///
/// Asserted by *attempting the insert*, not by reading the schema — a
/// `CREATE TABLE` whose text says `NOT NULL` and a table that enforces it are
/// different claims, and the rebuild makes the difference reachable.
#[tokio::test]
async fn after_the_migration_a_product_cannot_omit_its_plugin() {
    let conn = db_migrated_up_to_this_one().await;

    // Before: allowed, which is what makes the assertion below meaningful.
    insert_product(&conn, 1, "before", None).await;
    conn.execute_unprepared("DELETE FROM qa_products;")
        .await
        .unwrap();

    MigrationTrait::up(&super::Migration, &SchemaManager::new(&conn))
        .await
        .expect("the migration must apply to an empty table");

    let refused = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "INSERT INTO qa_products \
                 (id, tenant_id, name, product_key, description, folder, created_at, updated_at, \
                 plugin_instance_id) \
                 VALUES ('{}', '{}', 'after', 'AFTER', '', NULL, \
                 '2026-09-03 00:00:00+00:00', '2026-09-03 00:00:00+00:00', NULL);",
                Uuid::from_u128(2),
                Uuid::from_u128(7),
            ),
        ))
        .await;

    assert!(
        refused.is_err(),
        "the column must reject a NULL after the migration -- a rebuild whose \
         CREATE TABLE text says NOT NULL but whose table does not enforce it \
         would pass a schema-reading test and fail here"
    );
}

/// `up()` refuses **before** altering anything, and names the rows to fix.
///
/// This is the FW-1 window: a deployment whose UI created products between
/// `000003` and this migration has rows naming no plugin, and an `ALTER` that
/// merely failed would tell an operator that *some* row is null without
/// saying which.
#[tokio::test]
async fn up_refuses_loudly_and_names_every_unbound_product() {
    let conn = db_migrated_up_to_this_one().await;
    insert_product(&conn, 0x11, "bound", Some(BOUND)).await;
    insert_product(&conn, 0x22, "unbound-a", None).await;
    insert_product(&conn, 0x33, "unbound-b", None).await;

    let err = MigrationTrait::up(&super::Migration, &SchemaManager::new(&conn))
        .await
        .expect_err("a table with unbound products must not be tightened");

    let message = err.to_string();
    for unbound in [Uuid::from_u128(0x22), Uuid::from_u128(0x33)] {
        assert!(
            message.contains(&unbound.to_string()),
            "every offending id must be named, {unbound} was not: {message}"
        );
    }
    assert!(
        !message.contains(&Uuid::from_u128(0x11).to_string()),
        "and a bound product must not be named: {message}"
    );
    assert!(
        message.contains("GET /qa/v1/product-plugins"),
        "the operator must be told where a valid value comes from: {message}"
    );

    // Nothing was altered: the column still accepts a NULL, so the refusal
    // left the schema exactly as it was.
    insert_product(&conn, 0x44, "still-nullable", None).await;
}

/// The rebuild preserves **every column of every row**, not just the count.
///
/// The `INSERT ... SELECT` names its columns explicitly so a mismatch is an
/// error rather than a silent one-field shift; this is the assertion that
/// would catch it if that ever stopped being true.
#[tokio::test]
async fn the_rebuild_preserves_every_row_verbatim() {
    let conn = db_migrated_up_to_this_one().await;
    insert_product(&conn, 0x55, "keep", Some(BOUND)).await;

    MigrationTrait::up(&super::Migration, &SchemaManager::new(&conn))
        .await
        .expect("the migration must apply to a populated table");

    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT tenant_id, name, product_key, description, folder, created_at, updated_at, \
             plugin_instance_id FROM qa_products;"
                .to_owned(),
        ))
        .await
        .unwrap()
        .expect("the row must survive the rebuild");

    assert_eq!(
        row.try_get_by_index::<String>(0).unwrap(),
        Uuid::from_u128(7).to_string(),
        "tenant_id"
    );
    assert_eq!(row.try_get_by_index::<String>(1).unwrap(), "keep", "name");
    assert_eq!(
        row.try_get_by_index::<String>(2).unwrap(),
        "KEEP",
        "product_key"
    );
    assert_eq!(
        row.try_get_by_index::<String>(3).unwrap(),
        "desc-keep",
        "description"
    );
    assert_eq!(
        row.try_get_by_index::<String>(4).unwrap(),
        "folder-keep",
        "folder -- the one NULLable column, so a shifted copy shows up here first"
    );
    assert_eq!(
        row.try_get_by_index::<String>(7).unwrap(),
        BOUND,
        "plugin_instance_id"
    );
}

/// **Both unique indexes survive the rebuild.**
///
/// `DROP TABLE` takes a table's indexes with it, so the rebuild has to
/// recreate them. Losing `idx_qa_products_tenant_name` would take the "at most
/// one product per (tenant, name)" rule with it, and nothing else in this gear
/// enforces that — a duplicate would be accepted and
/// `DomainError::ProductNameExists` would become unreachable.
///
/// Asserted two ways, because they fail differently: the index must be
/// *present* in `sqlite_master`, and it must actually *reject* a duplicate.
#[tokio::test]
async fn the_rebuild_recreates_both_unique_indexes() {
    let conn = db_migrated_up_to_this_one().await;

    MigrationTrait::up(&super::Migration, &SchemaManager::new(&conn))
        .await
        .expect("the migration must apply");

    for index in ["idx_qa_products_tenant_name", "idx_qa_products_tenant_key"] {
        let count: i64 = scalar(
            &conn,
            &format!(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = '{index}';"
            ),
        )
        .await;
        assert_eq!(count, 1, "{index} must exist after the rebuild");
    }

    insert_product(&conn, 0x66, "dup", Some(BOUND)).await;
    let duplicate = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "INSERT INTO qa_products \
                 (id, tenant_id, name, product_key, description, folder, created_at, updated_at, \
                 plugin_instance_id) \
                 VALUES ('{}', '{}', 'dup', 'OTHER', '', NULL, \
                 '2026-09-03 00:00:00+00:00', '2026-09-03 00:00:00+00:00', '{BOUND}');",
                Uuid::from_u128(0x77),
                Uuid::from_u128(7),
            ),
        ))
        .await;
    assert!(
        duplicate.is_err(),
        "the (tenant_id, name) index must still REJECT a duplicate -- an index \
         present in sqlite_master but built on the wrong columns would pass the \
         check above and fail here"
    );
}

/// A row in the referencing table survives the rebuild, on the **bare
/// connection** path.
///
/// `qa_test_repositories.product_id REFERENCES qa_products(id)`, and the
/// rebuild `DROP`s the referenced table. Foreign keys **are** enforced here
/// (`sqlite_foreign_keys_are_enforced_outside_a_transaction` measures it), so
/// the drop is only permitted because `rebuild_sqlite` turns enforcement off
/// first — which works on this path precisely because `up()` is called
/// directly, with no transaction around it.
///
/// **That is not the path production takes**, and an earlier version of this
/// doc said the opposite of the module doc 250 lines above it — that `SQLite`
/// does not enforce foreign keys by default and `toolkit-db` does not set the
/// pragma. Both halves were false. The runner's path is covered by
/// `the_rebuild_survives_a_referencing_row_inside_the_runners_transaction`;
/// this test is kept because both paths exist and the mechanism differs
/// between them (review finding IMPORTANT-2).
#[tokio::test]
async fn a_referencing_test_repository_survives_the_rebuild() {
    let conn = db_migrated_up_to_this_one().await;
    insert_product(&conn, 0x88, "owner", Some(BOUND)).await;
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        format!(
            "INSERT INTO qa_test_repositories \
             (id, tenant_id, product_id, name, url, default_branch, content_root, \
             credential_ref, last_synced_at, sync_error, created_at, updated_at) \
             VALUES ('{}', '{}', '{}', 'repo', 'https://example.com/r.git', 'main', '', \
             NULL, NULL, NULL, '2026-09-03 00:00:00+00:00', '2026-09-03 00:00:00+00:00');",
            Uuid::from_u128(0x99),
            Uuid::from_u128(7),
            Uuid::from_u128(0x88),
        ),
    ))
    .await
    .expect("failed to insert the referencing repository row");

    MigrationTrait::up(&super::Migration, &SchemaManager::new(&conn))
        .await
        .expect("the migration must apply with a referencing row present");

    let product_id: String = scalar(
        &conn,
        "SELECT product_id FROM qa_test_repositories WHERE name = 'repo';",
    )
    .await;
    assert_eq!(
        product_id,
        Uuid::from_u128(0x88).to_string(),
        "the reference must still name the product the rebuild moved"
    );
}

/// `down()` widens the column again, and needs no data change to do it.
#[tokio::test]
async fn down_makes_the_column_nullable_again() {
    let conn = db_migrated_up_to_this_one().await;
    insert_product(&conn, 0xaa, "kept", Some(BOUND)).await;
    let manager = SchemaManager::new(&conn);

    MigrationTrait::up(&super::Migration, &manager)
        .await
        .expect("up");
    MigrationTrait::down(&super::Migration, &manager)
        .await
        .expect("down must not need a data change");

    // The round trip kept the row...
    let count: i64 = scalar(&conn, "SELECT COUNT(*) FROM qa_products;").await;
    assert_eq!(count, 1, "down must not lose rows");

    // ...and the column accepts a NULL again, which is what "nullable" means
    // operationally.
    insert_product(&conn, 0xbb, "unbound-again", None).await;
}

/// **Review finding IMPORTANT-2: the rebuild, inside a transaction — which is
/// the shape the migration runner actually applies it in.**
///
/// Every other test in this module calls `MigrationTrait::up` directly on a
/// bare `DatabaseConnection`. Production does not.
/// `toolkit_db::migration_runner::run_gear_migrations` — reached by
/// `run_migrations_for_gear` in production **and** by
/// `run_migrations_for_testing` — does this, at
/// `libs/toolkit-db/src/migration_runner.rs:357-366`:
///
/// ```ignore
/// let txn = conn.begin().await?;
/// let manager = sea_orm_migration::SchemaManager::new(&txn);
/// migration.up(&manager).await?;
/// ```
///
/// Inside a transaction `PRAGMA foreign_keys = OFF` is a documented **no-op**,
/// so the mechanism this migration originally relied on did nothing there and
/// `DROP TABLE qa_products` was refused whenever a referencing row existed.
/// The suite never saw it, because migrations under the runner run against an
/// **empty** database where the foreign key has nothing to violate. This test
/// puts a row in first.
///
/// **It reproduces the runner's three lines rather than calling
/// `run_migrations_for_testing`**, because that entry point takes a
/// `toolkit_db::Db` whose `DatabaseConnection` is `pub(crate)`: there is no
/// way from here to seed a row into the database it opened. The lines above
/// are quoted from the runner and cited by file and line so a change to them
/// is greppable — that is the cost of this substitution, and it is the reason
/// the citation is exact.
#[tokio::test]
async fn the_rebuild_survives_a_referencing_row_inside_the_runners_transaction() {
    use sea_orm::TransactionTrait;

    let conn = db_migrated_up_to_this_one().await;
    insert_product(&conn, 0x88, "owner", Some(BOUND)).await;
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        format!(
            "INSERT INTO qa_test_repositories \
             (id, tenant_id, product_id, name, url, default_branch, content_root, \
             credential_ref, last_synced_at, sync_error, created_at, updated_at) \
             VALUES ('{}', '{}', '{}', 'repo', 'https://example.com/r.git', 'main', '', \
             NULL, NULL, NULL, '2026-09-03 00:00:00+00:00', '2026-09-03 00:00:00+00:00');",
            Uuid::from_u128(0x99),
            Uuid::from_u128(7),
            Uuid::from_u128(0x88),
        ),
    ))
    .await
    .expect("failed to seed the referencing test repository");

    // The runner's shape, verbatim.
    let txn = conn.begin().await.expect("failed to open a transaction");
    let manager = SchemaManager::new(&txn);
    MigrationTrait::up(&super::Migration, &manager)
        .await
        .expect(
            "the rebuild must survive a referencing row INSIDE A TRANSACTION -- a \
         `FOREIGN KEY constraint failed` here means the suspension mechanism does not \
         work on the path the migration runner uses",
        );
    txn.commit().await.expect(
        "the deferred foreign-key check runs at COMMIT, so a violation the rebuild left \
         behind surfaces here rather than at `up()`",
    );

    // The referencing row survived, and still points at its product.
    let owner: String = scalar(&conn, "SELECT product_id FROM qa_test_repositories;").await;
    assert_eq!(
        owner.to_lowercase(),
        Uuid::from_u128(0x88).to_string(),
        "the reference must survive the rebuild, not merely the drop"
    );

    // And the constraint this migration exists for is in force afterwards.
    let unbound = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "INSERT INTO qa_products \
                 (id, tenant_id, product_key, name, description, folder, \
                 plugin_instance_id, created_at, updated_at) \
                 VALUES ('{}', '{}', 'k', 'n', '', NULL, NULL, \
                 '2026-09-03 00:00:00+00:00', '2026-09-03 00:00:00+00:00');",
                Uuid::from_u128(0xAA),
                Uuid::from_u128(7),
            ),
        ))
        .await;
    assert!(
        unbound.is_err(),
        "after the migration a product must not be able to omit its plugin"
    );
}

/// Diagnostic, kept: foreign keys are enforced, and the pragma that disables
/// them takes effect **on a bare connection**.
///
/// Renamed at the pre-Task-19 review (finding IMPORTANT-2). It used to be
/// called `..._and_the_migration_is_not_transactional` and was cited as
/// proof that `defer_foreign_keys` could not help — but it calls `up()`
/// directly, so all it ever measured was the absence of a transaction *it did
/// not open*. The migration runner opens one;
/// `the_rebuild_survives_a_referencing_row_inside_the_runners_transaction` is
/// what measures that path.
#[tokio::test]
async fn sqlite_foreign_keys_are_enforced_outside_a_transaction() {
    let conn = db_migrated_up_to_this_one().await;

    let fk: i64 = scalar(&conn, "PRAGMA foreign_keys;").await;
    assert_eq!(
        fk, 1,
        "foreign keys ARE enforced on this connection -- the rebuild has to cope \
         with `qa_test_repositories` referencing `qa_products`. The module doc \
         claimed the opposite until this test was written."
    );

    // `PRAGMA foreign_keys` is a no-op inside a transaction, so this only
    // establishes the bare-connection half. The runner's half — where
    // `defer_foreign_keys` is the mechanism that works — is measured by the
    // test named in this function's doc.
    conn.execute_unprepared("PRAGMA foreign_keys = OFF;")
        .await
        .unwrap();
    let off: i64 = scalar(&conn, "PRAGMA foreign_keys;").await;
    conn.execute_unprepared("PRAGMA foreign_keys = ON;")
        .await
        .unwrap();
    assert_eq!(
        off, 0,
        "the pragma takes effect outside a transaction, so the rebuild can cycle it"
    );
}
