//! The `(resource_type, action)` pairs this gear's PEP actually enforces.
//!
//! This list exists so `crate::gts::permissions` can be checked against the
//! enforcement path rather than against a reader's memory. It is the *source*
//! side of the anti-drift test: a pair enforced here and absent from the
//! catalog means a caller can be refused an action no role can grant, and a
//! catalog entry absent here means a grant that authorizes nothing.
//!
//! **Derived from the `resources::*` / `actions::*` consts, never from type
//! names.** `qa.platform` in qa-environments is the reason that rule is
//! written down: the aggregate is called `Environment` and the PDP string was
//! deliberately left as `qa.platform`, so a list built from the Rust type name
//! would name a resource type no policy mentions.
//!
//! The list is measured, not recalled: `authz_surface_tests.rs` scans this
//! crate's own source for `access_scope` call sites and fails in both
//! directions - a call site with no entry, and an entry no call site reaches.
//!
//! # Elevated and system-actor paths are in; PDP-less ones are not
//!
//! A background task whose pair is absent from the catalog is a task that is
//! inert by configuration, so every context a system-actor factory mints and
//! then hands to the enforcer contributes its pair here. What contributes
//! nothing is a call site that never reaches `access_scope` at all: a scope
//! taken straight from `AccessScope::allow_all()` or `for_tenant` asks the PDP
//! nothing, so there is no decision for a policy to grant or refuse.
//!
//! **This gear has no `domain::elevated` module.** Its one cross-tenant
//! enumerating read - `EnvironmentsRepository::list_all_with_tenant`, for the
//! observation ticker - passes the ratified `AccessScope::allow_all()` literal
//! inline in `domain::service::environments` instead, inside the block comment
//! that `unscoped_read_guard_tests` pins as this crate's only such exception.
//! It reaches no PDP and contributes no pair. The per-environment work that
//! follows it runs under `system_actor::for_observation` and is authorized
//! normally, which is where `qa.platform`/`update` below comes from.
//!
//! # `resources::PLATFORM_NAME` is this gear's own trap
//!
//! The header's warning is about *this* gear: the aggregate is `Environment`,
//! the PDP string is `gts.cf.qa.environments.platform.v1~`, and every entry
//! below is generated from `resources::PLATFORM_NAME`.
//!
//! # Why `ENFORCED` carries `#[allow(dead_code)]`, and `RESOURCE_TYPES` carries `#[cfg(test)]`
//!
//! [`ENFORCED`] has no non-test consumer, and none is waiting on it: it is read
//! under `cfg(test)` only, by the anti-drift test that compares this list to
//! the generated catalog. An `#[expect]` cannot express this - `clippy
//! --all-targets` builds this crate twice and the test build *does* use it, so
//! the expectation would be unfulfilled there and fulfilled in the lib build.
//! `#[allow(dead_code)]` is the annotation that survives both builds.
//!
//! [`RESOURCE_TYPES`] used to carry the same annotation for the same reason,
//! before it got the non-test consumer it was written for: each of its entries
//! now has a stub type-schema declared in [`crate::gts::authz_types`], which
//! `types-registry::init()` registers at boot. That registration is driven by
//! the `#[gts_type_schema]` declarations there, not by reading this const, so
//! the const itself is still read under `cfg(test)` only, by the scan's
//! equality test - which is why it is annotated `#[cfg(test)]` rather than
//! `#[allow(dead_code)]`: with no lib-build definition to warn about, there is
//! nothing for `#[expect]`'s two-build problem to bite on either. See that
//! item's own doc for the registration.
//!
//! Review finding #1.

use super::{actions, resources};

