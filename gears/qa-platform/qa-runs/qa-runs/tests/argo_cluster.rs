//! End-to-end checks for the Argo adapter **against a real cluster**.
//!
//! `#[ignore]`d, because they need a Kubernetes API server with Argo Workflows
//! installed. Nothing here is a substitute for the unit tests in
//! `infra::executor::argo::*`; what these add is the half no double can:
//! that the object this adapter builds is one the API server *accepts*, that
//! Argo schedules it, that the pod's log reaches this process line by line, and
//! that the event order the port demands actually comes out.
//!
//! # Running them
//!
//! ```text
//! export QA_RUNS_ARGO_KUBECONFIG=/path/to/kubeconfig   # must be reachable from here
//! export QA_RUNS_ARGO_NAMESPACE=argo                   # optional, defaults to argo
//! cargo test -p qa-runs --features argo --test argo_cluster -- --ignored --nocapture
//! ```
//!
//! The kubeconfig's `server:` must name an address this process can reach *and*
//! that the API server's certificate covers. A k3s host's own
//! `/etc/rancher/k3s/k3s.yaml` says `127.0.0.1`, which is useless from anywhere
//! else; rewrite it to the host address, which k3s' serving certificate already
//! carries as a SAN.
//!
//! # The runner image these use, and why it is not the pytest runner
//!
//! `alpine:3` plus a shell script that prints the runner's marker grammar. That
//! is deliberate: it exercises **this adapter's** contract — submission, log
//! streaming, marker parsing, event ordering, outcome mapping — without also
//! depending on a pytest image being built, imported into the node's
//! containerd, and able to authenticate to a bundle endpoint that today has no
//! answer for how a pod authenticates at all. Those are separate, larger pieces
//! of work; see the adapter's module docs. What these tests prove is that the
//! executor is real, not that the runner is wired.
//!
//! Every test deletes the workflow it created.

#![cfg(feature = "argo")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]

use std::collections::BTreeSet;
use std::time::Duration;

use base64::Engine;
use kube::api::{Api, DeleteParams, DynamicObject};
use uuid::Uuid;

use qa_runs::config::ArgoExecutorConfig;
use qa_runs::domain::ports::run_executor::{
    ExecutionEvent, ExecutionNode, ExecutionRef, RunAccess, RunEnv, RunExecutor, RunSpec,
    RunnerSpec,
};
use qa_runs::domain::repos::LogResume;
use qa_runs::domain::state_machine::ExecutorOutcome;
use qa_runs::infra::executor::argo::{ArgoRunExecutor, workflow_resource};

