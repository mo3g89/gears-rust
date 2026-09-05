//! Transport-agnostic models for the qa-insights contract.
//!
//! Every type below is a field-for-field port of a legacy struct or table, with
//! the citation on the type. Four adaptations recur, so they are stated once
//! here rather than repeated fourteen times.
//!
//! # 1. A plan is `(repo_id, plan_path)`, never a `plan_id: Uuid`
//!
//! The plan's drafts spell a plan's identity `plan_id: Uuid` — on
//! [`SkipListEntry`]'s lookup, on [`JiraBug`], and on three columns in Task 10.
//! **There is nothing to put in such a field.** A plan is not persisted in this
//! port: `qa_catalog_sdk::Plan` is materialized on read, there is no plans table,
//! and no plan UUID is ever minted. Task 7 hit this first and shipped
//! `qa_catalog_sdk::UniverseTest` with `repo_id: Uuid` + `plan_path: String`;
//! `qa_catalog_sdk::CustomPlanEntry` had reached the same conclusion before it,
//! and `qa_runs_sdk::RunTarget::Plan { repo_id, path }` is the same pair again.
//!
//! Legacy's own `jira_bugs.plan_id` is **not** a UUID either. The column is
//! `TEXT NOT NULL` (`manager/migrations/001_initial.sql:82`) and what it holds is
//! `compose_repo_plan_id(repo_id, dir_path, leaf_id)` — literally
//! `sanitize_k8s(format!("{repo}-{dir_path with / → -}"))`
//! (`manager/src/services/plans.rs:789-801`), a lossy, lowercased slug of the
//! repository id and the plan's directory path. `JiraService::get_open_bugs`
//! matches on it as an opaque string (`manager/src/services/jira.rs:220-229`),
//! fed straight from the launch route's path parameter
//! (`manager/src/routes/runs.rs:753`).
//!
//! So `(repo_id, plan_path)` is not merely *consistent* with the rest of the
//! port — it is **closer to legacy's meaning** than a `Uuid` would be, and it is
//! lossless where the slug is not. `plan_path` is the `plan.yaml` path within the
//! repository's content root, matching [`UniverseTest::plan_path`]'s spelling
//! exactly, because the analytics join and the bug registry key on the same
//! plans.
//!
//! [`UniverseTest::plan_path`]: https://docs.rs/qa-catalog-sdk
//!
//! # 2. A platform is a `Uuid`, never a name
//!
//! Legacy's `platform TEXT` columns hold a platform *name*. This port replaced
//! platform identity with a `Uuid` into qa-environments everywhere it appears —
//! `qa_runs_sdk::QueueEntry` documents the substitution for `run_queue.platform`
//! — so [`TestResultRecord::platform_id`] and [`JiraBug::platform_id`] follow.
//! The plan's Task 10 is internally inconsistent on this point (`platform_id
//! UUID NULL` on `qa_test_results`, `platform VARCHAR(255) NULL` on
//! `qa_jira_bugs`); the `Uuid` is the correct half.
//!
//! # 3. Counts are `u64`, durations are the runner's text
//!
//! Legacy's aggregate counters are `usize`, which is not a wire width. They are
//! `u64` here. Per-file case counts stay `u32`, matching
//! `qa_catalog_sdk::UniverseTest::static_case_count` so the two sides of the
//! expected-cases comparison have one type. Durations stay `Option<String>`: the
//! runner emits text such as `85.06s (0:01:25)` — that exact string is legacy's
//! own fixture at `manager/src/services/argo.rs:3289` and is asserted to survive
//! parsing at `:3296`, in the test `parse_test_results_keeps_extended_duration_text`
//! (`:3278-3297`). `qa_runs_sdk::RunTestResult::duration` already carries it
//! verbatim.
//!
//! **Citation corrected 2026-08-20** (Task 8 review): an earlier draft cited
//! `argo.rs:3279`, which is the test's `fn` line and contains no duration text.
//!
//! # 4. Status strings are open sets, not enums
//!
//! Test status, bug status and notification outcome are all unvalidated producer
//! text in legacy. `qa_runs_sdk::RunTestResult::status` documents the reasoning
//! for the first of them and this crate does not second-guess it: a consumer that
//! rejected an unknown value would drop a real result. The one exception is
//! [`SavedViewScope`], which legacy *does* validate against a closed two-value
//! set at the boundary (`manager/src/routes/analytics.rs:2104-2113`).

use time::OffsetDateTime;
use uuid::Uuid;

// ==================== Ingested results ====================

