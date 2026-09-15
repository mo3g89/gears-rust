//! Transport-agnostic models for the qa-insights contract.
//!
//! Four conventions recur across the types below, so they are stated once here
//! rather than repeated on each of them.
//!
//! # 1. A plan is `(repo_id, plan_path)`, never a `plan_id: Uuid`
//!
//! A plan is not persisted: `qa_catalog_sdk::Plan` is materialized on read,
//! there is no plans table, and no plan UUID is ever minted — so there would be
//! nothing to put in such a field. The whole subsystem uses the pair instead:
//! `qa_catalog_sdk::UniverseTest`, `qa_catalog_sdk::CustomPlanEntry` and
//! `qa_runs_sdk::RunTarget::Plan { repo_id, path }` are the same identity again.
//!
//! It is also lossless, which a slug derived from the path would not be.
//! `plan_path` is the `plan.yaml` path within the repository's content root,
//! spelled exactly as [`UniverseTest::plan_path`] spells it, because the
//! analytics join and the bug registry key on the same plans.
//!
//! [`UniverseTest::plan_path`]: https://docs.rs/qa-catalog-sdk
//!
//! # 2. An environment is a `Uuid`, never a name
//!
//! Environment identity is a `Uuid` into qa-environments everywhere it appears,
//! so [`TestResultRecord::environment_id`] and [`JiraBug::environment_id`] carry one.
//! A name is not an identity: it can be edited, and two deployments can reuse
//! one.
//!
//! # 3. Counts are `u64`, durations are the runner's text
//!
//! Aggregate counters are `u64` — `usize` is not a wire width. Per-file case
//! counts stay `u32`, matching
//! `qa_catalog_sdk::UniverseTest::static_case_count` so the two sides of the
//! expected-cases comparison have one type. Durations stay `Option<String>`
//! because the runner emits text such as `85.06s (0:01:25)`, and
//! `qa_runs_sdk::RunTestResult::duration` carries it verbatim.
//!
//! # 4. Status strings are open sets, not enums
//!
//! Test status, bug status and notification outcome are all producer text.
//! `qa_runs_sdk::RunTestResult::status` documents the reasoning and this crate
//! does not second-guess it: a consumer that rejected an unknown value would
//! drop a real result. The one exception is [`SavedViewScope`], which is
//! validated against a closed two-value set at the boundary.

use time::OffsetDateTime;
use uuid::Uuid;

// ==================== Ingested results ====================

/// One file-level test outcome within a run.
///
/// # Four shape decisions worth stating
///
/// 1. **There is no per-test `logs` field, and nothing could fill one.** This
///    gear's only source of test outcomes is qa-runs, and
///    `qa_runs_sdk::RunTestResult` carries no per-test log slice — a run's log is
///    whole-run and reachable only as an SSE stream. A field no writer can set
///    is the "field nothing could set" shape this subsystem has rejected before.
///    **This is a real gap**, not a tidy-up: closing it needs a per-test log
///    excerpt on qa-runs' event and SDK first, at which point it is one additive
///    field here.
/// 2. **Seven columns are denormalized from the run** — `product_version`,
///    `app_build`, `environment_id`, `repo_id`/`plan_path`, `branch`,
///    `run_finished_at`, `run_created_at`. Reading them live would be a
///    cross-gear call on every analytics query, which
///    `cpt-cf-qa-principle-async-insights` forbids on a hot path. Copying is safe
///    because a finished run's environment, version, build and branch never
///    change afterwards.
/// 3. **`test_file` is `String`, not `Option<String>`.**
///    `qa_runs_sdk::RunTestResult::test_file` already collapses absent to `""` so
///    that "absent" has exactly one spelling and no query needs a `COALESCE`;
///    the ingest side keeps that.
/// 4. **`ingest_ordinal` is a stored column and deliberately not a field here.**
///    `qa_test_results.ingest_ordinal` carries the row's position within the
///    batch that wrote it and serves as a stable ordering tiebreak. It is a
///    persistence detail, in the same class as `updated_at` — no consumer reads
///    it, and publishing it would invite one to. The repository derives it from
///    the batch's own order, so there is nothing for a caller to supply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestResultRecord {
    pub id: Uuid,
    /// The qa-runs run this outcome belongs to.
    pub run_id: Uuid,
    /// Test-file path relative to the repository content root; `""` when the
    /// producer reported none. See decision 3.
    pub test_file: String,
    pub test_name: String,
    /// Open, uppercase set — `PASSED` | `FAILED` | `ERROR` | `SKIPPED` |
    /// `PENDING` | `RUNNING` | `XFAIL` | `XPASS` today, deliberately not an
    /// enum. See this module's note 4.
    pub status: String,
    /// The runner's duration text verbatim; see this module's note 3.
    pub duration: Option<String>,
    /// `ReportPortal` launch link.
    pub launch_id: Option<String>,
    /// The **file**-level bug reference, as the runner reported it. Distinct
    /// from [`TestCaseResultRecord::ticket`], which is case-level, and from
    /// [`JiraBug`], which is this gear's own registry.
    pub jira_key: Option<String>,
    /// Denormalized from the run: the product version under test.
    pub product_version: Option<String>,
    /// Denormalized from the run: the build under test.
    ///
    /// **Not a duplicate of [`Self::product_version`].** That one is the
    /// analytics *filter*; this one is the analytics *projection* — the value
    /// `build_last_run_build_distribution` groups by and `api_build_tests`
    /// filters on. `None` means the run named no build, which a consumer renders
    /// as `"unknown"` rather than storing.
    pub app_build: Option<String>,
    /// Denormalized from the run; see this module's note 2.
    pub environment_id: Option<Uuid>,
    /// Denormalized from the run: the repository half of the plan identity.
    /// `None` for a target that has no plan (a custom-plan or collect run).
    /// See this module's note 1.
    pub repo_id: Option<Uuid>,
    /// Denormalized from the run: the `plan.yaml` path half of the plan
    /// identity. See this module's note 1.
    pub plan_path: Option<String>,
    /// Denormalized from the run: the branch the tests came from. The
    /// effective branch is resolved on ingest, so no reader has to fall back
    /// from one column to another.
    pub branch: Option<String>,
    /// Denormalized from the run. `None` while the run is still going —
    /// `RUNNING` and `PENDING` rows are written too.
    pub run_finished_at: Option<OffsetDateTime>,
    /// Denormalized from the run: the run's own creation instant
    /// (`qa_runs_sdk::Run::created_at`).
    ///
    /// **The fallback for an unfinished run, and the reason it is a field
    /// rather than a reuse of [`Self::created_at`].** The two are different
    /// instants:
    ///
    /// | | what it is | does it move? |
    /// |---|---|---|
    /// | [`Self::created_at`] | when *this row* was written | yes — ingest is delete-then-insert per run, re-run on every result event, so it is reset to `now` for the whole life of the run |
    /// | this field | when the *run* was created | no |
    ///
    /// Added by Task 21b, whose dashboard KPI window counts unfinished runs by
    /// design — the query carries no phase predicate. Windowing that on
    /// [`Self::created_at`] kept a run in progress for three days inside "the
    /// last 24 hours" for all three, so its failures never left
    /// `DashboardStats::failed_24h_count` and its cards never left
    /// `DashboardStats::failed_recent`.
    ///
    /// `None` if a run ever reports no creation instant. `qa_runs_sdk::Run` makes
    /// it non-optional today.
    pub run_created_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