/// Where the kubeconfig is. Absent means "skip", not "fail": these are run
/// deliberately, and a bare `--ignored` on a machine with no cluster should say
/// so rather than look like a bug in the adapter.
fn kubeconfig() -> Option<String> {
    std::env::var("QA_RUNS_ARGO_KUBECONFIG")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn namespace() -> String {
    std::env::var("QA_RUNS_ARGO_NAMESPACE").unwrap_or_else(|_| "argo".to_owned())
}

/// A shell script that speaks the runner's marker grammar.
///
/// It echoes `$TEST_FILES` and `$TEST_BUNDLE_REF` back, which is what makes the
/// per-node environment plumbing observable rather than assumed.
fn canary_script() -> String {
    let case = base64::engine::general_purpose::STANDARD.encode(
        r#"{"nodeid":"tests/canary.py::test_case","file":"tests/canary.py","name":"test_case","outcome":"passed","duration":0.25}"#,
    );
    format!(
        r#"set -e
echo "runner saw TEST_FILES=$TEST_FILES"
echo "runner saw TEST_BUNDLE_REF=$TEST_BUNDLE_REF"
echo "=== TEST_FILE: tests/canary.py ==="
echo "=== TEST_START: canary ==="
echo "============================= test session starts =============================="
echo "collected 1 item"
echo "============================== 1 passed in 0.25s ==============================="
echo "=== TEST_RESULT: canary PASSED ==="
echo "=== TEST_CASE: {case} ==="
"#
    )
}

fn config(command: Vec<String>) -> ArgoExecutorConfig {
    ArgoExecutorConfig {
        namespace: namespace(),
        kubeconfig_path: kubeconfig(),
        runner_image: "alpine:3".to_owned(),
        runner_command: command,
        // The image is already in the node's containerd, and this cluster has no
        // registry credentials.
        image_pull_policy: "IfNotPresent".to_owned(),
        // The Helm chart's workflow account. Without it the pod runs as
        // `default`, which cannot create `workflowtaskresults` and fails the
        // run *after* all of its output — see the field's doc.
        workflow_service_account: Some(
            std::env::var("QA_RUNS_ARGO_SERVICE_ACCOUNT")
                .unwrap_or_else(|_| "argo-workflow".to_owned()),
        ),
        // Short, so a failed run does not leave objects behind for an hour.
        workflow_ttl_seconds: 120,
        status_poll_seconds: 1,
        ..ArgoExecutorConfig::default()
    }
}

fn spec(run_name: &str) -> RunSpec {
    RunSpec {
        run_id: Uuid::new_v4(),
        run_name: run_name.to_owned(),
        nodes: vec![ExecutionNode {
            name: "repo-canary".to_owned(),
            bundle_ref: "/var/lib/qa-catalog/bundles/canary.tar.gz".to_owned(),
            test_files: vec!["tests/canary.py".to_owned()],
        }],
        env: RunEnv::default(),
        access: RunAccess::default(),
        runner: RunnerSpec::default(),
        timeout_seconds: 300,
    }
}

/// Drain a `watch` stream to its end, with a ceiling so a hung stream fails the
/// test instead of the suite.
async fn drain(executor: &ArgoRunExecutor, reference: &ExecutionRef) -> Vec<ExecutionEvent> {
    let mut stream = executor
        .watch(reference, LogResume::default())
        .await
        .expect("watch opens");
    let mut events = Vec::new();
    tokio::time::timeout(Duration::from_mins(3), async {
        while let Some(event) = stream.recv().await {
            events.push(event);
        }
    })
    .await
    .expect("the stream ends within three minutes");
    events
}

async fn delete(reference: &ExecutionRef) {
    let path = kubeconfig().expect("a kubeconfig");
    let kubeconfig = kube::config::Kubeconfig::read_from(&path).expect("readable kubeconfig");
    let config = kube::Config::from_custom_kubeconfig(
        kubeconfig,
        &kube::config::KubeConfigOptions::default(),
    )
    .await
    .expect("a usable kubeconfig");
    let client = kube::Client::try_from(config).expect("a client");
    let api: Api<DynamicObject> = Api::namespaced_with(client, &namespace(), &workflow_resource());
    drop(
        api.delete(reference.as_str(), &DeleteParams::default())
            .await,
    );
}

/// The vertical slice: submit, observe, reach a terminal state with a **real**
/// test result parsed out of a **real** pod log.
///
/// This is the test that distinguishes the adapter from the mock. Everything it
/// asserts is a clause of the port's contract:
///
/// * `Started` arrives, and it arrives first.
/// * `Log` lines carry the node's name and the runner's actual stdout,
///   including the environment the adapter put in the container.
/// * A `TestResult` carries a name that came out of the log — not
///   `test_mock_default`.
/// * `Finished` is **last**, and the stream then ends.
#[tokio::test]
#[ignore = "needs a Kubernetes cluster with Argo Workflows; see this file's header"]
async fn a_real_workflow_runs_and_reports_a_real_test_result() {
    let Some(_) = kubeconfig() else {
        eprintln!("QA_RUNS_ARGO_KUBECONFIG is unset; skipping");
        return;
    };
    let executor = ArgoRunExecutor::connect(config(vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        canary_script(),
    ]))
    .await
    .expect("connects to the cluster");

    // A leftover from a previous failed run would make `start` take its 409
    // retry path and mint a run-id-qualified name, which is correct behaviour
    // but not what this test is measuring.
    delete(&ExecutionRef::new("argo-canary-1")).await;

    let run = spec("Argo Canary-1");
    let reference = executor.start(run).await.expect("submits");
    assert_eq!(
        reference.as_str(),
        "argo-canary-1",
        "the reference is the workflow's own name, sanitised from the run name"
    );

    // Active the moment it is accepted, before the controller has given it a
    // status — the case `ACTIVE_PHASES`' doc is about.
    let active = executor.list_active().await.expect("lists");
    assert!(
        active.contains(&reference),
        "a just-submitted workflow must be active, or the dispatcher releases \
         its platform claim while the run is starting"
    );

    let events = drain(&executor, &reference).await;
    // Printed rather than only asserted on: with `--nocapture` this is the
    // primary evidence that a run reached a terminal state with real results,
    // and a reviewer should be able to read the sequence rather than trust a
    // green tick.
    println!("--- {} events observed ---", events.len());
    for event in &events {
        println!("{event:?}");
    }

    assert_eq!(
        events.first(),
        Some(&ExecutionEvent::Started),
        "Started must come first"
    );

    assert!(
        events
            .iter()
            .any(|event| matches!(event, ExecutionEvent::Log { .. })),
        "the pod's log must reach this process"
    );
    let log_text: String = events
        .iter()
        .filter_map(|event| match event {
            ExecutionEvent::Log { node, line } => {
                assert_eq!(node, "repo-canary", "logs are attributed per node");
                Some(line.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        log_text.contains("runner saw TEST_FILES=tests/canary.py"),
        "the per-node TEST_FILES the adapter built reached the container; log was:\n{log_text}"
    );
    assert!(
        log_text.contains("runner saw TEST_BUNDLE_REF=/var/lib/qa-catalog/bundles/canary.tar.gz"),
        "the bundle reference reached the container verbatim"
    );

    let results: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ExecutionEvent::TestResult(observation) => Some(observation),
            _ => None,
        })
        .collect();
    assert_eq!(
        results.len(),
        2,
        "one file-level row from TEST_RESULT and one case-level row from \
         TEST_CASE; got {results:#?}"
    );

    let file_level = results
        .iter()
        .find(|observation| observation.nodeid.is_none())
        .expect("a file-level row");
    assert_eq!(file_level.test_name, "canary");
    assert_ne!(
        file_level.test_name, "test_mock_default",
        "the whole point: this result came out of a pod's stdout"
    );
    assert_eq!(file_level.status, "PASSED");
    assert_eq!(file_level.test_file, "tests/canary.py");
    assert_eq!(file_level.duration.as_deref(), Some("0.25s"));
    assert_eq!(file_level.node, "repo-canary");

    let case_level = results
        .iter()
        .find(|observation| observation.nodeid.is_some())
        .expect("a case-level row");
    assert_eq!(
        case_level.nodeid.as_deref(),
        Some("tests/canary.py::test_case")
    );
    assert_eq!(case_level.status, "PASSED");

    let last = events.last().expect("events");
    assert!(
        matches!(
            last,
            ExecutionEvent::Finished {
                outcome: ExecutorOutcome::Succeeded,
                ..
            }
        ),
        "Finished must be last and green; was {last:?}"
    );
    let ExecutionEvent::Finished { nodes, .. } = last else {
        unreachable!()
    };
    assert_eq!(
        *nodes,
        qa_runs::domain::ports::run_executor::NodeOutcome::NoneFailed,
        "status.nodes reported one Pod node and it succeeded"
    );

    // Re-attachable: a second watch on a finished workflow replays the whole
    // observation from byte zero, which is what makes a control-plane restart
    // survivable (`run_executor.rs:804-810`).
    let replay = drain(&executor, &reference).await;
    assert_eq!(
        replay
            .iter()
            .filter(|event| matches!(event, ExecutionEvent::TestResult(_)))
            .count(),
        2,
        "a re-attach re-reads the log and re-emits every result; ingest's \
         upsert makes that idempotent"
    );

    delete(&reference).await;
}

/// `cancel` → the stream finishes `Canceled`, which Argo's phase alone cannot
/// say: it reports a terminated workflow as `Failed`, and `spec.shutdown` is
/// the signal that distinguishes them.
#[tokio::test]
#[ignore = "needs a Kubernetes cluster with Argo Workflows; see this file's header"]
async fn cancelling_a_running_workflow_finishes_it_as_canceled() {
    let Some(_) = kubeconfig() else {
        eprintln!("QA_RUNS_ARGO_KUBECONFIG is unset; skipping");
        return;
    };
    let executor = ArgoRunExecutor::connect(config(vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "echo starting; sleep 600".to_owned(),
    ]))
    .await
    .expect("connects");

    delete(&ExecutionRef::new("argo-cancel-1")).await;
    let reference = executor
        .start(spec("Argo Cancel-1"))
        .await
        .expect("submits");

    // Wait for it to actually be running, so the cancel lands on a live
    // workflow rather than a Pending one.
    let mut running = false;
    for _ in 0..60 {
        if executor
            .list_active()
            .await
            .expect("lists")
            .contains(&reference)
        {
            running = true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        if running {
            break;
        }
    }
    executor
        .cancel(&reference)
        .await
        .expect("cancel is accepted");

    let events = drain(&executor, &reference).await;
    let last = events.last().expect("events");
    assert!(
        matches!(
            last,
            ExecutionEvent::Finished {
                outcome: ExecutorOutcome::Canceled,
                ..
            }
        ),
        "was {last:?}"
    );

    // Idempotent, on an already-terminal execution and on one that never
    // existed (`run_executor.rs:816-817`).
    executor.cancel(&reference).await.expect("idempotent");
    executor
        .cancel(&ExecutionRef::new("no-such-workflow-at-all"))
        .await
        .expect("an unknown reference succeeds");

    let active: BTreeSet<ExecutionRef> = executor.list_active().await.expect("lists");
    assert!(
        !active.contains(&reference),
        "a terminated workflow is no longer active"
    );

    delete(&reference).await;
}

/// A reference the cluster does not have yields an **empty** stream, not an
/// error — "nothing more to say", never "this failed"
/// (`run_executor.rs:800-802`). This is the case a garbage-collected workflow
/// reaches, and reading it as a failure is what would fail live runs after a
/// `ttlStrategy` expiry.
#[tokio::test]
#[ignore = "needs a Kubernetes cluster with Argo Workflows; see this file's header"]
async fn an_unknown_reference_yields_an_empty_stream_rather_than_an_error() {
    let Some(_) = kubeconfig() else {
        eprintln!("QA_RUNS_ARGO_KUBECONFIG is unset; skipping");
        return;
    };
    let executor = ArgoRunExecutor::connect(config(vec!["/bin/true".to_owned()]))
        .await
        .expect("connects");

    let mut stream = executor
        .watch(
            &ExecutionRef::new("no-such-workflow-at-all"),
            LogResume::default(),
        )
        .await
        .expect("watch on an unknown reference is Ok, not Err");
    assert_eq!(stream.recv().await, None, "and it is empty");
}

/// The port's other explicit `start` rejection: an empty node list, which "would
/// execute nothing and report success" (`run_executor.rs:458-461`).
///
/// Needs a cluster only because the executor's constructor does.
#[tokio::test]
#[ignore = "needs a Kubernetes cluster with Argo Workflows; see this file's header"]
async fn a_spec_with_no_nodes_is_rejected_before_anything_is_submitted() {
    let Some(_) = kubeconfig() else {
        eprintln!("QA_RUNS_ARGO_KUBECONFIG is unset; skipping");
        return;
    };
    let executor = ArgoRunExecutor::connect(config(vec!["/bin/true".to_owned()]))
        .await
        .expect("connects");

    let mut run = spec("Argo Empty-1");
    run.nodes.clear();
    let error = executor.start(run).await.expect_err("must be rejected");
    assert!(
        format!("{error}").contains("no execution nodes"),
        "was {error}"
    );
}
