//! Behaviour the collapsed schema must have, which a schema dump cannot prove.
//!
//! Column names, types and index shapes are asserted by
//! `m20260812_000002_initial`'s own tests and, end to end, by diffing a
//! `pg_dump` of the applied chain. Neither can see what this file asserts: that
//! `plugin_instance_id` is genuinely refused when absent, that the binding
//! round-trips through the entity, and that foreign keys are actually enforced
//! on the connection the tests use.
//!
//! Carried over from the expand/contract pair the chain was collapsed into
//! `m20260812_000002_initial`. What was dropped with those files was the
//! mechanics of the step itself — the backfill to the VHP plugin, the `SQLite`
//! table rebuild, the `down` that made the column nullable again — all of which
//! describe a transition a fresh database never makes. Decision D6, *every
//! product names a plugin and there is no fallback path*, is the part that
//! outlives the transition, so it is the part asserted here.

// THE ONE LINT ALLOWANCE IN THIS FILE, and why it is here rather than in
// `clippy.toml`.
//
// `Select::one` is on `clippy.toml`'s `disallowed-methods` list because a
// production read that bypasses `SecureSelect` escapes tenant scoping. Nothing
// below is a production read, and there is no tenant to scope with: every test
// here opens a raw `SeaORM` connection against a freshly migrated, throwaway
// database and writes the rows it then reads back. The subject is the SCHEMA's
// behaviour -- that a foreign key really cascades, that an omitted column
// really takes its declared default, that a unique index is scoped to the
// tenant -- not the access path a production read travels. Routing these
// through `SecureSelect` means constructing an `AccessScope` that nothing in a
// migration module represents, and the assertion would then be about the
// wrapper rather than about the schema: the test would say it exercises the
// collapsed schema while exercising something else.
//
// IT IS SCOPED TO THIS FILE BECAUSE THAT IS THE NARROWEST FORM THAT EXISTS.
// Clippy's `disallowed-methods` is a single global list of paths with no
// per-file, per-module or per-crate form, so an "allowance" written in
// `clippy.toml` is not an allowance -- it deletes `Select::one` from the list
// for every crate in the workspace, including the repositories the rule was
// written for. An inner attribute stops at this file's last line, and moves
// with the file.
//
// The per-migration test modules in this directory carry the same allowance
// for the same reason; `m20260921_000003_repo_head_commit`'s test module
// carries the long form of the argument.
#![allow(
    clippy::disallowed_methods,
    reason = "a schema behaviour test drives a raw connection and has no tenant to scope \
              with -- see the comment above this attribute"
)]

use sea_orm::{
    ActiveModelTrait, ActiveValue, ConnectOptions, ConnectionTrait, Database, DatabaseConnection,
    EntityTrait, Statement, TryGetable,
};
use sea_orm_migration::MigratorTrait as _;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::infra::storage::entity::product;

/// An in-memory database with the whole (now single-migration) chain applied.
async fn migrated_db() -> DatabaseConnection {
    let mut opts = ConnectOptions::new("sqlite::memory:".to_owned());
    opts.max_connections(1).min_connections(1);
    let conn = Database::connect(opts)
        .await
        .expect("failed to connect to in-memory sqlite database");
    conn.execute_unprepared("PRAGMA foreign_keys = ON;")
        .await
        .expect("failed to enable sqlite foreign key enforcement");
    let manager = sea_orm_migration::SchemaManager::new(&conn);
    for migration in super::Migrator::migrations() {
        migration
            .up(&manager)
            .await
            .expect("failed to run a qa-catalog migration");
    }
    conn
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

/// **Decision D6 as a database constraint, not a service convention.**
///
/// `create_product` refuses a product that names no plugin, but a guard in one
/// service is not what makes the invariant hold — any other writer, and any
/// direct insert, would bypass it. `NOT NULL` is what makes a product without a
/// plugin unrepresentable.
#[tokio::test]
async fn a_product_cannot_omit_its_plugin() {
    let conn = migrated_db().await;

    let refused = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "INSERT INTO qa_products \
                 (id, tenant_id, name, product_key, description, folder, created_at, updated_at, \
                 plugin_instance_id) \
                 VALUES ('{}', '{}', 'unbound', 'UNBOUND', '', NULL, \
                 '2026-09-03 00:00:00+00:00', '2026-09-03 00:00:00+00:00', NULL);",
                Uuid::from_u128(2),
                Uuid::from_u128(7),
            ),
        ))
        .await;

    assert!(
        refused.is_err(),
        "a product naming no plugin must be refused by the column, not merely by \
         the service: there is no fallback plugin to resolve it to"
    );
}

/// The binding round-trips through the entity, which is what proves the column
/// the migration declares and the field the entity spells are the same column.
#[tokio::test]
async fn the_binding_round_trips_and_reaches_the_entity() {
    let conn = migrated_db().await;
    let id = Uuid::from_u128(2);
    let plugin = "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~acme._.other.v1";
    let now = OffsetDateTime::from_unix_timestamp(1_788_000_000).unwrap();

    product::ActiveModel {
        id: ActiveValue::Set(id),
        tenant_id: ActiveValue::Set(Uuid::from_u128(7)),
        name: ActiveValue::Set("other".to_owned()),
        product_key: ActiveValue::Set("OTHER".to_owned()),
        description: ActiveValue::Set(String::new()),
        folder: ActiveValue::Set(None),
        plugin_instance_id: ActiveValue::Set(plugin.to_owned()),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
    }
    .insert(&conn)
    .await
    .expect("the product row must insert");

    let stored = product::Entity::find_by_id(id)
        .one(&conn)
        .await
        .unwrap()
        .expect("the product row must read back");
    assert_eq!(stored.plugin_instance_id, plugin);
}

/// Foreign keys are enforced on the connection these tests use.
///
/// `qa_test_repositories` references `qa_products`, so a test that thought
/// foreign keys were off would be asserting nothing when it asserted a
/// rejection.
#[tokio::test]
async fn sqlite_foreign_keys_are_enforced_on_the_test_connection() {
    let conn = migrated_db().await;
    let fk: i64 = scalar(&conn, "PRAGMA foreign_keys;").await;
    assert_eq!(fk, 1, "foreign keys must be enforced on this connection");
}
