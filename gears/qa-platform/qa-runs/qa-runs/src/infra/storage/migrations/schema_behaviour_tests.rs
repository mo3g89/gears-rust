//! Behaviour the collapsed schema must have, which a schema dump cannot prove.
//!
//! Column names, types and index shapes are asserted by
//! `m20260813_000003_initial`'s own inventory tests and, end to end, by
//! diffing a `pg_dump` of the applied chain. Neither can see what this file
//! asserts: that a foreign key really cascades, that an omitted column really
//! takes its declared default, and that a unique index is scoped to the tenant
//! rather than global.
//!
//! These tests were carried over from the five migrations the chain was
//! collapsed into `m20260813_000003_initial`. What was dropped with those files
//! was the delta mechanics — "this migration adds exactly these three columns",
//! "its `down` drops exactly what its `up` added" — which describe a step that
//! no longer exists. What is here is the behaviour that survived the step.

// THE ONE LINT ALLOWANCE IN THIS FILE, and why it is here rather than in
// `clippy.toml`.
//
// `Select::one` is on `clippy.toml`'s `disallowed-methods` list because a
// production read that bypasses `SecureSelect` escapes tenant scoping. Nothing
// below is a production read, and there is no tenant to scope with: every test
// here opens a raw `SeaORM` connection against a freshly migrated, throwaway
// database and writes the rows it then reads back. The subject is the SCHEMA's
// behaviour -- that a foreign key really cascades, that an omitted column
// really takes its declared default, that a unique index is scoped to the
// tenant -- not the access path a production read travels. Routing these
// through `SecureSelect` means constructing an `AccessScope` that nothing in a
// migration module represents, and the assertion would then be about the
// wrapper rather than about the schema: the test would say it exercises the
// collapsed schema while exercising something else.
//
// IT IS SCOPED TO THIS FILE BECAUSE THAT IS THE NARROWEST FORM THAT EXISTS.
// Clippy's `disallowed-methods` is a single global list of paths with no
// per-file, per-module or per-crate form, so an "allowance" written in
// `clippy.toml` is not an allowance -- it deletes `Select::one` from the list
// for every crate in the workspace, including the repositories the rule was
// written for. An inner attribute stops at this file's last line, and moves
// with the file.
//
// The per-migration test modules in this directory carry the same allowance
// for the same reason; `m20260813_000003_initial`'s test module
// carries the long form of the argument.
#![allow(
    clippy::disallowed_methods,
    reason = "a schema behaviour test drives a raw connection and has no tenant to scope \
              with -- see the comment above this attribute"
)]

use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
    DatabaseConnection, EntityTrait, QueryFilter,
};
use sea_orm_migration::{MigratorTrait, SchemaManager};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::infra::storage::entity::{run, run_test_result, schedule, schedule_tick};

/// A per-test row's node id, with `ticket` deliberately different from
/// `jira_key` in the fixture that uses it.
const NODEID: &str =
    "tests/authn/test_error_handling.py::TestFailClosed::test_fail_closed[tls]";

/// Deterministic fixture UUIDs: `Uuid::new_v4` would make a failure
/// unreproducible.
fn uuid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// 2026-08-13 00:00:00 UTC, the fixture instant this crate uses throughout.
fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
}

/// An in-memory database with the whole (now single-migration) chain applied.
async fn migrated_db() -> DatabaseConnection {
    let mut opts = ConnectOptions::new("sqlite::memory:");
    opts.max_connections(1).min_connections(1);
    let conn = Database::connect(opts)
        .await
        .expect("failed to connect to in-memory sqlite database");
    conn.execute_unprepared("PRAGMA foreign_keys = ON;")
        .await
        .expect("failed to enable sqlite foreign key enforcement");

    let manager = SchemaManager::new(&conn);
    for migration in super::Migrator::migrations() {
        migration
            .up(&manager)
            .await
            .expect("failed to run qa-runs migrations");
    }
    conn
}


/// Collect the `name` column of a one-column query.
async fn name_column(conn: &DatabaseConnection, sql: String) -> Vec<String> {
    use sea_orm::Statement;
    conn.query_all_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        sql,
    ))
    .await
    .unwrap()
    .iter()
    .filter_map(|row| row.try_get::<String>("", "name").ok())
    .collect()
}

