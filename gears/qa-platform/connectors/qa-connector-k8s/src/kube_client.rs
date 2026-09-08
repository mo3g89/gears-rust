//! Lifted from `qa-environments/src/infra/observer/kube_observer.rs`, which
//! stays in place and stays active until Task 19 removes it. This is a copy,
//! not a move: Phase C must not change `qa-environments`' behaviour, so both
//! paths exist side by side until the one-way door in Phase E.
//!
//! What changed in the copy: the VHP-specific half went the other way. The
//! `core-install-metadata` / `vp-gateway-hostnames` / `platformVersion` rules
//! are install topology, not Kubernetes mechanics, so they belong to the
//! product plugin and Task 9 takes them. What is left here is the mechanics
//! the original wrapped around them — build a client from kubeconfig
//! material, read a `ConfigMap`, read the nodes — expressed as a
//! [`KubeClient`] any cluster-targeting plugin can drive. Failures cross the
//! boundary as [`PluginFailure`] rather than `String`; see [`crate::errors`].
//!
//! A Kubernetes client built fresh from an environment's own stored
//! kubeconfig on every call.
//!
//! The original's header went on to claim the module mirrors legacy's
//! `detect_platform_version` and `detect_base_domain` line for line. That
//! claim is deliberately **not** carried over: those two functions are exactly
//! the VHP half that did not come here, so repeating it would leave a future
//! reader reasoning from a false premise about what this file does. It belongs
//! with `detect_platform_version` and `detect_base_domain` themselves, in the
//! VHP plugin's `detect.rs`.

use std::collections::BTreeMap;

use credstore_sdk::SecretValue;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Node};
use kube::api::{Api, ListParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use qa_product_sdk::observation::{FailureClass, HealthOutcome, HealthState, PluginFailure};

use crate::errors::{CLIENT_BUILD_FAILURE, NOT_UTF8, classify, classify_kubeconfig};

/// Fixed explanation for a `ConfigMap` the API server answered about and does
/// not have. Distinct from a *failed* read, which classifies through
/// [`classify`] and says why the read itself did not happen.
const CONFIGMAP_NOT_FOUND: &str =
    "the ConfigMap does not exist in that namespace (the read itself succeeded)";

/// One node, as the cluster reported it.
///
/// Lifted from `qa-environments/src/domain/observation.rs`' `NodeSummary`,
/// which stays there until that module is retired. Nothing here is a
/// `k8s_openapi` type, so a consumer can hold and test these without linking
/// Kubernetes itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSummary {
    pub name: String,
    pub control_plane: bool,
    pub ready: bool,
    pub kubelet_version: Option<String>,
    pub os_image: Option<String>,
}

/// One cluster read.
///
/// `namespace_count` is `Option`, not `u32`: legacy folds a failed namespace
/// list into `unwrap_or(0)`, a zero indistinguishable from a cluster that
/// really has no namespaces. `None` here means "not read".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterHealth {
    pub nodes: Vec<NodeSummary>,
    pub namespace_count: Option<u32>,
}

/// Legacy's four derivable statuses.
///
/// **No `Unreachable` variant on purpose.** Unreachable is not a property of a
/// cluster that was read; it is the absence of a reading, and it lives in
/// [`HealthOutcome::Failed`]. A variant here would be one
/// [`ClusterHealth::status`] can never return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Warning,
}

impl ClusterStatus {
    /// Legacy's spelling, kept verbatim: these four strings are what
    /// `qa_environments.cluster_status` holds today and what the UI's dot
    /// palette keys off.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "Healthy",
            Self::Degraded => "Degraded",
            Self::Unhealthy => "Unhealthy",
            Self::Warning => "Warning",
        }
    }

    /// The platform-wide vocabulary this status coarsens into.
    ///
    /// [`HealthState`] has three verdicts where legacy has four, because it
    /// has to say something true about a `SaaS` tenant and an appliance too.
    /// `Warning` — legacy's name for a cluster that answered and listed no
    /// nodes at all — maps to [`HealthState::Degraded`] rather than
    /// [`HealthState::Down`]: legacy deliberately separates it from
    /// `Unhealthy` (every node present and none ready), and it is *not*
    /// [`HealthState::Unknown`], which means nothing looked. Nothing is lost
    /// in the coarsening: [`Self::as_str`] rides along as
    /// [`HealthOutcome::Checked`]'s `detail`, which is `&'static str` and so
    /// can carry it safely.
    #[must_use]
    pub const fn health_state(self) -> HealthState {
        match self {
            Self::Healthy => HealthState::Ok,
            Self::Degraded | Self::Warning => HealthState::Degraded,
            Self::Unhealthy => HealthState::Down,
        }
    }
}

impl ClusterHealth {
    /// Legacy's derivation, verbatim (`manager/src/services/platforms.rs:210-226`).
    ///
    /// The `total == 0` arm must come first: with no nodes, `ready == total`
    /// is `0 == 0` and an empty cluster would otherwise report Healthy.
    #[must_use]
    pub fn status(&self) -> ClusterStatus {
        let total = self.nodes.len();
        let ready = self.nodes.iter().filter(|node| node.ready).count();
        if total == 0 {
            ClusterStatus::Warning
        } else if ready == total {
            ClusterStatus::Healthy
        } else if ready == 0 {
            ClusterStatus::Unhealthy
        } else {
            ClusterStatus::Degraded
        }
    }

