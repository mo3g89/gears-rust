//! Canonical error mapping: `DomainError` → `CanonicalError`.
//!
//! Mirrors `gears/qa-platform/qa-environments/.../api/rest/error.rs`.
//! The match is deliberately exhaustive (no catch-all arm): adding a new
//! `DomainError` variant fails compilation here until a mapping is chosen.

use toolkit::api::canonical_prelude::*;

use crate::domain::error::DomainError;

/// Shared catalog-entry resource for variants that carry no per-service tag
/// (`NotFound`/`FileNotFound` are raised by several services over one
/// `DomainError`, and `Validation`/`Forbidden` likewise; mirrors
/// qa-environments' uniform use of its PLATFORM resource for `Validation`).
#[resource_error(gts_id!("cf.qa.catalog.entry.v1~"))]
struct CatalogResourceError;

#[resource_error(gts_id!("cf.qa.catalog.test_repo.v1~"))]
struct TestRepoResourceError;

#[resource_error(gts_id!("cf.qa.catalog.plan.v1~"))]
struct PlanResourceError;

#[resource_error(gts_id!("cf.qa.catalog.custom_plan.v1~"))]
struct CustomPlanResourceError;

#[resource_error(gts_id!("cf.qa.catalog.product.v1~"))]
struct ProductResourceError;

#[resource_error(gts_id!("cf.qa.catalog.ssh_key.v1~"))]
struct SshKeyResourceError;

