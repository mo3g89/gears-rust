//! What observation does, from the pure rules at one end to a cluster that
//! answers at the other: the namespace rule, the projection onto the declared
//! schema, `detect`'s fall-through, and the failure paths.
//!
//! # How the cluster reads are driven
//!
//! Through `qa_connector_k8s::test_support`, which exists because of this module:
//! ADR-0001 means this crate names no `kube` type, so it cannot hand
//! [`qa_connector_k8s::KubeClient`] the in-process `tower::Service` double
//! `qa-connector-k8s`' own tests use — building one requires `kube::Client::new`.
//! Task 9b published that double from the crate that is allowed to name it,
//! behind a `test-support` feature and with no `kube` type in any signature.
//! Before that, `find_configmap`, `read_configmap` and `scan_configmaps` had
//! **zero** coverage workspace-wide and nothing exercised the fall-through
//! from the targeted namespaced lookup to the all-namespace label scan.
//!
//! What is covered, and by whom:
//!
//! * `qa-connector-k8s` — the reads themselves: what each one sends (including
//!   the scan's label/field selector pair), what it makes of an answer, and
//!   how a failed one classifies.
//! * [`crate::detect`] — the VHP rules, against the real `sv-test` payloads.
//! * this module — `observe`'s orchestration of them: the namespace rule, the
//!   projection onto the declared schema, `detect`'s fall-through against a
//!   cluster that answers, and both failure paths that short-circuit before
//!   any read happens.
//! * `conformance_tests` — the same paths again under
//!   `qa_product_sdk::testing::assert_no_leak`, with the credential material
//!   planted by the harness and the canary planted in the `ConfigMap`
//!   responses.
//!
//! What is **not** driven here is `observe` itself on a success path: it
//! builds its own client from stored credential material, so reaching its
//! success path means standing a real server up and handing it a real
//! kubeconfig. That is exactly what `conformance_tests` does, so it is done
//! there once rather than twice.

use std::sync::{Arc, Mutex};

use credstore_sdk::SecretValue;
use qa_connector_k8s::test_support::{RawBuffer, StubConfigMap, StubRequest};
use qa_product_sdk::observation::project_roles;
use qa_product_sdk::plugin::CredentialSlot;

use super::*;
use crate::schemas::observed_schema;
use crate::test_fixtures::{Answer, REFUSED, absent, refused, scan_found_nothing};

/// The canary this module plants in a kubeconfig, to assert `observe`'s
/// parse-failure branch classifies rather than formats.
///
/// This is the shape that leaked on 2026-08-28 — a PEM private key pasted into
/// the kubeconfig field — and it is planted here rather than only through
/// `assert_no_leak` because the harness documents this exact blind spot: it
/// drives one happy-path-shaped plant and never gives a plugin something
/// malformed to fail on, which is where the original key escaped.
const KUBECONFIG_CANARY: &str = concat!(
    "-----BEGIN PRIVATE KEY-----\n",
    "CANARY-observe-parse-failure-material-do-not-render\n",
    "-----END PRIVATE KEY-----\n",
);

fn resolved_kubeconfig(value: &str) -> Vec<CredentialSlot> {
    vec![CredentialSlot::resolved(
        KUBECONFIG_KEY,
        "qa/vhp/kubeconfig",
        SecretValue::from(value.to_owned()),
    )]
}

fn detected() -> DetectedPlatform {
    DetectedPlatform {
        version: "26.5".to_owned(),
        build: Some("0".to_owned()),
        raw: "26.5.0".to_owned(),
        namespace: "virtuozzo".to_owned(),
        base_domain: Some("sv.jele.io".to_owned()),
    }
}

// ── the namespace rule ────────────────────────────────────────────────────

#[test]
fn an_absent_override_falls_back_to_the_vpadm_default() {
    assert_eq!(
        vpadm_namespace(&serde_json::Value::Null),
        DEFAULT_VPADM_NAMESPACE
    );
    assert_eq!(
        vpadm_namespace(&serde_json::json!({})),
        DEFAULT_VPADM_NAMESPACE
    );
}

#[test]
fn a_configured_override_wins_and_is_trimmed() {
    let config = serde_json::json!({ VPADM_NAMESPACE_KEY: "  vhp-alt  " });

    assert_eq!(vpadm_namespace(&config), "vhp-alt");
}

