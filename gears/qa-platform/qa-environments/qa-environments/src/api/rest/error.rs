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

            DomainError::Database { .. } => {
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
    use std::collections::BTreeSet;

    use uuid::Uuid;

    use super::{CanonicalError, DomainError};

    /// How many variants `DomainError` has, which is also the length of
    /// [`every_domain_error`]'s array.
    ///
    /// Bumping this without adding a value to that array is a **compile**
    /// error: the array literal would then be one element short of its declared
    /// length. That is the one link in the chain the compiler holds on its own.
    const DOMAIN_ERROR_VARIANTS: usize = 12;

    /// One of every `DomainError` variant.
    ///
    /// # What holds it, link by link, and which link is not held
    ///
    /// 1. **A variant added to `DomainError` is a compile error in
    ///    [`variant_index`]** and in `From<&DomainError> for ObservationClass`,
    ///    neither of which has a `_` arm, as does the mapping this module's
    ///    tests are about. All three land the author of that variant in code
    ///    they must classify.
    /// 2. **The array's length is [`DOMAIN_ERROR_VARIANTS`]**, so adding a
    ///    value without raising the count, or raising the count without adding
    ///    a value, does not compile. That link is the compiler's.
    /// 3. **[`the_sweep_above_covers_every_domain_error_variant`] asserts the
    ///    numbers this array yields are exactly `0..`[`DOMAIN_ERROR_VARIANTS`]**,
    ///    so a duplicated entry cannot stand in for a missing one and a
    ///    renumbering that leaves a hole fails.
    /// 4. **Nothing forces [`DOMAIN_ERROR_VARIANTS`] to equal the number of
    ///    variants the enum actually has.** An author who classifies a new
    ///    variant in link 1 and stops there leaves the sweep silently one
    ///    variant narrower, and every test here still passes. That link cannot
    ///    be closed from inside a test in stable Rust: no test can observe a
    ///    variant nobody constructed, `std::mem::variant_count` is nightly-only,
    ///    and a derive macro for one array is a larger thing than the array.
    ///
    /// qa-catalog's and qa-insights' catalogs carry the identical chain and the
    /// identical gap. Before this existed, this gear's sweep was a bare
    /// twelve-element array with no link to the enum at all: a thirteenth
    /// variant would have narrowed it in silence, which is the defect
    /// qa-insights' own fix round found the hard way.
    fn every_domain_error() -> [DomainError; DOMAIN_ERROR_VARIANTS] {
        [
            DomainError::EnvironmentNotFound { id: Uuid::nil() },
            DomainError::VariableNotFound { id: Uuid::nil() },
            DomainError::EnvironmentNameExists {
                name: "e".to_owned(),
            },
            DomainError::VariableNameExists {
                name: "v".to_owned(),
            },
            DomainError::EnvironmentUnavailable { id: Uuid::nil() },
            DomainError::EnvironmentLeased { id: Uuid::nil() },
            DomainError::Validation {
                field: "name".to_owned(),
                message: "required".to_owned(),
            },
            DomainError::LeaseConflict,
            DomainError::Forbidden,
            DomainError::CredStore("sealed".to_owned()),
            DomainError::database("connection reset"),
            DomainError::Internal("boom".to_owned()),
        ]
    }

    /// A number per `DomainError` variant, in declaration order.
    ///
    /// **Exhaustive with no `_` arm on purpose**: a variant added to
    /// `DomainError` does not compile here until somebody numbers it, which is
    /// what puts the author of that variant in this file, next to the array
    /// they also have to extend.
    ///
    /// The numbers mean nothing beyond being distinct and contiguous from zero.
    /// The last arm is written in terms of [`DOMAIN_ERROR_VARIANTS`] rather
    /// than as a literal so that raising the count *moves* it — leaving a hole
    /// in the middle of the range that
    /// [`the_sweep_above_covers_every_domain_error_variant`] reports — instead
    /// of sitting quietly at a number a stale count still agrees with.
    fn variant_index(error: &DomainError) -> usize {
        match error {
            DomainError::EnvironmentNotFound { .. } => 0,
            DomainError::VariableNotFound { .. } => 1,
            DomainError::EnvironmentNameExists { .. } => 2,
            DomainError::VariableNameExists { .. } => 3,
            DomainError::EnvironmentUnavailable { .. } => 4,
            DomainError::EnvironmentLeased { .. } => 5,
            DomainError::Validation { .. } => 6,
            DomainError::LeaseConflict => 7,
            DomainError::Forbidden => 8,
            DomainError::CredStore(_) => 9,
            DomainError::Database { .. } => 10,
            DomainError::Internal(_) => DOMAIN_ERROR_VARIANTS - 1,
        }
    }

    /// **The sweep's array numbers exactly `0..`[`DOMAIN_ERROR_VARIANTS`], once
    /// each.**
    ///
    /// [`the_metric_label_agrees_with_what_the_api_may_disclose`] iterates a
    /// hand-written array, so its coverage is whatever that array happens to
    /// hold. This checks the two ways that array can be wrong *without the
    /// count also being wrong*: a duplicated entry standing in for a missing
    /// one, and a renumbering in [`variant_index`] that leaves a hole. It
    /// compares the *set* of numbers, not the count of them, which is what
    /// makes the first of those visible.
    ///
    /// It would **not** catch a variant added to `DomainError` and left out of
    /// the array with the count left alone — see link 4 of
    /// [`every_domain_error`]'s chain.
    #[test]
    fn the_sweep_above_covers_every_domain_error_variant() {
        let swept: BTreeSet<usize> = every_domain_error().iter().map(variant_index).collect();
        let expected: BTreeSet<usize> = (0..DOMAIN_ERROR_VARIANTS).collect();
        assert_eq!(
            swept, expected,
            "every_domain_error must carry one of each DomainError variant; a number \
             missing here is a variant the disclosure sweep never sees"
        );
    }

    /// **The observation metric's refusal/failure label agrees, variant by
    /// variant, with what this mapping may disclose.**
    ///
    /// `ObservationClass::from(&DomainError)` splits the cycle's `Err` half into
    /// "a rule decided this" and "something broke", and it does so by naming the
    /// same three variants the mapping above renders as an opaque 500. Two
    /// partitions of one failure space is exactly the shape that drifts, so this
    /// asserts the agreement rather than describing it: a variant reclassified
    /// on one side and not the other fails here.
    ///
    /// **It lives in the API layer because it cannot live anywhere else.** The
    /// natural home is beside the label, in `domain::ports::metrics`' own tests
    /// — and `no_api_in_domain_tests` forbids any module under `src/domain` from
    /// naming `crate::api`, which is where `CanonicalError::from` is. The domain
    /// side carries a pointer to this test; qa-insights, whose canonical mapping
    /// is in its domain layer, keeps its version of this sweep there.
    ///
    /// The list is written out by hand — the compiler cannot enumerate an
    /// enum's variants — and it is [`every_domain_error`], which carries the
    /// [`DOMAIN_ERROR_VARIANTS`] + no-`_`-arm [`variant_index`] chain that
    /// qa-insights and qa-catalog wrap their identical sweeps in. Read that
    /// function's doc for what each link holds and for the one link that is not
    /// held. Before it existed this test iterated a bare twelve-element array
    /// literal with no link to the enum: a thirteenth variant would have
    /// narrowed the sweep in silence.
    #[test]
    fn the_metric_label_agrees_with_what_the_api_may_disclose() {
        use crate::domain::ports::metrics::ObservationClass;

        for error in every_domain_error() {
            let rendered = format!("{error:?}");
            let labelled_as_our_failure =
                ObservationClass::from(&error) == ObservationClass::Failed;
            let opaque = CanonicalError::from(error).status_code() == 500;
            assert_eq!(
                labelled_as_our_failure, opaque,
                "the observation label disagrees with the canonical rendering for {rendered}"
            );
        }
    }

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
        let ce: CanonicalError = DomainError::database("connection reset").into();
        assert_eq!(ce.status_code(), 500);
        assert!(
            matches!(ce, CanonicalError::Internal { .. }),
            "expected Internal, got {ce:?}"
        );
    }
}