/// Every `(resource_type, action)` pair this gear's PEP enforces.
///
/// Grouped by resource type in the order [`super::resources`] declares them,
/// each group's actions alphabetical, with the modules whose `access_scope`
/// calls produced them.
#[allow(
    dead_code,
    reason = "read under cfg(test) only - see this module's header"
)]
pub const ENFORCED: &[(&str, &str)] = &[
    // `qa.platform` - `environments`, plus the tenancy prechecks `leases` and
    // `variables` each compile their own PLATFORM scope for rather than
    // reusing their own type's. `update` is also what the observation ticker
    // enforces under `system_actor::for_observation`.
    (resources::PLATFORM_NAME, actions::CREATE),
    (resources::PLATFORM_NAME, actions::DELETE),
    (resources::PLATFORM_NAME, actions::GET),
    (resources::PLATFORM_NAME, actions::LIST),
    (resources::PLATFORM_NAME, actions::UPDATE),
    // `qa.variable` - `variables`. `create` and `update` both come from
    // `upsert`, which resolves which of the two it is from a natural-key probe
    // *before* asking the PDP, so that a create is never authorized under
    // `update` or the reverse; `get` is that probe's own scope.
    (resources::VARIABLE_NAME, actions::CREATE),
    (resources::VARIABLE_NAME, actions::DELETE),
    (resources::VARIABLE_NAME, actions::GET),
    (resources::VARIABLE_NAME, actions::LIST),
    (resources::VARIABLE_NAME, actions::UPDATE),
    // `qa.lease` - `leases`, plus the lease read `environments` makes on
    // delete. **No `create`**: a lease row is not a resource a caller creates,
    // it is compare-and-set into existence by `acquire`, so `acquire` and
    // `release` are the two verbs and there is no CRUD create to grant. No
    // `list` or `delete` either, for the same reason - a lease is only ever
    // addressed by its environment.
    (resources::LEASE_NAME, actions::ACQUIRE),
    (resources::LEASE_NAME, actions::GET),
    (resources::LEASE_NAME, actions::RELEASE),
];

/// The distinct resource types in [`ENFORCED`].
///
/// This list is the drift guard against [`ENFORCED`]: the platform
/// RBAC role-definition validator resolves a rule's `target_type` through the
/// types registry, so a resource type it cannot resolve is a permission no
/// role definition can target.
///
/// Each entry has a stub declared in [`crate::gts::authz_types`], which
/// `types-registry::init()` registers at boot from the `inventory`
/// collection — the same route `crate::gts::permissions` takes.
/// `authz_surface_tests` pins this list to [`ENFORCED`] in both directions
/// and asserts every entry resolves as a registered schema.
///
/// **Test-only, and `#[cfg(test)]` rather than `#[allow(dead_code)]` says so in
/// the type system.** The registration is driven by the `#[gts_type_schema]`
/// declarations in [`crate::gts::authz_types`], not by this list, so the list
/// genuinely has no non-test consumer. The annotation this replaces carried the
/// reason "test-only consumer by design", which was true, alongside a doc claiming
/// no such registration could be built from these strings — which stopped being
/// true when it was.
#[cfg(test)]
const RESOURCE_TYPES: &[&str] = &[
    resources::LEASE_NAME,
    resources::PLATFORM_NAME,
    resources::VARIABLE_NAME,
];

/// How many `.access_scope(` call sites this crate's non-test source has.
///
/// Not a summary of [`ENFORCED`] and not derivable from it: several call sites
/// enforce the same pair (`qa-environments`'s precondition reads), and one call site can
/// contribute several pairs (a forwarded action resolved through its helper's
/// callers). This counts the *calls*, and the scan's forward test asserts it
/// reaches exactly this many - so a scan that silently stops reading part of
/// the crate fails rather than passing against a smaller set.
///
/// **It moves whenever a call site is added or removed**, including one that
/// enforces a pair already listed above. Re-run the scan and take the number
/// from its failure message; do not adjust it to make a red test green without
/// checking what changed.
#[cfg(test)]
const EXPECTED_ACCESS_SCOPE_SITES: usize = 18;

#[cfg(test)]
#[path = "authz_surface_tests.rs"]
mod tests;
