//! Stub GTS type-schemas for qa-insights' authorization labels.
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

/// `qa_test_results` — RBAC/PEP target type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.insights.test_result.v1~"),
    description = "QA test result — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaTestResultV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// `qa_saved_views` — RBAC/PEP target type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.insights.saved_view.v1~"),
    description = "QA saved analytics view — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaSavedViewV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// The tenant's JIRA integration config — RBAC/PEP target type. This id is
/// minted with this module: the config carries no RFC-9457 error surface, so
/// unlike its neighbours it had no prior publication.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.insights.jira_config.v1~"),
    description = "QA JIRA integration config — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaJiraConfigV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// `qa_jira_bugs` — RBAC/PEP target type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.insights.jira_bug.v1~"),
    description = "QA JIRA bug link — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaJiraBugV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}

/// The tenant's notification config — RBAC/PEP target type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.qa.insights.notification_config.v1~"),
    description = "QA notification config — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct QaNotificationConfigV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization.
    pub id: gts::GtsInstanceId,
}
