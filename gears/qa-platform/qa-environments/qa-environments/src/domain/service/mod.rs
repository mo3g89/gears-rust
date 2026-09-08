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
use std::sync::atomic::{AtomicBool, Ordering};

use authz_resolver_sdk::AuthZResolverClient;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::ResourceType;
use credstore_sdk::CredStoreClientV1;
use toolkit_db::{DBProvider, DbError};
use toolkit_macros::domain_model;

use crate::domain::ports::metrics::{ObservationMetrics, PluginMetrics};
use crate::domain::ports::{ProductPluginPort, RunnerSecretWriter};
use crate::domain::repos::{EnvironmentsRepository, LeasesRepository, VariablesRepository};

/// The `(resource_type, action)` pairs this gear's PEP enforces, and the
/// distinct resource types among them. The source side of the permission
/// catalog's anti-drift test - review finding #1.
pub mod authz_surface;
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

/// Task 39's own tests: what the observation cycle reports about itself, read
/// back through a real `OpenTelemetry` pipeline.
#[cfg(test)]
mod environments_metrics_tests;

/// Task 15's own tests: what reaches the product plugin, and what its answer
/// writes into both column sets.
#[cfg(test)]
mod observation_projection_tests;

#[cfg(test)]
mod variables_tests;

/// What `GET /qa/v1/variables` -- and therefore what a run -- actually
/// receives. Both properties are about how `list_for_env` *composes* two paged
/// reads, so neither is expressible at the repository level.
#[cfg(test)]
mod variables_paging_tests;

#[cfg(test)]
mod tests_tenant_scoping;

#[cfg(test)]
mod unscoped_read_guard_tests;

#[cfg(test)]
mod resources_tests;

/// The one thing this gear's copy of [`emit`] owes the two gears it was copied
/// from: that it is still the same guard.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "emit_tests.rs"]
mod emit_tests;

/// Run one metric emission so that it cannot fail the path it is measuring.
///
/// **Copied verbatim, body for body, from qa-runs' and qa-insights'
/// `domain::service::emit`.** This is the third copy, and
/// `emit_tests::the_guard_is_byte_for_byte_the_guard_the_sibling_gears_run` is
/// what stops it becoming a third *variant*: it lifts the body out of all
/// three files and asserts the three are identical, so a fix applied to one is
/// visible as a failure in the other two. The docs are deliberately not
/// compared -- each gear's argument is about its own paths, and a whole-file
/// hash (the parity shape Phase 7 used for the permission catalog) would
/// therefore fail on prose rather than on behaviour.
///
/// # Why this exists when the port's contract already forbids failing
///
/// [`crate::domain::ports::metrics`] states the contract -- an implementation
/// must not panic, must not block, and has no error to propagate by
/// construction -- and [`crate::infra::metrics::QaEnvironmentsMetricsMeter`]
/// satisfies it structurally: every method is one `add` or one `record` on an
/// instrument it already holds, with no `?`, no fallible lookup and no panic
/// path.
///
/// Neither of those covers the call site.
/// [`EnvironmentsService`] takes an `Arc<dyn ObservationMetrics>`, so what it
/// holds is whatever was injected: a later adapter, a different gear's adapter
/// copied across, a `debug_assert!` somebody adds inside one. "Metrics must not
/// change behaviour" is a property of *the observation cycle*, and a property
/// of that path cannot be discharged by a promise written in another module --
/// a promise is exactly what a defect breaks. So the emission is guarded here,
/// where the path is, and
/// `a_broken_metrics_adapter_does_not_fail_an_observation_cycle` drives a
/// deliberately panicking port through it.
///
/// # It is silent
///
/// A caught panic is dropped rather than logged. Logging here would be a log
/// line **per emission**, and on the per-environment path that is one line per
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
/// **The latch is the caller's, not a global**, and the one service that emits
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

/// `DB` provider alias (mirrors users-info).
pub type DbProvider = DBProvider<DbError>;

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
        PLATFORM_NAME,
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// [`PLATFORM`]'s name as a `&'static str`, for the reason this module's
    /// header cites. **The string is `qa.platform`, not the aggregate's Rust
    /// name** — see [`PLATFORM`]'s own doc for why the rename to `Environment`
    /// deliberately stopped at the type and left the PDP string alone.
    pub const PLATFORM_NAME: &str = "qa.platform";

    pub const VARIABLE: ResourceType = ResourceType::from_static(
        VARIABLE_NAME,
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// [`VARIABLE`]'s name as a `&'static str`, for the reason this module's
    /// header cites.
    pub const VARIABLE_NAME: &str = "qa.variable";

    pub const LEASE: ResourceType = ResourceType::from_static(
        LEASE_NAME,
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// [`LEASE`]'s name as a `&'static str`, for the reason this module's
    /// header cites.
    pub const LEASE_NAME: &str = "qa.lease";
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
                  port, the product-plugin port, the two metrics ports, and the one scalar \
                  knob). Grouping them into a parameter struct would move the same eleven \
                  names one indirection away without removing any of them, and \
                  `AppServices::new` has exactly one caller (`gear.rs`'s `init`)."
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
        metrics: Option<Arc<dyn ObservationMetrics>>,
        plugin_metrics: Option<Arc<dyn PluginMetrics>>,
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
                metrics,
                plugin_metrics,
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
