//! REST DTOs (`serde` + `utoipa`) for the qa-insights gear.
//!
//! These types never leak into the SDK or the domain layer — `qa-insights-sdk`
//! carries no `serde` or `utoipa` (contract-layer purity, `lib.rs`), and the
//! domain layer speaks SDK models plus `DomainError`, with no wire-contract
//! `serde` of its own. (Qualified in the Phase B fix wave, Finding 7: an
//! earlier revision of this line, and others like it, read as an absolute
//! "no `serde` at all" for the domain layer, which stopped being true once
//! `domain::service::collect` derived `serde::Serialize` on a private struct
//! to drive `serde_urlencoded` over its own outbound query string — see that
//! module's doc. It is not a wire type and never crosses this REST boundary,
//! so it does not weaken the claim this module's layering rests on; only the
//! absolute wording was wrong.) The conversions here are
//! the only bridge.
//!
//! # Instants are RFC 3339 on the wire, in both directions
//!
//! `#[serde(with = "time::serde::rfc3339")]` per field, matching every sibling's
//! response DTOs. It is load-bearing on a *request* for a reason it is not on a
//! response: `time`'s default `Deserialize` for `OffsetDateTime` is not the
//! RFC 3339 form, so an operator posting `"2026-08-18T08:00:00Z"` without this
//! attribute gets a deserialization failure rather than a window.
//!
//! # Days are dates, not instants
//!
//! The two chart axes are calendar days — `HeatmapDataDto::days` and
//! `TrendPointDto::day` — and they are rendered as `YYYY-MM-DD` rather than as
//! RFC 3339. [`iso_date`] is the one place that decides it.
//!
//! # Three things a reader might expect to be labels are ids
//!
//! The analytics payload identifies a run, an environment and a plan by **id**:
//! a run id, an environment id resolved through qa-environments, and a
//! `(repo_id, plan_path)` pair — never by a display name.
//! [`AnalyticsListItemDto`] carries the argument for all three, and
//! [`EnvironmentGroupSummaryDto`] carries the one case where the *shape* of a
//! payload changes as a result rather than only a field name.

use qa_insights_sdk::{
    CoverageBuild, CoverageSummary, DailyStatusPoint, DashboardRun, DashboardStats, FailedTestCard,
    FlakyTestCard, JiraBug, JiraConfig, JiraPollerConfig, NotificationConfig, NotificationLogEntry,
    QualityVectorPassRate, RunTestTrendPoint, SavedView, SavedViewScope, ScheduledRunSlackTemplate,
    ScheduledRunSlackTemplates, TestCaseResultRecord, TestResultRecord,
};
use std::collections::HashMap;

use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::domain::analytics::aggregates::{
    AnalyticsListItem, BuildLastRunDistribution, BuildTestDetail, FlakyTest, GroupBy, GroupSummary,
    HeatmapData, OverviewSummary, PlatformGroupSummary, QualityVectorSummary, TrendData,
};
use crate::domain::analytics::query::{OverviewQuery, Scope};
use crate::domain::error::DomainError;
use crate::domain::service::analytics::{
    AnalyticsOverview, BuildTestsQuery, PlanBuildDistribution, PlanTestAnalytics, PlanTestHistory,
    PlanTestHistoryEntry, PlanTests,
};
use crate::domain::service::jira::JiraConfigInput;
use crate::domain::service::reconcile::ReconcileOutcome;
use crate::domain::service::saved_views::SavedViewInput;

/// `POST /qa/v1/insights/rebuild` — the window to replay.
///
/// **Half-open: `[from, to)`.** Same convention as
/// `ResultsRepository::ingested_run_ids_between`, so two rebuilds that share an
/// endpoint neither skip a run nor replay one. Documented on the field rather
/// than only in the endpoint description, because this is the field an operator
/// gets wrong.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct RebuildReq {
    /// Start of the window, **inclusive**. RFC 3339, e.g.
    /// `2026-08-18T08:00:00Z`.
    #[serde(with = "time::serde::rfc3339")]
    pub from: OffsetDateTime,
    /// End of the window, **exclusive**. RFC 3339. Must be strictly after
    /// `from`; an equal pair is refused rather than treated as an empty window.
    #[serde(with = "time::serde::rfc3339")]
    pub to: OffsetDateTime,
}

/// What a rebuild did.
///
/// # `watermark_advanced_to` is deliberately not on the wire
///
/// `ReconcileOutcome` carries it, and for a rebuild it is always `None` —
/// meaningfully so: not touching the watermark is the endpoint's contract, not a
/// gap in it. A field that is structurally always `null` invites a client to
/// branch on it, and the first client to do so would be writing dead code
/// against a promise the *other* caller of `ReconcileOutcome` (the reconcile
/// ticker, which has no HTTP surface) does not keep. The endpoint's description
/// states the guarantee instead.
///
/// # `stopped_at_run` is not on the wire either, and that one is only a scope
/// # decision
///
/// `ReconcileOutcome::stopped_at_run` names the run a stopped pass stopped on.
/// It exists for the log line and for the alert the reconcile ticker raises
/// (Task 40 — `crate::gear`'s `report_reconcile_outcome`), which has no HTTP
/// surface at all — see
/// `domain::service::reconcile`'s header on how a permanently failing run wedges
/// a tenant's backfill. It would be *useful* here too: an operator reading
/// `stopped_at_gap: true` currently has to go to the logs to find out which run.
/// Adding it is an additive key on a shipped response and nothing here objects
/// to it; it is simply not this review wave's to add. Unlike
/// `watermark_advanced_to`, there is no argument that it should stay off.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct RebuildOutcomeDto {
    /// Runs found in the window and examined.
    pub scanned: usize,
    /// Runs whose projection was rewritten. Equal to `scanned` unless the
    /// rebuild stopped early — a rebuild replays every run in its window, it
    /// does not skip the ones that already had rows.
    pub replayed: usize,
    /// `true` when a run could not be re-projected and the rebuild stopped
    /// there. The runs after it were **not** replayed; re-running the same
    /// window is idempotent and is the intended recovery.
    pub stopped_at_gap: bool,
}

impl From<ReconcileOutcome> for RebuildOutcomeDto {
    fn from(o: ReconcileOutcome) -> Self {
        Self {
            scanned: o.scanned,
            // `backfilled` is the domain field's name, chosen for the sweep that
            // shipped first. On this surface the honest word is `replayed`: a
            // rebuild rewrites runs that already had a projection, which
            // "backfilled" would misdescribe. See `ReconcileOutcome::backfilled`.
            replayed: o.backfilled,
            stopped_at_gap: o.stopped_at_gap,
        }
    }
}

// ===========================================================================
// The two flat collections (Task 17)
// ===========================================================================

/// One file-level test outcome, as `GET /qa/v1/test-results` returns it.
///
/// Every field of [`TestResultRecord`], in its order. The record itself already
/// drops the two columns a consumer has no business reading — `tenant_id`, which
/// is the caller's own scope, and `updated_at`, which moves on a re-ingest of an
/// outcome that happened once (`infra::storage::mapper::test_result_to_sdk`).
///
/// **One contract field is withheld here, and it is the only one:
/// `run_created_at`.** This doc said "there is nothing further to withhold here
/// and this is a straight projection" until Task 21b added that column, so the
/// claim is corrected rather than left standing. It is the fallback half of the
/// dashboard's window expression — `COALESCE(run_finished_at, run_created_at)`
/// — denormalized so this gear's aggregates need no cross-gear join. On the
/// *wire* it is redundant:
/// qa-runs owns the run and a consumer of this collection can ask it for the
/// run's creation instant, where it cannot for a per-row aggregate on a hot path.
/// [`Self::run_finished_at`] is here because the pager and the analytics windows
/// publish it, not because the pair travels together. Publishing it is one
/// additive field if a consumer ever needs it.
///
/// **Most of it is nullable**, and nothing here is `Option` because the wire
/// likes it that way: each nullable field is a column that is genuinely absent for
/// a real run. Named rather than counted — `duration`, `launch_id`, `jira_key`,
/// `product_version`, `app_build`, `environment_id`, `repo_id`, `plan_path`,
/// `branch` and `run_finished_at`; only `id`, `run_id`, `test_file`, `test_name`,
/// `status` and `created_at` are always present. `run_finished_at` is `null` while
/// the run is still going, `repo_id`/`plan_path` are `null` for a custom-plan or
/// collect run, `app_build` is `null` when the run named no build. A client
/// rendering this must handle them; `qa_insights_sdk::TestResultRecord`'s field
/// docs say what each absence means.
///
/// **`logs` is not here and cannot be**, which is a gap rather than a design
/// choice: this gear's only source of outcomes is qa-runs, whose
/// `RunTestResult` carries no per-test log slice. The whole argument is on
/// [`TestResultRecord`], which is where it belongs — a DTO cannot expose a field
/// the contract does not have.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct TestResultDto {
    pub id: Uuid,
    pub run_id: Uuid,
    /// `""` — not `null` — when the producer reported no file. The column is
    /// `NOT NULL DEFAULT ''`, and a client grouping by file must treat the empty
    /// string as its own bucket rather than as missing data.
    pub test_file: String,
    pub test_name: String,
    /// Open set, uppercase **by producer convention and not by validation**:
    /// `PASSED` | `FAILED` | `ERROR` | `SKIPPED` | `PENDING` | `RUNNING` |
    /// `XFAIL` | `XPASS` today, and deliberately not an enum — a ninth value is a
    /// runner change, not corruption. Nothing in this gear re-cases it; see
    /// `infra::storage::mapper`'s note on the status columns, and
    /// `infra::storage::odata::TestCaseResultsField::Status` for what that means
    /// for a caller who filters on one.
    pub status: String,
    /// The runner's own duration text, verbatim — e.g. `85.06s (0:01:25)`. Not a
    /// number and not normalised — the runner's text is the contract.
    pub duration: Option<String>,
    pub launch_id: Option<String>,
    /// The **file**-level bug reference as the runner reported it. Distinct from
    /// [`TestCaseResultDto::ticket`], which is case-level.
    pub jira_key: Option<String>,
    pub product_version: Option<String>,
    /// The build under test. Not a duplicate of [`Self::product_version`]: that
    /// one is the analytics *filter*, this one the analytics *projection*.
    pub app_build: Option<String>,
    /// Renamed from `environment_id` (Task 25): the wire now agrees with the
    /// Rust field. The column moved with it: `environment_id` is now the
    /// column, the Rust field and the wire key alike. Every other
    /// `environment_id` on this crate's wire, whatever its own source entity,
    /// was renamed the same way — this is the one place it is spelled out
    /// in full.
    ///
    /// **This was a breaking API change** (Task 25): a client reading
    /// `environment_id` out of a response now finds it absent, replaced by
    /// `environment_id`. Every renamed field on this crate's wire is a
    /// response field - unlike `qa-runs`, nothing here is also a request
    /// field, so there is no 400 to raise on this crate's side of ruling G-4.
    pub environment_id: Option<Uuid>,
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    pub branch: Option<String>,
    /// `null` while the run is still going. **Filterable but not sortable**: the
    /// pager refuses it as an order key, because a nullable cursor key breaks
    /// pagination at the first in-progress row —
    /// `infra::storage::odata::TestResultsODataMapper::is_orderable` carries the
    /// measurement. Note that the published `$orderby` parameter *does* list it;
    /// that over-advertisement is `toolkit`'s and is pinned by
    /// `api::rest::routes::tests::the_orderby_parameter_over_advertises_the_nullable_instant`.
    #[serde(with = "time::serde::rfc3339::option")]
    pub run_finished_at: Option<OffsetDateTime>,
    /// When this gear ingested the row, not when the test ran. One instant per
    /// ingest batch, which is why it is not the pagination key — see
    /// `domain::repos::ResultsRepository::list_page`.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<TestResultRecord> for TestResultDto {
    fn from(r: TestResultRecord) -> Self {
        Self {
            id: r.id,
            run_id: r.run_id,
            test_file: r.test_file,
            test_name: r.test_name,
            status: r.status,
            duration: r.duration,
            launch_id: r.launch_id,
            jira_key: r.jira_key,
            product_version: r.product_version,
            app_build: r.app_build,
            environment_id: r.environment_id,
            repo_id: r.repo_id,
            plan_path: r.plan_path,
            branch: r.branch,
            run_finished_at: r.run_finished_at,
            created_at: r.created_at,
        }
    }
}

/// One test *function* outcome, as `GET /qa/v1/test-case-results` returns it.
///
/// A visibly different shape from [`TestResultDto`] and not a subset of it: the
/// function-name column is `name`, not `test_name`, the bug reference is
/// `ticket` rather
/// than `jira_key`, and there are no denormalized run columns at all — case rows
/// reach every aggregate through the file-level table first, so a second copy of
/// the run's identity would be a second thing to keep true.
///
/// That also means there is **no timestamp to filter or sort a case row by other
/// than `created_at`**, which is the ingest instant. A caller who wants a time
/// window filters `run_id` from the file-level collection.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct TestCaseResultDto {
    pub id: Uuid,
    pub run_id: Uuid,
    /// The owning file. `NOT NULL` with no default and never `""`-defaulted,
    /// unlike [`TestResultDto::test_file`] — a case row always knows its file.
    pub test_file: String,
    /// pytest's node id — `tests/test_x.py::TestC::test_m[param]` — and `""` when
    /// the producer reported none.
    pub nodeid: String,
    /// The test function's name. Spelled `name`, not `test_name`; see this type's
    /// header.
    pub name: String,
    /// Open set, as [`TestResultDto::status`], plus `XFAIL`/`XPASS` which the
    /// case level is where they actually appear. **The one indexed status
    /// column** — `$filter=status eq 'FAILED'` is a range seek here and is not
    /// available at all on the file-level collection.
    ///
    /// **That filter is an exact, case-sensitive match on whatever the runner
    /// reported**, because nothing in this pipeline normalises the value:
    /// `status eq 'XFAIL'` does not find a row holding `xfail`, and it answers an
    /// empty page rather than an error.
    /// `infra::storage::odata::TestCaseResultsField::Status` has the measurement
    /// and the citations.
    pub status: String,
    pub duration: Option<String>,
    /// The xfail/skip explanation, if the marker gave one. `null` rather than
    /// `""`, because here absence *is* information.
    pub reason: Option<String>,
    /// The **case**-level bug reference; see [`TestResultDto::jira_key`].
    pub ticket: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<TestCaseResultRecord> for TestCaseResultDto {
    fn from(r: TestCaseResultRecord) -> Self {
        Self {
            id: r.id,
            run_id: r.run_id,
            test_file: r.test_file,
            nodeid: r.nodeid,
            name: r.name,
            status: r.status,
            duration: r.duration,
            reason: r.reason,
            ticket: r.ticket,
            created_at: r.created_at,
        }
    }
}

// ==================== Dashboard ====================

/// Query parameters for `GET /qa/v1/dashboard`.
///
/// A plain `serde::Deserialize` rather than an `api_dto`, matching
/// `qa-catalog`'s `ListPlansQuery`: a query struct is never a request *body*, so
/// it needs no `ToSchema` — the parameter and its description are declared on the
/// `OperationBuilder` instead.
///
/// # Two parameters, and the one that is deliberately absent
///
/// **There is no `product_key` parameter.** `qa_runs_sdk::Run` carries nothing
/// that identifies a product by key — qa-catalog owns products, and a run is
/// attributed to one through its target. A `product_key` accepted here would
/// have to be silently ignored, which is worse than not accepting it;
/// `domain::service::dashboard::dashboard_run` records the same gap.
///
/// **`product_id` is the parameter that does the scoping**, added by the task
/// that scopes the global product switcher's figures server-side. The join
/// `domain::service::dashboard::DashboardService::stats` performs with it is the
/// one `domain::service::analytics` already uses for the same id: a run is
/// attributed to a product through its target, never its environment, matching
/// the client's own rule (`qa-platform-ui/src/lib/productScope.ts`).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DashboardQuery {
    /// Days of history for the daily trend. Defaults to 14 and is **silently
    /// clamped** to `[3, 90]` — see
    /// `domain::service::dashboard::resolve_days` for why the clamp is not a 400.
    pub days: Option<u32>,
    /// Narrow every run-derived number to one product. Optional, and absent
    /// preserves today's deployment-wide behaviour exactly — see
    /// `domain::service::dashboard::DashboardService::stats`' `product_id`
    /// section for what does and does not narrow, and for why a malformed UUID
    /// is a 400 from the extractor rather than an ignored filter.
    pub product_id: Option<Uuid>,
}

/// One run as the dashboard draws it.
///
/// `qa_insights_sdk::DashboardRun`, which is itself a projection of
/// `qa_runs_sdk::Run` — that type's header says why this gear's contract does not
/// depend on qa-runs'.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct DashboardRunDto {
    pub run_id: Uuid,
    /// The run's human-facing name, `{slug}-{n}`.
    pub name: String,
    /// The run's lifecycle state, in **qa-runs' own lowercase spelling**:
    /// `created` | `queued` | `dispatching` | `running` | `succeeded` | `failed`
    /// | `canceled` | `timed_out` | `expired` | `error`. Named `phase` because
    /// that is what the card calls the column; the values are qa-runs' persisted
    /// spellings and never an execution backend's. Open set: a new run state is a
    /// qa-runs change, not corruption.
    pub phase: String,
    /// The plan this run targeted, as the `(repo_id, plan_path)` pair this port
    /// uses in place of a plan id. Both are `null` for a custom-plan or collect
    /// run.
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    /// The environment the run occupied, as an id. Resolving it to a display
    /// name is a qa-environments lookup this gear does not make yet.
    ///
    /// Sourced from `qa_runs_sdk::Run::environment_id` (this crate's own
    /// `test_result::Model` is not involved here — this row never touches
    /// `qa_test_results`), so it is qa-runs' own physical column, one gear
    /// over, that this field projects. Renamed from `environment_id` (Task 25)
    /// — see [`TestResultDto::environment_id`]'s doc for why: the same
    /// rename applies on both sides of the boundary, even though the source
    /// column this field is sourced from is qa-runs', not this crate's own.
    pub environment_id: Option<Uuid>,
    /// **Always `null` today.** `qa_runs_sdk::Run` carries no product key — see
    /// [`DashboardQuery`] for the same gap and who owns closing it. Present on the
    /// wire rather than omitted because the active-runs card draws it, so a
    /// client can bind the field now and see it populate later.
    pub product_key: Option<String>,
    pub app_version: Option<String>,
    /// `null` until the run starts.
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// The rendered duration text — `"2m 5s"` or `"45s"`.
    /// `null` unless the run has both
    /// a start and a finish, so a running run has none and a client showing
    /// elapsed time computes it from [`Self::started_at`].
    pub duration: Option<String>,
}

impl From<DashboardRun> for DashboardRunDto {
    fn from(run: DashboardRun) -> Self {
        Self {
            run_id: run.run_id,
            name: run.name,
            phase: run.phase,
            repo_id: run.repo_id,
            plan_path: run.plan_path,
            environment_id: run.environment_id,
            product_key: run.product_key,
            app_version: run.app_version,
            started_at: run.started_at,
            duration: run.duration,
        }
    }
}

/// Test volume for one run on the trend chart.
///
/// **`passed + failed + skipped` need not equal [`Self::tests_total`]**, and
/// that is the arithmetic rather than a rounding artefact: the total is a plain
/// `COUNT` while the three counters are filters, so an `XFAIL`, `XPASS`,
/// `PENDING` or `RUNNING` row is in the total and in none of them.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct RunTestTrendPointDto {
    pub run_id: Uuid,
    pub run_name: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// Every result row of the run. Zero for a run whose results this gear has
    /// not ingested yet, which asynchronous ingest makes a normal state.
    pub tests_total: u64,
    pub passed: u64,
    /// `FAILED` **and** `ERROR`, which are folded together in every aggregate.
    pub failed: u64,
    pub skipped: u64,
}

impl From<RunTestTrendPoint> for RunTestTrendPointDto {
    fn from(point: RunTestTrendPoint) -> Self {
        Self {
            run_id: point.run_id,
            run_name: point.run_name,
            started_at: point.started_at,
            tests_total: point.tests_total,
            passed: point.passed,
            failed: point.failed,
            skipped: point.skipped,
        }
    }
}

/// One day of the pass/fail trend.
///
/// **Two counters, not four.** The daily fold counts `PASSED` and
/// `IN ('FAILED','ERROR')` and nothing else, so a skipped test moves neither —
/// a different reading of the same rows from [`RunTestTrendPointDto`]'s, and
/// deliberately so.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct DailyStatusPointDto {
    /// `YYYY-MM-DD`, UTC. A string rather than a date type, and formatted here
    /// rather than left to a serde attribute so the wire shape does not depend on
    /// which date features the workspace's `time` and `utoipa` happen to have.
    pub day: String,
    pub passed: u64,
    /// `FAILED` **and** `ERROR`.
    pub failed: u64,
}

impl From<DailyStatusPoint> for DailyStatusPointDto {
    fn from(point: DailyStatusPoint) -> Self {
        Self {
            // `%Y-%m-%d` by hand: `time::format_description` would need a runtime
            // parse or a macro, and a four-digit year with zero-padded month and
            // day is the whole format. `time::Date::year()` can be negative and
            // `{:04}` would then render five characters — unreachable for a
            // dashboard window and not worth a fallible conversion.
            day: format!(
                "{:04}-{:02}-{:02}",
                point.day.year(),
                u8::from(point.day.month()),
                point.day.day(),
            ),
            passed: point.passed,
            failed: point.failed,
        }
    }
}

/// One recent failure, as the dashboard's failure card.
///
/// `qa_insights_sdk::FailedTestCard`, keyed by a run id and by the plan pair.
/// Every field is
/// on the wire: unlike [`DashboardStatsDto`], nothing here is omitted, because
/// every column is computed — the list itself is empty when nothing failed, which
/// is a measurement rather than a gap.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct FailedTestCardDto {
    pub test_name: String,
    /// `null` when the producer reported no file — see
    /// `qa_insights_sdk::FailedTestCard::test_file` for why an empty string is
    /// not used to mean this.
    pub test_file: Option<String>,
    /// The run the failure came from, as an id — a client that wants the run's
    /// name resolves it through qa-runs.
    pub run_id: Uuid,
    /// The plan the run targeted, as the `(repo_id, plan_path)` pair this port
    /// uses in place of a plan id — the same substitution
    /// [`DashboardRunDto`] makes. Both are `null` for a custom-plan or collect
    /// run.
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    /// The environment the run occupied, as an id rather than a name — the same
    /// substitution [`DashboardRunDto::environment_id`] documents.
    /// Renamed from `environment_id` (Task 25) — see
    /// [`TestResultDto::environment_id`]'s doc for why.
    pub environment_id: Option<Uuid>,
    /// When the failure's run finished, falling back to when the row was
    /// ingested. The fallback is why a run still in progress can appear on this
    /// list at all.
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// The **file**-level bug key the runner reported, off the result row. Not
    /// this gear's JIRA registry, which is a different thing keyed differently.
    pub jira_key: Option<String>,
    /// `ReportPortal` launch link, as the runner reported it.
    pub launch_id: Option<String>,
}