/// Index names on a table, from `SQLite`'s own catalogue, excluding the
/// implicit `sqlite_autoindex_*` entries that back `PRIMARY KEY`.
async fn index_names(conn: &DatabaseConnection, table: &str) -> Vec<String> {
    let mut names = name_column(
        conn,
        format!(
            "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='{table}' \
             AND name NOT LIKE 'sqlite_autoindex%'"
        ),
    )
    .await;
    names.sort();
    names
}

/// Column names of one index, **in index order**, from `PRAGMA index_info`.
async fn index_columns(conn: &DatabaseConnection, index: &str) -> Vec<String> {
    name_column(
        conn,
        format!("SELECT name FROM pragma_index_info('{index}')"),
    )
    .await
}

/// A schedule `ActiveModel` with **every** column set, so the generated
/// `INSERT` names all of them and a column this entity spells differently
/// from the DDL fails right here.
fn schedule_am(id: Uuid, tenant: Uuid, name: &str) -> schedule::ActiveModel {
    schedule::ActiveModel {
        id: ActiveValue::Set(id),
        tenant_id: ActiveValue::Set(tenant),
        name: ActiveValue::Set(name.to_owned()),
        run_kind: ActiveValue::Set("test".to_owned()),
        target_repo_id: ActiveValue::Set(Some(uuid(0x10))),
        target_path: ActiveValue::Set(Some("tests/smoke/plan.yaml".to_owned())),
        target_test_file: ActiveValue::Set(Some("tests/authn/test_a.py".to_owned())),
        target_custom_plan_id: ActiveValue::Set(None),
        target_collect_url: ActiveValue::Set(None),
        environment_id: ActiveValue::Set(Some(uuid(0x11))),
        branch: ActiveValue::Set(Some("release/5.0".to_owned())),
        cron: ActiveValue::Set("0 3 * * *".to_owned()),
        exclusive_choice: ActiveValue::Set("false".to_owned()),
        enabled: ActiveValue::Set(true),
        include_tags: ActiveValue::Set(serde_json::json!(["smoke"])),
        exclude_tags: ActiveValue::Set(serde_json::json!(["slow"])),
        parameters: ActiveValue::Set(serde_json::json!([{"name": "A", "value": "1"}])),
        // Added by `m20260818_000007_schedule_notifications` (folded into `migrations::m20260813_000003_initial` by the docs squash). Named here
        // because this literal's own doc says every column is set, so the
        // generated INSERT names all of them - which is what makes a column
        // this entity spells differently from the DDL fail right here.
        slack_notifications_enabled: ActiveValue::Set(true),
        slack_channel: ActiveValue::Set(Some("#qa-alerts".to_owned())),
        slack_notification_events: ActiveValue::Set(serde_json::json!(["failed"])),
        last_fired_tick: ActiveValue::Set(Some(now())),
        created_at: ActiveValue::Set(now()),
        updated_at: ActiveValue::Set(now()),
    }
}

fn tick_am(id: Uuid, tenant: Uuid, schedule_id: Uuid) -> schedule_tick::ActiveModel {
    schedule_tick::ActiveModel {
        id: ActiveValue::Set(id),
        tenant_id: ActiveValue::Set(tenant),
        schedule_id: ActiveValue::Set(schedule_id),
        due_at: ActiveValue::Set(now()),
        claimed_by: ActiveValue::Set("qa-runs-0".to_owned()),
        claimed_at: ActiveValue::Set(now()),
        run_id: ActiveValue::Set(Some(uuid(0x20))),
        error: ActiveValue::Set(Some("boom".to_owned())),
        created_at: ActiveValue::Set(now()),
    }
}

