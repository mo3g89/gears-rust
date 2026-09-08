//! Tests for the tickers' tenant enumeration.
//!
//! Against the **real** repository on in-memory `SQLite`, because the property
//! under test is what a cross-tenant `SELECT DISTINCT` returns and a repository
//! double would be testing the test.
//!
//! # Why every `AuthZ` double here is now beside the point, and is kept anyway
//!
//! Before this crate's ticker enumeration moved to `domain::elevated`,
//! [`TenantDirectory::scope`] compiled its scope by asking `PolicyEnforcer`,
//! and the doubles below modelled the two deployments that mattered:
//! `TenantScopedAuthZ` (no constraint for a nil-tenant actor, so
//! `ConstraintsRequiredButAbsent` — exactly what the shipped
//! `static-authz-plugin` does) and [`CrossTenantAuthZ`] (an explicit granting
//! policy). Neither is consulted any more: `scope` reaches for
//! `domain::elevated::enumeration_scope` directly. The doubles are kept, and
//! wired into every test below through `directory_over`, for exactly that
//! reason — a suite that stopped passing an `AuthZ` double at all would prove
//! nothing about whether the code *still* ignores one; a suite that keeps
//! passing deliberately hostile and deliberately narrow doubles and watches
//! the read succeed and stay unnarrowed either way is the live regression
//! guard. [`RecordingAuthZ`] and
//! [`ticker_enumeration_does_not_consult_the_policy_engine`] pin the property
//! directly: zero requests reach `evaluate` for this read, under any double.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::error::AuthZResolverError;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_security::pep_properties;
use uuid::Uuid;

use super::TenantDirectory;
use crate::domain::error::DomainError;
use crate::domain::repos::{NewTestResult, ResultsRepository};
use crate::domain::service::test_support::TenantScopedAuthZ;
use crate::domain::system_actor::TenantBound;
use crate::infra::storage::results_sea_repo::OrmResultsRepository;
use crate::infra::storage::test_db::{inmem_db, scope};
use toolkit_security::PlatformSecurityContext;
use toolkit_canonical_errors::CanonicalError;

/// Two tenants, and the **larger** UUID is deliberately not the interesting one:
/// this test asserts on the whole set, so the ascending-index-order trap Task 33
/// hit (a two-tenant test that passes regardless of its mutation) cannot apply —
/// there is no single row being picked.
const TENANT_A: Uuid = Uuid::from_u128(0x0A);
const TENANT_B: Uuid = Uuid::from_u128(0xB0);

/// Grants a cross-tenant `owner_tenant_id IN [TENANT_A, TENANT_B]` — a
/// deployment whose policy lets this gear's system actor enumerate.
struct CrossTenantAuthZ;

#[async_trait]
impl AuthZResolverApi for CrossTenantAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [TENANT_A, TENANT_B],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

/// Grants, but only for `TENANT_A` — a policy narrower than the question.
struct OneTenantAuthZ;

#[async_trait]
impl AuthZResolverApi for OneTenantAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [TENANT_A],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

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
        platform_id: None,
        repo_id: None,
        plan_path: None,
        branch: None,
        run_finished_at: Some(OffsetDateTime::now_utc()),
        run_created_at: Some(OffsetDateTime::now_utc()),
    }
}

/// A directory over a database holding one projected run per listed tenant.
async fn directory_over(
    tenants: &[Uuid],
    authz: Arc<dyn AuthZResolverApi>,
) -> TenantDirectory<OrmResultsRepository> {
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
        PolicyEnforcer::new(authz),
    )
}

/// The property the three tickers depend on: **every** tenant with a projected
/// row is returned, not just the first, and the ids are the ones that were
/// written.
#[tokio::test]
async fn every_tenant_with_a_projected_row_is_enumerated() {
    let directory = directory_over(&[TENANT_A, TENANT_B], Arc::new(CrossTenantAuthZ)).await;

    let tenants: Vec<Uuid> = directory
        .known_tenants()
        .await
        .expect("a granting policy enumerates")
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
    let directory =
        directory_over(&[TENANT_A, TENANT_A, TENANT_A], Arc::new(CrossTenantAuthZ)).await;

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
    let directory = directory_over(&[], Arc::new(CrossTenantAuthZ)).await;
    assert!(directory.known_tenants().await.unwrap().is_empty());
}

