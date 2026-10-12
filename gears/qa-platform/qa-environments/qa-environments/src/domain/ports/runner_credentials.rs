//! The two questions qa-environments' runner-credential route
//! (`POST /qa/v1/runner-credentials`, finding #94) must answer before it
//! hands a runner pod any credential material:
//!
//! 1. [`PodIdentityVerifier`]: is this token a live pod running as the
//!    runner `ServiceAccount`, and which pod is it? The Kubernetes adapter is a
//!    `TokenReview` plus a `GET pod`, uid compared.
//! 2. [`RunnerPodResolver`]: is that pod's run live, and which environment does
//!    it run against? qa-runs answers, through `QaRunsClientV1::resolve_runner_pod`.
//!
//! Both refusals carry `&'static str` only. Nothing a caller sent and nothing
//! the API server said reaches a response, which is why the refusals are
//! enums and not `String`s.

use async_trait::async_trait;
use uuid::Uuid;

/// A pod the verifier has proved: token reviewed, uid matched, phase live.
/// `tenant_id` and `run_id` are read off its labels and are routing hints.
/// `workflow_name` is its controller `ownerReference` (an Argo `Workflow`),
/// which a `Workflow` author cannot set; [`RunnerPodResolver`] binds the three
/// by requiring the run's tenant and `execution_ref` to match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRunnerPod {
    pub pod_name: String,
    pub tenant_id: Uuid,
    pub run_id: Uuid,
    pub workflow_name: String,
}

/// Why a token did not prove a runner pod.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PodIdentityRefusal {
    /// The token, the pod or its labels do not check out. Becomes 403.
    #[cfg_attr(
        all(not(test), not(feature = "runner-credentials")),
        expect(
            dead_code,
            reason = "constructed only by the `TokenReview` adapter \
                      (`infra::pod_identity`), which exists only under the \
                      `runner-credentials` feature"
        )
    )]
    Unauthenticated(&'static str),
    /// The API server could not be asked. Becomes 503.
    Unavailable(&'static str),
}

/// Prove that a projected `ServiceAccount` token belongs to a live runner pod.
#[async_trait]
pub trait PodIdentityVerifier: Send + Sync {
    /// The pod `token` was issued to, or why it proves none.
    async fn verify(&self, token: &str) -> Result<VerifiedRunnerPod, PodIdentityRefusal>;
}

/// Why qa-runs did not bind a verified pod to an environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerPodRefusal {
    /// qa-runs refused: no such live run for this workflow. Becomes 403.
    NotFound,
    /// qa-runs could not be asked. Becomes 503.
    Unavailable(&'static str),
}

/// Bind a verified pod to the environment its live run targets.
#[async_trait]
pub trait RunnerPodResolver: Send + Sync {
    /// The environment the pod's run runs against, or why there is none.
    async fn environment_of(&self, pod: &VerifiedRunnerPod) -> Result<Uuid, RunnerPodRefusal>;
}

/// The verifier of a build without the `runner-credentials` feature: no kube
/// client, so nothing can be verified. It refuses as unavailable (503), not
/// as forbidden: the deployment is misbuilt, and the pod did nothing wrong.
#[cfg_attr(
    all(not(test), feature = "runner-credentials"),
    expect(
        dead_code,
        reason = "a `runner-credentials` build wires `KubePodIdentityVerifier` instead"
    )
)]
pub struct DenyingPodIdentityVerifier;

#[async_trait]
impl PodIdentityVerifier for DenyingPodIdentityVerifier {
    async fn verify(&self, _token: &str) -> Result<VerifiedRunnerPod, PodIdentityRefusal> {
        Err(PodIdentityRefusal::Unavailable(
            "this build has no runner-credentials feature, so no pod can be verified",
        ))
    }
}
