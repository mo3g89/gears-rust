//! The registry's one-producer property, and the composition this whole task
//! exists to produce: **a run reaching a terminal state from an
//! `ExecutionEvent::Finished` rather than from its deadline.**
//!
//! The second half runs against a real in-memory `SQLite` database, this gear's
//! real migrations and repositories, a real `PolicyEnforcer`, and the
//! **production** `SpawningRunWatcher` — `ServiceDeps::watcher` is `None` here,
//! which is the wiring `gear.rs` uses. Only the executor, the two cross-gear
//! clients and the event sink are doubles, and the executor double is the point:
//! it is what emits the `Finished`.
//!
//! # What these tests do not prove
//!
//! * **That `init` wires any of this.** `gear.rs` needs a database, a
//!   `ClientHub` and four resolved cross-gear clients, so no test constructs it;
//!   the wiring is verified by reading it, exactly as
//!   `the_log_wiring_hands_out_two_views_of_one_broadcaster` and
//!   `admission_and_dispatch_share_one_platform_lock_registry` are.
//! * **That the leader gate gates anything.** `NoopLeaderElector` is the only
//!   elector shipped, so every replica runs the tick and the gate is vacuous —
//!   which also means nothing here can reach the multi-process behaviour
//!   `service::watch`'s header describes. It needs two processes; this suite has
//!   one, and its database has no row locks. The races that used to follow from
//!   it are closed and are covered by `ingest_races_pg_tests` against real
//!   Postgres, not here.
//! * **That a log line reaches anybody.** Neither side is exercised: nothing
//!   here builds a router, and no test in this file scripts an
//!   `ExecutionEvent::Log`, so nothing here reaches
//!   `RunLogBroadcaster::publish` at all. Note that
//!   the replica-locality consequence is *conditional*: `RunLogBroadcaster` is a
//!   per-process channel map, but under the shipped `NoopLeaderElector` every
//!   replica ticks and so publishes into the map its own router reads, and it is
//!   only under a real elector that a subscriber on a non-leader would get a 200
//!   and silence. `infra::logs::broadcast` carries it.
//! * **That re-attachment works against a real executor.** Every test above
//!   this line builds `harness()`, whose `NullLogArchive` always answers an
//!   empty [`LogResume`] (nothing was ever archived under it), so
//!   `MockRunExecutor` replays those runs' scripts from the beginning exactly
//!   as it always has — an adapter that dropped everything emitted before a
//!   re-attach would pass every one of them. The Finding #50 section below
//!   this comment is the exception: it builds its own harness over a real
//!   `RunLogArchive` precisely so a non-empty resume position reaches the
//!   mock, and the mock is what makes that position exact rather than a
//!   Kubernetes-shaped approximation — see `infra::executor::mock`'s module
//!   doc.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{AttachedSlot, WatchRegistry};

/// **The property the one-producer rule rests on.** A second `claim` for the
/// same run returns `None`, and `None` is not a spawn — there is no other
/// route to the observer task.
#[test]
fn a_run_can_be_claimed_once() {
    let registry = WatchRegistry::default();
    let run = Uuid::from_u128(0x00B5_E04E);

    let first = registry.claim(run);
    assert!(first.is_some(), "the first claim wins");
    assert!(
        registry.claim(run).is_none(),
        "a second observer on one run is what gives IngestService two \
         producers: no longer a correctness hazard since Task 16b step 1, but \
         still duplicated work that contends with itself"
    );
    assert!(registry.holds(run));

    drop(first);
    assert!(
        !registry.holds(run),
        "the slot's Drop is what frees the run, so an observer that ended - \
         however it ended - does not block the next attempt"
    );
}

/// Two different runs are independent, which is the other half: a registry
/// that serialised on one global flag would observe one run at a time.
#[test]
fn two_runs_are_claimed_independently() {
    let registry = WatchRegistry::default();
    let (a, b) = (Uuid::from_u128(0xA), Uuid::from_u128(0xB));

    let _first = registry.claim(a).expect("a is free");
    assert!(registry.claim(b).is_some(), "b is a different run");
}

/// The release happens on **every** exit path, not just the tidy one. A
/// hand-written release at the bottom of the drain would leak the slot when
/// the drain returned early or panicked, and the run would then never be
/// observed again - silently and permanently.
#[test]
fn a_panicking_observer_still_frees_its_run() {
    let registry = WatchRegistry::default();
    let run = Uuid::from_u128(0xDEAD);
    let slot = registry.claim(run).expect("free");

    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _slot: AttachedSlot = slot;
        panic!("the observer task died");
    }));

    assert!(unwound.is_err(), "the panic must have happened");
    assert!(
        !registry.holds(run),
        "and the run must still be re-attachable afterwards"
    );
}

