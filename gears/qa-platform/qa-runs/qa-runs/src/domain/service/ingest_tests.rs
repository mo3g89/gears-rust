//! Unit tests for result ingestion.
//!
//! The doubles are `admission_tests::fakes`, reused rather than re-declared —
//! that module's header gives the reason, and it applies with more force here:
//! this task's five newly-implemented `FakeRuns` methods model the same counters
//! the real repository clamps and the same status column the real repository
//! truncates, and a second copy would drift on exactly those two rules.
//!
//! Every expected counter value is read from the migration's
//! `qa_run_test_results.status` column comment, which is the definitive record of
//! the eight-value vocabulary and its mapping onto the five counters, and from
//! `../testrunner/manager/src/routes/plans.rs:188-192`, which that comment ports.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use qa_environments_sdk::{LeaseState, QaEnvironmentsClientV1};
use qa_runs_sdk::{QueueState, RunKind, RunResult, RunState};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::*;
use crate::domain::metrics::{QA_RUNS_INGEST, QA_RUNS_INGEST_DURATION};
use crate::domain::ports::metrics::{IngestMetrics, IngestOutcome, NoopMetrics};
use crate::domain::ports::run_executor::{ExecutionEvent, NodeOutcome, TestObservation};
use crate::domain::repos::{LogPosition, QueueRowRecord};
use crate::domain::service::admission::tests::fakes::{
    FakeEnvironments, FakeQueue, FakeRuns, PLATFORM_A, SystemGrantingAuthZ, queued_row, run_fixture,
};
use crate::domain::service::test_support::{OTHER_TENANT, OWNER_TENANT, ctx, test_db_provider};
use crate::domain::service::{FlushReport, LogArchive, LogFanout};
use crate::domain::state_machine::{ExecutorOutcome, can_transition, is_terminal};
use crate::infra::metrics::probe::MetricsProbe;

const RUN: Uuid = Uuid::from_u128(0x0501);
const QUEUE: Uuid = Uuid::from_u128(0x0E51);

// ---------------------------------------------------------------------------
// Doubles and wiring
// ---------------------------------------------------------------------------

/// Records every fan-out call, so "the line reached the broadcaster" is
/// observable without a live channel.
///
/// The broadcaster's own behaviour — bounding, gap markers, reaping — is tested
/// at `infra::logs::broadcast`, where the channel is. What this double is for is
/// the half only ingest can be wrong about: *which* lines are published, in what
/// shape, and whether a terminal run's channel is released.
///
/// `pub(in crate::domain::service)` so `runs_tests` asserts the reap through the
/// same double rather than declaring a second one — the drift argument
/// `admission_tests`' header makes about the repository doubles.
#[derive(Default)]
pub(in crate::domain::service) struct RecordingLogs {
    lines: Mutex<Vec<(Uuid, String)>>,
    reaped: Mutex<Vec<Uuid>>,
}

impl RecordingLogs {
    pub(in crate::domain::service) fn lines(&self) -> Vec<(Uuid, String)> {
        self.lines.lock().unwrap().clone()
    }

    pub(in crate::domain::service) fn reaped(&self) -> Vec<Uuid> {
        self.reaped.lock().unwrap().clone()
    }
}

impl LogFanout for RecordingLogs {
    fn publish(&self, run_id: Uuid, line: String) {
        self.lines.lock().unwrap().push((run_id, line));
    }

    fn reap(&self, run_id: Uuid) {
        self.reaped.lock().unwrap().push(run_id);
    }
}

/// A `LogArchive` double: buffers per-run text like the real
/// `infra::logs::RunLogArchive`, "writes" it into a separate stored map on
/// `flush`, and can be told to fail its *next* flush exactly once — the same
/// one-shot failure-injection shape `MockRunsRepository::fail_next_append`
/// uses for `infra::logs::archive`'s own tests.
///
/// Deliberately simpler than the real thing: it does not need `RunLogArchive`'s
/// per-run `in_flight` guard, because nothing in this file drives two
/// concurrent flushes for one run — that race is `infra::logs::archive`'s own
/// `a_second_concurrent_flush_for_the_same_run_is_a_no_op`, against the real
/// implementation. What this double is for is the four properties `finish`
/// and `fan_out_log` themselves own: the archive receives the prefixed line,
/// `finish` flushes it, a flush failure does not fail `finish`, and nothing
/// but `finish` (in this module) ever flushes at all.
///
/// # `flush_due`'s report is a real report
///
/// An earlier revision left `report.lines` at `0` always, counting only
/// `runs`. `gear.rs`'s dispatcher tick logs `lines = flushed.lines`, and
/// `FlushReport`'s own doc says what the field means, so a double that never
/// populated it made this file's `flush_due` assertions unable to notice the
/// real implementation losing the count. The real one is covered by
/// `infra::logs::archive`'s `flush_due_drains_every_buffered_run`; this double
/// now mirrors its shape too, including its `queued > 0` gate and its "sample
/// before the drain" ordering, so a test written against the double cannot
/// pass for a reason the real thing would not reproduce.
#[derive(Default)]
struct RecordingArchive {
    pending: Mutex<HashMap<Uuid, String>>,
    stored: Mutex<HashMap<Uuid, String>>,
    fail_next_flush: Mutex<bool>,
    /// How many times an injected failure has actually **fired**. A test that
    /// injects one and never checks this cannot tell a swallowed failure from
    /// a flush that never happened — see
    /// `a_failed_archive_flush_does_not_fail_the_completion`.
    flush_failures: Mutex<usize>,
    /// Per-run, per-node resume positions not yet flushed — the double's
    /// analogue of `infra::logs::archive::Pending::positions` (Task 2, WS5).
    pending_positions: Mutex<HashMap<Uuid, HashMap<String, OffsetDateTime>>>,
    /// Flushed positions — what `resume_positions` actually reads, mirroring
    /// the split between `pending`/`stored` above for the same reason (see
    /// that method's own doc).
    stored_positions: Mutex<HashMap<Uuid, HashMap<String, OffsetDateTime>>>,
}

impl RecordingArchive {
    fn buffered_text(&self, run_id: Uuid) -> String {
        self.pending
            .lock()
            .unwrap()
            .get(&run_id)
            .cloned()
            .unwrap_or_default()
    }

    fn stored_text(&self, run_id: Uuid) -> String {
        self.stored
            .lock()
            .unwrap()
            .get(&run_id)
            .cloned()
            .unwrap_or_default()
    }

    fn fail_next_flush(&self) {
        *self.fail_next_flush.lock().unwrap() = true;
    }

    fn flush_failures(&self) -> usize {
        *self.flush_failures.lock().unwrap()
    }
}

#[async_trait::async_trait]
impl LogArchive for RecordingArchive {
    fn record(
        &self,
        _tenant_id: Uuid,
        run_id: Uuid,
        node: &str,
        line: &str,
        emitted_at: Option<OffsetDateTime>,
    ) {
        let mut pending = self.pending.lock().unwrap();
        let entry = pending.entry(run_id).or_default();
        entry.push_str(line);
        entry.push('\n');
        drop(pending);
        if let Some(when) = emitted_at {
            self.pending_positions
                .lock()
                .unwrap()
                .entry(run_id)
                .or_default()
                .insert(node.to_owned(), when);
        }
    }

    async fn flush(&self, run_id: Uuid) -> Result<(), DomainError> {
        let mut fail_next = self.fail_next_flush.lock().unwrap();
        if *fail_next {
            *fail_next = false;
            *self.flush_failures.lock().unwrap() += 1;
            return Err(DomainError::Internal(
                "RecordingArchive: injected flush failure".to_owned(),
            ));
        }
        drop(fail_next);

        let Some(taken) = self.pending.lock().unwrap().remove(&run_id) else {
            return Ok(());
        };
        self.stored
            .lock()
            .unwrap()
            .entry(run_id)
            .or_default()
            .push_str(&taken);
        if let Some(positions) = self.pending_positions.lock().unwrap().remove(&run_id) {
            self.stored_positions
                .lock()
                .unwrap()
                .entry(run_id)
                .or_default()
                .extend(positions);
        }
        Ok(())
    }

    async fn flush_due(&self) -> FlushReport {
        let ids: Vec<Uuid> = self.pending.lock().unwrap().keys().copied().collect();
        let mut report = FlushReport::default();
        for run_id in ids {
            // Sampled before `flush` drains the buffer and gated on
            // `queued > 0` on the way out, both mirroring
            // `RunLogArchive::flush_due` — see this double's own doc on why
            // its report has to mean what the real one's does.
            let queued = u64::try_from(self.buffered_text(run_id).lines().count()).unwrap_or(0);
            match self.flush(run_id).await {
                Ok(()) if queued > 0 => {
                    report.runs += 1;
                    report.lines += queued;
                }
                Ok(()) => {}
                Err(_) => report.failed += 1,
            }
        }
        report
    }

    /// Reads `self.stored_positions` — **not** `self.pending_positions` —
    /// which is what makes
    /// `resume_positions_flushes_a_pending_tail_before_reading_it` below able
    /// to prove `IngestService::resume_positions` flushes before it reads:
    /// against a double that read the pending side directly, a missing
    /// flush would be invisible.
    async fn resume_positions(
        &self,
        _tenant: system_actor::TenantBound,
        run_id: Uuid,
    ) -> Result<LogResume, DomainError> {
        Ok(self
            .stored_positions
            .lock()
            .unwrap()
            .get(&run_id)
            .map(|per_node| {
                per_node
                    .iter()
                    .map(|(node, when)| (node.clone(), LogPosition { last_emitted_at: *when }))
                    .collect()
            })
            .unwrap_or_default())
    }
}

struct Harness {
    runs: Arc<FakeRuns>,
    queue: Arc<FakeQueue>,
    environments: Arc<FakeEnvironments>,
    logs: Arc<RecordingLogs>,
    archive: Arc<RecordingArchive>,
    ingest: IngestService<FakeRuns, FakeQueue>,
}

impl Harness {
    /// Ingest one log line for [`RUN`] under [`owner`]'s tenant.
    async fn ingest_log_line(&self, node: &str, line: &str) {
        self.ingest_log_line_at(node, line, None).await;
    }

    /// [`Self::ingest_log_line`] with an explicit `emitted_at` — the one
    /// caller that needs it is
    /// `resume_positions_flushes_a_pending_tail_before_reading_it`, which
    /// has to record an actual position for `resume_positions` to have
    /// anything to answer.
    async fn ingest_log_line_at(&self, node: &str, line: &str, emitted_at: Option<OffsetDateTime>) {
        self.ingest
            .apply(
                &owner(),
                RUN,
                ExecutionEvent::Log {
                    node: node.to_owned(),
                    line: line.to_owned(),
                    emitted_at,
                },
            )
            .await
            .unwrap();
    }

