//! `OData` field allow-lists and column mappings for the two flat collections.
//!
//! # One enum, two consumers, and why that is the point
//!
//! Each field enum here is passed to **both** the route
//! (`OperationBuilder::with_odata_filter::<F>()` /
//! `with_odata_orderby::<F>()`, which is what publishes the filterable and
//! sortable fields in the `OpenAPI` document) and the repository
//! (`paginate_odata::<F, M, ..>`, which is what translates a filter into SQL).
//! Passing the same type to both is the only thing that stops the advertised
//! field set and the translatable field set drifting apart — a field advertised
//! but not mapped is a filter that fails at runtime, and a field mapped but not
//! advertised is a filter nobody knows exists. Shape and this argument are
//! `qa-runs/src/infra/storage/odata.rs`'s; what is copied is the *pattern* (two
//! enums, three trait impls each), because the filter engine itself lives in
//! `toolkit-db` and there is nothing else to copy.
//!
//! **The claim holds for `$filter` and has one measured exception for
//! `$orderby`.** `with_odata_orderby::<T>()` enumerates every `T::FIELDS` entry
//! and never consults [`FieldToColumn::is_orderable`]
//! (`libs/toolkit/src/api/operation_builder.rs:389-424`), while `paginate_odata`
//! rejects a non-orderable key (`sea_orm_filter.rs:699-705`). So the published
//! sort list is *wider* than the accepted one by exactly the fields this module
//! marks non-orderable — one of them, [`TestResultsField::RunFinishedAt`]. The
//! route's own description carries the caveat and
//! `api::rest::routes::tests::the_orderby_parameter_over_advertises_the_nullable_instant`
//! pins the discrepancy so the caveat cannot silently become false. Measured
//! 2026-08-20.
//!
//! Three ways out were considered, and the one that is *available inside this
//! gear* is named here because an earlier draft of this paragraph implied there
//! were only two:
//!
//! 1. **Drop `.with_odata_orderby()` from the route.** Rejected: it leaves the
//!    four genuinely sortable fields documented nowhere.
//! 2. **Drop `RunFinishedAt` from `FIELDS`.** Rejected: it removes the indexed
//!    time-window filter, which is the plan's Step 1 requirement and the only
//!    recency mechanism either collection has.
//! 3. **Declare a second, four-variant `FilterField` enum holding only the
//!    orderable fields, and pass *that* to `with_odata_orderby`** while
//!    `TestResultsField` keeps `with_odata_filter` and `paginate_odata`. This
//!    would publish exactly the accepted set, needs no change outside this crate,
//!    and is a real option rather than a straw man. Not taken, because its cost is
//!    the very failure mode this module exists to prevent: two enums over one
//!    table, kept in agreement by hand, where a field added to one and not the
//!    other is a silent drift — so it would need its own test tying them
//!    together, and it trades a documented over-advertisement for an
//!    undocumented-drift risk. Reconsider if a second non-orderable field appears,
//!    at which point the caveat is carrying more weight than one field.
//!
//! # Named parity gap: one test's history cannot be ordered newest-first
//!
//! **This is the requirement `cpt-cf-qa-fr-insights-history`'s discharge does
//! not cover, and nothing said so until Phase A's whole-phase review.** It is
//! recorded here rather than only in the endpoint description because the
//! description is written for a caller and this is a *parity* statement about
//! the port.
//!
//! Legacy answers "one test file's history, newest first, capped at N" in one
//! query: `WHERE tr.test_file = $1 ORDER BY COALESCE(rr.finished_at,
//! rr.created_at) DESC LIMIT $2` (`manager/src/routes/tests.rs:308-318`, read
//! 2026-08-21). `GET /qa/v1/test-results` can express the `WHERE` and cannot
//! express the `ORDER BY`:
//!
//! * [`TestResultsField::RunFinishedAt`] is filterable and **not orderable**,
//!   correctly — the cursor codec cannot encode a `NULL` and the column is
//!   `NULL` for every row of an unfinished run
//!   ([`TestResultsODataMapper::is_orderable`] carries the measurement).
//! * `created_at` is in neither enum, because no index covers it
//!   (`m20260818_000001_initial.rs:468`), and
//!   `results_sea_repo::tests::the_chronological_order_a_caller_would_reach_for_is_refused`
//!   pins the refusal.
//! * So the effective order is the default, `id` descending over a **random v4
//!   UUID** — stable, and arbitrary. A client rendering one test's history gets
//!   an arbitrary order and cannot ask for the newest N.
//!
//! What a caller can do today is filter a time window
//! (`$filter=run_finished_at ge …`) and sort client-side, which is adequate for
//! a bounded window and not for "the last 20 runs of this file".
//!
//! **The real fix, so that whoever owns it inherits the design rather than
//! rediscovering it:** a new migration adding
//! `(tenant_id, test_file, run_finished_at DESC)` — the schema is append-only,
//! so an existing migration cannot be edited — **plus** an orderable key guarded
//! on non-null, which is the harder half. The guard is what stops the pager
//! minting a cursor over a `NULL`: either a `NOT NULL` projection of the column
//! for ordering purposes, or an orderable-only-when-the-filter-excludes-nulls
//! rule, and neither exists in `toolkit-db` today. `COALESCE(run_finished_at,
//! created_at)` — legacy's own expression, and what `results_sea_repo`'s
//! analytics reads use — is **not** the fix here: no index covers it, so it buys
//! the ordering by giving up the scan bound this module exists to keep.
//!
//! **Owner: Task 27** (Phase B, the three plan drill-downs). It is the task that
//! ports `api_plan_test_history` (`manager/src/routes/analytics.rs:2530`), whose
//! `ORDER BY t.test_name, r.finished_at DESC NULLS LAST` is this exact ordering
//! over this exact table — so it is the first task that has to produce it for a
//! surface a user reads. Its decision is a real fork, not a chore:
//!
//! 1. Order in its own SQL. Task 27's endpoints are aggregates with legacy
//!    parameters (D7), not `OData` collections, so nothing forces them through
//!    the pager and `NULLS LAST` is expressible directly. The collection's gap
//!    then stays open **permanently** and should be restated here as such.
//! 2. Add the index and the guarded orderable key, which closes it for both.
//!
//! Deliberately not done in the review fix wave that recorded it: a new index
//! plus a cursor-key design change is feature work. The plan's spec-coverage row
//! for `cpt-cf-qa-fr-insights-history` carries the same statement, so an auditor
//! of that requirement finds it without reading this file.
//!
//! # The allow-lists are the index lists, as closely as the schema permits
//!
//! These are the two tables `cpt-cf-qa-nfr-scale` targets 5M rows on, so a
//! filter on a column no index covers is a sequential scan wearing a query's
//! clothes. Every column outside the enums below is therefore *unfilterable by
//! construction* — the parse-level gate rejects it before any SQL exists.
//!
//! **What the gate does and does not promise, stated precisely because the
//! obvious phrasing is not true.** Every *secondary* index on both tables leads
//! with `tenant_id`, which the caller's `AccessScope` always supplies, so for a
//! field covered by one of those the question is whether it is the **next**
//! column. The two primary keys are the exception the table below opens with:
//! they are `id` alone, they are unique, and a scope predicate on top of an
//! equality on the whole key costs nothing — so `id` is a prefix on its own
//! terms rather than under the tenant. Measured against
//! `migrations/m20260818_000001_initial.rs`:
//!
//! | Field | Index | Position under the tenant predicate |
//! |---|---|---|
//! | [`TestResultsField::Id`] | `qa_test_results` PK | prefix (unique) |
//! | [`TestResultsField::RunId`] | `idx_qa_test_results_tenant_run` (`:471`) | prefix |
//! | [`TestResultsField::TestFile`] | `idx_qa_test_results_tenant_test` (`:472`) | prefix |
//! | [`TestResultsField::TestName`] | `idx_qa_test_results_tenant_test` (`:472`) | **member, not prefix** |
//! | [`TestResultsField::RunFinishedAt`] | `idx_qa_test_results_tenant_finished` (`:473`) | prefix |
//! | [`TestCaseResultsField::Id`] | `qa_test_case_results` PK | prefix (unique) |
//! | [`TestCaseResultsField::RunId`] | `idx_qa_test_case_results_tenant_run` (`:506`) | prefix |
//! | [`TestCaseResultsField::TestFile`] | `idx_qa_test_case_results_tenant_run_file` (`:507`) | **member, not prefix** |
//! | [`TestCaseResultsField::Status`] | `idx_qa_test_case_results_tenant_status` (`:508`) | prefix |
//!
//! So the honest rule is in two parts:
//!
//! 1. **Every field here is at least an index member**, and every field that is
//!    only a member becomes a prefix as soon as the caller supplies the column
//!    in front of it — `test_file` for `test_name`, `run_id` for the case
//!    table's `test_file`. Those are the realistic queries (a file's history; a
//!    run's cases for one file), and every legacy read of these tables that
//!    returns *rows* rather than an aggregate filters on exactly those columns:
//!    `WHERE rr.workflow_name = $1` joined on `run_id`
//!    (`manager/src/routes/runs.rs:150-153`), `WHERE tr.test_file = $1`
//!    (`manager/src/routes/tests.rs:313-315`), and — for the case table —
//!    `WHERE rr.workflow_name = ANY($1)` joined on `run_id`
//!    (`manager/src/routes/analytics.rs:1316-1319`). Legacy has no filterable
//!    listing of either table and no paging over them at all, which is why the
//!    surface below ports no legacy rule; it is what D7 decided.
//! 2. **A member-only filter is a range scan over one tenant's slice of that
//!    index, not a seek** — bounded by the tenant, not by the predicate. That is
//!    a real cost and it is accepted rather than hidden: dropping `test_name`
//!    would remove the "one test's history" query the plan's Step 1 test names
//!    as required. The remedy would be a `(tenant_id, test_name)` index, which
//!    means a **new migration** — the schema is append-only, so an existing one
//!    cannot be edited — and no task owns that.
//!
//! What the gate *does* buy unconditionally is the third case: a column no index
//! mentions at all. `qa_test_results.status`, `.branch`, `.product_version`,
//! `.created_at` and the rest, and `qa_test_case_results.reason`, `.nodeid`,
//! `.name`, `.ticket`, `.duration`, `.created_at`, are absent from these enums
//! and so cannot be named in a `$filter`, a `$orderby` or a cursor at all.
//!
//! **`reason` deserves its own sentence, because the plan's Step 1 test asserts
//! `TestResultsField` rejects it and calls it "unindexed".** It is not a column
//! of `qa_test_results` in the first place — it belongs to
//! `qa_test_case_results` (`m20260818_000001_initial.rs:499`,
//! `qa_insights_sdk::TestCaseResultRecord::reason`). The assertion holds for a
//! stronger reason than the one given, and it is unindexed on the table it does
//! belong to as well, so it is absent from both enums.
//!
//! # `status` is filterable on the case table and not on the file table
//!
//! An asymmetry of the schema, not of this module: `qa_test_case_results` has
//! `(tenant_id, status)` (`:508`) and `qa_test_results` has no status index. So
//! "every failure in this window" is an indexed query at case level and an
//! unindexed one at file level. Recorded rather than smoothed over, because a
//! caller will want it: the file-level substitute is `run_id` or `test_file`
//! plus a client-side status test, and the fix — a
//! `(tenant_id, status, run_finished_at)` index — is a migration, which this
//! task does not own.
//!
//! # Names are the persisted column names; camelCase is accepted as an alias
//!
//! [`FilterField::name`] is what the `OpenAPI` document advertises, and it is the
//! **column** name in every collection in this workspace
//! (`gears/qa-platform/qa-runs/qa-runs/src/infra/storage/odata.rs`,
//! `gears/bss/ledger/ledger/src/odata.rs`,
//! `gears/system/resource-group/resource-group-sdk/src/odata/groups.rs`). The response bodies agree:
//! `#[toolkit_macros::api_dto]` emits `#[serde(rename_all = "snake_case")]`
//! (`libs/toolkit-macros/src/api_dto.rs:78`), so a caller reads
//! `run_finished_at` off a row and writes `run_finished_at` in a filter.
//!
//! The [`FilterField::from_name`] overrides below additionally resolve a
//! **separator-insensitive** spelling, so `runFinishedAt` names the same field as
//! `run_finished_at`. Two reasons, and the first is the binding one: the plan's
//! Step 1 test requires `test_name`, `run_id` **and** `runFinishedAt` to resolve
//! on one enum, which no single spelling of `name()` can satisfy. The second is
//! that camelCase is the `OData` ecosystem's own property convention, so a client
//! generated against a camelCase-flavoured toolchain would otherwise get an
//! `UnknownField` for a field that exists. The alias cannot widen anything: it
//! resolves to the same variants, and a name matching none of them is still
//! rejected.
//!
//! Rejected alternative: spelling the variant `runFinishedAt` in `name()`. It
//! would satisfy the test with no override, and it would advertise one camelCase
//! field beside four `snake_case` ones in the published document and break the
//! column-name convention above for a single field.

