//! `SecureORM` implementation of [`SavedViewsRepository`].
//!
//! # Obligation #2 lives in this file, and only in this file
//!
//! `qa_analytics_saved_views.plan_key` materializes legacy's
//! `COALESCE(plan_id, '')` (`001_initial.sql:194-195`) because `SQLite` and
//! `MySQL` do not both support functional indexes. It is the fourth column of
//! `idx_qa_analytics_saved_views_unique`, it is `NOT NULL DEFAULT ''`, and
//! **nothing in the database checks that it agrees with `repo_id`/`plan_path`**.
//!
//! So every write here goes through [`mapper::plan_key`], and every read that
//! keys on the index does too. Deriving it in one place is the whole control: a
//! probe that computed it differently from a write would report a collision that
//! the write would not have produced, or miss one it would — which is the wrong
//! 409 with no error anywhere that the schema's header describes.
//!
//! A stored generated column would remove the obligation and was considered and
//! declined at Task 10; its migration's header carries the three reasons under
//! "A **stored generated column** would also work, and was declined".

use async_trait::async_trait;
use qa_insights_sdk::{NewSavedView, SavedView, SavedViewScope};
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert, secure_update_with_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::analytics::PlanRef;
use crate::domain::error::DomainError;
use crate::domain::repos::{SavedViewKey, SavedViewsRepository};
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::saved_view::{self, Column as ViewColumn, Entity as ViewEntity};
use crate::infra::storage::mapper::{plan_key, query_json_to_column, saved_view_to_sdk};

/// ORM-based implementation of the `SavedViewsRepository` trait.
#[derive(Clone, Default)]
pub struct OrmSavedViewsRepository;

/// `plan_key` for one [`NewSavedView`], and the only derivation the write paths
/// use.
///
/// Reads `repo_id`/`plan_path` and **not** `scope`: the two must agree, and if
/// they do not, the pair is what the index and the readable columns both key on.
/// A `Plan`-scoped view with no plan identity is a caller error legacy 400s on
/// (`manager/src/routes/analytics.rs:530-540`) and the REST layer rejects; it
/// gets `''` here rather than a third spelling of "global".
fn key_for(view: &NewSavedView) -> String {
    plan_key(view.repo_id, view.plan_path.as_deref())
}

/// `plan_key` for a list or probe, derived from the same function the writers
/// use.
fn key_for_plan(plan: Option<&PlanRef>) -> String {
    plan_key(plan.map(|p| p.repo_id), plan.map(|p| p.plan_path.as_str()))
}