/// One test *function* outcome within a run, grouped under its file.
///
/// Per-function test case outcomes — xfail/xpass/skip/pass/fail — one row per
/// test function per run, grouped under a file ([`Self::test_file`]). This is
/// what lets analytics aggregate per-case and not only per-file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestCaseResultRecord {
    pub id: Uuid,
    pub run_id: Uuid,
    /// The owning file. `NOT NULL` with no default, unlike
    /// [`TestResultRecord::test_file`] — a case row always knows its file.
    pub test_file: String,
    /// The pytest node identifier — `tests/test_x.py::TestC::test_m[param]` —
    /// and `""` when the producer reported none: the column is
    /// `NOT NULL DEFAULT ''`, matching `qa_runs_sdk::RunTestResult::nodeid`.
    pub nodeid: String,
    /// The test function's name. The column is `name`, not `test_name`, so the
    /// two result tables stay visibly different shapes.
    pub name: String,
    /// Open set; see this module's note 4. Case-level statuses add `XFAIL` and
    /// `XPASS` to the file-level vocabulary.
    pub status: String,
    pub duration: Option<String>,
    /// The xfail/skip explanation, if the runner gave one. Nullable rather than
    /// `""`-defaulted because here absence *is* information.
    pub reason: Option<String>,
    /// The **case**-level bug reference; see [`TestResultRecord::jira_key`].
    pub ticket: Option<String>,
    pub created_at: OffsetDateTime,
}

/// The collect job's exact expected-case count for one file on one branch.
///
/// Exact expected test-case counts per file, produced by the collect job
/// (`pytest --collect-only`, parametrize expanded). Keyed by repository, branch
/// and file so analytics can show an exact "expected cases" number without a
/// full run.
///
/// This value **wins over** `qa_catalog_sdk::UniverseTest::static_case_count`
/// wherever it exists, which is why the two share the `u32` width.
///
/// # No surrogate `id`
///
/// The natural key is the composite `(repo_id, branch, test_file)`, and nothing
/// addresses a collect row any other way — the collect report is an upsert on
/// exactly that triple. The
/// storage table still gets the platform's mandatory `id UUID` primary key
/// (Task 10 demotes the composite to a unique index), but surfacing a surrogate
/// key on the contract would invite a caller to hold one, and there is no
/// operation that takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectCount {
    pub repo_id: Uuid,
    pub branch: String,
    pub test_file: String,
    pub case_count: u32,
    pub collected_at: OffsetDateTime,
}

// ==================== Saved analytics views ====================

/// What an [`SavedView`] is scoped to.
///
/// Serialized `"all"` / `"plan"` and validated against exactly those two
/// spellings at the boundary — anything else is a 400. A closed enum, and the
/// one status-like field in this crate that is not open producer text, because
/// the values are the platform's own rather than a runner's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavedViewScope {
    /// Spans the whole universe; [`SavedView::repo_id`] and
    /// [`SavedView::plan_path`] are `None`.
    All,
    /// Scoped to one plan; the plan identity is required.
    Plan,
}

impl SavedViewScope {
    /// The stored and wire spelling — `"all"` or `"plan"`.
    ///
    /// The variant names are *not* what lands in the column, and getting that
    /// wrong makes a saved view unreadable by its own list query.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Plan => "plan",
        }
    }
}

