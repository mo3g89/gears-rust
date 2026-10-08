//! [`JiraClient`] over the Outbound API Gateway — Task 32.
//!
//! # Do not reach for `reqwest` here
//!
//! Stated first because a reader arriving with a JIRA base URL in hand will
//! reach for an HTTP client by reflex, and legacy did exactly that
//! (`manager/src/services/jira.rs:10`, a `reqwest::Client` field). This
//! subsystem's egress contract, `cpt-cf-qa-contract-egress`, permits **one**
//! direct-HTTP exception and it belongs to qa-catalog's git transport. Every
//! JIRA call in this gear goes through `oagw`. A `reqwest` dependency in this
//! crate's `Cargo.toml` is the review finding, not the code that uses it.
//!
//! # The alias problem, and how this file answers it
//!
//! `ServiceGatewayClientV1::proxy_request` resolves a URI of the form
//! `/{alias}/{path_suffix}?query` (`gears/system/oagw/oagw-sdk/src/api.rs:140-156`),
//! and **nothing in this gear holds an alias**: `QaInsightsConfig` has no oagw
//! field and `qa_jira_config` has no alias column. It cannot have a static one
//! either — the JIRA URL is per-tenant data in `qa_jira_config.url`, so one
//! configured alias could not serve two tenants pointing at different
//! instances.
//!
//! So the alias is **derived from the tenant's own URL** and the upstream is
//! **provisioned if absent, at call time**:
//!
//! * [`derive_alias`] computes `host` (or `host:port` for a non-standard port),
//!   lowercased. That is deliberately the same value oagw's own
//!   `compute_derived_alias` would produce for a single hostname endpoint
//!   (`gears/system/oagw/oagw/src/domain/services/management/alias.rs:67-100`),
//!   so an upstream created here and one created for the same host by any other
//!   route are the same alias rather than two. An instance's **context path** is
//!   not part of the alias — [`Endpoint`] has no path field — and rides on the
//!   route and the request URI instead; [`JiraEgress`] carries that argument.
//! * [`OagwJiraClient::ensure_upstream`] — under the request bound, see
//!   `provisioned_egress` — calls `create_upstream` and, on
//!   `CanonicalError::AlreadyExists`, resolves the existing upstream by alias and
//!   ensures its route — the in-repo idiom, from
//!   `gears/mini-chat/mini-chat/src/infra/oagw_provisioning.rs:282-286` and its
//!   `reuse_existing_upstream` at `:315-336`.
//! * The result is cached in process, keyed on `(tenant_id, alias, base_path)`
//!   and **only once the route is ensured too**, so the steady state costs no
//!   gateway calls beyond the JIRA request itself.
//!
//! **The lookup on the conflict path is fix round 1's, and its finding 2 is why
//! it exists.** The first draft excluded `list_upstreams` to keep this adapter
//! small. Without it, a transient `create_route` failure was permanent: the
//! upstream had been created, so every later attempt — in this process *and
//! after a restart* — took the `AlreadyExists` arm, which registered no route,
//! and a route is mandatory for proxying
//! (`gears/system/oagw/oagw/src/domain/services/management/mod.rs:443-451`). The
//! tenant's JIRA integration stayed dead until somebody deleted the upstream by
//! hand.
//!
//! **There is still no boot-time provisioning loop and no retry/deferral
//! classification.** mini-chat has both because it provisions a fixed set of
//! configured LLM providers at startup; this gear's JIRA instances are tenant
//! rows that appear and change while the process runs, and a per-request adapter
//! is not the place to reimplement ~300 lines of boot machinery. A provisioning
//! failure here fails the one call that provoked it — and, since fix round 1,
//! leaves the cache untouched so the next call tries again.
//!
//! # Failures on the far side are classified
//!
//! A failure on the far side is `UpstreamEgress` with `channel = "jira"`,
//! classified as Slack's is: the gateway's own answer is `Unreachable`, JIRA's
//! `401`/`403` are `Authentication` (the secret behind
//! `api_token_credstore_ref` was refused), a deadline is `Timeout`, and any other
//! non-2xx is `Rejected`. `Internal` is left for this gear's own faults.
//!
//! # The credential: what oagw actually permits, and the constraint it forces
//!
//! The brief says the JIRA credential is supplied by this adapter from
//! credstore. **It cannot be supplied as a request header, and that was
//! established by reading oagw rather than by preference.** `apply_passthrough`
//! strips `authorization` from every proxied request unconditionally — it is in
//! `STRIPPED_HEADERS` and the strip runs even under `PassthroughMode::All`
//! (`gears/system/oagw/oagw/src/infra/proxy/headers.rs:13-18`, applied at
//! `:50-53`). Only two things downstream of that strip can put the header back:
//! the upstream's auth plugin (step 5 of the proxy pipeline,
//! `gears/system/oagw/oagw/src/infra/proxy/service.rs:681-703`) and the
//! upstream's own `headers.request.set` rules (step 5d, `:730-735`).
//!
//! Of the four auth plugins oagw registers
//! (`gears/system/oagw/oagw/src/infra/plugin/registry.rs:34-66`) exactly one
//! fits a static credential: `apikey`, which resolves a credstore `secret_ref`
//! and injects `prefix + secret` into a named header
//! (`infra/plugin/apikey_auth.rs:34-64`). So this adapter provisions the
//! upstream with
//! `{ header: "authorization", prefix: "Basic ", secret_ref: <the tenant's ref> }`,
//! and the credential is still resolved from credstore by the reference stored
//! in `qa_jira_config` — by oagw's plugin rather than by this process, which is
//! **stronger** than the brief's wording asks for: the material never enters
//! this gear at all, so there is no in-process copy to leak into a log, a span
//! or an error body.
//!
//! The cost is a deployment contract with no enforcement point, recorded here
//! because it has nowhere else to live: **the secret named by
//! `qa_jira_config.api_token_credstore_ref` must hold
//! `base64("email:api_token")`**, not the bare token, because HTTP Basic is what
//! JIRA Cloud's REST API takes (`jira.rs:81`, `:159`, `:269` — legacy calls
//! `.basic_auth(&config.email, Some(&config.api_token))` at all three sites) and
//! the plugin concatenates rather than encodes. The rejected alternative was
//! resolving the token here and provisioning
//! `headers.request.set = {authorization: "Basic …"}`: that puts live credential
//! material into another gear's configuration store, in plaintext, where this
//! design puts only a reference.
//!
//! The reference's **name** has a contract too, and unlike the contents one it
//! *is* enforced: the value must be what `SecretRef::new` accepts,
//! `[a-zA-Z0-9_-]` only (`credstore_sdk::SecretRef`'s own doc).
//! [`validate_credstore_ref`] refuses anything else, here and at the `PUT` — fix
//! round 1, finding 1, which found the syntax undocumented and violated by every
//! example in the change that introduced it.
//!
//! **`JiraConfig::email` is consequently read by nothing in this file.** It is
//! part of the config surface because legacy's is and because an operator needs
//! to see which account a token belongs to; it is not part of the request this
//! adapter builds.
//!
//! # Known limitation: a rotated credential reference needs the upstream dropped
//!
//! The upstream is created once per `(tenant, alias)` and never **updated**. If
//! a tenant changes `api_token_credstore_ref` while pointing at the *same* JIRA
//! host, the alias does not change, `create_upstream` answers `AlreadyExists`,
//! and the upstream keeps its original `secret_ref` — so the new reference has
//! no effect until the oagw upstream is deleted and recreated. This is a
//! deliberate consequence of this adapter carrying no reconcile machinery,
//! rather than an oversight.
//!
//! **Fix round 1 narrowed it without closing it.** The `AlreadyExists` arm now
//! resolves the upstream by alias, so its id *is* in hand — which is the piece an
//! `update_upstream` would need, and it is no longer missing. What is still
//! missing is the ownership tagging that would keep such a PUT from overwriting
//! an upstream another gear created for the same host, and that is the decision
//! left open. The remedy that needs no code at all remains the one an operator
//! should prefer: rotate the value *behind* the reference rather than the
//! reference itself.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use oagw_sdk::api::{ErrorSource, ServiceGatewayClientV1};
use oagw_sdk::{
    APIKEY_AUTH_PLUGIN_ID, AuthConfig, Body, CreateRouteRequest, CreateUpstreamRequest, Endpoint,
    HTTP_PROTOCOL_ID, HttpMatch, HttpMethod, ListQuery, MatchRules, PathSuffixMode, Scheme, Server,
    SharingMode,
};
use qa_insights_sdk::{JIRA_SUMMARY_MAX_CHARS, JiraConfig};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::domain::error::{DomainError, EgressFailure};
use crate::domain::ports::jira_client::{
    IssueRef, JiraClient, JiraIssue, NewIssue, StatusCategory, validate_credstore_ref,
};

