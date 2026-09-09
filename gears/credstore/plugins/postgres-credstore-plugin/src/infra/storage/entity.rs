//! `SeaORM` entity for the `credstore_plugin_values` table.
//!
//! One row per stored value in one of the two runtime-written key classes:
//!
//! | `owner_id` | key class | unique on |
//! |---|---|---|
//! | `NULL`     | `tenant`  | `(tenant_id, secret_ref)` |
//! | `NOT NULL` | `private` | `(tenant_id, owner_id, secret_ref)` |
//!
//! `owner_id` is nullable rather than a nil-UUID sentinel so the two classes
//! are structurally distinct, which is why the uniqueness above is enforced by
//! **two partial** unique indexes and not one composite index — see
//! `migrations::m0001_initial_schema`.
//!
//! Tenant-scoped via `Scopable` (`tenant_col = "tenant_id"`): every query in
//! `repo.rs` runs through `SecureORM` with `AccessScope::for_tenant(...)`, so
//! the tenant clamp is applied by the ORM rather than by hand. `no_owner`:
//! `owner_id` here is the *secret's* owner within the plugin's key space, not
//! a PEP `owner_id` property, and this plugin filters it explicitly (including
//! the `IS NULL` case, which a scope predicate cannot express).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db::secure::Scopable;
use uuid::Uuid;

#[derive(Clone, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "credstore_plugin_values")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// `Some` selects the private key class, `None` the tenant key class —
    /// exactly the `owner_id: Option<&OwnerId>` argument of the plugin SPI.
    pub owner_id: Option<Uuid>,
    pub secret_ref: String,
    /// The secret's raw bytes, stored **unencrypted** (see the crate docs).
    pub secret_value: Vec<u8>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Hand-written so `secret_value` can never be printed.
///
/// The derived `Debug` would render the secret's bytes, and a single
/// `tracing::debug!(?row)` anywhere would then put a secret in a log file. All
/// fields are listed (the workspace denies `clippy::missing_fields_in_debug`);
/// only the value is replaced.
impl core::fmt::Debug for Model {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Model")
            .field("id", &self.id)
            .field("tenant_id", &self.tenant_id)
            .field("owner_id", &self.owner_id)
            .field("secret_ref", &self.secret_ref)
            .field("secret_value", &"[REDACTED]")
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
