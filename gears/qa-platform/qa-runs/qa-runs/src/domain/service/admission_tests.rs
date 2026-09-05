//! Unit tests for per-platform admission, plus the test doubles the whole
//! concurrency core shares.
//!
//! # Why the doubles live here rather than in `service::test_support`
//!
//! That file is Task 13's and its `MockRunsRepository` answers `unsupported(..)`
//! for `set_execution_ref`, `set_bundle_ids` and `list_timeout_candidates` —
//! precisely the three methods this task's writes go through. Extending it would
//! have meant editing a file this task does not own, so the doubles are declared
//! in [`fakes`], which is `pub(in crate::domain::service)` so `dispatch`'s tests
//! reach the same ones. Two copies of a queue double would drift on the state
//! transitions they model, which is the thing the tests are about.
//!
//! The doubles the launch tests already own are reused unchanged:
//! `PermissiveAuthZ`, `test_db_provider`, `ctx`, `OWNER_TENANT`, `OTHER_TENANT`.
//!
//! # Every double applies tenant scoping
//!
//! `domain::repos::OwnedRunId` states the rule and the reason: a token proves only
//! that *some* `RunsRepository::get` answered `Some` under this scope, so a double
//! whose `get` answers unconditionally mints tokens freely and every ownership
//! test "proves" a precheck it never ran. [`fakes::FakeRuns`] filters on
//! `scope.contains_uuid(OWNER_TENANT_ID, ..)` in every method that has a stored
//! row to filter — reads and writes alike, the second half mattering because
//! Task 13's `RecordingDispatcher::starting` mutates its fixture directly,
//! bypassing scope, so a test that "proved" a state change through it proved
//! nothing about the repository.
//!
//! **Two methods do not filter, and this sentence used to claim in bold that
//! every one did.** Corrected 2026-08-14 by the security review, which read the
//! bodies Task 15 added:
//!
//! * `create` has nothing to filter — there is no stored row yet, and the tenant
//!   it stamps is the caller's own `subject_tenant_id`. What it *does* model is
//!   the tenant-prefixed unique index, which is the property `RunNameExists`
//!   depends on.
//! * `upsert_test_result` writes the child row without re-checking the scope
//!   against a fixture, because the scoping it is asked about happened one call
//!   earlier: its `run: OwnedRunId` can only have come from `resolve_owned`, whose
//!   provided body is a scoped `get` on this same double. Filtering again here
//!   would assert the token's provenance a second time rather than something new.
//!
//! Both still assert the **resource-type marker**, so a scope compiled for
//! `qa.queue_entry` reaching either of them fails. The structural argument is what
//! carries the tenancy, and it is stated here rather than left as a bold claim
//! the code does not make.
//!
//! Every expected value for ported logic comes from reading the source system.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use qa_environments_sdk::LeaseState;
use qa_runs_sdk::{QueueState, RunState};
use uuid::Uuid;

use super::*;
use crate::domain::service::test_support::{OTHER_TENANT, OWNER_TENANT, ctx};
use fakes::{PLATFORM_A, PLATFORM_B, queued_row, run_fixture};

/// [`Admitter::admit`] keeping only the decision, releasing the capacity slot on
/// the spot.
///
/// Right for a test that admits and then asserts, and wrong for the tests below
/// that are about the cap: those hold the whole [`Admitted`], because the slot is
/// what a second caller counts.
#[async_trait]
trait AdmitDecision {
    async fn admit_decision(
        &self,
        ctx: &SecurityContext,
        run: &Run,
    ) -> Result<Admission, DomainError>;
}

#[async_trait]
impl<A: Admitter + ?Sized> AdmitDecision for A {
    async fn admit_decision(
        &self,
        ctx: &SecurityContext,
        run: &Run,
    ) -> Result<Admission, DomainError> {
        self.admit(ctx, run)
            .await
            .map(|admitted| admitted.admission)
    }
}

