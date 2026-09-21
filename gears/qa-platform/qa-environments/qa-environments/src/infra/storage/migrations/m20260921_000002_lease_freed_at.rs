//! `qa_environment_leases.freed_at` — the instant the environment last
//! transitioned **to free**.
//!
//! **Append-only**, per `migrations::mod`'s own header: a new column added by
//! its own migration, never an edit to `m20260812_000001_initial`.
//!
//! ## What this exists for
//!
//! `cpt-cf-qa-nfr-dispatch-latency` (`docs/PRD.md` §6) is stated over
//! *environment becomes free → queued run starts*. Nothing recorded the first
//! of those two instants, so the requirement could not be measured at all: the
//! 2026-09-18 attempt read `qa_run_queue.dispatched_at − enqueued_at` instead,
//! which is a different interval (queue residency, inflated by queue depth and
//! by the predecessor's own runtime) and was retracted for it —
//! `docs/DESIGN.md` §3.11, "The dispatch-latency window, and the measurement
//! that was retracted". This column is the missing endpoint.
//!
//! ## Why a column, and not an event or a metric
//!
//! * **A metric alone cannot answer "for *this* run".** The quantity the NFR
//!   names is per run: the window between the free transition a particular
//!   queued run was waiting on and that run's own start. A histogram of free
//!   instants has no key to join a run to, and the two instants are observed
//!   in two different gears — qa-environments frees, qa-runs starts — so
//!   nothing downstream could pair them either.
//! * **An event would have to be durable to be useful**, because the
//!   dispatcher may pick the environment up after a control-plane restart: the
//!   free transition can happen in one process lifetime and the start in the
//!   next. A durable event is a table; a table keyed by environment with one
//!   live row is this column.
//! * **The lease row is already the single record of who holds the
//!   environment** (`domain::queue`'s module header: "here there is one
//!   record"). Putting *since when it is not held* anywhere else would create
//!   a second record of the same fact, which is the merge that module deleted.
//!
//! So: one nullable column on the row that already arbitrates the fact, read
//! back by the acquisition that consumes it. It survives a restart because it
//! is in Postgres, and it pairs to a run because exactly one acquisition can
//! take the environment out of `Free`.
//!
//! ## NULL, and why there is no backfill
//!
//! `NULL` means *this environment has no recorded transition to free* — it was
//! never held, or it was last freed before this migration ran. There is no
//! honest backfill: `updated_at` is the last write of **any** kind, so on a
//! held row it is an acquisition instant and on a freed row it is only
//! coincidentally the free instant. Writing it in would manufacture anchors
//! that are wrong exactly where the lease is busiest. A `NULL` anchor produces
//! no measurement, which is the correct answer for a transition nobody
//! observed; `service::dispatch` counts those rows separately
//! (`qa_runs_dispatch_latency_unanchored_total`) so the gap in coverage is
//! visible rather than silent.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE qa_environment_leases ADD COLUMN IF NOT EXISTS freed_at TIMESTAMPTZ NULL;
";

/// `SQLite` has no `IF NOT EXISTS` on `ADD COLUMN`; the migration runner
/// applies each migration exactly once, so the bare form is the same
/// statement in practice.
const SQLITE_UP: &str = r"
ALTER TABLE qa_environment_leases ADD COLUMN freed_at TEXT NULL;
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
        conn.execute_unprepared("ALTER TABLE qa_environment_leases DROP COLUMN freed_at;")
            .await?;
        Ok(())
    }
}

/// Schema tests for this migration alone — `cargo build` proves nothing about
/// a column name that only a query round-trips. `SQLite` only, for the reason
/// `m20260812_000001_initial`'s own test module gives.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "a schema test needs a raw connection and SecureORM deliberately exposes none \
              -- see m20260812_000001_initial's own test module doc for the full argument, \
              which applies identically here"
)]
mod tests {
    use sea_orm::{ActiveModelTrait, ActiveValue, Database, EntityTrait};
    use sea_orm_migration::MigratorTrait;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::infra::storage::entity::{environment, environment_lease};
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
            .expect("failed to run qa-environments migrations");
        conn
    }

    /// The lease row has a foreign key onto `qa_environments`, so a parent row
    /// has to exist before a lease can be inserted.
    fn environment_am(id: Uuid, tenant: Uuid) -> environment::ActiveModel {
        environment::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant),
            name: ActiveValue::Set("staging-a".to_owned()),
            product_id: ActiveValue::Set(Uuid::from_u128(9)),
            description: ActiveValue::Set(None),
            available: ActiveValue::Set(true),
            observed_version: ActiveValue::Set(None),
            observed_build: ActiveValue::Set(None),
            default_branch: ActiveValue::Set(None),
            is_default: ActiveValue::Set(false),
            version_detect_error: ActiveValue::Set(None),
            version_detected_at: ActiveValue::Set(None),
            credentials: ActiveValue::Set(serde_json::json!([])),
            observed_attrs: ActiveValue::Set(serde_json::json!({})),
            config: ActiveValue::Set(serde_json::json!({})),
            observed_base_url: ActiveValue::Set(None),
            health_state: ActiveValue::Set("unknown".to_owned()),
            health_detail: ActiveValue::Set(None),
            health_checked_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    fn lease_am(
        environment_id: Uuid,
        tenant: Uuid,
        freed_at: Option<OffsetDateTime>,
    ) -> environment_lease::ActiveModel {
        environment_lease::ActiveModel {
            environment_id: ActiveValue::Set(environment_id),
            tenant_id: ActiveValue::Set(tenant),
            mode: ActiveValue::Set("free".to_owned()),
            holders: ActiveValue::Set(serde_json::json!([])),
            version: ActiveValue::Set(1),
            updated_at: ActiveValue::Set(now()),
            freed_at: ActiveValue::Set(freed_at),
        }
    }

    /// The column exists, is writable, and reads back — the only thing that
    /// actually exercises the name in both the DDL and `entity::environment_lease`.
    #[tokio::test]
    async fn the_freed_at_instant_round_trips() {
        let conn = migrated_db().await;
        let tenant = Uuid::from_u128(1);
        let environment_id = Uuid::from_u128(2);
        environment_am(environment_id, tenant)
            .insert(&conn)
            .await
            .unwrap();
        lease_am(environment_id, tenant, Some(now()))
            .insert(&conn)
            .await
            .unwrap();

        let stored = environment_lease::Entity::find_by_id(environment_id)
            .one(&conn)
            .await
            .unwrap()
            .expect("the lease must be readable");
        assert_eq!(
            stored.freed_at,
            Some(now()),
            "the free instant must round-trip"
        );
    }

    /// The no-backfill case: a row written without one reads `NULL`, not a
    /// fabricated instant. That is what makes "no recorded free transition"
    /// distinguishable from "freed at the epoch".
    #[tokio::test]
    async fn a_lease_with_no_recorded_free_reads_null() {
        let conn = migrated_db().await;
        let tenant = Uuid::from_u128(1);
        let environment_id = Uuid::from_u128(3);
        environment_am(environment_id, tenant)
            .insert(&conn)
            .await
            .unwrap();
        let mut am = lease_am(environment_id, tenant, None);
        am.freed_at = ActiveValue::NotSet;
        am.insert(&conn).await.unwrap();

        let stored = environment_lease::Entity::find_by_id(environment_id)
            .one(&conn)
            .await
            .unwrap()
            .expect("the lease must be readable");
        assert_eq!(
            stored.freed_at, None,
            "an unwritten free instant is NULL, never a fabricated one"
        );
    }
}
