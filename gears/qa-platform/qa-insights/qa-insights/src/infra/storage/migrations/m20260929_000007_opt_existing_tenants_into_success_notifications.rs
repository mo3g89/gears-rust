//! `notify_on_success = TRUE` for every tenant that already had a
//! notification config when this migration ran. The column's **default stays
//! `FALSE`**, so a tenant onboarded afterwards is not opted in.
//!
//! # Why a data migration rather than a new default
//!
//! `notify_on_failure` and `notify_on_success` were stored, round-tripped and
//! read by nothing until the owner's 2026-09-29 ruling made them a routing
//! gate (`domain::notify::routing`'s header, "Two of those three dead fields
//! are live here"). On the one intermediate build that ran with them inert
//! and notifications live (commits `c5bcf0a24` to `4d563bb3f`, both
//! 2026-09-29), **every** scheduled run notified, whatever its outcome; before
//! `c5bcf0a24` nothing notified at all. The moment they became live, a tenant
//! on that build carrying the stored default — `notify_on_failure` on,
//! `notify_on_success` off — would stop being told about passing runs it is
//! told about today.
//!
//! That is a *reduction*, and the ruling was to widen. This migration brings
//! existing rows to the behaviour they already have, so the upgrade changes
//! nothing for them until they ask it to.
//!
//! **Flipping the column default to `TRUE` was rejected**, and not only as a
//! matter of taste: `qa_insights_sdk::NotificationConfig::default`, this
//! schema's `DEFAULT FALSE`, the settings endpoint's own `OpenAPI` description
//! ("every gate off except `notify_on_failure`") and `qa-platform-ui`'s
//! `DEFAULT_FORM` all state the same documented intent, and a new tenant who
//! has expressed no preference should not be signed up for mail about runs
//! that went fine. The two questions are genuinely different — "do not change
//! what an existing deployment does" and "what should someone new get" — so
//! they get two different answers, which is what makes this a one-shot
//! `UPDATE` and not a DDL change.
//!
//! # The row set: "exists when this runs", and how that is expressed
//!
//! The scoping that matters is temporal, and most of it is inherent: a single
//! `UPDATE` executed at instant T touches exactly the rows that exist at T. A
//! tenant onboarded at T+1 has no row to touch.
//!
//! [`opt_in_statement`] adds one explicit predicate on top of that,
//! `created_at < :horizon`, with the horizon captured once at [`up`](MigrationTrait::up).
//! It closes the one window the statement alone does not: `job-db-migrate` is
//! a Job running against a live database, and a tenant saving their settings
//! while it runs would otherwise be opted in by a migration that is supposed
//! to be about the tenants that came before it. The predicate is bounded by an
//! instant this execution chose, not by "now" as the database sees it row by
//! row, which is why [`opt_in_statement`] takes the horizon as an argument and
//! `the_horizon_bounds_the_rows_the_statement_touches` can pin it.
//!
//! **That predicate is not, and is not documented as, a re-execution guard.**
//! See the next section, which says what is.
//!
//! # Re-execution, and why the guard is the ledger rather than a `WHERE`
//!
//! The hazard worth naming: a tenant who is opted in here, deliberately turns
//! `notify_on_success` back off, and then has the migration re-applied over
//! their choice.
//!
//! **No self-contained predicate on this table can prevent that**, and it is
//! worth writing down why, because two plausible ones look like they do and do
//! not. A re-execution would capture a *fresh* horizon, so `created_at <
//! horizon` and `updated_at < horizon` are both satisfied by a row the tenant
//! edited after the first execution — the horizon moves, and the row's
//! instants do not move with it. A guard would need a horizon persisted at the
//! first execution, and this table has nowhere to persist one; stamping a
//! recognisable sentinel into `updated_at` was considered and rejected, both
//! because it makes the column lie about when the tenant last saved and
//! because it fails outright on a deployment whose one opted-in tenant later
//! saves anything at all.
//!
//! So the guard is the **migration ledger**, which is the real mechanism in
//! any case: the chart runs `job-db-migrate` on every deploy, so
//! `MigratorTrait::up` is re-invoked constantly and applies each migration by
//! name exactly once per database. That is the production re-run path, and
//! `a_chosen_false_survives_a_redeploy` drives it — `up`, a tenant turning the
//! flag off, `up` again — rather than calling [`MigrationTrait::up`] directly,
//! which is a thing no deployment does.
//!
//! Stated plainly so the next reader is not misled: re-executing *this
//! statement* against a database it has already run on would re-opt-in a
//! tenant who has since opted out. Nothing in the deployed system does that.
//!
//! # Every value is **bound**, for the reason `m20260929_000004` records
//!
//! [`opt_in_statement`] builds a `sea_query` `UPDATE` and lets the driver
//! encode both values rather than spelling either into SQL text.
//! `m20260929_000004_run_completed_notification_cutoff`'s header carries the
//! defect that rule was bought with — a UUID literal that Postgres cast and
//! `SQLite` silently failed to match, so the bug shipped green on the
//! deployment that mattered and failed only where it was cheap to notice — and
//! **both** of this statement's values are in that class:
//!
//! * the **boolean** is a real `BOOLEAN` on Postgres and an `INTEGER` on
//!   `SQLite` (`m20260818_000001_initial`'s two DDL blocks: `BOOLEAN NOT NULL
//!   DEFAULT FALSE` against `INTEGER NOT NULL DEFAULT 0`). Bound, each driver
//!   encodes a Rust `bool` the way its own column stores it;
//! * the **instant** is a `TIMESTAMPTZ` on Postgres and `TEXT` on `SQLite`,
//!   and the `TEXT` encoding is `sqlx`'s, not a format this file gets to
//!   choose. A literal would have to guess it, and a `created_at < '…'`
//!   comparison that guessed wrong would silently match nothing — a migration
//!   that ran, reported success, and opted in no one.
//!
//! This is also why there is no `POSTGRES_UP`/`SQLITE_UP` pair here as in the
//! DDL migrations: there is no dialect-specific *text* to write. One statement
//! is rendered and bound per dialect by the connection it executes on, which
//! is the stronger form of "both dialect paths" rather than a waiver from it.
//! The backend is still matched, to refuse `MySQL` explicitly the way every
//! earlier migration in this chain does.
//!
//! # `down`
//!
//! A no-op that succeeds, and deliberately. The inverse of "set these rows to
//! `TRUE`" is "set them to what they were", and this migration recorded no
//! before-image — nor could it, without a table to put one in. Setting every
//! row back to `FALSE` would be worse than doing nothing: it would discard the
//! choice of every tenant who has switched success notifications on since,
//! which is a data-loss `down` for a migration whose `up` loses nothing.