/// A minimal parent run. Copied from the sibling migration's fixture
/// because `run::ActiveModel` is a struct literal that must name every
/// field; nothing here is asserted on, it only satisfies the foreign key.
fn run_am(id: Uuid, tenant: Uuid, name: &str) -> run::ActiveModel {
    run::ActiveModel {
        id: ActiveValue::Set(id),
        tenant_id: ActiveValue::Set(tenant),
        name: ActiveValue::Set(name.to_owned()),
        run_kind: ActiveValue::Set("plan".to_owned()),
        target_repo_id: ActiveValue::Set(Some(uuid(0x10))),
        target_path: ActiveValue::Set(Some("tests/smoke/plan.yaml".to_owned())),
        target_test_file: ActiveValue::Set(None),
        target_custom_plan_id: ActiveValue::Set(None),
        target_collect_url: ActiveValue::Set(None),
        environment_id: ActiveValue::Set(Some(uuid(0x11))),
        test_version: ActiveValue::Set(None),
        app_version: ActiveValue::Set(None),
        app_build: ActiveValue::Set(None),
        state: ActiveValue::Set("running".to_owned()),
        resolved_exclusive: ActiveValue::Set(false),
        exclusive_tier: ActiveValue::Set("plan.yaml".to_owned()),
        is_validation: ActiveValue::Set(false),
        parameters: ActiveValue::Set(serde_json::json!([])),
        include_tags: ActiveValue::Set(serde_json::json!([])),
        exclude_tags: ActiveValue::Set(serde_json::json!([])),
        source: ActiveValue::Set("manual".to_owned()),
        schedule_id: ActiveValue::Set(None),
        bundle_ids: ActiveValue::Set(serde_json::json!([])),
        execution_ref: ActiveValue::Set(None),
        log_storage_ref: ActiveValue::Set(None),
        timeout_at: ActiveValue::Set(None),
        started_at: ActiveValue::Set(None),
        finished_at: ActiveValue::Set(None),
        error: ActiveValue::Set(None),
        passed: ActiveValue::Set(0),
        failed: ActiveValue::Set(0),
        skipped: ActiveValue::Set(0),
        in_progress: ActiveValue::Set(0),
        xfail: ActiveValue::Set(0),
        xpass: ActiveValue::Set(0),
        total: ActiveValue::Set(0),
        created_at: ActiveValue::Set(now()),
        updated_at: ActiveValue::Set(now()),
    }
}

fn result_am(id: Uuid, tenant: Uuid, run_id: Uuid) -> run_test_result::ActiveModel {
    run_test_result::ActiveModel {
        id: ActiveValue::Set(id),
        tenant_id: ActiveValue::Set(tenant),
        run_id: ActiveValue::Set(run_id),
        test_file: ActiveValue::Set("tests/authn/test_error_handling.py".to_owned()),
        test_name: ActiveValue::Set("test_fail_closed".to_owned()),
        status: ActiveValue::Set("XFAIL".to_owned()),
        duration: ActiveValue::Set(Some("85.06s (0:01:25)".to_owned())),
        launch_id: ActiveValue::Set(Some("7204".to_owned())),
        jira_key: ActiveValue::Set(Some("VHP-2618".to_owned())),
        nodeid: ActiveValue::Set(NODEID.to_owned()),
        reason: ActiveValue::Set(Some("known upstream defect".to_owned())),
        ticket: ActiveValue::Set(Some("VHP-3117".to_owned())),
        created_at: ActiveValue::Set(now()),
        updated_at: ActiveValue::Set(now()),
    }
}

/// Insert one schedule through the entity, with **every** column named.
///
/// The struct literal names every field for the reason
/// `m20260818_000005_case_fidelity` (folded into `migrations::m20260813_000003_initial` by the docs squash) argues: a `..Default::default()` absorbs
/// the next column added after it without a compile error. All three
/// notification columns are set to distinct non-default values, so a caller
/// asserting on them cannot pass by reading a default back.
async fn seeded_schedule(conn: &DatabaseConnection, id: Uuid, name: &str) {
    schedule::ActiveModel {
        id: ActiveValue::Set(id),
        tenant_id: ActiveValue::Set(uuid(2)),
        name: ActiveValue::Set(name.to_owned()),
        run_kind: ActiveValue::Set("plan".to_owned()),
        target_repo_id: ActiveValue::Set(Some(uuid(0x10))),
        target_path: ActiveValue::Set(Some("plans/smoke.yaml".to_owned())),
        target_test_file: ActiveValue::Set(None),
        target_custom_plan_id: ActiveValue::Set(None),
        target_collect_url: ActiveValue::Set(None),
        environment_id: ActiveValue::Set(None),
        branch: ActiveValue::Set(Some("main".to_owned())),
        cron: ActiveValue::Set("0 3 * * *".to_owned()),
        exclusive_choice: ActiveValue::Set("auto".to_owned()),
        enabled: ActiveValue::Set(true),
        include_tags: ActiveValue::Set(serde_json::json!([])),
        exclude_tags: ActiveValue::Set(serde_json::json!([])),
        parameters: ActiveValue::Set(serde_json::json!([])),
        slack_notifications_enabled: ActiveValue::Set(true),
        slack_channel: ActiveValue::Set(Some("#qa-alerts".to_owned())),
        slack_notification_events: ActiveValue::Set(serde_json::json!(["failed", "error"])),
        last_fired_tick: ActiveValue::Set(None),
        created_at: ActiveValue::Set(now()),
        updated_at: ActiveValue::Set(now()),
    }
    .insert(conn)
    .await
    .unwrap();
}

