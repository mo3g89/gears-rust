//! Tests for the two flat collections' service layer.
//!
//! Against the **real** repository on in-memory `SQLite` and the real transaction
//! provider, with only the PDP doubled — the same shape as `reconcile_tests`, and
//! for the same reason: the property that matters is that the scope the PDP
//! compiles is the scope the `SELECT` runs under, and a repository double would
//! absorb exactly that.
//!
//! # What is *not* tested here, and where it is
//!
//! The `OData` surface itself — the field allow-list, the page-size clamp, the
//! cursor's exactness, the filter/order split — belongs to
//! `infra::storage::results_sea_repo::tests` and `infra::storage::odata::tests`,
//! because it is the repository's and the field enums'. This module tests only
//! what this layer adds: that a scope is derived per call, from the PDP, under the
//! right resource type and action, and that a denial is a denial before any row is
//! read.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use toolkit_db::DBProvider;
use toolkit_gts::GTS_ID_PREFIX;
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use super::ResultsService;
use crate::domain::error::DomainError;
use crate::domain::repos::{NewTestCaseResult, NewTestResult, ResultsRepository};
use crate::domain::service::test_support::{
    DenyAllAuthZ, RecordingAuthZ, ResourceConstrainedAuthZ, TenantScopedAuthZ, ctx,
};
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::test_db::{inmem_db, scope};

const TENANT: Uuid = Uuid::from_u128(0x0A);
const OTHER_TENANT: Uuid = Uuid::from_u128(0x0B);

struct Fixture {
    db: toolkit_db::Db,
    service: ResultsService<OrmResultsRepository>,
}

impl Fixture {
    async fn with_authz(authz: Arc<dyn AuthZResolverApi>) -> Self {
        let db = inmem_db().await;
        let provider = Arc::new(DBProvider::<DomainError>::new(db.clone()));
        let service =
            ResultsService::new(provider, OrmResultsRepository, PolicyEnforcer::new(authz));
        Self { db, service }
    }

    async fn new() -> Self {
        Self::with_authz(Arc::new(TenantScopedAuthZ)).await
    }

    /// One run's file-level and case-level rows for `tenant`, written through the
    /// real repository under a tenant-only scope — the shape the ingest path
    /// produces.
    async fn seed(&self, tenant: Uuid, run_id: Uuid, rows: usize) {
        let conn = self.db.conn().unwrap();
        let files = (0..rows)
            .map(|n| NewTestResult {
                test_file: format!("tests/t{n}.py"),
                test_name: format!("test_{n}"),
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
                run_finished_at: None,
                run_created_at: None,
            })
            .collect();
        let cases = (0..rows)
            .map(|n| NewTestCaseResult {
                test_file: format!("tests/t{n}.py"),
                nodeid: format!("tests/t{n}.py::test_{n}"),
                name: format!("test_{n}"),
                status: "XPASS".to_owned(),
                duration: None,
                reason: None,
                ticket: None,
            })
            .collect();
        OrmResultsRepository
            .upsert_run_results(&conn, &scope(tenant), tenant, run_id, files, cases)
            .await
            .unwrap();
    }
}

/// Both collections answer with the caller's own tenant's rows and no others.
///
/// The other tenant's rows exist in the same database, so an unscoped read
/// returns five rows rather than two — which is what makes this an isolation test
/// and not a smoke test.
#[tokio::test]
async fn a_page_holds_only_the_callers_tenants_rows() {
    let fx = Fixture::new().await;
    fx.seed(TENANT, Uuid::from_u128(0x80), 2).await;
    fx.seed(OTHER_TENANT, Uuid::from_u128(0x81), 3).await;

    let files = fx
        .service
        .list_results(&ctx(TENANT), &ODataQuery::new())
        .await
        .unwrap();
    assert_eq!(files.items.len(), 2);

    let cases = fx
        .service
        .list_case_results(&ctx(TENANT), &ODataQuery::new())
        .await
        .unwrap();
    assert_eq!(cases.items.len(), 2);

    // The other side of the same property: the other tenant sees its own three.
    let theirs = fx
        .service
        .list_results(&ctx(OTHER_TENANT), &ODataQuery::new())
        .await
        .unwrap();
    assert_eq!(theirs.items.len(), 3);
}

/// A PDP denial is a [`DomainError::Forbidden`] and **no rows**, on both
/// collections.
///
/// The rows exist, so an implementation that read first and authorized afterwards
/// would return them.
#[tokio::test]
async fn a_denied_caller_reads_nothing() {
    let fx = Fixture::with_authz(Arc::new(DenyAllAuthZ)).await;
    fx.seed(TENANT, Uuid::from_u128(0x82), 2).await;

    assert!(matches!(
        fx.service
            .list_results(&ctx(TENANT), &ODataQuery::new())
            .await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        fx.service
            .list_case_results(&ctx(TENANT), &ODataQuery::new())
            .await,
        Err(DomainError::Forbidden)
    ));
}

