//! REST DTOs (serde + utoipa) for the `qa-runs` gear, and the validation the
//! boundary owes before a request reaches a service.
//!
//! These types never leak into the SDK or the domain layer: `qa_runs_sdk`
//! carries no serde/utoipa (contract-layer purity) and the domain layer speaks
//! those SDK models plus `DomainError`. The conversions here are the only
//! bridge.
//!
//! # Tri-state fields are two-state on the wire
//!
//! `serde_with` is not a workspace dependency, so a patch-style field cannot
//! distinguish an explicit JSON `null` from an absent key: both deserialize to
//! `None`. Every `Option` below therefore means **absent-or-null**, and no DTO
//! here offers a "clear this back to unset" operation.
//!
//! One field would otherwise be affected and is not:
//! [`LaunchRunReq::exclusive`] is genuinely tri-state in the domain
//! (`None` = inherit, which is not `Some(false)` - guide lines 35-45), and it
//! survives intact because "absent" and "null" *mean the same thing here*:
//! inherit. The distinction `serde_with` would buy has no meaning to encode.

use time::OffsetDateTime;
use uuid::Uuid;

use toolkit_odata::ODataQuery;

use qa_runs_sdk as sdk;

use crate::domain::error::DomainError;
use crate::domain::repos::{RunWithResult, TestResultRow};
use crate::domain::timeout::MIN_LAUNCH_TIMEOUT_SECONDS;

// ===========================================================================
// Boundary limits
// ===========================================================================

/// The configured ceilings the boundary validates against.
///
/// A struct rather than a bare `u64` so a second limit cannot be added as a
/// positional parameter next to the first, and so the handler that builds it
/// reads as passing *the gear's configuration* rather than a number.
#[derive(Clone, Copy, Debug)]
pub struct BoundaryLimits {
    /// [`QaRunsConfig::effective_max_timeout_seconds`] - already reconciled
    /// with `domain::timeout`'s hard fail-safe, so this value is the one a
    /// rejection message may quote and the one that is actually enforced.
    ///
    /// [`QaRunsConfig::effective_max_timeout_seconds`]:
    ///     crate::config::QaRunsConfig::effective_max_timeout_seconds
    pub max_timeout_seconds: u64,
}

impl From<&crate::config::QaRunsConfig> for BoundaryLimits {
    /// **The one safe way to build this**, and the reason it is a `From` impl
    /// rather than a convention.
    ///
    /// There are two timeout ceilings - the operator's knob and
    /// `domain::timeout`'s hard fail-safe - and `effective_max_timeout_seconds`
    /// is where they are reconciled. Both are spelled `max_timeout_seconds`, so
    /// the wrong wiring reads naturally, compiles, and silently reinstates
    /// exactly the disagreement the accessor exists to remove:
    ///
    /// ```ignore
    /// BoundaryLimits { max_timeout_seconds: cfg.max_timeout_seconds }  // raw: wrong
    /// ```
    ///
    /// A caller that writes `cfg.into()` cannot make that mistake. The field
    /// stays `pub` so tests can build a strict ceiling directly.
    fn from(cfg: &crate::config::QaRunsConfig) -> Self {
        Self {
            max_timeout_seconds: cfg.effective_max_timeout_seconds(),
        }
    }
}

/// Longest `branch` this boundary accepts, from the column it lands in:
/// `qa_runs.test_version VARCHAR(512)`
/// (`infra::storage::migrations::m20260813_000003_initial`).
///
/// The launch path records the resolved branch as the run's test version, so an
/// over-long branch is not a business-rule violation - it is a value that
/// cannot be stored. Without this check the failure surfaces as a driver error
/// from the `INSERT`, which becomes `DomainError::Database` and therefore an
/// opaque 500: a caller-fixable mistake reported as a server fault, with the
/// offending field named nowhere.
///
/// Measured in **bytes**, because that is what the column width is and what
/// `str::len` compares against. A branch of 512 multi-byte characters is
/// refused, correctly - it would not fit.
/// Longest `path` or `test_file` this boundary accepts, from the columns they
/// land in: `qa_runs.target_path` and `qa_runs.target_test_file`, both
/// `VARCHAR(1024)`.
///
/// The same argument as [`MAX_BRANCH_LEN`], one column across. Added in the fix
/// round after the branch check shipped alone: an over-long path is a
/// caller-fixable mistake that would otherwise surface as a Postgres 22001, and
/// therefore as `DomainError::Database`, and therefore as an opaque 500 naming
/// no field at all.
///
/// **`SQLite` is blind to declared widths**, so no test in this crate's
/// database tier can observe the failure this prevents - `SQLite` stores an
/// over-long value happily and the divergence only appears on Postgres. That is
/// the same blindness `infra::storage::mapper` records for its own truncation,
/// and the same reason to fix it anyway: the tier that would catch it does not
/// exist here.
pub const MAX_TARGET_PATH_LEN: usize = 1024;
pub const MAX_BRANCH_LEN: usize = 512;

// ===========================================================================
// Shared value DTOs
// ===========================================================================

/// One `name=value` launch parameter.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct RunParameterDto {
    pub name: String,
    pub value: String,
}

impl From<sdk::RunParameter> for RunParameterDto {
    fn from(p: sdk::RunParameter) -> Self {
        Self {
            name: p.name,
            value: p.value,
        }
    }
}

impl From<RunParameterDto> for sdk::RunParameter {
    fn from(p: RunParameterDto) -> Self {
        Self {
            name: p.name,
            value: p.value,
        }
    }
}

/// What a launch targets, as a flat discriminated record.
///
/// A flat struct with a `kind` tag rather than a serde-tagged enum, mirroring
/// the storage shape (`qa_runs.run_kind` plus four nullable target columns) and
/// keeping the published `OpenAPI` schema a single object. The cost is that an
/// inconsistent combination is representable on the wire, so
/// [`TryFrom<RunTargetDto>`] is the thing that rejects it - see that impl for
/// the exact rule per kind.
///
/// **The field docs below are published**: `#[api_dto]` turns them into the
/// `OpenAPI` property descriptions, so a doc that overstates a constraint ships
/// to clients as a rule the server does not apply. Corrected 2026-08-15, where
/// `repo_id` said it "must be absent for `custom_plan`" while the impl - and the
/// test that pins it - accept and ignore it. Say what the server does.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct RunTargetDto {
    /// `plan`, `test`, `custom_plan` or `collect` - `sdk::RunKind::as_str`'s
    /// spellings.
    pub kind: String,
    /// Required for `plan`, `test` and `collect`. Ignored for `custom_plan`.
    pub repo_id: Option<Uuid>,
    /// Path of the `plan.yaml` within the repository content root. Required for
    /// `plan` and `test`. Ignored for the other kinds - a `collect` target names
    /// no plan, it enumerates whatever the branch holds.
    pub path: Option<String>,
    /// Required for `test`. Ignored for the other kinds.
    pub test_file: Option<String>,
    /// Required for `custom_plan`. Ignored for the other kinds.
    pub custom_plan_id: Option<Uuid>,
    /// Where a `collect` run's runner posts its per-file case counts, becoming
    /// `VHP_COLLECT_URL`. Required for `collect` - though an empty string is
    /// accepted and means "collect but report nowhere", which is the source
    /// system's own behaviour for a blank URL
    /// (`manager/src/services/argo.rs:52-58`). Ignored for the other kinds.
    pub collect_url: Option<String>,
}

impl From<sdk::RunTarget> for RunTargetDto {
    fn from(t: sdk::RunTarget) -> Self {
        let kind = t.kind().as_str().to_owned();
        match t {
            sdk::RunTarget::Plan { repo_id, path } => Self {
                kind,
                repo_id: Some(repo_id),
                path: Some(path),
                test_file: None,
                custom_plan_id: None,
                collect_url: None,
            },
            sdk::RunTarget::Test {
                repo_id,
                path,
                test_file,
            } => Self {
                kind,
                repo_id: Some(repo_id),
                path: Some(path),
                test_file: Some(test_file),
                custom_plan_id: None,
                collect_url: None,
            },
            sdk::RunTarget::CustomPlan { id } => Self {
                kind,
                repo_id: None,
                path: None,
                test_file: None,
                custom_plan_id: Some(id),
                collect_url: None,
            },
            sdk::RunTarget::Collect {
                repo_id,
                collect_url,
            } => Self {
                kind,
                repo_id: Some(repo_id),
                path: None,
                test_file: None,
                custom_plan_id: None,
                collect_url: Some(collect_url),
            },
        }
    }
}

impl TryFrom<RunTargetDto> for sdk::RunTarget {
    type Error = DomainError;

    /// # Errors
    ///
    /// [`DomainError::Validation`] naming `target.<field>` when `kind` is not
    /// one of the three spellings, or when a field the chosen kind requires is
    /// missing.
    ///
    /// **Extra fields are ignored rather than refused.** A `custom_plan` that
    /// also carries a `repo_id` is accepted and the `repo_id` is dropped. The
    /// alternative - refusing - reads stricter but converts a harmless client
    /// habit (sending the whole record back) into a failed launch, and nothing
    /// downstream can act on the surplus field.
    fn try_from(dto: RunTargetDto) -> Result<Self, Self::Error> {
        fn required<T>(value: Option<T>, field: &str, kind: &str) -> Result<T, DomainError> {
            value.ok_or_else(|| DomainError::Validation {
                field: format!("target.{field}"),
                message: format!("is required for a {kind} target"),
            })
        }

        /// The column-width check, applied to whichever text field the chosen
        /// kind actually stores. Bytes, for the reason [`MAX_BRANCH_LEN`] gives.
        fn within_column(value: String, field: &str) -> Result<String, DomainError> {
            if value.len() > MAX_TARGET_PATH_LEN {
                return Err(DomainError::Validation {
                    field: format!("target.{field}"),
                    message: format!(
                        "is {} bytes; the maximum is {MAX_TARGET_PATH_LEN}",
                        value.len()
                    ),
                });
            }
            Ok(value)
        }

        match dto.kind.as_str() {
            "plan" => Ok(Self::Plan {
                repo_id: required(dto.repo_id, "repo_id", "plan")?,
                path: within_column(required(dto.path, "path", "plan")?, "path")?,
            }),
            "test" => Ok(Self::Test {
                repo_id: required(dto.repo_id, "repo_id", "test")?,
                path: within_column(required(dto.path, "path", "test")?, "path")?,
                test_file: within_column(
                    required(dto.test_file, "test_file", "test")?,
                    "test_file",
                )?,
            }),
            "custom_plan" => Ok(Self::CustomPlan {
                id: required(dto.custom_plan_id, "custom_plan_id", "custom_plan")?,
            }),
            // Accepted here as well as rendered above, so this impl and its
            // `From` twin stay inverses: a run this API renders must be a run
            // this API can be handed back. The collect *trigger* belongs to
            // qa-insights and reaches `LaunchService` over the in-process SDK
            // rather than through this DTO, so nothing in this gear needs the
            // arm - but an arm that refuses what the other one produces is the
            // asymmetry `every_target_kind_round_trips` exists to catch.
            //
            // It grants no capability a caller did not already have:
            // `VHP_COLLECT_URL` is **not** in `domain::params::RESERVED_NAMES`
            // (see the SECURITY NOTE there), so a launch parameter could always
            // set it on any run.
            "collect" => Ok(Self::Collect {
                repo_id: required(dto.repo_id, "repo_id", "collect")?,
                collect_url: within_column(
                    required(dto.collect_url, "collect_url", "collect")?,
                    "collect_url",
                )?,
            }),
            other => Err(DomainError::Validation {
                field: "target.kind".to_owned(),
                message: format!("must be one of plan, test, custom_plan, collect; got '{other}'"),
            }),
        }
    }
}

// ===========================================================================
// Run responses
// ===========================================================================