pub(in crate::domain::service) mod fakes {
    //! In-memory doubles for the runs repository, the queue repository,
    //! qa-environments and qa-catalog, plus the wiring that assembles an
    //! admission and a dispatch service over them.

    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
    use authz_resolver_sdk::models::{
        EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
    };
    use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, PolicyEnforcer};
    use qa_catalog_sdk::{
        BundleRequest, CustomPlan, CustomPlanEntry, NewCustomPlan, NewTestRepository, Plan,
        Product, QaCatalogClientV1, QaCatalogError, SshKey, SyncRequest, TestBundle, TestFileMeta,
        TestRepository, TestRepositoryUpdate, UniverseTest,
    };
    use qa_environments_sdk::{
        AcquireOutcome, Environment, EnvironmentPatch, LeaseMode, LeaseState, NewEnvironment,
        NewVariable, QaEnvironmentsClientV1, QaEnvironmentsError, Variable,
    };
    use qa_runs_sdk::{
        ExclusiveTier, QueueState, Run, RunKind, RunResult, RunSource, RunState, RunTarget,
    };
    use time::OffsetDateTime;
    use toolkit_db::secure::DBRunner;
    use toolkit_security::pep_properties::{self, OWNER_TENANT_ID};
    use toolkit_security::{AccessScope, SecurityContext};
    use uuid::Uuid;

    use crate::domain::error::DomainError;
    use crate::domain::ports::product_plugin::{PluginUnavailable, ProductPluginPort};
    use crate::domain::ports::run_executor::{
        ExecutionRef, ExecutionStream, RunAccess, RunExecutor, RunSpec, RunnerSpec,
    };
    use crate::domain::queue::{AdmissionDecision, QueuedRow};
    use crate::domain::repos::{
        ClaimAge, ClaimRow, ExpiredRow, NewQueueRow, NewRun, NewTestResult, OwnedRunId,
        QueueRepository, QueueRowRecord, QueuedPlatform, RowStatus, RunResultDelta, RunStatePatch,
        RunWithResult, RunsRepository, TestResultRow, TimeoutCandidate, WatchCandidate, Windowed,
    };
    use crate::domain::service::admission::{AdmissionDeps, AdmissionService, PlatformLocks};
    use crate::domain::service::dispatch::{DispatchDeps, DispatchService};
    use crate::domain::service::test_support::unfiltered_page;
    use crate::domain::service::test_support::{
        OTHER_TENANT, OWNER_TENANT, RecordingWatcher, test_db_provider,
    };
    use crate::domain::service::{DbProvider, QueueLimits};
    use toolkit_odata::{ODataQuery, Page};

    pub(in crate::domain::service) const PLATFORM_A: Uuid = Uuid::from_u128(0x0C0A);
    pub(in crate::domain::service) const PLATFORM_B: Uuid = Uuid::from_u128(0x0C0B);
    pub(in crate::domain::service) const REPO: Uuid = Uuid::from_u128(0x0B0A);
    /// The product every fixture environment belongs to, and the one
    /// [`FakeProductPlugins`] resolves.
    pub(in crate::domain::service) const PRODUCT: Uuid = Uuid::from_u128(0x0F0A);

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    /// A run, in whatever state a test needs.
    pub(in crate::domain::service) fn run_fixture(
        id: Uuid,
        platform_id: Option<Uuid>,
        exclusive: bool,
        state: RunState,
    ) -> Run {
        Run {
            id,
            name: format!("run-{}", id.as_u128()),
            target: RunTarget::Plan {
                repo_id: REPO,
                path: "tests/plan.yaml".to_owned(),
            },
            platform_id,
            test_version: Some("main".to_owned()),
            app_version: Some("7.1".to_owned()),
            app_build: None,
            state,
            resolved_exclusive: exclusive,
            exclusive_tier: ExclusiveTier::Default,
            is_validation: false,
            parameters: Vec::new(),
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
            source: RunSource::Manual,
            schedule_id: None,
            bundle_ids: Vec::new(),
            execution_ref: None,
            log_storage_ref: None,
            timeout_at: None,
            started_at: None,
            finished_at: None,
            error: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    /// A stored queue row, enqueued (and, for a claim, dispatched) just now.
    pub(in crate::domain::service) fn queued_row(
        id: Uuid,
        tenant_id: Uuid,
        run_id: Uuid,
        platform_id: Uuid,
        exclusive: bool,
        state: QueueState,
    ) -> QueueRowRecord {
        row_aged(id, tenant_id, run_id, platform_id, exclusive, state, 0)
    }

    /// The same, `age_seconds` in the past.
    ///
    /// `dispatched_at` is set for the two claim states and left `None` otherwise,
    /// which is what makes `all_claims`' `dispatched_at.unwrap_or(enqueued_at)`
    /// coalescing (`run_queue.rs:341-346`) meaningful in a test.
    pub(in crate::domain::service) fn row_aged(
        id: Uuid,
        tenant_id: Uuid,
        run_id: Uuid,
        platform_id: Uuid,
        exclusive: bool,
        state: QueueState,
        age_seconds: i64,
    ) -> QueueRowRecord {
        let at = now() - time::Duration::seconds(age_seconds);
        QueueRowRecord {
            id,
            tenant_id,
            run_id,
            platform_id,
            run_kind: RunKind::Plan,
            source: RunSource::Manual,
            exclusive,
            state,
            error: None,
            enqueued_at: at,
            dispatched_at: matches!(state, QueueState::Dispatching | QueueState::Running)
                .then_some(at),
            finished_at: None,
        }
    }

    // -----------------------------------------------------------------------
    // AuthZ
    // -----------------------------------------------------------------------

    /// Grants every request, and — unlike `test_support::PermissiveAuthZ` —
    /// answers a **covering** constraint set for a nil-tenant subject.
    ///
    /// That is the deployment `domain::system_actor`'s header describes as the
    /// precondition for the enumeration factories to work at all: *"A nil-tenant
    /// context is therefore denied **unless the deployment's policy returns a
    /// covering constraint set for this subject**."* Without such a double the
    /// dispatcher tick is `Forbidden` at its first step and no tick test could
    /// reach anything.
    ///
    /// The covering set lists the two fixture tenants explicitly rather than
    /// being unconstrained, so a *write* that leaked the enumeration context
    /// would still be visible: it would touch both tenants' rows, which
    /// `the_dispatcher_writes_under_the_rows_own_tenant` detects.
    pub(in crate::domain::service) struct SystemGrantingAuthZ;

    fn tenant_of(request: &EvaluationRequest) -> Option<Uuid> {
        request
            .context
            .tenant_context
            .as_ref()
            .and_then(|tc| tc.root_id)
            .or_else(|| {
                request
                    .subject
                    .properties
                    .get("tenant_id")
                    .and_then(|value| value.as_str())
                    .and_then(|value| Uuid::parse_str(value).ok())
            })
            .filter(|id| !id.is_nil())
    }

    /// The property the `AuthZ` doubles stamp the resource type onto, so a compiled
    /// scope carries which resource type it was compiled *for*.
    ///
    /// # Why this exists: "one scope per resource type" was unfalsifiable
    ///
    /// `mod.rs`, `admission.rs` and `dispatch.rs` each carry a paragraph saying a
    /// scope compiled for `qa.run` is never passed to a query against
    /// `qa_run_queue`. Changing `admit`'s depth read from `queue_scope` to
    /// `run_scope` — that exact defect — left **419 tests passing and clippy
    /// clean**, because the repository doubles filter on `contains_uuid(
    /// OWNER_TENANT_ID, …)` and the `AuthZ` double answered the same tenant
    /// predicate for every resource and action. Three prose paragraphs guarding
    /// nothing.
    ///
    /// A real `AccessScope` does not expose the resource type it was compiled for,
    /// and `PolicyEnforcer` is not a seam these tests can reach behind — so the
    /// marker is injected where the type *is* visible: the `AuthZResolverClient`
    /// double sees `request.resource.resource_type`. Each repository double then
    /// asserts the marker matches its own table. That is a property of the test
    /// harness rather than of production, and it is stated as such: what it
    /// catches is a **service-layer transposition**, which is the only place the
    /// mistake is expressible — `admit` is the one body deriving three scopes for
    /// three repository calls.
    ///
    /// # It rides `RESOURCE_ID`, and that is a trap worth naming
    ///
    /// A made-up property name does not work: `pep::compiler` fails a constraint
    /// whose predicate names a property outside the `ResourceType`'s declared list
    /// (`compiler.rs:222-223`, fail-closed), and with one constraint that fails the
    /// whole compile — the first attempt at this turned all 43 admission and
    /// dispatch tests into `Forbidden`. So the marker uses
    /// [`pep_properties::RESOURCE_ID`], which both `resources::RUN` and
    /// `resources::QUEUE_ENTRY` declare.
    ///
    /// **The consequence: a scope from these doubles carries a
    /// `resource_id IN [marker]` filter, so handing one to a real
    /// `Orm*Repository` would filter out every row.** It cannot happen today —
    /// these doubles are reached only from `domain::service`'s unit tests, and the
    /// DB-backed tests in `infra::storage` build their scopes with
    /// `test_db::scope` — but if a future test ever wires `SystemGrantingAuthZ` to
    /// an ORM repository, this is why it returns nothing.
    ///
    /// # Two costs, named rather than left to be discovered
    ///
    /// 1. **`RESOURCE_ID` is now spent.** No test using these doubles can check
    ///    that a scope compiled with `Some(resource_id)` restricts to that row,
    ///    because the property that would carry it is occupied by the marker. That
    ///    is a real narrowing — every `access_scope(.., Some(id))` call in these two
    ///    services passes an id whose row-level effect is unobservable here. Closing
    ///    it needs a second declared property on the two `ResourceType`s, which is
    ///    production code changed for a test, or a fake `PolicyEnforcer`, which is
    ///    not a seam these services expose.
    /// 2. **The guard was opt-in per method, and is not any more.** Every method on
    ///    both doubles asserts — 12 on `FakeRuns` and 16 on `FakeQueue`, verified
    ///    exhaustively rather than by eye. It began as 21 sites covering only the
    ///    methods these two services happen to call, which left `create`,
    ///    `add_result_counts`, `get_result`, `upsert_test_result`,
    ///    `list_test_results`, `list_for_read`, `row_status` and `cancel_queued`
    ///    unguarded — **five of which Task 15 touches.** A guard that covers today's
    ///    callers is a guard the next task walks around without noticing.
    const RESOURCE_MARKER: &str = pep_properties::RESOURCE_ID;

    /// A stable id per resource-type name, so the marker survives a scope round
    /// trip through `InPredicate`, which carries `Uuid`s and not strings.
    pub(in crate::domain::service) fn resource_marker(resource_type: &str) -> Uuid {
        match resource_type {
            "qa.run" => Uuid::from_u128(0x0A11_0000_0000_0AAA),
            "qa.queue_entry" => Uuid::from_u128(0x0A11_0000_0000_0BBB),
            // A resource type these tests do not know about. Distinct from both, so
            // an added type fails the assertions rather than aliasing one of them.
            _ => Uuid::from_u128(0x0A11_0000_0000_0FFF),
        }
    }

    /// Assert that `scope` was compiled for `expected` — the harness half of "one
    /// scope per resource type".
    pub(in crate::domain::service) fn assert_scope_is_for(
        scope: &AccessScope,
        expected: &str,
        method: &str,
    ) {
        assert!(
            scope.contains_uuid(RESOURCE_MARKER, resource_marker(expected)),
            "{method} was handed a scope that was not compiled for {expected}; \
             see `resource_marker`'s doc for why this assertion exists"
        );
    }

    /// Assert that `scope` is the elevated, unconstrained scope
    /// `domain::elevated::enumeration_scope` returns — the harness half of
    /// Task 2's property for the four repository methods a nil-tenant sweep
    /// reaches (`platforms_with_queued_rows`, `list_timeout_candidates`,
    /// `list_watch_candidates`, `all_claims`).
    ///
    /// These four no longer receive a PEP-compiled scope at all, so there is
    /// no `RESOURCE_MARKER` left for [`assert_scope_is_for`] to check — a
    /// production caller that regressed to `policy_enforcer.access_scope`
    /// would still be caught here, because a real compiled scope is never
    /// unconstrained under any `AuthZ` double this suite uses.
    pub(in crate::domain::service) fn assert_scope_is_unconstrained(
        scope: &AccessScope,
        method: &str,
    ) {
        assert!(
            scope.is_unconstrained(),
            "{method} was handed a constrained scope; a nil-tenant sweep must elevate \
             through domain::elevated, not the PEP"
        );
    }

    fn constraint_for(tenants: &[Uuid], resource_type: &str) -> Vec<Constraint> {
        vec![Constraint {
            predicates: vec![
                Predicate::In(InPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    tenants.to_vec(),
                )),
                Predicate::In(InPredicate::new(
                    RESOURCE_MARKER,
                    vec![resource_marker(resource_type)],
                )),
            ],
        }]
    }

    #[async_trait]
    impl AuthZResolverClient for SystemGrantingAuthZ {
        async fn evaluate(
            &self,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            let constraints = match tenant_of(&request) {
                Some(id) => constraint_for(&[id], &request.resource.resource_type),
                // The covering grant for `qa_runs.system`.
                None => constraint_for(
                    &[OWNER_TENANT, OTHER_TENANT],
                    &request.resource.resource_type,
                ),
            };
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints,
                    ..Default::default()
                },
            })
        }
    }

    /// [`SystemGrantingAuthZ`]'s exact grant, plus a record of every nil-tenant
    /// request it was asked to decide.
    ///
    /// This is what a break-test needs: a sweep that still asked the PEP for a
    /// nil-tenant scope is otherwise indistinguishable from one elevated through
    /// `domain::elevated`, because both end up with the same covering constraint
    /// set and the same rows come back. `requested_any_for_nil_tenant` is the
    /// property Task 2 asserts — that the six enumeration sites in
    /// `domain::system_actor`'s header never reach `evaluate` at all.
    #[derive(Default)]
    pub(in crate::domain::service) struct RecordingAuthZ {
        nil_tenant_requests: Mutex<Vec<(String, String)>>,
    }

    impl RecordingAuthZ {
        pub(in crate::domain::service) fn new() -> Self {
            Self::default()
        }

        /// Whether any recorded request carried a nil-tenant context.
        ///
        /// A filter over the same recorded vector `requested` (on the sibling
        /// doubles) reads from, kept as its own predicate because the property
        /// under test is "was the PEP ever asked while nil-tenant", not "was it
        /// asked for this particular action or resource".
        pub(in crate::domain::service) fn requested_any_for_nil_tenant(&self) -> bool {
            !self.nil_tenant_requests.lock().unwrap().is_empty()
        }
    }

    #[async_trait]
    impl AuthZResolverClient for RecordingAuthZ {
        async fn evaluate(
            &self,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            let tenant = tenant_of(&request);
            if tenant.is_none() {
                self.nil_tenant_requests.lock().unwrap().push((
                    request.resource.resource_type.clone(),
                    request.action.name.clone(),
                ));
            }
            let constraints = match tenant {
                Some(id) => constraint_for(&[id], &request.resource.resource_type),
                // The covering grant for `qa_runs.system` -- see `SystemGrantingAuthZ`.
                None => constraint_for(
                    &[OWNER_TENANT, OTHER_TENANT],
                    &request.resource.resource_type,
                ),
            };
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints,
                    ..Default::default()
                },
            })
        }
    }

    /// Grants the **whole cluster** to this gear's system subject, whatever tenant
    /// its context carries — and **records the raw subject tenant each request
    /// named**, before it is compiled into a scope.
    ///
    /// # This is the adversarial deployment the per-row tenant check defends
    /// # against
    ///
    /// [`SystemGrantingAuthZ`] narrows a tenant-bound context to its own tenant and
    /// widens only the nil-tenant one. That is the *benign* PDP, and every other
    /// test here runs under it — which is why a set-based background write can look
    /// tenant-safe in this harness while being unsafe in a real deployment.
    ///
    /// The nil-tenant enumeration factories do not need this policy any more —
    /// they elevate through `domain::elevated::enumeration_scope` and never
    /// reach the PDP (`domain::system_actor`'s Authorization note). What still
    /// needs it is the tenant-bound *write* that follows each one:
    /// `domain::system_actor`'s header states the condition that write's own
    /// scoping guards against — a deployment whose policy grants
    /// `qa_runs.system` a **covering** constraint set for a write action,
    /// rather than clamping it to the tenant the context names. The natural
    /// way for a policy to do that is to key on the subject rather than on
    /// the resolved tenant — exactly as the reference dev stack's
    /// `static-authz` keys on the resolved tenant instead — and then every
    /// tenant-bound context built from that subject gets the same covering
    /// set. This double is that policy: it grants every tenant to any
    /// `qa_runs.system` subject and behaves normally for everyone else.
    ///
    /// It exists to make one thing observable: with a covering scope,
    /// `expire_queued_before` returns **every** tenant's expirable rows in the first
    /// iteration of the sweep's per-tenant loop, and only the per-row `tenant_id`
    /// check stops them being transitioned under the wrong tenant.
    /// `report.expired` is identical either way.
    ///
    /// # Recording exists because the *response* cannot prove this
    ///
    /// Under a covering grant every `qa_runs.system` request gets back the
    /// identical two-tenant constraint set, whichever tenant actually asked —
    /// that is the whole point of "covering". So a compiled `AccessScope`
    /// cannot distinguish a correct per-row context from the historical
    /// loop-tenant bug (deriving the context from the enumeration's tenant
    /// instead of the row's): both produce the same scope and the same
    /// `report.expired`. [`Self::subject_tenants_for`] and
    /// [`Self::subject_tenants`] read the *request* instead, the same way
    /// [`SchedulerAuthZ::asked`] does for resource type and action — the one
    /// place the real identity still exists before this double erases the
    /// distinction on purpose.
    /// `(resource_id, subject_tenant)` per request.
    type AskedTenant = (Option<Uuid>, Option<Uuid>);

    #[derive(Default)]
    pub(in crate::domain::service) struct CoveringSystemAuthZ {
        /// One entry per request, in order.
        asked: Mutex<Vec<AskedTenant>>,
    }

    impl CoveringSystemAuthZ {
        /// The subject tenant of every request that named `resource_id`, in
        /// order. Recoverable because `expire_one_row` -> `read_run` ->
        /// `run_scope(ctx, GET, Some(row.run_id))` puts the row's run id on
        /// the request this double sees.
        ///
        /// Deduplicated: a single row is typically the subject of several PEP
        /// calls under the *same* per-row context (a `GET` then a `DISPATCH`,
        /// for instance), and this answers "how many distinct tenants did
        /// they carry", not "how many calls were made" — a correct
        /// implementation answers with exactly one element regardless of call
        /// count, and a loop-tenant regression answers with the wrong one (or
        /// with more than one, if some calls happened to land right).
        pub(in crate::domain::service) fn subject_tenants_for(
            &self,
            resource_id: Uuid,
        ) -> Vec<Uuid> {
            let mut tenants: Vec<Uuid> = self
                .asked
                .lock()
                .unwrap()
                .iter()
                .filter(|(rid, _)| *rid == Some(resource_id))
                .filter_map(|(_, tenant)| *tenant)
                .collect();
            tenants.sort_unstable();
            tenants.dedup();
            tenants
        }

        /// Every request's subject tenant, in order, regardless of resource id.
        pub(in crate::domain::service) fn subject_tenants(&self) -> Vec<Uuid> {
            self.asked
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(_, tenant)| *tenant)
                .collect()
        }
    }

    #[async_trait]
    impl AuthZResolverClient for CoveringSystemAuthZ {
        async fn evaluate(
            &self,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            self.asked
                .lock()
                .unwrap()
                .push((request.resource.id, tenant_of(&request)));
            let is_gear_system = request.subject.subject_type.as_deref() == Some("qa_runs.system");
            let tenants: Vec<Uuid> = if is_gear_system {
                vec![OWNER_TENANT, OTHER_TENANT]
            } else {
                match tenant_of(&request) {
                    Some(id) => vec![id],
                    None => vec![OWNER_TENANT, OTHER_TENANT],
                }
            };
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: constraint_for(&tenants, &request.resource.resource_type),
                    ..Default::default()
                },
            })
        }
    }

    /// Denies everything, which is what the reference dev stack does to every
    /// gear system actor: `static-authz` derives its decision purely from the
    /// resolved tenant and denies a nil tenant outright.
    pub(in crate::domain::service) struct DenyingAuthZ;

    #[async_trait]
    impl AuthZResolverClient for DenyingAuthZ {
        async fn evaluate(
            &self,
            _request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            })
        }
    }

    /// Grants every action but one.
    ///
    /// Needed to reach the *per-row* denial the WARN deduplication exists for: a
    /// pass whose enumeration is also denied never gets as far as its rows, so a
    /// blanket deny cannot distinguish "one WARN per pass" from "one WARN,
    /// full stop".
    pub(in crate::domain::service) struct DenyingActionAuthZ {
        pub(in crate::domain::service) action: &'static str,
    }

    #[async_trait]
    impl AuthZResolverClient for DenyingActionAuthZ {
        async fn evaluate(
            &self,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            if request.action.name == self.action {
                return Ok(EvaluationResponse {
                    decision: false,
                    context: EvaluationResponseContext::default(),
                });
            }
            let constraints = match tenant_of(&request) {
                Some(id) => constraint_for(&[id], &request.resource.resource_type),
                None => constraint_for(
                    &[OWNER_TENANT, OTHER_TENANT],
                    &request.resource.resource_type,
                ),
            };
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints,
                    ..Default::default()
                },
            })
        }
    }

    // -----------------------------------------------------------------------
    // Runs repository
    // -----------------------------------------------------------------------

    /// In-memory `RunsRepository` that applies tenant scoping in every method.
    ///
    /// # The four result methods were `unsupported(..)` until Task 15
    ///
    /// `create`, `add_result_counts`, `get_result`, `upsert_test_result` and
    /// `list_test_results` asserted their scope and then refused. Task 15's
    /// ingest and re-run paths call all five, and the alternative — a second
    /// runs double in `ingest_tests` — is the drift this module's header warns
    /// about for the queue double: two in-memory models of the same five
    /// counters would disagree about clamping the moment one of them was
    /// "simplified". They are implemented here instead, mirroring the real
    /// repository's two observable rules (the zero floor on each counter, and
    /// the column-width truncation of a status) for the same reason
    /// `FakeEnvironments::acquire_lease` mirrors `decide_acquire` arm for arm:
    /// the conclusions drawn in the tests rest on the double behaving like the
    /// thing it stands in for.
    #[derive(Default)]
    pub(in crate::domain::service) struct FakeRuns {
        rows: Mutex<Vec<(Uuid, Run)>>,
        /// `(run_id, counters)`. Separate from the row because
        /// `qa_runs_sdk::Run` carries no counters — `get_result` is its own
        /// projection.
        counters: Mutex<Vec<(Uuid, RunResult)>>,
        /// `(tenant_id, row)`, the `qa_run_test_results` child table.
        results: Mutex<Vec<(Uuid, TestResultRow)>>,
        /// Every [`NewTestResult`] handed to `upsert_test_result`, in order.
        ///
        /// Recorded **in addition to** [`Self::results`], for the reason
        /// [`Self::count_deltas`] is recorded in addition to the counters: the
        /// stored projection is lossy about the thing under test.
        ///
        /// **Corrected by Task 4.** This used to add that `TestResultRow`
        /// carries none of `nodeid`, `reason` or `ticket`, making this "the
        /// only place the three are observable at the service tier". It now
        /// carries all three - the reconciler read needed them - so
        /// [`Self::results_of`] observes them too. The recording stays, because
        /// what it observes is still different: this is what ingest *asked* for,
        /// including the `Option` before the write collapses `nodeid` to `""`,
        /// while `results` is what was stored after the collapse.
        pub(in crate::domain::service) new_results: Mutex<Vec<NewTestResult>>,
        pub(in crate::domain::service) bundle_writes: Mutex<Vec<(Uuid, Vec<Uuid>)>>,
        pub(in crate::domain::service) execution_writes: Mutex<Vec<(Uuid, String)>>,
        pub(in crate::domain::service) transitions: Mutex<Vec<(Uuid, RunState, RunState)>>,
        /// Every delta `add_result_counts` was handed, in order — the signed
        /// values themselves, which is what a delta bug shows up in and what an
        /// accumulated total hides.
        pub(in crate::domain::service) count_deltas: Mutex<Vec<(Uuid, RunResultDelta)>>,
        /// Every `NewRun` handed to `create`, in order.
        pub(in crate::domain::service) created: Mutex<Vec<NewRun>>,
        /// Makes `set_execution_ref` fail, so "the submit succeeded but could not
        /// be recorded" is reachable.
        pub(in crate::domain::service) fail_execution_ref: Mutex<bool>,
        /// `list_watch_candidates`' window, in rows. `None` returns everything
        /// visible.
        ///
        /// Same reasoning as [`FakeQueue::claim_scan_window`], and the same
        /// limits: the real window is `MAX_WATCH_SCAN` rows, and what a small
        /// one here does **not** cover is the SQL, which
        /// `infra::storage::runs_sea_repo` pins against a real database.
        ///
        /// It exists because without it the rotation was **unpinned at the
        /// caller**, which is where its own doc says the bound lives. A double
        /// that never truncates leaves the service's cursor `None` on every
        /// tick, so `after` is never given a value and mutating the cursor
        /// advance to an unconditional `None` - the permanent starvation the
        /// trait doc says rotation prevents - was green across the whole suite.
        watch_scan_window: Option<usize>,
        /// For each `get`, the run id and **which of the two known tenants the
        /// scope it was issued under admits**.
        ///
        /// Recorded because the returned row looks identical whether the read
        /// was issued under the row's own tenant-bound context or under the
        /// cross-tenant enumeration identity, so nothing else in this harness
        /// can tell them apart. `reattach_watchers` claims the narrower one and
        /// the claim had no falsifying input.
        read_scope_tenants: Mutex<Vec<(Uuid, Vec<Uuid>)>>,
    }

    impl FakeRuns {
        pub(in crate::domain::service) fn with(rows: Vec<(Uuid, Run)>) -> Self {
            Self {
                rows: Mutex::new(rows),
                ..Self::default()
            }
        }

        /// [`Self::with`], with `list_watch_candidates` windowed to `window`
        /// rows so a test can drive the re-attachment scan's rotation.
        pub(in crate::domain::service) fn with_watch_scan_window(
            rows: Vec<(Uuid, Run)>,
            window: usize,
        ) -> Self {
            Self {
                rows: Mutex::new(rows),
                watch_scan_window: Some(window),
                ..Self::default()
            }
        }

        /// The tenants each `get` of `run_id` was authorized over, oldest first.
        pub(in crate::domain::service) fn tenants_admitted_reading(
            &self,
            run_id: Uuid,
        ) -> Vec<Vec<Uuid>> {
            self.read_scope_tenants
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _)| *id == run_id)
                .map(|(_, tenants)| tenants.clone())
                .collect()
        }

        pub(in crate::domain::service) fn state_of(&self, run_id: Uuid) -> Option<RunState> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .find(|(_, run)| run.id == run_id)
                .map(|(_, run)| run.state)
        }

        /// Whether `scope` admits the tenant owning `run_id` — the same filter
        /// every read here applies, factored out for the two counter methods,
        /// which key on the run rather than on a row of their own.
        fn visible(&self, scope: &AccessScope, run_id: Uuid) -> bool {
            self.rows.lock().unwrap().iter().any(|(tenant_id, run)| {
                run.id == run_id && scope.contains_uuid(OWNER_TENANT_ID, *tenant_id)
            })
        }

        pub(in crate::domain::service) fn execution_ref_of(&self, run_id: Uuid) -> Option<String> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .find(|(_, run)| run.id == run_id)
                .and_then(|(_, run)| run.execution_ref.clone())
        }

        pub(in crate::domain::service) fn error_of(&self, run_id: Uuid) -> Option<String> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .find(|(_, run)| run.id == run_id)
                .and_then(|(_, run)| run.error.clone())
        }

        /// Set an execution reference the way the executor would, **through the
        /// scope** rather than by reaching into the fixture.
        /// Add a run after construction, for tests that need the fixture to
        /// change between ticks.
        pub(in crate::domain::service) fn insert_for_test(&self, tenant_id: Uuid, run: Run) {
            self.rows.lock().unwrap().push((tenant_id, run));
        }

        pub(in crate::domain::service) fn seed_execution_ref(
            &self,
            run_id: Uuid,
            reference: &str,
            state: RunState,
        ) {
            let mut rows = self.rows.lock().unwrap();
            if let Some((_, run)) = rows.iter_mut().find(|(_, run)| run.id == run_id) {
                run.execution_ref = Some(reference.to_owned());
                run.state = state;
            }
        }

        pub(in crate::domain::service) fn seed_deadline(
            &self,
            run_id: Uuid,
            deadline: OffsetDateTime,
        ) {
            let mut rows = self.rows.lock().unwrap();
            if let Some((_, run)) = rows.iter_mut().find(|(_, run)| run.id == run_id) {
                run.timeout_at = Some(deadline);
            }
        }

        /// Move the counters without going through the service, so a test can
        /// put them in a state only a **race** produces.
        ///
        /// The delta race `IngestService::record_one_result` documents is not
        /// reproducible on a single-threaded `#[tokio::test]` runtime, and racing
        /// for it would be flaky even where it is. Injecting the worst state the
        /// race can reach is stronger than reproducing it: the assertion is about
        /// what completion does with skewed columns, not about how they got
        /// skewed.
        pub(in crate::domain::service) fn skew_counts(&self, run_id: Uuid, delta: &RunResultDelta) {
            let mut counters = self.counters.lock().unwrap();
            if !counters.iter().any(|(id, _)| *id == run_id) {
                counters.push((run_id, RunResult::default()));
            }
            let entry = &mut counters
                .iter_mut()
                .find(|(id, _)| *id == run_id)
                .expect("just ensured present")
                .1;
            entry.passed = bump(entry.passed, delta.passed);
            entry.failed = bump(entry.failed, delta.failed);
            entry.skipped = bump(entry.skipped, delta.skipped);
            entry.in_progress = bump(entry.in_progress, delta.in_progress);
            entry.total = bump(entry.total, delta.total);
        }

        /// The counters as `get_result` would report them, ignoring scope — for
        /// assertions, where the scope has already been exercised by the calls
        /// that produced them.
        pub(in crate::domain::service) fn counts_of(&self, run_id: Uuid) -> RunResult {
            self.counters
                .lock()
                .unwrap()
                .iter()
                .find(|(id, _)| *id == run_id)
                .map_or_else(RunResult::default, |(_, counts)| *counts)
        }

        /// The stored per-test rows for a run, newest write last.
        pub(in crate::domain::service) fn results_of(&self, run_id: Uuid) -> Vec<TestResultRow> {
            self.results
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, row)| row.run_id == run_id)
                .map(|(_, row)| row.clone())
                .collect()
        }

        pub(in crate::domain::service) fn finished_at_of(
            &self,
            run_id: Uuid,
        ) -> Option<OffsetDateTime> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .find(|(_, run)| run.id == run_id)
                .and_then(|(_, run)| run.finished_at)
        }
    }

    #[async_trait]
    impl RunsRepository for FakeRuns {
        /// Mirrors the tenant-prefixed `idx_qa_runs_tenant_name` unique index,
        /// which is what makes `RunNameExists` a collision this tenant can see
        /// rather than a cross-tenant oracle.
        async fn create<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            tenant_id: Uuid,
            new: NewRun,
        ) -> Result<Run, DomainError> {
            assert_scope_is_for(scope, "qa.run", "create");
            let mut rows = self.rows.lock().unwrap();
            if rows
                .iter()
                .any(|(owner, run)| *owner == tenant_id && run.name == new.name)
            {
                return Err(DomainError::RunNameExists { name: new.name });
            }
            self.created.lock().unwrap().push(new.clone());
            let stamp = now();
            let run = Run {
                id: Uuid::new_v4(),
                name: new.name,
                target: new.target,
                platform_id: new.platform_id,
                test_version: new.test_version,
                app_version: new.app_version,
                app_build: new.app_build,
                state: new.state,
                resolved_exclusive: new.resolved_exclusive,
                exclusive_tier: new.exclusive_tier,
                is_validation: new.is_validation,
                parameters: new.parameters,
                include_tags: new.include_tags,
                exclude_tags: new.exclude_tags,
                source: new.source,
                schedule_id: new.schedule_id,
                bundle_ids: new.bundle_ids,
                execution_ref: None,
                log_storage_ref: None,
                timeout_at: new.timeout_at,
                started_at: None,
                finished_at: None,
                error: None,
                created_at: stamp,
                updated_at: stamp,
            };
            rows.push((tenant_id, run.clone()));
            Ok(run)
        }

        async fn get<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
        ) -> Result<Option<Run>, DomainError> {
            assert_scope_is_for(scope, "qa.run", "get");
            // Which tenants this scope admits, recorded before the row is
            // looked up: a read issued under the enumeration identity and one
            // issued under the row's own tenant return the same row, so the
            // scope is the only place the difference is visible.
            self.read_scope_tenants.lock().unwrap().push((
                id,
                [OWNER_TENANT, OTHER_TENANT]
                    .into_iter()
                    .filter(|tenant| scope.contains_uuid(OWNER_TENANT_ID, *tenant))
                    .collect(),
            ));
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|(tenant_id, run)| {
                    run.id == id && scope.contains_uuid(OWNER_TENANT_ID, *tenant_id)
                })
                .map(|(_, run)| run.clone()))
        }

        /// Scope-only; the `OData` query is ignored. See
        /// `test_support::MockRunsRepository::list_page` for why a `Vec`-backed
        /// double must not pretend to translate a filter.
        ///
        /// **Paired with `RunResult::default()`, not a tracked result.** This
        /// double models admission and tenancy, neither of which reads a run's
        /// counters, so there is nothing here for a real result to pin. No test
        /// reached through this double may claim to pin `RunWithResult::result`.
        async fn list_page<C: DBRunner>(
            &self,
            runner: &C,
            scope: &AccessScope,
            _query: &ODataQuery,
        ) -> Result<Page<RunWithResult>, DomainError> {
            let runs = self.list(runner, scope).await?;
            Ok(unfiltered_page(
                runs.into_iter()
                    .map(|run| RunWithResult {
                        run,
                        result: RunResult::default(),
                    })
                    .collect(),
            ))
        }

        async fn list<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
        ) -> Result<Vec<Run>, DomainError> {
            assert_scope_is_for(scope, "qa.run", "list");
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(tenant_id, _)| scope.contains_uuid(OWNER_TENANT_ID, *tenant_id))
                .map(|(_, run)| run.clone())
                .collect())
        }

        /// The reconciler sweep, modelled: filter on the watermark, sort
        /// ascending on `(finished_at, id)`, truncate to `limit`.
        ///
        /// **Modelling a predicate, not translating a query.** `list_page`'s
        /// doc above refuses to model `$filter`/`$orderby` because doing so
        /// would be a second implementation of `paginate_odata` that could
        /// agree with the real one only by accident. This is a different case,
        /// and the Task 4 spec review adjudicated it as such: `OData` is an
        /// open-ended interpreter, whereas the sweep contract is three closed
        /// facts - one comparison, two sort keys - which a double can restate
        /// without reimplementing anything.
        ///
        /// # The consequence, demonstrated rather than assumed
        ///
        /// Because this double restates the contract instead of executing it,
        /// **it agrees with a wrong SQL implementation.** The review broke
        /// `list_finished_since` three ways - dropped the `id` sort key,
        /// changed `>=` to `>`, and flipped `finished_at` to `Desc` - and every
        /// service test reached through this method stayed green through all
        /// three. Nothing here can catch any of them.
        ///
        /// So the real-database tests are **load-bearing, not redundant**:
        /// `runs_sea_repo::tests::the_sweep_returns_runs_finished_at_or_after_the_watermark_oldest_first`,
        /// `runs_sea_repo::tests::the_sweep_breaks_a_finished_at_tie_on_id_so_the_page_boundary_is_stable`
        /// and
        /// `runs_sea_repo::tests::the_sweep_never_returns_another_tenants_finished_run`
        /// are the only coverage of the predicate, the ordering and the row-level
        /// scoping. Deleting one of them because "the service tests already cover
        /// the sweep" would leave a silent hole; the service tests cover
        /// *delegation*, and their names now say so.
        async fn list_finished_since<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            since: OffsetDateTime,
            limit: u32,
        ) -> Result<Vec<Run>, DomainError> {
            assert_scope_is_for(scope, "qa.run", "list_finished_since");
            let mut found: Vec<Run> = self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(tenant_id, run)| {
                    scope.contains_uuid(OWNER_TENANT_ID, *tenant_id)
                        && run.finished_at.is_some_and(|at| at >= since)
                })
                .map(|(_, run)| run.clone())
                .collect();
            found.sort_by_key(|run| (run.finished_at, run.id));
            found.truncate(limit as usize);
            Ok(found)
        }

        async fn update_state<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
            from: RunState,
            to: RunState,
            patch: RunStatePatch,
        ) -> Result<bool, DomainError> {
            assert_scope_is_for(scope, "qa.run", "update_state");
            let mut rows = self.rows.lock().unwrap();
            let Some((_, run)) = rows.iter_mut().find(|(tenant_id, run)| {
                run.id == id && scope.contains_uuid(OWNER_TENANT_ID, *tenant_id)
            }) else {
                return Ok(false);
            };
            if run.state != from {
                return Ok(false);
            }
            run.state = to;
            if patch.started_at.is_some() {
                run.started_at = patch.started_at;
            }
            if patch.finished_at.is_some() {
                run.finished_at = patch.finished_at;
            }
            if patch.error.is_some() {
                run.error = patch.error;
            }
            self.transitions.lock().unwrap().push((id, from, to));
            Ok(true)
        }

        async fn set_execution_ref<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
            execution_ref: &str,
        ) -> Result<bool, DomainError> {
            assert_scope_is_for(scope, "qa.run", "set_execution_ref");
            if *self.fail_execution_ref.lock().unwrap() {
                return Err(DomainError::Database(
                    "the execution reference could not be written".to_owned(),
                ));
            }
            let mut rows = self.rows.lock().unwrap();
            let Some((_, run)) = rows.iter_mut().find(|(tenant_id, run)| {
                run.id == id && scope.contains_uuid(OWNER_TENANT_ID, *tenant_id)
            }) else {
                return Ok(false);
            };
            run.execution_ref = Some(execution_ref.to_owned());
            self.execution_writes
                .lock()
                .unwrap()
                .push((id, execution_ref.to_owned()));
            Ok(true)
        }

        async fn set_bundle_ids<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
            bundle_ids: &[Uuid],
        ) -> Result<bool, DomainError> {
            assert_scope_is_for(scope, "qa.run", "set_bundle_ids");
            let mut rows = self.rows.lock().unwrap();
            let Some((_, run)) = rows.iter_mut().find(|(tenant_id, run)| {
                run.id == id && scope.contains_uuid(OWNER_TENANT_ID, *tenant_id)
            }) else {
                return Ok(false);
            };
            run.bundle_ids = bundle_ids.to_vec();
            self.bundle_writes
                .lock()
                .unwrap()
                .push((id, bundle_ids.to_vec()));
            Ok(true)
        }

        /// **Each counter is floored at zero, as the real statement floors it**
        /// (`infra::storage::runs_sea_repo::clamped_increment`). A double that
        /// let a counter go negative would make `get_result`'s corruption case
        /// reachable through a supported call and would hide a sign error in the
        /// delta arithmetic behind a plausible-looking negative number.
        async fn add_result_counts<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
            delta: RunResultDelta,
        ) -> Result<bool, DomainError> {
            assert_scope_is_for(scope, "qa.run", "add_result_counts");
            if !self.visible(scope, id) {
                return Ok(false);
            }
            self.count_deltas.lock().unwrap().push((id, delta.clone()));
            let mut counters = self.counters.lock().unwrap();
            if !counters.iter().any(|(run_id, _)| *run_id == id) {
                counters.push((id, RunResult::default()));
            }
            let entry = &mut counters
                .iter_mut()
                .find(|(run_id, _)| *run_id == id)
                .expect("just ensured present")
                .1;
            entry.passed = bump(entry.passed, delta.passed);
            entry.failed = bump(entry.failed, delta.failed);
            entry.skipped = bump(entry.skipped, delta.skipped);
            entry.in_progress = bump(entry.in_progress, delta.in_progress);
            entry.total = bump(entry.total, delta.total);
            Ok(true)
        }

        async fn get_result<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
        ) -> Result<Option<RunResult>, DomainError> {
            assert_scope_is_for(scope, "qa.run", "get_result");
            if !self.visible(scope, id) {
                return Ok(None);
            }
            Ok(Some(self.counts_of(id)))
        }

        async fn list_timeout_candidates<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            now: OffsetDateTime,
            after: Option<Uuid>,
        ) -> Result<Windowed<TimeoutCandidate>, DomainError> {
            assert_scope_is_unconstrained(scope, "list_timeout_candidates");
            // The `after` predicate and the id ordering are modelled, because
            // the sweep's coverage guarantee rests on them; the *window* is not,
            // so this double never truncates. `FakeQueue` models the whole
            // rotation for the claim scan, which is where the starvation was
            // measured.
            //
            // No tenant filter: this method is reached only through the
            // elevated, unconstrained scope asserted above, and a
            // cross-tenant read is exactly what it is for.
            let mut candidates: Vec<TimeoutCandidate> = self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, run)| {
                    matches!(run.state, RunState::Dispatching | RunState::Running)
                        && run.timeout_at.is_some_and(|deadline| deadline <= now)
                })
                .filter(|(_, run)| after.is_none_or(|after| run.id > after))
                .map(|(tenant_id, run)| TimeoutCandidate {
                    run_id: run.id,
                    tenant_id: *tenant_id,
                })
                .collect();
            candidates.sort_by_key(|candidate| candidate.run_id);
            Ok(Windowed::complete(candidates))
        }

        /// The `after` predicate, the id ordering **and** the window are all
        /// modelled here, which is where this double departs from
        /// `list_timeout_candidates` above.
        ///
        /// The window had to be, and the reason is a break-test rather than
        /// symmetry: `Windowed::complete` hardcodes `truncated: false`, so with
        /// no window the service's cursor is `None` on every tick, `after` is
        /// never given a value, and mutating the cursor advance to an
        /// unconditional `None` - the permanent starvation
        /// `list_watch_candidates`' doc says the rotation exists to prevent -
        /// left the whole suite green. The `after` predicate below was
        /// modelled and undelivered until [`FakeRuns::with_watch_scan_window`]
        /// gave it a non-`None` argument to receive.
        ///
        /// The `execution_ref.is_some()` half is modelled deliberately rather
        /// than left to the state filter. It is the whole difference between
        /// "this run holds an execution somebody should be observing" and "this
        /// run's submit never returned a handle", and a double that returned
        /// both would let the pass look correct while attaching watchers to
        /// references that do not exist.
        async fn list_watch_candidates<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            after: Option<Uuid>,
        ) -> Result<Windowed<WatchCandidate>, DomainError> {
            assert_scope_is_unconstrained(scope, "list_watch_candidates");
            // No tenant filter: this method is reached only through the
            // elevated, unconstrained scope asserted above, and a
            // cross-tenant read is exactly what it is for.
            let mut candidates: Vec<WatchCandidate> = self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, run)| {
                    matches!(run.state, RunState::Dispatching | RunState::Running)
                        && run.execution_ref.is_some()
                })
                .filter(|(_, run)| after.is_none_or(|after| run.id > after))
                .map(|(tenant_id, run)| WatchCandidate {
                    run_id: run.id,
                    tenant_id: *tenant_id,
                })
                .collect();
            candidates.sort_by_key(|candidate| candidate.run_id);
            match self.watch_scan_window {
                Some(window) => Ok(Windowed::from_overread(candidates, window)),
                None => Ok(Windowed::complete(candidates)),
            }
        }

        /// Delete-then-insert on `(test_file, test_name)`, matching the real
        /// dedupe, and **truncating the status to the column width** the way
        /// `infra::storage::runs_sea_repo` does. The truncation is mirrored on
        /// purpose: the ingest seam's job is to trim *before* this, and a double
        /// that stored the status verbatim would let a wrongly-ordered
        /// composition pass.
        async fn upsert_test_result<C: DBRunner>(
            &self,
            _runner: &C,
            results_scope: &AccessScope,
            tenant_id: Uuid,
            run: OwnedRunId,
            result: NewTestResult,
        ) -> Result<TestResultRow, DomainError> {
            assert_scope_is_for(results_scope, "qa.run", "upsert_test_result");
            self.new_results.lock().unwrap().push(result.clone());
            let stamp = now();
            let row = TestResultRow {
                id: Uuid::new_v4(),
                run_id: run.get(),
                test_file: result.test_file,
                test_name: result.test_name,
                status: crate::infra::storage::mapper::normalize_test_status(result.status),
                duration: result.duration,
                launch_id: result.launch_id,
                jira_key: result.jira_key,
                // Collapsed to `""` exactly as the real repository does - the
                // column is `NOT NULL DEFAULT ''` and `NewTestResult::nodeid`
                // says the collapse happens at the write, once.
                nodeid: result.nodeid.unwrap_or_default(),
                reason: result.reason,
                ticket: result.ticket,
                created_at: stamp,
                updated_at: stamp,
            };
            let mut results = self.results.lock().unwrap();
            results.retain(|(_, stored)| {
                stored.run_id != row.run_id
                    || stored.test_file != row.test_file
                    || stored.test_name != row.test_name
            });
            results.push((tenant_id, row.clone()));
            Ok(row)
        }

        async fn list_test_results<C: DBRunner>(
            &self,
            _runner: &C,
            results_scope: &AccessScope,
            run: OwnedRunId,
        ) -> Result<Vec<TestResultRow>, DomainError> {
            assert_scope_is_for(results_scope, "qa.run", "list_test_results");
            Ok(self
                .results
                .lock()
                .unwrap()
                .iter()
                .filter(|(tenant_id, stored)| {
                    stored.run_id == run.get()
                        && results_scope.contains_uuid(OWNER_TENANT_ID, *tenant_id)
                })
                .map(|(_, stored)| stored.clone())
                .collect())
        }
    }

    /// Apply a signed delta to a `usize` counter, floored at zero.
    fn bump(current: usize, delta: i64) -> usize {
        if delta >= 0 {
            return current.saturating_add(usize::try_from(delta).unwrap_or(usize::MAX));
        }
        current.saturating_sub(usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX))
    }

    // -----------------------------------------------------------------------
    // Queue repository
    // -----------------------------------------------------------------------

    /// In-memory `QueueRepository` modelling the seven states and their guards.
    #[derive(Default)]
    pub(in crate::domain::service) struct FakeQueue {
        rows: Mutex<Vec<QueueRowRecord>>,
        /// When set, `insert` fails with this error the first time it is called.
        fail_insert_once: Mutex<Option<DomainError>>,
        /// When set, `claims_for_platform` answers empty — the shape in which the
        /// drain cannot recover a just-claimed row's `run_id`, which is the only way
        /// to reach `claim_batch`'s deliberately-abandon-to-the-orphan-guard arm.
        pub(in crate::domain::service) hide_claims: Mutex<bool>,
        /// Every `after` value `all_claims` was called with, in order.
        ///
        /// The tick issues two claim scans and they **must** see the same
        /// window - `committed_active` looks the cap pass's rows up in the
        /// reconciliation pass's classification. Recording the arguments is the
        /// only way to observe that from outside; the returned rows look
        /// plausible either way.
        pub(in crate::domain::service) claim_scan_cursors: Mutex<Vec<Option<Uuid>>>,
        /// `all_claims`' window, in rows. `None` returns everything visible.
        ///
        /// The real window is `MAX_CLAIM_SCAN` rows, which would mean a
        /// thousand-row fixture per test; a small window here reaches the same
        /// states for the same reasons. What it does **not** cover is the SQL -
        /// the `WHERE id > ?`, the `ORDER BY id` and the `LIMIT` are pinned
        /// against a real database by `infra::storage::queue_sea_repo`'s own
        /// tests.
        claim_scan_window: Option<usize>,
    }

    impl FakeQueue {
        pub(in crate::domain::service) fn with(rows: Vec<QueueRowRecord>) -> Self {
            Self {
                rows: Mutex::new(rows),
                ..Self::default()
            }
        }

        /// [`Self::with`], with `all_claims` windowed to `window` rows so a test
        /// can drive the scan rotation.
        pub(in crate::domain::service) fn with_claim_scan_window(
            rows: Vec<QueueRowRecord>,
            window: usize,
        ) -> Self {
            Self {
                rows: Mutex::new(rows),
                claim_scan_window: Some(window),
                ..Self::default()
            }
        }

        pub(in crate::domain::service) fn failing_insert(error: DomainError) -> Self {
            Self {
                fail_insert_once: Mutex::new(Some(error)),
                ..Self::default()
            }
        }

        /// Add a row after construction - see `FakeRuns::insert_for_test`.
        pub(in crate::domain::service) fn insert_for_test(&self, row: QueueRowRecord) {
            self.rows.lock().unwrap().push(row);
        }

        pub(in crate::domain::service) fn rows(&self) -> Vec<QueueRowRecord> {
            self.rows.lock().unwrap().clone()
        }

        pub(in crate::domain::service) fn state_of(&self, id: Uuid) -> Option<QueueState> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.id == id)
                .map(|row| row.state)
        }

        fn visible(&self, scope: &AccessScope) -> Vec<QueueRowRecord> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .filter(|row| scope.contains_uuid(OWNER_TENANT_ID, row.tenant_id))
                .cloned()
                .collect()
        }

        /// [`Self::visible`]'s cross-tenant counterpart, for the two nil-tenant
        /// enumeration reads (`platforms_with_queued_rows`, `all_claims`) —
        /// a separate call path rather than widening `visible` itself, which
        /// every tenant-bound method above still relies on to filter by scope.
        fn all_rows(&self) -> Vec<QueueRowRecord> {
            self.rows.lock().unwrap().clone()
        }

        fn set_state(
            &self,
            scope: &AccessScope,
            id: Uuid,
            to: QueueState,
            guard: Option<QueueState>,
            error: Option<&str>,
        ) -> bool {
            let mut rows = self.rows.lock().unwrap();
            let Some(row) = rows
                .iter_mut()
                .find(|row| row.id == id && scope.contains_uuid(OWNER_TENANT_ID, row.tenant_id))
            else {
                return false;
            };
            if guard.is_some_and(|guard| row.state != guard) {
                return false;
            }
            row.state = to;
            if to == QueueState::Dispatching {
                row.dispatched_at = Some(now());
            }
            if let Some(text) = error {
                row.error = Some(text.to_owned());
            }
            true
        }
    }

    #[async_trait]
    impl QueueRepository for FakeQueue {
        async fn insert<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            tenant_id: Uuid,
            row: NewQueueRow,
        ) -> Result<QueueRowRecord, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "insert");
            if let Some(error) = self.fail_insert_once.lock().unwrap().take() {
                return Err(error);
            }
            let mut rows = self.rows.lock().unwrap();
            // The tenant-prefixed unique index `idx_qa_run_queue_tenant_run`.
            if rows
                .iter()
                .any(|existing| existing.tenant_id == tenant_id && existing.run_id == row.run.get())
            {
                return Err(DomainError::QueueRowExists {
                    run_id: row.run.get(),
                });
            }
            let state = match row.decision {
                AdmissionDecision::Dispatch => QueueState::Dispatching,
                AdmissionDecision::Queue => QueueState::Queued,
            };
            let record = QueueRowRecord {
                id: Uuid::new_v4(),
                tenant_id,
                run_id: row.run.get(),
                platform_id: row.platform_id,
                run_kind: row.run_kind,
                source: row.source,
                exclusive: row.exclusive,
                state,
                error: None,
                enqueued_at: now(),
                dispatched_at: (state == QueueState::Dispatching).then(now),
                finished_at: None,
            };
            rows.push(record.clone());
            Ok(record)
        }

        /// # The `yield_now` is load-bearing, and it is here rather than in a test
        ///
        /// The admission race the platform lock exists to close is "two concurrent
        /// launches against a queue one slot from full must both not observe room"
        /// (`manager/src/services/run_queue.rs:635-639`). Reaching it needs the two
        /// admissions to **interleave between the depth read and the insert** — and
        /// on the single-threaded runtime `#[tokio::test]` provides, every await in
        /// these doubles is immediately ready, so a spawned admission runs start to
        /// finish without ever yielding. A concurrency test written against such a
        /// double is serialised by the runtime and passes whether the lock is there
        /// or not.
        ///
        /// This was found by break-testing: removing the platform lock entirely left
        /// `two_concurrent_exclusive_launches_produce_one_start_and_one_queued`
        /// green. One yield makes the race reachable, and
        /// `two_concurrent_launches_cannot_both_observe_room_in_a_full_queue` then
        /// discriminates the lock.
        ///
        /// **The yield is *after* the count and before the return, and the order is
        /// the whole point.** Yielding first only interleaves the two *entries* into
        /// this method: the second task then reads after the first has already
        /// inserted, sees the true depth, and is correctly refused — lock or no lock,
        /// which is a second way the same test can be vacuous. Yielding after the
        /// count is what lets the second task hold a **stale** depth across the
        /// first's insert, which is exactly the race
        /// `run_queue.rs:635-639` describes. Both orders were tried; only this one
        /// reddens when the lock is removed.
        async fn queued_depth<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            platform_id: Uuid,
        ) -> Result<usize, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "queued_depth");
            let depth = self
                .visible(scope)
                .iter()
                .filter(|row| row.platform_id == platform_id && row.state == QueueState::Queued)
                .count();
            tokio::task::yield_now().await;
            Ok(depth)
        }

        async fn queued_rows<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            platform_id: Uuid,
        ) -> Result<Vec<QueuedRow>, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "queued_rows");
            let mut rows: Vec<QueueRowRecord> = self
                .visible(scope)
                .into_iter()
                .filter(|row| row.platform_id == platform_id && row.state == QueueState::Queued)
                .collect();
            // `ORDER BY enqueued_at ASC, id ASC` (`run_queue.rs:243-257`).
            rows.sort_by(|a, b| a.enqueued_at.cmp(&b.enqueued_at).then(a.id.cmp(&b.id)));
            Ok(rows
                .into_iter()
                .map(|row| QueuedRow {
                    id: row.id,
                    exclusive: row.exclusive,
                })
                .collect())
        }

        async fn claims_for_platform<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            platform_id: Uuid,
        ) -> Result<Vec<ClaimRow>, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "claims_for_platform");
            if *self.hide_claims.lock().unwrap() {
                return Ok(Vec::new());
            }
            Ok(self
                .visible(scope)
                .into_iter()
                .filter(|row| {
                    row.platform_id == platform_id
                        && matches!(row.state, QueueState::Dispatching | QueueState::Running)
                })
                .map(|row| ClaimRow {
                    id: row.id,
                    run_id: row.run_id,
                    exclusive: row.exclusive,
                })
                .collect())
        }

        async fn platforms_with_queued_rows<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
        ) -> Result<Vec<QueuedPlatform>, DomainError> {
            assert_scope_is_unconstrained(scope, "platforms_with_queued_rows");
            let mut out: Vec<QueuedPlatform> = Vec::new();
            for row in self.all_rows() {
                if row.state != QueueState::Queued {
                    continue;
                }
                let entry = QueuedPlatform {
                    platform_id: row.platform_id,
                    tenant_id: row.tenant_id,
                };
                if !out.iter().any(|seen| {
                    seen.platform_id == entry.platform_id && seen.tenant_id == entry.tenant_id
                }) {
                    out.push(entry);
                }
            }
            // **Deliberately unsorted, and this comment is the point.** The real
            // query is a `GROUP BY platform_id, tenant_id` with no `ORDER BY`
            // (`queue_sea_repo`), so drain order is planner-defined. An earlier
            // version of this double sorted by `platform_id`, which handed every
            // drain test a determinism production does not have — so neither the
            // order-dependence of the threaded budget nor cross-tenant competition
            // for it was observable. Insertion order here is the row order the
            // fixture supplied, which is no more meaningful than the planner's and
            // at least does not pretend to be.
            Ok(out)
        }

        async fn mark_dispatching<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
        ) -> Result<bool, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "mark_dispatching");
            Ok(self.set_state(
                scope,
                id,
                QueueState::Dispatching,
                Some(QueueState::Queued),
                None,
            ))
        }

        /// `dispatched_at` is cleared, as the real implementation clears it, so a
        /// requeued row that is claimed again is aged from its *second* claim.
        async fn requeue<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
        ) -> Result<bool, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "requeue");
            let requeued = self.set_state(
                scope,
                id,
                QueueState::Queued,
                Some(QueueState::Dispatching),
                None,
            );
            if requeued {
                let mut rows = self.rows.lock().unwrap();
                if let Some(row) = rows.iter_mut().find(|row| row.id == id) {
                    row.dispatched_at = None;
                }
            }
            Ok(requeued)
        }

        async fn mark_running<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
        ) -> Result<bool, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "mark_running");
            Ok(self.set_state(scope, id, QueueState::Running, None, None))
        }

        async fn mark_failed<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
            error: &str,
        ) -> Result<bool, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "mark_failed");
            Ok(self.set_state(scope, id, QueueState::Failed, None, Some(error)))
        }

        async fn mark_done<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
        ) -> Result<bool, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "mark_done");
            Ok(self.set_state(scope, id, QueueState::Done, None, None))
        }

        async fn cancel_queued<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
            reason: &str,
        ) -> Result<bool, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "cancel_queued");
            Ok(self.set_state(
                scope,
                id,
                QueueState::Cancelled,
                Some(QueueState::Queued),
                Some(reason),
            ))
        }

        /// Scope plus the `platform_id` narrowing, newest first. The `OData`
        /// query is ignored - see `FakeRuns::list_page`.
        async fn list_page<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            platform_id: Option<Uuid>,
            _query: &ODataQuery,
        ) -> Result<Page<QueueRowRecord>, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "list_page");
            let mut rows: Vec<QueueRowRecord> = self
                .visible(scope)
                .into_iter()
                .filter(|row| platform_id.is_none_or(|id| row.platform_id == id))
                .collect();
            rows.sort_by(|a, b| b.enqueued_at.cmp(&a.enqueued_at).then(b.id.cmp(&a.id)));
            Ok(unfiltered_page(rows))
        }

        /// **The whole rotation, not a stand-in for it.** `after` filters, `id`
        /// orders, and `claim_scan_window` truncates - so a test can drive
        /// several scans and observe that they cover different rows and wrap.
        /// That is the property F1 turned on, and a double that only modelled
        /// the truncation flag could not express it.
        async fn all_claims<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            after: Option<Uuid>,
        ) -> Result<Windowed<ClaimAge>, DomainError> {
            assert_scope_is_unconstrained(scope, "all_claims");
            self.claim_scan_cursors.lock().unwrap().push(after);
            let mut rows: Vec<ClaimAge> = self
                .all_rows()
                .into_iter()
                .filter(|row| matches!(row.state, QueueState::Dispatching | QueueState::Running))
                .filter(|row| after.is_none_or(|after| row.id > after))
                .map(|row| ClaimAge {
                    id: row.id,
                    tenant_id: row.tenant_id,
                    run_id: row.run_id,
                    platform_id: row.platform_id,
                    // `dispatched_at` falling back to `enqueued_at`
                    // (`run_queue.rs:341-346`).
                    age_basis: row.dispatched_at.unwrap_or(row.enqueued_at),
                })
                .collect();
            rows.sort_by_key(|claim| claim.id);
            match self.claim_scan_window {
                Some(window) => Ok(Windowed::from_overread(rows, window)),
                None => Ok(Windowed::complete(rows)),
            }
        }

        async fn expire_queued_before<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            cutoff: OffsetDateTime,
            reason: &str,
        ) -> Result<Vec<ExpiredRow>, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "expire_queued_before");
            let mut expired = Vec::new();
            let mut rows = self.rows.lock().unwrap();
            for row in rows.iter_mut() {
                if !scope.contains_uuid(OWNER_TENANT_ID, row.tenant_id) {
                    continue;
                }
                // `state = 'queued'` only: such a row holds no claim, so
                // expiring it cannot release a platform a live execution owns.
                if row.state != QueueState::Queued || row.enqueued_at >= cutoff {
                    continue;
                }
                row.state = QueueState::Expired;
                row.error = Some(reason.to_owned());
                expired.push(ExpiredRow {
                    id: row.id,
                    tenant_id: row.tenant_id,
                    run_id: row.run_id,
                    platform_id: row.platform_id,
                    exclusive: row.exclusive,
                    enqueued_at: row.enqueued_at,
                });
            }
            Ok(expired)
        }

        async fn fail_orphaned_dispatching<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            ids: &[Uuid],
            reason: &str,
        ) -> Result<u64, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "fail_orphaned_dispatching");
            let mut failed = 0u64;
            for id in ids {
                // `AND state = 'dispatching'` is the half only SQL can enforce.
                if self.set_state(
                    scope,
                    *id,
                    QueueState::Failed,
                    Some(QueueState::Dispatching),
                    Some(reason),
                ) {
                    failed += 1;
                }
            }
            Ok(failed)
        }

        /// Newest first, all states, clamped — the real query's shape
        /// (`:454-483`). The clamp is mirrored rather than ignored because
        /// `service::runs` reads a queue id's run id through this window and
        /// documents the ceiling as a real limitation; a double with no ceiling
        /// would make that paragraph untestable.
        async fn list_for_read<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            platform_id: Option<Uuid>,
            limit: u64,
        ) -> Result<Vec<QueueRowRecord>, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "list_for_read");
            let mut rows: Vec<QueueRowRecord> = self
                .visible(scope)
                .into_iter()
                .filter(|row| platform_id.is_none_or(|id| row.platform_id == id))
                .collect();
            rows.sort_by(|a, b| b.enqueued_at.cmp(&a.enqueued_at).then(b.id.cmp(&a.id)));
            let window = usize::try_from(limit.min(crate::domain::repos::MAX_QUEUE_READ_LIMIT))
                .unwrap_or(usize::MAX);
            rows.truncate(window);
            Ok(rows)
        }

        async fn row_status<C: DBRunner>(
            &self,
            _runner: &C,
            scope: &AccessScope,
            id: Uuid,
        ) -> Result<Option<RowStatus>, DomainError> {
            assert_scope_is_for(scope, "qa.queue_entry", "row_status");
            Ok(self
                .visible(scope)
                .into_iter()
                .find(|row| row.id == id)
                .map(|row| RowStatus {
                    platform_id: row.platform_id,
                    state: row.state,
                }))
        }
    }

    // -----------------------------------------------------------------------
    // qa-environments
    // -----------------------------------------------------------------------

    /// The lease, plus the two failure directions that matter.
    #[derive(Default)]
    pub(in crate::domain::service) struct FakeEnvironments {
        leases: Mutex<Vec<(Uuid, LeaseState)>>,
        /// Tenants whose `get_environment` calls are answered. Everything else is
        /// not-found, which is what makes the platform read a tenancy check.
        platform_tenants: Vec<Uuid>,
        pub(in crate::domain::service) fail_get_lease: Mutex<bool>,
        pub(in crate::domain::service) busy_on_acquire: Mutex<bool>,
        pub(in crate::domain::service) fail_acquire: Mutex<bool>,
        pub(in crate::domain::service) acquires: Mutex<Vec<(Uuid, Uuid, LeaseMode)>>,
        pub(in crate::domain::service) releases: Mutex<Vec<(Uuid, Uuid)>>,
        /// Tenants each `get_environment`/`list_variables` call was made under, so a
        /// test can prove a background write was bound to the row's tenant.
        pub(in crate::domain::service) tenants_seen: Mutex<Vec<Uuid>>,
        /// The environment `get_environment` answers with, when a test needs one
        /// this double's synthetic default cannot express. `None` keeps that
        /// default — see [`FakeEnvironments::serving`].
        environment: Option<Environment>,
        /// What `list_variables` answers. Empty for every test that does not
        /// care.
        variables: Vec<Variable>,
    }

    impl FakeEnvironments {
        pub(in crate::domain::service) fn free() -> Self {
            Self {
                platform_tenants: vec![OWNER_TENANT, OTHER_TENANT],
                ..Self::default()
            }
        }

        /// [`Self::free`], answering with a caller-supplied environment and
        /// variable list.
        ///
        /// `free`'s environment is the one a *never-observed* platform holds:
        /// no base URL, no namespace, and `list_variables` answers empty. The
        /// golden `RunSpec` fixture needs the opposite — an observed platform
        /// with both variable tiers populated — and there is no way to say that
        /// through a double whose answer is a literal.
        ///
        /// The id the caller asks for still wins over the fixture's own, so one
        /// seeded environment serves whichever platform id a test drives.
        pub(in crate::domain::service) fn serving(
            environment: Environment,
            variables: Vec<Variable>,
        ) -> Self {
            Self {
                environment: Some(environment),
                variables,
                ..Self::free()
            }
        }

        pub(in crate::domain::service) fn holding(platform_id: Uuid, state: LeaseState) -> Self {
            Self {
                leases: Mutex::new(vec![(platform_id, state)]),
                platform_tenants: vec![OWNER_TENANT, OTHER_TENANT],
                ..Self::default()
            }
        }

        pub(in crate::domain::service) fn unreadable() -> Self {
            Self {
                platform_tenants: vec![OWNER_TENANT, OTHER_TENANT],
                fail_get_lease: Mutex::new(true),
                ..Self::default()
            }
        }

        pub(in crate::domain::service) fn busy_on_acquire() -> Self {
            Self {
                platform_tenants: vec![OWNER_TENANT, OTHER_TENANT],
                busy_on_acquire: Mutex::new(true),
                ..Self::default()
            }
        }

        pub(in crate::domain::service) fn acquired_modes(&self) -> Vec<LeaseMode> {
            self.acquires
                .lock()
                .unwrap()
                .iter()
                .map(|(_, _, mode)| *mode)
                .collect()
        }

        pub(in crate::domain::service) fn released(&self) -> Vec<(Uuid, Uuid)> {
            self.releases.lock().unwrap().clone()
        }
    }

    /// The environment every dispatch test gets unless it says otherwise: a
    /// platform that has been detected but never observed through the plugin
    /// path, owned by [`PRODUCT`].
    ///
    /// Extracted from `get_environment` at Task 18 so a test can vary one
    /// field — `product_id: None`, a different credstore reference — with
    /// `..environment_fixture(id)` instead of restating thirty.
    #[must_use]
    pub(in crate::domain::service) fn environment_fixture(id: Uuid) -> Environment {
        let stamp = now();
        Environment {
            id,
            name: "staging".to_owned(),
            // **Since Task 18 every environment a run targets must name a
            // product**, because that is what resolves the plugin that
            // says how to reach it (**D6**: there is no fallback path). An
            // environment with `None` here fails its runs' dispatch, which
            // `a_run_whose_environment_names_no_product_cannot_dispatch`
            // is what covers — so this default is `Some`.
            product_id: PRODUCT,
            description: None,
            available: true,
            observed_version: Some("7.1".to_owned()),
            observed_build: None,
            default_branch: None,
            is_default: false,
            version_detect_error: None,
            version_detected_at: None,
            // Nothing has been observed through the plugin path (qa-environments
            // Task 14): every value is the one a never-observed environment holds.
            // **Keyed with the plugin double's own key.** Before Task 19 this
            // was an empty list beside `kubeconfig_credstore_ref`, and dispatch
            // derived the key from `sole_required_secret_key(schema)`. The
            // column is gone (ruling F-2), so the key comes from the row, and
            // an empty list here means "this environment stores no credential"
            // -- a real state, but not this fixture's.
            credentials: vec![qa_environments_sdk::EnvironmentCredential {
                key: PLUGIN_SECRET_KEY.to_owned(),
                credstore_ref: "credstore://kubeconfig".to_owned(),
            }],
            observed_attrs: qa_environments_sdk::ObservedAttrs::default(),
            config: serde_json::json!({}),
            observed_base_url: None,
            health_state: qa_environments_sdk::HealthState::Unknown,
            health_detail: None,
            health_checked_at: None,
            created_at: stamp,
            updated_at: stamp,
        }
    }

    fn env_unsupported(method: &str) -> QaEnvironmentsError {
        QaEnvironmentsError::internal(format!(
            "FakeEnvironments::{method} is not used by these tests"
        ))
        .create()
    }

    #[async_trait]
    impl QaEnvironmentsClientV1 for FakeEnvironments {
        /// **Tenant-filtered**, like `test_support::MockEnvironments::get_environment`
        /// and unlike this double's first version, which answered a synthetic
        /// platform for any id under any tenant — so `build_spec`'s platform read
        /// was not a tenancy check in any dispatch test. Not-found and forbidden are
        /// indistinguishable, which is what the real gear answers and what keeps
        /// this from being a cross-tenant existence oracle.
        ///
        /// The tenant admitted is whichever one the fixture platforms were seeded
        /// for; [`FakeEnvironments::free`] seeds both fixture tenants, because the
        /// dispatch tests drive two of them.
        async fn get_environment(
            &self,
            ctx: &SecurityContext,
            id: Uuid,
        ) -> Result<Environment, QaEnvironmentsError> {
            self.tenants_seen
                .lock()
                .unwrap()
                .push(ctx.subject_tenant_id());
            let tenant = ctx.subject_tenant_id();
            if !self.platform_tenants.contains(&tenant) {
                return Err(
                    QaEnvironmentsError::internal(format!("platform {id} not found")).create(),
                );
            }
            if let Some(environment) = &self.environment {
                return Ok(Environment {
                    id,
                    ..environment.clone()
                });
            }
            Ok(environment_fixture(id))
        }

        async fn list_environments(
            &self,
            _ctx: &SecurityContext,
        ) -> Result<Vec<Environment>, QaEnvironmentsError> {
            Err(env_unsupported("list_environments"))
        }

        async fn create_environment(
            &self,
            _ctx: &SecurityContext,
            _new: NewEnvironment,
        ) -> Result<Environment, QaEnvironmentsError> {
            Err(env_unsupported("create_environment"))
        }

        async fn update_environment(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
            _patch: EnvironmentPatch,
        ) -> Result<Environment, QaEnvironmentsError> {
            Err(env_unsupported("update_environment"))
        }

        async fn delete_environment(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<(), QaEnvironmentsError> {
            Err(env_unsupported("delete_environment"))
        }

        /// Answers the configured list **unfiltered**, which is not quite what
        /// the real gear does — it returns the global rows plus the requested
        /// platform's. The difference is unobservable here because
        /// `runvars::split_by_scope` drops a row scoped to any other platform
        /// anyway (that is the defence-in-depth its own doc describes), so a
        /// fixture says which tier a variable is in through the row's
        /// `environment_id` rather than through this argument.
        async fn list_variables(
            &self,
            ctx: &SecurityContext,
            _platform_id: Option<Uuid>,
        ) -> Result<Vec<Variable>, QaEnvironmentsError> {
            self.tenants_seen
                .lock()
                .unwrap()
                .push(ctx.subject_tenant_id());
            Ok(self.variables.clone())
        }

        async fn upsert_variable(
            &self,
            _ctx: &SecurityContext,
            _var: NewVariable,
        ) -> Result<Variable, QaEnvironmentsError> {
            Err(env_unsupported("upsert_variable"))
        }

        async fn delete_variable(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<(), QaEnvironmentsError> {
            Err(env_unsupported("delete_variable"))
        }

        async fn acquire_lease(
            &self,
            ctx: &SecurityContext,
            platform_id: Uuid,
            run_id: Uuid,
            mode: LeaseMode,
        ) -> Result<AcquireOutcome, QaEnvironmentsError> {
            self.tenants_seen
                .lock()
                .unwrap()
                .push(ctx.subject_tenant_id());
            if *self.fail_acquire.lock().unwrap() {
                return Err(env_unsupported("acquire_lease"));
            }
            self.acquires
                .lock()
                .unwrap()
                .push((platform_id, run_id, mode));
            if *self.busy_on_acquire.lock().unwrap() {
                return Ok(AcquireOutcome::Busy {
                    current: LeaseState::HeldExclusive {
                        holder: Uuid::from_u128(0xF00D),
                    },
                });
            }
            let mut leases = self.leases.lock().unwrap();
            let current = leases
                .iter()
                .find(|(id, _)| *id == platform_id)
                .map_or(LeaseState::Free, |(_, state)| state.clone());
            // The compare-and-swap, modelled rather than assumed. A double that
            // granted every request would make the CAS invisible, and the CAS is
            // this gear's source of truth: `domain::queue`'s planners are advisory
            // precisely because it can refuse them.
            // **Mirrors `qa_environments::domain::lease::decide_acquire` arm for
            // arm** (`qa-environments/src/domain/lease.rs:18-53`), including its two
            // narrow refusals, because the concurrency conclusions in this file rest
            // on the double's CAS being the real one.
            //
            // An earlier version was more permissive in exactly two states, neither
            // reachable through these tests and both wrong anyway:
            // `(HeldParallel { holders: [] }, Exclusive)` admitted here and is
            // `Busy` in reality — the real function has no empty-holder special case,
            // so an empty parallel hold blocks an exclusive acquire even though
            // `Occupancy::from_lease` reads it as `Free`; and
            // `(HeldExclusive { holder == run_id }, Parallel)` admitted here, where
            // the real same-run re-acquire guard is `Exclusive`-only.
            let admits = match (&current, mode) {
                // Nothing holds it, or a parallel run joining parallel holders.
                // Two cases, one answer: merged because `clippy::match_same_arms`
                // is denied, not because they are the same rule.
                (LeaseState::Free, _) | (LeaseState::HeldParallel { .. }, LeaseMode::Parallel) => {
                    true
                }
                // Re-acquiring one's own exclusive hold is idempotent.
                (LeaseState::HeldExclusive { holder }, LeaseMode::Exclusive) => *holder == run_id,
                // Everything else waits: an exclusive acquire against any parallel
                // hold, and any acquire against another run's exclusive hold.
                (LeaseState::HeldParallel { .. } | LeaseState::HeldExclusive { .. }, _) => false,
            };
            if !admits {
                return Ok(AcquireOutcome::Busy { current });
            }
            let state = match mode {
                LeaseMode::Exclusive => LeaseState::HeldExclusive { holder: run_id },
                LeaseMode::Parallel => match &current {
                    LeaseState::HeldParallel { holders } => {
                        let mut holders = holders.clone();
                        if !holders.contains(&run_id) {
                            holders.push(run_id);
                        }
                        LeaseState::HeldParallel { holders }
                    }
                    _ => LeaseState::HeldParallel {
                        holders: vec![run_id],
                    },
                },
            };
            leases.retain(|(id, _)| *id != platform_id);
            leases.push((platform_id, state));
            Ok(AcquireOutcome::Acquired)
        }

        async fn release_lease(
            &self,
            ctx: &SecurityContext,
            platform_id: Uuid,
            run_id: Uuid,
        ) -> Result<LeaseState, QaEnvironmentsError> {
            self.tenants_seen
                .lock()
                .unwrap()
                .push(ctx.subject_tenant_id());
            self.releases.lock().unwrap().push((platform_id, run_id));
            // **Mirrors `decide_release`** (`qa-environments/src/domain/lease.rs:57-70`):
            // idempotent for a run that does not hold the lease, and removing only
            // *this* holder from a parallel hold. The first version dropped the whole
            // lease regardless of `run_id`, which was permissive in the same
            // direction the `acquire_lease` mirror was before it was fixed — a
            // non-holder's release freed a platform someone else owned.
            let mut leases = self.leases.lock().unwrap();
            let current = leases
                .iter()
                .find(|(id, _)| *id == platform_id)
                .map_or(LeaseState::Free, |(_, state)| state.clone());
            let next = match &current {
                LeaseState::Free => LeaseState::Free,
                LeaseState::HeldExclusive { holder } if *holder == run_id => LeaseState::Free,
                LeaseState::HeldExclusive { holder } => {
                    LeaseState::HeldExclusive { holder: *holder }
                }
                LeaseState::HeldParallel { holders } => {
                    let remaining: Vec<Uuid> =
                        holders.iter().copied().filter(|h| *h != run_id).collect();
                    if remaining.is_empty() {
                        LeaseState::Free
                    } else {
                        LeaseState::HeldParallel { holders: remaining }
                    }
                }
            };
            leases.retain(|(id, _)| *id != platform_id);
            if next != LeaseState::Free {
                leases.push((platform_id, next.clone()));
            }
            Ok(next)
        }

        async fn get_lease(
            &self,
            ctx: &SecurityContext,
            platform_id: Uuid,
        ) -> Result<LeaseState, QaEnvironmentsError> {
            self.tenants_seen
                .lock()
                .unwrap()
                .push(ctx.subject_tenant_id());
            if *self.fail_get_lease.lock().unwrap() {
                return Err(env_unsupported("get_lease"));
            }
            Ok(self
                .leases
                .lock()
                .unwrap()
                .iter()
                .find(|(id, _)| *id == platform_id)
                .map_or(LeaseState::Free, |(_, state)| state.clone()))
        }
    }

    // -----------------------------------------------------------------------
    // The product plugin, and the resolver that finds it
    // -----------------------------------------------------------------------

    /// A product plugin shaped like a real one, product-neutrally.
    ///
    /// **Not a copy of `qa-vhp-product-plugin`.** It declares one required
    /// secret (so `sole_required_secret_key` resolves), mounts it, and returns
    /// the one variable that names the mount — the minimum a dispatch needs.
    /// Its variables are deliberately *not* VHP's: a double that reproduced
    /// VHP's four names would let a dispatch test "prove" the fixture by
    /// agreeing with itself, which is the defect the Phase D review found in
    /// `products_tests`' mock. The real plugin's own behaviour is asserted
    /// end to end in `golden_run_spec_tests`, against the real crate.
    pub(in crate::domain::service) struct ScriptedPlugin {
        /// Extra variables `prepare_run_access` returns, on top of the one
        /// naming the mount.
        pub(in crate::domain::service) extra_env: Vec<(String, String)>,
        /// Names `env_contract` reserves on top of the platform floor.
        pub(in crate::domain::service) reserved: Vec<String>,
        /// When set, `prepare_run_access` refuses with this classified detail.
        pub(in crate::domain::service) refuse: Option<&'static str>,
        /// A second required secret, so `sole_required_secret_key` resolves to
        /// nothing and the legacy single-reference column cannot be keyed.
        pub(in crate::domain::service) two_required_secrets: bool,
        /// The runner shape this double declares.
        ///
        /// **Non-default by default**, and that is the whole point: every real
        /// plugin today returns `RunnerSpec::default()` and no service
        /// account, so a double that did the same made
        /// `runner: plugin.runner(observed)` and the `service_account`
        /// hand-off unpinnable — cutting either wire left the entire suite and
        /// the golden fixture green (review finding C-1, proved by mutation).
        pub(in crate::domain::service) runner: crate::domain::ports::run_executor::RunnerSpec,
        /// The service account this double asks for, for the same reason.
        pub(in crate::domain::service) service_account: Option<String>,
        /// Every handle `prepare_run_access` was called with, flattened to
        /// what a test can assert on — see [`RecordedHandle`].
        pub(in crate::domain::service) handles: Mutex<Vec<RecordedHandle>>,
    }

    /// The runner image this double declares — distinguishable from the
    /// deployment default (`vhp-test-runner:latest` in the Argo config) so a
    /// spec carrying one can be told from a spec carrying the other.
    pub(in crate::domain::service) const PLUGIN_RUNNER_IMAGE: &str = "scripted-runner:9";
    /// The service account this double asks for.
    pub(in crate::domain::service) const PLUGIN_SERVICE_ACCOUNT: &str = "scripted-runner-sa";

    impl Default for ScriptedPlugin {
        fn default() -> Self {
            Self {
                extra_env: Vec::new(),
                reserved: Vec::new(),
                refuse: None,
                two_required_secrets: false,
                runner: crate::domain::ports::run_executor::RunnerSpec {
                    image: Some(PLUGIN_RUNNER_IMAGE.to_owned()),
                    command: vec!["/scripted.sh".to_owned()],
                    image_pull_policy: Some("Always".to_owned()),
                },
                service_account: Some(PLUGIN_SERVICE_ACCOUNT.to_owned()),
                handles: Mutex::new(Vec::new()),
            }
        }
    }

    /// One `prepare_run_access` call's handle, as much of it as outlives the
    /// call.
    ///
    /// `EnvironmentHandle` borrows, so a double cannot keep one; what a test
    /// needs to assert is which slots it was given (key and **reference** —
    /// never a value, which is the point), the config verbatim, and whether an
    /// observation was present at all.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(in crate::domain::service) struct RecordedHandle {
        pub(in crate::domain::service) slots: Vec<(String, String)>,
        pub(in crate::domain::service) config: serde_json::Value,
        pub(in crate::domain::service) observed: bool,
    }

    /// The credential key this double declares, mirroring the shape every real
    /// plugin has: exactly one required secret.
    pub(in crate::domain::service) const PLUGIN_SECRET_KEY: &str = "scripted_credential";
    /// Where this double mounts it, and the value of the variable that names
    /// it. One constant, two sites, as a real plugin does it.
    pub(in crate::domain::service) const PLUGIN_MOUNT_PATH: &str = "/.kube/kubeconfig";

    fn secret_field(key: &str) -> qa_product_sdk::descriptor::FieldDesc {
        qa_product_sdk::descriptor::FieldDesc {
            key: key.to_owned(),
            label: key.to_owned(),
            kind: qa_product_sdk::descriptor::FieldKind::MultilineSecret,
            required: true,
            role: None,
            in_table: false,
            in_detail: false,
            help: None,
        }
    }

    #[async_trait]
    impl qa_product_sdk::QaProductPluginV1 for ScriptedPlugin {
        fn credential_schema(&self) -> Vec<qa_product_sdk::descriptor::FieldDesc> {
            let mut schema = vec![secret_field(PLUGIN_SECRET_KEY)];
            if self.two_required_secrets {
                schema.push(secret_field("second_credential"));
            }
            schema
        }

        fn observed_schema(&self) -> Vec<qa_product_sdk::descriptor::FieldDesc> {
            Vec::new()
        }

        async fn validate_credentials(
            &self,
            _input: &qa_product_sdk::plugin::CredentialInput,
        ) -> Result<
            Vec<qa_product_sdk::plugin::CredentialClassification>,
            qa_product_sdk::observation::PluginFailure,
        > {
            unreachable!("dispatch never validates a credential form")
        }

        async fn observe(
            &self,
            _env: &qa_product_sdk::plugin::EnvironmentHandle<'_>,
        ) -> qa_product_sdk::observation::PluginObservation {
            unreachable!("dispatch never observes")
        }

        async fn prepare_run_access(
            &self,
            env: &qa_product_sdk::plugin::EnvironmentHandle<'_>,
        ) -> Result<
            crate::domain::ports::run_executor::RunAccess,
            qa_product_sdk::observation::PluginFailure,
        > {
            self.handles.lock().unwrap().push(RecordedHandle {
                slots: env
                    .slots
                    .iter()
                    .map(|slot| (slot.key.clone(), slot.credstore_ref.clone()))
                    .collect(),
                config: env.config.clone(),
                observed: env.observed.is_some(),
            });
            if let Some(detail) = self.refuse {
                return Err(qa_product_sdk::observation::PluginFailure::classified(
                    qa_product_sdk::observation::FailureClass::Internal,
                    detail,
                ));
            }
            // From the **reference**, never a resolved value: the contract's
            // central rule, and a double that read `slot.value` would hide a
            // caller that resolved plaintext it had no business resolving.
            let credstore_ref = env
                .credstore_ref(PLUGIN_SECRET_KEY)
                .ok_or_else(|| {
                    qa_product_sdk::observation::PluginFailure::classified(
                        qa_product_sdk::observation::FailureClass::Internal,
                        "no credential",
                    )
                })?
                .to_owned();
            let mut vars = vec![qa_product_sdk::access::RunVar {
                name: "KUBECONFIG".to_owned(),
                value: PLUGIN_MOUNT_PATH.to_owned(),
            }];
            vars.extend(self.extra_env.iter().map(|(name, value)| {
                qa_product_sdk::access::RunVar {
                    name: name.clone(),
                    value: value.clone(),
                }
            }));
            Ok(crate::domain::ports::run_executor::RunAccess {
                mounts: vec![crate::domain::ports::run_executor::MountSpec::Secret {
                    credstore_ref,
                    path: PLUGIN_MOUNT_PATH.to_owned(),
                    mode: None,
                }],
                env: vars,
                service_account: self.service_account.clone(),
            })
        }

        fn runner(
            &self,
            _observed: Option<&qa_product_sdk::observation::ObservedAttrs>,
        ) -> crate::domain::ports::run_executor::RunnerSpec {
            self.runner.clone()
        }

        fn env_contract(&self) -> qa_product_sdk::access::RunVarContract {
            qa_product_sdk::access::RunVarContract {
                reserved: self.reserved.iter().cloned().collect(),
            }
        }
    }

    /// The resolver double: answers for [`PRODUCT`] and nothing else.
    pub(in crate::domain::service) struct FakeProductPlugins {
        plugin: Arc<dyn qa_product_sdk::QaProductPluginV1>,
        /// What every `plugin_for` call answers instead, when a test wants the
        /// resolution itself to fail.
        unavailable: Option<PluginUnavailable>,
        pub(in crate::domain::service) resolved: Mutex<Vec<Uuid>>,
    }

    impl FakeProductPlugins {
        /// Any plugin, including a **real** one: `golden_run_spec_tests` hands
        /// this the actual `qa-vhp-product-plugin`, because a fixture proved
        /// against a double that reproduced VHP's rules would only prove the
        /// double agrees with itself.
        pub(in crate::domain::service) fn with(
            plugin: Arc<dyn qa_product_sdk::QaProductPluginV1>,
        ) -> Self {
            Self {
                plugin,
                unavailable: None,
                resolved: Mutex::new(Vec::new()),
            }
        }

        pub(in crate::domain::service) fn unavailable(reason: PluginUnavailable) -> Self {
            Self {
                unavailable: Some(reason),
                ..Self::with(Arc::new(ScriptedPlugin::default()))
            }
        }
    }

    impl Default for FakeProductPlugins {
        fn default() -> Self {
            Self::with(Arc::new(ScriptedPlugin::default()))
        }
    }

    #[async_trait]
    impl ProductPluginPort for FakeProductPlugins {
        async fn plugin_for(
            &self,
            _ctx: &SecurityContext,
            product_id: Uuid,
        ) -> Result<Arc<dyn qa_product_sdk::QaProductPluginV1>, PluginUnavailable> {
            self.resolved.lock().unwrap().push(product_id);
            if let Some(reason) = self.unavailable {
                return Err(reason);
            }
            // Keyed on the id, so a dispatch that resolved the wrong product's
            // plugin fails rather than passing against whatever this double
            // holds.
            if product_id == PRODUCT {
                Ok(Arc::clone(&self.plugin))
            } else {
                Err(PluginUnavailable::Unresolvable)
            }
        }
    }

    // -----------------------------------------------------------------------
    // qa-catalog
    // -----------------------------------------------------------------------

    /// Serves one plan per repository, one bundle per build, and can fail the
    /// first bundle build so decision D4's single retry is observable.
    #[derive(Default)]
    pub(in crate::domain::service) struct FakeCatalog {
        pub(in crate::domain::service) test_files: Mutex<Vec<String>>,
        pub(in crate::domain::service) custom_plan_files: Mutex<Vec<(Uuid, String)>>,
        pub(in crate::domain::service) metas: Mutex<Vec<TestFileMeta>>,
        pub(in crate::domain::service) fail_meta: Mutex<bool>,
        pub(in crate::domain::service) fail_bundle_times: Mutex<u32>,
        pub(in crate::domain::service) syncs: Mutex<Vec<(Uuid, SyncRequest)>>,
        /// `(repo_id, branch)` of every `list_plans` call — the collect
        /// dispatch's only catalog read for its file set.
        pub(in crate::domain::service) listed_branches: Mutex<Vec<(Uuid, String)>>,
        pub(in crate::domain::service) bundle_requests: Mutex<Vec<BundleRequest>>,
        /// See [`FakeCatalog::refusing_custom_plans`].
        refuse_custom_plans: bool,
    }

    /// The resource type qa-catalog raises a custom-plan not-found against
    /// (`qa-catalog/src/api/rest/error.rs`). Declared here so the double emits
    /// the same *category* the real gear would, rather than an `internal` that
    /// the launch path would classify differently.
    #[toolkit::api::canonical_prelude::resource_error(toolkit_gts::gts_id!(
        "cf.qa.catalog.custom_plan.v1~"
    ))]
    struct CatalogCustomPlanError;

    impl FakeCatalog {
        /// [`Self::serving`], but every custom-plan read answers not-found.
        ///
        /// **This double's shape is still the trap, and the trap is opt-in.**
        /// `FakeEnvironments` gates on `platform_tenants` and its `default()`
        /// refuses everyone, so a cross-tenant test written the obvious way
        /// against it fails closed. `FakeCatalog` gates on this `bool`, which
        /// defaults to permissive, and the shared `services()` helper in
        /// `tenant_scoping_tests` wires the permissive constructor - so a future
        /// cross-tenant custom-plan test written the obvious way passes
        /// **vacuously**. That already happened twice on this task.
        ///
        /// Inverting it - making refusal the default and permission opt-in -
        /// would touch every existing caller of `serving`, which is most of the
        /// launch suite. Recorded rather than done, so the next author reaches
        /// for this constructor deliberately instead of discovering the trap.
        ///
        /// Models the only thing this gear can observe about a plan in another
        /// tenant: qa-catalog refuses it, indistinguishably from one that does
        /// not exist. The double otherwise answers `get_custom_plan`
        /// unconditionally, which would make a cross-tenant test pass against a
        /// launch path that ignored the refusal entirely.
        pub(in crate::domain::service) fn refusing_custom_plans() -> Self {
            Self {
                refuse_custom_plans: true,
                ..Self::serving(&["tests/a.py"])
            }
        }

        pub(in crate::domain::service) fn serving(files: &[&str]) -> Self {
            Self {
                test_files: Mutex::new(files.iter().map(|f| (*f).to_owned()).collect()),
                ..Self::default()
            }
        }

        pub(in crate::domain::service) fn bundle_builds(&self) -> usize {
            self.bundle_requests.lock().unwrap().len()
        }

        pub(in crate::domain::service) fn syncs(&self) -> Vec<(Uuid, SyncRequest)> {
            self.syncs.lock().unwrap().clone()
        }
    }

    fn catalog_unsupported(method: &str) -> QaCatalogError {
        QaCatalogError::internal(format!("FakeCatalog::{method} is not used by these tests"))
            .create()
    }

    #[async_trait]
    impl QaCatalogClientV1 for FakeCatalog {
        /// Not implemented: `list_universe` is the analytics universe projection
        /// added for qa-insights (qa-catalog Task 7). qa-runs never calls it, so
        /// this double answers `unsupported` rather than fabricating a universe —
        /// if a qa-runs path ever starts calling it, this error is how we find out.
        async fn list_universe(
            &self,
            _ctx: &SecurityContext,
            _product_id: Option<Uuid>,
            _branch: Option<&str>,
        ) -> Result<Vec<UniverseTest>, QaCatalogError> {
            Err(catalog_unsupported("list_universe"))
        }

        async fn list_repos(
            &self,
            _ctx: &SecurityContext,
        ) -> Result<Vec<TestRepository>, QaCatalogError> {
            Err(catalog_unsupported("list_repos"))
        }

        /// Implemented since Task 15, because `service::launch` reads the
        /// repository's `default_branch` **unconditionally** — before it knows
        /// whether the request supplied an explicit branch (`launch.rs`,
        /// `plan_target_facts`' first statement) — so a re-run driven through the
        /// real launch path reaches this even though its branch is always
        /// explicit.
        async fn get_repo(
            &self,
            _ctx: &SecurityContext,
            id: Uuid,
        ) -> Result<TestRepository, QaCatalogError> {
            let stamp = now();
            Ok(TestRepository {
                id,
                product_id: Uuid::from_u128(0x0F01),
                name: "repo".to_owned(),
                url: "https://example.invalid/repo.git".to_owned(),
                default_branch: "main".to_owned(),
                content_root: String::new(),
                credential_ref: None,
                last_synced_at: Some(stamp),
                sync_error: None,
                created_at: stamp,
                updated_at: stamp,
            })
        }

        async fn create_repo(
            &self,
            _ctx: &SecurityContext,
            _new: NewTestRepository,
        ) -> Result<TestRepository, QaCatalogError> {
            Err(catalog_unsupported("create_repo"))
        }

        async fn update_repo(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
            _update: TestRepositoryUpdate,
        ) -> Result<TestRepository, QaCatalogError> {
            Err(catalog_unsupported("update_repo"))
        }

        async fn delete_repo(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<(), QaCatalogError> {
            Err(catalog_unsupported("delete_repo"))
        }

        async fn sync_repo(
            &self,
            _ctx: &SecurityContext,
            id: Uuid,
            req: SyncRequest,
        ) -> Result<TestRepository, QaCatalogError> {
            self.syncs.lock().unwrap().push((id, req));
            let stamp = now();
            Ok(TestRepository {
                id,
                product_id: Uuid::from_u128(0x0F01),
                name: "repo".to_owned(),
                url: "https://example.invalid/repo.git".to_owned(),
                default_branch: "main".to_owned(),
                content_root: String::new(),
                credential_ref: None,
                last_synced_at: Some(stamp),
                sync_error: None,
                created_at: stamp,
                updated_at: stamp,
            })
        }

        async fn list_branches(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<Vec<String>, QaCatalogError> {
            Err(catalog_unsupported("list_branches"))
        }

        /// The repository's plans on a branch, which is what a **collect**
        /// dispatch unions into its file set
        /// (`manager/src/services/collect.rs:56-75`).
        ///
        /// Two plans, so the union is observably a union rather than a single
        /// plan's list: one carries `test_files`, the other a fixed extra file.
        /// `branch` is recorded, because a collect run must enumerate the
        /// branch it was launched for.
        async fn list_plans(
            &self,
            _ctx: &SecurityContext,
            repo_id: Uuid,
            branch: &str,
        ) -> Result<Vec<Plan>, QaCatalogError> {
            self.listed_branches
                .lock()
                .unwrap()
                .push((repo_id, branch.to_owned()));
            let plan = |name: &str, files: Vec<String>| Plan {
                repo_id,
                product_id: Uuid::from_u128(0x0F01),
                branch: branch.to_owned(),
                path: format!("tests/{name}.yaml"),
                name: name.to_owned(),
                test_files: files,
                timeout_seconds: Some(300),
                tags: Vec::new(),
                validation: false,
                exclusive: None,
            };
            Ok(vec![
                plan("smoke", self.test_files.lock().unwrap().clone()),
                plan("extra", vec!["tests/test_extra.py".to_owned()]),
            ])
        }

        async fn get_plan(
            &self,
            _ctx: &SecurityContext,
            repo_id: Uuid,
            branch: &str,
            path: &str,
        ) -> Result<Plan, QaCatalogError> {
            Ok(Plan {
                repo_id,
                product_id: Uuid::from_u128(0x0F01),
                branch: branch.to_owned(),
                path: path.to_owned(),
                name: "Smoke".to_owned(),
                test_files: self.test_files.lock().unwrap().clone(),
                timeout_seconds: Some(300),
                tags: Vec::new(),
                validation: false,
                exclusive: None,
            })
        }

        async fn get_test_meta(
            &self,
            _ctx: &SecurityContext,
            _repo_id: Uuid,
            _branch: &str,
            _files: &[String],
        ) -> Result<Vec<TestFileMeta>, QaCatalogError> {
            if *self.fail_meta.lock().unwrap() {
                return Err(catalog_unsupported("get_test_meta"));
            }
            Ok(self.metas.lock().unwrap().clone())
        }

        async fn list_custom_plans(
            &self,
            _ctx: &SecurityContext,
        ) -> Result<Vec<CustomPlan>, QaCatalogError> {
            Err(catalog_unsupported("list_custom_plans"))
        }

        async fn get_custom_plan(
            &self,
            _ctx: &SecurityContext,
            id: Uuid,
        ) -> Result<CustomPlan, QaCatalogError> {
            if self.refuse_custom_plans {
                return Err(CatalogCustomPlanError::not_found(format!(
                    "custom plan {id} was not found"
                ))
                .with_resource(id.to_string())
                .create());
            }
            let stamp = now();
            Ok(CustomPlan {
                id,
                name: "mixed".to_owned(),
                files: self
                    .custom_plan_files
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(repo_id, path)| CustomPlanEntry {
                        repo_id: *repo_id,
                        path: path.clone(),
                        plan_path: Some("plans/nested.yaml".to_owned()),
                    })
                    .collect(),
                tags: Vec::new(),
                timeout_seconds: None,
                created_at: stamp,
                updated_at: stamp,
            })
        }

        async fn create_custom_plan(
            &self,
            _ctx: &SecurityContext,
            _new: NewCustomPlan,
        ) -> Result<CustomPlan, QaCatalogError> {
            Err(catalog_unsupported("create_custom_plan"))
        }

        async fn update_custom_plan(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
            _new: NewCustomPlan,
        ) -> Result<CustomPlan, QaCatalogError> {
            Err(catalog_unsupported("update_custom_plan"))
        }

        async fn delete_custom_plan(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<(), QaCatalogError> {
            Err(catalog_unsupported("delete_custom_plan"))
        }

        async fn list_products(
            &self,
            _ctx: &SecurityContext,
        ) -> Result<Vec<Product>, QaCatalogError> {
            Err(catalog_unsupported("list_products"))
        }

        async fn create_product(
            &self,
            _ctx: &SecurityContext,
            _new: qa_catalog_sdk::NewProduct,
        ) -> Result<Product, QaCatalogError> {
            Err(catalog_unsupported("create_product"))
        }

        async fn update_product(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
            _update: qa_catalog_sdk::ProductUpdate,
        ) -> Result<Product, QaCatalogError> {
            Err(catalog_unsupported("update_product"))
        }

        async fn delete_product(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<(), QaCatalogError> {
            Err(catalog_unsupported("delete_product"))
        }

        async fn list_ssh_keys(
            &self,
            _ctx: &SecurityContext,
        ) -> Result<Vec<SshKey>, QaCatalogError> {
            Err(catalog_unsupported("list_ssh_keys"))
        }

        async fn create_ssh_key(
            &self,
            _ctx: &SecurityContext,
            _name: String,
            _private_key_pem: String,
        ) -> Result<SshKey, QaCatalogError> {
            Err(catalog_unsupported("create_ssh_key"))
        }

        async fn delete_ssh_key(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<(), QaCatalogError> {
            Err(catalog_unsupported("delete_ssh_key"))
        }

        async fn create_bundle(
            &self,
            _ctx: &SecurityContext,
            req: BundleRequest,
        ) -> Result<TestBundle, QaCatalogError> {
            self.bundle_requests.lock().unwrap().push(req);
            {
                let mut remaining = self.fail_bundle_times.lock().unwrap();
                if *remaining > 0 {
                    *remaining -= 1;
                    return Err(catalog_unsupported("create_bundle"));
                }
            }
            let stamp = now();
            Ok(TestBundle {
                id: Uuid::new_v4(),
                storage_ref: "bundle://one".to_owned(),
                checksum_sha256: "0".repeat(64),
                size_bytes: 1024,
                expires_at: stamp,
                created_at: stamp,
            })
        }

        async fn get_bundle_content(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<Vec<u8>, QaCatalogError> {
            Err(catalog_unsupported("get_bundle_content"))
        }
    }

    // -----------------------------------------------------------------------
    // Executor wrapper that can fail `cancel`
    // -----------------------------------------------------------------------

    /// The mock executor plus an injectable `cancel` failure.
    ///
    /// `MockRunExecutor` has `fail_start` and `fail_list_active` but no
    /// `fail_cancel`, and the timeout sweep's fail-safe direction — a failed
    /// cancel must leave the claim and the lease held — cannot be exercised
    /// without one. Adding it to the mock would mean editing Task 11's file, so
    /// it is wrapped here instead.
    pub(in crate::domain::service) struct CancelFailingExecutor {
        inner: crate::infra::executor::mock::MockRunExecutor,
    }

    impl CancelFailingExecutor {
        pub(in crate::domain::service) fn new() -> Self {
            Self {
                inner: crate::infra::executor::mock::MockRunExecutor::new(),
            }
        }

        /// Register a live execution, so `list_active` reports it.
        ///
        /// Without this, `list_active` is empty and claim reconciliation
        /// legitimately classifies the claim as `Gone` and releases it — which is
        /// correct behaviour but hides whatever the timeout sweep did. A test
        /// about the sweep's fail-safe direction needs the execution to be alive.
        pub(in crate::domain::service) async fn seed_active(&self, run_id: Uuid) -> ExecutionRef {
            let spec = RunSpec {
                run_id,
                run_name: "seeded".to_owned(),
                nodes: vec![crate::domain::ports::run_executor::ExecutionNode {
                    name: "repo-a".to_owned(),
                    bundle_ref: "bundle://x".to_owned(),
                    test_files: vec!["tests/a.py".to_owned()],
                }],
                env: crate::domain::ports::run_executor::RunEnv::default(),
                access: RunAccess::default(),
                runner: RunnerSpec::default(),
                timeout_seconds: 60,
            };
            self.inner.start(spec).await.expect("the mock accepts it")
        }
    }

    #[async_trait]
    impl RunExecutor for CancelFailingExecutor {
        async fn start(&self, spec: RunSpec) -> Result<ExecutionRef, DomainError> {
            self.inner.start(spec).await
        }

        async fn watch(
            &self,
            execution_ref: &ExecutionRef,
        ) -> Result<ExecutionStream, DomainError> {
            self.inner.watch(execution_ref).await
        }

        async fn cancel(&self, _execution_ref: &ExecutionRef) -> Result<(), DomainError> {
            Err(DomainError::ExecutorFailed(
                "the execution plane is unreachable".to_owned(),
            ))
        }

        async fn list_active(
            &self,
        ) -> Result<std::collections::BTreeSet<ExecutionRef>, DomainError> {
            self.inner.list_active().await
        }
    }

    // -----------------------------------------------------------------------
    // Assembly
    // -----------------------------------------------------------------------

    /// One admission service and one dispatch service over one set of doubles,
    /// sharing one [`PlatformLocks`] — the same wiring `AppServices::new`
    /// performs.
    pub(in crate::domain::service) struct Fakes {
        pub(in crate::domain::service) runs: Arc<FakeRuns>,
        pub(in crate::domain::service) queue: Arc<FakeQueue>,
        pub(in crate::domain::service) environments: Arc<FakeEnvironments>,
        pub(in crate::domain::service) catalog: Arc<FakeCatalog>,
        /// The log fan-out both services were built over, so a test can assert
        /// that a terminal transition released the run's channel.
        pub(in crate::domain::service) logs:
            Arc<crate::domain::service::ingest::tests::RecordingLogs>,
        pub(in crate::domain::service) admission: AdmissionService<FakeRuns, FakeQueue>,
        pub(in crate::domain::service) dispatch: DispatchService<FakeRuns, FakeQueue>,
        /// The observer seam the dispatch service was built over, so a test can
        /// assert which live runs a tick decided to start watching.
        pub(in crate::domain::service) watcher: Arc<RecordingWatcher>,
        /// The plugin resolver the dispatch service was built over, so a test
        /// can assert which product a dispatch resolved and what the plugin
        /// was handed.
        pub(in crate::domain::service) product_plugins: Arc<FakeProductPlugins>,
    }

    /// Builder, so each test names only what it cares about.
    pub(in crate::domain::service) struct Builder {
        runs: Arc<FakeRuns>,
        queue: Arc<FakeQueue>,
        environments: Arc<FakeEnvironments>,
        catalog: Arc<FakeCatalog>,
        product_plugins: Arc<FakeProductPlugins>,
        executor: Option<Arc<dyn RunExecutor>>,
        authz: Option<Arc<dyn AuthZResolverClient>>,
        limits: QueueLimits,
        orphan_timeout_seconds: u64,
    }

    impl Builder {
        pub(in crate::domain::service) fn new() -> Self {
            Self {
                runs: Arc::new(FakeRuns::default()),
                queue: Arc::new(FakeQueue::default()),
                environments: Arc::new(FakeEnvironments::free()),
                catalog: Arc::new(FakeCatalog::serving(&["tests/a.py"])),
                product_plugins: Arc::new(FakeProductPlugins::default()),
                executor: None,
                authz: None,
                limits: QueueLimits {
                    queue_max_depth: 20,
                    max_concurrent_runs: 0,
                    queue_ttl_seconds: 7200,
                },
                orphan_timeout_seconds: 600,
            }
        }

        pub(in crate::domain::service) fn runs(mut self, runs: Arc<FakeRuns>) -> Self {
            self.runs = runs;
            self
        }

        pub(in crate::domain::service) fn queue(mut self, queue: Arc<FakeQueue>) -> Self {
            self.queue = queue;
            self
        }

        pub(in crate::domain::service) fn environments(
            mut self,
            environments: Arc<FakeEnvironments>,
        ) -> Self {
            self.environments = environments;
            self
        }

        pub(in crate::domain::service) fn catalog(mut self, catalog: Arc<FakeCatalog>) -> Self {
            self.catalog = catalog;
            self
        }

        pub(in crate::domain::service) fn product_plugins(
            mut self,
            product_plugins: Arc<FakeProductPlugins>,
        ) -> Self {
            self.product_plugins = product_plugins;
            self
        }

        pub(in crate::domain::service) fn executor(
            mut self,
            executor: Arc<dyn RunExecutor>,
        ) -> Self {
            self.executor = Some(executor);
            self
        }

        pub(in crate::domain::service) fn authz(
            mut self,
            authz: Arc<dyn AuthZResolverClient>,
        ) -> Self {
            self.authz = Some(authz);
            self
        }

        pub(in crate::domain::service) fn limits(mut self, limits: QueueLimits) -> Self {
            self.limits = limits;
            self
        }

        pub(in crate::domain::service) fn orphan_timeout(mut self, seconds: u64) -> Self {
            self.orphan_timeout_seconds = seconds;
            self
        }

        pub(in crate::domain::service) async fn build(self) -> Fakes {
            let db: Arc<DbProvider> = test_db_provider().await;
            let logs = Arc::new(crate::domain::service::ingest::tests::RecordingLogs::default());
            let executor: Arc<dyn RunExecutor> = self
                .executor
                .unwrap_or_else(|| Arc::new(crate::infra::executor::mock::MockRunExecutor::new()));
            let authz = self
                .authz
                .unwrap_or_else(|| Arc::new(SystemGrantingAuthZ) as Arc<dyn AuthZResolverClient>);
            let enforcer = PolicyEnforcer::new(authz);
            let locks = PlatformLocks::default();

            let admission = AdmissionService::new(AdmissionDeps {
                db: Arc::clone(&db),
                runs: Arc::clone(&self.runs),
                queue: Arc::clone(&self.queue),
                environments: Arc::clone(&self.environments) as Arc<dyn QaEnvironmentsClientV1>,
                executor: Arc::clone(&executor),
                locks: locks.clone(),
                limits: self.limits,
                policy_enforcer: enforcer.clone(),
            });
            let watcher = Arc::new(RecordingWatcher::new());
            let dispatch = DispatchService::new(DispatchDeps {
                db,
                runs: Arc::clone(&self.runs),
                queue: Arc::clone(&self.queue),
                catalog: Arc::clone(&self.catalog) as Arc<dyn QaCatalogClientV1>,
                environments: Arc::clone(&self.environments) as Arc<dyn QaEnvironmentsClientV1>,
                product_plugins: Arc::clone(&self.product_plugins) as Arc<dyn ProductPluginPort>,
                executor: Arc::clone(&executor),
                // Task 15: a terminal transition releases the run's live log channel.
                logs: Arc::clone(&logs) as Arc<dyn crate::domain::service::LogFanout>,
                locks,
                limits: self.limits,
                orphan_timeout_seconds: self.orphan_timeout_seconds,
                policy_enforcer: enforcer,
                watcher: Arc::clone(&watcher) as Arc<dyn crate::domain::service::watch::RunWatcher>,
            });

            Fakes {
                runs: self.runs,
                queue: self.queue,
                environments: self.environments,
                catalog: self.catalog,
                logs,
                admission,
                dispatch,
                watcher,
                product_plugins: self.product_plugins,
            }
        }
    }

    /// A `BTreeMap` of environment values, for readable assertions.
    pub(in crate::domain::service) fn env_values(spec: &RunSpec) -> BTreeMap<String, String> {
        spec.env
            .entries()
            .iter()
            .filter_map(|(name, source)| match source {
                crate::domain::ports::run_executor::EnvSource::Value(value) => {
                    Some((name.clone(), value.clone()))
                }
                crate::domain::ports::run_executor::EnvSource::Secret(_) => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The lock registry
// ---------------------------------------------------------------------------

/// The whole point of the registry: one mutex per platform, reused. A registry
/// that minted a fresh mutex per call would compile, look right, and serialise
/// nothing (`manager/src/services/run_queue.rs:1412-1428`).
#[tokio::test]
async fn the_same_platform_maps_to_the_same_lock() {
    let locks = PlatformLocks::default();
    let first = locks.get(PLATFORM_A).await;
    let second = locks.get(PLATFORM_A).await;
    assert!(
        Arc::ptr_eq(&first, &second),
        "the same platform must map to the same mutex, or admission is not serialised"
    );
}

#[tokio::test]
async fn different_platforms_do_not_share_a_lock() {
    let locks = PlatformLocks::default();
    let a = locks.get(PLATFORM_A).await;
    let b = locks.get(PLATFORM_B).await;
    assert!(
        !Arc::ptr_eq(&a, &b),
        "different platforms must not share a mutex, or they would block each other"
    );
}

/// **Bounded on purpose.** A global lock would block here forever rather than
/// fail an assertion, so this must fail loudly and not hang CI — the source
/// system pins exactly this, with the same reasoning, at
/// `manager/src/services/run_queue.rs:1430-1449`.
#[tokio::test]
async fn holding_one_platforms_lock_does_not_block_another() {
    let locks = PlatformLocks::default();
    let held = locks.get(PLATFORM_A).await;
    let _guard = held.lock().await;

    let other = locks.get(PLATFORM_B).await;
    let acquired = tokio::time::timeout(std::time::Duration::from_secs(5), other.lock()).await;
    assert!(
        acquired.is_ok(),
        "acquiring a second platform's lock timed out while the first was held - the lock \
         is global, not per platform"
    );
}

/// The outer registry mutex is held only for the lookup
/// (`run_queue.rs:580-588`), so a platform lock being held must not stop a
/// *third* platform's lock being handed out. Bounded for the same reason as
/// above.
#[tokio::test]
async fn the_registry_hands_out_a_lock_while_another_is_held() {
    let locks = PlatformLocks::default();
    let held = locks.get(PLATFORM_A).await;
    let _guard = held.lock().await;

    let handed_out = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        locks.get(Uuid::from_u128(0x0C0C)),
    )
    .await;
    assert!(
        handed_out.is_ok(),
        "the registry lock is held across the per-platform critical section"
    );
}

// ---------------------------------------------------------------------------
// admit: the platformless case and the two 429s
// ---------------------------------------------------------------------------

/// Guide line 76: "A run with no platform is never queued and never blocks
/// anything." No row, no lease, no lock.
#[tokio::test]
async fn a_run_without_a_platform_is_never_queued() {
    let run = run_fixture(Uuid::from_u128(1), None, false, RunState::Created);
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .build()
        .await;

    let admission = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .unwrap();

    assert_eq!(admission, Admission::Unqueued);
    assert!(fakes.queue.rows().is_empty(), "no queue row was written");
    assert!(
        fakes.environments.acquires.lock().unwrap().is_empty(),
        "no lease was taken"
    );
}

/// **A platformless launch is still cluster capacity.** The bypass at guide line
/// 76 is about coordination — no queue row, no lease, no lock — not about the
/// cluster-wide cap, and legacy proves the ordering: `enforce_global_cap` at
/// `manager/src/services/run_dispatcher.rs:81`, the platformless bypass not until
/// `:103-106`.
///
/// Until this test existed the bypass returned *before* the cap check, so a
/// platformless launch against a full cluster started instead of answering 429 —
/// and nothing caught it, because
/// `a_reached_concurrency_cap_refuses_the_launch_before_the_lock` uses a platform.
/// Removing the hoist reddens this and only this.
#[tokio::test]
async fn a_platformless_launch_is_still_refused_by_the_concurrency_cap() {
    let run = run_fixture(Uuid::from_u128(1), None, false, RunState::Created);
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let spec = crate::domain::ports::run_executor::RunSpec {
        run_id: Uuid::from_u128(0x77),
        run_name: "other-1".to_owned(),
        nodes: vec![crate::domain::ports::run_executor::ExecutionNode {
            name: "repo-a".to_owned(),
            bundle_ref: "bundle://x".to_owned(),
            test_files: vec!["tests/a.py".to_owned()],
        }],
        env: crate::domain::ports::run_executor::RunEnv::default(),
        access: crate::domain::ports::run_executor::RunAccess::default(),
        runner: crate::domain::ports::run_executor::RunnerSpec::default(),
        timeout_seconds: 60,
    };
    crate::domain::ports::run_executor::RunExecutor::start(executor.as_ref(), spec)
        .await
        .unwrap();

    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .executor(executor)
        .limits(super::QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 1,
            queue_ttl_seconds: 7200,
        })
        .build()
        .await;

    let error = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .expect_err("a full cluster refuses a platformless launch too");
    assert!(matches!(error, DomainError::ConcurrencyLimit { limit: 1 }));
    assert!(fakes.queue.rows().is_empty(), "still no queue row");
}

/// The bypass itself is unaffected when the cap is disabled or has room: no row,
/// no lease, no lock — the guard against "fixing" the ordering above by making
/// every platformless launch pay for the queue.
#[tokio::test]
async fn a_platformless_launch_under_the_cap_is_unqueued() {
    let run = run_fixture(Uuid::from_u128(1), None, false, RunState::Created);
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .limits(super::QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 4,
            queue_ttl_seconds: 7200,
        })
        .build()
        .await;

    assert_eq!(
        fakes
            .admission
            .admit_decision(&ctx(OWNER_TENANT), &run)
            .await
            .unwrap(),
        Admission::Unqueued
    );
    assert!(fakes.queue.rows().is_empty());
    assert!(fakes.environments.acquires.lock().unwrap().is_empty());
}

/// An idle platform dispatches inline, takes the lease in the run's own mode,
/// and files the row already `dispatching` — which is what makes the row a claim
/// before the caller submits (`manager/src/services/run_queue.rs:159-172`, the
/// doc on legacy's own `insert`; an earlier revision of this line cited `:207-212`,
/// which is `claims_for_platform`).
#[tokio::test]
async fn an_idle_platform_dispatches_inline_and_takes_the_lease() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        true,
        RunState::Created,
    );
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .build()
        .await;

    let admission = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .unwrap();

    assert!(matches!(admission, Admission::Dispatch { .. }));
    let rows = fakes.queue.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, QueueState::Dispatching);
    assert!(rows[0].dispatched_at.is_some());
    assert_eq!(
        fakes.environments.acquired_modes(),
        vec![qa_environments_sdk::LeaseMode::Exclusive],
        "an exclusive run must ask for an exclusive lease"
    );
}

/// The depth check is inside the lock and **before** the occupancy read
/// (`run_queue.rs:633-655`): a rejection writes no row, so there is nothing to
/// decide and no reason to pay for a read that would be thrown away. The
/// observable half is that no lease read happened at all.
#[tokio::test]
async fn a_full_queue_is_rejected_before_the_occupancy_read() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let existing = queued_row(
        Uuid::from_u128(0x11),
        OWNER_TENANT,
        Uuid::from_u128(0x99),
        PLATFORM_A,
        false,
        QueueState::Queued,
    );
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .queue(Arc::new(fakes::FakeQueue::with(vec![existing])))
        .limits(super::QueueLimits {
            queue_max_depth: 1,
            max_concurrent_runs: 0,
            queue_ttl_seconds: 7200,
        })
        .build()
        .await;

    let error = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .expect_err("a full queue refuses the launch");

    assert!(matches!(
        error,
        DomainError::QueueFull {
            queued: 1,
            limit: 1,
            ..
        }
    ));
    assert_eq!(fakes.queue.rows().len(), 1, "no row was written");
    assert!(
        fakes.environments.tenants_seen.lock().unwrap().is_empty(),
        "the occupancy read must not happen on a rejection"
    );
}

/// The cluster-wide cap is the other of the guide's exactly-two 429 causes, and
/// it is evaluated **before** the platform lock and before any queue read
/// (`run_dispatcher.rs:80-81`).
#[tokio::test]
async fn a_reached_concurrency_cap_refuses_the_launch_before_the_lock() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    // One live execution against a cap of one.
    let spec = crate::domain::ports::run_executor::RunSpec {
        run_id: Uuid::from_u128(0x77),
        run_name: "other-1".to_owned(),
        nodes: vec![crate::domain::ports::run_executor::ExecutionNode {
            name: "repo-a".to_owned(),
            bundle_ref: "bundle://x".to_owned(),
            test_files: vec!["tests/a.py".to_owned()],
        }],
        env: crate::domain::ports::run_executor::RunEnv::default(),
        access: crate::domain::ports::run_executor::RunAccess::default(),
        runner: crate::domain::ports::run_executor::RunnerSpec::default(),
        timeout_seconds: 60,
    };
    crate::domain::ports::run_executor::RunExecutor::start(executor.as_ref(), spec)
        .await
        .unwrap();

    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .executor(executor)
        .limits(super::QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 1,
            queue_ttl_seconds: 7200,
        })
        .build()
        .await;

    let error = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .expect_err("the cap refuses the launch");

    assert!(matches!(error, DomainError::ConcurrencyLimit { limit: 1 }));
    assert!(fakes.queue.rows().is_empty(), "no row was written");
}