/// **Cancelling the token an observer was spawned with ends it, and
/// `shutdown` proves that rather than hoping the scheduler got around to
/// it.** Built around `std::future::pending` rather than a scripted
/// executor: `MockRunExecutor::watch` "materialises the whole sequence into
/// the channel before returning" (its own module doc), so a real drain
/// against it completes on its own so fast that racing it against
/// cancellation would prove nothing about which one actually ended the
/// task. This isolates the one thing review finding #18 is about — the
/// `tokio::select!` `SpawningRunWatcher::attach` adds — by reproducing its
/// exact shape against a future that never resolves unless told to.
#[tokio::test]
async fn cancelling_the_token_ends_an_observer_that_would_otherwise_never_stop() {
    let registry = WatchRegistry::default();
    let run = Uuid::from_u128(0xF00D);
    let slot = registry.claim(run).expect("free");
    let cancel = CancellationToken::new();
    let ended = Arc::new(AtomicBool::new(false));

    let handle = {
        let cancel = cancel.clone();
        let ended = Arc::clone(&ended);
        tokio::spawn(async move {
            // Mirrors `SpawningRunWatcher::attach`'s own shape: the slot is
            // moved in so it drops - and the run frees - however this task
            // ends.
            let _slot = slot;
            tokio::select! {
                () = cancel.cancelled() => {}
                () = std::future::pending::<()>() => {}
            }
            ended.store(true, Ordering::SeqCst);
        })
    };
    registry.track(handle);

    // Let the spawned task actually reach its `select!` before asserting
    // anything about it - otherwise this could pass even if `track` or the
    // select were both missing, by racing the assertions ahead of the task
    // ever being polled.
    tokio::task::yield_now().await;
    assert!(
        registry.holds(run),
        "the observer is live before cancellation"
    );
    assert!(
        !ended.load(Ordering::SeqCst),
        "premise: nothing but cancellation ends this observer"
    );

    cancel.cancel();
    registry.shutdown().await;

    assert!(
        ended.load(Ordering::SeqCst),
        "shutdown must have actually waited for the task to run past its select, \
         not merely returned because nothing was tracked"
    );
    assert!(
        !registry.holds(run),
        "the slot drops when the observer ends, freeing the run for a later attempt"
    );
}

/// **`shutdown` on an empty registry is not a hang.** The loop in
/// `WatchRegistry::shutdown` re-snapshots until it sees an empty `Vec`; the
/// degenerate case — nothing was ever tracked — must return on the first
/// snapshot rather than waiting for something that will never arrive.
#[tokio::test]
async fn shutdown_of_a_registry_with_no_observers_returns_immediately() {
    let registry = WatchRegistry::default();

    tokio::time::timeout(std::time::Duration::from_secs(2), registry.shutdown())
        .await
        .expect("shutdown must return promptly when nothing was ever attached");
}

// ---------------------------------------------------------------------------
// The composition: a run that ends because it finished, not because it expired
// ---------------------------------------------------------------------------

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use qa_runs_sdk::{RunState, RunTarget};
use toolkit_security::pep_properties;

use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::{
    ExecutionEvent, ExecutionNode, ExecutionRef, ExecutionSink, ExecutionStream, NodeOutcome,
    RunAccess, RunEnv, RunExecutor, RunSpec, RunnerSpec, TestObservation,
};
use crate::domain::repos::{ArchivedLog, LogResume, RunLogsRepository, RunsRepository};
use crate::domain::service::admission::tests::fakes::{FakeCatalog, FakeEnvironments, PLATFORM_A};
use crate::domain::service::test_support::{NullLogArchive, OWNER_TENANT};
use crate::domain::service::watch::{RunWatcher, SpawningRunWatcher, WatchTarget};
use crate::domain::service::{AppServices, DbProvider, LogArchive, QueueLimits, ServiceDeps};
use crate::domain::state_machine::ExecutorOutcome;
use crate::domain::system_actor::TenantBound;
use crate::gear::ConcreteAppServices;
use crate::infra::executor::mock::MockRunExecutor;
use crate::infra::logs::{RunLogArchive, RunLogBroadcaster};
use crate::infra::storage::test_db::{inmem_db, sample_new_run, scope};
use crate::infra::storage::{OrmQueueRepository, OrmRunsRepository};
use toolkit_db::DBProvider;

/// A policy decision point of the shape `domain::system_actor`'s header
/// describes as the one the enumeration factories **require**: a nil-tenant
/// system subject gets a *covering* constraint set, and a tenant-bound one gets
/// its own tenant.
///
/// `test_support::PermissiveAuthZ` cannot stand in. It answers *no* constraints
/// for a nil tenant, which compiles to `Forbidden` — correct, and it means the
/// re-attachment scan would see nothing and every assertion below would pass
/// vacuously. `fakes::SystemGrantingAuthZ` cannot either: it stamps a marker
/// onto `RESOURCE_ID`, and its own doc says a scope carrying that marker filters
/// out every row of a real `Orm*Repository`.
struct EnumerationGrantingAuthZ {
    covering: Vec<Uuid>,
}

