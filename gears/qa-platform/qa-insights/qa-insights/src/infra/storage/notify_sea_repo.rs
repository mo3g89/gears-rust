//! `SecureORM` implementation of [`NotifyRepository`].
//!
//! # The dialect dispatch is `sea-query`'s, not this file's
//!
//! Task 12's plan asks for `INSERT … ON CONFLICT DO NOTHING`, "`INSERT IGNORE`
//! on `MySQL`, `INSERT OR IGNORE` on `SQLite`", dispatched on the backend as the
//! migrations do. **A backend `match` is not expressible at this seam, and that
//! was established by a compile attempt rather than by reading:** with
//! `let _ = runner.get_database_backend();` and `let _ = runner.db_engine();`
//! spliced into this method, `cargo build -p qa-insights` gives two
//! `error[E0599]: no method named … found for reference &'life1 C`. A repository
//! is handed `&impl DBRunner`; `DBRunner` declares no methods, and the internal
//! trait that could yield a connection is `pub(crate)` to `toolkit-db`.
//!
//! The information exists one layer out — `SecureConn::db_engine()` is `pub`
//! (`libs/toolkit-db/src/secure/secure_conn.rs:131`) — so this is a property of
//! the *bound these traits take*, not of the toolkit. A trait method that needed
//! the backend would have to be given it.
//!
//! It is also unnecessary for the two backends that exist, because the dispatch
//! already happens one layer down: `OnConflict::do_nothing()` renders
//! `ON CONFLICT … DO NOTHING` on both Postgres and `SQLite`.
//!
//! # `MySQL` gets malformed SQL here, and the plan's instruction was the right one
//!
//! **Corrected 2026-08-20.** This header claimed `MySQL` gets `INSERT IGNORE`.
//! It does not, and the claim was made from reading rather than from rendering.
//! `MysqlQueryBuilder::prepare_on_conflict_action`
//! (`sea-query-0.32.7/src/backend/mysql/query.rs:155-180`, `DoNothing` arm at
//! `:161-175`) writes ` IGNORE` as the conflict *action*, after
//! `prepare_on_conflict_keywords` (`:182-184`) has already written
//! ` ON DUPLICATE KEY`. The emitted text is `ON DUPLICATE KEY IGNORE`, which is
//! neither `INSERT IGNORE` nor valid `MySQL`.
//! `the_do_nothing_clause_renders_per_backend_and_mysqls_is_malformed` renders
//! all three and pins it.
//!
//! So the plan's "dispatch on the backend" was correct about the *need* and only
//! wrong about where the dispatch could live: a `MySQL` deployment would need
//! `INSERT IGNORE` spelled some other way. **This would be a defect if `MySQL`
//! were reachable** — `claim_notification` would fail with a syntax error on
//! every call, breaking the send-once protocol rather than degrading it. It is
//! not reachable: `up()` refuses that backend outright, because five of this
//! schema's indexes exceed `InnoDB`'s 3072-byte key limit. Recorded rather than
//! left implicit, so that whoever makes `MySQL` reachable finds it here.
//!
//! # Obligation #4: a unique violation here is "already sent"
//!
//! `SeaORM` reports a `do_nothing` insert that changed no rows as
//! `DbErr::RecordNotInserted` (`sea-orm-1.1.20/src/executor/insert.rs:351`),
//! which arrives wrapped as `ScopeError::Db`. That is the *answer* `false`, not
//! an error. Letting it surface as [`DomainError::Database`] would turn every
//! duplicate delivery attempt into a 500 on the one path whose entire purpose is
//! to be idempotent.

use async_trait::async_trait;
use qa_insights_sdk::{NotificationConfig, NotificationLogEntry};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, Order, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureOnConflict,
    secure_insert, validate_tenant_in_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{NewLogEntry, NotificationClaim, NotifyRepository};
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::notification_config::{
    self, Column as ConfigColumn, Entity as ConfigEntity,
};
use crate::infra::storage::entity::notification_log::{
    self, Column as LogColumn, Entity as LogEntity,
};
use crate::infra::storage::entity::run_notification::{
    self, Column as ClaimColumn, Entity as ClaimEntity,
};
use crate::infra::storage::mapper::{
    notification_config_to_sdk, notification_log_to_sdk, slack_templates_to_json,
};

/// ORM-based implementation of the `NotifyRepository` trait.
#[derive(Clone, Default)]
pub struct OrmNotifyRepository;

