# Run Log Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A finished run's log is still served after the broadcaster has evicted it and after the gears pod has restarted.

**Architecture:** Every ingested log line is buffered per run in a new `LogArchive` port; the buffer is drained into a new `qa_run_logs` table by the dispatcher tick and by `IngestService::finish`; the terminal branch of `GET /qa/v1/runs/{id}/logs` serves that durable text and falls back to today's in-memory tail when there is no row.

**Tech Stack:** Rust, SeaORM + `sea_orm_migration`, `toolkit_db` SecureORM (`.secure().scope_with(scope)`), `async_trait`, axum SSE, `time::OffsetDateTime`, `tokio`.

**Spec:** `gears/qa-platform/docs/superpowers/specs/2026-08-31-run-log-persistence-design.md`

## Global Constraints

- Toolchain: `export PATH="$HOME/.cargo/bin:$PATH"` before any cargo command.
- Clippy is `-D warnings`. Never run `cargo test --all-targets` at workspace level.
- All cargo commands are scoped to this crate: `-p qa-runs` — that is the package `name` in `qa-runs/Cargo.toml`; there is no `cf-gears-` prefix on it, unlike most gears in this workspace.
- **Never pipe a command whose exit code you need.** `cmd | tail` reports `tail`'s status. Redirect to a file and read `$?`. This project has produced false green reports this way more than once.
- `npm run lint` is broken repo-wide and `cargo fmt --check` is red on many untouched files. Both pre-existing — not findings, do not fix.
- **Break-test every guard.** For each guard added, mutate the code so the test *should* fail and confirm it does. Record the mutation and the observed failure in the commit message or the ledger. A guard that cannot fail is worse than no guard; this gear has shipped one.
- **No log text in any error message.** A `DomainError` from these paths carries the run id and the failure class, never a line of log.
- Table name: `qa_run_logs`. Columns exactly: `run_id`, `tenant_id`, `text`, `lines`, `updated_at`.
- Postgres timestamps are `TIMESTAMPTZ` (`m20260813_000003_initial.rs:243` uses it for `timeout_at`).
- `origin` is forbidden. `./push-to-fork.sh` is the only push, and only when the user asks.

---

## Structural refinement to the spec, decided during planning

**D-RLP-5 says "a `RunLogsRepository` port with a `RunLogsSeaRepo` implementation".
Keep the separate trait; do not add a separate struct or a fourth type parameter.**

`AppServices<R, Q, S>` (`domain/service/mod.rs:574`) is generic over three
repository types, and `ConcreteAppServices` (`infra/mod.rs:31`) pins them. A
fourth parameter would ripple through every service signature, the gear wiring
and every test that constructs `AppServices`. Meanwhile the trait's `get<C: DBRunner>`
method is generic, so `Arc<dyn RunLogsRepository>` is not an option either.

So: **a separate trait `RunLogsRepository`, implemented by the existing
`OrmRunsRepository` struct**, and where the methods are needed the bound becomes
`R: RunsRepository + RunLogsRepository`. Zero new type parameters, and the two
traits stay separate objects — which is what D-RLP-1 and D-RLP-7 actually rely
on. Putting log methods *on `RunsRepository` itself* was rejected: it would place
`get_log` next to `list`, and the whole point of the separate table is that those
two must not be within easy reach of one another.

`MockRunsRepository` (`domain/service/test_support.rs:299`) must therefore also
implement `RunLogsRepository`. Task 2 does that.

---

## File Structure

| Path | Responsibility |
|---|---|
| `infra/storage/migrations/m20260831_000008_run_logs.rs` | **Create.** The `qa_run_logs` DDL in three dialects, plus its SQL-shape and cascade tests. |
| `infra/storage/entity/run_log.rs` | **Create.** SeaORM entity + `Scopable` derive. Nothing else. |
| `domain/repos/run_logs_repo.rs` | **Create.** The `RunLogsRepository` port and its `ArchivedLog` record. |
| `infra/storage/run_logs_sea_repo.rs` | **Create.** `impl RunLogsRepository for OrmRunsRepository` — the upsert and the scoped read. |
| `infra/logs/archive.rs` | **Create.** `RunLogArchive`: pending buffers, `record`, `flush`, `flush_due`. No SQL of its own. |
| `domain/service/mod.rs` | **Modify.** The `LogArchive` port and `FlushReport`; the `AppServices` bound. |
| `domain/system_actor.rs` | **Modify.** `for_log_archive(tenant)`. |
| `domain/service/ingest.rs` | **Modify.** `IngestDeps.archive`; `fan_out_log` records; `finish` flushes. |
| `api/rest/handlers/runs.rs` | **Modify.** Terminal branch reads the archive, falls back to `replay`. |
| `gear.rs` | **Modify.** Build the archive, hand it to ingest, call `flush_due` in the dispatcher tick. |
| `infra/logs/broadcast.rs` | **Modify.** Correct two stale comments. |
| `infra/storage/migrations/mod.rs`, `infra/storage/entity/mod.rs`, `infra/logs/mod.rs`, `domain/repos/mod.rs`, `infra/storage/mod.rs` | **Modify.** Module registration only. |

---

## Task 1: The table and its entity

**Files:**
- Create: `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/migrations/m20260831_000008_run_logs.rs`
- Create: `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/entity/run_log.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/migrations/mod.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/entity/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: table `qa_run_logs`; entity module `run_log` exporting `Entity`, `Model`, `ActiveModel`, `Column`. `Model` fields: `run_id: Uuid`, `tenant_id: Uuid`, `text: String`, `lines: i64`, `updated_at: OffsetDateTime`.

- [ ] **Step 1: Write the failing SQL-shape test**

Create the migration file with only the test module and empty DDL constants, so the test compiles and fails on content. Copy the `added_columns` / DDL-parsing helper style from `m20260818_000006_collect_target.rs`'s test module.

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use super::{MYSQL_UP, PG_UP, SQLITE_UP};

    /// Every dialect must declare the same five columns in the same order.
    #[test]
    fn all_three_dialects_declare_the_same_columns() {
        for (name, ddl) in [
            ("postgres", PG_UP),
            ("mysql", MYSQL_UP),
            ("sqlite", SQLITE_UP),
        ] {
            let names = column_names(ddl);
            assert_eq!(
                names,
                vec!["run_id", "tenant_id", "text", "lines", "updated_at"],
                "{name} declares the wrong columns or order",
            );
        }
    }

    /// The cascade is the entire retention policy (spec D-RLP-1), so it is
    /// asserted in the DDL text of every dialect and not only behaviourally.
    #[test]
    fn every_dialect_cascades_from_qa_runs() {
        for (name, ddl) in [
            ("postgres", PG_UP),
            ("mysql", MYSQL_UP),
            ("sqlite", SQLITE_UP),
        ] {
            assert!(
                ddl.contains("REFERENCES qa_runs(id) ON DELETE CASCADE"),
                "{name} must cascade from qa_runs, or a deleted run orphans its log",
            );
        }
    }

    /// `run_id` is the primary key, so a doubled flush cannot make two rows.
    #[test]
    fn run_id_is_the_primary_key_in_every_dialect() {
        for (name, ddl) in [
            ("postgres", PG_UP),
            ("mysql", MYSQL_UP),
            ("sqlite", SQLITE_UP),
        ] {
            let pk_line = ddl
                .lines()
                .find(|l| l.contains("PRIMARY KEY"))
                .unwrap_or_else(|| panic!("{name} declares no primary key"));
            assert!(
                pk_line.contains("run_id"),
                "{name}'s primary key must be run_id, found: {pk_line}",
            );
        }
    }

    /// Column names of a `CREATE TABLE` body, in declaration order.
    fn column_names(ddl: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in ddl.lines() {
            let line = line.trim().trim_end_matches(',');
            if line.is_empty()
                || line.starts_with("--")
                || line.starts_with("CREATE")
                || line.starts_with(')')
                || line.starts_with("PRIMARY KEY")
            {
                continue;
            }
            if let Some(ident) = line.split_whitespace().next() {
                out.push(ident.to_owned());
            }
        }
        out
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs m20260831_000008 > /tmp/t1.txt 2>&1; echo "exit=$?"
```

