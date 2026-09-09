//! `SeaORM` entity for the `qa_jira_poller_config` table.
//!
//! One row per tenant. Legacy `JiraPollerConfig` (`manager/src/models.rs:1417-1420`),
//! and both column defaults are legacy's `Default` verbatim (`:1422-1429`).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_jira_poller_config")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// `i64` because the column is `BIGINT`; `qa_insights_sdk::JiraPollerConfig`
    /// types it `u64`, so somewhere a mapper owns that seam. **It is not Task
    /// 12's** — corrected 2026-08-20, having written that task's `mapper.rs`.
    /// `JiraRepository` has five bug methods and no settings methods, so nothing
    /// reads this table yet and a conversion for it would have been dead code
    /// with no call site. Task 32, which builds the JIRA settings surface, owns
    /// both the reader and the widening.
    ///
    /// Legacy clamps with `.max(1)` and the loop sleeps first
    /// (`manager/src/services/jira_poller.rs:26-27`) — the clamp belongs in the
    /// domain, not here and not in the DDL.
    pub poll_interval_seconds: i64,
    /// The global gate on relaunching a run when a bug resolves. Legacy needs
    /// *both* this and a new build (`jira_poller.rs:61-70`, D8). Turning it off
    /// does not stop bugs closing — see
    /// [`super::jira_bug::Model::resolved_at`].
    pub auto_rerun_on_resolve: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
