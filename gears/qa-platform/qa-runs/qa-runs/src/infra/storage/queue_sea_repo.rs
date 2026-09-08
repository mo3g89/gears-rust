//! `SecureORM` implementation of [`QueueRepository`].
//!
//! Filter-first throughout, for the reason given in `runs_sea_repo`.

use async_trait::async_trait;
use qa_runs_sdk::QueueState;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use time::OffsetDateTime;
use toolkit_db::odata::sea_orm_filter::{PaginateOdataTryError, paginate_odata_try};
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::queue::{AdmissionDecision, QueuedRow};
use crate::domain::repos::{
    ClaimAge, ClaimRow, ExpiredRow, MAX_CLAIM_SCAN, MAX_QUEUE_READ_LIMIT, NewQueueRow,
    QueueRepository, QueueRowRecord, QueuedPlatform, RowStatus, Windowed, overread, window_size,
};
use crate::infra::storage::db::{PAGE_LIMITS, db_err, odata_err};
use crate::infra::storage::entity::run_queue::{
    ActiveModel as QueueAM, Column as QueueColumn, Entity as QueueEntity, Model as QueueModel,
};
use crate::infra::storage::mapper::{
    queue_row_to_record, queue_state_from_str, run_kind_from_str, run_source_from_str,
};
use crate::infra::storage::odata::{QueueFilterField, QueueODataMapper};

/// ORM-based implementation of the `QueueRepository` trait.
#[derive(Clone, Default)]
pub struct OrmQueueRepository;

/// `WHERE id = $1`.
fn by_id(id: Uuid) -> Condition {
    Condition::all().add(QueueColumn::Id.eq(id))
}

/// The window size [`QueueRepository::list_for_read`] will actually use.
///
/// Extracted for the reason `expire_one` was: a bound that lives only as
/// `.min(..)` inside a query builder can be tested only by materialising
/// `MAX_QUEUE_READ_LIMIT + 1` rows, and a test that costs a thousand inserts
/// to run is a test that gets deleted. As a named function the arithmetic gets
/// a free unit test, and the DB test shrinks to proving that `.limit()` is
/// wired to this function's output rather than to the raw parameter — two
/// cheap checks in place of one expensive one, each pinning a different half.
fn read_limit(requested: u64) -> u64 {
    requested.min(MAX_QUEUE_READ_LIMIT)
}

/// The two columns [`QueueRepository::platforms_with_queued_rows`] projects.
///
/// A row shape rather than the entity, so the query can `GROUP BY` in the
/// database instead of materialising every queued row and folding them here.
#[derive(sea_orm::FromQueryResult)]
struct QueuedPlatformRow {
    platform_id: Uuid,
    tenant_id: Uuid,
}

/// `WHERE state = 'queued'`.
fn queued() -> Condition {
    Condition::all().add(QueueColumn::State.eq(QueueState::Queued.as_str()))
}

/// The two states that hold a claim on a platform — legacy's `CLAIM_STATES`
/// (`manager/src/services/run_queue.rs:104`).
fn claim_states() -> Condition {
    Condition::any()
        .add(QueueColumn::State.eq(QueueState::Dispatching.as_str()))
        .add(QueueColumn::State.eq(QueueState::Running.as_str()))
}

/// A guarded transition on one row, returning whether it matched.
///
/// Every terminal-ish write in this file goes through here so the
/// filter-first shape, the `updated_at` bump and the `rows_affected == 1`
/// reading are spelled once. `extra` carries the state guard when the caller
/// needs one.
async fn set_state<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    id: Uuid,
    state: QueueState,
    extra: Condition,
    columns: Vec<(QueueColumn, sea_orm::sea_query::SimpleExpr)>,
) -> Result<bool, DomainError> {
    let now = OffsetDateTime::now_utc();
    let mut update = QueueEntity::update_many()
        .filter(Condition::all().add(by_id(id)).add(extra))
        .secure()
        .scope_with(scope)
        .col_expr(QueueColumn::State, Expr::value(state.as_str()))
        .col_expr(QueueColumn::UpdatedAt, Expr::value(now));

    for (column, expr) in columns {
        update = update.col_expr(column, expr);
    }

    let result = update.exec(runner).await.map_err(db_err)?;
    Ok(result.rows_affected == 1)
}

/// The instant a claim's age is measured from:  `dispatched_at` when the row
/// was dispatched, else `enqueued_at`. Legacy coalesces exactly this way
/// (`manager/src/services/run_queue.rs:341-346`).
fn age_basis(m: &QueueModel) -> OffsetDateTime {
    m.dispatched_at.unwrap_or(m.enqueued_at)
}

/// Expire one row **if it is still queued**, reporting whether it was.
///
/// This is the security-critical half of [`QueueRepository::expire_queued_before`]
/// and it is a named function so it can be tested on its own. It closes the
/// TOCTOU between that method's select and its writes: a row dispatched in
/// between holds a claim on its platform, and expiring it would release that
/// claim while a live execution still owns the platform — the exact window in
/// which a second run can be admitted beside an exclusive one.
///
/// **Extracted after a break-test.** The sweep carries the same `queued`
/// predicate twice, once on the select and once here, and each made the other
/// unobservable: removing *either* alone left
/// `expire_queued_before_returns_what_it_expired_and_touches_no_claim` green,
/// so the guard that actually matters had no regression protection at all.
/// `the_expire_sweep_refuses_a_row_that_left_queued` now covers this one
/// directly. Inlining it back would re-open that hole, not just shorten the
/// file.
async fn expire_one<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    id: Uuid,
    reason: &str,
) -> Result<bool, DomainError> {
    set_state(
        runner,
        scope,
        id,
        QueueState::Expired,
        queued(),
        vec![
            (QueueColumn::Error, Expr::value(reason)),
            (
                QueueColumn::FinishedAt,
                Expr::value(OffsetDateTime::now_utc()),
            ),
        ],
    )
    .await
}

