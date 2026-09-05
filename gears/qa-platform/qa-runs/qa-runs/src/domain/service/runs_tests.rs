//! Unit tests for reads, cancel, re-run, and the queue operator actions.
//!
//! The doubles are `admission_tests::fakes` — one runs repository, one queue
//! repository, one qa-environments and one qa-catalog for the whole service
//! layer, for the reason that module's header gives.
//!
//! **Re-run is driven through the real [`LaunchService`]**, not through a
//! scripted launcher. That is the point of the feature: a re-run is
//! indistinguishable downstream from a fresh launch, and a double in that
//! position would let every claim this module makes about re-validation, the
//! platform re-read and the timeout re-resolution be true of the double instead
//! of the code. The seam that *is* scripted is the admitter, so a re-run does not
//! need a lease and a queue to reach its assertion.
//!
//! Every ported expectation is read from the source system or from the frozen
//! guide, cited at the test.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use qa_catalog_sdk::TestFileMeta;
use qa_environments_sdk::{LeaseMode, LeaseState, QaEnvironmentsClientV1};
use qa_runs_sdk::{
    ExclusiveTier, QueueState, RunKind, RunParameter, RunSource, RunState, RunTarget,
};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_odata::ODataQuery;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::*;
use crate::domain::repos::QueueRowRecord;
use crate::domain::service::LogFanout;
use crate::domain::service::admission::tests::fakes::{
    FakeCatalog, FakeEnvironments, FakeQueue, FakeRuns, PLATFORM_A, REPO, SystemGrantingAuthZ,
    queued_row, row_aged, run_fixture,
};
use crate::domain::service::admission::{AdmissionDeps, AdmissionService};
use crate::domain::service::ingest::tests::RecordingLogs;
use crate::domain::service::launch::Admission;
use crate::domain::service::test_support::{
    NullLogArchive, OTHER_TENANT, OWNER_TENANT, RecordingAdmitter, ctx, test_db_provider,
};
use crate::domain::service::{LogArchive, QueueLimits, launch};
use crate::infra::executor::mock::MockRunExecutor;

const RUN: Uuid = Uuid::from_u128(0x0601);
const QUEUE: Uuid = Uuid::from_u128(0x0E61);

// ---------------------------------------------------------------------------
// Doubles and wiring
// ---------------------------------------------------------------------------

/// Records `(run_id, queue_id)` and answers a scripted result.
///
/// `test_support::RecordingDispatcher` is Task 13's and is bound to
/// `MockRunsRepository`; force start drives the concurrency core's `FakeRuns`,
/// so this is the same shape over the other fixture.
#[derive(Default)]
struct RecordingDispatcher {
    calls: Mutex<Vec<(Uuid, Option<Uuid>)>>,
    fail_with: Option<String>,
}

impl RecordingDispatcher {
    fn failing(reason: &str) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_with: Some(reason.to_owned()),
        }
    }

    fn calls(&self) -> Vec<(Uuid, Option<Uuid>)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl launch::InlineDispatcher for RecordingDispatcher {
    async fn dispatch_inline(
        &self,
        _ctx: &SecurityContext,
        run_id: Uuid,
        queue_id: Option<Uuid>,
        _capacity: &crate::domain::service::admission::CapSlot,
    ) -> Result<(), DomainError> {
        self.calls.lock().unwrap().push((run_id, queue_id));
        match &self.fail_with {
            Some(reason) => Err(DomainError::ExecutorFailed(reason.clone())),
            None => Ok(()),
        }
    }
}

/// The executor, plus a count of `cancel` calls and an injectable failure.
///
/// `MockRunExecutor` records nothing about `cancel`, and "a queued run's cancel
/// never reaches the execution plane" is a property only a count can express.
struct CountingExecutor {
    inner: MockRunExecutor,
    cancels: Mutex<Vec<String>>,
    fail_cancel: bool,
}

impl CountingExecutor {
    fn new() -> Self {
        Self {
            inner: MockRunExecutor::new(),
            cancels: Mutex::new(Vec::new()),
            fail_cancel: false,
        }
    }

    fn failing_cancel() -> Self {
        Self {
            fail_cancel: true,
            ..Self::new()
        }
    }

    fn cancels(&self) -> Vec<String> {
        self.cancels.lock().unwrap().clone()
    }
}

#[async_trait]
impl RunExecutor for CountingExecutor {
    async fn start(
        &self,
        spec: crate::domain::ports::run_executor::RunSpec,
    ) -> Result<ExecutionRef, DomainError> {
        self.inner.start(spec).await
    }

    async fn watch(
        &self,
        execution_ref: &ExecutionRef,
    ) -> Result<crate::domain::ports::run_executor::ExecutionStream, DomainError> {
        self.inner.watch(execution_ref).await
    }

    async fn cancel(&self, execution_ref: &ExecutionRef) -> Result<(), DomainError> {
        self.cancels
            .lock()
            .unwrap()
            .push(execution_ref.as_str().to_owned());
        if self.fail_cancel {
            return Err(DomainError::ExecutorFailed(
                "the execution plane is unreachable".to_owned(),
            ));
        }
        Ok(())
    }

    async fn list_active(&self) -> Result<std::collections::BTreeSet<ExecutionRef>, DomainError> {
        self.inner.list_active().await
    }
}

struct Harness {
    runs: Arc<FakeRuns>,
    queue: Arc<FakeQueue>,
    environments: Arc<FakeEnvironments>,
    executor: Arc<CountingExecutor>,
    dispatcher: Arc<RecordingDispatcher>,
    admitter: Arc<RecordingAdmitter>,
    logs: Arc<RecordingLogs>,
    service: RunsService<FakeRuns, FakeQueue>,
}

struct Builder {
    runs: Arc<FakeRuns>,
    queue: Arc<FakeQueue>,
    environments: Arc<FakeEnvironments>,
    catalog: Arc<FakeCatalog>,
    executor: Arc<CountingExecutor>,
    dispatcher: Arc<RecordingDispatcher>,
    admitter: Arc<RecordingAdmitter>,
    logs: Arc<RecordingLogs>,
    limits: QueueLimits,
}

impl Builder {
    fn new() -> Self {
        Self {
            runs: Arc::new(FakeRuns::default()),
            queue: Arc::new(FakeQueue::default()),
            environments: Arc::new(FakeEnvironments::free()),
            catalog: Arc::new(FakeCatalog::serving(&["tests/a.py"])),
            executor: Arc::new(CountingExecutor::new()),
            dispatcher: Arc::new(RecordingDispatcher::default()),
            admitter: Arc::new(RecordingAdmitter::answering(Admission::Unqueued)),
            logs: Arc::new(RecordingLogs::default()),
            limits: QueueLimits {
                queue_max_depth: 20,
                max_concurrent_runs: 0,
                queue_ttl_seconds: 7200,
            },
        }
    }

    fn runs(mut self, runs: Arc<FakeRuns>) -> Self {
        self.runs = runs;
        self
    }

    fn queue(mut self, queue: Arc<FakeQueue>) -> Self {
        self.queue = queue;
        self
    }

    /// Override `queue_ttl_seconds` alone; the other two limits are
    /// admission's and no test here varies them.
    fn ttl_seconds(mut self, seconds: u64) -> Self {
        self.limits.queue_ttl_seconds = seconds;
        self
    }

