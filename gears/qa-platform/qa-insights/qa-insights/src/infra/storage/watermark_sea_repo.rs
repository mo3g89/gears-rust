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
use crate::domain::repos::{WatermarkKind, WatermarkRepository, Watermarks};
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
    /// Both arms are named, and the mapping is the whole reason
    /// `the_two_marks_are_independent_columns_of_one_row` exists: a `SweptAt`
    /// arm returning `LastReconciledFinishedAt` compiles, is correct Rust, and
    /// would make the reconcile poller replay from the sweep's clock.
    fn column(self) -> MarkColumn {
        match self {
            Self::ReconciledFinishedAt => MarkColumn::LastReconciledFinishedAt,
            Self::SweptAt => MarkColumn::LastSweptAt,
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
                WatermarkKind::SweptAt => None,
            }),
            last_swept_at: ActiveValue::Set(match kind {
                WatermarkKind::ReconciledFinishedAt => None,
                WatermarkKind::SweptAt => Some(at),
            }),
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

    use crate::domain::repos::{WatermarkKind, WatermarkRepository, Watermarks};
    use crate::infra::storage::test_db::{inmem_db, now, scope};
    use crate::infra::storage::watermark_sea_repo::OrmWatermarkRepository;

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
                            WatermarkKind::SweptAt,
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
            Some(later),
            "the mark must not have rewound"
        );
        assert_eq!(
            marks.last_swept_at,
            Some(later + Duration::hours(2)),
            "and the work done after the two no-ops must have committed"
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

    /// **The two marks share a row and must not overwrite each other.**
    ///
    /// This is the arm-mixing failure the [`WatermarkKind`] enum exists to make
    /// visible: an `advance(SweptAt, …)` that wrote
    /// `last_reconciled_finished_at` would compile, would pass every test that
    /// only ever set one mark, and would make the reconcile poller replay from
    /// the sweep's clock.
    #[tokio::test]
    async fn the_two_marks_are_independent_columns_of_one_row() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::from_u128(0xA);
        let ctx = scope(tenant);
        let reconciled = now();
        let swept = reconciled + Duration::hours(2);

        OrmWatermarkRepository
            .advance(
                &conn,
                &ctx,
                tenant,
                WatermarkKind::ReconciledFinishedAt,
                reconciled,
            )
            .await
            .unwrap();
        OrmWatermarkRepository
            .advance(&conn, &ctx, tenant, WatermarkKind::SweptAt, swept)
            .await
            .unwrap();

        assert_eq!(
            OrmWatermarkRepository.get(&conn, &ctx).await.unwrap(),
            Watermarks {
                last_reconciled_finished_at: Some(reconciled),
                last_swept_at: Some(swept),
            },
            "each kind must write its own column and leave the other alone"
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
