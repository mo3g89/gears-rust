use async_trait::async_trait;
use qa_environments_sdk::LeaseState;
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Versioned lease row for optimistic concurrency.
#[domain_model]
pub struct VersionedLease {
    pub state: LeaseState,
    pub version: i64,
    /// The instant this environment last transitioned **to free**, or `None`
    /// when no such transition is recorded — including a missing row, which is
    /// a never-leased environment.
    ///
    /// Read out so an acquisition can hand it to the run it admits; that is
    /// the start endpoint of `cpt-cf-qa-nfr-dispatch-latency`. See
    /// `migrations::m20260921_000002_lease_freed_at` for why it is a column.
    pub freed_at: Option<OffsetDateTime>,
}

/// Repository trait for environment lease state, read + compare-and-swap write.
#[async_trait]
pub trait LeasesRepository: Send + Sync {
    /// Read the current lease state for an environment. A missing row reads as
    /// `Free` with `version: 0`.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        environment_id: Uuid,
    ) -> Result<VersionedLease, DomainError>;

    /// Write the new state iff the stored version still equals
    /// `expected_version` (insert when `expected_version == 0` and no row
    /// exists yet). Returns `DomainError::LeaseConflict` on version
    /// mismatch — callers retry the read-decide-write loop.
    ///
    /// **An implementation must stamp `freed_at` when — and only when —
    /// `new_state` is [`LeaseState::Free`].** The caller writes only on a real
    /// state change and no acquisition can produce `Free`
    /// (`domain::lease`'s `no_acquisition_can_produce_a_free_state`), so a
    /// `Free` arriving here is exactly a transition to free and nothing else.
    /// The column must not be cleared by any other write: a held row keeps the
    /// instant its current holder consumed.
    async fn compare_and_set<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        environment_id: Uuid,
        expected_version: i64,
        new_state: &LeaseState,
    ) -> Result<(), DomainError>;
}
