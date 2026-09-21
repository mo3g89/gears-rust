//! `SeaORM` entity for the `qa_environment_leases` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_environment_leases")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "environment_id",
    no_owner,
    no_type
)]
pub struct Model {
    /// The environment this lease is for — 1:1 with it, which is why it is the
    /// primary key rather than a surrogate `id` (see
    /// `m20260812_000001_initial`'s comment on the table).
    ///
    /// Field and physical column are both `environment_id`; no
    /// `#[sea_orm(column_name)]` pin is needed or present —
    /// `m20260903_000010_rename_platform_tables` (folded into `migrations::m20260812_000001_initial` by the docs squash), once cited here as the
    /// reason one was, was removed by the migration squash.
    ///
    /// `resource_col` above names the *`SeaORM` column variant*
    /// (`Column::EnvironmentId`), not the physical column, so it renames with
    /// the field.
    #[sea_orm(primary_key, auto_increment = false)]
    pub environment_id: Uuid,
    pub tenant_id: Uuid,
    /// "free" | "parallel" | "exclusive" — denormalized from holders for indexing/display.
    pub mode: String,
    /// JSON array of holder run UUIDs.
    pub holders: Json,
    /// Optimistic-concurrency version; every write does WHERE version = `read_version`.
    pub version: i64,
    pub updated_at: OffsetDateTime,
    /// The instant this environment last transitioned **to free**, or `None`
    /// when no such transition has been recorded (never held, or last freed
    /// before `m20260921_000002_lease_freed_at` ran).
    ///
    /// Written only by a compare-and-swap whose new state is
    /// [`qa_environments_sdk::LeaseState::Free`], and **never cleared by an
    /// acquisition** — so on a held row it still names the free transition the
    /// current holder consumed, and on a free row it names when it became free.
    /// `updated_at` cannot substitute: it is the last write of any kind.
    ///
    /// This is the start endpoint of `cpt-cf-qa-nfr-dispatch-latency`; the
    /// migration that adds it carries the full argument for why the anchor is
    /// a column rather than an event or a metric.
    pub freed_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