/// A run as reported by the read endpoints.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct RunDto {
    pub id: Uuid,
    /// `{slug}-{n}`. Load-bearing beyond display: a queue row's `blocked_by`
    /// text names the run holding an environment.
    pub name: String,
    pub target: RunTargetDto,
    /// Renamed from `platform_id` (Task 25): the wire now agrees with the
    /// Rust field. The physical column is unmoved — it is still
    /// `platform_id`, and `infra::storage::entity::run::Model` still pins
    /// `#[sea_orm(column_name = "platform_id")]` on its own `environment_id`
    /// field — but that attribute is now the *only* bridge between two names
    /// instead of standing beside a second, wire-level one. The column
    /// itself waits for this plan's later expand/contract migrations (ruling
    /// B3); only the wire moved here. Every other `platform_id` on this
    /// crate's wire (requests and responses alike) was renamed the same way
    /// — this is the one place it is spelled out in full.
    ///
    /// **This was a breaking API change** (Task 25): a client reading
    /// `platform_id` out of a response now finds it absent, replaced by
    /// `environment_id`. (This type is a response - a client never *sends*
    /// one, so there is no 400 to raise here. The 400 for a stale *request*
    /// is on the request-side types: [`LaunchRunReq::environment_id`] and
    /// [`NewScheduleReq::environment_id`] both refuse a `platform_id` sent in
    /// their place explicitly - see the first one's doc, and ruling G-4.)
    pub environment_id: Option<Uuid>,
    /// Branch actually resolved and executed against.
    pub test_version: Option<String>,
    /// Environment application version snapshotted at launch, not re-derived.
    pub app_version: Option<String>,
    pub app_build: Option<String>,
    /// `sdk::RunState::as_str`'s spelling. Note `canceled`, one `l` - the queue
    /// row's equivalent state is spelled `cancelled`, and the difference is
    /// deliberate (see `sdk::RunState`).
    pub state: String,
    /// The exclusivity decision that was actually made - **not** the launch's
    /// request, which is a tri-state.
    pub resolved_exclusive: bool,
    /// Which tier supplied it: `launch`, `plan.yaml`, `test_meta`, `default`.
    pub exclusive_tier: String,
    pub is_validation: bool,
    pub parameters: Vec<RunParameterDto>,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    /// `manual` or `scheduled`.
    pub source: String,
    pub schedule_id: Option<Uuid>,
    pub bundle_ids: Vec<Uuid>,
    /// Opaque executor handle; `null` until dispatch succeeds.
    pub execution_ref: Option<String>,
    pub log_storage_ref: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub timeout_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// Terminal failure reason, operator-facing.
    ///
    /// This is the run's recorded text, which the domain layer has already put
    /// through `DomainError::recorded_text` where it originated in an error -
    /// so a cause that was not the caller's to see reads as
    /// `domain::error::OPAQUE_ERROR_TEXT` here. Nothing in this module
    /// re-redacts it, and nothing should: doing so would also blank the
    /// executor's own operator-facing message, which is the only description a
    /// failed run has.
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// The run's five outcome counters, notably `skipped`.
    ///
    /// Added on the run list DTO by Task 10, alongside the product owner's
    /// decision that a skipped test no longer fails a run
    /// (`domain::state_machine::derive_terminal_state`). A `Succeeded` run
    /// whose `skipped` is non-zero asserted less than the word "succeeded"
    /// implies, and the run list is exactly where that would otherwise go
    /// unnoticed — a caller scanning the list never reaches the per-run detail
    /// read that already carried this number. See `RunDetailDto`, which now
    /// inherits this field via its flattened `run` rather than repeating it.
    ///
    /// **Zero by construction when built from a bare `sdk::Run`, real when
    /// built from [`RunWithResult`].** A bare `sdk::Run` carries no counters —
    /// they live on the same row, but [`sdk::Run`] has no field for them — so
    /// a freshly-started launch response reads zero, which is correct: a run
    /// with no ingested results has no results yet. The run list populates
    /// the real value from [`RunWithResult`]; the run detail read sets it
    /// directly from `RunResultDto::from`.
    pub result: RunResultDto,
}

impl From<sdk::Run> for RunDto {
    fn from(r: sdk::Run) -> Self {
        Self {
            id: r.id,
            name: r.name,
            target: r.target.into(),
            environment_id: r.platform_id,
            test_version: r.test_version,
            app_version: r.app_version,
            app_build: r.app_build,
            state: r.state.as_str().to_owned(),
            resolved_exclusive: r.resolved_exclusive,
            exclusive_tier: r.exclusive_tier.as_str().to_owned(),
            is_validation: r.is_validation,
            parameters: r.parameters.into_iter().map(Into::into).collect(),
            include_tags: r.include_tags,
            exclude_tags: r.exclude_tags,
            source: r.source.as_str().to_owned(),
            schedule_id: r.schedule_id,
            bundle_ids: r.bundle_ids,
            execution_ref: r.execution_ref,
            log_storage_ref: r.log_storage_ref,
            timeout_at: r.timeout_at,
            started_at: r.started_at,
            finished_at: r.finished_at,
            error: r.error,
            created_at: r.created_at,
            updated_at: r.updated_at,
            result: RunResultDto::from(sdk::RunResult::default()),
        }
    }
}

/// The run list's read, run and result from the same row (Task 10). See
/// [`RunDto::result`] for why the pairing matters.
impl From<RunWithResult> for RunDto {
    fn from(item: RunWithResult) -> Self {
        Self {
            result: RunResultDto::from(item.result),
            ..Self::from(item.run)
        }
    }
}

/// A run's five outcome counters.
///
/// `failed` is the failed-**or-errored** count. The source system keeps
/// `FAILED` and `ERROR` as distinct per-test statuses and folds them together
/// in every aggregate that produces these numbers; `sdk::RunResult` explains
/// why a sixth counter would be a silent regression.
#[derive(Debug, Clone, Copy)]
#[toolkit_macros::api_dto(response)]
pub struct RunResultDto {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub in_progress: usize,
    pub total: usize,
}

impl From<sdk::RunResult> for RunResultDto {
    fn from(r: sdk::RunResult) -> Self {
        Self {
            passed: r.passed,
            failed: r.failed,
            skipped: r.skipped,
            in_progress: r.in_progress,
            total: r.total,
        }
    }
}

/// One per-test result row, nested inside [`RunDetailDto`].
///
/// The `Run` carries the name: qa-insights has its own `TestResultDto`, an
/// analytics projection across runs, and the two collided as `OpenAPI`
/// component schemas (same name, different fields) until this type was
/// renamed. Keep the `Run` prefix - dropping it back to `TestResultDto`
/// reintroduces that collision.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct RunTestResultDto {
    pub id: Uuid,
    pub run_id: Uuid,
    pub test_file: String,
    pub test_name: String,
    /// Open set, normalised but not enumerated - see
    /// `domain::repos::NewTestResult::status`.
    pub status: String,
    pub duration: Option<String>,
    pub launch_id: Option<String>,
    pub jira_key: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<TestResultRow> for RunTestResultDto {
    fn from(r: TestResultRow) -> Self {
        Self {
            id: r.id,
            run_id: r.run_id,
            test_file: r.test_file,
            test_name: r.test_name,
            status: r.status,
            duration: r.duration,
            launch_id: r.launch_id,
            jira_key: r.jira_key,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// The detail read: the run, its counters, and its per-test rows.
///
/// One response rather than three endpoints because the counters are derived
/// from the same rows the caller is about to render, and fetching them
/// separately would let a caller display totals that disagree with the list
/// beneath them.
///
/// **`result` lives on the flattened [`RunDto`], not here.** Task 10 moved it
/// there so the run list carries the same field — a caller building one view
/// model for both the list and the detail read finds `result` in the same
/// place either way. Before that move this struct had its own `result` field
/// beside `run`, which would now collide with `RunDto::result` under
/// `#[serde(flatten)]`: two `"result"` keys entering the same flattened
/// object. `get_run` sets [`RunDto::result`] explicitly, since the detail
/// handler already fetches it as a separate read.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct RunDetailDto {
    #[serde(flatten)]
    pub run: RunDto,
    pub test_results: Vec<RunTestResultDto>,
}

// ===========================================================================
// Queue responses
// ===========================================================================

/// A queue row as reported by the queue read.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct QueueEntryDto {
    pub id: Uuid,
    pub run_id: Uuid,
    /// Renamed from `platform_id` (Task 25) — see [`RunDto::environment_id`]'s
    /// doc for why.
    pub environment_id: Uuid,
    /// `plan`, `test`, or `custom_plan`.
    pub run_kind: String,
    /// `manual` or `scheduled`.
    pub source: String,
    pub exclusive: bool,
    /// One of the seven frozen queue-state names. Note `cancelled`, two `l`s.
    pub state: String,
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub enqueued_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub dispatched_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// 1-based position among this environment's `queued` rows, oldest first;
    /// `null` for any other state.
    ///
    /// Computed per request over the rows that request returned, so a
    /// truncating `limit` understates it - the environment-filtered call is
    /// the remedy, not a larger window.
    pub queue_position: Option<u32>,
    /// When the TTL sweep will expire this row. `null` unless `queued`, and
    /// `null` when `queue_ttl_seconds` is 0.
    #[serde(with = "time::serde::rfc3339::option")]
    pub ttl_expires_at: Option<OffsetDateTime>,
    /// Plain-text reason it has not started.
    pub blocked_by: Option<String>,
}

impl From<sdk::QueueEntry> for QueueEntryDto {
    fn from(q: sdk::QueueEntry) -> Self {
        Self {
            id: q.id,
            run_id: q.run_id,
            environment_id: q.platform_id,
            run_kind: q.run_kind.as_str().to_owned(),
            source: q.source.as_str().to_owned(),
            exclusive: q.exclusive,
            state: q.state.as_str().to_owned(),
            error: q.error,
            enqueued_at: q.enqueued_at,
            dispatched_at: q.dispatched_at,
            finished_at: q.finished_at,
            queue_position: q.queue_position,
            ttl_expires_at: q.ttl_expires_at,
            blocked_by: q.blocked_by,
        }
    }
}

// ===========================================================================
// Launch
// ===========================================================================

/// The 202 body: a launch that was admitted but has not started.
///
/// A distinct type from [`RunDto`] because there is no run *record* worth
/// returning yet and pretending otherwise would invite a client to poll fields
/// that are all null. The two ids are what a caller needs: one to follow the
/// run, one to cancel its place in the queue.
#[derive(Debug, Clone, Copy)]
#[toolkit_macros::api_dto(response)]
pub struct QueuedRunDto {
    pub run_id: Uuid,
    pub queue_id: Uuid,
}

/// The 200 body of a force start: the run that now holds the environment.
///
/// A body rather than a 204, unlike cancel. Force start *creates* a
/// consequence the caller needs a handle to - something is now running on an
/// environment an operator overrode the occupancy check for - whereas cancel
/// merely stops one they already named.
#[derive(Debug, Clone, Copy)]
#[toolkit_macros::api_dto(response)]
pub struct StartedRunDto {
    pub run_id: Uuid,
}

/// A launch request.
///
/// `source` and `schedule_id` are deliberately absent: every REST launch is
/// `manual` by definition, and a caller able to claim `scheduled` could forge
/// the provenance of a run. The schedule path constructs its own
/// `sdk::LaunchRequest`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct LaunchRunReq {
    pub target: RunTargetDto,
    /// `null` launches a run with no target environment. Such a run is never
    /// queued and never blocks anything. Renamed from `platform_id`
    /// (Task 25) — see [`RunDto::environment_id`]'s doc for why.
    pub environment_id: Option<Uuid>,
    /// Test-content branch. When absent, resolution falls back to the
    /// environment's `default_branch` and then the repository's.
    pub branch: Option<String>,
    #[serde(default)]
    pub include_tags: Vec<String>,
    #[serde(default)]
    pub exclude_tags: Vec<String>,
    #[serde(default)]
    pub parameters: Vec<RunParameterDto>,
    /// The launch exclusivity tier, and the one genuinely tri-state field here:
    /// absent means **inherit** from `plan.yaml`/`TEST_META`, which is not the
    /// same as `false`. See this module's header for why the missing
    /// `serde_with` costs nothing on this field.
    pub exclusive: Option<bool>,
    /// Per-run timeout override, seconds. Absent falls back to the plan's
    /// value, then the configured default.
    pub timeout_seconds: Option<u64>,
    // Trap for a client still sending the pre-Task-25 field name.
    //
    // `Some` means the request body named `platform_id` rather than
    // `environment_id`. `into_domain` refuses it with a 400 naming the
    // rename, rather than silently accepting it and launching a run
    // detached from any environment with `environment_id` left `None` - the
    // Critical-1 defect the Task 25 review found and ruling G-4 closes.
    // Not `#[serde(deny_unknown_fields)]`: that would be a blanket refusal
    // this type happens to tolerate safely, but the same blanket approach on
    // `QueueQuery` would break its deliberate tolerance of `OData`
    // `$`-parameters - this trap is written the same narrow way on every
    // renamed request DTO so the three stay consistent.
    //
    // A plain comment, not a doc comment, and `#[schema(ignore = true)]`
    // alongside it - NEW-I-1 of the Task 25 re-review: `pub(crate)` stops
    // other crates seeing this field, but does nothing to stop `utoipa`
    // deriving a `platform_id` schema property from it (and publishing this
    // comment as that property's description, back when it was a `///`).
    // Both halves are fixed here: the attribute hides the property, and the
    // demoted comment means there is nothing to publish even if the
    // attribute is ever dropped.
    #[serde(rename = "platform_id")]
    #[schema(ignore = true)]
    pub(crate) legacy_platform_id: Option<serde_json::Value>,
}

