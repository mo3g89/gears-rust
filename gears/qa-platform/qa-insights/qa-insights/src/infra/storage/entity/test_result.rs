//! `SeaORM` entity for the `qa_test_results` table.
//!
//! File-level test outcomes, and the table every analytics surface reads.
//!
//! **Eight of these columns are denormalized copies of run attributes** —
//! `product_version`, `app_build`, `platform_id`, `repo_id`, `plan_path`,
//! `branch`, `run_finished_at` and `run_created_at` (the last added by Task
//! 21b). Legacy gets them from a `JOIN run_results`
//! (`manager/src/routes/analytics.rs:2450`); here that join is a cross-gear
//! call, which `cpt-cf-qa-principle-async-insights` forbids on a hot path. The
//! migration's module header carries the full argument and the cost.
//!
//! `status` is a `String`, not an enum, and deliberately so: ingest must not
//! fail closed on a spelling the runner invents. The decode-and-bucketize
//! belongs to the analytics cores, which treat anything outside
//! `PASSED`/`FAILED`/`ERROR` as "other" exactly as legacy does
//! (`bucketize_status`).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_test_results")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub run_id: Uuid,
    /// `""` when the producer reported no file, never `NULL` — the column is
    /// `NOT NULL DEFAULT ''`. That is what lets the ingest dedupe be a per-run
    /// delete rather than legacy's per-row `COALESCE(test_file, '')` predicate
    /// (`manager/src/routes/runs.rs:1154`); `results_sea_repo` deletes on
    /// `run_id` alone, exactly as legacy's own *bulk* writer does
    /// (`manager/src/services/argo.rs:2593`).
    pub test_file: String,
    pub test_name: String,
    pub status: String,
    pub duration: Option<String>,
    pub launch_id: Option<String>,
    pub jira_key: Option<String>,
    pub product_version: Option<String>,
    /// Denormalized from `qa_runs.app_build`. The analytics *projection* to
    /// [`Self::product_version`]'s *filter*: legacy filters on `r.app_version`
    /// and groups on `r.app_build` (`manager/src/routes/analytics.rs:961`,
    /// `:988` for the filter; `:1032-1033` for the projection). Two different
    /// values, so one does not stand in for the other.
    ///
    /// `None` means the run named no build; legacy's `"unknown"` label is the
    /// consumer's substitution, not this column's — see
    /// [`crate::domain::analytics::ExecRow::build`].
    pub app_build: Option<String>,
    pub platform_id: Option<Uuid>,
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    pub branch: Option<String>,
    /// `None` while the run is still going: legacy writes `RUNNING`/`PENDING`
    /// rows too (`manager/src/services/argo.rs:2603`).
    pub run_finished_at: Option<OffsetDateTime>,
    /// Denormalized from `qa_runs.created_at` — **the fallback half of legacy's
    /// `COALESCE(rr.finished_at, rr.created_at)`**, and therefore the column that
    /// makes a row of an *unfinished* run land on a fixed instant.
    ///
    /// Distinct from [`Self::created_at`] and not interchangeable with it. That
    /// one is when this row was written, and ingest rewrites a run's rows on
    /// every result event, so it moves for the whole life of the run; this one is
    /// the run's own creation instant and does not move. The migration's DDL
    /// comment carries what windowing on the wrong one did.
    ///
    /// `None` if a run ever reports no creation instant. `qa_runs_sdk::Run`
    /// makes it non-optional today, so ingest always fills it.
    pub run_created_at: Option<OffsetDateTime>,
    /// The row's position within the batch that wrote it, and **the port of
    /// legacy's `test_results.id` SERIAL used as an ordering tiebreak**.
    ///
    /// Not one of the eight denormalized run columns above: it carries no run
    /// attribute. It is absent from both `domain::analytics::ExecRow` and
    /// `qa_insights_sdk::TestResultRecord` for the same reason `updated_at` is —
    /// a persistence detail no consumer reads.
    ///
    /// Legacy's winner among rows of one run sharing a file is the last row the
    /// log parser produced (`analytics.rs:971` orders on `t.id DESC`, `:1042`
    /// re-sorts *stably*, `argo.rs:2598-2615` inserts in parse order). A random
    /// `Uuid` primary key cannot express that; this can, and
    /// `results_sea_repo` derives it from the batch's own order rather than
    /// taking it from a caller.
    pub ingest_ordinal: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
