//! `evbk_consumer_offsets` — the broker consumer's durable progress.
//!
//! **Superseded.** The consumer this table served was deleted along with
//! qa-insights' event-broker dependency. The migration stays because it has
//! already run on deployed databases and the runner records it as applied;
//! deleting the file would make the code's list disagree with the schema. The
//! table is inert and no code reads or writes it.
//!
//! One table, owned by `event-broker-sdk`'s `LocalDbOffsetManager` and
//! declared here because it lived in **this gear's** database. That was the
//! whole point of the local offset store: the projection write and the offset
//! commit were the same transaction, which is only possible if the offset row is
//! in the same database as the rows it is a watermark for.
//!
//! # The SDK's own DDL constant is broken on Postgres, in two separate ways
//!
//! Both **measured on 2026-08-20 against `postgres:15-alpine`**, not reasoned,
//! and neither is visible on `SQLite` — which is the only dialect the SDK's own
//! `tests/consumer/db_tx.rs` runs the constant on, and therefore why nothing had
//! caught either.
//!
//! ## 1. `offset` is a reserved word, so the table cannot be created
//!
//! ```text
//! CREATE TABLE t1 (a UUID NOT NULL, offset BIGINT NOT NULL);
//!   ERROR:  syntax error at or near "offset"
//! CREATE TABLE t2 (a UUID NOT NULL, "offset" BIGINT NOT NULL);
//!   CREATE TABLE
//! ```
//!
//! `OFFSET` is *reserved* in `PostgreSQL`; `SQLite`'s parser still accepts it as
//! an identifier. `partition` is fine unquoted — it is *non-reserved* — which
//! the second statement above confirms by succeeding with only `offset` quoted.
//!
//! ## 2. `TIMESTAMP` is the wrong type, so the row cannot be read back
//!
//! Found by [`tests::the_offset_store_round_trips_on_postgres`] on its first
//! run, which is exactly what the integration tier is for. The constant declares
//! `updated_at TIMESTAMP`, and the SDK's own entity declares that column as
//! `DateTimeUtc` — `TIMESTAMPTZ`. The insert succeeds and the **read** fails:
//!
//! ```text
//! error occurred while decoding column "updated_at": mismatched types;
//! Rust type `Option<DateTime<Utc>>` (as SQL type `TIMESTAMPTZ`)
//! is not compatible with SQL type `TIMESTAMP`
//! ```
//!
//! This is the more dangerous of the two, because it fails *late and silently
//! in the direction that matters*: `commit_in_tx` writes happily, and
//! `load_position` then errors on every read. A consumer built on the constant
//! would appear to be committing progress and would in fact replay its whole
//! topic on every restart. `SQLite` has type affinity rather than types, so it
//! stores and returns the value either way.
//!
//! ## What this gear did about it
//!
//! Declared its own per-dialect DDL, in the shape
//! [`super::m20260818_000001_initial`] uses. It was **not** a copy that could
//! drift silently: [`tests::the_postgres_ddl_is_the_sdks_with_the_two_postgres_fixes`]
//! derived the Postgres blob from the same `SQLite` text below by applying
//! exactly those two substitutions, so a change to either would fail the
//! test rather than silently producing a table nothing could read. The
//! round-trip tests that drove the SDK's own offset manager against this
//! table are gone with the SDK dependency — see the module doc's
//! "Superseded" note above — and the drift guard is the coverage that
//! remains: both constants are frozen now that nothing writes new event
//! types, so there is nothing left to drift.
//!
//! # Why the table is not in `m20260818_000001_initial`
//!
//! Migrations are append-only, and `000001` shipped. Editing it would never
//! reach a deployment that already ran it. This is also the honest boundary:
//! `000001` is the gear's *own* eleven tables, and this one is a dependency's
//! table that happens to live here.

use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// `PostgreSQL`. The SDK's constant with its two Postgres defects fixed — the
/// quoted `"offset"` and the `TIMESTAMPTZ` — and nothing else changed. See this
/// module's header for both measurements.
const POSTGRES_UP: &str = r#"
CREATE TABLE IF NOT EXISTS evbk_consumer_offsets (
    consumer_group_id UUID NOT NULL,
    topic_id UUID NOT NULL,
    partition INTEGER NOT NULL,
    "offset" BIGINT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (consumer_group_id, topic_id, partition)
);
"#;

