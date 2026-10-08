//! `SeaORM` entity for the `qa_ingest_watermarks` table.
//!
//! **New in this port; no legacy original.** The reconcile sweep re-reads
//! qa-runs on its own cadence rather than sharing legacy's process and
//! transaction, so it needs a durable mark of how far it has already gotten —
//! held in memory, that mark would restart at zero on every deploy and the
//! sweep would re-scan every run this gear has ever known about. Both recovery
//! marks live here, one row per tenant, which is the failure this table exists
//! to prevent (Task 15).
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
    /// `finished_at` of the last run the previous sweep **fully consumed**,
    /// when that sweep ended short of catching up. Half of the keyset resume
    /// point; see [`Self::sweep_cursor_floor`] for what makes it a
    /// *within-window* position rather than a second watermark, and
    /// `m20260929_000006_ingest_watermarks_sweep_cursor` for the whole account.
    pub sweep_cursor_at: Option<OffsetDateTime>,
    /// The id of that same run. The other half of the `(finished_at, id)` key
    /// `qa_runs_sdk::FinishedRunCursor` rides on — the walk's own ordering, not
    /// a second notion of position.
    pub sweep_cursor_run_id: Option<Uuid>,
    /// The floor (`mark - lookback`) the pass that wrote the cursor was walking
    /// from.
    ///
    /// **This is what keeps the cursor from becoming a second mark.** The
    /// cursor vouches for `[sweep_cursor_floor, sweep_cursor_at]`, and a pass
    /// honours it only when its own floor lies inside that range; otherwise it
    /// is discarded in favour of the ordinary `mark - lookback` derivation, and
    /// a pass that catches up erases it. The rule lives on
    /// `domain::service::reconcile`'s `first_page_of` (it was an equality test
    /// on this column until finding #122's residual A). The comparison is made
    /// in Rust rather than in SQL on purpose — see the migration's header.
    pub sweep_cursor_floor: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