use sea_orm::Value;
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};
use toolkit_odata::filter::{FieldKind, FilterField};

use crate::infra::storage::entity::test_case_result::{
    Column as CaseColumn, Entity as CaseEntity, Model as CaseModel,
};
use crate::infra::storage::entity::test_result::{
    Column as ResultColumn, Entity as ResultEntity, Model as ResultModel,
};

/// A field name with separators removed and case folded, for the alias lookup
/// this module's header describes.
///
/// `_` is the only separator either table uses, so removing it is enough to make
/// `run_finished_at` and `runFinishedAt` the same key. `-` is deliberately *not*
/// removed: no column here contains one, and folding it in would make
/// `run-finished-at` resolve too, which is a spelling no part of this platform
/// produces.
fn squash(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// [`FilterField::from_name`] for both enums here: exact (case-insensitive)
/// first, then the separator-insensitive alias.
///
/// **Ambiguity resolves to `None`, not to the first hit.** Two fields whose
/// squashed keys collide would make the alias silently pick one of them, which is
/// how a filter ends up applied to the wrong column; a rejection is a 400 the
/// caller can read. Nothing collides today and
/// [`tests::no_two_fields_share_a_squashed_alias`] is what keeps that true when a
/// field is added.
///
/// This override **drops** the trait default's property-path fallback, which
/// resolves `hierarchy/depth` by its last segment. Both tables here are flat and
/// neither enum declares a slash-delimited name, so the fallback could only ever
/// accept a path this gear cannot serve. Narrowing is the safe direction.
fn resolve<F: FilterField>(name: &str) -> Option<F> {
    if let Some(exact) = F::FIELDS
        .iter()
        .copied()
        .find(|f| f.name().eq_ignore_ascii_case(name))
    {
        return Some(exact);
    }

    let wanted = squash(name);
    let mut matches = F::FIELDS
        .iter()
        .copied()
        .filter(|f| squash(f.name()) == wanted);
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(first)
}

/// Filterable, sortable and cursor-capable fields of `GET /qa/v1/test-results`.
///
/// Five of the table's nineteen columns. The header's table says which index
/// covers each and which of them is a member rather than a prefix.
///
/// The default order is `id` descending — see
/// [`OrmResultsRepository::list_page`](crate::infra::storage::results_sea_repo::OrmResultsRepository)'s
/// tiebreaker argument for why the *unique* column is the one that carries the
/// cursor here even though a timestamp would read better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestResultsField {
    Id,
    RunId,
    TestFile,
    TestName,
    /// Denormalized from the run, and **`NULL` while the run is still going**
    /// (`entity::test_result::Model::run_finished_at`). Filterable — it is the
    /// prefix of `idx_qa_test_results_tenant_finished`, so a time window is a
    /// range seek — and deliberately **not** orderable; see
    /// [`TestResultsODataMapper::is_orderable`].
    RunFinishedAt,
}

