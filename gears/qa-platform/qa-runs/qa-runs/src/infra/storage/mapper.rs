//! Entity <-> SDK conversions.
//!
//! ## Every decoder here fails closed
//!
//! An unrecognized enum string, a JSON column that does not parse, or a
//! negative count returns [`DomainError::CorruptState`] naming the offending
//! row — never a default. A permissive default is not a smaller bug here, it
//! is a larger one: a run whose `parameters` quietly decoded to `[]` would
//! execute with the wrong environment, one whose target decoded to `None`
//! would execute nothing at all while reporting success, and one whose `state`
//! decoded to `Created` would re-run finished work.
//!
//! ## …with exactly one deliberate exception, on an adjacent column
//!
//! `qa_run_test_results.status` is **not** decoded here and **must not** be.
//! Its vocabulary is an *open* set of uppercase strings, and the migration's
//! column comment is its definitive record. The source system puts
//! unconstrained runner text into that column twice over: the progress event's
//! `status: String` reaches the INSERT with no validation between the two
//! (`manager/src/routes/runs.rs:1115` declares it, `:1168-1174` binds it), and
//! the log-parse mapper falls through to `other.to_uppercase()` for any pytest
//! outcome outside its six-way map (`manager/src/services/argo.rs:2932-2943`).
//! A ninth value is therefore a runner change, not corruption, and rejecting
//! it would drop a real result. So the status crosses this layer as a
//! `String`: count what is recognized, store what arrives.
//!
//! ## Enum *encoding* is the SDK's job; enum decoding is this module's
//!
//! Narrowly about enums — the JSON and target columns are encoded here, by
//! `parameters_to_column`, `json_to_column` and `target_to_columns`, because
//! their storage shape is this layer's business and the SDK never sees it.
//! What this module has none of is `*_to_str`. `qa_runs_sdk` ships `as_str()` on
//! all five persisted enums ([`RunKind`], [`RunSource`], [`RunState`],
//! [`ExclusiveTier`], [`QueueState`]) and is the sole encoder. A second
//! encoder in this file would give the two spellings of cancel*ed* — the run's
//! `canceled` and the queue row's `cancelled` — two independent homes, and the
//! next person to "fix" the inconsistency would only have to find one of them.
//!
//! [`exclusive_choice_to_column`] is an encoder and is not a counter-example:
//! the schedule's tri-state is an `Option<bool>` on the contract, not an enum,
//! so the SDK ships no `as_str()` for it and there is nothing here to duplicate.
//! Its two halves are one codec and are kept adjacent for the reason the source
//! system keeps its own pair together — *"splitting them would mean a change to
//! that vocabulary had to be made in two files to stay consistent"*
//! (`manager/src/services/exclusivity.rs:60-72`).

use qa_runs_sdk::{
    ExclusiveTier, QueueState, Run, RunKind, RunParameter, RunResult, RunSource, RunState,
    RunTarget, Schedule,
};
use sea_orm::entity::prelude::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::entity::{run, run_queue, run_test_result, schedule};
use crate::domain::error::DomainError;
use crate::domain::repos::{QueueRowRecord, RunWithResult, TestResultRow};

/// Build a fail-closed decode error for `row_id`'s `what` column.
fn corrupt(what: &'static str, id: Uuid, value: impl std::fmt::Display) -> DomainError {
    DomainError::CorruptState {
        what,
        id,
        value: value.to_string(),
    }
}

/// Decode `qa_runs.state`. The encoder is [`RunState::as_str`].
///
/// # Errors
///
/// [`DomainError::CorruptState`] on any value the SDK does not spell.
pub(crate) fn run_state_from_str(raw: &str, row_id: Uuid) -> Result<RunState, DomainError> {
    match raw {
        "created" => Ok(RunState::Created),
        "queued" => Ok(RunState::Queued),
        "dispatching" => Ok(RunState::Dispatching),
        "running" => Ok(RunState::Running),
        "succeeded" => Ok(RunState::Succeeded),
        "failed" => Ok(RunState::Failed),
        // One `l`. The queue row's is spelled with two and is a different
        // vocabulary — see this module's header.
        "canceled" => Ok(RunState::Canceled),
        "timed_out" => Ok(RunState::TimedOut),
        "expired" => Ok(RunState::Expired),
        "error" => Ok(RunState::Error),
        other => Err(corrupt("run.state", row_id, other)),
    }
}

/// Decode `qa_run_queue.state` — the seven frozen names
/// (`../testrunner/docs/guides/exclusive-runs-and-the-queue.md` lines 88-96).
///
/// # Errors
///
/// [`DomainError::CorruptState`] on any value outside the seven.
pub(crate) fn queue_state_from_str(raw: &str, row_id: Uuid) -> Result<QueueState, DomainError> {
    match raw {
        "queued" => Ok(QueueState::Queued),
        "dispatching" => Ok(QueueState::Dispatching),
        "running" => Ok(QueueState::Running),
        "done" => Ok(QueueState::Done),
        "failed" => Ok(QueueState::Failed),
        // Two `l`s, ported verbatim from
        // `manager/src/services/run_queue.rs:413` (`SET state = 'cancelled'`).
        "cancelled" => Ok(QueueState::Cancelled),
        "expired" => Ok(QueueState::Expired),
        other => Err(corrupt("queue.state", row_id, other)),
    }
}