impl LaunchRunReq {
    /// Validate what the boundary owes, then build the domain request.
    ///
    /// # What is checked here and why it is here rather than downstream
    ///
    /// * **The pre-Task-25 field name.** The private `legacy_platform_id`
    ///   field being `Some` means the caller sent `platform_id`, and this is refused
    ///   before anything else - ruling G-4, closing Critical-1 of the Task 25
    ///   review, which found the field was otherwise silently ignored rather
    ///   than refused.
    /// * **`timeout_seconds` range.** `domain::timeout` clamps out-of-range
    ///   values as a fail-safe and its own doc says the boundary owes a 400
    ///   naming the ceiling instead: *"a caller who asks for a year should be
    ///   told the request was changed."* A silent clamp is the failure mode -
    ///   the caller believes they set a week and got a day.
    /// * **`branch` length.** It lands in `qa_runs.test_version VARCHAR(512)`;
    ///   see [`MAX_BRANCH_LEN`] for what happens without the check.
    /// * **target consistency**, via [`sdk::RunTarget`]'s `TryFrom`.
    /// * **the run kind is a submittable one** - see below.
    ///
    /// Everything else a launch needs checked - parameter names and values,
    /// the environment's tenancy, whether the plan resolves - needs I/O or the
    /// caller's scope and belongs to `service::launch`. This function performs
    /// no I/O, which is what makes it callable before any of that.
    ///
    /// # Why `collect` is refused *here* and not in the codec
    ///
    /// A collect run bypasses admission entirely (`service::launch`, and
    /// `manager/src/services/argo.rs:369-372`): no per-environment queue, no
    /// `queue_max_depth`, no `max_concurrent_runs`. Accepting one on the run
    /// submission endpoint would hand **any authenticated caller** an unbounded
    /// third path around both 429 causes the frozen guide enumerates (guide
    /// lines 240-242) - a starvation vector aimed at the very hourly cycle the
    /// bypass exists to protect.
    ///
    /// It is also a divergence from the source system, which is the deciding
    /// argument. Legacy has **no** path from a run-submission API to a collect
    /// launch: `run_collect_cycle` is internal
    /// (`manager/src/services/collect.rs:157-179`), and the Analytics button
    /// posts to `POST /api/analytics/collect` - a different endpoint on a
    /// different surface. Our REST surface should have none either.
    ///
    /// **The refusal is here rather than in [`TryFrom<RunTargetDto>`] on
    /// purpose.** That impl is the *codec*, shared with the read path: it is the
    /// inverse of `From<sdk::RunTarget>`, so a collect run this API renders must
    /// stay a value this API can parse back. Narrowing it would make the two
    /// halves disagree. The codec stays total; the **submission** refuses.
    /// `a_collect_target_is_refused_by_the_launch_boundary` and
    /// `every_target_kind_round_trips_through_the_wire_shape` are the pair that
    /// pins both halves - do not "simplify" them into one.
    ///
    /// The refusal keys on the decoded [`sdk::RunKind`], not on the request's
    /// `kind` string, so a second spelling could not slip past it.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`], which the error mapping renders as a 400
    /// naming the field.
    pub fn into_domain(self, limits: BoundaryLimits) -> Result<sdk::LaunchRequest, DomainError> {
        if self.legacy_platform_id.is_some() {
            return Err(DomainError::Validation {
                field: "platform_id".to_owned(),
                message: "was renamed to `environment_id` (Task 25); send `environment_id` \
                          instead of `platform_id`."
                    .to_owned(),
            });
        }

        if let Some(branch) = self.branch.as_deref()
            && branch.len() > MAX_BRANCH_LEN
        {
            return Err(DomainError::Validation {
                field: "branch".to_owned(),
                // Bytes, and the message says bytes: `MAX_BRANCH_LEN` is a
                // column width in bytes, and `.len()` is what compares against
                // it. Saying "characters" made a 200-character CJK branch refuse
                // with "is 600 characters", which is true of nothing the caller
                // typed.
                message: format!("is {} bytes; the maximum is {MAX_BRANCH_LEN}", branch.len()),
            });
        }

        if let Some(seconds) = self.timeout_seconds
            && !(MIN_LAUNCH_TIMEOUT_SECONDS..=limits.max_timeout_seconds).contains(&seconds)
        {
            return Err(DomainError::Validation {
                field: "timeout_seconds".to_owned(),
                message: format!(
                    "is {seconds}; it must be between {MIN_LAUNCH_TIMEOUT_SECONDS} and \
                     {} seconds. Omit the field for no per-run deadline.",
                    limits.max_timeout_seconds
                ),
            });
        }

        let target: sdk::RunTarget = self.target.try_into()?;
        if matches!(target.kind(), sdk::RunKind::Collect) {
            return Err(DomainError::Validation {
                field: "target.kind".to_owned(),
                // Deliberately NOT "unknown kind": the kind exists, it is
                // rendered by this API, and it is launched - just not from
                // here. A message that denied its existence would send a
                // reader hunting for a spelling mistake.
                message: "names a collect run, which bypasses admission and is launched by                           the collect trigger rather than by run submission. This endpoint                           accepts plan, test and custom_plan."
                    .to_owned(),
            });
        }

        Ok(sdk::LaunchRequest {
            target,
            platform_id: self.environment_id,
            branch: self.branch,
            include_tags: self.include_tags,
            exclude_tags: self.exclude_tags,
            parameters: self.parameters.into_iter().map(Into::into).collect(),
            exclusive: sdk::Exclusivity::from_option_bool(self.exclusive),
            timeout_seconds: self.timeout_seconds,
            // Not caller-supplied - see the type's doc.
            source: sdk::RunSource::Manual,
            schedule_id: None,
        })
    }
}

// ===========================================================================
// Schedules
// ===========================================================================

/// Longest `name` this boundary accepts, from the column it lands in:
/// `qa_schedules.name VARCHAR(255)`
/// (`infra::storage::migrations::m20260813_000004_schedules`).
///
/// The same argument as [`MAX_BRANCH_LEN`], one table across: a schedule's name
/// is caller-supplied, nothing between here and the `INSERT` measures it, and
/// without this check an over-long one surfaces as a Postgres 22001, therefore
/// as `DomainError::Database`, therefore as an opaque 500 naming no field. The
/// same `SQLite` blindness applies - the tier that would catch it does not
/// exist here.
///
/// **`cron` needs no counterpart.** `domain::cron::parse_cron` bounds its own
/// input to the width of `qa_schedules.cron`, and `ScheduleService::validate`
/// calls it on create *and* on update, so that column is already covered by a
/// check that is not this module's to duplicate.
pub const MAX_SCHEDULE_NAME_LEN: usize = 255;

/// A schedule's tri-state exclusivity choice, as the three tokens it is spelled
/// with on the wire.
///
/// # Three strings, not a nullable boolean
///
/// `"true"` forces exclusive, `"false"` forces parallel, `"auto"` inherits from
/// `plan.yaml` and then `TEST_META`. A nullable boolean would spell the third
/// as JSON `null`, and this module's header explains why that is exactly the
/// distinction the wire cannot be trusted to carry: absent and null are the
/// same value to serde without `serde_with`, so `null` would arrive
/// indistinguishable from a field the client forgot. The three-token form has no
/// absent case to confuse - a missing key is a deserialization failure, not a
/// silent `auto`.
///
/// It is also the vocabulary the source system stores
/// (`manager/src/services/exclusivity.rs`, `format_exclusive_annotation`), so an
/// operator reading a schedule here and the same schedule there sees the same
/// three words.
///
/// # This is a second home for the vocabulary, and the test is what pays for it
///
/// `infra::storage::mapper` keeps the storage codec's two halves adjacent
/// precisely so a change to the vocabulary is a one-file change, and a wire
/// codec is a second file. It is not merged into that one because the two
/// disagree about failure: an unrecognised value *from a column* is
/// `CorruptState` and an opaque 500 - the row is this gear's own and a fourth
/// spelling there means corruption - while an unrecognised value *from a
/// request* is the caller's mistake and owes a 400 naming the field.
///
/// What stops the two drifting is
/// `tests::the_wire_vocabulary_is_the_stored_vocabulary`, which compares this
/// encoder against `exclusive_choice_to_column` on all three inputs.
fn exclusive_choice_to_wire(choice: Option<bool>) -> &'static str {
    match choice {
        Some(true) => "true",
        Some(false) => "false",
        None => "auto",
    }
}

/// [`exclusive_choice_to_wire`]'s other half, failing with a 400 rather than a
/// 500 - see that function for why the storage codec is not reused.
///
/// # Errors
///
/// [`DomainError::Validation`] on any fourth spelling, naming the field and the
/// three values that are accepted.
fn exclusive_choice_from_wire(raw: &str) -> Result<Option<bool>, DomainError> {
    match raw {
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        "auto" => Ok(None),
        other => Err(DomainError::Validation {
            field: "exclusive_choice".to_owned(),
            message: format!("must be one of true, false, auto; got '{other}'"),
        }),
    }
}

/// A schedule as reported by the read endpoints.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ScheduleDto {
    pub id: Uuid,
    /// Unique within the tenant.
    pub name: String,
    pub target: RunTargetDto,
    /// `null` schedules a run with no target environment. Such a run is
    /// never queued and never blocks anything. Renamed from `platform_id`
    /// (Task 25) — see [`RunDto::environment_id`]'s doc for why.
    pub environment_id: Option<Uuid>,
    /// Branch each fire resolves against. `null` falls back to the
    /// environment's `default_branch` and then the repository's, at launch
    /// time.
    pub branch: Option<String>,
    /// Five-field cron expression, stored and returned as the operator wrote
    /// it.
    pub cron: String,
    /// `"true"`, `"false"` or `"auto"` - see [`exclusive_choice_to_wire`] for
    /// why this is a string rather than a nullable boolean.
    pub exclusive_choice: String,
    /// Whether the firing tick considers this schedule at all.
    pub enabled: bool,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub parameters: Vec<RunParameterDto>,
    /// Whether this schedule's runs notify Slack at all.
    ///
    /// **Read-only on this type's sibling request.** `NewScheduleReq` carries no
    /// notification fields, so a full replace cannot change them; `PUT
    /// /qa/v1/schedules/{id}/notifications` is their only writer. See
    /// [`UpdateScheduleNotificationsReq`].
    pub slack_notifications_enabled: bool,
    /// Channel override; `null` means the deployment-wide default channel.
    pub slack_channel: Option<String>,
    /// The events that notify, as the six lowercase tokens legacy serializes -
    /// `pending`, `in_progress`, `succeeded`, `failed`, `error`, `skipped`.
    pub slack_notification_events: Vec<String>,
    /// Latest due time this schedule has fired for; `null` if it never has.
    ///
    /// Server-owned. No request field sets it and an edit does not move it, so
    /// an operator rewriting a cron expression cannot rewind the cursor and
    /// re-fire the past.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_fired_tick: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<sdk::Schedule> for ScheduleDto {
    fn from(s: sdk::Schedule) -> Self {
        Self {
            id: s.id,
            name: s.name,
            target: s.target.into(),
            environment_id: s.platform_id,
            branch: s.branch,
            cron: s.cron,
            exclusive_choice: exclusive_choice_to_wire(s.exclusive_choice).to_owned(),
            enabled: s.enabled,
            include_tags: s.include_tags,
            exclude_tags: s.exclude_tags,
            parameters: s.parameters.into_iter().map(Into::into).collect(),
            slack_notifications_enabled: s.slack_notifications_enabled,
            slack_channel: s.slack_channel,
            slack_notification_events: s.slack_notification_events,
            last_fired_tick: s.last_fired_tick,
            created_at: s.created_at,
            updated_at: s.updated_at,
        }
    }
}

