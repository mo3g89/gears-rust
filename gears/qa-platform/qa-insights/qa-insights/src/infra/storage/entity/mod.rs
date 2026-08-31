//! `SeaORM` entities, one per table in `m20260818_000001_initial`.
//!
//! # Nothing links these structs to the schema at compile time
//!
//! Every `table_name` and every field name below is a **runtime string**. A
//! typo compiles cleanly and fails at query time — or worse, `SeaORM` names a
//! column the database does not have and the failure surfaces as an opaque
//! `DbErr` from a code path that looked reviewed. `cargo build` is therefore no
//! evidence at all about this module, exactly as the migration's module header
//! says about itself.
//!
//! `tests::round_trip_every_entity` is the evidence: it applies the **real**
//! `Migrator` and then inserts and reads back one row through each of the
//! eleven entities. That is the only thing that exercises all eleven table
//! names and every one of their 123 column names at once — 123 counted from the
//! DDL, not estimated (120 at Task 11, plus `qa_test_results.app_build` and
//! `qa_test_results.ingest_ordinal` from Task 12, plus
//! `qa_test_results.run_created_at` from Task 21b). It runs on **both**
//! tiers, from the same fixtures:
//!
//! * `tests::every_entity_round_trips_through_the_migrated_schema` —
//!   in-memory `SQLite`, on every `cargo test`. This is the tier that falsifies
//!   the *names*.
//! * `tests::every_entity_round_trips_through_the_real_postgres_schema` —
//!   a real container, behind `--features integration`. This is the tier that
//!   falsifies the *types*: `SQLite` stores every column here as `TEXT` or
//!   `INTEGER`, so a `Uuid`, an `OffsetDateTime` and a `Json` field all agree
//!   with almost anything there, while Postgres has `UUID`, `TIMESTAMPTZ`,
//!   `JSONB` and `BIGINT` as distinct types that reject a mismatched bind — and
//!   Postgres is the dialect this gear deploys.
//!
//! Both live here rather than in the migration module — which is where
//! `qa-runs` keeps its equivalent — because they test *these* structs, and they
//! are what should go red when one of them is edited.
//!
//! # Tenancy is an annotation, and getting it wrong is silent
//!
//! Each entity carries `#[derive(Scopable)]` and a `#[secure(...)]` attribute.
//! Ten of the eleven read
//! `tenant_col = "tenant_id", resource_col = "id", no_owner, no_type`;
//! [`saved_view`] alone reads `owner_col = "owner_id"` in place of `no_owner`,
//! because a saved view is genuinely owned and its unique index keys on the
//! owner. There is no third shape: no table here has a type dimension, and
//! every one of the eleven has a `tenant_id`.
//!
//! A wrong annotation is a cross-tenant leak that compiles and unit-tests
//! green, so `tests::every_entity_names_the_tenancy_columns_its_table_has`
//! asserts the **column names** of all four dimensions, for all eleven
//! entities, through `ScopableEntity`.
//!
//! **The name half is the load-bearing half, and an earlier version of this
//! module got that wrong.** It asserted only that each dimension was
//! *declared* — `is_some()`/`is_none()` — and said, in this paragraph, that it
//! "asserts the mapping". It did not. A presence check passes for
//! `tenant_col = "run_id"`, because `run_id` is a real column and the derive
//! accepts it; the resulting cross-tenant leak was demonstrated green across
//! the entire suite before the test was strengthened. Recorded here because
//! the hole had already been *written down* in this module — in the doc of the
//! very test that then failed to close it — which is the failure mode worth
//! remembering: naming a gap is not closing it.

pub mod ingest_watermark;
pub mod jira_bug;
pub mod jira_config;
pub mod jira_poller_config;
pub mod notification_config;
pub mod notification_log;
pub mod run_notification;
pub mod saved_view;
pub mod test_case_collect;
pub mod test_case_result;
pub mod test_result;

