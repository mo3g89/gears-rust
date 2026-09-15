//! `SeaORM` entity for the `qa_runs` table.
//!
//! Every enum-valued column is a `String` here, not a domain type. The decode
//! happens in the mapper so it can **fail closed** on an unrecognized value —
//! a `state` of `"suceeded"` must produce an error, never a default. A
//! `#[sea_orm(enum)]` column would give `SeaORM` the decode, and a corrupt row
//! would then surface as a bare `DbErr` with no way to say which run and which
//! column; keeping the string here keeps that decision in code that can.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_runs")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// `{slug}-{n}`, unique within the tenant.
    pub name: String,
    /// `qa_runs_sdk::RunKind::as_str` — `plan` | `test` | `custom_plan` |
    /// `collect`. Says which of the five `target_*` columns below are
    /// meaningful.
    pub run_kind: String,
    pub target_repo_id: Option<Uuid>,
    pub target_path: Option<String>,
    pub target_test_file: Option<String>,
    pub target_custom_plan_id: Option<Uuid>,
    /// Where a `collect` run's runner posts its per-file case counts. `NULL` for
    /// every other kind, and legal-but-`NULL` for a collect run that reports
    /// nowhere — see `m20260818_000006_collect_target`.
    pub target_collect_url: Option<String>,
    /// Physical column stays `environment_id`: Phase B renames the aggregate and
    /// this Rust field, not the column. The rename to the column itself is
    /// deferred to this plan's later expand/contract migrations.
    pub environment_id: Option<Uuid>,
    /// Branch actually resolved and executed against.
    pub test_version: Option<String>,
    /// Snapshotted from the target platform at launch, never re-derived — see
    /// the migration's module header.
    pub app_version: Option<String>,
    pub app_build: Option<String>,
    /// `qa_runs_sdk::RunState::as_str`, ten values.
    pub state: String,
    /// The *resolved* exclusivity decision, not the tri-state request. See the
    /// column comment in the migration for why the short name is a trap.
    pub resolved_exclusive: bool,
    /// `qa_runs_sdk::ExclusiveTier::as_str` — which tier supplied the decision.
    pub exclusive_tier: String,
    pub is_validation: bool,
    /// JSON array of `{name, value}` launch parameters.
    pub parameters: Json,
    /// JSON array of tag strings.
    pub include_tags: Json,
    /// JSON array of tag strings.
    pub exclude_tags: Json,
    /// `qa_runs_sdk::RunSource::as_str` — `manual` | `scheduled`.
    pub source: String,
    pub schedule_id: Option<Uuid>,
    /// JSON array of bundle UUIDs, one per repository group.
    pub bundle_ids: Json,
    pub execution_ref: Option<String>,
    pub log_storage_ref: Option<String>,
    pub timeout_at: Option<OffsetDateTime>,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub error: Option<String>,
    /// The five `qa_runs_sdk::RunResult` counters, denormalized onto the run.
    /// `failed` counts failed *and* errored results; there is no sixth counter.
    ///
    /// **`i32` here, `usize` on `RunResult` — the mapper owns that seam.** The
    /// column is `INTEGER`, the SDK counters are `usize`, and the workspace
    /// denies `clippy::cast_sign_loss`, so `as usize` is not available. Convert
    /// the way qa-catalog already does for the same problem: a helper that
    /// fails closed on a negative value rather than wrapping it
    /// (`qa-catalog/src/infra/storage/mapper.rs`, `u64_from_db`). A negative
    /// count is corrupt, and a wrapped one becomes an enormous positive that
    /// would sail through `derive_terminal_state`'s `failed > 0` test.
    pub passed: i32,
    pub failed: i32,
    pub skipped: i32,
    pub in_progress: i32,
    pub total: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
