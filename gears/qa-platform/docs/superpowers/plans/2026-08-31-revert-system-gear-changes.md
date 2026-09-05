# Revert the system-gear changes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore `gears/system/authz-resolver/` and `gears/system/event-broker/` to their exact upstream state, absorbing every capability qa-platform needed into qa-platform's own code.

**Architecture:** Two independent reverts.
*Authz* — qa-platform's nine nil-tenant "enumeration" contexts currently reach the PEP, which the stock plugin denies; the added `system_grants` config discharged that denial. Instead, each gear gains a **single named trust-elevation seam** returning `AccessScope::allow_all()`, which is the pattern `account-management` already uses upstream for exactly this (`tr_plugin/queries.rs`: *"centralizes the `AccessScope::allow_all()` trust elevation at a single named call site"*). The PEP is never consulted on those paths, so no grant is needed.
*Event broker* — the event path is already dead in production (the broker gear is a skeleton that registers no client, so `qa-insights` logs `event ingest DISABLED` and the reconcile sweep is the only ingest path). Deleting it removes dead code, drops qa-insights' dependency on the broker gear crate, and thereby removes the reason its config needed `#[serde(default)]`.

**Tech Stack:** Rust, ToolKit gear framework, SeaORM, `toolkit_security::AccessScope`, cargo features.

**Spec:** `gears/qa-platform/docs/FOOTPRINT-OUTSIDE-QA-PLATFORM.md` (sections 2.1 and 2.2 are the two items this plan discharges).

## Global Constraints

- **Zero net change under `gears/system/`.** The acceptance test for the whole plan is `git diff <upstream-base> -- gears/system/authz-resolver gears/system/event-broker` printing nothing. `<upstream-base>` is `db7660030`.
- **No new plugin.** The authz-resolver selects exactly one plugin by vendor (`domain/service.rs:41-63`, `get_plugin`) — there is no chaining, so a second plugin would *replace* static-authz rather than supplement it. Do not add one.
- Toolchain: `export PATH="$HOME/.cargo/bin:$PATH"`. Clippy is `-D warnings`. Never `cargo test --all-targets` at workspace level.
- **Never pipe a command whose exit code you need.** Redirect to a file and read `$?`.
- Every guard added must be **break-tested**: mutate the code so the new test *should* fail, confirm it does, then revert the mutation.
- The deploy feature set is `qa-platform,oidc-authn,static-authz,tenant-resolver-rg,static-credstore,postgres-credstore,platform-observation,qa-runs-argo` (`gears/qa-platform/deploy/cargo-features.argo`). `cargo check -p cf-gears-example-server --features "<that list>"` must pass before any deploy — a gear-only check misses cross-gear breakage.

---

## Phase 1 — Authz: absorb the grants into qa-platform

### Task 1: The trust-elevation seam in qa-runs

**Files:**
- Create: `gears/qa-platform/qa-runs/qa-runs/src/domain/elevated.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/mod.rs`
- Test: inline `#[cfg(test)]` in `elevated.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn enumeration_scope() -> AccessScope` — returns `AccessScope::allow_all()`. Tasks 2 and 3 call it.

- [ ] **Step 1: Write the failing test**

Create `gears/qa-platform/qa-runs/qa-runs/src/domain/elevated.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::enumeration_scope;

    /// The seam returns an unrestricted scope.
    ///
    /// Asserted through the secure-ORM's own predicate on the scope rather than a
    /// `Debug` string: the string is not a contract and would pass with a scope
    /// that merely *prints* like allow-all.
    #[test]
    fn the_enumeration_scope_is_unrestricted() {
        assert!(
            enumeration_scope().is_unconstrained(),
            "the sweep seam must be unrestricted; a clamped scope silently returns \
             only one tenant's rows and the sweep then does nothing for every other"
        );
    }
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p qa-runs elevated 2>&1 | tail -20`
Expected: FAIL — `cannot find function enumeration_scope`.

`is_unconstrained()` is the predicate's real name — verified at
`libs/toolkit-security/src/access_scope.rs:749`, alongside `is_deny_all()` at
`:757`. Do **not** add a method to `toolkit-security` if something looks missing;
that is another shared-crate change, which this plan exists to avoid.

