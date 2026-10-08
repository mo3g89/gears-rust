//! `SeaORM` entity for `qa_run_projection_locks`: one row per projected run,
//! upserted first by `ResultsRepository::upsert_run_results` so two writers of
//! one run serialize on it (`m20261007_000009_run_projection_locks`).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_run_projection_locks")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub run_id: Uuid,
    /// When the run's results were last written. Informational; nothing reads it.
    pub projected_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