/// One file-level test outcome within a run.
///
/// Legacy `test_results` (`manager/migrations/001_initial.sql:65-73`), plus the
/// two columns added by later `ALTER`s in the same file — `test_file` (`:166`)
/// and `duration` (`:167`).
///
/// # Divergences from legacy, and why
///
/// 1. **`logs` is not here.** Legacy's table has a `logs TEXT` column, written
///    from `TestResult::logs` (`manager/src/models.rs:323`, inserted at
///    `manager/src/services/argo.rs:2603-2612`) and genuinely read — the run
///    detail view renders it (`manager/src/routes/runs.rs:147`, `:167`) and the
///    per-test history derives a short error line from it
///    (`manager/src/routes/tests.rs:333`). It is omitted because **nothing can
///    fill it**: this gear's only source of test outcomes is qa-runs, and
///    `qa_runs_sdk::RunTestResult` carries no per-test log slice (qa-runs keeps
///    whole-run `raw_logs` instead). A permanently-`NULL` column and a field no
///    writer can set is the "field nothing could set" shape this subsystem has
///    already rejected once (`qa_environments_sdk::Environment::default_branch`,
///    renamed from `TargetPlatform::default_branch`, records that argument).
///    **This is a real parity gap**, not a tidy-up:
///    restoring it needs a per-test log excerpt on qa-runs' event and SDK first,
///    at which point it is one additive field here.
/// 2. **Seven denormalized columns are here and not there** — `product_version`,
///    `app_build`, `platform_id`, `repo_id`/`plan_path`, `branch`,
///    `run_finished_at`, `run_created_at`. (Six until Task 21b added the last,
///    which is the one legacy's `COALESCE` fallback needs.) Legacy
///    joins `run_results` in every analytics query; here that join is a
///    cross-gear call, which `cpt-cf-qa-principle-async-insights` forbids on a
///    hot path. Safe to copy because a finished run's platform, version, build
///    and branch never change afterwards.
/// 3. **`test_file` is `String`, not `Option<String>`.** Legacy's column is
///    nullable and every query pays for it with `COALESCE(test_file, '')`
///    (`manager/src/routes/runs.rs:1154`). `qa_runs_sdk::RunTestResult::test_file`
///    already collapsed absent to `""` so that "absent" has exactly one
///    spelling; the ingest side keeps that.
/// 4. **`ingest_ordinal` is a stored column and is deliberately not a field
///    here.** `qa_test_results.ingest_ordinal` carries the row's position within
///    the batch that wrote it, and exists to reproduce legacy's `test_results.id`
///    SERIAL used as an ordering tiebreak (`manager/src/routes/analytics.rs:971`
///    orders on `t.id DESC`; `:1042` re-sorts *stably*, so it survives). It is a
///    persistence ordering detail, in the same class as `updated_at` — no
///    consumer reads it, and publishing it would invite one to. The repository
///    derives it from the batch's own order, so there is nothing for a caller to
///    supply either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestResultRecord {
    pub id: Uuid,
    /// The qa-runs run this outcome belongs to.
    pub run_id: Uuid,
    /// Test-file path relative to the repository content root; `""` when the
    /// producer reported none. See divergence 3.
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
    /// Denormalized from the run: the product version under test
    /// (legacy `run_results.app_version`, `001_initial.sql:56`).
    pub product_version: Option<String>,
    /// Denormalized from the run: the build under test
    /// (legacy `run_results.app_build`, added by the `ALTER` at
    /// `001_initial.sql:149`).
    ///
    /// **Not a duplicate of [`Self::product_version`].** That one is the
    /// analytics *filter* (`WHERE r.app_version = $1`,
    /// `manager/src/routes/analytics.rs:961`); this one is the analytics
    /// *projection* — the value `build_last_run_build_distribution` groups by
    /// and `api_build_tests` filters on. `None` means the run named no build,
    /// which legacy's consumer renders as `"unknown"` (`:1032-1033`) rather
    /// than storing.
    pub app_build: Option<String>,
    /// Denormalized from the run; see this module's note 2.
    pub platform_id: Option<Uuid>,
    /// Denormalized from the run: the repository half of the plan identity.
    /// `None` for a target that has no plan (a custom-plan or collect run).
    /// See this module's note 1.
    pub repo_id: Option<Uuid>,
    /// Denormalized from the run: the `plan.yaml` path half of the plan
    /// identity. See this module's note 1.
    pub plan_path: Option<String>,
    /// Denormalized from the run: the branch the tests came from. Legacy's
    /// analytics branch filter reads `source_ref` and falls back to
    /// `test_version` (`manager/src/routes/analytics.rs:24-27`); that fallback
    /// is resolved on ingest so the stored value is already the effective one.
    pub branch: Option<String>,
    /// Denormalized from the run. `None` while the run is still going: legacy
    /// writes RUNNING/PENDING rows too (`manager/src/services/argo.rs:2603`).
    pub run_finished_at: Option<OffsetDateTime>,
    /// Denormalized from the run: the run's own creation instant
    /// (`qa_runs_sdk::Run::created_at`).
    ///
    /// **The fallback half of legacy's `COALESCE(rr.finished_at, rr.created_at)`,
    /// and the reason it is a field rather than a reuse of [`Self::created_at`].**
    /// The two are different instants and only one of them is legacy's:
    ///
    /// | | what it is | does it move? |
    /// |---|---|---|
    /// | [`Self::created_at`] | when *this row* was written | yes — ingest is delete-then-insert per run, re-run on every result event, so it is reset to `now` for the whole life of the run |
    /// | this field | when the *run* was created | no |
    ///
    /// Added by Task 21b, whose dashboard KPI window counts unfinished runs by
    /// design (legacy's query carries no phase predicate,
    /// `manager/src/routes/dashboard.rs:317-348`). Windowing that on
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
/// Legacy `test_case_results` (`manager/migrations/001_initial.sql:253-264`),
/// whose own comment states the purpose: *"Per-function test case outcomes
/// (xfail/xpass/skip/pass/fail) parsed from the runner's `TEST_CASE` markers. One
/// row per test function per run, grouped under a file (`test_file`). Lets
/// analytics aggregate per-case, not just per-file."* (`:249-252`).
///
/// Field-for-field with legacy's ten columns. `id` widens `SERIAL` to `Uuid` and
/// `run_id` widens legacy's `INTEGER` foreign key to the qa-runs run id; both are
/// the platform-wide key convention and neither is a semantic change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestCaseResultRecord {
    pub id: Uuid,
    pub run_id: Uuid,
    /// The owning file. `NOT NULL` in legacy with no default, unlike
    /// [`TestResultRecord::test_file`] — a case row always knows its file.
    pub test_file: String,
    /// The pytest node identifier — `tests/test_x.py::TestC::test_m[param]` —
    /// and `""` when the producer reported none, matching legacy's
    /// `NOT NULL DEFAULT ''` (`001_initial.sql:257`) and
    /// `qa_runs_sdk::RunTestResult::nodeid`.
    pub nodeid: String,
    /// The test function's name. Legacy's column is `name`, not `test_name`;
    /// the spelling is kept so the two result tables stay visibly different
    /// shapes.
    pub name: String,
    /// Open set; see this module's note 4. Case-level statuses add `XFAIL` and
    /// `XPASS` to the file-level vocabulary.
    pub status: String,
    pub duration: Option<String>,
    /// The xfail/skip explanation, if the runner gave one. Nullable rather than
    /// `""`-defaulted because here absence *is* information
    /// (`001_initial.sql:261`).
    pub reason: Option<String>,
    /// The **case**-level bug reference; see [`TestResultRecord::jira_key`].
    pub ticket: Option<String>,
    pub created_at: OffsetDateTime,
}

