//! The three consequences of two ingest producers meeting on one run, against
//! real Postgres.
//!
//! # What makes a second producer real
//!
//! [`IngestService::ingest`] drains one stream sequentially, and
//! `service::watch`'s registry keeps one such stream per run **per process**.
//! The second producer is therefore a second *process* observing the same run,
//! which the shipped `NoopLeaderElector` permits because it makes every replica
//! the dispatcher. From the database's side that is two connections running two
//! transactions, which is what these tests create: separate tasks on a pool
//! wide enough that they genuinely overlap.
//!
//! # The three consequences
//!
//! 1. duplicate per-test rows -- uniqueness on
//!    `(tenant_id, run_id, test_file, test_name)` is an application invariant
//!    held by delete-then-insert *inside one transaction*;
//! 2. the counter double-count -- two producers both reading `previous = None`
//!    both computing `+1`;
//! 3. the completion's phantom read -- a result committing between
//!    [`IngestService::finish`]'s tally and its terminal write, leaving a
//!    `Succeeded` run with a `FAILED` row stored.
//!
//! The first two cover 1 and 2, which share one mechanism -- one at the
//! two-replica contention this analysis is about, one at eight-way contention.
//! The third covers 3. **How many producers exhaust the retry budget at
//! eight-way is not pinned anywhere**, deliberately: it is a timing property
//! that moved from 4 of 8 to 2 of 8 when the image was pinned to
//! `postgres:15-alpine`, so the second test asserts only what holds at any
//! number, 0 included.
//!
//! A fourth test covers step 3's question about the same completion: whether the
//! counter *repair* can be left skewed by a producer committing beside it. It
//! can, without the escalation -- measured, not argued, and the mechanism is not
//! the one the step named. `reconcile_counts` carries it.
//!
//! # These tests do not pass either way
//!
//! All four were run against this tree with the `SERIALIZABLE` escalation
//! removed -- leaving the retry loop and every call site untouched, so only the
//! isolation level changed -- and all four fail there, on the pinned
//! `postgres:15-alpine`. Earlier rounds measured the same thing on
//! `postgres:11-alpine`, before the image was pinned; the conclusions matched,
//! but those runs were not measurements of the shipped image.
//! `infra::storage::isolation_pg_tests` characterises the same primitive
//! directly.
//!
//! # What is deliberately not covered
//!
//! The run below carries no platform, so `finish`'s release path returns before
//! it reaches the qa-environments client or the queue. Lease and claim release
//! under contention is a different race and is not this step's.

use std::sync::Arc;
use std::time::Duration;

use qa_runs_sdk::{RunResult, RunState};
use toolkit_db::DBProvider;
use uuid::Uuid;

use super::SerializedDb;
use super::ingest::{IngestDeps, IngestService};
use super::test_support::{OWNER_TENANT, PermissiveAuthZ, ctx};
use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::{ExecutionEvent, NodeOutcome, TestObservation};
use crate::domain::repos::{RunLogsRepository, RunsRepository, TestResultRow};
use crate::domain::service::LogArchive;
use crate::domain::state_machine::ExecutorOutcome;
use crate::infra::logs::{RunLogArchive, RunLogBroadcaster};
use crate::infra::storage::test_db::{PgHarness, pg_db, sample_new_run, scope};
use crate::infra::storage::{OrmQueueRepository, OrmRunsRepository};
use authz_resolver_sdk::PolicyEnforcer;
use toolkit_security::{AccessScope, SecurityContext};

/// Every method panics, which is itself the assertion: a platformless run must
/// not reach the environments client. If one fires, the release path changed
/// and these tests no longer test what they say.
struct UnreachableEnvironments;

