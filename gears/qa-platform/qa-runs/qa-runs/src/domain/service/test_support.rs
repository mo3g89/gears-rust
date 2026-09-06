//! Shared test doubles for the `domain::service` unit tests.
//!
//! # Every double carries a real tenant id
//!
//! Not `Uuid::nil()`. A nil-tenant double is what hid qa-catalog's
//! background-write bug: nil compares equal to whatever a wrong-tenant write
//! produced, so the test could not distinguish "wrote under the right tenant"
//! from "wrote under none". [`OWNER_TENANT`] is a fixed non-nil value and
//! [`OTHER_TENANT`] is a second one, so a cross-tenant read has something to be
//! wrong about.
//!
//! # The rule `OwnedRunId` places on this file
//!
//! `domain::repos::runs_repo::OwnedRunId` states it explicitly: a token proves
//! only that *some* `RunsRepository::get` answered `Some` for this id under this
//! scope, so it is exactly as trustworthy as the repository that minted it. A
//! double whose `get` returns `Some` unconditionally mints tokens freely, and
//! every service test then "proves" a precheck it never exercised.
//!
//! [`MockRunsRepository::get`] therefore **applies tenant scoping to its
//! fixture**, via `scope.contains_uuid(OWNER_TENANT_ID, row_tenant)`, and
//! returns `None` when the scope does not admit the row's tenant.
//! `a_foreign_tenant_cannot_resolve_an_owned_run_id` pins it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError};
use qa_catalog_sdk::{
    BundleRequest, CustomPlan, CustomPlanEntry, NewCustomPlan, NewTestRepository, Plan, Product,
    QaCatalogClientV1, QaCatalogError, SshKey, SyncRequest, TestBundle, TestFileMeta,
    TestRepository, TestRepositoryUpdate, UniverseTest,
};
use qa_environments_sdk::{
    AcquireOutcome, Environment, EnvironmentPatch, LeaseMode, LeaseState, NewEnvironment,
    NewVariable, QaEnvironmentsClientV1, QaEnvironmentsError, Variable,
};
use qa_runs_sdk::{ExclusiveTier, Run, RunResult, RunState, RunTarget};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::pep_properties::{self, OWNER_TENANT_ID};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::DbProvider;
use super::launch::{Admission, Admitter, InlineDispatcher};
use super::watch::{RunWatcher, WatchTarget};
use crate::domain::error::DomainError;
use toolkit_odata::{ODataQuery, Page};

use crate::domain::service::admission::CapSlot;
use crate::domain::service::admission::tests::fakes::{FakeCatalog, FakeEnvironments};
use crate::domain::service::launch::Admitted;
use crate::domain::service::{AppServices, FlushReport, LogArchive, QueueLimits, ServiceDeps};
use crate::infra::ConcreteAppServices;
use crate::infra::executor::mock::MockRunExecutor;
use crate::infra::logs::RunLogBroadcaster;
use crate::infra::storage::entity::schedule_tick;
use crate::infra::storage::test_db::{inmem_db, scope};
use crate::infra::storage::{OrmQueueRepository, OrmRunsRepository, OrmSchedulesRepository};
use sea_orm::EntityTrait;
use toolkit_db::secure::SecureEntityExt;

use crate::domain::repos::SchedulesRepository;
use crate::domain::repos::{
    ArchivedLog, LogPosition, LogResume, NewRun, NewTestResult, OwnedRunId, RunLogsRepository,
    RunResultDelta, RunStatePatch, RunWithResult, RunsRepository, TestResultRow, TimeoutCandidate,
    WatchCandidate, Windowed,
};

/// Wrap a double's whole fixture as a single page.
///
/// The `page_info` is deliberately inert - no cursors, and a `limit` equal to
/// the row count - so that a test asserting on it would be asserting on this
/// helper rather than on any pagination behaviour. See the doc on
/// `MockRunsRepository::list_page`.
pub(in crate::domain::service) fn unfiltered_page<T>(items: Vec<T>) -> Page<T> {
    let limit = items.len() as u64;
    Page {
        items,
        page_info: toolkit_odata::PageInfo {
            next_cursor: None,
            prev_cursor: None,
            limit,
        },
    }
}

/// The tenant every fixture belongs to. Non-nil on purpose — see the module
/// header.
pub(super) const OWNER_TENANT: Uuid = Uuid::from_u128(0x0A11_0000_0000_0001);
/// A second, non-nil tenant, so a cross-tenant read has something to be wrong
/// about.
pub(super) const OTHER_TENANT: Uuid = Uuid::from_u128(0x0A11_0000_0000_0002);

pub(super) const REPO_ID: Uuid = Uuid::from_u128(0x0B01);
pub(super) const OTHER_REPO_ID: Uuid = Uuid::from_u128(0x0B02);
pub(super) const PLATFORM_ID: Uuid = Uuid::from_u128(0x0C01);
pub(super) const CUSTOM_PLAN_ID: Uuid = Uuid::from_u128(0x0D01);
pub(super) const QUEUE_ID: Uuid = Uuid::from_u128(0x0E01);

/// Build a `SecurityContext` for `tenant_id` with a fresh random subject.
pub fn ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap()
}

/// Always grants, returning a tenant `IN` constraint derived from the subject's
/// tenant — the shape a real PDP's default tenant-isolation policy produces, and
/// the shape `PolicyEnforcer::access_scope` compiles into row-level filtering.
///
/// A nil UUID yields **no** constraints, which is not a platform-wide grant:
/// every service method that reaches the PEP calls `access_scope` (never
/// `access_scope_with(require_constraints(false))`), so an empty constraint set
/// fails compilation and lands as `DomainError::Forbidden` for any of them.
/// (The six nil-tenant enumeration reads named in `domain::system_actor`'s
/// header don't reach the PEP at all any more — they elevate through
/// `domain::elevated::enumeration_scope` instead — but every other call in
/// this gear, and every write those six reads feed, still goes through
/// `access_scope` and is still denied by a nil-tenant context exactly as
/// described here.)
///
/// **Exercised, not just asserted:** `a_nil_tenant_context_is_denied_and_writes_nothing`
/// drives a whole launch under `ctx(Uuid::nil())` and pins `Forbidden` plus zero
/// rows and zero dispatches. Until Task 13's review nothing in this
/// crate passed a nil tenant to anything, so this paragraph — and the identical
/// claim in `domain::system_actor`, where it is a *security* property — rested
/// entirely on reading `authz-resolver-sdk`.
fn permissive_response(request: &EvaluationRequest) -> EvaluationResponse {
    let root_id = request
        .context
        .tenant_context
        .as_ref()
        .and_then(|tc| tc.root_id)
        .or_else(|| {
            request
                .subject
                .properties
                .get("tenant_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        })
        .filter(|id| !id.is_nil());

    let constraints = match root_id {
        Some(id) => vec![Constraint {
            predicates: vec![Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [id],
            ))],
        }],
        None => vec![],
    };

    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints,
            ..Default::default()
        },
    }
}

/// `pub` rather than `pub(super)`: `infra::logs::archive`'s tests build a
/// `PolicyEnforcer` over this fake to exercise `RunLogArchive::flush`, and
/// that module is not a descendant of `domain::service`, so the narrower
/// visibility this file otherwise uses throughout would not reach it. Plain
/// `pub` and not `pub(crate)` because `test_support` itself is already
/// `pub(crate)` (`domain/service/mod.rs`) — clippy's `redundant_pub_crate`
/// (`-D warnings`) rejects re-stating that cap on an item one level down, so
/// the effective visibility is still crate-only.
pub struct PermissiveAuthZ;

#[async_trait]
impl AuthZResolverClient for PermissiveAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(permissive_response(&request))
    }
}

/// The mocks never touch the database — every mock method ignores its
/// `DBRunner` — so a real handle is needed only to satisfy `Arc<DbProvider>` and
/// to produce the `DbConn` values that are passed through. No migrations.
///
/// `pub` for the same reason as [`PermissiveAuthZ`]:
/// `infra::logs::archive::RunLogArchive` needs a real `Arc<DbProvider>` to
/// obtain a `DbConn` for `MockRunsRepository::append_log`, which is sealed to
/// only accept a real connection type.
pub async fn test_db_provider() -> Arc<DbProvider> {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("failed to connect to in-memory sqlite");
    Arc::new(DBProvider::<DomainError>::new(db))
}

// ---------------------------------------------------------------------------
// Runs repository double
// ---------------------------------------------------------------------------