impl FilterField for TestResultsField {
    const FIELDS: &'static [Self] = &[
        Self::Id,
        Self::RunId,
        Self::TestFile,
        Self::TestName,
        Self::RunFinishedAt,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::RunId => "run_id",
            Self::TestFile => "test_file",
            Self::TestName => "test_name",
            Self::RunFinishedAt => "run_finished_at",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Id | Self::RunId => FieldKind::Uuid,
            Self::TestFile | Self::TestName => FieldKind::String,
            Self::RunFinishedAt => FieldKind::DateTimeUtc,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        resolve::<Self>(name)
    }
}

/// Column mapping for [`TestResultsField`].
pub struct TestResultsODataMapper;

impl FieldToColumn<TestResultsField> for TestResultsODataMapper {
    type Column = ResultColumn;

    fn map_field(field: TestResultsField) -> ResultColumn {
        match field {
            TestResultsField::Id => ResultColumn::Id,
            TestResultsField::RunId => ResultColumn::RunId,
            TestResultsField::TestFile => ResultColumn::TestFile,
            TestResultsField::TestName => ResultColumn::TestName,
            TestResultsField::RunFinishedAt => ResultColumn::RunFinishedAt,
        }
    }

    /// `run_finished_at` is filterable but not orderable, and the reason is a
    /// measured property of the cursor codec rather than a preference.
    ///
    /// A keyset cursor carries the boundary row's value for every order key, and
    /// `encode_cursor_value` has no arm for a `NULL`: every arm matches
    /// `Some(..)` and the fallthrough is
    /// `Err("Unsupported or mismatched cursor value type")`
    /// (`libs/toolkit-db/src/odata/sea_orm_filter.rs:410-439`), which
    /// `build_cursor_from_model` turns into `ODataError::InvalidCursor`. This
    /// column is `NULL` for every row of a run that has not finished, and this
    /// gear writes those rows — legacy writes `RUNNING`/`PENDING` results too
    /// (`manager/src/services/argo.rs:2603`). So `$orderby=run_finished_at`
    /// would page normally until a page boundary landed on an in-progress row
    /// and then answer 400 for the rest of the collection, with the failure
    /// depending on the data rather than on the request.
    ///
    /// **Measured, not read:** `tests::a_null_instant_cannot_be_encoded_into_a_cursor`
    /// calls that function with `Value::TimeDateTimeWithTimeZone(None)`.
    ///
    /// The `NULL` ordering itself is dialect-dependent on top of that —
    /// `ORDER BY ... DESC` puts `NULL`s first on Postgres and last on `SQLite`,
    /// which `migrations/m20260818_000001_initial.rs` already records against
    /// `ingest_ordinal` — so an ordering that worked would still not be the same
    /// ordering on the unit tier and in production.
    ///
    /// Rejected alternatives: **`COALESCE(run_finished_at, run_created_at)`**,
    /// which is what `results_sea_repo::effective_ts` uses for the analytics reads
    /// and which no index covers, so it would buy an orderable field by giving up
    /// the scan bound this whole module exists to keep; and **leaving it orderable
    /// and documenting the hazard**, which leaves a 400 in the collection's
    /// pagination for whoever hits it first.
    fn is_orderable(field: TestResultsField) -> bool {
        !matches!(field, TestResultsField::RunFinishedAt)
    }
}

