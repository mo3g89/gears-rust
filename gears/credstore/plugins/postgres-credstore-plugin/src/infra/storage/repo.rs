//! The value-store repository — the only boundary secret bytes cross.
//!
//! `SecretValue` is deliberately non-`Clone`, non-`Serialize`/`Deserialize`
//! and zeroizes on drop (`credstore-sdk/src/models.rs`), so the conversion to
//! and from raw bytes happens here and nowhere else: the caller hands this
//! module a `&[u8]` borrowed from a live `SecretValue` and receives a
//! `Vec<u8>` it immediately wraps back into one.
//!
//! Every query is tenant-clamped by `SecureORM`
//! (`AccessScope::for_tenant(tenant_id)`), so a missing hand-written
//! `tenant_id` predicate cannot leak another tenant's row. The key class is
//! then selected explicitly: `owner_id = Some(o)` -> `owner_id = o`,
//! `owner_id = None` -> `owner_id IS NULL`.

use std::sync::Arc;

use credstore_sdk::{OwnerId, SecretRef, TenantId};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::entity;
use super::error::StoreError;

/// The plugin's DB entrypoint type.
pub type ValueDbProvider = DBProvider<StoreError>;

/// Repository over `credstore_plugin_values`.
///
/// `Clone` is cheap (an `Arc` bump): [`Service`](crate::domain::Service) needs
/// an owned handle it can move into a `'static`-bound transaction closure,
/// since every method here now takes an explicit runner instead of holding
/// one implicitly.
#[derive(Clone)]
pub struct ValueRepo {
    db: Arc<ValueDbProvider>,
}

/// Predicate selecting one key class for one reference.
///
/// The `IS NULL` branch is what makes the tenant key class addressable at all,
/// and is the reason uniqueness needs partial indexes (see
/// `migrations::m0001_initial_schema`).
fn key_filter(key: &SecretRef, owner_id: Option<&OwnerId>) -> Condition {
    let base = Condition::all().add(entity::Column::SecretRef.eq(key.as_ref()));
    match owner_id {
        Some(owner) => base.add(entity::Column::OwnerId.eq(owner.0)),
        None => base.add(entity::Column::OwnerId.is_null()),
    }
}

impl ValueRepo {
    /// Wrap a DB provider.
    #[must_use]
    pub fn new(db: Arc<ValueDbProvider>) -> Self {
        Self { db }
    }

    /// Clone of the provider this repo was constructed with.
    ///
    /// Every method below now takes an explicit `runner: &C where C: DBRunner`
    /// instead of reaching for a connection or a transaction internally
    /// (review finding #14, `TOOLKIT-DB-001`) — that was exactly what stopped
    /// a caller from composing two of them into one transaction. A caller
    /// that has no runner of its own yet (this crate's [`Service`](crate::domain::Service))
    /// gets one from here, once, at construction time — the same way every
    /// other service in this workspace holds its own `DBProvider` alongside a
    /// stateless repository.
    #[must_use]
    pub fn provider(&self) -> Arc<ValueDbProvider> {
        Arc::clone(&self.db)
    }

