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
use tracing::{debug, error, warn};

use crate::api::rest::sse::sanitize_line;
use crate::config::ArgoExecutorConfig;
use crate::domain::error::DomainError;
use crate::domain::ports::run_executor::{
    ExecutionEvent, ExecutionRef, ExecutionSink, ExecutionStream, NodeOutcome, TestObservation,
};
use crate::domain::repos::{LogResume, flatten_log_char};
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

/// How many more of one node's lines to suppress before letting them reach
/// the sink — Task 13, review finding #50, **fix-round 1**.
///
/// # Why a count, and not `LogParams::since_time`
///
/// The first version of this fix asked Kubernetes to filter by
/// `since_time`, using the archive row's `updated_at` as a stand-in for "this
/// node's last archived line". Review found that compares two different
/// clocks: `updated_at` is the **control plane's** write-time — stamped when
/// a flush *commits* the row — while Kubernetes filters by each log entry's
/// own **kubelet-recorded emission time**. A line can reach this process
/// (queued in `ExecutionStream`, which is a 512-slot channel, each entry
/// costing the consumer a database round trip before it is archived) and
/// still be sitting unflushed when a tick stamps `updated_at = now` for
/// whatever *had* been archived by then. Re-attaching with `since_time` set
/// to that stamp filters out every such line — its own emission time is
/// **earlier** than the stamp — and `RunLogsRepository` has no re-read to
/// recover it. That is permanent loss, worse than the bug this task fixes
/// (which only duplicated), so it was dropped before landing.
///
/// A count has no clock in it. This re-reads a node's pod log from byte 0,
/// exactly as before this task, and suppresses the first `lines` of what
/// comes back — the same lines [`domain::repos::LogResume::from_archived_text`]
/// already counted as archived for that node.
///
/// **It is not true that a count can never over-suppress relative to what
/// actually reached the executor — the first version of this doc said so,
/// and review disproved it.** kubelet only retains a container's log up to
/// `containerLogMaxSize` × `containerLogMaxFiles` (10Mi × 5 by default); once
/// rotation has dropped lines from the head, a re-attach's "byte 0" is no
/// longer this node's true byte 0, and suppressing by the archived count
/// alone would drop real, never-archived lines — on exactly the long, chatty
/// run `cpt-cf-qa-nfr-run-duration` exists for. See "Two guards" below for
/// the fix.
///
/// **The guarded property is not an absolute either, and this paragraph
/// used to claim one — the same mistake rounds 0 and 1 each made once
/// already.** Two residuals survive, both named on "Two guards"' own
/// paragraphs rather than repeated here in full: the inflated-count case is
/// detected, not recovered, so whatever it wrongly suppresses before the
/// last-line check fires is genuinely lost; and once the first-line guard
/// trips for a node, that node's count is dead for the rest of the run —
/// the archive's first line will never match the retained window again
/// after rotation moves it, so every later re-attach re-suppresses nothing
/// and the archive grows by this node's whole retained window each time,
/// unbounded for as long as the run and its rotation both continue. Neither
/// residual is a regression against the bug this task fixes — both fail
/// toward duplication, never toward silent loss beyond what "detected, not
/// recovered" already names — but "guarded" was never "solved", and the
/// property that holds without exception is narrower still: a count can
/// only ever *under*-suppress relative to the real log, or be caught
/// failing to align with it. It cannot silently lose a line the archive
/// did not already have.
///
/// The cost is an unchanged one: a re-attach re-reads a node's whole log over
/// the network, exactly as every attach always has. Finding #50 was about
/// `append_log`'s `CONCAT` duplicating what came back, never about paying for
/// the read itself — a real per-line emission timestamp (`LogParams::
/// timestamps: true`, parsed and stored per node) would let a resumed read
/// start late instead of at byte 0, and is a real future optimisation that
/// needs a parser and a schema change neither of which exists yet.
///
/// # Two guards — fix-round 2
///
/// See [`domain::repos::LogPosition`]'s own "Two anchors" section for the
/// full argument; this is the mechanism side of it.
///
/// **Rotation, guarded by the first line.** [`Self::consume`] compares the
/// very first line this node's fresh read produces against
/// [`LogResume::first_line_for`]'s answer, once, before suppressing
/// anything. A mismatch means the window has moved — this read's byte 0 is
/// not the archive's — and the response is to suppress *nothing* for this
/// node for the rest of this attach: safe, because emitting everything is
/// the duplication direction, never the loss direction.
///
/// **A pre-fix-inflated count, guarded by the last line, detected but not
/// recoverable.** Once [`Self::consume`] has suppressed exactly as many
/// lines as the count called for, it compares the last one it actually
/// suppressed against [`LogResume::last_line_for`]'s answer. A mismatch
/// means the count itself was wrong — see [`LogResume::lines_for`]'s doc for
/// the one way that happens, a run whose archive still carries duplicates
/// from before this fix shipped — and by the time this fires, whatever it
/// wrongly suppressed already did not reach the sink. There is no re-read
/// that gets it back, so the contract here is "log loudly enough that an
/// operator can tell this happened", not "recover it": an `error!` naming
/// the execution reference, the node, and both lines.
///
/// Pulled out of [`Watcher::follow`] as its own type so the suppression
/// decision — the actual fix — is unit-testable without a Kubernetes API
/// server: `follow` needs one to open the log stream; this needs only a
/// [`LogResume`], an execution reference and node name for its own log
/// lines, and a sequence of raw lines.
struct LineSkip {
    execution_ref: String,
    node: String,
    /// How many more lines to suppress. Reaches `0` and stops there:
    /// consuming more lines than were seeded — a resume position larger
    /// than what this fresh read actually has left — suppresses everything
    /// this read produces and never underflows.
    remaining: i64,
    /// `remaining`'s starting value, kept so the last-line guard can tell
    /// "suppression just finished" (`remaining` reached `0` having started
    /// above it) apart from "there was never anything to suppress"
    /// (`remaining` started at `0`).
    total: i64,
    first_line: Option<String>,
    last_line: Option<String>,
    /// Set on the first call to [`Self::consume`], so the first-line guard
    /// runs exactly once regardless of how many lines follow.
    checked_first: bool,
    /// Set once the first-line guard disagrees. Every following line is
    /// then emitted unconditionally, the safe direction — see this type's
    /// "Two guards" doc.
    misaligned: bool,
    /// The most recent line actually suppressed, kept only long enough to
    /// compare against `last_line` the moment `remaining` reaches `0`.
    last_suppressed: Option<String>,
}

