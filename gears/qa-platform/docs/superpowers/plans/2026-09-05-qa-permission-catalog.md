# QA Platform permission catalog (Phase 7) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the four QA Platform gears an `AuthzPermissionV1` catalog, so the
`qa.*` actions their PEP already enforces can be granted by GTS/RBAC.

**Architecture:** One `gts/permissions.rs` per gear, declaring a `gts_instance!`
per `(resource_type, action)` pair, generated from the *same* `resources::*` and
`actions::*` consts the enforcement path reads. An anti-drift test in each gear
pins catalog and enforced set to each other in both directions. Modelled
directly on `gears/bss/ledger/ledger/src/gts/permissions.rs` and its
`permissions_tests.rs`, which are the working precedent in this repo.

**Tech Stack:** `toolkit-gts` (`AuthzPermissionV1`, `gts_instance!`, `gts_id!`,
`inventory`), `toolkit-security` (`pep_properties`, `ResourceType`),
`authz-resolver-sdk` (`PolicyEnforcer`).

**Spec:** `gears/qa-platform/docs/superpowers/specs/2026-09-05-review-remediation-design.md` §9

**Prerequisites:**
- `2026-09-05-review-remediation-core.md` (Phases 1–4).
- **Task 25 of `2026-09-05-review-remediation-quality.md`** (#27, dropping
  `qa.plan`'s unused `RESOURCE_ID`). This plan generates from those consts; doing
  it first means the catalog is built from the corrected declaration and the
  anti-drift test never has to be edited to accommodate a fix.

## Global Constraints

- **The environments resource string is `"qa.platform"`.** The rework renamed the
  aggregate to `Environment` and deliberately kept the PDP string, because that
  string is what policies are written against and changing it would silently
  change who is authorized for what — the reasoning is at
  `qa-environments/src/domain/service/mod.rs:90-99`. **Generate from
  `resources::PLATFORM`, never from a type name.** A catalog emitting
  `qa.environment` would grant nothing, silently, and look correct.
- **Do not ship grants.** This plan ships the catalog and the drift test.
  Authoring role grants is a policy decision for whoever owns the deployment's
  realm, and the review says so explicitly.
- **Every instance id is generated from the enforcement consts**, not typed by
  hand into two places. The `EXPECTED_*_IDS` list in each test is the one
  hand-written copy, and that is deliberate: it is what makes an accidental
  catalog change fail a test rather than pass silently.
- **`resource_type` values live outside `gts.cf.resources.*`**, following
  ledger's note at `permissions.rs:11-13`, so only an explicit QA role covers
  them.
- One commit per task, on the same branch.

---

## The surface this catalog must cover

Measured at `a1767401f`. **Re-derive it in Task 30 rather than trusting this
table** — it is here so a reviewer can size the work.

| Gear | Resource types | Actions |
|---|---|---|
| qa-catalog | `qa.test_repo`, `qa.plan`, `qa.custom_plan`, `qa.product`, `qa.ssh_key`, `qa.bundle` | `get`, `list`, `create`, `update`, `delete`, `sync` |
| qa-environments | `qa.platform`, `qa.variable`, `qa.lease` | `get`, `list`, `create`, `update`, `delete`, `acquire`, `release` |
| qa-insights | `qa.test_result`, `qa.saved_view`, `qa.jira_config`, `qa.jira_bug`, `qa.notification_config`, `qa.jira` | `get`, `list`, `create`, `update`, `delete`, `collect`, `rebuild`, `test` |
| qa-runs | `qa.run`, `qa.queue_entry`, `qa.schedule` | `create`, `get`, `list`, `dispatch`, `cancel`, `rerun`, `force_start`, `update`, `delete`, `fire` |

18 resource types. **Not every (resource, action) pair is enforced** — `qa.lease`
has no `create`, `qa.plan` has no `delete`. Task 30 is what establishes the real
pairs; the catalog must contain exactly those and no cartesian product.

---

### Task 30: Establish the enforced set, per gear

**This task writes no catalog.** Its deliverable is a machine-checkable list of
the `(resource_type, action)` pairs each gear's code actually enforces, because
every later task is generated from it and a catalog built from a guess grants
the wrong things.

**Files:**
- Create: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/service/authz_surface.rs`
- Create: the same in the other three gears
- Modify: each gear's `domain/service/mod.rs` (module declaration)

**Interfaces:**
- Produces, in each gear:

```rust
/// Every `(resource_type, action)` pair this gear's PEP enforces.
pub(crate) const ENFORCED: &[(&str, &str)] = &[ /* … */ ];

/// The distinct resource types in `ENFORCED`.
pub(crate) const RESOURCE_TYPES: &[&str] = &[ /* … */ ];
```

  Tasks 31–34 generate their catalogs against `ENFORCED` and their drift tests
  compare both directions. `RESOURCE_TYPES` is consumed by Task 35 Step 3, which
  registers a stub type-schema per resource type — ledger's `labels::ALL`
  (`gears/bss/ledger/ledger/src/authz.rs:109`) exists for exactly that, and its
  doc records why: *"The platform RBAC role-definition validator resolves a
  rule's `target_type` through the types-registry (`get_type_schema`), so
  registering these at gear init lets a custom billing role target any ledger
  authz label."* Without it, the catalog names permissions no role definition
  can target.

- [ ] **Step 1: Enumerate every `access_scope` call site in one gear**

```bash
cd gears/qa-platform/qa-catalog/qa-catalog
grep -rn 'access_scope' --include=*.rs src/ | grep -v _tests
```

For each hit, record the `resources::X` and `actions::Y` it passes. Watch for:
- `elevated`/`system_actor` paths — they enforce too, and a background task that
  cannot be granted is a task that is inert by configuration. Include them.
- The `plugin_registry` calls (`plugin_for` at `:188`, `list_registered_plugins`
  at `:283`), which both use `PRODUCT` — the second deliberately, with its
  reasoning at `:277-282`. Include both; they are the same pair.

- [ ] **Step 2: Write `authz_surface.rs`**

```rust
//! The `(resource_type, action)` pairs this gear's PEP actually enforces.
//!
//! This list exists so `crate::gts::permissions` can be checked against the
//! enforcement path rather than against a reader's memory. It is the *source*
//! side of the anti-drift test: a pair enforced here and absent from the
//! catalog means a caller can be refused an action no role can grant, and a
//! catalog entry absent here means a grant that authorizes nothing.
//!
//! **Derived from the `resources::*` / `actions::*` consts, never from type
//! names.** `qa.platform` in qa-environments is the reason that rule is
//! written down: the aggregate is called `Environment` and the PDP string was
//! deliberately left as `qa.platform`, so a list built from the Rust type name
//! would name a resource type no policy mentions.
//!
//! Review finding #1.

use super::{actions, resources};

pub(crate) const ENFORCED: &[(&str, &str)] = &[
    (resources::TEST_REPO.name(), actions::GET),
    (resources::TEST_REPO.name(), actions::LIST),
    // … one line per measured call site
];
```

Check `ResourceType`'s accessor for the string — it may be `name()`, `as_str()`
or a public field. Use the real one.

- [ ] **Step 3: Write the test that keeps this honest**

The list is hand-written, so something must catch a call site added later
without a matching entry. A source-scanning test is the pragmatic option and
this repo has precedent (`qa-runs/src/lib.rs:23,32` — `doc_citations_tests`,
`file_citations_tests`):

```rust
/// **Every `access_scope` call site is represented in `ENFORCED`.**
///
/// The list is hand-written, so a new endpoint that enforces a pair nobody adds
/// here would silently be an action no role can grant. This scans the crate's
/// own source for `access_scope(` calls and checks the resource/action pair
/// each one names appears in the list.
///
/// A scan rather than a runtime capture because the pairs are decided at
/// compile time and half the call sites need a live PDP to reach.
#[test]
fn every_access_scope_call_site_appears_in_the_enforced_list() {
    let missing = scan_access_scope_sites("src/")
        .into_iter()
        .filter(|pair| !ENFORCED.contains(pair))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "these enforced pairs are missing from ENFORCED (and so from the \
         permission catalog): {missing:#?}"
    );
}
```

- [ ] **Step 4: Verify against the running gear**

```bash
cargo nextest run -p qa-catalog every_access_scope_call_site_appears_in_the_enforced_list
```

Expected: PASS once the list is complete. **Deliberately delete one entry and
confirm it FAILs** — otherwise the scanner may simply be finding nothing.

- [ ] **Step 5: Repeat for the other three gears**

qa-environments' is where the `qa.platform` trap lives. Confirm explicitly:

```bash
grep -rn 'qa\.environment' gears/qa-platform --include=*.rs
```

Expected: **zero hits.** If the catalog surface you derived contains
`qa.environment`, you built it from the type name.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform
git commit -m "feat(qa-platform): record the enforced authz surface per gear

The (resource_type, action) pairs each gear's PEP enforces, derived from the
resources::*/actions::* consts rather than from type names, with a source scan
that fails when an access_scope call site has no entry.

This is the source side of the permission catalog's anti-drift test. Building
it first means the catalog is generated from what the code enforces rather than
from what a reader remembers -- and qa-environments is why that matters: the
aggregate is Environment and the PDP string is deliberately still qa.platform,
so a surface built from the type name would name a resource type no policy
mentions. Review finding #1 (part 1 of 6)."
```

