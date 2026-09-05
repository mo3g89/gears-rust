# Platform Cluster Health Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the Platform Details page's "cluster health is not available" notice a real source, by reading nodes and namespaces inside the observation call that already exists.

**Architecture:** `infra/observer` gains a node/namespace read behind the existing `platform-observation` feature. `domain/observation.rs` gains a pure `ClusterHealth` type with no Kubernetes types in it. One migration adds five nullable columns; `record_observation` writes them in the same UPDATE as the version half. `TargetPlatform`/`PlatformDto` carry one optional `cluster` object. The UI replaces the notice with a real panel and switches its dots to cluster status.

**Tech Stack:** Rust, `kube` 3.1.0 + `k8s-openapi` (feature-gated), `sea-orm` + `sea-orm-migration`, `utoipa`, React + TypeScript + vitest.

**Spec:** `gears/qa-platform/docs/superpowers/specs/2026-08-28-cluster-health-design.md`

## Global Constraints

* **ADR-0001.** No `kube`/`k8s-openapi` type may appear outside `infra/observer/`, and none may appear in `domain/` at all. The waiver is conditional on the `platform-observation` cargo feature; a default build must compile and pass its tests with no `kube` in the tree.
* **No kubeconfig-derived value is ever formatted.** Not `Display`, not `Debug`, not into a message, a log line, or a DTO field. Every `kube::Error` goes through `infra::observer::errors::describe_kube_error`; every `KubeconfigError` through `describe_kubeconfig_error`. This is D-CH-5 and it is the fix for a measured private-key leak.
* **`kubeconfig_credstore_ref` must never reach `PlatformDto`.** Under `SharingMode::Tenant` the reference *is* a read path to the material.
* **Toolchain:** `export PATH="$HOME/.cargo/bin:$PATH"`. Never run `cargo test --all-targets` at workspace level. Never pipe a command whose exit code you need (`cmd | tail` reports `tail`'s status) — redirect to a file and read `$?` directly.
* **Never `docker compose down -v`.** It destroys the seeded tenant resource-group row and qa-catalog's clones.
* **Local only.** No push, no PR.
* Run `cargo clippy` for the crate you touched before every commit. It is `-D warnings` here; a green test suite does not imply a green lint.

---

## File Structure

| file | responsibility |
|---|---|
| `qa-environments/src/domain/observation.rs` | **modify** — add `NodeSummary`, `ClusterHealth`, `ClusterStatus`, `NodeCounts`, and the pure derivations |
| `qa-environments/src/domain/ports/platform_observer.rs` | **modify** — add `HealthOutcome`, `PlatformObservation`; change `observe`'s return type |
| `qa-environments/src/infra/observer/kube_observer.rs` | **modify** — add the node/namespace read |
| `qa-environments/src/infra/storage/migrations/m20260828_000008_platform_cluster_health.rs` | **create** — five nullable columns |
| `qa-environments/src/infra/storage/migrations/mod.rs` | **modify** — register it |
| `qa-environments/src/infra/storage/entity/platform.rs` | **modify** — five fields |
| `qa-environments/src/infra/storage/platforms_sea_repo.rs` | **modify** — write the health half |
| `qa-environments/src/domain/repos/platforms_repo.rs` | **modify** — port signature |
| `qa-environments/src/domain/service/platforms.rs` | **modify** — thread the pair through |
| `qa-environments-sdk/src/models.rs` | **modify** — `ClusterHealthView` on `TargetPlatform` |
| `qa-environments/src/api/rest/dto.rs` | **modify** — `cluster` on `PlatformDto` |
| `qa-platform-ui/src/lib/platform-observation.ts` | **modify** — cluster-status dots beside the existing reachability ones |
| `qa-platform-ui/src/components/platforms/ClusterHealthCard.tsx` | **create** — the panel |
| `qa-platform-ui/src/pages/PlatformDetailPage.tsx` | **modify** — notice out, panel in |
| `qa-platform-ui/src/components/dashboard/PlatformsStrip.tsx` | **modify** — dots and counts |
| `qa-platform-ui/src/components/platforms/PlatformsTable.tsx` | **modify** — status column |
| `deploy/remote/sync.sh` | **modify** — check 10 |

---

## Task 1: Pure cluster-health domain types

**Files:**
- Modify: `qa-environments/qa-environments/src/domain/observation.rs`
- Test: same file, `#[cfg(test)] mod cluster_health_tests`

**Interfaces:**
- Consumes: nothing.
- Produces: `NodeSummary { name: String, control_plane: bool, ready: bool, kubelet_version: Option<String>, os_image: Option<String> }`; `ClusterHealth { nodes: Vec<NodeSummary>, namespace_count: Option<u32> }`; `ClusterStatus` (`Healthy|Degraded|Unhealthy|Warning`); `NodeCounts { total, ready, control_plane, ready_control_plane, worker, ready_worker: u32 }`; `ClusterHealth::counts(&self) -> NodeCounts`; `ClusterHealth::status(&self) -> ClusterStatus`; `ClusterStatus::as_str(&self) -> &'static str`.

Note `namespace_count` is `Option<u32>`, not `u32` — §4.3 of the spec: a namespace list that could not be read is unknown, not zero.

`ClusterStatus` here has **no** `Unreachable` variant. Unreachable is not a property of a cluster we read; it is the absence of a reading, and it lives in `HealthOutcome::Failed` (Task 2) and in the stored string (Task 4). Putting it in this enum would let `status()` return a value it can never derive.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod cluster_health_tests {
    use super::*;

    fn node(name: &str, control_plane: bool, ready: bool) -> NodeSummary {
        NodeSummary {
            name: name.to_owned(),
            control_plane,
            ready,
            kubelet_version: None,
            os_image: None,
        }
    }

    fn health(nodes: Vec<NodeSummary>) -> ClusterHealth {
        ClusterHealth { nodes, namespace_count: Some(14) }
    }

    /// The real `sv-test` shape, measured 2026-08-28: a single-node k3s whose
    /// one node is control-plane. The worker row is 0/0 and that is correct.
    #[test]
    fn a_single_control_plane_node_is_healthy_with_no_workers() {
        let h = health(vec![node("sv-vhp-jele-io", true, true)]);
        assert_eq!(h.status(), ClusterStatus::Healthy);
        let c = h.counts();
        assert_eq!((c.total, c.ready), (1, 1));
        assert_eq!((c.control_plane, c.ready_control_plane), (1, 1));
        assert_eq!((c.worker, c.ready_worker), (0, 0));
    }

    /// The boundary a naive `ready == total` gets wrong: zero equals zero, so
    /// an empty cluster would report Healthy. Legacy checks it first, and so
    /// must this.
    #[test]
    fn a_cluster_with_no_nodes_is_warning_not_healthy() {
        assert_eq!(health(vec![]).status(), ClusterStatus::Warning);
    }

    #[test]
    fn no_node_ready_is_unhealthy() {
        let h = health(vec![node("a", true, false), node("b", false, false)]);
        assert_eq!(h.status(), ClusterStatus::Unhealthy);
        assert_eq!(h.counts().ready, 0);
    }

    #[test]
    fn some_nodes_ready_is_degraded() {
        let h = health(vec![node("a", true, true), node("b", false, false)]);
        assert_eq!(h.status(), ClusterStatus::Degraded);
        let c = h.counts();
        assert_eq!((c.ready, c.total), (1, 2));
    }

    /// Readiness is counted per role, not inferred from the totals: a ready
    /// control-plane next to an unready worker must not report the worker as
    /// ready.
    #[test]
    fn readiness_is_counted_within_each_role() {
        let h = health(vec![
            node("cp1", true, true),
            node("cp2", true, false),
            node("w1", false, true),
            node("w2", false, false),
            node("w3", false, false),
        ]);
        let c = h.counts();
        assert_eq!((c.total, c.ready), (5, 2));
        assert_eq!((c.control_plane, c.ready_control_plane), (2, 1));
        assert_eq!((c.worker, c.ready_worker), (3, 1));
        assert_eq!(h.status(), ClusterStatus::Degraded);
    }

    #[test]
    fn an_unread_namespace_count_is_none_not_zero() {
        let h = ClusterHealth { nodes: vec![node("a", true, true)], namespace_count: None };
        assert_eq!(h.namespace_count, None);
        assert_eq!(h.status(), ClusterStatus::Healthy);
    }

    /// The stored string is a wire value: a rename here silently reclassifies
    /// every row already in the database.
    #[test]
    fn the_status_strings_are_the_stored_wire_values() {
        assert_eq!(ClusterStatus::Healthy.as_str(), "Healthy");
        assert_eq!(ClusterStatus::Degraded.as_str(), "Degraded");
        assert_eq!(ClusterStatus::Unhealthy.as_str(), "Unhealthy");
        assert_eq!(ClusterStatus::Warning.as_str(), "Warning");
    }

    /// A stored blob written by one version must be readable by the next.
    #[test]
    fn cluster_health_round_trips_through_json() {
        let h = health(vec![NodeSummary {
            name: "sv-vhp-jele-io".to_owned(),
            control_plane: true,
            ready: true,
            kubelet_version: Some("v1.33.4+k3s1".to_owned()),
            os_image: Some("Ubuntu 24.04.3 LTS".to_owned()),
        }]);
        let json = serde_json::to_string(&h).expect("serialising ClusterHealth");
        let back: ClusterHealth = serde_json::from_str(&json).expect("deserialising ClusterHealth");
        assert_eq!(back, h);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p qa-environments cluster_health_tests`
Expected: FAIL — `NodeSummary` etc. not found.

- [ ] **Step 3: Implement**

```rust
/// One node, as the cluster reported it. Pure: nothing here is a `k8s_openapi`
/// type, so every rule below is testable in a default build with no `kube` in
/// the tree (ADR-0001 waiver amendment, 2026-08-28).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeSummary {
    pub name: String,
    pub control_plane: bool,
    pub ready: bool,
    pub kubelet_version: Option<String>,
    pub os_image: Option<String>,
}

/// One cluster read.
///
/// `namespace_count` is `Option`, not `u32`: legacy folds a failed namespace
/// list into `unwrap_or(0)`, a zero indistinguishable from a cluster that
/// really has no namespaces. `None` here means "not read".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClusterHealth {
    pub nodes: Vec<NodeSummary>,
    pub namespace_count: Option<u32>,
}

/// Legacy's four derivable statuses.
///
/// **No `Unreachable` variant on purpose.** Unreachable is not a property of a
/// cluster that was read; it is the absence of a reading, and it lives in
/// `HealthOutcome::Failed`. A variant here would be one `status()` can never
/// return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Warning,
}

