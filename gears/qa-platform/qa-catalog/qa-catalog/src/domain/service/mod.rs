//! Domain service layer - business logic and rules.
//!
//! ## Architecture
//!
//! Per-resource submodules mirror the repository layer:
//! - `repos` - test repository CRUD, on-demand sync, branch cache reads
//! - `plans` - plan discovery from synced working copies + `TEST_META` reads
//! - `custom_plans` - user-composed persisted plan CRUD
//! - `products` - products and version upserts
//! - `ssh_keys` - SSH key metadata (material lives in credstore only)
//! - `bundles` - ephemeral tar.gz bundle build/serve/GC
//! - `sync_cache` - branch freshness TTL cache + two-tier sync locks
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
//! One scope per resource type: a scope compiled for resource type A is never
//! reused on a query against type B's table — every precondition read derives
//! its own dedicated scope. **Two nil-tenant enumerating reads are the
//! exception to "every scope comes from a PEP decision"**:
//! `repos::ReposService::list_refresh_targets` and
//! `bundles::BundlesService::tenants_with_expired_bundles` pass
//! `domain::elevated::enumeration_scope`'s `AccessScope::allow_all()`
//! straight through instead, at the two call sites that constructor
//! sanctions — see that module's doc for why. Every *write* either one feeds
//! is still authorized normally, under a tenant-bound context.
//!
//! Reference: `gears/qa-platform/qa-environments/.../domain/service/mod.rs`.
//!
//! ## Connection Management
//!
//! Services acquire database connections internally via `DBProvider`. Callers
//! do NOT touch database objects - they simply call service methods with
//! business parameters only.

use std::path::PathBuf;
use std::sync::Arc;

use authz_resolver_sdk::AuthZResolverClient;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::ResourceType;
use credstore_sdk::CredStoreClientV1;
use toolkit_db::DBProvider;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::ports::bundle_store::BundleStore;
use crate::domain::ports::repo_sync::RepoSyncPort;
use crate::domain::repos::{
    BundlesRepository, CustomPlansRepository, ProductsRepository, SshKeysRepository,
    TestReposRepository,
};

mod bundles;
mod custom_plans;
mod plans;
mod plugin_registry;
mod products;
mod repos;
mod ssh_keys;
// Private, like its siblings. It was `pub mod` so that
// `tests/multi_branch.rs` could name `domain::service::sync_cache`; review
// finding #38 made `domain` itself `pub(crate)`, so that path stopped working
// and the test now reaches the type through `qa_catalog::SyncCache` (see
// `lib.rs`). Nothing outside this module names the module any more -- only the
// `pub use` below.
mod sync_cache;
mod validation;

pub use bundles::BundlesService;
pub use custom_plans::CustomPlansService;
pub use plans::PlansService;
pub use plugin_registry::{ProductPluginPresence, QaProductRegistry, RegisteredProductPlugin};
pub use products::ProductsService;
pub use repos::ReposService;
pub use ssh_keys::SshKeysService;
pub use sync_cache::SyncCache;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod repos_tests;

#[cfg(test)]
mod plans_tests;

#[cfg(test)]
mod plugin_registry_tests;

#[cfg(test)]
mod products_tests;

#[cfg(test)]
mod ssh_keys_tests;

#[cfg(test)]
mod bundles_tests;

#[cfg(test)]
mod tests_tenant_scoping;

#[cfg(test)]
mod unscoped_read_guard_tests;

/// `DB` provider alias.
///
/// Unlike qa-environments (which aliases `DBProvider<DbError>` and has no
/// multi-statement flows), this gear parameterizes the provider with
/// `DomainError` directly: `transaction(...)` closures then run repository
/// calls (which return `DomainError`) as-is, and any `Err` rolls the
/// transaction back while preserving the domain variant (e.g.
/// `BranchCacheConflict`) instead of flattening it to a database error.
pub type DbProvider = DBProvider<DomainError>;

/// Authorization resource types and their PEP-supported properties.
pub mod resources {
    use super::ResourceType;
    use toolkit_security::pep_properties;

