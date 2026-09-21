//! Entity ↔ SDK conversions, plus the two derived values a repository must not
//! leave to a caller.
//!
//! ## Every decoder here fails closed
//!
//! An unrecognized `scope` string, a JSON column that does not parse, or a
//! negative count returns [`DomainError::CorruptState`] naming the offending
//! row — never a default. Shape and reasoning copied from `qa-runs`'
//! `mapper.rs`. A permissive default is not a smaller bug here: a saved view
//! whose `scope` quietly decoded to `All` would appear in the wrong list and
//! collide with the wrong name, and a `case_count` that wrapped a negative into
//! a huge positive would inflate every "expected cases" number the analytics
//! surface shows.
//!
//! ## …with exactly one deliberate exception, on the status columns
//!
//! `qa_test_results.status` and `qa_test_case_results.status` are **not** decoded
//! and must not be. Their vocabulary is an *open* set of producer text — the
//! migration's column comments are the definitive record — and legacy's two
//! writers both put unvalidated runner strings in it (`manager/src/routes/runs.rs:1174`
//! binds the progress event's `status` raw; `manager/src/services/argo.rs:2932-2943`
//! falls through to `other.to_uppercase()`). A ninth value is a runner change,
//! not corruption, and rejecting it would drop a real result. So the status
//! crosses this layer as a `String`: count what is recognized, store what
//! arrives.
//!
//! ## Truncation is what stops the schema re-closing that open set
//!
//! The open-set rule and `status VARCHAR(16)` disagree for any status longer
//! than sixteen characters, and on Postgres such a write raises `22001` — a 500
//! on the one path whose whole purpose is to accept whatever the runner said.
//! `INFRASTRUCTURE_ERROR` is 20 characters, so this is not hypothetical.
//! [`truncate`] resolves it in favour of storing the row, and the same argument
//! extends to every other bounded column ingest writes from producer text.
//!
//! **`SQLite` is blind to this**: it has type affinity, not width, so every
//! DB-backed test in this crate would store an over-long value happily.
//! `results_sea_repo::tests::an_over_long_status_is_truncated_to_fit_its_column`
//! asserts the non-enforcement first, so the test cannot quietly become the
//! reason nobody noticed.
//!
//! **Truncation applies to every writer of *producer* text, and to no writer of
//! *operator* text.** That is the rule; the table it applies to is not.
//!
//! Producer text, truncated: `qa_test_results` and `qa_test_case_results` (the
//! runner's paths, names, statuses, durations and tickets) **and
//! `qa_test_case_collect`** — its `test_file` and `branch` come from
//! `pytest --collect-only` with parametrize expanded, which is the same
//! unconstrained source. Corrected 2026-08-20: this paragraph previously said
//! "the two *ingest* tables only", and collect fell through both halves of the
//! rule — it is not an ingest table and it is not operator input, so nothing
//! covered it and its writer shipped untruncated.
//!
//! Operator text, **not** truncated: the saved-view, bug and settings writers
//! take input through a REST surface that validates it (Tasks 16, 27, 32).
//! Silently shortening a name or a webhook reference there would corrupt data the
//! operator can see, where refusing it tells them. Producer text is the opposite
//! case: nobody is watching, and a dropped result is unrecoverable.
//!
//! ## The `TestCaseResultRecord` conversion arrived at Task 17
//!
//! This section used to explain why there was none: `qa_test_case_results` had a
//! *writer* in `results_sea_repo` and no reader, `ResultsRepository` had exactly
//! five methods and none read case rows, so a `to_sdk` here would have been dead
//! code that *looks* verified. Task 12 twice named Task 21 as the first consumer
//! and was twice corrected to Task 17.
//!
//! Task 17 is where it landed. It registers `GET /qa/v1/test-case-results`, and
//! an `OData` collection *is* a repository method calling `paginate_odata` with
//! the mapper's `*_to_sdk` (`qa-runs/src/infra/storage/runs_sea_repo.rs:248-266`
//! is the shape) — so [`test_case_result_to_sdk`] below, and
//! `ResultsRepository::list_case_page`, are that task's. Task 21's
//! `attach_case_summary` port is a later consumer.
//!
//! **What Task 12 deferred turned out to be smaller than it looked**, and worth
//! recording because the deferral was argued at length: the conversion is a
//! ten-field move with no fallible column — no enum to decode, no JSON to parse,
//! no width to widen — so it needed neither a `Result` nor a `CorruptState`
//! variant. `paginate_odata` rather than `paginate_odata_try` follows directly
//! (see [`test_case_result_to_sdk`]).
//!
//! ## `db_err` used to live here, and no longer does
//!
//! It sat in this module from Task 12 to Task 17 because the three sibling gears
//! keep it in `infra/storage/db.rs`, that file's other content is the `OData`
//! collections, and those did not exist yet — founding a file for one three-line
//! function would have put it in the tree ahead of its own module doc. **Task 17
//! created [`super::db`] and moved it**, which is what this section and
//! `infra::storage`'s header both said should happen. Nothing here constructs
//! [`DomainError::Database`] any more; this module's own failure modes are
//! [`DomainError::CorruptState`] for a stored value it will not guess at and
//! [`DomainError::Validation`] for the two caller inputs it refuses, both of
//! which are listed at the top of this header.

use qa_insights_sdk::{
    CollectCount, JiraBug, JiraConfig, JiraPollerConfig, NotificationConfig, NotificationLogEntry,
    SavedView, SavedViewScope, ScheduledRunSlackTemplate, ScheduledRunSlackTemplates,
    TestCaseResultRecord, TestResultRecord,
};
use sea_orm::entity::prelude::Json;
use std::fmt::Display;
use uuid::Uuid;

use crate::domain::analytics::{CaseRow, ExecRow};
use crate::domain::error::DomainError;
use crate::domain::repos::{PlanExecRow, Watermarks};
use crate::infra::storage::entity::{
    ingest_watermark, jira_bug, jira_config, jira_poller_config, notification_config,
    notification_log, saved_view, test_case_collect, test_case_result, test_result,
};

