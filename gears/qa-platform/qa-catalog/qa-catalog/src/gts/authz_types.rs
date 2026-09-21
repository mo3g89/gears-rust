//! Stub GTS type-schemas for qa-catalog's authorization labels.
//!
//! The RBAC role-definition validator resolves a rule's `target_type` through
//! the types-registry, so a resource type the registry cannot resolve is a
//! permission no role definition can name. Declaring each label here puts it
//! into the `inventory` collection `types-registry::init()` registers at boot.
//!
//! Each body is `id`-only, following `account-management-sdk`'s `TenantV1`:
//! authorization needs the type *id* known to the registry and nothing from
//! the schema body. The tenant-scope binding lives on the impl side, in
//! `domain::service::resources`' `ResourceType` (`OWNER_TENANT_ID`).

use toolkit_gts::gts_type_schema;

/// `qa_test_repos` — RBAC/PEP target type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.catalog.test_repo.v1~"),
    description = "QA test repository — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaTestRepoV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// Discovered `plan.yaml` files — RBAC/PEP target type. Read-only by
/// construction, for the reason `resources::PLAN`'s own doc gives.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.catalog.plan.v1~"),
    description = "QA test plan — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaPlanV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// `qa_custom_plans` — RBAC/PEP target type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.catalog.custom_plan.v1~"),
    description = "QA custom plan — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaCustomPlanV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// `qa_products` — RBAC/PEP target type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.catalog.product.v1~"),
    description = "QA product — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaProductV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// `qa_ssh_keys` — RBAC/PEP target type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.catalog.ssh_key.v1~"),
    description = "QA SSH key — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaSshKeyV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// Test bundles — RBAC/PEP target type. This id is minted with this module:
/// bundles carry no RFC-9457 error surface, so unlike its five neighbours it
/// had no prior publication.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.catalog.bundle.v1~"),
    description = "QA test bundle — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaBundleV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}
