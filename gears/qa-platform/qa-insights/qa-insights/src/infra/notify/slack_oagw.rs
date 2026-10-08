//! [`SlackClient`] over the Outbound API Gateway — Task 39.
//!
//! # Do not reach for `reqwest` here
//!
//! Stated first for the same reason `infra::jira::oagw_client` states it first:
//! a future reader with a webhook URL in hand will reach for `reqwest` by
//! reflex, and legacy did exactly that
//! (`manager/src/services/notifications.rs:20`, a `reqwest::Client` field).
//! This subsystem's egress contract, `cpt-cf-qa-contract-egress`, permits
//! **one** direct-HTTP exception and it belongs to qa-catalog's git transport.
//! Every Slack call in this gear goes through
//! [`ServiceGatewayClientV1::proxy_request`]. A `reqwest` dependency in this
//! crate's `Cargo.toml` is the review finding, not the code that uses it.
//!
//! # The 10-second bound is ported reasoning, not a guess
//!
//! Legacy's `NotificationService::new`
//! (`manager/src/services/notifications.rs:52-64`) builds its `reqwest::Client`
//! with a 10-second total-request timeout, and its own comment explains why:
//! the run-queue dispatcher awaits a Slack send *inside its tick*, for the
//! mandatory `expired` event, so an unbounded request against a black-holing
//! webhook host would stall the whole queue rather than losing just the one
//! notification. **It is a fixed bound, not derived from any tick interval** —
//! legacy's own comment says so explicitly of its dispatcher's tick
//! (`RUN_QUEUE_DISPATCHER_INTERVAL_SECONDS`), and that reasoning survives the
//! port even though the tick this gear will eventually have is qa-runs'
//! (Task 40's), not this one's own. See [`SlackOagwClient::REQUEST_TIMEOUT`].
//!
//! # Finding A (fix round 1) — closed: `send` now takes `ctx`
//!
//! Task 39's first draft proxied every message as
//! `SecurityContext::anonymous()`, because [`SlackClient::send`] then took no
//! [`SecurityContext`] at all while [`ServiceGatewayClientV1::proxy_request`]
//! requires one for `oagw`'s own per-tenant upstream resolution. Both of this
//! gear's call sites — `NotifyService::send_test`
//! (`domain/service/notify.rs:702`) and
//! `send_run_completed_channel`/`RunCompletedChannel::send` (`:1038`, `:1252`)
//! — already held the real sending tenant's `ctx` at the point they built a
//! [`SlackMessage`]; the context was being dropped at the port boundary, not
//! missing from this gear. Fix round 1 added `ctx: &SecurityContext` to
//! [`SlackClient::send`] and this adapter now forwards it verbatim to
//! `proxy_request` — see the port's own header, "One method, and `ctx` is not
//! optional", for the reasoning in full.
//! [`the_slack_client_forwards_the_callers_security_context`] (this module's
//! tests) pins that the identity reaching `oagw` is the caller's, not a fixed
//! placeholder.
//!
//! # Delivery — how the webhook's credential reaches Slack (formerly "Finding B")
//!
//! A Slack incoming webhook's credential **is its URL path**
//! (`https://hooks.slack.com/services/T…/B…/<token>`). `oagw`'s auth plugins
//! all inject into a request *header*, so the JIRA pattern — oagw resolves the
//! secret and injects it, and the token never enters this gear — cannot carry
//! it. That was an open item at first, and this adapter sent the credstore
//! reference itself as a placeholder path. The owner's decision on the third
//! review pass closed it by the first of the two candidate fixes that finding
//! named: this gear resolves the secret itself, as `SmtpMailClient` already
//! does for the relay password (ADR-0011's amendment of 2026-09-30 records it
//! as the second plaintext credential this process resolves).
//!
//! Each send, in order, **before anything is dialled** where it can fail —
//! all five inside [`SlackOagwClient::REQUEST_TIMEOUT`]:
//!
//! 1. **Resolve** [`SlackMessage::webhook_credstore_ref`] through
//!    [`CredStoreClientV1::get`] under the `ctx` it is handed — the qa-insights
//!    system actor bound to the sending tenant, on the automatic path and the
//!    settings test send alike (credstore is tenant- and owner-scoped; DESIGN
//!    §3.5, "Egress").
//! 2. **Validate** the secret with [`SlackWebhook::parse`]: HTTPS, host
//!    exactly [`SLACK_WEBHOOK_HOST`], default port, a plain `/services/…`
//!    path. The host is fixed by this code, not taken from the secret, so a
//!    stored value cannot aim this gear's egress anywhere else.
//! 3. **Provision**, once per tenant per process, a no-auth `oagw` upstream
//!    aliased `hooks.slack.com` (HTTPS, 443) and one route `POST /services`
//!    with [`PathSuffixMode::Append`] and no query parameters — the JIRA
//!    adapter's `ensure_upstream`/`ensure_route` shape, `AlreadyExists`
//!    recovery included (`infra::jira::oagw_client`).
//! 4. **Verify** the target: `oagw::resolve_proxy_target` for this very
//!    request, and refuse unless the upstream it resolves is exactly the one
//!    step 3 provisions and the route carries no plugins — every send (see
//!    below).
//! 5. **Proxy** `POST /hooks.slack.com/services/…` under the same `ctx`.
//!
//! The credential lookup (step 1) is inside that bound as well as steps 3-5,
//! so a credential store that never answers cannot stall the caller either.
//!
//! **The deployment contract:** the credential-store secret named by the
//! tenant's `slack_webhook_credstore_ref` holds the **full**
//! `https://hooks.slack.com/services/…` URL.
//!
//! # The upstream is verified, not trusted by alias (fix round 1)
//!
//! `oagw` aliases are arbitrary. A principal with upstream-management rights
//! in the tenant but no credential-store read could pre-create
//! `hooks.slack.com → https://elsewhere:443`; the first draft would then have
//! reused it on `AlreadyExists` and proxied every webhook path there. Now an
//! upstream found by alias is checked before a route is registered on it or
//! the tenant is cached, and **every** send asks `oagw` to resolve the exact
//! target (`resolve_proxy_target`, the same hierarchy walk `proxy_request`
//! performs, so inherited and shadowing upstreams are covered too) and refuses
//! unless it is `https://hooks.slack.com:443` over HTTP with no auth, no
//! plugins and no request-header rules, on a route with no plugins. A refusal
//! is `UpstreamEgress`/`Unreachable` with fixed text naming neither the
//! webhook nor the other host.
//!
//! **Residual:** a check-then-use gap of one in-process call. An upstream or
//! route edited between `resolve_proxy_target` and `proxy_request` — not
//! between sends, which the per-send check covers — is not caught. Closing it
//! needs `oagw` to accept an expected upstream id or endpoint on the proxy
//! call itself, which it does not.
//!
//! # Redaction — nothing that leaves this adapter carries the path
//!
//! The path is the credential, and every error this adapter returns can reach
//! the audit log (`domain::service::notify` stores `error.to_string()` and
//! `list_log` serves it) and the settings page's test response. `oagw`'s own
//! error text includes the full upstream URL. So **no gateway error is ever
//! formatted**: every proxy failure, timeout and non-2xx becomes
//! [`DomainError::UpstreamEgress`] with `channel = "slack"`,
//! `endpoint = "hooks.slack.com"` and a `detail` that is fixed text, the
//! gateway error's category title, or the HTTP status — never `{e}`. The
//! resolved value is never logged — here, nor by `oagw`, whose proxy logs name
//! the alias and route pattern rather than the request path at `info` and
//! above (`oagw`'s `infra::proxy::pingora_proxy`, "Request logs", including its
//! DEBUG/TRACE residual) — and [`SlackWebhook`]'s `Debug` is
//! redacted. The one error that does render another component's text is a
//! credential-store failure (`Internal`), and that text describes the store,
//! not the secret.
//!
//! Classification: this adapter's own bound or `oagw`'s `DeadlineExceeded` is
//! [`EgressFailure::Timeout`]; any other gateway error is
//! [`EgressFailure::Unreachable`]; a non-2xx response that `oagw` itself
//! generated (its `ErrorSource::Gateway` response extension — a 502/503/504
//! when it cannot reach Slack) is [`EgressFailure::Unreachable`] with text that
//! does not claim Slack answered (a *missing route* is not a status at all: called
//! in-process, `oagw` answers it as `Err(NotFound)`, which is the "any other
//! gateway error" case above);
//! otherwise 401/403/404 are [`EgressFailure::Authentication`] (Slack answers
//! 403/404 for a revoked or mistyped webhook) and any other non-2xx is
//! [`EgressFailure::Rejected`].
//!
//! A failed send still degrades the way Task 38's claim lifecycle intends: the
//! claim is released and an `OUTCOME_FAILED` audit row is written
//! (`domain::service::notify::NotifyService::send_run_completed_channel`), so
//! nothing is silently suppressed.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::{CredStoreClientV1, CredStoreError, SecretRef};
use oagw_sdk::api::{ErrorSource, ServiceGatewayClientV1};
use oagw_sdk::{
    Body, CreateRouteRequest, CreateUpstreamRequest, Endpoint, HTTP_PROTOCOL_ID, HttpMatch,
    HttpMethod, ListQuery, MatchRules, PathSuffixMode, RequestHeaderRules, Scheme, Server,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use tracing::{debug, warn};
use uuid::Uuid;

use super::UNREADABLE_SECRET_HINT;
use super::slack_webhook::{REF_FIELD, SERVICES_PREFIX, SLACK_WEBHOOK_HOST, SlackWebhook};
use crate::domain::error::{DomainError, EgressFailure};
use crate::domain::ports::{SendOutcome, SlackClient, SlackMessage};

/// The channel name every [`DomainError::UpstreamEgress`] from this adapter
/// carries — the same string `domain::service::notify` writes into the audit
/// log's `channel` column for this port.
const CHANNEL: &str = "slack";

/// [`SlackClient`] over `oagw`. See this module's header for how a send is
/// delivered and what its errors never say.
pub struct SlackOagwClient {
    gateway: Arc<dyn ServiceGatewayClientV1>,
    /// Resolves [`SlackMessage::webhook_credstore_ref`] under the caller's
    /// context — the module header's step 1.
    credstore: Arc<dyn CredStoreClientV1>,
    /// The bound `send` applies to one proxied request.
    /// [`Self::REQUEST_TIMEOUT`] in production, always — [`Self::with_timeout`]
    /// exists only so a test can prove the bound is *applied*, not merely
    /// declared, without waiting out ten real seconds.
    timeout: Duration,
    /// The tenants whose `hooks.slack.com` upstream **and** route this process
    /// has seen in place. Keyed by tenant alone because the alias and the
    /// route are fixed. A `std::sync::Mutex`, never held across an `.await` —
    /// `infra::jira::oagw_client`'s `provisioned` field, for its reason.
    provisioned: Mutex<HashSet<Uuid>>,
}

impl SlackOagwClient {
    /// The fixed bound this module's header explains. `Duration::from_secs(10)`
    /// is legacy's own value
    /// (`manager/src/services/notifications.rs:58-59`, "10s is generous for a
    /// webhook - Slack's own guidance is ~3s").
    pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

    #[must_use]
    pub fn new(
        gateway: Arc<dyn ServiceGatewayClientV1>,
        credstore: Arc<dyn CredStoreClientV1>,
    ) -> Self {
        Self {
            gateway,
            credstore,
            timeout: Self::REQUEST_TIMEOUT,
            provisioned: Mutex::new(HashSet::new()),
        }
    }

    /// Test-only escape hatch: a client whose bound is *not*
    /// [`Self::REQUEST_TIMEOUT`], so a test can exercise the bound with a
    /// duration measured in milliseconds rather than the real ten seconds.
    /// Production code has exactly one way to build this type —
    /// [`Self::new`] — and always gets the real bound.
    #[cfg(test)]
    fn with_timeout(
        gateway: Arc<dyn ServiceGatewayClientV1>,
        credstore: Arc<dyn CredStoreClientV1>,
        timeout: Duration,
    ) -> Self {
        Self {
            gateway,
            credstore,
            timeout,
            provisioned: Mutex::new(HashSet::new()),
        }
    }

    /// The webhook the tenant's credential store holds under `reference`,
    /// validated — `SmtpMailClient::password`'s shape, for its reasons.
    ///
    /// The resolved value lives only until [`SlackWebhook::parse`] has
    /// consumed it and is never formatted. Note the `|_|` on the UTF-8
    /// conversion: `FromUtf8Error` carries the bytes it failed on.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming `slack_webhook_credstore_ref` for a
    /// reference that is not valid syntax, or — with **one** fixed message for
    /// both — that this tenant cannot read or whose secret is not a Slack
    /// incoming-webhook URL.
    /// [`DomainError::Internal`] when the credential store itself fails.
    async fn webhook(
        &self,
        ctx: &SecurityContext,
        reference: &str,
    ) -> Result<SlackWebhook, DomainError> {
        // A syntax refusal is about the *name* the caller typed and says
        // nothing about which secrets exist, so it keeps its own text.
        let key = SecretRef::new(reference).map_err(|_| DomainError::Validation {
            field: REF_FIELD.to_owned(),
            message: "the Slack webhook reference is not a syntactically valid credential-store \
                      reference"
                .to_owned(),
        })?;

        // **One answer for "absent" and "present but not a webhook"** (fix
        // round 1). The `/test` route's config override lets the caller name
        // any reference, so two different texts here would be an existence
        // oracle over the tenant's secret names. `Ok(None)` is credstore's own
        // anti-enumeration surface ("no such secret, or not yours"); a client
        // that reports that surface as `NotFound`/`AccessDenied` instead gets
        // the same answer, so the oracle does not reopen through the error
        // path either.
        let found = match self.credstore.get(ctx, &key).await {
            Ok(found) => found,
            Err(CredStoreError::NotFound | CredStoreError::AccessDenied) => None,
            Err(e) => {
                return Err(DomainError::Internal(format!(
                    "the credential store refused the Slack webhook secret: {e}"
                )));
            }
        };
        let Some(secret) = found else {
            return Err(unusable_reference());
        };
        // Both conversions drop their error: `FromUtf8Error`/`Utf8Error`
        // describe the bytes, and `parse`'s own message would distinguish
        // "exists but malformed" from "absent".
        let value =
            std::str::from_utf8(secret.value.as_bytes()).map_err(|_| unusable_reference())?;
        SlackWebhook::parse(value).map_err(|_| unusable_reference())
    }

    /// Ensure the tenant has the `hooks.slack.com` upstream and its route —
    /// the module header's step 3. After the first success per tenant it costs
    /// no gateway calls at all.
    ///
    /// # Errors
    ///
    /// [`DomainError::UpstreamEgress`] ([`EgressFailure::Unreachable`]) when
    /// the gateway refused the upstream or the route.
    async fn ensure_upstream(&self, ctx: &SecurityContext) -> Result<(), DomainError> {
        let tenant = ctx.subject_tenant_id();
        if self.is_provisioned(tenant) {
            return Ok(());
        }

        match self
            .gateway
            .create_upstream(ctx.clone(), upstream_request())
            .await
        {
            Ok(upstream) => {
                debug!(
                    alias = %upstream.alias,
                    upstream_id = %upstream.id,
                    "provisioned the tenant's oagw upstream for Slack webhooks",
                );
                self.ensure_route(ctx, upstream.id).await?;
                self.mark_provisioned(tenant);
            }
            Err(CanonicalError::AlreadyExists { .. }) => {
                // Another replica, or an earlier run of this process, already
                // provisioned it. The route must still be ensured — the JIRA
                // adapter's reasoning (`reuse_existing_upstream`).
                if self.reuse_existing_upstream(ctx).await? {
                    self.mark_provisioned(tenant);
                }
            }
            Err(e) => {
                return Err(egress(
                    EgressFailure::Unreachable,
                    format!(
                        "the gateway could not provision the {SLACK_WEBHOOK_HOST} upstream ({})",
                        e.title()
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Resolve the upstream behind an `AlreadyExists` conflict and ensure its
    /// route. `true` when both succeeded and the tenant may be cached; `false`
    /// — with a WARN and no error — when the upstream could not be looked up,
    /// so the send proceeds to the per-send [`Self::verify_target`] (which
    /// refuses anything this gear did not provision) and the next one tries
    /// again. The JIRA adapter's `reuse_existing_upstream`, for its reasons.
    ///
    /// # Errors
    ///
    /// [`DomainError::UpstreamEgress`] ([`EgressFailure::Unreachable`]) when
    /// the existing upstream is not the no-auth `https://hooks.slack.com:443`
    /// one — [`check_upstream`] — or its route could not be registered.
    async fn reuse_existing_upstream(&self, ctx: &SecurityContext) -> Result<bool, DomainError> {
        let Some(upstream) = self.find_upstream_by_alias(ctx).await else {
            warn!(
                alias = SLACK_WEBHOOK_HOST,
                "the tenant's oagw upstream for Slack webhooks already exists but could not be \
                 looked up by alias; its route could not be ensured and the next send will try \
                 again",
            );
            return Ok(false);
        };
        // Never register a route on, or cache, an upstream this gear did not
        // provision the shape of — see the module header, "The upstream is
        // verified, not trusted by alias".
        check_upstream(&upstream)?;
        self.ensure_route(ctx, upstream.id).await?;
        Ok(true)
    }

    /// Register the one route a webhook send needs. A duplicate registration
    /// is success; anything else is an error (the JIRA adapter's fix round 1
    /// finding 2).
    async fn ensure_route(
        &self,
        ctx: &SecurityContext,
        upstream_id: Uuid,
    ) -> Result<(), DomainError> {
        let match_rules = MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Post],
                path: SERVICES_PREFIX.to_owned(),
                // Empty = no query parameter passes; a webhook takes none, and
                // `SlackWebhook::parse` refuses a secret that carries one.
                query_allowlist: Vec::new(),
                path_suffix_mode: PathSuffixMode::Append,
            }),
            grpc: None,
        };
        match self
            .gateway
            .create_route(
                ctx.clone(),
                CreateRouteRequest::builder(upstream_id, match_rules)
                    .enabled(true)
                    .build(),
            )
            .await
        {
            Ok(route) => {
                debug!(%upstream_id, route_id = %route.id, "registered the Slack webhook route");
                Ok(())
            }
            Err(CanonicalError::AlreadyExists { .. }) => Ok(()),
            Err(e) => Err(egress(
                EgressFailure::Unreachable,
                format!(
                    "the gateway could not register the {SLACK_WEBHOOK_HOST} route ({})",
                    e.title()
                ),
            )),
        }
    }

    /// The tenant's upstream aliased [`SLACK_WEBHOOK_HOST`], or `None` — the
    /// JIRA adapter's `find_upstream_by_alias`, paging included.
    async fn find_upstream_by_alias(&self, ctx: &SecurityContext) -> Option<oagw_sdk::Upstream> {
        let mut query = ListQuery::default();
        loop {
            let page = match self.gateway.list_upstreams(ctx.clone(), &query).await {
                Ok(page) => page,
                Err(e) => {
                    warn!(
                        alias = SLACK_WEBHOOK_HOST,
                        error = %e,
                        "the oagw upstream lookup by alias failed",
                    );
                    return None;
                }
            };
            let page_len = page.len();
            if let Some(found) = page.into_iter().find(|u| u.alias == SLACK_WEBHOOK_HOST) {
                return Some(found);
            }
            if page_len < query.top as usize {
                return None;
            }
            query.skip += query.top;
        }
    }

    /// Ask `oagw` which upstream and route it would use for this very request
    /// — `resolve_proxy_target` is the same single hierarchy walk
    /// `proxy_request` performs, alias shadowing and inheritance included —
    /// and refuse unless both are the shape this gear provisions. Runs on
    /// **every** send, cached tenant or not: the cache only saves the
    /// provisioning calls, never this check.
    ///
    /// # Errors
    ///
    /// [`DomainError::UpstreamEgress`] ([`EgressFailure::Unreachable`]) when
    /// the gateway cannot resolve the target or resolves one that
    /// [`check_upstream`]/[`check_route`] refuse.
    async fn verify_target(
        &self,
        ctx: &SecurityContext,
        webhook: &SlackWebhook,
    ) -> Result<(), DomainError> {
        let tenant = ctx.subject_tenant_id();
        let (upstream, route) = self
            .gateway
            .resolve_proxy_target(ctx.clone(), SLACK_WEBHOOK_HOST, "POST", webhook.path())
            .await
            // Not formatted: the path argument is the credential, and a
            // resolution error may echo it.
            .map_err(|e| {
                // The upstream or route this tenant was provisioned with is
                // gone (deleted out from under the cache). Forget the
                // tenant so the *next* send re-provisions; this send is still
                // refused. Only "not found" evicts: a target that resolves to
                // a foreign one is refused below and stays cached-and-refused.
                if matches!(e, CanonicalError::NotFound { .. }) {
                    self.forget_provisioned(tenant);
                }
                egress(
                    EgressFailure::Unreachable,
                    format!(
                        "the gateway could not resolve its {SLACK_WEBHOOK_HOST} route ({})",
                        e.title()
                    ),
                )
            })?;
        check_upstream(&upstream)?;
        check_route(&route)
    }

    fn is_provisioned(&self, tenant: Uuid) -> bool {
        self.provisioned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&tenant)
    }

    fn forget_provisioned(&self, tenant: Uuid) {
        self.provisioned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&tenant);
    }

    fn mark_provisioned(&self, tenant: Uuid) {
        self.provisioned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tenant);
    }
}

