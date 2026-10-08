//! Tests for the tickers' tenant enumeration.
//!
//! Against the **real** repository on in-memory `SQLite`, because the property
//! under test is what a cross-tenant `SELECT DISTINCT` returns and a repository
//! double would be testing the test.
//!
//! # Why there is no `AuthZ` double here any more
//!
//! Before this crate's ticker enumeration moved to `domain::elevated`,
//! [`TenantDirectory::scope`] compiled its scope by asking `PolicyEnforcer`,
//! and this suite carried four doubles (a granting one, a one-tenant one,
//! `TenantScopedAuthZ` and a recording one) to prove first the narrowing and
//! then its absence. [`TenantDirectory`] now holds no enforcer at all, so the
//! read cannot reach one: that property is carried by the type, and a double
//! passed to a constructor that no longer takes it would test nothing. What is
//! left is what the tickers depend on — the enumeration itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_db::DBProvider;
use uuid::Uuid;

use super::TenantDirectory;
use crate::domain::error::DomainError;
use crate::domain::repos::{NewTestResult, ResultsRepository};
use crate::domain::system_actor::TenantBound;
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::test_db::{inmem_db, scope};

/// Two tenants, and the **larger** UUID is deliberately not the interesting one:
/// this test asserts on the whole set, so the ascending-index-order trap Task 33
/// hit (a two-tenant test that passes regardless of its mutation) cannot apply —
/// there is no single row being picked.
const TENANT_A: Uuid = Uuid::from_u128(0x0A);
const TENANT_B: Uuid = Uuid::from_u128(0xB0);

fn row(file: &str) -> NewTestResult {
    NewTestResult {
        test_file: file.to_owned(),
        test_name: "test_a".to_owned(),
        status: "PASSED".to_owned(),
        duration: None,
        launch_id: None,
        jira_key: None,
        product_version: None,
        app_build: None,
        environment_id: None,
        repo_id: None,
        plan_path: None,
        branch: None,
        run_finished_at: Some(OffsetDateTime::now_utc()),
        run_created_at: Some(OffsetDateTime::now_utc()),
    }
}

/// A directory over a database holding one projected run per listed tenant.
async fn directory_over(tenants: &[Uuid]) -> TenantDirectory<OrmResultsRepository> {
    let db = inmem_db().await;
    let conn = db.conn().unwrap();
    for (ordinal, tenant) in tenants.iter().enumerate() {
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(*tenant),
                *tenant,
                Uuid::from_u128(0x100 + ordinal as u128),
                vec![row("tests/a.py")],
                vec![],
            )
            .await
            .unwrap();
    }
    TenantDirectory::new(
        Arc::new(DBProvider::<DomainError>::new(db)),
        OrmResultsRepository,
    )
}

/// The property the three tickers depend on: **every** tenant with a projected
/// row is returned, not just the first, and the ids are the ones that were
/// written.
#[tokio::test]
async fn every_tenant_with_a_projected_row_is_enumerated() {
    let directory = directory_over(&[TENANT_A, TENANT_B]).await;

    let tenants: Vec<Uuid> = directory
        .known_tenants()
        .await
        .expect("the elevated read enumerates")
        .into_iter()
        .map(TenantBound::get)
        .collect();

    // Ascending, which is the repository's documented order.
    assert_eq!(tenants, vec![TENANT_A, TENANT_B]);
}

/// One row per tenant is enough, and several runs for one tenant still yield one
/// entry — the `DISTINCT` is real and is applied in SQL.
#[tokio::test]
async fn a_tenant_with_several_runs_appears_once() {
    let directory = directory_over(&[TENANT_A, TENANT_A, TENANT_A]).await;

    assert_eq!(
        directory.known_tenants().await.unwrap().len(),
        1,
        "three runs for one tenant is one tenant"
    );
}

/// An empty projection enumerates nothing, which is what makes the tickers a
/// no-op on a fresh deployment rather than an error.
#[tokio::test]
async fn a_gear_that_has_projected_nothing_enumerates_nothing() {
    let directory = directory_over(&[]).await;
    assert!(directory.known_tenants().await.unwrap().is_empty());
}