impl From<FailedTestCard> for FailedTestCardDto {
    fn from(card: FailedTestCard) -> Self {
        Self {
            test_name: card.test_name,
            test_file: card.test_file,
            run_id: card.run_id,
            repo_id: card.repo_id,
            plan_path: card.plan_path,
            environment_id: card.environment_id,
            finished_at: card.finished_at,
            jira_key: card.jira_key,
            launch_id: card.launch_id,
        }
    }
}

/// A test that both passed and failed inside the dashboard's seven-day window.
///
/// `qa_insights_sdk::FlakyTestCard`, with the plan carried as the
/// `(repo_id, plan_path)` pair — that crate's note 1 argues why.
///
/// Every field is emitted, `test_file`, `repo_id` and `plan_path` as `null` when
/// absent — none of them is subject to [`DashboardStatsDto`]' omission
/// convention, which is about a *quantity nothing computed* rather than about a
/// value a row genuinely does not carry.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct FlakyTestCardDto {
    pub test_name: String,
    /// The file the runner reported this test from, `MAX`ed over the group as a
    /// representative pick, because the file is not one of the grouping keys.
    /// `null` when no row of the group named one.
    pub test_file: Option<String>,
    /// The plan's repository; half of the plan identity. See this type's note.
    pub repo_id: Option<Uuid>,
    /// The plan's `plan.yaml` path within that repository. The other half.
    pub plan_path: Option<String>,
    /// Rows of this test-and-plan that passed inside the window. **Strictly
    /// positive** — a test that only ever passed is not flaky and is not here.
    pub passed: u64,
    /// Rows that failed or errored. Strictly positive, on the same rule.
    pub failed: u64,
    /// Passed plus failed. **Not every row of the group**: a skipped or
    /// in-progress row is in no counter here — a sixth status classification —
    /// which is why this is emitted rather than left to the client to add up.
    pub total: u64,
}

impl From<FlakyTestCard> for FlakyTestCardDto {
    fn from(card: FlakyTestCard) -> Self {
        Self {
            test_name: card.test_name,
            test_file: card.test_file,
            repo_id: card.repo_id,
            plan_path: card.plan_path,
            passed: card.passed,
            failed: card.failed,
            total: card.total,
        }
    }
}

/// One Quality Vector's pass rate over the dashboard's seven-day window.
///
/// `qa_insights_sdk::QualityVectorPassRate`.
///
/// # The sums across the array exceed the row count, by design
///
/// A test file declaring two vectors contributes its counters to **both**, so
/// adding [`Self::total`] over the array double-counts and is not a row count.
/// The vectors partition *concerns*, not executions, and a client that summed
/// them would be computing nothing.
/// `domain::service::dashboard`'s `quality_vector_pass_rates` carries the five
/// properties of the fold, three of which look like defects and are not.
///
/// # Two spellings of one vector can both appear
///
/// `Security` and `security` declared by two different files are two entries,
/// not one: this fold keys on the display string where the *analytics* fold
/// case-folds. The asymmetry is deliberate, the fold's doc records it and a test
/// pins it, so a client must not assume the vector names are a case-normalized
/// set.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct QualityVectorPassRateDto {
    /// The vector as some file spelled it, trimmed. See this type's note on
    /// casing.
    pub vector: String,
    /// Rows of this vector's files that passed inside the window.
    pub passed: u64,
    /// Rows that failed or errored.
    pub failed: u64,
    /// Passed plus failed. **Not every row**: a skipped or in-progress row is in
    /// no counter here — the same sixth status classification
    /// [`FlakyTestCardDto::total`] carries. Emitted rather than left to the
    /// client to add up, for that reason.
    pub total: u64,
    /// Distinct **test files** carrying this vector that had **any** row in the
    /// window, as opposed to [`Self::total`] executions. A file listed by two
    /// plans counts once.
    ///
    /// **Any row, not a counted one.** This counter increments unconditionally,
    /// with no test on the other three,
    /// so a file whose window holds nothing but `SKIPPED` rows contributes here
    /// and to none of them. `("Security", 0, 0, 0, 5)` is therefore a legal and
    /// meaningful row — five files carry the vector and none of them was counted
    /// — and a client must not read `tests > 0` as implying `total > 0`.
    pub tests: u64,
}

impl From<QualityVectorPassRate> for QualityVectorPassRateDto {
    fn from(rate: QualityVectorPassRate) -> Self {
        Self {
            vector: rate.vector,
            passed: rate.passed,
            failed: rate.failed,
            total: rate.total,
            tests: rate.tests,
        }
    }
}

/// What `GET /qa/v1/dashboard` answers with.
///
/// # Fourteen fields, where the contract type has seventeen
///
/// Each of the three missing ones needs a read or an upstream this gear does not
/// have yet; they are enumerated on `qa_insights_sdk::DashboardStats` and in
/// `domain::service::dashboard`'s header — which also records that
/// `cpt-cf-qa-fr-insights-dashboard` is **not** discharged by this endpoint, and
/// that its coverage half is not discharged in this feature at all.
///
/// **They are omitted from this DTO rather than emitted as zeros**, which is the
/// decision worth recording. `"total_plans": 0` is indistinguishable from a
/// measured zero, and a client that renders it is reporting a number nothing
/// computed; an absent key cannot be misread that way, and adding a key later is
/// a compatible change while correcting a wrong one is not. The cost is that a
/// client must tolerate the absence — accepted, because the alternative ships a
/// plausible lie.
///
/// The three left — `total_plans`, `total_schedules`, `environments_summary` — are
/// the ones no task owns. Two fields have left this list rather than joined it:
/// [`Self::flaky_tests`] and `quality_vectors_pass_rate` are computed now.
///
/// # A `null` pass rate is a measurement, and that is a different thing
///
/// [`Self::pass_rate_24h`] is present and may be `null`, which is **not** the
/// omission convention above: the field is computed, and `null` spells "the
/// window held nothing to divide by" — an `Option<f64>` left at `None` behind an
/// `if total > 0` guard. A `0.0` there would say "everything failed", so the
/// distinction is load-bearing on the wire and not only in the domain.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct DashboardStatsDto {
    /// Runs this gear holds results for. **Not** a count of every run ever
    /// launched — `domain::repos::ResultsRepository::count_ingested_runs` records
    /// why that number is not obtainable across the gear boundary.
    pub total_runs: u64,
    /// Runs qa-runs currently reports as `dispatching` or `running`.
    pub active_runs: u64,
    /// Runs admitted to the queue and not yet handed to an executor.
    pub queued_runs: u64,
    /// The ten most recent runs, newest first.
    pub recent_runs: Vec<DashboardRunDto>,
    /// One point per entry of [`Self::recent_runs`], in the same order.
    pub recent_run_test_trend: Vec<RunTestTrendPointDto>,
    /// One point per day of the requested window, ascending, ending today. Every
    /// day is present, including the days nothing ran.
    pub daily_test_status_trend: Vec<DailyStatusPointDto>,
    /// At most ten of the [`Self::active_runs`], using the same predicate as the
    /// count — so a client reading "23 active" gets ten entries here.
    pub active_runs_list: Vec<DashboardRunDto>,
    /// The ten newest test failures of the last 24 hours, newest first.
    ///
    /// A different window from [`Self::daily_test_status_trend`]'s and it does
    /// **not** move with `days`; and a different row set from every other number
    /// here, because it includes runs that have not finished. Empty means nothing
    /// failed in that window.
    pub failed_recent: Vec<FailedTestCardDto>,
    /// Failed and errored test rows in the last 24 hours.
    ///
    /// Counted over the same rows as [`Self::failed_recent`], which is not the
    /// same rule as the trend counters above: there is no phase or finished-run
    /// restriction, so a run still in progress contributes. `ERROR` is counted as
    /// a failure, as everywhere else in this payload.
    pub failed_24h_count: u64,
    /// The same count over the 24 hours before that, so a client can draw a
    /// delta.
    pub failed_prev_24h_count: u64,
    /// Passed rows over passed-plus-failed rows in the last 24 hours, as a
    /// **ratio in `[0, 1]`** rather than a percentage.
    ///
    /// Skipped and in-progress rows are in neither the numerator nor the
    /// denominator, so a plan that skips half its tests does not lower this.
    /// `null` when the window held no passed or failed row at all — see this
    /// type's note on `null` versus omission.
    pub pass_rate_24h: Option<f64>,
    /// The same ratio over the 24 hours before that. `null` on the same rule.
    pub pass_rate_prev_24h: Option<f64>,
    /// The ten flakiest tests of the **last seven days**, flakiest first.
    ///
    /// One entry per `(test_name, repo_id, plan_path)` that both passed and failed
    /// in that window; a test that only passed or only failed is absent rather
    /// than present with a zero. "Flakiest" is the size of the smaller of the two
    /// counters, so 40 passes against 40 failures outranks 79 against 1, and ties
    /// are broken by the larger total.
    ///
    /// **A third window, and it does not move with `days` either** — seven days
    /// where [`Self::failed_recent`]'s is 24 hours. Empty means nothing in the
    /// window both passed and failed, which on a healthy suite is the ordinary
    /// answer.
    pub flaky_tests: Vec<FlakyTestCardDto>,
    /// Pass rate per Quality Vector over the **last seven days**, highest total
    /// first.
    ///
    /// One entry per vector declared by any test file with a row in the window,
    /// with the executions of every such file summed into it. **A fourth
    /// window**, equal to [`Self::flaky_tests`]' and independent of it — the
    /// seven days are a separate literal in a separate statement, so the two can
    /// diverge without either moving silently.
    ///
    /// "Any row", not "a counted row": an entry whose three counters are all zero
    /// is legal and means every contributing file was skipped — see
    /// [`QualityVectorPassRateDto::tests`].
    ///
    /// Empty means one of three things and the payload does not distinguish them:
    /// no file had a row in the window at all, no such file is in the catalog's
    /// universe, or no file in it declares a vector. All three are ordinary
    /// states; none is an error.
    ///
    /// The array's [`QualityVectorPassRateDto::total`]s **do not sum to a row
    /// count** — see that type.
    pub quality_vectors_pass_rate: Vec<QualityVectorPassRateDto>,
    /// How many runs a `product_id` filter dropped because they could not be
    /// attributed to *any* product — a custom-plan run, never one that simply
    /// belongs to a different product. **Always `0` when `product_id` was
    /// omitted.** A client renders a caveat exactly when this is non-zero,
    /// rather than unconditionally on every product-scoped view.
    pub unattributable_runs: u64,
}

impl From<DashboardStats> for DashboardStatsDto {
    fn from(stats: DashboardStats) -> Self {
        Self {
            total_runs: stats.total_runs,
            active_runs: stats.active_runs,
            queued_runs: stats.queued_runs,
            recent_runs: stats
                .recent_runs
                .into_iter()
                .map(DashboardRunDto::from)
                .collect(),
            recent_run_test_trend: stats
                .recent_run_test_trend
                .into_iter()
                .map(RunTestTrendPointDto::from)
                .collect(),
            daily_test_status_trend: stats
                .daily_test_status_trend
                .into_iter()
                .map(DailyStatusPointDto::from)
                .collect(),
            active_runs_list: stats
                .active_runs_list
                .into_iter()
                .map(DashboardRunDto::from)
                .collect(),
            failed_recent: stats
                .failed_recent
                .into_iter()
                .map(FailedTestCardDto::from)
                .collect(),
            failed_24h_count: stats.failed_24h_count,
            failed_prev_24h_count: stats.failed_prev_24h_count,
            pass_rate_24h: stats.pass_rate_24h,
            pass_rate_prev_24h: stats.pass_rate_prev_24h,
            flaky_tests: stats
                .flaky_tests
                .into_iter()
                .map(FlakyTestCardDto::from)
                .collect(),
            quality_vectors_pass_rate: stats
                .quality_vectors_pass_rate
                .into_iter()
                .map(QualityVectorPassRateDto::from)
                .collect(),
            unattributable_runs: stats.unattributable_runs,
        }
    }
}

// ==================== Coverage ====================

/// Code coverage percentages for one build.
///
/// The three field names are the JSON keys the coverage endpoint emits and the
/// ones its chart reads, so they are wire contract rather than taste.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
#[expect(
    clippy::struct_field_names,
    reason = "these are the JSON keys the coverage endpoint emits, so renaming them \
              would be a wire change. `expect` rather than `allow`: if the lint \
              stops firing on the `_pct` postfix this reason has become a claim \
              about nothing, and the gate should say so"
)]
pub struct CoverageSummaryDto {
    pub line_pct: f64,
    pub branch_pct: f64,
    pub function_pct: f64,
}

impl From<CoverageSummary> for CoverageSummaryDto {
    fn from(summary: CoverageSummary) -> Self {
        Self {
            line_pct: summary.line_pct,
            branch_pct: summary.branch_pct,
            function_pct: summary.function_pct,
        }
    }
}

/// One build's coverage, as `GET /qa/v1/dashboard/coverage` returns it.
///
/// Every field of `qa_insights_sdk::CoverageBuild`.
///
/// # The array is empty today, and no *field* is omitted to say so
///
/// The wire consequence first, because it differs deliberately from
/// [`DashboardStatsDto`]'s omission convention. There, a *field* nothing computes
/// is absent from the payload; here, an *entry* nothing measures is absent from
/// the array. A point that did exist would carry all four fields, so all four are
/// declared and a client can bind them now.
///
/// # Why it is empty
///
/// Recorded at length because an empty answer invites the wrong explanations. It
/// is neither a transient state nor an empty tenant: **nothing in this system
/// produces a coverage point, and two upstreams are missing before one could.**
///
/// * **The percentages would have to come from log text, and this gear has
///   none.** The sole source of run data here is qa-runs, and no method of its
///   client returns log text — by design rather than omission: a run's durable
///   log lives in `qa_run_logs` and is served only as an SSE stream, and
///   `qa_runs_sdk` carries no log slice on any model. It is the same gap
///   `qa_insights_sdk::TestResultRecord` records for its missing `logs` column,
///   one granularity up.
/// * **The grouping key does not exist either.** `qa_runs_sdk::Run` carries no
///   product key: qa-catalog owns products, and a run is attributed to one
///   through its target. This crate records the same gap twice more, on
///   `domain::service::dashboard::dashboard_run` and on [`DashboardQuery`], whose
///   `product_key` parameter is dropped for it.
///
/// So the empty array is the honest answer. What is deliberately **not** done is
/// the available temptation: folding something out of `qa_test_results` and
/// putting it under `line_pct`. Test-status counts are not code coverage, and a
/// plausible number under a coverage key is exactly the "plausible lie"
/// [`DashboardStatsDto`]'s omission convention refuses one level up — that
/// convention omits a *field* nothing computes, this omits an *entry* nothing
/// measures.
///
/// # The requirement and this endpoint do not describe the same quantity
///
/// `cpt-cf-qa-fr-insights-dashboard` (PRD §5.5) phrases coverage as "a coverage
/// view (which tests and plans ran against which product versions and
/// environments)" — *execution* coverage, for which `qa_test_results` does have
/// columns. This endpoint's shape answers *code* coverage: `line_pct`,
/// `branch_pct` and `function_pct`. **Which of the two readings the clause should
/// have is open**, and `gears/qa-platform/docs/DESIGN.md` §3.5 records it as
/// such; nothing here settles it and nothing here discharges the clause.
///
/// # It still compiles a PEP decision, and the objection to that is real
///
/// [`DashboardService::coverage`](crate::domain::service::dashboard::DashboardService::coverage)
/// compiles the same decision
/// [`DashboardService::stats`](crate::domain::service::dashboard::DashboardService::stats)
/// does, and then reads nothing. The objection is that a decision protecting no
/// data is ceremony. It is made anyway because the endpoint's authorization
/// contract must not change under a caller when the upstream lands: a client that
/// works today and starts receiving 403 the moment the first real point is
/// computed is a breaking change nobody would have chosen, and the `403` this
/// route publishes has to be reachable to be true. Pinned by
/// `a_denied_caller_is_refused_rather_than_answered_with_an_empty_coverage_list`
/// and `the_coverage_view_authorizes_under_test_result_list_once`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct CoverageBuildDto {
    /// The product this point is for. Never empty: a run with a blank product
    /// key contributes no point.
    pub product_key: String,
    /// The application version the run reported (`app_version`), never blank for
    /// the same reason.
    pub version: String,
    /// The chart's x-axis label — the product key and the version joined by a
    /// slash. A **label**, not a build identifier, and unrelated to
    /// `qa_test_results.app_build`.
    pub build: String,
    pub coverage: CoverageSummaryDto,
}

impl From<CoverageBuild> for CoverageBuildDto {
    fn from(build: CoverageBuild) -> Self {
        Self {
            product_key: build.product_key,
            version: build.version,
            build: build.build,
            coverage: CoverageSummaryDto::from(build.coverage),
        }
    }
}

// ---------------------------------------------------------------------------
// Analytics — the overview and its build-tests drill-down (Task 25b)
// ---------------------------------------------------------------------------

/// `GET /qa/v1/analytics/overview` — its nine query parameters.
///
/// **Every one of them is untyped here on
/// purpose**: `product_id` is a `String` and not a `Uuid` because a `Uuid` field
/// moves the "`product_id` is required" rejection into the deserializer, which
/// answers with its own message and its own shape —
/// `domain::analytics::query`'s header carries that argument in full, and
/// `domain::service::analytics::product_uuid` is where the parse actually
/// happens.
///
/// No `#[toolkit_macros::api_dto(request)]`, matching [`DashboardQuery`]: this is
/// a query string rather than a body, so the route declares each parameter
/// through `OperationBuilder::query_param_typed` and there is no component
/// schema to register.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnalyticsOverviewQuery {
    /// Required, non-blank, and a UUID once the service parses it.
    pub product_id: String,
    /// Required, non-blank. The `product_version` predicate on the rows.
    pub version: String,
    /// Required. `all` or `plan`, case-insensitively.
    pub scope: String,
    /// Required when `scope=plan`, and the plan's **path**.
    pub plan_id: Option<String>,
    /// Absent or blank means *every* branch on the rows, and each repository's
    /// **default** branch in the universe — the two readings run as a pair.
    pub branch: Option<String>,
    /// Defaults to 7, clamped to `[1, 30]`.
    pub days_heatmap: Option<u32>,
    /// Defaults to 90, clamped to `[7, 365]`.
    pub days_trend: Option<u32>,
    /// Defaults to `none`. A **present but blank** value is a 400 — an
    /// asymmetry with the fields above, and not a typo.
    pub group_by: Option<String>,
    /// Blank narrows nothing.
    pub group_value: Option<String>,
}

impl From<AnalyticsOverviewQuery> for OverviewQuery {
    fn from(query: AnalyticsOverviewQuery) -> Self {
        Self {
            product_id: query.product_id,
            version: query.version,
            scope: query.scope,
            plan_id: query.plan_id,
            branch: query.branch,
            days_heatmap: query.days_heatmap,
            days_trend: query.days_trend,
            group_by: query.group_by,
            group_value: query.group_value,
        }
    }
}

/// `GET /qa/v1/analytics/build-tests` — its eight query parameters.
///
/// Seven of the overview's nine, plus `build`. The two it does not take are the
/// day counts, because the drill-down draws no chart.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnalyticsBuildTestsQuery {
    pub product_id: String,
    pub version: String,
    pub scope: String,
    pub plan_id: Option<String>,
    pub branch: Option<String>,
    pub group_by: Option<String>,
    pub group_value: Option<String>,
    /// Required, non-blank — and refused **before** every rule above, so a
    /// request that is wrong in both ways is told about the build.
    pub build: String,
}

impl From<AnalyticsBuildTestsQuery> for BuildTestsQuery {
    fn from(query: AnalyticsBuildTestsQuery) -> Self {
        Self {
            product_id: query.product_id,
            version: query.version,
            scope: query.scope,
            plan_id: query.plan_id,
            branch: query.branch,
            group_by: query.group_by,
            group_value: query.group_value,
            build: query.build,
        }
    }
}

/// `GET /qa/v1/analytics/export` — its eleven query parameters. Task 26.
///
/// The overview's nine plus `format` and `section`. Both extra fields are
/// untyped
/// `Option<String>`s, deliberately: neither has a deserializer-level shape to
/// enforce, and [`crate::domain::analytics::export::is_csv_format`] and
/// [`crate::domain::analytics::export::normalize_export_section`] do their own
/// trim/case-fold/default before either vocabulary is consulted.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnalyticsExportQuery {
    pub product_id: String,
    pub version: String,
    pub scope: String,
    pub plan_id: Option<String>,
    pub branch: Option<String>,
    pub days_heatmap: Option<u32>,
    pub days_trend: Option<u32>,
    pub group_by: Option<String>,
    pub group_value: Option<String>,
    /// `csv` (case-insensitively) selects CSV; anything else, including
    /// absent, selects JSON. No rejection for an unrecognized value — see
    /// `domain::analytics::export`'s header.
    pub format: Option<String>,
    /// `summary` | `lists` | `heatmap` | `trend` | `flaky` | `all` (default).
    /// Validated only on the JSON branch; the CSV branch renders an unknown
    /// value as an empty body — the same module's header carries why.
    pub section: Option<String>,
}

impl From<AnalyticsExportQuery> for OverviewQuery {
    fn from(query: AnalyticsExportQuery) -> Self {
        Self {
            product_id: query.product_id,
            version: query.version,
            scope: query.scope,
            plan_id: query.plan_id,
            branch: query.branch,
            days_heatmap: query.days_heatmap,
            days_trend: query.days_trend,
            group_by: query.group_by,
            group_value: query.group_value,
        }
    }
}

