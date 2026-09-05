//! Which tenants do the tickers work on?
//!
//! Task 40. This module answers the one question
//! [`crate::domain::service::reconcile`]'s header left open — *"**Task 40 owns
//! 'which tenants does the ticker sweep?'**, and it is an open question, not an
//! oversight"* — for all three tickers at once.
//!
//! # Why the question exists at all
//!
//! Every unit the tickers drive is tenant-scoped by construction:
//! [`ReconcileService::reconcile_once`](crate::domain::service::reconcile::ReconcileService::reconcile_once)
//! takes a [`TenantBound`],
//! [`JiraPollerService::poll_once`](crate::domain::service::jira_poller::JiraPollerService::poll_once)
//! and
//! [`CollectService::run_collect_cycle`](crate::domain::service::collect::CollectService::run_collect_cycle)
//! take a `SecurityContext`. That is right: this gear's every write is scoped to
//! a tenant and there is no such thing as a tenant-less projection. But a ticker
//! holds no request, so unlike the REST path — `POST /qa/v1/insights/rebuild`
//! takes its tenant from the operator's own `SecurityContext` — it has nothing
//! to take a tenant *from*. Something has to enumerate.
//!
//! # The plan named two options and the first one cannot work
//!
//! `reconcile.rs`'s header offered *"a `qa_ingest_watermarks` scan (only finds
//! tenants already seen once) or a platform tenant directory (a new cross-gear
//! dependency)"*.
//!
//! **The watermark scan is circular, and provably so.** Grep for writers of that
//! table: the only one is
//! [`ReconcileService`](crate::domain::service::reconcile::ReconcileService)'s
//! own `advance_watermark`, reached from `sweep`, reached from
//! `reconcile_once(tenant)`. The rebuild endpoint deliberately writes none
//! (`rebuild`'s own doc: *"It neither reads nor writes the watermark"*). So on a
//! fresh deployment the table is empty, the scan returns nothing, no tenant is
//! ever swept, and nothing ever writes a first row. A ticker wired that way
//! would run forever and provably do nothing — which is the false-guarantee
//! defect this crate counts as a bug, not a limitation to accept.
//!
//! The second option is out of this plan's scope on the plan's own terms: it is a
//! new cross-gear dependency, and no task in the file map adds one.
//!
//! # So: the tenants this gear already holds results for
//!
//! `SELECT DISTINCT tenant_id FROM qa_test_results`
//! ([`ResultsRepository::tenants_with_results`]). It is **not** circular, which
//! is the whole reason it was chosen over the watermark: the sweep can only act
//! on a tenant this query already lists, so the sweep itself can never be that
//! tenant's first writer. `POST /qa/v1/insights/rebuild` is — it takes the
//! operator's own tenant-bound context and needs no enumeration at all (this
//! module's header, "The authority split"). One successful rebuild is enough to
//! make a tenant visible to all three tickers forever.
//!
//! A transactional broker consumer used to be a second way a tenant's first row
//! could land, under the tenant on its event envelope and with no ticker or
//! rebuild involved. No deployment ever registered the client that consumer
//! needed, so in practice it was never the writer that broke the circularity;
//! it and the event-broker dependency it needed were deleted once that was
//! established (`crate::gear`'s header, "Event ingest, and why there is only
//! one path"). `POST /qa/v1/insights/rebuild` is not a second bootstrap path
//! today — it is the only one there has ever actually been.
//!
//! ## What that costs, stated plainly
//!
//! A tenant that has **never** had a single result row projected is invisible
//! here, so:
//!
//! * a tenant is invisible to all three tickers until an operator plants its
//!   first row with one `POST /qa/v1/insights/rebuild`. That is not an outage
//!   recovery path but the ordinary way a fresh tenant becomes visible at all,
//!   since there is no other path that could deliver a first row on its own —
//!   `crate::gear`'s header states the same fact from `serve`'s point of view.
//! * the collect ticker will not collect for a tenant that has registered
//!   repositories but never run anything. Its exact case counts stay at the
//!   static universe numbers until the tenant's first run, which is also what
//!   `analytics::expected_cases` falls back to.
//! * the JIRA poller will not poll a tenant with no results, which cannot have
//!   any bugs either: `qa_jira_bugs` rows are filed *against* test results
//!   (`JiraService::file_bugs` resolves the run and its failing case first), so
//!   this ticker's blind spot is empty by construction rather than by luck.
//!
//! *Rejected:* enumerating a *different* table per ticker — `qa_jira_config` for
//! the poller, say. Three enumerations is three cross-tenant reads and three
//! scopes for one question, and R87 ("one scope per resource type") is about not
//! reading two resources under one scope, not about splitting one question across
//! three. The poller's own `active_config` early-return already makes a tenant
//! with no JIRA configuration a single cheap read per pass
//! (`jira_poller.rs`, Step 0 point 2), so the narrower enumeration would buy one
//! `SELECT` per idle tenant per pass and cost two more cross-tenant scopes.
//!
//! # The authority split, which is the part R86 is about
//!
//! This is the only place in the crate that mints a scope for a **nil-tenant**
//! actor ([`system_actor::for_ticker_enumeration`]), and it is the only place
//! allowed to: the answer is a list of `tenant_id`s, so a scope restricted to
//! one tenant could not express the question. Everything the tickers do
//! *afterwards* is issued under a tenant-bound context minted from a
//! [`TenantBound`] this method returned — and [`TenantBound`] cannot be built
//! from nil, so a ticker physically cannot carry the enumeration's authority into
//! its work. `qa-runs`' `domain::service::schedules` header states the same split
//! for the same reason ("The tenant travels with the schedule, and never comes
//! from anywhere else"), and its `list_enabled` returns each row paired with its
//! own tenant for exactly this.
//!
//! R86 itself does not bite here and it is worth saying why rather than leaving
//! it to a reviewer: R86 is about a `.one()` over a possibly-multi-tenant scope
//! silently picking a row. This read returns a `Vec`, has no single row to pick,
//! and a `tenant_id` equality predicate would make it answer its own question.
//!
//! # This scope is elevated, not PEP-compiled, and that is why the tickers are
//! # not idle under the shipped dev PDP
//!
//! `static-authz-plugin` — the development one of the tree's **two**
//! authorization plugins — answers *"Nil or missing tenant → `decision:
//! false`"*
//! (`gears/system/authz-resolver/plugins/static-authz-plugin/src/lib.rs:6`,
//! verified 2026-08-25), and so does `tr-authz-plugin`, the tenant-hierarchy
//! resolver
//! (`gears/system/authz-resolver/plugins/tr-authz-plugin/src/domain/service.rs:68-69`,
//! `"tr-authz: subject tenant_id is nil -- deny"`). A nil-tenant context
//! reaching `PolicyEnforcer::access_scope` therefore fails closed under either
//! shipped plugin — which is exactly the behaviour
//! [`TenantDirectory::known_tenants`] no longer exercises:
//! [`TenantDirectory::scope`] never calls `access_scope` for this read at all.
//! It reaches for [`crate::domain::elevated::enumeration_scope`] instead, the one
//! seam in this crate that elevates past the PEP rather than asking it — see
//! that module's doc for why an unscoped nil-tenant *read* here is not the
//! security regression a covering cross-tenant *write* grant would be.
//!
//! So on every deployment, shipped dev plugin included, this read succeeds and
//! every ticker pass sees whichever tenants this gear has projected results
//! for — with no deployment grant required. `qa-runs` and `qa-catalog` reach
//! the same conclusion for their own nil-tenant enumerations, for the same
//! reason: a `system_grants` config surface on a shared plugin was replaced by
//! one named function in the gear that needs it.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use toolkit_security::AccessScope;

