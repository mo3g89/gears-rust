use async_trait::async_trait;
use qa_environments_sdk::{NewVariable, Variable};
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Repository trait for pipeline (global) and per-environment variable persistence.
#[async_trait]
pub trait VariablesRepository: Send + Sync {
    /// One page of the global pipeline variables visible within `scope`,
    /// ordered by name, with the caller's `OData` query applied on top.
    ///
    /// Both this and [`Self::list_for_environment_page`] replaced unbounded
    /// `find().secure().scope_with(scope).all()` reads (review finding #55).
    /// The `scope` is applied before the filter and cannot be widened by one —
    /// see [`EnvironmentsRepository::list_page`](super::EnvironmentsRepository::list_page)
    /// for the type-level reason.
    async fn list_pipeline_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<Variable>, DomainError>;

    /// One page of a single environment's variables, ordered by name.
    ///
    /// `environment_id` is pinned by the repository rather than expressible as
    /// a `$filter`: the physical column is `platform_id`, and the endpoint's
    /// own `environment_id` parameter carries an authorization precheck a
    /// `$filter` would skip. See
    /// [`VariableFilterField`](crate::infra::storage::odata::VariableFilterField)'s
    /// doc for the full argument.
    async fn list_for_environment_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        environment_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<Variable>, DomainError>;

    /// Look up an existing variable by its natural key without applying
    /// action-specific authorization: `(environment_id, name)` when
    /// `environment_id` is `Some`, else `(tenant_id, name)` for pipeline
    /// variables.
    ///
    /// Used by the service layer *before* the CREATE/UPDATE PEP call in
    /// `upsert` — the caller doesn't yet know which action to authorize until
    /// it knows whether a row already exists. Callers must pass a real
    /// PEP-derived scope here (typically VARIABLE/GET) — never
    /// `AccessScope::allow_all()` — since this probe's result (existence)
    /// directly drives the next authorization decision. For the per-environment
    /// branch, implementations additionally bind the lookup to the caller's
    /// own `tenant_id` (not just the `scope`'s constraints), so the probe
    /// can never be used as a cross-tenant existence oracle.
    async fn find_by_natural_key<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        environment_id: Option<Uuid>,
        name: &str,
    ) -> Result<Option<Variable>, DomainError>;

    /// Insert or update a variable. `var.environment_id == None` targets the
    /// pipeline (global) table keyed by `(tenant_id, name)`; `Some(_)` targets
    /// the per-environment table keyed by `(environment_id, name)`.
    async fn upsert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        var: NewVariable,
    ) -> Result<Variable, DomainError>;

    /// Delete a variable by ID, trying the per-environment table then the
    /// pipeline table.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;
}
