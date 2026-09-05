# Platform Observation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `qa-environments` the ability to observe a platform's cluster — version, build, namespace, base domain — store what it sees, and deliver the base URL to every run's environment.

**Architecture:** Pure parsing rules live in `domain/` behind a `PlatformObserver` port that carries no Kubernetes types; a feature-gated `infra/` adapter is the only code that builds a client. Detection runs on a supervised ticker plus an on-demand endpoint, persists through three deliberately different merge rules, and feeds `qa-runs`' existing — but until now unfed — `E2E_VHP_BASE_URL` machinery.

**Tech Stack:** Rust, sea-orm + sea-orm-migration, `kube` 0.9x + `k8s-openapi` (optional, feature-gated), axum, toolkit gear framework, React/TypeScript UI.

**Spec:** `gears/qa-platform/docs/superpowers/specs/2026-08-28-platform-observation-design.md` — read it first; this plan argues from it.

## Global Constraints

- **Toolchain:** `export PATH="$HOME/.cargo/bin:$PATH"` before any cargo command. `/usr/bin/cargo` is 1.75.0 and cannot parse this workspace.
- **Never** run `cargo test --all-targets` at workspace level (20+ minute bench run). Use `--lib --bins --tests` scoped with `-p`.
- **Never** `docker compose down -v`. Plain `down` is fine. `-v` destroys the seeded tenant resource-group row and qa-catalog's clones.
- **Never pipe a command whose exit code you need** (`cmd | tail`). The status becomes the last pipe stage's. Capture `$?` directly. This has masked a failed deploy, a failed heredoc append, and a conflicted rebase on this project.
- **`domain/` must not name a Kubernetes type.** No `kube::`, no `k8s_openapi::` outside `infra/observer/` and the feature-gated modules this plan names. ADR-0001 plus the 2026-08-28 waiver amendment (Task 1).
- **Default builds stay kube-free.** `cargo tree -p qa-environments -i kube` must error with "package not in tree" without `--features platform-observation`.
- **The kubeconfig is a client private key.** It must never reach a log line, a `Debug` output, a DTO, or an error message. `qa-environments` already has a raw-`tracing`-buffer leak test for exactly this — extend it, do not weaken it.
- **`docker compose config`** on the base compose file must keep rendering **235 lines**, sha256 `78af9c06dd2f40f424717d4fcd4179707d0ac907cc400cbf07fcc6fb0b8ee023`. That is the guard on the human partner's seeded local data.
- **A suspiciously fast test did not run.** A 0.00s result is usually a skip. Any test gated on an environment variable must print its skip loudly.
- **A `0` or a `null` is evidence only when the query is right.** This project's documented failure mode is a query written from an assumed schema whose empty answer is read as a finding. Verify each claim at the moment you write it and report the method beside it.
- **Suggested code in this plan is a hypothesis. You hold the file; you are the authority.** Roughly 22 worker amendments to controller instructions on this project have all been correct. If a step here is wrong, say so and do the right thing instead.

---

## File Structure

**Created:**

| Path | Responsibility |
|---|---|
| `qa-environments/src/domain/observation.rs` | Pure parsing rules and their result type. No I/O, no Kubernetes types. |
| `qa-environments/src/domain/ports/mod.rs` | Port module root (the gear has `repos/` but no `ports/` yet). |
| `qa-environments/src/domain/ports/platform_observer.rs` | The `PlatformObserver` trait and its no-op default. |
| `qa-environments/src/infra/observer/mod.rs` | Feature-gated adapter module root. |
| `qa-environments/src/infra/observer/kube_observer.rs` | The only code that builds a `kube::Client` and reads ConfigMaps. |
| `qa-environments/src/infra/observer/secret_writer.rs` | D4: upserts the kubeconfig `Secret` into the Argo cluster. |
| `qa-environments/src/infra/storage/migrations/m20260828_000007_platform_observation.rs` | The four new columns. |
| `qa-environments/tests/fixtures/core-install-metadata.json` | Captured verbatim from the real cluster. |
| `qa-environments/tests/fixtures/vp-gateway-hostnames.json` | Captured verbatim, including the bare-IP and `internal-only` entries. |

**Modified:**

| Path | Change |
|---|---|
| `docs/ADR/0001-cpt-cf-qa-adr-serverless-execution.md` | Waiver amendment (Task 1). |
| `qa-environments/Cargo.toml` | `platform-observation` feature, optional deps. |
| `qa-environments/src/domain/service/platforms.rs` | Observation methods; header comment's invariant narrowed. |
| `qa-environments/src/domain/repos/platforms_repo.rs` | Persistence signature for the detection outcome. |
| `qa-environments/src/infra/storage/platforms_sea_repo.rs` | The three merge rules. |
| `qa-environments/src/infra/storage/entity/platform.rs` | Four new columns. |
| `qa-environments-sdk/src/models.rs` | `TargetPlatform` gains the observed fields. |
| `qa-environments/src/api/rest/dto.rs` | `PlatformDto` gains them too. |
| `qa-environments/src/api/rest/{routes,handlers}/platforms.rs` | Refresh endpoint. |
| `qa-environments/src/gear.rs`, `src/config.rs` | Ticker and its configuration. |
| `qa-runs/src/domain/env_assembly.rs` | `VPADM_BASE_DOMAIN`, `E2E_K8S_NAMESPACE`. |
| `qa-runs/src/domain/service/dispatch_spec.rs` | Stop passing `platform_base_url: None`. |
| `qa-runs/src/domain/state_machine.rs` | Skip semantics. |
| `qa-platform-ui/src/pages/PlatformDetailPage.tsx` | Real values, refresh button, detection error. |
| `deploy/compose/docker-compose.argo.yml`, `deploy/docker/Dockerfile`, `deploy/remote/sync.sh` | Feature switches and the check that proves it live. |

---

## Task 1: ADR-0001 waiver amendment and the feature gate

**Files:**
- Modify: `docs/ADR/0001-cpt-cf-qa-adr-serverless-execution.md`
- Modify: `gears/qa-platform/qa-environments/qa-environments/Cargo.toml`

**Interfaces:**
- Consumes: nothing.
- Produces: cargo feature `platform-observation` on `qa-environments`, off by default, gating optional `kube` and `k8s-openapi`.

**Why this is first and why it is not merely paperwork.** ADR-0001's Confirmation criterion is "no `kube`/`k8s-openapi` in any qa-platform crate". The waiver of 2026-08-27 is narrow by design: it permits the dependency "inside qa-runs, behind a non-default cargo feature". `qa-environments` is outside it. The ADR itself notes the `cargo-deny` rule that would have caught this **was never implemented**, so the breach would be silent — recording it is the only thing that makes it visible.

- [ ] **Step 1: Append the amendment to ADR-0001**

Add immediately after the existing "Waiver, 2026-08-27" section:

