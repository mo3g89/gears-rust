//! `SeaORM` entity for the `qa_ingest_watermarks` table.
//!
//! **New in this port; no legacy original.** The reconcile sweep re-reads
//! qa-runs on its own cadence rather than sharing legacy's process and
//! transaction, so it needs a durable mark of how far it has already gotten —
//! held in memory, that mark would restart at zero on every deploy and the
//! sweep would re-scan every run this gear has ever known about. Both recovery
//! marks live here, one row per tenant, which is the failure this table exists
//! to prevent (design §4.4, Task 15).
//!
//! `WatermarkRepository` is the domain-side view of this row.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_ingest_watermarks")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// How far the reconcile poller has read. `None` means **never
    /// reconciled**, and the first pass uses the configured lookback window
    /// instead. Distinct from the epoch, which would mean "reconciled up to
    /// 1970" and would make the first pass read everything.
    pub last_reconciled_finished_at: Option<OffsetDateTime>,
    /// When the stale-in-progress sweep last ran. `None` means never, with the
    /// same distinction from the epoch as the field above.
    pub last_swept_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