/// Both table names and every column name in both entities, exercised by a
/// real INSERT and a real SELECT.
#[tokio::test]
async fn both_schedule_entities_round_trip_through_the_migrated_schema() {
    let conn = migrated_db().await;
    let tenant = uuid(1);
    let schedule_id = uuid(2);

    schedule_am(schedule_id, tenant, "nightly")
        .insert(&conn)
        .await
        .unwrap();
    tick_am(uuid(3), tenant, schedule_id)
        .insert(&conn)
        .await
        .unwrap();

    let stored = schedule::Entity::find_by_id(schedule_id)
        .one(&conn)
        .await
        .unwrap()
        .expect("the schedule row must read back");
    assert_eq!(stored.name, "nightly");
    assert_eq!(stored.cron, "0 3 * * *");
    assert_eq!(stored.exclusive_choice, "false");
    assert!(stored.enabled);
    assert_eq!(stored.branch.as_deref(), Some("release/5.0"));
    assert_eq!(
        stored.target_test_file.as_deref(),
        Some("tests/authn/test_a.py")
    );
    assert_eq!(stored.include_tags, serde_json::json!(["smoke"]));
    assert_eq!(stored.last_fired_tick, Some(now()));

    let tick = schedule_tick::Entity::find_by_id(uuid(3))
        .one(&conn)
        .await
        .unwrap()
        .expect("the tick row must read back");
    assert_eq!(tick.schedule_id, schedule_id);
    assert_eq!(tick.due_at, now());
    assert_eq!(tick.claimed_by, "qa-runs-0");
    assert_eq!(tick.run_id, Some(uuid(0x20)));
}

/// The `NOT NULL DEFAULT 'auto'` on `exclusive_choice`, which no
/// `ActiveModel` insert can reach because `SeaORM` always names every
/// column. Raw SQL for that reason.
///
/// This is the half of the tri-state rule the schema owns: a writer that
/// omits the column gets `auto`, never NULL, so "inherit" always has a
/// spelling and can never be confused with "parallel". The mapper owns the
/// other half and refuses a fourth value.
#[tokio::test]
async fn an_omitted_exclusive_choice_defaults_to_auto() {
    let conn = migrated_db().await;

    conn.execute_unprepared(
        "INSERT INTO qa_schedules
             (id, tenant_id, name, run_kind, cron, created_at, updated_at)
         VALUES ('s1', 's1t', 'defaulted', 'plan', '0 3 * * *', '2026-08-13', '2026-08-13')",
    )
    .await
    .unwrap();

    let stored = name_column(
        &conn,
        "SELECT exclusive_choice AS name FROM qa_schedules WHERE id = 's1'".to_owned(),
    )
    .await;
    assert_eq!(
        stored,
        vec!["auto".to_owned()],
        "an omitted tri-state must default to `auto`, never NULL - an absent value \
         would mean both \"inherit\" and \"parallel\" \
         (`manager/src/services/exclusivity.rs:60-79`)"
    );
    // The same insert must also have taken the JSON and `enabled` defaults,
    // which are equally unreachable through an `ActiveModel`.
    let enabled = name_column(
        &conn,
        "SELECT enabled || '/' || include_tags AS name FROM qa_schedules WHERE id = 's1'"
            .to_owned(),
    )
    .await;
    assert_eq!(enabled, vec!["1/[]".to_owned()]);
}