use sea_orm::ConnectionTrait as _;
use sea_orm_migration::prelude::*;
use time::OffsetDateTime;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The table and the three columns this statement names.
///
/// `DeriveIden` renders each variant in snake case, so `Table` is
/// `qa_notification_config` and `NotifyOnSuccess` is `notify_on_success` — the
/// same names `m20260818_000001_initial`'s DDL declares and
/// `entity::notification_config` binds. Three independent spellings of one
/// schema, which is why every test below reads back through the *entity* after
/// writing through *this*.
#[derive(DeriveIden)]
enum QaNotificationConfig {
    Table,
    NotifyOnSuccess,
    CreatedAt,
}

/// Refuse a backend this gear has no schema for, before touching anything.
///
/// Same shape and the same refusal as every earlier migration's `up_ddl`,
/// minus the DDL it would have returned: this migration has none.
fn check_backend(backend: sea_orm::DatabaseBackend) -> Result<(), DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => Ok(()),
        sea_orm::DatabaseBackend::MySql => Err(DbErr::Custom(
            "qa-insights has no MySQL schema; see m20260818_000001_initial's header".to_owned(),
        )),
        other => Err(DbErr::Migration(format!(
            "unsupported database backend: {other:?}"
        ))),
    }
}

/// Opt in every config row created before `horizon` that is not opted in
/// already.
///
/// `horizon` is a parameter rather than a `now_utc()` call inside, so one
/// execution uses one instant for every row and so
/// `the_horizon_bounds_the_rows_the_statement_touches` can choose it. See this
/// module's header for what that predicate does and does not claim.
///
/// # `notify_on_success = false` is a write-narrowing clause, not a guard
///
/// Said plainly because the two look alike and this branch has shipped six
/// guards that were not one. Dropping this clause changes **no row's value**:
/// setting `TRUE` on a row that is already `TRUE` is a no-op, so no test can
/// distinguish the two statements, and none pretends to — the mutation was
/// run and survived, which is the correct outcome rather than a gap to paper
/// over with an assertion.
///
/// It earns its place anyway, on Postgres and not on the unit tier: without
/// it the `UPDATE` rewrites every config row that predates the horizon,
/// producing a dead tuple and a WAL record per tenant inside
/// `job-db-migrate`, for rows it does not change. This clause is about what
/// the statement writes; the `created_at` clause beside it is about which rows
/// it may touch at all.
fn opt_in_statement(horizon: OffsetDateTime) -> UpdateStatement {
    Query::update()
        .table(QaNotificationConfig::Table)
        .value(QaNotificationConfig::NotifyOnSuccess, true)
        .and_where(Expr::col(QaNotificationConfig::NotifyOnSuccess).eq(false))
        .and_where(Expr::col(QaNotificationConfig::CreatedAt).lt(horizon))
        .to_owned()
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        check_backend(manager.get_database_backend())?;
        // `ConnectionTrait::execute` takes the `sea_query` statement itself,
        // so the backend is never named twice: the driver renders the
        // placeholders and binds both values for whichever dialect this
        // connection is.
        conn.execute(&opt_in_statement(OffsetDateTime::now_utc()))
            .await?;
        Ok(())
    }

    /// Deliberately nothing — see this module's header, "`down`".
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

