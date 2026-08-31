//! A [`PlatformObserver`] backed by a real Kubernetes client, built fresh
//! from the platform's own stored kubeconfig on every call.
//!
//! Mirrors `manager/src/services/platforms.rs:1037-1149`
//! (`detect_platform_version` and `detect_base_domain`) line for line,
//! including its error wording: those strings are persisted and shown to an
//! operator, and "namespaces virtuozzo not found" is what makes a broken
//! platform fixable rather than merely broken.

use async_trait::async_trait;
use credstore_sdk::SecretValue;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Node};
use kube::api::{Api, ListParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};

use crate::domain::observation::{
    ClusterHealth, DetectedPlatform, NodeSummary, compute_base_domain, detected_from_data,
    external_hosts,
};
use crate::domain::ports::{HealthOutcome, ObservationOutcome, PlatformObservation, PlatformObserver};
use super::errors::{CLIENT_BUILD_FAILURE, describe_kube_error, describe_kubeconfig_error};

/// The `ConfigMap` vpadm writes at install time, carrying `platformVersion`.
const CORE_INSTALL_METADATA: &str = "core-install-metadata";

/// The `ConfigMap` vpadm writes with each component's resolved hostname, used
/// for best-effort base-domain detection.
const GATEWAY_HOSTNAMES_CONFIGMAP: &str = "vp-gateway-hostnames";

/// Reads a platform's own cluster to detect its installed version and base
/// domain, and materialises the kubeconfig `Secret` a runner pod mounts.
///
/// `observe` builds its client from the kubeconfig it is handed, not from the
/// four fields below — those exist for [`Self::ensure_kubeconfig_secret`],
/// which writes into the **Argo** cluster instead: `argo_kubeconfig_path`
/// names the kubeconfig to reach it (empty means `Config::infer()`, correct
/// from inside that cluster), `argo_namespace` and `secret_key` name where
/// and under what key the material goes, and `secret_prefix` derives the
/// `Secret`'s name so it agrees with what `qa-runs`' executor mounts and what
/// `deploy/argo/provision-platform-kubeconfig-secret.sh` writes today.
pub struct KubeObserver {
    argo_kubeconfig_path: Option<String>,
    argo_namespace: String,
    secret_prefix: String,
    secret_key: String,
}

impl KubeObserver {
    #[must_use]
    pub fn new(
        argo_kubeconfig_path: Option<String>,
        argo_namespace: String,
        secret_prefix: String,
        secret_key: String,
    ) -> Self {
        Self { argo_kubeconfig_path, argo_namespace, secret_prefix, secret_key }
    }

    /// Build a client from a platform's stored kubeconfig bytes.
    ///
    /// Every step here is a fact about the material itself (not valid UTF-8,
    /// not valid YAML, kubeconfig fields don't resolve to a usable config,
    /// the resulting config can't build a client) and is returned as a
    /// `String` rather than propagated with `?`, because the caller has no
    /// error type to propagate into — its return is `ObservationOutcome`, a
    /// value, not a `Result`.
    ///
    /// # Not one of those `String`s is derived from the bytes
    ///
    /// Every message here is assembled from literals in this crate. The
    /// upstream errors are **classified** ([`super::errors`]) and never
    /// formatted, because the value returned from this function is persisted
    /// into `qa_platforms.version_detect_error`, published on `PlatformDto`
    /// to every `qa.platform` GET/LIST-authorized caller, and rendered on the
    /// platform page — so a message carrying its input is the kubeconfig
    /// leaving the gear, which is the one property this whole module rests
    /// on. That is not hypothetical: it was measured on 2026-08-28, with a
    /// PEM private key pasted into the create dialog's kubeconfig field
    /// coming back out through this exact `map_err`. See [`super::errors`]
    /// for the rule and for why it is a classification rather than a filter.
    ///
    /// Nothing is logged here either, for the same reason and one more: the
    /// leak canary below asserts against the raw `tracing` output, and a
    /// `debug!` carrying the parse error would be that leak with a different
    /// destination.
    async fn client_from_kubeconfig(kubeconfig: &SecretValue) -> Result<Client, String> {
        let text = std::str::from_utf8(kubeconfig.as_bytes())
            .map_err(|_| "kubeconfig is not valid UTF-8".to_owned())?;
        let parsed = Kubeconfig::from_yaml(text).map_err(|error| {
            format!("Invalid kubeconfig format: {}", describe_kubeconfig_error(&error))
        })?;
        let config = Config::from_custom_kubeconfig(parsed, &KubeConfigOptions::default())
            .await
            .map_err(|error| {
                format!("Failed to load kubeconfig: {}", describe_kubeconfig_error(&error))
            })?;
        Client::try_from(config)
            .map_err(|_| format!("Failed to create Kubernetes client: {CLIENT_BUILD_FAILURE}"))
    }