/// A disabled cap costs no executor call at all — legacy returns before listing
/// anything when `max_concurrent_runs == 0` (`run_queue.rs:856-858`), which is
/// the shipped default. Proven by injecting a listing failure that would
/// otherwise fail the launch.
#[tokio::test]
async fn a_disabled_cap_never_calls_the_executor() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    executor.fail_list_active("the executor is unreachable");

    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .executor(executor)
        .build()
        .await;

    let admission = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .expect("a disabled cap must not consult the executor");
    assert!(matches!(admission, Admission::Dispatch { .. }));
}

/// An enabled cap that cannot be evaluated **fails the launch**, where the depth
/// limit fails open. Legacy makes exactly this asymmetry and says why
/// (`run_queue.rs:893-900`): over-committing is the worse outcome for the cap,
/// while for the depth limit the worse outcome is refusing a legitimate launch.
#[tokio::test]
async fn an_unreadable_executor_fails_a_launch_under_an_enabled_cap() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    executor.fail_list_active("the executor is unreachable");

    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .executor(executor)
        .limits(super::QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 4,
            queue_ttl_seconds: 7200,
        })
        .build()
        .await;

    let error = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .expect_err("an unevaluable cap must not admit");
    assert!(matches!(error, DomainError::ExecutorFailed(_)));
}

