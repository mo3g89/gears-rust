//! Database error conversion helpers.

use crate::domain::error::DomainError;

/// Convert a storage error into [`DomainError::Database`], keeping the error
/// itself as the `.source()`.
///
/// The bound used to be `impl Display` and the body `e.to_string()`, which
/// dropped the error at the one boundary almost every storage failure in this
/// gear crosses — review finding #25 is about `From<toolkit_db::DbError>`, but
/// fixing only that would have left this helper flattening `sea_orm::DbErr`
/// (SQLSTATE and constraint name included) to a bare message. Every caller
/// already passes a `sea_orm::DbErr`, so the tighter bound cost nothing.
pub fn db_err(e: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database {
        message: e.to_string(),
        source: Some(Box::new(e)),
    }
}
