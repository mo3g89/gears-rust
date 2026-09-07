# What qa-platform changed outside its own directory

**Scope.** Commit `925240eb7` ("feat(qa-platform): the QA platform subsystem"),
686 files, measured against its parent `db7660030` — the last upstream commit
before any qa-platform work. **Updated 2026-09-01:** the two behavioural
changes to `gears/system/` this document originally described have since been
reverted (see section 2). The figures below are recounted against the current
tree, not the original commit — they will not match a diff taken straight
against `925240eb7`.

**The question this answers.** qa-platform was supposed to live entirely in
`gears/qa-platform/`. It does not. This file lists every path outside that
directory, says why it was touched, and states whether it can be reverted.

**The short answer.** 31 paths lie outside `gears/qa-platform/`. **23 are new
files that add nothing to existing behaviour. 8 modify pre-existing files.** Of
those 8, **7 are mechanical registration or tooling** and **1 is a change to
shared CI**. The two behavioural changes to existing system gears that used to
account for the rest — `static-authz-plugin` and `event-broker/src/config.rs`
— were reverted on 2026-09-01 and no longer differ from upstream, so they no
longer appear in this count at all.

Nothing outside `gears/qa-platform/` is reachable in a default build:
`apps/cf-gears-example-server` declares `default = []`, and every qa-platform
dependency is `optional = true`. A build that does not name `--features
qa-platform` compiles none of it.

---

## 1. Summary table

| # | Path | Kind | Behavioural risk | Revertible |
|---|---|---|---|---|
| 1 | `gears/system/authz-resolver/plugins/static-authz-plugin/` (9 files) | **Reverted** (2026-09-01) | None — byte-identical to upstream | Done |
| 2 | `gears/system/event-broker/event-broker/src/config.rs` | **Reverted** (2026-09-01) | None — byte-identical to upstream | Done |
| 3 | `gears/credstore/plugins/postgres-credstore-plugin/` (22 files) | New crate | None | Yes, trivially |
| 4 | `apps/cf-gears-example-server/Cargo.toml` | Registration | None | Required |
| 5 | `apps/cf-gears-example-server/src/registered_gears.rs` | Registration | None | Required |
| 6 | `Cargo.toml` (root) | Workspace members | None | Required |
| 7 | `Cargo.lock` | Generated | None | Regenerated |
| 8 | `.github/workflows/ci.yml` | **Shared CI** | **Real** | Yes, trivially |
| 9 | `Makefile` | Tooling | Minimal — one narrowed exclude (§4.1) | Yes, trivially |
| 10 | `.gitignore` | Tooling | None for builds (two caveats — §4.2) | Yes, mostly |
| 11 | `docs/GEARS.md` | Docs | None | Yes, trivially |
| 12 | `config/qa-platform.yaml` | New config | None | Yes, trivially |

---

## 2. The two changes that used to alter existing gears — both reverted

Both changes described in this section were reverted on 2026-09-01. This
section is now a historical record of what they were, why they existed, and
how qa-platform absorbed what it needed once they were gone. The full
before/after argument, task-by-task, is in
`docs/superpowers/plans/2026-08-31-revert-system-gear-changes.md`.

**Verification.** `git diff --stat db7660030 -- gears/system/authz-resolver
gears/system/event-broker` is byte-for-byte empty and `git status --short
gears/system/` is clean. Both gears are their exact upstream selves again. The
composed build exits 0; qa-runs (853), qa-environments (173), qa-insights
(703) and qa-catalog (226) all pass; clippy is clean at `-D warnings` on all
four; `npx tsc --noEmit` and `npx vitest run` (211/211) are clean on the UI.
qa-runs and qa-insights' test counts dropped from their pre-revert figures
because deleted code took its own tests with it — every disappearance was
checked against the deleted files, not assumed benign.
`gears/qa-platform/deploy/helm/tests/test_no_system_gear_changes.py` runs the
same diff as a guard so a future change to either gear fails a manual
`pytest tests/` run — note that nothing in CI or any Makefile invokes it, so it
protects a deliberate re-run, not an automated one.