```markdown
### Waiver amendment, 2026-08-28 (human decision): observation in qa-environments

The product owner extended the 2026-08-27 waiver to `qa-environments` on the same
narrow terms, so the platform detail page can show the version, build, namespace and
base URL it has always had fields for. Stated before the decision and on the record:
this widens the existing breach of this ADR's Confirmation criterion and of
`cpt-cf-qa-constraint-no-kube` (`DESIGN.md:180-182`) from one crate to two.

Scope, unchanged in kind from the first waiver:

* `kube`/`k8s-openapi` are **optional** and reachable only through the
  `platform-observation` cargo feature, which is **off by default**. A default build
  of every qa-platform crate still satisfies the constraint.
* **Nothing in `domain/` learns that Kubernetes exists.** The parsing rules are pure
  functions over `BTreeMap<String, String>`, reached through a `PlatformObserver` port
  that names no Kubernetes type. Only `infra/observer/` constructs a client — the same
  separation the frozen `RunExecutor` port gives `qa-runs`.
* No deployment gains a Kubernetes dependency by upgrading: with the feature off the
  gear behaves exactly as before and the observed columns stay null.

The unimplemented `cargo-deny` rule named below is still unimplemented. Until it
exists, `cargo tree -p <crate> -i kube` in CI is the only enforcement, and this plan
adds it for `qa-environments`.
```

- [ ] **Step 2: Add the feature and optional dependencies**

In `qa-environments/Cargo.toml`, add a `[features]` section (the crate has none today) modelled on `qa-runs/Cargo.toml:89`, with the reasoning inline as that file does:

```toml
[features]
default = []

# Kubernetes observation of a platform's own cluster (`infra::observer`).
#
# Off by default. ADR-0001 forbids `kube`/`k8s-openapi` in any qa-platform crate;
# the waiver amendment of 2026-08-28 permits this adapter only behind a
# non-default feature, so a default build still has no Kubernetes in its tree.
# Verify with:
#   cargo tree -p qa-environments -i kube                              # errors
#   cargo tree -p qa-environments --features platform-observation -i kube
#
# * `kube` — the API client. `client` for HTTP and `rustls-tls`/`aws-lc-rs` for
#   TLS, matching qa-runs' `argo` feature so both gears agree on one TLS stack.
#   No `runtime`: nothing here watches or streams.
# * `k8s-openapi` — `core::v1::ConfigMap` (detection) and `core::v1::Secret` (D4).
platform-observation = ["dep:kube", "dep:k8s-openapi"]
```

And in `[dependencies]`, mirroring the versions and feature lists `qa-runs/Cargo.toml:182-188` already pins:

```toml
kube = { workspace = true, features = ["client", "rustls-tls", "aws-lc-rs"], optional = true }
k8s-openapi = { workspace = true, features = ["latest"], optional = true }
```

- [ ] **Step 3: Prove the default build is kube-free**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo tree -p qa-environments -i kube; echo "DEFAULT_EXIT=$?"
cargo tree -p qa-environments --features platform-observation -i kube > /dev/null; echo "FEATURE_EXIT=$?"
```

Expected: `DEFAULT_EXIT` non-zero with "package ID specification ... did not match any packages"; `FEATURE_EXIT=0`. Capture both directly — **do not pipe to `tail`**.

- [ ] **Step 4: Commit**

```bash
git add docs/ADR/0001-cpt-cf-qa-adr-serverless-execution.md \
        gears/qa-platform/qa-environments/qa-environments/Cargo.toml
git commit -m "chore(qa-environments): extend the ADR-0001 waiver, gate the kube dependency"
```

---

## Task 2: The pure parsing rules

**Files:**
- Create: `qa-environments/src/domain/observation.rs`
- Create: `qa-environments/tests/fixtures/core-install-metadata.json`
- Create: `qa-environments/tests/fixtures/vp-gateway-hostnames.json`
- Modify: `qa-environments/src/domain/mod.rs` (add `pub mod observation;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct DetectedPlatform { pub version: String, pub build: Option<String>, pub raw: String, pub namespace: String, pub base_domain: Option<String> }`
  - `pub fn parse_platform_version(raw: &str) -> (String, Option<String>)`
  - `pub fn detected_from_data(data: &BTreeMap<String, String>, namespace: &str, fallback_ns: &str) -> Option<DetectedPlatform>`
  - `pub fn compute_base_domain(hosts: &[String]) -> Option<String>`
  - `pub fn external_hosts(data: &BTreeMap<String, String>) -> Vec<String>`

**This entire task builds and tests in a default, kube-free build.** That is the point of taking `BTreeMap<String, String>` rather than a `ConfigMap`.

- [ ] **Step 1: Capture the fixtures from the real cluster**

```bash
export KUBECONFIG=/home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/remote/vhp-kubeconfig.yaml
D=gears/qa-platform/qa-environments/qa-environments/tests/fixtures
mkdir -p "$D"
kubectl -n virtuozzo get configmap core-install-metadata -o jsonpath='{.data}' > "$D/core-install-metadata.json"; echo "CIM_EXIT=$?"
kubectl -n virtuozzo get configmap vp-gateway-hostnames  -o jsonpath='{.data}' > "$D/vp-gateway-hostnames.json";  echo "GW_EXIT=$?"
```

Both must exit 0 and be non-empty. The gateway fixture **must** retain the `internal-gateway-clusterip` entry (a bare IP, not JSON) and the `core-monitoring-query` entry (`visibility: internal-only`) — those two are the cases the code has to survive, so a "cleaned up" fixture tests nothing.

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn fixture(name: &str) -> BTreeMap<String, String> {
        let raw = std::fs::read_to_string(format!("tests/fixtures/{name}.json")).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn a_build_is_split_off_only_when_the_head_still_has_a_dot() {
        assert_eq!(parse_platform_version("26.5.0"), ("26.5".into(), Some("0".into())));
        assert_eq!(parse_platform_version("1.0"), ("1.0".into(), None));
        assert_eq!(parse_platform_version("0.1.516"), ("0.1".into(), Some("516".into())));
        // A non-numeric tail is part of the version, not a build.
        assert_eq!(parse_platform_version("1.2.rc1"), ("1.2.rc1".into(), None));
        assert_eq!(parse_platform_version("  26.5.0  "), ("26.5".into(), Some("0".into())));
    }

    #[test]
    fn the_real_clusters_metadata_parses_to_the_expected_version() {
        let d = detected_from_data(&fixture("core-install-metadata"), "virtuozzo", "unknown").unwrap();
        assert_eq!(d.version, "26.5");
        assert_eq!(d.build.as_deref(), Some("0"));
        assert_eq!(d.namespace, "virtuozzo");
    }

    #[test]
    fn metadata_without_a_platform_version_is_not_a_detection() {
        let mut data = BTreeMap::new();
        data.insert("profile".to_owned(), "dev".to_owned());
        assert!(detected_from_data(&data, "virtuozzo", "unknown").is_none());
        // Present but blank is also not a detection.
        data.insert("platformVersion".to_owned(), "   ".to_owned());
        assert!(detected_from_data(&data, "virtuozzo", "unknown").is_none());
    }

    #[test]
    fn only_external_hosts_contribute_and_the_bare_ip_entry_is_skipped() {
        let hosts = external_hosts(&fixture("vp-gateway-hostnames"));
        assert!(hosts.contains(&"api.sv.jele.io".to_owned()));
        assert!(hosts.contains(&"app.sv.jele.io".to_owned()));
        // `internal-gateway-clusterip` holds a bare IP and would fail JSON parsing.
        assert!(!hosts.iter().any(|h| h.starts_with("10.")));
        // `core-monitoring-query` is visibility: internal-only.
        assert!(!hosts.contains(&"core-monitoring-query.sv.jele.io".to_owned()));
    }

    #[test]
    fn the_real_clusters_gateway_map_yields_the_base_domain() {
        let hosts = external_hosts(&fixture("vp-gateway-hostnames"));
        assert_eq!(compute_base_domain(&hosts), Some("sv.jele.io".to_owned()));
    }

    #[test]
    fn one_host_is_inconclusive_because_vpadm_never_exposes_the_bare_domain() {
        assert_eq!(compute_base_domain(&["api.sv.jele.io".to_owned()]), None);
        // The same host twice is still one distinct host.
        assert_eq!(
            compute_base_domain(&["api.sv.jele.io".to_owned(), "api.sv.jele.io".to_owned()]),
            None
        );
    }

    #[test]
    fn a_suffix_shorter_than_two_labels_is_inconclusive_rather_than_wrong() {
        assert_eq!(
            compute_base_domain(&["a.example.com".to_owned(), "b.example.org".to_owned()]),
            None
        );
    }
}
```