- [ ] **Step 3: Write the implementation**

Put this above the test module in the same file:

```rust
//! The one place this gear elevates past the policy engine.
//!
//! # Why this exists
//!
//! Six of this gear's system-actor contexts are **nil-tenant**
//! (`system_actor::for_dispatch_enumeration`, `for_claim_reconciliation`,
//! `for_ttl_sweep`, `for_timeout_sweep`, `for_watch_scan`,
//! `for_schedule_tick`). A ticker holds no request, so it has no tenant to take
//! one from, and the sweep's whole job is to find work across every tenant.
//!
//! `static-authz-plugin` denies a nil-tenant request — correctly, since granting
//! unrestricted access to an unauthenticated caller is exactly what it is there
//! to stop. qa-platform previously discharged that denial by adding a
//! `system_grants` list to that shared plugin. That change has been reverted; the
//! elevation now lives here, in the gear that needs it, where it is one function
//! and one audit point instead of a config surface on a system gear every other
//! deployment also links.
//!
//! # Why this is not a security regression
//!
//! The elevation is **not reachable from a request**. Every caller is a ticker
//! that this gear spawns itself, and every one of them uses the scope only for a
//! cross-tenant **read** that enumerates which tenants have work. The writes that
//! follow are re-scoped per row under `system_actor`'s *tenant-bound* factories
//! (`for_dispatch`, `for_ttl_expiry`, `for_timeout_enforcement`,
//! `for_schedule_fire`), which is the pairing `system_actor`'s own module doc
//! already describes.
//!
//! This mirrors `account-management`, which does the same thing for the same
//! reason and says so: its hierarchy read port *"centralizes the
//! `AccessScope::allow_all()` trust elevation at a single named call site so this
//! gear no longer carries that concern"*
//! (`gears/system/account-management/account-management/src/tr_plugin/queries.rs`).

use toolkit_security::AccessScope;

/// The unrestricted scope a nil-tenant sweep enumerates with.
///
/// **Read paths only.** Pass a tenant-bound scope to anything that writes.
#[must_use]
pub fn enumeration_scope() -> AccessScope {
    AccessScope::allow_all()
}
```

Register the module in `gears/qa-platform/qa-runs/qa-runs/src/domain/mod.rs` beside the existing `pub mod system_actor;`:

```rust
pub mod elevated;
```

- [ ] **Step 4: Run the test and confirm it passes**

Run: `cargo test -p qa-runs elevated > /tmp/t1.txt 2>&1; echo "exit=$?"; grep 'test result' /tmp/t1.txt`
Expected: exit 0, `1 passed`.

- [ ] **Step 5: Commit**

```bash
git add gears/qa-platform/qa-runs/qa-runs/src/domain/elevated.rs \
        gears/qa-platform/qa-runs/qa-runs/src/domain/mod.rs
git commit -m "feat(qa-runs): a named trust-elevation seam for nil-tenant sweeps"
```

---

### Task 2: Route qa-runs' six dispatch/sweep enumerations through the seam

**Files:**
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/dispatch.rs` (lines ~1215, ~1517, ~1552, ~1768, ~1958, ~2254)
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/schedules.rs:669`
- Test: `gears/qa-platform/qa-runs/qa-runs/src/domain/service/dispatch_tests.rs`

**Interfaces:**
- Consumes: `crate::domain::elevated::enumeration_scope` from Task 1.
- Produces: no new public API. Behaviour: the six sweeps no longer call `PolicyEnforcer::access_scope` for their enumerating read.

- [ ] **Step 1: Find every enumerating scope call**

Run:

```bash
cd gears/qa-platform/qa-runs/qa-runs/src/domain/service
grep -n 'for_dispatch_enumeration\|for_claim_reconciliation\|for_ttl_sweep\|for_timeout_sweep\|for_watch_scan\|for_schedule_tick' dispatch.rs schedules.rs
```

For each hit, follow the `enumeration` binding to the helper it is passed to
(`queue_scope`, `runs_scope`, or the equivalent in `schedules.rs`) and note the
file:line of the `policy_enforcer.access_scope(...)` call it reaches. Write the
list down — the next step edits exactly those.

