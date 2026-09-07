//! qa-environments authorization permissions catalog.
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
//! `gts.cf.toolkit.authz.permission.v1~cf.qa.environments.<pep_entity>_<action>.v1`,
//! where `pep_entity` is the `resource_type` string with its `qa.` prefix
//! stripped (e.g. `qa.platform` → `platform`). This reuses qa-environments'
//! own GTS namespace (`cf.qa.environments.*`, the same one its RFC-9457 error
//! surface uses — `cf.qa.environments.platform.v1~`,
//! `cf.qa.environments.variable.v1~`, `cf.qa.environments.lease.v1~`), not
//! `cf.core.*` — that namespace belongs to the system gears, and
//! qa-environments is not one of them. `permissions_tests` asserts every
//! hand-written id in `EXPECTED_PERMISSION_IDS` equals the id this rule
//! derives from its `(resource_type, action)` pair.
//!
//! # `qa.platform`, not `qa.environment`
//!
//! The aggregate this gear manages is called `Environment` in Rust, on the
//! REST wire, in the routes and in the UI. The PDP resource string and the
//! GTS type ids both kept `platform` — `domain::service::resources::PLATFORM`'s
//! own doc records why: the string is what deployment policies are written
//! against, and changing it would silently change who is authorized for what.
//! That doc is **named rather than line-cited**. As a `:N` range the citation
//! was wrong twice: it was copied stale from the spec, and the correction then
//! drifted three lines the same day. `file_citations_tests.rs` validates file
//! existence only - never a line number - so nothing here would have caught
//! either.
//!
//! This catalog follows the PDP string, because that is what a role grant
//! actually matches against — a catalog generated from the aggregate's Rust
//! name would emit `qa.environment`, match no policy, grant nothing, and look
//! entirely correct while doing it.
//! `permissions_tests::the_catalog_names_qa_platform_not_qa_environment` pins
//! both halves.
//!
//! Display names below say "environment" regardless of that string, because
//! that is the word a human meets everywhere except this one wire value — an
//! operator building a role reads the display name, not the PDP string, so it
//! should use the word the operator actually knows. Don't "fix" a display
//! name later to say "platform".
//!
//! **This catalog ships no grants.** Which role holds which permission is a
//! policy decision for the deployment's realm; what was missing was any way
//! to name these actions at all. Review finding #1.

#![allow(unknown_lints)]
#![allow(de0901_gts_string_pattern)]

use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::domain::service::{actions, resources};

// ── platform (gts.cf.qa.environments.platform_*.v1) — full CRUD ────────────

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.platform_create.v1"),
        resource_type: resources::PLATFORM_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Register an environment".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.platform_delete.v1"),
        resource_type: resources::PLATFORM_NAME.to_owned(),
        action: actions::DELETE.to_owned(),
        display_name: "Delete an environment".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.platform_get.v1"),
        resource_type: resources::PLATFORM_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read an environment".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.platform_list.v1"),
        resource_type: resources::PLATFORM_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List environments".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.platform_update.v1"),
        resource_type: resources::PLATFORM_NAME.to_owned(),
        action: actions::UPDATE.to_owned(),
        display_name: "Update an environment's registration or observed state".to_owned(),
    }
}

// ── variable (gts.cf.qa.environments.variable_*.v1) — full CRUD ────────────

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.variable_create.v1"),
        resource_type: resources::VARIABLE_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Create a pipeline or environment variable".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.variable_delete.v1"),
        resource_type: resources::VARIABLE_NAME.to_owned(),
        action: actions::DELETE.to_owned(),
        display_name: "Delete a pipeline or environment variable".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.variable_get.v1"),
        resource_type: resources::VARIABLE_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read a pipeline or environment variable".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.variable_list.v1"),
        resource_type: resources::VARIABLE_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List pipeline or environment variables".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.variable_update.v1"),
        resource_type: resources::VARIABLE_NAME.to_owned(),
        action: actions::UPDATE.to_owned(),
        display_name: "Update a pipeline or environment variable".to_owned(),
    }
}

// ── lease (gts.cf.qa.environments.lease_*.v1) — acquire/get/release: a lease
// row is compare-and-set into existence by `acquire`, not created by a
// caller, so there is no `create`, `list` or `delete` to grant (an
// environment is leased or it is not; the lease is addressed through its
// environment). A forced run that bypasses the lease is authorized under its
// own separate qa-runs action instead, never under one of these.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.lease_acquire.v1"),
        resource_type: resources::LEASE_NAME.to_owned(),
        action: actions::ACQUIRE.to_owned(),
        display_name: "Acquire an exclusive lease on an environment".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.lease_get.v1"),
        resource_type: resources::LEASE_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read an environment's current lease".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.environments.lease_release.v1"),
        resource_type: resources::LEASE_NAME.to_owned(),
        action: actions::RELEASE.to_owned(),
        display_name: "Release an environment's lease".to_owned(),
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
