---
status: accepted
date: 2026-08-14
---
# Git and SSH egress: direct transports, with host-key verification disabled

**ID**: `cpt-cf-qa-adr-git-egress`

## Context and Problem Statement

Two parts of the subsystem reach out over SSH-based transports:

* `qa-catalog` clones and fetches test repositories from git remotes.
* `qa-connector-ssh` opens sessions to a management node and drives a CLI there.

The platform's Outbound API Gateway (OAGW) is the standard egress path: it injects credentials and
controls what leaves the deployment. Should these two go through it, and how should the remote's
host key be verified?

## Decision Drivers

* OAGW speaks HTTP. The git wire protocol and an interactive SSH session are neither.
* A clone is not a request/response exchange; it is a negotiated, multi-round protocol producing a
  pack file.
* Repository credentials and node credentials must still come from credstore, whatever the
  transport.
* Operators register remotes and nodes ad hoc, and the deployment has no key-distribution
  mechanism for them.

## Considered Options

* `gix` directly in the `qa-catalog` infra adapter, and `ssh` directly in `qa-connector-ssh`
* Route both through OAGW
* Proxy git over an HTTP-only mirror that OAGW can front

## Decision Outcome

Chosen option: **direct transports in the respective adapters**. `gix` performs clone and fetch
inside `qa-catalog/src/infra/git`; `qa-connector-ssh` shells out to `ssh`. Both are confined to an
infra layer, and both take credentials only as credstore references.

**Host-key verification is disabled on both.** Git remotes and SSH sessions run with
`StrictHostKeyChecking=no`, and the SSH connector additionally sets
`UserKnownHostsFile=/dev/null`, so a key is accepted and then immediately forgotten.

This is the part of the decision that costs something, and the exposure differs between the two
cases:

* **Git remotes.** An attacker who can intercept the connection chooses what the platform reads —
  they serve a repository. The consequence is that tests the platform runs are attacker-chosen
  code.
* **SSH sessions.** An attacker who completes the handshake is handed the credential the session
  carries — a `vinfra` administrator password valid against the real cluster. The private key
  itself is not disclosed, because it never leaves the agent.

The second case is strictly worse than the first, and both are accepted for now on the same
grounds: there is no key-distribution mechanism for operator-registered remotes and nodes, and
failing closed would make the feature unusable on day one.

### Consequences

* Good, because clone and fetch use a real git implementation with no protocol translation.
* Good, because credentials still come from credstore; disabling host-key checking changes what we
  trust the peer to be, not how we store secrets.
* Good, because the exposure is written down in one place and the constant that sets it carries the
  same text, so nobody re-derives it from the flag.
* Bad, because an on-path attacker can substitute a git remote and have the platform execute code
  of their choosing.
* Bad, because an on-path attacker can capture the administrator credential the SSH session
  carries.
* Bad, because egress for these two paths is not visible to OAGW, so a deployment cannot centrally
  see or restrict it.

### What would have to change to tighten it

Pin the expected keys: drop the two options, provision a known-hosts file (from gear configuration
or a credstore-held blob), and pass `-o UserKnownHostsFile=<that file> -o
StrictHostKeyChecking=yes`. The per-use temporary directory the SSH connector already creates is
the natural home for that file. The corresponding git-side change is the same file passed through
`gix`'s SSH command. What is missing is not the code but the provisioning story for the keys.

### Confirmation

* `qa-catalog/src/infra/git/gix_sync.rs` and `connectors/qa-connector-ssh/src/agent.rs` are the
  only places these options are set, and each carries this ADR's reasoning in full.
* No credential reaches either transport as anything but a credstore reference resolved at the
  point of use.
