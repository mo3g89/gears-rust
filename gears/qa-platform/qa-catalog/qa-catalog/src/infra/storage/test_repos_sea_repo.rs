use std::collections::BTreeSet;

use async_trait::async_trait;
use qa_catalog_sdk::{NewTestRepository, TestRepository, TestRepositoryUpdate};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertExt, secure_insert,
    secure_update_with_scope,
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

/// Stale rows deleted per statement: an id list, not one statement per row,
/// and well under every backend's bind-parameter ceiling.
const STALE_BRANCH_ROWS_PER_DELETE: usize = 500;

/// The ids of `cached` rows a listing makes stale — filed under another
/// tenant than `owner`, or a name `listed` lacks — in ascending order.
///
/// Sorted because they are deleted in chunks of
/// [`STALE_BRANCH_ROWS_PER_DELETE`], one statement per chunk, inside one
/// transaction. Two writers replacing the same repository's branches read
/// the cached rows in whatever order the backend returns them; unsorted, each
/// could lock its first chunk and then wait for the other's, a deadlock once
/// more than one chunk is stale. Sorted, both take the same rows in the same
/// order.
fn stale_branch_rows(
    cached: &[repo_branch::Model],
    owner: Uuid,
    listed: &BTreeSet<String>,
) -> Vec<Uuid> {
    let mut stale: Vec<Uuid> = cached
        .iter()
        .filter(|row| row.tenant_id != owner || !listed.contains(&row.name))
        .map(|row| row.id)
        .collect();
    stale.sort_unstable();
    stale
}

