//! Storage error type.
//!
//! `DBProvider<E>` requires `E: From<toolkit_db::DbError>`, and
//! [`credstore_sdk::CredStoreError`] cannot implement that (the SDK does not
//! depend on `toolkit-db`), so the repository carries its own error and maps at
//! the SPI boundary.

use credstore_sdk::CredStoreError;
use thiserror::Error;
use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;

/// Errors raised by the value-store repository.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Connection/pool/transaction failure from the toolkit DB layer.
    #[error("database unavailable: {0}")]
    Db(#[from] DbError),

    /// A scoped query failed, or the access scope did not permit the tenant.
    #[error("scoped query failed: {0}")]
    Scope(#[from] ScopeError),
}

/// Wire-visible detail for every storage fault.
///
/// Curated on purpose: the underlying driver text can name hosts, users,
/// databases and constraints, and belongs in the operator's log (the repository
/// logs it at `warn`), not in a response body.
const UNAVAILABLE_DETAIL: &str = "credstore value store unavailable";

impl From<StoreError> for CredStoreError {
    fn from(_err: StoreError) -> Self {
        // Every variant is an infrastructure fault, not a request-shape
        // problem: the gear has already authorized the call and resolved
        // tenant/owner, and this plugin validates nothing of its own. The
        // gear's write saga treats `ServiceUnavailable` as retryable, which is
        // the right disposition for a transient database outage.
        //
        // The driver text is deliberately dropped here rather than forwarded:
        // `domain::service::map_store_err` logs the full error (with its
        // source chain) at `warn` before calling this conversion, so nothing
        // is lost for the operator.
        Self::service_unavailable(UNAVAILABLE_DETAIL)
    }
}
