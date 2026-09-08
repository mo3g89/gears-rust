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
use std::sync::atomic::{AtomicBool, Ordering};

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

/// The `(resource_type, action)` pairs this gear's PEP enforces, and the
/// distinct resource types among them. The source side of the permission
/// catalog's anti-drift test - review finding #1.
pub mod authz_surface;
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

#[cfg(test)]
mod resources_tests;

/// Run one metric emission so that it cannot fail the path it is measuring.
///
/// **Copied verbatim, body for body, from qa-runs', qa-insights' and
/// qa-environments' `domain::service::emit`.** This is the fourth copy, and
/// qa-environments'
/// `emit_tests::the_guard_is_byte_for_byte_the_guard_the_sibling_gears_run` is
/// what stops it becoming a fourth *variant*: it lifts the body out of all four
/// files and asserts they are identical, so a fix applied to one is visible as
/// a failure in the other three. The docs are deliberately not compared -- each
/// gear's argument is about its own paths, and a whole-file hash (the parity
/// shape Phase 7 used for the permission catalog) would therefore fail on prose
/// rather than on behaviour.
///
/// The parity test lives in qa-environments rather than here because that is
/// where it was written, and one host reading four files is cheaper than four
/// hosts reading four files each. A shared crate is the structural answer and
/// is out of this task's scope; the observability plan puts one there.
///
/// # Why this exists when the port's contract already forbids failing
///
/// [`crate::domain::ports::metrics`] states the contract -- an implementation
/// must not panic, must not block, and has no error to propagate by
/// construction -- and [`crate::infra::metrics::QaCatalogMetricsMeter`]
/// satisfies it structurally: every method is one `add` or one `record` on an
/// instrument it already holds, with no `?`, no fallible lookup and no panic
/// path.
///
/// Neither of those covers the call site. [`QaProductRegistry`] takes an
/// `Arc<dyn PluginResolutionMetrics>`, so what it holds is whatever was
/// injected: a later adapter, a different gear's adapter copied across, a
/// `debug_assert!` somebody adds inside one. "Metrics must not change
/// behaviour" is a property of *plugin resolution*, and a property of that path
/// cannot be discharged by a promise written in another module -- a promise is
/// exactly what a defect breaks. So the emission is guarded here, where the
/// path is, and `a_broken_metrics_adapter_does_not_fail_a_resolution` drives a
/// deliberately panicking port through it.
///
/// **What this path costs if it is not guarded is larger here than the call
/// count suggests.** Resolution is on the critical path of every observation
/// qa-environments performs and every product dispatch qa-runs makes, and it is
/// reached across the `ClientHub` from another gear's background ticker. A
/// panic raised here does not merely lose a metric: it unwinds into a caller
/// that is holding a database connection and a loop over every registered
/// environment.
///
/// # It is silent
///
/// A caught panic is dropped rather than logged. Logging here would be a log
/// line **per emission**, which on the observation path is one line per
/// registered environment per cycle -- the failure mode the observability
/// constraints name explicitly, arriving exactly when the process can least
/// absorb it. The panic hook has already run by the time control returns here,
/// so the panic itself is not invisible: it reaches stderr like any other.
///
/// [`std::panic::AssertUnwindSafe`] is sound for the reason it is normally
/// unsound: the state a panicking emission may have left inconsistent is that
/// implementation's own instrument state, and nothing in this crate ever reads
/// it back. A metric this gear cannot record is a metric this gear drops.
///
/// # It latches off, and that is the half the guard alone does not give
///
/// Catching is not enough on its own. A *persistently* broken adapter -- a
/// poisoned instrument lock is the realistic shape -- panics on **every** call,
/// and the default panic hook writes a line to stderr each time before control
/// returns here. So the first caught panic latches `silenced`, and every later
/// emission through that latch returns without calling anything. Deliberately
/// permanent: an emission that panicked once has no claim on being retried, the
/// alternatives (a rate limit, a backoff) are state and policy on a path whose
/// whole contract is that it changes nothing, and a metric that stops is a
/// visibly flat series -- a better failure than a log flood. Nothing resets it;
/// a restart does.
///
/// **The latch is the caller's, not a global**, and the one type that emits
/// owns one. Per service rather than process-wide because a global would make
/// the broken-adapter test silence every other metric test in the binary under
/// a threaded `cargo test`, where this crate's suites share a process; every
/// gate in this repository runs this crate through nextest, which is
/// process-per-test, so that would not have been caught here.
///
/// # Precondition: this crate unwinds
///
/// `catch_unwind` catches nothing under `panic = "abort"`, where the first
/// panicking emission would take the process instead. The workspace sets
/// `panic = "unwind"` explicitly in `[profile.release]`, and the dev and test
/// profiles inherit the same default, so the guard is live in every profile
/// this gear is built under today. **Changing that setting silently disables
/// everything documented above**: the broken-adapter test would abort rather
/// than fail, so the suite would report a crashed binary rather than a
/// regression here.
///
/// `Relaxed` on both accesses: the latch orders nothing and guards no data. The
/// whole cost of the weakest ordering is that a racing thread may read `false`
/// once more and produce one more panic, against paying for a fence on a path
/// whose contract is that it costs nothing.
pub(in crate::domain::service) fn emit(silenced: &AtomicBool, record: impl FnOnce()) {
    if silenced.load(Ordering::Relaxed) {
        return;
    }
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(record)).is_err() {
        silenced.store(true, Ordering::Relaxed);
    }
}

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
///
/// Each descriptor is built from a sibling `*_NAME` `&str` const rather than
/// from an inline literal, so the PDP resource string has one declaration and
/// two consumers: the descriptor the PEP is called with, and
/// [`authz_surface::ENFORCED`]. qa-insights' `resources::TEST_RESULT_NAME` -
/// its own doc, in that gear's `domain/service/mod.rs` - is the precedent and
/// carries the reason the descriptor cannot supply the string itself. Cited by
/// name rather than by line: nothing validates a `:N` in this repository, and
/// this citation had already drifted twice as a line range.
pub mod resources {
    use super::ResourceType;
    use toolkit_security::pep_properties;