/// Decode a `run_kind` column. The encoder is [`RunKind::as_str`].
///
/// **A catch-all `match`, so a new [`RunKind`] variant does not fail to
/// compile here — it fails at runtime, as `CorruptState`, on every row that
/// carries it.** That is the fail-closed direction and it is deliberate for a
/// *corrupt* value; it is the wrong direction for a *new* one, and nothing
/// warns. This arm is the reason the collect kind's addition had to be audited
/// by hand rather than by `cargo build`.
///
/// # Errors
///
/// [`DomainError::CorruptState`] on any value outside the four kinds.
pub(crate) fn run_kind_from_str(
    raw: &str,
    what: &'static str,
    row_id: Uuid,
) -> Result<RunKind, DomainError> {
    match raw {
        "plan" => Ok(RunKind::Plan),
        "test" => Ok(RunKind::Test),
        "custom_plan" => Ok(RunKind::CustomPlan),
        "collect" => Ok(RunKind::Collect),
        other => Err(corrupt(what, row_id, other)),
    }
}

/// Decode a `source` column. The encoder is [`RunSource::as_str`].
///
/// # Errors
///
/// [`DomainError::CorruptState`] on any value that is neither spelling.
///
/// Note the asymmetry with the source system, whose `normalize_run_source`
/// folds *anything* that is not `scheduled` into `manual`
/// (`manager/src/services/argo.rs:2461-2466`, called at `:2293-2296` with
/// `unwrap_or_else(|| "manual")` behind it). That normalization belongs at the
/// ingress that produces the value, not at the decoder that reads it back:
/// legacy is parsing an annotation a human may have written by hand, while
/// this is reading a column only this gear writes, so a third spelling here
/// can only mean corruption.
pub(crate) fn run_source_from_str(
    raw: &str,
    what: &'static str,
    row_id: Uuid,
) -> Result<RunSource, DomainError> {
    match raw {
        "manual" => Ok(RunSource::Manual),
        "scheduled" => Ok(RunSource::Scheduled),
        other => Err(corrupt(what, row_id, other)),
    }
}

/// Decode `qa_runs.exclusive_tier`. The encoder is [`ExclusiveTier::as_str`],
/// whose four strings are the ones the source system logs
/// (`manager/src/services/exclusivity.rs:29-36`), so an operator grepping old
/// and new logs sees the same tokens.
///
/// # Errors
///
/// [`DomainError::CorruptState`] on any value outside the four tiers.
pub(crate) fn exclusive_tier_from_str(
    raw: &str,
    row_id: Uuid,
) -> Result<ExclusiveTier, DomainError> {
    match raw {
        "launch" => Ok(ExclusiveTier::Launch),
        "plan.yaml" => Ok(ExclusiveTier::Plan),
        "test_meta" => Ok(ExclusiveTier::TestMeta),
        "default" => Ok(ExclusiveTier::Default),
        other => Err(corrupt("run.exclusive_tier", row_id, other)),
    }
}

/// Encode a schedule's tri-state exclusivity choice for `exclusive_choice`.
///
/// `Some(true) -> "true"`, `Some(false) -> "false"`, `None -> "auto"`, and the
/// `None` case is **written**, never omitted. The column is `NOT NULL`, so the
/// schema makes omission impossible — but the direction matters even so, and
/// the source system is where it was learned: its discarded first attempt wrote
/// the equivalent annotation only when the choice was `true`, which left
/// "absent" meaning both *inherit* and *parallel* in objects that had already
/// shipped (`manager/src/services/exclusivity.rs`,
/// `format_exclusive_annotation`, lines 60-79).
///
/// A `&'static str` rather than a `String`: the three values are the whole
/// vocabulary, and returning owned text would invite a caller to build a fourth.
pub(crate) const fn exclusive_choice_to_column(choice: Option<bool>) -> &'static str {
    match choice {
        Some(true) => "true",
        Some(false) => "false",
        None => "auto",
    }
}

/// Decode `qa_schedules.exclusive_choice`, failing closed.
///
/// **A fourth value is [`DomainError::CorruptState`], never `auto`.** Folding an
/// unrecognised string into `auto` is the exact conflation the tri-state exists
/// to prevent, one layer up from where the source system made it: `auto` means
/// "inherit from the lower exclusivity tiers", which resolves to whatever those
/// say, while `false` forces parallel. A corrupt row silently read as
/// `auto` would therefore let an exclusive plan turn a schedule the operator set
/// to parallel into an exclusive run — and, where the operator wrote `true`, it
/// would run a destructive suite beside other work.
///
/// # Errors
///
/// [`DomainError::CorruptState`] on any value outside the three.
pub(crate) fn exclusive_choice_from_str(
    raw: &str,
    row_id: Uuid,
) -> Result<Option<bool>, DomainError> {
    match raw {
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        "auto" => Ok(None),
        other => Err(corrupt("schedule.exclusive_choice", row_id, other)),
    }
}

/// The four flattened target columns, named so the encoder's output cannot be
/// transposed into the decoder's input.
///
/// The four columns are two same-typed pairs, not four distinguishable types:
/// `repo_id` and `custom_plan_id` are both `Option<Uuid>`, `path` and
/// `test_file` are both `Option<String>`. As the plain
/// `(Option<Uuid>, Option<String>, Option<String>, Option<Uuid>)` tuple this
/// started as, swapping either pair type-checks — and swapping `path` with
/// `test_file` at the one call site that assembles an `ActiveModel` would file
/// a plan path as a test file, launching a run against the wrong content
/// silently. Same remedy the source system reaches for against the same hazard
/// (`manager/src/services/run_queue.rs:682-693`, `OccupancySources`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetColumns {
    pub repo_id: Option<Uuid>,
    pub path: Option<String>,
    pub test_file: Option<String>,
    pub custom_plan_id: Option<Uuid>,
    /// Set for a `collect` target and `None` for every other kind. A distinct
    /// column rather than a reuse of `path` — `m20260818_000006_collect_target`
    /// argues why.
    pub collect_url: Option<String>,
}