    /// Every line [`Self::logs`] received, in publish order, unprefixed of
    /// which run they belong to — every test in this file drives exactly one.
    fn published_lines(&self) -> Vec<String> {
        self.logs
            .lines()
            .into_iter()
            .map(|(_, line)| line)
            .collect()
    }

    /// [`RUN`]'s text still sitting in the archive's pending buffer —
    /// buffered, not yet flushed to storage.
    fn archive_buffered_text(&self) -> String {
        self.archive.buffered_text(RUN)
    }

    /// [`RUN`]'s durably archived text — what a `flush` actually wrote.
    fn stored_text(&self) -> String {
        self.archive.stored_text(RUN)
    }

    fn fail_next_append(&self) {
        self.archive.fail_next_flush();
    }

    fn archive_flush_failures(&self) -> usize {
        self.archive.flush_failures()
    }

    /// Finish [`RUN`] with a green outcome. The completion guard may still
    /// downgrade it to `Failed` if no result was ever ingested — this helper
    /// is about driving `finish`'s archive flush, not about the verdict.
    async fn finish_run_successfully(&self) {
        self.ingest
            .apply(&owner(), RUN, finished(ExecutorOutcome::Succeeded))
            .await
            .unwrap();
    }

    fn run_is_terminal(&self) -> bool {
        self.runs.state_of(RUN).is_some_and(is_terminal)
    }
}

/// A run in `Running` on `PLATFORM_A`, its queue row claimed, its lease held.
///
/// The default fixture is the one every completion test needs, so a test that
/// wants something else says only what it changes.
async fn harness(state: RunState, lease: LeaseState) -> Harness {
    let run = qa_runs_sdk::Run {
        started_at: Some(time::OffsetDateTime::now_utc()),
        execution_ref: Some("mock-execution-1".to_owned()),
        ..run_fixture(RUN, Some(PLATFORM_A), true, state)
    };
    let runs = Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)]));
    let queue = Arc::new(FakeQueue::with(vec![queued_row(
        QUEUE,
        OWNER_TENANT,
        RUN,
        PLATFORM_A,
        true,
        QueueState::Running,
    )]));
    let environments = Arc::new(FakeEnvironments::holding(PLATFORM_A, lease));
    build(runs, queue, environments).await
}

async fn build(
    runs: Arc<FakeRuns>,
    queue: Arc<FakeQueue>,
    environments: Arc<FakeEnvironments>,
) -> Harness {
    build_with_metrics(runs, queue, environments, Arc::new(NoopMetrics)).await
}

/// The same wiring with an emission port named.
///
/// Split out rather than adding a parameter to [`build`], so the ~60 tests in
/// this file that do not read metrics keep the production default — a service
/// holding `NoopMetrics` — and say nothing about it.
async fn build_with_metrics(
    runs: Arc<FakeRuns>,
    queue: Arc<FakeQueue>,
    environments: Arc<FakeEnvironments>,
    metrics: Arc<dyn IngestMetrics>,
) -> Harness {
    let db = test_db_provider().await;
    let logs = Arc::new(RecordingLogs::default());
    let archive = Arc::new(RecordingArchive::default());
    let enforcer = authz_resolver_sdk::PolicyEnforcer::new(Arc::new(SystemGrantingAuthZ));
    let ingest = IngestService::new(IngestDeps {
        db: SerializedDb::new(db),
        runs: Arc::clone(&runs),
        queue: Arc::clone(&queue),
        environments: Arc::clone(&environments) as Arc<dyn QaEnvironmentsClientV1>,
        logs: Arc::clone(&logs) as Arc<dyn LogFanout>,
        archive: Arc::clone(&archive) as Arc<dyn LogArchive>,
        policy_enforcer: enforcer,
        metrics,
    });
    Harness {
        runs,
        queue,
        environments,
        logs,
        archive,
        ingest,
    }
}

/// A platformless run whose target is `Collect`, holding no lease and no claim
/// — the shape qa-insights' hourly cycle launches (`RunTarget::Collect` carries
/// no `environment_id`, and `service::launch` bypasses admission for the kind).
async fn collect_harness() -> Harness {
    let run = qa_runs_sdk::Run {
        target: qa_runs_sdk::RunTarget::Collect {
            repo_id: Uuid::from_u128(0x0B0A),
            collect_url: "http://gears:8087/qa/v1/collect/repo?sig=x".to_owned(),
        },
        ..run_fixture(RUN, None, false, RunState::Running)
    };
    build(
        Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])),
        Arc::new(FakeQueue::default()),
        Arc::new(FakeEnvironments::free()),
    )
    .await
}

fn observation(test_name: &str, status: &str) -> TestObservation {
    TestObservation {
        node: "repo-a".to_owned(),
        test_file: "tests/test_smoke.py".to_owned(),
        test_name: test_name.to_owned(),
        status: status.to_owned(),
        duration: Some("1.5s".to_owned()),
        launch_id: None,
        jira_key: None,
        // File-level: this suite is about counters and the terminal-state
        // drop, none of which the three case fields feed. `case_observation`
        // below is the case-level counterpart.
        nodeid: None,
        reason: None,
        ticket: None,
    }
}

/// [`observation`], reported at **case** granularity: a non-empty `nodeid`,
/// an xfail reason, and a case-level `ticket` that is deliberately *not* the
/// file-level `jira_key`.
///
/// The two differ so a path that filled `ticket` from `jira_key` — the collapse
/// `m20260818_000005_case_fidelity`'s (folded into `migrations::m20260813_000003_initial` by the docs squash) header argues against — cannot pass.
fn case_observation(test_name: &str, status: &str) -> TestObservation {
    TestObservation {
        jira_key: Some("VHP-2618".to_owned()),
        nodeid: Some(format!("tests/test_smoke.py::TestSmoke::{test_name}[tls]")),
        reason: Some("known upstream defect".to_owned()),
        ticket: Some("VHP-3117".to_owned()),
        ..observation(test_name, status)
    }
}

fn finished(outcome: ExecutorOutcome) -> ExecutionEvent {
    ExecutionEvent::Finished {
        outcome,
        nodes: NodeOutcome::NoneFailed,
        message: None,
    }
}

fn owner() -> SecurityContext {
    ctx(OWNER_TENANT)
}

// ---------------------------------------------------------------------------
// The pure delta arithmetic
// ---------------------------------------------------------------------------

/// The mapping the migration declares, asserted value by value rather than
/// through a whole ingest, because a wrong arm here under-counts silently.
#[test]
fn each_known_status_lands_in_the_counter_the_migration_names() {
    for (status, expected) in [
        ("PASSED", Bucket::Passed),
        ("FAILED", Bucket::Failed),
        ("ERROR", Bucket::Failed),
        ("SKIPPED", Bucket::Skipped),
        ("PENDING", Bucket::InProgress),
        ("RUNNING", Bucket::InProgress),
        ("XFAIL", Bucket::Xfail),
        ("XPASS", Bucket::Xpass),
    ] {
        assert_eq!(bucket(status), expected, "{status}");
    }
}

/// The open set: a value the runner invented is stored and counted in `total`,
/// never rejected. `INFRASTRUCTURE_ERROR` is the value
/// `infra::storage::mapper` names as "not hypothetical".
#[test]
fn a_ninth_status_counts_toward_the_total_alone() {
    assert_eq!(bucket("INFRASTRUCTURE_ERROR"), Bucket::Uncategorised);
    assert_eq!(
        counter_delta(None, "INFRASTRUCTURE_ERROR"),
        RunResultDelta {
            total: 1,
            ..RunResultDelta::default()
        }
    );
}

/// `normalize_status` deliberately does not recase, so a lower-case status
/// lands in no counter — the source system's own arithmetic on its progress
/// path (`routes/runs.rs:1174` binds raw; `routes/plans.rs:188-191` matches
/// upper case only).
#[test]
fn a_lower_case_status_is_counted_the_way_the_source_system_counts_it() {
    assert_eq!(bucket("passed"), Bucket::Uncategorised);
}

/// The reason the deltas are signed. A first sighting adds to `total`; a
/// transition between statuses does not.
#[test]
fn a_transition_moves_between_counters_without_moving_the_total() {
    assert_eq!(
        counter_delta(None, "PENDING"),
        RunResultDelta {
            in_progress: 1,
            xfail: 0,
            xpass: 0,
            total: 1,
            ..RunResultDelta::default()
        }
    );
    assert_eq!(
        counter_delta(Some("PENDING"), "PASSED"),
        RunResultDelta {
            passed: 1,
            in_progress: -1,
            total: 0,
            ..RunResultDelta::default()
        },
        "RunResultDelta's own doc gives exactly this example"
    );
    assert_eq!(
        counter_delta(Some("PASSED"), "PASSED"),
        RunResultDelta::default(),
        "a genuinely repeated result must move nothing"
    );
}

// ---------------------------------------------------------------------------
// The completion guard
// ---------------------------------------------------------------------------

#[test]
fn a_succeeded_derivation_with_no_results_is_refused() {
    let (state, reason) =
        apply_completion_guard(RunState::Succeeded, RunResult::default(), RunKind::Plan);
    assert_eq!(state, RunState::Failed);
    assert_eq!(reason, Some(NO_RESULTS_REASON));
}

/// The guard must not fire on a run that really did report results, and must not
/// touch a verdict that already refuses to claim a pass — otherwise it would
/// relabel a cancel or a timeout as a failure.
#[test]
fn the_completion_guard_touches_nothing_else() {
    let one = RunResult {
        passed: 1,
        total: 1,
        ..RunResult::default()
    };
    assert_eq!(
        apply_completion_guard(RunState::Succeeded, one, RunKind::Plan),
        (RunState::Succeeded, None)
    );
    for state in [
        RunState::Failed,
        RunState::Error,
        RunState::Canceled,
        RunState::TimedOut,
    ] {
        assert_eq!(
            apply_completion_guard(state, RunResult::default(), RunKind::Plan),
            (state, None),
            "{state:?} already declines to claim a pass"
        );
    }
}

/// The exemption is for the one kind that records nothing by construction, and
/// for no other: a plan, a single test or a custom plan that reported nothing
/// is still the dropped-ingest case the guard exists to catch.
#[test]
fn every_kind_that_runs_tests_is_still_refused_a_zero_result_pass() {
    for kind in [RunKind::Plan, RunKind::Test, RunKind::CustomPlan] {
        assert_eq!(
            apply_completion_guard(RunState::Succeeded, RunResult::default(), kind),
            (RunState::Failed, Some(NO_RESULTS_REASON)),
            "{kind:?} executes tests, so no result row means none was reported"
        );
    }
}

