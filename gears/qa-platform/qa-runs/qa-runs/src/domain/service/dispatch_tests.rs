//! Unit tests for the dispatcher: submission, the tick's ordering, crash
//! recovery, and the tenancy of background writes.
//!
//! The doubles are `service::admission::tests::fakes` — see that module's header
//! for why they live there and for the tenant-scoping rule every one of them
//! applies.
//!
//! Every expected value for ported logic comes from **reading the source
//! system**, never from running this code and recording what it did. Each
//! concurrency test names, in its doc comment, what breaks when its fix is
//! removed; every one of those was verified by removing the fix and watching the
//! test fail.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use qa_runs_sdk::{QueueState, RunState};
use time::Duration as TimeDuration;
use uuid::Uuid;

use super::*;
use crate::domain::ports::run_executor::MountSpec;
use crate::domain::service::admission::tests::fakes::{
    self, Builder, FakeCatalog, FakeEnvironments, FakeQueue, FakeRuns, PLATFORM_A, PLATFORM_B,
    RecordingAuthZ, queued_row, row_aged, run_fixture,
};
use crate::domain::service::test_support::{OTHER_TENANT, OWNER_TENANT, ctx};

/// Ids, so a failure names something.
const RUN_1: Uuid = Uuid::from_u128(0x1001);
const RUN_2: Uuid = Uuid::from_u128(0x1002);
const RUN_3: Uuid = Uuid::from_u128(0x1003);
const RUN_4: Uuid = Uuid::from_u128(0x1004);
/// Sorts **below** every other row id, so only a wrapped scan cursor sees it.
const ROW_0: Uuid = Uuid::from_u128(0x2000);
const ROW_1: Uuid = Uuid::from_u128(0x2001);
const ROW_2: Uuid = Uuid::from_u128(0x2002);
const ROW_3: Uuid = Uuid::from_u128(0x2003);

// ---------------------------------------------------------------------------
// dispatch_one
// ---------------------------------------------------------------------------

/// The happy path, in the order parity spec §3.4 rules 4-6 require:
/// **force-sync, then bundle, then start** — and the sync is forced, because the
/// source system drops the recency marker before syncing so a cached checkout
/// cannot be returned (`SyncRequest::force`'s own doc;
/// `manager/src/services/test_repos.rs:503-508`).
#[tokio::test]
async fn dispatch_one_force_syncs_then_bundles_then_starts() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let row = queued_row(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        false,
        QueueState::Dispatching,
    );
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .queue(Arc::new(FakeQueue::with(vec![row])))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, Some(ROW_1))
        .await
        .expect("the submit succeeds");

    let syncs = fakes.catalog.syncs();
    assert_eq!(syncs.len(), 1, "one sync per repository group");
    assert!(syncs[0].1.force, "a dispatch always force-syncs");
    assert_eq!(syncs[0].1.branch.as_deref(), Some("main"));
    assert_eq!(fakes.catalog.bundle_builds(), 1);

    let submitted = executor.submitted();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].nodes.len(), 1, "one node per repository group");
    assert_eq!(
        submitted[0].nodes[0].test_files,
        vec!["tests/a.py".to_owned()]
    );
    assert_eq!(submitted[0].nodes[0].bundle_ref, "bundle://one");

    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Running));
    assert_eq!(fakes.runs.state_of(RUN_1), Some(RunState::Running));
    assert_eq!(
        fakes.runs.bundle_writes.lock().unwrap().len(),
        1,
        "rule 5's bundle reference is recorded on the run"
    );
}

/// The branch comes from `Run::test_version` and is never re-resolved: the sync
/// must fetch the ref the run recorded, not whatever the platform or repository
/// default says now. See `launch::resolve_branch`, which is private for exactly
/// this reason.
#[tokio::test]
async fn the_sync_uses_the_branch_the_run_recorded() {
    let mut run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    run.test_version = Some("release/5.0".to_owned());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    assert_eq!(
        fakes.catalog.syncs()[0].1.branch.as_deref(),
        Some("release/5.0")
    );
}

/// A run whose `test_version` is NULL is a row this gear did not write. Fail
/// rather than invent a branch.
#[tokio::test]
async fn a_run_without_a_recorded_branch_is_corrupt_not_defaulted() {
    let mut run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    run.test_version = None;
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .build()
        .await;

    let error = fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .expect_err("a missing branch is not defaulted");
    assert!(matches!(
        error,
        DomainError::CorruptState {
            what: "run.test_version",
            ..
        }
    ));
    assert!(fakes.catalog.syncs().is_empty(), "nothing was fetched");
}

/// The kubeconfig composition no type can enforce: the mount's path and the
/// `KUBECONFIG` variable are produced on two tiers and must agree. Here they
/// come from one constant, and this is what "must" looks like when it holds in
/// production code rather than in a hand-built spec.
#[tokio::test]
async fn the_kubeconfig_mount_and_the_assembled_variable_come_from_one_value() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    let spec = &executor.submitted()[0];
    let [
        MountSpec::Secret {
            credstore_ref,
            path,
            mode,
        },
    ] = spec.access.mounts.as_slice()
    else {
        panic!("a platform run mounts exactly one secret")
    };
    assert_eq!(
        fakes::env_values(spec)
            .get("KUBECONFIG")
            .map(String::as_str),
        Some(path.as_str()),
        "the executor would otherwise resolve the kubeconfig to a path the run's \
         KUBECONFIG does not name"
    );
    assert_eq!(credstore_ref, "credstore://kubeconfig");
    assert_eq!(
        *mode, None,
        "this task changes no value: the source system's kubeconfig volume sets \
         no mode (`argo.rs:506-511`)"
    );
}

/// A run with no platform mounts nothing, exactly as the source system mounts
/// nothing in that case (`manager/src/services/argo.rs:504`).
#[tokio::test]
async fn a_platformless_run_carries_no_kubeconfig() {
    let run = run_fixture(RUN_1, None, false, RunState::Dispatching);
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    let spec = &executor.submitted()[0];
    assert!(spec.access.mounts.is_empty());
    assert!(
        spec.access.service_account.is_none(),
        "and no service account, which is meaningful now that the double asks \
         for one: a platformless run resolves no plugin, so it inherits nothing \
         from any product"
    );
    assert_eq!(
        spec.runner,
        crate::domain::ports::run_executor::RunnerSpec::default(),
        "and the deployment-wide runner, for the same reason"
    );
    assert!(!fakes::env_values(spec).contains_key("KUBECONFIG"));
}

/// `runvars`'s composition obligation 2: `APP_VERSION` and `APP_BUILD` come
/// from the run's **own snapshotted columns**, never a live platform lookup — a
/// platform upgrade must not silently change a queued run's version. The fixture
/// platform reports `7.1` and the run records `7.1`, so the assertion that
/// discriminates is `APP_BUILD`: the run has none and the platform has none, and
/// a blank one is skipped rather than exported empty (`argo.rs:461-469`).
#[tokio::test]
async fn the_environment_takes_the_runs_snapshotted_versions() {
    let mut run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    run.app_version = Some("6.0".to_owned());
    run.app_build = Some("   ".to_owned());
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    let env = fakes::env_values(&executor.submitted()[0]);
    assert_eq!(
        env.get("APP_VERSION").map(String::as_str),
        Some("6.0"),
        "the run's column, not the platform's 7.1"
    );
    assert!(
        !env.contains_key("APP_BUILD"),
        "a blank static is skipped, not exported empty"
    );
    assert_eq!(env.get("TEST_VERSION").map(String::as_str), Some("main"));
}

/// `TEST_FILES` and `TEST_BUNDLE_URL` belong to the **node**, not the shared
/// environment: the source system has one of each per node
/// (`manager/src/services/argo.rs:1220-1248`) and `ExecutionNode` carries both,
/// so a copy in the shared map would disagree with the node's whenever there is
/// more than one group.
#[tokio::test]
async fn the_shared_environment_carries_no_per_node_values() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    let env = fakes::env_values(&executor.submitted()[0]);
    assert!(!env.contains_key("TEST_FILES"));
    assert!(!env.contains_key("TEST_BUNDLE_URL"));
}

/// The executor deadline is the time **left** on the run's recorded `timeout_at`,
/// not a fresh full budget: the deadline is absolute and was decided at launch,
/// so a run that waited in the queue gets what remains.
#[tokio::test]
async fn the_executor_deadline_is_what_is_left_of_the_recorded_one() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let runs = Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)]));
    runs.seed_deadline(
        RUN_1,
        OffsetDateTime::now_utc() + TimeDuration::seconds(120),
    );
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(runs)
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    let seconds = executor.submitted()[0].timeout_seconds;
    assert!(
        (110..=120).contains(&seconds),
        "expected roughly the 120 s that remain, got {seconds}"
    );
}

/// A run with no deadline gets `0`, which is this gear's `0`-is-disabled
/// convention — and a run already **past** its deadline gets `1`, because `0`
/// would silently promote an overdue run to an unbounded one.
#[tokio::test]
async fn a_run_with_no_deadline_gets_zero_and_an_overdue_one_gets_one() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let runs = Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)]));
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::clone(&runs))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();
    assert_eq!(executor.submitted()[0].timeout_seconds, 0);

    let overdue = run_fixture(RUN_2, Some(PLATFORM_A), false, RunState::Dispatching);
    let runs = Arc::new(FakeRuns::with(vec![(OWNER_TENANT, overdue)]));
    runs.seed_deadline(RUN_2, OffsetDateTime::now_utc() - TimeDuration::seconds(60));
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(runs)
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;
    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_2, None)
        .await
        .unwrap();
    assert_eq!(executor.submitted()[0].timeout_seconds, 1);
}

/// Decision D4's bounded retry: a bundle build that fails immediately after this
/// dispatch's own force-sync is retried **once**, which closes the window
/// qa-catalog's clear-then-rewrite snapshot widened past legacy's in-place
/// worktree update.
#[tokio::test]
async fn a_bundle_build_that_fails_after_the_force_sync_is_retried_once() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let catalog = Arc::new(FakeCatalog::serving(&["tests/a.py"]));
    *catalog.fail_bundle_times.lock().unwrap() = 1;
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .catalog(Arc::clone(&catalog))
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .expect("the retry succeeds");

    assert_eq!(catalog.bundle_builds(), 2, "one failure plus one retry");
    assert_eq!(
        fakes.catalog.syncs().len(),
        1,
        "the retry re-issues the bundle build, not the sync: a second sync would \
         re-open the same window rather than wait out the first"
    );
}

/// **And only once.** A loop would turn a genuinely unbuildable group — a deleted
/// path, a branch without the files — into a hang instead of a failed run, which
/// decision D4 rules out explicitly.
#[tokio::test]
async fn a_bundle_build_is_retried_only_once() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let catalog = Arc::new(FakeCatalog::serving(&["tests/a.py"]));
    *catalog.fail_bundle_times.lock().unwrap() = 5;
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .catalog(Arc::clone(&catalog))
        .build()
        .await;

    let error = fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .expect_err("an unbuildable group fails the run");
    assert!(matches!(error, DomainError::Catalog(_)));
    assert_eq!(
        catalog.bundle_builds(),
        2,
        "exactly two attempts, never more"
    );
}

/// A tag filter is applied at dispatch, from the catalog, and only for a plan
/// run (`manager/src/routes/runs.rs:686-700`).
#[tokio::test]
async fn a_tag_filter_selects_the_files_a_plan_run_executes() {
    let mut run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    run.exclude_tags = vec!["destructive".to_owned()];
    let catalog = Arc::new(FakeCatalog::serving(&["tests/a.py", "tests/b.py"]));
    catalog.metas.lock().unwrap().extend([
        qa_catalog_sdk::TestFileMeta {
            path: "tests/a.py".to_owned(),
            title: None,
            tags: vec!["smoke".to_owned()],
            exclusive: qa_catalog_sdk::Exclusivity::Inherit,
            bugs: Vec::new(),
        },
        qa_catalog_sdk::TestFileMeta {
            path: "tests/b.py".to_owned(),
            title: None,
            tags: vec!["destructive".to_owned()],
            exclusive: qa_catalog_sdk::Exclusivity::Inherit,
            bugs: Vec::new(),
        },
    ]);
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .catalog(catalog)
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    assert_eq!(
        executor.submitted()[0].nodes[0].test_files,
        vec!["tests/a.py".to_owned()],
        "the excluded file must not reach the runner"
    );
}

/// **A tag read that fails fails the dispatch.** This is the one place where
/// failing open is the dangerous direction: an `exclude_tags` filter that
/// silently did not apply would run exactly the tests the caller asked to skip.
#[tokio::test]
async fn an_unreadable_tag_source_refuses_rather_than_running_everything() {
    let mut run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    run.exclude_tags = vec!["destructive".to_owned()];
    let catalog = Arc::new(FakeCatalog::serving(&["tests/a.py", "tests/b.py"]));
    *catalog.fail_meta.lock().unwrap() = true;
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .catalog(catalog)
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    let error = fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .expect_err("an unreadable tag source must not run everything");
    assert!(matches!(error, DomainError::Catalog(_)));
    assert!(
        executor.submitted().is_empty(),
        "nothing was submitted, so no excluded test ran"
    );
}

