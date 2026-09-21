//! `qa_runs.xpass` — the seventh denormalized counter, and the one that makes
//! the categorised counters sum to `total` unconditionally.
//!
//! **Append-only**, per `migrations::mod`'s own header: a new column added by
//! its own migration, never an edit to `m20260813_000003_initial`.
//!
//! ## What this fixes, and what its predecessor left open
//!
//! `m20260921_000005_run_xfail_counter` gave `XFAIL` a counter so that
//! `passed + failed + skipped + in_progress + xfail == total`, and said in its
//! own header that `XPASS` was "deliberately not" given the twin. That left
//! the identity conditional: a suite containing an unexpected pass still had a
//! row in `total` and in no counter, so the same difference the `xfail` work
//! set out to remove was still visible — just on a rarer suite. The owner's
//! intent for that work was that the buckets reconcile, which `xfail` alone
//! did not achieve, so the scope is reopened here rather than left as a
//! footnote.
//!
//! `XPASS` is an expected failure that unexpectedly passed: a suite whose
//! `xfail` marker is now stale. It is worth seeing and it is not a test
//! failure, which is why it gets its own counter rather than being folded into
//! `passed` — the source system counts the two as their own categories in
//! analytics (`../testrunner/manager/src/routes/analytics.rs:1359-1360`) and
//! never folds either.
//!
//! ## Why a column rather than a recomputation
//!
//! The counters are denormalized onto `qa_runs` by decision, and
//! `domain::repos::runs_repo`'s `RunResultDelta` table names
//! `add_result_counts` as their single writer. A seventh counter that was
//! recomputed per query instead would be the only one of the seven not written
//! that way, and `IngestService::reconciled_counts` — which re-tallies the
//! rows and corrects the columns — would have nothing to correct it against.
//!
//! ## `DEFAULT 0` and `NOT NULL` together, in one statement
//!
//! Every existing row gets `0`, which is the truthful backfill: it is what the
//! counter *would* have held, because nothing ever incremented it. It is not,
//! however, the count of `XPASS` rows those runs actually have — a historical
//! run with unexpected passes keeps a `total` its counters still do not sum
//! to, and no migration can fix that without re-tallying
//! `qa_run_test_results`, which is `reconciled_counts`' job and happens on the
//! next completion rather than here.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE qa_runs ADD COLUMN IF NOT EXISTS xpass INTEGER NOT NULL DEFAULT 0;
";

/// `SQLite` has no `IF NOT EXISTS` on `ADD COLUMN`; the migration runner
/// applies each migration exactly once, so the bare form is the same
/// statement in practice.
const SQLITE_UP: &str = r"
ALTER TABLE qa_runs ADD COLUMN xpass INTEGER NOT NULL DEFAULT 0;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
            other => {
                return Err(DbErr::Migration(format!(
                    "unsupported database backend: {other:?}"
                )));
            }
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("ALTER TABLE qa_runs DROP COLUMN xpass;")
            .await?;
        Ok(())
    }
}

/// Schema tests for this migration alone — see
/// `m20260813_000003_initial`'s own test module doc for why these exist
/// (`cargo build` proves nothing about a column name that only a query
/// round-trips) and why they run against `SQLite` only.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "a schema test needs a raw connection and SecureORM deliberately exposes none \
              -- see m20260813_000003_initial's own test module doc for the full argument, \
              which applies identically here"
)]
mod tests {
    use sea_orm::{ActiveModelTrait, ActiveValue, Database, EntityTrait};
    use sea_orm_migration::MigratorTrait;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::infra::storage::entity::run;
    use crate::infra::storage::migrations::Migrator;

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_789_689_600).unwrap()
    }

    async fn migrated_db() -> sea_orm::DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("failed to connect to in-memory sqlite database");
        Migrator::up(&conn, None)
            .await
            .expect("failed to run qa-runs migrations");
        conn
    }

    fn run_am(id: Uuid, tenant: Uuid, xpass: i32) -> run::ActiveModel {
        run::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant),
            name: ActiveValue::Set("smoke-1".to_owned()),
            run_kind: ActiveValue::Set("plan".to_owned()),
            target_repo_id: ActiveValue::Set(None),
            target_path: ActiveValue::Set(None),
            target_test_file: ActiveValue::Set(None),
            target_custom_plan_id: ActiveValue::Set(None),
            target_collect_url: ActiveValue::Set(None),
            environment_id: ActiveValue::Set(None),
            test_version: ActiveValue::Set(None),
            app_version: ActiveValue::Set(None),
            app_build: ActiveValue::Set(None),
            state: ActiveValue::Set("dispatching".to_owned()),
            resolved_exclusive: ActiveValue::Set(true),
            exclusive_tier: ActiveValue::Set("plan.yaml".to_owned()),
            is_validation: ActiveValue::Set(true),
            parameters: ActiveValue::Set(serde_json::json!([])),
            include_tags: ActiveValue::Set(serde_json::json!([])),
            exclude_tags: ActiveValue::Set(serde_json::json!([])),
            source: ActiveValue::Set("manual".to_owned()),
            schedule_id: ActiveValue::Set(None),
            bundle_ids: ActiveValue::Set(serde_json::json!([])),
            execution_ref: ActiveValue::Set(None),
            log_storage_ref: ActiveValue::Set(None),
            timeout_at: ActiveValue::Set(None),
            started_at: ActiveValue::Set(None),
            finished_at: ActiveValue::Set(None),
            error: ActiveValue::Set(None),
            passed: ActiveValue::Set(0),
            failed: ActiveValue::Set(0),
            skipped: ActiveValue::Set(0),
            in_progress: ActiveValue::Set(0),
            xfail: ActiveValue::Set(0),
            xpass: ActiveValue::Set(xpass),
            total: ActiveValue::Set(0),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    /// The column exists, is writable, and reads back — the only thing that
    /// actually exercises the name in both the DDL and `entity::run`.
    #[tokio::test]
    async fn the_xpass_counter_round_trips() {
        let conn = migrated_db().await;
        let id = Uuid::from_u128(2);
        run_am(id, Uuid::from_u128(1), 7).insert(&conn).await.unwrap();

        let stored = run::Entity::find_by_id(id)
            .one(&conn)
            .await
            .unwrap()
            .expect("the run must be readable");
        assert_eq!(stored.xpass, 7, "the xpass counter must round-trip");
    }

    /// The backfill: a writer that does not mention `xpass` still gets a row,
    /// because the column is `NOT NULL DEFAULT 0` rather than nullable.
    #[tokio::test]
    async fn an_unmentioned_xpass_defaults_to_zero() {
        let conn = migrated_db().await;
        let id = Uuid::from_u128(3);
        let mut am = run_am(id, Uuid::from_u128(1), 0);
        am.xpass = ActiveValue::NotSet;
        am.insert(&conn).await.unwrap();

        let stored = run::Entity::find_by_id(id)
            .one(&conn)
            .await
            .unwrap()
            .expect("the run must be readable");
        assert_eq!(stored.xpass, 0, "the default backfill is zero, not NULL");
    }
}
