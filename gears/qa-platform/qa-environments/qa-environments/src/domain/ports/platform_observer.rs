//! The observation port. Named types are `SecretValue` and `String` — never a
//! Kubernetes type — so `domain/` satisfies ADR-0001's waiver clause that
//! nothing here learns Kubernetes exists. `infra::observer` is the only impl
//! that does.

use async_trait::async_trait;
use credstore_sdk::SecretValue;

use crate::domain::observation::{ClusterHealth, DetectedPlatform};

/// The result of one observation attempt.
///
/// A failure is a **value**, not an error: legacy persists the message in
/// `platforms_meta.version_detect_error` and shows it, because an operator who
/// can see "namespace virtuozzo not found" can fix it, and one who sees a blank
/// page cannot.
#[derive(Debug, Clone)]
pub enum ObservationOutcome {
    Detected(DetectedPlatform),
    Failed(String),
}

/// The result of one cluster-health read, kept separate from
/// [`ObservationOutcome`] because the two fail independently (D-CH-4).
///
/// Legacy couples them — one client, and a client failure fails everything —
/// but version detection reads a single `ConfigMap` in one namespace while
/// health lists nodes and namespaces cluster-wide. A kubeconfig scoped to the
/// platform's namespace detects its version perfectly and is forbidden from
/// listing nodes; coupling would report that platform as having no version,
/// which is false.
#[derive(Debug, Clone)]
pub enum HealthOutcome {
    Checked(ClusterHealth),
    Failed(String),
    /// No cluster read was attempted, so nothing is known — distinct from
    /// [`Self::Failed`], which means one was attempted and did not succeed.
    ///
    /// `record_observation` writes **none** of the five `cluster_*` columns
    /// for this variant, not even `cluster_checked_at`: the platform stays in
    /// the never-checked state, which is the truth when nothing looked.
    /// Carrying no payload is also what keeps it trivially safe under the
    /// no-formatting invariant — there is nothing here to leak.
    NotAttempted,
}

/// Both halves of one `observe` call.
///
/// One call rather than two methods, so one client and one TLS handshake serve
/// both — an `observe_health` alongside `observe` would double the handshakes
/// per platform per ticker cycle.
#[derive(Debug, Clone)]
pub struct PlatformObservation {
    pub platform: ObservationOutcome,
    pub health: HealthOutcome,
}

#[async_trait]
pub trait PlatformObserver: Send + Sync {
    /// Read version, build, namespace, base domain and cluster health from a
    /// platform's cluster.
    async fn observe(&self, kubeconfig: &SecretValue, vpadm_namespace: &str) -> PlatformObservation;

    /// Decision D4: ensure the `Secret` a runner pod mounts exists and matches
    /// the material. Idempotent — safe to call on every create, update and poll.
    async fn ensure_kubeconfig_secret(
        &self,
        credstore_ref: &str,
        kubeconfig: &SecretValue,
    ) -> Result<(), String>;
}

/// The observer a build without `platform-observation` gets.
///
/// It fails loudly rather than returning a plausible empty success: a silent
/// `Ok` from `ensure_kubeconfig_secret` would put the platform straight back
/// into the five-minute `FailedMount` hang with nothing to read.
pub struct NoopObserver;

#[async_trait]
impl PlatformObserver for NoopObserver {
    /// The two halves answer differently on purpose, and the asymmetry is the
    /// whole point.
    ///
    /// The version half stays `Failed`: it lands in `version_detect_error`,
    /// an honest *error* field, and telling an operator "this build has no
    /// Kubernetes client" there is useful — it is why the thing is blank.
    ///
    /// The health half is [`HealthOutcome::NotAttempted`], because its
    /// `Failed` arm lands in `cluster_status` as the literal string
    /// `"Unreachable"`. That is not an error field: it is a *status claim*
    /// about somebody's cluster, and a build with no Kubernetes client has
    /// made no observation entitling it to that claim. Returning `Failed`
    /// here meant one press of the Refresh button (which is not
    /// feature-gated) permanently painted a perfectly healthy platform
    /// dark red, with nothing in such a build able to clear it. D-CH-6 says a
    /// feature-off build falls back to the reachability dot; `NotAttempted`
    /// leaves all five cluster columns `NULL`, which is what makes that
    /// fallback fire.
    async fn observe(&self, _kubeconfig: &SecretValue, _vpadm_namespace: &str) -> PlatformObservation {
        let message = "this build has no Kubernetes client: rebuild with the \
                        `platform-observation` cargo feature to observe a platform's cluster"
            .to_owned();
        PlatformObservation {
            platform: ObservationOutcome::Failed(message),
            health: HealthOutcome::NotAttempted,
        }
    }

    async fn ensure_kubeconfig_secret(
        &self,
        _credstore_ref: &str,
        _kubeconfig: &SecretValue,
    ) -> Result<(), String> {
        Err("this build has no Kubernetes client: the kubeconfig Secret must be provisioned \
             by deploy/argo/provision-platform-kubeconfig-secret.sh, or rebuild with the \
             `platform-observation` cargo feature"
            .to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_noop_observer_reports_a_failure_rather_than_pretending() {
        let observation = NoopObserver.observe(&SecretValue::from("irrelevant".to_owned()), "virtuozzo").await;
        match observation.platform {
            ObservationOutcome::Failed(message) => {
                assert!(
                    message.contains("platform-observation"),
                    "the message must name the missing feature, got: {message}"
                );
            }
            ObservationOutcome::Detected(_) => panic!("the no-op observer must never detect"),
        }
    }

    /// D-CH-4: the two halves are independent, and the no-op observer answers
    /// them differently on purpose.
    ///
    /// The version half is `Failed` — `version_detect_error` is an error
    /// field and naming the missing feature there is useful. The health half
    /// is `NotAttempted`, NOT `Failed`: a `Failed` health outcome persists as
    /// `cluster_status = "Unreachable"`, a status claim about a cluster this
    /// build never looked at, and one press of the un-gated Refresh button
    /// would have painted a healthy platform dark red forever. Anyone
    /// "making the two halves consistent" fails exactly here.
    #[tokio::test]
    async fn the_noop_observer_fails_the_version_half_and_attempts_no_cluster_read() {
        let observation = NoopObserver
            .observe(&SecretValue::from("irrelevant".to_owned()), "virtuozzo")
            .await;
        match observation.platform {
            ObservationOutcome::Failed(message) => assert!(
                message.contains("platform-observation"),
                "the message must name the missing feature, got: {message}"
            ),
            ObservationOutcome::Detected(_) => panic!("the no-op observer must never detect"),
        }
        match observation.health {
            HealthOutcome::NotAttempted => {}
            HealthOutcome::Failed(message) => panic!(
                "a build with no Kubernetes client attempted no cluster read, so it must not \
                 claim one failed -- that persists as cluster_status = \"Unreachable\". Got: \
                 {message}"
            ),
            HealthOutcome::Checked(_) => {
                panic!("a build with no Kubernetes client must never report a cluster read")
            }
        }
    }

    #[tokio::test]
    async fn the_noop_secret_writer_is_an_error_not_a_silent_success() {
        let result = NoopObserver
            .ensure_kubeconfig_secret("platform/x/kubeconfig", &SecretValue::from("irrelevant".to_owned()))
            .await;
        assert!(result.is_err(), "a silent Ok would recreate the FailedMount hang with no signal");
    }
}
