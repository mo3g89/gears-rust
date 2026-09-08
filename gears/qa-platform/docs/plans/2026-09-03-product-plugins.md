# QA Platform Product Plugins Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a product's behaviour an in-process Rust plugin resolved per product, so the QA platform can onboard a non-Kubernetes product without editing `qa-environments`, `qa-runs` or the UI.

**Architecture:** A new `qa-product-sdk` crate defines one trait, `QaProductPluginV1`. Plugin gears register a GTS instance and a `ClientHub`-scoped implementation exactly as `postgres-credstore-plugin` does; `qa-catalog` owns the single resolver (`QaProductRegistry`), keyed on `qa_products.plugin_instance_id`. `qa-environments` loses every Kubernetes type — the k8s machinery moves to a `qa-connector-k8s` library crate that only product plugins link. Observation becomes descriptor-driven with four semantic **role projections** into real columns, so the UI, `qa-insights` and `qa-runs`' `APP_VERSION`/`APP_BUILD` survive unchanged. All four QA gears share one process and one `ClientHub`, so one registration serves every consumer.

**Tech Stack:** Rust 2024 (workspace MSRV 1.95.0; `rust-toolchain.toml` pins 1.97.0), ToolKit stack (`toolkit`, `toolkit-gts`, `toolkit-db` SecureORM, `toolkit-security`), SeaORM + `sea_orm_migration`, `types-registry-sdk` for GTS registration, `async-trait`, `cargo test`, React + TypeScript + Vitest for the UI.

**Spec:** `gears/qa-platform/docs/PRODUCT-PLUGINS-DESIGN.md` — read it before Task 1. Decisions are cited below as **D1**–**D12** and refer to that document's §3.

---

## Global Constraints

Copied verbatim from the spec. Every task's requirements implicitly include this section.

- **ADR-0001, as amended by this work.** No qa-platform crate may depend on `kube` or `k8s-openapi` **except** `qa-connector-k8s` and the product plugins that link it. After Task 19 the `platform-observation` Cargo feature does not exist. Verify with `cargo tree -p qa-environments -i kube` returning "package ID not found".
- **No credential-derived value is ever formatted.** Not `Display`, not `Debug`, not into a message, log line or DTO. Failures are *classified* by variant and carry a fixed `&'static str`. This is the rule `qa-environments/src/infra/observer/errors.rs` exists to enforce, after a measured leak on 2026-08-28 put a PEM private key on the platform page. (**D12**)
- `**observed_schema()` may not declare `FieldKind::Secret` or `FieldKind::MultilineSecret`.** Violation is a boot failure, not a render-time leak.
- **At most one `FieldDesc` may claim each `FieldRole`.** Violation is a boot failure. Ambiguity here makes `APP_VERSION` non-deterministic.
- **The precedence ladder stays platform-owned.** A plugin supplies names and values; it never reorders tiers. `RESERVED_NAMES` is the platform floor **unioned** with the plugin's set, never replaced. (**D8**)
- **An environment belongs to exactly one product.** (**D9**)
- **Every product names a plugin. There is no fallback path.** (**D6**)
- **Legacy citations in doc comments must survive the rename untouched.** `platforms_meta`, `manager/src/services/platforms.rs`, `manager/src/services/argo.rs` and every line reference into them are references to the source system, not to this code. `doc_citations_tests.rs` is the guard.
- **Every commit builds and every commit's tests pass.** `cargo test --workspace` is the gate (`cargo nextest` is not installed; see Verification commands). This is why the schema change is expand/contract rather than a single flip.
- **Crate naming:** implementation and SDK crates under `gears/qa-platform/` use bare names (`qa-catalog-sdk`), `publish = false`, `version = "0.1.0"`, and inherit `edition`/`license`/`authors` from the workspace. Plugin crates outside qa-platform use the `cf-gears-` prefix; qa-platform plugins follow the local bare-name convention.

---

## File structure

### New crates


| Path                                               | Responsibility                                                                                                                                                                                                                                                                        |
| -------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `gears/qa-platform/qa-product-sdk/`                | The trait, the descriptor types, the failure type, the role projection, the leak-conformance harness, the GTS spec. Depends on nothing in qa-platform.                                                                                                                                |
| `gears/qa-platform/connectors/qa-connector-k8s/`         | Library crate. Kubeconfig parsing, client construction, ConfigMap reads, node/namespace health, Secret provisioning, and `errors.rs`' classification table. The **only** qa-platform crate that names Kubernetes types. Not a gear — no `#[toolkit::gear]`, no registration. (**D7**) |
| `gears/qa-platform/plugins/qa-vhp-product-plugin/` | Gear crate. VHP's install topology, env-var naming and runner shape. Registers `cf.core._.vhp_product.v1`.                                                                                                                                                                            |


### Modified, by phase


| Phase | Files                                                                                                                                                                               |
| ----- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| B     | `qa-runs/qa-runs/src/domain/env_assembly.rs` → `runvars.rs`; `qa-environments-sdk/src/models.rs`; every file naming `TargetPlatform`/`platform_id` across four gears and the UI     |
| D     | `qa-catalog/qa-catalog/src/domain/service/products.rs`, new `domain/service/plugin_registry.rs`, new migration; `qa-environments/src/domain/service/environments.rs`, new migration |
| E     | `qa-runs/qa-runs/src/domain/ports/run_executor.rs`, `domain/service/dispatch_spec.rs`, `infra/executor/argo/workflow.rs`                                                            |
| F     | delete `qa-environments/qa-environments/src/infra/observer/` (whole directory); `qa-environments/Cargo.toml`; `apps/cf-gears-example-server/Cargo.toml`                             |
| G     | `qa-platform-ui/src/components/platforms/`, `src/pages/PlatformDetailPage.tsx`, `src/api/adapters.ts`, `src/lib/selected*.ts`                                                       |


---

## Phases

Eight phases, twenty-five tasks (Tasks C0 and 9b were added during execution, see Phase C; Task 18b was added at Phase F open by user decision U7, see Phase F). Phases A–F each end on a green `cargo test --workspace`.


| Phase              | Tasks    | Deliverable                                                                                | Reversible?           |
| ------------------ | -------- | ------------------------------------------------------------------------------------------ | --------------------- |
| **A — Foundation** | 1–4      | `qa-product-sdk` compiles and is fully tested; nothing consumes it                         | yes                   |
| **B — The rename** | 5–7      | `TargetPlatform` → `Environment` everywhere; zero behaviour change                         | yes                   |
| **C — Plugins**    | C0, 8, 9, 9b, 10, 11 | `qa-connector-k8s` + `qa-vhp-product-plugin` build and register; still unresolved by anything | yes                   |
| **D — Expand**     | 12–15    | New columns and the registry exist alongside the old ones; observation dual-writes         | yes                   |
| **E — qa-runs**    | 16–18    | Golden `RunSpec` test, then dispatch through the plugin                                    | yes                   |
| **F — Contract**   | 19–20    | Old columns dropped, `infra/observer/` deleted                                             | **NO — one-way door** |
| **G — UI**         | 21–22    | Descriptor-driven tables and forms                                                         | yes                   |


**Task 16 is a hard gate.** The golden `RunSpec` test must exist and pass *before* Task 18 changes `build_spec`, and it is the only thing that makes Phase F safe. Do not reorder it.

---

## Verification commands

Used throughout; run from the repository root.

> **Corrected 2026-09-03, during Phase A execution.** This section originally
> named `cargo nextest`, copied from the Makefile's `test` target. Two things
> made that wrong on this machine, and both cost a fix round before they were
> caught:
>
> - `**cargo nextest` is not installed.** The Makefile's `test` target depends
> on `install-tools`, which installs it; invoking `cargo nextest` directly
> without that step fails. Use `cargo test` for per-task verification, or run
> `make test` if you want the Makefile's full path.
> - **The project toolchain is not on the default PATH.** `/usr/bin/cargo` is
> 1.75.0 and cannot build edition 2024. The toolchain `rust-toolchain.toml`
> pins (1.97.0) is installed under `~/.rustup` and reached through
> `~/.cargo/bin`. **Export the PATH first, and confirm the version, before
> trusting any result** — a shell without it fails in a way that looks like a
> code error.

```bash
export PATH="$HOME/.cargo/bin:$PATH"   # FIRST. Then confirm:
rustc --version                        # must print 1.97.0, not 1.75.0

cargo test -p qa-product-sdk           # one crate — the per-task gate
cargo test -p qa-environments          # one gear
cargo test --workspace                 # the full gate
cargo clippy -p <crate> --all-targets --all-features   # deny-level lint set
cargo fmt -p <crate>                   # NOT --fmt all: qa-catalog/.../bundles.rs
                                       # carries a pre-existing formatting diff
cargo tree -p qa-environments -i kube  # ADR-0001 containment check
make ui-test                           # UI (vitest)
```

---

# Phase A — Foundation

Nothing in this phase is consumed by any gear. It ends with a crate that compiles, is fully tested, and can be deleted without trace if the design changes.

### Task 1: `qa-product-sdk` crate with the descriptor types

**Files:**

- Create: `gears/qa-platform/qa-product-sdk/Cargo.toml`
- Create: `gears/qa-platform/qa-product-sdk/src/lib.rs`
- Create: `gears/qa-platform/qa-product-sdk/src/descriptor.rs`
- Create: `gears/qa-platform/qa-product-sdk/src/descriptor_tests.rs`
- Modify: `Cargo.toml:62` (workspace `members`, beside the other qa-platform entries)

**Interfaces:**

- Produces: `FieldDesc`, `FieldKind`, `FieldRole`, `SchemaError`, `validate_schemas(credential: &[FieldDesc], observed: &[FieldDesc]) -> Result<(), SchemaError>`. Task 4's registration path and Task 15's role projection both consume these.

- [ ] **Step 1: Create the crate manifest**

```toml
# gears/qa-platform/qa-product-sdk/Cargo.toml
[package]
name = "qa-product-sdk"
version = "0.1.0"
publish = false
edition.workspace = true
license.workspace = true
authors.workspace = true
description = "Contract every QA Platform product plugin implements: descriptors, observation, run access, and the leak-conformance harness"

[lints]
workspace = true

[dependencies]
async-trait = { workspace = true }
credstore-sdk = { package = "cf-gears-credstore-sdk", version = "0.2.4", path = "../../credstore/credstore-sdk" }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
gts = { workspace = true }
toolkit-gts = { workspace = true }
uuid = { workspace = true }
```

Add `"gears/qa-platform/qa-product-sdk",` to the workspace `members` list in the root `Cargo.toml`, immediately before `"gears/qa-platform/qa-environments/qa-environments-sdk",`.

- [ ] **Step 2: Write the failing tests**

```rust
// gears/qa-platform/qa-product-sdk/src/descriptor_tests.rs
use super::*;

fn f(key: &str, kind: FieldKind, role: Option<FieldRole>) -> FieldDesc {
    FieldDesc {
        key: key.to_owned(),
        label: key.to_owned(),
        kind,
        required: false,
        role,
        in_table: false,
        in_detail: true,
        help: None,
    }
}

#[test]
fn a_schema_with_no_roles_and_no_secrets_is_valid() {
    let creds = vec![f("kubeconfig", FieldKind::MultilineSecret, None)];
    let observed = vec![f("nodes", FieldKind::Int, None)];
    assert!(validate_schemas(&creds, &observed).is_ok());
}

#[test]
fn credential_schema_may_declare_secrets() {
    let creds = vec![
        f("token", FieldKind::Secret, None),
        f("kubeconfig", FieldKind::MultilineSecret, None),
    ];
    assert!(validate_schemas(&creds, &[]).is_ok());
}

/// The rule that makes `observed_attrs` structurally safe to render. Without
/// it a plugin could put credential material on the environment page, which is
/// the 2026-08-28 leak reintroduced through a new door.
#[test]
fn observed_schema_may_not_declare_a_secret() {
    let observed = vec![f("kubeconfig_echo", FieldKind::Secret, None)];
    let err = validate_schemas(&[], &observed).unwrap_err();
    assert!(
        matches!(&err, SchemaError::SecretInObservedSchema { key } if key == "kubeconfig_echo"),
        "got {err:?}"
    );
}

#[test]
fn observed_schema_may_not_declare_a_multiline_secret() {
    let observed = vec![f("dump", FieldKind::MultilineSecret, None)];
    assert!(matches!(
        validate_schemas(&[], &observed).unwrap_err(),
        SchemaError::SecretInObservedSchema { .. }
    ));
}

/// Two fields claiming `Version` would make `APP_VERSION` depend on iteration
/// order, so it is rejected at registration rather than resolved arbitrarily.
#[test]
fn two_fields_may_not_claim_the_same_role() {
    let observed = vec![
        f("platformVersion", FieldKind::Text, Some(FieldRole::Version)),
        f("coreVersion", FieldKind::Text, Some(FieldRole::Version)),
    ];
    let err = validate_schemas(&[], &observed).unwrap_err();
    assert!(
        matches!(&err, SchemaError::DuplicateRole { role: FieldRole::Version, .. }),
        "got {err:?}"
    );
}

#[test]
fn distinct_roles_coexist() {
    let observed = vec![
        f("platformVersion", FieldKind::Text, Some(FieldRole::Version)),
        f("build", FieldKind::Text, Some(FieldRole::Build)),
        f("baseUrl", FieldKind::Url, Some(FieldRole::BaseUrl)),
        f("namespace", FieldKind::Text, Some(FieldRole::Namespace)),
    ];
    assert!(validate_schemas(&[], &observed).is_ok());
}

/// Roles are a property of what was *observed*, so a credential field claiming
/// one is a mistake the author should hear about at boot.
#[test]
fn credential_fields_may_not_claim_a_role() {
    let creds = vec![f("baseUrl", FieldKind::Url, Some(FieldRole::BaseUrl))];
    assert!(matches!(
        validate_schemas(&creds, &[]).unwrap_err(),
        SchemaError::RoleOnCredentialField { .. }
    ));
}

#[test]
fn duplicate_keys_within_one_schema_are_rejected() {
    let observed = vec![f("v", FieldKind::Text, None), f("v", FieldKind::Int, None)];
    assert!(matches!(
        validate_schemas(&[], &observed).unwrap_err(),
        SchemaError::DuplicateKey { .. }
    ));
}

#[test]
fn an_empty_key_is_rejected() {
    assert!(matches!(
        validate_schemas(&[], &[f("  ", FieldKind::Text, None)]).unwrap_err(),
        SchemaError::BlankKey
    ));
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p qa-product-sdk`
Expected: compile failure — `cannot find type FieldDesc in this scope`.

- [ ] **Step 4: Write the implementation**

```rust
// gears/qa-platform/qa-product-sdk/src/descriptor.rs
//! What a plugin *declares*: the shape of an environment's credentials and of
//! what observing it yields.
//!
//! The UI renders forms, tables and detail pages from these descriptors
//! (spec §8), so a plugin adds a field without any UI change. Two invariants
//! are checked at registration rather than at render time, because both
//! failures are silent when they happen late — see [`validate_schemas`].

use serde::{Deserialize, Serialize};

/// How a field is entered and rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Url,
    Int,
    Bool,
    Enum,
    /// A single-line secret: an API token, a password.
    Secret,
    /// A multi-line secret: a kubeconfig, a PEM key. Renders as the textarea
    /// today's kubeconfig field uses, which is why VHP's form is unchanged in
    /// appearance despite being generated.
    MultilineSecret,
}

impl FieldKind {
    /// Whether a value of this kind is credential material.
    #[must_use]
    pub const fn is_secret(self) -> bool {
        matches!(self, Self::Secret | Self::MultilineSecret)
    }
}

/// A meaning the *platform* understands, claimed by at most one observed field.
///
/// This is what lets observation be fully plugin-defined without costing
/// `qa-insights` a stable `version`/`build` to group by: the plugin returns an
/// opaque attribute map, and the platform copies the role-claimed attributes
/// into real, indexable columns on every write (spec **D10**).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldRole {
    /// Becomes `qa_environments.observed_version` and a run's `APP_VERSION`.
    Version,
    /// Becomes `qa_environments.observed_build` and a run's `APP_BUILD`.
    Build,
    /// Becomes `qa_environments.observed_base_url` — the column that replaces
    /// `vhp_base_url`.
    BaseUrl,
    /// Surfaced on the detail page; has no column of its own because only
    /// Kubernetes products have a namespace at all.
    Namespace,
}

/// One declared field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDesc {
    /// Stable identifier, unique within its schema. Also the key under which
    /// the value appears in `observed_attrs` or `credentials`.
    pub key: String,
    /// Human-facing label. The UI shows this, never `key`.
    pub label: String,
    pub kind: FieldKind,
    pub required: bool,
    /// `None` for the great majority of fields.
    pub role: Option<FieldRole>,
    /// Render as a column in the environments table.
    pub in_table: bool,
    /// Render on the environment detail page.
    pub in_detail: bool,
    pub help: Option<String>,
}

/// Why a plugin's declared schemas were rejected at registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaError {
    /// `observed_schema()` declared a secret field. `observed_attrs` is
    /// rendered on the environment page, so nothing secret may reach it.
    SecretInObservedSchema { key: String },
    /// Two observed fields claimed the same role.
    DuplicateRole { role: FieldRole, first: String, second: String },
    /// A credential field claimed a role. Roles describe observations.
    RoleOnCredentialField { key: String },
    DuplicateKey { key: String },
    BlankKey,
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SecretInObservedSchema { key } => write!(
                f,
                "observed field `{key}` declares a secret kind; observed values are rendered on the environment page and may never be secret"
            ),
            Self::DuplicateRole { role, first, second } => write!(
                f,
                "fields `{first}` and `{second}` both claim role {role:?}; at most one field may claim each role"
            ),
            Self::RoleOnCredentialField { key } => write!(
                f,
                "credential field `{key}` claims a role; roles describe observed values, not credentials"
            ),
            Self::DuplicateKey { key } => write!(f, "duplicate field key `{key}` within one schema"),
            Self::BlankKey => write!(f, "a field key is empty or whitespace"),
        }
    }
}

impl std::error::Error for SchemaError {}

/// Check both declared schemas. Called once per plugin at registration; a
/// failure is a boot failure.
///
/// The two rules worth stating plainly, because both fail silently if deferred:
///
/// * **No secret in `observed`.** `observed_attrs` is serialised into
///   `EnvironmentDto` and rendered. A plugin that echoed its kubeconfig into an
///   observed field would reproduce the 2026-08-28 leak through a new door.
/// * **One field per role.** Two `Version` claims make `APP_VERSION` depend on
///   iteration order — a run stamped with an arbitrary one of two versions,
///   with nothing to notice it.
///
/// # Errors
///
/// Returns the first violation found, checking credentials before observations.
pub fn validate_schemas(
    credential: &[FieldDesc],
    observed: &[FieldDesc],
) -> Result<(), SchemaError> {
    check_keys(credential)?;
    check_keys(observed)?;

    if let Some(field) = credential.iter().find(|f| f.role.is_some()) {
        return Err(SchemaError::RoleOnCredentialField { key: field.key.clone() });
    }

    if let Some(field) = observed.iter().find(|f| f.kind.is_secret()) {
        return Err(SchemaError::SecretInObservedSchema { key: field.key.clone() });
    }

    let mut claimed: Vec<(FieldRole, &str)> = Vec::new();
    for field in observed {
        let Some(role) = field.role else { continue };
        if let Some((_, first)) = claimed.iter().find(|(r, _)| *r == role) {
            return Err(SchemaError::DuplicateRole {
                role,
                first: (*first).to_owned(),
                second: field.key.clone(),
            });
        }
        claimed.push((role, &field.key));
    }
    Ok(())
}

fn check_keys(fields: &[FieldDesc]) -> Result<(), SchemaError> {
    let mut seen: Vec<&str> = Vec::new();
    for field in fields {
        if field.key.trim().is_empty() {
            return Err(SchemaError::BlankKey);
        }
        if seen.contains(&field.key.as_str()) {
            return Err(SchemaError::DuplicateKey { key: field.key.clone() });
        }
        seen.push(&field.key);
    }
    Ok(())
}

#[cfg(test)]
#[path = "descriptor_tests.rs"]
mod descriptor_tests;
```

