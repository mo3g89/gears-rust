//! The two gates every product plugin owes the platform.
//!
//! Layer 2 (`validate_schemas`) is a *boot* failure at registration, and
//! Layer 3 (`assert_no_leak`) is the harness that drives a plugin with planted
//! credential material. Both are run here so a bad schema and a leak are
//! caught by this crate's own `cargo test` rather than by a server that will
//! not start or a page that shows a private key.

use qa_connector_k8s::test_support::{
    StubApiServer, StubConfigMap, StubRequest, empty_namespace_list_body, empty_node_list_body,
};
use qa_product_sdk::descriptor::validate_schemas;
use qa_product_sdk::plugin::QaProductPluginV1;
use qa_product_sdk::testing::{Canary, assert_no_leak};

use super::VhpProductPlugin;
use crate::test_fixtures::{Answer, absent, refused, scan_found_nothing};

/// Layer 2, run here rather than discovered at boot.
///
/// `RegisteredPlugin::new` calls exactly this at registration and refuses the
/// plugin on failure, so a violation is a server that does not start. The
/// three rules it enforces are all things a schema edit can break silently: a
/// secret kind in `observed_schema` (the 2026-08-28 leak through a new door),
/// two fields claiming one role (`APP_VERSION` decided by iteration order),
/// and one key declared in both schemas (the UI rendering whichever descriptor
/// it looked up first).
#[test]
fn the_declared_schemas_pass_the_registration_check() {
    let plugin = VhpProductPlugin::new_for_test();

    validate_schemas(&plugin.credential_schema(), &plugin.observed_schema()).unwrap();
}

/// Layer 3 on the path where the kubeconfig is unusable: the harness's own
/// `Canary::vhp_shaped` plants a bare PEM private key, so `observe` fails
/// inside `KubeClient::from_kubeconfig` and the surfaces this gates are the
/// early failure ones.
///
/// **This is the drive that has been proven to fire.** A leak planted at
/// `observe.rs`'s client-build failure was caught here and reported against
/// `observe().environment PluginFailure.remote_message`. It is kept alongside
/// the live-cluster drives below rather than replaced by them: they cover
/// different halves of the same function, and this half is the one the
/// 2026-08-28 leak was actually in.
///
/// All seven of VHP's own methods are driven and gated as of Task 10. (There
/// were eight: the trait carried a `health_check` default that VHP accepted
/// unchanged. It never had a production caller and was deleted.) The
/// three that were honest stubs through Task 9 — `prepare_run_access`,
/// `runner` and `env_contract` — now return real values, and
/// `prepare_run_access` in particular is driven twice here, once with
/// resolved plaintext and once with credstore references alone, with the two
/// results compared. `crate::run`'s own tests state the same property
/// directly; this is the drive that enforces it.
///
/// The harness's own documented blind spot is error branches: it drives one
/// happy-path-shaped plant and never hands the plugin something malformed to
/// fail on, which is precisely how the 2026-08-28 key escaped. `observe_tests`
/// closes that for this plugin by driving `observe`'s parse-failure path with
/// its own canary.
#[tokio::test]
async fn the_vhp_plugin_leaks_no_credential_material() {
    let plugin = VhpProductPlugin::new_for_test();

    assert_no_leak(&plugin, &Canary::vhp_shaped()).await;
}