Expected: FAIL — `column_names` returns an empty vec against the empty DDL constants.

- [ ] **Step 3: Write the migration**

```rust
//! `qa_run_logs` — the durable copy of a run's log output.
//!
//! # Its own table, not a column on `qa_runs`
//!
//! Legacy holds the same text in `run_results.raw_logs` and its own comment
//! records that selecting that column for every row of the full run history
//! once OOMKilled the manager. A separate table makes that query structurally
//! impossible rather than merely discouraged; `runs_sea_repo`'s list guard
//! (`a_run_list_query_never_reaches_the_log_table`) is what keeps it that way.
//!
//! # `run_id` is the primary key
//!
//! One log per run, so the append needs no prior lookup and a doubled flush
//! cannot produce a second row. This differs from `qa_run_test_results`, which
//! carries a surrogate `id` because a run has many results.
//!
//! # There is no size cap, by decision
//!
//! User decision, 2026-08-31, with the risk stated: nothing bounds one run's
//! text, and retention is the cascade alone. `lines` exists so a consumer can
//! learn a log's size without selecting it.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_run_logs (
    run_id UUID PRIMARY KEY NOT NULL REFERENCES qa_runs(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL,
    text TEXT NOT NULL DEFAULT '',
    lines BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL
);
";

const MYSQL_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_run_logs (
    run_id CHAR(36) PRIMARY KEY NOT NULL REFERENCES qa_runs(id) ON DELETE CASCADE,
    tenant_id CHAR(36) NOT NULL,
    text LONGTEXT NOT NULL,
    lines BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMP NOT NULL
);
";

const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_run_logs (
    run_id TEXT PRIMARY KEY NOT NULL REFERENCES qa_runs(id) ON DELETE CASCADE,
    tenant_id TEXT NOT NULL,
    text TEXT NOT NULL DEFAULT '',
    lines BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMP NOT NULL
);
";

const DOWN: &str = r"
DROP TABLE IF EXISTS qa_run_logs;
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
```

**Confirm against the existing file before committing:** open
`m20260813_000003_initial.rs` and check the MySQL and SQLite column types it uses
for `UUID` and for timestamps, and match them exactly rather than the guesses
above. The three dialect bodies in that file are the authority; if it stores
UUIDs as `BINARY(16)` in MySQL, use that.

- [ ] **Step 4: Register the migration and the entity**

In `migrations/mod.rs`, add `mod m20260831_000008_run_logs;` after the `000007`
line and `Box::new(m20260831_000008_run_logs::Migration),` as the last element of
the `migrations()` vec — order matters, it runs last.

Create `entity/run_log.rs`:

```rust
//! `qa_run_logs` — one durable log per run. See
//! `migrations::m20260831_000008_run_logs` for why it is its own table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_run_logs")]
#[secure(tenant_col = "tenant_id", resource_col = "run_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub run_id: Uuid,
    pub tenant_id: Uuid,
    #[sea_orm(column_type = "Text")]
    pub text: String,
    pub lines: i64,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
```

`resource_col = "run_id"` because the run id *is* this row's identity — there is
no surrogate key for a scope to filter on. Add `pub mod run_log;` to
`entity/mod.rs` in alphabetical position.

- [ ] **Step 5: Run the shape tests to verify they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs m20260831_000008 > /tmp/t1.txt 2>&1; echo "exit=$?"
```

Expected: PASS, 3 tests.

- [ ] **Step 6: Write the cascade test on the SQLite tier**

Add to the migration's test module. Copy the connection setup verbatim from
`m20260813_000003_initial.rs:840-862` — including its `PRAGMA foreign_keys = ON`
and the comment explaining that `toolkit-db` does not set it, so the pragma is
load-bearing rather than decorative.

```rust
    /// Deleting a run deletes its log. **The cascade is the entire retention
    /// policy** (spec D-RLP-1), so this is the test that stops an orphaned log
    /// surviving its run forever.
    ///
    /// `PRAGMA foreign_keys = ON` is load-bearing here for the reason
    /// `m20260813_000003_initial`'s cascade tests record: `toolkit-db` does not
    /// set it, and SQLite ignores foreign keys without it. The honest
    /// break-test is `OFF`, which turns this test red.
    #[tokio::test]
    async fn deleting_a_run_deletes_its_log() {
        let conn = sqlite_migrated().await;
        conn.execute_unprepared("PRAGMA foreign_keys = ON;")
            .await
            .unwrap();

        let tenant = uuid(1);
        let run_id = uuid(2);
        seed_run(&conn, run_id, tenant).await;

        run_log::ActiveModel {
            run_id: ActiveValue::Set(run_id),
            tenant_id: ActiveValue::Set(tenant),
            text: ActiveValue::Set("[node] hello\n".to_owned()),
            lines: ActiveValue::Set(1),
            updated_at: ActiveValue::Set(now()),
        }
        .insert(&conn)
        .await
        .unwrap();

        assert!(
            run_log::Entity::find_by_id(run_id)
                .one(&conn)
                .await
                .unwrap()
                .is_some(),
            "precondition: the log row must exist before the delete",
        );

        run::Entity::delete_by_id(run_id).exec(&conn).await.unwrap();

        assert!(
            run_log::Entity::find_by_id(run_id)
                .one(&conn)
                .await
                .unwrap()
                .is_none(),
            "the log must not survive its run",
        );
    }
```

`seed_run`, `sqlite_migrated`, `uuid` and `now` already exist in the sibling
migrations' test modules (`m20260818_000006_collect_target.rs` has `uuid`, `now`
and a run-seeding helper) — reuse their bodies rather than inventing new ones.

- [ ] **Step 7: Run it, then break-test it**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs deleting_a_run_deletes_its_log > /tmp/t1.txt 2>&1; echo "exit=$?"
```

Expected: PASS.

Now the break-test. Change the pragma line to `PRAGMA foreign_keys = OFF;` and
re-run. Expected: FAIL on "the log must not survive its run". **Revert the
mutation.** Then remove `ON DELETE CASCADE` from `SQLITE_UP`, re-run, expect FAIL
in both `every_dialect_cascades_from_qa_runs` and this test, and revert.

- [ ] **Step 8: Commit**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust
git add gears/qa-platform/qa-runs/qa-runs/src/infra/storage/migrations/ \
        gears/qa-platform/qa-runs/qa-runs/src/infra/storage/entity/
git commit -m "feat(qa-runs): qa_run_logs table, its own so no list query can reach it

