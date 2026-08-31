//! `SecureORM` implementation of [`RunsRepository`].
//!
//! Every chain here is **filter-first**: `.filter(..)` before
//! `.secure().scope_with(scope)`, matching both shipped sibling gears. The
//! order is not cosmetic — `scope_with` is what appends the tenant predicate,
//! and a chain that scoped first and filtered afterwards reads as though the
//! filter could replace the scope.

use async_trait::async_trait;
use qa_runs_sdk::{Run, RunResult, RunState};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, Condition, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::odata::sea_orm_filter::{PaginateOdataTryError, paginate_odata_try};
use toolkit_db::secure::{
    DBRunner, Scoped, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{
    MAX_TIMEOUT_SWEEP_SCAN, MAX_WATCH_SCAN, NewRun, NewTestResult, OwnedRunId, RunResultDelta,
    RunStatePatch, RunWithResult, RunsRepository, TestResultRow, TimeoutCandidate, WatchCandidate,
    Windowed, overread, window_size,
};
use crate::infra::storage::db::{PAGE_LIMITS, db_err, odata_err};
use crate::infra::storage::entity::run::{
    ActiveModel as RunAM, Column as RunColumn, Entity as RunEntity,
};
use crate::infra::storage::entity::run_test_result::{
    ActiveModel as ResultAM, Column as ResultColumn, Entity as ResultEntity,
};
use crate::infra::storage::mapper::{
    db_i32_from_i64, json_to_column, normalize_test_status, parameters_to_column,
    run_result_from_row, run_to_sdk, run_with_result_from_row, target_to_columns,
};
use crate::infra::storage::odata::{RunFilterField, RunODataMapper};

/// ORM-based implementation of the `RunsRepository` trait.
#[derive(Clone, Default)]
pub struct OrmRunsRepository;

/// `WHERE id = $1`, as a condition the scoped chains can prepend.
fn by_id(id: Uuid) -> Condition {
    Condition::all().add(Expr::col(RunColumn::Id).eq(id))
}

/// Every read on this repository that returns **more than one row**, as a
/// named builder rather than a chain inlined at its call site.
///
/// # Why they are functions at all
///
/// `tests::no_list_query_reaches_the_log_table` renders each of them and
/// asserts the SQL does not mention `qa_run_logs` — legacy's OOM, made
/// unrepeatable (D-RLP-7). That guard is only worth anything if it renders
/// **the same builder** the production path runs: a hand-copied lookalike in
/// the test module would keep passing while a join arrived in the real query.
/// So the extraction is the mechanism, not tidiness.
///
/// **All five are covered, not just `list`.** The first revision of that guard
/// covered `list_query` alone, which is the *narrower* of the two collection
/// reads — the `OData` paginated list is the full-history query shape that
/// actually `OOMKilled` legacy, and the three sweep windows each allocate whole
/// `Run`s too. A join reaching bulk log text through any of them is the same
/// incident.
///
/// The scoped, ordered query [`RunsRepository::list`] runs.
fn list_query(scope: &AccessScope) -> toolkit_db::secure::SecureSelect<RunEntity, Scoped> {
    RunEntity::find()
        .secure()
        .scope_with(scope)
        .order_by(RunColumn::CreatedAt, sea_orm::Order::Desc)
}

/// The scoped select [`RunsRepository::list_page`] hands to `paginate_odata_try`.
///
/// **This is the query shape the OOM lesson is actually about**: an unbounded
/// `$filter` over the full run history, one page at a time. `paginate_odata_try`
/// adds the caller's filter, the sort and the window on top of this select; it
/// cannot add a join to another table, so this is the whole surface a join
/// could arrive on.
fn odata_list_query(scope: &AccessScope) -> toolkit_db::secure::SecureSelect<RunEntity, Scoped> {
    RunEntity::find().secure().scope_with(scope)
}

/// The window [`RunsRepository::list_finished_since`] reads.
///
/// Takes its `since`/`limit` so the guard renders the real predicate set rather
/// than a stripped one — a join is usually added beside a filter, not instead
/// of it.
fn finished_since_query(
    scope: &AccessScope,
    since: OffsetDateTime,
    limit: u32,
) -> toolkit_db::secure::SecureSelect<RunEntity, Scoped> {
    RunEntity::find()
        .filter(
            Condition::all()
                .add(Expr::col(RunColumn::FinishedAt).gte(since))
                // Redundant against the comparison above under SQL's
                // three-valued logic, and kept anyway: the trait contract
                // says "not yet terminal is never returned", and a reader
                // checking that claim against this query should be able to
                // read it rather than derive it. It costs a planner-folded
                // conjunct.
                .add(Expr::col(RunColumn::FinishedAt).is_not_null()),
        )
        .secure()
        .scope_with(scope)
        // Ascending, and `id` after it. The caller advances a watermark as
        // it consumes this page: descending would strand the oldest gap
        // forever, and an unbroken tie between two runs that finished in
        // the same tick would let the watermark step over whichever one the
        // plan put second. See the trait doc - both halves are contract.
        .order_by(RunColumn::FinishedAt, sea_orm::Order::Asc)
        .order_by(RunColumn::Id, sea_orm::Order::Asc)
        .limit(sweep_limit(limit))
}

/// The window [`RunsRepository::list_timeout_candidates`] reads.
fn timeout_candidates_query(
    scope: &AccessScope,
    now: OffsetDateTime,
    after: Option<Uuid>,
) -> toolkit_db::secure::SecureSelect<RunEntity, Scoped> {
    let mut filter = Condition::all()
        .add(active_states())
        // A NULL `timeout_at` compares NULL and is excluded, which is the
        // wanted reading: a run with no deadline never times out.
        .add(Expr::col(RunColumn::TimeoutAt).lt(now));
    if let Some(after) = after {
        filter = filter.add(Expr::col(RunColumn::Id).gt(after));
    }
    RunEntity::find()
        .filter(filter)
        .secure()
        .scope_with(scope)
        // `id`, not `timeout_at`: a run whose cancel keeps failing stays a
        // candidate forever and would hold the head of an age ordering. See
        // the trait doc.
        .order_by(RunColumn::Id, sea_orm::Order::Asc)
        // One row past the window, so `Windowed::truncated` is exact rather
        // than "the answer happened to be exactly the window size".
        .limit(overread(MAX_TIMEOUT_SWEEP_SCAN))
}

/// The window [`RunsRepository::list_watch_candidates`] reads.
fn watch_candidates_query(
    scope: &AccessScope,
    after: Option<Uuid>,
) -> toolkit_db::secure::SecureSelect<RunEntity, Scoped> {
    let mut filter = Condition::all()
        .add(active_states())
        // `IS NOT NULL`, not `<> ''`: the column is nullable and
        // `set_execution_ref` is its only writer, so NULL is exactly "the
        // submit has not returned a handle yet" - a row boot recovery may
        // fail as orphaned, and one there is nothing to watch.
        .add(Expr::col(RunColumn::ExecutionRef).is_not_null());
    if let Some(after) = after {
        filter = filter.add(Expr::col(RunColumn::Id).gt(after));
    }
    RunEntity::find()
        .filter(filter)
        .secure()
        .scope_with(scope)
        // `id`, not an age column: a healthy live run stays a candidate for
        // its whole execution, so any stable ordering starves everything
        // behind a full window of them. See the trait doc.
        .order_by(RunColumn::Id, sea_orm::Order::Asc)
        // One row past the window, so `Windowed::truncated` is exact rather
        // than "the answer happened to be exactly the window size".
        .limit(overread(MAX_WATCH_SCAN))
}

/// The reconciler sweep's window, clamped.
///
/// `PAGE_LIMITS.max` rather than a constant of its own: this read allocates
/// whole `Run`s exactly as `list_page` does, so there is no argument for it
/// tolerating a larger window than the collection endpoint, and a second
/// number would be a second thing to keep in step. A `limit` of 0 is passed
/// through as 0 - "give me nothing" is a coherent request and clamping it up
/// to a floor of 1 would hand back a run the caller did not ask for, which for
/// a watermark consumer is worse than an empty page.
fn sweep_limit(requested: u32) -> u64 {
    u64::from(requested).min(PAGE_LIMITS.max)
}

/// `col = CASE WHEN col + $n < 0 THEN 0 ELSE col + $n END`.
///
/// The floor is load-bearing, not defensive tidiness. Without it a delta such
/// as `passed: -5` — which nothing rejects, since deltas are legitimately
/// signed — drives a counter negative and **manufactures the exact state the
/// reader classifies as corruption**: `usize_from_db` then fails closed on
/// every subsequent read, so the run's result projection returns
/// `CorruptState` forever while the run itself still reads fine. A supported
/// write must not be able to produce a permanently unreadable row. It also
/// reaches correctness, not just availability:
/// `domain::state_machine::derive_terminal_state` decides on `failed > 0`
/// (`skipped` no longer votes on the verdict — product owner decision,
/// 2026-08-28), so a stray delta on `failed` changes a run's verdict.
///
/// `CASE` rather than `GREATEST`: `SQLite` spells that one `MAX`, and this
/// expression is built once for all three dialects.
///
/// Clamping rather than rejecting, deliberately. Rejecting needs the current
/// value, and reading it first is precisely the read-modify-write this method
/// exists to avoid — two concurrent result events would both read, both
/// decide, and one would be lost. The clamp keeps the whole operation one
/// atomic statement. **The unclamped direction is still open**: each delta is
/// bounded by `db_i32_from_i64`, but the accumulated sum is not, so a counter
/// could in principle overflow `INTEGER` after ~2^31 results. No run has that
/// many, and clamping the top would silently freeze a counter instead, which
/// is the worse failure. Recorded rather than fixed.
fn clamped_increment(column: RunColumn, delta: i32) -> sea_orm::sea_query::SimpleExpr {
    let sum = Expr::col(column).add(delta);
    Expr::case(Expr::expr(sum.clone()).lt(0), Expr::value(0))
        .finally(sum)
        .into()
}

/// The two states that hold an execution: `dispatching` and `running`.
/// Legacy's `CLAIM_STATES` (`manager/src/services/run_queue.rs:104`) names the
/// same pair on the queue row.
fn active_states() -> Condition {
    Condition::any()
        .add(Expr::col(RunColumn::State).eq(RunState::Dispatching.as_str()))
        .add(Expr::col(RunColumn::State).eq(RunState::Running.as_str()))
}

/// Build the insert model for a run.
///
/// Every column this function decides on its own — rather than copying from
/// its argument — is a column [`NewRun`] deliberately does not have, so the
/// two cannot disagree. `id` is the one that mattered: `qa_runs.id` is a
/// *global* `PRIMARY KEY` (`migrations/m20260813_000003_initial.rs`), the one
/// index in this schema that cannot be tenant-prefixed, so honouring a
/// caller-supplied id made `create` a working cross-tenant existence oracle —
/// a probe carrying a victim's run id collided on the primary key, which is a
/// unique violation, which answered `RunNameExists`, while an unused id
/// answered `Ok`. Reproduced end to end. The migration already states the
/// principle: *"the oracle was never a property of the row — it is a property
/// of the response."*
///
/// The five counters start at zero, which is what `DEFAULT 0` would have
/// produced — spelled explicitly because `SeaORM` names every column in its
/// `INSERT`, so there is no "leave it to the default" option here.
fn run_active_model(tenant_id: Uuid, new: &NewRun) -> Result<RunAM, DomainError> {
    let columns = target_to_columns(&new.target);
    let now = OffsetDateTime::now_utc();

    Ok(RunAM {
        id: ActiveValue::Set(Uuid::new_v4()),
        tenant_id: ActiveValue::Set(tenant_id),
        name: ActiveValue::Set(new.name.clone()),
        run_kind: ActiveValue::Set(new.target.kind().as_str().to_owned()),
        target_repo_id: ActiveValue::Set(columns.repo_id),
        target_path: ActiveValue::Set(columns.path),
        target_test_file: ActiveValue::Set(columns.test_file),
        target_custom_plan_id: ActiveValue::Set(columns.custom_plan_id),
        target_collect_url: ActiveValue::Set(columns.collect_url),
        platform_id: ActiveValue::Set(new.platform_id),
        test_version: ActiveValue::Set(new.test_version.clone()),
        app_version: ActiveValue::Set(new.app_version.clone()),
        app_build: ActiveValue::Set(new.app_build.clone()),
        state: ActiveValue::Set(new.state.as_str().to_owned()),
        resolved_exclusive: ActiveValue::Set(new.resolved_exclusive),
        exclusive_tier: ActiveValue::Set(new.exclusive_tier.as_str().to_owned()),
        is_validation: ActiveValue::Set(new.is_validation),
        parameters: ActiveValue::Set(parameters_to_column(&new.parameters)?),
        include_tags: ActiveValue::Set(json_to_column("run.include_tags", &new.include_tags)?),
        exclude_tags: ActiveValue::Set(json_to_column("run.exclude_tags", &new.exclude_tags)?),
        source: ActiveValue::Set(new.source.as_str().to_owned()),
        schedule_id: ActiveValue::Set(new.schedule_id),
        bundle_ids: ActiveValue::Set(json_to_column("run.bundle_ids", &new.bundle_ids)?),
        // Each of these has exactly one writer, and it is not this one -- see
        // `NewRun`'s table.
        execution_ref: ActiveValue::Set(None),
        log_storage_ref: ActiveValue::Set(None),
        timeout_at: ActiveValue::Set(new.timeout_at),
        started_at: ActiveValue::Set(None),
        finished_at: ActiveValue::Set(None),
        error: ActiveValue::Set(None),
        passed: ActiveValue::Set(0),
        failed: ActiveValue::Set(0),
        skipped: ActiveValue::Set(0),
        in_progress: ActiveValue::Set(0),
        total: ActiveValue::Set(0),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
    })
}

#[async_trait]
impl RunsRepository for OrmRunsRepository {
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewRun,
    ) -> Result<Run, DomainError> {
        // Kept for the unique-violation error, which reports the taken name.
        let name = new.name.clone();
        let am = run_active_model(tenant_id, &new)?;

        // Return the INSERTED model rather than the argument: the two agree
        // today, but only the former would observe a database-side default.
        match secure_insert::<RunEntity>(am, scope, runner).await {
            Ok(model) => run_to_sdk(model),
            // **Two** constraints can raise a unique violation on this table,
            // and only one of them is safe to report:
            //
            // * `idx_qa_runs_tenant_name` is tenant-prefixed, so a collision
            //   there is always with a row this tenant can itself see. That is
            //   the collision `RunNameExists` describes.
            // * `qa_runs.id` is a **global** primary key. A collision there
            //   would leak the existence of another tenant's run — and would
            //   report it under a run *name* that is not even taken.
            //
            // The second is unreachable because `run_active_model` mints the
            // id (see its docs); this branch is safe only for as long as that
            // stays true. An earlier version of this comment claimed the
            // tenant-prefixed index was the only way in, which was false and
            // was a High finding.
            Err(e) if e.is_unique_violation() => Err(DomainError::RunNameExists { name }),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Run>, DomainError> {
        let found = RunEntity::find()
            .filter(by_id(id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        found.map(run_to_sdk).transpose()
    }

    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Run>, DomainError> {
        let rows = list_query(scope).all(runner).await.map_err(db_err)?;
        rows.into_iter().map(run_to_sdk).collect()
    }

    async fn list_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<RunWithResult>, DomainError> {
        // `.secure().scope_with(scope)` before `paginate_odata`, and not
        // optionally: that function's first parameter is
        // `SecureSelect<E, Scoped>`, so an unscoped select does not type-check
        // and the caller's `$filter` is applied on top of the tenant predicate
        // rather than in place of it.
        //
        // Built by [`odata_list_query`] rather than inline, so
        // `tests::no_list_query_reaches_the_log_table` renders this select and
        // not a lookalike. See that function's doc.
        let scoped = odata_list_query(scope);

        // `paginate_odata_try`, not `paginate_odata`: `run_with_result_from_row`
        // is fallible - a row whose `state`, `target` or count columns do not
        // decode is `CorruptState`, and the infallible variant would need this
        // mapper to panic on one.
        paginate_odata_try::<RunFilterField, RunODataMapper, _, _, _, DomainError, _>(
            scoped,
            runner,
            query,
            // Newest first, `id` breaking ties. `created_at` rather than
            // `updated_at`: a run's `updated_at` moves on every state change,
            // so paging by it would let a run the caller has already seen jump
            // back into a later page when it finishes.
            ("created_at", SortDir::Desc),
            PAGE_LIMITS,
            run_with_result_from_row,
        )
        .await
        .map_err(|error| match error {
            PaginateOdataTryError::OData(e) => odata_err(&e),
            PaginateOdataTryError::MapError(e) => e,
        })
    }

    async fn list_finished_since<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        since: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<Run>, DomainError> {
        let rows = finished_since_query(scope, since, limit)
            .all(runner)
            .await
            .map_err(db_err)?;
        rows.into_iter().map(run_to_sdk).collect()
    }

    async fn update_state<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        from: RunState,
        to: RunState,
        patch: RunStatePatch,
    ) -> Result<bool, DomainError> {
        let mut update = RunEntity::update_many()
            // The compare-and-set. Both predicates are in the same statement
            // as the writes, so a concurrent transition either loses this
            // update entirely or wins outright; there is no window in which
            // the state and the patch disagree.
            .filter(
                Condition::all()
                    .add(Expr::col(RunColumn::Id).eq(id))
                    .add(Expr::col(RunColumn::State).eq(from.as_str())),
            )
            .secure()
            .scope_with(scope)
            .col_expr(RunColumn::State, Expr::value(to.as_str()))
            .col_expr(RunColumn::UpdatedAt, Expr::value(OffsetDateTime::now_utc()));

        // `None` leaves the column alone. Writing all three unconditionally
        // would clear `started_at` on every completion.
        if let Some(started_at) = patch.started_at {
            update = update.col_expr(RunColumn::StartedAt, Expr::value(started_at));
        }
        if let Some(finished_at) = patch.finished_at {
            update = update.col_expr(RunColumn::FinishedAt, Expr::value(finished_at));
        }
        if let Some(error) = patch.error {
            update = update.col_expr(RunColumn::Error, Expr::value(error));
        }

        let result = update.exec(runner).await.map_err(db_err)?;
        Ok(result.rows_affected == 1)
    }

    async fn set_execution_ref<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        execution_ref: &str,
    ) -> Result<bool, DomainError> {
        let result = RunEntity::update_many()
            .filter(by_id(id))
            .secure()
            .scope_with(scope)
            .col_expr(RunColumn::ExecutionRef, Expr::value(execution_ref))
            .col_expr(RunColumn::UpdatedAt, Expr::value(OffsetDateTime::now_utc()))
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected == 1)
    }

    /// `bundle_ids = $2`, with the same shape as
    /// [`RunsRepository::set_execution_ref`] and for the same reason it is
    /// unguarded — see that method and `set_bundle_ids`' own trait doc.
    ///
    /// The JSON encoding goes through the same `json_to_column` the insert model
    /// uses, so the column has exactly one representation regardless of which
    /// writer produced it.
    async fn set_bundle_ids<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        bundle_ids: &[Uuid],
    ) -> Result<bool, DomainError> {
        let encoded = json_to_column("run.bundle_ids", &bundle_ids.to_vec())?;
        let result = RunEntity::update_many()
            .filter(by_id(id))
            .secure()
            .scope_with(scope)
            .col_expr(RunColumn::BundleIds, Expr::value(encoded))
            .col_expr(RunColumn::UpdatedAt, Expr::value(OffsetDateTime::now_utc()))
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected == 1)
    }

    async fn add_result_counts<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        delta: RunResultDelta,
    ) -> Result<bool, DomainError> {
        // The `i64`/`INTEGER` narrowing happens here, checked: an out-of-range
        // delta is a validation failure rather than a wrapped write.
        let passed = db_i32_from_i64(delta.passed, "passed")?;
        let failed = db_i32_from_i64(delta.failed, "failed")?;
        let skipped = db_i32_from_i64(delta.skipped, "skipped")?;
        let in_progress = db_i32_from_i64(delta.in_progress, "in_progress")?;
        let total = db_i32_from_i64(delta.total, "total")?;

        // `passed = passed + $n`, evaluated by the database. A
        // read-modify-write in this process would lose one of two concurrent
        // result events.
        let result = RunEntity::update_many()
            .filter(by_id(id))
            .secure()
            .scope_with(scope)
            .col_expr(
                RunColumn::Passed,
                clamped_increment(RunColumn::Passed, passed),
            )
            .col_expr(
                RunColumn::Failed,
                clamped_increment(RunColumn::Failed, failed),
            )
            .col_expr(
                RunColumn::Skipped,
                clamped_increment(RunColumn::Skipped, skipped),
            )
            .col_expr(
                RunColumn::InProgress,
                clamped_increment(RunColumn::InProgress, in_progress),
            )
            .col_expr(RunColumn::Total, clamped_increment(RunColumn::Total, total))
            .col_expr(RunColumn::UpdatedAt, Expr::value(OffsetDateTime::now_utc()))
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected == 1)
    }

    async fn get_result<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<RunResult>, DomainError> {
        let found = RunEntity::find()
            .filter(by_id(id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        found.as_ref().map(run_result_from_row).transpose()
    }

    async fn list_timeout_candidates<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        now: OffsetDateTime,
        after: Option<Uuid>,
    ) -> Result<Windowed<TimeoutCandidate>, DomainError> {
        let rows = timeout_candidates_query(scope, now, after)
            .all(runner)
            .await
            .map_err(db_err)?;

        Ok(Windowed::from_overread(
            rows.into_iter()
                .map(|m| TimeoutCandidate {
                    run_id: m.id,
                    tenant_id: m.tenant_id,
                })
                .collect(),
            window_size(MAX_TIMEOUT_SWEEP_SCAN),
        ))
    }

    async fn list_watch_candidates<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        after: Option<Uuid>,
    ) -> Result<Windowed<WatchCandidate>, DomainError> {
        let rows = watch_candidates_query(scope, after)
            .all(runner)
            .await
            .map_err(db_err)?;

        Ok(Windowed::from_overread(
            rows.into_iter()
                .map(|m| WatchCandidate {
                    run_id: m.id,
                    tenant_id: m.tenant_id,
                })
                .collect(),
            window_size(MAX_WATCH_SCAN),
        ))
    }

    async fn upsert_test_result<C: DBRunner>(
        &self,
        runner: &C,
        results_scope: &AccessScope,
        tenant_id: Uuid,
        run: OwnedRunId,
        result: NewTestResult,
    ) -> Result<TestResultRow, DomainError> {
        let now = OffsetDateTime::now_utc();

        // Delete-then-insert on `(run_id, test_file, test_name)`, matching
        // `manager/src/routes/runs.rs:1153-1185`. The tuple carries no unique
        // index, so this is an application invariant; pass a transaction
        // runner to make the pair atomic.
        ResultEntity::delete_many()
            .filter(
                Condition::all()
                    .add(Expr::col(ResultColumn::RunId).eq(run.get()))
                    .add(Expr::col(ResultColumn::TestName).eq(result.test_name.as_str()))
                    // Plain equality, not legacy's `COALESCE(test_file, '')`:
                    // the column is `NOT NULL DEFAULT ''`, so "absent" has a
                    // single spelling.
                    .add(Expr::col(ResultColumn::TestFile).eq(result.test_file.as_str())),
            )
            .secure()
            .scope_with(results_scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        let am = ResultAM {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            run_id: ActiveValue::Set(run.get()),
            test_file: ActiveValue::Set(result.test_file),
            test_name: ActiveValue::Set(result.test_name),
            // The vocabulary is an open set (see `NewTestResult::status`), so
            // the only thing done to it is making it fit the column — which is
            // what stops `VARCHAR(16)` re-closing the set on Postgres by
            // turning a long status into a 500 and a dropped result.
            status: ActiveValue::Set(normalize_test_status(result.status)),
            duration: ActiveValue::Set(result.duration),
            launch_id: ActiveValue::Set(result.launch_id),
            jira_key: ActiveValue::Set(result.jira_key),
            // The three case-fidelity columns
            // (`m20260818_000005_case_fidelity`). Named exhaustively rather
            // than `..Default::default()`, which does compile here —
            // `sea-orm-macros` derives `Default` for every `ActiveModel`. A
            // wildcard would absorb any column a later migration adds *without
            // a compile error*, silently writing its default through this path;
            // an exhaustive literal makes the next column stop the build here,
            // which is where the decision belongs.
            //
            // These three lines were `String::new()`/`None`/`None` until the
            // fields existed on `NewTestResult` to read. **Adding them produced
            // no error at this site** — the literal kept compiling and kept
            // writing `''`/`NULL`/`NULL`, dropping every case-level nodeid the
            // executor sent, with nothing failing and no test going red on its
            // own: the column would simply have stayed empty forever while
            // qa-insights reported per-case numbers that silently degraded to
            // per-file ones. No type catches that. What catches it is
            // `a_case_level_nodeid_reason_and_ticket_are_actually_written`
            // below, which break-verifies against exactly this hazard by
            // restoring the literals; it is the only control there is, so it
            // must not be weakened into a service-level assertion, which passes
            // either way.
            //
            // `unwrap_or_default()` on `nodeid` alone, at the column boundary
            // and nowhere earlier: the column is `NOT NULL DEFAULT ''`, so
            // `None` and `Some("")` have one spelling here, while `reason` and
            // `ticket` are genuinely nullable and keep theirs. The source
            // system collapses it at the same single point
            // (`manager/src/services/argo.rs:2973`).
            //
            // A non-empty `nodeid` is what marks this row case-level rather
            // than file-level — the distinction legacy got from having two
            // tables. `TestObservation::nodeid` states the convention in full,
            // including that no constraint in this schema enforces it.
            nodeid: ActiveValue::Set(result.nodeid.unwrap_or_default()),
            reason: ActiveValue::Set(result.reason),
            ticket: ActiveValue::Set(result.ticket),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        let model = secure_insert::<ResultEntity>(am, results_scope, runner)
            .await
            .map_err(db_err)?;
        Ok(crate::infra::storage::mapper::test_result_to_row(model))
    }

    async fn list_test_results<C: DBRunner>(
        &self,
        runner: &C,
        results_scope: &AccessScope,
        run: OwnedRunId,
    ) -> Result<Vec<TestResultRow>, DomainError> {
        let rows = ResultEntity::find()
            .filter(Condition::all().add(Expr::col(ResultColumn::RunId).eq(run.get())))
            .secure()
            .scope_with(results_scope)
            .order_by(ResultColumn::TestFile, sea_orm::Order::Asc)
            .order_by(ResultColumn::TestName, sea_orm::Order::Asc)
            .all(runner)
            .await
            .map_err(db_err)?;

        Ok(rows
            .into_iter()
            .map(crate::infra::storage::mapper::test_result_to_row)
            .collect())
    }
}

/// DB-backed tests: the only thing that actually exercises these queries.
///
/// `cargo build` checks none of it — the table name, every column name, every
/// `WHERE` and every `SET` in this file are runtime strings. Each test below
/// runs against the real migration on an in-memory `SQLite` database, through
/// the real `SecureORM` scoping.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use qa_runs_sdk::{RunSource, RunTarget};

    use super::*;
    use crate::infra::storage::test_db::{inmem_db, now, sample_new_run, scope};

    /// Every field of a fully-populated run survives the round trip —
    /// including all three JSON columns, all four enum columns and the
    /// flattened target.
    #[tokio::test]
    async fn a_run_round_trips_through_the_database() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = sample_new_run("smoke-1");

        let created = repo
            .create(&conn, &scope(tenant), tenant, run.clone())
            .await
            .unwrap();

        // Spelled out field by field rather than with `..`: this is the one
        // test that pins the whole `NewRun` -> row -> `Run` mapping, and a
        // struct-update would let a transposed pair of same-typed fields
        // through.
        let expected = Run {
            id: created.id,
            name: run.name.clone(),
            target: run.target.clone(),
            platform_id: run.platform_id,
            test_version: run.test_version.clone(),
            app_version: run.app_version.clone(),
            app_build: run.app_build.clone(),
            state: run.state,
            resolved_exclusive: run.resolved_exclusive,
            exclusive_tier: run.exclusive_tier,
            is_validation: run.is_validation,
            parameters: run.parameters.clone(),
            include_tags: run.include_tags.clone(),
            exclude_tags: run.exclude_tags.clone(),
            source: run.source,
            schedule_id: run.schedule_id,
            bundle_ids: run.bundle_ids.clone(),
            timeout_at: run.timeout_at,
            // Absent from `NewRun`; each has exactly one writer and it is not
            // `create`.
            execution_ref: None,
            log_storage_ref: None,
            started_at: None,
            finished_at: None,
            error: None,
            created_at: created.created_at,
            updated_at: created.updated_at,
        };
        assert_eq!(
            created, expected,
            "the inserted model must decode back to the input"
        );
        assert_eq!(
            created.created_at, created.updated_at,
            "a freshly created run has never been updated"
        );

        let fetched = repo
            .get(&conn, &scope(tenant), created.id)
            .await
            .unwrap()
            .expect("the run must read back");
        assert_eq!(fetched, expected);

        // The counters are a separate projection and start at zero.
        assert_eq!(
            repo.get_result(&conn, &scope(tenant), created.id)
                .await
                .unwrap()
                .expect("counters must read back"),
            RunResult::default()
        );
    }

    /// **The F1 regression, in the form the type system left available.**
    ///
    /// `create` honoured a caller-supplied `Run::id`, and `qa_runs.id` is a
    /// global primary key — the one index in this schema that cannot be
    /// tenant-prefixed. So a PK collision became a unique violation, became
    /// `RunNameExists`: probing an existing id answered `Err` while an unused
    /// id answered `Ok`, a working membership test over another tenant's run
    /// identifiers. Reproduced end to end.
    ///
    /// The first fix ignored the field and said so in a doc comment. The
    /// second removed the field: [`NewRun`] has no `id`, so **the probe this
    /// test used to perform can no longer be written** — `probe.id = victim.id`
    /// is now a compile error, which is a stronger guard than any assertion
    /// here could be and the reason this test changed shape.
    ///
    /// What is left to assert at runtime is the residue: the repository
    /// *mints* ids rather than deriving them from anything a caller controls.
    /// Two identical launches must not collide, in one tenant or across two —
    /// a derived id (a hash of name and tenant, say) would satisfy the type
    /// system and reintroduce the oracle.
    #[tokio::test]
    async fn every_created_run_gets_a_freshly_minted_id() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (victim, attacker) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmRunsRepository;

        let secret = repo
            .create(&conn, &scope(victim), victim, sample_new_run("victims-run"))
            .await
            .unwrap();

        // Byte-identical inputs, two tenants: ids must still differ.
        let twin = repo
            .create(
                &conn,
                &scope(attacker),
                attacker,
                sample_new_run("victims-run"),
            )
            .await
            .expect("a name is namespaced by tenant");
        assert_ne!(
            twin.id, secret.id,
            "an id derived from the caller's input would collide here, and a \
             collision on this global primary key is the oracle"
        );

        // Same tenant, different names: also distinct.
        let sibling = repo
            .create(&conn, &scope(attacker), attacker, sample_new_run("another"))
            .await
            .unwrap();
        assert_ne!(sibling.id, twin.id);

        // ...and the victim's run is untouched and still invisible to the
        // attacker, who now holds two ids that are not it.
        assert_eq!(
            repo.get(&conn, &scope(victim), secret.id)
                .await
                .unwrap()
                .expect("the victim's run must survive")
                .name,
            "victims-run"
        );
        assert!(
            repo.get(&conn, &scope(attacker), secret.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// A unique violation is a domain conflict (409), never a `Database`
    /// error (500).
    #[tokio::test]
    async fn a_duplicate_run_name_in_one_tenant_conflicts() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;

        repo.create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();
        let err = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::RunNameExists { ref name } if name == "smoke-1"),
            "expected RunNameExists, got {err:?}"
        );
    }

    /// `idx_qa_runs_tenant_name` is tenant-prefixed, so a name is namespaced.
    #[tokio::test]
    async fn the_same_run_name_in_two_tenants_is_fine() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmRunsRepository;

        repo.create(&conn, &scope(a), a, sample_new_run("smoke-1"))
            .await
            .unwrap();
        repo.create(&conn, &scope(b), b, sample_new_run("smoke-1"))
            .await
            .expect("run names are namespaced by tenant");
    }

    /// A foreign run reads as absent, not as forbidden: telling the two apart
    /// is the cross-tenant existence oracle.
    #[tokio::test]
    async fn a_run_is_invisible_to_another_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(a), a, sample_new_run("smoke-1"))
            .await
            .unwrap();

        assert!(repo.get(&conn, &scope(b), run.id).await.unwrap().is_none());
        assert!(repo.list(&conn, &scope(b)).await.unwrap().is_empty());
        assert_eq!(repo.list(&conn, &scope(a)).await.unwrap().len(), 1);

        // …and the writes are scoped too: B cannot move A's run.
        assert!(
            !repo
                .update_state(
                    &conn,
                    &scope(b),
                    run.id,
                    RunState::Created,
                    RunState::Canceled,
                    RunStatePatch::default(),
                )
                .await
                .unwrap()
        );
        assert!(
            !repo
                .set_execution_ref(&conn, &scope(b), run.id, "hijacked")
                .await
                .unwrap()
        );
        assert!(
            !repo
                .add_result_counts(
                    &conn,
                    &scope(b),
                    run.id,
                    RunResultDelta {
                        failed: 99,
                        ..RunResultDelta::default()
                    },
                )
                .await
                .unwrap()
        );
        let untouched = repo.get(&conn, &scope(a), run.id).await.unwrap().unwrap();
        assert_eq!(untouched.state, RunState::Created);
        assert_eq!(
            untouched.execution_ref, None,
            "B's `set_execution_ref` must not have landed"
        );
    }

    /// The compare-and-set. A stale `from` matches nothing, which is what
    /// makes the transition atomic against a racing writer.
    #[tokio::test]
    async fn update_state_succeeds_only_from_the_expected_state() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();

        assert!(
            repo.update_state(
                &conn,
                &scope(tenant),
                run.id,
                RunState::Created,
                RunState::Queued,
                RunStatePatch::default(),
            )
            .await
            .unwrap()
        );

        // The same call again: the run has moved on, so the guard refuses.
        assert!(
            !repo
                .update_state(
                    &conn,
                    &scope(tenant),
                    run.id,
                    RunState::Created,
                    RunState::Queued,
                    RunStatePatch::default(),
                )
                .await
                .unwrap(),
            "a stale `from` must match no row: a blind UPDATE would let two \
             racing completions both win"
        );
        assert_eq!(
            repo.get(&conn, &scope(tenant), run.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            RunState::Queued
        );
    }

    /// A patch writes only the columns it names. Writing all three blindly
    /// would clear `started_at` on every completion.
    #[tokio::test]
    async fn a_state_patch_leaves_the_columns_it_omits_alone() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();
        let finished = now() + time::Duration::seconds(90);

        // `started_at` is reached the only way it can be: a transition that
        // carries it. `create` cannot write it -- see `NewRun`.
        assert!(
            repo.update_state(
                &conn,
                &scope(tenant),
                run.id,
                RunState::Created,
                RunState::Running,
                RunStatePatch {
                    started_at: Some(now()),
                    ..RunStatePatch::default()
                },
            )
            .await
            .unwrap()
        );

        assert!(
            repo.update_state(
                &conn,
                &scope(tenant),
                run.id,
                RunState::Running,
                RunState::Failed,
                RunStatePatch {
                    started_at: None,
                    finished_at: Some(finished),
                    error: Some("workflow failed".to_owned()),
                },
            )
            .await
            .unwrap()
        );

        let stored = repo
            .get(&conn, &scope(tenant), run.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, RunState::Failed);
        assert_eq!(stored.finished_at, Some(finished));
        assert_eq!(stored.error.as_deref(), Some("workflow failed"));
        assert_eq!(
            stored.started_at,
            Some(now()),
            "an omitted patch field must not clear its column"
        );
    }

    /// Two `+1`s make `2`. A read-modify-write in the process would lose one
    /// of two concurrent result events; a SQL-side `passed = passed + 1`
    /// cannot.
    #[tokio::test]
    async fn add_result_counts_accumulates_across_calls() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();

        // Three tests start: in_progress 3, total 3.
        for _ in 0..3 {
            assert!(
                repo.add_result_counts(
                    &conn,
                    &scope(tenant),
                    run.id,
                    RunResultDelta {
                        in_progress: 1,
                        total: 1,
                        ..RunResultDelta::default()
                    },
                )
                .await
                .unwrap()
            );
        }
        // Two finish, one at a time. **Every** counter has to accumulate over
        // at least two calls carrying a non-zero value for it, or an
        // overwriting `SET passed = $n` passes: with a single `passed: 1`
        // event the overwrite and the increment agree, and this test was
        // briefly unable to tell them apart. The deltas are signed, so
        // `in_progress` comes back down as they land.
        for _ in 0..2 {
            assert!(
                repo.add_result_counts(
                    &conn,
                    &scope(tenant),
                    run.id,
                    RunResultDelta {
                        passed: 1,
                        in_progress: -1,
                        ..RunResultDelta::default()
                    },
                )
                .await
                .unwrap()
            );
        }

        assert_eq!(
            repo.get_result(&conn, &scope(tenant), run.id)
                .await
                .unwrap()
                .unwrap(),
            RunResult {
                passed: 2,
                failed: 0,
                skipped: 0,
                in_progress: 1,
                total: 3,
            }
        );
    }

    /// `set_bundle_ids` writes the bundle list, and **only** it.
    ///
    /// Added by Task 14's fix round, closing the gap that task reported itself.
    /// The residual risk was never a wrong column name — `RunColumn::BundleIds`
    /// derives from the same entity field `create` writes, and that path is
    /// DB-tested — it was the **`UPDATE`**: a `Json` value bound through
    /// `col_expr`, the `rows_affected == 1` contract, and the `scope_with`
    /// interaction, none of which an in-memory double exercises.
    ///
    /// Modelled on [`set_execution_ref_writes_only_the_execution_reference`]
    /// below, including its discriminating half: the neighbouring
    /// `execution_ref`/`state` assertions are what make this catch a *wrong*
    /// column rather than merely a missing write. Dispatch calls this **before**
    /// the submit, so the run below is still `Created`, which also pins that the
    /// write is independent of `state`.
    ///
    /// [`set_execution_ref_writes_only_the_execution_reference`]: self::set_execution_ref_writes_only_the_execution_reference
    #[tokio::test]
    async fn set_bundle_ids_writes_only_the_bundle_list() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();
        // The shared fixture seeds two ids, where a real launch writes the list
        // **empty** (`Resolved::clone_new_run`; rules 4 and 5 belong to dispatch).
        // Starting non-empty is the better setup anyway: it is what makes the
        // replace-versus-append assertion below discriminate.
        assert_eq!(
            run.bundle_ids,
            vec![Uuid::from_u128(0x13), Uuid::from_u128(0x14)],
            "premise: the fixture seeds a list for the first write to replace"
        );

        let bundles = vec![Uuid::from_u128(0xB1), Uuid::from_u128(0xB2)];
        assert!(
            repo.set_bundle_ids(&conn, &scope(tenant), run.id, &bundles)
                .await
                .unwrap()
        );

        let stored = repo
            .get(&conn, &scope(tenant), run.id)
            .await
            .unwrap()
            .expect("the run must read back");
        assert_eq!(
            stored.bundle_ids, bundles,
            "order is preserved: it is the repository-group order the execution \
             nodes were built in"
        );
        assert_eq!(stored.execution_ref, None, "no neighbouring column moved");
        assert_eq!(stored.state, RunState::Created);

        // Replaces rather than appends: one dispatch knows the complete set, and
        // an append would accumulate a re-dispatch's bundles beside the first
        // attempt's with nothing to say which the current execution used.
        let second = vec![Uuid::from_u128(0xB3)];
        assert!(
            repo.set_bundle_ids(&conn, &scope(tenant), run.id, &second)
                .await
                .unwrap()
        );
        assert_eq!(
            repo.get(&conn, &scope(tenant), run.id)
                .await
                .unwrap()
                .unwrap()
                .bundle_ids,
            second
        );

        // An empty list round-trips rather than being treated as "leave alone".
        assert!(
            repo.set_bundle_ids(&conn, &scope(tenant), run.id, &[])
                .await
                .unwrap()
        );
        assert!(
            repo.get(&conn, &scope(tenant), run.id)
                .await
                .unwrap()
                .unwrap()
                .bundle_ids
                .is_empty()
        );

        // Scoped: another tenant's write matches no row.
        assert!(
            !repo
                .set_bundle_ids(&conn, &scope(Uuid::new_v4()), run.id, &bundles)
                .await
                .unwrap(),
            "the guarded UPDATE must not cross tenants"
        );
    }

    /// `set_execution_ref` writes the execution reference, and **only** it.
    ///
    /// Added 2026-08-13 after the spec review found the method had no positive
    /// test at all: its sole appearance in this suite was the negative
    /// assertion in `a_run_is_invisible_to_another_tenant`, so repointing it at
    /// `LogStorageRef` left all 210 tests green. A method that names a column
    /// needs a test that reads that column back — the neighbouring assertion
    /// on `log_storage_ref` is what makes this discriminate a wrong column
    /// rather than merely a missing write.
    ///
    /// Dispatch records the reference *before* it transitions the run, so this
    /// also pins that the write is independent of `state`: the run below is
    /// still `Created`.
    #[tokio::test]
    async fn set_execution_ref_writes_only_the_execution_reference() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();
        assert_eq!(
            run.execution_ref, None,
            "`create` does not write the handle; dispatch does"
        );
        assert_eq!(run.log_storage_ref, None);

        assert!(
            repo.set_execution_ref(&conn, &scope(tenant), run.id, "workflow/vhp-smoke-42")
                .await
                .unwrap()
        );

        let stored = repo
            .get(&conn, &scope(tenant), run.id)
            .await
            .unwrap()
            .expect("the run must read back");
        assert_eq!(
            stored.execution_ref.as_deref(),
            Some("workflow/vhp-smoke-42"),
            "the executor's handle must land in `execution_ref`"
        );
        assert_eq!(
            stored.log_storage_ref, None,
            "and nowhere else: the neighbouring nullable text column is exactly \
             what a repointed write would silently land in, and nothing in this \
             gear writes it yet"
        );
        assert_eq!(
            stored.state,
            RunState::Created,
            "recording the handle must not move the run: dispatch writes the \
             reference first and transitions second, so a crash between the two \
             leaves something to reconcile against"
        );

        // An id this scope cannot resolve matches no row.
        assert!(
            !repo
                .set_execution_ref(&conn, &scope(tenant), Uuid::new_v4(), "nowhere")
                .await
                .unwrap()
        );
    }

    /// A write must not be able to manufacture the state the reader rejects.
    ///
    /// Deltas are legitimately signed, so nothing stops a caller sending
    /// `passed: -5`. Unclamped, that drives the column negative and
    /// `usize_from_db` then fails closed on **every subsequent read** — the run
    /// still reads fine while its result projection returns `CorruptState`
    /// forever. The floor is what keeps a supported call from producing a
    /// permanently unreadable row.
    #[tokio::test]
    async fn a_delta_cannot_drive_a_counter_negative() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();

        // Two results land, then five retractions arrive for them.
        assert!(
            repo.add_result_counts(
                &conn,
                &scope(tenant),
                run.id,
                RunResultDelta {
                    passed: 2,
                    total: 2,
                    ..RunResultDelta::default()
                },
            )
            .await
            .unwrap()
        );
        assert!(
            repo.add_result_counts(
                &conn,
                &scope(tenant),
                run.id,
                RunResultDelta {
                    passed: -5,
                    in_progress: -1,
                    ..RunResultDelta::default()
                },
            )
            .await
            .unwrap()
        );

        // The read still works -- which is the whole point.
        let counts = repo
            .get_result(&conn, &scope(tenant), run.id)
            .await
            .expect("the projection must not be permanently broken by a write")
            .expect("counters must read back");
        assert_eq!(counts.passed, 0, "the counter floors at zero, never below");
        assert_eq!(counts.in_progress, 0);
        assert_eq!(counts.total, 2, "untouched counters are unaffected");

        // ...and the floor does not become a reset: incrementing from the
        // floor still counts.
        assert!(
            repo.add_result_counts(
                &conn,
                &scope(tenant),
                run.id,
                RunResultDelta {
                    passed: 3,
                    ..RunResultDelta::default()
                },
            )
            .await
            .unwrap()
        );
        assert_eq!(
            repo.get_result(&conn, &scope(tenant), run.id)
                .await
                .unwrap()
                .unwrap()
                .passed,
            3
        );
    }

    /// **The rotation, driven against a real database.**
    ///
    /// This is the regression test for the starvation the claim scan shipped
    /// with on 2026-08-15 and which this ordering closes: a window over a
    /// *stable* ordering returns the same rows forever whenever a row can stay
    /// in the set indefinitely, and a run whose cancel keeps failing can. Three
    /// things are asserted together, because the guarantee needs all three -
    /// the window truncates, the `after` predicate advances past what was seen,
    /// and a short window is what says "start over".
    ///
    /// Deleting the `id > after` predicate leaves the first two assertions
    /// green and fails the third, which is the whole point.
    #[tokio::test]
    async fn the_timeout_scan_windows_by_id_and_resumes_where_it_stopped() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let past = now();
        let cutoff = past + time::Duration::seconds(1);

        let window = usize::try_from(MAX_TIMEOUT_SWEEP_SCAN).unwrap();
        let mut all_ids = Vec::new();
        for i in 0..=window {
            let mut run = sample_new_run(&format!("overdue-{i}"));
            run.state = RunState::Running;
            run.timeout_at = Some(past);
            let created = repo
                .create(&conn, &scope(tenant), tenant, run)
                .await
                .unwrap();
            all_ids.push(created.id);
        }
        all_ids.sort_unstable();

        let first = repo
            .list_timeout_candidates(&conn, &scope(tenant), cutoff, None)
            .await
            .unwrap();
        assert!(first.truncated, "one row past the window must be reported");
        assert_eq!(
            first.rows.iter().map(|c| c.run_id).collect::<Vec<_>>(),
            all_ids[..window],
            "the window is the id-ordered prefix"
        );

        // The next scan resumes after the last row seen, and returns the rest.
        let resumed = repo
            .list_timeout_candidates(
                &conn,
                &scope(tenant),
                cutoff,
                first.rows.last().map(|c| c.run_id),
            )
            .await
            .unwrap();
        assert!(
            !resumed.truncated,
            "the remainder fits, which is what tells the caller to wrap"
        );
        assert_eq!(
            resumed.rows.iter().map(|c| c.run_id).collect::<Vec<_>>(),
            all_ids[window..],
            "and it is exactly the rows the first window could not reach - so no \
             candidate is excluded by any other candidate's behaviour"
        );
    }

    /// The timeout sweep returns each candidate's own tenant, so the caller
    /// can mint a per-tenant system context for the write that follows.
    #[tokio::test]
    async fn list_timeout_candidates_carry_their_own_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmRunsRepository;
        let past = now();
        let future = now() + time::Duration::hours(1);

        let mut overdue = sample_new_run("overdue");
        overdue.state = RunState::Running;
        overdue.timeout_at = Some(past);
        let overdue = repo.create(&conn, &scope(a), a, overdue).await.unwrap();

        let mut not_yet = sample_new_run("not-yet");
        not_yet.state = RunState::Running;
        not_yet.timeout_at = Some(future);
        repo.create(&conn, &scope(a), a, not_yet).await.unwrap();

        // Overdue but already terminal: not a candidate.
        let mut finished = sample_new_run("finished");
        finished.state = RunState::Succeeded;
        finished.timeout_at = Some(past);
        repo.create(&conn, &scope(a), a, finished).await.unwrap();

        // Overdue but with no deadline at all: `timeout_at IS NULL` compares
        // NULL and is excluded, which is the reading we want.
        let mut deadline_less = sample_new_run("no-deadline");
        deadline_less.state = RunState::Running;
        deadline_less.timeout_at = None;
        repo.create(&conn, &scope(a), a, deadline_less)
            .await
            .unwrap();

        let cutoff = past + time::Duration::seconds(1);
        let found = repo
            .list_timeout_candidates(&conn, &scope(a), cutoff, None)
            .await
            .unwrap();
        assert!(!found.truncated, "one row cannot fill the scan window");
        assert_eq!(
            found.rows,
            vec![TimeoutCandidate {
                run_id: overdue.id,
                tenant_id: a,
            }]
        );

        // Another tenant's sweep sees none of it.
        assert!(
            repo.list_timeout_candidates(&conn, &scope(b), cutoff, None)
                .await
                .unwrap()
                .rows
                .is_empty()
        );
    }

    /// **The predicate that decides which runs need observing**, with a fixture
    /// that exercises each half of the `WHERE` separately: the live-state pair
    /// rejects the terminal row, the `IS NOT NULL` rejects the row whose submit
    /// returned no handle, and the scope rejects every row for the other tenant.
    ///
    /// (An earlier revision of this line read *"four rows, one candidate, and
    /// each of the three rejections"*. The fixture has two candidates and two
    /// rejected rows, and the scope rejects all four rather than being a third
    /// row — so the sentence contradicted both the assertion below it and this
    /// doc's own next paragraph.)
    ///
    /// The `dispatching`-with-a-reference row is the one worth naming. It is
    /// **included**, because `service::dispatch::record_started` writes the
    /// execution reference *before* the transition to `running` and swallows a
    /// failure of either, so a run holding a live execution while still reading
    /// `dispatching` is a state the dispatcher can genuinely leave behind — and
    /// it is exactly a run nothing else will ever observe. Narrowing this to
    /// `running` alone would compile, pass every other test, and lose those runs
    /// to the timeout sweep.
    #[tokio::test]
    async fn watch_candidates_are_live_runs_holding_an_execution() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmRunsRepository;

        let mut running = sample_new_run("running-with-handle");
        running.state = RunState::Running;
        let running = repo.create(&conn, &scope(a), a, running).await.unwrap();
        repo.set_execution_ref(&conn, &scope(a), running.id, "exec-running")
            .await
            .unwrap();

        // Dispatching *with* a handle: `record_started` writes the reference
        // first and swallows a failed transition, so this is reachable and is a
        // candidate.
        let mut mid = sample_new_run("dispatching-with-handle");
        mid.state = RunState::Dispatching;
        let mid = repo.create(&conn, &scope(a), a, mid).await.unwrap();
        repo.set_execution_ref(&conn, &scope(a), mid.id, "exec-mid")
            .await
            .unwrap();

        // Dispatching with no handle: nothing to watch. This is boot recovery's
        // row, not this pass's.
        let mut no_handle = sample_new_run("dispatching-no-handle");
        no_handle.state = RunState::Dispatching;
        repo.create(&conn, &scope(a), a, no_handle).await.unwrap();

        // Terminal with a handle: the execution is over, so re-attaching would
        // be a watch on something nothing can act on.
        let mut done = sample_new_run("finished-with-handle");
        done.state = RunState::Succeeded;
        let done = repo.create(&conn, &scope(a), a, done).await.unwrap();
        repo.set_execution_ref(&conn, &scope(a), done.id, "exec-done")
            .await
            .unwrap();

        let found = repo
            .list_watch_candidates(&conn, &scope(a), None)
            .await
            .unwrap();
        assert!(!found.truncated, "two rows cannot fill the scan window");
        let mut expected = vec![
            WatchCandidate {
                run_id: running.id,
                tenant_id: a,
            },
            WatchCandidate {
                run_id: mid.id,
                tenant_id: a,
            },
        ];
        expected.sort_by_key(|candidate| candidate.run_id);
        assert_eq!(
            found.rows, expected,
            "id-ordered, and only the two live runs"
        );

        // Another tenant's scan sees none of it, which is what makes the
        // cross-tenant enumeration identity the only way to see them all.
        assert!(
            repo.list_watch_candidates(&conn, &scope(b), None)
                .await
                .unwrap()
                .rows
                .is_empty()
        );
    }

    /// The same three-part rotation guarantee `the_timeout_scan_windows_by_id_and_resumes_where_it_stopped`
    /// pins, asserted separately because the starvation it prevents is a
    /// different one: a timeout candidate leaves the set when it is reclaimed,
    /// while a healthy live run stays a candidate for its whole execution, so a
    /// full window of them would pin the head of any stable ordering
    /// permanently.
    #[tokio::test]
    async fn the_watch_scan_windows_by_id_and_resumes_where_it_stopped() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;

        let window = usize::try_from(MAX_WATCH_SCAN).unwrap();
        let mut all_ids = Vec::new();
        for i in 0..=window {
            let mut run = sample_new_run(&format!("live-{i}"));
            run.state = RunState::Running;
            let created = repo
                .create(&conn, &scope(tenant), tenant, run)
                .await
                .unwrap();
            repo.set_execution_ref(&conn, &scope(tenant), created.id, &format!("exec-{i}"))
                .await
                .unwrap();
            all_ids.push(created.id);
        }
        all_ids.sort_unstable();

        let first = repo
            .list_watch_candidates(&conn, &scope(tenant), None)
            .await
            .unwrap();
        assert!(first.truncated, "one row past the window must be reported");
        assert_eq!(
            first.rows.iter().map(|c| c.run_id).collect::<Vec<_>>(),
            all_ids[..window],
            "the window is the id-ordered prefix"
        );

        let resumed = repo
            .list_watch_candidates(&conn, &scope(tenant), first.rows.last().map(|c| c.run_id))
            .await
            .unwrap();
        assert!(
            !resumed.truncated,
            "the remainder fits, which is what tells the caller to wrap"
        );
        assert_eq!(
            resumed.rows.iter().map(|c| c.run_id).collect::<Vec<_>>(),
            all_ids[window..],
            "so a run behind a full window of long-lived healthy runs is still \
             reached, which is the starvation this ordering exists to prevent"
        );
    }

    /// The ownership precheck. `resolve_owned` is the only way to obtain the
    /// `OwnedRunId` that every write referencing a run demands, and it cannot
    /// distinguish "does not exist" from "belongs to someone else" — which is
    /// the whole point, since distinguishing them is the oracle.
    #[tokio::test]
    async fn resolve_owned_refuses_a_run_this_scope_cannot_see() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(a), a, sample_new_run("smoke-1"))
            .await
            .unwrap();

        assert_eq!(
            repo.resolve_owned(&conn, &scope(a), run.id)
                .await
                .unwrap()
                .get(),
            run.id
        );

        let foreign = repo
            .resolve_owned(&conn, &scope(b), run.id)
            .await
            .unwrap_err();
        let absent = repo
            .resolve_owned(&conn, &scope(b), Uuid::new_v4())
            .await
            .unwrap_err();
        assert!(
            matches!(foreign, DomainError::RunNotFound { id } if id == run.id),
            "a foreign run must read as not-found, got {foreign:?}"
        );
        assert!(
            matches!(absent, DomainError::RunNotFound { .. }),
            "and so must a nonexistent one, got {absent:?}"
        );
        assert_eq!(
            std::mem::discriminant(&foreign),
            std::mem::discriminant(&absent),
            "the two must be the same error: telling them apart is the oracle"
        );
    }

    /// Delete-then-insert dedupe on `(run_id, test_file, test_name)`, matching
    /// `manager/src/routes/runs.rs:1153-1185`. The tuple carries no unique
    /// index, so nothing but this method maintains the invariant.
    #[tokio::test]
    async fn upsert_test_result_replaces_the_prior_row_for_the_same_test() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();
        let owned = repo
            .resolve_owned(&conn, &scope(tenant), run.id)
            .await
            .unwrap();

        for status in ["PENDING", "RUNNING", "PASSED"] {
            repo.upsert_test_result(
                &conn,
                &scope(tenant),
                tenant,
                owned,
                NewTestResult {
                    test_file: "tests/a.py".to_owned(),
                    test_name: "test_login".to_owned(),
                    status: status.to_owned(),
                    duration: Some("85.06s (0:01:25)".to_owned()),
                    launch_id: Some("7204".to_owned()),
                    jira_key: None,
                    nodeid: None,
                    reason: None,
                    ticket: None,
                },
            )
            .await
            .unwrap();
        }

        let rows = repo
            .list_test_results(&conn, &scope(tenant), owned)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "three reports of one test are one row");
        assert_eq!(rows[0].status, "PASSED");
        assert_eq!(
            rows[0].duration.as_deref(),
            Some("85.06s (0:01:25)"),
            "the runner's extended duration text must survive verbatim \
             (`manager/src/services/argo.rs:3279`)"
        );
    }

    /// The three case-fidelity columns are written from [`NewTestResult`], not
    /// left at their defaults.
    ///
    /// # This test is the only thing standing between the executor and a
    /// permanently empty column
    ///
    /// `upsert_test_result` builds a `ResultAM` struct literal. Before this
    /// change those three fields read `String::new()`/`None`/`None`, and adding
    /// the matching fields to [`NewTestResult`] **did not make that literal
    /// stop compiling** — a struct literal that names every field is complete
    /// whether or not the values it names come from the argument. So the write
    /// path would have kept storing `''`/`NULL`/`NULL` for every case-level
    /// result the executor reported, with a green build, a green suite, and no
    /// diagnostic anywhere. The column would just have stayed empty, and
    /// qa-insights' per-case analytics would have silently degraded to per-file
    /// (`manager/src/routes/analytics.rs:97-98`).
    ///
    /// Nothing in the type system catches that class. A **service**-level test
    /// does not catch it either: ingest can populate `NewTestResult` perfectly
    /// and assert on what it published, and pass identically whether or not the
    /// repository forwarded anything. The assertion has to be "insert here,
    /// read the row back", which is what this is.
    ///
    /// **Break-verified.** Restoring `nodeid: ActiveValue::Set(String::new())`
    /// alone turns this red on the first assertion and nothing else in the
    /// workspace notices; likewise for each of the other two.
    ///
    /// # Why it reads back through the entity rather than [`TestResultRow`]
    ///
    /// `upsert_test_result` returns a `TestResultRow`. **Corrected by Task 4:**
    /// this used to say asserting through it was "impossible today, and would
    /// be the wrong instrument even once it does [carry the three]" - the first
    /// half has expired, because `TestResultRow` now carries all three. The
    /// second half stands and is why this test did not change: a row-shaped
    /// assertion would pass on a repository that dropped the values on the way
    /// in and happened to reconstruct them on the way out. `ResultEntity::find`
    /// is a real `SELECT` of the stored row.
    ///
    /// The complementary test - that the three survive the *read* path all the
    /// way to the SDK projection - is
    /// [`every_field_of_a_per_test_row_survives_the_trip_to_the_sdk`].
    ///
    /// # The second row
    ///
    /// A file-level observation — all three `None` — stored alongside, because
    /// the claim is not only "a non-empty nodeid is written" but "the two
    /// granularities stay distinguishable in one table", which is what
    /// `m20260818_000005_case_fidelity` gave up a second table for.
    /// `TestObservation::nodeid` records that convention and that nothing
    /// enforces it; this pins that the write path at least produces it.
    #[tokio::test]
    async fn a_case_level_nodeid_reason_and_ticket_are_actually_written() {
        const NODEID: &str =
            "tests/authn/test_error_handling.py::TestFailClosed::test_fail_closed[tls]";

        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();
        let owned = repo
            .resolve_owned(&conn, &scope(tenant), run.id)
            .await
            .unwrap();

        repo.upsert_test_result(
            &conn,
            &scope(tenant),
            tenant,
            owned,
            NewTestResult {
                test_file: "tests/authn/test_error_handling.py".to_owned(),
                test_name: "test_fail_closed".to_owned(),
                status: "XFAIL".to_owned(),
                duration: Some("85.06s (0:01:25)".to_owned()),
                launch_id: Some("7204".to_owned()),
                // File-level key, deliberately different from `ticket` below:
                // collapsing the two columns would relabel a file-wide bug link
                // as a per-case one, and this is where that would show.
                jira_key: Some("VHP-2618".to_owned()),
                nodeid: Some(NODEID.to_owned()),
                reason: Some("known upstream defect".to_owned()),
                ticket: Some("VHP-3117".to_owned()),
            },
        )
        .await
        .unwrap();

        // A file-level row under the same run and the same file.
        repo.upsert_test_result(
            &conn,
            &scope(tenant),
            tenant,
            owned,
            NewTestResult {
                test_file: "tests/authn/test_error_handling.py".to_owned(),
                test_name: "tests/authn/test_error_handling.py".to_owned(),
                status: "FAILED".to_owned(),
                duration: None,
                launch_id: None,
                jira_key: Some("VHP-2618".to_owned()),
                nodeid: None,
                reason: None,
                ticket: None,
            },
        )
        .await
        .unwrap();

        let stored = ResultEntity::find()
            .filter(Expr::col(ResultColumn::RunId).eq(run.id))
            .secure()
            .scope_with(&scope(tenant))
            .order_by(ResultColumn::TestName, sea_orm::Order::Asc)
            .all(&conn)
            .await
            .unwrap();
        assert_eq!(stored.len(), 2, "two granularities, two rows: {stored:?}");

        let case = stored
            .iter()
            .find(|row| row.test_name == "test_fail_closed")
            .expect("the case-level row must be readable");
        assert_eq!(
            case.nodeid, NODEID,
            "the repository wrote the column default instead of the supplied \
             nodeid, so every case-level result is stored as if it were \
             file-level"
        );
        assert_eq!(case.reason.as_deref(), Some("known upstream defect"));
        assert_eq!(case.ticket.as_deref(), Some("VHP-3117"));
        assert_eq!(
            case.jira_key.as_deref(),
            Some("VHP-2618"),
            "the file-level key is a separate column and must not have been \
             overwritten by the case-level one"
        );
        assert_ne!(case.jira_key, case.ticket);

        let file = stored
            .iter()
            .find(|row| row.test_name == "tests/authn/test_error_handling.py")
            .expect("the file-level row must be readable");
        assert_eq!(
            file.nodeid, "",
            "`None` must collapse to the column's single spelling of absent, \
             not to a literal \"None\" or a NULL the column forbids"
        );
        assert_eq!(file.reason, None, "reason stays genuinely nullable");
        assert_eq!(file.ticket, None, "ticket stays genuinely nullable");
    }

    /// The dedupe key includes the file, so the same test name in two files is
    /// two rows — and the empty string is a real key, not a wildcard.
    #[tokio::test]
    async fn upsert_test_result_keeps_rows_for_different_test_files() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();
        let owned = repo
            .resolve_owned(&conn, &scope(tenant), run.id)
            .await
            .unwrap();

        for file in ["tests/a.py", "tests/b.py", ""] {
            repo.upsert_test_result(
                &conn,
                &scope(tenant),
                tenant,
                owned,
                NewTestResult {
                    test_file: file.to_owned(),
                    test_name: "test_login".to_owned(),
                    status: "PASSED".to_owned(),
                    duration: None,
                    launch_id: None,
                    jira_key: None,
                    nodeid: None,
                    reason: None,
                    ticket: None,
                },
            )
            .await
            .unwrap();
        }

        let rows = repo
            .list_test_results(&conn, &scope(tenant), owned)
            .await
            .unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| r.test_file.as_str())
                .collect::<Vec<_>>(),
            vec!["", "tests/a.py", "tests/b.py"],
            "the file is part of the dedupe key, and `''` is one of its values"
        );
    }

    /// The open-set rule, asserted directly. This is the **opposite** of the
    /// fail-closed rule on every other enum-shaped column, and it is
    /// deliberate: the source system writes unvalidated runner text into this
    /// column (`manager/src/routes/runs.rs:1115` -> `:1168-1174`) and
    /// uppercases any unmapped pytest outcome
    /// (`manager/src/services/argo.rs:2932-2943`). A ninth value is a runner
    /// change, not corruption, and rejecting it would drop a real result.
    #[tokio::test]
    async fn an_unrecognised_per_test_status_is_stored_verbatim() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();
        let owned = repo
            .resolve_owned(&conn, &scope(tenant), run.id)
            .await
            .unwrap();

        // The eight known values, plus one the runner might invent tomorrow.
        let statuses = [
            "PASSED", "FAILED", "ERROR", "SKIPPED", "PENDING", "RUNNING", "XFAIL", "XPASS", "RERUN",
        ];
        for (index, status) in statuses.iter().enumerate() {
            repo.upsert_test_result(
                &conn,
                &scope(tenant),
                tenant,
                owned,
                NewTestResult {
                    test_file: format!("tests/t{index}.py"),
                    test_name: "test_x".to_owned(),
                    status: (*status).to_owned(),
                    duration: None,
                    launch_id: None,
                    jira_key: None,
                    nodeid: None,
                    reason: None,
                    ticket: None,
                },
            )
            .await
            .expect("an unrecognised status must be stored, not rejected");
        }

        let stored = repo
            .list_test_results(&conn, &scope(tenant), owned)
            .await
            .unwrap();
        let mut seen: Vec<&str> = stored.iter().map(|r| r.status.as_str()).collect();
        seen.sort_unstable();
        let mut expected = statuses.to_vec();
        expected.sort_unstable();
        assert_eq!(seen, expected, "every status crosses the layer verbatim");
    }

    /// The open set must survive `status VARCHAR(16)`, which is a *closed*
    /// constraint on the same value.
    ///
    /// On Postgres an over-long status raises `22001`, which surfaces as a 500
    /// and drops a real result — the column rejecting exactly the ninth value
    /// the open-set rule exists to accept. `INFRASTRUCTURE_ERROR` is 20
    /// characters, so the gap is reachable with a plausible token.
    ///
    /// **The first assertion is the important one.** `SQLite` does not enforce
    /// `VARCHAR` widths, so without it this whole tier would be blind to the
    /// defect and its silence would read as coverage. It pins the
    /// non-enforcement explicitly, so the day this suite runs against a real
    /// Postgres the assumption is written down and checkable.
    #[tokio::test]
    async fn an_over_long_status_is_truncated_to_fit_the_column() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();
        let owned = repo
            .resolve_owned(&conn, &scope(tenant), run.id)
            .await
            .unwrap();

        // Ground truth: this backend does not police the declared width, which
        // is why the normalization cannot be tested by "the insert fails".
        // Written through `secure_insert` so it is the same statement shape the
        // repository issues, minus the normalization step under test.
        let oversized = ResultAM {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant),
            run_id: ActiveValue::Set(run.id),
            test_file: ActiveValue::Set("tests/width.py".to_owned()),
            test_name: ActiveValue::Set("probe".to_owned()),
            status: ActiveValue::Set("X".repeat(10_000)),
            duration: ActiveValue::Set(None),
            launch_id: ActiveValue::Set(None),
            jira_key: ActiveValue::Set(None),
            // Column defaults; this test is about `status` width alone.
            nodeid: ActiveValue::Set(String::new()),
            reason: ActiveValue::Set(None),
            ticket: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now()),
            updated_at: ActiveValue::Set(now()),
        };
        let stored_raw = secure_insert::<ResultEntity>(oversized, &scope(tenant), &conn)
            .await
            .expect(
                "SQLite must accept an over-wide VARCHAR: if this ever fails, the \
                 backend started enforcing widths and this test tier is no longer \
                 blind to the defect",
            );
        assert_eq!(
            stored_raw.status.chars().count(),
            10_000,
            "SQLite stores the whole string: VARCHAR(16) is type affinity here, \
             not a constraint, so no test in this crate can observe the width \
             that Postgres would enforce"
        );

        // The repository's own write is the one that must fit, whatever the
        // backend does about it.
        for status in ["INFRASTRUCTURE_ERROR", "PASSED"] {
            repo.upsert_test_result(
                &conn,
                &scope(tenant),
                tenant,
                owned,
                NewTestResult {
                    test_file: format!("tests/{status}.py"),
                    test_name: "test_x".to_owned(),
                    status: status.to_owned(),
                    duration: None,
                    launch_id: None,
                    jira_key: None,
                    nodeid: None,
                    reason: None,
                    ticket: None,
                },
            )
            .await
            .expect("a long status must be stored, never rejected");
        }

        let stored = repo
            .list_test_results(&conn, &scope(tenant), owned)
            .await
            .unwrap();
        let long = stored
            .iter()
            .find(|r| r.test_file == "tests/INFRASTRUCTURE_ERROR.py")
            .expect("the long-status row must exist");
        assert_eq!(
            long.status, "INFRASTRUCTURE_E",
            "a status wider than the column is truncated to fit, not dropped"
        );
        assert!(
            long.status.chars().count() <= crate::infra::storage::mapper::MAX_TEST_STATUS_LEN,
            "nothing this repository writes may exceed the column width"
        );
        // A status that already fits is untouched -- including its case, which
        // this layer deliberately does not fold.
        assert!(stored.iter().any(|r| r.status == "PASSED"));
    }

    // -----------------------------------------------------------------------
    // The reconciler sweep (Task 4)
    // -----------------------------------------------------------------------

    /// Drive a freshly created run to a terminal state that finished at `at`.
    ///
    /// `update_state` is used rather than a hand-written `UPDATE` because it is
    /// the only writer of `finished_at` in this gear, so a sweep fixture built
    /// any other way could be testing against a column shape the production
    /// path never produces.
    async fn finish_at<C: DBRunner>(
        repo: &OrmRunsRepository,
        conn: &C,
        tenant: Uuid,
        run_id: Uuid,
        at: OffsetDateTime,
    ) {
        let moved = repo
            .update_state(
                conn,
                &scope(tenant),
                run_id,
                RunState::Created,
                RunState::Succeeded,
                RunStatePatch {
                    started_at: None,
                    finished_at: Some(at),
                    error: None,
                },
            )
            .await
            .unwrap();
        assert!(
            moved,
            "the fixture run must actually reach a terminal state"
        );
    }

    /// `now()` plus `hours`, for readable sweep fixtures.
    fn at(hours: i64) -> OffsetDateTime {
        now() + time::Duration::hours(hours)
    }

    /// **The sweep predicate, end to end against real SQL.**
    ///
    /// Three claims in one fixture, because they share it and each is
    /// meaningless without the others:
    ///
    /// 1. a run finished **before** the watermark is excluded;
    /// 2. a run finished **exactly on** it is included - the bound is `>=`, and
    ///    an exclusive one would drop a run that finished in the same clock
    ///    tick the reconciler recorded;
    /// 3. a run that has not finished at all is excluded, `finished_at` being
    ///    `NULL`. That one is the reason the query says `IS NOT NULL` out loud:
    ///    it is what SQL's three-valued logic already does, and a reader should
    ///    not have to trust that it does.
    ///
    /// Ordering is asserted here too - ascending is the contract, and it is
    /// what a `Vec` comparison pins for free.
    #[tokio::test]
    async fn the_sweep_returns_runs_finished_at_or_after_the_watermark_oldest_first() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;

        let mut created = Vec::new();
        for (name, hours) in [("before", 8_i64), ("on", 9), ("after", 12)] {
            let run = repo
                .create(&conn, &scope(tenant), tenant, sample_new_run(name))
                .await
                .unwrap();
            finish_at(&repo, &conn, tenant, run.id, at(hours)).await;
            created.push((name, run.id));
        }
        // Never finished: still `Created`, `finished_at` still NULL.
        let unfinished = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("unfinished"))
            .await
            .unwrap();

        let found = repo
            .list_finished_since(&conn, &scope(tenant), at(9), 10)
            .await
            .unwrap();

        let ids: Vec<Uuid> = found.iter().map(|run| run.id).collect();
        let id_of = |name: &str| {
            created
                .iter()
                .find(|(n, _)| *n == name)
                .expect("fixture run")
                .1
        };
        assert_eq!(
            ids,
            vec![id_of("on"), id_of("after")],
            "the pre-watermark run must be excluded, the one on the watermark \
             kept, and the order must be ascending",
        );
        assert!(
            !ids.contains(&unfinished.id),
            "a run with no finished_at has nothing to backfill and must never \
             appear in a sweep",
        );
    }

    /// **The `id` tiebreak is in the statement, not in luck.**
    ///
    /// Six runs share one `finished_at`, which is what two completions landing
    /// in the same clock tick look like. Without the second sort key their
    /// relative order is whatever the plan produces, so a reconciler taking a
    /// short page and advancing its watermark past that timestamp would step
    /// over whichever run the database felt like putting second - a silent loss
    /// that never reproduces.
    ///
    /// The assertion is that the page is the `id`-ascending prefix, twice: the
    /// full page, and a page of three. Two calls, because "sorted" and "the
    /// same boundary every time" are different claims and only the second is
    /// what the watermark depends on.
    ///
    /// **On the strength of this as a break-detector.** `create` mints random
    /// v4 ids, so a tie-blind query returning insertion order would have to hit
    /// the one permutation in 720 that is also id-ascending to pass. The test
    /// is never flaky when the code is right - `id` ascending is the correct
    /// answer unconditionally - it is only the *detection* that is 719/720, and
    /// that is stated rather than papered over.
    #[tokio::test]
    async fn the_sweep_breaks_a_finished_at_tie_on_id_so_the_page_boundary_is_stable() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;

        let tick = at(10);
        let mut ids = Vec::new();
        for n in 0..6 {
            let run = repo
                .create(
                    &conn,
                    &scope(tenant),
                    tenant,
                    sample_new_run(&format!("tied-{n}")),
                )
                .await
                .unwrap();
            finish_at(&repo, &conn, tenant, run.id, tick).await;
            ids.push(run.id);
        }
        let mut ascending = ids.clone();
        ascending.sort_unstable();
        // Deliberately **no** `assert_ne!(ids, ascending)` guard here. It would
        // read as "prove the fixture is not already in id order", but it is
        // itself a 1-in-720 coin flip on random v4 ids - a guard that fails
        // spuriously is worse than the gap it covers, and the gap is described
        // in this test's doc instead.

        let full = repo
            .list_finished_since(&conn, &scope(tenant), tick, 10)
            .await
            .unwrap();
        assert_eq!(
            full.iter().map(|run| run.id).collect::<Vec<_>>(),
            ascending,
            "runs tied on finished_at must come back in id order",
        );

        let page = repo
            .list_finished_since(&conn, &scope(tenant), tick, 3)
            .await
            .unwrap();
        assert_eq!(
            page.iter().map(|run| run.id).collect::<Vec<_>>(),
            ascending[..3].to_vec(),
            "and a short page must cut the same total order in the same place, \
             which is the property the watermark rests on",
        );
    }

    /// The sweep is a read of runs and is scoped exactly as `list` is: another
    /// tenant's finished run is not merely reordered out of the page, it is not
    /// in it.
    #[tokio::test]
    async fn the_sweep_never_returns_another_tenants_finished_run() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmRunsRepository;

        let mine = repo
            .create(&conn, &scope(a), a, sample_new_run("mine"))
            .await
            .unwrap();
        finish_at(&repo, &conn, a, mine.id, at(10)).await;
        let theirs = repo
            .create(&conn, &scope(b), b, sample_new_run("theirs"))
            .await
            .unwrap();
        finish_at(&repo, &conn, b, theirs.id, at(10)).await;

        let sweep = repo
            .list_finished_since(&conn, &scope(a), at(1), 100)
            .await
            .unwrap();
        assert_eq!(
            sweep.iter().map(|run| run.id).collect::<Vec<_>>(),
            vec![mine.id],
            "a sweep is wider than a page in time, never in tenancy",
        );

        assert!(
            repo.list_finished_since(&conn, &scope(b), at(1), 100)
                .await
                .unwrap()
                .iter()
                .all(|run| run.id != mine.id),
            "and symmetrically",
        );
    }

    /// **All ten contract fields survive database -> row -> SDK projection.**
    ///
    /// `RunTestResult` is built by a struct literal from a `TestResultRow`
    /// which is itself built by a struct literal from an entity model. Neither
    /// literal fails to compile when a field is dropped, and a dropped
    /// `Option` field reads back as `None`, which looks exactly like a runner
    /// that did not report it. Tasks 2 and 3 each shipped one instance of
    /// precisely this.
    ///
    /// So every field is set to a **distinct, non-default** value, and every
    /// field is asserted. Distinct matters as much as non-default: three
    /// `Some("VHP-1")`s would not catch a literal that read `ticket` into
    /// `jira_key`.
    ///
    /// **Break-verified.** Dropping `nodeid: m.nodeid` from
    /// `mapper::test_result_to_row` (to `String::new()`) turns this red and
    /// leaves the rest of the workspace green; the same holds for `reason` and
    /// `ticket`, and for swapping `jira_key`/`ticket` in the `From` impl.
    #[tokio::test]
    async fn every_field_of_a_per_test_row_survives_the_trip_to_the_sdk() {
        use qa_runs_sdk::RunTestResult;

        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("fidelity-1"))
            .await
            .unwrap();
        let owned = repo
            .resolve_owned(&conn, &scope(tenant), run.id)
            .await
            .unwrap();

        repo.upsert_test_result(
            &conn,
            &scope(tenant),
            tenant,
            owned,
            NewTestResult {
                test_file: "tests/authn/test_error_handling.py".to_owned(),
                test_name: "test_fail_closed".to_owned(),
                // Uppercase and eight bytes, so it is stored verbatim rather
                // than normalized or truncated - this test is about carriage,
                // not about the two transformations that have their own tests.
                status: "XFAIL".to_owned(),
                duration: Some("85.06s (0:01:25)".to_owned()),
                launch_id: Some("7204".to_owned()),
                jira_key: Some("VHP-2618".to_owned()),
                nodeid: Some(
                    "tests/authn/test_error_handling.py::TestFailClosed::test_fail_closed[tls]"
                        .to_owned(),
                ),
                reason: Some("known upstream defect".to_owned()),
                ticket: Some("VHP-3117".to_owned()),
            },
        )
        .await
        .unwrap();

        let projected: Vec<RunTestResult> = repo
            .list_test_results(&conn, &scope(tenant), owned)
            .await
            .unwrap()
            .into_iter()
            .map(RunTestResult::from)
            .collect();

        assert_eq!(
            projected,
            vec![RunTestResult {
                run_id: run.id,
                test_file: "tests/authn/test_error_handling.py".to_owned(),
                test_name: "test_fail_closed".to_owned(),
                status: "XFAIL".to_owned(),
                duration: Some("85.06s (0:01:25)".to_owned()),
                launch_id: Some("7204".to_owned()),
                jira_key: Some("VHP-2618".to_owned()),
                nodeid: "tests/authn/test_error_handling.py::TestFailClosed::test_fail_closed[tls]"
                    .to_owned(),
                reason: Some("known upstream defect".to_owned()),
                ticket: Some("VHP-3117".to_owned()),
            }],
            "every one of the ten fields must arrive, and arrive in its own \
             slot: a whole-struct comparison is what makes a transposed pair \
             fail as loudly as a dropped one",
        );
    }

    /// The sweep window is the collection endpoint's ceiling, and a `limit` of
    /// zero stays zero.
    ///
    /// A floor of 1 would hand a watermark consumer a run it did not ask for,
    /// which for that caller is worse than an empty page - so this pins the
    /// absence of the clamp `LimitCfg` would otherwise apply.
    #[test]
    fn the_sweep_limit_is_clamped_to_the_page_ceiling_and_zero_stays_zero() {
        assert_eq!(super::sweep_limit(u32::MAX), PAGE_LIMITS.max);
        assert_eq!(super::sweep_limit(1), 1);
        assert_eq!(super::sweep_limit(0), 0);
    }

    /// Per-test rows are scoped by tenant in their own right, not only through
    /// their run.
    #[tokio::test]
    async fn per_test_rows_are_invisible_to_another_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(a), a, sample_new_run("smoke-1"))
            .await
            .unwrap();
        let owned = repo.resolve_owned(&conn, &scope(a), run.id).await.unwrap();
        repo.upsert_test_result(
            &conn,
            &scope(a),
            a,
            owned,
            NewTestResult {
                test_file: "tests/a.py".to_owned(),
                test_name: "test_login".to_owned(),
                status: "PASSED".to_owned(),
                duration: None,
                launch_id: None,
                jira_key: None,
                nodeid: None,
                reason: None,
                ticket: None,
            },
        )
        .await
        .unwrap();

        assert!(
            repo.list_test_results(&conn, &scope(b), owned)
                .await
                .unwrap()
                .is_empty(),
            "another tenant must not read this run's per-test rows even holding its id"
        );
    }

    /// Fail-closed decoding, proven against the database rather than in
    /// isolation: a `state` the SDK cannot spell makes the read an error, not
    /// a default. Written through the same `SecureORM` update the repository
    /// uses, so this is a value the storage layer genuinely accepts.
    #[tokio::test]
    async fn a_corrupt_state_column_fails_the_read_rather_than_defaulting() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;
        let run = repo
            .create(&conn, &scope(tenant), tenant, sample_new_run("smoke-1"))
            .await
            .unwrap();

        RunEntity::update_many()
            .filter(by_id(run.id))
            .secure()
            .scope_with(&scope(tenant))
            .col_expr(RunColumn::State, Expr::value("suceeded"))
            .exec(&conn)
            .await
            .unwrap();

        let err = repo.get(&conn, &scope(tenant), run.id).await.unwrap_err();
        assert!(
            matches!(
                err,
                DomainError::CorruptState { what: "run.state", id, .. } if id == run.id
            ),
            "a corrupt state must error and name the run, got {err:?}"
        );
        // …and the list read fails the same way rather than dropping the row.
        assert!(repo.list(&conn, &scope(tenant)).await.is_err());
    }

    /// A plan target has no test file; a custom-plan target has no repo; a
    /// collect target has a URL and no plan path. The flattened columns must
    /// come back as the same variant they went in as.
    #[tokio::test]
    async fn every_target_kind_survives_the_flattened_columns() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let repo = OrmRunsRepository;

        let targets = [
            RunTarget::Plan {
                repo_id: Uuid::from_u128(0x20),
                path: "plans/smoke.yaml".to_owned(),
            },
            RunTarget::CustomPlan {
                id: Uuid::from_u128(0x21),
            },
            // A collect target has a repository, no plan path, and a URL. The
            // insert reads that URL out of `target_to_columns`' fifth field,
            // which no compile error would have demanded — see
            // `m20260818_000006_collect_target`.
            RunTarget::Collect {
                repo_id: Uuid::from_u128(0x22),
                collect_url: "https://insights.example/qa/v1/collect/r/main".to_owned(),
            },
        ];
        for (index, target) in targets.into_iter().enumerate() {
            let mut sample = sample_new_run(&format!("run-{index}"));
            sample.target = target.clone();
            sample.source = RunSource::Manual;
            let created = repo
                .create(&conn, &scope(tenant), tenant, sample)
                .await
                .unwrap();
            assert_eq!(
                repo.get(&conn, &scope(tenant), created.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .target,
                target
            );
        }
    }

    /// Every multi-row `SELECT` this repository issues, rendered rather than
    /// executed — a scope is all these queries need, so this needs no
    /// database.
    ///
    /// Each entry calls the **production builder** rather than a hand-copied
    /// lookalike chain, so a join added to any of these queries is a join this
    /// renders too. `into_inner()` hands back the plain `sea_orm::Select` that
    /// `.build`/`QueryTrait` know how to render; the production paths never
    /// call either, because they go straight to `.all(runner)` instead.
    fn rendered_list_sqls() -> Vec<(&'static str, String)> {
        use sea_orm::{DatabaseBackend, QueryTrait};

        fn render(q: toolkit_db::secure::SecureSelect<RunEntity, Scoped>) -> String {
            q.into_inner().build(DatabaseBackend::Postgres).to_string()
        }

        let scope = scope(Uuid::new_v4());
        let now = OffsetDateTime::now_utc();
        vec![
            ("list", render(list_query(&scope))),
            ("list_page (OData)", render(odata_list_query(&scope))),
            (
                "list_finished_since",
                render(finished_since_query(&scope, now, 50)),
            ),
            (
                "list_timeout_candidates",
                render(timeout_candidates_query(&scope, now, Some(Uuid::new_v4()))),
            ),
            (
                "list_watch_candidates",
                render(watch_candidates_query(&scope, Some(Uuid::new_v4()))),
            ),
        ]
    }

    /// **Legacy's OOM, made unrepeatable.** Selecting bulk log text for every
    /// row of the full run history once `OOMKilled` the source system's manager.
    /// The log lives in its own table so that query cannot be written by
    /// accident; this asserts no multi-row read path has learned to join it.
    ///
    /// The rendered SQL is inspected rather than the source text: a grep over
    /// this file would pass while a join arrived through a shared query
    /// builder.
    ///
    /// **All five paths, not just `list`.** The first revision of this guard
    /// covered `list_query` alone — which is the narrower of the two
    /// collection reads. `list_page`'s `OData` query is the *full-history*
    /// shape that legacy actually died on, and `list_finished_since`,
    /// `list_timeout_candidates` and `list_watch_candidates` each allocate
    /// whole `Run`s over a window too. **Break-tested per path**, one at a
    /// time: adding `.filter(Expr::cust("qa_run_logs.text IS NOT NULL"))` to
    /// each of the five builders in turn — the smallest mutation that puts the
    /// table into the rendered statement, standing in for the join that would
    /// actually arrive — turns this red and names that path in the failure. All
    /// five were run, including the four the first revision of this guard could
    /// not have caught, which was the point of extending it.
    #[test]
    fn no_list_query_reaches_the_log_table() {
        let rendered = rendered_list_sqls();
        assert_eq!(
            rendered.len(),
            5,
            "every multi-row read must be listed here, or this guard silently \
             stops covering the one that was dropped",
        );
        for (path, sql) in rendered {
            assert!(
                !sql.contains("qa_run_logs"),
                "{path} must not touch qa_run_logs; got: {sql}",
            );
        }
    }
}
