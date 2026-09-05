//! Phase B schema: cron schedules and the exactly-once tick claim.
//!
//! A **second migration file**, not an edit to `m20260813_000003_initial`.
//! Migrations are append-only: Phase A's has already been applied wherever
//! Phase A ran, so a table added by editing it would never be created there.
//!
//! Same shape as the sibling — one `execute_unprepared` DDL blob per dialect,
//! with identical column and index order across all three. The header of
//! `m20260813_000003_initial.rs` carries the arguments this file does not
//! repeat: why `DESIGN.md` is cited by heading rather than by line number, why
//! `cargo build` is no evidence about a schema, and the squatting/oracle
//! reasoning behind "every unique index is tenant-prefixed". Read that file
//! first; what follows is only what is specific to these two tables.
//!
//! ## Which dialect blobs are actually executed
//!
//! `SQLITE_UP` runs on every `cargo test`. **`POSTGRES_UP` runs too**, against a
//! real Postgres container, whenever the `integration` feature is on:
//! `infra::storage::test_db`'s `pg_db` applies this gear's real migrator before
//! `isolation_pg_tests` and `domain::service::ingest_races_pg_tests` run. So the
//! Postgres blob's *syntax* is checked, just not by the default gate — measured
//! by running that tier, not assumed. (The sibling migration's header used to
//! say "nothing in this workspace executes" the server blobs, a claim that
//! predated the `integration` tier; Task 20 corrected it there, so the two files
//! now agree.)
//!
//! **`MYSQL_UP` is executed by nothing.** Its syntax is unverified and only its
//! *declarations* are checked, by the two parity tests below.
//!
//! ## `exclusive_choice` is `NOT NULL` because absence cannot mean two things
//!
//! Three values — `true`, `false`, `auto` — and the column is written on every
//! insert including the `auto` case. That is not tidiness; it is the one
//! mistake the source system made and had to undo. Its schedule equivalent is a
//! Kubernetes annotation, and the discarded first attempt wrote the annotation
//! only when the choice was `true`, which left an absent annotation meaning both
//! "inherit from the lower tiers" and "force parallel" in already-deployed
//! objects — *"a distinction that then cannot be recovered without migrating
//! live Kubernetes objects"*
//! (`manager/src/services/exclusivity.rs`, `format_exclusive_annotation`,
//! lines 60-79; the tri-state's own reading is at `:39-58`, where `None` is
//! *"not the same as an explicit `false`, which forces parallel — conflating
//! them is precisely the mistake this shape exists to prevent"*).
//!
//! `NOT NULL DEFAULT 'auto'` is that lesson expressed as a constraint rather
//! than as a convention a writer might forget. The `DEFAULT` is a backstop for
//! a hand-written `INSERT`; `SeaORM` names every column, so the repository
//! always supplies a value.
//!
//! The mapper owns the codec and **fails closed**: an unrecognised value is
//! [`DomainError::CorruptState`](crate::domain::error::DomainError::CorruptState),
//! never a silent `auto`. Decoding an unknown string as `auto` would recreate
//! exactly the conflation above, one layer up.
//!
//! ## `qa_schedule_ticks` has no `updated_at`, deliberately
//!
//! DESIGN §3.7 requires `id`, `tenant_id`, `created_at` and `updated_at` of
//! every table, and records the two existing `updated_at` departures in the
//! document itself — qa-catalog's `ssh_keys` and `test_bundles`, both immutable
//! after insert. A third departure from that column set, on a different column,
//! is recorded where qa-environments makes it: `qa_environment_leases` (renamed
//! from `qa_platform_leases`) has no `id` and no `created_at`, stated in that
//! migration's own comment rather than in
//! DESIGN. This is the fourth, and it takes the second form — the argument lives
//! here, beside the table.
//!
//! A tick row is a **claim**, and a claim is immutable once it is won. The two
//! columns that are written afterwards — `run_id` and `error` — are written
//! exactly once, by the instance that won the claim, reporting what its launch
//! did. There is no second writer and no state to track, so an `updated_at`
//! here could only ever be "the moment the outcome was recorded", which is not
//! what the column means anywhere else in this schema. Carrying it would invite
//! a reader to treat the row as mutable, which is the opposite of the property
//! the claim depends on.
//!
//! ## The claim is the exactly-once mechanism
//!
//! `cpt-cf-qa-nfr-scheduler-exactly-once` is discharged by
//! `idx_qa_schedule_ticks_claim` and by nothing else. Two instances racing for
//! one schedule's due time both attempt the insert; one wins, the other takes a
//! unique violation and never launches. DESIGN §3.7 states it in those terms
//! under **Constraints**: *"`schedule_ticks`
//! `UNIQUE(tenant_id, schedule_id, due_at)` makes the exactly-once claim a
//! constraint, not a convention"*.
//!
//! **A lost race is the normal path, not a fault.** Every non-leader instance
//! and every leader failover produces one, so the repository maps the violation
//! to `Ok(None)`; surfacing it as a database error would answer 500 for correct
//! behaviour. See `domain::repos::schedules_repo::SchedulesRepository`'s
//! `claim_tick`.
//!
//! ## The foreign key is tenant-blind, and that is the same existence oracle
//!
//! `schedule_id REFERENCES qa_schedules(id)` carries no tenant component, so an
//! insert with an attacker-chosen `schedule_id` **succeeds if that schedule
//! exists in any tenant** and **fails if it does not** — a membership test over
//! another tenant's schedule identifiers, exactly as `qa_run_queue.run_id` is.
//! `m20260813_000003_initial.rs`'s section *"The foreign keys are tenant-blind,
//! and that is an existence oracle"* is the full argument, including why a
//! composite foreign key stays declined.
//!
//! The obligation it hands on is the same: **resolve a caller-supplied
//! `schedule_id` under the caller's own scope before any write that references
//! it** — and here it is discharged structurally rather than by convention.
//! `domain::repos::OwnedScheduleId` is the schedules counterpart of
//! `domain::repos::OwnedRunId`: `claim_tick` takes the token, the token's only
//! source is `resolve_owned_schedule`, and that method resolves the id under the
//! caller's own scope and answers `ScheduleNotFound` when it cannot. So the
//! foreign key never gets to answer the existence question.
//!
//! **This paragraph previously ended "`qa-runs` has no `OwnedScheduleId` token
//! … because nothing yet needs one."** That was true of Task 17's own caller —
//! the firing ticker takes its ids from its own scoped enumeration, never from a
//! request — and wrong about the next one: Task 20 adds exactly the REST layer
//! that can pass a caller-supplied `schedule_id`. Retrofitting a token through a
//! live call site costs more than adding it while the trait has no consumers, so
//! it was added here.
//!
//! What the token does **not** promise: that the reference survives. A schedule
//! deleted after the token is minted still produces a foreign-key violation at
//! the insert, reported as an error rather than as a lost claim race —
//! `a_non_unique_failure_is_an_error_not_a_lost_race` pins that.
//!
//! ## What the enabled-schedule index is for
//!
//! `idx_qa_schedules_enabled` is **not** tenant-prefixed, and for the reason
//! `idx_qa_runs_state_timeout` is not: the evaluator enumerates enabled
//! schedules across every tenant under one nil-tenant context
//! (`domain::system_actor::for_schedule_tick`) and then writes under a
//! per-tenant one (`domain::system_actor::for_schedule_fire`). The enumeration
//! wants `enabled` leading. Being non-unique it is not a cross-tenant channel —
//! the rule the sibling states is about *unique* indexes.
//!
//! **What the second column actually buys, stated because it is less than it
//! looks.** The enumeration this serves is
//! `WHERE enabled = true ORDER BY id` (`schedules_sea_repo::list_enabled`), so
//! `last_fired_tick` appears in **no predicate and no `ORDER BY`**, the index
//! does not cover the query — every other column is fetched from the row — and
//! the `ORDER BY id` forces a sort whatever this index does. The leading
//! `enabled` narrows the scan; the trailing column earns nothing today. It is
//! kept because the plan specifies this index verbatim and because a cursor
//! filter on `last_fired_tick` is the obvious next shape for the enumeration,
//! at which point the column is already there.
//!
//! The tempting alternative — a partial index on `WHERE enabled` — is **not
//! available in this subsystem**, and not merely unfashionable: DESIGN §3.7
//! rejects partial indexes outright because the predicate has no `MySQL`
//! equivalent and one column list has to serve all three dialects. That is the
//! same ruling that made `idx_qa_run_queue_fifo` carry `state` as an ordinary
//! equality column instead.
//!
//! ## `MySQL` key-width budget (`InnoDB`, `utf8mb4`, 3072-byte limit)
//!
//! `idx_qa_schedules_tenant_name` is 36*4 + 255*4 = 1164 bytes, the same tuple
//! as `idx_qa_runs_tenant_name`. `idx_qa_schedule_ticks_claim` is
//! 36*4 + 36*4 + 4 = 292 bytes (`TIMESTAMP` is 4).
//! `idx_qa_schedules_enabled` is 1 + 4 = 5. All three fit with room to spare,
//! so no column had to shrink for an index.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r#"
CREATE TABLE IF NOT EXISTS qa_schedules (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    -- Discriminant for the four flattened target columns below, exactly as on
    -- `qa_runs`: `qa_runs_sdk::RunKind::as_str` is the sole encoder and the
    -- mapper reuses the run's target codec, so a schedule and the run it fires
    -- cannot disagree about how a target is stored.
    run_kind VARCHAR(16) NOT NULL,
    target_repo_id UUID NULL,
    target_path VARCHAR(1024) NULL,
    target_test_file VARCHAR(1024) NULL,
    target_custom_plan_id UUID NULL,
    -- Same obligation as `qa_runs.platform_id`, and it is the schedule that
    -- carries it first: ownership must be verified through a tenant-scoped
    -- qa-environments client, because `qa_platform_leases` is keyed on a bare
    -- `platform_id` and is not tenant-partitioned. A schedule persisting
    -- another tenant's platform id would re-acquire the global lease on every
    -- fire, indefinitely.
    platform_id UUID NULL,
    -- The branch a fire resolves against. Nullable: absent means the
    -- repository's own default branch is used at launch, which is the tier
    -- `service::launch` already resolves for a manual launch.
    branch VARCHAR(512) NULL,
    -- Five-field cron expression, parsed by `domain::cron` (Task 18). Stored as
    -- text and validated on write rather than decomposed into columns: the
    -- expression is the user's own input and has to survive a round trip
    -- verbatim for the UI to show it back.
    cron VARCHAR(255) NOT NULL,
    -- Tri-state, ALWAYS written: 'true' | 'false' | 'auto'. Never NULL, and
    -- never omitted for the inherit case -- an absent value would mean both
    -- "inherit" and "parallel", a distinction that cannot be recovered later
    -- (`manager/src/services/exclusivity.rs:60-79`). NOT NULL is the schema
    -- making that impossible rather than a convention hoping for it. The
    -- decoder fails closed on a fourth spelling; see this module's header.
    exclusive_choice VARCHAR(8) NOT NULL DEFAULT 'auto',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    include_tags JSONB NOT NULL DEFAULT '[]',
    exclude_tags JSONB NOT NULL DEFAULT '[]',
    parameters JSONB NOT NULL DEFAULT '[]',
    -- The latest due time this schedule has fired for, and the evaluator's
    -- cursor: `domain::cron` computes the next due time from it, so it must
    -- only ever move forward. The repository's advance is guarded on
    -- `last_fired_tick IS NULL OR last_fired_tick < $1` to keep that true.
    -- NULL means the schedule has never fired.
    last_fired_tick TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_schedules_tenant_name ON qa_schedules(tenant_id, name);
-- The evaluator's enumeration: enabled schedules across tenants.
CREATE INDEX IF NOT EXISTS idx_qa_schedules_enabled ON qa_schedules(enabled, last_fired_tick);

CREATE TABLE IF NOT EXISTS qa_schedule_ticks (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    -- Tenant-blind FK, and therefore an existence oracle over another tenant's
    -- schedule ids. The writer's obligation, and why the caller discharges it
    -- today, are in this module's header.
    schedule_id UUID NOT NULL REFERENCES qa_schedules(id) ON DELETE CASCADE,
    due_at TIMESTAMPTZ NOT NULL,
    -- Which instance won the claim. Diagnostic only: nothing reads it to make a
    -- decision, because the decision was made by the unique index.
    claimed_by VARCHAR(255) NOT NULL,
    claimed_at TIMESTAMPTZ NOT NULL,
    -- The run this tick produced. NULL means the claim succeeded but the
    -- launch has not completed yet, or failed -- the claim is deliberately
    -- durable either way, because a retried launch would violate
    -- exactly-once for a destructive run.
    run_id UUID NULL,
    -- No FK to `qa_runs(id)`: the run is created after the claim, so a
    -- constraint here would only fire on a bug, and the tenant-blind form would
    -- add a second existence oracle for nothing. `qa_runs.schedule_id` is the
    -- other half of the pair and is likewise unconstrained.
    error TEXT NULL,
    -- No `updated_at`. A claim row is immutable once won; `run_id` and `error`
    -- are written once by the winning instance. See this module's header for the
    -- full argument and for the three departures that precede it.
    created_at TIMESTAMPTZ NOT NULL
);
-- THE exactly-once mechanism (`cpt-cf-qa-nfr-scheduler-exactly-once`). The
-- claim is a UNIQUE constraint, not a convention: a second instance racing for
-- the same due time loses the insert and never launches. DESIGN section 3.7
-- states this explicitly -- "`schedule_ticks` UNIQUE(tenant_id, schedule_id,
-- due_at) makes the exactly-once claim a constraint, not a convention" -- and
-- it is tenant-prefixed for the same reason every unique index in this
-- subsystem is (DESIGN section 3.7, "Every unique index is tenant-prefixed").
--
-- The leading `tenant_id` MUST NOT be dropped as redundant now that
-- `domain::repos::OwnedScheduleId` exists. That token is a bare Uuid: it records
-- that *some* scope could see the schedule and never which tenant that scope
-- named, so one minted under a multi-tenant scope -- which is exactly what the
-- firing ticker's nil-tenant enumeration compiles to -- can be spent while
-- writing under a different tenant's scope. Without the prefix such a row
-- collides with the owning tenant's own claim, which is a denial of service on
-- their schedule and an oracle reporting whether it had already fired. The
-- token narrows who can reach the tenant-blind foreign key; it does not make
-- this prefix redundant. `OwnedScheduleId`'s own doc carries the long form.
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_schedule_ticks_claim
    ON qa_schedule_ticks(tenant_id, schedule_id, due_at);