- [ ] **Step 3: Run them and watch them fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-environments --lib observation:: -- --nocapture; echo "EXIT=$?"
```

Expected: compilation failure — the module does not exist yet.

- [ ] **Step 4: Implement the module**

Port `manager/src/services/platforms.rs:1268-1357` verbatim in behaviour. `compute_base_domain` is the longest common **label** suffix over distinct non-empty hosts, requiring at least two hosts and at least two remaining labels:

```rust
//! Pure detection rules, ported from `manager/src/services/platforms.rs`.
//!
//! Deliberately free of Kubernetes types: these take the ConfigMap's `data`
//! map, not a `ConfigMap`, so every rule is testable in a default build with
//! no `kube` in the tree (ADR-0001 waiver amendment, 2026-08-28).

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

/// One entry of the `vp-gateway-hostnames` ConfigMap.
#[derive(Debug, Deserialize)]
struct GatewayResolvedHost {
    host: String,
    visibility: String,
}

/// The key whose value is a bare cluster IP rather than JSON, and which must
/// therefore be skipped by name before any parse is attempted.
const GATEWAY_INTERNAL_CLUSTERIP_KEY: &str = "internal-gateway-clusterip";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedPlatform {
    pub version: String,
    pub build: Option<String>,
    pub raw: String,
    pub namespace: String,
    pub base_domain: Option<String>,
}

/// Split a raw `platformVersion` into a version prefix and a numeric build
/// suffix. Splits on the last `.` **only** when the head still contains a `.`
/// (so `1.0` is preserved whole) and the tail is entirely ASCII digits.
#[must_use]
pub fn parse_platform_version(raw: &str) -> (String, Option<String>) {
    let trimmed = raw.trim();
    if let Some((head, tail)) = trimmed.rsplit_once('.') {
        if head.contains('.') && !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            return (head.to_owned(), Some(tail.to_owned()));
        }
    }
    (trimmed.to_owned(), None)
}

/// Build a detection from a `core-install-metadata` `data` map. `None` when
/// `platformVersion` is absent or blank — which is "keep scanning", not an error.
#[must_use]
pub fn detected_from_data(
    data: &BTreeMap<String, String>,
    namespace: &str,
    fallback_ns: &str,
) -> Option<DetectedPlatform> {
    let raw = data.get("platformVersion").filter(|v| !v.trim().is_empty())?.clone();
    let namespace = if namespace.trim().is_empty() { fallback_ns } else { namespace };
    let (version, build) = parse_platform_version(&raw);
    Some(DetectedPlatform {
        version,
        build,
        raw,
        namespace: namespace.to_owned(),
        base_domain: None,
    })
}

/// Externally-visible hostnames from a `vp-gateway-hostnames` `data` map.
/// Unparseable entries are skipped rather than failing the whole read.
#[must_use]
pub fn external_hosts(data: &BTreeMap<String, String>) -> Vec<String> {
    let mut hosts = BTreeSet::new();
    for (component, raw) in data {
        if component == GATEWAY_INTERNAL_CLUSTERIP_KEY {
            continue;
        }
        match serde_json::from_str::<GatewayResolvedHost>(raw) {
            Ok(entry) if entry.visibility == "external" => {
                hosts.insert(entry.host);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(component = %component, %error, "unparseable gateway hostname entry");
            }
        }
    }
    hosts.into_iter().collect()
}

/// The domain suffix shared by every externally-visible hostname.
///
/// vpadm never exposes a component at the bare base domain — every entry
/// carries a subdomain — so the domain can only be recovered by comparing two
/// or more hostnames. `None` means **inconclusive**, never an error, and a
/// caller must not overwrite a stored value with it.
#[must_use]
pub fn compute_base_domain(hosts: &[String]) -> Option<String> {
    let distinct: BTreeSet<&str> =
        hosts.iter().map(|h| h.trim()).filter(|h| !h.is_empty()).collect();
    if distinct.len() < 2 {
        return None;
    }
    let mut iter = distinct.into_iter();
    let mut common: Vec<&str> = iter.next()?.split('.').collect();
    for host in iter {
        let labels: Vec<&str> = host.split('.').collect();
        let shared = common
            .iter()
            .rev()
            .zip(labels.iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        common = common[common.len() - shared..].to_vec();
        if common.len() < 2 {
            return None;
        }
    }
    Some(common.join("."))
}
```

- [ ] **Step 5: Run the tests and confirm they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-environments --lib observation:: -- --nocapture; echo "EXIT=$?"
```

Expected: 7 passed, `EXIT=0`. If any test reports 0.00s and "ok" without running, check it is not gated.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-environments/qa-environments/src/domain/observation.rs \
        gears/qa-platform/qa-environments/qa-environments/src/domain/mod.rs \
        gears/qa-platform/qa-environments/qa-environments/tests/fixtures/
git commit -m "feat(qa-environments): the detection rules, as pure functions over real fixtures"
```

---

## Task 3: The `PlatformObserver` port

**Files:**
- Create: `qa-environments/src/domain/ports/mod.rs`, `qa-environments/src/domain/ports/platform_observer.rs`
- Modify: `qa-environments/src/domain/mod.rs` (add `pub mod ports;`)

**Interfaces:**
- Consumes: `domain::observation::DetectedPlatform` (Task 2).
- `credstore_sdk::SecretValue`'s real surface, checked rather than assumed:
  `SecretValue::new(Vec<u8>)`, `From<Vec<u8>>`, `From<String>` (**not** `&str`),
  and `as_bytes(&self) -> &[u8]`. There is no `expose()`.
- Produces:
  - `pub enum ObservationOutcome { Detected(DetectedPlatform), Failed(String) }`
  - `#[async_trait] pub trait PlatformObserver: Send + Sync { async fn observe(&self, kubeconfig: &SecretValue, vpadm_namespace: &str) -> ObservationOutcome; async fn ensure_kubeconfig_secret(&self, credstore_ref: &str, kubeconfig: &SecretValue) -> Result<(), String>; }`
  - `pub struct NoopObserver;` implementing it.

**Why a port at all.** ADR-0001's waiver requires that `domain/` never learn Kubernetes exists. This trait is the seam: it names `SecretValue` and `String`, never `kube::Client`. It is also what makes the service testable without a cluster.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_noop_observer_reports_a_failure_rather_than_pretending() {
        let outcome = NoopObserver.observe(&SecretValue::from("irrelevant".to_owned()), "virtuozzo").await;
        match outcome {
            ObservationOutcome::Failed(message) => {
                assert!(
                    message.contains("platform-observation"),
                    "the message must name the missing feature, got: {message}"
                );
            }
            ObservationOutcome::Detected(_) => panic!("the no-op observer must never detect"),
        }
    }

    #[tokio::test]
    async fn the_noop_secret_writer_is_an_error_not_a_silent_success() {
        let result = NoopObserver
            .ensure_kubeconfig_secret("platform/x/kubeconfig", &SecretValue::from("irrelevant".to_owned()))
            .await;
        assert!(result.is_err(), "a silent Ok would recreate the FailedMount hang with no signal");
    }
}
```

- [ ] **Step 2: Run and confirm failure**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-environments --lib ports::platform_observer -- --nocapture; echo "EXIT=$?"
```

