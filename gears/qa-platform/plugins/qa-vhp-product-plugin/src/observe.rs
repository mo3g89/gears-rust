//! Reading a live VHP install: what version vpadm put on the cluster, what
//! base URL it exposed, and whether the cluster is healthy — in one client and
//! one handshake.
//!
//! Mirrors `manager/src/services/platforms.rs:1037-1149`
//! (`detect_platform_version` and `detect_base_domain`) including its error
//! wording: those strings are persisted and shown to an operator, and
//! "namespaces virtuozzo not found" is what makes a broken environment fixable
//! rather than merely broken. [`crate::detect`] carries the other half of that
//! paragraph — the pure rules; this file is the reads around them and the only
//! place in the crate that produces an error at all.
//!
//! The orchestration is `qa-environments/src/infra/observer/kube_observer.rs`'
//! `observe`, `detect` and `detect_base_domain`, rewritten against
//! [`qa_plugin_k8s::KubeClient`] instead of a bare `kube::Client`. That module
//! stays in place and stays working until Task 19 removes it; this is a copy,
//! not a move, because Phase C must not change `qa-environments`' behaviour.
//!
//! # What changed in the copy, and exactly what it costs
//!
//! Only the error channel — but the cost is larger than one word, so it is
//! itemised rather than summarised. Legacy assembled a `String` per failure and
//! persisted it, published it on the environment DTO, and rendered it. It had
//! **two distinct** sentences for the two reads:
//!
//! ```text
//! Failed to read core-install-metadata in namespace {ns}: {classified}
//! Failed to list core-install-metadata: {classified}
//! ```
//!
//! Here a failure is a [`PluginFailure`] whose `detail` is `&'static str`, and
//! both `?` sites in this file's `detect` surface whatever [`qa_plugin_k8s::classify`]
//! chose. Three things an operator used to read are therefore gone from the
//! persisted message:
//!
//! * **which of the two reads failed** — the targeted namespaced `get` or the
//!   all-namespace `list`. The two sentences have collapsed into one.
//! * **the namespace** the targeted read used.
//! * **the resource name** on a transport failure. A `kube::Error::Api` still
//!   carries the API server's own `Status.message` in `remote_message`, which
//!   normally names the resource; a connect or TLS failure never reached the
//!   server and so names nothing.
//!
//! The first two are **recovered on the log line**: each `?` site in `detect`
//! emits a `tracing::warn!` naming which read failed, with the namespace as a
//! field on the targeted one, before propagating the classified failure
//! untouched. That puts the two facts on the surface that should carry them
//! and costs the persisted message nothing. The third is not recovered, and
//! deliberately: putting the resource name back would mean formatting it into
//! a classified string, which is the rule this crate exists to keep.
//!
//! # Why the two reads do not just get their own fixed details
//!
//! Because that trade is not the cheap one it looks like. [`qa_plugin_k8s::classify`]
//! maps every connect failure, every TLS/certificate failure and every other
//! transport failure to the *same* [`FailureClass::Unreachable`]; what tells
//! them apart is only the fixed `detail` text it picked. `qa-plugin-k8s`'
//! `errors` module says so in its own header — the three-way split "lives where
//! it was always read: in the fixed `detail` text". Overwriting `detail` here
//! with "the targeted read failed" would buy back one fact and destroy three,
//! turning "the API server's certificate did not verify" into a failure with no
//! stated cause. `detail` holds one string and the more diagnostic one wins.
//!
//! The recovered half is that `classify` chose the sentence, so a *classified*
//! reason and the API server's own words both still reach the page — the
//! latter in `remote_message`, the one sanctioned carrier.
//!
//! # Version detection and health fail independently
//!
//! Two outcomes, not one. A kubeconfig scoped to a single namespace reads
//! `core-install-metadata` perfectly and is forbidden from listing nodes;
//! folding the node failure into the version half would report that
//! environment as having no version, which is false. This is the rule
//! `qa-environments`' `ports/platform_observer.rs` states on `HealthOutcome`
//! and the reason [`qa_product_sdk::observation::HealthOutcome`] is a separate
//! channel from [`ObservationOutcome`] at all. The one thing that *does* fail
//! both halves is a client that could never be built: there was no reading of
//! either kind to report.

