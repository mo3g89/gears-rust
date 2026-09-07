//! `qa_leader_claims` — the JIRA poller's mutual exclusion, one row per role.
//!
//! # Why this table exists, and why only one role uses it
//!
//! [`crate::infra::leader`]'s header argues that election in this gear is an
//! optimisation rather than a correctness requirement, and for
//! [`ROLE_RECONCILER`](crate::infra::leader::ROLE_RECONCILER) and
//! [`ROLE_COLLECT`](crate::infra::leader::ROLE_COLLECT) that is right: both
//! replay idempotent writes and two concurrent sweeps converge. It is **not**
//! right for [`ROLE_JIRA_POLLER`](crate::infra::leader::ROLE_JIRA_POLLER),
//! whose effect is `RunsLauncher::launch_test` — a new run, not a converging
//! write. Two replicas polling the same resolved bug launch it twice, and what
//! prevented that was `replicaCount: 1` in the chart. This table is where that
//! correctness property moves to, so a future scale-out cannot break it
//! silently. Review finding #5.
//!
//! # The row *is* the mechanism
//!
//! Exactly as `qa_run_notifications` is
//! (`m20260818_000001_initial`'s section on it, and
//! `infra::storage::entity::run_notification`'s header): nothing reads this
//! table to *report* anything. `idx_qa_leader_claims_role` — the unique index
//! over `(tenant_id, role)` — is what makes a role single-holder, and the
//! conditional UPDATE that only matches an expired row is what makes a crashed
//! holder recoverable. [`crate::infra::leader::claim_row`] is the only reader
//! or writer, and its header carries the CAS argument in full.
//!
//! # The columns, and the two that are absent
//!
//! `(id, tenant_id, role, holder, claimed_at, expires_at)`.
//!
//! * `tenant_id` is the **nil UUID** on every row this gear writes today, and
//!   that is a real value rather than a placeholder:
//!   `LeaderElector::run_role` takes a role and no tenant, because a ticker
//!   here holds the role for *every* tenant it then enumerates — the same
//!   "no tenant to take one from" that makes
//!   `system_actor::for_ticker_enumeration` nil-tenant
//!   (`domain::elevated`'s header). The column is in the unique index anyway,
//!   so a future per-tenant election — one that let two replicas split the
//!   tenants rather than alternate on the whole role — is a change to the
//!   elector and not to this schema.
//! * **No `created_at`/`updated_at`**, which every one of `000001`'s eleven
//!   tables carries. A claim row is overwritten in place by each takeover, so
//!   a `created_at` would date the row rather than the claim — it would say
//!   "since 2026-09-07" about a holder that took over a minute ago, which is
//!   the one thing an operator reading this table wants to know. `claimed_at`
//!   is that timestamp, and `expires_at` is the mechanism; a third and fourth
//!   would both be lies.
//!
//! # Both timestamps are written by the database, never by the holder
//!
//! There is no `DEFAULT` on either column and nothing here enforces that —
//! it is [`crate::infra::leader::claim_row`]'s discipline, stated here because
//! this is where the columns are declared. A replica whose clock runs slow
//! would otherwise judge a live claim expired *and* stamp that judgement into
//! its own `WHERE`, and steal a claim someone still holds: two leaders, which
//! is the exact failure this table exists to prevent. `coord`'s lease
//! (`gears/bss/libs/coord/src/lease/manager.rs`, its header) records the same
//! reasoning for the same reason.
//!
//! # Why it is not in `m20260818_000001_initial`
//!
//! Migrations are append-only and `000001` shipped;
//! [`super::m20260818_000002_offset_store`]'s header gives the argument in
//! full. This is the third file for the same reason it was the second.

use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// `PostgreSQL` — the dialect this gear deploys.
///
/// `role` is a **non-reserved** keyword in `PostgreSQL`, so it needs no
/// quoting; `m20260818_000002_offset_store`'s header is the reason that
/// sentence is not left to be assumed, and
/// [`tests::the_postgres_ddl_executes_and_matches_sqlite`] is what actually
/// proves it — the same measured-not-reasoned rule that file learned from
/// `"offset"`.
const POSTGRES_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_leader_claims (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    role VARCHAR(64) NOT NULL,
    holder UUID NOT NULL,
    claimed_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_leader_claims_role ON qa_leader_claims(tenant_id, role);
";

/// `SQLite` — the unit tier. `TEXT` throughout, matching `000001`'s own
/// `SQLITE_UP`, which spells every column of every table that way.
const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_leader_claims (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    role TEXT NOT NULL,
    holder TEXT NOT NULL,
    claimed_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_leader_claims_role ON qa_leader_claims(tenant_id, role);
";

/// The DDL for `backend`, or the refusal.
///
/// A free function for the reason `000001`'s and `000002`'s equivalents are
/// ones: the `MySQL` refusal is then reachable from a test without a `MySQL`
/// connection.
fn up_ddl(backend: sea_orm::DatabaseBackend) -> Result<&'static str, DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(POSTGRES_UP),
        sea_orm::DatabaseBackend::Sqlite => Ok(SQLITE_UP),
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
            .execute_unprepared("DROP TABLE IF EXISTS qa_leader_claims;")
            .await?;
        Ok(())
    }
}

