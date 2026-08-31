# qa-platform: legacy parity for branch selection, cross-repo plans, and products

**Date**: 2026-08-12
**Status**: approved (design)
**Scope**: `qa-catalog` (shipped, reviewed — reopened) + PRD/DESIGN/DECOMPOSITION edits are
executable now. `qa-runs` behavior (§3.4) is specified here as an inherited constraint but is not
buildable until that gear is scaffolded — see the sequencing note in §3.4.

## 1. Context and problem

Two gaps were recorded against `qa-catalog` as blockers for building `qa-runs`
(`DECOMPOSITION.md:121-122`): cross-repo custom plans cannot be bundled, and there is no
product-version→branch lookup on the SDK. The recorded options were "work around it in qa-runs" or
"amend the qa-catalog SDK".

Both framings turned out to be wrong once checked against the source system
(`../testrunner`, the legacy VHP test runner). The goal of this design is explicit:
**make the gears behave the way the legacy system behaves.** Where the recorded gaps describe
behavior legacy does not have, they are deleted rather than built.

Three findings drive everything below.

### Finding 1 — the real blocker is the single-branch working copy, not either recorded gap

`qa-catalog` p1 materializes only a repository's `default_branch`; any other branch returns
`RepoNotSynced`, and `default_branch` is immutable after registration (`DECOMPOSITION.md:112`).

Legacy is branch-first by design. Branch selection *is* the legacy run model after VHP-319, and it
is backed by a real multi-branch cache in `manager/src/services/test_repos.rs`:

| Legacy capability | Location |
| --- | --- |
| Per-branch checkout at `{repo_id}/branches/{branch}` | `test_repos.rs:513-517` |
| One host clone owns objects/refs; each branch is a `git worktree` on top (falls back to clone-per-branch) | `test_repos.rs:462-471` |
| In-memory freshness TTL cache keyed `(repo_id, branch)`; `force_sync` evicts | `test_repos.rs:57-70`, `:487` |
| Two-tier locking: per-`(repo_id, branch)` and per-repo | `test_repos.rs:543-558` |
| Arbitrary-branch sync on demand | `test_repos.rs:418` |

This blocks the cross-repo gap transitively: legacy requires multi-repo plans to pin an explicit
branch (`routes/custom_plans.rs:673-687`), and today's `qa-catalog` cannot serve a non-default
branch at all. No qa-runs-side workaround can reach past that. `DECOMPOSITION.md:123` already
half-records this, calling the version mapping "shipped but unusable for its stated purpose beyond
the default branch".

### Finding 2 — legacy builds N single-source bundles, not one multi-source bundle

