//! `SecureORM` implementation of [`SchedulesRepository`].
//!
//! Every chain here is **filter-first**: `.filter(..)` before
//! `.secure().scope_with(scope)`, matching the two sibling repositories. The
//! order is not cosmetic — `scope_with` is what appends the tenant predicate,
//! and a chain that scoped first and filtered afterwards reads as though the
//! filter could replace the scope.
//!
//! Both tables are reached under one scope, because they are one resource type;
//! see the trait module's header for why that does not relax the
//! one-scope-per-resource-type rule.

use async_trait::async_trait;
use qa_runs_sdk::{NewSchedule, Schedule, ScheduleNotificationSettings};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
    secure_update_with_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{OwnedScheduleId, SchedulesRepository};
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::schedule::{
    ActiveModel as ScheduleAM, Column as ScheduleColumn, Entity as ScheduleEntity,
};
use crate::infra::storage::entity::schedule_tick::{
    ActiveModel as TickAM, Column as TickColumn, Entity as TickEntity,
};
use crate::infra::storage::mapper::{
    exclusive_choice_to_column, json_to_column, parameters_to_column, schedule_to_sdk,
    target_to_columns,
};

/// ORM-based implementation of the [`SchedulesRepository`] trait.
#[derive(Clone, Default)]
pub struct OrmSchedulesRepository;

/// `WHERE id = $1` on `qa_schedules`, as a condition the scoped chains prepend.
///
/// Used by **every** chain in this file that addresses a schedule by id,
/// including the two that also carry a guard — an inlined copy beside a helper
/// with the same job reads as though the difference were meaningful.
fn by_id(id: Uuid) -> Condition {
    Condition::all().add(ScheduleColumn::Id.eq(id))
}

/// `WHERE id = $1` on `qa_schedule_ticks`.
///
/// A separate function rather than a generic over `ColumnTrait`: the two tables
/// have distinct `Column` enums, and the whole value of the pair is that a
/// schedule id cannot be filtered against the tick table by mistake.
fn tick_by_id(id: Uuid) -> Condition {
    Condition::all().add(TickColumn::Id.eq(id))
}

/// The caller-decidable columns of a schedule, shared by the insert and the
/// replace so the two cannot disagree about which fields a [`NewSchedule`]
/// governs.
///
/// Returns a partially-filled `ActiveModel`: `id`, `tenant_id`,
/// `last_fired_tick`, `created_at` and `updated_at` are left
/// [`ActiveValue::NotSet`] for the caller to decide, because each is owned by
/// this layer and none of them is a field [`NewSchedule`] has.
fn schedule_columns(new: &NewSchedule) -> Result<ScheduleAM, DomainError> {
    let columns = target_to_columns(&new.target);
    Ok(ScheduleAM {
        name: ActiveValue::Set(new.name.clone()),
        run_kind: ActiveValue::Set(new.target.kind().as_str().to_owned()),
        target_repo_id: ActiveValue::Set(columns.repo_id),
        target_path: ActiveValue::Set(columns.path),
        target_test_file: ActiveValue::Set(columns.test_file),
        target_custom_plan_id: ActiveValue::Set(columns.custom_plan_id),
        // Named explicitly, even though it is `None` for every target a
        // schedule can hold: this literal ends in `..Default::default()`, which
        // absorbs a new column **without a compile error** and would have
        // written `NULL` over a collect target's URL.
        // `m20260818_000006_collect_target` records why the column exists on
        // this table at all.
        //
        // # The unreachable state is load-bearing, and here is what would make
        // it reachable
        //
        // `ScheduleService::validate` refuses `RunKind::Collect` on create and
        // on update, so no collect schedule can be written through the service
        // and this expression is `None` on every production path today. That is
        // **not** a reason to drop the column or to replace this line with a
        // literal `None`: this repository is the codec's other half, and the
        // moment either of these becomes true it has to store the value —
        //
        // 1. `ScheduleService::validate`'s kind guard is removed or relaxed
        //    (it exists because a scheduled collect would fire
        //    admission-bypassing launches on a cron, not because the shape is
        //    unrepresentable); or
        // 2. a second writer appears that does not go through
        //    `ScheduleService` — a migration backfill, an import, a repository
        //    test seeding rows directly.
        //
        // A hard-coded `None` would survive both silently and turn a collect
        // schedule into a `CorruptState` on read, because `target_from_columns`
        // requires the column to be present for that kind. Reading the value
        // through means the guard above is the only thing deciding policy, and
        // deleting it changes behaviour rather than corrupting rows.
        target_collect_url: ActiveValue::Set(columns.collect_url),
        environment_id: ActiveValue::Set(new.platform_id),
        branch: ActiveValue::Set(new.branch.clone()),
        cron: ActiveValue::Set(new.cron.clone()),
        // Always written, including for the `None` case — that is the whole
        // point of the tri-state; see `mapper::exclusive_choice_to_column`.
        exclusive_choice: ActiveValue::Set(
            exclusive_choice_to_column(new.exclusive_choice).to_owned(),
        ),
        enabled: ActiveValue::Set(new.enabled),
        include_tags: ActiveValue::Set(json_to_column("schedule.include_tags", &new.include_tags)?),
        exclude_tags: ActiveValue::Set(json_to_column("schedule.exclude_tags", &new.exclude_tags)?),
        parameters: ActiveValue::Set(parameters_to_column(&new.parameters)?),
        // # The three notification columns are **deliberately** left `NotSet`
        //
        // `..Default::default()` below expands to `NotSet` for every field not
        // named above, so these three would be absent from this literal whether
        // or not anybody thought about them. Naming them is the difference
        // between a decision and an oversight — and this file has already been
        // bitten once, at `target_collect_url` above.
        //
        // `NotSet` means two different things in the two callers of this
        // function, and **both** are the wanted behaviour:
        //
        // * On the `INSERT`, `SeaORM` omits a `NotSet` column from the statement
        //   entirely, so the row takes the database defaults the migration
        //   declares — `FALSE`, `NULL`, `'[]'`. That is what legacy's create
        //   does too: `CreateScheduleForm`'s three slack fields are all
        //   `#[serde(default)]` (`manager/src/models.rs:189-194`), so a create
        //   that does not mention them creates a schedule that notifies nobody.
        // * On the `UPDATE`, a `NotSet` column is not in the `SET` list at all,
        //   so a **full replace leaves the notification settings standing**.
        //
        // That second one is a decision worth stating, because it is the point
        // at which this port and the source system's wire format differ. Legacy's
        // full edit takes a `CreateScheduleForm` whose slack fields default, so a
        // client that omitted them would clear them — and legacy's own UI
        // therefore has to re-send them on every edit, seeding the form from the
        // schedule being edited
        // (`manager-ui/src/components/schedules/CreateScheduleDialog.tsx:305-309`).
        // The *observable* legacy behaviour is "an edit preserves the Slack
        // settings"; here that is a property of the statement rather than of the
        // client's diligence. Making it a property of the client is exactly the
        // silent-drop shape this subsystem keeps finding.
        //
        // `SchedulesRepository::update_notifications` is the only writer.
        slack_notifications_enabled: ActiveValue::NotSet,
        slack_channel: ActiveValue::NotSet,
        slack_notification_events: ActiveValue::NotSet,
        ..Default::default()
    })
}

