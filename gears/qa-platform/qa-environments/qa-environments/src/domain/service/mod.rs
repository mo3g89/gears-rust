//! Domain service layer - business logic and rules.
//!
//! ## Architecture
//!
//! Per-resource submodules mirror the repository layer:
//! - `environments` - target environment CRUD + delete-precondition (no active lease)
//! - `variables` - pipeline (global) and per-environment variable CRUD
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

use crate::domain::ports::{ProductPluginPort, RunnerSecretWriter};
use crate::domain::repos::{EnvironmentsRepository, LeasesRepository, VariablesRepository};

mod environments;
mod leases;
mod variables;

pub use environments::EnvironmentsService;
pub use leases::LeasesService;
pub use variables::VariablesService;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod leases_tests;

#[cfg(test)]
mod environments_tests;

#[cfg(test)]
mod environments_kubeconfig_tests;

/// Task 18b's own tests: what a submitted credential form reaches, and where
/// each classified field lands.
#[cfg(test)]
mod environments_credentials_tests;

#[cfg(test)]
mod environments_observation_tests;

/// Task 15's own tests: what reaches the product plugin, and what its answer
/// writes into both column sets.
#[cfg(test)]
mod observation_projection_tests;

#[cfg(test)]
mod variables_tests;

#[cfg(test)]
mod tests_tenant_scoping;

#[cfg(test)]
mod unscoped_read_guard_tests;

/// `DB` provider alias (mirrors users-info).
pub type DbProvider = DBProvider<DbError>;

/// Authorization resource types and their PEP-supported properties.
pub mod resources {
    use super::ResourceType;
    use toolkit_security::pep_properties;

    /// The environment aggregate's authorization resource type.
    ///
    /// **Both the constant and the string stay `PLATFORM` / `"qa.platform"`
    /// after the aggregate was renamed `TargetPlatform` → `Environment`, on
    /// purpose.** The string is the resource type PDP policies are written
    /// against, so changing it would silently change who is authorized for
    /// what — a behaviour change, which the rename explicitly is not. The
    /// constant keeps the string's name so the two cannot drift apart in a
    /// reader's head. Renaming the resource type is a policy migration of its
    /// own, not part of this rename.
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

pub mod actions {
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
pub struct AppServices<P, V, L>
where
    P: EnvironmentsRepository,
    V: VariablesRepository,
    L: LeasesRepository,
{
    pub(crate) environments: EnvironmentsService<P, L>,
    pub(crate) variables: VariablesService<V, P>,
    pub(crate) leases: LeasesService<L, P>,
}

impl<P, V, L> AppServices<P, V, L>
where
    P: EnvironmentsRepository,
    V: VariablesRepository,
    L: LeasesRepository,
{
    #[allow(
        clippy::too_many_arguments,
        reason = "the DI container's constructor takes one argument per collaborator it wires \
                  (three repositories, the db provider, authz, credstore, the observation \
                  port, the product-plugin port, and the one scalar knob). Grouping them into \
                  a parameter struct would move the same nine names one indirection away \
                  without removing any of them, and `AppServices::new` has exactly one caller \
                  (`gear.rs`'s `init`)."
    )]
    pub fn new(
        environments_repo: Arc<P>,
        variables_repo: Arc<V>,
        leases_repo: Arc<L>,
        db: Arc<DbProvider>,
        authz: Arc<dyn AuthZResolverClient>,
        credstore: Arc<dyn CredStoreClientV1>,
        observer: Arc<dyn RunnerSecretWriter>,
        product_plugins: Arc<dyn ProductPluginPort>,
        max_variables: usize,
    ) -> Self {
        let enforcer = PolicyEnforcer::new(authz);

        Self {
            environments: EnvironmentsService::new(
                Arc::clone(&db),
                Arc::clone(&environments_repo),
                Arc::clone(&leases_repo),
                credstore,
                observer,
                product_plugins,
                enforcer.clone(),
            ),
            variables: VariablesService::new(
                Arc::clone(&db),
                variables_repo,
                Arc::clone(&environments_repo),
                enforcer.clone(),
                max_variables,
            ),
            leases: LeasesService::new(db, leases_repo, environments_repo, enforcer),
        }
    }
}