Break-tested: PRAGMA foreign_keys = OFF and a removed ON DELETE CASCADE each
turn the cascade test red."
```

---

## Task 2: The repository port, the upsert, and the list-query guard

**Files:**
- Create: `gears/qa-platform/qa-runs/qa-runs/src/domain/repos/run_logs_repo.rs`
- Create: `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/run_logs_sea_repo.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/repos/mod.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/infra/storage/mod.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/test_support.rs`

**Interfaces:**
- Consumes: `run_log::Entity`, `run_log::ActiveModel`, `run_log::Column` from Task 1.
- Produces:
  - `ArchivedLog { pub text: String, pub lines: i64 }`
  - `trait RunLogsRepository` with:
    - `async fn append_log<C: DBRunner>(&self, runner: &C, scope: &AccessScope, run_id: Uuid, tenant_id: Uuid, text: &str, lines: i64) -> Result<(), DomainError>`
    - `async fn get_log<C: DBRunner>(&self, runner: &C, scope: &AccessScope, run_id: Uuid) -> Result<Option<ArchivedLog>, DomainError>`
  - `impl RunLogsRepository for OrmRunsRepository`
  - `impl RunLogsRepository for MockRunsRepository`

- [ ] **Step 1: Write the failing round-trip and append tests**

Add to `run_logs_sea_repo.rs`'s test module. Copy the SQLite fixture setup from
`runs_sea_repo.rs`'s existing test module (it builds a migrated connection and a
scope; reuse those helpers verbatim).

```rust
    /// Two appends concatenate. **This is the property the flush depends on**
    /// (spec §4.4): the concatenation happens in the statement, so a flush
    /// costs the new text and not the whole log.
    #[tokio::test]
    async fn a_second_append_concatenates_rather_than_replacing() {
        let fx = fixture().await;
        let run_id = fx.seed_run().await;

        fx.repo
            .append_log(&fx.conn, &fx.scope, run_id, fx.tenant, "[a] one\n", 1)
            .await
            .unwrap();
        fx.repo
            .append_log(&fx.conn, &fx.scope, run_id, fx.tenant, "[a] two\n", 1)
            .await
            .unwrap();

        let stored = fx
            .repo
            .get_log(&fx.conn, &fx.scope, run_id)
            .await
            .unwrap()
            .expect("the row must exist after two appends");

        assert_eq!(stored.text, "[a] one\n[a] two\n");
        assert_eq!(stored.lines, 2, "line counts must add, not overwrite");
    }

    /// A run with no archived log reads as absent, not as an error. This is the
    /// ordinary case for every run that finished before this migration, and it
    /// is what drives the handler's fallback to the in-memory tail.
    #[tokio::test]
    async fn a_run_with_no_archived_log_reads_as_none() {
        let fx = fixture().await;
        let run_id = fx.seed_run().await;

        assert!(
            fx.repo
                .get_log(&fx.conn, &fx.scope, run_id)
                .await
                .unwrap()
                .is_none(),
        );
    }

    /// Tenant A cannot read tenant B's archived log. The scope, not the
    /// handler, is what enforces this — so a defect in the handler cannot turn
    /// into a cross-tenant read on its own.
    #[tokio::test]
    async fn a_foreign_tenant_cannot_read_an_archived_log() {
        let fx = fixture().await;
        let run_id = fx.seed_run().await;
        fx.repo
            .append_log(&fx.conn, &fx.scope, run_id, fx.tenant, "[a] secret\n", 1)
            .await
            .unwrap();

        let stranger = fx.scope_for_tenant(uuid(999));

        assert!(
            fx.repo
                .get_log(&fx.conn, &stranger, run_id)
                .await
                .unwrap()
                .is_none(),
            "another tenant's scope must not see this log",
        );
    }
```

- [ ] **Step 2: Run to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs run_logs_sea_repo > /tmp/t2.txt 2>&1; echo "exit=$?"
```

Expected: FAIL to compile — `append_log` and `get_log` do not exist.

- [ ] **Step 3: Write the port**

`domain/repos/run_logs_repo.rs`:

```rust
//! The archived-log repository port.
//!
//! # A separate trait, deliberately, and not a separate struct
//!
//! `OrmRunsRepository` implements this as well as [`RunsRepository`], because
//! `AppServices` is generic over its repository types and a fourth type
//! parameter would ripple through every service signature for no gain. What
//! matters is that the two *traits* are separate: `get_log` must not sit next
//! to `list`, or a future "list runs with their logs" convenience has
//! everything it needs to reproduce legacy's OOM.
//!
//! `a_run_list_query_never_reaches_the_log_table` is the guard that keeps that
//! true regardless of intent.

use async_trait::async_trait;
use toolkit_db::{AccessScope, DBRunner};
use uuid::Uuid;

use crate::domain::error::DomainError;

/// One run's archived log, and enough metadata to describe it without
/// re-reading the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchivedLog {
    pub text: String,
    pub lines: i64,
}

#[async_trait]
pub trait RunLogsRepository: Send + Sync {
    /// Concatenate `text` onto `run_id`'s archived log, creating the row if it
    /// is the first append, and add `lines` to its count.
    ///
    /// **The concatenation happens in the statement.** Reading the row,
    /// appending in Rust and writing it back would re-transfer the whole log on
    /// every flush and would lose a concurrent append.
    ///
    /// There is no size cap: user decision, 2026-08-31, risk recorded in the
    /// design's §8.
    async fn append_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
        tenant_id: Uuid,
        text: &str,
        lines: i64,
    ) -> Result<(), DomainError>;

    /// Read `run_id`'s archived log, or `None` when it has none.
    ///
    /// `None` is the ordinary answer for a run that finished before this table
    /// existed, and it is what makes the handler fall back to the broadcaster's
    /// in-memory tail rather than showing an empty pane.
    async fn get_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<Option<ArchivedLog>, DomainError>;
}
```

Add `mod run_logs_repo;` and re-export `ArchivedLog` and `RunLogsRepository` from
`domain/repos/mod.rs`, matching how `RunsRepository` is exported there.

- [ ] **Step 4: Write the implementation**

`infra/storage/run_logs_sea_repo.rs`:

```rust
//! `impl RunLogsRepository for OrmRunsRepository`.

use async_trait::async_trait;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::{AccessScope, DBRunner};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{ArchivedLog, RunLogsRepository};
use crate::infra::storage::entity::run_log;
use crate::infra::storage::runs_sea_repo::OrmRunsRepository;

#[async_trait]
impl RunLogsRepository for OrmRunsRepository {
    async fn append_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
        tenant_id: Uuid,
        text: &str,
        lines: i64,
    ) -> Result<(), DomainError> {
        // A SCOPED UPDATE FIRST, THEN AN INSERT IF THERE WAS NOTHING TO UPDATE.
        //
        // `on_conflict` was the obvious shape and is not used, because the
        // insert half of an upsert takes no `AccessScope` — it would be an
        // unscoped write. This form keeps the concatenation in the statement
        // (`text = text || $1`, exactly as `add_result_counts` does
        // `passed = passed + $n`) *and* keeps the scope on the path that
        // touches an existing row.
        //
        // The two statements cannot race for one run: `flush` takes the
        // pending buffer out of the map before writing, so only one caller
        // ever holds a given run's text.
        let updated = run_log::Entity::update_many()
            .filter(Condition::all().add(Expr::col(run_log::Column::RunId).eq(run_id)))
            .secure()
            .scope_with(scope)
            .col_expr(
                run_log::Column::Text,
                Expr::col(run_log::Column::Text).concat(Expr::val(text)),
            )
            .col_expr(
                run_log::Column::Lines,
                Expr::col(run_log::Column::Lines).add(Expr::val(lines)),
            )
            .col_expr(
                run_log::Column::UpdatedAt,
                Expr::value(OffsetDateTime::now_utc()),
            )
            .exec(runner)
            .await
            .map_err(db_err)?;

        if updated.rows_affected > 0 {
            return Ok(());
        }

        // First append for this run. `tenant_id` comes from the tenant-bound
        // system context the flush minted, never from a caller.
        run_log::ActiveModel {
            run_id: ActiveValue::Set(run_id),
            tenant_id: ActiveValue::Set(tenant_id),
            text: ActiveValue::Set(text.to_owned()),
            lines: ActiveValue::Set(lines),
            updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
        }
        .insert(runner)
        .await
        .map_err(db_err)?;

        Ok(())
    }

    async fn get_log<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<Option<ArchivedLog>, DomainError> {
        let found = run_log::Entity::find()
            .filter(run_log::Column::RunId.eq(run_id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        Ok(found.map(|m| ArchivedLog {
            text: m.text,
            lines: m.lines,
        }))
    }
}
```

**One thing to settle against the existing code rather than trusting the above:**

`db_err` is a private helper in `runs_sea_repo.rs` / `schedules_sea_repo.rs`.
Either make it `pub(crate)` or copy its body — do not invent a new error
mapping, because the classification it performs is what keeps database text out
of published errors.

**And one thing to verify empirically:** `Expr::col(..).concat(..)` is the line
most likely to differ by SeaORM version. It must render as `||` on Postgres and
`CONCAT` on MySQL. Assert on the rendered SQL if the API is ambiguous. The
requirement is non-negotiable: **concatenation in the statement, never
read-modify-write** — the latter re-transfers the whole log on every flush.

