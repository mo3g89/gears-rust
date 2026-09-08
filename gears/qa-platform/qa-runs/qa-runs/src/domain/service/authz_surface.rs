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
//! `LogArchive` compiles its own `qa.run` scopes
//! (`crate::infra::logs::archive`) under `system_actor::for_log_archive`:
//! `dispatch` for the archive write, `get` for the resume read. Both pairs are
//! already reached by the run surface, so neither adds a row - but it is why
//! the scan reads all of `src/` rather than `domain` alone, and why a
//! `domain`-only measurement would have been the wrong one.
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
/// `qa.queue_entry`, and renaming them to GTS type ids is precluded because they are
/// what a deployment's policies are written against. So this list is measured
/// and pinned to [`ENFORCED`] by the scan, but no production code consumes it;
/// the consequence -- no custom role can target a QA resource type -- is
/// recorded as a follow-up in
/// `docs/superpowers/specs/2026-09-05-review-remediation-design.md` section 12.
#[allow(
    dead_code,
    reason = "test-only consumer by design - see this item's doc"
)]
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