/// Encode a target into the four flattened columns.
pub(crate) fn target_to_columns(target: &RunTarget) -> TargetColumns {
    match target {
        RunTarget::Plan { repo_id, path } => TargetColumns {
            repo_id: Some(*repo_id),
            path: Some(path.clone()),
            test_file: None,
            custom_plan_id: None,
            collect_url: None,
        },
        RunTarget::Test {
            repo_id,
            path,
            test_file,
        } => TargetColumns {
            repo_id: Some(*repo_id),
            path: Some(path.clone()),
            test_file: Some(test_file.clone()),
            custom_plan_id: None,
            collect_url: None,
        },
        RunTarget::CustomPlan { id } => TargetColumns {
            repo_id: None,
            path: None,
            test_file: None,
            custom_plan_id: Some(*id),
            collect_url: None,
        },
        // A collect target has a repository and a report URL and no plan at
        // all: it enumerates whatever the branch holds rather than a named
        // `plan.yaml` (`manager/src/services/collect.rs:56-75`, the union of
        // every plan's files). So `path` stays `None`, and the branch is not
        // here either — it is the run's `test_version` (rule 6).
        //
        // The URL is stored even when blank, because blank and absent mean
        // different things on read-back: `None` for a `collect` row would be a
        // row whose kind and columns disagree, which `target_from_columns`
        // rejects as corrupt.
        RunTarget::Collect {
            repo_id,
            collect_url,
        } => TargetColumns {
            repo_id: Some(*repo_id),
            path: None,
            test_file: None,
            custom_plan_id: None,
            collect_url: Some(collect_url.clone()),
        },
    }
}

/// Decode a target from `run_kind` plus the four columns.
///
/// `run_kind` is the discriminant; a row whose columns disagree with it is
/// corrupt and rejected. Reconstructing a partial target instead would launch
/// a run against the wrong content.
///
/// # Errors
///
/// [`DomainError::CorruptState`] when the columns do not match the kind
/// exactly — including extra columns that are set but should not be.
///
/// **The final arm is a catch-all**, so a new [`RunKind`] variant compiles here
/// and then answers `CorruptState` for every row of that kind. Fail-closed, and
/// the wrong failure: the row is not corrupt, the decoder is incomplete. Audited
/// by hand when the collect kind landed, and named here so the next addition is
/// not audited by accident.
pub(crate) fn target_from_columns(
    run_kind: RunKind,
    columns: TargetColumns,
    row_id: Uuid,
) -> Result<RunTarget, DomainError> {
    let TargetColumns {
        repo_id,
        path,
        test_file,
        custom_plan_id,
        collect_url,
    } = columns;

    match (
        run_kind,
        repo_id,
        path,
        test_file,
        custom_plan_id,
        collect_url,
    ) {
        (RunKind::Plan, Some(repo_id), Some(path), None, None, None) => {
            Ok(RunTarget::Plan { repo_id, path })
        }
        (RunKind::Test, Some(repo_id), Some(path), Some(test_file), None, None) => {
            Ok(RunTarget::Test {
                repo_id,
                path,
                test_file,
            })
        }
        (RunKind::CustomPlan, None, None, None, Some(id), None) => Ok(RunTarget::CustomPlan { id }),
        (RunKind::Collect, Some(repo_id), None, None, None, Some(collect_url)) => {
            Ok(RunTarget::Collect {
                repo_id,
                collect_url,
            })
        }
        _ => Err(corrupt(
            "run.target",
            row_id,
            format_args!("run_kind={}", run_kind.as_str()),
        )),
    }
}

/// Decode a JSON column, failing closed.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if the stored value is not `T`.
pub(crate) fn json_from_column<T: serde::de::DeserializeOwned>(
    what: &'static str,
    value: &Json,
    row_id: Uuid,
) -> Result<T, DomainError> {
    serde_json::from_value(value.clone()).map_err(|error| corrupt(what, row_id, error))
}

/// The storage shape of one launch parameter.
///
/// `qa_runs_sdk` is deliberately serde-free (its `lib.rs` states the rule), so
/// [`RunParameter`] cannot be `#[derive(Serialize)]`d and the JSON shape is
/// spelled here instead. Field names are the wire contract for the
/// `parameters` column and must not drift from the migration's
/// "JSON array of `{name, value}`".
///
/// `deny_unknown_fields` closes one half of serde's leniency; the object-shape
/// check in [`parameters_from_column`] closes the other. See there.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredParameter {
    name: String,
    value: String,
}

/// Encode launch parameters for the `parameters` column.
///
/// # Errors
///
/// [`DomainError::Internal`] if serialization fails, which for a list of
/// string pairs cannot happen — it is surfaced rather than unwrapped because
/// an infallible-looking `expect` in a write path is a worse trade than one
/// unreachable error arm.
pub(crate) fn parameters_to_column(parameters: &[RunParameter]) -> Result<Json, DomainError> {
    let stored: Vec<StoredParameter> = parameters
        .iter()
        .map(|p| StoredParameter {
            name: p.name.clone(),
            value: p.value.clone(),
        })
        .collect();
    serde_json::to_value(stored)
        .map_err(|error| DomainError::Internal(format!("failed to encode run parameters: {error}")))
}

