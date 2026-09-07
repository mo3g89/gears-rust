//! Tests for the oagw Slack adapter.
//!
//! One [`FakeGateway`], scripted per test, in the same shape as
//! `infra::jira::oagw_client_tests::FakeGateway`: what is under test is the
//! *request this gear sends* and the *identity it sends as*, both of which a
//! real `oagw` would absorb into its own behaviour.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use crate::domain::ports::SlackBlock;
use async_trait::async_trait;
use oagw_sdk::api::ServiceGatewayClientV1;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

use super::*;

fn message() -> SlackMessage {
    SlackMessage {
        webhook_credstore_ref: "cred://slack-hook".to_owned(),
        channel: Some("#qa-alerts".to_owned()),
        text: "build failed".to_owned(),
        blocks: Vec::new(),
    }
}

/// The tenant every test sends as, unless a test is specifically about a
/// *different* tenant. Not [`SecurityContext::anonymous`] — R108 exists
/// precisely so this adapter never has to fall back to that.
const TENANT: Uuid = Uuid::from_u128(0x51ACC);

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xDEAD))
        .subject_tenant_id(TENANT)
        .build()
        .expect("subject_id and subject_tenant_id are both set")
}

/// One proxied request, flattened so the assertions can read it without a
/// non-`Clone` body in the way.
#[derive(Clone, Debug)]
struct Proxied {
    method: String,
    uri: String,
    /// Every request header name, lowercased. Finding B (module header) means
    /// a credential should never appear here for Slack; unlike the JIRA
    /// adapter's own such assertion, this one is about a shape that was never
    /// exercised to carry a credential in the first place — see this
    /// module's `no_proxied_request_carries_a_credential_header`.
    header_names: Vec<String>,
    body: String,
}

/// How [`FakeGateway::proxy_request`] answers.
enum ProxyBehavior {
    /// Answer with the given status and an empty body.
    Answer(u16),
    /// Never resolve — the black-holing webhook host legacy's own comment
    /// describes.
    Hang,
}

/// A scripted [`ServiceGatewayClientV1`] whose only real method is
/// `proxy_request`; every other method is unreachable from this adapter and
/// panics if called.
struct FakeGateway {
    behavior: ProxyBehavior,
    proxied: Mutex<Vec<Proxied>>,
    /// The `subject_tenant_id` of every `SecurityContext` `proxy_request` was
    /// called with — what
    /// [`the_slack_client_forwards_the_callers_security_context`] reads.
    ctx_tenants: Mutex<Vec<Uuid>>,
}

impl FakeGateway {
    fn answering(status: u16) -> Self {
        Self {
            behavior: ProxyBehavior::Answer(status),
            proxied: Mutex::new(Vec::new()),
            ctx_tenants: Mutex::new(Vec::new()),
        }
    }

