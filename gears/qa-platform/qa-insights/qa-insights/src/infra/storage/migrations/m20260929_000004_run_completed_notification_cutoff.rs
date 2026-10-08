//! "This deployment began notifying at instant T" — one row, written once.
//!
//! # Why the per-run claim seed was not enough
//!
//! `m20260929_000003_seed_run_completed_notification_claims` writes one claim
//! row per `(tenant_id, run_id)` in `qa_test_results`, which makes every run
//! this gear had *projected* read as already-notified. Its own header records
//! the gap it leaves: a run that finished with **zero** result rows is not in
//! `qa_test_results`, so it gets no claim — and `domain::service::reconcile`'s
//! header says such a run is never "already ingested" and is therefore
//! re-projected on every single pass, forever.
//!
//! That gap was measured on the dev stand on 2026-09-29 and it is not a corner:
//!
//! ```text
//! SELECT count(*) FROM (SELECT DISTINCT run_id FROM qa_test_results)  -> 1231
//! SELECT count(*) FROM qa_runs WHERE finished_at IS NOT NULL          -> 2326
//! ```
//!
//! **1095 finished runs carry no result rows on that stand alone** — 47% of its
//! history. None of them would have been claimed by the seed, all of them are
//! re-projected on every sweep, and each would have produced two notifications
//! (Slack and email) on the first pass after the upgrade. ~2190 messages about
//! history, from a feature whose entire point is one message per run, and the
//! ratio only grows as more runs fail before producing results.
//!
//! The root assumption is what failed: the claim seed treats `qa_test_results`
//! as a faithful record of what this gear has ingested, and it is a record of
//! what this gear has *rows* for. Those are different sets, and nothing in this
//! schema can be made to bridge them — "ingested, zero rows" is not
//! representable here (`domain::service::reconcile`'s header again, which
//! records that closing it needs a schema change no task owns).
//!
//! # So this closes the class instead of enumerating it
//!
//! A cutoff does not ask which runs were ingested. It asks when this
//! deployment started notifying, and declines everything older — zero-result
//! runs included, runs this gear never saw included, runs from a tenant that
//! has no row in any of our tables included. One value, one comparison, no
//! dependence on any table being a faithful census of anything.
//!
//! **The claim seed stays**, and the two are belt and braces rather than
//! alternatives. The cutoff covers everything that finished before the upgrade;
//! the claims cover the boundary the cutoff handles awkwardly — a run that
//! finished within clock-skew of the instant, or one swept from a replica whose
//! view of the ordering differs — and they also remain the mechanism for every
//! run *after* the cutoff, which is what makes send-once true in steady state.
//! Neither is a substitute for the other: delete the cutoff and 1095 runs mail;
//! delete the claims and every run mails on every sweep.
//!
//! # The row is **bound**, not spelled — and the first draft was not
//!
//! [`insert_row`] builds a `sea_query` `INSERT` and lets the driver encode
//! every value, rather than formatting the UUIDs and the instant into a SQL
//! literal. The first draft did format them, and it was wrong on the unit
//! tier in a way worth recording because it is invisible on the deployed one:
//!
//! **`sqlx` stores a `Uuid` on `SQLite` as a 16-byte BLOB**, not as the
//! 36-character hyphenated text a human writes (`sqlx-sqlite/src/types/uuid.rs`:
//! `Encode` pushes `SqliteArgumentValue::Blob(self.as_bytes())`, `Decode` calls
//! `Uuid::from_slice(value.blob())`). A row written with `'00000000-…'` reads
//! back as `ColumnDecode { index: "id", source: ParseByteLength { len: 36 } }`
//! — and, worse than failing, a *comparison* against such a row silently does
//! not match, because `SQLite` compares a BLOB and a TEXT as different types.
//! On Postgres, where `UUID` is a real type, the same literal casts and works,
//! so the defect ships green on the deployment that matters and fails only
//! where it is cheap to notice.
//!
//! The same applies to the instant, for the same reason in a different codec:
//! `CURRENT_TIMESTAMP` on `SQLite` yields `2026-09-29 12:00:00` — a space, no
//! offset — which `sqlx`'s `OffsetDateTime` decoder rejects, while Postgres
//! returns a `TIMESTAMPTZ` that reads back fine.
//! `crate::infra::leader::claim_row`'s entity carries that exact warning about
//! its own columns: "a `find`, a `one()` or a `RETURNING` added here would
//! compile, pass on Postgres, and fail on the unit tier". This row *is* read,
//! on every notification, so it cannot be written that way.
//!
//! Binding sidesteps both: the value goes through the same codec that will
//! later decode it, on whichever dialect, and this file contains no opinion
//! about either encoding.
//!
//! **`m20260929_000003` is not exposed to this** and the difference is worth
//! naming, since the two look alike: its `INSERT ... SELECT` copies
//! `tenant_id` and `run_id` **out of `qa_test_results`**, so it propagates
//! whatever the driver already stored rather than inventing a spelling. Only
//! its `id` is a literal, and nothing reads or matches on `id`.
//!
//! # Whose clock
//!
//! `OffsetDateTime::now_utc()` at [`MigrationTrait::up`], the migration
//! process's own. That is sound here because the migration runs once, in one
//! place — the chart runs it as a dedicated `job-db-migrate` Job, not per
//! replica — and because the value is compared against `finished_at` instants
//! that come from **qa-runs'** database, so this gear's database clock has no
//! authority over the comparison either. Skew costs at most a few seconds at
//! the boundary, which is precisely the boundary the claim seed also covers.
//!
//! # Idempotence and the conflict target
//!
//! The insert targets `idx_qa_notification_cutoff_tenant`, not the primary key.
//! A bare `DO NOTHING` on the PK would swallow every constraint violation on
//! the table — the initial migration's own note on `claim_conflict_target`
//! makes the same point — and, more to the point here, the *meaning* of the
//! conflict is "this deployment already has a cutoff", which is a statement
//! about `tenant_id` and not about a UUID nobody chose.
//!
//! **Re-running must not move the instant forward**, which is the whole
//! requirement: a second execution inserts nothing and leaves the first
//! instant standing. `the_cutoff_is_not_moved_by_re_running_the_migration`
//! is that property, asserted on the value rather than on the row count.