use qa_plugin_k8s::{KubeClient, LabelSelector};
use qa_product_sdk::observation::{
    FailureClass, HealthOutcome, ObservationOutcome, ObservedAttrs, PluginFailure,
    PluginObservation,
};
use qa_product_sdk::plugin::EnvironmentHandle;

use crate::detect::{DetectedPlatform, compute_base_domain, detected_from_data, external_hosts};
use crate::schemas::{
    BASE_DOMAIN_KEY, BUILD_KEY, DEFAULT_VPADM_NAMESPACE, EXTERNAL_HOSTS_KEY, KUBECONFIG_KEY,
    NAMESPACE_KEY, PLATFORM_VERSION_KEY, RAW_VERSION_KEY, VPADM_NAMESPACE_KEY,
};

/// The `ConfigMap` vpadm writes at install time, carrying `platformVersion`.
const CORE_INSTALL_METADATA: &str = "core-install-metadata";

/// The `ConfigMap` vpadm writes with each component's resolved hostname, used
/// for best-effort base-domain detection.
const GATEWAY_HOSTNAMES_CONFIGMAP: &str = "vp-gateway-hostnames";

/// The label vpadm stamps on everything it installs — the all-namespace
/// fallback's only handle on an install that did not land where it was
/// expected.
const VPADM_MANAGED_BY: &str = "app.kubernetes.io/managed-by=vpadm";

/// The namespace recorded when the all-namespace scan found the metadata but
/// the API server reported no namespace on the object. Legacy's literal.
const UNKNOWN_NAMESPACE: &str = "unknown";

/// The separator joining externally-visible hostnames into one `Text` value.
const HOSTS_SEPARATOR: &str = ", ";

/// The scheme every VHP base URL is built with, matching
/// `qa-environments/src/infra/storage/environments_sea_repo.rs`' own
/// `format!("https://{domain}")` — the value `vhp_base_url` holds today and
/// which `observed_base_url` must be a drop-in successor to.
const BASE_URL_SCHEME: &str = "https://";

/// Fixed failure for an environment whose kubeconfig this caller did not
/// resolve. `observe` is the one method whose caller is expected to have
/// resolved the slots it needs, so this is a caller defect, not the
/// operator's.
const KUBECONFIG_NOT_RESOLVED: &str =
    "this environment's kubeconfig was not resolved for the observation; nothing could be read";

/// Legacy's wording, verbatim: the all-namespace scan found no
/// `core-install-metadata` anywhere.
const INSTALL_METADATA_NOT_FOUND: &str = "vpadm install metadata not found";

/// Legacy's wording, verbatim: the `ConfigMap` is there and has no usable
/// version in it.
const PLATFORM_VERSION_MISSING: &str = "platformVersion field missing in core-install-metadata";

/// Observe the environment this handle points at.
///
/// A **value**, never an error: both halves of a failed observation are
/// persisted and shown, because an operator who can see why it failed can fix
/// it and one who sees a blank page cannot.
pub async fn observe(env: &EnvironmentHandle<'_>) -> PluginObservation {
    let Some(kubeconfig) = env.resolved(KUBECONFIG_KEY) else {
        return both_failed(PluginFailure::classified(
            FailureClass::Internal,
            KUBECONFIG_NOT_RESOLVED,
        ));
    };

    // One client for both halves, so one handshake serves both — and so a
    // client that could not be built fails both with the same classified
    // failure, which is the one coupling this function keeps.
    let client = match KubeClient::from_kubeconfig(kubeconfig).await {
        Ok(client) => client,
        Err(failure) => return both_failed(failure),
    };

    let vpadm_namespace = vpadm_namespace(env.config);
    let environment = observe_environment(&client, &vpadm_namespace).await;

    // Independent of the above, deliberately. See this module's header.
    let health = client.node_health().await;

    PluginObservation {
        environment,
        health,
    }
}