---

### Task 31: qa-catalog's permission catalog

**Files:**
- Create: `gears/qa-platform/qa-catalog/qa-catalog/src/gts/mod.rs`
- Create: `gears/qa-platform/qa-catalog/qa-catalog/src/gts/permissions.rs`
- Create: `gears/qa-platform/qa-catalog/qa-catalog/src/gts/permissions_tests.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/lib.rs` (add `pub mod gts;`)
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/Cargo.toml` (`toolkit-gts`, if absent)

**Interfaces:**
- Consumes: `authz_surface::ENFORCED` from Task 30.
- Produces: one `AuthzPermissionV1` inventory instance per enforced pair, under
  `gts.cf.toolkit.authz.permission.v1~cf.core.qa_catalog.<entity>_<action>.v1`.

- [ ] **Step 1: Write the failing test**

```rust
//! Unit tests for qa-catalog's GTS permission catalog.
//!
//! Three properties: every instance is registered in inventory, the id set
//! matches exactly, and the catalog's `(resource_type, action)` pairs equal
//! `domain::service::authz_surface::ENFORCED` in **both** directions.
//!
//! The third is the one this catalog exists for. A pair enforced with no
//! catalog entry is an action no role can grant -- the caller is refused and no
//! grant can fix it. A catalog entry with no enforcement is a grant that
//! authorizes nothing, which is worse than absent because it reads as coverage.
//! Review finding #1.

