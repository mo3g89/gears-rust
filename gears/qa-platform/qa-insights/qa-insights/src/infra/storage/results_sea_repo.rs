//! `SecureORM` implementation of [`ResultsRepository`].
//!
//! # Every statement here goes through the secure extension
//!
//! `Entity::find()`/`delete_many()`/`insert_many()` are always followed by
//! `.secure()` and a `scope_with(...)`/`scope_unchecked(...)`, which is the only
//! thing that puts the caller's `AccessScope` into the `WHERE` clause. A bare
//! `Entity::find()` compiles and reads every tenant's rows; `clippy`'s
//! `disallowed_methods` catches the read side, and nothing catches a forgotten
//! `scope_with`, so it is stated here.
//!
//! # Two reads here are paged, one is unbounded, and two reduce in the database
//!
//! [`OrmResultsRepository::list_page`] and
//! [`OrmResultsRepository::list_case_page`] are the `OData` collections, and they
//! are the only reads in this gear with a `LIMIT` the caller can move. Each is one
//! scoped select and one `paginate_odata` call — the field enum, the mapper, the
//! tiebreaker and [`PAGE_LIMITS`] as its arguments — plus the error
//! classification. Every
//! decision they encode lives somewhere the compiler or a test can see it — the
//! field allow-list in [`crate::infra::storage::odata`], the clamp in
//! [`crate::infra::storage::db`], the tiebreaker on
//! [`ResultsRepository::list_page`]'s own doc — which is why there is so little
//! code here.
//!
//! # One read here is unbounded; two reduce in the database
//!
//! [`OrmResultsRepository::list_for_universe`] returns every row its filter
//! admits, with no `LIMIT`. That is legacy's shape — its two `ExecRowRaw`
//! queries are bounded only by `app_version` and every analytics window is
//! applied in memory afterwards (`manager/src/routes/analytics.rs:961`, `:988`)
//! — and it is the method whose *contract* is "the rows an analytics read is
//! about", so there is nothing to reduce. `UniverseFilter::since` exists
//! precisely so a caller can push its window down into SQL; pagination is not
//! added because no caller exists yet to page.
//!
//! [`OrmResultsRepository::latest_per_test`] and
//! [`OrmResultsRepository::ingested_run_ids_between`] **do** reduce, in SQL,
//! through `SecureSelect::project_all`
//! (`libs/toolkit-db/src/secure/select.rs:396`). They used to materialise the
//! whole row set and fold it here, justified by the claim that `SecureSelect`
//! could not project or group — which is false: `project_all` hands the closure
//! the *already-scoped* `Select<E>`, so `select_only`, `column`, `group_by`,
//! `distinct` and a subquery are all available and cannot drop the scope
//! condition. `qa-runs`' `queue_sea_repo::platforms_with_queued_rows` records
//! the identical wrong argument being made and corrected, and was the precedent
//! followed here. The tables this is about are the two `cpt-cf-qa-nfr-scale`
//! targets 5M rows on, so "the output is small" was never a bound on the input.

use async_trait::async_trait;
use qa_insights_sdk::{TestCaseResultRecord, TestResultRecord};
use sea_orm::sea_query::{Expr, Func, SimpleExpr};
use sea_orm::{
    ActiveValue, Condition, EntityTrait, FromQueryResult, Order, QueryFilter, QueryOrder,
    QuerySelect, QueryTrait,
};
use time::OffsetDateTime;
use toolkit_db::odata::sea_orm_filter::paginate_odata;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureInsertExt, validate_tenant_in_scope,
};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::analytics::{CaseRow, ExecRow, UniverseFilter};
use crate::domain::error::DomainError;
use crate::domain::repos::{
    FileStatusCount, FlakyGroup, NewTestCaseResult, NewTestResult, PlanExecRow, ResultsRepository,
    RunStatusCount, StatusRowCount,
};
use crate::infra::storage::db::{PAGE_LIMITS, db_err, odata_err};
use crate::infra::storage::entity::test_case_result::{
    self, Column as CaseColumn, Entity as CaseEntity,
};
use crate::infra::storage::entity::test_result::{
    self, Column as ResultColumn, Entity as ResultEntity,
};
use crate::infra::storage::mapper::{
    MAX_DURATION, MAX_KEY, MAX_NAME, MAX_PATH, MAX_SHORT_TEXT, MAX_STATUS, case_row_from_result,
    exec_row_from_result, plan_exec_row_from_result, test_case_result_to_sdk, test_result_to_sdk,
    truncate, truncate_opt,
};
use crate::infra::storage::odata::{
    TestCaseResultsField, TestCaseResultsODataMapper, TestResultsField, TestResultsODataMapper,
};

/// ORM-based implementation of the `ResultsRepository` trait.
#[derive(Clone, Default)]
pub struct OrmResultsRepository;

/// `COALESCE(run_finished_at, run_created_at)` — legacy's effective **run**
/// timestamp, and the one such expression in this gear.
///
/// Legacy writes it in three places and all three coalesce to the *run's*
/// creation instant, off `run_results`: `ORDER BY COALESCE(r.finished_at,
/// r.created_at) DESC` on both `ExecRowRaw` queries
/// (`manager/src/routes/analytics.rs:971`, `:1001`), the same fallback in Rust at
/// `:1028` (`row.finished_at.unwrap_or(row.created_at)`, where `ExecRowRaw` is
/// `SELECT … r.finished_at, r.created_at`, `:961` — `:960` is the raw-string
/// opener), and the dashboard's KPI and card windows
/// (`manager/src/routes/dashboard.rs:321`, `:270`).
///
/// # It fell back to the wrong column twice, and the column is the whole story
///
/// `qa_test_results` has two candidate fallbacks and only one of them is
/// legacy's:
///
/// | | what it is | does it move? |
/// |---|---|---|
/// | `created_at` | when *this row* was written | **yes** — ingest is delete-then-insert per run, re-projected for the whole run every time the reconcile sweep or a rebuild re-reads it (`domain::service::ingest::IngestService::write_run_projection`), so it is reset to `now` for the life of the run |
/// | `run_created_at` | when the *run* was created | no |
///
/// This function read `created_at` through Task 21b. Its fix round added
/// `run_created_at` and pointed the **dashboard** reads at a second expression,
/// leaving the analytics reads here — recorded at the time as a known divergence
/// with an owner. Controller Ruling C closed that in the round after: four
/// downstream tasks (22, 23, 24, 25) fold over `list_for_universe` and
/// `latest_per_test`, so a window *and* an ordering tiebreak that disagree with
/// legacy would have been discovered by whichever of them first noticed a
/// universe it could not explain. The two expressions are now one again.
///
/// What that fixes on the analytics side: a run still in progress sorted and
/// bucketed by when its rows were last re-ingested rather than by when it
/// started, so its position in `list_for_universe`'s newest-first order — and
/// therefore which row `latest_per_test` calls latest — moved every time another
/// result landed.
///
/// # Only reached on the `finished_only == false` path, and that is the point
///
/// `UniverseFilter::since` makes the argument: no index covers a `COALESCE`, so
/// this expression means a scan and a filesort. When `finished_only` is set,
/// `run_finished_at IS NOT NULL` holds for every candidate row, the `COALESCE` is
/// *provably* the bare column, and writing the bare column is what lets
/// `idx_qa_test_results_tenant_finished` serve both the window and the sort. The
/// planner cannot make that deduction on its own.
///
/// The dashboard KPI reads have no such escape — the fallback *is* the rule they
/// port — so they take [`kpi_window`] for the predicate, which is this expression
/// decomposed into two index-usable branches, and this function for the sort.
///
/// **The second key of [`ordered_rows`]' four is still `created_at`, and that is
/// correct.** It is a tiebreak on *ingest order*, which is exactly what that
/// column is; legacy's `t.id DESC` carries the same fact for rows sharing an
/// instant. Nothing about this change touches it.
///
/// # Its in-memory twin, and the one corner where the two disagree
///
/// `infra::storage::mapper::exec_row_from_result` computes the same quantity in
/// Rust for `ExecRow::ts`, because this expression is what *selects and orders* a
/// row while that value is what the analytics cores *fold*. The two must stay the
/// same expression, and they are named on each other so a change to one is a
/// change with a second site.
///
/// **They differ in exactly one case, and it is unreachable.** If both
/// `run_finished_at` and `run_created_at` were `NULL`, this `COALESCE` yields
/// `NULL` — the row sorts as `NULL` and a `since` bound excludes it — where the
/// Rust falls back to the row's own `created_at`, because `ExecRow::ts` is not an
/// `Option`. No writer can produce such a row: `domain::service::ingest`'s
/// `project_rows` is the only production construction site of `NewTestResult`,
/// `IngestService` its only caller (the operator rebuild takes the same path),
/// and it fills `run_created_at` from `qa_runs_sdk::Run::created_at`, which is
/// non-optional. Stated here as well as on the mapper because **this** is the side
/// that behaves differently, and a reader arriving from the SQL would otherwise
/// not find it. Closing it properly means making `ExecRow::ts` an `Option`, which
/// touches every analytics core.
fn effective_ts() -> SimpleExpr {
    Func::coalesce([
        Expr::col(ResultColumn::RunFinishedAt).into(),
        Expr::col(ResultColumn::RunCreatedAt).into(),
    ])
    .into()
}

/// [`effective_ts`] `>= from`, and `< to` when there is one, written as an
/// **`OR` of
/// two index-usable branches** rather than as a predicate over the `COALESCE`.
///
/// # The rewrite is an identity, not an approximation
///
/// `COALESCE(a, b) >= x` is exactly `(a >= x) OR (a IS NULL AND b >= x)`:
///
/// * `a` not null — `COALESCE` is `a`, and the first branch is `a >= x`. The
///   second is false, because `a IS NULL` is false.
/// * `a` null — `COALESCE` is `b`, the first branch is `NULL >= x` which is
///   `NULL` and therefore not true, and the second is `b >= x`.
/// * both null — `COALESCE` is `NULL`, so the predicate is `NULL`; the first
///   branch is `NULL` and the second is `TRUE AND NULL`, also `NULL`. Excluded
///   either way.
///
/// Three-valued logic makes `a >= x` already imply `a IS NOT NULL`, so there is
/// no fourth case. Both bounds decompose the same way, and the closed window
/// carries both of them into each branch.
///
/// # Why it is written this way, and what is *not* claimed
///
/// No index covers an expression, so the `COALESCE` form is a scan of the
/// tenant's slice. Each branch here is a predicate on a bare column:
/// `run_finished_at >= x` and `run_finished_at IS NULL` are both ranges on
/// `idx_qa_test_results_tenant_finished` — `(tenant_id, run_finished_at DESC)`,
/// `m20260818_000001_initial.rs:473` — with `run_created_at` left as a filter on
/// the second branch's rows. No new index, and no semantic change.
///
/// **What is deliberately not claimed is a planner outcome.** Whether Postgres
/// picks a `BitmapOr` over that index at the 5M-row target
/// `cpt-cf-qa-nfr-scale` names is a question for `EXPLAIN` against real data,
/// and no such measurement has been taken. What is asserted here is only that
/// the *shape* is sargable where the `COALESCE` provably is not, which is the
/// part a reader can check without a database.
///
/// `recent_failures`' `ORDER BY` still uses [`effective_ts`] and still needs a
/// sort; this reduces the rows fed to it rather than removing it.
fn kpi_window(from: OffsetDateTime, to: Option<OffsetDateTime>) -> Condition {
    let mut finished = Condition::all().add(Expr::col(ResultColumn::RunFinishedAt).gte(from));
    let mut unfinished = Condition::all()
        .add(Expr::col(ResultColumn::RunFinishedAt).is_null())
        .add(Expr::col(ResultColumn::RunCreatedAt).gte(from));

    if let Some(to) = to {
        finished = finished.add(Expr::col(ResultColumn::RunFinishedAt).lt(to));
        unfinished = unfinished.add(Expr::col(ResultColumn::RunCreatedAt).lt(to));
    }

    Condition::any().add(finished).add(unfinished)
}

/// `COUNT(CASE WHEN status IN (…) THEN 1 END)` — legacy's
/// `COUNT(*) FILTER (WHERE tr.status IN (…))` in a spelling both dialects accept.
///
/// # `CASE` rather than `FILTER`, and it is an exact identity
///
/// A `CASE` with no `ELSE` yields `NULL` for a non-matching row, and `COUNT` of a
/// scalar counts non-`NULL` values — so this counts exactly the matching rows,
/// which is what `FILTER` does. `FILTER` itself is Postgres and `SQLite` ≥ 3.30
/// only and `SeaQuery` has no builder for it; the `CASE` is one expression for
/// every dialect this gear's two test tiers run on. `clamped_increment` in
/// `qa-runs`' `runs_sea_repo` records the same substitution for `GREATEST`.
///
/// # The vocabulary is the caller's, and that is the exception this module states
///
/// Every other read here leaves the status words to the domain —
/// [`grouped_status_counts`] records that as a property of the module. This one
/// takes them, for the reason
/// [`crate::domain::service::ingest::PASSED_STATUSES`] argues: a `HAVING` over
/// two status partitions cannot be applied after a domain-side fold. An empty
/// slice renders as `1 = 2` rather than as `IN ()` — `SeaQuery` special-cases it
/// (`sea-query-0.32.7/src/backend/query_builder.rs:386`) — so it is not a syntax
/// error and this counter would simply be `0`. The caller guards against it
/// anyway, and [`ResultsRepository::flaky_groups`] records both what the guard
/// says about the answer and that no test can catch its removal.
fn status_count(statuses: &[&str]) -> SimpleExpr {
    let matched: SimpleExpr = Expr::case(
        Expr::col(ResultColumn::Status).is_in(statuses.iter().copied()),
        1,
    )
    .into();
    Func::count(matched).into()
}

/// `LEAST(left, right)` as a `CASE`, because `SQLite` spells that function `MIN`.
///
/// Legacy ranks the flaky list by `LEAST(passed, failed) DESC`
/// (`manager/src/routes/dashboard.rs:395-398`) — the size of the smaller status
/// group. `CASE WHEN left < right THEN left ELSE right END` is the same value
/// wherever neither side is `NULL`, and neither side can be: both arguments are
/// [`status_count`]s, and `COUNT` never returns `NULL`.
///
/// Both arguments are used twice in the rendered SQL, which is why this takes
/// references and clones rather than consuming: the caller holds one expression
/// per counter and hands the same one to the select list, the `HAVING` and this.
fn smaller_of(left: &SimpleExpr, right: &SimpleExpr) -> SimpleExpr {
    Expr::case(Expr::expr(left.clone()).lt(right.clone()), left.clone())
        .finally(right.clone())
        .into()
}

/// The quantity `list_for_universe` orders by and `latest_per_test` takes the
/// maximum of, for one filter.
///
/// The bare column on the `finished_only` path and the `COALESCE` otherwise —
/// the same choice [`ordered_rows`] makes, factored out so the `ORDER BY`, the
/// `MAX(...)` and the subquery's join key cannot drift into three different
/// notions of "latest".
fn sort_key(filter: &UniverseFilter) -> SimpleExpr {
    if filter.finished_only {
        Expr::col(ResultColumn::RunFinishedAt).into()
    } else {
        effective_ts()
    }
}

/// The `WHERE` clause of legacy's two `ExecRowRaw` queries (`:959-1010`), as far
/// as this schema can express it.
///
/// `finished_only` is `r.phase IN ('Succeeded', 'Failed')` widened to
/// `run_finished_at IS NOT NULL` — this gear has no phase column, and
/// `UniverseFilter::finished_only` records that the substitute is wider than the
/// original.
fn universe_condition(filter: &UniverseFilter) -> Condition {
    let mut cond = Condition::all();

    if let Some(version) = &filter.product_version {
        cond = cond.add(Expr::col(ResultColumn::ProductVersion).eq(version.as_str()));
    }
    if let Some(branch) = &filter.branch {
        cond = cond.add(Expr::col(ResultColumn::Branch).eq(branch.as_str()));
    }
    if filter.finished_only {
        cond = cond.add(Expr::col(ResultColumn::RunFinishedAt).is_not_null());
    }
    if let Some(since) = filter.since {
        cond = cond.add(if filter.finished_only {
            Expr::col(ResultColumn::RunFinishedAt).gte(since)
        } else {
            Expr::expr(effective_ts()).gte(since)
        });
    }
    if !filter.plans.is_empty() {
        // An OR of `(repo_id, plan_path)` equalities rather than a tuple `IN`,
        // which `SQLite` does not support. One entry is legacy's plan scope
        // (`WHERE r.plan_id = $2`); many is the expressible half of its "all"
        // scope (`plan_id = ANY($3)`).
        let mut plans = Condition::any();
        for plan in &filter.plans {
            plans = plans.add(
                Condition::all()
                    .add(Expr::col(ResultColumn::RepoId).eq(plan.repo_id))
                    .add(Expr::col(ResultColumn::PlanPath).eq(plan.plan_path.as_str())),
            );
        }
        cond = cond.add(plans);
    }
    cond
}

/// The ordered, scoped read both analytics methods share.
///
/// # The tiebreak is FOUR keys, and each of the middle two is load-bearing
///
/// `sort_key DESC, created_at DESC, ingest_ordinal DESC, id DESC`.
///
/// **Do not "simplify" this to three keys. A three-key ordering without
/// `created_at` is not legacy-equivalent, and shipping one was a measured
/// regression** — `45dafe9b` dropped that term and made
/// `a_tie_across_two_runs_is_won_by_the_later_ingested_one` fail, on a case the
/// commit before it got right.
///
/// Legacy orders `COALESCE(finished_at, created_at) DESC, t.id DESC`
/// (`analytics.rs:971`, `:1001`) and re-sorts in memory with a **stable** sort
/// (`:1042`), so the SQL tiebreak survives into the result;
/// `latest_per_test_snapshot`'s first-wins loop consumes it (`:1615-1632`, its own
/// comment at `:1625`).
///
/// `t.id` is a **global** `SERIAL`, and that single column carries *two* facts.
/// This schema needs one key for each:
///
/// * **Across runs sharing a timestamp it is ingest order** — the run written
///   later has the higher serial. `created_at DESC` reproduces that: one instant
///   is stamped per batch, monotonic with ingest order.
/// * **Within one run it is parse order** — the bulk writer inserts in the order
///   the parser produced (`argo.rs:2598-2615`). `ingest_ordinal DESC` reproduces
///   that: the ordinal is the row's position in its batch.
///
/// Together they emulate a global serial descending exactly — a later batch sorts
/// ahead of an earlier one, and within a batch a later row sorts ahead of an
/// earlier one — which is what makes the *pair* equivalent to legacy where
/// neither key is on its own. Each has its own test, and each turns exactly one
/// red when removed: `a_tie_across_two_runs_is_won_by_the_later_ingested_one` for
/// `created_at`, `the_last_row_of_a_batch_wins_a_tie_on_one_test_file` for the
/// ordinal.
///
/// `id DESC` is a fourth key only to make the order **total**, so a repeated read
/// cannot reorder two rows that somehow share all three preceding keys. Reaching
/// it needs a writer that duplicated an ordinal within a batch, which
/// `ordinal_of` cannot do.
///
/// The decision record is in the plan, under Task 12's heading "DECIDED — the
/// within-run tiebreak of `list_for_universe`": option A, chosen 2026-08-20, with
/// why the three alternatives were declined.
///
/// The ordering is authoritative for callers: every "latest wins" map in the
/// cores keeps the first hit for a file, so a core must not re-sort on `ts`
/// alone. Legacy's own re-sort is stable precisely so it does not.
async fn ordered_rows<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    filter: &UniverseFilter,
) -> Result<Vec<test_result::Model>, DomainError> {
    let query = ResultEntity::find()
        .secure()
        .scope_with(scope)
        .filter(universe_condition(filter));

    query
        .order_by(sort_key(filter), Order::Desc)
        .order_by(Expr::col(ResultColumn::CreatedAt), Order::Desc)
        .order_by(Expr::col(ResultColumn::IngestOrdinal), Order::Desc)
        .order_by(Expr::col(ResultColumn::Id), Order::Desc)
        .all(runner)
        .await
        .map_err(db_err)
}