#[async_trait]
impl AuthZResolverApi for EnumerationGrantingAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let subject_tenant = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|tc| tc.root_id)
            .filter(|id| !id.is_nil());
        let tenants = match subject_tenant {
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

struct Harness {
    services: Arc<ConcreteAppServices>,
    db: Arc<DbProvider>,
    executor: Arc<MockRunExecutor>,
    /// The exact token the production watcher was built with — see
    /// `ServiceDeps::cancel`. Held so a test can cancel the real gear-shutdown
    /// signal rather than one it minted itself, which is what makes
    /// `shutdown_reaches_the_production_watcher_through_app_services` a
    /// wiring test rather than a restatement of `WatchRegistry::shutdown`'s
    /// own unit test.
    cancel: CancellationToken,
}

async fn harness() -> Harness {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let executor = Arc::new(MockRunExecutor::new());
    let cancel = CancellationToken::new();
    let services = Arc::new(AppServices::new(
        Arc::new(OrmRunsRepository),
        Arc::new(OrmQueueRepository),
        Arc::new(crate::infra::storage::OrmSchedulesRepository),
        ServiceDeps {
            db: Arc::clone(&db),
            authz: Arc::new(EnumerationGrantingAuthZ {
                covering: vec![OWNER_TENANT],
            }),
            catalog: Arc::new(FakeCatalog::serving(&["tests/a.py"])),
            environments: Arc::new(FakeEnvironments::free()),
            product_plugins: Arc::new(
                crate::domain::service::admission::tests::fakes::FakeProductPlugins::default(),
            ),
            executor: Arc::clone(&executor) as Arc<dyn RunExecutor>,
            logs: Arc::new(RunLogBroadcaster::new(8)),
            archive: Arc::new(NullLogArchive) as Arc<dyn LogArchive>,
            admitter: None,
            dispatcher: None,
            // **The production watcher**, which is what makes this a composition
            // test rather than an assertion about a double.
            watcher: None,
            cancel: cancel.clone(),
            default_timeout_seconds: 3600,
            limits: QueueLimits {
                queue_max_depth: 20,
                max_concurrent_runs: 0,
                queue_ttl_seconds: 7200,
            },
            orphan_timeout_seconds: 600,
            // `None` is the production default: `NoopMetrics`, which emits
            // everything a wired gear emits and lets nothing observe it.
            dispatch_metrics: None,
            ingest_metrics: None,
        },
    ));
    Harness {
        services,
        db,
        executor,
        cancel,
    }
}

/// The events a healthy execution reports: one passing test, then done.
fn passing_script() -> Vec<ExecutionEvent> {
    vec![
        ExecutionEvent::Started,
        ExecutionEvent::TestResult(TestObservation {
            node: "repo-a".to_owned(),
            test_file: "tests/a.py".to_owned(),
            test_name: "test_one".to_owned(),
            status: "PASSED".to_owned(),
            duration: Some("1.20s".to_owned()),
            launch_id: None,
            jira_key: None,
            // File-level; this suite is about the drain, not the payload.
            nodeid: None,
            reason: None,
            ticket: None,
        }),
        ExecutionEvent::Finished {
            outcome: ExecutorOutcome::Succeeded,
            nodes: NodeOutcome::NoneFailed,
            message: None,
        },
    ]
}

/// Put a live run in the database, hand its spec to the executor, and record the
/// reference the executor minted — the state a dispatch leaves behind.
///
/// `timeout_at` is an hour out, which is the load-bearing part of the fixture:
/// the control-plane timeout sweep runs in the same tick, and if it could be
/// what retires these runs the assertions below would say nothing.
async fn a_live_run(h: &Harness, name: &str, environment_id: Option<Uuid>) -> Uuid {
    let conn = h.db.conn().unwrap();
    let mut new = sample_new_run(name);
    new.state = RunState::Running;
    new.environment_id = environment_id;
    new.target = RunTarget::Plan {
        repo_id: Uuid::new_v4(),
        path: "tests/plan.yaml".to_owned(),
    };
    new.timeout_at = Some(time::OffsetDateTime::now_utc() + time::Duration::hours(1));
    let run = OrmRunsRepository
        .create(&conn, &scope(OWNER_TENANT), OWNER_TENANT, new)
        .await
        .unwrap();

    h.executor.script(run.id, passing_script());
    let reference = h
        .executor
        .start(RunSpec {
            run_id: run.id,
            run_name: run.name.clone(),
            nodes: vec![ExecutionNode {
                name: "repo-a".to_owned(),
                bundle_ref: "bundle://a".to_owned(),
                test_files: vec!["tests/a.py".to_owned()],
            }],
            env: RunEnv::default(),
            access: RunAccess::default(),
            runner: RunnerSpec::default(),
            timeout_seconds: 3600,
        })
        .await
        .unwrap();
    OrmRunsRepository
        .set_execution_ref(&conn, &scope(OWNER_TENANT), run.id, reference.as_str())
        .await
        .unwrap();
    run.id
}

/// Poll the run row until it leaves `Running`, or give up.
///
/// A bounded poll rather than a join handle: `RunWatcher::attach` spawns
/// detached on purpose — the observer's lifetime is the run's, not the caller's
/// — so there is no handle to await, and inventing one would be testing a shape
/// this design deliberately does not have.
async fn settled_state(h: &Harness, run_id: Uuid) -> RunState {
    for _ in 0..200 {
        let state = {
            let conn = h.db.conn().unwrap();
            OrmRunsRepository
                .get(&conn, &scope(OWNER_TENANT), run_id)
                .await
                .unwrap()
                .unwrap()
                .state
        };
        if state != RunState::Running {
            return state;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    RunState::Running
}

/// **The gap this task closes.** Before it, `report.timed_out` was the only way
/// a dispatched run ever left `Running`.
///
/// Everything here is the production wiring: the container's own
/// `SpawningRunWatcher`, the real ingest service, the real repositories. What
/// the double supplies is the `Finished` event, which is the input the whole
/// path was missing.
#[tokio::test]
async fn a_run_reaches_a_terminal_state_from_its_finished_event() {
    let h = harness().await;
    let run_id = a_live_run(&h, "platform-bound", Some(PLATFORM_A)).await;

    let report = h.services.dispatch.run_tick().await;
    assert_eq!(report.attached, 1, "the tick started observing the run");
    assert_eq!(
        report.timed_out, 0,
        "and the timeout sweep did nothing, so nothing below can be attributed to \
         the deadline"
    );

    assert_eq!(
        settled_state(&h, run_id).await,
        RunState::Succeeded,
        "the verdict must come from the executor's Finished event"
    );

    // The verdict is only half of it: the per-test row and the counters are
    // Task 15's whole ingest path, which had no production entry point either.
    let conn = h.db.conn().unwrap();
    let owned = OrmRunsRepository
        .resolve_owned(&conn, &scope(OWNER_TENANT), run_id)
        .await
        .unwrap();
    let rows = OrmRunsRepository
        .list_test_results(&conn, &scope(OWNER_TENANT), owned)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].test_name, "test_one");
    assert_eq!(rows[0].status, "PASSED");
    assert_eq!(
        OrmRunsRepository
            .get_result(&conn, &scope(OWNER_TENANT), run_id)
            .await
            .unwrap()
            .unwrap()
            .passed,
        1,
    );
}

/// The same, for a run with **no platform**.
///
/// It is a separate test because it is a separate *source*: an `Unqueued` run
/// has no queue row, so every claim-driven design for this pass would leave it
/// exactly where it started. Deleting the state predicate or swapping the read
/// back to the queue passes the test above and fails this one.
#[tokio::test]
async fn a_platformless_run_reaches_a_terminal_state_from_its_finished_event() {
    let h = harness().await;
    let run_id = a_live_run(&h, "unqueued", None).await;

    assert_eq!(h.services.dispatch.run_tick().await.attached, 1);
    assert_eq!(
        settled_state(&h, run_id).await,
        RunState::Succeeded,
        "a run that was never queued still has to be able to finish"
    );
}

/// **`AppServices::shutdown` reaches the *production* watcher, not a
/// hand-built one.** Review finding #18's wiring half: the mechanism is
/// proven in isolation by
/// `cancelling_the_token_ends_an_observer_that_would_otherwise_never_stop`
/// above, against a future that never resolves on its own; this test instead
/// drives the real `SpawningRunWatcher` this container built - over the real
/// `IngestService`, behind `DispatchService::reattach_watchers` - and cancels
/// the exact token `ServiceDeps::cancel` handed it, the same one a real
/// deployment's `gear.rs` bridges from its own shutdown. `MockRunExecutor`'s
/// drain completes quickly on its own regardless (see the test above for why
/// that rules out proving cancellation *causes* the end here), so what this
/// pins is narrower and just as necessary: the shutdown this container
/// exposes is not a no-op wired to nothing, and it returns promptly rather
/// than hanging.
#[tokio::test]
async fn shutdown_reaches_the_production_watcher_through_app_services() {
    let h = harness().await;
    // Only the attachment matters here, not the run's own outcome - the
    // shutdown wiring is what this test is about.
    let _run_id = a_live_run(&h, "shutdown-me", None).await;

    let report = h.services.dispatch.run_tick().await;
    assert_eq!(report.attached, 1, "premise: the tick attached the run");

    h.cancel.cancel();
    // Bounded rather than an unconditional `.await`: a regression that made
    // `shutdown` loop forever - tracking a new handle on every pass, never
    // converging - must fail this test rather than hang the suite.
    tokio::time::timeout(std::time::Duration::from_secs(5), h.services.shutdown())
        .await
        .expect(
            "shutdown must return once every observer this tick spawned has ended, \
             not hang",
        );
}

/// A `RunExecutor` whose `watch` never ends on its own, unlike
/// `MockRunExecutor`'s. That mock "materialises the whole sequence into the
/// channel before returning" (its own module doc) and then drops the sender,
/// so a real drain against it always completes fast on its own regardless of
/// cancellation — which is exactly why the mechanism-level test above this
/// one is built against a bare `WatchRegistry` and a hand-rolled `select!`
/// instead of driving `attach`. This type exists to close that gap: it hands
/// back an open channel and keeps the sending half alive for as long as the
/// executor itself lives, so `ExecutionStream::recv` blocks forever and the
/// only way `drain` ever returns is the observer's own cancellation.
#[derive(Default)]
struct NeverEndingExecutor {
    // Locked rather than held by value so `watch(&self, ..)` can populate it
    // on first call; kept `Some` forever after, which is what keeps the
    // channel open. `Mutex` rather than the async kind: held only for the
    // instant it takes to assign, never across an `.await`.
    sink: std::sync::Mutex<Option<ExecutionSink>>,
}

#[async_trait]
impl RunExecutor for NeverEndingExecutor {
    async fn start(&self, _spec: RunSpec) -> Result<ExecutionRef, DomainError> {
        unreachable!("this test never dispatches through the executor, only watches")
    }

    async fn watch(
        &self,
        _execution_ref: &ExecutionRef,
        _resume: LogResume,
    ) -> Result<ExecutionStream, DomainError> {
        let (sink, stream) = ExecutionStream::channel(1);
        *self
            .sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sink);
        Ok(stream)
    }

    async fn cancel(&self, _execution_ref: &ExecutionRef) -> Result<(), DomainError> {
        unreachable!("this test never cancels through the executor")
    }

    async fn list_active(&self) -> Result<std::collections::BTreeSet<ExecutionRef>, DomainError> {
        unreachable!("this test never reconciles claims")
    }
}

