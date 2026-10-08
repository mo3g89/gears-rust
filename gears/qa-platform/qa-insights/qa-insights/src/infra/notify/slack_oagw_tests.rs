//! Tests for the oagw Slack adapter.
//!
//! One [`FakeGateway`], scripted per test, in the same shape as
//! `infra::jira::oagw_client_tests::FakeGateway`: what is under test is the
//! *request this gear sends*, the *provisioning it performs*, the *identity it
//! sends as* and — because the webhook's path is the credential — **what its
//! errors do not say**.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use credstore_sdk::SharingMode;
use credstore_sdk::test_util::MockCredStoreClient;
use toolkit::api::canonical_prelude::*;
use uuid::Uuid;

use super::*;
use crate::domain::error::EgressFailure;
use crate::domain::ports::SlackBlock;
use crate::domain::system_actor::{self, TenantBound};
use crate::infra::notify::UNREADABLE_SECRET_HINT;
use crate::infra::notify::test_credstores::{
    DenyingCredStore, HangingCredStore, SharingCredStore, StoredSecret,
};

#[resource_error(gts_id!("cf.core.oagw.upstream.v1~"))]
struct TestUpstreamScope;

/// The reference every test's message carries, and the secret it resolves to.
const HOOK_REF: &str = "slack-hook";
const GOOD: &str = "https://hooks.slack.com/services/T000/B000/XXXXXXXX";
/// What `oagw` is asked to proxy for [`GOOD`].
const GOOD_PROXY_PATH: &str = "/hooks.slack.com/services/T000/B000/XXXXXXXX";

fn message() -> SlackMessage {
    SlackMessage {
        webhook_credstore_ref: HOOK_REF.to_owned(),
        channel: Some("#qa-alerts".to_owned()),
        text: "build failed".to_owned(),
        blocks: Vec::new(),
    }
}

/// The tenant every test sends as, unless a test is specifically about a
/// *different* tenant. Not [`SecurityContext::anonymous`] — `send`'s `ctx`
/// parameter exists precisely so this adapter never has to fall back to that.
const TENANT: Uuid = Uuid::from_u128(0x51ACC);

fn ctx_for(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xDEAD))
        .subject_tenant_id(tenant)
        .build()
        .expect("subject_id and subject_tenant_id are both set")
}

fn ctx() -> SecurityContext {
    ctx_for(TENANT)
}

fn credstore_with(secret: &str) -> Arc<dyn CredStoreClientV1> {
    Arc::new(MockCredStoreClient::with_secrets(vec![(
        HOOK_REF.to_owned(),
        secret.to_owned(),
    )]))
}

/// A client over `gateway` whose credential store holds [`GOOD`].
fn client(gateway: &Arc<FakeGateway>) -> SlackOagwClient {
    SlackOagwClient::new(
        Arc::clone(gateway) as Arc<dyn ServiceGatewayClientV1>,
        credstore_with(GOOD),
    )
}

/// One proxied request, flattened so the assertions can read it without a
/// non-`Clone` body in the way.
#[derive(Clone, Debug)]
struct Proxied {
    method: String,
    uri: String,
    /// Every request header name, lowercased — what
    /// `no_proxied_request_carries_a_credential_header` reads.
    header_names: Vec<String>,
    body: String,
}

/// How [`FakeGateway::proxy_request`] answers.
enum ProxyBehavior {
    /// Answer with the given status and an empty body.
    Answer(u16),
    /// Answer the way `oagw` does for a response it generated itself: the
    /// given status with the `ErrorSource::Gateway` response extension.
    GatewayAnswer(u16),
    /// Never resolve — the black-holing webhook host legacy's own comment
    /// describes.
    Hang,
    /// Fail the way `oagw` does when *its* request bound elapses: a
    /// `DeadlineExceeded` whose detail carries the full upstream URL
    /// (`oagw/src/infra/proxy/service.rs`, "request to {url} timed out").
    GatewayTimeout,
    /// Fail the way `oagw` does on a transport error, again with the URL in
    /// the detail.
    GatewayUnavailable,
}

/// A scripted [`ServiceGatewayClientV1`].
///
/// `create_upstream` succeeds by default and can be told to answer
/// `AlreadyExists` or fail; `create_route` and `list_upstreams` record and
/// answer from what was created. Everything the adapter must never call
/// panics.
struct FakeGateway {
    behavior: ProxyBehavior,
    upstream_already_exists: bool,
    upstream_fails: bool,
    create_calls: Mutex<Vec<oagw_sdk::CreateUpstreamRequest>>,
    route_calls: Mutex<Vec<oagw_sdk::CreateRouteRequest>>,
    list_calls: Mutex<u32>,
    /// What `list_upstreams` pages over — the upstreams that "already exist".
    existing: Vec<oagw_sdk::Upstream>,
    /// What `resolve_proxy_target` answers: the upstream `create_upstream`
    /// made, or the one a test planted (an existing or re-pointed upstream).
    /// `None` resolves to `NotFound`.
    resolved: Mutex<Option<oagw_sdk::Upstream>>,
    /// Plugins on the route `resolve_proxy_target` answers with.
    route_plugins: Option<oagw_sdk::PluginsConfig>,
    proxied: Mutex<Vec<Proxied>>,
    /// The `subject_tenant_id` of every `SecurityContext` `proxy_request` was
    /// called with — what
    /// [`the_slack_client_forwards_the_callers_security_context`] reads.
    ctx_tenants: Mutex<Vec<Uuid>>,
}

impl FakeGateway {
    fn with(behavior: ProxyBehavior) -> Self {
        Self {
            behavior,
            upstream_already_exists: false,
            upstream_fails: false,
            create_calls: Mutex::new(Vec::new()),
            route_calls: Mutex::new(Vec::new()),
            list_calls: Mutex::new(0),
            existing: Vec::new(),
            resolved: Mutex::new(None),
            route_plugins: None,
            proxied: Mutex::new(Vec::new()),
            ctx_tenants: Mutex::new(Vec::new()),
        }
    }

