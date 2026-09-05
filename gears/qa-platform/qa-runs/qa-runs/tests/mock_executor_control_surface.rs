//! The mock executor's control surface, exercised from outside the crate.
//!
//! This file exists to *prove* something a unit test cannot: ADR-0001's
//! Confirmation requires that "Integration suite runs the full launch→ingest
//! path against the mock adapter", and an integration test compiles the library
//! **without** `cfg(test)`. An earlier revision gated `script`, `fail_start`,
//! `fail_list_active` and `submitted` on `#[cfg(test)]`, which silenced a
//! dead-code warning and left a future `tests/dispatcher.rs` able to start runs
//! and nothing else — no scripts, no injected failures, no assertion on what
//! was submitted. Nothing inside `src/` could detect that, because everything
//! inside `src/` is compiled with `cfg(test)` when it is tested.
//!
//! So this is a reachability test, deliberately shallow: it touches every
//! control method through the public API and asserts each did what it says.
//! Tasks 13-15 own the real launch→ingest suite; what they inherit from here is
//! the guarantee that the surface they need still exists.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use qa_runs::domain::error::DomainError;
use qa_runs::domain::ports::run_executor::{
    ExecutionEvent, ExecutionNode, NodeOutcome, RunAccess, RunEnv, RunExecutor, RunSpec, RunnerSpec,
};
use qa_runs::domain::state_machine::ExecutorOutcome;
use qa_runs::infra::executor::mock::MockRunExecutor;
use uuid::Uuid;

fn spec_for(run_id: Uuid) -> RunSpec {
    RunSpec {
        run_id,
        run_name: "smoke-tests-1".to_owned(),
        nodes: vec![ExecutionNode {
            name: "repo-smoke".to_owned(),
            bundle_ref: "bundle-store://abc".to_owned(),
            test_files: vec!["tests/test_smoke.py".to_owned()],
        }],
        env: RunEnv::new(BTreeMap::new(), BTreeMap::new()),
        access: RunAccess::default(),
        runner: RunnerSpec::default(),
        timeout_seconds: 3600,
    }
}

#[tokio::test]
async fn every_control_method_is_reachable_from_an_integration_test() {
    let executor = MockRunExecutor::new();
    let run_id = Uuid::new_v4();

    // 1. `script`
    executor.script(
        run_id,
        vec![
            ExecutionEvent::Started,
            ExecutionEvent::Finished {
                outcome: ExecutorOutcome::Succeeded,
                nodes: NodeOutcome::NoneFailed,
                message: None,
            },
        ],
    );

    let execution_ref = executor.start(spec_for(run_id)).await.unwrap();

    // 2. `submitted`
    let recorded = executor.submitted();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].run_id, run_id);

    let mut stream = executor.watch(&execution_ref).await.unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    assert_eq!(events.len(), 2, "the scripted sequence replayed");

    // 3. `finish` — the execution is forgotten, its outcome untouched.
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
            .contains(&execution_ref)
    );

    // 4. `fail_list_active`
    executor.fail_list_active("executor unreachable");
    assert!(matches!(
        executor.list_active().await.unwrap_err(),
        DomainError::ExecutorFailed(message) if message == "executor unreachable"
    ));

    // 5. `fail_start`
    executor.fail_start("submission refused");
    assert!(matches!(
        executor.start(spec_for(Uuid::new_v4())).await.unwrap_err(),
        DomainError::ExecutorFailed(message) if message == "submission refused"
    ));

    // 6. `fail_watch` — the arm that lets a test tell an unreachable executor
    // from an execution with nothing more to say.
    executor.fail_watch("watch unreachable");
    assert!(matches!(
        executor.watch(&execution_ref).await.unwrap_err(),
        DomainError::ExecutorFailed(message) if message == "watch unreachable"
    ));
}
