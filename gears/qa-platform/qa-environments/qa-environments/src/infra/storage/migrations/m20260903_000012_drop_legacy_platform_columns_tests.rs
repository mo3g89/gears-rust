//! Tests for the contract migration.
//!
//! `cargo build` proves nothing about a migration — its SQL is a runtime
//! string — so every claim is made by running it against a real (if in-memory)
//! database and then asking the database.
//!
//! **Each of the five buckets and each of the five halts gets a test.** The
//! partition is what makes this migration safe to run once, and a partition
//! nothing exercises is a partition nobody has checked.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, TransactionTrait,
    TryGetable,
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

/// Every migration of this gear **up to but excluding this one**, so rows can
/// be planted while the legacy columns still exist.
///
/// Keyed on this migration's own name rather than on a position, for the
/// reason `m20260903_000011`'s equivalent gives.
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

async fn scalar<T: TryGetable>(conn: &DatabaseConnection, sql: &str) -> T {
    conn.query_one_raw(Statement::from_string(
        conn.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .unwrap()
    .expect("the query returned no row")
    .try_get_by_index::<T>(0)
    .expect("the row's first column is not the expected type")
}

/// Plant an environment with an explicit `credentials` blob and legacy
/// reference. Raw SQL, because the entity has no legacy field as of this task
/// and so cannot express the pre-migration row.
async fn plant(
    conn: &DatabaseConnection,
    id: u128,
    product: Option<u128>,
    credentials: &str,
    legacy_ref: &str,
) {
    // Blob literals, not text: `sea-orm` binds `Uuid` as a 16-byte blob on
    // `SQLite`, so a text id is unreadable through the entity and matches no
    // foreign key. See `legacy_row::uuid_lit`.
    let product = product.map_or_else(
        || "NULL".to_owned(),
        |p| super::super::legacy_row::uuid_lit(Uuid::from_u128(p)),
    );
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        format!(
            "INSERT INTO qa_environments \
             (id, tenant_id, name, product_id, description, kubeconfig_credstore_ref, \
              available, is_default, credentials, observed_attrs, config, health_state, \
              created_at, updated_at) \
             VALUES ({}, {}, 'env-{id}', {product}, NULL, '{legacy_ref}', \
                     1, 0, '{credentials}', '{{}}', '{{}}', 'unknown', \
                     '2026-09-05 00:00:00+00:00', '2026-09-05 00:00:00+00:00');",
            super::super::legacy_row::uuid_lit(Uuid::from_u128(id)),
            super::super::legacy_row::uuid_lit(Uuid::from_u128(7)),
        ),
    ))
    .await
    .expect("failed to plant an environment");
}

/// Run this migration the way the runner does — inside a transaction.
///
/// `toolkit_db::migration_runner::run_gear_migrations` opens one per migration
/// (`libs/toolkit-db/src/migration_runner.rs:357-366`) in production and in
/// tests alike, and `m20260903_000004`'s own tests learned the hard way that
/// measuring `up()` on a bare connection measures a path nothing uses.
async fn run_it(conn: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    let txn = conn.begin().await.expect("failed to open a transaction");
    let result = MigrationTrait::up(&super::Migration, &SchemaManager::new(&txn)).await;
    match result {
        Ok(()) => txn.commit().await,
        Err(error) => {
            txn.rollback().await.expect("rollback failed");
            Err(error)
        }
    }
}

fn credentials_of(id: u128) -> String {
    format!(
        "SELECT credentials FROM qa_environments WHERE id = {};",
        super::super::legacy_row::uuid_lit(Uuid::from_u128(id))
    )
}

const REF_A: &str = "credstore://team-a/kubeconfig-first";
// Deliberately NOT a string containing `REF_A`: the first version of this
// fixture used `…/kubeconfig` and `…/kubeconfig-rotated`, so `contains(REF_A)`
// was true of REF_B and bucket 2's "the stale one is gone" assertion could not
// fail.
const REF_B: &str = "credstore://team-a/kubeconfig-second";

