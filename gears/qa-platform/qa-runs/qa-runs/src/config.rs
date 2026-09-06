//! Typed configuration for the `qa-runs` gear (YAML section `qa-runs`).
//!
//! # Where each knob goes
//!
//! `crate::gear`'s `init` deserializes this once (`ctx.config_or_default()`)
//! and distributes every value; nothing else reads it.
//!
//! | Knob | Consumer |
//! |---|---|
//! | `dispatcher_enabled`, `dispatcher_interval_seconds` | `Cadence::dispatcher`, stored on the runtime and read by `serve` |
//! | `scheduler_enabled`, `schedule_interval_seconds` | `Cadence::scheduler`, likewise |
//! | `orphan_timeout_seconds` | `ServiceDeps::orphan_timeout_seconds` - the claim reconciler and boot recovery |
//! | `queue_max_depth`, `max_concurrent_runs`, `queue_ttl_seconds` | `domain::service::QueueLimits` |
//! | `default_timeout_seconds` | `ServiceDeps::default_timeout_seconds` |
//! | `max_timeout_seconds` | [`crate::api::rest::dto::BoundaryLimits`], via [`From`], layered on the router |
//! | `log_buffer_lines` | `RunLogBroadcaster::new`'s capacity |
//! | `executor`, `argo` | `crate::gear`'s executor wiring - see [`ExecutorKind`] |
//!
//! **Corrected 2026-08-15, twice in one day and in opposite directions.** This
//! header first claimed in the present tense that every knob "is consumed by
//! something" while the bootstrap did not exist; the correction replaced it
//! with "Nothing reads this yet" and a table reading "no" on every row - and
//! then the bootstrap landed and made *that* false. The lesson worth keeping is
//! not about tense: a header describing wiring in another file goes stale
//! whenever that file changes, and neither version was wrong when written.
//!
//! # Where the floors are applied
//!
//! The cadence knobs have floors, and this module owns both the floor
//! constants and the `effective_*` accessors that apply them
//! ([`QaRunsConfig::effective_dispatcher_interval_seconds`],
//! [`QaRunsConfig::effective_schedule_interval_seconds`],
//! [`QaRunsConfig::effective_orphan_timeout_seconds`]). qa-catalog puts the
//! equivalent helper in its `gear.rs`
//! (`qa-catalog/src/gear.rs`, `effective_branch_refresh_interval`); this crate
//! puts it here instead so the knob and the rule that bounds it cannot be read
//! apart, and so the rule is unit-testable without constructing a gear.
//!
//! **The accessors are the contract, not the fields.** The gear bootstrap must
//! read each cadence through its `effective_*` accessor **once at init** and
//! store the result, so the serve loop never sees a raw value - and so the
//! clamp warning is emitted once rather than on every tick. That second reason
//! is a caller obligation, not something these accessors enforce; see
//! [`QaRunsConfig::effective_dispatcher_interval_seconds`].

use serde::Deserialize;
use tracing::warn;

use crate::domain::timeout::MAX_LAUNCH_TIMEOUT_SECONDS;
use crate::infra::logs::DEFAULT_LOG_CHANNEL_CAPACITY;

/// Floor for [`QaRunsConfig::dispatcher_interval_seconds`] when the dispatcher
/// is enabled.
///
/// This is the source system's own floor promoted to a constant
/// (`manager/src/services/run_dispatcher.rs:313`, `interval_seconds.max(5)`).
/// Each tick performs cross-tenant reads plus one executor listing, so a
/// one-second ticker turns the gear into a load generator against its own
/// database and the execution plane. `0` still disables the ticker outright.
pub const MIN_DISPATCHER_INTERVAL_SECONDS: u64 = 5;

/// Floor for [`QaRunsConfig::schedule_interval_seconds`] when the scheduler is
/// enabled.
///
/// **Not the dispatcher's floor, and higher on purpose.** A cron expression's
/// finest resolution is one minute (`domain::cron`, five fields, no seconds
/// field), so a sub-minute tick can only ever re-evaluate the same due time it
/// already fired. Every extra pass costs a cross-tenant enumeration of every
/// schedule in the fleet — `SchedulesRepository::list_enabled` is deliberately
/// uncapped and unwindowed — and buys, at most, seconds of latency against a
/// value that changes once a minute.
///
/// Ten rather than sixty so a deployment that wants a due time acted on
/// promptly can still ask for it: the floor's job is to keep a misconfiguration
/// from turning the ticker into a load generator, not to pick the cadence.
/// `0` still disables the ticker outright.
pub const MIN_SCHEDULE_INTERVAL_SECONDS: u64 = 10;

/// Floor for [`QaRunsConfig::orphan_timeout_seconds`].
///
/// A claim sits in `dispatching` with no execution reference for the whole
/// force-sync plus bundle-build window, which is minutes. Failing it early
/// abandons a launch that is still in progress and momentarily releases its
/// claim, which is exactly the window in which a second run can be admitted
/// beside an exclusive one. Sixty seconds is not a *safe* value, it is the
/// lowest value that is not obviously broken; the shipped default is an order
/// of magnitude above it.
pub const MIN_ORPHAN_TIMEOUT_SECONDS: u64 = 60;