/// The JIRA REST prefix every call in this file sits under.
///
/// The provisioned route's path is this **appended to the instance's context
/// path**, not this alone — [`JiraEgress::api_prefix`]. oagw matches a route by
/// `path.starts_with(&http_match.path)`
/// (`gears/system/oagw/oagw/src/infra/storage/route_repo.rs:143`), so one route
/// at that prefix with [`PathSuffixMode::Append`] covers `/search`, `/issue` and
/// `/issue/{key}` — the three suffixes below — rather than needing three routes.
const API_PREFIX: &str = "/rest/api/3";

/// The channel every [`DomainError::UpstreamEgress`] from this adapter names.
const CHANNEL: &str = "jira";

/// One [`DomainError::UpstreamEgress`] aimed at `host`. `detail` is fixed text
/// or an HTTP status, never JIRA's body: a JIRA error document can carry
/// instance detail (see `fetch_issue`).
fn egress_error(host: &str, failure: EgressFailure, detail: String) -> DomainError {
    DomainError::UpstreamEgress {
        channel: CHANNEL.to_owned(),
        endpoint: host.to_owned(),
        failure,
        detail,
    }
}

/// What a non-2xx from `send` means, by Slack's adapter's rule: the gateway
/// answering for an unreachable JIRA is `Unreachable`; 401/403 are JIRA
/// refusing the secret behind `api_token_credstore_ref` (`Authentication`);
/// anything else is JIRA refusing this request (`Rejected`).
fn refusal(host: &str, answer: &JiraAnswer, what: &str) -> DomainError {
    let status = answer.status.as_u16();
    if answer.from_gateway {
        return egress_error(
            host,
            EgressFailure::Unreachable,
            format!("{what}: the gateway could not reach JIRA (HTTP {status})"),
        );
    }
    let failure = match status {
        401 | 403 => EgressFailure::Authentication,
        _ => EgressFailure::Rejected,
    };
    egress_error(
        host,
        failure,
        format!("{what}: the instance answered HTTP {status}"),
    )
}

/// A dedupe search whose call failed: an outage is returned, anything else is
/// logged and falls through to the create (legacy's fall-through).
fn search_failed(e: DomainError) -> Result<Option<String>, DomainError> {
    if is_outage(&e) {
        return Err(e);
    }
    warn!(
        failure = %e.failure_class(),
        error = %e,
        "the JIRA dedupe search failed; filing a new issue rather than dropping the report \
         (legacy's own fall-through)",
    );
    Ok(None)
}

/// A dedupe search answered with a non-2xx: an outage (the gateway answering
/// for JIRA, or JIRA refusing the credential) is returned, any other refusal
/// is logged and falls through to the create.
fn search_refused(host: &str, answer: &JiraAnswer) -> Result<Option<String>, DomainError> {
    let refused = refusal(host, answer, "the JIRA dedupe search");
    if is_outage(&refused) {
        return Err(refused);
    }
    warn!(
        status = %answer.status,
        failure = %refused.failure_class(),
        "the JIRA dedupe search was refused; filing a new issue rather than dropping the report",
    );
    Ok(None)
}

/// Whether `err` means this tenant's JIRA cannot be used right now: the gateway
/// could not reach it, a call timed out, or JIRA refused the credential. Any
/// later call to the same endpoint with the same credential would fail the
/// same way. `Rejected` (JIRA refusing one request) is not an outage.
const fn is_outage(err: &DomainError) -> bool {
    matches!(
        err,
        DomainError::UpstreamEgress {
            failure: EgressFailure::Unreachable
                | EgressFailure::Timeout
                | EgressFailure::Authentication,
            ..
        }
    )
}

/// One JIRA exchange: the status, whether the gateway generated the response
/// itself, and the capped body.
struct JiraAnswer {
    status: http::StatusCode,
    from_gateway: bool,
    body: Vec<u8>,
}

/// The label legacy puts on every issue it files (`jira.rs:71`, `:151`) and
/// searches by (`:71`).
///
/// Kept verbatim, `vhp-` prefix and all: it is the JQL predicate that makes the
/// dedupe probe find issues legacy filed, and renaming it would silently split
/// the two systems' issue sets during a migration.
const FAILURE_LABEL: &str = "vhp-test-failure";

