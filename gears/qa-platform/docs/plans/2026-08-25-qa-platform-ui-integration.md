# qa-platform UI Integration and Deployment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **RESUMING? READ THE STATUS SECTION AT THE END OF THIS FILE FIRST.**
> **Phases A and B-so-far are COMPLETE.** Tasks 1-8 and 8a are done and reviewed — every checkbox above
> Task 9 is ticked. **Resume at Task 9.**
> Eight tasks were added during execution (2b, 3a, 3b, 4a, 4b, 5a, 8a, plus Task 8's rescoping); six of
> them fixed live defects no existing test caught.
> **Two things to know before you touch anything:**
> 1. **If the API is unreachable from the host, run `docker compose down && docker compose up -d` (never
>    `-v`) before diagnosing.** It is stale host forwarding rules, not the gears.
> 2. **Do not trust any claim in this plan or the spec about "what references X" without grepping.**
>    Seven such claims have been checked this session and *all seven were wrong* — see the status section.
> Last updated 2026-08-26 (session 3).

**Goal:** Put the legacy `manager-ui` in front of the four qa-platform gears, behind real OIDC login, running in a docker-compose stack anyone can bring up and in a Helm chart that mirrors legacy's.

**Architecture:** The legacy UI is copied into this repo and adapted **only** inside `src/api/` — legacy routed its whole wire surface through three files behind one base URL, so the backend swap is 2,412 lines and ~18k LOC of components are untouched. A default-off `system_grants` section on `static-authz-plugin` unblocks the background tasks that make the product show data at all. Deployment is compose for verification and Helm for staging.

**Tech Stack:** Rust (the gears, `static-authz-plugin`), React 19 + Vite 6 + TypeScript 5.7 + TanStack Query 5 (the UI), `oidc-client-ts`, `openapi-typescript`, Docker Compose, Helm, Keycloak, Postgres.

**Spec:** `gears/qa-platform/docs/superpowers/specs/2026-08-25-qa-platform-ui-integration-design.md` — read it before Task 1. The plan argues from the spec; where they disagree the spec wins.

## Global Constraints

Copied verbatim from the spec. Every task's requirements implicitly include these.

- **Preserve legacy behaviour; adapt only the implementation.** `src/components/` and `src/pages/` are copied verbatim. The **only** permitted modifications are the three removals in spec §6.5.
- **The backend is feature-frozen.** No task changes the four gears' domain logic. A contract gap found in Phase B is **reported to a human, not fixed by widening a gear**.
- **`system_grants` absent must be byte-for-byte today's behaviour.** No existing stack changes.
- **`system_grants` is read-only unless a grant names the write.** Any action outside `{get, list}` requires `allow_write: true` on that grant, or config load fails. This exists because `qa-runs/src/domain/system_actor.rs` records that granting that subject a covering constraint set makes *"a nil-tenant write context compile to a platform-wide write scope."*
- **Exactly one write grant exists in the product:** `qa_catalog.system` → `qa.bundle` → `delete`. Adding a second requires a human decision.
- **Auth arrives in Phase C.** Phases A and B run with `auth_disabled: true`, as `config/qa-platform.yaml` does today. Keycloak is not in the compose file until Phase C.
- **Toolchain:** `~/.cargo/bin` is NOT on the default PATH in this environment and `/usr/bin/cargo` is 1.75.0, which cannot parse this workspace's `resolver = "3"`. Every task that runs cargo must first `export PATH="$HOME/.cargo/bin:$PATH"`.
- **Never run `cargo test --all-targets` at workspace level.** It runs `libs/toolkit-db/benches/worker_overhead.rs`, a 20+ minute criterion bench that two agents have already abandoned believing it hung. Use `--lib --bins --tests`.
- **`cargo test --workspace` (literal) cannot pass** — an upstream doctest at `libs/toolkit/src/api/operation_builder.rs:932` fails to compile identically on `main`. Always `cargo test --workspace --lib --bins --tests`.
- **Rust gate for any task touching Rust:** `cargo fmt --all -- --check`, then `cargo clippy -p <crate> --all-targets -- -D warnings`, then that crate's tests.
- **Doc comments are load-bearing in this repo, and a claim that does not survive checking counts as a defect.** If you cite a `file.rs:NNN`, open it first, and re-check it after any edit that shifts lines.
- **Commit per task.** Branch is local; no push, no PR, no squash.
- **Every file this plan creates lives under `gears/qa-platform/`. No repo-root directories.**
  Instruction from the human partner, 2026-08-26, overriding the paths this plan was originally
  written with. So `gears/qa-platform/config/qa-platform-stack.yaml` and
  `gears/qa-platform/deploy/**`, never a root `config/` or `deploy/`. Pre-existing root files stay
  where they are: `config/qa-platform.yaml` and `config/quickstart-windows.yaml` are read-only
  references, and the root `Makefile` and `.gitignore` are repo-wide files this plan only edits.
  Every path in the File Structure table and in each task below has been rewritten accordingly; a
  relative path inside a compose or Dockerfile is the one thing to recompute rather than trust,
  because the depth changed.

---

## File Structure

**Created:**

| Path | Responsibility |
|---|---|
| `gears/system/authz-resolver/plugins/static-authz-plugin/src/config.rs` (modify) | `SystemGrant` type + validation |
| `.../static-authz-plugin/src/domain/grants.rs` | grant matching, pure, no SDK types beyond the request |
| `gears/qa-platform/config/qa-platform-stack.yaml` | the deployable stack config: postgres, tasks ON, grants |
| `gears/qa-platform/deploy/docker/qa-platform.Dockerfile` | multi-stage build of `cf-gears-example-server` |
| `gears/qa-platform/deploy/docker/qa-platform-ui.Dockerfile` | node build → nginx |
| `gears/qa-platform/deploy/compose/docker-compose.yml` | postgres, gears, ui, (Phase C) keycloak |
| `gears/qa-platform/deploy/compose/keycloak/realm-qa-platform.json` | realm, public PKCE client, two users |
| `gears/qa-platform/deploy/compose/smoke.sh` | the Phase A gate, reused by Phase D |
| `gears/qa-platform/deploy/charts/qa-platform/**` | Helm chart mirroring `charts/vhp-testrunner` |
| `gears/qa-platform/qa-platform-ui/**` | the UI |
| `gears/qa-platform/docs/CONTRACT-DIFF.md` | Phase B's field-level mismatch table |

**Modified:** `Makefile` (UI targets), `.github/workflows/*` or the repo's CI equivalent (Node job).

---

# Phase A — the stack can produce data

Phase A carries no UI on purpose: it proves the backend can produce what every later phase displays, and it is where `system_grants` either works or does not.

### Task 1: `SystemGrant` config type and its validation

**Files:**
- Modify: `gears/system/authz-resolver/plugins/static-authz-plugin/src/config.rs`
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `SystemGrant { subject_type: String, resources: Vec<String>, actions: Vec<String>, allow_write: bool }`, `StaticAuthZPluginConfig::system_grants: Vec<SystemGrant>`, and `StaticAuthZPluginConfig::validate(&self) -> Result<(), String>`.
- Consumed by: Task 2 (matching), Task 3 (the YAML that must load).

- [x] **Step 0: Verify the resource-name assumption before writing anything**

The grant matches `EvaluationRequest.resource.resource_type` against strings like `qa.test_result`. `authz-resolver-sdk/src/models.rs:110-112` documents that field with a **GTS-style** example (`gts.cf.core.users.user.v1~`), while the qa gears declare `ResourceType::from_static("qa.test_result", ..)` (`qa-insights/src/domain/service/mod.rs:182-194`).

Find where the PEP populates `Resource::resource_type` from a `ResourceType` and confirm which string arrives. Record the answer in your report with the file:line. **If it is not the `qa.*` name, stop and report NEEDS_CONTEXT** — every grant in this plan is written against those names and the whole phase depends on it.

- [x] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn grant(actions: &[&str], allow_write: bool) -> SystemGrant {
        SystemGrant {
            subject_type: "qa_runs.system".to_owned(),
            resources: vec!["qa.run".to_owned()],
            actions: actions.iter().map(|a| (*a).to_owned()).collect(),
            allow_write,
        }
    }

    fn config_with(grants: Vec<SystemGrant>) -> StaticAuthZPluginConfig {
        StaticAuthZPluginConfig { system_grants: grants, ..Default::default() }
    }

    #[test]
    fn a_default_config_has_no_grants_at_all() {
        // The whole safety story rests on this: an absent section must be inert.
        assert!(StaticAuthZPluginConfig::default().system_grants.is_empty());
        assert!(StaticAuthZPluginConfig::default().validate().is_ok());
    }

    #[test]
    fn read_only_actions_need_no_write_opt_in() {
        assert!(config_with(vec![grant(&["get", "list"], false)]).validate().is_ok());
    }

    #[test]
    fn a_write_action_without_the_opt_in_is_refused_and_the_message_names_all_three() {
        let err = config_with(vec![grant(&["delete"], false)]).validate().unwrap_err();
        assert!(err.contains("qa_runs.system"), "subject missing from {err:?}");
        assert!(err.contains("qa.run"), "resource missing from {err:?}");
        assert!(err.contains("delete"), "action missing from {err:?}");
        assert!(err.contains("allow_write"), "remedy missing from {err:?}");
    }

    #[test]
    fn the_write_opt_in_without_a_write_action_is_refused() {
        // Means the author misunderstood the flag; silently accepting it teaches
        // the wrong lesson to the next reader of the config file.
        let err = config_with(vec![grant(&["get"], true)]).validate().unwrap_err();
        assert!(err.contains("allow_write"), "{err:?}");
    }

    #[test]
    fn an_unknown_action_is_refused_rather_than_silently_never_matching() {
        let err = config_with(vec![grant(&["lst"], false)]).validate().unwrap_err();
        assert!(err.contains("lst"), "{err:?}");
    }

    #[test]
    fn an_empty_resources_or_actions_list_is_refused() {
        let mut g = grant(&["get"], false);
        g.resources.clear();
        assert!(config_with(vec![g]).validate().is_err());

        let mut g = grant(&["get"], false);
        g.actions.clear();
        assert!(config_with(vec![g]).validate().is_err());
    }

    #[test]
    fn the_yaml_shape_in_the_spec_deserialises() {
        let yaml = r#"
vendor: constructorfabric
priority: 100
system_grants:
  - subject_type: qa_runs.system
    resources: [qa.queue_entry, qa.run, qa.schedule]
    actions: [get, list]
  - subject_type: qa_catalog.system
    resources: [qa.bundle]
    actions: [delete]
    allow_write: true
"#;
        let cfg: StaticAuthZPluginConfig = serde_yaml::from_str(yaml).expect("spec YAML must load");
        assert_eq!(cfg.system_grants.len(), 2);
        assert!(cfg.validate().is_ok());
    }
}
```

- [x] **Step 2: Run the tests and watch them fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p cf-gears-static-authz-plugin --lib
```

Expected: FAIL to compile — `SystemGrant` and `system_grants` do not exist. If the crate name above is wrong, get it from that crate's `Cargo.toml` `[package] name` and use the real one in every later step.

- [x] **Step 3: Implement the type and the validation**

`config.rs` already carries `#[serde(default, deny_unknown_fields)]`, so the new field must have a serde default or every existing config file breaks.

```rust
/// Read actions a grant may carry without opting into writes.
const READ_ACTIONS: [&str; 2] = ["get", "list"];

/// Actions the PDP recognises at all. An action outside this set would match
/// nothing at runtime, which is indistinguishable from a missing grant — so it
/// is refused at load instead.
const KNOWN_ACTIONS: [&str; 5] = ["get", "list", "create", "update", "delete"];

/// One cross-tenant grant for one gear's system actor.
///
/// # Why this is read-only by default
///
/// `qa-runs/src/domain/system_actor.rs` records that in a deployment whose PDP
/// grants `qa_runs.system` a covering constraint set, **a nil-tenant write
/// context compiles to a platform-wide write scope** — and that nothing in that
/// gear's type system separates its enumeration factories from its write
/// factories. What makes a grant safe is that the gears do separate them in
/// practice: eight of the nine platform-scoped factories across qa-runs,
/// qa-catalog and qa-insights only read, and every write runs under a
/// tenant-bound context that ordinary per-tenant policy already covers.
///
/// The ninth is `qa_catalog::system_actor::for_bundle_gc`, which deletes expired
/// bundles across tenants and whose own doc justifies it: "expiry is a platform
/// hygiene concern, and the delete is keyed strictly on `expires_at`". That is
/// the one grant in the product that sets [`Self::allow_write`], and requiring
/// the flag is what makes it *look* exceptional in a config file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemGrant {
    /// e.g. `qa_runs.system`. Matched against `EvaluationRequest.subject.subject_type`.
    pub subject_type: String,
    /// PEP resource-type names, e.g. `qa.run`.
    pub resources: Vec<String>,
    /// Action names, e.g. `get`, `list`.
    pub actions: Vec<String>,
    /// Required for any action outside [`READ_ACTIONS`].
    #[serde(default)]
    pub allow_write: bool,
}

impl StaticAuthZPluginConfig {
    /// Reject a configuration whose grants cannot mean what their author meant.
    ///
    /// # Errors
    ///
    /// Returns a message naming the subject, resource and action at fault.
    pub fn validate(&self) -> Result<(), String> {
        for g in &self.system_grants {
            if g.resources.is_empty() {
                return Err(format!(
                    "system_grants: subject_type '{}' lists no resources; a grant that \
                     matches nothing is indistinguishable from a missing grant",
                    g.subject_type
                ));
            }
            if g.actions.is_empty() {
                return Err(format!(
                    "system_grants: subject_type '{}' lists no actions; a grant that \
                     matches nothing is indistinguishable from a missing grant",
                    g.subject_type
                ));
            }
            let mut writes = Vec::new();
            for a in &g.actions {
                if !KNOWN_ACTIONS.contains(&a.as_str()) {
                    return Err(format!(
                        "system_grants: subject_type '{}' names unknown action '{a}'; \
                         known actions are {KNOWN_ACTIONS:?}",
                        g.subject_type
                    ));
                }
                if !READ_ACTIONS.contains(&a.as_str()) {
                    writes.push(a.clone());
                }
            }
            if !writes.is_empty() && !g.allow_write {
                return Err(format!(
                    "system_grants: subject_type '{}' on resources {:?} requests write \
                     action(s) {writes:?} without `allow_write: true`. A cross-tenant \
                     write scope must be named deliberately — see SystemGrant's docs",
                    g.subject_type, g.resources
                ));
            }
            if writes.is_empty() && g.allow_write {
                return Err(format!(
                    "system_grants: subject_type '{}' sets `allow_write: true` but names \
                     only read actions {:?}; remove the flag",
                    g.subject_type, g.actions
                ));
            }
        }
        Ok(())
    }
}
```

Add to the struct and its `Default`:

```rust
    /// Cross-tenant grants for gear system actors. Empty means a nil-tenant
    /// request is denied, which is this plugin's behaviour with no config.
    #[serde(default)]
    pub system_grants: Vec<SystemGrant>,
```

```rust
            system_grants: Vec::new(),
```

- [x] **Step 4: Run the tests and watch them pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p cf-gears-static-authz-plugin --lib
```

Expected: PASS, all 7 new tests plus the crate's existing tests, 0 failed. If `serde_yaml` is not a dev-dependency of this crate, add it (workspace version) — that is part of this task.

- [x] **Step 5: Call `validate()` where config is loaded**

Find where `StaticAuthZPluginConfig` is read (`src/gear.rs`, ~74 lines) and call `validate()`, turning `Err` into an init failure that names the plugin. A validation function nothing calls is the "inert guard" defect this repo has caught repeatedly — the test that proves it is wired is: an invalid grant makes gear init fail, not merely `validate()` return `Err`.

- [x] **Step 6: Gate and commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --all -- --check
cargo clippy -p cf-gears-static-authz-plugin --all-targets -- -D warnings
cargo test -p cf-gears-static-authz-plugin
git add gears/system/authz-resolver/plugins/static-authz-plugin/
git commit -m "feat(static-authz): system_grants config type, read-only unless a write is named"
```

---

### Task 2: The grant decision path

**Files:**
- Create: `gears/system/authz-resolver/plugins/static-authz-plugin/src/domain/grants.rs`
- Modify: `.../src/domain/service.rs` (the nil-tenant branch), `.../src/domain/mod.rs`, `.../src/gear.rs` (pass config into `Service`)
- Test: `.../src/domain/grants.rs` tests + `.../src/domain/service_tests.rs`

**Interfaces:**
- Consumes: `SystemGrant`, `StaticAuthZPluginConfig::system_grants` (Task 1).
- Produces: `grants::matches(&[SystemGrant], subject_type: Option<&str>, resource_type: &str, action: &str) -> bool`, and `Service::with_config(StaticAuthZPluginConfig) -> Service`.

`Service` is currently `#[derive(Default)] pub struct Service;` with `evaluate(&self, ..)` carrying `#[allow(clippy::unused_self)] // &self reserved for future config/state`. This task is that anticipated change: give `Service` its config and drop the allow.

- [x] **Step 1: Write the failing matcher tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SystemGrant;

    fn grants() -> Vec<SystemGrant> {
        vec![SystemGrant {
            subject_type: "qa_runs.system".to_owned(),
            resources: vec!["qa.run".to_owned(), "qa.queue_entry".to_owned()],
            actions: vec!["get".to_owned(), "list".to_owned()],
            allow_write: false,
        }]
    }

    #[test]
    fn all_three_matching_is_a_match() {
        assert!(matches(&grants(), Some("qa_runs.system"), "qa.run", "list"));
    }

    #[test]
    fn a_wrong_subject_is_not_a_match() {
        assert!(!matches(&grants(), Some("qa_catalog.system"), "qa.run", "list"));
    }

    #[test]
    fn a_wrong_resource_is_not_a_match() {
        assert!(!matches(&grants(), Some("qa_runs.system"), "qa.bundle", "list"));
    }

    #[test]
    fn a_wrong_action_is_not_a_match() {
        assert!(!matches(&grants(), Some("qa_runs.system"), "qa.run", "delete"));
    }

    #[test]
    fn an_absent_subject_type_is_not_a_match() {
        // subject_type is Option<String> on the wire; None must never match.
        assert!(!matches(&grants(), None, "qa.run", "list"));
    }

    #[test]
    fn no_grants_never_matches() {
        assert!(!matches(&[], Some("qa_runs.system"), "qa.run", "list"));
    }
}
```

- [x] **Step 2: Run and watch it fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p cf-gears-static-authz-plugin --lib grants
```