/// Legacy's `pick_vpadm_namespace` detail: a present but blank override does
/// not count as set any more than an absent one does. Without this an operator
/// who cleared the field would send the lookup at namespace `""`.
#[test]
fn a_blank_override_is_not_an_override() {
    let config = serde_json::json!({ VPADM_NAMESPACE_KEY: "   " });

    assert_eq!(vpadm_namespace(&config), DEFAULT_VPADM_NAMESPACE);
}

/// The configuration channel is operator-authored JSON, so a value of the
/// wrong type is a shape this has to survive rather than a case that cannot
/// happen.
#[test]
fn a_non_string_override_falls_back_rather_than_panicking() {
    let config = serde_json::json!({ VPADM_NAMESPACE_KEY: 42 });

    assert_eq!(vpadm_namespace(&config), DEFAULT_VPADM_NAMESPACE);
}

// ── the projection onto the declared schema ───────────────────────────────

#[test]
fn a_detection_projects_onto_the_declared_observed_keys() {
    let hosts = vec!["api.sv.jele.io".to_owned(), "app.sv.jele.io".to_owned()];

    let attrs = observed_attrs(&detected(), &hosts);

    assert_eq!(attrs.get(PLATFORM_VERSION_KEY), Some("26.5"));
    assert_eq!(attrs.get(BUILD_KEY), Some("0"));
    assert_eq!(attrs.get(RAW_VERSION_KEY), Some("26.5.0"));
    assert_eq!(attrs.get(NAMESPACE_KEY), Some("virtuozzo"));
    assert_eq!(
        attrs.get(EXTERNAL_HOSTS_KEY),
        Some("api.sv.jele.io, app.sv.jele.io")
    );
}

/// `baseDomain` claims `FieldRole::BaseUrl` and therefore becomes
/// `observed_base_url`, whose predecessor `vhp_base_url` was written as
/// `format!("https://{domain}")`. Storing the bare domain here would make the
/// successor column a different value under the same meaning, and every reader
/// of it — the environment page, Task 10's `E2E_VHP_BASE_URL` — would silently
/// change.
#[test]
fn the_base_url_is_the_detected_domain_with_a_scheme() {
    let attrs = observed_attrs(&detected(), &[]);

    assert_eq!(attrs.get(BASE_DOMAIN_KEY), Some("https://sv.jele.io"));
}

/// An inconclusive gateway read is `None`, never `Some("")`: `project_roles`
/// treats a blank as unset, and Task 15 persists this map as it stands, so a
/// written empty string is an empty `APP_VERSION` reaching every test in the
/// run.
#[test]
fn absent_facts_are_omitted_rather_than_written_blank() {
    let sparse = DetectedPlatform {
        build: None,
        base_domain: None,
        ..detected()
    };

    let attrs = observed_attrs(&sparse, &[]);

    assert_eq!(attrs.get(BUILD_KEY), None);
    assert_eq!(attrs.get(BASE_DOMAIN_KEY), None);
    assert_eq!(attrs.get(EXTERNAL_HOSTS_KEY), None);
    // What was detected is still there: an inconclusive base domain does not
    // cost the version.
    assert_eq!(attrs.get(PLATFORM_VERSION_KEY), Some("26.5"));
}

/// The end-to-end claim the roles exist for: what this plugin writes is what
/// the platform's `observed_version` / `observed_build` / `observed_base_url`
/// columns read, through the platform's own projection rather than a parallel
/// mapping that could drift.
#[test]
fn the_declared_roles_project_the_platforms_four_columns() {
    let attrs = observed_attrs(&detected(), &["api.sv.jele.io".to_owned()]);

    let projected = project_roles(&observed_schema(), &attrs);

    assert_eq!(projected.version.as_deref(), Some("26.5"));
    assert_eq!(projected.build.as_deref(), Some("0"));
    assert_eq!(projected.base_url.as_deref(), Some("https://sv.jele.io"));
    assert_eq!(projected.namespace.as_deref(), Some("virtuozzo"));
}

/// Every key `observe` can write is declared, so `retain_declared` drops none
/// of them. An undeclared key would be silently discarded before persistence —
/// a fact detected and then thrown away, with nothing to notice it.
#[test]
fn every_key_observation_writes_is_declared_in_the_schema() {
    let attrs = observed_attrs(&detected(), &["api.sv.jele.io".to_owned()]);
    let schema = observed_schema();

    for (key, _) in attrs.iter() {
        assert!(
            schema.iter().any(|field| field.key == key),
            "`{key}` is written by observe() and declared nowhere"
        );
    }
}

