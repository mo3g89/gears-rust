//! qa-insights authorization permissions catalog.
//!
//! Declares every grantable permission as an [`AuthzPermissionV1`] GTS
//! instance via [`gts_instance!`]. Each invocation submits an
//! `InventoryInstance` entry; `types-registry::init()` aggregates and
//! validates them at startup — no registration code in `crate::gear`.
//!
//! `resource_type` values come from [`crate::domain::service::resources`]'s
//! `*_NAME` consts (`TEST_RESULT_NAME`, `SAVED_VIEW_NAME`, `JIRA_CONFIG_NAME`,
//! `JIRA_BUG_NAME`, `NOTIFICATION_CONFIG_NAME`) — the same strings the service
//! paths pass to `PolicyEnforcer` at enforce time, so the catalog and the
//! enforcement path share one source of truth, and `permissions_tests` pins
//! them to each other in both directions. Every `(resource_type, action)` pair
//! below is taken verbatim from
//! [`crate::domain::service::authz_surface::ENFORCED`] — the measured
//! enforcement surface, not from how the five resource names resemble one
//! another. That distinction matters here specifically: `qa.jira_config` and
//! `qa.jira_bug` look like variants of one thing and are not — see their own
//! doc comments in `domain::service::mod`'s `resources` module — and an
//! earlier attempt to attribute pairs between them by name got it wrong in six
//! places across three reviews.
//!
//! Instance id layout (the suffix needs ≥5 dot-separated tokens):
//! `gts.cf.toolkit.authz.permission.v1~cf.qa.insights.<pep_entity>_<action>.v1`,
//! where `pep_entity` is the `resource_type` string with its `qa.` prefix
//! stripped (e.g. `qa.test_result` → `test_result`). This reuses qa-insights'
//! own GTS namespace (`cf.qa.insights.*`, the same one its RFC-9457 error
//! surface uses — `cf.qa.insights.test_result.v1~`, `.saved_view.v1~`,
//! `.notification.v1~`), not `cf.core.*` — that namespace belongs to the
//! system gears, and qa-insights is not one of them. `permissions_tests`
//! asserts every hand-written id in `EXPECTED_PERMISSION_IDS` equals the id
//! this rule derives from its `(resource_type, action)` pair.
//!
//! # `notification_config`: one PEP string, two other names, three contracts
//!
//! The PEP resource string is `qa.notification_config`, which is what this
//! catalog's entity token comes from and what a deployment's policy is
//! written against. The REST surface's `gts_id`, by contrast, is
//! `gts.cf.qa.insights.notification.v1~` — the RFC-9457 `type` a client
//! matches on, i.e. a wire contract with its own callers, which is why it was
//! never renamed to track the PEP string. Every other resource in this gear
//! keeps the same word on both sides; this one alone drops `_config` on the
//! REST side. So this catalog's permission instance ids read
//! `cf.qa.insights.notification_config_<action>.v1`, following the PEP
//! string, while the unrelated error type stays
//! `cf.qa.insights.notification.v1~`. Nothing here is being renamed to make
//! the two agree — a reader who notices the mismatch should read it as this
//! paragraph, not as a bug.
//!
//! # The two collect routes are not the same permission
//!
//! `actions::COLLECT` is enforced only on the *authenticated* trigger
//! (`domain::service::collect::CollectService::trigger`, reached from
//! `api::rest::handlers::collect`'s `trigger_collect`), so that is the only
//! grantable `collect` permission below. The public HMAC callback
//! (`report_collect_count`) has no `SecurityContext` to enforce against and
//! so compiles no `AccessScope` at all — there is nothing there for a role to
//! grant or withhold. A reader comparing this catalog to the REST surface and
//! finding one collect endpoint with a permission and one without has found
//! this asymmetry, not a gap.
//!
//! Also unlike every other resource type here, `qa.test_result` carries no
//! `get`/`create`/`update`/`delete` at all — only `collect`, `list` and
//! `rebuild` — because nothing in this gear writes or reads a single result
//! row outside those three operations; `authz_surface::ENFORCED`'s own
//! comments carry the detail.
//!
//! **This catalog ships no grants.** Which role holds which permission is a
//! policy decision for the deployment's realm; what was missing was any way
//! to name these actions at all. Review finding #1.

