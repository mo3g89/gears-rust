//! DESIGN §3.3 "Branch model and the first read of a branch" on a real server: two branch-cache writes of one repository that
//! overlap inside Postgres both commit.
//!
//! The race this closes: two first reads of one repository each list the
//! remote and rewrite the cache. The first transaction has inserted `main`
//! and not committed; the second, under READ COMMITTED, cannot see that row,
//! inserts `main` too and waits on the unique index. When the first commits,
//! a plain insert fails on `idx_qa_branches_unique` and the read answered 409.
//! The idempotent diff inserts with `ON CONFLICT DO NOTHING`, so the second
//! finds the row already there and commits.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use qa_catalog_sdk::NewTestRepository;
use toolkit_db::DBProvider;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::OrmTestReposRepository;
use crate::domain::error::DomainError;
use crate::domain::repos::TestReposRepository;
use crate::test_support::{
    TenantScopedAuthZ, all_branch_rows, build_services, ctx, pg_db, seed_product,
};

async fn replace(
    provider: &DBProvider<DomainError>,
    scope: &AccessScope,
    repo_id: Uuid,
    names: &[&str],
) -> Result<(), DomainError> {
    let scope = scope.clone();
    let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                OrmTestReposRepository
                    .replace_branches(tx, &scope, repo_id, names)
                    .await
            })
        })
        .await
}

#[tokio::test]
async fn two_overlapping_branch_cache_replacements_both_commit_on_postgres() {
    let pg = pg_db().await;
    let tenant = Uuid::new_v4();
    let services = build_services(pg.db.clone(), Arc::new(TenantScopedAuthZ));
    let product = seed_product(&services, &ctx(tenant), "product").await;
    let repo = services
        .repos
        .create_repo(
            &ctx(tenant),
            NewTestRepository {
                product_id: product,
                name: "repo".to_owned(),
                url: "https://example.com/org/repo.git".to_owned(),
                default_branch: "main".to_owned(),
                content_root: String::new(),
                credential_ref: None,
            },
        )
        .await
        .unwrap();
    let provider = Arc::new(DBProvider::<DomainError>::new(pg.db.clone()));
    let scope = AccessScope::for_tenants(vec![tenant]);

    // First writer: rewrites the cache, then holds its transaction open.
    let (wrote_tx, wrote_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let first = {
        let provider = Arc::clone(&provider);
        let scope = scope.clone();
        let repo_id = repo.id;
        tokio::spawn(async move {
            provider
                .transaction(move |tx| {
                    Box::pin(async move {
                        OrmTestReposRepository
                            .replace_branches(
                                tx,
                                &scope,
                                repo_id,
                                vec!["main".to_owned(), "26.7".to_owned()],
                            )
                            .await?;
                        wrote_tx.send(()).ok();
                        release_rx.await.ok();
                        Ok(())
                    })
                })
                .await
        })
    };
    wrote_rx.await.unwrap();

    // Second writer: overlaps on `main`, which the first has not committed.
    let second = {
        let provider = Arc::clone(&provider);
        let scope = scope.clone();
        let repo_id = repo.id;
        tokio::spawn(async move { replace(&provider, &scope, repo_id, &["main", "26.8"]).await })
    };
    // Give the second transaction time to reach the insert that waits on the
    // first's uncommitted `main`, then let the first commit.
    tokio::time::sleep(Duration::from_millis(300)).await;
    release_tx.send(()).unwrap();

    first.await.unwrap().expect("the first replacement commits");
    second
        .await
        .unwrap()
        .expect("the second commits too: a name another writer just inserted is already there, not a conflict");

    // What the second writer could not see it leaves alone; the next listing
    // converges the cache onto the remote.
    replace(&provider, &scope, repo.id, &["main", "26.8"])
        .await
        .unwrap();
    assert_eq!(
        all_branch_rows(&pg.db).await,
        vec![
            (repo.id, tenant, "26.8".to_owned()),
            (repo.id, tenant, "main".to_owned()),
        ]
    );
}
