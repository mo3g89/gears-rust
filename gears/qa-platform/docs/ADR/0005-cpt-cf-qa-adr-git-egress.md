---
status: accepted
date: 2026-08-12
amended: 2026-08-27
---
# Git egress: gix directly in the qa-catalog infra adapter, not through oagw

**ID**: `cpt-cf-qa-adr-git-egress`

## Context and Problem Statement

qa-catalog must clone/fetch test repositories and list their branches (`cpt-cf-qa-fr-catalog-repos`, `cpt-cf-qa-fr-catalog-branch-cache`). The platform rule routes outbound traffic through oagw (`cpt-cf-qa-constraint-platform-delegation`), and DESIGN §3.4 initially penciled git sync in on the oagw row. But git is not plain HTTP: the smart protocol is a stateful pkt-line exchange (`info/refs` capability negotiation, then a streamed `upload-pack` request/response pair, or an SSH channel), and oagw's egress contract is HTTP-request-centric. How should qa-catalog reach git remotes?

## Decision Drivers

* oagw has no git smart-protocol (or generic tunnel) capability today, and none is on its near-term roadmap.
* In-repo precedent already exists for protocol-specific egress living in a gear's infra adapter behind a domain port: file-storage's S3 backend (`gears/file-storage/.../infra/backend/s3.rs`, its ADR-0005) and oidc-authn-plugin's direct IdP calls.
* The engine must run in-process across all deployment shapes: no shelling out to a `git` binary that may not exist in the gear's container image.
* Credentials come from credstore and must be injectable programmatically, never written to disk or embedded in URLs.
* The domain layer is already insulated: services depend on `RepoSyncPort` only, so the engine choice is replaceable without touching domain code.

## Considered Options

* gix (pure-Rust gitoxide) called directly in the infra adapter
* gix over oagw via a custom gix transport that wraps oagw's HTTP egress
* Shell out to a `git` binary

## Decision Outcome

Chosen option: "gix directly in the infra adapter", because it follows the established file-storage/oidc precedent for protocol-specific egress (an infra adapter behind a domain port), works identically in every deployment shape, and injects credstore material through gix's in-process credential callback without ever materializing it on disk. **JIRA and SMTP egress stay on oagw**; git egress is the recorded exception.

This ADR amends the following spec statements, each of which previously routed git through oagw:

| Document | Location | Amended to |
|----------|----------|------------|
| DESIGN | §1.3 layer diagram (`CAT --> ... OAGW`, `OAGW --> ... GIT` edges) | qa-catalog reaches test repos directly; oagw carries JIRA/SMTP only |
| DESIGN | §3.2 qa-catalog — responsibility scope and related components | in-gear `gix` infra adapter behind `RepoSyncPort`; oagw not a dependency of this gear |
| DESIGN | §3.4 internal dependency table (oagw row) and the "only oagw talks to external HTTP systems" rule | oagw = JIRA polling + SMTP; git egress carved out as the recorded exception |
| DESIGN | §3.5 external dependencies — the former combined "JIRA / SMTP / git remotes" subsection | split; git remotes reached by qa-catalog directly (HTTPS token **and** SSH key — see the 2026-08-27 amendment below) |
| PRD | §7.2 `cpt-cf-qa-contract-egress` | scoped git-egress exception to "no direct socket egress from gears" |
| PRD | §10 dependency table (oagw row) | oagw = JIRA polling + SMTP egress |

### Consequences

