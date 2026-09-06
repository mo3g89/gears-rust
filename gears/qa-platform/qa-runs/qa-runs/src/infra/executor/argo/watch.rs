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
//!     terminal phase? -> one more pod pass; emit Finished only if no pod in
//!                        it stopped part-way, then end
//!     sleep
//! }
//! ```
//!
//! "No pod stopped part-way" is deliberately narrower than "every log was
//! read", and the gap is not an oversight: a pod with no log endpoint, a pod
//! that never reached a phase with one, and a failed pod `list` all leave the
//! pass able to emit `Finished`. `Watcher::drain_pods`' own doc lists all
//! three and says which of them is a pre-existing hole this file has not
//! closed.
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
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::api::rest::sse::sanitize_line_for_archive;
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
///
/// `cancel` is a child of [`ArgoRunExecutor`](super::ArgoRunExecutor)'s own
/// token — see that type's `cancel` field doc for why it arrives at
/// construction rather than as a `watch()` parameter. [`Watcher::run`] and
/// [`Watcher::follow`] select on it; review findings #20/#21.
pub async fn start(
    client: Client,
    config: ArgoExecutorConfig,
    execution_ref: &ExecutionRef,
    resume: LogResume,
    cancel: CancellationToken,
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
        cancel,
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
    /// The executor's own shutdown signal, as a child token — see
    /// [`super::ArgoRunExecutor`]'s `cancel` field doc for why it lives there
    /// and arrives here rather than being invented per call. [`Self::run`]
    /// selects on it between status polls and before each status read;
    /// [`Self::follow`] selects on it between log lines. Review findings
    /// #20/#21: before this, nothing ever asked a spawned `Watcher` to stop.
    cancel: CancellationToken,
}

/// How far a log read got: one pod's, in [`Watcher::follow`], or one whole
/// pass's, in [`Watcher::drain_pods`], which folds its pods' answers into a
/// single one of these.
///
/// # Why this is not a `bool`, which is the bug it exists to have fixed
///
/// `follow` used to return `bool`, and after whole-branch review C1 that
/// `false` meant two different things: *"this pod stopped part-way"* (the
/// idle deadline, a mid-stream reset) and *"end the whole observation"*
/// (cancelled, sink closed). `drain_pods` read every `false` as the second
/// and `return false`d at the first pod that gave up, so the pods **after**
/// it were never followed at all. `drained` is per-`Watcher`, every
/// re-attach builds a fresh one, and the pod `list` order is stable — so a
/// persistently wedged pod that sorts ahead of its siblings was hit first on
/// every pass and their logs and `=== TEST_CASE: … ===` markers were never
/// read for the life of the run. Multi-repository custom plans are exactly
/// the shape that produces sibling pods.
///
/// Three variants rather than a second bool beside the first, because two
/// bools is the same collapse one indirection later: the caller would still
/// have to remember which combination means what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FollowOutcome {
    /// Nothing more to read here, and nothing that should hold back a
    /// verdict. **Two sites in [`Watcher::follow`] produce it, and only the
    /// first marks the pod drained**, which is why this variant is not named
    /// "drained":
    ///
    /// * end-of-file — the pod's log provably has nothing more to say, so
    ///   [`Watcher::follow`] records it in `drained` and no later pass of
    ///   this `Watcher` re-reads it;
    /// * [`Watcher::open_log`] answering `None` — *"this pod has no log
    ///   endpoint"*, left exactly as it was before this change. It drains
    ///   nothing and is retried on the next pass, and it deliberately does
    ///   **not** block [`Watcher::finish`]: a pod that never produces a log
    ///   at all would otherwise hold its run open until the control-plane
    ///   timeout sweep reclaimed it. See that method's own doc, which also
    ///   names the terminal-workflow case in which `None` is not "not yet".
    ///
    /// **Two more sites produce it in [`Watcher::drain_pods`], and neither
    /// read a log at all**: a failed pod `list` (no pod was examined), and
    /// the fold's own default (a pass in which every pod was skipped, or
    /// there were none). Both are named on that method's *"Two things report
    /// `Complete` without having read a log"* section, which is the one to
    /// read before treating this variant as evidence that anything was read.
    /// This is the whole-pass reading of the same variant; see the type
    /// header above for why one enum carries both scopes.
    Complete,
    /// We were reading this pod's log and stopped part-way: the idle
    /// deadline fired, or the stream errored mid-read. The rest of that log
    /// is unread and nothing here knows how much of it there was.
    ///
    /// The pod is **not** drained, so a later `Watcher` re-follows it, and
    /// the pass carrying this must not reach [`Watcher::finish`] — a
    /// `Finished` emitted over an unread log is what retires the run out of
    /// `active_states()` and forecloses that re-attach (C1's own trace, in
    /// [`Watcher::follow`]'s doc). Its siblings in the same pass are still
    /// followed; that is what separates this from [`Self::Stop`].
    Incomplete,
    /// End the whole observation now, emitting nothing further: the gear's
    /// cancellation token fired, or the sink is closed (any `emit` /
    /// `emit_results` answering `false` — there is nobody left to emit to).
    ///
    /// Neither is a verdict about any one pod, so no pod after this one is
    /// followed either.
    Stop,
}

