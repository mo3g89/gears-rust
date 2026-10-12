//! `POST /qa/v1/runner-credentials`'s domain half (finding #94): hand a runner
//! pod the material of its own run's environment, and nothing else.
//!
//! The order is the security argument, so it is fixed:
//! 1. the pod is proved ([`PodIdentityVerifier`]: `TokenReview` + uid);
//! 2. the run is proved (qa-runs: live, and its handle is this pod's workflow);
//! 3. the environment is read as this gear's system actor, bound to the pod's
//!    tenant, through the policy enforcer, never `AccessScope::allow_all()`;
//! 4. the plugin says which credentials the run mounts (`prepare_run_access`),
//!    and only those are read from credstore.
//!
//! Nothing is read from credstore before steps 1 and 2 succeed, and nothing
//! at all when the plugin names a reference the environment does not hold:
//! "only the environment's credentials" is checked here, not trusted to the
//! plugin. A pod labelled with the nil tenant is refused before qa-runs is
//! asked. The whole call is bounded by [`RUNNER_CREDENTIALS_DEADLINE`], so a
//! hung API server, qa-runs, plugin or credstore is a 503, not a held
//! connection.
//!
//! Every refusal is `RunnerForbidden` or `RunnerCredentialsUnavailable`, and
//! neither carries text. The audit trail is the `qa_environments.runner_credentials` log
//! target, because this gear has no audit store.
//!
//! # A child of `environments`, not a sibling
//!
//! Declared from `environments.rs` with `#[path]`, as `credentials` is, and
//! for that module's reason: it reads `self.pod_identity`, `self.repo`,
//! `self.policy_enforcer` and the rest, which are private to `environments`. A
//! sibling would have forced every one of them to `pub(super)`.
//!
//! # What the grant line logs
//!
//! The pod, its tenant and run, the environment, and a **count** of files.
//! Not the credstore references: they are not material, but under
//! `SharingMode::Tenant` a reference is a read path to it, which is why
//! `environments_kubeconfig_tests`' leak test keeps them out of every response
//! body and why `EnvironmentDto` withholds them.
//!
//! [`PodIdentityVerifier`]: crate::domain::ports::PodIdentityVerifier

use std::collections::HashSet;
use std::time::Duration;

use credstore_sdk::SecretValue;
use qa_environments_sdk::Environment;
use qa_product_sdk::plugin::{CredentialSlot, EnvironmentHandle};
use qa_product_sdk::{MountSpec, RunAccess};
use toolkit_security::SecurityContext;
use tracing::{info, warn};
use uuid::Uuid;

use super::EnvironmentsService;
use crate::domain::error::DomainError;
use crate::domain::ports::{PodIdentityRefusal, RunnerPodRefusal, VerifiedRunnerPod};
use crate::domain::repos::{EnvironmentsRepository, LeasesRepository};
use crate::domain::service::{actions, resources};
use crate::domain::system_actor;

/// One file the runner writes before any test code runs.
pub struct RunnerFile {
    pub path: String,
    pub mode: Option<i32>,
    pub content: SecretValue,
}

// Hand-written so the redaction is this type's own property, not a property of
// `SecretValue`'s `Debug` that a future change there could drop.
impl std::fmt::Debug for RunnerFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerFile")
            .field("path", &self.path)
            .field("mode", &self.mode)
            .field("content", &"<redacted>")
            .finish()
    }
}

/// The log target every grant and refusal of the route is written under.
const TARGET: &str = "qa_environments.runner_credentials";

/// Why a pod was refused when the environment is not readable as its tenant.
const NOT_VISIBLE: &str = "the environment is not visible to the pod's tenant";

/// The bound on one whole [`EnvironmentsService::runner_credentials`] call:
/// the `TokenReview`, the pod read, qa-runs, the database, the plugin and
/// every credstore read together. A runner pod retries, so a hung dependency
/// becomes a 503 it can retry rather than a connection held open.
pub const RUNNER_CREDENTIALS_DEADLINE: Duration = Duration::from_secs(30);

