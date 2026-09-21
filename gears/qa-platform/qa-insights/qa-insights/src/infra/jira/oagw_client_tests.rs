//! Tests for the oagw JIRA adapter.
//!
//! Everything here runs against a scripted [`FakeGateway`] rather than a real
//! `oagw`: what is under test is the *request this gear sends* and the
//! *provisioning it performs*, both of which a real gateway would absorb into
//! its own behaviour. Two of those are security surfaces with no other witness —
//! the `secret_ref` the upstream carries, and the fact that no proxied request
//! carries credential material — and neither has a compile-time guard.
//!
//! Legacy citations throughout are `manager/src/services/jira.rs` unless stated.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use qa_insights_sdk::JiraConfig;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{OagwJiraClient, egress_for, normalize_base_path, summary_for, truncate_logs};
use crate::domain::error::DomainError;
use crate::domain::ports::jira_client::{JiraClient, NewIssue};
use crate::domain::service::test_support::ctx;

#[resource_error(gts_id!("cf.core.oagw.upstream.v1~"))]
struct TestUpstreamScope;

const TENANT: Uuid = Uuid::from_u128(0x0A);
const TOKEN_REF: &str = "cred://qa-jira-api-token";

/// A config pointing at `jira.example.com`, enabled.
fn config() -> JiraConfig {
    JiraConfig {
        url: "https://jira.example.com/".to_owned(),
        project_key: "VHP".to_owned(),
        email: "qa@example.com".to_owned(),
        api_token_credstore_ref: TOKEN_REF.to_owned(),
        issue_type: None,
        enabled: true,
    }
}

fn issue() -> NewIssue {
    NewIssue {
        test_name: "tests/test_login.py::test_ok".to_owned(),
        plan: "plans/smoke/plan.yaml".to_owned(),
        app_version: None,
        platform: None,
        run_name: "nightly-42".to_owned(),
        logs: "boom".to_owned(),
    }
}

/// One proxied request, flattened so the assertions can read it without a
/// non-`Clone` body in the way.
#[derive(Clone, Debug)]
struct Proxied {
    method: String,
    uri: String,
    /// Every request header name, lowercased. The **absence** of
    /// `authorization` here is what
    /// [`no_proxied_request_carries_credential_material`] asserts.
    header_names: Vec<String>,
    body: String,
}

/// What [`FakeGateway`] answers a proxied request with, chosen by the path it is
/// for.
#[derive(Clone)]
struct Scripted {
    status: u16,
    body: String,
}

/// A scripted [`ServiceGatewayClientV1`].
///
/// `create_upstream` succeeds by default and can be told to answer
/// `AlreadyExists` instead. Every proxied request is recorded; the response is
/// picked by whether the request URI contains `/search` or not, which is the only
/// distinction the three calls need.
struct FakeGateway {
    create_calls: Mutex<Vec<oagw_sdk::CreateUpstreamRequest>>,
    route_calls: Mutex<Vec<oagw_sdk::CreateRouteRequest>>,
    list_calls: Mutex<u32>,
    proxied: Mutex<Vec<Proxied>>,
    upstream_already_exists: bool,
    /// What `list_upstreams` pages over — the upstreams that "already exist".
    existing: Vec<oagw_sdk::Upstream>,
    route_outcome: RouteOutcome,
    search_response: Scripted,
    other_response: Scripted,
    /// When set, [`FakeGateway::proxy_request`] never resolves — the
    /// black-holing JIRA instance
    /// `infra::notify::slack_oagw_tests::FakeGateway::hanging`'s own reason,
    /// mirrored here for
    /// [`the_bound_is_actually_applied_to_a_hanging_gateway`].
    hang: bool,
}

/// How [`FakeGateway::create_route`] answers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RouteOutcome {
    Created,
    /// The route is already registered — idempotent, and **not** a failure.
    AlreadyExists,
    /// A transient gateway failure.
    Fails,
}

impl FakeGateway {
    fn new(search: Scripted, other: Scripted) -> Self {
        Self {
            create_calls: Mutex::new(Vec::new()),
            route_calls: Mutex::new(Vec::new()),
            list_calls: Mutex::new(0),
            proxied: Mutex::new(Vec::new()),
            upstream_already_exists: false,
            existing: Vec::new(),
            route_outcome: RouteOutcome::Created,
            search_response: search,
            other_response: other,
            hang: false,
        }
    }

    /// A gateway whose search finds nothing and whose issue calls answer `body`.
    fn answering(body: &str) -> Self {
        Self::new(
            Scripted {
                status: 200,
                body: r#"{"issues":[]}"#.to_owned(),
            },
            Scripted {
                status: 200,
                body: body.to_owned(),
            },
        )
    }

    /// A gateway whose `proxy_request` never resolves — the black-holing
    /// JIRA instance [`OagwJiraClient::REQUEST_TIMEOUT`]'s own doc names.
    /// [`the_bound_is_actually_applied_to_a_hanging_gateway`]'s fixture.
    fn hanging() -> Self {
        let mut gateway = Self::answering("");
        gateway.hang = true;
        gateway
    }

    fn upstreams(&self) -> Vec<oagw_sdk::CreateUpstreamRequest> {
        self.create_calls.lock().unwrap().clone()
    }

    fn routes(&self) -> Vec<oagw_sdk::CreateRouteRequest> {
        self.route_calls.lock().unwrap().clone()
    }

    fn requests(&self) -> Vec<Proxied> {
        self.proxied.lock().unwrap().clone()
    }

    fn list_calls(&self) -> u32 {
        *self.list_calls.lock().unwrap()
    }

    fn create_calls_len(&self) -> usize {
        self.create_calls.lock().unwrap().len()
    }
}

