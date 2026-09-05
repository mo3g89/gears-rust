//! `SeaORM` entity for the `qa_products` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_products")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    /// Durable short code the product is known by (legacy `Product::key`).
    /// Stored as `product_key` because `key` is a `MySQL` reserved word; the
    /// SDK model exposes it as `key`.
    pub product_key: String,
    pub description: String,
    pub folder: Option<String>,
    /// Full GTS instance id of the product plugin bound to this product —
    /// the key the resolver hands to `ClientScope::gts_id` unchanged, not the
    /// instance segment (see `m20260903_000003_product_plugin_instance`).
    ///
    /// Non-optional since Task 20a: `m20260903_000004_plugin_instance_id_not_null`
    /// tightened the column and the model followed in the same commit. It was
    /// an `Option` through the expand half of the expand/contract pair, and
    /// this comment still said so afterwards (review finding IMPORTANT-4).
    pub plugin_instance_id: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