/// The universe partitioned three ways, plus the per-case counters.
///
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct OverviewSummaryDto {
    /// Every test in the (filtered) universe. [`Self::passed`],
    /// [`Self::failed`] and [`Self::not_run`] partition it, so those three sum
    /// to this one.
    pub total: usize,
    pub passed: usize,
    /// Includes `ERROR`, which buckets as failed everywhere in this subsystem.
    pub failed: usize,
    /// Everything that is neither — including `SKIPPED`, and including a test
    /// with no execution row at all.
    pub not_run: usize,
    /// Percentages to one decimal place, over [`Self::total`]. `0.0` rather than
    /// `null` for an empty universe.
    pub passed_pct: f64,
    pub failed_pct: f64,
    pub not_run_pct: f64,
    /// The six per-case counters, over each test's **latest run**. They do
    /// **not** partition [`Self::case_total`]: a file whose latest run reported
    /// no per-case rows contributes one synthetic case of its file-level status,
    /// and only if that status is `PASSED` or `FAILED`.
    pub case_total: usize,
    pub case_passed: usize,
    pub case_failed: usize,
    pub case_skipped: usize,
    pub case_xfail: usize,
    pub case_xpass: usize,
    /// The number of test cases the universe is expected to contain, available
    /// without any run. `summarize` leaves it at `0` and
    /// `AnalyticsService::overview` fills it in afterwards.
    ///
    /// **Per file, the collect job's exact count wins where one exists, and
    /// the static count parsed out of the test source is the fallback** —
    /// never the other way, and never a whole-payload choice of one source or
    /// the other: `domain::analytics::universe::expected_cases` (Task 29) mixes
    /// the two per file. The static source is
    /// `qa_catalog_sdk::UniverseTest::static_case_count`, present on every
    /// universe entry the overview reads; its own doc records the same
    /// precedence from the catalog side. Because that fallback needs no
    /// collect report at all, **this field is non-zero on a deployment that
    /// has never run a collect job**.
    pub case_expected: usize,
}

impl From<OverviewSummary> for OverviewSummaryDto {
    fn from(summary: OverviewSummary) -> Self {
        Self {
            total: summary.total,
            passed: summary.passed,
            failed: summary.failed,
            not_run: summary.not_run,
            passed_pct: summary.passed_pct,
            failed_pct: summary.failed_pct,
            not_run_pct: summary.not_run_pct,
            case_total: summary.case_total,
            case_passed: summary.case_passed,
            case_failed: summary.case_failed,
            case_skipped: summary.case_skipped,
            case_xfail: summary.case_xfail,
            case_xpass: summary.case_xpass,
            case_expected: summary.case_expected,
        }
    }
}

/// One test as the three overview lists draw it.
///
/// **Three of its fields carry an id where a reader might expect a label**, and
/// each is named for what it carries:
///
/// * The plan is [`Self::repo_id`] + [`Self::plan_path`]. A plan has no UUID in
///   this subsystem — `qa_insights_sdk`'s header records that it is materialized
///   on read from qa-catalog — so the pair *is* the identity.
/// * The environment is [`Self::last_environment_id`] **and**
///   [`Self::last_environment`] (renamed from `last_environment_id`/`last_platform`
///   at ruling G-3): the id, and the name qa-environments resolved for it. Both,
///   rather than only the name, because the name is `null` for an environment
///   the caller cannot see, and a client that has to draw *something* needs the
///   id to disambiguate two unresolved bars.
/// * The run is [`Self::last_run_id`]. **There is no run-name read in this
///   gear**: `RunsReader` has no bulk name lookup, and a per-item `get_run`
///   would be an N+1 across a gear boundary on a list whose length is the
///   universe size. Named for what it carries rather than a `last_run_name`
///   with a UUID inside it, which is the discipline Task 23 applied to the
///   environment id.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct AnalyticsListItemDto {
    pub test_file: String,
    /// The `TEST_META` title if the file declared one, else the file stem.
    pub test_name: String,
    pub component: Option<String>,
    pub tags: Vec<String>,
    /// The repository half of the plan identity; see this type's header.
    pub repo_id: Uuid,
    /// The `plan.yaml` path within that repository — and the value
    /// `?scope=plan&plan_id=` takes.
    pub plan_path: String,
    pub plan_name: String,
    /// The versions the plan declares, newest first.
    pub versions: Vec<String>,
    /// `PASSED` | `FAILED` | `NOT_RUN` — the **bucketed** status of the latest
    /// row, not the runner's own spelling. A `SKIPPED` test is `NOT_RUN` here.
    pub last_status: String,
    /// See this type's header for why both this and
    /// [`Self::last_environment`] are present.
    pub last_environment_id: Option<Uuid>,
    /// `null` when no row named an environment **or** when qa-environments
    /// resolved none for the id — the two are deliberately indistinguishable,
    /// which `domain::ports::EnvironmentReader::names` records as a visibility rule
    /// rather than an omission.
    pub last_environment: Option<String>,
    /// See this type's header: an id, not a run name.
    pub last_run_id: Option<Uuid>,
    /// `unknown` when the latest run named no build — the collapse
    /// `domain::analytics::universe::collapse_build` applies, and `null` only
    /// when there is no latest row at all.
    pub last_build: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_run_finished_at: Option<OffsetDateTime>,
    /// Counted over every row in the read window, not only the latest — so
    /// `pass_count + fail_count + skipped_count` is [`Self::total_runs`].
    pub pass_count: u32,
    pub fail_count: u32,
    pub skipped_count: u32,
    pub total_runs: u32,
    /// The per-case verdict of the latest run, in the **case**-level vocabulary,
    /// which is wider than the file-level one: it adds `XFAIL` and `XPASS`.
    /// `null` when that run reported no cases.
    pub case_status: Option<String>,
    /// The case-level bug references of the latest run, sorted and
    /// de-duplicated.
    pub case_tickets: Vec<String>,
}

/// [`AnalyticsListItemDto`] with the environment name joined in.
///
/// Not a `From`, because the label is not on the item: it comes from
/// [`AnalyticsOverview::platform_names`], which the service resolved in one
/// cross-gear call over the distinct ids of the whole payload.
fn list_item_dto(item: AnalyticsListItem, names: &HashMap<Uuid, String>) -> AnalyticsListItemDto {
    AnalyticsListItemDto {
        test_file: item.test_file,
        test_name: item.test_name,
        component: item.component,
        tags: item.tags,
        repo_id: item.plan.repo_id,
        plan_path: item.plan.plan_path,
        plan_name: item.plan_name,
        versions: item.versions,
        last_status: item.last_status.to_owned(),
        last_environment_id: item.last_environment_id,
        last_environment: item.last_environment_id.and_then(|id| names.get(&id).cloned()),
        last_run_id: item.last_run_id,
        last_build: item.last_build,
        last_run_finished_at: item.last_run_finished_at,
        pass_count: item.pass_count,
        fail_count: item.fail_count,
        skipped_count: item.skipped_count,
        total_runs: item.total_runs,
        case_status: item.case_status,
        case_tickets: item.case_tickets,
    }
}

/// The universe as three lists, each sorted by display name.
///
/// Every universe entry is in exactly one of the three, so their lengths sum to
/// [`OverviewSummaryDto::total`].
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct AnalyticsListsDto {
    pub passed: Vec<AnalyticsListItemDto>,
    pub failed: Vec<AnalyticsListItemDto>,
    /// Includes every test that has never run **and** every test whose latest
    /// row was `SKIPPED` or an unrecognized status.
    pub not_run: Vec<AnalyticsListItemDto>,
}

/// One test's row of the heatmap.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct HeatmapRowDto {
    pub test_file: String,
    pub test_name: String,
    /// One cell per entry of [`HeatmapDataDto::days`], in the same order:
    /// `PASSED` | `FAILED` | `NOT_RUN`. Always exactly as long as that axis.
    pub values: Vec<String>,
}

/// The heatmap: a day axis and one row per test.
///
/// Rows are in the universe's order — **not** sorted, unlike the lists — and a
/// file listed by two plans is two identical rows.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct HeatmapDataDto {
    /// `YYYY-MM-DD`, oldest first, ending today. Between 1 and 30 entries.
    pub days: Vec<String>,
    pub rows: Vec<HeatmapRowDto>,
}

impl From<HeatmapData> for HeatmapDataDto {
    fn from(heatmap: HeatmapData) -> Self {
        Self {
            days: heatmap.days.into_iter().map(iso_date).collect(),
            rows: heatmap
                .rows
                .into_iter()
                .map(|row| HeatmapRowDto {
                    test_file: row.test_file,
                    test_name: row.test_name,
                    values: row.values.into_iter().map(str::to_owned).collect(),
                })
                .collect(),
        }
    }
}

/// One day of the trend.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct TrendPointDto {
    /// `YYYY-MM-DD`.
    pub day: String,
    pub passed: usize,
    pub failed: usize,
    pub not_run: usize,
}

/// The trend: one point per day, each totalling the universe.
///
/// **`passed + failed + not_run` is the universe size on every point**,
/// including days before any row exists —
/// the denominator is the universe and not the data, which is what makes the
/// chart comparable across days.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct TrendDataDto {
    /// Oldest first, ending today. Between 7 and 365 entries.
    pub points: Vec<TrendPointDto>,
}

impl From<TrendData> for TrendDataDto {
    fn from(trend: TrendData) -> Self {
        Self {
            points: trend
                .points
                .into_iter()
                .map(|point| TrendPointDto {
                    day: iso_date(point.day),
                    passed: point.passed,
                    failed: point.failed,
                    not_run: point.not_run,
                })
                .collect(),
        }
    }
}

/// One build's slice of the "latest run per test" snapshot.
///
/// The newest run in each bucket is [`Self::latest_run_id`], an id rather than a
/// name, for [`AnalyticsListItemDto`]'s reason.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct BuildLastRunDistributionDto {
    /// `unknown` for the tests whose latest run named no build — absent, blank
    /// and the literal string `unknown` all collapse into that one bar.
    pub build: String,
    /// The newest run in this bucket, as an id. See this type's header.
    pub latest_run_id: Option<Uuid>,
    pub passed: usize,
    pub failed: usize,
    /// **Not `passed + failed`**: a test whose latest run against this build was
    /// skipped is counted here and in neither counter.
    pub executed_total: usize,
}

impl From<BuildLastRunDistribution> for BuildLastRunDistributionDto {
    fn from(entry: BuildLastRunDistribution) -> Self {
        Self {
            build: entry.build,
            latest_run_id: entry.latest_run_id,
            passed: entry.passed,
            failed: entry.failed,
            executed_total: entry.executed_total,
        }
    }
}

/// One test the flaky detector picked out.
///
/// The window is the **trend's** day count, not a third one, and the
/// qualification is at least five executions with a pass rate in `[40, 80]`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct FlakyTestDto {
    pub test_file: String,
    pub test_name: String,
    pub component: Option<String>,
    pub tags: Vec<String>,
    /// Percentage to one decimal place, over passes plus failures plus skips.
    pub pass_rate: f64,
    pub executions: u32,
    pub pass_count: u32,
    pub fail_count: u32,
    pub skipped_count: u32,
}

impl From<FlakyTest> for FlakyTestDto {
    fn from(test: FlakyTest) -> Self {
        Self {
            test_file: test.test_file,
            test_name: test.test_name,
            component: test.component,
            tags: test.tags,
            pass_rate: test.pass_rate,
            executions: test.executions,
            pass_count: test.pass_count,
            fail_count: test.fail_count,
            skipped_count: test.skipped_count,
        }
    }
}

/// How many test files declare one Quality Vector.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct QualityVectorCountDto {
    /// The **first** spelling seen for this vector, case-folded for grouping but
    /// rendered verbatim. So `Security` and `security` are one entry here — and
    /// **two rows** on the dashboard's own quality-vector list, which keys on the
    /// display string. The asymmetry is deliberate, and pinned on both sides;
    /// see `domain::analytics::aggregates`.
    pub vector: String,
    /// Distinct **files**, so a file declaring two vectors is counted in both
    /// and the array's totals are not a file count.
    pub tests: usize,
}

/// The Quality Vector breakdown of the whole universe.
///
/// **Never narrowed by `group_by`/`group_value`** — the vectors are a property
/// of the suite rather than of a selection, which is easy to get wrong when
/// assembling the pipeline.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct QualityVectorSummaryDto {
    /// Most files first, then alphabetically.
    pub items: Vec<QualityVectorCountDto>,
    /// Files whose `TEST_META` declared no vector at all.
    pub unclassified_tests: usize,
    /// Distinct files in the **unfiltered** universe.
    pub total_tests: usize,
}

impl From<QualityVectorSummary> for QualityVectorSummaryDto {
    fn from(summary: QualityVectorSummary) -> Self {
        Self {
            items: summary
                .items
                .into_iter()
                .map(|item| QualityVectorCountDto {
                    vector: item.vector,
                    tests: item.tests,
                })
                .collect(),
            unclassified_tests: summary.unclassified_tests,
            total_tests: summary.total_tests,
        }
    }
}

/// One bar of a component or tag breakdown.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct GroupSummaryDto {
    /// The component, or the tag. `unknown` for a file with no component and
    /// `untagged` for one with no tags — **labels, not values**, so a
    /// `?group_value=unknown` drill-down matches nothing.
    pub value: String,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub not_run: usize,
}

impl From<GroupSummary> for GroupSummaryDto {
    fn from(group: GroupSummary) -> Self {
        Self {
            value: group.value,
            total: group.total,
            passed: group.passed,
            failed: group.failed,
            not_run: group.not_run,
        }
    }
}

/// One bar of the environment breakdown.
///
/// # Why this is not a [`GroupSummaryDto`]
///
/// The other two breakdowns bucket by a string the row itself carries, so their
/// bars are `GroupSummary { value: String, .. }`. An execution row carries the
/// environment as a `Uuid`, and the name comes from qa-environments — which can
/// decline to resolve it, for an environment deleted since the run executed or
/// one in another tenant, two cases `domain::ports::EnvironmentReader::names`
/// deliberately makes indistinguishable.
///
/// So the bar carries **both**: [`Self::environment_id`], which always
/// identifies the bucket, and [`Self::environment`], which is the label when
/// there is one. Collapsing them into a `value: String` would force a choice
/// between dropping an unresolvable bar — silent data loss on a chart whose
/// job is comparison — and rendering a UUID into a field a client will draw
/// as a name, which is the exact outcome Task 23 typed `PlatformGroupSummary`
/// around a `Uuid` to prevent.
///
/// Renamed from `PlatformGroupSummaryDto`, with its `environment_id`/`platform`
/// fields, to `EnvironmentGroupSummaryDto` with `environment_id`/`environment`
/// (Task 25) — see [`TestResultDto::environment_id`]'s doc for why.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct EnvironmentGroupSummaryDto {
    pub environment_id: Uuid,
    /// `null` when qa-environments resolved no name for
    /// [`Self::environment_id`].
    pub environment: Option<String>,
    /// The size of the **whole** universe, not of the environment's own tests:
    /// each bar re-buckets every test by its latest row *on that environment*,
    /// so a test that never ran there counts as `not_run` in that bar. The bars therefore
    /// do not partition anything and do not sum to
    /// [`OverviewSummaryDto::total`].
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub not_run: usize,
}

/// The three group breakdowns.
///
/// **Computed over the unfiltered universe and all rows**, so selecting one
/// group narrows the rest of
/// the payload and leaves this chart whole — which is what makes it a chart
/// rather than a single bar.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct GroupedSummariesDto {
    /// Alphabetical. Every universe file is in exactly one bucket, so these
    /// totals sum to the **unfiltered** universe size.
    pub component: Vec<GroupSummaryDto>,
    /// Alphabetical. A file with several tags is in several buckets, so these do
    /// **not** sum to anything meaningful.
    pub tag: Vec<GroupSummaryDto>,
    /// **Ordered by resolved name**, with the bars qa-environments could not
    /// name last, ordered by id.
    ///
    /// Task 23's fold orders by id because that is all it has, so the sort
    /// happens here, where the names exist. **A rendered order changes when an
    /// environment is
    /// renamed**, which is the correct direction and a change to expect rather than a
    /// regression to hunt.
    ///
    /// Renamed from `platform` (Task 25), alongside its element type
    /// (`PlatformGroupSummaryDto` → [`EnvironmentGroupSummaryDto`]).
    pub environment: Vec<EnvironmentGroupSummaryDto>,
}

/// Everything `GET /qa/v1/analytics/overview` answers with.
///
/// **Eight computed sections** plus the query echoed back.
///
/// # There is no `product_key` field
///
/// Nothing in this subsystem carries a product key on a run: qa-catalog owns
/// products, and a run reaches one through its target. Absent rather than echoed
/// back as the `product_id`, which would be a different value under the same
/// name.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct AnalyticsOverviewDto {
    /// The request's `product_id`, trimmed.
    pub product_id: String,
    /// The request's `version`, trimmed.
    pub version: String,
    /// `all` or `plan`, **normalized** — see [`AnalyticsScopeDto`].
    pub scope: AnalyticsScopeDto,
    /// Normalized: trimmed, and `null` when blank. Carried through an `all`
    /// scope, where it is never read.
    pub plan_id: Option<String>,
    /// Normalized: `null` means every branch.
    pub branch: Option<String>,
    /// `none` | `component` | `tag` | `environment`.
    pub group_by: String,
    /// Normalized: trimmed, and `null` when blank.
    pub group_value: Option<String>,
    pub summary: OverviewSummaryDto,
    pub lists: AnalyticsListsDto,
    pub heatmap: HeatmapDataDto,
    pub trend: TrendDataDto,
    /// Ordered newest build first, with the `unknown` bucket last.
    pub build_distribution: Vec<BuildLastRunDistributionDto>,
    /// Flakiest first. An empty array is the ordinary answer on a healthy suite.
    pub flaky: Vec<FlakyTestDto>,
    pub quality_vectors: QualityVectorSummaryDto,
    pub grouped: GroupedSummariesDto,
}

impl From<AnalyticsOverview> for AnalyticsOverviewDto {
    fn from(overview: AnalyticsOverview) -> Self {
        let names = overview.platform_names;
        Self {
            product_id: overview.product_id,
            version: overview.version,
            scope: overview.scope.into(),
            plan_id: overview.plan_id,
            branch: overview.branch,
            group_by: group_to_str(overview.group_by).to_owned(),
            group_value: overview.group_value,
            summary: OverviewSummaryDto::from(overview.summary),
            lists: AnalyticsListsDto {
                passed: overview
                    .lists
                    .passed
                    .into_iter()
                    .map(|item| list_item_dto(item, &names))
                    .collect(),
                failed: overview
                    .lists
                    .failed
                    .into_iter()
                    .map(|item| list_item_dto(item, &names))
                    .collect(),
                not_run: overview
                    .lists
                    .not_run
                    .into_iter()
                    .map(|item| list_item_dto(item, &names))
                    .collect(),
            },
            heatmap: HeatmapDataDto::from(overview.heatmap),
            trend: TrendDataDto::from(overview.trend),
            build_distribution: overview
                .build_distribution
                .into_iter()
                .map(BuildLastRunDistributionDto::from)
                .collect(),
            flaky: overview.flaky.into_iter().map(FlakyTestDto::from).collect(),
            quality_vectors: QualityVectorSummaryDto::from(overview.quality_vectors),
            grouped: GroupedSummariesDto {
                component: overview
                    .grouped
                    .component
                    .into_iter()
                    .map(GroupSummaryDto::from)
                    .collect(),
                tag: overview
                    .grouped
                    .tag
                    .into_iter()
                    .map(GroupSummaryDto::from)
                    .collect(),
                environment: platform_bars(overview.grouped.platform, &names),
            },
        }
    }
}

/// The platform bars with their names joined in and re-sorted on them.
///
/// See [`GroupedSummariesDto::environment`] for why the order moves and
/// [`EnvironmentGroupSummaryDto`] for why an unnamed bar is kept rather than
/// dropped. `sort_by` over `(Option<&str>, Uuid)` puts `None` **first** in Rust's
/// ordering, which is the wrong end, so the key is built explicitly with a
/// resolved/unresolved discriminant ahead of the name.
fn platform_bars(
    bars: Vec<PlatformGroupSummary>,
    names: &HashMap<Uuid, String>,
) -> Vec<EnvironmentGroupSummaryDto> {
    let mut bars: Vec<EnvironmentGroupSummaryDto> = bars
        .into_iter()
        .map(|bar| EnvironmentGroupSummaryDto {
            environment_id: bar.environment_id,
            environment: names.get(&bar.environment_id).cloned(),
            total: bar.total,
            passed: bar.passed,
            failed: bar.failed,
            not_run: bar.not_run,
        })
        .collect();

    bars.sort_by(|left, right| {
        let key = |bar: &EnvironmentGroupSummaryDto| {
            (
                u8::from(bar.environment.is_none()),
                bar.environment.clone().unwrap_or_default(),
                bar.environment_id,
            )
        };
        key(left).cmp(&key(right))
    });
    bars
}

/// One test of one build, as the drill-down lists it.
///
/// The run is [`Self::run_id`], an id rather than a name, for
/// [`AnalyticsListItemDto`]'s reason.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct BuildTestDetailDto {
    pub test_file: String,
    pub test_name: String,
    /// `PASSED` | `FAILED` | `SKIPPED`, **or the runner's own spelling** for
    /// anything else — this is the one place the overview does not bucketize
    /// away an unusual status, which is why the drill-down can show an `XFAIL`
    /// where the list beside it says `NOT_RUN`.
    pub status: String,
    /// The run this outcome came from, as an id; see this type's header.
    pub run_id: Uuid,
    /// `run_finished_at ?? run_created_at` — the run's instant, never the row's.
    #[serde(with = "time::serde::rfc3339")]
    pub run_finished_at: OffsetDateTime,
    pub component: Option<String>,
    pub tags: Vec<String>,
}

impl From<BuildTestDetail> for BuildTestDetailDto {
    fn from(detail: BuildTestDetail) -> Self {
        Self {
            test_file: detail.test_file,
            test_name: detail.test_name,
            status: detail.status,
            run_id: detail.run_id,
            run_finished_at: detail.run_finished_at,
            component: detail.component,
            tags: detail.tags,
        }
    }
}

/// A calendar day as `YYYY-MM-DD`.
///
/// The chart axes are days and not instants, so they are rendered as dates
/// rather than as RFC 3339. `time::Date`'s own `Display` is ISO 8601 for every
/// year this system can produce, so no format description is needed.
fn iso_date(day: Date) -> String {
    day.to_string()
}