#[async_trait]
impl ResultsRepository for OrmResultsRepository {
    async fn upsert_run_results<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        run_id: Uuid,
        files: Vec<NewTestResult>,
        cases: Vec<NewTestCaseResult>,
    ) -> Result<(), DomainError> {
        // Validated once for the whole batch rather than per row: the bulk
        // inserts below go through `scope_unchecked`, because
        // `scope_with_model` takes a single `ActiveModel` and there is no
        // batch form. Skipping this check is what would make `tenant_id` a
        // free parameter — a caller could write another tenant's rows and the
        // scope filter on the *reads* would then hide them from everyone.
        //
        // **It checks `owner_tenant_id` and nothing else** — see this method's
        // header, "The delete is scope-filtered and the insert is not". A caller
        // handing in a scope that constrains anything further has to have
        // refused it already; this line will not.
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;

        let now = OffsetDateTime::now_utc();

        // Delete-then-insert, per **run** and not per row, and deliberately in
        // the caller's runner rather than a transaction of our own: Task 14's
        // reconcile sweep and rebuild both open one transaction per run around
        // this call, and a repository that opened its own instead would break
        // the atomicity that makes a reprojection exactly-once. Legacy dedupes
        // the same way
        // (`manager/src/routes/runs.rs:1153-1185` per test,
        // `manager/src/services/argo.rs:2593-2615` per run); the tuple carries
        // no unique index — `a_repeated_result_tuple_is_accepted` pins its
        // absence — so this is an application invariant and nothing in the
        // database will notice if it is dropped.
        ResultEntity::delete_many()
            .filter(Condition::all().add(Expr::col(ResultColumn::RunId).eq(run_id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        CaseEntity::delete_many()
            .filter(Condition::all().add(Expr::col(CaseColumn::RunId).eq(run_id)))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        // Both tables are emptied above whether or not there is anything to put
        // back, which is what makes "this run now has no results" expressible.
        // The guards are only about `insert_many([])`, which SeaORM turns into
        // an `INSERT` with no rows.
        if !files.is_empty() {
            // **The ordinal is derived from the batch's own order, not taken from
            // a caller**, and that is a deliberate divergence from the shape the
            // review sketched (a field on `NewTestResult`).
            //
            // `files` arrives in the producer's order — Task 13 builds it from
            // qa-runs' per-test list — which is exactly what legacy's parse order
            // is, so `enumerate()` *is* the value. A caller-supplied field could
            // be duplicated, sparse, or transposed, and none of those would fail
            // to compile; a derived one cannot drift from the order the rows are
            // actually inserted in.
            //
            // Splitting one run's results across two calls would restart the
            // ordinals, but that is already broken for a stronger reason: this
            // method deletes the whole run first, so the second call would erase
            // the first. No new hazard.
            let rows = files.into_iter().enumerate().map(|(index, file)| {
                new_result_am(tenant_id, run_id, file, ordinal_of(index), now)
            });
            ResultEntity::insert_many(rows)
                .secure()
                .scope_unchecked(scope)
                .map_err(db_err)?
                .exec(runner)
                .await
                .map_err(db_err)?;
        }
        if !cases.is_empty() {
            let rows = cases
                .into_iter()
                .map(|case| new_case_am(tenant_id, run_id, case, now));
            CaseEntity::insert_many(rows)
                .secure()
                .scope_unchecked(scope)
                .map_err(db_err)?
                .exec(runner)
                .await
                .map_err(db_err)?;
        }

        Ok(())
    }

    async fn list_by_run<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<Vec<TestResultRecord>, DomainError> {
        let rows = ResultEntity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(Expr::col(ResultColumn::RunId).eq(run_id)))
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(test_result_to_sdk).collect())
    }

    /// See [`ResultsRepository::case_rows_for_run`]'s doc for why this exists
    /// beside [`Self::case_rows_for_runs`]: that method's mapper is
    /// `case_row_from_result`, which projects `reason` away; this one is
    /// `test_case_result_to_sdk`, which keeps it.
    async fn case_rows_for_run<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_id: Uuid,
    ) -> Result<Vec<TestCaseResultRecord>, DomainError> {
        let rows: Vec<test_case_result::Model> = CaseEntity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(Expr::col(CaseColumn::RunId).eq(run_id)))
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(test_case_result_to_sdk).collect())
    }

    async fn list_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<TestResultRecord>, DomainError> {
        // `.secure().scope_with(scope)` before `paginate_odata`, and not
        // optionally: that function's first parameter is
        // `SecureSelect<E, Scoped>`, so an unscoped select does not type-check
        // and the caller's `$filter` is applied on top of the tenant predicate
        // rather than in place of it.
        let scoped = ResultEntity::find().secure().scope_with(scope);

        // `paginate_odata`, not `paginate_odata_try`: `test_result_to_sdk` is
        // infallible — the status column is deliberately not decoded and no
        // other column narrows — so the fallible variant would need a `MapErr`
        // type nothing could ever construct. qa-runs uses `_try` because
        // `run_to_sdk` really can fail (`runs_sea_repo.rs:248-266`).
        paginate_odata::<TestResultsField, TestResultsODataMapper, _, _, _, _>(
            scoped,
            runner,
            query,
            // `id`, descending, and it is the *unique* column on purpose: with a
            // non-unique final order key the keyset predicate skips every row
            // sharing the boundary value, and on this table a whole ingest batch
            // shares one `created_at`. That is also why no chronological
            // `$orderby` is offered at all — `ResultsRepository::list_page` has
            // the whole argument and the alternatives it rejects.
            ("id", SortDir::Desc),
            PAGE_LIMITS,
            test_result_to_sdk,
        )
        .await
        .map_err(|error| odata_err(&error))
    }

    async fn list_case_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<TestCaseResultRecord>, DomainError> {
        // See `list_page`: same shape, different entity, different field enum.
        let scoped = CaseEntity::find().secure().scope_with(scope);

        paginate_odata::<TestCaseResultsField, TestCaseResultsODataMapper, _, _, _, _>(
            scoped,
            runner,
            query,
            ("id", SortDir::Desc),
            PAGE_LIMITS,
            test_case_result_to_sdk,
        )
        .await
        .map_err(|error| odata_err(&error))
    }

    async fn list_for_universe<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        filter: &UniverseFilter,
    ) -> Result<Vec<ExecRow>, DomainError> {
        let rows = ordered_rows(runner, scope, filter).await?;
        Ok(rows.into_iter().map(exec_row_from_result).collect())
    }

    /// # Reduced in SQL: `(test_file, latest) IN (SELECT test_file, MAX(latest) … GROUP BY test_file)`
    ///
    /// The subquery is built from a **clone of the already-scoped, already-filtered
    /// `Select`**, so its `WHERE` carries the same `AccessScope` condition and the
    /// same [`universe_condition`] as the outer query by construction. That is the
    /// property that makes this safe: a hand-written correlated subquery would have
    /// had to re-spell the tenant predicate, and getting that wrong is a
    /// cross-tenant leak rather than a wrong number.
    ///
    /// `project_all` is `SecureORM`'s documented path for this
    /// (`libs/toolkit-db/src/secure/select.rs:396`) and is what makes the
    /// projection impossible to un-scope, unlike `into_inner()`, which hands back
    /// a raw `Select` and would.
    ///
    /// **This replaced a fold over the whole row set**, which was justified here by
    /// the claim that `SecureSelect` exposed only `filter`/`order_by`/`limit`/`offset`.
    /// It does not; see this module's header, and `qa-runs`'
    /// `queue_sea_repo::platforms_with_queued_rows` for the same correction made
    /// one gear earlier.
    ///
    /// # A `MAX` is not a full tiebreak, so a small fold remains — and it cannot
    /// be removed
    ///
    /// `(test_file, MAX(latest))` still admits more than one row when two rows of
    /// one file share that instant, which is exactly what ingesting a single run
    /// produces. The remaining first-wins fold runs over *ties only*, not over
    /// history, and it consumes [`ordered_rows`]' ordering — whose within-run
    /// tiebreak is now `ingest_ordinal DESC`, so the winner is legacy's.
    ///
    /// **The obvious way to make the reduction exact is wrong, and that was
    /// measured rather than reasoned about.** Adding `MAX(ingest_ordinal)` to the
    /// group and the ordinal to the tuple compiles cleanly and breaks
    /// `a_newer_run_wins_even_when_its_row_has_a_lower_ordinal`: `MAX(ordinal)` is
    /// the maximum over *every* row of the file, not over the rows at
    /// `MAX(latest)`, so an older run holding a higher ordinal makes the tuple
    /// match nothing and the file vanishes from the result. An exact single
    /// statement needs a two-level argmax — a window function over a subquery, or
    /// a correlated `NOT EXISTS` whose tenant predicate would have to be
    /// hand-written, which this method has no `tenant_id` parameter to write. The
    /// two-stage form is what ships.
    ///
    /// The caveat the trait's doc already carries still applies: this keys on the
    /// **stored** `test_file`, where Task 20's latest-map keys on the *resolved*
    /// one, so for any run whose results carry no file path the two differ.
    ///
    /// # What no test here can catch, stated rather than implied
    ///
    /// **Deleting the subquery leaves the whole suite green.** Measured: with the
    /// `in_subquery` filter removed, all tests still pass, because the fold below
    /// then produces the identical answer — that is precisely why this method
    /// could ship as a fold in the first place. The reduction is a *cost*
    /// property, and the assertions here are about the *answer*, so a future edit
    /// that quietly reverts it would not go red.
    ///
    /// What *is* covered is everything that could make the reduction return a
    /// different answer than the fold:
    /// `the_sql_reduction_picks_the_same_latest_row_the_ordered_read_does`
    /// (equivalence), `the_latest_per_test_subquery_is_scoped_to_the_caller` and
    /// `the_latest_per_test_subquery_respects_the_universe_filter` (the subquery
    /// carries the scope and the filter), and
    /// `two_rows_of_one_run_sharing_a_test_file_reduce_to_one_stable_winner` (the
    /// tie fold). Same shape of gap as `collect_sea_repo::list_counts_for`'s
    /// empty-slice guard, recorded the same way.
    async fn latest_per_test<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        filter: &UniverseFilter,
    ) -> Result<Vec<ExecRow>, DomainError> {
        let rows: Vec<test_result::Model> = ResultEntity::find()
            .secure()
            .scope_with(scope)
            .filter(universe_condition(filter))
            .project_all(runner, |query| {
                // The inner query: one row per file, carrying that file's latest
                // instant. `query.clone()` is what keeps the scope condition on
                // it — `Select<E>` is `Clone` and `QueryTrait::into_query` turns
                // it into the `SelectStatement` a subquery needs.
                let latest_per_file = query
                    .clone()
                    .select_only()
                    .column(ResultColumn::TestFile)
                    .column_as(SimpleExpr::from(Func::max(sort_key(filter))), "latest")
                    .group_by(Expr::col(ResultColumn::TestFile))
                    .into_query();

                query
                    .filter(
                        Expr::tuple([Expr::col(ResultColumn::TestFile).into(), sort_key(filter)])
                            .in_subquery(latest_per_file),
                    )
                    .order_by(sort_key(filter), Order::Desc)
                    .order_by(Expr::col(ResultColumn::CreatedAt), Order::Desc)
                    .order_by(Expr::col(ResultColumn::IngestOrdinal), Order::Desc)
                    .order_by(Expr::col(ResultColumn::Id), Order::Desc)
                    .into_model::<test_result::Model>()
            })
            .await
            .map_err(db_err)?;

        // `contains` before `insert` so the `String` is cloned only for a row that
        // is actually kept — one per file — rather than for every row scanned.
        // `HashSet<String>` borrows as `str`, so the probe itself allocates
        // nothing.
        let mut seen = std::collections::HashSet::new();
        Ok(rows
            .into_iter()
            .filter(|row| {
                !seen.contains(row.test_file.as_str()) && seen.insert(row.test_file.clone())
            })
            .map(exec_row_from_result)
            .collect())
    }

    /// # No join, and no window
    ///
    /// Legacy joins `run_results` only to translate a run *name* into the foreign
    /// key (`manager/src/routes/analytics.rs:1310-1311`);
    /// `qa_test_case_results.run_id` is already that key, so the predicate is a
    /// plain `run_id IN (…)` on one table. The trait's doc records why the set is
    /// the latest run of each universe file rather than a window.
    ///
    /// The empty-slice guard is the same one
    /// [`Self::run_status_counts`] carries and is not decoration: an
    /// `IN ()` renders as a syntactically valid always-false predicate on
    /// `Postgres` and a syntax error on some backends, and either way the round
    /// trip is one the caller already knows the answer to. The caller has its own
    /// early return for the empty case — a *different* one, because "no run named
    /// a latest" and "the read returned nothing" are two different per-case
    /// answers — so reaching here with an empty slice is a caller that did not
    /// take it.
    async fn case_rows_for_runs<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_ids: &[Uuid],
    ) -> Result<Vec<CaseRow>, DomainError> {
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }

        let rows: Vec<test_case_result::Model> = CaseEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all().add(Expr::col(CaseColumn::RunId).is_in(run_ids.iter().copied())),
            )
            .all(runner)
            .await
            .map_err(db_err)?;

        Ok(rows.into_iter().map(case_row_from_result).collect())
    }

    /// See [`ResultsRepository::list_for_plan`]'s doc for the filter, the
    /// window and the ordering decisions; this is their direct translation.
    async fn list_for_plan<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        plan_path: &str,
        since: OffsetDateTime,
    ) -> Result<Vec<PlanExecRow>, DomainError> {
        // `kpi_window`, not `Expr::expr(effective_ts()).gte(since)`: this table
        // has no index on `plan_path` at all
        // (`m20260818_000001_initial.rs:499-501`), so
        // `idx_qa_test_results_tenant_finished` — `(tenant_id, run_finished_at
        // DESC)` — is the only index that could bound this read, and it can
        // only serve a predicate on the bare column. `kpi_window`'s own doc
        // proves the `OR`-of-two-branches form is an identity with the
        // `COALESCE` this read's own NFR argument exists to avoid scanning
        // past. The `ORDER BY` below stays on the expression — `recent_failures`'
        // doc gives the reason no decomposition helps a sort.
        let rows: Vec<test_result::Model> = ResultEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(Expr::col(ResultColumn::PlanPath).eq(plan_path))
                    .add(kpi_window(since, None)),
            )
            .order_by(effective_ts(), Order::Desc)
            .order_by(Expr::col(ResultColumn::CreatedAt), Order::Desc)
            .order_by(Expr::col(ResultColumn::IngestOrdinal), Order::Desc)
            .order_by(Expr::col(ResultColumn::Id), Order::Desc)
            .all(runner)
            .await
            .map_err(db_err)?;

        Ok(rows.into_iter().map(plan_exec_row_from_result).collect())
    }

    /// See [`ResultsRepository::latest_version_for_plan`]'s doc for the whole
    /// argument — fix round 1, Important 2. Direct translation: `product_version
    /// IS NOT NULL`, the same four-key `ORDER BY` [`list_for_plan`](Self::list_for_plan)
    /// uses, and `.one()` rather than `.all()` — legacy's `LIMIT 1`.
    async fn latest_version_for_plan<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        plan_path: &str,
    ) -> Result<Option<String>, DomainError> {
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;

        let row: Option<test_result::Model> = ResultEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(Expr::col(ResultColumn::TenantId).eq(tenant_id))
                    .add(Expr::col(ResultColumn::PlanPath).eq(plan_path))
                    .add(Expr::col(ResultColumn::ProductVersion).is_not_null()),
            )
            .order_by(effective_ts(), Order::Desc)
            .order_by(Expr::col(ResultColumn::CreatedAt), Order::Desc)
            .order_by(Expr::col(ResultColumn::IngestOrdinal), Order::Desc)
            .order_by(Expr::col(ResultColumn::Id), Order::Desc)
            .one(runner)
            .await
            .map_err(db_err)?;

        Ok(row.and_then(|row| row.product_version))
    }

    /// # Half-open, and `SELECT DISTINCT run_id` in the database
    ///
    /// `[from, to)` so that consecutive reconcile windows sharing an endpoint
    /// neither skip a run nor replay one.
    ///
    /// The projection reads **one column and de-duplicates in SQL**, so the rows
    /// crossing the wire are bounded by the number of runs in the window rather
    /// than by the number of result rows those runs produced. `project_all` keeps
    /// the scope condition on the query it projects
    /// (`libs/toolkit-db/src/secure/select.rs:396`).
    ///
    /// **This replaced a fold over every result row in the window**, justified by
    /// the claim that `SecureSelect` had no `distinct()` and no column projection.
    /// It has both, through `project_all`; see this module's header.
    ///
    /// Ordered by `run_id` so the answer is deterministic across reads. It is the
    /// only column in the select list, which is what `DISTINCT` requires of an
    /// `ORDER BY`; the reconciler does not care about the order, but a
    /// non-deterministic one is a flaky test waiting to be written.
    async fn ingested_run_ids_between<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        from: OffsetDateTime,
        to: OffsetDateTime,
    ) -> Result<Vec<Uuid>, DomainError> {
        let rows: Vec<RunIdRow> = ResultEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(Expr::col(ResultColumn::RunFinishedAt).gte(from))
                    .add(Expr::col(ResultColumn::RunFinishedAt).lt(to)),
            )
            .project_all(runner, |query| {
                query
                    .select_only()
                    .column(ResultColumn::RunId)
                    .distinct()
                    .order_by(Expr::col(ResultColumn::RunId), Order::Asc)
                    .into_model::<RunIdRow>()
            })
            .await
            .map_err(db_err)?;

        Ok(rows.into_iter().map(|row| row.run_id).collect())
    }

    /// # `GROUP BY run_id, status, run_finished_at`, in the database
    ///
    /// One statement, one group per `(run, status, instant)`, and no status
    /// vocabulary in the SQL — the fold belongs to
    /// [`crate::domain::service::ingest::classify`] and
    /// [`RunStatusCount`](crate::domain::repos::RunStatusCount) says why.
    ///
    /// `run_finished_at` is a **grouping key rather than a `MAX`**, which is what
    /// keeps this dialect-neutral: it is constant per run, so grouping on it
    /// changes nothing, while an aggregate over an instant reads back differently
    /// on `SQLite` (text) and Postgres (`timestamptz`).
    ///
    /// The empty-slice guard is the same one `collect_sea_repo::list_counts_for`
    /// carries: `IN ()` is a syntax error on some dialects, and a caller with no
    /// runs to ask about wants no statement rather than a portable spelling of
    /// "false". **On this builder version it is defensive rather than
    /// load-bearing** — that sibling's doc carries the measurement, and
    /// `ResultsRepository::run_status_counts` now carries it too; `SeaQuery` renders
    /// an empty `is_in` as `1 = 2`, so nothing here can reach an `IN ()`. The guard
    /// stays for the reason the sibling gives.
    async fn run_status_counts<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        run_ids: &[Uuid],
    ) -> Result<Vec<RunStatusCount>, DomainError> {
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }

        let rows: Vec<RunStatusCountRow> = ResultEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all().add(Expr::col(ResultColumn::RunId).is_in(run_ids.iter().copied())),
            )
            .project_all(runner, grouped_status_counts)
            .await
            .map_err(db_err)?;

        Ok(rows
            .into_iter()
            .map(RunStatusCountRow::into_domain)
            .collect())
    }

    /// # The same grouping as [`Self::run_status_counts`], over a time window
    ///
    /// `run_finished_at >= since` is a range seek on
    /// `idx_qa_test_results_tenant_finished` — `(tenant_id, run_finished_at
    /// DESC)`, `m20260818_000001_initial.rs:473` — and the **bare column** is
    /// written deliberately rather than legacy's
    /// `COALESCE(finished_at, created_at)`: no index covers a `COALESCE`, which
    /// is the argument `UniverseFilter::since` makes at length. The trait's doc
    /// records both differences from legacy that this costs.
    ///
    /// No `COUNT(*) FILTER (WHERE status = …)` and no `DATE(...)` group, which is
    /// what legacy writes here (`manager/src/routes/dashboard.rs:216-231`). Both
    /// are in the domain instead, for the reasons on `RunStatusCount`.
    async fn run_status_counts_since<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        since: OffsetDateTime,
    ) -> Result<Vec<RunStatusCount>, DomainError> {
        let rows: Vec<RunStatusCountRow> = ResultEntity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(Expr::col(ResultColumn::RunFinishedAt).gte(since)))
            .project_all(runner, grouped_status_counts)
            .await
            .map_err(db_err)?;

        Ok(rows
            .into_iter()
            .map(RunStatusCountRow::into_domain)
            .collect())
    }

    /// # `GROUP BY status` over the `COALESCE` window, in the database
    ///
    /// Legacy writes six `COUNT(*) FILTER (WHERE status … AND window …)` columns
    /// in one statement (`manager/src/routes/dashboard.rs:317-348`). Two things
    /// change and neither is a behaviour change:
    ///
    /// * **`FILTER` becomes a `GROUP BY` and a domain fold.** `FILTER` is
    ///   Postgres and `SQLite` ≥ 3.30 only, and it would put the status
    ///   vocabulary in SQL — which
    ///   [`crate::domain::service::ingest::classify`] owns, for the reason
    ///   `RunStatusCount` gives. Grouping is the same partition with the words
    ///   left out.
    /// * **Two windows become two calls.** Legacy's six filters carry two
    ///   distinct windows; one call per window is one predicate per statement,
    ///   which is what lets the boundary be asserted directly.
    ///
    /// `kpi_window` is the window, and it is legacy's `COALESCE` decomposed into
    /// two bare-column branches — an identity, proved in that function's doc,
    /// which the `COALESCE` form is not index-usable enough to deserve. The
    /// trait's doc records that this rule is legacy's third and widest, and
    /// `effective_ts` records why the fallback column is `run_created_at` and not
    /// the row's own `created_at`.
    async fn effective_status_counts<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        from: OffsetDateTime,
        to: Option<OffsetDateTime>,
    ) -> Result<Vec<StatusRowCount>, DomainError> {
        let rows: Vec<StatusCountRow> = ResultEntity::find()
            .secure()
            .scope_with(scope)
            // Half-open, and open-ended when `to` is absent — the trait's doc
            // records why legacy's current window has no upper bound and why
            // substituting `now` would drop a row rather than tidy one up.
            // `kpi_window` is `COALESCE(run_finished_at, run_created_at)` in
            // that range, decomposed into two bare-column branches; its doc
            // carries the equivalence.
            .filter(kpi_window(from, to))
            .project_all(runner, |query| {
                query
                    .select_only()
                    .column(ResultColumn::Status)
                    .column_as(Expr::col(ResultColumn::Id).count(), "row_count")
                    .group_by(Expr::col(ResultColumn::Status))
                    .into_model::<StatusCountRow>()
            })
            .await
            .map_err(db_err)?;

        Ok(rows.into_iter().map(StatusCountRow::into_domain).collect())
    }

    /// # The window and the status set in SQL, the card in the domain
    ///
    /// No projection, no grouping and no subquery, so this is a plain scoped
    /// select rather than a `project_all` — the whole row is what the caller
    /// wants, and `test_result_to_sdk` is the mapper the collections already use.
    ///
    /// The `ORDER BY` is [`effective_ts`] and then the three keys [`ordered_rows`]
    /// justifies, which is legacy's single key made total; the trait's doc argues
    /// that refinement. The *filter* is [`kpi_window`], the same decomposition
    /// `effective_status_counts` uses, so the card list and the counter beside it
    /// cannot disagree about which rows exist. `limit` is applied in SQL, so a
    /// window holding a million failures still returns ten rows — and the sort
    /// remains a filesort on the expression, which `kpi_window`'s doc is explicit
    /// about not fixing.
    async fn recent_failures<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        statuses: &[&str],
        since: OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<TestResultRecord>, DomainError> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }

        let rows = ResultEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(Expr::col(ResultColumn::Status).is_in(statuses.iter().copied()))
                    .add(kpi_window(since, None)),
            )
            .order_by(effective_ts(), Order::Desc)
            .order_by(Expr::col(ResultColumn::CreatedAt), Order::Desc)
            .order_by(Expr::col(ResultColumn::IngestOrdinal), Order::Desc)
            .order_by(Expr::col(ResultColumn::Id), Order::Desc)
            .limit(limit)
            .all(runner)
            .await
            .map_err(db_err)?;

        Ok(rows.into_iter().map(test_result_to_sdk).collect())
    }

    /// # The whole reduction is in the database, which is legacy's shape rather
    /// # than this module's default
    ///
    /// `GROUP BY test_name, repo_id, plan_path`, three
    /// [`status_count`] columns, a `MAX(test_file)`, legacy's two-sided `HAVING`,
    /// its `ORDER BY` and its `LIMIT` — one statement, at most `limit` rows out.
    /// The trait's doc carries the ruling that put the `HAVING`/`ORDER BY`/`LIMIT`
    /// here instead of in the domain, and [`FlakyGroup`] carries the grain and the
    /// denominator.
    ///
    /// Five substitutions, none of them a behaviour change, each with its own
    /// reason recorded where it is made:
    ///
    /// * `COUNT(*) FILTER (WHERE …)` becomes `COUNT(CASE WHEN … THEN 1 END)` —
    ///   [`status_count`].
    /// * `LEAST(passed, failed)` becomes a `CASE` — [`smaller_of`].
    /// * `COALESCE(rr.finished_at, rr.created_at) >= …` becomes the `OR` of two
    ///   bare-column branches — [`kpi_window`], and `effective_ts` records why the
    ///   fallback column is `run_created_at` rather than the row's own
    ///   `created_at`.
    /// * `rr.plan_id` becomes the `(repo_id, plan_path)` pair, and legacy's
    ///   `JOIN run_results` disappears with it: both columns are denormalized onto
    ///   `qa_test_results`, which is the whole point of the eight copies
    ///   `entity::test_result`' header lists.
    /// * The denominator's `('PASSED', 'FAILED', 'ERROR')` becomes the union of
    ///   the two sets the caller passed, so it cannot disagree with them.
    ///
    /// # Each counter expression is built once and used two or three times
    ///
    /// `passed` and `failed` each appear in the select list, in the `HAVING` and
    /// inside [`smaller_of`]; `total` in the select list and the `ORDER BY`. They
    /// are `clone`d rather than rebuilt, because two `status_count` calls with
    /// *different arguments* is precisely the defect this shape prevents — an
    /// `ORDER BY` ranking on a partition the select list does not report would be
    /// correct SQL and an unexplainable list.
    ///
    /// The `ORDER BY` repeats the expressions rather than naming the aliases:
    /// ordering by an output alias is Postgres-and-`SQLite` behaviour that
    /// `SeaQuery` would render as an unqualified identifier, and an alias that
    /// collides with a column name resolves to the column instead. `total` would
    /// be the one to do it.
    async fn flaky_groups<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        passed_statuses: &[&str],
        failed_statuses: &[&str],
        since: OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<FlakyGroup>, DomainError> {
        // Not the `IN ()` guard the other reads carry — `SeaQuery` renders an
        // empty `IN` as `1 = 2` rather than as a syntax error, so there is nothing
        // portable to protect. What this says is that with no passed or no failed
        // status legacy's two-sided `HAVING` admits no group at all, so empty is
        // the *answer*. The trait's doc carries the measurement, including that
        // removing this line changes nothing a test can see.
        if passed_statuses.is_empty() || failed_statuses.is_empty() {
            return Ok(Vec::new());
        }

        let passed = status_count(passed_statuses);
        let failed = status_count(failed_statuses);
        // Legacy's `('PASSED', 'FAILED', 'ERROR')` (`dashboard.rs:388`) derived as
        // the union of what the caller passed, rather than spelled a third time.
        let counted: Vec<&str> = passed_statuses
            .iter()
            .chain(failed_statuses.iter())
            .copied()
            .collect();
        let total = status_count(&counted);

        let rows: Vec<FlakyGroupRow> = ResultEntity::find()
            .secure()
            .scope_with(scope)
            // Legacy's `>= NOW() - INTERVAL '7 days'` (`dashboard.rs:391`), open
            // above for the reason `effective_status_counts` gives about its own
            // upper bound.
            .filter(kpi_window(since, None))
            .project_all(runner, move |query| {
                query
                    .select_only()
                    .column(ResultColumn::TestName)
                    // Legacy's own representative pick (`dashboard.rs:384`), not
                    // an invention of this port; `FlakyGroup::test_file` records
                    // what it means and how `''` differs from legacy's `NULL`.
                    .column_as(
                        SimpleExpr::from(Func::max(Expr::col(ResultColumn::TestFile))),
                        "test_file",
                    )
                    .column(ResultColumn::RepoId)
                    .column(ResultColumn::PlanPath)
                    .column_as(passed.clone(), "passed")
                    .column_as(failed.clone(), "failed")
                    .column_as(total.clone(), "total")
                    .group_by(Expr::col(ResultColumn::TestName))
                    .group_by(Expr::col(ResultColumn::RepoId))
                    .group_by(Expr::col(ResultColumn::PlanPath))
                    // Legacy's `HAVING … > 0 AND … > 0` (`:393-394`), as two
                    // calls: `QuerySelect::having` ANDs into one clause.
                    .having(Expr::expr(passed.clone()).gt(0))
                    .having(Expr::expr(failed.clone()).gt(0))
                    .order_by(smaller_of(&passed, &failed), Order::Desc)
                    .order_by(total, Order::Desc)
                    // Legacy's order stops here and is not total; the three
                    // grouping keys make it so. The trait's doc argues the
                    // refinement and notes that `NULL` placement within them is
                    // dialect-defined.
                    .order_by(Expr::col(ResultColumn::TestName), Order::Asc)
                    .order_by(Expr::col(ResultColumn::RepoId), Order::Asc)
                    .order_by(Expr::col(ResultColumn::PlanPath), Order::Asc)
                    .limit(limit)
                    .into_model::<FlakyGroupRow>()
            })
            .await
            .map_err(db_err)?;

        Ok(rows.into_iter().map(FlakyGroupRow::into_domain).collect())
    }

    /// # `flaky_groups` without the `HAVING`, the `LIMIT` or the rank
    ///
    /// Three counters, one `GROUP BY`, and the same three status expressions
    /// built from the caller's two sets. What it does **not** share is the
    /// grouping key (`test_file`, not the `(test_name, repo_id, plan_path)`
    /// triple) or any of the three clauses legacy attaches only to the flaky
    /// query — the trait's doc tabulates the difference and argues why adding
    /// either clause here would change a rendered number.
    ///
    /// The counter expressions are cloned rather than rebuilt: each is used once
    /// in the select list here, where `flaky_groups` needs its two a second and
    /// third time for the `HAVING` and the `ORDER BY`. Kept in locals anyway so
    /// that `total`'s derivation sits beside the two it is derived from.
    ///
    /// No `ORDER BY`. Legacy has none either (`dashboard.rs:483-492`) — the
    /// ordering that reaches the screen is the fold's, by summed total
    /// descending (`:537`) — so imposing one here would be a guess a test would
    /// then pin. The trait's doc says the read declares no order and the tests
    /// sort before asserting.
    async fn file_status_counts<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        passed_statuses: &[&str],
        failed_statuses: &[&str],
        since: OffsetDateTime,
    ) -> Result<Vec<FileStatusCount>, DomainError> {
        // **Not** `flaky_groups`' cosmetic guard. That one is measured to change
        // nothing because its two-sided `HAVING` rejects every group anyway; this
        // read has no `HAVING`, so without this line every file comes back as
        // three zeros — and the fold keeps zero-counter groups deliberately, so
        // the answer would be one `total: 0` bar per vector rather than an empty
        // array. `an_empty_status_set_answers_empty_rather_than_all_zeroes` pins
        // it, and the trait's doc carries why zero counters are real output.
        if passed_statuses.is_empty() || failed_statuses.is_empty() {
            return Ok(Vec::new());
        }

        let passed = status_count(passed_statuses);
        let failed = status_count(failed_statuses);
        // Legacy's `('PASSED', 'FAILED', 'ERROR')` (`dashboard.rs:487`) derived as
        // the union of what the caller passed, rather than spelled a third time —
        // the same derivation `flaky_groups` makes from `:388`.
        let counted: Vec<&str> = passed_statuses
            .iter()
            .chain(failed_statuses.iter())
            .copied()
            .collect();
        let total = status_count(&counted);

        let rows: Vec<FileStatusCountRow> = ResultEntity::find()
            .secure()
            .scope_with(scope)
            // Legacy's `>= NOW() - INTERVAL '7 days'` (`dashboard.rs:490`), open
            // above for the reason `effective_status_counts` gives about its own
            // upper bound.
            .filter(kpi_window(since, None))
            // Legacy's `tr.test_file IS NOT NULL` (`:491`). `''` is this schema's
            // spelling of that `NULL` — the column is `NOT NULL DEFAULT ''` — so
            // this admits exactly the rows legacy's predicate does.
            .filter(Condition::all().add(Expr::col(ResultColumn::TestFile).ne("")))
            .project_all(runner, move |query| {
                query
                    .select_only()
                    .column(ResultColumn::TestFile)
                    .column_as(passed, "passed")
                    .column_as(failed, "failed")
                    .column_as(total, "total")
                    .group_by(Expr::col(ResultColumn::TestFile))
                    .into_model::<FileStatusCountRow>()
            })
            .await
            .map_err(db_err)?;

        Ok(rows
            .into_iter()
            .map(FileStatusCountRow::into_domain)
            .collect())
    }

    /// # `COUNT(DISTINCT run_id)`, in the database
    ///
    /// One row out, whatever the table holds. `project_all` keeps the tenant
    /// predicate on it, so the count is the caller's slice and not the table's.
    ///
    /// An empty projection returns one row carrying `0` rather than no rows —
    /// aggregates without a `GROUP BY` always produce exactly one row — so the
    /// `map_or` below is a defence against a driver that disagrees, not the
    /// empty-table path. `the_run_total_counts_runs_and_not_rows` covers both.
    async fn count_ingested_runs<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<u64, DomainError> {
        let rows: Vec<RunTotalRow> = ResultEntity::find()
            .secure()
            .scope_with(scope)
            .project_all(runner, |query| {
                query
                    .select_only()
                    .column_as(Expr::col(ResultColumn::RunId).count_distinct(), "run_total")
                    .into_model::<RunTotalRow>()
            })
            .await
            .map_err(db_err)?;

        Ok(rows
            .first()
            .map_or(0, |row| u64::try_from(row.run_total).unwrap_or(0)))
    }

    /// # `SELECT DISTINCT tenant_id`, in the database, over a one-column
    /// # projection
    ///
    /// Same shape as [`Self::ingested_run_ids_between`]'s distinct run-id read
    /// and for the same reason: `project_all` is what makes a one-column
    /// projection impossible to un-scope, and the `DISTINCT` belongs in SQL so
    /// what crosses the wire is bounded by the number of tenants rather than by
    /// the number of rows they own.
    ///
    /// Ordered ascending so a ticker's pass visits tenants in a stable order
    /// across passes. That is not cosmetic: a `stopped_at_gap` WARN for one
    /// tenant is easier to correlate across passes when the surrounding log lines
    /// are in the same order, and an unordered `DISTINCT` is free to differ per
    /// execution plan.
    async fn tenants_with_results<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Uuid>, DomainError> {
        let rows: Vec<TenantIdRow> = ResultEntity::find()
            .secure()
            .scope_with(scope)
            .project_all(runner, |query| {
                query
                    .select_only()
                    .column(ResultColumn::TenantId)
                    .distinct()
                    .order_by(Expr::col(ResultColumn::TenantId), Order::Asc)
                    .into_model::<TenantIdRow>()
            })
            .await
            .map_err(db_err)?;

        Ok(rows.into_iter().map(|row| row.tenant_id).collect())
    }
}

/// The one-column projection [`OrmResultsRepository::tenants_with_results`]
/// reads.
#[derive(Debug, FromQueryResult)]
struct TenantIdRow {
    tenant_id: Uuid,
}

/// The `(run, status, instant)` grouping both counter reads share.
///
/// Factored out because the two differ only in their `WHERE` clause, and two
/// copies of a `select_only`/`group_by` chain are two places for a column to be
/// dropped from the group — which SQL will not always refuse and which would
/// silently merge two runs' counters.
fn grouped_status_counts(
    query: sea_orm::Select<ResultEntity>,
) -> sea_orm::Selector<sea_orm::SelectModel<RunStatusCountRow>> {
    query
        .select_only()
        .column(ResultColumn::RunId)
        .column(ResultColumn::Status)
        .column(ResultColumn::RunFinishedAt)
        .column_as(Expr::col(ResultColumn::Id).count(), "row_count")
        .group_by(Expr::col(ResultColumn::RunId))
        .group_by(Expr::col(ResultColumn::Status))
        .group_by(Expr::col(ResultColumn::RunFinishedAt))
        .into_model::<RunStatusCountRow>()
}

/// One group of [`grouped_status_counts`].
///
/// A named struct because `project_all` needs a `FromQueryResult`, and the field
/// names are the column names — the same runtime-string binding `RunIdRow`
/// records. `row_count` rather than `rows`: `ROWS` is a reserved word in
/// Postgres (window frames), and an alias that needs quoting is an alias waiting
/// to break.
#[derive(Debug, FromQueryResult)]
struct RunStatusCountRow {
    run_id: Uuid,
    status: String,
    run_finished_at: Option<OffsetDateTime>,
    row_count: i64,
}

impl RunStatusCountRow {
    /// `COUNT(*)` is signed in every dialect this ships on and cannot be
    /// negative; a negative would be a driver defect, and `0` is the answer that
    /// cannot make a dashboard show a nonsense number.
    fn into_domain(self) -> RunStatusCount {
        RunStatusCount {
            run_id: self.run_id,
            status: self.status,
            rows: u64::try_from(self.row_count).unwrap_or(0),
            run_finished_at: self.run_finished_at,
        }
    }
}

/// The one-row projection [`OrmResultsRepository::count_ingested_runs`] reads.
#[derive(Debug, FromQueryResult)]
struct RunTotalRow {
    run_total: i64,
}

/// One group of [`OrmResultsRepository::effective_status_counts`].
///
/// The unweighted sibling of [`RunStatusCountRow`], and `row_count` is spelled
/// the same way for the same reason: `ROWS` is a reserved word in Postgres.
#[derive(Debug, FromQueryResult)]
struct StatusCountRow {
    status: String,
    row_count: i64,
}

impl StatusCountRow {
    /// Same clamp as [`RunStatusCountRow::into_domain`] and for the same reason:
    /// a negative `COUNT` would be a driver defect, and `0` is the answer that
    /// cannot make a pass rate nonsense.
    fn into_domain(self) -> StatusRowCount {
        StatusRowCount {
            status: self.status,
            rows: u64::try_from(self.row_count).unwrap_or(0),
        }
    }
}

