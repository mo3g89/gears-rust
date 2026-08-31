//! What the provider's **default** transaction does and does not serialize, on
//! the shipped dialect.
//!
//! This module makes no claim about `domain::service::ingest`. It characterises
//! the primitive ingest is built on — `DBProvider::transaction`, which sets no
//! `TxConfig` and so inherits Postgres's `READ COMMITTED` — because the premise
//! of the whole race analysis is a statement about that primitive that no
//! `SQLite` test in this crate can check.
//!
//! # Why the sibling `SQLite` tests cannot cover this
//!
//! `test_db::inmem_db` admits one writer at a time. Two transactions that
//! interleave a read and a write cannot interleave there at all, so the
//! behaviour below is unobservable with a fix or without one.
//!
//! # The interleaving is forced, not raced
//!
//! Both transactions park on a `tokio::sync::Barrier` *between* their read and
//! their write, so the schedule is deterministic rather than likely: the
//! assertion cannot pass because a scheduler happened to be kind, and cannot
//! flake because it happened not to be.

use std::sync::Arc;

use toolkit_db::DBProvider;
use uuid::Uuid;

use super::OrmRunsRepository;
use super::test_db::{pg_db, sample_new_run, scope};
use crate::domain::error::DomainError;
use crate::domain::repos::{NewTestResult, RunResultDelta, RunsRepository};

fn result_for(status: &str) -> NewTestResult {
    NewTestResult {
        test_file: "tests/test_smoke.py".to_owned(),
        test_name: "test_login".to_owned(),
        status: status.to_owned(),
        duration: None,
        launch_id: None,
        jira_key: None,
        nodeid: None,
        reason: None,
        ticket: None,
    }
}

/// Two transactions each read "is there a row for this test yet?", each see
/// `None`, and each write — and the default isolation lets **both** commit.
///
/// This is the mechanism behind two of the three consequences the ingest path
/// carries, observed where they originate rather than inferred:
///
/// * the per-test tuple ends up stored **twice**, because uniqueness on
///   `(tenant_id, run_id, test_file, test_name)` is an application invariant
///   held by delete-then-insert inside one transaction, and two transactions
///   are not one transaction; and
/// * `total` reaches **2** for one logical test, because each transaction
///   computed its `+1` from a `previous` read before the other's insert
///   existed. The increment itself is a server-side `col = col + n` and is not
///   at fault — the read above it is.
///
/// The verdict path is not exercised here; that is
/// `domain::service::ingest_races_pg_tests`' subject.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_default_transaction_lets_two_readers_of_one_tuple_both_write() {
    let harness = pg_db().await;
    let db = Arc::new(DBProvider::<DomainError>::new(harness.db.clone()));
    let tenant = Uuid::from_u128(0xA1);
    let scope = scope(tenant);
    let repo = Arc::new(OrmRunsRepository);

    let run = {
        let conn = db.conn().unwrap();
        repo.create(&conn, &scope, tenant, sample_new_run("smoke-1"))
            .await
            .expect("the run inserts")
    };

    // Released only once both transactions have taken their read, so the write
    // half of each lands after the read half of both.
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let mut handles = Vec::with_capacity(2);
    for _ in 0..2 {
        let db = Arc::clone(&db);
        let repo = Arc::clone(&repo);
        let scope = scope.clone();
        let barrier = Arc::clone(&barrier);
        let run_id = run.id;
        handles.push(tokio::spawn(async move {
            db.transaction(move |tx| {
                Box::pin(async move {
                    let owned = repo.resolve_owned(tx, &scope, run_id).await?;

                    // The read `record_one_result` takes to decide its delta.
                    let previous = repo
                        .list_test_results(tx, &scope, owned)
                        .await?
                        .into_iter()
                        .find(|row| row.test_name == "test_login")
                        .map(|row| row.status);

                    barrier.wait().await;

                    repo.upsert_test_result(tx, &scope, tenant, owned, result_for("PASSED"))
                        .await?;
                    if previous.is_none() {
                        repo.add_result_counts(
                            tx,
                            &scope,
                            run_id,
                            RunResultDelta {
                                passed: 1,
                                total: 1,
                                ..Default::default()
                            },
                        )
                        .await?;
                    }
                    Ok(previous)
                })
            })
            .await
        }));
    }

    let mut committed = 0_usize;
    for handle in handles {
        if handle.await.expect("task joins").is_ok() {
            committed += 1;
        }
    }

    // Nothing aborted: `READ COMMITTED` has no conflict to detect here.
    assert_eq!(
        committed, 2,
        "the default isolation aborts neither writer; if this is 1, the \
         provider's default transaction is no longer READ COMMITTED and the \
         race analysis in domain::service::ingest needs re-deriving",
    );

    let conn = db.conn().unwrap();
    let owned = repo.resolve_owned(&conn, &scope, run.id).await.unwrap();
    let rows = repo.list_test_results(&conn, &scope, owned).await.unwrap();
    let stored = repo
        .get_result(&conn, &scope, run.id)
        .await
        .unwrap()
        .expect("the run's counter row is readable");

    assert_eq!(
        rows.len(),
        2,
        "the per-test tuple is stored twice: there is no unique index on \
         (tenant_id, run_id, test_file, test_name) and delete-then-insert only \
         dedupes within one transaction",
    );
    assert_eq!(
        stored.total, 2,
        "one logical test counted twice: both deltas were computed from a \
         `previous` read before either insert was visible",
    );
    assert_eq!(stored.passed, 2, "the passed counter skews with total");
}
