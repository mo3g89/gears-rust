//! Transport-agnostic models for the qa-runs contract.
//!
//! Field inventory derived from the source system's run record. That record has
//! two halves which the source system merges on read
//! (`manager/src/services/run_history.rs:216`, `overlay_persisted_with_live`):
//!
//! * the Argo Workflow object, parsed by `parse_workflow`
//!   (`manager/src/services/argo.rs:2236-2319`) — the live half, and the
//!   authoritative list of what a run carries;
//! * a persisted `run_results` row, read as `PersistedRunRow`
//!   (`manager/src/services/run_history.rs:10-48`) — the durable half, needed
//!   because the Workflow object is garbage-collected after its TTL while
//!   reruns happen much later (`manager/migrations/001_initial.sql:306-310`).
//!
//! The two carry the same field set apart from three columns the Workflow
//! cannot hold: `created_at`, `run_parameters`, and `raw_logs`. This gear
//! collapses both halves into one `qa_runs` row
//! (`cpt-cf-qa-principle-db-first-state`), so there is no live/persisted
//! overlay to port — only the union of their fields.

use qa_catalog_sdk::Exclusivity;
use time::OffsetDateTime;
use uuid::Uuid;

/// What a launch targets.
///
/// Three of the four kinds are the *test-executing* ones, and they are three
/// rather than the source system's four: its `RunIntent::GitPlan`
/// (`manager/src/services/run_dispatcher.rs:195-207`) reads plans through a
/// second, overlapping plan reader (`manager/src/services/git_plans.rs`) that
/// DECOMPOSITION:153 dispositions as unported.
///
/// [`Self::Collect`] is the fourth and is not a test run at all — see its own
/// doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunTarget {
    /// A plan discovered from a repository's `plan.yaml`.
    Plan {
        repo_id: Uuid,
        /// Path of the plan.yaml within the repository's content root.
        path: String,
    },
    /// A single test file within a discovered plan. A single-test run
    /// considers only that one file, and no tag filter applies to it
    /// (`manager/src/services/exclusivity.rs:214-223`).
    Test {
        repo_id: Uuid,
        path: String,
        test_file: String,
    },
    /// A persisted user-composed plan; its files may span repositories.
    CustomPlan { id: Uuid },
    /// Enumerate a repository's test cases instead of executing them.
    ///
    /// The runner is put in `pytest --collect-only` mode and posts per-file
    /// counts to `collect_url` rather than running anything. Ported from the
    /// source system's collect-cases job, whose own module header states the
    /// purpose: *"launch runner workflows in collect-only mode against a given
    /// branch of each repo so the runner enumerates exact test cases (pytest
    /// --collect-only, parametrize expanded) and posts per-file counts back.
    /// Feeds the Analytics 'expected cases' number"*
    /// (`manager/src/services/collect.rs:1-6`).
    ///
    /// # Where the third input lives, and why it is not a field here
    ///
    /// A collect launch is defined by three values — repository, **branch**, and
    /// report URL — and only two of them are on this variant. The branch travels
    /// on [`LaunchRequest::branch`], which every other kind already uses and
    /// which `service::launch`'s rule-1 chain already resolves; rule 6 then
    /// records it as the run's `test_version`. A second `branch` here would be a
    /// second place for the same value, and the two could disagree — which is
    /// exactly the duplication the source system warns about for its own branch
    /// chain (`manager/src/services/exclusivity.rs:290-296`).
    ///
    /// # The URL is supplied, never built
    ///
    /// The source system's control plane builds it —
    /// `format!("{}/api/collect/{}/{}", base, repo.id, branch)`
    /// (`manager/src/services/collect.rs:90`) — because there the control plane
    /// *is* the analytics service. Here it is not: qa-insights owns the report
    /// route, so it hands the URL in and the runner posts wherever it is told.
    /// That is the whole reason this is a `String` on the request rather than
    /// something qa-runs derives, and it is what preserves the
    /// `VHP_COLLECT_URL` contract across the gear split.
    ///
    /// A blank URL is legal and means "collect but report nowhere", matching the
    /// source system, which pushes `VHP_COLLECT_URL` only for a non-blank one
    /// (`manager/src/services/argo.rs:52-58`).
    Collect { repo_id: Uuid, collect_url: String },
}

