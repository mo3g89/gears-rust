//! The abstractions this domain depends on.
//!
//! The service used to hold the `SeaORM` repository and the DB provider
//! directly, which is what DE0301 (`no_infra_in_domain`) forbids: a domain
//! module may not name `crate::infra`. The concrete repository has not moved
//! and has not changed — it is still the only place secret bytes cross into or
//! out of the database, and its methods still take an explicit runner (review
//! finding #14). What moved is the *composition*: opening a connection, and
//! wrapping the multi-statement sequences in a transaction, are adapter
//! concerns, and they now live in
//! [`PgValueStore`](crate::infra::storage::store::PgValueStore) behind
//! [`ValueStore`] below.
//!
//! The trait is deliberately narrow — four methods, all of them in terms of
//! the SDK's own `TenantId`/`SecretRef`/`OwnerId` and raw bytes. Nothing in it
//! names a database, so the domain no longer has an opinion about one.

use async_trait::async_trait;
use credstore_sdk::{CredStoreError, OwnerId, SecretRef, TenantId};
use thiserror::Error;

/// Wire-visible detail for every storage fault.
///
/// Curated on purpose: the underlying driver text can name hosts, users,
/// databases and constraints, and belongs in the operator's log (the service
/// logs it at `warn` before converting), not in a response body.
const UNAVAILABLE_DETAIL: &str = "credstore value store unavailable";

/// An infrastructure fault raised by a [`ValueStore`] adapter.
///
/// Opaque by design: the domain can log it and classify it, but cannot inspect
/// it, so no storage-specific branch can grow here. The adapter's own error is
/// kept as the source, so the operator log keeps the full text it always had.
#[derive(Debug, Error)]
#[error("{source}")]
pub struct StoreFault {
    /// The adapter error this fault wraps.
    source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl StoreFault {
    /// Wrap an adapter's own error as an opaque store fault.
    #[must_use]
    pub fn new(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            source: Box::new(source),
        }
    }
}

impl From<StoreFault> for CredStoreError {
    fn from(_fault: StoreFault) -> Self {
        // Every fault is an infrastructure fault, not a request-shape problem:
        // the gear has already authorized the call and resolved tenant/owner,
        // and this plugin validates nothing of its own. The gear's write saga
        // treats `ServiceUnavailable` as retryable, which is the right
        // disposition for a transient database outage.
        //
        // The wrapped error is deliberately dropped here rather than
        // forwarded: `domain::service::map_store_err` logs it at `warn` before
        // calling this conversion, so nothing is lost for the operator.
        Self::service_unavailable(UNAVAILABLE_DETAIL)
    }
}

/// The persistent value store behind the two runtime-written key classes.
///
/// `owner_id = Some` addresses the `private` class, `None` the `tenant` class.
/// Config-seeded `shared`/global entries are *not* part of this port — they
/// are never written and never persisted, so the service holds them itself.
///
/// Values cross this boundary as raw bytes rather than as `SecretValue`,
/// because `SecretValue` is non-`Clone` and zeroizes on drop; the conversion
/// happens at the two ends that own the material.
#[async_trait]
pub trait ValueStore: Send + Sync {
    /// Read the stored bytes for one key, or `None` if no value exists.
    ///
    /// # Errors
    ///
    /// Returns [`StoreFault`] if the store is unreachable or the read fails.
    async fn find(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<Option<Vec<u8>>, StoreFault>;

    /// Insert or overwrite the value for one key, atomically.
    ///
    /// # Errors
    ///
    /// Returns [`StoreFault`] if the write fails.
    async fn upsert(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
        value: &[u8],
    ) -> Result<(), StoreFault>;

    /// Write the value for one key **only if none is stored yet**, atomically.
    ///
    /// Returns `true` when a value was written.
    ///
    /// # Errors
    ///
    /// Returns [`StoreFault`] if the probe or the write fails.
    async fn insert_if_absent(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
        value: &[u8],
    ) -> Result<bool, StoreFault>;

    /// Remove the value for one key. A miss is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`StoreFault`] if the delete fails.
    async fn delete(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), StoreFault>;
}