#[async_trait::async_trait]
impl qa_environments_sdk::QaEnvironmentsClientV1 for UnreachableEnvironments {
    async fn get_environment(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<qa_environments_sdk::Environment, qa_environments_sdk::QaEnvironmentsError> {
        unreachable!("a platformless run must not reach the environments client")
    }
    async fn list_environments(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<qa_environments_sdk::Environment>, qa_environments_sdk::QaEnvironmentsError>
    {
        unreachable!("a platformless run must not reach the environments client")
    }
    async fn create_environment(
        &self,
        _ctx: &SecurityContext,
        _new: qa_environments_sdk::NewEnvironment,
    ) -> Result<qa_environments_sdk::Environment, qa_environments_sdk::QaEnvironmentsError> {
        unreachable!("a platformless run must not reach the environments client")
    }
    async fn update_environment(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _patch: qa_environments_sdk::EnvironmentPatch,
    ) -> Result<qa_environments_sdk::Environment, qa_environments_sdk::QaEnvironmentsError> {
        unreachable!("a platformless run must not reach the environments client")
    }
    async fn delete_environment(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), qa_environments_sdk::QaEnvironmentsError> {
        unreachable!("a platformless run must not reach the environments client")
    }
    async fn list_variables(
        &self,
        _ctx: &SecurityContext,
        _environment_id: Option<Uuid>,
    ) -> Result<Vec<qa_environments_sdk::Variable>, qa_environments_sdk::QaEnvironmentsError> {
        unreachable!("a platformless run must not reach the environments client")
    }
    async fn upsert_variable(
        &self,
        _ctx: &SecurityContext,
        _var: qa_environments_sdk::NewVariable,
    ) -> Result<qa_environments_sdk::Variable, qa_environments_sdk::QaEnvironmentsError> {
        unreachable!("a platformless run must not reach the environments client")
    }
    async fn delete_variable(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), qa_environments_sdk::QaEnvironmentsError> {
        unreachable!("a platformless run must not reach the environments client")
    }
    async fn acquire_lease(
        &self,
        _ctx: &SecurityContext,
        _environment_id: Uuid,
        _run_id: Uuid,
        _mode: qa_environments_sdk::LeaseMode,
    ) -> Result<qa_environments_sdk::AcquireOutcome, qa_environments_sdk::QaEnvironmentsError> {
        unreachable!("a platformless run must not reach the environments client")
    }
    async fn release_lease(
        &self,
        _ctx: &SecurityContext,
        _environment_id: Uuid,
        _run_id: Uuid,
    ) -> Result<qa_environments_sdk::LeaseState, qa_environments_sdk::QaEnvironmentsError> {
        unreachable!("a platformless run must not reach the environments client")
    }
    async fn get_lease(
        &self,
        _ctx: &SecurityContext,
        _environment_id: Uuid,
    ) -> Result<qa_environments_sdk::LeaseState, qa_environments_sdk::QaEnvironmentsError> {
        unreachable!("a platformless run must not reach the environments client")
    }
}

type Ingest = IngestService<OrmRunsRepository, OrmQueueRepository>;

struct Fixture {
    _harness: PgHarness,
    provider: Arc<DBProvider<DomainError>>,
    ingest: Arc<Ingest>,
    repo: Arc<OrmRunsRepository>,
    /// **The same instance `ingest` records into**, kept concretely so a test
    /// can drive `flush` itself. This is the only place on the branch where
    /// the production archive shape — `RunLogArchive` over
    /// `OrmRunsRepository` over a real database — exists, so it is also the
    /// only place `append_log` can be executed against the shipped dialect
    /// before a deploy does it.
    archive: Arc<RunLogArchive<OrmRunsRepository>>,
    scope: AccessScope,
    run_id: Uuid,
}

/// A `Running`, platformless run and an [`IngestService`] wired to the real
/// repositories over the container.
async fn fixture() -> Fixture {
    let harness = pg_db().await;
    let provider = Arc::new(DBProvider::<DomainError>::new(harness.db.clone()));
    let scope = scope(OWNER_TENANT);
    let repo = Arc::new(OrmRunsRepository);

    let run = {
        let conn = provider.conn().unwrap();
        let mut new = sample_new_run("smoke-1");
        // Platformless, so the completion's release path returns before it
        // reaches the environments client -- see the module header.
        new.environment_id = None;
        new.state = RunState::Running;
        repo.create(&conn, &scope, OWNER_TENANT, new)
            .await
            .expect("the run inserts")
    };

    let enforcer = PolicyEnforcer::new(Arc::new(PermissiveAuthZ));
    // A real archive, over the same repository and provider as `ingest`
    // itself — the production shape, and available here (unlike the unit
    // tier's `MockRunsRepository`-backed fixtures) because `OrmRunsRepository`
    // implements `RunLogsRepository` too.
    let archive = Arc::new(RunLogArchive::new(
        Arc::clone(&provider),
        Arc::clone(&repo),
        enforcer.clone(),
    ));
    let ingest = Arc::new(IngestService::new(IngestDeps {
        db: SerializedDb::new(Arc::clone(&provider)),
        runs: Arc::clone(&repo),
        queue: Arc::new(OrmQueueRepository),
        environments: Arc::new(UnreachableEnvironments),
        logs: Arc::new(RunLogBroadcaster::new(8)),
        archive: Arc::clone(&archive) as Arc<dyn LogArchive>,
        policy_enforcer: enforcer,
        // This tier is about the isolation level, not about telemetry: the
        // no-op port emits everything a wired one does and lets nothing
        // observe it.
        metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
    }));

    Fixture {
        _harness: harness,
        provider,
        ingest,
        repo,
        archive,
        scope,
        run_id: run.id,
    }
}

impl Fixture {
    /// Another `Running`, platformless run on the same container, so a test can
    /// repeat a race on fresh state without paying for a second container.
    async fn another_run(&self, name: &str) -> Uuid {
        let conn = self.provider.conn().unwrap();
        let mut new = sample_new_run(name);
        new.environment_id = None;
        new.state = RunState::Running;
        self.repo
            .create(&conn, &self.scope, OWNER_TENANT, new)
            .await
            .expect("the run inserts")
            .id
    }
}

fn observation(test_name: &str, status: &str) -> TestObservation {
    TestObservation {
        node: "repo-smoke".to_owned(),
        test_file: "tests/test_smoke.py".to_owned(),
        test_name: test_name.to_owned(),
        status: status.to_owned(),
        duration: None,
        launch_id: None,
        jira_key: None,
        nodeid: None,
        reason: None,
        ticket: None,
    }
}

/// Drive `PRODUCERS` concurrent reports of the *same* test and read back what
/// was stored.
///
/// Returns `(successes, rows, counts)` so each caller can assert the part of
/// the contract it is about.
async fn race_one_test(f: &Fixture, producers: usize) -> (usize, Vec<TestResultRow>, RunResult) {
    let barrier = Arc::new(tokio::sync::Barrier::new(producers));
    let mut handles = Vec::with_capacity(producers);
    for _ in 0..producers {
        let ingest = Arc::clone(&f.ingest);
        let barrier = Arc::clone(&barrier);
        let ctx = ctx(OWNER_TENANT);
        let run_id = f.run_id;
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            ingest
                .apply(
                    &ctx,
                    run_id,
                    ExecutionEvent::TestResult(observation("test_login", "PASSED")),
                )
                .await
        }));
    }

    let mut successes = 0_usize;
    for handle in handles {
        if handle.await.expect("task joins").is_ok() {
            successes += 1;
        }
    }

    let conn = f.provider.conn().unwrap();
    let owned = f
        .repo
        .resolve_owned(&conn, &f.scope, f.run_id)
        .await
        .unwrap();
    let rows = f
        .repo
        .list_test_results(&conn, &f.scope, owned)
        .await
        .unwrap();
    let counts = f
        .repo
        .get_result(&conn, &f.scope, f.run_id)
        .await
        .unwrap()
        .expect("the counter row is readable");
    (successes, rows, counts)
}