**Not yet verified.** The end-to-end check on the dev stand — deploy, a live
run through the reconcile sweep, and a scan of the gears log for authz denials
— has **not** been performed. It is blocked on a permission gate around
deploying to the shared dev stand, not on anything about the code above; the
acceptance diff and the full local suite are what this document can currently
back up.

### 2.1 `static-authz-plugin` — the cross-tenant grant path, reverted

**What it was.** The plugin previously denied every request whose tenant was
the nil UUID — the shape a gear's *system actor* sends when a background
ticker acts with no user request behind it. A `system_grants` config list was
added so an entry covering the request's subject type, resource type and
action would permit it with a single predicate-less constraint, deliberately
without the per-tenant clamp, since cross-tenant breadth was the point.
`Service` gained a field (`Service::with_config`) and `init` called
`cfg.validate()`. Files: `Cargo.toml`, `src/config.rs`, `src/domain/grants.rs`
(new), `src/domain/mod.rs`, `src/domain/service.rs`, `src/domain/service_tests.rs`,
`src/gear.rs`, `src/gear_tests.rs` (new), `src/lib.rs` — 567 insertions, 8
deletions against upstream.

**Why it existed.** qa-platform's gears run tickers — qa-environments observes
platforms, qa-runs dispatches the queue, qa-insights reconciles results. A
ticker holds no request and therefore no tenant, so under the stock rule every
one of them was denied. There was no way to express "this gear's system actor
may read runs across tenants" without either a grant list on the shared plugin
or an equivalent seam somewhere else.

**What replaced it.** `static-authz-plugin` is exactly its upstream self again
— the `system_grants` config path is gone, and so is `src/domain/grants.rs`
and `src/gear_tests.rs`. In its place, each of the three gears that ran
nil-tenant tickers gained a single named seam, `domain::elevated::enumeration_scope()`,
returning `AccessScope::allow_all()` — the same pattern `account-management`
already uses upstream (`tr_plugin/queries.rs`). Nine nil-tenant `system_actor`
factories now feed that seam rather than the policy engine: six in qa-runs,
one in qa-insights, two in qa-catalog. They reach the seam through eight call
sites, not nine — qa-runs' `list_queued_platforms` is the enumerating read for
two of its six factories, so its own count is five call sites for six
factory mints; qa-insights and qa-catalog have one call site per factory, as
usual. The elevation is read-only; every write that
follows is re-scoped per row under a tenant-bound `system_actor` factory.
qa-catalog's bundle GC — the only write among the elevated sites — was split
so the elevated read enumerates the distinct tenants owning expired bundles,
and the existing atomic select+delete then runs once per tenant under a
PEP-derived tenant-bound scope from a new `system_actor::for_bundle_delete(tenant)`.
The `system_grants:` block is gone from both stack YAMLs (`config/qa-platform-stack.yaml`
and its Helm copy).

**Cost.** Nine nil-tenant factories (eight call sites) across three gears,
plus the tenant-bound split for the one write path, versus a single config
list on a shared plugin. The
trade-off was made deliberately: no shared system gear carries a
qa-platform-shaped config surface any more.

### 2.2 `event-broker` — the two serde defaults, reverted

**What it was.** `src/config.rs` added `#[derive(Default)]` and `#[default]`
on `DeploymentMode::Standalone`, `#[serde(default)]` on `mode` and
`default_storage_backend`, and a test module (the struct previously had no
tests at all). `qa-insights` declared `deps = [.., event_broker, ..]`, which
expands to a `pub use ::event_broker`, so every deployment that linked
qa-insights also linked and initialised the broker whether or not anything
published. The broker read its config through the *required* form, so a
`config:` section omitting `mode` failed the gear's `init` and with it the
whole server's boot — the two serde defaults existed to make that config
optional.

