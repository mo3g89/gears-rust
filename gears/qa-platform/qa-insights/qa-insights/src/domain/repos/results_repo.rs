//! Test results — the ingest write path, the two `OData` collections, and every
//! analytics read.

use async_trait::async_trait;
use qa_insights_sdk::{TestCaseResultRecord, TestResultRecord};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::analytics::{CaseRow, ExecRow, UniverseFilter};
use crate::domain::error::DomainError;

/// One file-level outcome to write. The insert side of
/// [`TestResultRecord`](qa_insights_sdk::TestResultRecord), which carries an
/// `id` and a `created_at` the repository mints.
///
/// The eight denormalized run columns are on this type rather than derived
/// inside the repository, because only the caller has the run: resolving it is
/// a cross-gear call, and the migration's obligation #1 requires that
/// resolution to have already happened under the caller's own scope. (Seven
/// until Task 21b added `run_created_at`.)
///
/// **`ingest_ordinal` is the opposite case and is deliberately absent.** That
/// column carries the row's position within its batch, and the batch is the
/// `Vec` handed to [`ResultsRepository::upsert_run_results`] — so the repository
/// already has the value and derives it with `enumerate()`. A field here could
/// be duplicated, sparse or transposed, and none of those would fail to compile;
/// a derived one cannot drift from the order the rows are actually inserted in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewTestResult {
    pub test_file: String,
    pub test_name: String,
    pub status: String,
    pub duration: Option<String>,
    pub launch_id: Option<String>,
    pub jira_key: Option<String>,
    pub product_version: Option<String>,
    /// The build under test, denormalized from `qa_runs.app_build`. The
    /// analytics *projection* to [`Self::product_version`]'s *filter*; see
    /// [`qa_insights_sdk::TestResultRecord::app_build`].
    pub app_build: Option<String>,
    pub environment_id: Option<Uuid>,
    pub repo_id: Option<Uuid>,
    pub plan_path: Option<String>,
    pub branch: Option<String>,
    pub run_finished_at: Option<OffsetDateTime>,
    /// The run's own creation instant, denormalized from `qa_runs.created_at`.
    ///
    /// **The fallback half of legacy's `COALESCE(rr.finished_at, rr.created_at)`,
    /// and not a duplicate of the row's `created_at`** — that one is minted by
    /// the repository on every write and ingest rewrites a run's rows on every
    /// result event, so it moves for the whole life of the run. See
    /// [`qa_insights_sdk::TestResultRecord::run_created_at`].
    pub run_created_at: Option<OffsetDateTime>,
}

/// One case-level outcome to write. The insert side of
/// [`TestCaseResultRecord`](qa_insights_sdk::TestCaseResultRecord).
///
/// **`qa_test_case_results` had a writer here and no reader through Task 16**,
/// deliberately: the plan gave [`ResultsRepository`] exactly five methods, none
/// of them read case rows, and a `list_cases_by_run` would have been a method
/// with no call site. Task 12 twice named Task 21 as the first consumer and was
/// twice corrected to **Task 17**, which is where the reader landed:
/// [`ResultsRepository::list_case_page`], behind
/// `GET /qa/v1/test-case-results`. Task 21's `attach_case_summary` port is a
/// later, separate consumer.
///
/// The write side could *not* wait the same way: ingest is Task 13, it must be
/// idempotent in one transaction with the file-level rows, and bolting a second
/// delete-then-insert on later is how the two tables end up disagreeing about a
/// run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewTestCaseResult {
    pub test_file: String,
    /// pytest's fully-qualified node id; `""` when the runner reported none.
    pub nodeid: String,
    /// The test **function** name — the case-level table spells this `name`,
    /// not `test_name`.
    pub name: String,
    pub status: String,
    pub duration: Option<String>,
    pub reason: Option<String>,
    pub ticket: Option<String>,
}

/// How many `qa_test_results` rows one run holds with one status.
///
/// The reduced shape behind Task 18's dashboard, and the one every later
/// aggregate over this table should reach for before it reaches for rows.
///
/// # Why `(run, status, count)` and not the four counters legacy selects
///
/// Legacy computes its per-run counters with `COUNT(*) FILTER (WHERE tr.status =
/// 'PASSED')` and friends (`manager/src/routes/dashboard.rs:167-178`), i.e. the
/// status vocabulary is written **into the SQL**. This gear already has that
/// vocabulary in one place — [`crate::domain::service::ingest::classify`], the
/// port of the same fold — and duplicating it into `SeaQuery` expressions would
/// be two spellings of `FAILED`+`ERROR` that nothing keeps in agreement. So the
/// database groups and counts, and the domain folds. Three consequences, all
/// intended:
///
/// * The status fold is a pure function a unit test can drive, and it is the one
///   production uses. A `FILTER` in SQL is testable only against a database.
/// * What crosses the wire is bounded by *runs × distinct statuses in the
///   window*, not by rows — the same bound
///   [`ResultsRepository::ingested_run_ids_between`] achieves, and the reason
///   neither of them materialises the 5M rows `cpt-cf-qa-nfr-scale` targets.
///   It is nonetheless **wider** than legacy's one-row-per-day answer, which is
///   the price of not truncating a timestamp to a date in SQL: `DATE(x)` returns
///   `TEXT` on `SQLite` and `date` on Postgres, so a single `GROUP BY` day would
///   need a per-dialect projection, and this gear's repository tests run on
///   `SQLite` while it ships on Postgres.
/// * The day bucketing moves to the domain, which is sound because
///   [`Self::run_finished_at`] is one instant for a whole run — see that field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunStatusCount {
    pub run_id: Uuid,
    /// The runner's word, verbatim and un-normalised, exactly as the column
    /// holds it. `classify` is what interprets it.
    pub status: String,
    /// Rows in this `(run, status, instant)` group.
    pub rows: u64,
    /// The run's finish instant, denormalized onto every one of its rows by
    /// ingest and therefore **a grouping key here rather than an aggregate**.
    ///
    /// It is in the `GROUP BY` on purpose: `MAX(run_finished_at)` would work on
    /// Postgres and is a lexicographic maximum over text on `SQLite`, and a
    /// grouping key needs no aggregate at all when the value is constant per run
    /// — which it is, because `upsert_run_results` writes one run's rows from one
    /// `qa_runs_sdk::Run`. If a writer ever *did* vary it within a run, grouping
    /// splits that run into several rows, which is strictly more faithful than a
    /// maximum would be: a per-day fold then attributes each part to its own day.
    ///
    /// `None` for a run with no finish instant — legacy writes `RUNNING` rows too
    /// (`manager/src/services/argo.rs:2603`).
    pub run_finished_at: Option<OffsetDateTime>,
}

/// Rows per status inside one *effective-timestamp* window.
///
/// The unweighted sibling of [`RunStatusCount`]: no run and no instant, because
/// the only consumer is the dashboard's 24-hour KPI pair
/// ([`crate::domain::service::dashboard`]) and it folds the whole window into
/// three numbers. Grouping by run as well would multiply the rows crossing the
/// wire by the number of runs in the window for an answer that sums them all
/// back together.
///
/// Two counters and a denominator come out of this, and the denominator is the
/// part worth naming: legacy's is `status IN ('PASSED','FAILED','ERROR')`
/// (`manager/src/routes/dashboard.rs:329`), **not** every row — so a
/// `SKIPPED` or `RUNNING` row in the window moves neither the numerator nor the
/// denominator. That is why this carries the raw statuses rather than
/// pre-summed totals: the vocabulary belongs to
/// [`crate::domain::service::ingest::classify`], for the reason
/// [`RunStatusCount`] gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusRowCount {
    /// The runner's word, verbatim and un-normalised, exactly as the column
    /// holds it.
    pub status: String,
    /// Rows in this status group, inside the window.
    pub rows: u64,
}

/// One `(test_name, repo_id, plan_path)` group of the dashboard's flaky query,
/// already reduced, ranked and truncated by the database.
///
/// The row shape behind `DashboardStats::flaky_tests`, and the port of legacy's
/// `FlakyRow` (`manager/src/routes/dashboard.rs:369-377`).
///
/// # The grain is legacy's, and it is **not** the analytics flaky grain
///
/// Legacy groups by `tr.test_name, rr.plan_id` (`dashboard.rs:392`). `plan_id`
/// is `(repo_id, plan_path)` here, per `qa_insights_sdk`' note 1, so the grain is
/// the three fields above. The *analytics* flaky fold
/// ([`crate::domain::analytics::build_flaky`], legacy `analytics.rs:1655`) keys
/// on `test_file` and uses `bucketize_status`' three-way split — a different
/// grain **and** a different classification, both deliberately. That function's
/// own header carries the comparison; unifying the two would change numbers the
/// UI already renders and nothing here would fail.
///
/// # `test_file` is not a grouping key, and legacy picks a representative
///
/// It is **`MAX(tr.test_file)`** (`dashboard.rs:384`) — verified at source, and
/// worth recording because the task brief for this read expected legacy to have
/// no answer and forbade inventing one. It does have an answer, so this carries
/// it: one test name under one plan can be reported from more than one file, and
/// the alphabetically last of them is what legacy renders. Legacy's column is
/// nullable and `MAX` skips `NULL`s; this schema's is `NOT NULL DEFAULT ''`
/// (`entity::test_result::Model::test_file`), and `''` sorts below every real
/// path, so the two agree wherever any row of the group carries a path. When
/// none does, this is `""` and
/// `crate::domain::service::dashboard`'s `flaky_card` maps it back to the
/// `None` legacy would have rendered — the same meeting point `failure_card`
/// documents.
///
/// # Three counters, and only two of them are independent
///
/// [`Self::total`] is legacy's *sixth* classification — the
/// `PASSED`+`FAILED`+`ERROR` denominator at `dashboard.rs:388`, which
/// `crate::domain::service::ingest`' R5 table indexes — so it excludes a
/// `SKIPPED`, `PENDING`, `XFAIL` or unrecognised row from the group entirely.
/// Because that set is exactly the union of the two counted sets, `total` is
/// arithmetically `passed + failed`; it is carried and selected separately
/// anyway, because it is a **rendered number** and legacy selects it separately
/// (`:388`) rather than deriving it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlakyGroup {
    /// The runner's test name, verbatim — legacy's `tr.test_name`.
    pub test_name: String,
    /// `MAX(test_file)` over the group, and `""` when no row of the group named
    /// a file. See this type's note on the representative pick.
    pub test_file: String,
    /// The plan's repository, from the denormalized column. `None` for a row
    /// whose run named no plan.
    pub repo_id: Option<Uuid>,
    /// The plan's `plan.yaml` path within that repository.
    pub plan_path: Option<String>,
    /// Rows of this group whose status is one of the caller's passed statuses.
    /// **Strictly positive** — a group with none is excluded by the `HAVING`.
    pub passed: u64,
    /// Rows whose status is one of the caller's failed statuses, `ERROR`
    /// included. Strictly positive, on the same rule.
    pub failed: u64,
    /// Rows in either set. See this type's note: legacy's sixth classification,
    /// and a rendered number rather than a derived one.
    pub total: u64,
}

