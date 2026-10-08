---
status: accepted
date: 2026-09-21
---
# SMTP egress: a direct transport, an in-process credential, and no network rule

**ID**: `cpt-cf-qa-adr-smtp-egress`

## Context and Problem Statement

qa-insights promises email notifications in four places across the PRD and DESIGN, and its only
production `MailClient` was `UnsupportedMailClient`: it opened no connection, never read
`smtp_host`, and never returned an error. An operator filled in the SMTP columns, sent a test, read
a success, and received nothing. The original design had deferred the socket deliberately; the owner
reopened it and settled it as *implement it fully*.

Implementing it raises three questions that the git decision ([ADR-0005](./0005-cpt-cf-qa-adr-git-egress.md))
raised for its own transport and that have to be answered again here, because the answers differ:

* The subsystem's egress contract (`cpt-cf-qa-contract-egress`) routes outbound calls through the
  Outbound API Gateway. Can SMTP go through it?
* If not, where does the relay password live, given that this gear has never held a plaintext
  credential?
* If not, what constrains where the connection goes?

## Decision Drivers

* OAGW speaks HTTP. SMTP is a stateful, multi-round, server-greets-first protocol that may be
  upgraded to TLS mid-stream.
* Relay credentials must come from credstore whatever the transport — ADR-0005's own rule.
* The relay address is per-tenant configuration (`qa_notification_config.email_smtp_host`), typed
  into a settings page at runtime.
* A Kubernetes `NetworkPolicy` matches CIDRs, pod selectors and namespace selectors. It has no
  DNS-name matcher, and a policy that selects a pod nothing else selects converts that pod from
  unrestricted egress to deny-all-but-this-list.
* All four qa-platform gears run in one container, in one pod, behind one Service.

## Considered Options

* A `lettre` transport directly in the qa-insights infra adapter
* Route SMTP through OAGW
* Ship a side-car SMTP relay inside the cluster and point every tenant at it

## Decision Outcome

Chosen option: **a direct `lettre` transport in `qa-insights/src/infra/notify/mail_smtp.rs`**,
confined to the infra layer exactly as `gix` is in qa-catalog. This is the subsystem's second
direct egress and the first that is not HTTP at all.

Three consequences follow, and each is part of the decision rather than an implementation detail.

### 1. TLS is mandatory and is selected by the port

Port 465 is implicit TLS (RFC 8314 §3.3): the socket is wrapped before the first byte. Every other
port is dialled in the clear and **required** to upgrade via `STARTTLS` — `Tls::Required`, not
`Tls::Opportunistic`, so a relay that does not offer it fails before any credential or message is
written.

There is deliberately no plaintext mode and no third settings column to select one. A `tls: none`
switch is set once to make a stubborn relay work and never revisited, on a path that carries a
password. The cost is stated rather than hidden: a deployment whose relay speaks neither gets a
failed send with the relay's own refusal in the audit log.

### 2. The gear resolves a credential itself, for the first time

The JIRA token rides an HTTP header, so OAGW fetches and injects it and no plaintext of it ever
enters this process. (This paragraph said the same of the Slack webhook when it was accepted; that
turned out to be false — see the Amendment of 2026-09-30.) SMTP has no such path, so `qa_notification_config` gains
`email_smtp_credstore_ref` (a reference, never a password, beside a plaintext `email_smtp_username`
— the split `qa_jira_config` already makes) and the adapter resolves it through
`credstore_sdk::CredStoreClientV1` under the **sending tenant's** own `SecurityContext`, holding
the value for the length of one `send`.

[ADR-0008](./0008-cpt-cf-qa-adr-credential-containment.md) still binds: nothing derived from that
value is formatted, anywhere. The rule is load-bearing in a new place — `FromUtf8Error` owns the
bytes it failed on, so rendering it would print the secret into a validation message an operator
sees.

### 3. There is no `NetworkPolicy`, and that is the hard half of this decision

The brief for this work asked for a Kubernetes egress rule "to the configured host and port, as
narrow as the configuration allows". **No such rule exists**, for three independent reasons, each
sufficient on its own:

