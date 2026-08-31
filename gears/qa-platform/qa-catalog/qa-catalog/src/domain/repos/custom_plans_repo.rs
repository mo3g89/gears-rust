use async_trait::async_trait;
use qa_catalog_sdk::{CustomPlan, NewCustomPlan};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Repository trait for `CustomPlan` persistence. `files` and `tags` live in
/// JSON columns; the mapper owns the round-trip and fails closed on corrupt
/// content (see `infra::storage::mapper`).
#[async_trait]
pub trait CustomPlansRepository: Send + Sync {
    /// Find a custom plan by ID within the given security scope.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<CustomPlan>, DomainError>;

    /// List all custom plans visible within the given security scope.
    async fn list<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<CustomPlan>, DomainError>;

    /// Create a new custom plan.
    async fn create<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        new: NewCustomPlan,
    ) -> Result<CustomPlan, DomainError>;

    /// Replace the mutable fields of an existing custom plan (full-document
    /// update, matching `QaCatalogClientV1::update_custom_plan`). Returns
    /// `Ok(None)` when no row with `id` is visible in `scope`.
    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        new: NewCustomPlan,
    ) -> Result<Option<CustomPlan>, DomainError>;

    /// Delete a custom plan by ID.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;
}