/// The environment half of an observation: detect the install, then read the
/// gateway hostnames the base domain is derived from.
///
/// Split out of [`observe`] so it can be driven against a stub API server
/// without a kubeconfig — [`observe`] builds its own client from stored
/// credential material, while the reads under test here only need a cluster
/// that answers. A test that reassembled these three calls itself would be
/// asserting against its own copy of the orchestration rather than this one.
async fn observe_environment(client: &KubeClient, vpadm_namespace: &str) -> ObservationOutcome {
    match detect(client, vpadm_namespace).await {
        Ok(mut detected) => {
            let hosts = gateway_hosts(client, &detected.namespace).await;
            detected.base_domain = compute_base_domain(&hosts);
            ObservationOutcome::Detected(observed_attrs(&detected, &hosts))
        }
        Err(failure) => ObservationOutcome::Failed(failure),
    }
}

/// A failure that predates either reading: report it on both channels.
fn both_failed(failure: PluginFailure) -> PluginObservation {
    PluginObservation {
        environment: ObservationOutcome::Failed(failure.clone()),
        health: HealthOutcome::Failed(failure),
    }
}

/// The namespace to look for `core-install-metadata` in.
///
/// Mirrors legacy's `pick_vpadm_namespace` and `qa-environments`' copy of it,
/// including the detail that a present-but-blank override does not count as
/// set any more than an absent one does.
fn vpadm_namespace(config: &serde_json::Value) -> String {
    config
        .get(VPADM_NAMESPACE_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_VPADM_NAMESPACE)
        .to_owned()
}

/// Version detection: the targeted namespace first, then an all-namespace scan
/// by the vpadm label.
///
/// # Errors
///
/// The reads failing, classified through [`qa_plugin_k8s::classify`]; or both
/// reads succeeding and finding no usable `platformVersion`.
///
/// # Why each `?` also logs
///
/// This is where the two facts named in this module's header — *which* of the
/// two reads failed, and the namespace the targeted one used — are recovered.
/// They cannot go in the returned failure: `detail` holds one `&'static str`
/// and [`qa_plugin_k8s::classify`]'s is the more diagnostic string, for the
/// reason set out above. So they go to the log instead, which is the surface
/// that should have carried them all along, and the classified value that
/// reaches the environment page is untouched.
///
/// Both recorded values are safe **by provenance**: `reason` is the classified
/// failure's `Display` (a fixed `&'static str` detail plus, at most,
/// `remote_message` — the one sanctioned carrier), and `namespace` is operator
/// configuration read off `EnvironmentHandle::config`, never credential
/// material. Same shape as `gateway_hosts` a few lines down.
///
/// **And, since Task 9b, by gate as well.** That was not true when these two
/// lines were written: `assert_no_leak` scans emitted `tracing` events, but
/// every drive of it failed at `KubeClient::from_kubeconfig` — the harness
/// plants a PEM private key as the kubeconfig — and returned before `detect`
/// was called. Measured then, and measured again now in the other direction:
/// a temporary `panic!` as the first statement of this function used to leave
/// the whole suite green, and now fails all five of `conformance_tests`'
/// live-cluster drives. Those drives plant a *working* kubeconfig pointing at
/// a stub API server, so `from_kubeconfig` succeeds — and two of those drives
/// point it at a server that refuses one read each, so each `?` site below is
/// entered under the harness's capture, one drive per site.
///
/// The provenance argument above is still the primary one and still has to
/// stand on its own: a gate proves what was emitted on the paths it drove,
/// while provenance is why no path can emit anything else.
async fn detect(
    client: &KubeClient,
    vpadm_namespace: &str,
) -> Result<DetectedPlatform, PluginFailure> {
    // 1. Targeted lookup in the configured (or default) vpadm namespace. A
    //    `ConfigMap` that is absent, or present with no usable
    //    `platformVersion`, falls through to the scan; only a failed *read*
    //    stops here.
    let targeted = client
        .find_configmap(vpadm_namespace, CORE_INSTALL_METADATA)
        .await
        .inspect_err(|failure| {
            tracing::warn!(
                namespace = vpadm_namespace,
                reason = %failure,
                "failed to read core-install-metadata in the targeted namespace"
            );
        })?;
    if let Some(found) = targeted
        && let Some(detected) = detected_from_data(&found.data, &found.namespace, vpadm_namespace)
    {
        return Ok(detected);
    }

    // 2. Fallback: scan all namespaces by the vpadm label.
    let found = client
        .scan_configmaps(LabelSelector(VPADM_MANAGED_BY), CORE_INSTALL_METADATA)
        .await
        .inspect_err(|failure| {
            // No namespace field: this read is deliberately cluster-wide, and
            // legacy's second sentence named none either.
            tracing::warn!(
                reason = %failure,
                "failed to list core-install-metadata across all namespaces"
            );
        })?;

    if found.len() > 1 {
        tracing::warn!(
            count = found.len(),
            "multiple core-install-metadata ConfigMaps found; taking first"
        );
    }

    let first = found.into_iter().next().ok_or(PluginFailure::classified(
        FailureClass::NotFound,
        INSTALL_METADATA_NOT_FOUND,
    ))?;

    detected_from_data(&first.data, &first.namespace, UNKNOWN_NAMESPACE).ok_or(
        PluginFailure::classified(FailureClass::Malformed, PLATFORM_VERSION_MISSING),
    )
}