    fn environments(mut self, environments: Arc<FakeEnvironments>) -> Self {
        self.environments = environments;
        self
    }

    fn catalog(mut self, catalog: Arc<FakeCatalog>) -> Self {
        self.catalog = catalog;
        self
    }

    fn executor(mut self, executor: Arc<CountingExecutor>) -> Self {
        self.executor = executor;
        self
    }

    fn dispatcher(mut self, dispatcher: Arc<RecordingDispatcher>) -> Self {
        self.dispatcher = dispatcher;
        self
    }

    fn max_concurrent_runs(mut self, max: u32) -> Self {
        self.limits.max_concurrent_runs = max;
        self
    }

    async fn build(self) -> Harness {
        let db = test_db_provider().await;
        let enforcer = authz_resolver_sdk::PolicyEnforcer::new(Arc::new(SystemGrantingAuthZ));
        let locks = PlatformLocks::default();

        let admission = Arc::new(AdmissionService::new(AdmissionDeps {
            db: Arc::clone(&db),
            runs: Arc::clone(&self.runs),
            queue: Arc::clone(&self.queue),
            environments: Arc::clone(&self.environments) as Arc<dyn QaEnvironmentsClientV1>,
            executor: Arc::clone(&self.executor) as Arc<dyn RunExecutor>,
            locks: locks.clone(),
            limits: self.limits,
            policy_enforcer: enforcer.clone(),
        }));
        let launch = Arc::new(LaunchService::new(
            Arc::clone(&db),
            Arc::clone(&self.runs),
            Arc::clone(&self.catalog) as Arc<dyn qa_catalog_sdk::QaCatalogClientV1>,
            Arc::clone(&self.environments) as Arc<dyn QaEnvironmentsClientV1>,
            Arc::clone(&self.logs) as Arc<dyn LogFanout>,
            Arc::clone(&self.admitter) as Arc<dyn launch::Admitter>,
            Arc::clone(&self.dispatcher) as Arc<dyn InlineDispatcher>,
            900,
            enforcer.clone(),
        ));
        let service = RunsService::new(RunsDeps {
            db,
            runs: Arc::clone(&self.runs),
            queue: Arc::clone(&self.queue),
            environments: Arc::clone(&self.environments) as Arc<dyn QaEnvironmentsClientV1>,
            executor: Arc::clone(&self.executor) as Arc<dyn RunExecutor>,
            logs: Arc::clone(&self.logs) as Arc<dyn LogFanout>,
            launch,
            dispatcher: Arc::clone(&self.dispatcher) as Arc<dyn InlineDispatcher>,
            admission,
            queue_ttl_seconds: self.limits.queue_ttl_seconds,
            locks,
            policy_enforcer: enforcer,
        });
        Harness {
            runs: self.runs,
            queue: self.queue,
            environments: self.environments,
            executor: self.executor,
            dispatcher: self.dispatcher,
            admitter: self.admitter,
            logs: self.logs,
            service,
        }
    }
}

impl Harness {
    /// Store a terminal run, owned by `tenant`, that finished at `finished_at`,
    /// and return its id.
    ///
    /// Beside the other run-construction helpers rather than inlined in the
    /// sweep tests, because those tests are about *which* runs a watermark
    /// selects and in what order - three lines of `Run` construction per
    /// fixture row would bury that under the thing it is not about.
    ///
    /// **Deviation from the plan's sketch, recorded rather than silently
    /// applied.** The plan spelled this `f.finished_run_at("2026-08-18T10:00:00Z")`,
    /// taking an RFC-3339 string. It takes an [`OffsetDateTime`] instead:
    /// callers write `datetime!(2026-08-18 10:00 UTC)`, which this crate
    /// already uses for time fixtures, and a parse that can fail inside a
    /// fixture helper is a second failure mode for no readability gain.
    fn finished_run_at(&self, tenant: Uuid, finished_at: OffsetDateTime) -> Uuid {
        let id = Uuid::new_v4();
        let mut run = run_fixture(id, Some(PLATFORM_A), false, RunState::Succeeded);
        run.finished_at = Some(finished_at);
        self.runs.insert_for_test(tenant, run);
        id
    }
}

fn owner() -> SecurityContext {
    ctx(OWNER_TENANT)
}

/// A stored run, in whatever shape a test needs, on `PLATFORM_A`.
fn stored(state: RunState, exclusive: bool) -> qa_runs_sdk::Run {
    run_fixture(RUN, Some(PLATFORM_A), exclusive, state)
}

fn queued_fixture() -> Arc<FakeQueue> {
    Arc::new(FakeQueue::with(vec![queued_row(
        QUEUE,
        OWNER_TENANT,
        RUN,
        PLATFORM_A,
        false,
        QueueState::Queued,
    )]))
}

fn row_in(state: QueueState) -> QueueRowRecord {
    queued_row(QUEUE, OWNER_TENANT, RUN, PLATFORM_A, false, state)
}

// ---------------------------------------------------------------------------
// Re-run
// ---------------------------------------------------------------------------

/// Guide lines 205-208: *"A run recorded as exclusive re-runs as exclusive."*
/// `Some(true)` on the launch tier replays the original decision, which is the
/// point of a re-run.
#[tokio::test]
async fn a_run_recorded_exclusive_reruns_exclusive() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Succeeded, true),
        )])))
        .build()
        .await;

    h.service.rerun(&owner(), RUN).await.expect("re-run");

    let admitted = h.admitter.admitted().expect("the launch reached admission");
    assert!(
        admitted.resolved_exclusive,
        "an exclusive run must re-run exclusive"
    );
    assert_eq!(
        admitted.exclusive_tier,
        ExclusiveTier::Launch,
        "`Some(true)` lands on the launch tier, which is what replaying means"
    );
}

/// The upward-only rule, and the case it exists for: guide lines 205-208 say a
/// run recorded parallel is **re-resolved** from the current tiers, *"so if its
/// test has been marked destructive since, the re-run correctly becomes
/// exclusive instead of replaying 'parallel'"*.
///
/// `Some(run.resolved_exclusive)` — the transcription that type-checks and is
/// wrong — would pin `false` on the launch tier, which outranks `TEST_META`, and
/// this test is what fails when it does.
#[tokio::test]
async fn a_run_recorded_parallel_is_re_resolved_not_pinned_parallel() {
    let catalog = Arc::new(FakeCatalog::serving(&["tests/a.py"]));
    catalog.metas.lock().unwrap().push(TestFileMeta {
        path: "tests/a.py".to_owned(),
        title: None,
        tags: Vec::new(),
        // Marked destructive since the original run.
        exclusive: Some(true),
        bugs: Vec::new(),
    });
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Succeeded, false),
        )])))
        .catalog(catalog)
        .build()
        .await;

    h.service.rerun(&owner(), RUN).await.expect("re-run");

    let admitted = h.admitter.admitted().expect("the launch reached admission");
    assert!(
        admitted.resolved_exclusive,
        "a stored `false` must fall through to the current tiers, not be pinned"
    );
    assert_eq!(admitted.exclusive_tier, ExclusiveTier::TestMeta);
}