/// A run whose plan has no runnable files fails rather than submitting a spec
/// with no nodes — which would execute nothing and report success. Legacy answers
/// `BAD_REQUEST` for the same input (`manager/src/routes/runs.rs:678-686`).
#[tokio::test]
async fn a_run_with_no_runnable_files_fails_rather_than_executing_nothing() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .catalog(Arc::new(FakeCatalog::serving(&[])))
        .build()
        .await;

    let error = fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .expect_err("an empty plan fails");
    assert!(matches!(error, DomainError::Validation { .. }));
    assert!(fakes.catalog.syncs().is_empty(), "no group to sync");
}

/// **A failed submit must release the claim.** Legacy marks the row `failed`
/// straight from `dispatching` (`manager/src/services/run_dispatcher.rs:47-58`);
/// here the run must also be retired and the platform lease handed back, or the
/// platform reads busy forever.
///
/// Removing the `mark_failed` call leaves the row `dispatching`, which blocks the
/// platform's queue for `orphan_timeout_seconds`; removing the lease release
/// leaves the platform occupied indefinitely. Both were verified by removal.
#[tokio::test]
async fn a_failed_submit_releases_the_claim_and_fails_the_run() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Dispatching);
    let row = queued_row(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Dispatching,
    );
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    executor.fail_start("the execution plane refused the submit");
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .queue(Arc::new(FakeQueue::with(vec![row])))
        .executor(executor)
        .build()
        .await;

    let error = fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, Some(ROW_1))
        .await
        .expect_err("the submit failed");
    assert!(matches!(error, DomainError::ExecutorFailed(_)));

    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Failed));
    assert_eq!(
        fakes.runs.state_of(RUN_1),
        Some(RunState::Error),
        "a submit failure is a control-plane fault, not a test verdict"
    );
    assert_eq!(
        fakes.environments.released(),
        vec![(PLATFORM_A, RUN_1)],
        "the platform must be handed back"
    );
}

/// The text written to `qa_runs.error` is redacted for a cause whose text
/// originates outside this gear: that column is served verbatim by
/// `GET /runs/{id}`. `DomainError::recorded_text` is the single rule
/// (`domain::error`), and this pins that dispatch uses it.
#[tokio::test]
async fn a_failed_submit_records_no_foreign_error_text() {
    let leak = "duplicate key value violates unique constraint \"idx_qa_run_queue_tenant_run\"";
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    executor.fail_start(leak);
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .executor(executor)
        .build()
        .await;

    let error = fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .expect_err("the submit failed");
    assert!(
        error.to_string().contains(leak),
        "the caller still receives the real cause"
    );
    let recorded = fakes
        .runs
        .error_of(RUN_1)
        .expect("the run records something");
    assert!(
        !recorded.contains("idx_qa_run_queue_tenant_run"),
        "raw driver text reached qa_runs.error: {recorded}"
    );
}

/// **The inverse case returns `Ok`.** If the submit succeeded but recording it
/// failed, returning `Err` would be a lie and could trigger a caller retry that
/// double-submits, so the failure is logged and `Ok` returned — accepting a stale
/// claim the next tick's reconciliation releases
/// (`manager/src/services/run_dispatcher.rs:32-44`).
///
/// Removing the swallow (propagating the error instead) makes this test fail on
/// the `Ok` assertion **and** leaves the caller free to retry a run that is
/// already executing. Verified by removal.
#[tokio::test]
async fn a_submit_that_succeeded_but_could_not_be_recorded_returns_ok() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let row = queued_row(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        false,
        QueueState::Dispatching,
    );
    let runs = Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)]));
    *runs.fail_execution_ref.lock().unwrap() = true;
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::clone(&runs))
        .queue(Arc::new(FakeQueue::with(vec![row])))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, Some(ROW_1))
        .await
        .expect("a started run must never be reported as failed");

    assert_eq!(
        executor.submitted().len(),
        1,
        "the execution really did start"
    );
    assert_eq!(
        runs.state_of(RUN_1),
        Some(RunState::Running),
        "the run is still recorded as running, so ingest can find it"
    );
    assert!(
        runs.execution_ref_of(RUN_1).is_none(),
        "the reference is genuinely missing, which is what leaves a claim for \
         reconciliation"
    );
}

/// The tick's caller arrives with the run in `queued`, or in `created` — the
/// state `launch::settle`'s unrepresentable-outcome arm leaves behind and
/// explicitly expects this tick to recover from. Both are legal predecessors of
/// `dispatching`.
#[tokio::test]
async fn dispatch_one_moves_a_queued_or_created_run_to_dispatching_first() {
    for state in [RunState::Queued, RunState::Created] {
        let run = run_fixture(RUN_1, Some(PLATFORM_A), false, state);
        let runs = Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)]));
        let fakes = Builder::new().runs(Arc::clone(&runs)).build().await;

        fakes
            .dispatch
            .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
            .await
            .unwrap();

        let transitions = runs.transitions.lock().unwrap().clone();
        assert_eq!(
            transitions.first().map(|(_, from, to)| (*from, *to)),
            Some((state, RunState::Dispatching)),
            "the claim state must be recorded before any expensive work"
        );
    }
}

// ---------------------------------------------------------------------------
// Tick ordering
// ---------------------------------------------------------------------------

/// **The TTL sweep runs before the cap check.** Both early returns fire in
/// exactly the situations where queued rows pile up unnoticed, so the sweep must
/// not be downstream of either (`manager/src/services/run_dispatcher.rs:344-351`,
/// `:495-508`).
///
/// Moving `ttl_sweep` below the cap check makes `expired` zero here. Verified by
/// moving it.
#[tokio::test]
async fn the_ttl_sweep_runs_before_the_cap_check() {
    // One claim, uncommitted, against a cap of one: the tick stops at the cap.
    let claim = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        false,
        QueueState::Dispatching,
        10,
    );
    // A queued row on another platform, long past its TTL.
    let stale = row_aged(
        ROW_2,
        OWNER_TENANT,
        RUN_2,
        PLATFORM_B,
        false,
        QueueState::Queued,
        9_000,
    );
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![
            (
                OWNER_TENANT,
                run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching),
            ),
            (
                OWNER_TENANT,
                run_fixture(RUN_2, Some(PLATFORM_B), false, RunState::Queued),
            ),
        ])))
        .queue(Arc::new(FakeQueue::with(vec![claim, stale])))
        .limits(QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 1,
            queue_ttl_seconds: 7200,
        })
        .orphan_timeout(600)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(
        report.expired, 1,
        "the sweep ran before the cap stopped the tick"
    );
    assert_eq!(report.claimed, 0, "the cap really was reached");
    assert_eq!(fakes.queue.state_of(ROW_2), Some(QueueState::Expired));
    assert_eq!(
        fakes.runs.state_of(RUN_2),
        Some(RunState::Expired),
        "the run row must stay readable as Expired, or QueueExpired's field list \
         is wrong"
    );
}

/// **The TTL sweep runs even when the executor listing fails** — the case that
/// needs it most, because unreadable occupancy deliberately reads as busy so
/// every launch queues and nothing drains (`run_dispatcher.rs:344-351`).
///
/// Moving `ttl_sweep` below the listing makes `expired` zero here. Verified by
/// moving it.
#[tokio::test]
async fn the_ttl_sweep_runs_even_when_the_executor_listing_fails() {
    let stale = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        false,
        QueueState::Queued,
        9_000,
    );
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    executor.fail_list_active("the executor is unreachable");
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Queued),
        )])))
        .queue(Arc::new(FakeQueue::with(vec![stale])))
        .executor(executor)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.expired, 1);
    assert_eq!(report.stopped_at, Some("executor listing"));
    assert_eq!(
        fakes.runs.state_of(RUN_1),
        Some(RunState::Expired),
        "the mandatory expiry alert's WARN fires regardless; this pins that the \
         run row backing it was actually reconciled"
    );
}

/// **A failed executor listing skips the whole tick, and releases no claim.** An
/// empty answer would release every claim at once
/// (`run_dispatcher.rs:353-359`), so the error is not read as "nothing is
/// running".
///
/// Treating the error as an empty set makes this test fail on both assertions —
/// the claim is released and the platform freed while its execution may still be
/// alive. Verified by substituting `unwrap_or_default()`.
#[tokio::test]
async fn a_failed_executor_listing_skips_the_whole_tick() {
    let claim = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Running,
        10,
    );
    let runs = Arc::new(FakeRuns::with(vec![(
        OWNER_TENANT,
        run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Running),
    )]));
    runs.seed_execution_ref(RUN_1, "mock-execution-1", RunState::Running);
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    executor.fail_list_active("the executor is unreachable");
    let fakes = Builder::new()
        .runs(runs)
        .queue(Arc::new(FakeQueue::with(vec![claim])))
        .executor(executor)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(
        report.released, 0,
        "no claim may be released on no information"
    );
    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Running));
    assert!(
        fakes.environments.released().is_empty(),
        "the platform stays held, which is what stops a run starting beside an \
         exclusive one"
    );
}

/// **Claims are counted after reconciliation, not before**
/// (`run_dispatcher.rs:376-377`). A claim whose execution has gone is released by
/// reconciliation and must not then be charged against the cap — otherwise a
/// cluster of finished runs permanently blocks the queue.
///
/// Evaluating the cap before reconciliation makes `claimed` zero here. Verified
/// by moving the cap evaluation above `reconcile_claims`.
#[tokio::test]
async fn claims_are_counted_after_reconciliation_not_before() {
    // A `running` claim whose execution the executor no longer lists.
    let finished = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        false,
        QueueState::Running,
        60,
    );
    // A queued row that can only start if the cap has room.
    let waiting = queued_row(
        ROW_2,
        OWNER_TENANT,
        RUN_2,
        PLATFORM_A,
        false,
        QueueState::Queued,
    );
    let runs = Arc::new(FakeRuns::with(vec![
        (
            OWNER_TENANT,
            run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Running),
        ),
        (
            OWNER_TENANT,
            run_fixture(RUN_2, Some(PLATFORM_A), false, RunState::Queued),
        ),
    ]));
    runs.seed_execution_ref(RUN_1, "vanished-execution", RunState::Running);
    let fakes = Builder::new()
        .runs(runs)
        .queue(Arc::new(FakeQueue::with(vec![finished, waiting])))
        .limits(QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 1,
            queue_ttl_seconds: 7200,
        })
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.released, 1, "the gone execution's claim is released");
    assert_eq!(
        report.claimed, 1,
        "the released claim must not be charged against the cap"
    );
}

/// **The global budget is threaded across platforms.** Two platforms, cap 6,
/// four live executions: two rows total, not two each — the overshoot legacy
/// warns about is `(platforms - 1) x (max - active)`
/// (`run_dispatcher.rs:406-414`).
///
/// Passing the un-threaded `cap` to every platform makes `claimed` four here.
/// Verified by removing the `with_claimed` call.
#[tokio::test]
async fn the_global_budget_is_threaded_across_platforms() {
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    for n in 0..4u128 {
        let spec = crate::domain::ports::run_executor::RunSpec {
            run_id: Uuid::from_u128(0x9000 + n),
            run_name: format!("other-{n}"),
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
    }

    let mut rows = Vec::new();
    let mut runs = Vec::new();
    for (index, platform) in [PLATFORM_A, PLATFORM_B].into_iter().enumerate() {
        for slot in 0..3u128 {
            let run_id = Uuid::from_u128(0x3000 + (index as u128) * 16 + slot);
            let row_id = Uuid::from_u128(0x4000 + (index as u128) * 16 + slot);
            rows.push(row_aged(
                row_id,
                OWNER_TENANT,
                run_id,
                platform,
                false,
                QueueState::Queued,
                i64::try_from(slot).unwrap(),
            ));
            runs.push((
                OWNER_TENANT,
                run_fixture(run_id, Some(platform), false, RunState::Queued),
            ));
        }
    }

    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(runs)))
        .queue(Arc::new(FakeQueue::with(rows)))
        .executor(executor)
        .limits(QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 6,
            queue_ttl_seconds: 0,
        })
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(
        report.claimed, 2,
        "4 active against a cap of 6 leaves 2 for the whole tick, not 2 per platform"
    );
}

/// A queue of parallel rows drains in one tick — all of them start together
/// (guide lines 102-104).
#[tokio::test]
async fn a_parallel_queue_drains_in_one_tick() {
    let fakes = drain_fixture(&[
        (ROW_1, RUN_1, false),
        (ROW_2, RUN_2, false),
        (ROW_3, RUN_3, false),
    ])
    .await;
    let report = fakes.dispatch.run_tick().await;
    assert_eq!(report.claimed, 3);
    for row in [ROW_1, ROW_2, ROW_3] {
        assert_eq!(fakes.queue.state_of(row), Some(QueueState::Running));
    }
}

/// A queue of exclusive rows drains **one per tick**, because an exclusive row is
/// claimed alone (guide lines 102-104;
/// `domain::queue::plan_dispatch_batch` folds the claim into occupancy so the
/// next iteration breaks).
#[tokio::test]
async fn an_exclusive_queued_row_drains_one_per_tick() {
    let fakes = drain_fixture(&[
        (ROW_1, RUN_1, true),
        (ROW_2, RUN_2, true),
        (ROW_3, RUN_3, true),
    ])
    .await;
    let report = fakes.dispatch.run_tick().await;
    assert_eq!(report.claimed, 1, "an exclusive row is claimed alone");
    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Running));
    assert_eq!(fakes.queue.state_of(ROW_2), Some(QueueState::Queued));
}