use sea_orm_migration::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The one row's primary key.
///
/// A fixed literal rather than a generated UUID, so the row is identifiable by
/// name in a psql session during an incident and so a re-run cannot produce a
/// second row even if the unique index were ever dropped. The `cf04` group is
/// this gear's, matching `domain::system_actor::QA_INSIGHTS_SYSTEM_ACTOR_UUID`;
/// the trailing bytes spell `cutoff`.
const CUTOFF_ROW_ID: Uuid = uuid::uuid!("00000000-0000-cf04-0001-6375746f6666");

/// The deployment-wide sentinel this row's `tenant_id` carries — see the
/// entity's header, and `infra::leader::claim_row`'s `CLAIM_TENANT`, which is
/// the same value for the same reason.
const DEPLOYMENT_WIDE_TENANT: Uuid = Uuid::nil();

/// The table and its columns, for the bound `INSERT`.
///
/// `DeriveIden` renders each variant in snake case, so `Table` is
/// `qa_notification_cutoff` and `CutoffAt` is `cutoff_at` — the same names the
/// DDL above declares and the entity binds. They are three independent
/// spellings of one schema, which is why
/// `the_cutoff_row_reads_back_as_a_real_instant` reads through the *entity*
/// after writing through *this*: it is the one assertion that makes all three
/// agree.
#[derive(DeriveIden)]
enum QaNotificationCutoff {
    Table,
    Id,
    TenantId,
    CutoffAt,
    CreatedAt,
    UpdatedAt,
}