// ---------------------------------------------------------------------------
// Fail-safe directions
// ---------------------------------------------------------------------------

/// **The fail-safe direction.** An unreadable lease makes the platform read as
/// exclusively held, so the run queues rather than trampling an exclusive run
/// (`run_dispatcher.rs:237-252`; guide line 218). Note what this also proves: no
/// lease was acquired, because the decision never reached `Dispatch`.
#[tokio::test]
async fn an_unreadable_lease_makes_the_platform_read_as_busy() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .environments(Arc::new(fakes::FakeEnvironments::unreadable()))
        .build()
        .await;

    let admission = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .unwrap();

    assert!(
        matches!(admission, Admission::Queued { .. }),
        "an unreadable oracle must queue, never dispatch"
    );
    assert_eq!(fakes.queue.rows()[0].state, QueueState::Queued);
    assert!(
        fakes.environments.acquires.lock().unwrap().is_empty(),
        "nothing may be dispatched blind"
    );
}

/// The operator-facing half of the same fail-safe: `lease_occupancy` decides to
/// treat the platform as held, and the **only** record that it decided anything
/// is one ERROR line. Without it the symptom is a run that queues forever with
/// no cause anywhere.
///
/// The sibling test above pins the decision; this one pins the emission, which
/// nothing in this crate could observe before `tracing-test` became a
/// dev-dependency - deleting the `error!` left the whole suite green.
#[tokio::test]
#[tracing_test::traced_test]
async fn an_unreadable_lease_logs_why_it_failed_closed() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .environments(Arc::new(fakes::FakeEnvironments::unreadable()))
        .build()
        .await;

    fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .unwrap();

    assert!(
        logs_contain("could not read the platform lease"),
        "the fail-closed decision must name itself in the log"
    );
    assert!(
        logs_contain(&PLATFORM_A.to_string()),
        "the line must name the platform, or an operator cannot act on it"
    );
}