/// The body of both `POST /qa/v1/schedules` and `PUT /qa/v1/schedules/{id}`.
///
/// # One type for create and replace
///
/// The PUT is a **full replace** matching `sdk::NewSchedule`, so the two bodies
/// are the same body and a second near-identical type would be a place for them
/// to drift. The source system's edit endpoint takes its *create* form too
/// (`manager/src/routes/schedules.rs`, `api_update`), so this is also what a
/// ported caller already sends.
///
/// It is not a PATCH: `serde_with` is absent (see this module's header), so a
/// tri-state patch on [`Self::exclusive_choice`] - itself a tri-state - would
/// need `Option<Option<Option<bool>>>` to distinguish "leave it", "set it to
/// inherit" and "set it to parallel". A full replace with stated semantics is
/// the honest shape.
///
/// # Which fields are required, and what that does **not** buy
///
/// Two have no default: `enabled` and `exclusive_choice`. Both are choices with
/// no neutral value - there is no "no opinion" for a switch, and `auto` is a
/// third choice rather than the absence of one - so serde refuses a body that
/// omits either.
///
/// **That is not a general safety rule and must not be read as one.** An earlier
/// version of this paragraph stated it as *"a field whose absent reading differs
/// from its empty reading is required"*, which is true of every `Option` here
/// and implies a protection this type does not provide. On a **full replace**,
/// omitting a defaulted field changes the schedule silently:
///
/// * omitting `environment_id` **detaches an environment-targeted schedule**, and by
///   that field's own doc the runs it then fires are never queued and never
///   block anything - the same class of silent behaviour change as the disable
///   below, and arguably a wider one;
/// * omitting `parameters` wipes them, and omitting either tag list changes
///   which tests the schedule runs;
/// * omitting `branch` returns the schedule to launch-time resolution.
///
/// This is what a replace *means*, and it is deliberate: a PUT that could not
/// clear a field would need a separate "unset" spelling, which is the tri-state
/// problem this type avoids by not being a PATCH. **A caller must send the whole
/// record.** `tests::omitting_a_defaulted_field_clears_it_on_a_replace` pins the
/// behaviour so it stays a documented property rather than a surprise.
///
/// The two required fields are therefore not a safety net over that. They close
/// the one omission whose blast radius is the whole schedule rather than one of
/// its fields, and the next section is why that one earns the exception.
///
/// ## `enabled` is required, and that is the legacy quirk this port exists not
/// to repeat
///
/// `ScheduleService::update` preserves `enabled` by construction - it is a field
/// of `NewSchedule`, so a replace states it. That is only safe while the wire
/// makes the caller state it too: an `#[serde(default)]` here would turn every
/// edit that omits the field into a silent disable, and an
/// `#[serde(default = "…true")]` into a silent *re-enable* of a schedule an
/// operator had paused.
///
/// The source system has exactly the second failure, from the other direction:
/// its edit form carries no suspended flag at all
/// (`manager/src/models.rs`, `CreateScheduleForm`), so it edits by deleting the
/// `CronWorkflow`, recreating it, and re-suspending it by hand - with an error
/// message that has to tell the operator the schedule is now running when the
/// restore fails. Requiring the field is what makes that unrepresentable here
/// rather than merely unlikely.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct NewScheduleReq {
    /// Unique within the tenant; a name already taken answers 409.
    pub name: String,
    pub target: RunTargetDto,
    /// `null` schedules a run with no target environment. Renamed from
    /// `platform_id` (Task 25) — see [`RunDto::environment_id`]'s doc for why.
    pub environment_id: Option<Uuid>,
    /// `null` resolves the branch at launch time, as a manual launch does.
    pub branch: Option<String>,
    /// Five-field cron expression. An expression `domain::cron` refuses answers
    /// 400 here rather than being stored and silently never firing.
    pub cron: String,
    /// `"true"`, `"false"` or `"auto"`. **Required**, and a string rather than a
    /// nullable boolean - see [`exclusive_choice_to_wire`].
    pub exclusive_choice: String,
    /// **Required on create and on replace.** See this type's doc for why it has
    /// no default.
    pub enabled: bool,
    #[serde(default)]
    pub include_tags: Vec<String>,
    #[serde(default)]
    pub exclude_tags: Vec<String>,
    #[serde(default)]
    pub parameters: Vec<RunParameterDto>,
    // Trap for a client still sending the pre-Task-25 field name. See
    // `LaunchRunReq`'s own `legacy_platform_id` comment, which this mirrors;
    // the `TryFrom` below refuses a `Some` the same way `into_domain` does.
    // Plain comment and `#[schema(ignore = true)]`, not a doc comment, for
    // the same reason (NEW-I-1 of the Task 25 re-review): otherwise `utoipa`
    // publishes both a `platform_id` schema property and this text as its
    // description.
    #[serde(rename = "platform_id")]
    #[schema(ignore = true)]
    pub(crate) legacy_platform_id: Option<serde_json::Value>,
}

/// What [`NewScheduleReq`]'s conversion answers, and **deliberately not
/// convertible to a `CanonicalError`**.
///
/// # Defence in depth, not the guarantee
///
/// The guarantee that a schedule's field violation is attributed to the schedule
/// is
/// `api::rest::handlers::schedules::handler_tests::the_create_handler_attributes_a_service_refusal_to_the_schedule`,
/// which drives the real handler over a real `ConcreteAppServices`. This type is
/// a second line, and it closes a different half: the test catches the mistake
/// after it is made, and this stops one shape of it being made at all.
///
/// The shape: `let new = req.try_into()?;` in a handler returning `ApiResult<_>`.
/// With a plain `DomainError` that compiles, because the blanket
/// `From<DomainError> for CanonicalError` picks it up — and it silently
/// attributes a schedule's field to the *run*, which is the defect this endpoint
/// shipped. Wrapping the error in a type with no `Into<CanonicalError>` makes
/// that line **fail to build**, so the only way through is
/// `handlers::schedules::decode_payload`, which attributes it.
///
/// It costs nothing inside the conversion: `From<DomainError>` means the `?`
/// operator still works on the helpers that raise one, including
/// `RunTargetDto`'s `TryFrom`, which the launch path shares and which must keep
/// answering a plain `DomainError` there.
#[derive(Debug)]
pub struct ScheduleFieldError(DomainError);

impl From<DomainError> for ScheduleFieldError {
    fn from(e: DomainError) -> Self {
        Self(e)
    }
}

impl ScheduleFieldError {
    /// Unwrap for the one caller entitled to render it.
    #[must_use]
    pub fn into_domain(self) -> DomainError {
        self.0
    }
}

impl TryFrom<NewScheduleReq> for sdk::NewSchedule {
    /// **Not `DomainError`.** See [`ScheduleFieldError`] for what that buys.
    type Error = ScheduleFieldError;

    /// Decode the wire shape and apply the checks that are the boundary's.
    ///
    /// Only two things happen here that the service does not do: the
    /// [`NewScheduleReq::exclusive_choice`] token is decoded,
    /// and the two column widths this layer owns are enforced
    /// ([`MAX_SCHEDULE_NAME_LEN`], [`MAX_BRANCH_LEN`], plus
    /// [`MAX_TARGET_PATH_LEN`] through the target's own `TryFrom`). The empty
    /// name and the cron expression are `ScheduleService::validate`'s and are
    /// deliberately **not** repeated: two copies of a rule are two places for it
    /// to change.
    ///
    /// # Errors
    ///
    /// [`ScheduleFieldError`] wrapping a [`DomainError::Validation`], which the
    /// error mapping renders as a 400 naming the field.
    fn try_from(req: NewScheduleReq) -> Result<Self, Self::Error> {
        // The pre-Task-25 field name, refused before anything else - see
        // `LaunchRunReq::into_domain`'s matching check and ruling G-4.
        if req.legacy_platform_id.is_some() {
            return Err(ScheduleFieldError::from(DomainError::Validation {
                field: "platform_id".to_owned(),
                message: "was renamed to `environment_id` (Task 25); send `environment_id` \
                          instead of `platform_id`."
                    .to_owned(),
            }));
        }

        // **Measured on the trimmed name, but not trimmed here.**
        // `ScheduleService::normalize` owns the trim, because it is the one
        // function both entry points share - `QaRunsLocalClient` hands an
        // `sdk::NewSchedule` from an in-process caller straight to the service
        // and never passes through this DTO. Normalising here as well would put
        // one rule in two places, and the boundary is the copy that does not
        // cover every caller.
        //
        // The *measurement* is still the trimmed length, so padding a legal name
        // cannot push it over a column width it will not occupy.
        let name = req.name.trim();
        if name.len() > MAX_SCHEDULE_NAME_LEN {
            return Err(ScheduleFieldError::from(DomainError::Validation {
                field: "name".to_owned(),
                // Bytes, for the reason `LaunchRunReq::into_domain` gives about
                // the same message one field over.
                message: format!(
                    "is {} bytes; the maximum is {MAX_SCHEDULE_NAME_LEN}",
                    name.len()
                ),
            }));
        }
        if let Some(branch) = req.branch.as_deref()
            && branch.len() > MAX_BRANCH_LEN
        {
            return Err(ScheduleFieldError::from(DomainError::Validation {
                field: "branch".to_owned(),
                message: format!("is {} bytes; the maximum is {MAX_BRANCH_LEN}", branch.len()),
            }));
        }

        Ok(Self {
            // Untrimmed on purpose - see above; the service normalises.
            name: req.name,
            target: req.target.try_into()?,
            platform_id: req.environment_id,
            branch: req.branch,
            cron: req.cron,
            exclusive_choice: exclusive_choice_from_wire(&req.exclusive_choice)?,
            enabled: req.enabled,
            include_tags: req.include_tags,
            exclude_tags: req.exclude_tags,
            parameters: req.parameters.into_iter().map(Into::into).collect(),
        })
    }
}

/// The body of `PUT /qa/v1/schedules/{id}/notifications`.
///
/// # `PUT`, where the source system uses `POST`
///
/// Legacy registers this as `axum::routing::post`
/// (`manager/src/routes/mod.rs:95-98`). The divergence is deliberate and is
/// already recorded in the PRD and DESIGN, so it is cited here rather than
/// re-argued: the frozen contract for this subsystem is the **test-facing** one
/// (environment variables, `plan.yaml`, `TEST_META` -
/// `cpt-cf-qa-fr-migration-runner-contract`), and the REST surface explicitly is
/// not. This route already changes its prefix (`/api` -> `/qa/v1`) and its key
/// (`{name}` -> `{id}`); a full, idempotent replacement of a settings
/// sub-resource is a `PUT`, which is also what the sibling gears do.
///
/// # A replacement, so every field is required
///
/// All three are stated on every call - there is no partial edit here for the
/// same reason `NewScheduleReq` is not a `PATCH`. Legacy defaults `channel` and
/// `events` (`UpdateScheduleNotificationsForm`,
/// `manager/src/models.rs:291-299`), and its UI compensates by re-sending the
/// current values whenever it toggles the switch
/// (`manager-ui/src/pages/SchedulesPage.tsx:118-127`). Requiring them makes the
/// same outcome a property of the contract instead of the client's diligence.
///
/// # What it cannot touch
///
/// Everything else on the schedule. That is the half of legacy's handler that is
/// behaviour rather than annotation plumbing - *"this endpoint edits Slack
/// settings only, so a schedule pinned to exclusive (or to parallel) must come
/// back pinned the same way"* (`manager/src/routes/schedules.rs:854-856`).
///
/// # Where its two checks live
///
/// **Not here.** The channel width and the event vocabulary are both enforced by
/// `ScheduleService::validate_notifications`, unlike `name` and `branch`, whose
/// widths this module owns. That function's doc gives the reason: the in-process
/// caller is the point of this endpoint's contract, and it never constructs a
/// DTO.
///
/// The `slack_` prefix is kept against `clippy::struct_field_names`, for the
/// reason [`sdk::ScheduleNotificationSettings`] gives: `ScheduleDto::enabled`
/// already exists on the same aggregate and means whether the schedule fires,
/// and these are legacy's own field names.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[allow(clippy::struct_field_names)]
pub struct UpdateScheduleNotificationsReq {
    /// The master switch. **Required**: there is no neutral value for a switch,
    /// and defaulting it either way would let an omission silently start or stop
    /// notifying.
    pub slack_enabled: bool,
    /// `null` uses the deployment-wide default channel.
    pub slack_channel: Option<String>,
    /// The events that notify, as the six lowercase tokens legacy serializes:
    /// `pending`, `in_progress`, `succeeded`, `failed`, `error`, `skipped`.
    /// An empty list notifies on nothing; an unrecognised name is a 400.
    pub slack_events: Vec<String>,
}

impl From<UpdateScheduleNotificationsReq> for sdk::ScheduleNotificationSettings {
    /// Infallible: every check this payload owes is the service's, so there is
    /// nothing here that can fail. A `TryFrom` would advertise a refusal that
    /// does not exist.
    fn from(req: UpdateScheduleNotificationsReq) -> Self {
        Self {
            slack_enabled: req.slack_enabled,
            slack_channel: req.slack_channel,
            slack_events: req.slack_events,
        }
    }
}

// ===========================================================================
// Queue query
// ===========================================================================

/// Default `limit` on the queue read, and the ceiling it is clamped to
/// (frozen guide: *"`limit` defaults to 200 and is clamped to 1-500"*).
pub const QUEUE_LIMIT_DEFAULT: u32 = 200;
/// See [`QUEUE_LIMIT_DEFAULT`].
pub const QUEUE_LIMIT_MAX: u32 = 500;
/// See [`QUEUE_LIMIT_DEFAULT`]. A `limit` of `0` returns nothing useful and is
/// far more likely to be an uninitialised variable than a request for an empty
/// page, so the floor is 1 rather than 0.
pub const QUEUE_LIMIT_MIN: u32 = 1;

/// Query parameters for the queue read.
#[derive(Debug, Clone, Default)]
#[toolkit_macros::api_dto(request)]
pub struct QueueQuery {
    /// Absent spans every environment in the same window — the same behaviour
    /// the source system has when its own `platform_id` is absent. **Both
    /// nouns are deliberate**: this gear's aggregate is the environment, the
    /// source system's is the platform, and an earlier edit renamed only the
    /// first half of this sentence, leaving "as the source system's does" to
    /// claim legacy spans every *environment* — a concept it does not have.
    /// `domain::repos::queue_repo`'s sibling doc keeps "platform" on both
    /// sides because it describes the legacy behaviour throughout.
    ///
    /// Renamed from `platform_id` (Task 25) — see
    /// [`RunDto::environment_id`]'s doc for why.
    pub environment_id: Option<Uuid>,
    /// Absent means [`QUEUE_LIMIT_DEFAULT`]. Out-of-range values are **clamped,
    /// not refused** - see [`Self::effective_limit`].
    pub limit: Option<u32>,
    // Trap for a client still sending the pre-Task-25 field name. See
    // `LaunchRunReq`'s own `legacy_platform_id` comment, which this mirrors -
    // `reject_legacy_field` is the check `list_queue` runs before using
    // `environment_id`. Not `#[serde(deny_unknown_fields)]` **on this type
    // specifically**: this query deliberately tolerates `OData`
    // `$`-parameters
    // (`the_queue_query_survives_absent_fields_and_ignores_odata_parameters`),
    // and a blanket denial would refuse those too.
    //
    // `QueueQuery` is never referenced as a request-body schema (this
    // endpoint declares its query parameters individually via
    // `.query_param`/`.query_param_typed`, not from this type's `ToSchema`),
    // so `#[schema(ignore = true)]` is not load-bearing the way it is on
    // `LaunchRunReq`/`NewScheduleReq` - added anyway, for the same reason the
    // doc comment is plain here too: so nothing changes if that ever stops
    // being true.
    #[serde(rename = "platform_id")]
    #[schema(ignore = true)]
    pub(crate) legacy_platform_id: Option<serde_json::Value>,
}