/// The foreign key, in both directions: an orphan tick is refused, and a
/// tick goes with its schedule.
///
/// The cascade is what stops a deleted schedule from leaving claim rows that
/// would block a future schedule reusing the same id — and, more to the
/// point, it is what the repository's `delete` relies on rather than issuing
/// a second statement.
#[tokio::test]
async fn a_tick_requires_its_schedule_and_cascades_with_it() {
    let conn = migrated_db().await;
    let tenant = uuid(1);
    let schedule_id = uuid(2);

    tick_am(uuid(3), tenant, uuid(0xdead))
        .insert(&conn)
        .await
        .expect_err("a tick row must reference an existing schedule");

    schedule_am(schedule_id, tenant, "nightly")
        .insert(&conn)
        .await
        .unwrap();
    tick_am(uuid(4), tenant, schedule_id)
        .insert(&conn)
        .await
        .unwrap();

    schedule::Entity::delete_by_id(schedule_id)
        .exec(&conn)
        .await
        .unwrap();

    assert!(
        schedule_tick::Entity::find_by_id(uuid(4))
            .one(&conn)
            .await
            .unwrap()
            .is_none(),
        "deleting a schedule must cascade to its tick rows"
    );
}

/// Tenant-prefixing, stated as the property it buys, for both unique
/// indexes on this pair.
///
/// For `idx_qa_schedules_tenant_name` it is the ordinary namespacing
/// argument. For `idx_qa_schedule_ticks_claim` it is sharper: the foreign
/// key is tenant-blind, so a second tenant *can* reach another tenant's
/// `schedule_id`, and without the prefix its insert would collide with the
/// victim's claim — which is a denial of service on the victim's schedule
/// **and** an oracle reporting whether the victim had already fired.
///
/// Verified by breaking it: with the claim index rewritten to
/// `(schedule_id, due_at)`, the second tenant's insert fails with a UNIQUE
/// violation and this test fails.
#[tokio::test]
async fn both_unique_indexes_are_per_tenant_not_global() {
    let conn = migrated_db().await;
    let victim = uuid(1);
    let squatter = uuid(7);
    let schedule_id = uuid(2);

    schedule_am(schedule_id, victim, "nightly")
        .insert(&conn)
        .await
        .unwrap();
    schedule_am(uuid(3), squatter, "nightly")
        .insert(&conn)
        .await
        .expect("schedule names are namespaced by tenant");
    schedule_am(uuid(4), victim, "nightly")
        .insert(&conn)
        .await
        .expect_err("schedule names are unique within a tenant");

    tick_am(uuid(5), victim, schedule_id)
        .insert(&conn)
        .await
        .unwrap();
    tick_am(uuid(6), squatter, schedule_id)
        .insert(&conn)
        .await
        .expect(
            "a second tenant's claim on the same (schedule_id, due_at) must be accepted - \
             if the claim index were tenant-blind, learning a schedule_id would be enough \
             to squat every one of its future ticks",
        );
    tick_am(uuid(8), victim, schedule_id)
        .insert(&conn)
        .await
        .expect_err("one (schedule, due time) may hold at most one claim within a tenant");
}

/// The declared indexes exist on the real schema with the declared columns.
///
/// `idx_qa_schedules_enabled` changes no observable behaviour, so a typo in
/// its name or its column list would be caught by nothing else — index names
/// are runtime strings, which is this file's own thesis about `cargo build`.
///
/// Column order is asserted for all three, but it is load-bearing for only
/// two of them: the claim index's order is what makes it tenant-prefixed,
/// and the name index's is what namespaces a name by tenant. The enabled
/// index's second column is pinned for shape, not for a benefit — the
/// enumeration filters on `enabled` and sorts by `id`, so
/// `last_fired_tick` serves neither. See this module's header.
#[tokio::test]
async fn every_declared_schedule_index_exists_with_the_declared_columns() {
    let conn = migrated_db().await;

    assert_eq!(
        index_names(&conn, "qa_schedules").await,
        vec!["idx_qa_schedules_enabled", "idx_qa_schedules_tenant_name"]
    );
    assert_eq!(
        index_names(&conn, "qa_schedule_ticks").await,
        vec!["idx_qa_schedule_ticks_claim"]
    );

    for (index, columns) in [
        ("idx_qa_schedules_tenant_name", vec!["tenant_id", "name"]),
        (
            "idx_qa_schedules_enabled",
            vec!["enabled", "last_fired_tick"],
        ),
        (
            "idx_qa_schedule_ticks_claim",
            vec!["tenant_id", "schedule_id", "due_at"],
        ),
    ] {
        assert_eq!(
            index_columns(&conn, index).await,
            columns,
            "{index} has the wrong columns or the wrong column order"
        );
    }
}

