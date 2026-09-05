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
    /// **The physical column is still `platform_id`, and this attribute is what
    /// keeps it that way — do not delete it as redundant.** The aggregate was
    /// renamed `TargetPlatform` → `Environment` (spec D5) and this field
    /// follows, but `m20260903_000010_rename_platform_tables` renames only the
    /// three *tables*: renaming a column is a behaviour change, three further
    /// gears own `platform_id` columns with no migration scheduled for them,
    /// and the plan defers column rewrites to the later expand/contract
    /// migrations that already touch these columns. Without `column_name` here
    /// `SeaORM` would derive the column from the field and emit
    /// `SELECT environment_id FROM qa_environment_leases` against a column that
    /// does not exist — and this one is the primary key, so every read and
    /// every optimistic-concurrency write would fail.
    ///
    /// `resource_col` above names the *`SeaORM` column variant*
    /// (`Column::EnvironmentId`), not the physical column, so it renames with
    /// the field.
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_name = "platform_id")]
    pub environment_id: Uuid,
    pub tenant_id: Uuid,
    /// "free" | "parallel" | "exclusive" — denormalized from holders for indexing/display.
    pub mode: String,
    /// JSON array of holder run UUIDs.
    pub holders: Json,
    /// Optimistic-concurrency version; every write does WHERE version = `read_version`.
    pub version: i64,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