/// Two producers report the same test at once — the deployment this analysis is
/// about, where a second replica is all it takes. Both must succeed, and one
/// logical test must be one stored row and one count.
///
/// Under the un-configured transaction this fails on both counts at once: the
/// tuple lands twice and `total` reaches 2. Under `SERIALIZABLE` the losing
/// transaction aborts with `40001` and is retried, and the retry re-reads a
/// `previous` that now exists — so its delta is zero and its upsert replaces
/// rather than adds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_producers_of_one_test_leave_one_row_and_one_count() {
    let f = fixture().await;
    let (successes, rows, counts) = race_one_test(&f, 2).await;

    assert_eq!(
        successes, 2,
        "at two-way contention the retry budget must absorb the abort rather \
         than surfacing it: a surfaced error stops the whole observation, \
         including the Finished event that has not arrived yet",
    );
    assert_eq!(
        rows.len(),
        1,
        "one logical test must be one stored row; got {rows:#?}",
    );
    assert_eq!(counts.total, 1, "one logical test must be counted once");
    assert_eq!(counts.passed, 1, "the passed counter must match the rows");
}

/// **The limit of this closure, exercised rather than left to be discovered.**
///
/// The retry budget is bounded ([`toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS`]), so
/// contention heavy enough to exhaust it surfaces a `Database` error to the
/// caller instead of retrying further. This was **measured, not predicted**:
/// at eight-way contention some producers do exhaust it, and the error text
/// that comes back is Postgres's own *"could not serialize access due to
/// concurrent update"*.
///
/// That is a real cost and it is the honest statement of what was bought.
/// Ingest's caller is a watch-stream drain with nobody to answer, so an
/// exhausted budget means that result is **dropped** — the outcome this module
/// exists to prevent. What `SERIALIZABLE` changes is *which* failure happens:
/// a loud, attributable one instead of a silently corrupt row set.
///
/// So the invariant this pins is not "everybody wins" -- and it does not assert
/// that anybody loses either, because how many producers exhaust the budget is a
/// timing property and pinning a number would make this flaky. What it pins is
/// that **the stored state is never inconsistent**, whether the budget held or not: never a
/// duplicate row, and counters that always equal the rows they are supposed to
/// summarise. Under the un-configured transaction all eight "succeed" and the
/// stored state is wrong, which is the trade this test exists to record.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn heavy_contention_may_exhaust_the_budget_but_never_corrupts_what_is_stored() {
    let f = fixture().await;
    let (successes, rows, counts) = race_one_test(&f, 8).await;

    assert!(successes >= 1, "at least one producer must have committed");
    assert_eq!(
        rows.len(),
        1,
        "however many producers won, one logical test is one row; got {rows:#?}",
    );
    assert_eq!(
        counts.total,
        rows.len(),
        "the counters must equal the rows they summarise, not the number of \
         producers that ran",
    );
    assert_eq!(counts.passed, 1, "the passed counter must match the rows");
}

