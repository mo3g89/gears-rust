use async_trait::async_trait;
use qa_environments_sdk::{NewVariable, Variable};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert, secure_update_with_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::VariablesRepository;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::pipeline_variable::{
    self, Column as PipelineColumn, Entity as PipelineEntity,
};
use crate::infra::storage::entity::platform_variable::{
    self, Column as PlatformVarColumn, Entity as PlatformVarEntity,
};
use crate::infra::storage::mapper::{pipeline_var_to_sdk, platform_var_to_sdk};

/// ORM-based implementation of the `VariablesRepository` trait.
#[derive(Clone, Default)]
pub struct OrmVariablesRepository;

#[async_trait]
impl VariablesRepository for OrmVariablesRepository {
    async fn list_pipeline<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Variable>, DomainError> {
        let rows = PipelineEntity::find()
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(pipeline_var_to_sdk).collect())
    }

    async fn list_for_platform<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        platform_id: Uuid,
    ) -> Result<Vec<Variable>, DomainError> {
        let rows = PlatformVarEntity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(Expr::col(PlatformVarColumn::PlatformId).eq(platform_id)),
            )
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(platform_var_to_sdk).collect())
    }

    async fn find_by_natural_key<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        platform_id: Option<Uuid>,
        name: &str,
    ) -> Result<Option<Variable>, DomainError> {
        if let Some(platform_id) = platform_id {
            let existing = find_platform_var(runner, scope, tenant_id, platform_id, name).await?;
            Ok(existing.map(platform_var_to_sdk))
        } else {
            let existing = find_pipeline_var(runner, scope, tenant_id, name).await?;
            Ok(existing.map(pipeline_var_to_sdk))
        }
    }

    async fn upsert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        var: NewVariable,
    ) -> Result<Variable, DomainError> {
        let now = OffsetDateTime::now_utc();

        if let Some(platform_id) = var.platform_id {
            upsert_platform_var(runner, scope, tenant_id, platform_id, &var, now).await
        } else {
            upsert_pipeline_var(runner, scope, tenant_id, &var, now).await
        }
    }

    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let platform_result = PlatformVarEntity::delete_many()
            .filter(sea_orm::Condition::all().add(Expr::col(PlatformVarColumn::Id).eq(id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        if platform_result.rows_affected > 0 {
            return Ok(true);
        }

        let pipeline_result = PipelineEntity::delete_many()
            .filter(sea_orm::Condition::all().add(Expr::col(PipelineColumn::Id).eq(id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        Ok(pipeline_result.rows_affected > 0)
    }
}

/// Find a per-platform variable row by its natural key `(platform_id, name)`,
/// additionally bound to `tenant_id` — symmetric with [`find_pipeline_var`],
/// which is keyed by `(tenant_id, name)`. Without the tenant predicate this
/// probe would be a cross-tenant existence oracle: a caller could learn
/// whether *any* tenant has a variable named `name` on `platform_id`,
/// independent of the row-level `AccessScope` filter applied by `.secure()`.
async fn find_platform_var<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    tenant_id: Uuid,
    platform_id: Uuid,
    name: &str,
) -> Result<Option<platform_variable::Model>, DomainError> {
    PlatformVarEntity::find()
        .filter(
            sea_orm::Condition::all()
                .add(Expr::col(PlatformVarColumn::TenantId).eq(tenant_id))
                .add(Expr::col(PlatformVarColumn::PlatformId).eq(platform_id))
                .add(Expr::col(PlatformVarColumn::Name).eq(name)),
        )
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
        .map_err(db_err)
}

/// Find a pipeline (global) variable row by its natural key `(tenant_id, name)`.
async fn find_pipeline_var<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    tenant_id: Uuid,
    name: &str,
) -> Result<Option<pipeline_variable::Model>, DomainError> {
    PipelineEntity::find()
        .filter(
            sea_orm::Condition::all()
                .add(Expr::col(PipelineColumn::TenantId).eq(tenant_id))
                .add(Expr::col(PipelineColumn::Name).eq(name)),
        )
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
        .map_err(db_err)
}

/// Insert-or-update a per-platform variable keyed by `(platform_id, name)`.
async fn upsert_platform_var<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    tenant_id: Uuid,
    platform_id: Uuid,
    var: &NewVariable,
    now: OffsetDateTime,
) -> Result<Variable, DomainError> {
    let existing = find_platform_var(runner, scope, tenant_id, platform_id, &var.name).await?;

    let model = if let Some(existing) = existing {
        let am = platform_variable::ActiveModel {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            platform_id: ActiveValue::Unchanged(existing.platform_id),
            name: ActiveValue::Unchanged(existing.name.clone()),
            value: ActiveValue::Set(var.value.clone()),
            created_at: ActiveValue::Unchanged(existing.created_at),
            updated_at: ActiveValue::Set(now),
        };
        secure_update_with_scope::<PlatformVarEntity>(am, scope, existing.id, runner)
            .await
            .map_err(db_err)?
    } else {
        let am = platform_variable::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            platform_id: ActiveValue::Set(platform_id),
            name: ActiveValue::Set(var.name.clone()),
            value: ActiveValue::Set(var.value.clone()),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };
        // A concurrent insert can race us between the `find_platform_var`
        // probe above and this insert; map that race to a domain error
        // instead of a generic `Database` error so the REST layer can
        // report it as `already_exists` rather than an opaque 500.
        match secure_insert::<PlatformVarEntity>(am, scope, runner).await {
            Ok(model) => model,
            Err(e) if e.is_unique_violation() => {
                return Err(DomainError::VariableNameExists {
                    name: var.name.clone(),
                });
            }
            Err(e) => return Err(db_err(e)),
        }
    };

    Ok(platform_var_to_sdk(model))
}

/// Insert-or-update a pipeline (global) variable keyed by `(tenant_id, name)`.
async fn upsert_pipeline_var<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    tenant_id: Uuid,
    var: &NewVariable,
    now: OffsetDateTime,
) -> Result<Variable, DomainError> {
    let existing = find_pipeline_var(runner, scope, tenant_id, &var.name).await?;

    let model = if let Some(existing) = existing {
        let am = pipeline_variable::ActiveModel {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            name: ActiveValue::Unchanged(existing.name.clone()),
            value: ActiveValue::Set(var.value.clone()),
            created_at: ActiveValue::Unchanged(existing.created_at),
            updated_at: ActiveValue::Set(now),
        };
        secure_update_with_scope::<PipelineEntity>(am, scope, existing.id, runner)
            .await
            .map_err(db_err)?
    } else {
        let am = pipeline_variable::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            name: ActiveValue::Set(var.name.clone()),
            value: ActiveValue::Set(var.value.clone()),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };
        // See the matching comment in `upsert_platform_var`: map an
        // insert-race unique violation to a domain error instead of a
        // generic `Database` error.
        match secure_insert::<PipelineEntity>(am, scope, runner).await {
            Ok(model) => model,
            Err(e) if e.is_unique_violation() => {
                return Err(DomainError::VariableNameExists {
                    name: var.name.clone(),
                });
            }
            Err(e) => return Err(db_err(e)),
        }
    };

    Ok(pipeline_var_to_sdk(model))
}
