//! Transport-agnostic models for the qa-runs contract.
//!
//! A run has exactly one durable representation: the `qa_runs` row
//! (`cpt-cf-qa-principle-db-first-state`). Nothing here is reconstructed from an
//! execution backend's objects, which are garbage-collected on their own TTL
//! while reruns happen much later, so the row carries the full field set —
//! including the three an execution object could not hold anyway: `created_at`,
//! `run_parameters` and the log reference.

use qa_catalog_sdk::Exclusivity;
use time::OffsetDateTime;
use uuid::Uuid;

/// What a launch targets.
///
/// Three of the four kinds are the *test-executing* ones: a discovered plan, a
/// single test file within one, and an operator-assembled custom plan. There is
/// no second plan reader — every plan is discovered the one way.
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
    /// considers only that one file, and no tag filter applies to it.
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
    /// counts to `collect_url` rather than running anything: it enumerates exact
    /// test cases with parametrize expanded, which is what feeds the analytics
    /// "expected cases" number.
    ///
    /// # Where the third input lives, and why it is not a field here
    ///
    /// A collect launch is defined by three values — repository, **branch**, and
    /// report URL — and only two of them are on this variant. The branch travels
    /// on [`LaunchRequest::branch`], which every other kind already uses and
    /// which `service::launch`'s rule-1 chain already resolves; rule 6 then
    /// records it as the run's `test_version`. A second `branch` here would be a
    /// second place for the same value, and the two could disagree.
    ///
    /// # The URL is supplied, never built
    ///
    /// qa-runs does not build the URL, because it does not own the route it
    /// points at: qa-insights owns the collect report endpoint, so it hands the
    /// URL in and the runner posts wherever it is told. That is why this is a
    /// `String` on the request rather than something qa-runs derives, and it is
    /// what keeps the `VHP_COLLECT_URL` contract intact across the gear split.
    ///
    /// A blank URL is legal and means "collect but report nowhere":
    /// `VHP_COLLECT_URL` is pushed only for a non-blank one.
    Collect { repo_id: Uuid, collect_url: String },
}

