//! qa-runs authorization permissions catalog.
//!
//! Declares every grantable permission as an [`AuthzPermissionV1`] GTS
//! instance via [`gts_instance!`]. Each invocation submits an
//! `InventoryInstance` entry; `types-registry::init()` aggregates and
//! validates them at startup — no registration code in `crate::gear`.
//!
//! `resource_type` values come from [`crate::domain::service::resources`] —
//! the same `*_NAME` consts (`RUN_NAME`, `QUEUE_ENTRY_NAME`, `SCHEDULE_NAME`)
//! the service paths pass to `PolicyEnforcer` at enforce time, so the catalog
//! and the enforcement path share one source of truth, and
//! `permissions_tests` pins them to each other in both directions. Every
//! `(resource_type, action)` pair below is taken verbatim from
//! [`crate::domain::service::authz_surface::ENFORCED`] — the measured
//! enforcement surface (18 pairs over the 3 resource types this gear
//! declares), not from a reader's memory of the service code.
//!
//! Instance id layout (the suffix needs ≥5 dot-separated tokens):
//! `gts.cf.toolkit.authz.permission.v1~cf.qa.runs.<pep_entity>_<action>.v1`,
//! where `pep_entity` is the `resource_type` string with its `qa.` prefix
//! stripped (e.g. `qa.queue_entry` → `queue_entry`). This reuses qa-runs' own
//! GTS namespace (`cf.qa.runs.*`, the same one its RFC-9457 error surface
//! uses — `cf.qa.runs.run.v1~`, `.queue_entry.v1~`, `.schedule.v1~`), not
//! `cf.core.*` — that namespace belongs to the system gears, and qa-runs is
//! not one of them. `permissions_tests` asserts every hand-written id in
//! `EXPECTED_PERMISSION_IDS` equals the id this rule derives from its
//! `(resource_type, action)` pair.
//!
//! # `cancel` is one action name on two resource types, and they are two
//! # different permissions
//!
//! An operator cancel writes both `qa_runs.state` and, for a run that has not
//! started, `qa_run_queue.state` — see `actions::CANCEL`'s own doc comment.
//! That is one request, but it is authorized as two separate grants below:
//! `qa.run`/`cancel` (stopping the run itself) and `qa.queue_entry`/`cancel`
//! (dropping its still-queued row). A deployment can therefore grant one
//! without the other, and the two `display_name`s say which side each one is.
//!
//! # Three actions exist only to gate an operator override separately
//!
//! `force_start`, `rerun` and `fire` are not folded into `dispatch` or
//! `create` even though each ultimately drives the same write path, because
//! each is a distinct authority a deployment must be able to grant or
//! withhold on its own:
//!
//! * `qa.queue_entry`/`force_start` lets an operator start a queued entry
//!   **now**, bypassing the platform's occupancy check — including an
//!   in-flight exclusive run's lease — while `max_concurrent_runs` stays
//!   enforced (`domain/service/runs.rs`'s `mark_dispatching`, around line
//!   1437). A forced start holds no lease of its own, so the review's
//!   `[by-design]` entry on the lease override is defensible only because
//!   this is its own PEP action: if starting a run could not be gated
//!   separately from granting `force_start`, granting anyone the ability to
//!   launch a run would silently grant them the ability to bypass another
//!   tenant's exclusive lease too. The `display_name` below says what it
//!   overrides, not just what it does.
//! * `qa.run`/`rerun` lets an operator relaunch a *stored* run — including
//!   one they did not create — without also granting `create`, `list` and
//!   `get` outright; `LaunchService::launch` derives those on its own once
//!   `rerun` is granted.
//! * `qa.schedule`/`fire` lets the background firing ticker trigger a
//!   schedule's due run out of band, so a deployment can grant the sweep
//!   without granting schedule edits (`update`/`delete`).
//!
//! **This catalog ships no grants.** Which role holds which permission is a
//! policy decision for the deployment's realm; what was missing was any way
//! to name these actions at all. Review finding #1.