/// Which executions an analytics overview is about: the whole universe, or one
/// plan. **Normalized** — a request that shouted its scope gets it back in
/// lower case.
//
// Mirrors `domain::analytics::query::Scope`, the same way `SavedViewScopeDto`
// mirrors `qa_insights_sdk::SavedViewScope`; see that type for why a mirror is
// needed at all (`#[api_dto]` needs serde and utoipa, and neither the SDK nor
// the domain layer carries them).
//
// # It replaces `scope_to_str`
//
// This was `const fn scope_to_str(Scope) -> &'static str`, whose output landed
// in an `AnalyticsOverviewDto::scope: String`. The two spellings are unchanged and
// this type's `#[serde(rename_all = "snake_case")]` is now their sole encoder -
// one encoder, at the boundary, rather than a rendering function beside a
// `String` field. `the_echoed_scope_and_grouping_use_legacys_spelling` asserts
// the rendered JSON, and
// `routes::tests::the_published_schema_declares_closed_enums_for_both_scopes`
// asserts the published schema.
//
// # Why the *request* side is still a `String`, unlike this
//
// `AnalyticsOverviewQuery`, `AnalyticsBuildTestsQuery` and
// `AnalyticsExportQuery` keep `scope: String`: `domain::analytics::query::
// parse_scope` accepts the value trimmed and case-insensitively and answers
// anything else with a 400 naming the field, so a serde enum there would refuse
// `ALL`, which is accepted today - a wire change. No such argument can apply to
// an encoder, which is why this response field is typed and those are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(request, response)]
pub enum AnalyticsScopeDto {
    All,
    Plan,
}

impl From<Scope> for AnalyticsScopeDto {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::All => Self::All,
            Scope::Plan => Self::Plan,
        }
    }
}

impl From<AnalyticsScopeDto> for Scope {
    /// The direction that closes the mirror: a variant added *here* and not to
    /// the domain enum is a compile error too, so the two sets stay in
    /// bijection.
    fn from(scope: AnalyticsScopeDto) -> Self {
        match scope {
            AnalyticsScopeDto::All => Self::All,
            AnalyticsScopeDto::Plan => Self::Plan,
        }
    }
}

/// `GroupBy` rendered for the wire. The fourth arm renders `"environment"`,
/// matching the domain vocabulary rather than the column name. The other three
/// arms keep the stored spelling, for [`AnalyticsScopeDto`]'s reason.
const fn group_to_str(group: GroupBy) -> &'static str {
    match group {
        GroupBy::None => "none",
        GroupBy::Component => "component",
        GroupBy::Tag => "tag",
        GroupBy::Environment => "environment",
    }
}

/// The query string all three plan drill-downs take.
///
/// Fix round on Task 27: `plan_id` was originally a path segment, and a real
/// `plan_path` routinely contains `/` (this crate's own fixtures,
/// `plans/nightly/plan.yaml`), which one path segment cannot carry unencoded —
/// the route 404'd for any real plan, and no test crossed the HTTP boundary to
/// catch it. Moved to a query parameter because that is what this gear
/// already ships: [`AnalyticsListItemDto::plan_path`]'s doc names `plan_path`
/// as the value `?scope=plan&plan_id=` on the overview already takes, so the
/// path segment was the outlier. No `#[toolkit_macros::api_dto(request)]`,
/// [`AnalyticsOverviewQuery`]'s reason: a query string, not a body.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnalyticsPlanQuery {
    /// Required, non-blank (trimmed and refused otherwise — Phase B fix wave,
    /// Finding 9; see
    /// `domain::service::analytics::AnalyticsService::plan_rows`'s doc). The
    /// plan's path within its repository, matched across every repository
    /// the caller's access scope admits — see
    /// [`crate::domain::repos::ResultsRepository::list_for_plan`]'s header.
    pub plan_id: String,
}

/// One test's aggregated analytics, as
/// `GET /qa/v1/analytics/plan/tests?plan_id=` renders it.
///
/// `last_environment_id` and `last_environment` (renamed from
/// `last_environment_id`/`last_platform` at ruling G-3) both ride along for
/// [`AnalyticsListItemDto`]'s reason: the name is `null` for an environment
/// the caller cannot see or that no row named, and the two are indistinguishable
/// on the wire, exactly as `EnvironmentReader::names`' header records.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PlanTestAnalyticsDto {
    pub test_name: String,
    /// The most recent execution's raw status.
    pub last_status: String,
    /// See this type's header for why this rides beside
    /// [`Self::last_environment`].
    pub last_environment_id: Option<Uuid>,
    /// `null` when no row named an environment **or** when qa-environments
    /// resolved none for the id — see this type's header.
    pub last_environment: Option<String>,
    /// `qa_test_results.product_version`, not the overview's `app_build` —
    /// [`crate::domain::repos::PlanExecRow`]'s header explains the column.
    pub last_version: Option<String>,
    /// The most recent execution's run, as an id rather than a name.
    pub last_run_id: Uuid,
    /// The most recent execution's JIRA reference.
    pub jira_key: Option<String>,
    /// Every execution inside the read window, unconditional.
    pub total_runs: u64,
    /// Executions whose status is the **literal** string `PASSED` — narrower
    /// than the dashboard's `PASSED_STATUSES`. See
    /// [`crate::domain::repos::PlanExecRow::status`].
    pub pass_count: u64,
    /// Executions whose status is the literal string `FAILED`. See
    /// [`Self::pass_count`].
    pub fail_count: u64,
}

/// [`PlanTestAnalyticsDto`] with the environment name joined in — the same
/// split [`list_item_dto`] makes, over [`PlanTests::platform_names`].
fn plan_test_analytics_dto(
    item: PlanTestAnalytics,
    names: &HashMap<Uuid, String>,
) -> PlanTestAnalyticsDto {
    PlanTestAnalyticsDto {
        test_name: item.test_name,
        last_status: item.last_status,
        last_environment_id: item.last_environment_id,
        last_environment: item.last_environment_id.and_then(|id| names.get(&id).cloned()),
        last_version: item.last_version,
        last_run_id: item.last_run_id,
        jira_key: item.jira_key,
        total_runs: item.total_runs,
        pass_count: item.pass_count,
        fail_count: item.fail_count,
    }
}

/// The whole answer of `GET /qa/v1/analytics/plan/tests?plan_id=` —
/// [`AnalyticsService::plan_tests`](crate::domain::service::analytics::AnalyticsService::plan_tests)'
/// `Vec<PlanTestAnalytics>` plus the resolved environment names, joined into
/// one array. Not a wrapper struct on the wire: the response is a bare JSON
/// array, and the environment names have no field of their own in it.
#[must_use]
pub fn plan_test_analytics_list_dto(response: PlanTests) -> Vec<PlanTestAnalyticsDto> {
    response
        .items
        .into_iter()
        .map(|item| plan_test_analytics_dto(item, &response.platform_names))
        .collect()
}

/// One build's distribution, as
/// `GET /qa/v1/analytics/plan/builds?plan_id=` renders it.
///
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PlanBuildDistributionDto {
    /// `unknown` when no execution named a version, sorted after every named
    /// one: an absent version coalesces to the literal `unknown`.
    pub build: String,
    /// Every execution of the group, unconditional; can exceed the sum of the
    /// three counters below.
    pub total: u64,
    /// Executions whose status is the literal string `PASSED`.
    pub passed: u64,
    /// Executions whose status is the literal string `FAILED`.
    pub failed: u64,
    /// Executions whose status is the literal string `SKIPPED`.
    pub skipped: u64,
}

impl From<PlanBuildDistribution> for PlanBuildDistributionDto {
    fn from(dist: PlanBuildDistribution) -> Self {
        Self {
            build: dist.build,
            total: dist.total,
            passed: dist.passed,
            failed: dist.failed,
            skipped: dist.skipped,
        }
    }
}

/// One run's outcome for one test, inside [`PlanTestHistoryDto::results`].
///
/// The run is [`Self::run_id`], an id rather than a name, for
/// [`AnalyticsListItemDto`]'s reason.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PlanTestHistoryEntryDto {
    /// `null` when the execution named no version — **not** coalesced to
    /// `"unknown"`, unlike [`PlanBuildDistributionDto::build`].
    pub build: Option<String>,
    pub status: String,
    /// The execution's run, as an id rather than a name. See this type's
    /// header.
    pub run_id: Uuid,
}

impl From<PlanTestHistoryEntry> for PlanTestHistoryEntryDto {
    fn from(entry: PlanTestHistoryEntry) -> Self {
        Self {
            build: entry.build,
            status: entry.status,
            run_id: entry.run_id,
        }
    }
}

/// One test's history, as `GET /qa/v1/analytics/plan/test-history?plan_id=`
/// renders it.
///
/// See [`PlanTestHistory`]'s header for why the outer array's order is
/// specified here rather than left to a map's iteration order.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PlanTestHistoryDto {
    pub test_name: String,
    /// Newest first. See this type's header.
    pub results: Vec<PlanTestHistoryEntryDto>,
}

impl From<PlanTestHistory> for PlanTestHistoryDto {
    fn from(history: PlanTestHistory) -> Self {
        Self {
            test_name: history.test_name,
            results: history.results.into_iter().map(Into::into).collect(),
        }
    }
}

// ==================== Saved views (Task 28) ====================

/// `GET /qa/v1/analytics/views`. The plan identity is
/// [`Self::repo_id`] + [`Self::plan_path`], for the reason
/// `domain::service::saved_views`'s header gives.
///
/// No `#[toolkit_macros::api_dto(request)]`, [`AnalyticsOverviewQuery`]'s
/// reason: a query string, not a body.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SavedViewsListQuery {
    /// Required. `all` or `plan`, case-insensitively.
    pub scope: String,
    /// Required together with [`Self::plan_path`] when `scope=plan`.
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
}

/// The body of a create or a replace, with the same plan-identity pair.
///
/// `query_json` is a JSON object on the wire in both directions, matching
/// a `serde_json::Value` — not the doubly-encoded string-holding-a-string
/// shape a bare `String` field would advertise here.
/// [`qa_insights_sdk::NewSavedView::query_json`]'s doc records why the *domain*
/// type is a `String` instead: it is `serde`-free contract-layer purity, not a
/// claim about the wire shape.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct NewSavedViewReq {
    pub scope: String,
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    pub name: String,
    pub query_json: serde_json::Value,
}

impl From<NewSavedViewReq> for SavedViewInput {
    fn from(req: NewSavedViewReq) -> Self {
        // `Value`'s `Display` is the compact serializer and cannot fail — a
        // `Value` can never hold a non-finite float or a non-string object
        // key, which are `serde_json::to_string`'s only failure modes over a
        // `Value` — so this is `.to_string()` rather than a `Result` this
        // `From` would have nowhere to put.
        Self {
            scope: req.scope,
            repo_id: req.repo_id,
            plan_path: req.plan_path,
            name: req.name,
            query_json: req.query_json.to_string(),
        }
    }
}

/// What a saved view is scoped to: the whole universe, or one plan.
//
// Mirrors `qa_insights_sdk::SavedViewScope`. Everything below is why rather
// than what a caller needs, and is kept off the doc comment so it stays out of
// the published schema description.
//
// # Why this is a mirror and not the SDK type
//
// `#[api_dto]` adds `serde` and `utoipa::ToSchema`, and every type nested in a
// DTO needs both. `qa-insights-sdk` carries neither by the repo-wide
// contract-purity rule its own crate docs state (an SDK model has no wire form
// and no `OpenAPI` dependency), so the wire vocabulary lives here, at the
// boundary - the same shape `qa-catalog`'s `FieldKindDto` and `qa-runs`'
// `RunStateDto` take for the same reason.
//
// # What it buys, over the `String` it replaces
//
// `SavedViewDto::scope` was a `String` filled from `SavedViewScope::as_str`
// while the closed enum sat beside it (review finding #35). Now the published
// `OpenAPI` schema is a two-value `enum`, so the generated TypeScript narrows
// to `"all" | "plan"` and an unknown value is a decode error rather than
// something a consumer must defend against. `From` and its reverse both match
// exhaustively with no wildcard arm, so neither side can gain a variant
// without a compile error.
//
// # Why only the *response* side is typed
//
// `NewSavedViewReq::scope`, `SavedViewsListQuery::scope` and the three
// analytics query strings (`AnalyticsOverviewQuery`,
// `AnalyticsBuildTestsQuery`, `AnalyticsExportQuery`) stay `String`
// **deliberately**. They are inbound, and their contract is not this closed
// set: `domain::service::saved_views::parse_scope` and
// `domain::analytics::query::parse_scope` accept the value *trimmed and
// case-insensitively*, and answer anything else with a 400 reading
// `"scope must be 'all' or 'plan'"`. A `serde` enum there would refuse `ALL`,
// which is accepted today, and would answer with the deserializer's own message
// and shape instead - a wire change, which this task is explicitly not. The
// unknown-value rejection those fields need already exists, one layer in, and
// it is the better one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(request, response)]
pub enum SavedViewScopeDto {
    All,
    Plan,
}

impl From<SavedViewScope> for SavedViewScopeDto {
    fn from(scope: SavedViewScope) -> Self {
        match scope {
            SavedViewScope::All => Self::All,
            SavedViewScope::Plan => Self::Plan,
        }
    }
}

impl From<SavedViewScopeDto> for SavedViewScope {
    /// The direction that closes the mirror: a variant added *here* and not to
    /// the SDK is a compile error too, so the two sets stay in bijection.
    fn from(scope: SavedViewScopeDto) -> Self {
        match scope {
            SavedViewScopeDto::All => Self::All,
            SavedViewScopeDto::Plan => Self::Plan,
        }
    }
}

/// A stored saved view, with the same plan-identity pair and `owner_id` still a
/// caller-visible field: it is the caller's own id
/// in every case this gear can construct (the repository narrows every read and
/// write to the caller's [`toolkit_security::AccessScope::ensure_owner`]-ed
/// scope), so echoing it back is inert rather than a cross-owner leak.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct SavedViewDto {
    pub id: Uuid,
    pub owner_id: Uuid,
    /// `all` or `plan` — see [`SavedViewScopeDto`].
    pub scope: SavedViewScopeDto,
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    pub name: String,
    pub query_json: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl TryFrom<SavedView> for SavedViewDto {
    type Error = DomainError;

    /// # Errors
    ///
    /// [`DomainError::CorruptState`] if the stored `query_json` text is not
    /// JSON. Unreachable in practice — every writer goes through
    /// `infra::storage::mapper::query_json_to_column`, which validates on every
    /// write — but a `TryFrom` rather than an `.expect` on a stored column is
    /// this crate's own fail-closed convention
    /// (`domain::error::DomainError::CorruptState`'s doc), and a panic in a
    /// request handler is a worse failure mode than a 500 that names the
    /// column.
    fn try_from(view: SavedView) -> Result<Self, DomainError> {
        let query_json =
            serde_json::from_str(&view.query_json).map_err(|_| DomainError::CorruptState {
                what: "saved_view.query_json",
                id: view.id,
                value: view.query_json.clone(),
            })?;
        Ok(Self {
            id: view.id,
            owner_id: view.owner_id,
            scope: view.scope.into(),
            repo_id: view.repo_id,
            plan_path: view.plan_path,
            name: view.name,
            query_json,
            created_at: view.created_at,
            updated_at: view.updated_at,
        })
    }
}

// ===========================================================================
// The collect trigger and report (Task 30)
// ===========================================================================

/// The query string `POST /qa/v1/analytics/collect` takes.
///
/// No `#[toolkit_macros::api_dto(request)]`, [`AnalyticsOverviewQuery`]'s
/// reason: a query string, not a body.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CollectTriggerQuery {
    /// Branch to collect. Blank and absent are the same — both fall back to
    /// this deployment's `default_collect_branch`.
    pub branch: Option<String>,
}

/// What `POST /qa/v1/analytics/collect` answers with: the launched count and
/// the branch, typed rather than an untyped `serde_json::Value` so the `OpenAPI`
/// schema states the two fields.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct CollectTriggerOutcomeDto {
    /// How many launch calls qa-runs **accepted**, not how many repositories
    /// are now confirmed to be collecting — `domain::service::collect`'s
    /// header ("`launched` counts launch calls...") states the reason a
    /// repository lacking the requested branch can still be counted here.
    pub launched: usize,
    /// The branch actually collected — the caller's own, or the default it
    /// fell back to. Never blank.
    pub branch: String,
}

/// The query string `POST /qa/v1/collect/{repo_id}` takes, alongside the
/// `repo_id` path parameter.
///
/// **The branch is a query parameter, not a path segment.**
/// `domain::service::collect`'s header ("The branch-in-path hazard") is the full
/// argument for moving it
/// here instead: a real branch routinely contains `/`, which a single path
/// segment cannot carry, and this gear controls both the URL this type
/// decodes and the code in [`crate::domain::service::collect::CollectService::collect_url`]
/// that encodes it, so nothing outside this gear ever has to compose one by
/// hand.
///
/// `tenant_id` and `sig` exist because the runner posts this report from
/// outside the control plane: the tenant it writes under has to travel on the
/// URL itself, and the signature is what makes that claim trustworthy. Fix
/// round 1's Critical 1
/// found that an embedded `tenant_id` with nothing backing it is a
/// cross-tenant write — see `domain::service::collect`'s header, "Fix round
/// 1, Critical 1", for why `sig` (an HMAC-SHA256 tag over `(repo_id, branch,
/// tenant_id)`) is what actually makes `tenant_id` trustworthy, not merely
/// that this gear chose the value.
///
/// # This struct's three fields must agree with
/// # [`crate::domain::service::collect::CollectService::collect_url`]'s
/// # private `Query` type
///
/// The two are independent type definitions (domain code must not import
/// `api::rest::dto`) encoding and decoding the identical wire shape — see
/// that method's own doc. A field renamed on one side and not the other
/// silently drops the value across the query string, and for `sig`
/// specifically, a mismatch would make every report fail closed rather than
/// leak a value — the fail-closed default this fix round chose deliberately.
///
/// **Pinned by [`tests::collect_report_query_decodes_collect_urls_real_encoder_output`]
/// below (Phase B fix wave, Finding 6), not only by
/// `collect_tests::the_collect_url_round_trips_a_slash_bearing_branch_through_its_query_string`.**
/// That domain-side test drives the real encoder but decodes into its own
/// third, local struct — legitimate given the layering rule above, but blind
/// to a change made to *this* type alone. The test below decodes
/// [`crate::domain::service::collect::encode_collect_report_query`]'s output
/// — the same free function [`CollectService::collect_url`] calls, factored
/// out for exactly this reason — into this real `CollectReportQuery`, so a
/// rename on either side that the other does not match is caught here even
/// when it is not caught anywhere else. See that function's own doc for the
/// verified (not assumed) account of which existing tests catch which
/// single-sided rename, and why neither is a substitute for this one.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CollectReportQuery {
    /// Required, non-blank. The branch the runner collected.
    pub branch: String,
    /// Required. The tenant this report claims — this gear's own choice,
    /// embedded when the collect job was launched, authenticated by `sig`
    /// rather than trusted on its own.
    pub tenant_id: Uuid,
    /// Required. HMAC-SHA256 over `(repo_id, branch, tenant_id)`, hex-encoded
    /// — this gear's own choice, verified by
    /// `CollectService::verify_signature` before the tenant claim above is
    /// used for anything.
    pub sig: String,
}

/// The body the runner posts with one file's exact case count —
/// `CollectCountPayload`.
///
/// `case_count` is `i64` on the wire and **not**
/// [`qa_insights_sdk::CollectCount::case_count`]'s `u32` — see
/// `domain::service::collect`'s header, "The `case_count` clamp needs an
/// `i64` wire field": a `u32` field here would turn a clamp-to-zero
/// into a deserialization 400, which is a different behaviour a client would
/// observe as a rejected report rather than a recorded zero.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CollectCountReq {
    pub test_file: String,
    pub case_count: i64,
}

// ==================== JIRA settings (Task 32) ====================

/// The tenant's JIRA settings on the wire — `GET/PUT /qa/v1/settings/jira`.
///
/// **One field is named for a reference rather than a value, and that naming is
/// the security boundary**: an `api_token` field would carry the token and its
/// `GET` substitutes `"********"` for it. [`Self::api_token_credstore_ref`]
/// carries a credential-store *reference*, so there is nothing to mask and
/// nothing to leak — `the_jira_settings_response_has_no_api_token_field` is
/// what keeps a future edit from reintroducing a raw-token field.
///
/// One DTO for both directions, unlike the saved-view pair: the six fields are
/// the same six either way, and the one asymmetry — an empty
/// `api_token_credstore_ref` means "keep the stored reference" on a `PUT` and
/// means "none is stored" on a `GET` — is a *rule*, not a shape, so a second
/// type would differ only in its doc. `domain::service::jira::JiraConfigInput`
/// is where that asymmetry is named in the type system, one layer in.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct JiraSettingsDto {
    /// Base URL of the JIRA instance, e.g. `https://acme.atlassian.net`.
    pub url: String,
    pub project_key: String,
    /// The account the credential belongs to.
    pub email: String,
    /// A credential-store reference. **Never a token.** Empty on a `PUT` keeps
    /// whatever is stored.
    pub api_token_credstore_ref: String,
    /// `null` uses the project's default issue type.
    pub issue_type: Option<String>,
    pub enabled: bool,
}

impl From<JiraConfig> for JiraSettingsDto {
    fn from(config: JiraConfig) -> Self {
        Self {
            url: config.url,
            project_key: config.project_key,
            email: config.email,
            api_token_credstore_ref: config.api_token_credstore_ref,
            issue_type: config.issue_type,
            enabled: config.enabled,
        }
    }
}

impl From<JiraSettingsDto> for JiraConfigInput {
    fn from(dto: JiraSettingsDto) -> Self {
        Self {
            url: dto.url,
            project_key: dto.project_key,
            email: dto.email,
            api_token_credstore_ref: dto.api_token_credstore_ref,
            issue_type: dto.issue_type,
            enabled: dto.enabled,
        }
    }
}

// ==================== JIRA poller settings (Task 35) ====================

/// The tenant's poller cadence and auto-rerun switch on the wire —
/// `GET/PUT /qa/v1/settings/jira-poller`.
///
/// Two fields, neither renamed: unlike [`JiraSettingsDto`], neither is a
/// secret, so there is no masking asymmetry between the two directions and one
/// `impl From` pair covers both.
#[derive(Debug, Clone, Copy)]
#[toolkit_macros::api_dto(request, response)]
pub struct JiraPollerConfigDto {
    /// Seconds between polls. [`crate::domain::service::jira::JiraService::poller_config`]
    /// clamps this to at least one on read; this DTO carries whatever that
    /// method returns or whatever the caller posts, unclamped itself.
    pub poll_interval_seconds: u64,
    /// Whether a bug resolving in JIRA, plus a new build, triggers a rerun.
    /// Resolution itself is recorded either way — turning this off does not
    /// stop bugs closing.
    pub auto_rerun_on_resolve: bool,
}