/// **The CAS is the source of truth and the planner is advisory**
/// (`domain::queue`'s module docs). An idle lease read followed by a `Busy`
/// acquisition must produce a queued row, not a dispatch.
#[tokio::test]
async fn a_busy_lease_overrides_an_admit_decision_from_the_planner() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .environments(Arc::new(fakes::FakeEnvironments::busy_on_acquire()))
        .build()
        .await;

    let admission = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .unwrap();

    assert!(
        matches!(admission, Admission::Queued { .. }),
        "the lease may refuse a dispatch the planner allowed"
    );
    assert_eq!(fakes.queue.rows()[0].state, QueueState::Queued);
    assert_eq!(
        fakes.environments.acquires.lock().unwrap().len(),
        1,
        "the acquisition was attempted, which is what makes the CAS the decider"
    );
}

/// A lease taken for a row that could not be written must be handed back, or the
/// platform reads busy forever — the lease is this gear's only occupancy oracle.
#[tokio::test]
async fn a_failed_insert_releases_the_lease_it_took() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        true,
        RunState::Created,
    );
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .queue(Arc::new(fakes::FakeQueue::failing_insert(
            DomainError::Database("the row could not be written".to_owned()),
        )))
        .build()
        .await;

    let error = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .expect_err("the insert failed");
    assert!(matches!(error, DomainError::Database(_)));
    assert_eq!(
        fakes.environments.released(),
        vec![(PLATFORM_A, run.id)],
        "the lease taken for a row that was never written must be given back"
    );
}