#[async_trait]
impl SavedViewsRepository for OrmSavedViewsRepository {
    /// Filters on `scope` **and** `plan_key`, not on `repo_id`/`plan_path`.
    ///
    /// The two are equivalent for a correctly written row, and keying on
    /// `plan_key` is what makes this reach
    /// `idx_qa_analytics_saved_views_unique`'s `(tenant_id, owner_id, scope,
    /// plan_key)` prefix. It also means a row whose `plan_key` was *not* written
    /// disappears from the list it belongs to rather than appearing in two — a
    /// failure that shows up in
    /// `a_plan_scoped_and_a_global_view_of_one_name_both_persist_and_both_list`
    /// instead of hiding behind a redundant predicate.
    ///
    /// The owner narrowing is the `AccessScope`'s, through `owner_col =
    /// "owner_id"` on the entity — the one entity in this gear with an owner.
    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope_ctx: &AccessScope,
        scope: SavedViewScope,
        plan: Option<&PlanRef>,
    ) -> Result<Vec<SavedView>, DomainError> {
        let rows = ViewEntity::find()
            .secure()
            .scope_with(scope_ctx)
            .filter(
                Condition::all()
                    .add(ViewColumn::Scope.eq(scope.as_str()))
                    .add(ViewColumn::PlanKey.eq(key_for_plan(plan))),
            )
            .all(runner)
            .await
            .map_err(db_err)?;
        rows.into_iter().map(saved_view_to_sdk).collect()
    }

    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope_ctx: &AccessScope,
        tenant_id: Uuid,
        owner_id: Uuid,
        view: NewSavedView,
    ) -> Result<SavedView, DomainError> {
        let now = OffsetDateTime::now_utc();
        let name = view.name.clone();
        let am = saved_view::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            owner_id: ActiveValue::Set(owner_id),
            scope: ActiveValue::Set(view.scope.as_str().to_owned()),
            repo_id: ActiveValue::Set(view.repo_id),
            plan_path: ActiveValue::Set(view.plan_path.clone()),
            // Obligation #2. Never a literal, never omitted.
            plan_key: ActiveValue::Set(key_for(&view)),
            name: ActiveValue::Set(view.name),
            query_json: ActiveValue::Set(query_json_to_column(&view.query_json)?),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        match secure_insert::<ViewEntity>(am, scope_ctx, runner).await {
            Ok(model) => saved_view_to_sdk(model),
            // A collision on `idx_qa_analytics_saved_views_unique` is the
            // domain's `SavedViewNameExists`, not a 500 — which is the point of
            // the coalesce: it is what makes two global views of one name
            // collide at all.
            Err(e) if e.is_unique_violation() => Err(DomainError::SavedViewNameExists { name }),
            Err(e) => Err(db_err(e)),
        }
    }

    /// Rewrites `plan_key` along with `scope`, `repo_id` and `plan_path`.
    ///
    /// An update that moves a view between scopes and leaves the old key behind
    /// is the second half of obligation #2:
    /// `updating_a_view_into_a_plan_scope_rewrites_its_plan_key` is the guard.
    ///
    /// `Ok(None)` when no row matched the caller's scope — absent and foreign
    /// are deliberately indistinguishable.
    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope_ctx: &AccessScope,
        id: Uuid,
        view: NewSavedView,
    ) -> Result<Option<SavedView>, DomainError> {
        let existing = ViewEntity::find()
            .secure()
            .scope_with(scope_ctx)
            .and_id(id)
            .map_err(db_err)?
            .one(runner)
            .await
            .map_err(db_err)?;
        let Some(existing) = existing else {
            return Ok(None);
        };

        let name = view.name.clone();
        let am = saved_view::ActiveModel {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            // The owner is not a caller-editable field: `NewSavedView` carries
            // none, and re-assigning a view would be a different operation.
            owner_id: ActiveValue::Unchanged(existing.owner_id),
            scope: ActiveValue::Set(view.scope.as_str().to_owned()),
            repo_id: ActiveValue::Set(view.repo_id),
            plan_path: ActiveValue::Set(view.plan_path.clone()),
            // Obligation #2, on the path where forgetting it is worse: the row
            // already exists with a key that is now wrong.
            plan_key: ActiveValue::Set(key_for(&view)),
            name: ActiveValue::Set(view.name),
            query_json: ActiveValue::Set(query_json_to_column(&view.query_json)?),
            created_at: ActiveValue::Unchanged(existing.created_at),
            updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
        };

        match secure_update_with_scope::<ViewEntity>(am, scope_ctx, id, runner).await {
            Ok(model) => saved_view_to_sdk(model).map(Some),
            Err(e) if e.is_unique_violation() => Err(DomainError::SavedViewNameExists { name }),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope_ctx: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let result = ViewEntity::delete_many()
            .filter(Condition::all().add(ViewColumn::Id.eq(id)))
            .secure()
            .scope_with(scope_ctx)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected > 0)
    }

    /// Bound to `tenant_id` **as well as** to the scope, so it cannot be used as
    /// a cross-tenant existence oracle: without the explicit predicate a caller
    /// could learn whether *any* tenant's owner has a view of this name.
    /// Symmetric with `qa-environments`' `find_platform_var`.
    async fn find_by_natural_key<C: DBRunner>(
        &self,
        runner: &C,
        scope_ctx: &AccessScope,
        tenant_id: Uuid,
        key: SavedViewKey<'_>,
    ) -> Result<Option<SavedView>, DomainError> {
        let row = ViewEntity::find()
            .secure()
            .scope_with(scope_ctx)
            .filter(
                Condition::all()
                    .add(ViewColumn::TenantId.eq(tenant_id))
                    .add(ViewColumn::OwnerId.eq(key.owner_id))
                    .add(ViewColumn::Scope.eq(key.scope.as_str()))
                    // The same derivation the writers use, which is the whole
                    // reason `SavedViewKey` carries no `plan_key` of its own.
                    .add(ViewColumn::PlanKey.eq(key_for_plan(key.plan)))
                    .add(ViewColumn::Name.eq(key.name)),
            )
            .one(runner)
            .await
            .map_err(db_err)?;
        row.map(saved_view_to_sdk).transpose()
    }
}

