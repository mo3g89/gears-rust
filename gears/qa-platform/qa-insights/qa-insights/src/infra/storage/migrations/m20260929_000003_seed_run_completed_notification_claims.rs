//! One send-once claim per already-projected run, per run-completed channel.
//!
//! # What this is for, and why it is data rather than a flag
//!
//! Until this migration's companion commit, `NotifyService::notify_run_completed`
//! had no production caller: the notification subsystem shipped whole —
//! routing, rendering, the audit log, the claim — and nothing drove it.
//! `ReconcileService::reproject` is that caller now, which means the first
//! sweep and every operator rebuild after the upgrade reach it for runs this
//! gear ingested **months** ago. A rebuild replays every run in its window
//! unconditionally (`domain::service::reconcile`'s `replay`), so without this
//! migration an operator's first `POST /qa/v1/insights/rebuild` after the
//! upgrade would mail one message per historical run, from a feature whose
//! entire point is one message per run.
//!
//! The fix is not a suppression window or a cut-off timestamp. `qa_run_notifications`
//! **is** the record of what has been notified — that is the table's whole
//! contract, and `claim_notification` reads a unique violation on
//! `idx_qa_run_notifications_claim` as "already sent" rather than as an error
//! (`m20260818_000001_initial`'s header, obligation #4). So the honest
//! statement of "this deployment has already dealt with these runs" is a claim
//! row per run per channel, which is exactly what this inserts. Afterwards
//! nothing special is true of those runs: they read as already-notified through
//! the same index every future run will.
//!
//! # The two kinds, and the one event
//!
//! `domain::notify::routing` spells the run-completed claim slots
//! `(kind, event)` = (`run_completed_slack`, `run_completed`) and
//! (`run_completed_email`, `run_completed`) — `NotificationKind::as_str` and
//! `Event::dedupe_token`. Both are seeded, because
//! `NotifyService::notify_run_completed` claims each channel separately and a
//! tenant that enables the second channel later must not get a backfill of
//! history down it.
//!
//! The strings are repeated here as SQL literals rather than imported from
//! `domain::notify::routing`, and that is a real duplication with a real
//! reason: a migration is a statement about a database at a moment in history
//! and must keep producing the same rows however the code above it is later
//! renamed. `the_seeded_kinds_are_the_ones_routing_claims` in this file's test
//! module is what keeps the two in step *today* — it asserts the literals
//! against `NotificationKind::as_str`, so a rename that silently diverged
//! fails a test instead of shipping a migration that seeds slots nothing
//! claims.
//!
//! # The source set: `qa_test_results`, and what it does not contain
//!
//! `SELECT DISTINCT tenant_id, run_id FROM qa_test_results` is the set of runs
//! this gear has a projection for, and it is the same table
//! `ResultsRepository::ingested_run_ids_between` answers the sweep's diff from
//! — so "already ingested" means the same thing here as it does there.
//!
//! **A run that finished before the upgrade and produced zero result rows is
//! not in it.** `domain::service::reconcile`'s header records that such a run
//! has no rows, is therefore never "already ingested", and is re-projected on
//! every sweep forever. This migration cannot claim them, and the gap is not
//! small: measured on the dev stand on 2026-09-29, `qa_test_results` held 1231
//! distinct run ids against 2326 finished runs in qa-runs — **1095 finished
//! runs with no result rows**, 47% of that stand's history, each worth two
//! notifications on the first pass after the upgrade.
//!
//! **`m20260929_000004_run_completed_notification_cutoff` is what closes that
//! class**, and it is a different mechanism rather than a bigger version of
//! this one: it records when the deployment began notifying and declines
//! everything older, which needs no table to be a faithful census of what was
//! ingested. The two ship together. This one is still not redundant — the
//! cutoff cannot judge a run whose `finished_at` lands just after the
//! migration's own clock through skew, and it is the claim rows that make
//! send-once true for every run *after* the cutoff, which is the steady state.
//!
//! # `id`, and why it is derived rather than random
//!
//! The primary key is a UUID this migration has to invent. Postgres derives it
//! with `md5(tenant || run || kind)::uuid`, which makes the whole statement
//! deterministic: re-running it produces the same rows, and
//! `ON CONFLICT DO NOTHING` absorbs them. `gen_random_uuid()` would have done
//! as well on PG13+, and is avoided only because it needs a version floor this
//! file has no reason to assert. `SQLite` has no `md5`, so its statement builds
//! a v4-shaped id from `randomblob` and leans on `INSERT OR IGNORE` for the
//! same idempotence; the unit tier rebuilds its database per test, so nothing
//! there depends on the value.
//!
//! # `sent_at`
//!
//! `CURRENT_TIMESTAMP` — the instant the deployment decided these runs were
//! already dealt with, which is the truth a reader of this column needs. It is
//! deliberately not the run's own `run_finished_at`: no notification was sent
//! at that instant, and writing one there would invent an event.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Postgres. One row per `(tenant, run)` per kind, ids derived so the
/// statement is idempotent on its own and `ON CONFLICT` is the belt to that
/// brace.
const POSTGRES_UP: &str = r"
INSERT INTO qa_run_notifications
    (id, tenant_id, run_id, notification_kind, event_type, sent_at, created_at, updated_at)
