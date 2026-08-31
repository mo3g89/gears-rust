//! Object-safe client trait for inter-gear consumption via `ClientHub`.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::errors::QaEnvironmentsError;
use crate::models::{
    AcquireOutcome, LeaseMode, LeaseState, NewPlatform, NewVariable, PlatformPatch, TargetPlatform,
    Variable,
};

/// Object-safe client for the qa-environments gear (Version 1).
///
/// Registered in `ClientHub`:
/// ```ignore
/// let envs = hub.get::<dyn QaEnvironmentsClientV1>()?;
/// ```
///
/// Primary consumer: the qa-runs dispatcher (lease operations, variable reads,
/// kubeconfig reference for `RunSpec` assembly).
#[async_trait]
pub trait QaEnvironmentsClientV1: Send + Sync {
    // ==================== Platforms ====================

    async fn get_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<TargetPlatform, QaEnvironmentsError>;

    async fn list_platforms(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<TargetPlatform>, QaEnvironmentsError>;

    async fn create_platform(
        &self,
        ctx: &SecurityContext,
        new: NewPlatform,
    ) -> Result<TargetPlatform, QaEnvironmentsError>;

    async fn update_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: PlatformPatch,
    ) -> Result<TargetPlatform, QaEnvironmentsError>;

    /// Fails with `FailedPrecondition` if the platform currently holds any lease.
    async fn delete_platform(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), QaEnvironmentsError>;

    // ==================== Variables ====================

    /// Variables for env assembly: global pipeline variables plus (if
    /// `platform_id` is Some) that platform's variables. Precedence is applied
    /// by the caller (qa-runs), not here.
    async fn list_variables(
        &self,
        ctx: &SecurityContext,
        platform_id: Option<Uuid>,
    ) -> Result<Vec<Variable>, QaEnvironmentsError>;

    async fn upsert_variable(
        &self,
        ctx: &SecurityContext,
        var: NewVariable,
    ) -> Result<Variable, QaEnvironmentsError>;

    async fn delete_variable(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), QaEnvironmentsError>;

    // ==================== Leases ====================

    /// Attempt to acquire the platform for a run. Never blocks; a conflicting
    /// hold returns `AcquireOutcome::Busy` and the caller queues.
    async fn acquire_lease(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        run_id: Uuid,
        mode: LeaseMode,
    ) -> Result<AcquireOutcome, QaEnvironmentsError>;

    /// Release a run's hold. Idempotent: releasing a non-held run is Ok.
    async fn release_lease(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
        run_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError>;

    /// Fails with `NotFound` for a nonexistent or foreign (wrong-tenant)
    /// platform, rather than reading it as `LeaseState::Free`.
    async fn get_lease(
        &self,
        ctx: &SecurityContext,
        platform_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError>;
}
