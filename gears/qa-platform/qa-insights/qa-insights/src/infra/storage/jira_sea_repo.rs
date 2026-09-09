//! `SecureORM` implementation of [`JiraRepository`].
//!
//! This is the gear's *own* registry of bugs filed against tests, distinct from
//! `qa_test_results.jira_key`, which is whatever reference the runner happened
//! to report on a file.
//!
//! # Three tables, not one, since Task 32
//!
//! `qa_jira_bugs` plus the two per-tenant configuration singletons,
//! `qa_jira_config` and `qa_jira_poller_config`. The trait's own header carries
//! why the settings landed on the bug repository rather than on a repository of
//! their own; the shape to compare against is `notify_sea_repo`, which has owned
//! `qa_notification_config` beside its claims and its log since Task 12.
//!
//! **Nothing in this file ever sees a JIRA token.**
//! `qa_jira_config.api_token_credstore_ref` is a credential-store reference and
//! the material never enters this gear at all — not even to be masked on the way
//! out, which is what legacy does instead (`manager/src/routes/settings.rs:254-259`).

use async_trait::async_trait;
use qa_insights_sdk::{JiraBug, JiraConfig, JiraPollerConfig, NewJiraBug};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureOnConflict, SecureUpdateExt,
    validate_tenant_in_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::analytics::PlanRef;
use crate::domain::error::DomainError;
use crate::domain::repos::JiraRepository;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::jira_bug::{self, Column as BugColumn, Entity as BugEntity};
use crate::infra::storage::entity::jira_config::{
    self, Column as ConfigColumn, Entity as ConfigEntity,
};
use crate::infra::storage::entity::jira_poller_config::{
    self, Column as PollerColumn, Entity as PollerEntity,
};
use crate::infra::storage::mapper::{
    db_i64_from_u64, jira_bug_to_sdk, jira_config_to_sdk, jira_poller_config_to_sdk,
};

/// The `status` value an open bug carries. Legacy's column default
/// (`001_initial.sql:85`) and the predicate `get_all_open_bugs` filters on
/// (`manager/src/services/jira.rs:232-241`).
const STATUS_OPEN: &str = "Open";

/// The `status` value `resolve_bug` writes (`jira.rs:243-251`).
///
/// A literal rather than an enum because the column is free JIRA workflow text —
/// an instance may report anything, and `check_jira_status` (`jira.rs:254`) reads
/// values back from the API. These two constants are the only spellings *this
/// gear* writes.
const STATUS_RESOLVED: &str = "Resolved";

/// The `status` value [`OrmJiraRepository::find_unclosed_for_test`] excludes.
///
/// **This gear never writes this value** — [`STATUS_OPEN`] and
/// [`STATUS_RESOLVED`] are the only two spellings this gear's own writers
/// produce, so a row can only reach `'Closed'` if a caller wrote it directly or
/// a future task (a manual-close endpoint, say) starts to. The literal exists
/// anyway because the *predicate* is legacy's own (`jira.rs:44`,
/// `status != 'Closed'`), ported verbatim per controller ruling R80 — see
/// `JiraRepository::find_unclosed_for_test`'s doc for the divergence this
/// choice declines to make.
const STATUS_CLOSED: &str = "Closed";

/// ORM-based implementation of the `JiraRepository` trait.
#[derive(Clone, Default)]
pub struct OrmJiraRepository;

/// The `ON CONFLICT` clause of the bug registration: the two columns of
/// `idx_qa_jira_bugs_tenant_key`, `DO NOTHING`.
///
/// **The target is named, and it must be.** A bare
/// `OnConflict::new().do_nothing()` renders `ON CONFLICT DO NOTHING` with no
/// target, which swallows *every* constraint violation on the table — including
/// one on the UUID primary key, turning a genuine bug into a re-filed row that
/// looks like the `DO NOTHING` path. Until 2026-08-20 `upsert_bug`'s doc said the
/// target was this index while the statement named none, so the paragraph
/// described an index rather than the SQL being sent.
///
/// A named function rather than an inline literal so
/// `the_bug_conflict_target_names_the_tenant_scoped_index` can observe the value
/// production uses; the same lesson `notify_sea_repo::claim_conflict_target`
/// records.
fn bug_conflict_target() -> OnConflict {
    OnConflict::columns([BugColumn::TenantId, BugColumn::JiraKey])
        .do_nothing()
        .to_owned()
}