/// `routes/runs.rs:927-933`: the re-run reuses the branch the original run
/// actually executed against, so a `default_branch` edited since cannot silently
/// move it.
#[tokio::test]
async fn rerun_reuses_the_original_branch() {
    let mut run = stored(RunState::Failed, false);
    run.test_version = Some("release-7.1".to_owned());
    let catalog = Arc::new(FakeCatalog::serving(&["tests/a.py"]));
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .catalog(Arc::clone(&catalog))
        .build()
        .await;

    h.service.rerun(&owner(), RUN).await.expect("re-run");

    assert_eq!(
        h.admitter.admitted().unwrap().test_version.as_deref(),
        Some("release-7.1"),
        "rule 6 records the branch label verbatim, and the re-run replays it"
    );
}

/// `routes/runs.rs:920-923`: *"cheap defense-in-depth against stale/tampered
/// rows and a reserved list that may have grown since."* Here it is not an extra
/// call — going through the one creation path *is* the re-validation, because
/// `launch` step 1 normalises and validates before any I/O.
#[tokio::test]
async fn rerun_re_validates_the_replayed_parameters() {
    let mut run = stored(RunState::Failed, false);
    run.parameters = vec![RunParameter {
        // Reserved, i.e. a name the list has grown to include since this run was
        // stored. `domain::params::RESERVED_NAMES`.
        name: "TEST_FILES".to_owned(),
        value: "tests/b.py".to_owned(),
    }];
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .build()
        .await;

    let error = h
        .service
        .rerun(&owner(), RUN)
        .await
        .expect_err("a replayed reserved parameter must be refused");

    assert!(
        matches!(error, DomainError::InvalidParameters(_)),
        "{error}"
    );
    assert!(
        h.admitter.admitted().is_none(),
        "validation runs before any I/O, so nothing reached admission"
    );
}

/// The analogue of `routes/runs.rs:910-918`'s `BadRequest` for a run missing its
/// plan metadata. `RunTarget` here is total, so the field that can actually be
/// missing is the recorded branch — and letting it fall through would re-resolve
/// from today's defaults, executing a different branch than the run being
/// repeated.
#[tokio::test]
async fn rerun_of_a_run_missing_its_recorded_branch_is_a_validation_error() {
    let mut run = stored(RunState::Failed, false);
    run.test_version = None;
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .build()
        .await;

    let error = h.service.rerun(&owner(), RUN).await.expect_err("refused");
    assert!(
        matches!(error, DomainError::Validation { ref field, .. } if field == "test_version"),
        "{error}"
    );
}

/// **One creation path.** A re-run is not a second way to make a run: the row it
/// produces goes through `create_run`'s name sequence and reaches the same
/// admitter — which is what keeps it indistinguishable downstream.
#[tokio::test]
async fn rerun_goes_through_the_same_launch_path() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Succeeded, false),
        )])))
        .build()
        .await;

    h.service.rerun(&owner(), RUN).await.expect("re-run");

    let created = h.runs.created.lock().unwrap().clone();
    assert_eq!(created.len(), 1, "exactly one new run row");
    assert_eq!(created[0].state, RunState::Created);
    assert!(h.admitter.admitted().is_some());
    assert_eq!(
        h.dispatcher.calls().len(),
        1,
        "and it dispatched through the same seam an inline admission uses"
    );
}

/// The source system hard-codes `run_source: Some("manual")` and
/// `schedule_id: None` on every re-run intent: a person re-running a scheduled
/// run is a manual launch, and attributing it to the schedule would corrupt that
/// schedule's history.
#[tokio::test]
async fn a_rerun_of_a_scheduled_run_is_recorded_manual_and_unscheduled() {
    let mut run = stored(RunState::Succeeded, false);
    run.source = RunSource::Scheduled;
    run.schedule_id = Some(Uuid::from_u128(0x5CED));
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .build()
        .await;

    h.service.rerun(&owner(), RUN).await.expect("re-run");

    let created = h.runs.created.lock().unwrap().clone();
    assert_eq!(created[0].source, RunSource::Manual);
    assert_eq!(created[0].schedule_id, None);
}

#[tokio::test]
async fn rerun_of_another_tenants_run_is_a_not_found() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Succeeded, false),
        )])))
        .build()
        .await;

    let error = h
        .service
        .rerun(&ctx(OTHER_TENANT), RUN)
        .await
        .expect_err("a foreign tenant must not re-run");
    assert!(matches!(error, DomainError::RunNotFound { id } if id == RUN));
    assert!(h.runs.created.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Cancel
// ---------------------------------------------------------------------------

/// A `queued` row holds nothing, so its cancel drops the row and never reaches
/// the execution plane — there is no execution.
#[tokio::test]
async fn cancelling_a_queued_run_drops_its_row_and_never_calls_the_executor() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Queued, false),
        )])))
        .queue(queued_fixture())
        .build()
        .await;

    h.service.cancel(&owner(), RUN).await.expect("cancel");

    assert_eq!(h.runs.state_of(RUN), Some(RunState::Canceled));
    assert_eq!(h.queue.state_of(QUEUE), Some(QueueState::Cancelled));
    assert!(
        h.executor.cancels().is_empty(),
        "a queued run has no execution to stop"
    );
}

/// A live run's cancel goes to the execution plane, and **nothing else is
/// released**.
///
/// This is a deliberate divergence from the plan's Step 6, which says a
/// dispatching/running cancel *"releases its lease"*. At the instant the cancel
/// is recorded the control plane has *asked* the executor to stop and has not
/// observed it stop — `RunExecutor::cancel` is documented fire-and-forget, and
/// the source system's stop handler writes no state at all
/// (`routes/runs.rs:1092-1106`). Freeing the platform then would let a new run
/// start beside an execution that is still winding down, which is the exact
/// hazard `cancel_queued`'s `state = 'queued'` guard exists to prevent one table
/// over. `service::ingest`'s `Finished` branch and the tick's claim
/// reconciliation each release it on evidence.
#[tokio::test]
async fn cancelling_a_running_run_stops_the_execution_and_releases_nothing_yet() {
    let mut run = stored(RunState::Running, true);
    run.execution_ref = Some("mock-execution-1".to_owned());
    let queue = Arc::new(FakeQueue::with(vec![row_in(QueueState::Running)]));
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .queue(queue)
        .environments(Arc::new(FakeEnvironments::holding(
            PLATFORM_A,
            LeaseState::HeldExclusive { holder: RUN },
        )))
        .build()
        .await;

    h.service.cancel(&owner(), RUN).await.expect("cancel");

    assert_eq!(h.executor.cancels(), vec!["mock-execution-1".to_owned()]);
    assert_eq!(h.runs.state_of(RUN), Some(RunState::Canceled));
    assert!(
        h.environments.released().is_empty(),
        "the lease is released when the execution is observed to end, not when it is \
         asked to stop"
    );
    assert_eq!(
        h.queue.state_of(QUEUE),
        Some(QueueState::Running),
        "and the claim is untouched: dropping it would free the platform under a live \
         execution"
    );
}

/// The fail-safe direction: an undeliverable cancel records nothing, so a run is
/// never reported stopped when it is not.
#[tokio::test]
async fn a_cancel_the_execution_plane_refuses_records_nothing() {
    let mut run = stored(RunState::Running, false);
    run.execution_ref = Some("mock-execution-1".to_owned());
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .executor(Arc::new(CountingExecutor::failing_cancel()))
        .build()
        .await;

    let error = h.service.cancel(&owner(), RUN).await.expect_err("refused");
    assert!(matches!(error, DomainError::ExecutorFailed(_)), "{error}");
    assert_eq!(h.runs.state_of(RUN), Some(RunState::Running));
}