/// A failing result and the completion race each other. The run must never end
/// up `Succeeded` while a `FAILED` row is stored.
///
/// Both orderings are legitimate and the assertion accepts either:
///
/// * the result lands first, so `finish` tallies it and derives `Failed`; or
/// * `finish` lands first, so the run is terminal and the late result is
///   dropped by the drop guard — leaving no `FAILED` row to contradict.
///
/// What must not happen is the third: a tally taken before the row, a verdict
/// derived from it, and the row committing into the gap. That is the phantom
/// read.
///
/// # The run is seeded with a passing result first, and that is load-bearing
///
/// **Measured, and it falsified an earlier version of this test.** With no
/// prior result the tally is empty, and `apply_completion_guard` already
/// refuses to call a `total == 0` run `Succeeded` — so the verdict came out
/// `Failed` whether or not the phantom read happened, and the test passed with
/// `SERIALIZABLE` removed. The guard was masking the very thing being tested.
///
/// Seeding one `PASSED` row gives the completion a tally it will happily call
/// `Succeeded`, so a `FAILED` row committing into the gap is observable.
///
/// # Why this sweeps instead of racing once
///
/// The window is between `finish`'s tally and its terminal write. The producer
/// has to commit inside it, which means starting earlier than the completion,
/// so the completion's start is staggered across the window on a fresh run each
/// time. The bounds were chosen by measuring the mutant.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_result_committing_beside_the_completion_never_leaves_a_lying_verdict() {
    const WINDOW_STEPS: u64 = 60;
    const STEP: Duration = Duration::from_micros(50);

    let f = fixture().await;

    for step in 0..WINDOW_STEPS {
        let run_id = f.another_run(&format!("phantom-{step}")).await;

        // The tally the completion will derive `Succeeded` from. Without it the
        // zero-count guard decides the verdict instead -- see above.
        f.ingest
            .apply(
                &ctx(OWNER_TENANT),
                run_id,
                ExecutionEvent::TestResult(observation("test_login", "PASSED")),
            )
            .await
            .expect("the seeded result records");

        let delay = STEP * u32::try_from(step).unwrap();

        let producer = {
            let ingest = Arc::clone(&f.ingest);
            let ctx = ctx(OWNER_TENANT);
            tokio::spawn(async move {
                ingest
                    .apply(
                        &ctx,
                        run_id,
                        ExecutionEvent::TestResult(observation("test_checkout", "FAILED")),
                    )
                    .await
            })
        };

        let completer = {
            let ingest = Arc::clone(&f.ingest);
            let ctx = ctx(OWNER_TENANT);
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                ingest
                    .apply(
                        &ctx,
                        run_id,
                        ExecutionEvent::Finished {
                            outcome: ExecutorOutcome::Succeeded,
                            nodes: NodeOutcome::NoneFailed,
                            message: None,
                        },
                    )
                    .await
            })
        };

        // Either side may legitimately exhaust the retry budget under
        // contention -- `heavy_contention_may_exhaust_the_budget_but_never_corrupts_what_is_stored`
        // records why. What must hold is the stored state, checked regardless
        // of who returned what.
        let _produced = producer.await.expect("task joins");
        let _completed = completer.await.expect("task joins");

        let conn = f.provider.conn().unwrap();
        let run = f
            .repo
            .get(&conn, &f.scope, run_id)
            .await
            .unwrap()
            .expect("the run is readable");
        let owned = f.repo.resolve_owned(&conn, &f.scope, run_id).await.unwrap();
        let rows = f
            .repo
            .list_test_results(&conn, &f.scope, owned)
            .await
            .unwrap();

        let has_failed_row = rows.iter().any(|row| row.status == "FAILED");
        assert!(
            !(run.state == RunState::Succeeded && has_failed_row),
            "step {step}: a Succeeded run must not have a FAILED row stored -- \
             the completion's tally missed a result that then committed. \
             state={:?} rows={rows:#?}",
            run.state,
        );
    }
}