impl RunTarget {
    /// Stable discriminant recorded on the run and the queue row. Mirrors the
    /// source system's `vhp-tests/run-kind` annotation vocabulary
    /// (`manager/src/services/argo.rs:2245-2256`).
    #[must_use]
    pub fn kind(&self) -> RunKind {
        match self {
            Self::Plan { .. } => RunKind::Plan,
            Self::Test { .. } => RunKind::Test,
            Self::CustomPlan { .. } => RunKind::CustomPlan,
            Self::Collect { .. } => RunKind::Collect,
        }
    }
}

/// The `run_kind` discriminant, as persisted and reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    Plan,
    Test,
    CustomPlan,
    /// A collect-only enumeration run — see [`RunTarget::Collect`].
    Collect,
}

impl RunKind {
    /// The persisted spelling, and the sole encoder — `infra::storage::mapper`
    /// writes only the `_from_str` half and delegates here.
    ///
    /// # `"collect"` is this port's spelling, and the source system's word — but
    /// not its run kind
    ///
    /// **Checked rather than assumed, and the obvious citation is wrong.** The
    /// source system's collect submission goes through `submit_workflow`, which
    /// hard-codes `annotations.insert("vhp-tests/run-kind", "plan")`
    /// (`manager/src/services/argo.rs:608`) — every collect workflow there is
    /// annotated a **plan** run, because collect reuses the plan submit path
    /// wholesale and the kind annotation is a constant on it.
    ///
    /// The literal `"collect"` does appear, one argument over: it is the
    /// `run_source` the collect job passes (`manager/src/services/collect.rs:140`),
    /// which `normalize_run_source` then folds to `"manual"`
    /// (`argo.rs:2461-2466`, `_ => "manual"`) before the annotation is written.
    /// So the word is legacy's and the *position* is this port's: a collect run
    /// here is a distinct kind rather than a plan run wearing a source label,
    /// because the split is what lets `service::launch` route it past admission
    /// by type instead of by inspecting a string. Its [`RunSource`] stays
    /// `Manual`, which is what legacy's normalization produces.
    ///
    /// The spelling is deliberately not `custom_plan`-shaped: it is one lowercase
    /// word with no `_`, so it is also safe as a run-name base under
    /// `domain::naming`'s `LIKE '{prefix}-%'` rule.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Test => "test",
            Self::CustomPlan => "custom_plan",
            Self::Collect => "collect",
        }
    }
}

/// Who asked. Normalized to exactly these two, as the source system does
/// (`manager/src/services/argo.rs:2293-2296`,
/// `manager/src/routes/runs.rs:595-603`: anything that is not `scheduled` is
/// `manual`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunSource {
    Manual,
    Scheduled,
}

impl RunSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
        }
    }
}

/// One `name=value` launch parameter.
///
/// No `secure` flag, unlike the source system's `PipelineVariable`
/// (`manager/src/models.rs:792-797`). That flag belongs to the *global*
/// pipeline-variables setting, not to a launch: the source system asserts run
/// parameters are never secret (`manager/src/routes/settings.rs:743`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunParameter {
    pub name: String,
    pub value: String,
}

/// A launch request. One shape for every caller — manual, CI, scheduled, and
/// auto-rerun all go through it, which is the invariant
/// `cpt-cf-qa-fr-runs-schedules` states as a requirement.
#[derive(Clone, Debug, PartialEq)]
pub struct LaunchRequest {
    pub target: RunTarget,
    /// `None` = a run with no target platform. Such a run is never queued and
    /// never blocks anything (guide line 76;
    /// `manager/src/services/run_dispatcher.rs:103-106`).
    pub platform_id: Option<Uuid>,
    /// Test-content branch. Resolution order when absent: platform
    /// `default_branch`, then repository `default_branch` (parity spec §3.4
    /// step 1; `manager/src/services/exclusivity.rs:297-312` plus
    /// `manager/src/routes/runs.rs:639-644`).
    pub branch: Option<String>,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub parameters: Vec<RunParameter>,
    /// The launch exclusivity tier: three-state. `Exclusivity::Inherit` means
    /// "inherit", which is **not** `Exclusivity::Shared` — that distinction is
    /// the whole reason this and the tiers below it are a closed three-state
    /// type rather than `bool` (guide lines 35-45). Was `Option<bool>` behind
    /// a `qa_catalog_sdk::ExclusiveFlag` alias; see [`Exclusivity`]'s doc for
    /// why the type closes it and why the wire form does not move. Review
    /// findings #10 and #11.
    pub exclusive: Exclusivity,
    /// Per-run timeout override, seconds. `None` falls back to the plan's
    /// `timeout_seconds`, then the configured default — the source system's
    /// two `activeDeadlineSeconds` sites (`manager/src/services/argo.rs:539`
    /// from the plan, `:845` from `default_timeout_seconds`).
    pub timeout_seconds: Option<u64>,
    pub source: RunSource,
    /// Set when a schedule produced this launch.
    pub schedule_id: Option<Uuid>,
}