/// `is_terminal` is what makes this idempotent rather than an error: answering
/// `409` to an operator clicking Cancel twice would report a fault for a correct
/// request.
#[tokio::test]
async fn cancelling_a_terminal_run_is_idempotent() {
    for state in [
        RunState::Succeeded,
        RunState::Failed,
        RunState::Canceled,
        RunState::TimedOut,
        RunState::Expired,
        RunState::Error,
    ] {
        let h = Builder::new()
            .runs(Arc::new(FakeRuns::with(vec![(
                OWNER_TENANT,
                stored(state, false),
            )])))
            .build()
            .await;

        let run = h
            .service
            .cancel(&owner(), RUN)
            .await
            .unwrap_or_else(|error| panic!("{state:?} must be idempotent, got {error}"));

        assert_eq!(run.state, state, "{state:?} is returned unchanged");
        assert_eq!(h.runs.state_of(RUN), Some(state));
        assert!(h.executor.cancels().is_empty(), "{state:?}");
    }
}

#[tokio::test]
async fn cancel_of_another_tenants_run_is_a_not_found() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Queued, false),
        )])))
        .queue(queued_fixture())
        .build()
        .await;

    let error = h
        .service
        .cancel(&ctx(OTHER_TENANT), RUN)
        .await
        .expect_err("a foreign tenant must not cancel");
    assert!(matches!(error, DomainError::RunNotFound { id } if id == RUN));
    assert_eq!(h.runs.state_of(RUN), Some(RunState::Queued));
    assert_eq!(h.queue.state_of(QUEUE), Some(QueueState::Queued));
}

// ---------------------------------------------------------------------------
// Queue operator actions
// ---------------------------------------------------------------------------

/// `run_queue.rs:403-421`: the `state = 'queued'` guard is *the safety
/// property*, because a `dispatching` row holds a claim an in-flight execution
/// depends on.
#[tokio::test]
async fn cancel_queued_refuses_a_dispatching_row_with_a_conflict() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Dispatching, false),
        )])))
        .queue(Arc::new(FakeQueue::with(vec![row_in(
            QueueState::Dispatching,
        )])))
        .build()
        .await;

    let error = h
        .service
        .cancel_queued(&owner(), QUEUE)
        .await
        .expect_err("a claimed row must not be dropped");

    assert!(
        matches!(
            error,
            DomainError::QueueRowNotQueued { id, state }
                if id == QUEUE && state == QueueState::Dispatching
        ),
        "{error}"
    );
    assert_eq!(h.queue.state_of(QUEUE), Some(QueueState::Dispatching));
    assert_eq!(h.runs.state_of(RUN), Some(RunState::Dispatching));
}

#[tokio::test]
async fn cancel_queued_of_an_unknown_row_is_a_not_found() {
    let h = Builder::new().build().await;
    let stranger = Uuid::from_u128(0xDEAD);

    let error = h
        .service
        .cancel_queued(&owner(), stranger)
        .await
        .expect_err("no such row");
    assert!(matches!(error, DomainError::QueueRowNotFound { id } if id == stranger));
}

/// A cancelled row must not leave its run `Queued` forever — that is the lost
/// run `QueueRepository::requeue` was added to prevent, arrived at from the
/// other side.
#[tokio::test]
async fn cancel_queued_retires_the_run_behind_the_row() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Queued, false),
        )])))
        .queue(queued_fixture())
        .build()
        .await;

    h.service
        .cancel_queued(&owner(), QUEUE)
        .await
        .expect("cancel");

    assert_eq!(h.queue.state_of(QUEUE), Some(QueueState::Cancelled));
    assert_eq!(h.runs.state_of(RUN), Some(RunState::Canceled));
}

/// A foreign tenant's queue row is invisible, so the answer is the same
/// not-found an absent row gets.
#[tokio::test]
async fn cancel_queued_for_another_tenants_row_is_a_not_found() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Queued, false),
        )])))
        .queue(queued_fixture())
        .build()
        .await;

    let error = h
        .service
        .cancel_queued(&ctx(OTHER_TENANT), QUEUE)
        .await
        .expect_err("invisible");
    assert!(matches!(error, DomainError::QueueRowNotFound { id } if id == QUEUE));
    assert_eq!(h.queue.state_of(QUEUE), Some(QueueState::Queued));
}

/// Guide lines 116-120: force start *"starts it now, ignoring what occupies the
/// platform, including an in-flight exclusive run."*
///
/// The occupant here is a real exclusive lease, so the CAS refuses the
/// acquisition and the row is dispatched anyway — which is the override, stated
/// in [`RunsService::claim_by_force`] together with what it costs.
#[tokio::test]
async fn force_start_ignores_an_exclusive_occupant() {
    let occupant = Uuid::from_u128(0xF00D);
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Queued, false),
        )])))
        .queue(queued_fixture())
        .environments(Arc::new(FakeEnvironments::holding(
            PLATFORM_A,
            LeaseState::HeldExclusive { holder: occupant },
        )))
        .build()
        .await;

    let started = h
        .service
        .force_start(&owner(), QUEUE)
        .await
        .expect("force start bypasses the platform");

    assert_eq!(started, RUN);
    assert_eq!(h.queue.state_of(QUEUE), Some(QueueState::Dispatching));
    assert_eq!(h.dispatcher.calls(), vec![(RUN, Some(QUEUE))]);
    // The occupant keeps the lease: the override is that we start anyway, not
    // that we evict them.
    assert_eq!(
        h.environments
            .get_lease(&owner(), PLATFORM_A)
            .await
            .unwrap(),
        LeaseState::HeldExclusive { holder: occupant }
    );
}

/// The other half of the same call: when the platform *is* free, force start
/// takes the lease normally, so the run registers as occupancy like any other.
#[tokio::test]
async fn force_start_takes_the_lease_when_the_platform_is_free() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Queued, true),
        )])))
        .queue(Arc::new(FakeQueue::with(vec![queued_row(
            QUEUE,
            OWNER_TENANT,
            RUN,
            PLATFORM_A,
            true,
            QueueState::Queued,
        )])))
        .build()
        .await;

    h.service
        .force_start(&owner(), QUEUE)
        .await
        .expect("started");

    assert_eq!(
        h.environments.acquired_modes(),
        vec![LeaseMode::Exclusive],
        "the row's own exclusivity chooses the mode"
    );
    assert_eq!(
        h.environments
            .get_lease(&owner(), PLATFORM_A)
            .await
            .unwrap(),
        LeaseState::HeldExclusive { holder: RUN }
    );
}