/// **The counters the completion repairs converge, and the escalation is what
/// makes them.**
///
/// `reconcile_counts` reads `stored`, tallies the rows, and applies the
/// difference. Measured both ways: green on this tree, and **red with the
/// `SERIALIZABLE` escalation removed** — counters `(3, 3)` against two rows.
///
/// The mechanism is not the one the step was written around, and the difference
/// matters for anyone reading the code. The correcting write is a *delta*
/// (`add_result_counts` builds `col = col + n` server-side), so a producer's
/// increment landing between the read and the write is **preserved** by the
/// correction rather than overwritten — that half of a read-modify-write is
/// already safe. What `READ COMMITTED` breaks is the pair of *reads*: each
/// statement takes a fresh snapshot, so `get_result` can miss a producer's
/// increment that `list_test_results` then sees, making the delta one too small
/// against the rows the producer left. A snapshot that holds across both reads is
/// what closes it, and that is what the escalation buys.
///
/// So the columns are convergent at the completion's commit, not best-effort —
/// `RunsRepository::get_result` states it where a reader of the columns will see
/// it.
///
/// # The seeded skew is load-bearing
///
/// Without it `stored == tallied` and `reconcile_counts` returns before the
/// correcting write, so the branch under test never runs. The skew is injected
/// through the repository rather than by corrupting a row, because that is the
/// state a real skew leaves: counters ahead of the rows.
///
/// # A step whose completion errored is skipped, and the skips are counted
///
/// A completion that exhausted its retry budget never reached the repair, so its
/// step has nothing to say about one. But the skip catches *every* error kind,
/// not only exhaustion, so a change that made every completion fail would leave
/// this green having asserted nothing across all forty steps. The count is
/// therefore checked at the end. It asserts 40 of 40 today, so the guard is
/// against a future regression rather than a present one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_completions_counter_repair_survives_a_producer_committing_beside_it() {
    const WINDOW_STEPS: u64 = 40;
    const STEP: Duration = Duration::from_micros(50);

    let mut asserted = 0_u64;
    let f = fixture().await;

    for step in 0..WINDOW_STEPS {
        let run_id = f.another_run(&format!("counters-{step}")).await;

        f.ingest
            .apply(
                &ctx(OWNER_TENANT),
                run_id,
                ExecutionEvent::TestResult(observation("test_login", "PASSED")),
            )
            .await
            .expect("the seeded result records");

        // The skew the completion has to repair: counters ahead of the rows.
        {
            let conn = f.provider.conn().unwrap();
            f.repo
                .add_result_counts(
                    &conn,
                    &f.scope,
                    run_id,
                    crate::domain::repos::RunResultDelta {
                        passed: 2,
                        failed: 0,
                        skipped: 0,
                        in_progress: 0,
                        xfail: 0,
                        xpass: 0,
                        total: 2,
                    },
                )
                .await
                .expect("the skew is injected");
        }

        let delay = STEP * u32::try_from(step).unwrap();

        let producer = {
            let ingest = Arc::clone(&f.ingest);
            let ctx = ctx(OWNER_TENANT);
            tokio::spawn(async move {
                ingest
                    .apply(
                        &ctx,
                        run_id,
                        ExecutionEvent::TestResult(observation("test_checkout", "PASSED")),
                    )
                    .await
            })
        };

        let completer = {
            let ingest = Arc::clone(&f.ingest);
            let ctx = ctx(OWNER_TENANT);
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                ingest
                    .apply(
                        &ctx,
                        run_id,
                        ExecutionEvent::Finished {
                            outcome: ExecutorOutcome::Succeeded,
                            nodes: NodeOutcome::NoneFailed,
                            message: None,
                        },
                    )
                    .await
            })
        };

        let _produced = producer.await.expect("task joins");
        let completion = completer.await.expect("task joins");

        let conn = f.provider.conn().unwrap();
        let owned = f.repo.resolve_owned(&conn, &f.scope, run_id).await.unwrap();
        let rows = f
            .repo
            .list_test_results(&conn, &f.scope, owned)
            .await
            .unwrap();
        let counts = f
            .repo
            .get_result(&conn, &f.scope, run_id)
            .await
            .unwrap()
            .expect("the counter row is readable");

        // A completion that exhausted its retry budget never reached the repair,
        // so the skew it was supposed to fix is still there. That is the
        // documented cost of the bounded budget, not a counter-example to this
        // test's claim -- assert only when the repair actually ran, and count
        // the times it did.
        if completion.is_err() {
            continue;
        }
        asserted += 1;

        let passed = rows.iter().filter(|row| row.status == "PASSED").count();
        assert_eq!(
            (counts.total, counts.passed),
            (rows.len(), passed),
            "step {step}: the repaired counters must equal the rows they \
             summarise. counts={counts:?} rows={rows:#?}",
        );
    }

    assert!(
        asserted > 0,
        "every completion errored, so the repair this test is about never ran \
         once in {WINDOW_STEPS} steps -- green here would mean nothing was checked",
    );
}