/// A run's lifecycle state.
///
/// The terminal states — those a run can never leave — are `Succeeded`,
/// `Failed`, `Canceled`, `TimedOut`, `Expired` and `Error`. Named rather than
/// counted ("the last six") because a positional claim silently becomes wrong
/// the moment a variant is inserted, and this crate cannot reference
/// `qa-runs`'s `domain::state_machine::TERMINAL_STATES`, which is where that
/// set is enforced.
///
/// **`Canceled` has one `l`, [`QueueState::Cancelled`] has two. This is
/// deliberate — do not "fix" either.** They are different vocabularies on
/// different tables. The run state is *not* ported from legacy: the source
/// system has no run-level cancel state at all, and contains **zero**
/// occurrences of the one-`l` spelling anywhere — this vocabulary is DESIGN
/// §3.1's own (`DESIGN.md:240`). The queue state *is* ported, from
/// `manager/src/services/run_queue.rs:413` (`SET state = 'cancelled'`), and is
/// frozen by guide line 95.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Created,
    Queued,
    Dispatching,
    Running,
    Succeeded,
    Failed,
    Canceled,
    TimedOut,
    /// The run's queue row hit `queue_ttl_seconds` and was swept before it ever
    /// started. **Added 2026-08-13 by user decision**, raised by Task 7: the
    /// transition table gave `Queued` exactly one terminal exit, `Canceled`,
    /// and nothing said what a TTL-expired run becomes. The source system never
    /// faced the question — it expires the *queue row* to `'expired'`
    /// (`manager/src/services/run_queue.rs:370-378`) and has no run row for a
    /// launch that never started. Reusing `Canceled` would make the TTL sweep
    /// indistinguishable from an operator cancel; reusing `TimedOut` would
    /// conflate a queue-wait clock with `cpt-cf-qa-fr-runs-timeout`'s execution
    /// deadline. The guide already makes the `expired` alert mandatory (guide
    /// line 96), so an operator sees that word — the run must not say something
    /// else.
    ///
    /// Unlike the cancel pair above, both vocabularies spell this event
    /// `expired`, for the same event on the queue row
    /// ([`QueueState::Expired`]). That agreement is the point — do not
    /// differentiate them.
    Expired,
    Error,
}

impl RunState {
    /// The persisted spelling. Sole encoder — the infra mapper writes only the
    /// `_from_str` half and delegates here, so the two spellings of cancel*ed*
    /// never get independent homes.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Queued => "queued",
            Self::Dispatching => "dispatching",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::TimedOut => "timed_out",
            Self::Expired => "expired",
            Self::Error => "error",
        }
    }
}

