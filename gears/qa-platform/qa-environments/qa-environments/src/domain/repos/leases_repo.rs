use async_trait::async_trait;
use qa_environments_sdk::LeaseState;
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
}

/// Repository trait for platform lease state, read + compare-and-swap write.
#[async_trait]
pub trait LeasesRepository: Send + Sync {
    /// Read the current lease state for a platform. A missing row reads as
    /// `Free` with `version: 0`.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        platform_id: Uuid,
    ) -> Result<VersionedLease, DomainError>;

    /// Write the new state iff the stored version still equals
    /// `expected_version` (insert when `expected_version == 0` and no row
    /// exists yet). Returns `DomainError::LeaseConflict` on version
    /// mismatch — callers retry the read-decide-write loop.
    async fn compare_and_set<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        platform_id: Uuid,
        expected_version: i64,
        new_state: &LeaseState,
    ) -> Result<(), DomainError>;
}
