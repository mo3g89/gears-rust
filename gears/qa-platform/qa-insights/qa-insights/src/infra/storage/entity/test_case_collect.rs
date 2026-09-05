//! `SeaORM` entity for the `qa_test_case_collect` table.
//!
//! Exact expected test-case counts per file, produced by the collect job
//! (`pytest --collect-only`, parametrize expanded).
//!
//! **The natural key is `(tenant_id, repo_id, branch, test_file)`, not `id`.**
//! Legacy's primary key is the composite `(repo_id, branch, test_file)`
//! (`001_initial.sql:278`); the platform's mandatory `id UUID` demoted it to
//! `idx_qa_test_case_collect_target`, which is the conflict target the collect
//! upsert runs against. A writer that generates a fresh `id` and inserts
//! without targeting that index duplicates the count instead of replacing it.
//! `qa_insights_sdk::CollectCount` deliberately carries no `id` for the same
//! reason.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_test_case_collect")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub repo_id: Uuid,
    pub branch: String,
    pub test_file: String,
    pub case_count: i32,
    /// When the collect job produced this count — distinct from `updated_at`,
    /// which moves on any write. Legacy has only this one (`:277`).
    pub collected_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
