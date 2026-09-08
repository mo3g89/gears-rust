use std::collections::BTreeSet;

use async_trait::async_trait;
use qa_catalog_sdk::{NewTestRepository, TestRepository, TestRepositoryUpdate};
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert, secure_update_with_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{RefreshTarget, TestReposRepository};
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::repo_branch::{
    self, Column as BranchColumn, Entity as BranchEntity,
};
use crate::infra::storage::entity::test_repository::{
    self, ActiveModel as RepoAM, Column as RepoColumn, Entity as RepoEntity,
};
use crate::infra::storage::mapper::repo_to_sdk;

/// ORM-based implementation of the `TestReposRepository` trait.
#[derive(Clone, Default)]
pub struct OrmTestReposRepository;

#[async_trait]
impl TestReposRepository for OrmTestReposRepository {
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TestRepository>, DomainError> {
        let found = RepoEntity::find()
            .filter(sea_orm::Condition::all().add(RepoColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        Ok(found.map(repo_to_sdk))
    }

    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<TestRepository>, DomainError> {
        let rows = RepoEntity::find()
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(repo_to_sdk).collect())
    }

    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewTestRepository,
    ) -> Result<TestRepository, DomainError> {
        let now = OffsetDateTime::now_utc();
        // Kept for the unique-violation error, which consumes `new.name`.
        let name = new.name.clone();

        let am = RepoAM {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            product_id: ActiveValue::Set(new.product_id),
            name: ActiveValue::Set(new.name),
            url: ActiveValue::Set(new.url),
            default_branch: ActiveValue::Set(new.default_branch),
            content_root: ActiveValue::Set(new.content_root),
            credential_ref: ActiveValue::Set(new.credential_ref),
            last_synced_at: ActiveValue::Set(None),
            sync_error: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        // Return the INSERTED model, not the locally built struct: the two are
        // equivalent today, but only the former would observe a DB-side
        // default or trigger. Same shape as `custom_plans`/`bundles`.
        match secure_insert::<RepoEntity>(am, scope, runner).await {
            Ok(model) => Ok(repo_to_sdk(model)),
            Err(e) if e.is_unique_violation() => Err(DomainError::RepositoryNameExists { name }),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        update: TestRepositoryUpdate,
        invalidate_working_copy: bool,
    ) -> Result<Option<TestRepository>, DomainError> {
        let existing = RepoEntity::find()
            .filter(sea_orm::Condition::all().add(RepoColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        let Some(existing) = existing else {
            return Ok(None);
        };

        // Invalidation and the field writes share one statement: the row can
        // never be observed advertising synced content for a new url.
        let (last_synced_at, sync_error) = if invalidate_working_copy {
            (ActiveValue::Set(None), ActiveValue::Set(None))
        } else {
            (
                ActiveValue::Unchanged(existing.last_synced_at),
                ActiveValue::Unchanged(existing.sync_error),
            )
        };

        let am: RepoAM = test_repository::ActiveModel {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            product_id: ActiveValue::Set(update.product_id),
            name: ActiveValue::Set(update.name.clone()),
            url: ActiveValue::Set(update.url),
            default_branch: ActiveValue::Set(update.default_branch),
            content_root: ActiveValue::Set(update.content_root),
            credential_ref: ActiveValue::Set(update.credential_ref),
            last_synced_at,
            sync_error,
            created_at: ActiveValue::Unchanged(existing.created_at),
            updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
        };

        match secure_update_with_scope::<RepoEntity>(am, scope, id, runner).await {
            Ok(model) => Ok(Some(repo_to_sdk(model))),
            // `idx_qa_repos_unique` is prefixed with `tenant_id`, so the
            // colliding row is always one this tenant can itself see.
            Err(e) if e.is_unique_violation() => {
                Err(DomainError::RepositoryNameExists { name: update.name })
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
        let result = RepoEntity::delete_many()
            .filter(sea_orm::Condition::all().add(RepoColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        Ok(result.rows_affected > 0)
    }

    async fn update_sync_state<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        last_synced_at: Option<OffsetDateTime>,
        sync_error: Option<String>,
    ) -> Result<Option<TestRepository>, DomainError> {
        let existing = RepoEntity::find()
            .filter(sea_orm::Condition::all().add(RepoColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        let Some(existing) = existing else {
            return Ok(None);
        };

        let am: RepoAM = test_repository::ActiveModel {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            product_id: ActiveValue::Unchanged(existing.product_id),
            name: ActiveValue::Unchanged(existing.name),
            url: ActiveValue::Unchanged(existing.url),
            default_branch: ActiveValue::Unchanged(existing.default_branch),
            content_root: ActiveValue::Unchanged(existing.content_root),
            credential_ref: ActiveValue::Unchanged(existing.credential_ref),
            last_synced_at: ActiveValue::Set(last_synced_at),
            sync_error: ActiveValue::Set(sync_error),
            created_at: ActiveValue::Unchanged(existing.created_at),
            updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
        };

        // No unique column is touched here, so a unique violation is not
        // reachable on this path.
        let model = secure_update_with_scope::<RepoEntity>(am, scope, id, runner)
            .await
            .map_err(db_err)?;
        Ok(Some(repo_to_sdk(model)))
    }

    async fn replace_branches<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        repo_id: Uuid,
        branches: Vec<String>,
    ) -> Result<(), DomainError> {
        let now = OffsetDateTime::now_utc();

        BranchEntity::delete_many()
            .filter(sea_orm::Condition::all().add(BranchColumn::RepoId.eq(repo_id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        // Collapse duplicates from the git remote (and give the cache a stable
        // insertion order) before hitting the `(tenant_id, repo_id, name)`
        // unique index.
        let unique: BTreeSet<String> = branches.into_iter().collect();

        for name in unique {
            let am = repo_branch::ActiveModel {
                id: ActiveValue::Set(Uuid::new_v4()),
                tenant_id: ActiveValue::Set(tenant_id),
                repo_id: ActiveValue::Set(repo_id),
                name: ActiveValue::Set(name),
                refreshed_at: ActiveValue::Set(now),
            };

            match secure_insert::<BranchEntity>(am, scope, runner).await {
                Ok(_) => {}
                // Two syncs of the same repository racing each other: the
                // loser sees rows it did not insert. Surface a retryable
                // domain conflict rather than an opaque `Database` error.
                // Since `idx_qa_branches_unique` is prefixed with `tenant_id`,
                // the colliding row is necessarily one of this tenant's own —
                // another tenant cannot make this branch fail forever, so
                // "retry" is honest advice.
                Err(e) if e.is_unique_violation() => {
                    return Err(DomainError::BranchCacheConflict { repo_id });
                }
                Err(e) => return Err(db_err(e)),
            }
        }

        Ok(())
    }

    async fn list_branches<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        repo_id: Uuid,
    ) -> Result<Vec<String>, DomainError> {
        let rows = BranchEntity::find()
            .filter(sea_orm::Condition::all().add(BranchColumn::RepoId.eq(repo_id)))
            .secure()
            .scope_with(scope)
            .order_by(BranchColumn::Name, sea_orm::Order::Asc)
            .all(runner)
            .await
            .map_err(db_err)?;

        Ok(rows.into_iter().map(|m| m.name).collect())
    }

    async fn list_refresh_targets<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<RefreshTarget>, DomainError> {
        let rows = RepoEntity::find()
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .map(|m| RefreshTarget {
                repo_id: m.id,
                tenant_id: m.tenant_id,
            })
            .collect())
    }
}