/// A stored analytics filter set.
///
/// # The uniqueness rule this type has to keep representable
///
/// A view is unique on `(owner_id, scope, plan identity, name)`, where an absent
/// plan identity coalesces to the empty string. That coalescing is the whole
/// point: it lets one owner keep a global view and a plan-scoped view of the
/// *same name* without collision. Storage materializes the coalesced value as
/// its own column, because `SQLite` and `MySQL` do not both support functional
/// indexes.
///
/// # `query_json` is opaque text
///
/// Nothing inspects it: every statement that touches the column binds or returns
/// it whole. This crate is `serde`-free, so it is a `String` holding the JSON
/// document verbatim, and parsing it here would be net-new behaviour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedView {
    pub id: Uuid,
    /// The principal that owns the view: the tenant's principal id, taken from
    /// the request identity and never from the body.
    pub owner_id: Uuid,
    pub scope: SavedViewScope,
    /// Repository half of the plan identity; `Some` exactly when
    /// [`Self::scope`] is [`SavedViewScope::Plan`]. See this module's note 1.
    pub repo_id: Option<Uuid>,
    /// `plan.yaml` path half of the plan identity; see this module's note 1.
    pub plan_path: Option<String>,
    pub name: String,
    /// The filter set, verbatim JSON. See this type's header.
    pub query_json: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Creation/replacement payload for a [`SavedView`].
///
/// Carries exactly `name`, `scope`, the plan identity and `query_json` — the
/// owner comes from the request identity and never from the body, and the
/// timestamps are the server's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewSavedView {
    pub scope: SavedViewScope,
    /// Required when [`Self::scope`] is [`SavedViewScope::Plan`]; a
    /// plan-scoped view naming no plan is refused with a 400.
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    pub name: String,
    pub query_json: String,
}

// ==================== JIRA bug registry ====================

/// A tracked bug against a test.
///
/// The plan identity is the `(repo_id, plan_path)` pair (this module's note 1,
/// which is written mostly about this table) and the environment is a `Uuid`
/// (note 2).
///
/// # There is no `auto_rerun` field
///
/// Auto-rerun is a *global* switch — [`JiraPollerConfig::auto_rerun_on_resolve`]
/// — not a per-bug one. A per-bug flag nothing reads would be a field no writer
/// sets and no consumer honours.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JiraBug {
    pub id: Uuid,
    /// The issue key, e.g. `VHP-2618`. Unique per tenant.
    pub jira_key: String,
    /// The test this bug is filed against, by display name — the same name
    /// [`TestResultRecord::test_name`] carries, which is what makes the skip
    /// list matchable by the runner.
    pub test_name: String,
    /// Repository half of the plan identity; see this module's note 1.
    pub repo_id: Uuid,
    /// `plan.yaml` path half of the plan identity; see this module's note 1.
    pub plan_path: String,
    /// Optional qualifier: the product version the failure was seen on.
    pub app_version: Option<String>,
    /// Optional qualifier: the environment the failure was seen on. See this
    /// module's note 2.
    pub environment_id: Option<Uuid>,
    /// `Open` until the poller sees the issue reach a `done` status category,
    /// at which point it becomes `Resolved`. Open set — the value can also come
    /// back from the JIRA API — so it is a `String`; see this module's note 4.
    pub status: String,
    pub summary: String,
    pub created_at: OffsetDateTime,
    /// Set when [`Self::status`] becomes `Resolved`; `None` while open.
    pub resolved_at: Option<OffsetDateTime>,
}

/// Filing payload for a [`JiraBug`].
///
/// The four server-owned fields are absent: `id` and `created_at` are the
/// server's, `status` starts at the column default `'Open'`, and `resolved_at`
/// is the poller's to write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewJiraBug {
    pub jira_key: String,
    pub test_name: String,
    pub repo_id: Uuid,
    pub plan_path: String,
    pub app_version: Option<String>,
    pub environment_id: Option<Uuid>,
    pub summary: String,
}

/// One entry of the runner's skip list.
///
/// # What the runner actually receives
///
/// Not this type — a single string, assembled at launch from the open bugs of
/// the plan being launched and joined with commas:
///
/// ```text
/// bugs.iter().map(|b| format!("{}:{}", b.test_name, b.jira_key)).join(",")
/// ```
///
/// which reaches the runner as `SKIP_TESTS_WITH_BUGS`. The rendering belongs to
/// qa-runs' environment assembly, so the contract carries the pairs and lets the
/// consumer render them.
///
/// # An empty list means the variable is absent, not empty
///
/// The emptiness is guarded twice: no open bugs yields `None` rather than
/// `Some("")`, and the environment push is additionally gated on the rendered
/// value being non-empty. So a run with no open bugs sees **no**
/// `SKIP_TESTS_WITH_BUGS` variable at all. A test repository that branches on
/// `os.environ.get("SKIP_TESTS_WITH_BUGS")` can tell the difference between an
/// absent variable and an empty one, which is why this is recorded on the
/// contract rather than left to the implementer (plan Task 34, Step 0).
///
/// `SKIP_TESTS_WITH_BUGS` is also one of the eleven names in
/// `qa_environments_sdk::RESERVED_VARIABLE_NAMES`, so no operator variable can
/// overwrite the assembled value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkipListEntry {
    /// The test's display name, exactly as the runner matches it.
    pub test_name: String,
    /// The open bug's key.
    pub jira_key: String,
}