/// The `ON CONFLICT` clause of a branch-cache insert: the three columns of
/// `idx_qa_branches_unique`, `DO NOTHING`. The target is named on purpose — a
/// bare `ON CONFLICT DO NOTHING` would also swallow a primary-key collision
/// (see qa-insights' `jira_sea_repo::bug_conflict_target`).
fn branch_conflict_target() -> OnConflict {
    OnConflict::columns([
        BranchColumn::TenantId,
        BranchColumn::RepoId,
        BranchColumn::Name,
    ])
    .do_nothing()
    .to_owned()
}

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

    async fn owner_tenant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Uuid>, DomainError> {
        let found = RepoEntity::find()
            .filter(sea_orm::Condition::all().add(RepoColumn::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        Ok(found.map(|row| row.tenant_id))
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
            head_commit: ActiveValue::Set(None),
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
        // `head_commit` is cleared with the rest: the working area is about to
        // be wiped, so the revision it recorded describes content that no
        // longer exists. Leaving it set would advertise a revision for a
        // repository that is no longer synced at all.
        let (last_synced_at, head_commit, sync_error) = if invalidate_working_copy {
            (
                ActiveValue::Set(None),
                ActiveValue::Set(None),
                ActiveValue::Set(None),
            )
        } else {
            (
                ActiveValue::Unchanged(existing.last_synced_at),
                ActiveValue::Unchanged(existing.head_commit),
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
            head_commit,
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
        head_commit: Option<String>,
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
            head_commit: ActiveValue::Set(head_commit),
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
        repo_id: Uuid,
        branches: Vec<String>,
    ) -> Result<(), DomainError> {
        // The owning tenant, from the repository row itself (DESIGN §3.8 "qa-catalog schema").
        let owner = RepoEntity::find()
            .filter(sea_orm::Condition::all().add(RepoColumn::Id.eq(repo_id)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?
            .ok_or(DomainError::NotFound { id: repo_id })?
            .tenant_id;

        // An idempotent diff (DESIGN §3.3), not delete-all-then-insert. Names
        // already cached for the owner are kept, cached names the listing lacks
        // (or rows filed under another tenant visible in `scope`, DESIGN §3.8)
        // are deleted, and the rest are inserted with `ON CONFLICT DO NOTHING`.
        // Two writers racing on
        // one repository therefore both commit: what one inserted the other
        // finds already there. A name the racing writer could not see yet
        // survives until the next listing, which converges the cache.
        let listed: BTreeSet<String> = branches.into_iter().collect();
        let cached = BranchEntity::find()
            .filter(sea_orm::Condition::all().add(BranchColumn::RepoId.eq(repo_id)))
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;

        let stale = stale_branch_rows(&cached, owner, &listed);
        for chunk in stale.chunks(STALE_BRANCH_ROWS_PER_DELETE) {
            BranchEntity::delete_many()
                .filter(
                    sea_orm::Condition::all().add(BranchColumn::Id.is_in(chunk.iter().copied())),
                )
                .secure()
                .scope_with(scope)
                .exec(runner)
                .await
                .map_err(db_err)?;
        }

        let present: BTreeSet<&str> = cached
            .iter()
            .filter(|row| row.tenant_id == owner)
            .map(|row| row.name.as_str())
            .collect();
        let now = OffsetDateTime::now_utc();
        for name in listed
            .iter()
            .filter(|name| !present.contains(name.as_str()))
        {
            let am = repo_branch::ActiveModel {
                id: ActiveValue::Set(Uuid::new_v4()),
                tenant_id: ActiveValue::Set(owner),
                repo_id: ActiveValue::Set(repo_id),
                name: ActiveValue::Set(name.clone()),
                refreshed_at: ActiveValue::Set(now),
            };
            let inserted = BranchEntity::insert(am.clone())
                .secure()
                .scope_with_model(scope, &am)
                .map_err(db_err)?
                .on_conflict_raw(branch_conflict_target())
                .exec(runner)
                .await;
            match inserted {
                // `RecordNotInserted` is how SeaORM reports the `DO NOTHING`
                // path: another writer filed this name first.
                Ok(_) | Err(ScopeError::Db(sea_orm::DbErr::RecordNotInserted)) => {}
                // A unique violation here can only be another constraint (the
                // named target cannot raise it), and on Postgres it has already
                // aborted the transaction: a real error.
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{STALE_BRANCH_ROWS_PER_DELETE, stale_branch_rows};
    use crate::infra::storage::entity::repo_branch;

    /// Two writers chunk the same stale set the same way whatever order the
    /// backend returned the cached rows in: the ids come back sorted, so the
    /// first chunk of one is the first chunk of the other. A finite fixture
    /// past two chunks.
    #[test]
    fn stale_rows_are_sorted_so_every_writer_deletes_the_same_chunks_in_the_same_order() {
        let owner = Uuid::new_v4();
        let repo_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        let rows: Vec<repo_branch::Model> = (0..(STALE_BRANCH_ROWS_PER_DELETE * 2 + 7))
            .map(|i| repo_branch::Model {
                id: Uuid::new_v4(),
                tenant_id: owner,
                repo_id,
                name: format!("gone-{i}"),
                refreshed_at: now,
            })
            .collect();
        let listed: BTreeSet<String> = BTreeSet::from(["main".to_owned()]);

        let forward = stale_branch_rows(&rows, owner, &listed);
        let mut reversed_rows = rows.clone();
        reversed_rows.reverse();
        let backward = stale_branch_rows(&reversed_rows, owner, &listed);

        assert_eq!(forward.len(), rows.len());
        assert!(forward.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(forward, backward);
    }

    /// A listed name under the owner is kept; the same name filed under
    /// another tenant is stale.
    #[test]
    fn a_listed_name_is_kept_only_under_its_owner() {
        let owner = Uuid::new_v4();
        let repo_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        let kept = repo_branch::Model {
            id: Uuid::new_v4(),
            tenant_id: owner,
            repo_id,
            name: "main".to_owned(),
            refreshed_at: now,
        };
        let foreign = repo_branch::Model {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            ..kept.clone()
        };
        let listed: BTreeSet<String> = BTreeSet::from(["main".to_owned()]);

        assert_eq!(
            stale_branch_rows(&[kept, foreign.clone()], owner, &listed),
            vec![foreign.id]
        );
    }
}