/// In-memory `RunsRepository`, storing `(tenant_id, Run)` pairs.
///
/// Only the methods the launch path calls are meaningful: `create`, `list`,
/// `get`, `update_state`. The rest return an error naming themselves, so a test
/// that reaches an unexpected method fails loudly instead of silently
/// succeeding.
///
/// **`pub`, not `pub(super)`.** Every other type in this file stays at
/// `pub(super)` because every other consumer lives inside `domain::service`.
/// This one does not: `infra::logs::archive`'s test module drives
/// `RunLogArchive<MockRunsRepository>` directly, per Task 3's brief, and that
/// module is a sibling of `domain::service`, not a descendant of it — so the
/// narrower visibility would not reach it. Plain `pub`, not `pub(crate)`: see
/// [`PermissiveAuthZ`]'s doc for why clippy requires that spelling here even
/// though the effective visibility is crate-only either way.
pub struct MockRunsRepository {
    rows: Mutex<Vec<(Uuid, Run)>>,
    /// Names whose first `create` must answer `RunNameExists`, simulating a
    /// concurrent launch that won the unique index. Popped as they fire.
    collide_once: Mutex<Vec<String>>,
    /// Every `NewRun` handed to `create`, in order.
    created: Mutex<Vec<NewRun>>,
    /// Every `(from, to)` pair handed to `update_state`, in order.
    transitions: Mutex<Vec<(RunState, RunState)>>,
    /// Archived logs, keyed by run id — the in-memory stand-in for
    /// `qa_run_logs`. `RunLogsRepository::append_log` concatenates into an
    /// entry here; `get_log` clones one out.
    logs: Mutex<HashMap<Uuid, ArchivedLog>>,
    /// Every `append_log` call, counted regardless of outcome. Lets
    /// `RunLogArchive`'s tests assert that an idle `flush_due` touches the
    /// database zero times.
    append_calls: Mutex<usize>,
    /// Set by [`Self::fail_next_append`]: the next `append_log` call returns
    /// an injected error once, then this clears itself. This is the
    /// failure-injection seam `a_failed_flush_keeps_its_text_for_the_next_one`
    /// needs and `MockRunsRepository` did not have before Task 3 — every
    /// other method on this double either always succeeds or always fails,
    /// and this is the first one that needs to do both from one test.
    fail_next_append: Mutex<bool>,
    /// Set by [`Self::block_next_append`]: taken by the next `append_log`
    /// call, which then rendezvous-blocks on it before failing. See
    /// [`AppendGate`]'s own doc for why this exists.
    append_gate: Mutex<Option<AppendGate>>,
}

/// A one-shot rendezvous between a test and one gated `append_log` call, added
/// for `infra::logs::archive`'s
/// `a_flush_that_fails_while_a_new_line_arrives_preserves_arrival_order` (fix
/// round 1, Important 2).
///
/// `fail_next_append` alone cannot exercise `RunLogArchive::restore`'s
/// order-preserving merge branch (`if let Some(newer) = ...`): that branch
/// only runs when something lands in the pending map *while a failing write
/// is still in flight*, and an un-gated mock's `append_log` fails and returns
/// synchronously, so nothing can `record` in the gap. This type creates that
/// gap on purpose: the gated call notifies [`Self::started`] the instant it is
/// entered, then waits on [`Self::resume`] before returning its injected
/// error, giving a test a window in which to call `record` and prove the
/// merge orders old-before-new.
#[derive(Clone)]
pub struct AppendGate {
    started: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}

impl AppendGate {
    /// Resolves once the gated `append_log` call has been entered — i.e. once
    /// it is safe to `record` a line that must land *after* the text already
    /// captured by that call's caller.
    pub async fn wait_for_entry(&self) {
        self.started.notified().await;
    }

    /// Let the gated `append_log` call proceed to its (failing) return.
    pub fn release(&self) {
        self.resume.notify_one();
    }
}

impl MockRunsRepository {
    /// `pub`, not `pub(super)` — see the struct's own doc.
    pub fn empty() -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
            collide_once: Mutex::new(Vec::new()),
            created: Mutex::new(Vec::new()),
            transitions: Mutex::new(Vec::new()),
            logs: Mutex::new(HashMap::new()),
            append_calls: Mutex::new(0),
            fail_next_append: Mutex::new(false),
            append_gate: Mutex::new(None),
        }
    }

    /// Seed an already-stored run, so `next_sequence` has something to count.
    pub(super) fn with_existing(self, tenant_id: Uuid, name: &str) -> Self {
        self.rows
            .lock()
            .unwrap()
            .push((tenant_id, run_fixture(name)));
        self
    }

    /// Make the next `create` of `name` collide exactly once.
    pub(super) fn colliding_on(self, name: &str) -> Self {
        self.collide_once.lock().unwrap().push(name.to_owned());
        self
    }

    pub(super) fn created(&self) -> Vec<NewRun> {
        self.created.lock().unwrap().clone()
    }

    pub(super) fn transitions(&self) -> Vec<(RunState, RunState)> {
        self.transitions.lock().unwrap().clone()
    }

    pub(super) fn stored(&self) -> Vec<Run> {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .map(|(_, run)| run.clone())
            .collect()
    }

    /// `run_id`'s archived text, or `""` if `append_log` has never been
    /// called for it. `pub` — see the struct's own doc.
    pub fn archived_text(&self, run_id: Uuid) -> String {
        self.logs
            .lock()
            .unwrap()
            .get(&run_id)
            .map(|log| log.text.clone())
            .unwrap_or_default()
    }

    /// How many times `append_log` has been invoked, successful or not.
    /// `pub` — see the struct's own doc.
    pub fn append_call_count(&self) -> usize {
        *self.append_calls.lock().unwrap()
    }

    /// Make the *next* `append_log` call fail once, then behave normally
    /// again. `pub` — see the struct's own doc.
    pub fn fail_next_append(&self) {
        *self.fail_next_append.lock().unwrap() = true;
    }

    /// Gate the *next* `append_log` call: it will block, once entered, until
    /// the returned [`AppendGate`] is released, then fail. `pub` — see the
    /// struct's own doc.
    pub fn block_next_append(&self) -> AppendGate {
        let gate = AppendGate {
            started: Arc::new(tokio::sync::Notify::new()),
            resume: Arc::new(tokio::sync::Notify::new()),
        };
        *self.append_gate.lock().unwrap() = Some(gate.clone());
        gate
    }
}

fn run_fixture(name: &str) -> Run {
    let now = OffsetDateTime::now_utc();
    Run {
        id: Uuid::new_v4(),
        name: name.to_owned(),
        target: RunTarget::Plan {
            repo_id: REPO_ID,
            path: "tests/plan.yaml".to_owned(),
        },
        platform_id: None,
        test_version: None,
        app_version: None,
        app_build: None,
        state: RunState::Created,
        resolved_exclusive: false,
        exclusive_tier: ExclusiveTier::Default,
        is_validation: false,
        parameters: Vec::new(),
        include_tags: Vec::new(),
        exclude_tags: Vec::new(),
        source: qa_runs_sdk::RunSource::Manual,
        schedule_id: None,
        bundle_ids: Vec::new(),
        execution_ref: None,
        log_storage_ref: None,
        timeout_at: None,
        started_at: None,
        finished_at: None,
        error: None,
        created_at: now,
        updated_at: now,
    }
}

fn unsupported(method: &str) -> DomainError {
    DomainError::Internal(format!(
        "MockRunsRepository::{method} is not used by these tests"
    ))
}