```rust
// gears/qa-platform/qa-product-sdk/src/lib.rs
//! The contract every QA Platform product plugin implements.
//!
//! See `gears/qa-platform/docs/PRODUCT-PLUGINS-DESIGN.md` for why this exists
//! and what each decision cost.

pub mod descriptor;

pub use descriptor::{FieldDesc, FieldKind, FieldRole, SchemaError, validate_schemas};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p qa-product-sdk`
Expected: 9 passed.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-product-sdk Cargo.toml
git commit -m "feat(qa-product-sdk): descriptor types and registration-time schema validation"
```

---

### Task 2: Observation outcome, the failure type, and role projection

**Files:**

- Create: `gears/qa-platform/qa-product-sdk/src/observation.rs`
- Create: `gears/qa-platform/qa-product-sdk/src/observation_tests.rs`
- Modify: `gears/qa-platform/qa-product-sdk/src/lib.rs`

**Interfaces:**

- Consumes: `FieldDesc`, `FieldRole` from Task 1.
- Produces: `ObservedAttrs`, `ObservationOutcome`, `PluginFailure`, `FailureClass`, `HealthState`, `HealthOutcome`, `RoleProjection`, `project_roles(&[FieldDesc], &ObservedAttrs) -> RoleProjection`. Task 15 writes `RoleProjection` into columns; Task 9 returns `ObservationOutcome`.

- [ ] **Step 1: Write the failing tests**

```rust
// gears/qa-platform/qa-product-sdk/src/observation_tests.rs
use super::*;
use crate::descriptor::{FieldDesc, FieldKind, FieldRole};

fn observed(key: &str, role: Option<FieldRole>) -> FieldDesc {
    FieldDesc {
        key: key.to_owned(),
        label: key.to_owned(),
        kind: FieldKind::Text,
        required: false,
        role,
        in_table: false,
        in_detail: true,
        help: None,
    }
}

#[test]
fn role_projection_lifts_the_claimed_attributes_into_columns() {
    let schema = vec![
        observed("platformVersion", Some(FieldRole::Version)),
        observed("build", Some(FieldRole::Build)),
        observed("baseDomain", Some(FieldRole::BaseUrl)),
        observed("namespace", Some(FieldRole::Namespace)),
        observed("nodeCount", None),
    ];
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");
    attrs.set("build", "1471");
    attrs.set("baseDomain", "https://sv.jele.io");
    attrs.set("namespace", "virtuozzo");
    attrs.set("nodeCount", "3");

    let p = project_roles(&schema, &attrs);
    assert_eq!(p.version.as_deref(), Some("9.2"));
    assert_eq!(p.build.as_deref(), Some("1471"));
    assert_eq!(p.base_url.as_deref(), Some("https://sv.jele.io"));
    assert_eq!(p.namespace.as_deref(), Some("virtuozzo"));
}

#[test]
fn an_unclaimed_role_projects_to_none() {
    let schema = vec![observed("platformVersion", Some(FieldRole::Version))];
    let mut attrs = ObservedAttrs::default();
    attrs.set("platformVersion", "9.2");

    let p = project_roles(&schema, &attrs);
    assert_eq!(p.version.as_deref(), Some("9.2"));
    assert_eq!(p.build, None, "a plugin that declares no Build role yields no APP_BUILD");
}

/// A declared role whose attribute the plugin did not return this cycle must
/// not project a stale or empty value.
#[test]
fn a_declared_role_with_no_value_projects_to_none() {
    let schema = vec![observed("build", Some(FieldRole::Build))];
    let p = project_roles(&schema, &ObservedAttrs::default());
    assert_eq!(p.build, None);
}

#[test]
fn a_blank_attribute_projects_to_none_rather_than_an_empty_string() {
    let schema = vec![observed("build", Some(FieldRole::Build))];
    let mut attrs = ObservedAttrs::default();
    attrs.set("build", "   ");
    assert_eq!(project_roles(&schema, &attrs).build, None);
}

/// `detail` is `&'static str`, so this is the compile-time guarantee that a
/// plugin cannot format credential material into a failure. The test documents
/// the intent; the type is what enforces it.
#[test]
fn a_failure_carries_a_class_and_fixed_text() {
    let f = PluginFailure::classified(FailureClass::AuthRejected, "the credential was rejected");
    assert_eq!(f.class, FailureClass::AuthRejected);
    assert_eq!(f.detail, Some("the credential was rejected"));
    assert_eq!(f.remote_message, None);
}

/// The one sanctioned exception: text the *remote* sent back. It is what keeps
/// "namespaces virtuozzo not found" visible to an operator who can act on it.
#[test]
fn a_failure_may_carry_a_remote_message() {
    let f = PluginFailure::classified(FailureClass::NotFound, "the object was not found")
        .with_remote_message("namespaces \"virtuozzo\" not found");
    assert_eq!(f.remote_message.as_deref(), Some("namespaces \"virtuozzo\" not found"));
}

#[test]
fn health_states_round_trip_through_their_wire_names() {
    for (state, wire) in [
        (HealthState::Ok, "ok"),
        (HealthState::Degraded, "degraded"),
        (HealthState::Down, "down"),
        (HealthState::Unknown, "unknown"),
    ] {
        assert_eq!(state.as_str(), wire);
        assert_eq!(HealthState::from_str_or_unknown(wire), state);
    }
}

/// An unrecognised persisted value must read as `Unknown`, not panic: a column
/// written by a newer build must not take down an older one.
#[test]
fn an_unrecognised_health_value_reads_as_unknown() {
    assert_eq!(HealthState::from_str_or_unknown("wobbly"), HealthState::Unknown);
}

#[test]
fn observed_attrs_serialise_to_a_json_object() {
    let mut attrs = ObservedAttrs::default();
    attrs.set("a", "1");
    let json = serde_json::to_value(&attrs).unwrap();
    assert_eq!(json, serde_json::json!({ "a": "1" }));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p qa-product-sdk observation`
Expected: compile failure — `cannot find type ObservedAttrs in this scope`.

- [ ] **Step 3: Write the implementation**

```rust
// gears/qa-platform/qa-product-sdk/src/observation.rs
//! What observing an environment yields, and how a failure crosses the plugin
//! boundary without carrying credential material with it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::descriptor::{FieldDesc, FieldRole};

/// A plugin's observed values, keyed by `FieldDesc::key`.
///
/// `BTreeMap` rather than `HashMap` so the serialised JSONB is stable: a
/// reordered blob is a spurious row update on every observation cycle.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObservedAttrs(BTreeMap<String, String>);

impl ObservedAttrs {
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), value.into());
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// The shape of a failure, independent of the bytes that caused it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureClass {
    /// The target could not be reached at all.
    Unreachable,
    /// The target was reached and refused the credential.
    AuthRejected,
    /// The target was reached but the thing being read is not there.
    NotFound,
    /// Something was read and could not be understood.
    Malformed,
    Timeout,
    /// A defect in the plugin itself.
    Internal,
}

/// A failure crossing the plugin boundary.
///
/// # Why `detail` is `&'static str`
///
/// This is the enforcement point for the rule in
/// `PRODUCT-PLUGINS-DESIGN.md` §9, which exists because a measured leak on
/// 2026-08-28 put a PEM private key on the platform page: a serde error
/// quoted the whole offending scalar, and for a document that *is* one scalar
/// the offending scalar is the whole document.
///
/// A `String` here would let a plugin author write
/// `format!("{upstream_error}")` and reproduce that leak from inside a crate
/// this team does not review. `&'static str` cannot be produced from runtime
/// bytes without `Box::leak`, which is greppable and lintable. The rule is
/// therefore *classification, not sanitisation* — the same shape
/// `qa-environments/src/infra/observer/errors.rs` already implements, moved
/// out to where third-party plugins must obey it too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginFailure {
    pub class: FailureClass,
    /// Fixed text chosen by variant. Never derived from any value the plugin
    /// read or was given.
    pub detail: Option<&'static str>,
    /// Text the **remote** sent back — an API server's `Status.message`, an
    /// HTTP error body's `message` field.
    ///
    /// The one sanctioned exception to "nothing formatted crosses this
    /// boundary", and it is deliberate: `kube_observer`'s own header argues
    /// that "namespaces virtuozzo not found" is what makes a broken
    /// environment *fixable* rather than merely broken. A plugin must never
    /// put its own formatting here — only text it received. The
    /// `assert_no_leak` harness (Task 4) is what checks that it didn't.
    pub remote_message: Option<String>,
}

impl PluginFailure {
    #[must_use]
    pub const fn classified(class: FailureClass, detail: &'static str) -> Self {
        Self { class, detail: Some(detail), remote_message: None }
    }

    #[must_use]
    pub const fn bare(class: FailureClass) -> Self {
        Self { class, detail: None, remote_message: None }
    }

    #[must_use]
    pub fn with_remote_message(mut self, message: impl Into<String>) -> Self {
        self.remote_message = Some(message.into());
        self
    }
}

impl std::fmt::Display for PluginFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.detail, &self.remote_message) {
            (Some(d), Some(r)) => write!(f, "{d}: {r}"),
            (Some(d), None) => write!(f, "{d}"),
            (None, Some(r)) => write!(f, "{r}"),
            (None, None) => write!(f, "{:?}", self.class),
        }
    }
}

impl std::error::Error for PluginFailure {}

/// The result of one observation attempt. A failure is a **value**, not an
/// error: the message is persisted and shown, because an operator who can see
/// why it failed can fix it and one who sees a blank page cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationOutcome {
    Detected(ObservedAttrs),
    Failed(PluginFailure),
}

/// Coarse health, in a vocabulary every product can express.
///
/// "Nodes ready" is not health for a SaaS tenant or an appliance, so the
/// platform keeps only the verdict and the plugin puts its own facts in
/// `ObservedAttrs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Ok,
    Degraded,
    Down,
    /// Nothing is known — distinct from `Down`, which means something looked
    /// and found the target unhealthy.
    #[default]
    Unknown,
}

impl HealthState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Down => "down",
            Self::Unknown => "unknown",
        }
    }

    /// Total, by design: a value written by a newer build must not panic an
    /// older one reading the same column.
    #[must_use]
    pub fn from_str_or_unknown(raw: &str) -> Self {
        match raw {
            "ok" => Self::Ok,
            "degraded" => Self::Degraded,
            "down" => Self::Down,
            _ => Self::Unknown,
        }
    }
}

/// One health read. Kept separate from [`ObservationOutcome`] because the two
/// fail independently: a credential scoped to one namespace can detect a
/// version perfectly and still be forbidden from reading cluster-wide health.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthOutcome {
    Checked { state: HealthState, detail: Option<&'static str> },
    Failed(PluginFailure),
    /// Nothing was attempted, so nothing is known. The platform writes none of
    /// the health columns for this variant — not even `health_checked_at`.
    NotAttempted,
}

/// Both halves of one `observe` call. One call rather than two methods, so one
/// client and one handshake serve both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginObservation {
    pub environment: ObservationOutcome,
    pub health: HealthOutcome,
}

/// The four role-claimed attributes, lifted out for the platform's columns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleProjection {
    pub version: Option<String>,
    pub build: Option<String>,
    pub base_url: Option<String>,
    pub namespace: Option<String>,
}

/// Copy the role-claimed attributes out of a plugin's opaque map.
///
/// This is decision **D10**: observation is fully plugin-defined, and the
/// platform still gets indexable `observed_version` / `observed_build` /
/// `observed_base_url` columns and a deterministic `APP_VERSION` / `APP_BUILD`
/// for `qa-runs` to snapshot. Blank projects to `None`, never to `Some("")` —
/// an empty `APP_VERSION` reaching every test is the failure this guards.
#[must_use]
pub fn project_roles(schema: &[FieldDesc], attrs: &ObservedAttrs) -> RoleProjection {
    let mut out = RoleProjection::default();
    for field in schema {
        let Some(role) = field.role else { continue };
        let value = attrs
            .get(&field.key)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned);
        match role {
            FieldRole::Version => out.version = value,
            FieldRole::Build => out.build = value,
            FieldRole::BaseUrl => out.base_url = value,
            FieldRole::Namespace => out.namespace = value,
        }
    }
    out
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod observation_tests;
```

Add to `lib.rs`:

```rust
pub mod observation;

pub use observation::{
    FailureClass, HealthOutcome, HealthState, ObservationOutcome, ObservedAttrs, PluginFailure,
    PluginObservation, RoleProjection, project_roles,
};
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p qa-product-sdk`
Expected: 18 passed.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-product-sdk
git commit -m "feat(qa-product-sdk): observation outcome, classified failures, role projection"
```

---

> **A note on fidelity from here.** Tasks 1–2 are written at full step-by-step
> density because they define the types every later task consumes — getting a
> field name wrong there costs every downstream task. Tasks 3–22 carry exact
> file paths, exact signatures, the real test cases with their real assertions,
> and the exact commands and commit messages, but do not spell out every test
> body at length. Where a task says "assert X", the assertion is the
> requirement; how it is spelled is the implementer's.

### Task 3: The trait, run access, and the runner spec

**Files:**

- Create: `gears/qa-platform/qa-product-sdk/src/access.rs`, `src/access_tests.rs`, `src/plugin.rs`
- Modify: `gears/qa-platform/qa-product-sdk/src/lib.rs`

**Interfaces:**

- Consumes: `ObservedAttrs`, `PluginFailure`, `PluginObservation` (Task 2); `FieldDesc` (Task 1).
- Produces:
  ```rust
  pub struct RunAccess { pub mounts: Vec<MountSpec>, pub env: Vec<RunVar>, pub service_account: Option<String> }
  pub enum MountSpec {
      Secret { credstore_ref: String, path: String, mode: Option<i32> },
      ConfigValue { value: SecretValue, path: String, mode: Option<i32> },
  }
  pub struct RunVar { pub name: String, pub value: String }
  pub struct RunnerSpec { pub image: Option<String>, pub command: Vec<String>, pub image_pull_policy: Option<String>, pub extra_volumes: Vec<VolumeSpec> }
  pub struct RunVarContract { pub names: Vec<String>, pub reserved: BTreeSet<String> }
  pub struct EnvironmentHandle<'a> { pub credentials: &'a [ResolvedCredential], pub config: &'a serde_json::Value }
  pub struct ResolvedCredential { pub key: String, pub value: SecretValue }
  pub struct CredentialInput { pub fields: BTreeMap<String, String> }
  pub struct StoredField { pub key: String, pub credstore_ref: String }
  #[async_trait] pub trait QaProductPluginV1: Send + Sync { /* eight methods, below */ }
  ```

  Task 9–10 implement the trait; Task 15 calls `observe`; Task 18 calls `prepare_run_access`, `runner` and `env_contract`.
  > **Superseded, 2026-09-03 review.** Several of the shapes sketched above did not survive the whole-branch review and are **not** what Tasks 9–18 will find in the crate. See `PRODUCT-PLUGINS-DESIGN.md` §5.3 for the full table and the reasoning. In short: `CredentialInput.fields` is `BTreeMap<String, SecretValue>`; `validate_credentials` returns `Vec<CredentialClassification { key, is_secret }>` (a plugin cannot know a `credstore_ref` — it runs before the gear writes one); `EnvironmentHandle` carries `slots: &[CredentialSlot { key, credstore_ref, value: Option<SecretValue> }]` and `prepare_run_access` **must work from `credstore_ref` alone**, so dispatch never resolves plaintext; `RunnerSpec` has no `extra_volumes` and `VolumeSpec` is deleted; `RunVarContract` has no `names`; `MountSpec`/`RunAccess` are not `Clone`; `RegisteredPlugin::new` is the only way to admit a plugin; and `retain_declared` must be applied before Task 15 persists an observation.

- [ ] **Step 1: Write the failing tests** in `access_tests.rs`

Four cases, all about the types rather than behaviour, because this task is mostly a contract:

1. `runner_spec_default_inherits_the_deployment_image` — `RunnerSpec::default().image` is `None`, and a doc comment records that `None` means "use `qa-runs.argo.runner_image`". A plugin indifferent to its image declares nothing.
2. `run_var_contract_reserved_is_a_set_not_a_replacement` — construct a contract reserving `MY_VAR`, call `RunVarContract::union_with_floor(&["TEST_FILES"])`, assert the result contains **both**. This is **D8** expressed as a test.
3. `reserved_names_compare_case_insensitively` — reserving `my_var` refuses `MY_VAR`, matching `params::RESERVED_NAMES`' existing uppercase-key behaviour.
4. `mount_spec_debug_does_not_render_a_config_value` — `format!("{:?}", MountSpec::ConfigValue { value: SecretValue::from("PRIVATE"), .. })` does not contain `PRIVATE`. `SecretValue` already redacts, so this pins that `MountSpec` does not defeat it by deriving `Debug` over an unwrapped copy.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p qa-product-sdk access`
Expected: compile failure — `cannot find type RunAccess`.

- [ ] **Step 3: Write `access.rs`**

`RunVarContract::union_with_floor(&self, floor: &[&str]) -> BTreeSet<String>` uppercases both sides and returns the union — never `self.reserved` alone. Its doc comment must say why: the platform's transport-critical floor (`TEST_FILES`, `TEST_BUNDLE_URL`, `TEST_VERSION`, `COLLECT_ONLY`, the collect and progress URLs) holds whatever a plugin declares, because a plugin that could shrink it could let a run parameter overwrite the bundle URL.

`MountSpec` derives `Clone` and hand-writes `Debug` for the `ConfigValue` arm as `ConfigValue { path, value: <redacted> }`.

- [ ] **Step 4: Write `plugin.rs`** — the trait, exactly as in spec §5:

```rust
#[async_trait]
pub trait QaProductPluginV1: Send + Sync {
    fn credential_schema(&self) -> Vec[[ORCA_RICH_MD:09e3bb8d68a75f1f4b44e3d9875e289d:inline-html:%3CFieldDesc%3E]];
    fn observed_schema(&self) -> Vec[[ORCA_RICH_MD:09e3bb8d68a75f1f4b44e3d9875e289d:inline-html:%3CFieldDesc%3E]];