/// The `ON CONFLICT` clause of the send-once claim: the four columns of
/// `idx_qa_run_notifications_claim`, `DO NOTHING`.
///
/// **A named function, not an inline literal, so a test can observe the value
/// production actually uses.** The rendering test used to build its own
/// equivalent, which meant a change to the real target rendered nothing red —
/// measured by mutation, not assumed.
///
/// The target must be named. A bare `OnConflict::new().do_nothing()` swallows
/// every constraint violation on the table, the UUID primary key included, which
/// would turn a genuine bug into a silent `Ok(false)`: "already sent", for a
/// notification that was never sent.
fn claim_conflict_target() -> OnConflict {
    OnConflict::columns([
        ClaimColumn::TenantId,
        ClaimColumn::RunId,
        ClaimColumn::NotificationKind,
        ClaimColumn::EventType,
    ])
    .do_nothing()
    .to_owned()
}

#[async_trait]
impl NotifyRepository for OrmNotifyRepository {
    async fn claim_notification<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        claim: NotificationClaim,
    ) -> Result<bool, DomainError> {
        // `scope_unchecked` is required here — `scope_with_model` would reject
        // nothing this does not already reject, but it is not what the
        // `on_conflict` builder chain accepts — so the tenant check is explicit.
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;

        let now = OffsetDateTime::now_utc();
        let am = run_notification::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            run_id: ActiveValue::Set(claim.run_id),
            notification_kind: ActiveValue::Set(claim.kind),
            event_type: ActiveValue::Set(claim.event),
            // The caller's instant, not `now()`: it is when the notification was
            // sent, which an operator reads to tell a stale claim from a fresh
            // one.
            sent_at: ActiveValue::Set(claim.sent_at),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        // `on_conflict_raw` rather than `SecureOnConflict`: that builder exists
        // to forbid `tenant_id` in an `ON CONFLICT DO UPDATE` set, and there is
        // no update here at all — `do_nothing()` writes nothing on conflict, so
        // there is no column list for it to police.
        //
        // The target is named — the four columns of
        // `idx_qa_run_notifications_claim` — rather than left bare. A targetless
        // `DO NOTHING` swallows every constraint violation on the table,
        // including one on the UUID primary key, which would turn a genuine bug
        // into a silent `Ok(false)`: "already sent", for a notification that was
        // never sent.
        let result = ClaimEntity::insert(am)
            .secure()
            .scope_unchecked(scope)
            .map_err(db_err)?
            .on_conflict_raw(claim_conflict_target())
            .exec(runner)
            .await;

        match result {
            Ok(_) => Ok(true),
            // Obligation #4. `RecordNotInserted` is what a `do_nothing` insert
            // reports when the claim was already taken; `is_unique_violation` is
            // the belt to that braces, for a backend that raises the constraint
            // instead of swallowing it.
            Err(ScopeError::Db(sea_orm::DbErr::RecordNotInserted)) => Ok(false),
            Err(e) if e.is_unique_violation() => Ok(false),
            Err(e) => Err(db_err(e)),
        }
    }

    /// R100/R86: deletes exactly the one claim row that shares `tenant_id`,
    /// `run_id`, `notification_kind` and `event_type` — every column
    /// [`claim_conflict_target`] names, so a release always targets the same
    /// slot a claim would have inserted into. `tenant_id` is an explicit
    /// `Condition` predicate alongside `.secure().scope_with(scope)`, not the
    /// scope alone: R86's own paragraph is that a scope over
    /// `owner_tenant_id` may legitimately span several tenants
    /// (`ScopeFilter::In`, `ScopeFilter::InTenantSubtree`), so a delete
    /// scoped only by the compiled scope could remove another in-scope
    /// tenant's claim on the same run id.
    ///
    /// **Fix round 3, Important 4.** `validate_tenant_in_scope` is called
    /// first, matching `claim_notification`, `get_config` and `save_config`
    /// in this same file — the review found this the one `tenant_id`-taking
    /// method here without it. Review confirmed no cross-tenant delete was
    /// possible even without the guard (`SecureDeleteMany::scope_with` ANDs
    /// the compiled scope into the statement), so this closes the
    /// fail-fast-and-consistency gap R86 asks for, not a live data leak.
    async fn release_notification<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        run_id: Uuid,
        kind: &str,
        event: &str,
    ) -> Result<(), DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;
        ClaimEntity::delete_many()
            .filter(
                Condition::all()
                    .add(ClaimColumn::TenantId.eq(tenant_id))
                    .add(ClaimColumn::RunId.eq(run_id))
                    .add(ClaimColumn::NotificationKind.eq(kind))
                    .add(ClaimColumn::EventType.eq(event)),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn append_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        entry: NewLogEntry,
    ) -> Result<(), DomainError> {
        let now = OffsetDateTime::now_utc();
        let am = notification_log::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            run_id: ActiveValue::Set(entry.run_id),
            channel: ActiveValue::Set(entry.channel),
            event_type: ActiveValue::Set(entry.event_type),
            outcome: ActiveValue::Set(entry.outcome),
            detail: ActiveValue::Set(entry.detail),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };
        secure_insert::<LogEntity>(am, scope, runner)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Newest first, which `idx_qa_notification_log_tenant_created` serves.
    ///
    /// The `LIMIT` is the caller's and there is no default: legacy's
    /// `get_notification_log(limit)` takes one too
    /// (`manager/src/services/notifications.rs:618-635`). This is the one read on
    /// this repository that is *not* unbounded.
    async fn list_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        limit: u64,
    ) -> Result<Vec<NotificationLogEntry>, DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;
        let rows = LogEntity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(LogColumn::TenantId.eq(tenant_id)))
            .order_by(Expr::col(LogColumn::CreatedAt), Order::Desc)
            // Total, so a repeated read of entries sharing a `created_at`
            // cannot reorder them and page a row twice.
            .order_by(Expr::col(LogColumn::Id), Order::Desc)
            .limit(limit)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(notification_log_to_sdk).collect())
    }

    /// `None` for a tenant that has never saved settings.
    ///
    /// Deliberately not [`NotificationConfig::default`]: applying the default is
    /// the service's job, so that a caller can still tell "unconfigured" from
    /// "configured to the defaults".
    ///
    /// Returns [`NotificationConfig::slack_webhook_credstore_ref`] and never a
    /// webhook URL — obligation #3 of the schema.
    async fn get_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<NotificationConfig>, DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;
        let row = ConfigEntity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(ConfigColumn::TenantId.eq(tenant_id)))
            .one(runner)
            .await
            .map_err(db_err)?;
        row.map(notification_config_to_sdk).transpose()
    }

    /// An upsert on `idx_qa_notification_config_tenant`, so the tenant keeps
    /// exactly one row.
    ///
    /// Every non-key column is in the update list, which is what makes this
    /// "create or **replace**". The list is written out rather than derived,
    /// because a column omitted from it keeps its old value on a save that
    /// looked like it stored the whole form — the silent half of a settings
    /// screen that appears to work.
    /// `saving_a_config_round_trips_every_field_including_the_templates` is what
    /// catches an omission.
    async fn save_config<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        config: NotificationConfig,
    ) -> Result<(), DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;

        let now = OffsetDateTime::now_utc();
        let am = notification_config::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            slack_webhook_credstore_ref: ActiveValue::Set(config.slack_webhook_credstore_ref),
            slack_channel: ActiveValue::Set(config.slack_channel),
            manager_ui_base_url: ActiveValue::Set(config.manager_ui_base_url),
            slack_enabled: ActiveValue::Set(config.slack_enabled),
            notify_on_failure: ActiveValue::Set(config.notify_on_failure),
            notify_on_success: ActiveValue::Set(config.notify_on_success),
            notify_on_schedule_completion: ActiveValue::Set(config.notify_on_schedule_completion),
            scheduled_run_slack_enabled: ActiveValue::Set(config.scheduled_run_slack_enabled),
            scheduled_run_slack_templates: ActiveValue::Set(slack_templates_to_json(
                &config.scheduled_run_slack_templates,
            )),
            run_queue_queued_slack_enabled: ActiveValue::Set(config.run_queue_queued_slack_enabled),
            email_smtp_host: ActiveValue::Set(config.email_smtp_host),
            email_smtp_port: ActiveValue::Set(i32::from(config.email_smtp_port)),
            email_from: ActiveValue::Set(config.email_from),
            email_recipients: ActiveValue::Set(config.email_recipients),
            email_enabled: ActiveValue::Set(config.email_enabled),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        let on_conflict = SecureOnConflict::<ConfigEntity>::columns([ConfigColumn::TenantId])
            .update_columns([
                ConfigColumn::SlackWebhookCredstoreRef,
                ConfigColumn::SlackChannel,
                ConfigColumn::ManagerUiBaseUrl,
                ConfigColumn::SlackEnabled,
                ConfigColumn::NotifyOnFailure,
                ConfigColumn::NotifyOnSuccess,
                ConfigColumn::NotifyOnScheduleCompletion,
                ConfigColumn::ScheduledRunSlackEnabled,
                ConfigColumn::ScheduledRunSlackTemplates,
                ConfigColumn::RunQueueQueuedSlackEnabled,
                ConfigColumn::EmailSmtpHost,
                ConfigColumn::EmailSmtpPort,
                ConfigColumn::EmailFrom,
                ConfigColumn::EmailRecipients,
                ConfigColumn::EmailEnabled,
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
}

#[cfg(test)]
mod tests {
    use qa_insights_sdk::{NotificationConfig, ScheduledRunSlackTemplate};
    use uuid::Uuid;

    use crate::domain::repos::{NewLogEntry, NotificationClaim, NotifyRepository};
    use crate::infra::storage::entity::run_notification::{self, Entity as ClaimEntity};
    use crate::infra::storage::notify_sea_repo::OrmNotifyRepository;
    use crate::infra::storage::test_db::{inmem_db, now, scope};
    use sea_orm::ActiveValue;

    fn claim(run_id: Uuid, kind: &str, event: &str) -> NotificationClaim {
        NotificationClaim {
            run_id,
            kind: kind.to_owned(),
            event: event.to_owned(),
            sent_at: now(),
        }
    }

    /// **The insert *is* the dedupe answer.**
    ///
    /// Two instances racing on the same finished run both see no claim row, both
    /// insert, and both send — unless the answer comes from the insert itself.
    /// `idx_qa_run_notifications_claim` is what makes one of the two inserts
    /// fail, and reporting *which* caller won is the only thing that turns a
    /// unique index into a send-once protocol. Legacy's shape is the same:
    /// `INSERT ... ON CONFLICT DO NOTHING` then `rows_affected() > 0`
    /// (`manager/src/services/notifications.rs:548-562`).
    #[tokio::test]
    async fn the_first_claim_on_a_slot_wins_and_the_second_loses() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);
        let run = Uuid::from_u128(0x20);

        assert!(
            OrmNotifyRepository
                .claim_notification(&conn, &ctx, tenant, claim(run, "slack", "succeeded"))
                .await
                .unwrap(),
            "the first caller must win the claim and send"
        );
        assert!(
            !OrmNotifyRepository
                .claim_notification(&conn, &ctx, tenant, claim(run, "slack", "succeeded"))
                .await
                .expect(
                    "obligation #4: a unique violation here is 'already sent', not a \
                     Database error: a 500 on the path whose whole purpose is \
                     idempotency"
                ),
            "the second caller must lose it and not send"
        );
    }

    /// **R100: a released claim can be re-taken.**
    ///
    /// The property `NotifyRepository::release_notification`'s own doc
    /// describes: a failed send is retryable by construction, unlike a
    /// successful one. Without the release this would behave exactly like
    /// `the_first_claim_on_a_slot_wins_and_the_second_loses` — the second
    /// `claim_notification` would lose.
    #[tokio::test]
    async fn a_released_claim_can_be_taken_again() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);
        let run = Uuid::from_u128(0x20);

        assert!(
            OrmNotifyRepository
                .claim_notification(&conn, &ctx, tenant, claim(run, "slack", "succeeded"))
                .await
                .unwrap()
        );

        OrmNotifyRepository
            .release_notification(&conn, &ctx, tenant, run, "slack", "succeeded")
            .await
            .unwrap();

        assert!(
            OrmNotifyRepository
                .claim_notification(&conn, &ctx, tenant, claim(run, "slack", "succeeded"))
                .await
                .unwrap(),
            "a released claim must be winnable again, so a retry after a failed \
             send is not permanently suppressed"
        );
    }

    /// R86: releasing under one tenant's scope must not delete another
    /// tenant's claim on the same run id and slot.
    #[tokio::test]
    async fn releasing_a_claim_does_not_touch_another_tenants_claim() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);
        let run = Uuid::from_u128(0x20);

        assert!(
            OrmNotifyRepository
                .claim_notification(&conn, &scope(mine), mine, claim(run, "slack", "succeeded"))
                .await
                .unwrap()
        );
        assert!(
            OrmNotifyRepository
                .claim_notification(
                    &conn,
                    &scope(theirs),
                    theirs,
                    claim(run, "slack", "succeeded")
                )
                .await
                .unwrap()
        );

        OrmNotifyRepository
            .release_notification(&conn, &scope(mine), mine, run, "slack", "succeeded")
            .await
            .unwrap();

        assert!(
            OrmNotifyRepository
                .claim_notification(&conn, &scope(mine), mine, claim(run, "slack", "succeeded"))
                .await
                .unwrap(),
            "my own released claim must be re-takeable"
        );
        assert!(
            !OrmNotifyRepository
                .claim_notification(
                    &conn,
                    &scope(theirs),
                    theirs,
                    claim(run, "slack", "succeeded")
                )
                .await
                .unwrap(),
            "releasing my claim must not have deleted another tenant's claim on \
             the same run id and slot"
        );
    }

    /// Releasing a slot nothing ever claimed is not an error — two releases
    /// racing each other, or a release after a claim that was never taken,
    /// both find nothing to delete.
    #[tokio::test]
    async fn releasing_an_unclaimed_slot_is_not_an_error() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);

        OrmNotifyRepository
            .release_notification(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0x20),
                "slack",
                "succeeded",
            )
            .await
            .expect("no matching row is not a failure");
    }

    /// **Fix round 3, Important 4.** Releasing a *different* tenant's claim
    /// than the one the compiled scope names is refused before the delete
    /// runs — matching `advancing_a_mark_for_a_tenant_outside_the_scope_is_refused`
    /// (`watermark_sea_repo.rs`) and
    /// `saving_settings_for_a_tenant_outside_the_scope_is_refused`
    /// (`jira_sea_repo.rs`). The claim itself is left in place, which the
    /// second assertion checks directly rather than trusting the `Err`
    /// alone.
    #[tokio::test]
    async fn releasing_a_tenant_outside_the_scope_is_refused() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);
        let run = Uuid::from_u128(0x20);

        assert!(
            OrmNotifyRepository
                .claim_notification(
                    &conn,
                    &scope(theirs),
                    theirs,
                    claim(run, "slack", "succeeded")
                )
                .await
                .unwrap(),
            "their claim exists so the release below has something to (wrongly) target"
        );

        let outcome = OrmNotifyRepository
            .release_notification(&conn, &scope(mine), theirs, run, "slack", "succeeded")
            .await;
        assert!(
            outcome.is_err(),
            "releasing another tenant's claim under my own scope must be refused: {outcome:?}"
        );

        assert!(
            !OrmNotifyRepository
                .claim_notification(
                    &conn,
                    &scope(theirs),
                    theirs,
                    claim(run, "slack", "succeeded")
                )
                .await
                .unwrap(),
            "the refused release must not have deleted their claim - it must still be held"
        );
    }

    /// **What `do_nothing()` actually renders, per backend.**
    ///
    /// This test exists because the module header used to claim `MySQL` gets
    /// `INSERT IGNORE`. It does not. `sea-query`'s `MysqlQueryBuilder` writes
    /// ` IGNORE` as the *conflict action*, after
    /// `prepare_on_conflict_keywords` has already written ` ON DUPLICATE KEY`
    /// (`sea-query-0.32.7/src/backend/mysql/query.rs:155-180`, the `DoNothing`
    /// arm at `:161-175`; the keywords at `:182-184`), so the emitted text is
    /// `ON DUPLICATE KEY IGNORE` — which is neither `INSERT IGNORE` nor valid
    /// `MySQL`.
    ///
    /// **It is a latent defect, not a live one**, and only because `up()` refuses
    /// the `MySql` backend outright (five of this schema's indexes exceed `InnoDB`'s
    /// 3072-byte key limit), so no `MySQL` connection can ever reach this
    /// statement. If `MySQL` ever became reachable, `claim_notification` would fail
    /// with a syntax error on every call and the send-once protocol would be
    /// broken rather than degraded. Pinned here so that whoever makes `MySQL`
    /// reachable finds this instead of discovering it in production.
    #[test]
    fn the_do_nothing_clause_renders_per_backend_and_mysqls_is_malformed() {
        use sea_orm::{DatabaseBackend, EntityTrait, QueryTrait};
        use toolkit_db::secure::SecureInsertExt;

        // **Rendered through the production chain**, not a hand-rolled
        // equivalent: `.secure().scope_unchecked()` then `on_conflict_raw`, with
        // the same named target `claim_notification` uses. `SecureInsertOne`
        // exposes `into_inner()`, so the statement the wrapper actually built is
        // reachable and can be handed to `QueryTrait::build` — checked, not
        // assumed. An earlier version of this test used a bare
        // `ClaimEntity::insert(..).on_conflict(..)`, which pinned `sea-query`'s
        // rendering of a statement production does not emit.
        let sql_for = |backend: DatabaseBackend| {
            ClaimEntity::insert(run_notification::ActiveModel {
                id: ActiveValue::Set(Uuid::from_u128(1)),
                tenant_id: ActiveValue::Set(Uuid::from_u128(0xA)),
                run_id: ActiveValue::Set(Uuid::from_u128(0x20)),
                notification_kind: ActiveValue::Set("slack".to_owned()),
                event_type: ActiveValue::Set("succeeded".to_owned()),
                sent_at: ActiveValue::Set(now()),
                created_at: ActiveValue::Set(now()),
                updated_at: ActiveValue::Set(now()),
            })
            .secure()
            .scope_unchecked(&scope(Uuid::from_u128(0xA)))
            .unwrap()
            .on_conflict_raw(super::claim_conflict_target())
            .into_inner()
            .build(backend)
            .to_string()
        };

        for backend in [DatabaseBackend::Postgres, DatabaseBackend::Sqlite] {
            let sql = sql_for(backend);
            assert!(
                sql.contains("ON CONFLICT") && sql.contains("DO NOTHING"),
                "{backend:?} must get ON CONFLICT ... DO NOTHING; got: {sql}"
            );
            // The named target, so a future edit back to `OnConflict::new()` --
            // which swallows every violation on the table, primary key included --
            // goes red here.
            //
            // Asserted on the clause **after** `ON CONFLICT`, not on the whole
            // statement: every one of these column names also appears in the
            // INSERT's own column list, so `sql.contains(..)` passed for a bare
            // target and this assertion was vacuous. Found by mutation.
            let clause = sql
                .split_once("ON CONFLICT")
                .expect("the statement must have an ON CONFLICT clause")
                .1;
            for column in ["tenant_id", "run_id", "notification_kind", "event_type"] {
                assert!(
                    clause.contains(column),
                    "{backend:?} must name {column} in the conflict target rather \
                     than leaving it bare; clause was: {clause}"
                );
            }
        }

        let mysql = sql_for(DatabaseBackend::MySql);
        assert!(
            mysql.contains("ON DUPLICATE KEY IGNORE"),
            "MySQL renders the malformed `ON DUPLICATE KEY IGNORE`, not \
             `INSERT IGNORE`; if this assertion ever fails the dependency has \
             changed and the module header needs re-reading. Got: {mysql}"
        );
        assert!(
            !mysql.contains("INSERT IGNORE"),
            "the header used to claim this spelling and it was never emitted: {mysql}"
        );
    }

    /// **`do_nothing()` is load-bearing on Postgres, and only a real Postgres can
    /// show it.**
    ///
    /// Replacing the `ON CONFLICT DO NOTHING` with a plain insert leaves the
    /// `SQLite` tier entirely green: the second claim raises a unique violation,
    /// `claim_notification`'s `is_unique_violation` arm answers `Ok(false)`, and
    /// the observable behaviour is identical. Measured, not assumed — that
    /// mutation was run.
    ///
    /// On Postgres it is **not** identical. A failed statement aborts the whole
    /// transaction, so a caller that claims inside one would get `Ok(false)` —
    /// "already sent", a perfectly ordinary answer — while holding a transaction
    /// in which every subsequent statement fails with `25P02`. That is the worst
    /// shape available: a correct-looking return value hiding a poisoned
    /// transaction. `DO NOTHING` never raises, so the transaction stays usable.
    ///
    /// This matters because `claim_notification` takes `&impl DBRunner`
    /// specifically so it *can* run in a caller's transaction alongside the
    /// notification it is claiming for.
    ///
    /// It also means the `is_unique_violation` arm is a fallback that must never
    /// actually fire on Postgres. It is kept for a backend that raises where this
    /// one does not, and this test is what pins which of the two paths is live.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn a_lost_claim_inside_a_transaction_leaves_the_transaction_usable() {
        use crate::domain::error::DomainError;
        use crate::infra::storage::test_db::pg_db;

        let harness = pg_db().await;
        let provider = std::sync::Arc::new(toolkit_db::DBProvider::<DomainError>::new(
            harness.db.clone(),
        ));
        let tenant = Uuid::from_u128(0xA);
        let run = Uuid::from_u128(0x20);

        let outcome: Result<(), DomainError> = provider
            .transaction(move |tx| {
                Box::pin(async move {
                    let ctx = scope(tenant);
                    assert!(
                        OrmNotifyRepository
                            .claim_notification(tx, &ctx, tenant, claim(run, "slack", "succeeded"))
                            .await?,
                        "the first claim wins"
                    );
                    assert!(
                        !OrmNotifyRepository
                            .claim_notification(tx, &ctx, tenant, claim(run, "slack", "succeeded"))
                            .await?,
                        "the second loses"
                    );
                    // The point of the test: the transaction is still alive after
                    // the lost claim. With a plain INSERT this write fails with
                    // `current transaction is aborted`.
                    OrmNotifyRepository
                        .append_log(
                            tx,
                            &ctx,
                            tenant,
                            NewLogEntry {
                                run_id: Some(run),
                                channel: "slack".to_owned(),
                                event_type: "succeeded".to_owned(),
                                outcome: "skipped".to_owned(),
                                detail: String::new(),
                            },
                        )
                        .await?;
                    Ok(())
                })
            })
            .await;

        assert!(
            outcome.is_ok(),
            "a lost claim must not poison the caller's transaction: {outcome:?}"
        );

        let conn = harness.db.conn().unwrap();
        assert_eq!(
            OrmNotifyRepository
                .list_log(&conn, &scope(tenant), tenant, 10)
                .await
                .unwrap()
                .len(),
            1,
            "and the work done after the lost claim must have committed"
        );
    }

    /// A different `event_type` on the same run and kind is a different slot.
    ///
    /// The fixture transposes nothing, which is why [`NotificationClaim`] is a
    /// struct: `kind` and `event` are both `String` and adjacent, and a call site
    /// that swapped them would claim a slot nothing else ever claims — deduping
    /// nothing while looking like it worked.
    #[tokio::test]
    async fn a_different_event_on_the_same_run_is_a_different_claim() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);
        let run = Uuid::from_u128(0x20);

        assert!(
            OrmNotifyRepository
                .claim_notification(&conn, &ctx, tenant, claim(run, "slack", "succeeded"))
                .await
                .unwrap()
        );
        assert!(
            OrmNotifyRepository
                .claim_notification(&conn, &ctx, tenant, claim(run, "slack", "failed"))
                .await
                .unwrap(),
            "the 'failed' slot is unclaimed"
        );
    }

    /// The claim is per tenant, not global — `run_id` is unique on its own, so
    /// without the tenant prefix one tenant's send would suppress another's.
    #[tokio::test]
    async fn a_claim_is_per_tenant_not_global() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);
        let run = Uuid::from_u128(0x20);

        assert!(
            OrmNotifyRepository
                .claim_notification(&conn, &scope(mine), mine, claim(run, "slack", "succeeded"))
                .await
                .unwrap()
        );
        assert!(
            OrmNotifyRepository
                .claim_notification(
                    &conn,
                    &scope(theirs),
                    theirs,
                    claim(run, "slack", "succeeded")
                )
                .await
                .unwrap(),
            "another tenant's claim on the same run must still be winnable"
        );
    }

    /// The audit trail comes back newest first, which is legacy's
    /// `ORDER BY created_at DESC LIMIT $1` (`notifications.rs:618-635`).
    ///
    /// The three entries are written in one call each, so they share a
    /// `created_at` at second resolution; the assertion is therefore about the
    /// *count* the limit admits, not about which of three equal timestamps leads.
    #[tokio::test]
    async fn the_audit_log_is_limited() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);

        for outcome in ["ok", "failed", "ok"] {
            OrmNotifyRepository
                .append_log(
                    &conn,
                    &ctx,
                    tenant,
                    NewLogEntry {
                        run_id: Some(Uuid::from_u128(0x20)),
                        channel: "slack".to_owned(),
                        event_type: "succeeded".to_owned(),
                        outcome: outcome.to_owned(),
                        detail: String::new(),
                    },
                )
                .await
                .unwrap();
        }

        assert_eq!(
            OrmNotifyRepository
                .list_log(&conn, &ctx, tenant, 2)
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            OrmNotifyRepository
                .list_log(&conn, &ctx, tenant, 10)
                .await
                .unwrap()
                .len(),
            3
        );
    }

    /// A log entry that belongs to no run — a settings `/test` send — stores
    /// `run_id = NULL` and reads back as `None`, not as a zero UUID.
    #[tokio::test]
    async fn a_log_entry_may_belong_to_no_run() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);

        OrmNotifyRepository
            .append_log(
                &conn,
                &ctx,
                tenant,
                NewLogEntry {
                    run_id: None,
                    channel: "email".to_owned(),
                    event_type: String::new(),
                    outcome: "failed".to_owned(),
                    detail: "smtp unreachable".to_owned(),
                },
            )
            .await
            .unwrap();

        let entries = OrmNotifyRepository
            .list_log(&conn, &ctx, tenant, 10)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].run_id, None);
        assert_eq!(entries[0].detail, "smtp unreachable");
    }

    /// `None` is distinct from a row at its defaults: a tenant that has never
    /// opened the settings page has no row, and applying
    /// [`NotificationConfig::default`] is the service's job, not the
    /// repository's.
    #[tokio::test]
    async fn an_unconfigured_tenant_has_no_config_rather_than_the_defaults() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);

        assert!(
            OrmNotifyRepository
                .get_config(&conn, &scope(tenant), tenant)
                .await
                .unwrap()
                .is_none(),
            "no row must read back as None, so a caller can tell 'unconfigured' \
             from 'configured to the defaults'"
        );
    }

    /// **The fifteen-field settings round trip, including the JSON document.**
    ///
    /// `qa-insights-sdk` is deliberately `serde`-free, so neither
    /// `ScheduledRunSlackTemplates` nor its six templates derive `Serialize`, and
    /// `mapper` encodes and decodes the column by hand. Two hand-written halves
    /// with six keys and seven fields each is the shape that drifts silently — a
    /// key written `in_progress` and read `inProgress` would leave that
    /// subscription permanently un-firing with nothing failing — so the round
    /// trip is the only control there is.
    #[tokio::test]
    async fn saving_a_config_round_trips_every_field_including_the_templates() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);

        let template = |icon: &str| ScheduledRunSlackTemplate {
            enabled: true,
            status_icon: Some(icon.to_owned()),
            header: Some("{{name}}".to_owned()),
            summary: Some("{{summary}}".to_owned()),
            results: Some("{{results}}".to_owned()),
            body: Some("{{message}}".to_owned()),
            footer: Some("{{footer}}".to_owned()),
        };
        let config = NotificationConfig {
            slack_webhook_credstore_ref: "cred://slack-hook".to_owned(),
            slack_channel: "#qa".to_owned(),
            manager_ui_base_url: "https://qa.example".to_owned(),
            slack_enabled: true,
            notify_on_failure: false,
            notify_on_success: true,
            notify_on_schedule_completion: true,
            scheduled_run_slack_enabled: true,
            scheduled_run_slack_templates: qa_insights_sdk::ScheduledRunSlackTemplates {
                pending: template(":clock1:"),
                in_progress: template(":runner:"),
                succeeded: template(":white_check_mark:"),
                failed: template(":x:"),
                error: template(":boom:"),
                skipped: template(":fast_forward:"),
            },
            run_queue_queued_slack_enabled: true,
            email_smtp_host: "smtp.example".to_owned(),
            email_smtp_port: 2525,
            email_from: "qa@example".to_owned(),
            email_recipients: "a@example, b@example".to_owned(),
            email_enabled: true,
        };

        OrmNotifyRepository
            .save_config(&conn, &ctx, tenant, config.clone())
            .await
            .unwrap();

        assert_eq!(
            OrmNotifyRepository
                .get_config(&conn, &ctx, tenant)
                .await
                .unwrap(),
            Some(config),
            "every one of the fifteen fields must survive, and the templates \
             document is the half nothing else checks"
        );
    }

    /// One row per tenant, enforced by `idx_qa_notification_config_tenant`: a
    /// second save replaces rather than duplicating.
    #[tokio::test]
    async fn saving_a_config_twice_replaces_the_tenants_one_row() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);

        OrmNotifyRepository
            .save_config(&conn, &ctx, tenant, NotificationConfig::default())
            .await
            .unwrap();
        OrmNotifyRepository
            .save_config(
                &conn,
                &ctx,
                tenant,
                NotificationConfig {
                    slack_channel: "#qa-alerts".to_owned(),
                    ..NotificationConfig::default()
                },
            )
            .await
            .expect("a second save must be a replacement, not a unique violation");

        assert_eq!(
            OrmNotifyRepository
                .get_config(&conn, &ctx, tenant)
                .await
                .unwrap()
                .map(|c| c.slack_channel),
            Some("#qa-alerts".to_owned())
        );
    }
}
