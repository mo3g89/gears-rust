//! `SeaORM` entity for `qa_run_log_positions` — one execution node's most
//! recent archived line's kubelet emission instant (Task 2, WS5).
//!
//! Keyed `(run_id, node)`, cascading from `qa_runs(id, tenant_id)` exactly as
//! `qa_run_logs` does — see `migrations::m20260918_000004_run_log_positions`
//! for the DDL and why the foreign key is declared out of line.
//!
//! # Why this table exists, in one sentence
//!
//! It replaces `infra::executor::argo::watch`'s deleted per-node
//! line-suppression counter (which re-read a node's whole pod log from
//! byte 0 on every re-attach) with kubelet's own per-line emission
//! timestamp, so a resumed read can ask Kubernetes to start late
//! (`LogParams::since_time`) instead of asking this process to suppress
//! what it already has.
//!
//! Unlike `run_log::Model`, this type carries no log text at all — only a
//! node name and a timestamp — so it needs none of that entity's
//! hand-written, redacting `Debug`.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_run_log_positions")]
#[secure(tenant_col = "tenant_id", resource_col = "run_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub run_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub node: String,
    pub tenant_id: Uuid,
    pub last_emitted_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