/// Typed configuration for the qa-runs gear.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QaRunsConfig {
    /// Whether the dispatcher ticker runs at all. Default `true`.
    ///
    /// Exists so an operator can disable the tick deliberately rather than by
    /// accident: a deployment whose policy plugin cannot grant this gear a
    /// system actor makes the tick inert anyway, and it then logs one
    /// actionable WARN per pass on every tick forever
    /// (`domain::service::dispatch`, `TickReport::note_failure`). The knob is
    /// what turns that from noise into a decision.
    ///
    /// # Two knobs can disable the ticker, and this one wins
    ///
    /// `dispatcher_interval_seconds: 0` also stops it. Either alone disables;
    /// **`dispatcher_enabled: false` is decisive regardless of the interval**,
    /// and the composite answer is [`Self::dispatcher_runs`] so the rule has one
    /// home rather than being re-derived by the bootstrap.
    ///
    /// The asymmetry the pair is worth keeping for: `enabled: false` says *this
    /// deployment does not run a dispatcher*, which is a statement about the
    /// role of the replica, while `interval: 0` is a cadence that happens to
    /// mean never. An operator turning the tick off for a shift wants the first
    /// and should not have to remember what the interval was.
    pub dispatcher_enabled: bool,

    /// Dispatcher tick interval, seconds. `0` disables the dispatcher.
    ///
    /// **Default 5, not the source system's 15** (user decision, 2026-08-14).
    /// `cpt-cf-qa-nfr-dispatch-latency` requires a queued run to start within
    /// 10 s p95 of its platform freeing; a 15 s sweep gives p95 around 14 s and
    /// cannot meet it, and the release-notification wake that the source
    /// system's design paired with its 15 s sweep is not built in this port.
    ///
    /// The source system's rationale for 15 is sound and does not apply here:
    /// it argues the common case, where an admissible run dispatches inline so
    /// the tick only ever delays already-queued runs
    /// (`manager/src/services/run_dispatcher.rs:306-311`). The NFR measures
    /// exactly that queued case.
    ///
    /// Clamped by [`Self::effective_dispatcher_interval_seconds`], never read
    /// raw by the ticker.
    pub dispatcher_interval_seconds: u64,

    /// Whether the schedule firing ticker runs at all. Default `true`.
    ///
    /// The scheduler's counterpart of [`Self::dispatcher_enabled`], with the
    /// same precedence rule and for the same reasons — see that field. The
    /// composite answer is [`Self::scheduler_runs`].
    ///
    /// **Independent of the dispatcher's knob, deliberately.** A deployment can
    /// legitimately want one and not the other: turning the scheduler off stops
    /// new scheduled runs being *created* while the dispatcher keeps draining
    /// what is already queued, which is what an operator freezing a fleet
    /// before a maintenance window wants. Folding them into one switch would
    /// have made that unexpressible.
    ///
    /// **Switching it off is not how a multi-replica deployment avoids double
    /// firing.** The tick claim is what does that; see
    /// `crate::domain::service::schedules`.
    pub scheduler_enabled: bool,

    /// Schedule evaluation interval, seconds. `0` disables the scheduler.
    ///
    /// Clamped by [`Self::effective_schedule_interval_seconds`], never read raw
    /// by the ticker. See [`MIN_SCHEDULE_INTERVAL_SECONDS`] for why its floor is
    /// not the dispatcher's.
    ///
    /// The default matches a cron expression's own resolution: a five-field
    /// expression cannot name anything finer than a minute, so a schedule due at
    /// 03:00 fires within a tick of it.
    pub schedule_interval_seconds: u64,

    /// Seconds a queue row may sit in `dispatching` with no execution
    /// reference before a tick fails it.
    ///
    /// Must comfortably exceed a force-sync plus a bundle build - minutes, not
    /// seconds. Clamped by [`Self::effective_orphan_timeout_seconds`]. See
    /// `crate::domain::state_machine::reconcile_claim`, which owns the rule
    /// this value parameterises.
    pub orphan_timeout_seconds: u64,

    /// Queue TTL, seconds. `0` disables expiry.
    ///
    /// `0`-means-disabled is the frozen guide's convention for this knob and
    /// for the two below it; `crate::domain::queue::expiry_cutoff` implements
    /// the reading.
    pub queue_ttl_seconds: u64,

    /// Per-platform queue depth. `0` = unlimited.
    ///
    /// Enforced per (access scope, platform) rather than per platform - see
    /// [`DomainError::QueueFull`](crate::domain::error::DomainError::QueueFull).
    pub queue_max_depth: u32,

    /// Cluster-wide concurrent-run cap.
    ///
    /// **Default is 50, not 0 — derived, not guessed.** `cpt-cf-qa-nfr-scale`
    /// (`docs/PRD.md`, "Scale envelope") is a `p1` requirement that this
    /// subsystem MUST handle "50 concurrently executing runs". Fifty is
    /// therefore not a number picked because it looked safe; it is the
    /// number the gear is already committed to sustaining, so admitting up to
    /// it and refusing beyond it asks nothing of a deployment the NFR did not
    /// already ask for.
    ///
    /// `0` **remains accepted as an explicit "unbounded" opt-out** — the
    /// frozen guide's convention for this knob (guide lines 116-120) and the
    /// behaviour every deployment got before this default changed.
    ///
    /// **Whole-branch review I1: that opt-out was, until this wave, not
    /// reachable in the shipped image**, and this paragraph said "sets
    /// `max_concurrent_runs: 0` and keeps it" without naming anywhere it could
    /// be set. `gears/qa-platform/config/qa-platform-stack.yaml` had no such
    /// key; the Helm chart embeds that file **verbatim** (`.Files.Get` in
    /// `gears-config-configmap.yaml`, byte-pinned by
    /// `deploy/helm/tests/test_chart_file_sync.py`), and the file's own comment
    /// says it is baked into the image and cannot be edited per deployment. It
    /// is now `--set qaRuns.maxConcurrentRuns=0` on the chart, rendered into
    /// the fragment `entrypoint.sh` inserts at `QA_RUNS_ARGO_ANCHOR`, which the
    /// chart sets unconditionally.
    ///
    /// **`${QA_RUNS_MAX_CONCURRENT_RUNS:-50}` in that file would not have
    /// worked, and this was checked rather than reasoned about.** `${VAR}`
    /// expansion is opt-in per config struct and runs *after* serde, on
    /// `String` fields marked `#[expand_vars]`
    /// (`libs/toolkit/src/context.rs`, `config_expanded_or_default`).
    /// [`QaRunsConfig`] derives no `ExpandVars`, `gear.rs` loads through plain
    /// `ctx.config_or_default()`, and this field is a `u32` — so a placeholder
    /// there would fail to deserialize the whole `qa-runs` section rather than
    /// expand. `entrypoint.sh` does targeted `sed` on named anchors, not an
    /// envsubst pass, so it does not reach it either. Making the placeholder
    /// route work means retyping this knob as a `String` and moving the gear
    /// onto the expanded loader — a change to how the gear reads *all* of its
    /// config, for one knob, and not this wave's.
    ///
    /// **This is a deployment-visible behaviour change, and the 429 is the
    /// smaller half of it.** A deployment that leaves this knob unset moves
    /// from "never rejected for concurrency" to "429 at launch past 50
    /// concurrent runs" — but enabling the cap by default also puts a
    /// cross-plane `executor.list_active()` call on the front of **every**
    /// launch (`admission::GlobalCapGate::reserve`), where a disabled cap
    /// (`max == 0`) short-circuits before that call is ever made. And that
    /// call's failure direction is the strict one:
    /// `AdmissionService::enforce_global_cap`'s own doc states the asymmetry
    /// against the depth limit — over-committing is the worse outcome for
    /// this cap, so **an unreadable executor now fails the launch**, where a
    /// disabled cap or the depth limit alone would have let it through. In
    /// concrete terms: a launch that today succeeds during an Argo outage can,
    /// after this default takes effect, fail with a 500 instead. Also worth
    /// stating plainly: the cap **refuses rather than queues** a burst past
    /// the limit, so it does not buy back the queue as a buffer the way
    /// `queue_max_depth` does — a deployment that saw bursts absorbed by the
    /// queue before will see some of them rejected outright now. All of this
    /// is the ruled trade-off, not a defect — a concurrency cap that cannot be
    /// evaluated should fail closed, and the alternative is the gear staying
    /// unbounded — but it belongs here rather than being discovered later.
    ///
    /// The knob is also the only one that transitively bounds how many result
    /// observers a process can hold open at once — see
    /// [`crate::domain::service::watch::SpawningRunWatcher`]'s header, "The
    /// number of observers is bounded transitively, by admission — not by
    /// this registry" — and it is the only knob that bounds how many claims
    /// can exist at once, which is what `QueueRepository::all_claims`' scan
    /// window has to cope with when the operator opts back into `0`.
    pub max_concurrent_runs: u32,

    /// Fallback run timeout when neither the launch request nor the plan
    /// supplies one. Honoured only when non-zero
    /// (`manager/src/services/argo.rs:168-181`).
    pub default_timeout_seconds: u64,

    /// Ceiling on any resolved timeout, so a plan cannot pin a platform
    /// indefinitely (`cpt-cf-qa-fr-runs-timeout`'s "configurable default
    /// ceiling").
    ///
    /// Enforced at the REST boundary, which rejects an out-of-range
    /// `timeout_seconds` with a 400 naming the ceiling rather than silently
    /// clamping it. Read through [`Self::effective_max_timeout_seconds`], never
    /// raw: this knob and `domain::timeout`'s hard fail-safe are two ceilings
    /// and that accessor is where they are reconciled.
    pub max_timeout_seconds: u64,

    /// SSE log fan-out buffer per run, in lines.
    ///
    /// Bounded on purpose: this is a per-run in-memory broadcast channel, and
    /// a subscriber that falls this far behind is served a gap marker rather
    /// than being allowed to grow the buffer.
    ///
    /// **It bounds a line count, not bytes.** `RunLogBroadcaster`'s own doc
    /// carries the measurement: at this capacity the resident buffer is
    /// `log_buffer_lines x` the longest line the executor emits, and the byte
    /// cap belongs to the read side - `api::rest::sse::MAX_LINE_BYTES`.
    ///
    /// The default is [`DEFAULT_LOG_CHANNEL_CAPACITY`] itself rather than a
    /// second number. **Deviation from the plan, which specified 1024**, and the
    /// reason is the one that produced
    /// [`Self::effective_max_timeout_seconds`] one field up: two independent
    /// defaults for one quantity disagree the moment either moves, and a
    /// deployment that set neither would get 1024 or 256 depending on whether
    /// the gear bootstrap remembered to pass this through. The constant is also
    /// the only one of the two with a derivation attached to it.
    pub log_buffer_lines: usize,

    /// Which [`RunExecutor`](crate::domain::ports::run_executor::RunExecutor)
    /// adapter the gear wires. Default [`ExecutorKind::Mock`].
    ///
    /// **The default must stay `Mock`**, per ADR-0001's 2026-08-27 waiver: "the
    /// mock stays the default executor, so no deployment gains a Kubernetes
    /// dependency by upgrading". Selecting [`ExecutorKind::Argo`] in a binary
    /// built without the `argo` cargo feature is a hard boot failure rather than
    /// a silent fall back to the mock — a deployment that asked for real tests
    /// and got `test_mock_default` would report fabricated passes, which is the
    /// exact failure the adapter exists to end.
    pub executor: ExecutorKind,

    /// Settings for the Argo adapter. Inert unless
    /// [`Self::executor`] is [`ExecutorKind::Argo`].
    ///
    /// Parsed unconditionally — in a build **without** the `argo` feature too —
    /// so one configuration file is valid against both builds. Nothing in this
    /// struct names a Kubernetes type; it is strings and numbers.
    pub argo: ArgoExecutorConfig,
}

