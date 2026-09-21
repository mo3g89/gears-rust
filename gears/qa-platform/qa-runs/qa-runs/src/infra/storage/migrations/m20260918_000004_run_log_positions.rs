//! `qa_run_log_positions` — one execution node's most recent archived
//! line's kubelet emission instant (Task 2, WS5).
//!
//! **Append-only**, per `migrations::mod`'s own header: a new table, never
//! an edit to `m20260813_000003_initial`. This is the schema half of the
//! replacement named in that migration's neighbouring code —
//! `infra::executor::argo::watch`'s deleted per-node suppression counter and
//! `domain::repos::run_logs_repo::LogResume`'s former count-based
//! anchors — both of which named "a parser and a schema change neither of
//! which exists yet" as the correct fix. This is the schema change; the
//! parser is `domain::repos::log_line::split_kubelet_timestamp`.
//!
//! Follows `m20260813_000003_initial`'s own conventions: a backend `match`
//! producing one `execute_unprepared` DDL blob per dialect. That migration
//! declares only `POSTGRES_UP`/`SQLITE_UP` (no `MYSQL_UP` — this gear has no
//! `MySQL` execution tier and its `up()` match has no `MySQL` arm), and this
//! one follows that, not the plan's illustrative three-dialect list.
//!
//! The foreign key is declared **out of line**, on a trailing `FOREIGN KEY`
//! clause, never inline on `run_id` — `schema_behaviour_tests.rs`'s
//! `every_dialect_cascades_from_qa_runs_on_the_composite_key` already pins
//! this for `qa_run_logs`'s identical composite parent key, and the same
//! `MySQL`/`InnoDB` hazard (a silently dropped constraint) applies here even
//! though this gear has no tier that would catch it running.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_run_log_positions (
    run_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    node TEXT NOT NULL,
    last_emitted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (run_id, node),
    FOREIGN KEY (run_id, tenant_id) REFERENCES qa_runs(id, tenant_id) ON DELETE CASCADE
);
";

const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_run_log_positions (
    run_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    node TEXT NOT NULL,
    last_emitted_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (run_id, node),
    FOREIGN KEY (run_id, tenant_id) REFERENCES qa_runs(id, tenant_id) ON DELETE CASCADE
);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS qa_run_log_positions;")
            .await?;
        Ok(())
    }
}

/// Schema tests for this migration alone — see
/// `m20260813_000003_initial`'s own test module doc for why these exist
/// (`cargo build` proves nothing about a table or column name that only a
/// query round-trips) and why they run against `SQLite` only.
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
    use sea_orm::{ActiveModelTrait, ActiveValue, ConnectionTrait, Database, EntityTrait};
    use sea_orm_migration::MigratorTrait;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::infra::storage::entity::{run, run_log_position};
    use crate::infra::storage::migrations::Migrator;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 2026-09-18 00:00:00 UTC, this task's own fixture instant.
    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_789_689_600).unwrap()
    }

    /// Every migration this gear has, applied in order — not only this
    /// file's own, because `qa_run_log_positions`' foreign key needs
    /// `qa_runs` (declared by `m20260813_000003_initial`) to exist first.
    async fn migrated_db() -> sea_orm::DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("failed to connect to in-memory sqlite database");
        Migrator::up(&conn, None)
            .await
            .expect("failed to run qa-runs migrations");
        conn
    }

    fn run_am(id: Uuid, tenant: Uuid) -> run::ActiveModel {
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
            xpass: ActiveValue::Set(0),
            total: ActiveValue::Set(0),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    fn position_am(run_id: Uuid, tenant: Uuid, node: &str) -> run_log_position::ActiveModel {
        run_log_position::ActiveModel {
            run_id: ActiveValue::Set(run_id),
            node: ActiveValue::Set(node.to_owned()),
            tenant_id: ActiveValue::Set(tenant),
            last_emitted_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    /// The whole point of this module: every column name in the real entity,
    /// exercised by a real INSERT and a real SELECT, and a second node for
    /// the same run landing as a second row rather than a conflict.
    #[tokio::test]
    async fn every_column_round_trips_and_the_key_is_per_node() {
        let conn = migrated_db().await;
        let tenant = uuid(1);
        let run_id = uuid(2);
        run_am(run_id, tenant).insert(&conn).await.unwrap();

        position_am(run_id, tenant, "node-a")
            .insert(&conn)
            .await
            .unwrap();
        position_am(run_id, tenant, "node-b")
            .insert(&conn)
            .await
            .unwrap();

        let rows = run_log_position::Entity::find()
            .all(&conn)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "two nodes for one run are two rows, not one");
    }

    /// The composite `(run_id, node)` key refuses a duplicate.
    #[tokio::test]
    async fn the_same_run_and_node_twice_collides_on_the_primary_key() {
        let conn = migrated_db().await;
        let tenant = uuid(1);
        let run_id = uuid(2);
        run_am(run_id, tenant).insert(&conn).await.unwrap();

        position_am(run_id, tenant, "node-a")
            .insert(&conn)
            .await
            .unwrap();

        assert!(
            position_am(run_id, tenant, "node-a")
                .insert(&conn)
                .await
                .is_err(),
            "a second row for the same (run_id, node) must collide on the primary key",
        );
    }

    /// D-RLP-1's retention shape, reused: deleting a run deletes its
    /// positions along with it.
    #[tokio::test]
    async fn deleting_a_run_deletes_its_log_positions() {
        let conn = migrated_db().await;
        let tenant = uuid(1);
        let run_id = uuid(2);
        run_am(run_id, tenant).insert(&conn).await.unwrap();
        position_am(run_id, tenant, "node-a")
            .insert(&conn)
            .await
            .unwrap();

        assert!(
            !run_log_position::Entity::find()
                .all(&conn)
                .await
                .unwrap()
                .is_empty(),
            "setup failed to write the position row"
        );

        run::Entity::delete_by_id(run_id).exec(&conn).await.unwrap();

        assert!(
            run_log_position::Entity::find()
                .all(&conn)
                .await
                .unwrap()
                .is_empty(),
            "deleting a run must cascade to its log positions"
        );
    }

    /// `down()` drops exactly this migration's own table.
    #[tokio::test]
    async fn down_drops_the_table() {
        let conn = migrated_db().await;
        Migrator::down(&conn, None).await.unwrap();

        let result = conn
            .execute_unprepared("SELECT 1 FROM qa_run_log_positions")
            .await;
        assert!(result.is_err(), "the table must be gone after down()");
    }

    /// The foreign key must not be declared inline on `run_id` — see this
    /// module's own doc for why an inline `REFERENCES` on a composite key's
    /// first column is the `MySQL`/`InnoDB` hazard
    /// `schema_behaviour_tests.rs` pins for `qa_run_logs`.
    #[test]
    fn the_foreign_key_is_declared_out_of_line_in_every_dialect() {
        for (name, ddl) in [("postgres", super::POSTGRES_UP), ("sqlite", super::SQLITE_UP)] {
            assert!(
                ddl.contains(
                    "FOREIGN KEY (run_id, tenant_id) REFERENCES qa_runs(id, tenant_id) \
                     ON DELETE CASCADE"
                ),
                "{name} must declare the composite cascade as a trailing FOREIGN KEY clause",
            );
            let run_id_line = ddl
                .lines()
                .find(|l| l.trim_start().starts_with("run_id "))
                .unwrap_or_else(|| panic!("{name} declares no run_id column"));
            assert!(
                !run_id_line.contains("REFERENCES"),
                "{name}'s cascade must not be declared inline on run_id: {run_id_line}",
            );
        }
    }
}