#[async_trait]
impl RunsRepository for MockRunsRepository {
    async fn create<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        tenant_id: Uuid,
        new: NewRun,
    ) -> Result<Run, DomainError> {
        {
            let mut collide = self.collide_once.lock().unwrap();
            if let Some(position) = collide.iter().position(|name| *name == new.name) {
                collide.remove(position);
                // Model the concurrent launch that actually won the unique
                // index: the row now exists, so the retry's `list` sees it and
                // `next_sequence` moves on. A collision that stored nothing
                // would let the retry recompute the same number and "pass"
                // without exercising the renumbering at all.
                drop(collide);
                self.rows
                    .lock()
                    .unwrap()
                    .push((tenant_id, run_fixture(&new.name)));
                return Err(DomainError::RunNameExists { name: new.name });
            }
        }
        self.created.lock().unwrap().push(new.clone());

        let now = OffsetDateTime::now_utc();
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
            created_at: now,
            updated_at: now,
        };
        self.rows.lock().unwrap().push((tenant_id, run.clone()));
        Ok(run)
    }

    /// **Tenant-scoped**, which is the rule `OwnedRunId` places on every double:
    /// a `get` that answered `Some` unconditionally would mint tokens freely and
    /// make every ownership test vacuous.
    async fn get<C: DBRunner>(
        &self,
        _runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Run>, DomainError> {
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

    /// **Scope-only, and the `OData` query is ignored.**
    ///
    /// Filtering, ordering and cursors are SQL translation, and translating
    /// them in a `Vec`-backed double would be a second implementation of
    /// `paginate_odata` that could agree with the real one only by accident.
    /// So no test reached through this double may claim to pin `$filter`,
    /// `$orderby` or paging behaviour. What this *does* model, and what the
    /// tests here are about, is that the caller's scope decides which rows are
    /// visible.
    ///
    /// **Nothing else covers it either.** This doc used to add that
    /// `infra::storage`'s own tests own that behaviour; they do not -
    /// `list_page` has no test in either repository or in the integration
    /// tier. Declaring a gap and pointing at coverage that does not exist is
    /// worse than declaring it plainly, because it stops the next reader
    /// looking.
    /// **Paired with `RunResult::default()`, not a tracked result** — this
    /// double never tracks one; [`Self::get_result`] returns `unsupported` for
    /// the same reason. No test reached through this double may claim to pin
    /// `RunWithResult::result`.
    async fn list_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        _query: &ODataQuery,
    ) -> Result<Page<RunWithResult>, DomainError> {
        let items = self.list(runner, scope).await?;
        Ok(unfiltered_page(
            items
                .into_iter()
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
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|(tenant_id, _)| scope.contains_uuid(OWNER_TENANT_ID, *tenant_id))
            .map(|(_, run)| run.clone())
            .collect())
    }

    /// Not reached: this double backs the launch and naming tests, and the
    /// reconciler sweep is a read no launch performs. `FakeRuns` in
    /// `admission_tests` models it for the service tests that do.
    async fn list_finished_since<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _since: OffsetDateTime,
        _limit: u32,
    ) -> Result<Vec<Run>, DomainError> {
        Err(unsupported("list_finished_since"))
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
        self.transitions.lock().unwrap().push((from, to));
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
        Ok(true)
    }

    async fn set_execution_ref<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
        _execution_ref: &str,
    ) -> Result<bool, DomainError> {
        Err(unsupported("set_execution_ref"))
    }

    /// Not reached by the launch path: `bundle_ids` is written empty at insert
    /// and filled in by `service::dispatch`, which the launch tests replace with
    /// a seam double. The concurrency core's own tests use their own runs double.
    async fn set_bundle_ids<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
        _bundle_ids: &[Uuid],
    ) -> Result<bool, DomainError> {
        Err(unsupported("set_bundle_ids"))
    }

    async fn add_result_counts<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
        _delta: RunResultDelta,
    ) -> Result<bool, DomainError> {
        Err(unsupported("add_result_counts"))
    }

    async fn get_result<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _id: Uuid,
    ) -> Result<Option<RunResult>, DomainError> {
        Err(unsupported("get_result"))
    }

    async fn list_timeout_candidates<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _now: OffsetDateTime,
        _after: Option<Uuid>,
    ) -> Result<Windowed<TimeoutCandidate>, DomainError> {
        Err(unsupported("list_timeout_candidates"))
    }

    async fn list_watch_candidates<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _after: Option<Uuid>,
    ) -> Result<Windowed<WatchCandidate>, DomainError> {
        Err(unsupported("list_watch_candidates"))
    }

    async fn upsert_test_result<C: DBRunner>(
        &self,
        _runner: &C,
        _results_scope: &AccessScope,
        _tenant_id: Uuid,
        _run: OwnedRunId,
        _result: NewTestResult,
    ) -> Result<TestResultRow, DomainError> {
        Err(unsupported("upsert_test_result"))
    }

    async fn list_test_results<C: DBRunner>(
        &self,
        _runner: &C,
        _results_scope: &AccessScope,
        _run: OwnedRunId,
    ) -> Result<Vec<TestResultRow>, DomainError> {
        Err(unsupported("list_test_results"))
    }
}

/// In-memory stand-in for `qa_run_logs`, so Tasks 3-6 can test the
/// accumulator and the HTTP read path without a database. `scope` is unused
/// here on purpose: unlike `OrmRunsRepository`'s implementation, there is no
/// separate table for a real scope predicate to be checked against, so the
/// tenant isolation this double would need to model is left to the tests that
/// actually drive `OrmRunsRepository` (`infra::storage::run_logs_sea_repo`'s
/// `a_foreign_tenant_cannot_read_an_archived_log`).
#[async_trait]
impl RunLogsRepository for MockRunsRepository {
    async fn append_log<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        run_id: Uuid,
        _tenant_id: Uuid,
        text: &str,
        lines: i64,
    ) -> Result<(), DomainError> {
        *self.append_calls.lock().unwrap() += 1;

        // Rendezvous-block, consumed on this call only, then fail — see
        // `AppendGate`'s own doc for why this exists (round 1's Important 2:
        // exercising `RunLogArchive::restore`'s merge branch needs a `record`
        // to land while a failing write is still in flight, which an
        // un-gated failure can never allow).
        let gate = self.append_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.started.notify_one();
            gate.resume.notified().await;
            return Err(DomainError::Internal(
                "MockRunsRepository: injected append_log failure (gated)".to_owned(),
            ));
        }

        // Injected failure, consumed on this call only — see
        // `fail_next_append`'s own doc for why this double needed a seam that
        // can fail on demand rather than always or never.
        {
            let mut fail_next = self.fail_next_append.lock().unwrap();
            if *fail_next {
                *fail_next = false;
                return Err(DomainError::Internal(
                    "MockRunsRepository: injected append_log failure".to_owned(),
                ));
            }
        }

        let mut logs = self.logs.lock().unwrap();
        match logs.get_mut(&run_id) {
            Some(existing) => {
                existing.text.push_str(text);
                existing.lines += lines;
            }
            None => {
                logs.insert(
                    run_id,
                    ArchivedLog {
                        text: text.to_owned(),
                        lines,
                    },
                );
            }
        }
        Ok(())
    }

    async fn get_log<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<Option<ArchivedLog>, DomainError> {
        Ok(self.logs.lock().unwrap().get(&run_id).cloned())
    }

    /// Mirrors `infra::storage::run_logs_sea_repo`'s real implementation —
    /// counting lines by their `"[{node}] "` prefix — rather than stubbing
    /// `unsupported`, so a test built over this double can exercise
    /// `RunLogArchive::resume_positions` too. `since_time` is always `None`:
    /// this double has no `updated_at` column to stand in for it, and no
    /// test here needs one — see `LogPosition`'s doc for what a real
    /// implementation uses it for.
    async fn log_resume_positions<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<LogResume, DomainError> {
        let Some(log) = self.logs.lock().unwrap().get(&run_id).cloned() else {
            return Ok(LogResume::default());
        };
        let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
        for line in log.text.lines() {
            if let Some(rest) = line.strip_prefix('[')
                && let Some(end) = rest.find(']')
            {
                *counts.entry(rest[..end].to_owned()).or_insert(0) += 1;
            }
        }
        Ok(counts
            .into_iter()
            .map(|(node, lines)| {
                (
                    node,
                    LogPosition {
                        lines,
                        since_time: None,
                    },
                )
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Catalog double
// ---------------------------------------------------------------------------

fn catalog_unsupported(method: &str) -> QaCatalogError {
    QaCatalogError::internal(format!("MockCatalog::{method} is not used by these tests")).create()
}

/// The resource type a per-file `TEST_META` failure maps through in the real
/// gear (`qa-catalog/src/api/rest/error.rs`'s `CatalogResourceError`, shared by
/// `DomainError::FileNotFound` and `DomainError::Forbidden`). Declared here so
/// [`MockCatalog::get_test_meta`]'s `NotFound` and `PermissionDenied` carry the
/// same *category* qa-catalog would produce -- the category
/// `LaunchService::gather_group_meta` discriminates on (review finding #9) --
/// rather than one double answering `internal` for every cause.
#[toolkit::api::canonical_prelude::resource_error(toolkit_gts::gts_id!(
    "cf.qa.catalog.entry.v1~"
))]
struct MockCatalogEntryError;

/// The resource type a `get_plan` failure maps through in the real gear
/// (`qa-catalog/src/api/rest/error.rs`'s `PlanResourceError` for
/// `DomainError::PlanNotFound`, sharing `CatalogResourceError`'s
/// `permission_denied` for `Forbidden` -- both `NotFound`-shaped errors here
/// regardless, since [`MockCatalog::get_plan`]'s match does not look at
/// `resource_type` any more than `gather_group_meta`'s does). Declared
/// separately from [`MockCatalogEntryError`] so a `get_plan` failure and a
/// `get_test_meta` failure are visibly two different calls in a test's
/// fixture, matching qa-catalog's own split, even though both collapse to the
/// same `CanonicalError` variants.
#[toolkit::api::canonical_prelude::resource_error(toolkit_gts::gts_id!(
    "cf.qa.catalog.plan.v1~"
))]
struct MockCatalogPlanError;

