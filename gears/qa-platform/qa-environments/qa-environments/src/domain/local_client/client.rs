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
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::gear::ConcreteAppServices;

/// Local implementation of the object-safe `QaEnvironmentsClientV1`.
pub struct QaEnvironmentsLocalClient {
    services: Arc<ConcreteAppServices>,
}

impl QaEnvironmentsLocalClient {
    #[must_use]
    pub(crate) fn new(services: Arc<ConcreteAppServices>) -> Self {
        Self { services }
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

    async fn list_environments(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Environment>, QaEnvironmentsError> {
        self.services
            .environments
            .list_environments(ctx)
            .await
            .map_err(Into::into)
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

    async fn list_variables(
        &self,
        ctx: &SecurityContext,
        environment_id: Option<Uuid>,
    ) -> Result<Vec<Variable>, QaEnvironmentsError> {
        self.services
            .variables
            .list_for_env(ctx, environment_id)
            .await
            .map_err(Into::into)
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