/// **The falsifying test finding #18 was missing.** Fix round 1, Important 2:
/// the mechanism-level test above this one hand-builds a `tokio::spawn` and
/// `select!` that *mirrors* `SpawningRunWatcher::attach`'s shape rather than
/// driving `attach` itself, and `shutdown_reaches_the_production_watcher_
/// through_app_services` above passes regardless of the cancel arm because
/// `MockRunExecutor`'s drain self-completes. Deleting the cancel arm from
/// `attach` — the entire point of this task — turned neither of them red.
///
/// This one drives the real `SpawningRunWatcher::attach` against
/// [`NeverEndingExecutor`], whose stream never ends on its own, so the only
/// way this observer's `drain` ever returns is if `attach`'s `select!`
/// actually races it against the cancellation token and the token wins.
/// **Break-tested**: with the cancel arm removed from `attach` (replacing the
/// `select!` with a bare `drain(...).await`), this test times out rather than
/// passing — see the fix report for the transcript.
#[tokio::test]
async fn cancelling_a_real_attach_ends_an_observer_blocked_forever_in_watch() {
    let h = harness().await;
    let executor: Arc<dyn RunExecutor> = Arc::new(NeverEndingExecutor::default());
    let cancel = CancellationToken::new();
    let watcher = SpawningRunWatcher::new(executor, Arc::clone(&h.services.ingest), cancel.clone());

    let run_id = Uuid::new_v4();
    watcher.attach(WatchTarget {
        run_id,
        tenant: TenantBound::new(OWNER_TENANT).expect("non-nil"),
        execution_ref: ExecutionRef::new("never-ending"),
    });
    assert!(
        watcher.is_watching(run_id),
        "attach must have claimed and spawned the observer"
    );

    // Let the spawned task actually reach the perpetual read inside `drain`
    // before cancelling - otherwise a `select!` that raced the two futures
    // before either was polled even once would prove nothing about which one
    // actually wins once both are live.
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), watcher.shutdown())
        .await
        .expect(
            "cancelling the token must end an observer that would otherwise never \
             return on its own; if this timed out, attach's select! stopped \
             racing the cancellation against the drain",
        );

    assert!(
        !watcher.is_watching(run_id),
        "the slot must be freed once the observer actually ends"
    );
}