#[async_trait]
impl SlackClient for SlackOagwClient {
    async fn send(
        &self,
        ctx: &SecurityContext,
        message: &SlackMessage,
    ) -> Result<SendOutcome, DomainError> {
        // `ctx` is the caller's own security context, forwarded verbatim —
        // see the port's header and this module's "Finding A". Every step is
        // inside the bound — the credential lookup and the provisioning too —
        // so neither a hanging credential store nor a gateway hanging on
        // `create_upstream` can stall the caller. Steps 1 and 2 of the module
        // header still come first: nothing is provisioned or dialled for a
        // reference that does not resolve to a Slack webhook, and a
        // `Validation` from the lookup still comes out unchanged through the
        // inner `?`.
        let sent = tokio::time::timeout(self.timeout, async {
            let webhook = self.webhook(ctx, &message.webhook_credstore_ref).await?;
            let request = build_request(&webhook, message)?;
            self.ensure_upstream(ctx).await?;
            self.verify_target(ctx, &webhook).await?;
            // The gateway's error is **not** formatted: its text carries the
            // full upstream URL, which is the credential. Its category title
            // is fixed text.
            self.gateway
                .proxy_request(ctx.clone(), request)
                .await
                .map_err(|e| match e {
                    CanonicalError::DeadlineExceeded { .. } => egress(
                        EgressFailure::Timeout,
                        "the gateway's request to Slack timed out".to_owned(),
                    ),
                    other => egress(
                        EgressFailure::Unreachable,
                        format!(
                            "the gateway could not deliver the request ({})",
                            other.title()
                        ),
                    ),
                })
        })
        .await
        .map_err(|_| {
            egress(
                EgressFailure::Timeout,
                format!("no answer within {:?}", self.timeout),
            )
        })??;

        let status = sent.status();
        if status.is_success() {
            return Ok(SendOutcome::Sent);
        }
        // `oagw` marks the responses it generated itself (it cannot reach
        // Slack) with `ErrorSource::Gateway`; Slack never answered.
        if sent.extensions().get::<ErrorSource>() == Some(&ErrorSource::Gateway) {
            return Err(egress(
                EgressFailure::Unreachable,
                format!(
                    "the gateway could not reach {SLACK_WEBHOOK_HOST} (HTTP {})",
                    status.as_u16()
                ),
            ));
        }
        let failure = match status.as_u16() {
            // Slack answers 403/404 for a revoked or mistyped webhook.
            401 | 403 | 404 => EgressFailure::Authentication,
            _ => EgressFailure::Rejected,
        };
        Err(egress(
            failure,
            format!("the webhook answered HTTP {}", status.as_u16()),
        ))
    }
}

