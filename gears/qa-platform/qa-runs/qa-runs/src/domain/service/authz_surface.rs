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
//! This gear's calls of the second kind are the six nil-tenant ticker
//! enumerations that read through [`crate::domain::elevated::enumeration_scope`] - see that
//! module's doc. The per-row work each one finds *is* authorized normally,
//! under `system_actor`'s tenant-bound factories (`for_dispatch`,
//! `for_ttl_expiry`, `for_timeout_enforcement`, `for_schedule_fire`), and
//! those pairs are listed.
//!
//! # Two call sites are in `infra`, not `domain`
//!
//! `LogArchive` compiles its own `qa.run` scopes
//! (`crate::infra::logs::archive`) under `system_actor::for_log_archive`:
//! `dispatch` for the archive write, `get` for the resume read. Both pairs are
//! already reached by the run surface, so neither adds a row - but it is why
//! the scan reads all of `src/` rather than `domain` alone, and why a
//! `domain`-only measurement would have been the wrong one.
//!
//! # Why both items carry `#[allow(dead_code)]`
//!
//! Their consumers arrive with the companion tasks: the anti-drift test that
//! compares this list to the generated catalog reads [`ENFORCED`] under
//! `cfg(test)` only, and [`RESOURCE_TYPES`] is read by the stub type-schema
//! registration once that lands. An `#[expect]` cannot express this - `clippy
//! --all-targets` builds this crate twice and the test build *does* use both,
//! so the expectation would be unfulfilled there and fulfilled in the lib
//! build. The `allow`s come off with the commits that add the consumers.
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
    reason = "no non-test consumer yet - see this module's header"
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
    // `qa_schedule_ticks`. `fire` is the background ticker's own action,
    // enforced under `system_actor::for_schedule_fire`, and it is separate so
    // a deployment can grant the sweep without granting schedule edits.
    (resources::SCHEDULE_NAME, actions::CREATE),
    (resources::SCHEDULE_NAME, actions::DELETE),
    (resources::SCHEDULE_NAME, actions::FIRE),
    (resources::SCHEDULE_NAME, actions::GET),
    (resources::SCHEDULE_NAME, actions::LIST),
    (resources::SCHEDULE_NAME, actions::UPDATE),
];

/// The distinct resource types in [`ENFORCED`].
///
/// Consumed to register one stub type schema per resource type: the platform
/// RBAC role-definition validator resolves a rule's `target_type` through the
/// types registry, so a resource type missing from this list is a permission
/// no role definition can target. `ledger`'s `labels::ALL`
/// (`gears/bss/ledger/ledger/src/authz.rs:109`) exists for the same reason.
#[allow(
    dead_code,
    reason = "no non-test consumer yet - see this module's header"
)]
pub(crate) const RESOURCE_TYPES: &[&str] = &[
    resources::QUEUE_ENTRY_NAME,
    resources::RUN_NAME,
    resources::SCHEDULE_NAME,
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "authz_surface_tests.rs"]
mod tests;
