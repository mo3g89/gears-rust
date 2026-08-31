//! `watch`: an Argo workflow and its pod logs as an
//! [`ExecutionEvent`] stream.
//!
//! # This operation has no source to port from
//!
//! "The source system does not stream anything: it polls `list_workflows` /
//! `get_workflow` and scrapes pod logs after the fact" (`run_executor.rs:126-131`).
//! So the *pieces* come from there — pod discovery by
//! `workflows.argoproj.io/workflow={name}` on container `main`
//! (`argo.rs:1593-1631`), phase and message off the object (`argo.rs:2272-2283`),
//! per-node phases out of `status.nodes` (`argo.rs:1540-1563`) — and the
//! composition is new.
//!
//! # The shape, and the two obligations that dictate it
//!
//! ```text
//! loop {
//!     get the Workflow            -> gone? end the stream (never an error)
//!     emit Started once            (first non-Pending phase)
//!     follow every new pod's log to EOF, emitting Log and TestResult
//!     terminal phase? -> one more pod pass, then emit Finished, end
//!     sleep
//! }
//! ```
//!
//! 1. **`Finished` must be last and the stream must then end**
//!    (`run_executor.rs:649`), or `drain`'s `while let Some(event)` never
//!    terminates. So the log stream is drained to end-of-file *before* the
//!    terminal event — and that ordering is easy to get wrong, because the
//!    workflow object goes terminal before its pod log drains. Getting it wrong
//!    loses the last test of every run: [`markers`](super::markers) holds the
//!    final observation until `finish`.
//! 2. **An unknown reference yields nothing, not an error.** "An empty stream is
//!    'nothing more to say', never 'this failed'" (`run_executor.rs:800-802`).
//!    A workflow past its `ttlStrategy` is exactly this case, and
//!    `domain::service::watch`'s drain handles it by design.
//!
//! # Status is polled; logs are streamed
//!
//! There is no `kube::runtime::watcher` here. The object's phase is re-read on
//! [`ArgoExecutorConfig::status_poll_seconds`](crate::config::ArgoExecutorConfig::status_poll_seconds),
//! while pod logs use `follow: true` and arrive line by line as the runner
//! prints them — so the poll cadence does not bound log latency and
//! `cpt-cf-qa-nfr-log-latency` (2 s p95) is not paid out of it. A watcher would
//! add desync-and-relist semantics to buy a faster notice of a phase this
//! adapter cannot act on until the logs have drained anyway.
//!
//! # Known limitation: log following is sequential across nodes
//!
//! One pod's log is followed to end-of-file before the next pod's begins, so on
//! a multi-node run the second node's lines are buffered by the API server until
//! the first node's pod exits. Correct but not concurrent, and worth fixing
//! before multi-node runs are common: one task per pod feeding the same sink.
//! Single-node runs — the only shape a single repository group produces — are
//! unaffected.

use std::collections::HashSet;
use std::pin::Pin;

use futures::{AsyncBufRead, AsyncBufReadExt, TryStreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, DynamicObject, ListParams, LogParams};
use kube::{Client, ResourceExt};
use serde_json::Value;
use tracing::{debug, warn};

use crate::config::ArgoExecutorConfig;
use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::{
    ExecutionEvent, ExecutionRef, ExecutionSink, ExecutionStream, NodeOutcome, TestObservation,
};
use crate::domain::state_machine::ExecutorOutcome;
use crate::infra::executor::argo::markers::MarkerParser;
use crate::infra::executor::argo::workflow::NODE_ANNOTATION;
use crate::infra::executor::argo::workflow_resource;

/// Channel capacity for one execution's events.
///
/// Bounded, so a chatty pod applies backpressure to this task rather than
/// growing the control plane's heap — the reason
/// [`ExecutionStream`] is a bounded channel in the first place. Larger than the
/// mock's script length because a real run emits one event per log line.
const CHANNEL_CAPACITY: usize = 512;

/// The container Argo runs the workflow step in (`argo.rs:1607`).
const MAIN_CONTAINER: &str = "main";

/// Argo phases past which nothing more will happen.
fn is_terminal(phase: &str) -> bool {
    matches!(phase, "Succeeded" | "Failed" | "Error")
}