/// Encode [`ScheduleNotificationSettings`] into its three columns.
///
/// Separate from [`schedule_columns`] rather than a branch inside it: the two
/// payloads govern disjoint column sets, and a single function taking both would
/// have to express "do not touch the other half" as an `Option`, which is the
/// shape that makes a dropped write look like a deliberate skip.
///
/// # Errors
///
/// [`DomainError::Internal`] if the event list fails to encode, which for a list
/// of strings cannot happen — surfaced rather than unwrapped for the reason
/// [`json_to_column`] gives.
fn notification_columns(
    settings: &ScheduleNotificationSettings,
) -> Result<ScheduleAM, DomainError> {
    Ok(ScheduleAM {
        slack_notifications_enabled: ActiveValue::Set(settings.slack_enabled),
        slack_channel: ActiveValue::Set(settings.slack_channel.clone()),
        // Always written, never omitted for the empty case: the column is
        // `NOT NULL` and an operator clearing their event list must be able to
        // clear it. `json_to_column` produces `[]`, which is the column default
        // and is also what "notify on nothing" means.
        slack_notification_events: ActiveValue::Set(json_to_column(
            "schedule.slack_notification_events",
            &settings.slack_events,
        )?),
        ..Default::default()
    })
}

#[async_trait]
impl SchedulesRepository for OrmSchedulesRepository {
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewSchedule,
    ) -> Result<Schedule, DomainError> {
        // Kept for the unique-violation error, which reports the taken name.
        let name = new.name.clone();
        let now = OffsetDateTime::now_utc();

        let am = ScheduleAM {
            // Minted here, never taken from the caller: `qa_schedules.id` is a
            // global primary key, so honouring a supplied one would make this
            // an existence oracle. `NewSchedule` has no `id` field precisely so
            // that this is not a rule anybody has to remember.
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            // A schedule has fired nothing yet. Spelled rather than left to the
            // column default, because `SeaORM` names every column in its
            // `INSERT` and there is no "leave it to the default" option.
            last_fired_tick: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
            ..schedule_columns(&new)?
        };

        // The INSERTED model rather than a locally built struct: the two agree
        // today, but only the former would observe a database-side default.
        match secure_insert::<ScheduleEntity>(am, scope, runner).await {
            Ok(model) => schedule_to_sdk(model),
            // `idx_qa_schedules_tenant_name` is tenant-prefixed, so a collision
            // is always with a row this tenant can itself see and the name is
            // safe to report. The other unique index on this table is the
            // *global* primary key, and that branch is unreachable only because
            // the id is minted above — the same argument, and the same
            // dependency, that `OrmRunsRepository::create` records.
            Err(e) if e.is_unique_violation() => Err(DomainError::ScheduleNameExists { name }),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Schedule>, DomainError> {
        let found = ScheduleEntity::find()
            .filter(by_id(id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        found.map(schedule_to_sdk).transpose()
    }

    async fn get_by_name<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        name: &str,
    ) -> Result<Option<Schedule>, DomainError> {
        let found = ScheduleEntity::find()
            .filter(Condition::all().add(ScheduleColumn::Name.eq(name)))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        found.map(schedule_to_sdk).transpose()
    }

    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Schedule>, DomainError> {
        let rows = ScheduleEntity::find()
            .secure()
            .scope_with(scope)
            .order_by(ScheduleColumn::Name, sea_orm::Order::Asc)
            .all(runner)
            .await
            .map_err(db_err)?;
        rows.into_iter().map(schedule_to_sdk).collect()
    }

    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        new: NewSchedule,
    ) -> Result<Option<Schedule>, DomainError> {
        let name = new.name.clone();

        let existing = ScheduleEntity::find()
            .filter(by_id(id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        let Some(existing) = existing else {
            return Ok(None);
        };

        let am = ScheduleAM {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            // `Unchanged`, so it is not in the generated `SET` list at all. The
            // cron evaluator's cursor must survive an edit: resetting it would
            // re-fire every due time since the schedule last ran, and an edit
            // does not un-fire the past. `advance_last_fired_tick` is its only
            // writer.
            last_fired_tick: ActiveValue::Unchanged(existing.last_fired_tick),
            created_at: ActiveValue::Unchanged(existing.created_at),
            updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
            ..schedule_columns(&new)?
        };

        match secure_update_with_scope::<ScheduleEntity>(am, scope, id, runner).await {
            Ok(model) => schedule_to_sdk(model).map(Some),
            Err(e) if e.is_unique_violation() => Err(DomainError::ScheduleNameExists { name }),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn update_notifications<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        settings: ScheduleNotificationSettings,
    ) -> Result<Option<Schedule>, DomainError> {
        // The same scoped read-then-update shape as `update`, and scoped for the
        // same reason: `secure_update_with_scope` re-reads under the scope too,
        // but a caller must be able to tell "no such schedule *for you*" from a
        // denial, and only `Ok(None)` says that.
        let existing = ScheduleEntity::find()
            .filter(by_id(id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        let Some(existing) = existing else {
            return Ok(None);
        };

        let am = ScheduleAM {
            id: ActiveValue::Unchanged(existing.id),
            tenant_id: ActiveValue::Unchanged(existing.tenant_id),
            created_at: ActiveValue::Unchanged(existing.created_at),
            updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
            // Everything else stays `NotSet` and is therefore absent from the
            // `SET` list. **That is the whole guarantee of this method** and the
            // half of legacy's handler that is behaviour rather than annotation
            // plumbing: *"this endpoint edits Slack settings only, so a schedule
            // pinned to exclusive (or to parallel) must come back pinned the same
            // way"* (`manager/src/routes/schedules.rs:854-856`). Legacy needs a
            // whole `CreateScheduleForm` copied field by field to achieve it,
            // because it deletes and recreates the `CronWorkflow`; here the
            // fields are simply not in the statement.
            //
            // It is also what keeps this method away from
            // `ScheduleService::validate`'s `RunKind::Collect` refusal: `run_kind`
            // and the five `target_*` columns are `NotSet`, so no call to this
            // method can change a schedule's kind or target, and the guard has
            // nothing to be snuck past.
            ..notification_columns(&settings)?
        };

        match secure_update_with_scope::<ScheduleEntity>(am, scope, id, runner).await {
            Ok(model) => schedule_to_sdk(model).map(Some),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        // No second statement for the tick rows: `ON DELETE CASCADE` reaps
        // them, which is also the only ordering that cannot leave orphans
        // behind on a failure between two statements.
        let result = ScheduleEntity::delete_many()
            .filter(by_id(id))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected > 0)
    }

    async fn list_enabled<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<(Schedule, Uuid)>, DomainError> {
        let rows = ScheduleEntity::find()
            .filter(Condition::all().add(ScheduleColumn::Enabled.eq(true)))
            .secure()
            .scope_with(scope)
            // `id`, so a cross-tenant enumeration has one deterministic order
            // rather than interleaving two tenants' schedules by name. Nothing
            // depends on *which* order; something does depend on it being
            // stable, because a ticker that logged its work would otherwise
            // report a different sequence every pass.
            .order_by(ScheduleColumn::Id, sea_orm::Order::Asc)
            .all(runner)
            .await
            .map_err(db_err)?;

        // **Skip a corrupt row; do not fail the pass.** `collect::<Result<..>>()`
        // here would abort the whole cross-tenant enumeration on the first
        // undecodable row, so every tenant's schedules stop firing until someone
        // repairs it — and nothing self-heals, because `advance_last_fired_tick`
        // is never reached either. A rolling upgrade is the reachable trigger:
        // an older replica's decoder meeting a newer replica's value would take
        // the scheduler dark for the deploy. Skipping still fails closed for the
        // offending schedule; it just stops one row from being fleet-wide. See
        // the trait method.
        Ok(rows
            .into_iter()
            .filter_map(|m| {
                let (id, tenant_id) = (m.id, m.tenant_id);
                match schedule_to_sdk(m) {
                    Ok(schedule) => Some((schedule, tenant_id)),
                    Err(error) => {
                        tracing::warn!(
                            schedule_id = %id,
                            %tenant_id,
                            %error,
                            "skipping an undecodable schedule row; it will not fire until \
                             it is repaired, and the rest of the fleet continues"
                        );
                        None
                    }
                }
            })
            .collect())
    }

    async fn claim_tick<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        schedule: OwnedScheduleId,
        due_at: OffsetDateTime,
        claimed_by: &str,
    ) -> Result<Option<Uuid>, DomainError> {
        let id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();

        let am = TickAM {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant_id),
            // An `OwnedScheduleId`, so the tenant-blind foreign key below can
            // never be the thing that answers whether this schedule exists.
            schedule_id: ActiveValue::Set(schedule.get()),
            due_at: ActiveValue::Set(due_at),
            claimed_by: ActiveValue::Set(claimed_by.to_owned()),
            claimed_at: ActiveValue::Set(now),
            // Written once, later, by `record_tick_outcome`.
            run_id: ActiveValue::Set(None),
            error: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now),
        };

        match secure_insert::<TickEntity>(am, scope, runner).await {
            Ok(model) => Ok(Some(model.id)),
            // **The exactly-once mechanism, and the normal path.** Somebody
            // else holds this claim; this instance must not launch. Mapped to
            // `Ok(None)` rather than to `Database` — a lost race happens on
            // every non-leader instance and every leader failover, so surfacing
            // it as a 500 would report correct behaviour as a fault.
            //
            // `idx_qa_schedule_ticks_claim` is the only unique index a claim can
            // violate other than the primary key, and the primary key is minted
            // here — the same dependency `create` above records.
            //
            // The classification is `is_unique_violation`'s and not this call
            // site's, which is a narrower claim than "not by matching driver
            // text": that helper prefers the parsed SQLSTATE but falls back to
            // substring matching on the rendered message. So a non-unique fault
            // carrying one of those phrases becomes a silently skipped fire. See
            // the trait method; the arm below is what keeps everything else an
            // error.
            Err(e) if e.is_unique_violation() => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn record_tick_outcome<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tick_id: Uuid,
        run_id: Option<Uuid>,
        error: Option<&str>,
    ) -> Result<bool, DomainError> {
        let result = TickEntity::update_many()
            .filter(tick_by_id(tick_id))
            .secure()
            .scope_with(scope)
            // Both columns unconditionally: this is the row's single post-claim
            // write and both are NULL until it happens, so there is nothing an
            // omitted column could preserve. No `updated_at` — the tick table
            // deliberately has none; see its entity.
            .col_expr(TickColumn::RunId, Expr::value(run_id))
            .col_expr(TickColumn::Error, Expr::value(error.map(str::to_owned)))
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected == 1)
    }

    async fn advance_last_fired_tick<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        due_at: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        let result = ScheduleEntity::update_many()
            .filter(
                by_id(id)
                    // Monotonic: the cursor may only move forward. Without this
                    // a late tick landing after an early one would rewind it and
                    // the evaluator would recompute due times it had already
                    // fired. `IS NULL` is the never-fired case, which SQL's
                    // three-valued logic would otherwise drop from a bare `<`.
                    .add(
                        Condition::any()
                            .add(ScheduleColumn::LastFiredTick.is_null())
                            .add(ScheduleColumn::LastFiredTick.lt(due_at)),
                    ),
            )
            .secure()
            .scope_with(scope)
            .col_expr(ScheduleColumn::LastFiredTick, Expr::value(due_at))
            .col_expr(
                ScheduleColumn::UpdatedAt,
                Expr::value(OffsetDateTime::now_utc()),
            )
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected == 1)
    }
}

/// DB-backed tests: the only thing that actually exercises these queries.
///
/// `cargo build` checks none of it — the table names, every column name, every
/// `WHERE` and every `SET` in this file are runtime strings. Each test below
/// runs against the real migration on an in-memory `SQLite` database, through
/// the real `SecureORM` scoping.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use qa_runs_sdk::{RunParameter, RunTarget};

    use super::*;
    use crate::infra::storage::test_db::{inmem_db, now, sample_new_schedule, scope};

    /// A ground-truth scope admitting exactly `tenants`, for driving
    /// `list_enabled` and `resolve_owned_schedule` across more than one
    /// tenant at once — **not** a model of what the firing ticker's own
    /// nil-tenant context compiles to. That context no longer reaches the
    /// PDP at all: it elevates through `domain::elevated::enumeration_scope`
    /// (`AccessScope::allow_all()`), so it compiles to nothing here. This
    /// function exists so these DB-backed tests can still pin the row-level
    /// scoping `list_enabled`'s production caller relies on being correct,
    /// against a boundary an unrestricted scope cannot exercise.
    ///
    /// `AccessScope::for_tenants`, **not** `allow_all()` — the ban on
    /// `allow_all()` (`no_production_path_uses_allow_all`) exempts exactly
    /// one file, `domain/elevated.rs`, and this is not it. A ground-truth
    /// read written that way would also defeat the point: it admits every
    /// tenant rather than exactly the two under test, so a query that forgot
    /// its own tenant filter would still pass.
    fn enumeration_scope(tenants: &[Uuid]) -> AccessScope {
        AccessScope::for_tenants(tenants.to_vec())
    }

    /// Mint the ownership token `claim_tick` demands, through the real
    /// `resolve_owned_schedule`.
    ///
    /// Never a shortcut around it: the token's field is private to
    /// `domain::repos::schedules_repo`, so this is the only way a test can get
    /// one — which is the property the type exists for.
    async fn owned(
        repo: &OrmSchedulesRepository,
        conn: &toolkit_db::DbConn<'_>,
        tenant: Uuid,
        schedule_id: Uuid,
    ) -> OwnedScheduleId {
        repo.resolve_owned_schedule(conn, &scope(tenant), schedule_id)
            .await
            .expect("the schedule is visible in its own tenant's scope")
    }

    /// Every field of a fully-populated schedule survives the round trip —
    /// all three JSON columns, the flattened target, and the tri-state.
    #[tokio::test]
    async fn a_schedule_round_trips_through_the_database() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;
        let new = sample_new_schedule("nightly");

        let created = repo
            .create(&conn, &scope(tenant), tenant, new.clone())
            .await
            .unwrap();

        // Spelled out field by field rather than with `..`: this is the one
        // test that pins the whole `NewSchedule` -> row -> `Schedule` mapping,
        // and a struct-update would let a transposed pair of same-typed fields
        // through.
        let expected = Schedule {
            id: created.id,
            name: new.name.clone(),
            target: new.target.clone(),
            platform_id: new.platform_id,
            branch: new.branch.clone(),
            cron: new.cron.clone(),
            exclusive_choice: new.exclusive_choice,
            enabled: new.enabled,
            include_tags: new.include_tags.clone(),
            exclude_tags: new.exclude_tags.clone(),
            parameters: new.parameters.clone(),
            // Owned by this layer and **not** settable through `NewSchedule`: a
            // created schedule notifies nobody until
            // `update_notifications` says otherwise, which is what the column
            // defaults give and what legacy's all-`#[serde(default)]` create
            // form gives (`manager/src/models.rs:189-194`). Asserted here rather
            // than left to `..`, so an accidental write from `schedule_columns`
            // would redden this test.
            slack_notifications_enabled: false,
            slack_channel: None,
            slack_notification_events: Vec::new(),
            // Owned by this layer; a new schedule has fired nothing.
            last_fired_tick: None,
            created_at: created.created_at,
            updated_at: created.updated_at,
        };
        assert_eq!(
            created, expected,
            "the inserted model must decode back to the input"
        );
        assert_eq!(created.created_at, created.updated_at);

        // ...and it reads back the same way through a fresh SELECT, which is
        // what proves the columns were actually written and not just echoed.
        assert_eq!(
            repo.get(&conn, &scope(tenant), created.id).await.unwrap(),
            Some(expected.clone())
        );
        assert_eq!(
            repo.get_by_name(&conn, &scope(tenant), "nightly")
                .await
                .unwrap(),
            Some(expected)
        );
        assert_eq!(repo.list(&conn, &scope(tenant)).await.unwrap().len(), 1);

        // Another tenant sees none of it, by id or by name.
        let other = Uuid::new_v4();
        assert!(
            repo.get(&conn, &scope(other), created.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.get_by_name(&conn, &scope(other), "nightly")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// **The notification codec, both directions, all three columns, at three
    /// distinct non-default values.**
    ///
    /// The codec has two halves — `notification_columns` here writes, and
    /// `mapper::schedule_to_sdk` reads — and both are struct literals over a
    /// growing column set. A literal that drops one of the three still compiles,
    /// still passes every other test in this crate, and silently loses a user's
    /// Slack settings on every read or write. Nothing but an assertion on all
    /// three values catches it.
    ///
    /// Every value is non-default and distinguishable from every other:
    /// `true` where the column defaults `false`, a channel where it defaults
    /// `NULL`, and a **two-element, ordered** event list where it defaults `[]`.
    /// A fixture with one event could not tell a preserved list from a truncated
    /// one; a fixture with `["error", "failed"]` sorted could not tell a
    /// preserved order from a sorted one.
    ///
    /// Read back through a **fresh `SELECT`** as well as off the returned model,
    /// which is what proves the columns were written rather than echoed.
    ///
    /// Break-verified, one line at a time, and the **counts are the measured
    /// ones** rather than the "and nothing else" this kind of note usually
    /// claims:
    ///
    /// * Deleting `slack_channel` from `notification_columns` (the write half)
    ///   reddens **four** tests — this one, both service-level notification
    ///   tests, and the handler test.
    /// * Hard-coding `slack_channel: None` in `mapper::schedule_to_sdk` (the
    ///   read half) reddens the same four. *Deleting* the line there does not
    ///   compile at all, because that literal has no `..`.
    /// * Hard-coding `slack_notifications_enabled: false` there: the same four.
    ///   Replacing the events decode with `Vec::new()`: those four plus
    ///   `an_event_outside_legacys_vocabulary_is_refused`.
    ///
    /// Four rather than one is the wanted result — a dropped column should be
    /// visible from several angles — but it is worth writing down, because a
    /// note claiming "and nothing else" would be the thing a later reader
    /// checks and finds false.
    #[tokio::test]
    async fn every_notification_setting_survives_the_column() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        let created = repo
            .create(
                &conn,
                &scope(tenant),
                tenant,
                sample_new_schedule("nightly"),
            )
            .await
            .unwrap();
        // The defaults first, so the assertions below cannot pass by reading
        // back what was already there.
        assert!(!created.slack_notifications_enabled);
        assert_eq!(created.slack_channel, None);
        assert!(created.slack_notification_events.is_empty());

        let settings = ScheduleNotificationSettings {
            slack_enabled: true,
            slack_channel: Some("#qa-alerts".to_owned()),
            slack_events: vec!["failed".to_owned(), "error".to_owned()],
        };
        let updated = repo
            .update_notifications(&conn, &scope(tenant), created.id, settings.clone())
            .await
            .unwrap()
            .expect("the schedule is visible in its own tenant's scope");

        assert!(updated.slack_notifications_enabled);
        assert_eq!(updated.slack_channel.as_deref(), Some("#qa-alerts"));
        assert_eq!(updated.slack_notification_events, vec!["failed", "error"]);

        let reread = repo
            .get(&conn, &scope(tenant), created.id)
            .await
            .unwrap()
            .expect("the row must still be there");
        assert_eq!(
            reread, updated,
            "a fresh SELECT must agree with the model the UPDATE returned"
        );

        // And the write touched nothing else on the row.
        assert_eq!(
            (
                reread.name.as_str(),
                reread.cron.as_str(),
                reread.exclusive_choice,
                reread.enabled,
                &reread.include_tags,
                &reread.exclude_tags,
                &reread.parameters,
                &reread.target,
            ),
            (
                created.name.as_str(),
                created.cron.as_str(),
                created.exclusive_choice,
                created.enabled,
                &created.include_tags,
                &created.exclude_tags,
                &created.parameters,
                &created.target,
            ),
            "`update_notifications` puts three columns in the SET list and no others"
        );

        // Clearing is expressible: the column is NOT NULL, so "no events" has
        // to be an empty array rather than an absent value.
        let cleared = repo
            .update_notifications(
                &conn,
                &scope(tenant),
                created.id,
                ScheduleNotificationSettings::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!cleared.slack_notifications_enabled);
        assert_eq!(cleared.slack_channel, None);
        assert!(cleared.slack_notification_events.is_empty());
    }

    /// Another tenant cannot reach the notification columns, by id.
    ///
    /// `Ok(None)`, never a write and never an error: the repository filters
    /// before it scopes, so a foreign id matches no row. Asserted at this layer
    /// as well as at the service's, because this is where the `WHERE` clause
    /// actually is — the service test proves the scope is derived, this one
    /// proves it is applied.
    #[tokio::test]
    async fn another_tenant_cannot_edit_notification_settings() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let owner = Uuid::new_v4();
        let stranger = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        let created = repo
            .create(&conn, &scope(owner), owner, sample_new_schedule("nightly"))
            .await
            .unwrap();

        assert!(
            repo.update_notifications(
                &conn,
                &scope(stranger),
                created.id,
                ScheduleNotificationSettings {
                    slack_enabled: true,
                    slack_channel: Some("#stranger".to_owned()),
                    slack_events: vec!["failed".to_owned()],
                },
            )
            .await
            .unwrap()
            .is_none(),
            "a foreign schedule must match no row"
        );

        let mine = repo
            .get(&conn, &scope(owner), created.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!mine.slack_notifications_enabled, "the row is untouched");
        assert_eq!(mine.slack_channel, None);
        assert!(mine.slack_notification_events.is_empty());
    }

    /// All three tri-state values survive a real column, which the mapper's own
    /// round-trip test cannot show: that one never touches a database.
    #[tokio::test]
    async fn every_exclusive_choice_survives_the_column() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        for (index, choice) in [Some(true), Some(false), None].into_iter().enumerate() {
            let mut new = sample_new_schedule(&format!("nightly-{index}"));
            new.exclusive_choice = choice;
            let created = repo
                .create(&conn, &scope(tenant), tenant, new)
                .await
                .unwrap();
            assert_eq!(
                repo.get(&conn, &scope(tenant), created.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .exclusive_choice,
                choice,
                "the tri-state must survive the column, including the `auto` case"
            );
        }
    }

    #[tokio::test]
    async fn a_duplicate_schedule_name_in_one_tenant_conflicts() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        repo.create(
            &conn,
            &scope(tenant),
            tenant,
            sample_new_schedule("nightly"),
        )
        .await
        .unwrap();
        let err = repo
            .create(
                &conn,
                &scope(tenant),
                tenant,
                sample_new_schedule("nightly"),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DomainError::ScheduleNameExists { name } if name == "nightly"),
            "got {err:?}"
        );
    }

    /// Renaming onto a taken name is the same conflict from the other side, and
    /// it is the arm `update` would otherwise report as a bare `Database` 500.
    #[tokio::test]
    async fn renaming_a_schedule_onto_a_taken_name_conflicts() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        repo.create(
            &conn,
            &scope(tenant),
            tenant,
            sample_new_schedule("nightly"),
        )
        .await
        .unwrap();
        let weekly = repo
            .create(&conn, &scope(tenant), tenant, sample_new_schedule("weekly"))
            .await
            .unwrap();

        let err = repo
            .update(
                &conn,
                &scope(tenant),
                weekly.id,
                sample_new_schedule("nightly"),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DomainError::ScheduleNameExists { name } if name == "nightly"),
            "got {err:?}"
        );
    }

    /// The tenant-prefixed name index, stated as the property it buys.
    #[tokio::test]
    async fn the_same_schedule_name_in_two_tenants_is_fine() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmSchedulesRepository;

        repo.create(&conn, &scope(a), a, sample_new_schedule("nightly"))
            .await
            .unwrap();
        repo.create(&conn, &scope(b), b, sample_new_schedule("nightly"))
            .await
            .expect("schedule names are namespaced by tenant");
    }

    /// A replace writes every caller-decidable field and **preserves the
    /// evaluator's cursor**.
    ///
    /// The cursor half is the one worth having: `last_fired_tick` is absent from
    /// `NewSchedule`, so an implementation that built its `ActiveModel` from
    /// scratch would silently reset it to NULL and re-fire every due time since
    /// the schedule last ran. Break-verified — setting that field to
    /// `ActiveValue::Set(None)` in `update` turns this red. It also reds
    /// `api::rest::handlers::schedules`' handler-level cursor test, which
    /// arrived later and covers the same column through `replace_schedule`;
    /// this one is still the only thing pinning the repository itself.
    #[tokio::test]
    async fn an_update_replaces_every_field_but_the_fired_cursor() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        let created = repo
            .create(
                &conn,
                &scope(tenant),
                tenant,
                sample_new_schedule("nightly"),
            )
            .await
            .unwrap();
        assert!(
            repo.advance_last_fired_tick(&conn, &scope(tenant), created.id, now())
                .await
                .unwrap()
        );

        let mut replacement = sample_new_schedule("nightly-renamed");
        replacement.target = RunTarget::CustomPlan {
            id: Uuid::from_u128(0x99),
        };
        replacement.cron = "*/15 * * * *".to_owned();
        replacement.exclusive_choice = Some(true);
        replacement.enabled = false;
        replacement.branch = None;
        replacement.include_tags = vec!["regression".to_owned()];
        replacement.exclude_tags = vec![];
        replacement.parameters = vec![];

        let updated = repo
            .update(&conn, &scope(tenant), created.id, replacement.clone())
            .await
            .unwrap()
            .expect("the schedule is visible in this scope");

        assert_eq!(updated.id, created.id);
        assert_eq!(updated.name, "nightly-renamed");
        assert_eq!(updated.target, replacement.target);
        assert_eq!(updated.cron, "*/15 * * * *");
        assert_eq!(updated.exclusive_choice, Some(true));
        assert!(!updated.enabled);
        assert_eq!(updated.branch, None);
        assert_eq!(updated.include_tags, vec!["regression".to_owned()]);
        assert!(updated.exclude_tags.is_empty());
        assert!(updated.parameters.is_empty());
        assert_eq!(
            updated.last_fired_tick,
            Some(now()),
            "an edit must not rewind the cron evaluator's cursor"
        );
        assert_eq!(updated.created_at, created.created_at);
        assert!(updated.updated_at >= created.updated_at);
    }

    /// A schedule another tenant owns is neither readable nor writable, and the
    /// write answers `None`/`false` rather than distinguishing absent from
    /// foreign.
    #[tokio::test]
    async fn another_tenants_schedule_is_invisible_and_unwritable() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmSchedulesRepository;

        let schedule = repo
            .create(&conn, &scope(a), a, sample_new_schedule("nightly"))
            .await
            .unwrap();

        assert!(
            repo.get(&conn, &scope(b), schedule.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(repo.list(&conn, &scope(b)).await.unwrap().is_empty());
        assert!(
            repo.update(
                &conn,
                &scope(b),
                schedule.id,
                sample_new_schedule("hijacked"),
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(!repo.delete(&conn, &scope(b), schedule.id).await.unwrap());
        assert!(
            !repo
                .advance_last_fired_tick(&conn, &scope(b), schedule.id, now())
                .await
                .unwrap()
        );

        let untouched = repo
            .get(&conn, &scope(a), schedule.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(untouched.name, "nightly");
        assert_eq!(untouched.last_fired_tick, None);
    }

    /// **The exactly-once claim.** The second attempt at the same
    /// `(tenant, schedule, due time)` answers `Ok(None)`, not an error.
    ///
    /// That distinction is the whole test: `Ok(None)` is what tells the losing
    /// instance not to launch, while a `Database` error would be a 500 on the
    /// normal path — every non-leader instance and every leader failover
    /// produces one of these.
    ///
    /// The third assertion is the one that keeps the second honest: a
    /// *different* due time on the same schedule must still be claimable, so
    /// the constraint is on the tuple and not on the schedule.
    #[tokio::test]
    async fn claiming_a_tick_twice_returns_none_the_second_time() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        let schedule = repo
            .create(
                &conn,
                &scope(tenant),
                tenant,
                sample_new_schedule("nightly"),
            )
            .await
            .unwrap();

        let first = repo
            .claim_tick(
                &conn,
                &scope(tenant),
                tenant,
                owned(&repo, &conn, tenant, schedule.id).await,
                now(),
                "qa-runs-0",
            )
            .await
            .unwrap();
        let tick_id = first.expect("the first claim on a due time must win");

        let second = repo
            .claim_tick(
                &conn,
                &scope(tenant),
                tenant,
                owned(&repo, &conn, tenant, schedule.id).await,
                now(),
                "qa-runs-1",
            )
            .await
            .expect("a lost race is Ok(None), never a Database error");
        assert_eq!(
            second, None,
            "the second claim on one due time must lose, and must lose quietly"
        );

        let later = repo
            .claim_tick(
                &conn,
                &scope(tenant),
                tenant,
                owned(&repo, &conn, tenant, schedule.id).await,
                now() + time::Duration::hours(1),
                "qa-runs-1",
            )
            .await
            .unwrap();
        assert!(
            later.is_some_and(|id| id != tick_id),
            "a different due time is a different claim"
        );
    }

    /// The claim index is tenant-prefixed, so two tenants racing on the *same*
    /// `(schedule_id, due_at)` do not collide — **and** the ownership token
    /// closes the oracle that made the race reachable in the first place.
    ///
    /// Two properties, in the order they now apply:
    ///
    /// 1. **Tenant B cannot mint a token for tenant A's schedule under its own
    ///    scope.** `resolve_owned_schedule` answers
    ///    [`DomainError::ScheduleNotFound`], indistinguishable from a schedule
    ///    that does not exist, so the tenant-blind foreign key never gets to
    ///    answer the existence question. This is what [`OwnedScheduleId`] buys
    ///    and it did not hold before the token existed.
    /// 2. **A broad enough scope still reaches the row-level case**, so the
    ///    index prefix is still load-bearing. A token minted under a
    ///    *multi-tenant* scope — the shape the firing ticker's nil-tenant
    ///    enumeration compiles to — carries no tenant of its own, so B can
    ///    present it while writing under `scope(b)` with `tenant_id = b`. The
    ///    insert is accepted, which is the good outcome: a tenant-blind claim
    ///    index would instead let B squat every one of A's future ticks and
    ///    would answer, by succeeding or failing, whether A had already fired.
    ///
    /// So the token narrows *who* can reach the FK; it does not remove the need
    /// for the prefix. Both defences are separately necessary and both are
    /// asserted here.
    ///
    /// Verified by breaking it: with `idx_qa_schedule_ticks_claim` rewritten to
    /// `(schedule_id, due_at)`, the second claim answers `Ok(None)` and this test
    /// fails. `both_unique_indexes_are_per_tenant_not_global` asserts the same
    /// property one layer down, against raw rows.
    #[tokio::test]
    async fn two_tenants_may_claim_the_same_schedule_id_and_due_at() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmSchedulesRepository;

        let victim = repo
            .create(&conn, &scope(a), a, sample_new_schedule("nightly"))
            .await
            .unwrap();

        // (1) B cannot even name A's schedule under its own scope.
        let refused = repo
            .resolve_owned_schedule(&conn, &scope(b), victim.id)
            .await
            .expect_err("a foreign schedule must not yield an ownership token");
        assert!(
            matches!(refused, DomainError::ScheduleNotFound { id } if id == victim.id),
            "a foreign schedule must be indistinguishable from an absent one; got {refused:?}"
        );

        // (2) The row-level race, reached the only way that is left.
        assert!(
            repo.claim_tick(
                &conn,
                &scope(a),
                a,
                owned(&repo, &conn, a, victim.id).await,
                now(),
                "qa-runs-0",
            )
            .await
            .unwrap()
            .is_some()
        );

        let cross_tenant = repo
            .resolve_owned_schedule(&conn, &enumeration_scope(&[a, b]), victim.id)
            .await
            .expect("an enumeration scope admitting A can resolve A's schedule");
        assert!(
            repo.claim_tick(&conn, &scope(b), b, cross_tenant, now(), "qa-runs-0")
                .await
                .unwrap()
                .is_some(),
            "the claim index is tenant-prefixed, so a second tenant's row for the same \
             (schedule_id, due_at) must be accepted - without the prefix, learning a \
             schedule_id would be enough to squat every one of its ticks",
        );
    }

    /// **A genuine database fault must stay an error**, not be swallowed as a
    /// lost race.
    ///
    /// `claim_tick`'s two error arms differ by one predicate, and collapsing them
    /// to `Err(_) => Ok(None)` leaves every other test in this crate green — so
    /// nothing but this test stands between a real fault and a **silently skipped
    /// fire**, forever, with no log and no error. Break-verified: that mutation
    /// turns this red and nothing else.
    ///
    /// The fault used is the one that is genuinely reachable past the ownership
    /// token: the schedule is deleted *after* the token is minted, so the insert
    /// hits the foreign key. That is a real TOCTOU race — an operator deleting a
    /// schedule while the ticker is mid-fire — and it demonstrates precisely what
    /// [`OwnedScheduleId`] does and does not promise: it closes the existence
    /// oracle, it does not make the reference durable.
    #[tokio::test]
    async fn a_non_unique_failure_is_an_error_not_a_lost_race() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        let schedule = repo
            .create(
                &conn,
                &scope(tenant),
                tenant,
                sample_new_schedule("nightly"),
            )
            .await
            .unwrap();
        let token = owned(&repo, &conn, tenant, schedule.id).await;

        // The delete lands between the precheck and the insert.
        assert!(
            repo.delete(&conn, &scope(tenant), schedule.id)
                .await
                .unwrap()
        );

        let outcome = repo
            .claim_tick(&conn, &scope(tenant), tenant, token, now(), "qa-runs-0")
            .await;
        assert!(
            matches!(outcome, Err(DomainError::Database { .. })),
            "a foreign-key violation is not a lost claim race and must not be reported \
             as one - `Ok(None)` here would silently skip a fire and log nothing; \
             got {outcome:?}"
        );
    }

    /// A won claim records what its launch produced, and the write is scoped.
    #[tokio::test]
    async fn a_tick_records_its_outcome_once() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmSchedulesRepository;

        let schedule = repo
            .create(&conn, &scope(a), a, sample_new_schedule("nightly"))
            .await
            .unwrap();
        let tick = repo
            .claim_tick(
                &conn,
                &scope(a),
                a,
                owned(&repo, &conn, a, schedule.id).await,
                now(),
                "qa-runs-0",
            )
            .await
            .unwrap()
            .unwrap();

        // Another tenant cannot write the outcome of this tick.
        assert!(
            !repo
                .record_tick_outcome(&conn, &scope(b), tick, Some(Uuid::new_v4()), None)
                .await
                .unwrap()
        );

        let run_id = Uuid::new_v4();
        assert!(
            repo.record_tick_outcome(&conn, &scope(a), tick, Some(run_id), None)
                .await
                .unwrap()
        );
        assert_eq!(read_tick(&conn, a, tick).await.run_id, Some(run_id));

        // The failure direction: no run, and a reason.
        assert!(
            repo.record_tick_outcome(&conn, &scope(a), tick, None, Some("launch refused"))
                .await
                .unwrap()
        );
        let stored = read_tick(&conn, a, tick).await;
        assert_eq!(stored.run_id, None);
        assert_eq!(stored.error.as_deref(), Some("launch refused"));

        // A tick that is not there.
        assert!(
            !repo
                .record_tick_outcome(&conn, &scope(a), Uuid::new_v4(), None, None)
                .await
                .unwrap()
        );
    }

    /// The cursor moves forward and refuses to move back.
    ///
    /// The backwards case is not hypothetical: two due times fired out of order
    /// — which a retry or a slow launch produces — would otherwise rewind the
    /// evaluator and make it recompute times it had already fired.
    #[tokio::test]
    async fn the_fired_cursor_only_moves_forward() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        let schedule = repo
            .create(
                &conn,
                &scope(tenant),
                tenant,
                sample_new_schedule("nightly"),
            )
            .await
            .unwrap();
        assert_eq!(
            schedule.last_fired_tick, None,
            "a new schedule has not fired"
        );

        let later = now() + time::Duration::hours(1);
        assert!(
            repo.advance_last_fired_tick(&conn, &scope(tenant), schedule.id, later)
                .await
                .unwrap(),
            "the never-fired case must be reachable: `IS NULL` is not `< $1`"
        );

        assert!(
            !repo
                .advance_last_fired_tick(&conn, &scope(tenant), schedule.id, now())
                .await
                .unwrap(),
            "an earlier due time must not rewind the cursor"
        );
        assert!(
            !repo
                .advance_last_fired_tick(&conn, &scope(tenant), schedule.id, later)
                .await
                .unwrap(),
            "re-advancing to the same instant is a no-op, not a write"
        );
        assert_eq!(
            repo.get(&conn, &scope(tenant), schedule.id)
                .await
                .unwrap()
                .unwrap()
                .last_fired_tick,
            Some(later)
        );
    }

    /// The ticker's enumeration: every enabled schedule, each with the tenant to
    /// mint a per-tenant write context from.
    #[tokio::test]
    async fn list_enabled_carries_each_schedules_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmSchedulesRepository;

        let first = repo
            .create(&conn, &scope(a), a, sample_new_schedule("nightly"))
            .await
            .unwrap();
        let second = repo
            .create(&conn, &scope(b), b, sample_new_schedule("hourly"))
            .await
            .unwrap();

        let found = repo
            .list_enabled(&conn, &enumeration_scope(&[a, b]))
            .await
            .unwrap();
        let mut pairs: Vec<(Uuid, Uuid)> = found.iter().map(|(s, t)| (s.id, *t)).collect();
        pairs.sort();
        let mut expected = vec![(first.id, a), (second.id, b)];
        expected.sort();
        assert_eq!(
            pairs, expected,
            "each schedule must carry its own tenant, not the enumerating identity"
        );

        // A single-tenant scope still sees only its own, which is what makes
        // the pairing above a real answer rather than a coincidence.
        let mine = repo.list_enabled(&conn, &scope(a)).await.unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].1, a);
    }

    /// A disabled schedule is not a candidate. Nothing else filters it out, so
    /// without the `WHERE` the ticker would fire every schedule an operator had
    /// deliberately turned off.
    #[tokio::test]
    async fn a_disabled_schedule_is_not_enumerated() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        let mut off = sample_new_schedule("disabled");
        off.enabled = false;
        let off = repo
            .create(&conn, &scope(tenant), tenant, off)
            .await
            .unwrap();
        let on = repo
            .create(
                &conn,
                &scope(tenant),
                tenant,
                sample_new_schedule("enabled"),
            )
            .await
            .unwrap();

        let found = repo.list_enabled(&conn, &scope(tenant)).await.unwrap();
        assert_eq!(
            found.iter().map(|(s, _)| s.id).collect::<Vec<_>>(),
            vec![on.id]
        );
        assert!(
            !off.enabled,
            "`create` must honour `enabled: false`, or the assertion above passes \
             for the wrong reason"
        );
        assert_eq!(
            repo.list(&conn, &scope(tenant)).await.unwrap().len(),
            2,
            "the disabled schedule is still there; only the enumeration skips it"
        );
    }

    /// **One corrupt row must not take the whole fleet dark.**
    ///
    /// `list_enabled` is the firing ticker's only enumeration and it spans every
    /// tenant, so a `collect::<Result<..>>()` would let a single undecodable row
    /// abort the pass — every tenant's schedules stop firing, and nothing
    /// self-heals because `advance_last_fired_tick` is never reached. The
    /// reachable trigger is a rolling upgrade, where an older replica's decoder
    /// meets a value a newer replica wrote.
    ///
    /// The row is corrupted through a **scoped** write rather than raw SQL, which
    /// is both what this test tier has available and the more honest fixture: it
    /// is the shape a schema migration or a newer writer would produce.
    ///
    /// # The WARN is asserted, because it is the only signal there is
    ///
    /// The trait doc makes the log line load-bearing — a skipped row is not
    /// returned, is not an error, and is indistinguishable from a row that does
    /// not exist. So without the assertions below, deleting the `tracing::warn!`
    /// leaves a schedule silently not firing with **no error, no return value
    /// and no log**, and the whole suite green. That was measured, not assumed.
    ///
    /// `schedule_id` and `tenant_id` are pinned individually rather than through
    /// the message alone: they are what an operator needs in order to find and
    /// repair the row, and a structured field is what a refactor drops most
    /// easily — the message would survive it untouched.
    ///
    /// (`mapper.rs`'s truncation WARN is untested and is not a precedent for
    /// leaving this one so: that one annotates a value the caller still
    /// receives, while this one is the sole evidence that work did not happen.)
    ///
    /// Break-verified: restoring the `collect::<Result<Vec<_>, _>>()` turns this
    /// red, and so does deleting the `tracing::warn!`. Nothing else in the suite
    /// notices either mutation.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn an_undecodable_row_is_skipped_and_the_rest_still_enumerate() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        let broken = repo
            .create(&conn, &scope(tenant), tenant, sample_new_schedule("broken"))
            .await
            .unwrap();
        let healthy = repo
            .create(
                &conn,
                &scope(tenant),
                tenant,
                sample_new_schedule("healthy"),
            )
            .await
            .unwrap();

        // A fourth `exclusive_choice` spelling: the decoder fails closed on it,
        // which is exactly the corruption this skip is about.
        ScheduleEntity::update_many()
            .filter(by_id(broken.id))
            .secure()
            .scope_with(&scope(tenant))
            .col_expr(
                ScheduleColumn::ExclusiveChoice,
                Expr::value("inherit-maybe"),
            )
            .exec(&conn)
            .await
            .unwrap();

        // The row really is undecodable - otherwise the assertion below would
        // pass for the wrong reason.
        assert!(
            matches!(
                repo.get(&conn, &scope(tenant), broken.id).await,
                Err(DomainError::CorruptState {
                    what: "schedule.exclusive_choice",
                    ..
                })
            ),
            "the fixture must actually be corrupt, or this test is vacuous"
        );

        let found = repo
            .list_enabled(&conn, &scope(tenant))
            .await
            .expect("one undecodable row must not fail the whole enumeration");
        assert_eq!(
            found.iter().map(|(s, _)| s.id).collect::<Vec<_>>(),
            vec![healthy.id],
            "the healthy schedule must still fire; the corrupt one is skipped, not \
             returned, and its absence is visible only in the log"
        );

        // ...and that log line is the only signal, so it is asserted rather than
        // hoped for. Both identifying fields, and the fact that they carry the
        // *offending* row rather than the healthy one.
        assert!(
            logs_contain("skipping an undecodable schedule row"),
            "the skip must be announced; it is the only evidence a schedule stopped firing"
        );
        assert!(
            logs_contain(&format!("schedule_id={}", broken.id)),
            "the WARN must name the row an operator has to repair"
        );
        assert!(
            logs_contain(&format!("tenant_id={tenant}")),
            "the WARN must name the tenant, or a cross-tenant enumeration's log is \
             unactionable"
        );
        assert!(
            !logs_contain(&format!("schedule_id={}", healthy.id)),
            "only the corrupt row may be reported skipped"
        );
    }

    /// Deleting a schedule takes its claim rows with it, by cascade.
    ///
    /// A surviving claim would be worse than an orphan row: it would keep
    /// `(tenant, schedule, due time)` occupied, so a schedule recreated with the
    /// same id could never claim that time again.
    #[tokio::test]
    async fn deleting_a_schedule_cascades_its_ticks() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        let schedule = repo
            .create(
                &conn,
                &scope(tenant),
                tenant,
                sample_new_schedule("nightly"),
            )
            .await
            .unwrap();
        let tick = repo
            .claim_tick(
                &conn,
                &scope(tenant),
                tenant,
                owned(&repo, &conn, tenant, schedule.id).await,
                now(),
                "qa-runs-0",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tick_count(&conn, tenant).await, 1);

        assert!(
            repo.delete(&conn, &scope(tenant), schedule.id)
                .await
                .unwrap()
        );
        assert!(
            repo.get(&conn, &scope(tenant), schedule.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            tick_count(&conn, tenant).await,
            0,
            "the claim rows must go with their schedule, or a recreated schedule could \
             never claim the same due time again"
        );
        // The tick really is gone, not merely uncounted: a write against it
        // matches no row.
        assert!(
            !repo
                .record_tick_outcome(&conn, &scope(tenant), tick, None, None)
                .await
                .unwrap()
        );

        // Deleting again answers `false` rather than erroring.
        assert!(
            !repo
                .delete(&conn, &scope(tenant), schedule.id)
                .await
                .unwrap()
        );
    }

    /// One tick row, read back under the owning tenant's scope.
    ///
    /// A scoped read rather than a raw one, for the reason
    /// `infra::storage::test_db`'s `scope` states: ground truth taken with an
    /// unscoped query is how a cross-tenant probe gets written by accident.
    async fn read_tick(
        conn: &toolkit_db::DbConn<'_>,
        tenant: Uuid,
        tick_id: Uuid,
    ) -> crate::infra::storage::entity::schedule_tick::Model {
        TickEntity::find()
            .filter(tick_by_id(tick_id))
            .secure()
            .scope_with(&scope(tenant))
            .one(conn)
            .await
            .unwrap()
            .expect("the tick row must be visible to its own tenant")
    }

    /// How many tick rows this tenant can see.
    async fn tick_count(conn: &toolkit_db::DbConn<'_>, tenant: Uuid) -> u64 {
        TickEntity::find()
            .secure()
            .scope_with(&scope(tenant))
            .count(conn)
            .await
            .unwrap()
    }

    /// The parameters column carries the same `{name, value}` shape the run's
    /// does, which is what lets a fire hand its parameters straight to a launch.
    #[tokio::test]
    async fn schedule_parameters_use_the_run_storage_shape() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmSchedulesRepository;

        let mut new = sample_new_schedule("nightly");
        new.parameters = vec![RunParameter {
            name: "APP_BUILD".to_owned(),
            value: "20260813".to_owned(),
        }];
        let created = repo
            .create(&conn, &scope(tenant), tenant, new.clone())
            .await
            .unwrap();
        assert_eq!(created.parameters, new.parameters);
    }
}
