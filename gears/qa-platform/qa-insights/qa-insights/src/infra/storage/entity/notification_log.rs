//! `SeaORM` entity for the `qa_notification_log` table.
//!
//! The notification audit trail. Egress failures are recorded here and **never
//! propagated to a caller** (design §4.8), which makes this table the only
//! place an operator can see that Slack or SMTP is broken. Legacy is the same:
//! `log_notification` swallows its own insert error into a `tracing::warn!`
//! (`manager/src/services/notifications.rs:594-617`).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_notification_log")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// `None` for an attempt that belongs to no run — a settings `/test` send,
    /// which legacy records as the empty string in its `workflow_name TEXT NOT
    /// NULL` column (`001_initial.sql:208`).
    pub run_id: Option<Uuid>,
    pub channel: String,
    pub event_type: String,
    pub outcome: String,
    /// Failure detail; `""` on success, never `NULL` (`001_initial.sql:212`).
    pub detail: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