/// `status.phase`, or `""` for a workflow the controller has not yet reconciled.
fn phase_of(workflow: &DynamicObject) -> &str {
    workflow
        .data
        .get("status")
        .and_then(|status| status.get("phase"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The port's five-variant outcome from Argo's five-variant phase, plus
/// `status.message`.
///
/// # Two of the port's variants are not in the phase
///
/// * **`Canceled`** — Argo reports a terminated workflow as `Failed`. The
///   reliable signal is `spec.shutdown == "Terminate"`, which is what
///   [`cancel`](crate::domain::ports::run_executor::RunExecutor::cancel) writes
///   (`argo.rs:1447-1453`), read back off the same object.
/// * **`TimedOut`** — **deliberately not derived, and this is a recorded
///   divergence.** Argo signals `activeDeadlineSeconds` only in
///   `status.message` text, and matching on it is brittle. The port says the
///   backstop should never be the thing that fires: "the control plane relies on
///   it alone … whereas `cpt-cf-qa-fr-runs-timeout` requires the control plane
///   to enforce the timeout itself" and "the control-plane sweep is the one that
///   must fire first" (`run_executor.rs:469-474`). So a run that really timed
///   out is already terminal in the database before Argo's deadline lands, and
///   a deadline-`Failed` that does reach here means the sweep did not fire —
///   which should be reported as `Failed` with Argo's own message rather than
///   silently relabelled.
#[must_use]
pub fn outcome_of(workflow: &DynamicObject) -> (ExecutorOutcome, Option<String>) {
    let message = workflow
        .data
        .get("status")
        .and_then(|status| status.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty());

    let terminated = workflow
        .data
        .get("spec")
        .and_then(|spec| spec.get("shutdown"))
        .and_then(Value::as_str)
        .is_some_and(|shutdown| shutdown.eq_ignore_ascii_case("Terminate"));

    let outcome = match phase_of(workflow) {
        "Succeeded" => ExecutorOutcome::Succeeded,
        "Failed" if terminated => ExecutorOutcome::Canceled,
        "Failed" => ExecutorOutcome::Failed,
        // `"Error"`, and also anything this adapter does not recognise — which
        // the loop below cannot reach, because it only calls this on a terminal
        // phase. `Errored` rather than a panic either way: "the executor said
        // something this adapter does not understand" is an execution fault,
        // not a control-plane bug.
        _ => ExecutorOutcome::Errored,
    };
    (outcome, message)
}

/// Per-node detail out of `status.nodes`.
///
/// Only `type: "Pod"` entries count, matching `get_dag_node_statuses`
/// (`argo.rs:1573-1575`) — a DAG node's own entry carries the aggregate and
/// would double-count. An absent or empty `status.nodes` is
/// [`NodeOutcome::Unknown`], which is what the source system's empty-vector
/// return already means (`argo.rs:1544-1551`) and what the port documents
/// (`run_executor.rs:614-616`).
#[must_use]
pub fn node_outcome(workflow: &DynamicObject) -> NodeOutcome {
    let Some(nodes) = workflow
        .data
        .get("status")
        .and_then(|status| status.get("nodes"))
        .and_then(Value::as_object)
    else {
        return NodeOutcome::Unknown;
    };

    let phases: Vec<&str> = nodes
        .values()
        .filter(|node| node.get("type").and_then(Value::as_str) == Some("Pod"))
        .filter_map(|node| node.get("phase").and_then(Value::as_str))
        .collect();

    if phases.is_empty() {
        NodeOutcome::Unknown
    } else if phases
        .iter()
        .any(|phase| matches!(*phase, "Failed" | "Error"))
    {
        // "Including a node that died before emitting any result at all, which
        // is the case per-test results cannot cover" (`run_executor.rs:620-622`).
        NodeOutcome::SomeFailed
    } else {
        NodeOutcome::NoneFailed
    }
}

/// The [`ExecutionNode`](crate::domain::ports::run_executor::ExecutionNode) name
/// a pod belongs to.
///
/// The un-sanitised name travels as a pod annotation, put there by
/// [`workflow::build`](super::workflow::build) — see [`NODE_ANNOTATION`] for why
/// the template name cannot be used instead. The two fallbacks exist so a pod
/// from an older submission, or one Argo annotated differently, still attributes
/// its output somewhere rather than being dropped.
fn node_of(pod: &Pod) -> String {
    let annotations = pod.annotations();
    annotations
        .get(NODE_ANNOTATION)
        .or_else(|| annotations.get("workflows.argoproj.io/node-name"))
        .cloned()
        .unwrap_or_else(|| pod.name_any())
}

/// Open an observation of one execution.
///
/// Returns an **empty, already-ended** stream for a workflow the cluster does
/// not have — never an error. See this module's obligation 2.
///
/// # Errors
/// [`DomainError::ExecutorFailed`] when the API server cannot be reached at all,
/// which is the case the port distinguishes from an empty stream: one means
/// "nothing is known", the other "nothing more to say".
pub async fn start(
    client: Client,
    config: ArgoExecutorConfig,
    execution_ref: &ExecutionRef,
) -> Result<ExecutionStream, DomainError> {
    let name = execution_ref.as_str().to_owned();
    let workflows: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &config.namespace, &workflow_resource());

    // Resolved before the stream is handed back so an unreachable API server is
    // an `Err` rather than a silent empty stream.
    let existing = workflows.get_opt(&name).await.map_err(|error| {
        DomainError::ExecutorFailed(format!("cannot read workflow {name}: {error}"))
    })?;

    let (sink, stream) = ExecutionStream::channel(CHANNEL_CAPACITY);
    if existing.is_none() {
        debug!(
            execution_ref = %name,
            "no such workflow; yielding an empty stream, which means 'nothing more to say'"
        );
        drop(sink);
        return Ok(stream);
    }

    let mut watcher = Watcher {
        workflows,
        pods: Api::namespaced(client, &config.namespace),
        config,
        name,
        sink,
        started: false,
        drained: HashSet::new(),
    };
    tokio::spawn(async move { watcher.run().await });
    Ok(stream)
}

/// One execution's observation state.
struct Watcher {
    workflows: Api<DynamicObject>,
    pods: Api<Pod>,
    config: ArgoExecutorConfig,
    name: String,
    sink: ExecutionSink,
    started: bool,
    /// Pods whose log has been read to end-of-file. A pod is followed exactly
    /// once per `watch` call; a *new* `watch` re-reads every log from byte zero,
    /// which is what makes re-attach work and is safe because
    /// `upsert_test_result` replaces the row rather than appending.
    drained: HashSet<String>,
}

impl Watcher {
    async fn run(&mut self) {
        let poll = std::time::Duration::from_secs(self.config.status_poll_seconds.max(1));
        loop {
            let workflow = match self.workflows.get_opt(&self.name).await {
                Ok(Some(workflow)) => workflow,
                // Garbage-collected mid-observation: end the stream. Not a
                // failure, and not a `Finished` either — there is nothing left
                // to report an outcome from.
                Ok(None) => return,
                Err(error) => {
                    warn!(
                        execution_ref = %self.name,
                        %error,
                        "lost contact with the api server; ending this observation"
                    );
                    return;
                }
            };

            let phase = phase_of(&workflow).to_owned();
            if !self.started && !phase.is_empty() && phase != "Pending" {
                self.started = true;
                if !self.sink.emit(ExecutionEvent::Started).await {
                    return;
                }
            }

            if !self.drain_pods().await {
                return;
            }

            if is_terminal(&phase) {
                // A pod can only be created while the workflow is not terminal,
                // so one more pass catches anything that appeared during the
                // last one. After this, every log is at end-of-file and every
                // observation the run produced is in the channel.
                if !self.drain_pods().await {
                    return;
                }
                self.finish(workflow).await;
                return;
            }

            tokio::time::sleep(poll).await;
        }
    }

    /// Emit the terminal event, re-reading the object first so `status.nodes`
    /// and `status.message` are the final ones.
    async fn finish(&self, observed: DynamicObject) {
        let workflow = match self.workflows.get_opt(&self.name).await {
            Ok(Some(fresh)) => fresh,
            // Collected in the moment between the last drain and this read; the
            // phase we already observed is still the right answer.
            _ => observed,
        };
        let (outcome, message) = outcome_of(&workflow);
        let nodes = node_outcome(&workflow);
        debug!(
            execution_ref = %self.name,
            ?outcome,
            ?nodes,
            "argo workflow reached a terminal phase"
        );
        let _ = self
            .sink
            .emit(ExecutionEvent::Finished {
                outcome,
                nodes,
                message,
            })
            .await;
    }

    /// Follow every not-yet-drained pod's log to end-of-file.
    ///
    /// Returns `false` once the observer has gone away, which is not a failure
    /// and must stop the task rather than be reported as one
    /// (`run_executor.rs:743-749`).
    async fn drain_pods(&mut self) -> bool {
        let selector = format!("workflows.argoproj.io/workflow={}", self.name);
        let pods = match self
            .pods
            .list(&ListParams::default().labels(&selector))
            .await
        {
            Ok(list) => list.items,
            Err(error) => {
                warn!(
                    execution_ref = %self.name,
                    %error,
                    "could not list this workflow's pods; retrying on the next pass"
                );
                return true;
            }
        };

        for pod in pods {
            let pod_name = pod.name_any();
            if self.drained.contains(&pod_name) {
                continue;
            }
            // A pod with no container started yet has no log endpoint, and
            // asking produces a 400 per pass.
            let pod_phase = pod
                .status
                .as_ref()
                .and_then(|status| status.phase.as_deref())
                .unwrap_or_default();
            if !matches!(pod_phase, "Running" | "Succeeded" | "Failed") {
                continue;
            }
            if !self.follow(&pod_name, &node_of(&pod)).await {
                return false;
            }
        }
        true
    }

    /// Stream one pod's `main` container log to end-of-file, emitting a
    /// [`ExecutionEvent::Log`] per line and a
    /// [`ExecutionEvent::TestResult`] per completed test.
    async fn follow(&mut self, pod_name: &str, node: &str) -> bool {
        let Some(stream) = self.open_log(pod_name).await else {
            return true;
        };
        let mut parser = MarkerParser::new(node);
        let mut lines = stream.lines();
        loop {
            match lines.try_next().await {
                Ok(Some(line)) => {
                    if !self.emit_line(&mut parser, node, line).await {
                        return false;
                    }
                }
                // End of log. The pod is finished, so this is the point at
                // which the last test's observation exists.
                Ok(None) => break,
                Err(error) => {
                    warn!(pod = %pod_name, %error, "log stream ended early");
                    break;
                }
            }
        }

        // `finish` is what produces the final test's observation - see
        // `markers`' "one-test lag". Emitted before this function returns, and
        // therefore before `Finished`.
        if !self.emit_results(parser.finish()).await {
            return false;
        }
        self.drained.insert(pod_name.to_owned());
        true
    }

    /// Open a follow-mode stream on one pod's `main` container log.
    ///
    /// `None` is "not yet", not "never": a pod that has only just gone Running
    /// can still refuse a log request, so the caller retries on the next pass
    /// rather than marking the pod drained. Boxed so the type is nameable,
    /// which is what lets [`Self::follow`] stay small.
    async fn open_log(&self, pod_name: &str) -> Option<Pin<Box<dyn AsyncBufRead + Send>>> {
        let params = LogParams {
            container: Some(MAIN_CONTAINER.to_owned()),
            follow: true,
            ..LogParams::default()
        };
        match self.pods.log_stream(pod_name, &params).await {
            Ok(stream) => Some(Box::pin(stream)),
            Err(error) => {
                debug!(pod = %pod_name, %error, "log stream not available yet");
                None
            }
        }
    }

    /// One log line: whatever it completed first, then the line itself.
    ///
    /// **Results before the line that produced them** so a consumer reading
    /// both streams never sees a `TEST_RESULT` marker in the log before the
    /// result it announced.
    async fn emit_line(&self, parser: &mut MarkerParser, node: &str, line: String) -> bool {
        if !self.emit_results(parser.line(&line)).await {
            return false;
        }
        self.sink
            .emit(ExecutionEvent::Log {
                node: node.to_owned(),
                line,
            })
            .await
    }

    /// Emit a batch of observations, stopping early if the observer has gone.
    async fn emit_results(&self, observations: Vec<TestObservation>) -> bool {
        for observation in observations {
            if !self
                .sink
                .emit(ExecutionEvent::TestResult(observation))
                .await
            {
                return false;
            }
        }
        true
    }
}
