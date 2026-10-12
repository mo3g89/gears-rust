//! `POST /qa/v1/runner-credentials` (finding #94, ADR-0008).

use std::sync::Arc;

use axum::Extension;
use axum::http::HeaderMap;

use toolkit::api::canonical_prelude::*;

use crate::api::rest::dto::RunnerCredentialsResp;
use crate::domain::error::DomainError;
use crate::gear::ConcreteAppServices;

/// The header a runner pod sends its projected `ServiceAccount` token in.
/// Not `Authorization`, because the out-of-process middleware re-validates any
/// bearer it sees.
pub const RUNNER_TOKEN_HEADER: &str = "x-qa-runner-token";

/// `POST /qa/v1/runner-credentials` (finding #94). Anonymous at the toolkit
/// layer: a runner pod has no user. The `X-QA-Runner-Token` header is the
/// credential, and the domain service decides everything.
///
/// There is deliberately no `#[tracing::instrument]` here: the default field
/// capture would record the headers, and so the token.
pub async fn runner_credentials(
    headers: HeaderMap,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
) -> ApiResult<Json<RunnerCredentialsResp>> {
    let token = headers
        .get(RUNNER_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or(DomainError::RunnerUnauthenticated)?;
    let files = svc.environments.runner_credentials(token).await?;
    Ok(Json(RunnerCredentialsResp::from(files)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! The handler called directly, as `handlers::environments`' tests do:
    //! `Ok` renders 200 through `Json<_>`, and an `Err` is a `CanonicalError`
    //! rendered through its own `IntoResponse`, which is what reaches a pod.

    use std::sync::Arc;

    use axum::Extension;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::response::IntoResponse;
    use base64::Engine as _;
    use qa_environments_sdk::{CredentialMaterial, CredentialSubmission, NewEnvironment};
    use qa_product_sdk::observation::{
        HealthOutcome, HealthState, ObservationOutcome, ObservedAttrs, PluginObservation,
    };
    use uuid::Uuid;

    use super::{RUNNER_TOKEN_HEADER, runner_credentials};
    use crate::domain::ports::{
        PodIdentityRefusal, PodIdentityVerifier, ProductPluginPort, RunnerPodRefusal,
        RunnerPodResolver, VerifiedRunnerPod,
    };
    use crate::gear::ConcreteAppServices;
    use crate::test_support::{
        FixedPluginPort, ScriptedPlugin, ScriptedPodIdentity, ScriptedRunnerPods, a_runner_pod,
        build_services_for_runner_credentials, ctx,
    };

    const CANARY: &str = "KUBECONFIG-CANARY";

    fn port() -> Arc<dyn ProductPluginPort> {
        let plugin = ScriptedPlugin::vhp_shaped(PluginObservation {
            environment: ObservationOutcome::Detected(ObservedAttrs::default()),
            health: HealthOutcome::Checked {
                state: HealthState::Ok,
                detail: None,
            },
        })
        .mounting(&[("kubeconfig", "/.kube/kubeconfig", Some(0o400))]);
        Arc::new(FixedPluginPort::new(Arc::new(plugin)))
    }

    /// Services holding one VHP environment with the canary, behind the
    /// given verifier, with qa-runs answering `runs` for that environment.
    async fn services(
        tenant: Uuid,
        identity: Arc<dyn PodIdentityVerifier>,
        runs: impl FnOnce(Uuid) -> Result<Uuid, RunnerPodRefusal>,
    ) -> Arc<ConcreteAppServices> {
        // The environment's id exists only after the create, so qa-runs'
        // answer is installed once the row is there.
        let resolver = Arc::new(LateRunnerPods::default());
        let (services, _credstore) =
            build_services_for_runner_credentials(port(), identity, Arc::clone(&resolver) as _)
                .await;
        let environment = services
            .environments
            .create_environment(
                &ctx(tenant),
                NewEnvironment {
                    kubeconfig_credstore_ref: None,
                    name: "runner-target".to_owned(),
                    product_id: Uuid::from_u128(0x9001),
                    description: None,
                    kubeconfig: None,
                    credentials: [(
                        "kubeconfig".to_owned(),
                        CredentialSubmission::Material(CredentialMaterial::new(CANARY.to_owned())),
                    )]
                    .into_iter()
                    .collect(),
                    default_branch: None,
                    is_default: false,
                },
            )
            .await
            .unwrap();
        resolver.set(runs(environment.id));
        services
    }

    /// A resolver whose scripted answer is installed after the row exists.
    #[derive(Default)]
    struct LateRunnerPods(std::sync::Mutex<Option<Result<Uuid, RunnerPodRefusal>>>);

    impl LateRunnerPods {
        fn set(&self, answer: Result<Uuid, RunnerPodRefusal>) {
            *self.0.lock().unwrap() = Some(answer);
        }
    }

    #[async_trait::async_trait]
    impl RunnerPodResolver for LateRunnerPods {
        async fn environment_of(&self, pod: &VerifiedRunnerPod) -> Result<Uuid, RunnerPodRefusal> {
            let answer = self.0.lock().unwrap().expect("installed before the call");
            ScriptedRunnerPods(answer).environment_of(pod).await
        }
    }

    fn with_token(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(RUNNER_TOKEN_HEADER, HeaderValue::from_str(token).unwrap());
        headers
    }

    /// Render the handler's outcome as the response a pod would receive.
    async fn respond(
        headers: HeaderMap,
        services: Arc<ConcreteAppServices>,
    ) -> (StatusCode, Vec<u8>) {
        let response = runner_credentials(headers, Extension(services))
            .await
            .into_response();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, body.to_vec())
    }

    fn proving(tenant: Uuid) -> Arc<dyn PodIdentityVerifier> {
        Arc::new(ScriptedPodIdentity::proving(a_runner_pod(tenant)))
    }

    #[tokio::test]
    async fn a_request_without_the_header_is_401() {
        let tenant = Uuid::new_v4();
        let services = services(tenant, proving(tenant), Ok).await;

        for headers in [HeaderMap::new(), with_token("   ")] {
            let (status, _body) = respond(headers, Arc::clone(&services)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn a_verified_pod_of_a_vouched_run_gets_its_files() {
        let tenant = Uuid::new_v4();
        let services = services(tenant, proving(tenant), Ok).await;

        let (status, body) = respond(with_token("tok"), services).await;

        assert_eq!(status, StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let files = body["files"].as_array().expect("a `files` array");
        assert_eq!(files.len(), 1, "{body}");
        assert_eq!(files[0]["path"], "/.kube/kubeconfig");
        assert_eq!(files[0]["mode"], 0o400);
        let content = base64::engine::general_purpose::STANDARD
            .decode(files[0]["content_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(content, CANARY.as_bytes());
    }

    /// Every 403 has the same body, whichever check refused: a caller learns
    /// nothing about which of its claims failed.
    #[tokio::test]
    async fn a_refused_token_and_an_unvouched_run_answer_the_same_403() {
        let tenant = Uuid::new_v4();
        let refused_token = services(
            tenant,
            Arc::new(ScriptedPodIdentity::refusing(
                PodIdentityRefusal::Unauthenticated("the token is not authenticated"),
            )),
            Ok,
        )
        .await;
        let unvouched_run =
            services(tenant, proving(tenant), |_| Err(RunnerPodRefusal::NotFound)).await;

        let (status_a, body_a) = respond(with_token("tok"), refused_token).await;
        let (status_b, body_b) = respond(with_token("tok"), unvouched_run).await;

        assert_eq!(status_a, StatusCode::FORBIDDEN);
        assert_eq!(status_b, StatusCode::FORBIDDEN);
        assert_eq!(
            String::from_utf8_lossy(&body_a),
            String::from_utf8_lossy(&body_b),
            "the two refusals must be byte-identical"
        );
        assert!(
            !String::from_utf8_lossy(&body_a).contains("the token is not authenticated"),
            "the verifier's reason must not reach the body: {}",
            String::from_utf8_lossy(&body_a)
        );
    }
}