impl From<DomainError> for CanonicalError {
    #[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
    fn from(e: DomainError) -> Self {
        let ce = match &e {
            DomainError::NotFound { id } => {
                CatalogResourceError::not_found(format!("Catalog entry {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }

            // Same AIP-193 category as `NotFound` above, and deliberately
            // so: from a caller's point of view a product whose plugin
            // cannot be resolved has no usable product behaviour to address,
            // and `PRODUCT-PLUGINS-DESIGN.md` §4.2 states the outcome for a
            // plugin gear that is not linked into the binary as a not-found
            // at *use* rather than a boot failure.
            //
            // One shape since Task 20a: a product always names a plugin, so
            // the only failure left is that the plugin is not registered here.
            // The "is not bound to a product plugin" arm was deleted with the
            // `Option` it matched on -- see `DomainError::ProductPluginUnavailable`
            // and review finding IMPORTANT-5.
            DomainError::ProductPluginUnavailable {
                product_id,
                instance_id,
            } => ProductResourceError::not_found(format!(
                "Product {product_id} names product plugin '{instance_id}', which is not \
                 registered in this deployment"
            ))
            .with_resource(product_id.to_string())
            .create(),

            DomainError::PlanNotFound {
                repo_id,
                branch,
                path,
            } => PlanResourceError::not_found(format!(
                "Plan '{path}' was not found in repository {repo_id} branch '{branch}'"
            ))
            .with_resource(path.clone())
            .create(),

            DomainError::FileNotFound { path } => CatalogResourceError::not_found(format!(
                "File '{path}' was not found in the synced repository content"
            ))
            .with_resource(path.clone())
            .create(),

            // Server-held state (the repository has no synced working copy
            // for the branch), not a malformed request: FailedPrecondition,
            // which renders as HTTP 400 (mirrors qa-environments'
            // PlatformLeased precedent).
            DomainError::RepoNotSynced { repo_id, branch } => {
                TestRepoResourceError::failed_precondition()
                    .with_precondition_violation(
                        "sync_state",
                        format!("Repository {repo_id} has no synced content for branch '{branch}'"),
                        "NOT_SYNCED",
                    )
                    .with_resource(repo_id.to_string())
                    .create()
            }

            // Also FailedPrecondition rather than InvalidArgument: the
            // offending `plan.yaml` is server-side repository content, not
            // client request input — the request was well-formed, but the
            // synced state it addresses cannot satisfy it (same category
            // qa-environments uses for state-shaped 400s like
            // PlatformLeased). The message is parser output over repo
            // content, safe to surface.
            DomainError::PlanYamlInvalid { message } => PlanResourceError::failed_precondition()
                .with_precondition_violation("plan_yaml", message.clone(), "PLAN_YAML_INVALID")
                .create(),

            DomainError::Validation { field, message } => CatalogResourceError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION")
                .create(),

            DomainError::RepositoryNameExists { name } => TestRepoResourceError::already_exists(
                format!("Test repository '{name}' already exists"),
            )
            .with_resource(name.clone())
            .create(),

            DomainError::CustomPlanNameExists { name } => CustomPlanResourceError::already_exists(
                format!("Custom plan '{name}' already exists"),
            )
            .with_resource(name.clone())
            .create(),

            DomainError::ProductNameExists { name } => {
                ProductResourceError::already_exists(format!("Product '{name}' already exists"))
                    .with_resource(name.clone())
                    .create()
            }

            DomainError::SshKeyNameExists { name } => {
                SshKeyResourceError::already_exists(format!("SSH key '{name}' already exists"))
                    .with_resource(name.clone())
                    .create()
            }

            // Retryable write-write race, like qa-environments'
            // LeaseConflict: Aborted → HTTP 409.
            DomainError::BranchCacheConflict { repo_id } => {
                TestRepoResourceError::aborted("Concurrent branch cache update, retry")
                    .with_reason("BRANCH_CACHE_CONFLICT")
                    .with_resource(repo_id.to_string())
                    .create()
            }

            DomainError::Forbidden => CatalogResourceError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),

            // The git remote / sync engine failed — an upstream dependency
            // fault, not a client error and not gear-internal corruption:
            // ServiceUnavailable (503) is the truthful category. The engine
            // message is built from gix error chains (never credential
            // material), but it is logged rather than surfaced — defense in
            // depth against secret echo.
            DomainError::SyncFailed { .. } => {
                tracing::error!(error = ?e, "Repository sync failed");
                CanonicalError::service_unavailable()
                    .with_detail("Repository synchronization failed")
                    .create()
            }

            // Infrastructure faults: internal 500 with NO driver/detail text
            // leaked to clients (`CanonicalError::internal` exposes exactly
            // the detail string it is given — qa-environments precedent).
            DomainError::CredStore(_) => {
                tracing::error!(error = ?e, "Credential store error occurred");
                CanonicalError::internal("An internal credential store error occurred").create()
            }

            DomainError::Storage(_) => {
                tracing::error!(error = ?e, "Bundle storage error occurred");
                CanonicalError::internal("An internal storage error occurred").create()
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
    //! status, via `CanonicalError::status_code()`) mapping above — one test
    //! per domain variant, so a silent category regression (e.g.
    //! `RepoNotSynced` drifting from 400 to 404) fails loudly.
    use uuid::Uuid;

    use super::{CanonicalError, DomainError};

    #[test]
    fn not_found_maps_to_not_found_404() {
        let ce: CanonicalError = DomainError::NotFound { id: Uuid::new_v4() }.into();
        assert_eq!(ce.status_code(), 404);
        assert!(
            matches!(ce, CanonicalError::NotFound { .. }),
            "expected NotFound, got {ce:?}"
        );
    }

    /// A 404 naming the plugin the deployment is missing.
    ///
    /// This used to assert "both shapes", the other being an *unbound*
    /// product (`instance_id: None`) with its own message. Task 20a made
    /// `plugin_instance_id` `NOT NULL`, so that shape became unconstructible
    /// and this test was the only thing keeping its arm alive — a test that
    /// builds by hand a state no caller can reach (review finding
    /// IMPORTANT-5).
    #[test]
    fn product_plugin_unavailable_maps_to_a_404_naming_the_missing_plugin() {
        let product_id = Uuid::new_v4();

        let unregistered: CanonicalError = DomainError::ProductPluginUnavailable {
            product_id,
            instance_id: "gts.a.b.v1~c.d.v1".to_owned(),
        }
        .into();
        assert_eq!(unregistered.status_code(), 404);
        assert!(matches!(unregistered, CanonicalError::NotFound { .. }));
        let rendered = format!("{unregistered:?}");
        assert!(
            rendered.contains("gts.a.b.v1~c.d.v1"),
            "the unresolvable id must reach the operator: {rendered}"
        );
        // The whole sentence, not just the id: this message is the only thing
        // that tells an operator which gear the deployment is missing, and a
        // `contains` on the id alone could not see that a lost
        // line-continuation backslash had put 22 literal spaces in the middle
        // of it -- which it had.
        assert!(
            rendered.contains(
                "names product plugin 'gts.a.b.v1~c.d.v1', which is not registered in this \
                 deployment"
            ),
            "the message must read as one sentence: {rendered}"
        );
    }

    #[test]
    fn plan_not_found_maps_to_not_found_404() {
        let ce: CanonicalError = DomainError::PlanNotFound {
            repo_id: Uuid::new_v4(),
            branch: "main".to_owned(),
            path: "plans/smoke.yaml".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 404);
        assert!(matches!(ce, CanonicalError::NotFound { .. }));
    }

    #[test]
    fn file_not_found_maps_to_not_found_404() {
        let ce: CanonicalError = DomainError::FileNotFound {
            path: "tests/a.py".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 404);
        assert!(matches!(ce, CanonicalError::NotFound { .. }));
    }

    #[test]
    fn repo_not_synced_maps_to_failed_precondition_400() {
        let ce: CanonicalError = DomainError::RepoNotSynced {
            repo_id: Uuid::new_v4(),
            branch: "main".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 400);
        assert!(
            matches!(ce, CanonicalError::FailedPrecondition { .. }),
            "expected FailedPrecondition, got {ce:?}"
        );
    }

    #[test]
    fn plan_yaml_invalid_maps_to_failed_precondition_400() {
        let ce: CanonicalError = DomainError::PlanYamlInvalid {
            message: "missing name".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 400);
        assert!(matches!(ce, CanonicalError::FailedPrecondition { .. }));
    }

    #[test]
    fn validation_maps_to_invalid_argument_400() {
        let ce: CanonicalError = DomainError::Validation {
            field: "name".to_owned(),
            message: "must not be empty".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 400);
        assert!(matches!(ce, CanonicalError::InvalidArgument { .. }));
    }

    #[test]
    fn exists_variants_map_to_already_exists_409() {
        let cases: Vec<DomainError> = vec![
            DomainError::RepositoryNameExists {
                name: "dup".to_owned(),
            },
            DomainError::CustomPlanNameExists {
                name: "dup".to_owned(),
            },
            DomainError::ProductNameExists {
                name: "dup".to_owned(),
            },
            DomainError::SshKeyNameExists {
                name: "dup".to_owned(),
            },
        ];
        for e in cases {
            let label = format!("{e:?}");
            let ce: CanonicalError = e.into();
            assert_eq!(ce.status_code(), 409, "{label}");
            assert!(
                matches!(ce, CanonicalError::AlreadyExists { .. }),
                "{label}: expected AlreadyExists, got {ce:?}"
            );
        }
    }

    #[test]
    fn branch_cache_conflict_maps_to_aborted_409() {
        let ce: CanonicalError = DomainError::BranchCacheConflict {
            repo_id: Uuid::new_v4(),
        }
        .into();
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
        assert!(matches!(ce, CanonicalError::PermissionDenied { .. }));
    }

    #[test]
    fn sync_failed_maps_to_service_unavailable_503_without_detail_leak() {
        let ce: CanonicalError = DomainError::SyncFailed {
            message: "fetch https://***@example.com failed".to_owned(),
        }
        .into();
        assert_eq!(ce.status_code(), 503);
        assert!(
            matches!(ce, CanonicalError::ServiceUnavailable { .. }),
            "expected ServiceUnavailable, got {ce:?}"
        );
        assert!(
            !format!("{ce:?}").contains("example.com"),
            "engine error text must not be surfaced: {ce:?}"
        );
    }

    #[test]
    fn infrastructure_variants_map_to_internal_500_without_detail_leak() {
        let cases: Vec<DomainError> = vec![
            DomainError::CredStore("vault sealed at 10.0.0.5".to_owned()),
            DomainError::Storage("/var/bundles: disk full".to_owned()),
            DomainError::Database("connection reset by peer".to_owned()),
            DomainError::Internal("index out of bounds".to_owned()),
        ];
        for e in cases {
            let label = format!("{e:?}");
            let ce: CanonicalError = e.into();
            assert_eq!(ce.status_code(), 500, "{label}");
            assert!(
                matches!(ce, CanonicalError::Internal { .. }),
                "{label}: expected Internal, got {ce:?}"
            );
            let rendered = format!("{ce:?}");
            for fragment in ["10.0.0.5", "disk full", "connection reset", "out of bounds"] {
                assert!(
                    !rendered.contains(fragment),
                    "{label}: driver detail leaked to the client-visible error: {rendered}"
                );
            }
        }
    }
}