/// **An empty stream is not a failure**, which is the port's contract and the
/// one place this pass could most easily have invented a verdict: the executor
/// has forgotten the execution, the observer sees nothing, and the run must be
/// left exactly as it was for the timeout sweep to own.
///
/// It is also the ablation the two tests above need. If the harness retired runs
/// for some reason other than the `Finished` event, this one would retire too.
#[tokio::test]
async fn an_execution_the_executor_has_forgotten_retires_nothing() {
    let h = harness().await;
    let conn = h.db.conn().unwrap();
    let mut new = sample_new_run("forgotten");
    new.state = RunState::Running;
    new.timeout_at = Some(time::OffsetDateTime::now_utc() + time::Duration::hours(1));
    let run = OrmRunsRepository
        .create(&conn, &scope(OWNER_TENANT), OWNER_TENANT, new)
        .await
        .unwrap();
    // A reference the executor has never heard of: `watch` yields an empty
    // stream, which means "nothing more to say" and never "this failed".
    OrmRunsRepository
        .set_execution_ref(&conn, &scope(OWNER_TENANT), run.id, "never-started")
        .await
        .unwrap();

    assert_eq!(h.services.dispatch.run_tick().await.attached, 1);
    assert_eq!(
        settled_state(&h, run.id).await,
        RunState::Running,
        "an empty stream must not be read as a verdict; the control-plane timeout \
         sweep owns this run"
    );
    // The premise, checked against the executor rather than against itself. An
    // earlier revision asserted `ExecutionRef::new("never-started").as_str() ==
    // "never-started"`, which is `Self(x).0 == x` - it cannot fail and says
    // nothing about what the executor knows. This does: the reference yields an
    // empty stream and appears in no active listing.
    let mut stream = h
        .executor
        .watch(&ExecutionRef::new("never-started"), LogResume::default())
        .await
        .expect("an unknown reference is not an error");
    assert!(
        stream.recv().await.is_none(),
        "premise: the executor really has nothing to say about this reference"
    );
    assert!(
        !h.executor
            .list_active()
            .await
            .unwrap()
            .contains(&ExecutionRef::new("never-started"))
    );
}