    /// This read, in the platform's vocabulary.
    #[must_use]
    pub fn outcome(&self) -> HealthOutcome {
        let status = self.status();
        HealthOutcome::Checked {
            state: status.health_state(),
            detail: Some(status.as_str()),
        }
    }
}

/// One `ConfigMap`, reduced to the two facts a caller can use without linking
/// `k8s-openapi`: which namespace it was found in, and what it holds.
///
/// The namespace is not redundant with the one asked for:
/// [`KubeClient::scan_configmaps`] searches every namespace, and *which* one
/// answered is the fact a caller needs next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigMapData {
    pub namespace: String,
    pub data: BTreeMap<String, String>,
}

/// A Kubernetes label selector, in the syntax `ListParams::labels` accepts
/// (`key=value`, comma-separated).
///
/// A newtype rather than a bare `&str` for one reason:
/// [`KubeClient::scan_configmaps`] and [`KubeClient::find_configmap`] both take
/// a scope and a name, and two bare `&str` parameters can be transposed
/// silently — the exact string-coupling footgun this subsystem keeps paying
/// for. Wrapping the selector makes the transposition a compile error while
/// leaving both methods in the same `(scope, name)` shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelSelector<'a>(pub &'a str);

/// A Kubernetes API client, plus the reads a product plugin needs.
///
/// Cheap to clone in the sense that matters: `kube::Client` is itself an
/// `Arc`-backed handle, so a `KubeClient` holds one connection pool however
/// many reads go through it. Build one per observation cycle, not one per
/// read — one client and one TLS handshake serve every call below.
pub struct KubeClient {
    client: Client,
}

impl KubeClient {
    /// Build a client from an environment's stored kubeconfig bytes.
    ///
    /// # Errors
    ///
    /// Every failure here is a fact about the material itself (not valid
    /// UTF-8, not valid YAML, kubeconfig fields don't resolve to a usable
    /// config, the resulting config can't build a client), classified into a
    /// [`PluginFailure`].
    ///
    /// # Not one value in that failure is derived from the bytes
    ///
    /// The upstream errors are **classified** ([`crate::errors`]) and never
    /// formatted, because the value returned from this function is persisted,
    /// published on the environment DTO to every `qa.platform`
    /// GET/LIST-authorized caller, and rendered on the environment page — so
    /// a message carrying its input is the kubeconfig leaving the gear, which
    /// is the one property this whole crate rests on. That is not
    /// hypothetical: it was measured on 2026-08-28, with a PEM private key
    /// pasted into the create dialog's kubeconfig field coming back out
    /// through this exact path. See [`crate::errors`] for the rule and for
    /// why it is a classification rather than a filter.
    ///
    /// Nothing is logged here either, for the same reason and one more: the
    /// leak canary in this module's tests asserts against the raw `tracing`
    /// output, and a `debug!` carrying the parse error would be that leak
    /// with a different destination.
    pub async fn from_kubeconfig(kubeconfig: &SecretValue) -> Result<Self, PluginFailure> {
        let text = std::str::from_utf8(kubeconfig.as_bytes())
            .map_err(|_| PluginFailure::classified(FailureClass::Malformed, NOT_UTF8))?;
        let parsed = Kubeconfig::from_yaml(text).map_err(|error| classify_kubeconfig(&error))?;
        let config = Config::from_custom_kubeconfig(parsed, &KubeConfigOptions::default())
            .await
            .map_err(|error| classify_kubeconfig(&error))?;
        Client::try_from(config)
            .map(Self::from_client)
            .map_err(|_| PluginFailure::classified(FailureClass::Malformed, CLIENT_BUILD_FAILURE))
    }

    /// Wrap an already-built `kube::Client`.
    ///
    /// The seam an in-process `tower::Service` double is handed through, and
    /// the seam a caller that inferred its own config (see
    /// [`crate::secret_writer`]) comes back in through.
    #[must_use]
    pub const fn from_client(client: Client) -> Self {
        Self { client }
    }

    /// Look a `ConfigMap` up by name in one namespace.
    ///
    /// `Ok(None)` means the API server answered and does not have it — a
    /// distinct outcome from a failed read, and the one a caller with a
    /// fallback path branches on.
    ///
    /// # Errors
    ///
    /// The read itself failing, classified through [`classify`].
    pub async fn find_configmap(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<ConfigMapData>, PluginFailure> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        let found = api
            .get_opt(name)
            .await
            .map_err(|error| classify(&error))?
            .map(|config_map| ConfigMapData {
                namespace: config_map.metadata.namespace.unwrap_or_default(),
                data: config_map.data.unwrap_or_default(),
            });
        Ok(found)
    }