// ── the failure paths ─────────────────────────────────────────────────────

/// `observe` is the one method whose caller is expected to have resolved the
/// slots it needs. When it did not, there was no reading of either kind to
/// report, so both channels carry the same classified failure — the same
/// coupling a client that could not be built gets, and the only one.
#[tokio::test]
async fn an_unresolved_kubeconfig_fails_both_halves_identically() {
    let slots = Vec::new();
    let config = serde_json::Value::Null;
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let observation = observe(&env).await;

    let ObservationOutcome::Failed(environment) = observation.environment else {
        panic!("an environment with no resolved kubeconfig cannot be observed")
    };
    let HealthOutcome::Failed(health) = observation.health else {
        panic!("and its health cannot be read either")
    };
    assert_eq!(environment.class, FailureClass::Internal);
    assert_eq!(environment, health);
}

/// The 2026-08-28 leak, driven directly: a PEM private key pasted into the
/// kubeconfig field. The original quoted the offending scalar, and for a
/// document that *is* one scalar the offending scalar is the whole document.
///
/// This is the branch `assert_no_leak` documents itself as blind to, so it is
/// asserted here instead — against the failure's `Display`, its `Debug`, and
/// its `remote_message`, which is the one `String` sanctioned to cross the
/// boundary and therefore the one a careless author would reach for.
///
/// `use_debug` is allowed here because the `Debug` rendering *is* the surface
/// under test — the same reason `qa-product-sdk`'s own harness allows it at
/// its one comparable callsite.
#[allow(clippy::use_debug)]
#[tokio::test]
async fn a_malformed_kubeconfig_classifies_rather_than_quoting_itself() {
    let slots = resolved_kubeconfig(KUBECONFIG_CANARY);
    let config = serde_json::Value::Null;
    let env = EnvironmentHandle {
        slots: &slots,
        config: &config,
        observed: None,
    };

    let observation = observe(&env).await;

    let ObservationOutcome::Failed(failure) = &observation.environment else {
        panic!("a PEM private key is not a kubeconfig and cannot be observed with")
    };
    assert_eq!(failure.class, FailureClass::Malformed);
    assert_eq!(failure.remote_message, None);
    // A client that could never be built is the one failure that costs both
    // halves: there was no reading of either kind to report.
    let HealthOutcome::Failed(health) = &observation.health else {
        panic!("nor could its health be read with the same unusable client")
    };
    assert_eq!(health, failure);

    for rendering in [format!("{failure}"), format!("{observation:?}")] {
        for marker in ["CANARY-observe-parse-failure", "BEGIN PRIVATE KEY"] {
            assert!(
                !rendering.contains(marker),
                "the kubeconfig reached a rendered failure"
            );
        }
    }
}

// ── `detect`'s fall-through, against a cluster that answers ───────────────
//
// The targeted namespaced lookup, the all-namespace label scan it falls
// through to, and the gateway read that follows the namespace the metadata
// was actually found in. These are the frozen VHP rules end to end: the
// assertions below are on the observed attributes they produce, not on a call
// having returned `Ok`.

/// The `vp-gateway-hostnames` map the real `sv-test` install carries, reduced
/// to the three entry shapes the rules distinguish: two externally-visible
/// hosts (which is the minimum a base domain can be derived from), and the
/// bare-cluster-IP key that must be skipped by name before any parse.
fn gateway_map(namespace: &str) -> Answer {
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
            .body(),
    )
}

/// A cluster that answers the three reads `observe_environment` makes, plus
/// the log of what it was asked.
///
/// Matched on the path rather than a whole URL because the namespace in the
/// two namespaced paths is what several of these tests are asserting about:
/// a route table keyed on the full path would have to know the answer in
/// advance.
fn cluster(
    targeted: Answer,
    scan: Answer,
    gateway: Answer,
) -> (KubeClient, Arc<Mutex<Vec<StubRequest>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let client = KubeClient::from_routes(move |request| {
        recorder.lock().unwrap().push(request.clone());
        let path = request.path.as_str();
        if path.ends_with("/configmaps/core-install-metadata") {
            targeted.clone()
        } else if path.ends_with("/configmaps/vp-gateway-hostnames") {
            gateway.clone()
        } else if path == "/api/v1/configmaps" {
            scan.clone()
        } else {
            panic!(
                "the stub cluster got a request observation does not make: {}",
                request.target()
            )
        }
    });
    (client, seen)
}

