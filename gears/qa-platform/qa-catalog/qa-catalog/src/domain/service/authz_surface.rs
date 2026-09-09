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
//! This gear's two calls of the second kind are the nil-tenant enumerating reads
//! `repos::ReposService::list_refresh_targets` and
//! `bundles::BundlesService::tenants_with_expired_bundles`, which read through
//! [`crate::domain::elevated::enumeration_scope`] - see that module's doc.
//! Neither appears below. Every write either one feeds *is* authorized
//! normally, under `system_actor::for_branch_refresh` or
//! `system_actor::for_bundle_delete`, and those pairs (`qa.test_repo`/`sync`,
//! `qa.bundle`/`delete`) are listed.
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
    // `qa.test_repo` - `repos`, plus the precondition reads `plans` and
    // `bundles` make before touching a repository's working copy. `sync` is
    // also what the branch-cache refresher enforces under
    // `system_actor::for_branch_refresh`.
    (resources::TEST_REPO_NAME, actions::CREATE),
    (resources::TEST_REPO_NAME, actions::DELETE),
    (resources::TEST_REPO_NAME, actions::GET),
    (resources::TEST_REPO_NAME, actions::LIST),
    (resources::TEST_REPO_NAME, actions::SYNC),
    (resources::TEST_REPO_NAME, actions::UPDATE),
    // `qa.plan` - `plans`. Read-only by construction: plans are discovered
    // from a synced working copy, so there is nothing to create, update or
    // delete under this type.
    (resources::PLAN_NAME, actions::GET),
    (resources::PLAN_NAME, actions::LIST),
    // `qa.custom_plan` - `custom_plans`, a full CRUD surface over
    // user-composed persisted plans.
    (resources::CUSTOM_PLAN_NAME, actions::CREATE),
    (resources::CUSTOM_PLAN_NAME, actions::DELETE),
    (resources::CUSTOM_PLAN_NAME, actions::GET),
    (resources::CUSTOM_PLAN_NAME, actions::LIST),
    (resources::CUSTOM_PLAN_NAME, actions::UPDATE),
    // `qa.product` - `products`, plus `plugin_registry`, whose two reads use
    // this type deliberately: `plugin_for` under `get` and
    // `list_registered_plugins` under `list`, with its reasoning inline at the
    // call site. One pair each; they are not a separate resource type.
    (resources::PRODUCT_NAME, actions::CREATE),
    (resources::PRODUCT_NAME, actions::DELETE),
    (resources::PRODUCT_NAME, actions::GET),
    (resources::PRODUCT_NAME, actions::LIST),
    (resources::PRODUCT_NAME, actions::UPDATE),
    // `qa.ssh_key` - `ssh_keys`, plus the per-repository credential read
    // `repos` makes before a sync. No `update`: the routes are create, list
    // and delete only, and a key's material lives in credstore, so a rotation
    // is a delete-then-create. `get` has no route of its own either - it is
    // the scope `delete_ssh_key` compiles to resolve the row's credstore
    // reference, and the one `repos` compiles before a sync.
    (resources::SSH_KEY_NAME, actions::CREATE),
    (resources::SSH_KEY_NAME, actions::DELETE),
    (resources::SSH_KEY_NAME, actions::GET),
    (resources::SSH_KEY_NAME, actions::LIST),
    // `qa.bundle` - `bundles`. No `list`: a bundle is ephemeral and its only
    // route is a `get` by id. `delete` is what the GC purge enforces, once per
    // tenant, under `system_actor::for_bundle_delete`.
    (resources::BUNDLE_NAME, actions::CREATE),
    (resources::BUNDLE_NAME, actions::DELETE),
    (resources::BUNDLE_NAME, actions::GET),
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
/// `qa.bundle`, and renaming them to GTS type ids is precluded because they are
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
    resources::BUNDLE_NAME,
    resources::CUSTOM_PLAN_NAME,
    resources::PLAN_NAME,
    resources::PRODUCT_NAME,
    resources::SSH_KEY_NAME,
    resources::TEST_REPO_NAME,
];

/// How many `.access_scope(` call sites this crate's non-test source has.
///
/// Not a summary of [`ENFORCED`] and not derivable from it: several call sites
/// enforce the same pair (`qa-catalog`'s precondition reads), and one call site can
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
const EXPECTED_ACCESS_SCOPE_SITES: usize = 38;

#[cfg(test)]
#[path = "authz_surface_tests.rs"]
mod tests;

/// **All four gears' copies of `authz_surface_tests.rs` are byte-identical.**
/// Hosted here rather than in each gear, because a per-gear copy of a
/// self-hashing test is a contradiction - see the file's own header.
#[cfg(test)]
#[path = "authz_surface_parity_tests.rs"]
mod parity_tests;