    /// One `ConfigMap`'s data, or a [`FailureClass::NotFound`] failure.
    ///
    /// [`Self::find_configmap`] is the same read for a caller that treats
    /// absence as a branch rather than a failure.
    ///
    /// # Errors
    ///
    /// The read failing, or succeeding and finding nothing.
    pub async fn read_configmap(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<BTreeMap<String, String>, PluginFailure> {
        self.find_configmap(namespace, name)
            .await?
            .map(|found| found.data)
            .ok_or(PluginFailure::classified(
                FailureClass::NotFound,
                CONFIGMAP_NOT_FOUND,
            ))
    }

    /// Every `ConfigMap` of this name, in any namespace, carrying `labels`.
    ///
    /// The all-namespace fallback the targeted lookup falls through to: a
    /// cluster whose install did not land in the namespace the caller
    /// expected is still findable by the label its installer stamped. Ordering
    /// is the API server's, unchanged — a caller that takes the first is
    /// responsible for saying so.
    ///
    /// # Errors
    ///
    /// The list failing, classified through [`classify`].
    pub async fn scan_configmaps(
        &self,
        labels: LabelSelector<'_>,
        name: &str,
    ) -> Result<Vec<ConfigMapData>, PluginFailure> {
        let api: Api<ConfigMap> = Api::all(self.client.clone());
        let list = api
            .list(
                &ListParams::default()
                    .labels(labels.0)
                    .fields(&format!("metadata.name={name}")),
            )
            .await
            .map_err(|error| classify(&error))?;
        Ok(list
            .items
            .into_iter()
            .map(|config_map| ConfigMapData {
                namespace: config_map.metadata.namespace.unwrap_or_default(),
                data: config_map.data.unwrap_or_default(),
            })
            .collect())
    }

    /// Read the cluster's nodes and namespaces.
    ///
    /// Two independent reads. The node list determines the status, so a
    /// failure there fails the whole health outcome; the namespace count is
    /// supplementary, so a failure there leaves it `None` rather than making a
    /// healthy cluster look unhealthy.
    ///
    /// Nodes are sorted by name, as legacy sorts them, so the rendered table
    /// is stable across ticks rather than reordering on every poll.
    ///
    /// # Errors
    ///
    /// The node list failing, classified through [`classify`].
    pub async fn read_cluster_health(&self) -> Result<ClusterHealth, PluginFailure> {
        let nodes_api: Api<Node> = Api::all(self.client.clone());
        let namespaces_api: Api<Namespace> = Api::all(self.client.clone());

        let items = nodes_api
            .list(&ListParams::default())
            .await
            .map_err(|error| classify(&error))?;

        let mut nodes: Vec<NodeSummary> = items.items.into_iter().map(node_summary).collect();
        nodes.sort_by(|a, b| a.name.cmp(&b.name));

        let namespace_count = match namespaces_api.list(&ListParams::default()).await {
            Ok(list) => u32::try_from(list.items.len()).ok(),
            Err(error) => {
                // Classified, not formatted, even though this one only reaches a
                // log line: the rule is the rule everywhere in this crate.
                tracing::debug!(
                    reason = %classify(&error),
                    "namespace list failed; the count is left unknown"
                );
                None
            }
        };

        Ok(ClusterHealth {
            nodes,
            namespace_count,
        })
    }

    /// The cluster's health as the platform expresses it.
    ///
    /// A **value**, never an error: a failed read is
    /// [`HealthOutcome::Failed`], which is persisted and shown, because an
    /// operator who can see why it failed can fix it and one who sees a blank
    /// page cannot. [`Self::read_cluster_health`] is the same read for a
    /// caller that also wants the node facts.
    pub async fn node_health(&self) -> HealthOutcome {
        match self.read_cluster_health().await {
            Ok(health) => health.outcome(),
            Err(failure) => HealthOutcome::Failed(failure),
        }
    }
}

/// One `Node`, reduced to the five facts the health rules read off it.
fn node_summary(node: Node) -> NodeSummary {
    let labels = node.metadata.labels.unwrap_or_default();
    // k3s sets both; legacy checks both.
    let control_plane = labels.contains_key("node-role.kubernetes.io/control-plane")
        || labels.contains_key("node-role.kubernetes.io/master");
    let status = node.status.as_ref();
    // Legacy's `unwrap_or(false)`: an absent Ready condition is not ready. A
    // node that does not say it is ready is not ready.
    let ready = status
        .and_then(|s| s.conditions.as_ref())
        .and_then(|c| c.iter().find(|c| c.type_ == "Ready"))
        .is_some_and(|c| c.status == "True");
    let info = status.and_then(|s| s.node_info.as_ref());
    NodeSummary {
        name: node
            .metadata
            .name
            .unwrap_or_else(|| "unknown-node".to_owned()),
        control_plane,
        ready,
        kubelet_version: info.map(|i| i.kubelet_version.clone()),
        os_image: info.map(|i| i.os_image.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::test_support::{
        RawBuffer, StubConfigMap, StubRequest, kubeconfig_for, not_found_body,
    };

    fn node(name: &str, control_plane: bool, ready: bool) -> NodeSummary {
        NodeSummary {
            name: name.to_owned(),
            control_plane,
            ready,
            kubelet_version: None,
            os_image: None,
        }
    }

    fn health(nodes: Vec<NodeSummary>) -> ClusterHealth {
        ClusterHealth {
            nodes,
            namespace_count: Some(14),
        }
    }

    /// The real `sv-test` shape, measured 2026-08-28: a single-node k3s whose
    /// one node is control-plane.
    #[test]
    fn a_single_control_plane_node_is_healthy_with_no_workers() {
        let h = health(vec![node("sv-vhp-jele-io", true, true)]);
        assert_eq!(h.status(), ClusterStatus::Healthy);
        assert_eq!(
            h.outcome(),
            HealthOutcome::Checked {
                state: HealthState::Ok,
                detail: Some("Healthy"),
            }
        );
    }

    /// The boundary a naive `ready == total` gets wrong: zero equals zero, so
    /// an empty cluster would report Healthy. Legacy checks it first, and so
    /// must this.
    #[test]
    fn a_cluster_with_no_nodes_is_warning_not_healthy() {
        let h = health(vec![]);
        assert_eq!(h.status(), ClusterStatus::Warning);
        assert_eq!(
            h.outcome(),
            HealthOutcome::Checked {
                state: HealthState::Degraded,
                detail: Some("Warning"),
            },
            "a cluster that answered and listed no nodes is not Unknown - something looked"
        );
    }

    #[test]
    fn no_node_ready_is_unhealthy() {
        let h = health(vec![node("a", true, false), node("b", false, false)]);
        assert_eq!(h.status(), ClusterStatus::Unhealthy);
        assert_eq!(
            h.outcome(),
            HealthOutcome::Checked {
                state: HealthState::Down,
                detail: Some("Unhealthy"),
            }
        );
    }

    #[test]
    fn some_nodes_ready_is_degraded() {
        let h = health(vec![node("a", true, true), node("b", false, false)]);
        assert_eq!(h.status(), ClusterStatus::Degraded);
        assert_eq!(
            h.outcome(),
            HealthOutcome::Checked {
                state: HealthState::Degraded,
                detail: Some("Degraded"),
            }
        );
    }

    /// The stored string is a wire value: a rename here silently reclassifies
    /// every row already in the database.
    #[test]
    fn the_status_strings_are_the_stored_wire_values() {
        assert_eq!(ClusterStatus::Healthy.as_str(), "Healthy");
        assert_eq!(ClusterStatus::Degraded.as_str(), "Degraded");
        assert_eq!(ClusterStatus::Unhealthy.as_str(), "Unhealthy");
        assert_eq!(ClusterStatus::Warning.as_str(), "Warning");
    }

    #[tokio::test]
    async fn a_malformed_kubeconfig_is_a_failure_naming_the_defect() {
        let failure = KubeClient::from_kubeconfig(&SecretValue::from(vec![0xFF, 0xFE]))
            .await
            .err()
            .expect("invalid bytes must never build a client");
        assert_eq!(failure.detail, Some("kubeconfig is not valid UTF-8"));
        assert_eq!(failure.class, FailureClass::Malformed);
    }

    #[tokio::test]
    async fn unparsable_yaml_is_a_failure_not_a_panic() {
        let failure =
            KubeClient::from_kubeconfig(&SecretValue::from("not: [valid, kubeconfig".to_owned()))
                .await
                .err()
                .expect("malformed YAML must never build a client");
        assert_eq!(failure.detail, Some("it is not valid YAML"));
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

    /// Drive a `document` all the way through what the original `observe`
    /// drove — build a client, then read the cluster with it — with every
    /// `tracing` byte captured, and return both halves of what could leak:
    /// the [`PluginFailure`] that is persisted and published, and the raw log.
    ///
    /// Both stages, not just the first, because the fixtures below split
    /// across them: a malformed document fails in
    /// [`KubeClient::from_kubeconfig`], while a well-formed one pointing at
    /// nothing gets a client and fails in the read. Either way the material
    /// has been handled and either way a failure is published, so both are
    /// surfaces this has to check.
    ///
    /// A thread-local default subscriber, not `with_default`'s sync-closure
    /// form: the calls are `async` and must stay under this subscriber across
    /// every `.await` point. `#[tokio::test]` defaults to a current-thread
    /// runtime, so the whole call runs on this one thread and the guard sees
    /// everything it logs.
    async fn observe_capturing_logs(document: String) -> (PluginFailure, String) {
        let buffer = RawBuffer::new();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .finish();

        let guard = tracing::subscriber::set_default(subscriber);
        let outcome = match KubeClient::from_kubeconfig(&SecretValue::from(document)).await {
            Ok(client) => client.node_health().await,
            Err(failure) => HealthOutcome::Failed(failure),
        };
        drop(guard);

        let captured = buffer.captured();
        let HealthOutcome::Failed(failure) = outcome else {
            panic!("every fixture here points at nothing and must fail")
        };
        (failure, captured)
    }

    /// Assert the surfaces a kubeconfig can escape through — the
    /// persisted/published failure text, its `Debug` rendering, and the log —
    /// carry neither the canary nor any PEM marker.
    ///
    /// `Debug` is checked as well as `Display` because `PluginFailure` derives
    /// it and a `Debug`-formatted failure is exactly what a `tracing` field
    /// or an `anyhow` chain renders.
    fn assert_nothing_leaked(label: &str, failure: &PluginFailure, captured: &str) {
        for (surface, text) in [
            ("Display", failure.to_string()),
            ("Debug", format!("{failure:?}")),
        ] {
            assert!(
                !text.contains(CANARY),
                "{label}: the {surface} rendering carries the material, and that rendering \
                 is a published surface:\n{text}"
            );
            assert!(
                !text.contains("BEGIN"),
                "{label}: the {surface} rendering carries PEM material:\n{text}"
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
        let (failure, captured) = observe_capturing_logs(fixture_kubeconfig()).await;
        assert_nothing_leaked(
            "well-formed kubeconfig, unreachable server",
            &failure,
            &captured,
        );
    }

    /// **The C1 regression test.** This is the branch the canary above never
    /// reached: its fixture is well-formed YAML, so `Kubeconfig::from_yaml`'s
    /// error path was never under test. Measured on 2026-08-28, before the
    /// fix, this produced
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
        let document =
            format!("-----BEGIN EC PRIVATE KEY-----\n{CANARY}\n-----END EC PRIVATE KEY-----\n");
        let (failure, captured) = observe_capturing_logs(document).await;
        assert_nothing_leaked(
            "a PEM private key pasted as a kubeconfig",
            &failure,
            &captured,
        );

        assert_eq!(
            failure.detail,
            Some(
                "it is valid YAML but does not have a kubeconfig's shape - check that what was \
                 stored is a kubeconfig file (`apiVersion: v1`, `kind: Config`, with `clusters`, \
                 `contexts` and `users`) and not a certificate, a private key, or some other YAML"
            ),
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
        let (failure, captured) = observe_capturing_logs(document).await;
        assert_nothing_leaked(
            "a kubeconfig whose `users` is a scalar",
            &failure,
            &captured,
        );
    }

    /// A syntax error rather than a shape error: the `Parse` branch, which is
    /// a different `KubeconfigError` variant and therefore a different arm of
    /// the classifier.
    #[tokio::test]
    async fn unparsable_yaml_carrying_material_never_echoes_it() {
        let document = format!("not: [valid, -----BEGIN EC PRIVATE KEY----- {CANARY}");
        let (failure, captured) = observe_capturing_logs(document).await;
        assert_nothing_leaked("unparsable YAML", &failure, &captured);
        assert_eq!(failure.detail, Some("it is not valid YAML"));
    }

    /// The step after parsing: the document is a kubeconfig, but its fields
    /// do not resolve into a usable `Config`. `Config::from_custom_kubeconfig`
    /// returns a `KubeconfigError` too, and its `LoadContext`/
    /// `LoadClusterOfContext` variants interpolate names taken straight out
    /// of the document.
    ///
    /// The original pinned "this is the load step, not the parse step" on a
    /// `"Failed to load kubeconfig: "` prefix that no longer exists — a
    /// `PluginFailure`'s `detail` is `&'static str`, so a per-call-site prefix
    /// cannot be assembled at runtime. The variant's own fixed text is a
    /// sharper pin anyway: only the load step can produce `LoadContext`.
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
        let (failure, captured) = observe_capturing_logs(document).await;
        assert_nothing_leaked(
            "a kubeconfig whose current-context does not resolve",
            &failure,
            &captured,
        );
        assert_eq!(
            failure.detail,
            Some("its `current-context` does not name any context the document defines"),
            "this must be the load step, not the parse step, or the test is not covering the \
             branch it claims to"
        );
    }

    /// D-CH-5: a cluster read that fails must classify, never echo. The
    /// canary is a token in the kubeconfig — the material this whole crate
    /// exists to keep out of a published health message.
    #[tokio::test]
    async fn a_failed_cluster_read_classifies_rather_than_echoing_the_kubeconfig() {
        const CANARY: &str = "CANARY-health-do-not-echo-me";
        // Bind then drop: the port is free, nothing listens, so the connect
        // is refused and the read fails at the transport.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let document = format!(
            "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster:\n    \
             server: https://127.0.0.1:{port}\n    insecure-skip-tls-verify: true\n\
             contexts:\n- name: c\n  context:\n    cluster: c\n    user: u\n\
             current-context: c\nusers:\n- name: u\n  user:\n    token: {CANARY}\n"
        );
        let client = KubeClient::from_kubeconfig(&SecretValue::from(document))
            .await
            .expect("this kubeconfig is well-formed; only the cluster is absent");

        match client.node_health().await {
            HealthOutcome::Failed(failure) => {
                let message = failure.to_string();
                assert!(
                    !message.contains(CANARY),
                    "the kubeconfig's token must never reach the health message, got: {message}"
                );
                // Pin the branch: this must be the connect-stage failure
                // classification `read_cluster_health` actually produces, not
                // (say) a client-build failure that never reached it, or this
                // test is not covering the branch it claims to.
                assert!(
                    message.contains("the API server could not be reached"),
                    "expected the connect-failure classification from the node list, got: \
                     {message}"
                );
                assert_eq!(failure.class, FailureClass::Unreachable);
            }
            HealthOutcome::Checked { .. } => panic!("nothing listens on that port"),
            HealthOutcome::NotAttempted => {
                panic!("node_health always attempts the read")
            }
        }
    }

    /// A hermetic double for the API server, for the two reads
    /// `read_cluster_health` makes: `/api/v1/nodes` always answers with
    /// `nodes_body`, `/api/v1/namespaces` answers `namespaces_status` with
    /// `namespaces_body`, and anything else is a test bug rather than a case
    /// `read_cluster_health` is meant to hit.
    ///
    /// The `tower::Service` this used to build by hand now lives in
    /// [`crate::test_support`], because the `ConfigMap` reads had to be
    /// drivable from `qa-vhp-product-plugin` too and ADR-0001 stops that crate
    /// from building one. This is the same double, narrowed to two routes.
    fn stub_client(
        nodes_body: Vec<u8>,
        namespaces_status: u16,
        namespaces_body: Vec<u8>,
    ) -> KubeClient {
        KubeClient::from_routes(move |request| match request.path.as_str() {
            "/api/v1/nodes" => (200, nodes_body.clone()),
            "/api/v1/namespaces" => (namespaces_status, namespaces_body.clone()),
            other => panic!("stub API server got an unexpected request: {other}"),
        })
    }

    /// One `Ready` condition, the only field the node rules read off it
    /// besides `type_` (fixed to `"Ready"` by every caller here).
    fn ready_condition(status: &str) -> k8s_openapi::api::core::v1::NodeCondition {
        k8s_openapi::api::core::v1::NodeCondition {
            type_: "Ready".to_owned(),
            status: status.to_owned(),
            ..Default::default()
        }
    }

    fn labelled(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// Three nodes, deliberately out of name order, exercising every rule
    /// `read_cluster_health` applies in one pass: sorting, both spellings of
    /// the control-plane label, an absent `Ready` condition counting as not
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
    /// same class of leak (D-CH-5) on the other read `read_cluster_health`
    /// makes. A bare JSON string is a valid document but the wrong shape for
    /// `ObjectList<Namespace>` (the same C1 shape mismatch `errors.rs`
    /// measured for kubeconfig parsing), so the client-side deserialize
    /// fails and `serde`'s own error message quotes it whole — proving, the
    /// same way `errors.rs`'s tests do, that the raw upstream error really
    /// does carry this text before checking that this module's own log line
    /// does not.
    const NAMESPACE_LIST_CANARY: &str =
        "CANARY-namespace-list-parse-error-do-not-echo-raw-into-the-log";

    /// D-CH-5, the namespace half: `read_cluster_health` must still classify a
    /// failed namespace list through [`classify`], not format the underlying
    /// error, even though the failure only ever reaches a log line and never
    /// the published [`HealthOutcome`]. Nothing at that call site stops an
    /// edit to `reason = %error` from compiling; only a test that proves the
    /// canary would leak through that edit catches it.
    #[tokio::test]
    async fn cluster_health_sorts_classifies_and_never_echoes_the_namespace_failure() {
        let node_list = kube::core::ObjectList {
            types: kube::core::TypeMeta::list::<Node>(),
            metadata: kube::core::ListMeta::default(),
            items: fixture_nodes(),
        };
        let nodes_body = serde_json::to_vec(&node_list).expect("serialising the fixture NodeList");
        let namespaces_body = format!("\"{NAMESPACE_LIST_CANARY}\"").into_bytes();
        let client = stub_client(nodes_body, 200, namespaces_body);

        let buffer = RawBuffer::new();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let health = client
            .read_cluster_health()
            .await
            .expect("the node list must succeed");
        drop(guard);
        let captured = buffer.captured();

        // Proof the upstream error really does carry the canary — the same
        // shape of proof `errors.rs` requires of its own C1 regression test,
        // so this test cannot pass by classifying an error that was
        // harmless anyway. (kube-client's own `Client::request` logs this
        // raw pair itself; that line is not under this crate's control and
        // is not what the assertions below are about.)
        assert!(
            captured.contains(NAMESPACE_LIST_CANARY),
            "test bug: the fixture must actually reach a real deserialize failure that echoes \
             its input, or this test proves nothing"
        );

        // Sorting: alphabetical by name, not insertion order.
        let names: Vec<&str> = health.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a-control-plane", "m-master-legacy", "z-worker"]
        );

        assert!(
            health.nodes[0].control_plane,
            "the `control-plane` label must be recognised"
        );
        assert!(health.nodes[0].ready);
        assert_eq!(
            health.nodes[0].kubelet_version.as_deref(),
            Some("v1.30.1+k3s1")
        );
        assert_eq!(
            health.nodes[0].os_image.as_deref(),
            Some("Ubuntu 24.04.3 LTS")
        );

        assert!(
            health.nodes[1].control_plane,
            "the legacy `master` label must be recognised too"
        );
        assert!(
            !health.nodes[1].ready,
            "a `False` Ready condition must not be ready"
        );

        assert!(!health.nodes[2].control_plane);
        assert!(
            !health.nodes[2].ready,
            "an absent Ready condition must not be ready"
        );
        assert_eq!(
            health.nodes[2].kubelet_version, None,
            "no nodeInfo means no kubelet version"
        );
        assert_eq!(
            health.nodes[2].os_image, None,
            "no nodeInfo means no OS image"
        );

        // The namespace branch: the point of this test.
        assert_eq!(
            health.namespace_count, None,
            "a failed namespace list must leave the count unknown, not zero"
        );

        let log_line = captured
            .lines()
            .find(|line| line.contains("namespace list failed"))
            .unwrap_or_else(|| {
                panic!("expected this module's own debug log line for the namespace failure, got:\n{captured}")
            });
        assert!(
            !log_line.contains(NAMESPACE_LIST_CANARY),
            "the log line must never carry the namespace response verbatim, got: {log_line}"
        );
        // `errors::OTHER_TRANSPORT_OR_CREDENTIAL_FAILURE` is module-private,
        // so this pins its exact wording rather than importing the constant;
        // `errors.rs`'s own `a_transport_or_credential_failure_is_fixed_text`
        // test pins the same string from the other side.
        assert!(
            log_line.contains("neither a TLS/certificate problem nor a plain connection failure"),
            "the logged reason must be the classifier's fixed text, not a raw formatting of the \
             error (which would have carried the canary above), got: {log_line}"
        );
    }

    // ── the three `ConfigMap` reads ───────────────────────────────────────
    //
    // These had **zero** coverage workspace-wide until this section: the stub
    // above routes only the two node/namespace paths and panics on anything
    // else, and `qa-vhp-product-plugin` — the only caller of `find_configmap`
    // and `scan_configmaps` — cannot build a double at all under ADR-0001.
    // What each read *sends* is asserted as well as what it returns: the
    // selector pair below is the one legacy sent, and nothing about it is
    // recoverable from the returned value.

    /// The vpadm label legacy scans by, and the one
    /// `qa-vhp-product-plugin`'s `observe` passes. Spelled out here rather
    /// than imported because this crate must not learn a product's labels;
    /// it is a fixture, not a constant.
    const VPADM_MANAGED_BY: &str = "app.kubernetes.io/managed-by=vpadm";

    /// A client whose answers come from `answer`, plus the log of everything
    /// it was asked.
    fn recording_client(
        answer: impl Fn(&StubRequest) -> (u16, Vec<u8>) + Send + Sync + 'static,
    ) -> (KubeClient, Arc<Mutex<Vec<StubRequest>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let client = KubeClient::from_routes(move |request| {
            recorder.lock().unwrap().push(request.clone());
            answer(request)
        });
        (client, seen)
    }

    fn only_request(seen: &Arc<Mutex<Vec<StubRequest>>>) -> StubRequest {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "expected exactly one request, got: {seen:?}");
        seen[0].clone()
    }

    /// The targeted read: one named object in one namespace, and the
    /// namespace the *answer* carried (not the one asked for — those differ
    /// on the scan path, and `ConfigMapData` exists to keep them apart).
    #[tokio::test]
    async fn a_targeted_configmap_read_asks_for_that_name_in_that_namespace() {
        let body = StubConfigMap::new("virtuozzo", "core-install-metadata")
            .with("platformVersion", "26.5.0")
            .body();
        let (client, seen) = recording_client(move |_| (200, body.clone()));

        let found = client
            .find_configmap("virtuozzo", "core-install-metadata")
            .await
            .expect("the stub answered 200")
            .expect("with the object itself");

        let request = only_request(&seen);
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.path,
            "/api/v1/namespaces/virtuozzo/configmaps/core-install-metadata"
        );
        assert_eq!(found.namespace, "virtuozzo");
        assert_eq!(
            found.data.get("platformVersion").map(String::as_str),
            Some("26.5.0")
        );
    }

    /// The distinction the whole `Option` exists for: the API server answered,
    /// and does not have it. A caller with a fallback branches on this, so
    /// folding it into the error path would silently delete that fallback.
    #[tokio::test]
    async fn a_configmap_the_server_does_not_have_is_absence_not_failure() {
        let (client, _) = recording_client(|_| {
            (
                404,
                not_found_body("configmaps \"core-install-metadata\" not found"),
            )
        });

        let found = client
            .find_configmap("virtuozzo", "core-install-metadata")
            .await
            .expect("a 404 is an answer, not a failed read");

        assert_eq!(found, None);
    }

    /// The same read for a caller that treats absence as a failure. Both
    /// halves are asserted in one test because the pair *is* the behaviour:
    /// present yields the data with no wrapper, absent yields the fixed
    /// `NotFound` — and the fixed text says the read itself succeeded, which
    /// is what tells an operator to look at the cluster rather than at the
    /// kubeconfig.
    #[tokio::test]
    async fn read_configmap_turns_absence_into_a_not_found_failure() {
        let body = StubConfigMap::new("virtuozzo", "core-install-metadata")
            .with("platformVersion", "26.5.0")
            .body();
        let (present, _) = recording_client(move |_| (200, body.clone()));
        assert_eq!(
            present
                .read_configmap("virtuozzo", "core-install-metadata")
                .await
                .expect("the stub answered 200")
                .get("platformVersion")
                .map(String::as_str),
            Some("26.5.0")
        );

        let (absent, _) = recording_client(|_| (404, not_found_body("not found")));
        let failure = absent
            .read_configmap("virtuozzo", "core-install-metadata")
            .await
            .expect_err("absence is a failure for this caller");

        assert_eq!(failure.class, FailureClass::NotFound);
        assert_eq!(failure.detail, Some(CONFIGMAP_NOT_FOUND));
        assert_eq!(
            failure.remote_message, None,
            "the read succeeded, so there is no remote refusal to carry"
        );
    }

    /// The scan's two selectors, which are the only part of it a returned
    /// value cannot show. Legacy sent this exact pair — the vpadm label plus
    /// `metadata.name` — and a scan that dropped either would find the wrong
    /// `ConfigMap`s in a cluster running more than one product.
    #[tokio::test]
    async fn the_scan_sends_legacys_label_and_field_selector_pair() {
        let (client, seen) = recording_client(|_| (200, StubConfigMap::list_body(&[])));

        let found = client
            .scan_configmaps(LabelSelector(VPADM_MANAGED_BY), "core-install-metadata")
            .await
            .expect("the stub answered 200");

        assert!(found.is_empty());
        let request = only_request(&seen);
        assert_eq!(
            request.path, "/api/v1/configmaps",
            "the scan is deliberately cluster-wide, not namespaced"
        );
        // Compared against the percent-encoded forms the client actually put
        // on the wire, not against the plain strings: asserting on the decoded
        // value would pass even if the encoding were wrong, and the API server
        // reads what was sent. Whole-value equality rather than `contains`,
        // because a substring match is satisfied by a prefix and would miss a
        // selector that went out with something appended to it.
        assert_eq!(
            request.query_param("labelSelector"),
            Some("app.kubernetes.io%2Fmanaged-by%3Dvpadm"),
            "the caller's label selector must reach the wire unchanged, got: {:?}",
            request.query
        );
        assert_eq!(
            request.query_param("fieldSelector"),
            Some("metadata.name%3Dcore-install-metadata"),
            "the name field selector must reach the wire unchanged, got: {:?}",
            request.query
        );
    }

    /// What the scan returns, and the field that makes it useful: *which*
    /// namespace answered. An object the API server reports with no namespace
    /// comes back as an empty one rather than panicking — the shape
    /// `qa-vhp-product-plugin` records as `unknown`.
    #[tokio::test]
    async fn the_scan_reports_the_namespace_each_match_came_from() {
        let body = StubConfigMap::list_body(&[
            StubConfigMap::new("virtuozzo", "core-install-metadata")
                .with("platformVersion", "26.5.0"),
            StubConfigMap::new("", "core-install-metadata")
                .without_namespace()
                .with("platformVersion", "26.4.1"),
        ]);
        let (client, _) = recording_client(move |_| (200, body.clone()));

        let found = client
            .scan_configmaps(LabelSelector(VPADM_MANAGED_BY), "core-install-metadata")
            .await
            .expect("the stub answered 200");

        assert_eq!(found.len(), 2);
        assert_eq!(found[0].namespace, "virtuozzo");
        assert_eq!(
            found[1].namespace, "",
            "an object the server reports with no namespace must not panic the read"
        );
        assert_eq!(
            found[1].data.get("platformVersion").map(String::as_str),
            Some("26.4.1")
        );
    }

    /// D-CH-5 on the `ConfigMap` reads: a read that *fails* must classify
    /// through [`classify`], never format. A real refused connection rather
    /// than a synthetic error, for the same reason `errors` insists on one —
    /// `hyper_util::client::legacy::Error` has no public constructor, so the
    /// only genuine `is_connect` failure is a connection that really failed
    /// that way, and the classifier's downcast is what is under test here.
    #[tokio::test]
    async fn a_failed_configmap_read_classifies_rather_than_echoing_the_kubeconfig() {
        const CANARY: &str = "CANARY-configmap-read-do-not-echo-me";
        // Bind then drop: the port is free, nothing listens, so the connect
        // is refused and the read fails at the transport.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let client = KubeClient::from_kubeconfig(&SecretValue::from(kubeconfig_for(
            &format!("https://127.0.0.1:{port}"),
            CANARY,
        )))
        .await
        .expect("this kubeconfig is well-formed; only the cluster is absent");

        for failure in [
            client
                .find_configmap("virtuozzo", "core-install-metadata")
                .await
                .expect_err("nothing listens on that port"),
            client
                .scan_configmaps(LabelSelector(VPADM_MANAGED_BY), "core-install-metadata")
                .await
                .expect_err("nothing listens on that port"),
        ] {
            let message = failure.to_string();
            assert!(
                !message.contains(CANARY),
                "the kubeconfig's token must never reach a read failure, got: {message}"
            );
            assert!(
                message.contains("the API server could not be reached"),
                "expected the connect-failure classification, got: {message}"
            );
            assert_eq!(failure.class, FailureClass::Unreachable);
        }
    }
}
