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
//! This gear's calls of the second kind are the ticker enumerations: **five**
//! [`crate::domain::elevated::enumeration_scope`] call sites, four in
//! `service::dispatch` and one in `service::schedules`, reached under the
//! **six** nil-tenant contexts `domain::system_actor` mints. The two counts are
//! not the same number and are stated separately on purpose - one factory can
//! back several enumerating reads. See `domain::elevated`'s doc. The per-row
//! work each enumeration finds *is* authorized normally, under
//! `system_actor`'s tenant-bound factories (`for_dispatch`, `for_ttl_expiry`,
//! `for_timeout_enforcement`, `for_schedule_fire`), and those pairs are listed.
//!
//! # Two call sites are in `infra`, not `domain`
//!
//! `LogArchive` compiles its own `resources::RUN_NAME` scopes
//! (`crate::infra::logs::archive`) under `system_actor::for_log_archive`:
//! `dispatch` for the archive write, `get` for the resume read. Both pairs are
//! already reached by the run surface, so neither adds a row - but it is why
//! the scan reads all of `src/` rather than `domain` alone, and why a
//! `domain`-only measurement would have been the wrong one.
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
pub(crate) const ENFORCED: &[(&str, &str)] = &[
    // `qa.run` - `runs`, `launch`, `admission`, `dispatch`, `ingest` and
    // `crate::infra::logs::archive`. `dispatch` is the write verb for a run's
    // own child data as well as for the dispatch itself, which is why the log
    // archive enforces it. No `update` or `delete`: a run is never edited or
    // removed, it is cancelled and re-run.
    (resources::RUN_NAME, actions::CANCEL),
    (resources::RUN_NAME, actions::CREATE),
    (resources::RUN_NAME, actions::DISPATCH),
    (resources::RUN_NAME, actions::GET),
    (resources::RUN_NAME, actions::LIST),
    (resources::RUN_NAME, actions::RERUN),
    // `qa.queue_entry` - `runs`, `admission`, `dispatch` and `ingest` over
    // `qa_run_queue`; `launch` reaches the queue only through `admission`. A scope
    // compiled for this type is never passed to a `qa_runs` query, which is
    // why the two surfaces are listed apart even where the actions coincide.
    // `force_start` is the queue operator action; no `rerun`, which is a run's
    // verb and not an entry's.
    (resources::QUEUE_ENTRY_NAME, actions::CANCEL),
    (resources::QUEUE_ENTRY_NAME, actions::CREATE),
    (resources::QUEUE_ENTRY_NAME, actions::DISPATCH),
    (resources::QUEUE_ENTRY_NAME, actions::FORCE_START),
    (resources::QUEUE_ENTRY_NAME, actions::GET),
    (resources::QUEUE_ENTRY_NAME, actions::LIST),
    // `qa.schedule` - `schedules`, over `qa_schedules` and
    // `qa_schedule_ticks`. `fire` is the background firing ticker's own
    // action and `check` is the background referential-check ticker's,
    // enforced under `system_actor::for_schedule_fire`; both are separate
    // from `create`/`update`/`delete` so a deployment can grant either
    // background sweep without granting schedule edits.
    (resources::SCHEDULE_NAME, actions::CHECK),
    (resources::SCHEDULE_NAME, actions::CREATE),
    (resources::SCHEDULE_NAME, actions::DELETE),
    (resources::SCHEDULE_NAME, actions::FIRE),
    (resources::SCHEDULE_NAME, actions::GET),
    (resources::SCHEDULE_NAME, actions::LIST),
    (resources::SCHEDULE_NAME, actions::UPDATE),
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
pub(crate) const RESOURCE_TYPES: &[&str] = &[
    resources::QUEUE_ENTRY_NAME,
    resources::RUN_NAME,
    resources::SCHEDULE_NAME,
];

/// How many `.access_scope(` call sites this crate's non-test source has.
///
/// Not a summary of [`ENFORCED`] and not derivable from it: several call sites
/// enforce the same pair (`qa-runs`'s precondition reads), and one call site can
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
const EXPECTED_ACCESS_SCOPE_SITES: usize = 12;

#[cfg(test)]
#[path = "authz_surface_tests.rs"]
mod tests;