/// The deliberate asymmetry, guide lines 116-120: *"It does **not** override
/// `max_concurrent_runs`; that still answers `429` and leaves the row queued.
/// … overriding cluster capacity can wedge the whole namespace for everyone."*
///
/// The cap is checked **before** the claim, so the row is left untouched.
#[tokio::test]
async fn force_start_does_not_override_the_concurrency_limit() {
    let executor = Arc::new(CountingExecutor::new());
    // One live execution, and a cap of one.
    executor
        .inner
        .start(crate::domain::ports::run_executor::RunSpec {
            run_id: Uuid::from_u128(0xBEEF),
            run_name: "occupant-1".to_owned(),
            nodes: vec![crate::domain::ports::run_executor::ExecutionNode {
                name: "repo-a".to_owned(),
                bundle_ref: "bundle://x".to_owned(),
                test_files: vec!["tests/a.py".to_owned()],
            }],
            env: crate::domain::ports::run_executor::RunEnv::default(),
            access: crate::domain::ports::run_executor::RunAccess::default(),
            runner: crate::domain::ports::run_executor::RunnerSpec::default(),
            timeout_seconds: 60,
        })
        .await
        .expect("the mock accepts it");

    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Queued, false),
        )])))
        .queue(queued_fixture())
        .executor(executor)
        .max_concurrent_runs(1)
        .build()
        .await;

    let error = h
        .service
        .force_start(&owner(), QUEUE)
        .await
        .expect_err("cluster capacity is not overridable");

    assert!(
        matches!(error, DomainError::ConcurrencyLimit { limit } if limit == 1),
        "{error}"
    );
    assert_eq!(
        h.queue.state_of(QUEUE),
        Some(QueueState::Queued),
        "a 429 must leave the row untouched and still queued"
    );
    assert!(h.dispatcher.calls().is_empty());
}

#[tokio::test]
async fn force_start_refuses_a_row_that_is_no_longer_queued() {
    for state in [
        QueueState::Dispatching,
        QueueState::Running,
        QueueState::Done,
        QueueState::Failed,
        QueueState::Cancelled,
        QueueState::Expired,
    ] {
        let h = Builder::new()
            .runs(Arc::new(FakeRuns::with(vec![(
                OWNER_TENANT,
                stored(RunState::Queued, false),
            )])))
            .queue(Arc::new(FakeQueue::with(vec![row_in(state)])))
            .build()
            .await;

        let error = h.service.force_start(&owner(), QUEUE).await.unwrap_err();
        assert!(
            matches!(error, DomainError::QueueRowNotQueued { .. }),
            "{state:?}: {error}"
        );
        assert!(h.dispatcher.calls().is_empty(), "{state:?}");
    }
}

/// A submit that fails after the claim propagates, so the caller learns the
/// force start did not start anything. The claim is left to
/// `service::dispatch`'s own failure handling, which is what `dispatch_inline`
/// wraps.
#[tokio::test]
async fn a_force_start_whose_submit_fails_reports_the_failure() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Queued, false),
        )])))
        .queue(queued_fixture())
        .dispatcher(Arc::new(RecordingDispatcher::failing("no capacity")))
        .build()
        .await;

    let error = h.service.force_start(&owner(), QUEUE).await.unwrap_err();
    assert!(matches!(error, DomainError::ExecutorFailed(_)), "{error}");
    assert_eq!(h.dispatcher.calls(), vec![(RUN, Some(QUEUE))]);
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// **The service hands the repository the watermark it was given, and returns
/// the page unaltered.** That, and not the ordering, is what this tier can
/// prove.
///
/// # Renamed by the Task 4 spec review, and why the old name was wrong
///
/// This shipped as `list_runs_finished_since_returns_oldest_first_within_the_limit`,
/// which is the plan's own name for it. The reviewer then broke the SQL three
/// separate ways - dropped the `id` ordering, changed `>=` to `>`, and flipped
/// `finished_at` to `Desc` - and this test stayed **green** through all three.
/// A test called "returns oldest first" that survives flipping the sort
/// direction cannot fail for the reason its name gives, and a reader looking at
/// a green run would have drawn exactly the wrong conclusion. The name now
/// describes the delegation, which is the claim the fixture actually falsifies:
/// a service that swallowed `since`, re-sorted the repository's answer, or
/// returned a different `Vec` than it was handed would fail here.
///
/// # What owns the ordering instead
///
/// The predicate and both sort keys are pinned against a real database by
/// `runs_sea_repo::tests::the_sweep_returns_runs_finished_at_or_after_the_watermark_oldest_first`
/// and
/// `runs_sea_repo::tests::the_sweep_breaks_a_finished_at_tie_on_id_so_the_page_boundary_is_stable`.
/// Those two are load-bearing and must **not** be deleted as redundant with
/// this one - `FakeRuns::list_finished_since` carries the argument for why the
/// double cannot stand in for them.
#[tokio::test]
async fn the_sweep_hands_the_repository_its_watermark_and_returns_the_page_unaltered() {
    let h = Builder::new().build().await;
    let older = h.finished_run_at(OWNER_TENANT, datetime!(2026-08-18 10:00:00 UTC));
    let newer = h.finished_run_at(OWNER_TENANT, datetime!(2026-08-18 12:00:00 UTC));
    let _before = h.finished_run_at(OWNER_TENANT, datetime!(2026-08-18 08:00:00 UTC));

    let found = h
        .service
        .list_runs_finished_since(&owner(), datetime!(2026-08-18 09:00:00 UTC), 10)
        .await
        .expect("sweep succeeds");

    assert_eq!(
        found.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![older, newer],
        "the watermark must reach the repository unchanged and its answer must \
         come back unreordered and untrimmed; the SQL that makes that answer \
         oldest-first is runs_sea_repo's to prove, not this test's"
    );
}

/// **The service hands the repository the `limit` it was given**, neither
/// widening it nor dropping it.
///
/// `limit` is not advisory: a sweep that returned more than it was asked for
/// would let a reconciler advance its watermark past runs it never processed.
/// A service that passed `u32::MAX`, or that fetched wide and truncated after
/// the fact, fails here.
///
/// # Renamed by the Task 4 spec review
///
/// Was `the_sweep_stops_at_the_limit_it_was_given`, which reads as a claim
/// about *where* the page is cut. It is not one: the reviewer's three SQL
/// breaks left this green, and the double is what does the truncating. The cut
/// itself - that a short page is the same prefix of the same total order every
/// time - belongs to
/// `runs_sea_repo::tests::the_sweep_breaks_a_finished_at_tie_on_id_so_the_page_boundary_is_stable`,
/// which takes a page of three from six runs tied on `finished_at`.
#[tokio::test]
async fn the_sweep_hands_the_repository_the_limit_it_was_given() {
    let h = Builder::new().build().await;
    let first = h.finished_run_at(OWNER_TENANT, datetime!(2026-08-18 10:00:00 UTC));
    let _second = h.finished_run_at(OWNER_TENANT, datetime!(2026-08-18 11:00:00 UTC));
    let _third = h.finished_run_at(OWNER_TENANT, datetime!(2026-08-18 12:00:00 UTC));

    let found = h
        .service
        .list_runs_finished_since(&owner(), datetime!(2026-08-18 09:00:00 UTC), 1)
        .await
        .unwrap();

    assert_eq!(
        found.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![first],
        "one run asked for, one run returned - a service that widened the limit \
         or ignored it would hand back three"
    );
}