/// Postgres. The table, its one-per-tenant unique index, and the row.
///
/// `TIMESTAMPTZ` for all three instants, matching every other timestamp column
/// in this schema.
const POSTGRES_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_notification_cutoff (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    cutoff_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_notification_cutoff_tenant ON qa_notification_cutoff(tenant_id);
";

/// `SQLite`. `TEXT` for the UUIDs and the instants, matching the initial
/// migration's own `SQLITE_UP`.
const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_notification_cutoff (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    cutoff_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_notification_cutoff_tenant ON qa_notification_cutoff(tenant_id);
";

/// The DDL for `backend`, or an error naming why there is none.
///
/// Same shape and the same refusal as every earlier migration's `up_ddl`: this
/// gear has no `MySQL` schema at all.
fn up_ddl(backend: sea_orm::DatabaseBackend) -> Result<&'static str, DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(POSTGRES_UP),
        sea_orm::DatabaseBackend::Sqlite => Ok(SQLITE_UP),
        sea_orm::DatabaseBackend::MySql => Err(DbErr::Custom(
            "qa-insights has no MySQL schema; see m20260818_000001_initial's header".to_owned(),
        )),
        other => Err(DbErr::Migration(format!(
            "unsupported database backend: {other:?}"
        ))),
    }
}

/// The one row, as a statement whose values the driver binds.
///
/// See this module's header for why not a literal. `values` rather than
/// `values_panic`: the arity is fixed two lines above the call, but a `panic!`
/// inside a migration is a `DbErr` this workspace's lints would rather see
/// spelled out than hidden behind a convenience.
///
/// `ON CONFLICT` renders per dialect from one builder — `SQLite` has supported
/// the Postgres syntax since 3.24 (2018) and the bundled `libsqlite3-sys` is
/// far newer. `INSERT OR IGNORE` would have worked on `SQLite` alone but could
/// not name the target, which is the thing worth naming here.
fn insert_row(at: OffsetDateTime) -> Result<InsertStatement, DbErr> {
    let mut stmt = Query::insert();
    stmt.into_table(QaNotificationCutoff::Table)
        .columns([
            QaNotificationCutoff::Id,
            QaNotificationCutoff::TenantId,
            QaNotificationCutoff::CutoffAt,
            QaNotificationCutoff::CreatedAt,
            QaNotificationCutoff::UpdatedAt,
        ])
        .values([
            CUTOFF_ROW_ID.into(),
            DEPLOYMENT_WIDE_TENANT.into(),
            at.into(),
            at.into(),
            at.into(),
        ])
        .map_err(|e| DbErr::Migration(format!("the cutoff row could not be built: {e}")))?
        .on_conflict(
            OnConflict::column(QaNotificationCutoff::TenantId)
                .do_nothing()
                .to_owned(),
        );
    Ok(stmt)
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        conn.execute_unprepared(up_ddl(backend)?).await?;
        // `ConnectionTrait::execute` takes the `sea_query` statement itself
        // (`StatementBuilder`), so the backend never has to be named twice:
        // the driver renders the placeholders and binds the values for
        // whichever dialect this connection is.
        conn.execute(&insert_row(OffsetDateTime::now_utc())?)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS qa_notification_cutoff;")
            .await?;
        Ok(())
    }
}

/// The same argument every migration module here makes: every name above is a
/// runtime string, so `cargo build` is no evidence at all about this file.
#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the initial migration's test module carries the same allow and the same reason: \
              every name in this file is a runtime string, so the only evidence about it comes \
              from raw statements against a real engine"
)]
mod tests {
    use sea_orm::EntityTrait;
    use sea_orm_migration::sea_orm::{Database, DatabaseConnection};
    use sea_orm_migration::{MigrationName as _, MigratorTrait as _};
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    use super::super::Migrator;
    use crate::infra::storage::entity::notification_cutoff;

