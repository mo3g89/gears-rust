//! `qa_run_logs` — one durable log per run.
//!
//! **Its own table, not a column on `qa_runs`**, so that selecting a run's
//! log text is a query against one row rather than a column a list query
//! could ever pull in for every run in a page — the migration this table
//! shipped in (`m20260831_000008_run_logs`, since squashed into
//! `migrations::m20260813_000003_initial`) named this by history: legacy
//! kept the same text on the run row itself and one comment there records
//! that selecting it for a full run history once `OOMKilled` the process.
//! `runs_sea_repo::no_list_query_reaches_the_log_table` is the guard that
//! keeps it structurally true here.
//!
//! # `Model` derives `Debug` over raw log text, and cannot not
//!
//! [`Model`] is the third carrier of a tenant's log text on this path, after
//! `infra::logs::archive::Pending` and `domain::repos::ArchivedLog` — both of
//! which hand-write a redacting `Debug` rather than deriving one, for the
//! reason those impls record (this crate has a recorded cross-tenant
//! disclosure of exactly that mechanism).
//!
//! `Model` cannot follow them: `DeriveEntityModel` requires `Debug` on the
//! model it is applied to. Measured, not assumed — removing it produces
//! `error[E0277]: run_log::Model doesn't implement std::fmt::Debug` from the
//! generated code, repeatedly.
//!
//! What contains it instead is reach. `run_log::{Model, ActiveModel}` are
//! named in exactly one place, `infra::storage::run_logs_sea_repo`, whose
//! `get_log` converts a `Model` into an `ArchivedLog` and drops it in the same
//! expression; neither type crosses into `domain` or `api`, and no function
//! holding one is `#[tracing::instrument]`ed. **A `#[tracing::instrument]` on
//! anything taking a `run_log::Model` — or a `dbg!`/`{:?}` on one — would
//! print a tenant's whole log**, and no type-level control stops it here.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_run_logs")]
#[secure(tenant_col = "tenant_id", resource_col = "run_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub run_id: Uuid,
    pub tenant_id: Uuid,
    #[sea_orm(column_type = "Text")]
    pub text: String,
    pub lines: i64,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
