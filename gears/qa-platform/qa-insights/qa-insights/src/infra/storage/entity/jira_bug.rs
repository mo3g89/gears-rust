//! `SeaORM` entity for the `qa_jira_bugs` table.
//!
//! This gear's own bug registry — distinct from `qa_test_results.jira_key`,
//! which is whatever bug reference the *runner* reported on a file.
//!
//! **There is no `auto_rerun` column**, and its absence is checked rather than
//! assumed: legacy's `CREATE TABLE` stops at `resolved_at`
//! (`001_initial.sql:88`) and `manager/migrations/` holds exactly one file.
//! Auto-rerun is the poller's global switch,
//! [`super::jira_poller_config::Model::auto_rerun_on_resolve`].

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_jira_bugs")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub jira_key: String,
    pub test_name: String,
    pub repo_id: Uuid,
    pub plan_path: String,
    pub app_version: Option<String>,
    pub platform_id: Option<Uuid>,
    /// Free JIRA workflow text, not a closed set — legacy's default is
    /// `'Open'` and `resolve_bug` writes `'Resolved'`
    /// (`manager/src/services/jira.rs:243-251`), but a JIRA instance may report
    /// anything.
    pub status: String,
    pub summary: String,
    /// Set when the poller observes a resolved transition — **recorded whether
    /// or not auto-rerun is on**. `resolve_bug` runs at
    /// `manager/src/services/jira_poller.rs:59`, *before* the
    /// `auto_rerun_on_resolve` gate at `:61-63`.
    pub resolved_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