// ── Layer 3 again, this time with a cluster that answers ──────────────────
//
// # Why these exist
//
// Until Task 9b the harness never reached this plugin's success path at all.
// It plants a PEM private key as the kubeconfig, so every drive failed inside
// `KubeClient::from_kubeconfig` and returned — measured twice, independently,
// by putting an unconditional `panic!` at the top of `observe`'s `detect` and
// watching the whole suite stay green. `detect`, `gateway_hosts`, every
// `ConfigMap` read and both `warn!` sites were ungated.
//
// What closes it: `qa_connector_k8s::test_support::StubApiServer` is a real
// loopback API server, and its `kubeconfig()` is a *valid* document pointing
// at it. Planted as `Canary::pem`, that document is what the plugin is handed
// as its kubeconfig — so `from_kubeconfig` runs for real and succeeds, the
// reads happen, and the canary markers this crate plants in the `ConfigMap`
// **responses** are in front of every surface the harness scans.
//
// # The alternative that was rejected
//
// A test-only constructor injecting a pre-built `KubeClient` into the plugin
// (`new_for_test` was already there to grow into one) is cheaper and needs no
// server. It was rejected because it buys a weaker gate: `from_kubeconfig`
// would never run, so the planted kubeconfig would reach nothing and the
// harness's central plant would go back to being vacuous on this path — and
// the conformance test would be exercising a construction production never
// uses, which is the shape of test that passes while the real path rots.
//
// # Three markers, three different things they prove absent
//
// * `Canary::pem` is the whole kubeconfig document — the credential itself.
// * `Canary::token` is the bearer token *inside* that document, so an echo of
//   one field rather than the whole file is caught too.
// * `Canary::password` is planted in the `ConfigMap` bodies the cluster
//   returns, so a plugin echoing detected cluster content into an observed
//   attribute, a failure message or a log line is caught.

/// The bearer token the stub kubeconfig authenticates with, and the marker
/// asserted absent from every surface.
const CLUSTER_TOKEN_CANARY: &str = "canary-kubeconfig-bearer-8f3d1a9c7b2e4056a9d3f7c1e8b2a406";

/// Planted in the `ConfigMap` responses, in keys and entries this plugin has
/// no business echoing: an extra `core-install-metadata` field it does not
/// read, and a gateway entry marked internal, which the rules skip.
///
/// Deliberately **not** planted anywhere `detect`'s frozen rules legitimately
/// render content: not in `platformVersion` (which becomes an observed
/// attribute by design), and not in an *unparseable* gateway entry, whose
/// `serde_json` error `detect::external_hosts` formats into a `warn!` by
/// design. A canary in either would fail this plugin for doing what legacy
/// does, which is the one thing Phase C must not change.
const CONFIGMAP_CANARY: &str = "canary-configmap-content-do-not-echo-4e91c7";

/// `core-install-metadata` as a real install carries it, plus one field this
/// plugin does not read and must not echo.
fn install_metadata(namespace: &str) -> Answer {
    (
        200,
        StubConfigMap::new(namespace, "core-install-metadata")
            .with("platformVersion", "26.5.0")
            .with("installerCredential", CONFIGMAP_CANARY)
            .body(),
    )
}

