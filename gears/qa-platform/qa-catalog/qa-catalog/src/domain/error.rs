use thiserror::Error;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Domain-specific errors using thiserror
#[domain_model]
#[derive(Error, Debug)]
pub enum DomainError {
    #[error("plan.yaml invalid: {message}")]
    PlanYamlInvalid { message: String },

    #[error("catalog entry {id} not found")]
    NotFound { id: Uuid },

    #[error("plan '{path}' not found in repository {repo_id} branch '{branch}'")]
    PlanNotFound {
        repo_id: Uuid,
        branch: String,
        path: String,
    },

    #[error("file '{path}' not found in the synced repository content")]
    FileNotFound { path: String },

    /// The requested `(repo, branch)` has no synced content to read
    /// plans/files from — either the repository has never synced
    /// successfully (or its last sync failed), or the requested branch has
    /// no materialized snapshot directory (see `infra::git::layout`).
    #[error("repository {repo_id} has no synced content for branch '{branch}'")]
    RepoNotSynced { repo_id: Uuid, branch: String },

    #[error("validation failed on {field}: {message}")]
    Validation { field: String, message: String },

    #[error("test repository '{name}' already exists")]
    RepositoryNameExists { name: String },

    #[error("custom plan '{name}' already exists")]
    CustomPlanNameExists { name: String },

    #[error("product '{name}' already exists")]
    ProductNameExists { name: String },

    #[error("ssh key '{name}' already exists")]
    SshKeyNameExists { name: String },

    #[error("concurrent branch cache update for repository {repo_id}, retry")]
    BranchCacheConflict { repo_id: Uuid },

    #[error("access denied")]
    Forbidden,

    /// Credential store (credstore) infrastructure failure. Carries only the
    /// credstore error's metadata message — never secret material
    /// (`SecretValue` redacts itself in all formatting).
    #[error("credential store error: {0}")]
    CredStore(String),

    /// Git sync engine failure (clone/fetch/checkout/ls-refs; the gix
    /// adapter, ADR-0005). The adapter never embeds credential material in
    /// `message`, and `ReposService` sanitizes it again before persisting
    /// it in `sync_error` (defense in depth).
    #[error("repository sync failed: {message}")]
    SyncFailed { message: String },

    /// Bundle blob store failure (local-fs `BundleStore` adapter, Task 9).
    #[error("bundle storage error: {0}")]
    Storage(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("internal error: {0}")]
    Internal(String),
}

// TODO(DE1302): `Database(String)` only stores a formatted message, so these
// `From` impls drop the source error. Extend `Database` to hold a boxed source
// so `.source()` returns the original error, then remove these allows.
// (Mirrors qa-environments' `domain::error`.)
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<toolkit_db::DbError> for DomainError {
    fn from(e: toolkit_db::DbError) -> Self {
        DomainError::Database(e.to_string())
    }
}

impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        log_enforcer_error(&e);
        match e {
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => Self::Forbidden,
            authz_resolver_sdk::EnforcerError::EvaluationFailed(err) => {
                Self::Internal(err.to_string())
            }
        }
    }
}

/// Log an enforcer failure at the level its severity warrants.
///
/// A PDP *deny* is a routine, expected outcome — an unauthorized caller, or a
/// background task running under a policy that does not grant the gear's
/// system actor — not a fault. Logging it at ERROR turns a correctly
/// fail-closed deployment into a permanent error stream and buries real
/// faults, so it goes to DEBUG (still diagnosable). A scope that cannot be
/// compiled, or a resolver that failed to answer, ARE faults and stay at
/// ERROR. This only chooses a log level: every variant still fails closed in
/// the mapping above.
#[allow(
    clippy::cognitive_complexity,
    reason = "three tracing macro expansions in one match, not real branching; \
              same precedent as qa-environments' api::rest::error"
)]
fn log_enforcer_error(e: &authz_resolver_sdk::EnforcerError) {
    match e {
        authz_resolver_sdk::EnforcerError::Denied { .. } => {
            tracing::debug!(error = %e, "AuthZ denied access");
        }
        authz_resolver_sdk::EnforcerError::CompileFailed(_) => {
            tracing::error!(error = %e, "AuthZ scope compilation failed");
        }
        authz_resolver_sdk::EnforcerError::EvaluationFailed(_) => {
            tracing::error!(error = %e, "AuthZ scope evaluation failed");
        }
    }
}