/// One platform, N queued rows oldest-first, everything else healthy.
async fn drain_fixture(rows: &[(Uuid, Uuid, bool)]) -> fakes::Fakes {
    let mut queue_rows = Vec::new();
    let mut run_rows = Vec::new();
    for (index, (row_id, run_id, exclusive)) in rows.iter().enumerate() {
        queue_rows.push(row_aged(
            *row_id,
            OWNER_TENANT,
            *run_id,
            PLATFORM_A,
            *exclusive,
            QueueState::Queued,
            i64::try_from(rows.len() - index).unwrap(),
        ));
        run_rows.push((
            OWNER_TENANT,
            run_fixture(*run_id, Some(PLATFORM_A), *exclusive, RunState::Queued),
        ));
    }
    Builder::new()
        .runs(Arc::new(FakeRuns::with(run_rows)))
        .queue(Arc::new(FakeQueue::with(queue_rows)))
        .limits(QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 0,
            queue_ttl_seconds: 0,
        })
        .build()
        .await
}

// ---------------------------------------------------------------------------
// Crash recovery
// ---------------------------------------------------------------------------

/// Boot recovery fails a row left mid-dispatch **once it has outlived the
/// orphan timeout**, releasing its lease so the platform frees.
///
/// **The age term was added 2026-08-15** and this test carried the inverted
/// claim - "whatever their age", with a one-second-old row. Legacy really does
/// have no age predicate, on the stated premise that a single replica's
/// interrupted submit is definitively gone; nothing enforces a single replica
/// here, so without the gate a booting replica fails another replica's live
/// build. `domain::state_machine::boot_recovery_action` carries the argument
/// and `boot_recovery_spares_a_dispatch_that_may_still_be_in_flight` pins the
/// other side.
#[tokio::test]
async fn boot_recovery_fails_rows_left_mid_dispatch() {
    let claim = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Dispatching,
        9_000,
    );
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Dispatching),
        )])))
        .queue(Arc::new(FakeQueue::with(vec![claim])))
        .build()
        .await;

    let report = fakes.dispatch.recover_after_boot().await;

    assert_eq!(report.failed_orphans, 1);
    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Failed));
    assert_eq!(fakes.runs.state_of(RUN_1), Some(RunState::Error));
    assert_eq!(
        fakes.environments.released(),
        vec![(PLATFORM_A, RUN_1)],
        "a boot that failed the rows and left the leases would leave every affected \
         platform reading busy forever"
    );
}

/// **A booting replica does not sabotage another replica's live dispatch.**
///
/// The scenario, which is reachable at shipped defaults because nothing
/// enforces a single replica: replica A is inside `dispatch_one` for a run -
/// `ensure_dispatching` has already moved the run and its queue row, and A is
/// in force-sync plus bundle build for minutes, so `execution_ref` is still
/// NULL. Replica B starts and runs boot recovery before its first tick.
///
/// Without the age gate B failed the queue row, moved the run to `Error` and
/// released the lease - and then A's submit *succeeded*, leaving a live
/// execution with no claim, no lease, and a run the operator reads as failed.
/// Nothing reconciles that afterwards: reconciliation matches claims to
/// executions and the claim is gone.
///
/// A young claim is exactly what that looks like from B's side, so the
/// assertion is that a young one is left entirely alone.
#[tokio::test]
async fn boot_recovery_leaves_a_young_claim_that_may_be_another_replicas_build() {
    let mid_dispatch = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Dispatching,
        // Younger than the 600 s orphan timeout: a plausible force-sync plus
        // bundle build still in progress on another replica.
        30,
    );
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Dispatching),
        )])))
        .queue(Arc::new(FakeQueue::with(vec![mid_dispatch])))
        .orphan_timeout(600)
        .build()
        .await;

    let report = fakes.dispatch.recover_after_boot().await;

    assert_eq!(report.failed_orphans, 0);
    assert_eq!(
        fakes.queue.state_of(ROW_1),
        Some(QueueState::Dispatching),
        "the claim must survive, or the other replica's submit lands on a run \
         this one has already failed"
    );
    assert_eq!(fakes.runs.state_of(RUN_1), Some(RunState::Dispatching));
    assert!(
        fakes.environments.released().is_empty(),
        "and its platform must stay held, or the next launch double-books it"
    );
}

/// **Boot recovery reads to exhaustion**, unlike the tick's windowed scans.
///
/// A row it misses holds a platform's lease and leaves its run stuck in
/// `dispatching` until a later tick's rotation happens to reach it, which is
/// what the boot pass exists to prevent - so it pays for full coverage, which
/// it can afford because it runs once.
///
/// Collapsing the loop to a single window was green until this test: the other
/// boot tests use a double with no window, so the body always ran exactly once
/// and a `loop` that never iterated looked identical.
#[tokio::test]
async fn boot_recovery_reads_past_its_first_window() {
    // Three orphans - `dispatching`, no execution reference - against a window
    // of two, so a single-window boot leaves the third behind.
    let rows: Vec<_> = [ROW_1, ROW_2, ROW_3]
        .into_iter()
        .zip([RUN_1, RUN_2, RUN_3])
        .map(|(row, run)| {
            row_aged(
                row,
                OWNER_TENANT,
                run,
                PLATFORM_A,
                false,
                QueueState::Dispatching,
                9_000,
            )
        })
        .collect();
    let runs: Vec<_> = [RUN_1, RUN_2, RUN_3]
        .into_iter()
        .map(|run| {
            (
                OWNER_TENANT,
                run_fixture(run, Some(PLATFORM_A), false, RunState::Dispatching),
            )
        })
        .collect();

    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(runs)))
        .queue(Arc::new(FakeQueue::with_claim_scan_window(rows, 2)))
        .build()
        .await;

    let report = fakes.dispatch.recover_after_boot().await;

    assert_eq!(
        report.failed_orphans, 3,
        "every orphan must be recovered, not just the first window's"
    );
    assert_eq!(fakes.queue.state_of(ROW_3), Some(QueueState::Failed));
}

/// A row that **does** carry an execution reference is left to the tick
/// reconciler, because only it knows whether that execution is still alive —
/// legacy's `WHERE ... workflow_name IS NULL` leaves exactly those rows
/// untouched.
#[tokio::test]
async fn boot_recovery_leaves_rows_that_have_an_execution_ref() {
    let claim = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Dispatching,
        9_000,
    );
    let runs = Arc::new(FakeRuns::with(vec![(
        OWNER_TENANT,
        run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Dispatching),
    )]));
    runs.seed_execution_ref(RUN_1, "mock-execution-1", RunState::Dispatching);
    let fakes = Builder::new()
        .runs(runs)
        .queue(Arc::new(FakeQueue::with(vec![claim])))
        .build()
        .await;

    let report = fakes.dispatch.recover_after_boot().await;

    assert_eq!(report.failed_orphans, 0);
    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Dispatching));
    assert!(fakes.environments.released().is_empty());
}

/// **The orphan-timeout age guard.** A row sits in `dispatching` with no
/// execution reference for the whole force-sync + bundle-build window, which is
/// minutes. Failing it early abandons a launch that is still in progress — and
/// momentarily releases its claim, which is exactly when a second run could be
/// admitted alongside an exclusive one
/// (`manager/src/services/run_dispatcher.rs:417-426`).
///
/// Removing the guard — passing `0` as the orphan timeout where
/// `self.orphan_timeout_seconds` is read — fails this row and releases its lease.
/// Verified by removal.
#[tokio::test]
async fn a_young_mid_build_row_survives_a_tick() {
    let claim = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Dispatching,
        5,
    );
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Dispatching),
        )])))
        .queue(Arc::new(FakeQueue::with(vec![claim])))
        .orphan_timeout(600)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(
        report.failed_orphans, 0,
        "a five-second-old mid-build row is alive"
    );
    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Dispatching));
    assert!(
        fakes.environments.released().is_empty(),
        "releasing this claim is what lets a run start beside an exclusive one"
    );
}

/// A row past the orphan timeout with no execution *is* failed — the other side
/// of the same guard, so the test above cannot pass by the reconciler doing
/// nothing at all.
#[tokio::test]
async fn an_old_mid_build_row_is_failed_by_a_tick() {
    let claim = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Dispatching,
        900,
    );
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Dispatching),
        )])))
        .queue(Arc::new(FakeQueue::with(vec![claim])))
        .orphan_timeout(600)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.failed_orphans, 1);
    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Failed));
    assert_eq!(fakes.environments.released(), vec![(PLATFORM_A, RUN_1)]);
}

/// **A reconciled claim releases its lease and frees the platform.** This is what
/// stops a stuck execution blocking the queue forever
/// (`run_dispatcher.rs:448-456`) — and in this gear the lease, not the row, is
/// what a concurrent launch reads, so releasing the row without the lease would
/// leave the platform busy.
///
/// Removing the `release_lease` call leaves `released()` empty and the platform
/// occupied. Verified by removal.
#[tokio::test]
async fn a_reconciled_claim_releases_its_lease_and_frees_the_platform() {
    let claim = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Running,
        60,
    );
    let runs = Arc::new(FakeRuns::with(vec![(
        OWNER_TENANT,
        run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Running),
    )]));
    runs.seed_execution_ref(RUN_1, "vanished-execution", RunState::Running);
    let fakes = Builder::new()
        .runs(runs)
        .queue(Arc::new(FakeQueue::with(vec![claim])))
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.released, 1);
    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Done));
    assert_eq!(fakes.environments.released(), vec![(PLATFORM_A, RUN_1)]);
    assert_eq!(
        fakes.runs.state_of(RUN_1),
        Some(RunState::Running),
        "reconciliation owns the platform, not the run's verdict: it knows the \
         execution ended but not how, and ingest derives that",
    );
}

// ---------------------------------------------------------------------------
// The timeout sweep
// ---------------------------------------------------------------------------

/// `cpt-cf-qa-fr-runs-timeout`, enforced in the control plane. The run is
/// cancelled, moved to `TimedOut`, its claim released and its platform freed —
/// and all of it under the **run's own tenant**, never the enumeration identity.
#[tokio::test]
async fn the_timeout_sweep_cancels_under_the_runs_own_tenant() {
    let claim = row_aged(
        ROW_1,
        OTHER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Running,
        60,
    );
    let runs = Arc::new(FakeRuns::with(vec![(
        OTHER_TENANT,
        run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Running),
    )]));
    runs.seed_execution_ref(RUN_1, "mock-execution-1", RunState::Running);
    runs.seed_deadline(RUN_1, OffsetDateTime::now_utc() - TimeDuration::seconds(30));
    let fakes = Builder::new()
        .runs(runs)
        .queue(Arc::new(FakeQueue::with(vec![claim])))
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.timed_out, 1);
    assert_eq!(fakes.runs.state_of(RUN_1), Some(RunState::TimedOut));
    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Failed));
    assert_eq!(fakes.environments.released(), vec![(PLATFORM_A, RUN_1)]);

    let tenants = fakes.environments.tenants_seen.lock().unwrap().clone();
    assert!(
        tenants.contains(&OTHER_TENANT),
        "the writes ran under the run's own tenant"
    );
    assert!(
        !tenants.iter().any(Uuid::is_nil),
        "no write may run under the nil-tenant enumeration identity: {tenants:?}"
    );
}