impl QueueQuery {
    /// Refuses the request if it still names the pre-Task-25 field
    /// `platform_id` instead of [`Self::environment_id`].
    ///
    /// Silently accepting it and leaving [`Self::environment_id`] at `None`
    /// was Critical-1 of the Task 25 review: `GET /qa/v1/queue?platform_id=X`
    /// returned the whole cluster's queue rather than one environment's, with
    /// no signal to the caller that their filter was dropped. Ruling G-4
    /// closes it.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming `platform_id`, which the error
    /// mapping renders as a 400.
    pub fn reject_legacy_field(&self) -> Result<(), DomainError> {
        if self.legacy_platform_id.is_some() {
            return Err(DomainError::Validation {
                field: "platform_id".to_owned(),
                message: "was renamed to `environment_id` (Task 25); send `environment_id` \
                          instead of `platform_id`."
                    .to_owned(),
            });
        }
        Ok(())
    }

    /// The window this query actually reads.
    ///
    /// Clamping rather than rejecting, which is the opposite of what
    /// [`LaunchRunReq::into_domain`] does with `timeout_seconds`, so the
    /// difference is worth stating. The guide specifies clamping for this
    /// parameter in as many words, and the two cases are not alike: a clamped
    /// `limit` changes how much of an answer a caller sees, whereas a clamped
    /// timeout silently changes when their run is killed.
    ///
    /// **A clamped `limit` is not visible in the response either**, and an
    /// earlier version of this doc claimed a caller could "see that it was
    /// clamped by counting the rows". They cannot: asking for 5000 and getting
    /// 500 is indistinguishable from asking for 5000 and there being 500 - which
    /// is verbatim the argument `domain::repos::Windowed` exists to make about a
    /// silent ceiling. What makes it tolerable here is the page's
    /// `next_cursor`, which is present exactly when there is more, so a caller
    /// paging correctly is never misled; a caller reading one page and stopping
    /// is.
    #[must_use]
    pub fn effective_limit(&self) -> u32 {
        self.limit
            .unwrap_or(QUEUE_LIMIT_DEFAULT)
            .clamp(QUEUE_LIMIT_MIN, QUEUE_LIMIT_MAX)
    }

    /// Fold the legacy `limit` parameter into an `OData` query.
    ///
    /// Two spellings of "how many rows" reach this endpoint: `limit`, which the
    /// frozen guide specifies and which every caller ported from the source
    /// system sends, and `$top`, which is what the rest of this platform's
    /// paginated collections use and what a pagination cursor round-trips.
    ///
    /// **`$top` wins when both are present.** It is the one tied to the cursor,
    /// so honouring `limit` over it would let a caller's page size disagree with
    /// the page size their `next_cursor` was minted for. `limit` fills in only
    /// when `$top` is absent, which is exactly the ported-caller case it exists
    /// for.
    ///
    /// The clamp is applied here as well as by `LimitCfg` inside the
    /// repository. That is deliberate belt-and-braces of a specific kind: this
    /// one is the *guide's* contract on the `limit` parameter (defaults 200,
    /// clamped 1-500), while `LimitCfg` is the repository's own floor and
    /// ceiling on any page.
    ///
    /// **They are pinned separately, and they have to be.** These constants
    /// govern only this parameter: a request sending neither `limit` nor `$top`
    /// takes its page size from `PAGE_LIMITS`, and a `$top` is clamped solely by
    /// it. An earlier version of this paragraph said "if one moves, the failing
    /// test says which contract changed" - no test would have failed, because
    /// `PAGE_LIMITS` had none. It does now
    /// (`infra::storage::db::tests::the_page_limits_are_the_literals_the_guide_freezes`).
    pub fn merge_into(&self, mut query: ODataQuery) -> ODataQuery {
        if query.limit.is_none() && self.limit.is_some() {
            query.limit = Some(u64::from(self.effective_limit()));
        }
        query
    }
}

// ===========================================================================
// SSE
// ===========================================================================

/// One live log line, as an SSE `data:` payload.
///
/// A struct rather than a bare string so the stream can carry a gap marker or
/// any later per-line metadata without a second event type: a client parsing
/// `{"line": "..."}` today keeps parsing when a field is added beside it.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct RunLogLineDto {
    pub line: String,
}

impl From<String> for RunLogLineDto {
    fn from(line: String) -> Self {
        Self { line }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BoundaryLimits, LaunchRunReq, MAX_BRANCH_LEN, MAX_SCHEDULE_NAME_LEN, MAX_TARGET_PATH_LEN,
        NewScheduleReq, OffsetDateTime, QUEUE_LIMIT_DEFAULT, QUEUE_LIMIT_MAX, QUEUE_LIMIT_MIN,
        QueueEntryDto, QueueQuery, RunDetailDto, RunDto, RunTargetDto, ScheduleDto,
        exclusive_choice_from_wire, exclusive_choice_to_wire, sdk,
    };
    use crate::domain::error::DomainError;
    use crate::domain::timeout::{MAX_LAUNCH_TIMEOUT_SECONDS, MIN_LAUNCH_TIMEOUT_SECONDS};
    use toolkit_odata::ODataQuery;
    use uuid::Uuid;

    fn limits() -> BoundaryLimits {
        BoundaryLimits {
            max_timeout_seconds: 86_400,
        }
    }

    fn plan_target() -> RunTargetDto {
        RunTargetDto {
            kind: "plan".to_owned(),
            repo_id: Some(Uuid::from_u128(0xA1)),
            path: Some("suites/smoke/plan.yaml".to_owned()),
            test_file: None,
            custom_plan_id: None,
            collect_url: None,
        }
    }

    fn request() -> LaunchRunReq {
        LaunchRunReq {
            target: plan_target(),
            environment_id: Some(Uuid::from_u128(0xB1)),
            branch: None,
            include_tags: vec![],
            exclude_tags: vec![],
            parameters: vec![],
            exclusive: None,
            timeout_seconds: None,
            legacy_platform_id: None,
        }
    }

    // -- target ------------------------------------------------------------

    /// Every kind's round trip, driven from the SDK side so the `kind` strings
    /// come from `RunKind::as_str` rather than from this test's opinion of
    /// them.
    ///
    /// **`collect` is in this list and must stay in it.** The codec is the
    /// inverse of `From<sdk::RunTarget>`, so a collect run this API *renders*
    /// has to parse back. That it may not be **submitted** is a different rule,
    /// enforced one layer up and pinned by
    /// `a_collect_target_is_refused_by_the_launch_boundary`. The two tests look
    /// contradictory and are not: this one is about the codec, that one about
    /// `POST /qa/v1/runs`. Merging them collapses a deliberate boundary.
    #[test]
    fn every_target_kind_round_trips_through_the_wire_shape() {
        for target in [
            sdk::RunTarget::Plan {
                repo_id: Uuid::from_u128(1),
                path: "p".to_owned(),
            },
            sdk::RunTarget::Test {
                repo_id: Uuid::from_u128(1),
                path: "p".to_owned(),
                test_file: "t.robot".to_owned(),
            },
            sdk::RunTarget::CustomPlan {
                id: Uuid::from_u128(2),
            },
            sdk::RunTarget::Collect {
                repo_id: Uuid::from_u128(1),
                collect_url: "https://insights.example/qa/v1/collect/r/main".to_owned(),
            },
            // A blank URL is legal and means "collect but report nowhere"
            // (`manager/src/services/argo.rs:52-58`). Included so the `TryFrom`
            // cannot start rejecting it as missing: `required` tests for
            // `None`, not for emptiness, and the two must stay different.
            sdk::RunTarget::Collect {
                repo_id: Uuid::from_u128(1),
                collect_url: String::new(),
            },
        ] {
            let dto = RunTargetDto::from(target.clone());
            assert_eq!(dto.kind, target.kind().as_str());
            let back = sdk::RunTarget::try_from(dto).expect("a rendered target must parse back");
            assert_eq!(back, target);
        }
    }

    /// `POST /qa/v1/runs` refuses a collect target.
    ///
    /// The capability withheld is not the `VHP_COLLECT_URL` variable — that is
    /// unreserved and a launch parameter could always set it — it is the
    /// **admission bypass**: no `max_concurrent_runs`, no `queue_max_depth`, no
    /// 429. Legacy exposes no such path from run submission
    /// (`manager/src/services/collect.rs:157-179` is internal; the Analytics
    /// button posts to `POST /api/analytics/collect`).
    ///
    /// Break-verified: removing the `matches!(.., RunKind::Collect)` guard in
    /// `into_domain` turns exactly this test red, and
    /// `every_target_kind_round_trips_through_the_wire_shape` stays green —
    /// which is the proof the two are testing different layers.
    #[test]
    fn a_collect_target_is_refused_by_the_launch_boundary() {
        let request = LaunchRunReq {
            target: RunTargetDto {
                kind: "collect".to_owned(),
                repo_id: Some(Uuid::from_u128(0xA1)),
                path: None,
                test_file: None,
                custom_plan_id: None,
                collect_url: Some("https://insights.example/qa/v1/collect/r/main".to_owned()),
            },
            environment_id: None,
            ..request()
        };

        let error = request
            .into_domain(limits())
            .expect_err("run submission must not launch a collect run");
        match error {
            DomainError::Validation { field, message } => {
                assert_eq!(field, "target.kind");
                // The message must not read as "this kind does not exist" — it
                // does exist, this API renders it, and a reader sent hunting
                // for a spelling mistake is a reader sent the wrong way.
                assert!(
                    message.contains("collect run"),
                    "the refusal must name what was refused: {message}"
                );
                assert!(
                    message.contains("bypasses admission"),
                    "the refusal must say why: {message}"
                );
            }
            other => panic!("expected a Validation on target.kind, got {other:?}"),
        }
    }