/// In-memory `QaCatalogClientV1` serving one repository default branch, one
/// discovered plan, one custom plan, and per-file `TEST_META`.
pub(super) struct MockCatalog {
    pub(super) repo_default_branch: String,
    pub(super) plan: Option<Plan>,
    /// `(repo_id, plan_path) -> plan`, consulted by `get_plan` before `plan`.
    ///
    /// A custom plan resolves each nested plan by `(repo_id, branch, plan_path)`,
    /// so a double whose `get_plan` ignores the path cannot tell two nested plans
    /// apart — and a test claiming "plan A declared exclusive, plan B did not"
    /// would be asserting against one fixture answering for both.
    pub(super) plans_by_path: Vec<((Uuid, String), Plan)>,
    /// `(repo_id, plan_path)` pairs whose `get_plan` answers `NotFound`:
    /// legacy's "nested plan not resolvable on this branch" case. Contributes
    /// nothing and must not fail the launch.
    pub(super) unresolvable_plans: Vec<(Uuid, String)>,
    /// `(repo_id, plan_path)` pairs whose `get_plan` answers `PermissionDenied`
    /// — a policy that denies this gear's system actor the read, as opposed to
    /// the plan simply not existing. Review finding #9's fix round 1: this
    /// must fail the launch, never resolve it parallel. Checked before
    /// [`Self::unresolvable_plans`], though a test should only ever put one
    /// `(repo_id, plan_path)` in one of the two.
    pub(super) denied_plans: Vec<(Uuid, String)>,
    /// Every `(repo_id, plan_path)` `get_plan` was asked for, in order.
    pub(super) plan_lookups: Mutex<Vec<(Uuid, String)>>,
    pub(super) custom_plan: Option<CustomPlan>,
    /// `path -> meta`. A path absent from this map is **unreadable**, which is
    /// what makes the batched call fail the way qa-catalog's really does.
    pub(super) metas: Vec<(String, TestFileMeta)>,
    /// When set, every `get_test_meta` call fails outright — the "catalog is
    /// down" case, as opposed to "one file is missing".
    pub(super) meta_unavailable: bool,
    /// When set, every `get_test_meta` call answers `PermissionDenied` — a
    /// policy that denies this gear's system actor the catalog read, as
    /// opposed to a fault. Review finding #9: this must fail the launch, never
    /// resolve it parallel. Checked before [`Self::meta_unavailable`], though
    /// a test should only ever set one.
    pub(super) meta_denied: bool,
    /// Branches `get_plan`/`get_test_meta` were asked for, in order.
    pub(super) branches_seen: Mutex<Vec<String>>,
    /// Number of `get_test_meta` calls, so a short-circuit can be proven by
    /// absence.
    pub(super) meta_calls: Mutex<usize>,
    /// Number of `sync_repo` calls. Resolution must never force a sync.
    pub(super) sync_calls: Mutex<usize>,
}

impl MockCatalog {
    pub(super) fn new() -> Self {
        Self {
            repo_default_branch: "main".to_owned(),
            plan: None,
            plans_by_path: Vec::new(),
            unresolvable_plans: Vec::new(),
            denied_plans: Vec::new(),
            plan_lookups: Mutex::new(Vec::new()),
            custom_plan: None,
            metas: Vec::new(),
            meta_unavailable: false,
            meta_denied: false,
            branches_seen: Mutex::new(Vec::new()),
            meta_calls: Mutex::new(0),
            sync_calls: Mutex::new(0),
        }
    }

    pub(super) fn with_repo_default(mut self, branch: &str) -> Self {
        self.repo_default_branch = branch.to_owned();
        self
    }

    pub(super) fn with_plan(mut self, plan: Plan) -> Self {
        self.plan = Some(plan);
        self
    }

    /// Register a nested plan at `(repo_id, plan_path)`, as a custom plan's
    /// `plan_path` resolves it.
    pub(super) fn with_plan_at(mut self, repo_id: Uuid, plan_path: &str, plan: Plan) -> Self {
        self.plans_by_path
            .push(((repo_id, plan_path.to_owned()), plan));
        self
    }

    /// Make `(repo_id, plan_path)` fail to resolve — legacy's unresolved nested
    /// plan, which contributes nothing and must not fail the launch.
    pub(super) fn with_unresolvable_plan(mut self, repo_id: Uuid, plan_path: &str) -> Self {
        self.unresolvable_plans
            .push((repo_id, plan_path.to_owned()));
        self
    }

    /// Make `(repo_id, plan_path)`'s `get_plan` answer `PermissionDenied`, as
    /// it would if a policy denied this gear's system actor the read. Review
    /// finding #9's fix-round-1 fixture: the plan this deployment cannot read
    /// may still declare `exclusive: True`, and the point is that the launch
    /// must fail rather than resolve as if the plan simply were not there.
    pub(super) fn with_denied_plan(mut self, repo_id: Uuid, plan_path: &str) -> Self {
        self.denied_plans.push((repo_id, plan_path.to_owned()));
        self
    }

    pub(super) fn plan_lookups(&self) -> Vec<(Uuid, String)> {
        self.plan_lookups.lock().unwrap().clone()
    }

    pub(super) fn with_custom_plan(mut self, plan: CustomPlan) -> Self {
        self.custom_plan = Some(plan);
        self
    }

    pub(super) fn with_meta(mut self, path: &str, meta: TestFileMeta) -> Self {
        self.metas.push((path.to_owned(), meta));
        self
    }

    pub(super) fn meta_unavailable(mut self) -> Self {
        self.meta_unavailable = true;
        self
    }

    /// Every `get_test_meta` call answers `PermissionDenied`, as it would if a
    /// policy denied this gear's system actor the catalog read. Review finding
    /// #9's fixture: the file this deployment cannot read may still declare
    /// `exclusive: True`, and the point is that the launch must fail rather
    /// than resolve as if nobody had an opinion.
    pub(super) fn deny_test_meta(mut self) -> Self {
        self.meta_denied = true;
        self
    }

    pub(super) fn meta_calls(&self) -> usize {
        *self.meta_calls.lock().unwrap()
    }

    pub(super) fn sync_calls(&self) -> usize {
        *self.sync_calls.lock().unwrap()
    }

    pub(super) fn branches_seen(&self) -> Vec<String> {
        self.branches_seen.lock().unwrap().clone()
    }
}

/// A discovered-plan fixture.
pub(super) fn plan_fixture(name: &str, test_files: &[&str]) -> Plan {
    Plan {
        repo_id: REPO_ID,
        product_id: Uuid::from_u128(0x0F01),
        branch: "main".to_owned(),
        path: "tests/plan.yaml".to_owned(),
        name: name.to_owned(),
        test_files: test_files.iter().map(|f| (*f).to_owned()).collect(),
        timeout_seconds: Some(300),
        tags: Vec::new(),
        validation: false,
        exclusive: None,
    }
}

/// A `TEST_META` fixture.
pub(super) fn meta_fixture(path: &str, tags: &[&str], exclusive: Option<bool>) -> TestFileMeta {
    TestFileMeta {
        path: path.to_owned(),
        title: None,
        tags: tags.iter().map(|t| (*t).to_owned()).collect(),
        exclusive,
        bugs: Vec::new(),
    }
}

/// A custom-plan fixture whose entries name **no** nested plan.
///
/// That is the shape a row written before `CustomPlanEntry::plan_path` existed
/// decodes to, and the shape an operator who picked files individually produces.
/// Use [`custom_plan_fixture_nested`] when the nested plan's `plan.yaml` tier is
/// what a test is about.
pub(super) fn custom_plan_fixture(files: &[(Uuid, &str)]) -> CustomPlan {
    custom_plan_fixture_nested(
        &files
            .iter()
            .map(|(repo, path)| (*repo, *path, None))
            .collect::<Vec<_>>(),
    )
}

/// A custom-plan fixture whose entries may name the nested plan they belong to.
pub(super) fn custom_plan_fixture_nested(files: &[(Uuid, &str, Option<&str>)]) -> CustomPlan {
    let now = OffsetDateTime::now_utc();
    CustomPlan {
        id: CUSTOM_PLAN_ID,
        name: "mixed".to_owned(),
        files: files
            .iter()
            .map(|(repo, path, plan_path)| CustomPlanEntry {
                repo_id: *repo,
                path: (*path).to_owned(),
                plan_path: plan_path.map(str::to_owned),
            })
            .collect(),
        tags: Vec::new(),
        timeout_seconds: None,
        created_at: now,
        updated_at: now,
    }
}

#[async_trait]
impl QaCatalogClientV1 for MockCatalog {
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