* **There is no qa-insights workload to select.** All four gears run in the single `gears`
  container behind the single `qa-platform-gears` Service — `gears-deployment.yaml` says so, and
  `runner-networkpolicy.yaml`'s rule 2 already relies on it ("There is no second Service to write a
  second rule for").
* **A policy would subtract, not add.** The gears pod is selected by no policy today, so its egress
  is unrestricted; the first policy with `policyTypes: [Egress]` to select it denies everything it
  does not list. The list would have to carry Postgres, Keycloak, the Argo and Kubernetes APIs,
  arbitrary git remotes (qa-catalog) and arbitrary tenant-supplied management nodes
  (qa-environments/qa-connector-ssh). The runner policy's own rule 3 concedes that the last shape is
  "NOT STATICALLY EXPRESSIBLE" and falls back to `0.0.0.0/0` minus the cluster's own CIDRs. Any
  gears policy has to do the same — at which point an SMTP rule inside it constrains nothing.
* **The destination is not knowable at render time and not expressible if it were.** The relay host
  is a per-tenant database column written through the settings API; `NetworkPolicy` has no DNS-name
  matcher, so even a chart that somehow knew the name could only encode an address.

So the egress control is in the application, where the host is actually known:
`QaInsightsConfig::smtp_allowed_hosts`, a deployment-level list of relay hostnames, rendered by the
chart from `qaInsights.smtpAllowedHosts`. It does two jobs. Empty — the default — binds
`UnsupportedMailClient`; non-empty binds the real transport, which refuses per send any host not on
the list. There is no wildcard.

### 4. `UnsupportedMailClient` stays, and now fails

It is the explicitly-bound answer for a deployment with no SMTP egress, and `send` returns
`DomainError::UnsupportedEgress` rather than an `Ok` outcome. `SendOutcome::UnsupportedEgress` —
a value the caller logged rather than an error it handled — is gone; the audit string
`unsupported_egress` survives, chosen from the error, so "this deployment cannot send email" stays
distinguishable from "the relay refused this message".

### Consequences