/// **An `acquire_lease` that fails may still have committed, so the failure path
/// releases.** The interesting input is not "the acquisition was refused" — that is
/// `Busy` — it is an acquisition that lands server-side and loses its response.
///
/// Without the release the platform is wedged permanently, and every recovery path
/// is closed: the row is `queued`, so it is not a claim and `all_claims` never
/// returns it; the next tick's occupancy read sees `HeldExclusive`, so
/// `plan_dispatch_batch` breaks on the first row and never retries; and the TTL
/// sweep releases no lease when the row finally expires. `decide_release` is
/// idempotent and frees only a hold the run actually has, so the release is a no-op
/// when the acquisition genuinely never landed.
///
/// Removing the `give_back_lease` call on the `Err` arm leaves `released()` empty.
/// Verified by removal.
#[tokio::test]
async fn a_failed_acquisition_releases_the_lease_it_may_have_taken() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        true,
        RunState::Created,
    );
    let environments = Arc::new(fakes::FakeEnvironments::free());
    *environments.fail_acquire.lock().unwrap() = true;
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .environments(Arc::clone(&environments))
        .build()
        .await;

    let admission = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .unwrap();

    assert!(
        matches!(admission, Admission::Queued { .. }),
        "an unknown platform state must not produce a start"
    );
    assert_eq!(
        environments.released(),
        vec![(PLATFORM_A, run.id)],
        "a lease the acquisition may have taken must be handed back, or nothing ever \
         frees the platform"
    );
}