"#;

const MYSQL_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_schedules (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    name VARCHAR(255) NOT NULL,
    run_kind VARCHAR(16) NOT NULL,
    target_repo_id VARCHAR(36) NULL,
    target_path VARCHAR(1024) NULL,
    target_test_file VARCHAR(1024) NULL,
    target_custom_plan_id VARCHAR(36) NULL,
    platform_id VARCHAR(36) NULL,
    branch VARCHAR(512) NULL,
    cron VARCHAR(255) NOT NULL,
    exclusive_choice VARCHAR(8) NOT NULL DEFAULT 'auto',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    -- Expression defaults (MySQL 8.0.13+), for the reason the sibling
    -- migration's `parameters` column states: the literal form is rejected for
    -- JSON columns and the parenthesised form is not, so there is no
    -- cross-dialect asymmetry to document. Note `exclusive_choice` above takes
    -- the plain literal form -- it is a VARCHAR, not JSON.
    include_tags JSON NOT NULL DEFAULT ('[]'),
    exclude_tags JSON NOT NULL DEFAULT ('[]'),
    parameters JSON NOT NULL DEFAULT ('[]'),
    last_fired_tick TIMESTAMP NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_schedules_tenant_name (tenant_id, name),
    KEY idx_qa_schedules_enabled (enabled, last_fired_tick)
);