* Good, because email notifications are delivered for the runs that notify at all, which four
  requirements already claimed. **The qualifier is load-bearing and was missing here until the
  second review (finding #71/#112).** It is also no longer the same qualifier. Until 2026-09-29
  `domain::notify::routing::route` gated both channels on one question — does the schedule that
  launched this run have notifications on — so an ad-hoc run notified on no channel and a schedule
  with notifications off silenced email as well as Slack, while `notify_on_failure` and
  `notify_on_success` were read by nothing and a passing run mailed exactly as a failing one did.
  Each was a deliberate port of the source system, recorded in that module's header; none was what
  this ADR decided, and none was stated here.

  **The owner reversed all three on 2026-09-29.** Email is now gated by the tenant's
  `email_enabled` and the outcome policy, and by no schedule-level flag at all — which is this
  ADR's own subject, so it is stated here rather than only in DESIGN: the answer to "should email
  get a schedule-level gate of its own" was no, because that is a qa-runs schema and SDK change
  and a tenant-level gate already exists to read. An ad-hoc run now notifies on the tenant's
  settings, and `notify_on_failure`/`notify_on_success` decide by outcome — which cuts mail as
  well as adding it, since the default is failures only. DESIGN §3.5, "What actually notifies", is
  the full list.
* Good, because the failure modes are honest: a deployment with no relay fails and is audited, and
  `POST /qa/v1/settings/notifications/test` answers `501` naming the channel instead of a success.

  **This bullet said "the settings test button", and there is no such button for email.** Measured
  2026-09-29: `qa-platform-ui`'s `NotificationsEmailPage` has the SMTP fields, the enable switch and
  a Save button, and nothing that issues a test send. The one test control in the UI is on
  `NotificationsSlackPage` (`useTestScheduledRunNotification`), and it posts a
  `ScheduledRun` payload, which `NotifyService::send_test` routes over Slack only — the email arm
  is reachable exclusively through a `TestSend::Generic` request, which no UI code sends. So the
  endpoint behaves as this ADR describes and no operator reaches it from the settings page.

  The claim is corrected rather than discharged by adding the button, deliberately: this ADR is a
  record of an egress decision, and it should describe the surface that exists. Adding an email
  test control to `NotificationsEmailPage` is a UI change with its own review, and it is the
  obvious follow-up — the endpoint, its authorization (`qa.notification_config/test`), its refusals
  and its audit rows are all already there: the settings-page test send writes one row on both
  outcomes, which `fix(qa-platform): a settings-page test send is audited on both outcomes`
  (`c2b71739d`, 2026-09-29) added — the run-completed producer audits its own sends, not this
  endpoint's (DESIGN §3.5, "Egress").

  **The status an operator sees has also changed for a deployment that *does* have SMTP.** `501`
  is the answer when `qaInsights.smtpAllowedHosts` is empty and `UnsupportedMailClient` is bound.
  When the real transport is bound and the relay refuses, times out or cannot be reached, the
  answer is `503` carrying the relay's own response and an `EgressFailure` naming which of the
  four things went wrong (`DomainError::UpstreamEgress`) — it used to be an opaque `500`, which
  told an operator this gear had broken when their own relay had refused their own password.
  `503` is the same answer qa-catalog gives for its own upstream, the git remote.
* Good, because the allow-list is enforced against the value an operator actually typed, which a
  CIDR rule could not have been.
* Bad, because this gear now holds plaintext credential material in process, which it never did —
  and, since the Amendment of 2026-09-30, a second kind of it, the Slack webhook URL. ADR-0008
  bounds both; ADR-0008 is a rule with one mechanical enforcement point (`assert_no_leak`)
  that does not cover this path.
* Bad, because the allow-list is an application control, so it is bypassed by anything that reaches
  the pod's network namespace by another route. A `NetworkPolicy` would not have been bypassable
  that way — it would simply not have been expressible.
* Bad, because a relay that speaks no TLS cannot be used at all.
* Bad, because the egress contract now has two exceptions rather than one, and the second is not
  even HTTP. `cpt-cf-qa-contract-egress` is amended in both PRD §7.4 and DESIGN's qa-insights
  section rather than quietly stretched.

### Confirmation

* `infra::notify::mail_smtp`'s tests drive a real `TcpListener` speaking real SMTP: success,
  authentication failure (`535`), a relay that refuses the recipient (`550`), a silent relay
  bounded by the ten-second timeout, an unreachable host, and the allow-list refusing before
  anything is dialled. The authenticated test decodes the `AUTH PLAIN` line off the wire and
  asserts the password in it is the credential store's. Each failure test asserts the
  `EgressFailure` discriminator rather than a substring of the message, so `535` and `550` — both
  permanent 5xx replies — cannot be told apart by accident.
* `tls_mode_for` is asserted directly, including that **no** port maps to plaintext.
* `deploy/helm/tests/check_smtp_egress.py` holds the chart's half: the default install allow-lists
  nothing, an operator's hosts arrive intact, and renaming the key the transform rewrites fails the
  render rather than discarding the setting.
* `make fips-policy` passes: `lettre` enters the graph on `tokio1-rustls` + `aws-lc-rs` +
  `rustls-native-certs`, with no `ring`, no `webpki-roots`, no `native-tls` and no OpenSSL of its
  own. It takes the process-default `rustls` provider that `libs/toolkit`'s bootstrap installs, so
  a FIPS build's provider applies without `lettre`'s own `fips` feature and without an
  `aws-lc-fips-sys` build.

## Amendments

**2026-09-30 — the Slack webhook is the second credential this process resolves in plaintext.**
§2 above said the Slack webhook rides HTTP and is therefore fetched and injected by OAGW. It is not:
a Slack incoming webhook's credential is its URL **path**
(`https://hooks.slack.com/services/T…/B…/<token>`), and every OAGW auth plugin injects into a
request *header*; none can set the path. Until this amendment the Slack adapter sent the credstore
reference itself as a placeholder proxy path, and no Slack message could be delivered (an open item
at the time). The owner ruled it implemented, by the same means this ADR chose for the SMTP
password: `infra::notify::slack_oagw` resolves the secret named by `slack_webhook_credstore_ref`
through `credstore_sdk::CredStoreClientV1` under the sending tenant's own `SecurityContext`, and the
secret holds the full webhook URL.

Unlike SMTP, the send itself still goes through OAGW, so `cpt-cf-qa-contract-egress` gains no new
exception: the adapter provisions a per-tenant, no-auth upstream `hooks.slack.com` (HTTPS, 443) with
one route `POST /services`, and proxies the resolved path through it. The containment:

* **The host is fixed** to `hooks.slack.com` by the code, not taken from the secret, so a stored
  value cannot aim this gear's egress anywhere else.
* **The value is validated before anything is dialled**: HTTPS, that exact host, the default port,
  no userinfo, no query or fragment, a `/services/` path whose segments are plain tokens.
* **It is never logged, rendered or echoed.** Every failure of the send itself (the gateway leg:
  provisioning, the target check, the proxy call, a non-2xx answer) is
  `DomainError::UpstreamEgress { channel: "slack", endpoint: "hooks.slack.com", … }` whose detail
  is fixed text or an HTTP status — never the gateway's own error text, which carries the full
  upstream URL — because that error's text is what the audit log stores and the settings `/test`
  route answers with (`503`, as for SMTP). A refusal of the stored value never quotes it. The two
  failures that happen before the gateway is reached are not `UpstreamEgress`: a reference that is
  not valid syntax, absent, unreadable by the tenant or not a Slack webhook is `Validation` (`400`,
  one fixed message for the last three), and a credential-store outage is `Internal` (`500`).
  **Neither qa-insights nor OAGW logs the webhook path.** qa-insights never formats the resolved
  value. OAGW, which must carry the path as the proxied request's suffix, was changed in the same
  closure so that its proxy request logs ("Connected to upstream", "Proxy request completed" /
  "failed", `infra::proxy::pingora_proxy`) name the upstream alias and the matched route's path
  *pattern* (`/services`) instead of the request path or proxy URI; pingora's own "Fail to proxy"
  error and retry lines carry the same alias-and-pattern summary (OAGW overrides
  `ProxyHttp::request_summary`, whose default is the request line); and its timeout error details
  name only the upstream's origin (`url_origin` in `infra::proxy::service`). That holds at `info`
  and above. **Residual:** pingora dumps the whole request header at DEBUG/TRACE, which no hook can
  override, so the `pingora_*` log targets must stay at `info` or below — this chart's default
  (`logging.default` is `info` and no `pingora_*` section raises it). OAGW still returns the
  proxy URI as a problem detail's `instance` to its caller, which qa-insights drops. A second named
  residual: pingora-core 0.8.0 logs its `InvalidHTTPHeader` parse error at ERROR with the raw request
  buffer, path included (`protocols/http/v1/server.rs`, the header-parse failure branch); it is
  unreachable from qa-insights' fixed header set and the plugin-free Slack route. Out of scope,
  and not on this path: `libs/toolkit`'s REST canonical-error layer logs `instance` for REST
  requests into OAGW; qa-insights calls OAGW in-process, not over REST.

**2026-10-07 — both secrets are read as the system actor, on the test send too.** The SMTP
password (§2) and the Slack webhook (the amendment above) are resolved under the qa-insights system
actor bound to the sending tenant — the identity every automatic send already had, and now the
identity of the settings `/test` send as well — so a secret stored with `private` sharing,
readable only by the user who created it, fails the test with a reason that says to share it at
tenant level instead of passing the test and failing every real send. A send that times out may still have been delivered, and its claim is released like any failure's, so a run-completed notification is delivered at least once — possibly twice — when the outcome is ambiguous (DESIGN §3.5, "Egress").
