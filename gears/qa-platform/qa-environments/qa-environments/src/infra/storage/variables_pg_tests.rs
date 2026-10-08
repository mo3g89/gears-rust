//! `PUT /qa/v1/variables` on a real server: an upsert whose insert waits on a
//! concurrent insert of the same name, which then commits, still succeeds
//! (DESIGN §3.2 "qa-environments", the `variables` service).
//!
//! The interleaving is forced. A holder transaction inserts the name and stays
//! open; the service's upsert cannot see that row (READ COMMITTED), takes the
//! create path, and its insert waits on `idx_qa_pipeline_vars_unique`. The test
//! waits until Postgres reports that wait in `pg_locks` and only then lets the
//! holder commit. The waiting insert then fails on the unique index, which
//! answered 409 before the service resolved the race.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "a raw SeaORM connection is the only way to read pg_locks: `Db` exposes none"
)]

use std::sync::Arc;
use std::time::Duration;

use qa_environments_sdk::NewVariable;
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use toolkit_db::DBProvider;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::OrmVariablesRepository;
use crate::domain::error::DomainError;
use crate::domain::repos::VariablesRepository;
use crate::test_support::{TenantScopedAuthZ, build_services, ctx, pg_db};

fn var(value: &str) -> NewVariable {
    NewVariable {
        environment_id: None,
        name: "RACED".to_owned(),
        value: value.to_owned(),
    }
}

/// Polls until some backend is waiting on a lock. Bounded: a run without the
/// wait (the insert did not block) gives up after five seconds and lets the
/// assertions below say what happened, instead of hanging.
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
async fn an_upsert_whose_insert_loses_to_a_concurrent_insert_updates_that_row() {
    let pg = pg_db().await;
    let tenant = Uuid::new_v4();
    let services = build_services(pg.db.clone(), Arc::new(TenantScopedAuthZ));
    let provider = Arc::new(DBProvider::<DomainError>::new(pg.db.clone()));

    let (inserted_tx, inserted_rx) = tokio::sync::oneshot::channel::<()>();
    let (commit_tx, commit_rx) = tokio::sync::oneshot::channel::<()>();
    let holder = {
        let provider = Arc::clone(&provider);
        tokio::spawn(async move {
            provider
                .transaction(move |tx| {
                    Box::pin(async move {
                        let scope = AccessScope::for_tenants(vec![tenant]);
                        let row = OrmVariablesRepository
                            .upsert(tx, &scope, tenant, var("first"))
                            .await?;
                        inserted_tx.send(()).ok();
                        commit_rx.await.ok();
                        Ok(row)
                    })
                })
                .await
        })
    };
    inserted_rx.await.unwrap();

    let writer = {
        let services = Arc::clone(&services);
        tokio::spawn(async move { services.variables.upsert(&ctx(tenant), var("second")).await })
    };
    until_a_lock_waits(&pg.url).await;
    commit_tx.send(()).unwrap();

    let first = holder.await.unwrap().expect("the holder commits");
    let second = writer.await.unwrap().expect(
        "the waiting upsert succeeds: the name another request just created is updated, not a 409",
    );
    assert_eq!(second.id, first.id, "one row, the one the holder created");
    assert_eq!(second.value, "second", "the later write wins");
}
