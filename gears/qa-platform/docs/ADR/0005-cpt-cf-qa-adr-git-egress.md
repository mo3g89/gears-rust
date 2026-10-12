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
* Good, because every egress operation is bounded in time and size (DESIGN §3.3, "Limits on
  talking to a remote"), so a tenant-supplied url cannot hold a request or the volume
  indefinitely. A peer that trickles its handshake can still hold, for as long as it keeps
  trickling, one listing thread per url, one sync thread per repository (holding that
  repository's clone directory) and one waiting sync for up to `sync_timeout_seconds`. Further
  listings of that url answer at once without a thread; that bound is keyed by the url, so it is
  shared by every repository and tenant that names it.
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

## Amendments

### 2026-08-27: SSH remotes and the scheme allow-list

Recorded retroactively on 2026-10-12 from the code: the decision was taken on 2026-08-27, but the
history of this branch was squashed and no commit in that window carries it, so no hash can be
cited. What follows is what `qa-catalog/qa-catalog/src/domain/git_url.rs` (lines 21-35, the
"Accepted forms" and "Userinfo is scheme-dependent" sections) implements today; code comments that
cite "ADR-0005, amended 2026-08-27" mean this section.

* The accepted forms of a registered remote are `https://host/path` and `http://host/path` with no
  userinfo, `ssh://[user@]host[:port]/path`, and scp-like `[user@]host:path`.
* Everything else is refused: empty input, bare local paths, `file://`, `git://`, Windows drive
  paths and a bare hostname with no path.
* Userinfo is scheme-dependent. For http(s) any userinfo is credential material and is refused.
  For ssh a bare `user@` is the username and is permitted; `user:password@` is refused for both
  schemes.
* SSH material is a private key held in credstore and reached through `qa-connector-ssh`'s
  short-lived `ssh-agent` (`connectors/qa-connector-ssh/src/agent.rs`), so the key never becomes a
  file. The URL scheme alone decides whether `credential_ref` names an SSH key or HTTP basic-auth
  material.
* The runtime image installs `openssh-client`, which the SSH path needs.