CREATE TABLE IF NOT EXISTS qa_schedule_ticks (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    tenant_id VARCHAR(36) NOT NULL,
    schedule_id VARCHAR(36) NOT NULL,
    due_at TIMESTAMP NOT NULL,
    claimed_by VARCHAR(255) NOT NULL,
    claimed_at TIMESTAMP NOT NULL,
    run_id VARCHAR(36) NULL,
    error TEXT NULL,
    created_at TIMESTAMP NOT NULL,
    UNIQUE KEY idx_qa_schedule_ticks_claim (tenant_id, schedule_id, due_at),
    CONSTRAINT fk_qa_schedule_ticks_schedule FOREIGN KEY (schedule_id) REFERENCES qa_schedules(id) ON DELETE CASCADE
);
";

const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_schedules (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    run_kind TEXT NOT NULL,
    target_repo_id TEXT NULL,
    target_path TEXT NULL,
    target_test_file TEXT NULL,
    target_custom_plan_id TEXT NULL,
    platform_id TEXT NULL,
    branch TEXT NULL,
    cron TEXT NOT NULL,
    exclusive_choice TEXT NOT NULL DEFAULT 'auto',
    enabled INTEGER NOT NULL DEFAULT 1,
    include_tags TEXT NOT NULL DEFAULT '[]',
    exclude_tags TEXT NOT NULL DEFAULT '[]',
    parameters TEXT NOT NULL DEFAULT '[]',
    last_fired_tick TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_schedules_tenant_name ON qa_schedules(tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_qa_schedules_enabled ON qa_schedules(enabled, last_fired_tick);

CREATE TABLE IF NOT EXISTS qa_schedule_ticks (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    schedule_id TEXT NOT NULL,
    due_at TEXT NOT NULL,
    claimed_by TEXT NOT NULL,
    claimed_at TEXT NOT NULL,
    run_id TEXT NULL,
    error TEXT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (schedule_id) REFERENCES qa_schedules(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_schedule_ticks_claim ON qa_schedule_ticks(tenant_id, schedule_id, due_at);
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::MySql => MYSQL_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        // Children first, or the foreign key refuses.
        let sql = r"
DROP TABLE IF EXISTS qa_schedule_ticks;
DROP TABLE IF EXISTS qa_schedules;
        ";
        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}

/// Schema tests for the schedules pair.
///
/// The sibling migration's test module explains at length why these exist:
/// `SeaORM` entities name their table and every column as **runtime strings**,
/// so a typo compiles cleanly and fails at query time, and only a query links
/// the two. Everything here issues one.
///
/// What this module covers: both table names, every column name in both
/// entities, the `DEFAULT 'auto'` that no `ActiveModel` insert can reach, the
/// foreign key in both directions, and three-way parity of the column and index
/// declarations. What it does **not** cover: `MYSQL_UP`'s *syntax*, which
/// nothing executes — the parity tests compare declarations, not dialect
/// acceptance. `POSTGRES_UP` is executed by the `integration` tier; see this
/// module's header.
///
/// The two DDL parsers below are near-copies of the sibling migration's. They
/// are duplicated rather than shared because this task does not own that file,
/// and reaching into another migration's private `#[cfg(test)]` module would
/// couple two append-only files that are supposed to be independent.
///
/// Raw `SeaORM` connections and `clippy::disallowed_methods` here for the reason
/// the sibling states: `SecureORM` exposes no `execute_unprepared`, which the
/// column-default test needs, and requires an `AccessScope` that a migration
/// test has no business constructing. The scoped equivalents live in
/// `infra::storage::schedules_sea_repo`.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{
        ActiveModelTrait, ActiveValue, ConnectOptions, ConnectionTrait, Database,
        DatabaseConnection, EntityTrait,
    };
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::infra::storage::entity::{schedule, schedule_tick};

    /// Deterministic fixture UUIDs, for the reason the sibling gives:
    /// `Uuid::new_v4` would make a failure unreproducible.
    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 2026-08-13 00:00:00 UTC, the fixture instant this crate uses throughout.
    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap()
    }

    /// Every column name of every table in one dialect blob, in declaration
    /// order.
    ///
    /// Strips `--` and blank lines, then takes the leading identifier of each
    /// remaining line inside a `CREATE TABLE` body, skipping the trailing
    /// constraint clauses `MySQL` declares inline and the others do not.
    fn columns_by_table(ddl: &str) -> Vec<(String, Vec<String>)> {
        let mut out = Vec::new();
        let mut table: Option<(String, Vec<String>)> = None;
        for line in ddl.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("CREATE TABLE IF NOT EXISTS ") {
                let name = rest.split_whitespace().next().unwrap_or_default();
                table = Some((name.to_owned(), Vec::new()));
                continue;
            }
            let Some((_, cols)) = table.as_mut() else {
                continue;
            };
            if line.starts_with(");") {
                out.push(table.take().expect("inside a table"));
                continue;
            }
            if line.is_empty() || line.starts_with("--") {
                continue;
            }
            let ident = line.split([' ', '(']).next().unwrap_or_default();
            if matches!(
                ident.to_uppercase().as_str(),
                "UNIQUE" | "KEY" | "CONSTRAINT" | "FOREIGN" | "PRIMARY" | "INDEX"
            ) {
                continue;
            }
            cols.push(ident.to_owned());
        }
        out
    }

    /// The three dialects declare the same tables with the same columns in the
    /// same order.
    ///
    /// Only `SQLITE_UP` is ever executed, so a column forgotten in one of the
    /// other two would be invisible to every other test in this crate. The
    /// sibling migration adopted this control after measuring that a three-way
    /// eyeball diff no longer works on commented DDL; the same applies here.
    ///
    /// Break-verified: dropping `branch` from `MYSQL_UP` alone turns this red
    /// and nothing else in the suite notices.
    #[test]
    fn every_dialect_declares_the_same_schedule_columns() {
        let pg = columns_by_table(super::POSTGRES_UP);
        let my = columns_by_table(super::MYSQL_UP);
        let sq = columns_by_table(super::SQLITE_UP);

        assert_eq!(
            pg.iter().map(|(t, _)| t).collect::<Vec<_>>(),
            vec!["qa_schedules", "qa_schedule_ticks"],
            "the parser lost a table; every assertion below would then compare \
             two empty lists and pass vacuously"
        );
        assert!(
            pg.iter().all(|(_, c)| c.len() >= 9),
            "the parser produced a suspiciously short column list: {pg:?}"
        );
        assert_eq!(pg, my, "POSTGRES_UP and MYSQL_UP disagree");
        assert_eq!(pg, sq, "POSTGRES_UP and SQLITE_UP disagree");
    }

    /// Every index declared by a dialect, as `(name, unique, columns)`.
    ///
    /// Two syntaxes: Postgres and `SQLite` write free-standing
    /// `CREATE [UNIQUE] INDEX ... ON t(cols)`, `MySQL` writes
    /// `[UNIQUE] KEY name (cols)` inline in the table body.
    fn indexes(ddl: &str) -> Vec<(String, bool, Vec<String>)> {
        let mut out = Vec::new();
        // `--` lines are stripped *before* flattening: a comment above a
        // statement would otherwise be folded onto its front and the statement
        // would stop matching its `CREATE ...` prefix.
        let flat = ddl
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("--"))
            .collect::<Vec<_>>()
            .join(" ");
        for stmt in flat.split(';') {
            let stmt = stmt.trim();
            let (unique, rest) =
                if let Some(r) = stmt.strip_prefix("CREATE UNIQUE INDEX IF NOT EXISTS ") {
                    (true, r)
                } else if let Some(r) = stmt.strip_prefix("CREATE INDEX IF NOT EXISTS ") {
                    (false, r)
                } else {
                    continue;
                };
            let (name, cols) = rest
                .split_once(" ON ")
                .expect("index statement has an ON clause");
            let cols = cols
                .split_once('(')
                .expect("index statement has a column list")
                .1
                .trim_end_matches(')');
            out.push((
                name.trim().to_owned(),
                unique,
                cols.split(',').map(|c| c.trim().to_owned()).collect(),
            ));
        }
        for line in ddl.lines() {
            let line = line.trim().trim_end_matches(',');
            let (unique, rest) = if let Some(r) = line.strip_prefix("UNIQUE KEY ") {
                (true, r)
            } else if let Some(r) = line.strip_prefix("KEY ") {
                (false, r)
            } else {
                continue;
            };
            let (name, cols) = rest.split_once(" (").expect("KEY clause has a column list");
            out.push((
                name.trim().to_owned(),
                unique,
                cols.trim_end_matches(')')
                    .split(',')
                    .map(|c| c.trim().to_owned())
                    .collect(),
            ));
        }
        out.sort();
        out
    }

    /// The three dialects declare the same indexes, with the same uniqueness and
    /// the same column order.
    ///
    /// The companion to the column test, and the sibling migration records why
    /// both are needed: changing an index in one server-dialect blob alone left
    /// its whole suite green, because the executed tests run against `SQLite`
    /// and the column parity test compares columns.
    ///
    /// Break-verified: dropping the leading `tenant_id` from
    /// `idx_qa_schedule_ticks_claim` in `MYSQL_UP` only is what turns this red.
    #[test]
    fn every_dialect_declares_the_same_schedule_indexes() {
        let pg = indexes(super::POSTGRES_UP);
        let my = indexes(super::MYSQL_UP);
        let sq = indexes(super::SQLITE_UP);

        assert_eq!(
            pg.len(),
            3,
            "expected 3 indexes; the parser found {} and every comparison below \
             would then be over the wrong set: {pg:?}",
            pg.len()
        );
        assert_eq!(pg, my, "POSTGRES_UP and MYSQL_UP declare different indexes");
        assert_eq!(
            pg, sq,
            "POSTGRES_UP and SQLITE_UP declare different indexes"
        );
    }

    /// In-memory `SQLite` with **every** migration this gear declares applied,
    /// through the real [`super::super::Migrator`].
    ///
    /// `max_connections(1)` is load-bearing: each `SQLite` `:memory:`
    /// connection is its own database, so a larger pool would let a query land
    /// on one where the migration never ran.
    ///
    /// `PRAGMA foreign_keys = ON` is an explicit statement of what the cascade
    /// test depends on and changes nothing on its own — the `sqlx` `SQLite`
    /// driver already enables foreign keys on every connection it opens. The
    /// sibling migration records the measurement: removing the statement leaves
    /// the suite green, while `PRAGMA foreign_keys = OFF` turns the cascade
    /// assertions red, which is what proves they are not vacuous.
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

    /// Collect the `name` column of a one-column query.
    async fn name_column(conn: &DatabaseConnection, sql: String) -> Vec<String> {
        use sea_orm::Statement;
        conn.query_all(Statement::from_string(
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
            // Added by `m20260818_000007_schedule_notifications`. Named here
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

    /// `down()` is dead weight unless it actually drops the tables, and it has
    /// to drop them children-first or the foreign key refuses.
    ///
    /// `super::Migration` directly, **not** a loop over
    /// `Migrator::migrations()`: that loop returns declaration order and `down()`
    /// has to run in the reverse of it, so with two migrations in the tree the
    /// loop would drop Phase A's tables first and then run this migration's
    /// `down()` against a schema that no longer has them. Each migration's test
    /// owns its own `down()`.
    #[tokio::test]
    async fn the_schedules_down_migration_drops_both_tables() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();

        for table in ["qa_schedules", "qa_schedule_ticks"] {
            conn.execute_unprepared(&format!("SELECT 1 FROM {table}"))
                .await
                .expect_err("every table must be gone after down()");
        }

        // Phase A's tables are untouched: this migration owns two tables and
        // must not reach the sibling's.
        conn.execute_unprepared("SELECT 1 FROM qa_runs")
            .await
            .expect("this migration's down() must not drop Phase A's tables");
    }
}