/// One test file's three counters, for the dashboard's quality-vector fold.
///
/// Legacy's `VectorTestRow` (`manager/src/routes/dashboard.rs:472-478`) — four
/// fields, four fields, with `test_file` non-optional here for the reason
/// [`Self::test_file`] gives.
///
/// # It is not [`FlakyGroup`], and the difference is the grain
///
/// Both are legacy's **sixth** classification
/// ([`crate::domain::service::ingest`]'s R5 table, row six) over the same
/// seven-day effective window, and they group differently and answer different
/// questions:
///
/// | | grain | `HAVING` | `LIMIT` |
/// |---|---|---|---|
/// | [`FlakyGroup`] | `(test_name, repo_id, plan_path)` (`dashboard.rs:392`) | both counters `> 0` (`:393-394`) | 10 (`:399`) |
/// | this type | `(repo_id, test_file)` (`dashboard.rs:492`) | none | none |
///
/// So a file whose tests only ever passed is **absent** from the flaky read and
/// **present** here with `failed == 0` — which it has to be, because the
/// quality-vector fold divides by [`Self::total`] and a vector whose files all
/// pass is a 100% bar rather than a missing one.
///
/// # `repo_id`, not `plan_path` — a fix-round-2 correction
///
/// Two repositories can each hold a `tests/test_smoke.py`, and the grain used
/// to be `test_file` alone: repo B's counts joined against repo A's universe
/// entry for the same path, so repo B's untagged file inherited repo A's
/// `quality_vectors` and one repo's execution outcome could land on the
/// other's vector. `repo_id` is carried the same way
/// [`FlakyGroup::repo_id`] is — `Option<Uuid>`, `None` for a row whose run
/// named no plan — to close that. `plan_path` is deliberately **not** added:
/// unlike the flaky grain, a file listed by two plans *within one repo* is
/// meant to collapse to one vector test
/// (`a_file_in_two_universe_entries_is_one_vector_test`), and `repo_id` alone
/// already disambiguates the cross-repository collision this fixes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatusCount {
    /// The normalized-ish stored path, verbatim from the column.
    ///
    /// A `String` and not an `Option`: the column is `NOT NULL DEFAULT ''`
    /// (`entity::test_result::Model::test_file`), so legacy's
    /// `tr.test_file IS NOT NULL` (`dashboard.rs:491`) is `test_file <> ''`
    /// here — see [`ResultsRepository::file_status_counts`].
    ///
    /// **Verbatim, not normalized.** It is whatever the runner reported, and the
    /// caller must put it through
    /// [`normalize_test_path`](crate::domain::analytics::universe::normalize_test_path)
    /// before matching it against the universe, exactly as legacy's fold does
    /// (`dashboard.rs:503-508`, an inline copy of `normalize_test_path`).
    /// Normalizing here would be a second definition of that rule in a place a
    /// reader of the fold would not look.
    pub test_file: String,
    /// The plan's repository, from the denormalized column — see this type's
    /// note on the grain. `None` for a row whose run named no plan, the same
    /// as [`FlakyGroup::repo_id`].
    pub repo_id: Option<Uuid>,
    /// Rows of this file whose status is one of the caller's passed statuses.
    /// May be `0` — there is no `HAVING`.
    pub passed: u64,
    /// Rows whose status is one of the caller's failed statuses, `ERROR`
    /// included. May be `0`.
    pub failed: u64,
    /// Rows in either set — legacy's `('PASSED','FAILED','ERROR')` denominator
    /// (`dashboard.rs:487`), derived as the union rather than spelled a third
    /// time. Provably [`Self::passed`] + [`Self::failed`] while the two sets are
    /// disjoint, and carried anyway because the fold sums all three
    /// independently, exactly as legacy's does (`:521-523`).
    pub total: u64,
}

/// One `qa_test_results` row for Task 27's three plan drill-downs — the port of
/// legacy's `api_plan_tests` (`manager/src/routes/analytics.rs:2434-2483`),
/// `api_plan_builds` (`:2486-2527`) and `api_plan_test_history`
/// (`:2530-2572`).
///
/// # This is not [`ExecRow`], and the difference is the grain and the columns
///
/// All three legacy statements are `SELECT … FROM test_results t JOIN
/// run_results r ON t.run_id = r.id WHERE r.plan_id = $1` with **no**
/// `app_version`/`app_build`/`branch`/`phase` predicate — no product, no
/// version, no universe and no catalog at all, unlike every other analytics
/// read in this gear. Two consequences follow, both load-bearing:
///
/// * The grain is `t.test_name` — `GROUP BY t.test_name` in SQL for
///   `api_plan_tests` (`:2453`), and the equivalent grouping done in Rust by
///   `api_plan_test_history`'s `history_map.entry(row.test_name.clone())`
///   (`:2557`) — **not** `test_file`, [`ExecRow`]'s grain. So this type carries
///   no `test_file` at all: none of the three legacy `SELECT` lists names it.
/// * Legacy reads `r.app_version` for what it names `last_version`
///   (`:2444`) and `build` (`:2493`, `:2536`) in all three responses —
///   **not** `r.app_build`, which is what [`ExecRow::build`] carries for the
///   overview's own build sections. The two legacy columns are genuinely
///   different (`manager/migrations/001_initial.sql:56,149`), and this port
///   preserves the plan drill-downs' choice of column verbatim: [`Self::version`]
///   is `qa_test_results.product_version`, the filter column
///   [`crate::domain::analytics::UniverseFilter::product_version`] denormalizes
///   from the same `r.app_version`, not `qa_test_results.app_build`.
/// * `t.jira_key` is selected only by `api_plan_tests` (`:2446`), and no other
///   read in this gear surfaces it — every other analytics row drops it
///   (`infra::storage::mapper::exec_row_from_result`'s header lists it among
///   what is "deliberately dropped").
///
/// # `run_name` becomes `run_id`, the same substitution `ExecRow::run_id` made
///
/// Legacy selects `r.workflow_name` as `last_run_name` (`:2445`) and as
/// `run_name` (`:2536`). This gear has no bulk run-name read — see
/// [`ExecRow::run_id`]'s header, which closed the identical obligation for the
/// overview by carrying the id and leaving the label to a caller who can
/// resolve it. The same choice is made here rather than re-derived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanExecRow {
    /// The run this outcome came from — legacy's `workflow_name`. See this
    /// type's header for why an id and not a name.
    pub run_id: Uuid,
    /// The runner's test name, verbatim — `t.test_name`, and the grain of all
    /// three responses.
    pub test_name: String,
    /// The raw uppercase status, exactly as the runner produced it — `t.status`.
    ///
    /// **Counted with a literal `'PASSED'`/`'FAILED'`(`/'SKIPPED'`) match, not
    /// through [`crate::domain::service::ingest::classify`]'s wider vocabulary.**
    /// `api_plan_tests`' `COUNT(*) FILTER (WHERE t.status = 'PASSED')` and
    /// `'FAILED'` (`:2448-2449`) and `api_plan_builds`' additional `'SKIPPED'`
    /// (`:2495-2497`) are exact string matches — an `ERROR` or `XFAIL` row
    /// counts toward a group's total but toward none of its named counters,
    /// which is narrower than the `PASSED_STATUSES`/`FAILED_STATUSES` union the
    /// dashboard's sixth classification uses. Preserved verbatim: the caller
    /// folds this field with the same three literals rather than reaching for
    /// that vocabulary.
    pub status: String,
    /// `qa_test_results.product_version` — legacy's `r.app_version`, read here
    /// as a **display** value rather than a filter, which is unique to these
    /// three endpoints. See this type's header for why it is not
    /// [`ExecRow::build`].
    pub version: Option<String>,
    /// Legacy's `r.platform`, a name; this schema stores the id. `None` for a
    /// run that named no platform. Read by `api_plan_tests` only — the other
    /// two responses carry no platform field.
    pub environment_id: Option<Uuid>,
    /// `t.jira_key`. Read by `api_plan_tests` only, and by no other analytics
    /// read in this gear — see this type's header.
    pub jira_key: Option<String>,
}