// ==================== Settings ====================

/// JIRA connection settings.
///
/// Stored as the `settings` row keyed `jira`.
///
/// # `api_token` is a credstore reference, never token material
///
/// **The token never lands in a gear table.** It lives in credstore and this
/// field holds only the reference. The field is *named* for the reference rather
/// than merely typed as a `String`, so that a `String` holding an actual token
/// cannot be assigned to it by accident — the same shape
/// `qa_environments_sdk::Environment::kubeconfig_credstore_ref` uses.
///
/// One of **two** credentials stored this way, the other being
/// [`NotificationConfig::slack_webhook_credstore_ref`]: a Slack incoming-webhook
/// URL *is* the bearer secret, so the same argument applies to it unchanged
/// (decided 2026-08-20).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JiraConfig {
    /// Base URL of the JIRA instance; a trailing `/` is trimmed at use.
    pub url: String,
    pub project_key: String,
    /// The account the token belongs to; JIRA basic auth is `email:token`.
    pub email: String,
    /// Credstore reference to the API token. **Never the token.** See this
    /// type's header.
    pub api_token_credstore_ref: String,
    /// `None` uses the project's default issue type.
    pub issue_type: Option<String>,
    /// A missing or disabled config short-circuits the poller silently.
    pub enabled: bool,
}

/// JIRA poller cadence and the auto-rerun switch.
///
/// Stored as the `settings` row keyed `jira_poller`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JiraPollerConfig {
    /// Clamped to a minimum of 1, and the loop **sleeps first**.
    pub poll_interval_seconds: u64,
    /// Global gate on relaunching a run when a bug resolves. A rerun needs
    /// *both* this and a new build; the resolution itself is recorded either
    /// way, so turning this off does not stop bugs closing.
    pub auto_rerun_on_resolve: bool,
}

impl Default for JiraPollerConfig {
    /// 300 seconds and auto-rerun **on**. Spelled out rather than derived
    /// because `derive(Default)` would silently give 0 and `false`, which is
    /// both a hot loop and a behaviour change.
    fn default() -> Self {
        Self {
            poll_interval_seconds: 300,
            auto_rerun_on_resolve: true,
        }
    }
}

/// One Slack Block Kit template for one scheduled-run status.
///
/// `body` carries a `message` serde alias on the wire; the alias is a wire
/// concern and belongs to the gear's REST DTO, not here.
///
/// The default *text* of each template belongs to the renderer and is not
/// carried in this crate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScheduledRunSlackTemplate {
    pub enabled: bool,
    pub status_icon: Option<String>,
    pub header: Option<String>,
    pub summary: Option<String>,
    pub results: Option<String>,
    pub body: Option<String>,
    pub footer: Option<String>,
}

/// Slack templates grouped by scheduled-run status.
///
/// The six field names are the six scheduled-run events, and they are the same
/// six tokens as `qa_runs_sdk::SLACK_NOTIFICATION_EVENTS` — `pending`,
/// `in_progress`, `succeeded`, `failed`, `error`, `skipped`.
///
/// # Why there is no event enum in this crate
///
/// qa-runs already owns that vocabulary and declares it in **its** SDK
/// precisely so both gears agree on the set, saying so in the constant's own
/// doc. Minting a second enum here would be the two-lists-that-must-stay-
/// identical drift hazard this subsystem has found repeatedly. The gear crate
/// depends on `qa-runs-sdk` (plan Task 9) and resolves tokens through
/// [`Self::template_for`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScheduledRunSlackTemplates {
    pub pending: ScheduledRunSlackTemplate,
    pub in_progress: ScheduledRunSlackTemplate,
    pub succeeded: ScheduledRunSlackTemplate,
    pub failed: ScheduledRunSlackTemplate,
    pub error: ScheduledRunSlackTemplate,
    pub skipped: ScheduledRunSlackTemplate,
}

impl ScheduledRunSlackTemplates {
    /// The template for one event token, or `None` for a token outside the set.
    ///
    /// This takes the serialized token rather than an enum, so it is partial.
    /// Matching on the **`snake_case` spelling** is load-bearing: `InProgress`
    /// lowercased is `inprogress`, which no parser here recognises, and a
    /// settings row written that way is a subscription that silently never
    /// fires.
    #[must_use]
    pub fn template_for(&self, event: &str) -> Option<&ScheduledRunSlackTemplate> {
        match event {
            "pending" => Some(&self.pending),
            "in_progress" => Some(&self.in_progress),
            "succeeded" => Some(&self.succeeded),
            "failed" => Some(&self.failed),
            "error" => Some(&self.error),
            "skipped" => Some(&self.skipped),
            _ => None,
        }
    }
}