impl ODataFieldMapping<TestResultsField> for TestResultsODataMapper {
    type Entity = ResultEntity;

    fn extract_cursor_value(model: &ResultModel, field: TestResultsField) -> Value {
        match field {
            TestResultsField::Id => Value::Uuid(Some(Box::new(model.id))),
            TestResultsField::RunId => Value::Uuid(Some(Box::new(model.run_id))),
            TestResultsField::TestFile => Value::String(Some(Box::new(model.test_file.clone()))),
            TestResultsField::TestName => Value::String(Some(Box::new(model.test_name.clone()))),
            // Unreachable through the pager, which rejects a non-orderable
            // field before it composes the effective order
            // (`sea_orm_filter.rs:699-705`), and populated anyway rather than
            // panicked on: `is_orderable` and this match are two places, and a
            // later task that makes the field orderable must find a cursor
            // value here rather than a panic.
            TestResultsField::RunFinishedAt => {
                Value::TimeDateTimeWithTimeZone(model.run_finished_at.map(Box::new))
            }
        }
    }
}

/// Filterable, sortable and cursor-capable fields of
/// `GET /qa/v1/test-case-results`.
///
/// Four of the table's twelve columns, and a **different** set from
/// [`TestResultsField`] rather than a copy of it, because the two tables carry
/// different indexes: this one has a status index and no time column at all
/// (case rows are not denormalized — `entity::test_case_result`'s header says
/// why), so `status` is in and `run_finished_at` has nothing to name.
///
/// `name` is absent even though it is the case-level counterpart of
/// [`TestResultsField::TestName`]: no index mentions it. The file-level table's
/// `(tenant_id, test_file, test_name)` has no twin here — this table's second
/// index is `(tenant_id, run_id, test_file)` — so admitting `name` would admit a
/// whole-tenant scan of a 5M-row table. A caller after one function's outcomes
/// filters `run_id` and `test_file` and picks the row out of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestCaseResultsField {
    Id,
    RunId,
    TestFile,
    /// Open producer text, **uppercase by producer convention and by nothing
    /// stronger**. The eight spellings seen today — `PASSED` | `FAILED` |
    /// `ERROR` | `SKIPPED` | `PENDING` | `RUNNING` | `XFAIL` | `XPASS` — are
    /// recorded as a *convention* on the column itself
    /// (`m20260818_000001_initial.rs:391-395`, which says "open, uppercase
    /// producer set … deliberately not validated"); see
    /// `infra::storage::mapper`'s note for why the column is never decoded.
    ///
    /// **So a filter is an exact, case-sensitive match against whatever the
    /// runner actually said.** `status eq 'XFAIL'` finds a row holding `XFAIL`
    /// and not one holding `xfail`, and nothing in this pipeline decides which of
    /// the two is stored.
    ///
    /// **Corrected 2026-08-21.** This doc said the stored value was "uppercased
    /// by ingest". Nothing uppercases: the projection clones the producer's string
    /// (`domain/service/ingest.rs:285` for the case row, `:295` for the file row),
    /// and qa-runs' `normalize_status` trims and *deliberately* does not re-case
    /// (`qa-runs/src/domain/service/ingest.rs:217`). Two headers in
    /// this crate already stated the rule in terms — `domain::service::ingest`
    /// ("a lowercase `passed` is `StatusBucket::Uncounted` here, exactly as it is
    /// in legacy. Normalising it would be a divergence") and
    /// `infra::storage::mapper` ("count what is recognized, store what arrives") —
    /// so the false clause contradicted its own gear.
    ///
    /// The consequence is silent, which is why it is written out: a runner that
    /// emits `xfail` produces a row that `status eq 'XFAIL'` misses, answering an
    /// empty page and no error. A caller that cannot assume its runners' casing
    /// should filter `run_id`/`test_file` and bucket the status itself.
    Status,
}