Expected: compilation failure.

- [ ] **Step 3: Implement the port**

```rust
//! The observation port. Named types are `SecretValue` and `String` — never a
//! Kubernetes type — so `domain/` satisfies ADR-0001's waiver clause that
//! nothing here learns Kubernetes exists. `infra::observer` is the only impl
//! that does.

use async_trait::async_trait;
use credstore_sdk::SecretValue;

use crate::domain::observation::DetectedPlatform;

/// The result of one observation attempt.
///
/// A failure is a **value**, not an error: legacy persists the message in
/// `platforms_meta.version_detect_error` and shows it, because an operator who
/// can see "namespace virtuozzo not found" can fix it, and one who sees a blank
/// page cannot.
#[derive(Debug, Clone)]
pub enum ObservationOutcome {
    Detected(DetectedPlatform),
    Failed(String),
}

#[async_trait]
pub trait PlatformObserver: Send + Sync {
    /// Read version, build, namespace and base domain from a platform's cluster.
    async fn observe(&self, kubeconfig: &SecretValue, vpadm_namespace: &str) -> ObservationOutcome;

    /// Decision D4: ensure the `Secret` a runner pod mounts exists and matches
    /// the material. Idempotent — safe to call on every create, update and poll.
    async fn ensure_kubeconfig_secret(
        &self,
        credstore_ref: &str,
        kubeconfig: &SecretValue,
    ) -> Result<(), String>;
}

/// The observer a build without `platform-observation` gets.
///
/// It fails loudly rather than returning a plausible empty success: a silent
/// `Ok` from `ensure_kubeconfig_secret` would put the platform straight back
/// into the five-minute `FailedMount` hang with nothing to read.
pub struct NoopObserver;

#[async_trait]
impl PlatformObserver for NoopObserver {
    async fn observe(&self, _kubeconfig: &SecretValue, _vpadm_namespace: &str) -> ObservationOutcome {
        ObservationOutcome::Failed(
            "this build has no Kubernetes client: rebuild with the `platform-observation` \
             cargo feature to observe a platform's cluster"
                .to_owned(),
        )
    }

    async fn ensure_kubeconfig_secret(
        &self,
        _credstore_ref: &str,
        _kubeconfig: &SecretValue,
    ) -> Result<(), String> {
        Err("this build has no Kubernetes client: the kubeconfig Secret must be provisioned \
             by deploy/argo/provision-platform-kubeconfig-secret.sh, or rebuild with the \
             `platform-observation` cargo feature"
            .to_owned())
    }
}
```

- [ ] **Step 4: Run and confirm they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-environments --lib ports::platform_observer -- --nocapture; echo "EXIT=$?"
```

Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-environments/qa-environments/src/domain/ports/ \
        gears/qa-platform/qa-environments/qa-environments/src/domain/mod.rs
git commit -m "feat(qa-environments): a port so the domain never learns Kubernetes exists"
```

---

## Task 4: Persistence — four columns and three merge rules

**Files:**
- Create: `qa-environments/src/infra/storage/migrations/m20260828_000007_platform_observation.rs`
- Modify: `qa-environments/src/infra/storage/migrations/mod.rs`, `entity/platform.rs`, `platforms_sea_repo.rs`, `domain/repos/platforms_repo.rs`

**Interfaces:**
- Consumes: `ObservationOutcome` (Task 3).
- Produces: `async fn record_observation(&self, ctx: &SecurityContext, platform_id: Uuid, outcome: &ObservationOutcome) -> Result<(), DomainError>` on `PlatformsRepository`.

**The three merge rules are the whole task.** They live in one statement in legacy (`platforms.rs:1189-1205`) and they differ from each other on purpose:

| column | rule |
|---|---|
| `observed_version`, `observed_build` | overwrite on success |
| `observed_namespace` | keep a set value; take detected only when unset |
| `vhp_base_url` | a successful detection wins; an inconclusive one preserves |

- [ ] **Step 1: Write the failing tests**

Follow the existing repository-test pattern in `platforms_tests.rs`. Each test asserts the rule's **difference** from its neighbours, because a test that only checks "the value was written" passes for all three rules:

```rust
#[tokio::test]
async fn a_successful_detection_overwrites_version_and_build() { /* seed 26.4/9, detect 26.5/0, expect 26.5/0 */ }

#[tokio::test]
async fn a_set_namespace_survives_detection_but_an_unset_one_is_filled() {
    // Seed observed_namespace = "custom", detect "virtuozzo", expect "custom".
    // Then seed NULL, detect "virtuozzo", expect "virtuozzo".
}

#[tokio::test]
async fn a_conclusive_base_url_wins_and_an_inconclusive_one_preserves() {
    // Seed https://old.example.com; detect base_domain Some("sv.jele.io")
    //   -> expect https://sv.jele.io
    // Seed https://sv.jele.io; detect base_domain None
    //   -> expect https://sv.jele.io STILL (this is the rule that matters:
    //      an unreadable gateway ConfigMap must not erase a known-good URL)
}

#[tokio::test]
async fn a_failed_detection_keeps_the_last_known_version_and_records_why() {
    // Seed 26.5/0; record Failed("namespaces \"virtuozzo\" not found").
    // Expect version/build UNCHANGED, version_detect_error set, version_detected_at bumped.
    // Stale-but-known beats blank.
}

#[tokio::test]
async fn a_success_clears_a_previous_error() { /* error set, then Detected, expect NULL */ }
```