- [ ] **Step 2: Write the failing test**

Append to `dispatch_tests.rs`. This asserts the property that matters — the sweep
does not consult the policy engine — using the recording enforcer the suite
already has (`grep -n 'RecordingAuthZ\|RecordingEnforcer' dispatch_tests.rs
test_support.rs` to find its exact name and constructor):

```rust
/// The TTL sweep enumerates without asking the policy engine.
///
/// This is the property that lets `static-authz-plugin` stay stock. If the sweep
/// reaches the PEP with a nil-tenant context the stock plugin denies it, the
/// sweep silently finds no tenants, and expired rows are never reaped -- a
/// failure with no error anywhere, which is why it is asserted rather than
/// assumed.
#[tokio::test]
async fn the_ttl_sweep_does_not_consult_the_policy_engine() {
    let enforcer = Arc::new(RecordingAuthZ::new());
    let svc = dispatch_service_with_enforcer(enforcer.clone()).await;

    svc.run_ttl_sweep().await;

    assert!(
        !enforcer.requested_any_for_nil_tenant(),
        "the sweep must elevate through domain::elevated, not the PEP: the stock \
         static-authz plugin denies every nil-tenant request"
    );
}
```

If `RecordingAuthZ` has no `requested_any_for_nil_tenant`, add it next to its
existing `requested` method — it is a filter over the same recorded vector.
Match the real method and service names found in Step 1; do not invent them.

- [ ] **Step 3: Run it and confirm it fails**

Run: `cargo test -p qa-runs the_ttl_sweep_does_not_consult > /tmp/t2.txt 2>&1; echo "exit=$?"; tail -20 /tmp/t2.txt`
Expected: FAIL — the enforcer records a nil-tenant request.

- [ ] **Step 4: Replace the enumerating scope calls**

At each site from Step 1, replace the PEP call with the seam. The shape, using
the TTL sweep's `queue_scope` as the model:

```rust
// Before
let depth_scope = self.queue_scope(&enumeration, actions::LIST, None).await?;

// After
// Nil-tenant enumeration: elevated here rather than authorized. See
// `domain::elevated` for why, and for why the per-row writes below are
// still tenant-bound.
let depth_scope = crate::domain::elevated::enumeration_scope();
```

Leave every **tenant-bound** call site alone. Only the six factories listed in
Task 1's doc comment change; if a helper is shared between a nil-tenant and a
tenant-bound caller, add a separate call path rather than widening the shared one.

- [ ] **Step 5: Run the test and the whole gear suite**

```bash
cargo test -p qa-runs > /tmp/t2b.txt 2>&1; echo "exit=$?"; grep 'test result' /tmp/t2b.txt
```
Expected: exit 0. The suite was **890 passing** before this plan; it must not drop.

- [ ] **Step 6: Break-test the new guard**