/// `vp-gateway-hostnames`: two externally-visible hosts, the bare cluster-IP
/// key that is skipped by name, and an internal entry whose host carries the
/// canary — parseable, so no error is formatted, and skipped, so nothing may
/// carry it onward.
fn gateway_hostnames(namespace: &str) -> Answer {
    (
        200,
        StubConfigMap::new(namespace, "vp-gateway-hostnames")
            .with(
                "api",
                r#"{"host":"api.sv.jele.io","visibility":"external"}"#,
            )
            .with(
                "app",
                r#"{"host":"app.sv.jele.io","visibility":"external"}"#,
            )
            .with("internal-gateway-clusterip", "10.43.0.17")
            .with(
                "admin",
                &format!(r#"{{"host":"{CONFIGMAP_CANARY}.internal","visibility":"internal"}}"#),
            )
            .body(),
    )
}

/// Drive the whole plugin through `assert_no_leak` against a cluster that
/// answers `metadata` for the targeted `core-install-metadata` read,
/// `gateway` for `vp-gateway-hostnames`, and `scan` for the all-namespace
/// list. The node and namespace lists always succeed, so the health half is
/// never what a failure here is about.
///
/// The namespace in each path is matched by suffix on purpose: the harness
/// synthesises `vpadm_namespace` from the declared schema, and a route table
/// that hard-coded the filler it happens to use today would break the moment
/// the SDK changed it.
async fn assert_no_leak_against(metadata: Answer, gateway: Answer, scan: Answer) {
    let server = StubApiServer::start(move |request: &StubRequest| {
        let path = request.path.as_str();
        match path {
            "/api/v1/nodes" => (200, empty_node_list_body()),
            "/api/v1/namespaces" => (200, empty_namespace_list_body()),
            "/api/v1/configmaps" => scan.clone(),
            _ if path.ends_with("/configmaps/core-install-metadata") => metadata.clone(),
            _ if path.ends_with("/configmaps/vp-gateway-hostnames") => gateway.clone(),
            _ => panic!(
                "the stub cluster got a request observation does not make: {}",
                request.target()
            ),
        }
    })
    .await;

    // The whole document is the `pem` marker, and the token inside it is the
    // `token` marker: an echo of either the file or the one field is a hit.
    // Built from `vhp_shaped` and overwritten field-by-field, rather than a
    // struct literal, so this compiles unchanged however `Canary`'s own
    // (private) fields grow: VHP's `kubeconfig` field carries its own
    // connection target, so this drive has never needed `Canary::with_config`.
    let mut canary = Canary::vhp_shaped();
    canary.pem = server.kubeconfig(CLUSTER_TOKEN_CANARY);
    canary.token = CLUSTER_TOKEN_CANARY.to_owned();
    canary.password = CONFIGMAP_CANARY.to_owned();

    assert_no_leak(&VhpProductPlugin::new_for_test(), &canary).await;

    assert!(
        server
            .requests()
            .iter()
            .any(|request| request.path.ends_with("/configmaps/core-install-metadata")),
        "the harness must have reached a ConfigMap read, or this drive gates nothing that \
         the PEM-kubeconfig drive above does not already gate"
    );
}

/// The success path: the install is found where it was looked for, the
/// gateway map yields a base domain, and the cluster's health is read. Every
/// `ConfigMap` byte the plugin saw carried the canary, and none of it may
/// reach an observed attribute, a serialised `ObservedAttrs`, or a log line.
#[tokio::test]
async fn the_vhp_plugin_leaks_nothing_when_the_cluster_answers() {
    assert_no_leak_against(
        install_metadata("virtuozzo"),
        gateway_hostnames("virtuozzo"),
        scan_found_nothing(),
    )
    .await;
}

/// The fall-through: the targeted lookup 404s and the all-namespace scan
/// answers. Same content, reached through the other branch — and the scan's
/// result is the one `detect` takes `namespace` off, which is an observed
/// attribute.
#[tokio::test]
async fn the_vhp_plugin_leaks_nothing_when_the_scan_is_what_answers() {
    assert_no_leak_against(
        absent(),
        gateway_hostnames("vhp-alt"),
        (
            200,
            StubConfigMap::list_body(&[StubConfigMap::new("vhp-alt", "core-install-metadata")
                .with("platformVersion", "26.5.0")
                .with("installerCredential", CONFIGMAP_CANARY)]),
        ),
    )
    .await;
}

/// The first of `detect`'s two `warn!` sites: the targeted read fails, and
/// the log line that recovers "which read, and in which namespace" is emitted
/// while the harness is capturing. Ruling 9-4's settling argument — that
/// `assert_no_leak` gates these lines — was false when it was made because
/// nothing reached them. This is the drive that makes it true.
#[tokio::test]
async fn the_vhp_plugin_leaks_nothing_when_the_targeted_read_is_refused() {
    assert_no_leak_against(refused(), gateway_hostnames("virtuozzo"), refused()).await;
}

/// The second `warn!` site: the targeted read 404s, the all-namespace scan
/// fails, and the cluster-wide log line is emitted under capture.
#[tokio::test]
async fn the_vhp_plugin_leaks_nothing_when_the_scan_is_refused() {
    assert_no_leak_against(absent(), gateway_hostnames("virtuozzo"), refused()).await;
}

/// `gateway_hosts`' `warn!` site: detection succeeds and the gateway read
/// fails, which is inconclusive rather than fatal — so this drive also covers
/// the one branch where a plugin logs a failure and still returns a
/// detection.
#[tokio::test]
async fn the_vhp_plugin_leaks_nothing_when_the_gateway_read_is_refused() {
    assert_no_leak_against(
        install_metadata("virtuozzo"),
        refused(),
        scan_found_nothing(),
    )
    .await;
}