/// **A policy narrower than the question does not narrow the answer.** Before
/// this crate's ticker enumeration moved to `domain::elevated`, a policy
/// granting only `TENANT_A` would have kept `TENANT_B` invisible — this test
/// used to pin exactly that. It now pins the opposite, deliberately: elevation
/// means the enumeration is unaffected by what any policy grants, so
/// `OneTenantAuthZ`'s narrow grant must not narrow this answer the way it once
/// did.
///
/// The mutation this catches is the one that matters now: reaching for
/// `self.policy_enforcer.access_scope` again instead of
/// `domain::elevated::enumeration_scope`, which would silently reintroduce the
/// old narrowing and, on the shipped dev PDP, deny the read outright (see
/// [`ticker_enumeration_does_not_consult_the_policy_engine`] and
/// `super`'s header).
#[tokio::test]
async fn a_policy_narrower_than_the_question_no_longer_narrows_the_answer() {
    let directory = directory_over(&[TENANT_A, TENANT_B], Arc::new(OneTenantAuthZ)).await;

    let tenants: Vec<Uuid> = directory
        .known_tenants()
        .await
        .expect("elevation does not depend on any policy grant")
        .into_iter()
        .map(TenantBound::get)
        .collect();
    assert_eq!(
        tenants,
        vec![TENANT_A, TENANT_B],
        "a policy scoped to TENANT_A alone must not narrow an elevated read"
    );
}

/// **The enumeration succeeds even where the shipped `static-authz-plugin`
/// would deny it.** Before this crate's ticker enumeration moved to
/// `domain::elevated`, a nil-tenant actor got no constraint from
/// `TenantScopedAuthZ` — which reproduces that plugin's rule exactly — and
/// `PolicyEnforcer::access_scope` refused with `Forbidden`. This test used to
/// pin that refusal; it now pins that elevation makes it moot: the read never
/// reaches `access_scope`, so there is no constraint to be missing and no
/// deny-all case to fail closed against.
///
/// `TenantScopedAuthZ` is kept as the double specifically because it is the
/// harshest case among the ones this suite carries — the one under which the
/// pre-elevation code always failed — so a regression back to the PEP would
/// turn this test's `Ok` back into an `Err(Forbidden)` immediately.
#[tokio::test]
async fn the_enumeration_no_longer_asks_a_pdp_that_would_deny_it() {
    let directory = directory_over(&[TENANT_A], Arc::new(TenantScopedAuthZ)).await;

    assert!(
        directory.known_tenants().await.is_ok(),
        "elevation must not depend on a deployment policy grant that this double never gives"
    );
}

/// [`AuthZResolverApi`] double that records every request it is asked to
/// decide and always grants — the harness for
/// [`ticker_enumeration_does_not_consult_the_policy_engine`]: a request that
/// slipped through to `evaluate` would be recorded here whether or not the
/// resulting decision happened to be permissive, which is what makes this a
/// stronger guard than reusing one of the doubles above.
#[derive(Default)]
struct RecordingAuthZ {
    requests: std::sync::Mutex<Vec<(String, String)>>,
}

impl RecordingAuthZ {
    fn new() -> Self {
        Self::default()
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl AuthZResolverApi for RecordingAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.requests
            .lock()
            .unwrap()
            .push((request.resource.resource_type, request.action.name));
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [TENANT_A, TENANT_B],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

/// The property this task exists to establish: `known_tenants` never asks the
/// policy engine anything. A full pass over a two-tenant projection is driven
/// through a recording double that would grant the read if it were ever
/// asked, so a request reaching `evaluate` here is unambiguously the
/// enumeration reaching for `self.policy_enforcer` again rather than for
/// `domain::elevated::enumeration_scope` — see `super`'s header.
///
/// Break-tested: reverting `TenantDirectory::scope` to call
/// `self.policy_enforcer.access_scope(...)` again makes this test fail with a
/// non-empty request log.
#[tokio::test]
async fn ticker_enumeration_does_not_consult_the_policy_engine() {
    let enforcer = Arc::new(RecordingAuthZ::new());
    let directory = directory_over(&[TENANT_A, TENANT_B], Arc::clone(&enforcer) as _).await;

    let _outcome = directory.known_tenants().await;

    assert_eq!(
        enforcer.request_count(),
        0,
        "the ticker enumeration must elevate through domain::elevated, not the PEP"
    );
}