/// The collect job's exact expected-case count for one file on one branch.
///
/// Legacy `test_case_collect` (`manager/migrations/001_initial.sql:272-280`),
/// whose comment explains why the number exists: *"Exact expected test-case
/// counts per file, produced by the collect job (pytest --collect-only,
/// parametrize expanded). Keyed by repo + branch + file so analytics can show an
/// exact 'expected cases' number without a full run."* (`:269-271`).
///
/// This value **wins over** `qa_catalog_sdk::UniverseTest::static_case_count`
/// wherever it exists (`manager/src/routes/analytics.rs:767-779`), which is why
/// the two share the `u32` width.
///
/// # No surrogate `id`
///
/// Legacy's primary key is the composite `(repo_id, branch, test_file)`
/// (`:278`), and nothing in legacy or in this port addresses a collect row any
/// other way — the collect report is an upsert on exactly that triple. The
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
/// Legacy's `Scope` (`manager/src/routes/analytics.rs:307-310`), serialized
/// `"all"` / `"plan"` (`scope_to_str`, `:2088-2093`) and validated against
/// exactly those two spellings at the boundary
/// (`parse_scope`, `:2104-2113` — anything else is a 400). A closed enum here
/// preserves that: this is the one status-like field in the crate that legacy
/// does *not* treat as open producer text.
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
    /// Legacy's `scope_to_str` (`manager/src/routes/analytics.rs:2088-2093`).
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
/// Legacy `analytics_saved_views` (`manager/migrations/001_initial.sql:183-192`)
/// as surfaced by `AnalyticsSavedView` (`manager/src/routes/analytics.rs:77-86`).
///
/// # The uniqueness rule this type has to keep representable
///
/// Legacy's unique index is on `(owner_id, scope, COALESCE(plan_id, ''), name)`
/// (`001_initial.sql:194-195`). The `COALESCE` is the whole point: it lets one
/// owner keep a global view and a plan-scoped view of the *same name* without
/// collision. The port has to reproduce that semantics, not the SQL — Task 10
/// materializes the coalesced value as its own column because `SQLite` and `MySQL`
/// do not both support functional indexes.
///
/// # `query_json` is opaque text
///
/// Legacy types it `serde_json::Value` and never inspects it: every one of the
/// six statements that touch the column binds or returns it whole
/// (`analytics.rs:542`, `:557`, `:606`, `:626`, `:664`, `:687`). This crate is
/// `serde`-free, so it is a `String` holding the JSON document verbatim. Parsing
/// it would be net-new behaviour, not a port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedView {
    pub id: Uuid,
    /// The principal that owns the view. Legacy's is a `TEXT` identifier lifted
    /// from a request header (`analytics_owner_id`); here it is the tenant's
    /// principal id.
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
/// Legacy `SavedViewUpsertRequest` (`manager/src/routes/analytics.rs:69-74`),
/// which carries exactly `name`, `scope`, `plan_id` and `query_json` — the owner
/// comes from the request identity and never from the body, and the timestamps
/// are the server's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewSavedView {
    pub scope: SavedViewScope,
    /// Required when [`Self::scope`] is [`SavedViewScope::Plan`]; legacy rejects
    /// a plan-scoped view with no plan with a 400 (`analytics.rs:530-540`).
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    pub name: String,
    pub query_json: String,
}

// ==================== JIRA bug registry ====================

/// A tracked bug against a test.
///
/// Legacy `jira_bugs` (`manager/migrations/001_initial.sql:78-89`) as surfaced by
/// `JiraBug` (`manager/src/models.rs:689-700`). **Ten columns, eleven fields** —
/// divergence 1 below splits legacy's single `plan_id` into two, so the counts
/// deliberately do not match. (Corrected 2026-08-20; an earlier draft said "ten
/// fields", which contradicted its own divergence list.)
///
/// # Divergences from legacy, and why
///
/// 1. **`plan_id: String` becomes `repo_id` + `plan_path`** — see this module's
///    note 1, which is written mostly about this table.
/// 2. **`platform: Option<String>` becomes `platform_id: Option<Uuid>`** — see
///    this module's note 2.
/// 3. **`id: i32` becomes `Uuid`** — platform key convention.
///
/// # There is no `auto_rerun` field
///
/// An earlier draft of the plan, and `DESIGN.md` §3.7's original one-line list,
/// gave this table an `auto_rerun` flag. **Legacy has no such column**: the table
/// stops at `resolved_at` (`001_initial.sql:88`), and auto-rerun is a *global*
/// switch — [`JiraPollerConfig::auto_rerun_on_resolve`]. A per-bug flag nothing
/// reads would be net-new design wearing a parity costume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JiraBug {
    pub id: Uuid,
    /// The issue key, e.g. `VHP-2618`. Globally unique in legacy
    /// (`001_initial.sql:80`); unique per tenant here.
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
    /// Optional qualifier: the platform the failure was seen on. See this
    /// module's note 2.
    pub platform_id: Option<Uuid>,
    /// `Open` until the poller sees the issue reach a `done` status category,
    /// at which point it becomes `Resolved`
    /// (`manager/src/services/jira.rs:243-251`). Open set — the value can also
    /// come back from the JIRA API (`check_jira_status`, `jira.rs:254`) — so it
    /// is a `String`; see this module's note 4.
    pub status: String,
    pub summary: String,
    pub created_at: OffsetDateTime,
    /// Set when [`Self::status`] becomes `Resolved`; `None` while open.
    pub resolved_at: Option<OffsetDateTime>,
}

/// Filing payload for a [`JiraBug`].
///
/// The four server-owned fields are absent: `id` and `created_at` are the
/// server's, `status` starts at legacy's column default `'Open'`
/// (`manager/migrations/001_initial.sql:85`), and `resolved_at` is the poller's
/// to write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewJiraBug {
    pub jira_key: String,
    pub test_name: String,
    pub repo_id: Uuid,
    pub plan_path: String,
    pub app_version: Option<String>,
    pub platform_id: Option<Uuid>,
    pub summary: String,
}