/// Decode the `parameters` column, failing closed.
///
/// ## Why the shape is checked before serde sees it
///
/// A derived `Deserialize` for a two-field struct also accepts a **sequence**
/// of two values, positionally: without the guard below, a `parameters` column
/// holding `[["APP_BUILD", "20260813"]]` — the shape a naive
/// `Vec<(String, String)>` encoder would write — decodes cleanly. That is
/// leniency in the one codec that advertises the opposite, and it hides
/// exactly the class of drift this column would suffer: a writer that changed
/// the storage shape would keep round-tripping through this function while
/// every other reader of the column broke. Found by the round-trip test below,
/// which asserted the wrong shape was rejected and was itself red.
///
/// `what` names the column, because two tables now store this shape —
/// `qa_runs.parameters` and `qa_schedules.parameters`. It was a hardcoded
/// `"run.parameters"` while there was one; a `CorruptState` naming the wrong
/// table is a false diagnostic pointing an operator at the wrong row.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if the column is not an array of
/// `{name, value}` objects.
pub(crate) fn parameters_from_column(
    what: &'static str,
    value: &Json,
    row_id: Uuid,
) -> Result<Vec<RunParameter>, DomainError> {
    let items = value
        .as_array()
        .ok_or_else(|| corrupt(what, row_id, "not a JSON array"))?;
    if let Some(position) = items.iter().position(|item| !item.is_object()) {
        return Err(corrupt(
            what,
            row_id,
            format_args!("element {position} is not a {{name, value}} object"),
        ));
    }

    let stored: Vec<StoredParameter> = json_from_column(what, value, row_id)?;
    Ok(stored
        .into_iter()
        .map(|p| RunParameter {
            name: p.name,
            value: p.value,
        })
        .collect())
}

/// Encode a JSON column from any serializable value.
///
/// # Errors
///
/// [`DomainError::Internal`] if serialization fails.
pub(crate) fn json_to_column<T: Serialize>(what: &str, value: &T) -> Result<Json, DomainError> {
    serde_json::to_value(value)
        .map_err(|error| DomainError::Internal(format!("failed to encode {what}: {error}")))
}

/// The width of `qa_run_test_results.status`, in characters.
///
/// `VARCHAR(16)`: eight known values, longest `SKIPPED`/`PENDING`/`RUNNING` at
/// 7. See [`normalize_test_status`] for why the number is repeated here.
pub(crate) const MAX_TEST_STATUS_LEN: usize = 16;

/// Make a runner-supplied status fit the column, so the column can never
/// reject it.
///
/// **This is what keeps the open-set rule from being undone by the schema.**
/// The status vocabulary is deliberately open — count what is recognized,
/// store what arrives — but `status VARCHAR(16)` is a *closed* constraint on
/// the same value, and the two disagree for any status longer than 16
/// characters. On Postgres such a write raises `22001`, which surfaces as
/// `Database` (a 500) and drops a real result: the column would reject exactly
/// the ninth value the open-set rule exists to accept. `INFRASTRUCTURE_ERROR`
/// is 20 characters, so this is not hypothetical.
///
/// **The test tier is blind to this.** `SQLite` does not enforce `VARCHAR`
/// lengths — it has type affinity, not width — so every DB-backed test in this
/// crate stores an over-long status happily.
/// `an_over_long_status_is_truncated_to_fit_the_column` asserts the
/// non-enforcement first, so the test cannot quietly become the reason nobody
/// noticed.
///
/// Truncation, not rejection: a truncated status is a wrong bucket at worst,
/// while a rejected one is a lost result, and the whole point of the open set
/// is that losing a result is the bad outcome. Truncation is logged at WARN
/// because it means the runner and this column have diverged.
///
/// **Case is left alone, deliberately**, though it is tempting to fold here.
/// The source system's two writers disagree: the log-parse mapper uppercases
/// (`manager/src/services/argo.rs:2940`, `other => return
/// other.to_uppercase()`) while the live progress endpoint binds the runner's
/// string raw (`manager/src/routes/runs.rs:1174`, `.bind(&event.status)`). The
/// four categorised counters match uppercase spellings only
/// (`manager/src/routes/plans.rs:188-191`), so in legacy a lowercase `passed`
/// from the progress path lands in **none of them** — while still counting
/// toward `total`, which is `COUNT(tr.id)` over every row regardless of status
/// (`plans.rs:192`). The result is a run whose four buckets do not sum to its
/// total, which is the same shape as the `XFAIL`/`XPASS` gap the migration
/// documents, arrived at by a different route.
///
/// **Corrected 2026-08-13 by the spec review.** This paragraph previously said
/// such a status "counts toward nothing", which is false and was propagated
/// into the plan before it was caught: the `total` arm of the very projection
/// cited is unconditional. The conclusion is unaffected — uppercasing here
/// would silently change that arithmetic, and under a parity mandate that call
/// belongs to whoever owns ingest and the counter mapping, made explicitly.
///
/// Truncation is by `char`, not by byte: the column is measured in characters
/// and slicing a byte index would panic mid-codepoint.
pub(crate) fn normalize_test_status(status: String) -> String {
    if status.chars().count() <= MAX_TEST_STATUS_LEN {
        return status;
    }
    let truncated: String = status.chars().take(MAX_TEST_STATUS_LEN).collect();
    tracing::warn!(
        original_len = status.chars().count(),
        truncated = %truncated,
        "per-test status exceeded the column width and was truncated; the runner \
         and qa_run_test_results.status have diverged"
    );
    truncated
}

/// Widen an `INTEGER` counter column into its unsigned contract type, failing
/// closed on a negative value.
///
/// The seam exists because the five counters are `INTEGER` in the schema and
/// `usize` on [`RunResult`], and the workspace denies `clippy::cast_sign_loss`
/// — so `as usize` is not available and would be wrong anyway: a wrapped
/// negative becomes an enormous positive that sails straight through
/// `domain::state_machine::derive_terminal_state`'s `failed > 0` test and
/// reports a clean run as failed. Same shape as qa-catalog's `u64_from_db`
/// (`qa-catalog/src/infra/storage/mapper.rs`), for the same reason.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if `value` is negative.
pub(crate) fn usize_from_db(
    value: i32,
    what: &'static str,
    row_id: Uuid,
) -> Result<usize, DomainError> {
    usize::try_from(value).map_err(|_| corrupt(what, row_id, format_args!("negative {value}")))
}

