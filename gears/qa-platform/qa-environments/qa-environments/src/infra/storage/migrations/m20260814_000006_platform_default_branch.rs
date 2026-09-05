//! Adds `qa_platforms.default_branch`, the per-platform default branch
//! override.
//!
//! **Added 2026-08-14 by user decision, raised by qa-runs Task 13.** qa-runs
//! resolves a run's branch as `explicit → the target platform's default_branch
//! → the repository's default_branch` (parity spec §3.4 rule 1). The source
//! system has the middle tier — `platforms_meta.default_branch TEXT`
//! (`manager/migrations/001_initial.sql:233`), read by **legacy's**
//! `PlatformsService::get_platform_default_branch`, documented there as *"the
//! per-platform default branch override (falls back to repo default)"*
//! (`manager/src/services/platforms.rs:845-848`). Legacy's "platform" is this
//! gear's `Environment`; the names in that citation are legacy's and do not
//! rename with it. This gear shipped without it. Left alone, a launch naming no
//! branch silently used the *repository's* default where the source system
//! would have used the platform's: no error, no warning, and visible only to an
//! operator who had pinned a platform to a release branch and then wondered why
//! `main` ran.
//!
//! ## Shape borrowed from `m20260813_000004_observed_build`, semantics not
//!
//! The mechanics are the same — a nullable string column, added by a second
//! migration rather than an edit to the initial one, with three per-dialect
//! statements. What differs is who writes it: `observed_build` is
//! machine-written and reaches no write DTO, while this column is
//! **operator-set** and therefore threads through `NewEnvironment`,
//! `EnvironmentPatch` and the REST request type. Copying `observed_build`'s
//! read-only shape by habit would have shipped a column nothing could set.
//!
//! ## A second migration, not an edit to the first
//!
//! `m20260812_000001_initial` must be **assumed** to have run wherever this gear
//! is deployed — it is the gear's initial schema, so any database that has the gear
//! has applied it — and the migration runner records it as applied, so editing it
//! would change nothing on an existing database while silently diverging from what
//! that database contains. (Stated as an assumption rather than a fact: no file in
//! this repository can enumerate deployments, and the safe direction is to assume
//! applied. Contrast the **dated, expiring** exemption further down for `000006`
//! itself, whose branch-local history *was* checkable at the time.) The numbering
//! (`000006`) continues the qa-platform
//! subsystem's shared sequence: `000001` qa-environments, `000002` qa-catalog,
//! `000003` qa-runs, `000004`/`000005` qa-environments.
//!
//! ## No `IF NOT EXISTS` on the `ALTER`
//!
//! For the reason `m20260813_000004_observed_build` records: `SQLite` rejects
//! `ALTER TABLE … ADD COLUMN IF NOT EXISTS` outright, `MySQL`'s grammar has no
//! such clause either, and the migration runner's apply-once guarantee is the
//! one that actually holds. All three dialects are bare rather than one
//! defensive and two not.
//!
//! ## `TEXT` in the source system, `VARCHAR(512)` here — anchored to the sink
//!
//! `platforms_meta.default_branch` is `TEXT` (unbounded) in Postgres, so any
//! bound here is a deliberate divergence and needs a reason.
//!
//! **There is no "string column convention" in this table to appeal to**, and an
//! earlier version of this comment claimed there was — that `VARCHAR(255)`
//! matched *"every other string column in `qa_platforms`"*. That was **false**:
//! `m20260812_000001_initial` declares `description TEXT NULL` (`:20`, `:72`) and
//! `kubeconfig_credstore_ref VARCHAR(1024) NOT NULL` (`:21`, `:73`) alongside the
//! three `VARCHAR(255)` columns. The table mixes 255, 1024 and `TEXT`, so
//! "convention" could have justified any width — which is the tell that it was
//! never the real argument.
//!
//! Worse for the original choice, the closest analogue points the other way:
//! `description` is nullable, operator-set free text, exactly this column's
//! shape, and it is **`TEXT`**.
//!
//! **The reason that does hold is dataflow, not convention.** This value's whole
//! purpose is to be resolved into a run's branch label: `resolve_branch` returns
//! it, `qa-runs/src/domain/service/launch.rs:1695` assigns it as `test_version`,
//! and that lands in `qa_runs.test_version`, declared **`VARCHAR(512)`** in both
//! server blobs of `qa-runs`' `m20260813_000003_initial` (`:214`, `:498`). So:
//!
//! * Unbounded `TEXT` here would let this gear accept an override that makes
//!   **every launch using it fail** on an insert into another gear's narrower
//!   column — a cross-gear break invisible from this side, which is the same
//!   failure class this whole field was added to fix, merely relocated.
//! * `VARCHAR(255)` was **too narrow**: it rejected values both this column and
//!   its downstream sink store perfectly well.
//!
//! **255 was not, however, arbitrary — an earlier version of this comment called
//! it that, and was wrong.** It is the width of **tier 3 of the same rule**:
//! `qa_test_repositories.default_branch VARCHAR(255) NOT NULL` in qa-catalog
//! (`m20260812_000002_initial.rs:81`, `:155`, and `TEXT` at `:235`). So a real
//! sibling argument existed for 255 and simply had not been examined.
//!
//! It loses to the sink anyway, and it is worth being precise about why, since
//! "matching a sibling" and "matching a sink" are both symmetry arguments:
//!
//! * Tier 3's width **does not constrain this column**. They are independently
//!   owned columns on different resources in different gears; a repository's
//!   default branch being capped at 255 says nothing about what a *platform's* may
//!   be. The sink, by contrast, is a hard constraint — every tier's value passes
//!   through it.
//! * The failure modes are asymmetric. Too narrow rejects a legitimate branch name
//!   (a 400 on valid input); too wide would break the sink. At exactly the sink's
//!   width there is **no** downstream break, so extra width up to 512 costs nothing
//!   while any narrowness costs real rejections. 512 is therefore the **maximum**
//!   width that cannot break the sink, and maximality is the tie-breaker — not
//!   "the only width that is neither", which an earlier version claimed and which
//!   is loose, since 256–511 are equally "neither".
//!
//! The bound is checkable (open `m20260813_000003_initial` and compare) and
//! `EnvironmentsService::validate_default_branch` enforces the same 512 in
//! characters, so the failure is a named 400 here rather than a `DbErr` later.
//!
//! ### What this bound does **not** do
//!
//! It does **not** guarantee the sink. `resolve_branch`'s **tier 1** is the
//! caller's explicit `LaunchRequest.branch` (`qa-runs-sdk/src/models.rs:128`),
//! which has **no length validation anywhere** — qa-runs has no `api/` layer yet
//! and no domain bound — so `branch: Some("x".repeat(600))` still reaches
//! `test_version` and still fails the insert. This bound closes the **platform
//! tier only**. The sink's real guarantee would have to live in qa-runs, validating
//! `facts.branch` *after* resolution, where one check covers all three tiers at
//! once; that is tracked for Task 14/16 and is not a qa-environments concern.
//! Keeping this bound is still worth it — a named 400 here beats a `DbErr`
//! there — but it is one tier's worth of protection, not the property.
//!
//! No claim is made about what any git host permits; an earlier version asserted
//! that "every hosting provider caps them far below that", which is an
//! unfalsifiable universal and was not checked.
//!
//! ## Why this file was edited in place rather than superseded — **expired premise**
//!
//! The width above was `VARCHAR(255)` when this migration was first committed
//! (`7a29756a`), and it was corrected in place rather than in a `000007`. That is
//! **not** in tension with the "a second migration, not an edit to the first"
//! doctrine at the top of this file: that doctrine turns on a migration having
//! *already run somewhere*, which is precisely what makes an edit invisible to
//! existing databases.
//!
//! **The licence was time-bounded and has almost certainly lapsed by the time you
//! are reading this.** Verified on **2026-08-14**, on the then-unmerged
//! `feature/qa-platform-specs` branch, which at that point had **no upstream
//! configured**; `000006` was introduced by `7a29756a` and existed on no other
//! branch, so no long-lived database could have applied it and editing it left
//! nothing stale.
//!
//! **That premise expires at merge.** Once this branch lands, treat `000006` as
//! shipped: do **not** edit the width here again, add a `000007` with
//! `ALTER TABLE qa_platforms ALTER COLUMN default_branch TYPE …` instead. If you
//! are here because you want a different width, the branch-state facts above are
//! historical and no longer license anything.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN default_branch VARCHAR(512) NULL;
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN default_branch VARCHAR(512) NULL;
";

