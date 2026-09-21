//! Stub GTS type-schemas for qa-environments' authorization labels.
//!
//! The RBAC role-definition validator resolves a rule's `target_type` through
//! the types-registry, so a resource type the registry cannot resolve is a
//! permission no role definition can name. Declaring each label here puts it
//! into the `inventory` collection `types-registry::init()` registers at boot.
//!
//! Each body is `id`-only, following `account-management-sdk`'s `TenantV1`:
//! authorization needs the type *id* known to the registry and nothing from
//! the schema body.

use toolkit_gts::gts_type_schema;

/// `qa_environments` — RBAC/PEP target type. The entity token is `platform`
/// for the reason `resources::PLATFORM_NAME`'s doc gives.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.environments.platform.v1~"),
    description = "QA target environment — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaPlatformV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// `qa_environment_variables` — RBAC/PEP target type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.environments.variable.v1~"),
    description = "QA environment variable — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaVariableV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// `qa_environment_leases` — RBAC/PEP target type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.environments.lease.v1~"),
    description = "QA environment lease — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaLeaseV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}
