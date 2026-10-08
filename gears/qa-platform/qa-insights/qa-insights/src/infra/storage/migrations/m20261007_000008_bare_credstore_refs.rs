//! Every stored credential-store reference loses a `cred://` prefix:
//! `qa_notification_config.slack_webhook_credstore_ref`,
//! `qa_notification_config.email_smtp_credstore_ref` and
//! `qa_jira_config.api_token_credstore_ref` hold `name` where they held
//! `cred://name`. A value without the prefix is left as it is.
//!
//! # Why
//!
//! In every gear a credential-store reference is exactly what
//! `credstore_sdk::SecretRef::new` accepts, with no scheme prefix; qa-insights
//! was the one gear that accepted `cred://` on top and stripped it, and
//! `domain::ports::validate_credstore_ref` now refuses it. Because the JIRA `PUT`
//! re-validates the *kept* stored reference on every save and
//! `infra::jira::OagwJiraClient` validates again before it provisions, a stored
//! `cred://name` left in place would lock its tenant out of saving any JIRA
//! field; so the stored values are rewritten here, and after this no row of
//! this gear's carries the prefix.
//!
//! **One copy is outside this database and is not rewritten.** An `oagw`
//! upstream `infra::jira::OagwJiraClient` provisioned before this migration
//! keeps the `secret_ref` it was created with, `cred://name` included: the
//! upstream is created once and never updated (`infra::jira::oagw_client`'s
//! header, "Known limitation: a rotated credential reference needs the
//! upstream dropped"). That keeps working, because `oagw`'s apikey plugin
//! still strips a `cred://` prefix before it calls `SecretRef::new`. Only an
//! upstream created after this migration carries the bare name.
//!
//! # Lossless
//!
//! `cred://name` and `name` always named the same secret: `oagw`'s apikey
//! plugin and both of this gear's resolvers (`infra::notify::slack_oagw` and
//! `infra::notify::mail_smtp`) stripped the same seven characters before
//! calling `SecretRef::new`. Rewriting the stored value to the name each of
//! them already resolved changes no secret that any of them reads.
//!
//! # Two dialects, and no `MySQL` blob
//!
//! Like the initial migration, this file serves Postgres and `SQLite` and
//! nothing for `MySQL`: no `MYSQL_UP` exists in this gear, and `up()` refuses
//! that backend outright (five of the initial schema's indexes exceed
//! `InnoDB`'s 3072-byte key limit). Unlike the DDL migrations it needs no
//! `POSTGRES_UP`/`SQLITE_UP` pair: [`STRIP`] is one text that means the same on
//! both, for the reason its own doc gives. [`check_backend`] refuses `MySQL` by
//! name, pointing at the initial migration's header for the reason.
//!
//! # `down`
//!
//! A no-op that succeeds, and deliberately. The prefix meant nothing that the
//! bare name does not, so restoring it would only reintroduce a spelling the
//! gear now refuses; and this migration recorded no before-image, so it could
//! not tell which rows had carried one.

use sea_orm::ConnectionTrait as _;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The same text runs on both dialects: `SUBSTR` is 1-indexed in Postgres
/// and `SQLite` alike and `cred://` is seven characters. The prefix test is an
/// exact `=` on the first seven characters, not a `LIKE`: `SQLite`'s `LIKE` is
/// case-insensitive for ASCII and Postgres' is not, so `CRED://x` would be
/// rewritten on one dialect and kept on the other. `=` is case-sensitive on
/// both, and so is `oagw`'s `strip_prefix`, which is the rule being matched.
const STRIP: &str = r"
UPDATE qa_notification_config
   SET slack_webhook_credstore_ref = SUBSTR(slack_webhook_credstore_ref, 8)
 WHERE SUBSTR(slack_webhook_credstore_ref, 1, 7) = 'cred://';
UPDATE qa_notification_config
   SET email_smtp_credstore_ref = SUBSTR(email_smtp_credstore_ref, 8)
 WHERE SUBSTR(email_smtp_credstore_ref, 1, 7) = 'cred://';