/// The same argument every migration module here makes: every name above is a
/// runtime string, so `cargo build` is no evidence at all about this file.
#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the initial migration's test module carries the same allow and the same reason: \
              every name in this file is a runtime string, so the only evidence about it comes \
              from raw statements against a real engine"
)]
mod tests {
    use sea_orm::{
        ActiveValue, ColumnTrait as _, ConnectionTrait as _, EntityTrait, QueryFilter as _,
    };
    use sea_orm_migration::sea_orm::{Database, DatabaseConnection};
    use sea_orm_migration::{MigrationName as _, MigratorTrait as _};
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    use super::super::Migrator;
    use crate::infra::storage::entity::notification_config;

    /// How many migrations stand in front of this one. A literal, not
    /// `Migrator::migrations().len() - 1`: derived from the list it is meant
    /// to index into, it would follow the list wherever a later append moved
    /// it, and `a_row_written_before_the_upgrade_is_opted_in` would quietly
    /// start building its "before" database at the wrong revision.
    const MIGRATIONS_BEFORE_THIS_ONE: u32 = 6;

    /// A connection at the revision **before** this migration — the state an
    /// upgrading deployment is in when it reaches it.
    async fn db_before_the_upgrade() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        Migrator::up(&conn, Some(MIGRATIONS_BEFORE_THIS_ONE))
            .await
            .expect("the earlier migrations apply");
        conn
    }

    async fn migrated_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        Migrator::up(&conn, None).await.expect("migrations apply");
        conn
    }

    /// One config row, written the way a deployment's own `save_config`
    /// writes one: every column set, `notify_on_success` as given.
    ///
    /// Through the **entity**, not a raw `INSERT`, so the instants and the
    /// booleans are encoded by the same driver codec the migration's bound
    /// statement will later compare against. A raw-literal fixture would be
    /// testing this file against a spelling no deployment produces.
    async fn write_config(
        conn: &DatabaseConnection,
        tenant_id: Uuid,
        notify_on_success: bool,
        created_at: OffsetDateTime,
    ) -> Uuid {
        let id = Uuid::new_v4();
        notification_config::Entity::insert(notification_config::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant_id),
            slack_webhook_credstore_ref: ActiveValue::Set("slack-hook".to_owned()),
            slack_channel: ActiveValue::Set("#qa".to_owned()),
            manager_ui_base_url: ActiveValue::Set("https://qa.example.test".to_owned()),
            slack_enabled: ActiveValue::Set(true),
            notify_on_failure: ActiveValue::Set(true),
            notify_on_success: ActiveValue::Set(notify_on_success),
            notify_on_schedule_completion: ActiveValue::Set(false),
            scheduled_run_slack_enabled: ActiveValue::Set(false),
            scheduled_run_slack_templates: ActiveValue::Set(serde_json::json!({})),
            run_queue_queued_slack_enabled: ActiveValue::Set(false),
            email_smtp_host: ActiveValue::Set(String::new()),
            email_smtp_port: ActiveValue::Set(587),
            email_smtp_username: ActiveValue::Set(String::new()),
            email_smtp_credstore_ref: ActiveValue::Set(String::new()),
            email_from: ActiveValue::Set(String::new()),
            email_recipients: ActiveValue::Set(String::new()),
            email_enabled: ActiveValue::Set(false),
            created_at: ActiveValue::Set(created_at),
            updated_at: ActiveValue::Set(created_at),
        })
        .exec(conn)
        .await
        .expect("the fixture config row is written");
        id
    }

    async fn notify_on_success(conn: &DatabaseConnection, id: Uuid) -> bool {
        notification_config::Entity::find_by_id(id)
            .one(conn)
            .await
            .expect("the config table is readable through its entity")
            .expect("the fixture row is still there")
            .notify_on_success
    }

    /// **The first half of the ruling.** A tenant whose row was written before
    /// the upgrade comes out of it opted in, so the moment
    /// `notify_on_success` becomes a routing gate their passing runs keep
    /// being announced.
    ///
    /// Built at the revision *before* this migration and then upgraded, rather
    /// than by applying the whole chain to an empty database and inserting
    /// afterwards: over an empty database this migration updates nothing, and
    /// a migration that did nothing at all would pass.
    ///
    /// Mutated against: deleting the `.value(...)` clause, and removing the
    /// entry from `Migrator::migrations()`, each turn this red.
    #[tokio::test]
    async fn a_row_written_before_the_upgrade_is_opted_in() {
        let conn = db_before_the_upgrade().await;
        let opted_out = write_config(
            &conn,
            Uuid::new_v4(),
            false,
            OffsetDateTime::now_utc() - Duration::days(30),
        )
        .await;
        let already_on = write_config(
            &conn,
            Uuid::new_v4(),
            true,
            OffsetDateTime::now_utc() - Duration::days(30),
        )
        .await;
        assert!(
            !notify_on_success(&conn, opted_out).await,
            "the fixture must start opted out, or this test proves nothing"
        );

        Migrator::up(&conn, None)
            .await
            .expect("the upgrade applies");

        assert!(
            notify_on_success(&conn, opted_out).await,
            "an existing tenant must come out of the upgrade opted in"
        );
        assert!(
            notify_on_success(&conn, already_on).await,
            "and a tenant who had already turned it on must be left alone"
        );
    }

    /// **The second half.** The column's default is untouched, so a tenant
    /// onboarded after the upgrade gets the documented `FALSE`.
    ///
    /// Asserted against the **schema**, through a raw `INSERT` that names no
    /// boolean at all — the only way to observe a column default, since every
    /// other path in this codebase sets the column explicitly. A
    /// `NotificationConfig::default()` assertion would have pinned the SDK's
    /// opinion, not the database's, and the two are exactly what a careless
    /// `ALTER COLUMN SET DEFAULT` would put out of step.
    ///
    /// Mutated against: `SQLite` cannot `ALTER COLUMN … SET DEFAULT`, so the
    /// mutation was applied where the default actually lives — flipping
    /// `m20260818_000001_initial`'s `SQLITE_UP` to `notify_on_success INTEGER
    /// NOT NULL DEFAULT 1`. This turns red, together with that migration's own
    /// `omitted_columns_land_on_legacy_defaults`, and nothing else does.
    #[tokio::test]
    async fn the_column_default_stays_false_for_a_tenant_onboarded_afterwards() {
        let conn = migrated_db().await;
        let id = Uuid::new_v4();
        conn.execute_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO qa_notification_config (id, tenant_id, created_at, updated_at) \
                 VALUES ('{id}', '{tenant}', '{now}', '{now}')",
                tenant = Uuid::new_v4(),
                now = "2026-12-01 00:00:00+00:00",
            ),
        ))
        .await
        .expect("a row that names no boolean column takes every default");

        let raw = conn
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                format!("SELECT notify_on_success FROM qa_notification_config WHERE id = '{id}'"),
            ))
            .await
            .expect("the default is readable")
            .expect("the row is there");
        let stored: i32 = raw
            .try_get("", "notify_on_success")
            .expect("notify_on_success reads back as an integer on sqlite");

        assert_eq!(
            stored, 0,
            "the column default must still be FALSE: a tenant who has expressed no preference \
             is not signed up for mail about runs that went fine"
        );
    }

    /// **What the `created_at` predicate buys**, pinned on the statement
    /// rather than through a migration run, because a migration run always
    /// captures a horizon later than every row in the database and so can
    /// never exercise the other side.
    ///
    /// `job-db-migrate` runs against a live database. A tenant saving their
    /// settings for the first time *while* it runs must not be opted in by a
    /// migration about the tenants that came before it.
    ///
    /// Mutated against: deleting the `created_at` `and_where` turns this red
    /// on `after`, and turns nothing else in the suite red — which is exactly
    /// why it is asserted here and not left to the migration-level tests.
    #[tokio::test]
    async fn the_horizon_bounds_the_rows_the_statement_touches() {
        let conn = migrated_db().await;
        let horizon = OffsetDateTime::now_utc();
        let before =
            write_config(&conn, Uuid::new_v4(), false, horizon - Duration::minutes(1)).await;
        let after =
            write_config(&conn, Uuid::new_v4(), false, horizon + Duration::minutes(1)).await;

        conn.execute(&super::opt_in_statement(horizon))
            .await
            .expect("the statement runs");

        assert!(
            notify_on_success(&conn, before).await,
            "a row created before the horizon is opted in"
        );
        assert!(
            !notify_on_success(&conn, after).await,
            "a row created after the horizon is not this migration's business"
        );
    }

    /// **A tenant's own choice survives a redeploy.** The chart runs
    /// `job-db-migrate` on every deploy, so `MigratorTrait::up` is re-invoked
    /// constantly; the ledger is what makes each migration apply once per
    /// database, and this drives that real path rather than calling
    /// `MigrationTrait::up` directly, which no deployment does.
    ///
    /// See this module's header for why the guard is the ledger and not a
    /// `WHERE` clause, and for the residual that leaves.
    ///
    /// Mutated against, and the result stated honestly: it turns red when the
    /// opt-in itself is broken (writing `FALSE`, or dropping the migration
    /// from the chain), because its first assertion is that the opt-in
    /// happened. **No mutation of this file turns its *final* assertion red
    /// on its own**, and that is inherent — the property is that
    /// `MigratorTrait::up` applies a migration once per database, which lives
    /// in the ledger and not here. The change it exists to stop is structural:
    /// moving the opt-in out of a migration and into anything that runs at
    /// boot.
    #[tokio::test]
    async fn a_chosen_false_survives_a_redeploy() {
        let conn = db_before_the_upgrade().await;
        let tenant = write_config(
            &conn,
            Uuid::new_v4(),
            false,
            OffsetDateTime::now_utc() - Duration::days(30),
        )
        .await;

        Migrator::up(&conn, None)
            .await
            .expect("the upgrade applies");
        assert!(notify_on_success(&conn, tenant).await);

        // The operator turns success notifications off, deliberately.
        notification_config::Entity::update_many()
            .filter(notification_config::Column::Id.eq(tenant))
            .col_expr(
                notification_config::Column::NotifyOnSuccess,
                sea_orm::sea_query::Expr::value(false),
            )
            .col_expr(
                notification_config::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(OffsetDateTime::now_utc()),
            )
            .exec(&conn)
            .await
            .expect("the tenant's own save applies");

        // The next deploy.
        Migrator::up(&conn, None)
            .await
            .expect("the redeploy applies");

        assert!(
            !notify_on_success(&conn, tenant).await,
            "a redeploy must not re-apply the opt-in over a choice the tenant made"
        );
    }

    /// `down()` is a no-op that succeeds, and must not silently become a
    /// blanket `FALSE` — see this module's header.
    #[tokio::test]
    async fn the_down_migration_leaves_every_row_alone() {
        let conn = migrated_db().await;
        let on = write_config(&conn, Uuid::new_v4(), true, OffsetDateTime::now_utc()).await;

        let manager = sea_orm_migration::SchemaManager::new(&conn);
        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .expect("down applies");

        assert!(
            notify_on_success(&conn, on).await,
            "down must not discard a tenant's own TRUE; the up recorded no before-image to \
             restore, so the honest inverse is nothing"
        );
    }

    /// `MySQL` is refused before anything is touched, the way every earlier
    /// migration in this chain refuses it.
    #[test]
    fn mysql_is_refused_by_name() {
        assert!(super::check_backend(sea_orm::DatabaseBackend::MySql).is_err());
        assert!(super::check_backend(sea_orm::DatabaseBackend::Postgres).is_ok());
        assert!(super::check_backend(sea_orm::DatabaseBackend::Sqlite).is_ok());
    }

    /// A migration missing from `Migrator::migrations()` runs on no deployment
    /// at all. [`MIGRATIONS_BEFORE_THIS_ONE`] is checked here too: it indexes
    /// into that list and is a literal on purpose, so an insertion before this
    /// migration would be caught here rather than by
    /// `db_before_the_upgrade` quietly building the wrong revision.
    #[test]
    fn the_migration_is_registered_at_its_position_in_the_chain() {
        let registered: Vec<String> = Migrator::migrations()
            .iter()
            .map(|m| m.name().to_owned())
            .collect();
        assert!(
            registered.contains(&super::Migration.name().to_owned()),
            "the opt-in migration is not in the applied chain; registered: {registered:?}"
        );
        assert_eq!(
            registered
                .get(MIGRATIONS_BEFORE_THIS_ONE as usize)
                .map(String::as_str),
            Some(super::Migration.name()),
            "MIGRATIONS_BEFORE_THIS_ONE no longer names this migration's position; \
             registered: {registered:?}"
        );
    }
}
