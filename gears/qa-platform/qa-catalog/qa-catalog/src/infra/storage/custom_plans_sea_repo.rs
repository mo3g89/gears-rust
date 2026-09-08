use async_trait::async_trait;
use qa_catalog_sdk::{CustomPlan, CustomPlanEntry, NewCustomPlan, NewCustomPlanEntry};
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert, secure_update_with_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::CustomPlansRepository;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::custom_plan::{
    self, ActiveModel as PlanAM, Column as PlanColumn, Entity as PlanEntity,
};
use crate::infra::storage::mapper::{
    custom_plan_entries_to_json, custom_plan_to_sdk, db_optional_i64_from_u64, tags_to_json,
};

/// ORM-based implementation of the `CustomPlansRepository` trait.
#[derive(Clone, Default)]
pub struct OrmCustomPlansRepository;

#[async_trait]
impl CustomPlansRepository for OrmCustomPlansRepository {
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<CustomPlan>, DomainError> {
        let found = PlanEntity::find()
            .filter(sea_orm::Condition::all().add(PlanColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        found.map(custom_plan_to_sdk).transpose()
    }

    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<CustomPlan>, DomainError> {
        let rows = PlanEntity::find()
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;

        rows.into_iter().map(custom_plan_to_sdk).collect()
    }

    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewCustomPlan,
    ) -> Result<CustomPlan, DomainError> {
        let now = OffsetDateTime::now_utc();
        // `NewCustomPlanEntry` -> `CustomPlanEntry` is the widening that fills in
        // the mandatory `plan_path` as `Some`. The storage codec takes the *read*
        // entry deliberately: it is the shape that can also represent a row
        // written before the field existed, which is what it has to decode.
        let files = custom_plan_entries_to_json(&entries_for_storage(&new.files))?;
        let tags = tags_to_json(&new.tags)?;
        let timeout_seconds = db_optional_i64_from_u64(new.timeout_seconds, "timeout_seconds")?;

        let am = PlanAM {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            name: ActiveValue::Set(new.name.clone()),
            files: ActiveValue::Set(files),
            tags: ActiveValue::Set(tags),
            timeout_seconds: ActiveValue::Set(timeout_seconds),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        match secure_insert::<PlanEntity>(am, scope, runner).await {
            Ok(model) => custom_plan_to_sdk(model),
            Err(e) if e.is_unique_violation() => {
                Err(DomainError::CustomPlanNameExists { name: new.name })
            }
            Err(e) => Err(db_err(e)),
        }
    }

    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        new: NewCustomPlan,
    ) -> Result<Option<CustomPlan>, DomainError> {
        let existing = PlanEntity::find()
            .filter(sea_orm::Condition::all().add(PlanColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        let Some(existing) = existing else {
            return Ok(None);
        };

        // `NewCustomPlanEntry` -> `CustomPlanEntry` is the widening that fills in
        // the mandatory `plan_path` as `Some`. The storage codec takes the *read*
        // entry deliberately: it is the shape that can also represent a row
        // written before the field existed, which is what it has to decode.
        let files = custom_plan_entries_to_json(&entries_for_storage(&new.files))?;
        let tags = tags_to_json(&new.tags)?;
        let timeout_seconds = db_optional_i64_from_u64(new.timeout_seconds, "timeout_seconds")?;

        let am: PlanAM = custom_plan::ActiveModel {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            name: ActiveValue::Set(new.name.clone()),
            files: ActiveValue::Set(files),
            tags: ActiveValue::Set(tags),
            timeout_seconds: ActiveValue::Set(timeout_seconds),
            created_at: ActiveValue::Unchanged(existing.created_at),
            updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
        };

        match secure_update_with_scope::<PlanEntity>(am, scope, id, runner).await {
            Ok(model) => custom_plan_to_sdk(model).map(Some),
            Err(e) if e.is_unique_violation() => {
                Err(DomainError::CustomPlanNameExists { name: new.name })
            }
            Err(e) => Err(db_err(e)),
        }
    }

    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let result = PlanEntity::delete_many()
            .filter(sea_orm::Condition::all().add(PlanColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        Ok(result.rows_affected > 0)
    }
}

/// Widen the write entries to the storage/read shape.
///
/// Both write paths go through this, so "mandatory on write, optional in
/// storage" is expressed in exactly one place. See
/// `qa_catalog_sdk::CustomPlanEntry`'s three-way table for why the two differ.
fn entries_for_storage(files: &[NewCustomPlanEntry]) -> Vec<CustomPlanEntry> {
    files.iter().cloned().map(CustomPlanEntry::from).collect()
}