    async fn validate_credentials(&self, input: &CredentialInput)
        -> Result<Vec[[ORCA_RICH_MD:09e3bb8d68a75f1f4b44e3d9875e289d:inline-html:%3CStoredField%3E]], PluginFailure>;
    async fn observe(&self, env: &EnvironmentHandle<'_>) -> PluginObservation;
    async fn prepare_run_access(&self, env: &EnvironmentHandle<'_>)
        -> Result<RunAccess, PluginFailure>;

    fn runner(&self, observed: Option<&ObservedAttrs>) -> RunnerSpec;
    fn env_contract(&self) -> RunVarContract;

    async fn health_check(&self) -> Result<HealthState, PluginFailure> {
        Ok(HealthState::Ok)
    }
}
```

`validate_credentials` returns `Vec<StoredField>` rather than writing to credstore itself: only the *gear* may write credstore, because only it holds the tenant-scoped `SecurityContext`. The plugin's job is to say which submitted fields are credential material and under what key they belong — mirroring `qa-catalog`'s `SshKeysRepository::create`, which likewise takes an already-resolved `credstore_ref`.

- [ ] **Step 5: Run tests, then the workspace gate**

Run: `cargo test -p qa-product-sdk` — expected: 22 passed.
Run: `cargo test --workspace` — expected: no regressions.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-product-sdk
git commit -m "feat(qa-product-sdk): the QaProductPluginV1 trait, RunAccess and RunnerSpec"
```

---

### Task 4: The leak-conformance harness and the GTS spec

**Files:**

- Create: `gears/qa-platform/qa-product-sdk/src/testing.rs`, `src/testing_tests.rs`, `src/gts.rs`
- Modify: `gears/qa-platform/qa-product-sdk/src/lib.rs`, `Cargo.toml` (add `tracing`, `tracing-subscriber` with the `test-util` feature)

**Interfaces:**

- Produces: `qa_product_sdk::testing::assert_no_leak(plugin: &dyn QaProductPluginV1, canary: &Canary)`, `Canary::vhp_shaped()`, and `QaProductPluginSpecV1`. Every plugin crate must call `assert_no_leak` in its own test suite — Tasks 9 and 10 do.

- [ ] **Step 1: Write the failing tests** in `testing_tests.rs`

Build two throwaway in-test plugins and assert the harness distinguishes them:

1. `a_well_behaved_plugin_passes` — a plugin whose `observe` returns `PluginFailure::classified(FailureClass::Malformed, "the document could not be parsed")` passes.
2. `a_plugin_that_echoes_the_canary_into_remote_message_fails` — a plugin that puts the canary into `remote_message` is caught. This is the case that matters: `remote_message` is the one `String` crossing the boundary, so it is the one hole the harness must cover.
3. `a_plugin_that_echoes_the_canary_into_observed_attrs_fails` — caught via the serialised blob.
4. `a_plugin_that_logs_the_canary_fails` — caught via a captured `tracing` subscriber.
5. `a_plugin_that_returns_the_canary_in_a_run_var_fails` — caught via `RunAccess.env`.

Each failing case asserts the harness panics with a message naming **which** surface leaked, because "a leak was detected" without the surface is not actionable.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p qa-product-sdk testing`
Expected: compile failure — `cannot find function assert_no_leak`.

- [ ] **Step 3: Write `testing.rs`**

`Canary::vhp_shaped()` carries three distinct markers, because the three leak shapes differ:

```rust
pub struct Canary {
    /// A real PEM block. This is the exact shape that leaked on 2026-08-28:
    /// serde quotes the offending scalar, and for a document that is entirely
    /// one scalar the offending scalar is the whole document.
    pub pem: String,
    /// A bearer token — the SaaS-plugin shape.
    pub token: String,
    /// A password — the appliance/admin-API shape.
    pub password: String,
}
```

`assert_no_leak` drives `credential_schema`, `observed_schema`, `validate_credentials`, `observe`, `prepare_run_access`, `runner` and `env_contract` with the canary as the credential material, then checks every marker against: each returned string field, each value's `Debug` rendering, the serialised `ObservedAttrs`, every `RunVar` name and value, every `MountSpec` path, and every `tracing` event captured for the duration. It panics naming the surface and the marker.

Generalises `postgres-credstore-plugin`'s `src/infra/storage/leak_tests.rs` and `tests/sea_orm_trace_exposure.rs` — read both before writing this.

- [ ] **Step 4: Write `gts.rs`**

Mirrors `credstore-sdk/src/gts.rs` exactly:

```rust
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~"),
    description = "QA Platform product plugin specification",
    properties = "",
)]
pub struct QaProductPluginSpecV1;
```

- [ ] **Step 5: Run the tests and the workspace gate**

Run: `cargo test -p qa-product-sdk` — expected: 27 passed at the time of writing; the final-review fix wave raised the crate to 53. Trust the crate, not this number.
Run: `cargo test --workspace && cargo clippy --workspace --all-targets --all-features`

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-product-sdk
git commit -m "feat(qa-product-sdk): leak-conformance harness and the GTS plugin spec"
```

---

# Phase B — The rename

Zero behaviour change, and the largest diff in the project. Three tasks, each its own commit, so a reviewer can read the mechanical part without the judgement part mixed in.

**Before starting:** re-read Global Constraints' rule on legacy citations. `manager/src/services/platforms.rs`, `platforms_meta`, `argo.rs:NNN` are references to the *source system*. They do not rename. `doc_citations_tests.rs` fails if they are touched.

### Task 5: Clear the `runvars` collision first

Do this **before** Task 6. Renaming `TargetPlatform` → `Environment` while `EnvVar`/`EnvInputs` still mean runner variables produces a window where "environment" means two things in one gear, and every judgement call in Task 6's doc pass is made worse by it.

**Files:**

- Rename: `qa-runs/qa-runs/src/domain/env_assembly.rs` → `runvars.rs`
- Modify: `qa-runs/qa-runs/src/domain/mod.rs`, `domain/params.rs`, `domain/naming.rs`, `domain/ports/run_executor.rs`, `domain/service/dispatch_spec.rs`, `domain/service/dispatch_tests.rs`, `infra/executor/mock.rs`

**Interfaces:**

- Produces: `RunVar`, `RunVarInputs`, `runvars::assemble`, `runvars::assemble_collect_vars`, `TieredRunVars`. Tasks 17–18 consume these.
- **Name collision, resolved deliberately:** Task 3 defines `qa_product_sdk::RunVar` as `{ name, value }`. This task renames `qa-runs`' `EnvVar` to the same name *locally*, because `qa-runs` does not yet depend on the SDK. Both must be structurally identical; Task 17 deletes the local definition and re-exports the SDK's. There is no `EnvContract` in `qa-runs` today — `RunVarContract` is the SDK's type and only the SDK's.


| before                  | after                   |
| ----------------------- | ----------------------- |
| `EnvVar`                | `RunVar`                |
| `EnvInputs`             | `RunVarInputs`          |
| `TieredVariables`       | `TieredRunVars`         |
| `assemble_collect_env`  | `assemble_collect_vars` |
| `env_assembly` (module) | `runvars`               |


- [ ] **Step 1: Run the baseline**

Run: `cargo test -p qa-runs`
Record the pass count. It must be identical after this task — a rename that changes a test count changed behaviour.

- [ ] **Step 2: Rename the module file and its references**

```bash
git mv gears/qa-platform/qa-runs/qa-runs/src/domain/env_assembly.rs \
       gears/qa-platform/qa-runs/qa-runs/src/domain/runvars.rs
```

Then apply the identifier table above across the eight files listed. 82 identifier occurrences total. Do **not** touch the frozen variable *names* — `E2E_VHP_BASE_URL`, `VPADM_BASE_DOMAIN`, `E2E_K8S_NAMESPACE`, `KUBECONFIG`, `VHP_COLLECT_URL`, `COLLECT_ONLY` are the contract with test authors and are unchanged by this entire plan until Task 10 moves *where they are declared*.

- [ ] **Step 3: Update the module's own header**

`runvars.rs`' module doc currently opens by describing environment assembly. Rewrite the first paragraph to say "runner variables", and add one sentence recording why the module was renamed: the aggregate in `qa-environments` is now `Environment`, and one word could not mean both.

- [ ] **Step 4: Verify**

Run: `cargo test -p qa-runs`
Expected: the same pass count as Step 1, zero failures.
Run: `cargo clippy -p qa-runs --all-targets --all-features` — expected: clean.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-runs
git commit -m "refactor(qa-runs): rename env_assembly to runvars, EnvVar to RunVar

Clears the word 'environment' for qa-environments' aggregate, which is
renamed from TargetPlatform to Environment in the next commit. Pure rename:
no variable name handed to a runner changes, and the test count is identical."
```

---

### Task 6: Rename the aggregate in `qa-environments`

**Files:**

- Modify: every file under `qa-environments/qa-environments-sdk/src/` and `qa-environments/qa-environments/src/`
- Create: `qa-environments/qa-environments/src/infra/storage/migrations/m20260903_000010_rename_platform_tables.rs`
- Modify: `qa-environments/qa-environments/src/infra/storage/migrations/mod.rs`

**Interfaces:**

- Produces: `qa_environments_sdk::{Environment, NewEnvironment, EnvironmentPatch}`, `EnvironmentsService`, `EnvironmentsRepository`, `EnvironmentDto`. Tasks 12–20 and the UI consume these.


| before                                                          | after                                                                    |
| --------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `TargetPlatform`                                                | `Environment`                                                            |
| `NewPlatform` / `PlatformPatch`                                 | `NewEnvironment` / `EnvironmentPatch`                                    |
| `PlatformsService` / `PlatformsRepository` / `PlatformsSeaRepo` | `Environments*`                                                          |
| `PlatformDto`                                                   | `EnvironmentDto`                                                         |
| `platform_id` / `target_platform_id`                            | `environment_id`                                                         |
| `qa_platforms` / `qa_platform_variables` / `qa_platform_leases` | `qa_environments` / `qa_environment_variables` / `qa_environment_leases` |
| `GET /qa/v1/platforms`                                          | `GET /qa/v1/environments`                                                |


**Not renamed:** the gear name `qa-environments`, the crate names, the config stanza, the database name, the Helm values. `qa-environments` was always the right gear name; only its aggregate was misnamed.

- [ ] **Step 1: Baseline** — `cargo test -p qa-environments`, record the count.

- [ ] **Step 2: Write the table-rename migration**

```rust
// m20260903_000010_rename_platform_tables.rs
//! Renames three tables to match the aggregate's new name (spec D5).
//!
//! `RENAME TABLE` rather than create-copy-drop: the tables carry live rows and
//! foreign keys, and a copy would need a maintenance window this change does
//! not otherwise need. Reversible in `down`.
```

Use `Table::rename()` for all three, plus `Index::drop`/`create` for any index whose name embeds the old table name. Write `down` as the exact inverse and test it.

- [ ] **Step 3: Apply the identifier rename**

~2,130 occurrences. Mechanical. Two rules:

- `platform_id` → `environment_id` **except** where it names a legacy column in a doc citation.
- `sdk::TargetPlatform` → `sdk::Environment` everywhere including `qa-runs`, `qa-insights` and the UI's generated types — those crates will not compile until Task 7, so Tasks 6 and 7 land as one push even though they are two commits.

- [ ] **Step 4: Verify the migration round-trips**

Run: `cargo test -p qa-environments migrations`
Expected: the up/down round-trip test passes on SQLite, Postgres and MySQL.

- [ ] **Step 5: Verify no behaviour moved**

Run: `cargo test -p qa-environments`
Expected: the same pass count as Step 1.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-environments
git commit -m "refactor(qa-environments)!: rename TargetPlatform to Environment

A product may be an IaaS, a PaaS, an OS or an appliance; 'platform' named the
product, not the thing tested against. Renames the aggregate, its three tables
and its REST path. The gear, crate, config stanza and database names are
unchanged. Behaviour is identical: same test count, no logic touched."
```

---

### Task 7: Propagate the rename, and the doc-prose pass

**Files:**

- Modify: `qa-runs/`, `qa-insights/`, `qa-platform-ui/src/`, `config/qa-platform.yaml`, `gears/qa-platform/docs/*.md`

- [ ] **Step 1: Fix the compile** — apply the identifier rename across `qa-runs`, `qa-insights` and the UI's `api/types.ts` + `api/adapters.ts`. Regenerate `api/generated/openapi.d.ts` with `make openapi`.
- [ ] **Step 2: The doc-prose pass — by review, never by `sed`**

~4,400 doc-comment mentions. For each, decide:

- Does it describe **this** code? → rename.
- Does it cite the **legacy** system (`platforms_meta`, `manager/src/services/*.rs`, `argo.rs:NNN`, "the source system's")? → leave verbatim.

Where a sentence contains both, rewrite it so the distinction is visible to the next reader, e.g. "this gear's `Environment` corresponds to legacy's `platforms_meta` row".

- [ ] **Step 3: Verify the citations survived**

Run: `cargo test -p qa-runs doc_citations`
Expected: PASS. This test exists precisely to catch a `sed` that ate a legacy reference.

- [ ] **Step 4: Full gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets --all-features
cargo fmt --all
make ui-test
```

Expected: all green, and the workspace pass count identical to the pre-Phase-B baseline.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: propagate the Environment rename through qa-runs, qa-insights and the UI

Completes the rename. Doc prose was reviewed by hand rather than substituted:
citations of the legacy system keep their original names, and
doc_citations_tests.rs is the guard."
```

---

# Phase C — The plugins

Both crates build and register. Nothing resolves them yet, so this phase is entirely additive and revertible by deleting two directories and three lines.

### Task C0: SDK amendments Phase C depends on

> **Added 2026-09-04, during execution.** Not part of the original twenty-two.
> The Phase C pre-flight scan found Task 10 structurally blocked, and the user
> ruled on two leak-harness holes that must close before any plugin is written
> against the harness. Full rulings: C4 and U1 in
> `.superpowers/sdd/2026-09-03-product-plugins/progress.md`. Task brief:
> `.superpowers/sdd/2026-09-03-product-plugins/task-C0-brief.md`.

Touches `qa-product-sdk` and nothing else. Nothing consumes the SDK yet, which
is why these are cheap now and expensive after Task 15.

1. `**EnvironmentHandle` gains `observed: Option<&ObservedAttrs>`.** Task 10's
 `prepare_run_access` must emit `E2E_VHP_BASE_URL`, `VPADM_BASE_DOMAIN` and
 `E2E_K8S_NAMESPACE`, whose values are role-projected *observation* state.
 The handle carries only credential slots and operator-set `config`, so today
 there is no source for them. Rejected alternative: have the gear write them
 into `config` — `config` is operator-set and these are machine-observed, and
 merging the two makes the environment page unable to say which is which.
2. **The leak harness scans `MountSpec::ConfigValue`'s value**, not only its
 `.path`. Today a plugin can move credential plaintext into a container mount
 and `assert_no_leak` raises nothing.
3. **The reference-only contract becomes unconditional.** `prepare_run_access`
 driven with references alone must return `Ok`, asserted flatly, rather than
 inferred from `run_access.is_ok() && ref_access.is_err()` — a plugin that
 prefers plaintext and degrades quietly passes that inference today.

Each becomes a row in the spec's §5.3 amendment table.

---

### Task 8: `qa-connector-k8s` library crate

Lift, do not rewrite. Every line moved here is behaviour the subsystem already proved; a rewrite would put that proof at risk for no gain.

**Files:**

- Create: `gears/qa-platform/connectors/qa-connector-k8s/Cargo.toml`, `src/lib.rs`
- Move: `qa-environments/qa-environments/src/infra/observer/{kube_observer.rs, errors.rs, secret_writer.rs}` → `qa-connector-k8s/src/`

`domain/observation.rs` is **not** moved here. Its rules — `core-install-metadata`,
`vp-gateway-hostnames`, `platformVersion` — are VHP install topology, not
Kubernetes mechanics, so they belong to the product plugin. Task 9 moves them.

- Modify: root `Cargo.toml` members

**Interfaces:**

- Produces: `KubeClient::from_kubeconfig(&SecretValue) -> Result<KubeClient, PluginFailure>`, `KubeClient::read_configmap(ns, name) -> Result<BTreeMap<String,String>, PluginFailure>`, `KubeClient::node_health() -> HealthOutcome`, `ensure_secret(credstore_ref, &SecretValue) -> Result<(), PluginFailure>`, `classify(&kube::Error) -> PluginFailure`.

- [ ] **Step 1: Create the crate** with `kube`, `k8s-openapi`, `hyper-util`, `rustls` as **non-optional** dependencies — the feature gate exists no longer, because the containment is now structural: this crate is the only one that names them, and only plugins that need clusters link it.
- [ ] **Step 2: Move the three files with `git mv`** so history follows them.
- [ ] **Step 3: Convert the error surface**

`errors.rs`' classification table currently returns `&'static str`. Rewrite each arm to return `PluginFailure::classified(class, fixed_text)`, preserving **every existing fixed string verbatim** and mapping each variant to a `FailureClass`. The `kube::Error::Api` arm keeps its `Status::message` interpolation — but now via `.with_remote_message(status.message)` rather than `format!`, which is the same behaviour with the leak made structurally impossible.

Read `errors.rs`' module header before touching it. It records the measured leak and the reasoning for classification-over-sanitisation; that header moves with the file and must survive.

- [ ] **Step 4: Verify the moved tests still pass**

Run: `cargo test -p qa-connector-k8s`
Expected: every test that lived in `observer/` passes unchanged, including the `leak`-named ones.

- [ ] **Step 5: Verify `qa-environments` still builds**

`qa-environments` still has its own `infra/observer/` path active until Task 19 — this task **copies the capability out**, it does not remove the old one. If `git mv` broke the old path, restore it: Phase C must not change `qa-environments` behaviour.

Run: `cargo test --workspace` — expected: green.

- [ ] **Step 6: Commit**

```bash
git add -A gears/qa-platform/connectors/qa-connector-k8s Cargo.toml
git commit -m "feat(qa-connector-k8s): lift the Kubernetes observer into a library crate"
```

---

### Task 9: `qa-vhp-product-plugin` — schemas and observation

**Files:**

- Create: `gears/qa-platform/plugins/qa-vhp-product-plugin/Cargo.toml`, `src/lib.rs`, `src/schemas.rs`, `src/observe.rs`, `src/observe_tests.rs`, `src/detect.rs`, `src/detect_tests.rs`

**Interfaces:**

- Consumes: `QaProductPluginV1` (Task 3), `qa-connector-k8s` (Task 8).
- Produces: `VhpProductPlugin`, implementing `credential_schema`, `observed_schema`, `validate_credentials`, `observe`.

- [ ] **Step 1: Declare the schemas** in `schemas.rs`

```
credential_schema():
  kubeconfig      MultilineSecret  required   "Kubeconfig"
  vpadm_namespace Text             optional   "vpadm namespace"  help: default "virtuozzo"

