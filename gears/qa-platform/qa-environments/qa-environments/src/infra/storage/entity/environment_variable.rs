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
    /// The environment this variable belongs to. Field and physical column
    /// are both `environment_id`; no `#[sea_orm(column_name)]` pin is needed
    /// or present — `m20260903_000010_rename_platform_tables` (folded into `migrations::m20260812_000001_initial` by the docs squash), once cited
    /// here as the reason one was, was removed by the migration squash.
    pub environment_id: Uuid,
    pub name: String,
    pub value: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