impl RunTarget {
    /// Stable discriminant recorded on the run and the queue row.
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
    /// # `"collect"` is a run **kind**, not a run source
    ///
    /// A collect run is a distinct kind rather than a plan run wearing a source
    /// label, because the split is what lets `service::launch` route it past
    /// admission by type instead of by inspecting a string. Its [`RunSource`]
    /// stays `Manual`: who asked for it is a separate question from what it is.
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

/// Who asked. Normalized to exactly these two: anything that is not `scheduled`
/// is `manual`.
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
/// No `secure` flag, unlike a pipeline variable. That flag belongs to the
/// *global* pipeline-variables setting, not to a launch: run parameters are
/// stored in plain text and are never secret
/// (`cpt-cf-qa-fr-runs-params`).
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
    /// `None` = a run with no target environment. Such a run is never queued
    /// and never blocks anything (guide line 76).
    pub environment_id: Option<Uuid>,
    /// Test-content branch. Resolution order when absent: the environment's
    /// `default_branch`, then the repository's.
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
    /// `timeout_seconds`, then to the configured `default_timeout_seconds`.
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
/// different tables. The one-`l` spelling belongs to the run state and the
/// two-`l` spelling to the queue row's `state` column, which is frozen by guide
/// line 95.
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
    /// and nothing said what a TTL-expired run becomes. Reusing `Canceled`
    /// would make the TTL sweep
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
    pub environment_id: Option<Uuid>,
    /// Branch actually resolved and executed against; also recorded as the
    /// run's test version.
    pub test_version: Option<String>,
    /// The environment's product version, **snapshotted at launch** and
    /// persisted rather than re-derived. Re-deriving it from the environment
    /// would let an upgrade silently change a queued run's or a re-run's
    /// `APP_VERSION`, breaking reproducibility and the runner contract
    /// (`cpt-cf-qa-fr-runner-contract`). Feeds the `APP_VERSION` static runner
    /// variable in tier 1 of environment assembly.
    pub app_version: Option<String>,
    /// The environment's product build, snapshotted at launch alongside
    /// [`Self::app_version`] — both come from the same environment lookup.
    /// Feeds `APP_BUILD`.
    pub app_build: Option<String>,
    pub state: RunState,
    /// The exclusivity decision actually made, plus which tier made it.
    ///
    /// **Both halves are persisted**, not just the boolean. An operator must be
    /// able to tell *why* a run became exclusive, and a log line cannot answer
    /// that once the log has rotated.
    ///
    /// **Named `resolved_exclusive`, not `exclusive`.** This is the *resolved*
    /// decision, while [`LaunchRequest::exclusive`] is a closed three-state
    /// [`Exclusivity`] tri-state request. Under the short name the obvious
    /// re-run transcription — `exclusive: if run.exclusive { Exclusivity::Exclusive }
    /// else { Exclusivity::Shared }` — type-checked and was **wrong**: it
    /// pins a stored `false` onto the launch tier as `Exclusivity::Shared`,
    /// which outranks everything and suppresses a `TEST_META`/`plan.yaml`
    /// declaration added since — relaunching a since-marked-destructive test
    /// in parallel on a shared environment. A prose comment cannot guard it; the
    /// distinct name is this port's structural replacement for it. The correct
    /// transcription is `if run.resolved_exclusive { Exclusivity::Exclusive }
    /// else { Exclusivity::Inherit }` — **never** `Exclusivity::Shared`, so a
    /// since-marked-destructive test is re-resolved rather than replayed
    /// parallel (`domain::service::runs::replay` is where this is done; see
    /// `replay_inherits_exclusivity_upward_only`).
    /// DESIGN §3.8 already names the column `resolved_exclusive` — the SDK
    /// field was the outlier. [`QueueEntry::exclusive`] keeps the short name:
    /// DESIGN's `run_queue` column list spells it that way, and it is a
    /// different table.
    pub resolved_exclusive: bool,
    pub exclusive_tier: ExclusiveTier,
    /// Whether this run is a validation run — `plan.yaml`'s `validation` bool
    /// OR'd with a case-insensitive `validation` tag.
    ///
    /// Set at launch and read back here: it is a property of the run rather than
    /// of the execution.
    pub is_validation: bool,
    pub parameters: Vec<RunParameter>,
    /// The tag filter this run was launched with.
    ///
    /// Recorded on the run. Persisting the filter is
    /// what makes `rerun`'s "with its original parameters"
    /// (`cpt-cf-qa-fr-runs-cancel-rerun`) actually hold.
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub source: RunSource,
    pub schedule_id: Option<Uuid>,
    /// Bundle descriptors backing this run, one per repository group — one
    /// execution node per group.
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
    /// property qa-insights relies on when it collects case counts by
    /// enumerating cases rather than parsing markers out of run logs (DESIGN
    /// §3.5).
    ///
    /// Log text is **not** an inline column on the run row: fetching it for
    /// every row of a full history is an out-of-memory hazard, which is why
    /// `qa_run_logs` is a separate table.
    pub log_storage_ref: Option<String>,
    /// Deadline the control plane enforces (`cpt-cf-qa-fr-runs-timeout`).
    pub timeout_at: Option<OffsetDateTime>,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    /// Terminal failure reason, operator-facing. Carries the execution
    /// backend's own status message.
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Which tier supplied a run's exclusivity flag — including the distinction
/// between `TestMeta` with a `false` answer ("files were read and all said
/// parallel") and `Default` ("nobody had an opinion"), which the
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
/// Counts only — **not** the run record.
/// [`QaRunsClientV1::get_run_result`](crate::QaRunsClientV1::get_run_result)
/// returns these five numbers; `get_run` returns the [`Run`].
///
/// **There is no `error` counter, and adding one as a sixth field would be a
/// silent regression.** `failed` is the *failed-or-errored* count: `FAILED` and
/// `ERROR` are distinct per-test statuses but are folded together in every
/// aggregate that produces these numbers —
/// `COUNT(*) FILTER (WHERE tr.status IN ('FAILED', 'ERROR')) AS failed`. That
/// matters here because `domain::state_machine::derive_terminal_state` reads
/// `failed` to decide a run's terminal state, on the rule
/// `has_failed = FAILED || ERROR`.
/// `skipped` does not vote on that verdict — ratified by the product owner on
/// 2026-08-28 (see
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
/// uniqueness tuple the write path maintains by delete-then-insert. Exposing
/// the surrogate id would
/// invite qa-insights to store a foreign key into another gear's table.
///
/// # Two ticket columns, deliberately
///
/// [`Self::jira_key`] is the **file**-level bug reference on `qa_test_results`;
/// [`Self::ticket`] is the **case**-level one on `qa_test_case_results`.
/// Analytics renders only the latter as `AnalyticsListItem::case_tickets`.
/// Do not collapse them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunTestResult {
    pub run_id: Uuid,
    /// `""` when the producer reported no file, never `None` - the column is
    /// `NOT NULL DEFAULT ''` so "absent" has exactly one spelling and no
    /// reader needs a `COALESCE`.
    pub test_file: String,
    pub test_name: String,
    /// Open set, uppercase - `PASSED` | `FAILED` | `ERROR` | `SKIPPED` |
    /// `PENDING` | `RUNNING` | `XFAIL` | `XPASS` today, and **not exhaustive**.
    /// There is deliberately no enum: the column carries the runner's own text,
    /// so a ninth value is a runner change rather than corruption, and a
    /// consumer that rejected it would drop a real result.
    pub status: String,
    /// The runner's duration text **verbatim**, including extended forms like
    /// `85.06s (0:01:25)`. Not a millisecond integer, and not to be "improved"
    /// into one here.
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

/// A queue row as reported by the read endpoint. Three column choices are worth
/// stating:
///
/// * the environment is [`environment_id`](Self::environment_id), a `Uuid` into
///   qa-environments, not a name;
/// * `target_id` and `intent` are not repeated because
///   [`run_id`](Self::run_id) resolves to a [`Run`] that already carries the
///   target;
/// * `workflow_name` is likewise absent — `run_id` is the run handle, and the
///   executor's opaque reference lives on [`Run::execution_ref`]. DESIGN §3.8
///   does list `execution_ref` on `run_queue`, but duplicating it on the queue
///   row would give one execution two homes that can disagree.
#[derive(Clone, Debug, PartialEq)]
pub struct QueueEntry {
    pub id: Uuid,
    pub run_id: Uuid,
    pub environment_id: Uuid,
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
/// deliberate — do not "fix" either.** This spelling is the queue column's and
/// is frozen by guide line 95; the run state has its own one-`l` spelling on a
/// different table.
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
    pub environment_id: Option<Uuid>,
    pub branch: Option<String>,
    /// Five-field cron expression.
    pub cron: String,
    /// The stored exclusivity choice, delivered into the launch as the
    /// **launch** tier when the schedule fires (guide lines 62-65). Serialized
    /// `true` / `false` / `auto` — always written, including for `None`,
    /// because an absent value must not mean both "inherit" and "parallel".
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

/// The scheduled-run event vocabulary, **as serialized**.
///
/// Six values, and these exact spellings. The snake_case spellings are what
/// crosses a wire and lands in a column — never a CamelCase variant name.
///
/// The distinction is load-bearing rather than pedantic: `InProgress`
/// lowercased is `inprogress`, which no parser here recognises, so a settings
/// row written with a variant spelling would be a subscription that silently
/// never fires.
///
/// Declared in the SDK rather than in the gear because **qa-insights parses
/// these strings** (Task 36) and the two gears must agree on the set. It is the
/// closed set the service validates against, so an unknown event name is refused
/// at the boundary.
pub const SLACK_NOTIFICATION_EVENTS: [&str; 6] = [
    "pending",
    "in_progress",
    "succeeded",
    "failed",
    "error",
    "skipped",
];

/// The three per-schedule Slack settings.
///
/// qa-insights reads these over the SDK when a scheduled run changes status; it
/// does not own them, because a schedule is a qa-runs aggregate and a
/// notification setting on it is a field, not a separate entity.
///
/// # Why this is not three more fields on [`NewSchedule`]
///
/// They are edited through their own endpoint, which carries every other field
/// forward untouched: that endpoint edits Slack settings only, so a schedule
/// pinned to exclusive (or to parallel) must come back pinned the same way.
/// Keeping them off the replace payload is how that is preserved
/// **structurally**, rather than by a caller remembering to re-send them.
///
/// # The `slack_` prefix is kept, against `clippy::struct_field_names`
///
/// The lint's advice — drop the common prefix, giving `enabled`, `channel`,
/// `events` — is wrong here for two reasons, and the allow is scoped to this
/// type rather than raised crate-wide. First, [`Schedule::enabled`] already
/// exists and means something entirely different: whether the schedule fires at
/// all. A bare `enabled` beside it, in a payload that edits the same aggregate,
/// is the single most confusable pair this contract could ship. Second, Slack is
/// not the only notification transport a schedule could ever grow, so the
/// prefix is what leaves room for a second one.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[allow(clippy::struct_field_names)]
pub struct ScheduleNotificationSettings {
    pub slack_enabled: bool,
    pub slack_channel: Option<String>,
    /// The event vocabulary: `pending`, `in_progress`, `succeeded`, `failed`,
    /// `error`, `skipped` — the **serialized** spellings, listed
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
    pub environment_id: Option<Uuid>,
    pub branch: Option<String>,
    pub cron: String,
    pub exclusive_choice: Option<bool>,
    pub enabled: bool,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub parameters: Vec<RunParameter>,
}
