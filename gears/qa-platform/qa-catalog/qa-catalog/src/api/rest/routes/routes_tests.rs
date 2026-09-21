//! What this gear's registration actually publishes, read off the **built
//! `OpenAPI` document** rather than off the builder calls.
//!
//! One property is under test here and it is the one the bundle-token change
//! turns on: `GET /qa/v1/test-bundles/{id}` is the single anonymous route in
//! this gear, it requires a `sig` query parameter, and **every other route
//! still requires a bearer token**. A change that widened `.anonymous()` past
//! the one route it belongs on would otherwise be invisible — nothing else in
//! this crate reads the auth axis back, and a route registered anonymously by
//! accident serves real data to an unauthenticated caller while every test in
//! the suite keeps passing.
//!
//! **Read off the document, not the source.** `OperationBuilder`'s typestate
//! already stops `.authenticated()` and `.anonymous()` being combined, so what
//! is left to prove is that the registration reaches the published contract:
//! `OpenApiRegistryImpl::build_openapi` emits a `security` requirement exactly
//! when `spec.authenticated` is set, and api-gateway's own middleware resolves
//! the same flag to `AuthRequirement::Required` or `AuthRequirement::None`.
//! Asserting on the document is asserting on the same bit, through the one
//! rendering of it that a reader can inspect.
//!
//! Shape follows qa-environments' and qa-runs' route tests: build the real
//! registry, build the real document, assert on `serde_json::Value` (never on
//! text — `serde_json`'s `preserve_order` is on workspace-wide, so a textual
//! comparison passes per-package and fails under `make test-no-macros`).

use axum::Router;
use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};

use super::register_operations;

/// The download route's path, as it appears in the document.
const BUNDLE_PATH: &str = "/qa/v1/test-bundles/{id}";

/// The real document, built the way `RestApiCapability` builds it.
///
/// `register_operations` is `register_routes` minus the `Extension` layer
/// (which needs an `AppServices` and contributes nothing to the document), so
/// a registrar added there is covered here without anyone editing this file.
fn published_document() -> serde_json::Value {
    let openapi = OpenApiRegistryImpl::new();
    let _router = register_operations(Router::new(), &openapi);
    let doc = openapi
        .build_openapi(&OpenApiInfo::default())
        .expect("the OpenAPI document must build");
    serde_json::to_value(&doc).expect("the document must serialize")
}

/// **The bundle download is anonymous, and its `sig` is required.**
///
/// The two halves are one property: an anonymous route whose access control is
/// optional has no access control. `sig` is declared `required: true` so a
/// request without it is rejected by the extractor before the handler runs,
/// rather than reaching a verification with an empty tag.
///
/// The route is anonymous because its caller is an Argo workflow pod with no
/// user to borrow a session from. It used to solve that with a
/// `fullScopeAllowed` OIDC client secret carried in the pod's own environment —
/// see `routes::bundles`' header for what tenant-authored test code could reach
/// with it, and why the signature replaced it rather than joining it.
#[test]
fn the_bundle_download_is_anonymous_and_requires_a_signature() {
    let rendered = published_document();
    let operation = &rendered["paths"][BUNDLE_PATH]["get"];

    assert!(
        !operation.is_null(),
        "the download route must be registered at {BUNDLE_PATH}"
    );
    assert!(
        operation.get("security").is_none(),
        "GET {BUNDLE_PATH} must carry NO security requirement: it is registered \
         .anonymous(), and api-gateway resolves that to AuthRequirement::None -- \
         it inserts SecurityContext::anonymous() and runs the handler without \
         reading the Authorization header at all. Found: {}",
        operation["security"]
    );

    let parameters = operation["parameters"]
        .as_array()
        .expect("the operation must declare parameters");
    let sig = parameters
        .iter()
        .find(|parameter| parameter["name"] == "sig")
        .unwrap_or_else(|| {
            panic!("GET {BUNDLE_PATH} must declare a `sig` query parameter: {parameters:?}")
        });
    assert_eq!(sig["in"], "query", "sig is a query parameter: {sig}");
    assert_eq!(
        sig["required"], true,
        "sig must be REQUIRED. It is the whole access control on an anonymously \
         reachable route; an optional one would let a caller omit it and reach \
         the handler with an empty tag: {sig}"
    );
}

/// **An omitted `sig` is a 400, and the contract says so.**
///
/// The sibling test above pins `required: true`; this one pins what that
/// actually costs a caller. Because the parameter is required and the handler
/// takes a non-optional `Query<BundleDownloadQuery>`, a request with no `sig`
/// is refused by the extractor **before** `get_bundle_content_signed` runs —
/// so it answers 400, not the 403 the signature refusal answers. Live
/// verification confirmed both: a tampered tag is 403, an absent one is 400.
///
/// This is fail-closed and correct. What was wrong was the documentation,
/// which described the absent case as part of the merged 403. A published
/// contract that omits 400 is the same defect one level up, so the status is
/// declared and asserted here rather than left to a reader to discover from a
/// live call.
#[test]
fn an_omitted_signature_is_documented_as_a_400() {
    let rendered = published_document();
    let responses = &rendered["paths"][BUNDLE_PATH]["get"]["responses"];

    assert!(
        !responses["400"].is_null(),
        "GET {BUNDLE_PATH} must document 400: `sig` is required, so an omitted \
         one is rejected by the extractor before the handler runs. Found: {responses}"
    );
    assert!(
        !responses["403"].is_null(),
        "GET {BUNDLE_PATH} must still document 403: that is the signature \
         refusal a tampered tag gets. Found: {responses}"
    );
}

/// **No other route in this gear is anonymous.**
///
/// The blast radius of the previous test, pinned from the other side. This gear
/// serves test repositories, plans, products, SSH-key metadata and the plugin
/// catalogue; exactly one of its operations may be reachable without a token,
/// and it is the one above.
///
/// Enumerated from the document rather than from a hardcoded list, so a route
/// added tomorrow is covered without anyone remembering to add it here.
#[test]
fn every_other_route_still_requires_a_bearer_token() {
    let rendered = published_document();
    let paths = rendered["paths"]
        .as_object()
        .expect("the document must declare paths");

    let mut anonymous: Vec<String> = Vec::new();
    for (path, item) in paths {
        let methods = item.as_object().expect("a path item is an object");
        for (method, operation) in methods {
            if operation.get("security").is_none() {
                anonymous.push(format!("{} {path}", method.to_uppercase()));
            }
        }
    }

    assert_eq!(
        anonymous,
        vec![format!("GET {BUNDLE_PATH}")],
        "exactly one operation in qa-catalog may be anonymous. Anything else in \
         this list is a route serving real data to a caller that presented no \
         credential at all"
    );
}
