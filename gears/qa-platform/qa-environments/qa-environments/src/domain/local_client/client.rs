//! Local client adapter: implements the object-safe `QaEnvironmentsClientV1`
//! by delegating to `AppServices`, converting `DomainError` into
//! `QaEnvironmentsError` (`CanonicalError`) via the `From` impl in
//! `api::rest::error`.

use std::sync::Arc;

use async_trait::async_trait;
use qa_environments_sdk::{
    AcquireOutcome, LeaseMode, LeaseState, NewPlatform, NewVariable, PlatformPatch,
    QaEnvironmentsClientV1, QaEnvironmentsError, TargetPlatform, Variable,
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
    // ==================== Platforms ====================

    async fn get_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<TargetPlatform, QaEnvironmentsError> {
        self.services
            .platforms
            .get_platform(ctx, id)
            .await
            .map_err(Into::into)
    }

    async fn list_platforms(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<TargetPlatform>, QaEnvironmentsError> {
        self.services
            .platforms
            .list_platforms(ctx)
            .await
            .map_err(Into::into)
    }

    async fn create_platform(
        &self,
        ctx: &SecurityContext,
        new: NewPlatform,
    ) -> Result<TargetPlatform, QaEnvironmentsError> {
        self.services
            .platforms
            .create_platform(ctx, new)
            .await
            .map_err(Into::into)
    }

    async fn update_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: PlatformPatch,
    ) -> Result<TargetPlatform, QaEnvironmentsError> {
        self.services
            .platforms
            .update_platform(ctx, id, patch)
            .await
            .map_err(Into::into)
    }

    async fn delete_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), QaEnvironmentsError> {
        self.services
            .platforms
            .delete_platform(ctx, id)
            .await
            .map_err(Into::into)
    }

    // ==================== Variables ====================

    async fn list_variables(
        &self,
        ctx: &SecurityContext,
        platform_id: Option<Uuid>,
    ) -> Result<Vec<Variable>, QaEnvironmentsError> {
        self.services
            .variables
            .list_for_env(ctx, platform_id)
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
        platform_id: Uuid,
        run_id: Uuid,
        mode: LeaseMode,
    ) -> Result<AcquireOutcome, QaEnvironmentsError> {
        self.services
            .leases
            .acquire(ctx, platform_id, run_id, mode)
            .await
            .map_err(Into::into)
    }

    async fn release_lease(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        run_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError> {
        self.services
            .leases
            .release(ctx, platform_id, run_id)
            .await
            .map_err(Into::into)
    }

    async fn get_lease(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError> {
        self.services
            .leases
            .get(ctx, platform_id)
            .await
            .map_err(Into::into)
    }
}