- [ ] **Step 5: Run the three tests to verify they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs run_logs_sea_repo > /tmp/t2.txt 2>&1; echo "exit=$?"
```

Expected: PASS, 3 tests.

- [ ] **Step 6: Break-test the cross-tenant guard**

Remove `.secure().scope_with(scope)` from `get_log` and re-run.
Expected: FAIL on `a_foreign_tenant_cannot_read_an_archived_log`. Revert.

- [ ] **Step 7: Write the list-query guard (spec D-RLP-7)**

Add to `runs_sea_repo.rs`'s test module, not the new file — it is a claim about
the runs list path.

```rust
    /// **Legacy's OOM, made unrepeatable.** Selecting bulk log text for every
    /// row of the full run history once OOMKilled the source system's manager.
    /// The log lives in its own table so that query cannot be written by
    /// accident; this asserts the list path has not learned to join it.
    ///
    /// The rendered SQL is inspected rather than the source text: a grep over
    /// this file would pass while a join arrived through a shared query
    /// builder.
    #[test]
    fn a_run_list_query_never_reaches_the_log_table() {
        let sql = rendered_list_sql();
        assert!(
            !sql.contains("qa_run_logs"),
            "the run list query must not touch qa_run_logs; got: {sql}",
        );
    }
```

`rendered_list_sql()` must build the query the way `OrmRunsRepository`'s list
method builds it and render it with
`.build(sea_orm::DatabaseBackend::Postgres).to_string()`. Follow whichever
existing test in this file already renders a query; if none does, the SQL-shape
tests in `m20260813_000003_initial.rs:765-881` are the precedent for asserting
against generated SQL in this crate.

- [ ] **Step 8: Run it, then break-test it**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs a_run_list_query_never_reaches_the_log_table > /tmp/t2.txt 2>&1; echo "exit=$?"
```

Expected: PASS. Break-test: add a join or a `.column(run_log::Column::Text)` to
the list query builder and confirm the test goes red. Revert.

- [ ] **Step 9: Teach `MockRunsRepository` the new trait**

In `domain/service/test_support.rs`, beside `impl RunsRepository for MockRunsRepository`
at line 299, add an in-memory implementation backed by a
`Mutex<HashMap<Uuid, ArchivedLog>>` field on the mock. `append_log`
concatenates into the map; `get_log` clones out. This is what lets Tasks 3-6
test service behaviour without a database.

- [ ] **Step 10: Full crate gate and commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo clippy -p qa-runs --all-targets -- -D warnings > /tmp/c2.txt 2>&1; echo "clippy=$?"
cargo test -p qa-runs > /tmp/t2.txt 2>&1; echo "test=$?"
```

Both must be 0.

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust
git add gears/qa-platform/qa-runs/qa-runs/src/domain/repos/ \
        gears/qa-platform/qa-runs/qa-runs/src/infra/storage/ \
        gears/qa-platform/qa-runs/qa-runs/src/domain/service/test_support.rs
git commit -m "feat(qa-runs): RunLogsRepository, appending in the statement

The list-query guard renders the SQL rather than grepping the source, so a join
arriving through a shared builder cannot slip past. Break-tested by adding the
join and by dropping scope_with from get_log."
```

---

## Task 3: The archive — port and pending buffers

**Files:**
- Create: `gears/qa-platform/qa-runs/qa-runs/src/infra/logs/archive.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/mod.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/infra/logs/mod.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/system_actor.rs`

**Interfaces:**
- Consumes: `RunLogsRepository` from Task 2.
- Produces:
  - `trait LogArchive` (in `domain/service/mod.rs`) with `fn record(&self, tenant_id: Uuid, run_id: Uuid, line: &str)`, `async fn flush(&self, run_id: Uuid) -> Result<(), DomainError>`, `async fn flush_due(&self) -> FlushReport`
  - `struct FlushReport { pub runs: usize, pub lines: u64, pub failed: usize }`
  - `struct RunLogArchive<R>` (in `infra/logs/archive.rs`) with `pub fn new(db: SerializedDb, runs: Arc<R>, policy_enforcer: PolicyEnforcer) -> Self`
  - `system_actor::for_log_archive(tenant: TenantBound) -> SecurityContext`

- [ ] **Step 1: Write the failing buffer tests**

In `archive.rs`'s test module, against `MockRunsRepository`:

```rust
    /// A drained buffer is not re-appended. Two flushes with one line between
    /// them must leave that line in the row exactly once.
    #[tokio::test]
    async fn flushing_twice_does_not_duplicate_a_line() {
        let fx = fixture();
        fx.archive.record(fx.tenant, fx.run_id, "[a] one");
        fx.archive.flush(fx.run_id).await.unwrap();
        fx.archive.flush(fx.run_id).await.unwrap();

        assert_eq!(fx.stored_text(), "[a] one\n");
    }

    /// **A failed flush must not lose text.** The buffer is put back, so a
    /// transient database error costs a delayed write and not a log.
    #[tokio::test]
    async fn a_failed_flush_keeps_its_text_for_the_next_one() {
        let fx = fixture();
        fx.archive.record(fx.tenant, fx.run_id, "[a] one");

        fx.fail_next_append();
        assert!(
            fx.archive.flush(fx.run_id).await.is_err(),
            "the injected failure must surface",
        );

        fx.archive.record(fx.tenant, fx.run_id, "[a] two");
        fx.archive.flush(fx.run_id).await.unwrap();

        assert_eq!(
            fx.stored_text(),
            "[a] one\n[a] two\n",
            "the line from the failed flush must survive",
        );
    }

    /// `flush_due` drains every buffered run, which is how a run retired by
    /// `runs::retire` or `dispatch::transition` gets archived without those
    /// services knowing this port exists.
    #[tokio::test]
    async fn flush_due_drains_every_buffered_run() {
        let fx = fixture();
        fx.archive.record(fx.tenant, uuid(10), "[a] first run");
        fx.archive.record(fx.tenant, uuid(11), "[a] second run");

        let report = fx.archive.flush_due().await;

        assert_eq!(report.runs, 2);
        assert_eq!(report.lines, 2);
        assert_eq!(report.failed, 0);
        assert_eq!(fx.stored_text_for(uuid(10)), "[a] first run\n");
        assert_eq!(fx.stored_text_for(uuid(11)), "[a] second run\n");
    }

    /// A run with nothing buffered is not written at all — an idle tick must
    /// not touch the database once per known run.
    #[tokio::test]
    async fn flush_due_writes_nothing_when_no_lines_were_recorded() {
        let fx = fixture();
        let report = fx.archive.flush_due().await;
        assert_eq!(report.runs, 0);
        assert_eq!(fx.append_calls(), 0);
    }

    /// An empty buffer left behind by a flush is not kept forever: a finished
    /// run must not hold a map entry for the process's lifetime.
    #[tokio::test]
    async fn a_drained_buffer_is_removed_from_the_map() {
        let fx = fixture();
        fx.archive.record(fx.tenant, fx.run_id, "[a] one");
        fx.archive.flush(fx.run_id).await.unwrap();
        assert_eq!(fx.buffered_runs(), 0);
    }
```

- [ ] **Step 2: Run to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs logs::archive > /tmp/t3.txt 2>&1; echo "exit=$?"
```

Expected: FAIL to compile — `RunLogArchive` does not exist.

- [ ] **Step 3: Add the port to `domain/service/mod.rs`**

Beside `LogFanout`:

```rust
/// What a flush pass did, for the dispatcher tick's log line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushReport {
    pub runs: usize,
    pub lines: u64,
    pub failed: usize,
}

/// Accumulate a run's log lines and write them to durable storage.
///
/// # Why this is not part of `LogFanout`
///
/// `LogFanout` is a fire-and-forget fan-out port whose two methods are
/// synchronous, and `reap` is called from four services that have no business
/// acquiring a database transaction. Keeping the archive separate keeps each
/// port's purpose answerable in one sentence.
///
/// # Why there is no `read`
///
/// The read path needs the repository, not the accumulator — the same reason
/// `LogFanout` deliberately has no `subscribe`.
#[async_trait]
pub trait LogArchive: Send + Sync {
    /// Buffer one line for `run_id`. Synchronous and infallible, like
    /// `LogFanout::publish`, so the per-line ingest path never awaits.
    ///
    /// `tenant_id` is carried on the buffer rather than looked up at flush
    /// time: `fan_out_log` already runs under a tenant-bound context, so the
    /// flush needs no read to learn where the row belongs.
    fn record(&self, tenant_id: Uuid, run_id: Uuid, line: &str);

    /// Drain `run_id`'s buffer into its row.
    ///
    /// On error the drained text is **put back**, so a transient database
    /// failure delays a write instead of losing a log.
    async fn flush(&self, run_id: Uuid) -> Result<(), DomainError>;