/// The ownership precheck. A run belonging to another tenant is not visible under
/// this caller's scope, so `resolve_owned` answers `RunNotFound` and no queue row
/// is written — the tenant-blind foreign key never gets to answer.
#[tokio::test]
async fn a_run_owned_by_another_tenant_cannot_be_admitted() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OTHER_TENANT,
            run.clone(),
        )])))
        .build()
        .await;

    let error = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .expect_err("a foreign run must not be admitted");
    assert!(matches!(error, DomainError::RunNotFound { .. }));
    assert!(fakes.queue.rows().is_empty());
    assert!(
        fakes.environments.acquires.lock().unwrap().is_empty(),
        "the precheck runs before the lease, so no lease is taken for a foreign run"
    );
}

/// Strict FIFO: once anything is queued for a platform, later launches queue
/// behind it even on an idle platform (guide lines 105-107).
///
/// **No longer asserts a position.** `Admission::Queued` carried a `position`
/// field — the depth read under the lock plus one — until this task deleted
/// it: nothing downstream reads it since `LaunchService::settle` stopped
/// forwarding it anywhere (it used to feed the deleted `LifecycleEvent::RunQueued`).
/// The FIFO-position concept itself is not gone and is not untested — it lives
/// in `domain::queue::assign_positions`, the REST-facing queue listing's own
/// computation, with its own tests — this one now pins only what
/// `Admission::Queued` itself still means: a run behind an existing queued row
/// is queued, not dispatched, even though the platform is otherwise idle.
#[tokio::test]
async fn a_queued_row_holds_back_a_later_launch_even_on_an_idle_platform() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let existing = queued_row(
        Uuid::from_u128(0x11),
        OWNER_TENANT,
        Uuid::from_u128(0x99),
        PLATFORM_A,
        false,
        QueueState::Queued,
    );
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .queue(Arc::new(fakes::FakeQueue::with(vec![existing])))
        .build()
        .await;

    let admission = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .unwrap();

    assert!(
        matches!(admission, Admission::Queued { .. }),
        "a run behind an existing queued row must queue, not dispatch, even \
         though nothing is running: {admission:?}"
    );
}

