//! Domain service layer - business logic and rules.
//!
//! ## Architecture
//!
//! Per-resource submodules mirror the repository layer:
//! - `platforms` - target platform CRUD + delete-precondition (no active lease)
//! - `variables` - pipeline (global) and per-platform variable CRUD
//! - `leases` - the CAS retry loop around the pure decision logic in
//!   `crate::domain::lease`
//!
//! ## Security
//!
//! All operations use the `AuthZ` Resolver PEP (Policy Enforcement Point)
//! pattern via [`PolicyEnforcer`](authz_resolver_sdk::PolicyEnforcer):
//! 1. Construct a `PolicyEnforcer` once (during init) — serves all resource types.
//! 2. Call `enforcer.access_scope(&ctx, &resource, action, resource_id)`.
//! 3. The enforcer builds the request, evaluates via PDP, and compiles to `AccessScope`.
//! 4. Pass the scope to repository methods for tenant-isolated queries.
//!
//! Reference: `examples/toolkit/users-info/users-info/src/domain/service/mod.rs`.
//!
//! ## Connection Management
//!
//! Services acquire database connections internally via `DBProvider`. Callers
//! do NOT touch database objects - they simply call service methods with
//! business parameters only.

use std::sync::Arc;

use authz_resolver_sdk::AuthZResolverClient;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::ResourceType;
use credstore_sdk::CredStoreClientV1;
use toolkit_db::{DBProvider, DbError};
use toolkit_macros::domain_model;

use crate::domain::ports::PlatformObserver;
use crate::domain::repos::{LeasesRepository, PlatformsRepository, VariablesRepository};

mod leases;
mod platforms;
mod variables;

pub(crate) use leases::LeasesService;
pub(crate) use platforms::PlatformsService;
pub(crate) use variables::VariablesService;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod leases_tests;

#[cfg(test)]
mod platforms_tests;

#[cfg(test)]
mod platforms_kubeconfig_tests;

#[cfg(test)]
mod platforms_observation_tests;

#[cfg(test)]
mod variables_tests;

#[cfg(test)]
mod tests_tenant_scoping;

#[cfg(test)]
mod unscoped_read_guard_tests;

/// `DB` provider alias (mirrors users-info).
pub(crate) type DbProvider = DBProvider<DbError>;

/// Authorization resource types and their PEP-supported properties.
pub(crate) mod resources {
    use super::ResourceType;
    use toolkit_security::pep_properties;

    pub const PLATFORM: ResourceType = ResourceType::from_static(
        "qa.platform",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    pub const VARIABLE: ResourceType = ResourceType::from_static(
        "qa.variable",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    pub const LEASE: ResourceType = ResourceType::from_static(
        "qa.lease",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );
}

pub(crate) mod actions {
    pub const GET: &str = "get";
    pub const LIST: &str = "list";
    pub const CREATE: &str = "create";
    pub const UPDATE: &str = "update";
    pub const DELETE: &str = "delete";
    pub const ACQUIRE: &str = "acquire";
    pub const RELEASE: &str = "release";
}

// DI Container - aggregates all domain services
//
// # Database Access
//
// Services acquire database connections internally via `DBProvider`. Callers
// do NOT touch database objects - they call service methods with business
// parameters only.
#[domain_model]
pub(crate) struct AppServices<P, V, L>
where
    P: PlatformsRepository,
    V: VariablesRepository,
    L: LeasesRepository,
{
    pub(crate) platforms: PlatformsService<P, V, L>,
    pub(crate) variables: VariablesService<V, P>,
    pub(crate) leases: LeasesService<L, P>,
}

impl<P, V, L> AppServices<P, V, L>
where
    P: PlatformsRepository,
    V: VariablesRepository,
    L: LeasesRepository,
{
    #[allow(
        clippy::too_many_arguments,
        reason = "the DI container's constructor takes one argument per collaborator it wires \
                  (three repositories, the db provider, authz, credstore, the observation port, \
                  and the one scalar knob). Grouping them into a parameter struct would move \
                  the same eight names one indirection away without removing any of them, and \
                  `AppServices::new` has exactly one caller (`gear.rs`'s `init`)."
    )]
    pub fn new(
        platforms_repo: Arc<P>,
        variables_repo: Arc<V>,
        leases_repo: Arc<L>,
        db: Arc<DbProvider>,
        authz: Arc<dyn AuthZResolverClient>,
        credstore: Arc<dyn CredStoreClientV1>,
        observer: Arc<dyn PlatformObserver>,
        max_variables: usize,
    ) -> Self {
        let enforcer = PolicyEnforcer::new(authz);

        Self {
            platforms: PlatformsService::new(
                Arc::clone(&db),
                Arc::clone(&platforms_repo),
                Arc::clone(&variables_repo),
                Arc::clone(&leases_repo),
                credstore,
                observer,
                enforcer.clone(),
            ),
            variables: VariablesService::new(
                Arc::clone(&db),
                variables_repo,
                Arc::clone(&platforms_repo),
                enforcer.clone(),
                max_variables,
            ),
            leases: LeasesService::new(db, leases_repo, platforms_repo, enforcer),
        }
    }
}