/// Entity tests.
///
/// ## Why this module talks to a raw `SeaORM` connection
///
/// `clippy::disallowed_methods` normally forbids `Entity::insert`/`::find`
/// outside the `SecureORM` wrappers, and that rule is right for every
/// production path. It is allowed here because **the thing under test is the
/// entity-to-column mapping, and `SecureORM` is not in that path**: going
/// through it would add a scope filter to every read and prove nothing extra
/// about a column name. The tenant-scoping tests that *do* go through
/// `SecureORM` live with the repositories in `infra::storage::*_sea_repo`; the
/// migration module carries the same allow for the same shape of reason.
#[cfg(test)]
// `expect` rather than `allow`, so that a suppression which stops being needed
// becomes a build failure instead of quiet residue. It earns that immediately:
// this attribute began as an `allow` listing `unwrap_used` and `expect_used`
// too, and both were already dead -- `clippy.toml` sets `allow-unwrap-in-tests`
// and `allow-expect-in-tests` workspace-wide. Under `expect` they would not
// have compiled.
//
// `cognitive_complexity`: `round_trip_every_entity` scores 52 with **no control
// flow at all** -- no `if`, no `match`, no loop -- only eleven independent
// insert-then-read blocks; the score is entirely the `assert*!` macros' expanded
// branches. Splitting it into eleven functions would move the fixture ids into
// eleven signatures and buy nothing the numbered section comments do not.
#[expect(
    clippy::disallowed_methods,
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "test module: raw SeaORM is the thing under test, and the \
              round-trip fixture is long and branchless by construction"
)]
mod tests {
    use sea_orm::IdenStatic;
    use sea_orm::entity::prelude::*;
    use sea_orm::{ActiveValue, ConnectOptions, Database, DatabaseConnection};
    use time::macros::datetime;
    use time::{Duration, OffsetDateTime};
    use toolkit_db::secure::ScopableEntity;
    use uuid::Uuid;

    use super::{
        ingest_watermark, jira_bug, jira_config, jira_poller_config, notification_config,
        notification_log, run_notification, saved_view, test_case_collect, test_case_result,
        test_result,
    };

    /// Deterministic fixture ids, so a failure names a stable value.
    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// The fixture instant, matching the migration module's `TS`.
    fn now() -> OffsetDateTime {
        datetime!(2026-08-18 00:00:00 UTC)
    }

    /// In-memory `SQLite` with this gear's real migrations applied through the
    /// real [`crate::infra::storage::migrations::Migrator`].
    ///
    /// `max_connections(1)` **is** load-bearing: each `SQLite` `:memory:`
    /// connection is its own database, so a larger pool would let an insert
    /// land on a connection where the migration never ran.
    async fn migrated_db() -> DatabaseConnection {
        use sea_orm_migration::MigratorTrait;

        let mut opts = ConnectOptions::new("sqlite::memory:");
        opts.max_connections(1).min_connections(1);
        let conn = Database::connect(opts)
            .await
            .expect("failed to connect to in-memory sqlite database");

        let manager = sea_orm_migration::SchemaManager::new(&conn);
        for migration in crate::infra::storage::migrations::Migrator::migrations() {
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-insights migrations");
        }
        conn
    }