UPDATE qa_jira_config
   SET api_token_credstore_ref = SUBSTR(api_token_credstore_ref, 8)
 WHERE SUBSTR(api_token_credstore_ref, 1, 7) = 'cred://';
";

/// Refuse a backend this gear has no schema for, before touching anything;
/// the same refusal as every earlier migration in this chain.
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

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        check_backend(manager.get_database_backend())?;
        manager.get_connection().execute_unprepared(STRIP).await?;
        Ok(())
    }

    /// Deliberately nothing; see this module's header, "`down`".
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
    use sea_orm::{ActiveValue, EntityTrait};
    use sea_orm_migration::sea_orm::{Database, DatabaseConnection};
    use sea_orm_migration::{MigrationName as _, MigratorTrait as _};
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::super::Migrator;
    use crate::infra::storage::entity::{jira_config, notification_config};

    /// How many migrations stand in front of this one. A literal, for the
    /// reason `m20260929_000007`'s constant of the same name gives.
    const MIGRATIONS_BEFORE_THIS_ONE: u32 = 7;

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

    /// One notification config row, every column set, through the entity, as
    /// `m20260929_000007`'s `write_config` writes one.
    async fn write_config(conn: &DatabaseConnection, slack_ref: &str, smtp_ref: &str) -> Uuid {
        let id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        notification_config::Entity::insert(notification_config::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(Uuid::new_v4()),
            slack_webhook_credstore_ref: ActiveValue::Set(slack_ref.to_owned()),
            slack_channel: ActiveValue::Set("#qa".to_owned()),
            manager_ui_base_url: ActiveValue::Set("https://qa.example.test".to_owned()),
            slack_enabled: ActiveValue::Set(true),
            notify_on_failure: ActiveValue::Set(true),
            notify_on_success: ActiveValue::Set(false),
            notify_on_schedule_completion: ActiveValue::Set(false),
            scheduled_run_slack_enabled: ActiveValue::Set(false),
            scheduled_run_slack_templates: ActiveValue::Set(serde_json::json!({})),
            run_queue_queued_slack_enabled: ActiveValue::Set(false),
            email_smtp_host: ActiveValue::Set(String::new()),
            email_smtp_port: ActiveValue::Set(587),
            email_smtp_username: ActiveValue::Set(String::new()),
            email_smtp_credstore_ref: ActiveValue::Set(smtp_ref.to_owned()),
            email_from: ActiveValue::Set(String::new()),
            email_recipients: ActiveValue::Set(String::new()),
            email_enabled: ActiveValue::Set(false),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        })
        .exec(conn)
        .await
        .expect("the fixture config row is written");
        id
    }

    /// One JIRA config row, every column set, through the entity.
    async fn write_jira(conn: &DatabaseConnection, token_ref: &str) -> Uuid {
        let id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        jira_config::Entity::insert(jira_config::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(Uuid::new_v4()),
            url: ActiveValue::Set("https://jira.example.test".to_owned()),
            project_key: ActiveValue::Set("QA".to_owned()),
            email: ActiveValue::Set("qa@example.test".to_owned()),
            api_token_credstore_ref: ActiveValue::Set(token_ref.to_owned()),
            issue_type: ActiveValue::Set(None),
            enabled: ActiveValue::Set(true),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        })
        .exec(conn)
        .await
        .expect("the fixture JIRA config row is written");
        id
    }

    /// **A stored `cred://` reference comes out of the upgrade as its bare
    /// name, in all three columns, and a bare one is untouched**, as is an
    /// upper-case `CRED://` that `oagw` would not have stripped either. Built
    /// at the revision before this migration and then upgraded: over an empty
    /// database a migration that did nothing would pass.
    #[tokio::test]
    async fn a_prefixed_reference_is_rewritten_to_its_bare_name() {
        let conn = db_before_the_upgrade().await;
        let prefixed = write_config(&conn, "cred://slack-hook", "cred://smtp-pass").await;
        let bare = write_config(&conn, "slack-hook", "").await;
        let jira = write_jira(&conn, "cred://qa-jira-api-token").await;
        let upper = write_config(&conn, "CRED://slack-hook", "").await;

        Migrator::up(&conn, None)
            .await
            .expect("the upgrade applies");

        let row = notification_config::Entity::find_by_id(prefixed)
            .one(&conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.slack_webhook_credstore_ref, "slack-hook");
        assert_eq!(row.email_smtp_credstore_ref, "smtp-pass");
        let row = notification_config::Entity::find_by_id(bare)
            .one(&conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.slack_webhook_credstore_ref, "slack-hook");
        assert_eq!(row.email_smtp_credstore_ref, "");
        let row = jira_config::Entity::find_by_id(jira)
            .one(&conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.api_token_credstore_ref, "qa-jira-api-token");
        let row = notification_config::Entity::find_by_id(upper)
            .one(&conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.slack_webhook_credstore_ref, "CRED://slack-hook",
            "the prefix test is case-sensitive on every dialect, as oagw's strip is"
        );
    }

    /// **[`STRIP`] means on Postgres what it means on `SQLite`.** The
    /// deployed dialect is the one the `SQLite` test above cannot speak for:
    /// `SUBSTR`'s indexing and `=`'s case-sensitivity are what the text relies
    /// on.
    ///
    /// This drives this migration's own `up` over rows written after the
    /// whole chain, not an upgrade from the revision before it: no Postgres
    /// test in this gear builds a database at an earlier revision, and the
    /// `SQLite` test above is the one that proves the upgrade path.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn the_rewrite_runs_on_real_postgres() {
        let harness = crate::infra::storage::test_db::pg_db().await;
        // A raw SeaORM connection alongside the toolkit-db pool: `Db` exposes
        // none, as `m20260818_000001_initial`'s Postgres test also notes.
        let pg = Database::connect(&harness.url)
            .await
            .expect("failed to open a raw connection to the container");
        let prefixed = write_config(&pg, "cred://slack-hook", "cred://smtp-pass").await;
        let bare = write_config(&pg, "credx-slack-hook", "").await;
        let jira = write_jira(&pg, "cred://qa-jira-api-token").await;
        let upper = write_config(&pg, "CRED://slack-hook", "").await;

        let manager = sea_orm_migration::SchemaManager::new(&pg);
        sea_orm_migration::MigrationTrait::up(&super::Migration, &manager)
            .await
            .expect("the rewrite applies on Postgres");

        let row = notification_config::Entity::find_by_id(prefixed)
            .one(&pg)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.slack_webhook_credstore_ref, "slack-hook");
        assert_eq!(row.email_smtp_credstore_ref, "smtp-pass");
        let row = notification_config::Entity::find_by_id(bare)
            .one(&pg)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.slack_webhook_credstore_ref, "credx-slack-hook",
            "a name that merely starts with `cred` is not a prefix"
        );
        let row = jira_config::Entity::find_by_id(jira)
            .one(&pg)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.api_token_credstore_ref, "qa-jira-api-token");
        let row = notification_config::Entity::find_by_id(upper)
            .one(&pg)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.slack_webhook_credstore_ref, "CRED://slack-hook",
            "the prefix test is case-sensitive on every dialect, as oagw's strip is"
        );
    }

    /// A migration missing from `Migrator::migrations()` runs on no deployment
    /// at all, and [`MIGRATIONS_BEFORE_THIS_ONE`] must still name its position,
    /// or `db_before_the_upgrade` builds the wrong revision.
    #[test]
    fn the_migration_is_registered_at_its_position_in_the_chain() {
        let registered: Vec<String> = Migrator::migrations()
            .iter()
            .map(|m| m.name().to_owned())
            .collect();
        assert_eq!(
            registered
                .get(MIGRATIONS_BEFORE_THIS_ONE as usize)
                .map(String::as_str),
            Some(super::Migration.name()),
            "MIGRATIONS_BEFORE_THIS_ONE no longer names this migration's position; \
             registered: {registered:?}"
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
}