/// One entry of the runner's skip list.
///
/// # What the runner actually receives
///
/// Not this type — a single string. Legacy assembles it at launch from the open
/// bugs of the plan being launched and joins it with commas
/// (`manager/src/routes/runs.rs:752-761`):
///
/// ```text
/// bugs.iter().map(|b| format!("{}:{}", b.test_name, b.jira_key)).join(",")
/// ```
///
/// which reaches the runner as `SKIP_TESTS_WITH_BUGS`
/// (`manager/src/services/argo.rs:476-481`). The rendering belongs to qa-runs'
/// environment assembly, so the contract carries the pairs and lets the consumer
/// render them.
///
/// # An empty list means the variable is absent, not empty
///
/// Legacy guards it twice: `get_open_bugs` returning no rows yields `None`
/// rather than `Some("")` (`runs.rs:753-761`, the `Ok(bugs) if !bugs.is_empty()`
/// arm), and the env-var push is additionally gated on `!skip.is_empty()`
/// (`argo.rs:477-479`). So a run with no open bugs sees **no**
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
/// Legacy `JiraConfig` (`manager/src/models.rs:678-685`), stored as the
/// `settings` row keyed `jira` (`001_initial.sql:93-96`). Six fields, six
/// fields — with one changed type.
///
/// # `api_token` is a credstore reference, never token material
///
/// Legacy stores the token itself in the settings JSONB. This port deliberately
/// diverges: the platform has a credential store, and writing a bearer token
/// into a gear table would fail review. The field is renamed rather than merely
/// retyped so that a `String` holding an actual token cannot be assigned to it
/// by accident — the same shape
/// `qa_environments_sdk::Environment::kubeconfig_credstore_ref` (renamed from
/// `TargetPlatform::kubeconfig_credstore_ref`) uses.
///
/// One of **two** such divergences from legacy's settings storage, the other
/// being [`NotificationConfig::slack_webhook_credstore_ref`]. The Task 8 report
/// raised the second as an open question — a Slack incoming-webhook URL *is* the
/// bearer secret, so the argument for the JIRA token applies unchanged — and it
/// was decided the same way on 2026-08-20. Task 10's schema carries both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JiraConfig {
    /// Base URL of the JIRA instance; legacy trims a trailing `/` at use
    /// (`manager/src/services/jira.rs:261-265`).
    pub url: String,
    pub project_key: String,
    /// The account the token belongs to; JIRA basic auth is `email:token`.
    pub email: String,
    /// Credstore reference to the API token. **Never the token.** See this
    /// type's header.
    pub api_token_credstore_ref: String,
    /// `None` uses the project's default issue type.
    pub issue_type: Option<String>,
    /// A missing or disabled config short-circuits the poller silently
    /// (`manager/src/services/jira_poller.rs:40-43`).
    pub enabled: bool,
}

/// JIRA poller cadence and the auto-rerun switch.
///
/// Legacy `JiraPollerConfig` (`manager/src/models.rs:1417-1420`), stored as the
/// `settings` row keyed `jira_poller`. Two fields, two fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JiraPollerConfig {
    /// Legacy clamps this with `.max(1)` and the loop **sleeps first**
    /// (`manager/src/services/jira_poller.rs:16-31`).
    pub poll_interval_seconds: u64,
    /// Global gate on relaunching a run when a bug resolves. Legacy needs
    /// *both* this and a new build before it reruns
    /// (`jira_poller.rs:60-70`); resolution itself is recorded either way
    /// (`:54-58`), so turning this off does not stop bugs closing.
    pub auto_rerun_on_resolve: bool,
}

impl Default for JiraPollerConfig {
    /// Legacy's defaults verbatim — 300 seconds and auto-rerun **on**
    /// (`manager/src/models.rs:1422-1429`). Spelled out rather than derived
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
/// Legacy `ScheduledRunSlackTemplate` (`manager/src/models.rs:1062-1071`).
/// Seven fields, seven fields. `body` carries `#[serde(alias = "message")]`
/// there; the alias is a wire concern and belongs to the gear's REST DTO, not
/// here.
///
/// The default *text* of each template is legacy's `default_header_template()`
/// and friends, which this crate does not carry: the strings are the renderer's
/// and are ported with it (plan Task 32).
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
/// Legacy `ScheduledRunSlackTemplatesConfig` (`manager/src/models.rs:1088-1097`).
/// The six field names are the six scheduled-run events, and they are the same
/// six tokens as `qa_runs_sdk::SLACK_NOTIFICATION_EVENTS` — `pending`,
/// `in_progress`, `succeeded`, `failed`, `error`, `skipped`
/// (`manager/src/models.rs:949-959`, `#[serde(rename_all = "snake_case")]`).
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
    /// Legacy's `ScheduledRunSlackTemplatesConfig::template`
    /// (`manager/src/models.rs:1100-1111`) takes the enum and is therefore
    /// total; this takes the serialized token, so it is partial. Matching on the
    /// **`snake_case` spelling** is load-bearing: `InProgress` lowercased is
    /// `inprogress`, which legacy's own parser does not recognise, and a
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
/// Legacy `NotificationsConfig` (`manager/src/models.rs:1338-1362`), stored as
/// the `settings` row keyed `notifications`. **Fifteen fields, fifteen fields**,
/// in legacy's order — verified 2026-08-18, and the plan's "fifteen fields" is
/// correct.
///
/// The name drops legacy's plural (`NotificationsConfig` →
/// `NotificationConfig`) to match the plan's model list and the
/// `qa_notification_config` table; nothing else about it changes.
///
/// # Which field gates which event
///
/// Every boolean here is a routing gate, and legacy's mapping is the behaviour
/// this port has to keep (`notify::routing`, design §4.2). Two are worth
/// flagging from legacy's own comments (`models.rs:1348-1356`):
/// [`Self::run_queue_queued_slack_enabled`] is *off* by default because a busy
/// platform produces a lot of queue events, and the companion `expired` event
/// has **no toggle at all** — it is the one that stops a run disappearing
/// silently.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "seven independent routing gates ported one-for-one from legacy's \
              NotificationsConfig (manager/src/models.rs:1338-1362); grouping \
              them into sub-structs would change the stored settings shape"
)]
pub struct NotificationConfig {
    /// Credstore reference to the Slack incoming webhook. **Never the URL.**
    ///
    /// The second of this gear's two deliberate credential divergences from
    /// legacy, which stores the URL itself in the `notifications` settings row.
    /// A Slack incoming-webhook URL is not an address that happens to be
    /// secret — possession of it *is* the authorization to post, exactly like
    /// [`JiraConfig::api_token_credstore_ref`], so it gets the same treatment
    /// and the same rename-not-retype so a raw URL cannot be assigned by
    /// accident.
    ///
    /// **Decided 2026-08-20** (Task 8 review). The first draft kept it a plain
    /// `slack_webhook_url: String` because the plan named only the JIRA token,
    /// and flagged the asymmetry as an open question rather than resolving it
    /// unilaterally.
    pub slack_webhook_credstore_ref: String,
    pub slack_channel: String,
    /// Base URL used to build run links in rendered messages.
    pub manager_ui_base_url: String,
    pub slack_enabled: bool,
    /// Legacy default `true` — the only notify gate that starts on.
    pub notify_on_failure: bool,
    pub notify_on_success: bool,
    pub notify_on_schedule_completion: bool,
    pub scheduled_run_slack_enabled: bool,
    pub scheduled_run_slack_templates: ScheduledRunSlackTemplates,
    /// Announce a run being *queued*. Off by default; see this type's header.
    pub run_queue_queued_slack_enabled: bool,
    pub email_smtp_host: String,
    /// Legacy default 587.
    pub email_smtp_port: u16,
    pub email_from: String,
    /// Legacy stores the recipient list as one string, not a `Vec` — the split
    /// happens in the mailer. Kept as stored so a round-trip through settings
    /// cannot reformat an operator's list.
    pub email_recipients: String,
    pub email_enabled: bool,
}

