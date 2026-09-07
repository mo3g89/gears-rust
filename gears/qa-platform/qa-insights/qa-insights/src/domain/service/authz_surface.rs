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
//! The calls of the second kind, as measured 2026-09-07 by grepping this
//! crate's production source for every `AccessScope::` constructor. **Apply
//! the rule above rather than this list**: it is current at that date and not
//! guaranteed closed, and a `for_tenant` call site added tomorrow is one of
//! these whether or not anyone adds it here.
//!
//! * `tenants::TenantDirectory`'s cross-tenant enumeration - the only path
//!   that reads through [`crate::domain::elevated::enumeration_scope`]'s
//!   `AccessScope::allow_all()`. See that module's doc.
//! * The event-ingest and reconcile sweeps, which scope with
//!   `AccessScope::for_tenant` (`qa-insights/src/domain/service/reconcile.rs:690`
//!   for the sweep itself and `:831` for its watermark advance).
//!   `qa.test_result`/`rebuild` below is the operator-triggered replay, not
//!   the sweep.
//! * The public HMAC collect callback
//!   (`qa-insights/src/domain/service/collect.rs:735`) - see the next section.
//! * `notify::NotifyService::log`'s audit-row append
//!   (`qa-insights/src/domain/service/notify.rs:1193`). The write always
//!   targets the same tenant's own log whichever action compiled the scope
//!   that authorized the operation being audited, so it takes
//!   `AccessScope::for_tenant` rather than re-deriving one.
//! * The leader-election claim rows
//!   (`qa-insights/src/infra/leader/claim_row.rs:379`, `:466` and `:498`),
//!   which scope with `AccessScope::for_tenant(CLAIM_TENANT)` - an
//!   infrastructure lock held in a reserved tenant, not a tenant resource any
//!   deployment policy is written about.
//!
//! Each ticker's per-tenant pass that follows the enumeration *is* authorized
//! normally, under `system_actor`'s tenant-bound factories.
//!
//! # The two collect routes are not the same call site
//!
//! `qa.test_result`/`collect` is enforced on the *authenticated* trigger
//! (`api::rest::handlers::collect`'s `trigger_collect`). The public HMAC route
//! has no `SecurityContext` to enforce against and so compiles no
//! **PDP-derived** scope - it is one of the `for_tenant` paths above.
//! `CollectService::record_count` does build an `AccessScope`
//! (`AccessScope::for_tenant(tenant_id)`,
//! `qa-insights/src/domain/service/collect.rs:735`), off the tenant its own
//! signed callback URL carries; what it never does is ask the PDP, so there is
//! no decision to grant or refuse and it is not a pair.
//!
//! # `qa.notification_config` is the PEP string, verbatim
//!
//! It deliberately differs from the notification resource's REST `gts_id`
//! (`cf.qa.insights.notification.v1~`): the first is what policies are written
//! against, the second is the RFC-9457 `type` clients match on. Nothing is
//! renamed here.
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
    // `qa.test_result` - `results`, `dashboard` and `analytics` all read the
    // two result tables under one `list`, which is that action's own doc's
    // decision; `jira`'s pre-filing read joins them under the same pair.
    // `collect` is the authenticated trigger only, `rebuild` the operator
    // replay.
    (resources::TEST_RESULT_NAME, actions::COLLECT),
    (resources::TEST_RESULT_NAME, actions::LIST),
    (resources::TEST_RESULT_NAME, actions::REBUILD),
    // `qa.saved_view` - `saved_views`. No `get`: a view is only ever listed,
    // then addressed by id for a write, so there is no single-view read to
    // authorize.
    (resources::SAVED_VIEW_NAME, actions::CREATE),
    (resources::SAVED_VIEW_NAME, actions::DELETE),
    (resources::SAVED_VIEW_NAME, actions::LIST),
    (resources::SAVED_VIEW_NAME, actions::UPDATE),
    // `qa.jira_config` - `jira`'s two settings singletons. `get` and `update`
    // only: a per-tenant singleton is neither created nor deleted nor listed.
    (resources::JIRA_CONFIG_NAME, actions::GET),
    (resources::JIRA_CONFIG_NAME, actions::UPDATE),
    // `qa.jira_bug` - `jira`'s bug registry, a different resource from the
    // connection settings above so a deployment can grant one without the
    // other. `update` is the single-bug resolve, `list` the open-bugs read,
    // `create` the find-or-file endpoint.
    (resources::JIRA_BUG_NAME, actions::CREATE),
    (resources::JIRA_BUG_NAME, actions::LIST),
    (resources::JIRA_BUG_NAME, actions::UPDATE),
    // `qa.notification_config` - `notify`. `test` is the send-test-message
    // endpoint and is separate on purpose: it sends real mail or Slack under
    // the tenant's configured credential, which is not something the grant
    // that lets somebody read the settings should carry.
    (resources::NOTIFICATION_CONFIG_NAME, actions::GET),
    (resources::NOTIFICATION_CONFIG_NAME, actions::TEST),
    (resources::NOTIFICATION_CONFIG_NAME, actions::UPDATE),
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
/// `qa.jira_bug`, and renaming them to GTS type ids is precluded because they are
/// what a deployment's policies are written against. So this list is measured
/// and pinned to [`ENFORCED`] by the scan, but no production code consumes it;
/// the consequence -- no custom role can target a QA resource type -- is
/// recorded as a follow-up in
/// `docs/superpowers/specs/2026-09-05-review-remediation-design.md` section 12.
///
/// Five, not six. `qa.jira` is not a resource type in this gear: the only
/// occurrence of that string in the tree is a `starts_with` prefix match in a
/// test fixture.
#[allow(
    dead_code,
    reason = "test-only consumer by design - see this item's doc"
)]
pub(crate) const RESOURCE_TYPES: &[&str] = &[
    resources::JIRA_BUG_NAME,
    resources::JIRA_CONFIG_NAME,
    resources::NOTIFICATION_CONFIG_NAME,
    resources::SAVED_VIEW_NAME,
    resources::TEST_RESULT_NAME,
];

/// How many `.access_scope(` call sites this crate's non-test source has.
///
/// Not a summary of [`ENFORCED`] and not derivable from it: several call sites
/// enforce the same pair (`qa-insights`'s precondition reads), and one call site can
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
const EXPECTED_ACCESS_SCOPE_SITES: usize = 10;

#[cfg(test)]
#[path = "authz_surface_tests.rs"]
mod tests;