observed_schema():
  platformVersion Text  role=Version   in_table  "Platform version"
  build           Text  role=Build     in_table  "Build"
  baseDomain      Url   role=BaseUrl   in_table  "Base URL"
  namespace       Text  role=Namespace           "Namespace"
  rawVersion      Text                           "Raw platformVersion"
  externalHosts   Text                           "External gateway hostnames"
```

`baseDomain` claiming `BaseUrl` is what makes `observed_base_url` the successor to `vhp_base_url` with no special case anywhere in the platform.

- [ ] **Step 2: Move the detection rules and their tests**

`detect.rs` is `qa-environments/src/domain/observation.rs` moved verbatim: `parse_platform_version`, `detected_from_data`, `external_hosts`, the `core-install-metadata` and `vp-gateway-hostnames` knowledge, and the `GATEWAY_INTERNAL_CLUSTERIP_KEY` skip. Its tests move with it and must pass unchanged — these are the frozen VHP rules and this task does not get to improve them.

- [ ] **Step 3: Write `observe`**

Reads `core-install-metadata` for the version, `vp-gateway-hostnames` for the base domain, and node/namespace health, in one client. Failures classify through `qa-connector-k8s`'s table. Version detection and health fail **independently** — a namespace-scoped kubeconfig detects a version perfectly and is forbidden from listing nodes, and coupling them would report that environment as having no version, which is false. This is existing behaviour (`ports/platform_observer.rs`' `HealthOutcome` doc) and it is preserved.

- [ ] **Step 4: Run the conformance harness**

```rust
#[tokio::test]
async fn the_vhp_plugin_leaks_no_credential_material() {
    let plugin = VhpProductPlugin::new_for_test();
    qa_product_sdk::testing::assert_no_leak(&plugin, &Canary::vhp_shaped()).await;
}
```

Plus `validate_schemas(&plugin.credential_schema(), &plugin.observed_schema()).unwrap()` as its own test, so a bad schema fails here rather than at boot.

- [ ] **Step 5: Verify**

Run: `cargo test -p qa-vhp-product-plugin` — expected: every moved detection test plus the two new ones pass.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/plugins/qa-vhp-product-plugin Cargo.toml
git commit -m "feat(qa-vhp-product-plugin): schemas, VHP detection rules and observation"
```

---

### Task 9b: ConfigMap read coverage, and making the leak harness reach the VHP rules

> **Added 2026-09-04, during execution.** Not part of the original twenty-two.
> Created by Task 9's review and enlarged by a measurement. Rulings 9-1 and 9-4
> in `.superpowers/sdd/2026-09-03-product-plugins/progress.md`; brief at
> `.superpowers/sdd/2026-09-03-product-plugins/task-9b-brief.md`.

Two facts, both measured rather than inferred:

1. `KubeClient::find_configmap`, `find_configmap_or_missing` and
   `scan_configmaps` have **zero coverage workspace-wide** — `qa-connector-k8s`'
   stub server routes only `/api/v1/nodes` and `/api/v1/namespaces` and panics
   on anything else. So `detect`'s targeted-lookup-then-label-scan fall-through
   is untested end to end, in the frozen rules whose whole value is fidelity.
2. **`assert_no_leak` never reaches this plugin's success path.** It drives
   `observe` with a PEM canary kubeconfig, so the call fails inside
   `KubeClient::from_kubeconfig` and returns before `detect` is entered. Proved
   twice independently: an unconditional `panic!` as `detect`'s first statement
   leaves all 35 tests passing.

The deliverable is therefore not four tests. It is a `test-support` feature on
`qa-connector-k8s` exposing a route-table constructor with **no `kube` type in its
signature** (ADR-0001), coverage for the three untested methods, the four
fall-through cases, and — the part that matters — a conformance test that plants
the canary in the `ConfigMap` responses so a plugin echoing detected content
into an observed attribute, a failure message or a log line is caught.

---

### Task 10: `qa-vhp-product-plugin` — the run-facing half

**Files:**

- Create: `qa-vhp-product-plugin/src/run.rs`, `src/run_tests.rs`

**Interfaces:**

- Produces: `env_contract`, `prepare_run_access`, `runner` on `VhpProductPlugin`.

- [ ] **Step 1: Move the frozen variable names here**

```rust
const PLATFORM_BASE_URL_VAR: &str = "E2E_VHP_BASE_URL";
const BASE_DOMAIN_VAR: &str       = "VPADM_BASE_DOMAIN";
const NAMESPACE_VAR: &str         = "E2E_K8S_NAMESPACE";
const KUBECONFIG_VAR: &str        = "KUBECONFIG";
```

These leave `qa-runs/src/domain/runvars.rs` and arrive here. Their **values and derivation rules do not change** — including that `VPADM_BASE_DOMAIN` is the bare host derived from the base URL, and that an unparseable base URL still yields `E2E_VHP_BASE_URL` while suppressing only the derived domain.

- [ ] **Step 2: Port the tests that pin those rules**

Move from `runvars.rs`' test module, unchanged in their assertions:

- a variable-tier value is overridden by a parameter (`E2E_VHP_BASE_URL` is not reserved — decision D3 of the parity spec)
- `VPADM_BASE_DOMAIN` shares `E2E_VHP_BASE_URL`'s position and precedence
- an unparseable base URL yields `E2E_VHP_BASE_URL` and no `VPADM_BASE_DOMAIN`
- no platform ⇒ neither variable is present

They are now the plugin's tests. The precedence *ladder* stays in `qa-runs` and keeps its own tests.

- [ ] **Step 3: Write `prepare_run_access`**

Returns the kubeconfig as `MountSpec::Secret { credstore_ref, path: "/etc/qa/kubeconfig", mode: Some(0o400) }` plus `RunVar`s for the four names above. The mount path and the `KUBECONFIG` value must be the same string — the equality the old `KubeconfigMount` documented as "an obligation the port cannot enforce" is now enforceable here, in one function, and a test asserts it.

- [ ] **Step 4: Write `runner`** — returns `RunnerSpec::default()`, i.e. inherit the deployment image. VHP does not need its own; **D11** means the option exists for the products that will.

- [ ] **Step 5: Verify and commit**

Run: `cargo test -p qa-vhp-product-plugin` and `cargo test --workspace`.

```bash
git add gears/qa-platform/plugins/qa-vhp-product-plugin
git commit -m "feat(qa-vhp-product-plugin): env contract, run access and runner spec"
```

---

### Task 11: Register the plugin gear

**Files:**

- Create: `qa-vhp-product-plugin/src/gear.rs`
- Modify: `apps/cf-gears-example-server/Cargo.toml`, `apps/cf-gears-example-server/src/registered_gears.rs`, `config/qa-platform.yaml`

- [ ] **Step 1: Write `gear.rs`** — the shape is `postgres-credstore-plugin/src/gear.rs`, read it first:

```rust
#[toolkit::gear(name = "qa-vhp-product-plugin", deps = [types_registry])]
pub struct VhpProductPluginGear { plugin: OnceLock<Arc<VhpProductPlugin>> }
```

`init` must, in this order: build the plugin, `**validate_schemas` and fail loudly on error**, `PluginV1::<QaProductPluginSpecV1>::build_registration("cf.core._.vhp_product.v1", vendor, priority)`, register with types-registry, `RegisterResult::ensure_all_ok`, commit to `OnceLock`, then `register_scoped::<dyn QaProductPluginV1>(ClientScope::gts_id(&instance_id), api)`.

No `deps` on the QA gears: resolution is lazy, so plugin `init` may run after theirs. No `capabilities = [db]`: this plugin owns no tables.

- [ ] **Step 2: Wire the feature and the inventory import**

`Cargo.toml`: `qa-vhp-product-plugin = { path = "...", optional = true }` and add it to the `qa-platform` feature list.

`registered_gears.rs`: `use qa_vhp_product_plugin as _;` under the same `#[cfg(feature = "qa-platform")]` shape as its neighbours. **Without this import the gear never registers via inventory** and the config stanza alone links nothing — the file's own comment on `oidc_authn_plugin` explains the failure mode.

- [ ] **Step 3: Add the config stanza** to `config/qa-platform.yaml`:

```yaml
  qa-vhp-product-plugin:
    config:
      vendor: "virtuozzo-vhp"
      priority: 100
```

- [ ] **Step 4: Write a boot test**

Assert that a server built with the `qa-platform` feature registers exactly one `QaProductPluginV1` under `cf.core._.vhp_product.v1`, and that `try_get_scoped` resolves it.

- [ ] **Step 5: Verify and commit**

Run: `cargo test --workspace` and `cargo build -p cf-gears-example-server --features qa-platform`.

```bash
git add -A
git commit -m "feat(qa-vhp-product-plugin): register the gear and wire it into the server"
```

---

# Phase D — Expand

Additive only. Old columns keep their values and old readers keep working; the new path writes alongside.

### Task 12: `QaProductRegistry` and `plugin_instance_id`

**Files:**

- Create: `qa-catalog/qa-catalog/src/domain/service/plugin_registry.rs`, `plugin_registry_tests.rs`
- Create: `qa-catalog/qa-catalog/src/infra/storage/migrations/m20260903_000003_product_plugin_instance.rs`
- Modify: `qa-catalog-sdk/src/models.rs` (`Product`), `domain/service/products.rs`, `infra/storage/entity/product.rs`, `api/rest/dto.rs`

**Interfaces:**

- Produces: `QaProductRegistry::plugin_for(&self, ctx, product_id) -> Result<Arc<dyn QaProductPluginV1>, DomainError>`; `Product::plugin_instance_id: String`.

- [ ] **Step 1: Migration** — add `plugin_instance_id TEXT` **nullable**, backfill every existing row to `cf.core._.vhp_product.v1`, leave it nullable until Task 20. Nullable-then-tighten is what keeps this step revertible.
- [ ] **Step 2: Write the registry**, mirroring `chat-engine`'s `PluginService::resolve`:

```rust
pub fn plugin_for(&self, ctx: &SecurityContext, product_id: Uuid)
    -> Result<Arc[[ORCA_RICH_MD:09e3bb8d68a75f1f4b44e3d9875e289d:inline-html:%3Cdyn%20QaProductPluginV1%3E]], DomainError>
```

Reads `plugin_instance_id` for the product, then `client_hub.try_get_scoped::<dyn QaProductPluginV1>(&ClientScope::gts_id(&id))`, returning `DomainError::NotFound { resource: "product plugin", id }` when absent. O(1).

- [ ] **Step 3: Tests** — resolves a registered plugin; `NotFound` for an unregistered id; `NotFound` for a product with a null column; tenant scoping is respected (a product in another tenant is not resolvable).

- [ ] **Step 4: Expose it** — `create_product`/`update_product` accept and validate `plugin_instance_id`; the DTO carries it. Note the existing `DECOMPOSITION.md` observation that these two take bare positional arguments; adding a sixth `String` to `update_product(ctx, id, name, key, description, folder)` makes the footgun materially worse, so **introduce `NewProduct`/`ProductUpdate` structs as part of this task** rather than adding the argument.

- [ ] **Step 5: Verify and commit**

```bash
cargo test -p qa-catalog
git add gears/qa-platform/qa-catalog
git commit -m "feat(qa-catalog): product plugin binding and the QaProductRegistry resolver"
```

---

### Task 13: `GET /qa/v1/product-plugins`

**Files:**

- Create: `qa-catalog/qa-catalog/src/api/rest/handlers/product_plugins.rs`, `routes/product_plugins.rs`
- Modify: `api/rest/dto.rs`, `api/rest/routes/mod.rs`, `api/rest/handlers/mod.rs`

**Interfaces:**

- Produces: `GET /qa/v1/product-plugins` → `[{ instance_id, vendor, credential_schema: [FieldDesc], observed_schema: [FieldDesc] }]`. Tasks 21–22 consume it.

- [ ] **Step 1: Write the handler test first** — one registered plugin yields one entry carrying both schemas; an empty registry yields `[]`, not a 404.
- [ ] **Step 2: Implement**, listing every `QaProductPluginV1` registered in the `ClientHub` and calling both schema methods.
- [ ] **Step 3: Regenerate the OpenAPI surface** — `make openapi`.
- [ ] **Step 4: Verify and commit**

```bash
cargo test -p qa-catalog && make openapi
git add -A && git commit -m "feat(qa-catalog): expose registered product plugins and their field descriptors"
```

---

### Task 14: Expand the `qa_environments` schema

**Files:**

- Create: `qa-environments/qa-environments/src/infra/storage/migrations/m20260903_000011_environment_plugin_columns.rs`
- Modify: `qa-environments-sdk/src/models.rs`, `infra/storage/entity/environment.rs`, `infra/storage/mapper.rs`

- [ ] **Step 1: Migration — additive only**

```
+ credentials         JSONB   NOT NULL DEFAULT '[]'
+ observed_attrs      JSONB   NOT NULL DEFAULT '{}'
+ observed_base_url   TEXT    NULL
+ health_state        TEXT    NOT NULL DEFAULT 'unknown'
+ health_detail       TEXT    NULL
+ health_checked_at   TIMESTAMPTZ NULL
```

`observed_version` and `observed_build` already exist and are reused as-is — they become role projections in Task 15 without a schema change.

Backfill in the same migration: `credentials = [{"key":"kubeconfig","credstore_ref":<kubeconfig_credstore_ref>}]`, `observed_base_url = vhp_base_url`, and `health_state` derived from the existing `cluster_*` columns. **Nothing is dropped.**

- [ ] **Step 2: Add the SDK fields** alongside the existing ones, each documented as "populated by the plugin path; the legacy column beside it is still authoritative until the contract migration".

- [ ] **Step 3: Test the round-trip** on all three dialects, and assert the backfill maps a representative existing row correctly.

- [ ] **Step 4: Verify and commit**

```bash
cargo test --workspace
git add gears/qa-platform/qa-environments
git commit -m "feat(qa-environments): expand schema with plugin-shaped credentials, attrs and health"
```

---

### Task 15: Observation through the plugin, dual-writing

**Files:**

- Modify: `qa-environments/qa-environments/src/domain/service/environments.rs` (the observation cycle), `domain/ports/` (new `ProductPluginPort`)
- Create: `domain/service/observation_projection_tests.rs`

- [ ] **Step 1: Write the failing tests**

- `the_cycle_resolves_the_plugin_for_the_environments_product` — a fake registry returning a fake plugin is called once per environment.
- `role_projection_writes_the_new_columns` — a plugin returning `platformVersion=9.2`, `build=1471`, `baseDomain=https://sv.jele.io` produces `observed_version=9.2`, `observed_build=1471`, `observed_base_url=https://sv.jele.io`.
- `the_legacy_columns_are_written_identically` — the same cycle still writes `vhp_base_url` and `observed_namespace` with the same values. This is the dual-write, and it is what makes Phase D revertible.
- `a_failed_observation_persists_only_classified_text` — `PluginFailure` with a `remote_message` writes `health_detail`; the canary never appears.
- `not_attempted_writes_no_health_columns` — including `health_checked_at`, because "never checked" is the truth when nothing looked.

- [ ] **Step 2: Implement.** The cycle keeps its existing shape — `list_all_with_tenant(allow_all)`, then a tenant-bound `SecurityContext` per environment (`system_actor::for_observation`) — and replaces only the observer call with: resolve plugin → resolve credentials from credstore → `plugin.observe(handle)` → `project_roles(plugin.observed_schema(), &attrs)` → write both column sets.
- [ ] **Step 3: Verify** — `cargo test -p qa-environments`, and confirm the pre-existing observation tests still pass against the old path where it is still exercised.
- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-environments
git commit -m "feat(qa-environments): observe through the product plugin, dual-writing legacy columns"
```

---

# Phase E — qa-runs

**Task 16 is a hard gate.** It must be complete and green before Task 18 touches `build_spec`, and it is the only thing that makes Phase F safe to walk through.

### Task 16: The golden `RunSpec` test

Written against **current** behaviour, before any dispatch change. Its whole value is that it was recorded before the code moved.

**Files:**

- Create: `qa-runs/qa-runs/tests/golden_run_spec.rs`
- Create: `qa-runs/qa-runs/tests/fixtures/vhp_run_spec.json`

- [ ] **Step 1: Build the fixture**

Assemble a `Run` for a VHP environment exercising every branch `build_spec` has: a target environment with a kubeconfig ref, an `observed_base_url`, an `observed_namespace`, one pipeline variable, one environment variable, one run parameter that shadows a variable, a multi-node file grouping across two repositories, and a non-empty `app_version`/`app_build`.

- [ ] **Step 2: Write the test**

```rust
/// Freezes the exact RunSpec today's code produces for a representative VHP
/// run. Tasks 17-19 move where every one of these values comes from; none of
/// them may change what it *is*.
///
/// If this test fails after a plugin change, the plugin changed behaviour.
/// Do not re-record the fixture to make it pass — that discards the only
/// evidence that the migration was faithful.
#[tokio::test]
async fn a_vhp_run_spec_is_byte_identical_to_the_recorded_fixture() { /* ... */ }
```

Serialise the `RunSpec` with sorted keys and compare to the fixture, asserting on the pretty-printed diff so a failure is readable.

- [ ] **Step 3: Record the fixture** by running once with `UPDATE_GOLDEN=1`, then **inspect it by eye**. Confirm `E2E_VHP_BASE_URL`, `VPADM_BASE_DOMAIN`, `E2E_K8S_NAMESPACE`, `KUBECONFIG`, `TEST_FILES`, `TEST_BUNDLE_URL`, `TEST_VERSION`, `APP_VERSION`, `APP_BUILD` are all present with the values you expect. A fixture recorded from a bug freezes the bug.

- [ ] **Step 4: Verify it fails when it should** — temporarily change one variable's value in `runvars.rs`, confirm the test fails, revert.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-runs/qa-runs/tests
git commit -m "test(qa-runs): freeze the VHP RunSpec before the plugin migration

Recorded against pre-migration code. This is the guard for the whole product
plugin change: every later task must leave it byte-identical."
```

---

### Task 17: `RunAccess` in the executor port

**Files:**

- Modify: `qa-runs/qa-runs/src/domain/ports/run_executor.rs`, `infra/executor/argo/workflow.rs`, `infra/executor/mock.rs`

