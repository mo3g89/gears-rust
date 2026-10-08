//! `SeaORM` entity for the `qa_notification_cutoff` table.
//!
//! **"This deployment began notifying at instant T."** One row, written once,
//! by `m20260929_000004_run_completed_notification_cutoff` at upgrade time, and
//! never written again by anything. `NotifyService::notify_run_completed`
//! reads it and declines to notify a run that finished before it.
//!
//! # Why a persisted instant rather than a process one
//!
//! The obvious implementation of "do not notify history" is to record
//! `OffsetDateTime::now_utc()` when the gear boots. That is wrong twice, and
//! both ways are silent:
//!
//! * **A restart moves it forward**, re-opening the window over every run that
//!   finished since the last boot. A gear that restarts hourly would re-notify
//!   an hour of runs each time, and the claim rows are the only thing that
//!   would stop it — which is the belt this cutoff is meant to be the braces
//!   for, not the other way round.
//! * **Replicas disagree.** `reconcile` runs under `NoopLeaderElector`, so
//!   *every* replica sweeps (`crate::infra::leader`'s header: election here is
//!   an optimisation, not mutual exclusion). Three replicas booted at three
//!   instants would each have their own idea of where history ends, and the
//!   effective cutoff would be the earliest of them — chosen by whichever pod
//!   happened to restart least recently.
//!
//! A row in this gear's own database has neither property: it is one value,
//! written once, that every replica reads and no restart changes.
//!
//! # No `qa-insights-sdk` contract type, for `run_notification`'s reason
//!
//! Nothing outside this gear reads it. It is a deployment-lifecycle fact, and
//! publishing it would oblige the gear to keep its shape.
//!
//! # `tenant_id` is always the nil UUID, and that is the claim-row's precedent
//!
//! The upgrade happened once, for the whole deployment, not once per tenant —
//! and it *cannot* be per-tenant derived from this gear's own tables, because
//! the runs this exists to suppress are precisely the ones that left no row in
//! `qa_test_results` (see the migration's header). So the row is
//! deployment-wide and carries the nil tenant, exactly as
//! `crate::infra::leader::claim_row`'s `CLAIM_TENANT` does and for the same
//! stated reason: the column is in the unique index regardless, so a
//! per-tenant cutoff later is a change to the reader and not to the schema.
//!
//! It is still read under a *tenant-bound* `AccessScope::for_tenant(nil)` and
//! never `AccessScope::allow_all()` — `crate::domain::elevated`'s doc reserves
//! the unrestricted scope for the one cross-tenant enumeration, and this is
//! not one.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_notification_cutoff")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// Always [`uuid::Uuid::nil`] in production — see this module's header.
    pub tenant_id: Uuid,
    /// The instant the deployment started notifying. A run whose
    /// `finished_at` is strictly before this is history and is not notified.
    ///
    /// Written by the migration as an RFC-3339 literal, which is exactly what
    /// `sqlx`'s `SQLite` codec encodes an `OffsetDateTime` as and the first
    /// format its decoder tries (`sqlx-sqlite/src/types/time.rs`), and what
    /// Postgres accepts for a `TIMESTAMPTZ`. That agreement is not assumed:
    /// `the_cutoff_row_reads_back_as_a_real_instant` reads it back through
    /// this struct.
    pub cutoff_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