/// `SQLite`. Copied verbatim from `event-broker-sdk`'s
/// `LOCAL_DB_OFFSET_STORE_MIGRATION_SQL` at the point the dependency on that
/// crate was removed, rather than imported from it — see the module doc's
/// "Superseded" note. Frozen from here on: this migration has already run on
/// deployed databases, so the DDL it applies cannot change.
const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS evbk_consumer_offsets (
    consumer_group_id UUID NOT NULL,
    topic_id UUID NOT NULL,
    partition INTEGER NOT NULL,
    offset BIGINT NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    PRIMARY KEY (consumer_group_id, topic_id, partition)
);
";

/// The DDL for `backend`, or the refusal.
///
/// A free function for the reason `000001`'s equivalent is one: the `MySQL`
/// refusal is then reachable from a test without a `MySQL` connection.
fn up_ddl(backend: sea_orm::DatabaseBackend) -> Result<&'static str, DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(POSTGRES_UP),
        sea_orm::DatabaseBackend::Sqlite => Ok(SQLITE_UP),
        // Consistent with `000001`, which refuses `MySQL` for this gear
        // outright. Answering with the `SQLite` blob would produce a table
        // whose `offset` column MySQL also reserves, one migration after the
        // gear already said it has no MySQL schema.
        sea_orm::DatabaseBackend::MySql => Err(DbErr::Custom(
            "qa-insights has no MySQL schema; see m20260818_000001_initial's header, \
             \"MySQL key-width budget\"."
                .to_owned(),
        )),
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        manager
            .get_connection()
            .execute_unprepared(up_ddl(backend)?)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS evbk_consumer_offsets;")
            .await?;
        Ok(())
    }
}

/// Schema tests.
///
/// `cargo build` proves nothing about a DDL string. Before the event-broker
/// dependency was removed, these established that `event-broker-sdk`'s
/// `LocalDbOffsetManager` could write a row into this table and read the same
/// value back on both dialects this gear ships — a stronger check than an
/// inventory assertion, because it failed on a wrong column name, a wrong type
/// or a wrong primary key alike. That coverage left with the SDK: the table is
/// inert now, so what remains is the drift guard between this file's own two
/// constants, and the untaken `MySQL` branch.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use super::{POSTGRES_UP, SQLITE_UP, up_ddl};

    /// The drift guard between this file's Postgres and `SQLite` DDL.
    ///
    /// The Postgres blob is *derived* from [`SQLITE_UP`] rather than compared
    /// to a second literal, so an edit to one that forgets the other fails
    /// this test instead of silently producing two schemas that disagree.
    /// Two substitutions are permitted and they are the two measured defects
    /// this module's header records against the original `event-broker-sdk`
    /// constant; a third divergence cannot be introduced without editing this
    /// test, which is the point. Both sides are frozen now — this migration
    /// has already run on deployed databases — so there is nothing left to
    /// drift *into*, only to notice if it happens.
    #[test]
    fn the_postgres_ddl_is_the_sqlite_ddl_with_the_two_postgres_fixes() {
        let quoted = SQLITE_UP.replace("    offset ", "    \"offset\" ");
        assert_ne!(
            quoted, SQLITE_UP,
            "the reserved-word substitution must have applied"
        );

        let expected = quoted.replace("updated_at TIMESTAMP ", "updated_at TIMESTAMPTZ ");
        assert_ne!(
            expected, quoted,
            "the timestamp substitution must have applied"
        );

        assert_eq!(POSTGRES_UP, expected);
    }

    /// The refusal is a real branch, and an unexecuted error path is the same
    /// defect class as an untested column name.
    #[test]
    fn the_mysql_arm_refuses_rather_than_executing() {
        let err = up_ddl(sea_orm::DatabaseBackend::MySql).expect_err("MySQL is refused");
        assert!(
            err.to_string().contains("no MySQL schema"),
            "the refusal should name the reason: {err}"
        );
    }
}