/// Legacy's issue type when the config names none (`jira.rs:112`).
const DEFAULT_ISSUE_TYPE: &str = "Bug";

/// Legacy truncates the log excerpt at 2000 characters (`jira.rs:114-118`).
const MAX_LOG_CHARS: usize = 2000;

/// [`JiraClient`] over `oagw`.
///
/// See this module's header for the alias derivation, the provisioning
/// behaviour, and why the credential is an upstream auth plugin rather than a
/// request header.
pub struct OagwJiraClient {
    gateway: Arc<dyn ServiceGatewayClientV1>,
    /// The bound [`Self::send`] applies to one proxied request —
    /// [`Self::REQUEST_TIMEOUT`] in production, always. See that constant's
    /// own doc for why this adapter had none until this task.
    timeout: Duration,
    /// The `(tenant_id, alias, base_path)` triples this process has already
    /// ensured **both** an upstream *and* a route for.
    ///
    /// # All three components are load-bearing
    ///
    /// `base_path` is in the key because two JIRA instances on the same host
    /// under different context paths share an alias and need *different* routes
    /// (see [`JiraEgress`]); keyed on `(tenant, alias)` alone, a tenant that
    /// moved its instance from `https://h` to `https://h/jira` would find the
    /// key already marked and never register the second route.
    ///
    /// # A key is inserted only when the route is ensured too
    ///
    /// **Fix round 1, finding 2.** This used to be marked unconditionally, right
    /// after the upstream branch, while route failures were swallowed — so one
    /// transient `create_route` failure disabled that tenant's JIRA egress for
    /// the life of the process, and a restart made it permanent (the upstream now
    /// existed, so the reuse arm answered `AlreadyExists` and never registered a
    /// route at all). A route is *mandatory* for proxying
    /// (`gears/system/oagw/oagw/src/domain/services/management/mod.rs:443-451`),
    /// so the integration stayed dead until somebody deleted the upstream by
    /// hand.
    ///
    /// A `std::sync::Mutex` rather than an async one, and every access is a
    /// lock-read-drop or a lock-write-drop with no `await` inside — the
    /// workspace denies `clippy::await_holding_lock`, and this set is contended
    /// for nanoseconds.
    ///
    /// **In process, not shared.** A second replica provisions once of its own,
    /// gets `AlreadyExists`, resolves the existing upstream and ensures its
    /// route.
    provisioned: Mutex<HashSet<(Uuid, String, String)>>,
}

/// Where one tenant's JIRA lives, as oagw needs it addressed.
///
/// # `base_path` exists because legacy preserves one
///
/// **Fix round 1, finding 3.** [`egress_for`] used to keep only the authority and
/// a comment claimed legacy did the same. It does not: every legacy call site is
/// `format!("{}/rest/api/3/...", config.url.trim_end_matches('/'))`
/// (`manager/src/services/jira.rs:76`, `:144`, `:261-265`), which strips a
/// *trailing slash* and preserves everything else — so `https://host/jira`, the
/// default for JIRA Data Center behind a context path, works in legacy and 404'd
/// here. Dropping it was a functional regression recorded as a non-divergence.
///
/// `oagw_sdk::Endpoint` has no path field (`oagw-sdk/src/models.rs:40-45`), so
/// the context path cannot ride on the upstream. It rides on the **route** and on
/// the proxy URI instead, which works because the URL oagw finally builds is
/// `endpoint + route_path + remaining_suffix`
/// (`oagw/src/infra/proxy/service.rs:785-800`, `request_builder::build_upstream_url`):
/// with a route at `/jira/rest/api/3` and a request URI of
/// `/{alias}/jira/rest/api/3/search`, the suffix left over is `/search` and the
/// upstream URL is `https://host/jira/rest/api/3/search` — legacy's own.
#[derive(Clone, Debug, PartialEq, Eq)]
struct JiraEgress {
    endpoint: Endpoint,
    /// The instance's context path, normalised: `""` for none, otherwise a
    /// leading slash and no trailing one (`/jira`, `/apps/jira`).
    base_path: String,
    /// The oagw alias for [`Self::endpoint`] — see [`derive_alias`].
    alias: String,
}

impl JiraEgress {
    /// `{base_path}/rest/api/3` — the route's path prefix, and the prefix every
    /// request URI carries after the alias segment.
    fn api_prefix(&self) -> String {
        format!("{}{API_PREFIX}", self.base_path)
    }

    /// `/{alias}{base_path}/rest/api/3` — what a proxy URI is built on.
    fn uri_prefix(&self) -> String {
        format!("/{}{}", self.alias, self.api_prefix())
    }
}