impl Default for NotificationConfig {
    /// Legacy's `Default` verbatim (`manager/src/models.rs:1364-1384`): every
    /// gate off except [`Self::notify_on_failure`], and SMTP port 587.
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
/// Legacy `notification_log` (`manager/migrations/001_initial.sql:205-213`) as
/// surfaced by `NotificationLogEntry` (`manager/src/models.rs:1468-1475`).
///
/// Egress failures are *recorded here and never propagated to a caller* (design
/// §4.8), which is what makes this table the only place an operator can see that
/// Slack or SMTP is broken.
///
/// **Divergence:** legacy's `workflow_name TEXT NOT NULL` becomes
/// [`Self::run_id`], nullable — the gear's run identity is a `Uuid`, and some
/// logged events (a settings `/test` send) belong to no run at all, which legacy
/// records with an empty string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationLogEntry {
    /// Widened from legacy's `BIGSERIAL`.
    pub id: Uuid,
    pub created_at: OffsetDateTime,
    /// The run this attempt was about; see this type's divergence note.
    pub run_id: Option<Uuid>,
    /// `slack` | `email` — the egress that was tried.
    pub channel: String,
    /// Legacy's column is `NOT NULL DEFAULT ''` (`001_initial.sql:210`), so an
    /// unattributed entry is `""` rather than absent.
    pub event_type: String,
    /// Open set; see this module's note 4.
    pub outcome: String,
    /// Failure detail, `""` on success (`001_initial.sql:212`).
    pub detail: String,
}

// ==================== Dashboard ====================

/// A run as the dashboard renders it.
///
/// **This type has no legacy counterpart and is a deliberate projection.**
/// Legacy's [`DashboardStats`] carries `Vec<WorkflowRun>` twice
/// (`manager/src/models.rs:363`, `:366`) — its whole run record. The equivalent
/// here is `qa_runs_sdk::Run`, and using it would make `qa-insights-sdk` depend
/// on `qa-runs-sdk`.
///
/// # Why not just depend on `qa-runs-sdk`
///
/// **No `*-sdk` crate in this workspace depends on another `*-sdk` crate** —
/// checked across every SDK manifest on 2026-08-18. Being the first would be an
/// architectural precedent set in passing, by a skeleton task, for a dashboard
/// widget. The gear crate does depend on `qa-runs-sdk` (plan Task 9) and reads
/// live run state through its `RunsReader` port, so it is the gear that projects
/// `Run` into this on the way out.
///
/// # Why these ten fields
///
/// **Eight legacy references, ten fields.** The two extra are [`Self::run_id`],
/// which legacy does not need because it keys a run by `run.name`, and the split
/// of legacy's single `run.plan_id` into `repo_id` + `plan_path` (this module's
/// note 1). Stated as two numbers because an earlier draft's heading said "six
/// fields" while the struct had eight — the same count-versus-list slip the Task
/// 8 review caught on [`DashboardStats`].
///
/// The eight are the union of what legacy's **two** run consumers actually
/// draw.
/// Both lists on [`DashboardStats`] feed this type, and they are read by
/// different front ends:
///
/// * [`DashboardStats::active_runs_list`] → `ActiveRunsCard`
///   (`manager-ui/src/components/dashboard/ActiveRunsCard.tsx:42-60`) — draws
///   `run.name`, `run.plan_id`, **`run.platform`** (`:53`), **`run.product_key`**
///   (`:54`), `run.phase` and `run.started_at`. It shows elapsed time computed
///   from `started_at` rather than `run.duration`.
/// * The server-rendered runs table (`manager/templates/dashboard.html:98-117`)
///   — draws `run.name`, `run.app_version`, `run.plan_id`, `run.phase` and
///   `run.duration`.
///
/// `run.phase_color()` is a further reference in that template (`:110`) and is a
/// pure function of `phase` (`WorkflowRun::phase_color`,
/// `manager/src/models.rs:103-112`), so it is a renderer concern rather than
/// data.
///
/// # Two corrections from the Task 8 review (2026-08-20)
///
/// The first draft of this type had six fields and a doc claiming they were
/// "exactly what legacy's dashboard template renders … the `recent_runs` and
/// `active_runs_list` loops … nothing that is drawn is missing". Three things
/// were wrong with that, and one of them cost fields:
///
/// 1. **`manager/templates/dashboard.html` has no `active_runs_list` loop.** The
///    identifier does not appear in any Askama template; it exists only on the
///    JSON contract (`manager-ui/src/api/types.ts:190`) and its React consumer
///    (`DashboardPage.tsx:60`).
/// 2. **That template's `recent_runs` loop is not this struct's.** It belongs to
///    `DashboardTemplate` (`manager/src/routes/dashboard.rs:39-48`), the
///    server-rendered page returned by `index` — a different struct from
///    `DashboardStats`, which `api_dashboard` returns as JSON. They happen to
///    share a field name.
/// 3. Consequently the real consumer of the active list was never read, and
///    **`platform_id` and `product_key` were missing** — both are drawn today.
///    They are added here.
///
/// # One field with no live consumer, kept anyway
///
/// [`DashboardStats::recent_runs`] has **no** React consumer:
/// `RecentRunsTable.tsx` exists but is imported nowhere in `manager-ui/src`, so
/// the JSON list is currently drawn by nothing. The field set above is kept as
/// the union regardless, because the server-rendered table draws the same runs
/// and dropping `app_version` or `duration` would silently delete a column from
/// the port's equivalent view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardRun {
    pub run_id: Uuid,
    /// Legacy renders `run.name`, the Argo workflow name. This gear's runs have
    /// a name of their own (`qa_runs_sdk::Run::name`).
    pub name: String,
    /// Legacy's `phase` string — `Running` | `Pending` | `Succeeded` | `Failed`
    /// | `Error`. Left as text rather than mapped onto qa-runs' state enum,
    /// because importing that enum is the dependency this type exists to avoid.
    ///
    /// **What the gear actually puts here is `RunState::as_str()`** — qa-runs'
    /// own persisted spelling, lowercase (`qa-runs-sdk/src/models.rs:266-280`),
    /// so `running` and not `Running`. Recorded because the two lists above and
    /// below are legacy's and a reader would otherwise expect the capitalised
    /// Argo phase. Re-casing here would invent a third vocabulary for the same
    /// value; `domain::service::dashboard` states the rule.
    pub phase: String,
    /// The plan identity, when the run targeted a plan; see this module's
    /// note 1.
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    /// The platform the run occupied. Drawn by `ActiveRunsCard.tsx:53` as
    /// legacy's `run.platform` *name*; a `Uuid` here per this module's note 2,
    /// with the gear's REST layer resolving the display name.
    ///
    /// **Added 2026-08-20** by the Task 8 review — see this type's corrections
    /// section. Without it the active-runs card loses a line it draws today.
    pub platform_id: Option<Uuid>,
    /// The product under test. Drawn by `ActiveRunsCard.tsx:54`. Legacy's
    /// `run_results.product_key` is a `TEXT` key, not an id, and stays text
    /// here — qa-catalog's products are keyed by it too.
    ///
    /// **Added 2026-08-20** by the Task 8 review, alongside
    /// [`Self::platform_id`].
    pub product_key: Option<String>,
    /// Drawn by the server-rendered table (`dashboard.html:104`), not by
    /// `ActiveRunsCard`.
    pub app_version: Option<String>,
    pub started_at: Option<OffsetDateTime>,
    /// The rendered duration text, as legacy's template prints it
    /// (`dashboard.html:114`). `ActiveRunsCard` computes elapsed time from
    /// [`Self::started_at`] instead and never reads this.
    pub duration: Option<String>,
}