/// The authoritative run record.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub id: Uuid,
    /// Human-facing run name, `{slug}-{n}`. Load-bearing beyond display: the
    /// queue's `blocked_by` text names the run holding a platform (guide line
    /// 177), so runs need a stable short name, not just a UUID.
    pub name: String,
    pub target: RunTarget,
    pub platform_id: Option<Uuid>,
    /// Branch actually resolved and executed against; also recorded as the
    /// run's test version (`manager/src/routes/custom_plans.rs:960`,
    /// `routes/runs.rs:645`).
    pub test_version: Option<String>,
    /// Platform application version, **snapshotted at launch** — legacy sets
    /// `app_version = platform_version` (`manager/src/routes/runs.rs:594`) and
    /// persists it on `run_results` (`migrations/001_initial.sql:56`) rather
    /// than re-deriving it, because the Workflow is GC'd while re-runs happen
    /// much later (`migrations/001_initial.sql:306-310`). Re-deriving from
    /// `platform_id` would let a platform upgrade silently change a queued
    /// run's or a re-run's `APP_VERSION`, breaking both reproducibility and
    /// PRD:577's "environment variable names consumed by tests are unchanged"
    /// contract. Feeds the `APP_VERSION` static runner variable in Task 8c's
    /// tier 1.
    pub app_version: Option<String>,
    /// Platform build identifier, snapshotted at launch alongside
    /// [`Self::app_version`] — both come from the same platform lookup
    /// (`manager/src/routes/runs.rs:579`), persisted at
    /// `migrations/001_initial.sql:149`. Feeds `APP_BUILD`.
    pub app_build: Option<String>,
    pub state: RunState,
    /// The exclusivity decision actually made, plus which tier made it.
    ///
    /// The source system records only the boolean, written as the
    /// `vhp-tests/exclusive` annotation by each of its three submit paths
    /// (`manager/src/services/argo.rs:612, 918, 1342`) after
    /// `manager/src/services/run_dispatcher.rs:91` carries it onto the intent.
    /// The tier it resolves goes to the log and nowhere else
    /// (`run_dispatcher.rs:92-99`, its only consumer). Persisting the tier is a
    /// deliberate improvement on that: the source system's own rationale for
    /// producing it is so an operator can always tell *why* a run became
    /// exclusive (`manager/src/services/exclusivity.rs:13-14`), which a log
    /// line cannot answer once the log has rotated.
    ///
    /// **Named `resolved_exclusive`, not `exclusive`.** This is the *resolved*
    /// decision, while [`LaunchRequest::exclusive`] is an `Option<bool>`
    /// tri-state request. Under the short name the obvious re-run
    /// transcription `exclusive: Some(run.exclusive)` type-checked and was
    /// **wrong**: it pins a stored `false` onto the launch tier, which
    /// outranks everything and suppresses a `TEST_META`/`plan.yaml`
    /// declaration added since — relaunching a since-marked-destructive test
    /// in parallel on a shared platform. Legacy had no type barrier either and
    /// guarded it with the same prose comment three times
    /// (`manager/src/routes/runs.rs:958-967`, `:1008-1018`, `:1071-1081`); the
    /// distinct name is this port's structural replacement for it. The correct
    /// transcription is `run.resolved_exclusive.then_some(true)`.
    /// `DESIGN.md:582` already names the column `resolved_exclusive` — the SDK
    /// field was the outlier. [`QueueEntry::exclusive`] keeps the short name:
    /// DESIGN's `run_queue` column list spells it that way, and it is a
    /// different table.
    pub resolved_exclusive: bool,
    pub exclusive_tier: ExclusiveTier,
    /// Whether this run is a validation run — `plan.yaml`'s `validation` bool
    /// OR'd with a case-insensitive `validation` tag
    /// (`manager/src/services/plans.rs:35-39`).
    ///
    /// Stamped as an annotation by the source system:
    /// `manager/src/services/argo.rs:628-630` is the **write**
    /// (`if plan.plan.validation { annotations.insert("vhp-tests/validation",
    /// "true") }`). `:2289-2292` is the *read* that recovers it into
    /// `is_validation`, which is what this citation used to point at — a "stamped
    /// by" claim needs the write site.
    pub is_validation: bool,
    pub parameters: Vec<RunParameter>,
    /// The tag filter this run was launched with.
    ///
    /// Recorded on the run, which the source system does not do
    /// (`manager/src/routes/runs.rs:1068-1069`). Persisting the filter is
    /// what makes `rerun`'s "with its original parameters"
    /// (`cpt-cf-qa-fr-runs-cancel-rerun`) actually hold.
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub source: RunSource,
    pub schedule_id: Option<Uuid>,
    /// Bundle descriptors backing this run, one per repository group (parity
    /// spec §3.4 step 5: one execution node per group).
    pub bundle_ids: Vec<Uuid>,
    /// Opaque handle from the `RunExecutor`; `None` until dispatch succeeds.
    pub execution_ref: Option<String>,
    /// Archived-log pointer. **Always `None`: nothing writes it, by decision,
    /// and no log text is reachable through this SDK.**
    ///
    /// Corrected 2026-08-31; it previously read "populated on completion (p2
    /// with 2.7)". Design D-RLP-6 decided it stays unwritten permanently
    /// rather than pending a feature: the column is `VARCHAR(2048)` and is
    /// published here shaped like a URI a consumer may fetch, so putting an
    /// internal reference in it would publish a schema detail and invite that
    /// misreading. A run's durable log lives in qa-runs' own `qa_run_logs`
    /// table and is served only as an SSE stream by
    /// `GET /qa/v1/runs/{id}/logs`; **no method on this SDK's client returns
    /// log text, and this field is not a way to reach any.** That is the
    /// property `docs/plans/2026-08-18-qa-insights-gear.md`'s coverage step
    /// relies on when it concludes the insights gear cannot parse
    /// `COVERAGE_SUMMARY` markers out of run logs — the conclusion is
    /// unchanged and now rests on a permanent decision instead of on a pending
    /// one.
    ///
    /// Replaces the source system's inline `raw_logs` column
    /// (`manager/src/services/run_history.rs:41-47`, which notes that fetching
    /// it for every row of the full history once OOM-killed the manager) — and
    /// `qa_run_logs` being a separate table is the direct lesson of that
    /// incident.
    pub log_storage_ref: Option<String>,
    /// Deadline the control plane enforces (`cpt-cf-qa-fr-runs-timeout`).
    pub timeout_at: Option<OffsetDateTime>,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    /// Terminal failure reason, operator-facing. Carries the source system's
    /// workflow `status.message` (`manager/src/services/argo.rs:2280-2283`).
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Which tier supplied a run's exclusivity flag. Ported verbatim from
/// `manager/src/services/exclusivity.rs:15-37` — including the distinction
/// between `TestMeta` with a `false` answer ("files were read and all said
/// parallel") and `Default` ("nobody had an opinion"), which the source
/// system has an explicit test for
/// (`exclusivity.rs:656-664`: the log must not read `default`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExclusiveTier {
    Launch,
    Plan,
    TestMeta,
    Default,
}