use crate::domain::error::DomainError;
use crate::domain::repos::ResultsRepository;
use crate::domain::service::DbProvider;
use crate::domain::system_actor::{self, TenantBound};

/// The tenants this gear's background work runs for.
///
/// Generic over the repository rather than boxed, for
/// [`crate::domain::service::AppServices`]' reason: [`ResultsRepository`]'s
/// methods are generic over their `DBRunner`, so the trait is not object-safe.
/// No `Clone + 'static` bound, like
/// [`crate::domain::service::results::ResultsService`] and unlike the
/// reconciler: the one read below opens no transaction.
pub struct TenantDirectory<R> {
    db: Arc<DbProvider>,
    results: R,
    /// Unused by [`Self::scope`] since the ticker enumeration moved to
    /// `domain::elevated` — kept on the struct, and required by [`Self::new`],
    /// so the tests that prove it is genuinely never asked
    /// (`tenants_tests::RecordingAuthZ`) can still wire an `AuthZ` double in
    /// through the same constructor every other caller uses.
    #[allow(
        dead_code,
        reason = "read by no method, but its presence is exactly what the RecordingAuthZ \
                   guard test needs to wire a double in and prove unused"
    )]
    policy_enforcer: PolicyEnforcer,
}

impl<R> TenantDirectory<R>
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

    /// Every tenant this gear holds projected results for, ascending.
    ///
    /// Each answer is a [`TenantBound`], so a caller cannot mistake one for the
    /// nil sentinel the enumeration itself ran under — see this module's header,
    /// "The authority split". A nil id would be dropped rather than returned,
    /// and it cannot occur: every writer of `qa_test_results` reaches
    /// `IngestService::write_run_projection` through
    /// `ReconcileService::reproject`, which takes a [`TenantBound`] rather than
    /// a `Uuid` — a nil tenant is refused before a projection is ever built,
    /// not filtered out after.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure. This read no longer asks
    /// the PDP anything (see [`Self::scope`]), so unlike every other call in
    /// this gear it cannot return [`DomainError::Forbidden`].
    pub async fn known_tenants(&self) -> Result<Vec<TenantBound>, DomainError> {
        let scope = Self::scope();
        let conn = self.db.conn()?;
        let ids = self.results.tenants_with_results(&conn, &scope).await?;
        Ok(ids.into_iter().filter_map(TenantBound::new).collect())
    }

    /// The enumeration scope: [`crate::domain::elevated::enumeration_scope`],
    /// not a PEP decision.
    ///
    /// [`system_actor::for_ticker_enumeration`] is still called, for its
    /// audit-logging side effect only — the `SecurityContext` it returns is
    /// discarded rather than handed to `self.policy_enforcer`. See
    /// `domain::elevated`'s module doc for why this one read bypasses the PEP,
    /// and this module's header, "This scope is elevated, not PEP-compiled",
    /// for what that means for a deployment running the shipped dev PDP.
    ///
    /// `self.policy_enforcer` is unused here and stays on the struct only
    /// because [`Self::new`]'s signature is otherwise unchanged from before this
    /// method stopped consulting it; `tenants_tests`' `RecordingAuthZ` double
    /// pins that it is genuinely never asked. An associated function rather
    /// than a `&self` method for exactly that reason — nothing on `self` is
    /// read any more.
    fn scope() -> AccessScope {
        let _enumeration = system_actor::for_ticker_enumeration();
        crate::domain::elevated::enumeration_scope()
    }
}

#[cfg(test)]
#[path = "tenants_tests.rs"]
mod tenants_tests;