/// A collect run's deliverable is the count report, not a result row — see
/// `apply_completion_guard`'s own doc for the measurement behind this.
#[test]
fn a_collect_run_keeps_a_pass_it_has_no_results_for() {
    assert_eq!(
        apply_completion_guard(RunState::Succeeded, RunResult::default(), RunKind::Collect),
        (RunState::Succeeded, None)
    );
}

// ---------------------------------------------------------------------------
// Per-test results
// ---------------------------------------------------------------------------

#[tokio::test]
async fn results_are_upserted_idempotently_for_a_repeated_test() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let ctx = owner();

    for _ in 0..3 {
        h.ingest
            .apply(
                &ctx,
                RUN,
                ExecutionEvent::TestResult(observation("t", "PASSED")),
            )
            .await
            .unwrap();
    }

    assert_eq!(h.runs.results_of(RUN).len(), 1, "delete-then-insert dedupe");
    assert_eq!(
        h.runs.counts_of(RUN),
        RunResult {
            passed: 1,
            total: 1,
            ..RunResult::default()
        },
        "a repeat must not inflate the counters"
    );
    // The second and third events computed a zero delta and skipped the write
    // entirely, which is the observable form of "no read-modify-write".
    assert_eq!(h.runs.count_deltas.lock().unwrap().len(), 1);
}

/// The signed-delta property end to end: three tests reported `PENDING` and then
/// resolved. Every count is the arithmetic the migration's mapping predicts, and
/// `total` counts rows rather than events.
///
/// **Renamed 2026-08-14 from `..._across_concurrent_events`.** The body is three
/// awaited calls in a `for` loop and always was: it exercises *sequential* events,
/// and the old name claimed a concurrency it did not drive. The concurrent case
/// is the one `IngestService::record_one_result` documents as open — two events
/// for the **same** test can still double-count a transition — and it is
/// contained at completion by `reconciled_counts`, not here.
#[tokio::test]
async fn counts_accumulate_from_signed_deltas_across_a_runs_events() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let ctx = owner();

    for name in ["a", "b", "c"] {
        h.ingest
            .apply(
                &ctx,
                RUN,
                ExecutionEvent::TestResult(observation(name, "PENDING")),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        h.runs.counts_of(RUN),
        RunResult {
            in_progress: 3,
            xfail: 0,
            xpass: 0,
            total: 3,
            ..RunResult::default()
        }
    );

    for (name, status) in [("a", "PASSED"), ("b", "FAILED"), ("c", "SKIPPED")] {
        h.ingest
            .apply(
                &ctx,
                RUN,
                ExecutionEvent::TestResult(observation(name, status)),
            )
            .await
            .unwrap();
    }

    assert_eq!(
        h.runs.counts_of(RUN),
        RunResult {
            passed: 1,
            failed: 1,
            skipped: 1,
            in_progress: 0,
            xfail: 0,
            xpass: 0,
            total: 3,
        },
        "in_progress must have been decremented by the transitions, not left at 3"
    );
    assert_eq!(h.runs.results_of(RUN).len(), 3);
}

// ---------------------------------------------------------------------------
// normalize_status
// ---------------------------------------------------------------------------

#[test]
fn a_status_is_trimmed_but_never_recased() {
    assert_eq!(normalize_status("PASSED\n"), "PASSED");
    assert_eq!(normalize_status("  SKIPPED  "), "SKIPPED");
    assert_eq!(
        normalize_status("passed"),
        "passed",
        "upper-casing would move this value from no counter into the \
         passed counter; the source system's progress path binds it raw \
         (routes/runs.rs:1174) and the counters match upper case only \
         (routes/plans.rs:188-191)"
    );
    assert_eq!(
        normalize_status("INFRASTRUCTURE_ERROR"),
        "INFRASTRUCTURE_ERROR",
        "the ninth value is a runner change, not corruption"
    );
}

/// The property that makes the trim safe under a parity mandate: it can
/// never take a value *out* of a counter, because a value that already
/// equals a counted spelling has no whitespace to remove.
#[test]
fn trimming_is_a_no_op_on_every_counted_spelling() {
    for counted in [
        "PASSED", "FAILED", "ERROR", "SKIPPED", "PENDING", "RUNNING", "XFAIL", "XPASS",
    ] {
        assert_eq!(normalize_status(counted), counted);
    }
}

/// The composition rule this module owes, asserted rather than left in prose:
/// `normalize_status` trims and `infra::storage::mapper::normalize_test_status`
/// truncates, and the two do **not** commute.
#[test]
fn trimming_must_precede_the_column_truncation() {
    use crate::infra::storage::mapper::normalize_test_status;

    // The minimal corrupting input, and the more dangerous of the two: at 11
    // leading spaces the wrong order yields a status that *looks* like a
    // status and matches no counter, rather than an obviously empty one.
    let corrupting = format!("{}PASSED", " ".repeat(11));
    assert_eq!(
        normalize_test_status(normalize_status(&corrupting)),
        "PASSED"
    );
    assert_eq!(
        normalize_status(&normalize_test_status(corrupting)),
        "PASSE",
        "truncating first silently mangles the status; ingest must trim \
         before the repository writes"
    );

    // 10 leading spaces fits the column, so the order stops mattering — which
    // is what makes 11 the threshold rather than an arbitrary pick.
    let fitting = format!("{}PASSED", " ".repeat(10));
    assert_eq!(normalize_test_status(normalize_status(&fitting)), "PASSED");
    assert_eq!(normalize_status(&normalize_test_status(fitting)), "PASSED");

    // And the erasing case, at 16.
    let erasing = format!("{}PASSED", " ".repeat(16));
    assert_eq!(normalize_test_status(normalize_status(&erasing)), "PASSED");
    assert_eq!(normalize_status(&normalize_test_status(erasing)), "");
}

/// The seam guarantee `normalize_status`' doc hands to this task: *"the column's
/// guarantee is Task 15's to test at its own seam."*
///
/// Eleven leading spaces is the minimal corrupting input — trimming after the
/// column truncation yields `PASSE`, a status that looks like a status and
/// matches no counter. The assertion is on the **stored** value and on the
/// **counter**, because a wrong order shows up in both and only the second one
/// is silent.
#[tokio::test]
async fn the_stored_status_is_trimmed_before_the_column_truncates_it() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let padded = format!("{}PASSED", " ".repeat(11));
    h.ingest
        .apply(
            &owner(),
            RUN,
            ExecutionEvent::TestResult(observation("t", &padded)),
        )
        .await
        .unwrap();

    let stored = h.runs.results_of(RUN);
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].status, "PASSED",
        "truncating first would have stored PASSE"
    );
    assert_eq!(
        h.runs.counts_of(RUN),
        RunResult {
            passed: 1,
            total: 1,
            ..RunResult::default()
        },
        "PASSE matches no counter, so a wrong order is silent in the row and \
         visible only here"
    );
}

/// **`resume_positions` flushes before it reads — fix-round 1, Important 3.**
///
/// `record` only buffers; a periodic `flush_due` tick or `finish` is what
/// writes the row `log_resume_positions` reads. `gear.rs` runs `flush_due`
/// *after* the dispatcher tick that triggers a re-attach, so without an
/// explicit flush here, a run whose previous observer queued lines but never
/// got to flush them would read a resume position that undercounts by up to a
/// full tick's worth of its own output — and the resuming executor would
/// re-fetch and re-archive exactly that tail.
///
/// `RecordingArchive::resume_positions` reads only `stored`, never `pending`
/// (see that impl's own doc), so this is the one test that can tell "flushed
/// first" apart from "read straight through": against a double that read
/// `pending` directly, a missing flush would be invisible, which is exactly
/// how this went unnoticed in the original commit.
#[tokio::test]
async fn resume_positions_flushes_a_pending_tail_before_reading_it() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let emitted_at = OffsetDateTime::now_utc();
    h.ingest_log_line_at("a", "line one", Some(emitted_at)).await;

    // Premise: nothing has been flushed yet, so a read that skipped the
    // flush would see an empty archive.
    assert!(
        h.stored_text().is_empty(),
        "premise: the line is only buffered, not yet flushed"
    );

    let resume = h
        .ingest
        .resume_positions(system_actor::TenantBound::new(OWNER_TENANT).unwrap(), RUN)
        .await
        .unwrap();

    assert_eq!(
        resume.last_emitted_for("a"),
        Some(emitted_at),
        "the pending position must be flushed before this reads, or it answers None"
    );
    assert!(
        !h.stored_text().is_empty(),
        "the flush must have actually happened as an observable side effect"
    );
}

/// A case-level observation reaches the **row** with all three fields, and none
/// of them disturbs the counters.
///
/// # What this covers that the repository test does not
///
/// `infra::storage::runs_sea_repo::tests::a_case_level_nodeid_reason_and_ticket_are_actually_written`
/// proves the repository stores what it is handed. It says nothing about what
/// ingest hands it, and that is a second, independent silent-drop site of the
/// same shape: `record_one_result` builds a `NewTestResult` struct literal, so
/// writing `nodeid: None` there instead of `observation.nodeid.clone()` would
/// compile and lose the value, and the harness's own `results_of` would report
/// the row as identical. The two tests together cover the chain; neither
/// covers it alone.
///
/// It asserts on `FakeRuns::new_results` — the raw `NewTestResult`s — rather
/// than on `results_of`.
///
/// **Corrected by Task 4.** The original reason given was that `TestResultRow`
/// "does not carry the three and the stored projection is therefore blind to
/// exactly this defect". That clause is now false: the reconciler read needed
/// all three, so `TestResultRow` carries them and `results_of` would see them.
/// The choice stands on a narrower argument that was always the real one —
/// `new_results` is what ingest **asked for**, before the write collapses
/// `nodeid`'s `Option` to `""` and before `normalize_test_status` touches the
/// status, so it is the only place a value can be compared to what the executor
/// reported rather than to what storage made of it. Break-verification after
/// the widening confirms the test still isolates this defect: replacing one of
/// the three with `None` in `record_one_result` turns exactly this test red.
///
/// **Break-verified**: replacing any one of the three with `None` in
/// `IngestService::record_one_result` turns this red and nothing else in the
/// workspace notices.
///
/// The counter assertion is the *negative* half, and it is the reason this is
/// one test rather than two: the three fields are carried, and `counter_delta`
/// is derived from `status` alone, so a case-level result must move the
/// counters exactly as its file-level twin does.
#[tokio::test]
async fn a_case_level_result_carries_its_three_fields_into_the_stored_row() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let seen = case_observation("test_fail_closed", "XFAIL");

    h.ingest
        .apply(&owner(), RUN, ExecutionEvent::TestResult(seen.clone()))
        .await
        .unwrap();

    let written = h.runs.new_results.lock().unwrap().clone();
    assert_eq!(written.len(), 1);
    assert_eq!(
        written[0].nodeid, seen.nodeid,
        "the pytest node id must reach the row; without it every case-level \
         result is indistinguishable from a file-level one"
    );
    assert_eq!(written[0].reason.as_deref(), Some("known upstream defect"));
    assert_eq!(
        written[0].ticket.as_deref(),
        Some("VHP-3117"),
        "the case-level ticket, not the file-level jira_key"
    );
    assert_eq!(written[0].jira_key.as_deref(), Some("VHP-2618"));
    assert_ne!(
        written[0].ticket, written[0].jira_key,
        "two columns, and this is the assertion that stops them being one"
    );

    // XFAIL feeds `total` and the `xfail` counter, exactly as it would for a
    // file-level row: none of the three new fields is an input to
    // `counter_delta`, which is derived from `status` alone.
    assert_eq!(
        h.runs.counts_of(RUN),
        RunResult {
            xfail: 1,
            xpass: 0,
            total: 1,
            ..RunResult::default()
        },
        "the case fields must not have reached the counter classification"
    );
}