    fn answering(status: u16) -> Self {
        Self::with(ProxyBehavior::Answer(status))
    }

    fn hanging() -> Self {
        Self::with(ProxyBehavior::Hang)
    }

    fn requests(&self) -> Vec<Proxied> {
        self.proxied.lock().unwrap().clone()
    }

    fn tenants(&self) -> Vec<Uuid> {
        self.ctx_tenants.lock().unwrap().clone()
    }

    fn upstreams(&self) -> Vec<oagw_sdk::CreateUpstreamRequest> {
        self.create_calls.lock().unwrap().clone()
    }

    fn routes(&self) -> Vec<oagw_sdk::CreateRouteRequest> {
        self.route_calls.lock().unwrap().clone()
    }

    /// Plant `upstream` as both an existing one (so `create_upstream` answers
    /// `AlreadyExists` and the lookup finds it) and the resolved target.
    fn with_existing(mut self, upstream: oagw_sdk::Upstream) -> Self {
        self.upstream_already_exists = true;
        self.existing = vec![upstream.clone()];
        *self.resolved.lock().unwrap() = Some(upstream);
        self
    }

    /// Delete the upstream (and with it the route), as an operator would.
    fn delete_target(&self) {
        *self.resolved.lock().unwrap() = None;
    }

    /// Re-point the resolved target, as an edit made after this process
    /// cached the tenant would.
    fn repoint(&self, upstream: oagw_sdk::Upstream) {
        *self.resolved.lock().unwrap() = Some(upstream);
    }
}

/// The one endpoint the adapter provisions and accepts.
fn slack_endpoint() -> oagw_sdk::Endpoint {
    oagw_sdk::Endpoint {
        scheme: oagw_sdk::Scheme::Https,
        host: "hooks.slack.com".to_owned(),
        port: 443,
    }
}

/// An upstream aliased `hooks.slack.com` with the given endpoints and auth.
fn slack_alias_upstream(
    endpoints: Vec<oagw_sdk::Endpoint>,
    auth: Option<oagw_sdk::AuthConfig>,
) -> oagw_sdk::Upstream {
    oagw_sdk::Upstream {
        server: oagw_sdk::Server { endpoints },
        auth,
        ..upstream_with_alias("hooks.slack.com")
    }
}

/// The upstream the adapter itself would have provisioned.
fn genuine_upstream() -> oagw_sdk::Upstream {
    slack_alias_upstream(vec![slack_endpoint()], None)
}

/// The same alias, aimed somewhere else — the pre-created attacker upstream.
fn elsewhere_upstream() -> oagw_sdk::Upstream {
    slack_alias_upstream(
        vec![oagw_sdk::Endpoint {
            scheme: oagw_sdk::Scheme::Https,
            host: "evil.test".to_owned(),
            port: 443,
        }],
        None,
    )
}

/// An upstream row, as `create_upstream`/`list_upstreams` answer it.
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

#[async_trait]
impl ServiceGatewayClientV1 for FakeGateway {
    async fn create_upstream(
        &self,
        _: SecurityContext,
        req: oagw_sdk::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        let alias = req.alias().unwrap_or_default().to_owned();
        let created = oagw_sdk::Upstream {
            server: req.server().clone(),
            auth: req.auth().cloned(),
            ..upstream_with_alias(&alias)
        };
        self.create_calls.lock().unwrap().push(req);
        if self.upstream_fails {
            return Err(CanonicalError::service_unavailable()
                .with_detail("upstream store unavailable")
                .create());
        }
        if self.upstream_already_exists {
            return Err(TestUpstreamScope::already_exists("upstream already exists")
                .with_resource(alias)
                .create());
        }
        *self.resolved.lock().unwrap() = Some(created.clone());
        Ok(created)
    }

    async fn get_upstream(
        &self,
        _: SecurityContext,
        _: Uuid,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unreachable!("SlackOagwClient never reads an upstream by id")
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
        unreachable!("SlackOagwClient never updates an upstream")
    }

    async fn delete_upstream(&self, _: SecurityContext, _: Uuid) -> Result<(), CanonicalError> {
        unreachable!("SlackOagwClient never deletes an upstream")
    }