/// **The observability half of the `watch`-error arm**, which the break-test
/// round found was the only half that survives a mutation.
///
/// Replacing the error arm's `return` with an empty stream leaves the run
/// untouched either way, so nothing about *state* is pinned there — see
/// `drain`'s own note. What does change is what an operator is told: the mutant
/// loses the ERROR line carrying the cause and reports the observation as having
/// *ended*, for an execution nobody could even reach. `drain`'s
/// `cognitive_complexity` allow-reason calls exactly that load-bearing, so it
/// gets an assertion rather than a paragraph.
///
/// `drain` is called directly rather than through `attach`. `attach` spawns a
/// detached task, so a test driving it would depend both on when that task is
/// scheduled and on whether it is inside the tracing context `tracing_test`
/// captures — neither of which this assertion is about. Calling the function
/// under test is the smaller claim.
#[tokio::test]
#[tracing_test::traced_test]
async fn an_unreachable_executor_is_reported_as_unreachable_not_as_ended() {
    let h = harness().await;
    let run_id = a_live_run(&h, "unreachable-logs", None).await;
    h.executor.fail_watch("executor unreachable");

    super::drain(
        h.executor.as_ref(),
        h.services.ingest.as_ref(),
        WatchTarget {
            run_id,
            tenant: TenantBound::new(OWNER_TENANT).expect("non-nil"),
            execution_ref: ExecutionRef::new("mock-execution-1"),
        },
    )
    .await;

    assert!(
        logs_contain("could not observe an execution"),
        "the cause has to reach the operator, or an outage is indistinguishable \
         from an execution that simply ended"
    );
    assert!(
        !logs_contain("the observation of an execution ended"),
        "and it must not be reported as an ordinary ending"
    );
}

/// **An unreachable executor is not a verdict either**, and it is a *different*
/// arm from the empty stream above: `watch` returning `Err` means nothing is
/// known, while `watch` returning an empty stream means there is nothing more to
/// say. The port forbids conflating them because releasing a claim on an outage
/// is what lets a second run start beside an exclusive one.
///
/// This arm was unreachable from any test until `MockRunExecutor::fail_watch`
/// landed with this task; before that the "retires nothing" sentence on `drain`
/// was prose with no falsifying input.
#[tokio::test]
async fn an_unreachable_executor_retires_nothing() {
    let h = harness().await;
    let run_id = a_live_run(&h, "unreachable", None).await;
    h.executor.fail_watch("executor unreachable");

    assert_eq!(h.services.dispatch.run_tick().await.attached, 1);
    assert_eq!(
        settled_state(&h, run_id).await,
        RunState::Running,
        "an errored watch says nothing about the execution, so the run keeps its \
         state, its claim and its lease"
    );

    // And the run is attachable again once the observer has ended, so a
    // recovered executor is picked up by a later tick rather than never.
    h.executor.clear_failures();
    assert_eq!(
        h.services.dispatch.run_tick().await.attached,
        1,
        "the slot was released when the failed observation ended"
    );
    assert_eq!(settled_state(&h, run_id).await, RunState::Succeeded);
}