/// The most bytes of one JIRA response body [`OagwJiraClient::send`] will
/// buffer.
///
/// Every call this adapter makes returns a single JSON document about at most
/// one issue: the dedupe search asks for `maxResults=1`, the create answers
/// `{id, key, self}`, and the issue read is one issue. An issue with hundreds of
/// comments is still well under a megabyte, so 2 MiB leaves generous headroom
/// while stopping a runaway or hostile upstream from being buffered whole.
/// `Body::into_bytes` cannot enforce this — it reads a stream to the end before
/// returning — so [`read_capped`] walks the stream itself.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Buffer `body`, failing as soon as it is known to exceed
/// [`MAX_RESPONSE_BYTES`] rather than after reading all of it.
async fn read_capped(body: Body) -> Result<Vec<u8>, DomainError> {
    use futures_util::StreamExt;

    let mut stream = body.into_stream();
    let mut buffer = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| DomainError::Internal(format!("could not read the JIRA response: {e}")))?;
        if buffer.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(DomainError::Internal(format!(
                "the JIRA response body exceeded {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer)
}

impl OagwJiraClient {
    /// The fixed bound [`Self::send`] applies to one proxied request.
    ///
    /// # Ported reasoning, not a guess — this task closes the gap between
    /// # this adapter and its Slack sibling
    ///
    /// `infra::notify::slack_oagw::SlackOagwClient::REQUEST_TIMEOUT` carries
    /// legacy's own 10-second bound
    /// (`manager/src/services/notifications.rs:58-59`, "10s is generous for
    /// a webhook - Slack's own guidance is ~3s") and this adapter had no
    /// timeout of its own at all: the only bound on one call was `oagw`'s own
    /// 30-second default (`gears/system/oagw/oagw/src/infra/proxy/service.rs:48`),
    /// and `domain::service::jira_poller`'s `run_pass` awaits **one
    /// `check_status` call per open bug, sequentially, inside one
    /// leader-claimed tick** (`crate::infra::leader::ROLE_JIRA_POLLER`) — so
    /// a single slow or black-holing JIRA instance could stall that whole
    /// tick for 30 seconds times however many bugs are open, rather than
    /// losing just the one lookup. The same "bound the one call rather than
    /// the whole loop" reasoning Slack's own module header records, applied
    /// here.
    ///
    /// Provisioning gets the same bound, separately — see
    /// [`Self::provisioned_egress`].
    pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

    #[must_use]
    pub fn new(gateway: Arc<dyn ServiceGatewayClientV1>) -> Self {
        Self {
            gateway,
            timeout: Self::REQUEST_TIMEOUT,
            provisioned: Mutex::new(HashSet::new()),
        }
    }

    /// Test-only escape hatch: a client whose bound is *not*
    /// [`Self::REQUEST_TIMEOUT`], so a test can exercise the bound with a
    /// duration measured in milliseconds rather than the real ten seconds —
    /// `SlackOagwClient::with_timeout`'s own reason, mirrored here.
    /// Production code has exactly one way to build this type —
    /// [`Self::new`] — and always gets the real bound.
    #[cfg(test)]
    fn with_timeout(gateway: Arc<dyn ServiceGatewayClientV1>, timeout: Duration) -> Self {
        Self {
            gateway,
            timeout,
            provisioned: Mutex::new(HashSet::new()),
        }
    }

    /// [`Self::ensure_upstream`] under the same bound [`Self::send`] applies.
    ///
    /// Provisioning is up to three gateway calls (`create_upstream`, the
    /// `list_upstreams` pages, `create_route`) and used to run *before* the
    /// request's timeout began, so a gateway hanging there stalled the
    /// leader-claimed poller tick with nothing to stop it — Slack's adapter
    /// already provisions inside its bound. A separate bound rather than one
    /// around provisioning *and* the request, because `create_or_find_issue`
    /// makes two requests after it and each keeps its own.
    async fn provisioned_egress(
        &self,
        ctx: &SecurityContext,
        config: &JiraConfig,
    ) -> Result<JiraEgress, DomainError> {
        tokio::time::timeout(self.timeout, self.ensure_upstream(ctx, config))
            .await
            .map_err(|_| {
                // Pure, and a bad URL fails inside `ensure_upstream` with its
                // own `Validation` before this bound can matter.
                let host = egress_for(&config.url)
                    .map(|e| e.endpoint.host)
                    .unwrap_or_default();
                egress_error(
                    &host,
                    EgressFailure::Timeout,
                    format!(
                        "provisioning the JIRA egress did not complete within {:?}",
                        self.timeout
                    ),
                )
            })?
    }

    /// Ensure the tenant has an oagw upstream *and* a route for this config's
    /// JIRA instance, and return how to address it.
    ///
    /// Idempotent, and after the first success per `(tenant, alias, base_path)`
    /// it costs no gateway calls at all.
    ///
    /// # Errors
    ///
    /// [`DomainError::JiraNotConfigured`] for a disabled config — raised before
    /// any gateway call, so a tenant that turned the integration off provisions
    /// no egress.
    /// [`DomainError::Validation`] naming `url` or `api_token_credstore_ref` for
    /// a stored value oagw could not use.
    /// [`DomainError::UpstreamEgress`] (`Unreachable`) when the gateway refused
    /// the upstream or the route.
    async fn ensure_upstream(
        &self,
        ctx: &SecurityContext,
        config: &JiraConfig,
    ) -> Result<JiraEgress, DomainError> {
        if !config.enabled {
            // Legacy's own refusal, `jira.rs:65-67` ("JIRA integration is
            // disabled"). Raised before any gateway call so a disabled config
            // provisions nothing.
            return Err(DomainError::JiraNotConfigured);
        }

        // Checked here as well as at the `PUT`, so a row stored before that rule
        // existed fails with a named field rather than as an opaque
        // `PluginError::Internal` from inside oagw at request time. One
        // definition, two call sites — see the function's own doc.
        validate_credstore_ref("api_token_credstore_ref", &config.api_token_credstore_ref)?;

        let egress = egress_for(&config.url)?;
        let key = (
            ctx.subject_tenant_id(),
            egress.alias.clone(),
            egress.base_path.clone(),
        );

        if self.is_provisioned(&key) {
            return Ok(egress);
        }

        let request = upstream_request(config, &egress);

        match self.gateway.create_upstream(ctx.clone(), request).await {
            Ok(upstream) => {
                debug!(
                    alias = %upstream.alias,
                    upstream_id = %upstream.id,
                    "provisioned an oagw upstream for a tenant's JIRA instance",
                );
                self.ensure_route(ctx, upstream.id, &egress).await?;
                self.mark_provisioned(key);
            }
            Err(CanonicalError::AlreadyExists { resource_name, .. }) => {
                // Another replica, or an earlier run of this process, already
                // provisioned the upstream. **The route still has to be
                // ensured** — fix round 1 finding 2: without this lookup a
                // restart after a failed route registration left the tenant's
                // egress permanently dead, because the upstream existed and
                // nothing ever registered a route against it. `mini-chat`'s
                // `reuse_existing_upstream` (`oagw_provisioning.rs:315-336`) is
                // the precedent for recovering the upstream behind the conflict.
                let taken = resource_name.as_deref().unwrap_or(&egress.alias);
                if self.reuse_existing_upstream(ctx, taken, &egress).await? {
                    self.mark_provisioned(key);
                }
            }
            Err(e) => {
                return Err(egress_error(
                    &egress.endpoint.host,
                    EgressFailure::Unreachable,
                    format!(
                        "could not provision an oagw upstream for the tenant's JIRA instance ({})",
                        e.title()
                    ),
                ));
            }
        }

        Ok(egress)
    }

    /// Resolve the upstream behind an `AlreadyExists` conflict and ensure its
    /// route. `true` when both succeeded and the key may be cached.
    ///
    /// `false` — with a WARN and **no** error — when the upstream could not be
    /// looked up: it exists, so we cannot re-create it, but we could not name its
    /// id either and therefore cannot say its route is there. The caller proceeds
    /// with the request anyway (the route was almost certainly created with the
    /// upstream, and if it was not the JIRA call reports it) and leaves the key
    /// uncached so the next call tries again. `mini-chat`'s
    /// `reuse_existing_upstream` treats the same case as `Deferred`
    /// (`oagw_provisioning.rs:315-336`).
    ///
    /// # Errors
    ///
    /// [`DomainError::UpstreamEgress`] when the route could not be registered — see
    /// [`Self::ensure_route`].
    async fn reuse_existing_upstream(
        &self,
        ctx: &SecurityContext,
        alias: &str,
        egress: &JiraEgress,
    ) -> Result<bool, DomainError> {
        let Some(upstream) = self.find_upstream_by_alias(ctx, alias).await else {
            warn!(
                alias = %egress.alias,
                "the oagw upstream for this JIRA instance already exists but could not be \
                 looked up by alias; its route could not be ensured and the next call will \
                 try again",
            );
            return Ok(false);
        };
        debug!(
            alias = %upstream.alias,
            upstream_id = %upstream.id,
            "an oagw upstream for this JIRA instance already exists; reusing it and ensuring \
             its route",
        );
        self.ensure_route(ctx, upstream.id, egress).await?;
        Ok(true)
    }

    /// Register the one route the three JIRA calls need, on `upstream_id`.
    ///
    /// # A failure here is an error, and that is fix round 1's finding 2
    ///
    /// This used to log and swallow *every* failure, on the reasoning that "the
    /// JIRA call that follows will report the real failure". That is true of the
    /// first call and false of every one after it, because the caller then
    /// cached the key and never tried again. It now takes `mini-chat`'s shape
    /// exactly (`oagw_provisioning.rs:571-580`): a **duplicate** registration is
    /// success — the route is there, which is all this method promises — and
    /// everything else propagates.
    ///
    /// # Errors
    ///
    /// [`DomainError::UpstreamEgress`] (`Unreachable`) when the gateway refused the route for any
    /// reason other than its already existing.
    async fn ensure_route(
        &self,
        ctx: &SecurityContext,
        upstream_id: Uuid,
        egress: &JiraEgress,
    ) -> Result<(), DomainError> {
        let route_path = egress.api_prefix();
        let match_rules = MatchRules {
            http: Some(HttpMatch {
                // Both methods on one route: the search and the two issue reads
                // are `GET`, the create is `POST`, and oagw's route match is
                // (methods, path-prefix) so a second route would only differ in
                // the method list.
                methods: vec![HttpMethod::Get, HttpMethod::Post],
                // Carries the instance's context path, if it has one — see
                // [`JiraEgress`] for why the base path rides here and not on the
                // upstream.
                path: route_path.clone(),
                // Enforced by oagw against every proxied request
                // (`infra/proxy/service.rs:604-621`): a query parameter absent
                // from this list is a 400 from the gateway, not a passthrough.
                // These are the two `GET /search` takes (`jira.rs:80`).
                query_allowlist: vec!["jql".to_owned(), "maxResults".to_owned()],
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
                debug!(%upstream_id, route_id = %route.id, %route_path, "registered the JIRA route");
                Ok(())
            }
            // Idempotent re-run against a reused upstream: the route is already
            // there, which is exactly what this method exists to guarantee.
            Err(CanonicalError::AlreadyExists { .. }) => {
                debug!(%upstream_id, %route_path, "the JIRA route is already registered");
                Ok(())
            }
            Err(e) => Err(egress_error(
                &egress.endpoint.host,
                EgressFailure::Unreachable,
                format!(
                    "could not register the JIRA route on the oagw upstream ({})",
                    e.title()
                ),
            )),
        }
    }

    /// The upstream carrying `alias`, or `None`.
    ///
    /// `mini-chat`'s `find_upstream_by_alias` (`oagw_provisioning.rs:393-417`),
    /// paging included: a page shorter than the requested `top` ends the walk, so
    /// this terminates whether or not the alias is present. A list failure is
    /// `None` with a WARN rather than an error — the caller treats "could not
    /// resolve it" as "do not cache", which retries, and that is a better answer
    /// than failing a JIRA call over a diagnostic lookup.
    ///
    /// Reached only on the `AlreadyExists` path, so the steady state still costs
    /// no `list_upstreams` call at all. Fix round 1 lifted the first draft's
    /// exclusion of this lookup for exactly this purpose.
    async fn find_upstream_by_alias(
        &self,
        ctx: &SecurityContext,
        alias: &str,
    ) -> Option<oagw_sdk::Upstream> {
        let mut query = ListQuery::default();
        loop {
            let page = match self.gateway.list_upstreams(ctx.clone(), &query).await {
                Ok(page) => page,
                Err(e) => {
                    warn!(alias, error = %e, "the oagw upstream lookup by alias failed");
                    return None;
                }
            };
            let page_len = page.len();
            if let Some(found) = page.into_iter().find(|u| u.alias == alias) {
                return Some(found);
            }
            if page_len < query.top as usize {
                return None;
            }
            query.skip += query.top;
        }
    }

    fn is_provisioned(&self, key: &(Uuid, String, String)) -> bool {
        self.provisioned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(key)
    }

    fn mark_provisioned(&self, key: (Uuid, String, String)) {
        self.provisioned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key);
    }

    /// One proxied request, returning the status, whether the gateway generated
    /// the response itself, and the body.
    ///
    /// The status is handed back rather than turned into an error here because
    /// the three callers disagree about what a non-2xx means: the dedupe search
    /// treats one as "no match" (legacy `jira.rs:83`), the create treats one as a
    /// failure (`:164-168`), and the issue read treats one as a failure too
    /// (`:274-276`).
    async fn send(
        &self,
        ctx: &SecurityContext,
        host: &str,
        method: http::Method,
        uri: String,
        body: Body,
    ) -> Result<JiraAnswer, DomainError> {
        let mut builder = http::Request::builder().method(method).uri(&uri);
        if !body.is_empty() {
            // `Content-Type` is the one header oagw forwards regardless of
            // passthrough mode (`infra/proxy/headers.rs:43-47`), which is why
            // this adapter configures no `HeadersConfig` at all.
            builder = builder.header(http::header::CONTENT_TYPE, "application/json");
        }
        let request = builder
            .body(body)
            .map_err(|e| DomainError::Internal(format!("could not build the JIRA request: {e}")))?;

        // One deadline covers the proxy call AND the body read. Bounding only
        // `proxy_request` left the read below it unbounded: the gateway hands
        // back a response as soon as the headers are in, so a JIRA instance that
        // then trickles (or never finishes) the body stalled the leader-claimed
        // poller tick with nothing to stop it. See `Self::REQUEST_TIMEOUT`'s own
        // doc for why this adapter's calls are bounded at all; Slack's adapter
        // wraps its one proxy call the same way.
        let exchange = async {
            let response = self
                .gateway
                .proxy_request(ctx.clone(), request)
                .await
                .map_err(|e| match e {
                    CanonicalError::DeadlineExceeded { .. } => egress_error(
                        host,
                        EgressFailure::Timeout,
                        "the gateway's request to JIRA timed out".to_owned(),
                    ),
                    // The title, not `{e}`: a gateway error's text can carry
                    // detail that must not reach an operator-facing string.
                    other => egress_error(
                        host,
                        EgressFailure::Unreachable,
                        format!(
                            "the gateway could not deliver the request ({})",
                            other.title()
                        ),
                    ),
                })?;
            let status = response.status();
            let from_gateway =
                response.extensions().get::<ErrorSource>() == Some(&ErrorSource::Gateway);
            let body = read_capped(response.into_body()).await?;
            Ok::<_, DomainError>(JiraAnswer {
                status,
                from_gateway,
                body,
            })
        };
        tokio::time::timeout(self.timeout, exchange)
            .await
            .map_err(|_| {
                egress_error(
                    host,
                    EgressFailure::Timeout,
                    format!("no answer within {:?}", self.timeout),
                )
            })?
    }

    /// The JQL dedupe probe, returning the key of an already-open issue for this
    /// test.
    ///
    /// **A failure that is not an outage is `Ok(None)`, not an error**, and
    /// that is legacy's behaviour rather than a simplification: the whole
    /// search block sits inside `if search_resp.status().is_success()`
    /// (`jira.rs:86-108`), and the fall-through then files a duplicate issue.
    /// Duplicating an issue is recoverable; dropping a bug report because JIRA
    /// answered one search badly is not. That covers JIRA's own non-2xx other
    /// than `401`/`403`, and a body that is not JSON.
    ///
    /// **An outage is `Err`**: an `Unreachable`, `Timeout` or `Authentication`
    /// failure ([`is_outage`]). The create goes to the same endpoint through
    /// the same gateway with the same credential, so it would fail the same way,
    /// and attempting it doubled one attempt's cost (provisioning, search and
    /// create, each under its own bound, came to about the API gateway's 30 s
    /// request timeout).
    async fn search_for_open_issue(
        &self,
        ctx: &SecurityContext,
        egress: &JiraEgress,
        project_key: &str,
        test_name: &str,
    ) -> Result<Option<String>, DomainError> {
        let Ok(query) = serde_urlencoded::to_string([
            ("jql", jql_for(project_key, test_name)),
            ("maxResults", "1".to_owned()),
        ]) else {
            return Ok(None);
        };

        let answer = self
            .send(
                ctx,
                &egress.endpoint.host,
                http::Method::GET,
                format!("{}/search?{query}", egress.uri_prefix()),
                Body::Empty,
            )
            .await;
        let answer = match answer {
            Ok(answer) => answer,
            Err(e) => return search_failed(e),
        };

        if !answer.status.is_success() {
            return search_refused(&egress.endpoint.host, &answer);
        }

        let document: serde_json::Value = match serde_json::from_slice(&answer.body) {
            Ok(document) => document,
            Err(error) => {
                // Same treatment as the HTTP-error arms above. A 200 whose body
                // is not JSON is a gateway or proxy answering in place of JIRA;
                // dropping it silently makes the caller re-file a bug it thinks
                // is still open, with nothing in the log to explain the
                // duplicate. Review finding #28.
                warn!(%error, "JIRA answered 200 with a body that could not be parsed as JSON");
                return Ok(None);
            }
        };
        Ok(document
            .pointer("/issues/0/key")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned))
    }

    /// `GET /rest/api/3/issue/{key}` — the call [`JiraClient::check_status`] is
    /// a projection of (`jira.rs:261-287`). The port's `get_issue`, which
    /// returned this whole issue, was deleted unused in finding #38's triage;
    /// this module's own tests are what still read the summary and the key.
    async fn fetch_issue(
        &self,
        ctx: &SecurityContext,
        config: &JiraConfig,
        jira_key: &str,
    ) -> Result<JiraIssue, DomainError> {
        let egress = self.provisioned_egress(ctx, config).await?;
        let answer = self
            .send(
                ctx,
                &egress.endpoint.host,
                http::Method::GET,
                format!("{}/issue/{jira_key}", egress.uri_prefix()),
                Body::Empty,
            )
            .await?;

        if !answer.status.is_success() {
            // Legacy's message shape (`jira.rs:275`), which deliberately does
            // not quote the response body: a JIRA error document can contain
            // instance detail, and this string reaches an operator through
            // the error.
            return Err(refusal(
                &egress.endpoint.host,
                &answer,
                &format!("failed to get JIRA issue {jira_key}"),
            ));
        }

        let document: serde_json::Value = serde_json::from_slice(&answer.body).map_err(|e| {
            DomainError::Internal(format!("the JIRA issue response is not JSON: {e}"))
        })?;

        Ok(JiraIssue {
            key: jira_key.to_owned(),
            summary: document
                .pointer("/fields/summary")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            status_category: document
                .pointer("/fields/status/statusCategory/key")
                .and_then(serde_json::Value::as_str)
                .map_or_else(StatusCategory::unknown, StatusCategory::new),
        })
    }
}

#[async_trait]
impl JiraClient for OagwJiraClient {
    async fn create_or_find_issue(
        &self,
        ctx: &SecurityContext,
        config: &JiraConfig,
        issue: NewIssue,
    ) -> Result<IssueRef, DomainError> {
        let egress = self.provisioned_egress(ctx, config).await?;
        let summary = summary_for(&issue.test_name);

        // Step 1: the JQL dedupe probe (`jira.rs:70-108`).
        if let Some(existing) = self
            .search_for_open_issue(ctx, &egress, &config.project_key, &issue.test_name)
            .await?
        {
            return Ok(IssueRef {
                jira_key: existing,
                created: false,
            });
        }

        // Step 2: the create (`jira.rs:144-174`).
        let issue_type = config.issue_type.as_deref().unwrap_or(DEFAULT_ISSUE_TYPE);
        let payload = serde_json::json!({
            "fields": {
                "project": { "key": config.project_key },
                "summary": summary,
                "description": description_document(&issue),
                "issuetype": { "name": issue_type },
                "labels": [FAILURE_LABEL],
            }
        });
        let answer = self
            .send(
                ctx,
                &egress.endpoint.host,
                http::Method::POST,
                format!("{}/issue", egress.uri_prefix()),
                Body::from(payload.to_string()),
            )
            .await?;

        if !answer.status.is_success() {
            return Err(refusal(
                &egress.endpoint.host,
                &answer,
                "creating the issue",
            ));
        }

        let document: serde_json::Value = serde_json::from_slice(&answer.body).map_err(|e| {
            DomainError::Internal(format!("the JIRA create response is not JSON: {e}"))
        })?;
        let jira_key = document
            .get("key")
            .and_then(serde_json::Value::as_str)
            // Legacy's `"No key in JIRA response"` (`jira.rs:174`). An issue
            // whose key this gear cannot read is worse than a failed create: the
            // issue exists and nothing can register it.
            .ok_or_else(|| {
                DomainError::Internal(
                    "JIRA accepted the issue but its response carried no key".to_owned(),
                )
            })?
            .to_owned();

        Ok(IssueRef {
            jira_key,
            created: true,
        })
    }

    async fn check_status(
        &self,
        ctx: &SecurityContext,
        config: &JiraConfig,
        jira_key: &str,
    ) -> Result<StatusCategory, DomainError> {
        Ok(self
            .fetch_issue(ctx, config, jira_key)
            .await?
            .status_category)
    }
}

/// The `create_upstream` request for one tenant's JIRA instance.
///
/// The auth config is the whole credential story — see this module's header for
/// why it is an oagw apikey plugin rather than a request header, and what the
/// secret behind [`JiraConfig::api_token_credstore_ref`] must contain.
fn upstream_request(config: &JiraConfig, egress: &JiraEgress) -> CreateUpstreamRequest {
    let auth = AuthConfig {
        plugin_type: APIKEY_AUTH_PLUGIN_ID.to_owned(),
        // `Private`: the credential reference belongs to this tenant and must not
        // be inherited by a child tenant that happens to point at the same JIRA
        // host.
        sharing: SharingMode::Private,
        config: Some(HashMap::from([
            ("header".to_owned(), "authorization".to_owned()),
            ("prefix".to_owned(), "Basic ".to_owned()),
            (
                "secret_ref".to_owned(),
                config.api_token_credstore_ref.clone(),
            ),
        ])),
    };

    CreateUpstreamRequest::builder(
        Server {
            endpoints: vec![egress.endpoint.clone()],
        },
        HTTP_PROTOCOL_ID,
    )
    .alias(egress.alias.clone())
    .auth(auth)
    .enabled(true)
    .build()
}

/// The issue summary. Legacy's format string, verbatim (`jira.rs:113`), now
/// bounded to [`JIRA_SUMMARY_MAX_CHARS`] — this task's fix.
///
/// Without this, a `test_name` near the top of its `VARCHAR(512)` column
/// produces a summary past JIRA's 255-character field limit, which JIRA
/// answers with a 400; [`crate::domain::service::jira`]'s `file_bugs` calls
/// `file_one` per failing test and, on an `Err`, logs it at error and moves
/// on to the next one rather than propagating — so the failing test whose
/// name was too long never gets a bug at all, silently as far as the caller
/// of `file_bugs` is concerned. Truncating the whole formatted string,
/// prefix included, the same way
/// [`truncate_logs`] truncates its field: a character count taken directly,
/// with no separate over-length check needed because `.take()` is already a
/// no-op on a summary shorter than the bound.
///
/// `pub(crate)` rather than private so this module's own tests can pin it
/// against [`crate::domain::service::jira::bug_summary`] — Task 33's
/// duplicate of this exact literal, kept in agreement by
/// `the_stored_summary_matches_the_one_sent_to_jira` (and, past the
/// truncation boundary, `the_stored_summary_matches_the_one_sent_to_jira_past_the_truncation_boundary`)
/// rather than compile-time sharing, because the domain layer must not
/// depend on this adapter. The truncation *bound* itself, unlike the format
/// string, is shared: both this function and `bug_summary` take
/// [`JIRA_SUMMARY_MAX_CHARS`] from `qa_insights_sdk`, the one crate both
/// layers already legitimately depend on — so there is exactly one `255`,
/// not two bare copies for the pinning test to reconcile after the fact.
pub fn summary_for(test_name: &str) -> String {
    format!("[VHP] Test Failed: {test_name}")
        .chars()
        .take(JIRA_SUMMARY_MAX_CHARS)
        .collect()
}

/// The dedupe JQL. Legacy's, verbatim (`jira.rs:70-74`).
///
/// The `"` escaping is legacy's `test_name.replace('"', "\\\"")` and it is the
/// only escaping legacy performs — a test name containing a backslash still
/// produces malformed JQL there. Reproduced rather than fixed because a *better*
/// escape changes which issues the probe finds, and the failure mode of the
/// existing one is a search that returns nothing (a duplicate issue), not a
/// search that returns somebody else's issue.
fn jql_for(project_key: &str, test_name: &str) -> String {
    let escaped = test_name.replace('"', "\\\"");
    format!(
        "project = {project_key} AND labels = \"{FAILURE_LABEL}\" AND summary ~ \"{escaped}\" \
         AND status != Done"
    )
}

/// The issue body, in Atlassian Document Format.
///
/// Legacy's document, structure for structure (`jira.rs:120-142`): one paragraph
/// of context, then a `text`-language code block holding the truncated logs. The
/// `None` substitutions are legacy's too — `"default"` for the platform and
/// `"unknown"` for the version (`:129-130`).
fn description_document(issue: &NewIssue) -> serde_json::Value {
    let context = format!(
        "Test '{}' failed in run '{}'\nPlan: {}\nPlatform: {}\nVersion: {}",
        issue.test_name,
        issue.run_name,
        issue.plan,
        issue.platform.as_deref().unwrap_or("default"),
        issue.app_version.as_deref().unwrap_or("unknown"),
    );
    serde_json::json!({
        "type": "doc",
        "version": 1,
        "content": [
            {
                "type": "paragraph",
                "content": [ { "type": "text", "text": context } ]
            },
            {
                "type": "codeBlock",
                "attrs": { "language": "text" },
                "content": [ { "type": "text", "text": truncate_logs(&issue.logs) } ]
            }
        ]
    })
}

/// The log excerpt, at most [`MAX_LOG_CHARS`] **characters**.
///
/// Legacy slices bytes — `&logs[..2000]` (`jira.rs:115`) — which panics on a
/// multi-byte boundary. Not reproduced: a filing path that can panic on a UTF-8
/// log line is a defect, not a behaviour, and the observable difference is
/// confined to logs containing non-ASCII within the last few bytes of the cut.
fn truncate_logs(logs: &str) -> String {
    logs.chars().take(MAX_LOG_CHARS).collect()
}

/// Split a JIRA base URL into the endpoint oagw addresses, the context path it
/// must preserve, and the alias to reach it by.
///
/// # The context path is kept, and that is fix round 1's finding 3
///
/// Legacy builds every request as
/// `format!("{}/rest/api/3/...", config.url.trim_end_matches('/'))`
/// (`manager/src/services/jira.rs:76`, `:144`, `:261-265`): a **trailing slash**
/// is stripped and nothing else is. `https://jira.example.com/jira` therefore
/// works there, and an earlier revision of this function dropped everything after
/// the authority — which 404'd every call for any JIRA Data Center behind a
/// context path, under a comment asserting legacy did the same. See
/// [`JiraEgress`] for where the path goes instead, since
/// [`Endpoint`](oagw_sdk::Endpoint) has no field for it.
///
/// A query string or a fragment on a *base* URL is meaningless and is dropped,
/// which is also what legacy's `trim_end_matches('/')` plus string concatenation
/// would do to them — badly (it would produce
/// `https://host?x=1/rest/api/3/search`). Dropping is the closest honest
/// reproduction.
///
/// # Errors
///
/// [`DomainError::Validation`] naming `url` when the value has no recognised
/// scheme, no host, a port that is not a number, or a host oagw would not accept
/// as an alias. The field name is the `qa_jira_config` column and the
/// settings-form field, which is what an operator can act on.
fn egress_for(url: &str) -> Result<JiraEgress, DomainError> {
    let invalid = |why: &str| DomainError::Validation {
        field: "url".to_owned(),
        message: format!("the JIRA base URL is not usable as an egress endpoint: {why}"),
    };

    let trimmed = url.trim();
    let (scheme, rest) = if let Some(rest) = trimmed.strip_prefix("https://") {
        (Scheme::Https, rest)
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        // Accepted here and refused by oagw itself unless the deployment sets
        // `allow_http_upstream` (`infra/proxy/service.rs:760-768`). Refusing it
        // here as well would be this gear deciding another gear's policy.
        (Scheme::Http, rest)
    } else {
        return Err(invalid("it must begin with https:// or http://"));
    };

    // A base URL's query and fragment are meaningless; drop them before the
    // authority/path split so neither can end up inside a host or a route path.
    let rest = rest.split(['?', '#']).next().unwrap_or_default();

    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, ""),
    };
    if authority.contains('@') {
        return Err(invalid("credentials in the URL are not supported"));
    }
    if authority.starts_with('[') {
        return Err(invalid("a bracketed IPv6 host is not supported"));
    }

    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (
            host,
            port.parse::<u16>()
                .map_err(|_| invalid("the port is not a number in 1..=65535"))?,
        ),
        None => (
            authority,
            match scheme {
                Scheme::Http => 80,
                _ => 443,
            },
        ),
    };
    // A trailing dot is the absolute-FQDN spelling of a hostname, and oagw
    // normalises it away: its own `Endpoint::normalized_host`
    // (`gears/system/oagw/oagw/src/domain/model.rs:58-64`) and `normalize_alias`
    // (`.../services/management/alias.rs:44-46`) both `trim_end_matches('.')`,
    // and its own tests pin that multiple trailing dots go too
    // (`management/tests.rs:2734`). `derive_alias` below replicated oagw's
    // *validation* rule but not its *normalisation* one — dots are in the legal
    // alias charset, so `jira.example.com.` passed the check unchanged — and the
    // result was an upstream provisioned under a key oagw normalises away, made
    // permanent for the process lifetime by the provisioned cache. Stripped
    // before the `is_empty` check so a host of `"."` alone is still "it names no
    // host" rather than a valid empty one.
    let host = host.trim_end_matches('.');
    if host.is_empty() {
        return Err(invalid("it names no host"));
    }

    let endpoint = Endpoint {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
    };
    let alias = derive_alias(&endpoint)?;

    Ok(JiraEgress {
        endpoint,
        base_path: normalize_base_path(path),
        alias,
    })
}