**What replaced it.** The event path was already dead in production before
the revert — the broker gear is a skeleton that registers no client. qa-runs'
publisher, its payloads and its port were deleted outright. qa-insights' event
consumer and both broker dependencies — the `event-broker` gear crate and
`event-broker-sdk` — were deleted, including the `event_broker` token in its
`#[toolkit::gear(deps = [...])]`. With nothing left depending on it,
`gears/system/event-broker/src/config.rs` was reverted to its exact upstream
form and the `event-broker:` stanza removed from both stack YAMLs and from
`config/qa-platform.yaml`. The `"event-broker"` entry was also removed from
the root `Cargo.toml`'s `cargo-shear` ignore list, since nothing under
`gears/qa-platform/` references the crate any more.

**One thing was deliberately kept.**
`qa-insights/src/infra/storage/migrations/m20260818_000002_offset_store.rs` was
not deleted, because the `evbk_consumer_offsets` table it creates already
exists on deployed databases and the migration runner records it as applied —
removing the file would make the code's migration list disagree with the live
schema. Its DDL, previously imported from `event-broker-sdk`, was inlined
locally and verified byte-identical to what the SDK produced, so a fresh
database still gets exactly the deployed schema. The table is now inert; no
code reads or writes it.

---

## 3. New files — additive, zero risk

### 3.1 `gears/credstore/plugins/postgres-credstore-plugin/` (22 files)

A brand-new credstore plugin storing secrets in Postgres, alongside the existing
in-memory `static-credstore-plugin`. Nothing pre-existing was edited to make room
for it.

**Why.** qa-platform stores platform kubeconfigs in credstore. The in-memory
plugin loses them on restart, which is not usable for a deployed stack.

**Is it safe?** The example-server feature comment states the property directly:
the feature is off by default, and with it present but `credstore.config.vendor`
left alone, the in-memory plugin still wins the vendor match. So enabling it
alone changes no secret resolution.

**Revertible?** Trivially — delete the directory, its workspace member line, its
optional dependency and its `registered_gears.rs` import. Nothing else refers to
it. It is arguably *general-purpose* infrastructure that belongs where it is, not
qa-platform-specific.

### 3.2 `config/qa-platform.yaml`

A standalone example configuration. Referenced by nothing at build time.
Deleting it affects nothing.

---

## 4. Registration and tooling — mechanical, but mandatory

These cannot be avoided while qa-platform lives in this workspace at all. They
are the irreducible cost of adding crates to a Cargo workspace and a composed
binary.

| File | Change | Note |
|---|---|---|
| `Cargo.toml` (root) | 8 workspace members, `event-broker-sdk` + `serde_yaml` workspace deps | Cargo requires every path crate to be a member |
| `apps/…/Cargo.toml` | features `qa-platform`, `qa-runs-argo`, `runner-secret`, `postgres-credstore`, `oidc-authn`; 6 optional deps | All off by default (`default = []`) |
| `apps/…/registered_gears.rs` | 6 `use … as _;` imports behind `#[cfg(feature)]` | Without the import the `inventory` registration never runs and the gear cannot resolve |
| `Cargo.lock` | Regenerated | Follows from the above |
| `docs/GEARS.md` | +12 lines listing the new gears | Documentation only |

Note two of these are **not qa-platform's**: the `oidc-authn` feature and import
wire up a plugin crate that **already existed and was wired into no binary**.
qa-platform needed it, so it got wired. That is arguably a fix the tree wanted
anyway, and it is independent of qa-platform.

### 4.1 `Makefile`

Adds `ui-install`, `ui-lint`, `ui-test`, `ui-build`, `ui-contract` and
`test-qa-runs-pg`, and — with the review remediation — `test-qa-insights-pg`,
`test-qa-catalog-git`, `test-qa-platform-features` and `helm-tests`. All new
targets, fully revertible.

**One pre-existing target was altered** (review remediation, Phase 7): the
shared `gts-docs` target gained `--exclude "**/tsconfig*.json"` beside the four
excludes it already carried. This is the only edit qa-platform has made to
something in this file that other gears use, which is why row 9's risk is no
longer literally "None".