/// Which execution-plane adapter to wire.
///
/// A config enum rather than a `#[cfg]` at the wiring site, so one image can be
/// pointed at either backend and a deployment does not have to be rebuilt to
/// switch. A GTS plugin is the eventual home for a second backend and is
/// deliberately premature here — ADR-0001: "The port stays internal (not a
/// public plugin) until a second real backend exists".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    /// The deterministic in-memory adapter
    /// ([`crate::infra::executor::mock::MockRunExecutor`]). Every run reports
    /// one fabricated passing test.
    #[default]
    Mock,
    /// Argo Workflows on Kubernetes
    /// (`crate::infra::executor::argo::ArgoRunExecutor`). Requires the `argo`
    /// cargo feature; see [`QaRunsConfig::executor`].
    Argo,
}

/// Deployment settings for the Argo adapter.
///
/// Everything here is an execution-plane deployment detail the port
/// deliberately does not carry (`run_executor.rs:120-123`): the namespace, the
/// runner image, the retention window, and how this process authenticates to
/// the API server.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArgoExecutorConfig {
    /// Namespace the `Workflow` objects are created in. Default `argo`,
    /// matching the source system (`manager/src/main.rs:61`).
    pub namespace: String,

    /// Path to a kubeconfig file. `None` uses the ambient configuration —
    /// in-cluster service-account credentials, or `KUBECONFIG`/`~/.kube/config`
    /// — which is `kube::Client::try_default`'s own order and what the source
    /// system relies on (`manager/src/main.rs:57`).
    ///
    /// # Why this knob exists at all
    ///
    /// The dev deployment runs the gears as Docker containers on a k3s host,
    /// where the host kubeconfig names `127.0.0.1:6443` — the *container's* own
    /// loopback, not the API server. Such a deployment needs a rewritten copy,
    /// mounted, and named here.
    pub kubeconfig_path: Option<String>,

    /// Container image the runner pods use. No default that could work
    /// anywhere: an unset image is a boot failure rather than a workflow that
    /// fails on `ImagePullBackOff` minutes later.
    pub runner_image: String,

    /// Entrypoint for [`Self::runner_image`]. Default `["/entrypoint.sh"]`,
    /// the source system's (`manager/src/services/argo.rs:528`).
    ///
    /// Configurable because the image is: an adapter that hardwired one
    /// entrypoint could only ever be exercised against one image, and the
    /// first cut of this adapter was verified against a marker-emitting shell
    /// canary before the pytest runner image existed anywhere the cluster
    /// could pull it.
    pub runner_command: Vec<String>,

    /// `imagePullPolicy` for the runner container. Default `IfNotPresent`,
    /// the source system's (`argo.rs:527`).
    pub image_pull_policy: String,

    /// `ttlStrategy.secondsAfterCompletion` on every submitted workflow —
    /// how long a finished workflow, and its pods and their logs, survive.
    /// Default 3600, the source system's fallback (`argo.rs:184`).
    ///
    /// # This is the one number that can lose results
    ///
    /// Argo garbage-collects the workflow and its pods when it expires, and
    /// the pod log is the only place per-test results exist
    /// (there is no live progress endpoint here — `domain::service::ingest`
    /// records why). Once collected, `watch` correctly yields an empty stream
    /// and the results are gone. Keep this comfortably above the dispatcher
    /// interval and the timeout sweep so a re-attach after a control-plane
    /// restart still finds something to read.
    pub workflow_ttl_seconds: u64,

    /// How often `watch` re-reads a workflow's status, in seconds. Default 3.
    ///
    /// Status is polled rather than watched. Pod logs are *streamed*
    /// (`follow: true`), so this cadence does not bound log latency and
    /// `cpt-cf-qa-nfr-log-latency` is unaffected by it; it bounds only how
    /// promptly a terminal phase is noticed after the logs have drained.
    pub status_poll_seconds: u64,

    /// Base URL a **workflow pod** uses to reach the gears' HTTP API, for
    /// downloading a node's test bundle. `None` submits no `TEST_BUNDLE_URL`.
    ///
    /// # Not wired end to end, and the reason is not reachability
    ///
    /// [`ExecutionNode::bundle_ref`](crate::domain::ports::run_executor::ExecutionNode::bundle_ref)
    /// is qa-catalog's `TestBundle::storage_ref`, which its only bundle store
    /// spells as an absolute path on qa-catalog's *own* filesystem
    /// (`qa-catalog/.../infra/bundle_store/local_fs.rs:3-4`) — a path no pod
    /// can read. The store writes `<uuid>.tar.gz`, so the bundle id is
    /// recoverable from the basename and a URL can be derived; that is what
    /// this knob does. What is **not** solved is authentication:
    /// `GET /qa/v1/test-bundles/{id}` is `.authenticated()`
    /// (`qa-catalog/.../api/rest/routes/bundles.rs:31`) where the source
    /// system's equivalent had no middleware at all, and nothing in this
    /// deployment specifies how a pod obtains a credential. See the adapter's
    /// module docs for the recommendation and why `s2s_oauth` as configured is
    /// not it.
    pub bundle_base_url: Option<String>,

    /// `spec.serviceAccountName` for the submitted workflow — the identity the
    /// **runner pod** runs as. `None` leaves it unset, so the namespace's
    /// `default` service account is used.
    ///
    /// # Unset is usually wrong, and the failure is confusing
    ///
    /// Argo's executor writes a `workflowtaskresults` object per step, so the
    /// pod's own service account needs `create` on
    /// `workflowtaskresults.argoproj.io`. The `default` account does not have
    /// it, and the run fails **after** producing all of its output, with
    /// `exit code 64` and `workflowtaskresults.argoproj.io is forbidden` in
    /// `status.message` — measured on the dev cluster, 2026-08-27, which is why
    /// this knob exists. The source system never met it because
    /// `quick-start-minimal.yaml` patches the default account; the
    /// `argo-workflows` Helm chart instead creates a named one
    /// (`argo-workflow`, with `workflow.serviceAccount.create=true`).
    pub workflow_service_account: Option<String>,

    /// How a **workflow pod** obtains a bearer token for
    /// [`Self::bundle_base_url`]. `None` submits no credential variables, and a
    /// pod fetching the bundle URL then gets a 401.
    ///
    /// # Why this is config and not part of `RunSpec`
    ///
    /// The credential is a property of the *deployment's* identity provider,
    /// not of a run: every run in a deployment presents the same service
    /// account. `RunSpec` carries what qa-catalog and qa-environments know
    /// about a run, and an `IdP` client id is neither.
    ///
    /// **The adapter never resolves the client secret.** It emits a
    /// `secretKeyRef` naming a Kubernetes `Secret` that somebody else
    /// pre-provisioned, and the kubelet resolves it — the same mechanism, and
    /// the same reason, as [`Self::secret_name_prefix`] below.
    pub bundle_auth: Option<BundleAuthConfig>,

    /// Kubernetes `Secret` name prefix used when a
    /// [`SecretRef`](crate::domain::ports::run_executor::SecretRef) has to be
    /// turned into a `secretKeyRef` or a secret volume. Default
    /// `qa-platform-`.
    ///
    /// **The adapter never resolves a reference.** It derives a Secret name
    /// from the reference and lets the kubelet resolve it, which is how the
    /// source system does it (`argo.rs:504-521`) and what keeps
    /// `run_executor.rs:85-86` — "qa-runs never reads a secret's contents" —
    /// literally true of this adapter. Materialising those Secrets is somebody
    /// else's job and is not done today; see the adapter's module docs (D4).
    pub secret_name_prefix: String,

    /// Key inside the derived `Secret` that carries the material. Default
    /// `value`.
    ///
    /// One key for both shapes, unlike the source system's two (`token` for
    /// the `ReportPortal` key, `kubeconfig` for a platform's kubeconfig): the
    /// port's [`SecretRef`](crate::domain::ports::run_executor::SecretRef) is
    /// one opaque string with no key component, so there is nothing to derive
    /// a per-reference key from.
    pub secret_key: String,

    /// How long a pod-log follow (`infra::executor::argo::watch::Watcher::follow`)
    /// may go **without a single line** before giving up on that pod. Default
    /// 600 (10 minutes). Review findings #20/#21: `follow: true` held a task
    /// forever on a wedged API-server connection, because nothing bounded it.
    /// Exposed to operators in the Helm chart (`argo.logFollowIdleSeconds`,
    /// `gears-argo-configmaps.yaml`), beside its neighbours below.
    ///
    /// # This is an idle bound, not a lifetime bound — and the difference is the
    /// # whole point
    ///
    /// `follow: true` on a *healthy* run is meant to stay open for as long as
    /// the node runs, and a single long test within that run can legitimately
    /// produce no log line for a while (a slow fixture, a network fetch, a
    /// test that just doesn't print). A deadline on the *whole* follow would
    /// cut that off mid-run and lose every line after it — trading the hang
    /// this field fixes for a log-loss bug wearing its clothes, which is
    /// exactly what the preceding three tasks on this file spent six fix
    /// rounds preventing (see `LineSkip`'s own doc). So this resets on every
    /// line: a fresh line resets the clock, and only a stretch of true
    /// silence this long trips it.
    ///
    /// # Where 600 comes from, and why not `cpt-cf-qa-nfr-run-duration`'s eight hours
    ///
    /// **Fix round 1 corrected this derivation; read this section as the
    /// correction, not the original reasoning.** The shipped version reused
    /// `api::rest::sse::MAX_STREAM_DURATION`'s eight hours, on the argument
    /// that `cpt-cf-qa-nfr-run-duration`'s longest contemplated run cannot
    /// legitimately go silent longer than the run itself. That is true and
    /// still the wrong bound for what this field gates: `MAX_STREAM_DURATION`
    /// justifies itself as a *total* ceiling on a client-facing SSE
    /// connection, and this crate's own numbers elsewhere never treat eight
    /// hours as "how long is silence still plausible" — `default_timeout_seconds:
    /// 3600`, `max_timeout_seconds: 86_400`, and
    /// `domain::timeout::kind_timeout_fallback`'s per-kind defaults
    /// (300/600/1800/3600) all put the *typical* run, and the *typical single
    /// step within it*, at far less. Reusing the SSE constant's digits
    /// imported its number without its reasoning.
    ///
    /// **The decisive problem: `run`'s whole loop is blocked for the entire
    /// idle wait, not just this one pod.** `follow` is called from
    /// `drain_pods`, synchronously, inside `run`'s own loop — never spawned
    /// concurrently — and `drain_pods` iterates pods one at a time (this
    /// module's own "Known limitation" header). So one wedged pod does not
    /// merely leak a task: for as long as this field allows, it holds up
    /// this run's `Finished` event *and every sibling pod's own follow*. Eight
    /// hours here was not a generous safety margin, it was an eight-hour
    /// stall on the whole observation, not a leaked task — a residual this
    /// field's very first version underweighted.
    ///
    /// The right anchor is **the longest silence still plausible *inside* one
    /// run**, not the longest run. A pod that is genuinely still executing has
    /// its own silence already bounded by Argo's `activeDeadlineSeconds`
    /// (`domain::timeout`'s whole module resolves this per run): once that
    /// fires, Argo kills the pod, the log stream reaches end-of-file, and
    /// `follow` returns on its own through its `Ok(Ok(None))` arm — the one
    /// exit that is a completed read — no idle timeout needed for a pod that
    /// is merely slow.
    /// What is *not* already bounded that way is a wedged **connection** to an
    /// otherwise-healthy pod, which is a transport failure, not a
    /// test-duration one — the same class [`Self`]'s own note below on
    /// `kube::Config::read_timeout` (295 s, connector-level, resets on any
    /// byte) already partially covers.
    ///
    /// **600 seconds**, taken from `QaRunsConfig::orphan_timeout_seconds`'s
    /// own default (`config.rs`, the claim reconciler's floor) — this
    /// codebase's own existing answer to "how long is silence long enough to
    /// call the thing on the other end dead" — and in the same order of
    /// magnitude as `kube`'s own 295 s, not two orders of magnitude above it.
    ///
    /// # The trade this makes, and what whole-branch review C1 found wrong with it
    ///
    /// **The paragraph that stood here was false, and it is worth saying how it
    /// was false rather than simply replacing it.** It said a pod quiet for
    /// longer than this had its follow abandoned at "a loss of granularity, not
    /// of correctness", because `run`'s loop resumed and Argo's eventual
    /// terminal phase still produced a correct `Finished`; and that if
    /// something later ended the whole observation, the dispatcher's 5 s
    /// re-attach (`domain::service::dispatch::reattach_watchers`) reopened the
    /// stream and `LogResume` suppressed the duplicate — "wasteful, not lossy".
    ///
    /// Both halves were right about the mechanism and wrong about the order.
    /// The `Finished` was not the *consolation* for the abandoned follow, it
    /// was what **prevented** the re-attach: `IngestService::finish` records
    /// the terminal state, `runs_sea_repo::active_states` is `dispatching |
    /// running` and nothing else, and `watch_candidates_query` filters on
    /// exactly that — so the run stopped being a re-attach candidate at the
    /// same moment its log stopped being read. The re-attach offered as the
    /// safety net is real only when an observation ends *abnormally*; on this
    /// path it ended by reporting a verdict.
    ///
    /// What was lost was the rest of that pod's log and, worse, every
    /// `=== TEST_CASE: … ===` marker after the gap — those tests produced no
    /// `TestResult` and no row, the five counters are tallied from stored rows,
    /// and the verdict came from Argo's workflow phase regardless. A runner
    /// exiting 0 despite failures therefore produced a run reported
    /// **Succeeded with a truncated, all-passing result set**.
    ///
    /// **What the give-up does now**: it leaves that pod un-drained and
    /// reports `FollowOutcome::Incomplete`, so the pass goes on to follow the
    /// pod's siblings and `Watcher::run` then ends the observation without a
    /// `Finished` — see
    /// [`Watcher::follow`](crate::infra::executor::argo::watch::Watcher::follow)'s
    /// own doc for the trace, for why end-of-file is the only exit that still
    /// reports one, and for the two costs summarised below. The run stays in
    /// `active_states`, `reattach_watchers` re-attaches on its next 5 s tick,
    /// and `LogResume` suppresses whatever was already archived, so a pod that
    /// was merely quiet resumes rather than duplicating its log — **unless that
    /// node's log has rotated**, in which case `LineSkip`'s first-line guard
    /// mismatches and that node's archive grows unbounded on every further
    /// re-attach. Pre-existing, but reachable more often now that every reset
    /// produces a re-attach where it used to produce none.
    ///
    /// **The cost this field now buys is a loop, not a truncation** — with two
    /// qualifications that "bounded" and "loud" would otherwise paper over:
    ///
    /// * The loop terminates at the control-plane timeout sweep, **except** for
    ///   a run whose `timeout_at` is NULL because an unclamped `plan.yaml`
    ///   `timeout_seconds` saturated. `domain::service::launch` records that
    ///   case and that `timeout_candidates_query` excludes it — *"a run the
    ///   control-plane timeout sweep can never reclaim"* — so for those the
    ///   loop has no terminator at all.
    /// * Each pass logs a `warn!` naming the pod, the node and this deadline —
    ///   **while the Argo workflow object still exists.** Once
    ///   [`Self::workflow_ttl_seconds`] collects it, `run`'s `Ok(None)` arm
    ///   returns immediately with no log line of its own and the period tightens
    ///   from `this value + 5 s` to the bare 5 s tick, traced only by
    ///   `service::watch`'s INFO. For a run whose timeout exceeds the workflow
    ///   TTL that is the steady state.
    ///
    /// Lowering this value tightens the loop; raising it loosens the loop and
    /// lengthens the window in which a wedged pod's siblings wait — the pods
    /// are followed one at a time, so each wedged pod ahead of them costs
    /// this many seconds on every pass.
    ///
    /// **That is a delay, and the version of this paragraph that stood here
    /// called it an abort, correctly, about the code it was written
    /// against.** C1's give-up returned the same `false` as "the observer has
    /// gone away", `drain_pods` read every `false` as the second, and so it
    /// returned at the first pod that gave up: with a stable `list` order and
    /// a per-`Watcher` `drained` set, a persistently wedged pod that sorted
    /// first meant its siblings' logs were never read for the life of the
    /// run. `FollowOutcome` separates the two meanings and `drain_pods` now
    /// walks past a give-up to the pods behind it — see `follow`'s own doc.
    /// A long deadline against a short run can still have the timeout sweep
    /// reclaim the run before a sibling is reached, so this is not a promise
    /// that nothing is lost; what is gone is the permanence. Neither
    /// direction silently reports a verdict over a log it did not read.
    ///
    /// Read through `.max(1)` at the call site (`Watcher::follow`), the same
    /// guard [`Self::status_poll_seconds`] gets, so a misconfigured `0` cannot
    /// produce a zero-duration timeout that fires between every single line.
    ///
    /// # This is not the first idle bound in the path, only the first explicit one
    ///
    /// `kube::Config`'s own `read_timeout` (295 s, unset by this adapter, so
    /// its library default stands) already sits under every call this
    /// `Watcher` makes, `follow`'s included — measured against `kube-client`
    /// 3.1.0's source, not assumed: it is a connector-level socket read
    /// timeout, not a per-request one, so it already bounds a connection that
    /// goes **completely silent at the transport level**. What it does not
    /// reach is the case this field is really for: bytes arriving (a
    /// keep-alive, a partial chunk) without ever completing one more log
    /// *line* — `kube`'s timeout resets on each such byte and never fires,
    /// while this field's clock is line-granular and does not. This field is
    /// also the one this codebase can see, name in a log line, change per
    /// deployment (the Helm chart row above), and change without a `kube`
    /// upgrade, where 295 s is presently none of those.
    pub log_follow_idle_seconds: u64,
}