- [ ] **Step 2: Run and confirm failure**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-environments --lib platforms_tests::  -- --nocapture; echo "EXIT=$?"
```

- [ ] **Step 3: Write the migration**

Four nullable columns on `qa_platforms`, following `m20260814_000006_platform_default_branch.rs` exactly for style and registration: `vhp_base_url` (text, null), `observed_namespace` (text, null), `version_detect_error` (text, null), `version_detected_at` (timestamp with time zone, null). All nullable, all additive — no backfill, no default. Register it in `migrations/mod.rs` alongside the others.

- [ ] **Step 4: Add the entity columns**

In `entity/platform.rs`, four `Option<...>` fields with doc comments that name the writer, as `observed_build`'s comment already does. Correct the now-stale claim on `observed_version`/`observed_build` that they have no writer — this task is the writer.

- [ ] **Step 5: Implement `record_observation`**

One `UPDATE`, expressing each rule explicitly rather than relying on a shared default:

```rust
// Three different merge rules in one statement, each deliberate:
//   version/build     — the cluster is authoritative, so overwrite.
//   observed_namespace— never overwrite an operator's value.
//   vhp_base_url      — a conclusive detection wins; `None` means the gateway
//                       map was missing or had too few hosts to compare, which
//                       is "inconclusive", NOT "no base URL". Overwriting with
//                       NULL there would erase a working URL every time the
//                       platform's gateway briefly failed to read.
```

Use `COALESCE(observed_namespace, $detected)` for the namespace and `COALESCE($detected_url, vhp_base_url)` for the URL — note the argument order differs between the two, and that difference *is* the behaviour.

- [ ] **Step 6: Run the tests and confirm they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-environments --lib --tests; echo "EXIT=$?"
```

Expected: all pass, including the pre-existing suite.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-environments/
git commit -m "feat(qa-environments): store what a detection saw, by three different rules"
```

---

## Task 5: The Kubernetes adapter

**Files:**
- Create: `qa-environments/src/infra/observer/mod.rs`, `qa-environments/src/infra/observer/kube_observer.rs`
- Modify: `qa-environments/src/infra/mod.rs`

**Interfaces:**
- Consumes: `PlatformObserver` (Task 3), the parsing rules (Task 2).
- Produces: `pub struct KubeObserver { argo_kubeconfig_path: Option<String>, argo_namespace: String, secret_prefix: String, secret_key: String }` with `pub fn new(...)`, implementing `PlatformObserver`.

Whole module behind `#![cfg(feature = "platform-observation")]`.

- [ ] **Step 1: Implement the detection half**

Mirror `manager/src/services/platforms.rs:1037-1090`:

1. `Kubeconfig::from_yaml(std::str::from_utf8(kubeconfig.as_bytes())?)` → `Config::from_custom_kubeconfig(kubeconfig, &Default::default()).await` → `Client::try_from`.
2. `Api::<ConfigMap>::namespaced(client.clone(), vpadm_namespace).get_opt("core-install-metadata")`.
   - `Ok(Some(cm))` → `detected_from_data(&cm.data.unwrap_or_default(), cm.metadata.namespace.as_deref().unwrap_or(""), vpadm_namespace)`; if that is `None`, fall through to the scan.
   - `Ok(None)` → fall through to the scan.
   - `Err(e)` → `ObservationOutcome::Failed(format!("Failed to read core-install-metadata in namespace {vpadm_namespace}: {e}"))`.
3. Scan: `Api::<ConfigMap>::all(client)` with `ListParams::default().labels("app.kubernetes.io/managed-by=vpadm").fields("metadata.name=core-install-metadata")`. More than one logs a warning and takes the first. Empty → `Failed("vpadm install metadata not found")`.
4. Base domain: read `vp-gateway-hostnames` in the **detected** namespace, `external_hosts` then `compute_base_domain`. Any error here logs a warning and yields `None` — never a failure, because an unreadable gateway map must not turn a good version detection into an error.

**Error strings are shown to an operator.** Keep legacy's wording; it is what makes "namespaces virtuozzo not found" actionable.

- [ ] **Step 2: Assert the kubeconfig never reaches a log**

