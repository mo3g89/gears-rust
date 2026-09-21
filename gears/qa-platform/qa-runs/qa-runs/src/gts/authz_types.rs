//! Stub GTS type-schemas for qa-runs' authorization labels.
//!
//! The RBAC role-definition validator resolves a rule's `target_type` through
//! the types-registry, so a resource type the registry cannot resolve is a
//! permission no role definition can name. Declaring each label here puts it
//! into the `inventory` collection `types-registry::init()` registers at boot
//! — the same route `crate::gts::permissions` takes, and the reason neither
//! needs registration code in `crate::gear`.
//!
//! Each body is `id`-only, following `account-management-sdk`'s `TenantV1`:
//! authorization needs the type *id* known to the registry and nothing from
//! the schema body. The tenant-scope binding lives on the impl side, in
//! `domain::service::resources`' `ResourceType` (`OWNER_TENANT_ID`).
//!
//! The ids are the ones this gear already publishes on its RFC-9457 error
//! surface. They are not new names; they are the same names, now registered.

use toolkit_gts::gts_type_schema;

/// `qa_runs` — RBAC/PEP target type for
/// [`crate::domain::service::resources::RUN`].
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.runs.run.v1~"),
    description = "QA run — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaRunV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// `qa_run_queue` — RBAC/PEP target type for
/// [`crate::domain::service::resources::QUEUE_ENTRY`].
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.runs.queue_entry.v1~"),
    description = "QA run queue entry — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaQueueEntryV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// `qa_schedules` and `qa_schedule_ticks` — RBAC/PEP target type for
/// [`crate::domain::service::resources::SCHEDULE`]. One type over two tables,
/// for the reason that constant's own doc gives.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.runs.schedule.v1~"),
    description = "QA schedule — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaScheduleV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}