/// Externally-visible gateway hostnames from `vp-gateway-hostnames` in the
/// namespace the version was detected in.
///
/// Never a failure. An unreadable or missing gateway map is *inconclusive*,
/// not an error, and must not turn a good version detection into a failed one.
/// An empty result therefore means "nothing to conclude from", which is what
/// [`compute_base_domain`] turns into `None`, and what tells the platform to
/// keep whatever base URL it already stored rather than clobber it.
async fn gateway_hosts(client: &KubeClient, namespace: &str) -> Vec<String> {
    match client
        .find_configmap(namespace, GATEWAY_HOSTNAMES_CONFIGMAP)
        .await
    {
        Ok(Some(found)) => external_hosts(&found.data),
        Ok(None) => Vec::new(),
        Err(failure) => {
            // The classified failure, not the upstream error, for the same
            // reason as every other site in this crate: a `kube::Error` from a
            // call made with an environment's own kubeconfig can box an
            // `AuthError::AuthExecRun`, which prints an exec credential
            // plugin's whole stdout/stderr. A log line is still the material
            // leaving the gear. `namespace` is operator configuration, not
            // credential material, and legacy logs it too.
            tracing::warn!(
                namespace,
                reason = %failure,
                "failed to read vp-gateway-hostnames for base-domain detection"
            );
            Vec::new()
        }
    }
}

/// Project a detection onto the keys [`crate::schemas::observed_schema`]
/// declares.
///
/// Absent facts are **omitted**, never written blank: `project_roles` treats a
/// blank as unset anyway, and Task 15 persists this map as-is, so a written
/// empty string is an empty `APP_VERSION` reaching every test in the run.
///
/// `baseDomain` carries `https://` + the domain, not the bare domain, because
/// it claims [`qa_product_sdk::descriptor::FieldRole::BaseUrl`] and so becomes
/// `observed_base_url` — the column that replaces `vhp_base_url`, whose value
/// has always been `format!("https://{domain}")`. The bare host
/// `VPADM_BASE_DOMAIN` needs is derived back out of it in [`crate::run`],
/// which is the same direction `qa-runs` derives it today.
fn observed_attrs(detected: &DetectedPlatform, hosts: &[String]) -> ObservedAttrs {
    let mut attrs = ObservedAttrs::default();
    attrs.set(PLATFORM_VERSION_KEY, detected.version.clone());
    if let Some(build) = &detected.build {
        attrs.set(BUILD_KEY, build.clone());
    }
    if let Some(domain) = &detected.base_domain {
        attrs.set(BASE_DOMAIN_KEY, format!("{BASE_URL_SCHEME}{domain}"));
    }
    attrs.set(NAMESPACE_KEY, detected.namespace.clone());
    attrs.set(RAW_VERSION_KEY, detected.raw.clone());
    if !hosts.is_empty() {
        attrs.set(EXTERNAL_HOSTS_KEY, hosts.join(HOSTS_SEPARATOR));
    }
    attrs
}

#[cfg(test)]
#[path = "observe_tests.rs"]
mod observe_tests;
