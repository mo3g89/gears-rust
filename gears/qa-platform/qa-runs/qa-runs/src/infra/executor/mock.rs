//! Deterministic in-memory `RunExecutor` (ADR-0001's p1 adapter).
//!
//! Not a stub: this is the adapter every non-execution test runs against, so it
//! is the thing that makes the launch path, the dispatcher, cancellation and
//! crash recovery verifiable before serverless-runtime exists — which, as of
//! 2026-08-13, is a docs-only gear with zero `.rs` files, so "before" is doing
//! real work here.
//!
//! Deliberately deterministic: a scripted event sequence per run, no sleeps a
//! test can race, and no randomness. A mock that needed `tokio::time` to settle
//! would make every dispatcher test flaky. `watch` materialises the whole
//! sequence into the channel before returning, so a test never has to wait for
//! a producer task to be scheduled.
//!
//! Five properties, in the order they matter, each with the test that pins it:
//!
//! 1. **Deterministic** — a scripted sequence per `run_id`, defaulting to a
//!    passing single-test run
//!    (`the_default_script_completes_the_run_successfully`).
//! 2. **Honours `cancel`** — a cancelled execution's stream ends
//!    `Finished { outcome: Canceled, .. }`, so the cancel path is exercised end
//!    to end without 2.7 (`cancel_makes_the_stream_finish_as_canceled`).
//! 3. **Re-attachable** — `watch` twice on one reference with no resume
//!    position given yields the sequence twice, so crash recovery is testable
//!    (`watch_is_re_attachable_and_replays_from_the_beginning`). Given a
//!    non-empty [`LogResume`], `watch` instead replays *from* it — skipping
//!    exactly the `Log` entries per node it says are already archived — which
//!    is Task 13's fix for review finding #50 and is what makes both
//!    directions (no lines lost, no lines duplicated) testable at once
//!    (`watch_resumes_without_duplicating_or_dropping_log_lines`).
//! 4. **Records what it was given** — `MockRunExecutor::submitted` is how
//!    environment assembly, node grouping and the
//!    reference-not-material rule get verified end to end
//!    (`the_recorded_spec_carries_the_secret_as_a_reference_and_never_a_value`).
//! 5. **Injectable failures** — `start`, `watch` and `list_active` can each be
//!    made to fail, so the fail-safe directions have something to fail against
//!    (`start_reports_the_injected_failure`,
//!    `watch_reports_the_injected_failure`,
//!    `list_active_reports_the_injected_failure`). `watch`'s arrived with Task
//!    16c: an errored `watch` and an empty stream are the two answers the port
//!    forbids conflating, and only one of them was constructible.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::{
    ExecutionEvent, ExecutionRef, ExecutionStream, NodeOutcome, RunExecutor, RunSpec,
    TestObservation,
};
use crate::domain::repos::LogResume;
use crate::domain::state_machine::ExecutorOutcome;

/// Test name the default script reports. Uppercase status, matching the
/// vocabulary the source system stores (`manager/src/services/argo.rs:2932-2943`),
/// so a run driven by the default script exercises the same counter mapping a
/// real one does.
const DEFAULT_TEST_NAME: &str = "test_mock_default";

/// Everything the mock remembers.
///
/// `scripts` is keyed by `run_id` rather than by [`ExecutionRef`] because a
/// test scripts a run *before* dispatching it, when no reference exists yet —
/// the reference is minted by [`RunExecutor::start`].
#[derive(Debug, Default)]
struct MockState {
    scripts: BTreeMap<Uuid, Vec<ExecutionEvent>>,
    /// `Arc`, not `RunSpec`: [`RunSpec`] is no longer `Clone` — its
    /// [`RunAccess`](crate::domain::ports::run_executor::RunAccess) may carry a
    /// resolved credential, and duplicating one is what that type refuses. A
    /// shared handle records every field just as faithfully.
    submitted: Vec<Arc<RunSpec>>,
    executions: BTreeMap<ExecutionRef, Uuid>,
    active: BTreeSet<ExecutionRef>,
    cancelled: BTreeSet<ExecutionRef>,
    fail_start: Option<String>,
    fail_watch: Option<String>,
    fail_list_active: Option<String>,
    sequence: u64,
}

