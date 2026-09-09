//! `SeaORM` entity for the `qa_notification_config` table.
//!
//! One row per tenant. Legacy `NotificationsConfig`
//! (`manager/src/models.rs:1338-1362`), fifteen fields as fifteen columns, with
//! every default legacy's `Default` verbatim (`:1364-1384`).
//!
//! Note what has **no** toggle: the run-queue `expired` event. Legacy gates
//! only `queued` behind [`Model::run_queue_queued_slack_enabled`], because
//! `expired` is "the one that stops a run disappearing silently". Its absence
//! from this table is the port of that.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_notification_config")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "seven independent routing gates, one column each, ported \
              one-for-one from legacy's NotificationsConfig \
              (manager/src/models.rs:1338-1362). Grouping them into sub-structs \
              would stop this being a SeaORM entity of that table. \
              `qa_insights_sdk::NotificationConfig` carries the same allow for \
              the same reason."
)]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// **A credential-store reference, never the URL.** The second of this
    /// schema's two deliberate divergences from legacy, which stores the
    /// incoming-webhook URL verbatim. Possession of a Slack webhook URL *is*
    /// the authorization to post as the app, exactly like the JIRA token, so it
    /// gets the same treatment — and the same rename-not-retype. See
    /// [`super::jira_config::Model::api_token_credstore_ref`].
    pub slack_webhook_credstore_ref: String,
    pub slack_channel: String,
    pub manager_ui_base_url: String,
    pub slack_enabled: bool,
    pub notify_on_failure: bool,
    pub notify_on_success: bool,
    pub notify_on_schedule_completion: bool,
    pub scheduled_run_slack_enabled: bool,
    /// Six Block Kit templates (pending, in progress, succeeded, failed, error,
    /// skipped), each with seven fields. A JSON document rather than 42
    /// columns: the set is written and read whole and nothing filters on it.
    /// `qa_insights_sdk::ScheduledRunSlackTemplates` is the typed shape Task
    /// 12's mapper produces.
    pub scheduled_run_slack_templates: Json,
    pub run_queue_queued_slack_enabled: bool,
    pub email_smtp_host: String,
    pub email_smtp_port: i32,
    pub email_from: String,
    /// One string, not a list: legacy stores the operator's recipient line as
    /// typed and the mailer splits it. Kept as stored so a settings round-trip
    /// cannot reformat it.
    pub email_recipients: String,
    pub email_enabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