use toolkit_gts::{InventoryInstance, gts_id};

const PERMISSION_TYPE_ID: &str = gts_id!("cf.toolkit.authz.permission.v1~");
const INSTANCE_SUFFIX_PREFIX: &str = "cf.core.qa_catalog.";

/// Every qa-catalog permission instance id — one per enforced
/// `(resource_type, action)` pair.
///
/// Hand-written on purpose: it is the second copy, and the point of a second
/// copy is that changing the catalog without meaning to fails here.
const EXPECTED_PERMISSION_IDS: &[&str] = &[
    gts_id!("cf.toolkit.authz.permission.v1~cf.core.qa_catalog.test_repo_get.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.core.qa_catalog.test_repo_list.v1"),
    // … one per pair
];

fn qa_catalog_permission_instances() -> Vec<&'static InventoryInstance> {
    toolkit_gts::inventory::iter::<InventoryInstance>
        .into_iter()
        .filter(|e| {
            e.instance_id.starts_with(PERMISSION_TYPE_ID)
                && e.instance_id[PERMISSION_TYPE_ID.len()..].starts_with(INSTANCE_SUFFIX_PREFIX)
        })
        .collect()
}

#[test]
fn all_qa_catalog_permissions_are_registered_in_inventory() {
    let entries = qa_catalog_permission_instances();
    assert_eq!(
        entries.len(),
        EXPECTED_PERMISSION_IDS.len(),
        "expected {} instances, found {}: {:?}",
        EXPECTED_PERMISSION_IDS.len(),
        entries.len(),
        entries.iter().map(|e| e.instance_id).collect::<Vec<_>>()
    );
}