/// **The service compiles a `qa.run` scope for the caller and reads under it**,
/// rather than reading unscoped and filtering the rows afterwards.
///
/// This is the one claim at this tier that no other test makes, and it is the
/// reason the spec review's verdict was *keep the double*: `FakeRuns` asserts
/// its scope through `assert_scope_is_for(scope, "qa.run", ..)` before it
/// answers, so a service that passed a `qa.queue_entry` scope, or an
/// unconstrained one, fails inside the repository call itself - before any row
/// comparison. Deleting this test to lean on the real-database suites would
/// lose that assertion, because a correctly scoped read and an unscoped read
/// filtered after the fact are indistinguishable from their *results*.
///
/// # Renamed by the Task 4 spec review
///
/// Was `the_sweep_is_scoped_to_the_callers_own_tenant`, which promises row-level
/// isolation this tier cannot deliver: the rows come from a `Vec` that chooses
/// to honour the scope it is handed. Isolation against a real
/// `PolicyEnforcer`, real `SecureORM` and real tables - reached through the SDK
/// method itself - is
/// `tenant_scoping_tests::the_reconciler_reads_are_invisible_to_another_tenant`,
/// and at the query level
/// `runs_sea_repo::tests::the_sweep_never_returns_another_tenants_finished_run`.
///
/// The symmetric fixture is kept: both tenants own a run that finished inside
/// the same window, so "the stranger sees nothing" cannot pass by the read
/// being broken for everybody.
#[tokio::test]
async fn the_sweep_reads_under_a_qa_run_scope_compiled_for_the_caller() {
    let h = Builder::new().build().await;
    let mine = h.finished_run_at(OWNER_TENANT, datetime!(2026-08-18 10:00:00 UTC));
    let theirs = h.finished_run_at(OTHER_TENANT, datetime!(2026-08-18 10:00:00 UTC));

    let watermark = datetime!(2026-08-18 09:00:00 UTC);
    let mine_sees = h
        .service
        .list_runs_finished_since(&owner(), watermark, 10)
        .await
        .unwrap();
    assert_eq!(
        mine_sees.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![mine],
        "the other tenant's run finished inside the same window and must still \
         not be in this page"
    );

    let theirs_sees = h
        .service
        .list_runs_finished_since(&ctx(OTHER_TENANT), watermark, 10)
        .await
        .unwrap();
    assert_eq!(
        theirs_sees.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![theirs],
        "and symmetrically - a sweep returns its caller's own runs, not an \
         empty page for everyone"
    );
}

#[tokio::test]
async fn reads_are_scoped_to_the_callers_own_tenant() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Succeeded, false),
        )])))
        .build()
        .await;

    assert_eq!(h.service.get(&owner(), RUN).await.unwrap().id, RUN);
    assert_eq!(
        h.service
            .list(&owner(), &ODataQuery::new())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(h.service.get_result(&owner(), RUN).await.unwrap().total, 0);

    let stranger = ctx(OTHER_TENANT);
    assert!(matches!(
        h.service.get(&stranger, RUN).await,
        Err(DomainError::RunNotFound { .. })
    ));
    assert!(
        h.service
            .list(&stranger, &ODataQuery::new())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(matches!(
        h.service.get_result(&stranger, RUN).await,
        Err(DomainError::RunNotFound { .. })
    ));
    assert!(matches!(
        h.service.test_results(&stranger, RUN).await,
        Err(DomainError::RunNotFound { .. })
    ));
}

// ---------------------------------------------------------------------------
// The container
// ---------------------------------------------------------------------------

/// **The composition no test inside either module can see.**
///
/// Three claims `AppServices` makes about the two services Task 15 added, each
/// of which would still compile, still pass every unit test above, and still be
/// broken:
///
/// 1. **Force start shares admission's platform lock registry.** Built over a
///    second `PlatformLocks::default()` it would serialise against nothing, so a
///    force start and a concurrent launch could each observe an idle platform.
///    That is the same failure
///    `dispatch::tests::admission_and_dispatch_share_one_platform_lock_registry`
///    exists for, arrived at from the third service.
/// 2. **A re-run goes through the *same* launch service the container built.** A
///    second `LaunchService` would be a second name sequence and a second set of
///    scopes for what is contractually one creation path.
/// 3. **Ingest's log fan-out is wired to the broadcaster the container was
///    given.** A container that dropped it on the floor would leave every SSE
///    subscriber silent, and nothing else here would notice.
#[tokio::test]
async fn the_container_wires_ingest_and_the_operator_actions_to_the_same_halves() {
    let logs = Arc::new(crate::infra::logs::RunLogBroadcaster::new(8));
    let services = crate::domain::service::AppServices::new(
        // Seeded, because a `Log` event is gated on the run existing under the
        // caller's own scope exactly as every other event is.
        Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Running, false),
        )])),
        Arc::new(FakeQueue::default()),
        // Task 19 gave the container a schedules repository. Nothing in this
        // test reaches it, so the real stateless unit struct is passed rather
        // than a double that would only assert it is never called.
        Arc::new(crate::infra::storage::OrmSchedulesRepository),
        crate::domain::service::ServiceDeps {
            db: test_db_provider().await,
            authz: Arc::new(SystemGrantingAuthZ),
            catalog: Arc::new(FakeCatalog::serving(&["tests/a.py"])),
            environments: Arc::new(FakeEnvironments::free()),
            product_plugins: Arc::new(
                crate::domain::service::admission::tests::fakes::FakeProductPlugins::default(),
            ),
            executor: Arc::new(MockRunExecutor::new()),
            logs: Arc::clone(&logs) as Arc<dyn LogFanout>,
            archive: Arc::new(NullLogArchive) as Arc<dyn LogArchive>,
            admitter: None,
            dispatcher: None,
            watcher: None,
            default_timeout_seconds: 900,
            limits: QueueLimits {
                queue_max_depth: 20,
                max_concurrent_runs: 0,
                queue_ttl_seconds: 7200,
            },
            orphan_timeout_seconds: 600,
        },
    );

    assert!(
        Arc::ptr_eq(
            &services.admission.locks().get(PLATFORM_A).await,
            &services.runs.locks().get(PLATFORM_A).await,
        ),
        "force start must contend for the same per-platform mutex admission and the \
         tick use, or a forced claim is invisible to a concurrent launch"
    );
    assert!(
        Arc::ptr_eq(&services.launch, services.runs.launch_service()),
        "a re-run must go through the one creation path, not a second launch service"
    );
    // Claim 4: the two callers of `max_concurrent_runs` reserve from one gate.
    // The gate counts the slots it has handed out and not yet had back, so a
    // second `AdmissionService` would be a second counter and force start would
    // stop seeing a concurrent launch's reservation — the frozen guide's
    // "force start does not override max_concurrent_runs" broken again, and
    // invisible to every test inside either module. Raw pointers because one
    // side is an `Arc<dyn Admitter>`.
    let via_launch: *const () = Arc::as_ptr(services.launch.admitter()).cast();
    let via_force_start: *const () = Arc::as_ptr(services.runs.admission()).cast();
    assert_eq!(
        via_launch, via_force_start,
        "launch and force start must reserve cluster capacity from one admission service"
    );

    let mut subscriber = logs
        .subscribe(RUN)
        .expect("under the per-run subscriber cap");
    services
        .ingest
        .apply(
            &owner(),
            RUN,
            crate::domain::ports::run_executor::ExecutionEvent::Log {
                node: "repo-a".to_owned(),
                line: "wired".to_owned(),
            },
        )
        .await
        .expect("the run is visible to this caller");
    assert_eq!(
        subscriber.recv().await.as_deref(),
        Some("[repo-a] wired"),
        "the container's ingest must publish to the broadcaster it was given"
    );
}