/// Slack and email notification settings.
///
/// Stored as the `settings` row keyed `notifications`, and named for the
/// `qa_notification_config` table it lands in.
///
/// # Which field gates which event
///
/// Every boolean here is a routing gate; `notify::routing` owns the mapping.
/// Two are worth flagging:
/// [`Self::run_queue_queued_slack_enabled`] is *off* by default because a busy
/// environment produces a lot of queue events, and the companion `expired` event
/// has **no toggle at all** — it is the one that stops a run disappearing
/// silently.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "seven independent routing gates; grouping them into sub-structs \
              would change the stored settings shape"
)]
pub struct NotificationConfig {
    /// Credstore reference to the Slack incoming webhook. **Never the URL.**
    ///
    /// The second of this gear's two credentials held by reference. A Slack
    /// incoming-webhook URL is not an address that happens to be secret —
    /// possession of it *is* the authorization to post, exactly like
    /// [`JiraConfig::api_token_credstore_ref`] — so it gets the same treatment,
    /// and the same naming-not-retyping so a raw URL cannot be assigned by
    /// accident.
    ///
    /// **Decided 2026-08-20.**
    pub slack_webhook_credstore_ref: String,
    pub slack_channel: String,
    /// Base URL used to build run links in rendered messages.
    pub manager_ui_base_url: String,
    pub slack_enabled: bool,
    /// Defaults to `true` — the only notify gate that starts on.
    pub notify_on_failure: bool,
    pub notify_on_success: bool,
    pub notify_on_schedule_completion: bool,
    pub scheduled_run_slack_enabled: bool,
    pub scheduled_run_slack_templates: ScheduledRunSlackTemplates,
    /// Announce a run being *queued*. Off by default; see this type's header.
    pub run_queue_queued_slack_enabled: bool,
    pub email_smtp_host: String,
    /// Defaults to 587.
    pub email_smtp_port: u16,
    pub email_from: String,
    /// The recipient list is one string, not a `Vec` — the split happens in the
    /// mailer. Kept as stored so a round-trip through settings cannot reformat
    /// an operator's list.
    pub email_recipients: String,
    pub email_enabled: bool,
}

impl Default for NotificationConfig {
    /// Every gate off except [`Self::notify_on_failure`], and SMTP port 587.
    fn default() -> Self {
        Self {
            slack_webhook_credstore_ref: String::new(),
            slack_channel: String::new(),
            manager_ui_base_url: String::new(),
            slack_enabled: false,
            notify_on_failure: true,
            notify_on_success: false,
            notify_on_schedule_completion: false,
            scheduled_run_slack_enabled: false,
            scheduled_run_slack_templates: ScheduledRunSlackTemplates::default(),
            run_queue_queued_slack_enabled: false,
            email_smtp_host: String::new(),
            email_smtp_port: 587,
            email_from: String::new(),
            email_recipients: String::new(),
            email_enabled: false,
        }
    }
}

/// One audited notification attempt.
///
/// Egress failures are *recorded here and never propagated to a caller*, which
/// is what makes this table the only place an operator can see that Slack or
/// SMTP is broken.
///
/// [`Self::run_id`] is nullable because some logged events — a settings `/test`
/// send, for instance — belong to no run at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationLogEntry {
    pub id: Uuid,
    pub created_at: OffsetDateTime,
    /// The run this attempt was about; `None` for an attempt that belongs to
    /// no run.
    pub run_id: Option<Uuid>,
    /// `slack` | `email` — the egress that was tried.
    pub channel: String,
    /// The column is `NOT NULL DEFAULT ''`, so an unattributed entry is `""`
    /// rather than absent.
    pub event_type: String,
    /// Open set; see this module's note 4.
    pub outcome: String,
    /// Failure detail, `""` on success.
    pub detail: String,
}

// ==================== Dashboard ====================

/// A run as the dashboard renders it.
///
/// **A deliberate projection, not qa-runs' run record.** The full record is
/// `qa_runs_sdk::Run`, and using it here would make `qa-insights-sdk` depend on
/// `qa-runs-sdk`.
///
/// # Why not just depend on `qa-runs-sdk`
///
/// **No `*-sdk` crate in this workspace depends on another `*-sdk` crate.**
/// Being the first would be an architectural precedent set in passing, for a
/// dashboard widget. The gear crate does depend on `qa-runs-sdk` and reads live
/// run state through its `RunsReader` port, so it is the gear that projects
/// `Run` into this on the way out.
///
/// # Why these ten fields
///
/// They are the union of what the dashboard's two run surfaces draw: the
/// active-runs card (name, plan identity, environment, product, phase and start
/// instant, from which it computes elapsed time) and the recent-runs table
/// (name, product version, plan identity, phase and duration).
///
/// [`Self::run_id`] is the tenth because a run is keyed by id here, not by name,
/// and the plan identity is the `(repo_id, plan_path)` pair rather than a single
/// value (this module's note 1).
///
/// The colour a phase renders in is a pure function of the phase, so it is a
/// renderer concern rather than data and is not carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardRun {
    pub run_id: Uuid,
    /// The run's own name (`qa_runs_sdk::Run::name`), not an execution
    /// backend's object name.
    pub name: String,
    /// The run's state, as text rather than qa-runs' state enum, because
    /// importing that enum is the dependency this type exists to avoid.
    ///
    /// **What the gear puts here is `RunState::as_str()`** — qa-runs' own
    /// persisted spelling, lowercase, so `running` and never `Running`.
    /// Re-casing here would invent a second vocabulary for one value;
    /// `domain::service::dashboard` states the rule.
    pub phase: String,
    /// The plan identity, when the run targeted a plan; see this module's
    /// note 1.
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    /// The environment the run occupied, drawn on the active-runs card. A
    /// `Uuid` per this module's note 2, with the gear's REST layer resolving the
    /// display name.
    pub environment_id: Option<Uuid>,
    /// The product under test, drawn on the active-runs card. A product key,
    /// not an id — qa-catalog's products are keyed by it too.
    pub product_key: Option<String>,
    /// Drawn by the recent-runs table, not by the active-runs card.
    pub app_version: Option<String>,
    pub started_at: Option<OffsetDateTime>,
    /// The rendered duration text. The active-runs card computes elapsed time
    /// from [`Self::started_at`] instead and never reads this.
    pub duration: Option<String>,
}

