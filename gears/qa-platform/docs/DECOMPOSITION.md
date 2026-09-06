# Decomposition: QA Platform

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-qa-status-subsystem`

## Table of Contents

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Environments Registry - HIGH](#21-environments-registry---high)
  - [2.2 Test Catalog - HIGH](#22-test-catalog---high)
  - [2.3 Run Orchestration Core - HIGH](#23-run-orchestration-core---high)
  - [2.4 Schedules - HIGH](#24-schedules---high)
  - [2.5 Insights Foundation - MEDIUM](#25-insights-foundation---medium)
  - [2.6 UI Gear & SPA Adaptation - MEDIUM](#26-ui-gear--spa-adaptation---medium)
  - [2.7 Serverless Execution & Runner Migration - HIGH (blocked)](#27-serverless-execution--runner-migration---high-blocked)
  - [2.8 JIRA Loop & Notifications - MEDIUM](#28-jira-loop--notifications---medium)
  - [2.9 Data Migration - MEDIUM](#29-data-migration---medium)
  - [2.10 Deployment Packaging - MEDIUM](#210-deployment-packaging---medium)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

The DESIGN decomposes into nine features ordered by dependency readiness (PRD §10): everything except feature 2.7 builds against today's platform, with run execution mocked behind the `RunExecutor` port. Feature 2.7 (real execution + runner migration) is the only slice blocked on serverless-runtime Python workloads; it drops into a frozen contract.

Strategy: foundations first (environments, catalog — no intra-subsystem dependencies), then the orchestration core against the mock executor with the ported semantics test suite as its gate, then reconciler-driven insights and the UI, then the blocked execution slice, integrations, and migration tooling last.

## 2. Entries

### 2.1 Environments Registry - HIGH

- [x] `p1` - **ID**: `cpt-cf-qa-feature-environments`

- **Purpose**: Stand up qa-environments: target environment registry with credstore-referenced kubeconfigs, environment/pipeline variables, lease state, version polling. Foundation for every launch decision.

- **Depends On**: None (platform gears only: credstore, cluster-sdk)

- **Scope**:
  - Gear + SDK crates, schema/migrations, REST CRUD, lease acquire/release SDK operations
  - Version poller scaffolding (leader-elected task, `qa.platform.version_changed` event schema); actual probing lands with `cpt-cf-qa-feature-execution` (probes run via the execution plane)
    **Not built**: the version poller and the `qa.platform.version_changed` schema were claimed by this feature's scope but never implemented — the gear declares no `stateful` capability and `observed_version` has no writer. **`observed_build` has no writer either** (column and SDK field added 2026-08-13 by user decision, qa-runs plan Task 9b, because `qa_runs.app_build` had no upstream source and `APP_BUILD` would otherwise reach every test empty — PRD:577). The two columns are populated by the same probe and must be retrofitted together: one poller, both fields, one event. Retrofit when 2.7 lands.

- **Out of scope**:
  - Queue decisions (qa-runs); any direct target-cluster access from the control plane

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-qa-fr-env-platforms`
  - [x] `p1` - `cpt-cf-qa-fr-env-variables`
  - [x] `p1` - `cpt-cf-qa-fr-env-lease`
  - [ ] `p2` - `cpt-cf-qa-fr-env-version-poll` (completed by `cpt-cf-qa-feature-execution`)

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-qa-constraint-no-kube`
  - [ ] `p1` - `cpt-cf-qa-constraint-platform-delegation`

- **Domain Model Entities**: Environment, EnvironmentVariable, PipelineVariable, environment lease

- **Design Components**:

  - [x] `p1` - `cpt-cf-qa-component-environments`

- **API**: `/qa/v1/environments*` (CRUD, variables, lease view)

- **Data**: `environments`, `environment_variables`, `pipeline_variables`, `environment_leases`

### 2.2 Test Catalog - HIGH

- [x] `p1` - **ID**: `cpt-cf-qa-feature-catalog`

- **Purpose**: Stand up qa-catalog: repositories with credentialed sync, branch cache, plan discovery, TEST_META parsing, custom plans, products/versions, bundles, SSH keys. Everything launches need to know about content.

- **Depends On**: None (platform gears only: credstore, authz-resolver — the shipped gear declares `deps = [authz_resolver, credstore]`). **file-storage is a pending convergence, not a dependency** (see the convergence notes below); oagw is not used by this gear at all (`cpt-cf-qa-adr-git-egress`).

- **Scope**:
  - Gear + SDK crates, schema/migrations, REST CRUD
  - `plan.yaml` and TEST_META parsers with the preserved text-only, any-occurrence-wins semantics
  - Bundle build/serve/GC (blob storage behind the `BundleStore` port — gear-local filesystem in p1, file-storage on convergence)

- **Out of scope**:
  - Exclusivity precedence resolution (qa-runs applies it; catalog only parses values)

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-qa-fr-catalog-repos`
  - [x] `p1` - `cpt-cf-qa-fr-catalog-branch-cache`
  - [x] `p1` - `cpt-cf-qa-fr-catalog-ssh-keys`
  - [x] `p1` - `cpt-cf-qa-fr-catalog-plan-discovery`
  - [x] `p1` - `cpt-cf-qa-fr-catalog-test-meta`
  - [ ] `p1` - `cpt-cf-qa-fr-catalog-custom-plans` (launch clause pending `cpt-cf-qa-feature-runs-core` — PRD amendment 2026-08-13)
  - [x] `p1` - `cpt-cf-qa-fr-catalog-products`
  - [x] `p1` - `cpt-cf-qa-fr-catalog-bundles`

- **Design Components**:

  - [x] `p1` - `cpt-cf-qa-component-catalog`

- **Domain Model Entities**: TestRepository, Plan, TestFileMeta, CustomPlan, Product, TestBundle, SshKey

- **API**: `/qa/v1/plans`, `/qa/v1/custom-plans*`, `/qa/v1/test-repos*`, `/qa/v1/test-bundles/{id}`, `/qa/v1/products*`, `/qa/v1/product-folders`, `/qa/v1/ssh-keys*`

- **Data**: `test_repositories` (carries a required `product_id` — every repository belongs to a product), `repo_branches`, `ssh_keys`, `custom_plans`, `products`, `test_bundles` (physically `qa_`-prefixed; see DESIGN §3.7 for the reconciled column set)