    /// Read the stored bytes for one key, or `None` if no row exists.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the scoped query fails.
    pub async fn find<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let row = entity::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(tenant_id.0))
            .filter(key_filter(key, owner_id))
            .one(runner)
            .await?;
        Ok(row.map(|m| m.secret_value))
    }

    /// Insert or overwrite the value for one key.
    ///
    /// Unconditional overwrite, matching the in-memory plugin's plain
    /// `HashMap::insert`: write preconditions and CAS live in the credstore
    /// *gear* (`WritePrecondition`), never in a plugin.
    ///
    /// `UPDATE`-then-`INSERT` rather than `INSERT ... ON CONFLICT`: `ON
    /// CONFLICT` must infer a unique index from the target column list, and a
    /// **partial** index is only inferable when the statement repeats its
    /// `WHERE` predicate — which `SeaORM`'s `OnConflict` builder cannot emit.
    ///
    /// The two statements run against whatever `runner` the caller supplies,
    /// so whether they are atomic with each other is the caller's call: pass
    /// a `DbTx` (e.g. from `DBProvider::transaction`) for that, same as any
    /// other multi-statement sequence in this subsystem. A bare `DbConn`
    /// still leaves the pair safe against silent duplication -- the partial
    /// unique indexes turn a concurrent first-write race into an error
    /// instead of a second row.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the update or the insert fails. Two
    /// concurrent first-writes of the same key can make one of them lose the
    /// insert race and surface a unique-violation here; the gear's write saga
    /// retries, and the partial unique indexes are what turn that race into
    /// an error instead of a silent duplicate row.
    pub async fn upsert<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
        value: &[u8],
    ) -> Result<(), StoreError> {
        let scope = AccessScope::for_tenant(tenant_id.0);
        let filter = key_filter(key, owner_id);
        let tenant = tenant_id.0;
        let owner = owner_id.map(|o| o.0);
        let reference = key.as_ref().to_owned();
        let bytes = value.to_vec();
        let now = OffsetDateTime::now_utc();

        let updated = entity::Entity::update_many()
            .col_expr(entity::Column::SecretValue, Expr::value(bytes.clone()))
            .col_expr(entity::Column::UpdatedAt, Expr::value(now))
            .filter(filter)
            .secure()
            .scope_with(&scope)
            .exec(runner)
            .await?;
        if updated.rows_affected == 0 {
            insert_row(runner, &scope, tenant, owner, reference, bytes, now).await?;
        }
        Ok(())
    }

    /// Insert the value for one key **only if no row exists yet**.
    ///
    /// Returns `true` when a row was written. Used for config seeding: unlike
    /// the in-memory plugin, which rebuilds its maps from configuration on
    /// every boot, this backend must not clobber a value a client rotated at
    /// runtime just because the same reference still appears in the YAML.
    ///
    /// The probe and the insert run against whatever `runner` the caller
    /// supplies -- pass a `DbTx` if the absence check and the insert must be
    /// atomic against a concurrent writer; [`Service::seed`](crate::domain::Service::seed)
    /// does exactly that, one transaction per seed.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the probe or the insert fails.
    pub async fn insert_if_absent<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
        value: &[u8],
    ) -> Result<bool, StoreError> {
        let scope = AccessScope::for_tenant(tenant_id.0);
        let filter = key_filter(key, owner_id);
        let tenant = tenant_id.0;
        let owner = owner_id.map(|o| o.0);
        let reference = key.as_ref().to_owned();
        let bytes = value.to_vec();

        let existing = entity::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(filter)
            .one(runner)
            .await?;
        if existing.is_some() {
            return Ok(false);
        }
        let now = OffsetDateTime::now_utc();
        insert_row(runner, &scope, tenant, owner, reference, bytes, now).await?;
        Ok(true)
    }

    /// Delete the row for one key. A miss is a no-op — the gear treats a
    /// missing backend value as success.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the scoped delete fails.
    pub async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), StoreError> {
        entity::Entity::delete_many()
            .filter(key_filter(key, owner_id))
            .secure()
            .scope_with(&AccessScope::for_tenant(tenant_id.0))
            .exec(runner)
            .await?;
        Ok(())
    }
}

/// Insert one row. Shared by `upsert` and `insert_if_absent`.
///
/// `scope_unchecked`: an `INSERT` has no existing row for the scope clamp to
/// filter on — the same reasoning the credstore gear's `insert_provisioning`
/// records. The tenant written is the one the SPI was called with.
async fn insert_row<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    tenant_id: Uuid,
    owner_id: Option<Uuid>,
    reference: String,
    value: Vec<u8>,
    now: OffsetDateTime,
) -> Result<(), StoreError> {
    let am = entity::ActiveModel {
        id: ActiveValue::Set(Uuid::new_v4()),
        tenant_id: ActiveValue::Set(tenant_id),
        owner_id: ActiveValue::Set(owner_id),
        secret_ref: ActiveValue::Set(reference),
        secret_value: ActiveValue::Set(value),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
    };
    entity::Entity::insert(am)
        .secure()
        .scope_unchecked(scope)?
        .exec(runner)
        .await?;
    Ok(())
}