/// **A covering policy grant must not turn the TTL sweep into a cross-tenant
/// write.** (Named for what it asserts. It was
/// `..._refuses_a_row_a_covering_scope_admitted`, which described the design this
/// test exists to *reject* — nothing is refused, and someone grepping test names
/// for the property would have read the abandoned answer.) `expire_queued_before` is set-based: its row set comes from the
/// compiled scope, so issuing it once per tenant is only tenant-safe if the PDP
/// narrows each tenant-bound write context to one tenant — and the
/// adversarial condition is a deployment whose policy instead grants the
/// `qa_runs.system` subject a *covering* set spanning every tenant.
///
/// **Driven under `CoveringSystemAuthZ`, deliberately.** The default
/// `SystemGrantingAuthZ` narrows a tenant-bound context to its own tenant, so it
/// models the *benign* PDP and this test is green with the check removed — found by
/// break-testing the first version of it. `CoveringSystemAuthZ` is the policy that
/// keys on the subject instead, which is the natural way for a policy to grant a
/// covering set to `qa_runs.system`'s tenant-bound contexts, and under it one
/// statement returns **both** tenants' rows in the first iteration of the
/// per-tenant loop.
///
/// **`expired == 2` alone cannot catch the defect.** Deriving the context from
/// the loop's tenant instead of the row's used to still leave `expired == 2` —
/// the deleted `LifecycleEvent::QueueExpired` used to stamp `ctx.subject_tenant_id()`
/// directly and that was what caught it originally. A covering scope is
/// indistinguishable from the correct per-row one by inspecting the *compiled*
/// scope alone (`AccessScope::contains_uuid` answers `true` for either identity
/// once the grant covers both tenants), so this test now reads
/// `CoveringSystemAuthZ`'s own record of the *request* instead — the raw
/// subject tenant each per-row context actually carried, before this double's
/// covering response erases the distinction. That is recoverable per row
/// because `expire_one_row` -> `read_run` -> `run_scope(ctx, GET, Some(row.run_id))`
/// puts the row's own run id on the request.
///
/// **`expired == 2` is also load-bearing in the other direction.** The first fix
/// *skipped* the foreign row, which is tenant-safe and still wrong: the bulk
/// statement had already moved it to `expired`, so skipping left its run
/// untouched — the guide's line 96 promise broken for that row, and `expired`
/// came back 1.
#[tokio::test]
async fn the_ttl_sweep_expires_a_foreign_row_under_its_own_tenant() {
    let stale_owner = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        false,
        QueueState::Queued,
        9_000,
    );
    let stale_other = row_aged(
        ROW_2,
        OTHER_TENANT,
        RUN_2,
        PLATFORM_B,
        false,
        QueueState::Queued,
        9_000,
    );
    let covering = Arc::new(fakes::CoveringSystemAuthZ::default());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![
            (
                OWNER_TENANT,
                run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Queued),
            ),
            (
                OTHER_TENANT,
                run_fixture(RUN_2, Some(PLATFORM_B), false, RunState::Queued),
            ),
        ])))
        .queue(Arc::new(FakeQueue::with(vec![stale_owner, stale_other])))
        .authz(Arc::clone(&covering) as Arc<dyn authz_resolver_sdk::AuthZResolverClient>)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.expired, 2, "both tenants' rows did expire");
    assert_eq!(
        fakes.runs.state_of(RUN_1),
        Some(RunState::Expired),
        "the owner-tenant row must reach Expired too, not only the other one"
    );
    assert_eq!(
        fakes.runs.state_of(RUN_2),
        Some(RunState::Expired),
        "and the foreign row, under its own tenant"
    );
    assert_eq!(
        covering.subject_tenants_for(RUN_1),
        vec![OWNER_TENANT],
        "the owner row's per-row context must carry the owner's own tenant, \
         not the loop's"
    );
    assert_eq!(
        covering.subject_tenants_for(RUN_2),
        vec![OTHER_TENANT],
        "and the foreign row's context must carry its own tenant, not the \
         owner's: this is the exact pairing a loop-tenant regression breaks"
    );
    assert!(
        !covering.subject_tenants().iter().any(Uuid::is_nil),
        "no per-row write may run under the nil-tenant enumeration identity"
    );
}

/// **A `Created` run is expired through `Queued`, not stranded.**
///
/// `can_transition` does not admit `Created -> Expired`, and a `Created` run with
/// a `queued` row is reachable: `launch::settle`'s unrepresentable-outcome arm
/// leaves exactly that. A single-edge attempt fails, the row goes `expired`, and
/// the run sits in `Created` forever with no queued row for any later tick — and
/// quietly, because `IllegalTransition` is not `Forbidden`, so `denied_passes`
/// stays empty while `expired` counts the row as handled.
///
/// Removing the `Created -> Queued` step reproduces exactly that:
/// `expired=1 row=Expired run=Created denied=[]`. Verified by removal.
///
/// Widening the state machine was rejected — `DESIGN.md:242` records a user
/// decision that `expired` is reachable only from `queued`. See
/// `DispatchService::expire_run`.
#[tokio::test]
async fn a_created_run_is_expired_through_queued_rather_than_stranded() {
    let stale = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        false,
        QueueState::Queued,
        9_000,
    );
    let runs = Arc::new(FakeRuns::with(vec![(
        OWNER_TENANT,
        // The state `launch::settle` leaves behind when it cannot record the
        // admission outcome.
        run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Created),
    )]));
    let fakes = Builder::new()
        .runs(Arc::clone(&runs))
        .queue(Arc::new(FakeQueue::with(vec![stale])))
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.expired, 1);
    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Expired));
    assert_eq!(
        runs.state_of(RUN_1),
        Some(RunState::Expired),
        "a run whose row expired must reach a terminal state, or it is lost: no \
         queued row remains for any later tick to claim it"
    );
    assert_eq!(
        runs.transitions.lock().unwrap().clone(),
        vec![
            (RUN_1, RunState::Created, RunState::Queued),
            (RUN_1, RunState::Queued, RunState::Expired),
        ],
        "through the edge the launch should have taken, not by widening the state \
         machine (DESIGN.md:242 is a user decision)"
    );
}

/// **The timeout sweep runs before every early return**, which is the whole
/// justification for placing it second rather than at the plan's position 8. A
/// failing `list_active` stops the tick at its third step, and an overdue run must
/// still be reclaimed — reclaiming it is what frees capacity when nothing else can
/// progress. Mirrors
/// `the_ttl_sweep_runs_even_when_the_executor_listing_fails`, for the same reason.
///
/// Moving the sweep to position 8 reddens this on `timed_out == 1` receiving `0`.
/// It also reddens `the_timeout_sweep_cancels_under_the_runs_own_tenant`, but only
/// **incidentally** — there on the queue row's state, because reconciliation gets
/// to the claim first and marks it `Done` instead of `Failed`. An argued deviation
/// whose only failing test fails for a neighbouring reason is how the next person
/// tidies it back.
///
/// The run has no execution reference, so there is nothing to cancel and the
/// executor is not consulted for it at all — which is what keeps this test about
/// the *ordering* rather than about `cancel`.
#[tokio::test]
async fn the_timeout_sweep_runs_even_when_the_executor_listing_fails() {
    let claim = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Dispatching,
        60,
    );
    let runs = Arc::new(FakeRuns::with(vec![(
        OWNER_TENANT,
        run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Dispatching),
    )]));
    runs.seed_deadline(RUN_1, OffsetDateTime::now_utc() - TimeDuration::seconds(30));
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    executor.fail_list_active("the executor is unreachable");
    let fakes = Builder::new()
        .runs(runs)
        .queue(Arc::new(FakeQueue::with(vec![claim])))
        .executor(executor)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(
        report.timed_out, 1,
        "an overdue run must be reclaimed even when the tick can go no further"
    );
    assert_eq!(report.stopped_at, Some("executor listing"));
    assert_eq!(fakes.runs.state_of(RUN_1), Some(RunState::TimedOut));
    assert_eq!(
        fakes.environments.released(),
        vec![(PLATFORM_A, RUN_1)],
        "reclaiming the run is what frees the platform, which is why the position \
         matters"
    );
}

/// **A claimed row whose `run_id` cannot be recovered is abandoned to the orphan
/// guard, not dispatched and not failed.**
///
/// The drain marks rows `dispatching` and then recovers each one's `run_id` from
/// `claims_for_platform`, because `queue::QueuedRow` carries only `{id, exclusive}`.
/// If that read comes back without the row, the tick has a claim it cannot act on —
/// and the decision is to leave it `dispatching`: the claim keeps the platform busy,
/// which is the fail-safe direction, and `orphan_timeout_seconds` reclaims it.
///
/// It was the only documented fail-safe arm in the drain with no test, so its
/// correctness lived entirely in prose. Three things must hold together and each
/// would be a different bug alone: the row is **not** dispatched (no execution
/// against an unknown run), **not** failed (the orphan guard owns that decision and
/// applies an age bound), and **no lease is taken** for it.
#[tokio::test]
async fn a_claimed_row_with_no_recoverable_run_is_left_to_the_orphan_guard() {
    let waiting = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        false,
        QueueState::Queued,
        5,
    );
    let queue = Arc::new(FakeQueue::with(vec![waiting]));
    *queue.hide_claims.lock().unwrap() = true;
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Queued),
        )])))
        .queue(Arc::clone(&queue))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .limits(QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 0,
            queue_ttl_seconds: 0,
        })
        .orphan_timeout(600)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.claimed, 0, "nothing was dispatched");
    assert!(
        executor.submitted().is_empty(),
        "no execution may be started for a run the tick cannot identify"
    );
    assert_eq!(
        queue.state_of(ROW_1),
        Some(QueueState::Dispatching),
        "left claimed on purpose: the claim keeps the platform busy, and the orphan \
         guard reclaims it with an age bound this pass has no basis to apply"
    );
    assert_eq!(
        report.failed_orphans, 0,
        "and not failed here - the row is five seconds old"
    );
    assert!(
        fakes.environments.acquires.lock().unwrap().is_empty(),
        "no lease may be taken for a run the tick cannot identify"
    );
    assert_eq!(fakes.runs.state_of(RUN_1), Some(RunState::Queued));
}

/// **A lost lease CAS returns the row to the queue; it does not kill the run.**
/// `domain::queue`'s module docs state the rule: a row the planner selects may
/// lose its `acquire` and *"stay queued for the next tick"*.
///
/// Reachable in production because Task 16's leader election protects the ticker
/// only — admission runs inline in every replica's REST handler, so one replica's
/// launch can hold the lease while another's tick is mid-drain.
///
/// Replacing `requeue_claim` with the `mark_failed`-plus-retire path this used to
/// have reddens both assertions, and the run does **not** reach `Error` even then:
/// at this point it is still `Queued`, which `can_transition` does not allow to
/// `Error`, so the old path left the run stranded in `Queued` with a `failed` row
/// no later tick could claim. Verified by restoring it.
#[tokio::test]
async fn a_row_that_loses_the_lease_race_goes_back_in_the_queue() {
    let waiting = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        false,
        QueueState::Queued,
        5,
    );
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Queued),
        )])))
        .queue(Arc::new(FakeQueue::with(vec![waiting])))
        // Free to read, `Busy` to acquire: the interleaving in which another
        // replica took the lease between this tick's occupancy read and its
        // acquisition.
        .environments(Arc::new(FakeEnvironments::busy_on_acquire()))
        .limits(QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 0,
            queue_ttl_seconds: 0,
        })
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.claimed, 0, "nothing was dispatched");
    assert_eq!(
        fakes.queue.state_of(ROW_1),
        Some(QueueState::Queued),
        "the row keeps its place for the next tick"
    );
    assert_eq!(
        fakes.runs.state_of(RUN_1),
        Some(RunState::Queued),
        "the run is untouched: losing a lease race is not a run failure"
    );
    assert!(
        fakes.queue.rows()[0].dispatched_at.is_none(),
        "a requeued row carries no dispatch instant, or `all_claims` would age it \
         from a claim it no longer has"
    );
}

/// The tick's half of the same lost-response hazard: a failed `acquire_lease`
/// releases, and the row still goes back in the queue.
///
/// Both must hold together. The release alone would leave the row `dispatching`
/// forever; the requeue alone would leave the platform leased to a run nothing will
/// dispatch — and, because a `queued` row is not a claim, with no pass able to free
/// it. See `admission::AdmissionService::take_lease` for why the acquisition may
/// have landed at all.
#[tokio::test]
async fn a_failed_acquisition_in_the_tick_releases_and_requeues() {
    let waiting = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Queued,
        5,
    );
    let environments = Arc::new(FakeEnvironments::free());
    *environments.fail_acquire.lock().unwrap() = true;
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Queued),
        )])))
        .queue(Arc::new(FakeQueue::with(vec![waiting])))
        .environments(Arc::clone(&environments))
        .limits(QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 0,
            queue_ttl_seconds: 0,
        })
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.claimed, 0);
    assert_eq!(
        environments.released(),
        vec![(PLATFORM_A, RUN_1)],
        "a lease the acquisition may have taken must be handed back"
    );
    assert_eq!(
        fakes.queue.state_of(ROW_1),
        Some(QueueState::Queued),
        "and the row keeps its place, so a later tick can retry it"
    );
    assert_eq!(fakes.runs.state_of(RUN_1), Some(RunState::Queued));
}

/// **A cancel that could not be delivered leaves the claim and the lease held.**
/// This pass looks like reconciliation and its fail-safe direction is the
/// opposite: reconciliation releases because it *observed* the execution end,
/// while this releases because the control plane *decided* to end it — and if the
/// cancel did not arrive, the execution may still be running. Releasing then is
/// what lets a second run start beside an exclusive one.
///
/// Releasing unconditionally (moving the release above the `continue`) makes both
/// assertions fail. Verified by moving it.
#[tokio::test]
async fn a_cancel_that_could_not_be_delivered_leaves_the_platform_held() {
    let claim = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Running,
        60,
    );
    let runs = Arc::new(FakeRuns::with(vec![(
        OWNER_TENANT,
        run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Running),
    )]));
    // The execution must be *alive* in the listing, or claim reconciliation
    // classifies it `Gone` and releases the platform for its own — correct —
    // reasons, and this test could not see what the sweep did.
    let executor = Arc::new(fakes::CancelFailingExecutor::new());
    let reference = executor.seed_active(RUN_1).await;
    runs.seed_execution_ref(RUN_1, reference.as_str(), RunState::Running);
    runs.seed_deadline(RUN_1, OffsetDateTime::now_utc() - TimeDuration::seconds(30));
    let fakes = Builder::new()
        .runs(runs)
        .queue(Arc::new(FakeQueue::with(vec![claim])))
        .executor(executor)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.timed_out, 0, "nothing was reclaimed");
    assert_eq!(
        fakes.runs.state_of(RUN_1),
        Some(RunState::Running),
        "the run stays running, because it may well still be"
    );
    assert!(
        fakes.environments.released().is_empty(),
        "the platform stays held on an undelivered cancel"
    );
}

// ---------------------------------------------------------------------------
// Tenancy and the denied-actor path
// ---------------------------------------------------------------------------