impl From<JiraPollerConfig> for JiraPollerConfigDto {
    fn from(config: JiraPollerConfig) -> Self {
        Self {
            poll_interval_seconds: config.poll_interval_seconds,
            auto_rerun_on_resolve: config.auto_rerun_on_resolve,
        }
    }
}

impl From<JiraPollerConfigDto> for JiraPollerConfig {
    fn from(dto: JiraPollerConfigDto) -> Self {
        Self {
            poll_interval_seconds: dto.poll_interval_seconds,
            auto_rerun_on_resolve: dto.auto_rerun_on_resolve,
        }
    }
}

// ==================== JIRA bug registry (Task 33) ====================

/// One row of `GET /qa/v1/jira/open-bugs`. The plan is
/// [`Self::repo_id`]/[`Self::plan_path`] and the environment is
/// [`Self::environment_id`], as `qa_insights_sdk::JiraBug`'s own header
/// states.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct JiraBugDto {
    pub id: Uuid,
    pub jira_key: String,
    pub test_name: String,
    pub repo_id: Uuid,
    pub plan_path: String,
    pub app_version: Option<String>,
    /// Renamed from `environment_id` (Task 25) — see
    /// [`TestResultDto::environment_id`]'s doc for why.
    pub environment_id: Option<Uuid>,
    /// Free JIRA workflow text — `"Open"` unless [`Self::resolved_at`] is set,
    /// in which case it is whatever the poller last wrote. See
    /// `qa_insights_sdk::JiraBug::status`'s own doc.
    pub status: String,
    pub summary: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
}

impl From<JiraBug> for JiraBugDto {
    fn from(bug: JiraBug) -> Self {
        Self {
            id: bug.id,
            jira_key: bug.jira_key,
            test_name: bug.test_name,
            repo_id: bug.repo_id,
            plan_path: bug.plan_path,
            app_version: bug.app_version,
            environment_id: bug.environment_id,
            status: bug.status,
            summary: bug.summary,
            created_at: bug.created_at,
            resolved_at: bug.resolved_at,
        }
    }
}

/// `GET /qa/v1/jira/open-bugs`'s query string.
///
/// [`SavedViewsListQuery`]'s shape without the `scope` field: there is no
/// scope concept here, only the optional pair — controller ruling R85. No
/// `#[toolkit_macros::api_dto(request)]`, [`SavedViewsListQuery`]'s reason: a
/// query string, not a body.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct OpenBugsQuery {
    /// Required together with [`Self::plan_path`]. Absent with `plan_path`
    /// also absent lists every open bug in the tenant.
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
}

/// `POST /qa/v1/jira/bugs`'s body. `test_name` is optional, and the run is
/// addressed by `run_id` rather than by a name, because this gear keys a run by
/// id and never by an execution backend's
/// workflow name.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct FileJiraBugsReq {
    pub run_id: Uuid,
    /// Absent files against every `FAILED` test of the run; present narrows to
    /// one.
    pub test_name: Option<String>,
}

/// One entry of `POST /qa/v1/jira/bugs`'s response. `created`
/// is `false` for **both** dedupe paths (a local hit, a
/// JIRA-side search hit) and `true` only for an issue this call actually
/// posted. [`crate::domain::ports::jira_client::IssueRef`]'s own doc carries
/// the full argument.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct JiraBugFilingDto {
    pub jira_key: String,
    pub created: bool,
}

impl From<crate::domain::ports::jira_client::IssueRef> for JiraBugFilingDto {
    fn from(issue: crate::domain::ports::jira_client::IssueRef) -> Self {
        Self {
            jira_key: issue.jira_key,
            created: issue.created,
        }
    }
}

// ==================== Notification settings (Task 38) ====================

/// One status's Slack Block Kit sections, on the wire. `enabled`
/// is a routing concern already spent by
/// `domain::notify::routing::route` and reaches the wire anyway because a
/// tenant edits it on the same settings screen as the five sections.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct ScheduledRunSlackTemplateDto {
    pub enabled: bool,
    pub status_icon: Option<String>,
    pub header: Option<String>,
    pub summary: Option<String>,
    pub results: Option<String>,
    pub body: Option<String>,
    pub footer: Option<String>,
}

impl From<ScheduledRunSlackTemplate> for ScheduledRunSlackTemplateDto {
    fn from(template: ScheduledRunSlackTemplate) -> Self {
        Self {
            enabled: template.enabled,
            status_icon: template.status_icon,
            header: template.header,
            summary: template.summary,
            results: template.results,
            body: template.body,
            footer: template.footer,
        }
    }
}

impl From<ScheduledRunSlackTemplateDto> for ScheduledRunSlackTemplate {
    fn from(dto: ScheduledRunSlackTemplateDto) -> Self {
        Self {
            enabled: dto.enabled,
            status_icon: dto.status_icon,
            header: dto.header,
            summary: dto.summary,
            results: dto.results,
            body: dto.body,
            footer: dto.footer,
        }
    }
}

/// The six status templates, on the wire.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct ScheduledRunSlackTemplatesDto {
    pub pending: ScheduledRunSlackTemplateDto,
    pub in_progress: ScheduledRunSlackTemplateDto,
    pub succeeded: ScheduledRunSlackTemplateDto,
    pub failed: ScheduledRunSlackTemplateDto,
    pub error: ScheduledRunSlackTemplateDto,
    pub skipped: ScheduledRunSlackTemplateDto,
}

impl From<ScheduledRunSlackTemplates> for ScheduledRunSlackTemplatesDto {
    fn from(templates: ScheduledRunSlackTemplates) -> Self {
        Self {
            pending: templates.pending.into(),
            in_progress: templates.in_progress.into(),
            succeeded: templates.succeeded.into(),
            failed: templates.failed.into(),
            error: templates.error.into(),
            skipped: templates.skipped.into(),
        }
    }
}

impl From<ScheduledRunSlackTemplatesDto> for ScheduledRunSlackTemplates {
    fn from(dto: ScheduledRunSlackTemplatesDto) -> Self {
        Self {
            pending: dto.pending.into(),
            in_progress: dto.in_progress.into(),
            succeeded: dto.succeeded.into(),
            failed: dto.failed.into(),
            error: dto.error.into(),
            skipped: dto.skipped.into(),
        }
    }
}

/// The tenant's notification settings, on the wire —
/// `GET/PUT /qa/v1/settings/notifications`. Unlike
/// [`JiraSettingsDto`], no field here needed a **rename** —
/// [`Self::slack_webhook_credstore_ref`] is already named for what it holds.
///
/// **The name was the only thing that was already right** (Phase C's final
/// review, Important 1). That field is a credential-store reference and never a
/// URL — possession of a Slack incoming-webhook URL *is* the authorization to
/// post, so the column is as sensitive as a JIRA API token's reference — and
/// until that review nothing enforced it, so an operator following the field's
/// own name was the only thing keeping the secret out of a document
/// `GET /qa/v1/settings/notifications` hands to any holder of
/// `qa.notification_config/get`.
/// `domain::service::notify::NotifyService::save_config` now applies the JIRA
/// surface's own syntax check to it. The one asymmetry that remains with
/// [`JiraSettingsDto`] is that an empty value here *clears* the reference
/// instead of preserving the stored one — see that method's doc.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "seven independent on/off settings, one-to-one with \
              qa_insights_sdk::NotificationConfig - not a state machine this DTO \
              invents"
)]
pub struct NotificationConfigDto {
    pub slack_webhook_credstore_ref: String,
    pub slack_channel: String,
    pub manager_ui_base_url: String,
    pub slack_enabled: bool,
    pub notify_on_failure: bool,
    pub notify_on_success: bool,
    pub notify_on_schedule_completion: bool,
    pub scheduled_run_slack_enabled: bool,
    pub scheduled_run_slack_templates: ScheduledRunSlackTemplatesDto,
    pub run_queue_queued_slack_enabled: bool,
    pub email_smtp_host: String,
    pub email_smtp_port: u16,
    pub email_from: String,
    pub email_recipients: String,
    pub email_enabled: bool,
}

impl From<NotificationConfig> for NotificationConfigDto {
    fn from(config: NotificationConfig) -> Self {
        Self {
            slack_webhook_credstore_ref: config.slack_webhook_credstore_ref,
            slack_channel: config.slack_channel,
            manager_ui_base_url: config.manager_ui_base_url,
            slack_enabled: config.slack_enabled,
            notify_on_failure: config.notify_on_failure,
            notify_on_success: config.notify_on_success,
            notify_on_schedule_completion: config.notify_on_schedule_completion,
            scheduled_run_slack_enabled: config.scheduled_run_slack_enabled,
            scheduled_run_slack_templates: config.scheduled_run_slack_templates.into(),
            run_queue_queued_slack_enabled: config.run_queue_queued_slack_enabled,
            email_smtp_host: config.email_smtp_host,
            email_smtp_port: config.email_smtp_port,
            email_from: config.email_from,
            email_recipients: config.email_recipients,
            email_enabled: config.email_enabled,
        }
    }
}

impl From<NotificationConfigDto> for NotificationConfig {
    fn from(dto: NotificationConfigDto) -> Self {
        Self {
            slack_webhook_credstore_ref: dto.slack_webhook_credstore_ref,
            slack_channel: dto.slack_channel,
            manager_ui_base_url: dto.manager_ui_base_url,
            slack_enabled: dto.slack_enabled,
            notify_on_failure: dto.notify_on_failure,
            notify_on_success: dto.notify_on_success,
            notify_on_schedule_completion: dto.notify_on_schedule_completion,
            scheduled_run_slack_enabled: dto.scheduled_run_slack_enabled,
            scheduled_run_slack_templates: dto.scheduled_run_slack_templates.into(),
            run_queue_queued_slack_enabled: dto.run_queue_queued_slack_enabled,
            email_smtp_host: dto.email_smtp_host,
            email_smtp_port: dto.email_smtp_port,
            email_from: dto.email_from,
            email_recipients: dto.email_recipients,
            email_enabled: dto.email_enabled,
        }
    }
}

/// One entry of `GET /qa/v1/settings/notifications/log`.
///
/// `run_id` is `null` for an entry that belongs to no run — a settings
/// `/test` send — rather than a zero UUID; see
/// `qa_insights_sdk::NotificationLogEntry`'s own divergence note.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct NotificationLogEntryDto {
    pub id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub run_id: Option<Uuid>,
    pub channel: String,
    pub event_type: String,
    pub outcome: String,
    pub detail: String,
}

impl From<NotificationLogEntry> for NotificationLogEntryDto {
    fn from(entry: NotificationLogEntry) -> Self {
        Self {
            id: entry.id,
            created_at: entry.created_at,
            run_id: entry.run_id,
            channel: entry.channel,
            event_type: entry.event_type,
            outcome: entry.outcome,
            detail: entry.detail,
        }
    }
}

/// `GET /qa/v1/settings/notifications/log`'s query string. [`OpenBugsQuery`]'s
/// reason for the plain `serde::Deserialize` rather than an `api_dto`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NotificationLogQuery {
    /// Defaults to 100, clamped to at most 500 by
    /// `domain::service::notify::NotifyService::list_log`.
    pub limit: Option<u64>,
}

/// `POST /qa/v1/settings/notifications/test`'s response — `{"status": "sent"}`.
/// This crate's convention is a typed response everywhere else, so the one field gets a
/// DTO rather than a bare `serde_json::Value`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct NotificationTestOutcomeDto {
    pub status: String,
}

/// `POST /qa/v1/settings/notifications/test`'s optional body. Absent (or an
/// absent body entirely) means the generic settings-page test; present means the
/// scheduled-run test, over the *given* config override and event.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct NotificationTestReq {
    pub config: NotificationConfigDto,
    /// One of `qa_runs_sdk::SLACK_NOTIFICATION_EVENTS`, e.g. `"failed"`.
    pub event: String,
}

/// `POST /qa/v1/settings/notifications/preview`'s body, always required (unlike
/// the test endpoint's optional one).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct NotificationPreviewReq {
    pub config: NotificationConfigDto,
    pub event: String,
}

/// `POST /qa/v1/settings/notifications/preview`'s response.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct NotificationPreviewDto {
    pub event: String,
    pub event_label: String,
    pub rendered_message: String,
    pub fallback_text: String,
    /// Slack Block Kit blocks: literally the `blocks` array the tenant's
    /// Slack would receive, so this endpoint's contract is Slack's wire
    /// format rather than this gear's own. Encoded from
    /// `domain::ports::SlackBlock` by `infra::notify::block_kit`, the one
    /// encoder the outbound adapter uses too — which is what makes "the
    /// preview shows what gets sent" true by construction (review finding
    /// #17).
    pub blocks: Vec<serde_json::Value>,
}

impl From<crate::domain::service::notify::ScheduledRunPreview> for NotificationPreviewDto {
    fn from(preview: crate::domain::service::notify::ScheduledRunPreview) -> Self {
        Self {
            event: preview.event,
            event_label: preview.event_label,
            rendered_message: preview.rendered_message,
            fallback_text: preview.fallback_text,
            blocks: crate::infra::notify::block_kit::encode_blocks(&preview.blocks),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Conversions with no test until Task 16's fix round, each of which a
    //! transposition compiles straight through.
    //!
    //! All of them are DB-free and PDP-free: what is under test is a field mapping
    //! and a serde attribute, and anything else in scope would be testing the
    //! harness.

    use time::macros::{date, datetime};

    use qa_insights_sdk::{
        CoverageBuild, CoverageSummary, DailyStatusPoint, DashboardRun, DashboardStats,
        FailedTestCard, FlakyTestCard, JiraPollerConfig, NotificationConfig, NotificationLogEntry,
        QualityVectorPassRate, SavedView, SavedViewScope, ScheduledRunSlackTemplate,
        ScheduledRunSlackTemplates, TestCaseResultRecord, TestResultRecord,
    };
    use uuid::Uuid;

    use super::{
        AnalyticsBuildTestsQuery, AnalyticsOverview, AnalyticsOverviewDto, AnalyticsOverviewQuery,
        AnalyticsPlanQuery, AnalyticsScopeDto, BuildTestDetailDto, BuildTestsQuery,
        CollectReportQuery, CoverageBuildDto, DailyStatusPointDto, DashboardRunDto,
        DashboardStatsDto, FailedTestCardDto, FlakyTestCardDto, GroupBy, HashMap, JiraBug,
        JiraBugDto, JiraBugFilingDto, JiraConfig, JiraPollerConfigDto, JiraSettingsDto,
        NewSavedViewReq, NotificationConfigDto, NotificationLogEntryDto, NotificationPreviewDto,
        PlanBuildDistributionDto, PlanTestHistoryDto, RebuildOutcomeDto, RebuildReq, SavedViewDto,
        SavedViewScopeDto, ScheduledRunSlackTemplateDto, ScheduledRunSlackTemplatesDto, Scope,
        TestCaseResultDto, TestResultDto, plan_test_analytics_list_dto,
    };
    use crate::domain::analytics::PlanRef;
    use crate::domain::analytics::aggregates::{
        AnalyticsListItem, AnalyticsLists, BuildTestDetail, GroupedSummaries, HeatmapData,
        HeatmapRow, OverviewSummary, PlatformGroupSummary, QualityVectorSummary, TrendData,
        TrendPoint,
    };
    use crate::domain::error::DomainError;
    use crate::domain::ports::jira_client::IssueRef;
    use crate::domain::service::analytics::{
        PlanBuildDistribution, PlanTestAnalytics, PlanTestHistory, PlanTestHistoryEntry, PlanTests,
    };
    use crate::domain::service::notify::ScheduledRunPreview;
    use crate::domain::service::reconcile::ReconcileOutcome;
    use crate::domain::service::saved_views::SavedViewInput;

    /// **`DashboardRunDto` serializes `environment_id`, never `environment_id`.**
    ///
    /// Important-4 of the Task 25 review: a struct-field read is a proxy for
    /// the wire shape, not the wire shape itself - only a real
    /// `serde_json::to_value` renders the actual key.
    #[test]
    fn the_dashboard_run_serialises_environment_id_not_platform_id() {
        let run = DashboardRun {
            run_id: Uuid::from_u128(0x61),
            name: "smoke-3".to_owned(),
            phase: "succeeded".to_owned(),
            repo_id: None,
            plan_path: None,
            environment_id: Some(Uuid::from_u128(0x62)),
            product_key: None,
            app_version: None,
            started_at: None,
            duration: None,
        };
        let json = serde_json::to_value(DashboardRunDto::from(run))
            .expect("a dashboard run must serialise");
        assert_eq!(json["environment_id"], Uuid::from_u128(0x62).to_string());
        assert!(
            json.get("platform_id").is_none(),
            "the pre-Task-25 key must not reappear: {json}"
        );
    }

    /// **`scanned` and `replayed` are both `usize`, so transposing them
    /// compiles and every other test in the crate still passes.**
    ///
    /// The values are deliberately distinct, and deliberately the *stopped* shape
    /// (`scanned: 3, backfilled: 1`) rather than the equal shape a clean rebuild
    /// produces — equal values are exactly where a transposition hides. A
    /// transposed mapping reports `replayed: 3` of `scanned: 1`, and an operator
    /// reading `replayed >= scanned` does not re-run the window that
    /// `stopped_at_gap` is telling them to.
    ///
    /// This is the hazard `ReconcileService::new`'s own `#[expect]` reasoning
    /// declines a params struct over — "all eight parameter types are distinct,
    /// so any transposition is a compile error". Here they are not distinct and
    /// there is no compile error, so the argument obliges a test instead.
    #[test]
    fn the_outcome_conversion_maps_each_field_to_its_own_name() {
        let dto = RebuildOutcomeDto::from(ReconcileOutcome {
            scanned: 3,
            backfilled: 1,
            watermark_advanced_to: None,
            stopped_at_gap: true,
            stopped_at_run: Some(Uuid::from_u128(0x5709)),
        });

        assert_eq!(
            dto.scanned, 3,
            "scanned must carry ReconcileOutcome::scanned"
        );
        assert_eq!(
            dto.replayed, 1,
            "replayed must carry ReconcileOutcome::backfilled, not scanned"
        );
        assert!(dto.stopped_at_gap);
    }

    /// The clean shape too, because it is the one an operator sees most often and
    /// the field-order defence above does not cover the `stopped_at_gap: false`
    /// leg.
    #[test]
    fn a_clean_rebuild_reports_every_scanned_run_as_replayed() {
        let dto = RebuildOutcomeDto::from(ReconcileOutcome {
            scanned: 7,
            backfilled: 7,
            watermark_advanced_to: None,
            stopped_at_gap: false,
            stopped_at_run: None,
        });

        assert_eq!((dto.scanned, dto.replayed), (7, 7));
        assert!(!dto.stopped_at_gap);
    }

    /// `#[serde(with = "time::serde::rfc3339")]` on a **request** field, which is
    /// the load-bearing case: `time`'s default `Deserialize` for
    /// `OffsetDateTime` is not the RFC 3339 form, so without the attribute an
    /// operator posting `"2026-08-18T08:00:00Z"` gets a deserialization failure
    /// rather than a window. Nothing else in this crate exercises that path —
    /// the route tests build the `OpenAPI` document, not a request body.
    ///
    /// Distinct instants per field, so a transposed `from`/`to` fails here as
    /// well.
    #[test]
    fn a_rebuild_request_deserializes_rfc3339_instants_into_the_right_fields() {
        let req: RebuildReq =
            serde_json::from_str(r#"{"from":"2026-08-18T08:00:00Z","to":"2026-08-18T10:30:00Z"}"#)
                .expect("an RFC 3339 body must deserialize");

        assert_eq!(req.from, datetime!(2026-08-18 08:00:00 UTC));
        assert_eq!(req.to, datetime!(2026-08-18 10:30:00 UTC));
    }

    /// An offset that is not UTC is accepted and normalised by comparison, so an
    /// operator in a local timezone is not silently given a different window.
    #[test]
    fn a_non_utc_offset_is_understood_rather_than_refused() {
        let req: RebuildReq = serde_json::from_str(
            r#"{"from":"2026-08-18T10:00:00+02:00","to":"2026-08-18T12:00:00+02:00"}"#,
        )
        .expect("a non-UTC offset is still RFC 3339");

        assert_eq!(req.from, datetime!(2026-08-18 08:00:00 UTC));
        assert_eq!(req.to, datetime!(2026-08-18 10:00:00 UTC));
    }

    /// A body that is not RFC 3339 is refused at the transport, which is what
    /// makes the 400 the route advertises reachable without the service being
    /// involved.
    #[test]
    fn a_body_that_is_not_rfc3339_is_refused() {
        assert!(
            serde_json::from_str::<RebuildReq>(r#"{"from":"2026-08-18","to":"2026-08-19"}"#)
                .is_err(),
            "a bare date is not an instant and must not be guessed at"
        );
    }

    /// **Every field of the file-level DTO carries its own record field.**
    ///
    /// Three groups of same-typed fields, within each of which a transposition is
    /// correct Rust that compiles clean and passes every other test in this crate:
    /// `test_file`/`test_name`/`status` (`String`),
    /// `duration`/`launch_id`/`jira_key`/`product_version`/`app_build`/`plan_path`/`branch`
    /// (`Option<String>`), and `environment_id`/`repo_id` (`Option<Uuid>`), with
    /// `id`/`run_id` a fourth (`Uuid`). Each value here identifies its own field.
    ///
    /// Asserted on the serialized JSON rather than field by field, which covers
    /// two things at once: the mapping, and the fact that `api_dto` renders the
    /// field names in `snake_case` — the same vocabulary a caller writes in a
    /// `$filter`, which is what `infra::storage::odata`'s field enums advertise.
    #[test]
    fn the_test_result_conversion_maps_each_field_to_its_own_name() {
        let dto = TestResultDto::from(TestResultRecord {
            id: Uuid::from_u128(1),
            run_id: Uuid::from_u128(2),
            test_file: "tests/smoke.py".to_owned(),
            test_name: "test_login".to_owned(),
            status: "FAILED".to_owned(),
            duration: Some("85.06s (0:01:25)".to_owned()),
            launch_id: Some("7204".to_owned()),
            jira_key: Some("VHP-319".to_owned()),
            product_version: Some("9.1.0".to_owned()),
            app_build: Some("9.1.0-4412".to_owned()),
            environment_id: Some(Uuid::from_u128(3)),
            repo_id: Some(Uuid::from_u128(4)),
            plan_path: Some("plans/smoke.yaml".to_owned()),
            branch: Some("main".to_owned()),
            run_finished_at: Some(datetime!(2026-08-18 10:30:00 UTC)),
            // Distinct from both other instants, and asserted *absent* below:
            // this is the one contract field the DTO withholds, so a projection
            // that started publishing it would turn this test red rather than
            // adding a key nobody decided to add.
            run_created_at: Some(datetime!(2026-08-18 09:15:00 UTC)),
            created_at: datetime!(2026-08-18 11:00:00 UTC),
        });

        let json: serde_json::Value = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["test_file"], "tests/smoke.py");
        assert_eq!(json["test_name"], "test_login");
        assert_eq!(json["status"], "FAILED");
        assert_eq!(json["duration"], "85.06s (0:01:25)");
        assert_eq!(json["launch_id"], "7204");
        assert_eq!(json["jira_key"], "VHP-319");
        assert_eq!(json["product_version"], "9.1.0");
        assert_eq!(json["app_build"], "9.1.0-4412");
        assert_eq!(json["plan_path"], "plans/smoke.yaml");
        assert_eq!(json["branch"], "main");
        assert_eq!(json["id"], Uuid::from_u128(1).to_string());
        assert_eq!(json["run_id"], Uuid::from_u128(2).to_string());
        assert_eq!(json["environment_id"], Uuid::from_u128(3).to_string());
        assert_eq!(json["repo_id"], Uuid::from_u128(4).to_string());
        // The two instants: RFC 3339 in both directions, and the nullable one
        // through `rfc3339::option`, which is the attribute that would otherwise
        // fail to compile rather than fail to serialise.
        assert_eq!(json["run_finished_at"], "2026-08-18T10:30:00Z");
        assert_eq!(json["created_at"], "2026-08-18T11:00:00Z");
        assert!(
            !json
                .as_object()
                .expect("the DTO is an object")
                .contains_key("run_created_at"),
            "run_created_at is the one contract field this DTO withholds, and \
             this type's doc says why; publishing it is a decision, not a \
             mapper detail",
        );
    }

    /// An absent `run_finished_at` is `null` on the wire, not omitted and not
    /// substituted.
    ///
    /// This is the run-still-in-progress row, which this gear really does store —
    /// and the field a client is most likely to mis-handle, because it is also the
    /// one the collection refuses to sort by.
    #[test]
    fn a_run_still_in_progress_serialises_a_null_finish_instant() {
        let dto = TestResultDto::from(TestResultRecord {
            id: Uuid::from_u128(1),
            run_id: Uuid::from_u128(2),
            test_file: String::new(),
            test_name: "test_login".to_owned(),
            status: "RUNNING".to_owned(),
            duration: None,
            launch_id: None,
            jira_key: None,
            product_version: None,
            app_build: None,
            environment_id: None,
            repo_id: None,
            plan_path: None,
            branch: None,
            run_finished_at: None,
            run_created_at: Some(datetime!(2026-08-18 10:00:00 UTC)),
            created_at: datetime!(2026-08-18 11:00:00 UTC),
        });

        let json: serde_json::Value = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["run_finished_at"], serde_json::Value::Null);
        // `""`, not null: the column is NOT NULL DEFAULT '' and a client grouping
        // by file must see the empty bucket rather than missing data.
        assert_eq!(json["test_file"], "");
    }

    /// **Every field of the case-level DTO carries its own record field.**
    ///
    /// `test_file`/`nodeid`/`name`/`status` and `duration`/`reason`/`ticket` are
    /// the two same-typed groups, and `name`/`nodeid` and `reason`/`ticket` the
    /// two pairs a transposition
    /// hides in, and `nodeid` is built from the other two so a swap cannot
    /// coincide. The field *names* are asserted too, because this DTO's spellings
    /// are deliberately different from the file-level one's — `name` not
    /// `test_name`, `ticket` not `jira_key` — and a "helpful" rename would break
    /// every client and every `$filter` written against them.
    #[test]
    fn the_test_case_result_conversion_maps_each_field_to_its_own_name() {
        let dto = TestCaseResultDto::from(TestCaseResultRecord {
            id: Uuid::from_u128(5),
            run_id: Uuid::from_u128(6),
            test_file: "tests/smoke.py".to_owned(),
            nodeid: "tests/smoke.py::TestLogin::test_login[eu]".to_owned(),
            name: "test_login".to_owned(),
            status: "XFAIL".to_owned(),
            duration: Some("0.31s".to_owned()),
            reason: Some("known upstream defect".to_owned()),
            ticket: Some("VHP-9".to_owned()),
            created_at: datetime!(2026-08-18 11:00:00 UTC),
        });

        let json: serde_json::Value = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["test_file"], "tests/smoke.py");
        assert_eq!(json["nodeid"], "tests/smoke.py::TestLogin::test_login[eu]");
        assert_eq!(json["name"], "test_login");
        assert_eq!(json["status"], "XFAIL");
        assert_eq!(json["duration"], "0.31s");
        assert_eq!(json["reason"], "known upstream defect");
        assert_eq!(json["ticket"], "VHP-9");
        assert_eq!(json["id"], Uuid::from_u128(5).to_string());
        assert_eq!(json["run_id"], Uuid::from_u128(6).to_string());
        assert_eq!(json["created_at"], "2026-08-18T11:00:00Z");

        // The spellings this table does *not* use. `Value::Null` is what
        // `serde_json::Value`'s `Index` yields for an absent key, so these assert
        // absence rather than emptiness.
        assert_eq!(json["test_name"], serde_json::Value::Null);
        assert_eq!(json["jira_key"], serde_json::Value::Null);
        assert_eq!(json["run_finished_at"], serde_json::Value::Null);
    }

    /// **The day is `YYYY-MM-DD`, zero-padded**, which is legacy's rendering
    /// (`row.day.format("%Y-%m-%d")`, `manager/src/routes/dashboard.rs:237`).
    ///
    /// A single-digit month and day is the case that matters: `format!("{}-{}-{}")`
    /// compiles, produces `2026-1-5`, and every chart library parses it
    /// differently or not at all. Asserted on a January date for exactly that
    /// reason.
    #[test]
    fn a_daily_point_renders_its_day_zero_padded() {
        let dto = DailyStatusPointDto::from(DailyStatusPoint {
            day: date!(2026 - 01 - 05),
            passed: 7,
            failed: 2,
        });
        assert_eq!(dto.day, "2026-01-05");

        let json: serde_json::Value = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["day"], "2026-01-05");
        assert_eq!(json["passed"], 7);
        assert_eq!(json["failed"], 2);
    }