impl ExclusiveTier {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Plan => "plan.yaml",
            Self::TestMeta => "test_meta",
            Self::Default => "default",
        }
    }
}

/// Per-run outcome counts, updated incrementally as results arrive.
///
/// Counts only — **not** the run record, and unrelated to the source system's
/// `run_results` table (which is the legacy *run* row, see this module's
/// header). [`QaRunsClientV1::get_run_result`](crate::QaRunsClientV1::get_run_result)
/// returns these five numbers; `get_run` returns the [`Run`].
///
/// **There is no `error` counter, and adding one as a sixth field would be a
/// silent regression.** `failed` is the *failed-or-errored* count: the source
/// system keeps `FAILED` and `ERROR` as distinct per-test statuses but folds
/// them together in every aggregate that produces these numbers —
/// `COUNT(*) FILTER (WHERE tr.status IN ('FAILED', 'ERROR')) AS failed`
/// (`manager/src/routes/plans.rs:188-192`, which computes exactly these five
/// and is where this shape comes from; the same fold appears in
/// `routes/dashboard.rs:387`, `routes/schedules.rs:379`). That matters here
/// because `domain::state_machine::derive_terminal_state` reads `failed` to
/// decide a run's terminal state, mirroring legacy's
/// `has_failed = FAILED || ERROR` (`manager/src/services/argo.rs:2194-2196`).
/// `skipped` no longer votes on that verdict — a deliberate divergence from
/// legacy, ratified by the product owner on 2026-08-28 (see
/// `derive_terminal_state`'s "Skips no longer fail a run") — but it still
/// counts what it always counted, and the run list and detail views surface it
/// so a run that asserted nothing is still legible.
/// A separate `error` field would therefore be invisible to that rule, and a
/// run with only errored tests would report `succeeded`. If errored results
/// ever need their own number, add it *in addition to* the fold — `failed`
/// must keep counting them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunResult {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub in_progress: usize,
    pub total: usize,
}