- [ ] **Step 1: Replace `KubeconfigMount` with `RunAccess`** (re-exported from `qa-product-sdk`), keeping `KubeconfigMount` as a deprecated alias for one commit so the change is reviewable in isolation.
- [ ] **Step 2: Teach the Argo executor to render `Vec<MountSpec>`** — `MountSpec::Secret` becomes the existing `secret` volume + `volumeMount`; `MountSpec::ConfigValue` becomes a generated `Secret` via the same path. `service_account` sets `serviceAccountName`, and `RunnerSpec::image` falls back to `argo.runner_image` when `None`.
- [ ] **Step 3: Assert the golden test still passes** — nothing about the produced workflow may change yet.
- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/qa-runs
git commit -m "refactor(qa-runs): generalise the executor port from KubeconfigMount to RunAccess"
```

---

### Task 18: `build_spec` dispatches through the plugin

**Files:**

- Modify: `qa-runs/qa-runs/src/domain/service/dispatch_spec.rs`, `domain/runvars.rs`, `domain/params.rs`

- [ ] **Step 1: Delete the inline block.** `build_spec`'s `platform_base_url = platform.vhp_base_url` / `platform_namespace` / `KubeconfigMount` construction is replaced by three calls: `prepare_run_access`, `runner`, `env_contract`.
- [ ] **Step 2: Move the four frozen names out of `runvars.rs`.** `PLATFORM_BASE_URL_VAR`, `BASE_DOMAIN_VAR`, `NAMESPACE_VAR`, `KUBECONFIG_VAR` are now the plugin's (Task 10). `runvars::assemble` gains a `plugin_env: Vec<RunVar>` input occupying tier 4 — exactly the position `platform_base_url` held.
- [ ] **Step 3: Union the reserved sets.** `params::RESERVED_NAMES` becomes the platform floor; `validate` takes the union with `RunVarContract::reserved`. Add a test that a plugin **cannot shrink** the floor: a contract reserving nothing still refuses `TEST_FILES`.
- [ ] **Step 4: Run the gate**

Run: `cargo test -p qa-runs`
Expected: **the golden test passes byte-identically.** If it does not, the migration changed behaviour — find out why before continuing. Do not re-record.

- [ ] **Step 5: Full workspace gate, then commit**

```bash
cargo test --workspace
git add gears/qa-platform/qa-runs
git commit -m "feat(qa-runs): dispatch through the product plugin

The golden RunSpec fixture is unchanged: same variables, same values, same
precedence. Only their source moved."
```

---

# Phase F — Contract

**This is the one-way door.** Do not start it until Tasks 16 and 18 are green and the golden fixture is unmodified since Task 16 recorded it.

**Task order in this phase is 18b, then 20, then 19 — NOT the order they appear below** (ruling F-6). Task 18b was added at Phase F open (user decision U7) and is Task 19's precondition: Step 2 cannot drop `kubeconfig_credstore_ref` until something writes the column that replaces it.

**Task 20 moves ahead of Task 19 for a second reason.** Task 18b can only classify an environment's credentials through its product's plugin, and `product_id` is `Option<Uuid>` today — a productless environment is creatable and simply unobservable (`environments.rs:736` already refuses to observe one). Running Task 20's `product_id NOT NULL` **before** the drop means every surviving row has a product, therefore a plugin, therefore a classifiable and populated `credentials` — which removes the productless case from the one-way door's blast radius entirely. It also puts the irreversible task last, where it belongs: `NOT NULL` can be relaxed, a dropped column cannot be un-dropped.

**Migration numbers follow the run order, not the task numbers.** `qa_environments`' last applied migration is `m20260903_000011_environment_plugin_columns`, and Phase F now runs **18b → 20a → 19 → 20b**, so:

| runs | task | migration |
|---|---|---|
| 1st | 20a | `qa-catalog`'s `m20260903_000004_plugin_instance_id_not_null` (**landed**) |
| 2nd | 19 | `qa-environments`' `m20260903_000012_drop_legacy_platform_columns` |
| 3rd | 20b | `qa-environments`' `m20260903_000013_environment_product_required` |

These numbers were assigned twice before and both earlier arrangements are wrong: ruling F-6 gave the drop `000013` and the environments constraint `000012` (Task 20 before Task 19), and ruling F-12 then split Task 20 and moved its environments half *after* the drop. **The table above is the current answer** — check it against the migrations directory rather than against any earlier sentence in this plan.

**Within Task 20, Step 3 runs before Step 2** (finding FW-1 of the Phase E review: `create_product` still mints `plugin_instance_id: NULL` rows through the shipped UI, so the `NOT NULL` migration fails on real data unless the API guard lands first).

### Task 18b: The plugin-driven credential write path

> **Added 2026-09-04 by user decision U7, at Phase F open.** This is the task
> item 5 of Task 19's warning calls "a task no part of this plan currently
> owns", and since Task 18 made `qa-runs` a production reader of
> `kubeconfig_credstore_ref` (ruling E-17) it is the **precondition for Task 19
> dropping that column at all**. Three options were put to the user — a partial
> drop that keeps the column, building this task first, or stopping at Phase E.
> The user chose to build it. Task 19 therefore runs **unamended**.
>
> **It dual-writes, and that is what makes it safe** (ruling F-1). The new path
> populates `credentials` and `config` *and keeps writing*
> `kubeconfig_credstore_ref`. So nothing breaks the moment this lands, every
> write path maintains `credentials` from here on, and Task 19's re-derivation
> only has to cover rows written **before** this task — for which the legacy
> column is authoritative, so overwriting from it is correct. That single
> statement fixes both bad states item 5 names: the *empty* `credentials` of a
> row created since Task 14, and the *stale* `credentials` of a row backfilled
> before a kubeconfig rotation.
>
> **The word "unconditional" stood here and was wrong** (pre-Task-19 review,
> CRITICAL-1): the legacy column can name only the *sole required secret*, so
> on a row with more than one stored credential an unconditional overwrite
> deletes the others. Task 19 Step 2 carries the pre-check and the restriction
> that answer it.
>
> **It reaches `qa-runs` too, and that is the point** (ruling F-2). Both
> `resolve_credential_slots` (qa-environments) and `plugin_dispatch` (qa-runs)
> read the legacy column today and neither reads `credentials`, because when
> rulings E-17/D-19 were taken nothing wrote it. After this task both **prefer
> `credentials` when non-empty and fall back to the legacy column when empty**,
> which is what leaves the legacy column with no required reader.

**Files:**

- Modify: `qa-environments/qa-environments-sdk/src/models.rs`, `.../src/lib.rs`
- Modify: `qa-environments/qa-environments/src/api/rest/dto.rs`, `.../api/rest/handlers/environments.rs`
- Modify: `qa-environments/qa-environments/src/domain/service/environments.rs`
- Modify: `qa-environments/qa-environments/src/domain/repos/environments_repo.rs`, `.../infra/storage/environments_sea_repo.rs`
- Modify: `qa-environments/qa-environments/src/infra/storage/mapper.rs` (the `environment_credentials` degrade decision, below)
- Modify: `qa-runs/qa-runs/src/domain/service/dispatch_spec.rs`
- Create: `qa-environments/qa-environments/src/domain/service/environments_credentials_tests.rs`

- [ ] **Step 1: `KubeconfigMaterial` → `CredentialMaterial`, and the generic input channel.**

Rename the type (ruling F-3 — measured 39 refs in 9 files, all inside
`qa-environments`). Keep its redacting hand-written `Debug` and **keep the
2026-08-28 leak citation in its doc verbatim**, per Global Constraints.

Add to `qa-environments-sdk`:

```rust
/// One submitted credential, in the shape the caller supplied it.
///
/// The two arms are mutually exclusive *structurally*, which is the point:
/// the legacy `kubeconfig`/`kubeconfig_credstore_ref` pair needed a validator
/// to establish "exactly one of", and this cannot express the violation.
pub enum CredentialSubmission {
    /// A pasted document, to be written to credstore under a generated
    /// reference this gear then owns.
    Material(CredentialMaterial),
    /// A credstore reference the caller already holds. Never written, and
    /// never deleted by this gear — `forget_owned_secret`'s ownership rule.
    Reference(String),
}
```

`NewEnvironment` and `EnvironmentPatch` each gain
`pub credentials: BTreeMap<String, CredentialSubmission>`, keyed by the
plugin's own `FieldDesc::key`. On a patch, an absent key means "not
mentioned"; a present key means "replace".

The legacy `kubeconfig` and `kubeconfig_credstore_ref` input fields **stay**
for this task — the shipped UI sends them until Task 22 — and the *service*
desugars them, because the DTO layer does not hold the plugin. See Step 3.

- [ ] **Step 2: the wire shape, for *n* credentials.**

`CreateEnvironmentRequestDto` and `UpdateEnvironmentRequestDto` gain
`pub credentials: Option<BTreeMap<String, CredentialSubmissionDto>>` with

```rust
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSubmissionDto {
    Material(String),
    Reference(String),
}
```

externally tagged, so the wire reads
`{"credentials": {"kubeconfig": {"material": "apiVersion: v1\n..."}}}`.

**`Debug` is hand-written and redacts `Material`**, exactly as the existing
DTOs redact `kubeconfig` (`dto.rs:290-293` is the precedent to copy). A
derived `Debug` here is the 2026-08-28 leak with a new field name.

`credentials` is **not** published on `EnvironmentDto` — `dto.rs:141-144`
already drops the SDK model's `credentials` and `config` and carries the "do
not add it back" reasoning. Do not weaken that; the test at `dto.rs:648`
pins it.

- [ ] **Step 3: the service path — classify, then store.**

In `EnvironmentsService`, replacing `resolve_kubeconfig_source` /
`store_kubeconfig_source` as the credential half of create and update:

```rust
/// What one classified, stored credential form produced.
struct StoredCredentials {
    /// The `credentials` column: `{key, credstore_ref}`, never a value.
    credentials: Vec<EnvironmentCredential>,
    /// Merges into the `config` column — the `is_secret: false` fields.
    config: serde_json::Map<String, serde_json::Value>,
    /// The dual-write (ruling F-1): the ref for
    /// `sole_required_secret_key`'s key, or `""` for a plugin that declares
    /// no single required secret.
    legacy_ref: String,
    /// Every reference this call minted, for the compensating delete.
    minted: Vec<String>,
}

async fn classify_and_store_credentials(
    &self,
    ctx: &SecurityContext,
    plugin: &Arc<dyn QaProductPluginV1>,
    submitted: BTreeMap<String, CredentialSubmission>,
) -> Result<StoredCredentials, DomainError>
```

Order, preserving `create_environment`'s existing guarantee that "everything
that can reject the request has now run, so nothing rejected ever reaches
credstore":

1. **Resolve the plugin from `product_id`.** `product_id` is `Option<Uuid>`
   until Task 20, and a productless environment is creatable today — so
   **this task must not start refusing one** (ruling F-7). With no product
   there is no plugin and nothing can be classified, so the credential takes
   the pre-plugin path: mint the secret, write `kubeconfig_credstore_ref`,
   leave `credentials` empty, and `warn` once that the environment has no
   product and therefore no plugin-shaped credentials. The empty column is
   exactly what Step 5's fallback exists to serve.

   **This branch is dead the moment Task 20 lands** (`product_id NOT NULL`),
   which is why Task 20 now runs before Task 19 — see the Phase F header. Say
   so where the branch is written, and let Task 19 delete it with the column.
   Refusing here instead would move Task 20's behaviour change one task
   earlier and break a create the shipped UI can still make.
2. **Desugar the legacy pair.** If `kubeconfig` or
   `kubeconfig_credstore_ref` is present, convert it to a single entry keyed
   by `sole_required_secret_key(&plugin.credential_schema())` — the same
   derivation rulings E-17/D-19 and `resolve_credential_slots` already use,
   *not* the literal `"kubeconfig"`. If that returns `None`, refuse:
   `LEGACY_CREDENTIAL_UNBINDABLE` is the existing text for exactly this. If
   the legacy pair **and** a `credentials` entry for the same resolved key
   are both present, that is a 400 — two spellings of one field.
3. **Resolve every `Reference` arm to its bytes** before validating (ruling
   F-4). `fetch_credential_material` already does this and is already called
   on this request by D4's `materialise_runner_secret`, so no new class of
   exposure. A reference that does not resolve is a 400.
4. **`plugin.validate_credentials(&input)`.** `PluginFailure.detail` is
   `Option<&'static str>` — a compile-time constant, so it is safe to surface
   verbatim and it is the only credential-shaped text that is. Map the
   failure to a validation error carrying that `&'static str` and **nothing
   interpolated**.
5. **Reconcile classifications against submissions.**
   * A submitted key with **no** classification is **dropped, with one
     `warn` naming the key and never a value** (ruling F-5). This is the
     plugin contract's own choice — `qa-vhp-product-plugin`'s
     `validate_credentials` doc says undeclared keys are ignored because
     "rejecting the whole form over one is a worse failure than dropping
     it" — and the `warn` is what stops it being silent.
   * A classification for a key that was **not** submitted is a hard
     refusal: it would have the gear mint a secret out of nothing.
   * A `Reference` submission classified `is_secret: false` is a 400 — a
     config value has no credstore reference to name.
6. **Write.** `is_secret: true` → for `Material`, mint under
   `GENERATED_CREDENTIAL_REF_PREFIX` (below) and record it in `minted`; for
   `Reference`, use it as given with no ownership. Push
   `EnvironmentCredential { key, credstore_ref }`. `is_secret: false` → the
   submitted value goes into `config` under its key.

Generalise the ownership prefix, keeping its ownership-marker doc:

```rust
/// Prefix of every credstore reference **this gear generates**, and the
/// ownership marker `forget_owned_secret` tests. Was
/// `qa-environments-kubeconfig-`, which named one product's credential.
const GENERATED_CREDENTIAL_REF_PREFIX: &str = "qa-environments-credential-";
```

**A reference minted under the old prefix is still this gear's to delete.**
`forget_owned_secret`'s ownership test must accept **both** prefixes, or
every kubeconfig this gear minted before this task becomes un-deletable and
a rotation orphans it. Pin that with a test.

- [ ] **Step 4: the compensating deletes, both directions.**

The existing single-secret cleanup becomes an *n*-secret cleanup, and the
ordering guarantees are the ones `update_environment`'s doc already states:

* **Create, row write fails:** forget every reference in `minted`. The
  existing single-ref cleanup at `environments.rs:351` is the shape.
* **Update, row write fails:** same — forget every reference this call
  minted, leaving the environment exactly as it was.
* **Update, row write succeeds:** only *then* forget each **superseded**
  reference, and only where this gear owned it (either prefix, per Step 3).
  A key that was not mentioned in the patch supersedes nothing.

`patching_a_reference_over_a_generated_one_removes_the_superseded_secret` is
the existing test whose *n*-credential equivalent must exist.

- [ ] **Step 5: both readers prefer `credentials`** (ruling F-2).

`EnvironmentsService::resolve_credential_slots` and
`DispatchService::plugin_dispatch` each currently build their slot(s) from
`kubeconfig_credstore_ref` keyed by `sole_required_secret_key`. Both become:
**if `credentials` is non-empty, build one `CredentialSlot` per entry from
it; otherwise fall back to the legacy column exactly as today.**

Note what this fixes rather than documents: `resolve_credential_slots`'
`LEGACY_CREDENTIAL_UNBINDABLE` doc **already claims** the error is
unreachable "for any environment whose `credentials` column is populated",
which is false today because the column is never read. This task makes the
sentence true. Its "**One source, and it is the legacy column**" section must
be rewritten to say what the code now does, and the rewrite must keep the
*reason* the section exists — a pre-18b row's `credentials` can name a
superseded reference, which is why the preference is for non-empty and the
fallback still exists.

`plugin_dispatch`'s `AMBIGUOUS_CREDENTIAL` refusal stops being reachable for
an environment with populated `credentials`: *n* keys need no disambiguation.
Say so where the constant is defined; do not delete it, because the legacy
fallback still reaches it.

- [ ] **Step 6: decide the `mapper.rs` degrade, do not inherit it.**

`environment_credentials` degrades a corrupt `credentials` blob to an empty
list. Until this task that was harmless — nothing read the column. **From
this task it is load-bearing**: an empty list now means "fall back to the
legacy column", so a corrupt blob silently takes the legacy path, and after
Task 19 it means "no credentials at all". Task 19's warning says to decide
rather than inherit. **Decide here, where the first reader lands:** keep the
degrade and log it at `warn` with the environment id, distinguishing it in
the log from a legitimately empty column. Pin both arms with a test —
`mapper.rs:773` already has the corrupt-blob fixtures.

- [ ] **Step 7: tests.**

New file `environments_credentials_tests.rs`. Every one of these must be
written so that it **can fail** — mutate the thing it pins and watch it fail,
and check the mutation is not a no-op. Six could-not-fail assertions have
been found on this plan, four of them the controller's own, and the recurring
shape is pinning a value that is also the type's default.

* a pasted credential is minted under the new prefix, lands in `credentials`
  under the plugin's key, **and** dual-writes `kubeconfig_credstore_ref`
* a submitted reference lands in `credentials` and is **not** minted or owned
* an `is_secret: false` field lands in `config`, not in `credentials`, and
  not in credstore — VHP's `vpadm_namespace` is the real case
* a plugin rejection surfaces its `&'static str` detail and **no** submitted
  value; assert the absence by searching the error text for the submitted
  bytes
* an unclassified submitted key is dropped and warned, not stored
* **a create with no `product_id` still succeeds**, takes the pre-plugin path,
  leaves `credentials` empty, and is then served by Step 5's fallback — the
  regression guard on ruling F-7
* a classification for an unsubmitted key is refused
* a row-write failure forgets every minted reference, both on create and on
  update
* an update supersedes only the keys it mentions, and deletes only what this
  gear owns — including a reference minted under the **old** prefix
* `resolve_credential_slots` prefers `credentials` and falls back when empty;
  the same pair for `plugin_dispatch` in `qa-runs`
* the corrupt-blob degrade warns and is distinguishable from empty
* **the golden `RunSpec` fixture is byte-identical** — this task changes where
  dispatch reads its credential reference from, not what it renders. If the
  fixture moves, this task changed behaviour and that is a finding, not a
  re-record. (**E-11**: `mode: 0o400` is the only field it was ever
  re-recorded for.)

- [ ] **Step 8: gates and commit.**

Affected crates: `qa-environments`, `qa-runs`. `qa-runs` builds
`qa-environments` anyway, so this is one build graph.

```bash
export PATH="$HOME/.cargo/bin:$PATH"   # rustc must print 1.97.0
cargo test -p qa-environments -j 6
cargo test -p qa-environments --features platform-observation -j 6
cargo test -p qa-runs -j 6
cargo test -p qa-runs --features argo -j 6
cargo clippy -p qa-environments -p qa-runs --all-targets --all-features -j 6
RUSTDOCFLAGS="-D warnings" cargo doc -p qa-environments --no-deps -j 6
RUSTDOCFLAGS="-D warnings" cargo doc -p qa-environments --all-features --no-deps -j 6
RUSTDOCFLAGS="-D warnings" cargo doc -p qa-runs --no-deps -j 6
RUSTDOCFLAGS="-D warnings" cargo doc -p qa-runs --all-features --no-deps -j 6
cargo fmt -p qa-environments -p qa-runs --check
```

