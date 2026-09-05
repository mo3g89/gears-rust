//! Raw-SQL row insertion for migration tests.
//!
//! **Why not the entity.** A migration test plants a row as it existed at that
//! migration's point in history. `m20260903_000012` drops eight columns and the
//! entity lost the matching fields in the same commit, so `environment::Model`
//! can no longer express a pre-drop row at all — and `kubeconfig_credstore_ref`
//! is `NOT NULL` with no default, so an entity insert that simply omits it
//! fails.
//!
//! This is the same lesson `qa-catalog`'s `m20260903_000004` tests recorded
//! ("raw SQL rather than the entity, because a migration must not depend on the
//! shape of today's model"), reached here from the other direction: the model
//! moved and the historical tests had to stop depending on it.
//!
//! Callers name **only** the columns that exist at their own point in the
//! migration sequence, which is why this takes pairs rather than a struct: a
//! struct would have to know every schema version.

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

/// Insert one row, naming exactly the columns given.
///
/// Values are SQL literals — the caller quotes them — because these fixtures
/// mix strings, numbers, `NULL` and JSON blobs, and a typed wrapper would be
/// more machinery than seven test modules need.
pub(super) async fn insert(conn: &DatabaseConnection, table: &str, columns: &[(&str, String)]) {
    let names = columns
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    let values = columns
        .iter()
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        format!("INSERT INTO {table} ({names}) VALUES ({values});"),
    ))
    .await
    .unwrap_or_else(|error| panic!("failed to plant a row in {table}: {error}"));
}

/// Plant an environment row with the columns every pre-`000012` schema
/// requires, plus whatever the caller's own migration is about.
///
/// The mandatory set is what is `NOT NULL` with no default at that point:
/// `id`, `tenant_id`, `name`, `kubeconfig_credstore_ref`, `created_at` and
/// `updated_at`. Callers add the column their test is the subject of.
pub(super) async fn plant_environment(
    conn: &DatabaseConnection,
    table: &str,
    id: uuid::Uuid,
    tenant: uuid::Uuid,
    name: &str,
    extra: &[(&str, String)],
) {
    const STAMP: &str = "2026-08-13 12:00:00+00:00";
    let mut columns: Vec<(&str, String)> = vec![
        ("id", uuid_lit(id)),
        ("tenant_id", uuid_lit(tenant)),
        ("name", text(name)),
        // Required since Task 20b's `m20260903_000013`. Planted for every
        // fixture, including those from migrations that predate the column
        // being NOT NULL: the row still has to satisfy today's schema by the
        // time the test reads it back through the entity.
        ("product_id", uuid_lit(uuid::Uuid::from_u128(0x9001))),
        ("kubeconfig_credstore_ref", text("credstore://ref")),
        ("created_at", text(STAMP)),
        ("updated_at", text(STAMP)),
    ];
    columns.extend(extra.iter().map(|(n, v)| (*n, v.clone())));
    insert(conn, table, &columns).await;
}

/// A `Uuid` as `SQLite` stores it: a **16-byte blob**, not text.
///
/// This is not cosmetic. `sea-orm` binds `Uuid` as a blob on `SQLite`, so a
/// row planted with `'0000...-0001'` reads back through the entity as
/// `ColumnDecode { ParseByteLength { len: 36 } }`, and a foreign key from a
/// row planted the other way never matches. Every UUID written or matched by
/// these fixtures goes through here.
pub(super) fn uuid_lit(value: uuid::Uuid) -> String {
    format!("X'{}'", value.simple())
}

/// A SQL string literal, single quotes doubled.
pub(super) fn text(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// `NULL`, or a quoted literal.
pub(super) fn maybe(value: Option<&str>) -> String {
    value.map_or_else(|| "NULL".to_owned(), text)
}