    /// Version detection: the targeted namespace first, then an
    /// all-namespace scan by the vpadm label.
    async fn detect(client: &Client, vpadm_namespace: &str) -> Result<DetectedPlatform, String> {
        // 1. Targeted lookup in the configured (or default) vpadm namespace.
        let targeted: Api<ConfigMap> = Api::namespaced(client.clone(), vpadm_namespace);
        match targeted.get_opt(CORE_INSTALL_METADATA).await {
            Ok(Some(cm)) => {
                let namespace = cm.metadata.namespace.clone().unwrap_or_default();
                if let Some(detected) =
                    detected_from_data(&cm.data.unwrap_or_default(), &namespace, vpadm_namespace)
                {
                    return Ok(detected);
                }
                // Present but no usable platformVersion — fall through to the scan.
            }
            Ok(None) => {
                // Not in the targeted namespace — fall through to the scan.
            }
            Err(error) => {
                return Err(format!(
                    "Failed to read core-install-metadata in namespace {vpadm_namespace}: {}",
                    describe_kube_error(&error)
                ));
            }
        }

        // 2. Fallback: scan all namespaces by the vpadm label.
        let all: Api<ConfigMap> = Api::all(client.clone());
        let list = all
            .list(
                &ListParams::default()
                    .labels("app.kubernetes.io/managed-by=vpadm")
                    .fields("metadata.name=core-install-metadata"),
            )
            .await
            .map_err(|error| {
                format!("Failed to list core-install-metadata: {}", describe_kube_error(&error))
            })?;

        if list.items.len() > 1 {
            tracing::warn!(
                count = list.items.len(),
                "multiple core-install-metadata ConfigMaps found; taking first"
            );
        }

        let cm = list
            .items
            .into_iter()
            .next()
            .ok_or_else(|| "vpadm install metadata not found".to_owned())?;

        let namespace = cm.metadata.namespace.clone().unwrap_or_default();
        detected_from_data(&cm.data.unwrap_or_default(), &namespace, "unknown")
            .ok_or_else(|| "platformVersion field missing in core-install-metadata".to_owned())
    }

    /// Best-effort base-domain detection from `vp-gateway-hostnames` in the
    /// namespace the version was detected in. Never a failure: an unreadable
    /// or missing gateway map is inconclusive, not an error, so it must not
    /// turn a good version detection into a failed one. `None` here tells a
    /// caller to keep any previously stored value rather than clobber it.
    async fn detect_base_domain(client: &Client, namespace: &str) -> Option<String> {
        let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
        let cm = match api.get_opt(GATEWAY_HOSTNAMES_CONFIGMAP).await {
            Ok(Some(cm)) => cm,
            Ok(None) => return None,
            Err(error) => {
                // `describe_kube_error`, not `%error`, for the same reason as
                // every other site in this file: a `kube::Error` from a call
                // made with a platform's own kubeconfig can box an
                // `AuthError::AuthExecRun`, which prints an exec credential
                // plugin's whole stdout/stderr. A log line is still the
                // material leaving the gear.
                tracing::warn!(
                    namespace,
                    error = %describe_kube_error(&error),
                    "failed to read {GATEWAY_HOSTNAMES_CONFIGMAP} for base-domain detection"
                );
                return None;
            }
        };

        let hosts = external_hosts(&cm.data.unwrap_or_default());
        compute_base_domain(&hosts)
    }