    async fn get_repo(
        &self,
        _ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<TestRepository, QaCatalogError> {
        let now = OffsetDateTime::now_utc();
        Ok(TestRepository {
            id,
            product_id: Uuid::from_u128(0x0F01),
            name: "repo".to_owned(),
            url: "https://example.invalid/repo.git".to_owned(),
            default_branch: self.repo_default_branch.clone(),
            content_root: String::new(),
            credential_ref: None,
            last_synced_at: Some(now),
            sync_error: None,
            created_at: now,
            updated_at: now,
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

    async fn delete_repo(&self, _ctx: &SecurityContext, _id: Uuid) -> Result<(), QaCatalogError> {
        Err(catalog_unsupported("delete_repo"))
    }

    async fn sync_repo(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _req: SyncRequest,
    ) -> Result<TestRepository, QaCatalogError> {
        *self.sync_calls.lock().unwrap() += 1;
        Err(catalog_unsupported("sync_repo"))
    }

    async fn list_branches(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<Vec<String>, QaCatalogError> {
        Err(catalog_unsupported("list_branches"))
    }

    async fn list_plans(
        &self,
        _ctx: &SecurityContext,
        _repo_id: Uuid,
        _branch: &str,
    ) -> Result<Vec<Plan>, QaCatalogError> {
        Err(catalog_unsupported("list_plans"))
    }

    /// **The failure category matters here too, for the same reason it does in
    /// [`Self::get_test_meta`] (review finding #9's fix round 1).** A
    /// `(repo_id, plan_path)` in [`Self::unresolvable_plans`] answers
    /// `NotFound` — a genuinely missing/unresolvable `plan.yaml` — and one in
    /// [`Self::denied_plans`] answers `PermissionDenied`. Before that fix
    /// round, both used `internal(...)`, indistinguishable from each other and
    /// from a real fault; that is exactly the defect this double must not
    /// reintroduce.
    async fn get_plan(
        &self,
        _ctx: &SecurityContext,
        repo_id: Uuid,
        branch: &str,
        path: &str,
    ) -> Result<Plan, QaCatalogError> {
        self.branches_seen.lock().unwrap().push(branch.to_owned());
        self.plan_lookups
            .lock()
            .unwrap()
            .push((repo_id, path.to_owned()));

        if self
            .denied_plans
            .iter()
            .any(|(r, p)| *r == repo_id && p == path)
        {
            return Err(MockCatalogPlanError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create());
        }
        if self
            .unresolvable_plans
            .iter()
            .any(|(r, p)| *r == repo_id && p == path)
        {
            return Err(MockCatalogPlanError::not_found("plan not on this branch")
                .with_resource(path.to_owned())
                .create());
        }
        if let Some((_, plan)) = self
            .plans_by_path
            .iter()
            .find(|((r, p), _)| *r == repo_id && p == path)
        {
            return Ok(plan.clone());
        }
        self.plan
            .clone()
            .ok_or_else(|| QaCatalogError::internal("no plan fixture").create())
    }

    /// All-or-nothing, exactly as the real one is: it fails the whole call on
    /// the first file it cannot read
    /// (`qa-catalog/src/domain/service/plans.rs`, the `read_to_string` arm).
    /// The launch path's per-file fallback exists because of this.
    ///
    /// **The failure category matters and is chosen deliberately, not just
    /// its presence.** A path absent from `self.metas` answers `NotFound` --
    /// what a genuinely missing file looks like through this SDK
    /// (`qa-catalog/src/api/rest/error.rs`'s `DomainError::FileNotFound` ->
    /// `CatalogResourceError::not_found`) -- and `gather_group_meta` omits it.
    /// [`Self::meta_denied`] answers `PermissionDenied`, and
    /// [`Self::meta_unavailable`] answers a plain `Internal`; `gather_group_meta`
    /// must fail the launch on both, per review finding #9. Before that
    /// finding, every one of these three cases used `internal(...)` and so
    /// were indistinguishable -- which is exactly the defect this double must
    /// not reintroduce.
    async fn get_test_meta(
        &self,
        _ctx: &SecurityContext,
        _repo_id: Uuid,
        branch: &str,
        files: &[String],
    ) -> Result<Vec<TestFileMeta>, QaCatalogError> {
        *self.meta_calls.lock().unwrap() += 1;
        self.branches_seen.lock().unwrap().push(branch.to_owned());
        if self.meta_denied {
            return Err(MockCatalogEntryError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create());
        }
        if self.meta_unavailable {
            return Err(QaCatalogError::internal("catalog unavailable").create());
        }
        let mut out = Vec::with_capacity(files.len());
        for file in files {
            let found = self
                .metas
                .iter()
                .find(|(path, _)| path == file)
                .map(|(_, meta)| meta.clone());
            match found {
                Some(meta) => out.push(meta),
                None => {
                    return Err(MockCatalogEntryError::not_found(format!(
                        "file not found: {file}"
                    ))
                    .with_resource(file.clone())
                    .create());
                }
            }
        }
        Ok(out)
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
        _id: Uuid,
    ) -> Result<CustomPlan, QaCatalogError> {
        self.custom_plan
            .clone()
            .ok_or_else(|| QaCatalogError::internal("no custom plan fixture").create())
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

    async fn list_products(&self, _ctx: &SecurityContext) -> Result<Vec<Product>, QaCatalogError> {
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

    async fn list_ssh_keys(&self, _ctx: &SecurityContext) -> Result<Vec<SshKey>, QaCatalogError> {
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
        _req: BundleRequest,
    ) -> Result<TestBundle, QaCatalogError> {
        Err(catalog_unsupported("create_bundle"))
    }

    async fn get_bundle_content(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<Vec<u8>, QaCatalogError> {
        Err(catalog_unsupported("get_bundle_content"))
    }
}

// ---------------------------------------------------------------------------
// Environments double
// ---------------------------------------------------------------------------

fn environments_unsupported(method: &str) -> QaEnvironmentsError {
    QaEnvironmentsError::internal(format!(
        "MockEnvironments::{method} is not used by these tests"
    ))
    .create()
}

/// In-memory `QaEnvironmentsClientV1` holding platforms by tenant.
///
/// `get_environment` answers only for a platform whose owning tenant matches the
/// caller's — which is exactly what the real gear's PEP does, and what makes
/// `a_platform_owned_by_another_tenant_fails_the_launch` a real test rather than
/// a tautology.
pub(super) struct MockEnvironments {
    platforms: Vec<(Uuid, Environment)>,
    lookups: Mutex<Vec<Uuid>>,
}

impl MockEnvironments {
    pub(super) fn with_platform(tenant_id: Uuid, platform: Environment) -> Self {
        Self {
            platforms: vec![(tenant_id, platform)],
            lookups: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn empty() -> Self {
        Self {
            platforms: Vec::new(),
            lookups: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn lookups(&self) -> Vec<Uuid> {
        self.lookups.lock().unwrap().clone()
    }
}

pub(super) fn platform_fixture(version: Option<&str>, build: Option<&str>) -> Environment {
    let now = OffsetDateTime::now_utc();
    Environment {
        id: PLATFORM_ID,
        name: "staging".to_owned(),
        // Required since qa-environments' Task 20b.
        product_id: uuid::Uuid::from_u128(0x9001),
        description: None,
        available: true,
        observed_version: version.map(str::to_owned),
        observed_build: build.map(str::to_owned),
        default_branch: None,
        is_default: false,
        version_detect_error: None,
        version_detected_at: None,
        // **The credential lives here since Task 19.** This fixture used to
        // carry `kubeconfig_credstore_ref: "credstore://kubeconfig"` with an
        // empty `credentials` beside it, and dispatch derived the key from the
        // plugin. The column is gone, `m20260903_000012` moved every row's
        // reference into this list, and `plugin_dispatch` reads only this --
        // so an empty list here would mean "this environment stores no
        // credential", which is a different fixture.
        // Keyed with the **scripted plugin's own** key, because that is what
        // the key now has to be: before Task 19 dispatch derived it from
        // `sole_required_secret_key(plugin.credential_schema())` for a single
        // unkeyed legacy reference, so any key in the fixture would do. The
        // key is read from the row now, so a fixture keyed "kubeconfig" would
        // be an environment whose credential this plugin does not declare --
        // which is a real state, and a different test.
        credentials: vec![qa_environments_sdk::EnvironmentCredential {
            key: crate::domain::service::admission::tests::fakes::PLUGIN_SECRET_KEY.to_owned(),
            credstore_ref: "credstore://kubeconfig".to_owned(),
        }],
        // Nothing has been observed through the plugin path (qa-environments
        // Task 14): every observation value is the one a never-observed
        // environment holds.
        observed_attrs: qa_environments_sdk::ObservedAttrs::default(),
        config: serde_json::json!({}),
        observed_base_url: None,
        health_state: qa_environments_sdk::HealthState::Unknown,
        health_detail: None,
        health_checked_at: None,
        created_at: now,
        updated_at: now,
    }
}

/// A platform carrying a `default_branch` override — rule 1's middle tier, live
/// since qa-runs Task 13b landed the field in `qa_environments_sdk`.
///
/// A separate constructor rather than a fourth parameter on `platform_fixture`,
/// because the override is orthogonal to the two `observed_*` values and every
/// existing caller wants it absent. `version`/`build` are pinned to the values
/// `platform_fixture(None, None)` would give, so the only difference between the
/// two fixtures is the field under test.
pub(super) fn platform_fixture_with_branch(default_branch: Option<&str>) -> Environment {
    Environment {
        default_branch: default_branch.map(str::to_owned),
        is_default: false,
        ..platform_fixture(None, None)
    }
}

#[async_trait]
impl QaEnvironmentsClientV1 for MockEnvironments {
    async fn get_environment(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Environment, QaEnvironmentsError> {
        self.lookups.lock().unwrap().push(id);
        self.platforms
            .iter()
            .find(|(tenant_id, platform)| {
                platform.id == id && *tenant_id == ctx.subject_tenant_id()
            })
            .map(|(_, platform)| platform.clone())
            .ok_or_else(|| {
                // Not-found and forbidden are indistinguishable here on
                // purpose: that is what the real gear answers, and telling them
                // apart is the cross-tenant existence oracle.
                QaEnvironmentsError::internal(format!("platform {id} not found")).create()
            })
    }

    async fn list_environments(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<Environment>, QaEnvironmentsError> {
        Err(environments_unsupported("list_environments"))
    }

    async fn create_environment(
        &self,
        _ctx: &SecurityContext,
        _new: NewEnvironment,
    ) -> Result<Environment, QaEnvironmentsError> {
        Err(environments_unsupported("create_environment"))
    }

    async fn update_environment(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _patch: EnvironmentPatch,
    ) -> Result<Environment, QaEnvironmentsError> {
        Err(environments_unsupported("update_environment"))
    }

    async fn delete_environment(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), QaEnvironmentsError> {
        Err(environments_unsupported("delete_environment"))
    }

    async fn list_variables(
        &self,
        _ctx: &SecurityContext,
        _platform_id: Option<Uuid>,
    ) -> Result<Vec<Variable>, QaEnvironmentsError> {
        Err(environments_unsupported("list_variables"))
    }

    async fn upsert_variable(
        &self,
        _ctx: &SecurityContext,
        _var: NewVariable,
    ) -> Result<Variable, QaEnvironmentsError> {
        Err(environments_unsupported("upsert_variable"))
    }

    async fn delete_variable(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), QaEnvironmentsError> {
        Err(environments_unsupported("delete_variable"))
    }

    async fn acquire_lease(
        &self,
        _ctx: &SecurityContext,
        _platform_id: Uuid,
        _run_id: Uuid,
        _mode: LeaseMode,
    ) -> Result<AcquireOutcome, QaEnvironmentsError> {
        Err(environments_unsupported("acquire_lease"))
    }

    async fn release_lease(
        &self,
        _ctx: &SecurityContext,
        _platform_id: Uuid,
        _run_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError> {
        Err(environments_unsupported("release_lease"))
    }

    async fn get_lease(
        &self,
        _ctx: &SecurityContext,
        _platform_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError> {
        Err(environments_unsupported("get_lease"))
    }
}

// ---------------------------------------------------------------------------
// Seam doubles
// ---------------------------------------------------------------------------

/// Records the [`Run`] it was asked to admit and answers a scripted outcome.
pub(super) struct RecordingAdmitter {
    outcome: Mutex<Option<Result<Admission, DomainError>>>,
    seen: Mutex<Vec<Run>>,
}

impl RecordingAdmitter {
    pub(super) fn answering(outcome: Admission) -> Self {
        Self {
            outcome: Mutex::new(Some(Ok(outcome))),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn refusing(error: DomainError) -> Self {
        Self {
            outcome: Mutex::new(Some(Err(error))),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The run this admitter was asked about, which is how a test reads what the
    /// launch actually recorded.
    pub(super) fn admitted(&self) -> Option<Run> {
        self.seen.lock().unwrap().first().cloned()
    }
}

#[async_trait]
impl Admitter for RecordingAdmitter {
    async fn admit(
        &self,
        _ctx: &SecurityContext,
        run: &Run,
    ) -> Result<super::launch::Admitted, DomainError> {
        self.seen.lock().unwrap().push(run.clone());
        match self.outcome.lock().unwrap().take() {
            // No cap of its own: this double scripts the decision, and the real
            // gate is `admission::GlobalCapGate`'s.
            Some(Ok(admission)) => Ok(super::launch::Admitted {
                admission,
                slot: super::admission::CapSlot::unlimited(),
            }),
            Some(Err(error)) => Err(error),
            None => Err(DomainError::Internal(
                "RecordingAdmitter was called twice".to_owned(),
            )),
        }
    }

    /// The bypass seam. **Deliberately does not touch `seen`**, so
    /// `admitted()` answers `None` exactly when admission was skipped — which
    /// is what `a_collect_launch_bypasses_admission_and_is_never_exclusive`
    /// asserts on. Nor does it consume `outcome`, so a test can script an
    /// admission and then prove the bypass never reached for it.
    async fn bypass(&self, _ctx: &SecurityContext) -> Result<super::launch::Admitted, DomainError> {
        Ok(super::launch::Admitted {
            admission: Admission::Unqueued,
            slot: super::admission::CapSlot::unlimited(),
        })
    }
}

/// Records `(run_id, queue_id)` and optionally moves the stored run on, so the
/// launch's post-dispatch re-read has something to observe.
pub(super) struct RecordingDispatcher {
    calls: Mutex<Vec<(Uuid, Option<Uuid>)>>,
    runs: Option<Arc<MockRunsRepository>>,
    fail_with: Option<String>,
}

impl RecordingDispatcher {
    /// Marks the stored run `running` with an execution reference, the way
    /// Task 14's `dispatch_one` will.
    pub(super) fn starting(runs: Arc<MockRunsRepository>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            runs: Some(runs),
            fail_with: None,
        }
    }

    pub(super) fn failing(reason: &str) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            runs: None,
            fail_with: Some(reason.to_owned()),
        }
    }

    pub(super) fn calls(&self) -> Vec<(Uuid, Option<Uuid>)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl InlineDispatcher for RecordingDispatcher {
    async fn dispatch_inline(
        &self,
        _ctx: &SecurityContext,
        run_id: Uuid,
        queue_id: Option<Uuid>,
        _capacity: &super::admission::CapSlot,
    ) -> Result<(), DomainError> {
        self.calls.lock().unwrap().push((run_id, queue_id));
        if let Some(reason) = &self.fail_with {
            return Err(DomainError::ExecutorFailed(reason.clone()));
        }
        if let Some(runs) = &self.runs {
            let mut rows = runs.rows.lock().unwrap();
            if let Some((_, run)) = rows.iter_mut().find(|(_, run)| run.id == run_id) {
                run.state = RunState::Running;
                run.execution_ref = Some("mock-execution-1".to_owned());
                run.started_at = Some(OffsetDateTime::now_utc());
            }
        }
        Ok(())
    }
}

/// Records every [`WatchTarget`] the re-attachment pass handed it, and models
/// the registry's idempotency **as the production one has it** — a second
/// `attach` for a run already recorded is dropped, and `is_watching` answers
/// from the same set.
///
/// Modelling the idempotency rather than recording every call is deliberate.
/// The pass asks `is_watching` *before* it spends a policy decision and a scoped
/// read, so a double that always answered `false` would make every test look
/// like a cold start and would hide the one behaviour that keeps a per-tick scan
/// over every live run affordable.
pub(super) struct RecordingWatcher {
    attached: Mutex<Vec<WatchTarget>>,
}

impl RecordingWatcher {
    pub(super) fn new() -> Self {
        Self {
            attached: Mutex::new(Vec::new()),
        }
    }

    /// Every target this watcher accepted, in the order it accepted them.
    pub(super) fn attached(&self) -> Vec<WatchTarget> {
        self.attached.lock().unwrap().clone()
    }

    /// Pretend an observer ended, so a test can drive the re-attachment a
    /// released slot allows.
    pub(super) fn detach(&self, run_id: Uuid) {
        self.attached
            .lock()
            .unwrap()
            .retain(|target| target.run_id != run_id);
    }
}

impl RunWatcher for RecordingWatcher {
    fn attach(&self, target: WatchTarget) {
        let mut attached = self.attached.lock().unwrap();
        if attached
            .iter()
            .any(|existing| existing.run_id == target.run_id)
        {
            return;
        }
        attached.push(target);
    }

    fn is_watching(&self, run_id: Uuid) -> bool {
        self.attached
            .lock()
            .unwrap()
            .iter()
            .any(|target| target.run_id == run_id)
    }
}

/// A [`LogArchive`] that accepts every line and archives nothing.
///
/// Production's equivalent no-op, `NoopLogArchive`, was deleted from
/// `domain::service` once `gear.rs` threaded a real `infra::logs::RunLogArchive`
/// through — see [`ServiceDeps::archive`]'s doc. This is that construction's
/// test-only stand-in: `archive` is a required field, but most suites here
/// build `AppServices` over `FakeRuns`, which does not implement
/// `RunLogsRepository` (`MockRunsRepository` does, at its own `impl` below,
/// but is not what most of these suites build `AppServices` over), so they
/// cannot build a real `RunLogArchive` and do not need to — none of them
/// exercise archiving. Tests that do (`ingest_tests`, `ingest_races_pg_tests`,
/// and `api::rest::handlers::runs`'s `handler_tests` via `Fleet::instance`,
/// which builds over the real `OrmRunsRepository`) supply their own
/// `RecordingArchive` double, write `qa_run_logs` directly, or read back a
/// row a real `RunLogArchive` wrote.
#[derive(Default)]
pub(super) struct NullLogArchive;

#[async_trait]
impl LogArchive for NullLogArchive {
    fn record(&self, _tenant_id: Uuid, _run_id: Uuid, _line: &str) {}

    async fn flush(&self, _run_id: Uuid) -> Result<(), DomainError> {
        Ok(())
    }

    async fn flush_due(&self) -> FlushReport {
        FlushReport::default()
    }

    /// Nothing is ever archived under this double, so nothing is ever
    /// resumable either — the empty map is exactly right, not a placeholder:
    /// it is what makes a caller read from the beginning, which is correct
    /// here because "the beginning" is the whole of what this double has.
    async fn resume_positions(
        &self,
        _tenant: crate::domain::system_actor::TenantBound,
        _run_id: Uuid,
    ) -> Result<LogResume, DomainError> {
        Ok(LogResume::default())
    }
}

// ===========================================================================
// The DB-backed fleet harness
// ===========================================================================
//
// Promoted here from `schedules_tests` by Task 20, because it now serves two
// modules: the schedule service suite it was written for, and
// `api::rest::handlers::schedules`, which drives real handler functions over a
// real `ConcreteAppServices`.
//
// **That second consumer is the point, and its absence was a false claim.**
// Task 20 shipped two doc comments asserting that a handler could not be tested
// because the crate had no harness able to build a `ConcreteAppServices` — while
// `Fleet::instance` had been returning exactly that since Task 19, three files
// away. Nobody grepped. The module header above already anticipated becoming the
// crate-wide harness; this is that, and the visibility is `pub(crate)` so the
// next consumer does not have to move anything.

// ---------------------------------------------------------------------------
// Doubles
// ---------------------------------------------------------------------------

/// One question this service asked the policy decision point.
///
/// The `resource_id` is `Option`, matching `access_scope`'s own argument, so a
/// call that should scope to a row and does not is visible as `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::domain::service) struct Asked {
    pub(in crate::domain::service) resource_type: String,
    pub(in crate::domain::service) action: String,
    pub(in crate::domain::service) resource_id: Option<Uuid>,
}

impl Asked {
    pub(in crate::domain::service) fn new(
        resource_type: &str,
        action: &str,
        resource_id: Option<Uuid>,
    ) -> Self {
        Self {
            resource_type: resource_type.to_owned(),
            action: action.to_owned(),
            resource_id,
        }
    }
}

/// Grants everything, narrowing a tenant-bound subject to its own tenant and
/// answering a covering set for a nil-tenant one — and **records what it was
/// asked**.
///
/// The covering arm is what `domain::system_actor`'s header names as the
/// precondition for the enumeration factories to work at all; without it the
/// firing tick is `Forbidden` at its first step. The set is **listed**, never
/// unconstrained, so a write that leaked the enumeration context would reach
/// both fixture tenants' rows and be visible.
///
/// # Record, do not constrain
///
/// Without [`Self::asked`] this double discriminated neither resource type nor
/// action, so **nothing pinned any scope this service derives**. Measured, on
/// the version that only granted: pointing `write_tick_outcome`'s scope at
/// `actions::LIST` with no id, and separately swapping `&resources::SCHEDULE`
/// for `&resources::RUN` across *every* method of the service, each left all
/// tests passing. The whole schedule service could ask the PDP about `qa.run`
/// and nothing noticed. `admission_tests`' `resource_marker` records the same
/// defect surviving a green suite, which is why that harness stamps a marker
/// onto `RESOURCE_ID`.
///
/// **That marker is not reusable here** and its own doc says why: it compiles
/// into a `resource_id IN [marker]` predicate, so handing one of those scopes to
/// a real `Orm*Repository` filters out every row — and this suite is built on
/// the real repositories. Recording is the version of the same guard that
/// injects nothing into the scope, and
/// `the_tick_asks_the_pdp_about_the_schedule_it_is_firing` is what reads it.
pub(in crate::domain::service) struct SchedulerAuthZ {
    covering: Vec<Uuid>,
    asked: Mutex<Vec<Asked>>,
    /// When true, every decision is a refusal.
    ///
    /// The shape a deployment whose policy engine has not been taught
    /// `qa.schedule` yet actually has, which is the most likely state of a fresh
    /// install of these endpoints - and therefore the error an operator is most
    /// likely to see first.
    deny: bool,
}

impl SchedulerAuthZ {
    /// The ordinary fleet: two real tenants.
    pub(in crate::domain::service) fn fleet() -> Self {
        Self::covering(vec![OWNER_TENANT, OTHER_TENANT])
    }

    /// A covering set that also admits [`Uuid::nil`].
    ///
    /// **Only for the corrupt-row test.** A nil `tenant_id` on a schedule row is
    /// not a tenant, so a scope that did not admit it would filter the row out
    /// of the enumeration and the guard under test would never be reached — the
    /// test would pass without exercising anything.
    pub(in crate::domain::service) fn admitting_nil() -> Self {
        Self::covering(vec![OWNER_TENANT, OTHER_TENANT, Uuid::nil()])
    }

    pub(in crate::domain::service) fn covering(covering: Vec<Uuid>) -> Self {
        Self {
            covering,
            asked: Mutex::new(Vec::new()),
            deny: false,
        }
    }

    /// Refuses every decision, as a policy engine that does not know
    /// `qa.schedule` does.
    pub(in crate::domain::service) fn denying() -> Self {
        Self {
            deny: true,
            ..Self::fleet()
        }
    }

    /// Every decision asked for, in order, filtered to one resource type.
    pub(in crate::domain::service) fn asked_about(&self, resource_type: &str) -> Vec<Asked> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|asked| asked.resource_type == resource_type)
            .cloned()
            .collect()
    }

    /// Drop what a test's fixture setup asked for, so an assertion is about the
    /// tick alone.
    pub(in crate::domain::service) fn forget_setup(&self) {
        self.asked.lock().unwrap().clear();
    }
}

fn subject_tenant(request: &EvaluationRequest) -> Option<Uuid> {
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

#[async_trait]
impl AuthZResolverClient for SchedulerAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        self.asked.lock().unwrap().push(Asked::new(
            &request.resource.resource_type,
            &request.action.name,
            request.resource.id,
        ));
        if self.deny {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            });
        }
        let tenants = match subject_tenant(&request) {
            Some(id) => vec![id],
            None => self.covering.clone(),
        };
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        tenants,
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

/// The admission seam, recording every run handed to it.
///
/// **This is the instrument the exactly-once assertions read.** `Admitter` is
/// on the one path from a launch request to a run row, and nothing but
/// `LaunchService::launch` calls it — so "a run reached the admitter" is
/// literally "the schedule launched through the same path a manual launch
/// takes", which is `cpt-cf-qa-fr-runs-schedules`' requirement.
///
/// Unlike `test_support::RecordingAdmitter` it answers **every** call rather
/// than one: the point of the multi-instance tests is that a second call does
/// *not* arrive, and a double that failed the second call could not tell "no
/// second launch" from "a second launch the double refused".
pub(in crate::domain::service) struct QueueingAdmitter {
    seen: Mutex<Vec<Run>>,
    /// When set, every admission fails with an
    /// [`DomainError::ExecutorFailed`] carrying this text.
    ///
    /// The text rather than the error, because `DomainError` is deliberately not
    /// `Clone` and this double answers every call.
    refuse_with: Option<String>,
}

impl QueueingAdmitter {
    pub(in crate::domain::service) fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            refuse_with: None,
        }
    }

    /// Refuses with a **non-disclosable** error, so the tick's use of
    /// `DomainError::recorded_text` is observable on the tick row.
    pub(in crate::domain::service) fn refusing() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            refuse_with: Some("kubeconfig secret vhp-staging-kubeconfig is missing".to_owned()),
        }
    }

    pub(in crate::domain::service) fn admitted(&self) -> Vec<Run> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl Admitter for QueueingAdmitter {
    async fn admit(&self, _ctx: &SecurityContext, run: &Run) -> Result<Admitted, DomainError> {
        self.seen.lock().unwrap().push(run.clone());
        if let Some(reason) = &self.refuse_with {
            return Err(DomainError::ExecutorFailed(reason.clone()));
        }
        Ok(Admitted {
            // `Queued`, so the launch settles without reaching the executor:
            // what these tests are about is which runs get *created*, not how
            // they are submitted.
            admission: Admission::Queued {
                queue_id: Uuid::new_v4(),
            },
            slot: CapSlot::unlimited(),
        })
    }

    /// Not recorded in `seen`, for the reason [`RecordingAdmitter::bypass`]
    /// gives: `admitted()` must stay the answer to "did this launch go through
    /// admission?".
    async fn bypass(&self, _ctx: &SecurityContext) -> Result<Admitted, DomainError> {
        Ok(Admitted {
            admission: Admission::Unqueued,
            slot: CapSlot::unlimited(),
        })
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One database and one set of collaborators, over which any number of
/// **instances** can be built.
///
/// That split is the whole point: a replica is an `AppServices` of its own — its
/// own `ScheduleService`, its own `claimed_by`, its own `LaunchService` — over a
/// shared store. [`Fleet::instance`] is what a second replica is.
pub struct Fleet {
    pub(in crate::domain::service) db: Arc<DbProvider>,
    pub(in crate::domain::service) catalog: Arc<FakeCatalog>,
    pub(in crate::domain::service) environments: Arc<FakeEnvironments>,
    pub(in crate::domain::service) admitter: Arc<QueueingAdmitter>,
    pub(in crate::domain::service) authz: Arc<SchedulerAuthZ>,
}

impl Fleet {
    pub async fn new() -> Self {
        Self::with(QueueingAdmitter::new(), SchedulerAuthZ::fleet()).await
    }

    pub(in crate::domain::service) async fn with(
        admitter: QueueingAdmitter,
        authz: SchedulerAuthZ,
    ) -> Self {
        Self {
            db: Arc::new(DBProvider::<DomainError>::new(inmem_db().await)),
            catalog: Arc::new(FakeCatalog::serving(&["tests/a.py"])),
            environments: Arc::new(FakeEnvironments::free()),
            admitter: Arc::new(admitter),
            authz: Arc::new(authz),
        }
    }

    /// Make every in-scope test file declare `exclusive: true` in its
    /// `TEST_META`, so the launch tier has something to override.
    pub(in crate::domain::service) fn with_exclusive_test_file(self) -> Self {
        *self.catalog.metas.lock().unwrap() = vec![TestFileMeta {
            path: "tests/a.py".to_owned(),
            title: None,
            tags: Vec::new(),
            exclusive: Some(true),
            bugs: Vec::new(),
        }];
        self
    }

    /// A replica.
    /// Move a schedule's fired-through cursor, as a successful fire would.
    ///
    /// Exposed so a test outside `domain::service` can set up the one state that
    /// makes "an edit cannot rewind the cursor" a *behavioural* claim rather
    /// than a structural one: a schedule that has actually fired.
    pub async fn advance_cursor(&self, tenant: Uuid, id: Uuid, to: OffsetDateTime) {
        let conn = self.db.conn().unwrap();
        assert!(
            OrmSchedulesRepository
                .advance_last_fired_tick(&conn, &scope(tenant), id, to)
                .await
                .unwrap(),
            "the fixture's cursor must actually move, or the test that reads it is vacuous"
        );
    }

    /// One run row under `tenant`, written directly at the repository.
    ///
    /// Exposed for the handler suites in `api::rest`, which need a run that
    /// *exists and is readable* before they can exercise anything downstream of
    /// the read - the SSE log stream in particular, whose whole ordering
    /// property is "the scoped read happens first". Going through
    /// `LaunchService` instead would make those tests depend on the catalog and
    /// environments doubles resolving a plan, which is a different subject.
    ///
    /// [`crate::infra::storage::test_db::sample_new_run`] leaves the row in
    /// [`RunState::Created`], which is **not** terminal - the state the log
    /// handler's subscribe path requires.
    pub async fn seed_run(&self, tenant: Uuid, name: &str) -> Run {
        let conn = self.db.conn().unwrap();
        OrmRunsRepository
            .create(
                &conn,
                &scope(tenant),
                tenant,
                crate::infra::storage::test_db::sample_new_run(name),
            )
            .await
            .expect("the fixture run must be insertable under its own tenant")
    }

    /// Write directly to `qa_run_logs` under `tenant`, bypassing
    /// `RunLogArchive` entirely.
    ///
    /// Task 6's handler suites need a row already **committed**, not a
    /// pending buffer waiting on a flush — `RunLogArchive` batches and this
    /// fixture must not depend on when a flush happens to run. Mirrors
    /// [`Self::seed_run`]: a fixture writing directly at the repository,
    /// under the run's own tenant scope, so tenancy is still real rather than
    /// bypassed.
    ///
    /// `text` is stored exactly as given - callers own their own trailing
    /// `\n`, matching `RunLogArchive::record`'s convention of one `\n` per
    /// logical line.
    pub async fn write_archived_log(&self, tenant: Uuid, run_id: Uuid, text: &str) {
        let conn = self.db.conn().unwrap();
        let lines = i64::try_from(text.lines().count()).unwrap_or(i64::MAX);
        OrmRunsRepository
            .append_log(&conn, &scope(tenant), run_id, tenant, text, lines)
            .await
            .expect("the fixture's archived log must be insertable");
    }

    /// The stored cursor, read directly.
    pub async fn cursor_of(&self, tenant: Uuid, id: Uuid) -> Option<OffsetDateTime> {
        let conn = self.db.conn().unwrap();
        OrmSchedulesRepository
            .get(&conn, &scope(tenant), id)
            .await
            .unwrap()
            .expect("the schedule must exist")
            .last_fired_tick
    }

    /// A fleet whose policy decision point refuses everything.
    pub async fn denying() -> Self {
        Self::with(QueueingAdmitter::new(), SchedulerAuthZ::denying()).await
    }

    pub fn instance(&self) -> Arc<ConcreteAppServices> {
        Arc::new(AppServices::new(
            Arc::new(OrmRunsRepository),
            Arc::new(OrmQueueRepository),
            Arc::new(OrmSchedulesRepository),
            ServiceDeps {
                db: Arc::clone(&self.db),
                authz: Arc::clone(&self.authz) as Arc<dyn AuthZResolverClient>,
                catalog: Arc::clone(&self.catalog) as Arc<dyn qa_catalog_sdk::QaCatalogClientV1>,
                environments: Arc::clone(&self.environments)
                    as Arc<dyn qa_environments_sdk::QaEnvironmentsClientV1>,
                product_plugins: Arc::new(
                    crate::domain::service::admission::tests::fakes::FakeProductPlugins::default(),
                ),
                executor: Arc::new(MockRunExecutor::new())
                    as Arc<dyn crate::domain::ports::run_executor::RunExecutor>,
                logs: Arc::new(RunLogBroadcaster::new(8)),
                archive: Arc::new(NullLogArchive) as Arc<dyn LogArchive>,
                // The one override: the admission seam is the instrument these
                // tests read. Everything else is the production wiring.
                admitter: Some(Arc::clone(&self.admitter) as Arc<dyn Admitter>),
                dispatcher: None,
                watcher: None,
                default_timeout_seconds: 3600,
                limits: QueueLimits {
                    queue_max_depth: 20,
                    max_concurrent_runs: 0,
                    queue_ttl_seconds: 7200,
                },
                orphan_timeout_seconds: 600,
            },
        ))
    }

    /// Every run stored under `tenant`, read **directly** rather than through
    /// the service: the service's own read is scoped too, so it could not tell
    /// "nothing was written" from "you are not allowed to look".
    pub(in crate::domain::service) async fn runs_of(&self, tenant: Uuid) -> Vec<Run> {
        let conn = self.db.conn().unwrap();
        OrmRunsRepository.list(&conn, &scope(tenant)).await.unwrap()
    }

    /// Every tick row under `tenant`.
    ///
    /// `qa_schedule_ticks` has **no read path on `SchedulesRepository`** — that
    /// absence is deliberate and recorded there as a tracked deferral — so this
    /// goes at the entity through the same `SecureORM` scoping a repository
    /// method would use. It exists only as ground truth for the two assertions
    /// that are about what a claim wrote.
    pub(in crate::domain::service) async fn ticks_of(
        &self,
        tenant: Uuid,
    ) -> Vec<schedule_tick::Model> {
        let conn = self.db.conn().unwrap();
        schedule_tick::Entity::find()
            .secure()
            .scope_with(&scope(tenant))
            .all(&conn)
            .await
            .unwrap()
    }
}