/// `observation.duration` and `observation.launch_id` reach [`NewTestResult`]
/// (`record_one_result`'s construction), and until now nothing asserted
/// either landed on the row. `duration` is `Some(..)` in every fixture
/// [`observation`] builds, so a regression that dropped it would still leave
/// every other test green; no fixture before this one ever set `launch_id` to
/// `Some(..)` at all; the deleted event-consumer test that did was removed as
/// dead code along with the event path, and nothing replaced it. Setting
/// either to `None` unconditionally would leave all 853 tests green and
/// return a null `launch_id` from `GET /qa/v1/runs/{id}/results` forever.
///
/// **Break-verified**: hard-coding `duration: None` or `launch_id: None` in
/// `IngestService::record_one_result`'s `NewTestResult` construction turns
/// this red and nothing else in the workspace notices.
#[tokio::test]
async fn a_results_duration_and_launch_id_reach_the_stored_row() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let mut seen = observation("test_smoke", "PASSED");
    seen.duration = Some("85.06s (0:01:25)".to_owned());
    seen.launch_id = Some("rp-launch-9182".to_owned());

    h.ingest
        .apply(&owner(), RUN, ExecutionEvent::TestResult(seen.clone()))
        .await
        .unwrap();

    let written = h.runs.new_results.lock().unwrap().clone();
    assert_eq!(written.len(), 1);
    assert_eq!(
        written[0].duration.as_deref(),
        Some("85.06s (0:01:25)"),
        "the runner's duration text must reach the row verbatim"
    );
    assert_eq!(
        written[0].launch_id.as_deref(),
        Some("rp-launch-9182"),
        "the ReportPortal launch id must reach the row"
    );
}

/// `add_result_counts` is unguarded and will bump a terminal run's counters;
/// `runs_repo` says whether a late event is dropped "belongs to ingest". It is
/// dropped — nothing is stored, nothing is counted, and the run's state is
/// untouched.
#[tokio::test]
async fn a_late_result_after_a_terminal_state_does_not_reopen_the_run() {
    let h = harness(RunState::Succeeded, LeaseState::Free).await;
    h.ingest
        .apply(
            &owner(),
            RUN,
            ExecutionEvent::TestResult(observation("late", "FAILED")),
        )
        .await
        .unwrap();

    assert_eq!(h.runs.state_of(RUN), Some(RunState::Succeeded));
    assert!(h.runs.results_of(RUN).is_empty());
    assert_eq!(h.runs.counts_of(RUN), RunResult::default());
}

/// The tenancy check, on the path that writes to the child table. Not-found and
/// forbidden are one answer, which is what closes the cross-tenant oracle.
#[tokio::test]
async fn ingest_for_another_tenants_run_is_a_not_found_and_writes_nothing() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let error = h
        .ingest
        .apply(
            &ctx(OTHER_TENANT),
            RUN,
            ExecutionEvent::TestResult(observation("t", "PASSED")),
        )
        .await
        .expect_err("a foreign tenant must not ingest");

    assert!(matches!(error, DomainError::RunNotFound { id } if id == RUN));
    assert!(h.runs.results_of(RUN).is_empty());
    assert_eq!(h.runs.counts_of(RUN), RunResult::default());
}

/// **A deliberate divergence from the plan's `an_event_for_an_unknown_run_is_
/// dropped_without_error`.**
///
/// That test cites the source system's `202`
/// (`../testrunner/manager/src/routes/runs.rs:1148`), whose own comment gives
/// the reason: *"Run row not persisted yet; poller will create it and
/// reconcile"* — legacy writes the run row only after a successful submit
/// (`argo.rs:674`, `persist_submitted_run`), so a progress event genuinely can
/// arrive before there is anything to attach it to.
///
/// **That precondition does not exist here.** This gear creates the run row at
/// launch, before admission and long before any execution, so an event for a run
/// this caller cannot see means the run is absent or belongs to someone else —
/// and those two must stay indistinguishable. Swallowing it would make ingest
/// silently accept another tenant's events, so it is a `RunNotFound` and Task 16
/// maps it.
#[tokio::test]
async fn an_event_for_an_unknown_run_is_a_not_found_rather_than_legacys_202() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let stranger = Uuid::from_u128(0xDEAD);
    let error = h
        .ingest
        .apply(
            &owner(),
            stranger,
            ExecutionEvent::TestResult(observation("t", "PASSED")),
        )
        .await
        .expect_err("an unknown run is not silently accepted");
    assert!(matches!(error, DomainError::RunNotFound { id } if id == stranger));
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_finished_event_derives_the_terminal_state_from_the_counts() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let ctx = owner();
    h.ingest
        .apply(
            &ctx,
            RUN,
            ExecutionEvent::TestResult(observation("a", "PASSED")),
        )
        .await
        .unwrap();
    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Succeeded))
        .await
        .unwrap();

    assert_eq!(h.runs.state_of(RUN), Some(RunState::Succeeded));
    assert!(h.runs.finished_at_of(RUN).is_some());
    assert_eq!(
        h.runs.counts_of(RUN),
        RunResult {
            passed: 1,
            total: 1,
            ..RunResult::default()
        },
        "the recorded counts the verdict was derived from"
    );
}

/// Decision D2, end to end, until 2026-08-28: this test used to assert
/// `RunState::Failed`, because "a skipped test means the run didn't fully
/// execute, so it must never read as passing either" (`argo.rs:2180-2181`,
/// `manager/src/services/argo.rs:2197`). The product owner overrode that rule
/// on 2026-08-28 — see `domain::state_machine::derive_terminal_state`'s "Skips
/// no longer fail a run" — because the suites this platform runs are full of
/// environment-gated skips, and the old rule marked nearly every real run red.
/// A skipped result on an otherwise-clean execution is `Succeeded` now; the
/// skip count is surfaced in the UI instead of hiding in the state.
#[tokio::test]
async fn a_skipped_result_no_longer_fails_the_run() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let ctx = owner();
    h.ingest
        .apply(
            &ctx,
            RUN,
            ExecutionEvent::TestResult(observation("a", "PASSED")),
        )
        .await
        .unwrap();
    h.ingest
        .apply(
            &ctx,
            RUN,
            ExecutionEvent::TestResult(observation("b", "SKIPPED")),
        )
        .await
        .unwrap();
    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Succeeded))
        .await
        .unwrap();

    assert_eq!(
        h.runs.state_of(RUN),
        Some(RunState::Succeeded),
        "ratified divergence from legacy (argo.rs:2197), product owner decision 2026-08-28"
    );
}

/// The completion guard in the composed pipeline, not just in isolation: the
/// executor says the run passed and no result was ever ingested.
#[tokio::test]
async fn a_succeeded_outcome_with_no_ingested_results_is_recorded_failed() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    h.ingest
        .apply(&owner(), RUN, finished(ExecutorOutcome::Succeeded))
        .await
        .unwrap();

    assert_eq!(
        h.runs.state_of(RUN),
        Some(RunState::Failed),
        "a run whose work never reported must never read as passing"
    );
    assert_eq!(h.runs.error_of(RUN).as_deref(), Some(NO_RESULTS_REASON));
}

/// **A collect run records no test result by construction, and must still pass.**
/// Its deliverable is the per-file exact count it POSTs to qa-insights
/// (`COLLECT_ONLY`/`VHP_COLLECT_URL`, the runner-facing contract), not a row in
/// `qa_run_test_results` — so `total == 0` is this run kind's SUCCESS shape, not
/// the evidence of a runner that never started that the guard exists to catch.
///
/// MEASURED, not hypothesised: on 2026-09-03 run
/// `collect-d4addf78-…-76` reported all 221 files' counts, its workflow phase
/// was `Succeeded`, and this guard recorded it `failed` with
/// [`NO_RESULTS_REASON`] — as it had for every collect run before it.
#[tokio::test]
async fn a_collect_run_with_no_ingested_results_is_recorded_succeeded() {
    let h = collect_harness().await;

    h.ingest
        .apply(&owner(), RUN, finished(ExecutorOutcome::Succeeded))
        .await
        .unwrap();

    assert_eq!(
        h.runs.state_of(RUN),
        Some(RunState::Succeeded),
        "a collect cycle that reported its counts is not a failed run"
    );
    assert_eq!(
        h.runs.error_of(RUN),
        None,
        "and it carries no failure reason to explain"
    );
}

/// The exemption is from the GUARD, not from the verdict: an executor that
/// reports failure still fails a collect run, with no reason invented for it.
#[tokio::test]
async fn a_failed_collect_run_is_still_failed() {
    let h = collect_harness().await;

    h.ingest
        .apply(&owner(), RUN, finished(ExecutorOutcome::Failed))
        .await
        .unwrap();

    assert_eq!(h.runs.state_of(RUN), Some(RunState::Failed));
    assert_eq!(h.runs.error_of(RUN), None);
}

/// A dead node with no results at all is the *other* zero-result case, and it
/// must reach `Failed` through the derivation rather than through the guard —
/// the recorded reason is how an operator tells them apart.
#[tokio::test]
async fn a_node_failure_downgrades_a_green_outcome_with_its_own_reason() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    h.ingest
        .apply(
            &owner(),
            RUN,
            ExecutionEvent::Finished {
                outcome: ExecutorOutcome::Succeeded,
                nodes: NodeOutcome::SomeFailed,
                message: Some("node repo-a could not pull its image".to_owned()),
            },
        )
        .await
        .unwrap();

    assert_eq!(h.runs.state_of(RUN), Some(RunState::Failed));
    assert_eq!(
        h.runs.error_of(RUN).as_deref(),
        Some("node repo-a could not pull its image"),
        "the executor's operator-facing message is what `qa_runs.error` records \
         (`argo.rs:2280-2283`), and the guard did not fire"
    );
}