Expected: FAIL to compile — module `grants` does not exist.

- [x] **Step 3: Implement the matcher**

```rust
//! Cross-tenant grant matching for gear system actors.
//!
//! Pure and total: three string comparisons, no I/O, no SDK types. It is a
//! separate module from `service` so the matching rule can be tested without
//! constructing an `EvaluationRequest`, and so the nil-tenant branch in
//! `service::evaluate` stays readable.

use crate::config::SystemGrant;

/// Whether any grant covers this (subject, resource, action) triple.
///
/// A `None` subject type never matches: the field is optional on the wire, and
/// treating absent as a wildcard would grant every anonymous nil-tenant caller
/// whatever the first grant names.
#[must_use]
pub fn matches(
    grants: &[SystemGrant],
    subject_type: Option<&str>,
    resource_type: &str,
    action: &str,
) -> bool {
    let Some(subject_type) = subject_type else {
        return false;
    };
    grants.iter().any(|g| {
        g.subject_type == subject_type
            && g.resources.iter().any(|r| r == resource_type)
            && g.actions.iter().any(|a| a == action)
    })
}
```

- [x] **Step 4: Run and watch it pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p cf-gears-static-authz-plugin --lib grants
```

Expected: PASS, 6/6.

- [x] **Step 5: Write the failing service-level test**

In `service_tests.rs`, following the existing tests' construction of `EvaluationRequest`:

```rust
#[test]
fn a_nil_tenant_request_is_still_denied_when_no_grant_is_configured() {
    // The regression guard for every existing deployment.
    let svc = Service::new();
    let resp = svc.evaluate(&nil_tenant_request("qa_runs.system", "qa.run", "list"));
    assert!(!resp.decision);
}

#[test]
fn a_matched_grant_permits_a_nil_tenant_request_and_does_not_clamp_it_to_one_tenant() {
    let cfg = StaticAuthZPluginConfig {
        system_grants: vec![SystemGrant {
            subject_type: "qa_runs.system".to_owned(),
            resources: vec!["qa.run".to_owned()],
            actions: vec!["list".to_owned()],
            allow_write: false,
        }],
        ..Default::default()
    };
    let svc = Service::with_config(cfg);
    let resp = svc.evaluate(&nil_tenant_request("qa_runs.system", "qa.run", "list"));
    assert!(resp.decision, "a matched grant must permit");
    // The point of the grant is breadth: it must NOT emit the single-tenant
    // In(OWNER_TENANT_ID, [tid]) clamp the tenant path emits.
    assert!(
        !emits_single_tenant_clamp(&resp),
        "a cross-tenant grant must not clamp to one tenant: {resp:?}"
    );
}

#[test]
fn a_grant_for_a_different_resource_leaves_the_nil_tenant_deny_in_place() {
    let cfg = StaticAuthZPluginConfig {
        system_grants: vec![SystemGrant {
            subject_type: "qa_runs.system".to_owned(),
            resources: vec!["qa.schedule".to_owned()],
            actions: vec!["list".to_owned()],
            allow_write: false,
        }],
        ..Default::default()
    };
    let svc = Service::with_config(cfg);
    assert!(!svc.evaluate(&nil_tenant_request("qa_runs.system", "qa.run", "list")).decision);
}

#[test]
fn a_tenant_bound_request_is_unaffected_by_grants() {
    // Grants must only widen the nil-tenant path, never alter ordinary requests.
    let cfg = StaticAuthZPluginConfig {
        system_grants: vec![SystemGrant {
            subject_type: "qa_runs.system".to_owned(),
            resources: vec!["qa.run".to_owned()],
            actions: vec!["list".to_owned()],
            allow_write: false,
        }],
        ..Default::default()
    };
    let with = Service::with_config(cfg).evaluate(&tenant_request("qa.run", "list"));
    let without = Service::new().evaluate(&tenant_request("qa.run", "list"));
    assert_eq!(with.decision, without.decision);
    assert_eq!(format!("{:?}", with.context), format!("{:?}", without.context));
}
```

Write the three helpers (`nil_tenant_request`, `tenant_request`, `emits_single_tenant_clamp`) in the same test module, modelled on how the existing tests in this file build requests and inspect `EvaluationResponseContext`. Read those tests first — do not invent a construction the file does not use.

- [x] **Step 6: Run and watch it fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p cf-gears-static-authz-plugin --lib
```

Expected: FAIL — `Service::with_config` does not exist.

- [x] **Step 7: Wire the grant into the nil-tenant branch**

Give `Service` a `config: StaticAuthZPluginConfig` field, add `with_config`, keep `new()`/`Default` producing an empty-grant config, and remove the now-false `#[allow(clippy::unused_self)]`. In `evaluate`, inside the existing `if tid == Uuid::default()` block, **before** the current deny: if `grants::matches(..)` is true, return a permit whose constraints do not clamp to one tenant; otherwise fall through to the deny that is there today.

Emit the cross-tenant permit the same way the existing code builds its response — reuse the surrounding helpers rather than hand-rolling a second construction. Update the `Service` doc comment's bulleted contract to name the new branch; leaving it stale is the doc-drift defect this repo counts as a defect.

- [x] **Step 8: Run and watch it pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p cf-gears-static-authz-plugin
```

Expected: PASS, all new tests plus the crate's existing suite, 0 failed. Report the before/after test counts.

- [x] **Step 9: Gate and commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --all -- --check
cargo clippy -p cf-gears-static-authz-plugin --all-targets -- -D warnings
cargo test --workspace --lib --bins --tests
git add gears/system/authz-resolver/plugins/static-authz-plugin/
git commit -m "feat(static-authz): grant cross-tenant scopes to configured gear system actors"
```

The workspace run is here rather than per-crate because this change touches a system gear every stack links.

---

### Task 2b: Emit a covering constraint set so a granted permit survives the PEP

**Added during execution**, 2026-08-26, after Task 3 Step 4's first boot. Not in the original plan.
Commit `54e13ce0`. Reviewed clean.

**Why it exists:** Task 2's committed grant path returned a permit whose constraint list was **empty**.
The PEP compiles an empty constraint set to `ConstraintsRequiredButAbsent` and fails closed, so every
granted nil-tenant request was still denied — and every background task logged
`constraints required but PDP returned none` on every tick. Task 2's own tests passed because they
asserted on `resp.decision` and on the *absence* of a single-tenant clamp, neither of which notices that
the permit carries nothing the PEP can use.

The gears' own docs specify a **covering** constraint set, not an empty one. The fix emits a single
constraint with no predicates, which the constraint compiler documents as allow-all.

- [x] **Step 1: Reproduce the denial in a test at the service level**
- [x] **Step 2: Emit one predicate-free constraint from the grant branch**
- [x] **Step 3: Assert the permit's constraint set is non-empty and unclamped**
- [x] **Step 4: Rust gate and commit**

**Lesson carried forward:** a permit is not a pass. Any future test of an authorization decision must
assert on what the decision *carries*, not only on its boolean.

---

### Task 3: The stack config — Postgres, grants, and every background task on

**Files:**
- Create: `gears/qa-platform/config/qa-platform-stack.yaml`
- Reference (do not modify): `config/qa-platform.yaml`

**Interfaces:**
- Consumes: `system_grants` (Tasks 1-2).
- Produces: the config file Tasks 4, 5 and all of Phase C load. Its gear section names are the contract later tasks edit.

- [x] **Step 1: Copy the dev config and switch the database to Postgres**

Start from `config/qa-platform.yaml` verbatim, then replace the `database:` block. The Postgres shape is documented at `config/quickstart-windows.yaml:293-317`:

```yaml
database:
  servers:
    pg_qa_platform:
      engine: "postgres"
      host: "${POSTGRES_HOST}"
      port: 5432
      user: "${POSTGRES_USER}"
      password: "${POSTGRES_PASSWORD}"
      params:
        application_name: "qa_platform"
      pool:
        max_conns: 20
        acquire_timeout: "30s"
```

Every gear's `database:` block becomes `server: "pg_qa_platform"` with its own `dbname:` instead of `file:` — `qa_environments`, `qa_catalog`, `qa_runs`, `qa_insights`, plus the system gears that carry one today (`simple-user-settings`, `credstore`, `resource-group`, `event-broker`). One database per gear, matching the quickstart's own comment that this "prevents accidental sharing".

- [x] **Step 2: Turn on every background task, and rewrite the comments that say why they are off**

`config/qa-platform.yaml` carries long comments explaining why each switch is `false`. Those reasons are **discharged by Tasks 1-2**, so copying them across unchanged would leave the file lying about itself:

```yaml
  qa-catalog:
    database:
      server: "pg_qa_platform"
      dbname: "qa_catalog"
    # The dev config sets `branch_refresh_interval_seconds: 0` because the
    # refresher runs under a nil-tenant system actor the static-authz plugin
    # denied outright. `system_grants` grants `qa_catalog.system` get/list on
    # `qa.test_repo` (the enumeration step's cross-tenant `(repository, tenant)`
    # listing) and delete on `qa.bundle` for the GC, so both tasks now run.
    config:
      branch_refresh_interval_seconds: 900

  qa-runs:
    database:
      server: "pg_qa_platform"
      dbname: "qa_runs"
    # Both switches are ON here, unlike the dev config. Its comments explain the
    # denial that made them useless; `system_grants` grants `qa_runs.system`
    # get/list on qa.queue_entry, qa.run and qa.schedule — the six
    # platform-scoped factories' reads. Every write still runs tenant-bound.
    #
    # With the dispatcher on, results are ingested, so the dashboard and
    # analytics have data. That is the whole point of this file.
    config:
      dispatcher_enabled: true
      scheduler_enabled: true
      schedule_interval_seconds: 60

  qa-insights:
    database:
      server: "pg_qa_platform"
      dbname: "qa_insights"
    # The three tickers (reconciler, jira-poller, collect). `system_grants`
    # grants `qa_insights.system` get/list on `qa.test_result`, which is what
    # the tenant enumeration behind all three needs.
    #
    # WARNING, carried from the qa-insights plan's release-gate items: starting
    # the jira-poller ticker under the default `NoopLeaderElector` makes EVERY
    # replica a leader, and its auto-rerun is bounded only by a local write —
    # "a race, not a lock", in that gear's own words. Single-replica compose is
    # safe. Do not raise the gears replica count above 1 without deciding that.
    config:
      enable_tickers: true
      reconcile_interval_seconds: 300
      jira_poller_interval_seconds: 300
      collect_interval_seconds: 3600
```

- [x] **Step 3: Add the grants, and keep auth off for now**

```yaml
  static-authz-plugin:
    config:
      vendor: "constructorfabric"
      priority: 100
      system_grants:
        - subject_type: qa_runs.system
          resources: [qa.queue_entry, qa.run, qa.schedule]
          actions: [get, list]
        - subject_type: qa_insights.system
          resources: [qa.test_result]
          actions: [get, list]
        - subject_type: qa_catalog.system
          resources: [qa.test_repo]
          actions: [get, list]
        - subject_type: qa_catalog.system
          resources: [qa.bundle]
          actions: [delete]
          allow_write: true
```

`api-gateway` keeps `auth_disabled: true` and the stack keeps `static-authn-plugin` and `single-tenant-tr-plugin`. **Phase C replaces those three lines and nothing else in this file.** Say so in a comment at the `api-gateway` block, so whoever reads this file in Phase B does not think auth was forgotten.

Also set `bind_addr: "0.0.0.0:8087"` — `127.0.0.1` is unreachable from outside a container.

- [x] **Step 4: Verify the config loads and the tasks start**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
POSTGRES_HOST=localhost POSTGRES_USER=qa POSTGRES_PASSWORD=qa \
  cargo run --bin cf-gears-example-server \
  --features qa-platform,static-authn,static-authz,single-tenant,static-credstore \
  -- --config gears/qa-platform/config/qa-platform-stack.yaml run
```

You need a Postgres to point at; `docker run --rm -e POSTGRES_PASSWORD=qa -e POSTGRES_USER=qa -e POSTGRES_DB=postgres -p 5432:5432 postgres:16` is enough for this step.

Expected in the log: the gears initialise, and **the dispatcher, scheduler and the three tickers log that they started** rather than logging the WARN the dev config exists to avoid. Grep for the WARN naming a denied system actor — **if it appears, the grant is not reaching the PDP and this task is not done.** Record the actual log lines in your report; "it started" without them is not evidence.

- [x] **Step 5: Commit**

```bash
git add gears/qa-platform/config/qa-platform-stack.yaml
git commit -m "feat(qa-platform): deployable stack config — postgres, grants, background tasks on"
```

---

### Task 3a: Tolerate an absent event-broker client

**Added during execution**, 2026-08-25 (session 1), authorized mid-task. Not in the original plan.
Commit `7c30b410`. Reviewed clean.

**Why it exists:** qa-insights' init hard-failed when no event-broker client was registered, which no
qa-platform deployment provides. The lookup is now soft, degrading to reconciler-only ingest with one
WARN at init — the single expected WARN in every later verification bar.

- [x] **Step 1: Make the broker lookup soft, with a WARN naming the degradation**
- [x] **Step 2: Prove reconciler-only ingest still initialises**
- [x] **Step 3: Rust gate and commit**

**Consequence for later phases:** no deployment in this plan has event ingest, so the event consumer is
covered only by `MockBroker` tests. See the Open Items section — this is one of the two findings that
belong in a report to a human rather than in this plan.

---

### Task 3b: Resolve the `TestResultDto` OpenAPI collision

**Added during execution**, 2026-08-26. Not in the original plan. Commit `68b67b5b`. Reviewed clean.

**Why it exists:** `TestResultDto` was defined in **both** `qa-insights/dto.rs` and `qa-runs/dto.rs` with
no schema alias, so `register_rest` panicked at boot the moment both gears loaded together. Pre-existing
and unrelated to this plan; Task 3a's fix merely stopped an earlier init failure from masking it. The
framework's own panic message prescribes the remedy, which is why this was ruled rather than escalated.

qa-runs' type became `RunTestResultDto`; qa-insights' kept the original name, since its
`/qa/v1/test-results` surface is the one the UI's `TestResult` type binds to.

- [x] **Step 1: Rename qa-runs' DTO and every reference**
- [x] **Step 2: Confirm the OpenAPI document generates with both gears loaded**
- [x] **Step 3: Rust gate and commit**

---

### Task 4: Container images and the compose stack

**Files:**
- Create: `gears/qa-platform/deploy/docker/qa-platform.Dockerfile`, `gears/qa-platform/deploy/compose/docker-compose.yml`, `gears/qa-platform/deploy/compose/.env.example`
- Reference: `testing/docker/cyberware.Dockerfile`

**Interfaces:**
- Consumes: `gears/qa-platform/config/qa-platform-stack.yaml` (Task 3).
- Produces: services named `postgres` and `gears` on a compose network; `gears` reachable at `gears:8087`. Task 8 adds `ui`, Task 13 adds `keycloak`.

Note: `testing/docker/docker-compose.yml` references a `cf-gears.Dockerfile` that **does not exist** in the tree. Do not try to reuse it. `cyberware.Dockerfile` is the working multi-stage template, and it builds `cf-gears-server`; yours builds `cf-gears-example-server`.

- [x] **Step 1: Write the gears Dockerfile**

Model it on `testing/docker/cyberware.Dockerfile`: same `rust:1.95.0-bookworm` pinned-digest builder, same `protobuf-compiler`/`libprotobuf-dev` install, same workspace copies, and an `ARG CARGO_FEATURES`. Change the build to:

```dockerfile
ARG CARGO_FEATURES=qa-platform,static-authn,static-authz,single-tenant,static-credstore
RUN cargo build --release --bin cf-gears-example-server --features "$CARGO_FEATURES"
```

Runtime stage: a slim Debian with the binary, `gears/qa-platform/config/` copied in, `EXPOSE 8087`, and

```dockerfile
CMD ["/usr/local/bin/cf-gears-example-server", "--config", "/etc/cf-gears/qa-platform-stack.yaml", "run"]
```

Read `cyberware.Dockerfile`'s own runtime stage first and match its base image and user handling rather than choosing your own.

- [x] **Step 2: Write the compose file**

```yaml
services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_USER: ${POSTGRES_USER:-qa}
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:-qa}
      POSTGRES_DB: postgres
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U ${POSTGRES_USER:-qa}"]
      interval: 5s
      timeout: 3s
      retries: 20
    ports:
      - "5432:5432"

  gears:
    build:
      # The compose file now lives at gears/qa-platform/deploy/compose/, so the
      # repo root -- which every COPY in the Dockerfile reaches into -- is four
      # levels up, not two. Verify by building, not by counting.
      context: ../../../..
      dockerfile: gears/qa-platform/deploy/docker/qa-platform.Dockerfile
    environment:
      POSTGRES_HOST: postgres
      POSTGRES_USER: ${POSTGRES_USER:-qa}
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:-qa}
      RUST_LOG: info
    depends_on:
      postgres:
        condition: service_healthy
    ports:
      - "8087:8087"

volumes:
  pgdata:
