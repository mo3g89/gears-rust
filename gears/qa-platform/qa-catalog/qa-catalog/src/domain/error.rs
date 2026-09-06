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

    /// The product exists and is visible, but no plugin can be resolved for
    /// it.
    ///
    /// **One shape, since Task 20a.** The product names a plugin that is not
    /// registered in this process: the row is fine, and the deployment is
    /// missing the gear that registers `instance_id`, or the id is stale.
    ///
    /// It used to carry `Option<String>`, with `None` meaning "the product's
    /// `plugin_instance_id` is null" and a second 404 message telling an
    /// operator to bind the product. `m20260903_000004` made the column
    /// `NOT NULL` and the model followed, so that state is unconstructible —
    /// the only producer is `QaProductRegistry::plugin_for`, which now always
    /// has an id. The comment that kept the `Option` alive claimed
    /// `qa-environments`' port also produced this variant; it does not and
    /// cannot, because that is a different type
    /// (`qa_environments::domain::ports::PluginUnavailable`) in a different
    /// crate (review finding IMPORTANT-5).
    ///
    /// Carries the GTS instance id, which is a type identifier and never
    /// credential-derived — the plugin object this error stands in for is the
    /// thing that handles credentials, and none of it reaches here.
    #[error(
        "product {product_id} has no resolvable product plugin (plugin_instance_id: {instance_id})"
    )]
    ProductPluginUnavailable {
        product_id: Uuid,
        instance_id: String,
    },

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
            // A denial, and the one `CompileFailed` shape `authz-resolver-sdk`
            // itself documents as a deny, are both about the caller.
            //
            // `CompileFailed` is not one thing — its two constructors
            // disagree about who the failure is about
            // (`authz-resolver-sdk/src/pep/compiler.rs:44-55`):
            //
            // - `ConstraintsRequiredButAbsent`: "a deny: the PEP asked for
            //   row-level constraints but received an empty set. Fail-closed"
            //   (`compiler.rs:45-48`) — the PDP answered allow but supplied no
            //   row scope. A scoped-permission refusal, the same shape as
            //   `Denied`, so it merges into this arm rather than getting a
            //   twin `=> Self::Forbidden` clippy would flag as
            //   `match_same_arms`.
            // - `AllConstraintsFailed` (below) means the PDP named predicates
            //   this PEP could not compile at all — a policy/PEP mismatch,
            //   not a fact about the caller's permissions. That is a fault,
            //   so it is a 500, not folded in here.
            //
            // Both `CompileFailed` shapes still fail closed: neither produces
            // an `AccessScope`, so no row becomes reachable either way. Only
            // the status differs. Review finding #3.
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(
                authz_resolver_sdk::pep::ConstraintCompileError::ConstraintsRequiredButAbsent,
            ) => Self::Forbidden,
            authz_resolver_sdk::EnforcerError::CompileFailed(err) => {
                Self::Internal(format!("authorization scope compilation failed: {err}"))
            }
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
/// faults, so it goes to DEBUG (still diagnosable).
///
/// `CompileFailed` splits the same way the status mapping above does:
/// `ConstraintsRequiredButAbsent` is the PDP answering allow with no row
/// scope, which is the same routine-deny shape as `Denied` and logs at DEBUG
/// to match; `AllConstraintsFailed`, and a resolver that failed to answer, ARE
/// faults and stay at ERROR. This only chooses a log level: every variant
/// still fails closed in the mapping above.
#[allow(
    clippy::cognitive_complexity,
    reason = "four tracing macro expansions in one match, not real branching; \
              same precedent as qa-environments' api::rest::error"
)]
fn log_enforcer_error(e: &authz_resolver_sdk::EnforcerError) {
    match e {
        authz_resolver_sdk::EnforcerError::Denied { .. } => {
            tracing::debug!(error = %e, "AuthZ denied access");
        }
        authz_resolver_sdk::EnforcerError::CompileFailed(
            authz_resolver_sdk::pep::ConstraintCompileError::ConstraintsRequiredButAbsent,
        ) => {
            tracing::debug!(error = %e, "AuthZ compiled an empty (deny) scope");
        }
        authz_resolver_sdk::EnforcerError::CompileFailed(_) => {
            tracing::error!(error = %e, "AuthZ scope compilation failed");
        }
        authz_resolver_sdk::EnforcerError::EvaluationFailed(_) => {
            tracing::error!(error = %e, "AuthZ scope evaluation failed");
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! `From<EnforcerError> for DomainError`, pinned one arm per test so a
    //! future change to any single arm fails exactly one test rather than
    //! being lost in a combined assertion.
    use authz_resolver_sdk::pep::ConstraintCompileError;
    use authz_resolver_sdk::{AuthZResolverError, EnforcerError};

    use super::*;

    /// The routine, expected outcome: a PDP deny is about the caller, and
    /// stays a 403.
    #[test]
    fn a_denial_is_still_forbidden() {
        let e = EnforcerError::Denied { deny_reason: None };
        assert!(matches!(DomainError::from(e), DomainError::Forbidden));
    }

    /// **Deliberately still a 403, not the 500 finding #3 is about.**
    ///
    /// `ConstraintsRequiredButAbsent` means the PDP answered *allow* but
    /// supplied no row-level constraints although the PEP asked for them.
    /// `authz-resolver-sdk`'s own compiler doc calls this a deny: "the PEP
    /// asked for row-level constraints but received an empty set. Fail-closed"
    /// (`authz-resolver-sdk/src/pep/compiler.rs:45-48`). That is a
    /// scoped-permission refusal — the same shape as `Denied` — not a broken
    /// policy engine, so a reader who knows only finding #3 ("a compile fault
    /// is a 500") would expect this test to assert `Internal` and would be
    /// wrong: this specific compile failure is about the caller, not the PDP.
    #[test]
    fn a_scope_required_but_absent_is_still_forbidden() {
        let e = EnforcerError::CompileFailed(ConstraintCompileError::ConstraintsRequiredButAbsent);
        assert!(
            matches!(DomainError::from(e), DomainError::Forbidden),
            "ConstraintsRequiredButAbsent is a documented deny, not a fault"
        );
    }

    /// The half finding #3 is actually about: the PDP named predicates this
    /// PEP cannot compile at all — a configuration fault, not a fact about
    /// the caller's permissions (`compiler.rs:52`).
    #[test]
    fn all_constraints_failing_to_compile_is_internal_not_forbidden() {
        let e = EnforcerError::CompileFailed(ConstraintCompileError::AllConstraintsFailed {
            reason: "unknown predicate `frobnicate`".to_owned(),
        });
        assert!(
            matches!(DomainError::from(e), DomainError::Internal(_)),
            "AllConstraintsFailed must map to Internal (500), not Forbidden (403)"
        );
    }

    /// Unchanged: the PDP RPC itself failing is a fault, not a decision.
    #[test]
    fn an_evaluation_failure_is_internal() {
        let e = EnforcerError::EvaluationFailed(AuthZResolverError::ServiceUnavailable(
            "plugin not registered".to_owned(),
        ));
        assert!(matches!(DomainError::from(e), DomainError::Internal(_)));
    }
}