/// The invariant Step 4 of the plan asks to be asserted directly, and the reason
/// the lease is released before the row: after a run reaches a terminal state,
/// the platform's lease no longer names it.
#[tokio::test]
async fn a_terminal_run_no_longer_holds_its_platforms_lease() {
    let h = harness(RunState::Running, LeaseState::HeldExclusive { holder: RUN }).await;
    let ctx = owner();
    assert_eq!(
        h.environments.get_lease(&ctx, PLATFORM_A).await.unwrap(),
        LeaseState::HeldExclusive { holder: RUN },
        "premise: the platform really is held by this run"
    );

    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Failed))
        .await
        .unwrap();

    assert_eq!(
        h.environments.get_lease(&ctx, PLATFORM_A).await.unwrap(),
        LeaseState::Free
    );
    assert_eq!(h.environments.released(), vec![(PLATFORM_A, RUN)]);
    assert_eq!(
        h.queue.state_of(QUEUE),
        Some(QueueState::Done),
        "the claim is released as completed, not as failed"
    );
}

/// A run with no platform holds nothing, so completion must not reach for a
/// lease or a claim — the platformless bypass, from the ingest side.
#[tokio::test]
async fn a_platformless_run_finishes_without_touching_a_lease_or_a_claim() {
    let runs = Arc::new(FakeRuns::with(vec![(
        OWNER_TENANT,
        run_fixture(RUN, None, false, RunState::Running),
    )]));
    let h = build(
        runs,
        Arc::new(FakeQueue::default()),
        Arc::new(FakeEnvironments::free()),
    )
    .await;

    h.ingest
        .apply(&owner(), RUN, finished(ExecutorOutcome::Failed))
        .await
        .unwrap();

    assert_eq!(h.runs.state_of(RUN), Some(RunState::Failed));
    assert!(h.environments.released().is_empty());
}

/// **The 409 trap.** `reconcile_recorded_state(Failed, Succeeded) == Failed` and
/// `can_transition(Failed, Failed) == false`, so a naive composition answers
/// `IllegalTransition` for a duplicate the executor is right to retry.
#[tokio::test]
async fn a_duplicate_finished_event_is_a_silent_no_op_not_a_conflict() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let ctx = owner();
    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Failed))
        .await
        .unwrap();
    let first_finished_at = h.runs.finished_at_of(RUN);

    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Failed))
        .await
        .expect("a duplicate completion is not a fault");

    assert_eq!(h.runs.state_of(RUN), Some(RunState::Failed));
    assert_eq!(
        h.runs.finished_at_of(RUN),
        first_finished_at,
        "nothing was rewritten"
    );
}

/// **Inverted 2026-08-14 by user decision.** This test previously asserted the
/// opposite — that a recorded `Succeeded` survived later evidence — and pinned
/// the conflict Task 15 surfaced between `reconcile_recorded_state`'s documented
/// `Succeeded` downgrade and `can_transition`'s blanket refusal to leave a
/// terminal state. The user resolved it by opening
/// `Succeeded -> {Failed, Error, Canceled, TimedOut}`, so the downgrade is now
/// the required behaviour and this asserts it **end to end through ingest**
/// rather than merely that the edge returns `true`.
///
/// It matters that it runs through ingest: the equality no-op, the reconciler and
/// the guard are three separate decisions, and only the composed path shows that
/// a second `Finished` carrying `Failed` reaches the write instead of being
/// swallowed by the no-op branch.
#[tokio::test]
async fn a_recorded_success_is_downgraded_by_a_later_finished_event() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let ctx = owner();
    h.ingest
        .apply(
            &ctx,
            RUN,
            ExecutionEvent::TestResult(observation("a", "PASSED")),
        )
        .await
        .unwrap();
    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Succeeded))
        .await
        .unwrap();
    assert_eq!(h.runs.state_of(RUN), Some(RunState::Succeeded));

    // A late failing *result* is still dropped, so the downgrade is driven from
    // the outcome — which is the only way to reach the reconciler's arm.
    h.ingest
        .apply(
            &ctx,
            RUN,
            ExecutionEvent::Finished {
                outcome: ExecutorOutcome::Failed,
                nodes: NodeOutcome::NoneFailed,
                message: Some(
                    "the poller resolved a failure the phase could not express".to_owned(),
                ),
            },
        )
        .await
        .expect("a downgrade is not a fault");

    assert_eq!(
        h.runs.state_of(RUN),
        Some(RunState::Failed),
        "a green verdict is the one later evidence may overturn, which is what \
         `effective_base_phase` does in the source system"
    );
}

/// The half of the same decision that must **not** have moved, asserted through
/// the same composed path: a duplicate `Finished` carrying the *same* outcome
/// still hits the equality no-op before `can_transition` is consulted, so the
/// 409 trap stays closed. `Succeeded -> Succeeded` is still refused by the guard,
/// which is exactly why the no-op has to come first.
#[tokio::test]
async fn a_duplicate_success_still_no_ops_rather_than_conflicting() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let ctx = owner();
    h.ingest
        .apply(
            &ctx,
            RUN,
            ExecutionEvent::TestResult(observation("a", "PASSED")),
        )
        .await
        .unwrap();
    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Succeeded))
        .await
        .unwrap();
    let finished_at = h.runs.finished_at_of(RUN);

    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Succeeded))
        .await
        .expect("a retried Succeeded is not a conflict");

    assert_eq!(h.runs.state_of(RUN), Some(RunState::Succeeded));
    assert_eq!(h.runs.finished_at_of(RUN), finished_at, "nothing rewritten");
    assert!(
        !can_transition(RunState::Succeeded, RunState::Succeeded),
        "premise: the guard would have refused this pair, so the no-op is what \
         kept it out of the guard"
    );
}

/// **A green completion on a `Dispatching` run records the verdict.**
///
/// `service::dispatch::record_started` swallows a failed
/// `Dispatching -> Running` write - deliberately, because the execution is
/// genuinely running by then and returning an error could trigger a retry that
/// double-submits - so a run really can be `Dispatching` when its execution
/// finishes. This is that run finishing green.
///
/// **Inverted 2026-08-15, and it used to assert the opposite.** The previous
/// version of this test expected `IllegalTransition` here and called it "the
/// genuinely illegal case", which was an accurate description of the code and
/// the wrong thing to want: every *other* exit from `Dispatching` was already
/// legal, so a run that skipped `Running` could record any verdict except the
/// correct one, and was then left for the timeout sweep to relabel `TimedOut`.
/// A passing run recorded as timed out is a worse outcome than the transient
/// write failure that caused it. `domain::state_machine::can_transition` now
/// admits the edge and carries the full argument, including why fixing
/// `record_started` instead was rejected.
#[tokio::test]
async fn a_completion_on_a_dispatching_run_records_its_verdict() {
    let h = harness(RunState::Dispatching, LeaseState::Free).await;
    let ctx = owner();
    h.ingest
        .apply(
            &ctx,
            RUN,
            ExecutionEvent::TestResult(observation("a", "PASSED")),
        )
        .await
        .unwrap();
    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Succeeded))
        .await
        .expect("a run that skipped Running still finished");

    assert_eq!(h.runs.state_of(RUN), Some(RunState::Succeeded));
    assert!(
        can_transition(RunState::Dispatching, RunState::Succeeded),
        "premise: the guard is what this test turns on"
    );
}

/// The sibling that must stay refused, so the widening above is not read as a
/// general relaxation: a run that never reached the executor cannot have
/// succeeded, whatever arrives claiming it did.
#[tokio::test]
async fn a_completion_on_a_queued_run_is_still_reported_rather_than_forced() {
    let h = harness(RunState::Queued, LeaseState::Free).await;
    let ctx = owner();
    let error = h
        .ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Succeeded))
        .await
        .expect_err("a queued run has nothing to have succeeded at");

    assert!(matches!(error, DomainError::IllegalTransition { .. }));
    assert_eq!(h.runs.state_of(RUN), Some(RunState::Queued));
}

// ---------------------------------------------------------------------------
// Logs and the stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_log_line_is_fanned_out_prefixed_with_the_node_that_produced_it() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    h.ingest
        .apply(
            &owner(),
            RUN,
            ExecutionEvent::Log {
                node: "repo-b".to_owned(),
                line: "collected 12 items".to_owned(),
                emitted_at: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        h.logs.lines(),
        vec![(RUN, "[repo-b] collected 12 items".to_owned())],
        "a multi-node run interleaves two logs; the prefix is what disambiguates them"
    );
}

/// **One ingested line is exactly one archived line.** A `line` carrying an
/// embedded `\n` or `\r` must not become two lines once the archive splits
/// its stored text back apart at read time - `fan_out_log` flattens both
/// unconditionally before either consumer sees the string, rather than
/// relying on the executor adapter to never produce one.
///
/// Break-tested by removing the `line` half of the flattening in
/// `fan_out_log`: this goes red on both assertions, `published_lines`
/// returning the line with its embedded `\n` intact and
/// `archive_buffered_text` therefore holding three `\n`-terminated lines for
/// what should be one archive entry.
#[tokio::test]
async fn an_embedded_newline_in_a_log_line_is_flattened_not_split() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    h.ingest_log_line("node-a", "first\nsecond\rthird").await;

    assert_eq!(
        h.published_lines(),
        vec!["[node-a] first second third".to_owned()],
        "a live subscriber must see one event for one ingested line"
    );
    assert_eq!(
        h.archive_buffered_text(),
        "[node-a] first second third\n",
        "and the archive must agree - one line, one trailing newline, not three"
    );
}

/// **The same invariant, over `node`.** The flatten covers the whole string
/// `fan_out_log` builds, not just its `line` half — see that method's doc on
/// why "`ExecutionNode::name` is a `format!(\"repo-{uuid}\")` today" was a
/// fact about a producer rather than an invariant. A `node` carrying a `\n`
/// is strictly worse than a `line` carrying one: the fragment after the split
/// has lost the opening `[` as well as the node name, so nothing marks it as
/// a continuation at all.
///
/// **Break-tested** by reverting `prefixed.extend(node.chars().map(flatten))`
/// to `prefixed.push_str(node)`: both assertions go red, the published line
/// coming back as `"[node\na] out"` and the buffered text holding two
/// `\n`-terminated lines for one ingested line.
#[tokio::test]
async fn an_embedded_newline_in_a_node_name_is_flattened_too() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    h.ingest_log_line("node\na\rb", "out").await;

    assert_eq!(
        h.published_lines(),
        vec!["[node a b] out".to_owned()],
        "a node name must not be able to split one event into several"
    );
    assert_eq!(
        h.archive_buffered_text(),
        "[node a b] out\n",
        "and the archive must hold exactly one line for it"
    );
}

// ---------------------------------------------------------------------------
// The archive: recorded on ingest, flushed at finish
// ---------------------------------------------------------------------------