/// All three values survive a real INSERT and a real SELECT.
///
/// This is the assertion that links the entity's three new field names to
/// the three column names above — the link `cargo build` does not make,
/// because both sides are runtime strings.
#[tokio::test]
async fn the_three_case_columns_round_trip_through_the_entity() {
    let conn = migrated_db().await;
    let tenant = uuid(1);
    let run_id = uuid(2);
    let result_id = uuid(3);

    run_am(run_id, tenant, "case-fidelity-1")
        .insert(&conn)
        .await
        .unwrap();

    result_am(result_id, tenant, run_id)
        .insert(&conn)
        .await
        .unwrap();

    let stored = run_test_result::Entity::find_by_id(result_id)
        .one(&conn)
        .await
        .unwrap()
        .expect("the row just inserted must be readable");

    assert_eq!(stored.nodeid, NODEID);
    assert_eq!(stored.reason.as_deref(), Some("known upstream defect"));
    assert_eq!(stored.ticket.as_deref(), Some("VHP-3117"));

    // `ticket` is the case-level reference and `jira_key` the file-level
    // one. They are distinct columns holding distinct values, which is the
    // executable form of the module header's "Two columns."
    assert_eq!(stored.jira_key.as_deref(), Some("VHP-2618"));
    assert_ne!(stored.jira_key, stored.ticket);
}

/// `nodeid` defaults to `''` and the other two to `NULL`.
///
/// Unreachable from an `ActiveModel` insert, which always names every
/// column, so this drops to raw SQL that omits all three — the shape a row
/// written by a pre-Task-3 deployment, or by a rollback to one, actually
/// has. Asserting `Some("")` rather than `None` is the point: the module
/// header's claim that there is one spelling of "absent" for `nodeid` is
/// only true if the database supplies it, and this is where that is
/// checked.
#[tokio::test]
async fn an_omitted_nodeid_defaults_to_empty_and_the_other_two_to_null() {
    let conn = migrated_db().await;
    let tenant = uuid(1);
    let run_id = uuid(2);

    run_am(run_id, tenant, "case-fidelity-2")
        .insert(&conn)
        .await
        .unwrap();
    result_am(uuid(4), tenant, run_id)
        .insert(&conn)
        .await
        .unwrap();

    // `INSERT ... SELECT` off the seeded row rather than a literal `VALUES`
    // list. Two things that would otherwise be under test here are not:
    // `sqlx`'s `SQLite` encoding of `OffsetDateTime`, and its encoding of
    // `Uuid` — which is a **blob**, not the hyphenated text a formatted
    // literal would produce, so a hand-written `VALUES` row is written but
    // then never found again. Copying both from a row `SeaORM` itself wrote
    // sidesteps both; `randomblob(16)` is a fresh primary key in the same
    // encoding, and `test_name` is the handle this test reads back by.
    conn.execute_unprepared(
        "INSERT INTO qa_run_test_results \
         (id, tenant_id, run_id, test_file, test_name, status, created_at, updated_at) \
         SELECT randomblob(16), tenant_id, run_id, test_file, 'test_defaults', \
         status, created_at, updated_at FROM qa_run_test_results \
         WHERE test_name = 'test_fail_closed'",
    )
    .await
    .unwrap();

    let stored = run_test_result::Entity::find()
        .filter(run_test_result::Column::TestName.eq("test_defaults"))
        .one(&conn)
        .await
        .unwrap()
        .expect("the row just inserted must be readable");

    assert_eq!(stored.nodeid, "", "nodeid must default to the empty string");
    assert_eq!(stored.reason, None, "reason must default to NULL");
    assert_eq!(stored.ticket, None, "ticket must default to NULL");
}