impl Default for ArgoExecutorConfig {
    fn default() -> Self {
        Self {
            namespace: "argo".to_owned(),
            kubeconfig_path: None,
            runner_image: String::new(),
            runner_command: vec!["/entrypoint.sh".to_owned()],
            image_pull_policy: "IfNotPresent".to_owned(),
            workflow_ttl_seconds: 3600,
            status_poll_seconds: 3,
            bundle_base_url: None,
            workflow_service_account: None,
            bundle_auth: None,
            secret_name_prefix: "qa-platform-".to_owned(),
            secret_key: "value".to_owned(),
            log_follow_idle_seconds: 600,
        }
    }
}

/// Client-credentials settings a **workflow pod** uses to authenticate its
/// bundle download.
///
/// # The problem this solves, and why it needed a human decision
///
/// `GET /qa/v1/test-bundles/{id}` is `.authenticated()`
/// (`qa-catalog/.../api/rest/routes/bundles.rs:31`) where the source system's
/// equivalent had no auth middleware at all, so a workflow pod fetching
/// [`ArgoExecutorConfig::bundle_base_url`] gets a 401 and no test *content*
/// ever reaches it. The product owner's decision on 2026-08-27 was a
/// dedicated `IdP` service-account client whose secret lives in a
/// pre-provisioned Kubernetes `Secret`.
///
/// # Everything here is a reference or a public identifier
///
/// [`Self::token_url`] and [`Self::client_id`] are public by construction — a
/// client id is not a credential. The secret is named by
/// [`Self::client_secret_secret`] / [`Self::client_secret_key`] and read by
/// the **kubelet**, never by this process: nothing in qa-runs opens that
/// `Secret`, which is what keeps `run_executor.rs:85-86` true of the bundle
/// path as well as of `EnvSource::Secret`.
///
/// # The token's tenant
///
/// The token's `tenant_id` claim decides which tenant's bundles the pod can
/// read, so the `IdP` client must be configured to mint the tenant that owns
/// them (a hardcoded claim mapper, in the dev realm). Getting that wrong is a
/// 403/404 on download, not a 401 — worth knowing, because it looks like a
/// missing bundle.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleAuthConfig {
    /// The `IdP`'s token endpoint, as a **workflow pod** can reach it.
    ///
    /// Not necessarily the issuer a browser uses: on the dev deployment the
    /// pod reaches Keycloak's http listener while the token it mints still
    /// carries the https *frontend* issuer as `iss` (that is what `KC_HOSTNAME`
    /// pins), so the token validates against the gears' `issuer_pattern`
    /// regardless of which listener minted it. Measured on the dev cluster,
    /// 2026-08-27.
    pub token_url: String,

    /// `client_id` for the `client_credentials` grant. A public identifier.
    pub client_id: String,

    /// Name of the pre-provisioned Kubernetes `Secret` holding the client
    /// secret, in [`ArgoExecutorConfig::namespace`].
    ///
    /// **Not created by this adapter.** A run whose pod cannot resolve this
    /// `Secret` fails to start, loudly, rather than running without a
    /// credential and reporting an empty test suite as a pass — which is why
    /// the emitted `secretKeyRef` is *not* `optional`.
    pub client_secret_secret: String,

    /// Key inside [`Self::client_secret_secret`]. Default `client_secret`.
    #[serde(default = "default_client_secret_key")]
    pub client_secret_key: String,
}