    /// Drain every buffer that has pending text. Never returns `Err`: one
    /// run's failure must not stop the others, so failures are counted in the
    /// report and logged.
    async fn flush_due(&self) -> FlushReport;
}
```

- [ ] **Step 4: Write `infra/logs/archive.rs`**

```rust
//! Per-run pending log buffers and the flush that drains them.
//!
//! # The buffer is drained, not a capped tail, and it is not the broadcaster's
//!
//! Reusing `RunLogBroadcaster`'s `retained` map would avoid holding each line
//! twice, and was rejected: `retained` is a *capped tail* that evicts from the
//! front at `MAX_RETAINED_BYTES_PER_RUN`. A run emitting more than that between
//! two flushes would lose its head **before it was ever written**, silently. A
//! drained buffer cannot lose a line it has not yet flushed.
//!
//! The cost is that a pending buffer is unbounded between flushes. That is
//! consistent with the no-cap decision (design §8) and bounded in practice by
//! the flush period.
//!
//! # A newline is added here, once
//!
//! `record` takes a line without its terminator and stores it with one `\n`, so
//! the archived text is a plain log file and the SSE read path can split it
//! back into lines with no ambiguity.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::RunLogsRepository;
use crate::domain::service::{FlushReport, LogArchive};
use crate::domain::system_actor;

/// One run's un-flushed text.
#[derive(Debug)]
struct Pending {
    tenant_id: Uuid,
    text: String,
    lines: i64,
}

#[derive(Debug)]
pub struct RunLogArchive<R> {
    pending: Mutex<HashMap<Uuid, Pending>>,
    db: SerializedDb,
    runs: Arc<R>,
    policy_enforcer: PolicyEnforcer,
}

impl<R> RunLogArchive<R>
where
    R: RunLogsRepository + 'static,
{
    pub fn new(db: SerializedDb, runs: Arc<R>, policy_enforcer: PolicyEnforcer) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            db,
            runs,
            policy_enforcer,
        }
    }

    /// Take one run's pending text out of the map, leaving no empty entry
    /// behind.
    fn take(&self, run_id: Uuid) -> Option<Pending> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        pending.remove(&run_id)
    }

    /// Put drained text back at the **front** of whatever has arrived since,
    /// preserving order. This is what makes a failed flush lossless.
    fn restore(&self, run_id: Uuid, mut taken: Pending) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(newer) = pending.remove(&run_id) {
            taken.text.push_str(&newer.text);
            taken.lines += newer.lines;
        }
        pending.insert(run_id, taken);
    }

    /// The one database write. Separated from `flush` so `flush` owns only the
    /// take/restore decision.
    async fn write(&self, run_id: Uuid, taken: &Pending) -> Result<(), DomainError> {
        let tenant = system_actor::TenantBound::new(taken.tenant_id)
            .ok_or_else(|| DomainError::internal("archived log has a nil tenant"))?;
        let ctx = system_actor::for_log_archive(tenant);
        // The same resolution `IngestService::run_scope` performs
        // (`domain/service/ingest.rs:534`): one fresh `qa.run` scope per call,
        // via the enforcer, never hoisted or cached. `DISPATCH` because this is
        // a write to a run's own child data, which is the action `finish`'s
        // write scope uses.
        let scope = self
            .policy_enforcer
            .access_scope(&ctx, &resources::RUN, actions::DISPATCH, Some(run_id))
            .await?;

        let runs = Arc::clone(&self.runs);
        let text = taken.text.clone();
        let lines = taken.lines;
        let tenant_id = taken.tenant_id;

        self.db
            .with_retry(move |tx| {
                let runs = Arc::clone(&runs);
                let scope = scope.clone();
                let text = text.clone();
                Box::pin(async move {
                    runs.append_log(tx, &scope, run_id, tenant_id, &text, lines)
                        .await
                })
            })
            .await
    }
}

#[async_trait]
impl<R> LogArchive for RunLogArchive<R>
where
    R: RunLogsRepository + 'static,
{
    fn record(&self, tenant_id: Uuid, run_id: Uuid, line: &str) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let entry = pending.entry(run_id).or_insert_with(|| Pending {
            tenant_id,
            text: String::new(),
            lines: 0,
        });
        entry.text.push_str(line);
        entry.text.push('\n');
        entry.lines += 1;
    }

    async fn flush(&self, run_id: Uuid) -> Result<(), DomainError> {
        let Some(taken) = self.take(run_id) else {
            return Ok(());
        };
        match self.write(run_id, &taken).await {
            Ok(()) => {
                debug!(run.id = %run_id, lines = taken.lines, "archived run log");
                Ok(())
            }
            Err(error) => {
                // The text goes back rather than being dropped: this is the
                // difference between a delayed write and a lost log.
                self.restore(run_id, taken);
                Err(error)
            }
        }
    }

    async fn flush_due(&self) -> FlushReport {
        let ids: Vec<Uuid> = {
            let pending = self
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            pending.keys().copied().collect()
        };

        let mut report = FlushReport::default();
        for run_id in ids {
            // Re-read the buffer's size before flushing, so the report counts
            // what was written and not what was queued a moment ago.
            let queued = {
                let pending = self
                    .pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                pending.get(&run_id).map_or(0, |p| p.lines)
            };
            match self.flush(run_id).await {
                Ok(()) if queued > 0 => {
                    report.runs += 1;
                    report.lines += u64::try_from(queued).unwrap_or(0);
                }
                Ok(()) => {}
                Err(error) => {
                    report.failed += 1;
                    // The run id and the failure class, never a line of log.
                    warn!(run.id = %run_id, %error, "archiving this run's log failed; text kept for the next pass");
                }
            }
        }
        report
    }
}
```

**Two things the implementer must resolve, both with a named precedent:**

1. `SerializedDb`, `PolicyEnforcer`, `resources::RUN`, `actions::DISPATCH` and
   `DomainError::internal` need the same imports `ingest.rs` uses. If
   `DomainError` has no `internal` constructor, use whichever variant `ingest.rs`
   uses for an impossible-state error, and **do not put the tenant id in its
   message** if that variant's text is published.
2. `TenantBound::new` returns `None` for a nil tenant
   (`domain/system_actor.rs:190`). The `ok_or_else` above turns that into an
   error rather than archiving under the nil tenant — a nil tenant here would
   mean `record` was called from an unbound context, which is a defect, not a
   case to accommodate.

- [ ] **Step 5: Add `for_log_archive`**

In `domain/system_actor.rs`, beside `for_result_ingest`:

```rust
/// Flushing one run's accumulated log lines into `qa_run_logs`. Tenant-bound to
/// the run's own tenant.
///
/// **There is no `for_log_archive_sweep`, and that is deliberate.**
/// `for_schedule_tick` / `for_schedule_fire` exist as a pair because a sweep
/// must first *enumerate* rows under a nil-tenant context and then write per
/// tenant. `LogArchive::flush_due` enumerates nothing from the database — its
/// work list is an in-memory map and each entry already carries its tenant. So
/// there is no nil-tenant read to authorize, and minting a nil-tenant context
/// with nothing to justify it would widen this gear's elevation surface for
/// free.
#[must_use]
pub fn for_log_archive(tenant: TenantBound) -> SecurityContext {
    log_site("log_archive", tenant.get());
    build_inner(Some(tenant))
}
```

Add `pub mod archive;` and re-export `RunLogArchive` from `infra/logs/mod.rs`.

- [ ] **Step 6: Run the buffer tests to verify they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs logs::archive > /tmp/t3.txt 2>&1; echo "exit=$?"
```

Expected: PASS, 5 tests.

- [ ] **Step 7: Break-test the lossless guarantee**

In `flush`'s error arm, delete the `self.restore(run_id, taken);` line and
re-run. Expected: FAIL on
`a_failed_flush_keeps_its_text_for_the_next_one`. Revert.

Then make `take` leave an empty `Pending` in the map instead of removing it, and
confirm `a_drained_buffer_is_removed_from_the_map` goes red. Revert.