fn paths(seen: &Arc<Mutex<Vec<StubRequest>>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|r| r.path.clone())
        .collect()
}

/// The one request that went to the cluster-wide `ConfigMap` list.
fn scan_request(seen: &Arc<Mutex<Vec<StubRequest>>>) -> StubRequest {
    seen.lock()
        .unwrap()
        .iter()
        .find(|request| request.path == "/api/v1/configmaps")
        .cloned()
        .expect("the fall-through must have reached the all-namespace scan")
}

fn detected_attrs(outcome: ObservationOutcome) -> ObservedAttrs {
    match outcome {
        ObservationOutcome::Detected(attrs) => attrs,
        ObservationOutcome::Failed(failure) => {
            panic!("expected a detection, got the failure: {failure}")
        }
    }
}

/// 1. The targeted lookup hits: nothing else is read for the version, and the
///    gateway map in that same namespace yields the base domain.
#[tokio::test]
async fn a_targeted_hit_detects_without_scanning_every_namespace() {
    let (client, seen) = cluster(
        (
            200,
            StubConfigMap::new(DEFAULT_VPADM_NAMESPACE, "core-install-metadata")
                .with("platformVersion", "26.5.0")
                .body(),
        ),
        scan_found_nothing(),
        gateway_map(DEFAULT_VPADM_NAMESPACE),
    );

    let attrs = detected_attrs(observe_environment(&client, DEFAULT_VPADM_NAMESPACE).await);

    assert_eq!(attrs.get(PLATFORM_VERSION_KEY), Some("26.5"));
    assert_eq!(attrs.get(BUILD_KEY), Some("0"));
    assert_eq!(attrs.get(RAW_VERSION_KEY), Some("26.5.0"));
    assert_eq!(attrs.get(NAMESPACE_KEY), Some(DEFAULT_VPADM_NAMESPACE));
    assert_eq!(attrs.get(BASE_DOMAIN_KEY), Some("https://sv.jele.io"));
    assert_eq!(
        attrs.get(EXTERNAL_HOSTS_KEY),
        Some("api.sv.jele.io, app.sv.jele.io"),
        "the internal cluster-IP entry must be skipped by name, not parsed"
    );
    assert_eq!(
        paths(&seen),
        vec![
            "/api/v1/namespaces/virtuozzo/configmaps/core-install-metadata".to_owned(),
            "/api/v1/namespaces/virtuozzo/configmaps/vp-gateway-hostnames".to_owned(),
        ],
        "a targeted hit must not go on to scan every namespace"
    );
}

/// 2. The targeted lookup 404s and the scan hits — the fall-through legacy
///    exists for: an install that did not land in the namespace this
///    environment was configured with is still findable by the label vpadm
///    stamps. Everything downstream then follows the namespace that
///    *answered*, not the one that was asked for.
#[tokio::test]
async fn a_targeted_miss_falls_through_to_the_all_namespace_scan() {
    let (client, seen) = cluster(
        absent(),
        (
            200,
            StubConfigMap::list_body(&[StubConfigMap::new("vhp-alt", "core-install-metadata")
                .with("platformVersion", "26.4.1")]),
        ),
        gateway_map("vhp-alt"),
    );

    let attrs = detected_attrs(observe_environment(&client, DEFAULT_VPADM_NAMESPACE).await);

    assert_eq!(attrs.get(PLATFORM_VERSION_KEY), Some("26.4"));
    assert_eq!(attrs.get(BUILD_KEY), Some("1"));
    assert_eq!(
        attrs.get(NAMESPACE_KEY),
        Some("vhp-alt"),
        "the recorded namespace is where it was found, not where it was sought"
    );
    assert_eq!(
        paths(&seen),
        vec![
            "/api/v1/namespaces/virtuozzo/configmaps/core-install-metadata".to_owned(),
            "/api/v1/configmaps".to_owned(),
            "/api/v1/namespaces/vhp-alt/configmaps/vp-gateway-hostnames".to_owned(),
        ],
        "the gateway read must follow the namespace the scan found"
    );

    // The two selectors *this plugin* chose, in the percent-encoded form they
    // went out in. `qa-connector-k8s` proves its `scan_configmaps` puts a caller's
    // selectors on the wire intact, but it holds its own fixture copy of the
    // vpadm label — correctly, since a product-agnostic crate must not learn a
    // product's labels — so nothing there or anywhere else pinned
    // `observe.rs`' own `VPADM_MANAGED_BY` and `CORE_INSTALL_METADATA`. Editing
    // either used to break no test.
    let scan = scan_request(&seen);
    assert_eq!(
        scan.query_param("labelSelector"),
        Some("app.kubernetes.io%2Fmanaged-by%3Dvpadm"),
        "the vpadm label is what makes an install findable outside its expected \
         namespace at all, got: {:?}",
        scan.query
    );
    assert_eq!(
        scan.query_param("fieldSelector"),
        Some("metadata.name%3Dcore-install-metadata"),
        "a scan without the name selector would match every vpadm-managed ConfigMap \
         in the cluster, got: {:?}",
        scan.query
    );
}