/// A URL path reduced to the form [`JiraEgress::base_path`] promises: `""`, or a
/// leading slash with no trailing one.
///
/// `trim_end_matches('/')` is legacy's own normalisation (`jira.rs:76`), applied
/// to the path half rather than to the whole URL because the authority has
/// already been split off by the time this runs. A lone `/` and an empty path
/// both reduce to the empty string,
/// so a URL written with or without a trailing slash produces the *same* route
/// and the same cache key — which matters, because the two spellings are the same
/// instance and must not provision two routes.
fn normalize_base_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        trimmed.to_owned()
    }
}

/// The oagw alias for an endpoint: `host`, or `host:port` when the port is not
/// 80/443.
///
/// This is `Endpoint::alias_contribution` (`oagw-sdk/src/models.rs:48-57`), and
/// therefore the same value oagw's own `compute_derived_alias` produces for a
/// single hostname endpoint — see this module's header for why matching it
/// matters. It is recomputed here rather than called because the alias must be
/// **validated** against the charset oagw enforces before it is sent, and
/// `alias_contribution` does not validate.
///
/// **It replicates oagw's `validate_alias` and not its `normalize_alias`, which
/// is why [`egress_for`] normalises the host instead** — Phase C's final review.
/// The one normalisation that mattered is the trailing dot, and doing it on the
/// *host* rather than on the finished alias is deliberate: `trim_end_matches('.')`
/// applied to `jira.example.com.:8443` would strip nothing, because the dot is
/// not at the end, while oagw derives that alias from an already-normalised host
/// and so produces `jira.example.com:8443`. Normalising the host makes the two
/// agree for a non-standard port as well as for a standard one.
///
/// # Errors
///
/// [`DomainError::Validation`] naming `url` when the derived alias is not one
/// oagw would accept — its own rule is alphanumerics plus `.`, `:`, `-` and `_`,
/// with at least one alphanumeric
/// (`gears/system/oagw/oagw/src/domain/services/management/alias.rs:9-39`).
/// Refused here rather than at the gateway so the operator gets a message naming
/// their own field.
fn derive_alias(endpoint: &Endpoint) -> Result<String, DomainError> {
    let alias = endpoint.alias_contribution().to_ascii_lowercase();
    let acceptable = !alias.is_empty()
        && alias.len() <= 253
        && alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_'))
        && alias.chars().any(|c| c.is_ascii_alphanumeric());
    if acceptable {
        Ok(alias)
    } else {
        Err(DomainError::Validation {
            field: "url".to_owned(),
            message: "the JIRA base URL's host cannot be used as an egress alias; it must be a \
                      hostname of alphanumerics, dots, dashes and underscores"
                .to_owned(),
        })
    }
}

#[cfg(test)]
#[path = "oagw_client_tests.rs"]
mod tests;