    /// Read the cluster's nodes and namespaces.
    ///
    /// Two independent reads. The node list determines the status, so a failure
    /// there fails the whole health outcome; the namespace count is
    /// supplementary, so a failure there leaves it `None` (spec §4.3) rather
    /// than making a healthy cluster look unhealthy.
    ///
    /// Nodes are sorted by name, as legacy sorts them, so the rendered table is
    /// stable across ticks rather than reordering on every poll.
    async fn check_health(client: &Client) -> Result<ClusterHealth, String> {
        let nodes_api: Api<Node> = Api::all(client.clone());
        let namespaces_api: Api<Namespace> = Api::all(client.clone());

        let items = nodes_api
            .list(&ListParams::default())
            .await
            .map_err(|error| describe_kube_error(&error))?;

        let mut nodes: Vec<NodeSummary> = items
            .items
            .into_iter()
            .map(|node| {
                let labels = node.metadata.labels.unwrap_or_default();
                // k3s sets both; legacy checks both.
                let control_plane = labels.contains_key("node-role.kubernetes.io/control-plane")
                    || labels.contains_key("node-role.kubernetes.io/master");
                let status = node.status.as_ref();
                // Legacy's `unwrap_or(false)`: an absent Ready condition is not
                // ready. A node that does not say it is ready is not ready.
                let ready = status
                    .and_then(|s| s.conditions.as_ref())
                    .and_then(|c| c.iter().find(|c| c.type_ == "Ready"))
                    .is_some_and(|c| c.status == "True");
                let info = status.and_then(|s| s.node_info.as_ref());
                NodeSummary {
                    name: node.metadata.name.unwrap_or_else(|| "unknown-node".to_owned()),
                    control_plane,
                    ready,
                    kubelet_version: info.map(|i| i.kubelet_version.clone()),
                    os_image: info.map(|i| i.os_image.clone()),
                }
            })
            .collect();
        nodes.sort_by(|a, b| a.name.cmp(&b.name));

        let namespace_count = match namespaces_api.list(&ListParams::default()).await {
            Ok(list) => u32::try_from(list.items.len()).ok(),
            Err(error) => {
                // Classified, not formatted, even though this one only reaches a
                // log line: the rule is the rule everywhere in this module.
                tracing::debug!(
                    reason = describe_kube_error(&error),
                    "namespace list failed; the count is left unknown"
                );
                None
            }
        };

        Ok(ClusterHealth { nodes, namespace_count })
    }
}

#[async_trait]
impl PlatformObserver for KubeObserver {
    async fn observe(&self, kubeconfig: &SecretValue, vpadm_namespace: &str) -> PlatformObservation {
        let client = match Self::client_from_kubeconfig(kubeconfig).await {
            Ok(client) => client,
            Err(message) => {
                // One client for both halves, so a client that could not be
                // built fails both with the same classified message.
                return PlatformObservation {
                    platform: ObservationOutcome::Failed(message.clone()),
                    health: HealthOutcome::Failed(message),
                };
            }
        };

        let platform = match Self::detect(&client, vpadm_namespace).await {
            Ok(mut detected) => {
                detected.base_domain = Self::detect_base_domain(&client, &detected.namespace).await;
                ObservationOutcome::Detected(detected)
            }
            Err(message) => ObservationOutcome::Failed(message),
        };

        let health = match Self::check_health(&client).await {
            Ok(health) => HealthOutcome::Checked(health),
            Err(message) => HealthOutcome::Failed(message),
        };

        PlatformObservation { platform, health }
    }