impl MockState {
    /// The event sequence one reference should replay, with cancellation
    /// applied.
    ///
    /// An unknown reference yields nothing at all, which is the port's
    /// contract: an empty stream means "nothing more to say", never "this
    /// failed" — the executor being unreachable is the [`Err`] case, and
    /// conflating the two is what would release a claim on an outage
    /// (`manager/src/services/run_dispatcher.rs:237-252`).
    fn script_for(&self, execution_ref: &ExecutionRef) -> Vec<ExecutionEvent> {
        let Some(run_id) = self.executions.get(execution_ref) else {
            return Vec::new();
        };
        let mut events = match self.scripts.get(run_id) {
            Some(scripted) => scripted.clone(),
            None => self
                .submitted
                .iter()
                .find(|spec| spec.run_id == *run_id)
                .map_or_else(Vec::new, |spec| default_script(spec)),
        };
        if self.cancelled.contains(execution_ref) {
            // Everything observed before the terminal event stands — a
            // cancelled run keeps the results it already produced, exactly as
            // the source system keeps the rows the runner already POSTed. Only
            // the ending changes.
            if let Some(terminal) = events
                .iter()
                .position(|event| matches!(event, ExecutionEvent::Finished { .. }))
            {
                events.truncate(terminal);
            }
            events.push(ExecutionEvent::Finished {
                outcome: ExecutorOutcome::Canceled,
                nodes: NodeOutcome::Unknown,
                message: Some("cancelled through RunExecutor::cancel".to_owned()),
            });
        }
        events
    }
}