impl LineSkip {
    /// Seeded from `resume`'s answers for `node` — `0` lines and no anchors
    /// for a node `resume` says nothing about, which is what a first
    /// attach's empty [`LogResume`] produces for every node, and what makes
    /// [`Self::consume`] suppress nothing for it.
    fn for_node(resume: &LogResume, execution_ref: &str, node: &str) -> Self {
        let remaining = resume.lines_for(node);
        Self {
            execution_ref: execution_ref.to_owned(),
            node: node.to_owned(),
            remaining,
            total: remaining,
            first_line: resume.first_line_for(node),
            last_line: resume.last_line_for(node),
            checked_first: false,
            misaligned: false,
            last_suppressed: None,
        }
    }

    /// `true` if `line` is already archived and must not reach the sink
    /// again; `false` if it should. See this type's "Two guards" doc for
    /// the two checks this performs around the count, and
    /// [`domain::repos::LogPosition`]'s "Two anchors" for why both exist.
    ///
    /// **`line` is flattened through [`flatten_log_char`] before either
    /// guard compares it — fix-round 3.** The anchors hold *flattened* text:
    /// `fan_out_log` maps every `'\n'`/`'\r'` to a space before archiving, so
    /// one archived entry can never split into two. `line` here is a fresh,
    /// raw pod line, and `futures`' `Lines` strips only its *trailing*
    /// terminator — an embedded `\r` survives. Comparing the raw line
    /// against a flattened anchor made every guard fail on any line
    /// carrying one, permanently for that node (the first-line guard has no
    /// second chance) and spuriously for the last-line guard (a false
    /// "pre-fix duplicate" alarm on a perfectly healthy archive). Both sides
    /// must go through the one shared flattening, not just the write side.
    fn consume(&mut self, line: &str) -> bool {
        let line: String = line.chars().map(flatten_log_char).collect();
        if !self.checked_first {
            self.checked_first = true;
            if self.remaining > 0 && self.first_line.as_deref() != Some(line.as_str()) {
                debug!(
                    execution_ref = %self.execution_ref,
                    node = %self.node,
                    "this node's re-read log does not start where its archive does (log \
                     rotation is the expected cause on a long-running node); resuming \
                     without suppression for it rather than trusting a misaligned count -- \
                     this node's count is now dead for the rest of this run and its archive \
                     will grow unbounded on every further re-attach",
                );
                self.misaligned = true;
                self.remaining = 0;
                return false;
            }
        }

        if self.misaligned || self.remaining <= 0 {
            return false;
        }

        self.last_suppressed = Some(line);
        self.remaining -= 1;

        if self.remaining == 0 && self.total > 0 {
            let matches = self.last_line.as_deref() == self.last_suppressed.as_deref();
            if !matches {
                error!(
                    execution_ref = %self.execution_ref,
                    node = %self.node,
                    expected = self.last_line.as_deref().unwrap_or_default(),
                    actual = self.last_suppressed.as_deref().unwrap_or_default(),
                    "this node's archived line count did not match its re-read log at the \
                     boundary the count expected; the archive most likely still carries \
                     duplicate lines from a run that hit review finding #50 before this \
                     resume fix shipped, and any lines this count wrongly suppressed cannot \
                     be recovered",
                );
            }
        }

        true
    }
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
    resume: LogResume,
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
        resume,
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
    /// Pods whose log has been read to end-of-file **within this `watch`
    /// call**. A pod is followed exactly once per call: `drain_pods` runs on
    /// every status-poll tick, and without this a still-running pod would be
    /// re-followed from wherever `open_log` starts it on every one of those
    /// ticks, not just once per re-attach.
    ///
    /// This is unrelated to *cross-attach* resumption, which is `resume`'s
    /// job below. Before Task 13 (review finding #50) a *new* `watch` call
    /// re-read every log from byte zero regardless of what an earlier call
    /// had already archived, and that was safe only for test results —
    /// `upsert_test_result` replaces the row rather than appending — not for
    /// the archived log, which `append_log`'s `CONCAT` duplicated on every
    /// re-attach.
    drained: HashSet<String>,
    /// Where to resume each pod's log read from, keyed by node — see
    /// [`LogResume`]'s own doc. Consulted once per pod, at the start of
    /// [`Self::follow`] via [`LineSkip::for_node`], the first time that pod
    /// is followed by *this* `watch` call; `drained` above is what stops a
    /// second consultation for the same pod on a later poll tick.
    resume: LogResume,
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
    ///
    /// **Re-reads from byte 0 every time**, exactly as before Task 13 — see
    /// [`LineSkip`]'s doc for why that read is unchanged and only the
    /// archive-facing half of what it produces is suppressed, guarded
    /// against both a moved window (log rotation) and an inflated count (a
    /// pre-fix duplicate archive). `skip` is seeded once per pod, here, from
    /// `self.resume`'s answers for `node`; a line it says is already
    /// archived still reaches the marker parser (`upsert_test_result`
    /// replaces a row rather than appending, so re-parsing a marker is
    /// harmless) but not the sink's `Log` event, which is what
    /// `append_log`'s `CONCAT` would otherwise duplicate.
    async fn follow(&mut self, pod_name: &str, node: &str) -> bool {
        let Some(stream) = self.open_log(pod_name).await else {
            return true;
        };
        let mut parser = MarkerParser::new(node);
        let mut skip = LineSkip::for_node(&self.resume, &self.name, node);
        let mut lines = stream.lines();
        loop {
            match lines.try_next().await {
                Ok(Some(line)) => {
                    if !handle_line(&self.sink, &mut parser, &mut skip, node, line).await {
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
        if !emit_results(&self.sink, parser.finish()).await {
            return false;
        }
        self.drained.insert(pod_name.to_owned());
        true
    }

    /// Open a follow-mode stream on one pod's `main` container log, from
    /// byte 0 — see [`LineSkip`]'s doc for why this carries no resume-shaped
    /// parameter at all.
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
}

/// Everything `follow`'s loop does with one freshly read pod-log line.
///
/// Free rather than a `Watcher` method for one reason: it only ever touches
/// the channel half of a `Watcher` (`sink`) plus the per-pod state
/// (`parser`, `skip`) `follow` already keeps in its own locals — nothing
/// here needs the `kube::Api` handles the rest of `Watcher` carries. That is
/// what lets this file's own tests drive it against a plain
/// `ExecutionStream::channel`, with no cluster and no `Api` construction.
///
/// # Two forms of one line, and why there are two (review finding #30)
///
/// [`sanitize_line`] — the same cap and character-boundary handling
/// `api::rest::sse` already applies on the read side — is computed once,
/// here, and used for two of this line's three destinations:
///
/// * the sink, and downstream of it `IngestService::fan_out_log`'s archive
///   write, so a pathological line no longer sits in the broadcaster or the
///   archive at full size — the truncation this task adds;
/// * [`LineSkip::consume`]'s anchor comparison, capped **the same way**,
///   because the anchor it compares against is now the *capped* archived
///   text. Comparing a raw re-read against a capped anchor would never
///   match again for any node that ever emitted an over-long line —
///   permanently disabling this run's resume suppression for that node,
///   the same failure shape `flatten_log_char`'s own doc already recounts
///   once for a different mismatch.
///
/// The **marker parser** gets the raw, uncapped line instead, not the
/// sanitized one. This is deliberate, not an oversight: `MarkerParser::case`
/// decodes a `=== TEST_CASE: <base64-json> ===` marker whose capture
/// (`CASE_RE`, `\S+`) has no length limit, carrying a pytest plugin's JSON —
/// `reason` and `ticket` fields this crate does not control the size of. So
/// a marker line is not provably under [`MAX_LINE_BYTES`] the way an
/// ordinary log line usually is (checked, not assumed — see this crate's
/// task notes for review finding #30); truncating the parser's input could
/// cut a marker in half and silently lose a test result that the log
/// truncation itself never claimed to preserve.
///
/// # A residual this change accepts: an archive written before this shipped
///
/// An archived `first_line`/`last_line` anchor from a run whose over-long
/// line was written before this fix deployed holds the *full, untruncated*
/// text — nothing rewrites an archive already on disk. Re-attaching to that
/// run afterwards compares that untruncated anchor against this function's
/// now-truncated re-read, which will not match. That is not a new failure
/// mode: it is the same misaligned-window case [`LineSkip::consume`]
/// already detects and handles by suppressing nothing for that node — the
/// safe direction (duplication), not the unsafe one (loss) — same as real
/// log rotation. Bounded to runs whose watch spans this deploy and whose
/// affected node is re-attached to afterwards.
async fn handle_line(
    sink: &ExecutionSink,
    parser: &mut MarkerParser,
    skip: &mut LineSkip,
    node: &str,
    line: String,
) -> bool {
    let sanitized = sanitize_line(&line);
    let suppress = skip.consume(&sanitized);
    // `&line` (raw) for the parser, `sanitized` for everything else -- see
    // this function's own doc for why the split exists. Do not collapse
    // these back to one form without re-checking that a marker line still
    // cannot exceed `MAX_LINE_BYTES`.
    emit_line(sink, parser, node, &line, sanitized, suppress).await
}

/// One log line: whatever it completed first, then the line itself —
/// unless `suppress`, in which case the line has already been archived by
/// an earlier `watch()` and must not reach the sink a second time (Task
/// 13, review finding #50; see [`LineSkip`]'s doc for the mechanism).
///
/// **Results before the line that produced them**, and **always**, even
/// when `suppress` is set — so a consumer reading both streams never sees
/// a `TEST_RESULT` marker in the log before the result it announced, and
/// a re-attach never fails to notice a marker just because its line is
/// being resumed past.
async fn emit_line(
    sink: &ExecutionSink,
    parser: &mut MarkerParser,
    node: &str,
    raw_line: &str,
    sanitized_line: String,
    suppress: bool,
) -> bool {
    if !emit_results(sink, parser.line(raw_line)).await {
        return false;
    }
    if suppress {
        return true;
    }
    sink.emit(ExecutionEvent::Log {
        node: node.to_owned(),
        line: sanitized_line,
    })
    .await
}

/// Emit a batch of observations, stopping early if the observer has gone.
async fn emit_results(sink: &ExecutionSink, observations: Vec<TestObservation>) -> bool {
    for observation in observations {
        if !sink.emit(ExecutionEvent::TestResult(observation)).await {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! `LineSkip` is the actual fix for review finding #50 in this adapter.
    //! Fix-round 1 added the count-based suppression properties below (every
    //! test that shipped with the original commit drove `MockRunExecutor`,
    //! never this file, because `argo/watch.rs` had no `#[cfg(test)]` module
    //! at all). Fix-round 2 added the two-guard tests: the first-line guard
    //! is Critical (kubelet log rotation moves the window forward, and an
    //! unguarded count would then suppress real, never-archived lines); the
    //! last-line guard is Important (a pre-fix archive's inflated count is
    //! detected, though not recovered — that half is exercised through
    //! `tracing_test` rather than its own dedicated test, since its only
    //! observable effect is the `error!` line). Fix-round 3 added two more:
    //! a normalisation test (an anchor and a fresh line must be flattened
    //! the same way, or a `\r` this crate's own `fan_out_log` strips before
    //! archiving would never match its own anchor), and a negative pin on
    //! the fully-aligned path added to the existing matching-first-line
    //! test, so the last-line `error!` staying inside its `if !matches`
    //! guard is itself covered rather than merely inspected.
    //!
    //! These run under `--features argo`, which is this module's own gate —
    //! no separate `#[cfg]` needed on the module itself.
    //!
    //! Task 15 (review finding #30) added `handle_line`'s own test below,
    //! driven directly against a plain `ExecutionStream::channel` rather
    //! than a `Watcher` — see that function's doc for why no `kube::Api` is
    //! needed to exercise it.

    use super::{LineSkip, handle_line};
    use crate::api::rest::sse::{MAX_LINE_BYTES, TRUNCATION_MARKER_MAX};
    use crate::domain::ports::run_executor::{ExecutionEvent, ExecutionStream};
    use crate::domain::repos::{LogPosition, LogResume};
    use crate::infra::executor::argo::markers::MarkerParser;

    /// Build a resume position the way a real `LogResume::from_archived_text`
    /// would for a node whose archived lines are exactly `lines`, in order —
    /// `first_line`/`last_line` derived from it, so a resume built this way
    /// is aligned by construction against a fresh read that reproduces the
    /// same lines.
    fn aligned_resume(node: &str, lines: &[&str]) -> LogResume {
        resume_with(
            node,
            i64::try_from(lines.len()).expect("test fixture length fits in i64"),
            lines.first().copied().unwrap_or_default(),
            lines.last().copied().unwrap_or_default(),
        )
    }

    /// Build a resume position with an explicit `lines` count against
    /// explicit anchors — for a test that wants a count larger than the
    /// handful of lines it is convenient to spell out, where
    /// [`aligned_resume`] (whose count is always the anchor slice's own
    /// length) cannot express the mismatch.
    fn resume_with(node: &str, lines: i64, first_line: &str, last_line: &str) -> LogResume {
        [(
            node.to_owned(),
            LogPosition {
                lines,
                first_line: first_line.to_owned(),
                last_line: last_line.to_owned(),
            },
        )]
        .into_iter()
        .collect()
    }

    /// An empty resume — what every first attach passes — suppresses
    /// nothing at all.
    #[test]
    fn an_empty_resume_suppresses_nothing() {
        let mut skip = LineSkip::for_node(&LogResume::default(), "wf-1", "a");

        for line in ["x0", "x1", "x2", "x3", "x4"] {
            assert!(!skip.consume(line), "nothing is archived for this node yet");
        }
    }

    /// **The first-line guard, matching.** A resume of `N` whose first line
    /// agrees with the fresh read's first line suppresses exactly the first
    /// `N` and lets every line after them through — fix-round 1's original
    /// property, now under the guard fix-round 2 added.
    ///
    /// **Also pins fix-round 3's Minor**: the aligned path must never log
    /// the last-line mismatch `error!` — nothing here should trip an alarm
    /// meant for a misaligned count. `#[traced_test]` so a future change
    /// that hoisted that `error!` out of its `if !matches` guard, logging it
    /// unconditionally, would turn this red instead of staying silently
    /// green.
    #[test]
    #[tracing_test::traced_test]
    fn a_matching_first_line_suppresses_the_full_count() {
        let resume = aligned_resume("a", &["l0", "l1", "l2"]);
        let mut skip = LineSkip::for_node(&resume, "wf-1", "a");

        let suppressed: Vec<bool> = ["l0", "l1", "l2", "l3", "l4"]
            .into_iter()
            .map(|line| skip.consume(line))
            .collect();

        assert_eq!(
            suppressed,
            vec![true, true, true, false, false],
            "the first 3 are suppressed, the 4th and 5th are not",
        );
        assert!(
            !logs_contain("archived line count did not match its re-read log"),
            "a fully aligned resume must never trip the last-line mismatch alarm",
        );
    }

    /// **Normalisation must agree on both sides of the guard — the
    /// Important fix, fix-round 3.** `fan_out_log` flattens every `'\n'` and
    /// `'\r'` in a line to a space before archiving it, so the anchor for a
    /// node whose real output was `"a\rb\rc"` is the flattened `"a b c"`.
    /// `futures`' `Lines` strips only the *trailing* terminator, so a fresh
    /// re-read still hands `consume` the raw `"a\rb\rc"`. Without matching
    /// normalisation on the read side, this would never match its own
    /// anchor — disabling suppression for this node permanently, for a
    /// reason that has nothing to do with log rotation.
    #[test]
    fn an_embedded_carriage_return_in_the_first_line_still_matches_its_anchor() {
        let resume = resume_with("a", 1, "a b c", "a b c");
        let mut skip = LineSkip::for_node(&resume, "wf-1", "a");

        assert!(
            skip.consume("a\rb\rc"),
            "the raw line must match its flattened archived anchor"
        );
        assert!(
            !skip.consume("next line"),
            "the count (1) is exhausted after the one archived line"
        );
    }

    /// **The first-line guard, mismatching — the Critical fix, fix-round
    /// 2.** kubelet log rotation (or anything else) can move a node's log
    /// window forward between attaches, so a fresh read's first line need
    /// not be the archive's first line any more. When it is not, the count
    /// must not be trusted at all: suppressing nothing here is the
    /// duplication direction, which is safe; suppressing by the stale count
    /// would drop real lines the archive never had a chance to see.
    #[test]
    fn a_mismatched_first_line_suppresses_nothing() {
        let resume = aligned_resume("a", &["l0", "l1", "l2"]);
        let mut skip = LineSkip::for_node(&resume, "wf-1", "a");

        // The fresh read's first line is not "l0" -- rotation has moved
        // the window forward past it.
        let suppressed: Vec<bool> = ["l2", "l3", "l4"]
            .into_iter()
            .map(|line| skip.consume(line))
            .collect();

        assert_eq!(
            suppressed,
            vec![false, false, false],
            "a misaligned window suppresses nothing at all, not even the lines \
             that happen to coincide with what the archive has",
        );
    }

    /// The first-line guard is per node: a mismatch for one node must not
    /// disable suppression for another, and a node with nothing archived at
    /// all is never compared against anything.
    #[test]
    fn the_first_line_guard_is_per_node() {
        let resume_a = aligned_resume("a", &["a0", "a1", "a2"]);
        let resume_b = aligned_resume("b", &["b0", "b1"]);
        let mut skip_a = LineSkip::for_node(&resume_a, "wf-1", "a");
        let mut skip_b = LineSkip::for_node(&resume_b, "wf-1", "b");

        // Node a's window has rotated; node b's has not.
        assert!(
            !skip_a.consume("a-rotated-past-the-anchor"),
            "node a is misaligned"
        );
        assert!(
            skip_b.consume("b0"),
            "node b's own guard is unaffected by node a's mismatch"
        );
        assert!(skip_b.consume("b1"), "node b keeps suppressing normally");
    }

    /// A resume larger than the lines this read will ever produce suppresses
    /// all of them and does not panic or underflow — the case a stale or
    /// otherwise-too-high resume position produces. The last-line guard
    /// never fires here: `remaining` never reaches `0`, so "suppression
    /// completed" never happens within this read.
    #[test]
    fn a_resume_larger_than_available_lines_suppresses_all_of_them() {
        // The last-line anchor is never reached (`remaining` stays above `0`
        // for this whole read), so its value cannot matter here.
        let resume = resume_with("a", 1_000_000, "l0", "irrelevant -- never reached");
        let mut skip = LineSkip::for_node(&resume, "wf-1", "a");

        for (i, line) in ["l0", "l1", "l2", "l3", "l4", "l5", "l6", "l7", "l8", "l9"]
            .into_iter()
            .enumerate()
        {
            assert!(
                skip.consume(line),
                "a read this short never exhausts the count (line {i})"
            );
        }
    }

    /// Suppression is per node, not global: a resume for one node must not
    /// suppress a single line of another's, however large its own count is.
    #[test]
    fn suppression_is_per_node_not_global() {
        let resume = resume_with("a", 10, "a0", "a9");
        let mut skip_a = LineSkip::for_node(&resume, "wf-1", "a");
        let mut skip_b = LineSkip::for_node(&resume, "wf-1", "b");

        assert!(skip_a.consume("a0"), "node a has 10 archived lines to skip");
        assert!(
            !skip_b.consume("anything"),
            "node b has none, regardless of node a's count",
        );
    }

    /// **The last-line guard fires when the count is inflated — Important,
    /// fix-round 2.** A pre-fix archive whose count over-counts this node's
    /// real content still passes the first-line guard (the window has not
    /// moved, only the count is wrong), so suppression proceeds and
    /// consumes real, never-before-archived lines it should not have. The
    /// mismatch is detected once the count is exhausted, and logged loudly
    /// rather than silently, because nothing at that point can undo the
    /// suppression already applied.
    #[test]
    #[tracing_test::traced_test]
    fn a_mismatched_last_line_logs_but_still_suppresses() {
        // Archive says node a has 2 lines, "l0" then "l1" -- but the real
        // log only ever had "l0"; "l1" is a pre-fix duplicate of "l0" that
        // never really existed as a second, distinct line.
        let resume: LogResume = [(
            "a".to_owned(),
            LogPosition {
                lines: 2,
                first_line: "l0".to_owned(),
                last_line: "l1".to_owned(),
            },
        )]
        .into_iter()
        .collect();
        let mut skip = LineSkip::for_node(&resume, "wf-1", "a");

        // The fresh read's real content: "l0", then genuinely new output
        // that was never archived at all.
        assert!(skip.consume("l0"), "the first line still matches");
        assert!(
            skip.consume("new-output-never-archived"),
            "the count says 2, so this is still suppressed -- wrongly"
        );
        assert!(
            !skip.consume("more-new-output"),
            "the count is now exhausted; later lines emit normally"
        );

        assert!(
            logs_contain("archived line count did not match its re-read log"),
            "the mismatch must be logged loudly, since it cannot be recovered",
        );
    }

    /// **A pathological log line is truncated before it enters the
    /// broadcaster.**
    ///
    /// Truncation lived only on the read side (`api::rest::sse`), so a 2 MB
    /// line was carried in full through the broadcaster and into the archive
    /// and only shrank when a reader asked. One such line per node is
    /// hundreds of megabytes of resident memory for output no reader can
    /// ever receive in full.
    ///
    /// The same cap and the same helper as the read side, deliberately: two
    /// truncation rules that must agree is a drift this crate has been
    /// bitten by (see `LogPosition`'s doc on `flatten_log_char`, one such
    /// drift already fixed once). Review finding #30.
    #[tokio::test]
    async fn a_pathological_line_is_truncated_before_it_is_emitted() {
        let (sink, mut stream) = ExecutionStream::channel(4);
        let mut parser = MarkerParser::new("node-1");
        let mut skip = LineSkip::for_node(&LogResume::default(), "wf-1", "node-1");

        assert!(
            handle_line(
                &sink,
                &mut parser,
                &mut skip,
                "node-1",
                "y".repeat(MAX_LINE_BYTES * 4),
            )
            .await,
            "the observer is still attached; this must not report false"
        );
        drop(sink);

        let mut emitted = Vec::new();
        while let Some(event) = stream.recv().await {
            if let ExecutionEvent::Log { line, .. } = event {
                emitted.push(line);
            }
        }

        assert_eq!(
            emitted.len(),
            1,
            "exactly one Log event for the one line fed in"
        );
        assert!(
            emitted[0].len() <= MAX_LINE_BYTES + TRUNCATION_MARKER_MAX,
            "the emitted line must be capped, was {} bytes",
            emitted[0].len()
        );
    }
}
