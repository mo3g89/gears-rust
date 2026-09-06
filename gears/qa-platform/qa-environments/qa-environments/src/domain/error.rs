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