Revert one site to the PEP call, re-run `cargo test -p qa-runs the_ttl_sweep_does_not_consult`,
confirm FAIL, then restore. A guard that cannot fail is worse than no guard.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-runs/qa-runs/src/domain/service/
git commit -m "refactor(qa-runs): elevate nil-tenant sweeps instead of authorizing them"
```

---

### Task 3: The same seam in qa-insights and qa-catalog

**Files:**
- Create: `gears/qa-platform/qa-insights/qa-insights/src/domain/elevated.rs`
- Create: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/elevated.rs`
- Modify: `gears/qa-platform/qa-insights/qa-insights/src/domain/mod.rs`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/domain/mod.rs`
- Modify: `gears/qa-platform/qa-insights/qa-insights/src/domain/service/tenants.rs:204`
- Modify: `gears/qa-platform/qa-catalog/qa-catalog/src/gear.rs:328` and `:385`

**Interfaces:**
- Consumes: nothing from other tasks (each gear gets its own copy — these are
  separate crates and a shared helper would mean a new shared crate, which is the
  coupling this plan is removing).
- Produces: `enumeration_scope()` in each of the two gears, same signature as Task 1.

- [ ] **Step 1: Copy the seam into both gears**

Copy `elevated.rs` from Task 1 into each gear's `src/domain/`, and in each,
rewrite the "Why this exists" paragraph to name that gear's own factories:

- qa-insights: `system_actor::for_ticker_enumeration` (one factory, used at
  `domain/service/tenants.rs:204`).
- qa-catalog: `system_actor::for_branch_refresh_enumeration` and
  `for_bundle_gc` (used at `gear.rs:328` and `gear.rs:385`).

Add `pub mod elevated;` to each gear's `domain/mod.rs`. Keep the
`the_enumeration_scope_is_unrestricted` test in each copy.

- [ ] **Step 2: Write the failing tests**

In qa-insights, the property is the one the gear's own docs already call out —
`tenants_with_results` is the sweep's only way to find tenants:

```rust
/// Ticker enumeration does not consult the policy engine.
///
/// `tenants.rs` enumerates with a nil-tenant context; under the stock
/// static-authz plugin that is a deny, which would leave the reconcile sweep
/// finding zero tenants forever -- the exact closed loop that made Analytics
/// show zeros before.
#[tokio::test]
async fn ticker_enumeration_does_not_consult_the_policy_engine() {
    let enforcer = Arc::new(RecordingAuthZ::new());
    let svc = tenants_service_with_enforcer(enforcer.clone()).await;

    let _ = svc.tenants_with_results().await;

    assert!(!enforcer.requested_any_for_nil_tenant());
}
```

Write the qa-catalog equivalent against `list_refresh_targets`, which
`tests_tenant_scoping.rs:869` already calls with
`for_branch_refresh_enumeration()` — reuse that test's setup.

- [ ] **Step 3: Run both and confirm they fail**

```bash
cargo test -p qa-insights ticker_enumeration_does_not > /tmp/t3a.txt 2>&1; echo "exit=$?"
cargo test -p qa-catalog refresh_targets > /tmp/t3b.txt 2>&1; echo "exit=$?"
```
Expected: both FAIL.

- [ ] **Step 4: Replace the three enumerating scope calls**

Same edit shape as Task 2 Step 4, at `tenants.rs:204`, `qa-catalog/gear.rs:328`
and `qa-catalog/gear.rs:385`.

`for_bundle_gc` is the one **write** among them (the grant said `qa.bundle` /
`delete`, `allow_write: true`). Elevate only the enumerating read that finds
candidate bundles; issue each delete under a tenant-bound context built from the
row's own `tenant_id`, matching the pairing in Task 1's doc. If no tenant-bound
factory exists for it, add `for_bundle_delete(tenant: TenantBound)` next to
`for_bundle_gc` in that gear's `system_actor.rs`.

- [ ] **Step 5: Run both suites**

```bash
cargo test -p qa-insights > /tmp/t3c.txt 2>&1; echo "exit=$?"; grep 'test result' /tmp/t3c.txt
cargo test -p qa-catalog > /tmp/t3d.txt 2>&1; echo "exit=$?"; grep 'test result' /tmp/t3d.txt
```
Expected: exit 0 for both. Baselines: qa-insights **716 + 4**, qa-catalog **219**.

- [ ] **Step 6: Break-test both new guards**, as in Task 2 Step 6.

- [ ] **Step 7: Commit**

```bash
git add gears/qa-platform/qa-insights gears/qa-platform/qa-catalog
git commit -m "refactor(qa-insights,qa-catalog): elevate ticker enumeration instead of authorizing it"
```

---

### Task 4: Revert static-authz-plugin and drop the grants from config

**Files:**
- Revert: `gears/system/authz-resolver/plugins/static-authz-plugin/` (9 paths)
- Modify: `gears/qa-platform/config/qa-platform-stack.yaml:344-357`
- Modify: `gears/qa-platform/deploy/helm/qa-platform/files/qa-platform-stack.yaml` (same block)
- Test: `gears/qa-platform/deploy/helm/tests/test_no_system_gear_changes.py` (new)

**Interfaces:**
- Consumes: Tasks 1-3 (nothing may still need a grant).
- Produces: an upstream-clean `gears/system/authz-resolver/`.

- [ ] **Step 1: Confirm nothing still needs a grant**

```bash
grep -rn 'system_grants' --include=*.rs --include=*.yaml . | grep -v node_modules > /tmp/grants.txt
cat /tmp/grants.txt
```
Every remaining hit must be in the two config files and the plugin itself. A hit
anywhere in `gears/qa-platform/**/*.rs` means a call site was missed — go back to
Task 2 or 3.

- [ ] **Step 2: Revert the plugin**

```bash
git checkout db7660030 -- gears/system/authz-resolver/plugins/static-authz-plugin/
git status --short gears/system/authz-resolver/
```
`grants.rs` and `gear_tests.rs` were added by this branch, so `checkout` will not
remove them. Delete them explicitly and confirm:

```bash
rm -f gears/system/authz-resolver/plugins/static-authz-plugin/src/domain/grants.rs \
      gears/system/authz-resolver/plugins/static-authz-plugin/src/gear_tests.rs
