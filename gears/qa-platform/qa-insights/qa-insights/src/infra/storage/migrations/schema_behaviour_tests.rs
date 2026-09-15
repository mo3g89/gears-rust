//! Behaviour the collapsed schema must have, which a schema dump cannot prove.
//!
//! Column names, types and index shapes are asserted by
//! `m20260818_000001_initial`'s own inventory tests and, end to end, by diffing
//! a `pg_dump` of the applied chain. Neither can see what this file asserts:
//! that `idx_qa_leader_claims_role` really admits one holder per role and
//! really scopes that to the tenant.
//!
//! Carried over from `m20260907_000003_leader_claims`, which the chain was
//! collapsed into `m20260818_000001_initial`. What was dropped with that file
//! was the delta mechanics — its `down`, its MySQL refusal, its dialect column
//! parity — which describe a step that no longer exists.

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait as _;

/// An in-memory database with the whole (now single-migration) chain applied.
async fn migrated_db() -> DatabaseConnection {
    let mut opts = ConnectOptions::new("sqlite::memory:");
    opts.max_connections(1).min_connections(1);
    let conn = Database::connect(opts).await.unwrap();
    let manager = sea_orm_migration::SchemaManager::new(&conn);
    for migration in super::Migrator::migrations() {
        migration.up(&manager).await.unwrap();
    }
    conn
}


/// The `SQLite` DDL runs, and the row it accepts is the shape the elector
/// writes: one row per `(tenant_id, role)` and no second holder.
///
/// The insert is raw SQL rather than the entity, so this test fails on a
/// wrong *column name* — the entity would only fail on a wrong mapping,
/// and it is the DDL under test here.
#[tokio::test]
async fn the_sqlite_ddl_creates_a_single_holder_slot() {
    let conn = migrated_db().await;
    let row = |id: &str, holder: &str| {
        format!(
            "INSERT INTO qa_leader_claims \
             (id, tenant_id, role, holder, claimed_at, expires_at) VALUES \
             ('{id}', '00000000-0000-0000-0000-000000000000', 'qa-insights-jira-poller', \
             '{holder}', '2026-09-07 00:00:00', '2026-09-07 00:01:00')"
        )
    };
    conn.execute_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        row(
            "11111111-1111-1111-1111-111111111111",
            "aaaaaaaa-0000-0000-0000-000000000001",
        ),
    ))
    .await
    .expect("the first claim on a free role must be accepted");

    let second = conn
        .execute_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            row(
                "22222222-2222-2222-2222-222222222222",
                "aaaaaaaa-0000-0000-0000-000000000002",
            ),
        ))
        .await;
    assert!(
        second.is_err(),
        "idx_qa_leader_claims_role must reject a second holder of the same role; \
         without it the whole elector degrades to NoopLeaderElector silently"
    );
}

/// A second *role* is a second slot — the index keys on the role, not on
/// the table.
///
/// Not vacuous next to the test above: an index over `(tenant_id)` alone
/// would pass that one and fail this, and it is the mistake that would
/// make one ticker's leadership gate the other two.
#[tokio::test]
async fn two_roles_hold_two_independent_slots() {
    let conn = migrated_db().await;
    for (id, role) in [
        (
            "11111111-1111-1111-1111-111111111111",
            "qa-insights-jira-poller",
        ),
        (
            "22222222-2222-2222-2222-222222222222",
            "qa-insights-reconciler",
        ),
    ] {
        conn.execute_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO qa_leader_claims \
                 (id, tenant_id, role, holder, claimed_at, expires_at) VALUES \
                 ('{id}', '00000000-0000-0000-0000-000000000000', '{role}', \
                 '{id}', '2026-09-07 00:00:00', '2026-09-07 00:01:00')"
            ),
        ))
        .await
        .expect("each role must get its own slot");
    }
}