/// Test volume for a single run on the dashboard trend chart.
///
/// Legacy `DashboardRunTestTrendPoint` (`manager/src/models.rs:431-439`),
/// renamed to drop the redundant `Dashboard` prefix inside this module. **Six
/// fields, seven** — the extra is [`Self::run_id`], for the reason given on
/// [`DashboardRun`].
///
/// The counters come from legacy's per-run SQL (`routes/dashboard.rs:170-190`),
/// and one rule there is easy to lose: **`ERROR` folds into `failed`**
/// (`FILTER (WHERE tr.status IN ('FAILED', 'ERROR'))`) while `SKIPPED` stands
/// alone, so `passed + failed + skipped` need not equal
/// [`Self::tests_total`], which is a plain `COUNT`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunTestTrendPoint {
    /// Legacy keys the chart by `run_name`; the run id is carried alongside so
    /// the point is linkable without a name lookup.
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
/// Legacy `DashboardDailyStatusPoint` (`manager/src/models.rs:442-447`), whose
/// `day` is a `chrono::NaiveDate` rendered as a string. It is a
/// [`time::Date`] here — the crate's date type, and the only place in this
/// module that is not an [`OffsetDateTime`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DailyStatusPoint {
    pub day: time::Date,
    pub passed: u64,
    pub failed: u64,
}

/// A recent failure surfaced on the dashboard.
///
/// Legacy `FailedTestCard` (`manager/src/models.rs:388-398`). Eight fields;
/// `workflow_name` becomes [`Self::run_id`] and `plan_id` becomes the plan pair
/// (this module's note 1), so nine here.
///
/// The ten newest of these, over the last 24 hours, are
/// [`DashboardStats::failed_recent`]; the query is `dashboard.rs:263-279` and
/// `qa-insights/src/domain/service/dashboard.rs`' `failure_card` is the
/// projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedTestCard {
    pub test_name: String,
    /// `None`, not `Some("")`, when the producer reported no file — legacy's
    /// column is nullable where [`TestResultRecord::test_file`] collapses absent
    /// to `""` (that type's divergence 3). This is where the two spellings meet
    /// again.
    pub test_file: Option<String>,
    /// Legacy's `workflow_name`.
    pub run_id: Uuid,
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    /// See this module's note 2.
    pub platform_id: Option<Uuid>,
    /// The **run's** effective instant — its finish, falling back to its
    /// creation. Legacy selects the fallback expression itself
    /// (`COALESCE(rr.finished_at, rr.created_at) AS finished_at`,
    /// `dashboard.rs:270`), where `rr.created_at` is the *run's* creation instant.
    ///
    /// So a card for a run still in progress carries
    /// [`TestResultRecord::run_created_at`] — which is why that run's failures
    /// appear here at all — and **never** `TestResultRecord::created_at`, the
    /// instant the result row was last written. That type's field tabulates the
    /// difference and why it matters.
    ///
    /// `Some` for every card this gear can produce today, because ingest always
    /// fills `run_created_at`; the `Option` is legacy's own type and is kept
    /// rather than narrowed.
    pub finished_at: Option<OffsetDateTime>,
    /// The **file**-level key the runner reported, off the result row itself
    /// (`tr.jira_key`, `dashboard.rs:271`) — not a join, and not
    /// [`JiraBug`], which is this gear's own registry. See
    /// [`TestResultRecord::jira_key`].
    pub jira_key: Option<String>,
    pub launch_id: Option<String>,
}