/// Narrow a counter delta into the `INTEGER` column type.
///
/// The write half of the same seam. `domain::repos::RunResultDelta` is `i64`
/// so the domain does not have to know how wide the column is; this is where
/// that width is applied, and an out-of-range delta is
/// [`DomainError::Validation`] rather than [`DomainError::CorruptState`]
/// because it is caller input, not a corrupt row — the same split qa-catalog
/// draws between `u64_from_db` and `db_i64_from_u64`.
///
/// # Errors
///
/// [`DomainError::Validation`] if the value does not fit an `i32`.
pub(crate) fn db_i32_from_i64(value: i64, field: &str) -> Result<i32, DomainError> {
    i32::try_from(value).map_err(|_| DomainError::Validation {
        field: field.to_owned(),
        message: format!("must be within {}..={}", i32::MIN, i32::MAX),
    })
}

/// Convert a run row into the contract model.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if any enum, JSON or target column does not
/// decode — see this module's header.
pub(crate) fn run_to_sdk(m: run::Model) -> Result<Run, DomainError> {
    let id = m.id;
    let run_kind = run_kind_from_str(&m.run_kind, "run.run_kind", id)?;
    let target = target_from_columns(
        run_kind,
        TargetColumns {
            repo_id: m.target_repo_id,
            path: m.target_path,
            test_file: m.target_test_file,
            custom_plan_id: m.target_custom_plan_id,
            collect_url: m.target_collect_url,
        },
        id,
    )?;

    Ok(Run {
        id,
        name: m.name,
        target,
        platform_id: m.environment_id,
        test_version: m.test_version,
        app_version: m.app_version,
        app_build: m.app_build,
        state: run_state_from_str(&m.state, id)?,
        resolved_exclusive: m.resolved_exclusive,
        exclusive_tier: exclusive_tier_from_str(&m.exclusive_tier, id)?,
        is_validation: m.is_validation,
        parameters: parameters_from_column("run.parameters", &m.parameters, id)?,
        include_tags: json_from_column("run.include_tags", &m.include_tags, id)?,
        exclude_tags: json_from_column("run.exclude_tags", &m.exclude_tags, id)?,
        source: run_source_from_str(&m.source, "run.source", id)?,
        schedule_id: m.schedule_id,
        bundle_ids: json_from_column("run.bundle_ids", &m.bundle_ids, id)?,
        execution_ref: m.execution_ref,
        log_storage_ref: m.log_storage_ref,
        timeout_at: m.timeout_at,
        started_at: m.started_at,
        finished_at: m.finished_at,
        error: m.error,
        created_at: m.created_at,
        updated_at: m.updated_at,
    })
}

/// Read the five denormalized counters off a run row.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if any counter is negative.
pub(crate) fn run_result_from_row(m: &run::Model) -> Result<RunResult, DomainError> {
    let id = m.id;
    Ok(RunResult {
        passed: usize_from_db(m.passed, "run.passed", id)?,
        failed: usize_from_db(m.failed, "run.failed", id)?,
        skipped: usize_from_db(m.skipped, "run.skipped", id)?,
        in_progress: usize_from_db(m.in_progress, "run.in_progress", id)?,
        total: usize_from_db(m.total, "run.total", id)?,
    })
}

/// [`run_to_sdk`] and [`run_result_from_row`], composed for
/// [`RunsRepository::list_page`](crate::domain::repos::RunsRepository::list_page).
///
/// One row, one decode, two views of it — not a second query. `result` is
/// read before `run` consumes the model by value, which is the only ordering
/// constraint; nothing else about the two conversions interacts.
pub(crate) fn run_with_result_from_row(m: run::Model) -> Result<RunWithResult, DomainError> {
    let result = run_result_from_row(&m)?;
    let run = run_to_sdk(m)?;
    Ok(RunWithResult { run, result })
}

/// Convert a schedule row into the contract model.
///
/// Reuses the run's target codec: a schedule stores its target in the same four
/// flattened columns discriminated by the same `run_kind`, so
/// [`target_from_columns`] is what decodes it. That is not merely convenient —
/// a schedule's whole purpose is to produce a run with this target, and two
/// codecs could drift into two readings of one column set.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if `run_kind`, the target columns,
/// `exclusive_choice` or any JSON column does not decode — the JSON columns
/// being `include_tags`, `exclude_tags`, `parameters` and, since
/// `m20260818_000007_schedule_notifications`, `slack_notification_events`.
pub(crate) fn schedule_to_sdk(m: schedule::Model) -> Result<Schedule, DomainError> {
    let id = m.id;
    let run_kind = run_kind_from_str(&m.run_kind, "schedule.run_kind", id)?;
    let target = target_from_columns(
        run_kind,
        TargetColumns {
            repo_id: m.target_repo_id,
            path: m.target_path,
            test_file: m.target_test_file,
            custom_plan_id: m.target_custom_plan_id,
            collect_url: m.target_collect_url,
        },
        id,
    )?;

    Ok(Schedule {
        id,
        name: m.name,
        target,
        platform_id: m.environment_id,
        branch: m.branch,
        cron: m.cron,
        exclusive_choice: exclusive_choice_from_str(&m.exclusive_choice, id)?,
        enabled: m.enabled,
        include_tags: json_from_column("schedule.include_tags", &m.include_tags, id)?,
        exclude_tags: json_from_column("schedule.exclude_tags", &m.exclude_tags, id)?,
        parameters: parameters_from_column("schedule.parameters", &m.parameters, id)?,
        // The three notification columns `m20260818_000007_schedule_notifications`
        // added. **Both halves of this codec have to name them or a user's Slack
        // settings are lost silently**: this direction is the read, and
        // `schedules_sea_repo::schedule_columns` is the write. A literal that
        // omits one of the three still compiles and still passes every
        // assertion about the other twelve fields, which is why
        // `schedules_sea_repo::tests::every_notification_setting_survives_the_column`
        // sets all three to distinct non-default values and checks all three.
        //
        // No `NULL`-means-empty arm on the events column: it is `NOT NULL` with
        // a `'[]'` default, which is the one strategy the migration commits to.
        // A second reading here would be exactly the drift that comment exists
        // to prevent, and it would decode as `CorruptState` rather than as an
        // empty list if the column were ever nullable — deliberately, since a
        // `NULL` in a `NOT NULL` column is corruption.
        slack_notifications_enabled: m.slack_notifications_enabled,
        slack_channel: m.slack_channel,
        slack_notification_events: json_from_column(
            "schedule.slack_notification_events",
            &m.slack_notification_events,
            id,
        )?,
        last_fired_tick: m.last_fired_tick,
        created_at: m.created_at,
        updated_at: m.updated_at,
    })
}

