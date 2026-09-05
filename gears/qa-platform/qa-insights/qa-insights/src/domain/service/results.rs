//! The two flat result collections, as domain operations.
//!
//! `GET /qa/v1/test-results` and `GET /qa/v1/test-case-results`. Per D7, `OData`
//! applies to these two tables and to nothing else in this gear — they are what
//! `cpt-cf-qa-nfr-scale`'s 5M-row target is about, and every other read is either
//! bounded by a run or is an analytics reduction with its own shape.
//!
//! # Two methods, three lines each, and the reason they exist at all
//!
//! Neither method decides anything. What they do is the one thing a handler must
//! not: derive the caller's [`AccessScope`] from the `PolicyEnforcer`
//! **immediately before** the read, so no scope is ever cached across requests
//! and no transport layer is ever in a position to skip the PDP. That placement
//! is the rule every sibling gear states — `api::rest::handlers`' header repeats
//! it — and it is the whole content of this module.
//!
//! # This is a read, so `refuse_scope_beyond_tenant` is deliberately **not**
//! # called
//!
//! [`crate::domain::service::refuse_scope_beyond_tenant`] is a guard over a
//! measured asymmetry in the *write* path:
//! `ResultsRepository::upsert_run_results` filters its `DELETE`s with the full
//! compiled scope and inserts through `scope_unchecked`, which is a documented
//! no-op, so a scope narrower than the tenant makes a replay duplicate rows.
//! Nothing of that applies here. A read is a single `SELECT` through
//! `.secure().scope_with(scope)`, which applies **every** mapped predicate — so a
//! PDP that scopes a subject down to particular rows gets exactly what it asked
//! for, and calling the guard would refuse a policy this operation can honour
//! perfectly. The rule, stated once so the next reader does not have to derive
//! it: that guard belongs to writers, and to writers only.
//!
//! # `actions::LIST`, not a new verb
//!
//! `list` is the action all three sibling gears authorize a collection read under
//! (`qa-runs/src/domain/service/mod.rs:348`,
//! `qa-catalog/src/domain/service/mod.rs:143`). Task 16 added `rebuild` because
//! there was nothing to match; here there is.
//!
//! **One action for both collections, and one resource type.**
//! `resources::TEST_RESULT` already covers both tables and its doc gives the
//! reason — a case row has no independent lifecycle, and the write that creates
//! it takes one `AccessScope` for both tables. A subject that may list a run's
//! file-level outcomes has no reason to be denied its case-level ones, and a
//! deployment that wanted to split them would be splitting a resource type, not
//! an action.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use qa_insights_sdk::{TestCaseResultRecord, TestResultRecord};
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, SecurityContext};

use crate::domain::error::DomainError;
use crate::domain::repos::ResultsRepository;
use crate::domain::service::{DbProvider, actions, resources};

/// Reads over `qa_test_results` and `qa_test_case_results`.
///
/// Generic over the repository rather than boxed, for the reason
/// [`crate::domain::service::AppServices`] gives: [`ResultsRepository`]'s methods
/// are generic over their `DBRunner`, so the trait is not object-safe and the
/// parameter propagates to the composition root.
///
/// `Clone + 'static` is **not** required here, unlike
/// [`crate::domain::service::reconcile::ReconcileService`]: that bound comes from
/// `DBProvider::transaction`'s closure, and neither method below opens a
/// transaction. A single `SELECT` runs on a pooled connection; wrapping one page
/// read in a transaction would buy an isolation guarantee nothing here needs.
pub struct ResultsService<R> {
    db: Arc<DbProvider>,
    results: R,
    policy_enforcer: PolicyEnforcer,
}

impl<R> ResultsService<R>
where
    R: ResultsRepository,
{
    #[must_use]
    pub const fn new(db: Arc<DbProvider>, results: R, policy_enforcer: PolicyEnforcer) -> Self {
        Self {
            db,
            results,
            policy_enforcer,
        }
    }

    /// One page of `qa_test_results`.
    ///
    /// The `OData` query is passed through untouched: the field allow-list, the
    /// page-size clamp and the sort validation are all the repository's, where
    /// they sit next to the entity and the index they are about. A service that
    /// re-validated any of them would be a second opinion able to disagree with
    /// the one the `OpenAPI` document advertises.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the PDP denies, or compiles a scope it
    /// cannot express — including the platform-root caller, whose nil tenant
    /// yields no constraint and so fails compilation with
    /// `ConstraintsRequiredButAbsent` (`PolicyEnforcer::access_scope` always
    /// requires constraints; `authz-resolver-sdk/src/pep/compiler.rs:84-86`).
    /// That is why there is no explicit nil-tenant refusal here, unlike
    /// `ReconcileService::rebuild` — that method needs a `TenantBound` for the
    /// qa-runs listing it makes *before* any scope exists, and this one has
    /// nothing to do before the scope.
    ///
    /// [`DomainError::Validation`] for a `$filter`, `$orderby`, `$top` or cursor
    /// the caller got wrong, and [`DomainError::Database`] for a driver failure.
    pub async fn list_results(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<TestResultRecord>, DomainError> {
        let scope = self.scope(ctx).await?;
        let conn = self.db.conn()?;
        self.results.list_page(&conn, &scope, query).await
    }

    /// One page of `qa_test_case_results`. [`Self::list_results`]' sibling, under
    /// the same resource type and the same action.
    ///
    /// # Errors
    ///
    /// As [`Self::list_results`].
    pub async fn list_case_results(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<TestCaseResultRecord>, DomainError> {
        let scope = self.scope(ctx).await?;
        let conn = self.db.conn()?;
        self.results.list_case_page(&conn, &scope, query).await
    }

    /// The caller's scope for a result read, derived fresh on every call.
    ///
    /// Factored out because there are two call sites and one authorization
    /// decision: two inline copies of this are two places for a resource type or
    /// an action string to drift, and nothing in this crate would fail if they
    /// did — which is the hazard
    /// `test_support::RecordingAuthZ` exists for.
    async fn scope(&self, ctx: &SecurityContext) -> Result<AccessScope, DomainError> {
        self.policy_enforcer
            .access_scope(ctx, &resources::TEST_RESULT, actions::LIST, None)
            .await
            .map_err(DomainError::from)
    }
}

#[cfg(test)]
#[path = "results_tests.rs"]
mod results_tests;