/// **Every background write is bound to the row's own tenant.** Two tenants, one
/// queued row each, one tick: both drain, each under its owner. A nil-tenant
/// write is *denied* rather than mis-scoped, so getting this wrong makes the
/// dispatcher inert.
///
/// **Driven under `CoveringSystemAuthZ`, not the harness default.** Under the
/// default `SystemGrantingAuthZ` a per-row context that named the *other* row's
/// tenant would simply be denied — a narrow scope only matches its own tenant,
/// so the wrong-pairing bug this test is named for would refuse the write
/// rather than mis-scope it, and the test would prove nothing about pairing.
/// `CoveringSystemAuthZ` grants both tenants to the gear's system subject
/// regardless of which one a request names, so a wrong-pairing write succeeds
/// and only `CoveringSystemAuthZ::subject_tenants_for` — reading the *request*,
/// not the response — can tell it apart from a correct one. `environments.tenants_seen`
/// still pins that both tenants were reached and neither is nil; it is the
/// run-tenant *pairing* that needs the recorder, because a flat set of tenants
/// seen cannot distinguish "each row got its own tenant" from "the two rows'
/// tenants were swapped".
#[tokio::test]
async fn the_dispatcher_writes_under_the_rows_own_tenant() {
    let rows = vec![
        row_aged(
            ROW_1,
            OWNER_TENANT,
            RUN_1,
            PLATFORM_A,
            false,
            QueueState::Queued,
            5,
        ),
        row_aged(
            ROW_2,
            OTHER_TENANT,
            RUN_2,
            PLATFORM_B,
            false,
            QueueState::Queued,
            5,
        ),
    ];
    let covering = Arc::new(fakes::CoveringSystemAuthZ::default());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![
            (
                OWNER_TENANT,
                run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Queued),
            ),
            (
                OTHER_TENANT,
                run_fixture(RUN_2, Some(PLATFORM_B), false, RunState::Queued),
            ),
        ])))
        .queue(Arc::new(FakeQueue::with(rows)))
        .limits(QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 0,
            queue_ttl_seconds: 0,
        })
        .authz(Arc::clone(&covering) as Arc<dyn authz_resolver_sdk::AuthZResolverClient>)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.claimed, 2, "both tenants' platforms drained");
    assert_eq!(fakes.queue.state_of(ROW_1), Some(QueueState::Running));
    assert_eq!(fakes.queue.state_of(ROW_2), Some(QueueState::Running));

    let tenants = fakes.environments.tenants_seen.lock().unwrap().clone();
    assert!(tenants.contains(&OWNER_TENANT));
    assert!(tenants.contains(&OTHER_TENANT));
    assert!(
        !tenants.iter().any(Uuid::is_nil),
        "an enumeration context reached a write: {tenants:?}"
    );

    assert_eq!(
        covering.subject_tenants_for(RUN_1),
        vec![OWNER_TENANT],
        "RUN_1's writes must carry the owner's own tenant, not the other row's"
    );
    assert_eq!(
        covering.subject_tenants_for(RUN_2),
        vec![OTHER_TENANT],
        "and RUN_2's must carry its own: this is the pairing tenants_seen alone \
         cannot see"
    );
    assert!(
        !covering.subject_tenants().iter().any(Uuid::is_nil),
        "no per-row write may run under the nil-tenant enumeration identity"
    );
}

/// **A denied system actor does not kill the ticker**, and each pass records its
/// denial exactly once.
///
/// This is the reference dev stack's steady state: `static-authz` denies every
/// gear system actor, so every pass is `Forbidden`. Two ticks are driven to show
/// the second one still runs.
///
/// **What this pins and what it does not.** This asserts the classification and
/// the per-pass folding that the WARN is derived from, not the emission itself.
/// The emission is pinned separately, by
/// [`a_pass_denied_once_per_row_logs_its_remedy_once`], which counts the lines
/// on a pass that is denied once per row - the case where "one WARN per pass"
/// and "one WARN per denial" actually differ.
#[tokio::test]
async fn a_forbidden_system_actor_records_each_pass_once_and_does_not_kill_the_ticker() {
    let fakes = Builder::new()
        .authz(Arc::new(fakes::DenyingAuthZ))
        .queue(Arc::new(FakeQueue::with(vec![queued_row(
            ROW_1,
            OWNER_TENANT,
            RUN_1,
            PLATFORM_A,
            false,
            QueueState::Queued,
        )])))
        .build()
        .await;

    let first = fakes.dispatch.run_tick().await;
    assert!(
        !first.denied_passes.is_empty(),
        "a denied deployment must be visible in the report"
    );
    let mut deduped = first.denied_passes.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(
        deduped.len(),
        first.denied_passes.len(),
        "a pass recorded twice would log its remedy twice: {:?}",
        first.denied_passes
    );
    assert_eq!(first.claimed, 0, "nothing was dispatched");

    // The ticker survives: a second tick runs and reports the same way.
    let second = fakes.dispatch.run_tick().await;
    assert_eq!(second.denied_passes, first.denied_passes);
}

/// The deduplication itself, on a pass that is denied **per row**.
///
/// A blanket deny never reaches a pass's rows, so it cannot tell "one WARN per
/// pass" from "one WARN, full stop". Here `list` is granted and `get` denied, so
/// claim reconciliation enumerates two claims and is refused twice — and records
/// one entry.
#[tokio::test]
async fn a_pass_denied_once_per_row_records_one_entry() {
    let rows = vec![
        row_aged(
            ROW_1,
            OWNER_TENANT,
            RUN_1,
            PLATFORM_A,
            false,
            QueueState::Running,
            60,
        ),
        row_aged(
            ROW_2,
            OWNER_TENANT,
            RUN_2,
            PLATFORM_A,
            false,
            QueueState::Running,
            60,
        ),
    ];
    let fakes = Builder::new()
        .authz(Arc::new(fakes::DenyingActionAuthZ { action: "get" }))
        .queue(Arc::new(FakeQueue::with(rows)))
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(
        report
            .denied_passes
            .iter()
            .filter(|pass| **pass == "claim reconciliation")
            .count(),
        1,
        "two denied rows in one pass must produce one entry: {:?}",
        report.denied_passes
    );
}

// ---------------------------------------------------------------------------
// The guarantees a log line is the only carrier of
// ---------------------------------------------------------------------------

/// The dedup rule as an operator experiences it: **one WARN, not one per row.**
///
/// [`a_pass_denied_once_per_row_records_one_entry`] pins the report entry, which
/// is the data structure the rule is derived from. This pins the derived thing -
/// the number of lines an operator actually sees - because the report is not
/// what a denied deployment is diagnosed from and a `denied_passes` of length
/// one alongside fifty WARNs would satisfy that test while failing the purpose.
///
/// The counted substring is the remedy sentence rather than the whole message,
/// so a rewording of the prose around it does not fail the test while a change
/// to the *rate* does.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_pass_denied_once_per_row_logs_its_remedy_once() {
    let rows = vec![
        row_aged(
            ROW_1,
            OWNER_TENANT,
            RUN_1,
            PLATFORM_A,
            false,
            QueueState::Running,
            60,
        ),
        row_aged(
            ROW_2,
            OWNER_TENANT,
            RUN_2,
            PLATFORM_A,
            false,
            QueueState::Running,
            60,
        ),
    ];
    let fakes = Builder::new()
        .authz(Arc::new(fakes::DenyingActionAuthZ { action: "get" }))
        .queue(Arc::new(FakeQueue::with(rows)))
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;
    assert_eq!(
        report
            .denied_passes
            .iter()
            .filter(|pass| **pass == "claim reconciliation")
            .count(),
        1
    );

    logs_assert(|lines: &[&str]| {
        let emitted = lines
            .iter()
            .filter(|line| line.contains("is not authorized for this pass"))
            .filter(|line| line.contains("claim reconciliation"))
            .count();
        if emitted == 1 {
            Ok(())
        } else {
            Err(format!(
                "expected exactly one denial WARN for the reconciliation pass, saw {emitted}"
            ))
        }
    });
}

/// **The expiry alert the frozen guide makes mandatory** (guide line 96: an
/// expired row "will never start" and an alert "is always sent for this, so it
/// cannot disappear silently").
///
/// This port has no Slack integration, so the WARN *is* the alert: it is the
/// only artefact that survives the tick and names the run. The state changes
/// this test's siblings assert are not a substitute - a row that quietly went
/// `expired` with no line is precisely the silent disappearance the guide
/// forbids.
///
/// The assertions name the run id and the waited/TTL pair rather than the
/// sentence alone, because a line that says a row expired without saying which
/// one or how long it waited does not discharge the guide's requirement.
#[tokio::test]
#[tracing_test::traced_test]
async fn an_expired_row_is_always_announced_with_its_run_and_its_wait() {
    let stale = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        false,
        QueueState::Queued,
        9_000,
    );
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(
            OWNER_TENANT,
            run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Queued),
        )])))
        .queue(Arc::new(FakeQueue::with(vec![stale])))
        .limits(QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 0,
            queue_ttl_seconds: 7200,
        })
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;
    assert_eq!(report.expired, 1);

    assert!(
        logs_contain("run-queue row EXPIRED"),
        "the guide's mandatory alert must be emitted"
    );
    assert!(
        logs_contain(&RUN_1.to_string()),
        "the alert must name the run, or it cannot be acted on"
    );
    assert!(
        logs_contain("ttl_seconds=7200"),
        "the alert must name the TTL the row outlived"
    );
}

/// **Both claim scans in one tick read the same window.**
///
/// The tick reads claims twice - once to reconcile, once to count against the
/// cluster-wide cap - and `committed_active` looks each cap-pass row up in the
/// classification the reconciliation pass built. If the two scans see disjoint
/// windows every lookup misses, so live executions are counted twice
/// (`active.len()` already has them) and the reconciled window's genuinely
/// uncommitted claims are not counted at all. On a cluster past
/// `MAX_CLAIM_SCAN` that can make the dispatcher stop while under its real cap.
///
/// **Regression test.** Advancing the cursor inside `reconcile_claims` - which
/// is what shipped in `54292938`, with several doc comments asserting the
/// opposite - produced `[None, Some(ROW_2)]` here. Nothing else observes it:
/// the rows each scan returns look plausible either way, which is why this
/// asserts on the *arguments*.
#[tokio::test]
async fn both_claim_scans_in_a_tick_read_the_same_window() {
    let rows: Vec<_> = [ROW_1, ROW_2, ROW_3]
        .into_iter()
        .zip([RUN_1, RUN_2, RUN_3])
        .map(|(row, run)| {
            row_aged(
                row,
                OWNER_TENANT,
                run,
                PLATFORM_A,
                false,
                QueueState::Dispatching,
                10,
            )
        })
        .collect();
    let runs: Vec<_> = [RUN_1, RUN_2, RUN_3]
        .into_iter()
        .map(|run| {
            (
                OWNER_TENANT,
                run_fixture(run, Some(PLATFORM_A), false, RunState::Dispatching),
            )
        })
        .collect();

    let queue = Arc::new(FakeQueue::with_claim_scan_window(rows, 2));
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(runs)))
        .queue(Arc::clone(&queue))
        // A cap that is *enabled*, so `evaluate_cap` really runs its scan.
        .limits(QueueLimits {
            queue_max_depth: 20,
            max_concurrent_runs: 10,
            queue_ttl_seconds: 7200,
        })
        .orphan_timeout(600)
        .build()
        .await;

    let _report = fakes.dispatch.run_tick().await;

    let cursors = queue.claim_scan_cursors.lock().unwrap().clone();
    assert_eq!(
        cursors.len(),
        2,
        "the tick issues exactly two claim scans: {cursors:?}"
    );
    assert_eq!(
        cursors[0], cursors[1],
        "both scans must read the same window, or the cap counts a \
         classification built over different rows: {cursors:?}"
    );
}