    pub const TEST_REPO: ResourceType = ResourceType::from_static(
        TEST_REPO_NAME,
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// [`TEST_REPO`]'s name as a `&'static str`, for the reason this module's
    /// header cites.
    pub const TEST_REPO_NAME: &str = "qa.test_repo";

    pub const PLAN: ResourceType =
        ResourceType::from_static(PLAN_NAME, &[pep_properties::OWNER_TENANT_ID]);

    /// [`PLAN`]'s name as a `&'static str`, for the reason this module's header
    /// cites.
    pub const PLAN_NAME: &str = "qa.plan";

    pub const CUSTOM_PLAN: ResourceType = ResourceType::from_static(
        CUSTOM_PLAN_NAME,
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// [`CUSTOM_PLAN`]'s name as a `&'static str`, for the reason this module's
    /// header cites.
    pub const CUSTOM_PLAN_NAME: &str = "qa.custom_plan";

    pub const PRODUCT: ResourceType = ResourceType::from_static(
        PRODUCT_NAME,
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// [`PRODUCT`]'s name as a `&'static str`, for the reason this module's
    /// header cites.
    pub const PRODUCT_NAME: &str = "qa.product";

    pub const SSH_KEY: ResourceType = ResourceType::from_static(
        SSH_KEY_NAME,
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// [`SSH_KEY`]'s name as a `&'static str`, for the reason this module's
    /// header cites.
    pub const SSH_KEY_NAME: &str = "qa.ssh_key";

    pub const BUNDLE: ResourceType = ResourceType::from_static(
        BUNDLE_NAME,
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// [`BUNDLE`]'s name as a `&'static str`, for the reason this module's
    /// header cites.
    pub const BUNDLE_NAME: &str = "qa.bundle";
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