Both rustdoc forms are required and are blind to opposite things. The error
**set** must be identical to the saved baseline in
`.superpowers/sdd/2026-09-03-product-plugins/doc-baselines/` — compare
`grep '^error' <file> | sort`, not the files, because rustdoc's emission
order is not stable. Baselines: `qa-environments` 3 plain / 5 all-features,
`qa-runs` 27/27.

Test baselines to beat, not merely match: `qa-environments` 211 (234 with
`platform-observation`), `qa-runs` 869 (908 with `argo`).

```
git commit -m "feat(qa-environments)!: write credentials through the product plugin

validate_credentials -> CredentialClassification -> credstore + the
credentials column for secrets, the config column for the rest. The legacy
kubeconfig_credstore_ref is dual-written so qa-runs' dispatch keeps working,
and both readers now prefer the plugin-shaped column. This is the write path
Task 19's warning said no task owned, and its absence is why that column
could not be dropped."
```

---

### Task 19: Drop the legacy columns and delete the observer

> ### ⚠️ READ THIS FIRST — Task 19 is not a code-deletion task
>
> **It breaks four deploy artefacts that no task in Phase C touched, and only
> one of the four failures happens at build time.** The image build stops
> outright; the rest fail on the next remote deploy, or — worse — pass while
> asserting something that is no longer true. Nothing in `cargo test --workspace`
> or `cargo clippy --workspace` sees any of it. Every claim below was verified
> against the tree at `07727d5ac`; paths are relative to `gears/qa-platform/`.
>
> **1. `deploy/remote/verify-k8s.sh` check 9 fails hard.** The check (`step "9:
> the deployed binary was built WITH the platform-observation feature"`, line
> 610) reads two strings out of the deployed binary and requires
> `'built without the .platform-observation. cargo feature'` **== 0** (line 621)
> *and* `'observation ticker started'` **>= 1** (line 625). Both strings live in
> `qa-environments/qa-environments/src/gear.rs`: the first in the
> `#[cfg(not(feature = "platform-observation"))]` arm of `serve_with_services`
> (`gear.rs:232-246`), the second in the `#[cfg(feature = ...)]` arm
> (`gear.rs:283`). Step 3 as written deletes the `#[cfg]` blocks, so **both**
> strings go — and a zero count is not an escape hatch: `grep_count`
> deliberately does not gate on the exec's own exit status (`verify-k8s.sh:329-336`
> explains why), so the check really reaches its comparison at line 629 and
> really exits 1 at line 633.
>
> **There is no version of Step 3 that leaves check 9 with a correct outcome.**
> Delete the `#[cfg]` blocks and it fails. Keep the ticker and merely drop its
> `#[cfg]` attribute — which is what has to happen, because observation
> continues through the plugin — and it *passes*, while asserting that the
> binary carries a cargo feature that no longer exists. A check that passes for
> a reason that has ceased to exist is worse than one that fails: the failure
> gets investigated, the pass gets trusted. **Check 9 must therefore be
> rewritten or deleted, in this same commit — and Task 19 as written does not
> budget for that.** If it is to be rewritten, the fact worth checking after
> this task is that the binary carries the *product plugin* (a string from
> `qa-vhp-product-plugin`'s registration path), not that it carries a feature
> flag. Its FAIL text (line 632) also tells the operator to go fix
> `CARGO_FEATURES`, which after this task is exactly the wrong advice.
>
> **2. In the same script, the database facts that go are checks 11 and 12 — not
> check 10.** Step 2 drops the five `cluster_*` columns, and:
>
> * check 11 (`step "11: cluster health observation actually ran"`, line 664)
>   queries `cluster_status` (line 671);
> * check 12 (`step "12: the cluster_status_message leak canary (D-CH-5)"`, line
>   681) queries `cluster_status_message` (line 691).
>
> `psql` exits nonzero on a missing column and `psql_count` *does* gate on that
> exit status (`verify-k8s.sh:304-307`), so each becomes a hard
> `FAIL: could not query ...` followed by `exit 1`. Check 12 is a **security
> canary**; deleting the column it watches is fine, but leaving the canary
> pointing at a dropped column turns it into a check that can only fail, which
> is how a canary gets commented out.
>
> Check 10 (line 636) is the exception and is worth knowing precisely: it reads
> `qa_environments.version_detected_at` (lines 647, 651), which is **not** in Step
> 2's drop list, and which is written by `record_observation` — a repo call Task 15
> keeps (it replaces the observer call inside the cycle, not the write). So check 10
> keeps working, and its query keeps resolving. What it loses is its explanation: its
> FAIL text (line 660) blames `platform-observation` for a missing observation, and
> after this task that names nothing. Fix the wording, not the check.
>
> **3. `deploy/cargo-features.argo` and `deploy/docker/qa-platform.Dockerfile:48`
> both still name the feature, so the image build errors outright.** The
> Dockerfile's `ARG CARGO_FEATURES` default (line 48) and the one-line argo list
> both include `platform-observation`, and the Dockerfile feeds that list
> straight to `cargo build --release --bin cf-gears-example-server --features
> "$CARGO_FEATURES"` (line 135). Cargo rejects an unknown feature name rather
> than ignoring it — measured on this tree: `error: none of the selected packages
> contains this feature: <name>`. This is the loudest of the four and the easiest
> to forget, because nothing in the Rust workspace references either file.
>
> **4. `deploy/helm/tests/test_features.py` guards the ARG↔argo relationship, but
> in one direction only.** It asserts every feature in the Dockerfile ARG appears
> in `cargo-features.argo` (and that `qa-runs-argo` is in the argo list). So:
> removing the feature from the **argo list** and forgetting the **ARG** fails
> this test; removing it from the **ARG** and forgetting the **argo list** passes
> this test and then fails the `--argo` image build from (3). Change both files in
> the same commit and re-run the test either way.
>
> Also: `docs/FOOTPRINT-OUTSIDE-QA-PLATFORM.md:199` lists `platform-observation`
> among the example server's features. Prose, not a build input, but it is the
> document someone reads to answer "what does qa-platform touch outside itself".
>
> ### ⚠️ Added 2026-09-04 by Task 14: a fifth item, and this one loses data
>
> **5. Nothing writes `qa_environments.credentials` for an environment created
> after Task 14, so Step 2 must not drop `kubeconfig_credstore_ref` until
> something does.** Task 14 added the column and backfilled it from
> `kubeconfig_credstore_ref` for every row that existed when the migration ran.
> It deliberately did **not** populate it in `OrmEnvironmentsRepository::create`:
> that would have written the literal `"kubeconfig"` — one product's credential
> key — into the gear whose whole purpose is to stop naming VHP, and the first
> non-Kubernetes product's environments have no kubeconfig to name. So between
> Task 14 and this task, every newly created environment carries
> `credentials = []` and a populated `kubeconfig_credstore_ref`.
>
> Task 15 is unaffected (brief amendment E-5 already tells it to fall back to the
> legacy field, which is exactly this case). **This task is not.** Dropping
> `kubeconfig_credstore_ref` while `credentials` is empty leaves those
> environments with no credential reference anywhere, and Step 2's `down` — a
> "best-effort restore that repopulates from `credentials`" — cannot recover what
> was never written. Two things close it, and the task needs both:
>
> * the create/patch path becomes **plugin-driven** — the plugin classifies its
>   own fields through `CredentialClassification` (`secret` -> credstore and
>   `credentials`, `config` -> the `config` column), which is the shape §5's
>   `validate_credentials` was designed for and which **no task in this plan
>   currently owns**; and
> * this task's own migration re-runs Task 14's backfill for the rows created
>   since, before the `DROP` — one more one-time backfill, legitimate for
>   `m20260903_000011`'s reason.
>
> Neither is budgeted in Steps 1-5 as written. Check `credentials = '[]'` row
> counts on a real deployment before running the drop.
>
> **And an empty `credentials` is not the only bad state — a *stale* one is
> worse.** `m20260903_000011`'s backfill is a one-time snapshot;
> `kubeconfig_credstore_ref` stays patchable, and replacing a kubeconfig
> **deletes** the superseded secret when this gear minted it
> (`forget_owned_secret`). So a row backfilled before a rotation names a
> reference that no longer resolves — or, for an operator-supplied reference,
> resolves to *stale material*, which decision D4 exists to prevent. Task 15
> therefore does not read that column at all: it resolves the legacy reference
> and takes the credential *key* from the plugin's own `credential_schema()`
> (`EnvironmentsService::resolve_credential_slots` documents why). This task's
> re-derivation must cover both states, not just the empty one.
>
> Related, and cheap to fold in: `infra/storage/mapper.rs`'
> `environment_credentials` degrades a corrupt blob to an empty list, so after
> the drop a corrupt row and a never-written one are indistinguishable and the
> failure is silent. Either error there once the legacy fallback is gone, or
> keep the degrade and log it at `warn` with the environment id — but decide,
> rather than inheriting the Phase D behaviour by accident.
>
> **DISCHARGED by Task 18b Step 6**, which is where the column's first reader
> lands and so where the decision became load-bearing. Kept as a degrade plus a
> `warn` naming the environment id. Nothing left for this task.

> ### ⚠️ Added 2026-09-04 by Task 18: a sixth item — the drop now breaks
> ### DISPATCH, not only observation
>
> **`qa-runs` reads `kubeconfig_credstore_ref` in production as of Task 18.**
> `DispatchService::plugin_dispatch` builds the run's single
> `CredentialSlot::reference_only` from that column, keyed by the plugin's own
> `sole_required_secret_key(credential_schema())` — ruling **D-19/E-17**, the
> same choice Task 15 made in `qa-environments`, and for the same reason:
> nothing writes `credentials`, so it goes stale on the first kubeconfig
> rotation and a rotation *deletes* the superseded secret.
>
> Measured at `ad00b50d5`: `grep -rln kubeconfig_credstore_ref --include=*.rs`
> reaches **three** gears, not one — `qa-environments` (as before), `qa-runs`
> (7 files, of which `domain/service/dispatch_spec.rs` is production), and
> `qa-insights`, whose two hits are a test fixture and a doc comment and are not
> readers.
>
> So item 5's blast radius is bigger than item 5 says. Dropping the column with
> `credentials` still empty does not merely leave an environment unobservable:
> **every run against every environment fails to dispatch**, with
> `plugin_dispatch`'s classified text, because `prepare_run_access` is handed a
> slot whose reference is the empty string. The two things item 5 requires
> before the drop are therefore not optional hardening — they are the
> precondition for runs continuing to work at all, and the plugin-driven
> credential write path is still the task **no part of this plan owns**.
>
> Also fold in, since Task 18 makes it reachable: `plugin_dispatch`'s
> `AMBIGUOUS_CREDENTIAL` refusal exists precisely because one column cannot key
> two required secrets. Whatever replaces the column has to key *n*, and that is
> the same interface `CredentialClassification` was designed for.

> ### ⚠️ Added 2026-09-04 by Task 18b's re-review: a SEVENTH item — this task
> ### inherits a decision Task 18b deliberately reversed
>
> **After the drop, the plugin-unavailable credential path has no column to
> fall back to.** Task 18b's write path resolves an environment's product
> plugin to classify its credentials, and when the plugin cannot be resolved
> — `PluginUnavailable::ResolverAbsent` is literally "qa-catalog is not running
> beside this gear" — it falls back to writing `kubeconfig_credstore_ref`
> alone, leaving `credentials` empty for the readers' fallback to serve.
>
> Task 18b first made that case **refuse** the write, and reverted it: this
> gear's `ProductPluginPort` is designed to treat an unavailable plugin as a
> recorded fact about the environment rather than a failure, and refusing every
> credential-bearing write while a sibling gear restarts contradicts that. Six
> existing tests said so. The independent reviewer, having originally leaned
> toward refusing, agreed the reversal was right.
>
> **That reasoning depended on the fallback column existing, and this task
> removes it.** Two things follow and neither is optional:
>
> * `credentials::store_legacy_pair` writes only the dropped column, so it
>   cannot survive this task in its current form — it will stop compiling;
> * whatever replaces it **is** the decision Task 18b reversed.
>
> ### ✅ DECIDED — ruling F-13: the branch REFUSES. This item is closed.
>
> Taken deliberately before any code, as this item demands, and **not** left to
> a build error. A credential can only be stored keyed; the only source of a key
> is the plugin's own `credential_schema()`; with the column gone there is
> nowhere else to put it. Inventing a key would put one product's credential
> name back into the gear built to stop naming it, and accepting the write while
> dropping the credential is worse than refusing.
>
> **The cost is much smaller than the reversal assumed**, and the thing that
> shrank it landed in the same fix wave: Critical C-1's guard already refuses
> `store_unclassified_patch` on any row with a populated `credentials`, and
> Step 2 populates it on every row that has a legacy reference. So on the update
> path the branch is *already* a refusal; on create there was never a row to
> fall back to. The pre-Task-19 review confirmed this independently with a probe
> (finding IMPORTANT-3). What this task changes is which message the caller gets.
>
> **Two consequences this task owns**, carried from the re-review (R2-4):
>
> * the refusal is reached on **all three** `PluginUnavailable` causes, so all
>   three `detail()` texts become operator-facing on the write path. Two are
>   wrong as they stand: `Unresolvable`'s says "names no product plugin", which
>   Task 20a made impossible, and the wrapper "retry once it is available" is
>   right for `ResolverAbsent` but wrong for a configuration fault no retry
>   clears. Fix the wrapper per cause and the `detail()` text with it;
> * `the_two_pre_plugin_routes_give_two_different_remedies` covers only
>   `ResolverAbsent` on route 2. Add the other causes when their text settles.
>
> ### The asymmetry, and the one-task correction to it
>
> Easy to conflate: the **productless** branch dies at Task 20b
> (`product_id NOT NULL`), and the **plugin-unavailable** branch does not —
> `NOT NULL` says nothing about whether a product's plugin resolves.
>
> **But Task 19 runs before Task 20b and takes the same column away from
> both.** The productless branch cannot store a credential after the drop
> either, so this task makes **both** refuse, with two messages because they
> have two remedies. Task 20b then deletes the productless one as unreachable,
> which is what its Step 2 already says it does — the only change is that what
> it deletes is a refusal rather than a write path.

**Files:**

- Create: `qa-environments/.../migrations/m20260903_000012_drop_legacy_platform_columns.rs` — the next free number, because this is now the next `qa-environments` migration to run (rulings F-6 then F-12 moved it twice; see the Phase F header's table)
- Delete: `qa-environments/qa-environments/src/infra/observer/` (whole directory), `domain/ports/platform_observer.rs`
- Modify: `qa-environments/Cargo.toml`, `apps/cf-gears-example-server/Cargo.toml`, `qa-environments-sdk/src/models.rs`
- Modify (deploy, per the warning above): `deploy/cargo-features.argo`, `deploy/docker/qa-platform.Dockerfile`, `deploy/remote/verify-k8s.sh`
- Modify (both halves of the byte-identical pair, per Step 3b): `config/qa-platform-stack.yaml`, `deploy/helm/qa-platform/files/qa-platform-stack.yaml`

- [ ] **Step 1: Confirm the guard.** `git diff --stat HEAD~N -- gears/qa-platform/qa-runs/qa-runs/tests/fixtures/vhp_run_spec.json` must be empty since Task 16. If the fixture moved, stop and find out why.
- [ ] **Step 2: Re-derive `credentials`, then drop** `kubeconfig_credstore_ref`, `vhp_base_url`, `observed_namespace` and the five `cluster_*` columns.

**The re-derivation overwrites from `kubeconfig_credstore_ref` rather than filling only empty rows** (ruling F-1, and this supersedes item 5's "re-runs Task 14's backfill for the rows created since"). Task 14's `m20260903_000011` filled `credentials` only `WHERE credentials = '[]'`, which leaves the *stale* state item 5 warns about — a row backfilled before a kubeconfig rotation names a superseded reference this gear may have deleted outright. The legacy column is the value every write path maintained right up to Task 18b, so overwriting from it fixes the empty state and the stale state in one statement. The key is the literal `'kubeconfig'`, on `m20260903_000011`'s own precedent and for its recorded reason: every row this touches was created in the era when VHP was the only product.

**It is NOT unconditional, and the word cost a Critical** (pre-Task-19 review, CRITICAL-1; ruling F-15). F-1 justified "unconditional" with "Task 18b's dual-write means every row written since 18b already agrees with it" — **false for any row with more than one stored credential.** `legacy_reference` writes the legacy column from the plugin's *sole required secret* only, so:

* a row with **one required secret plus any other stored secret** has *n* entries in `credentials` and one of them in the legacy column. An unconditional overwrite replaces *n* with **1**, deleting every other binding and orphaning its credstore secret with nothing able to name it — permanently, because the source column is dropped in the same migration;
* a row with **two or more required secrets** has `sole_required_secret_key() == None`, so the legacy column is `''` while `credentials` is valid. An unconditional overwrite writes `[{"key":"kubeconfig","credstore_ref":""}]` — the credential-less row **Critical C-2 was raised to prevent**, produced by the migration meant to repair it.

Neither shape is reachable with the one plugin this tree ships, which declares a single required secret. **That is an accident of there being one plugin, which is the thing this branch exists to stop being true**, and both shapes are already exercised by tests written for Task 18b. So Step 2 does two things instead:

**The predicate is by KEY as well as by cardinality**, and the two halves have to agree with each other — the first attempt at this failed both tests (re-review findings NEW-1 and NEW-4). Every row falls in exactly one of four buckets:

Write `N` for `json_array_length(credentials)`, `K` for the single entry's key, `R` for its `credstore_ref` and `L` for `kubeconfig_credstore_ref`.

| # | shape | action | why |
|---|---|---|---|
| 1 | `N=0`, `L≠''` | **overwrite** | the *empty* state item 5 names — a row whose credential was written by a path that could not key it: the pre-plugin era before Task 18b, and 18b's own productless and plugin-unavailable branches. Guarded — see below |
| 2 | `N=1`, `K='kubeconfig'`, `L≠''`, `R≠L` | **overwrite** | the *stale* state — `m20260903_000011`'s one-time backfill, snapshotted before a rotation that wrote `L` and deleted the superseded `R`. **This is the case ruling F-1 exists for** |
| 3 | `N=1`, `K='kubeconfig'`, `L≠''`, `R=L` | **skip** | already agrees with the column being dropped; nothing to do |
| 4 | `N=0`, `L=''` | **skip** | legitimate for a plugin that declares no required secret (`require_declared_secrets`' own doc says so). Nothing to derive, and manufacturing an entry that points at nothing is what ruling F-14 refuses |
| 5 | `N=1`, `K≠'kubeconfig'`, `L≠''`, `R=L` | **skip** | complete and internally consistent under its own key. After the drop the plugin looks up `K` and finds it. Nothing to carry across, so nothing to do (re-review R2-1) |

and the pre-check **halts, naming every offending environment id** — in the shape Task 20a's `offending_ids` uses — on everything else:

* **H1** `N>1` — the legacy column can name only one of them (CRITICAL-1's first shape);
* **H2** `N>0` beside an **empty** `L` — `sole_required_secret_key()` returned `None`, so the plugin declares two or more required secrets (CRITICAL-1's second shape);
* **H3** `N=1`, `K≠'kubeconfig'`, `R≠L` — the two disagree and the migration cannot tell which is current, because resolving that needs the plugin's own key and a migration cannot call a plugin (NEW-4; narrowed by R2-1, which is bucket 5);
* **H4** `credentials` is **not a JSON array** — a corrupt blob is in no bucket above. `mapper.rs` deliberately degrades one to an empty list, so without this clause Postgres would die with a type error from `json_array_length` and SQLite would silently read it as `0` and overwrite the row (re-review R2-2). Finding I-8 said Task 19 must *decide* this rather than inherit it; halting and naming the row is the decision;
* **H5** a **bucket-1 row exists and the table holds more than one distinct `product_id`** — see below.

Note what H3 is *not*: "the two disagree" alone would halt on exactly the stale rows bucket 2 repairs, which is the contradiction NEW-1 caught — a stale row disagrees **by construction**, because that is what makes it stale.

**H5 is what guards bucket 1, and bucket 1 needs a guard because it has no key to check** (re-review R2-3). Buckets 2, 3 and 5 are safe because their key is *read from the row*; bucket 1 writes the literal `'kubeconfig'` onto a row that never carried a key, so it is the one place the "VHP era" premise is still assumed rather than enforced. The harm needs a product whose plugin's sole required secret is named something else — which needs a second plugin, the same reachability as CRITICAL-1 — and it is reachable through a branch that is live today: `store_unclassified_credential`'s plugin-unavailable arm, which ruling F-13 keeps until this very task, writes exactly a bucket-1 row. Such an environment **works** before this task, because both readers key the fallback through `sole_required_secret_key`, and would be **broken by** the overwrite.

So: if every environment in the table belongs to one product, that product's plugin is the only key-giver and the literal cannot be wrong for it — provably safe, and true of every deployment today. If there is more than one product, the migration stops and names the rows, and the operator clears them by re-saving those environments, which routes them through the plugin path that keys them correctly. **A cheap SQL predicate that enforces the premise, on a step that cannot be undone.**

#### Four things about writing this that the re-review measured (R3-1 … R3-4)

* **H4 must be its own query, and an earlier one.** `json_array_length` on a Postgres scalar *raises*, and SQL guarantees no evaluation order across `OR` — so as one disjunct beside the `N`-based conditions, Postgres can abort on the corrupt row before H4's own predicate is considered, which is the opaque failure H4 exists to replace. Either run it first and separately, or guard every `N`-based predicate with `jsonb_typeof(credentials) = 'array' AND …`.
* **Scope H5's count to bucket-1 rows, not the whole table.** Counting distinct products across every row halts a two-product deployment whose bucket-1 rows all belong to one of them; counting among bucket-1 rows only gives the identical guarantee with a narrower trigger. And pin the `NULL` handling deliberately: `COUNT(DISTINCT product_id)` ignoring `NULL`s is the behaviour wanted — a productless bucket-1 row has no plugin whose key could disagree with the literal — but an implementer reaching for `COALESCE` gets a different answer, on the one-way door.
* **Record what H5 does *not* cover.** Its premise proves "one product", not "one key-giver". A **rebind** (ruling F-10 permits one, and `an_update_that_names_a_plugin_rebinds_the_product` proves it lands) or a plugin **renaming** its sole required secret between releases both defeat it invisibly. Neither is closable from this gear — the only authority is `qa_products`, another gear's table — so it is an accepted residual, recorded rather than absorbed. It is also why bucket-1 rows are worth protecting at all: `dispatch_spec.rs` derives their key **live** from `sole_required_secret_key(&plugin.credential_schema())`, so those rows track the current plugin today, and the overwrite is what freezes them.
* **`environment_credentials.rs:316-318` still says "Task 20 now runs before Task 19"** — true under ruling F-6, false since F-12 split Task 20. Step 3 edits that doc comment anyway; correct it there.

Write `down` as a best-effort restore that repopulates from `credentials`/`observed_attrs`, and document that it cannot recover a value the plugin path never wrote.

**E-23 is discharged by this step, not carried:** `observed_namespace` (sticky) and `observed_attrs` (overwritten) could disagree, so a redeployed environment ran in the new namespace and displayed the old one. Dropping the column removes the divergence. Say so in the migration's doc — it is the answer to a question the code asked for one phase.
- [ ] **Step 3: Delete the directory and the port**, plus the `platform-observation` feature from both `Cargo.toml`s and the `#[cfg(feature = ...)]` blocks that reference it. **Then do the deploy half in the same commit** — `deploy/cargo-features.argo`, the Dockerfile ARG default, and checks 9/11/12 of `verify-k8s.sh` — per the warning at the top of this task. `python3 deploy/helm/tests/test_features.py` and a `cargo build --features "$(cat deploy/cargo-features.argo)"` are the two commands that prove that half.

- [ ] **Step 3b: Give `qa-vhp-product-plugin` a stanza in the gears' stack config.**

`gears/qa-platform/config/qa-platform-stack.yaml` has a block for all four QA gears (`qa-environments:` 560, `qa-catalog:` 600, `qa-runs:` 616, `qa-insights:` 660) and for both credstore plugins (`static-credstore-plugin:` 434, `postgres-credstore-plugin:` 490). The product plugin is the odd one out only because it needs no `database:` block — it owns no tables — so it has never needed a stanza to boot. Add one anyway, restating `vendor: "virtuozzo-vhp"` and `priority: 100` at their code defaults (`plugins/qa-vhp-product-plugin/src/gear.rs`' `DEFAULT_VENDOR`/`DEFAULT_PRIORITY`), the same way `qa-environments`' block restates `max_variables` purely so the mapping is not empty.

**This is not a boot failure and must not be sold as one.** `init` uses `ctx.config_or_default()`, so a missing section yields exactly those two values; the only thing gained is operator visibility — a deployment where every gear that has settings shows them in one file. Task 19 is the first change that makes a deployment care, because after it the plugin is the *only* path to an observation, and "which product plugin is this stack running, at what priority" stops being a rhetorical question.

**`config/qa-platform-stack.yaml` is one half of a byte-identical pair.** The chart's copy at `deploy/helm/qa-platform/files/qa-platform-stack.yaml` exists because Helm's `.Files` cannot read `../`, and `deploy/helm/tests/test_chart_file_sync.py` fails on any diff between them. **Edit both.** `python3 deploy/helm/tests/test_chart_file_sync.py` is the check.

- [ ] **Step 4: Prove the containment**

Run: `cargo tree -p qa-environments -i kube`
Expected: `error: package ID specification 'kube' did not match any packages`. Add this as a test in `qa-environments`' suite so it cannot regress — mirroring the existing test that fails the build if a deployment-specific address becomes a default again.

**Measured at Task 18b: that command ALREADY returns that error today**, because `platform-observation` is not a default feature. So the default-feature form proves nothing and a test asserting only it would pass before this task and after it, which is a could-not-fail assertion of the kind this plan has now found six of. The form that has content today is `cargo tree -p qa-environments --features platform-observation -i kube`, which prints `kube v3.1.0 └── qa-environments`; after this task deletes the feature, that invocation is an error about the *feature*, not about containment. **So the test must assert the `--all-features` form** — the one spelling that means "there is no way to turn `kube` back on" both before and after, and that fails today.

- [ ] **Step 5: Full gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets --all-features
git add -A
git commit -m "feat(qa-environments)!: drop the Kubernetes-shaped columns and delete infra/observer

ADR-0001's containment is now structural: kube is a dependency of
qa-connector-k8s and of nothing else, so the platform-observation feature has
nothing left to gate."
```

---

### Task 20: Tighten the constraints

**Files:**

- Create: `qa-catalog/.../migrations/m20260903_000004_plugin_instance_id_not_null.rs`
- (No `qa-environments` migration here — `product_id NOT NULL` moved to **Task 20b**, which runs after Task 19; see ruling F-12 and the Phase F header's table.)

- [ ] **Step 1: Verify no nulls remain** — a pre-migration check that fails loudly with the offending row ids rather than letting the `ALTER` fail opaquely.
- [ ] **Step 2: Set `qa_products.plugin_instance_id NOT NULL`** (**D6**) and make it non-optional in `qa-catalog-sdk`'s model.

**`qa_environments.product_id NOT NULL` (D9) moves to Task 20b, after Task 19** (ruling F-12). The two columns are independent and one collides with the drop: SQLite cannot `ALTER COLUMN ... SET NOT NULL`, so each arm must restate its table, and `qa_environments` is 25+ columns of which **Task 19 drops eight**. Writing that rebuild here means writing it twice, the second replacing the first — two hand-written copies of a wide table, the drift class this branch keeps getting bitten by. `qa_products` is nine columns and two unique indexes, so it is tractable now and its index survival is assertable.

Deployment is Postgres (`config/qa-platform-stack.yaml:60`); SQLite is the in-memory test backend, which is why its arm has to work at all.

**What ruling F-6 loses by this: nothing, and it was checked rather than assumed.** F-6 ordered Task 20 before Task 19 so that every surviving row would have a product, hence a plugin, hence a populated `credentials`. That implication turned out not to be needed — a productless row still carries a legacy reference (`store_legacy_pair` refuses an empty one on both accepting arms) and Task 19 Step 2 re-derives `credentials` **from that column**, so it lands a usable `credentials` on a productless row too. F-6's real purchase was "the irreversible task last", and the drop still runs after Task 20a.

That re-derivation is **not** unconditional, and this paragraph said it was until the pre-Task-19 review (CRITICAL-1). See Task 19 Step 2: it skips any row whose `credentials` already holds more than one entry, because the legacy column can only ever name one of them.
- [ ] **Step 3: Add the service-level guard** — creating a product with an unresolvable `plugin_instance_id` is rejected at the API, not at first use. **Runs before Step 2** (finding FW-1).

Two halves, and only the first blocks Step 2. Mechanism established by reading the code at Task 18b close, so this task need not rediscover it:

* **The blocking half is that `create_product` must refuse `plugin_instance_id: None`** (**D6**: every product names a plugin, no fallback path). No resolver is involved. `validate_plugin_instance_id` returns `Ok(())` for `None` (`validation.rs:74`), which is **correct for update** — ruling D-18, `None` means "leave the binding alone", because the shipped UI cannot send the field and full-replace semantics silently unbound every product it touched — and **wrong for create**, where there is no binding to leave alone. The single validator is shared by both paths (`products.rs:88` and `:120`), so splitting that rule is the fix. Until it lands, the shipped UI keeps minting `NULL` rows and Step 2's `NOT NULL` fails on real data.
* **"Unresolvable" needs only the `ClientHub`.** `QaProductRegistry::plugin_for` is a product read *plus* an O(1) `client_hub.try_get_scoped::<dyn QaProductPluginV1>(&ClientScope::gts_id(id))`. That second half alone answers "is this id registered in this binary?" with no product row and no repository, so `ProductsService` does **not** need the registry — which is generic over `ProductsRepository` and holds the repo, making that wiring circular-ish. It needs `Arc<ClientHub>`; the call sites are `gear.rs`' `ProductsService::new` and `test_support.rs`'.

**Settle this rather than assuming it:** an id that is well-formed but not registered *in this binary* is not necessarily a mistake — a deployment can be mid-rollout, and refusing it outright means a product cannot be bound until the plugin gear ships. Weigh that against **D6**. Either answer is defensible; record it as a ruling with its cost.
- [ ] **Step 4: Verify and commit**

```bash
cargo test --workspace
git add -A && git commit -m "feat(qa-catalog): require a product plugin per product"
```

---

### Task 20b: `qa_environments.product_id NOT NULL`

Split from Task 20 by ruling F-12, and **runs after Task 19** so the SQLite table rebuild is written once, against the post-drop column set, rather than twice.

**Files:**

- Create: `qa-environments/.../migrations/m20260903_000013_environment_product_required.rs` (follows Task 19's `000012`; see the Phase F header's table)

- [ ] **Step 1: Verify no nulls remain** — the same loud pre-check Task 20a uses, naming the offending environment ids. Note that `create_environment` still accepts a productless create at this point (ruling F-7 keeps that deliberately, because refusing it is Task 20's behaviour change and not Task 18b's), so **this check can genuinely fail on real data** and its message has to say what to do: assign the environment a product, or delete it.
- [ ] **Step 2: `product_id NOT NULL`** (**D9**), non-optional in `qa-environments-sdk`'s model, and the productless branches in `credentials::store_unclassified_credential` / `store_unclassified_patch` deleted — they become unreachable, and dead code is a defect. Task 19's seventh warning item covers the *other* pre-plugin branch (plugin-unavailable), which this does **not** make unreachable.
- [ ] **Step 3: Verify and commit** — `cargo test -p qa-environments -j 6`, and the SQLite rebuild must assert both that a `NULL` insert is rejected **and** that every index on the table survived the rebuild.

---

# Phase G — UI

### Task 21: Descriptor-driven environments table and detail page

**Files:**

- Rename: `qa-platform-ui/src/components/platforms/PlatformsTable.tsx` → `components/environments/EnvironmentsTable.tsx`; `src/pages/PlatformDetailPage.tsx` → `EnvironmentDetailPage.tsx`
- Create: `src/api/productPlugins.ts`, `src/lib/fieldDesc.ts`, `src/lib/fieldDesc.test.ts`
- Modify: `src/api/adapters.ts`, `src/api/types.ts`

- [ ] **Step 1: Write the failing tests** — `fieldDesc.test.ts`: fixed leading columns (name, product, health, availability) always render; one column renders per `in_table` descriptor, in declaration order; a descriptor whose attr is absent renders `—` rather than `undefined`; a `Secret`-kind descriptor never renders a value even if one somehow arrives.
- [ ] **Step 2: Fetch the schemas** — `productPlugins.ts` calls `GET /qa/v1/product-plugins`, cached per session; the table resolves the environment's product to its plugin's `observed_schema`.
- [ ] **Step 3: Replace the hardcoded columns.** "VHP URL" and "VHP Base URL" are gone; the copper-thread detail is that VHP's own table looks identical afterwards, because its plugin declares `baseDomain` with `in_table: true`.
- [ ] **Step 4: Verify and commit**

```bash
make ui-test
git add -A && git commit -m "feat(qa-platform-ui): render environment tables and detail pages from field descriptors"
```

---

### Task 22: Generated credential form, and de-VHP the UI

**Files:**

- Create: `src/components/environments/CredentialFields.tsx`
- Modify: `src/lib/selectedProduct.ts`, `selectedBranch.ts`, `theme-provider.tsx`, `components/filters/FqlQueryInput.tsx`, `components/layout/Sidebar.tsx`, `components/analytics/AnalyticsDashboard.tsx`, `pages/ProductsPage.tsx`, `ProductDetailPage.tsx`, `SettingsPage.tsx`, `notifications/NotificationsEmailPage.tsx`

- [ ] **Step 1: Generate the form** from `credential_schema()`. `MultilineSecret` renders the same textarea the kubeconfig field uses today, so VHP's create dialog is visually unchanged.

**A 400 an existing client may be keying on has already changed.** Since Task 18b, a create that supplies neither `kubeconfig` nor `kubeconfig_credstore_ref` — and no `credentials` — answers `field: "credentials"`, `message: "an environment needs at least one credential: supply the fields this product's plugin declares"`, where it used to answer `field: "kubeconfig_credstore_ref"`, `message: "must not be empty"`. The new message is deliberate: the old one named one product's credential in a message every product would see, which is the coupling this plan removes. It is recorded here because this is the task where the UI stops sending the legacy pair, and until then the shipped UI is the client that can still provoke it. (A create that supplies a credential the plugin does not declare, or only non-secret fields, now answers with the plugin's own missing required field — see Task 18b's `require_declared_secrets`.)

**The request shape is the one Task 18b Step 2 defined**, and this task is its first UI caller: `credentials` is a map keyed by `FieldDesc::key`, each value externally tagged as `{"material": "…"}` for a pasted value or `{"reference": "…"}` for a credstore reference the operator already holds — e.g. `{"credentials": {"kubeconfig": {"material": "apiVersion: v1\n…"}}}`. Task 18b left the legacy `kubeconfig`/`kubeconfig_credstore_ref` fields accepted precisely so this task could be the change that stops sending them; **this step is where they stop being sent**, and a follow-up may then delete them from the DTOs.
- [ ] **Step 2: Storage-key migration** — `vhp.selectedProduct` → `qa.selectedProduct`, `vhp.selectedBranch` → `qa.selectedBranch`, `vhp-theme` → `qa-theme`, `vhp:fql:saved:*` → `qa:fql:saved:*`. Write a one-time read-through that copies an old key's value on first read, so nobody loses saved filters. Test it.
- [ ] **Step 3: Remove the remaining hardcodes** — `AnalyticsDashboard`'s `key === 'VHP'` default becomes first-product-or-remembered; "VHP Test Manager v1.0.0" becomes product-neutral; the placeholder strings (`"VHP"`, `"vhp"`, `"VHP Core"`, `"qa/vhp-tests"`, `"vhp-tests@company.com"`) become generic.
- [ ] **Step 4: Full gate and commit**

```bash
make ui-test && make ui-lint && cargo test --workspace
git add -A && git commit -m "feat(qa-platform-ui): generated credential forms and product-neutral copy"
```

---

# ============================================================
# PHASE G — the hole the self-review below did not catch
# ============================================================

**Why this phase exists.** Tasks 12 and 20 made `plugin_instance_id` required —
at the API (`create_product` refuses `None`, ruling D-18 splits that rule from
update) and in the schema (`m20260903_000004`, `NOT NULL`). **No task ever gave
the UI a way to send it.** The plan states the gap twice as a *premise* for
other decisions — Task 20's "the shipped UI cannot send the field", and
`qa-catalog/src/api/rest/dto.rs:429`'s "The shipped UI cannot send this field
until Task 22" — and Task 22's four steps are the credential form, the
localStorage rename, the VHP hardcodes and the gate. None of them is a product
plugin selector.

The consequence, measured on a freshly wiped deployment: **`POST /qa/v1/products`
from the UI is a guaranteed 400.** Every product create fails with *"every
product must name a product plugin"*. Existing products are unaffected —
`m20260903_000003` backfills them and an update with `None` leaves the binding
alone — so the failure is invisible on any database that already had products,
and appears the moment someone starts clean.

**How it hid.** The self-review table below maps §3 D6 to "Tasks 12, 20", and
both do their half. The table asks which task covers a spec section; it does not
ask, for each new server-side requirement, *which task lets a user satisfy it*.
The pre-flight conflict scan has the same blind spot: it pairs tasks that exist,
and a consumer nobody wrote is not a pair. Task 24 is the deploy-time guard for
that class; the table row for D6 is corrected below.

---

### Task 23: The product-plugin selector

**Files:**

- Modify: `qa-platform-ui/src/api/types.ts`, `src/api/adapters.ts`,
  `src/pages/ProductsPage.tsx`
- Create: `src/pages/ProductsPage.test.ts`

- [ ] **Step 1: Carry the field.** `CreateProductForm` (`types.ts:638`) gains
  `plugin_instance_id: string`. `productReqFromForm` (`adapters.ts:1561`) sends
  it.

  **The generated schema is stale for this field, in both directions.**
  `generated/openapi.d.ts:2329` `CreateProductReq` has only
  `description`/`folder`/`key`/`name`, and `:4152` `ProductDto` has no
  `plugin_instance_id` either. Ruling **F-21** governs: declare an explicit
  intersection type with a comment and a branch-close item, exactly as
  `ProductDtoWithPlugin` already does at `adapters.ts:1543` — **never `as any`**,
  which would also swallow a genuinely misspelt field. `Product.plugin_instance_id`
  already exists on the UI type (`types.ts:629`) and `productFromDto` already maps
  it (`adapters.ts:1551`), so the read path needs nothing.

- [ ] **Step 2: The selector.** `ProductsPage.tsx` gains a `Combobox` fed by
  `useProductPlugins()` (`@/api/productPlugins`), in the same shape
  `CreatePlatformDialog.tsx:133-142` uses. Options are
  `{ value: plugin.instance_id, label: plugin.vendor ?? plugin.instance_id }` —
  `ProductPlugin.vendor` is `string | null` (`lib/fieldDesc.ts:41-46`), so the
  fallback is not optional. Placeholder `"Select a product plugin"`; **no empty
  option** — the same rule finding m-12 applied to the environment dialog.

  Four call sites move together: the form state at `:31`, both resets (`:36`,
  `:102`), and `openEdit` at `:45-48`, which pre-fills from the product's current
  `plugin_instance_id`.

- [ ] **Step 3: Create requires it, edit preserves it.** The submit guard at
  `ProductsPage.tsx:69` (`if (!formData.name || !formData.key)`) also requires
  `plugin_instance_id` **when creating**. On edit the field is pre-filled and
  sent as-is, so an unchanged edit rebinds to the same plugin and a changed one
  rebinds deliberately. Do **not** send `undefined` on create — that is the 400
  this task exists to end. Ruling **D-18**'s "`None` means leave the binding
  alone" stays true of the API and is simply not exercised by this form.

- [ ] **Step 4: Tests.** `ProductsPage.test.ts`, in the shape
  `EnvironmentsTable.test.ts` uses (`.test.ts` not `.test.tsx` —
  `vitest.config.ts`'s include glob is `src/**/*.test.ts`; `createElement`
  stands in for JSX):

  1. the create payload handed to `apiPost` carries the selected
     `plugin_instance_id`;
  2. submitting with no plugin selected does not call `apiPost` at all;
  3. opening the dialog on an existing product pre-selects that product's plugin.

  Test 1 pins an exact value — **mutate it** (drop the field from
  `productReqFromForm`) and watch it fail, and check the mutation compiles.

- [ ] **Step 5: Gate and commit**

```bash
make ui-test && make ui-build
git add -A && git commit -m "feat(qa-platform-ui): product plugin selector on the product form"
```

  `make ui-lint` is **not** in this gate — ruling **F-22**, it is unrunnable at
  the merge base. `cargo test --workspace` is not either; no Rust changes here.

---

### Task 24: `verify-k8s.sh` proves the plugin catalogue answers

**Files:** Modify `gears/qa-platform/deploy/remote/verify-k8s.sh`

The deploy audit found the catalogue route uncovered: check 9 proves the VHP
plugin is *linked into the binary*, and nothing proves the gear **registered**
it or that the route answers. That route is now the only way any client learns a
valid `plugin_instance_id`, so an empty catalogue makes every product create
impossible — the exact failure Task 23 fixes in the UI, undetected at deploy
time.

- [ ] **Step 1:** a new check, placed after check 9, that `GET /qa/v1/product-plugins`
  answers `200` with a **non-empty** array over `PUBLIC_ORIGIN`. Assert at least
  one entry carries a non-empty `instance_id`.

  **Ruling G-6 — how it authenticates, because the obvious reading is wrong.**
  An earlier draft of this step said "using the same bearer-token acquisition the
  existing authenticated checks use". *There are no authenticated checks.* Every
  HTTP probe in `verify-k8s.sh` is unauthenticated and asserts `401`, and check
  16's header records that a previous version DID acquire a token and was
  deliberately rewritten to stop: it used the **master-realm admin** credentials
  against `/admin/realms/...`, which forced the admin console and the whole admin
  REST API to be published on the application's public origin for one check's
  convenience.

  This check uses a different door, and the distinction is the ruling. Take a
  **client-credentials token for the `qa-platform-workflow` service-account
  client** from the realm's ordinary token endpoint
  (`/realms/qa-platform/protocol/openid-connect/token`). That is not the surface
  check 16 removed: the token endpoint is already published and already exercised
  by the discovery-document checks (6 and k8s 4/4), no admin API is involved, and
  the realm **pins that client's secret** (`qa-platform-workflow-dev-secret`) —
  which is precisely the property `keycloak-deployment.yaml`'s header says the
  pinned secret exists to guarantee across an H2 discard.

  **Three outcomes, distinguished.** A check that cannot tell them apart is worse
  than no check: (a) no token — report the token failure and say the catalogue was
  not reached, do not report an empty catalogue; (b) token but non-200 — report
  the status and body; (c) 200 with `[]` — that is the real failure this check
  exists for. Place it after Keycloak is known ready, since it needs a login flow
  where the neighbouring checks deliberately do not.
- [ ] **Step 2:** FAIL text that names the consequence in operator language — no
  plugin registered means no product can be created and no environment observed —
  and points at check 9 as the "is it even linked in" discriminator.
- [ ] **Step 3:** run `deploy/helm/tests/` if any cover this script; otherwise
  state that the check is verified by the next real deploy.

---

### Task 25: Finish the rename on the wire

**User decision, 2026-09-05.** Ruling B3 left this open in as many words:
*"RESIDUAL FOR THE USER: qa-runs' and qa-insights' physical columns keep the name
`platform_id` for the life of this plan, so spec §6.2's `qa_runs.environment_id`
is aspirational unless a task is added."* The user has asked for the wire to be
renamed. **This task renames the wire only. It renames no database column** —
B3's reasoning about the PK and its two named FK constraints stands untouched.

**Why that is coherent rather than a half-measure.** The entity fields are
already `environment_id`, pinned with `#[sea_orm(column_name = "platform_id")]`.
Today's arrangement makes wire == column with the Rust field as the one bridge
(`qa-runs/.../dto.rs:317-325` spells it out). After this task, Rust == wire ==
`environment_id` and the `column_name` attribute already present is the one
bridge — the ordinary arrangement, one fewer name to hold.

**This is a BREAKING API change**, and the first one this plan makes. It needs a
line in the deploy/upgrade notes.

**Files:**

- Modify: `qa-runs/qa-runs/src/api/rest/dto.rs` (6 fields: `:327`, `:544`,
  `:637`, `:858`, `:994`, `:1215`), `qa-runs/.../api/rest/routes/queue.rs:28`
  (the `platform_id` query param and its description at `:21`),
  `qa-insights/qa-insights/src/api/rest/dto.rs` (`:219`, `:391`, `:532`,
  `:1642`, `:2408`, plus `pub platform: Option<String>` at `:1645`,
  `pub platform: Vec<PlatformGroupSummaryDto>` at `:1681`, and the type
  `PlatformGroupSummaryDto` at `:1639`)

- [ ] **Step 1: Rename the wire fields and the query param.** `platform_id` →
  `environment_id`; `platform` → `environment`; `PlatformGroupSummaryDto` →
  `EnvironmentGroupSummaryDto`. Leave every `#[sea_orm(column_name =
  "platform_id")]` exactly as it is.

- [ ] **Step 2: Rewrite the two "wire name stays `platform_id`, deliberately"
  doc blocks** (`qa-runs/.../dto.rs:317-325`, `qa-insights/.../dto.rs:209-218`).
  They currently argue for the opposite of what the code will do. Replace them
  with what is then true: Rust and wire agree, the column keeps its name behind
  a `column_name` attribute, and the physical rename is still deferred under
  ruling B3. **A doc that argues for the code's opposite is this branch's
  signature defect — do not leave one behind.**

- [ ] **Step 3: The wire-shape tests.** Several tests pin the serialized name —
  `qa-runs/.../dto.rs:1832` (`serde_urlencoded` on `platform_id=…`), `:1878`,
  `:2055`; `qa-insights/.../dto.rs:2958`, `:3367`. Update each, then **mutate**:
  revert one field's rename and confirm its test fails.

- [ ] **Step 4: Upgrade note.** Add the breaking change to
  `gears/qa-platform/docs/` where the deploy notes live, naming every renamed
  field and query param, and saying that a client sending `platform_id` now gets
  a 400.

- [ ] **Step 5: Gate and commit**

```bash
cargo test -p qa-runs -j 6 && cargo test -p qa-insights -j 6
cargo clippy -p qa-runs --all-targets --all-features -j 6
cargo clippy -p qa-insights --all-targets --all-features -j 6
cargo fmt -p qa-runs --check && cargo fmt -p qa-insights --check
git add -A && git commit -m "refactor(qa-runs,qa-insights)!: rename platform_id to environment_id on the wire"
```

  Rustdoc gate: the baselines in
  `.superpowers/sdd/2026-09-03-product-plugins/doc-baselines/` are deliberately
  RED; the gate is that the error **set** is unchanged, compared as
  `grep '^error' <file> | sort`. Do NOT run `cargo test --workspace`.

---

### Task 26: The UI speaks Environment

**User decision, 2026-09-05:** the full rename, including the route, with a
redirect so existing links survive.

**Why this is a task at all.** Task 7 Step 1 scoped "the UI's `api/types.ts` +
`api/adapters.ts`" and those files still carry `PlatformInfo`,
`PlatformDetails` and `platformFromDto`. Everything a *user reads* was never in
any task's scope: spec §8's UI table lists two component renames
(`PlatformsTable`, `PlatformDetailPage`), both done, and nothing else. Measured
surface: **50 files, ~1,292 occurrences** outside `api/generated/`.

**Runs after Task 25**, so the UI renames against the new wire rather than
renaming twice.

**Files:** the gear side of Step 0 — `qa-insights/qa-insights/src/` (`domain/analytics/aggregates.rs`, `domain/analytics/query.rs`, `api/rest/dto.rs`, `api/rest/routes/analytics.rs`) — then `qa-platform-ui/src/` — 50 files. Renamed on disk:
`pages/PlatformsPage.tsx` → `EnvironmentsPage.tsx`;
`components/platforms/` → `components/environments/` (merging with the existing
directory), including `CreatePlatformDialog.tsx` → `CreateEnvironmentDialog.tsx`
and `EditPlatformDialog.tsx` → `EditEnvironmentDialog.tsx`;
`components/dashboard/PlatformsStrip.tsx` → `EnvironmentsStrip.tsx`;
`lib/defaultPlatform.ts` (+ its test) → `defaultEnvironment.ts`;
`lib/platform-observation.ts` (+ its test) → `lib/environment-observation.ts`.

- [ ] **Step 0: the gear side — `group_by=platform` → `group_by=environment`.**
  Added at the user's request, 2026-09-05. The Task 25 review raised this as its
  M-4 and ruled it "noted for coherence, not for action in this task": the value
  is inert (`routes/analytics.rs:206` — *"group_by=platform narrows nothing at
  all"*), so it was out of G-3's scope. It is still a live query value naming a
  section the same response now calls `environment`, and it is the last one.

  * `GroupBy::Platform` → `GroupBy::Environment`
    (`domain/analytics/aggregates.rs:1974`).
  * `parse_group`'s `"platform" => …` arm → `"environment"`
    (`domain/analytics/query.rs:226`), and its rejection message
    (`:229`) → `"group_by must be one of: none, component, tag, environment"`.
  * `group_to_str`'s `GroupBy::Platform => "platform"` → `"environment"`
    (`api/rest/dto.rs:1906`).
  * `routes/analytics.rs:150`, `:206`, `:316` — the published parameter docs.

  **This one fails loudly and needs no G-4 trap.** `parse_group` already answers
  `DomainError::Validation` on any unrecognised value, so `group_by=platform`
  becomes a 400 listing the valid values rather than a silent fallback —
  confirmed by reading the function, not assumed. Verify that with a test before
  relying on it; if it turns out to fall back to `None` instead, stop and report,
  because that is the Critical-1 shape ruling G-4 exists for.

  **Two docs must change with it.** `parse_group`'s header says the message is
  *"legacy's message verbatim"* and `group_to_str`'s says it is *"`GroupBy` in
  legacy's wire spelling"*. Both become false here: this is a deliberate
  divergence from legacy, and the docs must say that rather than continue to
  claim parity. Keep the legacy citations (`analytics.rs:2095-2102`) verbatim.

  Add the value to the CHANGELOG's breaking entry beside the field renames.

- [ ] **Step 1: Types and adapters** — `PlatformInfo` → `EnvironmentInfo`,
  `PlatformDetails` → `EnvironmentDetails`, `platformFromDto` →
  `environmentFromDto`, `platformDetailsFromDto` → `environmentDetailsFromDto`,
  `defaultPlatformForProduct` → `defaultEnvironmentForProduct`,
  `formatPlatformLabel` → `formatEnvironmentLabel`, and the hooks
  (`usePlatformDetails` → `useEnvironmentDetails`, and its siblings).

- [ ] **Step 2: Files and directories** — `git mv` every file above, so history
  follows. Update every import.

- [ ] **Step 3: The route** — `/platforms` → `/environments` and
  `/platforms/:name` → `/environments/:name` (`App.tsx:104-105`), **plus a
  `<Navigate replace>` redirect from each old path**, so a bookmarked or shared
  link still lands. `Sidebar.tsx:34`'s nav entry becomes
  `{ name: 'Environments', href: '/environments', icon: Server }`.

- [ ] **Step 4: Rendered copy** — every user-visible string. Known sites:
  `EditPlatformDialog.tsx:82,120,146`; `ExclusivitySelect.tsx:47`;
  `RunPlanDialog.tsx:165`; `RunsTable.tsx:78`; `QueuedRunsCard.tsx:128`;
  `CreateScheduleDialog.tsx:440`; `SchedulesTable.tsx:214,260`;
  `RunCustomPlanDialog.tsx:183`; `RunTestDialog.tsx:170`;
  `EnvironmentDetailPage.tsx:86,88,116,181`; `PlatformsPage.tsx`'s heading.
  **`PlatformsPage.tsx`'s heading and subtitle currently disagree** — the
  subtitle was already changed to "Manage the environments tests run against"
  under a heading that still says "Platforms". Fix both.

  **Leave `"QA Platform"` alone** — `Sidebar.tsx:168` and
  `SlackMessagePreview.tsx:58`. That is the product's name, not the aggregate's.

- [ ] **Step 5: localStorage** — if any key carries `platform`, migrate it with
  the `readMigrated`/`migratePrefixedKeys` helpers in `lib/storageKeys.ts` that
  Task 22 built for the `vhp.*` → `qa.*` rename. Do not silently orphan a key.

- [ ] **Step 6: Gate and commit**

Step 0 is a separate commit from the UI work, and it lands first:

```bash
cargo test -p qa-insights -j 6
cargo clippy -p qa-insights --all-targets --all-features -j 6
cargo fmt -p qa-insights --check
cargo test -p qa-runs -j 6 doc_citations
git commit -m "refactor(qa-insights)!: group_by=platform becomes group_by=environment"
```

Then the UI:

```bash
make ui-test && make ui-build
git add -A && git commit -m "refactor(qa-platform-ui): the UI speaks Environment, not Platform"
```

  `make ui-lint` is **not** in this gate (ruling F-22 — unrunnable at the merge
  base). `make ui-build` is `tsc && vite build` and is the gate that catches a
  broken rename; `make ui-test` alone does not typecheck.

  **Back up before any blanket edit, with `cp`, not after it goes wrong** — and
  never `git checkout --` a file whose changes are not committed. A `sed` over
  50 files is exactly the operation that cost this branch ~400 lines once
  already. `platform` also appears inside legacy citations and inside the string
  `"QA Platform"`; rename **by review, not by `sed`**, the same rule spec §6.1
  set for the Rust doc prose.

---

## Self-review against the spec


| Spec section                            | Covered by                                                                                                                                              |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| §3 D1 in-process Rust plugins           | Tasks 4, 11                                                                                                                                             |
| §3 D2 non-Kubernetes environments       | Tasks 8, 14, 17                                                                                                                                         |
| §3 D3 descriptor-driven observation     | Tasks 1, 15, 21                                                                                                                                         |
| §3 D4 parsing stays platform-owned      | *no task* — deliberate; `qa-catalog/src/domain/parsing/` is untouched by this plan                                                                      |
| §3 D5 the rename                        | Tasks 5–7 (Rust), **25 (the wire), 26 (the UI's own vocabulary — neither was in any task's scope)**                                                      |
| §3 D6 every product names a plugin      | Tasks 12, 20 (server), **23 (UI — added after 12 and 20 shipped a requirement no task let a user satisfy)**                                              |
| §3 D7 generic k8s as a library          | Task 8                                                                                                                                                  |
| §3 D8 platform owns precedence          | Tasks 3, 18                                                                                                                                             |
| §3 D9 one environment per product       | Task 20                                                                                                                                                 |
| §3 D10 role projections                 | Tasks 2, 15                                                                                                                                             |
| §3 D11 runner per product               | Tasks 10, 17                                                                                                                                            |
| §3 D12 classified failures              | Tasks 2, 4, 8                                                                                                                                           |
| §5 the trait                            | Task 3                                                                                                                                                  |
| §6 data model                           | Tasks 12, 14, 19, 20                                                                                                                                    |
| §7 dispatch                             | Tasks 17, 18                                                                                                                                            |
| §8 UI                                   | Tasks 21, 22, 23, 26                                                                                                                                    |
| §9 secret containment, all three layers | Task 1 (layer 2), Task 2 (layer 1), Task 4 (layer 3)                                                                                                    |
| §10 rollout                             | the phase structure itself                                                                                                                              |
| §11 parity evidence                     | Tasks 9, 10, 16, 18                                                                                                                                     |
| §12 open items                          | *no task* — recorded as out of scope: the pytest-only runner image, the frozen `vhp-test-failure` JIRA label, per-product JIRA mapping, lease semantics |


**Known gap, deliberate:** nothing in this plan builds a second product plugin. The design is validated by VHP passing byte-identically plus `assert_no_leak`; the first real non-Kubernetes plugin is the true test of the abstraction and should be planned separately, against a concrete product.