#[test]
fn the_inventory_covers_every_expected_id() {
    let found: std::collections::BTreeSet<_> = qa_catalog_permission_instances()
        .iter()
        .map(|e| e.instance_id)
        .collect();
    for expected in EXPECTED_PERMISSION_IDS {
        assert!(found.contains(expected), "missing catalog instance: {expected}");
    }
}

/// **The catalog and the enforced set are the same set.**
///
/// Both directions, because the two failures are different and both are silent.
#[test]
fn the_catalog_matches_the_enforced_surface() {
    let cataloged = catalog_pairs();
    let enforced: std::collections::BTreeSet<_> =
        crate::domain::service::authz_surface::ENFORCED.iter().copied().collect();

    let ungrantable: Vec<_> = enforced.difference(&cataloged).collect();
    assert!(
        ungrantable.is_empty(),
        "these pairs are enforced but not in the catalog, so no role can grant \
         them: {ungrantable:#?}"
    );

    let unenforced: Vec<_> = cataloged.difference(&enforced).collect();
    assert!(
        unenforced.is_empty(),
        "these pairs are in the catalog but enforced nowhere, so granting them \
         authorizes nothing: {unenforced:#?}"
    );
}
```

`catalog_pairs()` reads `resource_type` and `action` off each
`InventoryInstance`'s payload. Read `AuthzPermissionV1`'s shape in
`libs/toolkit-gts/src/permission.rs` before writing it.

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run -p qa-catalog the_catalog_matches_the_enforced_surface
```

Expected: FAIL to compile (`crate::gts` does not exist), then FAIL with every
enforced pair listed as ungrantable.

- [ ] **Step 3: Write the catalog**

```rust
//! qa-catalog authorization permissions catalog.
//!
//! Declares every grantable permission as an [`AuthzPermissionV1`] GTS instance
//! via [`gts_instance!`]. Each invocation submits an `InventoryInstance` entry;
//! `types-registry::init()` aggregates and validates them at startup — no
//! registration code in `crate::gear`.
//!
//! `resource_type` values come from [`crate::domain::service::resources`] — the
//! same consts the service paths pass to `PolicyEnforcer` at enforce time, so
//! the catalog and the enforcement path share one source of truth, and
//! `permissions_tests` pins them to each other in both directions.
//!
//! Instance id layout (the suffix needs ≥5 dot-separated tokens):
//! `gts.cf.toolkit.authz.permission.v1~cf.core.qa_catalog.<entity>_<action>.v1`.
//!
//! **This catalog ships no grants.** Which role holds which permission is a
//! policy decision for the deployment's realm; what was missing was any way to
//! name these actions at all. Review finding #1.

#![allow(unknown_lints)]
#![allow(de0901_gts_string_pattern)]

use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::domain::service::{actions, resources};

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.qa_catalog.test_repo_get.v1"),
        resource_type: resources::TEST_REPO.name().to_owned(),
        action: actions::GET.to_owned(),
        display_name: "Read a test repository".to_owned(),
    }
}
// … one per enforced pair
```

Write a real `display_name` for each — an operator building a role reads that
string and nothing else. "Read a test repository", not "test_repo get".

- [ ] **Step 4: Run to verify it passes**

```bash
cargo nextest run -p qa-catalog --lib
make gts-docs
```

`make gts-docs` runs the `gts-validator`, which is what catches a malformed
instance id.

- [ ] **Step 5: Prove the drift test works in both directions**