/// A run row carrying a collect target, written and read back through the
/// entity.
///
/// This is the assertion that links the entity's new field name to the
/// column name above — the link `cargo build` does not make, because both
/// sides are runtime strings. The struct literal names every field for the
/// reason `m20260818_000005_case_fidelity` (folded into `migrations::m20260813_000003_initial` by the docs squash) argues: a `..Default::default()`
/// absorbs the next column added after it without a compile error.
#[tokio::test]
async fn a_collect_url_round_trips_through_the_run_entity() {
    let conn = migrated_db().await;
    let id = uuid(1);
    let url = "https://insights.example/qa/v1/collect/r/main";

    run::ActiveModel {
        id: ActiveValue::Set(id),
        tenant_id: ActiveValue::Set(uuid(2)),
        name: ActiveValue::Set("collect-1".to_owned()),
        run_kind: ActiveValue::Set("collect".to_owned()),
        target_repo_id: ActiveValue::Set(Some(uuid(0x10))),
        target_path: ActiveValue::Set(None),
        target_test_file: ActiveValue::Set(None),
        target_custom_plan_id: ActiveValue::Set(None),
        target_collect_url: ActiveValue::Set(Some(url.to_owned())),
        environment_id: ActiveValue::Set(None),
        test_version: ActiveValue::Set(Some("main".to_owned())),
        app_version: ActiveValue::Set(None),
        app_build: ActiveValue::Set(None),
        state: ActiveValue::Set("running".to_owned()),
        resolved_exclusive: ActiveValue::Set(false),
        exclusive_tier: ActiveValue::Set("default".to_owned()),
        is_validation: ActiveValue::Set(false),
        parameters: ActiveValue::Set(serde_json::json!([])),
        include_tags: ActiveValue::Set(serde_json::json!([])),
        exclude_tags: ActiveValue::Set(serde_json::json!([])),
        source: ActiveValue::Set("manual".to_owned()),
        schedule_id: ActiveValue::Set(None),
        bundle_ids: ActiveValue::Set(serde_json::json!([])),
        execution_ref: ActiveValue::Set(None),
        log_storage_ref: ActiveValue::Set(None),
        timeout_at: ActiveValue::Set(None),
        started_at: ActiveValue::Set(None),
        finished_at: ActiveValue::Set(None),
        error: ActiveValue::Set(None),
        passed: ActiveValue::Set(0),
        failed: ActiveValue::Set(0),
        skipped: ActiveValue::Set(0),
        in_progress: ActiveValue::Set(0),
        xfail: ActiveValue::Set(0),
        xpass: ActiveValue::Set(0),
        total: ActiveValue::Set(0),
        created_at: ActiveValue::Set(now()),
        updated_at: ActiveValue::Set(now()),
    }
    .insert(&conn)
    .await
    .unwrap();

    let stored = run::Entity::find_by_id(id)
        .one(&conn)
        .await
        .unwrap()
        .expect("the row just inserted must be readable");
    assert_eq!(stored.target_collect_url.as_deref(), Some(url));
    assert_eq!(stored.run_kind, "collect");
}

/// All three columns round-trip through the entity, at **distinct
/// non-default values**.
///
/// This is the assertion that links the entity's three new field names to
/// the column names above — the link `cargo build` does not make, because
/// both sides are runtime strings. Non-default values in all three, because
/// a fixture that left one at its default could not tell a written column
/// from a dropped one.
#[tokio::test]
async fn the_three_notification_columns_round_trip_through_the_schedule_entity() {
    let conn = migrated_db().await;
    let id = uuid(1);
    seeded_schedule(&conn, id, "nightly").await;

    let stored = schedule::Entity::find_by_id(id)
        .one(&conn)
        .await
        .unwrap()
        .expect("the row just inserted must be readable");
    assert!(stored.slack_notifications_enabled);
    assert_eq!(stored.slack_channel.as_deref(), Some("#qa-alerts"));
    assert_eq!(
        stored.slack_notification_events,
        serde_json::json!(["failed", "error"])
    );
}