impl<P: EnvironmentsRepository, L: LeasesRepository> EnvironmentsService<P, L> {
    /// The files a runner pod writes before its tests start: one per
    /// [`MountSpec::Secret`] the environment's plugin names for the run, each
    /// with the material credstore holds under it.
    ///
    /// The four steps of the module doc, one helper each, in that order.
    ///
    /// # Errors
    ///
    /// [`DomainError::RunnerForbidden`] when the token, the pod, the run or
    /// the environment does not check out; [`DomainError::RunnerCredentialsUnavailable`]
    /// when Kubernetes, qa-runs, the database, the plugin or credstore cannot
    /// answer. Neither carries any text.
    pub async fn runner_credentials(&self, token: &str) -> Result<Vec<RunnerFile>, DomainError> {
        tokio::time::timeout(
            RUNNER_CREDENTIALS_DEADLINE,
            self.runner_credentials_unbounded(token),
        )
        .await
        .unwrap_or_else(|_elapsed| {
            // No pod fields: the deadline may expire before one is proved.
            warn!(
                target: TARGET,
                reason = "the request did not complete within its deadline",
                "runner credentials unavailable"
            );
            Err(DomainError::RunnerCredentialsUnavailable)
        })
    }

    /// [`Self::runner_credentials`] without its deadline.
    async fn runner_credentials_unbounded(
        &self,
        token: &str,
    ) -> Result<Vec<RunnerFile>, DomainError> {
        let pod = self.prove_runner_pod(token).await?;
        let environment_id = self.prove_runner_run(&pod).await?;
        let ctx = system_actor::for_runner_credentials(pod.tenant_id);
        let (environment, owner_tenant) = self
            .read_runner_environment(&ctx, &pod, environment_id)
            .await?;
        let access = self.runner_access(&ctx, &pod, &environment).await?;

        // Every secret mount must name one of the environment's own
        // references, checked for all of them before any is read: a plugin
        // that names another reference would otherwise turn this route into a
        // read of any secret the owning tenant holds.
        let held: HashSet<&str> = environment
            .credentials
            .iter()
            .map(|credential| credential.credstore_ref.as_str())
            .collect();
        let mut secrets = Vec::with_capacity(access.mounts.len());
        for mount in &access.mounts {
            let MountSpec::Secret {
                credstore_ref,
                path,
                mode,
            } = mount
            else {
                continue;
            };
            if !held.contains(credstore_ref.as_str()) {
                unavailable(
                    &pod,
                    "the plugin named a credential this environment does not hold",
                );
                return Err(DomainError::RunnerCredentialsUnavailable);
            }
            secrets.push((credstore_ref, path, *mode));
        }

        // Only now is credstore read, and only for what the plugin named.
        let mut files = Vec::with_capacity(secrets.len());
        for (credstore_ref, path, mode) in secrets {
            // The error is dropped, not logged: its text names the reference.
            let content = self
                .fetch_credential_material(owner_tenant, Some(environment_id), credstore_ref)
                .await
                .map_err(|_| {
                    unavailable(&pod, "a credential could not be read from credstore");
                    DomainError::RunnerCredentialsUnavailable
                })?;
            files.push(RunnerFile {
                path: path.clone(),
                mode,
                content,
            });
        }

        info!(
            target: TARGET,
            pod = %pod.pod_name,
            tenant_id = %pod.tenant_id,
            run_id = %pod.run_id,
            environment_id = %environment_id,
            count = files.len(),
            "runner credentials granted"
        );
        Ok(files)
    }

    /// Step 1: the token is a live runner pod's, labelled with a real tenant.
    async fn prove_runner_pod(&self, token: &str) -> Result<VerifiedRunnerPod, DomainError> {
        let pod = self
            .pod_identity
            .verify(token)
            .await
            .map_err(|refusal| match refusal {
                PodIdentityRefusal::Unauthenticated(reason) => {
                    warn!(target: TARGET, reason, "runner credentials denied");
                    DomainError::RunnerForbidden
                }
                PodIdentityRefusal::Unavailable(reason) => {
                    warn!(target: TARGET, reason, "runner credentials unavailable");
                    DomainError::RunnerCredentialsUnavailable
                }
            })?;
        // qa-runs refuses a nil tenant too; refusing here keeps the question
        // from being asked at all.
        if pod.tenant_id.is_nil() {
            deny(&pod, "the pod's tenant label is the nil UUID");
            return Err(DomainError::RunnerForbidden);
        }
        Ok(pod)
    }