/// [`BundleAuthConfig::client_secret_key`]'s default.
fn default_client_secret_key() -> String {
    "client_secret".to_owned()
}

impl Default for QaRunsConfig {
    /// Named rather than positional, so a field added mid-struct cannot
    /// silently shift a value onto the wrong knob.
    fn default() -> Self {
        Self {
            dispatcher_enabled: true,
            dispatcher_interval_seconds: 5,
            scheduler_enabled: true,
            schedule_interval_seconds: 60,
            orphan_timeout_seconds: 600,
            queue_ttl_seconds: 7200,
            queue_max_depth: 20,
            // 50, not 0 - see the field's own doc for the derivation
            // (`cpt-cf-qa-nfr-scale`'s "50 concurrently executing runs") and
            // why `0` stays available as an explicit unbounded opt-out.
            max_concurrent_runs: 50,
            default_timeout_seconds: 3600,
            max_timeout_seconds: 86_400,
            log_buffer_lines: DEFAULT_LOG_CHANNEL_CAPACITY,
            executor: ExecutorKind::Mock,
            argo: ArgoExecutorConfig::default(),
        }
    }
}

impl QaRunsConfig {
    /// Whether a ticker should be started at all.
    ///
    /// The composite of the two knobs that can stop it, so the bootstrap asks
    /// once instead of re-deriving the precedence - see
    /// [`Self::dispatcher_enabled`] for which wins and why both exist.
    #[must_use]
    pub fn dispatcher_runs(&self) -> bool {
        self.dispatcher_enabled && self.effective_dispatcher_interval_seconds() != 0
    }