- **Interim mechanisms & convergence notes** (as shipped 2026-08-12; each names where it lives so the interim is findable):

  - **Bundle blobs are stored on the gear-local filesystem** behind the `BundleStore` domain port (`qa-catalog/src/domain/ports/bundle_store.rs`, `infra/bundle_store/local_fs.rs`), because `FileStorageClientV1` is a placeholder trait with no P1 operations. **Convergence**: swap in a file-storage adapter when those operations land — a one-adapter change with no domain impact, and the follow-up the implementation plan promised to record here. Until then `cpt-cf-qa-fr-catalog-bundles`' "stored via the platform file-storage capability" clause is met by an interim in-gear mechanism, not by file-storage.
  - **OData filtering and paging are deferred on every catalog collection** (`bundles.rs` module docs record the rationale: operator-sized collections, and paging matters at the qa-runs/analytics surfaces). This re-scopes `cpt-cf-qa-nfr-scale`'s allocation — see the qa-catalog carve-out in DESIGN §1.2.
  - **Multi-branch working copies (shipped 2026-08-12, replacing the p1 single-branch interim).** One `gix` clone per repository at `<repos_dir>/<repo_id>/git` owns objects and refs; each requested branch is materialized as a plain content directory (no `.git`) at `<repos_dir>/<repo_id>/branches/<branch_dir>` (`infra/git/layout.rs` — the directory name appends an 8-character digest because normalization is lossy). Two lock tiers guard the shared object store (per-repo for every git mutation, per-`(repo, branch)` to collapse duplicate syncs, always taken repo-then-branch); freshness is an in-memory TTL cache (`branch_freshness_ttl_seconds`, default 300, `0` disables) that launches force-sync past. `default_branch` is mutable — it selects which branch content reads use when none is named, and identifies nothing. Still true: changing `url` or `content_root` clears the synced state so content reads reject until the next sync rather than serving content from the old source. See DESIGN §3.7 "Multi-branch working copies" for the full model, and the `sync_error` coarseness follow-up below.
  - **Git authentication is HTTPS-token or SSH-key**, selected by the remote's URL scheme. SSH was deferred in p1 on the grounds that gix shells out to a system `ssh` with no in-process key injection, "so SSH support would mean writing key material to disk"; **that premise was retired on 2026-08-27** by loading the key into a short-lived per-sync `ssh-agent` over stdin (`cpt-cf-qa-adr-git-egress`, "Amendment: SSH remotes are supported"). The asymmetry noted here — `cpt-cf-qa-fr-catalog-ssh-keys` shipping the SSH-key *entity* while SSH was the one unusable auth mode — is resolved: the entity is now what the ssh sync path resolves its key from.
  - **The stored SSH-key fingerprint is `SHA256:<sha256 of the PEM bytes>`**, not the OpenSSH public-key fingerprint (`domain/service/ssh_keys.rs`, `fingerprint`). It is a change-detection/display value only and will not match `ssh-keygen -lf` output; anything that compares fingerprints across tools needs the real OpenSSH computation first.
  - **`get_plan`, `get_test_meta`, and `create_bundle` are SDK-only** with no REST route — launch internals for qa-runs, and keeping `create_bundle` off REST is what bounds the in-memory bundle build (DESIGN §3.3).