* **This decision requires a PRD-level exception, which an ADR cannot grant by itself.** `cpt-cf-qa-contract-egress` (PRD §7.2) is a p1 contract whose compatibility clause reads "no direct socket egress from gears", and gix opens sockets from inside qa-catalog. An ADR records a design decision; it does not have the authority to narrow a requirement. The contract is therefore amended in the PRD itself with an explicit, scoped carve-out — git clone/fetch/ls-remote from the qa-catalog infra adapter, credentials from credstore (HTTPS only as first written; widened to HTTPS+SSH on 2026-08-27, see the Amendment below) — and the contract's general prohibition stands unchanged for every other protocol and every other gear. See PRD §7.2 `cpt-cf-qa-contract-egress`, which cross-references this ADR. If that exception is ever refused, this ADR must be reopened, not reinterpreted.
* New workspace dependency `gix 0.86` (many small `gix-*` transitive crates). Feature-trimmed to `sha1`, `blocking-network-client`, `blocking-http-transport-reqwest-rust-tls`, `worktree-mutation` — the https transport rides reqwest 0.13 whose `rustls` feature selects the aws-lc-rs provider, i.e. the same reqwest version and rustls provider the workspace already uses (no `ring` enters the graph).
* License review against `deny.toml` (per `guidelines/DEPENDENCIES.md`): all `gix-*` crates are `MIT OR Apache-2.0`; new transitive crates are MIT/Apache-2.0/BSD-3-Clause/`Unlicense OR MIT` — every expression satisfiable from the existing allow-list, no `deny.toml` change needed. (`cargo-deny` was not installed on the implementing machine; the check was a manual sweep of `cargo metadata` license fields for every crate added to `Cargo.lock`.)
* p1 authentication was **HTTPS token only**: the credstore material is passed to gix's credential callback as basic-auth identity (`user:secret` pairs used as-is; a bare token is sent as user `oauth2` with the token as password). SSH-key auth was deferred, with this revisit trigger: *"revisit when gix grows native ssh **or the key can be scoped to a per-process agent**"*. **The second disjunct fired on 2026-08-27 — see "Amendment: SSH remotes are supported" below.**
* The gix API is blocking; the adapter runs it under `tokio::task::spawn_blocking` behind the async `RepoSyncPort`.
* Revisit this ADR if oagw grows git smart-HTTP tunneling (or a generic stream-egress capability); the port boundary makes the swap a one-adapter change.

### Confirmation

`RepoSyncPort` has exactly one implementation, in `qa-catalog/src/infra/git/` — no gix import appears in `domain/` or `api/` (architecture lints / dependency review). The feature-gated integration test (`tests/gix_sync_integration.rs`, `--features integration`) exercises clone, re-fetch, branch listing, and plan discovery against a gix-built fixture repository.

For the 2026-08-27 SSH amendment, the checked properties are: `domain::git_url` (table-driven accept/reject policy for both ssh syntaxes, scheme-aware userinfo); `infra::git::ssh_agent::ssh_agent_tests` (the command carries the agent socket, the `IdentitiesOnly`/`IdentityFile` pair, `BatchMode=yes` and the host-key options; the options come from the single documented constant; the agent's socket, directory and process are gone after drop on both the success and failure paths; a passphrase-protected key is named as such; and **the private key reaches no file, no `argv` and no environment variable**); `infra::git::gix_sync::ssh_config_tests` (the ssh command is resolved by gix from the **repository config** rather than the process environment, independently per repository); `infra::git::gix_sync::http_credential_tests` (an ssh remote's PEM is never handed to the HTTP basic-auth callback); and `domain::service::tests_tenant_scoping` (an ssh `credential_ref` does not resolve across tenants, against the real DB and ORM repository).

**`infra::git::ssh_agent::ssh_auth_tests` authenticates end-to-end against a throwaway `sshd`** on an ephemeral port with a temporary host key and `authorized_keys`. This suite exists because the option-pair defect above passed every other test in the module: argv capture and in-image agent exercises never involve a server *deciding whether to accept a signature*. It asserts the production `core.sshCommand` authenticates using the agent identity (`explicit agent`), and carries a negative control asserting that removing `IdentityFile` breaks authentication — so the pair cannot be silently unpaired again.

## Amendment: SSH remotes are supported (2026-08-27)

**Decided by the human partner**, who directed that SSH git remotes must work: their test
repositories are private Bitbucket repositories reachable only over SSH (unauthenticated HTTPS to
both returns 401), the UI's repository form already offers an SSH-key picker, and the SSH-key data
model (`qa_ssh_keys` + credstore) already existed. This ADR is **reopened, not reinterpreted**, as
its Consequences require.

This is not an override against the ADR's terms. The original deferral carried a two-part revisit
trigger — "gix grows native ssh **or the key can be scoped to a per-process agent**" — and the
second disjunct is exactly what is implemented here.

### The "key material on disk" objection is resolved, not accepted

The original reasoning was that gix's ssh transport shells out to a system `ssh`, which offers no
in-process key injection, "so supporting it would mean writing key material to disk". That
inference does not hold, and the implementation demonstrates why
(`qa-catalog/src/infra/git/ssh_agent.rs`):

* a **short-lived `ssh-agent` per sync operation** is spawned in foreground mode (`-D`) on a unique
  socket inside a private `0700` temp directory;
* the credstore-held PEM is handed to **`ssh-add -` on stdin** — so the *private key* never becomes a file, never
  appears in `argv` (`/proc/<pid>/cmdline`), never appears in an environment variable
  (`/proc/<pid>/environ`), and is never logged or `Debug`-formatted;
* `ssh` is pointed at that agent with
  `-o IdentityAgent=<socket> -o IdentitiesOnly=yes -o IdentityFile=<agent public key>`.