- [ ] **Step 8: Clippy, test, commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo clippy -p qa-runs --all-targets -- -D warnings > /tmp/c3.txt 2>&1; echo "clippy=$?"
cargo test -p qa-runs > /tmp/t3.txt 2>&1; echo "test=$?"
```

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust
git add gears/qa-platform/qa-runs/qa-runs/src/infra/logs/ \
        gears/qa-platform/qa-runs/qa-runs/src/domain/service/mod.rs \
        gears/qa-platform/qa-runs/qa-runs/src/domain/system_actor.rs
git commit -m "feat(qa-runs): LogArchive port and the per-run pending buffers

A drained buffer, not the broadcaster's capped tail: that tail evicts from the
front, so a run emitting more than 512 KiB between flushes would lose its head
before it was ever written. Break-tested by deleting the restore-on-failure."
```

---

## Task 4: Record on ingest, flush on finish

**Files:**
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/ingest.rs`
- Test: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/ingest_tests.rs`

**Interfaces:**
- Consumes: `LogArchive`, `FlushReport` from Task 3.
- Produces: `IngestDeps.archive: Arc<dyn LogArchive>` — Task 5 must supply it.

- [ ] **Step 1: Write the failing tests**

In `ingest_tests.rs`:

```rust
    /// **Archived bytes equal live bytes, prefix included.** The UI's marker
    /// parser was taught to strip `[node] ` in 5538cf972; if the archive stored
    /// an unprefixed line, that parser would need a second rule and a
    /// replayed log would group differently from a live one.
    #[tokio::test]
    async fn an_archived_line_is_byte_identical_to_the_live_one() {
        let fx = fixture().await;
        fx.ingest_log_line("node-a", "PASS | smoke.robot").await;

        assert_eq!(fx.published_lines(), vec!["[node-a] PASS | smoke.robot"]);
        assert_eq!(
            fx.archive_buffered_text(),
            "[node-a] PASS | smoke.robot\n",
            "the archive must hold exactly what subscribers received",
        );
    }

    /// `finish` flushes, so a run that completes normally is durable without
    /// waiting for a tick.
    #[tokio::test]
    async fn finishing_a_run_archives_its_log() {
        let fx = fixture().await;
        fx.ingest_log_line("node-a", "starting").await;
        fx.finish_run_successfully().await;

        assert_eq!(fx.stored_text(), "[node-a] starting\n");
    }

    /// **A cancelled run's tail reaches the row via the tick, not `finish`.**
    /// This is the claim D-RLP-4 rests on: `LogFanout::reap` has four callers
    /// and adding a flush to each was rejected, because the port's own doc
    /// records that this convention has already been got wrong once.
    #[tokio::test]
    async fn a_cancelled_runs_tail_is_archived_by_the_tick() {
        let fx = fixture().await;
        fx.ingest_log_line("node-a", "before the cancel").await;
        fx.cancel_run().await;

        assert_eq!(
            fx.stored_text(),
            "",
            "precondition: retire must not flush by itself",
        );

        let report = fx.archive.flush_due().await;

        assert_eq!(report.runs, 1);
        assert_eq!(fx.stored_text(), "[node-a] before the cancel\n");
    }

    /// A flush failure must not fail the completion. The run's terminal state,
    /// its lease release and its reap are what matter; the archive is
    /// best-effort beside them.
    #[tokio::test]
    async fn a_failed_archive_flush_does_not_fail_the_completion() {
        let fx = fixture().await;
        fx.ingest_log_line("node-a", "starting").await;
        fx.fail_next_append();

        fx.finish_run_successfully().await;

        assert!(
            fx.run_is_terminal().await,
            "the run must still be recorded terminal",
        );
    }
```

- [ ] **Step 2: Run to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs ingest_tests > /tmp/t4.txt 2>&1; echo "exit=$?"
```

Expected: FAIL to compile — `IngestDeps` has no `archive`.

- [ ] **Step 3: Add the dependency**

In `ingest.rs`, add to `IngestDeps` (line 473) and to `IngestService` (line 499):

```rust
    /// Where every fanned-out line is also accumulated for durable storage.
    pub archive: Arc<dyn LogArchive>,
```

and wire it through `IngestService::new`.

- [ ] **Step 4: Record in `fan_out_log`**

Replace the body at `ingest.rs:729-739`:

```rust
    async fn fan_out_log(
        &self,
        ctx: &SecurityContext,
        run_id: Uuid,
        node: &str,
        line: &str,
    ) -> Result<(), DomainError> {
        self.read_run(ctx, run_id).await?;
        let prefixed = format!("[{node}] {line}");
        // The archive is fed the **same string** the subscribers get, so a
        // replayed log is byte-identical to a live one and the UI's marker
        // parser needs no second rule for archived lines.
        self.archive
            .record(ctx.subject_tenant_id(), run_id, &prefixed);
        self.logs.publish(run_id, prefixed);
        Ok(())
    }
```

Extend that method's existing `# The prefix` doc section with a sentence saying
the archive stores the prefixed form for the reason above.

- [ ] **Step 5: Flush in `finish`**

In `finish`, after the transaction commits and beside the existing
`self.logs.reap(run_id)` call:

```rust
        // Best-effort, and after the commit. The recorded terminal state, the
        // lease release and the reap are what this method owes its caller; the
        // archive is beside them. A failure is logged and the text is kept by
        // the buffer for the next tick — the same treatment `reap` gets, and
        // the reason `flush`'s error is not propagated here.
        if let Err(error) = self.archive.flush(run_id).await {
            warn!(run.id = %run_id, %error, "archiving this run's log failed at finish; the tick will retry");
        }
```

- [ ] **Step 6: Run the tests to verify they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs ingest_tests > /tmp/t4.txt 2>&1; echo "exit=$?"
```

Expected: PASS.

- [ ] **Step 7: Break-test both new guarantees**

Change `record` in `fan_out_log` to be passed `line` instead of `&prefixed` and
confirm `an_archived_line_is_byte_identical_to_the_live_one` goes red. Revert.

Change the `finish` flush to `?` instead of logging and confirm
`a_failed_archive_flush_does_not_fail_the_completion` goes red. Revert.

- [ ] **Step 8: Clippy, test, commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo clippy -p qa-runs --all-targets -- -D warnings > /tmp/c4.txt 2>&1; echo "clippy=$?"
cargo test -p qa-runs > /tmp/t4.txt 2>&1; echo "test=$?"
```

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust
git add gears/qa-platform/qa-runs/qa-runs/src/domain/service/
git commit -m "feat(qa-runs): accumulate every ingested log line, flush at finish

The archive gets the same prefixed string the subscribers get, so a replayed log
is byte-identical to a live one. A cancelled run's tail is left to the tick
rather than adding a flush to all four reap sites."
```

---

## Task 5: Wire the archive into the gear and the dispatcher tick

**Files:**
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/gear.rs`

**Interfaces:**
- Consumes: `RunLogArchive::new` from Task 3, `IngestDeps.archive` from Task 4.
- Produces: a single `RunLogArchive` instance shared by `IngestService` and the dispatcher tick.

- [ ] **Step 1: Write the failing composition test**

`gear.rs` already carries composition guards for exactly this hazard — a second
`RunLogBroadcaster::new` in `init` type-checks and silently streams nothing
(`gear.rs:874-928`, `gear.rs:1096-1130`). The archive has the identical hazard:
a second instance would buffer lines that the tick never flushes. Add a test
modelled on the existing one:

```rust
    /// **A second `RunLogArchive::new` in `init` would type-check and would
    /// silently archive nothing** — the ingest service would buffer into one
    /// instance while the dispatcher tick drained another. This is the same
    /// hazard `a_second_broadcaster_would_stream_nothing` guards, and it is
    /// guarded the same way: by reading `init`'s source for a second
    /// construction.
    #[test]
    fn init_constructs_exactly_one_archive() {
        let body = init_source();
        assert_eq!(
            body.matches("RunLogArchive::new").count(),
            1,
            "init must construct the archive once and share the Arc",
        );
    }
```

Follow whatever helper the existing test uses to read `init`'s source
(`gear.rs:1119` asserts on a `body` string — reuse that mechanism).

