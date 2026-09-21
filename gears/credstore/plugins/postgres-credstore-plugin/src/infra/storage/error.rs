//! Storage error type.
//!
//! `DBProvider<E>` requires `E: From<toolkit_db::DbError>`, and
//! [`credstore_sdk::CredStoreError`] cannot implement that (the SDK does not
//! depend on `toolkit-db`), so the repository carries its own error.
//!
//! It never reaches the SPI boundary as itself: `store::PgValueStore` wraps it
//! in [`StoreFault`](crate::domain::StoreFault), which is what the domain
//! logs and converts. Curating the wire detail is a domain decision and lives
//! with that type.

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
