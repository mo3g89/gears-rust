//! Adds the three case-level columns `qa_run_test_results` needs in order to
//! carry the fidelity `qa-insights` analytics is built on.
//!
//! A **third migration file**, not an edit to `m20260813_000003_initial`, for
//! the reason `mod.rs` states: Phase A's DDL has already been applied wherever
//! qa-runs runs, so a column added by editing that file would never appear
//! there. `ALTER TABLE` in a new file is the only shape that reaches a
//! deployed schema.
//!
//! # Why these three, and why now
//!
//! The source system keeps results at **two** granularities and this gear
//! shipped with one. Per test *file* it has `test_results`
//! (`manager/migrations/001_initial.sql:65`), which is what `qa_run_test_results`
//! was ported from. Per test *function* it has a second table,
//! `test_case_results` (`:253`), whose own comment states the reason it exists:
//!
//! > Per-function test case outcomes (xfail/xpass/skip/pass/fail) parsed from
//! > the runner's TEST_CASE markers. One row per test function per run, grouped
//! > under a file (test_file). Lets analytics aggregate per-case, not just
//! > per-file.
//!
//! (`001_initial.sql:250-252`.) Three of that table's columns have no
//! counterpart here — `nodeid`, the pytest node identifier (`:257`); `reason`,
//! the xfail/skip explanation (`:261`); and `ticket`, the per-case bug
//! reference (`:262`). Its other columns already have homes: `test_file` and
//! `name` map onto `test_file`/`test_name`, `status` onto `status`, `duration`
//! onto `duration`.
//!
//! Without the three, qa-insights cannot reproduce two published shapes **at
//! all** — not approximately, not with a caveat:
//!
//! * `OverviewSummary`'s six per-case counters (`case_total`, `case_passed`,
//!   `case_failed`, `case_skipped`, `case_xfail`, `case_xpass`,
//!   `manager/src/routes/analytics.rs:99-104`), which legacy computes over the
//!   per-case rows and which degrade to per-file counts without them — legacy
//!   says so itself at `:97-98`: *"A file with no per-case rows (older runner)
//!   contributes one case of its file status."*
//! * `AnalyticsListItem::case_status` and `case_tickets`
//!   (`analytics.rs:131-132`), the per-case dot colour and ticket badges the
//!   list renders without a second fetch.
//!
//! See decision **D1** in
//! `gears/qa-platform/docs/plans/2026-08-18-qa-insights-gear.md` and the
//! finding it is drawn from in
//! `gears/qa-platform/docs/superpowers/specs/2026-08-18-qa-insights-design.md`.
//!
//! # Why the columns land here and not on a second table
//!
//! Legacy's two tables are two *granularities*, not two key shapes: both carry
//! `run_id` and `test_file` — `test_results.test_file` is an `ALTER` at
//! `001_initial.sql:166`, not part of its original `CREATE` — and they differ
//! only in what one row means. A `test_results` row is a whole test **file**; a
//! `test_case_results` row is one test **function** inside it.
//!
//! This gear does not restore the split, and that is D1's resolution rather
//! than a shortcut: **qa-insights** owns both tables, and qa-runs' job is to
//! carry the case-level detail as far as the per-test row so qa-insights'
//! reconcile sweep can read it and build them. So the three columns ride on
//! the existing per-test row, which is already keyed on `(tenant_id, run_id,
//! test_file, test_name)`.
//!
//! What that costs is recorded rather than hidden: a row describing a whole
//! file now has a `nodeid` column it leaves `''`. That is the same signal
//! legacy's own analytics fallback reads — *"A file with no per-case rows
//! (older runner) contributes one case of its file status"*
//! (`analytics.rs:97-98`) — expressed as an empty column on one row instead of
//! as an absence from a second table. **How a consumer is meant to tell the two
//! apart is not settled by this migration**; it is a property of what ingest
//! writes. This file only makes both representable.
//!
//! **Settled since, and recorded here because this is where the question
//! arises.** The convention is *non-empty `nodeid` means case-level, empty or
//! absent means file-level*, and it is stated in full — including that no
//! constraint in this schema enforces it — on
//! `domain::ports::run_executor::TestObservation`'s `nodeid` field, which is
//! where the value enters the system. It is a producer convention, not a
//! property this DDL can assert.
//!
//! (An earlier draft of this comment claimed legacy needed two tables because
//! its per-file table had no per-case key, and that the per-case table was
//! added later. Both were wrong: `test_results` does carry `test_file`, and
//! both tables are declared in the same initial migration, so nothing in the
//! source says which came first. Corrected rather than softened.)
//!
//! # Additive by construction
//!
//! `nodeid` defaults to `''` rather than being nullable, for the same reason
//! `test_file` does one column over: one spelling of "absent" keeps every
//! comparison a plain equality instead of legacy's `COALESCE(test_file, '')`
//! (`manager/src/routes/runs.rs:1154`). The sibling migration's comment on
//! `test_file` carries that argument in full. `nodeid` inherits it *and* the
//! precedent — legacy's own `test_case_results.nodeid` is likewise
//! `TEXT NOT NULL DEFAULT ''` (`001_initial.sql:257`), the only one of the
//! three that is.
//!
//! `reason` and `ticket` are genuinely optional and stay `NULL`-able, because
//! for them absence is real information rather than a spelling choice: a case
//! with no `reason` is a case the runner gave no xfail/skip explanation for,
//! and a `''` reason and a missing reason would then be the same value with
//! two meanings. Legacy declares both as bare nullable `TEXT` (`:261`, `:262`)
//! and this follows it.
//!
//! Nothing existing changes shape. No column changes type, nullability or
//! name; the `NOT NULL` one carries a default, so every pre-existing row
//! satisfies it without a backfill and the migration is safe against a
//! populated table.
//!
//! The write path was additive in the same sense but not by omission, and the
//! distinction is worth stating exactly. As this migration shipped,
//! `OrmRunsRepository::upsert_test_result` named all three new fields and set
//! them to `''`/`NULL`/`NULL` — byte-for-byte what an `INSERT` that omitted
//! them would have stored — because `NewTestResult` did not carry the values.
//! Widening the ingest contract here would have made the change observable
//! rather than additive.
//!
//! **That is history, not current behaviour.** The following commit widened
//! `NewTestResult` and `TestObservation` and made
//! `upsert_test_result` read all three from the argument, so the write path now
//! stores what the executor reports. Left in the past tense rather than deleted,
//! because the *hazard* it describes outlived it: adding those fields produced
//! **no compile error** at that struct literal, which is why
//! `a_case_level_nodeid_reason_and_ticket_are_actually_written` exists and why
//! the same trap waits for the next column added to this table.
//!
//! **Corrected by Task 4**, which is the third instalment of that same trap.
//! This paragraph read: *"`mapper::test_result_to_row` still does not read the
//! three back, and that remains deliberate — `TestResultRow` records why."*
//! Both halves are now false, and leaving them would have put this file in
//! direct contradiction with `TestResultRow`'s own doc. Task 4 gave the three
//! columns a reader — qa-insights' reconciler, over
//! `QaRunsClientV1::list_run_test_results` — so `test_result_to_row` reads all
//! three and `TestResultRow` carries them.
//!
//! The trap fired a third time on exactly the shape described above: the mapper
//! is another struct literal, so widening `TestResultRow` produced **no compile
//! error** at it, and a mapper that had silently kept writing the defaults would
//! have handed every consumer a case-level row that looked file-level.
//! `runs_sea_repo::tests::every_field_of_a_per_test_row_survives_the_trip_to_the_sdk`
//! is that instalment's guard, and it is break-verified against each of the ten
//! fields it names.
//!
//! Naming them exhaustively is a **choice, not a requirement**.
//! `sea-orm-macros` derives `Default` for every `ActiveModel`, so
//! `..Default::default()` compiles here and would have left the two files
//! holding those literals untouched. It is declined for the reason this file
//! exists to argue: a wildcard absorbs every column added after it **without a
//! compile error**, so a later migration's columns would quietly start writing
//! their defaults through a write path nobody was prompted to revisit. An
//! exhaustive literal turns that into a build failure at exactly the site that
//! has to be reconsidered, and it matches the style `run_am`, `queue_am` and
//! `result_am` already use.
//!
//! (An earlier draft of this comment — and the commit message that shipped it —
//! said the struct literal *must* name every field and that touching those two
//! files was therefore forced. That was false, and checking it costs one
//! substitution and one `cargo build`. Corrected rather than deleted: the
//! choice was right and the stated reason was not, and only the reason
//! survives to be read six months from now.)
//!
//! # `ticket` is not `jira_key`
//!
//! They look interchangeable and are not. `jira_key` is the **file**-level link
//! legacy keeps on `test_results` (`001_initial.sql:71`); `ticket` is the
//! **case**-level one on `test_case_results` (`:262`). Analytics renders the
//! latter, and only the latter, as `AnalyticsListItem::case_tickets`
//! (`analytics.rs:132`). Collapsing them would silently relabel a file-wide bug
//! link as a per-case one on every row. Two columns.
//!
//! # The status vocabulary does not change here
//!
//! Worth stating because this migration is the one that makes case-level rows
//! representable, and the natural next thought is that the case-level statuses
//! need a column of their own or a tightened `status` check. They do not. The
//! sibling migration already documents `status` as an **open** set that ingest
//! must not reject, and the reason it gives is precisely the case-level mapper:
//! `status_of` maps six pytest outcomes and then falls through to
//! `other => return other.to_uppercase()` for everything else
//! (`manager/src/services/argo.rs:2932-2943`, the fallthrough arm at `:2940`).
//! That
//! openness now covers the rows these three columns describe, unchanged.
//!
//! # What the tests below do and do not reach
//!
//! `SQLITE_UP` and `DOWN` run on every `cargo test`. `PG_UP` is executed by the
//! `integration` tier against a real Postgres; `MYSQL_UP` is executed by
//! nothing in this workspace, so for it the parity test below is the whole
//! control — it compares *declarations*, not dialect acceptance. The sibling
//! migrations' test modules argue this at length; not repeated here.
//!
//! **`MySQL` has no `ADD COLUMN IF NOT EXISTS`.** That is the single difference
//! between `PG_UP` and `MYSQL_UP`, and the parity test normalizes it away
//! rather than letting it hide a real divergence. Re-running an already-applied
//! migration is the migrator's job to prevent (`seaql_migrations` records what
//! ran); the `IF NOT EXISTS` on Postgres is belt-and-braces, not the mechanism.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &str = r"
ALTER TABLE qa_run_test_results ADD COLUMN IF NOT EXISTS nodeid VARCHAR(1024) NOT NULL DEFAULT '';
ALTER TABLE qa_run_test_results ADD COLUMN IF NOT EXISTS reason TEXT NULL;
ALTER TABLE qa_run_test_results ADD COLUMN IF NOT EXISTS ticket VARCHAR(64) NULL;
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_run_test_results ADD COLUMN nodeid VARCHAR(1024) NOT NULL DEFAULT '';
ALTER TABLE qa_run_test_results ADD COLUMN reason TEXT NULL;
ALTER TABLE qa_run_test_results ADD COLUMN ticket VARCHAR(64) NULL;
";