#![allow(unknown_lints)]
#![allow(de0901_gts_string_pattern)]

use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::domain::service::{actions, resources};

// ── run (gts.cf.qa.runs.run_*.v1) — no update or delete: a run is never ────
// edited or removed, it is cancelled and re-run.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.run_cancel.v1"),
        resource_type: resources::RUN_NAME.to_owned(),
        action: actions::CANCEL.to_owned(),
        display_name: "Cancel a run".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.run_create.v1"),
        resource_type: resources::RUN_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Launch a run, including recording its admission outcome".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.run_dispatch.v1"),
        resource_type: resources::RUN_NAME.to_owned(),
        action: actions::DISPATCH.to_owned(),
        display_name:
            "Move a run from queued to executing, and every write the dispatcher tick \
             makes to it"
                .to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.run_get.v1"),
        resource_type: resources::RUN_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read a run".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.run_list.v1"),
        resource_type: resources::RUN_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List runs".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.run_rerun.v1"),
        resource_type: resources::RUN_NAME.to_owned(),
        action: actions::RERUN.to_owned(),
        display_name:
            "Re-run a stored run: launch a new run from it, regardless of who created \
             the original"
                .to_owned(),
    }
}

// ── queue_entry (gts.cf.qa.runs.queue_entry_*.v1) — no `rerun`, which is a ─
// run's verb and not an entry's.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.queue_entry_cancel.v1"),
        resource_type: resources::QUEUE_ENTRY_NAME.to_owned(),
        action: actions::CANCEL.to_owned(),
        display_name: "Cancel a run's still-queued entry, dropping it from the queue".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.queue_entry_create.v1"),
        resource_type: resources::QUEUE_ENTRY_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Enqueue a new run's queue entry during admission".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.queue_entry_dispatch.v1"),
        resource_type: resources::QUEUE_ENTRY_NAME.to_owned(),
        action: actions::DISPATCH.to_owned(),
        display_name:
            "Move a queued entry to the executor, and every write the dispatcher tick \
             makes to it"
                .to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.queue_entry_force_start.v1"),
        resource_type: resources::QUEUE_ENTRY_NAME.to_owned(),
        action: actions::FORCE_START.to_owned(),
        display_name:
            "Force-start a queued entry now, overriding platform exclusivity -- including \
             bypassing an in-flight exclusive run's lease -- while max_concurrent_runs \
             still applies"
                .to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.queue_entry_get.v1"),
        resource_type: resources::QUEUE_ENTRY_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read a queue entry".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.queue_entry_list.v1"),
        resource_type: resources::QUEUE_ENTRY_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List queue entries".to_owned(),
    }
}

// ── schedule (gts.cf.qa.runs.schedule_*.v1) — full CRUD plus the firing ────
// ticker's own `fire`, over `qa_schedules` and `qa_schedule_ticks`.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.schedule_create.v1"),
        resource_type: resources::SCHEDULE_NAME.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Create a schedule".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.schedule_delete.v1"),
        resource_type: resources::SCHEDULE_NAME.to_owned(),
        action: actions::DELETE.to_owned(),
        display_name:
            "Delete a schedule, and with it every recorded tick that fired it".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.schedule_fire.v1"),
        resource_type: resources::SCHEDULE_NAME.to_owned(),
        action: actions::FIRE.to_owned(),
        display_name:
            "Trigger a scheduled run when its due time arrives -- the background firing \
             ticker's own action, separate from editing the schedule"
                .to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.schedule_get.v1"),
        resource_type: resources::SCHEDULE_NAME.to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read a schedule".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.schedule_list.v1"),
        resource_type: resources::SCHEDULE_NAME.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List schedules".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.qa.runs.schedule_update.v1"),
        resource_type: resources::SCHEDULE_NAME.to_owned(),
        action: actions::UPDATE.to_owned(),
        display_name: "Update a schedule's caller-decidable fields".to_owned(),
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