git diff --stat db7660030 -- gears/system/authz-resolver/
```
Expected: **no output** — a clean revert.

- [ ] **Step 3: Remove the grants from both config files**

Delete the `system_grants:` block (the four entries at
`config/qa-platform-stack.yaml:344-357`) and the same block in the Helm copy.
Leave the surrounding `vendor`/`priority` keys. Update the three comments that
reference `system_grants` (lines ~8, ~277, ~620, ~631, ~673 in each file) to say
the elevation now lives in each gear's `domain::elevated`.

- [ ] **Step 4: Write the guard that keeps it reverted**

Create `gears/qa-platform/deploy/helm/tests/test_no_system_gear_changes.py`,
alongside the existing `test_no_environment_hardcode.py`:

```python
"""qa-platform must not modify gears/system/authz-resolver or event-broker.

Both were modified once and reverted (see
docs/FOOTPRINT-OUTSIDE-QA-PLATFORM.md). This fails the build if either comes
back, because the cost of finding out at review time is a rewrite of whatever
depended on it.
"""
import subprocess

UPSTREAM_BASE = "db7660030"
GUARDED = [
    "gears/system/authz-resolver",
    "gears/system/event-broker",
]


def test_system_gears_are_untouched():
    for path in GUARDED:
        diff = subprocess.run(
            ["git", "diff", "--stat", UPSTREAM_BASE, "--", path],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        assert diff == "", (
            f"{path} differs from upstream {UPSTREAM_BASE}:\n{diff}\n\n"
            "qa-platform must absorb what it needs on its own side -- see each "
            "gear's domain::elevated module for the authz precedent."
        )
```

- [ ] **Step 5: Run it, and break-test it**

```bash
cd gears/qa-platform/deploy/helm && python -m pytest tests/test_no_system_gear_changes.py -v > /tmp/t4.txt 2>&1; echo "exit=$?"; tail -5 /tmp/t4.txt
```
Expected: PASS. Then break-test: `echo "// x" >> gears/system/event-broker/event-broker/src/config.rs`,
re-run (expect FAIL), `git checkout -- gears/system/event-broker/event-broker/src/config.rs`, re-run (expect PASS).

- [ ] **Step 6: Verify the whole build**

```bash
FEATS="qa-platform,oidc-authn,static-authz,tenant-resolver-rg,static-credstore,postgres-credstore,platform-observation,qa-runs-argo"
cargo check -p cf-gears-example-server --features "$FEATS" > /tmp/t4b.txt 2>&1; echo "exit=$?"
for p in qa-runs qa-environments qa-insights qa-catalog; do
  cargo clippy -p $p --all-targets > /tmp/clippy-$p.txt 2>&1; echo "$p clippy=$?"
done
```
Expected: all exit 0.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "revert(authz-resolver): restore the stock plugin, elevate in qa-platform instead"
```

---

## Phase 2 — Event broker: delete the dead path

> Ordering note: Phase 2 is independent of Phase 1 and may be done first. The
> guard added in Task 4 Step 4 covers both, so whichever phase runs second only
> needs to un-skip its half.

### Task 5: Delete qa-runs' publisher

**Files:**
- Delete: `gears/qa-platform/qa-runs/qa-runs/src/infra/events/publisher.rs`, `payloads.rs`
- Modify: `gears/qa-platform/qa-runs/qa-runs/src/infra/events/mod.rs`, `src/domain/ports/event_publisher.rs`, `src/gear.rs` (~line 431-443), `Cargo.toml:123`
- Test: existing `qa-runs` suite

**Interfaces:**
- Consumes: nothing.
- Produces: a qa-runs with no `event-broker-sdk` dependency.

- [ ] **Step 1: Establish the baseline**

```bash
cargo test -p qa-runs > /tmp/t5-before.txt 2>&1; grep 'test result' /tmp/t5-before.txt
```
Record the number. It is the figure Step 5 compares against.

- [ ] **Step 2: Delete the publisher and its port**

```bash
cd gears/qa-platform/qa-runs/qa-runs
git rm src/infra/events/publisher.rs src/infra/events/payloads.rs
git rm src/domain/ports/event_publisher.rs
```

Remove the `pub mod` lines for them, the `BrokerEventPublisher::connect(...)`
block at `src/gear.rs:431-443`, and every call site the compiler then names.
Where a service published an event inline (`grep -rn 'publish' src/domain/service/`),
delete the call — **not** the surrounding logic. The gear already logs
`qa-runs: no event broker deployed; lifecycle events are not published` on every
boot, so nothing downstream consumes these.

- [ ] **Step 3: Drop the dependency**

Remove `event-broker-sdk = { workspace = true }` from `Cargo.toml:123`.

- [ ] **Step 4: Compile and fix what the compiler names**

```bash
cargo check -p qa-runs --all-targets > /tmp/t5.txt 2>&1; echo "exit=$?"; grep -E '^error' -A5 /tmp/t5.txt | head -40
```
Iterate until exit 0. Expect the exhaustive-destructuring and mock-repository
patterns from `test_support.rs` to need updating — they are compile errors, not
judgement calls.

- [ ] **Step 5: Run the suite and compare to the baseline**

```bash
cargo test -p qa-runs > /tmp/t5b.txt 2>&1; echo "exit=$?"; grep 'test result' /tmp/t5b.txt
```
The count will be **lower** than Step 1 (the publisher's own tests are gone).
Confirm every removed test belonged to the deleted files:
`diff <(grep '^test ' /tmp/t5-before.txt | sort) <(grep '^test ' /tmp/t5b.txt | sort)`.
Any *other* test disappearing means logic was deleted with the plumbing — restore it.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor(qa-runs): delete the unreachable event publisher"
```

---

### Task 6: Delete qa-insights' consumer and its broker dependency

**Files:**
- Delete: `gears/qa-platform/qa-insights/qa-insights/src/infra/events/` (whole directory: `consumer.rs`, `consumer_tests.rs`, `payloads.rs`, `mod.rs`)
- Modify: `src/gear.rs`, `src/lib.rs`, `src/config.rs`, `src/domain/local_client/mod.rs`, `src/domain/service/ingest.rs`, `src/infra/leader/mod.rs`, `Cargo.toml:85,111,201,208,211,408`
- **Keep:** `src/infra/storage/migrations/m20260818_000002_offset_store.rs`
- Test: existing `qa-insights` suite

**Interfaces:**
- Consumes: nothing.
- Produces: a qa-insights that depends on neither `event-broker` nor `event-broker-sdk`.

- [ ] **Step 1: Establish the baseline**

```bash
cargo test -p qa-insights > /tmp/t6-before.txt 2>&1; grep 'test result' /tmp/t6-before.txt
```

- [ ] **Step 2: Delete the consumer**

```bash
cd gears/qa-platform/qa-insights/qa-insights
git rm -r src/infra/events/
```

In `src/gear.rs`, remove the `deps = [...]` entry `event_broker` (at
`src/gear.rs:459`), the consumer construction, and the two startup log lines that
describe the broker being absent — with no broker in the dependency list those
statements are no longer true, and a log that describes a configuration that
cannot occur is worse than none.

**Keep the reconcile ticker exactly as it is.** It is the only ingest path today
and this task must not touch it.

- [ ] **Step 3: Keep the offset-store migration, and say why**

Do **not** delete `m20260818_000002_offset_store.rs`. It has already run on
deployed databases (`evbk_consumer_offsets` exists on `10.136.20.200`), and the
migration runner records it as applied — removing the file makes the code's
migration list disagree with what the database contains. Add to its module doc:

```rust
//! **Superseded 2026-08-31.** The consumer this table served was deleted with
//! qa-insights' event-broker dependency. The migration stays because it has
//! already run on deployed databases and the runner records it as applied;
//! deleting the file would make the code's list disagree with the schema. The
//! table is inert and no code reads or writes it.
```

- [ ] **Step 4: Drop the dependencies and the config knobs**

Remove from `Cargo.toml` both the `event-broker` gear crate (line 85) and every
`event-broker-sdk` entry (lines ~201, ~208, ~211, ~408), plus the comment blocks
that explain them. Remove any consumer-only keys from `src/config.rs` (the
consumer-group and offset settings) along with their doc comments.

- [ ] **Step 5: Compile, then run the suite against the baseline**

```bash
cargo check -p qa-insights --all-targets > /tmp/t6.txt 2>&1; echo "exit=$?"; grep -E '^error' -A5 /tmp/t6.txt | head -40
cargo test -p qa-insights > /tmp/t6b.txt 2>&1; echo "exit=$?"; grep 'test result' /tmp/t6b.txt
```

As in Task 5 Step 5, diff the test-name lists and confirm every disappearance
belongs to a deleted file.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor(qa-insights): delete the event consumer and the broker dependency"
```

---

### Task 7: Revert event-broker and remove its config stanza

**Files:**
- Revert: `gears/system/event-broker/event-broker/src/config.rs`
- Modify: `gears/qa-platform/config/qa-platform-stack.yaml:566-572`
- Modify: `gears/qa-platform/deploy/helm/qa-platform/files/qa-platform-stack.yaml` (same stanza)

**Interfaces:**
- Consumes: Task 6 (nothing may still link the broker gear).
- Produces: an upstream-clean `gears/system/event-broker/`.

- [ ] **Step 1: Prove nothing links the broker gear any more**

```bash
grep -rn 'event-broker\|event_broker' --include=Cargo.toml . | grep -v node_modules
```
Expected: only the broker's own manifests and the root workspace member line.
Any hit under `gears/qa-platform/` means Task 6 is incomplete.

- [ ] **Step 2: Revert the config**

```bash
git checkout db7660030 -- gears/system/event-broker/event-broker/src/config.rs
git diff --stat db7660030 -- gears/system/event-broker/
```
Expected: **no output**.

- [ ] **Step 3: Remove the stanza from both config files**

Delete `config/qa-platform-stack.yaml:566-572` (the `event-broker:` block with
its `database:` and `config:` keys) and the same block in the Helm copy. Leave
the neighbouring comment about `cluster` using `config_or_default`.

This is the step the revert depends on: with the gear no longer linked, an
`event-broker:` section describes a gear that does not exist.

- [ ] **Step 4: Verify the deploy build and the guard**

```bash
FEATS="qa-platform,oidc-authn,static-authz,tenant-resolver-rg,static-credstore,postgres-credstore,platform-observation,qa-runs-argo"
cargo check -p cf-gears-example-server --features "$FEATS" > /tmp/t7.txt 2>&1; echo "exit=$?"
cd gears/qa-platform/deploy/helm && python -m pytest tests/ -v > /tmp/t7b.txt 2>&1; echo "exit=$?"; tail -8 /tmp/t7b.txt
```
Expected: both exit 0, including `test_no_system_gear_changes`.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "revert(event-broker): restore the stock config, drop the qa-platform dependency"
```

---

### Task 8: End-to-end verification on the dev stand

**Files:** none — this task only runs things.

- [ ] **Step 1: The acceptance test for the whole plan**

```bash
git diff --stat db7660030 -- gears/system/authz-resolver gears/system/event-broker > /tmp/t8.txt 2>&1
echo "bytes of diff: $(wc -c < /tmp/t8.txt)"
```
Expected: **0**. If not, the plan is not done.

- [ ] **Step 2: Full local verification**

```bash
FEATS="qa-platform,oidc-authn,static-authz,tenant-resolver-rg,static-credstore,postgres-credstore,platform-observation,qa-runs-argo"
cargo check -p cf-gears-example-server --features "$FEATS" > /tmp/t8a.txt 2>&1; echo "server check=$?"
for p in qa-runs qa-environments qa-insights qa-catalog; do
  cargo test  -p $p > /tmp/t8-$p.txt 2>&1;  echo "$p test=$?  $(grep -h 'test result' /tmp/t8-$p.txt | head -1)"
  cargo clippy -p $p --all-targets > /tmp/t8c-$p.txt 2>&1; echo "$p clippy=$?"
done
cd gears/qa-platform/qa-platform-ui && npx tsc --noEmit > /tmp/t8-tsc.txt 2>&1; echo "tsc=$?"
```
Expected: every exit 0.

- [ ] **Step 3: Deploy**

```bash
cd gears/qa-platform
./deploy/remote/deploy-k8s.sh --target root@10.136.20.200 \
    --public-origin https://10.136.20.200 > /tmp/deploy.txt 2>&1; echo "exit=$?"
```
**Read `/tmp/deploy.txt`; do not trust the exit code alone** — a previous deploy
reported success while its last step had failed. Confirm `51 PASS, 0 FAIL` and
`VERIFY-K8S: every check above passed individually`.

- [ ] **Step 4: Prove the tickers still work without grants**

This is the behavioural acceptance test: with `system_grants` gone, the sweeps
must still find work. Launch a run, let it finish, then confirm the reconcile
sweep ingested it **automatically** within one `reconcile_interval_seconds` (300s):

```bash
# after the run reaches a terminal state, wait one interval, then:
ssh root@10.136.20.200 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
  kubectl exec -n qa-platform statefulset/qa-platform-postgres -- \
    psql -U qa -d qa_insights -tAc \
    "select count(*) from qa_test_results where run_id = '"'"'<new-run-id>'"'"'"'
```
Expected: non-zero. A zero here means an enumeration path is still being denied —
check the gears log for a PEP denial rather than assuming the sweep is slow.

- [ ] **Step 5: Confirm no authz denials in the log**

```bash
ssh root@10.136.20.200 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
  kubectl -n qa-platform logs deploy/qa-platform-gears --tail=100000' > /tmp/gears.txt 2>&1
grep -icE 'denied|forbidden|not authorized' /tmp/gears.txt
```
Expected: 0, or only entries unrelated to the tickers.

- [ ] **Step 6: Commit and update the footprint document**

Update `docs/FOOTPRINT-OUTSIDE-QA-PLATFORM.md` — sections 2.1 and 2.2 become
"reverted on 2026-08-31, see `docs/superpowers/plans/2026-08-31-revert-system-gear-changes.md`",
and the summary table's two "Modified gear" rows become "Reverted".

```bash
git add -A && git commit -m "docs(qa-platform): record the system-gear reverts"
```

---

## Self-review notes

**Spec coverage.** The spec's section 2.1 (authz) is Tasks 1-4; section 2.2
(event broker) is Tasks 5-7. Section 5 (`ci.yml`) and sections 3-4 are explicitly
**out of scope** — the user marked them acceptable.

**Known risk, stated rather than hidden.** Task 3's qa-catalog `for_bundle_gc`
path is the only **write** among the elevated sites. The plan splits it into an
elevated read plus tenant-bound deletes, which is more work than the other five
sites and is where an executor is most likely to take a shortcut. If the split
proves awkward, stop and raise it rather than elevating the delete.

**Assumption checked before writing, not left to the executor.**
`AccessScope::allow_all()` is `pub`
(`libs/toolkit-security/src/access_scope.rs:689`) and its own doc states the
posture this plan relies on: *"This represents a legitimate PDP decision with no
row-level filtering. Not a bypass — it's a valid authorization outcome."* The
predicate is `is_unconstrained()` (`:749`). Both are used by
`account-management` already, so no shared-crate change is needed.