// ---------------------------------------------------------------------------
// Finding #50: a re-attach must resume the log read, not replay it
// ---------------------------------------------------------------------------

/// A second harness, over a **real** [`RunLogArchive`] rather than
/// [`NullLogArchive`].
///
/// `harness()` above uses `NullLogArchive` deliberately — none of its tests
/// read the archive, and most of this suite's `AppServices` instances are
/// built over repositories that do not implement `RunLogsRepository`. This
/// one needs the opposite: an archive whose writes are readable back, which
/// is what proves a re-attach did or did not duplicate them. `archive` is
/// kept concretely, the way `ingest_races_pg_tests::Fixture` keeps its own,
/// so a test can force a flush directly rather than waiting on a periodic
/// tick this suite never runs.
struct ResumeHarness {
    services: Arc<ConcreteAppServices>,
    db: Arc<DbProvider>,
    executor: Arc<MockRunExecutor>,
    archive: Arc<RunLogArchive<OrmRunsRepository>>,
}

async fn resume_harness() -> ResumeHarness {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let executor = Arc::new(MockRunExecutor::new());
    let authz: Arc<dyn AuthZResolverApi> = Arc::new(EnumerationGrantingAuthZ {
        covering: vec![OWNER_TENANT],
    });
    let archive = Arc::new(RunLogArchive::new(
        Arc::clone(&db),
        Arc::new(OrmRunsRepository),
        PolicyEnforcer::new(Arc::clone(&authz)),
    ));
    let services = Arc::new(AppServices::new(
        Arc::new(OrmRunsRepository),
        Arc::new(OrmQueueRepository),
        Arc::new(crate::infra::storage::OrmSchedulesRepository),
        ServiceDeps {
            db: Arc::clone(&db),
            authz,
            catalog: Arc::new(FakeCatalog::serving(&["tests/a.py"])),
            environments: Arc::new(FakeEnvironments::free()),
            product_plugins: Arc::new(
                crate::domain::service::admission::tests::fakes::FakeProductPlugins::default(),
            ),
            executor: Arc::clone(&executor) as Arc<dyn RunExecutor>,
            logs: Arc::new(RunLogBroadcaster::new(8)),
            archive: Arc::clone(&archive) as Arc<dyn LogArchive>,
            admitter: None,
            dispatcher: None,
            watcher: None,
            cancel: CancellationToken::new(),
            default_timeout_seconds: 3600,
            limits: QueueLimits {
                queue_max_depth: 20,
                max_concurrent_runs: 0,
                queue_ttl_seconds: 7200,
            },
            orphan_timeout_seconds: 600,
            // `None` is the production default: `NoopMetrics`, which emits
            // everything a wired gear emits and lets nothing observe it.
            dispatch_metrics: None,
            ingest_metrics: None,
        },
    ));
    ResumeHarness {
        services,
        db,
        executor,
        archive,
    }
}

/// Three log lines for node `repo-a`, then **nothing else** — deliberately no
/// `Finished`. A run that finished would leave `Running` and
/// `list_watch_candidates` would stop offering it, which would make a second
/// tick's "is it still unwatched" answer vacuous. A live run whose observer
/// simply ended is exactly Finding #50's trigger: "a process restart, a
/// transient API-server error, an ingest failure" all end the stream with no
/// terminal event, and `reattach_watchers` re-attaches on the next tick
/// regardless of which of the three it was.
fn unfinished_log_script() -> Vec<ExecutionEvent> {
    vec![
        ExecutionEvent::Started,
        ExecutionEvent::Log {
            node: "repo-a".to_owned(),
            line: "one".to_owned(),
        },
        ExecutionEvent::Log {
            node: "repo-a".to_owned(),
            line: "two".to_owned(),
        },
        ExecutionEvent::Log {
            node: "repo-a".to_owned(),
            line: "three".to_owned(),
        },
    ]
}