impl FilterField for TestCaseResultsField {
    const FIELDS: &'static [Self] = &[Self::Id, Self::RunId, Self::TestFile, Self::Status];

    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::RunId => "run_id",
            Self::TestFile => "test_file",
            Self::Status => "status",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Id | Self::RunId => FieldKind::Uuid,
            Self::TestFile | Self::Status => FieldKind::String,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        resolve::<Self>(name)
    }
}

/// Column mapping for [`TestCaseResultsField`].
///
/// No `is_orderable` override, and that is a property of the field set rather
/// than an omission: all four columns are `NOT NULL` — `id` (`:486`), `run_id`
/// (`:488`), `test_file` (`:492`) and `status` (`:496`) of
/// `m20260818_000001_initial.rs` — so none of them can reach the
/// cursor codec's `NULL` hole that [`TestResultsODataMapper::is_orderable`]
/// describes.
pub struct TestCaseResultsODataMapper;

impl FieldToColumn<TestCaseResultsField> for TestCaseResultsODataMapper {
    type Column = CaseColumn;

    fn map_field(field: TestCaseResultsField) -> CaseColumn {
        match field {
            TestCaseResultsField::Id => CaseColumn::Id,
            TestCaseResultsField::RunId => CaseColumn::RunId,
            TestCaseResultsField::TestFile => CaseColumn::TestFile,
            TestCaseResultsField::Status => CaseColumn::Status,
        }
    }
}