Delete one `gts_instance!` block, run the test, confirm it FAILs naming that
pair as ungrantable. Restore. Then add a `gts_instance!` for a pair nothing
enforces, run, confirm it FAILs naming it as unenforced. Remove.

**Do not skip this.** An anti-drift test that passes vacuously is the exact
failure mode this catalog is being added to prevent.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-catalog
git commit -m "feat(qa-catalog): AuthzPermissionV1 permission catalog

grep -rln AuthzPermissionV1 gears/qa-platform returned nothing: the PEP asked
for qa.* actions AM-style and there was no catalog from which GTS/RBAC could
grant them. The request path itself is sound -- .authenticated() ->
access_scope(require_constraints=true) -> AccessScope -> #[secure(tenant_col)]
-- and it was not a complete AM/RMS integration without this.

Generated from the same resources::*/actions::* consts the enforcement path
reads, with an anti-drift test pinning catalog and enforced set in both
directions: an enforced pair with no entry is an action no role can grant, and
an entry with no enforcement is a grant that authorizes nothing.

No grants ship here. Review finding #1 (part 2 of 6)."
```

---

### Task 32: qa-environments' permission catalog

Identical structure to Task 31, with the one difference this whole plan is
careful about.

**Files:**
- Create: `gears/qa-platform/qa-environments/qa-environments/src/gts/{mod,permissions,permissions_tests}.rs`
- Modify: `qa-environments/qa-environments/src/lib.rs`, `Cargo.toml`

**Interfaces:**
- Consumes: `authz_surface::ENFORCED` from Task 30.
- Produces: instances under
  `gts.cf.toolkit.authz.permission.v1~cf.core.qa_environments.<entity>_<action>.v1`.

- [ ] **Step 1: Write the catalog with `qa.platform`, and pin it**

The instance id suffix says `environment` (it names the gear's concept, and the
gear is qa-environments); the `resource_type` says `qa.platform` (it names the
PDP string, which was deliberately not renamed). Those two disagreeing is
correct, and a reader will assume it is a mistake, so pin it:

```rust
/// **The instance id says `environment` and the `resource_type` says
/// `qa.platform`, and that is not a mistake.**
///
/// The aggregate was renamed `TargetPlatform` -> `Environment` across the Rust
/// identifiers, the REST wire, the routes and the UI. The PDP resource string
/// was deliberately left alone, because it is what policies are written
/// against and changing it would silently change who is authorized for what —
/// `domain::service::mod`'s `resources` doc records that decision in full.
///
/// So this catalog must emit `qa.platform`. A catalog generated from the
/// aggregate's name would emit `qa.environment`, match no policy, grant
/// nothing, and look entirely correct while doing it. Review finding #1.
#[test]
fn the_catalog_names_qa_platform_not_qa_environment() {
    let types: std::collections::BTreeSet<_> =
        catalog_pairs().iter().map(|(rt, _)| *rt).collect();
    assert!(
        types.contains("qa.platform"),
        "the environments resource type on the wire to the PDP is qa.platform; \
         found {types:?}"
    );
    assert!(
        !types.contains("qa.environment"),
        "qa.environment matches no policy in any deployment; found {types:?}"
    );
}
```

- [ ] **Step 2–5: As Task 31**

Failing test → catalog → `make gts-docs` → prove the drift test fails in both
directions.

Note `qa.lease`'s actions: `acquire` and `release` (`domain/service/mod.rs`),
which the review's `[by-design]` section connects to the force-start override.
Both are enforced and both belong in the catalog.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-environments
git commit -m "feat(qa-environments): AuthzPermissionV1 permission catalog

Same shape as qa-catalog's, with one difference worth stating: the instance ids
say 'environment' and the resource_type says 'qa.platform'. The aggregate was
renamed TargetPlatform -> Environment across Rust, REST, routes and UI, and the
PDP string was deliberately left alone because policies are written against it.

A catalog generated from the aggregate's name would emit qa.environment, match
no policy, grant nothing, and look correct. A test pins both halves.
Review finding #1 (part 3 of 6)."
```

---