    /// The tick interval the ticker must actually use.
    ///
    /// `0` disables the ticker and is returned unchanged; anything else is
    /// raised to [`MIN_DISPATCHER_INTERVAL_SECONDS`], with a WARN naming both
    /// the configured and the effective value so the divergence is visible in
    /// the boot log rather than only in this doc comment.
    ///
    /// **The warning fires on every call, not once.** An earlier version of this
    /// doc said "one-time", which was a description of how the accessor is
    /// meant to be *used* rather than of what it does. Calling it inside a serve
    /// loop would emit a WARN every tick - at the default cadence, every five
    /// seconds - which is the exact noise failure
    /// [`Self::dispatcher_enabled`] exists to prevent. **Callers must read it
    /// once at init and store the result.**
    #[must_use]
    pub fn effective_dispatcher_interval_seconds(&self) -> u64 {
        clamp_up(
            self.dispatcher_interval_seconds,
            MIN_DISPATCHER_INTERVAL_SECONDS,
            "dispatcher_interval_seconds",
            "each tick does cross-tenant queue and run reads plus one executor listing",
        )
    }

    /// Whether a schedule firing ticker should be started at all.
    ///
    /// [`Self::dispatcher_runs`]'s counterpart, and the same composite.
    #[must_use]
    pub fn scheduler_runs(&self) -> bool {
        self.scheduler_enabled && self.effective_schedule_interval_seconds() != 0
    }

    /// The schedule tick interval the ticker must actually use.
    ///
    /// `0` disables the ticker and is returned unchanged; anything else is
    /// raised to [`MIN_SCHEDULE_INTERVAL_SECONDS`]. Warns on every call, with
    /// the same caller obligation as
    /// [`Self::effective_dispatcher_interval_seconds`].
    #[must_use]
    pub fn effective_schedule_interval_seconds(&self) -> u64 {
        clamp_up(
            self.schedule_interval_seconds,
            MIN_SCHEDULE_INTERVAL_SECONDS,
            "schedule_interval_seconds",
            "a five-field cron expression resolves to the minute, so a faster tick \
             re-enumerates every schedule in the fleet for nothing",
        )
    }

    /// The orphan timeout the reconciliation pass must actually use.
    ///
    /// Unlike the tick interval, `0` is **not** a disable here - it would mean
    /// "fail every claim that has no execution reference yet", which is every
    /// claim that is mid-launch. It is clamped like any other sub-floor value.
    ///
    /// Warns on every call, with the same caller obligation as
    /// [`Self::effective_dispatcher_interval_seconds`].
    #[must_use]
    pub fn effective_orphan_timeout_seconds(&self) -> u64 {
        if self.orphan_timeout_seconds >= MIN_ORPHAN_TIMEOUT_SECONDS {
            return self.orphan_timeout_seconds;
        }
        warn!(
            configured_seconds = self.orphan_timeout_seconds,
            effective_seconds = MIN_ORPHAN_TIMEOUT_SECONDS,
            "qa-runs: orphan_timeout_seconds is below the supported floor; clamping (failing a \
             claim before its launch has finished building momentarily releases its platform)"
        );
        MIN_ORPHAN_TIMEOUT_SECONDS
    }

    /// The timeout ceiling the REST boundary rejects against, reconciled with
    /// the domain's own fail-safe.
    ///
    /// There are two ceilings and they can disagree. This knob is the
    /// operator's; [`MAX_LAUNCH_TIMEOUT_SECONDS`] is the hard limit
    /// `domain::timeout` clamps to whatever the boundary let through. An
    /// operator who sets this *above* the hard limit would otherwise get the
    /// silent clamp that `domain::timeout`'s doc explicitly asks the boundary
    /// to prevent: the request is accepted, the deadline is quietly shortened,
    /// and nothing tells the caller. Taking the lower of the two makes the
    /// number in the rejection message the number that is actually enforced.
    ///
    /// `0` means "no operator ceiling", leaving the hard limit in force. That
    /// is this method's reading rather than a documented convention of the
    /// knob: `0` is a disable for `dispatcher_interval_seconds` and
    /// `queue_ttl_seconds`, and reading it as "reject every timeout" would make
    /// an unset-looking value break every launch that names one.
    #[must_use]
    pub fn effective_max_timeout_seconds(&self) -> u64 {
        if self.max_timeout_seconds == 0 {
            return MAX_LAUNCH_TIMEOUT_SECONDS;
        }
        self.max_timeout_seconds.min(MAX_LAUNCH_TIMEOUT_SECONDS)
    }
}

