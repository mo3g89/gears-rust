//! qa-catalog authorization permissions catalog.
//!
//! Declares every grantable permission as an [`AuthzPermissionV1`] GTS
//! instance via [`gts_instance!`]. Each invocation submits an
//! `InventoryInstance` entry; `types-registry::init()` aggregates and
//! validates them at startup — no registration code in `crate::gear`.
//!
//! `resource_type` values come from [`crate::domain::service::resources`] —
//! the same `*_NAME` consts the service paths pass to `PolicyEnforcer` at
//! enforce time, so the catalog and the enforcement path share one source of
//! truth, and `permissions_tests` pins them to each other in both
//! directions.
//!
//! Instance id layout (the suffix needs ≥5 dot-separated tokens):
//! `gts.cf.toolkit.authz.permission.v1~cf.qa.catalog.<pep_entity>_<action>.v1`,
//! where `pep_entity` is the entity token of the resource type's GTS id:
//! strip the registry's `GTS_ID_PREFIX` (`gts.`), then this gear's own
//! `cf.qa.catalog.` prefix, then the trailing `.v1~` suffix (e.g.
//! `gts.cf.qa.catalog.test_repo.v1~` → `test_repo`). This reuses qa-catalog's own
//! GTS namespace (`cf.qa.catalog.*`, the same one its RFC-9457 error surface
//! uses), not `cf.core.*` — that namespace belongs to the system gears, and
//! qa-catalog is not one of them. `permissions_tests` asserts every hand-written
//! id in `EXPECTED_PERMISSION_IDS` equals the id this rule derives from its
//! `(resource_type, action)` pair.
//!
//! **This catalog ships no grants.** Which role holds which permission is a
//! policy decision for the deployment's realm; what was missing was any way
//! to name these actions at all. Review finding #1.

#![allow(unknown_lints)]
#![allow(de0901_gts_string_pattern)]

use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::domain::service::{actions, resources};

// ── test_repo (gts.cf.qa.catalog.test_repo_*.v1) ────────────────────────────

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.test_repo_create.v1"),
        resource_type: resources::TEST_REPO_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Register a test repository".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.test_repo_delete.v1"),
        resource_type: resources::TEST_REPO_NAME.to_owned(),
        action: actions::DELETE.to_owned(),
        display_name: "Delete a test repository".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.test_repo_get.v1"),
        resource_type: resources::TEST_REPO_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read a test repository".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.test_repo_list.v1"),
        resource_type: resources::TEST_REPO_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List test repositories".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.test_repo_sync.v1"),
        resource_type: resources::TEST_REPO_NAME.to_owned(),
        action: actions::SYNC.to_owned(),
        display_name: "Sync a test repository's working copy from its remote".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.test_repo_update.v1"),
        resource_type: resources::TEST_REPO_NAME.to_owned(),
        action: actions::UPDATE.to_owned(),
        display_name: "Update a test repository's registration settings".to_owned(),
    }
}

// ── plan (gts.cf.qa.catalog.plan_*.v1) — discovery is read-only ────────────

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.plan_get.v1"),
        resource_type: resources::PLAN_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read a discovered plan (incl. its TEST_META)".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.plan_list.v1"),
        resource_type: resources::PLAN_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List plans discovered from a synced repository".to_owned(),
    }
}

// ── custom_plan (gts.cf.qa.catalog.custom_plan_*.v1) — full CRUD ───────────

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.custom_plan_create.v1"),
        resource_type: resources::CUSTOM_PLAN_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Create a custom (user-composed) plan".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.custom_plan_delete.v1"),
        resource_type: resources::CUSTOM_PLAN_NAME.to_owned(),
        action: actions::DELETE.to_owned(),
        display_name: "Delete a custom plan".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.custom_plan_get.v1"),
        resource_type: resources::CUSTOM_PLAN_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read a custom plan".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.custom_plan_list.v1"),
        resource_type: resources::CUSTOM_PLAN_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List custom plans".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.custom_plan_update.v1"),
        resource_type: resources::CUSTOM_PLAN_NAME.to_owned(),
        action: actions::UPDATE.to_owned(),
        display_name: "Update a custom plan".to_owned(),
    }
}

// ── product (gts.cf.qa.catalog.product_*.v1) — also covers the plugin ──────
// registry's two reads (`plugin_for` under `get`, `list_registered_plugins`
// under `list`); they are not a separate resource type.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.product_create.v1"),
        resource_type: resources::PRODUCT_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Create a product".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.product_delete.v1"),
        resource_type: resources::PRODUCT_NAME.to_owned(),
        action: actions::DELETE.to_owned(),
        display_name: "Delete a product".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.product_get.v1"),
        resource_type: resources::PRODUCT_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Resolve the plugin bound to a product".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.product_list.v1"),
        resource_type: resources::PRODUCT_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List products and registered product plugins".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.product_update.v1"),
        resource_type: resources::PRODUCT_NAME.to_owned(),
        action: actions::UPDATE.to_owned(),
        display_name: "Update a product".to_owned(),
    }
}

// ── ssh_key (gts.cf.qa.catalog.ssh_key_*.v1) — metadata only, no update: ────
// key material lives in credstore, so rotation is delete-then-create.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.ssh_key_create.v1"),
        resource_type: resources::SSH_KEY_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Register an SSH key for repository sync credentials".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.ssh_key_delete.v1"),
        resource_type: resources::SSH_KEY_NAME.to_owned(),
        action: actions::DELETE.to_owned(),
        display_name: "Delete an SSH key".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.ssh_key_get.v1"),
        resource_type: resources::SSH_KEY_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Resolve an SSH key's credstore reference".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.ssh_key_list.v1"),
        resource_type: resources::SSH_KEY_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List registered SSH keys".to_owned(),
    }
}

// ── bundle (gts.cf.qa.catalog.bundle_*.v1) — ephemeral: no `list`, and ─────
// `delete` is also what the expiry GC purge enforces per tenant.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.bundle_create.v1"),
        resource_type: resources::BUNDLE_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Build a test bundle from a synced repository".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.bundle_delete.v1"),
        resource_type: resources::BUNDLE_NAME.to_owned(),
        action: actions::DELETE.to_owned(),
        display_name: "Delete (or garbage-collect) an expired test bundle".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.catalog.bundle_get.v1"),
        resource_type: resources::BUNDLE_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Download a test bundle's content".to_owned(),
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