#[cfg(test)]
mod tests {
    use qa_insights_sdk::{NewSavedView, SavedViewScope};
    use uuid::Uuid;

    use crate::domain::analytics::PlanRef;
    use crate::domain::error::DomainError;
    use crate::domain::repos::{SavedViewKey, SavedViewsRepository};
    use crate::infra::storage::saved_views_sea_repo::OrmSavedViewsRepository;
    use crate::infra::storage::test_db::{inmem_db, scope};

    fn plan() -> PlanRef {
        PlanRef {
            repo_id: Uuid::from_u128(0x12),
            plan_path: "plans/smoke/plan.yaml".to_owned(),
        }
    }

    fn global_view(name: &str) -> NewSavedView {
        NewSavedView {
            scope: SavedViewScope::All,
            repo_id: None,
            plan_path: None,
            name: name.to_owned(),
            query_json: r#"{"version":"5.0.1"}"#.to_owned(),
        }
    }

    /// The natural-key probe for a plan-scoped view called "Shared Name", which
    /// is the only thing
    /// [`the_natural_key_probe_distinguishes_two_plans_at_the_same_scope`] varies.
    async fn probe_shared_name(
        conn: &toolkit_db::secure::DbConn<'_>,
        ctx: &toolkit_security::AccessScope,
        tenant: Uuid,
        owner: Uuid,
        plan: &PlanRef,
    ) -> Option<Uuid> {
        OrmSavedViewsRepository
            .find_by_natural_key(
                conn,
                ctx,
                tenant,
                SavedViewKey {
                    owner_id: owner,
                    scope: SavedViewScope::Plan,
                    plan: Some(plan),
                    name: "Shared Name",
                },
            )
            .await
            .unwrap()
            .map(|v| v.id)
    }

    fn plan_view(name: &str) -> NewSavedView {
        let plan = plan();
        NewSavedView {
            scope: SavedViewScope::Plan,
            repo_id: Some(plan.repo_id),
            plan_path: Some(plan.plan_path),
            name: name.to_owned(),
            query_json: r#"{"version":"5.0.1"}"#.to_owned(),
        }
    }

    /// **`query_json` round-trips as a *document*, not as text.**
    ///
    /// Both existing fixtures were already compact, so neither direction of this
    /// property was tested. `Json::to_string()` re-serializes: insignificant
    /// whitespace is dropped and key order becomes the parser's. Pinned in both
    /// directions so that the right half (the document survives) is guaranteed and
    /// the wrong half (the bytes survive) cannot be relied on by a later caller —
    /// a REST handler that diffed the stored string against the submitted one
    /// would otherwise report a spurious change on every read.
    #[tokio::test]
    async fn a_saved_views_query_json_round_trips_as_a_document_not_as_text() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let owner = Uuid::from_u128(0xB);
        let ctx = scope(tenant);