/// Schema tests. `cargo build` proves nothing about a DDL string —
/// [`super::m20260818_000001_initial`]'s header is the long form of that
/// argument and it applies here unchanged.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
    use sea_orm_migration::MigratorTrait;

    use super::{SQLITE_UP, up_ddl};

    /// In-memory `SQLite` with the gear's whole `Migrator` applied — this
    /// migration included, since it is the last entry.
    async fn migrated_db() -> DatabaseConnection {
        let mut opts = ConnectOptions::new("sqlite::memory:");
        opts.max_connections(1).min_connections(1);
        let conn = Database::connect(opts).await.unwrap();
        let manager = sea_orm_migration::SchemaManager::new(&conn);
        for migration in crate::infra::storage::migrations::Migrator::migrations() {
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
        conn.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            row("11111111-1111-1111-1111-111111111111", "aaaaaaaa-0000-0000-0000-000000000001"),
        ))
        .await
        .expect("the first claim on a free role must be accepted");

        let second = conn
            .execute(Statement::from_string(
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
            ("11111111-1111-1111-1111-111111111111", "qa-insights-jira-poller"),
            ("22222222-2222-2222-2222-222222222222", "qa-insights-reconciler"),
        ] {
            conn.execute(Statement::from_string(
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

    /// `down()` drops the table, so the migration is reversible in the way the
    /// module list's header describes.
    #[tokio::test]
    async fn down_drops_the_table() {
        let conn = migrated_db().await;
        let manager = sea_orm_migration::SchemaManager::new(&conn);
        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();
        assert!(
            !manager.has_table("qa_leader_claims").await.unwrap(),
            "down() must remove the table it created"
        );
    }

    /// **`POSTGRES_UP` runs, and declares the same table and index `SQLITE_UP`
    /// does.**
    ///
    /// [`super::super::m20260818_000001_initial`]'s own Postgres test states
    /// the argument: the parity of two string constants proves they agree, not
    /// that either executes, and Postgres is the dialect this gear deploys. It
    /// is also the only thing that answers whether `role` is a keyword the
    /// server refuses unquoted — `m20260818_000002_offset_store` shipped a
    /// `SQLite`-clean DDL that Postgres could not parse for exactly that
    /// reason.
    ///
    /// Gated on `integration`, so a default `cargo test` needs no Docker.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn the_postgres_ddl_executes_and_matches_sqlite() {
        let harness = crate::infra::storage::test_db::pg_db().await;
        let pg = Database::connect(&harness.url).await.unwrap();

        let names = |conn: &DatabaseConnection, sql: String, backend| {
            let conn = conn.clone();
            async move {
                let mut out = conn
                    .query_all(Statement::from_string(backend, sql))
                    .await
                    .unwrap()
                    .iter()
                    .filter_map(|row| row.try_get::<String>("", "name").ok())
                    .collect::<Vec<_>>();
                out.sort();
                out
            }
        };

        let pg_indexes = names(
            &pg,
            "SELECT indexname AS name FROM pg_indexes WHERE schemaname = 'public' \
             AND tablename = 'qa_leader_claims' AND indexname NOT LIKE '%_pkey'"
                .to_owned(),
            sea_orm::DatabaseBackend::Postgres,
        )
        .await;

        let sqlite = migrated_db().await;
        let sqlite_indexes = names(
            &sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'index' \
             AND tbl_name = 'qa_leader_claims' AND name NOT LIKE 'sqlite_%'"
                .to_owned(),
            sea_orm::DatabaseBackend::Sqlite,
        )
        .await;

        assert_eq!(
            pg_indexes,
            vec!["idx_qa_leader_claims_role".to_owned()],
            "the unique index is the mechanism; without it on Postgres the elector \
             lets every replica win"
        );
        assert_eq!(
            pg_indexes, sqlite_indexes,
            "POSTGRES_UP and SQLITE_UP declared different indexes when actually run"
        );
    }

    /// The refusal is a real branch, and an unexecuted error path is the same
    /// defect class as an untested column name — `000002`'s wording, and its
    /// test, one migration on.
    #[test]
    fn the_mysql_arm_refuses_rather_than_executing() {
        let err = up_ddl(sea_orm::DatabaseBackend::MySql).expect_err("MySQL is refused");
        assert!(
            err.to_string().contains("no MySQL schema"),
            "the refusal should name the reason: {err}"
        );
    }

    /// The two dialects declare the same six columns.
    ///
    /// A textual guard rather than a catalogue one, so it runs on every
    /// `cargo test` and not only under `--features integration`: the
    /// column *names* are what the entity binds to, and a column added to one
    /// blob and forgotten in the other is the failure this catches without a
    /// container.
    #[test]
    fn both_dialects_declare_the_same_columns() {
        let columns = |ddl: &str| {
            ddl.lines()
                .filter_map(|line| {
                    let line = line.trim();
                    let name = line.split_whitespace().next()?;
                    name.chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_')
                        .then(|| name.to_owned())
                })
                .collect::<Vec<_>>()
        };
        let sqlite = columns(SQLITE_UP);
        assert_eq!(
            sqlite,
            vec!["id", "tenant_id", "role", "holder", "claimed_at", "expires_at"],
            "the extractor must actually be seeing the column list"
        );
        assert_eq!(sqlite, columns(super::POSTGRES_UP));
    }
}
