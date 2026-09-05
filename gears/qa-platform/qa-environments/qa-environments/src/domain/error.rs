use thiserror::Error;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Domain-specific errors using thiserror
#[domain_model]
#[derive(Error, Debug)]
pub enum DomainError {
    #[error("environment {id} not found")]
    EnvironmentNotFound { id: Uuid },

    #[error("variable {id} not found")]
    VariableNotFound { id: Uuid },

    #[error("environment name '{name}' already exists")]
    EnvironmentNameExists { name: String },

    #[error("variable '{name}' already exists")]
    VariableNameExists { name: String },

    #[error("environment {id} is unavailable")]
    EnvironmentUnavailable { id: Uuid },

    #[error("environment {id} holds an active lease and cannot be deleted")]
    EnvironmentLeased { id: Uuid },

    #[error("validation failed on {field}: {message}")]
    Validation { field: String, message: String },

    #[error("concurrent lease update, retry")]
    LeaseConflict,

    #[error("access denied")]
    Forbidden,

    /// Credential store (credstore) infrastructure failure, raised on the
    /// kubeconfig write/replace/delete paths. Mirrors `qa-catalog`'s variant of
    /// the same name.
    ///
    /// What this gear guarantees: the string is credstore's **own** message
    /// (`CredStoreError::to_string`), which credstore redacts — this gear never
    /// adds the kubeconfig document to it, and passes the document to credstore
    /// wrapped in a `SecretValue`, which redacts itself in all formatting.
    ///
    /// What it does **not** guarantee, because it cannot: that credstore's
    /// message is free of anything sensitive. `CredStoreError::TypeViolation {
    /// detail }` and `Internal(String)` (`credstore-sdk/src/error.rs:7-31`) are
    /// free-form plugin text. Client exposure is blocked one layer up instead —
    /// `api::rest::error` maps this variant to a bare 500 that carries no detail
    /// at all, and a test pins that.
    #[error("credential store error: {0}")]
    CredStore(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("internal error: {0}")]
    Internal(String),
}

// TODO(DE1302): `Database(String)` only stores a formatted message, so these
// `From` impls drop the source error. Extend `Database` to hold a boxed source
// so `.source()` returns the original error, then remove these allows.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<toolkit_db::DbError> for DomainError {
    fn from(e: toolkit_db::DbError) -> Self {
        DomainError::Database(e.to_string())
    }
}

impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        tracing::error!(error = %e, "AuthZ scope resolution failed");
        match e {
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => Self::Forbidden,
            authz_resolver_sdk::EnforcerError::EvaluationFailed(err) => {
                Self::Internal(err.to_string())
            }
        }
    }
}