/// **Archived bytes equal live bytes, prefix included.** The UI's marker
/// parser was taught to strip `[node] ` in `5538cf972`; if the archive stored
/// an unprefixed line, that parser would need a second rule and a replayed
/// log would group differently from a live one.
#[tokio::test]
async fn an_archived_line_is_byte_identical_to_the_live_one() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    h.ingest_log_line("node-a", "PASS | smoke.robot").await;

    assert_eq!(
        h.published_lines(),
        vec!["[node-a] PASS | smoke.robot".to_owned()]
    );
    assert_eq!(
        h.archive_buffered_text(),
        "[node-a] PASS | smoke.robot\n",
        "the archive must hold exactly what subscribers received",
    );
}

/// `finish` flushes, so a run that completes normally is durable without
/// waiting for a tick.
#[tokio::test]
async fn finishing_a_run_archives_its_log() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    h.ingest_log_line("node-a", "starting").await;
    h.finish_run_successfully().await;

    assert_eq!(h.stored_text(), "[node-a] starting\n");
}

/// **A tail that `finish` never flushed is archived by `flush_due`.**
///
/// # What this test does and does not exercise, stated because its name used
/// to overstate it
///
/// It was named "a cancelled run's tail is archived by the tick", and it
/// involves no tick and no cancel. There is no dispatcher here — `flush_due`
/// is called directly — and `Harness::cancel_run` is `self.logs.reap(RUN)` and
/// nothing else; `service::runs::retire` is never constructed, let alone
/// called. Its "precondition: retire must not flush by itself" assertion was
/// therefore true because nothing in the test had flushed, not because
/// `retire` had declined to, which is an assertion that could not fail.
///
/// What it does pin, and what is worth pinning here, is the composition
/// `IngestService` itself owns: a line reaches the archive's **pending
/// buffer** on ingest, `finish` is the module's only flusher so a run that
/// never finishes leaves that buffer un-drained, and a later `flush_due`
/// picks it up. The two assertions before the flush are what make that
/// load-bearing — buffered non-empty *and* stored empty together say "ingest
/// recorded it and nothing has written it yet", which is what makes the
/// `flush_due` below the thing that wrote it. Either one alone would be
/// vacuous.
///
/// # Where D-RLP-4's claim is actually covered
///
/// D-RLP-4 says the tick covers the terminal paths `finish` does not, and its
/// three parts live in three places, none of them this test:
///
/// * **`flush_due` drains every buffered run** — against the real
///   `RunLogArchive` and a real repository, in
///   `infra::logs::archive`'s `flush_due_drains_every_buffered_run`. This test
///   calls `RecordingArchive::flush_due`, a double.
/// * **The dispatcher tick calls it** — `gear.rs`'s
///   `the_dispatcher_tick_flushes_the_archive`, which reads
///   `dispatcher_ticker`'s source for the same reason
///   `init_builds_exactly_one_log_broadcaster` reads `init`'s: the loop needs
///   a database, a `ClientHub` and four resolved cross-gear clients, so
///   nothing can drive it.
/// * **`retire` does not flush** — structural, and so not a test at all:
///   `RunsService` holds no `LogArchive` (`RunsDeps` declares no such field),
///   so `retire` has nothing to flush *through*. That is stronger than any
///   behavioural assertion and is why no test asserts it.
#[tokio::test]
async fn a_tail_that_finish_never_flushed_is_archived_by_flush_due() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    h.ingest_log_line("node-a", "never finished").await;

    assert_eq!(
        h.archive_buffered_text(),
        "[node-a] never finished\n",
        "premise: ingest must have recorded the line into the pending buffer",
    );
    assert_eq!(
        h.stored_text(),
        "",
        "premise: and nothing must have flushed it yet, or the flush_due below \
         proves nothing",
    );

    let report = h.archive.flush_due().await;

    assert_eq!(report.runs, 1);
    assert_eq!(
        report.lines, 1,
        "the report the dispatcher tick logs must count the line it wrote",
    );
    assert_eq!(h.stored_text(), "[node-a] never finished\n");
}

/// A flush failure must not fail the completion. The run's terminal state,
/// its lease release and its reap are what matter; the archive is
/// best-effort beside them.
///
/// **The first assertion is the one that makes this a test.** `run_is_terminal()`
/// is equally true on the success path, so on its own it could not tell "the
/// flush failed and was swallowed" from "the flush succeeded" — or from
/// "`finish` never flushed at all". Asserting the injected failure actually
/// fired is what discriminates them, the same shape
/// `infra::logs::archive`'s `a_failed_flush_keeps_its_text_for_the_next_one`
/// gets right with `assert!(flush(..).await.is_err())`; that form is
/// unavailable here because `finish` swallows the error by design, which is
/// the property under test.
///
/// **Break-tested** two ways: deleting `finish`'s `self.archive.flush(..)`
/// call turns the first assertion red (nothing fires the injection), and
/// changing `finish` to propagate the flush error turns
/// `finish_run_successfully`'s own `.unwrap()` red.
#[tokio::test]
async fn a_failed_archive_flush_does_not_fail_the_completion() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    h.ingest_log_line("node-a", "starting").await;
    h.fail_next_append();

    h.finish_run_successfully().await;

    assert_eq!(
        h.archive_flush_failures(),
        1,
        "the injected failure must actually have fired - otherwise this test \
         passes on a `finish` that never flushed",
    );
    assert_eq!(
        h.stored_text(),
        "",
        "and the failed flush must have stored nothing, which is what tells it \
         apart from a flush that succeeded",
    );
    assert!(
        h.run_is_terminal(),
        "the run must still be recorded terminal",
    );
}

#[tokio::test]
async fn a_terminated_runs_log_channel_is_reaped() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let ctx = owner();
    assert!(h.logs.reaped().is_empty());

    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Failed))
        .await
        .unwrap();
    assert_eq!(h.logs.reaped(), vec![RUN]);

    // And a duplicate completion reaps again rather than leaking, because the
    // no-op branch is the one a retry lands on.
    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Failed))
        .await
        .unwrap();
    assert_eq!(h.logs.reaped(), vec![RUN, RUN]);
}

/// `dispatch` already recorded `Running` when the executor accepted the run,
/// so acting on this event too would attempt an illegal `Running -> Running`.
#[tokio::test]
async fn a_started_event_writes_nothing() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    h.ingest
        .apply(&owner(), RUN, ExecutionEvent::Started)
        .await
        .unwrap();

    assert_eq!(h.runs.state_of(RUN), Some(RunState::Running));
    assert!(h.runs.transitions.lock().unwrap().is_empty());
}

/// The whole stream, in the order an executor emits it, driven through the
/// public entry point rather than event by event.
#[tokio::test]
async fn the_stream_is_consumed_in_order_and_ends_with_the_verdict() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let (sink, mut stream) = crate::domain::ports::run_executor::ExecutionStream::channel(8);
    assert!(sink.emit(ExecutionEvent::Started).await);
    assert!(
        sink.emit(ExecutionEvent::Log {
            node: "repo-a".to_owned(),
            line: "start".to_owned(),
            emitted_at: None,
        })
        .await
    );
    assert!(
        sink.emit(ExecutionEvent::TestResult(observation("a", "FAILED")))
            .await
    );
    assert!(sink.emit(finished(ExecutorOutcome::Succeeded)).await);
    drop(sink);

    h.ingest.ingest(&owner(), RUN, &mut stream).await.unwrap();

    assert_eq!(
        h.runs.state_of(RUN),
        Some(RunState::Failed),
        "a failed result downgrades the green outcome"
    );
    assert_eq!(h.logs.lines().len(), 1);
    assert_eq!(h.logs.reaped(), vec![RUN]);
}

/// A queue row that is not this run's must not be released by this run's
/// completion. The fixture holds two claims on one platform.
#[tokio::test]
async fn completion_releases_only_this_runs_claim() {
    let other_run = Uuid::from_u128(0x0502);
    let other_queue = Uuid::from_u128(0x0E52);
    let runs = Arc::new(FakeRuns::with(vec![
        (
            OWNER_TENANT,
            run_fixture(RUN, Some(PLATFORM_A), false, RunState::Running),
        ),
        (
            OWNER_TENANT,
            run_fixture(other_run, Some(PLATFORM_A), false, RunState::Running),
        ),
    ]));
    let queue = Arc::new(FakeQueue::with(vec![
        row(QUEUE, RUN),
        row(other_queue, other_run),
    ]));
    let h = build(runs, queue, Arc::new(FakeEnvironments::free())).await;

    h.ingest
        .apply(&owner(), RUN, finished(ExecutorOutcome::Failed))
        .await
        .unwrap();

    assert_eq!(h.queue.state_of(QUEUE), Some(QueueState::Done));
    assert_eq!(
        h.queue.state_of(other_queue),
        Some(QueueState::Running),
        "a sibling claim on the same platform is untouched"
    );
}

fn row(id: Uuid, run_id: Uuid) -> QueueRowRecord {
    queued_row(
        id,
        OWNER_TENANT,
        run_id,
        PLATFORM_A,
        false,
        QueueState::Running,
    )
}

// ---------------------------------------------------------------------------
// The security review's findings, as regressions
// ---------------------------------------------------------------------------

/// **B2.** `ExecutionEvent::Log` used to bypass the PEP entirely: no scope, no
/// repository call, no tenancy check. A probe delivered a foreign tenant's line
/// into this run's stream and got `Ok`.
///
/// The second assertion is the one that makes this more than a missing check:
/// `TestResult` and `Finished` answer `RunNotFound` for absent and foreign alike,
/// so before the fix the *discriminator moved one match arm over* — a caller
/// learned from the response which arm it had hit.
#[tokio::test]
async fn a_log_line_from_another_tenant_is_refused_and_never_reaches_the_stream() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let error = h
        .ingest
        .apply(
            &ctx(OTHER_TENANT),
            RUN,
            ExecutionEvent::Log {
                node: "repo-a".to_owned(),
                line: "another tenant's output".to_owned(),
                emitted_at: None,
            },
        )
        .await
        .expect_err("a foreign tenant must not write to this run's log stream");

    assert!(matches!(error, DomainError::RunNotFound { id } if id == RUN));
    assert!(h.logs.lines().is_empty());
}

/// **B2, the existence half.** Every arm must answer the same way for a run that
/// does not exist, or the response tells a prober which arm they reached.
#[tokio::test]
async fn every_event_arm_answers_not_found_for_an_unknown_run() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let stranger = Uuid::from_u128(0xDEAD);
    let events = [
        ExecutionEvent::Log {
            node: "repo-a".to_owned(),
            line: "x".to_owned(),
            emitted_at: None,
        },
        ExecutionEvent::TestResult(observation("t", "PASSED")),
        finished(ExecutorOutcome::Succeeded),
    ];
    for event in events {
        let error = h
            .ingest
            .apply(&owner(), stranger, event.clone())
            .await
            .unwrap_err();
        assert!(
            matches!(error, DomainError::RunNotFound { id } if id == stranger),
            "{event:?} answered {error} instead of RunNotFound"
        );
    }
    assert!(h.logs.lines().is_empty());
}

