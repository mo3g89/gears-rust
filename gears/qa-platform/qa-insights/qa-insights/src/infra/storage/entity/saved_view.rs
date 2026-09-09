//! `SeaORM` entity for the `qa_analytics_saved_views` table.
//!
//! # The one entity in this gear with an owner
//!
//! Its `#[secure(...)]` reads `owner_col = "owner_id"` where the other ten read
//! `no_owner`, and that is not cosmetic: a saved view is genuinely owned, the
//! unique index keys on the owner, and `owner_col` is what lets a PEP-compiled
//! `AccessScope` carrying an `owner_id` property narrow a query to the caller's
//! own views. Declaring `no_owner` here would compile, pass every unit test,
//! and silently make one tenant's users able to see each other's saved views
//! wherever policy meant to scope by owner.
//!
//! # `plan_key` is written by the repository, and nothing checks it
//!
//! This is obligation #2 of the migration's module header, and it is the only
//! silent-correctness failure mode in this schema. Legacy's uniqueness is a
//! *functional* index over `COALESCE(plan_id, '')` (`001_initial.sql:194-195`);
//! `SQLite` and `MySQL` do not both support functional indexes, so the
//! coalesced value is materialized here as its own column. It must be written on
//! **every insert and every update**: `""` when `repo_id`/`plan_path` are
//! `None`, otherwise `"<repo_id>/<plan_path>"`. A writer that forgets it gets
//! the `''` column default, which quietly collides a plan-scoped view with the
//! owner's global view of the same name.
//!
//! Task 12 discharged it in exactly one place —
//! `crate::infra::storage::mapper::plan_key`, which both writers *and* the
//! natural-key probe in `saved_views_sea_repo` derive it through, so a probe
//! cannot disagree with what a write would produce. The three tests that go red
//! if any of those call sites stops using it are
//! `a_plan_scoped_and_a_global_view_of_one_name_both_persist_and_both_list`,
//! `updating_a_view_into_a_plan_scope_rewrites_its_plan_key` and
//! `a_plan_scoped_list_returns_only_that_plans_views`.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_analytics_saved_views")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "id",
    owner_col = "owner_id",
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_id: Uuid,
    pub scope: String,
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    /// The materialized `COALESCE(plan_id, '')`. Indexed, never displayed —
    /// `repo_id`/`plan_path` above are the readable representation, and
    /// `qa_insights_sdk::SavedView` therefore has no field for this one. See this
    /// module's header for the obligation it carries and where it is discharged.
    pub plan_key: String,
    pub name: String,
    /// Opaque to this gear: legacy types it `serde_json::Value` and never
    /// inspects it, binding or returning it whole in all six statements that
    /// touch the column. `qa_insights_sdk::SavedView::query_json` is a
    /// `String` holding the same document verbatim, so Task 12's mapper is the
    /// only place the two spellings meet.
    pub query_json: Json,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