/// An upstream row for [`FakeGateway::existing`].
fn upstream_with_alias(alias: &str) -> oagw_sdk::Upstream {
    oagw_sdk::Upstream {
        id: Uuid::from_u128(0xBEEF),
        tenant_id: TENANT,
        alias: alias.to_owned(),
        server: oagw_sdk::Server { endpoints: vec![] },
        protocol: oagw_sdk::HTTP_PROTOCOL_ID.to_owned(),
        enabled: true,
        auth: None,
        headers: None,
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
    }
}

#[async_trait::async_trait]
impl oagw_sdk::api::ServiceGatewayClientV1 for FakeGateway {
    async fn create_upstream(
        &self,
        _: SecurityContext,
        req: oagw_sdk::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        let alias = req.alias().unwrap_or_default().to_owned();
        self.create_calls.lock().unwrap().push(req);
        if self.upstream_already_exists {
            return Err(TestUpstreamScope::already_exists("upstream already exists")
                .with_resource(alias)
                .create());
        }
        Ok(upstream_with_alias(&alias))
    }

    async fn create_route(
        &self,
        _: SecurityContext,
        req: oagw_sdk::CreateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        let upstream_id = req.upstream_id();
        let match_rules = req.match_rules().clone();
        self.route_calls.lock().unwrap().push(req);
        match self.route_outcome {
            RouteOutcome::Created => {}
            RouteOutcome::AlreadyExists => {
                return Err(TestUpstreamScope::already_exists("route already exists")
                    .with_resource("jira-route")
                    .create());
            }
            RouteOutcome::Fails => {
                return Err(CanonicalError::service_unavailable().create());
            }
        }
        Ok(oagw_sdk::Route {
            id: Uuid::from_u128(0xF00D),
            tenant_id: TENANT,
            upstream_id,
            match_rules,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
        })
    }

    async fn proxy_request(
        &self,
        _: SecurityContext,
        req: http::Request<oagw_sdk::Body>,
    ) -> Result<http::Response<oagw_sdk::Body>, CanonicalError> {
        if self.hang {
            return std::future::pending().await;
        }

        let method = req.method().to_string();
        let uri = req.uri().to_string();
        let header_names = req
            .headers()
            .keys()
            .map(|k| k.as_str().to_ascii_lowercase())
            .collect();
        let body = String::from_utf8(
            req.into_body()
                .into_bytes()
                .await
                .expect("the fake body is buffered")
                .to_vec(),
        )
        .expect("the request body is UTF-8");

        let scripted = if uri.contains("/search") {
            self.search_response.clone()
        } else {
            self.other_response.clone()
        };
        self.proxied.lock().unwrap().push(Proxied {
            method,
            uri,
            header_names,
            body,
        });

        Ok(http::Response::builder()
            .status(scripted.status)
            .body(oagw_sdk::Body::from(scripted.body))
            .expect("the fake response builds"))
    }

