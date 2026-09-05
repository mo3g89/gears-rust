# Platform cluster health — design

**Status:** awaiting approval
**Date:** 2026-08-28
**Gear:** `qa-environments` (plus `qa-environments-sdk`, `qa-platform-ui`)
**Feature gate:** `platform-observation` (existing; no new gate)
**Predecessor:** [2026-08-28 platform observation](./2026-08-28-platform-observation-design.md)

---

## 1. Context

The Platform Details page carries a notice reading *"Cluster health is not available in
this deployment — a platform is modelled here as available or not, and nothing observes
the cluster behind it: node, worker and control-plane readiness, the namespace count, an
overall health status, a status message, the per-node table and the last-checked time have
no source."*

That notice was accurate when it was written and is still accurate today. This design
gives those fields a source.

### What legacy does

`PlatformsService::get_platform_details` (`vhp-testrunner/manager/src/services/platforms.rs:91-228`)
builds a client from the platform's kubeconfig, lists `Node` and `Namespace` cluster-wide,
and derives:

| field | source |
|---|---|
| `nodes[]` | one entry per node: name, role, ready, `kubelet_version`, `os_image` |
| `node_count` / `ready_node_count` | `nodes.len()` / count of `ready` |
| `control_plane_count` / `ready_control_plane_count` | nodes labelled `node-role.kubernetes.io/control-plane` or `…/master` |
| `worker_count` / `ready_worker_count` | the rest |
| `namespace_count` | `namespaces.list().len()` |
| `status` | `Healthy` \| `Degraded` \| `Unhealthy` \| `Warning` \| `Unreachable` |
| `status_message` | a sentence, or `None` when Healthy |
| `checked_at` | now |

Status derivation, verbatim from `platforms.rs:210-226`:

```
node_count == 0                       -> Warning     "Connected, but no nodes were discovered"
ready_node_count == node_count        -> Healthy     (no message)
ready_node_count == 0                 -> Unhealthy   "Connected, but none of the nodes are Ready"
otherwise                             -> Degraded    "{ready}/{total} nodes are Ready"
client or list failed                 -> Unreachable the failure text
```

The dashboard aggregates these into `platforms_summary` by fanning out one
`get_platform_details` per platform in parallel (`manager/src/routes/dashboard.rs:420-466`)
— parallel because each one is a live round trip to a different cluster.

### What already exists on our side

The predecessor design shipped everything this one builds on:

* `infra/observer/kube_observer.rs` already builds a `kube::Client` from a platform's
  kubeconfig — under ADR-0001's waiver it is the only place in the gear allowed to.
* An observation ticker runs every 5 minutes (measured on the remote: cycles at 19:02:36
  and 19:07:36), enumerating every platform and calling `observe`.
* `POST /qa/v1/platforms/{id}/refresh` re-observes one platform on demand, and the
  Platform Version row already has the button that calls it.
* `record_observation` persists an outcome under three documented merge rules
  (`infra/storage/platforms_sea_repo.rs:290-364`).
* `infra/observer/errors.rs` classifies every kubeconfig-derived failure without
  formatting it — the fix for the leak the last final review found.

### What is genuinely missing

Only the cluster read itself and the columns to put it in. There is no new plumbing: the
client, the ticker, the refresh endpoint, the persistence path and the error classifier are
all in place and all reusable.

### Verified feasible before designing

Run against the real `sv-test` cluster from the remote host, read-only:

```
$ kubectl auth can-i list nodes        -> yes
$ kubectl auth can-i list namespaces   -> yes
$ kubectl get nodes                    -> sv-vhp-jele-io  control-plane+master  Ready
                                          v1.33.4+k3s1  Ubuntu 24.04.3 LTS
$ kubectl get ns | wc -l               -> 14
```

The kubeconfig has the cluster-wide list permissions this design needs. Had it not, the
design would be different.

---

## 2. Scope

**In:**

* Reading nodes and namespaces in `infra/observer`, behind the existing feature gate.
* A pure `ClusterHealth` domain type and status derivation, with no Kubernetes types.
* Five new columns and one migration.
* `TargetPlatform` / `PlatformDto` carrying the health.
* Platform Details: the real cluster panel replacing the notice.
* Dashboard strip and Platforms table: dots driven by cluster status.

