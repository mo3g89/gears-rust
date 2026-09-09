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
//! # `qa.platform` is this gear's own trap
//!
//! The header's warning is about *this* gear: the aggregate is `Environment`,
//! the PDP string is `qa.platform`, and every entry below is generated from
//! `resources::PLATFORM_NAME`.
//!
//! # Why both items carry `#[allow(dead_code)]`
//!
//! Neither has a non-test consumer, and neither is waiting for one.
//! [`ENFORCED`] is read under `cfg(test)` only, by the anti-drift test that
//! compares this list to the generated catalog; [`RESOURCE_TYPES`] likewise, by
//! the scan's equality test - and it is **not** getting the stub type-schema
//! registration it was written for, which is the non-test consumer it was
//! waiting on (see that item's own doc for what settled that). An `#[expect]`
//! cannot express this - `clippy --all-targets` builds this crate twice and the
//! test build *does* use both, so the expectation would be unfulfilled there
//! and fulfilled in the lib build.
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
/// Written to drive one stub type-schema registration per resource type: the
/// platform RBAC role-definition validator resolves a rule's `target_type`
/// through the types registry, so a resource type it cannot resolve is a
/// permission no role definition can target -- which is why `ledger` registers
/// a stub per authz label from `labels::ALL`
/// (`gears/bss/ledger/ledger/src/authz.rs:109`).
///
/// **No such registration exists here, and none can be built from these
/// strings.** A types-registry type-schema id must end with `~`
/// (`types-registry-sdk/src/models.rs:53-55`), these are plain strings like
/// `qa.lease`, and renaming them to GTS type ids is precluded because they are
/// what a deployment's policies are written against. So this list is measured
/// and pinned to [`ENFORCED`] by the scan, but no production code consumes it;
/// the consequence -- no custom role can target a QA resource type -- is
/// recorded as a follow-up in
/// `gears/qa-platform/docs/DESIGN.md` section 3.10, "Authorization surface".
#[allow(
    dead_code,
    reason = "test-only consumer by design - see this item's doc"
)]
pub const RESOURCE_TYPES: &[&str] = &[
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