const SQLITE_UP: &str = r"
ALTER TABLE qa_run_test_results ADD COLUMN nodeid VARCHAR(1024) NOT NULL DEFAULT '';
ALTER TABLE qa_run_test_results ADD COLUMN reason TEXT NULL;
ALTER TABLE qa_run_test_results ADD COLUMN ticket VARCHAR(64) NULL;
";

/// Reverse order of `*_UP`, and one blob for all three dialects.
///
/// Plain `DROP COLUMN`, without `IF EXISTS`: Postgres accepts the guard,
/// `MySQL` and `SQLite` do not, and a per-dialect `DOWN` for a guard that only
/// matters when `down()` is run twice is not worth three blobs that can drift.
///
/// `SQLite` gained `ALTER TABLE ... DROP COLUMN` in 3.35.0 (2021-03); the
/// bundled `libsqlite3-sys` this workspace builds against is well past that,
/// and `the_case_fidelity_down_migration_drops_all_three_columns` below is what
/// proves it rather than the version number. None of the three columns is
/// indexed or part of a key, which is the condition `SQLite`'s `DROP COLUMN`
/// imposes.
const DOWN: &str = r"
ALTER TABLE qa_run_test_results DROP COLUMN ticket;
ALTER TABLE qa_run_test_results DROP COLUMN reason;
ALTER TABLE qa_run_test_results DROP COLUMN nodeid;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => PG_UP,
            sea_orm::DatabaseBackend::MySql => MYSQL_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(DOWN).await?;
        Ok(())
    }
}

