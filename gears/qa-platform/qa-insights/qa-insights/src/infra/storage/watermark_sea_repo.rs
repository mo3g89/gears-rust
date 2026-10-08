//! `SecureORM` implementation of [`WatermarkRepository`].
//!
//! # Monotonicity is enforced in the `WHERE` clause, not read-then-compared
//!
//! `advance` must never move a mark backwards, and the obvious implementation —
//! read, compare, write — has a window in which two leaders overlapping across a
//! failover both read the old value and the later one loses. What closes it is
//! doing the comparison *in the statement*: an `UPDATE … WHERE col IS NULL OR
//! col < at` can only ever move the mark forward, whichever order the two
//! statements interleave in.
//!
//! # Why not `GREATEST` in an `ON CONFLICT DO UPDATE`
//!
//! One statement instead of two, and it *is* buildable —
//! `SecureOnConflict::value` takes a `SimpleExpr`, so `Expr::cust` could carry
//! any expression. It was not used for three reasons, none of which is
//! "impossible":
//!
//! 1. **The function name is dialect-specific.** Postgres has `GREATEST`;
//!    `SQLite` spells the scalar form `MAX`. Choosing between them needs the
//!    backend, which is not reachable through the `DBRunner` bound these methods
//!    take — measured by a compile attempt, see `notify_sea_repo`'s header.
//! 2. **So is the `excluded` pseudo-table.** `sea-query` renders it as
//!    `"excluded"` generally (`backend/query_builder.rs:1312-1318`) and as
//!    `VALUES(col)` on `MySQL` (`backend/mysql/query.rs:190-194`), so an
//!    `Expr::cust` string naming `excluded` would be a third dialect assumption
//!    baked into hand-written SQL text.
//! 3. **`NULL` handling would need a `CASE`.** `col < at` is `NULL`, not true,
//!    when `col IS NULL`, so the guard has to say so either way.
//!
//! The two-statement form needs none of that, and its monotonicity is visible in
//! the predicate rather than inside a string — which is the property worth
//! optimising for here, since a silent rewind is the whole failure mode.

use async_trait::async_trait;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
    validate_tenant_in_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{SweepCursor, WatermarkKind, WatermarkRepository, Watermarks};
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::ingest_watermark::{
    self, Column as MarkColumn, Entity as MarkEntity,
};
use crate::infra::storage::mapper::watermarks_from_row;

/// ORM-based implementation of the `WatermarkRepository` trait.
#[derive(Clone, Default)]
pub struct OrmWatermarkRepository;

impl WatermarkKind {
    /// The column this kind writes.
    ///
    /// Kept as a mapping rather than inlined so a second kind, if one ever
    /// returns, has exactly one place that says which column it writes.
    fn column(self) -> MarkColumn {
        match self {
            Self::ReconciledFinishedAt => MarkColumn::LastReconciledFinishedAt,
        }
    }
}

#[async_trait]
impl WatermarkRepository for OrmWatermarkRepository {
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Watermarks, DomainError> {
        let row = MarkEntity::find()
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        // `Watermarks::default` for a tenant with no row, so a caller never has
        // to distinguish "no row" from "row with nulls" — both mean *never*.
        Ok(row.as_ref().map(watermarks_from_row).unwrap_or_default())
    }

