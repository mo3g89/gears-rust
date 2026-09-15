use async_trait::async_trait;
use qa_environments_sdk::{NewVariable, Variable};
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::odata::sea_orm_filter::paginate_odata;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert, secure_update_with_scope,
};
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::VariablesRepository;
use crate::infra::storage::db::{PAGE_LIMITS, db_err, odata_err};
use crate::infra::storage::entity::environment_variable::{
    self, Column as EnvironmentVarColumn, Entity as EnvironmentVarEntity,
};
use crate::infra::storage::entity::pipeline_variable::{
    self, Column as PipelineColumn, Entity as PipelineEntity,
};
use crate::infra::storage::mapper::{environment_var_to_sdk, pipeline_var_to_sdk};
use crate::infra::storage::odata::{
    EnvironmentVarODataMapper, NAME_TIEBREAKER, PipelineVarODataMapper, VariableFilterField,
};

/// ORM-based implementation of the `VariablesRepository` trait.
#[derive(Clone, Default)]
pub struct OrmVariablesRepository;

#[async_trait]
impl VariablesRepository for OrmVariablesRepository {
    async fn list_pipeline_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<Variable>, DomainError> {
        // Scoped select first — `paginate_odata` takes `SecureSelect<E, Scoped>`
        // and nothing else, so the caller's `$filter` is `AND`ed onto the tenant
        // predicate rather than substituted for it.
        let scoped = PipelineEntity::find().secure().scope_with(scope);

        paginate_odata::<VariableFilterField, PipelineVarODataMapper, _, _, _, _>(
            scoped,
            runner,
            query,
            NAME_TIEBREAKER,
            PAGE_LIMITS,
            pipeline_var_to_sdk,
        )
        .await
        .map_err(|error| odata_err(&error))
    }

    async fn list_for_environment_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        environment_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<Variable>, DomainError> {
        // `EnvironmentVarColumn::EnvironmentId` is the SeaORM *variant*; the
        // physical column it renders is `environment_id`
        // (`entity/environment_variable.rs`'s `#[sea_orm(column_name = ...)]`).
        // That is also why `environment_id` is pinned here rather than left to
        // a `$filter` — see `VariableFilterField`'s doc.
        let scoped = EnvironmentVarEntity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(EnvironmentVarColumn::EnvironmentId.eq(environment_id)),
            )
            .secure()
            .scope_with(scope);

        paginate_odata::<VariableFilterField, EnvironmentVarODataMapper, _, _, _, _>(
            scoped,
            runner,
            query,
            NAME_TIEBREAKER,
            PAGE_LIMITS,
            environment_var_to_sdk,
        )
        .await
        .map_err(|error| odata_err(&error))
    }

    async fn find_by_natural_key<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        environment_id: Option<Uuid>,
        name: &str,
    ) -> Result<Option<Variable>, DomainError> {
        if let Some(environment_id) = environment_id {
            let existing =
                find_environment_var(runner, scope, tenant_id, environment_id, name).await?;
            Ok(existing.map(environment_var_to_sdk))
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

        if let Some(environment_id) = var.environment_id {
            upsert_environment_var(runner, scope, tenant_id, environment_id, &var, now).await
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
        let environment_result = EnvironmentVarEntity::delete_many()
            .filter(sea_orm::Condition::all().add(EnvironmentVarColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        if environment_result.rows_affected > 0 {
            return Ok(true);
        }

        let pipeline_result = PipelineEntity::delete_many()
            .filter(sea_orm::Condition::all().add(PipelineColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        Ok(pipeline_result.rows_affected > 0)
    }
}

/// Find a per-environment variable row by its natural key `(environment_id, name)`,
/// additionally bound to `tenant_id` — symmetric with [`find_pipeline_var`],
/// which is keyed by `(tenant_id, name)`. Without the tenant predicate this
/// probe would be a cross-tenant existence oracle: a caller could learn
/// whether *any* tenant has a variable named `name` on `environment_id`,
/// independent of the row-level `AccessScope` filter applied by `.secure()`.
async fn find_environment_var<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    tenant_id: Uuid,
    environment_id: Uuid,
    name: &str,
) -> Result<Option<environment_variable::Model>, DomainError> {
    EnvironmentVarEntity::find()
        .filter(
            sea_orm::Condition::all()
                .add(EnvironmentVarColumn::TenantId.eq(tenant_id))
                .add(EnvironmentVarColumn::EnvironmentId.eq(environment_id))
                .add(EnvironmentVarColumn::Name.eq(name)),
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
                .add(PipelineColumn::TenantId.eq(tenant_id))
                .add(PipelineColumn::Name.eq(name)),
        )
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
        .map_err(db_err)
}

/// Insert-or-update a per-environment variable keyed by `(environment_id, name)`.
async fn upsert_environment_var<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    tenant_id: Uuid,
    environment_id: Uuid,
    var: &NewVariable,
    now: OffsetDateTime,
) -> Result<Variable, DomainError> {
    let existing =
        find_environment_var(runner, scope, tenant_id, environment_id, &var.name).await?;

    let model = if let Some(existing) = existing {
        let am = environment_variable::ActiveModel {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            environment_id: ActiveValue::Unchanged(existing.environment_id),
            name: ActiveValue::Unchanged(existing.name.clone()),
            value: ActiveValue::Set(var.value.clone()),
            created_at: ActiveValue::Unchanged(existing.created_at),
            updated_at: ActiveValue::Set(now),
        };
        secure_update_with_scope::<EnvironmentVarEntity>(am, scope, existing.id, runner)
            .await
            .map_err(db_err)?
    } else {
        let am = environment_variable::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            environment_id: ActiveValue::Set(environment_id),
            name: ActiveValue::Set(var.name.clone()),
            value: ActiveValue::Set(var.value.clone()),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };
        // A concurrent insert can race us between the `find_environment_var`
        // probe above and this insert; map that race to a domain error
        // instead of a generic `Database` error so the REST layer can
        // report it as `already_exists` rather than an opaque 500.
        match secure_insert::<EnvironmentVarEntity>(am, scope, runner).await {
            Ok(model) => model,
            Err(e) if e.is_unique_violation() => {
                return Err(DomainError::VariableNameExists {
                    name: var.name.clone(),
                });
            }
            Err(e) => return Err(db_err(e)),
        }
    };

    Ok(environment_var_to_sdk(model))
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
        // See the matching comment in `upsert_environment_var`: map an
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
