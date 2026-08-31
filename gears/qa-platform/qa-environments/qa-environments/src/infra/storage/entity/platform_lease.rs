//! `SeaORM` entity for the `qa_platform_leases` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_platform_leases")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "platform_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub platform_id: Uuid,
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