### Task 33: qa-insights' permission catalog

**Files:**
- Create: `qa-insights/qa-insights/src/gts/{mod,permissions,permissions_tests}.rs`
- Modify: `qa-insights/qa-insights/src/lib.rs`, `Cargo.toml`

- [ ] **Steps 1–5: As Task 31** (failing test → catalog → `make gts-docs` → prove the drift test fails in both directions)

Two things specific to this gear:

- `qa.jira` and `qa.jira_config` and `qa.jira_bug` are three distinct resource
  types (`domain/service/mod.rs`), and the review found the attribution between
  them wrong in six places across three reviews. Get the pairs from Task 30's
  measured list, not from the names.
- `actions::COLLECT` is enforced on the *authenticated* trigger
  (`handlers/collect.rs:33`'s `trigger_collect`), not on the public HMAC route
  (`report_collect_count`), which has no `SecurityContext` to enforce against.
  Only the first is a grantable permission. Say so in the catalog's doc, because
  the omission looks like an oversight.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-insights
git commit -m "feat(qa-insights): AuthzPermissionV1 permission catalog

Six resource types. qa.jira, qa.jira_config and qa.jira_bug are distinct and
the review found their attribution wrong in six places across three reviews, so
the pairs come from the measured enforcement surface rather than from the names.

actions::COLLECT is catalogued for the authenticated trigger only: the public
HMAC route has no SecurityContext to enforce against, so it is not a grantable
permission and the catalog's doc says why. Review finding #1 (part 4 of 6)."
```

---

### Task 34: qa-runs' permission catalog

**Files:**
- Create: `qa-runs/qa-runs/src/gts/{mod,permissions,permissions_tests}.rs`
- Modify: `qa-runs/qa-runs/src/lib.rs`, `Cargo.toml`

- [ ] **Steps 1–5: As Task 31** (failing test → catalog → `make gts-docs` → prove the drift test fails in both directions)

This gear has the largest action set, including three that exist because an
operator override needs its own gate: `rerun`, `force_start` and `fire`.
`force_start` in particular is what the review's `[by-design]` entry on the
lease override depends on (`domain/service/runs.rs:1437`) — a force-start that
could not be gated separately would make that entry indefensible. Give it a
`display_name` that says what it overrides.

- [ ] **Step 6: Commit**

```bash
git add gears/qa-platform/qa-runs
git commit -m "feat(qa-runs): AuthzPermissionV1 permission catalog

Three resource types and the largest action set, including rerun, force_start
and fire. force_start is the gate the review's by-design entry on the lease
override depends on -- a forced run holds no lease, and the override is
defensible only because it has its own PEP action. Its display_name says so.
Review finding #1 (part 5 of 6)."
```

---

### Task 35: The tests the catalog unblocks

Three of the review's six mandatory-and-missing tests were blocked on the
catalog. They land with it.

**Files:**
- Create/modify: one test module per gear
- Create: a shared PDP fixture if none exists — check
  `qa-insights/src/domain/service/test_support.rs:581`'s `permissive_response`
  and `:617`'s `TenantScopedAuthZ` first; one of them may already be the fixture
  this needs.

**Interfaces:**
- Consumes: the four catalogs and `authz_surface::ENFORCED`.

- [ ] **Step 1: Action without a grant is denied**

```rust
/// **An action the caller has no grant for is denied.**
///
/// Listed by the review as mandatory and missing, and blocked until there was a
/// catalog to grant *from*: without one, every action was ungrantable and
/// "denied without a grant" was indistinguishable from "denied always".
///
/// Driven through a PDP fixture holding a role that grants exactly one pair, so
/// the denial of the second pair is a denial of something the same principal
/// could have been granted. Review finding #1.
#[tokio::test]
async fn an_action_without_a_grant_is_denied() {
    let pdp = pdp_granting(&[(resources::PRODUCT.name(), actions::GET)]);
    let svc = service_with_pdp(pdp);

    svc.get_product(&ctx(TENANT), product_id).await.expect("the granted action");

    let err = svc
        .delete_product(&ctx(TENANT), product_id)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Forbidden), "got {err:?}");
}
```

The positive half is load-bearing: without it the test passes against a fixture
that denies everything.

- [ ] **Step 2: A tenant-scoped grant sees only its own subtree**

```rust
/// **A grant scoped to one tenant does not reach another's rows.**
///
/// The review lists this as mandatory and missing and notes it needs a real PDP
/// fixture -- a mock that ignores the scope proves nothing, which is exactly
/// what review finding #41 was about in the products tests.
#[tokio::test]
async fn a_tenant_scoped_grant_reaches_only_its_own_subtree() {
    let pdp = pdp_granting_in_tenant(TENANT_A, &[(resources::PRODUCT.name(), actions::LIST)]);
    let svc = service_with_pdp(pdp);
    seed_product_in(TENANT_A, "ours").await;
    seed_product_in(TENANT_B, "theirs").await;

    let visible = svc.list_products(&ctx(TENANT_A)).await.unwrap();
    let names: Vec<_> = visible.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["ours"], "a tenant-scoped grant must not see {names:?}");
}
```

- [ ] **Step 3: Register a stub type-schema per resource type**

The catalog names permissions; a role definition targets a *type*. The platform
RBAC role-definition validator resolves a rule's `target_type` through the
types-registry (`get_type_schema`), so a resource type with no schema registered
is one no custom role can name — the catalog would list permissions nobody can
put in a role.

Ledger solves this with `authz_label_type_schemas()`
(`gears/bss/ledger/ledger/src/authz.rs:257-263`), built from `labels::ALL`. Do
the same in each gear, built from Task 30's `RESOURCE_TYPES`, and call it at
gear init:

```rust
/// Stub type-schemas for every resource type this gear enforces
/// ([`RESOURCE_TYPES`]).
///
/// The RBAC role-definition validator resolves a rule's `target_type` through
/// the types-registry, so without these a custom QA role cannot name any of
/// this gear's resources — the permission catalog would list permissions no
/// role definition can hold. Ledger's `authz_label_type_schemas` is the
/// precedent. Review finding #1.
#[must_use]
pub fn authz_resource_type_schemas() -> Vec<serde_json::Value> { /* … */ }
```

Add a test asserting one is produced per entry in `RESOURCE_TYPES`, so a
resource type added later cannot get a permission without a schema.

- [ ] **Step 4: The anti-drift test, per gear**

Already written in Tasks 31–34. This step is the confirmation that all four are
present and that each was proven to fail in both directions.

- [ ] **Step 5: Run everything**

```bash
cargo nextest run --workspace --exclude cf-gears-example-server
make gts-docs clippy
```

- [ ] **Step 6: Record what is still not covered**

The review lists six mandatory tests. Three land here. Update
`docs/Reviews/qa-platform-review-findings.md`'s "Mandatory tests still missing"
section to strike those three and leave the rest, so the next reader sees the
current state rather than the state at filing time.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform
git commit -m "test(qa-platform): the three tests the permission catalog unblocks

An action without a grant is denied; a tenant-scoped grant reaches only its own
subtree; the catalog equals the enforced set in all four gears. All three were
listed by the review as mandatory and missing, and all three were blocked on
there being a catalog to grant from -- without one, every action was
ungrantable and 'denied without a grant' could not be told from 'denied
always'.

Each has a positive half, deliberately: a denial test that passes against a
fixture denying everything checks nothing. Review finding #1 (part 6 of 6)."
```

---

## Phase completion

```bash
make fmt clippy gts-docs
make test-no-macros
make test-qa-runs-pg test-qa-insights-pg test-qa-catalog-git test-qa-platform-features
```

**Then hand off, do not proceed.** Authoring the role grants that use this
catalog is a policy decision for whoever owns the deployment's Keycloak realm,
and the review is explicit: *"Do not ship grants first."* Tell them the catalog
exists, and give them
`gears/qa-platform/deploy/realm/keycloak/` as the place the grants would live.