// ---------------------------------------------------------------- buckets

/// **Bucket 1** — the *empty* state: a credential the write path could not key.
#[tokio::test]
async fn bucket_1_an_empty_credentials_is_rebuilt_from_the_legacy_reference() {
    let conn = db_migrated_up_to_this_one().await;
    plant(&conn, 1, Some(0x10), "[]", REF_A).await;

    run_it(&conn).await.expect("the migration must run");

    let stored: String = scalar(&conn, &credentials_of(1)).await;
    assert!(
        stored.contains("\"key\":\"kubeconfig\"") && stored.contains(REF_A),
        "the empty state must be rebuilt from the legacy column: {stored}"
    );
}

/// **Bucket 2** — the *stale* state, and the case ruling F-1 exists for.
///
/// `m20260903_000011`'s backfill was a one-time snapshot; a pre-Task-18b
/// rotation then wrote a new legacy reference and **deleted** the superseded
/// secret without touching `credentials`. Overwriting is what repairs it.
#[tokio::test]
async fn bucket_2_a_stale_credentials_is_overwritten_from_the_legacy_reference() {
    let conn = db_migrated_up_to_this_one().await;
    plant(
        &conn,
        2,
        Some(0x10),
        &format!(r#"[{{"key":"kubeconfig","credstore_ref":"{REF_A}"}}]"#),
        REF_B,
    )
    .await;

    run_it(&conn).await.expect("the migration must run");

    let stored: String = scalar(&conn, &credentials_of(2)).await;
    assert!(
        stored.contains(REF_B) && !stored.contains(REF_A),
        "the stale entry must be replaced by the legacy column's current value: {stored}"
    );
}

/// **Bucket 3** — the two already agree, so nothing is written.
#[tokio::test]
async fn bucket_3_a_row_that_already_agrees_is_left_alone() {
    let conn = db_migrated_up_to_this_one().await;
    let blob = format!(r#"[{{"key":"kubeconfig","credstore_ref":"{REF_A}"}}]"#);
    plant(&conn, 3, Some(0x10), &blob, REF_A).await;

    run_it(&conn).await.expect("the migration must run");

    let stored: String = scalar(&conn, &credentials_of(3)).await;
    assert!(stored.contains(REF_A), "unchanged: {stored}");
}

/// **Bucket 4** — empty and empty. Legitimate for a plugin that declares no
/// required secret, and there is nothing to derive.
///
/// Ruling F-14: manufacturing `[{"key":"kubeconfig","credstore_ref":""}]` here
/// would turn "this row has no credential" into "this row has a credential
/// that resolves to nothing", which reads as a different fact to every reader.
#[tokio::test]
async fn bucket_4_no_credential_anywhere_stays_empty_rather_than_gaining_one() {
    let conn = db_migrated_up_to_this_one().await;
    plant(&conn, 4, Some(0x10), "[]", "").await;

    run_it(&conn).await.expect("the migration must run");

    let stored: String = scalar(&conn, &credentials_of(4)).await;
    assert_eq!(
        stored, "[]",
        "an empty row must not gain an entry that points at nothing"
    );
}

/// **Bucket 5** — one credential under a non-`kubeconfig` key that agrees with
/// the legacy column. Complete and internally consistent; after the drop the
/// plugin looks up its own key and finds it.
#[tokio::test]
async fn bucket_5_a_consistent_non_kubeconfig_key_is_left_alone() {
    let conn = db_migrated_up_to_this_one().await;
    let blob = format!(r#"[{{"key":"api_token","credstore_ref":"{REF_A}"}}]"#);
    plant(&conn, 5, Some(0x10), &blob, REF_A).await;

    run_it(&conn).await.expect("the migration must run");

    let stored: String = scalar(&conn, &credentials_of(5)).await;
    assert!(
        stored.contains("api_token"),
        "the key must survive -- rewriting it to 'kubeconfig' unbinds the credential \
         with no column left to re-derive from: {stored}"
    );
}

// ------------------------------------------------------------------ halts

async fn expect_halt(conn: &DatabaseConnection, expect: &str) {
    let error = run_it(conn).await.expect_err("the migration must refuse");
    let text = error.to_string();
    assert!(
        text.contains(expect),
        "the refusal must name {expect:?} so an operator knows which rows: {text}"
    );
}

/// **H1** — more than one credential; the legacy column can name only one.
#[tokio::test]
async fn h1_a_row_with_two_credentials_halts_the_migration() {
    let conn = db_migrated_up_to_this_one().await;
    plant(
        &conn,
        0x11,
        Some(0x10),
        &format!(
            r#"[{{"key":"kubeconfig","credstore_ref":"{REF_A}"}},{{"key":"api_token","credstore_ref":"{REF_B}"}}]"#
        ),
        REF_A,
    )
    .await;
    expect_halt(&conn, &Uuid::from_u128(0x11).to_string()).await;
}

/// **H2** — credentials beside an empty legacy reference: two or more required
/// secrets, so `sole_required_secret_key()` returned `None`.
#[tokio::test]
async fn h2_credentials_with_no_legacy_reference_halt_the_migration() {
    let conn = db_migrated_up_to_this_one().await;
    plant(
        &conn,
        0x12,
        Some(0x10),
        &format!(r#"[{{"key":"kubeconfig","credstore_ref":"{REF_A}"}}]"#),
        "",
    )
    .await;
    expect_halt(&conn, &Uuid::from_u128(0x12).to_string()).await;
}

/// **H3** — a non-`kubeconfig` key whose reference *disagrees* with the legacy
/// column. Which one is current cannot be decided without the plugin.
#[tokio::test]
async fn h3_a_disagreeing_non_kubeconfig_key_halts_the_migration() {
    let conn = db_migrated_up_to_this_one().await;
    plant(
        &conn,
        0x13,
        Some(0x10),
        &format!(r#"[{{"key":"api_token","credstore_ref":"{REF_A}"}}]"#),
        REF_B,
    )
    .await;
    expect_halt(&conn, &Uuid::from_u128(0x13).to_string()).await;
}

/// **H6** — a single entry with no `key`, whose reference *agrees* with the
/// legacy column.
///
/// Without this halt the row lands in bucket 5 and is skipped as "complete
/// under its own key" — which it has none of (whole-branch review, m-3). The
/// agreeing reference is the point: a disagreeing one already halted under H3.
#[tokio::test]
async fn h6_a_keyless_single_credential_halts_the_migration() {
    let conn = db_migrated_up_to_this_one().await;
    plant(
        &conn,
        0x17,
        Some(0x10),
        &format!(r#"[{{"credstore_ref":"{REF_A}"}}]"#),
        REF_A,
    )
    .await;
    expect_halt(&conn, &Uuid::from_u128(0x17).to_string()).await;
}

/// **H4** — `credentials` holds valid JSON that is not an array.
///
/// Its own query, and first: `json_array_length` raises on a scalar in
/// Postgres and no `OR` guarantees evaluation order, so folded in beside the
/// length predicates this row could abort the whole pre-check with a type
/// error instead of being named. Finding I-8 asked Task 19 to *decide* the
/// corrupt-blob behaviour rather than inherit `mapper.rs`' silent degrade;
/// halting and naming the row is that decision.
#[tokio::test]
async fn h4_a_credentials_blob_that_is_not_an_array_halts_the_migration() {
    let conn = db_migrated_up_to_this_one().await;
    plant(&conn, 0x14, Some(0x10), r#"{"key":"kubeconfig"}"#, REF_A).await;
    expect_halt(&conn, &Uuid::from_u128(0x14).to_string()).await;
}

/// **The other corrupt shape: not JSON at all.**
///
/// `SQLite` stores `credentials` as `TEXT`, so a blob like this really can be
/// in the column, and `json_type` **raises** on it — which would have H4 die
/// with a type error instead of naming the row, the exact failure it exists to
/// replace. The `json_valid` guard on that arm is what makes this case halt
/// like the other one (whole-branch review, m-1 and m-2).
#[tokio::test]
async fn h4_a_credentials_blob_that_is_not_json_at_all_halts_the_migration() {
    let conn = db_migrated_up_to_this_one().await;
    plant(&conn, 0x19, Some(0x10), "not json at all", REF_A).await;
    expect_halt(&conn, &Uuid::from_u128(0x19).to_string()).await;
}

/// **H5** — bucket-1 rows spanning more than one product.
///
/// Bucket 1 writes the literal `'kubeconfig'` onto a row that never carried a
/// key, so it is the one bucket whose correctness rests on "one product, one
/// plugin, one key-giver". With two products in play that premise is unproven
/// and the migration refuses rather than guessing.
#[tokio::test]
async fn h5_unkeyed_rows_spanning_two_products_halt_the_migration() {
    let conn = db_migrated_up_to_this_one().await;
    plant(&conn, 0x15, Some(0x10), "[]", REF_A).await;
    plant(&conn, 0x16, Some(0x20), "[]", REF_B).await;
    expect_halt(&conn, "2 products").await;
}

/// H5 must not fire on productless rows: `COUNT(DISTINCT product_id)` ignores
/// `NULL`s deliberately, because a row with no product has no plugin whose key
/// could disagree with the literal.
#[tokio::test]
async fn h5_does_not_fire_on_a_productless_unkeyed_row() {
    let conn = db_migrated_up_to_this_one().await;
    plant(&conn, 0x17, Some(0x10), "[]", REF_A).await;
    plant(&conn, 0x18, None, "[]", REF_B).await;

    run_it(&conn)
        .await
        .expect("one product plus NULLs is one product");
}

// ------------------------------------------------------------- the columns

/// The eight columns are actually gone, asked of the database rather than
/// assumed from the SQL.
#[tokio::test]
async fn the_eight_legacy_columns_are_gone_afterwards() {
    let conn = db_migrated_up_to_this_one().await;
    plant(&conn, 0x20, Some(0x10), "[]", REF_A).await;
    run_it(&conn).await.expect("the migration must run");

    for column in [
        "kubeconfig_credstore_ref",
        "vhp_base_url",
        "observed_namespace",
        "cluster_status",
        "cluster_status_message",
        "cluster_nodes",
        "cluster_namespace_count",
        "cluster_checked_at",
    ] {
        let probe = conn
            .execute_raw(Statement::from_string(
                conn.get_database_backend(),
                format!("SELECT {column} FROM qa_environments;"),
            ))
            .await;
        assert!(probe.is_err(), "`{column}` must no longer exist");
    }

    // And the columns that replaced them still do.
    for column in [
        "credentials",
        "observed_attrs",
        "observed_base_url",
        "health_state",
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("SELECT {column} FROM qa_environments;"),
        ))
        .await
        .unwrap_or_else(|error| panic!("`{column}` must survive the drop: {error}"));
    }
}

/// `down()` is best-effort: it re-adds the columns and repopulates the legacy
/// reference from `credentials`. It cannot recover what the plugin path never
/// wrote, and the module doc says so rather than implying a clean reverse.
#[tokio::test]
async fn down_restores_the_legacy_reference_from_credentials() {
    let conn = db_migrated_up_to_this_one().await;
    plant(&conn, 0x21, Some(0x10), "[]", REF_A).await;
    run_it(&conn).await.expect("the migration must run");

    let txn = conn.begin().await.unwrap();
    MigrationTrait::down(&super::Migration, &SchemaManager::new(&txn))
        .await
        .expect("down must run");
    txn.commit().await.unwrap();

    let restored: String = scalar(
        &conn,
        &format!(
            "SELECT kubeconfig_credstore_ref FROM qa_environments WHERE id = {};",
            super::super::legacy_row::uuid_lit(Uuid::from_u128(0x21))
        ),
    )
    .await;
    assert_eq!(
        restored, REF_A,
        "the reference must come back from `credentials`"
    );
}