    async fn advance<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        kind: WatermarkKind,
        at: OffsetDateTime,
    ) -> Result<(), DomainError> {
        // Required rather than defensive, and it became so with the fix below:
        // the insert now goes through `scope_unchecked`, which performs no
        // validation, where `secure_insert` used to run `validate_insert_scope`
        // for us. Without this a caller could create another tenant's watermark
        // row.
        validate_tenant_in_scope(tenant_id, scope).map_err(db_err)?;
        let column = kind.column();

        // Attempt 1: the conditional update. This is the whole monotonicity
        // guarantee — the predicate is what makes a backwards `at` a no-op
        // rather than a rewind.
        if advance_existing(runner, scope, column, at).await? {
            return Ok(());
        }

        // Nothing matched: either the tenant has no row, or its mark is already
        // at or past `at`. Creating the row is only right in the first case, so a
        // conflict on `idx_qa_ingest_watermarks_tenant` means the second — or a
        // concurrent writer that created it between the two statements — and the
        // repair is to re-run the conditional update, which is safe to repeat.
        //
        // **`DO NOTHING`, not a plain insert, and that distinction is the fix for
        // a critical defect.** This was `secure_insert`, which raises. The
        // *supported no-op* — advancing to an instant the mark is already at or
        // past, which is what an idempotent replay produces and what
        // `advancing_a_mark_backwards_is_a_successful_no_op` pins — reaches this
        // line every time, so on Postgres the insert raised `23505` and **aborted
        // the caller's transaction**. `advance_existing` below then failed
        // `25P02`, so `advance` returned `Database` for a call whose correct
        // answer is `Ok(())`, and the caller lost a transaction it was still
        // using. It does not self-heal: retrying reproduces it.
        //
        // Identical in shape to `notify_sea_repo::claim_notification`, whose
        // header argues the same point — the defect was that argument existing
        // one file over and not being applied here. `SQLite` cannot see it (a
        // failed statement does not abort), which is why
        // `an_advance_no_op_inside_a_transaction_leaves_the_transaction_usable`
        // runs on a real Postgres.
        let now = OffsetDateTime::now_utc();
        let am = ingest_watermark::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(tenant_id),
            last_reconciled_finished_at: ActiveValue::Set(match kind {
                WatermarkKind::ReconciledFinishedAt => Some(at),
            }),
            // A row created by an advance carries no resume cursor, and that
            // is the right value rather than a default worth thinking about:
            // `set_sweep_cursor` is the only writer of these three, the sweep
            // calls it after this, and a cursor invented here would name a
            // window no pass walked.
            sweep_cursor_at: ActiveValue::Set(None),
            sweep_cursor_run_id: ActiveValue::Set(None),
            sweep_cursor_floor: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };

        // The target is named — `(tenant_id)`, the columns of
        // `idx_qa_ingest_watermarks_tenant` — rather than left bare. A targetless
        // `ON CONFLICT DO NOTHING` swallows *every* constraint violation on the
        // table, including one on the UUID primary key, which would turn a
        // genuine bug into a silent no-op.
        let insert = MarkEntity::insert(am)
            .secure()
            .scope_unchecked(scope)
            .map_err(db_err)?
            .on_conflict_raw(
                OnConflict::columns([MarkColumn::TenantId])
                    .do_nothing()
                    .to_owned(),
            )
            .exec(runner)
            .await;

        match insert {
            Ok(_) => Ok(()),
            // The row already existed: `DO NOTHING` wrote nothing and the
            // transaction is intact, so the conditional update can now run.
            // `is_unique_violation` is the belt to that braces, for a backend
            // that raises where these two do not.
            Err(ScopeError::Db(sea_orm::DbErr::RecordNotInserted)) => {
                advance_existing(runner, scope, column, at).await?;
                Ok(())
            }
            Err(e) if e.is_unique_violation() => {
                advance_existing(runner, scope, column, at).await?;
                Ok(())
            }
            Err(e) => Err(db_err(e)),
        }
    }

    /// One scoped `UPDATE` writing all three cursor columns together.
    ///
    /// # No monotonic predicate, and that is the contract rather than an
    /// # omission
    ///
    /// [`Self::advance`] above puts its whole guarantee in a `WHERE` clause
    /// because a mark that rewinds is unbounded work. This statement carries no
    /// such predicate *on purpose*: the cursor is a within-window resume point
    /// that the sweep must be able to move backwards and to erase, and a
    /// forward-only one would be a second watermark with the lookback dead
    /// behind it. [`SweepCursor`]'s own doc carries why last-writer-wins is
    /// safe when two replicas share the row.
    ///
    /// # The three columns move as one
    ///
    /// Written in one statement, `None` writing `NULL` into all three, so no
    /// interleaving of two replicas' writes can leave a row holding one
    /// replica's instant beside another's run id. `mapper::watermarks_from_row`
    /// folds a partial row to `None` as the second line of that defence.
    ///
    /// # Bound, never formatted
    ///
    /// `Expr::value` hands `sea_query` the `Uuid` and the two instants and lets
    /// the driver encode them for whichever dialect this runner is. A `Uuid`
    /// formatted into SQL text reads back on `SQLite` as a length error and
    /// *compares* as a non-match — see
    /// `m20260929_000006_ingest_watermarks_sweep_cursor`'s header, and
    /// `m20260929_000004_run_completed_notification_cutoff`'s for the round
    /// this argument was first paid for.
    async fn set_sweep_cursor<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        cursor: Option<SweepCursor>,
    ) -> Result<(), DomainError> {
        // Destructured into three `Option`s rather than three `map` calls at
        // the point of use, so "all three or none of them" is visible in one
        // line instead of being a property of three separate expressions.
        let (floor, at, run_id) = cursor.map_or((None, None, None), |c| {
            (Some(c.window_floor), Some(c.at), Some(c.run_id))
        });

        MarkEntity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(MarkColumn::SweepCursorFloor, Expr::value(floor))
            .col_expr(MarkColumn::SweepCursorAt, Expr::value(at))
            .col_expr(MarkColumn::SweepCursorRunId, Expr::value(run_id))
            .col_expr(
                MarkColumn::UpdatedAt,
                Expr::value(OffsetDateTime::now_utc()),
            )
            .exec(runner)
            .await
            .map_err(db_err)?;
        // Zero rows affected is not an error: a tenant with no watermark row
        // has no window to resume inside, and clearing a cursor that does not
        // exist is what the caller asked for.
        Ok(())
    }
}

