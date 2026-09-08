use async_trait::async_trait;
use qa_catalog_sdk::{NewProduct, Product, ProductUpdate};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert, secure_update_with_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::ProductsRepository;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::product::{
    self, ActiveModel as ProductAM, Column as ProductColumn, Entity as ProductEntity,
};
use crate::infra::storage::mapper::product_to_sdk;

/// ORM-based implementation of the `ProductsRepository` trait.
#[derive(Clone, Default)]
pub struct OrmProductsRepository;

#[async_trait]
impl ProductsRepository for OrmProductsRepository {
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Product>, DomainError> {
        let found = ProductEntity::find()
            .filter(sea_orm::Condition::all().add(ProductColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        Ok(found.map(product_to_sdk))
    }

    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Product>, DomainError> {
        let rows = ProductEntity::find()
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(product_to_sdk).collect())
    }

    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewProduct,
    ) -> Result<Product, DomainError> {
        let now = OffsetDateTime::now_utc();
        // Kept for the unique-violation error, which consumes `name`.
        let conflict_name = new.name.clone();

        let am = ProductAM {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            name: ActiveValue::Set(new.name),
            product_key: ActiveValue::Set(new.key),
            description: ActiveValue::Set(new.description),
            folder: ActiveValue::Set(new.folder),
            plugin_instance_id: ActiveValue::Set(new.plugin_instance_id),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        // The INSERTED model, not a locally built struct — see
        // `OrmTestReposRepository::create`.
        match secure_insert::<ProductEntity>(am, scope, runner).await {
            Ok(model) => Ok(product_to_sdk(model)),
            Err(e) if e.is_unique_violation() => Err(DomainError::ProductNameExists {
                name: conflict_name,
            }),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        update: ProductUpdate,
    ) -> Result<Option<Product>, DomainError> {
        let existing = ProductEntity::find()
            .filter(sea_orm::Condition::all().add(ProductColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        let Some(existing) = existing else {
            return Ok(None);
        };

        // Kept for the unique-violation error, which consumes `name`.
        let conflict_name = update.name.clone();

        let am: ProductAM = product::ActiveModel {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            name: ActiveValue::Set(update.name),
            product_key: ActiveValue::Set(update.key),
            description: ActiveValue::Set(update.description),
            folder: ActiveValue::Set(update.folder),
            // `None` leaves the stored binding alone -- the one field on
            // `ProductUpdate` that is not full-replace. See its own doc for
            // the measurement: the shipped UI cannot send this field, so a
            // full replace silently unbound a product's plugin on any
            // description edit.
            plugin_instance_id: match update.plugin_instance_id {
                Some(instance_id) => ActiveValue::Set(instance_id),
                None => ActiveValue::Unchanged(existing.plugin_instance_id.clone()),
            },
            created_at: ActiveValue::Unchanged(existing.created_at),
            updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
        };

        match secure_update_with_scope::<ProductEntity>(am, scope, id, runner).await {
            Ok(model) => Ok(Some(product_to_sdk(model))),
            Err(e) if e.is_unique_violation() => Err(DomainError::ProductNameExists {
                name: conflict_name,
            }),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let result = ProductEntity::delete_many()
            .filter(sea_orm::Condition::all().add(ProductColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        Ok(result.rows_affected > 0)
    }
}
