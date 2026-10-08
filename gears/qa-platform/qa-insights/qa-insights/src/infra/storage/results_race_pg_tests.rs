//! Two reprojections of one run that overlap leave **one** batch of rows, not
//! two (DESIGN §3.8 "qa-insights schema", `qa_run_projection_locks`).
//!
//! Before the per-run lock, the second transaction's `DELETE` could not see the
//! first's uncommitted inserts, and under READ COMMITTED it did not wait for
//! them, so both batches committed and every count read from the run doubled.
//! The test holds the first transaction open after its write, starts the second,
//! waits until Postgres reports a lock wait (bounded; without the lock no wait
//! appears), then lets the first commit.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "a raw SeaORM connection is the only way to read pg_locks: `Db` exposes none"
)]

use std::sync::Arc;
use std::time::Duration;

use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use toolkit_db::DBProvider;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{NewTestCaseResult, NewTestResult, ResultsRepository};
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::test_db::{now, pg_db, scope};

const FILES_PER_BATCH: usize = 3;

fn batch(tag: &str) -> (Vec<NewTestResult>, Vec<NewTestCaseResult>) {
    let files = (0..FILES_PER_BATCH)
        .map(|i| NewTestResult {
            test_file: format!("tests/t{i}.py"),
            test_name: format!("test_{i}"),
            status: "PASSED".to_owned(),
            duration: None,
            launch_id: None,
            jira_key: None,
            product_version: Some(tag.to_owned()),
            app_build: None,
            environment_id: None,
            repo_id: None,
            plan_path: None,
            branch: None,
            run_finished_at: Some(now()),
            run_created_at: Some(now()),
        })
        .collect();
    let cases = (0..FILES_PER_BATCH)
        .map(|i| NewTestCaseResult {
            test_file: format!("tests/t{i}.py"),
            nodeid: format!("tests/t{i}.py::test_{i}"),
            name: format!("test_{i}"),
            status: "PASSED".to_owned(),
            duration: None,
            reason: Some(tag.to_owned()),
            ticket: None,
        })
        .collect();
    (files, cases)
}

/// Polls until some backend is waiting on a lock. Bounded: without the per-run
/// lock the second writer does not block, and after five seconds the test goes
/// on and lets the assertions below say what happened, instead of hanging.
async fn until_a_lock_waits(url: &str) {
    let raw = Database::connect(url).await.expect("a raw connection");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let row = raw
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                "SELECT count(*)::bigint AS n FROM pg_locks WHERE NOT granted",
            ))
            .await
            .expect("pg_locks is readable")
            .expect("count returns a row");
        if row.try_get::<i64>("", "n").expect("n") > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_overlapping_reprojections_of_one_run_leave_one_batch() {
    let harness = pg_db().await;
    let provider = Arc::new(DBProvider::<DomainError>::new(harness.db.clone()));
    let tenant = Uuid::from_u128(0xA);
    let run_id = Uuid::from_u128(0x77);

    let (wrote_tx, wrote_rx) = tokio::sync::oneshot::channel::<()>();
    let (commit_tx, commit_rx) = tokio::sync::oneshot::channel::<()>();
    let first = {
        let provider = Arc::clone(&provider);
        tokio::spawn(async move {
            provider
                .transaction(move |tx| {
                    Box::pin(async move {
                        let (files, cases) = batch("first");
                        OrmResultsRepository
                            .upsert_run_results(tx, &scope(tenant), tenant, run_id, files, cases)
                            .await?;
                        wrote_tx.send(()).ok();
                        commit_rx.await.ok();
                        Ok(())
                    })
                })
                .await
        })
    };
    wrote_rx.await.unwrap();

    let second = {
        let provider = Arc::clone(&provider);
        tokio::spawn(async move {
            provider
                .transaction(move |tx| {
                    Box::pin(async move {
                        let (files, cases) = batch("second");
                        OrmResultsRepository
                            .upsert_run_results(tx, &scope(tenant), tenant, run_id, files, cases)
                            .await
                    })
                })
                .await
        })
    };
    until_a_lock_waits(&harness.url).await;
    commit_tx.send(()).unwrap();
    first
        .await
        .unwrap()
        .expect("the first reprojection commits");
    second.await.unwrap().expect("the second commits too");

    let conn = harness.db.conn().unwrap();
    let files = OrmResultsRepository
        .list_by_run(&conn, &scope(tenant), run_id)
        .await
        .unwrap();
    assert_eq!(
        files.len(),
        FILES_PER_BATCH,
        "one batch of file rows, not two: {files:?}"
    );
    assert!(
        files
            .iter()
            .all(|r| r.product_version.as_deref() == Some("second")),
        "the later reprojection replaced the earlier one: {files:?}"
    );
    let cases = OrmResultsRepository
        .case_rows_for_run(&conn, &scope(tenant), run_id)
        .await
        .unwrap();
    assert_eq!(
        cases.len(),
        FILES_PER_BATCH,
        "one batch of case rows, not two: {cases:?}"
    );
}