Extend the existing raw-`tracing`-buffer leak test (the crate's `tracing-subscriber` dev-dependency exists for exactly this, and its comment explains why `tracing-test` is unusable here — a multi-line value survives capture only as its first line). Assert the buffer contains **zero** occurrences of `BEGIN` and of a canary embedded in the fixture's `client-key-data`.

Do **not** plant the canary in a comment field — session 7 planted one in an OpenSSH comment, which is base64'd inside the key body, so the check was vacuous. Put it where a leak would actually print it.

- [ ] **Step 3: Run against the real cluster**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export QA_ENV_OBSERVER_TEST_KUBECONFIG=/home/serhii/Jelastic/projects/fabric/gears-rust/gears/qa-platform/deploy/remote/vhp-kubeconfig.yaml
cargo test -p qa-environments --features platform-observation --lib observer:: -- --nocapture; echo "EXIT=$?"
```

Expected: version `26.5`, build `0`, namespace `virtuozzo`, base domain `sv.jele.io`.

**This test must print SKIPPED loudly to stderr when the variable is unset**, and CI must set it. An integration test that silently skips is this project's documented trap.

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-environments/qa-environments/src/infra/
git commit -m "feat(qa-environments): read a platform's own cluster, behind the feature"
```

---

## Task 6: D4 — materialise the kubeconfig Secret

**Files:**
- Create: `qa-environments/src/infra/observer/secret_writer.rs`
- Modify: `qa-runs/src/infra/executor/argo/naming.rs` (extend the parity oracle only)

**Interfaces:**
- Consumes: `KubeObserver`'s config (Task 5).
- Produces: `ensure_kubeconfig_secret`'s real implementation.

**This closes the worst-behaved failure in the system**: a UI-created platform sits `Pending` for ~5 minutes and then fails on `FailedMount` with no explanation, because nothing materialises the Secret and an operator must run a script by hand.

**The Secret goes into the Argo cluster, not the platform's.** Client from `argo_kubeconfig_path` when set, else `Config::infer()` — the same shape `qa-runs` uses at `argo/mod.rs:219-230`. Empty means "infer", which is correct inside the cluster, so this code is unchanged by the queued Kubernetes-deployment work.

- [ ] **Step 1: Write the failing naming test**

The name must equal what `qa-runs` mounts and what the script writes. `naming.rs:196-210` already has a parity oracle for two of those three; extend it to cover this writer so three implementations of one name cannot drift:

```rust
#[test]
fn the_writer_the_executor_and_the_script_agree_on_every_name() {
    for (reference, expected) in [
        ("argo-proof-kubeconfig", "qa-platform-argo-proof-kubeconfig"),
        ("platform/9f2c.../kubeconfig", "qa-platform-platform-9f2c----kubeconfig"),
    ] {
        assert_eq!(secret_name("qa-platform-", reference), expected);
    }
}
```

- [ ] **Step 2: Implement the upsert**

Server-side apply (`Api::<Secret>::patch` with `PatchParams::apply("qa-environments")`) so repeated calls converge rather than conflict. Data: one key, the configured `secret_key` (default `value`), holding the kubeconfig bytes. Namespace: the configured `argo_namespace` (default `argo`).

Errors return `Err(String)` describing the failure **without** the material.

- [ ] **Step 3: Call it from the service**

`PlatformsService::create` and `::update` call `ensure_kubeconfig_secret` after the credstore write succeeds, and the ticker calls it each cycle as a self-heal. A failure is logged and surfaced; it does not roll back the platform, because a platform whose row exists and whose Secret is missing is recoverable, while one that silently vanished is not.

- [ ] **Step 4: Verify on the remote, end to end**

Create a platform through the UI and watch the pod. Expected: it reaches `Running` **without** anyone running `provision-platform-kubeconfig-secret.sh`. Record `kubectl -n argo get secret <name>` before and after, and the pod's phase transitions with timestamps.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-environments/ gears/qa-platform/qa-runs/qa-runs/src/infra/executor/argo/naming.rs
git commit -m "feat(qa-environments): materialise the runner's kubeconfig Secret, closing D4"
```

---

## Task 7: Service, SDK model, DTO, and the refresh endpoint

**Files:**
- Modify: `qa-environments-sdk/src/models.rs`, `qa-environments/src/api/rest/dto.rs`, `src/domain/service/platforms.rs`, `src/api/rest/{routes,handlers}/platforms.rs`

**Interfaces:**
- Consumes: the port (Task 3), persistence (Task 4).
- Produces:
  - `TargetPlatform` and `PlatformDto` gain `vhp_base_url: Option<String>`, `observed_namespace: Option<String>`, `version_detect_error: Option<String>`, `version_detected_at: Option<OffsetDateTime>`.
  - `PlatformsService::observe_platform(&self, ctx, platform_id) -> Result<TargetPlatform, DomainError>`.
  - `POST /qa/v1/platforms/{id}/refresh`.

**`kubeconfig_credstore_ref` stays off the DTO.** Its doc comment explains why at length: under `SharingMode::Tenant` the reference *is* a read path to the material. Do not add it back. The four new fields are all non-secret.

- [ ] **Step 1: Narrow the module's stated invariant, in the same commit as the code that narrows it**

`domain/service/platforms.rs`'s header says the material is such that "nothing on any read path ever returns it". After this task the material is read in-process for the first time. Rewrite that paragraph to state the invariant that is actually true — **the material never leaves the gear** — and keep the three mechanisms that enforce it (redacted `Debug`, `#[instrument]` skips, references at the repository boundary), extending them to the new path. A doc comment that no longer describes the code is a defect by this project's standing rule.

- [ ] **Step 2: Write the failing endpoint test**

```rust
#[tokio::test]
async fn a_detection_failure_is_a_200_carrying_the_reason_not_a_5xx() {
    // With an observer that returns Failed("namespaces \"virtuozzo\" not found"):
    // expect HTTP 200, body.version_detect_error == that message,
    // and body.observed_version unchanged from its seeded value.
    //
    // 5xx is reserved for a genuine fault in THIS service. A platform whose
    // cluster is unreachable is a fact about the platform, and the operator
    // needs to read it. Matches legacy `routes/platforms.rs:363`.
}

#[tokio::test]
async fn refreshing_an_unknown_platform_is_404() { /* ... */ }
```

- [ ] **Step 3: Run and confirm failure, implement, run again**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-environments --lib --tests; echo "EXIT=$?"
```

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-environments/
git commit -m "feat(qa-environments): serve what was observed, and a way to ask again"
```

---

## Task 8: The refresh ticker

**Files:**
- Modify: `qa-environments/src/gear.rs`, `qa-environments/src/config.rs`

**Interfaces:**
- Consumes: `observe_platform` (Task 7).
- Produces: a supervised ticker; config `qa-environments.observation.{enabled, poll_interval_seconds}`.

**Copy the established shape, do not invent one.** `qa-runs/src/gear.rs:504` and `qa-insights/src/gear.rs:925` both spawn tickers through the same `Tickers`/`supervise` helpers, and `gear.rs:714-730` explains why the role must come from the task id rather than the future's output. Follow it.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_interval_has_a_sixty_second_floor() {
    // Legacy clamps with .max(60) in both branches of its config load
    // (platform_version_poller.rs). A 1-second poll would hammer every
    // registered cluster.
    assert_eq!(resolve_interval(0), Duration::from_secs(60));
    assert_eq!(resolve_interval(30), Duration::from_secs(60));
    assert_eq!(resolve_interval(300), Duration::from_secs(300));
}

#[tokio::test]
async fn one_platforms_failure_does_not_abort_the_cycle() {
    // Three platforms, the middle one's observer returns Failed.
    // Expect all three recorded: the outer two Detected, the middle one's error stored.
}
```

- [ ] **Step 2: Implement**

Per cycle: list platforms, observe each, record the outcome, and call `ensure_kubeconfig_secret` as a self-heal. Wrap each platform's work so a failure is logged and the loop continues. When the `platform-observation` feature is off, the ticker is not spawned at all — log once at startup saying so, so a deployment missing the feature is legible rather than silently inert.

- [ ] **Step 3: Run tests, then confirm the boot log on the local stack**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-environments --lib --tests; echo "EXIT=$?"
```

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-environments/
git commit -m "feat(qa-environments): keep observations fresh without anyone clicking"
```

---

## Task 9: Deliver the base URL to runs

**Files:**
- Modify: `qa-runs/src/domain/env_assembly.rs`, `qa-runs/src/domain/service/dispatch_spec.rs`

**Interfaces:**
- Consumes: `TargetPlatform`'s new fields (Task 7).
- Produces: `E2E_VHP_BASE_URL`, `VPADM_BASE_DOMAIN`, `E2E_K8S_NAMESPACE` in a run's environment.

**Most of this already exists.** `env_assembly.rs:236-294` implements `E2E_VHP_BASE_URL` and legacy's "fifth position" override ordering, with tests at `:445-479`. It has been fed `None` at `dispatch_spec.rs:559` since it was written, for want of a source. Detection is that source.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_base_domain_accompanies_the_base_url_and_shares_its_precedence() {
    let mut i = inputs();
    i.platform_base_url = Some("https://sv.jele.io".to_owned());
    let env = assemble(i);
    assert_eq!(env.get("E2E_VHP_BASE_URL").map(String::as_str), Some("https://sv.jele.io"));
    assert_eq!(env.get("VPADM_BASE_DOMAIN").map(String::as_str), Some("sv.jele.io"));
}

#[test]
fn a_scheme_less_base_url_still_yields_a_bare_domain() {
    // legacy's base_domain_from_url prepends https:// when there is no "://"
    let mut i = inputs();
    i.platform_base_url = Some("sv.jele.io".to_owned());
    assert_eq!(assemble(i).get("VPADM_BASE_DOMAIN").map(String::as_str), Some("sv.jele.io"));
}

#[test]
fn an_unparseable_base_url_yields_the_url_without_a_domain_rather_than_failing() {
    let mut i = inputs();
    i.platform_base_url = Some("::::".to_owned());
    let env = assemble(i);
    assert_eq!(env.get("E2E_VHP_BASE_URL").map(String::as_str), Some("::::"));
    assert!(!env.contains_key("VPADM_BASE_DOMAIN"));
}

#[test]
fn a_blank_base_url_contributes_neither_variable() {
    let mut i = inputs();
    i.platform_base_url = Some("   ".to_owned());
    let env = assemble(i);
    assert!(!env.contains_key("E2E_VHP_BASE_URL"));
    assert!(!env.contains_key("VPADM_BASE_DOMAIN"));
}

#[test]
fn the_observed_namespace_becomes_e2e_k8s_namespace() {
    let mut i = inputs();
    i.platform_namespace = Some("virtuozzo".to_owned());
    assert_eq!(assemble(i).get("E2E_K8S_NAMESPACE").map(String::as_str), Some("virtuozzo"));
}
```