const SQLITE_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN default_branch TEXT NULL;
";

/// Pick the statement for a backend.
///
/// Extracted from `up()` **so it can be tested**. Inline in the `match`, nothing
/// could reach it: the gear's tests only ever run `SQLite`, so mapping
/// `Postgres => SQLITE_UP` and `Sqlite => POSTGRES_UP` — a plausible edit, the two
/// arms being adjacent and similar — left every test green while silently giving
/// Postgres deployments a `TEXT` column. That would void this migration's entire
/// width argument on the one dialect the argument is about, so the dispatch is
/// part of the contract and not plumbing.
///
/// Found as an uncovered surface by Task 13b's code-quality review (mutant M7).
const fn sql_for(backend: sea_orm::DatabaseBackend) -> &'static str {
    match backend {
        sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
        sea_orm::DatabaseBackend::MySql => MYSQL_UP,
        sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = sql_for(manager.get_database_backend());
        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("ALTER TABLE qa_platforms DROP COLUMN default_branch;")
            .await?;
        Ok(())
    }
}

/// Schema tests for the added column.
///
/// **`cargo build` proves nothing about a `SeaORM` entity**: its table and
/// column names are runtime strings, so `environment::Model::default_branch`
/// compiling says nothing about whether a column of that name exists. Only a
/// query does. Break-tested by renaming the column in `SQLITE_UP` **alone** —
/// leaving the entity untouched so the crate still compiles — which turns these
/// red along with every other DB-backed test in the gear, since
/// `OrmEnvironmentsRepository::create` names every column in its `INSERT`.
///
/// The second thing these tests exist for is the coverage hole Task 9b found in
/// this very gear: **no DB-backed fixture here ever wrote a non-`NULL`
/// `observed_version`**, so a value dropped on the way out was invisible to all
/// 46 tests then shipped, because a `NULL` survives almost any mistake
/// unchanged. Every round-trip below therefore writes a **non-`NULL`**
/// `default_branch` and asserts it survives the full read path, entity through
/// mapper to SDK model.
///
/// `clippy::disallowed_methods` is allowed for the same narrow reason as in
/// `m20260813_000004_observed_build`: a schema test needs a raw connection and
/// the `SecureORM` wrappers deliberately expose none. Nothing here is a
/// production path.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, EntityTrait};
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    use uuid::Uuid;

    use crate::infra::storage::entity::environment;
    use crate::infra::storage::mapper::environment_to_sdk;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// All three dialect blobs must name the column, and the two server dialects
    /// must agree on its width.
    ///
    /// **Only `SQLITE_UP` is executed by any test in this repository**, so the
    /// Postgres and `MySQL` statements are unexecuted strings that no other
    /// assertion here can reach. That is exactly the gap this subsystem's tooling
    /// has fallen into before: a `perl -0p` rewrite without `/g` patches only the
    /// **first** of three blobs, leaving two dialects silently divergent and every
    /// test green, because the green ones all run on `SQLite`.
    ///
    /// So this test reads the constants as text. It is weak evidence about SQL
    /// correctness and strong evidence about the three blobs staying in step,
    /// which is the failure that has actually happened. `qa-runs`'
    /// `m20260813_000003_initial` pins the same property for its own blobs with a
    /// full column-set comparison; this column's migration is one `ALTER` per
    /// dialect, so a targeted check is enough.
    ///
    /// # What it pins, after the review's mutants
    ///
    /// The first version of this guard let three mutants through, and the fixes are
    /// itemised beside the assertions: the **table name** was never checked at all
    /// (M3), the nullability check was **vacuous** because `"NOT NULL"` contains
    /// `"NULL"` (M4), and a `--` comment could hide a narrowed width from a
    /// non-adjacent `contains` (M5). It does pin `== 512` rather than mere
    /// agreement between the two server blobs, which the review confirmed by moving
    /// both to `1024` together and getting red.
    ///
    /// It deliberately does **not** try to be a SQL parser. `ADD default_branch`
    /// without the optional `COLUMN` keyword is valid in both server dialects and
    /// turns this red — a false positive the review found and judged acceptable,
    /// because the failure it prevents is real and the one it invents is a
    /// one-line test edit away from being fixed.
    /// Strip `--` line comments before matching.
    ///
    /// `contains` does not require adjacency, so `-- was VARCHAR(512)` on one line
    /// makes a `VARCHAR(255)` on the next invisible to a naive width check. That
    /// was mutant M5 and it escaped the first version of this guard. Comments in
    /// migration SQL are ordinary, so stripping them is the fix rather than
    /// forbidding them.
    fn without_comments(sql: &str) -> String {
        sql.lines()
            .map(|line| line.split("--").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Table names this migration must not mention, in two groups.
    ///
    /// The first three are **this gear's other tables**, and they are the dangerous
    /// group precisely because they *exist*: a server migration that alters
    /// `qa_platform_variables` **succeeds**, and only the later queries against
    /// `qa_platforms.default_branch` fail — at runtime, in production, invisibly
    /// from `SQLite`, which is where every executed test here runs.
    ///
    /// `qa_products` is the fourth and belongs to **qa-catalog**, not this gear, so
    /// altering it would fail loudly rather than silently. It is listed anyway
    /// because it is the name the code-quality review's M3 mutant actually used —
    /// plausible exactly because this file was copied from another migration — and
    /// catching a loud failure at test time still beats catching it at deploy time.
    const OTHER_TABLES: [&str; 4] = [
        "qa_platform_variables",
        "qa_pipeline_variables",
        "qa_platform_leases",
        "qa_products",
    ];

    #[test]
    fn every_dialect_blob_alters_qa_platforms_and_pins_its_own_column_type() {
        for (dialect, raw) in [
            ("POSTGRES_UP", super::POSTGRES_UP),
            ("MYSQL_UP", super::MYSQL_UP),
            ("SQLITE_UP", super::SQLITE_UP),
        ] {
            let sql = without_comments(raw);

            // M3. This file was created by copying `m20260813_000004_observed_build`,
            // and "copied the file, forgot the table name" is the standard form of
            // that mistake. The first version of this guard never mentioned the
            // table at all, so altering the wrong one was green.
            assert!(
                sql.contains("qa_platforms"),
                "{dialect} must alter qa_platforms; this file was copied from \
                 another migration, so the table name is exactly what a \
                 copy-paste gets wrong"
            );
            for other in OTHER_TABLES {
                assert!(
                    !sql.contains(other),
                    "{dialect} must not touch {other}: altering a table that \
                     *exists* succeeds on the server and only fails later, when \
                     qa_platforms.default_branch turns out not to be there"
                );
            }

            assert!(
                sql.contains("ADD COLUMN default_branch"),
                "{dialect} must add default_branch; a rewrite that patched only \
                 the first blob is how two dialects drift apart unnoticed"
            );

            // M4. The predecessor of this pair asserted `sql.contains("NULL")`,
            // which `"NOT NULL"` also satisfies — so it could not enforce what its
            // own message claimed, and was vacuous for precisely the two blobs it
            // was written to protect. (A `NOT NULL` in SQLITE_UP would at least
            // fail at runtime: SQLite rejects adding a NOT NULL column with no
            // default to a populated table. The server blobs had nothing.)
            assert!(
                !sql.contains("NOT NULL"),
                "{dialect} must leave default_branch nullable -- 'no override' is \
                 the normal state, and NULL is how EnvironmentPatch's clear is stored"
            );
            assert!(
                sql.contains("NULL"),
                "{dialect} must say NULL explicitly rather than relying on the \
                 dialect's default nullability"
            );
        }

        // The width is load-bearing rather than cosmetic: it matches
        // `qa_runs.test_version`, which is where this value ends up. See the
        // module doc for why the sink is the anchor.
        for (dialect, raw) in [
            ("POSTGRES_UP", super::POSTGRES_UP),
            ("MYSQL_UP", super::MYSQL_UP),
        ] {
            assert!(
                without_comments(raw).contains("VARCHAR(512)"),
                "{dialect} must declare VARCHAR(512), matching the \
                 qa_runs.test_version column this value flows into; found: {raw}"
            );
        }
        assert!(
            without_comments(super::SQLITE_UP).contains("TEXT"),
            "SQLITE_UP declares TEXT, as every string column does in that blob"
        );
    }

    /// Each backend gets a statement of the right **type**, which no executed test
    /// can otherwise reach.
    ///
    /// M7: swapping the `Postgres` and `Sqlite` arms left all 76 tests green while
    /// handing Postgres deployments a `TEXT` column — voiding this migration's
    /// width argument on the very dialect it is about. The guard above pins the
    /// *constants*; without this, nothing pinned which constant each backend gets.
    ///
    /// # What this cannot detect, stated because the obvious phrasing over-claims
    ///
    /// `POSTGRES_UP` and `MYSQL_UP` are **byte-identical**, so swapping *those two*
    /// arms is undetectable here — by any test, at any strength, including
    /// `assert_eq!` against the named constant, which is why this test asserts the
    /// declared type rather than constant identity. That swap is also **harmless**
    /// for exactly the same reason: the two backends would receive the same SQL
    /// they receive now. A test named "each backend gets its own statement" would
    /// therefore be claiming more than it checks; the distinction it really pins is
    /// **server-width versus `SQLite`-`TEXT`**, which is the one that carries
    /// consequences. If the two server blobs ever diverge, this comment stops being
    /// true and the assertions below need tightening to constant identity.
    #[test]
    fn each_backend_gets_a_statement_of_the_right_column_type() {
        use sea_orm::DatabaseBackend;

        for backend in [DatabaseBackend::Postgres, DatabaseBackend::MySql] {
            let sql = without_comments(super::sql_for(backend));
            assert!(
                sql.contains("VARCHAR(512)"),
                "{backend:?} must get a VARCHAR(512) statement, not SQLite's TEXT"
            );
        }

        let sqlite = without_comments(super::sql_for(DatabaseBackend::Sqlite));
        assert!(
            sqlite.contains("TEXT"),
            "SQLite must get the TEXT statement"
        );
        assert!(
            !sqlite.contains("VARCHAR"),
            "and must not get a server blob: the arms are adjacent and similar, \
             which is what makes swapping them a plausible edit"
        );
    }

    /// In-memory `SQLite` with **every** registered migration applied in order,
    /// which is also what proves `Migrator::migrations()` lists the new one: an
    /// unregistered migration leaves the column absent and every assertion
    /// below fails.
    ///
    /// `max_connections(1)` because each `SQLite` `:memory:` connection is its
    /// own database.
    async fn migrated_db() -> DatabaseConnection {
        let mut opts = ConnectOptions::new("sqlite::memory:");
        opts.max_connections(1).min_connections(1);
        let conn = Database::connect(opts)
            .await
            .expect("failed to connect to in-memory sqlite database");
        let manager = SchemaManager::new(&conn);
        // **Every migration except the contract one.** Task 19's
        // `m20260903_000012` drops eight columns, so a test that ran the whole
        // list would assert against a schema its own subject no longer has --
        // while stopping at *this* migration would cut off the later
        // `m20260903_000010` rename these tests' table names depend on. Stop
        // immediately before the drop, which is the last schema state in which
        // the legacy columns and the modern names coexist.
        for migration in super::super::Migrator::migrations() {
            if sea_orm_migration::MigrationName::name(&*migration)
                == "m20260903_000012_drop_legacy_platform_columns"
            {
                break;
            }
            migration
                .up(&manager)
                .await
                .expect("failed to run qa-environments migrations");
        }
        conn
    }

    /// Plant a row with raw SQL.
    ///
    /// **Not the entity.** Task 19's `m20260903_000012` dropped
    /// `kubeconfig_credstore_ref` and `environment::Model` lost the field in
    /// the same commit, so an entity insert omits a `NOT NULL` column that
    /// still exists at this migration's point in history. Reads below still go
    /// through the entity: this migration's own column survives Task 19, so
    /// the model can express it.
    async fn plant(conn: &DatabaseConnection, default_branch: Option<&str>) {
        super::super::legacy_row::plant_environment(
            conn,
            "qa_environments",
            uuid(1),
            uuid(2),
            "staging-a",
            &[
                ("observed_version", super::super::legacy_row::text("5.0.1")),
                ("observed_build", super::super::legacy_row::text("20260813")),
                (
                    "default_branch",
                    super::super::legacy_row::maybe(default_branch),
                ),
            ],
        )
        .await;
    }

    /// One column of the planted row, as text (`NULL` reads as `None`).
    ///
    /// Raw SQL on the read side too: the row is planted with raw SQL (the
    /// entity cannot express `kubeconfig_credstore_ref` any more), and mixing
    /// a raw insert with an entity read makes the test depend on the two
    /// agreeing about how a `Uuid` is bound.
    async fn col(conn: &DatabaseConnection, column: &str) -> Option<String> {
        conn.query_one(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            format!(
                "SELECT {column} FROM qa_environments WHERE id = {};",
                super::super::legacy_row::uuid_lit(uuid(1))
            ),
        ))
        .await
        .unwrap()
        .expect("the environment row must read back")
        .try_get_by_index::<Option<String>>(0)
        .expect("the column must be readable as text")
    }

    /// The column exists under exactly the name the entity uses, and a
    /// **non-`NULL`** value written to it comes back unchanged.
    #[tokio::test]
    async fn default_branch_round_trips_through_the_migrated_schema() {
        let conn = migrated_db().await;

        plant(&conn, Some("release-9.0")).await;

        assert_eq!(
            col(&conn, "default_branch").await.as_deref(),
            Some("release-9.0"),
            "default_branch must survive the round trip; a NULL-only fixture \
             would not have proven this"
        );
        assert_eq!(
            col(&conn, "observed_build").await.as_deref(),
            Some("20260813"),
            "and the columns added before it must be unaffected by the ALTER"
        );
    }

    /// Nullable, because "no override" is the normal case: most platforms take
    /// whatever branch the repository defaults to.
    #[tokio::test]
    async fn default_branch_is_nullable() {
        let conn = migrated_db().await;

        plant(&conn, None).await;

        assert_eq!(col(&conn, "default_branch").await, None);
    }

    /// The column has to reach the **SDK model**, not merely the database —
    /// that is the whole point of adding it, since qa-runs reads
    /// `Environment::default_branch` and nothing else. This is the assertion
    /// Task 9b's coverage hole would otherwise have hidden: dropping the field
    /// in `mapper::environment_to_sdk` (`default_branch: None`) leaves the row
    /// itself perfectly correct.
    #[tokio::test]
    async fn default_branch_reaches_the_sdk_model() {
        let conn = migrated_db().await;

        plant(&conn, Some("release-9.0")).await;

        let stored = environment::Entity::find()
            .one(&conn)
            .await
            .unwrap()
            .expect("the environment row must read back");
        let sdk = environment_to_sdk(stored);
        assert_eq!(sdk.default_branch.as_deref(), Some("release-9.0"));
    }

    /// `down()` has to actually drop the column, or a rollback leaves a schema
    /// the previous release's entity cannot read.
    #[tokio::test]
    async fn the_down_migration_drops_the_column() {
        let conn = migrated_db().await;
        let manager = SchemaManager::new(&conn);

        // The rename in `m20260903_000010_rename_platform_tables` has to come off
        // first: this migration's `down` names `qa_platforms` in raw SQL, which
        // exists only once the rename is reversed. Note this is NOT a full
        // reverse-order rollback -- the migrations between that one and this one
        // are skipped -- and it does not need to be: none of them stands in the
        // way of this `down`, and the rename is the only one that does.
        sea_orm_migration::MigrationTrait::down(
            &super::super::m20260903_000010_rename_platform_tables::Migration,
            &manager,
        )
        .await
        .unwrap();

        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .unwrap();

        conn.execute_unprepared("SELECT default_branch FROM qa_platforms")
            .await
            .expect_err("default_branch must be gone after down()");
        conn.execute_unprepared("SELECT observed_build FROM qa_platforms")
            .await
            .expect("down() must not touch observed_build");
    }
}