/// **The starvation regression.** A tenant's healthy claims cannot exclude
/// another tenant's rows from reconciliation.
///
/// This is the failure the claim scan shipped with on 2026-08-15 and which the
/// rotation closes, reproduced at the scale a test can drive.
/// `reconcile_claim` answers `Keep` for a healthy claim and `Keep` writes
/// nothing, so under the original `enqueued_at ASC` ordering the first two rows
/// here would have been returned by every scan forever and the third would never
/// have been reconciled at all - its finished execution never releasing its
/// platform.
///
/// Three ticks, a window of two: the first covers rows one and two, the second
/// resumes past them and reaches the third, the third wraps. The assertion is on
/// **which claims were reconciled**, observed through the release of the one
/// whose execution is gone.
///
/// Restoring a stable ordering - having the double ignore `after` - leaves the
/// third row unreleased through all three ticks, which is the mutation this
/// test exists to catch.
#[tokio::test]
async fn a_scan_window_rotates_so_one_tenants_claims_cannot_starve_another() {
    // Ids ascend, so the scan order is ROW_1, ROW_2, ROW_3.
    let healthy_a = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        false,
        QueueState::Dispatching,
        10,
    );
    let healthy_b = row_aged(
        ROW_2,
        OWNER_TENANT,
        RUN_2,
        PLATFORM_A,
        false,
        QueueState::Dispatching,
        10,
    );
    // The victim: its execution is gone, so reconciliation must release it -
    // but only once a scan actually reaches it.
    let finished = row_aged(
        ROW_3,
        OTHER_TENANT,
        RUN_3,
        PLATFORM_B,
        false,
        QueueState::Running,
        60,
    );

    // The two healthy rows carry no execution reference and are younger than
    // the orphan timeout, so they classify `Absent` -> `Keep`: a claim
    // mid-launch, which is the ordinary state of a healthy row and the one that
    // performs no write.
    let runs = Arc::new(FakeRuns::with(vec![
        (
            OWNER_TENANT,
            run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching),
        ),
        (
            OWNER_TENANT,
            run_fixture(RUN_2, Some(PLATFORM_A), false, RunState::Dispatching),
        ),
        (
            OTHER_TENANT,
            run_fixture(RUN_3, Some(PLATFORM_B), false, RunState::Running),
        ),
    ]));
    // The victim: a reference the executor no longer lists, so it classifies
    // `Gone` and reconciliation must release it - once a scan reaches it.
    runs.seed_execution_ref(RUN_3, "vanished-execution", RunState::Running);

    let fakes = Builder::new()
        .runs(runs)
        .queue(Arc::new(FakeQueue::with_claim_scan_window(
            vec![healthy_a, healthy_b, finished],
            2,
        )))
        .orphan_timeout(600)
        .build()
        .await;

    let first = fakes.dispatch.run_tick().await;
    assert_eq!(
        first.released, 0,
        "the first window holds only the two healthy claims"
    );
    assert_eq!(fakes.queue.state_of(ROW_3), Some(QueueState::Running));

    let second = fakes.dispatch.run_tick().await;
    assert_eq!(
        second.released, 1,
        "the second scan resumes past them and reaches the row a stable \
         ordering would have excluded forever"
    );
    assert_eq!(fakes.queue.state_of(ROW_3), Some(QueueState::Done));

    // **The wrap**, which is the half a passing resume does not imply. The
    // second scan came back short, so the cursor must reset to the beginning of
    // the id space. Pinning it at the end instead - the mutation this asserts
    // against - makes every later scan return zero rows forever: no claim is
    // reconciled again and every platform lease leaks permanently.
    //
    // Driven by seeding a row whose id sorts *below* everything scanned so far.
    // Only a wrapped cursor can reach it.
    let below = row_aged(
        ROW_0,
        OTHER_TENANT,
        RUN_4,
        PLATFORM_B,
        false,
        QueueState::Running,
        60,
    );
    fakes.runs.insert_for_test(
        OTHER_TENANT,
        run_fixture(RUN_4, Some(PLATFORM_B), false, RunState::Running),
    );
    fakes
        .runs
        .seed_execution_ref(RUN_4, "also-vanished", RunState::Running);
    fakes.queue.insert_for_test(below);

    let third = fakes.dispatch.run_tick().await;
    assert_eq!(third.stopped_at, None, "the third tick runs normally");
    assert_eq!(
        fakes.queue.state_of(ROW_0),
        Some(QueueState::Done),
        "the cursor wrapped to the beginning of the id space and reached a row \
         below everything it had already scanned"
    );
}

// ---------------------------------------------------------------------------
// Composition
// ---------------------------------------------------------------------------

/// **The composition no test inside either module can reach.** Admission
/// serialises "may this run start now?" per platform and the tick serialises
/// "which queued rows may I claim?" per platform, and those are the same critical
/// section. Two registries would leave both halves individually correct and the
/// composition broken.
///
/// Giving `AppServices::new` a second `PlatformLocks::default()` for the dispatch
/// service makes this fail. Verified by doing so.
#[tokio::test]
async fn admission_and_dispatch_share_one_platform_lock_registry() {
    let runs = Arc::new(FakeRuns::default());
    let queue = Arc::new(FakeQueue::default());
    let services = crate::domain::service::AppServices::new(
        runs,
        queue,
        Arc::new(crate::infra::storage::OrmSchedulesRepository),
        crate::domain::service::ServiceDeps {
            db: crate::domain::service::test_support::test_db_provider().await,
            authz: Arc::new(fakes::SystemGrantingAuthZ),
            catalog: Arc::new(FakeCatalog::serving(&["tests/a.py"])),
            environments: Arc::new(FakeEnvironments::free()),
            product_plugins: Arc::new(
                crate::domain::service::admission::tests::fakes::FakeProductPlugins::default(),
            ),
            executor: Arc::new(crate::infra::executor::mock::MockRunExecutor::new()),
            logs: Arc::new(crate::infra::logs::RunLogBroadcaster::default()),
            archive: Arc::new(crate::domain::service::test_support::NullLogArchive)
                as Arc<dyn crate::domain::service::LogArchive>,
            admitter: None,
            dispatcher: None,
            watcher: None,
            cancel: tokio_util::sync::CancellationToken::new(),
            default_timeout_seconds: 900,
            limits: QueueLimits {
                queue_max_depth: 20,
                max_concurrent_runs: 0,
                queue_ttl_seconds: 7200,
            },
            orphan_timeout_seconds: 600,
        },
    );

    let from_admission = services.admission.locks().get(PLATFORM_A).await;
    let from_dispatch = services.dispatch.locks().get(PLATFORM_A).await;
    assert!(
        Arc::ptr_eq(&from_admission, &from_dispatch),
        "admission and the tick must contend for one mutex per platform, or a launch \
         and a tick can each observe an idle platform and each start a run on it"
    );
}

/// **Added 2026-08-14 by the security review's B4(a).** Only `service::ingest`
/// released a run's live log channel, so a run the *tick* retired — a TTL
/// expiry, a control-plane timeout, an orphan recovery, a failed submit — kept
/// its channel forever. `LogSubscription::recv` answers `None` only when the
/// channel is dropped, so an SSE handler watching such a run held its connection
/// open indefinitely.
///
/// The reap lives in `DispatchService::transition` rather than at those four call
/// sites, so this covers all of them through the one that is easiest to drive.
#[tokio::test]
async fn a_run_the_tick_times_out_releases_its_log_channel() {
    let run_id = Uuid::new_v4();
    let runs = Arc::new(FakeRuns::with(vec![(
        OWNER_TENANT,
        run_fixture(run_id, Some(PLATFORM_A), false, RunState::Running),
    )]));
    runs.seed_execution_ref(run_id, "mock-execution-1", RunState::Running);
    runs.seed_deadline(
        run_id,
        OffsetDateTime::now_utc() - time::Duration::seconds(60),
    );
    let fakes = fakes::Builder::new().runs(Arc::clone(&runs)).build().await;
    assert!(fakes.logs.reaped().is_empty());

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.timed_out, 1, "premise: the sweep reclaimed the run");
    assert_eq!(runs.state_of(run_id), Some(RunState::TimedOut));
    assert_eq!(
        fakes.logs.reaped(),
        vec![run_id],
        "a terminal transition must release the channel, or the run's SSE watchers \
         never see the stream end"
    );
}

/// The other half: a transition that does **not** reach a terminal state leaves
/// the channel alone, so a still-live run's watchers keep their stream.
#[tokio::test]
async fn a_non_terminal_transition_leaves_the_log_channel_alone() {
    let run_id = Uuid::new_v4();
    let runs = Arc::new(FakeRuns::with(vec![(
        OWNER_TENANT,
        run_fixture(run_id, Some(PLATFORM_A), false, RunState::Queued),
    )]));
    let queue = Arc::new(FakeQueue::with(vec![queued_row(
        Uuid::new_v4(),
        OWNER_TENANT,
        run_id,
        PLATFORM_A,
        false,
        QueueState::Queued,
    )]));
    let fakes = fakes::Builder::new()
        .runs(Arc::clone(&runs))
        .queue(queue)
        .build()
        .await;

    fakes.dispatch.run_tick().await;

    assert_eq!(runs.state_of(run_id), Some(RunState::Running));
    assert!(
        fakes.logs.reaped().is_empty(),
        "the run is live; its watchers must keep their stream"
    );
}

/// **The guard the user's state-machine widening made necessary, pinned.**
///
/// Opening `Succeeded -> {Failed, Error, Canceled, TimedOut}` made
/// `Succeeded -> Error` a *legal* transition, and `fail_orphan` is the one pass
/// in this gear that can be handed an already-terminal run: `reconcile_claim`
/// answers `FailOrphaned` for a claim whose row carries no execution reference,
/// and a run can be `Succeeded` with no reference recorded because
/// `record_started` deliberately swallows a failed `set_execution_ref` — the
/// execution is genuinely running, so an error return would be a lie.
///
/// Before the widening the state machine refused the write and the verdict
/// survived by accident. The `is_terminal` guard makes that deliberate, and this
/// is what fails when it is removed: without it the sweep performs a real
/// `update_state(Succeeded -> Error)`, overwriting `finished_at` and `error` on a
/// run whose results ingest already judged.
///
/// The claim and the lease are still released, because the row is genuinely
/// orphaned whatever the run says and leaving it would keep the platform busy.
#[tokio::test]
async fn an_orphaned_claim_never_overwrites_a_finished_runs_verdict() {
    let claim = row_aged(
        ROW_1,
        OWNER_TENANT,
        RUN_1,
        PLATFORM_A,
        true,
        QueueState::Dispatching,
        900,
    );
    let runs = Arc::new(FakeRuns::with(vec![(
        OWNER_TENANT,
        // No `execution_ref`, so the claim classifies as `Absent`; already
        // `Succeeded`, because ingest got there first.
        run_fixture(RUN_1, Some(PLATFORM_A), true, RunState::Succeeded),
    )]));
    let fakes = Builder::new()
        .runs(Arc::clone(&runs))
        .queue(Arc::new(FakeQueue::with(vec![claim])))
        .environments(Arc::new(fakes::FakeEnvironments::holding(
            PLATFORM_A,
            qa_environments_sdk::LeaseState::HeldExclusive { holder: RUN_1 },
        )))
        .orphan_timeout(600)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(
        report.failed_orphans, 1,
        "premise: the sweep really did classify this claim as orphaned"
    );
    assert_eq!(
        runs.state_of(RUN_1),
        Some(RunState::Succeeded),
        "an orphan sweep must never overwrite a verdict ingest derived from results"
    );
    assert!(
        runs.transitions
            .lock()
            .unwrap()
            .iter()
            .all(|(_, _, to)| *to != RunState::Error),
        "and it must not have attempted the write either"
    );
    assert_eq!(
        fakes.queue.state_of(ROW_1),
        Some(QueueState::Failed),
        "the row is genuinely orphaned, so its claim is still failed"
    );
    assert_eq!(
        fakes.environments.released(),
        vec![(PLATFORM_A, RUN_1)],
        "and the platform is still handed back, or it reads busy forever"
    );
}

// ---------------------------------------------------------------------------
// Watcher re-attachment
// ---------------------------------------------------------------------------

/// A live run with a reference gets an observer; the two rows that should not
/// are named beside it so the predicate is asserted rather than assumed.
///
/// Removing `execution_ref IS NOT NULL` from `list_watch_candidates` attaches an
/// observer to `RUN_2`, whose submit never returned a handle — a `watch` on a
/// reference that does not exist, on a row boot recovery owns. Removing the
/// state pair attaches one to `RUN_3`, whose execution is over.
#[tokio::test]
async fn the_tick_observes_a_live_run_that_holds_an_execution() {
    let mut live = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Running);
    live.execution_ref = Some("mock-execution-1".to_owned());
    // Dispatching, no handle: `record_started` never got that far.
    let no_handle = run_fixture(RUN_2, Some(PLATFORM_A), false, RunState::Dispatching);
    // Terminal, with a handle: the execution is over.
    let mut done = run_fixture(RUN_3, Some(PLATFORM_A), false, RunState::Succeeded);
    done.execution_ref = Some("mock-execution-3".to_owned());

    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![
            (OWNER_TENANT, live),
            (OWNER_TENANT, no_handle),
            (OWNER_TENANT, done),
        ])))
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.attached, 1);
    let attached = fakes.watcher.attached();
    assert_eq!(attached.len(), 1, "one observer, on the one live run");
    assert_eq!(attached[0].run_id, RUN_1);
    assert_eq!(attached[0].execution_ref.as_str(), "mock-execution-1");
    assert_eq!(
        attached[0].tenant.get(),
        OWNER_TENANT,
        "the observer is bound to the run row's own tenant, because ingest stamps \
         that tenant onto every per-test row it writes"
    );
}

/// **A run with no platform is observed too, and that is the whole reason the
/// re-attachment read is over `qa_runs` and not over the queue.**
///
/// `AdmissionService` answers `Admission::Unqueued` for a platformless run and
/// writes **no queue row at all**, so a claim-scan-driven re-attach — the shape
/// this task started from — would leave exactly these runs with no observer and
/// no way to end but their deadline. `FakeQueue` is empty here on purpose: if
/// this test passes with a claim in the fixture, it is not testing what it says.
#[tokio::test]
async fn a_platformless_run_is_observed_even_though_it_has_no_queue_row() {
    let mut unqueued = run_fixture(RUN_1, None, false, RunState::Running);
    unqueued.execution_ref = Some("mock-execution-1".to_owned());

    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, unqueued)])))
        .queue(Arc::new(FakeQueue::default()))
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(report.attached, 1);
    assert_eq!(
        fakes.watcher.attached()[0].run_id,
        RUN_1,
        "a platformless run appears in no claim scan, so the run table is the only \
         source that can find it"
    );
}

