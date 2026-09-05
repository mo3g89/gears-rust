//! `SeaORM` entity for the `qa_jira_config` table.
//!
//! One row per tenant. Legacy `JiraConfig` (`manager/src/models.rs:678-685`),
//! which lives as a JSON bag in an untyped `settings` table; D6 replaces the
//! bag with typed columns and keeps the same GET/PUT surface.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_jira_config")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub url: String,
    pub project_key: String,
    pub email: String,
    /// **A credential-store reference, never the token.** One of the two
    /// deliberate divergences from legacy in this schema — legacy stores the
    /// bearer token itself. The rename is the guard: a `String` holding an
    /// actual token cannot be assigned to a field called
    /// `api_token_credstore_ref` by accident. Obligation #3 of the migration's
    /// header: the `GET` surface returns the reference and never the material.
    pub api_token_credstore_ref: String,
    pub issue_type: Option<String>,
    /// A missing or disabled config short-circuits the poller *silently*
    /// (`manager/src/services/jira_poller.rs:40-43`).
    pub enabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