/// Convert a queue row into the repository-level record.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if `state`, `run_kind` or `source` does not
/// decode.
pub(crate) fn queue_row_to_record(m: run_queue::Model) -> Result<QueueRowRecord, DomainError> {
    let id = m.id;
    Ok(QueueRowRecord {
        id,
        tenant_id: m.tenant_id,
        run_id: m.run_id,
        platform_id: m.environment_id,
        run_kind: run_kind_from_str(&m.run_kind, "queue.run_kind", id)?,
        source: run_source_from_str(&m.source, "queue.source", id)?,
        exclusive: m.exclusive,
        state: queue_state_from_str(&m.state, id)?,
        error: m.error,
        enqueued_at: m.enqueued_at,
        dispatched_at: m.dispatched_at,
        finished_at: m.finished_at,
    })
}

/// Convert a per-test row into the repository-level record.
///
/// Infallible: `status` crosses as a `String` on purpose — see this module's
/// header. There is nothing on this row that can fail to decode, which is
/// exactly the property the open-set rule buys.
pub(crate) fn test_result_to_row(m: run_test_result::Model) -> TestResultRow {
    TestResultRow {
        id: m.id,
        run_id: m.run_id,
        test_file: m.test_file,
        test_name: m.test_name,
        status: m.status,
        duration: m.duration,
        launch_id: m.launch_id,
        jira_key: m.jira_key,
        // The three case-fidelity columns Task 2 added and Task 3 wrote. They
        // were not read back until Task 4 gave them a reader; a literal that
        // drops one of them still compiles and still passes every assertion
        // about the other seven, which is why
        // `runs_sea_repo::tests::every_field_of_a_per_test_row_survives_the_trip_to_the_sdk`
        // sets all ten to distinct values and checks all ten.
        nodeid: m.nodeid,
        reason: m.reason,
        ticket: m.ticket,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Deterministic stand-in for the row whose column is being decoded.
    fn row() -> Uuid {
        Uuid::from_u128(0xB0)
    }

    /// Every state round-trips through the SDK's encoder, and the set is
    /// exhaustive: a new variant this decoder does not know fails here rather
    /// than reaching production as a corrupt-state error on a real run.
    #[test]
    fn every_run_state_round_trips() {
        for state in [
            RunState::Created,
            RunState::Queued,
            RunState::Dispatching,
            RunState::Running,
            RunState::Succeeded,
            RunState::Failed,
            RunState::Canceled,
            RunState::TimedOut,
            RunState::Expired,
            RunState::Error,
        ] {
            let encoded = state.as_str();
            assert_eq!(
                run_state_from_str(encoded, row()).unwrap(),
                state,
                "{encoded}"
            );
        }
    }

    #[test]
    fn every_queue_state_round_trips() {
        for state in [
            QueueState::Queued,
            QueueState::Dispatching,
            QueueState::Running,
            QueueState::Done,
            QueueState::Failed,
            QueueState::Cancelled,
            QueueState::Expired,
        ] {
            let encoded = state.as_str();
            assert_eq!(
                queue_state_from_str(encoded, row()).unwrap(),
                state,
                "{encoded}"
            );
        }
    }

    /// The two vocabularies differ by one letter and must not be unified: the
    /// queue row's frozen spelling is `cancelled` (guide line 95,
    /// `manager/src/services/run_queue.rs:413`) while the run's is `canceled`
    /// (DESIGN §3.1's own vocabulary, which legacy has no counterpart for). A
    /// mapper that "fixed" either would silently reject every persisted row of
    /// the other kind.
    #[test]
    fn the_run_and_queue_cancel_spellings_are_deliberately_different() {
        assert_eq!(RunState::Canceled.as_str(), "canceled");
        assert_eq!(QueueState::Cancelled.as_str(), "cancelled");
        assert!(run_state_from_str("cancelled", row()).is_err());
        assert!(queue_state_from_str("canceled", row()).is_err());
    }

    /// The tier strings are what the source system logs
    /// (`manager/src/services/exclusivity.rs:29-36`), so an operator grepping
    /// old and new logs sees the same four tokens.
    #[test]
    fn exclusive_tier_strings_match_the_legacy_log_tokens() {
        assert_eq!(ExclusiveTier::Launch.as_str(), "launch");
        assert_eq!(ExclusiveTier::Plan.as_str(), "plan.yaml");
        assert_eq!(ExclusiveTier::TestMeta.as_str(), "test_meta");
        assert_eq!(ExclusiveTier::Default.as_str(), "default");
        for tier in [
            ExclusiveTier::Launch,
            ExclusiveTier::Plan,
            ExclusiveTier::TestMeta,
            ExclusiveTier::Default,
        ] {
            assert_eq!(exclusive_tier_from_str(tier.as_str(), row()).unwrap(), tier);
        }
    }

    #[test]
    fn every_run_kind_and_source_round_trips() {
        for kind in [RunKind::Plan, RunKind::Test, RunKind::CustomPlan] {
            assert_eq!(
                run_kind_from_str(kind.as_str(), "run.run_kind", row()).unwrap(),
                kind
            );
        }
        for source in [RunSource::Manual, RunSource::Scheduled] {
            assert_eq!(
                run_source_from_str(source.as_str(), "run.source", row()).unwrap(),
                source
            );
        }
        // `custom-plan` (a hyphen) and `Manual` (a capital) are the two
        // near-misses a hand-written migration or a fixture would produce.
        assert!(run_kind_from_str("custom-plan", "run.run_kind", row()).is_err());
        assert!(run_source_from_str("Manual", "run.source", row()).is_err());
    }

    #[test]
    fn an_unknown_state_string_fails_closed_and_names_the_row() {
        let id = Uuid::from_u128(0x5E);
        let err = run_state_from_str("Succeeded", id).unwrap_err();
        assert!(
            matches!(err, DomainError::CorruptState { what: "run.state", id: named, .. } if named == id),
            "an unrecognised value must be an error naming the row, never a default; got {err:?}"
        );
        assert!(run_state_from_str("", id).is_err());
        assert!(
            exclusive_tier_from_str("plan", id).is_err(),
            "the tier is `plan.yaml`, not `plan`"
        );
    }

    /// All three values, both directions. The `None` arm is the one that
    /// matters: it must encode to a written `"auto"` rather than to nothing, so
    /// "inherit" always has a spelling and can never be read as "parallel".
    #[test]
    fn the_exclusive_choice_tri_state_round_trips() {
        for choice in [Some(true), Some(false), None] {
            let encoded = exclusive_choice_to_column(choice);
            assert_eq!(
                exclusive_choice_from_str(encoded, row()).unwrap(),
                choice,
                "{encoded}"
            );
        }

        // The spellings themselves, pinned: they are the persisted vocabulary
        // and the source system's annotation values
        // (`manager/src/services/exclusivity.rs:73-79`), so changing one is a
        // schema change and not a rename.
        assert_eq!(exclusive_choice_to_column(Some(true)), "true");
        assert_eq!(exclusive_choice_to_column(Some(false)), "false");
        assert_eq!(exclusive_choice_to_column(None), "auto");

        // `false` and `auto` are different answers, which is the whole point of
        // the third value: one forces parallel, the other defers to the plan
        // and test-metadata tiers.
        assert_ne!(
            exclusive_choice_from_str("false", row()).unwrap(),
            exclusive_choice_from_str("auto", row()).unwrap(),
        );
    }

    /// An unrecognised value is corruption, **never** a silent `auto`.
    ///
    /// Folding it into `auto` is the conflation the tri-state exists to prevent
    /// (`manager/src/services/exclusivity.rs:60-79`), so the near-misses a
    /// hand-written migration or a fixture would produce are checked
    /// individually rather than through one representative.
    #[test]
    fn an_unknown_exclusive_choice_fails_closed() {
        let id = Uuid::from_u128(0x5C);
        for bad in [
            "", "TRUE", "False", "AUTO", "inherit", "1", "0", "null", "yes",
        ] {
            let err = exclusive_choice_from_str(bad, id).unwrap_err();
            assert!(
                matches!(
                    err,
                    DomainError::CorruptState {
                        what: "schedule.exclusive_choice",
                        id: named,
                        ..
                    } if named == id
                ),
                "{bad:?} must be a corrupt-state error naming the row, never a default; \
                 got {err:?}"
            );
        }
    }

    #[test]
    fn every_target_round_trips() {
        let repo = Uuid::from_u128(1);
        let plan = Uuid::from_u128(2);
        for target in [
            RunTarget::Plan {
                repo_id: repo,
                path: "plans/smoke.yaml".to_owned(),
            },
            RunTarget::Test {
                repo_id: repo,
                path: "plans/smoke.yaml".to_owned(),
                test_file: "tests/test_a.py".to_owned(),
            },
            RunTarget::CustomPlan { id: plan },
            RunTarget::Collect {
                repo_id: repo,
                collect_url: "https://insights.example/qa/v1/collect/r/main".to_owned(),
            },
            // A blank URL is legal and must survive as blank rather than
            // collapsing to `None` — which for a `collect` row would decode as
            // corrupt, because the arm requires the column to be present.
            RunTarget::Collect {
                repo_id: repo,
                collect_url: String::new(),
            },
        ] {
            let columns = target_to_columns(&target);
            assert_eq!(
                target_from_columns(target.kind(), columns, row()).unwrap(),
                target
            );
        }
    }

    /// Columns that disagree with `run_kind` are corrupt, not partially
    /// usable. Reconstructing a partial target would launch a run against the
    /// wrong content.
    #[test]
    fn a_target_inconsistent_with_its_run_kind_fails_closed() {
        let repo = Uuid::from_u128(1);
        let plan = Uuid::from_u128(2);
        let columns =
            |repo_id, path: Option<&str>, test_file: Option<&str>, custom| TargetColumns {
                repo_id,
                path: path.map(str::to_owned),
                test_file: test_file.map(str::to_owned),
                custom_plan_id: custom,
                collect_url: None,
            };

        // Plan kind with no path.
        assert!(
            target_from_columns(RunKind::Plan, columns(Some(repo), None, None, None), row())
                .is_err()
        );
        // Test kind with no test file.
        assert!(
            target_from_columns(
                RunKind::Test,
                columns(Some(repo), Some("p"), None, None),
                row()
            )
            .is_err()
        );
        // Custom-plan kind carrying repo columns.
        assert!(
            target_from_columns(
                RunKind::CustomPlan,
                columns(Some(repo), None, None, Some(plan)),
                row()
            )
            .is_err()
        );
        // Plan kind with a custom-plan id as well.
        assert!(
            target_from_columns(
                RunKind::Plan,
                columns(Some(repo), Some("p"), None, Some(plan)),
                row()
            )
            .is_err()
        );
        // Plan kind carrying a test file: this is the one that would otherwise
        // "work" — the plan arm's columns are all present, so a decoder that
        // ignored the extra column would hand back a plan run for a row that
        // was written as a single-test run.
        assert!(
            target_from_columns(
                RunKind::Plan,
                columns(Some(repo), Some("p"), Some("t.py"), None),
                row()
            )
            .is_err()
        );
        // Collect kind with no URL column. The write path always sets it — a
        // blank URL is stored as `Some("")` — so `None` here means the row was
        // written as some other kind and its `run_kind` was edited, or the
        // column was added and never backfilled.
        assert!(
            target_from_columns(
                RunKind::Collect,
                columns(Some(repo), None, None, None),
                row()
            )
            .is_err()
        );
        // Collect kind carrying a plan path as well: the decoder must not hand
        // back a collect target for a row that also claims a `plan.yaml`.
        assert!(
            target_from_columns(
                RunKind::Collect,
                TargetColumns {
                    repo_id: Some(repo),
                    path: Some("plans/smoke.yaml".to_owned()),
                    test_file: None,
                    custom_plan_id: None,
                    collect_url: Some("https://x".to_owned()),
                },
                row()
            )
            .is_err()
        );
        // A URL on a kind that has no business carrying one.
        assert!(
            target_from_columns(
                RunKind::Plan,
                TargetColumns {
                    repo_id: Some(repo),
                    path: Some("p".to_owned()),
                    test_file: None,
                    custom_plan_id: None,
                    collect_url: Some("https://x".to_owned()),
                },
                row()
            )
            .is_err()
        );
    }

    #[test]
    fn a_corrupt_json_column_fails_closed() {
        let bad = serde_json::json!({"not": "an array"});
        let out: Result<Vec<String>, _> = json_from_column("run.include_tags", &bad, row());
        assert!(
            matches!(
                out,
                Err(DomainError::CorruptState {
                    what: "run.include_tags",
                    ..
                })
            ),
            "a JSON column that does not parse must be an error, never unwrap_or_default()"
        );
    }

    #[test]
    fn a_well_formed_json_column_decodes() {
        let good = serde_json::json!(["e2e", "smoke"]);
        let out: Vec<String> = json_from_column("run.include_tags", &good, row()).unwrap();
        assert_eq!(out, vec!["e2e".to_owned(), "smoke".to_owned()]);
    }

    #[test]
    fn parameters_round_trip_and_fail_closed() {
        let params = vec![
            RunParameter {
                name: "APP_BUILD".to_owned(),
                value: "20260813".to_owned(),
            },
            RunParameter {
                name: "TEST_FILES".to_owned(),
                value: "tests/a.py".to_owned(),
            },
        ];
        let encoded = parameters_to_column(&params).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!([
                {"name": "APP_BUILD", "value": "20260813"},
                {"name": "TEST_FILES", "value": "tests/a.py"},
            ]),
            "the `{{name, value}}` shape is the storage contract the migration states"
        );
        assert_eq!(
            parameters_from_column("run.parameters", &encoded, row()).unwrap(),
            params
        );

        // The shape a naive `Vec<(String, String)>` encoder would have
        // written. serde's derived `Deserialize` accepts a two-element
        // sequence for a two-field struct, so this decoded happily until the
        // explicit object-shape check landed.
        let positional = serde_json::json!([["APP_BUILD", "20260813"]]);
        assert!(
            parameters_from_column("run.parameters", &positional, row()).is_err(),
            "a positional pair is not the storage shape and must not decode"
        );
        // An extra key is drift too — `deny_unknown_fields` is what refuses it.
        let extra_key = serde_json::json!([{"name": "A", "value": "1", "secure": true}]);
        assert!(parameters_from_column("run.parameters", &extra_key, row()).is_err());
        // ...and the plainly wrong shapes.
        assert!(parameters_from_column("run.parameters", &serde_json::json!({}), row()).is_err());
        assert!(parameters_from_column("run.parameters", &serde_json::json!([1]), row()).is_err());
    }

    /// The `INTEGER`/`usize` seam. A negative count is corrupt; a wrapped one
    /// becomes an enormous positive that `derive_terminal_state` would read as
    /// "this run failed".
    #[test]
    fn a_negative_counter_fails_closed_rather_than_wrapping() {
        let err = usize_from_db(-1, "run.failed", row()).unwrap_err();
        assert!(
            matches!(
                err,
                DomainError::CorruptState {
                    what: "run.failed",
                    ..
                }
            ),
            "got {err:?}"
        );
        assert_eq!(usize_from_db(0, "run.failed", row()).unwrap(), 0);
        assert_eq!(usize_from_db(7, "run.total", row()).unwrap(), 7);
    }

    #[test]
    fn an_out_of_range_delta_is_a_validation_error_not_corruption() {
        let err = db_i32_from_i64(i64::from(i32::MAX) + 1, "passed").unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
        // Both directions: deltas are signed, so the negative bound is real.
        assert!(db_i32_from_i64(i64::from(i32::MIN) - 1, "in_progress").is_err());
        assert_eq!(db_i32_from_i64(3, "passed").unwrap(), 3);
        assert_eq!(db_i32_from_i64(-1, "in_progress").unwrap(), -1);
    }
}