/// The idempotency, driven through the pass rather than through the registry:
/// a second tick must not add a second producer. Since Task 16b step 1 that is
/// no longer a correctness requirement — two producers contend and retry rather
/// than corrupting the rows — but a second observer is duplicated work on every
/// event, so the pass still owes idempotence.
///
/// The second half is what makes the first non-vacuous: once the observer ends,
/// the run is attachable again, so the pass is not simply refusing everything
/// after the first tick.
#[tokio::test]
async fn a_second_tick_does_not_add_a_second_observer() {
    let mut live = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Running);
    live.execution_ref = Some("mock-execution-1".to_owned());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, live)])))
        .build()
        .await;

    assert_eq!(fakes.dispatch.run_tick().await.attached, 1);
    assert_eq!(
        fakes.dispatch.run_tick().await.attached,
        0,
        "a run already being observed costs neither a policy decision nor a query, \
         and above all does not get a second producer"
    );
    assert_eq!(fakes.watcher.attached().len(), 1);

    fakes.watcher.detach(RUN_1);
    assert_eq!(
        fakes.dispatch.run_tick().await.attached,
        1,
        "an observer that ended frees the run, or a stream that closed early would \
         leave it unobserved forever"
    );
}

/// Each candidate is observed under **its own** tenant, never under the
/// enumeration identity and never under the previous candidate's.
///
/// This is the shape the TTL sweep got wrong: a covering policy scope makes the
/// loop's tenant and the row's tenant differ, and every write downstream of the
/// wrong one lands in the wrong tenant. Here the consequence is concrete —
/// `IngestService` writes `ctx.subject_tenant_id()` into
/// `qa_run_test_results.tenant_id`.
#[tokio::test]
async fn each_observed_run_carries_its_own_tenant() {
    let mut mine = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Running);
    mine.execution_ref = Some("mock-execution-1".to_owned());
    let mut theirs = run_fixture(RUN_2, Some(PLATFORM_B), false, RunState::Running);
    theirs.execution_ref = Some("mock-execution-2".to_owned());

    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![
            (OWNER_TENANT, mine),
            (OTHER_TENANT, theirs),
        ])))
        .build()
        .await;

    fakes.dispatch.run_tick().await;

    let mut seen: Vec<(Uuid, Uuid)> = fakes
        .watcher
        .attached()
        .iter()
        .map(|target| (target.run_id, target.tenant.get()))
        .collect();
    seen.sort_unstable();
    assert_eq!(seen, vec![(RUN_1, OWNER_TENANT), (RUN_2, OTHER_TENANT)]);

    // **And the candidate read itself is narrow.** The pass's doc claims every
    // step after the scan runs under `for_result_ingest(tenant)`; the target's
    // tenant above is carried from the scanned row and would look identical if
    // the read had been issued under the cross-tenant enumeration identity,
    // which admits both tenants here. Break-tested: swapping `&ctx` for
    // `&enumeration` at that call site was green until this assertion. Not
    // exploitable today - the run id came from the scan and only
    // `execution_ref` is consumed - which is why it is a line here rather than
    // a redesign.
    assert_eq!(
        fakes.runs.tenants_admitted_reading(RUN_1),
        vec![vec![OWNER_TENANT]],
        "the candidate read must be authorized over the run's own tenant alone"
    );
    assert_eq!(
        fakes.runs.tenants_admitted_reading(RUN_2),
        vec![vec![OTHER_TENANT]]
    );
}

/// **The rotation, pinned at the caller — which is where its own doc says the
/// bound lives.**
///
/// `list_watch_candidates`' doc promises every candidate is visited within
/// `ceil(candidates / MAX_WATCH_SCAN)` calls *"provided the caller advances
/// `after` past what it saw and restarts on a short window"*, and warns that
/// without the rotation the pass has *"a reachable set of inputs on which it
/// does nothing forever"* — because a healthy live run, unlike a claim or a
/// timeout candidate, never leaves the candidate set.
///
/// Mutating the cursor advance to an unconditional `None` reproduces exactly
/// that starvation, and it was green across the whole suite until this test:
/// `Windowed::complete` never truncates, so the cursor was `None` on every tick
/// anyway. Same three-part shape as
/// [`a_scan_window_rotates_so_one_tenants_claims_cannot_starve_another`], and
/// the wrap is the half a passing resume does not imply.
///
/// (An earlier revision cited that precedent as
/// `the_claim_scan_wraps_and_reaches_a_row_below_its_cursor`, an identifier that
/// exists nowhere in the tree. It landed in the one sentence whose purpose is to
/// let a reader check that this test follows an established shape — which is
/// exactly where Task 16's invented identifier landed too. **A rustdoc link is
/// not what fixes that**: measured, a `[`nonexistent`]` link in a
/// `#[cfg(test)]` module leaves `cargo clippy --all-targets` at exit 0, because
/// `broken_intra_doc_links` is a rustdoc lint and `cargo doc` does not build
/// this module at all. Nothing checks the sentence you are reading.)
#[tokio::test]
async fn the_watch_scan_resumes_past_what_it_saw_and_wraps() {
    // **The precedent function, named in code rather than only in prose.** A
    // `const` rather than a `let _`, which
    // `clippy::no_effect_underscore_binding` denies, and `fn()` because
    // `#[tokio::test]` rewrites the async fn to one.
    //
    // Verified by execution: renaming the precedent fails this file with
    // `E0425`, and restoring it returns to exit 0.
    //
    // **What it does not cover.** The doc sentence above is a separate,
    // unchecked string. A reviewer reverted only that link to the invented
    // identifier, left this binding untouched, and clippy stayed at exit 0. No
    // further claim is made here about what this binding buys: three revisions
    // of that claim have been wrong, and the third was the one asserting this
    // binding fixed the second.
    const _PRECEDENT: fn() = a_scan_window_rotates_so_one_tenants_claims_cannot_starve_another;

    let mut first = run_fixture(RUN_2, Some(PLATFORM_A), false, RunState::Running);
    first.execution_ref = Some("exec-2".to_owned());
    let mut second = run_fixture(RUN_3, Some(PLATFORM_A), false, RunState::Running);
    second.execution_ref = Some("exec-3".to_owned());

    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with_watch_scan_window(
            vec![(OWNER_TENANT, first), (OWNER_TENANT, second)],
            1,
        )))
        .build()
        .await;

    let one = fakes.dispatch.run_tick().await;
    assert_eq!(one.attached, 1, "the window holds one candidate");
    assert_eq!(fakes.watcher.attached()[0].run_id, RUN_2);

    let two = fakes.dispatch.run_tick().await;
    assert_eq!(
        two.attached, 1,
        "the second scan resumes past the first and reaches the run a stable \
         ordering would have excluded for as long as the first kept running"
    );
    assert_eq!(fakes.watcher.attached()[1].run_id, RUN_3);

    // **The wrap.** The second scan came back short, so the cursor resets to
    // the beginning of the id space. Pinned at the end instead, every later
    // scan returns nothing and a run started afterwards is never observed —
    // which is the permanent starvation, not a delay.
    let mut below = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Running);
    below.execution_ref = Some("exec-1".to_owned());
    fakes.runs.insert_for_test(OWNER_TENANT, below);

    let three = fakes.dispatch.run_tick().await;
    assert_eq!(three.attached, 1);
    assert_eq!(
        fakes.watcher.attached()[2].run_id,
        RUN_1,
        "the cursor wrapped and reached a run whose id sorts below everything \
         it had already scanned"
    );
}

/// **The pass runs ahead of the executor listing's early return**, which is the
/// ordering argument the two sweeps already make and this one inherits: an
/// unreadable executor makes the tick `return` before reconciling anything, so a
/// re-attachment placed after it would stop observing runs for the whole outage
/// and every run started before it would end at its deadline.
///
/// Moving the call below `list_active` leaves every other dispatcher test green.
#[tokio::test]
async fn an_unreadable_executor_does_not_stop_the_tick_observing_live_runs() {
    let mut live = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Running);
    live.execution_ref = Some("mock-execution-1".to_owned());
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    executor.fail_list_active("executor unreachable");

    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, live)])))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    let report = fakes.dispatch.run_tick().await;

    assert_eq!(
        report.stopped_at,
        Some("executor listing"),
        "premise: the listing really did stop the tick"
    );
    assert_eq!(
        report.attached, 1,
        "and the run was observed anyway, because observing a run depends on \
         neither the listing nor the cap"
    );
}

// ---------------------------------------------------------------------------
// Dispatch through the product plugin (Task 18)
// ---------------------------------------------------------------------------

/// **The end-to-end ladder assertion Task 18 owes.**
///
/// Task 10 copied `qa-runs`' four frozen variable names onto the plugin's own
/// surface, so until now the property "a variable-tier value is overridden by a
/// run parameter" was still pinned by `runvars`' own test over the platform's
/// copy. Task 18 deleted that copy — the values in the fifth tier are a
/// plugin's now — so the override is asserted here instead, through a real
/// dispatch: a plugin's variable, an environment variable of the same name, and
/// a run parameter, in one spec.
///
/// The name is the double's, not VHP's, and it is not reserved by either side:
/// this test is about the ladder, and `params::validate` is what covers the
/// names that may not be reached this way.
#[tokio::test]
async fn a_run_parameter_overrides_a_plugin_supplied_variable() {
    let mut run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    run.parameters = vec![qa_runs_sdk::RunParameter {
        name: "PRODUCT_ENDPOINT".to_owned(),
        value: "from-parameter".to_owned(),
    }];
    let plugin = Arc::new(fakes::ScriptedPlugin {
        extra_env: vec![("PRODUCT_ENDPOINT".to_owned(), "from-plugin".to_owned())],
        ..fakes::ScriptedPlugin::default()
    });
    let environments = Arc::new(FakeEnvironments::serving(
        fakes::environment_fixture(PLATFORM_A),
        vec![qa_environments_sdk::Variable {
            id: Uuid::from_u128(0x0E11),
            environment_id: Some(PLATFORM_A),
            name: "PRODUCT_ENDPOINT".to_owned(),
            value: "from-variable".to_owned(),
        }],
    ));
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .environments(environments)
        .product_plugins(Arc::new(fakes::FakeProductPlugins::with(plugin)))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    let env = fakes::env_values(&executor.submitted()[0]);
    assert_eq!(
        env.get("PRODUCT_ENDPOINT").map(String::as_str),
        Some("from-parameter"),
        "the per-launch parameter is the most specific value for this run; the \
         plugin's own value beat the environment variable and lost to it"
    );
}

/// The plugin's variable beats an environment variable of the same name — the
/// other half of the ladder, asserted separately so a failure names which rung
/// broke.
#[tokio::test]
async fn a_plugin_variable_overrides_an_environment_variable_end_to_end() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let plugin = Arc::new(fakes::ScriptedPlugin {
        extra_env: vec![("PRODUCT_ENDPOINT".to_owned(), "from-plugin".to_owned())],
        ..fakes::ScriptedPlugin::default()
    });
    let environments = Arc::new(FakeEnvironments::serving(
        fakes::environment_fixture(PLATFORM_A),
        vec![qa_environments_sdk::Variable {
            id: Uuid::from_u128(0x0E11),
            environment_id: Some(PLATFORM_A),
            name: "PRODUCT_ENDPOINT".to_owned(),
            value: "from-variable".to_owned(),
        }],
    ));
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .environments(environments)
        .product_plugins(Arc::new(fakes::FakeProductPlugins::with(plugin)))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    assert_eq!(
        fakes::env_values(&executor.submitted()[0])
            .get("PRODUCT_ENDPOINT")
            .map(String::as_str),
        Some("from-plugin")
    );
}

/// **What the plugin is handed, asserted from the plugin's side.**
///
/// Three properties no other test can see, because the handle exists only for
/// the duration of the call:
///
/// 1. the credential slot is **reference-only** — dispatch resolves no
///    plaintext, which is what `prepare_run_access` is specified around;
/// 2. its reference and key come from the `credentials` column, which Task
///    18b gave a writer — reversing rulings D-19/E-17, which preferred the
///    legacy column precisely *because* nothing wrote this one (ruling F-2).
///    **Task 19 dropped that column and the fallback to it**, so this is the
///    only path now: the key is stored beside the reference, and no product
///    literal is needed in this gear;
/// 4. `config` arrives verbatim (ruling D-9).
#[tokio::test]
async fn the_plugin_is_handed_a_reference_only_slot_keyed_by_its_own_schema() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let plugin = Arc::new(fakes::ScriptedPlugin::default());
    let environment = qa_environments_sdk::Environment {
        credentials: vec![qa_environments_sdk::EnvironmentCredential {
            key: fakes::PLUGIN_SECRET_KEY.to_owned(),
            credstore_ref: "credstore://the-maintained-one".to_owned(),
        }],
        config: serde_json::json!({ "operator_set": "value" }),
        ..fakes::environment_fixture(PLATFORM_A)
    };
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .environments(Arc::new(FakeEnvironments::serving(environment, vec![])))
        .product_plugins(Arc::new(fakes::FakeProductPlugins::with(
            Arc::clone(&plugin) as Arc<dyn qa_product_sdk::QaProductPluginV1>,
        )))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    let handles = plugin.handles.lock().unwrap().clone();
    assert_eq!(handles.len(), 1, "one dispatch, one prepare_run_access");
    let recorded = &handles[0];
    assert_eq!(
        recorded.slots.as_slice(),
        [(
            fakes::PLUGIN_SECRET_KEY.to_owned(),
            "credstore://the-maintained-one".to_owned()
        )],
        "the reference and key come from `credentials`, the column Task 18b \
         gave a writer, not from the pre-plugin single-reference column \
         (ruling F-2)"
    );
    assert_eq!(
        recorded.config,
        serde_json::json!({ "operator_set": "value" })
    );
    assert!(
        !recorded.observed,
        "this fixture has never been observed through the plugin path, and the \
         contract distinguishes that from observed-and-empty"
    );
    // The mount carries the same reference the slot did, which is the only way
    // the executor can resolve it — and the proof that nothing here resolved it
    // first.
    let spec = &executor.submitted()[0];
    let [MountSpec::Secret { credstore_ref, .. }] = spec.access.mounts.as_slice() else {
        panic!("one secret mount")
    };
    assert_eq!(credstore_ref, "credstore://the-maintained-one");
}

