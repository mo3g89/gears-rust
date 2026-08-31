//! `SeaORM` entity for the `qa_run_queue` table.
//!
//! There is no `position` column and none is wanted: FIFO order is
//! `enqueued_at ASC, id ASC` (`manager/src/services/run_queue.rs:248`) and
//! `qa_runs_sdk::QueueEntry::queue_position` is computed in memory over the
//! rows a request returned.
//!
//! There is likewise no `execution_ref`. Legacy's queue row carries
//! `workflow_name` because legacy has no run row to hang it on; here `run_id`
//! resolves to a `qa_runs` row that already owns the handle, and a second copy
//! could only disagree with the first. See the migration's module header.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_run_queue")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Queue partition key.
    pub platform_id: Uuid,
    /// The waiting run. Unique per tenant — see `idx_qa_run_queue_tenant_run`.
    pub run_id: Uuid,
    /// Denormalized from the run so the FIFO planner needs no join.
    pub run_kind: String,
    pub source: String,
    /// The run's resolved exclusivity. Named `exclusive` here and
    /// `resolved_exclusive` on the run: DESIGN §3.7's qa-runs table list spells the two columns
    /// differently and they are different tables.
    pub exclusive: bool,
    /// `qa_runs_sdk::QueueState::as_str`, seven values. `cancelled` with two
    /// `l`s — the *run*'s `canceled` has one, deliberately.
    pub state: String,
    pub error: Option<String>,
    /// FIFO sort key and the TTL sweep's clock.
    pub enqueued_at: OffsetDateTime,
    pub dispatched_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