// ---------------------------------------------------------------------------
// The archived log, against real Postgres
// ---------------------------------------------------------------------------
//
// **Why these live in this file.** Design §6 puts items 3, 5, 7, 8 and 10 on
// the Postgres tier, and none of them landed there: every archive test on the
// branch ran either against `SQLite` (`infra::storage::run_logs_sea_repo`) or
// against an in-memory double (`infra::logs::archive`,
// `domain::service::ingest_tests`). This fixture is the **only** place the
// production shape exists — a real `RunLogArchive` over the real
// `OrmRunsRepository` over a real database — and it asserted nothing about
// `qa_run_logs` at all, which meant a deploy would be the first and only time
// `append_log` ever executed against Postgres.
//
// Three things are dialect-specific enough to be worth the container:
//
// * `CONCAT(text, $1)` — chosen over `||` precisely because it renders
//   identically on every builder, which is an argument that wants executing on
//   more than one engine (`run_logs_sea_repo`'s module doc makes the claim and
//   verified it on `SQLite` only);
// * the composite foreign key onto `qa_runs(id, tenant_id)` and the unique
//   **index** it references — Postgres resolves a foreign key's parent key
//   against `pg_index` rather than requiring a named `UNIQUE` constraint, and
//   that reasoning had been verified by argument and by `SQLite`, not by
//   Postgres;
// * `for_log_archive`'s system context reaching a scoped `UPDATE` and a
//   `secure_insert` for real.