/// Schema tests for the three case-fidelity columns.
///
/// The sibling migrations' test modules explain at length why a module like
/// this exists at all: `SeaORM` entities name their table and every column as
/// **runtime strings**, so a typo in `entity/run_test_result.rs` or in a blob
/// here compiles cleanly and fails at query time. Only a query links the two.
///
/// What this module covers: the three column names as `SQLite` reports them
/// after the real `Migrator` runs, three-way parity of the `ADD COLUMN`
/// declarations, the `DEFAULT ''` that no `ActiveModel` insert can reach, a
/// round-trip of all three values through the entity, and `down()`.
///
/// What it does **not** cover: `MYSQL_UP`'s syntax, which nothing here
/// executes. `PG_UP` is executed by the `integration` tier.
///
/// The `migrated_db` and `column_names` helpers are near-copies of the sibling
/// migrations' — duplicated rather than shared for the reason
/// `m20260813_000004_schedules.rs` gives: reaching into another migration's
/// private `#[cfg(test)]` module would couple two append-only files that are
/// meant to be independent.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{
        ActiveModelTrait, ActiveValue, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
        DatabaseConnection, EntityTrait, QueryFilter,
    };
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::infra::storage::entity::{run, run_test_result};

    /// Deterministic fixture UUIDs; `Uuid::new_v4` would need a cargo feature
    /// this crate does not ask for and would make a failure unreproducible.
    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 2026-08-13 00:00:00 UTC, the fixture instant the sibling modules use.
    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
    }

    /// The `(name, definition)` of each `ADD COLUMN` in a blob, in order, with
    /// Postgres's `IF NOT EXISTS` normalized away.
    ///
    /// That guard is the one difference the three blobs are *allowed* to have —
    /// `MySQL` has no such clause — so it is stripped here rather than being
    /// left to make the comparison always-unequal, which would turn the parity
    /// test into a test of nothing.
    fn added_columns(ddl: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for line in ddl.lines() {
            let line = line.trim().trim_end_matches(';');
            let Some(rest) = line.strip_prefix("ALTER TABLE qa_run_test_results ADD COLUMN ")
            else {
                continue;
            };
            let rest = rest.strip_prefix("IF NOT EXISTS ").unwrap_or(rest);
            let (name, definition) = rest
                .split_once(' ')
                .expect("an ADD COLUMN clause has a name and a type");
            out.push((name.to_owned(), definition.trim().to_owned()));
        }
        out
    }

    /// The three dialects add the same columns, in the same order, with the
    /// same types and nullability.
    ///
    /// This is the *only* control over `MYSQL_UP`, which nothing in this
    /// workspace executes: a column added to two blobs and forgotten in the
    /// third leaves every other test in this file green, because they all run
    /// against `SQLite`. The sibling migrations record the same measurement.
    ///
    /// Break-verified: changing `reason TEXT NULL` to `reason VARCHAR(64) NULL`
    /// in `MYSQL_UP` alone is what turns this red, and nothing else notices.
    #[test]
    fn every_dialect_adds_the_same_three_columns() {
        let pg = added_columns(super::PG_UP);
        let my = added_columns(super::MYSQL_UP);
        let sq = added_columns(super::SQLITE_UP);

        assert_eq!(
            pg.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["nodeid", "reason", "ticket"],
            "the parser lost a column; every comparison below would then be \
             over the wrong set: {pg:?}"
        );
        assert_eq!(pg, my, "PG_UP and MYSQL_UP add different columns");
        assert_eq!(pg, sq, "PG_UP and SQLITE_UP add different columns");
    }

    /// `DOWN` drops exactly what the `*_UP` blobs add, in the reverse order.
    ///
    /// Reverse order is not load-bearing for three unindexed columns, but a
    /// `DOWN` that drops a *different* set than `UP` adds is the failure this
    /// catches — most plausibly by someone adding a fourth column to the three
    /// `UP` blobs and not to `DOWN`.
    #[test]
    fn down_drops_exactly_what_up_adds() {
        let added: Vec<String> = added_columns(super::PG_UP)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        let dropped: Vec<String> = super::DOWN
            .lines()
            .filter_map(|line| {
                line.trim()
                    .trim_end_matches(';')
                    .strip_prefix("ALTER TABLE qa_run_test_results DROP COLUMN ")
                    .map(str::to_owned)
            })
            .collect();

        assert_eq!(dropped.len(), 3, "DOWN parser found {dropped:?}");
        assert_eq!(
            dropped,
            added.iter().rev().cloned().collect::<Vec<_>>(),
            "DOWN must drop exactly the UP columns, in reverse order"
        );
    }

    /// In-memory `SQLite` with **every** migration this gear declares applied,
    /// through the real [`super::super::Migrator`].
    ///
    /// `max_connections(1)` is load-bearing: each `SQLite` `:memory:`
    /// connection is its own database, so a larger pool would let a query land
    /// on one where the migration never ran.
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
        for migration in super::super::Migrator::migrations() {
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-runs migrations");
        }
        conn
    }

    /// Column names of a table, in declaration order, from `SQLite`'s own
    /// catalogue. The sibling migrations query `sqlite_master` for indexes;
    /// there was no column-inventory helper to reuse, so this is the new one.
    async fn column_names(conn: &DatabaseConnection, table: &str) -> Vec<String> {
        use sea_orm::Statement;
        conn.query_all(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            format!("SELECT name FROM pragma_table_info('{table}')"),
        ))
        .await
        .unwrap()
        .iter()
        .filter_map(|row| row.try_get::<String>("", "name").ok())
        .collect()
    }

    /// Name, declared type and `NOT NULL` of each column, in declaration
    /// order. The companion to [`column_names`] for assertions that have to see
    /// more than the name.
    async fn column_specs(conn: &DatabaseConnection, table: &str) -> Vec<(String, String, bool)> {
        use sea_orm::Statement;
        conn.query_all(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            format!("SELECT name, type, \"notnull\" FROM pragma_table_info('{table}')"),
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.try_get::<String>("", "name").unwrap(),
                row.try_get::<String>("", "type").unwrap(),
                row.try_get::<i32>("", "notnull").unwrap() != 0,
            )
        })
        .collect()
    }

    /// The three columns exist on the real migrated schema.
    ///
    /// Deliberately an assertion about the **table**, not about the migration
    /// having run: `Migrator::migrations()` returning this migration proves
    /// only that it is registered. `pragma_table_info` is the database's own
    /// answer, so this stays honest if `up()` is ever silently short-circuited.
    #[tokio::test]
    async fn case_fidelity_adds_three_nullable_columns() {
        let conn = migrated_db().await;
        let cols = column_names(&conn, "qa_run_test_results").await;
        assert!(
            cols.contains(&"nodeid".to_owned()),
            "nodeid missing: {cols:?}"
        );
        assert!(
            cols.contains(&"reason".to_owned()),
            "reason missing: {cols:?}"
        );
        assert!(
            cols.contains(&"ticket".to_owned()),
            "ticket missing: {cols:?}"
        );
    }

    /// Every pre-existing column still has its **name, declared type and
    /// nullability**.
    ///
    /// The whole rule for this task is *additive only*, and "additive" is a
    /// claim about what did not change. An earlier version of this test
    /// compared names alone, which let it pass its own doc comment: a migration
    /// that retyped `test_file` or made `duration` `NOT NULL` would have been
    /// waved through while the comment claimed additivity was guarded. Widened
    /// after the spec review rather than having its claim narrowed, because
    /// `pragma_table_info` already returns all three and the test was reading
    /// one of them.
    ///
    /// `notnull` is `SQLite`'s own 0/1, so this reads what the database
    /// concluded from the DDL rather than re-parsing the DDL — which is the
    /// same reason the rest of this module queries instead of string-matching.
    #[tokio::test]
    async fn the_pre_existing_columns_are_untouched() {
        let conn = migrated_db().await;
        let specs = column_specs(&conn, "qa_run_test_results").await;

        // Exactly the eleven columns `m20260813_000003_initial` declares, in
        // its order, with its types and nullability. `nodeid`/`reason`/`ticket`
        // are appended after these and are asserted on elsewhere.
        let expected: Vec<(String, String, bool)> = [
            ("id", "TEXT", true),
            ("tenant_id", "TEXT", true),
            ("run_id", "TEXT", true),
            ("test_file", "TEXT", true),
            ("test_name", "TEXT", true),
            ("status", "TEXT", true),
            ("duration", "TEXT", false),
            ("launch_id", "TEXT", false),
            ("jira_key", "TEXT", false),
            ("created_at", "TEXT", true),
            ("updated_at", "TEXT", true),
        ]
        .into_iter()
        .map(|(n, t, nn)| (n.to_owned(), t.to_owned(), nn))
        .collect();

        assert_eq!(
            specs.len(),
            14,
            "expected 11 pre-existing columns plus 3 new ones: {specs:?}"
        );
        assert_eq!(
            &specs[..11],
            &expected[..],
            "a pre-existing column changed name, type or nullability: this \
             change is no longer additive"
        );
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
            platform_id: ActiveValue::Set(Some(uuid(0x11))),
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
            total: ActiveValue::Set(0),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        }
    }

    /// A per-test row with all three new columns populated, and with `ticket`
    /// deliberately *different* from `jira_key`.
    const NODEID: &str =
        "tests/authn/test_error_handling.py::TestFailClosed::test_fail_closed[tls]";

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

    /// `down()` removes all three columns and leaves the table and its
    /// pre-existing columns alone.
    ///
    /// `super::Migration` directly, **not** a loop over `Migrator::migrations()`
    /// — that loop is in declaration order and `down()` runs in the reverse of
    /// it, so the loop would drop Phase A's tables out from under this
    /// migration. Each migration's test owns its own `down()`, as `mod.rs`
    /// states.
    ///
    /// This test is also the evidence for the `DOWN` comment's claim that the
    /// bundled `SQLite` is new enough for `ALTER TABLE ... DROP COLUMN`
    /// (3.35.0+): on an older library it fails with a syntax error rather than
    /// passing quietly. It is *not* evidence about `MySQL` or Postgres, which
    /// this tier does not execute.
    #[tokio::test]
    async fn the_case_fidelity_down_migration_drops_all_three_columns() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();

        let cols = column_names(&conn, "qa_run_test_results").await;
        for gone in ["nodeid", "reason", "ticket"] {
            assert!(
                !cols.contains(&gone.to_owned()),
                "{gone} survived down(): {cols:?}"
            );
        }
        assert_eq!(
            cols.len(),
            11,
            "down() must drop exactly three columns: {cols:?}"
        );

        // The table itself, and the sibling migrations' tables, are untouched:
        // this migration owns three columns and must not reach further.
        for table in ["qa_run_test_results", "qa_runs", "qa_schedules"] {
            conn.execute_unprepared(&format!("SELECT 1 FROM {table}"))
                .await
                .expect("down() must not drop any table");
        }
    }
}
