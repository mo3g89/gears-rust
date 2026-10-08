//! Ingest high-water marks — the state that survives a deploy.

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// One tenant's ingest marks.
///
/// `None` for a tenant that has never been reconciled — which is
/// also what [`WatermarkRepository::get`] returns when no row exists at all, so
/// a caller never has to distinguish "no row" from "row with nulls". `None`
/// means *never*, and is deliberately distinct from the epoch: "reconciled up
/// to 1970" would make the first pass read every run ever finished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Watermarks {
    /// How far Task 15's reconcile poller has read. On `None` the first pass
    /// uses the configured lookback window instead.
    pub last_reconciled_finished_at: Option<OffsetDateTime>,
    /// Where the last sweep stopped **inside** the window it was walking, when
    /// it stopped short of catching up. `None` — the steady state — means the
    /// next pass derives its start from the mark and the lookback, as it always
    /// has. See [`SweepCursor`].
    pub sweep_cursor: Option<SweepCursor>,
}

/// A sweep's **within-window resume point**: where a pass that ran out of page
/// budget left off, so the next tick continues rather than repeating it.
///
/// # This is not a second watermark, and [`Self::window_floor`] is why
///
/// The sweep starts each tick at `floor = mark - lookback`, deliberately behind
/// the mark, because a run can be *written* with a `finished_at` earlier than
/// one already swept. A window holding more runs than one pass' page budget
/// therefore lets a pass spend every page without reaching past the mark, where
/// [`WatermarkRepository::advance`] is monotonic and the advance is a successful
/// no-op — so the next tick derives the identical floor and does the identical
/// work, forever. Second review, finding #122;
/// `domain::service::reconcile`'s `MAX_PAGES_PER_SWEEP` carries the account and
/// the outage it is a cousin of.
///
/// A resume point that only ever moved forward would fix that and break the
/// lookback with it: a late-written run behind the cursor would never be read
/// again. So this cursor vouches for a *range*, `[window_floor, at]`, and is
/// honoured only by a pass whose own floor lies inside it: stored
/// [`Self::window_floor`] at or before the reading pass' floor, and [`Self::at`]
/// at or after it. A mark that moved forward during a drain keeps the cursor;
/// a reader with a wider lookback (an earlier floor) discards it, because the
/// cursor says nothing about the band before its own floor. And the sweep
/// **clears** the cursor on any pass that catches up, so the tick after a drain
/// finishes is back to `mark - lookback`.
///
/// **The known cost** is that while a drain resumes across a moving floor, the
/// band behind the cursor is not re-read, so the effective late-arrival
/// lookback shrinks by however far the drain moved the mark. A run written
/// later than the pass that listed its band, and older than
/// `mark_at_catch_up - lookback`, is recovered only by
/// `POST /qa/v1/insights/rebuild`. `domain::service::reconcile`'s
/// `first_page_of` carries the argument.
///
/// # It is advisory, and that is what makes concurrent replicas safe
///
/// Leader election in this gear is an optimisation rather than mutual exclusion
/// (`crate::infra::leader`'s header), and under the shipped `NoopLeaderElector`
/// every replica is the leader — so two replicas sweep one tenant against one
/// cursor row. The write is a plain last-writer-wins `UPDATE`, with no
/// monotonic predicate, and that is safe because of what the value *is*: every
/// cursor written names a run its writer **fully consumed**, and a range
/// `[window_floor, at]` every listed run of which some pass consumed (the
/// writer, or the pass whose cursor the writer honoured, which the honour rule
/// only allows when that pass' range covers the writer's floor). So a replica
/// overwriting another's cursor can only ever move it to a position some pass
/// genuinely reached, or backwards to an earlier one. Backwards costs repeated
/// work — which every write on this path is idempotent under — and can never
/// skip a run. There is no interleaving in which a run between two replicas'
/// positions goes unread, because neither replica ever writes a position it did
/// not walk to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SweepCursor {
    /// The floor the pass that wrote this cursor was walking from.
    ///
    /// A reading pass honours the cursor only when this is **at or before**
    /// the floor it derives (and [`Self::at`] at or after it). This was an
    /// equality test until finding #122's residual A: equality discarded the
    /// cursor on the very tick a drain moved the mark, and the pass re-walked
    /// ground already consumed. `<=` and not `>=` because a cursor vouches for
    /// nothing before its own floor, so a reader with an earlier floor must
    /// walk that band itself.
    pub window_floor: OffsetDateTime,
    /// `finished_at` of the last run that pass fully consumed.
    pub at: OffsetDateTime,
    /// That run's id. With [`Self::at`] this is the `(finished_at, id)` key
    /// `qa_runs_sdk::FinishedRunCursor` already pages on, so resuming is
    /// `FinishedRunCursor::after` and nothing else — the walk has one notion of
    /// position, not two.
    pub run_id: Uuid,
}

/// Which mark [`WatermarkRepository::advance`] moves.
///
/// An enum rather than a bare method so that a second mark, should one ever
/// return, arrives as a variant sharing this row and upsert instead of as a
/// second method that is the same statement with one column name changed. It
/// has one variant today: a second mark (`last_swept_at`, for a stale-in-progress
/// sweep that never existed) was dropped by `m20260929_000005_drop_ingest_watermarks_last_swept_at`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatermarkKind {
    /// [`Watermarks::last_reconciled_finished_at`].
    ReconciledFinishedAt,
}

/// Persistence for `qa_ingest_watermarks`.
///
/// **New in this port; no legacy original.** The reconcile sweep re-reads
/// qa-runs on its own cadence, a periodic reconcile that needs a durable mark
/// of how far it has already gotten — held in memory, that mark would restart
/// at zero on every deploy — which is the failure this table exists to
/// prevent (Task 15).
#[async_trait]
pub trait WatermarkRepository: Send + Sync {
    /// The tenant's marks, or [`Watermarks::default`] when no row exists.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Watermarks, DomainError>;

    /// Move one mark forward to `at`, creating the tenant's row if needed.
    ///
    /// **Advance, not set: the mark must never move backwards.** Two leaders
    /// overlapping across a failover, or a reconcile pass that finishes out of
    /// order, would otherwise rewind the mark and make the next pass replay a
    /// window that was already ingested. Re-ingest is idempotent
    /// (`ResultsRepository::upsert_run_results`), so a rewind is not corrupting
    /// — it is unbounded work, growing with every rewind. Implementations
    /// therefore write `GREATEST(existing, at)`, or its portable equivalent,
    /// and a call with an `at` behind the stored value is a successful no-op
    /// rather than an error.
    async fn advance<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        kind: WatermarkKind,
        at: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Record this tenant's [`SweepCursor`], or clear it with `None`.
    ///
    /// **Deliberately not monotonic**, which is the one thing that separates it
    /// from [`Self::advance`] and the whole reason it is a separate method
    /// rather than a second [`WatermarkKind`]. A cursor that could only move
    /// forward would be a second watermark: the lookback would never be
    /// re-walked and a run written behind the cursor would be lost. The sweep
    /// has to be able to move this backwards and to erase it, and both are
    /// ordinary outcomes — see [`SweepCursor`] for why a last-writer-wins write
    /// is safe under concurrent replicas.
    ///
    /// **It creates no row.** The sweep only ever writes a cursor for a tenant
    /// whose mark it has just advanced (or tried to), and
    /// [`Self::advance`] is what creates the row; a cursor for a tenant with no
    /// row updates nothing and is a successful no-op, which is the right answer
    /// for "there is no window to resume inside".
    async fn set_sweep_cursor<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        cursor: Option<SweepCursor>,
    ) -> Result<(), DomainError>;
}
