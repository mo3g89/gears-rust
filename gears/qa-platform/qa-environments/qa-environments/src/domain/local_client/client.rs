//! Local client adapter: implements the object-safe `QaEnvironmentsClientV1`
//! by delegating to `AppServices`, converting `DomainError` into
//! `QaEnvironmentsError` (`CanonicalError`) via the `From` impl in
//! `api::rest::error`.

use std::sync::Arc;

use async_trait::async_trait;
use qa_environments_sdk::{
    AcquireOutcome, Environment, EnvironmentPatch, LeaseMode, LeaseState, NewEnvironment,
    NewVariable, QaEnvironmentsClientV1, QaEnvironmentsError, Variable,
};
use toolkit_odata::{CursorV1, ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::gear::ConcreteAppServices;

/// Local implementation of the object-safe `QaEnvironmentsClientV1`.
pub struct QaEnvironmentsLocalClient {
    services: Arc<ConcreteAppServices>,
}

impl QaEnvironmentsLocalClient {
    #[must_use]
    pub fn new(services: Arc<ConcreteAppServices>) -> Self {
        Self { services }
    }
}

/// Follow a paginated service read to exhaustion, concatenating the pages.
///
/// The SDK's two collection reads are `Vec`-shaped and the services behind them
/// are `Page`-shaped, so something has to bridge the two; this is it, once,
/// rather than the same loop written twice. Each round trip is bounded by
/// `PAGE_LIMITS`; the aggregate is not, which is the SDK contract those two
/// methods' docs defend.
///
/// Terminates when a page reports no `next_cursor`, or -- defensively -- when a
/// cursor comes back that does not decode. The latter cannot happen against
/// this gear's own pager (it emits what `CursorV1::encode` produced), and
/// stopping is the right answer if it ever does: a partial list is what the
/// caller already handles for a page, whereas looping on an undecodable cursor
/// would not terminate.
async fn drain_pages<T, F, Fut>(mut read: F) -> Result<Vec<T>, QaEnvironmentsError>
where
    F: FnMut(ODataQuery) -> Fut,
    Fut: std::future::Future<Output = Result<Page<T>, DomainError>>,
{
    let mut query = ODataQuery::default();
    let mut all = Vec::new();
    loop {
        let page = read(query).await?;
        all.extend(page.items);
        let Some(token) = page.page_info.next_cursor else {
            return Ok(all);
        };
        let Ok(cursor) = CursorV1::decode(&token) else {
            return Ok(all);
        };
        query = ODataQuery::default().with_cursor(cursor);
    }
}

#[async_trait]
impl QaEnvironmentsClientV1 for QaEnvironmentsLocalClient {
    // ==================== Environments ====================

    async fn get_environment(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Environment, QaEnvironmentsError> {
        self.services
            .environments
            .get_environment(ctx, id)
            .await
            .map_err(Into::into)
    }

    /// **Every** environment the caller may see, drained page by page.
    ///
    /// The service read became paginated with review finding #55, and the SDK
    /// contract did not: `qa-insights`' `EnvironmentReader`
    /// (`infra/clients/qa_environments.rs`) resolves environment *names* for
    /// chart labels by set difference against this list, and its own header
    /// spells out that *"a cap that silently dropped an environment would
    /// silently unlabel a bar"*. Truncating here to whatever
    /// `PAGE_LIMITS.default` happens to be would do exactly that, invisibly.
    ///
    /// So the loop, rather than one call: each round trip is bounded (which is
    /// what the finding was about — an unbounded `SELECT` materialising the
    /// whole table into one `Vec`), and the SDK contract is unchanged. The
    /// aggregate result is still one `Vec` of the tenant's environments, which
    /// is a read of tens of rows for the reason that file argues at length.
    async fn list_environments(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Environment>, QaEnvironmentsError> {
        drain_pages(|query| async move {
            self.services
                .environments
                .list_environments(ctx, &query)
                .await
        })
        .await
    }

    async fn create_environment(
        &self,
        ctx: &SecurityContext,
        new: NewEnvironment,
    ) -> Result<Environment, QaEnvironmentsError> {
        self.services
            .environments
            .create_environment(ctx, new)
            .await
            .map_err(Into::into)
    }

    async fn update_environment(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: EnvironmentPatch,
    ) -> Result<Environment, QaEnvironmentsError> {
        self.services
            .environments
            .update_environment(ctx, id, patch)
            .await
            .map_err(Into::into)
    }

    async fn delete_environment(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), QaEnvironmentsError> {
        self.services
            .environments
            .delete_environment(ctx, id)
            .await
            .map_err(Into::into)
    }

    // ==================== Variables ====================

    /// Every variable that would assemble into a run's environment, drained the
    /// same way [`Self::list_environments`] is and for the same reason:
    /// `qa-runs`' `runvars` splits this response into its two precedence tiers,
    /// and a variable missing from it is a variable the run silently does not
    /// get.
    ///
    /// **The drain terminates after one page whenever `environment_id` is
    /// `Some`**, because `VariablesService::list_for_env` returns no cursor in
    /// that case — the response is the union of two tables and a single-table
    /// cursor cannot address it. See that method's doc. The bound that then
    /// applies is `PAGE_LIMITS.default` and the deployment's own
    /// `max_variables`, which is the bound the previous code applied too (as a
    /// silent `truncate`), so this is not a new ceiling.
    async fn list_variables(
        &self,
        ctx: &SecurityContext,
        environment_id: Option<Uuid>,
    ) -> Result<Vec<Variable>, QaEnvironmentsError> {
        drain_pages(|query| async move {
            self.services
                .variables
                .list_for_env(ctx, environment_id, &query)
                .await
        })
        .await
    }

    async fn upsert_variable(
        &self,
        ctx: &SecurityContext,
        var: NewVariable,
    ) -> Result<Variable, QaEnvironmentsError> {
        self.services
            .variables
            .upsert(ctx, var)
            .await
            .map_err(Into::into)
    }

    async fn delete_variable(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), QaEnvironmentsError> {
        self.services
            .variables
            .delete(ctx, id)
            .await
            .map_err(Into::into)
    }

    // ==================== Leases ====================

    async fn acquire_lease(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
        run_id: Uuid,
        mode: LeaseMode,
    ) -> Result<AcquireOutcome, QaEnvironmentsError> {
        self.services
            .leases
            .acquire(ctx, environment_id, run_id, mode)
            .await
            .map_err(Into::into)
    }

    async fn release_lease(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
        run_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError> {
        self.services
            .leases
            .release(ctx, environment_id, run_id)
            .await
            .map_err(Into::into)
    }

    async fn get_lease(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError> {
        self.services
            .leases
            .get(ctx, environment_id)
            .await
            .map_err(Into::into)
    }
}