    /// Decision D4: materialise the `Secret` a runner pod mounts, in the
    /// **Argo** cluster — a different cluster and a different client than
    /// the one `observe` above builds. See [`super::secret_writer`] for why
    /// that distinction is the whole point.
    async fn ensure_kubeconfig_secret(
        &self,
        credstore_ref: &str,
        kubeconfig: &SecretValue,
    ) -> Result<(), String> {
        super::secret_writer::ensure_kubeconfig_secret(
            self.argo_kubeconfig_path.as_deref(),
            &self.argo_namespace,
            &self.secret_prefix,
            &self.secret_key,
            credstore_ref,
            kubeconfig,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    fn observer() -> KubeObserver {
        KubeObserver::new(None, "argo".to_owned(), "qa-platform-".to_owned(), "value".to_owned())
    }

    #[tokio::test]
    async fn a_malformed_kubeconfig_is_a_failure_naming_the_defect() {
        let outcome = observer().observe(&SecretValue::from(vec![0xFF, 0xFE]), "virtuozzo").await;
        match outcome.platform {
            ObservationOutcome::Failed(message) => {
                assert_eq!(message, "kubeconfig is not valid UTF-8");
            }
            ObservationOutcome::Detected(_) => panic!("invalid bytes must never detect"),
        }
    }

    /// Where the live cluster's kubeconfig is. Absent means "skip", not
    /// "fail" — this suite is meant to run without a cluster available, and a
    /// bare `cargo test` on a machine with no cluster should say so loudly
    /// rather than look like a passing detection.
    fn live_kubeconfig_path() -> Option<String> {
        std::env::var("QA_ENV_OBSERVER_TEST_KUBECONFIG").ok().filter(|value| !value.trim().is_empty())
    }

    /// Reads the real vhp cluster's `core-install-metadata` and
    /// `vp-gateway-hostnames` end to end, through every layer this task
    /// added: kubeconfig parsing, client construction, the targeted lookup,
    /// and base-domain derivation.
    ///
    /// Gated on `QA_ENV_OBSERVER_TEST_KUBECONFIG` because it is the one test
    /// here that is not a double — it needs a real API server reachable from
    /// wherever this runs. **A silent skip is this project's documented
    /// trap**: `cargo test` hides stderr for a passing test, and a `return`
    /// with no output at all would be indistinguishable from a real green
    /// detection. `eprintln!` is the loudest signal available inside the
    /// test harness's constraints; run with `-- --nocapture` (as this
    /// module's doc example does) to see it even when the variable is set.
    #[tokio::test]
    async fn the_real_clusters_metadata_and_gateway_map_detect_correctly() {
        let Some(path) = live_kubeconfig_path() else {
            eprintln!(
                "SKIPPED: the_real_clusters_metadata_and_gateway_map_detect_correctly -- \
                 QA_ENV_OBSERVER_TEST_KUBECONFIG is unset, so this test cannot reach a real \
                 cluster and did not run. Set it to a kubeconfig path to exercise this check."
            );
            return;
        };
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("QA_ENV_OBSERVER_TEST_KUBECONFIG names an unreadable file {path}: {error}"));

        let outcome = observer().observe(&SecretValue::from(text), "virtuozzo").await;
        match outcome.platform {
            ObservationOutcome::Detected(detected) => {
                assert_eq!(detected.version, "26.5");
                assert_eq!(detected.build.as_deref(), Some("0"));
                assert_eq!(detected.namespace, "virtuozzo");
                assert_eq!(detected.base_domain.as_deref(), Some("sv.jele.io"));
            }
            ObservationOutcome::Failed(message) => {
                panic!("expected a successful detection against the live cluster, got: {message}");
            }
        }
        match outcome.health {
            HealthOutcome::Checked(health) => {
                assert!(!health.nodes.is_empty(), "the real cluster must report at least one node");
            }
            HealthOutcome::Failed(message) => {
                panic!("expected a successful health read against the live cluster, got: {message}");
            }
            HealthOutcome::NotAttempted => {
                panic!("KubeObserver always attempts the read; NotAttempted is NoopObserver's answer");
            }
        }
    }

    #[tokio::test]
    async fn unparsable_yaml_is_a_failure_not_a_panic() {
        let outcome = observer().observe(&SecretValue::from("not: [valid, kubeconfig".to_owned()), "virtuozzo").await;
        match outcome.platform {
            ObservationOutcome::Failed(message) => {
                assert!(message.starts_with("Invalid kubeconfig format:"), "was: {message}");
            }
            ObservationOutcome::Detected(_) => panic!("malformed YAML must never detect"),
        }
    }

    /// A shared writer that appends every byte `tracing-subscriber` emits
    /// into one buffer, independent of span names or field filters.
    ///
    /// `tracing-test` (used elsewhere in this crate) is deliberately not used
    /// here: it keeps only lines containing the test's span name, so a
    /// multi-line value — exactly the shape a leaked kubeconfig has — would
    /// survive capture only as its first line, and a leak on any later line
    /// would read as "no leak". This buffer has no such hole: it is
    /// everything `tracing` wrote, verbatim.
    #[derive(Clone)]
    struct RawBuffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for RawBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            // `unwrap` on a lock is allowed in tests crate-wide (clippy.toml
            // `allow-unwrap-in-tests`); a poisoned lock here is a test bug.
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for RawBuffer {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// The canary sits directly in `client-key-data`'s value, unencoded and
    /// outside any comment sub-field — where an accidental
    /// `tracing::info!(%kubeconfig)` or similar would actually print it. A
    /// canary buried inside something that itself gets base64-encoded (as an
    /// OpenSSH-style key comment would be) survives only as unrecognisable
    /// base64 noise even when the whole blob leaks, which makes the
    /// assertion pass regardless of whether a leak occurred — the mistake
    /// this test exists to not repeat.
    const CANARY: &str = "CANARY-f2e7c1-observer-leak-probe-do-not-log-me";

    fn fixture_kubeconfig() -> String {
        format!(
            "apiVersion: v1\n\
             kind: Config\n\
             clusters:\n\
             \x20 - cluster:\n\
             \x20     server: https://127.0.0.1:1\n\
             \x20   name: default\n\
             contexts:\n\
             \x20 - context:\n\
             \x20     cluster: default\n\
             \x20     user: default\n\
             \x20   name: default\n\
             current-context: default\n\
             users:\n\
             \x20 - name: default\n\
             \x20   user:\n\
             \x20     client-certificate-data: not-a-real-certificate\n\
             \x20     client-key-data: \"-----BEGIN EC PRIVATE KEY-----\\n{CANARY}\\n-----END EC PRIVATE KEY-----\\n\"\n"
        )
    }

    /// Observe `document` with every `tracing` byte captured, and return both
    /// halves of what could leak: the `PlatformObservation`'s two published
    /// messages (`platform` persisted into `qa_platforms.version_detect_error`
    /// and published on `PlatformDto`; `health` published alongside it) and
    /// the raw log.
    ///
    /// A thread-local default subscriber, not `with_default`'s sync-closure
    /// form: `observe` is `async` and must stay under this subscriber across
    /// every `.await` point. `#[tokio::test]` defaults to a current-thread
    /// runtime, so the whole call runs on this one thread and the guard sees
    /// everything it logs.
    async fn observe_capturing_logs(document: String) -> (PlatformObservation, String) {
        let buffer = RawBuffer(Arc::new(Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .finish();

        let guard = tracing::subscriber::set_default(subscriber);
        let observation = observer().observe(&SecretValue::from(document), "virtuozzo").await;
        drop(guard);

        let captured = String::from_utf8_lossy(&buffer.0.lock().unwrap()).into_owned();
        (observation, captured)
    }

    /// Assert the three surfaces a kubeconfig can escape through — the
    /// persisted/published `platform` failure message, the equally published
    /// `health` failure message, and the log — carry neither the canary nor
    /// any PEM marker.
    ///
    /// Every fixture this is called with fails inside `client_from_kubeconfig`
    /// (before a `Client` ever exists), so `observe` fails both halves with
    /// the same classified message (see `KubeObserver::observe`) — checking
    /// both here is not redundant, it is the only thing that would catch a
    /// future edit that let one half's message diverge from the other's.
    fn assert_nothing_leaked(label: &str, observation: &PlatformObservation, captured: &str) {
        let ObservationOutcome::Failed(platform_message) = &observation.platform else {
            panic!("{label}: this fixture must never produce a detection");
        };
        let HealthOutcome::Failed(health_message) = &observation.health else {
            panic!("{label}: this fixture must never produce a successful health check");
        };
        for (surface, message) in [("platform", platform_message), ("health", health_message)] {
            assert!(
                !message.contains(CANARY),
                "{label}: the {surface} failure message carries the material, and that message \
                 is a published surface:\n{message}"
            );
            assert!(
                !message.contains("BEGIN"),
                "{label}: the {surface} failure message carries PEM material:\n{message}"
            );
        }
        assert!(
            !captured.contains(CANARY),
            "{label}: the material reached a log line:\n{captured}"
        );
        assert!(
            !captured.contains("BEGIN"),
            "{label}: PEM material reached a log line:\n{captured}"
        );
    }

    #[tokio::test]
    async fn the_kubeconfig_never_reaches_the_log() {
        // The server is unreachable by construction, so this is expected to
        // fail — that is not what this test is about. What matters is what
        // reached the message and the log while it ran.
        let (observation, captured) = observe_capturing_logs(fixture_kubeconfig()).await;
        assert_nothing_leaked("well-formed kubeconfig, unreachable server", &observation, &captured);
    }

    /// **The C1 regression test.** This is the branch the canary above never
    /// reached: its fixture is well-formed YAML, so the failure it observes
    /// comes from the TCP connect and `Kubeconfig::from_yaml`'s error path
    /// was never under test. Measured on 2026-08-28, before the fix, this
    /// produced
    ///
    /// ```text
    /// Invalid kubeconfig format: the structure of the parsed kubeconfig is
    /// invalid: invalid type: string "-----BEGIN EC PRIVATE KEY----- … ",
    /// expected struct Kubeconfig
    /// ```
    ///
    /// — the whole pasted document, stored in Postgres and rendered in the
    /// browser for every `qa.platform` reader in the tenant.
    ///
    /// Pasting a private key file into a field labelled "paste a kubeconfig"
    /// is the single most likely operator mistake on the create dialog, which
    /// is why this is the fixture rather than a synthetic one.
    #[tokio::test]
    async fn a_pasted_private_key_never_comes_back_out() {
        let document = format!(
            "-----BEGIN EC PRIVATE KEY-----\n{CANARY}\n-----END EC PRIVATE KEY-----\n"
        );
        let (observation, captured) = observe_capturing_logs(document).await;
        assert_nothing_leaked("a PEM private key pasted as a kubeconfig", &observation, &captured);

        let ObservationOutcome::Failed(message) = observation.platform else { unreachable!() };
        assert_eq!(
            message,
            "Invalid kubeconfig format: it is valid YAML but does not have a kubeconfig's \
             shape - check that what was stored is a kubeconfig file (`apiVersion: v1`, \
             `kind: Config`, with `clusters`, `contexts` and `users`) and not a certificate, \
             a private key, or some other YAML",
            "the operator still has to be told what to do about it"
        );
    }

    /// The second shape the review reproduced: a document that *is* a
    /// kubeconfig everywhere except one field, whose value is a scalar where
    /// a mapping belongs. serde quotes that scalar; a kubeconfig's scalars
    /// are its credentials.
    #[tokio::test]
    async fn a_kubeconfig_whose_users_entry_is_a_scalar_never_echoes_it() {
        let document = format!(
            "apiVersion: v1\n\
             kind: Config\n\
             clusters: []\n\
             contexts: []\n\
             current-context: default\n\
             users: \"-----BEGIN EC PRIVATE KEY-----\\n{CANARY}\\n-----END EC PRIVATE KEY-----\"\n"
        );
        let (observation, captured) = observe_capturing_logs(document).await;
        assert_nothing_leaked("a kubeconfig whose `users` is a scalar", &observation, &captured);
    }

    /// A syntax error rather than a shape error: the `Parse` branch, which is
    /// a different `KubeconfigError` variant and therefore a different arm of
    /// the classifier.
    #[tokio::test]
    async fn unparsable_yaml_carrying_material_never_echoes_it() {
        let document = format!("not: [valid, -----BEGIN EC PRIVATE KEY----- {CANARY}");
        let (observation, captured) = observe_capturing_logs(document).await;
        assert_nothing_leaked("unparsable YAML", &observation, &captured);

        let ObservationOutcome::Failed(message) = observation.platform else { unreachable!() };
        assert_eq!(message, "Invalid kubeconfig format: it is not valid YAML");
    }

    /// The step after parsing: the document is a kubeconfig, but its fields
    /// do not resolve into a usable `Config`. `Config::from_custom_kubeconfig`
    /// returns a `KubeconfigError` too, and its `LoadContext`/
    /// `LoadClusterOfContext` variants interpolate names taken straight out
    /// of the document.
    #[tokio::test]
    async fn a_kubeconfig_whose_context_does_not_resolve_never_echoes_it() {
        let document = format!(
            "apiVersion: v1\n\
             kind: Config\n\
             clusters: []\n\
             contexts: []\n\
             current-context: \"{CANARY}\"\n\
             users: []\n"
        );
        let (observation, captured) = observe_capturing_logs(document).await;
        assert_nothing_leaked("a kubeconfig whose current-context does not resolve", &observation, &captured);

        let ObservationOutcome::Failed(message) = observation.platform else { unreachable!() };
        assert!(
            message.starts_with("Failed to load kubeconfig: "),
            "this must be the load step, not the parse step, or the test is not covering the \
             branch it claims to: {message}"
        );
    }

    /// D-CH-5: a cluster read that fails must classify, never echo. The
    /// canary is a token in the kubeconfig — the material this whole module
    /// exists to keep out of `cluster_status_message`.
    #[tokio::test]
    async fn a_failed_cluster_read_classifies_rather_than_echoing_the_kubeconfig() {
        const CANARY: &str = "CANARY-health-do-not-echo-me";
        // Bind then drop: the port is free, nothing listens, so the connect
        // is refused and both halves fail at the transport.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let document = format!(
            "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster:\n    \
             server: https://127.0.0.1:{port}\n    insecure-skip-tls-verify: true\n\
             contexts:\n- name: c\n  context:\n    cluster: c\n    user: u\n\
             current-context: c\nusers:\n- name: u\n  user:\n    token: {CANARY}\n"
        );
        let observation = observer()
            .observe(&SecretValue::from(document), "virtuozzo")
            .await;

        match observation.health {
            HealthOutcome::Failed(message) => {
                assert!(
                    !message.contains(CANARY),
                    "the kubeconfig's token must never reach the health message, got: {message}"
                );
                // Pin the branch: this must be the connect-stage failure
                // classification `check_health` actually produces, not (say)
                // a client-build failure that never reached it, or this
                // test is not covering the branch it claims to.
                assert!(
                    message.contains("the API server could not be reached"),
                    "expected the connect-failure classification from check_health's node \
                     list, got: {message}"
                );
            }
            HealthOutcome::Checked(_) => panic!("nothing listens on that port"),
            HealthOutcome::NotAttempted => {
                panic!("KubeObserver always attempts the read; NotAttempted is NoopObserver's answer");
            }
        }
    }

    /// A hermetic double for the API server: an in-process `tower::Service`
    /// handed straight to `kube::Client::new`, so `check_health` can be
    /// driven end to end (parsing, sorting, both control-plane label
    /// spellings, an absent `Ready` condition, and the namespace-list
    /// failure classification) with no real cluster and no network at all.
    ///
    /// `/api/v1/nodes` always answers with `nodes_body`; `/api/v1/namespaces`
    /// answers `namespaces_status` with `namespaces_body`. Anything else is a
    /// test bug, not a case `check_health` is meant to hit.
    fn stub_client(nodes_body: Vec<u8>, namespaces_status: u16, namespaces_body: Vec<u8>) -> Client {
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let nodes_body = nodes_body.clone();
            let namespaces_body = namespaces_body.clone();
            async move {
                let (status, body) = match request.uri().path() {
                    "/api/v1/nodes" => (200, nodes_body),
                    "/api/v1/namespaces" => (namespaces_status, namespaces_body),
                    other => panic!("stub API server got an unexpected request: {other}"),
                };
                let response = http::Response::builder()
                    .status(status)
                    .body(kube::client::Body::from(body))
                    .expect("building a stub HTTP response");
                Ok::<_, std::convert::Infallible>(response)
            }
        });
        Client::new(service, "default")
    }

    /// One `Ready` condition, the only field `check_health` reads off it
    /// besides `type_` (fixed to `"Ready"` by every caller here).
    fn ready_condition(status: &str) -> k8s_openapi::api::core::v1::NodeCondition {
        k8s_openapi::api::core::v1::NodeCondition {
            type_: "Ready".to_owned(),
            status: status.to_owned(),
            ..Default::default()
        }
    }

    fn labelled(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    /// Three nodes, deliberately out of name order, exercising every rule
    /// `check_health` applies in one pass: sorting, both spellings of the
    /// control-plane label, an absent `Ready` condition counting as not
    /// ready (not a panic and not vacuously ready), and a node with no
    /// `nodeInfo` at all (kubelet/OS fields must come back `None`, not
    /// empty strings).
    fn fixture_nodes() -> Vec<Node> {
        vec![
            Node {
                metadata: kube::core::ObjectMeta {
                    name: Some("z-worker".to_owned()),
                    ..Default::default()
                },
                spec: None,
                status: Some(k8s_openapi::api::core::v1::NodeStatus {
                    conditions: None,
                    node_info: None,
                    ..Default::default()
                }),
            },
            Node {
                metadata: kube::core::ObjectMeta {
                    name: Some("a-control-plane".to_owned()),
                    labels: Some(labelled(&[("node-role.kubernetes.io/control-plane", "")])),
                    ..Default::default()
                },
                spec: None,
                status: Some(k8s_openapi::api::core::v1::NodeStatus {
                    conditions: Some(vec![ready_condition("True")]),
                    node_info: Some(k8s_openapi::api::core::v1::NodeSystemInfo {
                        kubelet_version: "v1.30.1+k3s1".to_owned(),
                        os_image: "Ubuntu 24.04.3 LTS".to_owned(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            },
            Node {
                metadata: kube::core::ObjectMeta {
                    name: Some("m-master-legacy".to_owned()),
                    labels: Some(labelled(&[("node-role.kubernetes.io/master", "")])),
                    ..Default::default()
                },
                spec: None,
                status: Some(k8s_openapi::api::core::v1::NodeStatus {
                    conditions: Some(vec![ready_condition("False")]),
                    node_info: None,
                    ..Default::default()
                }),
            },
        ]
    }

    /// The canary this test exists for: not a kubeconfig secret, but the
    /// same class of leak (D-CH-5) on the other read `check_health` makes.
    /// A bare JSON string is a valid document but the wrong shape for
    /// `ObjectList<Namespace>` (the same C1 shape mismatch `errors.rs`
    /// measured for kubeconfig parsing), so the client-side deserialize
    /// fails and `serde`'s own error message quotes it whole — proving, the
    /// same way `errors.rs`'s tests do, that the raw upstream error really
    /// does carry this text before checking that `check_health`'s own log
    /// line does not.
    const NAMESPACE_LIST_CANARY: &str =
        "CANARY-namespace-list-parse-error-do-not-echo-raw-into-the-log";

    /// D-CH-5, the namespace half: `check_health` must still classify a
    /// failed namespace list through `describe_kube_error`, not format the
    /// underlying error, even though the failure only ever reaches a log
    /// line and never the published `HealthOutcome`. `describe_kube_error`
    /// returns `String`, not `errors.rs`'s `&'static str` — the same type
    /// `format!("{error}")` returns — so nothing at this call site stops an
    /// edit to `reason = %error` from compiling; only a test that proves the
    /// canary would leak through that edit catches it.
    #[tokio::test]
    async fn check_health_sorts_classifies_and_never_echoes_the_namespace_failure() {
        let node_list = kube::core::ObjectList {
            types: kube::core::TypeMeta::list::<Node>(),
            metadata: kube::core::ListMeta::default(),
            items: fixture_nodes(),
        };
        let nodes_body = serde_json::to_vec(&node_list).expect("serialising the fixture NodeList");
        let namespaces_body = format!("\"{NAMESPACE_LIST_CANARY}\"").into_bytes();
        let client = stub_client(nodes_body, 200, namespaces_body);

        let buffer = RawBuffer(Arc::new(Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let health = KubeObserver::check_health(&client).await.expect("the node list must succeed");
        drop(guard);
        let captured = String::from_utf8_lossy(&buffer.0.lock().unwrap()).into_owned();

        // Proof the upstream error really does carry the canary — the same
        // shape of proof `errors.rs` requires of its own C1 regression test,
        // so this test cannot pass by classifying an error that was
        // harmless anyway. (kube-client's own `Client::request` logs this
        // raw pair itself; that line is not under this module's control and
        // is not what the assertions below are about.)
        assert!(
            captured.contains(NAMESPACE_LIST_CANARY),
            "test bug: the fixture must actually reach a real deserialize failure that echoes \
             its input, or this test proves nothing"
        );

        // Sorting: alphabetical by name, not insertion order.
        let names: Vec<&str> = health.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["a-control-plane", "m-master-legacy", "z-worker"]);

        assert!(health.nodes[0].control_plane, "the `control-plane` label must be recognised");
        assert!(health.nodes[0].ready);
        assert_eq!(health.nodes[0].kubelet_version.as_deref(), Some("v1.30.1+k3s1"));
        assert_eq!(health.nodes[0].os_image.as_deref(), Some("Ubuntu 24.04.3 LTS"));

        assert!(health.nodes[1].control_plane, "the legacy `master` label must be recognised too");
        assert!(!health.nodes[1].ready, "a `False` Ready condition must not be ready");

        assert!(!health.nodes[2].control_plane);
        assert!(!health.nodes[2].ready, "an absent Ready condition must not be ready");
        assert_eq!(health.nodes[2].kubelet_version, None, "no nodeInfo means no kubelet version");
        assert_eq!(health.nodes[2].os_image, None, "no nodeInfo means no OS image");

        // The namespace branch: the point of this test.
        assert_eq!(
            health.namespace_count, None,
            "a failed namespace list must leave the count unknown, not zero"
        );

        let log_line = captured
            .lines()
            .find(|line| line.contains("namespace list failed"))
            .unwrap_or_else(|| {
                panic!("expected check_health's own debug log line for the namespace failure, got:\n{captured}")
            });
        assert!(
            !log_line.contains(NAMESPACE_LIST_CANARY),
            "check_health's own log line must never carry the namespace response verbatim, \
             got: {log_line}"
        );
        // `errors::OTHER_TRANSPORT_OR_CREDENTIAL_FAILURE` is module-private
        // (not even `pub(super)`), so this pins its exact wording rather
        // than importing the constant; `errors.rs`'s own
        // `a_transport_or_credential_failure_is_fixed_text` test pins the
        // same string from the other side.
        assert!(
            log_line.contains(
                "neither a TLS/certificate problem nor a plain connection failure"
            ),
            "the logged reason must be describe_kube_error's fixed classification, not a raw \
             formatting of the error (which would have carried the canary above), got: {log_line}"
        );
    }
}