/// Test volume for a single run on the dashboard trend chart.
///
/// [`Self::run_id`] is carried for the reason given on [`DashboardRun`].
///
/// One counting rule is easy to lose: **`ERROR` folds into `failed`**
/// (`FILTER (WHERE tr.status IN ('FAILED', 'ERROR'))`) while `SKIPPED` stands
/// alone, so `passed + failed + skipped` need not equal
/// [`Self::tests_total`], which is a plain `COUNT`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunTestTrendPoint {
    /// The chart labels by `run_name`; the run id is carried alongside so the
    /// point is linkable without a name lookup.
    pub run_id: Uuid,
    pub run_name: String,
    pub started_at: Option<OffsetDateTime>,
    pub tests_total: u64,
    pub passed: u64,
    /// Includes `ERROR`; see this type's header.
    pub failed: u64,
    pub skipped: u64,
}

/// Daily pass/fail counters for the dashboard status trend.
///
/// [`Self::day`] is a [`time::Date`] — the crate's date type, and the only
/// place in this module that is not an [`OffsetDateTime`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DailyStatusPoint {
    pub day: time::Date,
    pub passed: u64,
    pub failed: u64,
}

/// A recent failure surfaced on the dashboard.
///
/// A run is keyed by [`Self::run_id`] and a plan by the `(repo_id, plan_path)`
/// pair (this module's note 1).
///
/// The ten newest of these, over the last 24 hours, are
/// [`DashboardStats::failed_recent`]; `domain::service::dashboard`'s
/// `failure_card` is the projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedTestCard {
    pub test_name: String,
    /// `None`, not `Some("")`, when the producer reported no file — this
    /// projection keeps the distinction where [`TestResultRecord::test_file`]
    /// collapses absent to `""` (that type's decision 3).
    pub test_file: Option<String>,
    pub run_id: Uuid,
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    /// See this module's note 2.
    pub environment_id: Option<Uuid>,
    /// The **run's** effective instant — its finish, falling back to the run's
    /// own creation instant.
    ///
    /// So a card for a run still in progress carries
    /// [`TestResultRecord::run_created_at`] — which is why that run's failures
    /// appear here at all — and **never** `TestResultRecord::created_at`, the
    /// instant the result row was last written. That type's field tabulates the
    /// difference and why it matters.
    ///
    /// `Some` for every card this gear can produce today, because ingest always
    /// fills `run_created_at`; the `Option` is kept rather than narrowed so a
    /// future producer that cannot is representable.
    pub finished_at: Option<OffsetDateTime>,
    /// The **file**-level key the runner reported, off the result row itself —
    /// not a join, and not [`JiraBug`], which is this gear's own registry. See
    /// [`TestResultRecord::jira_key`].
    pub jira_key: Option<String>,
    pub launch_id: Option<String>,
}

/// A test that both passed and failed inside the dashboard's window.
///
/// The plan is the `(repo_id, plan_path)` pair per this module's note 1. **The
/// window is the last seven days**, fixed — not the `days` query parameter.
///
/// Computed over `domain::repos::ResultsRepository::flaky_groups` by
/// `domain::service::dashboard`'s `flaky_card`. The grain is
/// `(test_name, repo_id, plan_path)`, which is **not** the grain of
/// `domain::analytics::build_flaky`, the *analytics* flaky fold: that one keys
/// on `test_file` and splits statuses three ways. **Two different folds share
/// the word "flaky" and agree on nothing else.**
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlakyTestCard {
    pub test_name: String,
    pub test_file: Option<String>,
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    pub passed: u64,
    pub failed: u64,
    pub total: u64,
}

/// Pass rate aggregated per Quality Vector over a recent window.
///
/// The vectors come from `TEST_META`, which this gear reads through
/// `qa_catalog_sdk::UniverseTest::quality_vectors` — the field that exists
/// specifically because dropping it silently empties this section.
///
/// Computed over `domain::repos::ResultsRepository::file_status_counts` by
/// `domain::service::dashboard`'s `quality_vector_pass_rates`. The grain is
/// `test_file`, which is **not** the grain of
/// `domain::analytics::build_quality_vector_summary`, the *analytics*
/// quality-vector fold: that one counts files per vector off the universe alone,
/// carries an unclassified residue, and folds case variants across files where
/// this one does not. **Two folds share the word and agree on nothing else**, as
/// with the two flaky folds.
///
/// [`Self::total`] is a **counted** number and not `passed + failed` derived: it
/// is summed independently, and happens to equal the sum only while the two
/// status sets stay disjoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QualityVectorPassRate {
    pub vector: String,
    pub passed: u64,
    pub failed: u64,
    pub total: u64,
    /// Distinct **test files** contributing, as opposed to [`Self::total`]
    /// executions — and a file contributes on **any** row in the window, not
    /// only a counted one. So this can be non-zero while all three counters are
    /// zero.
    pub tests: u64,
}