- [ ] **Step 2: Run to verify it fails**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs init_constructs_exactly_one_archive > /tmp/t5.txt 2>&1; echo "exit=$?"
```

Expected: FAIL — zero matches, nothing constructs it yet.

- [ ] **Step 3: Construct the archive in `init` and pass it to ingest**

Near where `LogWiring` is built (`gear.rs:98-111`) and where `IngestDeps` is
populated (`gear.rs:308`):

```rust
        let archive = Arc::new(RunLogArchive::new(
            db.clone(),
            Arc::clone(&runs_repo),
            policy_enforcer.clone(),
        ));
```

and add `archive: Arc::clone(&archive) as Arc<dyn LogArchive>,` to the
`IngestDeps` literal. Keep the `Arc` in scope — the tick needs it too. Match the
surrounding names for `db`, `runs_repo` and `policy_enforcer` rather than the
guesses above.

- [ ] **Step 4: Flush from the dispatcher tick**

In the dispatcher ticker at `gear.rs:591`, inside the `_ = ticker.tick()` arm,
after `services.dispatch.run_tick().await` and its `debug!`:

```rust
                                // The archive is flushed by the tick because
                                // the process that ingests lines is the process
                                // that ticks: the log publisher is the
                                // `service::watch` observer this tick starts.
                                // This is also what covers the terminal paths
                                // `IngestService::finish` does not — cancel,
                                // TTL expiry, control-plane timeout, orphan
                                // recovery, and a refused launch's abandon.
                                let flushed = archive.flush_due().await;
                                if flushed.runs > 0 || flushed.failed > 0 {
                                    debug!(
                                        runs = flushed.runs,
                                        lines = flushed.lines,
                                        failed = flushed.failed,
                                        "qa-runs log archive flush",
                                    );
                                }
```

The archive `Arc` must be cloned into the ticker's async block the same way
`services` is.

- [ ] **Step 5: Run the composition test and the full suite**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs > /tmp/t5.txt 2>&1; echo "exit=$?"
```

Expected: PASS throughout.

- [ ] **Step 6: Break-test the composition guard**

Add a second `RunLogArchive::new(..)` in `init` bound to `_unused` and confirm
`init_constructs_exactly_one_archive` goes red. Revert.

- [ ] **Step 7: Clippy and commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo clippy -p qa-runs --all-targets -- -D warnings > /tmp/c5.txt 2>&1; echo "clippy=$?"
```

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust
git add gears/qa-platform/qa-runs/qa-runs/src/gear.rs
git commit -m "feat(qa-runs): flush the log archive from the dispatcher tick

One instance, guarded the way the broadcaster's is: a second RunLogArchive::new
in init type-checks and would silently archive nothing."
```

---

## Task 6: Serve the archive, and reproduce the symptom

**Files:**
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/api/rest/handlers/runs.rs:276-288`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/infra/logs/broadcast.rs` (comments only)
- Test: `gears/qa-platform/qa-runs/qa-runs/src/api/rest/handlers/runs_handler_tests.rs`

**Interfaces:**
- Consumes: `RunLogsRepository::get_log` and `ArchivedLog` from Task 2.
- Produces:
  - `RunsService::archived_log(&self, ctx: &SecurityContext, run_id: Uuid) -> Result<Option<ArchivedLog>, DomainError>` — the handler reaches the repository only through this.
  - The endpoint's wire contract is **unchanged**, so no UI change ships.

**Before writing the tests:** `MAX_RETAINED_RUNS` is `pub` in `broadcast.rs:131`
but is **not** re-exported by `infra/logs/mod.rs` (which exports only
`DEFAULT_LOG_CHANNEL_CAPACITY`, `LogSubscription`, `MAX_SUBSCRIBERS_PER_RUN`,
`RunLogBroadcaster` and `gap_marker`). Add it to that re-export list, or the
eviction test cannot name the bound it is driving past — and hard-coding `32`
there would silently stop testing eviction the day the constant changes.

- [ ] **Step 1: Write the two symptom-reproduction tests**

These are the tests that would have caught the reported bug. Neither exists
today.

```rust
    /// **The reported symptom, reproduced.** `authentication-1` showed no lines
    /// because `MAX_RETAINED_RUNS` is 32: once 32 other runs have logged, the
    /// oldest retained log is evicted and the terminal branch had nothing left
    /// to replay. With the archive, the log survives.
    #[tokio::test]
    async fn a_finished_runs_log_survives_broadcaster_eviction() {
        let fx = fixture().await;
        let watched = fx.finished_run_with_log("[node-a] the line that matters").await;

        // Push the watched run out of the retained map.
        for n in 0..=MAX_RETAINED_RUNS {
            fx.finished_run_with_log(&format!("[node-b] filler {n}")).await;
        }
        assert!(
            fx.broadcaster.replay(watched).is_empty(),
            "precondition: the in-memory tail must actually be gone",
        );

        let lines = fx.stream_logs(watched).await;

        assert_eq!(lines, vec!["[node-a] the line that matters"]);
    }

    /// **The other half of the symptom.** The retained map is a process-local
    /// `Mutex<Inner>`, and the gears pod restarted 3 times before reaching
    /// Ready on the first in-cluster deploy. A rebuilt broadcaster must not
    /// cost the log.
    #[tokio::test]
    async fn a_finished_runs_log_survives_a_rebuilt_process() {
        let fx = fixture().await;
        let run_id = fx.finished_run_with_log("[node-a] before the restart").await;

        let fx = fx.rebuild_broadcaster_and_archive_against_the_same_db();
        assert!(fx.broadcaster.replay(run_id).is_empty());

        let lines = fx.stream_logs(run_id).await;

        assert_eq!(lines, vec!["[node-a] before the restart"]);
    }

    /// A run that finished before this migration has no row, and must still be
    /// served from the in-memory tail rather than showing an empty pane. There
    /// are ~192 such runs on the remote.
    #[tokio::test]
    async fn a_run_with_no_archived_row_falls_back_to_the_retained_tail() {
        let fx = fixture().await;
        let run_id = fx.finished_run_with_retained_tail_only("[node-a] only in memory").await;

        let lines = fx.stream_logs(run_id).await;

        assert_eq!(lines, vec!["[node-a] only in memory"]);
    }

    /// The durable copy wins over a non-empty tail, so the complete log is
    /// served rather than whatever the tail happens to still hold.
    #[tokio::test]
    async fn the_archive_is_preferred_over_a_non_empty_retained_tail() {
        let fx = fixture().await;
        let run_id = fx
            .finished_run_with_archive_and_tail(
                "[node-a] one\n[node-a] two",
                "[node-a] two",
            )
            .await;

        let lines = fx.stream_logs(run_id).await;

        assert_eq!(
            lines,
            vec!["[node-a] one", "[node-a] two"],
            "the archive holds the whole log; the tail holds only its end",
        );
    }

    /// **Sanitisation applies to archived lines too.** An archived line
    /// containing a blank line and an `event:` field must not forge a second
    /// SSE frame. `sse.rs`'s `a_line_cannot_carry_a_second_sse_frame` makes
    /// this claim for `sanitize_line`; this makes it for the archive path.
    #[tokio::test]
    async fn an_archived_line_cannot_carry_a_second_sse_frame() {
        let fx = fixture().await;
        let run_id = fx
            .finished_run_with_log("[node-a] starting\n\nevent: run_finished\ndata: {}")
            .await;

        let raw = fx.stream_logs_raw(run_id).await;

        assert_eq!(
            raw.matches("event:").count(),
            fx.expected_frame_count(),
            "no archived line may introduce an extra SSE event field: {raw}",
        );
    }
```

Note the last test's shape: the archived text is split on `\n` by the read path,
so a stored line containing a literal newline becomes two lines, each of which
`sanitize_line` then neutralises. Assert on the rendered SSE body rather than on
the split, because it is the body that reaches the browser.