/// A stored value this layer refuses to guess at.
fn corrupt(what: &'static str, id: Uuid, value: impl Display) -> DomainError {
    DomainError::CorruptState {
        what,
        id,
        value: value.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Column widths, and the helper that makes producer text fit them
// ---------------------------------------------------------------------------

/// `qa_test_results.status` / `qa_test_case_results.status` — `VARCHAR(16)`.
pub(crate) const MAX_STATUS: usize = 16;
/// `qa_test_results.{test_file,plan_path}`,
/// `qa_test_case_results.{test_file,nodeid}`,
/// `qa_test_case_collect.test_file` — `VARCHAR(1024)`.
pub(crate) const MAX_PATH: usize = 1024;
/// `qa_test_results.{test_name,branch}`, `qa_test_case_results.name`,
/// `qa_test_case_collect.branch` — `VARCHAR(512)`.
pub(crate) const MAX_NAME: usize = 512;
/// `qa_test_results.duration`, `qa_test_case_results.duration` — `VARCHAR(64)`.
pub(crate) const MAX_DURATION: usize = 64;
/// `qa_test_results.launch_id`, `qa_test_results.product_version`,
/// `qa_test_results.app_build` — `VARCHAR(255)`.
pub(crate) const MAX_SHORT_TEXT: usize = 255;
/// `qa_test_results.jira_key`, `qa_test_case_results.ticket` — `VARCHAR(64)`.
pub(crate) const MAX_KEY: usize = 64;

/// Make a producer-supplied value fit its column, so the column can never
/// reject it. See this module's header for why truncating beats rejecting on
/// the ingest path, and why it does not generalise off it.
///
/// By `char`, not by byte: the columns are measured in characters and slicing a
/// byte index would panic mid-codepoint.
pub(crate) fn truncate(value: String, max: usize, column: &'static str) -> String {
    // Counted once. Both the guard and the log line need it, and `chars().count()`
    // is a full walk of the string — on the hot ingest path, for every column of
    // every row.
    let len = value.chars().count();
    if len <= max {
        return value;
    }
    let truncated: String = value.chars().take(max).collect();
    tracing::warn!(
        column,
        original_len = len,
        max,
        "an ingested value exceeded its column width and was truncated; the \
         producer and this schema have diverged"
    );
    truncated
}

/// [`truncate`] over an optional column.
pub(crate) fn truncate_opt(
    value: Option<String>,
    max: usize,
    column: &'static str,
) -> Option<String> {
    value.map(|v| truncate(v, max, column))
}

// ---------------------------------------------------------------------------
// Widening helpers
// ---------------------------------------------------------------------------

/// Widen an `INTEGER` column into an unsigned contract type, failing closed on
/// a negative value.
///
/// The seam exists because the workspace denies `clippy::cast_sign_loss`, and
/// `as` would be wrong anyway: a wrapped negative becomes an enormous positive
/// that inflates the number it feeds and fails nothing. Same shape as qa-runs'
/// `usize_from_db` and qa-catalog's `u64_from_db`.
fn u32_from_db(value: i32, what: &'static str, id: Uuid) -> Result<u32, DomainError> {
    u32::try_from(value).map_err(|_| corrupt(what, id, value))
}

/// As [`u32_from_db`], for the `INTEGER` SMTP port against the SDK's `u16`.
fn u16_from_db(value: i32, what: &'static str, id: Uuid) -> Result<u16, DomainError> {
    u16::try_from(value).map_err(|_| corrupt(what, id, value))
}

/// As [`u32_from_db`], for a `BIGINT` column against a contract `u64`.
///
/// **The seam `infra::storage::entity::jira_poller_config` names Task 32 as the
/// owner of.** `poll_interval_seconds` is `BIGINT`/`i64` in the column and `u64`
/// in [`JiraPollerConfig`], and the conversion fails closed for a sharper reason
/// than the sign lint: a wrapped negative becomes an interval of some hundreds
/// of millions of years, so a poller that read it would sleep once and never
/// poll again — a silent stop, not a visible wrong number.
///
/// Legacy's `.max(1)` clamp (`manager/src/services/jira_poller.rs:26`) is
/// deliberately **not** here. This function's job is to say whether the stored
/// value is representable; deciding that zero is unusable belongs to whoever
/// sleeps, and `domain::service::jira::JiraService::poller_config` is where it
/// lives. A clamp here would also make the settings screen echo back a value the
/// tenant never saved.
fn u64_from_db(value: i64, what: &'static str, id: Uuid) -> Result<u64, DomainError> {
    u64::try_from(value).map_err(|_| corrupt(what, id, value))
}

/// Narrow a contract count into its `INTEGER` column.
///
/// The write half of [`u32_from_db`]'s seam, and
/// [`DomainError::Validation`] rather than `CorruptState` because it is *caller*
/// input rather than a stored row — the same split qa-runs draws between
/// `usize_from_db` and `db_i32_from_i64`.
///
/// **Refuses rather than clamps.** A clamped `case_count` is a wrong "expected
/// cases" number on a screen that still renders, which is exactly the failure
/// `u32_from_db` fails closed to avoid on the way back out; clamping here would
/// reintroduce it from the other side.
///
/// # Errors
///
/// [`DomainError::Validation`] if the value does not fit an `i32`.
pub(crate) fn db_i32_from_u32(value: u32, field: &str) -> Result<i32, DomainError> {
    i32::try_from(value).map_err(|_| DomainError::Validation {
        field: field.to_owned(),
        message: format!("must be within 0..={}", i32::MAX),
    })
}

// ---------------------------------------------------------------------------
// `plan_key` — obligation #2 of the schema
// ---------------------------------------------------------------------------

/// The materialized `COALESCE(plan_id, '')` that
/// `idx_qa_analytics_saved_views_unique` keys on.
///
/// **The one function in this gear whose absence from a call site is invisible.**
/// `qa_analytics_saved_views.plan_key` is `NOT NULL DEFAULT ''`, so a writer
/// that forgets it compiles, inserts, and silently collides a plan-scoped view
/// with the owner's *global* view of the same name — a wrong 409 with no error
/// anywhere. It exists as a named function, rather than inline at the three call
/// sites that need it, so that the read probe
/// (`find_by_natural_key`) and the two writers cannot derive it differently:
/// a probe that disagreed with what a write would produce is precisely the
/// failure obligation #2 is about.
///
/// `""` when the plan identity is absent, otherwise `"<repo_id>/<plan_path>"`.
/// A half-present identity — one of the two `None` — is also `""`: the pair is
/// one value, and a key built from half of it would be a third spelling of
/// "global".
pub(crate) fn plan_key(repo_id: Option<Uuid>, plan_path: Option<&str>) -> String {
    match (repo_id, plan_path) {
        (Some(repo_id), Some(plan_path)) => format!("{repo_id}/{plan_path}"),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// `qa_test_results` → the contract record.
///
/// `tenant_id` and `updated_at` have no contract field: tenancy is the caller's
/// `AccessScope`, not something a consumer reads off a row, and `updated_at`
/// moves on any write while `TestResultRecord` models an outcome that happened
/// once.
pub(crate) fn test_result_to_sdk(m: test_result::Model) -> TestResultRecord {
    TestResultRecord {
        id: m.id,
        run_id: m.run_id,
        test_file: m.test_file,
        test_name: m.test_name,
        status: m.status,
        duration: m.duration,
        launch_id: m.launch_id,
        jira_key: m.jira_key,
        product_version: m.product_version,
        app_build: m.app_build,
        environment_id: m.environment_id,
        repo_id: m.repo_id,
        plan_path: m.plan_path,
        branch: m.branch,
        run_finished_at: m.run_finished_at,
        run_created_at: m.run_created_at,
        created_at: m.created_at,
    }
}

/// `qa_test_case_results` → the contract record.
///
/// Sibling of [`test_result_to_sdk`], and the same two columns are dropped for
/// the same two reasons: `tenant_id` is the caller's `AccessScope` rather than
/// something a consumer reads off a row, and `updated_at` moves on any write
/// while `TestCaseResultRecord` models an outcome that happened once.
///
/// # Infallible, and that is why the collection uses `paginate_odata`
///
/// Ten fields, every one of them a move or a `Copy`. Nothing here decodes: the
/// `status` column is deliberately *not* decoded (this module's note on the open
/// status set), and unlike `saved_view.scope` or `test_case_collect.case_count`
/// there is no enum and no narrowed integer to refuse. So there is no
/// [`DomainError`] this function could return, `ResultsRepository::list_case_page`
/// calls `paginate_odata` rather than `paginate_odata_try` — which is where
/// qa-runs' `list_page` differs, because `run_to_sdk` *is* fallible — and a
/// `Result` here would be a wrapper nobody could ever populate.
///
/// **The spellings differ from the file-level table's and that is deliberate**:
/// this table's function-name column is `name`, not `test_name`, matching legacy
/// (`manager/migrations/001_initial.sql:258`). `qa_insights_sdk`'s field keeps
/// the same spelling so the two result shapes stay visibly different, and the
/// pairing is checked by
/// `results_sea_repo::tests::the_case_collection_maps_every_column_to_its_own_field`
/// — a transposition of `name` and `nodeid`, or of `reason` and `ticket`, is two
/// `String`/`Option<String>` fields swapping places and compiles clean.
pub(crate) fn test_case_result_to_sdk(m: test_case_result::Model) -> TestCaseResultRecord {
    TestCaseResultRecord {
        id: m.id,
        run_id: m.run_id,
        test_file: m.test_file,
        nodeid: m.nodeid,
        name: m.name,
        status: m.status,
        duration: m.duration,
        reason: m.reason,
        ticket: m.ticket,
        created_at: m.created_at,
    }
}

/// `qa_test_case_results` → the four columns the per-case roll-up folds.
///
/// The in-memory half of legacy's own narrow projection
/// (`manager/src/routes/analytics.rs:1308-1319`, four columns of ten) — see
/// [`CaseRow`] for why the analytics side gets its own row type rather than
/// folding [`TestCaseResultRecord`](qa_insights_sdk::TestCaseResultRecord).
///
/// Six of the ten columns are dropped here rather than in the `SELECT`, which is
/// the one thing this function does *not* buy: `SecureORM` reads the whole
/// entity, so the saving is a copy and not a fetch. Kept as a mapper anyway,
/// because the alternative is `build_case_data` learning the entity type — and
/// `status` in particular must cross unvalidated, which is this module's one
/// deliberate exception and is stated in its header.
pub(crate) fn case_row_from_result(m: test_case_result::Model) -> CaseRow {
    CaseRow {
        run_id: m.run_id,
        test_file: m.test_file,
        status: m.status,
        ticket: m.ticket,
    }
}

/// `qa_test_results` → the analytics row the pure cores consume.
///
/// Three fields are *derived* rather than copied, and all three are legacy's:
///
/// * `build` ← `app_build`, carried as `Option` because legacy substitutes
///   `"unknown"` at the consumer (`analytics.rs:1032-1033`), not at the read.
/// * `ts` ← `run_finished_at ?? run_created_at`, legacy's `:1028`. The fallback
///   is load-bearing: a run still in progress has no finish instant, and legacy
///   buckets those rows by **the run's** creation time rather than dropping them.
///   Not the row's `created_at` — the body comment below says what that cost, and
///   this bullet said it until Task 21b's third fix round.
/// * `day` ← `ts`'s calendar date, legacy's `ts.date_naive()`.
///
/// Everything `qa_test_results` carries that legacy's `ExecRow` does not —
/// `duration`, `launch_id`, `jira_key`, `product_version`, `branch` — is
/// deliberately dropped. Both are *predicates* here, on
/// [`UniverseFilter`](crate::domain::analytics::UniverseFilter), not
/// projections of the row.
///
/// **`repo_id` and `plan_path` are not dropped, and this is the one place they
/// are filled.** Legacy's own `ExecRow::repo_id` is write-only — assigned and
/// read by nothing — which is exactly why an earlier pass of `ExecRow`'s
/// header declared the field dead and left it off this port too. It is not
/// dead here: `(tenant_id, product_id)` is not a unique index, so a product's
/// several repositories can each hold `tests/test_smoke.py`, and
/// `walk_repo_universe` only deduplicates *within* one repository — two such
/// repositories produce two universe entries sharing one `test_file`. Every
/// fold downstream must key on `(repo_id, test_file)` or it attributes one
/// repository's status to the other's test. The values are not fetched
/// specially: `m.repo_id`/`m.plan_path` are exactly the pair
/// [`UniverseFilter::plans`](crate::domain::analytics::UniverseFilter::plans)'s
/// predicate already filtered this row on, so filling them here is carrying
/// the query's own predicate forward.
///
/// Both columns are nullable — a row from a custom or collect run that names
/// no plan stores `NULL` in both, deliberately (`domain::service::ingest::plan_identity`).
/// `list_for_universe`'s scoped read only ever returns such a row when the
/// caller's plan filter is empty, which the analytics service never sends
/// (`domain::service::analytics::universe_filter` always derives `plans` from
/// the universe), so [`ExecRow::repo_id`] and
/// [`ExecRow::plan_path`](crate::domain::analytics::ExecRow::plan_path) default
/// to the nil `Uuid` and `""` rather than carry an `Option` no real caller can
/// produce; a row that hit that default resolves to nothing in
/// [`resolve_rows`](crate::domain::analytics::universe::resolve_rows) and is
/// dropped there, exactly as an out-of-universe row already is.
pub(crate) fn exec_row_from_result(m: test_result::Model) -> ExecRow {
    // The in-memory twin of `results_sea_repo::effective_ts`, which names this
    // function back and carries the both-NULL corner from the SQL side. It has to
    // stay the same expression: this value is what the analytics cores fold over,
    // while that one is what selected and ordered the row. Legacy's own pair is
    // `ORDER BY COALESCE(r.finished_at, r.created_at)`
    // (`manager/src/routes/analytics.rs:971`) and
    // `row.finished_at.unwrap_or(row.created_at)` (`:1028`) over a `SELECT` of
    // `r.finished_at, r.created_at` (`:961`; `:960` is the raw-string opener) —
    // both off `run_results`, so both fall back to the *run's* instant.
    //
    // `created_at` is the last resort only because `run_created_at` is nullable
    // and `ExecRow::ts` is not. It is unreachable today: ingest fills
    // `run_created_at` from `qa_runs_sdk::Run::created_at`, which is
    // non-optional. The SQL side sorts such a row as `NULL` instead, which is the
    // one corner where the two spellings differ, and it is a corner no writer can
    // reach.
    let ts = m
        .run_finished_at
        .or(m.run_created_at)
        .unwrap_or(m.created_at);
    ExecRow {
        run_id: m.run_id,
        repo_id: m.repo_id.unwrap_or_default(),
        plan_path: m.plan_path.unwrap_or_default(),
        test_file: m.test_file,
        test_name: m.test_name,
        status: m.status,
        build: m.app_build,
        environment_id: m.environment_id,
        ts,
        day: ts.date(),
    }
}

/// `qa_test_results` → [`PlanExecRow`], Task 27's plan-drill-down projection.
///
/// **Not [`exec_row_from_result`] with fields dropped and one renamed.**
/// [`PlanExecRow::version`] is `m.product_version`, not `m.app_build` —
/// [`PlanExecRow`]'s header records that legacy's three plan endpoints read
/// `r.app_version` where the overview's own build sections read `r.app_build`,
/// and the two are different columns. No `ts`/`day` derivation here: the
/// repository's own `ORDER BY` is what a caller of [`PlanExecRow`] must trust,
/// exactly as [`ExecRow`]'s header states for its own ordering.
pub(crate) fn plan_exec_row_from_result(m: test_result::Model) -> PlanExecRow {
    PlanExecRow {
        run_id: m.run_id,
        test_name: m.test_name,
        status: m.status,
        version: m.product_version,
        environment_id: m.environment_id,
        jira_key: m.jira_key,
    }
}

/// `qa_test_case_collect` → the contract count.
///
/// `id`, `tenant_id`, `created_at` and `updated_at` have no contract field:
/// `CollectCount` deliberately carries no surrogate key (the natural key is
/// `(repo_id, branch, test_file)` and nothing addresses a collect row any other
/// way), and `collected_at` is the timestamp that means something to a reader.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if `case_count` is negative.
pub(crate) fn collect_count_to_sdk(
    m: test_case_collect::Model,
) -> Result<CollectCount, DomainError> {
    Ok(CollectCount {
        repo_id: m.repo_id,
        branch: m.branch,
        test_file: m.test_file,
        case_count: u32_from_db(m.case_count, "test_case_collect.case_count", m.id)?,
        collected_at: m.collected_at,
    })
}

// ---------------------------------------------------------------------------
// Saved views
// ---------------------------------------------------------------------------

/// Decode the stored `scope` text.
///
/// The two spellings are `qa_insights_sdk::SavedViewScope::as_str`'s, which are
/// legacy's `scope_to_str` (`manager/src/routes/analytics.rs:2088-2093`). Fails
/// closed: a third value is a row this gear did not write, and guessing which
/// list it belongs in would put it in the wrong one.
fn saved_view_scope_from_str(value: &str, id: Uuid) -> Result<SavedViewScope, DomainError> {
    match value {
        "all" => Ok(SavedViewScope::All),
        "plan" => Ok(SavedViewScope::Plan),
        other => Err(corrupt("saved_view.scope", id, other)),
    }
}

/// `qa_analytics_saved_views` → the contract view.
///
/// `tenant_id` is omitted for the reason [`test_result_to_sdk`] gives.
/// **`plan_key` is omitted deliberately**: it is derived from `repo_id` and
/// `plan_path`, which are right there and readable, and surfacing it would let a
/// caller hold a value only the repository is allowed to compute.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if `scope` is not `all` or `plan`.
pub(crate) fn saved_view_to_sdk(m: saved_view::Model) -> Result<SavedView, DomainError> {
    Ok(SavedView {
        id: m.id,
        owner_id: m.owner_id,
        scope: saved_view_scope_from_str(&m.scope, m.id)?,
        repo_id: m.repo_id,
        plan_path: m.plan_path,
        name: m.name,
        // `query_json` is opaque in both directions: legacy binds and returns
        // the document whole in all six statements that touch it, and nothing in
        // this gear inspects it.
        //
        // **Not byte-for-byte verbatim, though — corrected 2026-08-20.**
        // `Json::to_string()` re-serializes, so insignificant whitespace is
        // dropped and object key order becomes `serde_json`'s (insertion order
        // as the parser saw it, or sorted under `preserve_order`). The
        // *document* round-trips; its formatting does not. That is the right
        // behaviour for a filter set nothing renders as text, and
        // `a_saved_views_query_json_round_trips_as_a_document_not_as_text` pins
        // both halves so a caller cannot come to depend on the formatting.
        query_json: m.query_json.to_string(),
        created_at: m.created_at,
        updated_at: m.updated_at,
    })
}

/// The SDK's verbatim JSON text → the `JSONB` column.
///
/// # Errors
///
/// [`DomainError::Validation`] if the text is not JSON. This is caller input,
/// not stored state, so it is a 400 rather than a `CorruptState`.
pub(crate) fn query_json_to_column(text: &str) -> Result<Json, DomainError> {
    serde_json::from_str(text).map_err(|error| DomainError::Validation {
        field: "query_json".to_owned(),
        message: format!("must be a JSON document: {error}"),
    })
}

// ---------------------------------------------------------------------------
// JIRA
// ---------------------------------------------------------------------------

/// `qa_jira_bugs` → the contract bug. `tenant_id` and `updated_at` are omitted
/// for the reasons [`test_result_to_sdk`] gives.
///
/// `status` crosses as a `String`: it is free JIRA workflow text and can also
/// come back from the JIRA API (`check_jira_status`,
/// `manager/src/services/jira.rs:254`), so it is an open set like the result
/// statuses.
pub(crate) fn jira_bug_to_sdk(m: jira_bug::Model) -> JiraBug {
    JiraBug {
        id: m.id,
        jira_key: m.jira_key,
        test_name: m.test_name,
        repo_id: m.repo_id,
        plan_path: m.plan_path,
        app_version: m.app_version,
        environment_id: m.environment_id,
        status: m.status,
        summary: m.summary,
        created_at: m.created_at,
        resolved_at: m.resolved_at,
    }
}

/// `qa_jira_config` → the contract settings.
///
/// `id`, `tenant_id`, `created_at` and `updated_at` have no contract field, for
/// [`notification_config_to_sdk`]'s reason: the row is a per-tenant singleton
/// addressed by its tenant.
///
/// **Returns [`JiraConfig::api_token_credstore_ref`] and never token
/// material** — obligation #3 of the schema, and the reason this conversion is
/// infallible where a masking step would not be: there is nothing to mask,
/// because the column holds a reference. Legacy's `api_get_jira` overwrites the
/// token with `"********"` on the way out
/// (`manager/src/routes/settings.rs:254-259`), which is a mitigation for storing
/// the material in the first place.
pub(crate) fn jira_config_to_sdk(m: jira_config::Model) -> JiraConfig {
    JiraConfig {
        url: m.url,
        project_key: m.project_key,
        email: m.email,
        api_token_credstore_ref: m.api_token_credstore_ref,
        issue_type: m.issue_type,
        enabled: m.enabled,
    }
}

/// `qa_jira_poller_config` → the contract settings.
///
/// The one fallible field is the interval; [`u64_from_db`] carries why it fails
/// closed and why the `.max(1)` clamp is not applied here.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if `poll_interval_seconds` is negative.
pub(crate) fn jira_poller_config_to_sdk(
    m: &jira_poller_config::Model,
) -> Result<JiraPollerConfig, DomainError> {
    Ok(JiraPollerConfig {
        poll_interval_seconds: u64_from_db(
            m.poll_interval_seconds,
            "jira_poller_config.poll_interval_seconds",
            m.id,
        )?,
        auto_rerun_on_resolve: m.auto_rerun_on_resolve,
    })
}

/// The contract interval narrowed back into its `BIGINT` column.
///
/// The write half of [`u64_from_db`]'s seam, and [`DomainError::Validation`]
/// rather than `CorruptState` for [`db_i32_from_u32`]'s reason: it is caller
/// input, not a stored row. **Refuses rather than clamps**, again for that
/// function's reason — an interval silently changed on save is a settings screen
/// that lies about what it stored.
///
/// # Errors
///
/// [`DomainError::Validation`] if the value does not fit an `i64`.
pub(crate) fn db_i64_from_u64(value: u64, field: &str) -> Result<i64, DomainError> {
    i64::try_from(value).map_err(|_| DomainError::Validation {
        field: field.to_owned(),
        message: format!("must be within 0..={}", i64::MAX),
    })
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

/// One `ScheduledRunSlackTemplate` out of the stored document.
///
/// A missing key is [`ScheduledRunSlackTemplate::default`], matching legacy's
/// `#[serde(default)]` on both structs (`manager/src/models.rs:1061`, `:1089`).
/// A key of the **wrong type** is corruption and fails closed — legacy's serde
/// would reject it too, with a 500.
///
/// Legacy's `#[serde(alias = "message")]` on `body` (`:1068`) is *not* honoured
/// here, and that is the right seam: an alias is a wire concern, this gear is
/// the only writer of this column, and `qa_insights_sdk`'s own note says the
/// alias belongs to the REST DTO.
fn slack_template_from_json(
    value: Option<&Json>,
    what: &'static str,
    id: Uuid,
) -> Result<ScheduledRunSlackTemplate, DomainError> {
    let Some(value) = value else {
        return Ok(ScheduledRunSlackTemplate::default());
    };
    if value.is_null() {
        return Ok(ScheduledRunSlackTemplate::default());
    }
    let obj = value.as_object().ok_or_else(|| corrupt(what, id, value))?;

    let flag = |key: &str| -> Result<bool, DomainError> {
        match obj.get(key) {
            None | Some(Json::Null) => Ok(false),
            Some(v) => v.as_bool().ok_or_else(|| corrupt(what, id, v)),
        }
    };
    let text = |key: &str| -> Result<Option<String>, DomainError> {
        match obj.get(key) {
            None | Some(Json::Null) => Ok(None),
            Some(v) => v
                .as_str()
                .map(|s| Some(s.to_owned()))
                .ok_or_else(|| corrupt(what, id, v)),
        }
    };

    Ok(ScheduledRunSlackTemplate {
        enabled: flag("enabled")?,
        status_icon: text("status_icon")?,
        header: text("header")?,
        summary: text("summary")?,
        results: text("results")?,
        body: text("body")?,
        footer: text("footer")?,
    })
}

/// The six templates out of the stored document.
///
/// The six keys are the six `snake_case` event tokens, and matching that
/// spelling is load-bearing for the same reason
/// `ScheduledRunSlackTemplates::template_for` says: `InProgress` lowercased is
/// `inprogress`, which nothing recognises, and a settings row written that way
/// is a subscription that silently never fires.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if the column is not a JSON object, or if any
/// template field holds the wrong JSON type.
fn slack_templates_from_json(
    value: &Json,
    id: Uuid,
) -> Result<ScheduledRunSlackTemplates, DomainError> {
    let what = "notification_config.scheduled_run_slack_templates";
    if value.is_null() {
        return Ok(ScheduledRunSlackTemplates::default());
    }
    let obj = value.as_object().ok_or_else(|| corrupt(what, id, value))?;
    Ok(ScheduledRunSlackTemplates {
        pending: slack_template_from_json(obj.get("pending"), what, id)?,
        in_progress: slack_template_from_json(obj.get("in_progress"), what, id)?,
        succeeded: slack_template_from_json(obj.get("succeeded"), what, id)?,
        failed: slack_template_from_json(obj.get("failed"), what, id)?,
        error: slack_template_from_json(obj.get("error"), what, id)?,
        skipped: slack_template_from_json(obj.get("skipped"), what, id)?,
    })
}

/// One template into the stored document.
///
/// `serde_json::json!` is not reachable: the SDK is `serde`-free by design (its
/// module header states it), so neither struct derives `Serialize` and the
/// document is built by hand. The six keys and seven fields below are the
/// encoding half of [`slack_templates_from_json`]; the round-trip test
/// `notify_sea_repo::tests::saving_a_config_round_trips_every_field_including_the_templates`
/// is what keeps the two halves in step, because nothing else can.
fn slack_template_to_json(t: &ScheduledRunSlackTemplate) -> Json {
    let mut map = serde_json::Map::new();
    map.insert("enabled".to_owned(), Json::Bool(t.enabled));
    for (key, value) in [
        ("status_icon", t.status_icon.as_ref()),
        ("header", t.header.as_ref()),
        ("summary", t.summary.as_ref()),
        ("results", t.results.as_ref()),
        ("body", t.body.as_ref()),
        ("footer", t.footer.as_ref()),
    ] {
        map.insert(
            key.to_owned(),
            value.map_or(Json::Null, |v| Json::String(v.clone())),
        );
    }
    Json::Object(map)
}

/// The six templates into the stored document. Encoding half of
/// [`slack_templates_from_json`].
pub(crate) fn slack_templates_to_json(t: &ScheduledRunSlackTemplates) -> Json {
    let mut map = serde_json::Map::new();
    for (key, value) in [
        ("pending", &t.pending),
        ("in_progress", &t.in_progress),
        ("succeeded", &t.succeeded),
        ("failed", &t.failed),
        ("error", &t.error),
        ("skipped", &t.skipped),
    ] {
        map.insert(key.to_owned(), slack_template_to_json(value));
    }
    Json::Object(map)
}

/// `qa_notification_config` → the contract settings.
///
/// `id`, `tenant_id`, `created_at` and `updated_at` have no contract field:
/// this is a per-tenant singleton addressed by its tenant, so a surrogate key
/// on the contract would be a value with no operation that takes it.
///
/// Returns [`NotificationConfig::slack_webhook_credstore_ref`] and never a
/// webhook URL — obligation #3 of the schema. The column holds a
/// credential-store reference and the material never enters this gear.
///
/// # Errors
///
/// [`DomainError::CorruptState`] if `email_smtp_port` is outside `u16`, or if
/// the templates document does not decode.
pub(crate) fn notification_config_to_sdk(
    m: notification_config::Model,
) -> Result<NotificationConfig, DomainError> {
    Ok(NotificationConfig {
        slack_webhook_credstore_ref: m.slack_webhook_credstore_ref,
        slack_channel: m.slack_channel,
        manager_ui_base_url: m.manager_ui_base_url,
        slack_enabled: m.slack_enabled,
        notify_on_failure: m.notify_on_failure,
        notify_on_success: m.notify_on_success,
        notify_on_schedule_completion: m.notify_on_schedule_completion,
        scheduled_run_slack_enabled: m.scheduled_run_slack_enabled,
        scheduled_run_slack_templates: slack_templates_from_json(
            &m.scheduled_run_slack_templates,
            m.id,
        )?,
        run_queue_queued_slack_enabled: m.run_queue_queued_slack_enabled,
        email_smtp_host: m.email_smtp_host,
        email_smtp_port: u16_from_db(
            m.email_smtp_port,
            "notification_config.email_smtp_port",
            m.id,
        )?,
        email_smtp_username: m.email_smtp_username,
        email_smtp_credstore_ref: m.email_smtp_credstore_ref,
        email_from: m.email_from,
        email_recipients: m.email_recipients,
        email_enabled: m.email_enabled,
    })
}

/// `qa_notification_log` → the contract entry. `tenant_id` and `updated_at` are
/// omitted for the reasons [`test_result_to_sdk`] gives.
pub(crate) fn notification_log_to_sdk(m: notification_log::Model) -> NotificationLogEntry {
    NotificationLogEntry {
        id: m.id,
        created_at: m.created_at,
        run_id: m.run_id,
        channel: m.channel,
        event_type: m.event_type,
        outcome: m.outcome,
        detail: m.detail,
    }
}

// ---------------------------------------------------------------------------
// Watermarks
// ---------------------------------------------------------------------------

/// `qa_ingest_watermarks` → the two marks.
///
/// `id`, `tenant_id`, `created_at` and `updated_at` have no domain field: the
/// row is a per-tenant singleton and `Watermarks` is the pair of marks, nothing
/// else. A caller that has no row at all gets [`Watermarks::default`], which is
/// the same value — see the trait's note on why "no row" and "row with nulls"
/// must be indistinguishable.
pub(crate) fn watermarks_from_row(m: &ingest_watermark::Model) -> Watermarks {
    Watermarks {
        last_reconciled_finished_at: m.last_reconciled_finished_at,
        last_swept_at: m.last_swept_at,
    }
}

/// Mapper tests.
///
/// **This module had none until 2026-08-20**, in a file whose header opens "Every
/// decoder here fails closed" — so every fail-closed path was prose. A crate-wide
/// `grep CorruptState src/` returned only the enum variant and doc comments:
/// nothing constructed or asserted one. `qa-runs`' `mapper.rs` carries the
/// equivalent module and asserts its own `usize_from_db(-1)` → `CorruptState`,
/// which is the pattern followed here.
///
/// Everything below is a pure function over owned values, so none of it needs a
/// database — which is also why its absence was cheap to fix and expensive to
/// leave.
#[cfg(test)]
mod tests {
    use super::{
        MAX_PATH, MAX_STATUS, db_i32_from_u32, notification_config_to_sdk, plan_key,
        query_json_to_column, saved_view_scope_from_str, slack_templates_from_json,
        slack_templates_to_json, truncate, truncate_opt, u16_from_db, u32_from_db,
    };
    use crate::domain::error::DomainError;
    use crate::infra::storage::entity::notification_config;
    use qa_insights_sdk::{ScheduledRunSlackTemplate, ScheduledRunSlackTemplates};
    use sea_orm::entity::prelude::Json;
    use time::macros::datetime;
    use uuid::Uuid;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    // ---------------------------------------------------------------------
    // Fail-closed decoders
    // ---------------------------------------------------------------------

    /// The `scope` column's third value is corruption, not a default.
    ///
    /// A permissive fallback here would put a saved view in the wrong list and
    /// collide it with the wrong name — the failure `SavedViewNameExists` reports
    /// for a reason the operator could not act on.
    #[test]
    fn an_unrecognized_saved_view_scope_is_corrupt_state() {
        assert!(matches!(
            saved_view_scope_from_str("all", uuid(1)),
            Ok(qa_insights_sdk::SavedViewScope::All)
        ));
        assert!(matches!(
            saved_view_scope_from_str("plan", uuid(1)),
            Ok(qa_insights_sdk::SavedViewScope::Plan)
        ));

        let err = saved_view_scope_from_str("ALL", uuid(7)).expect_err("must fail closed");
        match err {
            DomainError::CorruptState { what, id, value } => {
                assert_eq!(what, "saved_view.scope");
                assert_eq!(id, uuid(7), "the error must name the offending row");
                assert_eq!(value, "ALL");
            }
            other => panic!("expected CorruptState, got {other:?}"),
        }
    }

    /// A negative `case_count` must not wrap into an enormous positive: that
    /// number reaches the analytics surface as "expected cases".
    #[test]
    fn a_negative_stored_count_is_corrupt_state() {
        assert_eq!(u32_from_db(7, "x", uuid(1)).unwrap(), 7);
        let err = u32_from_db(-1, "test_case_collect.case_count", uuid(9))
            .expect_err("a negative count must fail closed");
        assert!(
            matches!(&err, DomainError::CorruptState { value, .. } if value == "-1"),
            "got {err:?}"
        );
    }

    /// The SMTP port is `INTEGER` in the column and `u16` on the contract, so both
    /// ends of the range are refusals rather than wraps.
    #[test]
    fn an_out_of_range_stored_smtp_port_is_corrupt_state() {
        assert_eq!(u16_from_db(587, "x", uuid(1)).unwrap(), 587);
        for bad in [-1, 65_536] {
            let err = u16_from_db(bad, "notification_config.email_smtp_port", uuid(3))
                .expect_err("out of range must fail closed");
            // `what`, `id` and `value` are all inspected: a decoder that named the
            // wrong column, or lost the offending value, passes a bare
            // `matches!(.., CorruptState { .. })`.
            match err {
                DomainError::CorruptState { what, id, value } => {
                    assert_eq!(what, "notification_config.email_smtp_port");
                    assert_eq!(id, uuid(3));
                    assert_eq!(value, bad.to_string());
                }
                other => panic!("expected CorruptState, got {other:?}"),
            }
        }
    }

    /// The write half of the same seam refuses rather than clamps, and is
    /// `Validation` rather than `CorruptState` because it is *caller* input.
    #[test]
    fn an_out_of_range_incoming_count_is_a_validation_error() {
        assert_eq!(db_i32_from_u32(9, "case_count").unwrap(), 9);
        let err = db_i32_from_u32(u32::MAX, "case_count").expect_err("must refuse");
        assert!(
            matches!(err, DomainError::Validation { ref field, .. } if field == "case_count"),
            "a clamped count would be a wrong number on a screen that still \
             renders; got {err:?}"
        );
    }

    /// `query_json` that is not JSON is the caller's fault, so `Validation` — a
    /// 400, not a 500.
    #[test]
    fn a_non_json_query_is_a_validation_error() {
        assert!(query_json_to_column(r#"{"version":"5.0.1"}"#).is_ok());
        let err = query_json_to_column("not json at all").expect_err("must refuse");
        assert!(
            matches!(err, DomainError::Validation { ref field, .. } if field == "query_json"),
            "got {err:?}"
        );
    }

    /// A templates column that is not an object, or whose fields hold the wrong
    /// JSON type, is corruption — `NULL`/absent is not.
    #[test]
    fn a_malformed_templates_document_is_corrupt_state() {
        assert_eq!(
            slack_templates_from_json(&Json::Null, uuid(1)).unwrap(),
            ScheduledRunSlackTemplates::default(),
            "an absent document is the defaults, not an error"
        );
        assert_eq!(
            slack_templates_from_json(&serde_json::json!({}), uuid(1)).unwrap(),
            ScheduledRunSlackTemplates::default(),
            "and so is an empty one -- legacy's #[serde(default)]"
        );

        // Each fixture pairs the malformed document with the JSON fragment the
        // error must quote back, so a decoder that named the wrong column or
        // swallowed the value cannot pass.
        for (bad, expected_value) in [
            (serde_json::json!("a string"), "\"a string\""),
            (serde_json::json!({ "pending": 7 }), "7"),
            (
                serde_json::json!({ "pending": { "enabled": "yes" } }),
                "\"yes\"",
            ),
            (serde_json::json!({ "pending": { "header": 3 } }), "3"),
        ] {
            let err = slack_templates_from_json(&bad, uuid(4))
                .expect_err("a wrong-typed field must fail closed");
            match err {
                DomainError::CorruptState { what, id, value } => {
                    assert_eq!(
                        what, "notification_config.scheduled_run_slack_templates",
                        "the error must name the column, not a nested field"
                    );
                    assert_eq!(id, uuid(4));
                    assert_eq!(
                        value, expected_value,
                        "and must quote the offending fragment back"
                    );
                }
                other => panic!("expected CorruptState, got {other:?}"),
            }
        }
    }

    /// The two hand-written halves of the templates codec must agree, including
    /// the `snake_case` keys — `in_progress`, not `inProgress`.
    #[test]
    fn the_templates_codec_round_trips_through_json() {
        let template = |icon: &str| ScheduledRunSlackTemplate {
            enabled: true,
            status_icon: Some(icon.to_owned()),
            header: None,
            summary: Some("{{summary}}".to_owned()),
            results: None,
            body: Some("{{message}}".to_owned()),
            footer: None,
        };
        let templates = ScheduledRunSlackTemplates {
            pending: template(":clock1:"),
            in_progress: template(":runner:"),
            succeeded: template(":white_check_mark:"),
            failed: template(":x:"),
            error: template(":boom:"),
            skipped: template(":fast_forward:"),
        };

        let encoded = slack_templates_to_json(&templates);
        assert!(
            encoded.get("in_progress").is_some(),
            "the key is snake_case; `inProgress` is a subscription that silently \
             never fires: {encoded}"
        );
        assert_eq!(
            slack_templates_from_json(&encoded, uuid(1)).unwrap(),
            templates
        );
    }

    /// The fifteen-field settings decoder, driven straight off a row.
    #[test]
    fn a_config_row_decodes_every_field() {
        let row = notification_config::Model {
            id: uuid(1),
            tenant_id: uuid(2),
            slack_webhook_credstore_ref: "cred://hook".to_owned(),
            slack_channel: "#qa".to_owned(),
            manager_ui_base_url: "https://qa.example".to_owned(),
            slack_enabled: true,
            notify_on_failure: false,
            notify_on_success: true,
            notify_on_schedule_completion: true,
            scheduled_run_slack_enabled: true,
            scheduled_run_slack_templates: serde_json::json!({}),
            run_queue_queued_slack_enabled: true,
            email_smtp_host: "smtp.example".to_owned(),
            email_smtp_port: 2525,
            email_smtp_username: "qa@example".to_owned(),
            email_smtp_credstore_ref: "qa-smtp-password".to_owned(),
            email_from: "qa@example".to_owned(),
            email_recipients: "a@example, b@example".to_owned(),
            email_enabled: true,
            created_at: datetime!(2026-08-18 00:00:00 UTC),
            updated_at: datetime!(2026-08-18 00:00:00 UTC),
        };
        let config = notification_config_to_sdk(row).unwrap();
        assert_eq!(config.slack_webhook_credstore_ref, "cred://hook");
        assert_eq!(config.email_smtp_port, 2525);
        assert_eq!(config.email_smtp_username, "qa@example");
        assert_eq!(
            config.email_smtp_credstore_ref, "qa-smtp-password",
            "the reference crosses the mapper; the password never does"
        );
        assert!(!config.notify_on_failure, "the one gate legacy starts ON");
        assert_eq!(config.email_recipients, "a@example, b@example");
    }

    // ---------------------------------------------------------------------
    // `plan_key` — obligation #2's derivation
    // ---------------------------------------------------------------------

    /// The derivation both writers and the read probe share.
    ///
    /// A half-present plan identity is `""`, the same as absent: the pair is one
    /// value, and a key built from half of it would be a third spelling of
    /// "global" that neither a write nor a probe would agree on.
    #[test]
    fn plan_key_is_empty_unless_both_halves_are_present() {
        let repo = uuid(0x12);
        assert_eq!(
            plan_key(Some(repo), Some("plans/smoke/plan.yaml")),
            format!("{repo}/plans/smoke/plan.yaml")
        );
        assert_eq!(plan_key(None, None), "");
        assert_eq!(plan_key(Some(repo), None), "", "half an identity is global");
        assert_eq!(plan_key(None, Some("plans/smoke/plan.yaml")), "");
    }

    // ---------------------------------------------------------------------
    // Truncation
    // ---------------------------------------------------------------------

    /// **Truncation is by `char`, not by byte, and a multi-byte fixture is the
    /// only thing that shows it.**
    ///
    /// Added 2026-08-20: every truncation fixture in the crate was pure ASCII, so
    /// swapping `chars().take(max)` for `&value[..max]` left the suite green and
    /// would have panicked the first time a runner reported a non-ASCII path —
    /// which pytest does whenever a test id contains one.
    ///
    /// The fixture is built so the byte slice would land **mid-codepoint**: a
    /// three-byte `€` straddles byte index `MAX_STATUS`.
    #[test]
    fn truncation_counts_chars_not_bytes() {
        // 15 ASCII chars, then U+20AC EURO SIGN (3 bytes in UTF-8), so byte
        // index 16 falls *inside* that codepoint.
        //
        // Written as `\u{20ac}` rather than as the character, because the
        // workspace denies `clippy::non_ascii_literal` — which is very likely why
        // no multi-byte fixture existed in this module until now: the lint makes
        // the obvious spelling fail the build, and the escape is easy to not
        // bother with.
        let euro = '\u{20ac}';
        let value = format!("{}{euro}xx", "A".repeat(MAX_STATUS - 1));
        assert!(
            !value.is_char_boundary(MAX_STATUS),
            "the fixture must straddle a codepoint at the truncation point, or \
             this test proves nothing"
        );
        assert!(value.chars().count() > MAX_STATUS);

        let out = truncate(value, MAX_STATUS, "qa_test_results.status");
        assert_eq!(
            out.chars().count(),
            MAX_STATUS,
            "the result must be exactly the column width in *characters*"
        );
        assert!(
            out.ends_with(euro),
            "the boundary character must survive whole: {out}"
        );
    }

    /// A multi-byte path whose truncation point lands **inside** a codepoint.
    ///
    /// **Rebuilt 2026-08-20; the first version did not straddle.** It used a
    /// six-byte prefix (`"tests/"`) followed by two-byte characters, so byte index
    /// `MAX_PATH` (1024 = 6 + 2*509) fell exactly *between* two of them —
    /// `&value[..1024]` would not have panicked there — and its second assertion,
    /// `is_char_boundary(out.len())`, is true for every string by definition
    /// (`str::is_char_boundary` returns `true` at `self.len()`). Two assertions,
    /// neither able to fail. The prefix is seven bytes now, which puts byte 1024
    /// mid-character; the first assertion below is the non-vacuity guard that keeps
    /// it that way.
    #[test]
    fn a_multibyte_path_truncates_inside_a_codepoint() {
        // U+0444 CYRILLIC SMALL LETTER EF, two bytes each. The seven-byte ASCII
        // prefix is what makes the parity work out so that byte MAX_PATH falls
        // *inside* one of them rather than between two.
        let value = format!("tests/x{}.py", "\u{444}".repeat(MAX_PATH));
        assert!(
            !value.is_char_boundary(MAX_PATH),
            "the fixture must straddle a codepoint at the byte index a naive \
             `&value[..MAX_PATH]` would slice, or this test proves nothing"
        );
        assert!(value.chars().count() > MAX_PATH);

        let out = truncate(value, MAX_PATH, "qa_test_results.test_file");
        assert_eq!(
            out.chars().count(),
            MAX_PATH,
            "the result is exactly the column width in *characters*"
        );
        // The byte length is the real discriminator: seven ASCII bytes plus
        // MAX_PATH-7 two-byte characters. A byte-sliced result would be MAX_PATH
        // bytes, if it did not panic first; this number is not that.
        assert_eq!(
            out.len(),
            7 + 2 * (MAX_PATH - 7),
            "a char-truncated result is longer in bytes than in chars; a length \
             equal to MAX_PATH would mean the slice was taken on bytes"
        );
    }

    /// A value that already fits is returned **untouched**, and a value of exactly
    /// the column width counts as fitting.
    ///
    /// **The boundary assertion compares heap pointers, and that is deliberate.**
    /// Mutating the guard from `len <= max` to `len < max` produces the identical
    /// *string* for an input of exactly `max` characters, so the obvious assertion
    /// (`out.len() == MAX_STATUS`) cannot see it — and the earlier version of this
    /// test could not: all 91 tests stayed green under that mutation. What the
    /// mutant does differently is re-collect the string and log a spurious
    /// truncation warning, which is exactly what this test's doc claims to rule
    /// out. `truncate` takes its `String` by value, so on the early-return path
    /// the heap buffer is *moved* and its address survives; the re-collecting path
    /// allocates a new buffer while the original is still alive, so the two
    /// addresses cannot coincide.
    #[test]
    fn a_value_that_fits_is_returned_without_reallocating() {
        assert_eq!(truncate("PASSED".to_owned(), MAX_STATUS, "x"), "PASSED");

        let exact = "A".repeat(MAX_STATUS);
        let before = exact.as_ptr();
        let out = truncate(exact, MAX_STATUS, "x");
        assert_eq!(out.chars().count(), MAX_STATUS);
        assert_eq!(
            out.as_ptr(),
            before,
            "exactly at the width is not over it: the buffer must be moved \
             through, not re-collected -- a re-collect here also logs a \
             truncation warning for a value that was never truncated"
        );
        assert_eq!(truncate_opt(None, MAX_STATUS, "x"), None);
        assert_eq!(
            truncate_opt(Some("x".repeat(MAX_STATUS + 5)), MAX_STATUS, "x")
                .map(|v| v.chars().count()),
            Some(MAX_STATUS)
        );
    }
}
