//! `SeaORM` entity for the `qa_schedules` table.
//!
//! Every enum-shaped column is a `String` here, not a domain type, for the
//! reason `entity::run` states: the decode belongs in the mapper so it can
//! **fail closed** and name the offending row, which a `#[sea_orm(enum)]`
//! column cannot do.
//!
//! That applies with particular force to [`Model::exclusive_choice`], whose
//! contract type is not an enum at all but an `Option<bool>` — see that field.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_schedules")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Unique within the tenant — see `idx_qa_schedules_tenant_name`.
    pub name: String,
    /// `qa_runs_sdk::RunKind::as_str` — `plan` | `test` | `custom_plan` |
    /// `collect`. Says which of the five `target_*` columns below are
    /// meaningful, exactly as on `qa_runs`, and decoded by the same codec.
    pub run_kind: String,
    pub target_repo_id: Option<Uuid>,
    pub target_path: Option<String>,
    pub target_test_file: Option<String>,
    pub target_custom_plan_id: Option<Uuid>,
    /// The collect report URL, present only for a `collect` target. Carried
    /// because this table shares the run's target codec, not because a collect
    /// schedule is a feature — `m20260818_000006_collect_target` argues both
    /// halves.
    pub target_collect_url: Option<String>,
    /// The platform every fire targets. Ownership is verified at launch through
    /// a tenant-scoped qa-environments client and nowhere else — see the
    /// migration's column comment.
    pub platform_id: Option<Uuid>,
    /// The branch a fire resolves against; `None` defers to the repository's
    /// default branch.
    pub branch: Option<String>,
    /// Five-field cron expression, stored verbatim.
    pub cron: String,
    /// The tri-state exclusivity choice, as `true` | `false` | `auto`.
    ///
    /// **A `String`, where `qa_runs_sdk::Schedule::exclusive_choice` is an
    /// `Option<bool>`** — `infra::storage::mapper` bridges the two and refuses a
    /// fourth spelling. A `bool` column could not carry the third state, and a
    /// nullable `bool` would make "inherit" and "never written" the same value,
    /// which is the conflation the whole tri-state exists to prevent.
    pub exclusive_choice: String,
    pub enabled: bool,
    /// JSON array of tag strings.
    pub include_tags: Json,
    /// JSON array of tag strings.
    pub exclude_tags: Json,
    /// JSON array of `{name, value}` launch parameters, the same storage shape
    /// as `qa_runs.parameters`.
    pub parameters: Json,
    /// Whether this schedule's runs notify Slack at all. The master switch of
    /// the three ported from legacy's `CronWorkflow` annotations
    /// (`manager/src/routes/schedules.rs::api_update_notifications`).
    ///
    /// **qa-runs owns these three columns and sends nothing.** The sending lives
    /// in qa-insights (D9, Task 36), which reads them over the SDK when a
    /// scheduled run changes status. A schedule is a qa-runs aggregate, so a
    /// notification setting on it is a field of that aggregate, not a separate
    /// entity in another gear.
    pub slack_notifications_enabled: bool,
    /// Channel override, `None` meaning "the deployment-wide default channel".
    /// Nullable because `None` and `Some("")` must not both be spellable for the
    /// same thing.
    pub slack_channel: Option<String>,
    /// JSON array of event-name strings, from legacy's
    /// `ScheduledRunNotificationEvent` vocabulary — `pending`, `in_progress`,
    /// `succeeded`, `failed`, `error`, `skipped`
    /// (`manager/src/models.rs:948-959`, `#[serde(rename_all = "snake_case")]`).
    ///
    /// **`NOT NULL` with a `'[]'` default, never nullable** — one strategy, and
    /// `m20260818_000007_schedule_notifications` says why. There is therefore no
    /// `NULL`-means-empty case anywhere in the mapper.
    pub slack_notification_events: Json,
    /// The latest due time this schedule has fired for, and the cron
    /// evaluator's cursor. `None` means it has never fired.
    pub last_fired_tick: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
