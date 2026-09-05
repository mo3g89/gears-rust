//! Ingest high-water marks — the state that survives a deploy.

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// One tenant's two ingest marks.
///
/// Both `None` for a tenant that has never been reconciled or swept — which is
/// also what [`WatermarkRepository::get`] returns when no row exists at all, so
/// a caller never has to distinguish "no row" from "row with nulls". `None`
/// means *never*, and is deliberately distinct from the epoch: "reconciled up
/// to 1970" would make the first pass read every run ever finished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Watermarks {
    /// How far Task 15's reconcile poller has read. On `None` the first pass
    /// uses the configured lookback window instead.
    pub last_reconciled_finished_at: Option<OffsetDateTime>,
    /// When the stale-in-progress sweep last ran.
    ///
    /// **Nothing consumes this, and no task in the plan owns a sweep that
    /// would.** Measured 2026-08-21, in Phase A's whole-phase review: the only
    /// reader is this gear's own tests, the reconciler explicitly does *not*
    /// touch in-progress runs (`domain::service::reconcile`'s header, recorded
    /// behaviour 1 — `list_runs_finished_since` never returns a `NULL`
    /// `finished_at`), and Task 18 settled the consequence the other way by
    /// having the dashboard read active runs from qa-runs live. So the column,
    /// the [`WatermarkKind::SweptAt`] arm, its repository support and the three
    /// tests that exercise it have **no forecast consumer** — the phrase "the
    /// stale-in-progress sweep" names something that does not exist.
    ///
    /// Recorded rather than removed. Deleting it would mean editing a shipped
    /// migration, which the schema's append-only rule forbids, or adding a
    /// second migration to drop a column — neither of which is a review-fix
    /// action. **It is an open item in the plan for a human to decide whether
    /// Phase B drops it.** Until then: do not build on it, and do not read the
    /// name as a forecast.
    pub last_swept_at: Option<OffsetDateTime>,
}

/// Which mark [`WatermarkRepository::advance`] moves.
///
/// An enum rather than two methods because the two marks share a row and an
/// upsert: a second method would be the same statement with one column name
/// changed, and the pair would drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatermarkKind {
    /// [`Watermarks::last_reconciled_finished_at`].
    ReconciledFinishedAt,
    /// [`Watermarks::last_swept_at`].
    SweptAt,
}

/// Persistence for `qa_ingest_watermarks`.
///
/// **New in this port; no legacy original.** The reconcile sweep re-reads
/// qa-runs on its own cadence, a periodic reconcile that needs a durable mark
/// of how far it has already gotten — held in memory, that mark would restart
/// at zero on every deploy — which is the failure this table exists to
/// prevent (design §4.4, Task 15).
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
}