/// A parallel run joins a platform held by parallel runs, and asks for a
/// parallel lease (`domain::queue::platform_admits`).
#[tokio::test]
async fn a_parallel_run_joins_a_parallel_hold() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .environments(Arc::new(fakes::FakeEnvironments::holding(
            PLATFORM_A,
            LeaseState::HeldParallel {
                holders: vec![Uuid::from_u128(0x55)],
            },
        )))
        .build()
        .await;

    let admission = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .unwrap();
    assert!(matches!(admission, Admission::Dispatch { .. }));
    assert_eq!(
        fakes.environments.acquired_modes(),
        vec![qa_environments_sdk::LeaseMode::Parallel]
    );
}

/// An exclusive run needs the platform to itself, so it queues behind a parallel
/// hold.
#[tokio::test]
async fn an_exclusive_run_queues_behind_a_parallel_hold() {
    let run = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        true,
        RunState::Created,
    );
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![(
            OWNER_TENANT,
            run.clone(),
        )])))
        .environments(Arc::new(fakes::FakeEnvironments::holding(
            PLATFORM_A,
            LeaseState::HeldParallel {
                holders: vec![Uuid::from_u128(0x55)],
            },
        )))
        .build()
        .await;

    let admission = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &run)
        .await
        .unwrap();
    assert!(matches!(admission, Admission::Queued { .. }));
    assert!(fakes.environments.acquires.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Concurrency
// ---------------------------------------------------------------------------

/// Two concurrent exclusive launches against one idle platform: exactly one
/// starts and the other queues.
///
/// # This test does **not** discriminate the platform lock, and that is a finding
///
/// It was written as the lock's regression test and it is not one: deleting the
/// lock leaves it green.
///
/// **What saves the second launch here is the occupancy read, not the lease CAS**
/// — and the first version of this paragraph said the opposite. The single yield
/// in `FakeQueue::queued_depth` sits between the depth count and the return, so
/// with the lock gone the second task resumes *after* the first has already
/// acquired the lease; its `lease_occupancy` therefore reads `HeldExclusive` and
/// `decide_admission` queues it. `take_lease` is never reached for that launch, so
/// the CAS is not exercised at all. Verified the way the wrong version should have
/// been: forcing the double's CAS to grant every request **and** deleting the lock
/// leaves this green, which it could not do if the CAS were the saviour.
///
/// Both mechanisms do exist in production, and which one fires depends on where
/// the real interleaving lands — occupancy-read-then-acquire is refused by
/// `decide_admission`, acquire-then-acquire is refused by the CAS. That is why the
/// *conclusion* (one start, one queued, lock or no lock) holds while the mechanism
/// sentence did not.
///
/// So what this pins is the **outcome the guide promises** (guide lines 3-6), which
/// is worth pinning. What pins the other three things:
///
/// * the **lock** — [`two_concurrent_launches_cannot_both_observe_room_in_a_full_queue`],
///   whose race is about the depth read and which the CAS cannot cover;
/// * the **CAS** — `a_busy_lease_overrides_an_admit_decision_from_the_planner`,
///   through `FakeEnvironments::busy_on_acquire`, which is the only test that
///   reaches `take_lease`'s `Busy` arm;
/// * the **registry** — the three `Arc::ptr_eq` tests above.
///
/// (The first version of this test was also green against a double whose
/// `acquire_lease` granted every request. That is fixed, and it changed nothing
/// here — which is itself the evidence above.)
/// A [`RunExecutor`] whose listing yields, so two admissions genuinely overlap.
///
/// Without it the cap tests below are inconclusive: `MockRunExecutor::list_active`
/// awaits nothing, so on the single-threaded test runtime the first task runs to
/// completion before the second starts and the un-coordinated version passes.
struct YieldingListing(Arc<crate::infra::executor::mock::MockRunExecutor>);

#[async_trait]
impl RunExecutor for YieldingListing {
    async fn start(
        &self,
        spec: crate::domain::ports::run_executor::RunSpec,
    ) -> Result<crate::domain::ports::run_executor::ExecutionRef, DomainError> {
        self.0.start(spec).await
    }

    async fn cancel(
        &self,
        reference: &crate::domain::ports::run_executor::ExecutionRef,
    ) -> Result<(), DomainError> {
        self.0.cancel(reference).await
    }

    async fn watch(
        &self,
        reference: &crate::domain::ports::run_executor::ExecutionRef,
    ) -> Result<crate::domain::ports::run_executor::ExecutionStream, DomainError> {
        self.0.watch(reference).await
    }

    async fn list_active(
        &self,
    ) -> Result<
        std::collections::BTreeSet<crate::domain::ports::run_executor::ExecutionRef>,
        DomainError,
    > {
        tokio::task::yield_now().await;
        self.0.list_active().await
    }
}

/// **The race `enforce_global_cap` was.** It read `list_active()`, compared, and
/// returned; the run it admitted does not enter that listing until it is
/// submitted, so two concurrent callers both read an empty cluster and both
/// passed a cap of one — and the guide makes this the one cap force start may not
/// override (`exclusive-runs-and-the-queue.md:117`).
///
/// Platformless runs, so the outcome is decided by the cap and nothing else: no
/// queue row, no lease, no platform lock.
///
/// Making the gate ignore its outstanding slots reddens this; verified. What it
/// does **not** reach is the atomicity of the check-and-increment — the test
/// runtime is single-threaded, so nothing can interleave between a plain load and
/// a plain store, and replacing the `fetch_update` with the two leaves this
/// green.
#[tokio::test]
async fn two_concurrent_launches_cannot_both_pass_a_cap_of_one() {
    let first = run_fixture(Uuid::from_u128(1), None, false, RunState::Created);
    let second = run_fixture(Uuid::from_u128(2), None, false, RunState::Created);
    let fakes = Arc::new(
        fakes::Builder::new()
            .runs(Arc::new(fakes::FakeRuns::with(vec![
                (OWNER_TENANT, first.clone()),
                (OWNER_TENANT, second.clone()),
            ])))
            .executor(Arc::new(YieldingListing(Arc::new(
                crate::infra::executor::mock::MockRunExecutor::new(),
            ))))
            .limits(super::QueueLimits {
                queue_max_depth: 20,
                max_concurrent_runs: 1,
                queue_ttl_seconds: 7200,
            })
            .build()
            .await,
    );

    // The whole `Admitted` is kept, not just the decision: the slot inside it is
    // what the other caller counts.
    let one = {
        let fakes = Arc::clone(&fakes);
        let run = first.clone();
        tokio::spawn(async move { fakes.admission.admit(&ctx(OWNER_TENANT), &run).await })
    };
    let two = {
        let fakes = Arc::clone(&fakes);
        let run = second.clone();
        tokio::spawn(async move { fakes.admission.admit(&ctx(OWNER_TENANT), &run).await })
    };

    let outcomes = [one.await.unwrap(), two.await.unwrap()];
    let admitted = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    let refused = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Err(DomainError::ConcurrencyLimit { limit: 1 })))
        .count();
    assert_eq!(
        (admitted, refused),
        (1, 1),
        "a cap of one admits exactly one concurrent launch: {outcomes:?}"
    );
}

/// The other half: a **queued** run holds no cluster capacity, so its slot goes
/// back before `admit` returns. Without that release the cap would be spent by
/// runs that never started, and a platform with a full queue would refuse
/// launches cluster-wide.
///
/// The third launch is the discriminator: with the queued run's slot leaked it is
/// refused, and the platform is busy either way so its own outcome is `Queued`.
#[tokio::test]
async fn a_queued_run_gives_its_capacity_slot_back() {
    let first = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        true,
        RunState::Created,
    );
    let second = run_fixture(
        Uuid::from_u128(2),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let third = run_fixture(
        Uuid::from_u128(3),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let fakes = fakes::Builder::new()
        .runs(Arc::new(fakes::FakeRuns::with(vec![
            (OWNER_TENANT, first.clone()),
            (OWNER_TENANT, second.clone()),
            (OWNER_TENANT, third.clone()),
        ])))
        .limits(super::QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 2,
            queue_ttl_seconds: 7200,
        })
        .build()
        .await;

    // Held for the rest of the test, exactly as the launch path holds it.
    let dispatched = fakes
        .admission
        .admit(&ctx(OWNER_TENANT), &first)
        .await
        .expect("the platform is free");
    assert!(matches!(dispatched.admission, Admission::Dispatch { .. }));

    let queued = fakes
        .admission
        .admit(&ctx(OWNER_TENANT), &second)
        .await
        .expect("an exclusive occupant queues rather than refusing");
    // Kept alive on purpose: if the release happened on drop rather than inside
    // `admit`, dropping it here would make the third launch pass either way.
    assert!(matches!(queued.admission, Admission::Queued { .. }));

    assert!(
        matches!(
            fakes
                .admission
                .admit(&ctx(OWNER_TENANT), &third)
                .await
                .map(|admitted| admitted.admission),
            Ok(Admission::Queued { .. })
        ),
        "one dispatched run and one queued run must leave a slot under a cap of two"
    );
}

#[tokio::test]
async fn two_concurrent_exclusive_launches_produce_one_start_and_one_queued() {
    let first = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        true,
        RunState::Created,
    );
    let second = run_fixture(
        Uuid::from_u128(2),
        Some(PLATFORM_A),
        true,
        RunState::Created,
    );
    let fakes = Arc::new(
        fakes::Builder::new()
            .runs(Arc::new(fakes::FakeRuns::with(vec![
                (OWNER_TENANT, first.clone()),
                (OWNER_TENANT, second.clone()),
            ])))
            .build()
            .await,
    );

    let one = {
        let fakes = Arc::clone(&fakes);
        let run = first.clone();
        tokio::spawn(async move {
            fakes
                .admission
                .admit_decision(&ctx(OWNER_TENANT), &run)
                .await
        })
    };
    let two = {
        let fakes = Arc::clone(&fakes);
        let run = second.clone();
        tokio::spawn(async move {
            fakes
                .admission
                .admit_decision(&ctx(OWNER_TENANT), &run)
                .await
        })
    };

    let outcomes = vec![one.await.unwrap().unwrap(), two.await.unwrap().unwrap()];
    let dispatched = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Admission::Dispatch { .. }))
        .count();
    let queued = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Admission::Queued { .. }))
        .count();
    assert_eq!(
        (dispatched, queued),
        (1, 1),
        "exactly one exclusive run may start on an idle platform: {outcomes:?}"
    );
}

/// **The race the platform lock actually closes.** *"Two concurrent launches
/// against a queue one slot from full must not both observe room. Checked before
/// the occupancy read and the decision because a rejection writes no row"*
/// (`manager/src/services/run_queue.rs:635-639`).
///
/// The lease CAS cannot cover this one: it arbitrates the *platform*, and this
/// race is about the *depth read*. Two launches that both observe room both write
/// a row, and the platform's queue ends up over `queue_max_depth` with no error
/// anywhere — which is a capacity limit an operator set and the system silently
/// exceeded.
///
/// **The setup has to be one slot from full, not full.** With the queue already
/// at its limit both launches are refused whatever the interleaving, so that
/// version of the test is green with the lock deleted — which is how the first
/// draft of it was found. `queue_max_depth: 2` with one row queued leaves exactly
/// one slot, so the outcome is one `Queued` and one `QueueFull`; without the lock
/// both observe the free slot and both insert, taking the platform to depth 3 with
/// no error anywhere. Verified by removing the lock.
///
/// The interleaving is reachable only because `FakeQueue::queued_depth` yields —
/// see its doc comment.
#[tokio::test]
async fn two_concurrent_launches_cannot_both_observe_room_in_a_full_queue() {
    let first = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let second = run_fixture(
        Uuid::from_u128(2),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let existing = queued_row(
        Uuid::from_u128(0x11),
        OWNER_TENANT,
        Uuid::from_u128(0x99),
        PLATFORM_A,
        false,
        QueueState::Queued,
    );
    let fakes = Arc::new(
        fakes::Builder::new()
            .runs(Arc::new(fakes::FakeRuns::with(vec![
                (OWNER_TENANT, first.clone()),
                (OWNER_TENANT, second.clone()),
            ])))
            .queue(Arc::new(fakes::FakeQueue::with(vec![existing])))
            .limits(super::QueueLimits {
                queue_max_depth: 2,
                max_concurrent_runs: 0,
                queue_ttl_seconds: 7200,
            })
            .build()
            .await,
    );

    let one = {
        let fakes = Arc::clone(&fakes);
        let run = first.clone();
        tokio::spawn(async move {
            fakes
                .admission
                .admit_decision(&ctx(OWNER_TENANT), &run)
                .await
        })
    };
    let two = {
        let fakes = Arc::clone(&fakes);
        let run = second.clone();
        tokio::spawn(async move {
            fakes
                .admission
                .admit_decision(&ctx(OWNER_TENANT), &run)
                .await
        })
    };

    let outcomes = [one.await.unwrap(), two.await.unwrap()];
    let refused = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Err(DomainError::QueueFull { .. })))
        .count();
    let admitted = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Ok(Admission::Queued { .. })))
        .count();
    assert_eq!(
        (admitted, refused),
        (1, 1),
        "one slot admits exactly one launch: {outcomes:?}"
    );
    assert_eq!(
        fakes.queue.rows().len(),
        2,
        "the platform's queue must not have been taken past queue_max_depth"
    );
}

/// **The platform lock does not outlive `admit`.** A second admission on the same
/// platform completes after the first has returned, so nothing the first caller
/// goes on to do — the sync and the bundle build, minutes in production
/// (`run_queue.rs:569-572`) — can be blocking it. Bounded at 5 s, so a leaked
/// guard fails loudly rather than hanging CI.
///
/// # What this test is, and what the plan asked for
///
/// Step 6 specifies *"a submit that blocks on a barrier must not prevent a second
/// admission"*. **That property is unbuildable here, and the reason is
/// structural**: `admit` does not perform the submit — the caller does, after
/// `admit` returns — so there is nothing inside the critical section for a barrier
/// to hold, and no barrier appears below. Recorded as a plan defect rather than
/// simulated with a test whose name promised more than it did.
///
/// What is left is still worth having, and it is narrow: **this cannot fail unless
/// a guard escapes `admit`'s scope.** Verified by `std::mem::forget`ing the guard
/// inside `admit`, which makes the second call time out at 5.01 s with `Elapsed`.
/// It is a leak detector, not a concurrency test.
#[tokio::test]
async fn the_platform_lock_does_not_outlive_admit() {
    let first = run_fixture(
        Uuid::from_u128(1),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let second = run_fixture(
        Uuid::from_u128(2),
        Some(PLATFORM_A),
        false,
        RunState::Created,
    );
    let fakes = Arc::new(
        fakes::Builder::new()
            .runs(Arc::new(fakes::FakeRuns::with(vec![
                (OWNER_TENANT, first.clone()),
                (OWNER_TENANT, second.clone()),
            ])))
            .build()
            .await,
    );

    let admitted = fakes
        .admission
        .admit_decision(&ctx(OWNER_TENANT), &first)
        .await
        .unwrap();
    assert!(matches!(admitted, Admission::Dispatch { .. }));

    // The first `admit` has returned. If its guard were still alive, this would
    // block forever; the timeout turns that into a failure.
    let second_admission = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fakes.admission.admit_decision(&ctx(OWNER_TENANT), &second),
    )
    .await
    .expect("a second admission blocked while the first caller was submitting");
    assert!(second_admission.is_ok());
}
