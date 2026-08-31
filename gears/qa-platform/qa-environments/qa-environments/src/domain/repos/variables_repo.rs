use async_trait::async_trait;
use qa_environments_sdk::{NewVariable, Variable};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Repository trait for pipeline (global) and per-platform variable persistence.
#[async_trait]
pub trait VariablesRepository: Send + Sync {
    /// List all global pipeline variables visible within the given security scope.
    async fn list_pipeline<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Variable>, DomainError>;

    /// List all variables scoped to a single platform.
    async fn list_for_platform<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        platform_id: Uuid,
    ) -> Result<Vec<Variable>, DomainError>;

    /// Look up an existing variable by its natural key without applying
    /// action-specific authorization: `(platform_id, name)` when
    /// `platform_id` is `Some`, else `(tenant_id, name)` for pipeline
    /// variables.
    ///
    /// Used by the service layer *before* the CREATE/UPDATE PEP call in
    /// `upsert` — the caller doesn't yet know which action to authorize until
    /// it knows whether a row already exists. Callers must pass a real
    /// PEP-derived scope here (typically VARIABLE/GET) — never
    /// `AccessScope::allow_all()` — since this probe's result (existence)
    /// directly drives the next authorization decision. For the per-platform
    /// branch, implementations additionally bind the lookup to the caller's
    /// own `tenant_id` (not just the `scope`'s constraints), so the probe
    /// can never be used as a cross-tenant existence oracle.
    async fn find_by_natural_key<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        platform_id: Option<Uuid>,
        name: &str,
    ) -> Result<Option<Variable>, DomainError>;

    /// Insert or update a variable. `var.platform_id == None` targets the
    /// pipeline (global) table keyed by `(tenant_id, name)`; `Some(_)` targets
    /// the per-platform table keyed by `(platform_id, name)`.
    async fn upsert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        var: NewVariable,
    ) -> Result<Variable, DomainError>;

    /// Delete a variable by ID, trying the per-platform table then the
    /// pipeline table.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;
}