/// One group of [`OrmResultsRepository::flaky_groups`].
///
/// Seven fields against [`FlakyGroup`]'s seven, and the field names are the
/// column aliases — the same runtime-string binding [`RunIdRow`] records, which
/// here covers four aliases the projection invents (`test_file`, `passed`,
/// `failed`, `total`) rather than only column names. None of the four needs
/// quoting in Postgres: `PASSED` is not a reserved word and neither are the other
/// three, unlike the `rows` that made [`RunStatusCountRow`] spell its count
/// `row_count`.
///
/// `test_file` is a plain `String` and not an `Option`: it is
/// `MAX(test_file)` over a group that has at least one row by construction, and
/// the column is `NOT NULL`, so the aggregate cannot be `NULL`. `""` is the
/// no-file case — see [`FlakyGroup::test_file`].
#[derive(Debug, FromQueryResult)]
struct FlakyGroupRow {
    test_name: String,
    test_file: String,
    repo_id: Option<Uuid>,
    plan_path: Option<String>,
    passed: i64,
    failed: i64,
    total: i64,
}

impl FlakyGroupRow {
    /// Same clamp as [`RunStatusCountRow::into_domain`] and for the same reason:
    /// a negative `COUNT` would be a driver defect, and `0` is the answer that
    /// cannot make a flaky card render a nonsense number.
    ///
    /// **Every field is named explicitly** rather than moved by a struct-update
    /// wildcard, which is what makes a column added to [`FlakyGroup`] later a
    /// compile error here. Three of the seven are same-typed integers and **two**
    /// are same-typed `String`s — `test_name` and `test_file`; `plan_path` is an
    /// `Option<String>` on both sides, so transposing it with either does not
    /// compile. A transposition inside either of the two real groups is correct
    /// Rust, and two existing tests pin it:
    /// `the_representative_file_is_the_alphabetically_last_one_legacy_picks`
    /// asserts `("test_files", "tests/z.py")`, so a swapped name and file fails
    /// there; `the_flaky_denominator_counts_only_passed_failed_and_error` asserts
    /// `(2, 4, 6)` over three deliberately unequal counters, so any pair of them
    /// swapped fails there. Both were observed red under exactly those mutations.
    ///
    /// **This doc cited `the_flaky_groups_reach_the_domain_row_field_for_field`
    /// through Task 23b's review round, and no such test exists** — the mutations
    /// were caught, but by the two tests named above rather than by the one named
    /// here. A doc that names its witness is only worth as much as the grep that
    /// checks the name.
    fn into_domain(self) -> FlakyGroup {
        FlakyGroup {
            test_name: self.test_name,
            test_file: self.test_file,
            repo_id: self.repo_id,
            plan_path: self.plan_path,
            passed: u64::try_from(self.passed).unwrap_or(0),
            failed: u64::try_from(self.failed).unwrap_or(0),
            total: u64::try_from(self.total).unwrap_or(0),
        }
    }
}

/// One group of [`OrmResultsRepository::file_status_counts`].
///
/// Four fields against [`FileStatusCount`]'s four, and the field names are the
/// column aliases — the same runtime-string binding [`RunIdRow`] records. Three
/// of the four aliases are invented by the projection (`passed`, `failed`,
/// `total`) and none needs quoting in Postgres, exactly as
/// [`FlakyGroupRow`]'s do not.
///
/// `test_file` is a plain `String` and not an `Option`, and here it is the
/// *grouping key* rather than [`FlakyGroupRow`]'s `MAX()` — the column is
/// `NOT NULL` and the read filters `<> ''`, so the group can be neither `NULL`
/// nor empty.
#[derive(Debug, FromQueryResult)]
struct FileStatusCountRow {
    test_file: String,
    passed: i64,
    failed: i64,
    total: i64,
}

impl FileStatusCountRow {
    /// Same clamp as [`RunStatusCountRow::into_domain`] and for the same reason:
    /// a negative `COUNT` would be a driver defect, and `0` is the answer that
    /// cannot make a pass rate nonsense.
    ///
    /// **Every field is named explicitly**, which makes a column added to
    /// [`FileStatusCount`] later a compile error here. Three of the four are
    /// same-typed integers and a transposition among them is correct Rust;
    /// `the_denominator_counts_only_passed_failed_and_error` asserts `(2, 3, 5)`
    /// over three deliberately unequal counters, so any pair of them swapped
    /// fails there. Observed red under exactly that mutation.
    fn into_domain(self) -> FileStatusCount {
        FileStatusCount {
            test_file: self.test_file,
            passed: u64::try_from(self.passed).unwrap_or(0),
            failed: u64::try_from(self.failed).unwrap_or(0),
            total: u64::try_from(self.total).unwrap_or(0),
        }
    }
}

/// A batch index as the `INTEGER` `ingest_ordinal` column.
///
/// Saturates rather than failing: the column is a tiebreak among rows that share
/// a timestamp, and a run with more than `i32::MAX` result rows would have to
/// lose *something* — losing tiebreak precision on the 2-billionth row of one run
/// is strictly better than refusing the whole batch, which is what `try_from`
/// plus `?` would do. The clamp is unreachable in any real run and is written
/// down rather than left to `as`, which the workspace denies anyway.
fn ordinal_of(index: usize) -> i32 {
    i32::try_from(index).unwrap_or(i32::MAX)
}

/// The one-column projection [`OrmResultsRepository::ingested_run_ids_between`]
/// reads.
///
/// A named struct rather than a tuple because `project_all` needs a
/// `FromQueryResult`, and the column name in the derive is what binds it to
/// `qa_test_results.run_id` — a rename there and a silent `None` here is the
/// same runtime-string hazard `entity/mod.rs`' header is about, which is why
/// `ingested_run_ids_are_distinct_and_the_window_is_half_open` reads a real row
/// back rather than only counting.
#[derive(Debug, FromQueryResult)]
struct RunIdRow {
    run_id: Uuid,
}

/// One [`NewTestResult`] as a row.
///
/// **Every field is named explicitly, and `..Default::default()` is not used
/// even though it would compile** — `sea-orm-macros` derives `Default` for every
/// `ActiveModel`. A wildcard would absorb any column a later migration adds
/// *without a compile error*, silently writing its default through this path.
/// That is not hypothetical for this exact struct: `app_build` was added to the
/// schema in this task's own Step 0b, and an exhaustive literal is what makes
/// the next such column stop the build here, where the decision belongs.
fn new_result_am(
    tenant_id: Uuid,
    run_id: Uuid,
    file: NewTestResult,
    ordinal: i32,
    now: OffsetDateTime,
) -> test_result::ActiveModel {
    test_result::ActiveModel {
        id: ActiveValue::Set(Uuid::new_v4()),
        tenant_id: ActiveValue::Set(tenant_id),
        run_id: ActiveValue::Set(run_id),
        test_file: ActiveValue::Set(truncate(
            file.test_file,
            MAX_PATH,
            "qa_test_results.test_file",
        )),
        test_name: ActiveValue::Set(truncate(
            file.test_name,
            MAX_NAME,
            "qa_test_results.test_name",
        )),
        // The vocabulary is an open set, so the only thing done to it is making
        // it fit the column — which is what stops `VARCHAR(16)` re-closing the
        // set on Postgres by turning a long status into a 500 and a dropped
        // result. See `mapper`'s header.
        status: ActiveValue::Set(truncate(file.status, MAX_STATUS, "qa_test_results.status")),
        duration: ActiveValue::Set(truncate_opt(
            file.duration,
            MAX_DURATION,
            "qa_test_results.duration",
        )),
        launch_id: ActiveValue::Set(truncate_opt(
            file.launch_id,
            MAX_SHORT_TEXT,
            "qa_test_results.launch_id",
        )),
        jira_key: ActiveValue::Set(truncate_opt(
            file.jira_key,
            MAX_KEY,
            "qa_test_results.jira_key",
        )),
        product_version: ActiveValue::Set(truncate_opt(
            file.product_version,
            MAX_SHORT_TEXT,
            "qa_test_results.product_version",
        )),
        app_build: ActiveValue::Set(truncate_opt(
            file.app_build,
            MAX_SHORT_TEXT,
            "qa_test_results.app_build",
        )),
        platform_id: ActiveValue::Set(file.platform_id),
        repo_id: ActiveValue::Set(file.repo_id),
        plan_path: ActiveValue::Set(truncate_opt(
            file.plan_path,
            MAX_PATH,
            "qa_test_results.plan_path",
        )),
        branch: ActiveValue::Set(truncate_opt(
            file.branch,
            MAX_NAME,
            "qa_test_results.branch",
        )),
        run_finished_at: ActiveValue::Set(file.run_finished_at),
        run_created_at: ActiveValue::Set(file.run_created_at),
        // The batch position, not a caller-supplied value — see
        // `upsert_run_results`' comment on why it is derived here.
        ingest_ordinal: ActiveValue::Set(ordinal),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
    }
}