        let pretty = "{\n  \"version\" : \"5.0.1\",\n  \"tags\": [ \"smoke\" ,\"e2e\" ]\n}";
        let stored = OrmSavedViewsRepository
            .create(
                &conn,
                &ctx,
                tenant,
                owner,
                NewSavedView {
                    query_json: pretty.to_owned(),
                    ..global_view("Pretty View")
                },
            )
            .await
            .unwrap();

        assert_ne!(
            stored.query_json, pretty,
            "the formatting is NOT preserved; if this ever passes, the doc on \
             `mapper::saved_view_to_sdk` is wrong again"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stored.query_json).unwrap(),
            serde_json::from_str::<serde_json::Value>(pretty).unwrap(),
            "but the document must be identical -- same keys, same values, same \
             array order"
        );

        // And it survives a read back through the list query, not only the
        // create's return value.
        let listed = OrmSavedViewsRepository
            .list(&conn, &ctx, SavedViewScope::All, None)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&listed[0].query_json).unwrap(),
            serde_json::from_str::<serde_json::Value>(pretty).unwrap()
        );
    }

    /// **The guard for obligation #2, and the only one possible.**
    ///
    /// `qa_analytics_saved_views.plan_key` is `NOT NULL DEFAULT ''`, so a writer
    /// that forgets it compiles, inserts, and silently collides a plan-scoped
    /// view with the owner's *global* view of the same name — a wrong 409 with
    /// no error anywhere. Nothing in the database can detect that.
    ///
    /// Task 10's `a_global_and_a_plan_scoped_view_may_share_a_name` proves the
    /// *schema* permits the pair; this proves the *repository* populates the
    /// column that makes it work, which is why it goes through `create` and
    /// `list` rather than raw SQL.
    #[tokio::test]
    async fn a_plan_scoped_and_a_global_view_of_one_name_both_persist_and_both_list() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let owner = Uuid::from_u128(0xB);
        let ctx = scope(tenant);

        let global = OrmSavedViewsRepository
            .create(&conn, &ctx, tenant, owner, global_view("My View"))
            .await
            .expect("the global view must persist");
        let scoped = OrmSavedViewsRepository
            .create(&conn, &ctx, tenant, owner, plan_view("My View"))
            .await
            .expect(
                "the plan-scoped view must persist too: without plan_key it \
                 collides with the global view of the same name",
            );

        let globals = OrmSavedViewsRepository
            .list(&conn, &ctx, SavedViewScope::All, None)
            .await
            .unwrap();
        assert_eq!(
            globals.iter().map(|v| v.id).collect::<Vec<_>>(),
            vec![global.id],
            "the global list must hold exactly the global view"
        );

        let scoped_list = OrmSavedViewsRepository
            .list(&conn, &ctx, SavedViewScope::Plan, Some(&plan()))
            .await
            .unwrap();
        assert_eq!(
            scoped_list.iter().map(|v| v.id).collect::<Vec<_>>(),
            vec![scoped.id],
            "and the plan list exactly the plan-scoped one"
        );
    }

    /// **A plan-scoped list must return *this* plan's views, not every
    /// plan-scoped view.**
    ///
    /// Added 2026-08-20 after break-verification: removing the `plan_key`
    /// predicate from `list` left every test green, because the fixtures only
    /// ever had one plan-scoped view and `scope` alone separated it from the
    /// global one. Two plans is what makes the predicate load-bearing.
    #[tokio::test]
    async fn a_plan_scoped_list_returns_only_that_plans_views() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let owner = Uuid::from_u128(0xB);
        let ctx = scope(tenant);

        let mine = OrmSavedViewsRepository
            .create(&conn, &ctx, tenant, owner, plan_view("Smoke View"))
            .await
            .unwrap();
        let other = PlanRef {
            repo_id: plan().repo_id,
            plan_path: "plans/regression/plan.yaml".to_owned(),
        };
        OrmSavedViewsRepository
            .create(
                &conn,
                &ctx,
                tenant,
                owner,
                NewSavedView {
                    plan_path: Some(other.plan_path.clone()),
                    ..plan_view("Regression View")
                },
            )
            .await
            .unwrap();

        assert_eq!(
            OrmSavedViewsRepository
                .list(&conn, &ctx, SavedViewScope::Plan, Some(&plan()))
                .await
                .unwrap()
                .iter()
                .map(|v| v.id)
                .collect::<Vec<_>>(),
            vec![mine.id],
            "another plan's view is not on this plan"
        );
    }

    /// A second global view of the same name and owner is the collision the
    /// coalesce exists to create: in SQL a `NULL` never equals another `NULL`, so
    /// without `plan_key` one owner could keep unlimited global views all called
    /// "My View".
    #[tokio::test]
    async fn a_second_global_view_of_one_name_is_rejected_as_a_name_collision() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let owner = Uuid::from_u128(0xB);
        let ctx = scope(tenant);

        OrmSavedViewsRepository
            .create(&conn, &ctx, tenant, owner, global_view("My View"))
            .await
            .unwrap();
        let again = OrmSavedViewsRepository
            .create(&conn, &ctx, tenant, owner, global_view("My View"))
            .await;

        assert!(
            matches!(again, Err(DomainError::SavedViewNameExists { ref name }) if name == "My View"),
            "a duplicate name must be SavedViewNameExists, not a Database error: {again:?}"
        );
    }

    /// **The probe must key on `plan_key`, and only two plan-scoped views can
    /// show it.**
    ///
    /// Added 2026-08-20 after break-verification: deleting the `plan_key`
    /// predicate from `find_by_natural_key` left the whole suite green.
    /// `the_natural_key_probe_distinguishes_a_plan_scoped_view_from_a_global_one`
    /// could not catch it, because the two rows there differ in `scope` as well,
    /// so the `scope` predicate alone separated them. Two views at the *same*
    /// scope on *different* plans, sharing a name, is the case where `plan_key` is
    /// the only thing that tells them apart — and getting this wrong is the wrong
    /// 409 the whole obligation is about, since this probe is what decides
    /// create-versus-update.
    #[tokio::test]
    async fn the_natural_key_probe_distinguishes_two_plans_at_the_same_scope() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let owner = Uuid::from_u128(0xB);
        let ctx = scope(tenant);

        let on_smoke = OrmSavedViewsRepository
            .create(&conn, &ctx, tenant, owner, plan_view("Shared Name"))
            .await
            .unwrap();
        let other = PlanRef {
            repo_id: plan().repo_id,
            plan_path: "plans/regression/plan.yaml".to_owned(),
        };
        let on_regression = OrmSavedViewsRepository
            .create(
                &conn,
                &ctx,
                tenant,
                owner,
                NewSavedView {
                    plan_path: Some(other.plan_path.clone()),
                    ..plan_view("Shared Name")
                },
            )
            .await
            .expect("the same name on a different plan is a different key");
        assert_ne!(on_smoke.id, on_regression.id);

        assert_eq!(
            probe_shared_name(&conn, &ctx, tenant, owner, &plan()).await,
            Some(on_smoke.id),
            "the smoke probe must find the smoke view"
        );
        assert_eq!(
            probe_shared_name(&conn, &ctx, tenant, owner, &other).await,
            Some(on_regression.id),
            "and the regression probe the regression one; without plan_key both \
             probes return whichever row the engine happened to order first"
        );
    }

    /// **`update` has to rewrite `plan_key`, and this is what says so.**
    ///
    /// Moving a view from global to plan-scoped and leaving the old `''` key
    /// behind is the second half of obligation #2: the row would then be
    /// invisible to the plan list it now claims to belong to, and would still
    /// collide with a global view of the same name.
    #[tokio::test]
    async fn updating_a_view_into_a_plan_scope_rewrites_its_plan_key() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let owner = Uuid::from_u128(0xB);
        let ctx = scope(tenant);

        let view = OrmSavedViewsRepository
            .create(&conn, &ctx, tenant, owner, global_view("My View"))
            .await
            .unwrap();

        let moved = OrmSavedViewsRepository
            .update(&conn, &ctx, view.id, plan_view("My View"))
            .await
            .unwrap()
            .expect("the view must still be there");
        assert_eq!(moved.scope, SavedViewScope::Plan);

        assert_eq!(
            OrmSavedViewsRepository
                .list(&conn, &ctx, SavedViewScope::Plan, Some(&plan()))
                .await
                .unwrap()
                .iter()
                .map(|v| v.id)
                .collect::<Vec<_>>(),
            vec![view.id],
            "a stale plan_key leaves the row out of the list it now belongs to"
        );
        assert!(
            OrmSavedViewsRepository
                .list(&conn, &ctx, SavedViewScope::All, None)
                .await
                .unwrap()
                .is_empty(),
            "and it must have left the global list"
        );
    }

    /// The probe that drives the create-or-update decision has to derive
    /// `plan_key` the same way the writers do, or it answers about a row the
    /// write would not have produced.
    #[tokio::test]
    async fn the_natural_key_probe_distinguishes_a_plan_scoped_view_from_a_global_one() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let owner = Uuid::from_u128(0xB);
        let ctx = scope(tenant);

        let scoped = OrmSavedViewsRepository
            .create(&conn, &ctx, tenant, owner, plan_view("My View"))
            .await
            .unwrap();

        let plan = plan();
        let found = OrmSavedViewsRepository
            .find_by_natural_key(
                &conn,
                &ctx,
                tenant,
                SavedViewKey {
                    owner_id: owner,
                    scope: SavedViewScope::Plan,
                    plan: Some(&plan),
                    name: "My View",
                },
            )
            .await
            .unwrap();
        assert_eq!(found.map(|v| v.id), Some(scoped.id));

        let global = OrmSavedViewsRepository
            .find_by_natural_key(
                &conn,
                &ctx,
                tenant,
                SavedViewKey {
                    owner_id: owner,
                    scope: SavedViewScope::All,
                    plan: None,
                    name: "My View",
                },
            )
            .await
            .unwrap();
        assert!(
            global.is_none(),
            "the plan-scoped view must not answer a global probe: {global:?}"
        );
    }

    /// `delete` reports whether a row matched, and says nothing about why one
    /// did not — "gone", "never existed" and "not yours" are deliberately
    /// indistinguishable.
    #[tokio::test]
    async fn deleting_a_view_reports_whether_a_row_matched() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let owner = Uuid::from_u128(0xB);
        let ctx = scope(tenant);

        let view = OrmSavedViewsRepository
            .create(&conn, &ctx, tenant, owner, global_view("My View"))
            .await
            .unwrap();

        assert!(
            OrmSavedViewsRepository
                .delete(&conn, &ctx, view.id)
                .await
                .unwrap()
        );
        assert!(
            !OrmSavedViewsRepository
                .delete(&conn, &ctx, view.id)
                .await
                .unwrap(),
            "a second delete matches nothing"
        );
    }

    /// A view stored by one tenant is invisible to another, read back through
    /// the repository rather than asserted about the column.
    ///
    /// This is where `AccessScope::allow_all()` would be tempting and is not
    /// used: the property that matters is *invisibility*, and an allow-all read
    /// cannot express it. (The one call to that constructor this crate's
    /// production code sanctions is `domain::elevated::enumeration_scope`, the
    /// nil-tenant ticker enumeration — an unrelated read over a different
    /// table, not a saved view.)
    #[tokio::test]
    async fn a_view_is_invisible_to_another_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);
        let owner = Uuid::from_u128(0xC);

        OrmSavedViewsRepository
            .create(&conn, &scope(mine), mine, owner, global_view("My View"))
            .await
            .unwrap();

        assert!(
            OrmSavedViewsRepository
                .list(&conn, &scope(theirs), SavedViewScope::All, None)
                .await
                .unwrap()
                .is_empty(),
            "another tenant must see nothing"
        );
    }
}
