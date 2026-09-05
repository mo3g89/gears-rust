//! Canonical error mapping: `DomainError` → `CanonicalError`.
//!
//! Mirrors `examples/toolkit/users-info/users-info/src/api/rest/error.rs`.
//! Builder method names/signatures verified against
//! `libs/toolkit-canonical-errors/src/builder.rs` and precedent usage in
//! `gears/file-storage`, `gears/system/resource-group`, and `gears/mini-chat`.

use toolkit::api::canonical_prelude::*;

use crate::domain::error::DomainError;

/// The environment aggregate's canonical-error resource.
///
/// **The GTS id and this struct's name stay `platform` after the aggregate was
/// renamed `TargetPlatform` → `Environment`, on purpose.** `gts_id` values are
/// registered external contract identifiers — `qa-insights` matches on this
/// exact one (`qa-insights/src/infra/clients/qa_environments.rs`) — so
/// renaming it would break a consumer rather than rename a concept. The struct
/// keeps the id's name so the two cannot drift apart in a reader's head.
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
            DomainError::EnvironmentNotFound { id } => {
                PlatformResourceError::not_found(format!("Environment {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }

            DomainError::VariableNotFound { id } => {
                VariableResourceError::not_found(format!("Variable {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }

            DomainError::EnvironmentNameExists { name } => PlatformResourceError::already_exists(
                format!("Environment '{name}' already exists"),
            )
            .with_resource(name.clone())
            .create(),

            DomainError::VariableNameExists { name } => {
                VariableResourceError::already_exists(format!("Variable '{name}' already exists"))
                    .with_resource(name.clone())
                    .create()
            }

            // Raised only from the lease-acquire path (a new run cannot
            // start on an unavailable environment), so this is modeled as a
            // LEASE-resource precondition rather than a PLATFORM one.
            //
            // The subject string below RENAMED with the aggregate --
            // `platform_availability` became `environment_availability` -- and
            // that is deliberate rather than an oversight, so it is worth
            // spelling out because it *is* wire-visible: it appears in the
            // RFC-9457 body of every 400 this arm raises.
            //
            // Corrected (Task 25 review, Important-3): this comment used to
            // state, as live policy, that `platform_id` on qa-runs' and
            // qa-insights' wire keeps its name because it mirrors a physical
            // column those gears did not rename, citing `RunDto::platform_id`
            // for the reasoning in full. Both halves are now false: Task 25
            // renamed that wire to `environment_id` in both gears (their
            // physical columns still did not move, and still do not need to
            // for this string's own reasoning to hold), and the anchor this
            // comment sent a reader to no longer exists --
            // `qa-runs`' `RunDto::environment_id`'s doc is the current one.
            // What was always true and stays true: this string is
            // qa-environments' own contract, names no column at all, and
            // therefore renames with the aggregate regardless of what any
            // other gear's wire does. Measured before changing it: no
            // consumer anywhere under `gears/` spells either form, and
            // CONTRACT-DIFF.md documents neither.
            DomainError::EnvironmentUnavailable { id } => LeaseResourceError::failed_precondition()
                .with_precondition_violation(
                    "environment_availability",
                    format!("Environment {id} is unavailable for new runs"),
                    "UNAVAILABLE",
                )
                .with_resource(id.to_string())
                .create(),

            DomainError::EnvironmentLeased { id } => PlatformResourceError::failed_precondition()
                .with_precondition_violation(
                    "lease_state",
                    format!("Environment {id} holds an active lease and cannot be deleted"),
                    "ACTIVE_LEASE",
                )
                .with_resource(id.to_string())
                .create(),

            // Validation errors originate from both the environments and
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
    //! `EnvironmentLeased` precondition violation accidentally mapping to 404
    //! instead of 400 — going unnoticed because both are still `Err`.
    use uuid::Uuid;

    use super::{CanonicalError, DomainError};

    #[test]
    fn environment_not_found_maps_to_not_found_404() {
        let ce: CanonicalError = DomainError::EnvironmentNotFound { id: Uuid::new_v4() }.into();
        assert_eq!(ce.status_code(), 404);
        assert!(
            matches!(ce, CanonicalError::NotFound { .. }),
            "expected NotFound, got {ce:?}"
        );
    }

    #[test]
    fn environment_name_exists_maps_to_already_exists_409() {
        let ce: CanonicalError = DomainError::EnvironmentNameExists {
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
    fn environment_leased_maps_to_failed_precondition_400() {
        let ce: CanonicalError = DomainError::EnvironmentLeased { id: Uuid::new_v4() }.into();
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