#[async_trait]
impl QueueRepository for OrmQueueRepository {
    async fn insert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        row: NewQueueRow,
    ) -> Result<QueueRowRecord, DomainError> {
        let now = OffsetDateTime::now_utc();
        let run_id = row.run.get();
        // An admitted launch is filed directly in `dispatching` with
        // `dispatched_at` set — that is what makes the row a claim *before*
        // the caller submits (`manager/src/services/run_queue.rs:169-172`).
        let (state, dispatched_at) = match row.decision {
            AdmissionDecision::Dispatch => (QueueState::Dispatching, Some(now)),
            AdmissionDecision::Queue => (QueueState::Queued, None),
        };

        let am = QueueAM {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            environment_id: ActiveValue::Set(row.platform_id),
            run_id: ActiveValue::Set(run_id),
            run_kind: ActiveValue::Set(row.run_kind.as_str().to_owned()),
            source: ActiveValue::Set(row.source.as_str().to_owned()),
            exclusive: ActiveValue::Set(row.exclusive),
            state: ActiveValue::Set(state.as_str().to_owned()),
            error: ActiveValue::Set(None),
            enqueued_at: ActiveValue::Set(now),
            dispatched_at: ActiveValue::Set(dispatched_at),
            finished_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        match secure_insert::<QueueEntity>(am, scope, runner).await {
            Ok(model) => queue_row_to_record(model),
            // `idx_qa_run_queue_tenant_run`, which is tenant-prefixed: the
            // colliding row belongs to this tenant, so naming the run here
            // leaks nothing.
            Err(e) if e.is_unique_violation() => Err(DomainError::QueueRowExists { run_id }),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn queued_depth<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        platform_id: Uuid,
    ) -> Result<usize, DomainError> {
        let count = QueueEntity::find()
            .filter(
                Condition::all()
                    .add(QueueColumn::EnvironmentId.eq(platform_id))
                    .add(queued()),
            )
            .secure()
            .scope_with(scope)
            .count(runner)
            .await
            .map_err(db_err)?;

        usize::try_from(count)
            .map_err(|_| DomainError::Internal(format!("queue depth {count} does not fit a usize")))
    }

    async fn queued_rows<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        platform_id: Uuid,
    ) -> Result<Vec<QueuedRow>, DomainError> {
        let rows = QueueEntity::find()
            .filter(
                Condition::all()
                    .add(QueueColumn::EnvironmentId.eq(platform_id))
                    .add(queued()),
            )
            .secure()
            .scope_with(scope)
            // FIFO. `id` breaks ties on the timestamp, exactly as legacy's
            // `ORDER BY enqueued_at ASC, id ASC`
            // (`manager/src/services/run_queue.rs:248`) — without it two rows
            // enqueued in the same clock tick would order arbitrarily and the
            // dispatcher would not be deterministic.
            //
            // KNOWN LIMIT OF THE TEST, recorded so nobody reads
            // `queued_rows_are_fifo_by_enqueue_then_id` as stronger evidence
            // than it is. Break-testing found that **deleting both clauses
            // leaves that test green** on `SQLite`: this query's filter is
            // `(tenant_id, platform_id, state)` and
            // `idx_qa_run_queue_fifo` continues `(enqueued_at, id)`, so the
            // index scan already returns exactly this order and an absent
            // `ORDER BY` is indistinguishable from a correct one. What the
            // test *does* discriminate is a **wrong** order — reversing either
            // clause turns it red, since that contradicts the scan. The
            // clauses stay because Postgres and `MySQL` are free to choose
            // another plan and neither is exercised anywhere in this
            // workspace.
            .order_by(QueueColumn::EnqueuedAt, sea_orm::Order::Asc)
            .order_by(QueueColumn::Id, sea_orm::Order::Asc)
            .all(runner)
            .await
            .map_err(db_err)?;

        Ok(rows
            .into_iter()
            .map(|m| QueuedRow {
                id: m.id,
                exclusive: m.exclusive,
                enqueued_at: m.enqueued_at,
            })
            .collect())
    }

    async fn claims_for_platform<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        platform_id: Uuid,
    ) -> Result<Vec<ClaimRow>, DomainError> {
        let rows = QueueEntity::find()
            .filter(
                Condition::all()
                    .add(QueueColumn::EnvironmentId.eq(platform_id))
                    .add(claim_states()),
            )
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;

        Ok(rows
            .into_iter()
            .map(|m| ClaimRow {
                id: m.id,
                run_id: m.run_id,
                exclusive: m.exclusive,
            })
            .collect())
    }

    async fn platforms_with_queued_rows<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<QueuedPlatform>, DomainError> {
        use sea_orm::ExprTrait;
        // De-duplicated **in SQL**, by `GROUP BY (platform_id, tenant_id)`.
        //
        // This used to fetch every queued row in scope and fold them in
        // memory, justified by two arguments that were both wrong. The first
        // said `DISTINCT` could not be used "because the distinct value is a
        // pair" — but SQL `DISTINCT` applies to the whole select list, so a
        // pair is exactly what it de-duplicates. The second said the result
        // was "bounded by the number of platforms with a queue", which bounds
        // the *output* and says nothing about the input: the input was every
        // queued row in scope, materialised whole, on every dispatcher tick.
        // That is the same allocation argument `MAX_QUEUE_READ_LIMIT` makes at
        // length one method over, applied inconsistently.
        //
        // `project_all` is `SecureORM`'s documented path for this
        // (`libs/toolkit-db/src/secure/select.rs:396-407`): the scope
        // condition is already on the `Select` handed to the closure, so
        // projecting cannot drop it — unlike `into_inner()`, which would. Rows
        // returned are now one per `(platform, tenant)` with a queue, which is
        // also the bound legacy's `SELECT DISTINCT platform` has
        // (`manager/src/services/run_queue.rs:262`); it has no tenant to
        // carry, which is the only reason the projection is a pair here.
        let rows: Vec<QueuedPlatformRow> = QueueEntity::find()
            .filter(queued())
            .secure()
            .scope_with(scope)
            .project_all(runner, |query| {
                query
                    .select_only()
                    .column(QueueColumn::EnvironmentId)
                    .column(QueueColumn::TenantId)
                    .group_by(QueueColumn::EnvironmentId)
                    .group_by(QueueColumn::TenantId)
                    // **Ordered by the oldest queued row on each platform.**
                    // Without an `ORDER BY` the drain order was whatever the
                    // planner returned, which is stable in practice and
                    // therefore a starvation primitive of the same family as
                    // the claim scan's: the tick spends its per-tick budget on
                    // whichever platform happens to sort first, every tick. The
                    // platform whose head-of-queue row has waited longest goes
                    // first instead, which is the FIFO the queue already
                    // promises within a platform, applied across them.
                    //
                    // This bounds *unfairness*, not latency: a platform whose
                    // runs build for minutes still holds the tick while it
                    // dispatches - `drain_platform` claims a platform's whole
                    // parallel FIFO - so a per-platform claim ceiling is a
                    // separate, still-open question. See
                    // `service::dispatch::run_tick`.
                    .order_by(
                        Expr::col(QueueColumn::EnqueuedAt).min(),
                        sea_orm::Order::Asc,
                    )
                    .into_model::<QueuedPlatformRow>()
            })
            .await
            .map_err(db_err)?;

        Ok(rows
            .into_iter()
            .map(|r| QueuedPlatform {
                platform_id: r.platform_id,
                tenant_id: r.tenant_id,
            })
            .collect())
    }

    /// `SET state = 'queued', dispatched_at = NULL WHERE state = 'dispatching'`.
    ///
    /// The guard is the safety property, not an optimisation — see the trait doc.
    /// `dispatched_at` is cleared through the same `set_state` column list
    /// [`QueueRepository::mark_dispatching`] uses to set it, so the two are
    /// visibly each other's inverse.
    async fn requeue<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        set_state(
            runner,
            scope,
            id,
            QueueState::Queued,
            Condition::all()
                .add(QueueColumn::State.eq(QueueState::Dispatching.as_str())),
            vec![(
                QueueColumn::DispatchedAt,
                Expr::value(None::<OffsetDateTime>),
            )],
        )
        .await
    }

    async fn mark_dispatching<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        set_state(
            runner,
            scope,
            id,
            QueueState::Dispatching,
            // `AND state = 'queued'` — the cheap guard against double dispatch
            // (`manager/src/services/run_queue.rs:285`).
            queued(),
            vec![(
                QueueColumn::DispatchedAt,
                Expr::value(OffsetDateTime::now_utc()),
            )],
        )
        .await
    }

    async fn mark_running<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        set_state(
            runner,
            scope,
            id,
            QueueState::Running,
            Condition::all(),
            Vec::new(),
        )
        .await
    }

    async fn mark_failed<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        error: &str,
    ) -> Result<bool, DomainError> {
        set_state(
            runner,
            scope,
            id,
            QueueState::Failed,
            Condition::all(),
            vec![
                (QueueColumn::Error, Expr::value(error)),
                (
                    QueueColumn::FinishedAt,
                    Expr::value(OffsetDateTime::now_utc()),
                ),
            ],
        )
        .await
    }

    async fn mark_done<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        set_state(
            runner,
            scope,
            id,
            QueueState::Done,
            Condition::all(),
            vec![(
                QueueColumn::FinishedAt,
                Expr::value(OffsetDateTime::now_utc()),
            )],
        )
        .await
    }

    async fn cancel_queued<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        reason: &str,
    ) -> Result<bool, DomainError> {
        set_state(
            runner,
            scope,
            id,
            // Two `l`s. Deliberate — see the mapper's header.
            QueueState::Cancelled,
            // The safety property, not an optimisation: a dispatching or
            // running row holds a claim an in-flight execution depends on.
            queued(),
            vec![
                (QueueColumn::Error, Expr::value(reason)),
                (
                    QueueColumn::FinishedAt,
                    Expr::value(OffsetDateTime::now_utc()),
                ),
            ],
        )
        .await
    }

    async fn all_claims<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        after: Option<Uuid>,
    ) -> Result<Windowed<ClaimAge>, DomainError> {
        let mut filter = claim_states();
        if let Some(after) = after {
            // The rotation. `id` is the only column here that nothing a row
            // does can change, which is exactly why it is the sort key - see
            // the trait doc on what happened when this ordered by `enqueued_at`.
            filter = Condition::all()
                .add(filter)
                .add(QueueColumn::Id.gt(after));
        }
        let rows = QueueEntity::find()
            .filter(filter)
            .secure()
            .scope_with(scope)
            .order_by(QueueColumn::Id, sea_orm::Order::Asc)
            .limit(overread(MAX_CLAIM_SCAN))
            .all(runner)
            .await
            .map_err(db_err)?;

        Ok(Windowed::from_overread(
            rows.into_iter()
                .map(|m| ClaimAge {
                    id: m.id,
                    tenant_id: m.tenant_id,
                    run_id: m.run_id,
                    platform_id: m.environment_id,
                    age_basis: age_basis(&m),
                })
                .collect(),
            window_size(MAX_CLAIM_SCAN),
        ))
    }

    async fn expire_queued_before<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        cutoff: OffsetDateTime,
        reason: &str,
    ) -> Result<Vec<ExpiredRow>, DomainError> {
        let candidates = QueueEntity::find()
            .filter(
                Condition::all()
                    .add(queued())
                    .add(QueueColumn::EnqueuedAt.lt(cutoff)),
            )
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;

        // Select-then-update, one guarded statement per row, where legacy uses
        // a single `UPDATE ... RETURNING`
        // (`manager/src/services/run_queue.rs:377-380`). `SeaORM`'s
        // `UpdateMany` reports only a row count, so a bulk update could not
        // tell the caller *which* rows it expired — and the mandatory alert
        // (guide line 96) needs exactly that. Per-row keeps the property
        // legacy's RETURNING buys: a row appears in this result only if its
        // own `AND state = 'queued'` update matched, so a row that was
        // dispatched between the read and the write is neither expired nor
        // announced. The loop is bounded by the sweep's candidate set.
        //
        // **Decode before writing, and never abort the batch.** An earlier
        // version expired the row first and decoded `run_kind`/`source`
        // afterwards with `?`, which cost twice over: a single corrupt row
        // aborted the sweep *after* earlier rows had been expired, so the
        // caller got an `Err` and no `ExpiredRow` for any of them — and those
        // rows were no longer `queued`, so a retried sweep could never revisit
        // them either. Decoding first makes an undecodable row one this sweep
        // declines to touch at all: it stays `queued`, produces no
        // `ExpiredRow`, and the next sweep tries again rather than this pass
        // marking a row `expired` whose own recorded kind or source it could
        // not even read back.
        let mut expired = Vec::new();
        for m in candidates {
            // Fail-closed decode, but scoped to this row rather than the batch.
            // The decoded values themselves are not kept — this is a
            // well-formedness gate, not a read the caller needs.
            if let Err(error) = run_kind_from_str(&m.run_kind, "queue.run_kind", m.id)
                .and_then(|_| run_source_from_str(&m.source, "queue.source", m.id))
            {
                tracing::error!(
                    queue_row = %m.id,
                    tenant_id = %m.tenant_id,
                    %error,
                    "queue row has a corrupt run_kind or source; leaving it queued rather \
                     than marking it expired with data this sweep could not decode"
                );
                continue;
            }

            if !expire_one(runner, scope, m.id, reason).await? {
                continue;
            }
            expired.push(ExpiredRow {
                id: m.id,
                tenant_id: m.tenant_id,
                run_id: m.run_id,
                platform_id: m.environment_id,
                exclusive: m.exclusive,
                enqueued_at: m.enqueued_at,
            });
        }
        Ok(expired)
    }

    async fn fail_orphaned_dispatching<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        ids: &[Uuid],
        reason: &str,
    ) -> Result<u64, DomainError> {
        if ids.is_empty() {
            // An empty `IN ()` is a syntax error on some backends, and a
            // no-op call is the normal case at a boot with a clean queue.
            return Ok(0);
        }

        let result = QueueEntity::update_many()
            .filter(
                Condition::all()
                    .add(QueueColumn::Id.is_in(ids.iter().copied()))
                    // The half only SQL can enforce: a row that started
                    // running between the caller's read and this write keeps
                    // its claim.
                    .add(QueueColumn::State.eq(QueueState::Dispatching.as_str())),
            )
            .secure()
            .scope_with(scope)
            .col_expr(QueueColumn::State, Expr::value(QueueState::Failed.as_str()))
            .col_expr(QueueColumn::Error, Expr::value(reason))
            .col_expr(
                QueueColumn::FinishedAt,
                Expr::value(OffsetDateTime::now_utc()),
            )
            .col_expr(
                QueueColumn::UpdatedAt,
                Expr::value(OffsetDateTime::now_utc()),
            )
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected)
    }

    async fn list_for_read<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        platform_id: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<QueueRowRecord>, DomainError> {
        let filter = match platform_id {
            Some(platform_id) => {
                Condition::all().add(QueueColumn::EnvironmentId.eq(platform_id))
            }
            None => Condition::all(),
        };

        let rows = QueueEntity::find()
            .filter(filter)
            .secure()
            .scope_with(scope)
            // Newest first, all states — legacy's window
            // (`manager/src/services/run_queue.rs:466`, `:477`). `id`
            // disambiguates ties so the window is stable across calls, which
            // legacy's does not do.
            .order_by(QueueColumn::EnqueuedAt, sea_orm::Order::Desc)
            .order_by(QueueColumn::Id, sea_orm::Order::Desc)
            // Clamped here rather than trusted from the caller: this is the
            // layer that allocates the rows.
            .limit(read_limit(limit))
            .all(runner)
            .await
            .map_err(db_err)?;

        rows.into_iter().map(queue_row_to_record).collect()
    }

    async fn list_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        platform_id: Option<Uuid>,
        query: &ODataQuery,
    ) -> Result<Page<QueueRowRecord>, DomainError> {
        let filter = match platform_id {
            Some(platform_id) => {
                Condition::all().add(QueueColumn::EnvironmentId.eq(platform_id))
            }
            None => Condition::all(),
        };
        // Filter-first, then scope: `paginate_odata_try` takes a
        // `SecureSelect<E, Scoped>`, so the tenant predicate is present
        // whatever the caller's `$filter` says.
        let scoped = QueueEntity::find()
            .filter(filter)
            .secure()
            .scope_with(scope);

        paginate_odata_try::<QueueFilterField, QueueODataMapper, _, _, _, DomainError, _>(
            scoped,
            runner,
            query,
            // Newest first, as legacy's window is
            // (`manager/src/services/run_queue.rs:466`), `id` breaking ties so
            // the page boundary is stable.
            ("enqueued_at", SortDir::Desc),
            PAGE_LIMITS,
            queue_row_to_record,
        )
        .await
        .map_err(|error| match error {
            PaginateOdataTryError::OData(e) => odata_err(&e),
            PaginateOdataTryError::MapError(e) => e,
        })
    }

    async fn row_status<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<RowStatus>, DomainError> {
        let found = QueueEntity::find()
            .filter(by_id(id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        found
            .map(|m| {
                Ok(RowStatus {
                    platform_id: m.environment_id,
                    state: queue_state_from_str(&m.state, m.id)?,
                })
            })
            .transpose()
    }
}

/// DB-backed tests. See `runs_sea_repo`'s test module for why this tier
/// exists: none of the SQL in this file is checked by `cargo build`.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use qa_runs_sdk::{Run, RunKind, RunSource, RunState};
    use toolkit_db::secure::Db;

    use super::*;
    use crate::domain::repos::{OwnedRunId, RunsRepository};
    use crate::infra::storage::runs_sea_repo::OrmRunsRepository;
    use crate::infra::storage::test_db::{inmem_db, now, sample_new_run, scope};

    /// **The rollback `service::runs`' cancel depends on, proven where it can
    /// be.**
    ///
    /// That service cancels a queue row and retires its run inside one
    /// `DBProvider::transaction`, because the two writes half-applying strands a
    /// run in `Queued` with no row — a state `list_timeout_candidates`
    /// (`dispatching | running` only) and the TTL sweep (`queued` **rows**) both
    /// miss, so nothing reclaims it.
    ///
    /// **The service tier cannot test that.** Its in-memory doubles mutate a
    /// `Mutex<Vec<_>>` and ignore the runner they are handed, so a rollback is
    /// invisible to them — a test written there would have asserted the harness,
    /// not the fix. This is the tier where the runner is real, so the property is
    /// asserted here, over the two repositories the service composes.
    #[tokio::test]
    async fn a_failed_write_rolls_back_a_cancel_in_the_same_transaction() {
        let db = inmem_db().await;
        let provider = std::sync::Arc::new(toolkit_db::DBProvider::<DomainError>::new(db.clone()));
        let tenant = Uuid::new_v4();
        let platform = Uuid::new_v4();
        let (run, owned) = seeded_run(&db, tenant, "smoke-1").await;

        let conn = db.conn().unwrap();
        let row = OrmQueueRepository
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap();

        // Cancel the row and then fail, exactly as a denied scope or a dropped
        // connection would after the first statement had succeeded.
        let outcome: Result<(), DomainError> = provider
            .transaction(move |tx| {
                Box::pin(async move {
                    assert!(
                        OrmQueueRepository
                            .cancel_queued(tx, &scope(tenant), row.id, "operator")
                            .await?
                    );
                    Err(DomainError::database("the connection dropped"))
                })
            })
            .await;
        assert!(outcome.is_err());

        let conn = db.conn().unwrap();
        assert_eq!(
            OrmQueueRepository
                .row_status(&conn, &scope(tenant), row.id)
                .await
                .unwrap()
                .map(|status| status.state),
            Some(QueueState::Queued),
            "the row must be back where it started; a cancelled row whose run was \
             never retired is a lost run"
        );
        assert_eq!(
            OrmRunsRepository
                .get(&conn, &scope(tenant), run.id)
                .await
                .unwrap()
                .map(|run| run.state),
            Some(RunState::Created),
            "and the run is untouched"
        );
    }

    /// Create a run in `tenant` and resolve it, which is the only way to reach
    /// [`QueueRepository::insert`].
    async fn seeded_run(db: &Db, tenant: Uuid, name: &str) -> (Run, OwnedRunId) {
        let conn = db.conn().unwrap();
        let runs = OrmRunsRepository;
        let run = runs
            .create(&conn, &scope(tenant), tenant, sample_new_run(name))
            .await
            .unwrap();
        let owned = runs
            .resolve_owned(&conn, &scope(tenant), run.id)
            .await
            .unwrap();
        (run, owned)
    }

    fn new_row(platform_id: Uuid, run: OwnedRunId, decision: AdmissionDecision) -> NewQueueRow {
        NewQueueRow {
            platform_id,
            run,
            run_kind: RunKind::Test,
            source: RunSource::Manual,
            exclusive: false,
            decision,
        }
    }

    /// Force a row's `enqueued_at`, so FIFO order can be asserted against
    /// controlled values rather than whatever two consecutive `now_utc()`
    /// calls happened to produce.
    async fn set_enqueued_at(db: &Db, tenant: Uuid, id: Uuid, at: OffsetDateTime) {
        let conn = db.conn().unwrap();
        QueueEntity::update_many()
            .filter(by_id(id))
            .secure()
            .scope_with(&scope(tenant))
            .col_expr(QueueColumn::EnqueuedAt, Expr::value(at))
            .exec(&conn)
            .await
            .unwrap();
    }

    /// An admitted launch is a claim the moment it is filed: `dispatching`
    /// with `dispatched_at` set, before the caller has submitted anything
    /// (`manager/src/services/run_queue.rs:169-172`).
    #[tokio::test]
    async fn an_admitted_launch_is_filed_as_a_claim_and_a_queued_one_is_not() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);

        let (_, dispatch) = seeded_run(&db, tenant, "dispatched").await;
        let (_, queued_run) = seeded_run(&db, tenant, "queued").await;

        let claim = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, dispatch, AdmissionDecision::Dispatch),
            )
            .await
            .unwrap();
        assert_eq!(claim.state, QueueState::Dispatching);
        assert!(
            claim.dispatched_at.is_some(),
            "a dispatch decision must stamp `dispatched_at` -- that is what makes \
             the row a claim before the executor is called"
        );

        let waiting = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, queued_run, AdmissionDecision::Queue),
            )
            .await
            .unwrap();
        assert_eq!(waiting.state, QueueState::Queued);
        assert_eq!(waiting.dispatched_at, None);
        assert_eq!(waiting.run_kind, RunKind::Test);
        assert_eq!(waiting.source, RunSource::Manual);
        assert_eq!(waiting.platform_id, platform);

        // `row_status`' platform is the lock key the force-start path takes,
        // so it is asserted rather than assumed: nothing else in this suite
        // reads that field, and a wrong column here would serialise launches
        // on the wrong mutex.
        let status = queue
            .row_status(&conn, &scope(tenant), waiting.id)
            .await
            .unwrap()
            .expect("the row must have a status");
        assert_eq!(status.platform_id, platform);
        assert_eq!(status.state, QueueState::Queued);
    }

    /// `idx_qa_run_queue_tenant_run` is a domain conflict, not a 500.
    #[tokio::test]
    async fn a_second_queue_row_for_the_same_run_conflicts() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let (run, owned) = seeded_run(&db, tenant, "smoke-1").await;

        queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(Uuid::from_u128(0x30), owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap();
        let err = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(Uuid::from_u128(0x30), owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::QueueRowExists { run_id } if run_id == run.id),
            "expected QueueRowExists, got {err:?}"
        );
    }

    /// FIFO is `enqueued_at ASC, id ASC`
    /// (`manager/src/services/run_queue.rs:248`). The `id` tiebreak is what
    /// makes the dispatcher deterministic when two rows share a timestamp.
    #[tokio::test]
    async fn queued_rows_are_fifo_by_enqueue_then_id() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);

        let mut rows = Vec::new();
        for index in 0..3 {
            let (_, owned) = seeded_run(&db, tenant, &format!("run-{index}")).await;
            rows.push(
                queue
                    .insert(
                        &conn,
                        &scope(tenant),
                        tenant,
                        new_row(platform, owned, AdmissionDecision::Queue),
                    )
                    .await
                    .unwrap(),
            );
        }

        // Oldest last by insertion order, so a query that ignored the sort
        // would return them the wrong way round.
        set_enqueued_at(&db, tenant, rows[0].id, now() + time::Duration::seconds(20)).await;
        // Two rows sharing an instant: only the `id` tiebreak orders these.
        let tie = now();
        set_enqueued_at(&db, tenant, rows[1].id, tie).await;
        set_enqueued_at(&db, tenant, rows[2].id, tie).await;

        let mut tied = [rows[1].id, rows[2].id];
        tied.sort_unstable();

        assert_eq!(
            queue
                .queued_rows(&conn, &scope(tenant), platform)
                .await
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![tied[0], tied[1], rows[0].id]
        );
    }

    /// The depth read the admission path takes **inside the platform lock**,
    /// before the occupancy read (`manager/src/services/run_queue.rs:633-655`).
    /// It counts this platform's `queued` rows and nothing else — a claim is
    /// not queue depth, and another platform's queue is not this one's.
    #[tokio::test]
    async fn queued_depth_counts_only_this_platforms_queued_rows() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);
        let other = Uuid::from_u128(0x31);

        for (index, (target, decision)) in [
            (platform, AdmissionDecision::Queue),
            (platform, AdmissionDecision::Queue),
            (platform, AdmissionDecision::Dispatch),
            (other, AdmissionDecision::Queue),
        ]
        .into_iter()
        .enumerate()
        {
            let (_, owned) = seeded_run(&db, tenant, &format!("run-{index}")).await;
            queue
                .insert(
                    &conn,
                    &scope(tenant),
                    tenant,
                    new_row(target, owned, decision),
                )
                .await
                .unwrap();
        }

        assert_eq!(
            queue
                .queued_depth(&conn, &scope(tenant), platform)
                .await
                .unwrap(),
            2
        );
        // …and another tenant's depth for the same platform is its own.
        assert_eq!(
            queue
                .queued_depth(&conn, &scope(Uuid::new_v4()), platform)
                .await
                .unwrap(),
            0
        );
    }

    /// `CLAIM_STATES` is exactly `dispatching` and `running`
    /// (`manager/src/services/run_queue.rs:104`). A terminal row holds nothing.
    #[tokio::test]
    async fn claims_for_platform_returns_only_dispatching_and_running() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);

        let mut ids = Vec::new();
        for index in 0..5 {
            let (_, owned) = seeded_run(&db, tenant, &format!("run-{index}")).await;
            ids.push(
                queue
                    .insert(
                        &conn,
                        &scope(tenant),
                        tenant,
                        new_row(platform, owned, AdmissionDecision::Dispatch),
                    )
                    .await
                    .unwrap()
                    .id,
            );
        }

        // ids[0] stays dispatching; ids[1] runs; the rest go terminal.
        assert!(
            queue
                .mark_running(&conn, &scope(tenant), ids[1])
                .await
                .unwrap()
        );
        assert!(
            queue
                .mark_done(&conn, &scope(tenant), ids[2])
                .await
                .unwrap()
        );
        assert!(
            queue
                .mark_failed(&conn, &scope(tenant), ids[3], "submit failed")
                .await
                .unwrap()
        );
        assert_eq!(
            queue
                .fail_orphaned_dispatching(&conn, &scope(tenant), &[ids[4]], "boot")
                .await
                .unwrap(),
            1
        );

        let mut claims: Vec<Uuid> = queue
            .claims_for_platform(&conn, &scope(tenant), platform)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        claims.sort_unstable();
        let mut expected = vec![ids[0], ids[1]];
        expected.sort_unstable();
        assert_eq!(claims, expected);
    }

    /// `requeue` is `mark_dispatching`'s inverse, and this is the test that says
    /// so against a real schema.
    ///
    /// **A new query needs a DB test, not a double**: `cargo build` proves nothing
    /// about `QueueColumn::DispatchedAt` being nullable in the DDL, and setting a
    /// column to `NULL` through `col_expr` is the one thing in this method that
    /// could work against an in-memory `HashMap` and fail against `SQLite`.
    ///
    /// Both halves are read back: the state, and the cleared `dispatched_at`.
    /// The second is not decoration — `all_claims` ages a claim from
    /// `dispatched_at` falling back to `enqueued_at`, so a requeued row that kept
    /// its old value would be aged from its *first* claim and could cross
    /// `orphan_timeout_seconds` the moment it is claimed again.
    #[tokio::test]
    async fn requeue_returns_a_claimed_row_to_the_queue_and_clears_its_dispatch_instant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let (_, owned) = seeded_run(&db, tenant, "smoke-1").await;
        let row = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(Uuid::from_u128(0x40), owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap();
        assert_eq!(row.state, QueueState::Queued);
        assert_eq!(row.dispatched_at, None);

        assert!(
            queue
                .mark_dispatching(&conn, &scope(tenant), row.id)
                .await
                .unwrap()
        );
        let claimed = queue
            .list_for_read(&conn, &scope(tenant), None, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.id == row.id)
            .expect("the row must read back");
        assert_eq!(claimed.state, QueueState::Dispatching);
        assert!(
            claimed.dispatched_at.is_some(),
            "premise: the claim recorded an instant for `requeue` to clear"
        );

        assert!(queue.requeue(&conn, &scope(tenant), row.id).await.unwrap());
        let back = queue
            .list_for_read(&conn, &scope(tenant), None, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.id == row.id)
            .expect("the row must read back");
        assert_eq!(back.state, QueueState::Queued);
        assert_eq!(
            back.dispatched_at, None,
            "a requeued row must not carry a dispatch instant it no longer has"
        );
        assert_eq!(
            back.enqueued_at, row.enqueued_at,
            "the row keeps its FIFO position, which is the point of requeueing \
             rather than failing it"
        );

        // And it is visible to the planner again.
        assert_eq!(
            queue
                .queued_rows(&conn, &scope(tenant), row.platform_id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// The guard, which is the safety property: a row that reached `running`
    /// holds a live execution, and returning it to the queue would let the same
    /// run be dispatched twice.
    #[tokio::test]
    async fn requeue_refuses_a_running_row() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let (_, owned) = seeded_run(&db, tenant, "smoke-1").await;
        let row = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(Uuid::from_u128(0x41), owned, AdmissionDecision::Dispatch),
            )
            .await
            .unwrap();
        assert!(
            queue
                .mark_running(&conn, &scope(tenant), row.id)
                .await
                .unwrap()
        );

        assert!(
            !queue.requeue(&conn, &scope(tenant), row.id).await.unwrap(),
            "a running row must never go back in the queue"
        );
        assert_eq!(
            queue
                .row_status(&conn, &scope(tenant), row.id)
                .await
                .unwrap()
                .expect("the row exists")
                .state,
            QueueState::Running
        );
    }

    /// The double-dispatch guard (`:285`).
    #[tokio::test]
    async fn mark_dispatching_is_false_for_a_row_that_is_no_longer_queued() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let (_, owned) = seeded_run(&db, tenant, "smoke-1").await;
        let row = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(Uuid::from_u128(0x30), owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap();

        assert!(
            queue
                .mark_dispatching(&conn, &scope(tenant), row.id)
                .await
                .unwrap()
        );
        assert!(
            !queue
                .mark_dispatching(&conn, &scope(tenant), row.id)
                .await
                .unwrap(),
            "a second dispatcher tick must not re-claim a row it already claimed"
        );
    }

    /// The safety property, asserted directly: a `dispatching` row holds a
    /// claim an in-flight execution depends on, and cancelling it here would
    /// let a new run be admitted beside an exclusive one
    /// (`manager/src/services/run_queue.rs:403-421`).
    #[tokio::test]
    async fn cancel_queued_refuses_a_dispatching_row() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let (_, owned) = seeded_run(&db, tenant, "smoke-1").await;
        let row = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(Uuid::from_u128(0x30), owned, AdmissionDecision::Dispatch),
            )
            .await
            .unwrap();

        assert!(
            !queue
                .cancel_queued(&conn, &scope(tenant), row.id, "user asked")
                .await
                .unwrap(),
            "cancelling a claim must refuse, not silently release the platform"
        );
        assert_eq!(
            queue
                .row_status(&conn, &scope(tenant), row.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            QueueState::Dispatching
        );
    }

    /// The TTL sweep returns what it expired — the property legacy gets from
    /// `UPDATE ... RETURNING` — and touches no claim, because a claimed row
    /// may still own a live execution.
    #[tokio::test]
    async fn expire_queued_before_returns_what_it_expired_and_touches_no_claim() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);

        let (old_run, old_owned) = seeded_run(&db, tenant, "old").await;
        let old = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, old_owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap();
        let (_, fresh_owned) = seeded_run(&db, tenant, "fresh").await;
        let fresh = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, fresh_owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap();
        let (_, claim_owned) = seeded_run(&db, tenant, "claim").await;
        let claim = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, claim_owned, AdmissionDecision::Dispatch),
            )
            .await
            .unwrap();

        let stale = now();
        set_enqueued_at(&db, tenant, old.id, stale).await;
        // The claim is just as old, and must still be left alone.
        set_enqueued_at(&db, tenant, claim.id, stale).await;
        set_enqueued_at(&db, tenant, fresh.id, now() + time::Duration::hours(1)).await;

        let expired = queue
            .expire_queued_before(
                &conn,
                &scope(tenant),
                stale + time::Duration::seconds(1),
                "queue TTL exceeded",
            )
            .await
            .unwrap();

        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].id, old.id);
        assert_eq!(
            expired[0].run_id, old_run.id,
            "the alert needs the run, not only the queue row"
        );
        assert_eq!(expired[0].tenant_id, tenant);
        assert_eq!(expired[0].platform_id, platform);
        assert_eq!(expired[0].enqueued_at, stale);

        assert_eq!(
            queue
                .row_status(&conn, &scope(tenant), claim.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            QueueState::Dispatching,
            "a claim of the same age must survive the sweep"
        );
        assert_eq!(
            queue
                .row_status(&conn, &scope(tenant), fresh.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            QueueState::Queued
        );
    }

    /// One corrupt row must not cost the batch its mandatory alert.
    ///
    /// The sweep used to expire first and decode `run_kind`/`source`
    /// afterwards with `?`. A single corrupt row then aborted the sweep
    /// **after** earlier rows had already been expired: the caller got an
    /// `Err` and no `ExpiredRow` for any of them, and because those rows were
    /// no longer `queued`, a retried sweep could never announce them either.
    /// Guide line 96 makes the `expired` alert mandatory, and this method's
    /// whole design claim is that a row cannot be expired without the caller
    /// holding what the alert needs — so silently expiring rows was the one
    /// outcome it must not produce.
    ///
    /// Three rows, the corrupt one in the middle so the ordering is what is
    /// under test: the two good rows are expired and announced, and the
    /// corrupt one is left `queued` for a later sweep rather than expired
    /// unannounced.
    #[tokio::test]
    async fn a_corrupt_row_costs_only_itself_and_stays_queued() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);

        let mut ids = Vec::new();
        for name in ["first", "corrupt", "last"] {
            let (_, owned) = seeded_run(&db, tenant, name).await;
            ids.push(
                queue
                    .insert(
                        &conn,
                        &scope(tenant),
                        tenant,
                        new_row(platform, owned, AdmissionDecision::Queue),
                    )
                    .await
                    .unwrap()
                    .id,
            );
        }
        // Pin `enqueued_at` to the fixture instant: `insert` stamps wall-clock
        // time, and a cutoff derived from `now()` would otherwise race it.
        for id in &ids {
            set_enqueued_at(&db, tenant, *id, now()).await;
        }
        // Corrupt the middle row's `run_kind` through the same secure update
        // the repository uses, so this is a value the storage layer accepts.
        QueueEntity::update_many()
            .filter(by_id(ids[1]))
            .secure()
            .scope_with(&scope(tenant))
            .col_expr(QueueColumn::RunKind, Expr::value("custom-plan"))
            .exec(&conn)
            .await
            .unwrap();

        let cutoff = now() + time::Duration::seconds(1);
        let expired = queue
            .expire_queued_before(&conn, &scope(tenant), cutoff, "queue TTL exceeded")
            .await
            .expect("one corrupt row must not fail the sweep");

        let announced: Vec<Uuid> = expired.iter().map(|r| r.id).collect();
        assert_eq!(
            announced,
            vec![ids[0], ids[2]],
            "both healthy rows must be expired AND announced"
        );
        for id in [ids[0], ids[2]] {
            assert_eq!(
                queue
                    .row_status(&conn, &scope(tenant), id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state,
                QueueState::Expired
            );
        }
        assert_eq!(
            queue
                .row_status(&conn, &scope(tenant), ids[1])
                .await
                .unwrap()
                .unwrap()
                .state,
            QueueState::Queued,
            "the corrupt row must be left alone: expiring it would strand it in a \
             terminal state that no later sweep can announce"
        );
    }

    /// The TTL sweep's per-row guard, on its own.
    ///
    /// `expire_queued_before` selects candidates and then writes each one, so
    /// a row can be dispatched in between — and expiring a dispatched row
    /// releases a platform claim while a live execution still holds the
    /// platform. `expire_one` is what closes that window, and this test is the
    /// only thing that covers it: the sweep's select carries the same `queued`
    /// predicate, so the end-to-end test stays green when *either* guard is
    /// deleted alone and only goes red when both are. Driving the real race
    /// would need to interpose between two statements inside one call, which
    /// this harness cannot do; testing the guard directly is the honest
    /// substitute, and it is the statement that actually runs during a sweep.
    #[tokio::test]
    async fn the_expire_sweep_refuses_a_row_that_left_queued() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);

        let (_, waiting_owned) = seeded_run(&db, tenant, "waiting").await;
        let waiting = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, waiting_owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap();
        let (_, claimed_owned) = seeded_run(&db, tenant, "claimed").await;
        let claimed = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, claimed_owned, AdmissionDecision::Dispatch),
            )
            .await
            .unwrap();

        // This is the row the select saw as queued and that was dispatched
        // before the write reached it.
        assert!(
            !super::expire_one(&conn, &scope(tenant), claimed.id, "queue TTL exceeded")
                .await
                .unwrap(),
            "expiring a row that now holds a claim must refuse: the platform is \
             owned by a live execution and releasing it admits a second run \
             beside an exclusive one"
        );
        assert_eq!(
            queue
                .row_status(&conn, &scope(tenant), claimed.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            QueueState::Dispatching
        );

        // …and a row that really is still queued is expired.
        assert!(
            super::expire_one(&conn, &scope(tenant), waiting.id, "queue TTL exceeded")
                .await
                .unwrap()
        );
        assert_eq!(
            queue
                .row_status(&conn, &scope(tenant), waiting.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            QueueState::Expired
        );

        // The same guard is scoped: another tenant cannot expire this row.
        let (_, other_owned) = seeded_run(&db, tenant, "other").await;
        let other = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, other_owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap();
        assert!(
            !super::expire_one(&conn, &scope(Uuid::new_v4()), other.id, "hijack")
                .await
                .unwrap()
        );
    }

    /// Boot recovery fails only rows still `dispatching`. The other half of
    /// legacy's predicate — "and carries no execution reference" — is
    /// `domain::state_machine::boot_recovery_action`'s and the caller's; this
    /// asserts the half that has to be in the SQL, because a row that started
    /// running between the caller's read and this write must keep its claim.
    #[tokio::test]
    async fn fail_orphaned_dispatching_only_touches_rows_still_dispatching() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);

        let (_, stuck_owned) = seeded_run(&db, tenant, "stuck").await;
        let stuck = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, stuck_owned, AdmissionDecision::Dispatch),
            )
            .await
            .unwrap();
        let (_, live_owned) = seeded_run(&db, tenant, "live").await;
        let live = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, live_owned, AdmissionDecision::Dispatch),
            )
            .await
            .unwrap();
        assert!(
            queue
                .mark_running(&conn, &scope(tenant), live.id)
                .await
                .unwrap()
        );

        // The caller nominates both; only the dispatching one is failed.
        assert_eq!(
            queue
                .fail_orphaned_dispatching(
                    &conn,
                    &scope(tenant),
                    &[stuck.id, live.id],
                    "manager restarted mid-submit",
                )
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            queue
                .row_status(&conn, &scope(tenant), stuck.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            QueueState::Failed
        );
        assert_eq!(
            queue
                .row_status(&conn, &scope(tenant), live.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            QueueState::Running,
            "a row that started running keeps its claim"
        );
        // An empty nomination is a no-op, not an empty `IN ()`.
        assert_eq!(
            queue
                .fail_orphaned_dispatching(&conn, &scope(tenant), &[], "boot")
                .await
                .unwrap(),
            0
        );
    }

    /// **The drain order is oldest-queued-first**, which is the tick's only
    /// fairness property.
    ///
    /// Without an `ORDER BY` the planner's order was arbitrary but stable in
    /// practice, so the tick spent its budget on whichever platform sorted
    /// first, every tick - the same shape as the claim scan's starvation. The
    /// ordering landed in `2283dc30` and nothing pinned it: deleting it left
    /// the whole suite green.
    ///
    /// The platforms are seeded in the *opposite* order to their queue ages, so
    /// insertion order and id order both disagree with the expected answer and
    /// neither can produce it by accident.
    #[tokio::test]
    async fn the_drain_order_is_oldest_queued_first() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;

        // Seeded newest-first; `platform_old` also has the highest id, so an
        // id-ordered or insertion-ordered answer is the reverse of this one.
        let platform_new = Uuid::from_u128(0x10);
        let platform_mid = Uuid::from_u128(0x20);
        let platform_old = Uuid::from_u128(0x30);

        let base = now();
        for (platform, age_seconds) in [
            (platform_new, 10),
            (platform_mid, 100),
            (platform_old, 1_000),
        ] {
            let (_, owned) = seeded_run(&db, tenant, &format!("run-{}", Uuid::new_v4())).await;
            let row = queue
                .insert(
                    &conn,
                    &scope(tenant),
                    tenant,
                    new_row(platform, owned, AdmissionDecision::Queue),
                )
                .await
                .unwrap();
            set_enqueued_at(
                &db,
                tenant,
                row.id,
                base - time::Duration::seconds(age_seconds),
            )
            .await;
        }

        let found = queue
            .platforms_with_queued_rows(&conn, &scope(tenant))
            .await
            .unwrap();

        assert_eq!(
            found.iter().map(|p| p.platform_id).collect::<Vec<_>>(),
            vec![platform_old, platform_mid, platform_new],
            "the platform whose head-of-queue row has waited longest must drain first"
        );
    }

    /// The dispatcher enumerates platforms across tenants and then writes
    /// under a per-tenant system context, so each entry must carry its own
    /// row's tenant — not the enumerating one's.
    #[tokio::test]
    async fn platforms_with_queued_rows_carry_their_own_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);
        let other = Uuid::from_u128(0x31);

        for (tenant, target) in [(a, platform), (a, platform), (a, other), (b, platform)] {
            let (_, owned) = seeded_run(&db, tenant, &format!("run-{}", Uuid::new_v4())).await;
            queue
                .insert(
                    &conn,
                    &scope(tenant),
                    tenant,
                    new_row(target, owned, AdmissionDecision::Queue),
                )
                .await
                .unwrap();
        }

        let mut found = queue
            .platforms_with_queued_rows(&conn, &scope(a))
            .await
            .unwrap();
        found.sort_by_key(|p| p.platform_id);
        assert_eq!(
            found,
            vec![
                QueuedPlatform {
                    platform_id: platform,
                    tenant_id: a,
                },
                QueuedPlatform {
                    platform_id: other,
                    tenant_id: a,
                },
            ],
            "two queued rows on one platform are one entry, and B's row is not A's"
        );

        assert_eq!(
            queue
                .platforms_with_queued_rows(&conn, &scope(b))
                .await
                .unwrap(),
            vec![QueuedPlatform {
                platform_id: platform,
                tenant_id: b,
            }]
        );
    }

    /// `dispatched_at` when the row was dispatched, `enqueued_at` when it was
    /// not — legacy's `dispatched_at.unwrap_or(enqueued_at)`
    /// (`manager/src/services/run_queue.rs:341-346`). Getting this backwards
    /// would age every claim from the wrong instant and make the orphan
    /// timeout fire early or never.
    #[tokio::test]
    async fn all_claims_ages_from_dispatched_at_falling_back_to_enqueued_at() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);

        // A dispatched row: `dispatched_at` is set, `enqueued_at` is forced to
        // a distinctly older instant so the two cannot be confused.
        let (_, dispatched_owned) = seeded_run(&db, tenant, "dispatched").await;
        let dispatched = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, dispatched_owned, AdmissionDecision::Dispatch),
            )
            .await
            .unwrap();
        let ancient = now() - time::Duration::days(1);
        set_enqueued_at(&db, tenant, dispatched.id, ancient).await;

        // A row promoted to running without ever being stamped dispatched:
        // its age basis falls back to `enqueued_at`.
        let (_, never_owned) = seeded_run(&db, tenant, "never-dispatched").await;
        let never = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(platform, never_owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap();
        assert!(
            queue
                .mark_running(&conn, &scope(tenant), never.id)
                .await
                .unwrap()
        );
        set_enqueued_at(&db, tenant, never.id, ancient).await;

        let claims = queue.all_claims(&conn, &scope(tenant), None).await.unwrap();
        assert!(!claims.truncated, "three rows cannot fill the scan window");
        let by_id = |id: Uuid| {
            claims
                .rows
                .iter()
                .find(|c| c.id == id)
                .expect("claim must be listed")
        };
        assert_ne!(
            by_id(dispatched.id).age_basis,
            ancient,
            "a dispatched row ages from `dispatched_at`, not `enqueued_at`"
        );
        assert_eq!(
            by_id(dispatched.id).age_basis,
            dispatched.dispatched_at.unwrap()
        );
        assert_eq!(
            by_id(never.id).age_basis,
            ancient,
            "a row with no `dispatched_at` falls back to `enqueued_at`"
        );
        assert_eq!(by_id(never.id).tenant_id, tenant);
    }

    /// The read endpoint's window: newest first, every state.
    #[tokio::test]
    async fn list_for_read_is_newest_first_across_all_states() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);
        let other = Uuid::from_u128(0x31);

        let mut ids = Vec::new();
        for (index, target) in [platform, platform, other].into_iter().enumerate() {
            let (_, owned) = seeded_run(&db, tenant, &format!("run-{index}")).await;
            ids.push(
                queue
                    .insert(
                        &conn,
                        &scope(tenant),
                        tenant,
                        new_row(target, owned, AdmissionDecision::Queue),
                    )
                    .await
                    .unwrap()
                    .id,
            );
        }
        // A terminal row still appears — legacy's window is all states.
        assert!(
            queue
                .cancel_queued(&conn, &scope(tenant), ids[0], "user asked")
                .await
                .unwrap()
        );
        set_enqueued_at(&db, tenant, ids[0], now()).await;
        set_enqueued_at(&db, tenant, ids[1], now() + time::Duration::seconds(10)).await;

        let rows = queue
            .list_for_read(&conn, &scope(tenant), Some(platform), 10)
            .await
            .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ids[1], ids[0]],
            "newest first, and a cancelled row is still in the window"
        );
        assert_eq!(rows[1].state, QueueState::Cancelled);

        // Unfiltered spans every platform; the limit truncates the window.
        assert_eq!(
            queue
                .list_for_read(&conn, &scope(tenant), None, 10)
                .await
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            queue
                .list_for_read(&conn, &scope(tenant), None, 1)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// The clamp arithmetic, free of a database.
    ///
    /// Pairs with `list_for_read_passes_its_limit_through_read_limit` below:
    /// this pins *what* the ceiling is, that one pins that the query is wired
    /// to it. Splitting them is what let the DB half shrink from
    /// `MAX_QUEUE_READ_LIMIT + 1` rows to three — the same move `expire_one`
    /// made, and for the same reason.
    #[test]
    fn read_limit_is_a_ceiling_not_a_page_size() {
        assert_eq!(super::read_limit(u64::MAX), MAX_QUEUE_READ_LIMIT);
        assert_eq!(
            super::read_limit(MAX_QUEUE_READ_LIMIT + 1),
            MAX_QUEUE_READ_LIMIT
        );
        assert_eq!(
            super::read_limit(MAX_QUEUE_READ_LIMIT),
            MAX_QUEUE_READ_LIMIT
        );
        // Below the ceiling the caller's number is honoured exactly -- a clamp
        // that always returned the maximum would pass the assertions above.
        assert_eq!(super::read_limit(7), 7);
        assert_eq!(super::read_limit(0), 0);
    }

    /// `list_for_read`'s `.limit()` is wired to [`super::read_limit`], not to
    /// the raw parameter.
    ///
    /// Three rows, not a thousand: with the ceiling's *value* pinned by the
    /// unit test above, all this has to show is that the query respects
    /// whatever that function returns. `read_limit(2)` is 2, so a query that
    /// bypassed it and used the caller's `u64::MAX` would return all three.
    #[tokio::test]
    async fn list_for_read_passes_its_limit_through_read_limit() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);

        for index in 0..3 {
            let (_, owned) = seeded_run(&db, tenant, &format!("run-{index}")).await;
            queue
                .insert(
                    &conn,
                    &scope(tenant),
                    tenant,
                    new_row(platform, owned, AdmissionDecision::Queue),
                )
                .await
                .unwrap();
        }

        assert_eq!(
            queue
                .list_for_read(&conn, &scope(tenant), Some(platform), 2)
                .await
                .unwrap()
                .len(),
            2,
            "the query must use `read_limit`'s answer, not the raw parameter"
        );
        assert_eq!(
            queue
                .list_for_read(&conn, &scope(tenant), Some(platform), u64::MAX)
                .await
                .unwrap()
                .len(),
            3,
            "and a limit above both the ceiling and the row count returns \
             everything there is"
        );
    }

    /// Every read and write on this table is tenant-scoped in its own right.
    #[tokio::test]
    async fn a_queue_row_is_invisible_to_another_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let queue = OrmQueueRepository;
        let platform = Uuid::from_u128(0x30);
        let (_, owned) = seeded_run(&db, a, "smoke-1").await;
        let row = queue
            .insert(
                &conn,
                &scope(a),
                a,
                new_row(platform, owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap();

        assert!(
            queue
                .row_status(&conn, &scope(b), row.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            queue
                .list_for_read(&conn, &scope(b), None, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            queue
                .all_claims(&conn, &scope(b), None)
                .await
                .unwrap()
                .rows
                .is_empty()
        );
        assert!(
            !queue
                .cancel_queued(&conn, &scope(b), row.id, "hijack")
                .await
                .unwrap()
        );
        assert!(
            !queue
                .mark_dispatching(&conn, &scope(b), row.id)
                .await
                .unwrap()
        );
        assert_eq!(
            queue
                .row_status(&conn, &scope(a), row.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            QueueState::Queued,
            "B's attempts must have changed nothing"
        );
    }

    /// **The boundary, stated as an executable argument.**
    ///
    /// `secure_insert` validates the `tenant_id` column of the row being
    /// written and nothing else. It cannot know that the *referenced* run
    /// belongs to the same tenant, and the foreign key is tenant-blind — so a
    /// queue row pointing at another tenant's run is accepted by this
    /// repository, and would be by any repository. Asserting that here is what
    /// stops the gap being "fixed" in the wrong layer.
    ///
    /// What closes it is [`OwnedRunId`]: the only way to obtain one is
    /// `RunsRepository::resolve_owned` under the caller's own scope, which is
    /// what `resolve_owned_refuses_a_run_this_scope_cannot_see` pins. So the
    /// unreachable-in-production insert below can only be written by a test
    /// that first resolved the run as its owner — and the foreign key never
    /// gets asked the existence question in the first place.
    #[tokio::test]
    async fn the_token_is_what_stops_a_cross_tenant_run_id_not_the_foreign_key() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let (victim, squatter) = (Uuid::new_v4(), Uuid::new_v4());
        let queue = OrmQueueRepository;
        let runs = OrmRunsRepository;

        let (run, _) = seeded_run(&db, victim, "victims-run").await;

        // The squatter cannot even name the run: this is the precheck, and it
        // is why the insert below is unreachable from a service.
        assert!(
            matches!(
                runs.resolve_owned(&conn, &scope(squatter), run.id).await,
                Err(DomainError::RunNotFound { .. })
            ),
            "the ownership precheck must refuse before any write is attempted"
        );

        // Minted as the victim, then used under the squatter's scope — the
        // shape a service would produce if it skipped the precheck. The
        // repository accepts it, which is the point being recorded.
        let owned = runs
            .resolve_owned(&conn, &scope(victim), run.id)
            .await
            .unwrap();
        let filed = queue
            .insert(
                &conn,
                &scope(squatter),
                squatter,
                new_row(Uuid::from_u128(0x30), owned, AdmissionDecision::Queue),
            )
            .await
            .expect(
                "secure_insert validates only this row's tenant_id, so a foreign \
                 run_id is accepted here; the precheck above is what prevents it",
            );
        assert_eq!(filed.tenant_id, squatter);
        // …and the victim never sees it, so the row is inert rather than
        // harmful: `idx_qa_run_queue_tenant_run` is tenant-prefixed.
        assert!(
            queue
                .list_for_read(&conn, &scope(victim), None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// A corrupt queue state fails the read rather than defaulting — the same
    /// rule as `qa_runs.state`, on the vocabulary that spells cancel with two
    /// `l`s.
    #[tokio::test]
    async fn a_corrupt_queue_state_fails_the_read_rather_than_defaulting() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let queue = OrmQueueRepository;
        let (_, owned) = seeded_run(&db, tenant, "smoke-1").await;
        let row = queue
            .insert(
                &conn,
                &scope(tenant),
                tenant,
                new_row(Uuid::from_u128(0x30), owned, AdmissionDecision::Queue),
            )
            .await
            .unwrap();

        QueueEntity::update_many()
            .filter(by_id(row.id))
            .secure()
            .scope_with(&scope(tenant))
            // The *run*'s spelling, which is not this table's vocabulary.
            .col_expr(QueueColumn::State, Expr::value("canceled"))
            .exec(&conn)
            .await
            .unwrap();

        let err = queue
            .row_status(&conn, &scope(tenant), row.id)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                DomainError::CorruptState { what: "queue.state", id, .. } if id == row.id
            ),
            "got {err:?}"
        );
        assert!(
            queue
                .list_for_read(&conn, &scope(tenant), None, 10)
                .await
                .is_err()
        );
    }
}