impl ClusterStatus {
    /// The stored and published spelling. These strings are a wire contract
    /// with `qa_platforms.cluster_status` and with the UI's dot palette.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "Healthy",
            Self::Degraded => "Degraded",
            Self::Unhealthy => "Unhealthy",
            Self::Warning => "Warning",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeCounts {
    pub total: u32,
    pub ready: u32,
    pub control_plane: u32,
    pub ready_control_plane: u32,
    pub worker: u32,
    pub ready_worker: u32,
}

impl ClusterHealth {
    /// Counts derived, never stored (D-CH-2): they cannot disagree with the
    /// node list because there is nothing to disagree with.
    #[must_use]
    pub fn counts(&self) -> NodeCounts {
        let count = |f: &dyn Fn(&NodeSummary) -> bool| -> u32 {
            u32::try_from(self.nodes.iter().filter(|n| f(n)).count()).unwrap_or(u32::MAX)
        };
        NodeCounts {
            total: count(&|_| true),
            ready: count(&|n| n.ready),
            control_plane: count(&|n| n.control_plane),
            ready_control_plane: count(&|n| n.control_plane && n.ready),
            worker: count(&|n| !n.control_plane),
            ready_worker: count(&|n| !n.control_plane && n.ready),
        }
    }

    /// Legacy's derivation, verbatim (`manager/src/services/platforms.rs:210-226`).
    ///
    /// The `total == 0` arm must come first: with no nodes, `ready == total`
    /// is `0 == 0` and an empty cluster would otherwise report Healthy.
    #[must_use]
    pub fn status(&self) -> ClusterStatus {
        let counts = self.counts();
        if counts.total == 0 {
            ClusterStatus::Warning
        } else if counts.ready == counts.total {
            ClusterStatus::Healthy
        } else if counts.ready == 0 {
            ClusterStatus::Unhealthy
        } else {
            ClusterStatus::Degraded
        }
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p qa-environments cluster_health_tests`
Expected: PASS, 8 tests.

- [ ] **Step 5: Break-test the ordering guard**

Reorder `status()` so the `ready == total` arm precedes the `total == 0` arm. Run the tests again and confirm `a_cluster_with_no_nodes_is_warning_not_healthy` fails and the others pass. Restore. Report which test failed in your report — a break-test that fails nothing means the guard is not guarded.

- [ ] **Step 6: Lint and commit**

```bash
cargo clippy -p qa-environments --all-targets
git add gears/qa-platform/qa-environments/qa-environments/src/domain/observation.rs
git commit -m "feat(qa-environments): pure cluster-health types and legacy's status derivation"
```

---

## Task 2: The observation port carries two outcomes

**Files:**
- Modify: `qa-environments/qa-environments/src/domain/ports/platform_observer.rs`
- Test: same file's existing `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: Task 1's `ClusterHealth`.
- Produces: `HealthOutcome { Checked(ClusterHealth), Failed(String) }`; `PlatformObservation { platform: ObservationOutcome, health: HealthOutcome }`; `PlatformObserver::observe` now returns `PlatformObservation`.

`ObservationOutcome` is **unchanged** — do not rename it, do not add variants. Task 4 and Task 5 both match on it as it stands.

- [ ] **Step 1: Write the failing test**

```rust
    /// D-CH-4: the two halves are independent, and the no-op observer must
    /// fail both rather than pretending either succeeded.
    #[tokio::test]
    async fn the_noop_observer_fails_both_halves() {
        let observation = NoopObserver
            .observe(&SecretValue::from("irrelevant".to_owned()), "virtuozzo")
            .await;
        assert!(matches!(observation.platform, ObservationOutcome::Failed(_)));
        match observation.health {
            HealthOutcome::Failed(message) => assert!(
                message.contains("platform-observation"),
                "the message must name the missing feature, got: {message}"
            ),
            HealthOutcome::Checked(_) => {
                panic!("a build with no Kubernetes client must never report a cluster read")
            }
        }
    }
```

Also update the two existing tests to destructure `observation.platform` rather than the old bare return value.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p qa-environments platform_observer`
Expected: FAIL — `HealthOutcome` not found.

- [ ] **Step 3: Implement**

```rust
/// The result of one cluster-health read, kept separate from
/// [`ObservationOutcome`] because the two fail independently (D-CH-4).
///
/// Legacy couples them — one client, and a client failure fails everything —
/// but version detection reads a single `ConfigMap` in one namespace while
/// health lists nodes and namespaces cluster-wide. A kubeconfig scoped to the
/// platform's namespace detects its version perfectly and is forbidden from
/// listing nodes; coupling would report that platform as having no version,
/// which is false.
#[derive(Debug, Clone)]
pub enum HealthOutcome {
    Checked(ClusterHealth),
    Failed(String),
}

/// Both halves of one `observe` call.
///
/// One call rather than two methods, so one client and one TLS handshake serve
/// both — an `observe_health` alongside `observe` would double the handshakes
/// per platform per ticker cycle.
#[derive(Debug, Clone)]
pub struct PlatformObservation {
    pub platform: ObservationOutcome,
    pub health: HealthOutcome,
}
```

Change the trait method to `async fn observe(&self, kubeconfig: &SecretValue, vpadm_namespace: &str) -> PlatformObservation;` and update `NoopObserver` to return both halves `Failed` with its existing message.

**Five more implementors live in `qa-environments/src/test_support.rs`** and will stop
compiling with this change: `ScriptedObserver` (:409), `KeyedObserver` (:465),
`NamespaceRecordingObserver` (:517), `RecordingSecretObserver` (:562) and
`FailingSecretObserver` (:599). Update all five here, in this task, so the crate compiles
for the next one. Each keeps its existing `platform` behaviour verbatim and gains a
`health` half; give them all `HealthOutcome::Failed("health not scripted".to_owned())`
by default, because none of them exists to exercise health and a `Checked` default would
silently satisfy Task 5's assertions without anything having read a cluster.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p qa-environments platform_observer`
Expected: PASS. `cargo check -p qa-environments` will now fail at `kube_observer` and `service::platforms` — that is Tasks 3 and 5, and is expected at this point. Note it in your report; do not fix it here.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-environments/qa-environments/src/domain/ports/platform_observer.rs \
        gears/qa-platform/qa-environments/qa-environments/src/test_support.rs
git commit -m "feat(qa-environments): observe() returns version and health as independent outcomes"
```

---

## Task 3: The Kubernetes adapter reads nodes and namespaces

**Files:**
- Modify: `qa-environments/qa-environments/src/infra/observer/kube_observer.rs`
- Test: same file's `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: Tasks 1 and 2; `errors::describe_kube_error`.
- Produces: nothing new outside the crate — `KubeObserver::observe` now satisfies the Task 2 signature.

This is the **only** file in this plan permitted to name a `kube` or `k8s_openapi` type. Everything here is inside the crate's existing `#[cfg(feature = "platform-observation")]` module.

- [ ] **Step 1: Write the failing test**

```rust
    /// D-CH-5: a cluster read that fails must classify, never echo. The
    /// canary is a token in the kubeconfig — the material this whole module
    /// exists to keep out of `cluster_status_message`.
    #[tokio::test]
    async fn a_failed_cluster_read_classifies_rather_than_echoing_the_kubeconfig() {
        const CANARY: &str = "CANARY-health-do-not-echo-me";
        // Bind then drop: the port is free, nothing listens, so the connect
        // is refused and both halves fail at the transport.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let document = format!(
            "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster:\n    \
             server: https://127.0.0.1:{port}\n    insecure-skip-tls-verify: true\n\
             contexts:\n- name: c\n  context:\n    cluster: c\n    user: u\n\
             current-context: c\nusers:\n- name: u\n  user:\n    token: {CANARY}\n"
        );
        let observation = observer()
            .observe(&SecretValue::from(document), "virtuozzo")
            .await;

        match observation.health {
            HealthOutcome::Failed(message) => {
                assert!(
                    !message.contains(CANARY),
                    "the kubeconfig's token must never reach the health message, got: {message}"
                );
            }
            HealthOutcome::Checked(_) => panic!("nothing listens on that port"),
        }
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p qa-environments --features platform-observation kube_observer`
Expected: FAIL to compile — `observe` still returns the old type.

- [ ] **Step 3: Implement**

```rust
use k8s_openapi::api::core::v1::{Namespace, Node};

/// Read the cluster's nodes and namespaces.
///
/// Two independent reads. The node list determines the status, so a failure
/// there fails the whole health outcome; the namespace count is
/// supplementary, so a failure there leaves it `None` (spec §4.3) rather
/// than making a healthy cluster look unhealthy.
///
/// Nodes are sorted by name, as legacy sorts them, so the rendered table is
/// stable across ticks rather than reordering on every poll.
async fn check_health(client: &Client) -> Result<ClusterHealth, String> {
    let nodes_api: Api<Node> = Api::all(client.clone());
    let namespaces_api: Api<Namespace> = Api::all(client.clone());

    let items = nodes_api
        .list(&ListParams::default())
        .await
        .map_err(|error| super::errors::describe_kube_error(&error))?;

    let mut nodes: Vec<NodeSummary> = items
        .items
        .into_iter()
        .map(|node| {
            let labels = node.metadata.labels.unwrap_or_default();
            // k3s sets both; legacy checks both.
            let control_plane = labels.contains_key("node-role.kubernetes.io/control-plane")
                || labels.contains_key("node-role.kubernetes.io/master");
            let status = node.status.as_ref();
            // Legacy's `unwrap_or(false)`: an absent Ready condition is not
            // ready. A node that does not say it is ready is not ready.
            let ready = status
                .and_then(|s| s.conditions.as_ref())
                .and_then(|c| c.iter().find(|c| c.type_ == "Ready"))
                .is_some_and(|c| c.status == "True");
            let info = status.and_then(|s| s.node_info.as_ref());
            NodeSummary {
                name: node.metadata.name.unwrap_or_else(|| "unknown-node".to_owned()),
                control_plane,
                ready,
                kubelet_version: info.map(|i| i.kubelet_version.clone()),
                os_image: info.map(|i| i.os_image.clone()),
            }
        })
        .collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));

    let namespace_count = match namespaces_api.list(&ListParams::default()).await {
        Ok(list) => u32::try_from(list.items.len()).ok(),
        Err(error) => {
            // Classified, not formatted, even though this one only reaches a
            // log line: the rule is the rule everywhere in this module.
            tracing::debug!(
                reason = super::errors::describe_kube_error(&error),
                "namespace list failed; the count is left unknown"
            );
            None
        }
    };

    Ok(ClusterHealth { nodes, namespace_count })
}
```

Then rewrite `observe` to build the client once and drive both halves from it:

```rust
    async fn observe(&self, kubeconfig: &SecretValue, vpadm_namespace: &str) -> PlatformObservation {
        let client = match Self::client_from_kubeconfig(kubeconfig).await {
            Ok(client) => client,
            Err(message) => {
                // One client for both halves, so a client that could not be
                // built fails both with the same classified message.
                return PlatformObservation {
                    platform: ObservationOutcome::Failed(message.clone()),
                    health: HealthOutcome::Failed(message),
                };
            }
        };

        let platform = match Self::detect(&client, vpadm_namespace).await {
            Ok(mut detected) => {
                detected.base_domain = Self::detect_base_domain(&client, &detected.namespace).await;
                ObservationOutcome::Detected(detected)
            }
            Err(message) => ObservationOutcome::Failed(message),
        };

        let health = match Self::check_health(&client).await {
            Ok(health) => HealthOutcome::Checked(health),
            Err(message) => HealthOutcome::Failed(message),
        };

        PlatformObservation { platform, health }
    }
```

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test -p qa-environments --features platform-observation > /tmp/t.log 2>&1; echo "EXIT=$?"
```
Expected: EXIT=0 (the crate compiles again for this feature; `service::platforms` is Task 5 and may still fail — if so, note it and proceed).

- [ ] **Step 5: Prove the default build still has no `kube`**

```bash
cargo test -p qa-environments > /tmp/default.log 2>&1; echo "EXIT=$?"
cargo tree -p qa-environments -i kube > /tmp/tree.log 2>&1; echo "TREE_EXIT=$?"
```
Expected: the default test run passes, and `cargo tree -i kube` reports the package is not in the default graph. This is the ADR-0001 gate; do not skip it.

- [ ] **Step 6: Lint and commit**

```bash
cargo clippy -p qa-environments --features platform-observation --all-targets
git add gears/qa-platform/qa-environments/qa-environments/src/infra/observer/kube_observer.rs
git commit -m "feat(qa-environments): read nodes and namespaces alongside version detection"
```

---

## Task 4: Migration, entity, and the health merge rule

**Files:**
- Create: `qa-environments/qa-environments/src/infra/storage/migrations/m20260828_000008_platform_cluster_health.rs`
- Modify: `qa-environments/qa-environments/src/infra/storage/migrations/mod.rs`
- Modify: `qa-environments/qa-environments/src/infra/storage/entity/platform.rs`
- Modify: `qa-environments/qa-environments/src/infra/storage/platforms_sea_repo.rs`
- Modify: `qa-environments/qa-environments/src/domain/repos/platforms_repo.rs`

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces: `PlatformsRepository::record_observation(&self, runner, scope, id, observation: &PlatformObservation)` — the parameter changes from `&ObservationOutcome` to `&PlatformObservation`.

- [ ] **Step 1: Write the migration**

Follow `m20260828_000007_platform_observation.rs` exactly — same three-dialect `const` blobs, same `sql_for`, same `up`/`down` shape, same `#[cfg(test)]` schema-test module writing **non-NULL** values for all five columns before asserting the round trip.

```rust
const POSTGRES_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN cluster_status TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_status_message TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_nodes JSONB NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_namespace_count INTEGER NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_checked_at TIMESTAMPTZ NULL;
";

const MYSQL_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN cluster_status TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_status_message TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_nodes JSON NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_namespace_count INTEGER NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_checked_at TIMESTAMP NULL;
";

const SQLITE_UP: &str = r"
ALTER TABLE qa_platforms ADD COLUMN cluster_status TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_status_message TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_nodes TEXT NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_namespace_count INTEGER NULL;
ALTER TABLE qa_platforms ADD COLUMN cluster_checked_at TEXT NULL;
";
```

Module doc must record: why a new file rather than an edit (every earlier migration must be assumed to have run); why no `IF NOT EXISTS` (SQLite rejects it, MySQL has no such clause); why `JSONB`/`JSON`/`TEXT` differ per dialect; and why all five are nullable with no backfill (an existing platform has never been health-checked, so NULL is the only honest value).

- [ ] **Step 2: Register it and add the entity fields**

`migrations/mod.rs`: add `mod m20260828_000008_platform_cluster_health;` and its `Box::new(...)` at the end of the vec.

`entity/platform.rs`: five fields, each with a doc comment naming its writer and its merge rule.

```rust
    /// `Healthy`|`Degraded`|`Unhealthy`|`Warning` from a successful cluster
    /// read, or `Unreachable` when the read failed. `NULL` means **never
    /// checked**, which is a different fact from `Unreachable` and is rendered
    /// differently. Writer: `OrmPlatformsRepository::record_observation`.
    pub cluster_status: Option<String>,
    /// Set only when `cluster_status` is `Unreachable`; `NULL` for every other
    /// status. The three sentences legacy attaches to Warning, Unhealthy and
    /// Degraded are pure functions of the status and the counts, so they are
    /// composed in the UI rather than stored — which means every non-
    /// `Unreachable` row has nothing in this column to leak. The value that
    /// does land here comes from `infra::observer::errors`, never from
    /// formatting a `kube::Error` (D-CH-5).
    pub cluster_status_message: Option<String>,
    /// The node list as JSON. Every count the UI shows is derived from this,
    /// so no stored count can disagree with it (D-CH-2). `NULL` after a failed
    /// read, per D-CH-3.
    pub cluster_nodes: Option<serde_json::Value>,
    /// `NULL` means the namespace list could not be read — **not** zero
    /// namespaces.
    pub cluster_namespace_count: Option<i32>,
    /// When the last cluster read was attempted, success or failure.
    pub cluster_checked_at: Option<OffsetDateTime>,
```

- [ ] **Step 3: Write the failing merge-rule tests**

In `platforms_sea_repo.rs`'s existing test module. The existing `detected(...)` helper
returns an `ObservationOutcome` and every current call site passes it straight to
`record_observation`; with the signature change it must now be wrapped. Add these two
helpers beside it rather than changing `detected` itself, so the existing tests change by
one wrapper call each and nothing else:

```rust
    /// Wrap the two halves for `record_observation`'s new parameter.
    fn observation(platform: ObservationOutcome, health: HealthOutcome) -> PlatformObservation {
        PlatformObservation { platform, health }
    }

    /// A `Checked` health outcome with `count` nodes, all ready, the first of
    /// them control-plane -- the `sv-test` shape when `count == 1`.
    fn checked(count: usize, namespace_count: Option<u32>) -> HealthOutcome {
        HealthOutcome::Checked(ClusterHealth {
            nodes: (0..count)
                .map(|i| NodeSummary {
                    name: format!("node-{i}"),
                    control_plane: i == 0,
                    ready: true,
                    kubelet_version: Some("v1.33.4+k3s1".to_owned()),
                    os_image: Some("Ubuntu 24.04.3 LTS".to_owned()),
                })
                .collect(),
            namespace_count,
        })
    }
```

Every existing `record_observation(&conn, &scope, id, &detected(...))` call becomes
`&observation(detected(...), checked(1, Some(14)))`.

```rust
    /// D-CH-3, and the reason it diverges from the version rule above: a
    /// failed read must CLEAR the node list, not keep it. Stale readiness
    /// rendered as a green dot on a dead cluster is the exact failure
    /// `Unreachable` exists to prevent.
    #[tokio::test]
    async fn a_failed_health_read_clears_the_nodes_rather_than_keeping_them() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(detected("26.5", Some("0"), "virtuozzo", None), checked(2, Some(14))),
            )
            .await
            .unwrap();

        // The precondition: without this the assertions below could pass on a
        // row that never held nodes in the first place.
        let seeded = fetch_row(&conn, &scope, id).await;
        assert_eq!(seeded.cluster_status.as_deref(), Some("Healthy"));
        assert!(seeded.cluster_nodes.is_some(), "the seeded read must have stored nodes");
        assert_eq!(seeded.cluster_namespace_count, Some(14));
        let seeded_at = seeded.cluster_checked_at.expect("a stamped check time");

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", None),
                    HealthOutcome::Failed("the API server could not be reached".to_owned()),
                ),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(row.cluster_status.as_deref(), Some("Unreachable"));
        assert_eq!(
            row.cluster_status_message.as_deref(),
            Some("the API server could not be reached")
        );
        assert!(
            row.cluster_nodes.is_none(),
            "D-CH-3: a failed read must clear the node list, not keep an hour-old one"
        );
        assert!(
            row.cluster_namespace_count.is_none(),
            "D-CH-3: the namespace count is part of the same reading"
        );
        assert!(
            row.cluster_checked_at.expect("still stamped") >= seeded_at,
            "a failed attempt is still an attempt and must stamp the time"
        );
    }

    /// D-CH-4: the halves are independent, so a failed health read must not
    /// disturb a version that was detected in the same call.
    #[tokio::test]
    async fn a_failed_health_read_leaves_the_observed_version_alone() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", Some("sv.jele.io")),
                    checked(1, Some(14)),
                ),
            )
            .await
            .unwrap();
        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    detected("26.5", Some("0"), "virtuozzo", Some("sv.jele.io")),
                    HealthOutcome::Failed("cannot list nodes".to_owned()),
                ),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(row.observed_version.as_deref(), Some("26.5"));
        assert_eq!(row.observed_build.as_deref(), Some("0"));
        assert_eq!(row.vhp_base_url.as_deref(), Some("https://sv.jele.io"));
        assert!(row.version_detect_error.is_none());
        assert_eq!(row.cluster_status.as_deref(), Some("Unreachable"));
    }

    /// The mirror: a failed version detection must not clear a cluster read
    /// that succeeded in the same call. Without this test, "clear everything
    /// on any failure" would pass the one above.
    #[tokio::test]
    async fn a_failed_version_detection_leaves_a_successful_health_read_alone() {
        let db = inmem_db().await;
        let conn = db.conn().unwrap();
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let id = seed_platform(&conn, &scope, tenant, "p1").await;

        super::OrmPlatformsRepository
            .record_observation(
                &conn,
                &scope,
                id,
                &observation(
                    ObservationOutcome::Failed("namespaces \"virtuozzo\" not found".to_owned()),
                    checked(1, Some(14)),
                ),
            )
            .await
            .unwrap();

        let row = fetch_row(&conn, &scope, id).await;
        assert_eq!(
            row.version_detect_error.as_deref(),
            Some("namespaces \"virtuozzo\" not found")
        );
        assert_eq!(row.cluster_status.as_deref(), Some("Healthy"));
        assert!(row.cluster_nodes.is_some(), "the health half succeeded and must be stored");
        assert_eq!(row.cluster_namespace_count, Some(14));
    }
```

- [ ] **Step 4: Run to verify they fail**

Run: `cargo test -p qa-environments platforms_sea_repo`
Expected: FAIL — the columns and the new parameter do not exist.

- [ ] **Step 5: Implement the repository half**

Change the port signature in `domain/repos/platforms_repo.rs` to take `observation: &PlatformObservation`, and extend its doc to name the two new rules alongside the existing three.

In `record_observation`, keep the existing `match` on `observation.platform` exactly as it is, then chain the health half onto the same `update` so both are one statement:

```rust
        let update = match &observation.health {
            HealthOutcome::Checked(health) => {
                let nodes = serde_json::to_value(&health.nodes).unwrap_or(serde_json::Value::Null);
                update
                    // The cluster is authoritative about its own nodes:
                    // overwrite outright, exactly as observed_version does.
                    .col_expr(PlatformColumn::ClusterStatus,
                              Expr::value(Some(health.status().as_str().to_owned())))
                    .col_expr(PlatformColumn::ClusterStatusMessage, Expr::value(None::<String>))
                    .col_expr(PlatformColumn::ClusterNodes, Expr::value(Some(nodes)))
                    .col_expr(PlatformColumn::ClusterNamespaceCount,
                              Expr::value(health.namespace_count.and_then(|n| i32::try_from(n).ok())))
                    .col_expr(PlatformColumn::ClusterCheckedAt, Expr::value(Some(now)))
            }
            HealthOutcome::Failed(message) => update
                // D-CH-3: CLEARED, not kept. Unlike the version half above,
                // stale-but-known is worse than blank here.
                .col_expr(PlatformColumn::ClusterStatus,
                          Expr::value(Some("Unreachable".to_owned())))
                .col_expr(PlatformColumn::ClusterStatusMessage,
                          Expr::value(Some(message.clone())))
                .col_expr(PlatformColumn::ClusterNodes, Expr::value(None::<serde_json::Value>))
                .col_expr(PlatformColumn::ClusterNamespaceCount, Expr::value(None::<i32>))
                .col_expr(PlatformColumn::ClusterCheckedAt, Expr::value(Some(now))),
        };
```

`now` is the same binding the version half already uses, so both halves carry one timestamp.

- [ ] **Step 6: Run to verify they pass**

```bash
cargo test -p qa-environments > /tmp/t4.log 2>&1; echo "EXIT=$?"
```
Expected: EXIT=0 for everything except `service::platforms`, which is Task 5.

- [ ] **Step 7: Break-test D-CH-3**

Change the `Failed` arm to omit the two clearing `col_expr` calls (i.e. behave like the version rule). Re-run and confirm `a_failed_health_read_clears_the_nodes_rather_than_keeping_them` fails. Restore. Name the failing test in your report.

- [ ] **Step 8: Lint and commit**

```bash
cargo clippy -p qa-environments --all-targets
git add gears/qa-platform/qa-environments/qa-environments/src/infra/storage gears/qa-platform/qa-environments/qa-environments/src/domain/repos/platforms_repo.rs
git commit -m "feat(qa-environments): persist cluster health, clearing it on a failed read"
```

---

## Task 5: Thread the pair through the service, SDK and DTO

**Files:**
- Modify: `qa-environments/qa-environments/src/domain/service/platforms.rs`
- Modify: `qa-environments/qa-environments-sdk/src/models.rs`
- Modify: `qa-environments/qa-environments/src/api/rest/dto.rs`
- Modify: `qa-environments/qa-environments/src/test_support.rs` (the five observers, if Task 2 left anything)

**Interfaces:**
- Consumes: Tasks 1-4.
- Produces: `sdk::ClusterHealthView { status: String, status_message: Option<String>, nodes: Vec<NodeSummary>, namespace_count: Option<u32>, counts: NodeCounts, checked_at: OffsetDateTime }` and `TargetPlatform::cluster: Option<ClusterHealthView>`; the same shape as `ClusterHealthDto` on `PlatformDto` under `cluster`.

`None` means never checked. A failed read is `Some` with `status: "Unreachable"` and an empty `nodes`. The UI must be able to tell those apart — that is the whole reason this is one optional object rather than eleven flat nullable fields.

- [ ] **Step 1: Write the failing test**

In `domain/service/platforms_tests.rs`, using `ScriptedObserver` to drive each outcome.

```rust
    /// The three states the UI has to tell apart, and the ones a flat set of
    /// nullable fields would blur together:
    ///   never looked        -> cluster is None
    ///   looked, unreachable -> cluster is Some, status "Unreachable"
    ///   looked, fine        -> cluster is Some, status "Healthy"
    /// A `cluster: None` and a `status: "Unreachable"` are different facts and
    /// this is the test that keeps them different.
    #[tokio::test]
    async fn never_checked_unreachable_and_healthy_are_three_distinct_states() {
        let harness = /* the crate's existing PlatformsService harness */;
        let created = harness.create_platform("p1").await.unwrap();
        assert!(
            created.cluster.is_none(),
            "a platform no cycle has reached has never been looked at"
        );

        harness.observer.script(PlatformObservation {
            platform: ObservationOutcome::Failed("namespaces not found".to_owned()),
            health: HealthOutcome::Failed("the API server could not be reached".to_owned()),
        });
        let unreachable = harness.observe(created.id).await.unwrap();
        let view = unreachable.cluster.expect("checked, so a view exists");
        assert_eq!(view.status, "Unreachable");
        assert_eq!(
            view.status_message.as_deref(),
            Some("the API server could not be reached")
        );
        assert!(view.nodes.is_empty(), "a failed read stores no nodes (D-CH-3)");
        assert_eq!(view.counts.total, 0);
        assert_eq!(view.namespace_count, None);

        harness.observer.script(PlatformObservation {
            platform: ObservationOutcome::Detected(/* 26.5 / 0 / virtuozzo */),
            health: HealthOutcome::Checked(ClusterHealth {
                nodes: vec![NodeSummary {
                    name: "sv-vhp-jele-io".to_owned(),
                    control_plane: true,
                    ready: true,
                    kubelet_version: Some("v1.33.4+k3s1".to_owned()),
                    os_image: Some("Ubuntu 24.04.3 LTS".to_owned()),
                }],
                namespace_count: Some(14),
            }),
        });
        let healthy = harness.observe(created.id).await.unwrap();
        let view = healthy.cluster.expect("checked, so a view exists");
        assert_eq!(view.status, "Healthy");
        assert_eq!(
            view.status_message, None,
            "only Unreachable stores a message; the rest are composed in the UI"
        );
        assert_eq!(view.counts.total, 1);
        assert_eq!(view.counts.control_plane, 1);
        assert_eq!(view.counts.worker, 0);
        assert_eq!(view.namespace_count, Some(14));
        assert_eq!(view.nodes[0].kubelet_version.as_deref(), Some("v1.33.4+k3s1"));
    }
```

The `/* … */` placeholders are the harness and the `DetectedPlatform` literal this
crate's existing tests already build — match whatever `platforms_tests.rs` uses today
rather than inventing a second way to construct them. Everything that matters to this
task is asserted above.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p qa-environments platforms_tests`
Expected: FAIL — no `cluster` field.

- [ ] **Step 3: Implement**

`observe_cluster` returns `PlatformObservation` instead of `ObservationOutcome`; `observe_platform` and `run_observation_cycle` pass it to `record_observation` unchanged. The `SecurityContext`-level authorization stays exactly as it is — `actions::UPDATE`, for the reason its existing comment gives.

`ClusterHealthView::from` builds `counts` by calling `ClusterHealth::counts()`, so the derivation exists once in Rust and the UI renders rather than re-derives.

Map the stored row: `cluster_status.is_none()` → `None`; otherwise `Some(view)` with `nodes` deserialised from `cluster_nodes` (defaulting to empty on a NULL or an unparseable blob) and `counts` derived from those nodes.

`PlatformDto` gains `pub cluster: Option<ClusterHealthDto>` with a doc comment stating: `None` is never-checked; `Unreachable` is checked-and-failed; and that `status_message` carries classified text only, never a formatted `kube::Error`.

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test -p qa-environments > /tmp/t5.log 2>&1; echo "EXIT=$?"
cargo test -p qa-environments --features platform-observation > /tmp/t5f.log 2>&1; echo "EXIT=$?"
```
Expected: EXIT=0 for both.

- [ ] **Step 5: Regenerate the OpenAPI types the UI reads**

```bash
cargo check -p cf-gears-example-server --features platform-observation > /tmp/srv.log 2>&1; echo "EXIT=$?"
```
Expected: EXIT=0. The UI's `src/api/generated/openapi.d.ts` is refreshed in Task 6 against a running server.

- [ ] **Step 6: Lint and commit**

```bash
cargo clippy -p qa-environments --all-targets
cargo clippy -p qa-environments --features platform-observation --all-targets
git add gears/qa-platform/qa-environments
git commit -m "feat(qa-environments): serve cluster health on TargetPlatform and PlatformDto"
```

---

## Task 6: UI — the cluster panel, and dots that mean health

**Files:**
- Modify: `qa-platform-ui/src/lib/platform-observation.ts`
- Test: `qa-platform-ui/src/lib/platform-observation.test.ts`
- Modify: `qa-platform-ui/src/api/types.ts`, `src/api/adapters.ts`
- Create: `qa-platform-ui/src/components/platforms/ClusterHealthCard.tsx`
- Modify: `qa-platform-ui/src/pages/PlatformDetailPage.tsx`
- Modify: `qa-platform-ui/src/components/dashboard/PlatformsStrip.tsx`
- Modify: `qa-platform-ui/src/components/platforms/PlatformsTable.tsx`

**Interfaces:**
- Consumes: Task 5's `cluster` field on `PlatformDto`.
- Produces: `clusterDotClass(status: string): string`; `clusterStatusMessage(cluster: ClusterHealth): string | null`; `platformDotClass(platform: PlatformInfo): string`.

`.test.ts`, never `.test.tsx` — `vitest.config.ts`'s include glob is `src/**/*.test.ts` only, so a `.tsx` test silently does not run.

- [ ] **Step 1: Write the failing tests**

Three fixture helpers first. `platform()` already exists in this file; add the other two
beside it:

```ts
function counts(overrides: Partial<NodeCounts> = {}): NodeCounts {
  return {
    total: 0, ready: 0,
    control_plane: 0, ready_control_plane: 0,
    worker: 0, ready_worker: 0,
    ...overrides,
  };
}

function cluster(overrides: Partial<ClusterHealth> = {}): ClusterHealth {
  return {
    status: 'Healthy',
    status_message: null,
    nodes: [],
    namespace_count: null,
    counts: counts(),
    checked_at: '2026-08-28T18:27:36Z',
    ...overrides,
  };
}
```

```ts
describe('clusterDotClass', () => {
  it('uses legacy’s palette, with Degraded and Warning sharing amber', () => {
    expect(clusterDotClass('Healthy')).toBe('bg-emerald-500');
    expect(clusterDotClass('Degraded')).toBe('bg-amber-500');
    expect(clusterDotClass('Warning')).toBe('bg-amber-500');
    expect(clusterDotClass('Unhealthy')).toBe('bg-red-500');
    expect(clusterDotClass('Unreachable')).toBe('bg-red-700');
  });

  it('does not invent a colour for a status it does not know', () => {
    expect(clusterDotClass('Sideways')).toBe('bg-muted-foreground');
  });
});

describe('clusterStatusMessage', () => {
  // The three sentences legacy stores are composed here instead, because
  // they are pure functions of status + counts (spec §4.1).
  it('composes the Degraded sentence from the counts', () => {
    expect(clusterStatusMessage(cluster({ status: 'Degraded', counts: counts({ total: 3, ready: 1 }) })))
      .toBe('1/3 nodes are Ready');
  });
  it('names the zero-node case', () => {
    expect(clusterStatusMessage(cluster({ status: 'Warning', counts: counts({ total: 0, ready: 0 }) })))
      .toBe('Connected, but no nodes were discovered');
  });
  it('names the none-ready case', () => {
    expect(clusterStatusMessage(cluster({ status: 'Unhealthy', counts: counts({ total: 2, ready: 0 }) })))
      .toBe('Connected, but none of the nodes are Ready');
  });
  it('prefers the server’s own message when Unreachable', () => {
    expect(clusterStatusMessage(cluster({ status: 'Unreachable', status_message: 'the API server could not be reached' })))
      .toBe('the API server could not be reached');
  });
  it('says nothing when Healthy', () => {
    expect(clusterStatusMessage(cluster({ status: 'Healthy' }))).toBeNull();
  });
});

describe('platformDotClass', () => {
  it('uses cluster status when health has been checked', () => {
    expect(platformDotClass(platform({ cluster: cluster({ status: 'Healthy' }) }))).toBe('bg-emerald-500');
  });

  // D-CH-6: without this fallback a build with the feature off, or a platform
  // no cycle has reached, would render a manufactured "unhealthy" dot.
  it('falls back to the reachability dot when cluster is null', () => {
    expect(platformDotClass(platform({ cluster: null, version_detected_at: '2026-08-28T10:00:00Z' })))
      .toBe(observationDotClass('observed'));
  });
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/lib/platform-observation.test.ts`
Expected: FAIL — the three functions do not exist.

- [ ] **Step 3: Implement the derivations and the types**

Add `ClusterHealth` and `NodeSummary` to `types.ts`, `cluster` to `PlatformInfo`, and map it in `platformFromDto`. Implement the three functions in `platform-observation.ts` with the module doc explaining why the three sentences are composed here rather than stored.

- [ ] **Step 4: Run to verify they pass**

Run: `npx vitest run src/lib/platform-observation.test.ts`
Expected: PASS.

- [ ] **Step 5: Build the panel and wire the three surfaces**

`ClusterHealthCard.tsx`: status badge on the palette, the composed message, `ready/total` for nodes / control-plane / workers, the namespace count (rendering `null` as "not read", never as `0`), the last-checked time, and the per-node table.

Two rules the card must follow, both from the spec:

* **On `Unreachable`, suppress the counts.** `cluster_nodes` is NULL in that state so `counts` serialises as all zeros; rendering "0/0 nodes Ready" beside "Unreachable" states a measurement never taken.
* **When `cluster` is `null`, say no check has run** — which is a different sentence from the notice being removed, which said there was no source at all.

`PlatformDetailPage.tsx`: delete the `UnavailableNotice` at :152-158 and render `<ClusterHealthCard cluster={platform.cluster} />`.

`PlatformsStrip.tsx`: dot from `platformDotClass`; the count line becomes `N healthy · N degraded · N unhealthy · N unreachable · N not yet checked`.

`PlatformsTable.tsx`: the Status column reports cluster status where present, falling back to the existing observation label.

- [ ] **Step 6: Typecheck, test, build**

```bash
npx tsc --noEmit > /tmp/tsc.log 2>&1; echo "EXIT=$?"
npx vitest run > /tmp/vt.log 2>&1; echo "EXIT=$?"
npm run build > /tmp/build.log 2>&1; echo "EXIT=$?"
```
Expected: EXIT=0 for all three. (`npm run lint` is broken repo-wide — no `eslint.config.js`, ESLint 9 dropped `.eslintrc`. Pre-existing; do not fix it here.)

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-platform-ui/src
git commit -m "feat(qa-platform-ui): render real cluster health and drive the dots from it"
```

---

## Task 7: Deploy verification

**Files:**
- Modify: `gears/qa-platform/deploy/remote/sync.sh`

**Interfaces:**
- Consumes: everything above.
- Produces: check 10.

- [ ] **Step 1: Add check 10**

Model it on the existing check 9 (search for `version_detected_at` in that file). It must assert, against the deployed stack's own database:

```sql
SELECT count(*) FROM qa_platforms WHERE cluster_status IS NOT NULL;
```

A count of zero on a **cold** start is a `NOTE`, not a `FAIL` — the ticker may not have run yet — exactly as check 9 treats its own cold-start case. A non-zero count is a `PASS` naming how many platforms carry a status.

Add a second assertion that no row has a `cluster_status_message` containing `BEGIN` or `PRIVATE KEY`. That is a cheap standing canary for D-CH-5 against the real database, and it costs one query.

- [ ] **Step 2: Verify the script still parses**

```bash
bash -n gears/qa-platform/deploy/remote/sync.sh; echo "EXIT=$?"
```
Expected: EXIT=0.

- [ ] **Step 3: Commit**

```bash
git add gears/qa-platform/deploy/remote/sync.sh
git commit -m "test(deploy): assert cluster health reached the database, and never a key"
```

---

## Verification after the final review

Not a task — the controller runs this once the final whole-branch review is clean.

```bash
bash gears/qa-platform/deploy/remote/sync.sh --argo > /tmp/deploy.log 2>&1; echo "EXIT=$?"
```

Then, against the live stack, assert the predicted values from the spec's §7 — measured read-only from the real cluster on 2026-08-28, so anything else is a defect rather than a surprise:

| field | expected |
|---|---|
| `cluster.status` | `Healthy` |
| `cluster.nodes` | one entry: `sv-vhp-jele-io`, control-plane, ready, `v1.33.4+k3s1`, `Ubuntu 24.04.3 LTS` |
| `cluster.counts` | `total 1, ready 1, control_plane 1, ready_control_plane 1, worker 0, ready_worker 0` |
| `cluster.namespace_count` | `14` |
| the other 7 platforms | `cluster: null` — they fail at kubeconfig resolution before `observe` runs |


---

## STATUS — 2026-08-29: COMPLETE, deployed and verified

**All seven tasks are implemented, reviewed and committed** on `feature/qa-platform-specs`
(`9942c10a2..e726db7e6`, 12 commits, local only — no push, no PR). Working tree clean.
Every task passed a task review plus, where findings arose, a scoped re-review. A final
whole-branch review ran on the most capable model; its one Important finding was fixed and
that fix was itself re-reviewed and cleared as ready to deploy.

| task | state |
|---|---|
| 1 pure cluster-health types | complete, review clean |
| 2 port carries two outcomes | complete, review clean |
| 3 Kubernetes adapter | complete, 1 fix round (2 Important) |
| 4 migration, entity, merge rule | complete, 1 fix round (2 Important) |
| 5 service, SDK, DTO | complete, 1 fix round (4 Minor) |
| 6 UI card and dots | complete, 1 fix round (2 Important) |
| 7 deploy check 10 | complete, review clean — no findings |

Verification at the stop point: `cargo test -p qa-environments` 165 default / 188 with
`platform-observation`; clippy clean on both feature sets; `qa-runs` and `qa-insights`
suites green; `cf-gears-example-server --features platform-observation` checks; UI
`tsc --noEmit`, `vitest` (188 tests / 12 files) and `npm run build` all clean. ADR-0001
gate holds — `cargo tree -p qa-environments -i kube` finds no `kube` in the default graph.

### The finding only a whole-branch review could have caught

**A feature-off build fabricated an `Unreachable` status.** `NoopObserver` returned
`HealthOutcome::Failed`, `record_observation` persisted any `Failed` as
`cluster_status='Unreachable'`, and the refresh endpoint is not feature-gated — so one press
of Refresh in a default build permanently marked a healthy platform Unreachable, with
nothing in that build able to clear it. Task 2 wrote the Noop contract, Task 4 the merge
rule, Task 6 the fallback; each was correct alone, and each per-task review passed. Fixed
with a third `HealthOutcome::NotAttempted` whose repository arm writes none of the five
columns, leaving the platform genuinely never-checked.

The re-reviewer proved it gone rather than reading the code: exposed the real
`cfg(not(feature))` observer, pressed Refresh three times through the service, and read the
raw row. Two mutations confirmed the probe detects rather than tautologises — and one of
them (stamping `cluster_checked_at` alone) is invisible to a `cluster.is_none()` assertion,
because `cluster_health_view` short-circuits on `status?` first.

### NEXT SESSION — resume here

1. ~~**Deploy.**~~ **DONE 2026-08-29** — `sync.sh --argo`, exit 0, 42 PASS, zero FAIL. The
   new check 10 fired on its first real run and passed both halves: one platform carries a
   non-null `cluster_status`, and no `cluster_status_message` row contains `BEGIN` or
   `PRIVATE KEY`.
2. ~~**Then regenerate the UI's OpenAPI types.**~~ **DONE 2026-08-29 (`748943d68`)** — and it
   was never blocked on the deploy: `npm run gen:api` reads `localhost:8087`, the *local*
   compose stack, not the remote. The local gears image was rebuilt, the schema regenerated,
   and the `PlatformDtoWithCluster` stopgap deleted, so `tsc` now checks the cluster shapes
   against what the gear actually serves.
3. ~~**Verify §7's predicted values.**~~ **DONE 2026-08-29 — every field matched exactly,
   first try.** `sv-test`: status `Healthy`, `status_message` null, one node
   `sv-vhp-jele-io` (control-plane, ready, `v1.33.4+k3s1`, `Ubuntu 24.04.3 LTS`), counts
   `total 1 / ready 1 / control_plane 1 / ready_control_plane 1 / worker 0 / ready_worker 0`,
   `namespace_count` 14. The other seven platforms report `cluster: null`, as predicted —
   they fail kubeconfig resolution before `observe()` runs. `kubeconfig_credstore_ref` is
   absent from the payload. The deployed UI bundle carries the card and none of the three
   stale "not available" notices.

### Carried, not fixed

* **A kubeconfig that cannot be resolved is still invisible in the UI.** Seven of the eight
  registered platforms fail at credstore resolution *before* `observe` runs, so no
  observation column is written and the reason exists only in a log line repeated every
  five minutes. Raised in the spec's §8, deliberately undecided, and untouched by this work.
* `PlatformsStrip` and `PlatformsTable` still have no render tests.
* Code coverage remains a separate design (legacy scrapes finished workflow logs; qa-runs
  serves logs only as a live stream and archives none).
