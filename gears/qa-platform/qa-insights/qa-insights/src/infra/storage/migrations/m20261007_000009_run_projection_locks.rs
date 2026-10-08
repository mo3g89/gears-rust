//! `qa_run_projection_locks`: one row per projected run, which every writer of
//! that run's results upserts first, inside its own transaction.
//!
//! # Why
//!
//! `ResultsRepository::upsert_run_results` replaces a run's rows by deleting
//! them and inserting the new batch. Two writers of one run that overlapped —
//! two replicas' reconcile sweeps, or a sweep and an operator rebuild — both
//! committed under READ COMMITTED: the second's `DELETE` could not see the
//! first's uncommitted rows and did not wait for them, so the run kept both
//! batches and every count read from it doubled. Upserting this row first takes
//! a row lock that the other writer waits on until the first commits. Its
//! `DELETE` then runs on a fresh snapshot and removes the first batch. So the
//! later write replaces the earlier one, which is what the reconciler's leader
//! documentation already relied on.
//!
//! The result tables still have no unique index on `(run_id, test_file,
//! test_name)`: one batch may legitimately repeat that tuple, which the initial
//! migration's header (obligation 5) explains.
//!
//! # The cleanup
//!
//! Rows already doubled by the race are removed: for each run, only the newest
//! batch is kept. A batch's rows share the `created_at` the writer stamped once
//! per call, so "a row of the same run with a later `created_at` exists" names
//! exactly the rows of an older batch.
//!
//! On Postgres the comparison is a plain column-to-column `>` on `TIMESTAMPTZ`.
//! **On `SQLite` it is not, because the stored text does not sort as time.** The
//! driver writes the instant as RFC 3339 with only as many fractional digits as
//! it needs (`10:00:00Z`, `10:00:00.5Z`, `10:00:00.512345Z`), and `Z` sorts
//! after every digit, so `10:00:00.5Z` compares *above* the later
//! `10:00:00.512345Z` as text and a text `>` would keep the older batch. The
//! `SQLite` statements compare `julianday(created_at)` instead, which parses that
//! text and orders to the millisecond. (`SQLite` serializes writers, so the race
//! itself cannot leave two batches there; its statements exist so both dialects
//! run one cleanup.)
//!
//! File rows and case rows are cleaned independently: each table keeps its own
//! newest batch per run, judged by its own `created_at`, and neither delete
//! reads the other table.
//!
//! For `qa_test_results`, two batches stamped the same instant are then
//! separated by `ingest_ordinal`, keeping the highest `id` per ordinal. That
//! rule compares the stored values for equality, which is exact on both
//! dialects. `qa_test_case_results` has no ordinal and keeps both batches in
//! that case: two whole reprojections stamped within the same microsecond on
//! Postgres, or the same millisecond on `SQLite`.
//!
//! # `down`
//!
//! Drops the table. The deleted duplicates are not restored: they were the
//! defect.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Postgres. `IF NOT EXISTS` keeps a re-run harmless, and the three deletes
/// find nothing on a second pass.
///
/// **Each delete is linear in the table, not quadratic in a run.** A correlated
/// `EXISTS (… newer.created_at > t.created_at)` pairs every row of a run with
/// every other row of it: measured on Postgres 15 over 20 runs of 6000 rows
/// each, it filtered 360 million pairs and took 10.4 s, against 31 ms for the
/// aggregate join below, and `qa_test_case_results` is the table that grows
/// largest. So the newest stamp per run is computed once (`GROUP BY`) and
/// joined back; the same-instant twins are ranked once with a window function.
/// `max(uuid)` does not exist on Postgres 15, which is why the twin rule ranks
/// by `id` rather than aggregating it. Both keep exactly the rows the `SQLite`
/// statements keep.
const POSTGRES_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_run_projection_locks (
    id UUID PRIMARY KEY NOT NULL,
    tenant_id UUID NOT NULL,
    run_id UUID NOT NULL,
    projected_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_run_projection_locks_run
    ON qa_run_projection_locks(tenant_id, run_id);
DELETE FROM qa_test_results t
 USING (SELECT tenant_id, run_id, max(created_at) AS newest
          FROM qa_test_results GROUP BY tenant_id, run_id) n
 WHERE t.tenant_id = n.tenant_id
   AND t.run_id = n.run_id
   AND t.created_at < n.newest;
DELETE FROM qa_test_results t
 USING (SELECT id, row_number() OVER (
                   PARTITION BY tenant_id, run_id, created_at, ingest_ordinal
                   ORDER BY id DESC) AS rank
          FROM qa_test_results) r
 WHERE t.id = r.id
   AND r.rank > 1;
DELETE FROM qa_test_case_results t
 USING (SELECT tenant_id, run_id, max(created_at) AS newest
          FROM qa_test_case_results GROUP BY tenant_id, run_id) n
 WHERE t.tenant_id = n.tenant_id
   AND t.run_id = n.run_id
   AND t.created_at < n.newest;
";

/// `SQLite`. The same statements with `TEXT` in place of `UUID` and
/// `TIMESTAMPTZ`, matching the initial migration's `SQLITE_UP` for these
/// tables, and with the two "newer batch" comparisons made on
/// `julianday(created_at)` for the reason the module header's "The cleanup"
/// gives. The correlated subqueries name the outer table rather than an alias
/// because `SQLite`'s `DELETE` takes no table alias.
const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS qa_run_projection_locks (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    projected_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_qa_run_projection_locks_run
    ON qa_run_projection_locks(tenant_id, run_id);
DELETE FROM qa_test_results WHERE EXISTS (
    SELECT 1 FROM qa_test_results newer
     WHERE newer.tenant_id = qa_test_results.tenant_id
       AND newer.run_id = qa_test_results.run_id
       AND julianday(newer.created_at) > julianday(qa_test_results.created_at));
DELETE FROM qa_test_results WHERE EXISTS (
    SELECT 1 FROM qa_test_results twin
     WHERE twin.tenant_id = qa_test_results.tenant_id
       AND twin.run_id = qa_test_results.run_id
       AND twin.created_at = qa_test_results.created_at
       AND twin.ingest_ordinal = qa_test_results.ingest_ordinal
       AND twin.id > qa_test_results.id);
DELETE FROM qa_test_case_results WHERE EXISTS (
    SELECT 1 FROM qa_test_case_results newer
     WHERE newer.tenant_id = qa_test_case_results.tenant_id
       AND newer.run_id = qa_test_case_results.run_id
       AND julianday(newer.created_at) > julianday(qa_test_case_results.created_at));
";

/// Both dialects. The index goes with its table.
const POSTGRES_DOWN: &str = "DROP TABLE IF EXISTS qa_run_projection_locks;";
const SQLITE_DOWN: &str = "DROP TABLE IF EXISTS qa_run_projection_locks;";

/// The DDL for `backend`, or an error naming why there is none.
///
/// Same shape and the same refusal as every earlier migration's `ddl`.
fn ddl(
    backend: sea_orm::DatabaseBackend,
    postgres: &'static str,
    sqlite: &'static str,
) -> Result<&'static str, DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(postgres),
        sea_orm::DatabaseBackend::Sqlite => Ok(sqlite),
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
        let backend = manager.get_database_backend();
        manager
            .get_connection()
            .execute_unprepared(ddl(backend, POSTGRES_UP, SQLITE_UP)?)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        manager
            .get_connection()
            .execute_unprepared(ddl(backend, POSTGRES_DOWN, SQLITE_DOWN)?)
            .await?;
        Ok(())
    }
}