/// 3. The targeted lookup *hits* but carries no usable `platformVersion` —
///    which is "keep scanning", not an answer and not an error. The scanned
///    object here also carries no namespace of its own, which is the shape
///    legacy records as `unknown`.
#[tokio::test]
async fn a_targeted_hit_without_a_version_keeps_scanning() {
    let (client, seen) = cluster(
        (
            200,
            StubConfigMap::new(DEFAULT_VPADM_NAMESPACE, "core-install-metadata")
                .with("installedAt", "2026-08-01T00:00:00Z")
                .body(),
        ),
        (
            200,
            StubConfigMap::list_body(&[StubConfigMap::new("", "core-install-metadata")
                .without_namespace()
                .with("platformVersion", "26.5.0")]),
        ),
        absent(),
    );

    let attrs = detected_attrs(observe_environment(&client, DEFAULT_VPADM_NAMESPACE).await);

    assert_eq!(attrs.get(PLATFORM_VERSION_KEY), Some("26.5"));
    assert_eq!(
        attrs.get(NAMESPACE_KEY),
        Some("unknown"),
        "an object the API server reports with no namespace is legacy's `unknown`"
    );
    assert_eq!(
        attrs.get(BASE_DOMAIN_KEY),
        None,
        "an absent gateway map is inconclusive, and must not overwrite a stored base URL"
    );
    assert!(
        paths(&seen).contains(&"/api/v1/configmaps".to_owned()),
        "a ConfigMap without a platformVersion must not stop the scan"
    );
}

/// 4. Nothing anywhere: legacy's wording, and `NotFound` rather than
///    `Malformed`, because nothing was found to be malformed.
#[tokio::test]
async fn a_scan_that_finds_nothing_is_the_install_metadata_not_found_failure() {
    let (client, _) = cluster(absent(), scan_found_nothing(), absent());

    let ObservationOutcome::Failed(failure) =
        observe_environment(&client, DEFAULT_VPADM_NAMESPACE).await
    else {
        panic!("a cluster with no vpadm install metadata cannot be detected")
    };

    assert_eq!(failure.class, FailureClass::NotFound);
    assert_eq!(failure.detail, Some(INSTALL_METADATA_NOT_FOUND));
}

/// The other end of the same fall-through, and a different verdict: the scan
/// *did* find the `ConfigMap`, and it has no usable version in it. Legacy
/// distinguishes the two — "not found" sends an operator to look at the
/// install, "field missing" sends them to look at the object — and a copy
/// that collapsed them would be a behaviour change with nothing to notice it.
#[tokio::test]
async fn a_scan_that_finds_a_versionless_configmap_is_the_field_missing_failure() {
    let (client, _) = cluster(
        absent(),
        (
            200,
            StubConfigMap::list_body(&[StubConfigMap::new("vhp-alt", "core-install-metadata")
                .with("platformVersion", "  ")]),
        ),
        absent(),
    );

    let ObservationOutcome::Failed(failure) =
        observe_environment(&client, DEFAULT_VPADM_NAMESPACE).await
    else {
        panic!("a blank platformVersion is not a version")
    };

    assert_eq!(failure.class, FailureClass::Malformed);
    assert_eq!(failure.detail, Some(PLATFORM_VERSION_MISSING));
}

// ── the two `warn!` sites, and what they are allowed to carry ─────────────
//
// `detect` cannot put *which* read failed, or the namespace it used, in the
// returned failure: `PluginFailure::detail` holds one `&'static str` and the
// classifier's is the more diagnostic one (this module's parent explains the
// trade). Both facts go to the log instead, so the log is where they have to
// be asserted — nothing else in the crate can see them.
//
// These lines were unreachable by any test when they were written: the leak
// harness failed at the client build and never entered `detect`, measured by
// a `panic!` on entry that broke nothing. They are reachable now, from here
// and from `conformance_tests`.