/// A passing single-test run, for a spec nobody scripted.
///
/// Attributed to the first node so the result is not orphaned: `node` has to
/// name a real [`ExecutionNode`](crate::domain::ports::run_executor::ExecutionNode),
/// or a test asserting per-node attribution would pass against a value no
/// executor could produce.
fn default_script(spec: &RunSpec) -> Vec<ExecutionEvent> {
    let (node, test_file) = match spec.nodes.first() {
        Some(first) => (
            first.name.clone(),
            first.test_files.first().cloned().unwrap_or_default(),
        ),
        None => (String::new(), String::new()),
    };
    vec![
        ExecutionEvent::Started,
        ExecutionEvent::TestResult(TestObservation {
            node,
            test_file,
            test_name: DEFAULT_TEST_NAME.to_owned(),
            status: "PASSED".to_owned(),
            duration: None,
            launch_id: None,
            jira_key: None,
            // All three `None`, which is a *choice about what this mock is*
            // rather than a placeholder. The default script models an executor
            // that reports no per-case detail — the "older runner" case
            // legacy's analytics has a fallback for
            // (`manager/src/routes/analytics.rs:97-98`) — so the row it
            // produces is file-level, and a file-level row is precisely the one
            // with no `nodeid`. A test wanting the case-level shape scripts it.
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

/// The p1 execution adapter.
///
/// Cloneable, and cloning shares one state: a test keeps a handle while the
/// dispatcher holds the same mock as an `Arc<dyn RunExecutor>`, then asserts on
/// `submitted()` afterwards.
#[derive(Clone, Debug, Default)]
pub struct MockRunExecutor {
    state: Arc<Mutex<MockState>>,
}

impl MockRunExecutor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Recover the guard on a poisoned lock rather than panicking.
    ///
    /// Poisoning means some other test panicked while holding it; propagating
    /// that as a second panic here reports the wrong test as the failure, and
    /// the state behind it is a plain collection with no invariant a panic
    /// could have broken halfway.
    fn lock(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Script the events one run's execution will replay, replacing any
    /// previous script. Set before dispatching the run.
    ///
    /// **`pub`, and deliberately not `#[cfg(test)] pub(crate)`**, like the four
    /// control methods that follow. An earlier revision gated them on
    /// `cfg(test)` to silence a dead-code warning — which worked, and made the
    /// mock's entire control surface invisible to exactly the tests ADR-0001
    /// requires: "Integration suite runs the full launch→ingest path against
    /// the mock adapter" (ADR-0001, Confirmation). An integration test compiles
    /// the library *without* `cfg(test)`, so a `tests/dispatcher.rs` would have
    /// been able to start runs and nothing else — no scripts, no injected
    /// failures, no assertions on what was submitted. This gear's siblings
    /// already use that pattern (`qa-catalog/tests/gix_sync_integration.rs`,
    /// `tests/multi_branch.rs`).
    ///
    /// So these are the public API of a test double, and the dead-code warning
    /// was a symptom of their being crate-private rather than a reason to hide
    /// them: `pub` items on a `pub` type are never dead code.
    pub fn script(&self, run_id: Uuid, events: Vec<ExecutionEvent>) {
        self.lock().scripts.insert(run_id, events);
    }

    /// Make every subsequent [`RunExecutor::start`] fail with this message.
    pub fn fail_start(&self, message: impl Into<String>) {
        self.lock().fail_start = Some(message.into());
    }

    /// Make every subsequent [`RunExecutor::watch`] fail with this message.
    ///
    /// **Added by Task 16c, and the distinction it exists for is the one the
    /// port spends three paragraphs on**: a `watch` that *errors* means nothing
    /// is known about the execution, while a `watch` that succeeds and yields
    /// *nothing* means "nothing more to say". Both leave the run untouched and
    /// they are reached by entirely different code, so without this the error
    /// arm of every watcher was unreachable from any test and its "retires
    /// nothing" contract was prose.
    ///
    /// An unknown reference is the other half and needs no injection: it already
    /// yields an empty stream (`script_for`).
    pub fn fail_watch(&self, message: impl Into<String>) {
        self.lock().fail_watch = Some(message.into());
    }

    /// Make every subsequent [`RunExecutor::list_active`] fail with this
    /// message.
    pub fn fail_list_active(&self, message: impl Into<String>) {
        self.lock().fail_list_active = Some(message.into());
    }

    /// Clear every injected failure, so a test can model an executor coming
    /// back — which is the case that distinguishes "left alone" from "left
    /// alone forever".
    pub fn clear_failures(&self) {
        let mut state = self.lock();
        state.fail_start = None;
        state.fail_watch = None;
        state.fail_list_active = None;
    }

    /// The executor forgets an execution: it drops off
    /// [`RunExecutor::list_active`] and **nothing else changes** — no terminal
    /// event is rewritten, no stream is altered.
    ///
    /// This is the ordinary end of a run's life, and without it the mock could
    /// not produce it. [`RunExecutor::cancel`] was the only path that removed
    /// an execution from the active set, and it unconditionally rewrites the
    /// stream's terminal event to `Canceled` — so "an execution finished
    /// normally and then dropped off the executor's list" was
    /// **unconstructible**, and that state is
    /// [`ClaimExecution::Gone`](crate::domain::state_machine::ClaimExecution::Gone)
    /// → [`ClaimAction::Release`](crate::domain::state_machine::ClaimAction::Release),
    /// the single most important input to
    /// [`reconcile_claim`](crate::domain::state_machine::reconcile_claim) and
    /// the reason the source system's reconciler exists at all
    /// (`manager/src/services/run_dispatcher.rs:448-456`). Tasks 14 and 15
    /// would otherwise have had to mislabel such a run as cancelled, or
    /// hand-build a `ClaimObservation` and test the pure function instead of
    /// the wiring.
    ///
    /// `cancel` is this plus the terminal-event rewrite; the two differ in
    /// exactly that.
    pub fn finish(&self, execution_ref: &ExecutionRef) {
        self.lock().active.remove(execution_ref);
    }

    /// The specs the mock **accepted**, in submission order.
    ///
    /// A rejected submit produced no execution, so it is not recorded — a test
    /// asserting "the dispatcher submitted exactly this" should not have to
    /// filter out attempts that never happened.
    /// Handles rather than copies, because [`RunSpec`] is no longer `Clone` —
    /// its `RunAccess` may carry a resolved credential, and duplicating one is
    /// what that type refuses. A caller reads through the `Arc` exactly as it
    /// read through the value: `submitted()[0].env`, `&submitted()[0]` passed
    /// to a `&RunSpec` parameter, all unchanged.
    #[must_use]
    pub fn submitted(&self) -> Vec<Arc<RunSpec>> {
        self.lock().submitted.clone()
    }
}

#[async_trait]
impl RunExecutor for MockRunExecutor {
    async fn start(&self, spec: RunSpec) -> Result<ExecutionRef, DomainError> {
        let mut state = self.lock();
        // The injected failure is checked first: it is the test's explicit
        // instruction, and a test injecting a submit failure should not have to
        // build a spec that would otherwise be valid.
        if let Some(message) = &state.fail_start {
            return Err(DomainError::ExecutorFailed(message.clone()));
        }
        if spec.nodes.is_empty() {
            return Err(DomainError::ExecutorFailed(format!(
                "run {} was submitted with no execution nodes",
                spec.run_id
            )));
        }
        state.sequence += 1;
        let execution_ref = ExecutionRef::new(format!("mock-execution-{}", state.sequence));
        state.executions.insert(execution_ref.clone(), spec.run_id);
        state.active.insert(execution_ref.clone());
        state.submitted.push(Arc::new(spec));
        Ok(execution_ref)
    }

    async fn watch(
        &self,
        execution_ref: &ExecutionRef,
        resume: LogResume,
    ) -> Result<ExecutionStream, DomainError> {
        if let Some(message) = &self.lock().fail_watch {
            return Err(DomainError::ExecutorFailed(message.clone()));
        }
        // Resolved under the lock, replayed outside it: holding a std mutex
        // across an await is denied for good reason, and the whole sequence is
        // known before the first send.
        let events = self.lock().script_for(execution_ref);
        let (sink, stream) = ExecutionStream::channel(events.len().max(1));
        // **Resume, not replay** — Task 13, review finding #50. The mock has
        // no real log to seek within, so it does what the Argo adapter now
        // does too (fix-round 1): skip exactly the `Log` entries `resume`
        // says this node's archive already has, in order, and keep
        // everything else. `remaining` starts at `resume.lines_for(node)` per
        // node the first time that node is seen and counts down, so a node
        // `resume` says nothing about (the common case: a first attach,
        // where `resume` is empty) skips nothing.
        //
        // This keeps the mock's long-standing "replays from the beginning"
        // guarantee (no lines lost —
        // `watch_is_re_attachable_and_replays_from_the_beginning` below) while
        // making the *duplication* direction falsifiable too
        // (`watch_resumes_without_duplicating_or_dropping_log_lines`), which
        // an adapter that silently dropped the gap could not pass.
        let mut remaining: HashMap<String, i64> = HashMap::new();
        for event in events {
            if let ExecutionEvent::Log { node, .. } = &event {
                let left = remaining
                    .entry(node.clone())
                    .or_insert_with(|| resume.lines_for(node));
                if *left > 0 {
                    *left -= 1;
                    continue;
                }
            }
            // Capacity is the script's length and the observer is still alive
            // here, so this cannot report a dropped observer — written the way
            // an adapter must write it anyway, because the mock is the shape
            // 2.7 will copy.
            if !sink.emit(event).await {
                break;
            }
        }
        Ok(stream)
    }

    async fn cancel(&self, execution_ref: &ExecutionRef) -> Result<(), DomainError> {
        // Idempotent, and accepting of a reference the mock has never seen —
        // the port promises both, and the source system's cancel is a blind
        // patch that writes no state either
        // (`manager/src/routes/runs.rs:1092-1106`).
        //
        // This is `MockRunExecutor::finish` plus the terminal-event rewrite in
        // `script_for`. Use `finish` for a run that simply ended: cancelling to
        // make an execution disappear from `list_active` would also relabel its
        // outcome.
        let mut state = self.lock();
        state.cancelled.insert(execution_ref.clone());
        state.active.remove(execution_ref);
        Ok(())
    }

    async fn list_active(&self) -> Result<BTreeSet<ExecutionRef>, DomainError> {
        let state = self.lock();
        if let Some(message) = &state.fail_list_active {
            return Err(DomainError::ExecutorFailed(message.clone()));
        }
        Ok(state.active.clone())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::domain::ports::run_executor::{
        EnvSource, ExecutionNode, RunAccess, RunEnv, RunnerSpec, SecretRef,
    };
    use crate::domain::runvars::{self, RunVar, RunVarInputs};

    fn node() -> ExecutionNode {
        ExecutionNode {
            name: "repo-smoke".to_owned(),
            bundle_ref: "bundle-store://abc".to_owned(),
            test_files: vec!["tests/test_smoke.py".to_owned()],
        }
    }

    fn spec_for(run_id: Uuid) -> RunSpec {
        RunSpec {
            run_id,
            run_name: "smoke-tests-1".to_owned(),
            nodes: vec![node()],
            env: RunEnv::default(),
            access: RunAccess::default(),
            runner: RunnerSpec::default(),
            timeout_seconds: 3600,
        }
    }

    async fn drain(stream: &mut ExecutionStream) -> Vec<ExecutionEvent> {
        let mut collected = Vec::new();
        while let Some(event) = stream.recv().await {
            collected.push(event);
        }
        collected
    }

    fn finished(outcome: ExecutorOutcome) -> ExecutionEvent {
        ExecutionEvent::Finished {
            outcome,
            nodes: NodeOutcome::NoneFailed,
            message: None,
        }
    }

    #[tokio::test]
    async fn start_records_the_spec_and_returns_a_reference() {
        let executor = MockRunExecutor::new();
        let run_id = Uuid::new_v4();

        let execution_ref = executor.start(spec_for(run_id)).await.unwrap();

        assert_eq!(execution_ref.as_str(), "mock-execution-1");
        let recorded = executor.submitted();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].run_id, run_id);
        assert_eq!(recorded[0].nodes, vec![node()]);
        assert_eq!(recorded[0].timeout_seconds, 3600);
    }

    #[tokio::test]
    async fn start_reports_the_injected_failure() {
        let executor = MockRunExecutor::new();
        executor.fail_start("submission refused");

        let error = executor.start(spec_for(Uuid::new_v4())).await.unwrap_err();

        assert!(matches!(
            error,
            DomainError::ExecutorFailed(message) if message == "submission refused"
        ));
        assert!(
            executor.submitted().is_empty(),
            "a rejected submit produced no execution, so it is not recorded"
        );
    }

    /// A run with no nodes would execute nothing and report success. The port
    /// makes that a submit failure; an empty `test_files` inside a node stays
    /// legal, because the source system pushes `TEST_FILES` even when blank
    /// (`manager/src/services/argo.rs:436`).
    #[tokio::test]
    async fn start_rejects_a_spec_with_no_execution_nodes() {
        let executor = MockRunExecutor::new();
        let mut empty = spec_for(Uuid::new_v4());
        empty.nodes.clear();

        let error = executor.start(empty).await.unwrap_err();
        assert!(matches!(error, DomainError::ExecutorFailed(_)));

        let mut no_files = spec_for(Uuid::new_v4());
        no_files.nodes[0].test_files.clear();
        assert!(executor.start(no_files).await.is_ok());
    }

    #[tokio::test]
    async fn watch_replays_the_scripted_events_in_order() {
        let executor = MockRunExecutor::new();
        let run_id = Uuid::new_v4();
        let script = vec![
            ExecutionEvent::Started,
            ExecutionEvent::Log {
                node: "repo-smoke".to_owned(),
                line: "collecting ...".to_owned(),
            },
            finished(ExecutorOutcome::Succeeded),
        ];
        executor.script(run_id, script.clone());

        let execution_ref = executor.start(spec_for(run_id)).await.unwrap();
        let mut stream = executor
            .watch(&execution_ref, LogResume::default())
            .await
            .unwrap();

        assert_eq!(drain(&mut stream).await, script);
    }

    /// `cpt-cf-qa-nfr-run-duration` requires an 8-hour run to survive a
    /// control-plane restart, allocated to "`watch(execution_id)` resumes"
    /// (`DESIGN.md:57`). With no resume position given — `LogResume::default()`,
    /// what a first attach always passes — the mock replays from the
    /// beginning, which is stronger than the contract asks for and is what
    /// makes the crash-recovery path assertable. Given an actual resume
    /// position it does not replay in full; see the test below for that half.
    #[tokio::test]
    async fn watch_is_re_attachable_and_replays_from_the_beginning() {
        let executor = MockRunExecutor::new();
        let run_id = Uuid::new_v4();
        executor.script(
            run_id,
            vec![
                ExecutionEvent::Started,
                finished(ExecutorOutcome::Succeeded),
            ],
        );
        let execution_ref = executor.start(spec_for(run_id)).await.unwrap();

        let mut first = executor
            .watch(&execution_ref, LogResume::default())
            .await
            .unwrap();
        let first_events = drain(&mut first).await;
        let mut second = executor
            .watch(&execution_ref, LogResume::default())
            .await
            .unwrap();

        assert_eq!(drain(&mut second).await, first_events);
        assert_eq!(first_events.len(), 2);
    }

    /// **Both directions of Finding #50's fix.** Since fix-round 1 the Argo
    /// adapter uses this same count-based mechanism against its own re-read
    /// pod log (`infra::executor::argo::watch`'s `LineSkip`); this pins what
    /// "resume, don't replay" means against the mock's in-memory script,
    /// where nothing stands in the way of doing it exactly.
    ///
    /// Two nodes, so a resume position for one cannot be satisfied by
    /// accident from the other's count.
    #[tokio::test]
    async fn watch_resumes_without_duplicating_or_dropping_log_lines() {
        let executor = MockRunExecutor::new();
        let run_id = Uuid::new_v4();
        let script = vec![
            ExecutionEvent::Started,
            ExecutionEvent::Log {
                node: "a".to_owned(),
                line: "a-one".to_owned(),
            },
            ExecutionEvent::Log {
                node: "b".to_owned(),
                line: "b-one".to_owned(),
            },
            ExecutionEvent::Log {
                node: "a".to_owned(),
                line: "a-two".to_owned(),
            },
            ExecutionEvent::Log {
                node: "a".to_owned(),
                line: "a-three".to_owned(),
            },
            finished(ExecutorOutcome::Succeeded),
        ];
        executor.script(run_id, script.clone());
        let execution_ref = executor.start(spec_for(run_id)).await.unwrap();

        // "Two of node a's three lines and none of node b's are already
        // archived" -- what a real re-attach's `log_resume_positions` would
        // answer partway through this script.
        let resume: LogResume = [(
            "a".to_owned(),
            crate::domain::repos::LogPosition {
                lines: 2,
                first_line: "a-one".to_owned(),
                last_line: "a-two".to_owned(),
            },
        )]
        .into_iter()
        .collect();

        let mut resumed = executor.watch(&execution_ref, resume).await.unwrap();
        let replayed = drain(&mut resumed).await;

        // No duplication: the two already-archived `a` lines are not resent.
        assert!(
            !replayed.iter().any(
                |event| matches!(event, ExecutionEvent::Log { node, line } if node == "a" && (line == "a-one" || line == "a-two"))
            ),
            "the two lines `resume` already accounts for must not be replayed"
        );
        // No loss: everything after the resume position, on both nodes, is
        // still there, in order.
        assert_eq!(
            replayed,
            vec![
                script[0].clone(),
                script[2].clone(),
                script[4].clone(),
                script[5].clone(),
            ],
            "b's line and a's un-archived third line must both survive, in \
             their original order",
        );
    }

    /// An empty stream is "nothing more to say", not an error — the error case
    /// is reserved for an unreachable executor, and the caller must be able to
    /// tell them apart or it will release a claim during an outage
    /// (`manager/src/services/run_dispatcher.rs:237-252`).
    #[tokio::test]
    async fn watch_of_an_unknown_reference_yields_an_empty_stream() {
        let executor = MockRunExecutor::new();

        let mut stream = executor
            .watch(&ExecutionRef::new("never-started"), LogResume::default())
            .await
            .unwrap();

        assert!(drain(&mut stream).await.is_empty());
    }

    #[tokio::test]
    async fn cancel_makes_the_stream_finish_as_canceled() {
        let executor = MockRunExecutor::new();
        let run_id = Uuid::new_v4();
        executor.script(
            run_id,
            vec![
                ExecutionEvent::Started,
                finished(ExecutorOutcome::Succeeded),
            ],
        );
        let execution_ref = executor.start(spec_for(run_id)).await.unwrap();

        executor.cancel(&execution_ref).await.unwrap();
        let mut stream = executor
            .watch(&execution_ref, LogResume::default())
            .await
            .unwrap();
        let events = drain(&mut stream).await;

        assert!(matches!(
            events.last(),
            Some(ExecutionEvent::Finished {
                outcome: ExecutorOutcome::Canceled,
                ..
            })
        ));
    }

    /// Results the execution already produced survive the cancel; only the
    /// terminal event changes.
    #[tokio::test]
    async fn cancel_keeps_the_results_reported_before_it() {
        let executor = MockRunExecutor::new();
        let run_id = Uuid::new_v4();
        let result = ExecutionEvent::TestResult(TestObservation {
            node: "repo-smoke".to_owned(),
            test_file: "tests/test_smoke.py".to_owned(),
            test_name: "test_one".to_owned(),
            status: "PASSED".to_owned(),
            duration: Some("1.20s".to_owned()),
            launch_id: None,
            jira_key: None,
            nodeid: None,
            reason: None,
            ticket: None,
        });
        executor.script(
            run_id,
            vec![
                ExecutionEvent::Started,
                result.clone(),
                finished(ExecutorOutcome::Succeeded),
            ],
        );
        let execution_ref = executor.start(spec_for(run_id)).await.unwrap();

        executor.cancel(&execution_ref).await.unwrap();
        let mut stream = executor
            .watch(&execution_ref, LogResume::default())
            .await
            .unwrap();
        let events = drain(&mut stream).await;

        assert_eq!(events.len(), 3);
        assert_eq!(events[1], result);
        assert!(matches!(
            events[2],
            ExecutionEvent::Finished {
                outcome: ExecutorOutcome::Canceled,
                ..
            }
        ));
    }

    /// Idempotent means "the second cancel changes nothing", which is only
    /// worth asserting once the *first* one demonstrably did something. An
    /// earlier revision compared the two replays and nothing else — and stayed
    /// green under a cancel that was a total no-op, because two identical
    /// uncancelled replays are equal too (found 2026-08-13 by the spec review's
    /// own mutation). The first assertion is the one that gives the second its
    /// meaning.
    /// The ordinary end of a run's life, and the input `reconcile_claim` cares
    /// most about: the execution drops off `list_active` — which is
    /// `ClaimExecution::Gone`, hence `ClaimAction::Release` — while its
    /// terminal outcome stays whatever it was.
    ///
    /// The second half is what separates `finish` from `cancel`. Before
    /// `finish` existed, the only way to empty the active set was to cancel,
    /// which relabels the outcome, so a test for "finished and forgotten" would
    /// have had to assert a run was `Canceled` when it had succeeded.
    #[tokio::test]
    async fn finish_makes_the_execution_gone_without_relabelling_its_outcome() {
        let executor = MockRunExecutor::new();
        let run_id = Uuid::new_v4();
        executor.script(
            run_id,
            vec![
                ExecutionEvent::Started,
                finished(ExecutorOutcome::Succeeded),
            ],
        );
        let execution_ref = executor.start(spec_for(run_id)).await.unwrap();
        assert!(
            executor
                .list_active()
                .await
                .unwrap()
                .contains(&execution_ref)
        );

        executor.finish(&execution_ref);

        assert!(
            !executor
                .list_active()
                .await
                .unwrap()
                .contains(&execution_ref),
            "a forgotten execution is Gone, which is what releases the claim"
        );
        let mut stream = executor
            .watch(&execution_ref, LogResume::default())
            .await
            .unwrap();
        assert!(
            matches!(
                drain(&mut stream).await.last(),
                Some(ExecutionEvent::Finished {
                    outcome: ExecutorOutcome::Succeeded,
                    ..
                })
            ),
            "finish must not rewrite the outcome the way cancel does"
        );
    }

    #[tokio::test]
    async fn cancel_is_idempotent_on_an_already_cancelled_execution() {
        let executor = MockRunExecutor::new();
        let run_id = Uuid::new_v4();
        let execution_ref = executor.start(spec_for(run_id)).await.unwrap();

        executor.cancel(&execution_ref).await.unwrap();
        let mut first = executor
            .watch(&execution_ref, LogResume::default())
            .await
            .unwrap();
        let after_first = drain(&mut first).await;
        assert!(
            matches!(
                after_first.last(),
                Some(ExecutionEvent::Finished {
                    outcome: ExecutorOutcome::Canceled,
                    ..
                })
            ),
            "the first cancel must have taken effect, or the replay comparison \
             below is vacuous"
        );

        executor.cancel(&execution_ref).await.unwrap();
        let mut second = executor
            .watch(&execution_ref, LogResume::default())
            .await
            .unwrap();

        assert_eq!(drain(&mut second).await, after_first);
        assert!(executor.list_active().await.unwrap().is_empty());
    }

    /// Idempotency taken to its edge, per the trait doc: cancelling something
    /// the executor has never heard of succeeds. A cancel arriving after the
    /// execution has been forgotten is the normal case, not a fault.
    #[tokio::test]
    async fn cancel_of_an_unknown_reference_succeeds() {
        let executor = MockRunExecutor::new();

        assert!(
            executor
                .cancel(&ExecutionRef::new("never-started"))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn list_active_reports_started_executions() {
        let executor = MockRunExecutor::new();
        let first = executor.start(spec_for(Uuid::new_v4())).await.unwrap();
        let second = executor.start(spec_for(Uuid::new_v4())).await.unwrap();

        let active = executor.list_active().await.unwrap();
        assert_eq!(active.len(), 2);
        assert!(active.contains(&first));
        assert!(active.contains(&second));

        executor.cancel(&first).await.unwrap();
        let remaining = executor.list_active().await.unwrap();
        assert_eq!(remaining, [second].into_iter().collect());
    }

    /// An unreachable executor is an `Err`, and it is **not** the empty stream
    /// one arm above. The port's whole fail-safe direction rests on a caller
    /// being able to tell them apart, so the mock has to be able to produce
    /// both.
    #[tokio::test]
    async fn watch_reports_the_injected_failure() {
        let executor = MockRunExecutor::new();
        let run_id = Uuid::new_v4();
        let execution_ref = executor.start(spec_for(run_id)).await.unwrap();
        executor.fail_watch("executor unreachable");

        let error = executor
            .watch(&execution_ref, LogResume::default())
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            DomainError::ExecutorFailed(message) if message == "executor unreachable"
        ));
    }

    #[tokio::test]
    async fn list_active_reports_the_injected_failure() {
        let executor = MockRunExecutor::new();
        executor.start(spec_for(Uuid::new_v4())).await.unwrap();
        executor.fail_list_active("executor unreachable");

        let error = executor.list_active().await.unwrap_err();

        assert!(matches!(
            error,
            DomainError::ExecutorFailed(message) if message == "executor unreachable"
        ));
    }

    /// The unscripted path: a run dispatched with no script still reaches a
    /// terminal state, attributed to a node that exists.
    #[tokio::test]
    async fn the_default_script_completes_the_run_successfully() {
        let executor = MockRunExecutor::new();
        let execution_ref = executor.start(spec_for(Uuid::new_v4())).await.unwrap();

        let mut stream = executor
            .watch(&execution_ref, LogResume::default())
            .await
            .unwrap();
        let events = drain(&mut stream).await;

        assert_eq!(events.first(), Some(&ExecutionEvent::Started));
        let ExecutionEvent::TestResult(observation) = &events[1] else {
            panic!("the default script reports one test result");
        };
        assert_eq!(observation.node, node().name);
        assert_eq!(observation.test_file, node().test_files[0]);
        assert_eq!(observation.status, "PASSED");
        assert!(matches!(
            events.last(),
            Some(ExecutionEvent::Finished {
                outcome: ExecutorOutcome::Succeeded,
                nodes: NodeOutcome::NoneFailed,
                ..
            })
        ));
    }

    /// The end-to-end form of the port's one security rule: what the executor
    /// is handed carries the `RP_API_KEY` **reference**, and no literal in the
    /// environment holds it. `runvars` never models the secret at all
    /// (its composition obligation 4), so this is the only place the two halves
    /// meet.
    #[tokio::test]
    async fn the_recorded_spec_carries_the_secret_as_a_reference_and_never_a_value() {
        let executor = MockRunExecutor::new();
        let assembled = runvars::assemble(RunVarInputs {
            statics: vec![RunVar {
                name: "TEST_FILES".to_owned(),
                value: "tests/test_smoke.py".to_owned(),
            }],
            ..RunVarInputs::default()
        });
        let mut submission = spec_for(Uuid::new_v4());
        submission.env = RunEnv::new(
            assembled,
            [(
                "RP_API_KEY".to_owned(),
                SecretRef::new("credstore://reportportal-token"),
            )]
            .into_iter()
            .collect(),
        );

        executor.start(submission).await.unwrap();

        let recorded = executor.submitted();
        assert_eq!(
            recorded[0].env.get("RP_API_KEY"),
            Some(&EnvSource::Secret(SecretRef::new(
                "credstore://reportportal-token"
            )))
        );
        // Independent of the assertion above rather than implied by it: that
        // one pins the entry under its own name, this one sweeps *every*
        // literal for the reference's text, so a spec that leaked the
        // credstore ref into some other variable's value still fails.
        assert!(
            !recorded[0].env.entries().values().any(|source| matches!(
                source,
                EnvSource::Value(value) if value.contains("reportportal-token")
            )),
            "no literal in the environment carries the reference, let alone the material"
        );
        assert_eq!(
            recorded[0].env.get("TEST_FILES"),
            Some(&EnvSource::Value("tests/test_smoke.py".to_owned()))
        );
    }

    // ── DELETED at the Phase E review (finding I-7) ────────────────────────
    //
    // `the_kubeconfig_mount_path_matches_the_assembled_kubeconfig_variable`
    // lived here. Its doc claimed it asserted the mount's path against the
    // `KUBECONFIG` variable "rather than the variable against itself" — and it
    // did exactly that: the test built both sides from one local
    // `mount_path`, so it could not fail. An independent review proved it by
    // mutation.
    //
    // The property is real and it is asserted twice, in the two places that
    // can see it:
    //
    // * `qa-vhp-product-plugin`'s `the_mount_path_and_the_kubeconfig_variable_are_one_string`
    //   — at the one place both strings are *derived*, from one constant. Since
    //   Task 18 the path and the variable are both the plugin's.
    // * `domain::service::dispatch_tests`'
    //   `the_kubeconfig_mount_and_the_assembled_variable_come_from_one_value`
    //   — the composition, where the two values reach the spec by different
    //   routes (`access.mounts` and `access.env` -> `runvars::assemble`), so
    //   dropping either wire fails it.
    //
    // Nothing this file can see remains: the mock records what it is handed
    // and derives neither side.
}