**`IdentitiesOnly=yes` and `IdentityFile` are a pair, and the pair is load-bearing.**
`IdentitiesOnly=yes` does not mean "use the agent" — per `ssh_config(5)`, *"IdentityFile may be used
in conjunction with IdentitiesOnly to select which identities in an agent are offered during
authentication."* With `IdentitiesOnly=yes` and no `IdentityFile`, the candidate set is the default
`~/.ssh/id_*` files only, so an agent key matching none of them is **never offered**. Measured
against a real `sshd`: the agent's key appeared zero times in `ssh -v`, and the connection ended
`Permission denied (publickey)`, rc=255. Worse, on an image that *does* carry a default identity
file, that configuration silently authenticates as **the wrong identity**.

The fix keeps `IdentitiesOnly=yes` (dropping it would authenticate, but would leave `ssh` free to
offer any other identity it finds) and names the agent's own key: after `ssh-add -`, the identity's
**public** key is read back out of the agent with `ssh-add -L` and written into the per-sync
directory. Only public material is written — it is what the client sends the server in the clear
during authentication anyway — and `ssh -v` confirms the signature still comes from the agent by
labelling the identity `explicit agent`. The private key remains agent-only.

The private key's path is therefore credstore → memory → pipe → agent memory. The property is
pinned by tests rather than asserted in prose: `private_key_never_touches_the_filesystem` asserts
that **no file** in the agent directory contains the private material (and that the only files
present are the socket and the `.pub`), and fails if anyone later "simplifies" this to
`ssh -i /tmp/key`.

The agent is reaped by a `Drop` guard, so every exit path — success, error, and unwind — kills the
process and removes its directory. A happy-path kill would leak one agent per failed sync.

### The ssh command is repository config, not process environment

`core.sshCommand` is set on the **gix repository object**, never as a process-global
`SSH_AUTH_SOCK`. gix resolves the ssh program per repository
(`gix-0.86.0/src/repository/config/mod.rs:91-97`), which is what makes concurrent syncs holding
different keys safe; an environment variable would be process-global and would race, letting
whichever sync wrote last decide which key *both* authenticated with. The override is applied with
`gix_config::Source::Api`, whose kind is `Override` rather than `Repository`, so it survives gix's
trust filter regardless of who owns the repository directory.

### The image now depends on `openssh-client` — a cost knowingly paid

`deploy/docker/qa-platform.Dockerfile` installs `openssh-client` in the runtime stage. This is a
**real cost against this ADR's driver** "the engine must run in-process across all deployment
shapes: no shelling out to a `git` binary that may not exist in the gear's container image", and it
is now knowingly paid rather than quietly ignored.

Scope of the cost, stated plainly: git itself is still never shelled out to — gix remains the
engine, and the object/ref/checkout machinery is unchanged pure Rust. What is shelled out to is
`ssh`/`ssh-agent`/`ssh-add`, and only on the SSH path; HTTP(S) syncs invoke no external binary at
all. The deployment-shape neutrality the driver protects is therefore narrowed, not abandoned: any
image running qa-catalog with SSH remotes registered must ship `openssh-client`, and an image
without it fails at `SshAgent::start_with_key` with an error that says so. Verified against
`debian:13.3-slim` (the runtime base): `openssh-client` supplies `ssh`, `ssh-agent` and `ssh-add`
(OpenSSH 10.0p2).

The alternative — the option this cost buys out of — was writing the key to disk, which is the one
thing the design exists to avoid.

### Host key verification is DISABLED by explicit human decision

`StrictHostKeyChecking=no` together with `UserKnownHostsFile=/dev/null`, at the human partner's
explicit request: *"i want host key do not checked and all repos can be cloned without errors."*

**The risk, stated plainly.** Nothing pins the remote server's identity. On a hostile or
compromised network path (DNS hijack, BGP hijack, on-path attacker) an impostor host can complete
the handshake, and qa-catalog will authenticate to it and fetch from it. The private key is not
disclosed by this — the agent only ever produces a signature, and the
`IdentitiesOnly=yes`/`IdentityFile` pair bounds which identity is offered — but **the repository content the gear ingests can be attacker-chosen**, and
that content drives plan discovery and test execution. `UserKnownHostsFile=/dev/null` additionally
means no trust-on-first-use memory accumulates, so a key that silently changes between syncs is
never noticed.

**Containment.** The weakening is expressed in exactly one place —
`HOST_KEY_VERIFICATION_OPTIONS` in `qa-catalog/src/infra/git/ssh_agent.rs` — which is the only
site in the codebase that passes host-key options to `ssh`, and a test asserts the command string
is built from that constant. Tightening it later is a change to that constant.

