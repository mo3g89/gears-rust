//! Object-safe client trait for inter-gear consumption via `ClientHub`.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::errors::QaEnvironmentsError;
use crate::models::{
    AcquireOutcome, Environment, EnvironmentPatch, LeaseMode, LeaseState, NewEnvironment,
    NewVariable, Variable,
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
    // ==================== Environments ====================

    async fn get_environment(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Environment, QaEnvironmentsError>;

    async fn list_environments(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Environment>, QaEnvironmentsError>;

    async fn create_environment(
        &self,
        ctx: &SecurityContext,
        new: NewEnvironment,
    ) -> Result<Environment, QaEnvironmentsError>;

    async fn update_environment(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: EnvironmentPatch,
    ) -> Result<Environment, QaEnvironmentsError>;

    /// Fails with `FailedPrecondition` if the environment currently holds any lease.
    async fn delete_environment(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), QaEnvironmentsError>;

    // ==================== Variables ====================

    /// Variables for env assembly: global pipeline variables plus (if
    /// `environment_id` is Some) that environment's variables. Precedence is applied
    /// by the caller (qa-runs), not here.
    async fn list_variables(
        &self,
        ctx: &SecurityContext,
        environment_id: Option<Uuid>,
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

    /// Attempt to acquire the environment for a run. Never blocks; a conflicting
    /// hold returns `AcquireOutcome::Busy` and the caller queues.
    async fn acquire_lease(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
        run_id: Uuid,
        mode: LeaseMode,
    ) -> Result<AcquireOutcome, QaEnvironmentsError>;

    /// Release a run's hold. Idempotent: releasing a non-held run is Ok.
    async fn release_lease(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
        run_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError>;

    /// Fails with `NotFound` for a nonexistent or foreign (wrong-tenant)
    /// environment, rather than reading it as `LeaseState::Free`.
    async fn get_lease(
        &self,
        ctx: &SecurityContext,
        environment_id: Uuid,
    ) -> Result<LeaseState, QaEnvironmentsError>;
}