/// A terminal run's log output is **not** dropped, unlike its results.
///
/// The asymmetry is deliberate and easy to get wrong in the other direction: a
/// log line moves no counter and no verdict, so the reason results are dropped
/// after completion does not apply, and dropping them would truncate the tail of
/// the output an operator is watching.
#[tokio::test]
async fn a_log_line_for_a_terminal_run_is_still_fanned_out() {
    let h = harness(RunState::Succeeded, LeaseState::Free).await;
    h.ingest
        .apply(
            &owner(),
            RUN,
            ExecutionEvent::Log {
                node: "repo-a".to_owned(),
                line: "tail".to_owned(),
                emitted_at: None,
            },
        )
        .await
        .expect("a terminal run's tail output still belongs to its watchers");
    assert_eq!(h.logs.lines(), vec![(RUN, "[repo-a] tail".to_owned())]);
}

/// **B1, the verdict half.** The exact sequence the security review executed:
/// two results, then a delta race on one of them that drives the *columns* to
/// `passed: 2, failed: 0` while a `FAILED` row is still stored.
///
/// The race itself is not reproducible in a single-threaded test — and does not
/// need to be, because what is asserted is the containment: `finish` tallies the
/// rows, so a `Succeeded` outcome over skewed columns still records `Failed`. The
/// skew is injected directly, which is stronger than racing for it: it is the
/// worst state the race can reach.
#[tokio::test]
async fn a_completion_trusts_the_rows_and_not_the_skewed_columns() {
    let h = harness(RunState::Running, LeaseState::Free).await;
    let ctx = owner();
    for name in ["x", "y"] {
        h.ingest
            .apply(
                &ctx,
                RUN,
                ExecutionEvent::TestResult(observation(name, "FAILED")),
            )
            .await
            .unwrap();
    }
    // The second, racing decrement the concurrent retry would have applied. `x`'s
    // row is left `FAILED`, which is what the rows-are-authoritative rule catches.
    h.runs.skew_counts(
        RUN,
        &RunResultDelta {
            passed: 2,
            failed: -2,
            ..RunResultDelta::default()
        },
    );
    assert_eq!(
        h.runs.counts_of(RUN),
        RunResult {
            passed: 2,
            failed: 0,
            total: 2,
            ..RunResult::default()
        },
        "premise: the columns now say the run passed"
    );

    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Succeeded))
        .await
        .unwrap();

    assert_eq!(
        h.runs.state_of(RUN),
        Some(RunState::Failed),
        "two FAILED rows are stored; a green outcome over skewed columns must not \
         produce a passing run"
    );
    assert_eq!(
        h.runs.counts_of(RUN),
        RunResult {
            failed: 2,
            total: 2,
            ..RunResult::default()
        },
        "and the columns are corrected to the rows, so the numbers explain the verdict"
    );
}

/// The tally itself, over the eight known statuses plus a ninth — the source
/// system's projection (`../testrunner/manager/src/routes/plans.rs:188-192`)
/// plus this port's own `xfail` and `xpass` counters, and including that the
/// ninth, unknown status still reaches `total` and none of the six.
#[test]
fn the_tally_is_the_source_systems_projection() {
    let rows: Vec<crate::domain::repos::TestResultRow> = [
        "PASSED",
        "FAILED",
        "ERROR",
        "SKIPPED",
        "PENDING",
        "RUNNING",
        "XFAIL",
        "XPASS",
        "INFRASTRUCTURE_E",
    ]
    .into_iter()
    .map(result_row)
    .collect();

    assert_eq!(
        tally(&rows),
        RunResult {
            passed: 1,
            failed: 2,
            skipped: 1,
            in_progress: 2,
            xfail: 1,
            xpass: 1,
            total: 9,
        }
    );
    assert_eq!(tally(&[]), RunResult::default());
}

/// The correction is a delta, so it composes with the incremental path rather
/// than being a second way to write the counters.
#[test]
fn the_correction_moves_the_stored_counts_onto_the_tally() {
    let stored = RunResult {
        passed: 2,
        failed: 0,
        total: 2,
        ..RunResult::default()
    };
    let wanted = RunResult {
        passed: 0,
        failed: 2,
        total: 2,
        ..RunResult::default()
    };
    assert_eq!(
        correction(stored, wanted),
        RunResultDelta {
            passed: -2,
            failed: 2,
            ..RunResultDelta::default()
        }
    );
    assert_eq!(
        correction(wanted, wanted),
        RunResultDelta::default(),
        "an agreeing pair moves nothing, which is what keeps the common path free"
    );
}

/// **Adopted from the spec review's mutant.** Deleting the `self.release(..)`
/// from `finish`'s equality no-op branch left all 488 tests green, because no
/// shipped fixture put a *terminal* run next to a *held* lease — a state only an
/// operator cancel produces, since `service::runs::stop_execution` records
/// `Canceled` and deliberately releases nothing.
///
/// Without the release the platform reads busy forever from the queue's point of
/// view: `lease_occupancy` fails closed to `Occupancy::Exclusive`,
/// `platform_admits` refuses everything, and every later launch on that platform
/// queues — bounded by one tick only if the reconciler is running at all.
#[tokio::test]
async fn a_completion_for_an_already_cancelled_run_still_hands_back_the_platform() {
    let h = harness(
        RunState::Canceled,
        LeaseState::HeldExclusive { holder: RUN },
    )
    .await;
    let ctx = owner();
    assert_eq!(
        h.environments.get_lease(&ctx, PLATFORM_A).await.unwrap(),
        LeaseState::HeldExclusive { holder: RUN },
        "premise: an operator cancel left the lease held"
    );

    h.ingest
        .apply(&ctx, RUN, finished(ExecutorOutcome::Canceled))
        .await
        .expect("a completion for an already-terminal run is a no-op, not a fault");

    assert_eq!(
        h.runs.state_of(RUN),
        Some(RunState::Canceled),
        "nothing was rewritten"
    );
    assert_eq!(
        h.environments.get_lease(&ctx, PLATFORM_A).await.unwrap(),
        LeaseState::Free,
        "but the platform was handed back: this event is the evidence the execution \
         really ended, which is the one moment releasing is correct"
    );
    assert_eq!(h.queue.state_of(QUEUE), Some(QueueState::Done));
}

fn result_row(status: &str) -> crate::domain::repos::TestResultRow {
    let stamp = time::OffsetDateTime::now_utc();
    crate::domain::repos::TestResultRow {
        id: Uuid::new_v4(),
        run_id: RUN,
        test_file: "tests/a.py".to_owned(),
        test_name: status.to_owned(),
        status: status.to_owned(),
        duration: None,
        launch_id: None,
        jira_key: None,
        nodeid: String::new(),
        reason: None,
        ticket: None,
        created_at: stamp,
        updated_at: stamp,
    }
}

// ---------------------------------------------------------------------------
// Telemetry: one ingest pass is one measured observation
// ---------------------------------------------------------------------------

/// An ingest emission port that panics on its only method. See
/// `dispatch_tests`' `PanickingMeter` for why one is needed at all.
struct PanickingIngestMeter;

impl IngestMetrics for PanickingIngestMeter {
    fn ingest_batch(&self, _outcome: IngestOutcome, _duration: std::time::Duration) {
        panic!("a metrics adapter must never be able to fail the path it measures");
    }
}

/// The default fixture with a real adapter installed over a private meter.
async fn metered_harness(probe: &MetricsProbe) -> Harness {
    let run = qa_runs_sdk::Run {
        started_at: Some(time::OffsetDateTime::now_utc()),
        execution_ref: Some("mock-execution-1".to_owned()),
        ..run_fixture(RUN, Some(PLATFORM_A), true, RunState::Running)
    };
    build_with_metrics(
        Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])),
        Arc::new(FakeQueue::with(vec![queued_row(
            QUEUE,
            OWNER_TENANT,
            RUN,
            PLATFORM_A,
            true,
            QueueState::Running,
        )])),
        Arc::new(FakeEnvironments::holding(PLATFORM_A, LeaseState::Free)),
        probe.adapter(),
    )
    .await
}

/// **An ingest pass emits exactly one counter increment and one duration.**
///
/// Driven through the real `OTel` SDK with an in-memory exporter for the reason
/// `a_dispatch_pass_records_one_observation` gives: what needs proving is that
/// the rendered series is what a dashboard query finds.
#[tokio::test]
async fn an_ingest_pass_records_one_observation() {
    let probe = MetricsProbe::new();
    let h = metered_harness(&probe).await;

    h.ingest_log_line("repo-a", "hello").await;

    let series = probe.collect();
    assert_eq!(
        series.counter(QA_RUNS_INGEST),
        1,
        "one observation, one increment; the exported names were {:?}",
        series.names()
    );
    assert_eq!(series.histogram_count(QA_RUNS_INGEST_DURATION), 1);
    assert_eq!(
        series.counter_with(QA_RUNS_INGEST, &[("outcome", "applied")]),
        1,
        "a log line is an observation applied to a live run"
    );
}

/// **A completion and its retry are told apart.**
///
/// The two are one `Finished` event applied twice, and folding the second into
/// `applied` is what would hide a stuck executor — see [`IngestOutcome`]'s
/// `Duplicate`.
#[tokio::test]
async fn a_completion_and_its_retry_record_different_outcomes() {
    let probe = MetricsProbe::new();
    let h = metered_harness(&probe).await;

    h.ingest
        .apply(&owner(), RUN, finished(ExecutorOutcome::Failed))
        .await
        .unwrap();
    h.ingest
        .apply(&owner(), RUN, finished(ExecutorOutcome::Failed))
        .await
        .unwrap();

    let series = probe.collect();
    assert_eq!(
        series.counter_with(QA_RUNS_INGEST, &[("outcome", "completed")]),
        1,
        "the first event wrote a terminal state"
    );
    assert_eq!(
        series.counter_with(QA_RUNS_INGEST, &[("outcome", "duplicate")]),
        1,
        "the second reconciled to the state already recorded"
    );
    assert_eq!(series.histogram_count(QA_RUNS_INGEST_DURATION), 2);
}