/// Move one column forward if and only if `at` is ahead of what is stored.
///
/// `col IS NULL` is the first disjunct because `NULL < at` is `NULL` in SQL, not
/// true: without it, a row created by the *other* kind's advance would keep this
/// mark at `NULL` forever, and the reconcile poller would re-read its whole
/// lookback window on every pass.
///
/// Returns whether a row moved.
async fn advance_existing<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    column: MarkColumn,
    at: OffsetDateTime,
) -> Result<bool, DomainError> {
    let result = MarkEntity::update_many()
        .secure()
        .scope_with(scope)
        .filter(Condition::any().add(column.is_null()).add(column.lt(at)))
        .col_expr(column, Expr::value(at))
        .col_expr(
            MarkColumn::UpdatedAt,
            Expr::value(OffsetDateTime::now_utc()),
        )
        .exec(runner)
        .await
        .map_err(db_err)?;
    Ok(result.rows_affected > 0)
}

#[cfg(test)]
mod tests {
    use time::Duration;
    use uuid::Uuid;

    use crate::domain::repos::{
        SweepCursor, WatermarkKind, WatermarkRepository, Watermarks,
    };
    use crate::infra::storage::test_db::{inmem_db, now, scope};
    use crate::infra::storage::watermark_sea_repo::OrmWatermarkRepository;

    fn cursor_at(at: time::OffsetDateTime, run: u128) -> SweepCursor {
        SweepCursor {
            window_floor: at - Duration::hours(1),
            at,
            run_id: Uuid::from_u128(run),
        }
    }