/// One platform in the dashboard's footer strip.
///
/// The environment is identified by `Uuid` (this module's note 2); the name is
/// carried alongside because it is what is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentBrief {
    pub environment_id: Uuid,
    pub name: String,
    /// `Healthy` | `Degraded` | `Warning` | `Unhealthy` | `Unreachable` — the
    /// five buckets. Open set; see this module's note 4.
    pub status: String,
    pub version: Option<String>,
    pub build: Option<String>,
}

/// Compact platforms overview for the dashboard footer strip.
///
/// `Degraded` and `Warning` **share** a counter, which is why there are five
/// status values and four counters. The data is qa-environments', read through
/// the gear's ports; insights only aggregates it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentsSummary {
    pub total: u64,
    pub healthy: u64,
    /// Counts both `Degraded` and `Warning`; see this type's header.
    pub degraded: u64,
    pub unhealthy: u64,
    pub unreachable: u64,
    pub items: Vec<EnvironmentBrief>,
}

/// Everything `GET /qa/v1/dashboard` returns.
///
/// # The window, and the two that ignore it
///
/// `days` defaults to 14 and is clamped to `[3, 90]`. It governs [`Self::daily_test_status_trend`]. The 24-hour pairs and
/// [`Self::flaky_tests`] have windows of their own and do not move with it.
///
/// # Which fields Task 18 computes, and who owns the rest
///
/// **`GET /qa/v1/dashboard` does not satisfy `cpt-cf-qa-fr-insights-dashboard`
/// on its own**, and this field list is half the reason: that requirement
/// (PRD §5.5, `cpt-cf-qa-fr-insights-dashboard`) also names *pass rates*, which are plan Tasks 21 and
/// 23.
///
/// The other half is **not** owned by any task in the plan. This doc claimed
/// until 2026-08-21 that the requirement is "discharged across plan Tasks 18, 19,
/// 21 and 23"; Task 19's Step 0 disproved the `19` in that list. The requirement's
/// coverage bullet is not discharged in this feature at all — see
/// [`CoverageBuild`], which carries what ships and what does not.
///
/// Task 18 fills [`Self::total_runs`], [`Self::active_runs`],
/// [`Self::queued_runs`], [`Self::recent_runs`],
/// [`Self::recent_run_test_trend`], [`Self::daily_test_status_trend`] and
/// [`Self::active_runs_list`].
///
/// **Task 21b fills five more** — [`Self::failed_recent`],
/// [`Self::failed_24h_count`], [`Self::failed_prev_24h_count`],
/// [`Self::pass_rate_24h`] and [`Self::pass_rate_prev_24h`]. They were held back
/// from Task 18 rather than approximated: the KPI query has no phase
/// restriction, windows on the run's finish falling back to its creation — a
/// third row-inclusion rule inside that one endpoint — and computes its pass
/// rate over a denominator of `PASSED`+`FAILED`+`ERROR` rather than every row,
/// which is a status classification of its own.
/// `domain::service::ingest`'s table indexes it as the sixth.
///
/// **Task 23b fills [`Self::flaky_tests`]** — the *dashboard* flaky fold,
/// grouped by `(test_name, repo_id, plan_path)` over the
/// seven-day effective window, with the `HAVING`, the
/// `ORDER BY LEAST(passed, failed)` and the `LIMIT 10` **in SQL**. That last part
/// was the open decision this doc recorded: this gear's recorded preference is to
/// group in the database and fold in the domain, and the ruling went the other way
/// because folding here means returning every group in the window rather than ten
/// rows, on the table `cpt-cf-qa-nfr-scale` sizes at 5M rows.
/// `ResultsRepository::flaky_groups` carries the ruling.
///
/// **Task 25a fills [`Self::quality_vectors_pass_rate`]** — the *dashboard*
/// quality-vector fold, a group-by per `test_file` under the same sixth
/// classification, joined against
/// `qa_catalog_sdk::UniverseTest::quality_vectors` and folded at `:501-538`. This
/// entry said "nothing in this crate computes it" and, more decisively, that
/// **nothing in production implemented `CatalogReader` at all** — the adapter was
/// Task 40's. Both were true and the second is what made the field unfillable;
/// Task 25a shipped the adapter fifteen tasks early and the `ClientHub` lookup
/// with it, so `DashboardService` now holds the port. The rest of this entry
/// stands as written — Task 23 was named for this field and for
/// [`Self::flaky_tests`] and shipped neither on the wire: what that task shipped
/// is the analytics tier's four folds (`domain::analytics::aggregates`), a
/// different grain and a different classification.
///
/// The remaining three are at their `Default` because each needs an upstream this
/// gear does not have yet, not because the number is zero — and **none of the
/// three is owned by any task in the plan**: [`Self::total_plans`] (a qa-catalog
/// plan listing), [`Self::total_schedules`] (a qa-runs schedule listing) and
/// [`Self::environments_summary`] (a qa-environments port for platform *health*,
/// which is not the one Task 25a added — [`Self::environments_summary`] needs a
/// per-environment health fan-out, where `EnvironmentReader` resolves names).
///
/// `domain::service::dashboard`'s header carries the same list with the reason
/// per field, and the endpoint description says so on the wire, because a `0`
/// here is indistinguishable from a measured zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DashboardStats {
    /// Plans discovered in the catalog, after the optional product filter.
    pub total_plans: u64,
    pub total_runs: u64,
    /// Runs whose phase is `Running` or `Pending`.
    pub active_runs: u64,
    /// Runs admitted to the queue and not yet handed to an executor.
    ///
    /// **Named by one clause of the PRD.** `cpt-cf-qa-fr-insights-dashboard`
    /// (PRD §5.5) requires the dashboard to report "active **and queued** runs" —
    /// one clause of a requirement the dashboard endpoint does not discharge
    /// alone; see this type's field list.
    ///
    /// The count is available at all because run state is database-first: a
    /// queued launch already has an authoritative row in `RunState::Queued`, so
    /// it is read from the same single listing as every other run number rather
    /// than from a second call to an execution backend, which could not see it.
    pub queued_runs: u64,
    pub total_schedules: u64,
    /// The ten most recent runs. This, the active list, the active count and
    /// the total all come from **one** listing: a second query would cost a
    /// round trip for no new information.
    pub recent_runs: Vec<DashboardRun>,
    pub recent_run_test_trend: Vec<RunTestTrendPoint>,
    pub daily_test_status_trend: Vec<DailyStatusPoint>,
    /// Capped at ten, using the same predicate as [`Self::active_runs`] — so a
    /// dashboard reading "23 active" lists ten.
    pub active_runs_list: Vec<DashboardRun>,
    pub failed_recent: Vec<FailedTestCard>,
    pub failed_24h_count: u64,
    pub failed_prev_24h_count: u64,
    /// A **ratio in `[0, 1]`**, not a percentage: it is divided and not
    /// multiplied.
    ///
    /// `None` when the window held no counted row, which is distinct from "0%".
    /// "Counted" is narrower than "any row": the denominator is
    /// `PASSED`+`FAILED`+`ERROR`, so a window holding
    /// nothing but skipped tests is `None` rather than `Some(0.0)`, while a window
    /// of nothing but failures is `Some(0.0)`.
    pub pass_rate_24h: Option<f64>,
    /// The 24 hours before [`Self::pass_rate_24h`]'s window, on the same rule —
    /// `[now - 48h, now - 24h)` (`dashboard.rs:338-339`). The pair exists so the
    /// UI can draw a delta, which is why "no data" must not read as 0%.
    pub pass_rate_prev_24h: Option<f64>,
    /// The ten flakiest tests of the **last seven days** — tests that both passed
    /// and failed, ranked by the size of the smaller of the two counters
    /// (`dashboard.rs:395-398`) and then by the larger total. A window of its own,
    /// which does not move with `days`.
    pub flaky_tests: Vec<FlakyTestCard>,
    pub environments_summary: EnvironmentsSummary,
    /// Pass rate per Quality Vector over the **last seven days**, highest total
    /// first — a fourth window, equal to [`Self::flaky_tests`]' but declared
    /// separately, so the two can diverge without either moving silently.
    ///
    /// One entry per vector declared by any test file with a row in the window,
    /// with the executions of every such file summed in. **A file declaring two
    /// vectors contributes to both**, so the
    /// [`QualityVectorPassRate::total`]s across this array are not a row count.
    ///
    /// "Any row", not "a counted row" — the distinct-file count is incremented
    /// unconditionally (`dashboard.rs:524`), so an entry of all zeros with a
    /// non-zero [`QualityVectorPassRate::tests`] is legal and means every
    /// contributing file was skipped.
    pub quality_vectors_pass_rate: Vec<QualityVectorPassRate>,
    /// How many runs of the page [`Self::recent_runs`] and the other
    /// run-derived fields are drawn from could not be attributed to *any*
    /// product — a custom-plan run, which spans repositories and so names
    /// none — and were therefore dropped by a `product_id` filter for a
    /// different reason than "belongs to another product".
    ///
    /// **Always `0` when no `product_id` was given.** Nothing is dropped for
    /// either reason on an unscoped request, so this field does not count
    /// "custom-plan runs that exist" in general — only the ones a filter this
    /// request actually applied could not place. A UI renders this as a
    /// caveat exactly when it is non-zero, rather than a permanent one that
    /// says nothing about the current answer.
    pub unattributable_runs: u64,
}

// ==================== Coverage ====================

/// Code coverage percentages.
///
/// Line, branch and function percentages.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[allow(
    clippy::struct_field_names,
    reason = "these are the JSON keys the coverage endpoint emits, so renaming \
              them would be a wire change"
)]
pub struct CoverageSummary {
    pub line_pct: f64,
    pub branch_pct: f64,
    pub function_pct: f64,
}

/// Coverage for one product build.
///
/// `GET /qa/v1/dashboard/coverage` takes **no parameters**, returning one point
/// per product from the latest completed run that carried coverage.
///
/// # A consumer gets an empty list, in every deployment, today
///
/// Not a transient state and not an empty tenant: **nothing in this system
/// measures a coverage point yet.** The single copy of that argument is on
/// `CoverageBuildDto` in `qa-insights/src/api/rest/dto.rs`; it is not restated
/// here, because near-copies of it drifted once already.
///
/// The type nevertheless declares the **whole** shape rather than a subset: the
/// shape of a point is known and it is the points that nothing produces, so a
/// client can bind all four fields now and an empty array is the honest answer
/// for a build with no coverage.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverageBuild {
    pub product_key: String,
    pub version: String,
    pub build: String,
    pub coverage: CoverageSummary,
}