    /// **The four fields nothing computes yet are absent from the wire, not
    /// zero — and an absent key is not the same thing as a `null` one.**
    ///
    /// The whole argument is on [`DashboardStatsDto`]: a `0` is indistinguishable
    /// from a measured zero, and this is the assertion that makes the decision
    /// visible rather than a matter of which fields somebody remembered to add.
    ///
    /// **The absence is checked with `contains_key`, not against
    /// `Value::Null`.** It was the latter through Task 21a, when every uncomputed
    /// field was also an *absent* one and the two were indistinguishable. They no
    /// longer are: `pass_rate_24h` is computed, is declared, and is `null` here
    /// precisely because this fixture's window is empty — so an
    /// `assert_eq!(json["pass_rate_24h"], Value::Null)` would pass whether the
    /// field were declared or dropped. `contains_key` separates the two, which is
    /// what the omission convention is actually claiming.
    ///
    /// It also pins the counts against transposition. `total_runs`,
    /// `active_runs`, `queued_runs`, `failed_24h_count` and
    /// `failed_prev_24h_count` are all `u64` and would compile in any order, so
    /// the fixture gives each a distinct value.
    ///
    /// **The two rates are not pinned here, and this doc claimed they were.** It
    /// said "the two `Option<f64>` rates get distinct values for the same reason"
    /// while the fixture left both at `Default`, i.e. both `None` — under which
    /// `pass_rate_24h: stats.pass_rate_prev_24h` compiles, ships and passes. That
    /// is the field-copy family this crate has already been caught by twice, on
    /// the one pair whose entire purpose is a delta. The claim is retracted here
    /// and the assertion is
    /// [`the_two_pass_rates_do_not_swap_on_the_way_to_the_wire`], which needs a
    /// non-null fixture and therefore cannot live in this test.
    ///
    /// **It listed five until Task 23b**, which computed `flaky_tests` and moved
    /// it into the *present* loop below — where it is asserted as an empty array
    /// rather than only as a key, because "the field is on the wire" and "the
    /// window held no flaky test" are two different statements and this fixture
    /// makes the second one true.
    ///
    /// **It listed four until Task 25a**, which computed
    /// `quality_vectors_pass_rate` and moved it the same way. Three are left, and
    /// **none of them has an owning task** — the previous two both did, so the
    /// list has stopped shrinking on a schedule; whoever adds a plan task for one
    /// of the three is the next reader of this comment.
    #[test]
    fn the_dashboard_payload_omits_what_nothing_computes_yet() {
        let dto = DashboardStatsDto::from(DashboardStats {
            total_runs: 11,
            active_runs: 22,
            queued_runs: 33,
            failed_24h_count: 44,
            failed_prev_24h_count: 55,
            ..DashboardStats::default()
        });

        let json: serde_json::Value = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["total_runs"], 11);
        assert_eq!(json["active_runs"], 22);
        assert_eq!(json["queued_runs"], 33);
        assert_eq!(json["failed_24h_count"], 44);
        assert_eq!(json["failed_prev_24h_count"], 55);
        assert_eq!(json["recent_runs"], serde_json::json!([]));
        assert_eq!(json["daily_test_status_trend"], serde_json::json!([]));
        assert_eq!(json["failed_recent"], serde_json::json!([]));
        assert_eq!(
            json["flaky_tests"],
            serde_json::json!([]),
            "computed since Task 23b, so the key is present and the empty window \
             is an empty array rather than a missing field",
        );
        assert_eq!(
            json["quality_vectors_pass_rate"],
            serde_json::json!([]),
            "computed since Task 25a, on the same rule: the key is present and \
             the empty answer is an empty array",
        );

        let keys = json.as_object().expect("the payload is an object");
        for present in ["pass_rate_24h", "pass_rate_prev_24h"] {
            assert!(
                keys.contains_key(present),
                "{present} is computed, so the key must be on the wire even when \
                 its value is null",
            );
            assert_eq!(
                json[present],
                serde_json::Value::Null,
                "{present} is null for a window with nothing to divide by, which \
                 is not the same as 0.0",
            );
        }