// ---------------------------------------------------------------------------
// The replay transcription, on its own
// ---------------------------------------------------------------------------

/// The upward-only rule at the one line that implements it, so the property is
/// pinned independently of a whole launch.
#[test]
fn replay_inherits_exclusivity_upward_only() {
    let exclusive = replay(&stored(RunState::Succeeded, true)).unwrap();
    assert_eq!(exclusive.exclusive, Some(true));

    let parallel = replay(&stored(RunState::Succeeded, false)).unwrap();
    assert_eq!(
        parallel.exclusive, None,
        "`None` means inherit; `Some(false)` would pin the launch tier and suppress a \
         TEST_META declaration added since"
    );
}

/// Everything else the replay carries, field by field, because each one is a
/// decision and a whole-launch test only observes the ones that changed
/// behaviour.
#[test]
fn replay_carries_the_stored_target_filter_and_parameters() {
    let mut run = stored(RunState::Succeeded, false);
    run.target = RunTarget::Test {
        repo_id: REPO,
        path: "tests/plan.yaml".to_owned(),
        test_file: "tests/a.py".to_owned(),
    };
    run.include_tags = vec!["smoke".to_owned()];
    run.exclude_tags = vec!["destructive".to_owned()];
    run.parameters = vec![RunParameter {
        name: "REGION".to_owned(),
        value: "eu".to_owned(),
    }];

    let request = replay(&run).unwrap();
    assert_eq!(request.target, run.target);
    assert_eq!(request.target.kind(), RunKind::Test);
    assert_eq!(request.platform_id, Some(PLATFORM_A));
    assert_eq!(request.include_tags, vec!["smoke".to_owned()]);
    assert_eq!(request.exclude_tags, vec!["destructive".to_owned()]);
    assert_eq!(request.parameters, run.parameters);
    assert_eq!(
        request.timeout_seconds, None,
        "a re-run resolves a new deadline through `domain::timeout`'s three chains \
         rather than replaying the old row's absolute `timeout_at`"
    );
}

/// A collect run is **refused** by `replay`, which is what closes the second
/// REST-reachable path to the admission bypass.
///
/// `POST /qa/v1/runs/{id}/rerun` reaches `LaunchService::launch` through this
/// function (`api::rest::routes::runs`, `handlers::runs::rerun_run`,
/// `RunsService::rerun`). Without the guard a caller who can see one collect
/// run — which the hourly cycle guarantees exists — could launch admission-
/// bypassing runs at will: no `max_concurrent_runs`, no `queue_max_depth`, no
/// 429.
///
/// The guard is not "collect must never re-run" as a matter of taste. Legacy
/// cannot re-run one either: its collect workflow is annotated
/// `run-kind = "plan"` (`manager/src/services/argo.rs:608`) with the synthetic
/// plan id `collect-{repo.id}` and an empty `dir_path`
/// (`manager/src/services/collect.rs:93-94`), so legacy's rerun resolves a plan
/// that was never written and fails.
///
/// **This test replaced one asserting the opposite.** `replay` clones
/// `run.target`, so a collect run replayed cleanly and the first version of
/// this test pinned that as correct behaviour. It was correct about the
/// mechanism and wrong about the policy; the mechanism is what made the hole.
///
/// Break-verified: removing the `matches!(.., RunKind::Collect)` guard in
/// `replay` turns this red and leaves every other `replay` test green.
#[test]
fn replay_refuses_a_collect_run() {
    let mut run = stored(RunState::Succeeded, false);
    run.platform_id = None;
    run.target = RunTarget::Collect {
        repo_id: REPO,
        collect_url: "https://insights.example/qa/v1/collect/r/main".to_owned(),
    };

    match replay(&run) {
        Err(DomainError::Validation { field, message }) => {
            assert_eq!(field, "target.kind");
            assert!(
                message.contains("bypasses admission"),
                "the refusal must say why: {message}"
            );
        }
        other => panic!("expected a Validation on target.kind, got {other:?}"),
    }
}

/// The refusal is keyed on the **kind**, so the three submittable kinds are
/// untouched by it. Asserted rather than assumed: a guard written as
/// `!matches!(.., RunKind::Plan)` would compile, refuse re-runs of every single
/// test and custom plan, and turn no collect test red.
#[test]
fn replay_still_accepts_every_submittable_kind() {
    for target in [
        RunTarget::Plan {
            repo_id: REPO,
            path: "tests/plan.yaml".to_owned(),
        },
        RunTarget::Test {
            repo_id: REPO,
            path: "tests/plan.yaml".to_owned(),
            test_file: "tests/a.py".to_owned(),
        },
        RunTarget::CustomPlan { id: REPO },
    ] {
        let mut run = stored(RunState::Succeeded, false);
        run.target = target.clone();
        assert_eq!(
            replay(&run).expect("a submittable kind re-runs").target,
            target
        );
    }
}

/// A branch of whitespace is no branch: the source system trims and
/// non-empty-filters the value it replays (`routes/runs.rs:927-933`).
#[test]
fn replay_treats_a_blank_recorded_branch_as_missing() {
    let mut run = stored(RunState::Succeeded, false);
    run.test_version = Some("   ".to_owned());
    assert!(matches!(
        replay(&run),
        Err(DomainError::Validation { ref field, .. }) if field == "test_version"
    ));

    run.test_version = Some("  main  ".to_owned());
    assert_eq!(replay(&run).unwrap().branch.as_deref(), Some("main"));
}

// ---------------------------------------------------------------------------
// The security review's findings, as regressions
// ---------------------------------------------------------------------------

/// **B3.** The row cancel and the run's retirement used to be two writes in that
/// order, so *any* failure between them — a foreign run, a dropped connection, a
/// denied scope — left `row = cancelled` and `run = queued`. The security review
/// executed it and got exactly that.
///
/// It is unrecoverable, not untidy: `list_timeout_candidates` matches
/// `dispatching | running` only, so the control-plane sweep never sees a `Queued`
/// run, and the TTL sweep works on `queued` **rows**, which is the one just
/// cancelled. The run waits forever.
///
/// The failure is injected by putting the run in another tenant while leaving the
/// row visible — the same split the review used, and the cheapest way to make the
/// second step fail after the first would have succeeded.
#[tokio::test]
async fn a_queue_cancel_that_cannot_retire_its_run_leaves_the_row_alone() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OTHER_TENANT,
            stored(RunState::Queued, false),
        )])))
        .queue(queued_fixture())
        .build()
        .await;

    let error = h
        .service
        .cancel_queued(&owner(), QUEUE)
        .await
        .expect_err("the run behind the row is not this caller's");

    assert!(matches!(error, DomainError::RunNotFound { .. }), "{error}");
    assert_eq!(
        h.queue.state_of(QUEUE),
        Some(QueueState::Queued),
        "the row must be untouched: a cancelled row whose run stays `Queued` is a \
         lost run, and nothing in this gear reclaims it"
    );
}