impl ODataFieldMapping<TestCaseResultsField> for TestCaseResultsODataMapper {
    type Entity = CaseEntity;

    fn extract_cursor_value(model: &CaseModel, field: TestCaseResultsField) -> Value {
        match field {
            TestCaseResultsField::Id => Value::Uuid(Some(Box::new(model.id))),
            TestCaseResultsField::RunId => Value::Uuid(Some(Box::new(model.run_id))),
            TestCaseResultsField::TestFile => {
                Value::String(Some(Box::new(model.test_file.clone())))
            }
            TestCaseResultsField::Status => Value::String(Some(Box::new(model.status.clone()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::Value;
    use toolkit_db::odata::sea_orm_filter::{FieldToColumn, encode_cursor_value};
    use toolkit_odata::filter::{FieldKind, FilterField};

    use super::{
        TestCaseResultsField, TestCaseResultsODataMapper, TestResultsField, TestResultsODataMapper,
        squash,
    };

    /// **The plan's Step 1 test, with its four assertions verbatim** and its
    /// name kept because the plan's Step 2 command runs it by name.
    ///
    /// The name is an approximation and the module header carries the accurate
    /// rule: `test_name` is an index *member* rather than an index *prefix*
    /// under the tenant predicate, so "only the indexed columns" is true of the
    /// column list and not of the seek/scan distinction. It is asserted anyway
    /// because it is the requirement: the one-test's-history query is the reason
    /// `(tenant_id, test_file, test_name)` exists.
    ///
    /// `parse` in the plan's pseudo-code is [`FilterField::from_name`], which is
    /// the real API — the one `paginate_odata` and `with_odata_filter` both go
    /// through.
    ///
    /// `reason` is rejected, and the plan's `"unindexed"` label for it
    /// understates the case: it is not a column of `qa_test_results` at all.
    #[test]
    fn only_indexed_columns_are_filterable() {
        assert!(TestResultsField::from_name("test_name").is_some());
        assert!(TestResultsField::from_name("run_id").is_some());
        assert!(TestResultsField::from_name("runFinishedAt").is_some());
        assert!(TestResultsField::from_name("reason").is_none(), "unindexed");
    }

    /// The other half of the gate, which the plan's test does not reach: the
    /// columns that are genuinely covered by no index are rejected.
    ///
    /// Named individually rather than counted. Each is a column that exists on
    /// its table, so each is a name a caller will plausibly try; `status` on the
    /// **file** table is the one worth staring at — it is filterable on the case
    /// table and not here, which is the schema asymmetry the header records.
    #[test]
    fn the_columns_no_index_covers_are_rejected() {
        for name in [
            "status",
            "created_at",
            "updated_at",
            "duration",
            "launch_id",
            "jira_key",
            "product_version",
            "app_build",
            // Names the physical column, which stays `platform_id` — see
            // `TestResultDto::environment_id`'s doc (`api/rest/dto.rs`) for
            // why the *wire* field renamed while the column did not.
            // `test_result::Model` renames the Rust field to
            // `environment_id` and pins it to this column name; this list
            // asserts on the column, not the Rust field, so it stays as-is.
            "platform_id",
            "repo_id",
            "plan_path",
            "branch",
            "ingest_ordinal",
            "tenant_id",
        ] {
            assert!(
                TestResultsField::from_name(name).is_none(),
                "{name} is not covered by any qa_test_results index and must not be filterable",
            );
        }

        for name in [
            "nodeid",
            "name",
            "duration",
            "reason",
            "ticket",
            "created_at",
            "updated_at",
            "tenant_id",
        ] {
            assert!(
                TestCaseResultsField::from_name(name).is_none(),
                "{name} is not covered by any qa_test_case_results index and must not be \
                 filterable",
            );
        }
    }

    /// **`FIELDS` must list every variant**, because it is what the route
    /// advertises *and* what the repository iterates — a variant missing from it
    /// is a field that exists in the type and nowhere else, silently
    /// unfilterable. `map_field`, `name` and `kind` are exhaustive matches, so
    /// they cannot see the gap: adding a variant breaks them, and *omitting one
    /// from this slice* breaks nothing.
    ///
    /// Asserted as the exact sorted name list rather than as a count. qa-runs'
    /// equivalent pins a count against a hand-maintained `const`, having
    /// measured that a loop over `FIELDS` catches nothing; a list is strictly
    /// stronger — it fails on a missing variant *and* on a renamed one, and it
    /// needs no second number to keep true.
    #[test]
    fn every_field_variant_is_advertised_under_its_own_name() {
        let mut names: Vec<&str> = TestResultsField::FIELDS
            .iter()
            .map(FilterField::name)
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["id", "run_finished_at", "run_id", "test_file", "test_name"],
        );

        let mut case_names: Vec<&str> = TestCaseResultsField::FIELDS
            .iter()
            .map(FilterField::name)
            .collect();
        case_names.sort_unstable();
        assert_eq!(case_names, ["id", "run_id", "status", "test_file"]);
    }

    /// The advertised name **is** the column name, for every field, checked
    /// against the entity rather than against a second list.
    ///
    /// This is the pairing no exhaustive match can defend: `map_field` is total
    /// either way, so `TestName => Column::TestFile` compiles and every other
    /// test in this crate still passes — the collection would simply filter the
    /// wrong column. `Iden`'s rendering of a `SeaORM` `Column` is the physical
    /// column name, which is what makes the two sides comparable.
    #[test]
    fn each_advertised_name_is_the_column_it_maps_to() {
        for field in TestResultsField::FIELDS {
            let column = TestResultsODataMapper::map_field(*field);
            assert_eq!(
                sea_orm::Iden::to_string(&column),
                field.name(),
                "the advertised name and the mapped column disagree",
            );
        }
        for field in TestCaseResultsField::FIELDS {
            let column = TestCaseResultsODataMapper::map_field(*field);
            assert_eq!(
                sea_orm::Iden::to_string(&column),
                field.name(),
                "the advertised name and the mapped column disagree",
            );
        }
    }

    /// The camelCase alias resolves to the same variant as the canonical
    /// spelling, in both enums and in both directions of case.
    #[test]
    fn a_camel_case_spelling_names_the_same_field() {
        assert_eq!(
            TestResultsField::from_name("runFinishedAt"),
            Some(TestResultsField::RunFinishedAt),
        );
        assert_eq!(
            TestResultsField::from_name("testFile"),
            Some(TestResultsField::TestFile),
        );
        assert_eq!(
            TestResultsField::from_name("RUN_ID"),
            Some(TestResultsField::RunId),
        );
        assert_eq!(
            TestCaseResultsField::from_name("runId"),
            Some(TestCaseResultsField::RunId),
        );
        // The alias widens spellings, never the field set.
        assert!(TestResultsField::from_name("productVersion").is_none());
        assert!(TestCaseResultsField::from_name("nodeId").is_none());
    }

    /// The alias lookup is only unambiguous while no two fields squash to the
    /// same key, and `resolve` answers `None` rather than guessing when they do.
    /// This is the guard for the next field somebody adds — `test_name` and a
    /// hypothetical `testname` would collide, and the collision would be
    /// invisible without this.
    #[test]
    fn no_two_fields_share_a_squashed_alias() {
        for fields in [
            TestResultsField::FIELDS
                .iter()
                .map(FilterField::name)
                .collect::<Vec<_>>(),
            TestCaseResultsField::FIELDS
                .iter()
                .map(FilterField::name)
                .collect::<Vec<_>>(),
        ] {
            let mut keys: Vec<String> = fields.iter().map(|n| squash(n)).collect();
            let before = keys.len();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(before, keys.len(), "two fields share an alias: {keys:?}");
        }
    }

    /// `run_finished_at` is filterable and **not** orderable; everything else in
    /// both enums is both.
    #[test]
    fn the_nullable_instant_is_filterable_but_not_orderable() {
        assert!(!TestResultsODataMapper::is_orderable(
            TestResultsField::RunFinishedAt
        ));
        for field in TestResultsField::FIELDS
            .iter()
            .filter(|f| **f != TestResultsField::RunFinishedAt)
        {
            assert!(
                TestResultsODataMapper::is_orderable(*field),
                "{} lost its ordering",
                field.name(),
            );
        }
        for field in TestCaseResultsField::FIELDS {
            assert!(
                TestCaseResultsODataMapper::is_orderable(*field),
                "{} is NOT NULL and has no reason to be unorderable",
                field.name(),
            );
        }
    }

    /// The measurement behind [`TestResultsODataMapper::is_orderable`]'s
    /// argument: the cursor codec cannot encode a `NULL`, so a nullable order
    /// key turns into a 400 as soon as a page boundary lands on an absent value.
    ///
    /// Called directly on `toolkit-db`'s own function rather than inferred from
    /// reading it, and paired with the `Some` case so this is evidence about the
    /// `None` arm specifically and not about the call being wrong.
    #[test]
    fn a_null_instant_cannot_be_encoded_into_a_cursor() {
        let present = time::macros::datetime!(2026-08-18 00:00:00 UTC);
        assert!(
            encode_cursor_value(
                &Value::TimeDateTimeWithTimeZone(Some(Box::new(present))),
                FieldKind::DateTimeUtc,
            )
            .is_ok(),
            "a present instant must encode, or this test proves nothing about NULL",
        );
        assert!(
            encode_cursor_value(
                &Value::TimeDateTimeWithTimeZone(None),
                FieldKind::DateTimeUtc,
            )
            .is_err(),
            "if a NULL instant encodes, run_finished_at can be made orderable",
        );
    }
}