    /// Step 2: qa-runs vouches for the pod's run, and names its environment.
    async fn prove_runner_run(&self, pod: &VerifiedRunnerPod) -> Result<Uuid, DomainError> {
        self.runner_pods
            .environment_of(pod)
            .await
            .map_err(|refusal| match refusal {
                RunnerPodRefusal::NotFound => {
                    deny(pod, "qa-runs does not vouch for this run");
                    DomainError::RunnerForbidden
                }
                RunnerPodRefusal::Unavailable(reason) => {
                    unavailable(pod, reason);
                    DomainError::RunnerCredentialsUnavailable
                }
            })
    }

    /// Step 3: the environment and its owning tenant, read under `ctx` (the
    /// system actor in the pod's tenant) through the PEP like every other
    /// read. A row that scope cannot see is a refusal, not a 404.
    async fn read_runner_environment(
        &self,
        ctx: &SecurityContext,
        pod: &VerifiedRunnerPod,
        environment_id: Uuid,
    ) -> Result<(Environment, Uuid), DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope(
                ctx,
                &resources::PLATFORM,
                actions::GET,
                Some(environment_id),
            )
            .await
            .map_err(|e| {
                if matches!(DomainError::from(e), DomainError::Forbidden) {
                    deny(pod, NOT_VISIBLE);
                    DomainError::RunnerForbidden
                } else {
                    unavailable(pod, "the authorization scope could not be resolved");
                    DomainError::RunnerCredentialsUnavailable
                }
            })?;
        let conn = self.db.conn().map_err(|_| {
            unavailable(pod, "the database is not reachable");
            DomainError::RunnerCredentialsUnavailable
        })?;
        let read_failed = |_: DomainError| {
            unavailable(pod, "the environment could not be read");
            DomainError::RunnerCredentialsUnavailable
        };
        let environment = self
            .repo
            .get(&conn, &scope, environment_id)
            .await
            .map_err(read_failed)?;
        let owner_tenant = self
            .repo
            .owner_tenant(&conn, &scope, environment_id)
            .await
            .map_err(read_failed)?;
        environment.zip(owner_tenant).ok_or_else(|| {
            deny(pod, NOT_VISIBLE);
            DomainError::RunnerForbidden
        })
    }

    /// Step 4: what the run mounts, from the environment's plugin, handed
    /// references only (`CredentialSlot::reference_only`), as dispatch does.
    async fn runner_access(
        &self,
        ctx: &SecurityContext,
        pod: &VerifiedRunnerPod,
        environment: &Environment,
    ) -> Result<RunAccess, DomainError> {
        let plugin = self
            .product_plugins
            .plugin_for(ctx, environment.product_id)
            .await
            .map_err(|unavailable_plugin| {
                unavailable(pod, unavailable_plugin.detail());
                DomainError::RunnerCredentialsUnavailable
            })?;
        let slots: Vec<CredentialSlot> = environment
            .credentials
            .iter()
            .map(|credential| {
                CredentialSlot::reference_only(
                    credential.key.clone(),
                    credential.credstore_ref.clone(),
                )
            })
            .collect();
        let observed =
            (!environment.observed_attrs.is_empty()).then_some(&environment.observed_attrs);
        let handle = EnvironmentHandle {
            slots: &slots,
            config: &environment.config,
            observed,
        };
        plugin.prepare_run_access(&handle).await.map_err(|failure| {
            unavailable(
                pod,
                failure
                    .detail
                    .unwrap_or("the product plugin could not prepare the run's access"),
            );
            DomainError::RunnerCredentialsUnavailable
        })
    }
}

fn deny(pod: &VerifiedRunnerPod, reason: &'static str) {
    warn!(
        target: TARGET,
        pod = %pod.pod_name,
        tenant_id = %pod.tenant_id,
        run_id = %pod.run_id,
        reason,
        "runner credentials denied"
    );
}

fn unavailable(pod: &VerifiedRunnerPod, reason: &'static str) {
    warn!(
        target: TARGET,
        pod = %pod.pod_name,
        tenant_id = %pod.tenant_id,
        run_id = %pod.run_id,
        reason,
        "runner credentials unavailable"
    );
}