impl Watcher {
    /// # Where cancellation is checked, and why that is enough
    ///
    /// `self.cancel` is raced against the two points this loop can otherwise
    /// block for a while: the status read and the between-poll sleep.
    /// `drain_pods`/`follow` are not raced here directly — [`Self::follow`]
    /// already selects on the same token around its own blocking read, so a
    /// cancellation arriving mid-follow is caught there, at worst after the
    /// idle deadline or the current line, not after this whole loop's poll
    /// interval. [`CancellationToken::cancelled`] resolves immediately if the
    /// token is already cancelled, so a cancellation that lands between two
    /// selects (rather than during one) is still caught at the very next one,
    /// not merely "eventually" — there is no polling delay of its own to add.
    #[allow(
        clippy::cognitive_complexity,
        reason = "the two `tokio::select!`s Task 17 added each carry their own cancelled-arm \
                  `info!` beside the existing branch, and the two `match`es on \
                  `FollowOutcome` each carry an `info!` of their own; the metric counts every \
                  arm and macro expansion, while the loop's own shape (poll, drain, terminal \
                  check) is the one it has always had"
    )]
    async fn run(&mut self) {
        let poll = std::time::Duration::from_secs(self.config.status_poll_seconds.max(1));
        loop {
            let workflow = tokio::select! {
                () = self.cancel.cancelled() => {
                    info!(execution_ref = %self.name, "ending this observation (cancelled)");
                    return;
                }
                result = self.workflows.get_opt(&self.name) => match result {
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
                },
            };

            let phase = phase_of(&workflow).to_owned();
            if !self.started && !phase.is_empty() && phase != "Pending" {
                self.started = true;
                if !self.sink.emit(ExecutionEvent::Started).await {
                    return;
                }
            }

            match self.drain_pods().await {
                FollowOutcome::Stop => return,
                // At least one pod stopped part-way. End this observation
                // without a verdict rather than polling again in place: a
                // second follow of the same pod by the *same* `Watcher` would
                // re-read it from byte 0 against `self.resume`, which still
                // holds the count from before this call started, and so would
                // re-emit everything this call already emitted for it (see
                // `drained`'s field doc, which is why a pod is followed at
                // most once per call). A fresh `Watcher` from
                // `reattach_watchers` gets a fresh `LogResume` instead, and
                // `LineSkip` suppresses what is already archived.
                FollowOutcome::Incomplete => {
                    info!(
                        execution_ref = %self.name,
                        "a pod's log stopped part-way; ending this observation without a \
                         verdict so the dispatcher's re-attach can read the rest"
                    );
                    return;
                }
                FollowOutcome::Complete => {}
            }

            if is_terminal(&phase) {
                // A pod can only be created while the workflow is not terminal,
                // so one more pass catches anything that appeared during the
                // last one.
                //
                // **This second pass is the one that gates the verdict**, and
                // it is the current answer rather than a stale one: every pod
                // the first pass left un-drained is followed again here, and
                // every pod it drained is skipped. The first pass cannot have
                // been `Incomplete` and still reach this line: that arm
                // returned above.
                //
                // **What `Complete` here does and does not assert.** It
                // asserts that no pod was observed to stop part-way. It does
                // **not** assert that every pod's log was read: a pod with no
                // log endpoint, a pod whose phase never reached
                // `Running`/`Succeeded`/`Failed`, and — the one worth
                // stopping at, because it means *no pod was examined at all*
                // — a failed pod `list` all answer `Complete` too. So this
                // gate closes C1's hole (a verdict over a log we watched stop
                // mid-read) and leaves the pre-existing one (a verdict over a
                // log we never opened) exactly where it was. `drain_pods`'
                // "Three things report `Complete` without having read a log"
                // is the full list; do not read this `Complete` as "every log
                // was drained".
                match self.drain_pods().await {
                    FollowOutcome::Stop => return,
                    FollowOutcome::Incomplete => {
                        info!(
                            execution_ref = %self.name,
                            "this workflow is terminal but a pod's log is unread; ending \
                             without a verdict so the run stays in `active_states()` and the \
                             dispatcher re-attaches"
                        );
                        return;
                    }
                    FollowOutcome::Complete => {}
                }
                self.finish(workflow).await;
                return;
            }

            tokio::select! {
                () = self.cancel.cancelled() => {
                    info!(execution_ref = %self.name, "ending this observation (cancelled)");
                    return;
                }
                () = tokio::time::sleep(poll) => {}
            }
        }
    }

    /// Emit the terminal event, re-reading the object first so `status.nodes`
    /// and `status.message` are the final ones.
    ///
    /// Not raced against `self.cancel`, same as [`Self::open_log`]: one
    /// bounded request/response, covered by `kube::Config::read_timeout`
    /// (295 s, connector-level) rather than by anything this file adds.
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

    /// Follow every not-yet-drained pod's log to end-of-file, and report how
    /// far the pass as a whole got.
    ///
    /// # The fold, which is the fix
    ///
    /// * [`FollowOutcome::Stop`] from any pod returns immediately: the token
    ///   fired or the sink is closed, so there is nobody to emit the
    ///   remaining pods' lines to anyway.
    /// * [`FollowOutcome::Incomplete`] from any pod is **remembered and the
    ///   loop continues**. That is the whole difference from the shape this
    ///   replaces, which returned at the first such pod and so never reached
    ///   its siblings on any pass of any `Watcher` — see [`FollowOutcome`]'s
    ///   own doc for why that was permanent rather than a delay. The pass
    ///   reports `Incomplete`, and [`Self::run`] is what turns that into "no
    ///   verdict"; this method emits no verdict either way.
    /// * [`FollowOutcome::Complete`] only if every pod this pass looked at
    ///   answered `Complete`.
    ///
    /// A pod already in `drained` is skipped without being followed and does
    /// not make the pass incomplete: it is at end-of-file already.
    ///
    /// # Three things report `Complete` without having read a log, unchanged
    ///
    /// Named because `Complete` otherwise reads as "we read everything", and
    /// on a terminal workflow this is the answer that releases the verdict.
    /// **`Complete` is the pass's *default*, not an achievement**: it is what
    /// this method answers when nothing downgraded it, including when
    /// nothing was looked at.
    ///
    /// * **A pod whose phase is not yet `Running`/`Succeeded`/`Failed`** is
    ///   skipped by the loop below without being followed at all — it has no
    ///   log endpoint, and asking produces a 400 per pass. Same disposition
    ///   as [`Self::open_log`]'s `None`, and for the same reason: blocking
    ///   the verdict on a pod that may never produce a log would hold the run
    ///   open until the control-plane timeout sweep reclaimed it.
    /// * **A failed pod `list`** returns `Complete`, exactly as it returned
    ///   `true` before this change. That is a pre-existing hole of the same
    ///   family as C1 (a `Finished` over logs this pass never saw),
    ///   deliberately left alone here: making it `Incomplete` would put every
    ///   run whose API server rejects `list` into the re-attach loop, which
    ///   is a separate decision from this fix. Note what `Complete` means at
    ///   *that* return: not "every pod is at end-of-file" but "no pod was
    ///   examined at all".
    /// * **A pass with nothing to follow** — an empty `list`, or one whose
    ///   every pod was skipped by the two clauses above — falls out of the
    ///   loop still holding the initial `Complete`. On a terminal workflow
    ///   the second pass is normally exactly this, which is intended: its
    ///   pods were drained by the first pass. It is listed because the same
    ///   answer arrives from a workflow that never had a pod at all.
    ///
    /// The pod `list` call below is not raced against `self.cancel`, same
    /// reason as [`Self::open_log`]: one bounded request/response, covered by
    /// `kube::Config::read_timeout` rather than by anything this file adds.
    /// What *is* raced is the per-pod [`Self::follow`] this method calls in a
    /// loop — pods are still followed one at a time, so a wedged pod still
    /// delays its siblings by up to
    /// [`ArgoExecutorConfig::log_follow_idle_seconds`](crate::config::ArgoExecutorConfig::log_follow_idle_seconds)
    /// per pass. A delay is what it now is; it used to be an abort.
    async fn drain_pods(&mut self) -> FollowOutcome {
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
                return FollowOutcome::Complete;
            }
        };

        let mut pass = FollowOutcome::Complete;
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
            match self.follow(&pod_name, &node_of(&pod)).await {
                FollowOutcome::Complete => {}
                // Remembered, not returned: the pods after this one still
                // have logs and `=== TEST_CASE: … ===` markers to read.
                FollowOutcome::Incomplete => pass = FollowOutcome::Incomplete,
                FollowOutcome::Stop => return FollowOutcome::Stop,
            }
        }
        pass
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
    ///
    /// # Where this ends, and what each ending leaves behind
    ///
    /// Review findings #20/#21: `follow: true` on a wedged API-server
    /// connection used to hold this task for the life of the process, because
    /// nothing bounded the read and nothing could ask it to stop. Task 17
    /// bounded it. **Whole-branch review C1 corrected what the bound then
    /// did** — its two previous versions both said the idle give-up "stops
    /// only this pod" and cost "a loss of granularity, not of correctness",
    /// which was false of an observation that ended by reporting a *verdict*
    /// (the trace below is what that fix was derived from). **C1's own fix
    /// then had to be corrected in turn**, because it made the give-up return
    /// the same `false` as "the observer has gone away", and
    /// [`Self::drain_pods`] read that as "stop everything" — which cost the
    /// wedged pod's siblings their entire logs. That is why this function
    /// answers with a [`FollowOutcome`] and not a `bool`.
    ///
    /// ## Every exit this function has, counted off the code
    ///
    /// The first version of this section said *"every other exit `return
    /// false`s from inside the loop"*, which is an absolute with two
    /// counterexamples in this same function -- exactly the habit the trace
    /// below exists to break. So here they all are, in source order, with what
    /// each leaves behind:
    ///
    /// | Exit | Answers | Pod drained? | Siblings still followed? |
    /// |---|---|---|---|
    /// | [`Self::open_log`] answered `None` (before the loop) | `Complete` | **no** | yes |
    /// | `Ok(Ok(None))` -- end-of-file | `Complete` | **yes** | yes |
    /// | `handle_line` reported the observer gone | `Stop` | no | no |
    /// | Cancelled | `Stop` | no | no |
    /// | Idle deadline elapsed | `Incomplete` | no | **yes** |
    /// | The stream errored mid-read | `Incomplete` | no | **yes** |
    /// | `emit_results` reported the observer gone (after the loop) | `Stop` | no | no |
    ///
    /// Two of those are not "this pod gave up" at all and are listed only so
    /// the count is honest: `open_log`'s `None` is *"not yet"* -- a pod whose
    /// container has no log endpoint yet, retried on the next poll pass, which
    /// is why it answers `Complete` without draining anything and without
    /// holding back the verdict (see that method's own doc, and
    /// [`FollowOutcome::Complete`]) -- and the two observer-gone exits are
    /// `ExecutionSink::emit`'s own `false`, which needs no verdict because
    /// there is nobody left to give one to.
    ///
    /// What matters for C1 is the pair in the middle. **End-of-file is the
    /// only exit that marks the pod drained**, and that is the one case in
    /// which this pod provably has nothing more to say. The three give-up
    /// exits leave the pod un-drained, and they differ in blast radius:
    ///
    /// * **Cancelled** — the same token [`Self::run`] selects on. This is the
    ///   executor's own shutdown signal (see [`super::ArgoRunExecutor`]'s
    ///   `cancel` doc), not a verdict about this one pod, so there is nothing
    ///   left worth reading for *any* pod: `Stop`, and no sibling is followed
    ///   after it. Losing an unread tail here is no worse than losing it to
    ///   the process exiting mid-read, which was already possible before
    ///   Task 17.
    /// * **Idle deadline elapsed** — [`crate::config::ArgoExecutorConfig::log_follow_idle_seconds`],
    ///   reset on every line, not on the whole follow — see that field's own
    ///   doc for why a *lifetime* bound here would turn this fix into log loss
    ///   on a healthy long-running node. Nothing about this call can tell
    ///   "wedged forever" apart from "quiet a moment too long", so the give-up
    ///   is a bet; `Incomplete` is what keeps the bet's cost to this pod.
    /// * **The stream errored mid-read** — a reset on a long-lived
    ///   `follow: true` connection, which is ordinary rather than exotic. Same
    ///   disposition, and it is the same defect: the rest of this pod's log is
    ///   unread, and nothing here knows how much of it there was.
    ///
    /// ## What `break` did instead, traced against the run's own state
    ///
    /// Stated as a trace because this file has now been wrong five times about
    /// an absolute it reasoned to from the design rather than read off the
    /// code. Each link below was read at its own definition:
    ///
    /// 1. `break` fell through to `emit_results(parser.finish())` and
    ///    `self.drained.insert(pod_name)`, and returned `true`.
    /// 2. `drained` is never cleared (see its field doc), so this `Watcher`
    ///    never followed that pod again.
    /// 3. `run` therefore reached its `is_terminal` branch normally and called
    ///    [`Self::finish`], which emits [`ExecutionEvent::Finished`].
    /// 4. `IngestService::finish` records the terminal state, so the run
    ///    leaves `infra::storage::runs_sea_repo::active_states` — which is
    ///    `dispatching | running` and nothing else.
    /// 5. `watch_candidates_query` filters on exactly that condition, so
    ///    `domain::service::dispatch::reattach_watchers` no longer had the run
    ///    as a candidate. The re-attach the old prose promised was not delayed;
    ///    it could not happen at all.
    ///
    /// The loss was not granularity. Every line after the gap was absent from
    /// the archive and from SSE, every `=== TEST_CASE: … ===` marker after it
    /// was never parsed so those tests produced no row, the five counters are
    /// tallied from stored rows and so reported a partial set, and the verdict
    /// still came from Argo's workflow phase — so a runner that exits 0 despite
    /// failures yielded a run reported **Succeeded with a truncated,
    /// all-passing result set**. Reproduced before the fix by
    /// `an_idle_give_up_neither_drains_the_pod_nor_finishes_the_run`, which
    /// observed exactly `[Started, Finished { outcome: Succeeded, .. }]` from a
    /// pod that had emitted nothing at all.
    ///
    /// ## What an `Incomplete` costs now, which is a loop and is meant to be visible
    ///
    /// `Incomplete` emits no `Finished` of its own and none is emitted over
    /// it: [`Self::drain_pods`] carries it to the end of the pass and
    /// [`Self::run`] then returns without a verdict, which is what
    /// `domain::service::watch`'s `drain` already calls *"nothing more to
    /// say"*, **never** *"this failed"*. The slot is released, the run stays
    /// in `active_states`, and `reattach_watchers` re-attaches on its next 5 s
    /// tick with a fresh [`LogResume`], which suppresses whatever was already
    /// archived. So a pod that was merely quiet resumes without duplicating
    /// what it had already emitted.
    ///
    /// **A fresh `Watcher` rather than another pass of this one, deliberately.**
    /// `run` could poll again in place instead of returning, but a second
    /// follow of the same pod by the *same* `Watcher` seeds [`LineSkip`] from
    /// `self.resume` again — the archived count from before this call started
    /// — and would re-emit everything this call had already emitted for that
    /// pod. `drained`'s field doc is the same invariant from the other side: a
    /// pod is followed at most once per `watch` call.
    ///
    /// **Unless that node's log has rotated.** [`LineSkip::consume`]'s
    /// first-line guard is what compares the re-read against the archive, and
    /// its own doc records what happens when they disagree: suppression is dead
    /// for that node for the rest of the run and *"its archive will grow
    /// unbounded on every further re-attach"*, announced by a `debug!` that
    /// names rotation as the expected cause. That residual is not new, but C1
    /// made it **more reachable**: before it, a mid-stream reset drained
    /// the pod and produced no re-attach at all, so the guard was never
    /// re-consulted; now every reset produces one. Bounded duplication in
    /// exchange for a correct verdict is still the right trade, but "loses
    /// nothing" is the wrong summary and an earlier version of this paragraph
    /// said it.
    ///
    /// A pod whose connection is *genuinely* wedged does not resume: it wedges
    /// the next attach too, and the pair re-attach until the control-plane
    /// timeout sweep reclaims the run. That loop is the price, and it is the
    /// disposition `drain`'s own doc already accepts for a forgotten execution
    /// — *"re-watched once per tick until the control-plane timeout sweep
    /// reclaims it, which is a bounded cost paid for never inventing a
    /// verdict"*.
    ///
    /// ### The loop's period is not uniform, and it goes quiet halfway through
    ///
    /// Two regimes, and only the first is loud:
    ///
    /// * **While the Argo workflow object still exists**, each pass costs
    ///   [`ArgoExecutorConfig::log_follow_idle_seconds`](crate::config::ArgoExecutorConfig::log_follow_idle_seconds)
    ///   *per wedged pod* — they are followed one at a time — plus the
    ///   dispatcher's 5 s tick, and each wedged pod logs the `warn!` below
    ///   naming itself, its node and the deadline.
    /// * **Once `workflow_ttl_seconds` collects the workflow**, [`Self::run`]'s
    ///   `Ok(None)` arm returns immediately -- no pods listed, no follow, and
    ///   no log line of its own -- so the period tightens to the bare 5 s tick
    ///   and the only trace left is `drain`'s INFO. For any run whose timeout
    ///   exceeds the workflow TTL, that is the steady state.
    ///
    /// So "every pass emits a `warn!`" is true only of the first regime. It is
    /// still incomparably louder than the old behaviour, which logged one line
    /// and then silently shipped a wrong verdict.
    ///
    /// ### "Until the timeout sweep reclaims it" has one documented exception
    ///
    /// `LaunchService`'s own doc (`domain::service::launch`, the `plan.yaml`
    /// `timeout_seconds` saturation) records a run whose `timeout_at` is NULL
    /// because an unclamped plan timeout saturated, and
    /// `timeout_candidates_query` excludes exactly those -- *"a run the
    /// control-plane timeout sweep can never reclaim"*. For such a run this
    /// loop has no terminator: small population, unbounded duration. It is a
    /// pre-existing hole that C1 gave a new way to occupy, not one this file
    /// opens, and it is recorded here because "bounded" is a claim and this is
    /// the case in which it is false.
    ///
    /// ## What a wedged pod costs its siblings, which is a delay again
    ///
    /// `run` does not run concurrently with `follow` -- it is synchronously
    /// blocked inside [`Self::drain_pods`] for however long this call takes --
    /// and pods are followed one at a time in a stable `list` order. So a
    /// wedged pod that sorts first still holds every sibling behind it for up
    /// to one `log_follow_idle_seconds`, on every pass, and there is no
    /// concurrency here that would avoid it (this module's own "Known
    /// limitation" header).
    ///
    /// **The version of this section that stood here described that as an
    /// abort rather than a delay, and it was right about the code it was
    /// written against.** C1's `return false` made `drain_pods` stop at the
    /// first pod that gave up; `drained` is per-`Watcher`, every re-attach
    /// builds a fresh one, and the `list` order is stable — so a persistently
    /// wedged pod that sorted ahead of its siblings was hit first on every
    /// pass and their logs and `=== TEST_CASE: … ===` markers were never read
    /// for the life of the run. [`FollowOutcome`] is what separates the two
    /// meanings that `false` had, and `drain_pods`' loop is what now walks
    /// past a give-up to the pods behind it.
    ///
    /// A delay is not free, and this is not a claim that no sibling can lose
    /// anything: a long deadline against a short run can still have the
    /// control-plane timeout sweep reclaim the run before a sibling behind a
    /// wedged pod is ever reached. What is gone is the *permanence* — the
    /// siblings are read on this pass and on every later one, rather than on
    /// none.
    ///
    /// The deadline is anchored per line rather than per follow for this same
    /// reason: a lifetime bound would abort a healthy long-running pod, and take
    /// every sibling behind it down with it.
    #[allow(
        clippy::cognitive_complexity,
        reason = "Task 17 wrapped the read in a `tokio::select!` (its own cancelled arm) and \
                  a `tokio::time::timeout` (its own elapsed arm), on top of the pre-existing \
                  three-way line/eof/error match; each arm carries a `warn!`/`info!` naming a \
                  different way this follow can end, which the metric counts fully"
    )]
    async fn follow(&mut self, pod_name: &str, node: &str) -> FollowOutcome {
        let Some(stream) = self.open_log(pod_name).await else {
            return FollowOutcome::Complete;
        };
        let mut parser = MarkerParser::new(node);
        let mut skip = LineSkip::for_node(&self.resume, &self.name, node);
        let mut lines = stream.lines();
        let idle = std::time::Duration::from_secs(self.config.log_follow_idle_seconds.max(1));
        loop {
            let next = tokio::select! {
                () = self.cancel.cancelled() => {
                    info!(
                        execution_ref = %self.name,
                        pod = %pod_name,
                        "log follow stopping (cancelled)"
                    );
                    return FollowOutcome::Stop;
                }
                outcome = tokio::time::timeout(idle, lines.try_next()) => outcome,
            };
            match next {
                Ok(Ok(Some(line))) => {
                    if !handle_line(&self.sink, &mut parser, &mut skip, node, line).await {
                        return FollowOutcome::Stop;
                    }
                }
                // End of log. The pod is finished, so this is the point at
                // which the last test's observation exists.
                Ok(Ok(None)) => break,
                // A reset on a `follow: true` stream, which is ordinary. The
                // rest of this pod's log is unread, so this pod is not
                // drained and no verdict may be reported over it — but its
                // siblings in this pass are still read: see the doc above.
                Ok(Err(error)) => {
                    warn!(
                        execution_ref = %self.name,
                        pod = %pod_name,
                        node = %node,
                        %error,
                        "this pod's log stream ended early; leaving this pod un-drained so no \
                         verdict is reported over it and the dispatcher's re-attach can read \
                         the rest"
                    );
                    return FollowOutcome::Incomplete;
                }
                Err(_elapsed) => {
                    warn!(
                        execution_ref = %self.name,
                        pod = %pod_name,
                        node = %node,
                        idle_seconds = idle.as_secs(),
                        "this pod's log produced no line within the idle deadline; leaving \
                         this pod un-drained so no verdict is reported over it and the \
                         dispatcher's re-attach can read the rest (review findings #20/#21, \
                         whole-branch review C1)"
                    );
                    return FollowOutcome::Incomplete;
                }
            }
        }

        // `finish` is what produces the final test's observation - see
        // `markers`' "one-test lag". Emitted before this function returns, and
        // therefore before `Finished`.
        if !emit_results(&self.sink, parser.finish()).await {
            return FollowOutcome::Stop;
        }
        self.drained.insert(pod_name.to_owned());
        FollowOutcome::Complete
    }

    /// Open a follow-mode stream on one pod's `main` container log, from
    /// byte 0 — see [`LineSkip`]'s doc for why this carries no resume-shaped
    /// parameter at all.
    ///
    /// `None` is "not yet", not "never": a pod that has only just gone Running
    /// can still refuse a log request, so the caller retries on the next pass
    /// rather than marking the pod drained. Boxed so the type is nameable,
    /// which is what lets [`Self::follow`] stay small.
    ///
    /// # "Not yet" is an assumption about *when* this was asked, and it is
    /// # sometimes wrong
    ///
    /// Nothing here distinguishes the two cases, and only one of them is
    /// benign:
    ///
    /// * **On a non-terminal workflow**, "not yet" is almost always right: the
    ///   container is still starting, the next poll pass asks again, and the
    ///   pod is followed then.
    /// * **On a terminal workflow with a `Succeeded`/`Failed` pod**, there is
    ///   no "yet" left. A `None` there means the log is gone or unreachable —
    ///   kubelet rotation or garbage collection took it, the pod object
    ///   outlived its container, or the API server returned a transient error
    ///   this method logs at `debug!` and discards. And that is precisely when
    ///   it matters, because [`FollowOutcome::Complete`] is what releases the
    ///   verdict: `run`'s terminal pass will emit `Finished` over a pod whose
    ///   log was never opened at all.
    ///
    /// **Left as it is, deliberately** — see [`FollowOutcome::Complete`] and
    /// the brief that requested this shape. Blocking `finish` on a `None`
    /// would hang every run whose pod genuinely never produces a log, in a
    /// re-attach loop the control-plane timeout sweep cannot always terminate
    /// (`domain::service::launch`'s saturated-`timeout_seconds` case, recorded
    /// in [`Self::follow`]'s doc). Trading a rare truncated-but-finished run
    /// for a rare run that never finishes at all is not obviously the right
    /// trade, and making it would need the two cases told apart — by the
    /// workflow phase and the pod phase this method is not currently given —
    /// rather than by treating every `None` as the worse one. What is not
    /// acceptable is leaving the reader thinking `None` only ever means
    /// "not yet", which is what this section is for.
    ///
    /// **Not raced against `self.cancel`.** This is one bounded request/response
    /// (`kube`'s own HTTP timeout applies, the same as every other call `run`
    /// makes), not the open-ended `follow: true` read [`Self::follow`]'s own
    /// loop guards — the finding this task fixes is about that read, not
    /// about opening it. A cancellation that lands while this is in flight is
    /// observed at the very next loop iteration inside [`Self::follow`], not
    /// indefinitely: bounded by however long this one request takes, never by
    /// how long the pod itself keeps running.
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
/// [`sanitize_line_for_archive`] — `api::rest::sse`'s own write-side
/// truncation, capping at a budget that leaves room for the archive prefix
/// `IngestService::fan_out_log` wraps every line in (see that function's
/// doc for why a plain `sanitize_line` here would get re-truncated on
/// read, discarding the true dropped-byte count) — is computed once, here,
/// and used for two of this line's three destinations:
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
/// a marker line is not provably under `MAX_LINE_BYTES` the way an
/// ordinary log line usually is. And truncating it would not merely cut the
/// marker in half: [`sanitize_line_for_archive`] *appends* its own marker
/// after whatever survives the cut, so a truncated `TEST_CASE` line would no
/// longer end in `===` at all — `CASE_RE` (`^=== TEST_CASE: (\S+) ===$`)
/// would simply fail to match, and the whole case would be silently
/// dropped, not partially decoded. The parser's input has to stay raw to
/// avoid that.
///
/// # A residual this change accepts, and only for one of three positions
///
/// An archived `first_line`/`last_line` anchor from a run whose over-long
/// line was written before this fix deployed holds the *full, untruncated*
/// text — nothing rewrites an archive already on disk. A post-deploy
/// re-attach's now-truncated re-read of that same line will not match that
/// stale anchor, and what happens next depends on *where* the over-long
/// line sits in that node's archived output, because [`LineSkip`] has two
/// independent guards over three positions:
///
/// * **First archived line for that node**: the first-line guard fires,
///   exactly the misaligned-window case it exists for. Suppression is
///   disabled for that node for the rest of the re-attach — the safe
///   direction (duplication, not loss) — same as real log rotation.
/// * **A middle line**: neither guard runs against it at all — suppression
///   is purely count-based between the two anchors, so a stale (untruncated
///   in the archive, truncated on re-read) middle line changes what that
///   one re-read line's own bytes look like, not whether it is suppressed.
///   Nothing diverges: no duplication, no loss.
/// * **Last archived line for that node**: the last-line guard fires
///   instead, once the count is exhausted, and logs an `error!` asserting
///   the archive "most likely still carries duplicate lines from a run
///   that hit review finding #50" — which is not what happened here. That
///   `error!` is a false alarm with no data effect in this specific case;
///   an operator reading it would misdiagnose why.
///
/// Bounded to runs whose watch spans this deploy and whose affected node is
/// re-attached to afterwards.
async fn handle_line(
    sink: &ExecutionSink,
    parser: &mut MarkerParser,
    skip: &mut LineSkip,
    node: &str,
    line: String,
) -> bool {
    let sanitized = sanitize_line_for_archive(&line);
    let suppress = skip.consume(&sanitized);
    // `&line` (raw) for the parser, `sanitized` for everything else -- see
    // this function's own doc for why the split exists. Do not collapse
    // these back to one form without re-checking that a marker line still
    // cannot exceed `MAX_LINE_BYTES`, and do not swap `sanitize_line_for_archive`
    // back for plain `sanitize_line` without re-checking `WRITE_SIDE_MAX_LINE_BYTES`'s
    // doc -- that swap is exactly what let the read side re-truncate an
    // already-truncated line and lose the true dropped-byte count.
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
    //!
    //! Task 17 (review findings #20/#21) added the two `Watcher`-level tests
    //! at the bottom of this module: cancellation stopping a running `run()`,
    //! and `follow()` giving up on a pod whose log accepts a connection and
    //! then never writes to it. Both drive a real `kube::Client` built over a
    //! hand-rolled `tower::Service` rather than a live cluster — one that
    //! answers instantly (`responsive_client`), one whose response body never
    //! produces a frame (`silent_client`) — informed by
    //! `qa-plugin-k8s::test_support::StubApiServer`'s loopback-listener shape
    //! but not reusing it: that server always writes a full response the
    //! moment its route table returns, which cannot produce "accepts and
    //! never writes" — the shape review finding #21 needed a red test for.

    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use bytes::Bytes;
    use http_body::{Body as HttpBody, Frame};
    use kube::Client;
    use kube::api::Api;
    use tokio_util::sync::CancellationToken;

    use super::{FollowOutcome, LineSkip, Watcher, handle_line, workflow_resource};
    use crate::api::rest::sse::{MAX_LINE_BYTES, TRUNCATION_MARKER_MAX, sanitize_line_for_archive};
    use crate::config::ArgoExecutorConfig;
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

    /// **The property `handle_line`'s "two forms" doc turns on: `skip.consume`
    /// must see the *sanitized* form, not the raw one — fix round 1.**
    ///
    /// The test above seeds `LogResume::default()`, under which
    /// `skip.consume` returns `false` no matter what text it is given, so it
    /// cannot tell a correct call (`skip.consume(&sanitized)`) apart from the
    /// exact regression this crate's own resume invariant exists to prevent
    /// (`skip.consume(&line)`, the raw re-read). This test seeds an aligned
    /// resume whose one archived anchor is the *capped* form a real
    /// `fan_out_log` would have archived for this same over-long line, then
    /// re-feeds the identical raw line and asserts it is suppressed. Red
    /// under `skip.consume(&line)` (raw never matches a capped anchor, so
    /// nothing is ever suppressed for a node that once emitted an over-long
    /// line); green under the shipped `skip.consume(&sanitized)`.
    #[tokio::test]
    async fn a_re_read_pathological_line_is_suppressed_against_its_capped_anchor() {
        let huge = "y".repeat(MAX_LINE_BYTES * 4);
        let archived_anchor = sanitize_line_for_archive(&huge);
        let resume = aligned_resume("node-1", &[archived_anchor.as_str()]);

        let (sink, mut stream) = ExecutionStream::channel(4);
        let mut parser = MarkerParser::new("node-1");
        let mut skip = LineSkip::for_node(&resume, "wf-1", "node-1");

        assert!(
            handle_line(&sink, &mut parser, &mut skip, "node-1", huge).await,
            "the observer is still attached; this must not report false"
        );
        drop(sink);

        let mut emitted = Vec::new();
        while let Some(event) = stream.recv().await {
            if let ExecutionEvent::Log { line, .. } = event {
                emitted.push(line);
            }
        }

        assert!(
            emitted.is_empty(),
            "a re-read of an already-archived (capped) line must be suppressed, \
             not re-emitted: {emitted:?}"
        );
    }

    /// A response body that never produces a frame — modelling a wedged
    /// API-server connection from this process' point of view: the
    /// connection is up and the response headers already arrived (so
    /// `log_stream` itself succeeds), but no byte of the follow ever comes.
    ///
    /// `poll_frame` returns `Poll::Pending` and never wakes its `Context` —
    /// nothing here will ever have a frame to report, so there is nothing to
    /// wake it *for*. What actually drives `a_follow_with_no_output_gives_up_
    /// within_the_idle_deadline` forward is `Watcher::follow`'s own
    /// `tokio::time::timeout`, whose timer independently re-polls the
    /// `select!` at the deadline and drops this read — the same reason a
    /// real wedged socket does not need this test to poll it either.
    struct PendingBody;

    impl HttpBody for PendingBody {
        type Data = Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Pending
        }
    }

    /// A `kube::Client` that answers every request with `200` and a
    /// [`PendingBody`] — "accepts and never writes". Informed by
    /// `qa-plugin-k8s::test_support::StubApiServer`'s loopback-listener shape
    /// (this module's own doc explains why that server could not be reused
    /// as-is: it always writes a full response the moment its route table
    /// returns, which cannot produce a body that never completes).
    fn silent_client() -> Client {
        let service = tower::service_fn(
            move |_request: http::Request<kube::client::Body>| async move {
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(200)
                        .body(PendingBody)
                        .expect("building a stub response with a pending body"),
                )
            },
        );
        Client::new(service, "ns")
    }

    /// A `kube::Client` that answers every request instantly, with a body
    /// `route` picks from the request's own path — enough to serve
    /// `Watcher::run`'s two calls (a workflow `get_opt`, a pod `list`)
    /// without a cluster or a socket.
    fn responsive_client(route: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> Client {
        let route = std::sync::Arc::new(route);
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let route = std::sync::Arc::clone(&route);
            async move {
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(200)
                        .body(kube::client::Body::from(route(request.uri().path())))
                        .expect("building a stub response"),
                )
            }
        });
        Client::new(service, "ns")
    }

    /// A `Workflow` `DynamicObject`, `status.phase: Running` — non-terminal,
    /// non-empty, so `Watcher::run` emits `Started` and then reaches its
    /// between-poll sleep, which is the point the cancellation test targets.
    fn running_workflow_json() -> Vec<u8> {
        br#"{"apiVersion":"argoproj.io/v1alpha1","kind":"Workflow",
             "metadata":{"name":"wf-1","namespace":"ns"},
             "status":{"phase":"Running"}}"#
            .to_vec()
    }

    /// An empty `PodList` — no pod to follow, so `drain_pods` returns `true`
    /// at once and `run`'s loop reaches its sleep on the first pass.
    fn empty_pod_list_json() -> Vec<u8> {
        br#"{"apiVersion":"v1","kind":"PodList","metadata":{},"items":[]}"#.to_vec()
    }

    /// **Cancelling a running watcher makes `run()` return promptly — Critical,
    /// review finding #20.**
    ///
    /// `status_poll_seconds: 3600` is deliberate: pre-fix, with nothing in
    /// `run`'s loop ever consulting a token, this watcher is still asleep in
    /// `tokio::time::sleep(poll)` an hour later, so the bounding `timeout`
    /// below is what turns "would hang for an hour" into a fast, clean FAIL
    /// (the `.expect` panics on `Elapsed`) rather than an actual multi-minute
    /// hang — this task's own cancel test is not meant to reproduce the hang
    /// itself, only to prove cancellation is honoured; the sibling test below
    /// is where the real hang is observed. Post-fix, `run()` returns as soon
    /// as the spawned task's `cancel()` is observed, well inside the bound.
    #[tokio::test]
    async fn a_cancelled_watcher_returns_from_run_promptly() {
        let client = responsive_client(|path| {
            if path.contains("/pods") {
                empty_pod_list_json()
            } else {
                running_workflow_json()
            }
        });
        let (sink, _stream) = ExecutionStream::channel(8);
        let cancel = CancellationToken::new();
        let mut watcher = Watcher {
            workflows: Api::namespaced_with(client.clone(), "ns", &workflow_resource()),
            pods: Api::namespaced(client, "ns"),
            config: ArgoExecutorConfig {
                status_poll_seconds: 3600,
                ..ArgoExecutorConfig::default()
            },
            name: "wf-1".to_owned(),
            sink,
            started: false,
            drained: std::collections::HashSet::new(),
            resume: LogResume::default(),
            cancel: cancel.clone(),
        };

        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            canceller.cancel();
        });

        tokio::time::timeout(Duration::from_secs(5), watcher.run())
            .await
            .expect("run() must return promptly once cancelled, not wait out the poll interval");
    }

    /// **A follow that produces nothing gives up within its own idle deadline
    /// instead of hanging forever — Critical, review finding #21.**
    ///
    /// `silent_client` models a wedged API-server connection: the request
    /// succeeds (so this exercises `follow`'s read loop, not `open_log`), but
    /// no line — not even one — ever arrives. Pre-fix, `follow` neither
    /// selects on a token nor bounds the read at all, so this call never
    /// returns; that was observed directly (not through this test's own
    /// assertions, which cannot fire on a call that never returns) by running
    /// this test alone under a bounded shell timeout and watching the process
    /// get killed rather than the test failing on its own — the same "nextest
    /// times it out" signal the task brief describes, substituted here
    /// because this repository ships no `nextest.toml` `slow-timeout`/
    /// `terminate-after` for nextest's default profile to enforce one.
    ///
    /// **No wrapping `tokio::time::timeout` here, deliberately**: the fix has
    /// to make `follow()` return on its own, not merely make this test's own
    /// harness give up on it — wrapping it would make the red run indistinguishable
    /// from a passing assertion failure instead of the hang the fix is for.
    ///
    /// **What this asserts has been flipped twice, and the second flip is
    /// what makes the answer meaningful again.** It first asserted `true` —
    /// "giving up on an idle pod is not the same as the observer going away"
    /// — which was the sentence that made C1's defect look deliberate. C1
    /// made it `false`, the same `false` cancellation reports, so the return
    /// value stopped distinguishing the two arms at all and only *timing*
    /// did. [`FollowOutcome`] separates them again: this arm answers
    /// `Incomplete` (this pod stopped part-way; its siblings are still read)
    /// and the cancel arm answers `Stop` (end everything). The
    /// `log_follow_idle_seconds`/`status_poll_seconds` split with
    /// `a_cancelled_follow_stops_without_waiting_for_the_idle_deadline` is
    /// kept anyway, so each test can only reach the arm it names.
    /// `an_idle_give_up_neither_drains_the_pod_nor_finishes_the_run` covers
    /// the third question, which is what the *run* is left in. What this test
    /// still owns on its own is the bound: the call returns at all,
    /// unwrapped, without the pod being marked drained.
    #[tokio::test]
    async fn a_follow_with_no_output_gives_up_within_the_idle_deadline() {
        let client = silent_client();
        let (sink, _stream) = ExecutionStream::channel(8);
        let mut watcher = Watcher {
            workflows: Api::namespaced_with(client.clone(), "ns", &workflow_resource()),
            pods: Api::namespaced(client, "ns"),
            config: ArgoExecutorConfig {
                // A tiny override for this test, not the 600-second
                // production default — see that field's own doc for why the
                // default itself must stay generous.
                log_follow_idle_seconds: 1,
                ..ArgoExecutorConfig::default()
            },
            name: "wf-1".to_owned(),
            sink,
            started: false,
            drained: std::collections::HashSet::new(),
            resume: LogResume::default(),
            cancel: CancellationToken::new(),
        };

        let outcome = watcher.follow("pod-1", "node-1").await;

        assert_eq!(
            outcome,
            FollowOutcome::Incomplete,
            "an idle give-up read part of a log and stopped: it must not report a verdict \
             over the rest (whole-branch review C1), and it must not end the whole \
             observation either, which is what cost this pod's siblings their logs"
        );
        assert!(
            watcher.drained.is_empty(),
            "a pod whose log was never read is not drained, and saying it is is what stops \
             this watch call from retrying it: {:?}",
            watcher.drained
        );
    }

    /// A response body chosen per request path: a complete JSON payload for
    /// the two object reads `Watcher::run` makes, and — for the pod log — a
    /// stream that either never produces a frame or produces one line and
    /// then fails at the transport level.
    ///
    /// One enum rather than three bodies because `kube::Client::new` fixes a
    /// single response-body type for the whole service, and
    /// `kube::client::Body`'s own `wrap_body` is `pub(crate)`, so a
    /// never-completing body cannot be expressed in kube's own type — see
    /// [`silent_client`], which pays the same price by serving `PendingBody`
    /// to *every* path and so cannot serve a workflow read beside it.
    enum ScriptedBody {
        /// One whole payload, then end-of-stream.
        Whole(Option<Bytes>),
        /// Headers arrived, no byte of the body ever will — a wedged
        /// connection, the case the idle deadline exists for.
        Wedged,
        /// One line, then a transport error mid-stream — a reset on a
        /// `follow: true` stream, which is the ordinary way a long-lived log
        /// connection dies.
        LineThenReset { line_sent: bool },
    }

    impl HttpBody for ScriptedBody {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            match self.get_mut() {
                ScriptedBody::Whole(payload) => {
                    Poll::Ready(payload.take().map(|bytes| Ok(Frame::data(bytes))))
                }
                // Never wakes its `Context`, same as [`PendingBody`]: what
                // drives the test forward is `follow`'s own timer.
                ScriptedBody::Wedged => Poll::Pending,
                ScriptedBody::LineThenReset { line_sent } => {
                    if *line_sent {
                        Poll::Ready(Some(Err(std::io::Error::other("connection reset by peer"))))
                    } else {
                        *line_sent = true;
                        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(
                            b"one line before the reset\n",
                        )))))
                    }
                }
            }
        }
    }

    /// A `kube::Client` serving a whole miniature cluster: a terminal
    /// workflow, a `PodList` holding one Running pod, and that pod's log
    /// stream from `log_body`.
    ///
    /// The workflow is **`Succeeded`** deliberately — that is what puts
    /// `Watcher::run` on its `is_terminal` path, which is the only path that
    /// reaches `finish` and so the only one that can emit
    /// `ExecutionEvent::Finished`. A test that used a Running workflow could
    /// not tell "no `Finished` because the observation ended first" apart
    /// from "no `Finished` because the workflow had not ended yet".
    fn one_pod_cluster(log_body: fn() -> ScriptedBody) -> Client {
        scripted_cluster(one_running_pod_list_json, move |_pod| log_body())
    }

    /// [`one_pod_cluster`] generalised over how many pods the `PodList` holds
    /// and which body each pod's log gets — the shape the sibling-starvation
    /// tests need, where one pod wedges and another must still be read.
    ///
    /// `log_body` is called with the pod name taken off the request path, so
    /// a test scripts per pod rather than per request.
    fn scripted_cluster<F>(pod_list: fn() -> Vec<u8>, log_body: F) -> Client
    where
        F: Fn(&str) -> ScriptedBody + Copy + Send + Sync + 'static,
    {
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let path = request.uri().path().to_owned();
            async move {
                // `/log` first: a pod log's path contains `/pods` too.
                let body = if let Some(pod) = pod_in_log_path(&path) {
                    log_body(pod)
                } else if path.contains("/pods") {
                    ScriptedBody::Whole(Some(Bytes::from(pod_list())))
                } else {
                    ScriptedBody::Whole(Some(Bytes::from(succeeded_workflow_json())))
                };
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(200)
                        .body(body)
                        .expect("building a stub response"),
                )
            }
        });
        Client::new(service, "ns")
    }

    /// The pod a `.../pods/<name>/log` request names, or `None` for every
    /// other request `Watcher::run` makes.
    fn pod_in_log_path(path: &str) -> Option<&str> {
        path.strip_suffix("/log")?.rsplit('/').next()
    }

    /// A `Workflow` `DynamicObject`, `status.phase: Succeeded` — terminal, so
    /// `Watcher::run` reaches `finish` unless the drain stopped it first.
    fn succeeded_workflow_json() -> Vec<u8> {
        br#"{"apiVersion":"argoproj.io/v1alpha1","kind":"Workflow",
             "metadata":{"name":"wf-1","namespace":"ns"},
             "status":{"phase":"Succeeded"}}"#
            .to_vec()
    }

    /// A `PodList` holding one pod in a phase `drain_pods` will follow. No
    /// node annotation, so `node_of` falls back to the pod name.
    fn one_running_pod_list_json() -> Vec<u8> {
        br#"{"apiVersion":"v1","kind":"PodList","metadata":{},
             "items":[{"metadata":{"name":"pod-1","namespace":"ns"},
                       "status":{"phase":"Running"}}]}"#
            .to_vec()
    }

    /// A `PodList` holding two pods, `pod-1` before `pod-2` — the shape a
    /// custom plan spanning two repositories produces, and the one the
    /// sibling-starvation tests need. `drain_pods` walks `list.items` in
    /// order, so `pod-1` is always followed first, which is exactly the
    /// stable ordering that made the regression permanent rather than
    /// intermittent.
    fn two_running_pods_list_json() -> Vec<u8> {
        br#"{"apiVersion":"v1","kind":"PodList","metadata":{},
             "items":[{"metadata":{"name":"pod-1","namespace":"ns"},
                       "status":{"phase":"Running"}},
                      {"metadata":{"name":"pod-2","namespace":"ns"},
                       "status":{"phase":"Running"}}]}"#
            .to_vec()
    }

    /// Run one `Watcher` against [`one_pod_cluster`] to completion and report
    /// what it drained and what it emitted.
    ///
    /// Returns the pod names marked drained and every event, in that order,
    /// because the sink has to be dropped before the channel can be read to
    /// end and `drained` lives on the same struct as the sink.
    async fn observe_one_pod_cluster(
        log_body: fn() -> ScriptedBody,
        idle_seconds: u64,
    ) -> (Vec<String>, Vec<ExecutionEvent>) {
        observe_cluster(one_pod_cluster(log_body), idle_seconds).await
    }

    /// [`observe_one_pod_cluster`] over any [`scripted_cluster`], so a
    /// multi-pod test reports the same two things: what was drained, and what
    /// reached the sink.
    async fn observe_cluster(
        client: Client,
        idle_seconds: u64,
    ) -> (Vec<String>, Vec<ExecutionEvent>) {
        let (sink, mut stream) = ExecutionStream::channel(32);
        let mut watcher = Watcher {
            workflows: Api::namespaced_with(client.clone(), "ns", &workflow_resource()),
            pods: Api::namespaced(client, "ns"),
            config: ArgoExecutorConfig {
                // Nothing here should ever reach the between-poll sleep; if
                // it does, the bounding timeout below fails fast instead of
                // the test hanging for an hour.
                status_poll_seconds: 3600,
                log_follow_idle_seconds: idle_seconds,
                ..ArgoExecutorConfig::default()
            },
            name: "wf-1".to_owned(),
            sink,
            started: false,
            drained: std::collections::HashSet::new(),
            resume: LogResume::default(),
            cancel: CancellationToken::new(),
        };

        tokio::time::timeout(Duration::from_secs(20), watcher.run())
            .await
            .expect("run() must return rather than hold this observation open");

        let drained: Vec<String> = watcher.drained.iter().cloned().collect();
        drop(watcher);
        let mut events = Vec::new();
        while let Some(event) = stream.recv().await {
            events.push(event);
        }
        (drained, events)
    }

    /// **A pod that goes quiet past the idle deadline must not take the run's
    /// remaining log and test results with it — Critical, whole-branch review
    /// C1.**
    ///
    /// The give-up used to `break`, which falls through to `emit_results`,
    /// marks the pod drained, and returns `true`; `run` then reached `finish`
    /// and emitted `Finished`. `IngestService::finish` records the terminal
    /// state, the run leaves `active_states()`
    /// (`infra::storage::runs_sea_repo::active_states`, `dispatching |
    /// running`), and `reattach_watchers`' candidate query filters on exactly
    /// that — so there was no re-attach, ever, and every line and every
    /// `=== TEST_CASE: … ===` marker after the gap was permanently absent
    /// while the verdict still came from Argo's workflow phase.
    ///
    /// These two assertions are the ones that pin it, and each fails against
    /// the pre-fix code: `drained` non-empty is what forecloses a retry
    /// within this call, and `Finished` is what forecloses re-attach across
    /// calls.
    #[tokio::test]
    async fn an_idle_give_up_neither_drains_the_pod_nor_finishes_the_run() {
        let (drained, events) = observe_one_pod_cluster(|| ScriptedBody::Wedged, 1).await;

        assert!(
            drained.is_empty(),
            "an idle give-up is not a completed read: marking the pod drained is what stops \
             this watch call from ever retrying it, got {drained:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ExecutionEvent::Finished { .. })),
            "an observation abandoned mid-log must not report a verdict: a `Finished` here \
             retires the run out of `active_states()` and permanently forecloses the \
             re-attach that recovers the rest of the log, got {events:?}"
        );
    }

    /// **The same defect one line up: a mid-stream reset — whole-branch
    /// review I4.**
    ///
    /// A ledger note claimed this branch was "recovered on the next
    /// re-attach". It was not, for exactly the reason above: the recovery it
    /// named cannot happen once `Finished` has retired the run. A reset on a
    /// `follow: true` stream is ordinary, so this was the commoner of the two
    /// paths into the same loss.
    ///
    /// `log_follow_idle_seconds: 3600` so the idle arm provably cannot be
    /// what ends this call — the reset arrives on the second poll of the
    /// body, long before it.
    #[tokio::test]
    async fn a_mid_stream_reset_neither_drains_the_pod_nor_finishes_the_run() {
        let (drained, events) =
            observe_one_pod_cluster(|| ScriptedBody::LineThenReset { line_sent: false }, 3600)
                .await;

        assert!(
            drained.is_empty(),
            "a reset leaves the rest of the log unread; marking the pod drained claims the \
             opposite, got {drained:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ExecutionEvent::Finished { .. })),
            "a truncated read must not report a verdict, got {events:?}"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                ExecutionEvent::Log { line, .. } if line.contains("one line before the reset")
            )),
            "whatever did arrive before the reset is still an observation and must reach \
             the sink, got {events:?}"
        );
    }

    /// Every `Log` line an observation put in the channel, in order — the
    /// sink's own view of what the pass actually captured.
    fn logged_lines(events: &[ExecutionEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| match event {
                ExecutionEvent::Log { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect()
    }

    /// `true` if this observation reported a verdict.
    fn finished(events: &[ExecutionEvent]) -> bool {
        events
            .iter()
            .any(|event| matches!(event, ExecutionEvent::Finished { .. }))
    }

    /// **A pod that gives up must not take its siblings' logs with it —
    /// Critical, the capture regression whole-branch review C1's own fix
    /// introduced.**
    ///
    /// C1 made the idle give-up `return false`, and `drain_pods` reads
    /// `false` as "stop the whole observation", so it returned at `pod-1` and
    /// never followed `pod-2`. `drained` is per-`Watcher` and every re-attach
    /// builds a fresh one, and the `list` order is stable, so that repeated
    /// identically on every pass for the life of the run: `pod-2`'s lines and
    /// every `=== TEST_CASE: … ===` marker in them were never read at all.
    /// Multi-repository custom plans are exactly the shape that produces
    /// sibling pods.
    ///
    /// Red before the fix on its first assertion — `pod-2`'s lines are simply
    /// absent from the channel — and the two assertions after it pin the
    /// halves of C1 that must survive this fix: the wedged pod is still not
    /// drained, and no verdict is reported over its unread log.
    #[tokio::test]
    async fn a_wedged_pod_does_not_starve_the_pods_after_it() {
        let client = scripted_cluster(two_running_pods_list_json, |pod| {
            if pod == "pod-1" {
                ScriptedBody::Wedged
            } else {
                ScriptedBody::Whole(Some(Bytes::from_static(
                    b"pod-2 first line\npod-2 second line\n",
                )))
            }
        });

        let (drained, events) = observe_cluster(client, 1).await;

        let lines = logged_lines(&events);
        assert!(
            lines.contains(&"pod-2 first line") && lines.contains(&"pod-2 second line"),
            "a pod that gave up must not cost the pods after it their logs: `drain_pods` \
             has to walk past it in the same pass, got {lines:?}"
        );
        assert_eq!(
            drained,
            vec!["pod-2".to_owned()],
            "only the pod that reached end-of-file is drained; the wedged one must stay \
             un-drained so a later `Watcher` re-follows it, got {drained:?}"
        );
        assert!(
            !finished(&events),
            "one pod's log is still unread, so this observation must report no verdict at \
             all -- a `Finished` here retires the run out of `active_states()`, got {events:?}"
        );
    }

    /// **A terminal workflow with one pod stopped part-way reports no
    /// verdict.** The guard on the other side of
    /// [`a_wedged_pod_does_not_starve_the_pods_after_it`]: continuing past a
    /// give-up must not turn into finishing over it.
    ///
    /// **Honest about its own colour: this is green against the pre-fix code
    /// too**, because there `drain_pods` returned `false` at `pod-2` and
    /// `run` returned before `finish` for a different reason — the same one
    /// that starved the siblings. What it is red against is the naive
    /// correction (drop the give-up's effect on the return value and let the
    /// pass report success), which is the over-correction this fix has to
    /// avoid. Its `pod-1` assertion also fixes the ordering that matters: the
    /// incomplete pod is the *second* one, so the pass provably ran to the
    /// end before the verdict was withheld.
    #[tokio::test]
    async fn a_terminal_workflow_with_a_pod_stopped_part_way_emits_no_verdict() {
        let client = scripted_cluster(two_running_pods_list_json, |pod| {
            if pod == "pod-1" {
                ScriptedBody::Whole(Some(Bytes::from_static(b"pod-1 only line\n")))
            } else {
                ScriptedBody::Wedged
            }
        });

        let (drained, events) = observe_cluster(client, 1).await;

        assert!(
            !finished(&events),
            "the workflow is `Succeeded`, but `pod-2`'s log was never read to end-of-file: \
             emitting the verdict here is what forecloses the re-attach that would read \
             it, got {events:?}"
        );
        assert_eq!(
            drained,
            vec!["pod-1".to_owned()],
            "the pod that reached end-of-file is drained and the one that stopped part-way \
             is not, got {drained:?}"
        );
        assert!(
            logged_lines(&events).contains(&"pod-1 only line"),
            "whatever was read still reaches the sink; withholding the verdict is not \
             withholding the log, got {events:?}"
        );
    }

    /// **A terminal workflow whose pods all reached end-of-file still
    /// finishes.** The guard against over-correcting into a run that never
    /// reports a verdict at all: `Incomplete` must not be the answer for a
    /// pass in which nothing stopped part-way.
    ///
    /// Green before the fix as well as after it — a pin, not a reproduction.
    /// Its value is that it is the *only* test in this file that requires a
    /// `Finished` to be emitted from a multi-pod workflow, so a fix that made
    /// `drain_pods` pessimistic (say, by treating a skipped already-drained
    /// pod, or the second terminal pass' empty walk, as incomplete) goes red
    /// here rather than silently stranding every run in `active_states()`.
    #[tokio::test]
    async fn a_terminal_workflow_whose_pods_all_reached_eof_still_finishes() {
        let client = scripted_cluster(two_running_pods_list_json, |pod| {
            if pod == "pod-1" {
                ScriptedBody::Whole(Some(Bytes::from_static(b"pod-1 only line\n")))
            } else {
                ScriptedBody::Whole(Some(Bytes::from_static(b"pod-2 only line\n")))
            }
        });

        let (mut drained, events) = observe_cluster(client, 1).await;

        drained.sort();
        assert_eq!(
            drained,
            vec!["pod-1".to_owned(), "pod-2".to_owned()],
            "both logs reached end-of-file, so both pods are drained, got {drained:?}"
        );
        assert!(
            finished(&events),
            "every pod is at end-of-file and the workflow is terminal: this run must report \
             its verdict rather than waiting for a re-attach that has nothing left to read, \
             got {events:?}"
        );
        let lines = logged_lines(&events);
        assert!(
            lines.contains(&"pod-1 only line") && lines.contains(&"pod-2 only line"),
            "both pods' lines must precede the verdict, got {lines:?}"
        );
    }

    /// A response body that yields one line every `interval`, `count` times,
    /// then ends — the shape a healthy, merely slow-cadence pod actually has.
    /// Exists only for
    /// [`a_healthy_intermittent_follow_reaches_eof_with_every_line_emitted`];
    /// see that test's own doc for the regression no all-[`PendingBody`] test
    /// can catch.
    struct IntermittentBody {
        remaining: usize,
        interval: Duration,
        line: &'static str,
        timer: Option<Pin<Box<tokio::time::Sleep>>>,
    }

    impl IntermittentBody {
        fn new(count: usize, interval: Duration, line: &'static str) -> Self {
            Self {
                remaining: count,
                interval,
                line,
                timer: None,
            }
        }
    }

    impl HttpBody for IntermittentBody {
        type Data = Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            // `IntermittentBody` has no self-referential fields (`timer` is a
            // heap-pinned `Sleep`, itself `Unpin` regardless of what it
            // pins), so getting a plain `&mut` out of the `Pin` is sound.
            let this = self.get_mut();
            if this.remaining == 0 {
                return Poll::Ready(None);
            }
            let interval = this.interval;
            let timer = this
                .timer
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(interval)));
            match timer.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(()) => {
                    this.remaining -= 1;
                    this.timer = None;
                    Poll::Ready(Some(Ok(Frame::data(Bytes::from(format!(
                        "{}\n",
                        this.line
                    ))))))
                }
            }
        }
    }

    /// A `kube::Client` whose response body emits `count` lines, `interval`
    /// apart, then ends — [`silent_client`]'s opposite: this one always
    /// produces something, eventually, and never stalls.
    fn intermittent_client(count: usize, interval: Duration, line: &'static str) -> Client {
        let service = tower::service_fn(
            move |_request: http::Request<kube::client::Body>| async move {
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(200)
                        .body(IntermittentBody::new(count, interval, line))
                        .expect("building a stub response with an intermittent body"),
                )
            },
        );
        Client::new(service, "ns")
    }

    /// A `kube::Client` whose every request's response future never resolves
    /// at all — modelling a connection stuck before its headers even arrive,
    /// unlike [`silent_client`] (which answers with headers instantly and
    /// only the *body* never completes). Used to put `Watcher::run` to sleep
    /// **inside its own `get_opt` await**, rather than its between-poll
    /// sleep, which is a different `select!` arm than
    /// [`a_cancelled_watcher_returns_from_run_promptly`] exercises.
    fn never_responding_client() -> Client {
        let service = tower::service_fn(
            move |_request: http::Request<kube::client::Body>| async move {
                std::future::pending::<
                    Result<http::Response<kube::client::Body>, std::convert::Infallible>,
                >()
                .await
            },
        );
        Client::new(service, "ns")
    }

    /// **The property that actually distinguishes an idle bound from a total
    /// one, pinned directly — fix round 1, Important #1.**
    ///
    /// `a_follow_with_no_output_gives_up_within_the_idle_deadline` only proves
    /// `follow` does not hang forever; its body ([`PendingBody`]) never
    /// produces a single frame, so it cannot tell an idle bound apart from a
    /// bound on the *whole loop* — under `tokio::time::timeout(idle,
    /// whole_loop)` that test would still pass at exactly the same 1 s. That
    /// is precisely the regression `ArgoExecutorConfig::log_follow_idle_seconds`'s
    /// own doc and `LineSkip`'s six fix rounds on this same file both exist to
    /// prevent, and nothing in the suite would have gone red if a future edit
    /// collapsed the per-iteration timeout into one around the loop.
    ///
    /// This body ([`IntermittentBody`]) emits a line every 300 ms, ten times
    /// (3 s total) — comfortably longer than the 1 s idle deadline this test
    /// configures, but never silent for longer than 300 ms at a stretch.
    /// Under an **idle** bound each line resets the clock, so the follow
    /// reaches end-of-file and every line is emitted; under a bound on the
    /// whole loop, this would be killed well before the third line. Green
    /// today; would go red under a `timeout(idle, async { loop { ... } })`
    /// rewrite that collapsed the two.
    #[tokio::test]
    async fn a_healthy_intermittent_follow_reaches_eof_with_every_line_emitted() {
        const LINES: usize = 10;
        let client = intermittent_client(LINES, Duration::from_millis(300), "chatty line");
        let (sink, mut stream) = ExecutionStream::channel(32);
        let watcher_config = ArgoExecutorConfig {
            log_follow_idle_seconds: 1,
            ..ArgoExecutorConfig::default()
        };
        let mut watcher = Watcher {
            workflows: Api::namespaced_with(client.clone(), "ns", &workflow_resource()),
            pods: Api::namespaced(client, "ns"),
            config: watcher_config,
            name: "wf-1".to_owned(),
            sink,
            started: false,
            drained: std::collections::HashSet::new(),
            resume: LogResume::default(),
            cancel: CancellationToken::new(),
        };

        let reached_eof =
            tokio::time::timeout(Duration::from_secs(10), watcher.follow("pod-1", "node-1"))
                .await
                .expect(
                    "a healthy pod that is merely chatty at a slow cadence must not be killed by \
             the idle bound at all -- if this elapses, the idle-vs-total distinction has \
             been lost",
                );
        assert_eq!(
            reached_eof,
            FollowOutcome::Complete,
            "reaching end-of-file is neither the observer going away nor a part-way stop; \
             it is the one exit that both drains the pod and releases the verdict"
        );

        drop(watcher);
        let mut emitted = Vec::new();
        while let Some(event) = stream.recv().await {
            if let ExecutionEvent::Log { line, .. } = event {
                emitted.push(line);
            }
        }
        assert_eq!(
            emitted.len(),
            LINES,
            "an idle bound resets on every line and must never cut a healthy, merely-slow \
             follow short: got {emitted:?}"
        );
    }

    /// **`follow`'s own cancel arm, exercised directly — fix round 1,
    /// Important #3.** `a_follow_with_no_output_gives_up_within_the_idle_
    /// deadline` never cancels its token, so it cannot tell `follow`'s
    /// cancelled arm apart from its idle-timeout arm — reverting the
    /// cancelled arm alone still leaves that test green, because the idle
    /// deadline it configures is what actually ends the call.
    ///
    /// `log_follow_idle_seconds: 3600` is deliberate, mirroring
    /// `a_cancelled_watcher_returns_from_run_promptly`'s own
    /// `status_poll_seconds: 3600`: absent the cancel arm, this call would
    /// still be waiting out the idle deadline an hour later, so the bounding
    /// `timeout` below turns a reverted arm into a fast, clean FAIL rather
    /// than an hour-long hang.
    ///
    /// Asserts [`FollowOutcome::Stop`]: cancellation ends the whole
    /// observation, no sibling pod after this one is followed, and nothing
    /// further is emitted — see [`Watcher::follow`]'s own doc on the three
    /// give-up arms' different blast radius. That is what separates it from
    /// the idle arm's `Incomplete`, which stops this pod only.
    #[tokio::test]
    async fn a_cancelled_follow_stops_without_waiting_for_the_idle_deadline() {
        let client = silent_client();
        let (sink, _stream) = ExecutionStream::channel(8);
        let cancel = CancellationToken::new();
        let mut watcher = Watcher {
            workflows: Api::namespaced_with(client.clone(), "ns", &workflow_resource()),
            pods: Api::namespaced(client, "ns"),
            config: ArgoExecutorConfig {
                log_follow_idle_seconds: 3600,
                ..ArgoExecutorConfig::default()
            },
            name: "wf-1".to_owned(),
            sink,
            started: false,
            drained: std::collections::HashSet::new(),
            resume: LogResume::default(),
            cancel: cancel.clone(),
        };

        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            canceller.cancel();
        });

        let outcome =
            tokio::time::timeout(Duration::from_secs(5), watcher.follow("pod-1", "node-1"))
                .await
                .expect(
                    "follow() must return promptly once cancelled, not wait out the idle deadline",
                );

        assert_eq!(
            outcome,
            FollowOutcome::Stop,
            "cancellation must stop the whole observation, the same as the observer going \
             away -- not merely this pod, which is the idle arm's `Incomplete`. The \
             `log_follow_idle_seconds: 3600` above is what makes the idle arm unreachable \
             within this test's own timeout, so this answer can only have come from the \
             cancel arm"
        );
    }

    /// **`run`'s cancel arm around its own status read, exercised directly —
    /// fix round 1, Important #3.** `a_cancelled_watcher_returns_from_run_
    /// promptly`'s client answers `get_opt` instantly, so cancellation there
    /// can only ever be observed at the between-poll sleep — reverting that
    /// arm alone still leaves that test green, because the sleep's own arm is
    /// what actually ends the call. This one uses [`never_responding_client`]
    /// so `run` is genuinely blocked inside `get_opt` when cancellation
    /// fires, exercising the other arm.
    #[tokio::test]
    async fn a_cancelled_watcher_stuck_in_the_status_read_still_returns_promptly() {
        let client = never_responding_client();
        let (sink, _stream) = ExecutionStream::channel(8);
        let cancel = CancellationToken::new();
        let mut watcher = Watcher {
            workflows: Api::namespaced_with(client.clone(), "ns", &workflow_resource()),
            pods: Api::namespaced(client, "ns"),
            config: ArgoExecutorConfig::default(),
            name: "wf-1".to_owned(),
            sink,
            started: false,
            drained: std::collections::HashSet::new(),
            resume: LogResume::default(),
            cancel: cancel.clone(),
        };

        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            canceller.cancel();
        });

        tokio::time::timeout(Duration::from_secs(5), watcher.run())
            .await
            .expect(
                "run() must return promptly even while its status read never completes on \
                 its own",
            );
    }
}