impl ResumeHarness {
    /// A live, platformless run with an execution reference, scripted with
    /// [`unfinished_log_script`].
    async fn live_run_with_execution_ref(&self) -> Uuid {
        let conn = self.db.conn().unwrap();
        let mut new = sample_new_run("resume-fixture");
        new.state = RunState::Running;
        new.target = RunTarget::Plan {
            repo_id: Uuid::new_v4(),
            path: "tests/plan.yaml".to_owned(),
        };
        new.timeout_at = Some(time::OffsetDateTime::now_utc() + time::Duration::hours(1));
        let run = OrmRunsRepository
            .create(&conn, &scope(OWNER_TENANT), OWNER_TENANT, new)
            .await
            .unwrap();

        self.executor.script(run.id, unfinished_log_script());
        let reference = self
            .executor
            .start(RunSpec {
                run_id: run.id,
                run_name: run.name.clone(),
                nodes: vec![ExecutionNode {
                    name: "repo-a".to_owned(),
                    bundle_ref: "bundle://a".to_owned(),
                    test_files: vec!["tests/a.py".to_owned()],
                }],
                env: RunEnv::default(),
                access: RunAccess::default(),
                runner: RunnerSpec::default(),
                timeout_seconds: 3600,
            })
            .await
            .unwrap();
        OrmRunsRepository
            .set_execution_ref(&conn, &scope(OWNER_TENANT), run.id, reference.as_str())
            .await
            .unwrap();
        run.id
    }

    /// One dispatcher tick — which attaches `run_id` if it is not already
    /// watched — then wait for the spawned observer to finish and settle
    /// into the archive. Returns the tick's `attached` count, so a caller can
    /// assert a re-attach actually happened rather than only that the
    /// archive looks unchanged — see this file's
    /// `a_reattach_does_not_duplicate_the_archived_log`, whose own history
    /// (fix-round 1, Important 4) is that discarding this count let the test
    /// pass vacuously if `reattach_watchers` stopped re-attaching altogether.
    ///
    /// There is no accessor for `is_watching` from outside `domain::service`
    /// (`AppServices::watcher`'s own doc: "no accessor... production surface
    /// existing for a test"), so this polls the one thing observable from
    /// here — the archived line count — until it stops moving, forcing a
    /// flush each time since this script never emits `Finished` and so never
    /// triggers `IngestService::finish`'s own flush.
    async fn attach_and_drain(&self, run_id: Uuid) -> usize {
        let report = self.services.dispatch.run_tick().await;

        let mut last = -1_i64;
        let mut stable_polls = 0;
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            self.archive.flush(run_id).await.ok();
            let current = self.archived_log(run_id).await.map_or(0, |log| log.lines);
            if current == last {
                stable_polls += 1;
                if stable_polls >= 3 {
                    return report.attached;
                }
            } else {
                stable_polls = 0;
            }
            last = current;
        }
        panic!("the observer did not settle within the poll budget");
    }

    async fn archived_log(&self, run_id: Uuid) -> Option<ArchivedLog> {
        let conn = self.db.conn().unwrap();
        OrmRunsRepository
            .get_log(&conn, &scope(OWNER_TENANT), run_id)
            .await
            .unwrap()
    }
}

/// **A re-attach must not append the run's whole log a second time.**
///
/// `reattach_watchers` runs on every tick and re-attaches any live run whose
/// observer ended — a process restart, a transient API-server error, an
/// ingest failure. Before this task the Argo watcher opened each pod log with
/// no `since_time` and no `tail_lines`, so it re-read from byte 0, and
/// `append_log` is a `CONCAT`. `RunLogsRepository` has no truncate, replace or
/// offset, so nothing could undo it. Review finding #50.
///
/// **TDD evidence (recorded in the commit that fixes this):** before the fix,
/// `after_second.lines` was double `after_first.lines` (6 vs 3) — the whole
/// script replayed again rather than resuming past it.
#[tokio::test]
async fn a_reattach_does_not_duplicate_the_archived_log() {
    let h = resume_harness().await;
    let run_id = h.live_run_with_execution_ref().await;

    let attached_first = h.attach_and_drain(run_id).await;
    assert_eq!(
        attached_first, 1,
        "the first tick must have attached the run"
    );
    let after_first = h
        .archived_log(run_id)
        .await
        .expect("the first observation must have archived something");
    assert!(after_first.lines > 0, "premise: something was archived");

    // The observer ended (the script has no `Finished`); the slot is freed
    // by `AttachedSlot`'s Drop, and the next tick's `reattach_watchers`
    // re-attaches because the run is still `Running` and unwatched.
    //
    // Asserted explicitly (fix-round 1, Important 4): without this, a
    // regression that stopped `reattach_watchers` from re-attaching at all
    // would leave `after_second == after_first` trivially — nothing would
    // have been read a second time for there to be anything to duplicate —
    // and this test would stay green while pinning nothing.
    let attached_second = h.attach_and_drain(run_id).await;
    assert_eq!(
        attached_second, 1,
        "the second tick must have re-attached, or nothing below is evidence of anything"
    );
    let after_second = h.archived_log(run_id).await.expect("the row still exists");

    assert_eq!(
        after_second.lines, after_first.lines,
        "a re-attach must not re-append the log; it grew from {} to {} lines",
        after_first.lines, after_second.lines
    );
    assert_eq!(
        after_second.text, after_first.text,
        "a re-attach must not change the archived text"
    );
}