- [ ] **Step 2: Run, confirm failure, implement**

Add `platform_namespace: Option<String>` to `EnvInputs` beside `platform_base_url`, and derive the domain in the same tier with the same dedupe. `dispatch_spec.rs:559` stops passing `None` and reads the platform it already fetches for the kubeconfig mount at `:511-518` — no extra lookup.

Update `dispatch_spec.rs:484`'s "Four things the source system sets and this cannot yet" comment: `E2E_K8S_NAMESPACE` now has a source and moves off that list. Leave the others.

- [ ] **Step 3: Run and confirm they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs --lib --tests; echo "EXIT=$?"
```

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-runs/
git commit -m "feat(qa-runs): give the suite the base URL it has always expected"
```

---

## Task 10: Skip semantics

**Files:**
- Modify: `qa-runs/src/domain/state_machine.rs:395`, plus the run list and detail UI

**This is a deliberate divergence from the benchmark, ratified by the human partner on 2026-08-28.** Legacy is explicit and considered — `manager/src/services/argo.rs:2197`: "a skipped test means the run didn't fully execute, so it must never read as passing either" — and our line is a faithful port of it. The reason for diverging: the human partner's suite is full of gated skips, so the rule marks nearly every real run red and destroys the signal it exists to give.

**Do not stop at the one-line change.** Trading a false red for a false green is no improvement. The skip count must be visible enough that "succeeded, 68 skipped" cannot be mistaken for a clean run.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_skip_no_longer_fails_a_run_but_a_failure_still_does() {
    // Ratified divergence from legacy (argo.rs:2197), human decision 2026-08-28.
    assert_eq!(derive_terminal_state(Succeeded, counts(10, 0, 5), None), RunState::Succeeded);
    assert_eq!(derive_terminal_state(Succeeded, counts(10, 1, 5), None), RunState::Failed);
    // Everything skipped is still not a failure — it is a run that asserted nothing,
    // which the skip count must make visible rather than the state hiding it.
    assert_eq!(derive_terminal_state(Succeeded, counts(0, 0, 68), None), RunState::Succeeded);
    // A node that died without emitting results still fails the run.
    assert_eq!(derive_terminal_state(Succeeded, counts(10, 0, 5), Some(true)), RunState::Failed);
}
```

- [ ] **Step 2: Change the line**

```rust
// A skipped test no longer fails the run (human decision, 2026-08-28), which
// diverges deliberately from `manager/src/services/argo.rs:2197`. The suites
// this platform runs are full of gated skips, so the old rule reported almost
// every real run as failed and the colour stopped carrying information. The
// skip count is surfaced in the list and detail views instead, so a run that
// asserted nothing is still legible.
let results_say_no = counts.failed > 0;
```

Update — do not delete — the existing tests that pinned the old behaviour, recording the decision in each.

- [ ] **Step 3: Surface the skip count in the UI**

Run list and run detail show skipped alongside passed/failed, with a visible marker on any succeeded run whose skip count is non-zero.

- [ ] **Step 4: Run tests, then commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p qa-runs --lib --tests; echo "EXIT=$?"
git add gears/qa-platform/qa-runs/ gears/qa-platform/qa-platform-ui/
git commit -m "fix(qa-runs): a skipped test is not a failed run, and the count says so"
```

---

## Task 11: The platform detail page

**Files:**
- Modify: `qa-platform-ui/src/pages/PlatformDetailPage.tsx`, and the API types it reads

**Three separate wrongs to fix, and the second is the worst:**

1. `:131` renders an `UnavailableNotice` because nothing ever filled the fields. Now they are filled.
2. `:225` renders `platform.namespace || 'vhp-platform (default)'` against a DTO that has **no** `namespace` field — so the left operand is permanently `undefined` and the page has always displayed a default while presenting it as fact. Bind it to `observed_namespace` and show nothing rather than a fiction when it is null.
3. "VHP Base URL" had no field behind it at all. Now it has `vhp_base_url`.

- [ ] **Step 1: Add the fields to the API types**

Four fields on the platform type, matching the DTO from Task 7 exactly. A name mismatch here fails silently as `undefined`, which is what wrong #2 already is.

- [ ] **Step 2: Render observation, including its failure**

Show version, build, namespace and base URL when present. When `version_detect_error` is set, show the message — legacy shows it rather than swallowing it, and "namespaces virtuozzo not found" is what an operator can act on. Show `version_detected_at` so a stale value is legible as stale.

- [ ] **Step 3: Add the Refresh button**

Calls `POST /qa/v1/platforms/{id}/refresh`, disabled while in flight, and renders the returned row — including a returned `version_detect_error`. **Do not report success on HTTP 200 alone**: this is exactly the "lying toast" defect fixed in `89b55eb22`, where `ProductDetailPage.tsx:185` showed a green toast beside a red failure badge because the gear answers 200 with the error recorded in the row.

- [ ] **Step 4: Verify in a browser and commit**

Check the built bundle hash actually changed — a UI claim without a new bundle name is unproven.

```bash
git add gears/qa-platform/qa-platform-ui/
git commit -m "feat(ui): show what the platform actually is, including why it could not be read"
```

---

## Task 12: Deploy switches, the guard, and live verification

**Files:**
- Modify: `deploy/docker/Dockerfile`, `deploy/compose/docker-compose.argo.yml`, `deploy/remote/sync.sh`

**Ruling 23 is the trap.** The effective feature list lives in **two** places and `docker-compose.argo.yml:66` carries its own hard-coded copy — the copy that governs every `--argo` deploy. Flipping only the Dockerfile default produces a remote build silently missing the feature. That file's own comment names this drift risk.

- [ ] **Step 1: Add `platform-observation` to both feature lists**

The Dockerfile `ARG CARGO_FEATURES` default, and `docker-compose.argo.yml:66`'s copy. Both, in the same commit.

- [ ] **Step 2: Add a `sync.sh` check that proves it is live**

Model it on check 8, which reads both what was *selected* and what actually *exists*, because either alone is ambiguous. Here: the rendered config requests observation **and** a platform row has a non-null `version_detected_at`, proving the ticker ran against a real cluster rather than merely being configured.

- [ ] **Step 3: Confirm the compose guard still holds**

```bash
cd gears/qa-platform/deploy/compose
docker compose config > /tmp/rendered.yaml; echo "EXIT=$?"
wc -l < /tmp/rendered.yaml          # must be 235
sha256sum /tmp/rendered.yaml        # must be 78af9c06dd2f40f424717d4fcd4179707d0ac907cc400cbf07fcc6fb0b8ee023
```

Neither switch is a compose input on the default path, so this must be unchanged. If it moved, stop and report — that guard protects the human partner's seeded local data.

- [ ] **Step 4: Deploy and verify, per item, with the method beside each claim**

```bash
gears/qa-platform/deploy/remote/sync.sh --argo > /tmp/deploy.log 2>&1; echo "DEPLOY_EXIT=$?"
```

Capture the status **directly**. A `| tail` here masked a failed deploy in session 6.