/// Run `body` with every `tracing` byte emitted on this thread captured.
///
/// A thread-local default subscriber, not `with_default`'s sync-closure form:
/// the calls are `async` and must stay under this subscriber across every
/// `.await`. `#[tokio::test]` defaults to a current-thread runtime, so the
/// whole call runs on this one thread.
async fn capturing_logs<F, T>(body: F) -> (T, String)
where
    F: std::future::Future<Output = T>,
{
    let buffer = RawBuffer::new();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let value = body.await;
    drop(guard);
    (value, buffer.captured())
}

fn line_containing<'a>(captured: &'a str, needle: &str) -> &'a str {
    captured
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("expected a log line containing `{needle}`, got:\n{captured}"))
}

/// The targeted read failing: the log says which read it was and which
/// namespace it used, and the failure that reaches the environment page is
/// the classifier's, untouched.
#[tokio::test]
async fn a_failed_targeted_read_logs_which_read_and_which_namespace() {
    let (client, _) = cluster(refused(), scan_found_nothing(), absent());

    let (outcome, captured) =
        capturing_logs(observe_environment(&client, DEFAULT_VPADM_NAMESPACE)).await;

    let ObservationOutcome::Failed(failure) = outcome else {
        panic!("a refused read is not a detection")
    };
    assert_eq!(failure.class, FailureClass::AuthRejected);
    assert_eq!(
        failure.remote_message.as_deref(),
        Some(REFUSED),
        "the API server's own words are the one text sanctioned to travel"
    );

    let line = line_containing(
        &captured,
        "failed to read core-install-metadata in the targeted namespace",
    );
    assert!(
        line.contains("namespace=\"virtuozzo\"") || line.contains("namespace=virtuozzo"),
        "the namespace the targeted read used is one of the two facts this line exists to \
         recover, got: {line}"
    );
    assert!(
        line.contains("the API server refused the request"),
        "the logged reason must be the classifier's fixed text, got: {line}"
    );
}

/// The scan failing: the same recovery, minus the namespace. This read is
/// deliberately cluster-wide and legacy's second sentence named none either,
/// so a namespace on this line would be an invention.
#[tokio::test]
async fn a_failed_scan_logs_that_it_was_the_all_namespace_one() {
    let (client, _) = cluster(absent(), refused(), absent());

    let (outcome, captured) =
        capturing_logs(observe_environment(&client, DEFAULT_VPADM_NAMESPACE)).await;

    assert!(
        matches!(outcome, ObservationOutcome::Failed(_)),
        "a refused scan is not a detection"
    );
    let line = line_containing(
        &captured,
        "failed to list core-install-metadata across all namespaces",
    );
    assert!(
        !line.contains("namespace="),
        "this read names no namespace, so neither may its log line: {line}"
    );
    assert!(
        line.contains("the API server refused the request"),
        "the logged reason must be the classifier's fixed text, got: {line}"
    );
}

/// The gateway read failing is *not* a failed observation: an unreadable
/// gateway map is inconclusive, so the version detection stands and only the
/// base domain is left unset. The log still says what happened, in the
/// namespace it happened in.
#[tokio::test]
async fn a_failed_gateway_read_is_logged_and_costs_only_the_base_domain() {
    let (client, _) = cluster(
        (
            200,
            StubConfigMap::new(DEFAULT_VPADM_NAMESPACE, "core-install-metadata")
                .with("platformVersion", "26.5.0")
                .body(),
        ),
        scan_found_nothing(),
        refused(),
    );

    let (outcome, captured) =
        capturing_logs(observe_environment(&client, DEFAULT_VPADM_NAMESPACE)).await;

    let attrs = detected_attrs(outcome);
    assert_eq!(attrs.get(PLATFORM_VERSION_KEY), Some("26.5"));
    assert_eq!(
        attrs.get(BASE_DOMAIN_KEY),
        None,
        "an unreadable gateway map must not overwrite a stored base URL"
    );
    let line = line_containing(
        &captured,
        "failed to read vp-gateway-hostnames for base-domain detection",
    );
    assert!(
        line.contains("the API server refused the request"),
        "the logged reason must be the classifier's fixed text, got: {line}"
    );
}