/// **The write path and the read path, composed, against Postgres.**
///
/// Ingest lines through the real `IngestService`, flush through the real
/// `RunLogArchive`, read back through `RunLogsRepository::get_log`, and assert
/// the two things the archive promises: the stored bytes are the prefixed
/// bytes a live subscriber saw, and `lines` agrees with
/// `text.lines().count()`.
///
/// **That last equality is the whole invariant `fan_out_log`'s newline
/// flattening exists to protect** (`record` counts one call as one line, so
/// any embedded terminator would make the column disagree with its own text),
/// and this is the only place it is checked against a real row rather than
/// against a double's `String`.
///
/// The second `flush` covers design §6 item 3 on this tier: a drained buffer
/// must not be re-appended, so a doubled flush leaves the row once with each
/// line once.
///
/// **Break-tested**, both assertions, against the container:
/// * `record`'s `entry.lines += 1` changed to `+= 2` — the `lines` equality
///   goes red at 6 against 3, which is exactly the disagreement an unflattened
///   embedded newline would produce;
/// * `flush`'s `Ok` arm made to `restore` the taken buffer as well as report
///   success — the second flush then re-appends and `text` comes back doubled.
#[tokio::test]
async fn an_ingested_log_round_trips_through_qa_run_logs() {
    let f = fixture().await;
    let ctx = ctx(OWNER_TENANT);

    for line in ["collected 3 items", "PASS | test_login", "1 passed"] {
        f.ingest
            .apply(
                &ctx,
                f.run_id,
                ExecutionEvent::Log {
                    node: "repo-smoke".to_owned(),
                    line: line.to_owned(),
                    emitted_at: None,
                },
            )
            .await
            .expect("a log event must be accepted");
    }

    f.archive
        .flush(f.run_id)
        .await
        .expect("the flush must reach Postgres");

    let conn = f.provider.conn().unwrap();
    let stored = f
        .repo
        .get_log(&conn, &f.scope, f.run_id)
        .await
        .expect("the read must not error")
        .expect("the row must exist after a flush");

    assert_eq!(
        stored.text,
        "[repo-smoke] collected 3 items\n[repo-smoke] PASS | test_login\n[repo-smoke] 1 passed\n",
        "the archive must hold the prefixed lines, in order, one newline each",
    );
    assert_eq!(
        usize::try_from(stored.lines).unwrap(),
        stored.text.lines().count(),
        "qa_run_logs.lines must agree with its own text, or a consumer reading \
         the cheap count gets a different answer from one reading the log",
    );

    // Design 6.3 on this tier: a second flush with nothing recorded between
    // must not re-append the drained text.
    f.archive.flush(f.run_id).await.expect("a no-op flush");
    let reread = f
        .repo
        .get_log(&conn, &f.scope, f.run_id)
        .await
        .unwrap()
        .expect("the row is still there");
    assert_eq!(
        reread.text, stored.text,
        "a drained buffer must not re-append"
    );
    assert_eq!(reread.lines, stored.lines);
}