    /// A tenant that has never been reconciled reads back as "never", and
    /// **never is not the epoch**: "reconciled up to 1970" would make the first
    /// pass read every run ever finished.
    #[tokio::test]
    async fn a_tenant_with_no_row_reads_back_as_never() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);

        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &scope(tenant))
                .await
                .unwrap(),
            Watermarks::default(),
            "no row and a row of nulls must be indistinguishable"
        );
    }

    /// **`tenant_id` is a parameter, and the guard is what stops it being a free
    /// one.**
    ///
    /// Added 2026-08-20 after break-verification: deleting
    /// `validate_tenant_in_scope` from `advance` left the whole suite green. The
    /// guard was redundant while the insert went through `secure_insert` (which
    /// runs `validate_insert_scope` for you); fixing the transaction-poisoning
    /// defect moved it to `scope_unchecked`, which performs **no** validation —
    /// so the same edit that fixed one defect made an untested line
    /// load-bearing. Without it a caller could create or move another tenant's
    /// watermark row, and the reconciler would then read a mark it did not write.
    #[tokio::test]
    async fn advancing_a_mark_for_a_tenant_outside_the_scope_is_refused() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);

        let outcome = OrmWatermarkRepository
            .advance(
                &conn,
                &scope(mine),
                theirs,
                WatermarkKind::ReconciledFinishedAt,
                now(),
            )
            .await;
        assert!(
            outcome.is_err(),
            "advancing another tenant's mark must be refused: {outcome:?}"
        );
        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &scope(theirs))
                .await
                .unwrap(),
            Watermarks::default(),
            "and nothing must have been written"
        );
    }

    /// **Advance, not set: the mark must never move backwards.**
    ///
    /// Two leaders overlapping across a failover, or a reconcile pass finishing
    /// out of order, would otherwise rewind the mark and make the next pass
    /// replay a window that was already ingested. Re-ingest is idempotent, so a
    /// rewind is not corrupting — it is unbounded work, growing with every
    /// rewind. A call with an `at` behind the stored value is a successful no-op.
    #[tokio::test]
    async fn advancing_a_mark_backwards_is_a_successful_no_op() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);
        let later = now();
        let earlier = later - Duration::hours(1);

        OrmWatermarkRepository
            .advance(
                &conn,
                &ctx,
                tenant,
                WatermarkKind::ReconciledFinishedAt,
                later,
            )
            .await
            .unwrap();
        OrmWatermarkRepository
            .advance(
                &conn,
                &ctx,
                tenant,
                WatermarkKind::ReconciledFinishedAt,
                earlier,
            )
            .await
            .expect("a backwards advance is a no-op, not an error");

        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &ctx)
                .await
                .unwrap()
                .last_reconciled_finished_at,
            Some(later),
            "the mark must not have rewound"
        );
    }

    /// **A no-op advance must not poison the caller's transaction.**
    ///
    /// This is the test the critical defect was invisible without. `advance`'s
    /// supported no-op — advancing to an instant the mark is already at or past —
    /// always reaches the row-creating insert, because the conditional update
    /// matches nothing. While that insert was a plain `secure_insert`, Postgres
    /// raised `23505` and aborted the transaction; `advance_existing` then failed
    /// `25P02`, `advance` returned `Database` for a call whose correct answer is
    /// `Ok(())`, and the caller lost a transaction it was still using.
    ///
    /// **The five `SQLite` tests in this module cannot see it**: they run on a
    /// bare connection, and `SQLite` does not abort a transaction on a failed
    /// statement. `WatermarkRepository::advance` takes `&impl DBRunner` precisely
    /// so Task 15's reconciler can call it inside a transaction, which is the
    /// caller that would have hit this.
    ///
    /// Shaped after `notify_sea_repo`'s
    /// `a_lost_claim_inside_a_transaction_leaves_the_transaction_usable`, which
    /// documents the identical hazard for the identical reason.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn an_advance_no_op_inside_a_transaction_leaves_the_transaction_usable() {
        use crate::domain::error::DomainError;
        use crate::infra::storage::test_db::pg_db;

        let harness = pg_db().await;
        let provider = std::sync::Arc::new(toolkit_db::DBProvider::<DomainError>::new(
            harness.db.clone(),
        ));
        let tenant = Uuid::from_u128(0xA);
        let later = now();
        let earlier = later - Duration::hours(1);

        // Seed the row outside the transaction, so the transaction's first
        // `advance` is the no-op.
        OrmWatermarkRepository
            .advance(
                &harness.db.conn().unwrap(),
                &scope(tenant),
                tenant,
                WatermarkKind::ReconciledFinishedAt,
                later,
            )
            .await
            .unwrap();

        let outcome: Result<(), DomainError> = provider
            .transaction(move |tx| {
                Box::pin(async move {
                    let ctx = scope(tenant);
                    // Backwards: the no-op. Must succeed and leave `tx` usable.
                    OrmWatermarkRepository
                        .advance(
                            tx,
                            &ctx,
                            tenant,
                            WatermarkKind::ReconciledFinishedAt,
                            earlier,
                        )
                        .await?;
                    // Equal: the other route to the same no-op, which an
                    // idempotent replay produces.
                    OrmWatermarkRepository
                        .advance(tx, &ctx, tenant, WatermarkKind::ReconciledFinishedAt, later)
                        .await?;
                    // The point: more work in the same transaction. With a
                    // raising insert this fails with `current transaction is
                    // aborted`.
                    OrmWatermarkRepository
                        .advance(
                            tx,
                            &ctx,
                            tenant,
                            WatermarkKind::ReconciledFinishedAt,
                            later + Duration::hours(2),
                        )
                        .await?;
                    Ok(())
                })
            })
            .await;

        assert!(
            outcome.is_ok(),
            "a no-op advance must not poison the caller's transaction: {outcome:?}"
        );

        let marks = OrmWatermarkRepository
            .get(&harness.db.conn().unwrap(), &scope(tenant))
            .await
            .unwrap();
        assert_eq!(
            marks.last_reconciled_finished_at,
            Some(later + Duration::hours(2)),
            "the mark must not have rewound, and the forward advance made after the two \
             no-ops must have committed"
        );
    }

    /// Advancing forwards moves the mark, and creating the row on first advance
    /// is part of the contract.
    #[tokio::test]
    async fn advancing_a_mark_forwards_moves_it_and_creates_the_row() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);
        let first = now();
        let second = first + Duration::hours(1);

        OrmWatermarkRepository
            .advance(
                &conn,
                &ctx,
                tenant,
                WatermarkKind::ReconciledFinishedAt,
                first,
            )
            .await
            .unwrap();
        OrmWatermarkRepository
            .advance(
                &conn,
                &ctx,
                tenant,
                WatermarkKind::ReconciledFinishedAt,
                second,
            )
            .await
            .unwrap();

        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &ctx)
                .await
                .unwrap()
                .last_reconciled_finished_at,
            Some(second)
        );
    }

    /// The cursor round-trips through the row, and `None` **erases** it.
    ///
    /// The erase is the half worth a test of its own: a caught-up sweep clears
    /// the cursor on every pass, and a `set_sweep_cursor(None)` that quietly
    /// left the old value standing would make the resume point a second
    /// watermark — the lookback would never be re-walked and a run written
    /// behind the cursor would be lost for good.
    #[tokio::test]
    async fn a_sweep_cursor_round_trips_and_is_erased_by_none() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);
        let cursor = cursor_at(now(), 0xC1);

        OrmWatermarkRepository
            .advance(&conn, &ctx, tenant, WatermarkKind::ReconciledFinishedAt, now())
            .await
            .unwrap();
        OrmWatermarkRepository
            .set_sweep_cursor(&conn, &ctx, Some(cursor))
            .await
            .unwrap();

        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &ctx)
                .await
                .unwrap()
                .sweep_cursor,
            Some(cursor),
            "all three columns must come back as one cursor"
        );

        OrmWatermarkRepository
            .set_sweep_cursor(&conn, &ctx, None)
            .await
            .unwrap();
        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &ctx)
                .await
                .unwrap()
                .sweep_cursor,
            None,
            "`None` erases; anything else makes this a second, monotonic mark"
        );
    }

    /// **The cursor may move backwards, and that is the contract**, not an
    /// accident of the implementation.
    ///
    /// [`WatermarkRepository::advance`] above is monotonic in its `WHERE`
    /// clause and must stay so. This one must *not* be: two replicas share the
    /// row under `NoopLeaderElector` and the sweep has to be able to rewind and
    /// erase. Adding the tempting `WHERE sweep_cursor_at < at` predicate here
    /// turns the resume point into a second watermark and kills the lookback;
    /// this is the assertion that goes red if anyone does.
    #[tokio::test]
    async fn a_sweep_cursor_may_move_backwards() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);
        let later = cursor_at(now(), 0xC1);
        let earlier = cursor_at(now() - Duration::hours(2), 0xC2);

        OrmWatermarkRepository
            .advance(&conn, &ctx, tenant, WatermarkKind::ReconciledFinishedAt, now())
            .await
            .unwrap();
        OrmWatermarkRepository
            .set_sweep_cursor(&conn, &ctx, Some(later))
            .await
            .unwrap();
        OrmWatermarkRepository
            .set_sweep_cursor(&conn, &ctx, Some(earlier))
            .await
            .unwrap();

        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &ctx)
                .await
                .unwrap()
                .sweep_cursor,
            Some(earlier),
            "last writer wins; a rewind costs repeated work, which every write on \
             the sweep's path is idempotent under"
        );
    }

    /// A cursor written under one tenant's scope is invisible to another, and
    /// writing under a scope that matches no row changes nothing.
    ///
    /// `set_sweep_cursor` takes no `tenant_id` — unlike `advance`, it creates
    /// no row — so the scope is the *only* thing keeping it inside the tenant.
    #[tokio::test]
    async fn a_sweep_cursor_is_scoped_to_its_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);
        let cursor = cursor_at(now(), 0xC1);

        OrmWatermarkRepository
            .advance(
                &conn,
                &scope(mine),
                mine,
                WatermarkKind::ReconciledFinishedAt,
                now(),
            )
            .await
            .unwrap();
        OrmWatermarkRepository
            .advance(
                &conn,
                &scope(theirs),
                theirs,
                WatermarkKind::ReconciledFinishedAt,
                now(),
            )
            .await
            .unwrap();

        OrmWatermarkRepository
            .set_sweep_cursor(&conn, &scope(mine), Some(cursor))
            .await
            .unwrap();

        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &scope(theirs))
                .await
                .unwrap()
                .sweep_cursor,
            None,
            "another tenant's row must be untouched"
        );
    }

    /// Writing a cursor for a tenant with no row at all is a successful no-op.
    ///
    /// `ReconcileService::sweep` calls this after the advance, so in practice a
    /// row exists — but the one case where it does not is a first pass that
    /// consumed nothing, and answering `Err` there would turn "there is no
    /// window to resume inside" into a failed sweep.
    #[tokio::test]
    async fn setting_a_cursor_for_a_tenant_with_no_row_is_a_no_op() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);

        OrmWatermarkRepository
            .set_sweep_cursor(&conn, &scope(tenant), Some(cursor_at(now(), 0xC1)))
            .await
            .expect("no row is not an error");

        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &scope(tenant))
                .await
                .unwrap(),
            Watermarks::default(),
            "and nothing was created"
        );
    }

    /// **The same round trip on a real Postgres**, because the deployed dialect
    /// is the one `m20260929_000006_ingest_watermarks_sweep_cursor` has to be
    /// accepted by.
    ///
    /// The unit tier runs `SQLite`, where a `Uuid` lands as a BLOB in a
    /// `TEXT`-affinity column and an instant lands as text; Postgres has real
    /// `UUID` and `TIMESTAMPTZ` types and a stricter `ALTER TABLE`. A migration
    /// green only on `SQLite` is not proven — which is the lesson
    /// `m20260929_000004_run_completed_notification_cutoff`'s header records
    /// from the opposite direction, where the defect shipped green on Postgres.
    ///
    /// Gated on `integration` so a default `cargo test` needs no Docker daemon.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn the_sweep_cursor_round_trips_on_postgres() {
        use crate::infra::storage::test_db::pg_db;

        let harness = pg_db().await;
        let conn = harness.db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);
        let cursor = cursor_at(now(), 0xC1);

        OrmWatermarkRepository
            .advance(&conn, &ctx, tenant, WatermarkKind::ReconciledFinishedAt, now())
            .await
            .unwrap();
        OrmWatermarkRepository
            .set_sweep_cursor(&conn, &ctx, Some(cursor))
            .await
            .expect("the Postgres DDL must accept the bound cursor write");

        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &ctx)
                .await
                .unwrap()
                .sweep_cursor,
            Some(cursor),
            "the three columns the migration adds must round-trip on the deployed \
             dialect, not only on the unit tier's"
        );

        // A rewind, then an erase: the two writes the sweep depends on and the
        // two a monotonic predicate would silently swallow.
        let earlier = cursor_at(now() - Duration::hours(2), 0xC2);
        OrmWatermarkRepository
            .set_sweep_cursor(&conn, &ctx, Some(earlier))
            .await
            .unwrap();
        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &ctx)
                .await
                .unwrap()
                .sweep_cursor,
            Some(earlier)
        );

        OrmWatermarkRepository
            .set_sweep_cursor(&conn, &ctx, None)
            .await
            .unwrap();
        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &ctx)
                .await
                .unwrap()
                .sweep_cursor,
            None
        );
    }

    /// One row per tenant, and one tenant's marks are invisible to another.
    #[tokio::test]
    async fn marks_are_per_tenant() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let mine = Uuid::from_u128(0xA);
        let theirs = Uuid::from_u128(0xB);

        OrmWatermarkRepository
            .advance(
                &conn,
                &scope(mine),
                mine,
                WatermarkKind::ReconciledFinishedAt,
                now(),
            )
            .await
            .unwrap();

        assert_eq!(
            OrmWatermarkRepository
                .get(&conn, &scope(theirs))
                .await
                .unwrap(),
            Watermarks::default(),
            "another tenant must see nothing"
        );
    }
}