    /// The refusal is on the **kind**, not on a malformed request: a collect
    /// target that is missing its URL still fails on `collect_url` first, in
    /// the codec, so the two failures stay distinguishable.
    #[test]
    fn a_collect_target_missing_its_url_still_fails_in_the_codec() {
        let dto = RunTargetDto {
            kind: "collect".to_owned(),
            repo_id: Some(Uuid::from_u128(0xA1)),
            path: None,
            test_file: None,
            custom_plan_id: None,
            collect_url: None,
        };
        match sdk::RunTarget::try_from(dto) {
            Err(DomainError::Validation { field, .. }) => {
                assert_eq!(field, "target.collect_url");
            }
            other => panic!("expected a Validation on target.collect_url, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_target_kind_is_a_field_named_validation_error() {
        let dto = RunTargetDto {
            kind: "workflow".to_owned(),
            ..plan_target()
        };
        let error = sdk::RunTarget::try_from(dto).expect_err("an unknown kind must be refused");
        match error {
            DomainError::Validation { field, message } => {
                assert_eq!(field, "target.kind");
                assert!(message.contains("workflow"), "{message}");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// The combination the flat shape makes representable: a `test` target with
    /// no `test_file`. Without this check it reaches the launch path as a plan
    /// run, which executes a whole suite instead of one file.
    #[test]
    fn a_test_target_without_its_file_is_refused_not_silently_downgraded() {
        let dto = RunTargetDto {
            kind: "test".to_owned(),
            ..plan_target()
        };
        let error = sdk::RunTarget::try_from(dto).expect_err("a test needs its file");
        match error {
            DomainError::Validation { field, .. } => assert_eq!(field, "target.test_file"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn a_custom_plan_target_ignores_the_fields_its_kind_does_not_use() {
        let dto = RunTargetDto {
            kind: "custom_plan".to_owned(),
            repo_id: Some(Uuid::from_u128(9)),
            path: Some("ignored".to_owned()),
            test_file: Some("ignored.robot".to_owned()),
            custom_plan_id: Some(Uuid::from_u128(3)),
            collect_url: None,
        };
        assert_eq!(
            sdk::RunTarget::try_from(dto).unwrap(),
            sdk::RunTarget::CustomPlan {
                id: Uuid::from_u128(3)
            }
        );
    }

    // -- timeout -----------------------------------------------------------

    /// The whole point of doing this at the boundary: the caller is **told**,
    /// and the message names the ceiling so they can pick a legal value without
    /// reading the deployment's configuration.
    #[test]
    fn an_over_ceiling_timeout_is_refused_with_the_ceiling_in_the_message() {
        let over = limits().max_timeout_seconds + 1;
        let error = LaunchRunReq {
            timeout_seconds: Some(over),
            ..request()
        }
        .into_domain(limits())
        .expect_err("a timeout above the ceiling must be refused, not clamped");

        match error {
            DomainError::Validation { field, message } => {
                assert_eq!(field, "timeout_seconds");
                assert!(
                    message.contains(&limits().max_timeout_seconds.to_string()),
                    "the ceiling must be named: {message}"
                );
                assert!(message.contains(&over.to_string()), "{message}");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// `Some(0)` is not "no deadline" - it is a run killed by the next tick.
    /// `domain::timeout::MIN_LAUNCH_TIMEOUT_SECONDS` carries the argument.
    #[test]
    fn a_zero_timeout_is_refused_rather_than_read_as_no_deadline() {
        let error = LaunchRunReq {
            timeout_seconds: Some(0),
            ..request()
        }
        .into_domain(limits())
        .expect_err("zero is not a deadline");
        assert!(matches!(error, DomainError::Validation { .. }));
    }

    #[test]
    fn the_boundary_values_of_the_timeout_range_are_accepted() {
        for seconds in [MIN_LAUNCH_TIMEOUT_SECONDS, limits().max_timeout_seconds] {
            LaunchRunReq {
                timeout_seconds: Some(seconds),
                ..request()
            }
            .into_domain(limits())
            .unwrap_or_else(|e| panic!("{seconds} must be accepted, got {e:?}"));
        }
    }

    /// An absent timeout is not a rejected one: the fallback chain (plan, then
    /// the configured default) is what fills it in, and refusing here would
    /// make the field mandatory.
    #[test]
    fn an_absent_timeout_passes_through_untouched() {
        let request = request().into_domain(limits()).unwrap();
        assert_eq!(request.timeout_seconds, None);
    }

    /// **The bridge from configuration, and the mistake it removes.** Both the
    /// config field and the DTO field are called `max_timeout_seconds`, so the
    /// raw wiring type-checks and silently restores the two-ceilings
    /// disagreement. The `From` impl is what makes the right thing the easy
    /// thing; this pins that it really does go through the accessor.
    #[test]
    fn the_config_bridge_uses_the_reconciled_ceiling_not_the_raw_knob() {
        let generous = crate::config::QaRunsConfig {
            max_timeout_seconds: MAX_LAUNCH_TIMEOUT_SECONDS * 10,
            ..crate::config::QaRunsConfig::default()
        };
        let limits = BoundaryLimits::from(&generous);
        assert_eq!(limits.max_timeout_seconds, MAX_LAUNCH_TIMEOUT_SECONDS);
        assert_ne!(
            limits.max_timeout_seconds, generous.max_timeout_seconds,
            "the raw knob is exactly what must not be copied through"
        );

        // And a ceiling below the fail-safe passes through untouched.
        let strict = crate::config::QaRunsConfig {
            max_timeout_seconds: 60,
            ..crate::config::QaRunsConfig::default()
        };
        assert_eq!(BoundaryLimits::from(&strict).max_timeout_seconds, 60);
    }

    /// The configured ceiling is what the check uses, so a deployment that
    /// lowers it lowers what the boundary accepts. Pinned because the obvious
    /// wrong implementation - validating against the domain constant - would
    /// pass every other test in this module.
    #[test]
    fn the_ceiling_comes_from_configuration_not_from_the_domain_constant() {
        let strict = BoundaryLimits {
            max_timeout_seconds: 60,
        };
        assert!(
            LaunchRunReq {
                timeout_seconds: Some(120),
                ..request()
            }
            .into_domain(strict)
            .is_err(),
            "120s must be refused under a 60s configured ceiling"
        );
        const {
            assert!(
                MAX_LAUNCH_TIMEOUT_SECONDS > 120,
                "this test is only meaningful while the domain constant is the looser of the two"
            );
        }
    }

    // -- branch ------------------------------------------------------------

    /// The column is `VARCHAR(512)`, so 512 must be accepted and 513 refused -
    /// an off-by-one in the wrong direction turns a legal branch name into a
    /// 400, and in the other direction restores the 500 this check exists to
    /// prevent.
    #[test]
    fn the_branch_length_boundary_matches_the_column_it_lands_in() {
        LaunchRunReq {
            branch: Some("b".repeat(MAX_BRANCH_LEN)),
            ..request()
        }
        .into_domain(limits())
        .expect("a branch exactly at the column width must be accepted");

        let error = LaunchRunReq {
            branch: Some("b".repeat(MAX_BRANCH_LEN + 1)),
            ..request()
        }
        .into_domain(limits())
        .expect_err("one character over the column width must be refused");
        match error {
            DomainError::Validation { field, message } => {
                assert_eq!(field, "branch");
                assert!(message.contains(&MAX_BRANCH_LEN.to_string()), "{message}");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// The gap that reopened one column across from the branch check: an
    /// over-long path is a caller-fixable mistake, and without this it is a
    /// Postgres 22001 rendered as an opaque 500 naming no field.
    #[test]
    fn an_over_long_target_path_or_test_file_is_a_named_400() {
        let long = "p".repeat(MAX_TARGET_PATH_LEN + 1);

        let error = sdk::RunTarget::try_from(RunTargetDto {
            path: Some(long.clone()),
            ..plan_target()
        })
        .expect_err("a path past the column width must be refused");
        match error {
            DomainError::Validation { field, message } => {
                assert_eq!(field, "target.path");
                assert!(
                    message.contains(&MAX_TARGET_PATH_LEN.to_string()),
                    "{message}"
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }

        let error = sdk::RunTarget::try_from(RunTargetDto {
            kind: "test".to_owned(),
            test_file: Some(long),
            ..plan_target()
        })
        .expect_err("a test file past the column width must be refused");
        assert!(
            matches!(error, DomainError::Validation { ref field, .. } if field == "target.test_file"),
            "{error:?}"
        );

        // Exactly at the width fits.
        sdk::RunTarget::try_from(RunTargetDto {
            path: Some("p".repeat(MAX_TARGET_PATH_LEN)),
            ..plan_target()
        })
        .expect("a path exactly at the column width must be accepted");
    }

    // -- provenance --------------------------------------------------------

    /// A REST launch is always `manual`. If the wire shape ever grows a
    /// `source` field, a caller could claim a run was produced by a schedule.
    #[test]
    fn a_rest_launch_is_always_manual_and_carries_no_schedule() {
        let request = request().into_domain(limits()).unwrap();
        assert_eq!(request.source, sdk::RunSource::Manual);
        assert_eq!(request.schedule_id, None);
    }

    /// Absent `exclusive` must stay `Inherit`, not become `Shared`: `Shared`
    /// is the launch tier asserting parallel, which outranks a `plan.yaml` or
    /// `TEST_META` declaration and would run a destructive test beside another.
    #[test]
    fn an_absent_exclusive_stays_inherit_rather_than_becoming_parallel() {
        let request = request().into_domain(limits()).unwrap();
        assert_eq!(request.exclusive, sdk::Exclusivity::Inherit);
    }

    /// The inbound wire boundary for `LaunchRequest::exclusive`:
    /// `sdk::Exclusivity` carries no serde impl of its own — this crate's
    /// contract-purity rule is exactly what makes that type's doc name this
    /// DTO as one of the boundaries the guarantee is pinned at, by test,
    /// rather than by the type. `LaunchRunReq::exclusive` stays a plain
    /// `Option<bool>` and `into_domain`'s `Exclusivity::from_option_bool`
    /// call is what actually reads `null`/absent/`true`/`false` off the wire.
    ///
    /// **Inbound only, and deliberately so.** `LaunchRunReq` is a request DTO
    /// (`#[api_dto(request)]`) — no HTTP client here ever serializes a
    /// `LaunchRequest` back out, so there is no outbound direction to cover.
    ///
    /// Driven from JSON literals, not struct literals, so this pins the wire
    /// shape and not a proxy for it (the same reason
    /// `the_launch_request_deserialises_environment_id_from_the_wire` is
    /// JSON-driven). Absent is asserted separately from explicit `null`
    /// because they are two different wire shapes that must read identically
    /// — this module's header explains why `serde_with`'s absence makes that
    /// automatic rather than incidental for this one field.
    #[test]
    fn the_exclusive_tri_state_survives_every_inbound_wire_shape() {
        fn body_with(exclusive: Option<serde_json::Value>) -> serde_json::Value {
            let mut body = serde_json::json!({
                "target": {
                    "kind": "plan",
                    "repo_id": Uuid::from_u128(0xA1),
                    "path": "suites/smoke/plan.yaml",
                },
            });
            if let Some(exclusive) = exclusive {
                body["exclusive"] = exclusive;
            }
            body
        }

        for (wire, expected) in [
            (
                body_with(Some(serde_json::json!(null))),
                sdk::Exclusivity::Inherit,
            ),
            (
                body_with(Some(serde_json::json!(true))),
                sdk::Exclusivity::Exclusive,
            ),
            (
                body_with(Some(serde_json::json!(false))),
                sdk::Exclusivity::Shared,
            ),
            (body_with(None), sdk::Exclusivity::Inherit),
        ] {
            let req: LaunchRunReq = serde_json::from_value(wire).expect("must deserialize");
            let request = req.into_domain(limits()).unwrap();
            assert_eq!(request.exclusive, expected);
        }
    }

    // -- legacy field trap (ruling G-4) -------------------------------------

    /// `environment_id` really is read off the wire under its own name - the
    /// positive half of the pair below. Driven from a JSON literal rather
    /// than a struct literal, so it is the wire shape and not a proxy for it
    /// (Important-4 of the Task 25 review).
    #[test]
    fn the_launch_request_deserialises_environment_id_from_the_wire() {
        let body = serde_json::json!({
            "target": {
                "kind": "plan",
                "repo_id": Uuid::from_u128(0xA1),
                "path": "suites/smoke/plan.yaml",
            },
            "environment_id": Uuid::from_u128(0xB1),
        });
        let req: LaunchRunReq = serde_json::from_value(body).expect("must deserialize");
        assert_eq!(req.environment_id, Some(Uuid::from_u128(0xB1)));
    }

    /// **Critical-1 of the Task 25 review, closed.** A client still sending
    /// `platform_id` used to get a 200 with `environment_id` silently left
    /// `None` - the CHANGELOG claimed a 400 that did not happen. Driven from a
    /// JSON literal naming `platform_id`, not `environment_id`, so this is the
    /// wire shape a stale client actually sends.
    #[test]
    fn a_legacy_platform_id_in_the_launch_body_is_refused_not_silently_dropped() {
        let body = serde_json::json!({
            "target": {
                "kind": "plan",
                "repo_id": Uuid::from_u128(0xA1),
                "path": "suites/smoke/plan.yaml",
            },
            "platform_id": Uuid::from_u128(0xB1),
        });
        let req: LaunchRunReq =
            serde_json::from_value(body).expect("the legacy field name still parses");
        assert_eq!(
            req.environment_id, None,
            "the legacy field must not populate the new one"
        );

        let error = req
            .into_domain(limits())
            .expect_err("platform_id must be refused, not silently dropped");
        match error {
            DomainError::Validation { field, message } => {
                assert_eq!(field, "platform_id");
                assert!(
                    message.contains("environment_id"),
                    "the refusal must name the new field: {message}"
                );
            }
            other => panic!("expected a Validation on platform_id, got {other:?}"),
        }
    }

    /// **NEW-Minor-1 of the Task 25 re-review.** A request naming *both*
    /// fields is refused the same as one naming only the legacy field - the
    /// check is unconditional on `legacy_platform_id` being `Some`, not
    /// conditional on `environment_id` being absent, so a caller cannot pair
    /// the two and have the new field win silently.
    #[test]
    fn a_launch_body_naming_both_the_legacy_and_the_new_field_is_still_refused() {
        let body = serde_json::json!({
            "target": {
                "kind": "plan",
                "repo_id": Uuid::from_u128(0xA1),
                "path": "suites/smoke/plan.yaml",
            },
            "environment_id": Uuid::from_u128(0xB2),
            "platform_id": Uuid::from_u128(0xB1),
        });
        let req: LaunchRunReq =
            serde_json::from_value(body).expect("both field names still parse together");
        assert_eq!(req.environment_id, Some(Uuid::from_u128(0xB2)));

        let error = req
            .into_domain(limits())
            .expect_err("platform_id must be refused even when environment_id is also present");
        match error {
            DomainError::Validation { field, .. } => assert_eq!(field, "platform_id"),
            other => panic!("expected a Validation on platform_id, got {other:?}"),
        }
    }

    // -- wire shape --------------------------------------------------------

    /// `RunDetailDto` flattens its run, so the detail response is one object
    /// rather than an envelope. Two things are pinned: that the run's fields
    /// really do appear at the top level, and that a `ToSchema` can be built
    /// for the flattened type at all - `utoipa` renders `serde(flatten)` as a
    /// composition, and a type that serializes fine but cannot produce a schema
    /// fails at **route registration**, i.e. at boot, not here.
    #[test]
    fn the_detail_response_flattens_its_run_and_still_has_a_schema() {
        let run = sdk::Run {
            id: Uuid::from_u128(0x21),
            name: "smoke-1".to_owned(),
            target: sdk::RunTarget::CustomPlan {
                id: Uuid::from_u128(0x22),
            },
            platform_id: None,
            test_version: Some("main".to_owned()),
            app_version: None,
            app_build: None,
            state: sdk::RunState::Succeeded,
            resolved_exclusive: false,
            exclusive_tier: sdk::ExclusiveTier::Default,
            is_validation: false,
            parameters: vec![],
            include_tags: vec![],
            exclude_tags: vec![],
            source: sdk::RunSource::Manual,
            schedule_id: None,
            bundle_ids: vec![],
            execution_ref: None,
            log_storage_ref: None,
            timeout_at: None,
            started_at: None,
            finished_at: None,
            error: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        };
        let detail = RunDetailDto {
            run: RunDto::from(run),
            test_results: vec![],
        };

        let json = serde_json::to_value(&detail).expect("the detail response must serialize");
        assert_eq!(json["name"], "smoke-1", "the run is flattened, not nested");
        assert_eq!(json["state"], "succeeded", "as_str's spelling, not Debug's");
        assert_eq!(json["target"]["kind"], "custom_plan");
        assert_eq!(json["result"]["total"], 0);
        assert!(json.get("run").is_none(), "flattened means no `run` key");

        let schema = <RunDetailDto as utoipa::PartialSchema>::schema();
        let rendered = serde_json::to_value(&schema).expect("the schema must serialize");
        assert!(
            rendered.get("allOf").is_some(),
            "utoipa renders a flattened field as a composition; got {rendered}"
        );
    }

    /// **`RunDto` and `QueueEntryDto` serialize `environment_id`, never
    /// `platform_id`.**
    ///
    /// Important-4 of the Task 25 review: a struct-field read
    /// (`assert_eq!(dto.environment_id, ...)`) is a proxy for the wire shape,
    /// not the wire shape itself - `#[serde(rename_all = "snake_case")]`
    /// happens to make the two agree today, but a field-level
    /// `#[serde(rename = "platform_id")]` added later would flip the wire
    /// back to the pre-Task-25 name while every such struct-field assertion
    /// stayed green. Only a real `serde_json::to_value` renders the actual
    /// key, which is what is asserted here.
    #[test]
    fn run_and_queue_entry_dtos_serialise_environment_id_not_platform_id() {
        let run = sdk::Run {
            id: Uuid::from_u128(0x41),
            name: "smoke-2".to_owned(),
            target: sdk::RunTarget::CustomPlan {
                id: Uuid::from_u128(0x42),
            },
            platform_id: Some(Uuid::from_u128(0x43)),
            test_version: None,
            app_version: None,
            app_build: None,
            state: sdk::RunState::Succeeded,
            resolved_exclusive: false,
            exclusive_tier: sdk::ExclusiveTier::Default,
            is_validation: false,
            parameters: vec![],
            include_tags: vec![],
            exclude_tags: vec![],
            source: sdk::RunSource::Manual,
            schedule_id: None,
            bundle_ids: vec![],
            execution_ref: None,
            log_storage_ref: None,
            timeout_at: None,
            started_at: None,
            finished_at: None,
            error: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        };
        let run_json = serde_json::to_value(RunDto::from(run)).expect("a run must serialize");
        assert_eq!(
            run_json["environment_id"],
            Uuid::from_u128(0x43).to_string()
        );
        assert!(
            run_json.get("platform_id").is_none(),
            "the pre-Task-25 key must not reappear: {run_json}"
        );

        let entry = sdk::QueueEntry {
            id: Uuid::from_u128(0x51),
            run_id: Uuid::from_u128(0x52),
            platform_id: Uuid::from_u128(0x53),
            run_kind: sdk::RunKind::Plan,
            source: sdk::RunSource::Manual,
            exclusive: false,
            state: sdk::QueueState::Queued,
            error: None,
            enqueued_at: OffsetDateTime::UNIX_EPOCH,
            dispatched_at: None,
            finished_at: None,
            queue_position: Some(1),
            ttl_expires_at: None,
            blocked_by: None,
        };
        let entry_json =
            serde_json::to_value(QueueEntryDto::from(entry)).expect("a queue entry must serialize");
        assert_eq!(
            entry_json["environment_id"],
            Uuid::from_u128(0x53).to_string()
        );
        assert!(
            entry_json.get("platform_id").is_none(),
            "the pre-Task-25 key must not reappear: {entry_json}"
        );
    }

    // -- queue query -------------------------------------------------------

    /// `$top` wins over `limit`, and `limit` fills in when `$top` is absent.
    ///
    /// The precedence is not arbitrary: `$top` is the one a pagination cursor
    /// is minted for, so honouring `limit` over it would let a caller's page
    /// size disagree with the cursor they were handed.
    #[test]
    fn the_legacy_limit_fills_in_only_when_odata_did_not_ask() {
        let query = QueueQuery {
            environment_id: None,
            limit: Some(50),
            legacy_platform_id: None,
        };
        assert_eq!(query.merge_into(ODataQuery::new()).limit, Some(50));

        let with_top = ODataQuery::new().with_limit(10);
        assert_eq!(
            query.merge_into(with_top).limit,
            Some(10),
            "$top must win when both are given"
        );

        // No `limit` at all leaves the query alone, so the repository's own
        // default applies rather than this layer imposing one.
        assert_eq!(
            QueueQuery::default().merge_into(ODataQuery::new()).limit,
            None
        );
    }

    /// The query struct is filled by axum's `Query` extractor from a raw query
    /// string, so its two fields have to survive being absent, and the string
    /// also carries `OData`'s own parameters, which this struct must ignore
    /// rather than reject.
    ///
    /// Driven through `serde_urlencoded` - the encoding `Query` uses - rather
    /// than through the extractor itself, which would need a request. What that
    /// leaves unproven is the extractor's own wiring; what it does prove is the
    /// part that would silently break, which is the shape of this struct.
    #[test]
    fn the_queue_query_survives_absent_fields_and_ignores_odata_parameters() {
        let empty: QueueQuery =
            serde_urlencoded::from_str("").expect("an empty query string is valid");
        assert_eq!(empty.environment_id, None);
        assert_eq!(empty.limit, None);

        let mixed: QueueQuery =
            serde_urlencoded::from_str("limit=50&%24filter=state%20eq%20%27queued%27&%24top=10")
                .expect("OData parameters must be ignored, not rejected");
        assert_eq!(mixed.limit, Some(50));
        assert_eq!(mixed.environment_id, None);

        let scoped: QueueQuery =
            serde_urlencoded::from_str("environment_id=00000000-0000-0000-0000-0000000000b1")
                .expect("an environment filter parses");
        assert_eq!(scoped.environment_id, Some(Uuid::from_u128(0xB1)));
    }

    /// **Critical-1 of the Task 25 review, closed.** `GET
    /// /qa/v1/queue?platform_id=X` used to return the whole cluster's queue
    /// silently - `environment_id` stayed `None` and nothing told the caller
    /// their filter was dropped. Driven through `serde_urlencoded`, the same
    /// encoding the extractor uses, so this is the actual query string a
    /// stale client sends.
    #[test]
    fn a_legacy_platform_id_query_param_is_refused_not_silently_ignored() {
        let scoped: QueueQuery =
            serde_urlencoded::from_str("platform_id=00000000-0000-0000-0000-0000000000b1")
                .expect("the legacy parameter name still parses");
        assert_eq!(
            scoped.environment_id, None,
            "the legacy parameter must not populate the new field"
        );

        let error = scoped
            .reject_legacy_field()
            .expect_err("platform_id must be refused, not silently ignored");
        match error {
            DomainError::Validation { field, message } => {
                assert_eq!(field, "platform_id");
                assert!(
                    message.contains("environment_id"),
                    "the refusal must name the new field: {message}"
                );
            }
            other => panic!("expected a Validation on platform_id, got {other:?}"),
        }
    }

    /// **NEW-Minor-1 of the Task 25 re-review.** Naming both parameters is
    /// refused the same as naming only the legacy one - the check does not
    /// treat a present `environment_id` as license to ignore `platform_id`.
    #[test]
    fn a_queue_query_naming_both_the_legacy_and_the_new_parameter_is_still_refused() {
        let scoped: QueueQuery = serde_urlencoded::from_str(
            "environment_id=00000000-0000-0000-0000-0000000000b2&platform_id=00000000-0000-0000-0000-0000000000b1",
        )
        .expect("both parameter names still parse together");
        assert_eq!(scoped.environment_id, Some(Uuid::from_u128(0xB2)));

        let error = scoped
            .reject_legacy_field()
            .expect_err("platform_id must be refused even when environment_id is also present");
        match error {
            DomainError::Validation { field, .. } => assert_eq!(field, "platform_id"),
            other => panic!("expected a Validation on platform_id, got {other:?}"),
        }
    }

    /// The guide's clamp, applied to the legacy parameter on the way through.
    #[test]
    fn an_out_of_range_legacy_limit_is_clamped_before_it_reaches_the_query() {
        let huge = QueueQuery {
            environment_id: None,
            limit: Some(u32::MAX),
            legacy_platform_id: None,
        };
        assert_eq!(
            huge.merge_into(ODataQuery::new()).limit,
            Some(u64::from(QUEUE_LIMIT_MAX))
        );
    }

    /// The guide's numbers, as numbers.
    ///
    /// `the_queue_limit_defaults_and_clamps_as_the_guide_specifies` compares
    /// behaviour against the constants, so it stays green if a constant moves -
    /// 200 to 15, 500 to 9999 and 1 to 0 were all green mutants. The literals
    /// are the contract; this is where they are pinned.
    #[test]
    fn the_queue_limit_constants_are_the_literals_the_guide_freezes() {
        assert_eq!(QUEUE_LIMIT_DEFAULT, 200);
        assert_eq!(QUEUE_LIMIT_MIN, 1);
        assert_eq!(QUEUE_LIMIT_MAX, 500);
    }

    // -- schedules ---------------------------------------------------------

    /// A complete, valid `POST`/`PUT` body as JSON, so the tests below can
    /// remove or corrupt exactly one key.
    ///
    /// JSON rather than the struct, because what several of them are about is
    /// **deserialization** - a struct literal cannot express an absent field.
    fn schedule_body() -> serde_json::Value {
        serde_json::json!({
            "name": "nightly",
            "target": {
                "kind": "plan",
                "repo_id": Uuid::from_u128(0xA1),
                "path": "suites/smoke/plan.yaml",
            },
            "environment_id": null,
            "branch": null,
            "cron": "0 3 * * *",
            "exclusive_choice": "auto",
            "enabled": true,
        })
    }

    fn parse_schedule(body: &serde_json::Value) -> Result<sdk::NewSchedule, String> {
        let req: NewScheduleReq =
            serde_json::from_value(body.clone()).map_err(|e| format!("deserialize: {e}"))?;
        sdk::NewSchedule::try_from(req).map_err(|e| format!("convert: {e:?}"))
    }

    /// **Critical-1 of the Task 25 review, closed.** A `PUT
    /// /qa/v1/schedules/{id}` still sending `platform_id` used to get a 200
    /// with `environment_id` silently `None` - detaching an exclusive
    /// schedule from its environment without telling the caller, exactly the
    /// worst of the four silent-replace effects this type's own doc
    /// describes. Adds `platform_id` beside the fixture's `environment_id`,
    /// so this is what a client that has not migrated actually sends.
    #[test]
    fn a_legacy_platform_id_in_the_schedule_body_is_refused_not_silently_dropped() {
        let mut body = schedule_body();
        body["platform_id"] = serde_json::json!(Uuid::from_u128(0xB1));

        let error =
            parse_schedule(&body).expect_err("platform_id must be refused, not silently dropped");
        assert!(
            error.contains("platform_id"),
            "the refusal must name the legacy field: {error}"
        );
        assert!(
            error.contains("environment_id"),
            "the refusal must name the new field: {error}"
        );
    }

    /// **NEW-Minor-1 of the Task 25 re-review.** Naming both fields is
    /// refused the same as naming only the legacy one - a caller cannot pair
    /// `platform_id` with a stated `environment_id` and have the refusal
    /// stand down.
    #[test]
    fn a_schedule_body_naming_both_the_legacy_and_the_new_field_is_still_refused() {
        let mut body = schedule_body();
        body["environment_id"] = serde_json::json!(Uuid::from_u128(0xB2));
        body["platform_id"] = serde_json::json!(Uuid::from_u128(0xB1));

        let error = parse_schedule(&body)
            .expect_err("platform_id must be refused even when environment_id is also present");
        assert!(error.contains("platform_id"), "{error}");
    }

    /// **The wire vocabulary is the stored vocabulary**, pinned rather than
    /// assumed.
    ///
    /// `exclusive_choice_to_wire` is a second home for three tokens that
    /// `infra::storage::mapper::exclusive_choice_to_column` already owns, and
    /// that module's header is explicit that splitting a vocabulary across files
    /// is how the two spellings drift. This is what pays for the split: change
    /// either encoder alone and this goes red.
    ///
    /// **Both halves of both codecs, not just the encoders.** The first version
    /// compared `to_wire` against `to_column` and nothing else, which left the
    /// decoders pinned only transitively through their own round-trips - so the
    /// two could have disagreed about which *fourth* spelling to accept, and the
    /// accepting side would have been the one that stores a value the other
    /// cannot read back. The agreement asserted here is: same three tokens, and
    /// the same refusal of anything else.
    #[test]
    fn the_wire_vocabulary_is_the_stored_vocabulary() {
        use crate::infra::storage::mapper::{
            exclusive_choice_from_str, exclusive_choice_to_column,
        };

        for choice in [Some(true), Some(false), None] {
            let token = exclusive_choice_to_wire(choice);
            assert_eq!(
                token,
                exclusive_choice_to_column(choice),
                "the wire and the column must spell {choice:?} the same way"
            );
            // And each decoder reads the other's output back to the same value.
            let row = Uuid::from_u128(0x33);
            assert_eq!(exclusive_choice_from_wire(token).unwrap(), choice);
            assert_eq!(exclusive_choice_from_str(token, row).unwrap(), choice);
        }

        // Neither side accepts a fourth spelling the other refuses. They differ
        // only in *how* they refuse - a caller's 400 here, a `CorruptState` 500
        // there - which is the whole reason the two codecs are separate.
        for bad in ["", "TRUE", "yes", "auto ", "inherit", "1"] {
            let row = Uuid::from_u128(0x33);
            assert!(
                exclusive_choice_from_wire(bad).is_err(),
                "the wire decoder accepted {bad:?}, which the column decoder refuses"
            );
            assert!(
                exclusive_choice_from_str(bad, row).is_err(),
                "the column decoder accepted {bad:?}, which the wire decoder refuses"
            );
        }
    }

    /// The tri-state survives a round trip, and `None` is **written** as `auto`
    /// rather than omitted - the conflation of "inherit" with "parallel" is the
    /// one mistake the source system made and had to undo.
    #[test]
    fn the_exclusive_choice_tri_state_round_trips_through_the_wire() {
        for choice in [Some(true), Some(false), None] {
            let token = exclusive_choice_to_wire(choice);
            assert_eq!(
                exclusive_choice_from_wire(token).expect("a rendered token must parse back"),
                choice
            );
        }
        assert_eq!(exclusive_choice_to_wire(None), "auto");
    }

    /// A fourth spelling is the **caller's** mistake, so it is a 400 naming the
    /// field - not the `CorruptState` 500 the storage decoder answers, which is
    /// the whole reason the two codecs are separate.
    #[test]
    fn an_unknown_exclusive_choice_is_a_named_400_not_an_opaque_500() {
        for bad in ["", "TRUE", "yes", "null", "inherit"] {
            let error =
                exclusive_choice_from_wire(bad).expect_err("only the three tokens may be accepted");
            match error {
                DomainError::Validation { field, message } => {
                    assert_eq!(field, "exclusive_choice");
                    assert!(message.contains("auto"), "{message}");
                }
                other => panic!("expected Validation for {bad:?}, got {other:?}"),
            }
        }

        // And through the whole request conversion, so the field reaches the
        // caller by the name they sent.
        let mut body = schedule_body();
        body["exclusive_choice"] = serde_json::json!("maybe");
        let error = parse_schedule(&body).expect_err("a fourth spelling must be refused");
        assert!(error.contains("exclusive_choice"), "{error}");
    }

    /// **`enabled` has no default, and an edit that omits it is refused.**
    ///
    /// This is the legacy quirk the port exists not to repeat, stated as a wire
    /// property. `ScheduleService::update` preserves `enabled` across an edit by
    /// construction - but only because a `NewSchedule` always carries one, and
    /// this DTO is what decides whether the *caller* has to. An
    /// `#[serde(default)]` here would make every edit that forgets the field a
    /// silent disable, and a `default = true` would silently resume a schedule
    /// an operator had paused, which is verbatim what the source system does
    /// when its hand-rolled restore fails.
    ///
    /// Both directions are asserted, because a rule that only refuses absence
    /// is half a rule: a stated `false` must survive to the domain request
    /// unchanged.
    #[test]
    fn a_schedule_payload_must_state_enabled_and_a_stated_false_survives() {
        let mut missing = schedule_body();
        missing
            .as_object_mut()
            .expect("the fixture is an object")
            .remove("enabled");
        let error = parse_schedule(&missing)
            .expect_err("an absent `enabled` must be refused, never defaulted");
        assert!(
            error.contains("enabled"),
            "the refusal must name the missing field: {error}"
        );

        let mut paused = schedule_body();
        paused["enabled"] = serde_json::json!(false);
        let new = parse_schedule(&paused).expect("a stated `enabled` parses");
        assert!(
            !new.enabled,
            "a schedule an operator paused must stay paused across an edit"
        );

        assert!(
            parse_schedule(&schedule_body())
                .expect("the fixture parses")
                .enabled
        );
    }

    /// `exclusive_choice` is required for the same reason `enabled` is: it has
    /// three values and no neutral one, so an absent key cannot be read as any
    /// of them.
    #[test]
    fn a_schedule_payload_must_state_its_exclusive_choice() {
        let mut missing = schedule_body();
        missing
            .as_object_mut()
            .expect("the fixture is an object")
            .remove("exclusive_choice");
        let error = parse_schedule(&missing).expect_err("an absent choice must be refused");
        assert!(error.contains("exclusive_choice"), "{error}");
    }

    /// **Omitting a defaulted field clears it**, which is what a full replace
    /// means and is not a safety property.
    ///
    /// Pinned as behaviour rather than left implied, because the type's doc used
    /// to imply the opposite: it stated the required-field rule in a form that
    /// reads as "omission cannot change a schedule", and omission changes four
    /// things. `environment_id` is the one that matters most - a replace that drops
    /// it detaches an environment-targeted schedule, and the runs it then fires are
    /// never queued and never block anything.
    ///
    /// Driven as a real before/after, not as one parse: a body carrying every
    /// optional field, then the same body without them.
    #[test]
    fn omitting_a_defaulted_field_clears_it_on_a_replace() {
        let mut full = schedule_body();
        full["environment_id"] = serde_json::json!(Uuid::from_u128(0xB1));
        full["branch"] = serde_json::json!("release/9.0");
        full["include_tags"] = serde_json::json!(["smoke"]);
        full["exclude_tags"] = serde_json::json!(["slow"]);
        full["parameters"] = serde_json::json!([{"name": "K", "value": "v"}]);

        let stated = parse_schedule(&full).expect("a complete body parses");
        assert_eq!(stated.platform_id, Some(Uuid::from_u128(0xB1)));
        assert_eq!(stated.branch.as_deref(), Some("release/9.0"));
        assert_eq!(stated.include_tags, ["smoke"]);
        assert_eq!(stated.exclude_tags, ["slow"]);
        assert_eq!(stated.parameters.len(), 1);

        // The same schedule, replaced by a caller who sent only what they meant
        // to change. Every one of the five is gone.
        let cleared = parse_schedule(&schedule_body()).expect("the fixture parses");
        assert_eq!(cleared.platform_id, None, "a replace detaches the platform");
        assert_eq!(cleared.branch, None);
        assert!(cleared.include_tags.is_empty());
        assert!(cleared.exclude_tags.is_empty());
        assert!(cleared.parameters.is_empty());
    }

    /// The column widths this layer owns on a schedule, both boundaries.
    ///
    /// `qa_schedules.name` is `VARCHAR(255)` and `qa_schedules.branch` is
    /// `VARCHAR(512)`; without these an over-long value is a Postgres 22001,
    /// therefore a `Database` error, therefore an opaque 500 naming no field -
    /// and `SQLite` cannot reproduce any of it.
    #[test]
    fn the_schedule_length_boundaries_match_the_columns_they_land_in() {
        let mut at_the_width = schedule_body();
        at_the_width["name"] = serde_json::json!("n".repeat(MAX_SCHEDULE_NAME_LEN));
        at_the_width["branch"] = serde_json::json!("b".repeat(MAX_BRANCH_LEN));
        parse_schedule(&at_the_width).expect("exactly at each column width must be accepted");

        let mut long_name = schedule_body();
        long_name["name"] = serde_json::json!("n".repeat(MAX_SCHEDULE_NAME_LEN + 1));
        let error = parse_schedule(&long_name).expect_err("one byte over must be refused");
        assert!(error.contains("name"), "{error}");
        assert!(
            error.contains(&MAX_SCHEDULE_NAME_LEN.to_string()),
            "{error}"
        );

        let mut long_branch = schedule_body();
        long_branch["branch"] = serde_json::json!("b".repeat(MAX_BRANCH_LEN + 1));
        let error = parse_schedule(&long_branch).expect_err("one byte over must be refused");
        assert!(error.contains("branch"), "{error}");
    }

    /// Padding does not count against the column width.
    ///
    /// The *trim itself* is `ScheduleService::normalize`'s, not this layer's -
    /// see the conversion - so what is pinned here is only the measurement:
    /// a name that is legal once trimmed must not be refused for a width it
    /// will not occupy. `schedules_tests::a_padded_name_is_stored_trimmed_for_
    /// every_caller` is where the storage invariant lives, because it is the one
    /// that covers the in-process caller too.
    #[test]
    fn whitespace_does_not_count_against_the_name_column_width() {
        let mut padded_long = schedule_body();
        padded_long["name"] =
            serde_json::json!(format!("  {}  ", "n".repeat(MAX_SCHEDULE_NAME_LEN)));
        parse_schedule(&padded_long).expect("whitespace must not count against the column width");

        let mut genuinely_long = schedule_body();
        genuinely_long["name"] = serde_json::json!("n".repeat(MAX_SCHEDULE_NAME_LEN + 1));
        let error =
            parse_schedule(&genuinely_long).expect_err("one byte over must still be refused");
        assert!(error.contains("name"), "{error}");
    }

    /// The response shape: `exclusive_choice` is the **string** `"auto"`, never
    /// JSON `null`, and the target is flattened the same way a run's is.
    #[test]
    fn a_schedule_renders_its_tri_state_as_a_string_never_as_null() {
        let schedule = sdk::Schedule {
            id: Uuid::from_u128(0x31),
            name: "nightly".to_owned(),
            target: sdk::RunTarget::CustomPlan {
                id: Uuid::from_u128(0x32),
            },
            platform_id: Some(Uuid::from_u128(0x33)),
            branch: None,
            cron: "0 3 * * *".to_owned(),
            exclusive_choice: None,
            enabled: true,
            include_tags: vec![],
            exclude_tags: vec![],
            parameters: vec![],
            // Non-default, all three, so this rendering test also witnesses that
            // `From<sdk::Schedule>` copies them rather than filling in defaults
            // - the whole failure mode `ScheduleDto` has for a newly added
            // field, since `Default` would make every assertion below still
            // pass.
            slack_notifications_enabled: true,
            slack_channel: Some("#qa-alerts".to_owned()),
            slack_notification_events: vec!["failed".to_owned(), "in_progress".to_owned()],
            last_fired_tick: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        };

        let json = serde_json::to_value(ScheduleDto::from(schedule.clone()))
            .expect("a schedule must serialize");
        assert_eq!(
            json["exclusive_choice"], "auto",
            "the inherit case is written as a token, not as null: {json}"
        );
        assert!(
            !json["exclusive_choice"].is_null(),
            "null could not be told apart from a forgotten field"
        );
        assert_eq!(json["target"]["kind"], "custom_plan");
        // Important-4 of the Task 25 review: the wire key, not the struct
        // field - see `run_and_queue_entry_dtos_serialise_environment_id_not_platform_id`.
        assert_eq!(json["environment_id"], Uuid::from_u128(0x33).to_string());
        assert!(
            json.get("platform_id").is_none(),
            "the pre-Task-25 key must not reappear: {json}"
        );
        assert!(json["last_fired_tick"].is_null(), "{json}");
        assert_eq!(json["slack_notifications_enabled"], true, "{json}");
        assert_eq!(json["slack_channel"], "#qa-alerts", "{json}");
        assert_eq!(
            json["slack_notification_events"],
            serde_json::json!(["failed", "in_progress"]),
            "the events render as the serialized snake_case tokens, in the order \
             the operator gave them: {json}"
        );

        let forced = sdk::Schedule {
            exclusive_choice: Some(false),
            ..schedule
        };
        let json = serde_json::to_value(ScheduleDto::from(forced)).expect("must serialize");
        assert_eq!(json["exclusive_choice"], "false");
    }

    #[test]
    fn the_queue_limit_defaults_and_clamps_as_the_guide_specifies() {
        assert_eq!(QueueQuery::default().effective_limit(), QUEUE_LIMIT_DEFAULT);
        assert_eq!(
            QueueQuery {
                limit: Some(0),
                ..QueueQuery::default()
            }
            .effective_limit(),
            QUEUE_LIMIT_MIN
        );
        assert_eq!(
            QueueQuery {
                limit: Some(u32::MAX),
                ..QueueQuery::default()
            }
            .effective_limit(),
            QUEUE_LIMIT_MAX
        );
        assert_eq!(
            QueueQuery {
                limit: Some(50),
                ..QueueQuery::default()
            }
            .effective_limit(),
            50
        );
    }
}
