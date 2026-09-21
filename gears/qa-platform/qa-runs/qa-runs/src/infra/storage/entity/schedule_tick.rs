//! `SeaORM` entity for the `qa_schedule_ticks` table — the exactly-once claim.
//!
//! **There is no `updated_at`**, and that is a deliberate departure from the
//! house-style standard column set most other tables carry. The migration's
//! module header carries the argument and names the three departures that
//! precede it; in short, a claim row is immutable once won, and its only later
//! writes — `run_id` and `error` — are made once by the instance that won it.
//!
//! Nothing here decodes: every column is a scalar or an opaque string, so there
//! is no mapper entry for this entity and no `CorruptState` it can produce. The
//! repository returns bare ids rather than a record type, because the only
//! consumers are the claim (which wants "did I win?") and the outcome write
//! (which wants "which row?").

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_schedule_ticks")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// The schedule this tick claims. The foreign key is tenant-blind; see the
    /// migration's header for the obligation that places on every writer.
    pub schedule_id: Uuid,
    /// The due time being claimed. `(tenant_id, schedule_id, due_at)` is unique,
    /// and that constraint *is* the exactly-once guarantee.
    pub due_at: OffsetDateTime,
    /// Which instance won the claim. Diagnostic only — no decision reads it.
    pub claimed_by: String,
    pub claimed_at: OffsetDateTime,
    /// The run this tick produced. `None` means the launch has not completed
    /// yet, or failed; the claim stays durable either way, because a retried
    /// launch would violate exactly-once for a destructive run.
    pub run_id: Option<Uuid>,
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