/// One per-test row as qa-runs recorded it.
///
/// This is the **authoritative** copy. `qa-insights` builds its analytical
/// tables by reading this type back through the reconciler's SDK sweep
/// (`cpt-cf-qa-principle-async-insights` forbids the run path from reading
/// them back the other way), which is why it is a read-only projection with no
/// constructor and no `New` twin.
///
/// # Why this is not [`RunResult`]
///
/// [`RunResult`] is the *run*'s five counters. This is one **row** of the
/// `qa_run_test_results` child table, and the reconciler needs the rows rather
/// than the counters: it rebuilds qa-insights' per-test analytics, which the
/// counters cannot be un-summed back into.
///
/// # Why it carries no row identity
///
/// No `id`, no `created_at`, no `updated_at`. The row's primary key is
/// qa-runs' private business - the reconciler keys on
/// `(run_id, test_file, test_name)`, which is the same application-level
/// uniqueness tuple the write path maintains by delete-then-insert
/// (`manager/src/routes/runs.rs:1153-1185`). Exposing the surrogate id would
/// invite qa-insights to store a foreign key into another gear's table.
///
/// # Two ticket columns, deliberately
///
/// [`Self::jira_key`] is the **file**-level bug reference legacy keeps on
/// `test_results` (`manager/migrations/001_initial.sql:71`);
/// [`Self::ticket`] is the **case**-level one on `test_case_results`
/// (`:262`). Legacy's analytics renders only the latter as
/// `AnalyticsListItem::case_tickets` (`manager/src/routes/analytics.rs:132`).
/// Do not collapse them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunTestResult {
    pub run_id: Uuid,
    /// `""` when the producer reported no file, never `None` - the column is
    /// `NOT NULL DEFAULT ''` so "absent" has exactly one spelling
    /// (legacy needs `COALESCE(test_file, '')` at
    /// `manager/src/routes/runs.rs:1154` for want of that).
    pub test_file: String,
    pub test_name: String,
    /// Open set, uppercase - `PASSED` | `FAILED` | `ERROR` | `SKIPPED` |
    /// `PENDING` | `RUNNING` | `XFAIL` | `XPASS` today, and **not exhaustive**.
    /// There is deliberately no enum: legacy writes unvalidated runner text
    /// into this column (`manager/src/routes/runs.rs:1115` -> `:1168-1174`),
    /// so a ninth value is a runner change rather than corruption and a
    /// consumer that rejected it would drop a real result.
    pub status: String,
    /// The runner's duration text **verbatim**, including extended forms like
    /// `85.06s (0:01:25)` (`manager/src/services/argo.rs:3279`). Not a
    /// millisecond integer, and not to be "improved" into one here.
    pub duration: Option<String>,
    /// `ReportPortal` launch link.
    pub launch_id: Option<String>,
    /// The **file**-level bug reference. See this type's header.
    pub jira_key: Option<String>,
    /// The pytest node identifier - `tests/test_x.py::TestC::test_m[param]` -
    /// and `""` when the producer reported none, never `None`, for the same
    /// single-spelling-of-absent reason as [`Self::test_file`]. A non-empty
    /// value means this row is case-level rather than file-level.
    pub nodeid: String,
    /// The xfail/skip explanation, if the runner gave one. Nullable rather
    /// than `""`-defaulted because here absence *is* information
    /// (`manager/migrations/001_initial.sql:261`).
    pub reason: Option<String>,
    /// The **case**-level bug reference. See this type's header.
    pub ticket: Option<String>,
}

/// The three-outcome launch contract (`cpt-cf-qa-fr-runs-launch`; guide lines
/// 68-76). Load-bearing for both UI and CI callers, so it is an enum rather
/// than a `Run` with a nullable queue id.
///
/// `Started` boxes its [`Run`] so the two variants stay close in size
/// (`clippy::large_enum_variant`: an unboxed `Run` makes every `Queued` value
/// carry ~416 bytes for two UUIDs). The extra allocation is one per launch, on
/// a path that already does a git sync and a bundle build, so it is not worth
/// an `#[allow]`.
#[derive(Clone, Debug, PartialEq)]
pub enum LaunchOutcome {
    /// Started immediately → HTTP 200.
    Started { run: Box<Run> },
    /// Queued; it will start on its own, no further call needed → HTTP 202.
    Queued { run_id: Uuid, queue_id: Uuid },
}

impl LaunchOutcome {
    /// The launched run's id, whichever way the launch went. Every caller
    /// needs it — to poll, to link, or to record — and it is the one fact both
    /// variants share, so it is spelled once here rather than at each `match`.
    #[must_use]
    pub fn run_id(&self) -> Uuid {
        match self {
            Self::Started { run } => run.id,
            Self::Queued { run_id, .. } => *run_id,
        }
    }
}