SELECT
    md5(r.tenant_id::text || r.run_id::text || k.kind)::uuid,
    r.tenant_id,
    r.run_id,
    k.kind,
    'run_completed',
    CURRENT_TIMESTAMP,
    CURRENT_TIMESTAMP,
    CURRENT_TIMESTAMP
FROM (SELECT DISTINCT tenant_id, run_id FROM qa_test_results) AS r
CROSS JOIN (VALUES ('run_completed_slack'), ('run_completed_email')) AS k(kind)
ON CONFLICT (tenant_id, run_id, notification_kind, event_type) DO NOTHING;
";

/// `SQLite`. Same set, same columns; `INSERT OR IGNORE` for the conflict and a
/// `randomblob`-built v4 id, because `SQLite` has no `md5`.
const SQLITE_UP: &str = r"
INSERT OR IGNORE INTO qa_run_notifications
    (id, tenant_id, run_id, notification_kind, event_type, sent_at, created_at, updated_at)
SELECT
    lower(
        hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
        substr(hex(randomblob(2)), 2) || '-a' || substr(hex(randomblob(2)), 2) || '-' ||
        hex(randomblob(6))
    ),
    r.tenant_id,
    r.run_id,
    k.kind,
    'run_completed',
    CURRENT_TIMESTAMP,
    CURRENT_TIMESTAMP,
    CURRENT_TIMESTAMP
FROM (SELECT DISTINCT tenant_id, run_id FROM qa_test_results) AS r
CROSS JOIN (SELECT 'run_completed_slack' AS kind UNION ALL SELECT 'run_completed_email') AS k;
";

/// The DDL for `backend`, or an error naming why there is none.
///
/// Same shape and the same refusal as both earlier migrations' `up_ddl`: this
/// gear has no `MySQL` schema at all, so there is no table here to seed.
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

/// Undo the seed and nothing else: the two run-completed kinds, and only rows
/// naming a run that has a projection — which is exactly the set `up()`
/// inserted.
///
/// **This deletes real claims on any run notified between `up()` and `down()`.**
/// That is unavoidable and is the honest behaviour anyway: `down()` exists to
/// return the schema to what it was before this migration, and before it the
/// only run-completed claims that could exist were ones a send had actually
/// taken. A deployment rolling back far enough to run this is choosing to
/// re-notify whatever was notified in between, which is a smaller surprise
/// than a `down()` that silently left rows behind.
const DOWN: &str = r"
DELETE FROM qa_run_notifications
WHERE event_type = 'run_completed'
  AND notification_kind IN ('run_completed_slack', 'run_completed_email')
  AND EXISTS (
      SELECT 1 FROM qa_test_results t
      WHERE t.tenant_id = qa_run_notifications.tenant_id
        AND t.run_id = qa_run_notifications.run_id
  );
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        conn.execute_unprepared(up_ddl(backend)?).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}