`DECOMPOSITION.md:121` offers a false dichotomy ("a multi-source `BundleRequest`, or an enforced
single-repo constraint"). Legacy takes a third path that neither option names:

- `RepoRunConfig.bundle_url` is a single `String`, not a list (`services/argo.rs:64-66`)
- `build_repo_run_config(state, repo_id, branch, repo_tests, …)` is invoked **once per repo**,
  scoped to that repo's tests (`routes/custom_plans.rs:931-937`)
- each result becomes one `DagNodeSpec { bundle_url: Some(…), test_version: … }`
  (`models.rs:496-504`, populated at `routes/custom_plans.rs:752-763`)

So a launch spanning N repositories produces **N single-source bundles and N execution nodes**.
`qa-catalog`'s existing single-source `BundleRequest` is already the correct shape. **No SDK
change, no `BundleRequest` change, no `create_bundle` change.** The work is entirely in qa-runs'
launch path.

### Finding 3 — product-version→branch mapping was deliberately deleted from legacy

`manager/migrations/001_initial.sql:230-234`:

```sql
-- VHP-319: drop "Platform Versions" concept. Runs are now driven by branch
-- selection (per-platform default + run-time override), not by a curated
-- product_versions table.
ALTER TABLE platforms_meta ADD COLUMN IF NOT EXISTS default_branch TEXT;
DROP TABLE IF EXISTS product_versions;
```

The dropped table was `(product_id, version, test_repo_id, test_branch)` — precisely the
`find_version(product_id, version)` lookup the recorded gap asks for, and precisely the shape of
`ProductVersion.repo_branches: Vec<(Uuid, String)>` in the SDK today.

Two corroborations:

- Legacy sets `test_version = Some(branch.clone())` (`routes/custom_plans.rs:960`). The mapping
  direction is **branch → version**, the inverse of what the PRD asks for. The branch label *is*
  the recorded test version.
- Legacy's remaining version data is purely *observed*: `api_list_observed_versions` reads distinct
  `app_version` from `run_results` and its doc comment states "With the legacy `product_versions`
  mapping removed, this endpoint is the analytics UI's only source" (`routes/products.rs:275-313`).

Therefore `PRD.md:274`'s rationale — "version-to-branch mapping is how the current tool pins
compatible tests" — is factually false about the current tool. It describes a removed feature.

A related claim also failed verification: there is no TEST_META version applicability in legacy.
`TestFileInfo.versions` is populated as `Vec::new()` on every catalog path
(`services/plans.rs:581`, `:717`, `:1110`, `:1134`; `services/collect.rs:102`;
`routes/tests.rs:143`) — a dead field. Legacy TEST_META parses exactly `title`, `tags`,
`exclusive`, `bugs`, which `qa-catalog` already matches 1:1. Version awareness in legacy lives
only in analytics, derived from run history — a `qa-insights` concern, not a catalog one.

## 2. Decisions

| # | Decision | Rationale |
| --- | --- | --- |
| D1 | Multi-branch working copies in `qa-catalog`, shared object store (one clone per repo, per-branch worktrees) | Legacy's model; one fetch serves N branches |
| D2 | Per-branch freshness state is an **in-memory TTL cache**, no table, no migration | Legacy's model exactly; the launch path force-syncs past it, so persistence buys nothing for launches |
| D3 | `BundleRequest` stays single-source; qa-runs issues N calls, one per repo group | Finding 2 |
| D4 | Remove `ProductVersion` entity, `product_versions` table, and its REST/SDK surface | Finding 3 |
| D5 | Amend `fr-catalog-products` to drop the version→branch clause and its false rationale | Finding 3 |
| D6 | Add `product_id` to `TestRepository`, **required** (non-optional) | Legacy `TestRepository.product_id: String` (`models.rs:594`); repo ownership is the primary plan→product attribution path |
| D7 | Add `Product.key` and `Product.description` | `key` is the durable analytics join key denormalized into `run_results.product_key`; UUIDs never appear there |
| D8 | Do **not** add `tests_folder` | It exists only to attribute *local* plans baked into the runner image. The new design is repo-backed only (`PRD.md:78`, `:281`) |
| D9 | Keep `qa-catalog`'s existing `Product.folder` | A UI grouping label legacy lacks, but additive and required by PRD's "product folders" clause plus its shipped `/qa/v1/product-folders` route |

### Rejected alternatives

- **Multi-source `BundleRequest`** — not legacy's shape (Finding 2); would add SDK surface for
  behavior legacy achieves with N calls.
- **Enforce a single-repo constraint on custom plans** — regresses a shipped, bug-fixed legacy
  feature. `routes/custom_plans.rs:647-651` records the production failure it fixed: mixing
  Backend + UI "used to bail out here (`repo_ids.len() != 1` → no TEST_BUNDLE_URL), leaving every
  pod on the empty image `/test_plans` — which is exactly the 'Test file not found' /
  'no playwright.config.ts' failure mode."
- **DB-tracked per-branch sync state** (`qa_repo_branch_syncs`) — durable across restarts and
  correct across replicas, and would give qa-runs a per-branch `head_commit` for bundle provenance.
  Rejected as a deviation from legacy; revisit if multi-replica `qa-catalog` or bundle provenance
  becomes a requirement.
- **Clone-per-branch instead of a shared object store** — simpler, avoids shared-`.git` locking,
  but pays full network + disk per branch, the cost legacy's worktree model exists to avoid.
  Retained as a **fallback**: the directory layout, cache, and all domain code are identical, so
  it is a drop-in retreat if materializing multiple worktrees from one `gix` clone proves
  impractical.
- **Re-sync one working copy per request** — two concurrent launches on different branches thrash
  one directory, and a bundle build can observe another launch's checkout mid-read. Unsafe under
  the concurrent multi-repo bundle builds this design exists to enable.

## 3. Design

### 3.1 `qa-catalog` — multi-branch working copies

**Directory layout**

```
<repos_dir>/<repo_id>/git                     # one clone: objects + refs
<repos_dir>/<repo_id>/branches/<branch_dir>   # per-branch worktree
```

`branch_dir` follows legacy's `branch_dir_name` (`test_repos.rs:1340-1366`): lowercase,
non-ASCII-alphanumeric → `-`, trim `-`, **plus an 8-character digest suffix**. The suffix is
load-bearing: normalization is lossy (`release/5.0` → `release-5-0`), so the digest is what keeps
`release/5.0` and `release-5-0` from colliding on disk.

**Locking — two tiers, matching legacy**

- per-`(repo_id, branch)`, key `{repo_id}::{branch}` — serializes syncs of the same branch
- per-repo, key `{repo_id}::*repo*` (`test_repos.rs:552-558`) — serializes *all* on-disk git
  mutations for a repo, because every branch worktree shares one host `.git` and concurrent
  fetch/checkout races on index and ref locks

The per-repo tier is not optional here: this design builds N bundles concurrently, which is exactly
the condition that races the shared object store.

**Freshness**

In-memory cache keyed `(repo_id, branch)` with a TTL, plus `force_sync` semantics that evict the
entry and sync unconditionally. Per D2 the launch path always force-syncs, so the cache serves only
browse/list reads (plan discovery, test listing).

**Port and infra**

`RepoSyncPort` is unchanged — it already takes both `branch` and `workdir`
(`domain/ports/repo_sync.rs:36-42`). `infra/git/gix_sync.rs` gains a "fetch into the host clone,
then materialize the branch tree into a target directory" path, reusing the existing
`checkout_worktree` (`gix_sync.rs:274-277`), which already writes an arbitrary commit's tree into a
chosen directory.

**Domain changes**

- `require_synced(repo, branch)` (`domain/service/plans.rs:293-304`) — drop the
  `branch == repo.default_branch` clause. New predicate: **the branch's worktree directory exists
  on disk, and the repository-level `sync_error` column is null.** Note this deliberately keeps
  `sync_error` at repository granularity: D2 rules out a per-branch state store, so there is
  nowhere to record a per-branch failure. The consequence is accepted and explicit — a failed sync
  of branch B marks the *repository* as errored, so reads of an otherwise-healthy branch A also
  fail closed until the next successful sync. Legacy has the same coarseness (its per-branch cache
  records freshness only, never errors). Fixing it requires the rejected per-branch table.
- `content_root_dir(repos_dir, repo, branch)` (`plans.rs:311-320`) — resolve under the branch
  directory. The signature already receives `branch` and currently discards it; containment
  canonicalization is retained.
- `default_branch` becomes **mutable**. The immutability rationale
  (`domain/service/repos.rs:171-181`) rests entirely on the single-branch model and its own text
  says "Revisit with the multi-branch cache."
- `url` / `content_root` change still invalidates the working copy, now across all branches.
- Repository delete still removes `<repos_dir>/<repo_id>` wholesale.

### 3.2 `qa-catalog` — product parity

- `TestRepository` gains `product_id: Uuid`, required, on create and update DTOs, the entity, the
  mapper, and the migration.
- `Product` gains `key: String` and `description: String`.
- `Product.folder` is retained unchanged (D9).
- Plan discovery attributes plans to products by repo ownership and stamps `product_key` /
  `product_name`, mirroring legacy's `enrich_plan_products` (`services/plans.rs:822-845`) minus its
  `tests_folder` branch.

`folder` and legacy's `tests_folder` are **unrelated concepts with confusingly similar names** —
one groups products in a UI, the other attributes test content to a product. This is recorded in
DESIGN so the two are not re-conflated.

### 3.3 `qa-catalog` — `ProductVersion` removal

Remove the entity, table, and surface across: `domain/service/products.rs`,
`domain/service/products_tests.rs`, `domain/repos/products_repo.rs`, `domain/error.rs`,
`domain/local_client/client.rs`, `domain/service/repos.rs`, `api/rest/{dto,error}.rs`,
`api/rest/routes/products.rs`, `api/rest/handlers/products.rs`,
`infra/storage/{products_sea_repo,mapper}.rs`, `infra/storage/entity/product_version.rs`,
`infra/storage/migrations/m20260812_000002_initial.rs`, and
`qa-catalog-sdk/src/{client,models,lib}.rs`.

The table lives in the **initial** migration, so this is an in-place edit with no migration chain
and no deployed data to migrate. If a deployed environment has already run that migration, this
becomes an additive drop-migration instead — a documented assumption, not a verified fact.

### 3.4 `qa-runs` — launch path (constraint, not work in this plan)

**Sequencing note.** `qa-runs` does not exist yet. This section is **not** implementation work for
the plan derived from this spec — it is the contract that plan must leave buildable, and the
specification the future qa-runs plan inherits. Everything in §3.1-§3.3 and §5 is executable now;
§3.4 is executable only once qa-runs is scaffolded. An implementation plan that tries to build both
has mis-read this boundary.

1. **Resolve branch**: explicit request → platform `default_branch` → repo `default_branch`
   (legacy: `routes/custom_plans.rs:955-959`; platform default at `models.rs:214-216`).
2. **Group** `CustomPlan.files: Vec<(Uuid, String)>` by `repo_id`.
3. **Guard**: more than one group and no explicit branch → `FailedPrecondition`, naming the
   constraint (legacy returns 400 at `routes/custom_plans.rs:673-687`).
4. **Per group, concurrently** (legacy uses `try_join_all` at `routes/custom_plans.rs:696-712`):
   force-sync the branch, then `create_bundle(BundleRequest { repo_id, branch, files })`.
5. **Emit one execution node per group**, each carrying its own bundle reference. A single group
   yields a single node with no synthetic DAG.
6. **Record `test_version` as the branch label** (legacy: `routes/custom_plans.rs:960`).

### 3.5 Error handling

Legacy's launch-time failure modes are preserved, mapped onto gear canonical errors:

| Condition | Legacy | Gear |
| --- | --- | --- |
| Repository not found | 400 | `NotFound` |
| Branch sync failure | 400 naming repo + branch | `FailedPrecondition` (caller-fixable input) |
| Plan missing on chosen branch | 400 listing missing plans, advising a branch where all exist (`custom_plans.rs:636-644`) | `FailedPrecondition`, same message content |
| Unpinned multi-repo plan | 400 | `FailedPrecondition` |
| Sync-engine failure | — | `SyncFailed` → 503, no detail leak (existing mapping) |

## 4. Testing

- **Multi-branch resolution**: integration test over a real local git repo with two branches — sync
  both, assert each worktree holds its own branch's content at the expected `head_commit`, and
  assert a non-default branch resolves (the case that returns `RepoNotSynced` today).
- **Branch dir collisions**: `release/5.0` and `release-5-0` map to distinct directories.
- **Concurrency**: N concurrent syncs of different branches of one repository complete without
  corruption. This is the test that fails without the per-repo lock tier.
- **Product**: repository registration without `product_id` is rejected; discovered plans carry
  `product_key`.
- **Grouping** *(qa-runs, deferred per §3.4)*: a two-repo custom plan yields two bundles and two
  execution nodes, each bundle containing only its own repo's files; a one-repo plan yields one node
  and no DAG.
- **Guard** *(qa-runs, deferred per §3.4)*: unpinned multi-repo plan → `FailedPrecondition`.
- **Removal**: `ProductVersion` is absent from the SDK, the REST surface, and the schema.
- **Regression**: existing `qa-catalog` suites continue to pass; `require_synced` and
  `content_root_dir` call sites are covered by the tests that currently assert the single-branch
  behavior, which must be rewritten rather than deleted.

## 5. Document and requirement changes

| File | Change |
| --- | --- |
| `PRD.md:272-274` | Drop the version→branch clause from `fr-catalog-products`; remove the false rationale. Requirement becomes products + folders. |
| `PRD.md:212` | `fr-catalog-branch-cache` justifies itself partly by "product version branch mappings" — reword to launch-time branch selection only. |
| `DESIGN.md:221` | Remove `ProductVersion` and "version→branch mappings" from the entity table. |
| `DESIGN.md:537` | Drop `product_versions` from the schema list; add `product_id` to `test_repositories` and `key`/`description` to `products`. |
| `DESIGN.md:560` | The tenant-prefixed-index rule cites `product_versions` as an example; reassign to `repo_branches` so the rule survives the deletion. |
| `DESIGN.md` (new note) | Record the multi-branch layout, the two-tier lock, and the `folder` vs `tests_folder` distinction. |
| `DECOMPOSITION.md:102`, `:106` | Remove `ProductVersion` from entities and `product_versions` from data. |
| `DECOMPOSITION.md:112`, `:121` | Resolved — built by this design. |
| `DECOMPOSITION.md:122`, `:123` | Deleted as non-requirements (Finding 3). |
| `docs/plans/2026-08-12-qa-platform-continuation-prompt.md:169-179` | Rewrite the gap register; qa-runs' launch path changes shape (branch-first resolution, N bundles per launch). |

## 6. Risks and open items

- **`gix` worktree materialization**: whether multiple worktrees can be materialized from one `gix`
  clone is unproven. `checkout_worktree` already writes an arbitrary tree to a directory, so the
  primitive exists, but the host-clone/worktree split is not yet exercised. Mitigation: the
  clone-per-branch fallback needs no change to layout, cache, or domain code.
- **`qa-catalog` re-review**: this reopens a shipped, reviewed gear. The `ProductVersion` removal
  alone spans 17 files.
- **Initial-migration assumption**: §3.3 assumes no deployed environment has run
  `m20260812_000002_initial.rs`. Unverified.
- **Disk growth**: per-branch worktrees grow with the number of branches touched. Legacy has no
  eviction beyond repository delete; this design inherits that. Not a parity gap, but a known
  operational edge.
- **Deferred, unrelated to this design**: notification parity (legacy's Slack/email in
  `services/notifications.rs`) depends on both a real event-broker adapter and `qa-insights`, and
  is tracked separately.