- **Tracked follow-ups** (real gaps against PRD §5.1 and adjacent requirements, deliberately not built — recorded so they are not rediscovered as bugs):

  - **Periodic repository *content* sync is not implemented.** `cpt-cf-qa-fr-catalog-repos` requires sync "on demand and on a configurable interval"; only the *branch cache* refreshes on an interval (`branch_refresh_interval_seconds`). Content sync happens solely on `POST /qa/v1/test-repos/{id}/sync`, so a freshly registered repository's working copy goes stale immediately and plan discovery keeps reading the stale copy. Needs a design decision, not just a loop: a cadence knob, and how a background content sync interacts with concurrent on-demand syncs and with in-flight discovery/bundle reads of the same working copy.
  - **Repository access tokens have no managed entity.** `cpt-cf-qa-fr-catalog-ssh-keys` covers "named SSH keys **and repository access tokens**". Only SSH PEMs get one; a repository's token is a free-form client-supplied credstore reference the operator must provision out-of-band. Both auth modes work as of 2026-08-27, so the gap is narrower than it was — but the token half is still the credential type the gear should manage and does not.
  - **Resolved (2026-08-12) — cross-repo custom plans are served by N single-source bundles.** The qa-runs launch path groups `CustomPlan.files` by repository and creates one bundle and one execution node per group, matching the source system's behavior; `BundleRequest` stayed single-source and needed no change. The grouping contract is the parity spec §3.4 (`docs/superpowers/specs/2026-08-12-qa-platform-legacy-parity-design.md`).
  - **Withdrawn (2026-08-12) — there is no product-version→branch requirement.** Two entries here recorded "no product-version→branch resolution on the SDK" and "the single-branch limit makes the version→branch mapping inert" as gaps. They were requirements that never existed: the source system deleted its curated version→branch table in VHP-319 in favour of branch selection (per-platform default plus a launch-time override), and this platform followed — the `product_versions` table and the `ProductVersion` entity were removed from the schema, SDK, and REST surface. Launch-time branch selection is specified in the parity spec §3.4.
  - **`Product` parity is partial, deliberately.** `key` (persisted as `product_key`; SDK `Product::key`) and `description` are carried; the source system's `tests_folder` is deliberately absent — it attributed *local* plans baked into the runner image, and the new design is repo-backed only, so repository ownership carries all attribution (DESIGN §3.7, "`folder` is not `tests_folder`"). And per-branch `sync_error` granularity is a known coarseness: the column is repository-scoped, so a failed sync of one branch fails reads of the other branches until the next success; the named fix is a persisted per-branch state table.
  - **SDK `sync_repo(ctx, id)` has no `branch` or `force` parameters.** The gear's service and REST layers support both (REST sync takes an optional `?branch=` and always force-syncs), but `QaCatalogClientV1::sync_repo` was left unchanged. qa-runs' launch path must force-sync a specific branch through the SDK before `create_bundle` (parity spec §3.4, step 4), so the trait must gain those parameters — or a params struct — when qa-runs lands.
  - **Reads do not serialize against snapshot rewrites.** The two lock tiers serialize *writers* only; plan discovery, `TEST_META` reads, and bundle packing (`domain/service/plans.rs`, `bundles.rs`) walk a branch snapshot with no lock, so a read overlapping that branch's re-sync can observe a mid-rewrite directory (`gix_sync.rs` clears and rewrites the snapshot in place) — a transient partial or empty result, not corruption. The source system had the same coarseness over its single working copy, so this is at-parity, recorded so it is not rediscovered as a bug. Named fix if it ever bites: a per-branch read lock (readers share, sync excludes), or generation-numbered snapshot directories with an atomic "current" pointer so readers finish on the generation they opened.

    **Verified at-parity 2026-08-13 (qa-runs plan, decision D4).** The source
    system has the same two-tier writer-only lock structure — `sync_locks`
    keyed `"{repo_id}::{branch}"` plus a per-repo lock keyed
    `"{repo_id}::*repo*"` (`manager/src/services/test_repos.rs:30, 543-558`),
    taken only by writers (`:405-406, 452-456`) — while its readers
    (`plans::find_plan_in_repository_root` and
    `test_bundles::create_bundle_from_checkout`, both called from
    `routes/runs.rs:662-722`) walk the checkout unlocked. So two concurrent
    launches on one `(repo, branch)` race there exactly as they do here, and
    neither generation-numbered snapshots nor a reader lock is a parity
    requirement — both would be new design. One honest difference in degree:
    the source system updates a branch directory via `git worktree`
    (`test_repos.rs:463-467`), rewriting in place, whereas `gix_sync.rs` clears
    before rewriting — so this gear additionally exposes a transient *empty*
    read where legacy exposes only a partial one. qa-runs closes just that
    widened part with one bounded retry when post-force-sync discovery comes
    back empty; the named fix for the residual (generation-numbered snapshot
    directories with an atomic pointer swap) stays recorded here, unbuilt.
  - **REST `PlanDto` omits `product_id` while `sdk::Plan` carries it.** Discovery attributes every plan to its repository's owning product, and the SDK model exposes it (qa-runs consumes it at launch), but the REST DTO (`api/rest/dto.rs`, `PlanDto`) does not — a REST consumer must join plan→repo→product itself. Intended for now: no REST consumer needs the field yet, and the UI lists plans per repository where the product is already in hand. Adding the field is a two-line DTO/schema change; do it the moment a REST surface (UI plan browser, cross-repo plan search) wants product attribution without the join.
  - **Products are the odd one out in the SDK client trait.** `create_repo`/`update_repo` and `create_custom_plan`/`update_custom_plan` all take a parameter struct (`NewTestRepository`, `TestRepositoryUpdate`, `NewCustomPlan`), but `create_product`/`update_product` take bare positional arguments — `update_product` carries `ctx, id, name, key, description, folder`, three of them adjacent `String`s. Transposing `name`, `key`, and `description` at a call site compiles silently. A `NewProduct` / `ProductUpdate` pair would match the trait's dominant idiom and close that footgun. Deliberately not done in the parity effort: it is a cross-cutting change through the SDK trait, local client, service, repository, and REST handlers, and bundling it in would have widened the diff without serving the parity goal. Surfaced by the Tasks 6-7 code-quality review.
  - **Exclusivity *tier* semantics must be reconciled in qa-runs.** The catalog's per-file flag is `Option<bool>` where the source system's per-file parser returned `bool`. This is outcome-neutral for the parallel-vs-exclusive decision, but not for the tier the source system records and annotates: with files read and none requesting exclusivity, legacy reports tier `TestMeta` with `exclusive = false` (`manager/src/services/exclusivity.rs`, `aggregate_test_meta` returns `Some(false)` for a non-empty flag set, and the source has an explicit test asserting the log must not read `default`), whereas a naive "`None` means no opinion" reading attributes the same run to tier `Default`. qa-runs must distinguish "no file was read" from "files were read and all said parallel". Flagged for `cpt-cf-qa-fr-runs-exclusivity`.
  - **The legacy tag-filter asymmetry must be ported deliberately, not reinvented.** `tags_admit` (`manager/src/services/test_meta.rs:81-108`) is intentionally asymmetric — an untagged file is admitted by an exclude-only filter and rejected by an include filter, fail-open for exclude and fail-closed for include — and the same predicate gates a file's vote in the exclusivity tier (`manager/src/services/exclusivity.rs:160-173`, `file_declares_exclusive` returns `None` for a filtered-out file). The catalog correctly stays out of it and returns enough data (tags plus the three-state flag); qa-runs owns the asymmetry and must implement it on purpose. Also ported behavior: all three tag inputs are trimmed and lowercased before comparison.
  - **Legacy fields not carried over** — an inventory, so each omission is a decision on record rather than an oversight:
    - `plan.yaml`: `description`, `node_selector`, `tolerations`, `validation` (the last being a plain bool OR'd with a case-insensitive `validation` *tag*, `manager/src/services/plans.rs`). Correction to an earlier assumption: `node_selector`/`tolerations` are **not** execution-plane scheduling inputs in the source system — they are parsed, stored, and returned by the API, `node_selector` is rendered read-only in one UI page and `tolerations` has no consumer at all, and neither reaches the Argo workflow spec (zero hits in `manager/src/services/argo.rs`). So there is no scheduling behavior to re-home; the only parity surface is the API/UI shape, which belongs to `cpt-cf-qa-fr-ui-parity`. `validation`, by contrast, does have a consumer — it stamps a `vhp-tests/validation` annotation on the run — so if anything is re-homed it is that, as a run classification in an execution/runs requirement.
    - TEST_META: `component`, `quality_vectors`, and the module-docstring-derived `description`. The PRD does not require them (no occurrence of `quality_vector` anywhere in these documents), so skipping them is permissible — but the legacy dashboard and analytics features are built on `quality_vectors` (`manager/src/routes/dashboard.rs:470` quality-vector pass-rate card; `manager/src/routes/analytics.rs:1985` `collect_quality_vectors_by_file`, a cached whole-catalog TEST_META scan invalidated on repo sync), so `cpt-cf-qa-fr-insights-dashboard` (p1) and `cpt-cf-qa-fr-ui-parity` (p2) may need them. Deciding this needs to happen before insights ships, because the fields are parsed in the catalog and insights reads the universe directly over qa-catalog-sdk rather than receiving them any other way.
    - The source system also has a *second*, overlapping plan reader (`manager/src/services/git_plans.rs`) that parses a DAG schema out of the same `plans/*.yaml` location. Not ported, and not required by any p1/p2 requirement — noted because both readers scan the same path permissively, so a repository can contain files that one reader accepts and the other silently mis-reads.
  - **The YAML engine swap is unverified for parity.** Legacy parses `plan.yaml` with `serde_yaml 0.9` (`0.9.34+deprecated`); the gear uses `serde-saphyr` because the workspace forbids `serde_yaml` as deprecated/unmaintained. No shared-fixture parity test exists for duplicate keys, YAML-1.1 booleans (`yes`/`on`/`off`), or unquoted-scalar coercion. That is a fidelity risk against `cpt-cf-qa-fr-migration-runner-contract`'s "plan.yaml format unchanged" claim, and the cheap mitigation is a fixture corpus checked into the gear's tests.
  - **Plan discovery has no cache.** `list_plans` re-walks the working copy and re-parses every plan file on each request (`domain/service/plans.rs`, `discover_plans`). At `cpt-cf-qa-nfr-scale`'s 5,000-test-file target that is repeated filesystem work per UI page load. The source system faced the same problem for its TEST_META scan and solved it with a TTL cache invalidated on repository sync (`manager/src/routes/analytics.rs:1976-2013`) — the same shape applies here, keyed on the synced revision.

### 2.3 Run Orchestration Core - HIGH

- [x] `p1` - **ID**: `cpt-cf-qa-feature-runs-core`

- **Purpose**: The subsystem's heart on a mock executor: launch validation, exclusivity resolution, per-platform FIFO queue, dispatcher with crash recovery, run state machine, env assembly, cancellation/re-run, timeout, SSE logs, incremental ingestion, ~~lifecycle events~~. Gated by the ported semantics test suite. **Corrected 2026-09-01: "lifecycle events" struck** — the publisher was dead code and was deleted; see the Scope bullet below.

- **Depends On**: `cpt-cf-qa-feature-environments`, `cpt-cf-qa-feature-catalog`

- **Scope**:
  - qa-runs gear + SDK, schema/migrations, `RunExecutor` port + mock adapter
  - Semantics test suite ported from the source system **before** queue/exclusivity implementation (`cpt-cf-qa-principle-semantics-parity`)
  - ~~Event publication via event-broker SDK (in-memory broker acceptable at
    this stage)~~ **Corrected 2026-09-01: built, then removed.** A publisher
    shipped and called the SDK, but `event-broker` registered no client in any
    deployment, so every call was a logged no-op; the publisher was deleted as
    dead code rather than left running against nothing. See the row below for
    what that means for `cpt-cf-qa-fr-runs-events`.
  - **Owned the subsystem's event vocabulary; the vocabulary is now unpublished
    rather than owned.** `cpt-cf-qa-interface-events` is p1 with no owner: 2.1
    claimed the `qa.platform.version_changed` schema in its scope and did not
    build it, and this feature's now-removed "event publication via
    event-broker SDK" line assumed the vocabulary already existed. qa-runs
    *defined* the whole vocabulary (it was the dominant publisher — every
    event in DESIGN §3.3's table but one) before its publisher was deleted;
    `qa.platform.version_changed` was never retrofitted into it, and there is
    now no producer left to retrofit it onto.

- **Out of scope**:
  - Real execution (2.7); schedules (2.4, separated to keep this slice reviewable)

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-qa-fr-runs-launch`
  - [x] `p1` - `cpt-cf-qa-fr-runs-params`
  - [x] `p1` - `cpt-cf-qa-fr-runs-env-assembly`
  - [x] `p1` - `cpt-cf-qa-fr-runs-exclusivity`
  - [x] `p1` - `cpt-cf-qa-fr-runs-queue`
  - [x] `p1` - `cpt-cf-qa-fr-runs-dispatch`
  - [x] `p1` - `cpt-cf-qa-fr-runs-cancel-rerun`
  - [x] `p1` - `cpt-cf-qa-fr-runs-timeout`
  - [ ] `p1` - `cpt-cf-qa-fr-runs-events` — **not built; regressed from "half built" on the system-gear revert that deleted the publisher.** The requirement is one MUST with two conjoined obligations: publish typed lifecycle events to the platform event capability, **"with schemas registered in the type system"**. Through the state this entry originally described, eight events were published and their ids were GTS-*spelled* `&'static str` constants — not GTS-*registered* — so the first obligation was met and the second was not (**half built**, unticked 2026-08-17 by the independent spec-coverage audit, having been ticked in error hours earlier). **Neither obligation is met now.** `event-broker` registered no client in any deployment, so every one of those publish calls was a logged no-op; the publisher was removed as dead code rather than left running against nothing, and nothing in this gear publishes a lifecycle event today. The registration gap's own analysis stands unchanged and moot: `cpt-cf-qa-interface-events` makes registration the defining property of the contract ("New schema versions registered in the type system; existing versions never mutated"), the platform's own `type_provisioning` is still a bare `todo!()`, and there is now no producer left to register in the first place. Retrofit needs both a producer and a working registration path.
  - [ ] `p1` - `cpt-cf-qa-interface-events` — **listed here 2026-08-17 because nothing tracked it anywhere.** 2.3's scope claims qa-runs "owns the subsystem's event vocabulary" and answers this ownerless interface, but the id never appeared in any Requirements Covered list, so its status was untracked by construction. Same state as the row above, for the same reason.
  - [ ] `p1` - `cpt-cf-qa-nfr-dispatch-latency` — **implemented, unmeasured; deliberately left unticked.** The 5 s cadence is shipped and its knob has a floor, but no benchmark, load test or observation of dispatch latency exists anywhere in the gear. This requirement is met by a design argument, and a design argument is not a verification method.
  - [ ] `p1` - `cpt-cf-qa-nfr-scale` — **implemented, unmeasured, and narrower than this entry first claimed.** OData and paging are shipped on the runs and queue collections and the scans are windowed, but nothing has been run at the target scale, and `GET /qa/v1/schedules` is unpaginated. **Corrected 2026-08-17 by the audit: the two paths the NFR's own numbers bite hardest are not collection endpoints at all, so no amount of paging reaches them.** `RunsRepository::list` has no `.limit()` and runs on **every launch** for name sequencing, materialising the tenant's whole run set on a write path; and `list_test_results` is an unbounded re-read issued once per ingested event, which at the stated 10,000 results per run is quadratic. DESIGN §1.2's "indexed queue and result tables" overstates the result table: `upsert_test_result` deletes on `(tenant_id, run_id, test_name, test_file)` while only `(tenant_id, run_id)` is indexed, and two declared indexes serve no repository query at all.

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-qa-principle-semantics-parity` — the four pure cores (`domain::exclusivity`, `domain::queue`, `domain::state_machine`, `domain::cron`) are ported from the source system with per-rule citations and exhaustive unit tests. **The scope line's "before implementation" ordering is evidenced by the plan's own test-first steps, not independently audited after the fact.**
  - [x] `p1` - `cpt-cf-qa-principle-db-first-state`
  - [x] `p1` - `cpt-cf-qa-principle-executor-port`

- **Domain Model Entities**: Run, QueueEntry, RunResult

- **Design Components**:

  - [x] `p1` - `cpt-cf-qa-component-runs`

- **API**: `POST /qa/v1/runs`, `/qa/v1/runs*`, `/qa/v1/runs/{id}/cancel|rerun`, `/qa/v1/runs/{id}/logs` (SSE), `/qa/v1/queue*`

- **Sequences**:

  - [x] `p1` - `cpt-cf-qa-seq-launch`

- **Data**: `runs`, `run_queue`

- **Tracked follow-ups** (real gaps, deliberately not built — recorded so they are not rediscovered as bugs). Subsection added 2026-08-14, per the qa-runs plan's Task 14 Step 5:

  - ~~**The reference dev stack cannot grant a gear system actor, so the dispatcher is inert there.**~~ **Resolved — not by growing a policy plugin.** `static-authz` derives its decision purely from the resolved tenant and **denies a nil tenant outright**; its config is only `{vendor, priority}`, so there is no per-subject grant to express. qa-catalog met this by disabling its refresher in the dev config, but a GC being inert is far smaller than the dispatcher being inert: queued runs never start, and because admission is strict FIFO **the first queued row blocks every later launch on that platform**. Task 14 therefore kept the dispatcher enabled and mapped `DomainError::Forbidden` to one actionable WARN per pass naming the remedy, with a `dispatcher_enabled` knob (Task 16) so an operator can disable it deliberately rather than by accident. ~~**Whether the dev stack should instead grow a policy plugin able to grant a system subject is an open decision and explicitly not the qa-runs plan's to make** — it is the one change that would make the reference deployment's dispatcher do anything at all.~~ The open decision was answered differently: the dispatcher's cross-tenant enumerating reads now elevate through a gear-local seam (`domain::elevated::enumeration_scope`, this gear's `domain/elevated.rs`) instead of asking the PDP at all, so `static-authz`'s nil-tenant denial has nothing left to deny. Each write the tick issues afterwards is re-scoped per tenant under `system_actor`'s tenant-bound factories and is granted by the stock plugin exactly as it would grant any end-user request naming that tenant — no plugin capable of granting a system subject was needed. **Amended 2026-08-17: Phase B makes this worse in kind, not only in degree.** ~~The schedule tick enumerates every tenant's schedules under the same nil-tenant system actor, so it fails closed in the reference stack exactly as the dispatcher does~~ **— true when this was written, resolved the same way since.** The schedule tick enumerates every tenant's schedules under the same nil-tenant system actor the dispatcher uses, which is exactly why the fix below applies to it without a separate story: `config/qa-platform.yaml` still carries `scheduler_enabled: false` beside `dispatcher_enabled: false`, but not because either fails closed any more. The consequence is worth stating plainly because it is not the dispatcher's: schedule **CRUD works** there, since it runs under the caller's own identity — so an operator can create a schedule, see it listed, and watch it never fire, with no error anywhere. **The schedule tick's enumeration resolves the same way**, through the same seam, so this too no longer needs a plugin able to grant a system subject. Both switches remain `false` in `config/qa-platform.yaml`: the mechanism is in place and exercised by this crate's own tests, but enabling a ticker that has never run against the reference deployment is a dev-stand verification that has not happened yet.
  - **Dispatch is woken only by the interval sweep.** `cpt-cf-qa-nfr-dispatch-latency` is met by polling at **5 s** (user decision, 2026-08-14 — see DESIGN §2's row). The release-notification wake that DESIGN originally prescribed is **not built**: the ticker is leader-elected and its lock registry is process-local, so a lease released on one replica must wake the leader on another, which needs a broker event or a DB signal rather than an in-process notify. Recorded as the optimization, not as shipped behaviour.
  - ~~**`RunsRepository::set_bundle_ids` has no DB-backed test.**~~ **Closed — re-verified 2026-08-17 during the Phase B close-out.** `runs_sea_repo.rs` now carries `set_bundle_ids_writes_only_the_bundle_list`, which drives the real `UPDATE`, plus a foreign-scope negative. Recorded as closed rather than deleted, because a follow-up that quietly disappears is indistinguishable from one nobody checked.
  - **`queue::QueuedRow` carries no `run_id`**, so the drain re-reads `claims_for_platform` inside the critical section to recover run ids after marking. Widening `QueuedRow` or `queued_rows` would remove that query.
  - ~~**No log line is asserted anywhere in qa-runs**, including the expiry WARN … Blocked on `tracing-subscriber` as a dev-dependency.~~ **Closed — re-verified 2026-08-17.** `tracing-test` is a dev-dependency carrying its own rationale, thirteen `traced_test` sites exist across eight files, and the expiry announcement specifically is asserted by `an_expired_row_is_always_announced_with_its_run_and_its_wait`. The blocking reason was resolved by later work that never linked back here.

  **Added 2026-08-17 at the Phase B close-out. The first three are consequences of the schedules work; the last two are gear-wide and were found by running the thing rather than by reading it.**

  - **A schedule's field violation reports the *run* resource type.** `NewScheduleReq`'s boundary checks raise `DomainError::Validation`, which the REST error mapping attributes to `RunResourceError` — so a caller sending a bad `exclusive_choice` on a schedule is told `cf.qa.runs.run.v1~` rejected their field. Observed in Task 20's live smoke, not deduced. The `field`, the message and the status are all correct; only the resource type is wrong. Three of the four sources are fixable at the handler, which knows the resource statically; the fourth is raised inside `ScheduleService::validate` and would need a second domain variant.
  - **`qa_schedule_ticks` has no read path.** The repository exposes `claim_tick`, `record_tick_outcome` and `advance_last_fired_tick` and no tick read at all, so a failed fire's recorded `error` — which the service names as the operator's only record of a due time that produced no run — is retrievable by no query, endpoint or SDK method this subsystem ships. 2.4's scope notes record the *skipped*-occurrence half of this; this is the *claimed-then-failed* half. Needs a scoped tick read and somewhere to surface it.
  - **A `QueueFull` on a scheduled launch is permanent, and worse for a schedule than for a person.** The claim commits before the launch by design, so a launch answered `QueueFull` is recorded on the tick row and never retried — that occurrence produces no run, ever. A manual launch answered 429 has a human who can try again; a scheduled one has nobody, and by the entry above cannot even read why. `MAX_FIRES_PER_TICK` reduces the exposure and does not remove it. **The fix is a retry policy, and a retry after a committed claim is exactly what exactly-once forbids — so this needs a design decision, not a patch.**
  - **`GET /qa/v1/schedules` is unpaginated and unfiltered.** `SchedulesRepository::list` issues a scoped `find()` ordered by name with no limit, so the endpoint returns every schedule in the caller's scope in one body. Every other collection in this gear is a page with OData. Defensible while schedules are operator-authored and few; it is the one list in qa-runs whose response size no knob bounds.
  - **The firing pass starves a permanently over-subscribed fleet.** `list_enabled` orders by id and `MAX_FIRES_PER_TICK` caps fires at twenty, so a fleet with more than twenty schedules due on *every* pass drains the low ids and never reaches the tail. Ordinary cron alignment drains in a few passes, because a fired schedule stops being due; permanent over-subscription does not. The remedy is a rotating cursor, which needs a `list_enabled` that takes one.
  - **A schedule's cron is validated at write time; its target and platform are not, and the argument for the first applies verbatim to the second.** `ScheduleService::validate` checks the name and the cron expression only. The stated reason for parsing the cron at write time is that otherwise it "would be stored happily and then fail on every evaluation, forever, in a background pass whose only output is a log line" — which is exactly what a dangling `target.custom_plan_id`, `target.repo_id` or `platform_id` does, since the fire path resolves the target through the catalog at launch. Combined with the two entries above — a failed fire produces no run ever, and the tick row recording why is unreachable — an operator gets nothing at all. **A requirements gap rather than an implementation defect**: no task asked for referential validation at write time, and doing it properly means deciding what happens when a target is deleted *after* a schedule references it. Sibling of the tick-read item above; they want closing together.
  - **A missing required field answers 422 with a plain-text body, not the 400 the routes declare.** This is axum's `Json` rejection and is uniform across every DTO in the gear — no route anywhere in qa-runs declares 422. The `enabled`-cannot-be-omitted guarantee does hold; it is simply enforced one status code away from where the OpenAPI document says it is.

### 2.4 Schedules - HIGH

- [x] `p1` - **ID**: `cpt-cf-qa-feature-schedules`

- **Purpose**: Cron schedules with stored exclusivity choice, fired exactly once by a leader-elected task through the standard launch path.

- **Depends On**: `cpt-cf-qa-feature-runs-core`

- **Scope**:
  - Schedule CRUD, cron evaluator, transactional tick claims, multi-instance failover tests

- **Out of scope**:
  - Jobs Manager convergence (p3, PRD §10)

- **Scope notes** (added 2026-08-17 during Task 18; each records a decision the PRD and DESIGN do not make):

  - **Catch-up policy: fire at most one tick per evaluation, the most recent due time at or before `now`, and never back-fill.** Argo decides this with `startingDeadlineSeconds`, which legacy leaves unset (`manager/src/services/argo.rs`, `create_cron_workflow`), so legacy already skips missed occurrences. Back-filling is the worse failure here specifically: a control plane down six hours would enqueue six hours of an hourly destructive suite onto one platform, and the queue is strict FIFO, so those exclusive runs drain one per dispatcher tick with everything else behind them. Skipping is recoverable by an operator relaunch; back-filling is not. This is also what gives `claim_tick` its self-healing property — an orphaned claim is bypassed at the next occurrence rather than wedging the schedule forever, which an "earliest outstanding occurrence" evaluator would do, since nothing in the schedules repository can advance the cursor past a lost claim.
  - **A skipped occurrence leaves no record an operator can query.** `domain::cron::skipped_since` computes the skipped times in memory, but nothing persists them: only a claimed due time gets a `qa_schedule_ticks` row, and `SchedulesRepository` exposes no tick read at all — so even a failed launch's recorded `error` cannot be retrieved. A scoped tick read and somewhere to surface it are a **follow-up**, not covered by 2.4 as decomposed. An earlier revision of the plan claimed these rows existed; that claim was retracted.
  - **The accepted cron grammar is POSIX, translated onto a Quartz-shaped crate, and two divergences are corrected at parse time.** `cron 0.17` numbers day-of-week 1-7 with 1 = Sunday, so POSIX ordinals are renumbered by set expansion; and the crate requires day-of-month **and** day-of-week to match where POSIX fires when **either** does, so a both-restricted expression is evaluated as the union of two schedules. Both were found by measuring the crate, not by reading it — untranslated, every weekly schedule would have fired a day early. Expressions are five-field, UTC only, with the seven `@` descriptors expanded before parsing; `@reboot` is refused, having no due time to claim.
  - **Legacy sets `concurrencyPolicy: "Replace"` on its `CronWorkflow`s and it has no ported analogue.** It bounds overlap of the *trigger* workflow — whose body is a single HTTP POST to the manager API — not of test runs, which legacy governs through its queue and exclusivity machinery. If "a schedule's previous run is cancelled when the next fires" is wanted, that is new behaviour belonging to the schedule service, and nothing in the PRD asks for it.

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-qa-fr-runs-schedules` — CRUD over `/qa/v1/schedules`, firing through the one shared `LaunchService` a manual launch takes.
  - [ ] `p1` - `cpt-cf-qa-nfr-scheduler-exactly-once` — **the no-duplicates half is satisfied; the no-misses half is not. Unticked 2026-08-17 by the independent spec-coverage audit.**

    *No duplicates* is **discharged by `idx_qa_schedule_ticks_claim`, not by leadership.** The unique index is the guarantee and the leader gate is defence in depth; the claim holds even with every replica evaluating. Break-tested by dropping the index and observing a second run.

    *No misses* fails on the requirement's own wording, "under instance failover". `a_failover_mid_fire_does_not_produce_a_second_run` seeds an orphaned claim from a departed instance and **asserts zero runs** — that due time produces nothing, ever, and no reclaim pass exists. Four further miss vectors are documented in the code: a post-claim launch failure, `QueueFull`, `MAX_FIRES_PER_TICK` starving the id-ordered tail, and an undecodable row. The catch-up policy is a fifth by design. **PRD §"no misses" therefore needs an explicit approved deviation, or the misses need closing** — this is a decision, not a patch, because retrying after a committed claim is what the no-duplicates half forbids.

    **Two corrections to what this entry said before.** The verification was *not* "against two real service instances" across the board: only `two_instances_evaluating_the_same_due_time_produce_exactly_one_run` has two, and the failover test has **one instance and a hand-seeded claim row**. And nothing here is concurrent — the store is single-connection in-memory SQLite, so PRD's stated verification method, "integration tests with multiple concurrent scheduler instances", is **not performed**, not merely weakened.

- **Domain Model Entities**: Schedule

- **Design Components**:

  - [x] `p1` - `cpt-cf-qa-component-runs`

- **API**: `GET|POST /qa/v1/schedules`, `GET|PUT|DELETE /qa/v1/schedules/{id}`. The edit is a **full replace, not a PATCH**: `exclusive_choice` is a tri-state and `serde_with` is not a workspace dependency, so a patch would need `Option<Option<Option<bool>>>`. On the wire `exclusive_choice` is the string `"true"`/`"false"`/`"auto"` — the vocabulary the source system stores — so `null` can never be mistaken for `false`. **`enabled` is required on both writes**, which is what stops an operator editing a cron expression and silently resuming a paused schedule: the legacy quirk this feature exists not to port.

- **Sequences**:

  - [x] `p1` - `cpt-cf-qa-seq-schedule`

- **Data**: `schedules`, `schedule_ticks`

### 2.5 Insights Foundation - MEDIUM

- [ ] `p1` - **ID**: `cpt-cf-qa-feature-insights-foundation`

- **Purpose**: qa-insights gear with idempotent, reconciler-driven ingestion at two result granularities, per-test history, dashboard/coverage aggregates, the full analytics surface with saved views and expected-case collection, ReportPortal links.

- **Depends On**: `cpt-cf-qa-feature-runs-core` (event vocabulary; the `Collect` run kind), `cpt-cf-qa-feature-catalog` (plan/TEST_META universe and static per-file case counts, over `qa-catalog-sdk` — added 2026-08-18 per design-spec Finding 4; every analytics list item carries `component`, `tags`, `plan_id`, `plan_name` and `versions`, which the source system reads off the on-disk checkout and this split makes qa-catalog's alone)

- **Scope**:
  - Gear + SDK pair; analytical schema of eleven tables (DESIGN §3.7)
  - **Corrected 2026-09-01**: the two bullets below described a transactional
    event-broker consumer as qa-insights' primary ingest path, with the
    leader-elected reconciler as its backfill. event-broker registered no
    client, so that consumer never ran and has been deleted along with the
    event-broker dependency it needed; the reconciler was, and remains,
    qa-insights' only ingest path. Left below for the historical record of
    what was scoped, not as a description of what ships.
  - ~~**Transactional** event ingestion — the offset commit and the projection write share one database transaction, so redelivery cannot double-count and a crash cannot skip; qa-insights is the platform's first event-broker *consumer*~~
  - Both result granularities: per test file per run, and per test case per run (`nodeid`, `reason`, `ticket`) — D1
  - Leader-elected reconciler that re-projects every run it finds via qa-runs' SDK, plus an operator rebuild-from-runs endpoint; the reconciler is qa-insights' ingest path, not a backfill for one
  - Dashboard and coverage aggregates
  - The analytics surface: the overview payload's eight sections (`summary`, `lists`, `heatmap`, `trend`, `build_distribution`, `flaky`, `quality_vectors`, `grouped`), export, build-tests, the three plan drill-downs, and saved views keyed `(owner, scope, plan, name)` — D3, D4
  - Expected-case counts both ways: static per-file counts from qa-catalog, and exact `--collect-only` counts via a `Collect` run in qa-runs whose report route this gear serves — D2

- **Out of scope**:
  - JIRA and notifications (2.8)
  - Real test execution (2.7) — until it lands, exact collect produces no counts; the trigger, the run kind, the report route and the table all ship and are exercised against the mock executor

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-qa-fr-insights-history`
  - [ ] `p1` - `cpt-cf-qa-fr-insights-dashboard`
  - [ ] `p1` - `cpt-cf-qa-fr-insights-analytics`
  - [ ] `p1` - `cpt-cf-qa-fr-insights-expected-cases`
  - [ ] `p1` - `cpt-cf-qa-fr-insights-reportportal`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-qa-principle-async-insights`

- **Domain Model Entities**: TestResultRecord, TestCaseResultRecord, ExpectedCaseCount, SavedView (see DESIGN §3.1 — the last three were added there on 2026-08-18 per D1/D2/D4)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-qa-component-insights`

- **API**: `/qa/v1/dashboard`, `/qa/v1/dashboard/coverage`,
  `/qa/v1/analytics/overview`, `/qa/v1/analytics/build-tests`,
  `/qa/v1/analytics/export`, `/qa/v1/analytics/views` (+ `/{id}`),
  `/qa/v1/analytics/plan/{plan_id}/{tests,builds,test-history}`,
  `/qa/v1/analytics/collect`, `/qa/v1/collect/{repo_id}/{branch}` (the
  runner-facing report target), `/qa/v1/test-results` and
  `/qa/v1/test-case-results` (the two **OData** collections),
  `/qa/v1/insights/rebuild`. Aggregates keep the source system's fixed
  parameters; OData applies to the two flat collections only (D7).

- **Data**: `test_results`, `test_case_results`, `test_case_collect`,
  `analytics_saved_views`, `ingest_watermarks` (see DESIGN §3.7 for the full
  eleven-table schema, of which `jira_bugs`, the three config singletons,
  `run_notifications` and `notification_log` belong to 2.8).

- **Amended 2026-08-18** by `docs/plans/2026-08-18-qa-insights-gear.md` and the
  design spec it implements (`docs/superpowers/specs/2026-08-18-qa-insights-design.md`,
  decisions D1–D10). **Phase 0 of that plan amends two already-shipped gears**,
  so this feature is *not* buildable against the gears as they stand at commit
  `c3abe942`: `qa-runs` gains `nodeid` / `reason` / `ticket` on its per-test rows
  (D1; the companion `qa.test.result` event field never shipped a running
  publisher — event-broker has no working consumer — and the dead publish
  code has since been removed; see PRD `cpt-cf-qa-interface-events`), a
  `Collect` run kind carrying
  `COLLECT_ONLY` / `VHP_COLLECT_URL` and bypassing admission (D2), and three
  per-schedule Slack notification columns (D9); `qa-catalog` gains SDK exposure
  of the plan/TEST_META universe and per-file static test-function counts (D2,
  Finding 4). All are additive — new nullable columns, new optional event fields,
  a new run-kind variant, new SDK methods — and the shipped qa-runs suite (778
  tests at `c3abe942`) staying green is the phase gate.

### 2.6 UI Gear & SPA Adaptation - MEDIUM

- [ ] `p1` - **ID**: `cpt-cf-qa-feature-ui`

- **Purpose**: qa-ui gear with embedded SPA delivery, plus the React app adapted to `/qa/v1`, platform token auth, and SSE logs. Parity page list per PRD.

- **Depends On**: `cpt-cf-qa-feature-runs-core`, `cpt-cf-qa-feature-catalog`, `cpt-cf-qa-feature-environments`, `cpt-cf-qa-feature-insights-foundation` (pages need their APIs; gear shell can start earlier)

- **Scope**:
  - qa-ui crate (rust_embed, feature-gated, SPA fallback), CI build orchestration for `dist/`
  - SPA adaptation: API client, auth, WS→SSE, queued-run cards

- **Out of scope**:
  - Visual redesign; legacy HTML pages

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-qa-fr-ui-embedded`
  - [ ] `p2` - `cpt-cf-qa-fr-ui-parity` (final parity sign-off needs 2.7 for live-execution pages)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-qa-component-ui`

- **API**: `GET /qa/ui/*`

- **Data**: none

- **Tracked follow-ups** (real gaps, deliberately not built — recorded so they are not rediscovered as bugs). Subsection added 2026-09-06, during the review-remediation branch's CI-wiring phase:

  - **`make ui-lint` has never been able to run, and the UI therefore has no lint gate.** `package.json` pins `eslint: ^9.17.0`, which reads only flat config, and **no `eslint.config.js` has ever been committed on any branch** (`git log --all` over `eslint.config.*` and `.eslintrc*` is empty). The `lint` script also still passes `--ext ts,tsx`, a flag ESLint 9 removed. The target was added alongside `ui-test`/`ui-build` and nothing ever invoked it, so the breakage was invisible — which is review finding #49's own thesis one level down: a target no job runs is a target nobody discovers is broken.
    **Measured scope, so this is not rediscovered as unbounded:** a probe flat config using `@typescript-eslint/recommended` reports **15 errors and 1 warning across 12 of 139 files** — `react-hooks/exhaustive-deps` ×7, `@typescript-eslint/no-explicit-any` ×6, `@typescript-eslint/no-empty-object-type` ×2, plus one parse-level message. Closing it is roughly a 40-line flat config plus those 15 fixes, and a decision about which rule set the team wants — which is why it was not folded into a CI-wiring task.
    **Until it is closed**, the `ui` job in `.github/workflows/ci.yml` deliberately runs `make ui-test ui-build` only, with a comment at the run step recording the same reason. The UI does have a real gate — 231 vitest tests plus `tsc` — just not a lint one.

### 2.7 Serverless Execution & Runner Migration - HIGH (blocked)

- [ ] `p2` - **ID**: `cpt-cf-qa-feature-execution`

- **Purpose**: Replace the mock with the serverless-runtime adapter and migrate the Python runner from stdout markers to typed execution events. The parity-defining slice.

- **Depends On**: `cpt-cf-qa-feature-runs-core`; **blocked on serverless-runtime Python workloads** (PRD §10, §12)

- **Scope**:
  - Serverless `RunExecutor` adapter (start/cancel/watch with stream re-attach)
  - Runner repackaging as a Python serverless workflow; event emitter replacing markers; env contract untouched
  - Live/archived logs end-to-end; soak tests for multi-hour runs; reference-repository equivalence run

- **Out of scope**:
  - Multi-runner sharding (PRD open question — contract must not preclude it)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-qa-fr-runs-execute`
  - [ ] `p2` - `cpt-cf-qa-fr-runs-results-ingest`
  - [ ] `p2` - `cpt-cf-qa-fr-runs-logs`
  - [ ] `p2` - `cpt-cf-qa-fr-migration-runner-contract`
  - [ ] `p2` - `cpt-cf-qa-nfr-run-duration`
  - [ ] `p2` - `cpt-cf-qa-nfr-result-latency`
  - [ ] `p2` - `cpt-cf-qa-nfr-log-latency`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-qa-constraint-serverless-readiness`

- **Design Components**:

  - [ ] `p2` - `cpt-cf-qa-component-runs`

- **Sequences**:

  - [ ] `p2` - `cpt-cf-qa-seq-launch` (real-executor variant)

- **Data**: `runs` (execution_id, log_storage_ref population)

### 2.8 JIRA Loop & Notifications - MEDIUM

- [ ] `p1` - **ID**: `cpt-cf-qa-feature-jira-notifications`

- **Purpose**: JIRA bug registry + poller + skip-lists + auto-rerun, and the full run-notification surface — Slack and email, scheduled-run and queue events, dedupe and audit log — all external egress via oagw.

- **Depends On**: `cpt-cf-qa-feature-insights-foundation`; auto-rerun additionally on `cpt-cf-qa-feature-runs-core`

- **Scope**:
  - Bug registry keyed on test identity, open-bug views, filing/linking a bug from a run view, skip-tests-with-bugs delivered to the runner as the frozen comma-separated `test_name:JIRA-KEY` list
  - JIRA config and poller (leader-elected); resolution-triggered rerun via `qa-runs-sdk` through the **normal admission path**, gated on a resolved transition **and** a newer build (D8)
  - **Slack**: webhook and named-channel forms, Block Kit block rendering, scheduled-run message templates with placeholder and conditional expansion
  - **Email**: the full configuration surface, recipient routing, dedupe and logging — but see the deferral below (D10)
  - Six scheduled-run status events (`Pending`, `InProgress`, `Succeeded`, `Failed`, `Error`, `Skipped`), consuming the three per-schedule settings qa-runs gains in Phase 0 (D9)
  - Two queue events: `Queued` (opt-in, off by default) and `Expired` (**mandatory, not toggleable** — the event that stops a run vanishing silently)
  - The dedupe table keyed `(run, notification kind, event type)` and the notification audit log with its read endpoint; test-send and preview operations; a bounded outbound timeout, because the mandatory `Expired` send happens inline on the dispatcher tick

- **Out of scope**:
  - Notifications Service convergence (p3)
  - **The email *send* itself (D10)** — the platform has no SMTP egress: oagw
    speaks HTTP, SSE and WebSocket only. Everything else about email ships and is
    tested; the mail client is a domain port whose only adapter records outcome
    `unsupported_egress` in the log and returns success. Slack ships fully
    working. See `cpt-cf-qa-fr-insights-notifications`.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-qa-fr-insights-jira`
  - [ ] `p2` - `cpt-cf-qa-fr-insights-auto-rerun`
  - [ ] `p1` - `cpt-cf-qa-fr-insights-notifications`

- **Domain Model Entities**: JiraBug, NotificationConfig (per-tenant singleton), NotificationLogEntry

- **Design Components**:

  - [ ] `p2` - `cpt-cf-qa-component-insights`

- **API**: `/qa/v1/jira/open-bugs`, `/qa/v1/jira/bugs`,
  `/qa/v1/settings/jira`, `/qa/v1/settings/jira-poller`,
  `/qa/v1/settings/notifications` (+ `/test`, `/preview`, `/log`). The skip list
  is **SDK-only** — qa-runs asks for it at launch when skip-tests-with-bugs is
  requested; it is not a REST route.

- **Sequences**:

  - [ ] `p2` - `cpt-cf-qa-seq-jira-rerun`

- **Data**: `jira_bugs`, `jira_config`, `jira_poller_config`,
  `notification_config`, `run_notifications`, `notification_log`.
  **`notification_rules` is deleted from this list**: no such table exists in the
  source system and none is being built (D6). Notification configuration is a
  singleton row per tenant, not a rules engine — the source system keeps it as
  one JSON row in a generic `settings` table under the key `notifications`
  (`manager/migrations/001_initial.sql:93`,
  `manager/src/services/notifications.rs:87`), and this gear ships the same
  fields as typed per-tenant columns because a generic JSONB bag would forfeit
  the migration and validation typed columns give.

- **Amended 2026-08-18** by `docs/plans/2026-08-18-qa-insights-gear.md` and the
  design spec it implements (`docs/superpowers/specs/2026-08-18-qa-insights-design.md`,
  decisions D1–D10). **Phase 0 of that plan amends two already-shipped gears**,
  so this feature is *not* buildable against the gears as they stand at commit
  `c3abe942`: `qa-runs` gains `nodeid` / `reason` / `ticket` on its per-test rows
  (D1; the companion `qa.test.result` event field never shipped a running
  publisher — event-broker has no working consumer — and the dead publish
  code has since been removed; see PRD `cpt-cf-qa-interface-events`), a
  `Collect` run kind carrying
  `COLLECT_ONLY` / `VHP_COLLECT_URL` and bypassing admission (D2), and three
  per-schedule Slack notification columns (D9); `qa-catalog` gains SDK exposure
  of the plan/TEST_META universe and per-file static test-function counts (D2,
  Finding 4). All are additive — new nullable columns, new optional event fields,
  a new run-kind variant, new SDK methods — and the shipped qa-runs suite (778
  tests at `c3abe942`) staying green is the phase gate. Of that list, this feature depends specifically on D9's
  per-schedule columns and on the normal-admission guarantee restated in D8.

### 2.9 Data Migration - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-qa-feature-migration`

- **Purpose**: One-shot import of a VHP Test Runner PostgreSQL database into the per-gear schemas with tenant assignment and reconciliation reporting.

- **Depends On**: All schema-owning features (2.1, 2.2, 2.3, 2.4, 2.5, 2.8)

- **Scope**:
  - Migration tool with dry-run mode, mapping documentation, reconciliation report, production-snapshot rehearsal

- **Out of scope**:
  - Live dual-write or phased cutover (one-shot by design)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-qa-fr-migration-data`

- **Data**: all subsystem tables (as targets)

### 2.10 Deployment Packaging - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-qa-feature-deploy`

- **Purpose**: Deliver the three run shapes as supported artifacts: reference host-app composition (single-node), container image, and Helm chart, under `gears/qa-platform/deploy/` following the mini-chat precedent.

- **Depends On**: `cpt-cf-qa-feature-runs-core`, `cpt-cf-qa-feature-ui` (a meaningful composition needs the control plane and the UI); chart e2e sign-off additionally benefits from `cpt-cf-qa-feature-execution` but is not blocked by it (mock-executor smoke acceptable for chart validation)

- **Scope**:
  - Reference host-app registering the qa-* gears + required platform gears, with example `config/qa.yml`
  - `deploy/docker/`: source-build and prebuilt-binary Dockerfiles
  - `deploy/helm/qa-platform/`: Deployment, Service, ConfigMap, Secret, RBAC/lease templates; external-or-bundled Postgres values
  - CI: single-node smoke run; Helm install + e2e on a disposable cluster

- **Out of scope**:
  - Serverless-runtime deployment (owned by that gear); runner workload packaging (2.7); production sizing guidance

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-qa-fr-deploy-packaging`

- **Design Components**:

  - [ ] `p2` - `cpt-cf-qa-topology`

- **API**: none (packaging only)

- **Data**: none

## 3. Feature Dependencies

```text
cpt-cf-qa-feature-environments   cpt-cf-qa-feature-catalog
        └──────────────┬────────────────┘
                       ↓
        cpt-cf-qa-feature-runs-core
             ↓               ↓                    ↓
  ...-feature-schedules  ...-feature-insights-foundation   ...-feature-execution (blocked: serverless-runtime)
                              ↓               ↘
                 ...-feature-jira-notifications  ...-feature-ui
                                             ↓            ↓
                              cpt-cf-qa-feature-migration  cpt-cf-qa-feature-deploy
                              (after all schemas stable)   (after runs-core + ui)
```

**Dependency Rationale**:

- `runs-core` requires `environments` and `catalog`: launch resolution needs environments/leases/variables and plans/TEST_META/bundles.
- `schedules`, `insights-foundation`, and `execution` all require `runs-core` (launch path, event vocabulary, executor port) and are mutually independent — parallelizable.
- `insights-foundation` **also requires `catalog` directly** (added 2026-08-18,
  qa-insights design spec Finding 4). The graph above shows only the transitive
  path through `runs-core`, which was enough while insights was assumed to read
  nothing but events; it is not, because every analytics list item carries
  `component`, `tags`, `plan_id`, `plan_name` and `versions` and the
  quality-vector rollup is computed over TEST_META, all of which live behind
  `qa-catalog-sdk`. The edge is contract-mediated and non-cyclic — insights
  depends on the SDK crate, not on the gear, and catalog depends on neither
  (DESIGN §3.4).
- `environments` and `catalog` are independent of each other — parallelizable from day one.
- `ui` requires the APIs its pages render; the gear shell itself can be built alongside `runs-core`.
- `migration` last: it targets final schemas; running it against moving schemas would churn the mapping and reconciliation logic.