/// A queue row as reported by the read endpoint. Column set ported from the
/// source system's `run_queue` table
/// (`manager/migrations/001_initial.sql:288-302`), with three deliberate
/// departures:
///
/// * legacy's `platform TEXT` is a platform *name*; here it is
///   [`platform_id`](Self::platform_id), a `Uuid` into qa-environments;
/// * `target_id` and `intent` are not repeated because
///   [`run_id`](Self::run_id) resolves to a [`Run`] that already carries the
///   target;
/// * `workflow_name` is likewise absent — `run_id` is the run handle, and the
///   executor's opaque reference lives on [`Run::execution_ref`]. `DESIGN.md:582`
///   does list `execution_ref` on `run_queue`, but duplicating it on the queue
///   row would give one execution two homes that can disagree.
#[derive(Clone, Debug, PartialEq)]
pub struct QueueEntry {
    pub id: Uuid,
    pub run_id: Uuid,
    pub platform_id: Uuid,
    pub run_kind: RunKind,
    pub source: RunSource,
    pub exclusive: bool,
    pub state: QueueState,
    pub error: Option<String>,
    pub enqueued_at: OffsetDateTime,
    pub dispatched_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    /// 1-based position among this platform's `queued` rows, oldest first;
    /// `None` for any other state. Computed per request over the rows that
    /// request returned, so a truncating limit understates it — use the
    /// platform-filtered call (guide lines 171-184).
    pub queue_position: Option<u32>,
    /// When the TTL sweep will expire this row. `None` unless `queued`, and
    /// `None` when `queue_ttl_seconds` is 0.
    pub ttl_expires_at: Option<OffsetDateTime>,
    /// Plain-text reason it has not started (guide line 177).
    pub blocked_by: Option<String>,
}

/// The seven queue states (guide lines 88-96; decision D1). `Dispatching`
/// and `Running` are the two that hold a claim on the platform.
///
/// **`Cancelled` has two `l`s, [`RunState::Canceled`] has one. This is
/// deliberate — do not "fix" either.** This spelling is the ported one
/// (`manager/src/services/run_queue.rs:413`) and is frozen by guide line 95;
/// the run state's one-`l` spelling is new vocabulary, since legacy has no
/// run-level cancel state and zero occurrences of `canceled`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueState {
    Queued,
    Dispatching,
    Running,
    Done,
    Failed,
    Cancelled,
    Expired,
}

impl QueueState {
    /// The persisted spelling — the seven frozen names. Sole encoder; see
    /// [`RunState::as_str`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatching => "dispatching",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
}

/// A cron schedule (Phase B).
#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    pub id: Uuid,
    pub name: String,
    pub target: RunTarget,
    pub platform_id: Option<Uuid>,
    pub branch: Option<String>,
    /// Five-field cron expression.
    pub cron: String,
    /// The stored exclusivity choice, delivered into the launch as the
    /// **launch** tier when the schedule fires (guide lines 62-65). Serialized
    /// `true` / `false` / `auto` — always written, including for `None`,
    /// because an absent value must not mean both "inherit" and "parallel"
    /// (`manager/src/services/exclusivity.rs:67-79`).
    pub exclusive_choice: Option<bool>,
    pub enabled: bool,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub parameters: Vec<RunParameter>,
    /// Whether this schedule's runs notify Slack at all.
    ///
    /// The first of the three per-schedule settings (D9). Read-only through
    /// [`Schedule`]: they are written by
    /// [`QaRunsClientV1::update_schedule_notifications`], never by a
    /// [`NewSchedule`] — see [`ScheduleNotificationSettings`] for why the two
    /// are separate payloads.
    ///
    /// [`QaRunsClientV1::update_schedule_notifications`]: crate::QaRunsClientV1::update_schedule_notifications
    pub slack_notifications_enabled: bool,
    /// Channel override; `None` means the deployment-wide default channel.
    pub slack_channel: Option<String>,
    /// The events that notify, from the six-token vocabulary in
    /// [`SLACK_NOTIFICATION_EVENTS`].
    pub slack_notification_events: Vec<String>,
    /// Latest due time this schedule has fired for.
    pub last_fired_tick: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Legacy's scheduled-run event vocabulary, **as serialized**.