/// One [`NewTestCaseResult`] as a row. Exhaustive for the reason
/// [`new_result_am`] gives.
///
/// `reason` is the one producer column not truncated: it is `TEXT`, not a
/// bounded `VARCHAR`, because the xfail/skip explanation is prose.
fn new_case_am(
    tenant_id: Uuid,
    run_id: Uuid,
    case: NewTestCaseResult,
    now: OffsetDateTime,
) -> test_case_result::ActiveModel {
    test_case_result::ActiveModel {
        id: ActiveValue::Set(Uuid::new_v4()),
        tenant_id: ActiveValue::Set(tenant_id),
        run_id: ActiveValue::Set(run_id),
        test_file: ActiveValue::Set(truncate(
            case.test_file,
            MAX_PATH,
            "qa_test_case_results.test_file",
        )),
        nodeid: ActiveValue::Set(truncate(
            case.nodeid,
            MAX_PATH,
            "qa_test_case_results.nodeid",
        )),
        name: ActiveValue::Set(truncate(case.name, MAX_NAME, "qa_test_case_results.name")),
        status: ActiveValue::Set(truncate(
            case.status,
            MAX_STATUS,
            "qa_test_case_results.status",
        )),
        duration: ActiveValue::Set(truncate_opt(
            case.duration,
            MAX_DURATION,
            "qa_test_case_results.duration",
        )),
        reason: ActiveValue::Set(case.reason),
        ticket: ActiveValue::Set(truncate_opt(
            case.ticket,
            MAX_KEY,
            "qa_test_case_results.ticket",
        )),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ActiveValue, Condition, EntityTrait};
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    use crate::domain::analytics::{PlanRef, UniverseFilter};
    use crate::domain::error::DomainError;
    use crate::domain::repos::{
        FileStatusCount, NewTestCaseResult, NewTestResult, ResultsRepository, StatusRowCount,
    };
    use crate::infra::storage::db::PAGE_LIMITS;
    use crate::infra::storage::entity::test_case_result::{
        Column as CaseColumn, Entity as CaseEntity,
    };
    use crate::infra::storage::entity::test_result::{
        self, Column as ResultColumn, Entity as ResultEntity,
    };
    use crate::infra::storage::results_sea_repo::OrmResultsRepository;
    use crate::infra::storage::test_db::{inmem_db, now, scope};
    use sea_orm::sea_query::Expr;
    use toolkit_db::secure::{SecureEntityExt, secure_insert};
    use toolkit_odata::{CursorV1, ODataQuery};
    use toolkit_security::AccessScope;

    const TENANT: u128 = 0xA;
    /// The plan path `list_for_plan`'s own tests filter on.
    const PLAN: &str = "plans/nightly/plan.yaml";

    fn result_row(file: &str, name: &str, status: &str) -> NewTestResult {
        NewTestResult {
            test_file: file.to_owned(),
            test_name: name.to_owned(),
            status: status.to_owned(),
            duration: Some("1.23s".to_owned()),
            launch_id: Some("7204".to_owned()),
            jira_key: Some("VHP-1".to_owned()),
            product_version: Some("5.0.1".to_owned()),
            app_build: Some("20260818.3".to_owned()),
            platform_id: Some(Uuid::from_u128(0x11)),
            repo_id: Some(Uuid::from_u128(0x12)),
            plan_path: Some("plans/smoke/plan.yaml".to_owned()),
            branch: Some("main".to_owned()),
            run_finished_at: Some(now()),
            // A distinct instant from `run_finished_at` and from the `created_at`
            // the repository mints, so the enumeration guard below can tell all
            // three apart.
            run_created_at: Some(now() - Duration::hours(2)),
        }
    }

    fn case_row(file: &str, name: &str, status: &str) -> NewTestCaseResult {
        NewTestCaseResult {
            test_file: file.to_owned(),
            nodeid: format!("{file}::{name}[1]"),
            name: name.to_owned(),
            status: status.to_owned(),
            duration: Some("0.31s".to_owned()),
            reason: Some("known upstream defect".to_owned()),
            ticket: Some("VHP-9".to_owned()),
        }
    }

    /// A default filter that admits every row, so a test that cares about one
    /// predicate sets only that one.
    ///
    /// `finished_only` is `false` here even though every caller in the gear will
    /// set it: the default filter must be the *widest* one, so a predicate that
    /// silently stopped being applied shows up as extra rows rather than as
    /// none.
    fn any_row() -> UniverseFilter {
        UniverseFilter::default()
    }

    /// **The write-side enumeration guard for `qa_test_results`.**
    ///
    /// Every one of the **fourteen** columns a caller can reach — the fields of
    /// [`NewTestResult`] — is set to a distinguishable value and read back,
    /// because `new_result_am` is a **twenty**-field struct literal and a field
    /// that took the wrong source — or a
    /// column that a later migration adds and nobody wires up — is correct Rust
    /// that writes the wrong row. Two columns are the case in point, and both
    /// were added by the task that needed them rather than one task later:
    /// `app_build` in Task 12's own Step 0b, where a mapper that had kept writing
    /// `None` would have emptied Task 24's build distribution on a screen that
    /// still rendered; and `run_created_at` in Task 21b's fix round, where a
    /// mapper writing `None` would have made every unfinished run's rows fall
    /// out of the dashboard's 24-hour window instead of into it.
    #[tokio::test]
    async fn a_runs_result_rows_are_written_with_every_column() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let run_id = Uuid::from_u128(0x20);

        let before = OffsetDateTime::now_utc();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                run_id,
                vec![result_row("tests/a.py", "test_a", "PASSED")],
                vec![],
            )
            .await
            .unwrap();
        let after = OffsetDateTime::now_utc();

        let rows = OrmResultsRepository
            .list_by_run(&conn, &scope(tenant), run_id)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.run_id, run_id);
        assert_eq!(row.test_file, "tests/a.py");
        assert_eq!(row.test_name, "test_a");
        assert_eq!(row.status, "PASSED");
        assert_eq!(row.duration.as_deref(), Some("1.23s"));
        assert_eq!(row.launch_id.as_deref(), Some("7204"));
        assert_eq!(row.jira_key.as_deref(), Some("VHP-1"));
        assert_eq!(row.product_version.as_deref(), Some("5.0.1"));
        assert_eq!(
            row.app_build.as_deref(),
            Some("20260818.3"),
            "app_build is the analytics *projection* and product_version the \
             *filter*; a write path that dropped it would empty Task 24's build \
             distribution silently"
        );
        assert_eq!(row.platform_id, Some(Uuid::from_u128(0x11)));
        assert_eq!(row.repo_id, Some(Uuid::from_u128(0x12)));
        assert_eq!(row.plan_path.as_deref(), Some("plans/smoke/plan.yaml"));
        assert_eq!(row.branch.as_deref(), Some("main"));
        assert_eq!(row.run_finished_at, Some(now()));
        assert_eq!(
            row.run_created_at,
            Some(now() - Duration::hours(2)),
            "run_created_at is the fallback half of the dashboard's KPI window; a \
             write path that dropped it would make every unfinished run's rows \
             fall out of that window rather than into it"
        );
        // And `run_created_at` is not the row's own `created_at`, which the
        // repository mints from its own clock. Bracketed by two readings of that
        // clock taken either side of the write, for the reason
        // `the_case_collection_maps_every_column_to_its_own_field` records at
        // length: `assert!(created_at > now())` fails on a machine whose clock
        // predates the 2026-08-18 fixture date and passes trivially for ever
        // afterwards. The bracket holds on any clock and says more — that this
        // column carries the instant of *this* write, so a transposition with
        // `run_created_at` (a fixture instant two hours before it) fails here.
        assert!(
            row.created_at >= before && row.created_at <= after,
            "created_at is the repository's clock and must be the instant of this \
             write, inside [{before}, {after}]: {}",
            row.created_at,
        );
    }

    /// **The write-side enumeration guard for `qa_test_case_results`.**
    ///
    /// It reads the entity back through the secure extension rather than through
    /// [`test_case_result_to_sdk`], and it kept doing so after Task 17 gave the
    /// table a reader and that conversion: the two paths are what this test
    /// exists to keep independent. `new_case_am` could drop `nodeid`, `reason` or
    /// `ticket`, and a test that read the row back through the same
    /// column-name pairing the writer used would not see it.
    ///
    /// (Through Task 16 the reason was simply that the conversion did not
    /// exist — `ResultsRepository` had five methods and none read case rows.
    /// Task 17 added the reader; this test's justification changed, its shape
    /// did not.)
    #[tokio::test]
    async fn a_runs_case_rows_are_written_with_every_column() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let run_id = Uuid::from_u128(0x20);

        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                run_id,
                vec![result_row("tests/a.py", "test_a", "PASSED")],
                vec![case_row("tests/a.py", "test_a", "XFAIL")],
            )
            .await
            .unwrap();

        let cases = CaseEntity::find()
            .secure()
            .scope_with(&scope(tenant))
            .filter(Condition::all().add(Expr::col(CaseColumn::RunId).eq(run_id)))
            .all(&conn)
            .await
            .unwrap();
        assert_eq!(cases.len(), 1);
        let case = &cases[0];
        assert_eq!(case.test_file, "tests/a.py");
        assert_eq!(case.nodeid, "tests/a.py::test_a[1]");
        assert_eq!(case.name, "test_a");
        assert_eq!(case.status, "XFAIL");
        assert_eq!(case.duration.as_deref(), Some("0.31s"));
        assert_eq!(case.reason.as_deref(), Some("known upstream defect"));
        assert_eq!(case.ticket.as_deref(), Some("VHP-9"));
    }

    /// The per-case read answers **only the runs asked about**, and only within
    /// the caller's scope.
    ///
    /// Both halves in one test because both are silent when wrong: a read that
    /// ignored `run_ids` would attribute one run's cases to another file's latest
    /// run (`build_case_data` keys on the `(run_id, test_file)` pair, so the
    /// extra rows would land in a bucket nothing looks up — until two runs share
    /// a file, which is every re-run), and a read that ignored the scope would
    /// cross a tenant.
    #[tokio::test]
    async fn the_case_read_answers_only_the_requested_runs_within_the_scope() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let other_tenant = Uuid::from_u128(0x0B);
        let wanted = Uuid::from_u128(0x20);
        let unwanted = Uuid::from_u128(0x21);
        let foreign = Uuid::from_u128(0x22);

        for (owner, run_id, status) in [
            (tenant, wanted, "XFAIL"),
            (tenant, unwanted, "PASSED"),
            (other_tenant, foreign, "FAILED"),
        ] {
            OrmResultsRepository
                .upsert_run_results(
                    &conn,
                    &scope(owner),
                    owner,
                    run_id,
                    vec![result_row("tests/a.py", "test_a", "PASSED")],
                    vec![case_row("tests/a.py", "test_a", status)],
                )
                .await
                .unwrap();
        }

        let rows = OrmResultsRepository
            .case_rows_for_runs(&conn, &scope(tenant), &[wanted, foreign])
            .await
            .unwrap();

        assert_eq!(rows.len(), 1, "got {rows:?}");
        assert_eq!(rows[0].run_id, wanted);
        assert_eq!(rows[0].test_file, "tests/a.py");
        assert_eq!(rows[0].status, "XFAIL");
        assert_eq!(rows[0].ticket.as_deref(), Some("VHP-9"));
    }

    /// An empty `run_ids` performs no read and answers nothing — the guard that
    /// keeps an `IN ()` out of the SQL. The caller has its own, different early
    /// return; this one is the backstop.
    #[tokio::test]
    async fn an_empty_run_set_reads_no_case_rows() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);

        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0x20),
                vec![result_row("tests/a.py", "test_a", "PASSED")],
                vec![case_row("tests/a.py", "test_a", "XFAIL")],
            )
            .await
            .unwrap();

        let rows = OrmResultsRepository
            .case_rows_for_runs(&conn, &scope(tenant), &[])
            .await
            .unwrap();

        assert!(rows.is_empty());
    }

    // -------------------------------------------------------------------
    // `list_for_plan` — Task 27's plan drill-down read
    // -------------------------------------------------------------------

    /// `ResultsRepository::list_for_plan`'s doc, "`plan_id` matches `plan_path`,
    /// and repository is not part of it": two rows sharing a `plan_path` under
    /// **different** `repo_id`s both come back, exactly as
    /// `narrow_to_plan`'s own test proves for the query-parameter case
    /// (`a_plan_path_shared_by_two_repositories_selects_both`,
    /// `domain::service::analytics_tests`).
    #[tokio::test]
    async fn list_for_plan_matches_the_path_across_repositories() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);

        for (repo, run_id) in [
            (Uuid::from_u128(0x40), Uuid::from_u128(0x50)),
            (Uuid::from_u128(0x41), Uuid::from_u128(0x51)),
        ] {
            OrmResultsRepository
                .upsert_run_results(
                    &conn,
                    &scope(tenant),
                    tenant,
                    run_id,
                    vec![NewTestResult {
                        repo_id: Some(repo),
                        plan_path: Some(PLAN.to_owned()),
                        ..result_row("tests/a.py", "test_a", "PASSED")
                    }],
                    vec![],
                )
                .await
                .unwrap();
        }

        let rows = OrmResultsRepository
            .list_for_plan(&conn, &scope(tenant), PLAN, now() - Duration::days(1))
            .await
            .unwrap();

        assert_eq!(
            rows.len(),
            2,
            "both repositories' rows must match: {rows:?}"
        );
    }

    /// `since` excludes a row whose effective timestamp falls before it and
    /// **admits one exactly on the boundary** — `kpi_window`'s bound is `>=`,
    /// not `>` — the NFR-driven window `ResultsRepository::list_for_plan`'s
    /// doc argues for, which legacy's own unwindowed statement does not have.
    #[tokio::test]
    async fn list_for_plan_excludes_rows_older_than_since() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let since = now() - Duration::days(90);

        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0x50),
                vec![NewTestResult {
                    plan_path: Some(PLAN.to_owned()),
                    run_finished_at: Some(now() - Duration::days(200)),
                    run_created_at: Some(now() - Duration::days(200)),
                    ..result_row("tests/old.py", "test_old", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0x51),
                vec![NewTestResult {
                    plan_path: Some(PLAN.to_owned()),
                    run_finished_at: Some(now()),
                    run_created_at: Some(now()),
                    ..result_row("tests/new.py", "test_new", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();
        // Exactly on the boundary: `kpi_window`'s bound is `>= since`, so this
        // must be admitted rather than excluded — the sibling test above only
        // pins the exclusive side, one day outside.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0x52),
                vec![NewTestResult {
                    plan_path: Some(PLAN.to_owned()),
                    run_finished_at: Some(since),
                    run_created_at: Some(since),
                    ..result_row("tests/on_boundary.py", "test_on_boundary", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();

        let rows = OrmResultsRepository
            .list_for_plan(&conn, &scope(tenant), PLAN, since)
            .await
            .unwrap();

        let mut names: Vec<&str> = rows.iter().map(|r| r.test_name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["test_new", "test_on_boundary"],
            "the boundary row is included and the older-than-window row is not: {rows:?}"
        );
    }

    /// The order is the coalesced effective timestamp, newest first — a run
    /// still in progress (`run_finished_at: None`) sorts by its **creation**
    /// instant rather than falling to the back, which is
    /// `ResultsRepository::list_for_plan`'s documented divergence from legacy's
    /// bare `finished_at DESC NULLS LAST`.
    #[tokio::test]
    async fn list_for_plan_orders_by_the_coalesced_instant_newest_first() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);

        // Finished two hours ago.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0x50),
                vec![NewTestResult {
                    plan_path: Some(PLAN.to_owned()),
                    run_finished_at: Some(now() - Duration::hours(2)),
                    run_created_at: Some(now() - Duration::hours(3)),
                    ..result_row("tests/finished.py", "test_finished", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();
        // Still running: no `run_finished_at`, created one hour ago — newer than
        // the finished run's instant.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0x51),
                vec![NewTestResult {
                    plan_path: Some(PLAN.to_owned()),
                    run_finished_at: None,
                    run_created_at: Some(now() - Duration::hours(1)),
                    ..result_row("tests/running.py", "test_running", "RUNNING")
                }],
                vec![],
            )
            .await
            .unwrap();

        let rows = OrmResultsRepository
            .list_for_plan(&conn, &scope(tenant), PLAN, now() - Duration::days(1))
            .await
            .unwrap();

        assert_eq!(
            rows.iter()
                .map(|r| r.test_name.as_str())
                .collect::<Vec<_>>(),
            vec!["test_running", "test_finished"],
            "the in-progress run's creation instant is newer than the finished \
             run's finish instant: {rows:?}"
        );
    }

    // -------------------------------------------------------------------
    // `latest_version_for_plan` — fix round 1, Important 2
    // -------------------------------------------------------------------

    /// The newest versioned row's version, not the oldest.
    ///
    /// **Inserted out of chronological order on purpose**: the row with the
    /// newer `run_finished_at` is written to the database *first*, so an
    /// implementation that (by defect) picked "whichever row comes back
    /// first" or "the last one inserted" rather than actually ordering by the
    /// coalesced instant would answer with the wrong version here, not the
    /// right one by insertion-order coincidence.
    #[tokio::test]
    async fn latest_version_for_plan_returns_the_newest_version_not_the_oldest() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);

        // The newer row, inserted first.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0x70),
                vec![NewTestResult {
                    product_version: Some("2.0.0".to_owned()),
                    plan_path: Some(PLAN.to_owned()),
                    run_finished_at: Some(now()),
                    run_created_at: Some(now() - Duration::hours(1)),
                    ..result_row("tests/newer.py", "test_newer", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();

        // The older row, inserted second — later in the database's own
        // `created_at`/`id` ordering than the row above, but it must still
        // lose on `effective_ts`.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0x71),
                vec![NewTestResult {
                    product_version: Some("1.0.0".to_owned()),
                    plan_path: Some(PLAN.to_owned()),
                    run_finished_at: Some(now() - Duration::days(3)),
                    run_created_at: Some(now() - Duration::days(3) - Duration::hours(1)),
                    ..result_row("tests/older.py", "test_older", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();

        let latest = OrmResultsRepository
            .latest_version_for_plan(&conn, &scope(tenant), tenant, PLAN)
            .await
            .unwrap();

        assert_eq!(
            latest.as_deref(),
            Some("2.0.0"),
            "must be the newest row's version, not the oldest, and not whichever \
             row happened to be inserted or scanned first",
        );
    }

    /// A newer row with **no** `product_version` must not shadow an older row
    /// that has one, and must not make the answer `None`.
    ///
    /// Pins the `IS NOT NULL` half of legacy's predicate
    /// (`manager/src/services/jira_poller.rs:231-233`): an implementation that
    /// merely took the newest row overall — without filtering to versioned
    /// rows first — would answer `None` here, where the correct answer is the
    /// older, versioned row's version.
    #[tokio::test]
    async fn latest_version_for_plan_skips_a_newer_row_with_no_version() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);

        // The versioned row, older.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0x72),
                vec![NewTestResult {
                    product_version: Some("3.0.0".to_owned()),
                    plan_path: Some(PLAN.to_owned()),
                    run_finished_at: Some(now() - Duration::days(1)),
                    run_created_at: Some(now() - Duration::days(1) - Duration::hours(1)),
                    ..result_row("tests/versioned.py", "test_versioned", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();

        // The unversioned row, newer.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0x73),
                vec![NewTestResult {
                    product_version: None,
                    plan_path: Some(PLAN.to_owned()),
                    run_finished_at: Some(now()),
                    run_created_at: Some(now() - Duration::hours(1)),
                    ..result_row("tests/unversioned.py", "test_unversioned", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();

        let latest = OrmResultsRepository
            .latest_version_for_plan(&conn, &scope(tenant), tenant, PLAN)
            .await
            .unwrap();

        assert_eq!(
            latest.as_deref(),
            Some("3.0.0"),
            "the newer, unversioned row must not shadow the older, versioned one",
        );
    }

    /// No row at all for the plan (or none carrying a version) is `None`, not
    /// an error.
    #[tokio::test]
    async fn latest_version_for_plan_with_no_versioned_row_is_none() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);

        let latest = OrmResultsRepository
            .latest_version_for_plan(&conn, &scope(tenant), tenant, PLAN)
            .await
            .unwrap();

        assert_eq!(latest, None);
    }

    /// A tenant outside the caller's single-tenant scope never sees another
    /// tenant's version. This alone does not exercise the `tenant_id`
    /// predicate R86 requires — `.secure().scope_with(scope)` already
    /// excludes `other` here, since `test_db::scope` only ever compiles a
    /// scope over one tenant — so
    /// [`latest_version_for_plan_returns_the_callers_own_tenant_under_a_multi_tenant_scope`]
    /// below is the test that actually pins the predicate's own necessity.
    #[tokio::test]
    async fn latest_version_for_plan_is_scoped_to_the_caller() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(TENANT);
        let other = Uuid::from_u128(0xB);

        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(other),
                other,
                Uuid::from_u128(0x74),
                vec![NewTestResult {
                    product_version: Some("9.9.9".to_owned()),
                    plan_path: Some(PLAN.to_owned()),
                    ..result_row("tests/other_tenant.py", "test_other_tenant", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();

        let latest = OrmResultsRepository
            .latest_version_for_plan(&conn, &scope(mine), mine, PLAN)
            .await
            .unwrap();

        assert_eq!(
            latest, None,
            "another tenant's row under the same plan_path must not answer this read",
        );
    }

    /// **Controller ruling R86, the property the single-tenant-scope test
    /// above cannot exercise.** Under a scope spanning both tenants,
    /// `latest_version_for_plan` must answer with the *caller's own*
    /// tenant's version, never another in-scope tenant's — the same defect
    /// `JiraRepository::find_by_key_returns_the_callers_own_row_under_a_multi_tenant_scope`
    /// (`jira_sea_repo.rs`) pins for its own `.one()` read.
    ///
    /// # Why this test's determinism does not rest on tenant-id ordinal
    ///
    /// That `JiraRepository` test had to make the caller's tenant id the
    /// numerically **larger** of the two, because its query carries no
    /// `ORDER BY` at all and falls back to `idx_qa_jira_bugs_tenant_key`'s own
    /// `(tenant_id, jira_key)` scan order — ascending `tenant_id` — so an
    /// un-predicated read there returns the row with the *smaller* tenant id.
    /// This query is different: it always carries an explicit four-key
    /// `ORDER BY` (`effective_ts`, `created_at`, `ingest_ordinal`, `id`), none
    /// of which is `tenant_id`. So instead of relying on tenant-id ordinal,
    /// `theirs`'s row is built to sort **ahead** of `mine`'s on that
    /// `ORDER BY` (a `run_finished_at` a full day newer) — deterministically,
    /// regardless of either tenant's id. With the `tenant_id` predicate
    /// removed, `.secure().scope_with(scope)` alone still admits both rows
    /// (the spanning scope covers both), and the newer, foreign row would win
    /// the sort and be returned to a caller asking as `mine`. With the
    /// predicate in place, only `mine`'s row is ever a candidate.
    ///
    /// A genuine multi-tenant `AccessScope` is built directly
    /// (`for_tenants`), not through `test_db::scope`, which only ever
    /// compiles a single-tenant scope.
    #[tokio::test]
    async fn latest_version_for_plan_returns_the_callers_own_tenant_under_a_multi_tenant_scope() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(TENANT);
        let theirs = Uuid::from_u128(0xB);

        // `theirs`, engineered to sort ahead of `mine` on the query's own
        // `ORDER BY` — a full day newer.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(theirs),
                theirs,
                Uuid::from_u128(0x77),
                vec![NewTestResult {
                    product_version: Some("8.8.8".to_owned()),
                    plan_path: Some(PLAN.to_owned()),
                    run_finished_at: Some(now() + Duration::days(1)),
                    run_created_at: Some(now()),
                    ..result_row("tests/theirs.py", "test_theirs", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(mine),
                mine,
                Uuid::from_u128(0x78),
                vec![NewTestResult {
                    product_version: Some("9.9.9".to_owned()),
                    plan_path: Some(PLAN.to_owned()),
                    ..result_row("tests/mine.py", "test_mine", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();

        let spanning = AccessScope::for_tenants(vec![mine, theirs]);

        let latest = OrmResultsRepository
            .latest_version_for_plan(&conn, &spanning, mine, PLAN)
            .await
            .unwrap();

        assert_eq!(
            latest.as_deref(),
            Some("9.9.9"),
            "the caller's own tenant's version must come back, not the other in-scope \
             tenant's newer one",
        );
    }

    /// Re-ingest replaces the **case** rows too, in the same call.
    ///
    /// A partial replacement leaves a run whose file-level and case-level counts
    /// disagree, which is why the trait's contract is both tables in one
    /// transaction rather than two idempotent halves.
    #[tokio::test]
    async fn re_ingesting_a_run_replaces_its_case_rows_as_well() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let run_id = Uuid::from_u128(0x20);

        for _ in 0..2 {
            OrmResultsRepository
                .upsert_run_results(
                    &conn,
                    &scope(tenant),
                    tenant,
                    run_id,
                    vec![result_row("tests/a.py", "test_a", "PASSED")],
                    vec![case_row("tests/a.py", "test_a", "XFAIL")],
                )
                .await
                .unwrap();
        }

        let cases = CaseEntity::find()
            .secure()
            .scope_with(&scope(tenant))
            .filter(Condition::all().add(Expr::col(CaseColumn::RunId).eq(run_id)))
            .all(&conn)
            .await
            .unwrap();
        assert_eq!(cases.len(), 1, "redelivery must not duplicate case rows");
    }

    /// A re-ingest that reports **no** results empties the run rather than
    /// leaving the previous batch behind.
    ///
    /// The delete runs unconditionally and the inserts are guarded, so "this run
    /// now has no results" is expressible. A guard that skipped the delete on an
    /// empty batch would make a run's projection un-shrinkable.
    #[tokio::test]
    async fn re_ingesting_a_run_with_no_results_empties_it() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let run_id = Uuid::from_u128(0x20);

        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                run_id,
                vec![result_row("tests/a.py", "test_a", "PASSED")],
                vec![case_row("tests/a.py", "test_a", "XFAIL")],
            )
            .await
            .unwrap();
        OrmResultsRepository
            .upsert_run_results(&conn, &scope(tenant), tenant, run_id, vec![], vec![])
            .await
            .unwrap();

        assert!(
            OrmResultsRepository
                .list_by_run(&conn, &scope(tenant), run_id)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            CaseEntity::find()
                .secure()
                .scope_with(&scope(tenant))
                .filter(Condition::all().add(Expr::col(CaseColumn::RunId).eq(run_id)))
                .all(&conn)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// One run's re-ingest must not touch another run's rows: the delete is
    /// predicated on `run_id`.
    #[tokio::test]
    async fn re_ingesting_one_run_leaves_another_runs_rows_alone() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let (first, second) = (Uuid::from_u128(0x20), Uuid::from_u128(0x21));

        for run in [first, second] {
            OrmResultsRepository
                .upsert_run_results(
                    &conn,
                    &scope(tenant),
                    tenant,
                    run,
                    vec![result_row("tests/a.py", "test_a", "PASSED")],
                    vec![],
                )
                .await
                .unwrap();
        }
        OrmResultsRepository
            .upsert_run_results(&conn, &scope(tenant), tenant, first, vec![], vec![])
            .await
            .unwrap();

        assert_eq!(
            OrmResultsRepository
                .list_by_run(&conn, &scope(tenant), second)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// One tenant's results are invisible to another, and a re-ingest under one
    /// tenant's scope cannot delete the other's rows.
    ///
    /// The second half is the sharper one: the delete is
    /// `WHERE run_id = ?` plus the scope filter, and a missing `scope_with` there
    /// would compile and would silently wipe every tenant's rows for that run.
    #[tokio::test]
    async fn a_runs_results_are_invisible_to_another_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);
        let run_id = Uuid::from_u128(0x20);

        for tenant in [mine, theirs] {
            OrmResultsRepository
                .upsert_run_results(
                    &conn,
                    &scope(tenant),
                    tenant,
                    run_id,
                    vec![result_row("tests/a.py", "test_a", "PASSED")],
                    vec![],
                )
                .await
                .unwrap();
        }

        OrmResultsRepository
            .upsert_run_results(&conn, &scope(mine), mine, run_id, vec![], vec![])
            .await
            .unwrap();

        assert!(
            OrmResultsRepository
                .list_by_run(&conn, &scope(mine), run_id)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            OrmResultsRepository
                .list_by_run(&conn, &scope(theirs), run_id)
                .await
                .unwrap()
                .len(),
            1,
            "another tenant's rows for the same run must survive"
        );
    }

    /// **`SQLite` does not enforce `VARCHAR` widths, and the truncation is
    /// therefore the only thing that keeps the status vocabulary open.**
    ///
    /// The first half of this test asserts the non-enforcement, so the test
    /// cannot quietly become the reason nobody noticed: `INFRASTRUCTURE_ERROR` is
    /// 20 characters and `status` is `VARCHAR(16)`, so on Postgres an untruncated
    /// write raises `22001` — a 500 that drops a real result on the one path whose
    /// whole purpose is to accept whatever the runner said.
    #[tokio::test]
    async fn an_over_long_status_is_truncated_to_fit_its_column() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);
        let run_id = Uuid::from_u128(0x20);
        let long = "INFRASTRUCTURE_ERROR";
        assert_eq!(long.len(), 20, "the fixture must exceed VARCHAR(16)");

        // Written straight through the entity, bypassing the repository's
        // truncation: this is the assertion that the test *tier* would not have
        // caught an over-long write on its own.
        secure_insert::<ResultEntity>(
            test_result::ActiveModel {
                id: ActiveValue::Set(Uuid::new_v4()),
                tenant_id: ActiveValue::Set(tenant),
                run_id: ActiveValue::Set(Uuid::from_u128(0x21)),
                test_file: ActiveValue::Set("tests/a.py".to_owned()),
                test_name: ActiveValue::Set("test_a".to_owned()),
                status: ActiveValue::Set(long.to_owned()),
                duration: ActiveValue::Set(None),
                launch_id: ActiveValue::Set(None),
                jira_key: ActiveValue::Set(None),
                product_version: ActiveValue::Set(None),
                app_build: ActiveValue::Set(None),
                platform_id: ActiveValue::Set(None),
                repo_id: ActiveValue::Set(None),
                plan_path: ActiveValue::Set(None),
                branch: ActiveValue::Set(None),
                run_finished_at: ActiveValue::Set(None),
                run_created_at: ActiveValue::Set(None),
                ingest_ordinal: ActiveValue::Set(0),
                created_at: ActiveValue::Set(now()),
                updated_at: ActiveValue::Set(now()),
            },
            &ctx,
            &conn,
        )
        .await
        .expect("SQLite has type affinity, not width: it stores this happily");

        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                run_id,
                vec![result_row("tests/a.py", "test_a", long)],
                vec![],
            )
            .await
            .unwrap();

        assert_eq!(
            OrmResultsRepository
                .list_by_run(&conn, &ctx, run_id)
                .await
                .unwrap()[0]
                .status,
            "INFRASTRUCTURE_E",
            "truncation is by char to the column's sixteen; a rejected status \
             would be a lost result, which is the worse outcome"
        );
    }

    /// **`ExecRow::build` reads `app_build`, and `ts` falls back to
    /// `created_at`.**
    ///
    /// Both are derived rather than copied, and `build` is the field Task 11
    /// deliberately shipped without because the column did not exist. `None` is
    /// carried through rather than resolved to legacy's `"unknown"`: that label is
    /// the consumer's (`analytics.rs:1032-1033`), and applying it here would make
    /// "no build reported" indistinguishable from "the literal string unknown".
    #[tokio::test]
    async fn the_analytics_row_carries_the_build_and_derives_its_timestamp() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        let with_build = Uuid::from_u128(0x20);
        let unfinished = Uuid::from_u128(0x21);
        // Read before either write, so `ts` can be asserted to be *older* than
        // the repository's own clock — which is what separates the run's
        // creation instant from the row's.
        let before = OffsetDateTime::now_utc();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                with_build,
                vec![result_row("tests/a.py", "test_a", "PASSED")],
                vec![],
            )
            .await
            .unwrap();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                unfinished,
                vec![NewTestResult {
                    app_build: None,
                    run_finished_at: None,
                    ..result_row("tests/b.py", "test_b", "RUNNING")
                }],
                vec![],
            )
            .await
            .unwrap();

        let rows = OrmResultsRepository
            .list_for_universe(&conn, &ctx, &any_row())
            .await
            .unwrap();
        let finished = rows
            .iter()
            .find(|r| r.run_id == with_build)
            .expect("the finished run's row");
        assert_eq!(finished.build.as_deref(), Some("20260818.3"));
        assert_eq!(finished.ts, now());
        assert_eq!(finished.day, now().date());

        let running = rows
            .iter()
            .find(|r| r.run_id == unfinished)
            .expect("the in-progress run's row");
        assert_eq!(
            running.build, None,
            "an absent build stays absent; the 'unknown' label is Task 24's"
        );
        assert_eq!(
            running.ts,
            now() - Duration::hours(2),
            "with no run_finished_at, ts falls back to the *run's* creation \
             instant (`result_row`'s `run_created_at`, two hours back) rather \
             than being dropped or zeroed"
        );
        assert!(
            running.ts < before,
            "and it is not the row's own created_at, which this write stamped \
             from the repository's clock. That is the column this read used until \
             controller Ruling C, and `effective_ts` records why an unfinished \
             run sorted by it moved every time another of its results landed"
        );
    }

    /// Newest first, which is what makes every "latest wins" map in the cores
    /// correct: they iterate and keep the first hit for a file.
    #[tokio::test]
    async fn the_analytics_read_returns_rows_newest_first() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        let older = now();
        let newer = older + Duration::days(1);
        for (run, at) in [(0x20_u128, older), (0x21, newer)] {
            OrmResultsRepository
                .upsert_run_results(
                    &conn,
                    &ctx,
                    tenant,
                    Uuid::from_u128(run),
                    vec![NewTestResult {
                        run_finished_at: Some(at),
                        ..result_row("tests/a.py", "test_a", "PASSED")
                    }],
                    vec![],
                )
                .await
                .unwrap();
        }

        let rows = OrmResultsRepository
            .list_for_universe(
                &conn,
                &ctx,
                &UniverseFilter {
                    finished_only: true,
                    ..any_row()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![newer, older],
            "descending on the effective timestamp"
        );
    }

    /// `latest_per_test` keeps the newest row per **stored** `test_file`, over
    /// exactly the order `list_for_universe` returns.
    #[tokio::test]
    async fn the_latest_row_per_test_file_is_the_newest_one() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        let older = now();
        let newer = older + Duration::days(1);
        for (run, at, status) in [(0x20_u128, older, "FAILED"), (0x21, newer, "PASSED")] {
            OrmResultsRepository
                .upsert_run_results(
                    &conn,
                    &ctx,
                    tenant,
                    Uuid::from_u128(run),
                    vec![
                        NewTestResult {
                            run_finished_at: Some(at),
                            ..result_row("tests/a.py", "test_a", status)
                        },
                        NewTestResult {
                            run_finished_at: Some(at),
                            ..result_row("tests/b.py", "test_b", status)
                        },
                    ],
                    vec![],
                )
                .await
                .unwrap();
        }

        let mut latest = OrmResultsRepository
            .latest_per_test(
                &conn,
                &ctx,
                &UniverseFilter {
                    finished_only: true,
                    ..any_row()
                },
            )
            .await
            .unwrap();
        latest.sort_by(|a, b| a.test_file.cmp(&b.test_file));

        assert_eq!(latest.len(), 2, "one row per file: {latest:?}");
        assert!(
            latest.iter().all(|r| r.status == "PASSED" && r.ts == newer),
            "each must be the newer of the two: {latest:?}"
        );
    }

    /// The three settled predicates, each one applied.
    ///
    /// One test rather than three because the risk is a predicate silently *not*
    /// applied, and the way that shows up is extra rows — so every assertion here
    /// is "exactly the rows this predicate admits", against a fixture that has a
    /// non-matching row for each.
    #[tokio::test]
    async fn the_universe_filter_applies_version_branch_and_plan() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        let wanted = Uuid::from_u128(0x20);
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                wanted,
                vec![result_row("tests/a.py", "test_a", "PASSED")],
                vec![],
            )
            .await
            .unwrap();
        // One row differing in each predicate, so a dropped predicate shows up.
        for (run, row) in [
            (
                0x21_u128,
                NewTestResult {
                    product_version: Some("4.9.0".to_owned()),
                    ..result_row("tests/b.py", "test_b", "PASSED")
                },
            ),
            (
                0x22,
                NewTestResult {
                    branch: Some("release/5.0".to_owned()),
                    ..result_row("tests/c.py", "test_c", "PASSED")
                },
            ),
            (
                0x23,
                NewTestResult {
                    plan_path: Some("plans/regression/plan.yaml".to_owned()),
                    ..result_row("tests/d.py", "test_d", "PASSED")
                },
            ),
            (
                0x24,
                NewTestResult {
                    run_finished_at: None,
                    ..result_row("tests/e.py", "test_e", "RUNNING")
                },
            ),
        ] {
            OrmResultsRepository
                .upsert_run_results(&conn, &ctx, tenant, Uuid::from_u128(run), vec![row], vec![])
                .await
                .unwrap();
        }

        let rows = OrmResultsRepository
            .list_for_universe(
                &conn,
                &ctx,
                &UniverseFilter {
                    product_version: Some("5.0.1".to_owned()),
                    since: Some(now() - Duration::hours(1)),
                    plans: vec![PlanRef {
                        repo_id: Uuid::from_u128(0x12),
                        plan_path: "plans/smoke/plan.yaml".to_owned(),
                    }],
                    branch: Some("main".to_owned()),
                    finished_only: true,
                },
            )
            .await
            .unwrap();

        assert_eq!(
            rows.iter().map(|r| r.run_id).collect::<Vec<_>>(),
            vec![wanted],
            "each predicate must exclude its own non-matching row: {rows:?}"
        );
    }

    /// An **absent** branch is every branch — the half of the branch predicate
    /// that `the_universe_filter_applies_version_branch_and_plan` above does not
    /// reach.
    ///
    /// Legacy spells it as a guard inside the SQL —
    /// `$N::text IS NULL OR COALESCE(NULLIF(r.source_ref,''), NULLIF(r.test_version,'')) = $N`
    /// (`manager/src/routes/analytics.rs:967-970`, `:997-1000`) — with the route
    /// parameter documented as *"Absent = all branches (unchanged behavior)"*
    /// (`:24-27`). Here it is the `if let Some(branch)` at
    /// [`universe_condition`], so the failure mode is not a wrong `NULL`
    /// comparison but an unconditional predicate: a `branch` that stopped being
    /// optional would silently narrow every unfiltered analytics read to whatever
    /// default it acquired, and `qa_insights_sdk`'s callers would see a smaller
    /// universe rather than an error.
    ///
    /// **Added by Task 20**, whose brief asks for this rule as a core test named
    /// `an_absent_branch_filter_matches_every_row` over
    /// `filter_by_branch(&rows, ..)`. It cannot live there: the branch is not a
    /// field of `crate::domain::analytics::ExecRow` — deliberately, because in
    /// legacy it is a predicate and not a projection — so the rule has no pure
    /// function to be a test of. It is here, where the predicate is.
    #[tokio::test]
    async fn an_absent_branch_filter_matches_every_row() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        for (run, branch) in [(0x20_u128, "main"), (0x21, "release/5.0")] {
            OrmResultsRepository
                .upsert_run_results(
                    &conn,
                    &ctx,
                    tenant,
                    Uuid::from_u128(run),
                    vec![NewTestResult {
                        branch: Some(branch.to_owned()),
                        ..result_row("tests/a.py", "test_a", "PASSED")
                    }],
                    vec![],
                )
                .await
                .unwrap();
        }

        let unfiltered = UniverseFilter {
            product_version: Some("5.0.1".to_owned()),
            finished_only: true,
            ..UniverseFilter::default()
        };
        let all = OrmResultsRepository
            .list_for_universe(&conn, &ctx, &unfiltered)
            .await
            .unwrap();
        assert_eq!(all.len(), 2, "absent = all branches: {all:?}");

        let one = OrmResultsRepository
            .list_for_universe(
                &conn,
                &ctx,
                &UniverseFilter {
                    branch: Some("main".to_owned()),
                    ..unfiltered
                },
            )
            .await
            .unwrap();
        assert_eq!(one.len(), 1, "a named branch admits only its own rows");
    }

    /// `since` is a lower bound on the effective timestamp, and it is applied on
    /// the `finished_only == false` path too — where it becomes the `COALESCE`
    /// that no index covers.
    ///
    /// # Three runs, because two could not say *which* column the fallback reads
    ///
    /// This test had two: an old finished run (excluded) and an unfinished one
    /// (admitted). It stayed green through controller Ruling C's column change and
    /// that was the problem — with a one-day window, the row's `created_at` (this
    /// write's `now()`) and `result_row`'s `run_created_at` (two hours back) are
    /// *both* inside it, so the assertion could not tell them apart. It pinned
    /// that a fallback exists, not that it is legacy's.
    ///
    /// The third run is what discriminates: unfinished, created **thirty days
    /// ago**, with rows written a moment ago. Under `run_created_at` it is outside
    /// the window; under the row's `created_at` it is inside. The old two-run
    /// version admits it and passes.
    #[tokio::test]
    async fn the_since_bound_applies_to_the_coalesced_timestamp() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        let old = now() - Duration::days(30);
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                Uuid::from_u128(0x20),
                vec![NewTestResult {
                    run_finished_at: Some(old),
                    ..result_row("tests/a.py", "test_a", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();
        // No finish instant at all, so its effective timestamp is the run's
        // creation instant — `result_row`'s `run_created_at`, two hours back,
        // comfortably inside the window.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                Uuid::from_u128(0x21),
                vec![NewTestResult {
                    run_finished_at: None,
                    ..result_row("tests/b.py", "test_b", "RUNNING")
                }],
                vec![],
            )
            .await
            .unwrap();

        // Unfinished *and* long-running: no finish instant, and a run created
        // thirty days ago whose rows were written by this test a moment ago. This
        // is the row that separates the two candidate fallback columns.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                Uuid::from_u128(0x22),
                vec![NewTestResult {
                    run_finished_at: None,
                    run_created_at: Some(old),
                    ..result_row("tests/c.py", "test_c", "RUNNING")
                }],
                vec![],
            )
            .await
            .unwrap();

        let rows = OrmResultsRepository
            .list_for_universe(
                &conn,
                &ctx,
                &UniverseFilter {
                    since: Some(now() - Duration::days(1)),
                    ..any_row()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| r.test_file.as_str())
                .collect::<Vec<_>>(),
            vec!["tests/b.py"],
            "the unfinished run created two hours ago is admitted through the \
             fallback; the old finished run is excluded by its finish instant, \
             and the unfinished run created thirty days ago is excluded by the \
             fallback, which only holds if the fallback is the *run's* creation \
             instant and not the row's write time"
        );
    }

    /// **A long-running run does not sort to the front just because its rows were
    /// re-ingested a moment ago** — the ordering half of controller Ruling C.
    ///
    /// The three tests above pin the *window* and the derived `ExecRow::ts`. This
    /// pins what `sort_key` does with them, which is the half with four consumers
    /// queued behind it: `list_for_universe`'s newest-first contract is what every
    /// "latest wins" map in the analytics cores relies on, and
    /// `latest_per_test`'s `MAX(sort_key)` picks the winner for a file from the
    /// same expression.
    ///
    /// One file, two runs, and the wrong column inverts them:
    ///
    /// * a **finished** run that finished an hour ago;
    /// * an **unfinished** run created thirty days ago, whose rows this test wrote
    ///   a moment ago — so its `created_at` is the freshest instant in the table
    ///   and its `run_created_at` the oldest.
    ///
    /// Under `run_created_at` the finished run is newer and wins both. Under the
    /// row's `created_at` the thirty-day-old run has the maximum timestamp, sorts
    /// first, and becomes the file's "latest" — which is the defect, and it moves
    /// again on the next result event.
    #[tokio::test]
    async fn a_long_running_run_does_not_outrank_a_freshly_finished_one() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        let finished = Uuid::from_u128(0xA0);
        let long_running = Uuid::from_u128(0xA1);

        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                finished,
                vec![NewTestResult {
                    run_finished_at: Some(now() - Duration::hours(1)),
                    ..result_row("tests/shared.py", "test_shared", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                long_running,
                vec![NewTestResult {
                    run_finished_at: None,
                    run_created_at: Some(now() - Duration::days(30)),
                    ..result_row("tests/shared.py", "test_shared", "FAILED")
                }],
                vec![],
            )
            .await
            .unwrap();

        let ordered = OrmResultsRepository
            .list_for_universe(&conn, &ctx, &any_row())
            .await
            .unwrap();
        assert_eq!(
            ordered.iter().map(|r| r.run_id).collect::<Vec<_>>(),
            vec![finished, long_running],
            "newest first by the run's own instant, not by when the rows were \
             last written: {ordered:?}"
        );

        let latest = OrmResultsRepository
            .latest_per_test(&conn, &ctx, &any_row())
            .await
            .unwrap();
        assert_eq!(
            latest.iter().map(|r| r.run_id).collect::<Vec<_>>(),
            vec![finished],
            "and the SQL reduction picks the same winner for the shared file: \
             {latest:?}"
        );
    }

    /// Half-open `[from, to)`, distinct, so consecutive reconcile windows sharing
    /// an endpoint neither skip a run nor replay one.
    ///
    /// The run at exactly `to` is the assertion that matters: if the upper bound
    /// were inclusive, the next window would ingest it a second time.
    #[tokio::test]
    async fn ingested_run_ids_are_distinct_and_the_window_is_half_open() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        let from = now();
        let to = from + Duration::hours(2);
        let inside = Uuid::from_u128(0x20);
        let at_upper_bound = Uuid::from_u128(0x21);
        let before = Uuid::from_u128(0x22);

        for (run, at) in [
            (inside, from + Duration::hours(1)),
            (at_upper_bound, to),
            (before, from - Duration::hours(1)),
        ] {
            OrmResultsRepository
                .upsert_run_results(
                    &conn,
                    &ctx,
                    tenant,
                    run,
                    // Two rows for the run, so a missing DISTINCT shows up.
                    vec![
                        NewTestResult {
                            run_finished_at: Some(at),
                            ..result_row("tests/a.py", "test_a", "PASSED")
                        },
                        NewTestResult {
                            run_finished_at: Some(at),
                            ..result_row("tests/b.py", "test_b", "PASSED")
                        },
                    ],
                    vec![],
                )
                .await
                .unwrap();
        }

        let ids = OrmResultsRepository
            .ingested_run_ids_between(&conn, &ctx, from, to)
            .await
            .unwrap();
        assert_eq!(
            ids,
            vec![inside],
            "one id for the one run in [from, to): {ids:?}"
        );
    }

    /// **`finished_only` must exclude rows with no finish instant on its own.**
    ///
    /// Added 2026-08-20 after break-verification: deleting the
    /// `run_finished_at IS NOT NULL` predicate left the whole suite green, because
    /// `the_universe_filter_applies_version_branch_and_plan` also sets `since`,
    /// and on the `finished_only` path `since` compares the bare column — so
    /// `NULL >= since` already excluded the unfinished row. Two predicates, one
    /// doing the work of both, is exactly the mutual masking that makes a dropped
    /// filter invisible. This one sets `finished_only` and nothing else.
    ///
    /// It is also the widening the filter's doc records: legacy's predicate is
    /// `r.phase IN ('Succeeded', 'Failed')`, and this gear has no phase column, so
    /// `run_finished_at IS NOT NULL` is the nearest expressible thing and admits a
    /// cancelled run that legacy would have dropped.
    #[tokio::test]
    async fn finished_only_excludes_a_run_with_no_finish_instant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        let finished = Uuid::from_u128(0x20);
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                finished,
                vec![result_row("tests/a.py", "test_a", "PASSED")],
                vec![],
            )
            .await
            .unwrap();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                Uuid::from_u128(0x21),
                vec![NewTestResult {
                    run_finished_at: None,
                    ..result_row("tests/b.py", "test_b", "RUNNING")
                }],
                vec![],
            )
            .await
            .unwrap();

        // `finished_only` alone -- no `since`, so nothing else can do its work.
        let rows = OrmResultsRepository
            .list_for_universe(
                &conn,
                &ctx,
                &UniverseFilter {
                    finished_only: true,
                    ..any_row()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.run_id).collect::<Vec<_>>(),
            vec![finished],
            "the in-progress run's row must be excluded: {rows:?}"
        );

        // And with the flag off it comes back, so the assertion above is about
        // the predicate rather than about the fixture.
        assert_eq!(
            OrmResultsRepository
                .list_for_universe(&conn, &ctx, &any_row())
                .await
                .unwrap()
                .len(),
            2,
            "without finished_only, both rows are in scope"
        );
    }

    /// **The database-side reduction must pick the same winner the ordered read
    /// would.**
    ///
    /// `latest_per_test` reduces in SQL — `(test_file, latest) IN (SELECT
    /// test_file, MAX(latest) … GROUP BY test_file)` — where it used to fold
    /// [`list_for_universe`]'s output in memory. Those are two different
    /// statements over the same data, and nothing in the types says they agree.
    /// This asserts they do, against a fixture where the newest row for each file
    /// is *not* the one a naive `GROUP BY` would surface: each file has three
    /// runs, its newest is in the middle run, and the statuses differ per run so a
    /// wrong winner is visible rather than merely a different row id.
    #[tokio::test]
    async fn the_sql_reduction_picks_the_same_latest_row_the_ordered_read_does() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        // Deliberately not in timestamp order: the newest run is written second.
        for (run, offset_days, status) in [
            (0x20_u128, 0_i64, "FAILED"),
            (0x21, 5, "PASSED"),
            (0x22, 2, "ERROR"),
        ] {
            OrmResultsRepository
                .upsert_run_results(
                    &conn,
                    &ctx,
                    tenant,
                    Uuid::from_u128(run),
                    vec![
                        NewTestResult {
                            run_finished_at: Some(now() + Duration::days(offset_days)),
                            ..result_row("tests/a.py", "test_a", status)
                        },
                        NewTestResult {
                            run_finished_at: Some(now() + Duration::days(offset_days)),
                            ..result_row("tests/b.py", "test_b", status)
                        },
                    ],
                    vec![],
                )
                .await
                .unwrap();
        }

        let filter = UniverseFilter {
            finished_only: true,
            ..any_row()
        };

        // Ground truth: the same first-wins fold, over the full ordered read.
        let mut seen = std::collections::HashSet::new();
        let mut expected = OrmResultsRepository
            .list_for_universe(&conn, &ctx, &filter)
            .await
            .unwrap()
            .into_iter()
            .filter(|row| seen.insert(row.test_file.clone()))
            .collect::<Vec<_>>();
        expected.sort_by(|a, b| a.test_file.cmp(&b.test_file));

        let mut actual = OrmResultsRepository
            .latest_per_test(&conn, &ctx, &filter)
            .await
            .unwrap();
        actual.sort_by(|a, b| a.test_file.cmp(&b.test_file));

        assert_eq!(
            actual, expected,
            "the SQL reduction and the in-memory fold must agree on the winner"
        );
        // Non-vacuity: the fixture must actually have a newest-in-the-middle run,
        // or the assertion above would pass for a reduction that just took the
        // last row it saw.
        assert_eq!(actual.len(), 2, "one row per file: {actual:?}");
        assert!(
            actual.iter().all(|r| r.status == "PASSED"),
            "the winner is the middle-written run, which is the newest: {actual:?}"
        );
    }

    /// **The reduction's subquery must be scoped, or another tenant's newer row
    /// suppresses yours.**
    ///
    /// `latest_per_test` filters on `(test_file, latest) IN (SELECT test_file,
    /// MAX(latest) … GROUP BY test_file)`. If that subquery is not bound to the
    /// caller's scope, the `MAX` it computes is the maximum across *every*
    /// tenant — so a tenant whose newest row for a file is older than some other
    /// tenant's gets **no row at all** for that file. Not a leak of another
    /// tenant's data; a silent hole in your own, which is harder to notice.
    ///
    /// The implementation is safe by construction because the subquery is a clone
    /// of the already-scoped `Select`. This test is what makes that structural
    /// property observable: replacing `query.clone()` with a bare
    /// `ResultEntity::find()` compiles, keeps every other test green, and turns
    /// this one red.
    #[tokio::test]
    async fn the_latest_per_test_subquery_is_scoped_to_the_caller() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);

        // My row is older; theirs is newer, on the same file.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(mine),
                mine,
                Uuid::from_u128(0x20),
                vec![NewTestResult {
                    run_finished_at: Some(now()),
                    ..result_row("tests/a.py", "test_a", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(theirs),
                theirs,
                Uuid::from_u128(0x21),
                vec![NewTestResult {
                    run_finished_at: Some(now() + Duration::days(7)),
                    ..result_row("tests/a.py", "test_a", "FAILED")
                }],
                vec![],
            )
            .await
            .unwrap();

        let filter = UniverseFilter {
            finished_only: true,
            ..any_row()
        };
        let latest = OrmResultsRepository
            .latest_per_test(&conn, &scope(mine), &filter)
            .await
            .unwrap();

        assert_eq!(
            latest.len(),
            1,
            "my own latest row for the file must come back; an unscoped MAX \
             would compute another tenant's newer instant and match nothing: \
             {latest:?}"
        );
        assert_eq!(latest[0].ts, now());
        assert_eq!(latest[0].status, "PASSED");
    }

    /// The same property for the universe filter: a row the filter excludes must
    /// not set the `MAX` that the included rows are compared against.
    #[tokio::test]
    async fn the_latest_per_test_subquery_respects_the_universe_filter() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                Uuid::from_u128(0x20),
                vec![NewTestResult {
                    run_finished_at: Some(now()),
                    ..result_row("tests/a.py", "test_a", "PASSED")
                }],
                vec![],
            )
            .await
            .unwrap();
        // Newer, but on a product version the filter excludes.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                Uuid::from_u128(0x21),
                vec![NewTestResult {
                    product_version: Some("4.9.0".to_owned()),
                    run_finished_at: Some(now() + Duration::days(7)),
                    ..result_row("tests/a.py", "test_a", "FAILED")
                }],
                vec![],
            )
            .await
            .unwrap();

        let latest = OrmResultsRepository
            .latest_per_test(
                &conn,
                &ctx,
                &UniverseFilter {
                    product_version: Some("5.0.1".to_owned()),
                    finished_only: true,
                    ..any_row()
                },
            )
            .await
            .unwrap();

        assert_eq!(
            latest.len(),
            1,
            "the excluded row must not set the MAX: {latest:?}"
        );
        assert_eq!(latest[0].status, "PASSED");
    }

    /// **The ordinal is the batch position, written 0..n in the order given.**
    ///
    /// Read back through the entity rather than through `ExecRow` or
    /// `TestResultRecord`, because the column is deliberately on neither: it is a
    /// persistence ordering detail. Without this, `ordinal_of` could return a
    /// constant and only the tie tests would notice — and only indirectly.
    #[tokio::test]
    async fn the_ingest_ordinal_is_the_rows_position_in_the_batch() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let run_id = Uuid::from_u128(0x20);

        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                run_id,
                vec![
                    result_row("tests/a.py", "test_a", "PASSED"),
                    result_row("tests/b.py", "test_b", "FAILED"),
                    result_row("tests/c.py", "test_c", "SKIPPED"),
                ],
                vec![],
            )
            .await
            .unwrap();

        let mut rows = ResultEntity::find()
            .secure()
            .scope_with(&scope(tenant))
            .filter(Condition::all().add(Expr::col(ResultColumn::RunId).eq(run_id)))
            .all(&conn)
            .await
            .unwrap();
        rows.sort_by_key(|r| r.ingest_ordinal);
        assert_eq!(
            rows.iter()
                .map(|r| (r.ingest_ordinal, r.test_name.as_str()))
                .collect::<Vec<_>>(),
            vec![(0, "test_a"), (1, "test_b"), (2, "test_c")],
            "ordinals must be the batch positions, in the order the caller gave"
        );
    }

    /// **A tie ACROSS two runs is broken by ingest order, not by batch
    /// position.**
    ///
    /// Legacy's `t.id DESC` is a **global** `SERIAL`, which carries two distinct
    /// facts: within a run it is parse order, and across two runs sharing a
    /// timestamp it is *ingest* order. `ingest_ordinal` reproduces only the first.
    /// `created_at DESC` — one instant stamped per batch, monotonic with ingest
    /// order — is what reproduces the second, and the two together emulate a
    /// global serial exactly: a later batch sorts ahead of an earlier one, and
    /// within a batch a later row sorts ahead of an earlier one.
    ///
    /// **This test exists because `45dafe9b` dropped `created_at` from the
    /// ordering and regressed a case the commit before it got right.** The
    /// fixture is the distinguishing one: the *older-ingested* run holds
    /// `tests/a.py` at a **higher** ordinal than the newer-ingested run does, so
    /// `sort_key DESC, ingest_ordinal DESC` picks the older row and
    /// `sort_key DESC, created_at DESC, ingest_ordinal DESC` picks the newer one.
    /// Only the second matches legacy.
    ///
    /// Reachable for the same reasons the column itself is: `run_finished_at` is
    /// denormalized from the run, so two runs genuinely share it after a backfill
    /// or a re-ingest, and every row whose producer reported no file collides on
    /// `''`.
    #[tokio::test]
    async fn a_tie_across_two_runs_is_won_by_the_later_ingested_one() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);
        // One shared finish instant, so `sort_key` cannot separate the two runs.
        let shared = now();

        // Ingested FIRST, and `tests/a.py` is its third row -> ordinal 2.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                Uuid::from_u128(0x20),
                vec![
                    NewTestResult {
                        run_finished_at: Some(shared),
                        ..result_row("tests/x.py", "test_x", "PASSED")
                    },
                    NewTestResult {
                        run_finished_at: Some(shared),
                        ..result_row("tests/y.py", "test_y", "PASSED")
                    },
                    NewTestResult {
                        run_finished_at: Some(shared),
                        ..result_row("tests/a.py", "test_a_older_run", "PASSED")
                    },
                ],
                vec![],
            )
            .await
            .unwrap();

        // Ingested SECOND, and `tests/a.py` is its only row -> ordinal 0, i.e.
        // *lower* than the older run's. Batch position must not decide this.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                Uuid::from_u128(0x21),
                vec![NewTestResult {
                    run_finished_at: Some(shared),
                    ..result_row("tests/a.py", "test_a_newer_run", "FAILED")
                }],
                vec![],
            )
            .await
            .unwrap();

        let filter = UniverseFilter {
            finished_only: true,
            ..any_row()
        };
        let latest = OrmResultsRepository
            .latest_per_test(&conn, &ctx, &filter)
            .await
            .unwrap();
        let for_a = latest
            .iter()
            .find(|r| r.test_file == "tests/a.py")
            .expect("tests/a.py must be in the result");
        assert_eq!(
            for_a.test_name, "test_a_newer_run",
            "legacy's global SERIAL puts the LAST-INGESTED row first; a tiebreak \
             of ingest_ordinal alone picks the older run's row because its batch \
             position happens to be higher"
        );

        // The full ordered read must agree, since the fold consumes its order.
        let ordered = OrmResultsRepository
            .list_for_universe(&conn, &ctx, &filter)
            .await
            .unwrap();
        let names: Vec<&str> = ordered
            .iter()
            .filter(|r| r.test_file == "tests/a.py")
            .map(|r| r.test_name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["test_a_newer_run", "test_a_older_run"],
            "the later-ingested run's row must sort first: {ordered:?}"
        );
    }

    /// **A newer run whose row has a *lower* ordinal must still win.**
    ///
    /// This is the fixture that rules out the tempting-but-wrong exact reduction:
    /// `(test_file, MAX(sort_key), MAX(ingest_ordinal)) GROUP BY test_file` takes
    /// the maximum ordinal over *every* row of the file, not over the rows at the
    /// maximum instant — so with an older run holding a higher ordinal than the
    /// newer run's row, the tuple matches nothing and the file disappears from the
    /// result entirely. Attempted and measured, not reasoned about: that shortcut
    /// makes this test fail with an empty result.
    ///
    /// The shipped reduction narrows on the instant only and lets the fold break
    /// the ordinal tie, which is why it is correct here.
    #[tokio::test]
    async fn a_newer_run_wins_even_when_its_row_has_a_lower_ordinal() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);

        // Older run: the file is the *third* row, so ordinal 2.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                Uuid::from_u128(0x20),
                vec![
                    NewTestResult {
                        run_finished_at: Some(now()),
                        ..result_row("tests/x.py", "test_x", "PASSED")
                    },
                    NewTestResult {
                        run_finished_at: Some(now()),
                        ..result_row("tests/y.py", "test_y", "PASSED")
                    },
                    NewTestResult {
                        run_finished_at: Some(now()),
                        ..result_row("tests/a.py", "test_a_old", "PASSED")
                    },
                ],
                vec![],
            )
            .await
            .unwrap();
        // Newer run: the file is the *only* row, so ordinal 0 — lower than above.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                Uuid::from_u128(0x21),
                vec![NewTestResult {
                    run_finished_at: Some(now() + Duration::days(1)),
                    ..result_row("tests/a.py", "test_a_new", "FAILED")
                }],
                vec![],
            )
            .await
            .unwrap();

        let latest = OrmResultsRepository
            .latest_per_test(
                &conn,
                &ctx,
                &UniverseFilter {
                    finished_only: true,
                    ..any_row()
                },
            )
            .await
            .unwrap();

        let for_a = latest
            .iter()
            .find(|r| r.test_file == "tests/a.py")
            .expect("tests/a.py must still be in the result at all");
        assert_eq!(
            for_a.test_name, "test_a_new",
            "the newer run wins on the instant; the ordinal only breaks ties \
             *within* an instant"
        );
    }

    /// **The `MAX` alone does not settle a tie, and `ingest_ordinal` is what
    /// does.**
    ///
    /// Two rows of one run share `run_finished_at` *and* `created_at` — ingest
    /// stamps one `now` for the whole batch — so `(test_file, MAX(latest))` admits
    /// both and the reduction has to break the tie itself. Without the fold this
    /// returns two rows for one file, which every latest-wins caller would then
    /// double-count.
    ///
    /// **The winner is asserted, not just its uniqueness.** Legacy's is the last
    /// row the log parser produced (`analytics.rs:971` + the stable re-sort at
    /// `:1042` + parse-order inserts at `argo.rs:2598-2615`), so the winner here
    /// must be the last element of the batch — the highest `ingest_ordinal`. That
    /// assertion is the whole point of the column, and until 2026-08-20 this test
    /// could only claim stability.
    #[tokio::test]
    async fn the_last_row_of_a_batch_wins_a_tie_on_one_test_file() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);
        let run_id = Uuid::from_u128(0x20);

        // Same file, three different file-level display names — legacy's dedupe
        // tuple includes `test_name`, so this is a shape ingest really produces.
        // The statuses differ so the winner is identifiable by value, not by id.
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                run_id,
                vec![
                    result_row("tests/a.py", "test_a_first", "PASSED"),
                    result_row("tests/a.py", "test_a_second", "SKIPPED"),
                    result_row("tests/a.py", "test_a_last", "FAILED"),
                ],
                vec![],
            )
            .await
            .unwrap();

        let filter = UniverseFilter {
            finished_only: true,
            ..any_row()
        };
        let latest = OrmResultsRepository
            .latest_per_test(&conn, &ctx, &filter)
            .await
            .unwrap();
        assert_eq!(
            latest.len(),
            1,
            "the tie must reduce to one row, not three: {latest:?}"
        );
        assert_eq!(
            latest[0].test_name, "test_a_last",
            "legacy's winner is the LAST row the parser produced, which is the \
             highest ingest_ordinal, not the first and not an arbitrary UUID"
        );
        assert_eq!(latest[0].status, "FAILED");

        // The full ordered read must agree, since the fold consumes its order.
        let ordered = OrmResultsRepository
            .list_for_universe(&conn, &ctx, &filter)
            .await
            .unwrap();
        assert_eq!(
            ordered
                .iter()
                .map(|r| r.test_name.as_str())
                .collect::<Vec<_>>(),
            vec!["test_a_last", "test_a_second", "test_a_first"],
            "descending on ingest_ordinal within the run: {ordered:?}"
        );
    }

    /// The tie reduces to **one** row and repeated reads agree — the properties
    /// that hold independently of which row the tiebreak picks.
    ///
    /// Kept alongside
    /// [`the_last_row_of_a_batch_wins_a_tie_on_one_test_file`] rather than folded
    /// into it: totality of the ordering and *identity* of the winner are
    /// different claims, and a future change to the tiebreak should have to face
    /// them separately.
    #[tokio::test]
    async fn two_rows_of_one_run_sharing_a_test_file_reduce_to_one_stable_winner() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let ctx = scope(tenant);
        let run_id = Uuid::from_u128(0x20);

        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &ctx,
                tenant,
                run_id,
                vec![
                    result_row("tests/a.py", "test_a_first", "PASSED"),
                    result_row("tests/a.py", "test_a_second", "FAILED"),
                ],
                vec![],
            )
            .await
            .unwrap();

        let filter = UniverseFilter {
            finished_only: true,
            ..any_row()
        };
        let first = OrmResultsRepository
            .latest_per_test(&conn, &ctx, &filter)
            .await
            .unwrap();
        assert_eq!(
            first.len(),
            1,
            "the tie must reduce to one row, not two: {first:?}"
        );

        let second = OrmResultsRepository
            .latest_per_test(&conn, &ctx, &filter)
            .await
            .unwrap();
        assert_eq!(
            first, second,
            "the order is total, so a repeated read must pick the same winner"
        );

        assert_eq!(
            OrmResultsRepository
                .list_for_universe(&conn, &ctx, &filter)
                .await
                .unwrap(),
            OrmResultsRepository
                .list_for_universe(&conn, &ctx, &filter)
                .await
                .unwrap(),
            "repeated reads of the same rows must not reorder them"
        );
    }

    /// **`tenant_id` is a parameter, and the explicit guard is what stops it
    /// being a free one.**
    ///
    /// The bulk inserts go through `scope_unchecked`, because `scope_with_model`
    /// takes a single `ActiveModel` and there is no batch form — so unlike every
    /// `secure_insert` path in this gear, nothing in the library validates the
    /// tenant here. Without `validate_tenant_in_scope` a caller could write rows
    /// stamped with another tenant's id: the *reads* would then hide them from
    /// everyone, which is a silent corruption rather than a visible leak.
    ///
    /// The same guard is on every other `scope_unchecked` write in
    /// `infra::storage` — `collect_sea_repo::upsert_count`,
    /// `notify_sea_repo::{claim_notification, save_config}`,
    /// `jira_sea_repo::upsert_bug` and `watermark_sea_repo::advance`. This is the
    /// one that is measured, because it is the ingest path Task 13 drives.
    #[tokio::test]
    async fn writing_results_for_a_tenant_outside_the_scope_is_refused() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);
        let run_id = Uuid::from_u128(0x20);

        let outcome = OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(mine),
                theirs,
                run_id,
                vec![result_row("tests/a.py", "test_a", "PASSED")],
                vec![],
            )
            .await;

        assert!(
            outcome.is_err(),
            "writing another tenant's rows must be refused: {outcome:?}"
        );
        assert!(
            OrmResultsRepository
                .list_by_run(&conn, &scope(theirs), run_id)
                .await
                .unwrap()
                .is_empty(),
            "and nothing must have landed"
        );
    }

    /// Ingesting the same run's results twice must leave one row per
    /// `(run_id, test_file, test_name)`.
    ///
    /// Legacy achieves this by deleting the run's rows and re-inserting
    /// (`manager/src/routes/runs.rs:1153-1185`); this reproduces the semantics,
    /// and it is what makes a re-run of the sweep or a re-run of the rebuild
    /// over the same run a no-op rather than a double count.
    #[tokio::test]
    async fn upserting_a_runs_results_twice_leaves_one_row_per_test() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let run_id = Uuid::new_v4();

        let batch = vec![result_row("tests/a.py", "test_a", "PASSED")];
        OrmResultsRepository
            .upsert_run_results(&conn, &scope(tenant), tenant, run_id, batch.clone(), vec![])
            .await
            .unwrap();
        OrmResultsRepository
            .upsert_run_results(&conn, &scope(tenant), tenant, run_id, batch, vec![])
            .await
            .unwrap();

        let rows = OrmResultsRepository
            .list_by_run(&conn, &scope(tenant), run_id)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "redelivery must not duplicate: {rows:?}");
    }
    // -----------------------------------------------------------------------
    // The two `OData` collections (Task 17)
    // -----------------------------------------------------------------------

    /// One run's worth of rows, written as a single batch under `tenant`.
    ///
    /// A **batch** on purpose: `upsert_run_results` stamps one `created_at` for
    /// the whole call, so these rows are indistinguishable on every timestamp
    /// they carry. That is precisely the input the `id` tiebreaker exists for —
    /// see [`crate::domain::repos::ResultsRepository::list_page`].
    async fn seed_batch(db: &toolkit_db::Db, tenant: Uuid, run_id: Uuid, rows: usize) {
        let conn = db.conn().unwrap();
        let files: Vec<NewTestResult> = (0..rows)
            .map(|n| result_row(&format!("tests/t{n}.py"), &format!("test_{n}"), "PASSED"))
            .collect();
        let cases: Vec<NewTestCaseResult> = (0..rows)
            .map(|n| case_row(&format!("tests/t{n}.py"), &format!("test_{n}"), "XFAIL"))
            .collect();
        OrmResultsRepository
            .upsert_run_results(&conn, &scope(tenant), tenant, run_id, files, cases)
            .await
            .unwrap();
    }

    /// A `$filter` string, parsed the way the transport parses it.
    ///
    /// Through `toolkit_odata::parse_filter_string`, which is what
    /// `toolkit::api::odata`'s extractor calls — so a test cannot accidentally
    /// hand the repository an AST no HTTP request could produce.
    fn filtered(raw: &str) -> ODataQuery {
        let parsed = toolkit_odata::parse_filter_string(raw).expect("the fixture filter parses");
        ODataQuery::new().with_filter(parsed.into_expr())
    }

    /// An `$orderby` string, parsed the way the transport parses it.
    ///
    /// `toolkit::api::odata::parse_orderby`, which is the extractor's own parser
    /// (`libs/toolkit/src/api/odata.rs:100`) — it lives in `toolkit` rather than
    /// `toolkit-odata`, unlike the filter parser.
    fn ordered(raw: &str) -> ODataQuery {
        ODataQuery::new()
            .with_order(toolkit::api::odata::parse_orderby(raw).expect("the fixture parses"))
    }

    /// **Paging walks every row exactly once**, across three page boundaries.
    ///
    /// Seven rows written as one batch, in pages of two. The assertion is on the
    /// *set* of ids and on the count, not on the order: a v4 `Uuid` order is
    /// arbitrary by construction, and pinning it would be pinning the fixture's
    /// random numbers.
    ///
    /// # What this observes, and what actually holds the property
    ///
    /// **Corrected 2026-08-21.** This doc called itself "the test the `id`
    /// tiebreaker exists for" and said a non-unique final order key would make the
    /// next page skip the rest of the batch. The mechanism is real —
    /// `build_cursor_predicate`
    /// (`libs/toolkit-db/src/odata/sea_orm_filter.rs:844-903`) is a strict
    /// OR-of-AND-prefixes with no equal-tuple branch, so rows sharing the boundary
    /// value are lost — but **this test cannot observe it**, and saying otherwise
    /// overstated it.
    ///
    /// Two reasons. `seed_batch` gives every row a distinct `test_file` and
    /// `test_name`, so every *reachable* alternative tiebreaker is unique in this
    /// fixture. And the two columns that genuinely are non-unique here —
    /// `created_at` (one instant per batch) and `run_finished_at` (one per run) —
    /// cannot be order keys at all: the first is absent from `TestResultsField`
    /// and the second is `is_orderable == false`. Mutating the tiebreaker to
    /// `"created_at"` does turn this red, but with `InvalidOrderByField`, not with
    /// a missing row.
    ///
    /// So **the allow-list is what holds the property**, and the `id` tiebreaker is
    /// what makes the allow-list sufficient; `ResultsRepository::list_page` carries
    /// that argument. What this test observes is narrower and still worth having:
    /// that the cursor round-trips through `CursorV1::decode`, that three
    /// boundaries lose and duplicate nothing, and that paging terminates.
    #[tokio::test]
    async fn paging_visits_every_row_of_one_batch_exactly_once() {
        let db = inmem_db().await;
        let tenant = Uuid::from_u128(TENANT);
        let run_id = Uuid::from_u128(0x70);
        seed_batch(&db, tenant, run_id, 7).await;

        let conn = db.conn().unwrap();
        let mut seen: Vec<Uuid> = Vec::new();
        let mut query = ODataQuery::new().with_limit(2);
        let mut pages = 0;
        loop {
            let page = OrmResultsRepository
                .list_page(&conn, &scope(tenant), &query)
                .await
                .unwrap();
            pages += 1;
            assert!(pages <= 10, "paging did not terminate");
            seen.extend(page.items.iter().map(|r| r.id));
            let Some(cursor) = page.page_info.next_cursor.clone() else {
                break;
            };
            query = ODataQuery::new()
                .with_limit(2)
                .with_cursor(CursorV1::decode(&cursor).expect("the pager's own cursor decodes"));
        }

        assert_eq!(seen.len(), 7, "a row was skipped or repeated: {seen:?}");
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 7, "a row was returned twice: {seen:?}");
        assert_eq!(pages, 4, "7 rows in pages of 2 is four pages");
    }

    /// The clamp: no `$top` means [`PAGE_LIMITS`]`.default`, and an absurd one
    /// means `.max`.
    ///
    /// Asserted through `page_info.limit`, which is the *effective* size
    /// `clamp_limit` produced, rather than by counting rows — three rows would
    /// satisfy any limit at all and the assertion would say nothing.
    #[tokio::test]
    async fn the_page_size_is_defaulted_and_clamped() {
        let db = inmem_db().await;
        let tenant = Uuid::from_u128(TENANT);
        seed_batch(&db, tenant, Uuid::from_u128(0x71), 3).await;
        let conn = db.conn().unwrap();

        let defaulted = OrmResultsRepository
            .list_page(&conn, &scope(tenant), &ODataQuery::new())
            .await
            .unwrap();
        assert_eq!(defaulted.page_info.limit, PAGE_LIMITS.default);

        let clamped = OrmResultsRepository
            .list_page(&conn, &scope(tenant), &ODataQuery::new().with_limit(9_999))
            .await
            .unwrap();
        assert_eq!(clamped.page_info.limit, PAGE_LIMITS.max);

        // The case collection shares the constant, which is the whole point of
        // it being one constant.
        let cases = OrmResultsRepository
            .list_case_page(&conn, &scope(tenant), &ODataQuery::new().with_limit(9_999))
            .await
            .unwrap();
        assert_eq!(cases.page_info.limit, PAGE_LIMITS.max);
    }

    /// A page is scoped, and the `$filter` composes with the scope rather than
    /// replacing it.
    ///
    /// The second half is what a mock could not show: the filter names the *other*
    /// tenant's run id, and the answer is still empty. A repository that had
    /// applied the filter to an unscoped select would return that run's rows.
    #[tokio::test]
    async fn a_page_cannot_be_widened_past_the_callers_tenant() {
        let db = inmem_db().await;
        let mine = Uuid::from_u128(TENANT);
        let theirs = Uuid::from_u128(0xB);
        let my_run = Uuid::from_u128(0x72);
        let their_run = Uuid::from_u128(0x73);
        seed_batch(&db, mine, my_run, 2).await;
        seed_batch(&db, theirs, their_run, 2).await;
        let conn = db.conn().unwrap();

        let mine_page = OrmResultsRepository
            .list_page(&conn, &scope(mine), &ODataQuery::new())
            .await
            .unwrap();
        assert_eq!(mine_page.items.len(), 2);
        assert!(mine_page.items.iter().all(|r| r.run_id == my_run));

        let probe = OrmResultsRepository
            .list_page(
                &conn,
                &scope(mine),
                &filtered(&format!("run_id eq {their_run}")),
            )
            .await
            .unwrap();
        assert!(
            probe.items.is_empty(),
            "a $filter naming another tenant's run returned rows",
        );

        let case_probe = OrmResultsRepository
            .list_case_page(
                &conn,
                &scope(mine),
                &filtered(&format!("run_id eq {their_run}")),
            )
            .await
            .unwrap();
        assert!(case_probe.items.is_empty());
    }

    /// A field outside the allow-list is the **caller's** mistake, named by the
    /// parameter they typed.
    ///
    /// `status` for the file-level collection and `reason` for the case-level one
    /// are both real columns of their own tables — so this is the case that
    /// matters: not a typo, but a column that exists and is not indexed. A
    /// repository that answered `Database` here would report a client error as a
    /// server fault.
    #[tokio::test]
    async fn a_filter_outside_the_allow_list_is_a_validation_error() {
        let db = inmem_db().await;
        let tenant = Uuid::from_u128(TENANT);
        let conn = db.conn().unwrap();

        let err = OrmResultsRepository
            .list_page(&conn, &scope(tenant), &filtered("status eq 'FAILED'"))
            .await
            .expect_err("status is not filterable on qa_test_results");
        match err {
            DomainError::Validation { field, message } => {
                assert_eq!(field, "$filter");
                assert!(message.contains("status"), "{message}");
            }
            other => panic!("expected the caller's 400, got {other:?}"),
        }

        let case_err = OrmResultsRepository
            .list_case_page(&conn, &scope(tenant), &filtered("reason eq 'flaky'"))
            .await
            .expect_err("reason is not filterable on qa_test_case_results");
        assert!(matches!(
            case_err,
            DomainError::Validation { ref field, .. } if field == "$filter"
        ));
    }

    /// `$orderby=run_finished_at` is refused, naming `$orderby`.
    ///
    /// The field is filterable — the first half of this test proves the *same*
    /// column is accepted in a `$filter` — so what is being pinned is the
    /// filter/order split, not a missing field. `is_orderable`'s doc has the
    /// measurement behind the split, and `odata::tests` has the measurement
    /// itself.
    #[tokio::test]
    async fn the_nullable_instant_filters_but_does_not_sort() {
        let db = inmem_db().await;
        let tenant = Uuid::from_u128(TENANT);
        seed_batch(&db, tenant, Uuid::from_u128(0x74), 2).await;
        let conn = db.conn().unwrap();

        let windowed = OrmResultsRepository
            .list_page(
                &conn,
                &scope(tenant),
                &filtered("run_finished_at ge 2026-08-17T00:00:00Z"),
            )
            .await
            .expect("run_finished_at is filterable");
        assert_eq!(windowed.items.len(), 2);

        let err = OrmResultsRepository
            .list_page(&conn, &scope(tenant), &ordered("run_finished_at desc"))
            .await
            .expect_err("run_finished_at must not be an order key");
        assert!(matches!(
            err,
            DomainError::Validation { ref field, .. } if field == "$orderby"
        ));
    }

    /// `$orderby=created_at desc` — the query a caller reaches for when the
    /// default order turns out to be arbitrary — is **refused**, on both
    /// collections.
    ///
    /// A test rather than a note because an earlier draft of
    /// `ResultsRepository::list_page`'s own doc offered this query as the remedy,
    /// and it does not work: `created_at` is covered by no index
    /// (`m20260818_000001_initial.rs:468` and `:503`), so the allow-list excludes
    /// it. The doc now says recency is a `run_finished_at` filter instead, and
    /// this is what stops the wrong remedy being written back in.
    #[tokio::test]
    async fn the_chronological_order_a_caller_would_reach_for_is_refused() {
        let db = inmem_db().await;
        let tenant = Uuid::from_u128(TENANT);
        let conn = db.conn().unwrap();

        for query in [ordered("created_at desc"), ordered("created_at asc")] {
            let err = OrmResultsRepository
                .list_page(&conn, &scope(tenant), &query)
                .await
                .expect_err("created_at is covered by no index and is not orderable");
            assert!(matches!(
                err,
                DomainError::Validation { ref field, .. } if field == "$orderby"
            ));
        }

        let case_err = OrmResultsRepository
            .list_case_page(&conn, &scope(tenant), &ordered("created_at desc"))
            .await
            .expect_err("the case table has no created_at index either");
        assert!(matches!(
            case_err,
            DomainError::Validation { ref field, .. } if field == "$orderby"
        ));
    }

    /// The case collection's `status` filter, which is the one thing it can do
    /// that the file-level collection cannot.
    #[tokio::test]
    async fn the_case_collection_can_be_filtered_by_status() {
        let db = inmem_db().await;
        let tenant = Uuid::from_u128(TENANT);
        let run_id = Uuid::from_u128(0x75);
        seed_batch(&db, tenant, run_id, 3).await;
        let conn = db.conn().unwrap();

        let hits = OrmResultsRepository
            .list_case_page(&conn, &scope(tenant), &filtered("status eq 'XFAIL'"))
            .await
            .unwrap();
        assert_eq!(hits.items.len(), 3);

        let misses = OrmResultsRepository
            .list_case_page(&conn, &scope(tenant), &filtered("status eq 'PASSED'"))
            .await
            .unwrap();
        assert!(misses.items.is_empty());
    }

    /// **A lowercase status is stored verbatim, and an uppercase filter misses it
    /// silently.**
    ///
    /// The test behind the correction to
    /// `infra::storage::odata::TestCaseResultsField::Status`, whose doc claimed the
    /// stored value was "uppercased by ingest". Nothing uppercases — the projection
    /// clones the producer's string and qa-runs' `normalize_status` trims without
    /// re-casing — so the uppercase vocabulary is a producer *convention* and a
    /// filter is an exact match against whatever actually arrived.
    ///
    /// Both halves are asserted, because the dangerous one is the silence: the
    /// lowercase row is readable and `status eq 'XFAIL'` returns **`Ok` with an
    /// empty page**, not an error. A client author who trusted the old sentence
    /// would see nothing and have nothing to look at.
    ///
    /// Dialect-safe: `=` on text is case-sensitive under both `SQLite`'s default
    /// BINARY collation and Postgres. Deliberately not extended to
    /// `contains`/`startswith`, which compile to `LIKE` and whose ASCII
    /// case-folding *does* differ between the two — that is a separate claim and
    /// this task measured only `eq`.
    #[tokio::test]
    async fn a_lowercase_status_is_stored_verbatim_and_an_uppercase_filter_misses_it() {
        let db = inmem_db().await;
        let tenant = Uuid::from_u128(TENANT);
        let run_id = Uuid::from_u128(0x77);
        let conn = db.conn().unwrap();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                run_id,
                vec![result_row("tests/a.py", "test_a", "passed")],
                vec![case_row("tests/a.py", "test_a", "xfail")],
            )
            .await
            .unwrap();

        // Stored exactly as the producer said it, on both tables.
        let files = OrmResultsRepository
            .list_page(&conn, &scope(tenant), &ODataQuery::new())
            .await
            .unwrap();
        assert_eq!(
            files.items.first().map(|r| r.status.as_str()),
            Some("passed"),
            "ingest must not re-case a producer status",
        );
        let cases = OrmResultsRepository
            .list_case_page(&conn, &scope(tenant), &ODataQuery::new())
            .await
            .unwrap();
        assert_eq!(
            cases.items.first().map(|r| r.status.as_str()),
            Some("xfail")
        );

        // The lowercase filter finds it...
        let hit = OrmResultsRepository
            .list_case_page(&conn, &scope(tenant), &filtered("status eq 'xfail'"))
            .await
            .unwrap();
        assert_eq!(hit.items.len(), 1);

        // ...and the uppercase one does not, with no error to notice.
        let miss = OrmResultsRepository
            .list_case_page(&conn, &scope(tenant), &filtered("status eq 'XFAIL'"))
            .await
            .expect("a case-mismatched filter is a successful empty page, not an error");
        assert!(
            miss.items.is_empty(),
            "if this finds the row, `eq` is case-insensitive on this backend and the \
             endpoint's case-sensitivity caveat is wrong",
        );
    }

    /// **The read-side enumeration guard for `test_case_result_to_sdk`**, named by
    /// that function's own doc.
    ///
    /// Ten fields, of which four are `String` and three are `Option<String>` — so
    /// `name`/`nodeid` transposed, or `reason`/`ticket` transposed, is correct
    /// Rust returning the wrong record. Every field is given a value that
    /// identifies it, and `nodeid` is deliberately derived from the other two
    /// (`case_row` builds `file::name[1]`) so a swap cannot coincide.
    #[tokio::test]
    async fn the_case_collection_maps_every_column_to_its_own_field() {
        let db = inmem_db().await;
        let tenant = Uuid::from_u128(TENANT);
        let run_id = Uuid::from_u128(0x76);
        let conn = db.conn().unwrap();

        let before = OffsetDateTime::now_utc();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                run_id,
                vec![result_row("tests/a.py", "test_a", "PASSED")],
                vec![case_row("tests/a.py", "test_a", "XFAIL")],
            )
            .await
            .unwrap();
        let after = OffsetDateTime::now_utc();

        let page = OrmResultsRepository
            .list_case_page(&conn, &scope(tenant), &ODataQuery::new())
            .await
            .unwrap();
        let row = page.items.first().expect("the case row is readable");

        assert_eq!(row.run_id, run_id);
        assert_eq!(row.test_file, "tests/a.py");
        assert_eq!(row.nodeid, "tests/a.py::test_a[1]");
        assert_eq!(
            row.name, "test_a",
            "name must not carry nodeid or test_file"
        );
        assert_eq!(row.status, "XFAIL");
        assert_eq!(row.duration.as_deref(), Some("0.31s"));
        assert_eq!(
            row.reason.as_deref(),
            Some("known upstream defect"),
            "reason must not carry ticket",
        );
        assert_eq!(row.ticket.as_deref(), Some("VHP-9"));
        // `created_at` is the **repository's** clock — `upsert_run_results` stamps
        // `OffsetDateTime::now_utc()` — not the fixture instant `now()` that the
        // *caller-supplied* columns carry. So the assertion is that it is a real
        // ingest instant rather than a default or a copy of a fixture value;
        // pinning an exact time would mean freezing the clock, which this tier
        // does not do.
        //
        // **Bracketed by two readings of the same clock**, taken either side of the
        // write, rather than compared against the fixture instant.
        //
        // `assert!(row.created_at > now())` was the first version and it is two bad
        // tests in one: it fails on any machine whose clock predates the
        // 2026-08-18 fixture date, and once past that date it passes trivially and
        // for ever. The bracket has no such dependency — it holds on any clock,
        // including a wrong one — and it is *stronger*: it says the column carries
        // the instant of this write specifically, not merely some instant later
        // than a constant. `before`/`after` are captured around the
        // `upsert_run_results` call above; the bounds are inclusive because the
        // clock's resolution may not separate three calls made in a row.
        assert!(
            row.created_at >= before && row.created_at <= after,
            "created_at must be the instant of this write, inside [{before}, {after}]: {}",
            row.created_at,
        );
        // …and it is therefore not the fixture instant the caller-supplied columns
        // carry, which is the transposition this half of the assertion rules out.
        assert_ne!(row.created_at, now());
        // `tenant_id` and `updated_at` have no contract field, which is what
        // `test_case_result_to_sdk` says; there is nothing to assert about them
        // except that the record compiles without them.
        assert!(!row.id.is_nil());
    }

    // -----------------------------------------------------------------------
    // The three aggregate reads Task 18's dashboard is built on
    // -----------------------------------------------------------------------

    /// `(status, rows)` sorted by status, so a test asserts on a set rather than
    /// on whatever order the planner produced for a `GROUP BY`.
    fn sorted_counts(counts: Vec<StatusRowCount>) -> Vec<(String, u64)> {
        let mut out: Vec<(String, u64)> = counts
            .into_iter()
            .map(|count| (count.status, count.rows))
            .collect();
        out.sort();
        out
    }

    /// Seed one run's file-level rows, each with `run_finished_at = at`.
    async fn seed_run(
        conn: &toolkit_db::secure::DbConn<'_>,
        tenant: Uuid,
        run: Uuid,
        at: Option<OffsetDateTime>,
        statuses: &[&str],
    ) {
        let files = statuses
            .iter()
            .enumerate()
            .map(|(n, status)| NewTestResult {
                run_finished_at: at,
                ..result_row(&format!("tests/t{n}.py"), &format!("test_{n}"), status)
            })
            .collect();
        OrmResultsRepository
            .upsert_run_results(conn, &scope(tenant), tenant, run, files, vec![])
            .await
            .unwrap();
    }

    /// **The grouping is per `(run, status)` and the counts are row counts.**
    ///
    /// Two runs are seeded with overlapping statuses, and only one is asked
    /// about: a grouping that dropped `run_id` would merge them, and a query that
    /// ignored `run_ids` would return both. `ERROR` and `FAILED` come back as
    /// **separate groups** — folding them is the domain's job
    /// (`domain::service::ingest::classify`) and a repository that folded here
    /// would have put legacy's status vocabulary in two places.
    #[tokio::test]
    async fn per_run_status_counts_are_grouped_by_run_and_status() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let asked = Uuid::from_u128(0x40);
        let other = Uuid::from_u128(0x41);

        seed_run(
            &conn,
            tenant,
            asked,
            Some(now()),
            &["PASSED", "PASSED", "ERROR", "FAILED", "SKIPPED"],
        )
        .await;
        seed_run(&conn, tenant, other, Some(now()), &["PASSED", "PASSED"]).await;

        let mut counts = OrmResultsRepository
            .run_status_counts(&conn, &scope(tenant), &[asked])
            .await
            .unwrap();
        counts.sort_by(|a, b| a.status.cmp(&b.status));

        assert_eq!(
            counts
                .iter()
                .map(|c| (c.status.as_str(), c.rows))
                .collect::<Vec<_>>(),
            vec![("ERROR", 1), ("FAILED", 1), ("PASSED", 2), ("SKIPPED", 1)],
            "four groups for the one run asked about: {counts:?}"
        );
        assert!(
            counts.iter().all(|c| c.run_id == asked),
            "the other run's rows must not be in the answer: {counts:?}"
        );
        assert!(
            counts.iter().all(|c| c.run_finished_at == Some(now())),
            "the run's instant is carried on every group, because the day fold \
             needs it: {counts:?}"
        );
    }

    /// An empty `run_ids` is an empty answer.
    ///
    /// `IN ()` is a syntax error on some dialects and a match-nothing on others,
    /// so the guard is the contract rather than an optimisation. Rows exist, so a
    /// missing guard that degenerated to "no predicate" would return them.
    ///
    /// That the guard also issues **no statement** is the other half of its
    /// contract and is deliberately *not* asserted here: it is not observable
    /// through the repository's own return value, and the title said so until
    /// Task 18's fix round. It is stated on the trait method instead.
    #[tokio::test]
    async fn asking_about_no_runs_returns_nothing_rather_than_everything() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        seed_run(
            &conn,
            tenant,
            Uuid::from_u128(0x42),
            Some(now()),
            &["PASSED"],
        )
        .await;

        let counts = OrmResultsRepository
            .run_status_counts(&conn, &scope(tenant), &[])
            .await
            .unwrap();
        assert!(counts.is_empty(), "{counts:?}");
    }

    /// **The window is a lower bound on the bare `run_finished_at`.**
    ///
    /// Three runs: one inside, one before, and one that never finished. The
    /// unfinished run is the assertion that matters — legacy buckets it on
    /// `COALESCE(finished_at, created_at)` and would include it, and this gear
    /// deliberately does not, because no index covers a `COALESCE`. The trait's
    /// doc records that difference; this pins it.
    #[tokio::test]
    async fn the_window_admits_finished_runs_at_or_after_since_and_no_others() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let since = now();
        let inside = Uuid::from_u128(0x43);
        let before = Uuid::from_u128(0x44);
        let unfinished = Uuid::from_u128(0x45);

        seed_run(&conn, tenant, inside, Some(since), &["PASSED"]).await;
        seed_run(
            &conn,
            tenant,
            before,
            Some(since - Duration::hours(1)),
            &["PASSED"],
        )
        .await;
        seed_run(&conn, tenant, unfinished, None, &["RUNNING"]).await;

        let counts = OrmResultsRepository
            .run_status_counts_since(&conn, &scope(tenant), since)
            .await
            .unwrap();
        assert_eq!(
            counts.iter().map(|c| c.run_id).collect::<Vec<_>>(),
            vec![inside],
            "only the run finished at or after `since`: {counts:?}"
        );
    }

    /// Another tenant's rows are invisible to both counter reads.
    ///
    /// The point of doing this through the repository rather than by inspecting
    /// SQL: `project_all` is what keeps the scope on a grouped query, and a
    /// projection built from a raw `Select` would drop it silently.
    #[tokio::test]
    async fn the_counter_reads_are_scoped_to_the_caller() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(TENANT);
        let theirs = Uuid::from_u128(0xB);
        let run = Uuid::from_u128(0x46);

        seed_run(&conn, mine, run, Some(now()), &["PASSED"]).await;
        seed_run(&conn, theirs, run, Some(now()), &["FAILED", "FAILED"]).await;

        let counts = OrmResultsRepository
            .run_status_counts(&conn, &scope(mine), &[run])
            .await
            .unwrap();
        assert_eq!(
            counts
                .iter()
                .map(|c| (c.status.as_str(), c.rows))
                .collect::<Vec<_>>(),
            vec![("PASSED", 1)],
            "the other tenant seeded the same run id: {counts:?}"
        );

        let windowed = OrmResultsRepository
            .run_status_counts_since(&conn, &scope(mine), now())
            .await
            .unwrap();
        assert_eq!(
            windowed
                .iter()
                .map(|c| (c.status.as_str(), c.rows))
                .collect::<Vec<_>>(),
            vec![("PASSED", 1)],
            "{windowed:?}"
        );
    }

    /// **The total counts runs, not rows**, and it counts the caller's runs only.
    ///
    /// Two runs of three rows each must be 2 and not 6 — a `COUNT(*)` instead of
    /// `COUNT(DISTINCT run_id)` would be 6 and would look plausible on a
    /// dashboard. The other tenant's run makes the scope observable, and the
    /// empty-database leg is asserted because an aggregate with no `GROUP BY`
    /// returns one row carrying zero rather than no rows.
    #[tokio::test]
    async fn the_run_total_counts_runs_and_not_rows() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(TENANT);
        let theirs = Uuid::from_u128(0xB);

        assert_eq!(
            OrmResultsRepository
                .count_ingested_runs(&conn, &scope(mine))
                .await
                .unwrap(),
            0,
            "an empty projection is zero, not an error"
        );

        for run in [Uuid::from_u128(0x47), Uuid::from_u128(0x48)] {
            seed_run(
                &conn,
                mine,
                run,
                Some(now()),
                &["PASSED", "FAILED", "SKIPPED"],
            )
            .await;
        }
        seed_run(
            &conn,
            theirs,
            Uuid::from_u128(0x49),
            Some(now()),
            &["PASSED"],
        )
        .await;

        assert_eq!(
            OrmResultsRepository
                .count_ingested_runs(&conn, &scope(mine))
                .await
                .unwrap(),
            2,
        );
        assert_eq!(
            OrmResultsRepository
                .count_ingested_runs(&conn, &scope(theirs))
                .await
                .unwrap(),
            1,
        );
    }

    /// **The KPI window is the `COALESCE`, and it is half-open at both ends of
    /// the pair.**
    ///
    /// Legacy's two windows are `>= NOW() - 24h` with no upper bound and
    /// `[NOW() - 48h, NOW() - 24h)` (`manager/src/routes/dashboard.rs:319-321`,
    /// `:323-327`), over `COALESCE(rr.finished_at, rr.created_at)`. Four runs, one
    /// per property, and each one turns a different mutation red:
    ///
    /// * `recent` (finished 10h ago) — the ordinary case, and the row that makes
    ///   the previous window's *upper* bound observable: drop it and `recent`
    ///   leaks into `previous`.
    /// * `boundary` (finished exactly 24h ago) — belongs to the **current**
    ///   window (`>= from`) and not to the previous (`< to`). A `>` or a `<=`
    ///   anywhere in the pair moves it and double-counts or loses it.
    ///   [`Duration::hours`] arithmetic makes the two calls agree on the instant
    ///   exactly, which is the whole reason the boundary is testable at all.
    /// * `unfinished` (`run_finished_at` **`NULL`**, so its effective instant is
    ///   the `created_at` the repository stamps — now) — the row legacy counts
    ///   and `run_status_counts_since` drops. Replacing `effective_ts()` with the
    ///   bare column makes it vanish, which is this rule's whole difference from
    ///   the daily trend's.
    /// * `stale` (finished 60h ago) — outside both windows, so a missing lower
    ///   bound is visible rather than merely unasserted.
    ///
    /// Statuses are distinct per run so a lost `GROUP BY` cannot be mistaken for
    /// a correct merge, and the other tenant's row is what pins the scope.
    #[tokio::test]
    async fn the_kpi_window_counts_by_status_over_the_coalesced_instant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(TENANT);
        let theirs = Uuid::from_u128(0xB);
        let at = now();
        let from = at - Duration::hours(24);
        let prev_from = from - Duration::hours(24);

        seed_run(
            &conn,
            mine,
            Uuid::from_u128(0x60),
            Some(at - Duration::hours(10)),
            &["PASSED", "PASSED", "FAILED"],
        )
        .await;
        seed_run(&conn, mine, Uuid::from_u128(0x61), Some(from), &["SKIPPED"]).await;
        seed_run(&conn, mine, Uuid::from_u128(0x62), None, &["ERROR"]).await;
        seed_run(
            &conn,
            mine,
            Uuid::from_u128(0x63),
            Some(at - Duration::hours(30)),
            &["RUNNING"],
        )
        .await;
        seed_run(
            &conn,
            mine,
            Uuid::from_u128(0x64),
            Some(at - Duration::hours(60)),
            &["XFAIL"],
        )
        .await;
        seed_run(
            &conn,
            theirs,
            Uuid::from_u128(0x65),
            Some(at - Duration::hours(1)),
            &["PASSED"],
        )
        .await;

        assert_eq!(
            sorted_counts(
                OrmResultsRepository
                    .effective_status_counts(&conn, &scope(mine), from, None)
                    .await
                    .unwrap()
            ),
            vec![
                ("ERROR".to_owned(), 1),
                ("FAILED".to_owned(), 1),
                ("PASSED".to_owned(), 2),
                ("SKIPPED".to_owned(), 1),
            ],
            "the 24h-ago boundary row is in, the NULL-finish row is in, the 30h \
             and 60h rows are out, and the other tenant's PASSED is out"
        );

        assert_eq!(
            sorted_counts(
                OrmResultsRepository
                    .effective_status_counts(&conn, &scope(mine), prev_from, Some(from))
                    .await
                    .unwrap()
            ),
            vec![("RUNNING".to_owned(), 1)],
            "only the 30h row: the boundary row belongs to the current window and \
             the 60h row is older than both"
        );

        assert_eq!(
            sorted_counts(
                OrmResultsRepository
                    .effective_status_counts(&conn, &scope(theirs), from, None)
                    .await
                    .unwrap()
            ),
            vec![("PASSED".to_owned(), 1)],
            "the other tenant sees its own row and nothing of mine"
        );
    }

    /// Seed one unfinished run — no `run_finished_at` — created at `created`.
    ///
    /// The row's own `created_at` is whatever the repository stamps, i.e. *now*,
    /// which is the whole point: these rows are freshly ingested and their run is
    /// old, so the two instants disagree by design.
    async fn seed_unfinished_run(
        conn: &toolkit_db::secure::DbConn<'_>,
        tenant: Uuid,
        run: Uuid,
        created: OffsetDateTime,
        statuses: &[&str],
    ) {
        let files = statuses
            .iter()
            .enumerate()
            .map(|(n, status)| NewTestResult {
                run_finished_at: None,
                run_created_at: Some(created),
                ..result_row(&format!("tests/t{n}.py"), &format!("test_{n}"), status)
            })
            .collect();
        OrmResultsRepository
            .upsert_run_results(conn, &scope(tenant), tenant, run, files, vec![])
            .await
            .unwrap();
    }

    /// **An unfinished run is windowed by the *run's* age, not by when its rows
    /// were last ingested.** This is the test for the defect Ruling A closed.
    ///
    /// The KPI rule counts runs that have not finished — legacy's query carries no
    /// phase predicate at all (`manager/src/routes/dashboard.rs:317-348`) — so the
    /// fallback column decides where every in-progress row lands. Two columns were
    /// available and only one is legacy's:
    ///
    /// * `qa_test_results.created_at` is when *this row* was written. Ingest is
    ///   delete-then-insert per run, and the reconcile sweep or a rebuild
    ///   re-projects the whole run every time it re-reads it
    ///   (`domain::service::ingest::IngestService::write_run_projection`), so
    ///   it is reset to `now` every time.
    /// * `run_created_at` is the run's own creation instant and never moves.
    ///
    /// Task 21b first shipped the first one. **Under it all three runs below land
    /// in the current window**, because all three have rows written by this test a
    /// moment ago: a run stuck for three days kept every `FAILED` row in
    /// `failed_24h_count` and every card at the top of `failed_recent`, for as long
    /// as it stayed stuck. Under `run_created_at` they partition by their real age,
    /// which is what this asserts — one run per window and one in neither, with a
    /// distinct status each so the answer names which run it found.
    ///
    /// `SQLite` tier: the property is which column the expression reads, and that
    /// is dialect-independent. `the_kpi_reads_are_accepted_by_real_postgres`
    /// carries the same fallback branch on the shipping dialect.
    #[tokio::test]
    async fn an_unfinished_runs_rows_are_windowed_by_the_runs_age_not_the_ingest_instant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let at = OffsetDateTime::now_utc();
        let from = at - Duration::hours(24);
        let prev_from = from - Duration::hours(24);

        // Stuck for two and a half days: in neither window.
        seed_unfinished_run(
            &conn,
            tenant,
            Uuid::from_u128(0x90),
            at - Duration::hours(60),
            &["XFAIL"],
        )
        .await;
        // Started 40 hours ago: the previous window.
        seed_unfinished_run(
            &conn,
            tenant,
            Uuid::from_u128(0x91),
            at - Duration::hours(40),
            &["SKIPPED"],
        )
        .await;
        // Started two hours ago: the current window.
        seed_unfinished_run(
            &conn,
            tenant,
            Uuid::from_u128(0x92),
            at - Duration::hours(2),
            &["FAILED"],
        )
        .await;

        assert_eq!(
            sorted_counts(
                OrmResultsRepository
                    .effective_status_counts(&conn, &scope(tenant), from, None)
                    .await
                    .unwrap()
            ),
            vec![("FAILED".to_owned(), 1)],
            "only the run started two hours ago. The 40h and 60h runs have rows \
             written a moment ago, so a window over the row's created_at would \
             return all three"
        );

        assert_eq!(
            sorted_counts(
                OrmResultsRepository
                    .effective_status_counts(&conn, &scope(tenant), prev_from, Some(from))
                    .await
                    .unwrap()
            ),
            vec![("SKIPPED".to_owned(), 1)],
            "only the 40h run: the 60h one is older than both bounds and the 2h \
             one is newer than the upper one"
        );

        let failures = OrmResultsRepository
            .recent_failures(&conn, &scope(tenant), &["FAILED", "ERROR"], from, 10)
            .await
            .unwrap();
        assert_eq!(
            failures.iter().map(|row| row.run_id).collect::<Vec<_>>(),
            vec![Uuid::from_u128(0x92)],
            "the card list uses the same window, so a stuck run's failures leave \
             it as the run ages: {failures:?}"
        );
        assert_eq!(
            failures[0].run_created_at,
            Some(at - Duration::hours(2)),
            "and the card's own instant is the run's, not the row's write time"
        );
    }

    /// **The failure card read: status set, window, order and `LIMIT`, all four
    /// in the statement.**
    ///
    /// Legacy's query is `WHERE tr.status IN ('FAILED','ERROR') AND
    /// COALESCE(...) >= NOW() - 24h ORDER BY COALESCE(...) DESC LIMIT 10`
    /// (`manager/src/routes/dashboard.rs:275-278`). Every clause has a row here
    /// that fails without it:
    ///
    /// * a `PASSED` and a `SKIPPED` row inside the window — a dropped status
    ///   predicate returns them;
    /// * a `FAILED` row 30 hours old — a dropped window returns it, and it is
    ///   *newer than nothing else*, so it would also be last rather than absent;
    /// * three admissible failures where the limit is two — a dropped `LIMIT`
    ///   returns three, and an ascending order returns the wrong two;
    /// * the newest admissible failure is the one with **no** `run_finished_at`,
    ///   so a bare-column `ORDER BY` sorts it wrong (`NULL`s last or first, by
    ///   dialect) and a bare-column *filter* drops it entirely.
    /// * another tenant's very recent failure, which no answer may contain.
    ///
    /// The `ERROR` row proves the set is the pair rather than `FAILED` alone.
    #[tokio::test]
    async fn recent_failures_are_the_newest_admissible_rows_up_to_the_limit() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(TENANT);
        let theirs = Uuid::from_u128(0xB);
        let at = now();
        let from = at - Duration::hours(24);

        // Ascending in effective instant, so the expected order is the reverse of
        // the seeding order and a copied-through ordering cannot pass.
        seed_run(
            &conn,
            mine,
            Uuid::from_u128(0x70),
            Some(at - Duration::hours(30)),
            &["FAILED"],
        )
        .await;
        seed_run(
            &conn,
            mine,
            Uuid::from_u128(0x71),
            Some(at - Duration::hours(20)),
            &["FAILED"],
        )
        .await;
        seed_run(
            &conn,
            mine,
            Uuid::from_u128(0x72),
            Some(at - Duration::hours(5)),
            &["ERROR"],
        )
        .await;
        seed_run(
            &conn,
            mine,
            Uuid::from_u128(0x73),
            Some(at - Duration::hours(2)),
            &["PASSED", "SKIPPED"],
        )
        .await;
        seed_run(&conn, mine, Uuid::from_u128(0x74), None, &["FAILED"]).await;
        seed_run(&conn, theirs, Uuid::from_u128(0x75), None, &["FAILED"]).await;

        let rows = OrmResultsRepository
            .recent_failures(&conn, &scope(mine), &["FAILED", "ERROR"], from, 2)
            .await
            .unwrap();

        assert_eq!(
            rows.iter()
                .map(|row| (row.run_id, row.status.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (Uuid::from_u128(0x74), "FAILED"),
                (Uuid::from_u128(0x72), "ERROR"),
            ],
            "newest first by the coalesced instant, capped at two: {rows:?}"
        );

        let all = OrmResultsRepository
            .recent_failures(&conn, &scope(mine), &["FAILED", "ERROR"], from, 10)
            .await
            .unwrap();
        assert_eq!(
            all.len(),
            3,
            "three admissible failures in the window - the 30h one and the other \
             tenant's are excluded by the window and the scope, and the PASSED \
             and SKIPPED rows by the status set: {all:?}"
        );

        assert!(
            OrmResultsRepository
                .recent_failures(&conn, &scope(mine), &[], from, 10)
                .await
                .unwrap()
                .is_empty(),
            "an empty status set answers empty and issues no statement"
        );
    }

    // -----------------------------------------------------------------------
    // The flaky groups Task 23b's dashboard field is built on
    // -----------------------------------------------------------------------

    /// One fixture row for the flaky reads: `(test_name, test_file, repo, plan,
    /// status)`.
    ///
    /// `repo` is a `u128` rather than a `Uuid` so a fixture line stays on one
    /// line, and `None` on either of the two plan halves is the "run named no
    /// plan" case the column allows.
    type FlakySeed<'a> = (&'a str, &'a str, Option<u128>, Option<&'a str>, &'a str);

    /// Seed one run whose rows carry per-row group keys, finished at `at`.
    ///
    /// [`seed_run`] cannot serve these tests: it derives `test_name` from the row
    /// index and takes `test_file`, `repo_id` and `plan_path` from
    /// [`result_row`]'s constants — so every row it writes for one run is in a
    /// *different* group on `test_name` and the *same* group on the plan pair,
    /// which is the opposite of what a grouping test needs to vary.
    async fn seed_flaky(
        conn: &toolkit_db::secure::DbConn<'_>,
        tenant: Uuid,
        run: Uuid,
        at: Option<OffsetDateTime>,
        rows: &[FlakySeed<'_>],
    ) {
        let files = rows
            .iter()
            .map(|(name, file, repo, plan, status)| NewTestResult {
                test_file: (*file).to_owned(),
                repo_id: repo.map(Uuid::from_u128),
                plan_path: plan.map(str::to_owned),
                run_finished_at: at,
                ..result_row(file, name, status)
            })
            .collect();
        OrmResultsRepository
            .upsert_run_results(conn, &scope(tenant), tenant, run, files, vec![])
            .await
            .unwrap();
    }

    /// `n` rows of one group with `status`, for a fixture that only cares about
    /// counts.
    fn flaky_rows<'a>(
        name: &'a str,
        plan: &'a str,
        status: &'a str,
        n: usize,
    ) -> Vec<FlakySeed<'a>> {
        vec![(name, "tests/f.py", Some(0xA1), Some(plan), status); n]
    }

    /// One flaky group flattened for assertion: `(test_name, repo_id, plan_path,
    /// passed, failed, total)`.
    ///
    /// A named alias because `clippy::type_complexity` denies the tuple inline,
    /// and the name is worth having anyway: the three counters are same-typed and
    /// the reader needs their order.
    type FlatGroup = (String, Option<Uuid>, Option<String>, u64, u64, u64);

    /// Every group flattened and sorted by the group key — so a test about the
    /// *grouping* asserts on a set rather than on the rank.
    fn sorted_groups(groups: Vec<crate::domain::repos::FlakyGroup>) -> Vec<FlatGroup> {
        let mut out: Vec<_> = groups
            .into_iter()
            .map(|g| {
                (
                    g.test_name,
                    g.repo_id,
                    g.plan_path,
                    g.passed,
                    g.failed,
                    g.total,
                )
            })
            .collect();
        out.sort();
        out
    }

    /// The two status partitions every flaky test passes, as the service does.
    const PASSED: [&str; 1] = crate::domain::service::ingest::PASSED_STATUSES;
    const FAILED: [&str; 2] = crate::domain::service::ingest::FAILED_STATUSES;

    /// **The grain is `(test_name, repo_id, plan_path)`, and every one of the
    /// three keys is load-bearing.**
    ///
    /// Legacy groups by `tr.test_name, rr.plan_id` (`manager/src/routes/
    /// dashboard.rs:392`), and `plan_id` is the `(repo_id, plan_path)` pair in
    /// this port. Four groups are seeded that differ from the first in exactly one
    /// key each, with a distinct `(passed, failed)` pair per group, so dropping
    /// any one key from the `GROUP BY` merges two groups whose counts then match
    /// neither expectation:
    ///
    /// * drop `plan_path` → group 1 merges with group 2,
    /// * drop `repo_id` → group 1 merges with group 3,
    /// * drop `test_name` → group 1 merges with group 4.
    ///
    /// **Group 1's rows are split across two runs**, which is the other half of
    /// the grain: legacy's grouping has no run in it, so a `run_id` that leaked
    /// into the `GROUP BY` would split that group into a pass-only and a
    /// fail-only half — and both would then be dropped by the `HAVING`, so group 1
    /// would vanish rather than merely double.
    ///
    /// `SQLite` tier: what is measured is which columns are in the `GROUP BY`,
    /// which is dialect-independent. Postgres' *refusal* of an ungrouped selected
    /// column is what
    /// [`the_flaky_read_is_accepted_by_real_postgres`] adds.
    #[tokio::test]
    async fn the_flaky_grain_is_the_test_name_and_the_plan_pair() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let at = now();
        let one = "plans/one/plan.yaml";
        let two = "plans/two/plan.yaml";

        // Group 1 — (test_alpha, 0xA1, one): 1 passed here, 2 failed in the run
        // below. Split across two runs on purpose.
        seed_flaky(
            &conn,
            tenant,
            Uuid::from_u128(0xF1),
            Some(at),
            &[("test_alpha", "tests/a.py", Some(0xA1), Some(one), "PASSED")],
        )
        .await;
        seed_flaky(
            &conn,
            tenant,
            Uuid::from_u128(0xF2),
            Some(at),
            &[
                ("test_alpha", "tests/a.py", Some(0xA1), Some(one), "FAILED"),
                ("test_alpha", "tests/a.py", Some(0xA1), Some(one), "ERROR"),
            ],
        )
        .await;

        let mut rest: Vec<FlakySeed<'_>> = Vec::new();
        // Group 2 — the same test under a different plan path: 2 passed, 3 failed.
        rest.extend(flaky_rows("test_alpha", two, "PASSED", 2));
        rest.extend(flaky_rows("test_alpha", two, "FAILED", 3));
        // Group 3 — the same test and path under a different repository: 4 and 5.
        rest.extend(std::iter::repeat_n(
            ("test_alpha", "tests/a.py", Some(0xA2), Some(one), "PASSED"),
            4,
        ));
        rest.extend(std::iter::repeat_n(
            ("test_alpha", "tests/a.py", Some(0xA2), Some(one), "FAILED"),
            5,
        ));
        // Group 4 — a different test under the same plan: 6 and 7.
        rest.extend(flaky_rows("test_beta", one, "PASSED", 6));
        rest.extend(flaky_rows("test_beta", one, "FAILED", 7));
        seed_flaky(&conn, tenant, Uuid::from_u128(0xF3), Some(at), &rest).await;

        let groups = OrmResultsRepository
            .flaky_groups(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
                10,
            )
            .await
            .unwrap();

        assert_eq!(
            sorted_groups(groups),
            vec![
                (
                    "test_alpha".to_owned(),
                    Some(Uuid::from_u128(0xA1)),
                    Some(one.to_owned()),
                    1,
                    2,
                    3
                ),
                (
                    "test_alpha".to_owned(),
                    Some(Uuid::from_u128(0xA1)),
                    Some(two.to_owned()),
                    2,
                    3,
                    5
                ),
                (
                    "test_alpha".to_owned(),
                    Some(Uuid::from_u128(0xA2)),
                    Some(one.to_owned()),
                    4,
                    5,
                    9
                ),
                (
                    "test_beta".to_owned(),
                    Some(Uuid::from_u128(0xA1)),
                    Some(one.to_owned()),
                    6,
                    7,
                    13
                ),
            ],
            "four groups differing in one key each, and group 1's rows come from \
             two runs; every (passed, failed, total) triple is distinct, so any \
             merge or split shows up as a wrong number rather than only a wrong \
             count of groups",
        );
    }

    /// **A group needs a pass *and* a failure, and the threshold is straddled on
    /// both sides.**
    ///
    /// Legacy's `HAVING` is
    /// `COUNT(… 'PASSED') > 0 AND COUNT(… 'FAILED','ERROR') > 0`
    /// (`manager/src/routes/dashboard.rs:393-394`) — the definition of
    /// "flaky" on this surface. Four groups:
    ///
    /// | group | passed | failed | admitted |
    /// |---|---|---|---|
    /// | `test_only_passed` | 3 | **0** | no |
    /// | `test_only_failed` | **0** | 3 | no |
    /// | `test_barely_flaky` | **1** | **1** | yes |
    /// | `test_never_counted` | 0 | 0 | no |
    ///
    /// Rows 1 and 3 straddle the failed threshold (0 against 1) and rows 2 and 3
    /// straddle the passed one, which is what makes this an assertion about `> 0`
    /// rather than about "has some rows". Dropping either `having` call admits the
    /// corresponding one-sided group; dropping both admits **all four**, and the
    /// exact-vector assertion below fails either way.
    ///
    /// `test_never_counted` holds four `SKIPPED` rows, so all three of its counters
    /// are `0` — and it is nonetheless a group, because `GROUP BY` emits one row
    /// per group of *rows* and not per counted row. So the `HAVING` is the only
    /// thing excluding it, and it returns as `(0, 0, 0)` once both calls are gone.
    /// **This doc claimed the opposite through Task 23b's review round** — that the
    /// denominator excluded it and that it survived the `having` mutation. Coverage
    /// is unaffected (the assertion is an exact vector, so an extra group fails it);
    /// only the reason was wrong. What the group actually adds is a case where
    /// *neither* counter is positive, which is the third of the three ways the
    /// two-sided threshold can be missed.
    ///
    /// `SQLite` tier: `HAVING` over a `COUNT(CASE …)` is the same statement on
    /// both dialects, and
    /// [`the_flaky_read_is_accepted_by_real_postgres`] runs it on the shipping one.
    #[tokio::test]
    async fn a_group_needs_both_a_pass_and_a_failure_to_be_flaky() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let at = now();
        let plan = "plans/having/plan.yaml";

        let mut rows: Vec<FlakySeed<'_>> = Vec::new();
        rows.extend(flaky_rows("test_only_passed", plan, "PASSED", 3));
        rows.extend(flaky_rows("test_only_failed", plan, "FAILED", 3));
        rows.extend(flaky_rows("test_barely_flaky", plan, "PASSED", 1));
        rows.extend(flaky_rows("test_barely_flaky", plan, "ERROR", 1));
        rows.extend(flaky_rows("test_never_counted", plan, "SKIPPED", 4));
        seed_flaky(&conn, tenant, Uuid::from_u128(0xF4), Some(at), &rows).await;

        let groups = OrmResultsRepository
            .flaky_groups(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
                10,
            )
            .await
            .unwrap();

        assert_eq!(
            groups
                .iter()
                .map(|g| (g.test_name.as_str(), g.passed, g.failed, g.total))
                .collect::<Vec<_>>(),
            vec![("test_barely_flaky", 1, 1, 2)],
            "only the group that has both, and it has exactly one of each - the \
             group just below the threshold on each side is seeded with three \
             rows, so an admitted one-sided group is unmistakable: {groups:?}",
        );
    }

    /// **The rank is `LEAST(passed, failed) DESC, total DESC` — the size of the
    /// smaller status group, not the total and not the pass count.**
    ///
    /// Legacy: `manager/src/routes/dashboard.rs:395-398`. Four groups, chosen so
    /// that three plausible wrong rankings each produce a different order:
    ///
    /// | group | passed | failed | smaller | total |
    /// |---|---|---|---|---|
    /// | `test_high` | 5 | 4 | **4** | 9 |
    /// | `test_tie_z` | 3 | 7 | **3** | 10 |
    /// | `test_tie_a` | 3 | 3 | **3** | 6 |
    /// | `test_low` | 9 | 1 | **1** | 10 |
    ///
    /// * Ranking by `total` instead puts `test_tie_z` and `test_low` (10) first
    ///   and `test_tie_a` (6) last.
    /// * Ranking by `passed` puts `test_low` (9) first, where it must be last.
    /// * Taking the *larger* of the two — the `<` in
    ///   [`super::smaller_of`] flipped to `>` — gives 9, 7, 5, 3, i.e.
    ///   `test_low` first again.
    /// * Ascending instead of descending reverses the whole list.
    /// * **Dropping the `total DESC` key** leaves the two tied groups ordered by
    ///   the `test_name` tiebreak, which is `test_tie_a` before `test_tie_z` — the
    ///   opposite of what `total` gives. The names are deliberately chosen so that
    ///   alphabetical and total order disagree on that pair; with the obvious
    ///   names they would have agreed and the key would be untested.
    ///
    /// `SQLite` tier: `CASE`-as-`LEAST` is one expression for both dialects, which
    /// is [`super::smaller_of`]'s whole point.
    /// [`the_flaky_read_is_accepted_by_real_postgres`] asserts the same order
    /// where `LEAST` would have been spelled natively.
    #[tokio::test]
    async fn the_flaky_rank_is_the_smaller_status_group_then_the_total() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let at = now();
        let plan = "plans/rank/plan.yaml";

        let mut rows: Vec<FlakySeed<'_>> = Vec::new();
        rows.extend(flaky_rows("test_high", plan, "PASSED", 5));
        rows.extend(flaky_rows("test_high", plan, "FAILED", 4));
        rows.extend(flaky_rows("test_tie_z", plan, "PASSED", 3));
        rows.extend(flaky_rows("test_tie_z", plan, "FAILED", 7));
        rows.extend(flaky_rows("test_tie_a", plan, "PASSED", 3));
        rows.extend(flaky_rows("test_tie_a", plan, "FAILED", 3));
        rows.extend(flaky_rows("test_low", plan, "PASSED", 9));
        rows.extend(flaky_rows("test_low", plan, "FAILED", 1));
        seed_flaky(&conn, tenant, Uuid::from_u128(0xF5), Some(at), &rows).await;

        let groups = OrmResultsRepository
            .flaky_groups(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
                10,
            )
            .await
            .unwrap();

        assert_eq!(
            groups
                .iter()
                .map(|g| (g.test_name.as_str(), g.passed, g.failed, g.total))
                .collect::<Vec<_>>(),
            vec![
                ("test_high", 5, 4, 9),
                ("test_tie_z", 3, 7, 10),
                ("test_tie_a", 3, 3, 6),
                ("test_low", 9, 1, 10),
            ],
            "the flakiest first by the smaller of the two counters; test_low has \
             the most passes and one of the two highest totals and still sorts \
             last: {groups:?}",
        );
    }

    /// **The `LIMIT` is the caller's, it is applied in SQL, and the group it
    /// drops is the least flaky one.**
    ///
    /// Legacy's is `LIMIT 10` (`manager/src/routes/dashboard.rs:399`); the
    /// constant lives in `domain::service::dashboard` and the value is a
    /// parameter here, exactly as `recent_failures`' is.
    ///
    /// **Eleven groups, one more than the limit**, with `passed = failed = k` for
    /// `k` in `1..=11` so the rank is `k` descending. Asked for ten, the answer
    /// must be `k = 11..=2` — which straddles the limit in the way that matters:
    /// it pins *which* group falls off, not merely how many come back. A `LIMIT`
    /// applied before the `ORDER BY`, or dropped in favour of a domain-side
    /// `take(10)` over an unordered read, would return ten groups too.
    ///
    /// Asked for eleven, all eleven come back, so the eleventh group is genuinely
    /// admissible and its absence above is the limit rather than the `HAVING`.
    ///
    /// `SQLite` tier: a `LIMIT` after an `ORDER BY` is the same on both dialects.
    #[tokio::test]
    async fn the_flaky_list_is_capped_at_the_callers_limit_and_drops_the_least_flaky() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let at = now();

        let names: Vec<String> = (1..=11).map(|k| format!("test_k{k:02}")).collect();
        let plans: Vec<String> = (1..=11)
            .map(|k| format!("plans/k{k:02}/plan.yaml"))
            .collect();
        let mut rows: Vec<FlakySeed<'_>> = Vec::new();
        for (k, (name, plan)) in (1..=11usize).zip(names.iter().zip(plans.iter())) {
            rows.extend(flaky_rows(name, plan, "PASSED", k));
            rows.extend(flaky_rows(name, plan, "FAILED", k));
        }
        seed_flaky(&conn, tenant, Uuid::from_u128(0xF6), Some(at), &rows).await;

        let ten = OrmResultsRepository
            .flaky_groups(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
                10,
            )
            .await
            .unwrap();

        let expected: Vec<&str> = (2..=11).rev().map(|k| names[k - 1].as_str()).collect();
        assert_eq!(
            ten.iter().map(|g| g.test_name.as_str()).collect::<Vec<_>>(),
            expected,
            "the ten flakiest, ranked; test_k01 is the one that falls off, not \
             whichever group the planner happened to emit eleventh",
        );

        let all = OrmResultsRepository
            .flaky_groups(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
                11,
            )
            .await
            .unwrap();
        assert_eq!(
            all.len(),
            11,
            "all eleven groups are admissible, so the missing one above is the \
             limit and not the HAVING",
        );
        assert_eq!(all[10].test_name, names[0], "and it is the least flaky one");
    }

    /// **The denominator is legacy's *sixth* classification, and it is not
    /// `COUNT(*)`.**
    ///
    /// `COUNT(*) FILTER (WHERE tr.status IN ('PASSED','FAILED','ERROR'))`
    /// (`manager/src/routes/dashboard.rs:388`) — ruling R5's sixth row, indexed in
    /// `domain::service::ingest`' header. One group holding every kind of row this
    /// gear has seen:
    ///
    /// | rows | status | in `passed` | in `failed` | in `total` |
    /// |---|---|---|---|---|
    /// | 2 | `PASSED` | yes | | yes |
    /// | 1 | `FAILED` | | yes | yes |
    /// | 3 | `ERROR` | | yes | yes |
    /// | 4 | `SKIPPED` | | | no |
    /// | 1 | `PENDING` | | | no |
    /// | 1 | `XFAIL` | | | no |
    /// | 1 | `passed` (lowercase) | | | no |
    ///
    /// So `(2, 4, 6)` and not `(2, 4, 13)`. Four mutations this separates, each
    /// producing a different triple: `total` as `COUNT(*)` gives 13; `ERROR`
    /// dropped from the failure set gives `(2, 1, 3)`; the lowercase row admitted
    /// gives `(3, 4, 7)`; and the two counters transposed gives `(4, 2, 6)`, which
    /// the deliberately unequal 2-against-4 makes visible.
    ///
    /// `SQLite` tier: the partition is the caller's slice list, which is
    /// dialect-independent.
    #[tokio::test]
    async fn the_flaky_denominator_counts_only_passed_failed_and_error() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let at = now();
        let plan = "plans/denominator/plan.yaml";

        let mut rows: Vec<FlakySeed<'_>> = Vec::new();
        rows.extend(flaky_rows("test_mixed", plan, "PASSED", 2));
        rows.extend(flaky_rows("test_mixed", plan, "FAILED", 1));
        rows.extend(flaky_rows("test_mixed", plan, "ERROR", 3));
        rows.extend(flaky_rows("test_mixed", plan, "SKIPPED", 4));
        rows.extend(flaky_rows("test_mixed", plan, "PENDING", 1));
        rows.extend(flaky_rows("test_mixed", plan, "XFAIL", 1));
        rows.extend(flaky_rows("test_mixed", plan, "passed", 1));
        seed_flaky(&conn, tenant, Uuid::from_u128(0xF7), Some(at), &rows).await;

        let groups = OrmResultsRepository
            .flaky_groups(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
                10,
            )
            .await
            .unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(
            (groups[0].passed, groups[0].failed, groups[0].total),
            (2, 4, 6),
            "thirteen rows in the group and six in the denominator; ERROR is a \
             failure and a lowercase passed is neither: {groups:?}",
        );
    }

    /// **`test_file` is `MAX(test_file)` over the group, and `""` becomes the
    /// no-file case for the domain.**
    ///
    /// Legacy selects `MAX(tr.test_file)` (`manager/src/routes/
    /// dashboard.rs:384`) because `test_file` is not one of its grouping keys —
    /// verified at source, and recorded because the brief for this task expected
    /// legacy to have no answer here. Two groups:
    ///
    /// * one whose rows name `tests/m.py`, `tests/z.py` and `tests/a.py` in that
    ///   insertion order, so `MAX` is `tests/z.py` and neither the first nor the
    ///   last inserted row is the winner — which is what separates a genuine
    ///   aggregate from `SQLite`'s bare-column pick;
    /// * one whose every row carries `""`, this schema's single spelling of
    ///   "absent" (legacy's column is nullable and `MAX` skips `NULL`s). The
    ///   aggregate is `""`, and `domain::service::dashboard::flaky_card` is where
    ///   that becomes the `None` legacy would have rendered.
    ///
    /// `SQLite` tier for the pick itself; the *dialect* hazard is that `SQLite`
    /// would silently accept `test_file` selected without an aggregate at all,
    /// which is what [`the_flaky_read_is_accepted_by_real_postgres`] exists to
    /// catch.
    #[tokio::test]
    async fn the_representative_file_is_the_alphabetically_last_one_legacy_picks() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let at = now();
        let files = "plans/files/plan.yaml";
        let blank = "plans/blank/plan.yaml";

        seed_flaky(
            &conn,
            tenant,
            Uuid::from_u128(0xF8),
            Some(at),
            &[
                (
                    "test_files",
                    "tests/m.py",
                    Some(0xA1),
                    Some(files),
                    "PASSED",
                ),
                (
                    "test_files",
                    "tests/z.py",
                    Some(0xA1),
                    Some(files),
                    "FAILED",
                ),
                (
                    "test_files",
                    "tests/a.py",
                    Some(0xA1),
                    Some(files),
                    "PASSED",
                ),
                ("test_blank", "", Some(0xA1), Some(blank), "PASSED"),
                ("test_blank", "", Some(0xA1), Some(blank), "ERROR"),
            ],
        )
        .await;

        let groups = OrmResultsRepository
            .flaky_groups(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
                10,
            )
            .await
            .unwrap();

        let mut files_of: Vec<(&str, &str)> = groups
            .iter()
            .map(|g| (g.test_name.as_str(), g.test_file.as_str()))
            .collect();
        files_of.sort_unstable();
        assert_eq!(
            files_of,
            vec![("test_blank", ""), ("test_files", "tests/z.py")],
            "the alphabetically last path wins, and it is neither the first nor \
             the last row inserted; a group with no path at all reports the empty \
             string rather than a path from another group: {groups:?}",
        );
    }

    /// **The window is the effective timestamp over seven days, and the answer is
    /// the caller's tenant only.**
    ///
    /// Legacy: `COALESCE(rr.finished_at, rr.created_at) >= NOW() - INTERVAL '7
    /// days'` (`manager/src/routes/dashboard.rs:391`) — the same expression the
    /// KPI reads use, so this inherits `kpi_window`'s decomposition and
    /// `effective_ts`' argument about which fallback column is legacy's. Four
    /// groups, each of them flaky on its own:
    ///
    /// * `test_inside` — finished three days ago, admitted.
    /// * `test_unfinished` — **no finish instant at all** and a run created two
    ///   days ago, so only the fallback branch admits it. A window written on
    ///   `run_finished_at` alone loses it.
    /// * `test_stale` — finished eight days ago, excluded. One day outside, so the
    ///   pair straddles the boundary rather than sitting either side of a month.
    /// * the other tenant's `test_inside`, inside the window and invisible. Same
    ///   group key as the first, so a dropped scope predicate doubles that group's
    ///   counters instead of adding a row — which the distinct 1-against-2 counts
    ///   below make visible either way.
    ///
    /// `SQLite` tier: the *columns* the window reads are dialect-independent, and
    /// `an_unfinished_runs_rows_are_windowed_by_the_runs_age_not_the_ingest_instant`
    /// makes the same point for the KPI pair. Timestamp comparison on the shipping
    /// dialect is [`the_flaky_read_is_accepted_by_real_postgres`]'s.
    #[tokio::test]
    async fn the_flaky_window_is_seven_days_of_the_effective_timestamp_and_scoped() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(TENANT);
        let theirs = Uuid::from_u128(0xB);
        let at = OffsetDateTime::now_utc();
        let since = at - Duration::days(7);
        let plan = "plans/window/plan.yaml";

        let mut inside: Vec<FlakySeed<'_>> = Vec::new();
        inside.extend(flaky_rows("test_inside", plan, "PASSED", 1));
        inside.extend(flaky_rows("test_inside", plan, "FAILED", 2));
        seed_flaky(
            &conn,
            mine,
            Uuid::from_u128(0xFA),
            Some(at - Duration::days(3)),
            &inside,
        )
        .await;

        let mut stale: Vec<FlakySeed<'_>> = Vec::new();
        stale.extend(flaky_rows("test_stale", plan, "PASSED", 3));
        stale.extend(flaky_rows("test_stale", plan, "FAILED", 4));
        seed_flaky(
            &conn,
            mine,
            Uuid::from_u128(0xFB),
            Some(at - Duration::days(8)),
            &stale,
        )
        .await;

        // No finish instant: only `run_created_at` can put this in the window.
        let mut unfinished: Vec<FlakySeed<'_>> = Vec::new();
        unfinished.extend(flaky_rows("test_unfinished", plan, "PASSED", 5));
        unfinished.extend(flaky_rows("test_unfinished", plan, "ERROR", 6));
        let files = unfinished
            .iter()
            .map(|(name, file, repo, plan, status)| NewTestResult {
                test_file: (*file).to_owned(),
                repo_id: repo.map(Uuid::from_u128),
                plan_path: plan.map(str::to_owned),
                run_finished_at: None,
                run_created_at: Some(at - Duration::days(2)),
                ..result_row(file, name, status)
            })
            .collect();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(mine),
                mine,
                Uuid::from_u128(0xFC),
                files,
                vec![],
            )
            .await
            .unwrap();

        seed_flaky(
            &conn,
            theirs,
            Uuid::from_u128(0xFD),
            Some(at - Duration::days(1)),
            &inside,
        )
        .await;

        let groups = OrmResultsRepository
            .flaky_groups(&conn, &scope(mine), &PASSED, &FAILED, since, 10)
            .await
            .unwrap();

        assert_eq!(
            groups
                .iter()
                .map(|g| (g.test_name.as_str(), g.passed, g.failed))
                .collect::<Vec<_>>(),
            vec![("test_unfinished", 5, 6), ("test_inside", 1, 2)],
            "the three-day and the never-finished runs; the eight-day one is one \
             day outside and the other tenant's rows are outside the scope, so \
             test_inside stays at (1, 2) rather than doubling: {groups:?}",
        );
    }

    /// **Either status set empty answers empty.**
    ///
    /// Legacy's `HAVING` needs both counters positive, so with no passed statuses
    /// no group can qualify — the answer is empty on the *rule* and not only
    /// because a statement was skipped. The fixture is a group that is flaky under
    /// the real sets, so a wrong implementation would have something to return.
    ///
    /// # What this test cannot catch, stated because the sweep found it
    ///
    /// **Deleting the early return in `flaky_groups` leaves the whole suite
    /// green.** Measured: `SeaQuery` renders an empty `IN` list as the constant
    /// `1 = 2` (`sea-query-0.32.7/src/backend/query_builder.rs:386`) rather than
    /// as the `IN ()` that is a syntax error on some dialects, so the unguarded
    /// statement counts `0` for the empty partition, the `HAVING` rejects every
    /// group, and the answer is this same empty `Vec`. The guard is a decision
    /// about *not issuing a statement*, and the assertions here are about the
    /// answer.
    ///
    /// So what this pins is that the empty-set answer is empty **by whichever
    /// route** — which is the property a caller depends on, and which a future
    /// edit that made an empty partition mean "match everything" would break. Same
    /// shape of gap as `latest_per_test`'s reduction and
    /// `collect_sea_repo::list_counts_for`'s own guard, recorded the same way.
    ///
    /// **The claim it replaces was inherited and is wrong for this builder**, on two
    /// sites and not three. `ResultsRepository::run_status_counts` and
    /// `run_status_counts`' implementation comment both justify their guard as
    /// protecting against `IN ()`; on `SeaQuery` there is nothing there to protect
    /// against, and each now carries a pointer to that measurement rather than
    /// having its sentence rewritten, because plan carried item 3 makes those docs
    /// Task 18's. **`collect_sea_repo::list_counts_for` was never stale** — Task
    /// 23b's report first listed it among them, which was wrong: that doc already
    /// carries the corrected, measured statement and the argument for keeping the
    /// guard anyway. It is the precedent the two pointers cite.
    ///
    /// `SQLite` tier: the guard is a Rust early return and touches no dialect.
    #[tokio::test]
    async fn a_flaky_read_with_an_empty_status_set_answers_empty() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let at = now();
        let plan = "plans/guard/plan.yaml";

        let mut rows: Vec<FlakySeed<'_>> = Vec::new();
        rows.extend(flaky_rows("test_guard", plan, "PASSED", 2));
        rows.extend(flaky_rows("test_guard", plan, "FAILED", 2));
        seed_flaky(&conn, tenant, Uuid::from_u128(0xFE), Some(at), &rows).await;

        let since = at - Duration::days(7);
        assert_eq!(
            OrmResultsRepository
                .flaky_groups(&conn, &scope(tenant), &PASSED, &FAILED, since, 10)
                .await
                .unwrap()
                .len(),
            1,
            "the fixture is flaky under the real sets, so the two empty-set \
             answers below are the guard and not an empty table",
        );
        assert!(
            OrmResultsRepository
                .flaky_groups(&conn, &scope(tenant), &[], &FAILED, since, 10)
                .await
                .unwrap()
                .is_empty(),
            "no passed status means no group can satisfy the HAVING"
        );
        assert!(
            OrmResultsRepository
                .flaky_groups(&conn, &scope(tenant), &PASSED, &[], since, 10)
                .await
                .unwrap()
                .is_empty(),
            "and neither can it with no failed status"
        );
    }

    /// **The three aggregates run on real Postgres, which is the only dialect
    /// this gear ships.**
    ///
    /// Every other test here is `SQLite`, and for these three that is a genuine
    /// gap rather than a formality: they are the first statements in this gear
    /// with a `GROUP BY` and a `COUNT(DISTINCT ...)`, and the two dialects differ
    /// on exactly that ground — Postgres refuses a selected column that is not
    /// grouped or aggregated, where `SQLite` picks an arbitrary row and says
    /// nothing. So a grouping key dropped from `grouped_status_counts` would pass
    /// the `SQLite` tier and fail at runtime on a deployment.
    ///
    /// One container and one assertion per read, because what is under test is
    /// whether the *statement* is accepted and returns the same shape — the
    /// semantics are covered above, on the tier that needs no Docker. `run_a` and
    /// `run_b` share a status so a lost `run_id` group key merges them.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn the_aggregate_reads_are_accepted_by_real_postgres() {
        use crate::infra::storage::test_db::pg_db;

        let harness = pg_db().await;
        let conn = harness.db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let run_a = Uuid::from_u128(0x4A);
        let run_b = Uuid::from_u128(0x4B);

        seed_run(
            &conn,
            tenant,
            run_a,
            Some(now()),
            &["PASSED", "PASSED", "ERROR"],
        )
        .await;
        seed_run(
            &conn,
            tenant,
            run_b,
            Some(now() + Duration::hours(1)),
            &["PASSED"],
        )
        .await;

        let mut per_run = OrmResultsRepository
            .run_status_counts(&conn, &scope(tenant), &[run_a, run_b])
            .await
            .unwrap();
        per_run.sort_by(|a, b| a.run_id.cmp(&b.run_id).then(a.status.cmp(&b.status)));
        assert_eq!(
            per_run
                .iter()
                .map(|c| (c.run_id, c.status.as_str(), c.rows))
                .collect::<Vec<_>>(),
            vec![
                (run_a, "ERROR", 1),
                (run_a, "PASSED", 2),
                (run_b, "PASSED", 1),
            ],
            "three groups, and run_a's two must not merge with run_b's: {per_run:?}"
        );

        let windowed = OrmResultsRepository
            .run_status_counts_since(&conn, &scope(tenant), now() + Duration::minutes(30))
            .await
            .unwrap();
        assert_eq!(
            windowed
                .iter()
                .map(|c| (c.run_id, c.rows))
                .collect::<Vec<_>>(),
            vec![(run_b, 1)],
            "only the later run is inside the window: {windowed:?}"
        );
        assert_eq!(
            windowed[0].run_finished_at,
            Some(now() + Duration::hours(1)),
            "the instant survives the round trip as a grouping key",
        );

        assert_eq!(
            OrmResultsRepository
                .count_ingested_runs(&conn, &scope(tenant))
                .await
                .unwrap(),
            2,
            "two runs, four rows",
        );
    }

    /// **`list_for_plan` runs on real Postgres for the same reason
    /// [`Self::recent_failures`]' own dialect test does**: its `ORDER BY` is
    /// `effective_ts` (the `COALESCE`), and `SQLite` stores a timestamp as text
    /// and compares lexicographically, so it cannot vouch for the
    /// `timestamptz` arithmetic underneath it. Neither the sort nor the
    /// `kpi_window` filter is novel here — `recent_failures` already exercises
    /// both — but the combination (a `plan_path` equality predicate, no status
    /// filter, no `LIMIT`, a full-row projection) has not itself run against
    /// this dialect before, and the fix round that replaced this method's
    /// filter with `kpi_window` (`ResultsRepository::list_for_plan`'s doc)
    /// is exactly the kind of change this test exists to catch a regression
    /// in. The SQLite-tier tests above cover the filter/order shape; this
    /// covers the dialect.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn the_plan_read_is_accepted_by_real_postgres() {
        use crate::infra::storage::test_db::pg_db;

        let harness = pg_db().await;
        let conn = harness.db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let plan_path = "plans/smoke/plan.yaml";

        // Finished an hour ago.
        seed_run(
            &conn,
            tenant,
            Uuid::from_u128(0x5A),
            Some(now() - Duration::hours(1)),
            &["PASSED"],
        )
        .await;
        // Still running: no `run_finished_at`, so `effective_ts` falls back to
        // `run_created_at`, which `result_row` stamps two hours before `now()` —
        // older than the finished run above, unlike the SQLite-tier test's
        // in-progress row. Chosen so this test also proves the window bound
        // reaches the fallback column on real Postgres.
        seed_run(&conn, tenant, Uuid::from_u128(0x5B), None, &["RUNNING"]).await;

        let rows = OrmResultsRepository
            .list_for_plan(&conn, &scope(tenant), plan_path, now() - Duration::days(1))
            .await
            .unwrap();

        assert_eq!(
            rows.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(),
            vec!["PASSED", "RUNNING"],
            "newest effective instant first: {rows:?}"
        );

        let windowed = OrmResultsRepository
            .list_for_plan(
                &conn,
                &scope(tenant),
                plan_path,
                now() - Duration::minutes(90),
            )
            .await
            .unwrap();
        assert_eq!(
            windowed
                .iter()
                .map(|r| r.status.as_str())
                .collect::<Vec<_>>(),
            vec!["PASSED"],
            "the in-progress run's fallback instant is outside a 90-minute window: \
             {windowed:?}"
        );
    }

    /// **Every tenant with a row, and nothing outside the scope.**
    ///
    /// Two properties in one test because they are the same statement's two
    /// halves: the `DISTINCT` (three runs for one tenant is one entry) and the
    /// scope (a tenant the scope does not span is invisible even though the
    /// question is cross-tenant by nature). `AccessScope::for_tenants` here is a
    /// scope this test builds directly, exercising the trait method's own
    /// contract in isolation from its actual caller — `TenantDirectory` no
    /// longer compiles a scope this way at all; it passes
    /// `domain::elevated::enumeration_scope`'s `allow_all()`, the one call to
    /// that constructor this crate's production code sanctions (see that
    /// module's doc).
    ///
    /// `domain::service::tenants::tenants_tests` covers the service tier over the
    /// same real repository; this is the repository's own, beside its siblings.
    #[tokio::test]
    async fn the_tenant_enumeration_is_distinct_and_scoped() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0x0A);
        let theirs = Uuid::from_u128(0xB0);

        seed_run(&conn, mine, Uuid::from_u128(0x51), Some(now()), &["PASSED"]).await;
        seed_run(&conn, mine, Uuid::from_u128(0x52), Some(now()), &["FAILED"]).await;
        seed_run(
            &conn,
            theirs,
            Uuid::from_u128(0x53),
            Some(now()),
            &["PASSED"],
        )
        .await;

        assert_eq!(
            OrmResultsRepository
                .tenants_with_results(&conn, &AccessScope::for_tenants(vec![mine, theirs]))
                .await
                .unwrap(),
            vec![mine, theirs],
            "both tenants, ascending, each once despite two runs for the first"
        );
        assert_eq!(
            OrmResultsRepository
                .tenants_with_results(&conn, &scope(mine))
                .await
                .unwrap(),
            vec![mine],
            "a single-tenant scope must not leak the other tenant's existence"
        );
    }

    /// The enumeration runs on real Postgres.
    ///
    /// `SELECT DISTINCT` over one column with an `ORDER BY` on that same column
    /// is accepted by both dialects, so unlike the aggregate reads above this is
    /// not covering a known divergence — it is covering the fact that
    /// `project_all`'s generated statement is the one a deployment executes, and
    /// this read is the only one in the crate whose whole point is to span
    /// tenants. Cheap: one container, one assertion.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn the_tenant_enumeration_is_accepted_by_real_postgres() {
        use crate::infra::storage::test_db::pg_db;

        let harness = pg_db().await;
        let conn = harness.db.conn().unwrap();
        let mine = Uuid::from_u128(0x0A);
        let theirs = Uuid::from_u128(0xB0);

        seed_run(&conn, mine, Uuid::from_u128(0x54), Some(now()), &["PASSED"]).await;
        seed_run(
            &conn,
            theirs,
            Uuid::from_u128(0x55),
            Some(now()),
            &["PASSED"],
        )
        .await;

        assert_eq!(
            OrmResultsRepository
                .tenants_with_results(&conn, &AccessScope::for_tenants(vec![mine, theirs]))
                .await
                .unwrap(),
            vec![mine, theirs],
        );
    }

    /// **The two KPI reads run on real Postgres, because their timestamp
    /// expression is the one thing `SQLite` cannot vouch for.**
    ///
    /// Both reads window on `COALESCE(run_finished_at, run_created_at)` — as an
    /// `OR` of two bare-column branches on the filter side ([`kpi_window`]) and as
    /// the expression itself on the sort side ([`effective_ts`]) — and that is
    /// where the two dialects stop agreeing:
    ///
    /// * On `SQLite` a timestamp is **text** and the comparison is lexicographic,
    ///   so a window boundary can pass there for reasons that have nothing to do
    ///   with time ordering. On Postgres it is `timestamptz` arithmetic. The
    ///   window semantics are asserted on the `SQLite` tier, where they need no
    ///   Docker; what needs a container is that the same statement means the same
    ///   thing on the dialect this ships.
    /// * `effective_status_counts` selects a column beside an aggregate, and
    ///   Postgres **refuses** a selected column that is neither grouped nor
    ///   aggregated where `SQLite` picks an arbitrary row silently — the argument
    ///   [`the_aggregate_reads_are_accepted_by_real_postgres`] makes for the other
    ///   three. `run_created_at` is also the newest column in this schema, so this
    ///   is where a column that only the `SQLite` blob declares would surface.
    /// * `recent_failures` combines that expression with `ORDER BY … LIMIT`, the
    ///   first place in this gear where an un-indexable expression sorts a bounded
    ///   read. `NULL` ordering differs between dialects, which is exactly why the
    ///   newest row here has no `run_finished_at`.
    ///
    /// One container, three assertions: the grouping shape with the `COALESCE`
    /// admitting a `NULL`-finish row, the previous window's two bounds, and the
    /// order the bounded read comes back in.
    ///
    /// **This test earned its tier on the first run** — though not in the way the
    /// bullets above predicted. It came up red because the fixture's "outside
    /// both windows" row was stamped 40 hours back, which is *inside* the
    /// previous window; the `SQLite` tier had no such row and could not have said
    /// so. The row is now asserted where it actually belongs, which makes this
    /// the assertion for both bounds of the interval rather than for an empty
    /// answer.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn the_kpi_reads_are_accepted_by_real_postgres() {
        use crate::infra::storage::test_db::pg_db;

        let harness = pg_db().await;
        let conn = harness.db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let at = now();
        let from = at - Duration::hours(24);

        // Inside the window, finished.
        seed_run(
            &conn,
            tenant,
            Uuid::from_u128(0x80),
            Some(at - Duration::hours(3)),
            &["PASSED", "PASSED", "FAILED"],
        )
        .await;
        // Inside the window only through the fallback branch: no finish instant
        // at all, so `run_created_at` decides — which `result_row` stamps two
        // hours back, newer than the finished run above.
        seed_run(&conn, tenant, Uuid::from_u128(0x81), None, &["ERROR"]).await;
        // Outside the current window and inside the previous one.
        seed_run(
            &conn,
            tenant,
            Uuid::from_u128(0x82),
            Some(at - Duration::hours(40)),
            &["SKIPPED"],
        )
        .await;
        // Older than both windows.
        seed_run(
            &conn,
            tenant,
            Uuid::from_u128(0x83),
            Some(at - Duration::hours(60)),
            &["XFAIL"],
        )
        .await;

        assert_eq!(
            sorted_counts(
                OrmResultsRepository
                    .effective_status_counts(&conn, &scope(tenant), from, None)
                    .await
                    .unwrap()
            ),
            vec![
                ("ERROR".to_owned(), 1),
                ("FAILED".to_owned(), 1),
                ("PASSED".to_owned(), 2),
            ],
            "the NULL-finish row is inside the window; the 40h and 60h rows are \
             not",
        );

        assert_eq!(
            sorted_counts(
                OrmResultsRepository
                    .effective_status_counts(
                        &conn,
                        &scope(tenant),
                        from - Duration::hours(24),
                        Some(from),
                    )
                    .await
                    .unwrap()
            ),
            vec![("SKIPPED".to_owned(), 1)],
            "the 40h row and only it: the 3h and NULL-finish rows are newer than \
             the upper bound and the 60h row is older than the lower one, so this \
             asserts both bounds of the interval on the shipping dialect",
        );

        let failures = OrmResultsRepository
            .recent_failures(&conn, &scope(tenant), &["FAILED", "ERROR"], from, 10)
            .await
            .unwrap();
        assert_eq!(
            failures
                .iter()
                .map(|row| (row.run_id, row.status.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (Uuid::from_u128(0x81), "ERROR"),
                (Uuid::from_u128(0x80), "FAILED"),
            ],
            "the NULL-finish row is the newest by the coalesced instant, so it \
             sorts first on Postgres too: {failures:?}",
        );
    }

    /// **The flaky read runs on real Postgres, and it is the read in this gear
    /// with the most that `SQLite` cannot vouch for.**
    ///
    /// Every assertion above is on the `SQLite` tier, where it needs no Docker and
    /// where the *semantics* — which columns group, which statuses count, which
    /// group the limit drops — are dialect-independent. Five things about this
    /// statement are not, and each is a mutation the `SQLite` tier would pass:
    ///
    /// * **`test_file` is selected beside an aggregate.** Postgres **refuses** a
    ///   selected column that is neither grouped nor aggregated; `SQLite` picks an
    ///   arbitrary row and says nothing. So the `MAX(...)` dropped from
    ///   `dashboard.rs:384`'s port — or one of the three grouping keys dropped
    ///   from the `GROUP BY` — is a green `SQLite` run and a 500 in production.
    ///   This is the argument
    ///   [`the_aggregate_reads_are_accepted_by_real_postgres`] makes for the other
    ///   three reads, and it is sharper here because this projection has seven
    ///   columns and only three of them are grouping keys.
    /// * **`COUNT(CASE WHEN … THEN 1 END)` appears in the select list, in a
    ///   two-sided `HAVING` and inside the `ORDER BY`.** `HAVING` over an
    ///   aggregate expression that is not one of the output columns is where the
    ///   two dialects' tolerance differs most.
    /// * **`LEAST` is a `CASE`** ([`super::smaller_of`]), and this is the tier
    ///   where the native spelling would have worked — so it is the tier that
    ///   proves the substitution is not merely portable but *right*.
    /// * **The window is `COALESCE(run_finished_at, run_created_at)` decomposed
    ///   into two branches.** On `SQLite` a timestamp is text and the comparison
    ///   is lexicographic; on Postgres it is `timestamptz` arithmetic. The fixture
    ///   below carries a row with **no** finish instant, so the fallback branch is
    ///   exercised on the shipping dialect.
    /// * **`COUNT` is `bigint` on Postgres and `INTEGER` on `SQLite`**, and
    ///   `FlakyGroupRow` binds all three counters as `i64` by *alias*. A driver
    ///   that could not decode one of them would fail here and nowhere else.
    ///
    /// One container. The fixture is three groups that rank in a known order with
    /// a distinct `(passed, failed, total)` triple each, one group excluded by the
    /// `HAVING` and one row outside the window, so the single assertion covers the
    /// rank, the counters, the representative file, the `HAVING` and the window at
    /// once — which is what a per-test container buys and does not repeat.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn the_flaky_read_is_accepted_by_real_postgres() {
        use crate::infra::storage::test_db::pg_db;

        let harness = pg_db().await;
        let conn = harness.db.conn().unwrap();
        let tenant = Uuid::from_u128(TENANT);
        let at = now();
        let plan = "plans/pg/plan.yaml";

        // `test_mid` is the flakiest by the smaller counter (3) even though
        // `test_top` has more rows in total; `test_low`'s smaller counter is 1.
        let mut rows: Vec<FlakySeed<'_>> = Vec::new();
        rows.extend(flaky_rows("test_mid", plan, "PASSED", 3));
        rows.extend(flaky_rows("test_mid", plan, "ERROR", 5));
        rows.extend(flaky_rows("test_top", plan, "PASSED", 2));
        rows.extend(flaky_rows("test_top", plan, "FAILED", 9));
        rows.extend(flaky_rows("test_low", plan, "PASSED", 7));
        rows.extend(flaky_rows("test_low", plan, "FAILED", 1));
        // Excluded by the HAVING, and by the widest margin the fixture has.
        rows.extend(flaky_rows("test_never", plan, "PASSED", 8));
        rows.extend(flaky_rows("test_never", plan, "SKIPPED", 8));
        seed_flaky(&conn, tenant, Uuid::from_u128(0xE1), Some(at), &rows).await;

        // Two files under one group key, so `MAX` has something to pick from.
        seed_flaky(
            &conn,
            tenant,
            Uuid::from_u128(0xE2),
            Some(at),
            &[
                ("test_mid", "tests/zz.py", Some(0xA1), Some(plan), "PASSED"),
                ("test_mid", "tests/aa.py", Some(0xA1), Some(plan), "FAILED"),
            ],
        )
        .await;

        // No finish instant: admitted only through the fallback branch, and
        // ranked below everything above it.
        let unfinished = flaky_rows("test_unfinished", plan, "PASSED", 1)
            .into_iter()
            .chain(flaky_rows("test_unfinished", plan, "FAILED", 1))
            .map(|(name, file, repo, plan, status)| NewTestResult {
                test_file: file.to_owned(),
                repo_id: repo.map(Uuid::from_u128),
                plan_path: plan.map(str::to_owned),
                run_finished_at: None,
                run_created_at: Some(at - Duration::days(2)),
                ..result_row(file, name, status)
            })
            .collect();
        OrmResultsRepository
            .upsert_run_results(
                &conn,
                &scope(tenant),
                tenant,
                Uuid::from_u128(0xE3),
                unfinished,
                vec![],
            )
            .await
            .unwrap();

        // Eight days back: outside the window on the shipping dialect's own
        // timestamp arithmetic.
        let mut stale: Vec<FlakySeed<'_>> = Vec::new();
        stale.extend(flaky_rows("test_stale", plan, "PASSED", 4));
        stale.extend(flaky_rows("test_stale", plan, "FAILED", 4));
        seed_flaky(
            &conn,
            tenant,
            Uuid::from_u128(0xE4),
            Some(at - Duration::days(8)),
            &stale,
        )
        .await;

        let groups = OrmResultsRepository
            .flaky_groups(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
                10,
            )
            .await
            .unwrap();

        assert_eq!(
            groups
                .iter()
                .map(|g| (
                    g.test_name.as_str(),
                    g.test_file.as_str(),
                    g.passed,
                    g.failed,
                    g.total
                ))
                .collect::<Vec<_>>(),
            vec![
                ("test_mid", "tests/zz.py", 4, 6, 10),
                ("test_top", "tests/f.py", 2, 9, 11),
                ("test_low", "tests/f.py", 7, 1, 8),
                ("test_unfinished", "tests/f.py", 1, 1, 2),
            ],
            "ranked by the smaller counter (4, 2, 1, 1), and the 1-1 tie is \
             broken by total (8 before 2); test_never has no failure, test_stale \
             is a day outside the window, and test_mid's file is the \
             alphabetically last of the three it was reported from: {groups:?}",
        );
    }

    // -----------------------------------------------------------------------
    // `file_status_counts` — the quality-vector pass rate's SQL half
    // -----------------------------------------------------------------------

    /// One row of a `file_status_counts` fixture: `(test_name, test_file,
    /// status)`.
    ///
    /// No plan pair, unlike [`FlakySeed`]: this read groups on `test_file` alone,
    /// so varying the plan is what a test *about the grain* varies and every other
    /// fixture leaves it constant.
    type FileSeed<'a> = (&'a str, &'a str, &'a str);

    /// Seed one run whose rows carry per-row files and statuses, finished at `at`.
    async fn seed_files(
        conn: &toolkit_db::secure::DbConn<'_>,
        tenant: Uuid,
        run: Uuid,
        at: Option<OffsetDateTime>,
        rows: &[FileSeed<'_>],
    ) {
        let files = rows
            .iter()
            .map(|(name, file, status)| NewTestResult {
                test_file: (*file).to_owned(),
                run_finished_at: at,
                ..result_row(file, name, status)
            })
            .collect();
        OrmResultsRepository
            .upsert_run_results(conn, &scope(tenant), tenant, run, files, vec![])
            .await
            .unwrap();
    }

    /// Every group flattened to `(test_file, passed, failed, total)` and sorted by
    /// file — the read declares no order, so a test asserts on a set.
    fn sorted_files(counts: Vec<FileStatusCount>) -> Vec<(String, u64, u64, u64)> {
        let mut out: Vec<_> = counts
            .into_iter()
            .map(|c| (c.test_file, c.passed, c.failed, c.total))
            .collect();
        out.sort();
        out
    }

    /// **The grain is `test_file`**, and it is not the flaky read's.
    ///
    /// Legacy groups this one by `tr.test_file` alone (`dashboard.rs:492`) where
    /// the flaky read groups by `tr.test_name, rr.plan_id` (`:392`). Two different
    /// test *names* reported against the same file are therefore **one** group
    /// here and two there — which is the whole point: a quality vector is a
    /// property of a file, so the fold that divides by this denominator has to see
    /// the file's rows together.
    ///
    /// Seeded so that grouping by `test_name` instead gives `(1,1,2)` and
    /// `(2,1,3)` rather than the single `(3,2,5)` this asserts.
    #[tokio::test]
    async fn the_counts_are_grouped_by_file_and_not_by_test_name() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xF11E);
        let at = OffsetDateTime::now_utc() - Duration::days(1);

        seed_files(
            &conn,
            tenant,
            Uuid::from_u128(0x1),
            Some(at),
            &[
                ("test_one", "tests/shared.py", "PASSED"),
                ("test_one", "tests/shared.py", "FAILED"),
                ("test_two", "tests/shared.py", "PASSED"),
                ("test_two", "tests/shared.py", "PASSED"),
                ("test_two", "tests/shared.py", "ERROR"),
                ("test_solo", "tests/other.py", "PASSED"),
            ],
        )
        .await;

        let counts = OrmResultsRepository
            .file_status_counts(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
            )
            .await
            .unwrap();

        assert_eq!(
            sorted_files(counts),
            vec![
                ("tests/other.py".to_owned(), 1, 0, 1),
                ("tests/shared.py".to_owned(), 3, 2, 5),
            ],
            "two test names against one file are one group, and `ERROR` is a \
             failure",
        );
    }

    /// **A file whose rows all passed is present with `failed == 0`.**
    ///
    /// This read has no `HAVING`, unlike `flaky_groups`, and the difference is
    /// visible rather than only cheaper: the quality-vector fold divides by
    /// `total`, so a vector whose files all pass must render as a full bar. A
    /// `HAVING` copied across from the flaky read would drop it and the bar would
    /// silently disappear at 100%.
    ///
    /// The mirror case is asserted too — all-failed, which a one-sided `HAVING`
    /// would keep.
    #[tokio::test]
    async fn a_file_with_no_failure_is_present_rather_than_filtered_out() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xF11F);
        let at = OffsetDateTime::now_utc() - Duration::days(1);

        seed_files(
            &conn,
            tenant,
            Uuid::from_u128(0x2),
            Some(at),
            &[
                ("test_green", "tests/green.py", "PASSED"),
                ("test_green", "tests/green.py", "PASSED"),
                ("test_red", "tests/red.py", "FAILED"),
            ],
        )
        .await;

        let counts = OrmResultsRepository
            .file_status_counts(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
            )
            .await
            .unwrap();

        assert_eq!(
            sorted_files(counts),
            vec![
                ("tests/green.py".to_owned(), 2, 0, 2),
                ("tests/red.py".to_owned(), 0, 1, 1),
            ],
        );
    }

    /// **The denominator is `PASSED`+`FAILED`+`ERROR` and nothing else** — ruling
    /// R5's sixth classification (`dashboard.rs:487`).
    ///
    /// A `SKIPPED` row and an unknown status are in **no** counter, the total
    /// included, so a file that ran once and skipped four times reports
    /// `(1, 0, 1)` rather than `(1, 0, 5)`. Reusing `classify`'s total — every row
    /// — would deflate every quality-vector pass rate by however many tests a plan
    /// skips, on a bar that renders either number without complaint.
    ///
    /// The three counters are deliberately unequal (`2, 3, 5`) so that any pair of
    /// them transposed on the way out of SQL fails here.
    #[tokio::test]
    async fn the_denominator_counts_only_passed_failed_and_error() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xF120);
        let at = OffsetDateTime::now_utc() - Duration::days(1);

        seed_files(
            &conn,
            tenant,
            Uuid::from_u128(0x3),
            Some(at),
            &[
                ("t1", "tests/mixed.py", "PASSED"),
                ("t2", "tests/mixed.py", "PASSED"),
                ("t3", "tests/mixed.py", "FAILED"),
                ("t4", "tests/mixed.py", "FAILED"),
                ("t5", "tests/mixed.py", "ERROR"),
                ("t6", "tests/mixed.py", "SKIPPED"),
                ("t7", "tests/mixed.py", "SKIPPED"),
                ("t8", "tests/mixed.py", "XFAIL"),
                ("t9", "tests/mixed.py", "RUNNING"),
            ],
        )
        .await;

        let counts = OrmResultsRepository
            .file_status_counts(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
            )
            .await
            .unwrap();

        assert_eq!(
            sorted_files(counts),
            vec![("tests/mixed.py".to_owned(), 2, 3, 5)],
            "SKIPPED, XFAIL and RUNNING are in no counter, the total included",
        );
    }

    /// A row with **no file** forms no group — legacy's
    /// `tr.test_file IS NOT NULL` (`dashboard.rs:491`), which is `<> ''` against a
    /// `NOT NULL DEFAULT ''` column.
    ///
    /// Without the predicate there is one extra group keyed on `''`. The fold
    /// would discard it for having no quality vectors, so nothing *rendered*
    /// changes — which is exactly why the predicate is written in SQL and asserted
    /// here rather than left to the fold: the two are independent, and a later fold
    /// that stopped discarding unknown files would start counting a phantom file.
    #[tokio::test]
    async fn a_row_with_no_file_forms_no_group() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xF121);
        let at = OffsetDateTime::now_utc() - Duration::days(1);

        seed_files(
            &conn,
            tenant,
            Uuid::from_u128(0x4),
            Some(at),
            &[
                ("named", "tests/named.py", "PASSED"),
                ("orphan", "", "PASSED"),
                ("orphan2", "", "FAILED"),
            ],
        )
        .await;

        let counts = OrmResultsRepository
            .file_status_counts(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
            )
            .await
            .unwrap();

        assert_eq!(
            sorted_files(counts),
            vec![("tests/named.py".to_owned(), 1, 0, 1)],
        );
    }

    /// The window is the caller's `since` on `COALESCE(finished_at, created_at)`,
    /// **with no phase restriction** — the third row-inclusion rule
    /// (`dashboard.rs:490`).
    ///
    /// Three files: one inside the window, one a day outside it, and one whose run
    /// has **no finish instant at all** and is therefore admitted on
    /// `run_created_at`. That last one is what separates this rule from the daily
    /// trend's: `window_start` would drop it, and a run still in progress
    /// contributes here.
    #[tokio::test]
    async fn the_window_admits_unfinished_runs_and_excludes_stale_ones() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xF122);
        let now = OffsetDateTime::now_utc();

        seed_files(
            &conn,
            tenant,
            Uuid::from_u128(0x5),
            Some(now - Duration::days(1)),
            &[("fresh", "tests/fresh.py", "PASSED")],
        )
        .await;
        seed_files(
            &conn,
            tenant,
            Uuid::from_u128(0x6),
            Some(now - Duration::days(8)),
            &[("stale", "tests/stale.py", "PASSED")],
        )
        .await;
        seed_unfinished_run(
            &conn,
            tenant,
            Uuid::from_u128(0x7),
            now - Duration::days(2),
            &["PASSED"],
        )
        .await;

        let counts = OrmResultsRepository
            .file_status_counts(
                &conn,
                &scope(tenant),
                &PASSED,
                &FAILED,
                now - Duration::days(7),
            )
            .await
            .unwrap();

        let files: Vec<String> = sorted_files(counts).into_iter().map(|c| c.0).collect();
        assert!(
            files.contains(&"tests/fresh.py".to_owned()),
            "the finished run inside the window is counted: {files:?}",
        );
        assert!(
            !files.contains(&"tests/stale.py".to_owned()),
            "a run finished eight days ago is outside a seven-day window: {files:?}",
        );
        assert_eq!(
            files.len(),
            2,
            "the unfinished run's rows are admitted on run_created_at, which is \
             what makes this the third row-inclusion rule rather than the daily \
             trend's: {files:?}",
        );
    }

    /// Another tenant's rows are invisible, because `project_all` keeps the scope
    /// predicate on an aggregate.
    ///
    /// The same property `flaky_groups`' scope test pins, asserted separately
    /// because this read is a second `project_all` call site and `into_inner()`
    /// would have compiled.
    #[tokio::test]
    async fn the_counts_are_scoped_to_the_callers_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xF123);
        let theirs = Uuid::from_u128(0xF124);
        let at = OffsetDateTime::now_utc() - Duration::days(1);

        seed_files(
            &conn,
            mine,
            Uuid::from_u128(0x8),
            Some(at),
            &[("t", "tests/mine.py", "PASSED")],
        )
        .await;
        seed_files(
            &conn,
            theirs,
            Uuid::from_u128(0x9),
            Some(at),
            &[("t", "tests/theirs.py", "PASSED")],
        )
        .await;

        let counts = OrmResultsRepository
            .file_status_counts(
                &conn,
                &scope(mine),
                &PASSED,
                &FAILED,
                at - Duration::days(7),
            )
            .await
            .unwrap();

        assert_eq!(
            sorted_files(counts),
            vec![("tests/mine.py".to_owned(), 1, 0, 1)],
        );
    }

    /// Either status set empty answers empty, and here that guard is **not**
    /// cosmetic.
    ///
    /// `flaky_groups`' equivalent is measured to be unobservable — `SeaQuery`
    /// renders an empty `IN` as `1 = 2`, so its two-sided `HAVING` rejects every
    /// group anyway. This read has no `HAVING`, so without the guard every file
    /// would come back as `(0, 0, 0)` and the fold's `passed / total` would divide
    /// by zero. The trait's doc records the difference.
    #[tokio::test]
    async fn an_empty_status_set_answers_empty_rather_than_all_zeroes() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xF125);
        let at = OffsetDateTime::now_utc() - Duration::days(1);

        seed_files(
            &conn,
            tenant,
            Uuid::from_u128(0xA),
            Some(at),
            &[("t", "tests/a.py", "PASSED")],
        )
        .await;
        let since = at - Duration::days(7);

        for (passed, failed) in [
            (&[][..], &FAILED[..]),
            (&PASSED[..], &[][..]),
            (&[][..], &[][..]),
        ] {
            assert!(
                OrmResultsRepository
                    .file_status_counts(&conn, &scope(tenant), passed, failed, since)
                    .await
                    .unwrap()
                    .is_empty(),
            );
        }
    }
}