/// One [`DomainError::UpstreamEgress`], so the construction sites cannot
/// disagree about the channel or the endpoint. `detail` must never be derived
/// from the webhook or from a gateway error's text — see the module header.
fn egress(failure: EgressFailure, detail: String) -> DomainError {
    DomainError::UpstreamEgress {
        channel: CHANNEL.to_owned(),
        endpoint: SLACK_WEBHOOK_HOST.to_owned(),
        failure,
        detail,
    }
}

/// Refuse an upstream that is not exactly the one [`upstream_request`] builds:
/// HTTP protocol, one endpoint `https://hooks.slack.com:443`, **no** auth, no
/// plugins and no request-header rules.
///
/// # Why an alias is not enough
///
/// `oagw` aliases are arbitrary: anyone holding upstream-management rights in
/// the tenant (who need not be able to read its credential store) can create
/// `hooks.slack.com → https://elsewhere:443`, and every webhook path — the
/// credential — would then be proxied there. So the alias only *finds* the
/// upstream; this decides whether it may carry a webhook.
///
/// # Errors
///
/// [`DomainError::UpstreamEgress`] with [`EgressFailure::Unreachable`] and
/// fixed text naming neither the webhook nor the upstream's actual host.
/// `Unreachable` rather than `Rejected`: Slack was never contacted — the
/// tenant's gateway path to it is what is wrong — and `Rejected` means the far
/// side spoke and refused the message.
fn check_upstream(upstream: &oagw_sdk::Upstream) -> Result<(), DomainError> {
    let endpoint_is_slack = match upstream.server.endpoints.as_slice() {
        [only] => {
            let host = only.host.to_ascii_lowercase();
            only.scheme == Scheme::Https
                && only.port == 443
                && host.strip_suffix('.').unwrap_or(&host) == SLACK_WEBHOOK_HOST
        }
        _ => false,
    };
    let rewrites_requests = upstream
        .headers
        .as_ref()
        .and_then(|headers| headers.request.as_ref())
        .is_some_and(|rules| *rules != RequestHeaderRules::default());
    if endpoint_is_slack
        && upstream.protocol == HTTP_PROTOCOL_ID
        && upstream.auth.is_none()
        && !has_plugins(upstream.plugins.as_ref())
        && !rewrites_requests
    {
        return Ok(());
    }
    warn!(
        upstream_id = %upstream.id,
        "refusing a Slack send: the tenant's oagw upstream aliased {SLACK_WEBHOOK_HOST} is not \
         the no-auth https://{SLACK_WEBHOOK_HOST}:443 upstream this gear provisions",
    );
    Err(untrusted_target())
}