**What tightening would require:** provisioning a known-hosts file (from gear config or a
credstore-held blob) into the per-sync temp directory this module already creates, then passing
`-o UserKnownHostsFile=<that file> -o StrictHostKeyChecking=yes`. The blocker is not the code but
the absence of an operator-facing way to supply host keys.

### Other consequences of the amendment

* **URL policy widens.** `ssh://[user@]host[:port]/path` and scp-like `[user@]host:path` are
  accepted; `file://`, `git://`, bare local paths and Windows drive paths remain refused, for the
  unchanged original reason (gix's local transport would otherwise make repo registration an
  arbitrary host-file read via plan discovery). The embedded-credential check became
  **scheme-aware**: `git@host` is a username over ssh and is permitted, while any userinfo over
  http(s), and `user:password@` under every scheme, stay refused. The classifier is
  `qa-catalog/src/domain/git_url.rs`, shared by validation, credential resolution and the sync
  engine so the three cannot drift.
* **`credential_ref` means different things per scheme**, because `qa_test_repositories` has one
  credential column and no auth-mode column. Over http(s) it is a credstore reference, as before.
  Over ssh it is a **`qa_ssh_keys` row id**, and the material lives at that row's `credstore_ref` —
  it has to be this way round because `SshKeyDto` deliberately withholds `credstore_ref` (publishing
  it would let any tenant member read another member's key straight out of credstore), so the row id
  is the only handle a client can send, and the UI sends exactly that.
* **An SSH remote with no credential is attempted unauthenticated** rather than failing early;
  public repositories over SSH exist.
* **A passphrase-protected key is refused with a clear error** before `ssh-agent` is spawned.
  `ssh-add` cannot unlock one non-interactively and fails silently (exit 1, empty stderr), which
  would otherwise surface as an unattributable failure.
* **`BatchMode=yes` is mandatory** on the ssh command: without it `ssh` can block forever on an
  interactive prompt in a tty-less container, turning an auth failure into a hung sync.
* New dependency `base64` (already a workspace dependency) — used only to read the cipher name out
  of an OpenSSH private-key header for the passphrase check.

## Pros and Cons of the Options

### gix directly in the infra adapter

* Good, because it matches the file-storage S3 / oidc-authn-plugin precedent: protocol-specific egress in one infra adapter behind a domain port.
* Good, because pure Rust in-process: same behavior in single-node, multi-node, and Kubernetes shapes; no container-image prerequisites.
* Good, because credentials flow credstore → memory → gix credential callback, never to disk, environment, or URL.
* Bad, because it is an exception to "all egress via oagw" that must be recorded (this ADR) and re-justified if oagw's capabilities grow.
* Bad, because gix pulls ~60 transitive crates and its higher-level APIs (worktree checkout on re-fetch) are still maturing, requiring some hand-assembly in the adapter.

### gix over oagw via a custom transport

* Good, because egress policy (allow-lists, observability) would apply to git traffic uniformly.
* Bad, because oagw exposes no streaming/tunnel contract today — this option requires designing and shipping a new oagw capability plus a bespoke gix `Transport` implementation before any catalog feature works, an unacceptable p1 dependency chain.
* Bad, because half of git egress (SSH remotes) cannot be expressed as HTTP forwarding at all.

### Shell out to a `git` binary

* Good, because maximal protocol fidelity and battle-tested behavior for free.
* Bad, because it adds a runtime binary dependency the platform images do not guarantee, violating deployment-shape neutrality.
* Bad, because credential injection degrades to askpass helpers/env vars or on-disk credential files — strictly worse secret hygiene than an in-process callback.

## More Information

Engine choice and adapter placement decided during Task 9 planning of the qa-catalog implementation plan (2026-08-12), following the plan's recorded decision to mirror the file-storage S3 precedent. Bundle blobs similarly stay gear-local (filesystem behind `BundleStore`) until the file-storage SDK gains operations; that interim is recorded in the port's docs (`qa-catalog/src/domain/ports/bundle_store.rs`) and as a convergence note in DECOMPOSITION 2.2, not as a separate ADR.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-qa-fr-catalog-repos`, `cpt-cf-qa-fr-catalog-branch-cache` — the sync engine these features run on
* `cpt-cf-qa-constraint-platform-delegation` — records the git-egress exception (JIRA/SMTP remain on oagw)
* `cpt-cf-qa-contract-egress` — requires the scoped PRD-level exception recorded there (see Consequences); widened on 2026-08-27 from HTTPS-only to HTTPS+SSH
* `cpt-cf-qa-component-catalog` — the owning component of the adapter
