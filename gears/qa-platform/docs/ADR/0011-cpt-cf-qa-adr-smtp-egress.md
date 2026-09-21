---
status: accepted
date: 2026-09-21
---
# SMTP egress: a direct transport, an in-process credential, and no network rule

**ID**: `cpt-cf-qa-adr-smtp-egress`

## Context and Problem Statement

qa-insights promises email notifications in four places across the PRD and DESIGN, and its only
production `MailClient` was `UnsupportedMailClient`: it opened no connection, never read
`smtp_host`, and never returned an error. An operator filled in the SMTP columns, sent a test,
read a success, and received nothing. Decision D10 had deferred the socket deliberately; the owner
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

The Slack webhook and the JIRA token ride HTTP, so OAGW fetches and injects them and no plaintext
ever enters this process. SMTP has no such path, so `qa_notification_config` gains
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

* Good, because email notifications are delivered, which four requirements already claimed.
* Good, because the failure modes are honest: a deployment with no relay fails and is audited, and
  the settings test button answers `501` naming the channel instead of a success.
* Good, because the allow-list is enforced against the value an operator actually typed, which a
  CIDR rule could not have been.
* Bad, because this gear now holds plaintext credential material in process, which it never did.
  ADR-0008 bounds it; ADR-0008 is a rule with one mechanical enforcement point (`assert_no_leak`)
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
  authentication failure (`535`), a silent relay bounded by the ten-second timeout, an unreachable
  host, and the allow-list refusing before anything is dialled. The authenticated test decodes the
  `AUTH PLAIN` line off the wire and asserts the password in it is the credential store's.
* `tls_mode_for` is asserted directly, including that **no** port maps to plaintext.
* `deploy/helm/tests/check_smtp_egress.py` holds the chart's half: the default install allow-lists
  nothing, an operator's hosts arrive intact, and renaming the key the transform rewrites fails the
  render rather than discarding the setting.
* `make fips-policy` passes: `lettre` enters the graph on `tokio1-rustls` + `aws-lc-rs` +
  `rustls-native-certs`, with no `ring`, no `webpki-roots`, no `native-tls` and no OpenSSL of its
  own. It takes the process-default `rustls` provider that `libs/toolkit`'s bootstrap installs, so
  a FIPS build's provider applies without `lettre`'s own `fips` feature and without an
  `aws-lc-fips-sys` build.