    pub const TEST_REPO: ResourceType = ResourceType::from_static(
        "qa.test_repo",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    pub const PLAN: ResourceType =
        ResourceType::from_static("qa.plan", &[pep_properties::OWNER_TENANT_ID]);

    pub const CUSTOM_PLAN: ResourceType = ResourceType::from_static(
        "qa.custom_plan",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    pub const PRODUCT: ResourceType = ResourceType::from_static(
        "qa.product",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    pub const SSH_KEY: ResourceType = ResourceType::from_static(
        "qa.ssh_key",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    pub const BUNDLE: ResourceType = ResourceType::from_static(
        "qa.bundle",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );
}

pub mod actions {
    pub const GET: &str = "get";
    pub const LIST: &str = "list";
    pub const CREATE: &str = "create";
    pub const UPDATE: &str = "update";
    pub const DELETE: &str = "delete";
    pub const SYNC: &str = "sync";
}

// DI Container - aggregates all domain services
//
// # Database Access
//
// Services acquire database connections internally via `DBProvider`. Callers
// do NOT touch database objects - they call service methods with business
// parameters only.
#[domain_model]
pub struct AppServices<R, C, P, K, B>
where
    R: TestReposRepository,
    C: CustomPlansRepository,
    P: ProductsRepository,
    K: SshKeysRepository,
    B: BundlesRepository,
{
    pub(crate) repos: ReposService<R, K>,
    pub(crate) plans: PlansService<R>,
    pub(crate) custom_plans: CustomPlansService<C>,
    pub(crate) products: ProductsService<P>,
    pub(crate) ssh_keys: SshKeysService<K>,
    pub(crate) bundles: BundlesService<B, R>,
    /// The product -> plugin resolver, and the catalogue of registered
    /// plugins behind `GET /qa/v1/product-plugins`.
    ///
    /// An `Arc` rather than a value because this is the *same* instance the
    /// gear registers in the `ClientHub` as `dyn QaProductPluginResolverV1`
    /// for `qa-environments` and `qa-runs`: one resolver per process, two
    /// kinds of consumer.
    pub(crate) plugin_registry: Arc<QaProductRegistry<P>>,
}

/// Everything `AppServices::new` needs beyond the repositories: shared
/// infrastructure handles plus the typed config values the services enforce.
pub struct ServiceDeps {
    pub(crate) db: Arc<DbProvider>,
    pub(crate) authz: Arc<dyn AuthZResolverClient>,
    pub(crate) credstore: Arc<dyn CredStoreClientV1>,
    pub(crate) sync_engine: Arc<dyn RepoSyncPort>,
    pub(crate) bundle_store: Arc<dyn BundleStore>,
    /// `QaCatalogConfig::repos_dir` — root of the per-repo working areas.
    pub(crate) repos_dir: PathBuf,
    /// `QaCatalogConfig::bundle_ttl_seconds` as a duration.
    pub(crate) bundle_ttl: time::Duration,
    /// Branch freshness cache + two-tier sync locks (TTL from
    /// `QaCatalogConfig::branch_freshness_ttl_seconds`).
    pub(crate) sync_cache: Arc<SyncCache>,
}

impl<R, C, P, K, B> AppServices<R, C, P, K, B>
where
    R: TestReposRepository + 'static,
    C: CustomPlansRepository,
    // `'static` because the registry is coerced to `Arc<dyn
    // ProductPluginPresence>` below, and a trait object's erased type must
    // outlive the object. Every concrete repository here is a unit struct, so
    // this constrains nothing real -- `R`, `K` and `B` already carry it.
    P: ProductsRepository + 'static,
    K: SshKeysRepository + 'static,
    B: BundlesRepository + 'static,
{
    pub fn new(
        repos_repo: Arc<R>,
        custom_plans_repo: Arc<C>,
        products_repo: Arc<P>,
        ssh_keys_repo: Arc<K>,
        bundles_repo: Arc<B>,
        plugin_registry: Arc<QaProductRegistry<P>>,
        deps: ServiceDeps,
    ) -> Self {
        let enforcer = PolicyEnforcer::new(deps.authz);

        Self {
            repos: ReposService::new(
                Arc::clone(&deps.db),
                Arc::clone(&repos_repo),
                Arc::clone(&ssh_keys_repo),
                Arc::clone(&deps.credstore),
                deps.sync_engine,
                deps.repos_dir.clone(),
                deps.sync_cache,
                enforcer.clone(),
            ),
            plans: PlansService::new(
                Arc::clone(&deps.db),
                Arc::clone(&repos_repo),
                deps.repos_dir.clone(),
                enforcer.clone(),
            ),
            custom_plans: CustomPlansService::new(
                Arc::clone(&deps.db),
                custom_plans_repo,
                enforcer.clone(),
            ),
            // The plugin-presence port is the registry, narrowed to one
            // method (ruling F-11). `plugin_registry` is already in scope
            // here, which is why this wiring needs no change in `gear.rs`.
            products: ProductsService::new(
                Arc::clone(&deps.db),
                products_repo,
                enforcer.clone(),
                Arc::clone(&plugin_registry) as Arc<dyn ProductPluginPresence>,
            ),
            ssh_keys: SshKeysService::new(
                Arc::clone(&deps.db),
                ssh_keys_repo,
                deps.credstore,
                enforcer.clone(),
            ),
            bundles: BundlesService::new(
                deps.db,
                bundles_repo,
                repos_repo,
                deps.bundle_store,
                deps.repos_dir,
                deps.bundle_ttl,
                enforcer,
            ),
            plugin_registry,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::resources;
    use toolkit_security::pep_properties;

    /// **A declared PEP property that no call site supplies is a constraint
    /// nothing can satisfy.**
    ///
    /// `qa.plan` declared `RESOURCE_ID` while every `resources::PLAN` call passes
    /// `None` -- plans are addressed by (repo, branch, path), not by a row id, so
    /// there is no id to supply. `qa.jira_config` already dropped its for the same
    /// reason. Review finding #27.
    #[test]
    fn qa_plan_declares_only_the_properties_its_call_sites_supply() {
        assert_eq!(
            resources::PLAN.supported_properties(),
            &[pep_properties::OWNER_TENANT_ID],
            "qa.plan has no row id to constrain on"
        );
    }
}
