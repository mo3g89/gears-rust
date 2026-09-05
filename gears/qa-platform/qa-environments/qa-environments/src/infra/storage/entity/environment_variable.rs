//! `SeaORM` entity for the `qa_environment_variables` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_environment_variables")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// The environment this variable belongs to.
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
    /// `SELECT environment_id FROM qa_environment_variables` against a column
    /// that does not exist.
    #[sea_orm(column_name = "platform_id")]
    pub environment_id: Uuid,
    pub name: String,
    pub value: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