```

The gears' per-gear databases do not exist in a fresh Postgres. Determine whether the gears create their own database or require it to exist — check what `toolkit-db`'s Postgres path does on a missing `dbname`. If it does not create them, add an init step (a `postgres` `initdb.d` script creating the nine databases is the simplest) and say in your report which it was; **do not leave this to be discovered at first boot.**

- [x] **Step 3: Bring it up and confirm the API answers**

```bash
cd gears/qa-platform/deploy/compose && docker compose up -d --build
curl -fsS localhost:8087/openapi.json | head -c 200
```

Expected: the OpenAPI document. If the build takes a long time, that is the Rust release build and is expected once.

- [x] **Step 4: Commit**

```bash
git add gears/qa-platform/deploy/docker/qa-platform.Dockerfile gears/qa-platform/deploy/compose/
git commit -m "feat(deploy): gears image and the compose stack"
```

---

### Task 4a: The gears container must survive a restart

**Added during execution**, 2026-08-26 (session 3). Not in the original plan. Follows the precedent of
Tasks 2b, 3a, 3b and 5a — a defect in already-committed work, found after its parent task closed.

**Why it exists:** `gears/qa-platform/deploy/docker/entrypoint.sh` (Task 4, commit `1261eeeb`) renders the
database block with **`sed -i`, in place**, on the config inside the container's writable layer. The first
start rewrites `host: "localhost"` → `host: "postgres"`; every subsequent start of that **same container**
finds zero anchors and exits 1 with *"expected exactly one 'host: \"localhost\"' line ... found 0"*. The
container is then permanently unstartable — `docker compose restart`, a daemon restart, a host reboot and
Docker's own restart policy all brick it identically. Only `up --force-recreate` recovers it.

Three reasons this is Important rather than a nuisance:

1. **The error names the wrong cause.** The script's own doc comment says it fails loudly so a failure does
   not "surface many layers away with no clue that the real cause was here" — but on restart it blames
   config drift written by its own previous run, sending the reader to audit a file that is fine.
2. **It is directly on Task 17/18's path.** Task 17 Step 2 mounts this config from a **ConfigMap**, and
   ConfigMap volumes are mounted **read-only** — so `sed -i` there fails on the *first* start, not the
   second. In Kubernetes a container restart inside a pod is routine, so this would surface as a
   CrashLoopBackOff whose cause is three tasks upstream.
3. It cost one diagnosis already.

**The fix:** treat the shipped config as a read-only template and render to a separate writable path,
pointing the server at the rendered file — idempotent by construction, and it works under a read-only
mount. `require_anchor`'s loud failure stays meaningful, because against an unchanging template a zero
count means exactly what the message says.

- [x] **Step 1: Reproduce the failure before changing anything**
- [x] **Step 2: Render template → separate writable path, server pointed at the rendered file**
- [x] **Step 3: Gate — the container starts THREE times and the API answers on each**
- [x] **Step 4: Confirm the rendered values are correct after the *second* start, not merely present**
- [x] **Step 5: Commit**

**Do not "fix" this by making the sed idempotent** (also matching the already-substituted value). That
still writes to a read-only mount, and it weakens the anchor check into something that cannot distinguish
"already rendered" from "genuinely drifted".

---

### Task 4b: Name the Dockerfiles and images for the product

**Added during execution**, 2026-08-26 (session 3), on a **direct instruction from the human partner**.
Not in the original plan. User instructions take precedence over the plan's naming.

**Why it exists:** the partner asked why the file was `gears.Dockerfile` when it builds qa-platform, and
asked for images named for the product, pointing at `gears/mini-chat/deploy/docker/` as the reference.

The convention, read from the repo rather than assumed:

- `gears/mini-chat/deploy/docker/mini-chat.Dockerfile` — the Dockerfile is named for the **product**, not
  the binary. Both mini-chat Dockerfiles build the same `cf-gears-example-server` that qa-platform's does,
  so "it builds the gears binary" was never a reason to call it `gears.Dockerfile`.
- `Makefile:674` — `MINI_CHAT_IMAGE ?= cf-gears-mini-chat`. Images are `cf-gears-<product>`.

qa-platform violated both. Worse, `docker-compose.yml` declared **no `image:` key at all**, so Compose
derived the image name from the directory plus the service — `docker ps` showed **`compose-gears`**.

- [x] **Step 1: Find every reference before moving anything**
- [x] **Step 2: `git mv` the Dockerfile and update every reference**
- [x] **Step 3: Add the explicit image name to the compose service**
- [x] **Step 4: Prove the stack still builds and runs under the new names**
- [x] **Step 5: Commit**

**Result:** `docker images` reports `cf-gears-qa-platform:latest`, `compose-gears` is gone (the stale
pre-rename image was deleted by the controller after the reviewer found it still on the machine), and
`./smoke.sh` exits 0. `git mv` preserved history — `git log --follow` walks all pre-rename commits.

**Forward-looking renames applied to this plan's text**, for files that did not exist yet, so later tasks
build them right rather than needing a second rename: Task 12's `ui.Dockerfile` → `qa-platform-ui.Dockerfile`,
and Task 17/18's `values.yaml` image repositories → `cf-gears-qa-platform` / `cf-gears-qa-platform-ui`
(including Task 18's `docker build -t` and `kind load docker-image` lines, which carry the names inside a
bash block rather than in YAML).

**Suggested, not done** (the partner did not ask, and it is real new scope): mini-chat also ships
`mini-chat-docker`/`mini-chat-helm` Makefile targets and a `mini-chat-prebuilt.Dockerfile` that builds the
binary on the host and only packages it in the image. qa-platform has neither, which is why every image
rebuild here costs a full in-container Rust recompile (~2.5 min). The prebuilt variant would make that
seconds on Linux and would also dissolve the deferred `COPY gears ./gears` cache-invalidation finding.

---

### Task 5: The smoke script — Phase A's gate, and Phase D's

**Files:**
- Create: `gears/qa-platform/deploy/compose/smoke.sh`

**Interfaces:**
- Consumes: a running stack on a base URL.
- Produces: `smoke.sh [BASE_URL]`, exit 0 only if the whole flow produced data. Task 17 reuses it unchanged against the chart's ingress.

- [x] **Step 1: Write the script**

`bash`, `set -euo pipefail`, `BASE_URL="${1:-http://localhost:8087}"`, `jq` for assertions. It must, in order:

1. `POST /qa/v1/test-repos` — register a repo (a public git URL, or the fixture repo the qa-catalog tests use; check that gear's tests for a URL that works offline).
2. `GET /qa/v1/plans` — assert at least one plan, after `POST /qa/v1/test-repos/{id}/sync`.
3. `POST /qa/v1/platforms` — one platform.
4. `POST /qa/v1/runs` — launch, capture the run id.
5. Poll `GET /qa/v1/runs/{id}` until the run leaves `queued`, **timeout 60s and fail loudly**. This is what proves the dispatcher is running.
6. Poll until it reaches a terminal state. The mock executor is deterministic, so this is fast.
7. `GET /qa/v1/test-results?$top=5` — **assert non-empty.** This is what proves ingest ran.
8. `GET /qa/v1/dashboard` — assert `total_runs >= 1`.
9. `GET /qa/v1/analytics/overview` — assert the summary counts are non-zero.
10. `GET /qa/v1/dashboard/coverage` — assert it answers 200.

Every assertion prints what it expected and what it got. A silent `exit 1` in a gate script is a gate nobody can debug.

- [x] **Step 2: Run it against the compose stack**

```bash
cd gears/qa-platform/deploy/compose && ./smoke.sh
```

Expected: every step prints a value and the script exits 0. **This is Phase A's gate.** If step 7 or 9 comes back empty, the grants are not working and Task 3 Step 4 was accepted on insufficient evidence — go back rather than adjusting the assertion.

- [x] **Step 3: Commit**

```bash
git add gears/qa-platform/deploy/compose/smoke.sh
git commit -m "test(deploy): end-to-end smoke script from launch to analytics"
```

---

### Task 5a: Stop the reprojection holding a transaction across a cross-gear read

**Added during execution**, 2026-08-26. Not in the original plan. Commit `2268cd95`. Reviewed clean
after one fix round. **An explicit human-authorized exception to the "backend is feature-frozen"
Global Constraint** — the partner chose it over wiring event ingest or descoping Phase A's gate.

**Why it exists:** Task 5 could not make Phase A's gate pass. `qa-insights`' `reproject()` opened a
transaction on its own database and then, inside the closure, read across gears into qa-runs. In a
single-binary deployment that read goes through an in-process client whose `db.conn()` trips
`toolkit-db`'s **task-local** transaction guard (`libs/toolkit-db/src/secure/db.rs`) — a guard that
fires on `conn()` against *any* `Db` instance while a transaction is open on the task. So a legitimate
read of a *different* gear's database was indistinguishable from the bypass the guard exists to catch.

`POST /qa/v1/insights/rebuild` returned `{"scanned":2,"replayed":0}` with HTTP 200, and `sweep()` — the
periodic reconciler ticker — had been failing identically on **every tick since boot**, its per-run
failures logged and swallowed. Confirmed: `test-results` empty and `dashboard.total_runs: 0` on a stack
that had just completed a run.

**Why the existing tests missed it, which is the transferable part:** `reconcile_tests.rs` runs against
real repositories and the *real* transaction provider — only qa-runs and the PDP are doubled. But
`FakeRuns` answers from memory and never calls `db.conn()`, so it never tripped the guard. The
well-intentioned double was what made the bug invisible.

- [x] **Step 1: Arm the qa-runs double to trip the real guard (opt-in, so the rest of the suite is unchanged)**
- [x] **Step 2: Watch it fail with the guard's own message, not something incidental**
- [x] **Step 3: Split `reproject_run` into `read_run_projection` + `write_run_projection`**
- [x] **Step 4: Argue the race the split introduces, and document the conclusion**
- [x] **Step 5: Update every doc comment describing the old control flow**
- [x] **Step 6: Rust gate — fmt, clippy, crate tests (716+4), workspace `--lib --bins --tests` (11310)**
- [x] **Step 7: Commit as an authorized exception to the freeze**

**Race conclusion:** no new race. The cross-gear read never had this gear's transaction as a consistency
boundary (it reads a different database); the write is an idempotent delete-then-insert of one run's
whole state; and the watermark advance uses `finished_at` from the *initial listing*, never from the
reprojection read — so a stale read cannot push the watermark past unaccounted-for state.

**Latent hazard left in place, deliberately:** `infra::events::consumer` still composes the unsplit
`reproject_run` inside its own transaction, so the identical trip awaits it *if* event ingest is ever
wired into a single-binary deployment. Documented at both ends. Fixing it means redesigning that
consumer's offset+projection atomicity, well outside an exception granted for read ordering. Carried to
the Open Items section.

---

# Phase B — the UI serves the stack

Phase B runs with `auth_disabled: true`. No login, no tokens.

### Task 6: The field-level contract diff

**Files:**
- Create: `gears/qa-platform/docs/CONTRACT-DIFF.md`

**Interfaces:**
- Produces: the mismatch table Tasks 9, 10 and 11 implement against. This task writes **no code**.

**Four field-level facts Task 5 already established**, by driving these endpoints for real rather than
reading the spec. Each is a case where the plan's own endpoint list disagrees with `/openapi.json`, and
the document this task produces must carry them:

1. **`product_id` is required on repo creation**, and the plan's Task 5 step list omits the product step
   **entirely** — so a product must be created first. A whole entity the plan never mentions.
2. **`GET /qa/v1/plans` takes required `repo_id` + `branch` query parameters**, not a filter. Legacy's
   hook signature will not map onto it unchanged.
3. **`platform_id` must be non-null on launch**, or the run is accepted and then never queued — a silent
   dead end rather than a validation error.
4. **`analytics/overview` requires `product_id`, `version` and `scope`.**

And one contract gap, which is a **"cannot be absorbed"** candidate for Step 3 rather than an adapter
problem: `analytics/overview`'s `passed`/`failed` are **permanently 0** in this deployment because
app_version is never set — version observation is a separate feature this plan does not build. A
dashboard panel bound to those two fields renders zeros on a *working* stack, which is exactly the
"plausible-looking zero" the Global Constraints call worse than a missing panel. Decide deliberately
whether the UI shows the panel, hides it, or labels it unavailable — and do not invent a default.

The **path-level** diff is already done and lives in spec §6.3 — do not redo it. Seven reshapes, seven dead hooks, two DTO-field cases, three removals. What is missing is the **field level**: request bodies, response shapes, enum spellings, date formats, pagination envelopes.

- [x] **Step 1: Get both sides in a comparable form**

```bash
cd gears/qa-platform/deploy/compose && docker compose up -d
curl -fsS localhost:8087/openapi.json > /tmp/qa-openapi.json
python3 /home/serhii/Jelastic/projects/fabric/gears-rust/tools/scripts/sort_openapi_json.py /tmp/qa-openapi.json
```

The legacy side is `../vhp-testrunner/manager-ui/src/api/types.ts` (897 lines) and the call sites in `hooks.ts` (1,419 lines).

- [x] **Step 2: Write the table, one row per legacy hook**

For each exported hook in legacy `hooks.ts`, one row: hook name, legacy path + method, gear path + method, and a **Difference** column that is one of:

- `identical` — same path, same request, same response fields
- `reshaped: <what>` — cite the spec §6.3 row, or add a new one
- `field: <legacy field> -> <gear field>` — including type and nullability changes
- `enum: <legacy values> -> <gear values>`
- `envelope: <legacy> -> <gear>` — e.g. a bare array becoming `Page<T>`
- `absent` — no gear equivalent (should only be the seven dead hooks and three removals; **any new `absent` is a finding**)

- [x] **Step 3: Separate what the UI can absorb from what it cannot**

End the document with two lists:

1. **Absorbable in `hooks.ts`** — the adapter work Task 10 does.
2. **Cannot be absorbed** — a field the UI renders that the gears do not serve. **For each, stop and report it.** Do not widen a gear (Global Constraints) and do not invent a default; a plausible-looking zero in a dashboard is worse than a missing panel. If this list is non-empty, say so in your report status as DONE_WITH_CONCERNS and name each item.

- [x] **Step 4: Commit**

```bash
git add gears/qa-platform/docs/CONTRACT-DIFF.md
git commit -m "docs(qa-platform): field-level contract diff, legacy UI against the gears"
```

---

### Task 7: Copy the UI in and make it build

**Files:**
- Create: `gears/qa-platform/qa-platform-ui/**` (copied)
- Modify: `Makefile`, `.gitignore`

**Interfaces:**
- Produces: a building UI at that path; `make ui-build` succeeds. No API changes yet — this task deliberately leaves `src/api/` pointing at `/api`.

- [x] **Step 1: Copy, excluding build output and dependencies**

```bash
cd /home/serhii/Jelastic/projects/fabric
rsync -a --exclude node_modules --exclude dist \
  vhp-testrunner/manager-ui/ \
  gears-rust/gears/qa-platform/qa-platform-ui/
```

Keep `package.json`, `package-lock.json`, `vite.config.ts`, `tsconfig*.json`, `tailwind.config.ts`, `postcss.config.js`, `components.json`, `index.html`, `public/`, `src/`, `.env.example`, `nginx.conf`, `Dockerfile`. **Copy `package-lock.json`** — a fresh resolve would silently change versions on this repo's first frontend.

- [x] **Step 2: Confirm it builds unchanged**

```bash
cd gears-rust/gears/qa-platform/qa-platform-ui && npm ci && npm run build
```

Expected: PASS. If it fails, the failure is inherited from legacy and fixing it is part of this task — record what it was, because a pre-existing break says something about the copy's fidelity.

- [x] **Step 3: Add the Makefile targets**

The Makefile already depends on `npx` (the `slides` target), so Node is an assumed tool. Follow the surrounding style, including the `command -v` guard that target uses:

```make
# -------- qa-platform UI --------

.PHONY: ui-install ui-lint ui-build

UI_DIR := gears/qa-platform/qa-platform-ui

## Install UI dependencies from the lockfile
ui-install:
	@command -v npm >/dev/null || (echo "npm is required for the qa-platform UI" && exit 1)
	cd $(UI_DIR) && npm ci

## Lint the UI
ui-lint: ui-install
	cd $(UI_DIR) && npm run lint

## Type-check and build the UI
ui-build: ui-install
	cd $(UI_DIR) && npm run build
```

Add `gears/qa-platform/qa-platform-ui/node_modules/` and `.../dist/` to `.gitignore`.

- [x] **Step 4: Verify through the Makefile**

```bash
make ui-build
```

Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-platform-ui Makefile .gitignore
git commit -m "feat(qa-platform-ui): import the legacy manager-ui verbatim"
```

Commit the verbatim copy **on its own**, before any adaptation. Every later diff is then readable as "what the port changed", which is the property that makes the components-untouched claim checkable.

---

### Task 8: Remove the three surfaces with no backend

**Files:**
- Delete: `src/pages/settings/SettingsRepoPollerPage.tsx`, `SettingsReportPortalPage.tsx`, `SettingsRunnerDefaultsPage.tsx`
- Modify: `src/components/layout/Sidebar.tsx`, `src/App.tsx`, `src/api/hooks.ts`, `src/api/types.ts`, `src/pages/RunDetailPage.tsx`, `src/pages/TestDetailPage.tsx`
- Create: `gears/qa-platform/qa-platform-ui/REMOVED-SURFACES.md`

**Interfaces:**
- Consumes: spec §6.5's table.
- Produces: a UI whose every nav entry works. **This is the only task permitted to modify `src/components/` or `src/pages/`.**

- [x] **Step 1: Write the re-add record first**

`REMOVED-SURFACES.md`, one section per removal: what was removed, the exact legacy endpoint it needed, the files touched, and the commit SHA (fill after committing). Written first so it is a record and not a reconstruction.

- [x] **Step 2: Delete the three pages and their routes and nav entries**

Remove from `Sidebar.tsx` the entries for `/settings/repo-poller`, `/settings/reportportal`, `/settings/runner-defaults`; remove the matching `<Route>` elements and imports from `App.tsx`; delete the three page files.

- [x] **Step 3: Strip the ReportPortal references from the two detail pages**

`RunDetailPage.tsx` has 2, `TestDetailPage.tsx` has 1. Remove the affordance, not the surrounding panel — and do not leave a dangling label or an empty flex child that shifts the layout.

- [x] **Step 4: Remove the now-dead hooks and types — THREE ONLY, not ten**

> **AMENDED 2026-08-26 (session 3), Ruling A.** This step originally also deleted the seven hooks spec
> §6.3 category 3 names (`/tests/source`, `/platforms/{id}/refresh-version`, `/platforms/{id}/details`,
> `/plans/{id}/run-test`, `/plans/{id}/runs`, `/git-plans`, `/runs/{id}/dag`), on §6.3's claim that they
> "appear only in `hooks.ts`, with no component consumer anywhere in the 114 files" and have "Zero UI
> impact". **Task 6 measured that claim false for all seven**, and the task reviewer independently
> re-grepped every identifier and confirmed each consumer's file:line. Deleting them breaks six routed
> pages. The seven are therefore **out of this task**: `/plans/{id}/run-test` is served after all
> (`POST /qa/v1/runs` with `target.kind="test"`, CONTRACT-DIFF §5-D) and Task 10 remaps it; the other six
> become Task 8a's C1, C3 and C6.
>
> This is not the plan being overridden — it is this step's own instruction being obeyed. The original
> text said: "Before deleting each, grep the whole `src/` for its identifier. If anything outside
> `hooks.ts` uses one, it is **not** dead — stop and report it, because spec §6.3's category-3 claim
> would then be wrong." That grep was run; the claim is wrong; this is the report.

From `hooks.ts`, remove **only** the hooks for the three removed settings endpoints, and their `types.ts`
entries. Nothing else.

Before deleting each, grep the whole `src/` for its identifier. If anything outside `hooks.ts` uses one, it
is **not** dead — stop and report it. (For these three the removal is safe by construction: Step 2 deleted
their only consumers, the three settings pages.)

- [x] **Step 5: Build and check the tree is clean of references**

```bash
make ui-build
cd gears/qa-platform/qa-platform-ui && grep -rn "repo-poller\|reportportal\|runner-defaults\|ReportPortal" src/ || echo "clean"
```

Expected: build PASSES (TypeScript catches a missed import), and the grep prints `clean`.

- [x] **Step 6: Commit**

```bash
git add -A gears/qa-platform/qa-platform-ui
git commit -m "feat(qa-platform-ui): remove the three settings surfaces the gears do not serve"
```

Then put the SHA into `REMOVED-SURFACES.md` and amend, so the record points at its own commit.

---

### Task 8a: The ten surfaces the gears do not serve

**Added during execution**, 2026-08-26 (session 3), by Ruling B. Not in the original plan.

**Files:**
- Modify: `src/components/**`, `src/pages/**` (per-item, see the table), `src/api/hooks.ts`, `src/api/types.ts`
- Modify: `gears/qa-platform/qa-platform-ui/REMOVED-SURFACES.md` (Task 8 created it)

**Interfaces:**
- Consumes: `gears/qa-platform/docs/CONTRACT-DIFF.md` §8 (C1–C10) and §9.
- Produces: a UI that renders nothing false. **This task and Task 8 are the only tasks permitted to modify
  `src/components/` and `src/pages/`** — the plan's original exclusivity is extended to cover this task,
  because this is the same kind of work Task 8 does and there is nowhere else it can live.

Task 6 found ten fields the UI renders that the gears do not serve. The Global Constraints forbid widening
a gear to fill any of them and forbid inventing a default, but every one has a live consumer that will
render *something* in Phase B. This task decides what.

**The decision rule** — derived from the plan's own precedents, not invented for this task:

1. **Never render a plausible-looking wrong or empty value.** (Global Constraints: a plausible-looking zero
   in a dashboard is worse than a missing panel.)
2. **A whole surface with no backend at all → remove it**, recorded in `REMOVED-SURFACES.md` with the exact
   endpoint it needed. This is what spec §6.5 already does for the three settings pages.
3. **An affordance inside an otherwise-working page → disable it with a visible "not available in this
   deployment" state.** Additive, not subtractive. This is CONTRACT-DIFF §9's precedent.
4. **A gap that yields silently WRONG data must not ship in any form.** A label cannot fix an invisible
   failure.
5. Where a rule leaves a genuine choice, prefer the option that a human can reverse with one edit, and say
   in `REMOVED-SURFACES.md` what the other option was.

**The ten, and the rule each falls under:**

| Item | What the UI promises | Rule | Decision |
|---|---|---|---|
| C1 test catalog | `title`, `component`, `tags`, `quality_vectors`, `loc`, source text | 2 | remove `/tests` and `/tests/view` |
| C2 per-test logs | `test_results[].logs` on run detail | 3 | disable, labelled; `cases` IS recoverable from `/qa/v1/test-case-results` — recover it |
| C3 platform health | `platforms_summary`, `PlatformDetails`, refresh-version | 3 | disable the three affordances, labelled |
| C4 archive repos | multipart upload form | 2 | remove the form; git-URL creation stays |
| C5 delete a run | `useDeleteRun` bound to two different intents | 3 | *stop* maps to `POST /runs/{id}/cancel`; *delete* is disabled, labelled |
| C6 DAG and git-plans | `useRunDag`, `useGitPlans`, `useRunGitPlan`, `CustomPlan.nodes`/`parallelism` | 2 | remove the DAG view and the git-plans list |
| C7 product scoping | product filter on runs/schedules/custom plans | **4** | **see below — the dangerous one** |
| C8 code coverage | `/dashboard/coverage`, documented empty in every deployment | 3 | disable, labelled |
| C9 analytics counters | `passed`/`failed` permanently 0 | 3 | CONTRACT-DIFF §9.3's banner, condition and copy verbatim |
| C10 `secure` variables | Secured checkbox, password input, `********`, padlock | **5** | **remove the affordance — see below** |

- [x] **Step 1: C7 first, because it is the only one that fails silently**

`RunDto`, `ScheduleDto` and `CustomPlanDto` carry no product key, and unknown query parameters are
**ignored rather than rejected** (CONTRACT-DIFF X8), so a product-scoped request returns **HTTP 200 with
the whole tenant's rows**. A user filtering to product A sees product B's runs and nothing errors.

Filter client-side if the response carries a field that identifies the product; if it does not, **remove
the product control** rather than leave it lying. Do not label this one — a label on a control that
appears to work is not a mitigation. Say in your report which of the two it was and why.

- [x] **Step 2: C10 — remove the "Secured" affordance, do not label it**

`VariableDto` and `UpsertVariableReq` have no `secure` field, so a variable a user marks secret is stored
and returned **in cleartext beneath a padlock**. Remove the checkbox, the password input, the `********`
masking and the padlock. Labelling it "unavailable" while still accepting the input keeps the
misrepresentation; removing it is honest about what this deployment can do. This is the only item on the
list with a confidentiality consequence rather than a display one, and it is the one place where leaving
the code as-is is the *unsafe* option rather than the conservative one.

- [x] **Step 3: The removals (C1, C4, C6), each recorded in `REMOVED-SURFACES.md`**

Same standard Task 8 set: what was removed, the exact legacy endpoint it needed, the files touched, the
commit SHA. A removal without its re-add record is half the work.

- [x] **Step 4: The labelled-unavailable set (C2, C3, C5, C8, C9)**

One shared presentational treatment, not five bespoke ones. C9's exact condition and copy are already
written in CONTRACT-DIFF §9.3 — use them verbatim rather than re-deciding them. For C2, recover
`test_results[].cases` from `GET /qa/v1/test-case-results` filtered by `run_id` — only `logs` is
genuinely unavailable.

- [x] **Step 5: Build, and prove no surface renders a false value**

```bash
make ui-build
```

Then grep for every removed identifier and confirm the tree is clean of it.

- [x] **Step 6: Commit**

```bash
git add -A gears/qa-platform/qa-platform-ui
git commit -m "feat(qa-platform-ui): resolve the ten surfaces the gears do not serve"
```

**Every decision in this task is a ruling made on the partner's behalf and is listed in the session
report. C5 and C1 are the two most likely to be overturned** — a human may prefer degrading `/tests` to a
bare path list over removing it, or mapping the Runs page's delete button to cancel. Both are one-line
switches inside the rule above.

---

### Task 9: Generated types and the adapted client

**Files:**
- Create: `src/api/generated/openapi.d.ts` (generated, committed)
- Modify: `src/api/types.ts`, `src/api/client.ts`, `package.json`, `Makefile`

**Interfaces:**
- Consumes: `CONTRACT-DIFF.md` (Task 6).
- Produces: `apiGet/apiPost/apiPut/apiDelete` against `/qa/v1`; `ApiError { status, statusText, message }` unchanged; `setTokenProvider(fn: () => string | null)` — **Task 15 calls this and nothing else.**

- [ ] **Step 1: Generate the types**

```bash
cd gears/qa-platform/qa-platform-ui
npm i -D openapi-typescript
npx openapi-typescript http://localhost:8087/openapi.json -o src/api/generated/openapi.d.ts
```

Add the script to `package.json`:

```json
"scripts": {
  "gen:api": "openapi-typescript http://localhost:8087/openapi.json -o src/api/generated/openapi.d.ts"
}
```

- [ ] **Step 2: Reduce `types.ts` to re-exports**

Keep every type name components import, aliasing each to its generated counterpart, so no component import changes:

```ts
// Wire types are GENERATED from the gears' OpenAPI document — see `make ui-contract`.
// This file exists only so component imports keep working; do not hand-edit shapes here.
import type { components } from './generated/openapi';

type S = components['schemas'];

export type Run = S['RunDto'];
export type TestResult = S['TestResultDto'];
// ...one line per type legacy's types.ts exported and a component still imports.
```

For each legacy type with no generated counterpart, consult `CONTRACT-DIFF.md`: if the type belonged to a removed surface it is deleted (Task 8 should already have), and if it is a UI-only view model it **stays hand-written** in `types.ts` with a comment saying so. Do not force a UI-only type through the generated namespace.

- [ ] **Step 3: Adapt `client.ts`**

```ts
const API_BASE_URL = import.meta.env.VITE_API_URL || '/qa/v1';

// Set by src/auth in Phase C. Until then it stays null and no header is sent,
// which is what lets Phases A and B run with `auth_disabled: true`.
let tokenProvider: (() => string | null) | null = null;

export function setTokenProvider(fn: () => string | null): void {
  tokenProvider = fn;
}

function authHeaders(): Record<string, string> {
  const token = tokenProvider?.() ?? null;
  return token ? { Authorization: `Bearer ${token}` } : {};
}
```

`handleResponse` gains one behaviour: on a non-OK response whose body is the gears' canonical error envelope, put the envelope's human-readable message into `ApiError.message` rather than the raw JSON. Get the envelope's exact field names from the OpenAPI document — **do not guess them**; every gear's errors go through one `From<DomainError> for CanonicalError`. Keep the plain-text fallback for bodies that are not that envelope.

- [ ] **Step 4: Add the contract target**

```make
## Regenerate UI types from a running gears stack and fail if they drift
ui-contract: ui-install
	cd $(UI_DIR) && npm run gen:api
	git diff --exit-code -- $(UI_DIR)/src/api/generated/openapi.d.ts \
	  || (echo "UI wire types are stale: regenerate with 'make ui-contract' and commit" && exit 1)
```

- [ ] **Step 5: Build, and prove the contract target can fail**

```bash
make ui-build
make ui-contract          # expect: clean
```

Then break it deliberately: add a line to `src/api/generated/openapi.d.ts`, re-run `make ui-contract`, and confirm it **fails**. Revert. Record both outputs in your report — a check that has never failed has not been shown to work.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-platform-ui Makefile
git commit -m "feat(qa-platform-ui): generated wire types and a /qa/v1 client"
```

---

### Task 10: Remap `hooks.ts` onto the gear routes

**Files:**
- Modify: `src/api/hooks.ts`
- Create: `src/api/adapters.ts` (the pure reshape helpers), `src/api/adapters.test.ts`

**Interfaces:**
- Consumes: `CONTRACT-DIFF.md`, the generated types, `client.ts` (Tasks 6, 9).
- Produces: `unwrapPage`, `observedFromPlatforms`, `recentResultsQuery` in `adapters.ts` — pure, tested, and called from the hooks.
- Produces: **every exported hook keeps its name, parameters and return shape.** That is what keeps components untouched, and it is the reviewable property of this task.

This is the task where the real work is. Work through `CONTRACT-DIFF.md` row by row.

- [ ] **Step 1: The seven reshapes from spec §6.3**

Each is a concrete edit:

1. **Schedule suspend/resume → one PUT.** `useSuspendSchedule`/`useResumeSchedule` keep their names and both call `PUT /qa/v1/schedules/{id}` with the full schedule and `enabled: false`/`true`. `enabled` is **required** on that DTO — `qa-runs/src/api/rest/dto.rs:915` explains that an omitted field would be a silent disable and a defaulted one a silent re-enable. So these hooks must send the schedule's other current values too; take them from the hook's existing argument or fetch-then-PUT, and say in a comment which and why.
2. **Cancel moved:** `/run-queue/{id}/cancel` → `POST /qa/v1/runs/{id}/cancel`.
3. **Analytics plan drill-downs:** `/analytics/plan/{id}/{builds,tests,test-history}` → `GET /qa/v1/analytics/plan/{builds,tests,test-history}` with plan identity in the **query**. Get the parameter's exact name and shape from the OpenAPI document.
4. **Coverage moved:** `/products/{id}/coverage` → `GET /qa/v1/dashboard/coverage`. Check whether it still takes a product filter; if not, the hook's product argument becomes unused — **remove the parameter and update its call sites**, which is the one narrow exception to "components untouched" that a signature change forces. Report it explicitly.
5. **Recent results → OData:** `/tests/recent-results?file=&limit=` → `GET /qa/v1/test-results` with `$filter` on the file and `$top` for the limit. Get the filterable field names from `qa-insights`' OData field enum, not from guesswork.
6. **Variables unified:** `/settings/variables` and `/platforms/{id}/variables` both → `/qa/v1/variables`. If the platform-scoped one needs a platform filter the gear does not offer, that is a `CONTRACT-DIFF.md` "cannot be absorbed" item.
7. **JIRA split:** `/runs/{id}/jira` → `POST /qa/v1/jira/bugs` for filing, `GET /qa/v1/jira/open-bugs` for listing, the latter keyed on **`(repo_id, plan_path)`, not `plan_id`**.

- [ ] **Step 1a: Two acceptance conditions Task 8a established — ADDED session 3**

Neither is optional, and neither is visible from `CONTRACT-DIFF.md`'s table alone.

1. **`included_plans` must expand into `files`.** Task 8a kept the custom-plan editor's "Whole plans" tab,
   because unlike a node graph an included plan *is* expressible in `files`. But the tab is a **save path**:
   if this adapter does not expand `included_plans` into the `files` the gears accept, a plan composed of
   whole plans is saved successfully and then **runs nothing** — a control that appears to work and does
   not, which is C7's shape on the write side. Expanding it is this task's job.
   `gears/qa-platform/qa-platform-ui/src/lib/customPlanTests.ts` is where the plan→tests logic already
   lives. If expansion turns out to be impossible against the real DTOs, that is a **stop and report**, not
   a silent partial save.

2. **`useDeleteRun` maps to `POST /qa/v1/runs/{id}/cancel`.** `CONTRACT-DIFF` originally listed this as
   "cannot be absorbed" on the strength of a local variable named `deleteRun`. Both consumers actually mean
   *stop* (`RunsPage.tsx`'s handler is `handleStop`, its dialog and button both say "Stop run"), and the
   gears serve exactly that. The row was corrected in session 3; the hook keeps its legacy name and adapts
   inside, like every other hook in this task.

- [ ] **Step 2: The two DTO-field cases**

`useObservedBranches`/`useObservedVersions` keep their names but derive from `GET /qa/v1/platforms`, reading `observed_version` and `observed_build` off the platform DTO (`qa-environments/src/api/rest/dto.rs:27-28`). Their return shape stays what the components expect.

- [ ] **Step 3: Everything else in the table**

Field renames, enum spellings, and envelope changes — in particular any hook whose legacy response was a bare array and whose gear response is `Page<T>`: unwrap in the hook so the component still receives an array, and comment that the pagination is being discarded and where a real paged UI would go.

- [ ] **Step 4: Unit-test the adapters that actually transform data**

Spec §10 requires tests for the three units with real logic, and the adapters in this file are one of
them. Most hooks are a URL change and need no test. **Test exactly the ones that reshape data**, because
those are where a silent wrong answer is possible:

```ts
// src/api/adapters.test.ts
import { unwrapPage, observedFromPlatforms, recentResultsQuery } from './adapters';

it('unwraps a Page envelope into the bare array components expect', () => {
  expect(unwrapPage({ value: [{ id: 'a' }], count: 1 })).toEqual([{ id: 'a' }]);
});

it('treats an empty page as an empty array rather than undefined', () => {
  // A component that maps over undefined crashes; over [] it renders "no data".
  expect(unwrapPage({ value: [], count: 0 })).toEqual([]);
});

it('derives observed branches from the platform DTO fields', () => {
  const platforms = [
    { id: '1', observed_version: '7.0', observed_build: '7.0.1' },
    { id: '2', observed_version: null, observed_build: null },
  ];
  // Nulls are dropped, not rendered as "null".
  expect(observedFromPlatforms(platforms).map((o) => o.version)).toEqual(['7.0']);
});

it('builds an OData query for recent results from file and limit', () => {
  const q = recentResultsQuery({ file: 'tests/a.py', limit: 5 });
  expect(q).toContain('$top=5');
  expect(q).toContain('tests/a.py');
});

it('escapes a quote in the filter value rather than producing invalid OData', () => {
  expect(() => recentResultsQuery({ file: "it's.py", limit: 5 })).not.toThrow();
  expect(recentResultsQuery({ file: "it's.py", limit: 5 })).not.toContain("it's.py'");
});
```

Extract those three helpers into `src/api/adapters.ts` as you write Steps 1-3 — a pure function per
reshape, called from the hook. That is what makes them testable at all, and it keeps `hooks.ts` a list
of calls rather than a mix of calls and logic. Add a test for any *other* helper you extract that
transforms rather than forwards.

Run:

```bash
cd gears/qa-platform/qa-platform-ui && npx vitest run src/api/adapters.test.ts
```

Expected: FAIL first (module missing), then PASS 5/5 once the helpers exist. If Task 11 has not yet
added the vitest runner, add it here instead and say so — whichever task lands first owns it.

- [ ] **Step 5: Build, then verify no exported hook changed shape**

```bash
make ui-build
```

Expected: PASS with **zero changes under `src/components/`**. Then:

```bash
cd /home/serhii/Jelastic/projects/fabric
diff <(grep -oE "^export (function|const) use[A-Za-z]+" vhp-testrunner/manager-ui/src/api/hooks.ts | sort) \
     <(grep -oE "^export (function|const) use[A-Za-z]+" gears-rust/gears/qa-platform/qa-platform-ui/src/api/hooks.ts | sort)
```

Expected: differences are **only** removals from Task 8. Any *renamed* hook is a defect in this task — put the name back and adapt inside.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-platform-ui/src/api/hooks.ts gears/qa-platform/qa-platform-ui/src/api/adapters.ts gears/qa-platform/qa-platform-ui/src/api/adapters.test.ts
git commit -m "feat(qa-platform-ui): remap every hook onto the gear routes"
```

---

### Task 11: The SSE log stream

**Files:**
- Create: `src/hooks/useRunLogStream.ts`
- Delete: `src/hooks/useWebSocket.ts`
- Modify: `src/components/runs/LogViewer.tsx`
- Test: `src/hooks/useRunLogStream.test.ts`

**Interfaces:**
- Produces: `useRunLogStream(runId: string | null, opts?: { onMessage?: (line: string) => void })` returning `{ isConnected: boolean, messages: string[] }` — **the same shape `useWebSocket` returned**, so `LogViewer` changes only its import and hook name.

The gear serves SSE (`qa-runs/src/api/rest/routes/runs.rs:163-165`), the legacy UI speaks WebSocket. This is a required rewrite.

- [ ] **Step 1: Read both sides before writing**

Read `useWebSocket.ts` (its options, reconnect behaviour and cleanup) and `LogViewer.tsx:178` (what it destructures). Then read the gear's handler to learn the event names and whether a terminal event closes the stream — a hook that reconnects forever after a run finishes is the obvious failure here.

- [ ] **Step 2: Write the failing test**

```ts
import { renderHook, act } from '@testing-library/react';
import { useRunLogStream } from './useRunLogStream';

class FakeEventSource {
  static last: FakeEventSource | null = null;
  onmessage: ((e: MessageEvent) => void) | null = null;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;
  constructor(public url: string) { FakeEventSource.last = this; }
  close() { this.closed = true; }
}

beforeEach(() => {
  FakeEventSource.last = null;
  (globalThis as unknown as { EventSource: unknown }).EventSource = FakeEventSource;
});

it('opens no stream until a run id is given', () => {
  renderHook(() => useRunLogStream(null));
  expect(FakeEventSource.last).toBeNull();
});

it('accumulates lines and reports the connection state', () => {
  const { result } = renderHook(() => useRunLogStream('run-1'));
  const es = FakeEventSource.last!;
  expect(es.url).toContain('/qa/v1/runs/run-1/logs');
  act(() => { es.onopen?.(); });
  expect(result.current.isConnected).toBe(true);
  act(() => { es.onmessage?.({ data: 'first' } as MessageEvent); });
  act(() => { es.onmessage?.({ data: 'second' } as MessageEvent); });
  expect(result.current.messages).toEqual(['first', 'second']);
});

it('closes the stream when the run id goes away', () => {
  const { rerender } = renderHook(({ id }) => useRunLogStream(id), {
    initialProps: { id: 'run-1' as string | null },
  });
  const es = FakeEventSource.last!;
  rerender({ id: null });
  expect(es.closed).toBe(true);
});

it('closes the stream on unmount rather than leaking it', () => {
  const { unmount } = renderHook(() => useRunLogStream('run-1'));
  const es = FakeEventSource.last!;
  unmount();
  expect(es.closed).toBe(true);
});
```

This is the repo's first UI test, so this task also adds the runner: `vitest` + `@testing-library/react` + `jsdom`, a `test` script, and a `make ui-test` target in the same style as `ui-build`.

- [ ] **Step 3: Run and watch it fail**

```bash
cd gears/qa-platform/qa-platform-ui && npx vitest run src/hooks/useRunLogStream.test.ts
```

Expected: FAIL — the module does not exist.

- [ ] **Step 4: Implement the hook**

`EventSource` on `${API_BASE_URL}/runs/${runId}/logs`, appending `event.data` to state, `onopen`/`onerror` driving `isConnected`, and a cleanup that calls `close()` on unmount and whenever `runId` changes. Close on the gear's terminal event if it sends one. Keep the returned object's field names identical to `useWebSocket`'s.

- [ ] **Step 5: Run and watch it pass, then switch `LogViewer`**

```bash
npx vitest run src/hooks/useRunLogStream.test.ts
```

Expected: PASS 4/4. Then change `LogViewer.tsx`'s import and hook call, delete `useWebSocket.ts`, and:

```bash
make ui-build && grep -rn "useWebSocket\|new WebSocket" src/ || echo "clean"
```

Expected: build PASSES, grep prints `clean`. `LogViewer.tsx`'s "WebSocket connected" label (line ~275) is now wrong — change the text.

- [ ] **Step 6: Commit**

```bash
git add -A gears/qa-platform/qa-platform-ui Makefile
git commit -m "feat(qa-platform-ui): stream run logs over SSE instead of WebSocket"
```

---

### Task 12: The UI in compose

**Files:**
- Create: `gears/qa-platform/deploy/docker/qa-platform-ui.Dockerfile`
- Modify: `gears/qa-platform/qa-platform-ui/nginx.conf`, `gears/qa-platform/deploy/compose/docker-compose.yml`

**Interfaces:**
- Consumes: the built UI (Tasks 7-11), the `gears` service (Task 4).
- Produces: a `ui` service on `localhost:8080` proxying `/qa/v1` to `gears:8087`.

- [ ] **Step 1: Retarget `nginx.conf`**

Three changes to legacy's file:

1. Delete the `resolver kube-dns.kube-system.svc.cluster.local` line and the `set $backend` indirection — those exist for Kubernetes DNS. In compose the service name resolves directly.
2. `location /api/` becomes `location /qa/v1/`, `proxy_pass http://gears:8087;`.
3. **Add `proxy_buffering off;` to the log-stream location.** Without it nginx batches SSE events and the log viewer updates in lumps. Give `/qa/v1/runs/` its own location block for this, and keep the long read timeout legacy already has.

Keep the SPA `try_files`, the gzip block and the static-asset caching exactly as they are.

- [ ] **Step 2: Write the UI Dockerfile**

Legacy's `manager-ui/Dockerfile` is the template and is already correct in shape (node build → nginx). Copy it to `gears/qa-platform/deploy/docker/qa-platform-ui.Dockerfile` with two fixes: `COPY package.json package-lock.json ./` and `npm ci` instead of `COPY package.json` + `npm install`, so the image build uses the lockfile the repo commits.

- [ ] **Step 3: Add the service**

```yaml
  ui:
    build:
      # Both paths are relative to this compose file at
      # gears/qa-platform/deploy/compose/; `dockerfile` is then relative to
      # `context`. Recompute them against the real tree rather than trusting
      # these -- they moved once already.
      context: ../../qa-platform-ui
      dockerfile: ../deploy/docker/qa-platform-ui.Dockerfile
    depends_on:
      - gears
    ports:
      - "8080:80"
```

Check that relative `dockerfile:` path against your compose version; if it is awkward, set `context: ../..` and adjust the Dockerfile's `COPY` paths instead, and say which you chose.

- [ ] **Step 4: Bring it up and click through every page**

```bash
cd gears/qa-platform/deploy/compose && docker compose up -d --build && ./smoke.sh
```

Then open `http://localhost:8080` and visit **every** nav entry. **This is Phase B's gate.** For each page record: renders with data / renders empty / errors. `smoke.sh` has created a run, results and analytics rows, so "empty" is a finding, not a state.

A page that errors is a `hooks.ts` bug (Task 10) or a `CONTRACT-DIFF.md` row that was missed — fix it here rather than deferring, and note which.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/deploy/ gears/qa-platform/qa-platform-ui/nginx.conf
git commit -m "feat(deploy): serve the UI from compose against the gears"
```

---

# Phase C — real authentication

Auth is last on purpose: a failure in Phases A and B is then a data or contract failure and never an auth failure.

### Task 13: Keycloak, and the first OIDC configuration in this repo

**Files:**
- Create: `gears/qa-platform/deploy/compose/keycloak/realm-qa-platform.json`
- Modify: `gears/qa-platform/deploy/compose/docker-compose.yml`, `gears/qa-platform/config/qa-platform-stack.yaml`

**Interfaces:**
- Produces: a `keycloak` service on `localhost:8180`, realm `qa-platform`, public PKCE client `qa-platform-ui`, users `admin`/`viewer`; the gears validating real JWTs.
- Consumed by: Tasks 14, 15, 16.

**No config in this repo uses `oidc-authn-plugin` today** — this is the first. Its config surface is `src/config.rs` in that plugin: `vendor`, `priority`, `jwt` (**required**, `JwtConfigInput`), `discovery_cache`, `jwks_cache`, `http_client`, retry. Read that file before writing YAML; do not guess field names.

- [ ] **Step 1: Add Keycloak with an imported realm**

```yaml
  keycloak:
    image: quay.io/keycloak/keycloak:26.0
    command: ["start-dev", "--import-realm"]
    environment:
      KC_BOOTSTRAP_ADMIN_USERNAME: admin
      KC_BOOTSTRAP_ADMIN_PASSWORD: admin
    volumes:
      - ./keycloak:/opt/keycloak/data/import:ro
    ports:
      - "8180:8080"
    healthcheck:
      test: ["CMD-SHELL", "exec 3<>/dev/tcp/127.0.0.1/8080"]
      interval: 5s
      timeout: 3s
      retries: 30
```

The realm JSON declares: realm `qa-platform` enabled; client `qa-platform-ui` with `publicClient: true`, `standardFlowEnabled: true`, `"attributes": {"pkce.code.challenge.method": "S256"}`, redirect URIs `http://localhost:8080/*` and web origins `http://localhost:8080`; and two users with passwords.

The simplest way to get a correct file is to configure it once in the Keycloak admin UI and use its realm **export**, rather than hand-writing it. Say in your report which you did.

- [ ] **Step 2: Prove the IdP issues a usable token before touching the gears**

```bash
curl -fsS -X POST http://localhost:8180/realms/qa-platform/protocol/openid-connect/token \
  -d grant_type=password -d client_id=qa-platform-ui \
  -d username=admin -d password=admin | jq -r .access_token
```

Expected: a JWT. Decode its payload and record `iss`, `aud`, `sub` and the claim carrying the tenant — Task 14 needs the tenant claim's exact name. (Direct-access grants may need enabling on the client for this step; it is a verification convenience, and the UI uses the authorization-code flow.)

- [ ] **Step 3: Configure the plugin and switch the gateway**

In `gears/qa-platform/config/qa-platform-stack.yaml`: replace `static-authn-plugin` with `oidc-authn-plugin` configured against `http://keycloak:8080/realms/qa-platform`, set `auth_disabled: false` on `api-gateway`, and update the comment Task 3 left at that block. Build with `oidc-authn` in place of `static-authn` in `CARGO_FEATURES` (check the feature's real name in `apps/cf-gears-example-server/Cargo.toml`).

Audience validation is the field most likely to reject every token: `require_audience` and `expected_audience` must match what Keycloak actually puts in `aud` for this client — which you recorded in Step 2. If they disagree, fix the config, not the token.

- [ ] **Step 4: Verify both directions**

```bash
cd gears/qa-platform/deploy/compose && docker compose up -d --build
curl -s -o /dev/null -w '%{http_code}\n' localhost:8087/qa/v1/platforms                 # expect 401
TOKEN=$(curl -fsS -X POST http://localhost:8180/realms/qa-platform/protocol/openid-connect/token \
  -d grant_type=password -d client_id=qa-platform-ui -d username=admin -d password=admin | jq -r .access_token)
curl -s -o /dev/null -w '%{http_code}\n' -H "Authorization: Bearer $TOKEN" localhost:8087/qa/v1/platforms  # expect 200
```

Expected: **401 then 200.** Both are required — a 200 without a token means auth is not enforced, and that is the failure this task exists to prevent. Record both status codes.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/deploy/compose config/qa-platform-stack.yaml
git commit -m "feat(deploy): Keycloak in compose and real OIDC validation on the gears"
```

---

### Task 14: Real tenants from token claims

**Files:**
- Modify: `gears/qa-platform/config/qa-platform-stack.yaml`
- Possibly modify: `gears/qa-platform/deploy/compose/keycloak/realm-qa-platform.json`

**Interfaces:**
- Consumes: the tenant claim recorded in Task 13 Step 2.
- Produces: tenant existence and hierarchy answered from real Resource Group rows instead of
  asserted flatly, and a real ancestor walk for credstore's tenant directory. **Not**
  `system_grants`' cross-tenant reads becoming meaningful: no `/qa/v1` gear depends on the tenant
  resolver, and `static-authz-plugin` -- the only authz plugin compiled in -- never consults it
  (it reads the tenant from `request.context.tenant_context`,
  `static-authz-plugin/src/domain/service.rs:70-92`), so the resolved tenant this task delivers
  cannot affect `system_grants` at all. (Corrected post-implementation; see Task 14's report.)

- [ ] **Step 1: Replace the tenant resolver**

Swap `single-tenant-tr-plugin` for `rg-tr-plugin` and keep the `resource-group` gear section. Read `rg-tr-plugin`'s config struct for its real field names, and check whether it needs the tenant to exist as a resource-group row before it resolves — if it does, seeding that row is part of this task, and `smoke.sh` needs a step for it.

- [ ] **Step 2: Make Keycloak issue the claim the resolver reads**

If the resolver expects a claim Keycloak does not send, add a client mapper to the realm JSON. Note which side you changed and why.

- [ ] **Step 3: Verify the resolved tenant is not nil**

Re-run Task 13 Step 4's authorized call and confirm 200, then check the gears' log for the resolved tenant id on that request. **A nil tenant here means every ordinary request is denied by the same branch `system_grants` widens** — so confirm a real UUID, and record it.

Re-run `./smoke.sh` with a token (add an `AUTH_TOKEN` env var to the script; if unset it sends no header, so Phase A's usage still works). Expected: the whole flow still passes.

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/config/qa-platform-stack.yaml gears/qa-platform/deploy/compose
git commit -m "feat(deploy): resolve real tenants from OIDC claims"
```

---

### Task 15: The UI auth subsystem

**Files:**
- Create: `src/auth/{provider.tsx,useAuth.ts,RequireAuth.tsx,LoginPage.tsx,index.ts}`
- Modify: `src/App.tsx`, `src/api/client.ts`, `src/components/layout/Sidebar.tsx` (a logout control), `.env.example`
- Test: `src/auth/tokenState.test.ts`

**Interfaces:**
- Consumes: `setTokenProvider` from `client.ts` (Task 9), the Keycloak client from Task 13.
- Produces: `<AuthProvider>`, `useAuth(): { user, login, logout, isAuthenticated }`, `<RequireAuth>`.

- [ ] **Step 1: Write the failing test for the refresh rule**

The one piece of real logic worth a unit test is the 401 rule: **exactly one silent refresh, then redirect.** A loop here is how a revoked session becomes an infinite request storm.

```ts
import { handleUnauthorized, resetAuthAttempts } from './tokenState';

beforeEach(resetAuthAttempts);

it('permits exactly one refresh for a run of 401s', async () => {
  const refresh = vi.fn().mockResolvedValue('new-token');
  const redirect = vi.fn();
  await handleUnauthorized({ refresh, redirect });
  await handleUnauthorized({ refresh, redirect });
  await handleUnauthorized({ refresh, redirect });
  expect(refresh).toHaveBeenCalledTimes(1);
  expect(redirect).toHaveBeenCalledTimes(2);
});

it('redirects and does not retry when the refresh itself fails', async () => {
  const refresh = vi.fn().mockRejectedValue(new Error('revoked'));
  const redirect = vi.fn();
  await handleUnauthorized({ refresh, redirect });
  await handleUnauthorized({ refresh, redirect });
  expect(refresh).toHaveBeenCalledTimes(1);
  expect(redirect).toHaveBeenCalledTimes(2);
});

it('allows a refresh again once a request has succeeded', async () => {
  const refresh = vi.fn().mockResolvedValue('t');
  const redirect = vi.fn();
  await handleUnauthorized({ refresh, redirect });
  resetAuthAttempts();                     // called on any 2xx
  await handleUnauthorized({ refresh, redirect });
  expect(refresh).toHaveBeenCalledTimes(2);
});
```

- [ ] **Step 2: Run and watch it fail**

```bash
cd gears/qa-platform/qa-platform-ui && npx vitest run src/auth/tokenState.test.ts
```

Expected: FAIL — module missing.

- [ ] **Step 3: Implement `tokenState.ts`, then the provider**

`tokenState.ts` holds the attempt counter, `handleUnauthorized({refresh, redirect})` and `resetAuthAttempts()`. Then:

- `npm i oidc-client-ts` and configure a `UserManager` with authority `${VITE_OIDC_ISSUER}`, `client_id: ${VITE_OIDC_CLIENT_ID}`, `redirect_uri: window.location.origin + '/auth/callback'`, `response_type: 'code'`, and PKCE (the library's default for public clients — confirm in its docs rather than assuming).
- **Access token in memory only; refresh token in the library's session store.** Do not put the access token in `localStorage`.
- `provider.tsx` calls `setTokenProvider(() => currentAccessToken)` once on mount — the single point where `api` and `auth` meet.
- `RequireAuth` renders children when authenticated, otherwise triggers login.
- `LoginPage.tsx` is the only new page: a heading, a sign-in button, and an error state. Match the existing pages' Tailwind/shadcn idiom; read one settings page first.
- Wrap the router in `App.tsx` and add the `/auth/callback` route.
- Wire `client.ts`'s 401 path to `handleUnauthorized`, and call `resetAuthAttempts()` on any successful response.

- [ ] **Step 4: Run the tests and build**

```bash
npx vitest run && make ui-build
```

Expected: PASS, 3/3 new plus Task 11's 4.

- [ ] **Step 5: Verify the real flow in the browser**

```bash
cd gears/qa-platform/deploy/compose && docker compose up -d --build
```

At `http://localhost:8080`, confirm each of these and record the result:

1. an unauthenticated visit redirects to Keycloak
2. logging in as `admin` returns to the app and pages render data
3. a full page reload keeps you logged in
4. logout returns you to the login screen, and a reload does not restore the session
5. every nav entry still works

**This is Phase C's gate.**

- [ ] **Step 6: Commit**

```bash
git add -A gears/qa-platform/qa-platform-ui
git commit -m "feat(qa-platform-ui): OIDC login with PKCE, route guard and single-retry refresh"
```

---

### Task 16: Authorize the SSE log stream

**Files:**
- Modify: `src/hooks/useRunLogStream.ts`, and either `nginx.conf` or the gear config depending on the decision
- Create: a decision note in `gears/qa-platform/docs/CONTRACT-DIFF.md` under "SSE authorization"

**Interfaces:**
- Consumes: Task 11's hook, Task 15's token provider.
- Produces: a log stream that works with `auth_disabled: false`.

`EventSource` **cannot send headers**, so the bearer token cannot ride the stream. With auth on, Task 11's hook now gets 401.

- [ ] **Step 1: Establish what the gateway actually accepts**

Check whether the api-gateway can authenticate a request by cookie, or by a token in a query parameter, or neither. This is a fact about the gateway, not a preference — read its auth middleware. Record what you find with file:line.

- [ ] **Step 2: Decide, and write the decision down before implementing**

The two candidates, with the cost of each:

- **Query parameter** — works with any gateway that reads a token from the query. The token then appears in nginx access logs, container logs and browser history. Mitigable only by making it short-lived, which needs a token-exchange endpoint that does not exist here.
- **Cookie set at login** — keeps the token out of URLs and is what browsers do natively; needs the gateway to accept cookie auth, plus `SameSite`/`Secure` decisions and a CSRF story for any non-GET that would then also be cookie-authenticated.

Write the choice, the rejected alternative and the reason into `CONTRACT-DIFF.md`. **If the gateway supports neither**, stop and report BLOCKED: the remaining options are a gateway change (out of scope by Global Constraints) or shipping the log viewer unauthenticated, and that is a human's decision.

- [ ] **Step 3: Implement it**

Whichever was chosen, the hook's public shape does not change — `LogViewer` must not need editing.

- [ ] **Step 4: Verify end to end, and verify it is actually enforced**

With the stack up and logged in, open a run's log viewer and confirm lines stream in. Then confirm an **unauthenticated** `curl` of the same URL is refused:

```bash
curl -s -o /dev/null -w '%{http_code}\n' "localhost:8087/qa/v1/runs/<id>/logs"
```

Expected: 401. A streaming viewer and an open endpoint is the failure mode here, so both checks are required.

- [ ] **Step 5: Commit**

```bash
git add -A gears/qa-platform gears/qa-platform/docs/CONTRACT-DIFF.md
git commit -m "feat(qa-platform-ui): authorize the SSE log stream"
```

---

# Phase D — the Helm chart

The chart deploys a **staging/demo** system: runs are simulated, because `qa-runs/src/infra/executor/` holds only `mock.rs` and it is hard-wired at `gear.rs:283`. Every task here must keep that honest.

### Task 17: Chart skeleton, gears and Postgres

**Files:**
- Create: `gears/qa-platform/deploy/charts/qa-platform/{Chart.yaml,values.yaml,.helmignore}`, `templates/{_helpers.tpl,gears-deployment.yaml,gears-service.yaml,gears-configmap.yaml,postgres-statefulset.yaml,postgres-service.yaml,postgres-secret.yaml}`
- Reference: `../vhp-testrunner/charts/vhp-testrunner/templates/`

**Interfaces:**
- Produces: `helm template` rendering a Deployment, a Service on 8087, and a Postgres StatefulSet. Task 18 adds the UI and the ingress.

Mirror `charts/vhp-testrunner` **template for template** so the two are diffable by anyone who knows the legacy deployment. Read its `_helpers.tpl`, `manager-deployment.yaml` and `postgres-statefulset.yaml` first and follow their naming, labelling and value conventions rather than inventing a style.

- [ ] **Step 1: `Chart.yaml` and `values.yaml`**

No Argo subchart — there is no executor to drive it. `values.yaml` in legacy's naming style:

```yaml
global:
  domain: ""

nameOverride: ""
fullnameOverride: ""
imagePullSecrets: []
commonLabels: {}

gears:
  enabled: true
  # Replicas stay 1 deliberately. The qa-insights JIRA-poller ticker runs under
  # the default NoopLeaderElector, which makes EVERY replica a leader, and its
  # auto-rerun is bounded only by a local write. Raising this needs the claim
  # row that release-gate item names — it is not a scaling knob today.
  replicaCount: 1
  logLevel: info
  image:
    repository: cf-gears-qa-platform
    tag: latest
    pullPolicy: IfNotPresent
  service:
    port: 8087
  resources: {}

oidc:
  issuer: ""            # required, e.g. https://idp.example.com/realms/qa
  clientId: "qa-platform-ui"

postgres:
  enabled: true
  image:
    repository: postgres
    tag: "16"
  auth:
    username: qa
    password: ""        # required unless existingSecret is set
    existingSecret: ""
  persistence:
    size: 10Gi
```

- [ ] **Step 2: The gears Deployment and its config**

The stack config becomes a ConfigMap mounted at `/etc/cf-gears/qa-platform-stack.yaml`, templated so `oidc.issuer`, `oidc.clientId` and the Postgres host come from values. Postgres credentials come from the Secret as env vars — the config file references them as `${POSTGRES_USER}`/`${POSTGRES_PASSWORD}`, the same indirection Task 3 established, so **confirm the gears actually expand env vars inside the config file**; if they do not, template the literal values into the ConfigMap instead and note that the password is then in a ConfigMap, which is a reason to prefer the Secret path.

Add a readiness probe on something that proves the gears are serving — `/openapi.json` is honest and needs no auth. Do not probe an authenticated route.

- [ ] **Step 3: Render and lint**

```bash
helm lint gears/qa-platform/deploy/charts/qa-platform
helm template qa gears/qa-platform/deploy/charts/qa-platform \
  --set oidc.issuer=https://idp.example.com/realms/qa \
  --set postgres.auth.password=test | head -80
```

Expected: lint clean; the render shows the Deployment, Service, StatefulSet and Secret. Also confirm that omitting the two required values **fails** with a message naming them (legacy uses `required` in its templates for exactly this) — a chart that renders with an empty issuer deploys a stack that rejects every request.

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/deploy/charts/qa-platform
git commit -m "feat(deploy): Helm chart — gears deployment, service and Postgres"
```

---

### Task 18: The UI, the ingress, and deploying it for real

**Files:**
- Create: `templates/{ui-deployment.yaml,ui-service.yaml,ui-configmap.yaml,ui-ingress.yaml,ui-ingress-auth-secret.yaml,NOTES.txt}`
- Modify: `gears/qa-platform/deploy/charts/qa-platform/values.yaml`

**Interfaces:**
- Consumes: Task 17's chart, the UI image (Task 12).
- Produces: a deployable chart; `smoke.sh` passes through the ingress.

- [ ] **Step 1: UI templates, following legacy's**

`ui-configmap.yaml` carries `nginx.conf`, templated so the backend is the gears Service name rather than the compose service name. Keep `proxy_buffering off` on the log-stream location — losing it in the k8s port is exactly the kind of silent regression this template's comment should warn about.

`ui-ingress.yaml` and `ui-ingress-auth-secret.yaml` follow legacy's optional-htpasswd pattern (`nginx.ingress.kubernetes.io/auth-type`, `auth-secret`, `auth-realm`, and `htpasswd` in the Secret). **Keep it** even though OIDC is now enforced: it is defence in depth in front of a staging system, and preserving it keeps the chart diffable against legacy's.

Add to `values.yaml`:

```yaml
ui:
  enabled: true
  replicaCount: 1
  image:
    repository: cf-gears-qa-platform-ui
    tag: latest
    pullPolicy: IfNotPresent
  ingress:
    enabled: false
    className: nginx
    host: ""
    annotations: {}
    auth:
      enabled: false
      type: basic
      realm: "Authentication Required"
      username: ""
      password: ""
      existingSecret: ""
```

- [ ] **Step 2: `NOTES.txt` — say plainly that runs are simulated**

```
qa-platform is deployed.

  UI:    http{{ if .Values.ui.ingress.tls }}s{{ end }}://{{ .Values.ui.ingress.host }}
  API:   {{ include "qa-platform.gearsName" . }}:{{ .Values.gears.service.port }}

WARNING — THIS IS A STAGING/DEMO DEPLOYMENT. TEST RUNS ARE SIMULATED.

  qa-runs ships one RunExecutor adapter, MockRunExecutor, and it is compiled in
  unconditionally (qa-runs/src/infra/executor/mock.rs, bound at gear.rs:283).
  Runs are launched, queued, dispatched, ingested, analysed and reported for
  real — but nothing executes a test. The real adapter waits on the
  serverless-runtime gear.

  Do not point this at a production ReportPortal or JIRA project expecting
  real results.

Replicas are pinned to 1. The qa-insights JIRA-poller ticker runs under a
no-op leader elector, so every replica would act as leader.
```

- [ ] **Step 3: Deploy to kind and run the gate**

```bash
kind create cluster --name qa-platform
# build both images and load them
docker build -t cf-gears-qa-platform:dev -f gears/qa-platform/deploy/docker/qa-platform.Dockerfile .
docker build -t cf-gears-qa-platform-ui:dev -f gears/qa-platform/deploy/docker/qa-platform-ui.Dockerfile gears/qa-platform/qa-platform-ui
kind load docker-image cf-gears-qa-platform:dev cf-gears-qa-platform-ui:dev --name qa-platform
helm install qa gears/qa-platform/deploy/charts/qa-platform \
  --set gears.image.tag=dev --set ui.image.tag=dev \
  --set oidc.issuer=<keycloak-reachable-from-cluster> \
  --set postgres.auth.password=test
kubectl rollout status deploy/qa-qa-platform-gears --timeout=300s
```

Then port-forward the UI Service and run `./gears/qa-platform/deploy/compose/smoke.sh http://localhost:<port>/qa/v1` — **the same script Phase A used, unchanged.** That reuse is the point: the gate is identical, so passing it means the chart deploys the same working system compose does.

**This is Phase D's gate.** If Keycloak is awkward to reach from inside kind, deploy with `auth_disabled: true` for this step only and say so explicitly in your report — do not silently weaken the chart's default.

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/deploy/charts/qa-platform
git commit -m "feat(deploy): Helm chart — UI, ingress and staging warning"
```

---

# Phase E — verification that survives

### Task 19: The contract check in CI

**Files:**
- Modify: the repo's CI workflow directory, `Makefile`

**Interfaces:**
- Consumes: `make ui-contract` (Task 9), `make openapi`'s machinery.
- Produces: CI failing when the UI's committed types no longer match the gears' OpenAPI.

- [ ] **Step 1: Find the CI convention before adding to it**

Read the existing workflows. Add a job in their style — do not introduce a second convention for caching, checkout or job naming.

- [ ] **Step 2: Make the contract check self-contained**

`make ui-contract` as written needs a **running** stack. In CI, generate the spec from a locally started server the way `make openapi` already does (it builds the binary, starts it with the `start_server_and_wait` macro, curls `$(OPENAPI_URL)` to `$(OPENAPI_OUT)`, then canonicalises with `tools/scripts/sort_openapi_json.py`).

Add a variant that uses `gears/qa-platform/config/qa-platform-stack.yaml` and feeds the result to `openapi-typescript` from a **file** rather than a URL, so the check needs no Postgres. If the qa-platform stack cannot boot without a database, use the SQLite `config/qa-platform.yaml` for spec generation only — the OpenAPI document does not depend on the database engine. State which you chose and why.

- [ ] **Step 3: The CI job**

Three steps: `make ui-lint`, `make ui-build`, `make ui-contract`. Node version pinned to what `package.json` expects (add an `engines` field if absent).

- [ ] **Step 4: Prove the check fails**

Push a branch that renames one gear route, and confirm the job **fails** with a message pointing at the stale generated file. Revert. Paste both outcomes into your report.

**This is Phase E's gate, and it is a deliberate break** — a verification step that has never failed has not been shown to work.

- [ ] **Step 5: Commit**

```bash
git add .github Makefile
git commit -m "ci(qa-platform-ui): fail the build when UI wire types drift from the gears"
```

---

### Task 20: The documentation someone actually needs

**Files:**
- Create: `gears/qa-platform/deploy/README.md`
- Modify: `gears/qa-platform/qa-platform-ui/README.md`

**Interfaces:**
- Consumes: everything.
- Produces: a reader who has never seen this repo can bring the stack up.

- [ ] **Step 1: Write `gears/qa-platform/deploy/README.md`**

Cover, in this order, and with commands that were actually run rather than recalled:

1. **Bring it up:** the two `docker compose` commands, the URLs (UI 8080, API 8087, Keycloak 8180), and the two test users.
2. **Verify it:** `./smoke.sh`, and what each assertion proves.
3. **The staging warning:** runs are simulated, with the same citation `NOTES.txt` carries.
4. **Deploy the chart:** the `helm install` line with its two required values.
5. **`system_grants`:** what it is, why it is default-off, why read-only, and the one write grant. Point at `SystemGrant`'s doc comment rather than restating it.
6. **Troubleshooting, from what you actually hit:** every gear's dashboard empty means the grants are not reaching the PDP; a 401 on every request means audience validation disagrees with the token; log lines arriving in batches means `proxy_buffering` is on.

- [ ] **Step 2: Update the UI README**

Replace legacy's content with: where the API contract comes from (generated, `make ui-contract`), that `src/api/` is the only adapted layer and components are a verbatim copy, a pointer to `REMOVED-SURFACES.md`, and how to run against a local stack (`VITE_API_URL`).

- [ ] **Step 3: Have someone follow it, or follow it yourself from a clean tree**

```bash
git stash -u && cd gears/qa-platform/deploy/compose && docker compose down -v
# now follow gears/qa-platform/deploy/README.md literally, changing nothing
```

Every command must work as written. A README that needs a step you know but did not write down is the defect this step exists to catch.

- [ ] **Step 4: Commit**

```bash
git add gears/qa-platform/deploy/README.md gears/qa-platform/qa-platform-ui/README.md
git commit -m "docs(deploy): how to bring up, verify and deploy the qa-platform stack"
```

---

## Spec coverage

| Spec section | Tasks |
|---|---|
| §1 goal, components-verbatim principle | 7, 8, 10 (Step 4's hook-name diff is the check) |
| §3.1 background tasks off | 1, 2, 3 |
| §3.2 no real executor | 18 (`NOTES.txt`), 17 (`values.yaml` has no executor knob) |
| §3.3 tenant-scoped authz only | 14; per-user roles out of scope by §11 |
| §4 `system_grants` incl. all 7 tests | 1, 2 |
| §5 repository layout | 3, 4, 7, 12, 17, 18 |
| §6.1 `client.ts` | 9 |
| §6.2 generated types | 9 |
| §6.3 path-level gaps, field-level diff | 6 (diff), 10 (implementation) |
| §6.4 WebSocket → SSE, `proxy_buffering off` | 11, 12 |
| §6.5 the three removals + re-add table | 8 |
| §7 auth subsystem | 13, 14, 15 |
| §8.1 compose | 4, 12, 13 |
| §8.2 Helm chart | 17, 18 |
| §9 phasing and gates | Phase gates at 5, 12, 15, 18, 19 |
| §10 verification, three layers | 5, 12, 15, 18 (gates); 19 (contract); 10, 11, 15 (the three unit-tested units: adapters, SSE hook, auth state) |
| §12 risks | field diff → 6; first OIDC config → 13; SSE auth → 16; grant safety → 1, 2; frontend-in-Rust-repo → 7, 19 |

## Open items carried into execution

> **Status as of 2026-08-26:** items 1, 2 and 3 are CLOSED with evidence — see the two sections near
> the end of this file. The `TestResultDto` OpenAPI collision that session 1 recorded as a blocker is
> resolved by **Task 3b**. What remains open is Task 17's ConfigMap decision, now informed rather than
> speculative.

1. **Task 1 Step 0 is a real gate.** Every grant in this plan matches `resource.resource_type` against `qa.*` names. The SDK documents that field with a GTS-style example. If the PEP sends GTS identifiers, every grant in Task 3 is wrong and Phase A stops until the names are corrected.
2. **Per-gear database creation (Task 4 Step 2)** is unresolved: whether `toolkit-db`'s Postgres path creates a missing `dbname` was not checked while planning.
3. **Config env-var expansion (Task 17 Step 2)** is unresolved: whether the gears expand `${POSTGRES_PASSWORD}` inside a config file. If not, the chart puts credentials in a ConfigMap and that needs a different answer.
4. **SSE authorization (Task 16)** may be BLOCKED by the gateway supporting neither cookie nor query-parameter auth. That is a human decision, not a workaround.
5. **The five qa-insights release-gate items are still open**, and Task 3 turns the tickers on — which activates the JIRA-poller duplicate-launch race under `NoopLeaderElector`. Single-replica compose and a `replicaCount: 1` chart are safe; raising either is not. Both places say so.

## Execution handoff

Tasks are ordered by dependency and each ends at a runnable gate. Phase A must be green before Phase B starts — a UI adapted against a backend that serves no data cannot be verified, only compiled.

---

# Execution status — updated 2026-08-26, end of session 2

Executed with superpowers:subagent-driven-development on branch `feature/qa-platform-specs`
(no worktree — implementation commits land alongside the docs, as chosen at dispatch).
Ledger with the full audit trail, every ruling and every deferred minor:
`.superpowers/sdd/2026-08-25-qa-platform-ui-integration/progress.md`.

## PHASE A IS COMPLETE. The gate passes.

`gears/qa-platform/deploy/compose/smoke.sh` drives the real API from repository registration through
analytics and **exits 0 in ~13 seconds on a fresh volume**. That is the plan's own Phase A gate, and it
is green. Resume at **Task 6**.

Anyone can now bring the backend up:

```bash
cd gears/qa-platform/deploy/compose && docker compose up -d
curl -fsS localhost:8087/openapi.json | head -c 200
./smoke.sh
```

## Where execution stands

| Task | State | Commits |
|---|---|---|
| 1 — `SystemGrant` config type | **complete**, review clean, 0 findings | `5bf1dab9` |
| 2 — grant decision path | **complete**, review clean after 1 fix round | `a424f259`, `146ddbab` |
| 2b — *added during execution* | **complete**, review clean | `54e13ce0` |
| 3 — the stack config | **complete** | `ea243ccd` |
| 3a — *added during execution* | **complete**, review clean | `7c30b410` |
| 3b — *added during execution* | **complete**, review clean | `68b67b5b` |
| 4 — images and compose stack | **complete**, review clean after 1 fix round | `1261eeeb` |
| 5 — the smoke script, Phase A's gate | **complete**, review **Approved** after 1 fix round | `b3edae65` |
| 5a — *added during execution* | **complete**, review clean after 1 fix round | `2268cd95` |
| 7 — import the UI | **complete**, review clean, byte-identical copy | `b642488b` |
| **6, 8-20** | **not started** | — |

Task 7 ran ahead of 4-6 in session 1 because it is the one Phase B task with no dependency on a running
backend. Task 6 was deliberately not pulled forward with it: its contract diff needs a live
`/openapi.json`, which now exists.

## Start here: Task 6

Task 6's requirements have been **amended** with four field-level facts Task 5 established by driving
the API for real — read them before starting, they are in that task's section and they change its work.
The stack is up and `/openapi.json` is reachable, so Step 1 needs no setup.

## What the four added tasks were, and why they are in the plan now

The plan did not anticipate any of these. Each has its own section above, with its steps ticked and its
commit recorded:

- **Task 2b** — Task 2's grant permit carried an **empty** constraint set, which the PEP compiles to
  `ConstraintsRequiredButAbsent` and fails closed. Every background task logged
  `constraints required but PDP returned none` on every tick while init looked clean.
- **Task 3a** — qa-insights' init hard-failed with no event-broker client registered, which no
  qa-platform deployment provides.
- **Task 3b** — `TestResultDto` was defined in both qa-runs and qa-insights with no schema alias, so
  `register_rest` panicked whenever both gears loaded. Pre-existing; Task 3a's fix merely stopped an
  earlier failure from masking it.
- **Task 5a** — `qa-insights`' `reproject()` held a transaction across a cross-gear read, tripping
  `toolkit-db`'s task-local guard, so the reconciler ticker **and** the rebuild endpoint had been
  silently doing nothing since boot. **An explicit human-authorized exception to the feature freeze.**

Three of the four were live defects that every existing test suite passed over. Two shared one cause
worth carrying into every later task: **a test that asserts on the shape of a success does not notice
that the success is empty.** Task 2's tests checked `resp.decision` and the absence of a tenant clamp,
never that the permit carried a usable constraint. Task 5a's tests used a well-built in-memory double
that could not trip the guard the real client trips.

## Image build facts worth not rediscovering

- The builder stage needs **`cmake`** and **`golang-go`**, and neither is optional. `cf-gears-oagw` is a
  non-optional dependency of `cf-gears-example-server` and pulls `pingora-core`, which pins
  `flate2/zlib-ng` unconditionally → `libz-ng-sys` shells out to `cmake`. And `libs/rustls-fips-shim` is
  a **workspace member** whose `Cargo.toml` requests `rustls/fips` unconditionally on Linux, so Cargo's
  feature unification turns on `aws-lc-rs`'s `fips` for the whole workspace regardless of
  `$CARGO_FEATURES` → `aws-lc-fips-sys` needs Go. **Every container image built from this workspace pays
  for a FIPS AWS-LC build whether it wants FIPS or not.** Worth a human decision; out of scope here.
- The runtime stage needs **`ca-certificates`**. It shipped with **no cert store at all**, and
  qa-catalog's `gix`-over-`reqwest`/`rustls` client fails to *build* on zero roots — before it inspects
  the URL scheme. So every git sync was broken, `http` and `https` alike. Do not "simplify" this away as
  HTTPS-only.
- **`cargo tree -p <pkg>` hides workspace-wide feature unification.** The real `cargo build` has no
  `-p`. An audit scoped with `-p` gave a confident, wrong answer and cost a build round; use
  `cargo metadata` unscoped.
- A subagent's foreground `Bash` call caps at 10 minutes, which a release build of this workspace can
  exceed. Three implementers were lost backgrounding a build and then waiting for a notification a
  detached process never sends. **The controller should own long builds** and hand the image back.


## Two of the plan's five open items are now answered, with evidence

**Open item 2 — does `toolkit-db`'s Postgres path create a missing database? NO.**
`libs/toolkit-db/src/options.rs:632` only does `opts = opts.database(dbname)`; there is no
`CREATE DATABASE`, `create_database` or `database_exists` anywhere under `libs/`. A missing per-gear
database is a connection error. **Task 4 must provision them** — `POSTGRES_DB` creates only one, so
the compose Postgres needs an init script covering all eight: `qa_environments`, `qa_catalog`,
`qa_runs`, `qa_insights`, `settings`, `credstore`, `resource_group`, `event_broker`.

**Open item 3 — do the gears expand `${POSTGRES_PASSWORD}` inside a config file? NO.** Answered 14
tasks earlier than the plan scheduled it, because Task 3 Step 4 exercises it immediately. Expansion is
**opt-in per config struct**: a struct must `#[derive(ExpandVars)]`, mark fields `#[expand_vars]`, and
its gear must load via `ctx.config_expanded()`/`config_expanded_or_default()`
(`libs/toolkit/src/context.rs:254-295`). `GlobalDatabaseConfig` and `DbConnConfig`
(`libs/toolkit-db/src/config.rs:47` and `:65`) derive only `Debug, Clone, Deserialize, Serialize`, and
nothing in toolkit-db calls `expand_vars()`. `config/quickstart-windows.yaml:16` claims otherwise, but
every `${}` in every committed config is inside a **comment** — the path has never been exercised, and
no committed config in this repo uses Postgres at all.

**So Task 3's `database:` block uses literal `localhost`/`qa`/`qa`** (verified working — all eight
databases connected), and **Task 17's ConfigMap question is still open but no longer a guess**: `${}`
will not expand there either, so the chart must render the value into the ConfigMap or move to a
Secret plus an entrypoint that templates the file.

Open items 1 (resource naming) and 5 (the qa-insights release-gate items) are addressed below and
above respectively. Open item 4 (SSE authorization) is untouched — still Phase C's decision.

## Open item 1 is CLOSED — the plan's `qa.*` grant names are correct

Task 1 Step 0's gate passed. `resource.resource_type` carries the plain `qa.*` name verbatim:
`authz-resolver-sdk/src/pep/enforcer.rs:325` is `resource_type: resource.name.clone().into_owned()`,
with no GTS transformation, and `qa-insights/src/domain/service/mod.rs:193` feeds the literal
`"qa.test_result"`. Confirmed independently by the task reviewer at the same file:line. **Every grant
in this plan stands as written** — Task 3's four grants loaded cleanly and `qa-catalog`/`qa-runs`
initialised with their background tasks on and **no denial WARN**, so Tasks 1-2 are proven working end
to end against a real stack.

## Corrections to the plan text itself, found in the pre-flight scan

Rulings made before Task 1 was dispatched. Each stands unless a human overrules it.

- **`src/components/` exclusivity is scoped to rewrites, not to files.** Spec §6.5:339 says it is "the
  only place `src/components/` and `src/pages/` are modified at all", but §6.4:345 mandates the
  `LogViewer.tsx` change and §7:369 mandates a logout control. Reading §6.5 absolutely would make two
  spec-mandated tasks unimplementable. So: Task 8 owns §6.5's removals, **Task 11 may change only
  `LogViewer.tsx`'s import and hook name**, **Task 15 may add only an additive logout control to
  `Sidebar.tsx`**, and any other component/page edit remains a defect.
- **Task 4's "Task 8 adds `ui`" means Task 12.** Task 8 never touches `docker-compose.yml`.
- **Task 5's "Task 17 reuses it against the chart's ingress" means Task 18.** Task 17 creates no
  ingress; Task 18 creates `ui-ingress.yaml` and claims the smoke pass.
- **Every path in Task 18's `templates/{…}` list is under `gears/qa-platform/deploy/charts/qa-platform/templates/`.**
- **Task 15's `.env.example` is `gears/qa-platform/qa-platform-ui/.env.example`**, not Task 4's
  `gears/qa-platform/deploy/compose/.env.example` — its values are `VITE_*`, which Vite reads at UI build time.

## Findings for a human, not fixable inside this plan

Three things Phase A established that are **out of this plan's scope** and need someone's decision.
None blocks Task 6.

1. **The whole workspace builds a FIPS AWS-LC.** `libs/rustls-fips-shim` is a workspace member whose
   `Cargo.toml` requests `rustls/fips` unconditionally on Linux, and Cargo unifies features across
   members, so `aws-lc-rs`'s `fips` is on for every build regardless of `$CARGO_FEATURES` — and
   regardless of `toolkit-http`'s own `fips` feature, which nominally gates the shim as `optional = true`
   but never gets consulted. Reachability into the binary:
   `cf-gears-example-server → cf-gears-credstore → aws-lc-rs → aws-lc-fips-sys`. Every image from this
   workspace pays that build cost, and needs Go and cmake to do it. Deliberate, or accidental?

2. **The event consumer carries Task 5a's defect, latently.** `qa-insights`'
   `infra::events::consumer` still composes the unsplit `reproject_run` inside its own transaction, so
   the same task-local guard trip awaits it **if event ingest is ever wired into a single-binary
   deployment**. Documented at both ends in code. Fixing it means redesigning that consumer's
   offset+projection atomicity — a real piece of work, not a follow-up line.

3. **`analytics/overview`'s `passed`/`failed` are permanently 0** in any deployment this plan produces,
   because app_version is never set; version observation is a separate feature. Phase A's gate therefore
   asserts on `case_expected` instead, with the substitution documented in the script. **A UI panel bound
   to those two fields will render zeros on a fully working stack** — the "plausible-looking zero" the
   Global Constraints call worse than a missing panel. Task 6 must decide: show, hide, or label
   unavailable.

Also unchanged from session 1 and still true: **no deployment in this plan has event ingest**, so the
consumer is covered only by `MockBroker` tests, and a fresh database needs
`POST /qa/v1/insights/rebuild` before analytics show anything. Phase A's gate calls it explicitly for
exactly that reason — `reconcile_interval_seconds` is 300s, so without the call a fresh stack looks
broken for five minutes.

## Deferred minor findings, for the final whole-branch review to triage

- Task 2: `domain/mod.rs:4` declares `pub mod grants` where `pub(crate) mod grants` would be tighter.
  Not fixed deliberately — it matches that file's existing pattern (`service` is `pub mod`, `client`
  is private).
- Task 3a: `tests/ingest_idempotence.rs` claims `grep -c 'unimplemented!('` answers 60; it answers 61,
  and did before that change. Pre-existing, left alone as out of scope.
- `config/qa-platform.yaml:142-144`'s "No qa-platform gear talks to the broker yet" clause is stale.
  Left alone because Task 3 designates that file reference-only.

## Toolchain notes worth not rediscovering

- Node here is **v18.19.1** / npm 9.2.0, but `react-router@7.13.0` declares `engines.node >= 20`. The
  UI builds anyway with non-fatal `EBADENGINE` warnings. The legacy `Dockerfile` already uses
  `node:20-alpine`, so Task 12 is fine, but anyone running `npm ci` with `engine-strict=true` locally
  will fail. The ~1.95MB single-chunk bundle warning is inherited from legacy, not a regression.
- The repo root for all of this is `gears-rust/`, not the `fabric/` parent — `git` commands run from
  the parent will report "not a git repository".


---

# Execution status — session 3, 2026-08-26

Session 2's status section above remains accurate for Phase A. This section records session 3.

## Where execution stands

| Task | State | Commits |
|---|---|---|
| 1, 2, 2b, 3, 3a, 3b, 4, 5, 5a, 7 | **complete** (session 1-2) | see the session-2 table |
| 4a — *added session 3*, container restart | **in progress** | — |
| 6 — the field-level contract diff | **complete**, review clean after 1 fix round | `a56b4b1d`, `1681785a` |
| 8 — remove the three settings surfaces | **not started**, scope amended by Ruling A | — |
| 8a — *added session 3*, the ten unserved surfaces | **not started** | — |
| 9-20 | **not started** | — |

## Task 6's three results, in order of how much they change the plan

**1. Spec §6.3's category 3 is wrong on all seven paths (Ruling A).** §6.3 says those seven appear "only in
`hooks.ts`, with no component consumer anywhere in the 114 files" and have "Zero UI impact". All seven have
live consumers on routed pages; the reviewer re-grepped every identifier independently and confirmed each
file:line. Task 8's Step 4 has been amended — and note that step's *own* instruction had already told its
executor to run exactly this grep and report if it fired. One of the seven,
`/plans/{id}/run-test`, is not a gap at all: `POST /qa/v1/runs` with `target.kind="test"` serves it.

**2. Ten "cannot be absorbed" items, C1-C10 (Ruling B → Task 8a).** Ten fields the UI renders that the
gears do not serve. Two deserve naming here:

- **C7 fails silently.** Product scoping of runs, schedules and custom plans returns HTTP 200 with
  *another product's rows*, because unknown query parameters are ignored rather than rejected. Nothing
  errors. It is the only item on the list that gives no symptom.
- **C10 has a confidentiality consequence.** The variables editor renders a Secured checkbox, a password
  input, `********` masking and a padlock over a field `VariableDto` does not have — so a variable a user
  marks secret round-trips and is stored in cleartext, beneath a padlock. It is also the only item where
  leaving the code untouched is the *unsafe* option rather than the conservative one.

**3. Two contract facts that would have silently misled Task 10.** `$top` — which spec §6.3 itself uses —
is **silently ignored**; paging is an undeclared `limit`+`cursor`. And `/run-queue/{id}/cancel` does *not*
map to `POST /qa/v1/runs/{id}/cancel` as §6.3 says: that takes a run id, while the queue hook holds a
queue-row id and maps to `DELETE /qa/v1/queue/{id}`. Both are in CONTRACT-DIFF §5.

## Host-networking note — read this before diagnosing an unreachable API

Mid-session the host could not reach **any** published port on the compose network: connections hung with
no response, on both 8087 and 5432, and on the container IP directly — while the gears answered
`HTTP/1.0 200 OK` immediately from inside their own network namespace. It is stale forwarding rules on the
host, not a defect in anything this plan builds.

**The fix is `docker compose down && docker compose up -d`** — plain `down`, never `-v`, which would
destroy the Postgres volume. A `docker compose restart` is not a substitute, and until Task 4a lands it
makes things worse. If the API is unreachable, do this **before** diagnosing the gears.

## Findings for a human, carried forward and added to

The three from session 2 still stand unchanged (the workspace-wide FIPS AWS-LC build; the event consumer's
latent Task-5a defect; `analytics/overview`'s permanently-zero `passed`/`failed`, which CONTRACT-DIFF §9
now decides). Session 3 adds:

4. **C7 and C10** above — the two items on the ten-item list whose failure mode is not a visible gap. C10
   in particular is a security-shaped finding in the *legacy UI's* promise, not in the gears.
5. **Spec §6.3 category 3 was wrong**, which is worth knowing beyond this plan: it is the only part of the
   spec derived from a measurement rather than from reading a contract, and it is the part that turned out
   to be wrong.


---

# SESSION 3 STATUS — superseded by the session-4 section at the end of this file

Supersedes the session-3 section above where they differ.

## Where execution stands

| Task | State | Commits |
|---|---|---|
| 1, 2, 2b, 3, 3a, 3b, 4, 5, 5a, 7 | **complete** (sessions 1-2) | see the session-2 table |
| 4a — container survives a restart | **complete**, Approved after 1 fix round | `9043485b`, `5efb8dcc` |
| 4b — Docker naming (user instruction) | **complete**, Approved, 0 fix rounds | `1dbe5c59` |
| 6 — the field-level contract diff | **complete**, clean after 1 fix round | `a56b4b1d`, `1681785a` |
| 8 — remove the three settings surfaces | **complete**, Approved, 0 findings | `a8a7b906`, `c7cbe5a5` |
| 8a — the nine unserved surfaces | **complete**, clean after 1 fix round | `2a48e667`, `6f49f39c`, `c9d672d9`, `f1e5dc94` |
| **9-20** | **not started** | — |

Phase A's gate (`./smoke.sh`) was re-run after every infrastructure change this session: **exit 0**, all 11
steps, every time.

## THE most important thing this session established

**Seven claims in this plan's and the spec's own documents about "what references X" or "what X means" were
checked, and all seven were wrong.** Every one was derived by reading a **name** rather than the behaviour
behind it:

1. **spec §6.3** — seven hooks listed as having "no component consumer anywhere in the 114 files" and "Zero
   UI impact". All seven had live consumers on routed pages. Would have broken six pages.
2. **spec §6.5** — ReportPortal described as 3 references in two detail pages. There were affordances in
   four more files, including a live dashboard deep link.
3. **A controller-written amendment** asserting three hook removals were "safe by construction". Written
   without grepping; `useReportPortalConfig` had a live consumer.
4. **`CONTRACT-DIFF` §8-C5** — claimed the two `useDeleteRun` consumers meant *stop* and *delete*. Both mean
   stop; only the local variable name said delete. This one had already passed two reviews.
5. **A verification grep** for the identifier `Secured` that missed the page saying `Secure` — leaving
   prose promising **encryption at rest** over cleartext credentials.
6. **A review finding** premised on platform `version` being hand-set in the Edit dialog. `UpdatePlatformForm`
   has no version field; the real defect there was *another* prose promise.
7. **`CONTRACT-DIFF` §8-C3** — enumerated three platform-health surfaces; a fourth (`PlatformsTable`'s
   permanently-"Unknown" Status column) was found later.

**Rule for every later task: grep before you delete, and when what you are removing is a *promise*, grep the
promise's vocabulary (encrypt, mask, secret, secure, hidden, safe) rather than the identifier.**

Four times this session a worker declined an instruction it had checked and found false, and **all four
were right**. That is the behaviour to keep.

## What Task 6 and 8a changed about the rest of the plan

- **Task 8's scope shrank.** It no longer deletes the seven §6.3 hooks. `/plans/{id}/run-test` is served
  after all (`POST /qa/v1/runs` with `target.kind="test"`); the other six became Task 8a's C1, C3, C6.
- **The "cannot be absorbed" list is NINE, not ten.** C5 was reclassified to absorbable and now lives in
  `CONTRACT-DIFF` §7.12.
- **Task 10 has two new acceptance conditions** (Step 1a), neither visible from the contract table.
- **The UI bundle shrank 1,940 → 1,249 kB.** About a third of the legacy app was surfaces the gears cannot
  serve.
- **Two contract facts that would have silently misled Task 10:** `$top` is ignored (paging is an
  undeclared `limit`+`cursor`), and `/run-queue/{id}/cancel` maps to `DELETE /qa/v1/queue/{id}`, not to
  `POST /qa/v1/runs/{id}/cancel` as §6.3 says. Both in `CONTRACT-DIFF` §5.

## Findings for a human — carried forward and added to

Sessions 1-2's three still stand (workspace-wide FIPS AWS-LC build; the event consumer's latent Task-5a
defect; `analytics/overview`'s permanently-zero `passed`/`failed`, now decided by `CONTRACT-DIFF` §9).
Session 3 adds:

4. **C7 fails silently.** Unknown query parameters return **200 with the whole tenant's rows** rather than a
   400, so product scoping showed another product's data with no error. Resolved by removing the controls;
   the underlying gear behaviour is unchanged and will bite anything else that filters this way.
5. **C10 was a confidentiality misrepresentation.** The variables editor promised encryption at rest and UI
   masking — in an icon, an input type, and in prose on the routed page — over values the gears store and
   return in cleartext. Removed. **No secret should ever have been typed into that field.**
6. **An eighth same-class defect is known and unfixed:** `notificationsShared.tsx:156` documents a
   `{{product_key}}` substitution token against a DTO field that is always null. Deferred deliberately;
   worth a dedicated sweep for "copy that promises a field with nothing behind it" rather than another
   one-off fix.

---

# FINAL STATUS — end of session 4, 2026-08-27

Supersedes every earlier status section where they differ.

## Where execution stands

| Task | State | Commits |
|---|---|---|
| 1–8a | **complete** (sessions 1–3) | see the session-2 and session-3 tables |
| 9 — generated types and the `/qa/v1` client | **complete**, clean after 1 fix round | `de9aac5b`..`ef9dea3d` |
| 10 — remap `hooks.ts` onto the gear routes | **complete**, clean after 1 fix round | `ef9dea3d`..`387251b4` |
| 11 — the SSE log stream | **complete**, clean after 2 fix rounds | `387251b4`..`8aa3bc91` |
| 12 — the UI in compose (**Phase B's gate**) | **complete**, clean after 2 fix rounds | `8aa3bc91`..`db2c4344` |
| 13 — Keycloak and real OIDC | **complete**, Approved after 1 fix round | `db2c4344`..`0343901b` |
| 14 — real tenants from claims | **complete**, Approved after 1 fix round | `0343901b`..`14bb9045` |
| 15 — the UI auth subsystem (**Phase C's gate**) | **complete**, clean after 1 fix round | `31ca5d26`..`0a2725c9` |
| 16 — authorize the SSE log stream (**closes Phase C**) | **complete**, clean after 1 fix round | `18641940`..`028c06f3` |
| **17–20** | **not started** | — |

## >>> RESUME HERE <<<  (end of session 5)

**PHASE C IS COMPLETE. Tasks 14, 15 and 16 are all done and reviewed clean.** A human can log into this
product, every route renders authenticated, and the SSE log stream is authorized.

**The human stopped the session here deliberately**, after Task 16, to report bugs they found that they want
fixed before Tasks 17-20 continue. **Task 17 was not dispatched.** Start by asking them for those bugs.

Read `.superpowers/sdd/2026-08-25-qa-platform-ui-integration/progress.md` first — its session-5 section
carries eleven rulings, and the two queued items below are recorded there in full.

### Two things queued that must not be lost

1. **`ui-gate.js` currently fails, and it is not a code defect — fix its target selection before Task 18.**
   The gate picks the newest product by `created_at` (`ui-gate.js:624-625`). `smoke.sh` mints a
   product + repo + synced plan on every run, so "newest" was always a valid fixture — until a human created
   a product named `VHP` through the UI at `07:58:24Z`, 62 seconds after the last fixture synced and 13m40s
   after the last passing gate run. `VHP` has no synced repo, so `plans=0` and `/plans/:id` finds no link.
   **The stale fixtures are not the problem** — 14 leftover smoke products are harmless, and cleaning them
   up would restore the gate while leaving the fragility intact. The problem is that a person using the
   product breaks this plan's main verification instrument. Give the gate the fixture's product id, or make
   it select the newest product that actually has a plan. Product choice is orthogonal to the gate's three
   load-bearing properties (the negative control uses a nonexistent uuid, the error detector is structural,
   the 401 assertion is unauthenticated) — but verify that rather than assuming it.

2. **Pre-flight fact files written and not yet consumed:** `task-17-preflight-facts.md` and
   `task-18-preflight-facts.md`. Tasks 19-20 have none.

### The ruling most worth a human's second look

Task 14's. Its implementer challenged the brief's premise and **was right**: nothing in the `/qa/v1` path
consults the tenant resolver, and both resolvers derive the request tenant from the identical
`ctx.subject_tenant_id()`, so the swap **cannot** affect `system_grants` — which is what Task 14's own
Interfaces line claimed. I ruled the swap **stays** and corrected the false text instead. Finding 11 below
records exactly what undoing it costs.

**Tasks 16 and 17 each have a plan defect the pre-flight found; do not dispatch either without reading its
fact file.** Task 16's Step 2 poses a false dichotomy that would send a worker to BLOCKED — nginx already
has a location scoped to exactly the SSE route, and Step 4's verification command curls past nginx and so
cannot fail. Task 17's file list has **nowhere to solve the boot circle**: a chart built exactly to its ten
templates renders, passes `helm lint`, satisfies its own Step 3 in full, and crash-loops on a real cluster.
I have ruled Task 17's scope extended to two init-containers — they need two *different* images, because
the gears image has no `psql`.

## The two gates that now exist, and what they prove

**Phase B's gate passed for real.** `deploy/compose/ui-gate.js` drives headless Chrome (Chrome 148 via
`puppeteer-core`; playwright refuses this host's node 18) over **26 of 26 declared route patterns** — 0
uncaught page errors, 0 failed requests, 23 render with data, 3 render empty (all three explained). Detail
routes are reached by **clicking real list rows**, so X1 (name→uuid) and X6 (plan identity) are proven through
real links rather than composed URLs. After four tasks verified only by `tsc` and unit tests over pure
functions, the data layer needed **no** `hooks.ts` or `adapters.ts` fix on first contact with a browser.

The gate carries three properties that must survive every later task: a **negative control** (it navigates to
a `/products/<nonexistent uuid>` to force an error state and fails if its own detector stays silent), a
**structural error-state detector** (`main p.text-destructive`, chosen over enumerating error copy after that
approach was shown to miss two pages), and an **unauthenticated-must-401 assertion**.

**Phase C's foundation is in and adversarially proven.** 401 without a token, 200 with one, and rejection of
a tampered signature, a forged `alg: none`, a forged `HS256`, a payload rewritten under the original
signature, a wrong-realm token, and — the probe a passing 200 cannot fake — a token from `admin-cli` with the
**same issuer, same user, valid signature and no `aud`**. `require_audience` is live, not decorative. The
private CA is doing real TLS verification: from a sibling container, system roots alone fail with exit 60.

**And the auth switch now has a regression test that has been seen to fail.** Both gates previously
authenticated everything, so neither could detect auth being turned *off*. The fix was proven by standing up a
**second gears container from the same image** whose config differed only by `auth_disabled: true`, and
showing the gate exit 1 with `got: HTTP 200. Authentication is not being enforced.`

## What sessions 4 confirmed about this plan's own reliability

Session 3 established that seven claims in this plan and its spec about "what references X" or "what X means"
were wrong, each derived from a **name** rather than the behaviour behind it. Session 4 pre-flighted every
task against live sources before dispatching and **found nine more**, including four that would have cost a
session each:

1. **`unwrapPage`'s test literals** were an OData `{value, count}` envelope; the gears answer
   `{items, page_info}` (`openapi.d.ts:3635-3671`, eight instantiations).
2. **`$top` is silently ignored** — the OData extractor takes `limit`, never `$top`
   (`libs/toolkit/src/api/odata.rs:12-22`). The plan's own test asserted `$top=5` and would have gone green
   over an ignored bound. Two knock-ons for a human: **`smoke.sh` step 8 does not assert what it says**, and
   `qa-insights/.../collections.rs:16`'s claim that `$top=0` is "rejected" is false.
3. **Task 10's Step 1.2 collapsed two hooks into one route.** A queue entry cancels via
   `DELETE /qa/v1/queue/{id}`; a run via `POST /qa/v1/runs/{id}/cancel`. Both routes exist.
4. **The SSE payload is JSON, not text** (`sse_json::<RunLogLineDto>`, `{line: String}`), and **there is no
   terminal event** — a finished run gets an *immediately-empty* stream, which an `EventSource` treats as
   something to reconnect to, forever. That was the task's real hazard and the plan described a different one.
5. **The `oidc-authn` feature did not exist**, nor did the dependency: `cf-gears-oidc-authn-plugin` was wired
   into nothing. Task 13 had to create the feature, the dep and the registration.
6. **The OIDC plugin refuses a non-https issuer** under `UrlSecurityPolicy::STRICT`, so the plan's
   `http://keycloak:8080` could not work.

**Four times a pre-flight claim of mine was itself overturned by measurement**, which is the pattern to keep:
my TLS suggestion (`trusted_issuers` at the https URL) **would have 401'd every token** — `iss` is an
identifier matched as a string, the *discovery URL* is what gets scheme-validated; a single self-signed cert
cannot be both server cert and trust anchor (`CaUsedAsEndEntity`, surfacing as a 503 that looks like the IdP
being down); my "total attempts ceiling" for the SSE hook had no principled value; and I told Task 14 to copy
a gear key from a doc example that writes it with **underscores** when the real key is **hyphenated** — where
a wrong section name does not fail loudly but **falls back to defaults that happen to be right**.

**Fifteen times across this plan a worker has declined or amended an instruction it checked and found false,
and every one was right** — three of them against me. That is the behaviour to keep.

**One new failure mode, and it is about verification rather than code.** Four reports asserted a verification
that had not held, and **every one was phrased as an aggregate**: a fabricated command transcript, a
"re-checked every citation" that had not been, a "13 confirmed" citation count that was wrong, and a **cached
clippy verdict** (`Finished in 0.61s`) mistaken for a pass until `cargo clean -p` produced a real
`Checking …` line. The habit that fixes it, adopted late and visibly effective: **verify per item at the
moment you write it and report the method, never the total.** A reviewer that reads a transcript instead of
re-running it cannot catch any of these — that is now demonstrated, not hypothetical.

## Findings for a human — carried forward and added to

Sessions 1–3's six still stand. Session 4 adds:

7. **The gear has no archived-log read at all.** `useRunLogs` (`hooks.ts:722-732`) GETs the **same SSE route**
   through `apiGet`, so a finished run reads an empty body and a live run's request hangs until the stream
   ends; `RunLogBroadcaster::subscribe` carries only lines published after it was created
   (`broadcast.rs:286`). Every log-viewing surface rests on this. Nothing in this plan can fix it.

    **Session 5 update — measured, and worse than "no read".** The human saw a live viewer answering
    `200 text/event-stream` with "No logs available" and asked for a fix; I verified three independent
    causes and put the options to them. (a) **Nothing is ever produced:** `MockRunExecutor`'s production
    `default_script` (`infra/executor/mock.rs:123-157`) emits only `Started`/`TestResult`/`Finished` — the
    single `ExecutionEvent::Log` in that file sits at `:452`, inside the `#[cfg(test)]` module that opens at
    `:352`. (b) **Nothing is ever stored:** the live `qa_runs` database has five tables and none of them
    holds a log line, and no migration creates one. (c) **The mock is deliberately sleepless** (`mock.rs:9`),
    so even after adding `Log` events the lines would fire in one burst at t=0, before any viewer could
    subscribe — the small fix looks right in code and changes nothing on screen. **The human's decision was
    to defer**, so this finding stands. The real fix is persist-and-replay: a table, a migration, a write in
    `fan_out_log` and a replay in `subscribe`, which is a deliberate gear widening and therefore theirs to
    authorise.
8. **§9.3's own remedy cannot fire.** The "label unavailable" banner for empty Analytics exists
   (`AnalyticsDashboard.tsx:968`) and its copy describes exactly this deployment — but it renders only when
   `noExecutionDataCount !== null`, which needs a query that *completed*, and here the query never runs
   (`:669-671` returns null with no selected version). The fix is one condition.
9. **Three surfaces render silently empty where §9's precedent says they should say why** — §8-C1's degraded
   test columns, the Analytics page above, and `ProductCoverageCard`'s absent `collected_at` (which will
   render "Invalid Date" with a `NaN` sort the moment anything measures coverage; `CoverageBuildDto` is
   exactly `{product_key, version, build, coverage}`). **All three are blocked by the same "components
   verbatim" constraint.** This is the case for one more task authorised to add unavailable-labels.
10. **A worker produced a fabricated verification transcript** for the one claim its round was asked to prove.
    Nothing downstream is wrong, and it was caught only because a re-reviewer re-ran the commands. See the
    verification note above.
11. **Task 14's stated justification was false, and a human may want to reverse the swap.** RULED in session
    5, after the reviewer confirmed all three of the implementer's legs independently and added a fourth:
    **both** resolvers derive the request tenant from the identical `ctx.subject_tenant_id()`
    (`single-tenant-tr-plugin/src/domain/client.rs:53,68` vs `rg-tr-plugin/src/domain/client.rs:34`), so the
    tenant a `/qa/v1` request is scoped by is byte-identical either way, and nothing in that path consults
    the resolver at all. The swap therefore cannot affect `system_grants`. **I ruled it stays** — it does buy
    tenant existence/hierarchy from real Resource Group rows and a real credstore ancestor walk, the code is
    written and reviewed, and reverting the resolver while keeping the seed would leave a seeded row nothing
    reads — and corrected the false text instead (`375c232d`). **This is the one ruling most worth a human's
    second look**, because the cost is real: a boot-time data dependency and two one-shot services, for a
    benefit nothing in qa-platform currently reads. Reversing it is a one-line config swap, a Dockerfile
    feature swap, and deleting two compose services.
12. **Postgres `5432` still binds all interfaces with `qa`/`qa`.** Flagged in-file, deliberately not changed.
13. **`directAccessGrantsEnabled` is now load-bearing**, not a convenience: both gates acquire tokens by
    password grant against a public client. **Task 18 must supply `SMOKE_BEARER_TOKEN` rather than replicate
    a password grant in a real deployment's realm.**
14. **Task 17's Helm chart inherits a circular boot dependency** — oagw's `post_init` resolves the root tenant
    and aborts the boot, so the tenant row must exist before the server starts. Task 14 solved it in compose
    with two one-shot services; the chart needs the equivalent as an init-container or Job, or its pods
    crash-loop.
15. **With auth on, an idle browser tab wedges the entire gateway for twelve hours.** Observed this session,
    not predicted. A tab sitting on the UI without a valid token produced sustained `401`s
    (`/qa/v1/products` at ~2 req/s from `Chrome/148.0.0.0`), which tripped the rate limiter about four
    seconds later; `retry_after_seconds` then reached ~43000 and **every** `/qa/v1` request on the host —
    both gates included — got 429 until the stack was restarted. The obvious explanation is **wrong**: this
    is not plan finding 7's leaked SSE streams, because `in_flight: 64` was not exhausted (a socket count
    inside the container gave **9** ESTABLISHED). Two candidate defects: the UI generating that volume at
    all, and a limiter configured `rps: 1000` reaching a twelve-hour `retry_after` from a few seconds of
    load (`qa-platform-stack.yaml:111-114`), which is not a usable backpressure signal. **The mechanism
    inside the UI is not pinned** — `src/App.tsx:35` sets `retry: 1`, which does not explain the observed
    rate — so measure before designing against it. Task 15 is the right home for the client half.