///
/// Six values, and these exact spellings: `ScheduledRunNotificationEvent`
/// carries `#[serde(rename_all = "snake_case")]`
/// (`manager/src/models.rs:949-959`), so the variant names `Pending`,
/// `InProgress`, `Succeeded`, `Failed`, `Error`, `Skipped` are **not** what
/// crosses a wire or lands in a column — legacy's own test
/// `scheduled_run_event_serializes_to_snake_case` (`manager/src/models.rs:940-946`)
/// asserts `"in_progress"`, and its UI declares the union as the same six
/// lowercase tokens (`manager-ui/src/api/types.ts:761-767`).
///
/// The distinction is load-bearing rather than pedantic: `InProgress`
/// lowercased is `inprogress`, which legacy's own
/// `ScheduledRunNotificationEvent::parse` does not recognise, so a settings row
/// written with the variant spelling would be a subscription that silently never
/// fires.
///
/// Declared in the SDK rather than in the gear because **qa-insights parses
/// these strings** (Task 36) and the two gears must agree on the set. It is the
/// closed set the service validates against, which is what preserves legacy's
/// behaviour: its form deserializes straight into the enum, so an unknown event
/// name is refused at the boundary there too.
pub const SLACK_NOTIFICATION_EVENTS: [&str; 6] = [
    "pending",
    "in_progress",
    "succeeded",
    "failed",
    "error",
    "skipped",
];

/// The three per-schedule Slack settings, ported from legacy's `CronWorkflow`
/// annotations (`manager/src/routes/schedules.rs::api_update_notifications`,
/// the handler at `schedules.rs:833`).
///
/// qa-insights reads these over the SDK when a scheduled run changes status; it
/// does not own them, because a schedule is a qa-runs aggregate and a
/// notification setting on it is a field, not a separate entity.
///
/// # Why this is not three more fields on [`NewSchedule`]
///
/// Legacy edits them through their own endpoint, which carries every other field
/// forward untouched — *"this endpoint edits Slack settings only, so a schedule
/// pinned to exclusive (or to parallel) must come back pinned the same way"*
/// (`manager/src/routes/schedules.rs:854-856`). Keeping them off the replace
/// payload is how that is preserved **structurally** here rather than by a
/// caller remembering to re-send them, which is what legacy's UI has to do
/// (`manager-ui/src/components/schedules/CreateScheduleDialog.tsx:305-309`
/// seeds the create form from the schedule being edited, precisely so a full
/// edit does not wipe them).
///
/// # The `slack_` prefix is kept, against `clippy::struct_field_names`
///
/// The lint's advice — drop the common prefix, giving `enabled`, `channel`,
/// `events` — is wrong here for two reasons, and the allow is scoped to this
/// type rather than raised crate-wide. First, [`Schedule::enabled`] already
/// exists and means something entirely different: whether the schedule fires at
/// all. A bare `enabled` beside it, in a payload that edits the same aggregate,
/// is the single most confusable pair this contract could ship. Second, these
/// are the source system's own field names
/// (`CreateScheduleForm::{slack_notifications_enabled, slack_channel,
/// slack_notification_events}`, `manager/src/models.rs:189-194`), and a reader
/// holding both trees open should not have to translate. Slack is also not the
/// only notification transport a schedule could ever grow, which is the third,
/// weaker reason.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[allow(clippy::struct_field_names)]
pub struct ScheduleNotificationSettings {
    pub slack_enabled: bool,
    pub slack_channel: Option<String>,
    /// Legacy's event vocabulary: `pending`, `in_progress`, `succeeded`,
    /// `failed`, `error`, `skipped` (`ScheduledRunNotificationEvent`,
    /// `manager/src/models.rs:949-959`) — the **serialized** spellings, listed
    /// in [`SLACK_NOTIFICATION_EVENTS`]. Stored as strings rather than an enum
    /// because the SDK is `serde`-free and the column is JSON; the routing core
    /// in qa-insights parses them (Task 36).
    pub slack_events: Vec<String>,
}

/// Creation/replace payload for a schedule.
#[derive(Clone, Debug, PartialEq)]
pub struct NewSchedule {
    pub name: String,
    pub target: RunTarget,
    pub platform_id: Option<Uuid>,
    pub branch: Option<String>,
    pub cron: String,
    pub exclusive_choice: Option<bool>,
    pub enabled: bool,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub parameters: Vec<RunParameter>,
}