// Two tests were deleted here by Task 19, with the column they were about.
//
// `an_environment_written_before_task_18b_falls_back_to_the_legacy_column`
// covered the fallback ruling F-2 dropped along with
// `kubeconfig_credstore_ref`, and
// `a_plugin_with_two_required_credentials_cannot_dispatch_yet` covered
// `AMBIGUOUS_CREDENTIAL`, which that fallback was the only route to. See the
// note where the constant stood in `dispatch_spec.rs` for where the rule went:
// `qa-environments` refuses the same shape at the **write** now, and
// `the_pre_plugin_pair_cannot_bind_to_a_plugin_with_no_sole_required_secret`
// is the test that holds it.
//
// What still holds dispatch's side: `plugin_dispatch` builds one
// `CredentialSlot` per entry in `credentials`, and the golden `RunSpec`
// fixture is byte-identical across this change (E-11).

/// An observed environment reaches the plugin **with** its observation, which is
/// where a detected base URL or namespace comes from.
#[tokio::test]
async fn an_observed_environment_reaches_the_plugin_with_its_attributes() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let plugin = Arc::new(fakes::ScriptedPlugin::default());
    let mut observed_attrs = qa_environments_sdk::ObservedAttrs::default();
    observed_attrs.set("namespace", "somewhere");
    let environment = qa_environments_sdk::Environment {
        observed_attrs,
        ..fakes::environment_fixture(PLATFORM_A)
    };
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .environments(Arc::new(FakeEnvironments::serving(environment, vec![])))
        .product_plugins(Arc::new(fakes::FakeProductPlugins::with(
            Arc::clone(&plugin) as Arc<dyn qa_product_sdk::QaProductPluginV1>,
        )))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    assert!(
        plugin.handles.lock().unwrap()[0].observed,
        "an environment with attributes must arrive as Some, or every detected \
         run variable is silently omitted"
    );
}

/// **The two plugin calls nothing pinned.** Review finding C-1: `runner()` and
/// the plugin's `service_account` reached the spec through code no test could
/// tell from its absence, because every shipped plugin and every double
/// returned exactly the type's default. Cutting both wires left 908 tests and
/// the golden fixture green.
///
/// So `ScriptedPlugin` now declares a runner and an account that are **not**
/// the defaults, and this asserts both arrive. D11 — a runner shape varies per
/// product — has an enforcement point for the first time.
#[tokio::test]
async fn the_plugins_runner_and_service_account_reach_the_spec() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    let spec = &executor.submitted()[0];
    assert_eq!(
        spec.runner.image.as_deref(),
        Some(fakes::PLUGIN_RUNNER_IMAGE),
        "the product's image, not the deployment default: without this, \
         `runner: plugin.runner(observed)` could be replaced by \
         `RunnerSpec::default()` and nothing would notice"
    );
    assert_eq!(spec.runner.command, vec!["/scripted.sh".to_owned()]);
    assert_eq!(spec.runner.image_pull_policy.as_deref(), Some("Always"));
    assert_eq!(
        spec.access.service_account.as_deref(),
        Some(fakes::PLUGIN_SERVICE_ACCOUNT),
        "and the account the plugin asked its pod to assume"
    );
}

// `a_run_whose_environment_names_no_product_cannot_dispatch` was deleted when
// qa-environments' Task 20b made `qa_environments.product_id` NOT NULL. It
// planted an environment with no product and asserted dispatch refused with
// `PluginUnavailable::NoProduct`'s own text rather than the unresolvable-plugin
// one -- a distinction worth pinning while both were reachable from a row.
//
// The row shape is now unrepresentable. `NoProduct` itself still exists on the
// port because qa-catalog's resolver can answer it, and
// `a_run_whose_product_names_an_unregistered_plugin_cannot_dispatch` covers
// the arm that is still reachable from here.

/// A product whose plugin cannot be resolved fails the dispatch the same way,
/// and so does a deployment with no resolver at all. Both are configuration
/// faults with fixed, actionable text.
#[tokio::test]
async fn an_unresolvable_plugin_and_an_absent_resolver_both_fail_the_dispatch() {
    for (reason, expected) in [
        (
            crate::domain::ports::product_plugin::PluginUnavailable::Unresolvable,
            "names no product plugin",
        ),
        (
            crate::domain::ports::product_plugin::PluginUnavailable::ResolverAbsent,
            "resolver is not available",
        ),
    ] {
        let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
        let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
        let fakes = Builder::new()
            .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
            .product_plugins(Arc::new(fakes::FakeProductPlugins::unavailable(reason)))
            .executor(
                Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>
            )
            .build()
            .await;

        let error = fakes
            .dispatch
            .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
            .await
            .expect_err("an unresolvable plugin");
        assert!(
            error.to_string().contains(expected),
            "{reason:?} must say {expected:?}: {error}"
        );
        assert!(executor.submitted().is_empty());
    }
}

/// A plugin that refuses to prepare access fails the dispatch with **its own
/// classified text** — the fixed `&'static str` the contract guarantees, never
/// a formatted value.
#[tokio::test]
async fn a_plugins_refusal_carries_its_classified_detail_onto_the_run() {
    let run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    let plugin = Arc::new(fakes::ScriptedPlugin {
        refuse: Some("this environment has no kubeconfig: add one"),
        ..fakes::ScriptedPlugin::default()
    });
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .product_plugins(Arc::new(fakes::FakeProductPlugins::with(plugin)))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    let error = fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .expect_err("the plugin refused");
    assert!(
        error
            .to_string()
            .contains("this environment has no kubeconfig"),
        "the plugin's own classified detail is what an operator needs: {error}"
    );
}

/// **The union, end to end.** A run parameter the *plugin* reserves — and the
/// platform does not — is refused at dispatch, which is the only place the
/// product's own names are knowable.
///
/// Launch accepted it: `params::validate` runs there against the platform floor
/// alone, before any I/O. That split is `params`' fourth composition
/// obligation, and this is the half of it that only a dispatch can show.
#[tokio::test]
async fn a_parameter_the_plugin_reserves_is_refused_at_dispatch() {
    let mut run = run_fixture(RUN_1, Some(PLATFORM_A), false, RunState::Dispatching);
    run.parameters = vec![qa_runs_sdk::RunParameter {
        name: "PRODUCT_ENDPOINT".to_owned(),
        value: "hijacked".to_owned(),
    }];
    // The floor really does admit it, which is what makes the dispatch check
    // load-bearing rather than a second copy of the launch check.
    crate::domain::params::validate(
        &run.parameters,
        &qa_product_sdk::access::RunVarContract::default(),
    )
    .expect("premise: the platform floor does not reserve this name");

    let plugin = Arc::new(fakes::ScriptedPlugin {
        reserved: vec!["PRODUCT_ENDPOINT".to_owned()],
        ..fakes::ScriptedPlugin::default()
    });
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .product_plugins(Arc::new(fakes::FakeProductPlugins::with(plugin)))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    let error = fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .expect_err("the plugin reserves that name");
    assert!(
        error.to_string().contains("PRODUCT_ENDPOINT"),
        "the message names the offending parameter: {error}"
    );
    assert!(executor.submitted().is_empty());
}

/// A platformless run resolves **no plugin at all** — no environment read, no
/// product, no resolver call — which is what the source system does in that
/// case (`manager/src/services/argo.rs:489-492`).
#[tokio::test]
async fn a_platformless_run_resolves_no_plugin() {
    let run = run_fixture(RUN_1, None, false, RunState::Dispatching);
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    assert!(
        fakes.product_plugins.resolved.lock().unwrap().is_empty(),
        "nothing to resolve a plugin from, and nothing tried"
    );
    let spec = &executor.submitted()[0];
    assert!(spec.access.mounts.is_empty());
    assert!(!fakes::env_values(spec).contains_key("KUBECONFIG"));
}

// ---------------------------------------------------------------------------
// The `Collect` run kind, at dispatch
// ---------------------------------------------------------------------------

/// A collect run reaches the executor as a **collection**, not as an execution.
///
/// This is the guard for the one collect site the compiler cannot force.
/// `resolve_groups`' `match` on `run.target` is exhaustive, so its collect arm
/// could not be forgotten; `build_spec`'s is an `if let`, so it could — and
/// forgetting it produces a run that carries a full `TEST_FILES` node list and
/// no `COLLECT_ONLY`, i.e. one that **executes every test in the repository**
/// instead of counting them. That is not a degraded collect, it is the most
/// expensive possible wrong answer.
///
/// Break-verified: deleting the `if let RunTarget::Collect` block in
/// `build_spec` turns exactly this test red and leaves the rest of the suite
/// green.
#[tokio::test]
async fn a_collect_dispatch_sets_the_two_frozen_variables() {
    let mut run = run_fixture(RUN_1, None, false, RunState::Dispatching);
    run.target = qa_runs_sdk::RunTarget::Collect {
        repo_id: fakes::REPO,
        collect_url: "https://insights.example/qa/v1/collect/r/main".to_owned(),
    };
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    let env = fakes::env_values(&executor.submitted()[0]);
    assert_eq!(
        env.get("COLLECT_ONLY").map(String::as_str),
        Some("true"),
        "without this the runner executes the tests it was asked to count"
    );
    assert_eq!(
        env.get("VHP_COLLECT_URL").map(String::as_str),
        Some("https://insights.example/qa/v1/collect/r/main")
    );
}

/// A collect run's file set is the **union of every plan on the branch**, which
/// is how legacy builds its collect bundle
/// (`manager/src/services/collect.rs:56-75`: *"Files = union of every plan's
/// test files on this branch (matches the analytics universe)"*). It reads
/// `list_plans`, never `get_plan` — there is no named `plan.yaml` to read.
#[tokio::test]
async fn a_collect_dispatch_bundles_every_plan_on_the_branch() {
    let mut run = run_fixture(RUN_1, None, false, RunState::Dispatching);
    run.target = qa_runs_sdk::RunTarget::Collect {
        repo_id: fakes::REPO,
        collect_url: "https://insights.example/qa/v1/collect/r/main".to_owned(),
    };
    let executor = Arc::new(crate::infra::executor::mock::MockRunExecutor::new());
    let catalog = Arc::new(FakeCatalog::serving(&["tests/test_smoke.py"]));
    let fakes = Builder::new()
        .runs(Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])))
        .catalog(Arc::clone(&catalog))
        .executor(Arc::clone(&executor) as Arc<dyn crate::domain::ports::run_executor::RunExecutor>)
        .build()
        .await;

    fakes
        .dispatch
        .dispatch_one(&ctx(OWNER_TENANT), RUN_1, None)
        .await
        .unwrap();

    assert_eq!(
        catalog.listed_branches.lock().unwrap().clone(),
        vec![(fakes::REPO, "main".to_owned())],
        "the branch enumerated is the one the run recorded as its test_version"
    );
    let spec = &executor.submitted()[0];
    assert_eq!(spec.nodes.len(), 1, "one repository, one node");
    assert_eq!(
        spec.nodes[0].test_files,
        vec![
            "tests/test_smoke.py".to_owned(),
            "tests/test_extra.py".to_owned(),
        ],
        "both plans on the branch contribute, not just the first"
    );
}

// ---------------------------------------------------------------------------
// Nil-tenant enumeration is elevated, not authorized
// ---------------------------------------------------------------------------

/// A tick's six nil-tenant enumerating reads do not consult the policy engine.
///
/// This is the property that lets `static-authz-plugin` stay stock. If any of
/// them reached the PEP with a nil-tenant context the stock plugin denies it,
/// the pass silently finds no tenants, and its work is never done -- a failure
/// with no error anywhere, which is why it is asserted rather than assumed. A
/// bare `run_tick()` reaches all six sites this task changed (`ttl_sweep`,
/// `timeout_sweep`, `reattach_watchers`, `reconcile_claims`, `evaluate_cap`,
/// `queued_platforms`) with no fixture rows needed, since each enumerates
/// against an empty repository without erroring.
#[tokio::test]
async fn the_ttl_sweep_does_not_consult_the_policy_engine() {
    let enforcer = Arc::new(RecordingAuthZ::new());
    let fakes = Builder::new().authz(enforcer.clone()).build().await;

    fakes.dispatch.run_tick().await;

    assert!(
        !enforcer.requested_any_for_nil_tenant(),
        "the sweep must elevate through domain::elevated, not the PEP: the stock \
         static-authz plugin denies every nil-tenant request"
    );
}
