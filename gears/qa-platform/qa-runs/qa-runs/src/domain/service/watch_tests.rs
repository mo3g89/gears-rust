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

use super::{AttachedSlot, WatchRegistry};
use uuid::Uuid;

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

// ---------------------------------------------------------------------------
// The composition: a run that ends because it finished, not because it expired
// ---------------------------------------------------------------------------

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, PolicyEnforcer};
use qa_runs_sdk::{RunState, RunTarget};
use toolkit_security::pep_properties;

use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::{
    ExecutionEvent, ExecutionNode, ExecutionRef, NodeOutcome, RunAccess, RunEnv, RunExecutor,
    RunSpec, RunnerSpec, TestObservation,
};
use crate::domain::repos::{ArchivedLog, LogResume, RunLogsRepository, RunsRepository};
use crate::domain::service::admission::tests::fakes::{FakeCatalog, FakeEnvironments, PLATFORM_A};
use crate::domain::service::test_support::{NullLogArchive, OWNER_TENANT};
use crate::domain::service::watch::WatchTarget;
use crate::domain::service::{AppServices, DbProvider, LogArchive, QueueLimits, ServiceDeps};
use crate::domain::state_machine::ExecutorOutcome;
use crate::domain::system_actor::TenantBound;
use crate::infra::ConcreteAppServices;
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
impl AuthZResolverClient for EnumerationGrantingAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
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
}

async fn harness() -> Harness {
    let db = Arc::new(DBProvider::<DomainError>::new(inmem_db().await));
    let executor = Arc::new(MockRunExecutor::new());
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
            default_timeout_seconds: 3600,
            limits: QueueLimits {
                queue_max_depth: 20,
                max_concurrent_runs: 0,
                queue_ttl_seconds: 7200,
            },
            orphan_timeout_seconds: 600,
        },
    ));
    Harness {
        services,
        db,
        executor,
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
async fn a_live_run(h: &Harness, name: &str, platform_id: Option<Uuid>) -> Uuid {
    let conn = h.db.conn().unwrap();
    let mut new = sample_new_run(name);
    new.state = RunState::Running;
    new.platform_id = platform_id;
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
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(EnumerationGrantingAuthZ {
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
            default_timeout_seconds: 3600,
            limits: QueueLimits {
                queue_max_depth: 20,
                max_concurrent_runs: 0,
                queue_ttl_seconds: 7200,
            },
            orphan_timeout_seconds: 600,
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
    /// into the archive.
    ///
    /// There is no accessor for `is_watching` from outside `domain::service`
    /// (`AppServices::watcher`'s own doc: "no accessor... production surface
    /// existing for a test"), so this polls the one thing observable from
    /// here — the archived line count — until it stops moving, forcing a
    /// flush each time since this script never emits `Finished` and so never
    /// triggers `IngestService::finish`'s own flush.
    async fn attach_and_drain(&self, run_id: Uuid) {
        self.services.dispatch.run_tick().await;

        let mut last = -1_i64;
        let mut stable_polls = 0;
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            self.archive.flush(run_id).await.ok();
            let current = self.archived_log(run_id).await.map_or(0, |log| log.lines);
            if current == last {
                stable_polls += 1;
                if stable_polls >= 3 {
                    return;
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

    h.attach_and_drain(run_id).await;
    let after_first = h
        .archived_log(run_id)
        .await
        .expect("the first observation must have archived something");
    assert!(after_first.lines > 0, "premise: something was archived");

    // The observer ended (the script has no `Finished`); the slot is freed
    // by `AttachedSlot`'s Drop, and the next tick's `reattach_watchers`
    // re-attaches because the run is still `Running` and unwatched.
    h.attach_and_drain(run_id).await;
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