/// **The action string is a security surface with no other witness.**
///
/// Nothing in this crate fails if `actions::LIST` is silently replaced by
/// `actions::REBUILD`, and no test in this workspace evaluates a real policy — so
/// this is the only place the request this gear actually sends is observable. It
/// is the same argument `test_support::RecordingAuthZ`'s own doc makes for the
/// rebuild.
///
/// Both methods are called, and both must ask for the same pair: one resource
/// type over both tables is `resources::TEST_RESULT`'s documented decision, and a
/// second entry naming something else would mean a deployment had to grant twice
/// for one conceptual permission.
#[tokio::test]
async fn both_collections_authorize_under_test_result_list() {
    let authz = Arc::new(RecordingAuthZ::default());
    let fx = Fixture::with_authz(authz.clone()).await;

    fx.service
        .list_results(&ctx(TENANT), &ODataQuery::new())
        .await
        .unwrap();
    fx.service
        .list_case_results(&ctx(TENANT), &ODataQuery::new())
        .await
        .unwrap();

    let test_result = format!("{GTS_ID_PREFIX}cf.qa.insights.test_result.v1~");
    assert_eq!(
        authz.asked(),
        vec![
            (test_result.clone(), "list".to_owned()),
            (test_result, "list".to_owned()),
        ],
        "a read must ask for cf.qa.insights.test_result.v1~/list, once per call",
    );
}

/// **A scope narrower than the tenant is honoured, not refused** — which is the
/// difference between a read and this gear's projection write.
///
/// [`ResourceConstrainedAuthZ`] compiles `owner_tenant_id IN [tenant]` **and**
/// `resource_id IN [<random>]`, the shape a PDP produces for a row-scoped grant.
/// `refuse_scope_beyond_tenant` rejects it on the write path, for the measured
/// reason recorded there; a read applies it, and the random id matches no stored
/// row, so the honest answer is an **empty page** rather than an error.
///
/// This is the test that would fail if somebody copied the write path's guard
/// into `ResultsService` — it would answer `UnsupportedScope` to a policy the
/// `SELECT` can execute exactly. `domain::service::results`' header states the
/// rule; this pins it.
#[tokio::test]
async fn a_row_scoped_grant_narrows_the_page_instead_of_disabling_it() {
    let fx = Fixture::with_authz(Arc::new(ResourceConstrainedAuthZ)).await;
    fx.seed(TENANT, Uuid::from_u128(0x83), 2).await;

    let page = fx
        .service
        .list_results(&ctx(TENANT), &ODataQuery::new())
        .await
        .expect("a row-scoped grant is a narrower read, not a refusal");
    assert!(
        page.items.is_empty(),
        "the constraint names a random resource_id, so it must match nothing",
    );

    let cases = fx
        .service
        .list_case_results(&ctx(TENANT), &ODataQuery::new())
        .await
        .expect("same on the case collection");
    assert!(cases.items.is_empty());
}

/// The `OData` query reaches the repository untouched, so a caller's `$top`
/// governs the page the service returns.
///
/// A service that had built its own query — or dropped the caller's — would
/// return all three rows here. The cursor's presence is asserted too: it is what
/// tells the caller there is more, and a service that reconstructed the query
/// would lose it.
#[tokio::test]
async fn the_callers_query_is_passed_through() {
    let fx = Fixture::new().await;
    fx.seed(TENANT, Uuid::from_u128(0x84), 3).await;

    let page = fx
        .service
        .list_results(&ctx(TENANT), &ODataQuery::new().with_limit(2))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.page_info.limit, 2);
    assert!(
        page.page_info.next_cursor.is_some(),
        "a truncated page must carry a cursor",
    );
}

/// A `$filter` the caller got wrong is theirs, and it survives the service layer
/// as a [`DomainError::Validation`] rather than being flattened.
///
/// The boundary maps that variant to 400; anything else here would answer 500 to a
/// mistyped field name.
#[tokio::test]
async fn a_bad_filter_stays_the_callers_error_through_the_service() {
    let fx = Fixture::new().await;
    let parsed = toolkit_odata::parse_filter_string("branch eq 'main'").unwrap();
    let query = ODataQuery::new().with_filter(parsed.into_expr());

    let err = fx
        .service
        .list_results(&ctx(TENANT), &query)
        .await
        .expect_err("branch is not in the allow-list");
    assert!(matches!(
        err,
        DomainError::Validation { ref field, .. } if field == "$filter"
    ));
}