/// The same argument both earlier migrations' test modules make: every name
/// above is a runtime string, so `cargo build` is no evidence at all about this
/// file. These run real statements against a real in-memory `SQLite`.
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
    use sea_orm_migration::sea_orm::{
        ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement,
    };
    use sea_orm_migration::{MigrationName as _, MigratorTrait as _};

    use super::super::{Migrator, m20260818_000001_initial, m20260921_000002_smtp_credentials};
    use crate::domain::notify::routing::NotificationKind;

    /// The three claim-slot strings the statements above spell as SQL
    /// literals, repeated once here so
    /// [`the_seeded_kinds_are_the_ones_routing_claims`] can hold them against
    /// `domain::notify::routing` and every other test can assert on them.
    /// They live in the test module rather than beside the SQL because
    /// production has no use for them — the statements carry their own
    /// literals, deliberately (this file's header says why).
    const SLACK_KIND: &str = "run_completed_slack";
    const EMAIL_KIND: &str = "run_completed_email";
    const RUN_COMPLETED_EVENT: &str = "run_completed";

    const TENANT: &str = "00000000-0000-0000-0000-0000000000aa";
    const RUN: &str = "00000000-0000-0000-0000-0000000000bb";
    const OTHER_RUN: &str = "00000000-0000-0000-0000-0000000000cc";

    /// A database with **every migration before this one** applied, so a test
    /// can plant the rows this migration reads and then run it.
    ///
    /// Not `Migrator::up`: that would run this migration too, against an empty
    /// `qa_test_results`, and the seed would have nothing to see. The whole
    /// property under test is what happens to rows that were already there.
    async fn db_before_the_seed() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let manager = sea_orm_migration::SchemaManager::new(&conn);
        sea_orm_migration::MigrationTrait::up(&m20260818_000001_initial::Migration, &manager)
            .await
            .expect("the initial migration applies");
        sea_orm_migration::MigrationTrait::up(
            &m20260921_000002_smtp_credentials::Migration,
            &manager,
        )
        .await
        .expect("the smtp-credentials migration applies");
        conn
    }

    async fn seed(conn: &DatabaseConnection) {
        let manager = sea_orm_migration::SchemaManager::new(conn);
        sea_orm_migration::MigrationTrait::up(&super::Migration, &manager)
            .await
            .expect("the seed migration applies");
    }

    /// Plant one projected result row for `run`.
    async fn project(conn: &DatabaseConnection, run: &str, test_name: &str) {
        conn.execute_unprepared(&format!(
            "INSERT INTO qa_test_results \
             (id, tenant_id, run_id, test_file, test_name, status, created_at, updated_at) \
             VALUES ('{}', '{TENANT}', '{run}', 'tests/t.py', '{test_name}', 'PASSED', \
                     '2026-09-29 00:00:00+00:00', '2026-09-29 00:00:00+00:00')",
            uuid::Uuid::new_v4(),
        ))
        .await
        .expect("insert a projected result row");
    }

    /// Every `(run_id, notification_kind, event_type)` claim in the table,
    /// sorted, so an assertion reads as a set rather than as an insert order.
    async fn claims(conn: &DatabaseConnection) -> Vec<(String, String, String)> {
        let rows = conn
            .query_all_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT run_id, notification_kind, event_type FROM qa_run_notifications",
            ))
            .await
            .expect("select claims");
        let mut out: Vec<(String, String, String)> = rows
            .iter()
            .map(|r| {
                (
                    r.try_get::<String>("", "run_id").expect("run_id"),
                    r.try_get::<String>("", "notification_kind")
                        .expect("notification_kind"),
                    r.try_get::<String>("", "event_type").expect("event_type"),
                )
            })
            .collect();
        out.sort();
        out
    }

    /// The property this migration exists for: a run this gear had already
    /// projected comes out of the upgrade with both run-completed slots
    /// already claimed, so `claim_notification` answers `false` for it
    /// forever after.
    #[tokio::test]
    async fn an_already_projected_run_is_claimed_on_both_channels() {
        let conn = db_before_the_seed().await;
        project(&conn, RUN, "test_one").await;
        // Two rows for the same run: the seed must key on the run, not on the
        // result row, or a run with 400 tests would get 800 claims.
        project(&conn, RUN, "test_two").await;

        seed(&conn).await;

        let mut expected = vec![
            (
                RUN.to_owned(),
                SLACK_KIND.to_owned(),
                RUN_COMPLETED_EVENT.to_owned(),
            ),
            (
                RUN.to_owned(),
                EMAIL_KIND.to_owned(),
                RUN_COMPLETED_EVENT.to_owned(),
            ),
        ];
        expected.sort();
        assert_eq!(claims(&conn).await, expected);
    }

    /// The other half: a deployment with nothing ingested gets no claims at
    /// all. Without this, the test above would also pass for a seed that
    /// inserted a claim for every run in existence.
    #[tokio::test]
    async fn a_deployment_with_no_projection_is_seeded_with_nothing() {
        let conn = db_before_the_seed().await;

        seed(&conn).await;

        assert_eq!(claims(&conn).await, Vec::new());
    }

    /// Applying it twice inserts nothing the second time. The `Migrator`
    /// never would, but a deployment that re-runs a half-applied upgrade
    /// might, and the conflict clause is what makes that safe rather than a
    /// primary-key failure.
    #[tokio::test]
    async fn re_running_the_seed_inserts_nothing_new() {
        let conn = db_before_the_seed().await;
        project(&conn, RUN, "test_one").await;
        project(&conn, OTHER_RUN, "test_two").await;

        seed(&conn).await;
        let after_first = claims(&conn).await;
        seed(&conn).await;

        assert_eq!(after_first.len(), 4, "two runs, two channels each");
        assert_eq!(claims(&conn).await, after_first);
    }

    /// The literals above are the strings `domain::notify::routing` actually
    /// claims on. They are duplicated deliberately (this file's header says
    /// why); this is what keeps the duplication honest.
    #[test]
    fn the_seeded_kinds_are_the_ones_routing_claims() {
        assert_eq!(NotificationKind::RunCompletedSlack.as_str(), SLACK_KIND);
        assert_eq!(NotificationKind::RunCompletedEmail.as_str(), EMAIL_KIND);
    }

    /// `down()` undoes `up()` — the same property both earlier migrations
    /// pin for their own halves.
    #[tokio::test]
    async fn the_down_migration_removes_the_seed() {
        let conn = db_before_the_seed().await;
        project(&conn, RUN, "test_one").await;
        seed(&conn).await;
        assert_eq!(claims(&conn).await.len(), 2);

        let manager = sea_orm_migration::SchemaManager::new(&conn);
        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .expect("down applies");

        assert_eq!(claims(&conn).await, Vec::new());
    }

    /// A migration missing from `Migrator::migrations()` runs on no
    /// deployment at all, and every test above would still pass — they call
    /// `up()` directly. This is the one that would go red.
    #[test]
    fn the_migration_is_registered_in_the_chain() {
        let registered: Vec<String> = Migrator::migrations()
            .iter()
            .map(|m| m.name().to_owned())
            .collect();
        assert!(
            registered.contains(&super::Migration.name().to_owned()),
            "the seed migration is not in the applied chain; registered: {registered:?}"
        );
    }
}
