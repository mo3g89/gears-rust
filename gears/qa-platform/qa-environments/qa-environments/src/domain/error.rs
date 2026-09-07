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

    /// A storage failure: the driver's own message, plus the error it came
    /// from when there was a typed one.
    ///
    /// # Why there is a boxed source here — review finding #25
    ///
    /// This was `Database(String)`, so `From<toolkit_db::DbError>` kept
    /// `e.to_string()` and dropped the error itself: `.source()` returned
    /// `None`, and anything a caller might have wanted from the original —
    /// `sea_orm::DbErr`'s SQLSTATE, a `sqlx::Error`'s constraint name, the
    /// whole cause chain rendered into a `tracing` field with `{:?}` — was
    /// unrecoverable by the time the error left `infra::storage`. The
    /// `TODO(DE1302)` that used to sit above the `From` impl below named
    /// exactly this fix.
    ///
    /// `message` keeps the rendered text and the `Display` string is
    /// unchanged (`"database error: {message}"`), so every response body, log
    /// line and persisted `error` column this variant reaches reads exactly as
    /// it did before.
    #[error("database error: {message}")]
    Database {
        /// The driver's rendered message — `DbError::to_string()` for a
        /// converted error, or the text a caller had in hand for one built
        /// with [`DomainError::database`].
        message: String,
        /// The error this was converted from. `From<toolkit_db::DbError>`
        /// always sets it; `None` for a failure that only ever existed as text
        /// (a `toolkit_odata::Error::Db` string, a test fixture), which is why
        /// this is an `Option` rather than a required field.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("internal error: {0}")]
    Internal(String),
}

impl DomainError {
    /// A [`Self::Database`] from a rendered message alone, with no source to
    /// attach — for a storage failure that arrives as text rather than as a
    /// typed error (`infra::storage`'s `toolkit_odata` mapping, and test
    /// fixtures).
    ///
    /// Prefer `?` on a `toolkit_db::DbError`, which goes through
    /// `From<toolkit_db::DbError>` and keeps the original as `.source()`
    /// (review finding #25).
    #[must_use]
    pub fn database(message: impl Into<String>) -> Self {
        Self::Database {
            message: message.into(),
            source: None,
        }
    }
}

/// Review finding #25: the source is boxed into [`DomainError::Database`]
/// rather than flattened to `e.to_string()`, so `.source()` reaches the
/// original `DbError` and its own cause chain. See that variant's doc; the
/// TODO(DE1302) comment and its lint allowance, which named exactly this fix,
/// are gone with it.
impl From<toolkit_db::DbError> for DomainError {
    fn from(e: toolkit_db::DbError) -> Self {
        DomainError::Database {
            message: e.to_string(),
            source: Some(Box::new(e)),
        }
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

    /// Review finding #25, and the `TODO(DE1302)` that used to sit above
    /// `From<toolkit_db::DbError>`: the conversion keeps the original error as
    /// `.source()` instead of flattening it to its `Display` text. Without
    /// this, `sea_orm`'s SQLSTATE and `sqlx`'s constraint name are gone by the
    /// time the error leaves `infra::storage`.
    #[test]
    fn a_db_error_converted_to_a_domain_error_keeps_its_source() {
        let db_error = toolkit_db::DbError::UnknownDsn("mysql://nowhere".to_owned());
        let rendered = db_error.to_string();

        let domain = DomainError::from(db_error);

        // The `Display` string is unchanged by the boxing, which is what lets
        // every existing response body and `error` column stay as it was.
        assert_eq!(domain.to_string(), format!("database error: {rendered}"));

        let source = std::error::Error::source(&domain).expect("the DbError is now the source");
        assert!(
            source.is::<toolkit_db::DbError>(),
            "the source must be the original error, not a re-wrapping of its text"
        );
        assert_eq!(source.to_string(), rendered);
    }

    /// The other constructor deliberately has no source: a failure that only
    /// ever existed as text cannot invent one, and `.source()` says so rather
    /// than pointing at a stand-in.
    #[test]
    fn a_database_error_built_from_text_alone_has_no_source() {
        let domain = DomainError::database("relation \"qa\" does not exist");

        assert_eq!(
            domain.to_string(),
            "database error: relation \"qa\" does not exist"
        );
        assert!(std::error::Error::source(&domain).is_none());
    }
}
