use async_trait::async_trait;
use qa_catalog_sdk::SshKey;
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::SshKeysRepository;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::ssh_key::{
    ActiveModel as SshKeyAM, Column as SshKeyColumn, Entity as SshKeyEntity,
};
use crate::infra::storage::mapper::ssh_key_to_sdk;

/// ORM-based implementation of the `SshKeysRepository` trait.
#[derive(Clone, Default)]
pub struct OrmSshKeysRepository;

#[async_trait]
impl SshKeysRepository for OrmSshKeysRepository {
    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<SshKey>, DomainError> {
        let rows = SshKeyEntity::find()
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(ssh_key_to_sdk).collect())
    }

    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        name: String,
        credstore_ref: String,
        fingerprint: String,
    ) -> Result<SshKey, DomainError> {
        // Kept for the unique-violation error, which consumes `name`.
        let conflict_name = name.clone();

        let am = SshKeyAM {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            name: ActiveValue::Set(name),
            credstore_ref: ActiveValue::Set(credstore_ref),
            fingerprint: ActiveValue::Set(fingerprint),
            created_at: ActiveValue::Set(OffsetDateTime::now_utc()),
        };

        // The INSERTED model, not a locally built struct — see
        // `OrmTestReposRepository::create`.
        match secure_insert::<SshKeyEntity>(am, scope, runner).await {
            Ok(model) => Ok(ssh_key_to_sdk(model)),
            Err(e) if e.is_unique_violation() => Err(DomainError::SshKeyNameExists {
                name: conflict_name,
            }),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn find_by_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<SshKey>, DomainError> {
        let row = SshKeyEntity::find_by_id(id)
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        Ok(row.map(ssh_key_to_sdk))
    }

    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let result = SshKeyEntity::delete_many()
            .filter(sea_orm::Condition::all().add(SshKeyColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        Ok(result.rows_affected > 0)
    }
}