#[async_trait]
impl JiraRepository for OrmJiraRepository {
    /// `status = 'Open'`, a literal string match and **not**
    /// `resolved_at IS NULL`.
    ///
    /// The two can disagree — a bug the poller has resolved has both a
    /// `resolved_at` and a `status` of `'Resolved'`, but `status` is free JIRA
    /// workflow text and an instance may report something else entirely. Legacy
    /// keys on `status` (`manager/src/services/jira.rs:232-241`), so this does
    /// too, and `idx_qa_jira_bugs_tenant_status` is the index for it — whose
    /// leading column is the `tenant_id` this read now names.
    ///
    /// **The `tenant_id` predicate — Phase C's final review, Critical 1b.** An
    /// `.all()` rather than a `.one()`, so no row-picking ambiguity, and R86 as
    /// written did not reach it; the trait's own doc carries why it applies
    /// anyway and what the poller did with the extra rows.
    async fn list_open<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<JiraBug>, DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;
        let rows = BugEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(BugColumn::TenantId.eq(tenant_id))
                    .add(BugColumn::Status.eq(STATUS_OPEN)),
            )
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(jira_bug_to_sdk).collect())
    }

    /// Legacy's `get_open_bugs(plan_id)` (`jira.rs:220-229`), keyed on the
    /// `(repo_id, plan_path)` pair rather than legacy's path-derived slug.
    ///
    /// An empty result means the runner's `SKIP_TESTS_WITH_BUGS` variable is
    /// **absent**, not empty — legacy guards that twice (`runs.rs:753-761`,
    /// `argo.rs:477-479`). The rendering belongs to the caller.
    ///
    /// **The `tenant_id` predicate — Phase C's final review, Critical 1b**, for
    /// [`Self::list_open`]'s reason plus the skip list's own: see the trait's
    /// doc for the runner environment an unpinned read leaked into.
    async fn list_open_for_plan<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        plan: &PlanRef,
    ) -> Result<Vec<JiraBug>, DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;
        let rows = BugEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(BugColumn::TenantId.eq(tenant_id))
                    .add(BugColumn::Status.eq(STATUS_OPEN))
                    .add(BugColumn::RepoId.eq(plan.repo_id))
                    .add(BugColumn::PlanPath.eq(plan.plan_path.as_str())),
            )
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(jira_bug_to_sdk).collect())
    }

    /// `INSERT … ON CONFLICT DO NOTHING`, then read back what ended up stored.
    ///
    /// **The `DO NOTHING` is the behaviour, not an optimisation.** Legacy is
    /// `INSERT ... ON CONFLICT (jira_key) DO NOTHING` (`jira.rs:206-215`), and
    /// re-filing an existing key must not overwrite the summary, the status, or a
    /// `resolved_at` the poller has already written — an upsert here would
    /// silently re-open every resolved bug the next time something re-filed it.
    /// `refiling_a_bug_leaves_the_stored_row_untouched` is the guard.
    ///
    /// The read-back is unconditional rather than only on conflict: the return
    /// value is *the row that ended up stored*, which on the conflict path is not
    /// the argument, and a caller cannot tell the two apart without it.
    ///
    /// The conflict target is `idx_qa_jira_bugs_tenant_key` —
    /// `(tenant_id, jira_key)`. Legacy's index is global and a JIRA key is not a
    /// secret, so without the tenant prefix one tenant filing `VHP-1` would
    /// permanently deny it to every other tenant *and* learn from the error
    /// whether someone else had filed it first.
    async fn upsert_bug<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        bug: NewJiraBug,
    ) -> Result<JiraBug, DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;

        let now = OffsetDateTime::now_utc();
        let key = bug.jira_key.clone();
        let am = jira_bug::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            jira_key: ActiveValue::Set(bug.jira_key),
            test_name: ActiveValue::Set(bug.test_name),
            repo_id: ActiveValue::Set(bug.repo_id),
            plan_path: ActiveValue::Set(bug.plan_path),
            app_version: ActiveValue::Set(bug.app_version),
            environment_id: ActiveValue::Set(bug.platform_id),
            // Legacy's column default, spelled here because this gear writes
            // every column explicitly rather than relying on DDL defaults.
            status: ActiveValue::Set(STATUS_OPEN.to_owned()),
            summary: ActiveValue::Set(bug.summary),
            // `NewJiraBug` carries none: `resolved_at` is the poller's to write.
            resolved_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        let result = BugEntity::insert(am)
            .secure()
            .scope_unchecked(scope)
            .map_err(db_err)?
            .on_conflict_raw(bug_conflict_target())
            .exec(runner)
            .await;

        // `RecordNotInserted` is what `do_nothing` reports when the key was
        // already filed; that is the `DO NOTHING` path and not an error, and
        // `is_unique_violation` is the belt to that braces for a backend that
        // raises the constraint instead of swallowing it. Any other failure is
        // real.
        if let Err(e) = result
            && !matches!(e, ScopeError::Db(sea_orm::DbErr::RecordNotInserted))
            && !e.is_unique_violation()
        {
            return Err(db_err(e));
        }

        // `BugNotFound` is unreachable in practice: either the insert landed or
        // the key was already there **for this tenant**. It is the honest
        // answer to "the row is gone between two statements of the same
        // transaction" rather than an `expect`, because a repository must not
        // panic on a concurrent delete.
        //
        // `tenant_id` is passed through explicitly — fix round 2, controller
        // ruling R89. Without it, a scope spanning several tenants could make
        // this re-read return a *different* tenant's row for the identical
        // key: the whole reason `idx_qa_jira_bugs_tenant_key` is
        // `(tenant_id, jira_key)` rather than a global unique index is that
        // two tenants filing the same key is expected (see this method's own
        // doc, a few lines up), and `find_by_key`'s own doc names this call
        // site as its most consequential caller.
        self.find_by_key(runner, scope, tenant_id, &key)
            .await?
            .ok_or(DomainError::BugNotFound { key })
    }

    /// `tenant_id = $1 AND test_name = $2 AND status != 'Closed'`, first
    /// match, no `ORDER BY` — legacy's shape for the last two predicates
    /// (`jira.rs:43-49`), see the trait's doc for why this is not `list_open`'s
    /// `status = 'Open'`.
    ///
    /// **The `tenant_id` predicate — fix round 1, Critical 1.** Added for
    /// [`OrmJiraRepository::get_config`]'s reason: this `.one()` carries no
    /// `ORDER BY` either, and a scope over `OWNER_TENANT_ID` may span several
    /// tenants, so without it a parent-tenant grant would make this method
    /// return an arbitrary in-scope tenant's row rather than refusing or
    /// narrowing. `idx_qa_jira_bugs_tenant_test_plan` is `(tenant_id,
    /// test_name, repo_id, plan_path)`; its leading two columns serve this
    /// predicate's leftmost prefix even though this read names neither of the
    /// index's other two columns.
    async fn find_unclosed_for_test<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        test_name: &str,
    ) -> Result<Option<JiraBug>, DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;
        let row = BugEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(BugColumn::TenantId.eq(tenant_id))
                    .add(BugColumn::TestName.eq(test_name))
                    .add(BugColumn::Status.ne(STATUS_CLOSED)),
            )
            .one(runner)
            .await
            .map_err(db_err)?;
        Ok(row.map(jira_bug_to_sdk))
    }

    /// `status = 'Resolved'` and `resolved_at = at`, on an **open** bug.
    ///
    /// Legacy's `resolve_bug` (`jira.rs:243-251`). The resolution is recorded
    /// whether or not auto-rerun is on: legacy calls it at
    /// `manager/src/services/jira_poller.rs:59`, *before* the
    /// `auto_rerun_on_resolve` gate at `:61-63`, so turning the switch off must
    /// not stop bugs closing.
    ///
    /// `at` is the caller's rather than this method's `now()`, so a poller stamps
    /// every bug in one pass with one instant. `updated_at` is this write's,
    /// which is the distinction the two columns exist for.
    ///
    /// `false` when no *open* bug with that key was visible — an already-resolved
    /// bug is not an open bug, so a second resolve matches nothing, and that is
    /// the same answer as a key that was never filed. The repository deliberately
    /// does not say which.
    ///
    /// **The `tenant_id` predicate — Phase C's final review, Critical 1.** The
    /// one R86 instance that is a *write*: this statement sat between two
    /// methods that each spend a paragraph on scopes spanning tenants and
    /// carried neither the parameter nor the predicate, so one tenant's poller
    /// pass resolved every in-scope tenant's row for a colliding `jira_key`.
    /// [`JiraRepository::resolve_bug`]'s own doc carries the full failure
    /// chain; `resolve_bug_only_touches_the_callers_own_tenants_row` is the
    /// guard.
    async fn resolve_bug<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        jira_key: &str,
        at: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;
        let result = BugEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(BugColumn::TenantId.eq(tenant_id))
                    .add(BugColumn::JiraKey.eq(jira_key))
                    .add(BugColumn::Status.eq(STATUS_OPEN)),
            )
            .col_expr(BugColumn::Status, Expr::value(STATUS_RESOLVED))
            .col_expr(BugColumn::ResolvedAt, Expr::value(at))
            .col_expr(BugColumn::UpdatedAt, Expr::value(OffsetDateTime::now_utc()))
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected > 0)
    }

    /// `tenant_id = $1 AND jira_key = $2`, through `idx_qa_jira_bugs_tenant_key`.
    /// No `status` predicate: a caller looking a key up wants the row whatever
    /// state it is in.
    ///
    /// **The `tenant_id` predicate — fix round 2, controller ruling R89.**
    /// [`Self::find_unclosed_for_test`]'s exact reasoning: this `.one()`
    /// carries no `ORDER BY`, and `jira_key` is not scoped to this gear's own
    /// tenants — two tenants can and, per [`Self::upsert_bug`]'s own doc, are
    /// *expected* to produce the same key. [`Self::upsert_bug`]'s re-read is
    /// this method's most consequential caller and is on the production path
    /// `POST /qa/v1/jira/bugs` reaches on every call, so an unpinned read here
    /// could hand one tenant's newly-filed issue back with another tenant's
    /// `id`/`summary`/`status`.
    async fn find_by_key<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        jira_key: &str,
    ) -> Result<Option<JiraBug>, DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;
        let row = BugEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(BugColumn::TenantId.eq(tenant_id))
                    .add(BugColumn::JiraKey.eq(jira_key)),
            )
            .one(runner)
            .await
            .map_err(db_err)?;
        Ok(row.map(jira_bug_to_sdk))
    }

    // -- The two configuration singletons ------------------------------------

    /// The named tenant's one `qa_jira_config` row, or `None`.
    ///
    /// **The `tenant_id` predicate is applied here in addition to the compiled
    /// scope, and it is what makes the `.one()` unambiguous** — fix round 1,
    /// finding 4. `idx_qa_jira_config_tenant` guarantees at most one row *per
    /// tenant*, not one row per scope, and a scope over
    /// `OWNER_TENANT_ID` may legitimately span several tenants (`ScopeFilter::In`,
    /// `ScopeFilter::InTenantSubtree`). Without this filter the `.one()` — which
    /// carries no `ORDER BY` either — returns whichever in-scope row the engine
    /// hands back first. The trait's own doc carries what that cost
    /// `save_jira_config`.
    ///
    /// The scope is still applied in full; this **narrows**, never widens, and
    /// `validate_tenant_in_scope` refuses a tenant the caller has no grant for
    /// rather than silently returning `None`.
    async fn get_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<JiraConfig>, DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;
        let row = ConfigEntity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(ConfigColumn::TenantId.eq(tenant_id)))
            .one(runner)
            .await
            .map_err(db_err)?;
        Ok(row.map(jira_config_to_sdk))
    }

    /// An upsert on `idx_qa_jira_config_tenant`, so the tenant keeps exactly one
    /// row.
    ///
    /// Every non-key column is in the update list, which is what makes this
    /// "create or **replace**" — a column omitted from it keeps its old value on
    /// a save that looked like it stored the whole form, which is the silent half
    /// of a settings screen that appears to work.
    /// `saving_a_jira_config_round_trips_every_field` is what catches an
    /// omission, and `api_token_credstore_ref` is the entry that matters most:
    /// left out, a tenant repointing the credential would keep authenticating
    /// with the old one.
    async fn save_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        config: JiraConfig,
    ) -> Result<(), DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;

        let now = OffsetDateTime::now_utc();
        let am = jira_config::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            url: ActiveValue::Set(config.url),
            project_key: ActiveValue::Set(config.project_key),
            email: ActiveValue::Set(config.email),
            api_token_credstore_ref: ActiveValue::Set(config.api_token_credstore_ref),
            issue_type: ActiveValue::Set(config.issue_type),
            enabled: ActiveValue::Set(config.enabled),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        let on_conflict = SecureOnConflict::<ConfigEntity>::columns([ConfigColumn::TenantId])
            .update_columns([
                ConfigColumn::Url,
                ConfigColumn::ProjectKey,
                ConfigColumn::Email,
                ConfigColumn::ApiTokenCredstoreRef,
                ConfigColumn::IssueType,
                ConfigColumn::Enabled,
                ConfigColumn::UpdatedAt,
            ])
            .map_err(db_err)?;

        ConfigEntity::insert(am)
            .secure()
            .scope_unchecked(scope)
            .map_err(db_err)?
            .on_conflict(on_conflict)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// The named tenant's one `qa_jira_poller_config` row, or `None`.
    ///
    /// Same explicit `tenant_id` predicate as [`Self::get_config`], for the same
    /// reason. Fallible where that method is not, and only on the interval —
    /// `infra::storage::mapper::jira_poller_config_to_sdk` widens `BIGINT` into
    /// the contract's `u64` and fails closed on a negative rather than wrapping.
    async fn get_poller_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<JiraPollerConfig>, DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;
        let row = PollerEntity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(PollerColumn::TenantId.eq(tenant_id)))
            .one(runner)
            .await
            .map_err(db_err)?;
        row.as_ref().map(jira_poller_config_to_sdk).transpose()
    }

    /// An upsert on `idx_qa_jira_poller_config_tenant`.
    ///
    /// The interval is narrowed back to `BIGINT` by
    /// `infra::storage::mapper::db_i64_from_u64`, which **refuses** rather than
    /// clamps: a value silently changed on save is a settings screen that lies
    /// about what it stored. The `.max(1)` legacy applies belongs to the reader
    /// that sleeps, not to this write — `domain::service::jira`'s header carries
    /// the split.
    async fn save_poller_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        config: JiraPollerConfig,
    ) -> Result<(), DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;

        let now = OffsetDateTime::now_utc();
        let am = jira_poller_config::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            poll_interval_seconds: ActiveValue::Set(db_i64_from_u64(
                config.poll_interval_seconds,
                "poll_interval_seconds",
            )?),
            auto_rerun_on_resolve: ActiveValue::Set(config.auto_rerun_on_resolve),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        let on_conflict = SecureOnConflict::<PollerEntity>::columns([PollerColumn::TenantId])
            .update_columns([
                PollerColumn::PollIntervalSeconds,
                PollerColumn::AutoRerunOnResolve,
                PollerColumn::UpdatedAt,
            ])
            .map_err(db_err)?;

        PollerEntity::insert(am)
            .secure()
            .scope_unchecked(scope)
            .map_err(db_err)?
            .on_conflict(on_conflict)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use qa_insights_sdk::{JiraConfig, NewJiraBug};
    use uuid::Uuid;

    use crate::domain::analytics::PlanRef;
    use crate::domain::error::DomainError;
    use crate::domain::repos::JiraRepository;
    use crate::infra::storage::entity::jira_bug::{self, Entity as BugEntity};
    use crate::infra::storage::entity::jira_poller_config::{self, Entity as PollerEntity};
    use crate::infra::storage::jira_sea_repo::OrmJiraRepository;
    use crate::infra::storage::test_db::{inmem_db, now, scope};
    use sea_orm::{ActiveValue, EntityTrait};
    use toolkit_db::secure::{AccessScope, SecureInsertExt};

    fn plan() -> PlanRef {
        PlanRef {
            repo_id: Uuid::from_u128(0x12),
            plan_path: "plans/smoke/plan.yaml".to_owned(),
        }
    }

    fn bug(key: &str, summary: &str) -> NewJiraBug {
        let plan = plan();
        NewJiraBug {
            jira_key: key.to_owned(),
            test_name: "AuthN Login".to_owned(),
            repo_id: plan.repo_id,
            plan_path: plan.plan_path,
            app_version: Some("5.0.1".to_owned()),
            platform_id: Some(Uuid::from_u128(0x11)),
            summary: summary.to_owned(),
        }
    }

    /// **`DO NOTHING` is the behaviour, not an optimisation.**
    ///
    /// Legacy is `INSERT ... ON CONFLICT (jira_key) DO NOTHING`
    /// (`manager/src/services/jira.rs:206-215`): re-filing an existing key must
    /// not overwrite the summary, the status or a `resolved_at` the poller has
    /// already written. An implementation that upserted instead would silently
    /// re-open every resolved bug the next time something re-filed it.
    #[tokio::test]
    async fn refiling_a_bug_leaves_the_stored_row_untouched() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);

        let first = OrmJiraRepository
            .upsert_bug(&conn, &ctx, tenant, bug("VHP-1", "login is broken"))
            .await
            .unwrap();
        assert!(
            OrmJiraRepository
                .resolve_bug(&conn, &ctx, tenant, "VHP-1", now())
                .await
                .unwrap()
        );

        let again = OrmJiraRepository
            .upsert_bug(&conn, &ctx, tenant, bug("VHP-1", "a different summary"))
            .await
            .expect("re-filing must return the stored row rather than failing");

        assert_eq!(again.id, first.id, "the same row, not a second one");
        assert_eq!(
            again.summary, "login is broken",
            "the stored summary must survive a re-file"
        );
        assert_eq!(again.status, "Resolved", "and so must the resolution");
        assert!(again.resolved_at.is_some());
    }

    /// **The bug registration's conflict target names the tenant-scoped index.**
    ///
    /// A bare `ON CONFLICT DO NOTHING` would swallow every constraint violation
    /// on `qa_jira_bugs`, primary key included. Asserted on the clause after
    /// `ON CONFLICT` rather than on the whole statement, because both column
    /// names also appear in the INSERT's own column list — an assertion over the
    /// full SQL passes for a bare target and proves nothing.
    #[test]
    fn the_bug_conflict_target_names_the_tenant_scoped_index() {
        use sea_orm::{DatabaseBackend, EntityTrait, QueryTrait};
        use toolkit_db::secure::SecureInsertExt;

        for backend in [DatabaseBackend::Postgres, DatabaseBackend::Sqlite] {
            let sql = BugEntity::insert(jira_bug::ActiveModel {
                id: ActiveValue::Set(Uuid::from_u128(1)),
                tenant_id: ActiveValue::Set(Uuid::from_u128(0xA)),
                jira_key: ActiveValue::Set("VHP-1".to_owned()),
                test_name: ActiveValue::Set("AuthN Login".to_owned()),
                repo_id: ActiveValue::Set(Uuid::from_u128(0x12)),
                plan_path: ActiveValue::Set("plans/smoke/plan.yaml".to_owned()),
                app_version: ActiveValue::Set(None),
                environment_id: ActiveValue::Set(None),
                status: ActiveValue::Set(super::STATUS_OPEN.to_owned()),
                summary: ActiveValue::Set("s".to_owned()),
                resolved_at: ActiveValue::Set(None),
                created_at: ActiveValue::Set(now()),
                updated_at: ActiveValue::Set(now()),
            })
            .secure()
            .scope_unchecked(&scope(Uuid::from_u128(0xA)))
            .unwrap()
            .on_conflict_raw(super::bug_conflict_target())
            .into_inner()
            .build(backend)
            .to_string();

            let clause = sql
                .split_once("ON CONFLICT")
                .expect("the statement must have an ON CONFLICT clause")
                .1;
            for column in ["tenant_id", "jira_key"] {
                assert!(
                    clause.contains(column),
                    "{backend:?} must name {column} in the conflict target; \
                     clause was: {clause}"
                );
            }
        }
    }

    /// `list_open` keys on `status = 'Open'`, a literal string match rather than
    /// `resolved_at IS NULL` — legacy's `get_all_open_bugs`
    /// (`manager/src/services/jira.rs:232-241`). The two can disagree, and legacy
    /// keys on `status`, so this does too.
    #[tokio::test]
    async fn a_resolved_bug_leaves_the_open_list() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);

        OrmJiraRepository
            .upsert_bug(&conn, &ctx, tenant, bug("VHP-1", "one"))
            .await
            .unwrap();
        OrmJiraRepository
            .upsert_bug(&conn, &ctx, tenant, bug("VHP-2", "two"))
            .await
            .unwrap();
        assert_eq!(
            OrmJiraRepository
                .list_open(&conn, &ctx, tenant)
                .await
                .unwrap()
                .len(),
            2
        );

        OrmJiraRepository
            .resolve_bug(&conn, &ctx, tenant, "VHP-1", now())
            .await
            .unwrap();

        let open = OrmJiraRepository
            .list_open(&conn, &ctx, tenant)
            .await
            .unwrap();
        assert_eq!(
            open.iter().map(|b| b.jira_key.as_str()).collect::<Vec<_>>(),
            vec!["VHP-2"]
        );
    }

    /// **Controller ruling R80, pinned.** `find_unclosed_for_test`'s predicate
    /// is `status != 'Closed'`, not `list_open`'s `status = 'Open'` — the two
    /// disagree on exactly a resolved-but-not-closed bug, and this is the test
    /// that goes red if a future edit "simplifies" the probe onto `list_open`'s
    /// predicate instead. See `JiraRepository::find_unclosed_for_test`'s doc for
    /// the full argument and the user-visible consequence.
    #[tokio::test]
    async fn a_resolved_bug_still_blocks_the_local_refile_probe() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);

        OrmJiraRepository
            .upsert_bug(&conn, &ctx, tenant, bug("VHP-1", "login is broken"))
            .await
            .unwrap();
        OrmJiraRepository
            .resolve_bug(&conn, &ctx, tenant, "VHP-1", now())
            .await
            .unwrap();

        assert!(
            OrmJiraRepository
                .list_open(&conn, &ctx, tenant)
                .await
                .unwrap()
                .is_empty(),
            "the open list must no longer carry a resolved bug"
        );

        let found = OrmJiraRepository
            .find_unclosed_for_test(&conn, &ctx, tenant, "AuthN Login")
            .await
            .unwrap()
            .expect(
                "a resolved-but-not-closed bug must still match the local re-file probe - \
                 'Resolved' != 'Closed'",
            );
        assert_eq!(found.jira_key, "VHP-1");
        assert_eq!(found.status, "Resolved");
    }

    /// The other half of R80's predicate: a bug actually marked `'Closed'` is
    /// excluded. Nothing this gear writes produces that value today
    /// (`STATUS_CLOSED`'s own doc), so the row is inserted directly, the same
    /// way `the_bug_conflict_target_names_the_tenant_scoped_index` bypasses the
    /// trait to construct a shape the trait cannot write.
    #[tokio::test]
    async fn a_closed_bug_is_excluded_from_the_local_refile_probe() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);
        let plan = plan();

        BugEntity::insert(jira_bug::ActiveModel {
            id: ActiveValue::Set(Uuid::from_u128(9)),
            tenant_id: ActiveValue::Set(tenant),
            jira_key: ActiveValue::Set("VHP-9".to_owned()),
            test_name: ActiveValue::Set("AuthN Login".to_owned()),
            repo_id: ActiveValue::Set(plan.repo_id),
            plan_path: ActiveValue::Set(plan.plan_path),
            app_version: ActiveValue::Set(None),
            environment_id: ActiveValue::Set(None),
            status: ActiveValue::Set(super::STATUS_CLOSED.to_owned()),
            summary: ActiveValue::Set("s".to_owned()),
            resolved_at: ActiveValue::Set(Some(now())),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        })
        .secure()
        .scope_unchecked(&ctx)
        .unwrap()
        .exec(&conn)
        .await
        .unwrap();

        let found = OrmJiraRepository
            .find_unclosed_for_test(&conn, &ctx, tenant, "AuthN Login")
            .await
            .unwrap();
        assert!(
            found.is_none(),
            "a Closed bug must not block a re-file: {found:?}"
        );
    }

    /// Resolving reports whether an open bug matched, and a second resolve of the
    /// same key matches nothing — `resolve_bug`'s predicate includes
    /// `status = 'Open'`, as legacy's does (`jira.rs:243-251`).
    #[tokio::test]
    async fn resolving_reports_whether_an_open_bug_matched() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);

        OrmJiraRepository
            .upsert_bug(&conn, &ctx, tenant, bug("VHP-1", "one"))
            .await
            .unwrap();

        assert!(
            OrmJiraRepository
                .resolve_bug(&conn, &ctx, tenant, "VHP-1", now())
                .await
                .unwrap()
        );
        assert!(
            !OrmJiraRepository
                .resolve_bug(&conn, &ctx, tenant, "VHP-1", now())
                .await
                .unwrap(),
            "an already-resolved bug is not an open bug"
        );
        assert!(
            !OrmJiraRepository
                .resolve_bug(&conn, &ctx, tenant, "VHP-404", now())
                .await
                .unwrap(),
            "and neither is one that was never filed"
        );
    }

    /// The plan-scoped list keys on the `(repo_id, plan_path)` pair, which is
    /// lossless where legacy's path-derived `plan_id` slug is not.
    #[tokio::test]
    async fn the_plan_scoped_open_list_keys_on_the_repo_and_path_pair() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);

        OrmJiraRepository
            .upsert_bug(&conn, &ctx, tenant, bug("VHP-1", "one"))
            .await
            .unwrap();
        let other_plan = NewJiraBug {
            plan_path: "plans/regression/plan.yaml".to_owned(),
            ..bug("VHP-2", "two")
        };
        OrmJiraRepository
            .upsert_bug(&conn, &ctx, tenant, other_plan)
            .await
            .unwrap();

        let open = OrmJiraRepository
            .list_open_for_plan(&conn, &ctx, tenant, &plan())
            .await
            .unwrap();
        assert_eq!(
            open.iter().map(|b| b.jira_key.as_str()).collect::<Vec<_>>(),
            vec!["VHP-1"],
            "a bug on another path of the same repository is not on this plan"
        );
    }

    /// **A JIRA key is unique per tenant, not globally**, which legacy's index is
    /// not (`001_initial.sql:80`). Without the tenant prefix one tenant filing
    /// `VHP-1` would permanently deny it to every other tenant *and* learn from
    /// the error whether someone else had filed it first.
    #[tokio::test]
    async fn two_tenants_may_each_file_the_same_jira_key() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);

        let ours = OrmJiraRepository
            .upsert_bug(&conn, &scope(mine), mine, bug("VHP-1", "ours"))
            .await
            .unwrap();
        let yours = OrmJiraRepository
            .upsert_bug(&conn, &scope(theirs), theirs, bug("VHP-1", "theirs"))
            .await
            .expect("another tenant must be able to file the same key");

        assert_ne!(ours.id, yours.id);
        assert_eq!(yours.summary, "theirs");
        assert_eq!(
            OrmJiraRepository
                .find_by_key(&conn, &scope(mine), mine, "VHP-1")
                .await
                .unwrap()
                .map(|b| b.summary),
            Some("ours".to_owned()),
            "and neither sees the other's"
        );
    }

    /// **Controller ruling R89, fix round 2.** Under a scope spanning both
    /// tenants, `find_by_key` must return the *caller's* row for a shared key,
    /// not whichever of the two identically-keyed rows the engine happens to
    /// return first — this is `upsert_bug`'s own last statement
    /// (`OrmJiraRepository::upsert_bug`'s doc names it as its most
    /// consequential caller), pinned directly rather than through it.
    ///
    /// **Pinned on `find_by_key` directly, not through `upsert_bug`, per the
    /// finding's own instruction — and the ordering below is measured, not
    /// guessed.** A first version of this test re-filed through `upsert_bug`
    /// (hitting its `DO NOTHING` path so the return value was entirely the
    /// re-read's answer) with `mine = 0xA`, `theirs = 0xB`, `mine` inserted
    /// first. Removing the `tenant_id` predicate did **not** turn it red: the
    /// query still resolves through `idx_qa_jira_bugs_tenant_key`
    /// `(tenant_id, jira_key)`, whose leaf order is ascending `tenant_id`, so
    /// `SQLite` returned the lower-`tenant_id` row (`mine`'s, `0xA < 0xB`)
    /// regardless of the removed predicate or of insertion order — a false
    /// negative, not a passing fix. Swapping the tenant ids so `mine`'s is the
    /// **higher** of the two (`0xB`) made the same mutation turn it red
    /// deterministically: with the predicate gone, the index scan for a
    /// shared key returns the lower-`tenant_id` row first, which is `theirs`,
    /// so a caller asking as `mine` gets the wrong row back exactly when the
    /// fix is missing. Insertion order was not the load-bearing variable; the
    /// tenant id ordering is, and the values below are chosen for it, not for
    /// which is inserted first.
    ///
    /// A genuine multi-tenant `AccessScope` is built directly (`for_tenants`)
    /// rather than reusing `test_db::scope`, which only ever compiles a
    /// single-tenant scope — the same distinction
    /// `domain::service::jira_tests`' `TwoTenantAuthZ` exists for.
    #[tokio::test]
    async fn find_by_key_returns_the_callers_own_row_under_a_multi_tenant_scope() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        // `mine` is deliberately the numerically larger id — see this test's
        // own doc for why that, and not insertion order, is what makes the
        // covering mutation deterministic under `idx_qa_jira_bugs_tenant_key`.
        let mine = Uuid::from_u128(0xB);
        let theirs = Uuid::from_u128(0xA);

        OrmJiraRepository
            .upsert_bug(&conn, &scope(theirs), theirs, bug("VHP-1", "theirs"))
            .await
            .unwrap();
        OrmJiraRepository
            .upsert_bug(&conn, &scope(mine), mine, bug("VHP-1", "mine"))
            .await
            .unwrap();

        let spanning = AccessScope::for_tenants(vec![mine, theirs]);

        let found = OrmJiraRepository
            .find_by_key(&conn, &spanning, mine, "VHP-1")
            .await
            .unwrap()
            .expect("a row must be found under the spanning scope");

        assert_eq!(
            found.summary, "mine",
            "the caller's own row must come back, not the other tenant's - {found:?}",
        );
    }

    /// **Controller ruling R86, Phase C's final review, Critical 1 — and the
    /// first R86 guard in this file over a *write*.** Under a scope spanning
    /// both tenants, `resolve_bug` must resolve only the caller's own row for a
    /// shared `jira_key`, never every in-scope tenant's.
    ///
    /// # Why this one needs no ordinal or timestamp engineering
    ///
    /// [`find_by_key_returns_the_callers_own_row_under_a_multi_tenant_scope`]
    /// above had to make the caller's tenant id the numerically larger of the
    /// two, because its `.one()` read carries no `ORDER BY` and an un-predicated
    /// version returns *one* row — whichever the index scan reaches first — so
    /// the covering mutation is only visible for one of the two orderings. The
    /// sibling guard in `results_sea_repo`
    /// (`latest_version_for_plan_returns_the_callers_own_tenant_under_a_multi_tenant_scope`)
    /// needed the same care in timestamp form, separating its two tenants by a
    /// full day of `run_finished_at`.
    ///
    /// Neither applies here, and stating why is the point: this is an
    /// `update_many`. With the `tenant_id` predicate removed it writes **both**
    /// rows rather than picking one, so the assertion on `theirs` below turns
    /// red for every choice of tenant ids and every insertion order. There is no
    /// ordering for a bulk write to be lucky about — which is also why an R86
    /// sweep phrased around `.one()` never looked at it.
    #[tokio::test]
    async fn resolve_bug_only_touches_the_callers_own_tenants_row() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        // The same ordering the `find_by_key` guard above needs, kept only so
        // the two tests read alike; this one does not depend on it.
        let mine = Uuid::from_u128(0xB);
        let theirs = Uuid::from_u128(0xA);

        OrmJiraRepository
            .upsert_bug(&conn, &scope(theirs), theirs, bug("VHP-1", "theirs"))
            .await
            .unwrap();
        OrmJiraRepository
            .upsert_bug(&conn, &scope(mine), mine, bug("VHP-1", "mine"))
            .await
            .unwrap();

        let spanning = AccessScope::for_tenants(vec![mine, theirs]);

        assert!(
            OrmJiraRepository
                .resolve_bug(&conn, &spanning, mine, "VHP-1", now())
                .await
                .unwrap(),
            "the caller's own open bug must be the one that matched"
        );

        let ours = OrmJiraRepository
            .find_by_key(&conn, &scope(mine), mine, "VHP-1")
            .await
            .unwrap()
            .expect("the caller's row must still exist");
        assert_eq!(ours.status, "Resolved");
        assert!(ours.resolved_at.is_some());

        let foreign = OrmJiraRepository
            .find_by_key(&conn, &scope(theirs), theirs, "VHP-1")
            .await
            .unwrap()
            .expect("the other tenant's row must still exist");
        assert_eq!(
            foreign.status, "Open",
            "another tenant's bug against a different JIRA instance must not be resolved by \
             this tenant's poller pass - {foreign:?}",
        );
        assert!(
            foreign.resolved_at.is_none(),
            "and it must carry no resolution instant either - {foreign:?}",
        );
    }

    /// **Controller ruling R86, Phase C's final review, Critical 1b.** Under a
    /// scope spanning both tenants, the two open-bug listings must answer with
    /// the caller's own rows only.
    ///
    /// `.all()` reads, so there is no row-picking to engineer here either: with
    /// either `tenant_id` predicate removed the foreign row simply appears in
    /// the returned `Vec`. What makes this worth a test rather than an obvious
    /// property is *who* consumes the two lists — `JiraPollerService::poll_once`
    /// and the SDK skip-list provider, both single-tenant by contract; the
    /// trait's own doc carries that argument.
    #[tokio::test]
    async fn the_open_bug_listings_are_pinned_to_the_callers_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xB);
        let theirs = Uuid::from_u128(0xA);

        OrmJiraRepository
            .upsert_bug(&conn, &scope(theirs), theirs, bug("VHP-1", "theirs"))
            .await
            .unwrap();
        OrmJiraRepository
            .upsert_bug(&conn, &scope(mine), mine, bug("VHP-2", "mine"))
            .await
            .unwrap();

        let spanning = AccessScope::for_tenants(vec![mine, theirs]);

        let all_open = OrmJiraRepository
            .list_open(&conn, &spanning, mine)
            .await
            .unwrap();
        assert_eq!(
            all_open
                .iter()
                .map(|b| b.jira_key.as_str())
                .collect::<Vec<_>>(),
            vec!["VHP-2"],
            "the whole-tenant listing must not enumerate another in-scope tenant's bugs",
        );

        let plan_open = OrmJiraRepository
            .list_open_for_plan(&conn, &spanning, mine, &plan())
            .await
            .unwrap();
        assert_eq!(
            plan_open
                .iter()
                .map(|b| b.jira_key.as_str())
                .collect::<Vec<_>>(),
            vec!["VHP-2"],
            "and neither must the per-plan listing the runner's skip list is built from",
        );
    }

    // -- The two configuration singletons ------------------------------------

    /// **A negative stored interval is corruption, not an enormous positive
    /// one.**
    ///
    /// The column is `BIGINT` and the contract field is `u64`; an `as` cast
    /// would turn `-1` into 18 446 744 073 709 551 615 seconds — roughly 585
    /// billion years — so a poller reading it would sleep once and never poll
    /// again. A *silent stop*, with no failure anywhere to notice it.
    ///
    /// # What makes this test able to fail
    ///
    /// The row is written **through the entity, not through `save_poller_config`**,
    /// because that method's own `db_i64_from_u64` refuses a value it cannot
    /// represent and so can never produce this row. A test that saved through the
    /// repository could not construct the input the widening exists for, which is
    /// the defect class this plan has shipped before.
    #[tokio::test]
    async fn a_negative_stored_poll_interval_is_corrupt_state_and_not_a_wrapped_huge_number() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);

        PollerEntity::insert(jira_poller_config::ActiveModel {
            id: ActiveValue::Set(Uuid::from_u128(0x101)),
            tenant_id: ActiveValue::Set(tenant),
            poll_interval_seconds: ActiveValue::Set(-1),
            auto_rerun_on_resolve: ActiveValue::Set(true),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        })
        .secure()
        .scope_unchecked(&scope(tenant))
        .unwrap()
        .exec(&conn)
        .await
        .unwrap();

        let err = OrmJiraRepository
            .get_poller_config(&conn, &scope(tenant), tenant)
            .await
            .expect_err("a negative interval must fail closed");
        assert!(
            matches!(
                err,
                DomainError::CorruptState {
                    what: "jira_poller_config.poll_interval_seconds",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    /// Both singletons are one row per tenant and invisible across tenants.
    ///
    /// Read back under the *other* tenant's scope rather than through an
    /// `allow_all()` ground-truth read — this module's `test_db::scope` states
    /// why.
    #[tokio::test]
    async fn each_tenants_jira_settings_are_its_own() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);

        let config = |project: &str| JiraConfig {
            url: "https://jira.example.com".to_owned(),
            project_key: project.to_owned(),
            email: "qa@example.com".to_owned(),
            api_token_credstore_ref: "qa-jira-token".to_owned(),
            issue_type: None,
            enabled: true,
        };

        OrmJiraRepository
            .save_config(&conn, &scope(mine), mine, config("MINE"))
            .await
            .unwrap();
        OrmJiraRepository
            .save_config(&conn, &scope(theirs), theirs, config("THEIRS"))
            .await
            .unwrap();
        // A second save for the same tenant must replace, not duplicate:
        // `idx_qa_jira_config_tenant`. Without the upsert this would raise a
        // unique violation here rather than at the read below.
        OrmJiraRepository
            .save_config(&conn, &scope(mine), mine, config("MINE-AGAIN"))
            .await
            .unwrap();

        assert_eq!(
            OrmJiraRepository
                .get_config(&conn, &scope(mine), mine)
                .await
                .unwrap()
                .map(|c| c.project_key),
            Some("MINE-AGAIN".to_owned()),
        );
        assert_eq!(
            OrmJiraRepository
                .get_config(&conn, &scope(theirs), theirs)
                .await
                .unwrap()
                .map(|c| c.project_key),
            Some("THEIRS".to_owned()),
            "one tenant's save must not have replaced the other's row",
        );
    }

    /// Writing another tenant's settings is refused by `validate_tenant_in_scope`
    /// before any statement runs — the same guard every write in this crate
    /// applies.
    #[tokio::test]
    async fn saving_settings_for_a_tenant_outside_the_scope_is_refused() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);

        let err = OrmJiraRepository
            .save_poller_config(
                &conn,
                &scope(mine),
                theirs,
                qa_insights_sdk::JiraPollerConfig::default(),
            )
            .await
            .expect_err("a caller scoped to one tenant cannot write another's settings");
        assert!(matches!(err, DomainError::Database { .. }), "{err:?}");
        assert_eq!(
            OrmJiraRepository
                .get_poller_config(&conn, &scope(theirs), theirs)
                .await
                .unwrap(),
            None,
            "and nothing was written",
        );
    }
}
