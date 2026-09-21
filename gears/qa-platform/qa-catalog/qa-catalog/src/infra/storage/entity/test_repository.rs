//! `SeaORM` entity for the `qa_test_repositories` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_test_repositories")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Owning product. Required — repo ownership is how a discovered plan is
    /// attributed to a product (legacy `enrich_plan_products`).
    pub product_id: Uuid,
    pub name: String,
    pub url: String,
    pub default_branch: String,
    /// Subdirectory within the repo that contains test content ("" = root).
    pub content_root: String,
    /// credstore reference for the access credential (SSH key or token). `None` = public repo.
    pub credential_ref: Option<String>,
    pub last_synced_at: Option<OffsetDateTime>,
    /// Commit id the last successful sync materialized, or `None` when the
    /// repository has never synced (or last synced before
    /// `m20260921_000003_repo_head_commit` existed). This is the content
    /// revision — `last_synced_at` is only when the attempt happened.
    pub head_commit: Option<String>,
    pub sync_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