        for absent in ["total_plans", "total_schedules", "platforms_summary"] {
            assert!(
                !keys.contains_key(absent),
                "{absent} must be absent rather than reported as a value nothing computed",
            );
        }
    }

    /// **The quality-vector row's four counters do not rotate on the way to the
    /// wire.**
    ///
    /// `passed`, `failed`, `total` and `tests` are **all** `u64` and adjacent in
    /// both structs, so any permutation of the four in
    /// `QualityVectorPassRateDto::from` is correct Rust. That is the worst
    /// transposition surface in this file — four same-typed fields where
    /// `FlakyTestCardDto`'s three were already the family this crate has been
    /// caught by — and there is no other witness: the service test asserts at the
    /// domain type, one layer earlier.
    ///
    /// The values are deliberately not round, not in ascending order, and chosen
    /// so that no two are equal and `total != passed + failed`: `total` is `9`
    /// where the sum would be `10`. A conversion that recomputed the denominator
    /// instead of copying it fails here, which matters because
    /// `qa_insights_sdk::QualityVectorPassRate::total` is a **rendered** number
    /// and legacy sums it independently.
    #[test]
    fn the_quality_vector_counters_do_not_rotate_on_the_way_to_the_wire() {
        let dto = DashboardStatsDto::from(DashboardStats {
            quality_vectors_pass_rate: vec![QualityVectorPassRate {
                vector: "Security".to_owned(),
                passed: 7,
                failed: 3,
                total: 9,
                tests: 4,
            }],
            ..DashboardStats::default()
        });

        let json: serde_json::Value = serde_json::to_value(&dto).expect("the DTO serialises");
        let row = &json["quality_vectors_pass_rate"][0];
        assert_eq!(row["vector"], "Security");
        assert_eq!(row["passed"], 7);
        assert_eq!(row["failed"], 3);
        assert_eq!(row["total"], 9, "copied, not recomputed as passed + failed");
        assert_eq!(row["tests"], 4);
    }

    /// **The delta pair does not swap on the way to the wire.**
    ///
    /// `pass_rate_24h` and `pass_rate_prev_24h` are both `Option<f64>` and
    /// adjacent in both structs, so `DashboardStatsDto::from` transposing them is
    /// correct Rust. Nothing else would notice:
    /// `dashboard_tests::the_dashboard_reports_the_twenty_four_hour_kpis` pins the
    /// pair at the *service* layer, one type earlier, and this conversion has no
    /// other witness with a non-null rate.
    ///
    /// A sibling test rather than two more lines in
    /// [`the_dashboard_payload_omits_what_nothing_computes_yet`], because that one
    /// needs *both* rates `None` to assert that a computed-but-null field still
    /// carries its key — the two fixtures are mutually exclusive. Both counts are
    /// carried along for the same reason and with distinct values, so a
    /// four-field rotation across the block cannot pass either.
    ///
    /// The values are deliberately not round and not in ascending field order:
    /// `0.91` then `0.42` is a pass rate that *fell*, which is the direction a
    /// transposition would silently reverse on a delta the UI draws.
    #[test]
    fn the_two_pass_rates_do_not_swap_on_the_way_to_the_wire() {
        let dto = DashboardStatsDto::from(DashboardStats {
            failed_24h_count: 7,
            failed_prev_24h_count: 3,
            pass_rate_24h: Some(0.91),
            pass_rate_prev_24h: Some(0.42),
            ..DashboardStats::default()
        });

        let json: serde_json::Value = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["pass_rate_24h"], 0.91);
        assert_eq!(
            json["pass_rate_prev_24h"], 0.42,
            "the previous window's rate is the lower one in this fixture; a swap \
             turns a falling pass rate into a rising one"
        );
        assert_eq!(json["failed_24h_count"], 7);
        assert_eq!(json["failed_prev_24h_count"], 3);
    }

    /// **Every field of a flaky card reaches its own JSON key.**
    ///
    /// Seven fields, and the transposition surface is the widest on this endpoint:
    /// `passed`, `failed` and `total` are all `u64`, and `test_file` and
    /// `plan_path` are both `Option<String>` — so four of the six same-typed pairs
    /// compile in either order. The fixture gives every field a distinct value,
    /// and the counters deliberately do **not** ascend in field order: `19` passed
    /// against `4` failed with a total of `23` means a transposed pair reads as a
    /// mostly-failing test where this one mostly passes.
    ///
    /// `total` is `passed + failed` here, as it always is
    /// (`crate::domain::repos::FlakyGroup::total` says why it is nonetheless
    /// carried), so it cannot be given a value unrelated to the other two without
    /// making the fixture describe a group the query could not produce. `23` is
    /// still distinct from both, which is what the transposition check needs.
    ///
    /// The layer below is
    /// `domain::service::dashboard::a_flaky_card_carries_every_field_of_its_group`,
    /// and it has to be covered again here because this is a second seven-field
    /// literal — the same reason
    /// [`a_failure_card_maps_every_column_to_its_own_key`] duplicates its own.
    #[test]
    fn a_flaky_card_maps_every_field_to_its_own_key() {
        let dto = FlakyTestCardDto::from(FlakyTestCard {
            test_name: "test_retries_on_transient_dns_failure".to_owned(),
            test_file: Some("tests/flaky/dns.py".to_owned()),
            repo_id: Some(Uuid::from_u128(0xD4)),
            plan_path: Some("plans/nightly/plan.yaml".to_owned()),
            passed: 19,
            failed: 4,
            total: 23,
        });

        let json: serde_json::Value = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["test_name"], "test_retries_on_transient_dns_failure");
        assert_eq!(
            json["test_file"], "tests/flaky/dns.py",
            "the file is the group's representative pick, not the plan path"
        );
        assert_eq!(json["repo_id"], "00000000-0000-0000-0000-0000000000d4");
        assert_eq!(json["plan_path"], "plans/nightly/plan.yaml");
        assert_eq!(json["passed"], 19);
        assert_eq!(
            json["failed"], 4,
            "this test mostly passes; a transposed pair renders it as mostly \
             failing, which is the direction the flaky ranking depends on"
        );
        assert_eq!(json["total"], 23);
    }

    /// **A flaky card with no file and no plan emits `null`, not an empty
    /// string.**
    ///
    /// The three `Option`s are `None` when the group's rows named no file and its
    /// run named no plan. `""` on the wire would be a path a client could render;
    /// `null` is what legacy emits for its own nullable `test_file`, and
    /// `domain::service::dashboard::flaky_card` is where the `""` this schema
    /// stores becomes the `None` this asserts.
    #[test]
    fn a_flaky_card_with_no_file_or_plan_emits_nulls() {
        let dto = FlakyTestCardDto::from(FlakyTestCard {
            test_name: "test_unattributed".to_owned(),
            test_file: None,
            repo_id: None,
            plan_path: None,
            passed: 2,
            failed: 5,
            total: 7,
        });

        let json: serde_json::Value = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["test_file"], serde_json::Value::Null);
        assert_eq!(json["repo_id"], serde_json::Value::Null);
        assert_eq!(json["plan_path"], serde_json::Value::Null);
        assert_eq!(json["passed"], 2);
        assert_eq!(json["failed"], 5);
        assert_eq!(json["total"], 7);
    }

    /// **Every column of a failure card reaches its own JSON key.**
    ///
    /// Nine fields, three of them `Uuid`-shaped and four `Option<String>`, so a
    /// transposition inside either group compiles and ships — the same hazard
    /// `domain::service::dashboard::a_failure_card_carries_every_column_of_its_row`
    /// covers one layer down, and it has to be covered again here because this is
    /// a second nine-field literal.
    ///
    /// `finished_at` is asserted as a *string*: the field carries `#[serde(with
    /// = "time::serde::rfc3339::option")]`, and dropping that attribute changes
    /// the wire format without changing any Rust type. Legacy emits RFC 3339
    /// for this field too (`r.finished_at.map(|ts| ts.to_rfc3339)`,
    /// `manager/src/routes/dashboard.rs:293`).
    #[test]
    fn a_failure_card_maps_every_column_to_its_own_key() {
        let dto = FailedTestCardDto::from(FailedTestCard {
            test_name: "test_login_rejects_expired_token".to_owned(),
            test_file: Some("tests/regression/login.py".to_owned()),
            run_id: Uuid::from_u128(0xA1),
            repo_id: Some(Uuid::from_u128(0xB2)),
            plan_path: Some("plans/regression/plan.yaml".to_owned()),
            environment_id: Some(Uuid::from_u128(0xC3)),
            finished_at: Some(datetime!(2026-08-20 11:30:00 UTC)),
            jira_key: Some("VHP-4711".to_owned()),
            launch_id: Some("88213".to_owned()),
        });

        let json: serde_json::Value = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["test_name"], "test_login_rejects_expired_token");
        assert_eq!(json["test_file"], "tests/regression/login.py");
        assert_eq!(json["run_id"], "00000000-0000-0000-0000-0000000000a1");
        assert_eq!(json["repo_id"], "00000000-0000-0000-0000-0000000000b2");
        assert_eq!(json["plan_path"], "plans/regression/plan.yaml");
        assert_eq!(
            json["environment_id"],
            "00000000-0000-0000-0000-0000000000c3"
        );
        assert_eq!(json["finished_at"], "2026-08-20T11:30:00Z");
        assert_eq!(json["jira_key"], "VHP-4711");
        assert_eq!(json["launch_id"], "88213");
    }

    /// **A coverage point carries legacy's whole field set, percentages
    /// included** — `product_key`, `version`, `build` and the three percentages
    ///.
    ///
    /// The list `GET /qa/v1/dashboard/coverage` answers with is empty today and
    /// [`super::CoverageBuildDto`] says why, so this conversion has no other
    /// witness — which is precisely why it is tested here rather than left to the
    /// endpoint. Absence is expressed at the *list* level, as legacy expresses it;
    /// no field of a point is omitted, because the shape of a point is known and
    /// it is the points themselves that nothing produces yet.
    ///
    /// Every value is distinct and the three `f64`s are in no natural order, so a
    /// transposition — the failure a same-typed triple invites, and the one
    /// `RebuildOutcomeDto`'s test exists for — fails here instead of shipping a
    /// branch percentage drawn as a line percentage.
    #[test]
    fn the_coverage_point_carries_legacys_whole_field_set() {
        let dto = CoverageBuildDto::from(CoverageBuild {
            product_key: "vhp".to_owned(),
            version: "8.1.2".to_owned(),
            build: "vhp/8.1.2".to_owned(),
            coverage: CoverageSummary {
                line_pct: 81.5,
                branch_pct: 64.25,
                function_pct: 92.0,
            },
        });

        let json: serde_json::Value = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["product_key"], "vhp");
        assert_eq!(json["version"], "8.1.2");
        assert_eq!(
            json["build"], "vhp/8.1.2",
            "legacy's build label is the product key and the version joined by a slash, \
             not either half"
        );
        assert_eq!(json["coverage"]["line_pct"], 81.5);
        assert_eq!(json["coverage"]["branch_pct"], 64.25);
        assert_eq!(json["coverage"]["function_pct"], 92.0);
    }
    // -----------------------------------------------------------------------
    // Analytics (Task 25b)
    // -----------------------------------------------------------------------

    /// **Legacy's nine query parameters deserialize into the nine fields of the
    /// same name.**
    ///
    /// Eight of the nine are `String` or `Option<String>`, so any transposition
    /// among them compiles and passes every other test in this crate. Each value
    /// here identifies its own field.
    ///
    /// The route tests otherwise build the `OpenAPI` document rather than a
    /// request; [`AnalyticsPlanQuery`] is the other type in this crate driven
    /// through `serde_urlencoded` directly, for the fix-round reason its own
    /// header gives.
    #[test]
    fn an_overview_query_deserializes_each_of_legacys_nine_parameters() {
        let query: AnalyticsOverviewQuery = serde_urlencoded::from_str(
            "product_id=the-product&version=the-version&scope=the-scope&plan_id=the-plan\
             &branch=the-branch&days_heatmap=11&days_trend=222&group_by=the-group\
             &group_value=the-value",
        )
        .expect("legacy's nine parameters");

        assert_eq!(query.product_id, "the-product");
        assert_eq!(query.version, "the-version");
        assert_eq!(query.scope, "the-scope");
        assert_eq!(query.plan_id.as_deref(), Some("the-plan"));
        assert_eq!(query.branch.as_deref(), Some("the-branch"));
        assert_eq!(query.days_heatmap, Some(11));
        assert_eq!(query.days_trend, Some(222));
        assert_eq!(query.group_by.as_deref(), Some("the-group"));
        assert_eq!(query.group_value.as_deref(), Some("the-value"));
    }

    /// **Fix round, Important 1.** The whole reason `plan_id` moved off a path
    /// segment: a real `plan_path` contains `/` (this crate's own fixtures,
    /// `plans/nightly/plan.yaml`), and `serde_urlencoded` is the exact decoder
    /// `axum::extract::Query` uses at the route (this module's header on
    /// [`AnalyticsPlanQuery`] and the sibling test above both rely on that
    /// equivalence). Both the percent-encoded spelling a client would normally
    /// send and the raw slash a client is not required to encode — `/` is not
    /// a reserved character inside a query component, RFC 3986 3.4 — decode to
    /// the same value a path segment could never have carried.
    #[test]
    fn a_plan_query_carries_a_slash_bearing_plan_id_through_the_same_decoder_the_route_uses() {
        let percent_encoded: AnalyticsPlanQuery =
            serde_urlencoded::from_str("plan_id=plans%2Fnightly%2Fplan.yaml")
                .expect("a percent-encoded slash decodes");
        assert_eq!(percent_encoded.plan_id, "plans/nightly/plan.yaml");

        let raw: AnalyticsPlanQuery = serde_urlencoded::from_str("plan_id=plans/nightly/plan.yaml")
            .expect("a raw slash is not a reserved query character");
        assert_eq!(raw.plan_id, "plans/nightly/plan.yaml");
    }

    /// The three required parameters are enough, and the six optional ones are
    /// absent rather than defaulted at the transport — the defaults are
    /// `domain::analytics::query`'s, which is what makes them testable without a
    /// router.
    #[test]
    fn an_overview_query_needs_only_the_three_required_parameters() {
        let query: AnalyticsOverviewQuery =
            serde_urlencoded::from_str("product_id=p&version=v&scope=all")
                .expect("the three required parameters");

        assert_eq!(query.days_heatmap, None);
        assert_eq!(query.days_trend, None);
        assert_eq!(query.group_by, None);

        assert!(
            serde_urlencoded::from_str::<AnalyticsOverviewQuery>("version=v&scope=all").is_err(),
            "product_id has no default at the transport",
        );
    }

    /// The drill-down's eight, and in particular that `build` is its own
    /// parameter rather than being folded into the shared seven.
    #[test]
    fn a_build_tests_query_deserializes_legacys_eight_parameters() {
        let query: AnalyticsBuildTestsQuery = serde_urlencoded::from_str(
            "product_id=the-product&version=the-version&scope=the-scope&plan_id=the-plan\
             &branch=the-branch&group_by=the-group&group_value=the-value&build=the-build",
        )
        .expect("legacy's eight parameters");

        let domain = BuildTestsQuery::from(query);
        assert_eq!(domain.build, "the-build");
        assert_eq!(domain.product_id, "the-product");
        assert_eq!(domain.group_value.as_deref(), Some("the-value"));
    }

    /// **The environment name is joined into both sections that carry an id**,
    /// and an id the map does not hold renders as `null` rather than as a UUID.
    ///
    /// This is the whole point of Task 25a's port and of
    /// `AnalyticsOverview::platform_names`: the read is the service's and the
    /// rendering is this layer's, so a payload that carried the ids and no names
    /// would pass every service test and draw UUIDs at a user.
    #[test]
    fn the_environment_names_are_joined_into_the_bars_and_the_list_items() {
        let linux = Uuid::from_u128(0x51);
        let ghost = Uuid::from_u128(0x52);
        let mut overview = overview_shell();
        overview.platform_names.insert(linux, "Linux".to_owned());
        overview.grouped.platform = vec![platform_bar(ghost), platform_bar(linux)];
        overview.lists.passed = vec![list_item(Some(linux)), list_item(Some(ghost))];

        let dto = AnalyticsOverviewDto::from(overview);

        assert_eq!(
            dto.lists.passed[0].last_environment.as_deref(),
            Some("Linux"),
            "a resolved id renders its name",
        );
        assert_eq!(
            dto.lists.passed[1].last_environment, None,
            "an unresolved id is null, not the UUID's string",
        );
        assert_eq!(dto.lists.passed[1].last_environment_id, Some(ghost));

        let bars: Vec<(Option<&str>, Uuid)> = dto
            .grouped
            .environment
            .iter()
            .map(|bar| (bar.environment.as_deref(), bar.environment_id))
            .collect();
        assert_eq!(
            bars,
            vec![(Some("Linux"), linux), (None, ghost)],
            "named bars sort first, by name; the unnamed one is kept and sorted last",
        );

        // Important-4 of the Task 25 review: every assertion above reads the
        // Rust struct, which is a proxy for the wire shape - `rename_all =
        // "snake_case"` happens to make the two agree today, but a
        // field-level `#[serde(rename)]` added later would flip the wire back
        // to the pre-Task-25 names while every struct-field assertion above
        // stayed green (this is exactly Mutation A of the review). Only the
        // serialized JSON keys are the wire shape itself.
        let json = serde_json::to_value(&dto).expect("the overview must serialise");
        assert_eq!(
            json["lists"]["passed"][0]["last_environment"], "Linux",
            "{json}"
        );
        assert_eq!(
            json["lists"]["passed"][1]["last_environment_id"],
            ghost.to_string(),
            "{json}"
        );
        assert!(
            json["lists"]["passed"][0].get("last_platform").is_none(),
            "the pre-Task-25 key must not reappear: {json}"
        );
        assert!(
            json["lists"]["passed"][0].get("last_platform_id").is_none(),
            "the pre-Task-25 key must not reappear: {json}"
        );
        assert_eq!(
            json["grouped"]["environment"][0]["environment"], "Linux",
            "{json}"
        );
        assert_eq!(
            json["grouped"]["environment"][1]["environment_id"],
            ghost.to_string(),
            "{json}"
        );
        assert!(
            json["grouped"].get("platform").is_none(),
            "the pre-Task-25 section key must not reappear: {json}"
        );
        assert!(
            json["grouped"]["environment"][0]
                .get("platform_id")
                .is_none(),
            "the pre-Task-25 key must not reappear: {json}"
        );
    }

    /// The named bars are ordered by **name**, not by id — legacy's own order,
    /// which its `BTreeMap<String, _>` gave it for free and Task 23's fold could
    /// not. The two ids here are chosen so that id order and name order disagree.
    #[test]
    fn the_environment_bars_are_ordered_by_resolved_name_and_not_by_id() {
        let first_id = Uuid::from_u128(0x01);
        let second_id = Uuid::from_u128(0x02);
        let mut overview = overview_shell();
        overview
            .platform_names
            .insert(first_id, "Windows".to_owned());
        overview.platform_names.insert(second_id, "Alma".to_owned());
        overview.grouped.platform = vec![platform_bar(first_id), platform_bar(second_id)];

        let dto = AnalyticsOverviewDto::from(overview);

        let names: Vec<&str> = dto
            .grouped
            .environment
            .iter()
            .filter_map(|bar| bar.environment.as_deref())
            .collect();
        assert_eq!(names, vec!["Alma", "Windows"]);
    }

    /// The two chart axes are **calendar days**, rendered `YYYY-MM-DD`, which is
    /// legacy's wire shape. Rendering them as RFC 3339 would put a spurious
    /// instant on a bucket that has none.
    #[test]
    fn the_chart_axes_render_as_dates_rather_than_instants() {
        let mut overview = overview_shell();
        overview.heatmap = HeatmapData {
            days: vec![date!(2026 - 08 - 17), date!(2026 - 08 - 18)],
            rows: vec![HeatmapRow {
                test_file: "tests/a.py".to_owned(),
                test_name: "a".to_owned(),
                values: vec!["PASSED", "NOT_RUN"],
            }],
        };
        overview.trend = TrendData {
            points: vec![TrendPoint {
                day: date!(2026 - 08 - 18),
                passed: 1,
                failed: 0,
                not_run: 0,
            }],
        };

        let dto = AnalyticsOverviewDto::from(overview);

        assert_eq!(dto.heatmap.days, vec!["2026-08-17", "2026-08-18"]);
        assert_eq!(dto.heatmap.rows[0].values, vec!["PASSED", "NOT_RUN"]);
        assert_eq!(dto.trend.points[0].day, "2026-08-18");
    }

    /// The echoed `scope` and `group_by` are legacy's lowercase spellings —
    /// `scope_to_str` and `group_to_str` — and
    /// not the enums' `Debug`.
    #[test]
    fn the_echoed_scope_and_grouping_use_legacys_spelling() {
        let mut overview = overview_shell();
        overview.scope = Scope::Plan;
        overview.group_by = GroupBy::Component;

        let dto = AnalyticsOverviewDto::from(overview);
        let json = serde_json::to_value(&dto).expect("the DTO serialises");

        // Asserted through `serde_json` rather than off the struct field (Task
        // 20 fix round): `scope` is [`AnalyticsScopeDto`] now, and the rendered
        // JSON is what the echo is for.
        assert_eq!(json["scope"], "plan");
        assert_eq!(dto.group_by, "component");
    }

    /// **Both analytics scopes render the spelling `scope_to_str` rendered.**
    ///
    /// [`AnalyticsOverviewDto::scope`] was a `String` filled by a private
    /// `const fn scope_to_str(Scope)` while the domain's closed [`Scope`] sat
    /// on the other side of the conversion. It is now [`AnalyticsScopeDto`],
    /// which is the sole encoder; `scope_to_str` is gone. Legacy's spellings
    /// are unchanged.
    #[test]
    fn every_analytics_scope_serialises_to_legacys_spelling() {
        assert_eq!(
            serde_json::to_value(AnalyticsScopeDto::from(Scope::All))
                .expect("a scope must serialize"),
            serde_json::json!("all")
        );
        assert_eq!(
            serde_json::to_value(AnalyticsScopeDto::from(Scope::Plan))
                .expect("a scope must serialize"),
            serde_json::json!("plan")
        );
        for scope in [Scope::All, Scope::Plan] {
            assert_eq!(
                Scope::from(AnalyticsScopeDto::from(scope)),
                scope,
                "the mirror must round-trip, so neither side can drift"
            );
        }
    }

    /// The half the `String` could not give: an unknown scope is a decode
    /// error rather than a value the UI has to defend against.
    ///
    /// **This is the echo, not the request.** The three analytics *query*
    /// strings stay `String` on purpose — `domain::analytics::query::
    /// parse_scope` accepts them trimmed and case-insensitively, so a serde
    /// enum there would refuse `ALL`, which is accepted today. No such
    /// argument applies to this field, which is an encoder.
    #[test]
    fn an_unknown_analytics_scope_is_rejected() {
        assert!(
            serde_json::from_value::<AnalyticsScopeDto>(serde_json::json!("everything")).is_err(),
            "an invented scope must not decode"
        );
        assert!(
            serde_json::from_value::<AnalyticsScopeDto>(serde_json::json!("All")).is_err(),
            "the Rust variant name is not the wire spelling and must not decode either"
        );
    }

    /// **Every field of the list item carries its own source field.**
    ///
    /// Four groups of same-typed fields, within each of which a transposition
    /// compiles clean and passes every other test in this crate:
    /// `test_file`/`test_name`/`plan_path`/`plan_name`/`last_status` (`String`),
    /// `component`/`last_build`/`case_status` (`Option<String>`), the four
    /// `u32` counters, and `repo_id` against `last_environment_id`. Each value
    /// below identifies its own field.
    ///
    /// It also pins the *renames*: legacy's `plan_id` becomes the
    /// `(repo_id, plan_path)` pair and its `last_run_name` becomes
    /// `last_run_id`, so this is what fails if either is quietly folded back
    /// into one string.
    #[test]
    fn the_list_item_conversion_maps_each_field_to_its_own_name() {
        let repo = Uuid::from_u128(0xAA);
        let platform = Uuid::from_u128(0xBB);
        let run = Uuid::from_u128(0xCC);
        let mut overview = overview_shell();
        overview.lists.failed = vec![AnalyticsListItem {
            test_file: "tests/the_file.py".to_owned(),
            test_name: "the display name".to_owned(),
            component: Some("the component".to_owned()),
            tags: vec!["the tag".to_owned()],
            plan: PlanRef {
                repo_id: repo,
                plan_path: "plans/the_plan.yaml".to_owned(),
            },
            plan_name: "the plan name".to_owned(),
            versions: vec!["9.1".to_owned()],
            last_status: "FAILED",
            last_environment_id: Some(platform),
            last_run_id: Some(run),
            last_build: Some("the build".to_owned()),
            last_run_finished_at: Some(datetime!(2026-08-18 10:00:00 UTC)),
            pass_count: 1,
            fail_count: 2,
            skipped_count: 3,
            total_runs: 6,
            case_status: Some("XFAIL".to_owned()),
            case_tickets: vec!["VHP-1".to_owned()],
        }];

        let dto = AnalyticsOverviewDto::from(overview);
        let item = &dto.lists.failed[0];

        assert_eq!(item.test_file, "tests/the_file.py");
        assert_eq!(item.test_name, "the display name");
        assert_eq!(item.component.as_deref(), Some("the component"));
        assert_eq!(item.tags, vec!["the tag".to_owned()]);
        assert_eq!(item.repo_id, repo);
        assert_eq!(item.plan_path, "plans/the_plan.yaml");
        assert_eq!(item.plan_name, "the plan name");
        assert_eq!(item.versions, vec!["9.1".to_owned()]);
        assert_eq!(item.last_status, "FAILED");
        assert_eq!(item.last_environment_id, Some(platform));
        assert_eq!(item.last_run_id, Some(run));
        assert_eq!(item.last_build.as_deref(), Some("the build"));
        assert_eq!(
            item.last_run_finished_at,
            Some(datetime!(2026-08-18 10:00:00 UTC)),
        );
        assert_eq!(
            (
                item.pass_count,
                item.fail_count,
                item.skipped_count,
                item.total_runs
            ),
            (1, 2, 3, 6),
        );
        assert_eq!(item.case_status.as_deref(), Some("XFAIL"));
        assert_eq!(item.case_tickets, vec!["VHP-1".to_owned()]);
    }

    /// The drill-down item keeps the runner's own status and renders the run's
    /// instant as RFC 3339 — the two things that distinguish it from the list
    /// item beside it.
    #[test]
    fn the_build_test_detail_conversion_keeps_the_runners_status() {
        let run = Uuid::from_u128(0xCC);
        let dto = BuildTestDetailDto::from(BuildTestDetail {
            test_file: "tests/the_file.py".to_owned(),
            test_name: "the display name".to_owned(),
            status: "XPASS".to_owned(),
            run_id: run,
            run_finished_at: datetime!(2026-08-18 10:00:00 UTC),
            component: Some("the component".to_owned()),
            tags: vec!["the tag".to_owned()],
        });

        assert_eq!(dto.status, "XPASS");
        assert_eq!(dto.run_id, run);
        let json = serde_json::to_value(&dto).expect("a detail serialises");
        assert_eq!(json["run_finished_at"], "2026-08-18T10:00:00Z");
    }

    /// `GET /qa/v1/analytics/plan/tests?plan_id=`' response shape: the
    /// environment id and the resolved name ride together, and an id absent
    /// from the resolved map renders `null` for the name —
    /// [`AnalyticsListItemDto`]'s same rule, applied here.
    #[test]
    fn plan_test_analytics_joins_the_resolved_environment_name() {
        let resolved = Uuid::from_u128(0xE1);
        let unresolved = Uuid::from_u128(0xE2);
        let run = Uuid::from_u128(0xE3);
        let mut names = HashMap::new();
        names.insert(resolved, "Linux x86_64".to_owned());

        let response = PlanTests {
            items: vec![
                PlanTestAnalytics {
                    test_name: "test_resolved".to_owned(),
                    last_status: "PASSED".to_owned(),
                    last_environment_id: Some(resolved),
                    last_version: Some("8.1.2".to_owned()),
                    last_run_id: run,
                    jira_key: Some("VHP-1".to_owned()),
                    total_runs: 3,
                    pass_count: 2,
                    fail_count: 1,
                },
                PlanTestAnalytics {
                    test_name: "test_unresolved".to_owned(),
                    last_status: "FAILED".to_owned(),
                    last_environment_id: Some(unresolved),
                    last_version: None,
                    last_run_id: run,
                    jira_key: None,
                    total_runs: 1,
                    pass_count: 0,
                    fail_count: 1,
                },
            ],
            platform_names: names,
        };

        let items = plan_test_analytics_list_dto(response);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].last_environment_id, Some(resolved));
        assert_eq!(items[0].last_environment.as_deref(), Some("Linux x86_64"));
        assert_eq!(
            (
                items[0].total_runs,
                items[0].pass_count,
                items[0].fail_count
            ),
            (3, 2, 1),
        );
        assert_eq!(
            items[1].last_environment, None,
            "an id the resolver did not return renders null, not the id itself",
        );

        // Important-4 of the Task 25 review: the wire key, not the struct
        // field - see `the_environment_names_are_joined_into_the_bars_and_the_list_items`.
        let json =
            serde_json::to_value(&items[0]).expect("a plan test analytics row must serialise");
        assert_eq!(json["last_environment_id"], resolved.to_string());
        assert_eq!(json["last_environment"], "Linux x86_64");
        assert!(
            json.get("last_platform_id").is_none(),
            "the pre-Task-25 key must not reappear: {json}"
        );
        assert!(
            json.get("last_platform").is_none(),
            "the pre-Task-25 key must not reappear: {json}"
        );
    }

    /// `GET /qa/v1/analytics/plan/builds?plan_id=`' response shape: a field
    /// copy with nothing derived, pinned against a transposition the way
    /// [`the_build_test_detail_conversion_keeps_the_runners_status`] pins
    /// [`BuildTestDetailDto`]'s.
    #[test]
    fn plan_build_distribution_conversion_copies_every_field() {
        let dto = PlanBuildDistributionDto::from(PlanBuildDistribution {
            build: "unknown".to_owned(),
            total: 10,
            passed: 3,
            failed: 4,
            skipped: 3,
        });

        assert_eq!(dto.build, "unknown");
        assert_eq!(
            (dto.total, dto.passed, dto.failed, dto.skipped),
            (10, 3, 4, 3)
        );
    }

    /// `GET /qa/v1/analytics/plan/test-history?plan_id=`'s response shape: a
    /// missing version serialises to `null`, not to `"unknown"` —
    /// [`PlanTestHistoryEntry::build`]'s header states this is deliberately
    /// **not** [`PlanBuildDistributionDto`]'s rule.
    #[test]
    fn plan_test_history_conversion_leaves_a_missing_build_null() {
        let run = Uuid::from_u128(0xE4);
        let dto = PlanTestHistoryDto::from(PlanTestHistory {
            test_name: "test_login".to_owned(),
            results: vec![
                PlanTestHistoryEntry {
                    build: None,
                    status: "FAILED".to_owned(),
                    run_id: run,
                },
                PlanTestHistoryEntry {
                    build: Some("8.1.2".to_owned()),
                    status: "PASSED".to_owned(),
                    run_id: run,
                },
            ],
        });

        assert_eq!(dto.test_name, "test_login");
        assert_eq!(dto.results.len(), 2);
        let json = serde_json::to_value(&dto).expect("a history entry serialises");
        assert_eq!(json["results"][0]["build"], serde_json::Value::Null);
        assert_eq!(json["results"][1]["build"], "8.1.2");
    }

    /// An [`AnalyticsOverview`] with every section empty, for the tests above to
    /// fill one at a time.
    fn overview_shell() -> AnalyticsOverview {
        AnalyticsOverview {
            product_id: "the-product".to_owned(),
            version: "9.1".to_owned(),
            scope: Scope::All,
            plan_id: None,
            branch: None,
            group_by: GroupBy::None,
            group_value: None,
            summary: OverviewSummary::default(),
            lists: AnalyticsLists::default(),
            heatmap: HeatmapData {
                days: Vec::new(),
                rows: Vec::new(),
            },
            trend: TrendData { points: Vec::new() },
            build_distribution: Vec::new(),
            flaky: Vec::new(),
            quality_vectors: QualityVectorSummary::default(),
            grouped: GroupedSummaries::default(),
            platform_names: HashMap::new(),
        }
    }

    fn platform_bar(environment_id: Uuid) -> PlatformGroupSummary {
        PlatformGroupSummary {
            environment_id,
            total: 1,
            passed: 1,
            failed: 0,
            not_run: 0,
        }
    }

    fn list_item(environment_id: Option<Uuid>) -> AnalyticsListItem {
        AnalyticsListItem {
            test_file: "tests/a.py".to_owned(),
            test_name: "a".to_owned(),
            component: None,
            tags: Vec::new(),
            plan: PlanRef {
                repo_id: Uuid::from_u128(0xAA),
                plan_path: "plans/smoke.yaml".to_owned(),
            },
            plan_name: "Smoke".to_owned(),
            versions: Vec::new(),
            last_status: "PASSED",
            last_environment_id: environment_id,
            last_run_id: None,
            last_build: None,
            last_run_finished_at: None,
            pass_count: 0,
            fail_count: 0,
            skipped_count: 0,
            total_runs: 0,
            case_status: None,
            case_tickets: Vec::new(),
        }
    }

    fn saved_view(query_json: &str) -> SavedView {
        SavedView {
            id: Uuid::from_u128(1),
            owner_id: Uuid::from_u128(2),
            scope: SavedViewScope::All,
            repo_id: None,
            plan_path: None,
            name: "Regressions".to_owned(),
            query_json: query_json.to_owned(),
            created_at: datetime!(2026-08-20 00:00:00 UTC),
            updated_at: datetime!(2026-08-20 00:00:00 UTC),
        }
    }

    /// **`query_json` must render as a JSON object on the wire, not as a
    /// string holding one.** A `String` field on [`SavedViewDto`] would
    /// serialise the stored text as `"query_json":"{\"a\":1}"` — valid JSON,
    /// and the wrong shape: legacy's own `AnalyticsSavedView::query_json` is a
    /// `serde_json::Value`, so a client
    /// expects an object it can read fields off directly.
    #[test]
    fn a_saved_views_query_json_renders_as_an_object_not_a_nested_string() {
        let dto = SavedViewDto::try_from(saved_view(r#"{"a":1}"#)).unwrap();
        let json = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["query_json"], serde_json::json!({"a": 1}));
    }

    /// A view with no plan renders both halves as `null`, not omitted —
    /// matching every other optional pair this crate's DTOs carry.
    #[test]
    fn a_global_saved_view_renders_a_null_plan_pair() {
        let dto = SavedViewDto::try_from(saved_view("{}")).unwrap();
        let json = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["repo_id"], serde_json::Value::Null);
        assert_eq!(json["plan_path"], serde_json::Value::Null);
    }

    /// `scope` renders as legacy's own wire spelling (`"all"`/`"plan"`), not
    /// as the Rust variant name — `SavedViewScope::as_str`'s own doc records
    /// that the two are not the same string.
    ///
    /// Asserted through `serde_json` rather than off the struct field (Task
    /// 20): the field is [`SavedViewScopeDto`] now, so `dto.scope == "all"`
    /// no longer even type-checks, and the rendered JSON is what the finding
    /// was ever about.
    #[test]
    fn a_saved_views_scope_renders_as_the_wire_spelling() {
        let dto = SavedViewDto::try_from(saved_view("{}")).unwrap();
        let json = serde_json::to_value(&dto).expect("the DTO serialises");
        assert_eq!(json["scope"], "all");
    }

    /// **Every `SavedViewScope` renders the spelling it always rendered.**
    ///
    /// `SavedViewDto::scope` was a `String` filled from
    /// `SavedViewScope::as_str` while the closed enum sat beside it (review
    /// finding #35). It is now [`SavedViewScopeDto`], a closed mirror whose
    /// spellings are asserted against `as_str` itself, so the SDK stays the
    /// single source of truth and this test cannot drift from it.
    #[test]
    fn every_saved_view_scope_serialises_to_its_sdk_spelling() {
        for scope in [SavedViewScope::All, SavedViewScope::Plan] {
            let rendered = serde_json::to_value(SavedViewScopeDto::from(scope))
                .expect("a scope must serialize");
            assert_eq!(
                rendered,
                serde_json::Value::String(scope.as_str().to_owned()),
                "the wire spelling must stay SavedViewScope::as_str's, for {scope:?}"
            );
            assert_eq!(
                SavedViewScope::from(SavedViewScopeDto::from(scope)),
                scope,
                "the mirror must round-trip, so neither side can drift"
            );
        }
    }

    /// The half the `String` could not give: an unknown scope is a decode
    /// error rather than a value the UI has to defend against.
    ///
    /// **This is the response side only.** The *request* side
    /// ([`NewSavedViewReq::scope`], [`SavedViewsListQuery::scope`] and the
    /// three analytics query strings) stays a `String` on purpose — see
    /// [`SavedViewScopeDto`]'s own doc — and already rejects an unknown value,
    /// with legacy's verbatim 400.
    #[test]
    fn an_unknown_saved_view_scope_is_rejected() {
        assert!(
            serde_json::from_value::<SavedViewScopeDto>(serde_json::json!("everything")).is_err(),
            "an invented scope must not decode"
        );
        assert!(
            serde_json::from_value::<SavedViewScopeDto>(serde_json::json!("All")).is_err(),
            "the Rust variant name is not the wire spelling and must not decode either"
        );
    }

    /// **Stored corruption is a `CorruptState`, not a panic.** Nothing in this
    /// gear's write path can produce this — `query_json_to_column` validates
    /// on every write — but the conversion has to be a `TryFrom` rather than
    /// an `.expect()` on principle: a stored-column decode failure is exactly
    /// the shape `domain::error::DomainError::CorruptState` exists for, and a
    /// panicking `From` would turn one bad row into a crashed request handler
    /// instead of a 500 that names the column.
    #[test]
    fn a_non_json_stored_query_json_is_corrupt_state_not_a_panic() {
        let err = SavedViewDto::try_from(saved_view("not json")).unwrap_err();
        match err {
            DomainError::CorruptState { what, id, value } => {
                assert_eq!(what, "saved_view.query_json");
                assert_eq!(id, Uuid::from_u128(1));
                assert_eq!(value, "not json");
            }
            other => panic!("expected CorruptState, got {other:?}"),
        }
    }

    /// The create/update request converts `query_json` to the domain's
    /// verbatim-string form by serialising the compact document — not by
    /// quoting it, which would be the double-encoding
    /// [`a_saved_views_query_json_renders_as_an_object_not_a_nested_string`]
    /// exists to keep off the *response* side too.
    ///
    /// The four field asserts below are **not** review finding #46's
    /// constructor echoes, even though they look like it: the compact-JSON
    /// assert covers exactly one field (`query_json`), while `impl
    /// From<NewSavedViewReq> for SavedViewInput` moves five, and `scope` and
    /// `name` are both plain `String`s — nothing but this assert stops a
    /// transposition (`scope: req.name, name: req.scope`) from compiling and
    /// shipping. This is also the only test that exercises that `From` impl at
    /// all; `saved_views_tests.rs`'s `view` helper builds `SavedViewInput`
    /// directly and never goes through it.
    #[test]
    fn a_new_saved_view_req_serialises_query_json_to_compact_text() {
        let req = NewSavedViewReq {
            scope: "plan".to_owned(),
            repo_id: Some(Uuid::from_u128(9)),
            plan_path: Some("plans/smoke/plan.yaml".to_owned()),
            name: "Regressions".to_owned(),
            query_json: serde_json::json!({"version": "5.0.1"}),
        };
        let input: SavedViewInput = req.into();
        assert_eq!(input.query_json, r#"{"version":"5.0.1"}"#);
        assert_eq!(input.scope, "plan");
        assert_eq!(input.repo_id, Some(Uuid::from_u128(9)));
        assert_eq!(input.plan_path.as_deref(), Some("plans/smoke/plan.yaml"));
        assert_eq!(input.name, "Regressions");
    }

    /// **`CollectReportQuery`'s own header says its three fields "must agree
    /// with `collect_url`'s private `Query` type"; this is the test that
    /// actually checks it (Phase B fix wave, Finding 6).**
    ///
    /// `collect_tests::the_collect_url_round_trips_a_slash_bearing_branch_through_its_query_string`
    /// drives the same production encoder,
    /// [`crate::domain::service::collect::encode_collect_report_query`], but
    /// decodes into its own local struct — a deliberate, legitimate choice
    /// (domain code must not import `api::rest::dto`) that nonetheless means
    /// it cannot see a change made to `CollectReportQuery` alone. This test
    /// decodes the identical, real encoder's output into the real
    /// `CollectReportQuery`, so the two independent type definitions this
    /// route depends on are checked against each other at least once,
    /// somewhere in this crate.
    ///
    /// A slash-bearing branch is used rather than a plain one for the same
    /// reason the domain-side test does: it is the one shape that actually
    /// distinguishes "percent-encoded and decoded correctly" from "merely
    /// present" — see `domain::service::collect`'s header, "The
    /// branch-in-path hazard".
    #[test]
    fn collect_report_query_decodes_collect_urls_real_encoder_output() {
        let tenant_id = Uuid::from_u128(0x42);
        let query = crate::domain::service::collect::encode_collect_report_query(
            "feature/VHP-123-thing",
            tenant_id,
            "deadbeef".to_owned(),
        );

        let decoded: CollectReportQuery = serde_urlencoded::from_str(&query)
            .expect("encode_collect_report_query's output must decode through CollectReportQuery");

        assert_eq!(decoded.branch, "feature/VHP-123-thing");
        assert_eq!(decoded.tenant_id, tenant_id);
        assert_eq!(decoded.sig, "deadbeef");
    }
    /// **The response body has no `api_token` field, under that or any other
    /// name that could hold material.**
    ///
    /// Legacy's `GET /api/settings/jira` returns a field literally called
    /// `api_token`, masked to `"********"`. This gear returns a
    /// credential-store reference instead, and this test is the one thing that
    /// notices if a future edit reintroduces the legacy field name — a change
    /// that would compile, would serialize, and would be a
    /// credential-disclosure bug the moment something populated it.
    ///
    /// Asserted over the *rendered JSON keys*, not over the struct: a struct
    /// field can be added without any other test in this crate changing.
    #[test]
    fn the_jira_settings_response_has_no_api_token_field() {
        let dto = JiraSettingsDto::from(JiraConfig {
            url: "https://acme.atlassian.net".to_owned(),
            project_key: "VHP".to_owned(),
            email: "qa@example.com".to_owned(),
            api_token_credstore_ref: "cred://qa-jira-api-token".to_owned(),
            issue_type: Some("Bug".to_owned()),
            enabled: true,
        });

        let json = serde_json::to_value(&dto).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .expect("the settings document is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "api_token_credstore_ref",
                "email",
                "enabled",
                "issue_type",
                "project_key",
                "url",
            ],
            "the wire document must carry exactly these six keys; `api_token` in particular \
             must never be one of them",
        );
        assert_eq!(
            json["api_token_credstore_ref"], "cred://qa-jira-api-token",
            "the reference is returned verbatim - unmasked, because it is not material",
        );
    }

    /// The request body maps onto the domain input field for field. Six `String`
    /// or `Option<String>` fields, four of which a transposition compiles
    /// straight through — `url`, `project_key`, `email` and the reference.
    #[test]
    fn a_jira_settings_request_maps_onto_the_domain_input() {
        let dto = JiraSettingsDto {
            url: "https://acme.atlassian.net".to_owned(),
            project_key: "VHP".to_owned(),
            email: "qa@example.com".to_owned(),
            api_token_credstore_ref: "cred://qa-jira-api-token".to_owned(),
            issue_type: None,
            enabled: true,
        };

        let input = crate::domain::service::jira::JiraConfigInput::from(dto);

        assert_eq!(input.url, "https://acme.atlassian.net");
        assert_eq!(input.project_key, "VHP");
        assert_eq!(input.email, "qa@example.com");
        assert_eq!(input.api_token_credstore_ref, "cred://qa-jira-api-token");
        assert_eq!(input.issue_type, None);
        assert!(input.enabled);
    }

    /// `JiraPollerConfigDto` round-trips both ways — no field rename, no
    /// masking asymmetry, unlike [`JiraSettingsDto`]'s credential reference.
    #[test]
    fn a_jira_poller_config_round_trips_through_its_dto() {
        let config = JiraPollerConfig {
            poll_interval_seconds: 900,
            auto_rerun_on_resolve: false,
        };

        let dto = JiraPollerConfigDto::from(config);
        assert_eq!(dto.poll_interval_seconds, 900);
        assert!(!dto.auto_rerun_on_resolve);

        let back = JiraPollerConfig::from(dto);
        assert_eq!(back, config);
    }

    /// Every field of a registry row survives the wire conversion, `plan_path`
    /// and `environment_id` in particular — the two fields
    /// `qa_insights_sdk::JiraBug`'s header names as this schema's divergence
    /// from legacy's single `plan_id`/`platform` strings.
    #[test]
    fn a_jira_bug_row_maps_onto_its_dto_field_for_field() {
        let bug = JiraBug {
            id: Uuid::from_u128(1),
            jira_key: "VHP-42".to_owned(),
            test_name: "AuthN Login".to_owned(),
            repo_id: Uuid::from_u128(2),
            plan_path: "plans/smoke/plan.yaml".to_owned(),
            app_version: Some("5.0.1".to_owned()),
            environment_id: Some(Uuid::from_u128(3)),
            status: "Resolved".to_owned(),
            summary: "[VHP] Test Failed: AuthN Login".to_owned(),
            created_at: datetime!(2026-08-18 08:00:00 UTC),
            resolved_at: Some(datetime!(2026-08-19 08:00:00 UTC)),
        };

        let dto = JiraBugDto::from(bug.clone());

        assert_eq!(dto.id, bug.id);
        assert_eq!(dto.jira_key, bug.jira_key);
        assert_eq!(dto.test_name, bug.test_name);
        assert_eq!(dto.repo_id, bug.repo_id);
        assert_eq!(dto.plan_path, bug.plan_path);
        assert_eq!(dto.app_version, bug.app_version);
        assert_eq!(dto.environment_id, bug.environment_id);
        assert_eq!(dto.status, bug.status);
        assert_eq!(dto.summary, bug.summary);
        assert_eq!(dto.created_at, bug.created_at);
        assert_eq!(dto.resolved_at, bug.resolved_at);

        // Important-4 of the Task 25 review: the struct-field asserts above
        // are a proxy for the wire shape, not the wire shape itself. Only the
        // serialized JSON key is what a caller actually reads.
        let json = serde_json::to_value(&dto).expect("a jira bug must serialise");
        assert_eq!(json["environment_id"], Uuid::from_u128(3).to_string());
        assert!(
            json.get("platform_id").is_none(),
            "the pre-Task-25 key must not reappear: {json}"
        );
    }

    /// A never-resolved bug's `resolved_at` renders as JSON `null`, not an
    /// absent key — the same `rfc3339::option` discipline every other nullable
    /// instant in this file follows.
    #[test]
    fn an_open_bugs_resolved_at_is_null_not_absent() {
        let dto = JiraBugDto::from(JiraBug {
            id: Uuid::from_u128(1),
            jira_key: "VHP-42".to_owned(),
            test_name: "AuthN Login".to_owned(),
            repo_id: Uuid::from_u128(2),
            plan_path: "plans/smoke/plan.yaml".to_owned(),
            app_version: None,
            environment_id: None,
            status: "Open".to_owned(),
            summary: "s".to_owned(),
            created_at: datetime!(2026-08-18 08:00:00 UTC),
            resolved_at: None,
        });

        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["resolved_at"], serde_json::Value::Null, "{json}");
    }

    /// Legacy's `created` is `false` for both of the port's dedupe paths and
    /// `true` only for a posted issue — the wire type carries whichever the
    /// domain settled on, unchanged.
    #[test]
    fn a_filed_issue_maps_onto_its_dto() {
        let created = JiraBugFilingDto::from(IssueRef {
            jira_key: "VHP-1".to_owned(),
            created: true,
        });
        assert_eq!(created.jira_key, "VHP-1");
        assert!(created.created);

        let found = JiraBugFilingDto::from(IssueRef {
            jira_key: "VHP-2".to_owned(),
            created: false,
        });
        assert_eq!(found.jira_key, "VHP-2");
        assert!(!found.created);
    }

    // ==================== Notification settings (Task 38, fix round 3, Important 6) ====================

    /// One status template, all seven fields distinct — the six `Option<String>`
    /// fields get their own literal each, and `enabled` is asserted directly
    /// since a lone `bool` has nothing to transpose with *inside* this type.
    fn sample_template(tag: &str) -> ScheduledRunSlackTemplate {
        ScheduledRunSlackTemplate {
            enabled: true,
            status_icon: Some(format!("icon-{tag}")),
            header: Some(format!("header-{tag}")),
            summary: Some(format!("summary-{tag}")),
            results: Some(format!("results-{tag}")),
            body: Some(format!("body-{tag}")),
            footer: Some(format!("footer-{tag}")),
        }
    }

    #[test]
    fn the_scheduled_run_slack_template_conversion_maps_each_field_to_its_own_name() {
        let dto = ScheduledRunSlackTemplateDto::from(sample_template("t"));
        assert!(dto.enabled);
        assert_eq!(dto.status_icon.as_deref(), Some("icon-t"));
        assert_eq!(dto.header.as_deref(), Some("header-t"));
        assert_eq!(dto.summary.as_deref(), Some("summary-t"));
        assert_eq!(dto.results.as_deref(), Some("results-t"));
        assert_eq!(dto.body.as_deref(), Some("body-t"));
        assert_eq!(dto.footer.as_deref(), Some("footer-t"));

        let back = ScheduledRunSlackTemplate::from(dto);
        assert_eq!(back, sample_template("t"));
    }

    /// Six statuses, each carrying a template only that status can produce —
    /// the transposition [`ScheduledRunSlackTemplates`]'s six same-shaped
    /// fields invite.
    #[test]
    fn the_scheduled_run_slack_templates_conversion_maps_each_status_to_its_own_template() {
        let templates = ScheduledRunSlackTemplates {
            pending: sample_template("pending"),
            in_progress: sample_template("in_progress"),
            succeeded: sample_template("succeeded"),
            failed: sample_template("failed"),
            error: sample_template("error"),
            skipped: sample_template("skipped"),
        };

        let dto = ScheduledRunSlackTemplatesDto::from(templates.clone());
        assert_eq!(dto.pending.header.as_deref(), Some("header-pending"));
        assert_eq!(
            dto.in_progress.header.as_deref(),
            Some("header-in_progress")
        );
        assert_eq!(dto.succeeded.header.as_deref(), Some("header-succeeded"));
        assert_eq!(dto.failed.header.as_deref(), Some("header-failed"));
        assert_eq!(dto.error.header.as_deref(), Some("header-error"));
        assert_eq!(dto.skipped.header.as_deref(), Some("header-skipped"));

        assert_eq!(ScheduledRunSlackTemplates::from(dto), templates);
    }

    /// **The seven-adjacent-bools test the reviewer asked for.** Alternating
    /// `true`/`false` so that every *neighbouring* pair of the seven differs —
    /// a transposition between any two adjacent bool fields flips at least one
    /// of them relative to the fixture, which a same-value fixture (all
    /// `true`, say) would not catch.
    #[test]
    fn the_notification_config_conversion_maps_each_field_to_its_own_name() {
        let config = NotificationConfig {
            slack_webhook_credstore_ref: "cred://slack-hook".to_owned(),
            slack_channel: "#qa-alerts".to_owned(),
            manager_ui_base_url: "https://qa.example.com".to_owned(),
            slack_enabled: true,
            notify_on_failure: false,
            notify_on_success: true,
            notify_on_schedule_completion: false,
            scheduled_run_slack_enabled: true,
            scheduled_run_slack_templates: ScheduledRunSlackTemplates {
                pending: sample_template("pending"),
                in_progress: sample_template("in_progress"),
                succeeded: sample_template("succeeded"),
                failed: sample_template("failed"),
                error: sample_template("error"),
                skipped: sample_template("skipped"),
            },
            run_queue_queued_slack_enabled: false,
            email_smtp_host: "smtp.example.com".to_owned(),
            email_smtp_port: 2525,
            email_from: "qa@example.com".to_owned(),
            email_recipients: "a@example.com, b@example.com".to_owned(),
            email_enabled: true,
        };

        let dto = NotificationConfigDto::from(config.clone());
        assert_eq!(dto.slack_webhook_credstore_ref, "cred://slack-hook");
        assert_eq!(dto.slack_channel, "#qa-alerts");
        assert_eq!(dto.manager_ui_base_url, "https://qa.example.com");
        assert!(dto.slack_enabled);
        assert!(!dto.notify_on_failure);
        assert!(dto.notify_on_success);
        assert!(!dto.notify_on_schedule_completion);
        assert!(dto.scheduled_run_slack_enabled);
        assert!(!dto.run_queue_queued_slack_enabled);
        assert_eq!(dto.email_smtp_host, "smtp.example.com");
        assert_eq!(dto.email_smtp_port, 2525);
        assert_eq!(dto.email_from, "qa@example.com");
        assert_eq!(dto.email_recipients, "a@example.com, b@example.com");
        assert!(dto.email_enabled);
        assert_eq!(
            dto.scheduled_run_slack_templates.failed.header.as_deref(),
            Some("header-failed")
        );

        assert_eq!(NotificationConfig::from(dto), config);
    }

    /// `run_id` is `Option<Uuid>` and every other field is a distinct
    /// `String`/`Uuid` — asserted on the serialized JSON, [`the_test_result_conversion_maps_each_field_to_its_own_name`]'s
    /// reason.
    #[test]
    fn the_notification_log_entry_conversion_maps_each_field_to_its_own_name() {
        let dto = NotificationLogEntryDto::from(NotificationLogEntry {
            id: Uuid::from_u128(1),
            created_at: datetime!(2026-08-18 10:30:00 UTC),
            run_id: Some(Uuid::from_u128(2)),
            channel: "slack".to_owned(),
            event_type: "failed".to_owned(),
            outcome: "sent".to_owned(),
            detail: "Run-completed notification".to_owned(),
        });

        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["id"], Uuid::from_u128(1).to_string());
        assert_eq!(json["run_id"], Uuid::from_u128(2).to_string());
        assert_eq!(json["channel"], "slack");
        assert_eq!(json["event_type"], "failed");
        assert_eq!(json["outcome"], "sent");
        assert_eq!(json["detail"], "Run-completed notification");
    }

    /// A log entry belonging to no run reads back `null`, not a zero `Uuid` —
    /// the same divergence `qa_insights_sdk::NotificationLogEntry`'s own doc
    /// states.
    #[test]
    fn a_notification_log_entrys_absent_run_id_is_null_not_absent() {
        let dto = NotificationLogEntryDto::from(NotificationLogEntry {
            id: Uuid::from_u128(1),
            created_at: datetime!(2026-08-18 10:30:00 UTC),
            run_id: None,
            channel: "email".to_owned(),
            event_type: String::new(),
            outcome: "failed".to_owned(),
            detail: "smtp unreachable".to_owned(),
        });

        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["run_id"], serde_json::Value::Null, "{json}");
    }

    /// Every field of the preview response distinct, including the one
    /// non-scalar (`blocks`).
    #[test]
    fn the_notification_preview_conversion_maps_each_field_to_its_own_name() {
        let dto = NotificationPreviewDto::from(ScheduledRunPreview {
            event: "failed".to_owned(),
            event_label: "Failed".to_owned(),
            rendered_message: "the rendered body".to_owned(),
            fallback_text: "the fallback text".to_owned(),
            blocks: vec![crate::domain::ports::SlackBlock::Section {
                text: "the block text".to_owned(),
            }],
        });

        assert_eq!(dto.event, "failed");
        assert_eq!(dto.event_label, "Failed");
        assert_eq!(dto.rendered_message, "the rendered body");
        assert_eq!(dto.fallback_text, "the fallback text");
        assert_eq!(
            dto.blocks,
            vec![serde_json::json!({
                "type": "section",
                "text": { "type": "mrkdwn", "text": "the block text" }
            })]
        );
    }
}