/// Refuse a matched route that carries plugins — a plugin can rewrite the
/// request this gear hands over. [`check_upstream`]'s errors and reasoning.
fn check_route(route: &oagw_sdk::Route) -> Result<(), DomainError> {
    if has_plugins(route.plugins.as_ref()) {
        warn!(
            route_id = %route.id,
            "refusing a Slack send: the matched oagw route carries plugins",
        );
        return Err(untrusted_target());
    }
    Ok(())
}

fn has_plugins(plugins: Option<&oagw_sdk::PluginsConfig>) -> bool {
    plugins.is_some_and(|plugins| !plugins.items.is_empty())
}

/// The refusal [`check_upstream`] and [`check_route`] share.
fn untrusted_target() -> DomainError {
    egress(
        EgressFailure::Unreachable,
        format!(
            "the tenant's gateway route to {SLACK_WEBHOOK_HOST} is not the one this gear \
             provisions (endpoint, auth or plugins differ); nothing was sent"
        ),
    )
}

/// The one refusal for a reference that does not name a readable secret
/// holding a Slack incoming-webhook URL — see [`SlackOagwClient::webhook`] for
/// why "absent" and "malformed" share it.
fn unusable_reference() -> DomainError {
    DomainError::Validation {
        field: REF_FIELD.to_owned(),
        message: format!(
            "the reference does not name a readable secret that holds a Slack incoming-webhook \
             URL (https://{SLACK_WEBHOOK_HOST}/services/\u{2026}); {UNREADABLE_SECRET_HINT}"
        ),
    }
}

