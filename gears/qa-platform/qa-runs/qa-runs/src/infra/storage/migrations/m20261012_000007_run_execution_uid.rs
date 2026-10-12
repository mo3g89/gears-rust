//! `qa_runs.execution_uid` — the API-server uid of the Workflow qa-runs
//! submitted for a run, stored beside the Workflow's name in `execution_ref`.
//!
//! **Append-only**, per `migrations::mod`'s own header: a new column added by
//! its own migration, never an edit to `m20260813_000003_initial`.
//!
//! ## Why
//!
//! Re-verification P2: a runner pod is bound to its run by the Workflow *name*
//! alone, and Workflow names are derived from the run name, so they are
//! predictable. Anyone who may delete and create Workflows in the Argo
//! namespace can recreate a live run's Workflow under the same name, and the
//! pods of that impostor then pass the name check and receive the run's
//! credentials. The uid is minted by the API server at creation and cannot be
//! chosen or recreated, so the uid recorded here is what a pod's owning
//! Workflow is compared against.
//!
//! ## Nullable, and what `NULL` means
//!
//! Nullable, with no default: a run submitted before this migration has no
//! uid, and neither does a run whose executor mints none. Such a run is
//! refused credentials (fail closed; `UPGRADING.md` says so). A runner fetches
//! its credentials once, at start, so only a pod that starts after the upgrade
//! under a pre-upgrade submit is affected. There is no backfill, because no
//! source for the historical uids exists in this table.
//!
//! The column is not part of the domain `Run` or the SDK `Run`: the uid is a
//! binding detail, not run data, and is read only through
//! `RunsRepository::execution_uid`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE qa_runs ADD COLUMN IF NOT EXISTS execution_uid TEXT NULL;
";

/// `SQLite` has no `IF NOT EXISTS` on `ADD COLUMN`; the migration runner
/// applies each migration exactly once, so the bare form is the same
/// statement in practice.
const SQLITE_UP: &str = r"
ALTER TABLE qa_runs ADD COLUMN execution_uid TEXT NULL;
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
        conn.execute_unprepared("ALTER TABLE qa_runs DROP COLUMN execution_uid;")
            .await?;
        Ok(())
    }
}
