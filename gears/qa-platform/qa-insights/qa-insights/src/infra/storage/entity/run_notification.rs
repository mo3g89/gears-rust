//! `SeaORM` entity for the `qa_run_notifications` table.
//!
//! Notification dedupe claims, and **the only entity here with no
//! `qa-insights-sdk` contract type** — deliberately. Nothing outside this gear
//! ever reads a dedupe row; publishing a `RunNotification` model would turn an
//! internal idempotency detail into contract.
//!
//! # The row *is* the mechanism, not a record of it
//!
//! `idx_qa_run_notifications_claim` — `(tenant_id, run_id, notification_kind,
//! event_type)` — is what makes a notification send-once. An insert that
//! violates it is the answer "someone else already sent this", which is why
//! `NotifyRepository::claim_notification` returns `bool` and why obligation #4
//! of the migration's header requires the unique violation to be read as
//! "already sent" rather than surfaced as a `DomainError::Database`.
//! Legacy's equivalent is `INSERT ... ON CONFLICT DO NOTHING` plus
//! `rows_affected() > 0` (`manager/src/services/notifications.rs:548-562`).
//!
//! `crate::infra::storage::notify_sea_repo::OrmNotifyRepository::claim_notification`
//! discharges it: `SeaORM` reports a `do_nothing` insert that changed nothing as
//! `DbErr::RecordNotInserted`, and that arm answers `Ok(false)`. Forcing it to
//! `Ok(true)` turns `the_first_claim_on_a_slot_wins_and_the_second_loses` red.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_run_notifications")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub run_id: Uuid,
    pub notification_kind: String,
    pub event_type: String,
    /// When the claim was taken. Legacy has no such column — its table is
    /// `(workflow_name, notification_kind, event_type)` plus a `created_at` —
    /// and it exists here so an operator can tell a stale claim from a fresh
    /// one without joining the audit log.
    pub sent_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