/// A test that both passed and failed inside the dashboard's window.
///
/// Legacy `FlakyTestCard` (`manager/src/models.rs:401-409`, corrected from
/// `:400-409` by Task 23b — the neighbouring citations run from the `derive` line
/// to the closing brace, and `:400` is this type's doc comment). **Six fields,
/// seven** — `plan_id` is expanded into the plan pair per this module's note 1.
/// Legacy's window is the last
/// seven days (`routes/dashboard.rs:391`), not the `days` query parameter.
///
/// Filled by Task 23b, over
/// `qa_insights::domain::repos::ResultsRepository::flaky_groups` and
/// `qa_insights::domain::service::dashboard`'s `flaky_card`. The grain is
/// `(test_name, repo_id, plan_path)` — legacy's `GROUP BY tr.test_name,
/// rr.plan_id` (`dashboard.rs:392`) — which is **not** the grain of
/// `qa_insights::domain::analytics::build_flaky`, the *analytics* flaky fold: that
/// one keys on `test_file` and splits statuses three ways. Legacy has two flaky
/// folds and they agree on nothing but the word.
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
/// Legacy `QualityVectorPassRate` (`manager/src/models.rs:378-385`). Five
/// fields, five fields. The vectors come from `TEST_META`, which this gear reads
/// through `qa_catalog_sdk::UniverseTest::quality_vectors` — the field Task 7
/// added specifically because dropping it silently empties this section.
///
/// **Filled by Task 25a**, over
/// `qa_insights::domain::repos::ResultsRepository::file_status_counts` and
/// `qa_insights::domain::service::dashboard`'s `quality_vector_pass_rates`. The
/// grain is `test_file` — legacy's `GROUP BY tr.test_file`
/// (`routes/dashboard.rs:492`) — which is **not** the grain of
/// `qa_insights::domain::analytics::build_quality_vector_summary`, the *analytics*
/// quality-vector fold: that one counts files per vector off the universe alone,
/// carries an unclassified residue, and folds case variants across files where
/// this one does not. Legacy has two quality-vector folds and, like its two flaky
/// folds, they agree on nothing but the word.
///
/// [`Self::total`] is a **rendered** number and not `passed + failed` derived:
/// legacy selects and sums it independently (`routes/dashboard.rs:487`, `:523`),
/// and it happens to equal the sum only while the two status sets stay disjoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QualityVectorPassRate {
    pub vector: String,
    pub passed: u64,
    pub failed: u64,
    pub total: u64,
    /// Distinct **test files** contributing, as opposed to [`Self::total`]
    /// executions — and a file contributes on **any** row in the window, not
    /// only a counted one (`routes/dashboard.rs:524` has no test on the
    /// counters). So this can be non-zero while all three counters are zero.
    pub tests: u64,
}

/// One platform in the dashboard's footer strip.
///
/// Legacy `PlatformBrief` (`manager/src/models.rs:422-428`). Four fields, plus
/// the id: legacy identifies a platform by name, this port by `Uuid` (this
/// module's note 2), and the name is still carried because it is what is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformBrief {
    pub platform_id: Uuid,
    pub name: String,
    /// `Healthy` | `Degraded` | `Warning` | `Unhealthy` | `Unreachable`, the
    /// five values legacy buckets on (`routes/dashboard.rs:456-460`). Open set;
    /// see this module's note 4.
    pub status: String,
    pub version: Option<String>,
    pub build: Option<String>,
}

/// Compact platforms overview for the dashboard footer strip.
///
/// Legacy `PlatformsSummary` (`manager/src/models.rs:412-420`). Six fields, six
/// fields.
///
/// Note that `Degraded` and `Warning` **share** a counter
/// (`routes/dashboard.rs:457`), which is why there are five status values and
/// four counters. The data is qa-environments', read through the gear's ports;
/// insights only aggregates it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlatformsSummary {
    pub total: u64,
    pub healthy: u64,
    /// Counts both `Degraded` and `Warning`; see this type's header.
    pub degraded: u64,
    pub unhealthy: u64,
    pub unreachable: u64,
    pub items: Vec<PlatformBrief>,
}