/// Persistence for `qa_test_results` and `qa_test_case_results`.
#[async_trait]
pub trait ResultsRepository: Send + Sync {
    /// Replace one run's results, file-level and case-level together.
    ///
    /// # Delete-then-insert is the contract, not an implementation detail
    ///
    /// This is obligation #5 of the schema (`m20260818_000001_initial`, "Result-row
    /// ingest must be idempotent"). There is deliberately **no** unique index on
    /// `(run_id, test_file, test_name)` — the tuple is 6432 bytes under
    /// `utf8mb4` and `a_repeated_result_tuple_is_accepted` pins its absence — so
    /// the database will happily store the same outcome twice and every count
    /// the analytics surface computes will then be wrong, with nothing failing.
    /// Legacy dedupes exactly this way: `DELETE ... WHERE run_id = $1 AND
    /// test_name = $2 AND COALESCE(test_file, '') = $3` then an unconditional
    /// `INSERT` (`manager/src/routes/runs.rs:1153-1185`).
    ///
    /// The delete is per **run**, not per row, and the two tables are replaced
    /// in one transaction: a partial replacement leaves a run whose file-level
    /// and case-level counts disagree.
    ///
    /// **Re-ingest is routine, not exceptional.** `qa_ingest_watermarks` exists
    /// because the event broker has no durable backend, so Task 15's reconcile
    /// poller replays a window of finished runs on every pass. A non-idempotent
    /// writer double-counts on the happy path.
    ///
    /// `run_id` arrives as a plain `Uuid` rather than a resolved token: this
    /// gear has no `qa_runs` table to resolve it against, and obligation #1 —
    /// never write a row against a run the caller cannot see — is discharged by
    /// the ingest service's qa-runs lookup before it calls here.
    ///
    /// # The delete is scope-filtered and the insert is not
    ///
    /// **Read this before calling with a `scope` that is not
    /// `AccessScope::for_tenant`.** Measured 2026-08-20, in Task 16's review.
    ///
    /// The implementation filters its two `DELETE`s with the **full** compiled
    /// scope (`.secure().scope_with(scope)`, which applies every mapped
    /// predicate) and performs its two bulk inserts through
    /// `.secure().scope_unchecked(scope)`. `scope_unchecked` is a documented
    /// **no-op**: it discards the scope and returns
    /// (`libs/toolkit-db/src/secure/db_ops.rs:376-385`). It is used because
    /// `scope_with_model` validates a single `ActiveModel` and there is no batch
    /// form, so a bulk insert has no validated path at all.
    ///
    /// The only guard between the two is `validate_tenant_in_scope`
    /// (`db_ops.rs:281-298`), and it inspects **`owner_tenant_id` alone** — it
    /// even returns `Ok` for an unconstrained scope (`:285-287`), which
    /// `scope_with` would then apply as *no filter*, making the per-run delete
    /// cross every tenant.
    ///
    /// So the two halves of "replace" only agree when the scope constrains the
    /// tenant and nothing else. Under any narrower scope the delete matches less
    /// than the insert writes and a replay **adds duplicate rows** — which the
    /// schema has no unique index to catch (see above) and every aggregate
    /// silently double-counts. Both entities declare `resource_col = "id"`
    /// (`infra/storage/entity/test_result.rs:25`,
    /// `entity/test_case_result.rs:20`), so a PDP `resource_id` constraint
    /// really does compile through and really does reach the delete.
    ///
    /// **The guard therefore lives at the caller**, in
    /// [`crate::domain::service::refuse_scope_beyond_tenant`], which every
    /// request-scoped writer must call before reaching this method; the two
    /// background callers pass `AccessScope::for_tenant` and satisfy it by
    /// construction. It is not enforced here because a refusal here would change
    /// what this method means for those two callers, who are already correct —
    /// and because the error that expresses it names a PEP resource type, which
    /// is a `domain::service` constant and not something `infra::storage` should
    /// be naming. Narrowing this method to take a tenant-only scope *type* is
    /// the fix that would move the guarantee into the compiler; it is a
    /// six-repository change and no task owns it.
    async fn upsert_run_results<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        run_id: Uuid,
        files: Vec<NewTestResult>,
        cases: Vec<NewTestCaseResult>,
    ) -> Result<(), DomainError>;

    /// Every file-level result of one run.
    ///
    /// **No production consumer, and none forecast** — stated because every
    /// other unused method on this trait names the task that will call it, and a
    /// silence here reads as an oversight. Measured 2026-08-21 (Phase A's
    /// whole-phase review): the only callers in the crate were tests, in four
    /// files, where it is how a test observes what the projection wrote
    /// (a transactional broker consumer's tests, `domain::service::reconcile_tests`,
    /// `domain::service::dashboard_tests`, and this repository's own tests). The
    /// consumer and its tests were deleted along with the event-broker
    /// dependency they needed (`crate::gear`'s header); the other three remain.
    ///
    /// That is a legitimate role and the reason it is kept: the alternative is
    /// each test file reaching past the repository into the entity, which is how
    /// a test stops proving that the scoped read path works. Every analytics
    /// surface reads through [`Self::list_for_universe`] instead, which windows;
    /// a per-run listing with no window is not the shape any of those needs. If a
    /// later task does find a use, the thing to check first is whether it wants
    /// `list_page` with `$filter=run_id eq …` instead, which pages.
    async fn list_by_run<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<Vec<TestResultRecord>, DomainError>;

    /// Every case-level outcome of one run, `reason` included.
    ///
    /// Task 33's own read, added for the bug-filing path: [`Self::list_by_run`]
    /// answers the *file*-level question ("which tests failed"), and neither it
    /// nor [`Self::case_rows_for_runs`] carries the per-case failure text —
    /// that trait method's own doc names its query verbatim, `SELECT
    /// rr.workflow_name, tcr.test_file, tcr.status, tcr.ticket` (no `reason`),
    /// because its only consumer is the overview's per-case roll-up, which
    /// never renders one. `domain::service::jira::JiraService::file_bugs`
    /// concatenates the `reason` of every `FAILED` case sharing a failing file's
    /// `(run_id, test_file)` into the JIRA issue body — this task's R84
    /// decision, since `qa_test_results` carries no `reason` column of its own
    /// (`infra::storage::entity::test_case_result::Model::reason`'s doc).
    ///
    /// Same shape as [`Self::list_by_run`] — one run, no window, no reduction —
    /// for the identical reason: the caller already holds the one run it cares
    /// about.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure.
    async fn case_rows_for_run<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<Vec<TestCaseResultRecord>, DomainError>;

    /// The rows an analytics read is about, newest first.
    ///
    /// The port of legacy's two `ExecRowRaw` queries (`analytics.rs:959-1010`).
    /// Ordering is load-bearing and not a caller's convenience: every
    /// "latest wins" map in the cores is built by iterating and keeping the
    /// first hit for a file (`:1625`, "rows are already sorted by timestamp
    /// desc in `load_universe_and_rows`; first wins"). Legacy orders by
    /// `COALESCE(r.finished_at, r.created_at) DESC, t.id DESC` and then re-sorts
    /// in memory on the same key, so an implementation must issue the same
    /// `ORDER BY`: [`ExecRow::ts`] descending, tiebroken on the row id
    /// descending.
    ///
    /// **That tiebreak is the repository's alone and cannot be reconstructed
    /// downstream** — [`ExecRow`] carries no `id`, deliberately, because no core
    /// reads one. So a core must treat the order it receives as authoritative
    /// and must not re-sort on `ts`: a stable re-sort would preserve the
    /// tiebreak, but an unstable one would silently pick a different "latest"
    /// row among results sharing a timestamp — which is exactly what ingesting
    /// a single run produces.
    ///
    /// # The tiebreak is `created_at` **and** `ingest_ordinal`, not either alone
    ///
    /// `ORDER BY sort_key DESC, created_at DESC, ingest_ordinal DESC, id DESC`.
    /// Decided 2026-08-20 (option A). This section has been wrong twice: it once
    /// claimed a solution it did not have, then said the question was open, and
    /// then — in `45dafe9b` — asserted equivalence for a *three*-key ordering that
    /// had silently dropped `created_at`. That regressed a case an earlier commit
    /// got right. The claim below is stated as two halves because the property is
    /// two halves, which is exactly what the earlier claims got wrong.
    ///
    /// What legacy does, established not inferred: the SQL orders by
    /// `COALESCE(...) DESC, t.id DESC` (`analytics.rs:971`, `:1001`), and the
    /// in-memory re-sort at `:1042` is `rows.sort_by(|a, b| b.ts.cmp(&a.ts))` —
    /// `sort_by` is **stable**, so the SQL tiebreak survives it.
    /// `latest_per_test_snapshot`'s first-wins loop (`:1615-1632`) consumes it.
    ///
    /// **`t.id` is a *global* `SERIAL`, so it carries two facts, and this schema
    /// needs a key for each:**
    ///
    /// 1. *Across* two runs sharing a timestamp, the higher serial is the
    ///    later-**ingested** run. `created_at DESC` reproduces this — one instant
    ///    per batch, monotonic with ingest order.
    /// 2. *Within* one run, the higher serial is the later row the **parser**
    ///    produced (`argo.rs:2598-2615` inserts in parse order).
    ///    `qa_test_results.ingest_ordinal DESC` reproduces this — the ordinal is
    ///    the row's position in its batch.
    ///
    /// Neither key is sufficient alone, and the pair is equivalent to a global
    /// serial descending. A three-key ordering that omits `created_at` picks the
    /// *older* run's row whenever that run happens to hold the file at a higher
    /// batch position, which is what shipped in `45dafe9b`.
    ///
    /// `ingest_ordinal` is derived by the repository from the batch's own order
    /// rather than supplied by a caller, and is deliberately absent from
    /// [`ExecRow`] and from
    /// [`TestResultRecord`](qa_insights_sdk::TestResultRecord): a persistence
    /// ordering detail, not an analytics projection.
    ///
    /// **Why a column was needed at all.** Both keys tried before it degenerate
    /// *within* a run — its rows share `run_finished_at`, and
    /// `upsert_run_results` stamps one `created_at` for the whole batch — leaving
    /// `id DESC` on a random v4 `Uuid`: stable per database, arbitrary across
    /// them. Reachable, not theoretical: legacy's dedupe tuple includes
    /// `test_name`, so one run may hold several rows for one `test_file`, and
    /// every row whose producer reported no file stores `''`.
    ///
    /// `id DESC` is a fourth key only to keep the order **total**.
    ///
    /// Returns the **unresolved** rows: no alias resolution and no universe
    /// filtering, both of which need the universe and belong to Task 20. See
    /// [`ExecRow`].
    async fn list_for_universe<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        filter: &UniverseFilter,
    ) -> Result<Vec<ExecRow>, DomainError>;

    /// The case-level outcomes of `run_ids`, as the overview's per-case roll-up
    /// consumes them.
    ///
    /// The port of the private query `attach_case_data` declares inline
    /// (`manager/src/routes/analytics.rs:1308-1319`):
    ///
    /// ```sql
    /// SELECT rr.workflow_name, tcr.test_file, tcr.status, tcr.ticket
    /// FROM test_case_results tcr JOIN run_results rr ON rr.id = tcr.run_id
    /// WHERE rr.workflow_name = ANY($1)
    /// ```
    ///
    /// No join here, because `qa_test_case_results` already carries `run_id` —
    /// legacy joins only to translate its run *name* into the foreign key.
    ///
    /// # `run_ids` is the latest run of each universe file, not every run
    ///
    /// Legacy builds the set from the latest map (`:1295-1303`) and this method
    /// is called the same way, which is what bounds it: the read is one row set
    /// per *file* rather than the whole case table, on a table
    /// `cpt-cf-qa-nfr-scale` sizes in the millions. An implementation must not
    /// widen it to a window or a plan scope — the fold on the other side looks
    /// each file up under **its own** latest run
    /// ([`CaseRow`]'s header, "the join key is `(run_id, test_file)`").
    ///
    /// # An empty `run_ids` is the caller's early return, not this method's
    ///
    /// Legacy returns before the query when the set is empty (`:1304-1306`),
    /// leaving the six per-case counters at zero — which is
    /// [`CaseData::default`](crate::domain::analytics::aggregates::CaseData),
    /// and is **not** the same value as folding a successful read of zero rows.
    /// That distinction belongs to the caller, so this method is not the place
    /// to encode it; it answers an empty `Vec` for an empty `run_ids` and
    /// performs no read.
    ///
    /// # Ordering is not specified and no fold depends on one
    ///
    /// Unlike [`Self::list_for_universe`], whose four-key ordering *is* the
    /// latest-wins rule. `build_case_data` indexes these rows on
    /// `(run_id, test_file)` and reduces each bucket with a severity order of
    /// its own (`effective_case_status`), so no answer here can depend on which
    /// row came first.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure. **Not laundered into an
    /// empty `Vec`**, where legacy warns and continues (`:1326-1329`): a broken
    /// read there silently reports a suite with no per-case data, which is
    /// indistinguishable from an older runner that reported none.
    async fn case_rows_for_runs<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_ids: &[Uuid],
    ) -> Result<Vec<CaseRow>, DomainError>;

    /// Every `qa_test_results` row of one plan, newest first — Task 27's read,
    /// behind the three plan drill-downs. [`PlanExecRow`]'s header records why
    /// this is not [`Self::list_for_universe`] under a different filter: there
    /// is no universe, no product and no version here at all.
    ///
    /// # `plan_id` matches `plan_path`, and repository is not part of it
    ///
    /// Legacy's `plan_id` is matched literally against `run_results.plan_id`,
    /// a string composed from `(repo_id, dir_path)` at plan-listing time
    /// (`compose_repo_plan_id`, `manager/src/services/plans.rs:789-801`) — so a
    /// legacy request already carries both halves squeezed into one opaque
    /// token. This schema has no such column and no equivalent slug, and this
    /// task's own path parameter is a single string with nothing else in the
    /// request to fix a repository — no `product_id`, no `version`, unlike
    /// [`crate::domain::service::analytics::narrow_to_plan`]'s query parameter,
    /// whose repository half is fixed by the product elsewhere in the request.
    ///
    /// The resolution follows the *same principle* `narrow_to_plan` already
    /// applied — the caller holds one string and it names the path, so it is
    /// matched against `plan_path` alone — generalized to its only sound
    /// reading here: every repository the caller's scope admits, not one
    /// product's. That is not a new decision; it is the wire contract this gear
    /// already ships, stated at
    /// [`crate::api::rest::dto::AnalyticsListItemDto::plan_path`]: "the value
    /// `?scope=plan&plan_id=` takes" is `plan_path` and nothing else, so a
    /// client already never sends a repository half for any `plan_id`
    /// anywhere in this gear.
    ///
    /// # A read window exists here for the same NFR reason `since` exists on
    /// # [`UniverseFilter`], though legacy has none
    ///
    /// All three legacy statements are unwindowed — `cpt-cf-qa-nfr-scale`
    /// targets 5M rows on this table, and an unbounded per-plan scan is exactly
    /// the read [`UniverseFilter::since`]'s header argues against. `since` is
    /// the caller's; [`crate::domain::service::analytics::universe_window_start`]
    /// supplies the same 90-day default the build-tests drill-down already
    /// inherits under ruling R22, for the identical reason.
    ///
    /// **The predicate has to be the `kpi_window` decomposition, not a bound on
    /// the `COALESCE` expression.** `qa_test_results` has no index on
    /// `plan_path` at all (`m20260818_000001_initial.rs:499-501`), so
    /// `idx_qa_test_results_tenant_finished` — `(tenant_id, run_finished_at
    /// DESC)` — is the only index this filter could reach, and it can only
    /// serve a predicate on the bare column. Writing the window as
    /// `COALESCE(run_finished_at, run_created_at) >= since` — this method's own
    /// first implementation — defeats that index and scans the tenant's whole
    /// slice, which undercuts the NFR reason the window exists at all. The
    /// implementation uses `kpi_window`'s `OR`-of-two-branches identity
    /// instead, exactly as [`Self::effective_status_counts`] and
    /// [`Self::recent_failures`] already do.
    ///
    /// # Ordering is the gear's own convention, not legacy's per-statement one
    ///
    /// Legacy orders each of the three by a **bare** `r.finished_at DESC NULLS
    /// LAST` (`:2442`, `:2540`; `api_plan_builds` has no time ordering at all).
    /// This reads `COALESCE(run_finished_at, run_created_at) DESC`, the
    /// convention [`Self::list_for_universe`] and controller Ruling C
    /// established for every analytics read in this gear, tie-broken
    /// `created_at DESC, ingest_ordinal DESC, id DESC` for the same reason that
    /// method's doc gives. The two disagree only for a run still in progress
    /// (no `run_finished_at`): legacy's bare column sorts it as the *oldest*
    /// row regardless of how recently it started, where the coalesced instant
    /// sorts it near its own creation time — legacy's re-projection-on-every-event
    /// design is exactly what Ruling C's fallback was chosen to answer, and an
    /// in-progress run's plan-tests row is the same case, not a different one.
    /// A deliberate divergence, not an oversight, and consistent with every
    /// other read this trait exposes.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure.
    async fn list_for_plan<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        plan_path: &str,
        since: OffsetDateTime,
    ) -> Result<Vec<PlanExecRow>, DomainError>;

    /// The most recent `product_version` reported for a plan, among the rows
    /// that carry one — Task 35's `check_new_build` predicate
    /// (`domain::service::jira::JiraService::latest_version_for_plan`).
    ///
    /// Legacy's own query, a single targeted row: `SELECT app_version FROM
    /// run_results WHERE plan_id = $1 AND app_version IS NOT NULL ORDER BY
    /// created_at DESC LIMIT 1` (`manager/src/services/jira_poller.rs:231-233`,
    /// inside `check_new_build`, `:225-243`). This is a dedicated method and
    /// not a call to [`Self::list_for_plan`] for a reason that is a
    /// correctness fix, not a style choice — **fix round 1, Important 2**: an
    /// earlier revision called `list_for_plan(.., OffsetDateTime::UNIX_EPOCH)`
    /// and reduced in Rust with `.find_map`. That method's own doc states why
    /// its time window exists: for the same NFR reason `since` exists on
    /// `UniverseFilter` — an unbounded per-plan scan is exactly what that
    /// window guards against. Passing `UNIX_EPOCH` disables exactly that
    /// window, and `list_for_plan` has no
    /// `LIMIT` of its own, so **every row ever recorded for the plan crossed
    /// the wire** on every poll pass, for every open bug, forever. This
    /// method pushes the reduction into the query instead: `product_version
    /// IS NOT NULL`, newest first, `LIMIT 1` (a `.one()` read in the
    /// implementation) — one row transferred, not the plan's whole history.
    ///
    /// # Deliberately still unwindowed, unlike every other read on this trait
    ///
    /// A bug's own recorded version can be arbitrarily old, so any `since`
    /// bound this method picked could silently stop finding the very row
    /// `check_new_build` exists to compare against — the failure mode fix
    /// round 1 found. **Not claimed**: that the database can answer this
    /// query without examining every row of the plan's history server-side —
    /// there is no index on `plan_path` or on the `effective_ts` expression
    /// (`ResultsRepository::list_for_plan`'s own doc), so this is still a
    /// scan bounded by the plan's own row count, same as legacy's query
    /// against `run_results`. What `product_version IS NOT NULL` and
    /// `LIMIT 1` change is what crosses the *wire*: one row transferred to
    /// this process instead of the plan's entire row history, which is the
    /// defect fix round 1 found and this method exists to close.
    ///
    /// # `tenant_id` is a parameter — controller ruling R86
    ///
    /// This is a `.one()` read over a scope compiled against
    /// `OWNER_TENANT_ID`, which may legitimately span several tenants
    /// (`ScopeFilter::In`, `ScopeFilter::InTenantSubtree`) — the same shape
    /// [`crate::domain::repos::JiraRepository::find_unclosed_for_test`] and
    /// [`crate::domain::repos::JiraRepository::get_config`] already carry this
    /// requirement for, and the same defect those two docs record being found
    /// and fixed for. Without a `tenant_id` equality predicate, a multi-tenant
    /// scope would let this method answer with an arbitrary in-scope tenant's
    /// latest version, and a poll pass could compare *tenant A*'s bug against
    /// *tenant B*'s build. The tenant is validated against the scope first
    /// (`validate_tenant_in_scope`), so this cannot be used to probe a tenant
    /// the caller has no grant for.
    ///
    /// # `repo_id` is a parameter too — task 2's fix, the same shape as `tenant_id` above
    ///
    /// `plan_path` is repository-relative (`plans/smoke.yaml` names a
    /// different file in every repository that has one), so `(tenant_id,
    /// plan_path)` alone lets two repositories of one tenant share one
    /// answer. An earlier revision of this method took only `plan_path` and
    /// dropped the caller's `repo_id` — the query filtered `(tenant_id,
    /// plan_path)` and this method answered with whichever repository's row
    /// sorted newest under the `ORDER BY` below, regardless of which
    /// repository the caller actually asked about. The composite key this
    /// method now filters on is `(tenant_id, repo_id, plan_path)`; see
    /// `domain::service::jira_poller::JiraPollerService::maybe_rerun`'s call
    /// site for the failure this produced both ways — another repository's
    /// newer build triggering a rerun this gate exists to prevent, or another
    /// repository's version suppressing one that should have happened.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure.
    async fn latest_version_for_plan<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        repo_id: Uuid,
        plan_path: &str,
    ) -> Result<Option<String>, DomainError>;

    /// One page of `qa_test_results`, as an `OData` collection.
    ///
    /// `GET /qa/v1/test-results`. The caller's `$filter`, `$orderby`, `$top` and
    /// cursor arrive in `query`; the allow-list of fields any of them may name is
    /// `infra::storage::odata::TestResultsField`, and that module's header carries
    /// the index reasoning behind it. **Per D7, `OData` belongs here and to
    /// [`Self::list_case_page`] and nowhere else in this gear**: these are the two
    /// tables `cpt-cf-qa-nfr-scale` targets 5M rows on, and every other read this
    /// trait offers is either bounded by a run or is an analytics reduction with
    /// its own shape.
    ///
    /// # The `$filter` cannot widen what this returns
    ///
    /// The scope is composed *with* the filter, not replaced by it: the
    /// implementation scopes the select before `paginate_odata`, whose first
    /// parameter is a `SecureSelect<E, Scoped>` — so an unscoped select does not
    /// type-check.
    ///
    /// # The default order is `id` descending, and a timestamp was the rejected
    /// # alternative
    ///
    /// The pager takes **one** tiebreaker field, and with no `$orderby` that
    /// field is the whole order. It has to be *unique*, because the keyset
    /// predicate is a lexicographic comparison over exactly the order keys
    /// (`build_cursor_predicate`,
    /// `libs/toolkit-db/src/odata/sea_orm_filter.rs:844-903`): with a
    /// non-unique final key, the next page's `col < boundary` **skips every
    /// remaining row that shares the boundary value**.
    ///
    /// On this table that is not a corner case, it is the normal case.
    /// [`Self::upsert_run_results`] stamps **one** `created_at` for a whole
    /// batch, and `run_finished_at` is one instant for every row of a run — so a
    /// page boundary falling inside a run would silently drop the rest of that
    /// run. `id` is the primary key, so the order is total and the paging is
    /// exact.
    ///
    /// # There is no chronological ordering, and that follows from the index gate
    ///
    /// The cost of the `id` tiebreaker is that a v4 `Uuid` order is stable but
    /// arbitrary. The obvious remedy — `$orderby=created_at desc` — is **not
    /// available**, and saying so is the point of this paragraph: `created_at` is
    /// covered by no index on either table
    /// (`m20260818_000001_initial.rs:468`, `:503`), so the allow-list excludes it
    /// and an `$orderby` naming it is a 400. Ordering by an unindexed column over
    /// five million rows is a filesort of the tenant's whole slice, which is the
    /// cost `cpt-cf-qa-nfr-scale` forbids; `run_finished_at` is indexed and
    /// cannot be an order key either, for the separate reason
    /// `infra::storage::odata::TestResultsODataMapper::is_orderable` measures.
    ///
    /// So **recency is expressed as a filter, not as a sort**:
    /// `$filter=run_finished_at ge <t>` is an index range seek, and the rows
    /// inside the window come back in `id` order. The orderable fields are `id`,
    /// `run_id`, `test_file` and `test_name` — the ones a caller groups by, not
    /// the ones a caller would sort a timeline by.
    ///
    /// `the_chronological_order_a_caller_would_reach_for_is_refused` pins this,
    /// because an earlier draft of this very doc offered
    /// `$orderby=created_at desc` as the remedy and was wrong.
    ///
    /// qa-runs' equivalent uses `created_at` as its tiebreaker
    /// (`runs_sea_repo.rs:260`), which is not copied here for two independent
    /// reasons: this table has no index on it, and a whole ingest batch shares one
    /// value. One run has one `created_at`, so collisions there are rare; one
    /// *batch* of results shares one, so collisions here are guaranteed.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] for anything the caller got wrong — a
    /// `$filter` naming a field outside the allow-list, an `$orderby` on a
    /// non-orderable one, a cursor from a different sort order. See
    /// `infra::storage::db::odata_err` for the classification.
    ///
    /// [`DomainError::Database`] for a driver failure.
    async fn list_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<TestResultRecord>, DomainError>;

    /// One page of `qa_test_case_results`, as an `OData` collection.
    ///
    /// `GET /qa/v1/test-case-results`. [`Self::list_page`]'s sibling. Three of that
    /// method's paragraphs carry over unchanged — the scope/filter composition, the
    /// `id`-descending default order (this table's `created_at` is also one instant
    /// per ingest batch), and the error classification.
    ///
    /// Two things differ. The **field allow-list is not a copy**:
    /// `infra::storage::odata::TestCaseResultsField` admits `status`, because this
    /// table has `(tenant_id, status)` and the file-level one has no status index.
    /// And there is **no time field at all** — case rows carry no denormalized run
    /// columns — so [`Self::list_page`]'s "recency is a `run_finished_at` filter"
    /// remedy has no counterpart here: a caller wanting a window takes run ids from
    /// the file-level collection and filters `run_id`.
    ///
    /// # Errors
    ///
    /// As [`Self::list_page`].
    async fn list_case_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<TestCaseResultRecord>, DomainError>;

    /// The distinct runs that already have results ingested, whose
    /// `run_finished_at` falls in `[from, to)`.
    ///
    /// The read half of Task 15's reconcile: the poller asks qa-runs which runs
    /// finished in the window, asks this which of them already landed, and
    /// ingests the difference. Half-open so that consecutive windows sharing an
    /// endpoint neither skip a run nor replay one.
    ///
    /// **The `DISTINCT` is applied in SQL**, over a one-column projection, so what
    /// crosses the wire is bounded by the number of runs in the window rather than
    /// by the number of result rows those runs produced. `SecureSelect::project_all`
    /// is what makes that projection impossible to un-scope.
    ///
    /// (Task 12 first shipped this as a fold over every row in the window and
    /// wrote here that `SecureSelect` "has no `distinct()` and no column
    /// projection". It has both. The fullest account of how that claim got
    /// written, and why it was the more damaging kind of error — it was filed
    /// *as a correction* — was on `latest_per_test`'s doc, which went with that
    /// method; `qa-runs`' `queue_sea_repo::platforms_with_queued_rows` carries
    /// the same correction, made one gear earlier.)
    async fn ingested_run_ids_between<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        from: OffsetDateTime,
        to: OffsetDateTime,
    ) -> Result<Vec<Uuid>, DomainError>;

    /// Per-status row counts for each of `run_ids`.
    ///
    /// The port of legacy's per-run dashboard counter query, which selects
    /// `COUNT(tr.id)`, `FILTER (WHERE tr.status = 'PASSED')`,
    /// `FILTER (WHERE tr.status IN ('FAILED','ERROR'))` and
    /// `FILTER (WHERE tr.status = 'SKIPPED')` grouped by run. The four counters
    /// are folded from these groups by the caller; [`RunStatusCount`] says why.
    ///
    /// **The statement is `manager/src/routes/dashboard.rs:167-178`** — the same
    /// range the plan's Task 18 and `domain::service::dashboard`'s header give,
    /// spelled one way on purpose (this doc said `:165-181` until Task 18's fix
    /// round, and two spellings of a citation this task had just corrected invite
    /// the next reader to "fix" one of them). The emptiness guard the next
    /// paragraph is about is the line above it, `:165`.
    ///
    /// **A run with no rows is absent from the answer, not zero-filled.** Legacy
    /// reaches the same outcome the other way round — its `LEFT JOIN` yields the
    /// run with `COUNT(tr.id) = 0`, and its consumer then reads a missing entry
    /// as zeros anyway (`dashboard.rs:196-212`, `counts.map(...).unwrap_or(0)`).
    /// Zero-filling here would need the run set to be a *result* rather than a
    /// parameter, which it is not: the runs come from qa-runs.
    ///
    /// An empty `run_ids` answers with an empty `Vec` and issues no statement.
    /// That guard is explicit rather than left to `IN ()`, which is a syntax
    /// error on some dialects and a match-nothing on others.
    ///
    /// **Measured 2026-08-21 (Task 23b): `SeaQuery` is not one of those dialects.**
    /// It renders an empty `is_in` as the constant `1 = 2`
    /// (`sea-query-0.32.7/src/backend/query_builder.rs:386`), so on this dependency
    /// version the guard cannot be reached by an `IN ()` at all and removing it is
    /// invisible to every test. `infra::storage::collect_sea_repo::list_counts_for`
    /// reached the same conclusion for its own guard and states the case for keeping
    /// it anyway: the property is read-widening, and resting it on an undocumented
    /// builder behaviour that a version bump could change is worse than a redundant
    /// early return. Left standing here, and the sentence above is retained rather
    /// than rewritten because plan carried item 3 makes this doc Task 18's.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure.
    async fn run_status_counts<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_ids: &[Uuid],
    ) -> Result<Vec<RunStatusCount>, DomainError>;

    /// Per-status row counts for every run that **finished** at or after `since`.
    ///
    /// The port of legacy's daily status trend
    /// (`manager/src/routes/dashboard.rs:216-231`), whose day buckets the caller
    /// folds; see [`RunStatusCount`] for why the day is not a `GROUP BY` key.
    ///
    /// # Two deliberate differences from legacy's query, both recorded
    ///
    /// * Legacy restricts to `rr.phase IN ('Succeeded', 'Failed')`. **This gear
    ///   has no phase column** — `crate::domain::analytics::UniverseFilter::finished_only`
    ///   records that `run_finished_at IS NOT NULL` is the nearest expressible
    ///   substitute and is *wider*: a cancelled or errored run has a finish
    ///   instant and no `Succeeded`/`Failed` phase. `run_finished_at >= since`
    ///   implies that predicate, so the same widening applies here.
    /// * Legacy buckets on `DATE(COALESCE(rr.finished_at, rr.created_at))`, so a
    ///   run with no finish instant lands on its creation day. Here the bound is
    ///   the bare column, which drops such runs. That is the choice
    ///   `UniverseFilter::since` argues for and for the same reason: **no index
    ///   covers a `COALESCE`**, and `idx_qa_test_results_tenant_finished` is
    ///   `(tenant_id, run_finished_at DESC)` — exactly this window. A run still
    ///   in progress has no completed counters worth trending, and it is the
    ///   dashboard's active list that reports it.
    ///
    /// A lower bound only, with no `LIMIT`: the caller's window is at most 90
    /// days (legacy's clamp) and the answer is bounded by the runs inside it.
    ///
    /// **The worst case needs an explicit `days=90`**, which is three times the
    /// window a caller gets by default — `days` defaults to 14
    /// (`domain::service::dashboard::resolve_days`), so the ordinary read spans a
    /// fortnight. [`RunStatusCount`]'s header records that this returns more rows
    /// than legacy's one-per-day answer and why that trade was taken.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure.
    /// `repo_ids: Some(_)` additionally restricts to rows whose `repo_id` is one
    /// of the given ids — the dashboard's product filter
    /// (`domain::service::dashboard::DashboardService::stats`), applied the same
    /// way the client attributes a row to a product: through the repository a
    /// run's target names, never through a checkout-derived test universe. A row
    /// with no `repo_id` — ingest's `plan_identity` stamps `NULL` for a
    /// **custom-plan or collect** run alike, neither of which executes a plan —
    /// matches no non-empty set; `infra::storage::results_sea_repo::repo_scope`'s
    /// own doc says why that is *not* the same exclusion the live-run half of
    /// this filter makes for a collect run. `None` is every row this tenant's
    /// scope admits, exactly as before this parameter existed.
    async fn run_status_counts_since<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        since: OffsetDateTime,
        repo_ids: Option<&[Uuid]>,
    ) -> Result<Vec<RunStatusCount>, DomainError>;

    /// Rows per status in the half-open window
    /// `[from, to)` of the **effective** row timestamp.
    ///
    /// The port of legacy's dashboard KPI query
    /// (`manager/src/routes/dashboard.rs:317-348`), whose six `COUNT(*) FILTER`
    /// columns become two calls to this and one fold —
    /// `domain::service::dashboard::kpi_of`.
    ///
    /// # This is legacy's *third* row-inclusion rule, and it is the widest
    ///
    /// Inside one legacy endpoint, three statements read `test_results` three
    /// different ways, and the two Task 18 ported are both narrower than this
    /// one:
    ///
    /// | | rows admitted | citation |
    /// |---|---|---|
    /// | [`Self::run_status_counts`] | no time window at all; `workflow_name = ANY($1)`, a named set of runs | `:167-178` |
    /// | [`Self::run_status_counts_since`] | `DATE(COALESCE(finished_at, created_at))` in the day series **and** `phase IN ('Succeeded','Failed')` | `:216-231` |
    /// | this one | `COALESCE(finished_at, created_at)` in the window, and **no `phase` predicate at all** — zero occurrences of `phase` in the statement | `:317-348` |
    ///
    /// So this read counts rows of runs that are still going, which the daily
    /// trend excludes twice over. `domain::service::dashboard::window_start` is
    /// **not** the helper for this window: it exists for
    /// [`Self::run_status_counts_since`] and drops a run with no finish instant,
    /// which is precisely the row this read must keep.
    ///
    /// # `to: None` is legacy's current window, and it has no upper bound
    ///
    /// Legacy's current-window filters are `>= NOW() - INTERVAL '24 hours'` and
    /// nothing else (`:319-321`), while its previous-window filters carry both
    /// bounds (`:323-327`). An upper bound of "now" would be a divergence rather
    /// than a tidy-up: a row whose effective timestamp is in the future — a
    /// producer clock ahead of the clock the bounds came from, which is the
    /// ordinary case in a distributed runner — is counted by legacy and would be
    /// dropped here. `None` reproduces that, so the two windows still partition
    /// every row newer than `now - 48h` exactly once.
    ///
    /// **"The clock the bounds came from" is this process's, not the database's**,
    /// and that is a substitution rather than a translation: legacy evaluates
    /// `NOW()` inside the statement, while `from` and `to` here are bound
    /// parameters read from `OffsetDateTime::now_utc()` by
    /// `domain::service::dashboard::stats`. That module's header states what the
    /// substitution costs and buys. This doc said "ahead of the database's" until
    /// Task 21b's fix round, which described legacy's arrangement rather than
    /// this one.
    ///
    /// # What it costs, and what the endpoint now issues
    ///
    /// **Three statements per request touch the tenant's slice of
    /// `qa_test_results` on this window's expression, not two**: this one twice,
    /// once per window, and [`Self::recent_failures`] a third time. That read is
    /// bounded in its *output* by `limit`, which is not a bound on the work done
    /// to produce it, and its `ORDER BY` is the expression itself.
    ///
    /// **No index covers an expression.** `idx_qa_test_results_tenant_finished` is
    /// `(tenant_id, run_finished_at DESC)`
    /// (`m20260818_000001_initial.rs:473`) and cannot serve
    /// `COALESCE(run_finished_at, run_created_at)`. That is why the predicate is
    /// **not written as the `COALESCE`**:
    /// `infra::storage::results_sea_repo`'s `kpi_window` decomposes it into
    /// `(a >= x) OR (a IS NULL AND b >= x)`, which is an identity — its doc proves
    /// it case by case — whose branches are predicates on bare columns that the
    /// index can serve. No new index and no semantic change.
    ///
    /// `crate::domain::analytics::UniverseFilter::since` resolves the same problem
    /// by offering a bare-column path, and that escape genuinely does not apply
    /// here: it needs `run_finished_at IS NOT NULL`, which is the predicate this
    /// rule must not have. This doc concluded "there is no such option here" from
    /// that, which was too strong — the `OR` decomposition is a different option
    /// and it does apply. Corrected in Task 21b's fix round, where the
    /// decomposition was also written.
    ///
    /// **What is deliberately not claimed is a plan.** Whether Postgres chooses a
    /// `BitmapOr` over that index at the 5M-row target `cpt-cf-qa-nfr-scale`
    /// names is a question for `EXPLAIN` against real data, and no such
    /// measurement has been taken here. Nor is this ranked against
    /// [`Self::count_ingested_runs`] — this doc called it "the most expensive read
    /// on the endpoint" and that was unsupported: a `COUNT(DISTINCT run_id)` is a
    /// hash aggregate over the same slice, and which of the two dominates is
    /// exactly what has not been measured. Stated rather than discovered, in the
    /// register that method uses: if a measurement ever shows this window
    /// dominating, the remedy is a stored effective timestamp with its own index,
    /// which no task in the plan owns.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure.
    ///
    /// `repo_ids` is the dashboard's product filter — see
    /// [`Self::run_status_counts_since`]'s doc for what it admits and why `None`
    /// is unchanged behaviour.
    async fn effective_status_counts<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        from: OffsetDateTime,
        to: Option<OffsetDateTime>,
        repo_ids: Option<&[Uuid]>,
    ) -> Result<Vec<StatusRowCount>, DomainError>;

    /// The newest `limit` rows whose status is one of `statuses` and whose
    /// **effective** timestamp is at or after `since`.
    ///
    /// The port of legacy's recent-failures query
    /// (`manager/src/routes/dashboard.rs:263-279`), which the dashboard turns
    /// into `DashboardStats::failed_recent`. Same window expression as
    /// [`Self::effective_status_counts`] and the same absence of a `phase`
    /// predicate, so the card list and the counter it sits under agree on which
    /// rows exist.
    ///
    /// # `statuses` is a parameter, and that is the one place SQL sees the
    /// # vocabulary
    ///
    /// Every other read here leaves the status words to the domain — see
    /// `infra::storage::results_sea_repo::grouped_status_counts`. This one
    /// cannot: `LIMIT 10` over failures is only equivalent to legacy if the
    /// predicate is in the statement, and a domain-side filter would have to read
    /// the whole window first. The caller passes
    /// [`crate::domain::service::ingest::FAILED_STATUSES`], which is **not** a
    /// second spelling of the fold's vocabulary:
    /// [`crate::domain::service::ingest::classify`]'s `Failed` arm is defined
    /// *from* that constant, so this predicate and that fold are one definition
    /// and cannot drift. (Task 21b shipped them as two literals with a test
    /// asserting they agreed; the test could not catch the drift it named, so the
    /// fix round removed the possibility instead.) An empty slice answers with
    /// an empty `Vec` and issues no statement, the guard
    /// [`Self::run_status_counts`] documents.
    ///
    /// # Whole rows, and the card is assembled in the domain
    ///
    /// Legacy projects eight columns and maps them to its card in the handler.
    /// This returns [`TestResultRecord`] — the row shape the collections already
    /// use — and `domain::service::dashboard::failure_card` does the nine-field
    /// projection, so the field copy is pinned by a pure test with a distinct
    /// value per field rather than only by a database round trip. The columns not
    /// on the card are read and dropped, which costs a wider row for at most
    /// `limit` rows.
    ///
    /// # The order is total, where legacy's is not
    ///
    /// Legacy is `ORDER BY COALESCE(finished_at, created_at) DESC LIMIT 10`
    /// (`:277-278`) with no tiebreak, so *which* ten come back is unspecified the
    /// moment eleven rows share an instant — and rows of one run all share one.
    /// The three keys `ordered_rows` justifies are appended here
    /// (`created_at DESC, ingest_ordinal DESC, id DESC`), which refines legacy's
    /// order rather than contradicting it: any total order is a legal reading of
    /// an under-specified one, and a card list that reshuffles between two
    /// identical requests is a bug a client would report.
    ///
    /// # What it costs
    ///
    /// **The third of the endpoint's three statements over this window** — see
    /// [`Self::effective_status_counts`], which counts them and carries the
    /// index argument. The *filter* is the same decomposed `OR`, so the same
    /// index can serve it; the `ORDER BY` is the expression itself and **no
    /// decomposition helps a sort**, so a top-`limit` sort over the admitted rows
    /// remains.
    ///
    /// `limit` bounds the rows returned and **not** the work: this doc said
    /// "bounded: `limit` rows come back whatever the window holds", which is true
    /// of the output and reads as a bound on the cost. Corrected in Task 21b's fix
    /// round. What the window narrowing buys is fewer rows fed to that sort, which
    /// is a real reduction and not a removal.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure.
    ///
    /// `repo_ids` is the dashboard's product filter — see
    /// [`Self::run_status_counts_since`]'s doc for what it admits and why `None`
    /// is unchanged behaviour.
    async fn recent_failures<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        statuses: &[&str],
        since: OffsetDateTime,
        limit: u64,
        repo_ids: Option<&[Uuid]>,
    ) -> Result<Vec<TestResultRecord>, DomainError>;

    /// The `limit` flakiest `(test_name, repo_id, plan_path)` groups whose
    /// **effective** timestamp is at or after `since`, reduced, ranked and
    /// truncated in the database.
    ///
    /// The port of legacy's dashboard flaky query
    /// (`manager/src/routes/dashboard.rs:380-400`), which becomes
    /// `DashboardStats::flaky_tests` through
    /// `crate::domain::service::dashboard`'s `flaky_card`. [`FlakyGroup`] carries
    /// the grain, the representative `test_file` and the denominator; what this
    /// doc adds is the four decisions legacy makes *after* the grouping and why
    /// they are in SQL.
    ///
    /// # Legacy's reduction is pushed down, against this gear's usual direction
    ///
    /// Every other aggregate read here groups in the database and folds in the
    /// domain — [`RunStatusCount`] argues that at length, and the status
    /// vocabulary is the reason. This one also carries the `HAVING`, the
    /// `ORDER BY` and the `LIMIT`, which is legacy's shape
    /// (`dashboard.rs:393-399`) rather than this gear's default, for two reasons
    /// recorded as a controller ruling on 2026-08-21:
    ///
    /// * **Folding in the domain means returning every group in the window** so
    ///   that ten survive. `cpt-cf-qa-nfr-scale` targets 5M rows on
    ///   `qa_test_results`, and the number of `(test_name, plan)` groups in seven
    ///   days is bounded by nothing this gear controls. The same argument
    ///   [`Self::recent_failures`] makes for its `LIMIT`, one aggregation level up.
    /// * **`SecureSelect` can express it.** `project_all`
    ///   (`libs/toolkit-db/src/secure/select.rs:396`) hands the closure the
    ///   already-scoped `Select`, so `group_by`, `having`, `order_by` and `limit`
    ///   are all available and none of them can drop the tenant predicate —
    ///   unlike `into_inner()` (`:416`). Two reads in this module were moved into
    ///   SQL on that discovery (`ingested_run_ids_between`, and the
    ///   since-deleted `latest_per_test`); this one is written that way from the
    ///   start rather than moved later.
    ///
    /// What stays in the domain is the *vocabulary* and the *card*: the two
    /// status sets are parameters, exactly as [`Self::recent_failures`]' are, and
    /// the seven-field field copy is a pure function with its own test.
    ///
    /// # The two status sets, and why the denominator is not a third
    ///
    /// `passed_statuses` and `failed_statuses` are legacy's two `FILTER` sets
    /// (`dashboard.rs:386`, `:387`). The caller passes
    /// [`crate::domain::service::ingest::PASSED_STATUSES`] and
    /// [`crate::domain::service::ingest::FAILED_STATUSES`], and
    /// [`crate::domain::service::ingest::classify`]'s `Passed` and `Failed` arms
    /// are defined *from* those constants — so this predicate and that fold are
    /// one definition and cannot drift, which is the shape Task 21b's fix round
    /// arrived at for the failure set.
    ///
    /// The denominator is **derived** as their union rather than passed as a
    /// third set. Legacy spells it out as `('PASSED', 'FAILED', 'ERROR')`
    /// (`:388`), which is exactly that union; deriving it makes
    /// [`FlakyGroup::total`] provably `passed + failed` instead of a third
    /// literal that could disagree with the two above it.
    ///
    /// Either set empty answers with an empty `Vec` and issues no statement. Unlike
    /// [`Self::run_status_counts`]' guard this is a statement about the *answer*
    /// and not about portability: legacy's `HAVING` requires both counters to be
    /// positive, so with no passed statuses no group can qualify.
    ///
    /// **And it is not load-bearing, which was measured rather than assumed.**
    /// `SeaQuery` renders an empty `IN` list as the constant `1 = 2`
    /// (`libs/../sea-query-0.32.7/src/backend/query_builder.rs:386`), not as the
    /// `IN ()` that is a syntax error on some dialects — so with the guard removed
    /// the counter is `0` for every row, the `HAVING` rejects every group, and the
    /// answer is the same empty `Vec`. Deleting the guard leaves the whole suite
    /// green; `a_flaky_read_with_an_empty_status_set_answers_empty` records that
    /// and says what it does pin instead. Same shape of gap as the
    /// since-deleted `latest_per_test`'s reduction, which was recorded the same
    /// way.
    ///
    /// # The rank, and the tiebreak that makes it total
    ///
    /// Legacy ranks by `LEAST(passed, failed) DESC, total DESC`
    /// (`:395-398`) — the size of the *smaller* status group, so a test that
    /// passed 40 times and failed 40 is flakier than one that passed 79 and
    /// failed 1, and among equally flaky tests the best-evidenced first.
    ///
    /// `LEAST` is **not** written as `LEAST`: `SQLite` spells that function `MIN`,
    /// so the expression is a `CASE`, which is an identity and is one spelling for
    /// both dialects. `qa-runs`' `clamped_increment`
    /// (`qa-runs/src/infra/storage/runs_sea_repo.rs:88`) records the same
    /// substitution in the other direction (`GREATEST`/`MAX`); this doc cited it as
    /// "`queue_sea_repo`-adjacent", which is the wrong file — corrected to agree
    /// with the sibling citation on
    /// `infra::storage::results_sea_repo`'s `smaller_of`.
    ///
    /// The three grouping keys are appended as ascending tiebreaks, which refines
    /// legacy's under-specified order rather than contradicting it — the argument
    /// [`Self::recent_failures`] makes. It is total **within a dialect**: where a
    /// `NULL` `repo_id` sorts relative to a non-`NULL` one is dialect-defined, and
    /// nothing here depends on it.
    ///
    /// # The window is seven days and does not move with `days`
    ///
    /// Legacy's is `COALESCE(rr.finished_at, rr.created_at) >= NOW() - INTERVAL
    /// '7 days'` (`:391`) — the `days` query parameter governs only the daily
    /// trend. Same expression as [`Self::effective_status_counts`] and
    /// [`Self::recent_failures`], so this is the **fourth** statement on the
    /// endpoint over that window and it inherits the whole of that method's
    /// argument: legacy's third and widest row-inclusion rule, no `phase`
    /// predicate, `run_created_at` as the fallback column, and the `OR`
    /// decomposition that keeps the predicate index-usable.
    ///
    /// No upper bound, for the reason [`Self::effective_status_counts`] gives
    /// about `to: None`: legacy's flaky filter carries one bound and only one.
    ///
    /// # What it costs
    ///
    /// A hash aggregate over the admitted rows, then a top-`limit` sort over the
    /// groups. `limit` bounds the output and not the work, exactly as
    /// [`Self::recent_failures`]' does; what the window buys is fewer rows fed to
    /// the aggregate. **No planner outcome is claimed** — the shape is sargable on
    /// the filter where legacy's `COALESCE` provably is not, and whether Postgres
    /// picks a `BitmapOr` at the 5M-row target is a question for `EXPLAIN` against
    /// real data that nobody here has run.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure.
    ///
    /// `repo_ids` is the dashboard's product filter — see
    /// [`Self::run_status_counts_since`]'s doc for what it admits and why `None`
    /// is unchanged behaviour.
    #[expect(
        clippy::too_many_arguments,
        reason = "seven parameters plus `&self`: `repo_ids` is this round's addition to an \
                  already-wide signature every other field of which has its own citation above; \
                  a params struct here would have exactly one construction site \
                  (`domain::service::dashboard::DashboardService::stats`) and would unpack it \
                  again immediately"
    )]
    async fn flaky_groups<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        passed_statuses: &[&str],
        failed_statuses: &[&str],
        since: OffsetDateTime,
        limit: u64,
        repo_ids: Option<&[Uuid]>,
    ) -> Result<Vec<FlakyGroup>, DomainError>;

    /// The three counters per **`(repo_id, test_file)`** over `[since, ∞)`, for
    /// the dashboard's quality-vector pass rate.
    ///
    /// Legacy's `dashboard.rs:483-492`, the SQL half of
    /// `DashboardStats::quality_vectors_pass_rate`. The join half is the
    /// catalog's `TEST_META` vectors and lives in
    /// [`crate::domain::service::dashboard`]'s `quality_vector_pass_rates`.
    /// `repo_id` joined the grain in a fix-round-2 correction — see
    /// [`FileStatusCount`]'s own doc for why.
    ///
    /// # Ruling R5's **sixth** classification, and not a seventh
    ///
    /// `COUNT(*) FILTER (WHERE tr.status = 'PASSED')`,
    /// `... IN ('FAILED','ERROR')`, denominator `... IN ('PASSED','FAILED',
    /// 'ERROR')` (`dashboard.rs:485-487`) — the same partition
    /// [`Self::flaky_groups`] takes, indexed as row six of
    /// [`crate::domain::service::ingest`]'s table. It arrives the same way and
    /// for the same reason: as [`crate::domain::service::ingest::PASSED_STATUSES`]
    /// and [`crate::domain::service::ingest::FAILED_STATUSES`] in SQL rather than
    /// through `dashboard::kpi_of`, because that fold reduces a whole window to
    /// one pair of numbers and this needs the same partition *per file*. That
    /// module's header told Task 25 to expect exactly this, and it held.
    ///
    /// The denominator is **derived** as the union of the two sets rather than
    /// passed as a third, for [`Self::flaky_groups`]' reason: it makes
    /// [`FileStatusCount::total`] provably `passed + failed` instead of a third
    /// literal that could disagree with the two above it.
    ///
    /// Either set empty answers with an empty `Vec`, and here that is a statement
    /// about the *caller* rather than about the answer: unlike
    /// [`Self::flaky_groups`] this read has no `HAVING`, so without the guard
    /// **every** file in the window comes back as three zeros — and the fold does
    /// not drop those (see the section below), so the answer would be a
    /// full-looking array with one `total: 0` bar per vector rather than an empty
    /// one. That is worse than empty: it is a rendered denominator of nothing.
    /// Legacy cannot reach the state at all — its status words are literals in
    /// the statement.
    ///
    /// # No `HAVING` and no `LIMIT`, unlike the flaky read
    ///
    /// Legacy has neither (`dashboard.rs:483-492` is a bare `GROUP BY`), and
    /// adding either changes a rendered number rather than only a cost.
    ///
    /// **A `HAVING passed + failed > 0` would delete rows, and this doc claimed
    /// the opposite.** It said such a clause would only drop files whose rows are
    /// all `SKIPPED`, "which legacy counts as `total == 0` and the fold then
    /// contributes to no vector — the same outcome by a different route". The
    /// second half is **false**: the fold's `entry.3.insert(normalized)`
    /// (`dashboard.rs:524`) runs unconditionally inside the per-vector loop, with
    /// no test on the counters. So an all-`SKIPPED` file whose universe entry
    /// declares `Security` increments that vector's
    /// [`qa_insights_sdk::QualityVectorPassRate::tests`] by one while adding
    /// nothing to its three counters — and if it is the *only* file carrying
    /// `Security`, the rendered row is `("Security", 0, 0, 0, tests: 1)`. Adding
    /// the clause deletes that row outright. Zero-counter groups are therefore
    /// load-bearing output, not filler, and
    /// `a_file_with_no_counted_row_still_counts_toward_its_vectors_tests` is the
    /// fold-side test that fails if either side starts dropping them.
    ///
    /// A `LIMIT` would silently truncate the vector sums.
    ///
    /// **The output is bounded by distinct test files, not by rows**, which is
    /// what makes the absent `LIMIT` affordable where
    /// [`Self::list_for_universe`]'s absence of one would not be: a deployment
    /// has as many files as its plans list, i.e. hundreds, against the 5M rows
    /// `cpt-cf-qa-nfr-scale` sizes the table at. It is not bounded by the
    /// *universe*, though — the grouping key is a stored string, so a file
    /// deleted from every plan still forms a group until its rows age out of the
    /// window. Those groups match nothing in the vector map and the fold drops
    /// them (`dashboard.rs:516-518`).
    ///
    /// # `test_file <> ''` is legacy's `tr.test_file IS NOT NULL`
    ///
    /// Not a widening. The column here is `NOT NULL DEFAULT ''`
    /// (`entity::test_result::Model::test_file` records why), so `''` is this
    /// schema's spelling of legacy's `NULL` and the predicate admits exactly the
    /// same rows. Dropping it would add one group keyed on `''` that the fold
    /// then discards for having no vectors — invisible today and wrong the moment
    /// a caller counted the groups, which is why it is written rather than left
    /// to the fold.
    ///
    /// # The window is seven days and is the caller's
    ///
    /// `COALESCE(rr.finished_at, rr.created_at) >= NOW() - INTERVAL '7 days'`
    /// (`dashboard.rs:490`) — the same expression, the same open upper bound and
    /// the same absence of a `phase` predicate as [`Self::flaky_groups`] and
    /// [`Self::effective_status_counts`], so this is the **fifth** statement on
    /// the endpoint over that rule and it inherits the whole of that method's
    /// argument. `since` is a parameter rather than a literal for the reason
    /// every other read here takes one: the clock belongs to the service, which
    /// reads it once for the whole payload.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure.
    ///
    /// `repo_ids` is the dashboard's product filter — see
    /// [`Self::run_status_counts_since`]'s doc for what it admits and why `None`
    /// is unchanged behaviour.
    async fn file_status_counts<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        passed_statuses: &[&str],
        failed_statuses: &[&str],
        since: OffsetDateTime,
        repo_ids: Option<&[Uuid]>,
    ) -> Result<Vec<FileStatusCount>, DomainError>;

    /// How many distinct runs this gear holds results for.
    ///
    /// `COUNT(DISTINCT run_id)`, in the database.
    ///
    /// # It is not legacy's `total_runs`, and the difference is not a bug
    ///
    /// Legacy's dashboard total is `runs.len()` over its whole run history
    /// (`manager/src/routes/dashboard.rs:147`), which includes runs that produced
    /// no results at all. There is no cross-gear read that can answer that here:
    /// `QaRunsClientV1::list_runs`' `limit` is **mandatory**, because *"`qa_runs`
    /// grows strictly faster than the queue and never drains, so an unbounded
    /// inter-gear call would materialize every run ever executed"*
    /// (`qa-runs-sdk/src/client.rs:42-46`) — so a total taken from that listing
    /// would silently be `min(total, limit)`. The plan's own mapping row for the
    /// dashboard says the run counts are read *locally*
    /// (`plans/2026-08-18-qa-insights-gear.md:330`), and this is the local
    /// quantity that is exact: runs whose results are ingested. A run whose
    /// results have not landed yet is missing, which
    /// `cpt-cf-qa-principle-async-insights` makes a normal transient state.
    ///
    /// # What it costs
    ///
    /// An index-only scan of the tenant's slice of
    /// `idx_qa_test_results_tenant_run` — `(tenant_id, run_id)`
    /// (`m20260818_000001_initial.rs:499`). Bounded by rows rather than by runs,
    /// so on the 5M-row table `cpt-cf-qa-nfr-scale` targets it is the most
    /// expensive statement on this endpoint. Stated rather than discovered: the
    /// remedy is a per-tenant run counter maintained by ingest, which no task in
    /// the plan owns.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure.
    ///
    /// `repo_ids` is the dashboard's product filter — see
    /// [`Self::run_status_counts_since`]'s doc for what it admits and why `None`
    /// is unchanged behaviour.
    async fn count_ingested_runs<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        repo_ids: Option<&[Uuid]>,
    ) -> Result<u64, DomainError>;

    /// Every distinct `tenant_id` this table holds a row for, ascending.
    ///
    /// **The one deliberately cross-tenant read in this crate**, and the only
    /// method here whose *whole purpose* is to span tenants. Task 40's three
    /// tickers hold no request and no event envelope, so they cannot be handed a
    /// tenant; this is how they find out which tenants exist.
    /// [`TenantDirectory`](crate::domain::service::tenants::TenantDirectory) is
    /// its only caller and carries the design argument in full, including why the
    /// plan's other candidate — a scan of `qa_ingest_watermarks` — cannot work.
    ///
    /// # It is still scoped, and the implementation still applies whatever it is
    /// # handed
    ///
    /// This method itself takes `scope` as given and executes it, the same as
    /// every other method in this trait — it does not know or care whether its
    /// caller compiled `scope` from the PEP. Its actual caller,
    /// [`TenantDirectory`](crate::domain::service::tenants::TenantDirectory),
    /// no longer asks the PEP for this read at all: it passes
    /// `domain::elevated::enumeration_scope`'s `AccessScope::allow_all()`
    /// directly, at the one call site in this crate's production code sanctioned
    /// to do so — see that module's doc for why. This trait method's own
    /// contract is unchanged by that: pass it a tenant-restricted
    /// `AccessScope::for_tenants` (as
    /// `results_sea_repo`'s `the_tenant_enumeration_is_distinct_and_scoped` does,
    /// directly, without going through `TenantDirectory` at all) and it answers
    /// with whatever that narrower scope permits. R86 does not apply:
    /// this is not a `.one()`, there is no single row whose tenancy could be
    /// mistaken, and a `tenant_id` equality predicate would make the method
    /// answer its own question.
    ///
    /// # What it costs, and what it therefore is not
    ///
    /// `SELECT DISTINCT tenant_id` over `idx_qa_test_results_tenant_run`'s
    /// leading column (`m20260818_000001_initial.rs:499`) — an index-only scan,
    /// and on a 5M-row table with few tenants a loose index scan is not what
    /// every engine will choose. It is called **once per ticker pass** (300s by
    /// default), never per request and never inside a transaction, which is what
    /// makes that acceptable. Stated rather than discovered: if it ever becomes
    /// hot, the answer is a tenant directory maintained upstream, not an index
    /// here.
    ///
    /// # Errors
    ///
    /// [`DomainError::Database`] for a driver failure.
    async fn tenants_with_results<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Uuid>, DomainError>;
}