**Out:**

* **Code coverage.** Separate design, ratified this session. Legacy parses
  `=== COVERAGE_SUMMARY: … ===` out of finished workflow logs; qa-runs serves logs only as
  a live SSE broadcast (a finished run's stream is `stream::empty()`), nothing archives
  them (measured on the remote: 200 runs, 192 finished, **zero** with a `log_storage_ref`),
  our runner emits no marker, and `qa_runs_sdk::Run` has no product key since VHP-319. That
  is a three-gear change and gets its own spec.
* **Leader election.** Still absent, still assessed as wasteful rather than harmful: every
  write is idempotent and every cluster read is a GET.
* **Per-request freshness.** Ratified below as D-CH-1.
* **Pod-level or workload health.** Legacy reads nodes and namespaces; so does this.

---

## 3. Decisions

### D-CH-1 — Ticker-persisted, with the existing Refresh button reading live

*Ratified by the product owner, 2026-08-28.*

Health is read inside the same `observe` call the ticker already makes, persisted, and
served from the database. The Refresh button already on the Platform Version row re-reads
live, because it calls the same `observe`.

The alternative — legacy's read-on-every-request — was rejected for two measured reasons.
The dashboard would fan out one live cluster round trip per platform on every page load;
legacy parallelises precisely because that is expensive. And the UI currently polls
`GET /qa/v1/platforms` **every ~5 seconds** (observed in the remote access log), which
would turn into a continuous stream of cluster reads against every registered platform.

The cost is that `checked_at` can be up to one tick (5 minutes) stale. It is displayed, so
the staleness is visible rather than implied, and Refresh collapses it to zero.

### D-CH-2 — Persist the node list; derive the counts

Legacy persists six counts. Every one is a pure function of the node list, so this design
stores the list and derives the counts in one place. Two consequences, both wanted:

* the counts can never disagree with the list, because there is nothing to disagree with;
* the per-node table legacy shows has a source, rather than being the one field left out.

`cluster_nodes` is JSONB. It is not a relational child table because nothing queries into
it — it is read whole, with the platform, always.

### D-CH-3 — A failed cluster read clears the health rather than keeping it stale

This is a **deliberate divergence from the version merge rule**, which is
"stale-but-known beats blank" (`platforms_sea_repo.rs:349-351`).

That rule is right for a version: a version detected an hour ago is almost certainly still
the version. It is wrong for readiness. Reporting "3/3 nodes Ready" from an hour ago as
though it were current is the exact failure `Unreachable` exists to prevent, and it is the
one a dashboard dot would render as a green light on a dead cluster.

So a failed cluster read writes `cluster_status = 'Unreachable'`, the classified message,
`cluster_nodes = NULL`, `cluster_namespace_count = NULL`, and stamps `cluster_checked_at`.
Legacy reaches the same end by returning zeros; nulls say "unknown" where zeros would say
"measured, and it is zero".

### D-CH-4 — Version detection and health are independent outcomes

Legacy couples them: one client, and a client failure fails everything. But the two need
different RBAC — version detection reads a ConfigMap in one namespace, health lists nodes
and namespaces cluster-wide. A kubeconfig scoped to the platform's namespace would detect
its version perfectly and be forbidden from listing nodes.

Coupling them would report that platform as having no version, which is false. So
`observe` returns both outcomes, from one client, and each is persisted under its own
rule. A platform can legitimately show version 26.5 and health "Unreachable".

### D-CH-5 — The status message goes through the existing classifier

`cluster_status_message` is persisted, served on `PlatformDto` to every
`qa.platform` GET/LIST-authorised caller, and rendered in the UI. That is the identical
path along which the last final review found a private key being echoed onto the platform
page.

Legacy writes `format!("Cluster is unreachable: {}", e)` (`platforms.rs:147`). **That
formatting must not be ported.** The message comes from `errors::describe_kube_error`,
whose contract is that no value derived from the kubeconfig is ever formatted — its only
interpolation is `kube::Error::Api`'s `Status::message` and `code`, which are the API
server's own words and are what make a broken platform fixable.

Note precisely: `describe_kube_error` returns `String`, not `&'static str`, because of that
one branch. `describe_kubeconfig_error` returns `&'static str`. The invariant is
*input-independence*, not the return type, and it is enforced by the canary tests already
in `errors.rs`.

### D-CH-6 — The dashboard and list dots switch to cluster status, falling back to reachability

Today's dot means "did the last detection attempt succeed". With health available it means
legacy's status, on legacy's palette (`manager-ui/src/components/dashboard/PlatformsStrip.tsx:11-24`):

| status | dot |
|---|---|
| Healthy | emerald |
| Degraded, Warning | amber |
| Unhealthy | red |
| Unreachable | dark red |
| no health recorded | falls back to today's reachability dot |

The fallback is what keeps a build without the feature, or a platform never yet checked,
from rendering a manufactured grey "unhealthy".

---

## 4. Architecture

### 4.1 Domain types — pure, no Kubernetes

New in `domain/observation.rs`. Named types are `String`, `bool` and `u32`; nothing here
knows what a `Node` is, which is ADR-0001's waiver clause.

```rust
/// One node, as the cluster reported it.
pub struct NodeSummary {
    pub name: String,
    pub control_plane: bool,
    pub ready: bool,
    pub kubelet_version: Option<String>,
    pub os_image: Option<String>,
}

pub struct ClusterHealth {
    pub nodes: Vec<NodeSummary>,
    pub namespace_count: u32,
}

pub enum ClusterStatus { Healthy, Degraded, Unhealthy, Warning, Unreachable }

pub struct NodeCounts {
    pub total: u32,
    pub ready: u32,
    pub control_plane: u32,
    pub ready_control_plane: u32,
    pub worker: u32,
    pub ready_worker: u32,
}

impl ClusterHealth {
    pub fn counts(&self) -> NodeCounts;
    /// Legacy's derivation, verbatim.
    pub fn status(&self) -> ClusterStatus;
}
```

**No status message is persisted except for `Unreachable`.** Legacy carries one for three
of its five statuses — `"Connected, but no nodes were discovered"` (Warning),
`"Connected, but none of the nodes are Ready"` (Unhealthy), and
`format!("{}/{} nodes are Ready", ready, total)` (Degraded). All three are pure functions
of the status and the counts, both of which are already served, so they are composed in the
UI rather than stored. That leaves `cluster_status_message` carrying text from outside the
process in exactly one case — `Unreachable` — which is the case D-CH-5 is about, and it
means every other row has nothing in that column to leak.

### 4.2 The port

`ObservationOutcome` stays exactly as it is. A second outcome joins it, and `observe`
returns both:

```rust
pub enum HealthOutcome {
    Checked(ClusterHealth),
    Failed(String),
}

pub struct PlatformObservation {
    pub platform: ObservationOutcome,
    pub health: HealthOutcome,
}

async fn observe(&self, kubeconfig: &SecretValue, vpadm_namespace: &str)
    -> PlatformObservation;
```

One call, one client, one TLS handshake — the reason for returning a pair rather than
adding an `observe_health` method, which would double the handshakes per platform per tick.

`NoopObserver` returns `Failed` for both halves, keeping its existing contract that a
build without the feature fails loudly rather than returning a plausible empty success.

### 4.3 The adapter

In `infra/observer/kube_observer.rs`, after the existing detection:

```rust
async fn check_health(client: &Client) -> Result<ClusterHealth, String> {
    // Api::all(client) for Node and Namespace; ListParams::default().
    // A node is control-plane if its labels carry
    // `node-role.kubernetes.io/control-plane` or `node-role.kubernetes.io/master`
    // (legacy checks both; k3s sets both).
    // Ready is the `Ready` condition with status == "True", defaulting to false
    // when the condition is absent — legacy's `unwrap_or(false)`.
}
```

Every error goes through `errors::describe_kube_error`. Nodes are sorted by name, as
legacy sorts them, so the rendered table is stable across ticks.

**The namespace list is a separate failure from the node list.** Legacy tolerates a failed
namespace list with `unwrap_or(0)` — a zero indistinguishable from a cluster that really
has no namespaces. Here a failed namespace list leaves `cluster_namespace_count` **NULL**,
and the UI renders NULL as "not read" rather than as the number zero. The node list alone
determines status: a namespace count that could not be read does not make a healthy cluster
unhealthy.

### 4.4 Persistence

Migration `m20260828_000008_platform_cluster_health.rs`, five nullable columns on
`qa_platforms`:

```
cluster_status           TEXT        NULL   -- Healthy|Degraded|Unhealthy|Warning|Unreachable
cluster_status_message   TEXT        NULL   -- classified; NULL unless Unreachable
cluster_nodes            JSONB       NULL   -- [{name, control_plane, ready, kubelet_version, os_image}]
cluster_namespace_count  INTEGER     NULL
cluster_checked_at       TIMESTAMPTZ NULL
```

All five NULL means **never checked** — the state that renders as today's reachability
dot. `cluster_status = 'Unreachable'` with `cluster_nodes` NULL means checked and failed.
The two are distinct on purpose: "we have never looked" and "we looked and could not
reach it" are different facts and legacy conflates them into one grey dot.

`record_observation` takes the pair and issues **one** UPDATE carrying both halves' column
expressions, so version and health can never be stamped at different instants.

Merge rules, alongside the three the predecessor documents:

| outcome | columns written |
|---|---|
| `Checked` | all five, outright — the cluster is authoritative about its own nodes |
| `Failed` | `cluster_status='Unreachable'`, message, `cluster_nodes=NULL`, `cluster_namespace_count=NULL`, `cluster_checked_at=now` (D-CH-3) |

### 4.5 SDK and DTO

`TargetPlatform` gains one optional field rather than eleven flat nullable ones:

```rust
pub struct ClusterHealthView {
    pub status: String,
    pub status_message: Option<String>,
    pub nodes: Vec<NodeSummary>,
    pub namespace_count: Option<u32>,
    pub counts: NodeCounts,
    pub checked_at: OffsetDateTime,
}

// on TargetPlatform / PlatformDto:
pub cluster: Option<ClusterHealthView>,
```

`None` is "never checked", which is exactly the state the UI must distinguish. Counts are
computed server-side from the node list, so the derivation exists once, in Rust, and the UI
renders rather than re-derives.

`kubeconfig_credstore_ref` remains absent from the DTO. Nothing here changes that.

### 4.6 UI

**Platform Details** — the `UnavailableNotice` at `PlatformDetailPage.tsx:131` is removed
and replaced by a cluster panel: status badge on legacy's palette, `ready/total` for nodes,
control-plane and workers, the namespace count, the last-checked time, and the per-node
table (name, role, ready, kubelet version, OS image). When `cluster` is `null` the panel
states that no health check has run, which is a different sentence from today's "there is
no source for this".

On `Unreachable` the panel shows the status and its message and **suppresses the counts**.
Per D-CH-3 the node list is NULL in that state, so `counts` serialises as all zeros, and
rendering "0/0 nodes Ready" beside "Unreachable" would state a measurement that was never
taken. The zeros are an artefact of the empty list, not a reading.

**Dashboard strip and Platforms table** — dot from `cluster.status` per D-CH-6, falling
back to `platformObservation` when `cluster` is `null`. The strip's count line becomes
legacy's: `N healthy · N degraded · N unhealthy · N unreachable`.

The derivation moves into `lib/platform-observation.ts` beside the existing one, so both
dots come from one module with one test suite.

---

## 5. Error handling

Three failure points, three distinct behaviours:

1. **Kubeconfig cannot be resolved from credstore.** Happens *before* `observe` is called,
   in the service layer. See §8's open item — this is currently invisible in the UI.
2. **A client cannot be built.** Both halves fail; both messages come from the classifier.
3. **A list call fails.** Only that half fails, per D-CH-4.

Nothing in this design formats a `kube::Error`, a `KubeconfigError` or any value derived
from kubeconfig material into a persisted string, a log line, or a DTO field.

---

## 6. Testing

**Pure, in a default kube-free build:**

* `counts()` over: an empty cluster; one control-plane-only node (the real `sv-test` shape);
  a mixed cluster where readiness differs between roles; a node labelled `master` but not
  `control-plane`.
* `status()` over each of the four branches, including the `node_count == 0` → `Warning`
  boundary that a naive `ready == total` would call Healthy — zero equals zero.
* JSON round-trip of `ClusterHealth`, so a stored blob written by one version is readable
  by the next.

**Leak canaries**, following the existing pattern:

* A cluster-read failure over a canary-bearing kubeconfig must classify, not echo — the
  same assertion `errors.rs` makes on the primitive, made again at the point where the
  result reaches `cluster_status_message`.

**Merge rules:**

* A `Failed` health outcome after a `Checked` one must leave `cluster_nodes` NULL, not
  stale (D-CH-3). This is the test that would fail if someone "made it consistent" with the
  version rule.
* A `Failed` health outcome must not disturb `observed_version` (D-CH-4).

**Break-test each guard before trusting it**, as the predecessor did: invert the
`node_count == 0` check, invert D-CH-3's clearing, and confirm each fails exactly the test
named for it.

---

## 7. Verification plan

Against the live `sv-test` cluster, with values predicted from the read-only probe already
run. Anything other than these is a defect, not a surprise:

| field | predicted |
|---|---|
| `cluster.status` | `Healthy` |
| `cluster.status_message` | `null` |
| `cluster.nodes` | one entry: `sv-vhp-jele-io`, `control_plane: true`, `ready: true`, `v1.33.4+k3s1`, `Ubuntu 24.04.3 LTS` |
| `cluster.counts` | `total 1, ready 1, control_plane 1, ready_control_plane 1, worker 0, ready_worker 0` |
| `cluster.namespace_count` | `14` |
| `cluster.checked_at` | within one ticker period of the deploy |
| the other 7 platforms | `cluster: null` — see §8 |

Plus: the Platform Details notice is gone and the panel shows the above; the dashboard chip
for `sv-test` is emerald; `sync.sh` grows a check asserting a non-null `cluster_status` for
at least one platform, in the shape of the existing check 9.

`worker 0` is worth stating out loud: this is a single-node k3s, so the worker row reads
`0/0`. That is correct, not a bug, and the panel should not hide a zero row.

---

## 8. Risks and open items

**A kubeconfig that cannot be resolved is invisible in the UI.** Measured on the remote:
seven of the eight platforms fail at kubeconfig resolution — `platform …'s kubeconfig
secret (argo-proof-kubeconfig) was not found in credstore` — *before* `observe` runs. No
observation column is written, so the UI shows "not yet observed" forever and the actual
reason exists only in a log line, repeated every five minutes. This design does not change
that, and after it those platforms will also show `cluster: null`. Recommendation: record
the resolution failure in `version_detect_error` so it surfaces, which is a small change to
the service layer and a separate one from this. Raised, not decided here.

**Payload growth against a 5-second poll.** `GET /qa/v1/platforms` currently returns 3,195
bytes for eight platforms and the UI polls it every ~5 seconds. Adding a node array per
platform grows that with cluster size. Not a problem at this scale; worth measuring before
a deployment with large clusters. Serving `cluster` only on the single-platform GET, and a
status-only summary on the list, is the obvious mitigation if it is ever needed.

**Cluster-wide RBAC is a real requirement.** Listing nodes and namespaces is cluster-scoped.
`sv-test`'s kubeconfig has it (verified), but a platform registered with a namespace-scoped
kubeconfig will report `Unreachable` while its version detection keeps working. D-CH-4 makes
that state expressible; the UI copy should make it legible, so an operator reads "this
kubeconfig cannot list nodes" rather than "this cluster is down".

**No pod or workload health.** Legacy reads nodes and namespaces only, and a cluster whose
nodes are all Ready can still be running nothing. `Healthy` here means what it means in
legacy, and the panel should not overclaim.