/// The tenant's Slack upstream: `hooks.slack.com` over HTTPS on 443, **no
/// auth** — the credential is the request path, which this adapter supplies.
fn upstream_request() -> CreateUpstreamRequest {
    CreateUpstreamRequest::builder(
        Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: SLACK_WEBHOOK_HOST.to_owned(),
                port: 443,
            }],
        },
        HTTP_PROTOCOL_ID,
    )
    .alias(SLACK_WEBHOOK_HOST)
    .enabled(true)
    .build()
}

/// Build the one proxied HTTP request for `message`, to `webhook`.
///
/// # Errors
///
/// [`DomainError::Internal`] with fixed text if the request cannot be built —
/// `http`'s own error is dropped rather than rendered, because the URI it
/// would describe is the credential.
fn build_request(
    webhook: &SlackWebhook,
    message: &SlackMessage,
) -> Result<http::Request<Body>, DomainError> {
    let payload = slack_webhook_payload(message);
    http::Request::builder()
        .method(http::Method::POST)
        .uri(webhook.proxy_path())
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .map_err(|_| DomainError::Internal("could not build the Slack request".to_owned()))
}

/// The outgoing Slack webhook payload. Legacy's `slack_webhook_payload`
/// (`manager/src/services/notifications.rs:880-904`), field for field: `text`
/// always, `channel` only when [`SlackMessage::channel`] is present and
/// non-blank after trimming, `blocks` only when non-empty.
///
/// The `blocks` value is [`super::block_kit::encode_blocks`]' output — review
/// finding #17 moved that encoding out of `domain::notify::render` and into
/// this adapter's own module.
fn slack_webhook_payload(message: &SlackMessage) -> serde_json::Value {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "text".to_owned(),
        serde_json::Value::String(message.text.clone()),
    );
    let channel = message
        .channel
        .as_deref()
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty());
    if let Some(channel) = channel {
        payload.insert(
            "channel".to_owned(),
            serde_json::Value::String(channel.to_owned()),
        );
    }
    if !message.blocks.is_empty() {
        payload.insert(
            "blocks".to_owned(),
            serde_json::Value::Array(super::block_kit::encode_blocks(&message.blocks)),
        );
    }
    serde_json::Value::Object(payload)
}

#[cfg(test)]
#[path = "slack_oagw_tests.rs"]
mod tests;