*Why it was needed.* `make gts-docs` had been **red since `a1767401f`**, the
commit that landed the QA Platform subsystem: the validator's strict JSON
parser reports a scan error on `qa-platform-ui/tsconfig.json:9`
(`/* Bundler mode */`, from the Vite template), and the target treats an
unscannable file as a failure — *"✗ 1 file(s) could not be scanned — CI must
treat this as a failure"*, exit 2. Nothing had named it until Phase 7's
completion gates ran, because no workflow calls `gts-docs` on this branch (see
§5 and finding Z9-3 on the missing UI gate).

*Why an exclude rather than editing `tsconfig.json`.* tsconfig is **JSONC by
specification** — TypeScript's own format permits comments — so the parse
failure is a false positive by nature, not a malformed file; stripping the
comments would leave the next gear with a Vite UI to rediscover it. A GTS
*documentation* validator also has no business parsing a TypeScript build
config: no tsconfig can contain a GTS id, so nothing is lost by not scanning
one.

*What the risk actually is.* Any `tsconfig*.json` anywhere under `docs/`,
`gears/`, `libs/` or `examples/` is no longer scanned for GTS ids — today that
is two files, both qa-platform's. Measured after the change: 839 files scanned,
0 failed, 0 errors, exit 0 (it was 840 scanned / 1 failed / exit 2 before).
Reverting is deleting one line, at the cost of putting the gate back to red.

### 4.2 `.gitignore`

Three additions:

1. `!**/src/**/logs/` — a **fix to a pre-existing bug**, not a qa-platform
   concern. The tree's unanchored `logs` pattern silently swallowed any Rust
   module directory named `logs`. `qa-runs/src/infra/logs` was one, and was
   untracked until `git status` was actually read. **This one should be kept even
   if everything else is reverted** — it affects any gear that ever adds a `logs`
   module.
2. `gears/qa-platform/qa-platform-ui/{node_modules,dist}/` — already covered by
   existing unanchored patterns; listed explicitly. Safe to drop.
3. `gears/qa-platform/deploy/remote/*kubeconfig*.yaml` — prevents committing a
   client private key. qa-platform-specific; drop it with the directory.
4. `gears/qa-platform/docs/Reviews/qa-platform-review-findings.md` — a
   single-file rule, added later than the three above and **not** the caveat
   row 10 refers to (that is item 1, the `logs` un-ignore, which is a fix worth
   keeping). Its effect is worth stating plainly: the 55-finding review this
   subsystem's remediation branch exists to answer is **invisible to the
   repository** — it lives only in a working copy, and so does every "closed
   by" annotation added as findings were fixed. The rule's own comment explains
   the intent (it was swept in by a `git add -A`, and deleting it would let the
   next one put it back) and says to remove the line if `docs/Reviews/` ever
   becomes tracked. Durable records therefore go elsewhere: §12 of
   `docs/superpowers/specs/2026-09-05-review-remediation-design.md` carries the
   Phase 7 follow-up, and this file carries the tooling change above.

---

## 5. The one change that affects other people's builds

### `.github/workflows/ci.yml`

```yaml
+      - name: Test qa-runs with Postgres (integration)
+        run: make test-qa-runs-pg
```

**This is unconditional.** It is not gated on a path filter, a feature or a
label, so **every CI run in the repository now runs qa-runs' Postgres integration
suite**, including PRs that touch nothing in `gears/qa-platform`.

**Consequences for others:** added wall-clock on every build, and a qa-platform
regression turns unrelated PRs red.

**Revertible?** Trivially — delete the three lines. The suite still runs locally
via `make test-qa-runs-pg`.

**Recommendation.** This is the single change most likely to annoy the rest of
the team, and the cheapest to remove or gate behind a `paths:` filter.

---

## 6. Was it reverted to the pre-commit state, and what did it cost?

**Yes, for the two items that mattered.** Both changes to `gears/system/` in
section 2 are reverted as of 2026-09-01. This section originally argued, in
the future tense, about whether that was possible and what each item would
cost; it now records what was actually done, so the reasoning behind the
choices made is kept rather than deleted.

