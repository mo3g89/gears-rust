//! Canonical error mapping: `DomainError` → `CanonicalError`.
//!
//! Mirrors `examples/toolkit/users-info/users-info/src/api/rest/error.rs`.
//! Builder method names/signatures verified against
//! `libs/toolkit-canonical-errors/src/builder.rs` and precedent usage in
//! `gears/file-storage`, `gears/system/resource-group`, and `gears/mini-chat`.

use toolkit::api::canonical_prelude::*;

use crate::domain::error::DomainError;

#[resource_error(gts_id!("cf.qa.environments.platform.v1~"))]
struct PlatformResourceError;

#[resource_error(gts_id!("cf.qa.environments.variable.v1~"))]
struct VariableResourceError;

#[resource_error(gts_id!("cf.qa.environments.lease.v1~"))]
struct LeaseResourceError;

impl From<DomainError> for CanonicalError {
    #[allow(clippy::cognitive_complexity)]
    fn from(e: DomainError) -> Self {
        let ce = match &e {
            DomainError::PlatformNotFound { id } => {
                PlatformResourceError::not_found(format!("Platform {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }

            DomainError::VariableNotFound { id } => {
                VariableResourceError::not_found(format!("Variable {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }

            DomainError::PlatformNameExists { name } => {
                PlatformResourceError::already_exists(format!("Platform '{name}' already exists"))
                    .with_resource(name.clone())
                    .create()
            }

            DomainError::VariableNameExists { name } => {
                VariableResourceError::already_exists(format!("Variable '{name}' already exists"))
                    .with_resource(name.clone())
                    .create()
            }

            // Raised only from the lease-acquire path (a new run cannot
            // start on an unavailable platform), so this is modeled as a
            // LEASE-resource precondition rather than a PLATFORM one.
            DomainError::PlatformUnavailable { id } => LeaseResourceError::failed_precondition()
                .with_precondition_violation(
                    "platform_availability",
                    format!("Platform {id} is unavailable for new runs"),
                    "UNAVAILABLE",
                )
                .with_resource(id.to_string())
                .create(),

            DomainError::PlatformLeased { id } => PlatformResourceError::failed_precondition()
                .with_precondition_violation(
                    "lease_state",
                    format!("Platform {id} holds an active lease and cannot be deleted"),
                    "ACTIVE_LEASE",
                )
                .with_resource(id.to_string())
                .create(),

            // Validation errors originate from both the platforms and
            // variables services; DomainError::Validation carries no
            // service tag to disambiguate, so PLATFORM is used uniformly
            // (mirrors the users-info reference, which does the same for
            // its own multi-field Validation variant).
            DomainError::Validation { field, message } => PlatformResourceError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION")
                .create(),

            DomainError::LeaseConflict => {
                LeaseResourceError::aborted("Concurrent lease update, retry")
                    .with_reason("LEASE_CONFLICT")
                    .create()
            }

            DomainError::Forbidden => PlatformResourceError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),

            // Infrastructure fault: internal 500 with NO credstore detail text
            // leaked to clients (mirrors qa-catalog's mapping of its own
            // `CredStore` variant).
            DomainError::CredStore(_) => {
                tracing::error!(error = ?e, "Credential store error occurred");
                CanonicalError::internal("An internal credential store error occurred").create()
            }

            DomainError::Database(_) => {
                tracing::error!(error = ?e, "Database error occurred");
                CanonicalError::internal("An internal database error occurred").create()
            }

            DomainError::Internal(_) => {
                tracing::error!(error = ?e, "Internal error occurred");
                CanonicalError::internal("An internal error occurred").create()
            }
        };

        if let Some(diag) = ce.diagnostic() {
            tracing::debug!(diagnostic = %diag, "Canonical error diagnostic");
        }

        ce
    }
}

#[cfg(test)]
mod tests {
    //! Locks down the `DomainError` → `CanonicalError` category (and HTTP
    //! status, via `CanonicalError::status_code()`) mapping above. Guards
    //! against a silent regression to the wrong RFC-9457 category — e.g. a
    //! `PlatformLeased` precondition violation accidentally mapping to 404
    //! instead of 400 — going unnoticed because both are still `Err`.
    use uuid::Uuid;

    use super::{CanonicalError, DomainError};

    #[test]
    fn platform_not_found_maps_to_not_found_404() {
        let ce: CanonicalError = DomainError::PlatformNotFound { id: Uuid::new_v4() }.into();
        assert_eq!(ce.status_code(), 404);
        assert!(
            matches!(ce, CanonicalError::NotFound { .. }),
            "expected NotFound, got {ce:?}"
        );
    }

    #[test]
    fn platform_name_exists_maps_to_already_exists_409() {
        let ce: CanonicalError = DomainError::PlatformNameExists {
            name: "dup".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 409);
        assert!(
            matches!(ce, CanonicalError::AlreadyExists { .. }),
            "expected AlreadyExists, got {ce:?}"
        );
    }

    #[test]
    fn platform_leased_maps_to_failed_precondition_400() {
        let ce: CanonicalError = DomainError::PlatformLeased { id: Uuid::new_v4() }.into();
        assert_eq!(ce.status_code(), 400);
        assert!(
            matches!(ce, CanonicalError::FailedPrecondition { .. }),
            "expected FailedPrecondition, got {ce:?}"
        );
    }

    #[test]
    fn lease_conflict_maps_to_aborted_409() {
        let ce: CanonicalError = DomainError::LeaseConflict.into();
        assert_eq!(ce.status_code(), 409);
        assert!(
            matches!(ce, CanonicalError::Aborted { .. }),
            "expected Aborted, got {ce:?}"
        );
    }

    #[test]
    fn forbidden_maps_to_permission_denied_403() {
        let ce: CanonicalError = DomainError::Forbidden.into();
        assert_eq!(ce.status_code(), 403);
        assert!(
            matches!(ce, CanonicalError::PermissionDenied { .. }),
            "expected PermissionDenied, got {ce:?}"
        );
    }

    /// The kubeconfig write path is the only new source of `CredStore`, and
    /// its message can name infrastructure (a vault host, a socket path). It
    /// must reach the client as a bare 500.
    #[test]
    fn credstore_error_maps_to_internal_500_without_detail_leak() {
        let ce: CanonicalError =
            DomainError::CredStore("vault sealed at 10.0.0.5".to_owned()).into();
        assert_eq!(ce.status_code(), 500);
        assert!(
            matches!(ce, CanonicalError::Internal { .. }),
            "expected Internal, got {ce:?}"
        );
        assert!(
            !format!("{ce:?}").contains("10.0.0.5"),
            "credstore detail text must not be surfaced: {ce:?}"
        );
    }

    #[test]
    fn database_error_maps_to_internal_500() {
        let ce: CanonicalError = DomainError::Database("connection reset".to_owned()).into();
        assert_eq!(ce.status_code(), 500);
        assert!(
            matches!(ce, CanonicalError::Internal { .. }),
            "expected Internal, got {ce:?}"
        );
    }
}