#![allow(unknown_lints)]
#![allow(de0901_gts_string_pattern)]

use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::domain::service::{actions, resources};

// ── test_result (gts.cf.qa.insights.test_result_*.v1) — collect, list and ──
// rebuild only; see this module's header for why there is no CRUD quartet.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.test_result_collect.v1"),
        resource_type: resources::TEST_RESULT_NAME.to_owned(),
        action: actions::COLLECT.to_owned(),
        display_name: "Trigger collection of a run's test result counts".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.test_result_list.v1"),
        resource_type: resources::TEST_RESULT_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List test results and view the dashboard and analytics built from them"
            .to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.test_result_rebuild.v1"),
        resource_type: resources::TEST_RESULT_NAME.to_owned(),
        action: actions::REBUILD.to_owned(),
        display_name: "Replay a closed run window and rebuild its test result projection"
            .to_owned(),
    }
}

// ── saved_view (gts.cf.qa.insights.saved_view_*.v1) — per-user, not ────────
// tenant-wide: `AccessScope::ensure_owner` narrows every one of these to the
// caller's own views regardless of what the compiled scope grants.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.saved_view_create.v1"),
        resource_type: resources::SAVED_VIEW_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Create one's own saved analytics view".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.saved_view_delete.v1"),
        resource_type: resources::SAVED_VIEW_NAME.to_owned(),
        action: actions::DELETE.to_owned(),
        display_name: "Delete one's own saved analytics view".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.saved_view_list.v1"),
        resource_type: resources::SAVED_VIEW_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List one's own saved analytics views".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.saved_view_update.v1"),
        resource_type: resources::SAVED_VIEW_NAME.to_owned(),
        action: actions::UPDATE.to_owned(),
        display_name: "Update one's own saved analytics view".to_owned(),
    }
}

// ── jira_config (gts.cf.qa.insights.jira_config_*.v1) — the tenant-wide ────
// JIRA connection settings singleton: get/update only, distinct from
// jira_bug below.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.jira_config_get.v1"),
        resource_type: resources::JIRA_CONFIG_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read the tenant's JIRA connection settings".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.jira_config_update.v1"),
        resource_type: resources::JIRA_CONFIG_NAME.to_owned(),
        action: actions::UPDATE.to_owned(),
        display_name: "Update the tenant's JIRA connection settings (URL, credential, poll cadence)"
            .to_owned(),
    }
}

// ── jira_bug (gts.cf.qa.insights.jira_bug_*.v1) — the bug registry itself, ─
// a different resource from jira_config above so a deployment can grant one
// without the other.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.jira_bug_create.v1"),
        resource_type: resources::JIRA_BUG_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "File (or re-file) a JIRA bug for a failing test".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.jira_bug_list.v1"),
        resource_type: resources::JIRA_BUG_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List the tenant's open tracked JIRA bugs".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.jira_bug_update.v1"),
        resource_type: resources::JIRA_BUG_NAME.to_owned(),
        action: actions::UPDATE.to_owned(),
        display_name: "Resolve or otherwise update a tracked JIRA bug".to_owned(),
    }
}

// ── notification_config (gts.cf.qa.insights.notification_config_*.v1) — ────
// the PEP string, not the REST `gts_id`; see this module's header.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.notification_config_get.v1"),
        resource_type: resources::NOTIFICATION_CONFIG_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read the tenant's notification settings".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.notification_config_test.v1"),
        resource_type: resources::NOTIFICATION_CONFIG_NAME.to_owned(),
        action: actions::TEST.to_owned(),
        display_name:
            "Send a test notification probe over the tenant's configured channel".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.insights.notification_config_update.v1"),
        resource_type: resources::NOTIFICATION_CONFIG_NAME.to_owned(),
        action: actions::UPDATE.to_owned(),
        display_name: "Update the tenant's notification settings".to_owned(),
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