/// **B4(a).** Only `service::ingest` reaped, so an operator cancel left the run's
/// log channel alive — and since `LogSubscription::recv` answers `None` only when
/// the channel is dropped, an SSE handler watching a cancelled run held its
/// connection open forever.
#[tokio::test]
async fn cancelling_a_run_reaps_its_live_log_channel() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Queued, false),
        )])))
        .queue(queued_fixture())
        .build()
        .await;
    assert!(h.logs.reaped().is_empty());

    h.service.cancel(&owner(), RUN).await.expect("cancel");
    assert_eq!(h.logs.reaped(), vec![RUN]);
}

/// The same for the queue-row entry point, which retires the run through the same
/// choke point.
#[tokio::test]
async fn cancelling_a_queued_row_reaps_the_runs_log_channel() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Queued, false),
        )])))
        .queue(queued_fixture())
        .build()
        .await;

    h.service
        .cancel_queued(&owner(), QUEUE)
        .await
        .expect("cancel");
    assert_eq!(h.logs.reaped(), vec![RUN]);
}

/// A refused cancel must **not** reap: the run is still live and its watchers
/// must keep their stream.
#[tokio::test]
async fn a_refused_cancel_leaves_the_log_channel_alone() {
    let mut run = stored(RunState::Running, false);
    run.execution_ref = Some("mock-execution-1".to_owned());
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .executor(Arc::new(CountingExecutor::failing_cancel()))
        .build()
        .await;

    h.service.cancel(&owner(), RUN).await.unwrap_err();
    assert!(h.logs.reaped().is_empty());
}

/// **Adopted from the spec review's probe.** A re-run re-reads the platform
/// rather than replaying the stored `platform_id`, which is what stops a run
/// whose platform was deleted or reassigned since from taking the *global*,
/// non-tenant-partitioned lease on it.
///
/// The behaviour was already correct and had no test: all eight `rerun_*` tests
/// used `FakeEnvironments::free()`, which serves both fixture tenants, so the
/// re-read could have been deleted without reddening anything.
#[tokio::test]
async fn rerun_re_reads_the_platform_and_refuses_one_that_is_gone() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            stored(RunState::Succeeded, false),
        )])))
        // No platform is served to any tenant: the platform the stored run names
        // has been deleted or reassigned since it ran.
        .environments(Arc::new(FakeEnvironments::default()))
        .build()
        .await;

    let error = h
        .service
        .rerun(&owner(), RUN)
        .await
        .expect_err("a re-run must not replay an unverifiable platform");

    assert!(matches!(error, DomainError::Environments(_)), "{error}");
    assert!(
        h.runs.created.lock().unwrap().is_empty(),
        "and it fails before any row is written, so nothing is stranded"
    );
}

// ---------------------------------------------------------------------------
// The queue listing
// ---------------------------------------------------------------------------

const RUN_2: Uuid = Uuid::from_u128(0x0602);
const RUN_3: Uuid = Uuid::from_u128(0x0603);
const QUEUE_2: Uuid = Uuid::from_u128(0x0E62);
const QUEUE_3: Uuid = Uuid::from_u128(0x0E63);

/// Two queued rows on one platform, oldest first, plus a running holder.
async fn queue_view_harness() -> Harness {
    Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![
            (
                OWNER_TENANT,
                run_fixture(RUN, Some(PLATFORM_A), false, RunState::Running),
            ),
            (
                OWNER_TENANT,
                run_fixture(RUN_2, Some(PLATFORM_A), false, RunState::Queued),
            ),
            (
                OWNER_TENANT,
                run_fixture(RUN_3, Some(PLATFORM_A), false, RunState::Queued),
            ),
        ])))
        .queue(Arc::new(FakeQueue::with(vec![
            // The holder: `running`, so it takes no position and is what the
            // head-of-queue row is waiting for.
            row_aged(
                QUEUE,
                OWNER_TENANT,
                RUN,
                PLATFORM_A,
                false,
                QueueState::Running,
                300,
            ),
            row_aged(
                QUEUE_2,
                OWNER_TENANT,
                RUN_2,
                PLATFORM_A,
                false,
                QueueState::Queued,
                200,
            ),
            row_aged(
                QUEUE_3,
                OWNER_TENANT,
                RUN_3,
                PLATFORM_A,
                false,
                QueueState::Queued,
                100,
            ),
        ])))
        .build()
        .await
}

/// The three derived fields, together, because they are only meaningful
/// together: a position without a TTL deadline or a blocker sentence is a
/// number an operator cannot act on.
///
/// **What this does not pin.** The double ignores the `ODataQuery` entirely -
/// it applies scope and the `platform_id` narrowing and nothing else - so
/// nothing here says anything about `$filter`, `$orderby` or cursors. **Nor
/// does anything else**: `list_page` has no test at the storage tier either.
/// See `test_support::MockRunsRepository::list_page`.
#[tokio::test]
async fn the_queue_listing_derives_position_ttl_and_blocker_together() {
    let h = queue_view_harness().await;

    let page = h
        .service
        .queue_page(&owner(), Some(PLATFORM_A), &ODataQuery::new())
        .await
        .unwrap();

    let by_id = |id: Uuid| {
        page.items
            .iter()
            .find(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("{id} must be listed"))
    };

    // The holder takes no position, and therefore no deadline and no blocker:
    // it is not waiting for anything, it is what everything else waits for.
    let holder = by_id(QUEUE);
    assert_eq!(holder.queue_position, None);
    assert_eq!(holder.ttl_expires_at, None);
    assert_eq!(holder.blocked_by, None);

    // Oldest queued row is position 1 and is told what holds the platform, by
    // name - which is the whole reason runs carry a stable short name.
    let head = by_id(QUEUE_2);
    assert_eq!(head.queue_position, Some(1));
    assert_eq!(head.blocked_by.as_deref(), Some("waiting for run run-1537"));
    assert!(
        head.ttl_expires_at.is_some(),
        "a queued row has a TTL deadline whenever queue_ttl_seconds is non-zero"
    );

    // The one behind it is told about the queue, not about the platform.
    let second = by_id(QUEUE_3);
    assert_eq!(second.queue_position, Some(2));
    assert_eq!(
        second.blocked_by.as_deref(),
        Some("waiting for 1 queued run ahead of it")
    );
}

/// `ttl_expires_at` is `None` when expiry is switched off, and that is not the
/// same as "no deadline computed yet" - it is the documented meaning of
/// `queue_ttl_seconds: 0`.
#[tokio::test]
async fn a_disabled_ttl_reports_no_deadline_rather_than_a_deadline_of_now() {
    let h = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            run_fixture(RUN, Some(PLATFORM_A), false, RunState::Queued),
        )])))
        .queue(queued_fixture())
        .ttl_seconds(0)
        .build()
        .await;

    let page = h
        .service
        .queue_page(&owner(), None, &ODataQuery::new())
        .await
        .unwrap();

    assert_eq!(page.items[0].queue_position, Some(1));
    assert_eq!(page.items[0].ttl_expires_at, None);
}

/// The listing is scoped like every other read on this gear: another tenant
/// sees nothing, rather than seeing rows without their details.
#[tokio::test]
async fn the_queue_listing_is_invisible_to_another_tenant() {
    let h = queue_view_harness().await;

    let page = h
        .service
        .queue_page(&ctx(OTHER_TENANT), None, &ODataQuery::new())
        .await
        .unwrap();

    assert!(page.items.is_empty());
}