    async fn migrated_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        Migrator::up(&conn, None).await.expect("migrations apply");
        conn
    }

    async fn rows(conn: &DatabaseConnection) -> Vec<notification_cutoff::Model> {
        notification_cutoff::Entity::find()
            .all(conn)
            .await
            .expect("the cutoff table is readable through its entity")
    }

    /// **The property the whole mechanism rests on**, and the one a raw
    /// `SELECT` would not have proved: the instant this migration wrote is
    /// readable back *through the entity*, as a real `OffsetDateTime`.
    ///
    /// `crate::infra::leader::claim_row`'s entity doc is why this is asserted
    /// rather than assumed — its own timestamp columns are written by the
    /// database's clock and are documented as undecodable on the unit tier. A
    /// cutoff written the same way would have compiled, passed on Postgres and
    /// failed here, and the failure would have surfaced as a notification path
    /// that errored on every run.
    #[tokio::test]
    async fn the_cutoff_row_reads_back_as_a_real_instant() {
        let before = OffsetDateTime::now_utc() - Duration::minutes(1);
        let conn = migrated_db().await;
        let after = OffsetDateTime::now_utc() + Duration::minutes(1);

        let rows = rows(&conn).await;
        assert_eq!(rows.len(), 1, "exactly one cutoff row: {rows:?}");
        let row = &rows[0];

        assert_eq!(row.tenant_id, Uuid::nil(), "the row is deployment-wide");
        assert_eq!(row.id, super::CUTOFF_ROW_ID);
        assert!(
            row.cutoff_at > before && row.cutoff_at < after,
            "the cutoff must be the instant the migration ran, got {} (window {before} .. {after})",
            row.cutoff_at,
        );
    }

    /// Re-running must **not** move the instant forward. A cutoff that moved
    /// on each application would re-open the window over everything that
    /// finished since the last one, which is the same defect as computing it
    /// from `now()` at boot.
    ///
    /// Asserted on the **value**, not the row count: an implementation that
    /// deleted and re-inserted would keep the count at one and still be wrong.
    #[tokio::test]
    async fn the_cutoff_is_not_moved_by_re_running_the_migration() {
        let conn = migrated_db().await;
        let first = rows(&conn).await[0].cutoff_at;

        // Enough for a second `now_utc()` to differ at the resolution the
        // format carries, so "unchanged" is a real observation rather than
        // two calls landing on one microsecond.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let manager = sea_orm_migration::SchemaManager::new(&conn);
        sea_orm_migration::MigrationTrait::up(&super::Migration, &manager)
            .await
            .expect("re-applying the migration must be safe");

        let rows = rows(&conn).await;
        assert_eq!(rows.len(), 1, "still exactly one row: {rows:?}");
        assert_eq!(
            rows[0].cutoff_at, first,
            "the cutoff moved on a second application; a restart or a re-run must not re-open \
             the history window"
        );
    }

    /// `down()` undoes `up()`.
    #[tokio::test]
    async fn the_down_migration_drops_the_table() {
        let conn = migrated_db().await;
        assert_eq!(rows(&conn).await.len(), 1);

        let manager = sea_orm_migration::SchemaManager::new(&conn);
        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .expect("down applies");

        assert!(
            notification_cutoff::Entity::find()
                .all(&conn)
                .await
                .is_err(),
            "the table must be gone, so a read of it fails"
        );
    }

    /// A migration missing from `Migrator::migrations()` runs on no deployment
    /// at all, and every test above would still pass — they reach it through
    /// `Migrator::up`, which is the one link that would break.
    #[test]
    fn the_migration_is_registered_in_the_chain() {
        let registered: Vec<String> = Migrator::migrations()
            .iter()
            .map(|m| m.name().to_owned())
            .collect();
        assert!(
            registered.contains(&super::Migration.name().to_owned()),
            "the cutoff migration is not in the applied chain; registered: {registered:?}"
        );
    }
}