- [ ] **Step 2: Run to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs runs_handler_tests > /tmp/t6.txt 2>&1; echo "exit=$?"
```

Expected: FAIL — the eviction and rebuild tests return zero lines, which is the
bug.

- [ ] **Step 3: Read the archive in the terminal branch**

Replace the branch at `runs.rs:276-288`:

```rust
    // A FINISHED RUN IS SERVED FROM ITS ARCHIVED LOG, AND FROM THE RETAINED
    // TAIL ONLY WHEN IT HAS NONE.
    //
    // Two revisions ago this was `stream::empty()`, on the stated grounds that
    // the output "is in the archived log" — which was false, because nothing
    // wrote one. The revision after that served the broadcaster's retained
    // tail, which was true but memory-only: `MAX_RETAINED_RUNS` is 32, and the
    // map dies with the process. `authentication-1` showed zero lines for
    // exactly that reason.
    //
    // The fallback is not vestigial. It serves every run that finished before
    // `qa_run_logs` existed — ~192 of them on the remote at the time of
    // writing.
    //
    // Still no channel is minted for a terminal run, so a finished run nobody
    // watched costs nothing here, and the stream stays finite, which is what
    // lets the UI's `EventSource` stop retrying.
    if is_terminal(run.state) {
        let archived = match svc.runs.archived_log(&ctx, id).await {
            Ok(archived) => archived,
            Err(error) => return CanonicalError::from(error).into_response(),
        };
        let lines: Vec<String> = match archived {
            Some(log) => log.text.lines().map(str::to_owned).collect(),
            None => logs.replay(id),
        };
        debug!(
            state = run.state.as_str(),
            lines = lines.len(),
            archived = archived_flag,
            "log requested for a finished run",
        );
        return sse_response(futures::stream::iter(
            lines
                .into_iter()
                .map(|line| Ok::<_, Infallible>(sse_event(&line))),
        ));
    }
```

`svc.runs.archived_log(&ctx, id)` means `RunsService` gains a thin method that
resolves a `qa.run`/`get` scope and calls `RunLogsRepository::get_log`. Add it
next to `RunsService::get`, following that method's scope resolution exactly. Do
not reach the repository directly from the handler — no other handler in this
gear does, and the scope resolution is what keeps absent and foreign
indistinguishable.

`archived_flag` is a `bool` computed before the `match` consumes `archived`;
compute it as `archived.is_some()` and log it, so an operator can tell from the
log line which source answered.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs runs_handler_tests > /tmp/t6.txt 2>&1; echo "exit=$?"
```

Expected: PASS, 5 tests.

- [ ] **Step 5: Break-test the preference and the fallback**

Swap the `match archived` arms so the tail wins, and confirm both
`a_finished_runs_log_survives_broadcaster_eviction` and
`the_archive_is_preferred_over_a_non_empty_retained_tail` go red. Revert.

Then replace the `None => logs.replay(id)` arm with `None => Vec::new()` and
confirm `a_run_with_no_archived_row_falls_back_to_the_retained_tail` goes red.
Revert.

- [ ] **Step 6: Correct the stale comments**

Three places assert that no archive exists. Each says, in substance, "nothing in
this gear writes `log_storage_ref`" — which stays literally true and stops being
the whole truth:

- `infra/logs/broadcast.rs:88-90` — the retention doc's claim that the archived
  log "does not exist".
- `infra/logs/broadcast.rs:275-278` — the same claim in the lagged-subscriber
  doc.
- `api/rest/handlers/runs.rs:264` — replaced wholesale by Step 3.

Rewrite the two in `broadcast.rs` to say that a durable archive now exists in
`qa_run_logs`, that `log_storage_ref` is deliberately still unwritten (design
D-RLP-6: it is published in `RunDto` and shaped like a fetchable URI, so an
internal table reference does not belong in it), and that the retained tail is
now a cache in front of the archive rather than the only copy.

- [ ] **Step 7: Full local gate**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo clippy -p qa-runs --all-targets -- -D warnings > /tmp/c6.txt 2>&1; echo "clippy=$?"
cargo test -p qa-runs > /tmp/t6.txt 2>&1; echo "test=$?"
```

Both must be 0. Read the files — do not pipe.

Also run the Postgres tier, by whatever gate `ingest_races_pg_tests` uses (look
for its `#[cfg(feature = ...)]` or `#[ignore]` attribute and follow it). Items 3,
5 and 7 from the design's §6 are the ones that need a real dialect.

- [ ] **Step 8: Commit**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust
git add gears/qa-platform/qa-runs/qa-runs/src/api/ \
        gears/qa-platform/qa-runs/qa-runs/src/domain/service/runs.rs \
        gears/qa-platform/qa-runs/qa-runs/src/infra/logs/broadcast.rs
git commit -m "feat(qa-runs): serve a finished run's log from qa_run_logs

Reproduces the reported symptom first: a_finished_runs_log_survives_broadcaster_eviction
pushes a run out of the 32-run retained map and asserts the log is still served.
The retained tail stays as the fallback for the ~192 runs that finished before
this table existed."
```

---

## Task 7: Deploy and verify on the remote

**Files:** none. This task is verification only.

**Interfaces:**
- Consumes: everything above.
- Produces: the acceptance evidence.

**The VPN must be up.** If `10.136.20.200` times out, that is the tunnel, not the
code.

- [ ] **Step 1: Confirm the local gate is green before spending 20 minutes**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo clippy -p qa-runs --all-targets -- -D warnings > /tmp/c7.txt 2>&1; echo "clippy=$?"
cargo test -p qa-runs > /tmp/t7.txt 2>&1; echo "test=$?"
```

- [ ] **Step 2: Deploy**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform
./deploy/remote/deploy-k8s.sh <host> > /tmp/deploy.txt 2>&1; echo "exit=$?"
```

A gears change is a full Rust rebuild on the remote — roughly 20 minutes. Note
that `db-migrate` runs as a Helm hook job and used 4 of its 6 attempts on the
first deploy; the new migration adds one statement, so watch that it still lands.

- [ ] **Step 3: Re-run the verification suite**

```bash
./deploy/remote/verify-k8s.sh > /tmp/verify.txt 2>&1; echo "exit=$?"
```

Expected: exit 0, **28 PASS, 0 FAIL, 0 NOTE** — the same numbers as before this
work. Read the file. Do not pipe.

- [ ] **Step 4: Watch a run live**

Launch a run through the UI, open its log pane, and confirm lines appear as they
do today. This is the regression check on the live path: `record` must not have
changed what subscribers see.

- [ ] **Step 5: Confirm the row**

```sql
SELECT run_id, lines, length(text) FROM qa_run_logs ORDER BY updated_at DESC LIMIT 5;
```

Expected: a row for that run, `lines` matching what the pane showed.

- [ ] **Step 6: THE ACCEPTANCE TEST — restart and reload**

```bash
kubectl -n <ns> rollout restart deployment/<gears-deployment>
kubectl -n <ns> rollout status deployment/<gears-deployment> > /tmp/rollout.txt 2>&1; echo "exit=$?"
```

Then reload the finished run's page. **The lines must still be served.** This is
the one thing that is impossible today, and it is the whole point of the change.

- [ ] **Step 7: Confirm the pre-migration fallback**

Open a run that finished before this deploy — `authentication-1` is the reported
one. It must not error. It will show whatever the in-memory tail holds, which
after the restart in Step 6 is nothing; an empty pane with a 200 is the correct
and expected answer for those runs.

- [ ] **Step 8: Update the session prompt and the ledger**

Update `docs/NEXT-SESSION-PROMPT.md`'s status block: the log-persistence item is
done and verified, with the measured evidence from Steps 3-7. Record every ruling
in `.superpowers/sdd/2026-08-31-run-log-persistence/progress.md`, following the
shape of the `2026-08-29-in-cluster-deployment` ledger.

- [ ] **Step 9: Commit the docs**

```bash
cd /home/serhii/Jelastic/projects/fabric/gears-rust
git add gears/qa-platform/docs/NEXT-SESSION-PROMPT.md \
        gears/qa-platform/.superpowers/
git commit -m "docs(qa-platform): record the run-log persistence deploy and its evidence"
```

**Nothing is pushed.** `./push-to-fork.sh` only when the user asks; `origin` stays
forbidden.

---

## After the plan

Two things the design defers to a human, both from §8:

- **The whole-branch review.** This project's habit is a task review after every
  task and a whole-branch review at the end, and the last whole-branch review
  found a defect no per-task review could see. This work spans a migration, a
  repository, a port, a service and a handler — exactly the shape where three
  separately-correct tasks compose into a wrong one.
- **The unpushed backlog.** 38 commits were already unpushed before this work
  started.