/// **A refused observation records, and is not counted as a failure.**
///
/// A run invisible under the caller's scope is the caller's own request being
/// wrong, which `DomainError::disclosable` is the classification of — so it
/// must not land in the series an alert fires on.
#[tokio::test]
async fn a_refused_ingest_records_a_refusal_not_a_failure() {
    let probe = MetricsProbe::new();
    let h = metered_harness(&probe).await;

    let refused = h
        .ingest
        .apply(
            &ctx(OTHER_TENANT),
            RUN,
            ExecutionEvent::Log {
                node: "repo-a".to_owned(),
                line: "not yours".to_owned(),
                emitted_at: None,
            },
        )
        .await;

    assert!(refused.is_err(), "the fixture must actually be refused");
    let series = probe.collect();
    assert_eq!(
        series.counter_with(QA_RUNS_INGEST, &[("outcome", "refused")]),
        1
    );
    assert_eq!(
        series.counter_with(QA_RUNS_INGEST, &[("outcome", "failed")]),
        0,
        "a scoping refusal is not this gear's fault and must not page anybody"
    );
}

/// **A metric emission never fails an ingest pass.**
///
/// The ingest counterpart of `a_broken_metrics_adapter_does_not_fail_the_pass`,
/// and the one that matters more: this path runs per log line.
#[tokio::test]
async fn a_broken_metrics_adapter_does_not_fail_an_ingest_pass() {
    let run = qa_runs_sdk::Run {
        started_at: Some(time::OffsetDateTime::now_utc()),
        execution_ref: Some("mock-execution-1".to_owned()),
        ..run_fixture(RUN, Some(PLATFORM_A), true, RunState::Running)
    };
    let h = build_with_metrics(
        Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)])),
        Arc::new(FakeQueue::default()),
        Arc::new(FakeEnvironments::holding(PLATFORM_A, LeaseState::Free)),
        Arc::new(PanickingIngestMeter),
    )
    .await;

    // Must not panic, and must still do the work.
    h.ingest_log_line("repo-a", "hello").await;

    assert_eq!(
        h.published_lines(),
        vec!["[repo-a] hello".to_owned()],
        "a broken metrics adapter must not change what ingest does"
    );
}

/// **The inert `Started` event is not counted as an observation.**
///
/// `apply`'s `Started` arm returns `Ok` without reading or writing anything —
/// the module header calls it deliberately inert. Counting it would put a no-op
/// in the `applied` series, inflating the rate an operator reads as ingest
/// throughput, and would feed the duration histogram a near-zero sample whose
/// only effect is to drag down the p95 the family exists to report.
///
/// The pairing is what makes this a gate rather than an assertion that nothing
/// happened: the same harness then applies a log line and must record exactly
/// one observation, so a green result cannot come from an uninstalled adapter.
#[tokio::test]
async fn the_inert_started_event_is_not_counted_as_an_observation() {
    let probe = MetricsProbe::new();
    let h = metered_harness(&probe).await;

    h.ingest
        .apply(&owner(), RUN, ExecutionEvent::Started)
        .await
        .unwrap();

    assert_eq!(
        probe.collect().counter(QA_RUNS_INGEST),
        0,
        "an event that reads nothing and writes nothing is not an observation"
    );

    h.ingest_log_line("repo-a", "hello").await;

    let series = probe.collect();
    assert_eq!(
        series.counter(QA_RUNS_INGEST),
        1,
        "premise: the adapter is installed and the next event was counted"
    );
    assert_eq!(series.histogram_count(QA_RUNS_INGEST_DURATION), 1);
}

/// A [`LogArchive`] that takes a known, non-trivial amount of wall-clock time
/// to flush, delegating everything else to [`RecordingArchive`].
///
/// `finish` flushes the run's buffered log before it returns, so a delay there
/// puts a floor under one whole ingest pass. Its only job is to make the
/// *magnitude* of the recorded sample assertable: every other double in this
/// file answers instantly, so a call site that recorded a constant would
/// produce a plausible sample and no count-based assertion could tell.
struct SlowArchive {
    inner: RecordingArchive,
    delay: std::time::Duration,
}

#[async_trait::async_trait]
impl LogArchive for SlowArchive {
    fn record(
        &self,
        tenant_id: Uuid,
        run_id: Uuid,
        node: &str,
        line: &str,
        emitted_at: Option<OffsetDateTime>,
    ) {
        self.inner.record(tenant_id, run_id, node, line, emitted_at);
    }

    async fn flush(&self, run_id: Uuid) -> Result<(), DomainError> {
        tokio::time::sleep(self.delay).await;
        self.inner.flush(run_id).await
    }

    async fn flush_due(&self) -> FlushReport {
        self.inner.flush_due().await
    }

    async fn resume_positions(
        &self,
        tenant: system_actor::TenantBound,
        run_id: Uuid,
    ) -> Result<crate::domain::repos::LogResume, DomainError> {
        self.inner.resume_positions(tenant, run_id).await
    }
}

/// **The recorded ingest duration really is a clock around the pass.**
///
/// Every other ingest metric assertion here is about counts and labels, and
/// counts cannot see a *value*: a call site that recorded `Duration::ZERO`, or
/// a constant, or an `Instant` taken in the wrong place would satisfy all of
/// them and hand a dashboard a fabricated distribution. Measured in
/// qa-environments during Task 40 — a mutation replacing the elapsed time with
/// a six-second constant passed that gear's whole suite, and this family had
/// the same hole.
///
/// Written as **two bracketing assertions rather than one equality**, because
/// an equality would be a timing test:
///
/// * the archive's flush sleeps 150 ms and `finish` awaits it before returning,
///   so the sample cannot be in a bucket whose upper edge is 100 ms or below —
///   that direction is deterministic, since a sleep can only overrun;
/// * and it must not be in the `(5 s, 10 s]` bucket, which no in-memory fixture
///   can honestly reach.
///
/// Probed edge by edge rather than through one call: `histogram_bucket_of`
/// answers for the single bucket a value falls in, so asking about one edge
/// says nothing about the buckets below it.
///
/// The `Finished` event is the one driven here rather than a log line, because
/// the flush this fixture slows down happens in `finish` alone — a log line's
/// `record` is synchronous by contract and would leave the delay outside the
/// measured pass.
#[tokio::test]
async fn the_recorded_ingest_duration_tracks_the_pass_it_measures() {
    let delay = std::time::Duration::from_millis(150);
    let probe = MetricsProbe::new();

    let run = qa_runs_sdk::Run {
        started_at: Some(time::OffsetDateTime::now_utc()),
        execution_ref: Some("mock-execution-1".to_owned()),
        ..run_fixture(RUN, Some(PLATFORM_A), true, RunState::Running)
    };
    let db = test_db_provider().await;
    let runs = Arc::new(FakeRuns::with(vec![(OWNER_TENANT, run)]));
    let queue = Arc::new(FakeQueue::with(vec![queued_row(
        QUEUE,
        OWNER_TENANT,
        RUN,
        PLATFORM_A,
        true,
        QueueState::Running,
    )]));
    let environments = Arc::new(FakeEnvironments::holding(PLATFORM_A, LeaseState::Free));
    let ingest = IngestService::new(IngestDeps {
        db: SerializedDb::new(db),
        runs,
        queue,
        environments: environments as Arc<dyn QaEnvironmentsClientV1>,
        logs: Arc::new(RecordingLogs::default()) as Arc<dyn LogFanout>,
        archive: Arc::new(SlowArchive {
            inner: RecordingArchive::default(),
            delay,
        }) as Arc<dyn LogArchive>,
        policy_enforcer: authz_resolver_sdk::PolicyEnforcer::new(Arc::new(SystemGrantingAuthZ)),
        metrics: probe.adapter(),
    });

    ingest
        .apply(&owner(), RUN, finished(ExecutorOutcome::Succeeded))
        .await
        .unwrap();

    let series = probe.collect();
    assert_eq!(
        series.histogram_count(QA_RUNS_INGEST_DURATION),
        1,
        "premise: exactly one ingest pass was timed"
    );
    for edge in [0.005_f64, 0.01, 0.025, 0.05, 0.1] {
        assert_eq!(
            series.histogram_bucket_of(QA_RUNS_INGEST_DURATION, edge),
            Some(0),
            "a pass whose archive flush slept for {delay:?} cannot have been measured at \
             {edge} s or less, so that bucket must be empty -- a zero or a near-zero here \
             means the clock is not around the pass"
        );
    }
    assert_eq!(
        series.histogram_bucket_of(QA_RUNS_INGEST_DURATION, 6.0),
        Some(0),
        "and it cannot have taken between five and ten seconds either: an in-memory \
         fixture does not, so a sample there is a fabricated or stale duration rather \
         than a measured one"
    );
}

/// The categorised counters must account for **every** row of a suite whose
/// statuses are all known — over **every** known status, not a chosen subset.
///
/// This is the arithmetic the report, the run card and every analytics number
/// built on the counters assume and never stated: a reader who sees `total: 4`
/// and `passed + failed + skipped + in_progress == 3` has no way to learn where
/// the fourth row went. `XFAIL` and `XPASS` are the statuses that used to break
/// it — both are *known* values, so "the open set swallowed it" is not the
/// explanation — and each having its own counter is what makes the sum close.
///
/// **This asserted a limit and no longer does.** An earlier version pinned that
/// `XPASS` was deliberately uncounted, with an `assert_ne!` on a two-row suite,
/// because the `xfail` work stopped short of it. The owner's intent was that
/// the buckets reconcile, so the limit was removed rather than documented. The
/// loop below is the replacement: a suite carrying one row of every known
/// status reconciles, which no subset-based assertion can claim.
///
/// The unknown status is still outside the guarantee, and
/// `the_tally_is_the_source_systems_projection` is where that is pinned: it
/// tallies a ninth, invented value into `total` and no counter.
#[test]
fn every_categorised_counter_sums_to_the_total() {
    let every_known_status = [
        "PASSED",
        "FAILED",
        "ERROR",
        "SKIPPED",
        "PENDING",
        "RUNNING",
        "XFAIL",
        "XPASS",
    ];
    let rows: Vec<crate::domain::repos::TestResultRow> =
        every_known_status.into_iter().map(result_row).collect();

    let counts = tally(&rows);

    assert_eq!(
        counts.passed
            + counts.failed
            + counts.skipped
            + counts.in_progress
            + counts.xfail
            + counts.xpass,
        counts.total,
        "the categorised counters must sum to the total; a known status that reaches \
         `total` and no counter is a row the reader cannot account for"
    );
    assert_eq!(counts.xfail, 1, "the expected failure lands in its own counter");
    assert_eq!(
        counts.xpass, 1,
        "and the unexpected pass in its own, which is what closes the sum"
    );

    // Each status alone, so a compensating pair of errors cannot make the
    // aggregate above pass. `XPASS` on its own is the case that used to fail.
    for status in every_known_status {
        let counts = tally(&[result_row(status)]);
        assert_eq!(
            counts.passed
                + counts.failed
                + counts.skipped
                + counts.in_progress
                + counts.xfail
                + counts.xpass,
            counts.total,
            "a one-row suite of {status} must reconcile"
        );
    }
}