    /// **The point of this module.** Every table name and every column name in
    /// all eleven entities, exercised by a real INSERT and a real SELECT
    /// against the real migration.
    ///
    /// Every field of every `ActiveModel` is named explicitly rather than
    /// filled with `..Default::default()`, which would also compile: a wildcard
    /// lets a column nobody named join the literal silently, and a column
    /// nothing names is a column nothing checks. The read-backs then assert the
    /// values that would survive a *wrong* column mapping unnoticed — the
    /// nullable/non-null boundary, the two `Json` columns, and the numeric
    /// widths.
    ///
    /// Factored out of its two callers so the identical fixtures run on both
    /// tiers; see
    /// [`every_entity_round_trips_through_the_real_postgres_schema`] for why
    /// the `SQLite` tier alone is not enough.
    async fn round_trip_every_entity(conn: &DatabaseConnection) {
        let tenant = uuid(1);
        let run_id = uuid(2);
        let repo_id = uuid(3);
        let platform_id = uuid(4);
        let owner_id = uuid(5);

        // ---- 1. qa_test_results -------------------------------------------
        test_result::ActiveModel {
            id: ActiveValue::Set(uuid(10)),
            tenant_id: ActiveValue::Set(tenant),
            run_id: ActiveValue::Set(run_id),
            test_file: ActiveValue::Set("tests/authn/test_login.py".to_owned()),
            test_name: ActiveValue::Set("AuthN Login".to_owned()),
            status: ActiveValue::Set("PASSED".to_owned()),
            duration: ActiveValue::Set(Some("85.06s (0:01:25)".to_owned())),
            launch_id: ActiveValue::Set(Some("7204".to_owned())),
            jira_key: ActiveValue::Set(Some("VHP-2618".to_owned())),
            product_version: ActiveValue::Set(Some("5.0.1".to_owned())),
            app_build: ActiveValue::Set(Some("20260818.3".to_owned())),
            platform_id: ActiveValue::Set(Some(platform_id)),
            repo_id: ActiveValue::Set(Some(repo_id)),
            plan_path: ActiveValue::Set(Some("plans/smoke/plan.yaml".to_owned())),
            branch: ActiveValue::Set(Some("main".to_owned())),
            run_finished_at: ActiveValue::Set(Some(now())),
            run_created_at: ActiveValue::Set(Some(now() - Duration::hours(2))),
            ingest_ordinal: ActiveValue::Set(3),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();

        let row = test_result::Entity::find_by_id(uuid(10))
            .one(conn)
            .await
            .unwrap()
            .expect("the result row must read back");
        assert_eq!(
            row.duration.as_deref(),
            Some("85.06s (0:01:25)"),
            "the runner's duration text is stored verbatim, not parsed"
        );
        assert_eq!(
            row.product_version.as_deref(),
            Some("5.0.1"),
            "the eight denormalized run columns are what let analytics avoid a \
             cross-gear join; a mis-mapped one is silently NULL"
        );
        assert_eq!(
            row.app_build.as_deref(),
            Some("20260818.3"),
            "app_build is the analytics *projection* and product_version the \
             *filter*; a mis-mapped app_build empties Task 24's build \
             distribution on a screen that still renders"
        );
        assert_eq!(row.repo_id, Some(repo_id));
        assert_eq!(row.plan_path.as_deref(), Some("plans/smoke/plan.yaml"));
        assert_eq!(row.branch.as_deref(), Some("main"));
        assert_eq!(row.run_finished_at, Some(now()));
        assert_eq!(
            row.run_created_at,
            Some(now() - Duration::hours(2)),
            "run_created_at is a distinct instant from run_finished_at and from \
             created_at, and it is the fallback half of the dashboard's window \
             expression; a mapper that transposed it with either would still \
             round-trip a timestamp"
        );
        assert_eq!(
            row.ingest_ordinal, 3,
            "the batch-position tiebreak must survive the round trip; a \
             mis-mapped one silently reverts the ordering to the UUID key"
        );

        // ---- 2. qa_test_case_results --------------------------------------
        test_case_result::ActiveModel {
            id: ActiveValue::Set(uuid(11)),
            tenant_id: ActiveValue::Set(tenant),
            run_id: ActiveValue::Set(run_id),
            test_file: ActiveValue::Set("tests/authn/test_login.py".to_owned()),
            nodeid: ActiveValue::Set("tests/authn/test_login.py::test_ok[1]".to_owned()),
            name: ActiveValue::Set("test_ok".to_owned()),
            status: ActiveValue::Set("XFAIL".to_owned()),
            duration: ActiveValue::Set(Some("0.31s".to_owned())),
            reason: ActiveValue::Set(Some("known upstream defect".to_owned())),
            ticket: ActiveValue::Set(Some("VHP-9".to_owned())),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();

        let case = test_case_result::Entity::find_by_id(uuid(11))
            .one(conn)
            .await
            .unwrap()
            .expect("the case row must read back");
        assert_eq!(
            case.name, "test_ok",
            "the case-level column is `name`, not `test_name` -- the two tables \
             spell this differently and legacy does too"
        );
        assert_eq!(case.nodeid, "tests/authn/test_login.py::test_ok[1]");
        assert_eq!(case.reason.as_deref(), Some("known upstream defect"));

        // ---- 3. qa_test_case_collect --------------------------------------
        test_case_collect::ActiveModel {
            id: ActiveValue::Set(uuid(12)),
            tenant_id: ActiveValue::Set(tenant),
            repo_id: ActiveValue::Set(repo_id),
            branch: ActiveValue::Set("main".to_owned()),
            test_file: ActiveValue::Set("tests/authn/test_login.py".to_owned()),
            case_count: ActiveValue::Set(42),
            collected_at: ActiveValue::Set(now()),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();

        let collect = test_case_collect::Entity::find_by_id(uuid(12))
            .one(conn)
            .await
            .unwrap()
            .expect("the collect row must read back");
        assert_eq!(collect.case_count, 42);
        assert_eq!(
            collect.collected_at,
            now(),
            "`collected_at` is when the job ran, not when the row was touched; \
             mapping it onto `updated_at` would compile"
        );

        // ---- 4. qa_analytics_saved_views ----------------------------------
        saved_view::ActiveModel {
            id: ActiveValue::Set(uuid(13)),
            tenant_id: ActiveValue::Set(tenant),
            owner_id: ActiveValue::Set(owner_id),
            scope: ActiveValue::Set("plan".to_owned()),
            repo_id: ActiveValue::Set(Some(repo_id)),
            plan_path: ActiveValue::Set(Some("plans/smoke/plan.yaml".to_owned())),
            plan_key: ActiveValue::Set(format!("{repo_id}/plans/smoke/plan.yaml")),
            name: ActiveValue::Set("My View".to_owned()),
            query_json: ActiveValue::Set(serde_json::json!({"branch": "main"})),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();

        let view = saved_view::Entity::find_by_id(uuid(13))
            .one(conn)
            .await
            .unwrap()
            .expect("the saved view must read back");
        assert_eq!(
            view.query_json,
            serde_json::json!({"branch": "main"}),
            "`query_json` is opaque and must survive the column whole"
        );
        assert_eq!(
            view.plan_key,
            format!("{repo_id}/plans/smoke/plan.yaml"),
            "the repository writes `plan_key`; nothing in the database checks \
             that it agrees with repo_id/plan_path (migration obligation #2)"
        );

        // ---- 5. qa_jira_bugs ----------------------------------------------
        jira_bug::ActiveModel {
            id: ActiveValue::Set(uuid(14)),
            tenant_id: ActiveValue::Set(tenant),
            jira_key: ActiveValue::Set("VHP-1".to_owned()),
            test_name: ActiveValue::Set("AuthN Login".to_owned()),
            repo_id: ActiveValue::Set(repo_id),
            plan_path: ActiveValue::Set("plans/smoke/plan.yaml".to_owned()),
            app_version: ActiveValue::Set(Some("5.0.1".to_owned())),
            platform_id: ActiveValue::Set(Some(platform_id)),
            status: ActiveValue::Set("Open".to_owned()),
            summary: ActiveValue::Set("login flaps".to_owned()),
            resolved_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();

        let bug = jira_bug::Entity::find_by_id(uuid(14))
            .one(conn)
            .await
            .unwrap()
            .expect("the bug must read back");
        assert_eq!(bug.status, "Open");
        assert_eq!(
            bug.resolved_at, None,
            "an open bug has no resolution instant; the poller sets it"
        );

        // ---- 6. qa_jira_config --------------------------------------------
        jira_config::ActiveModel {
            id: ActiveValue::Set(uuid(15)),
            tenant_id: ActiveValue::Set(tenant),
            url: ActiveValue::Set("https://jira.example.com".to_owned()),
            project_key: ActiveValue::Set("VHP".to_owned()),
            email: ActiveValue::Set("qa@example.com".to_owned()),
            api_token_credstore_ref: ActiveValue::Set("cred://jira-token".to_owned()),
            issue_type: ActiveValue::Set(Some("Bug".to_owned())),
            enabled: ActiveValue::Set(true),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();

        let jira = jira_config::Entity::find_by_id(uuid(15))
            .one(conn)
            .await
            .unwrap()
            .expect("the jira config must read back");
        assert!(jira.enabled);
        assert_eq!(
            jira.api_token_credstore_ref, "cred://jira-token",
            "this column holds a reference and never token material"
        );

        // ---- 7. qa_jira_poller_config -------------------------------------
        jira_poller_config::ActiveModel {
            id: ActiveValue::Set(uuid(16)),
            tenant_id: ActiveValue::Set(tenant),
            poll_interval_seconds: ActiveValue::Set(300),
            auto_rerun_on_resolve: ActiveValue::Set(true),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();

        let poller = jira_poller_config::Entity::find_by_id(uuid(16))
            .one(conn)
            .await
            .unwrap()
            .expect("the poller config must read back");
        assert_eq!(
            poller.poll_interval_seconds, 300,
            "BIGINT, so i64 -- an i32 field would compile and truncate"
        );
        assert!(poller.auto_rerun_on_resolve);

        // ---- 8. qa_notification_config ------------------------------------
        notification_config::ActiveModel {
            id: ActiveValue::Set(uuid(17)),
            tenant_id: ActiveValue::Set(tenant),
            slack_webhook_credstore_ref: ActiveValue::Set("cred://slack-hook".to_owned()),
            slack_channel: ActiveValue::Set("#qa".to_owned()),
            manager_ui_base_url: ActiveValue::Set("https://qa.example.com".to_owned()),
            slack_enabled: ActiveValue::Set(true),
            notify_on_failure: ActiveValue::Set(true),
            notify_on_success: ActiveValue::Set(false),
            notify_on_schedule_completion: ActiveValue::Set(false),
            scheduled_run_slack_enabled: ActiveValue::Set(true),
            scheduled_run_slack_templates: ActiveValue::Set(
                serde_json::json!({"failed": {"enabled": true}}),
            ),
            run_queue_queued_slack_enabled: ActiveValue::Set(false),
            email_smtp_host: ActiveValue::Set("smtp.example.com".to_owned()),
            email_smtp_port: ActiveValue::Set(587),
            email_from: ActiveValue::Set("qa@example.com".to_owned()),
            email_recipients: ActiveValue::Set("a@example.com, b@example.com".to_owned()),
            email_enabled: ActiveValue::Set(true),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();

        let notify = notification_config::Entity::find_by_id(uuid(17))
            .one(conn)
            .await
            .unwrap()
            .expect("the notification config must read back");
        assert_eq!(
            notify.scheduled_run_slack_templates,
            serde_json::json!({"failed": {"enabled": true}}),
            "the six Block Kit templates are one JSON document, written and \
             read whole"
        );
        assert_eq!(notify.email_smtp_port, 587);
        assert_eq!(
            notify.email_recipients, "a@example.com, b@example.com",
            "one string as the operator typed it -- splitting belongs to the \
             mailer, and a round trip must not reformat it"
        );
        assert!(
            notify.notify_on_failure && !notify.notify_on_success,
            "the seven booleans must not be transposed; only notify_on_failure \
             starts on"
        );

        // ---- 9. qa_run_notifications --------------------------------------
        run_notification::ActiveModel {
            id: ActiveValue::Set(uuid(18)),
            tenant_id: ActiveValue::Set(tenant),
            run_id: ActiveValue::Set(run_id),
            notification_kind: ActiveValue::Set("scheduled_run_slack".to_owned()),
            event_type: ActiveValue::Set("failed".to_owned()),
            sent_at: ActiveValue::Set(now()),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();

        let claim = run_notification::Entity::find_by_id(uuid(18))
            .one(conn)
            .await
            .unwrap()
            .expect("the claim must read back");
        assert_eq!(claim.notification_kind, "scheduled_run_slack");
        assert_eq!(claim.event_type, "failed");

        // ---- 10. qa_notification_log --------------------------------------
        notification_log::ActiveModel {
            id: ActiveValue::Set(uuid(19)),
            tenant_id: ActiveValue::Set(tenant),
            run_id: ActiveValue::Set(None),
            channel: ActiveValue::Set("slack".to_owned()),
            event_type: ActiveValue::Set(String::new()),
            outcome: ActiveValue::Set("sent".to_owned()),
            detail: ActiveValue::Set(String::new()),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();

        let log = notification_log::Entity::find_by_id(uuid(19))
            .one(conn)
            .await
            .unwrap()
            .expect("the log entry must read back");
        assert_eq!(
            log.run_id, None,
            "a settings /test send belongs to no run, which legacy records as \
             the empty string and this column records as NULL"
        );
        assert_eq!(log.detail, "", "'' on success, never NULL");

        // ---- 11. qa_ingest_watermarks -------------------------------------
        ingest_watermark::ActiveModel {
            id: ActiveValue::Set(uuid(20)),
            tenant_id: ActiveValue::Set(tenant),
            last_reconciled_finished_at: ActiveValue::Set(Some(now())),
            last_swept_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(conn)
        .await
        .unwrap();

        let mark = ingest_watermark::Entity::find_by_id(uuid(20))
            .one(conn)
            .await
            .unwrap()
            .expect("the watermark must read back");
        assert_eq!(mark.last_reconciled_finished_at, Some(now()));
        assert_eq!(
            mark.last_swept_at, None,
            "NULL means 'never swept' and must not decay into the epoch, which \
             would mean 'swept up to 1970'"
        );
    }

    /// The `SQLite` tier: eleven entities against the real `Migrator`,
    /// in-memory.
    #[tokio::test]
    async fn every_entity_round_trips_through_the_migrated_schema() {
        let conn = migrated_db().await;
        round_trip_every_entity(&conn).await;
    }

    /// The same eleven entities against **real Postgres**, which is the dialect
    /// this gear deploys.
    ///
    /// Not redundant with the `SQLite` tier, and the difference is exactly
    /// where entities go wrong. `SQLite` has no real types: every column above
    /// is `TEXT` or `INTEGER` there, so a `Uuid` field, an `OffsetDateTime`
    /// field and a `Json` field all round-trip through string storage and agree
    /// with almost anything. Postgres has `UUID`, `TIMESTAMPTZ`, `JSONB`,
    /// `BIGINT` and `BOOLEAN` as distinct types that reject a mismatched bind,
    /// so this is the tier that can actually falsify the type half of the
    /// mapping — the `SQLite` tier only falsifies the *name* half.
    ///
    /// Gated behind `integration`, so a default `cargo test` needs no Docker:
    /// `cargo test -p qa-insights --features integration --lib`.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn every_entity_round_trips_through_the_real_postgres_schema() {
        let harness = crate::infra::storage::test_db::pg_db().await;

        // A raw `SeaORM` connection alongside the `toolkit-db` pool: `Db`
        // exposes none, and an entity insert needs one. The container already
        // has this gear's real migrations applied by `pg_db`.
        let conn = Database::connect(&harness.url)
            .await
            .expect("failed to open a raw connection to the container");

        round_trip_every_entity(&conn).await;
    }

    /// The `#[secure(...)]` annotation of every entity, asserted through the
    /// trait it generates and **as the column strings that reach SQL**.
    ///
    /// # This is the cross-tenant-leak guard, and it has to check names
    ///
    /// `#[secure(...)]` is what lets `SecureORM` constrain a query by tenant at
    /// all. Get it wrong and the entity compiles, every repository over it
    /// compiles, and both round-trip tiers stay green — a leak with no symptom.
    ///
    /// **Checking that a dimension is merely *declared* is not enough, and this
    /// test used to do only that.** A misspelling like `tenant_col = "tenat_id"`
    /// does fail the derive, so a presence check appears to cover the case. But
    /// `tenant_col = "run_id"` does not fail anything: `run_id` is a real column
    /// of a real entity, the derive accepts it, a presence check sees
    /// `Some(_)` and passes, and every scoped query then filters by the wrong
    /// column. Verified by mutation, twice: with `tenant_col = "run_id"` on
    /// `test_case_result` — and independently on `notification_log` — the whole
    /// suite passed until this test began comparing strings for all eleven
    /// entities. (22 tests at the time; 21 after that change merged two weaker
    /// tests into this one.)
    ///
    /// So the assertion below is the complete four-dimension mapping, by name,
    /// for every entity — not a sample, and not a shape.
    ///
    /// Compared via [`IdenStatic::as_str`] rather than by value because
    /// `SeaORM`'s generated `Column` enum derives no `PartialEq`, and the
    /// rendered string is closer to what actually reaches SQL anyway.
    #[test]
    fn every_entity_names_the_tenancy_columns_its_table_has() {
        /// The `(tenant, resource, owner, type)` columns of one entity, by
        /// name.
        fn dims<E: ScopableEntity>() -> [Option<String>; 4] {
            assert!(
                !E::IS_UNRESTRICTED,
                "no entity in this gear is unrestricted"
            );
            let name = |c: Option<E::Column>| c.map(|c| c.as_str().to_owned());
            [
                name(E::tenant_col()),
                name(E::resource_col()),
                name(E::owner_col()),
                name(E::type_col()),
            ]
        }

        /// Readability shim: the expected tuple as `&str`s.
        fn expect(
            tenant: Option<&str>,
            resource: Option<&str>,
            owner: Option<&str>,
            ty: Option<&str>,
        ) -> [Option<String>; 4] {
            [tenant, resource, owner, ty].map(|c| c.map(str::to_owned))
        }

        // Ten of eleven: tenant + resource by name, no owner, no type.
        let plain = expect(Some("tenant_id"), Some("id"), None, None);
        assert_eq!(dims::<test_result::Entity>(), plain, "qa_test_results");
        assert_eq!(
            dims::<test_case_result::Entity>(),
            plain,
            "qa_test_case_results"
        );
        assert_eq!(
            dims::<test_case_collect::Entity>(),
            plain,
            "qa_test_case_collect"
        );
        assert_eq!(dims::<jira_bug::Entity>(), plain, "qa_jira_bugs");
        assert_eq!(dims::<jira_config::Entity>(), plain, "qa_jira_config");
        assert_eq!(
            dims::<jira_poller_config::Entity>(),
            plain,
            "qa_jira_poller_config"
        );
        assert_eq!(
            dims::<notification_config::Entity>(),
            plain,
            "qa_notification_config"
        );
        assert_eq!(
            dims::<run_notification::Entity>(),
            plain,
            "qa_run_notifications"
        );
        assert_eq!(
            dims::<notification_log::Entity>(),
            plain,
            "qa_notification_log"
        );
        assert_eq!(
            dims::<ingest_watermark::Entity>(),
            plain,
            "qa_ingest_watermarks"
        );

        // The one exception, and the reason it is one: `qa_analytics_saved_views`
        // has an `owner_id` column and its unique index keys on it. `owner_id`
        // specifically -- not `id`, and not `repo_id`.
        assert_eq!(
            dims::<saved_view::Entity>(),
            expect(Some("tenant_id"), Some("id"), Some("owner_id"), None),
            "a saved view is owned; declaring `no_owner` here, or pointing the \
             owner dimension at any other column, would compile and would stop \
             an owner-scoped policy from narrowing anything"
        );
    }
}