    async fn get_upstream(
        &self,
        _: SecurityContext,
        _: Uuid,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unimplemented!("the adapter never looks an upstream up by id")
    }
    async fn list_upstreams(
        &self,
        _: SecurityContext,
        query: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Upstream>, CanonicalError> {
        *self.list_calls.lock().unwrap() += 1;
        Ok(self
            .existing
            .iter()
            .skip(query.skip as usize)
            .take(query.top as usize)
            .cloned()
            .collect())
    }
    async fn update_upstream(
        &self,
        _: SecurityContext,
        _: Uuid,
        _: oagw_sdk::UpdateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unimplemented!("the adapter never updates an upstream; see the module header")
    }
    async fn delete_upstream(&self, _: SecurityContext, _: Uuid) -> Result<(), CanonicalError> {
        unimplemented!()
    }
    async fn get_route(
        &self,
        _: SecurityContext,
        _: Uuid,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unimplemented!()
    }
    async fn list_routes(
        &self,
        _: SecurityContext,
        _: Option<Uuid>,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Route>, CanonicalError> {
        unimplemented!()
    }
    async fn update_route(
        &self,
        _: SecurityContext,
        _: Uuid,
        _: oagw_sdk::UpdateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unimplemented!()
    }
    async fn delete_route(&self, _: SecurityContext, _: Uuid) -> Result<(), CanonicalError> {
        unimplemented!()
    }
    async fn resolve_proxy_target(
        &self,
        _: SecurityContext,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), CanonicalError> {
        unimplemented!("the adapter provisions rather than probing; see the module header")
    }
}

/// An issue document whose workflow *status* and status *category* disagree,
/// which is the whole point of reading the category.
const RESOLVED_ISSUE: &str = r#"{
  "key": "VHP-319",
  "fields": {
    "summary": "[VHP] Test Failed: tests/test_login.py::test_ok",
    "status": { "name": "Resolved", "statusCategory": { "key": "done" } }
  }
}"#;

// ---------------------------------------------------------------------------
// The alias derivation
// ---------------------------------------------------------------------------

/// The alias is the JIRA host, and the port only appears when it is not the
/// scheme's default.
///
/// This must agree with oagw's own derivation
/// (`gears/system/oagw/oagw/src/domain/services/management/alias.rs:67-100`) or
/// an upstream created here and one created for the same host anywhere else are
/// two different aliases — see the module header.
#[test]
fn the_alias_is_the_host_and_names_the_port_only_when_it_is_not_standard() {
    for (url, expected) in [
        ("https://jira.example.com/", "jira.example.com"),
        ("https://JIRA.Example.COM", "jira.example.com"),
        ("http://jira.example.com", "jira.example.com"),
        (
            "https://jira.example.com:8443/jira",
            "jira.example.com:8443",
        ),
        ("http://jira.example.com:80", "jira.example.com"),
    ] {
        assert_eq!(egress_for(url).expect(url).alias, expected, "for {url}");
    }
}

/// **A trailing-dot FQDN provisions under the alias oagw would derive, not one
/// it normalises away.** Phase C's final review.
///
/// `jira.example.com.` is a legal absolute hostname and a dot is in oagw's legal
/// alias charset, so `derive_alias` — which replicates oagw's `validate_alias`
/// and not its `normalize_alias` — accepted it verbatim. oagw strips trailing
/// dots from a host before deriving an alias
/// (`gears/system/oagw/oagw/src/domain/model.rs:58-64`,
/// `.../services/management/alias.rs:44-46`), so the upstream this adapter
/// created was addressed by a key oagw normalises away, and the in-process
/// provisioned cache made that permanent for the process lifetime: one tenant
/// with a trailing dot in its settings had a JIRA integration that could not be
/// fixed without a restart.
///
/// # What makes this test able to fail
///
/// The non-standard-port case is the one that would still be wrong if the strip
/// were applied to the finished alias instead of to the host:
/// `trim_end_matches('.')` on `jira.example.com.:8443` removes nothing. The
/// `uri_prefix` assertion is what pins that the *proxy path* — not merely the
/// derived string — carries the normalised alias, since that is the value oagw
/// actually resolves against.
#[test]
fn a_trailing_dot_fqdn_derives_the_alias_oagw_would_normalise_to() {
    for (url, expected) in [
        ("https://jira.example.com.", "jira.example.com"),
        ("https://jira.example.com./jira", "jira.example.com"),
        ("https://jira.example.com...", "jira.example.com"),
        ("https://JIRA.Example.COM.", "jira.example.com"),
        (
            "https://jira.example.com.:8443/jira",
            "jira.example.com:8443",
        ),
    ] {
        let egress = egress_for(url).expect(url);
        assert_eq!(egress.alias, expected, "for {url}");
        assert!(
            egress.uri_prefix().starts_with(&format!("/{expected}/")),
            "the proxy path must carry the normalised alias too, got {} for {url}",
            egress.uri_prefix(),
        );
    }

    // A host that is nothing *but* dots still names no host, rather than
    // normalising to an empty-but-valid one.
    match egress_for("https://./jira") {
        Err(DomainError::Validation { field, .. }) => assert_eq!(field, "url"),
        other => panic!("a dots-only host must be refused, got {other:?}"),
    }
}

/// **A JIRA context path survives into the route and the request URI.**
///
/// Fix round 1, finding 3. `https://host/jira` is the default for JIRA Data
/// Center behind a context path and legacy preserves it —
/// `format!("{}/rest/api/3/...", config.url.trim_end_matches('/'))`
/// (`manager/src/services/jira.rs:76`, `:144`, `:261-265`). An earlier revision
/// of `egress_for` kept only the authority, so every call 404'd.
///
/// # What makes this test able to fail
///
/// It asserts the **normalised path itself**, not merely that parsing succeeded,
/// and the sibling cases pin the two ways of getting the normalisation wrong: a
/// trailing slash must not become a path (or `https://h/` and `https://h` would
/// provision two different routes for one instance), and a bare host must stay
/// empty (or every ordinary JIRA Cloud URL would gain a spurious `/` segment).
#[test]
fn a_context_path_is_preserved_and_normalised() {
    for (url, expected) in [
        ("https://jira.example.com", ""),
        ("https://jira.example.com/", ""),
        ("https://jira.example.com///", ""),
        ("https://jira.example.com/jira", "/jira"),
        ("https://jira.example.com/jira/", "/jira"),
        ("https://jira.example.com:8443/apps/jira", "/apps/jira"),
        // A query or fragment on a *base* URL is meaningless and is dropped
        // before the path split, so neither can leak into a route path.
        ("https://jira.example.com/jira?x=1", "/jira"),
        ("https://jira.example.com/jira#frag", "/jira"),
    ] {
        assert_eq!(egress_for(url).expect(url).base_path, expected, "for {url}");
    }

    // The helper directly, for the two degenerate inputs the URLs above cannot
    // produce.
    assert_eq!(normalize_base_path(""), "");
    assert_eq!(normalize_base_path("/"), "");
}

/// The route path and the proxy URI are both built from the context path, which
/// is what makes oagw rebuild legacy's URL: the final upstream URL is
/// `endpoint + route_path + remaining_suffix`
/// (`gears/system/oagw/oagw/src/infra/proxy/service.rs:785-800`).
#[test]
fn the_route_prefix_and_uri_prefix_carry_the_context_path() {
    let plain = egress_for("https://jira.example.com").unwrap();
    assert_eq!(plain.api_prefix(), "/rest/api/3");
    assert_eq!(plain.uri_prefix(), "/jira.example.com/rest/api/3");

    let with_path = egress_for("https://jira.example.com/jira").unwrap();
    assert_eq!(with_path.api_prefix(), "/jira/rest/api/3");
    assert_eq!(with_path.uri_prefix(), "/jira.example.com/jira/rest/api/3");
}

/// A URL this adapter cannot turn into an egress endpoint is a `400` naming
/// `url` — the operator's own settings field — rather than a gateway error they
/// cannot act on.
#[test]
fn an_unusable_jira_url_is_a_validation_error_naming_the_url_field() {
    for url in [
        "jira.example.com",
        "ftp://jira.example.com",
        "https://",
        "https://user:pw@jira.example.com",
        "https://[::1]:8443",
        "https://jira.example.com:not-a-port",
    ] {
        match egress_for(url) {
            Err(DomainError::Validation { field, .. }) => assert_eq!(field, "url", "for {url}"),
            other => panic!("{url} must be refused naming `url`, got {other:?}"),
        }
    }
}

/// Legacy slices bytes (`jira.rs:115`), which panics mid-codepoint. This
/// truncates characters instead, and the test drives a log whose 2000-byte cut
/// lands inside a multi-byte character.
#[test]
fn the_log_excerpt_is_truncated_by_characters_and_never_panics_mid_codepoint() {
    let logs = "\u{e9}".repeat(3000);
    let excerpt = truncate_logs(&logs);
    assert_eq!(excerpt.chars().count(), 2000);
    assert!(
        logs.len() > 2000 && excerpt.len() > 2000,
        "the fixture must be multi-byte, or this test proves nothing about the boundary",
    );
}

/// **The two `"[VHP] Test Failed: {test_name}"` literals must not drift.**
///
/// Task 33's `domain::service::jira::bug_summary` builds the same string this
/// adapter sends JIRA as the issue summary, duplicated rather than shared
/// because the domain layer must not depend on this adapter
/// (`bug_summary`'s own doc). Nothing else would catch the two diverging: the
/// domain layer stores what it built, this adapter sends what it built, and
/// neither reads the other's value.
#[test]
fn the_stored_summary_matches_the_one_sent_to_jira() {
    let test_name = "AuthN Login";
    assert_eq!(
        summary_for(test_name),
        crate::domain::service::jira::bug_summary(test_name),
    );
}

/// The same pin, past the truncation boundary. `test_name` is `VARCHAR(512)`,
/// wide enough that a parameterised name routinely exceeds JIRA's
/// 255-character `summary` field; [`summary_for`] truncates for that reason
/// (this module's `the_summary_is_truncated_at_jiras_field_limit`). Before
/// this task, `bug_summary` did not, so the two literals — identical for any
/// short name, which is all [`the_stored_summary_matches_the_one_sent_to_jira`]
/// above exercises — diverged the moment a name pushed past the bound, and
/// the locally stored record claimed a summary JIRA never actually received.
#[test]
fn the_stored_summary_matches_the_one_sent_to_jira_past_the_truncation_boundary() {
    let test_name = "x".repeat(1000);
    assert_eq!(
        summary_for(&test_name),
        crate::domain::service::jira::bug_summary(&test_name),
    );
}

/// **This task's bug.** `test_name` is `VARCHAR(512)`, wider than JIRA's own
/// 255-character `summary` field; before this task, a parameterised name
/// anywhere near that width produced a summary JIRA refused with a 400 —
/// logged and skipped, so the failing test never got a bug at all.
/// Characters, not bytes, [`the_summary_truncation_never_panics_mid_codepoint`]'s
/// reason.
#[test]
fn the_summary_is_truncated_at_jiras_field_limit() {
    let test_name = "x".repeat(1000);
    let summary = summary_for(&test_name);
    assert_eq!(summary.chars().count(), 255);
    assert!(summary.starts_with("[VHP] Test Failed: "));
}

/// The same boundary as [`the_log_excerpt_is_truncated_by_characters_and_never_panics_mid_codepoint`],
/// on the summary instead of the log excerpt: a byte-based cut on a
/// multi-byte fixture would either panic or split a codepoint, and
/// `test_name` can carry non-ASCII from a parameterised test.
#[test]
fn the_summary_truncation_never_panics_mid_codepoint() {
    let test_name = "\u{e9}".repeat(1000);
    let summary = summary_for(&test_name);
    assert_eq!(summary.chars().count(), 255);
}

/// A summary that already fits needs no truncation at all — `.chars().take()`
/// is a no-op below the bound, so nothing about a normal test name changes.
#[test]
fn a_short_summary_is_not_truncated() {
    assert_eq!(summary_for("AuthN Login"), "[VHP] Test Failed: AuthN Login");
}

// ---------------------------------------------------------------------------
// REQUEST_TIMEOUT
// ---------------------------------------------------------------------------

/// The one test in this module that can actually fail on the timeout wiring:
/// if `send` stopped wrapping the proxy call in `tokio::time::timeout`, this
/// would hang instead of returning within the assertion's deadline. A
/// millisecond-scale bound stands in for the real ten seconds via
/// `OagwJiraClient::with_timeout` so the suite does not pay for it —
/// production code has no way to reach that constructor and always gets
/// [`OagwJiraClient::REQUEST_TIMEOUT`]. Mirrors
/// `infra::notify::slack_oagw_tests::the_bound_is_actually_applied_to_a_hanging_gateway`.
#[tokio::test]
async fn the_bound_is_actually_applied_to_a_hanging_gateway() {
    let client = OagwJiraClient::with_timeout(
        std::sync::Arc::new(FakeGateway::hanging()),
        std::time::Duration::from_millis(20),
    );

    let started = std::time::Instant::now();
    let result = client.get_issue(&ctx(TENANT), &config(), "VHP-1").await;
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "a hanging gateway must not be reported as a successful fetch"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "the call must return promptly once its own bound elapses, took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// Provisioning
// ---------------------------------------------------------------------------

/// **The upstream carries the credstore *reference*, never material.**
///
/// `qa_jira_config.api_token_credstore_ref` is a reference by construction
/// (Task 10), and this is the assertion that it reaches oagw as one: the apikey
/// auth plugin's `secret_ref` is resolved by oagw against credstore
/// (`gears/system/oagw/oagw/src/infra/plugin/apikey_auth.rs:49-54`), so the
/// material never enters this process. `Basic ` is the prefix because JIRA's
/// REST API takes HTTP basic auth (`jira.rs:81`, `:159`, `:269`).
#[tokio::test]
async fn the_provisioned_upstream_names_the_credstore_reference_and_the_basic_prefix() {
    let gateway = std::sync::Arc::new(FakeGateway::answering(RESOLVED_ISSUE));
    let client = OagwJiraClient::new(gateway.clone());

    client
        .check_status(&ctx(TENANT), &config(), "VHP-319")
        .await
        .unwrap();

    let upstreams = gateway.upstreams();
    assert_eq!(upstreams.len(), 1, "one upstream per tenant and host");
    let auth = upstreams[0].auth().expect("the upstream must carry auth");
    assert_eq!(auth.plugin_type, oagw_sdk::APIKEY_AUTH_PLUGIN_ID);
    let cfg = auth.config.as_ref().expect("the plugin must be configured");
    assert_eq!(cfg.get("secret_ref").map(String::as_str), Some(TOKEN_REF));
    assert_eq!(cfg.get("prefix").map(String::as_str), Some("Basic "));
    assert_eq!(
        cfg.get("header").map(String::as_str),
        Some("authorization"),
        "any other header name leaves JIRA unauthenticated",
    );
}

/// **No proxied request carries credential material, and it could not.**
///
/// oagw strips `authorization` from every proxied request unconditionally
/// (`gears/system/oagw/oagw/src/infra/proxy/headers.rs:13-18`, `:50-53`), so an
/// adapter that tried to send one would be silently unauthenticated rather than
/// insecure — but a *span* or a log built from the same value would still leak.
/// This asserts the adapter never assembles one at all.
#[tokio::test]
async fn no_proxied_request_carries_credential_material() {
    let gateway = std::sync::Arc::new(FakeGateway::answering(r#"{"key":"VHP-320"}"#));
    let client = OagwJiraClient::new(gateway.clone());

    client
        .create_or_find_issue(&ctx(TENANT), &config(), issue())
        .await
        .unwrap();

    for request in gateway.requests() {
        assert!(
            !request.header_names.iter().any(|h| h == "authorization"),
            "a proxied request carried an Authorization header: {request:?}",
        );
        assert!(
            !request.body.contains(TOKEN_REF),
            "a proxied request body carried the credential reference: {request:?}",
        );
    }
}

/// One upstream per `(tenant, host)`, whatever the call count: the in-process
/// cache is what keeps the steady state at one gateway call per JIRA request.
#[tokio::test]
async fn the_upstream_is_provisioned_once_and_then_cached() {
    let gateway = std::sync::Arc::new(FakeGateway::answering(RESOLVED_ISSUE));
    let client = OagwJiraClient::new(gateway.clone());
    let ctx = ctx(TENANT);

    for _ in 0..3 {
        client
            .check_status(&ctx, &config(), "VHP-319")
            .await
            .unwrap();
    }

    assert_eq!(gateway.upstreams().len(), 1);
    assert_eq!(gateway.routes().len(), 1);
    assert_eq!(gateway.requests().len(), 3, "every call still reaches JIRA");
}

/// **An upstream another replica already provisioned is reused *and its route is
/// ensured*.**
///
/// Fix round 1, finding 2, and controller ruling R81. This test previously
/// pinned the opposite — that the reuse path registers no route — which is what
/// made a restart after a failed route registration permanently fatal: the
/// upstream existed, `create_upstream` answered `AlreadyExists`, and nothing ever
/// created the route a proxy call requires
/// (`gears/system/oagw/oagw/src/domain/services/management/mod.rs:443-451`).
///
/// # What makes this test able to fail
///
/// `gateway.routes()` must be **non-empty**, which is the exact assertion the old
/// version inverted. Reverting the reuse arm to "log and carry on" turns this red.
#[tokio::test]
async fn an_upstream_that_already_exists_is_reused_and_its_route_is_ensured() {
    let mut fake = FakeGateway::answering(RESOLVED_ISSUE);
    fake.upstream_already_exists = true;
    fake.existing = vec![upstream_with_alias("jira.example.com")];
    let gateway = std::sync::Arc::new(fake);
    let client = OagwJiraClient::new(gateway.clone());

    let category = client
        .check_status(&ctx(TENANT), &config(), "VHP-319")
        .await
        .expect("AlreadyExists is a reuse, not a failure");

    assert!(category.is_resolved());
    assert_eq!(
        gateway.routes().len(),
        1,
        "a reused upstream must still get its route ensured, or a restart after a failed \
         route registration leaves this tenant's egress permanently dead",
    );
    assert_eq!(
        gateway.routes()[0].upstream_id(),
        Uuid::from_u128(0xBEEF),
        "the route must be registered against the upstream that was found, not a fresh id",
    );
    assert_eq!(
        gateway.list_calls(),
        1,
        "one lookup, on the conflict path only"
    );
}

/// **A route registration that fails is an error, and the next call retries.**
///
/// Fix round 1, finding 2. `ensure_route` used to log and swallow every failure
/// while `ensure_upstream` cached the key regardless, so one transient
/// `create_route` failure disabled that tenant's JIRA egress for the life of the
/// process.
///
/// # What makes this test able to fail
///
/// Two assertions, and they fail independently. The first goes red if the failure
/// is swallowed; the second — a **second** `create_upstream` on the next call —
/// goes red if the key is cached despite the failure, which is the half a
/// propagate-only fix would miss.
#[tokio::test]
async fn a_failed_route_registration_is_an_error_and_is_not_cached() {
    let mut fake = FakeGateway::answering(RESOLVED_ISSUE);
    fake.route_outcome = RouteOutcome::Fails;
    let gateway = std::sync::Arc::new(fake);
    let client = OagwJiraClient::new(gateway.clone());
    let ctx = ctx(TENANT);

    let err = client
        .check_status(&ctx, &config(), "VHP-319")
        .await
        .expect_err("a route this gear could not register is not a usable egress");
    assert!(matches!(err, DomainError::Internal(_)), "{err:?}");
    assert!(
        gateway.requests().is_empty(),
        "nothing should have been proxied through a route that does not exist",
    );

    // The second attempt fails the same way; what is under test is that it was
    // *attempted* at all.
    assert!(
        client
            .check_status(&ctx, &config(), "VHP-319")
            .await
            .is_err(),
        "the route still cannot be registered, so the second call fails too",
    );
    assert_eq!(
        gateway.create_calls_len(),
        2,
        "the key must not have been cached, or the failure is permanent for the life of \
         the process",
    );
}

/// A route that is **already** registered is success, not a failure — the
/// idempotent re-run `mini-chat` treats the same way
/// (`gears/mini-chat/mini-chat/src/infra/oagw_provisioning.rs:571-580`). Without
/// this arm, every second replica would refuse to talk to JIRA at all.
#[tokio::test]
async fn a_route_that_already_exists_is_not_a_failure_and_is_cached() {
    let mut fake = FakeGateway::answering(RESOLVED_ISSUE);
    fake.route_outcome = RouteOutcome::AlreadyExists;
    let gateway = std::sync::Arc::new(fake);
    let client = OagwJiraClient::new(gateway.clone());
    let ctx = ctx(TENANT);

    client
        .check_status(&ctx, &config(), "VHP-319")
        .await
        .unwrap();
    client
        .check_status(&ctx, &config(), "VHP-319")
        .await
        .unwrap();

    assert_eq!(
        gateway.create_calls_len(),
        1,
        "a duplicate route is the state this method promises, so the key is cached",
    );
}

/// A reused upstream this adapter cannot look up is **not** cached, so the next
/// call tries again rather than proxying forever through a route it never
/// confirmed.
#[tokio::test]
async fn a_reused_upstream_that_cannot_be_resolved_is_not_cached() {
    let mut fake = FakeGateway::answering(RESOLVED_ISSUE);
    fake.upstream_already_exists = true;
    // `existing` is empty: the conflict is real but the lookup finds nothing.
    let gateway = std::sync::Arc::new(fake);
    let client = OagwJiraClient::new(gateway.clone());
    let ctx = ctx(TENANT);

    client
        .check_status(&ctx, &config(), "VHP-319")
        .await
        .expect("the call proceeds - the route was almost certainly created with the upstream");
    client
        .check_status(&ctx, &config(), "VHP-319")
        .await
        .unwrap();

    assert_eq!(
        gateway.create_calls_len(),
        2,
        "an unresolved conflict must not be cached",
    );
    assert!(gateway.routes().is_empty());
}

/// The alias lookup pages, and terminates whether or not the alias is present —
/// `mini-chat`'s `find_upstream_by_alias` (`oagw_provisioning.rs:393-417`), whose
/// paging this follows. `ListQuery::default()` is `top: 50`, so the target sits
/// on the second page here.
#[tokio::test]
async fn the_alias_lookup_pages_past_a_full_first_page() {
    let mut fake = FakeGateway::answering(RESOLVED_ISSUE);
    fake.upstream_already_exists = true;
    fake.existing = (0..50)
        .map(|i| upstream_with_alias(&format!("other-{i}.example.com")))
        .chain(std::iter::once(upstream_with_alias("jira.example.com")))
        .collect();
    let gateway = std::sync::Arc::new(fake);
    let client = OagwJiraClient::new(gateway.clone());

    client
        .check_status(&ctx(TENANT), &config(), "VHP-319")
        .await
        .unwrap();

    assert!(
        gateway.list_calls() >= 2,
        "the lookup must have paged past the first full page, or an alias beyond page one \
         is invisible",
    );
    assert_eq!(gateway.routes().len(), 1, "and the route was still ensured");
}

/// **A tenant that moves its instance under a context path gets a second route.**
///
/// The in-process cache is keyed on `(tenant, alias, base_path)` and the base
/// path is load-bearing: `https://h` and `https://h/jira` share an alias, so a
/// key without the path would mark the second configuration as already
/// provisioned and its route would never be registered.
#[tokio::test]
async fn changing_the_context_path_provisions_a_second_route() {
    let gateway = std::sync::Arc::new(FakeGateway::answering(RESOLVED_ISSUE));
    let client = OagwJiraClient::new(gateway.clone());
    let ctx = ctx(TENANT);

    client
        .check_status(&ctx, &config(), "VHP-319")
        .await
        .unwrap();
    let moved = JiraConfig {
        url: "https://jira.example.com/jira".to_owned(),
        ..config()
    };
    client.check_status(&ctx, &moved, "VHP-319").await.unwrap();

    let paths: Vec<String> = gateway
        .routes()
        .iter()
        .map(|r| r.match_rules().http.as_ref().unwrap().path.clone())
        .collect();
    assert_eq!(paths, vec!["/rest/api/3", "/jira/rest/api/3"]);
    assert_eq!(
        gateway.requests()[1].uri,
        "/jira.example.com/jira/rest/api/3/issue/VHP-319",
        "and the request URI carries the context path too",
    );
}

/// **A credential reference the credential store could not resolve is refused
/// before anything is provisioned.**
///
/// Fix round 1, finding 1. `SecretRef` accepts `[a-zA-Z0-9_-]` only
/// (`gears/credstore/credstore-sdk/src/models.rs:42-83`), and this adapter copies
/// the stored value straight into the apikey plugin's `secret_ref`. Checked here
/// as well as at the `PUT` so a row stored before that rule existed fails with a
/// named field rather than as an opaque `PluginError::Internal` from inside oagw.
#[tokio::test]
async fn an_unresolvable_credential_reference_provisions_nothing() {
    let gateway = std::sync::Arc::new(FakeGateway::answering(RESOLVED_ISSUE));
    let client = OagwJiraClient::new(gateway.clone());
    let bad = JiraConfig {
        // The shape a URL-flavoured guess produces, and the one every fixture in
        // this task's first round carried.
        api_token_credstore_ref: "credstore://qa/jira/api-token".to_owned(),
        ..config()
    };

    let err = client
        .check_status(&ctx(TENANT), &bad, "VHP-319")
        .await
        .expect_err("a reference oagw cannot resolve is not a usable configuration");

    match err {
        DomainError::Validation { field, .. } => {
            assert_eq!(field, "api_token_credstore_ref");
        }
        other => panic!("expected a validation error naming the reference, got {other:?}"),
    }
    assert!(gateway.upstreams().is_empty());
    assert!(gateway.requests().is_empty());
}

/// The route admits the two query parameters the dedupe search sends and no
/// others, because oagw rejects an unlisted parameter outright
/// (`gears/system/oagw/oagw/src/infra/proxy/service.rs:604-621`) — so a missing
/// entry here is a 400 on every search.
#[tokio::test]
async fn the_route_admits_the_search_parameters_and_covers_both_methods() {
    let gateway = std::sync::Arc::new(FakeGateway::answering(RESOLVED_ISSUE));
    let client = OagwJiraClient::new(gateway.clone());

    client
        .check_status(&ctx(TENANT), &config(), "VHP-319")
        .await
        .unwrap();

    let routes = gateway.routes();
    let http = routes[0]
        .match_rules()
        .http
        .as_ref()
        .expect("the route must carry an HTTP match");
    assert_eq!(http.path, "/rest/api/3");
    assert_eq!(http.path_suffix_mode, oagw_sdk::PathSuffixMode::Append);
    assert_eq!(
        http.query_allowlist,
        vec!["jql".to_owned(), "maxResults".to_owned()],
    );
    assert!(http.methods.contains(&oagw_sdk::HttpMethod::Get));
    assert!(
        http.methods.contains(&oagw_sdk::HttpMethod::Post),
        "without POST the issue create is refused by the gateway",
    );
}

/// A disabled config is refused **before** any gateway call: legacy's
/// `"JIRA integration is disabled"` (`jira.rs:65-67`), and provisioning an
/// upstream for a tenant that turned the integration off would be egress
/// configuration nobody asked for.
#[tokio::test]
async fn a_disabled_config_reaches_the_gateway_not_at_all() {
    let gateway = std::sync::Arc::new(FakeGateway::answering(RESOLVED_ISSUE));
    let client = OagwJiraClient::new(gateway.clone());
    let disabled = JiraConfig {
        enabled: false,
        ..config()
    };

    let err = client
        .check_status(&ctx(TENANT), &disabled, "VHP-319")
        .await
        .expect_err("a disabled config cannot be called through");

    assert!(matches!(err, DomainError::JiraNotConfigured), "{err:?}");
    assert!(gateway.upstreams().is_empty());
    assert!(gateway.requests().is_empty());
}

// ---------------------------------------------------------------------------
// The three calls
// ---------------------------------------------------------------------------

/// **The category, not the status.** `check_jira_status` reads
/// `fields.status.statusCategory.key` (`jira.rs:279-286`) and the poller compares
/// it to `"done"` (`manager/src/services/jira_poller.rs:57`). The fixture's
/// workflow status is `"Resolved"`, so an adapter reading `status.name` would
/// return a value that is not `"done"` and this assertion would fail.
#[tokio::test]
async fn check_status_reads_the_status_category_and_not_the_workflow_status() {
    let gateway = std::sync::Arc::new(FakeGateway::answering(RESOLVED_ISSUE));
    let client = OagwJiraClient::new(gateway.clone());

    let category = client
        .check_status(&ctx(TENANT), &config(), "VHP-319")
        .await
        .unwrap();

    assert_eq!(category.as_str(), "done");
    assert!(category.is_resolved());

    let request = &gateway.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.uri, "/jira.example.com/rest/api/3/issue/VHP-319",
        "the URI must be /{{alias}}/{{path_suffix}} (oagw-sdk/src/api.rs:140-156)",
    );
}

/// A missing category is legacy's `"unknown"` (`jira.rs:285`) and is **not**
/// resolved — so an instance that answers a shape this gear does not recognise
/// leaves the bug open rather than closing it silently.
#[tokio::test]
async fn a_missing_status_category_is_unknown_and_not_resolved() {
    let gateway = std::sync::Arc::new(FakeGateway::answering(r#"{"fields":{"summary":"x"}}"#));
    let client = OagwJiraClient::new(gateway);

    let found = client
        .get_issue(&ctx(TENANT), &config(), "VHP-1")
        .await
        .unwrap();

    assert_eq!(
        found.key, "VHP-1",
        "the key is the caller's, not the body's"
    );
    assert_eq!(found.summary, "x");
    assert_eq!(found.status_category.as_str(), "unknown");
    assert!(!found.status_category.is_resolved());
}

/// A non-2xx from the issue read is an error — legacy's `jira.rs:274-276`.
#[tokio::test]
async fn a_refused_issue_read_is_an_error() {
    let gateway = std::sync::Arc::new(FakeGateway::new(
        Scripted {
            status: 200,
            body: r#"{"issues":[]}"#.to_owned(),
        },
        Scripted {
            status: 404,
            body: r#"{"errorMessages":["Issue does not exist"]}"#.to_owned(),
        },
    ));
    let client = OagwJiraClient::new(gateway);

    let err = client
        .check_status(&ctx(TENANT), &config(), "VHP-999")
        .await
        .expect_err("a 404 from JIRA is a failure");
    assert!(matches!(err, DomainError::Internal(_)), "{err:?}");
}

/// **Search first, then create** — the order legacy fixes (`jira.rs:70-108`
/// before `:144-174`), and the JQL is legacy's own predicate.
#[tokio::test]
async fn a_filing_searches_before_it_creates_and_reports_created() {
    let gateway = std::sync::Arc::new(FakeGateway::answering(r#"{"key":"VHP-320"}"#));
    let client = OagwJiraClient::new(gateway.clone());

    let filed = client
        .create_or_find_issue(&ctx(TENANT), &config(), issue())
        .await
        .unwrap();

    assert_eq!(filed.jira_key, "VHP-320");
    assert!(filed.created);

    let requests = gateway.requests();
    assert_eq!(requests.len(), 2, "one search, one create: {requests:?}");
    assert_eq!(requests[0].method, "GET");
    assert!(
        requests[0]
            .uri
            .starts_with("/jira.example.com/rest/api/3/search?"),
        "{:?}",
        requests[0],
    );
    assert!(
        requests[0]
            .uri
            .contains("labels+%3D+%22vhp-test-failure%22"),
        "the JQL must carry legacy's label predicate verbatim: {:?}",
        requests[0],
    );
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].uri, "/jira.example.com/rest/api/3/issue");
    assert!(
        requests[1].body.contains("[VHP] Test Failed: "),
        "{:?}",
        requests[1],
    );
    assert!(
        requests[1].body.contains("\"name\":\"Bug\""),
        "an absent issue_type is legacy's Bug (jira.rs:112): {:?}",
        requests[1],
    );
}

/// A search hit short-circuits the create, and reports `created: false` —
/// legacy's `jira.rs:103-104`.
#[tokio::test]
async fn a_search_hit_is_reused_and_nothing_is_created() {
    let gateway = std::sync::Arc::new(FakeGateway::new(
        Scripted {
            status: 200,
            body: r#"{"issues":[{"key":"VHP-7"}]}"#.to_owned(),
        },
        Scripted {
            status: 200,
            body: r#"{"key":"VHP-SHOULD-NOT-BE-CREATED"}"#.to_owned(),
        },
    ));
    let client = OagwJiraClient::new(gateway.clone());

    let filed = client
        .create_or_find_issue(&ctx(TENANT), &config(), issue())
        .await
        .unwrap();

    assert_eq!(filed.jira_key, "VHP-7");
    assert!(!filed.created);
    assert_eq!(
        gateway.requests().len(),
        1,
        "a dedupe hit must not post an issue",
    );
}

/// **A refused search files the issue rather than dropping the report.**
///
/// Legacy guards the whole search block with
/// `if search_resp.status().is_success()` and falls through to the create
/// (`jira.rs:86-108`). Duplicating an issue is recoverable; losing a bug report
/// because a search was refused is not.
#[tokio::test]
async fn a_refused_search_still_files_the_issue() {
    let gateway = std::sync::Arc::new(FakeGateway::new(
        Scripted {
            status: 503,
            body: String::new(),
        },
        Scripted {
            status: 201,
            body: r#"{"key":"VHP-321"}"#.to_owned(),
        },
    ));
    let client = OagwJiraClient::new(gateway.clone());

    let filed = client
        .create_or_find_issue(&ctx(TENANT), &config(), issue())
        .await
        .unwrap();

    assert_eq!(filed.jira_key, "VHP-321");
    assert!(filed.created);
    assert_eq!(gateway.requests().len(), 2);
}

/// **A non-JSON 200 from the dedupe search still files the issue — and,
/// unlike before the fix, says why nothing was found.** Review finding #28.
///
/// Before the fix, `search_for_open_issue`'s
/// `serde_json::from_slice(&body).ok()?` dropped a body that failed to parse
/// with nothing written down: indistinguishable, to an operator, from a
/// legitimate "no matching issue" search result, and the poller's next act on
/// a *real* duplicate storm is to re-file a bug it believes is still open.
///
/// # What this test can and cannot show
///
/// The observable **behaviour** this test drives — a search miss still files
/// a new issue — is unchanged by the fix: `None` from the search was always
/// legacy's own fall-through (see `a_refused_search_still_files_the_issue`
/// just above), and still is. What changed is only the `warn!` line the fix
/// adds. This crate carries no log-capture harness — `qa-product-sdk`'s
/// `testing` module is `assert_no_leak`/`Canary`, leak canaries rather than
/// line captures, and no `tracing-test`/`tracing-subscriber` dev-dependency
/// exists here the way qa-environments' does (that crate's own `Cargo.toml`
/// explains why it built a raw-`tracing`-output capture instead of adopting
/// `tracing-test`). Task 9's brief is explicit that this crate must not grow
/// one just for this test, so the `warn!` text itself is asserted nowhere:
/// this test pins the control flow only, and this doc says so rather than
/// implying a red/green pair that a log assertion would have given it.
#[tokio::test]
async fn a_non_json_search_body_still_files_the_issue() {
    let gateway = std::sync::Arc::new(FakeGateway::new(
        Scripted {
            status: 200,
            body: "<html>gateway error</html>".to_owned(),
        },
        Scripted {
            status: 201,
            body: r#"{"key":"VHP-322"}"#.to_owned(),
        },
    ));
    let client = OagwJiraClient::new(gateway.clone());

    let filed = client
        .create_or_find_issue(&ctx(TENANT), &config(), issue())
        .await
        .unwrap();

    assert_eq!(filed.jira_key, "VHP-322");
    assert!(filed.created);
    assert_eq!(
        gateway.requests().len(),
        2,
        "one search (whose body could not be parsed), one create",
    );
}

/// A create JIRA accepts but answers without a key is an error, not a silently
/// unregistered issue — legacy's `"No key in JIRA response"` (`jira.rs:174`).
#[tokio::test]
async fn a_create_whose_response_has_no_key_is_an_error() {
    let gateway = std::sync::Arc::new(FakeGateway::answering(r#"{"id":"10001"}"#));
    let client = OagwJiraClient::new(gateway);

    let err = client
        .create_or_find_issue(&ctx(TENANT), &config(), issue())
        .await
        .expect_err("an issue whose key cannot be read cannot be registered");
    assert!(matches!(err, DomainError::Internal(_)), "{err:?}");
}