/// Raise `configured` to `floor`, leaving `0` alone, warning when it changes.
///
/// Shared by the accessors that treat `0` as "disabled" — the two ticker
/// cadences. Kept private: the public contract is the `effective_*` accessors,
/// because they are what carry the per-knob reasoning about what `0` means, and
/// `orphan_timeout_seconds` deliberately does not reach this helper at all.
fn clamp_up(configured: u64, floor: u64, knob: &'static str, why: &'static str) -> u64 {
    if configured == 0 || configured >= floor {
        return configured;
    }
    warn!(
        knob,
        configured_seconds = configured,
        effective_seconds = floor,
        "qa-runs: {knob} is below the supported floor; clamping ({why})"
    );
    floor
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_LOG_CHANNEL_CAPACITY, ExecutorKind, MAX_LAUNCH_TIMEOUT_SECONDS,
        MIN_DISPATCHER_INTERVAL_SECONDS, MIN_ORPHAN_TIMEOUT_SECONDS, MIN_SCHEDULE_INTERVAL_SECONDS,
        QaRunsConfig,
    };

    /// The defaults are asserted field by field rather than through a
    /// serialized snapshot, because the failure this guards against is a value
    /// moving, not the struct's shape changing.
    #[test]
    fn the_shipped_defaults_are_what_the_deployment_notes_promise() {
        let config = QaRunsConfig::default();
        assert!(config.dispatcher_enabled);
        assert_eq!(config.dispatcher_interval_seconds, 5);
        assert!(config.scheduler_enabled);
        assert_eq!(config.schedule_interval_seconds, 60);
        assert_eq!(config.orphan_timeout_seconds, 600);
        assert_eq!(config.queue_ttl_seconds, 7200);
        assert_eq!(config.queue_max_depth, 20);
        assert_eq!(
            config.max_concurrent_runs, 50,
            "review finding #19: the default moved off 0 - see the field's own doc \
             for the derivation"
        );
        assert_eq!(config.default_timeout_seconds, 3600);
        assert_eq!(config.max_timeout_seconds, 86_400);
        assert_eq!(
            config.log_buffer_lines, DEFAULT_LOG_CHANNEL_CAPACITY,
            "one quantity, one default - see the field's doc"
        );
    }

    /// **Named for what this actually exercises, not for the property it
    /// stands in for.** Fix round 1, Minor: this used to be named
    /// "the default cap actually bounds concurrent runs", which is a claim
    /// about `AdmissionService` that a test calling two pure functions does
    /// not get to make on its own. What this *does* pin: the default plugged
    /// into `domain::queue::global_cap_status` and `cap_reached` — the exact
    /// pair `admission::GlobalCapGate::reserve` calls — reads as a real,
    /// reached cap rather than as "disabled". Before this task the shipped
    /// default was `0`, which `global_cap_status` reads as "disabled"
    /// regardless of how many runs are active, so no deployment that left the
    /// knob unset was ever refused for concurrency; a regression that
    /// reverted the default to `0` (or any other value the pair never treats
    /// as reached) fails here. Whether `GlobalCapGate` itself calls this pair
    /// correctly is a different, already-covered claim —
    /// `admission`'s own test suite owns that.
    #[test]
    fn the_default_reads_as_a_reached_cap_through_global_cap_status_and_cap_reached() {
        use crate::domain::queue::{cap_reached, global_cap_status};

        let max = QaRunsConfig::default().max_concurrent_runs;
        assert!(
            !cap_reached(global_cap_status(max - 1, max)),
            "one below the default limit must still be admitted"
        );
        assert!(
            cap_reached(global_cap_status(max, max)),
            "at the default limit the next run must be refused - this is the cap \
             that a `0` default made unreachable"
        );
    }

    /// The waiver in ADR-0001 turns this from a preference into a constraint:
    /// "the mock stays the default executor, so no deployment gains a
    /// Kubernetes dependency by upgrading". A default that drifted to `Argo`
    /// would make every existing deployment try to reach an API server.
    #[test]
    fn the_default_executor_is_the_mock_and_the_argo_block_is_inert() {
        let config = QaRunsConfig::default();
        assert_eq!(config.executor, ExecutorKind::Mock);
        assert_eq!(config.argo.namespace, "argo");
        assert!(
            config.argo.runner_image.is_empty(),
            "no image can be a sensible default; an unset one must fail at boot"
        );
        assert_eq!(
            config.argo.runner_command,
            vec!["/entrypoint.sh".to_owned()]
        );
        assert_eq!(config.argo.workflow_ttl_seconds, 3600);
    }

    /// One configuration file has to be valid against both builds — with and
    /// without the `argo` cargo feature — so the block is deserialized
    /// unconditionally and `deny_unknown_fields` on the outer struct does not
    /// reject it.
    #[test]
    fn the_argo_block_parses_in_any_build() {
        // JSON rather than YAML: the gear reads YAML, but what is under test is
        // the `serde` shape, and `serde_json` is already a dependency where
        // `serde_yaml` would be a new one for one assertion.
        let config: QaRunsConfig = serde_json::from_str(
            r#"{"executor":"argo","argo":{"namespace":"qa","runner_image":"runner:1","status_poll_seconds":1}}"#,
        )
        .expect("the argo block must parse");
        assert_eq!(config.executor, ExecutorKind::Argo);
        assert_eq!(config.argo.namespace, "qa");
        assert_eq!(config.argo.runner_image, "runner:1");
        assert_eq!(config.argo.status_poll_seconds, 1);
        // Untouched keys keep their defaults rather than being zeroed.
        assert_eq!(config.argo.workflow_ttl_seconds, 3600);
        assert_eq!(config.argo.secret_key, "value");
    }

    /// `0` is the documented disable, and clamping it to the floor would turn
    /// "no dispatcher" into "a dispatcher every five seconds".
    #[test]
    fn a_zero_tick_interval_stays_zero_because_zero_disables_the_ticker() {
        let config = QaRunsConfig {
            dispatcher_interval_seconds: 0,
            ..QaRunsConfig::default()
        };
        assert_eq!(config.effective_dispatcher_interval_seconds(), 0);
    }

    #[test]
    fn a_sub_floor_tick_interval_is_raised_to_the_floor() {
        let config = QaRunsConfig {
            dispatcher_interval_seconds: 1,
            ..QaRunsConfig::default()
        };
        assert_eq!(
            config.effective_dispatcher_interval_seconds(),
            MIN_DISPATCHER_INTERVAL_SECONDS
        );
    }

    #[test]
    fn a_tick_interval_at_or_above_the_floor_is_taken_verbatim() {
        for configured in [MIN_DISPATCHER_INTERVAL_SECONDS, 900] {
            let config = QaRunsConfig {
                dispatcher_interval_seconds: configured,
                ..QaRunsConfig::default()
            };
            assert_eq!(config.effective_dispatcher_interval_seconds(), configured);
        }
    }

    /// The scheduler's cadence obeys the same `0`-disables rule as the
    /// dispatcher's — pinned separately, because the two knobs have different
    /// floors and a shared assertion could not tell one accessor reading the
    /// other's field.
    #[test]
    fn a_zero_schedule_interval_stays_zero_because_zero_disables_the_ticker() {
        let config = QaRunsConfig {
            schedule_interval_seconds: 0,
            ..QaRunsConfig::default()
        };
        assert_eq!(config.effective_schedule_interval_seconds(), 0);
    }

    /// **The two cadence floors are different numbers, and each accessor must
    /// apply its own.** Reading `dispatcher_interval_seconds` here, or clamping
    /// to `MIN_DISPATCHER_INTERVAL_SECONDS`, compiles and is invisible to a test
    /// that only checks "some clamping happened".
    #[test]
    fn a_sub_floor_schedule_interval_is_raised_to_its_own_floor() {
        let config = QaRunsConfig {
            // Deliberately above the schedule floor and legal for the
            // dispatcher, so an accessor reading the wrong field answers 900
            // rather than the floor.
            dispatcher_interval_seconds: 900,
            schedule_interval_seconds: 1,
            ..QaRunsConfig::default()
        };
        assert_eq!(
            config.effective_schedule_interval_seconds(),
            MIN_SCHEDULE_INTERVAL_SECONDS
        );
        assert_ne!(
            MIN_SCHEDULE_INTERVAL_SECONDS, MIN_DISPATCHER_INTERVAL_SECONDS,
            "premise: the two floors really are different, which is what makes \
             the assertion above discriminating"
        );
    }

    #[test]
    fn a_schedule_interval_at_or_above_the_floor_is_taken_verbatim() {
        for configured in [MIN_SCHEDULE_INTERVAL_SECONDS, 3600] {
            let config = QaRunsConfig {
                schedule_interval_seconds: configured,
                ..QaRunsConfig::default()
            };
            assert_eq!(config.effective_schedule_interval_seconds(), configured);
        }
    }

    /// The clamp warning names the knob that moved. Without the knob name an
    /// operator with both cadences misconfigured cannot tell which line is
    /// about which.
    #[test]
    #[tracing_test::traced_test]
    fn a_clamped_schedule_interval_says_which_knob_it_was() {
        let config = QaRunsConfig {
            schedule_interval_seconds: 3,
            ..QaRunsConfig::default()
        };
        assert_eq!(
            config.effective_schedule_interval_seconds(),
            MIN_SCHEDULE_INTERVAL_SECONDS
        );
        assert!(logs_contain("schedule_interval_seconds"));
        assert!(logs_contain("configured_seconds=3"));
        assert!(logs_contain(&format!(
            "effective_seconds={MIN_SCHEDULE_INTERVAL_SECONDS}"
        )));
        assert!(
            !logs_contain("dispatcher_interval_seconds"),
            "the default dispatcher cadence is legal, so nothing may claim it clamped"
        );
    }

    /// Both ways of switching the scheduler off, and the independence from the
    /// dispatcher's pair — which is the property that lets an operator freeze
    /// new scheduled runs while the queue keeps draining.
    #[test]
    fn either_disable_stops_the_scheduler_and_the_two_tickers_are_independent() {
        assert!(QaRunsConfig::default().scheduler_runs());

        for switched_off in [
            QaRunsConfig {
                scheduler_enabled: false,
                ..QaRunsConfig::default()
            },
            QaRunsConfig {
                schedule_interval_seconds: 0,
                ..QaRunsConfig::default()
            },
            QaRunsConfig {
                scheduler_enabled: false,
                schedule_interval_seconds: 3600,
                ..QaRunsConfig::default()
            },
        ] {
            assert!(!switched_off.scheduler_runs());
            assert!(
                switched_off.dispatcher_runs(),
                "switching the scheduler off must not stop the dispatcher"
            );
        }

        let no_dispatcher = QaRunsConfig {
            dispatcher_enabled: false,
            ..QaRunsConfig::default()
        };
        assert!(
            no_dispatcher.scheduler_runs(),
            "and the other direction: schedules still fire with the dispatcher off"
        );
    }

    /// The asymmetry against the tick interval, pinned so it cannot be
    /// "tidied" into a shared helper: `0` here means "fail every claim that has
    /// not yet recorded an execution reference", which is every claim that is
    /// still building.
    #[test]
    fn a_zero_orphan_timeout_is_clamped_rather_than_treated_as_disabled() {
        let config = QaRunsConfig {
            orphan_timeout_seconds: 0,
            ..QaRunsConfig::default()
        };
        assert_eq!(
            config.effective_orphan_timeout_seconds(),
            MIN_ORPHAN_TIMEOUT_SECONDS
        );
    }

    #[test]
    fn a_sub_floor_orphan_timeout_is_raised_to_the_floor() {
        let config = QaRunsConfig {
            orphan_timeout_seconds: 59,
            ..QaRunsConfig::default()
        };
        assert_eq!(
            config.effective_orphan_timeout_seconds(),
            MIN_ORPHAN_TIMEOUT_SECONDS
        );
    }

    #[test]
    fn an_orphan_timeout_at_or_above_the_floor_is_taken_verbatim() {
        for configured in [MIN_ORPHAN_TIMEOUT_SECONDS, 600] {
            let config = QaRunsConfig {
                orphan_timeout_seconds: configured,
                ..QaRunsConfig::default()
            };
            assert_eq!(config.effective_orphan_timeout_seconds(), configured);
        }
    }

    /// **Both clamp warnings, asserted.** Plan Step 1 makes them mandatory and
    /// nothing observed either: deleting the `warn!` in `clamp_up` or in
    /// `effective_orphan_timeout_seconds` left this module green.
    ///
    /// The configured and the effective value are both asserted, because a line
    /// saying only "clamped" leaves an operator unable to tell whether their
    /// setting was the one that moved.
    #[test]
    #[tracing_test::traced_test]
    fn a_clamped_tick_interval_says_what_it_was_and_what_it_became() {
        let config = QaRunsConfig {
            dispatcher_interval_seconds: 1,
            ..QaRunsConfig::default()
        };
        assert_eq!(
            config.effective_dispatcher_interval_seconds(),
            MIN_DISPATCHER_INTERVAL_SECONDS
        );
        assert!(logs_contain("below the supported floor"));
        assert!(logs_contain("dispatcher_interval_seconds"));
        assert!(logs_contain("configured_seconds=1"));
        assert!(logs_contain(&format!(
            "effective_seconds={MIN_DISPATCHER_INTERVAL_SECONDS}"
        )));
    }

    #[test]
    #[tracing_test::traced_test]
    fn a_clamped_orphan_timeout_says_what_it_was_and_what_it_became() {
        let config = QaRunsConfig {
            orphan_timeout_seconds: 5,
            ..QaRunsConfig::default()
        };
        assert_eq!(
            config.effective_orphan_timeout_seconds(),
            MIN_ORPHAN_TIMEOUT_SECONDS
        );
        assert!(logs_contain(
            "orphan_timeout_seconds is below the supported floor"
        ));
        assert!(logs_contain("configured_seconds=5"));
        assert!(logs_contain(&format!(
            "effective_seconds={MIN_ORPHAN_TIMEOUT_SECONDS}"
        )));
    }

    /// A value at or above the floor must **not** warn, or the line stops
    /// meaning anything.
    #[test]
    #[tracing_test::traced_test]
    fn a_value_within_range_does_not_warn() {
        let config = QaRunsConfig::default();
        assert_eq!(config.effective_dispatcher_interval_seconds(), 5);
        assert_eq!(config.effective_schedule_interval_seconds(), 60);
        assert_eq!(config.effective_orphan_timeout_seconds(), 600);
        assert!(!logs_contain("below the supported floor"));
    }

    /// The precedence between the two ways to disable the ticker, pinned so the
    /// bootstrap does not have to guess - and so `enabled: true` with a zero
    /// interval cannot be read as "run it anyway".
    #[test]
    fn either_disable_stops_the_ticker_and_enabled_false_is_decisive() {
        assert!(QaRunsConfig::default().dispatcher_runs());

        let switched_off = QaRunsConfig {
            dispatcher_enabled: false,
            ..QaRunsConfig::default()
        };
        assert!(!switched_off.dispatcher_runs());

        let no_cadence = QaRunsConfig {
            dispatcher_interval_seconds: 0,
            ..QaRunsConfig::default()
        };
        assert!(!no_cadence.dispatcher_runs());

        // `enabled: false` wins whatever the interval says.
        let contradictory = QaRunsConfig {
            dispatcher_enabled: false,
            dispatcher_interval_seconds: 900,
            ..QaRunsConfig::default()
        };
        assert!(!contradictory.dispatcher_runs());
    }

    /// The two ceilings, and the direction that matters: an operator cannot
    /// raise this knob past the domain's hard limit and thereby re-introduce
    /// the silent clamp the boundary check exists to prevent.
    #[test]
    fn the_effective_timeout_ceiling_never_exceeds_the_domain_fail_safe() {
        let generous = QaRunsConfig {
            max_timeout_seconds: MAX_LAUNCH_TIMEOUT_SECONDS * 10,
            ..QaRunsConfig::default()
        };
        assert_eq!(
            generous.effective_max_timeout_seconds(),
            MAX_LAUNCH_TIMEOUT_SECONDS
        );
    }

    #[test]
    fn a_ceiling_below_the_domain_fail_safe_is_taken_verbatim() {
        let strict = QaRunsConfig {
            max_timeout_seconds: 60,
            ..QaRunsConfig::default()
        };
        assert_eq!(strict.effective_max_timeout_seconds(), 60);
        assert_eq!(
            QaRunsConfig::default().effective_max_timeout_seconds(),
            86_400,
            "the shipped default is below the hard limit, so it passes through"
        );
    }

    /// `0` is a disable elsewhere in this struct, so its reading here is
    /// pinned: no operator ceiling, hard limit still in force. The alternative
    /// reading - reject every timeout - would make an unset-looking value break
    /// every launch that names one.
    #[test]
    fn a_zero_timeout_ceiling_falls_back_to_the_domain_fail_safe() {
        let unset = QaRunsConfig {
            max_timeout_seconds: 0,
            ..QaRunsConfig::default()
        };
        assert_eq!(
            unset.effective_max_timeout_seconds(),
            MAX_LAUNCH_TIMEOUT_SECONDS
        );
    }

    /// An unknown key is a typo, and `deny_unknown_fields` is what turns a
    /// silently ignored setting into a boot failure.
    ///
    /// Driven through JSON rather than YAML - the deployment format - because
    /// `serde_yaml` is not a workspace dependency. That is a real gap in what
    /// this pins: it exercises the `serde` attributes, which are
    /// format-independent, but not the gear host's own config plumbing.
    #[test]
    fn an_unknown_key_is_rejected_rather_than_ignored() {
        let error = serde_json::from_str::<QaRunsConfig>(r#"{"dispatcher_intervall_seconds": 30}"#)
            .expect_err("an unknown key must not deserialize");
        assert!(
            error.to_string().contains("unknown field"),
            "expected an unknown-field error, got: {error}"
        );
    }

    /// A partial section fills the rest from [`Default`] rather than failing,
    /// which is what `#[serde(default)]` buys and what every deployment relies
    /// on.
    #[test]
    fn an_absent_key_falls_back_to_its_default() {
        let config: QaRunsConfig = serde_json::from_str(r#"{"dispatcher_enabled": false}"#)
            .expect("a partial section parses");
        assert!(!config.dispatcher_enabled);
        assert_eq!(config.max_timeout_seconds, 86_400);
    }
}