/// Everything `GET /qa/v1/dashboard` returns.
///
/// Legacy `DashboardStats` (`manager/src/models.rs:358-375`) has sixteen fields,
/// assembled by `api_dashboard` (`manager/src/routes/dashboard.rs:84`,
/// constructed at `:542-559`). This type carries **seventeen**: those sixteen
/// plus [`Self::queued_runs`], which legacy cannot have. The count has been wrong
/// once already (it said "sixteen fields, sixteen fields" while the list was
/// right; corrected 2026-08-20, Task 8 review), so it is stated as an arithmetic
/// claim rather than a matched pair — 16 + 1.
///
/// **Citation correction:** the plan cites `manager/src/routes/dashboard.rs` for
/// "`DashboardStats` shape". The struct is not defined there — it is defined at
/// `manager/src/models.rs:358-375` and merely constructed in `dashboard.rs`. The
/// nested card and point types are all in `models.rs` too, and none of them
/// appear in the plan's model list even though the type cannot exist without
/// them.
///
/// # The window, and the two that ignore it
///
/// `days` defaults to 14 and is clamped to `[3, 90]` (`dashboard.rs:104`). It
/// governs [`Self::daily_test_status_trend`]. The 24-hour pairs and
/// [`Self::flaky_tests`] have windows of their own and do not move with it.
///
/// # Which fields Task 18 computes, and who owns the rest
///
/// **`GET /qa/v1/dashboard` does not satisfy `cpt-cf-qa-fr-insights-dashboard`
/// on its own**, and this field list is half the reason: that requirement
/// (`docs/PRD.md:575-581`) also names *pass rates*, which are plan Tasks 21 and
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
/// from Task 18 rather than approximated: legacy's KPI query
/// (`manager/src/routes/dashboard.rs:317-348`) has no phase restriction, windows
/// on `COALESCE(finished_at, created_at)` — a third row-inclusion rule inside
/// that one endpoint — and computes its pass rate over a denominator of
/// `PASSED`+`FAILED`+`ERROR` (`:329`) rather than every row, which is a status
/// classification of its own. `qa-insights/src/domain/service/ingest.rs`' table
/// indexes it as the sixth.
///
/// **Task 23b fills [`Self::flaky_tests`]** — legacy's *dashboard* flaky query
/// (`dashboard.rs:380-400`), grouped by `(test_name, repo_id, plan_path)` over the
/// seven-day effective window, with the `HAVING`, the
/// `ORDER BY LEAST(passed, failed)` and the `LIMIT 10` **in SQL**. That last part
/// was the open decision this doc recorded: this gear's recorded preference is to
/// group in the database and fold in the domain, and the ruling went the other way
/// because folding here means returning every group in the window rather than ten
/// rows, on the table `cpt-cf-qa-nfr-scale` sizes at 5M rows.
/// `ResultsRepository::flaky_groups` carries the ruling.
///
/// **Task 25a fills [`Self::quality_vectors_pass_rate`]** — legacy's *dashboard*
/// quality-vector query, a group-by per `test_file` under the same sixth
/// classification (the SQL at `dashboard.rs:483-492`) joined against
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
/// [`Self::platforms_summary`] (a qa-environments port for platform *health*,
/// which is not the one Task 25a added — [`Self::platforms_summary`] needs
/// legacy's per-platform health fan-out (`routes/dashboard.rs:421-460`), where
/// `EnvironmentReader` resolves names).
///
/// `domain::service::dashboard`'s header carries the same list with the reason
/// per field, and the endpoint description says so on the wire, because a `0`
/// here is indistinguishable from a measured zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DashboardStats {
    /// Plans discovered in the catalog, after the optional product filter.
    pub total_plans: u64,
    pub total_runs: u64,
    /// Runs whose phase is `Running` or `Pending` (`dashboard.rs:147-151`).
    pub active_runs: u64,
    /// Runs admitted to the queue and not yet handed to an executor.
    ///
    /// **Net-new against legacy, and named by one clause of the PRD.**
    /// `cpt-cf-qa-fr-insights-dashboard` (`docs/PRD.md:579`) requires the
    /// dashboard to report "active **and queued** runs" — one clause of a
    /// requirement the dashboard endpoint does not discharge alone; see this
    /// type's field list. Legacy's own dashboard
    /// reports no queued count at all, and cannot: its queue table is written by
    /// `manager/src/services/run_queue.rs`, which never touches `run_results`
    /// (grep: zero occurrences), and a queued launch has no Argo Workflow — so a
    /// queued run is invisible to the `list_runs_with_history` listing
    /// `api_dashboard` derives every run number from. This architecture has an
    /// authoritative run row in `RunState::Queued` instead, so the count exists
    /// and is read from the same single listing rather than from a second call.
    pub queued_runs: u64,
    pub total_schedules: u64,
    /// The ten most recent runs. Legacy derives this, the active list, the
    /// active count and the total from **one** listing, with an explicit comment
    /// that a second call costs a kube round-trip for no new information
    /// (`dashboard.rs:143-146`).
    pub recent_runs: Vec<DashboardRun>,
    pub recent_run_test_trend: Vec<RunTestTrendPoint>,
    pub daily_test_status_trend: Vec<DailyStatusPoint>,
    /// Capped at ten, using the same predicate as [`Self::active_runs`]
    /// (`dashboard.rs:152-157`) — so a dashboard reading "23 active" lists ten.
    pub active_runs_list: Vec<DashboardRun>,
    pub failed_recent: Vec<FailedTestCard>,
    pub failed_24h_count: u64,
    pub failed_prev_24h_count: u64,
    /// A **ratio in `[0, 1]`**, not a percentage: legacy divides and does not
    /// multiply (`dashboard.rs:357`).
    ///
    /// `None` when the window held no counted row — legacy distinguishes "no
    /// data" from "0%", and so does this. "Counted" is narrower than "any row":
    /// the denominator is `PASSED`+`FAILED`+`ERROR` (`:329`), so a window holding
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
    pub platforms_summary: PlatformsSummary,
    /// Pass rate per Quality Vector over the **last seven days**, highest total
    /// first — a fourth window, equal to [`Self::flaky_tests`]' and spelled
    /// separately by legacy (`dashboard.rs:490` against `:391`).
    ///
    /// One entry per vector declared by any test file with a row in the window,
    /// with the executions of every such file summed in. **A file declaring two
    /// vectors contributes to both** (`dashboard.rs:519-525`), so the
    /// [`QualityVectorPassRate::total`]s across this array are not a row count.
    ///
    /// "Any row", not "a counted row" — the distinct-file count is incremented
    /// unconditionally (`dashboard.rs:524`), so an entry of all zeros with a
    /// non-zero [`QualityVectorPassRate::tests`] is legal and means every
    /// contributing file was skipped.
    pub quality_vectors_pass_rate: Vec<QualityVectorPassRate>,
}

// ==================== Coverage ====================

/// Code coverage percentages.
///
/// Legacy `CoverageSummary` (`manager/src/models.rs:1482-1486`). Three fields,
/// three fields.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[allow(
    clippy::struct_field_names,
    reason = "legacy's own field names (manager/src/models.rs:1482-1486); they \
              are the JSON keys the coverage endpoint already emits, so renaming \
              them would be a wire change"
)]
pub struct CoverageSummary {
    pub line_pct: f64,
    pub branch_pct: f64,
    pub function_pct: f64,
}

/// Coverage for one product build.
///
/// Legacy `CoverageBuild` (`manager/src/routes/dashboard.rs:564-569` — the
/// plan's `:564` citation is exact). Four fields, four fields.
///
/// `GET /qa/v1/dashboard/coverage` takes **no parameters**: legacy's
/// `api_coverage` has none (`dashboard.rs:573`), returning one point per product
/// from the latest completed run that carried coverage.
///
/// # A consumer gets an empty list, in every deployment, today
///
/// Not a transient state and not an empty tenant: **nothing in this system
/// measures a coverage point yet.** Legacy parses these three percentages out of
/// runner log text, which qa-insights has no reader for, and groups them by a
/// `product_key` that `qa_runs_sdk::Run` does not carry. The single copy of that
/// argument, with its citations, is on `CoverageBuildDto` in
/// `qa-insights/src/api/rest/dto.rs`; it is not restated here, because five
/// near-copies of it drifted once already.
///
/// **Citation moved 2026-08-21 (Task 21b's doc split.)** It read "the "coverage
/// view" section of `qa-insights/src/domain/service/dashboard.rs`' module header"
/// until that transcript was moved onto the DTO, at the request of Phase A's
/// whole-phase review. The prose it points at is unchanged.
///
/// This type is nevertheless the whole legacy field set rather than a subset: the
/// *shape* of a point is known and it is the points that nothing produces, which
/// is also how legacy expresses "no coverage for this build".
#[derive(Clone, Debug, PartialEq)]
pub struct CoverageBuild {
    pub product_key: String,
    pub version: String,
    pub build: String,
    pub coverage: CoverageSummary,
}