/// **Tenant isolation on `qa_run_logs`, against Postgres** — design §6 item 8,
/// and the finding the whole-branch review raised as I-1.
///
/// Before the composite foreign key, a foreign tenant could create a run's log
/// row on its **first** append and poison it permanently: the scoped `UPDATE`
/// matched nothing, `secure_insert`'s `validate_insert_scope` compared the
/// `ActiveModel`'s own `tenant_id` against the caller's scope — which the
/// caller supplied — and nothing tied `qa_run_logs.tenant_id` to
/// `qa_runs.tenant_id`.
///
/// `infra::storage::run_logs_sea_repo`'s `a_foreign_scoped_append_is_refused`
/// pins the same property on `SQLite`. This runs it on the **shipped** dialect,
/// which is where the reasoning had a gap worth closing: the constraint
/// references a plain `CREATE UNIQUE INDEX` on `qa_runs(id, tenant_id)` rather
/// than a named `UNIQUE` constraint, and "Postgres accepts a unique index as a
/// foreign key's parent key" was an argument about `pg_index` until this test
/// executed it. If that were wrong, the migration's `CREATE TABLE` would fail
/// outright and `fixture()` would never return — so every test in this file is
/// now also a check on it.
#[tokio::test]
async fn a_foreign_tenant_can_neither_read_nor_create_a_log_row() {
    let f = fixture().await;
    let ctx = ctx(OWNER_TENANT);
    f.ingest
        .apply(
            &ctx,
            f.run_id,
            ExecutionEvent::Log {
                node: "repo-smoke".to_owned(),
                line: "secret".to_owned(),
                emitted_at: None,
            },
        )
        .await
        .unwrap();
    f.archive.flush(f.run_id).await.unwrap();

    let stranger_tenant = Uuid::from_u128(0x5747);
    let stranger = scope(stranger_tenant);
    let conn = f.provider.conn().unwrap();

    assert!(
        f.repo
            .get_log(&conn, &stranger, f.run_id)
            .await
            .unwrap()
            .is_none(),
        "another tenant's scope must not see this run's log",
    );

    // A second run, so the poisoning attempt is against a row that does not
    // exist yet — which is the only moment the defect was reachable.
    let fresh = f.another_run("smoke-2").await;
    assert!(
        f.repo
            .append_log(&conn, &stranger, fresh, stranger_tenant, "[x] poison\n", 1)
            .await
            .is_err(),
        "a stranger must not be able to create another tenant's run's log row",
    );
    f.repo
        .append_log(&conn, &f.scope, fresh, OWNER_TENANT, "[x] mine\n", 1)
        .await
        .expect("and the owner's first append must still succeed");
    assert_eq!(
        f.repo
            .get_log(&conn, &f.scope, fresh)
            .await
            .unwrap()
            .expect("the owner's row exists")
            .text,
        "[x] mine\n",
    );
}