/// Every name above is a runtime string, so `cargo build` is no evidence about
/// this file; the evidence is statements against a real engine, over rows
/// written the way the writer writes them.
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
    use sea_orm::{ActiveModelTrait, ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
    use sea_orm_migration::sea_orm::{
        ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement,
    };
    use sea_orm_migration::{MigrationName as _, MigratorTrait as _};
    use time::OffsetDateTime;
    use time::macros::datetime;
    use uuid::Uuid;

    use super::super::Migrator;
    use crate::infra::storage::entity::{run_projection_lock, test_case_result, test_result};

    /// How many migrations stand in front of this one. A literal, for the
    /// reason `m20260929_000007`'s constant of the same name gives.
    const MIGRATIONS_BEFORE_THIS_ONE: u32 = 8;

    const TENANT: Uuid = Uuid::from_u128(0xA);
    const RUN: Uuid = Uuid::from_u128(0x1);
    const OTHER_RUN: Uuid = Uuid::from_u128(0x2);
    const ROWS_PER_BATCH: u128 = 3;

    /// A batch's stamp, with sub-second digits: the writer stamps
    /// `OffsetDateTime::now_utc()`, and a cleanup that only orders whole
    /// seconds correctly would pass a whole-second fixture.
    fn at_a() -> OffsetDateTime {
        datetime!(2026-10-07 10:00:00.5 UTC)
    }

    /// Later than [`at_a`] by less than a second, and by more digits.
    fn at_b() -> OffsetDateTime {
        datetime!(2026-10-07 10:00:00.512345 UTC)
    }

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

    async fn upgrade(conn: &DatabaseConnection) {
        Migrator::up(conn, None)
            .await
            .expect("the rest of the chain applies");
    }

    /// One `qa_test_results` row, written through the entity the way the
    /// writer writes it. `id` from `id_seed` so a test can name which row
    /// survived.
    async fn file_row(
        conn: &DatabaseConnection,
        id_seed: u128,
        run_id: Uuid,
        ordinal: i32,
        created_at: OffsetDateTime,
    ) -> Uuid {
        let id = Uuid::from_u128(id_seed);
        test_result::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(TENANT),
            run_id: ActiveValue::Set(run_id),
            test_file: ActiveValue::Set(format!("tests/t{ordinal}.py")),
            test_name: ActiveValue::Set(format!("test_{ordinal}")),
            status: ActiveValue::Set("PASSED".to_owned()),
            duration: ActiveValue::Set(None),
            launch_id: ActiveValue::Set(None),
            jira_key: ActiveValue::Set(None),
            product_version: ActiveValue::Set(None),
            app_build: ActiveValue::Set(None),
            environment_id: ActiveValue::Set(None),
            repo_id: ActiveValue::Set(None),
            plan_path: ActiveValue::Set(None),
            branch: ActiveValue::Set(None),
            run_finished_at: ActiveValue::Set(None),
            run_created_at: ActiveValue::Set(None),
            ingest_ordinal: ActiveValue::Set(ordinal),
            created_at: ActiveValue::Set(created_at),
            updated_at: ActiveValue::Set(created_at),
        }
        .insert(conn)
        .await
        .expect("a file row inserts");
        id
    }

    async fn case_row(
        conn: &DatabaseConnection,
        id_seed: u128,
        run_id: Uuid,
        created_at: OffsetDateTime,
    ) -> Uuid {
        let id = Uuid::from_u128(id_seed);
        test_case_result::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(TENANT),
            run_id: ActiveValue::Set(run_id),
            test_file: ActiveValue::Set("tests/t.py".to_owned()),
            nodeid: ActiveValue::Set(format!("tests/t.py::case_{id_seed}")),
            name: ActiveValue::Set(format!("case_{id_seed}")),
            status: ActiveValue::Set("PASSED".to_owned()),
            duration: ActiveValue::Set(None),
            reason: ActiveValue::Set(None),
            ticket: ActiveValue::Set(None),
            created_at: ActiveValue::Set(created_at),
            updated_at: ActiveValue::Set(created_at),
        }
        .insert(conn)
        .await
        .expect("a case row inserts");
        id
    }

    /// One whole batch of a run: [`ROWS_PER_BATCH`] file rows and as many case
    /// rows, all stamped `at`, ids from `seed`. Returns the file and case ids.
    async fn batch(
        conn: &DatabaseConnection,
        seed: u128,
        run_id: Uuid,
        at: OffsetDateTime,
    ) -> (Vec<Uuid>, Vec<Uuid>) {
        let mut files = Vec::new();
        let mut cases = Vec::new();
        for i in 0..ROWS_PER_BATCH {
            let ordinal = i32::try_from(i).expect("a small ordinal");
            files.push(file_row(conn, seed + i, run_id, ordinal, at).await);
            cases.push(case_row(conn, seed + 0x100 + i, run_id, at).await);
        }
        (files, cases)
    }

    async fn sorted_file_ids(conn: &DatabaseConnection, run_id: Uuid) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = test_result::Entity::find()
            .filter(test_result::Column::RunId.eq(run_id))
            .all(conn)
            .await
            .expect("file rows read")
            .into_iter()
            .map(|r| r.id)
            .collect();
        ids.sort();
        ids
    }

    async fn sorted_case_ids(conn: &DatabaseConnection, run_id: Uuid) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = test_case_result::Entity::find()
            .filter(test_case_result::Column::RunId.eq(run_id))
            .all(conn)
            .await
            .expect("case rows read")
            .into_iter()
            .map(|r| r.id)
            .collect();
        ids.sort();
        ids
    }

    fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
        ids.sort();
        ids
    }

    /// Seeds two batches of one run and one batch of another, the state the
    /// race left behind, and returns what must survive.
    async fn seed_a_doubled_run(
        conn: &DatabaseConnection,
    ) -> ((Vec<Uuid>, Vec<Uuid>), (Vec<Uuid>, Vec<Uuid>)) {
        // Batch B is written first, so a cleanup that kept the first-inserted
        // rows (or the lowest ids) instead of the newest stamp keeps A.
        let newest = batch(conn, 0x2000, RUN, at_b()).await;
        let _older = batch(conn, 0x1000, RUN, at_a()).await;
        let other = batch(conn, 0x3000, OTHER_RUN, at_a()).await;
        (newest, other)
    }

    #[tokio::test]
    async fn a_run_projected_twice_keeps_only_its_newest_batch() {
        let conn = db_before_the_upgrade().await;
        let ((newest_files, newest_cases), (other_files, other_cases)) =
            seed_a_doubled_run(&conn).await;

        upgrade(&conn).await;

        assert_eq!(
            sorted_file_ids(&conn, RUN).await,
            sorted(newest_files),
            "only the newest batch's file rows are left of the doubled run"
        );
        assert_eq!(
            sorted_case_ids(&conn, RUN).await,
            sorted(newest_cases),
            "only the newest batch's case rows are left of the doubled run"
        );
        assert_eq!(
            sorted_file_ids(&conn, OTHER_RUN).await,
            sorted(other_files),
            "a run stored once is untouched"
        );
        assert_eq!(
            sorted_case_ids(&conn, OTHER_RUN).await,
            sorted(other_cases),
            "a run stored once is untouched"
        );
    }

    /// Two file rows of one run with the same stamp and the same ordinal can
    /// only be two batches written at one instant; the higher `id` is kept. Two
    /// rows of one batch with *different* ordinals are both kept: a batch may
    /// repeat a `(test_file, test_name)` tuple, but never an ordinal.
    #[tokio::test]
    async fn a_batch_written_twice_at_one_instant_is_deduplicated_by_ordinal() {
        let conn = db_before_the_upgrade().await;
        let low = file_row(&conn, 0x10, RUN, 0, at_a()).await;
        let high = file_row(&conn, 0x20, RUN, 0, at_a()).await;
        let sibling = file_row(&conn, 0x05, RUN, 1, at_a()).await;
        assert!(high > low, "the fixture's ids order as the test expects");

        upgrade(&conn).await;

        assert_eq!(
            sorted_file_ids(&conn, RUN).await,
            sorted(vec![high, sibling]),
            "the twin with the lower id goes, the row with its own ordinal stays"
        );
    }

    #[tokio::test]
    async fn the_lock_table_is_unique_per_run() {
        let conn = db_before_the_upgrade().await;
        upgrade(&conn).await;
        let lock = |id: u128| run_projection_lock::ActiveModel {
            id: ActiveValue::Set(Uuid::from_u128(id)),
            tenant_id: ActiveValue::Set(TENANT),
            run_id: ActiveValue::Set(RUN),
            projected_at: ActiveValue::Set(at_a()),
        };
        lock(1).insert(&conn).await.expect("the first lock row");
        assert!(
            lock(2).insert(&conn).await.is_err(),
            "a second lock row for one (tenant_id, run_id) must be refused"
        );
        run_projection_lock::ActiveModel {
            run_id: ActiveValue::Set(OTHER_RUN),
            ..lock(3)
        }
        .insert(&conn)
        .await
        .expect("another run has its own lock row");
    }

    async fn tables(conn: &DatabaseConnection) -> Vec<String> {
        conn.query_all_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
        ))
        .await
        .expect("sqlite_master")
        .iter()
        .map(|r| r.try_get::<String>("", "name").expect("name"))
        .collect()
    }

    #[tokio::test]
    async fn the_down_migration_drops_exactly_the_lock_table() {
        let conn = db_before_the_upgrade().await;
        upgrade(&conn).await;
        let before = tables(&conn).await;
        assert!(before.iter().any(|t| t == "qa_run_projection_locks"));

        let manager = sea_orm_migration::SchemaManager::new(&conn);
        sea_orm_migration::MigrationTrait::down(&super::Migration, &manager)
            .await
            .expect("down applies");

        let mut expected = before;
        expected.retain(|t| t != "qa_run_projection_locks");
        assert_eq!(
            tables(&conn).await,
            expected,
            "down must drop the lock table and nothing else"
        );
    }

    /// A migration missing from `Migrator::migrations()` runs on no deployment
    /// at all, and [`MIGRATIONS_BEFORE_THIS_ONE`] must still name its position,
    /// or `db_before_the_upgrade` builds the wrong revision.
    #[test]
    fn the_migration_is_registered_in_the_chain() {
        let registered: Vec<String> = Migrator::migrations()
            .iter()
            .map(|m| m.name().to_owned())
            .collect();
        assert_eq!(
            registered
                .get(MIGRATIONS_BEFORE_THIS_ONE as usize)
                .map(String::as_str),
            Some(super::Migration.name()),
            "the projection-lock migration is not at its position in the applied chain; \
             registered: {registered:?}"
        );
    }

    #[test]
    fn mysql_is_refused_by_name() {
        assert!(
            super::ddl(sea_orm::DatabaseBackend::MySql, "", "").is_err(),
            "qa-insights has no MySQL schema"
        );
    }

    /// **The cleanup means on Postgres what it means on `SQLite`.** `TIMESTAMPTZ`
    /// ordering and `UUID` `>` are what the text relies on there.
    ///
    /// This drives this migration's own `up` again over rows written after the
    /// whole chain, not an upgrade from the revision before it, for the reason
    /// `m20261007_000008_bare_credstore_refs`' Postgres test gives; the
    /// `IF NOT EXISTS` on the table and index is what makes the second run of
    /// its DDL harmless.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn the_cleanup_runs_on_real_postgres() {
        let harness = crate::infra::storage::test_db::pg_db().await;
        // A raw SeaORM connection alongside the toolkit-db pool: `Db` exposes
        // none, as `m20260818_000001_initial`'s Postgres test also notes.
        let pg = Database::connect(&harness.url)
            .await
            .expect("failed to open a raw connection to the container");
        let ((newest_files, newest_cases), (other_files, other_cases)) =
            seed_a_doubled_run(&pg).await;
        let low = file_row(&pg, 0x10, Uuid::from_u128(0x3), 0, at_a()).await;
        let high = file_row(&pg, 0x20, Uuid::from_u128(0x3), 0, at_a()).await;
        assert!(high > low);

        let manager = sea_orm_migration::SchemaManager::new(&pg);
        sea_orm_migration::MigrationTrait::up(&super::Migration, &manager)
            .await
            .expect("the migration applies on Postgres");

        assert_eq!(sorted_file_ids(&pg, RUN).await, sorted(newest_files));
        assert_eq!(sorted_case_ids(&pg, RUN).await, sorted(newest_cases));
        assert_eq!(sorted_file_ids(&pg, OTHER_RUN).await, sorted(other_files));
        assert_eq!(sorted_case_ids(&pg, OTHER_RUN).await, sorted(other_cases));
        assert_eq!(
            sorted_file_ids(&pg, Uuid::from_u128(0x3)).await,
            vec![high],
            "the twin with the lower id goes on Postgres too"
        );
    }
}