    fn hanging() -> Self {
        Self {
            behavior: ProxyBehavior::Hang,
            proxied: Mutex::new(Vec::new()),
            ctx_tenants: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<Proxied> {
        self.proxied.lock().unwrap().clone()
    }

    fn tenants(&self) -> Vec<Uuid> {
        self.ctx_tenants.lock().unwrap().clone()
    }
}

#[async_trait]
impl ServiceGatewayClientV1 for FakeGateway {
    async fn create_upstream(
        &self,
        _: SecurityContext,
        _: oagw_sdk::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unreachable!("SlackOagwClient never provisions an upstream")
    }

    async fn get_upstream(
        &self,
        _: SecurityContext,
        _: Uuid,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unreachable!("SlackOagwClient never reads an upstream")
    }

    async fn list_upstreams(
        &self,
        _: SecurityContext,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Upstream>, CanonicalError> {
        unreachable!("SlackOagwClient never lists upstreams")
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
        _: oagw_sdk::CreateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unreachable!("SlackOagwClient never provisions a route")
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
        unreachable!("SlackOagwClient never resolves a proxy target directly")
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
            uri,
            header_names,
            body,
        });

        let ProxyBehavior::Answer(status) = self.behavior else {
            unreachable!("Hang already returned above")
        };
        Ok(http::Response::builder()
            .status(status)
            .body(Body::from(""))
            .expect("the fake response builds"))
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
    let client =
        SlackOagwClient::with_timeout(Arc::new(FakeGateway::hanging()), Duration::from_millis(20));

    let started = std::time::Instant::now();
    let result = client.send(&ctx(), &message()).await;
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "a hanging gateway must not be reported as sent"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "send must return promptly once its own bound elapses, took {elapsed:?}"
    );
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
    let client = SlackOagwClient::new(gateway.clone());

    let outcome = client
        .send(&ctx(), &message())
        .await
        .expect("the fake gateway answers 200");
    assert_eq!(outcome, SendOutcome::Sent);

    let proxied = gateway.requests();
    assert_eq!(proxied.len(), 1);
    assert_eq!(proxied[0].method, "POST");
    assert_eq!(proxied[0].uri, "/slack-hook");

    let payload: serde_json::Value =
        serde_json::from_str(&proxied[0].body).expect("the body is JSON");
    assert_eq!(payload["text"], "build failed");
    assert_eq!(payload["channel"], "#qa-alerts");
    assert!(payload.get("blocks").is_none());
}

/// No proxied request carries a credential header. Not a guarantee of
/// correctness (Finding B: the credential cannot be delivered at all today),
/// but a guard against a future edit accidentally putting one in a header
/// `oagw` would strip anyway (`headers.rs:13-18`).
#[tokio::test]
async fn no_proxied_request_carries_a_credential_header() {
    let gateway = Arc::new(FakeGateway::answering(200));
    let client = SlackOagwClient::new(gateway.clone());

    client
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
    let client = SlackOagwClient::new(gateway.clone());
    let mut message = message();
    message.channel = Some("   ".to_owned());

    client
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
    let client = SlackOagwClient::new(gateway.clone());
    let mut message = message();
    message.blocks = vec![SlackBlock::Section {
        text: "*Failed*".to_owned(),
    }];

    client
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
/// silent success.
#[tokio::test]
async fn a_non_success_status_is_an_error() {
    let client = SlackOagwClient::new(Arc::new(FakeGateway::answering(404)));
    let result = client.send(&ctx(), &message()).await;
    assert!(
        result.is_err(),
        "a 404 from the gateway must not be reported as sent"
    );
}

// ---------------------------------------------------------------------------
// Finding A — closed by R108, pinned the other way now
// ---------------------------------------------------------------------------

/// Finding A (module header) is closed: this adapter forwards the caller's
/// own `SecurityContext` to `oagw::proxy_request` rather than authenticating
/// as [`SecurityContext::anonymous`]'s nil tenant. This test used to pin the
/// opposite (the anonymous-context placeholder) before R108 added the `ctx`
/// parameter [`SlackClient::send`] was missing; inverted here rather than
/// deleted, so a future regression back to a fixed/placeholder identity fails
/// this test instead of going unnoticed.
#[tokio::test]
async fn the_slack_client_forwards_the_callers_security_context() {
    let gateway = Arc::new(FakeGateway::answering(200));
    let client = SlackOagwClient::new(gateway.clone());

    client
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
// webhook_path
// ---------------------------------------------------------------------------

/// [`webhook_path`] strips the one legal `cred://` scheme and leaves everything
/// else alone — Phase C's final review, Important 1. `credstore://` used to be
/// stripped too; it is not valid syntax
/// (`domain::ports::jira_client::validate_credstore_ref` rejects the colon and
/// the slashes), so it is now left verbatim rather than rewritten into
/// something that looks resolvable.
#[test]
fn webhook_path_strips_only_the_legal_credstore_scheme() {
    assert_eq!(webhook_path("cred://slack-hook"), "slack-hook");
    assert_eq!(webhook_path("slack-hook"), "slack-hook");
    assert_eq!(
        webhook_path("credstore://slack/hook"),
        "credstore://slack/hook",
        "not a scheme this crate accepts anywhere, so not one this function pretends to \
         understand"
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
    let client = SlackOagwClient::new(gateway.clone());
    client
        .send(
            &ctx(),
            &SlackMessage {
                webhook_credstore_ref: "cred://slack-hook".to_owned(),
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