**Free — no functional loss, and done regardless of the revert:**

* `.github/workflows/ci.yml` — left as-is; still unconditional, still the item
  most likely to annoy the rest of the team (see section 5). Out of scope for
  this revert — the user marked it acceptable.
* `docs/GEARS.md`, `Makefile`, the two cosmetic `.gitignore` lines — untouched,
  out of scope.
* The `!**/src/**/logs/` fix — kept; it is an unrelated bug fix.

**Free but couples to a decision — left as originally weighed:**

* `postgres-credstore-plugin` — kept as general infrastructure. Out of scope
  for this revert.
* `oidc-authn` wiring — kept. Out of scope for this revert.

**What `event-broker/src/config.rs` actually cost.** The precondition this
section once named — qa-insights dropping its `event_broker` dependency — is
what happened. qa-insights' event consumer and both broker dependencies (the
`event-broker` gear crate and `event-broker-sdk`) were deleted; qa-runs'
publisher, payloads and port were deleted too, since they fed the same dead
path. With nothing left depending on it, the config file reverted cleanly to
byte-identical upstream. The one exception is deliberate, not an oversight:
`qa-insights/src/infra/storage/migrations/m20260818_000002_offset_store.rs`
stays, because the table it creates already exists on deployed databases and
the migration runner has recorded it as applied — see section 2.2.

**What `static-authz-plugin` actually cost.** The alternative this section
used to recommend — re-homing the grant logic as a **separate plugin inside
`gears/qa-platform/`** — was *not* what was built, because the authz-resolver
selects exactly one plugin by vendor with no chaining: a second plugin would
have **replaced** `static-authz-plugin`, not supplemented it, which the
original argument here did not account for. Instead, each of the three gears
that ran nil-tenant tickers gained its own `domain::elevated::enumeration_scope()`
seam, feeding nine nil-tenant `system_actor` factories through eight call
sites (five in qa-runs for six factories, one in qa-insights, two in
qa-catalog), matching a pattern `account-management` already used upstream.
This is more code across more files than the single-plugin alternative would
have been, but it means no shared system gear carries any qa-platform-specific
surface — config, plugin, or otherwise.

**Unavoidable while the code lives in this workspace — unchanged, and still
true:**

* Root `Cargo.toml` members, `Cargo.lock`, the example-server features/deps and
  `registered_gears.rs` imports. The only way to zero these is to move
  qa-platform to a **separate repository/workspace** that depends on the gears
  toolkit as a versioned dependency rather than a path. This revert did not
  attempt that, and it was never in scope.

---

## 7. Verification

Every claim above was measured, not inferred:

```bash
# The complete list of paths outside gears/qa-platform, current tree (31)
git diff --name-status db7660030 HEAD -- . ':(exclude)gears/qa-platform'

# Default build compiles none of it
grep -A1 '^\[features\]' apps/cf-gears-example-server/Cargo.toml   # default = []

# The two system gears are byte-for-byte upstream again
git diff --stat db7660030 -- gears/system/authz-resolver gears/system/event-broker
git status --short gears/system/
```

## 8. Recommendation

**Both items this section used to flag are done.** `static-authz-plugin` and
`event-broker/src/config.rs` are reverted to upstream; the capability each one
provided now lives inside `gears/qa-platform/` (section 2). What's left:

1. **`ci.yml`** — still unconditional, still the one most visible to
   colleagues (section 5). Remove or gate behind a `paths:` filter. Explicitly
   out of scope for the revert this document otherwise records.
2. **The dev-stand check** — the code-level work is done and locally verified
   (section 2's verification note), but the live behavioural proof — a run
   ingested automatically by the reconcile sweep with no authz denial in the
   log, on the deployed stand — has not been run. It is blocked on a
   permission gate, not on anything above.

Everything else is either new files, mandatory Cargo registration, or tooling
that can be dropped in minutes, and none of it was in scope for this revert.