    async fn create_route(
        &self,
        _: SecurityContext,
        req: oagw_sdk::CreateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        let upstream_id = req.upstream_id();
        let match_rules = req.match_rules().clone();
        self.route_calls.lock().unwrap().push(req);
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

    async fn get_route(
        &self,
        _: SecurityContext,
        _: Uuid,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unreachable!("SlackOagwClient never reads a route")
    }

    async fn list_routes(
        &self,
        _: SecurityContext,
        _: Option<Uuid>,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Route>, CanonicalError> {
        unreachable!("SlackOagwClient never lists routes")
    }

    async fn update_route(
        &self,
        _: SecurityContext,
        _: Uuid,
        _: oagw_sdk::UpdateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unreachable!("SlackOagwClient never updates a route")
    }

    async fn delete_route(&self, _: SecurityContext, _: Uuid) -> Result<(), CanonicalError> {
        unreachable!("SlackOagwClient never deletes a route")
    }

    async fn resolve_proxy_target(
        &self,
        _: SecurityContext,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), CanonicalError> {
        let Some(upstream) = self.resolved.lock().unwrap().clone() else {
            return Err(TestUpstreamScope::not_found("no upstream for alias")
                .with_resource("hooks.slack.com")
                .create());
        };
        let route = oagw_sdk::Route {
            id: Uuid::from_u128(0xF00D),
            tenant_id: TENANT,
            upstream_id: upstream.id,
            match_rules: oagw_sdk::MatchRules {
                http: None,
                grpc: None,
            },
            plugins: self.route_plugins.clone(),
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
        };
        Ok((upstream, route))
    }

    async fn proxy_request(
        &self,
        ctx: SecurityContext,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, CanonicalError> {
        self.ctx_tenants
            .lock()
            .unwrap()
            .push(ctx.subject_tenant_id());

        if matches!(self.behavior, ProxyBehavior::Hang) {
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
        self.proxied.lock().unwrap().push(Proxied {
            method,
            uri: uri.clone(),
            header_names,
            body,
        });

        // What oagw's own error text looks like: the full upstream URL, path
        // (= the credential) included.
        let url = format!(
            "https://hooks.slack.com:443{}",
            uri.trim_start_matches("/hooks.slack.com")
        );
        match self.behavior {
            ProxyBehavior::Answer(status) => Ok(http::Response::builder()
                .status(status)
                .body(Body::from(""))
                .expect("the fake response builds")),
            ProxyBehavior::GatewayAnswer(status) => {
                let mut resp = http::Response::builder()
                    .status(status)
                    .body(Body::from(""))
                    .expect("the fake response builds");
                resp.extensions_mut()
                    .insert(oagw_sdk::api::ErrorSource::Gateway);
                Ok(resp)
            }
            ProxyBehavior::GatewayTimeout => Err(TestUpstreamScope::deadline_exceeded(format!(
                "request to {url} timed out after 30s"
            ))
            .create()),
            ProxyBehavior::GatewayUnavailable => Err(CanonicalError::service_unavailable()
                .with_detail(format!("proxy bridge error: connect to {url} refused"))
                .create()),
            ProxyBehavior::Hang => unreachable!("Hang already returned above"),
        }
    }
}

/// The error must be `UpstreamEgress` on the Slack channel with `failure`, and
/// neither its rendering nor its `Debug` may carry any part of the webhook
/// path.
fn assert_redacted_egress(error: &DomainError, failure: EgressFailure) {
    let DomainError::UpstreamEgress {
        channel,
        endpoint,
        failure: got,
        ..
    } = error
    else {
        panic!("expected UpstreamEgress, got {error:?}");
    };
    assert_eq!(channel, "slack");
    assert_eq!(endpoint, "hooks.slack.com");
    assert_eq!(*got, failure, "{error:?}");
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(
            !rendered.contains("XXXXXXXX"),
            "leaked the token: {rendered}"
        );
        assert!(
            !rendered.contains("/services/T000"),
            "leaked the path: {rendered}"
        );
    }
}

// ---------------------------------------------------------------------------
// REQUEST_TIMEOUT
// ---------------------------------------------------------------------------

/// The one test in this module that can actually fail on the timeout wiring:
/// if `send` stopped wrapping the proxy call in `tokio::time::timeout`, this
/// would hang instead of returning within the assertion's deadline. A
/// millisecond-scale bound stands in for the real ten seconds via
/// [`SlackOagwClient::with_timeout`] so the suite does not pay for it —
/// production code has no way to reach that constructor and always gets
/// [`SlackOagwClient::REQUEST_TIMEOUT`].
#[tokio::test]
async fn the_bound_is_actually_applied_to_a_hanging_gateway() {
    let client = SlackOagwClient::with_timeout(
        Arc::new(FakeGateway::hanging()),
        credstore_with(GOOD),
        Duration::from_millis(20),
    );

    let started = std::time::Instant::now();
    let result = client.send(&ctx(), &message()).await;
    let elapsed = started.elapsed();

    let error = result.expect_err("a hanging gateway must not be reported as sent");
    assert_redacted_egress(&error, EgressFailure::Timeout);
    assert!(
        elapsed < Duration::from_secs(1),
        "send must return promptly once its own bound elapses, took {elapsed:?}"
    );
}

/// **The credential lookup is inside the bound.** A credential store
/// that never answers used to stall `send` before its timeout began.
#[tokio::test]
async fn the_bound_covers_a_credential_store_that_never_answers() {
    let gateway = Arc::new(FakeGateway::answering(200));
    let client = SlackOagwClient::with_timeout(
        Arc::clone(&gateway) as Arc<dyn ServiceGatewayClientV1>,
        Arc::new(HangingCredStore),
        Duration::from_millis(20),
    );

    let started = std::time::Instant::now();
    let error = client
        .send(&ctx(), &message())
        .await
        .expect_err("a hanging credential store must not be reported as sent");

    assert_redacted_egress(&error, EgressFailure::Timeout);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "took {:?}",
        started.elapsed()
    );
    assert!(gateway.requests().is_empty(), "nothing was dialled");
}

// ---------------------------------------------------------------------------
// Delivery: resolve, validate, provision, proxy
// ---------------------------------------------------------------------------

/// The webhook the credential store holds is what `oagw` is asked to proxy,
/// under the `hooks.slack.com` upstream alias — the fix for what used to be
/// "Finding B", where the credstore reference itself was sent as the path.
#[tokio::test]
async fn the_resolved_webhook_is_proxied_under_the_slack_upstream() {
    let gateway = Arc::new(FakeGateway::answering(200));

    let outcome = client(&gateway)
        .send(&ctx(), &message())
        .await
        .expect("the fake gateway answers 200");
    assert_eq!(outcome, SendOutcome::Sent);

    let proxied = gateway.requests();
    assert_eq!(proxied.len(), 1);
    assert_eq!(proxied[0].method, "POST");
    assert_eq!(proxied[0].uri, GOOD_PROXY_PATH);
}

/// One upstream and one route per tenant, however many sends: the upstream is
/// `hooks.slack.com` over HTTPS on 443 with **no auth** (the credential is the
/// path, which this adapter supplies), and the route admits `POST /services…`
/// with no query parameters.
#[tokio::test]
async fn the_slack_upstream_and_route_are_provisioned_once_per_tenant() {
    let gateway = Arc::new(FakeGateway::answering(200));
    let client = client(&gateway);

    client.send(&ctx(), &message()).await.expect("first send");
    client.send(&ctx(), &message()).await.expect("second send");

    let upstreams = gateway.upstreams();
    assert_eq!(upstreams.len(), 1, "provisioned once for the tenant");
    let upstream = &upstreams[0];
    assert_eq!(upstream.alias(), Some("hooks.slack.com"));
    assert_eq!(upstream.protocol(), oagw_sdk::HTTP_PROTOCOL_ID);
    assert!(
        upstream.auth().is_none(),
        "the Slack upstream carries no auth"
    );
    assert!(upstream.enabled());
    assert_eq!(
        upstream.server().endpoints,
        vec![oagw_sdk::Endpoint {
            scheme: oagw_sdk::Scheme::Https,
            host: "hooks.slack.com".to_owned(),
            port: 443,
        }]
    );

    let routes = gateway.routes();
    assert_eq!(routes.len(), 1, "one route for the tenant");
    let http = routes[0]
        .match_rules()
        .http
        .as_ref()
        .expect("an HTTP route");
    assert_eq!(http.methods, vec![oagw_sdk::HttpMethod::Post]);
    assert_eq!(http.path, "/services");
    assert!(http.query_allowlist.is_empty());
    assert_eq!(http.path_suffix_mode, oagw_sdk::PathSuffixMode::Append);
    assert_eq!(gateway.requests().len(), 2);

    // The cache is per tenant: a second tenant provisions its own.
    client
        .send(&ctx_for(Uuid::from_u128(0x0B)), &message())
        .await
        .expect("another tenant's send");
    assert_eq!(gateway.upstreams().len(), 2);
    assert_eq!(gateway.routes().len(), 2);
}

/// An upstream that already exists (another replica, an earlier process) is
/// found by alias and its route ensured, and the send goes through.
#[tokio::test]
async fn an_existing_upstream_is_reused() {
    let gateway = Arc::new(FakeGateway::answering(200).with_existing(genuine_upstream()));

    let outcome = client(&gateway)
        .send(&ctx(), &message())
        .await
        .expect("the existing upstream is reused");
    assert_eq!(outcome, SendOutcome::Sent);
    assert_eq!(*gateway.list_calls.lock().unwrap(), 1);
    assert_eq!(
        gateway.routes().len(),
        1,
        "the route is ensured on the reused upstream"
    );
    assert_eq!(gateway.routes()[0].upstream_id(), Uuid::from_u128(0xBEEF));
    assert_eq!(gateway.requests()[0].uri, GOOD_PROXY_PATH);
}

/// **Fix round 1.** An upstream aliased `hooks.slack.com` that points
/// anywhere else — pre-created by someone with oagw upstream rights but no
/// credential-store read — is refused: no route is registered on it, and the
/// webhook path is never proxied.
#[tokio::test]
async fn an_existing_upstream_pointing_elsewhere_is_refused_and_nothing_is_proxied() {
    let gateway = Arc::new(FakeGateway::answering(200).with_existing(elsewhere_upstream()));

    let error = client(&gateway)
        .send(&ctx(), &message())
        .await
        .expect_err("an upstream aimed elsewhere must not carry the webhook");
    assert_redacted_egress(&error, EgressFailure::Unreachable);
    assert!(!format!("{error} {error:?}").contains("evil"), "{error:?}");
    assert!(gateway.requests().is_empty(), "nothing was proxied");
    assert!(
        gateway.routes().is_empty(),
        "no route on a foreign upstream"
    );
}

/// The same for the right endpoint with an auth plugin attached: this gear
/// provisions the Slack upstream with no auth, so one that has it is not
/// this gear's.
#[tokio::test]
async fn an_existing_upstream_with_auth_is_refused_and_nothing_is_proxied() {
    let with_auth = slack_alias_upstream(
        vec![slack_endpoint()],
        Some(oagw_sdk::AuthConfig {
            plugin_type: oagw_sdk::APIKEY_AUTH_PLUGIN_ID.to_owned(),
            sharing: oagw_sdk::SharingMode::Private,
            config: None,
        }),
    );
    let gateway = Arc::new(FakeGateway::answering(200).with_existing(with_auth));

    let error = client(&gateway)
        .send(&ctx(), &message())
        .await
        .expect_err("an upstream with auth is not the one this gear provisions");
    assert_redacted_egress(&error, EgressFailure::Unreachable);
    assert!(gateway.requests().is_empty());
}

/// The check runs on every send, not only while provisioning: a tenant this
/// process has cached whose upstream is re-pointed afterwards is refused on
/// the next send.
#[tokio::test]
async fn a_cached_tenants_repointed_upstream_is_refused_on_the_next_send() {
    let gateway = Arc::new(FakeGateway::answering(200));
    let client = client(&gateway);
    client.send(&ctx(), &message()).await.expect("first send");

    gateway.repoint(elsewhere_upstream());
    let error = client
        .send(&ctx(), &message())
        .await
        .expect_err("the re-pointed upstream must be refused");
    assert_redacted_egress(&error, EgressFailure::Unreachable);
    assert_eq!(
        gateway.requests().len(),
        1,
        "only the first send was proxied"
    );
    assert_eq!(gateway.upstreams().len(), 1, "the tenant stayed cached");
}

/// A cached tenant whose upstream was deleted is refused once, and the next
/// send re-provisions it and succeeds.
#[tokio::test]
async fn a_cached_tenants_deleted_upstream_is_reprovisioned_on_the_next_send() {
    let gateway = Arc::new(FakeGateway::answering(200));
    let client = client(&gateway);
    client.send(&ctx(), &message()).await.expect("first send");
    assert_eq!(gateway.upstreams().len(), 1);

    gateway.delete_target();
    let error = client
        .send(&ctx(), &message())
        .await
        .expect_err("a missing target must be refused");
    assert_redacted_egress(&error, EgressFailure::Unreachable);
    assert_eq!(gateway.requests().len(), 1, "the refused send was not proxied");

    client
        .send(&ctx(), &message())
        .await
        .expect("the next send re-provisions and succeeds");
    assert_eq!(gateway.upstreams().len(), 2, "the tenant was provisioned again");
    assert_eq!(gateway.requests().len(), 2);
}

/// A foreign upstream is never evicted-and-recreated around: it is refused on
/// every send, and nothing is re-provisioned.
#[tokio::test]
async fn a_foreign_upstream_stays_refused_on_every_send_without_reprovisioning() {
    let gateway = Arc::new(FakeGateway::answering(200));
    let client = client(&gateway);
    client.send(&ctx(), &message()).await.expect("first send");
    gateway.repoint(elsewhere_upstream());
    for _ in 0..3 {
        let error = client
            .send(&ctx(), &message())
            .await
            .expect_err("refused");
        assert_redacted_egress(&error, EgressFailure::Unreachable);
    }
    assert_eq!(gateway.upstreams().len(), 1, "no re-provisioning");
    assert_eq!(gateway.requests().len(), 1);
}

/// A matched route carrying plugins is refused: a plugin can rewrite the
/// request this gear hands over.
#[tokio::test]
async fn a_route_with_plugins_is_refused_and_nothing_is_proxied() {
    let mut fake = FakeGateway::answering(200);
    fake.route_plugins = Some(oagw_sdk::PluginsConfig {
        sharing: oagw_sdk::SharingMode::Private,
        items: vec![oagw_sdk::PluginBinding {
            plugin_ref: "gts.x.transform".to_owned(),
            config: std::collections::HashMap::new(),
        }],
    });
    let gateway = Arc::new(fake);

    let error = client(&gateway)
        .send(&ctx(), &message())
        .await
        .expect_err("a route with plugins is refused");
    assert_redacted_egress(&error, EgressFailure::Unreachable);
    assert!(gateway.requests().is_empty());
}

/// A gateway that cannot provision the upstream fails the send as an egress
/// failure, and nothing is proxied.
#[tokio::test]
async fn a_provisioning_failure_is_an_egress_failure_and_nothing_is_proxied() {
    let mut fake = FakeGateway::answering(200);
    fake.upstream_fails = true;
    let gateway = Arc::new(fake);

    let error = client(&gateway)
        .send(&ctx(), &message())
        .await
        .expect_err("the upstream could not be provisioned");
    assert_redacted_egress(&error, EgressFailure::Unreachable);
    assert!(gateway.requests().is_empty());
}

/// A reference the tenant's credential store does not answer is the settings
/// page's problem, named as such — and nothing is dialled.
#[tokio::test]
async fn an_unresolvable_reference_is_a_validation_error_and_nothing_is_dialled() {
    let gateway = Arc::new(FakeGateway::answering(200));
    let client = SlackOagwClient::new(
        Arc::clone(&gateway) as Arc<dyn ServiceGatewayClientV1>,
        Arc::new(MockCredStoreClient::empty()),
    );

    let error = client
        .send(&ctx(), &message())
        .await
        .expect_err("no secret of that name");
    assert!(
        matches!(&error, DomainError::Validation { field, .. }
            if field == "slack_webhook_credstore_ref"),
        "{error:?}"
    );
    assert!(gateway.requests().is_empty());
    assert!(gateway.upstreams().is_empty());
}

/// A credential store that fails is an internal fault, not a settings problem,
/// and nothing is dialled.
#[tokio::test]
async fn a_failing_credential_store_is_internal_and_nothing_is_dialled() {
    let gateway = Arc::new(FakeGateway::answering(200));
    let client = SlackOagwClient::new(
        Arc::clone(&gateway) as Arc<dyn ServiceGatewayClientV1>,
        Arc::new(MockCredStoreClient::always_failing()),
    );

    let error = client
        .send(&ctx(), &message())
        .await
        .expect_err("the store fails");
    assert!(matches!(error, DomainError::Internal(_)), "{error:?}");
    assert!(gateway.requests().is_empty());
}

/// A stored secret that is not a Slack incoming-webhook URL is refused before
/// anything is provisioned or proxied, and the refusal does not echo it.
#[tokio::test]
async fn a_stored_secret_that_is_not_a_slack_url_is_refused_before_dialling() {
    for secret in [
        "https://evil.test/x".to_owned(),
        // Not UTF-8 at all: `FromUtf8Error` carries the bytes, so this also
        // pins that the conversion's error is dropped, not rendered.
        String::new(),
    ] {
        let gateway = Arc::new(FakeGateway::answering(200));
        let credstore: Arc<dyn CredStoreClientV1> = if secret.is_empty() {
            Arc::new(MockCredStoreClient::returning_raw_value(vec![
                0xFF, b'e', b'v', b'i', b'l',
            ]))
        } else {
            credstore_with(&secret)
        };
        let client = SlackOagwClient::new(
            Arc::clone(&gateway) as Arc<dyn ServiceGatewayClientV1>,
            credstore,
        );

        let error = client
            .send(&ctx(), &message())
            .await
            .expect_err("not a Slack webhook");
        assert!(
            matches!(&error, DomainError::Validation { field, .. }
                if field == "slack_webhook_credstore_ref"),
            "{error:?}"
        );
        assert!(
            !format!("{error} {error:?}").contains("evil"),
            "echoed: {error:?}"
        );
        assert!(gateway.requests().is_empty());
        assert!(gateway.upstreams().is_empty());
        assert!(gateway.routes().is_empty());
    }
}

/// **Fix round 1: no existence oracle.** The `/test` override can name any
/// reference, so "no such secret" and "a secret that is not a webhook" (or not
/// UTF-8, or reported as `NotFound` by the client) must read identically.
#[tokio::test]
async fn absent_and_malformed_secrets_are_refused_with_one_message() {
    let stores: Vec<Arc<dyn CredStoreClientV1>> = vec![
        Arc::new(MockCredStoreClient::empty()),
        Arc::new(MockCredStoreClient::erroring_not_found()),
        Arc::new(DenyingCredStore),
        credstore_with("https://evil.test/x"),
        credstore_with("not a url"),
        Arc::new(MockCredStoreClient::returning_raw_value(vec![0xFF, 0xFE])),
    ];
    let mut messages = Vec::new();
    for store in stores {
        let gateway = Arc::new(FakeGateway::answering(200));
        let error = SlackOagwClient::new(
            Arc::clone(&gateway) as Arc<dyn ServiceGatewayClientV1>,
            store,
        )
        .send(&ctx(), &message())
        .await
        .expect_err("unusable reference");
        let DomainError::Validation { field, message } = error else {
            panic!("expected Validation, got {error:?}");
        };
        assert_eq!(field, "slack_webhook_credstore_ref");
        assert!(gateway.requests().is_empty());
        messages.push(message);
    }
    messages.dedup();
    assert_eq!(
        messages.len(),
        1,
        "one message for every case: {messages:?}"
    );
}

/// A webhook secret stored with `private` sharing is readable by
/// the user who stored it and by nobody else — in particular not by the
/// qa-insights system actor every real send runs as. The refusal says so,
/// because the operator is looking at a secret they can see and a send that
/// says it does not exist.
#[tokio::test]
async fn a_private_webhook_secret_is_unreadable_to_the_system_actor_and_the_refusal_names_sharing()
{
    let user = ctx();
    let actor = system_actor::for_settings_test_send(TenantBound::new(TENANT).expect("non-nil"));
    let store = |sharing| -> Arc<dyn CredStoreClientV1> {
        Arc::new(SharingCredStore::new(vec![StoredSecret {
            reference: "slack-hook",
            value: GOOD,
            tenant: TENANT,
            owner: user.subject_id(),
            sharing,
        }]))
    };
    let send = |store: Arc<dyn CredStoreClientV1>, as_: SecurityContext| async move {
        let gateway = Arc::new(FakeGateway::answering(200));
        let result = SlackOagwClient::new(
            Arc::clone(&gateway) as Arc<dyn ServiceGatewayClientV1>,
            store,
        )
        .send(&as_, &message())
        .await;
        (result, gateway)
    };

    // The fake honours the owner: without this, the refusal below would also
    // pass for a fake that hides every private secret from everyone.
    let (owner_read, _) = send(store(SharingMode::Private), user.clone()).await;
    assert_eq!(
        owner_read.expect("the owner reads their own secret"),
        SendOutcome::Sent
    );

    let (actor_read, gateway) = send(store(SharingMode::Private), actor.clone()).await;
    let error = actor_read.expect_err("a private secret is invisible to the system actor");
    match &error {
        DomainError::Validation { field, message } => {
            assert_eq!(field, "slack_webhook_credstore_ref");
            assert!(message.contains(UNREADABLE_SECRET_HINT), "{message}");
        }
        other => panic!("expected a Validation naming the reference, got {other:?}"),
    }
    assert!(
        gateway.requests().is_empty(),
        "nothing is dialled for an unreadable secret"
    );

    let (tenant_read, _) = send(store(SharingMode::Tenant), actor).await;
    assert_eq!(
        tenant_read.expect("a tenant-shared secret is readable by the system actor"),
        SendOutcome::Sent
    );
}

/// **The redaction test.** Every way the send can fail after the secret is
/// resolved — oagw's own timeout, a transport error, a revoked webhook (404),
/// a Slack-side refusal (500), this adapter's own bound — comes back as
/// `UpstreamEgress` on the Slack channel with the right kind, and **none of
/// them carries the webhook path**, although oagw's error text does
/// (`error.to_string()` is what the audit log stores and `list_log` serves).
#[tokio::test]
async fn no_gateway_error_carries_the_webhook_path() {
    for (behavior, failure) in [
        (ProxyBehavior::GatewayTimeout, EgressFailure::Timeout),
        (
            ProxyBehavior::GatewayUnavailable,
            EgressFailure::Unreachable,
        ),
        (ProxyBehavior::Answer(404), EgressFailure::Authentication),
        (ProxyBehavior::Answer(403), EgressFailure::Authentication),
        (ProxyBehavior::Answer(401), EgressFailure::Authentication),
        (ProxyBehavior::Answer(400), EgressFailure::Rejected),
        (ProxyBehavior::Answer(500), EgressFailure::Rejected),
    ] {
        let gateway = Arc::new(FakeGateway::with(behavior));
        let error = client(&gateway)
            .send(&ctx(), &message())
            .await
            .expect_err("every scripted answer is a failure");
        assert_redacted_egress(&error, failure);
        assert_eq!(gateway.requests().len(), 1, "the request was sent");
    }

    let error = SlackOagwClient::with_timeout(
        Arc::new(FakeGateway::hanging()),
        credstore_with(GOOD),
        Duration::from_millis(20),
    )
    .send(&ctx(), &message())
    .await
    .expect_err("the adapter's own bound");
    assert_redacted_egress(&error, EgressFailure::Timeout);
}

// ---------------------------------------------------------------------------
// The payload shape
// ---------------------------------------------------------------------------

/// The payload this adapter sends matches legacy's `slack_webhook_payload`
/// shape (`notifications.rs:880-904`): `text` always, `channel` when present,
/// no `blocks` key when empty.
#[tokio::test]
async fn send_posts_the_legacy_payload_shape_through_oagw() {
    let gateway = Arc::new(FakeGateway::answering(200));

    client(&gateway)
        .send(&ctx(), &message())
        .await
        .expect("the fake gateway answers 200");

    let proxied = gateway.requests();
    let payload: serde_json::Value =
        serde_json::from_str(&proxied[0].body).expect("the body is JSON");
    assert_eq!(payload["text"], "build failed");
    assert_eq!(payload["channel"], "#qa-alerts");
    assert!(payload.get("blocks").is_none());
}

/// No proxied request carries a credential header: the Slack credential is the
/// path, and a header `oagw` would strip anyway (`headers.rs:13-18`) is not
/// where it belongs.
#[tokio::test]
async fn no_proxied_request_carries_a_credential_header() {
    let gateway = Arc::new(FakeGateway::answering(200));

    client(&gateway)
        .send(&ctx(), &message())
        .await
        .expect("the fake gateway answers 200");

    let proxied = gateway.requests();
    assert!(proxied[0].header_names.contains(&"content-type".to_owned()));
    assert!(
        !proxied[0].header_names.iter().any(|h| h == "authorization"),
        "no proxied request should carry a credential header"
    );
}

/// A blank/whitespace channel is omitted, matching legacy's
/// `normalized_channel` (`notifications.rs:834-838`).
#[tokio::test]
async fn a_blank_channel_is_omitted_from_the_payload() {
    let gateway = Arc::new(FakeGateway::answering(200));
    let mut message = message();
    message.channel = Some("   ".to_owned());

    client(&gateway)
        .send(&ctx(), &message)
        .await
        .expect("the fake gateway answers 200");

    let proxied = gateway.requests();
    let payload: serde_json::Value =
        serde_json::from_str(&proxied[0].body).expect("the body is JSON");
    assert!(payload.get("channel").is_none());
}

/// Non-empty blocks reach the payload as Block Kit, encoded by
/// [`super::super::block_kit`] — the port carries `SlackBlock`, not JSON
/// (review finding #17), so this is now also the assertion that the adapter
/// does the encoding at all.
#[tokio::test]
async fn non_empty_blocks_are_included_in_the_payload() {
    let gateway = Arc::new(FakeGateway::answering(200));
    let mut message = message();
    message.blocks = vec![SlackBlock::Section {
        text: "*Failed*".to_owned(),
    }];

    client(&gateway)
        .send(&ctx(), &message)
        .await
        .expect("the fake gateway answers 200");

    let proxied = gateway.requests();
    let payload: serde_json::Value =
        serde_json::from_str(&proxied[0].body).expect("the body is JSON");
    assert_eq!(
        payload["blocks"],
        serde_json::json!([{
            "type": "section",
            "text": { "type": "mrkdwn", "text": "*Failed*" }
        }])
    );
}

/// A non-2xx answer from the gateway (or the far side) is a failure, not a
/// silent success — and a 404, Slack's answer for a revoked or mistyped
/// webhook, is named as a credential problem.
#[tokio::test]
async fn a_non_success_status_is_an_error() {
    let gateway = Arc::new(FakeGateway::answering(404));
    let error = client(&gateway)
        .send(&ctx(), &message())
        .await
        .expect_err("a 404 from the gateway must not be reported as sent");
    assert_redacted_egress(&error, EgressFailure::Authentication);
}

/// A 502/503/504 that `oagw` generated (it carries `ErrorSource::Gateway`) is
/// Unreachable, and does not claim the webhook answered.
#[tokio::test]
async fn a_gateway_generated_status_is_unreachable_not_a_slack_answer() {
    for status in [502u16, 503, 504] {
        let gateway = Arc::new(FakeGateway::with(ProxyBehavior::GatewayAnswer(status)));
        let error = client(&gateway)
            .send(&ctx(), &message())
            .await
            .expect_err("a gateway-generated failure is an error");
        assert_redacted_egress(&error, EgressFailure::Unreachable);
        let text = error.to_string();
        assert!(
            text.contains(&format!("the gateway could not reach hooks.slack.com (HTTP {status})")),
            "{text}"
        );
        assert!(!text.contains("the webhook answered"), "{text}");
    }
}

/// Without the gateway marker the status is Slack's own answer and keeps the
/// Slack-answered wording and classification.
#[tokio::test]
async fn an_unmarked_502_is_still_a_rejected_slack_answer() {
    let gateway = Arc::new(FakeGateway::answering(502));
    let error = client(&gateway)
        .send(&ctx(), &message())
        .await
        .expect_err("a 502 is an error");
    assert_redacted_egress(&error, EgressFailure::Rejected);
    assert!(error.to_string().contains("the webhook answered HTTP 502"));
}

// ---------------------------------------------------------------------------
// Finding A — closed by `send`'s `ctx` parameter, pinned the other way now
// ---------------------------------------------------------------------------

/// Finding A (module header) is closed: this adapter forwards the caller's own
/// `SecurityContext` to `oagw::proxy_request` rather than authenticating as
/// [`SecurityContext::anonymous`]'s nil tenant. This test used to pin the
/// opposite (the anonymous-context placeholder) before fix round 1 added the
/// `ctx` parameter [`SlackClient::send`] was missing; inverted here rather than
/// deleted, so a future regression back to a fixed/placeholder identity fails
/// this test instead of going unnoticed.
#[tokio::test]
async fn the_slack_client_forwards_the_callers_security_context() {
    let gateway = Arc::new(FakeGateway::answering(200));

    client(&gateway)
        .send(&ctx(), &message())
        .await
        .expect("the fake gateway answers 200");

    assert_eq!(gateway.tenants(), [TENANT]);
    assert_ne!(
        gateway.tenants(),
        [Uuid::nil()],
        "must not fall back to SecurityContext::anonymous()'s nil tenant"
    );
}

// ---------------------------------------------------------------------------
// The golden payload — finding #17
// ---------------------------------------------------------------------------

/// The fallback text the golden render produces — `strip_mrkdwn` of the header
/// section, then the summary, the counts and the body, joined by ` \u{b7} `.
const GOLDEN_FALLBACK_TEXT: &str = ":redcircle: nightly-smoke-142 \u{2014} Failed \u{b7} \
                                    vhp/smoke  \u{b7}  vp-nightly-1  \u{b7}  QA 8.0.1 (1024) \
                                    \u{b7} passed 2, failed 2, skipped 1 \u{b7} \
                                    >2 test failures detected";

/// The golden render's `context` block text.
const GOLDEN_FOOTER_TEXT: &str = "nightly  \u{b7}  qa-e2e @ main  \u{b7}  \
                                  Started: 2026-04-07T01:01:00Z  \u{b7}  \
                                  Finished: 2026-04-07T01:11:02Z";

/// Every one of the six templates enabled with default wording, i.e. the state
/// a tenant is in immediately after switching scheduled-run Slack alerts on.
/// Mirrors `domain::notify::render_tests::config_with_every_template_enabled`.
fn config_with_every_template_enabled() -> qa_insights_sdk::NotificationConfig {
    let template = qa_insights_sdk::ScheduledRunSlackTemplate {
        enabled: true,
        ..qa_insights_sdk::ScheduledRunSlackTemplate::default()
    };
    qa_insights_sdk::NotificationConfig {
        scheduled_run_slack_templates: qa_insights_sdk::ScheduledRunSlackTemplates {
            pending: template.clone(),
            in_progress: template.clone(),
            succeeded: template.clone(),
            failed: template.clone(),
            error: template.clone(),
            skipped: template,
        },
        ..qa_insights_sdk::NotificationConfig::default()
    }
}

fn golden_render_context() -> crate::domain::notify::render::ScheduledRunRenderContext {
    crate::domain::notify::render::ScheduledRunRenderContext {
        run_name: "nightly-smoke-142".to_owned(),
        plan_id: "vhp/smoke".to_owned(),
        phase: "Failed".to_owned(),
        run_source: "scheduled".to_owned(),
        platform: Some("vp-nightly-1".to_owned()),
        product_key: Some("QA".to_owned()),
        app_version: Some("8.0.1".to_owned()),
        app_build: Some("1024".to_owned()),
        test_version: Some("main".to_owned()),
        schedule_id: Some("nightly".to_owned()),
        repo_name: Some("qa-e2e".to_owned()),
        source_ref: Some("main".to_owned()),
        source_ref_kind: Some("branch".to_owned()),
        started_at: Some("2026-04-07T01:01:00Z".to_owned()),
        finished_at: Some("2026-04-07T01:11:02Z".to_owned()),
        duration: Some("10m 2s".to_owned()),
        message: Some("2 test failures detected".to_owned()),
        result_statuses: vec![
            "PASSED".to_owned(),
            "FAILED".to_owned(),
            "PASSED".to_owned(),
            "ERROR".to_owned(),
            "SKIPPED".to_owned(),
        ],
    }
}

/// The whole webhook body, byte for byte, for one fully-populated
/// scheduled-run render — the guard finding #17's refactor needed and this
/// repo did not have. Nothing else asserts the *composition* of the domain
/// renderer's blocks with the adapter's payload builder: `render_tests`
/// reaches into one block at a time and this module's other payload tests use
/// a hand-written stub block. Compared as [`serde_json::Value`], not as text,
/// because `serde_json/preserve_order` is on workspace-wide and a text
/// comparison would pass per-package and fail under `make test-no-macros`.
#[tokio::test]
async fn the_rendered_scheduled_run_payload_is_the_golden_block_kit_body() {
    let rendered = crate::domain::notify::render::render_scheduled_run(
        &config_with_every_template_enabled(),
        "failed",
        &golden_render_context(),
    )
    .expect("`failed` is one of the six tokens");

    let gateway = Arc::new(FakeGateway::answering(200));
    client(&gateway)
        .send(
            &ctx(),
            &SlackMessage {
                webhook_credstore_ref: HOOK_REF.to_owned(),
                channel: Some("#qa-alerts".to_owned()),
                text: rendered.fallback_text,
                blocks: rendered.blocks,
            },
        )
        .await
        .expect("the fake gateway answers 200");

    let proxied = gateway.requests();
    let payload: serde_json::Value =
        serde_json::from_str(&proxied[0].body).expect("the body is JSON");
    assert_eq!(
        payload,
        serde_json::json!({
            "text": GOLDEN_FALLBACK_TEXT,
            "channel": "#qa-alerts",
            "blocks": [
                {
                    "type": "section",
                    "text": {
                        "type": "mrkdwn",
                        "text": ":red_circle: `nightly-smoke-142` \u{2014} *Failed*"
                    }
                },
                {
                    "type": "section",
                    "text": {
                        "type": "mrkdwn",
                        "text": "`vhp/smoke`  \u{b7}  vp-nightly-1  \u{b7}  QA 8.0.1 (1024)"
                    }
                },
                {
                    "type": "section",
                    "text": {
                        "type": "mrkdwn",
                        "text": ":white_check_mark: 2   :x: 2   :fast_forward: 1   \
                                 :stopwatch: 10m 2s"
                    }
                },
                {
                    "type": "section",
                    "text": { "type": "mrkdwn", "text": ">2 test failures detected" }
                },
                {
                    "type": "context",
                    "elements": [{ "type": "mrkdwn", "text": GOLDEN_FOOTER_TEXT }]
                }
            ]
        })
    );
}