Then verify each of these separately, naming the query used:

| claim | expected |
|---|---|
| `observed_version` / `observed_build` | `26.5` / `0` |
| `observed_namespace` | `virtuozzo` |
| `vhp_base_url` | `https://sv.jele.io` |
| a run's env carries `E2E_VHP_BASE_URL` | `https://sv.jele.io` |
| a run's env carries `VPADM_BASE_DOMAIN` | `sv.jele.io` |
| a UI-created platform's pod | reaches `Running` with **no** manual script |

Reminders that have each cost this project a session: `test-case-results` wants an OData `$filter` with the uuid **bare** and strings quoted, not `?run_id=`; the run counts are nested under `result`, not top-level; the SSH-key table is `qa_ssh_keys`; and results ingest with a lag, so a `0` queried the instant a run finishes means "too early", not "none".

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/deploy/
git commit -m "feat(deploy): select platform observation, and fail the deploy if it is not live"
```

---

## Out of scope, deliberately

- **Kubernetes deployment of qa-platform into the remote k3s** — requested 2026-08-28, queued as the next piece with its own brainstorm. Task 6's config shape (`argo_kubeconfig_path` empty ⇒ `Config::infer()`) is chosen so it lands unchanged.
- **Slack bot-token parity** (`1c622f9`) — a different gear, nothing here depends on it.
- **Anything from `origin/VHP-1655-YZ`** — excluded by name; it is divergent rather than newer.
- **Durable run logs** — still open, still needs the `log_storage_ref` path a comment claimed existed.

---

## STATUS — 2026-08-28, all 12 tasks executed

**All twelve tasks are implemented, reviewed, and committed** on `feature/qa-platform-specs`
(`fef331758..c5780190f`, 24 commits, local only — no push, no PR). Every task passed a two-stage
gate: a task review plus, where findings arose, a scoped re-review. A final whole-branch review ran
on the most capable model, and its fix wave has itself been re-reviewed and cleared as merge-ready.

| task | state |
|---|---|
| 1 ADR waiver + feature gate | complete, review clean |
| 2 pure detection rules | complete, 1 fix round |
| 3 `PlatformObserver` port | complete, review clean |
| 4 persistence, three merge rules | complete, review clean |
| 5 Kubernetes adapter | complete, review clean |
| 6 D4 Secret writer | complete, review clean |
| 7 service / SDK / DTO / refresh endpoint | complete, 1 fix round (authz GET→UPDATE) |
| 8 observation ticker | complete, 1 fix round (ratified `allow_all` exception + guard test) |
| 9 run environment variables | complete, review clean |
| 10 skip semantics | complete, 1 fix round (marker was dead code) |
| 11 platform detail page | complete, review clean |
| 12 deploy switches + check 9 | complete, review clean |

### The three findings that mattered most, none of which a green suite would have shown

1. **A kubeconfig leak onto the platform page (Critical).** A YAML parse error's `Display` reached
   `version_detect_error`, which is persisted, published on `PlatformDto` to every
   `qa.platform` GET/LIST-authorized caller, and rendered in the UI. Serde quotes the offending
   scalar, so pasting a **private key** into the kubeconfig field echoed the key body back. Fixed by
   classifying every kubeconfig-derived error to a fixed `&'static str` — a type-level guarantee
   rather than a filter. The audit found **three further leaks** nobody had named. It had survived
   six reviews because the leak canary used a *well-formed* fixture, so the parse-error branch was
   never under test.
2. **D4 could not have worked in the deployment task 12 enables.** The Argo client fell back to
   `Config::infer()` with no kubeconfig path and no `KUBECONFIG`; every Secret write would have
   failed, and check 9 verifies observation rather than the Secret, so it would not have caught it.
3. **D4 was half-wired** — the Secret writer had one caller (the ticker), so a UI-created platform
   still waited out the ~5-minute `FailedMount` window D4 exists to close.

### Separate change, same session

**The UI phase-casing defect** (`7436ad6a6`): the gear emits lowercase run states and `adapters.ts`
passes them through deliberately, but five UI sites compared against Title-Case. **The Runs page
status filter matched zero rows for every chip.** Fixed against the value set enumerated from
`RunState::as_str()`. Kept deliberately out of the feature branch's task history.

### Follow-up round, same session — UI surfaces the observation work left stale

Three defects reported from the running deployment, each fixed as its own commit. Suite: 163 tests
pass, `tsc --noEmit` and `vite build` clean. `npm run lint` fails to start — the repo has no
`eslint.config.js` and ESLint 9 dropped `.eslintrc` — which is pre-existing and untouched.

1. **The dashboard did not support platforms** (`59bec52d0`). Three surfaces still asserted that
   nothing observes a platform, which stopped being true when task 4 landed: the dashboard's
   Platforms card was a labelled-unavailable notice; the platforms page claimed no version is
   detected while the table below it already rendered one; and the table's Status column had been
   removed because the two columns it read were ones nothing set. The card now renders legacy's
   chip-per-platform (dot, name, version) and the Status column is back. The dot is **reachability**,
   not legacy's node health — that distinction is stated in both notices and in
   `lib/platform-observation.ts`.
2. **Schedules showed a plan's raw id** (`26b1d4f41`). `ScheduleDto` carries no plan name, so
   `scheduleFromDto` sets `plan_name: null` and every row fell through to the opaque
   `{repo_uuid}--{base64(path)}`. Now joined client-side against product-unscoped plan and
   custom-plan listings, mirroring `manager/src/routes/schedules.rs:227-229`.
3. **A blank white square on the dashboard chart** (`ffdd0f08a`). `tooltip.custom` returned HTML
   with no colours, and ApexCharts' `.apexcharts-theme-light` sets a white background and no
   `color` — so in dark mode the text inherited the page's near-white foreground. All four charts
   were theme-blind; `lib/chart-theme.ts` makes them theme-aware via `theme.mode`.

**Still not observed, and the obvious next ask:** legacy's Healthy/Degraded/Unhealthy come from
listing the cluster's nodes and counting ready ones (`manager/src/services/platforms.rs:140-200`),
plus a namespace count. `PlatformObserver` already holds a live `kube::Client`, so this is an
extension of the existing observe call rather than new plumbing — new columns, four DTO fields, and
the dashboard aggregate. Not started; needs its own design pass because it widens the observation
surface and every new error string is another leak candidate (see finding 1 above).

### Open, and deliberately not decided here

* `RunsPage` still offers `Pending` and `Skipped` chips that this gear's `RunState` can never
  produce, so they always return zero rows. Removing a UI affordance is a product decision.
* `canceled` / `timed_out` / `expired` have no dedicated badge styling and fall through to default.
* Nothing deletes the old Argo Secret on kubeconfig rotation or platform delete.
* `qa-environments` has no leader election, unlike its sibling gears. Assessed as wasteful rather
  than harmful under multi-replica: every write is idempotent and cluster reads are pure GETs.
* Kubernetes deployment of qa-platform into the remote k3s — requested this session, queued as the
  next piece with its own brainstorm. Task 6's config shape (`kubeconfig_path` empty ⇒
  `Config::infer()`) was chosen so it lands unchanged.
