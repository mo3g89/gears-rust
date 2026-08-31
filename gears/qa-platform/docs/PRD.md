# PRD — QA Platform

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Test Catalog](#51-test-catalog)
  - [5.2 Run Orchestration](#52-run-orchestration)
  - [5.3 Environments](#53-environments)
  - [5.4 Insights](#54-insights)
  - [5.5 User Interface](#55-user-interface)
  - [5.6 Migration](#56-migration)
  - [5.7 Packaging & Deployment](#57-packaging--deployment)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

QA Platform is a subsystem of cooperating gears that provides end-to-end test orchestration for the Constructor Fabric Gears platform: cataloging test plans from git repositories, launching and queueing test runs against registered target environments, scheduling recurring runs, collecting per-test results, and analyzing quality trends with JIRA bug correlation.

The subsystem is a conversion of the standalone **VHP Test Runner** application (Rust `manager` control plane + React UI + Python pytest runner + Argo Workflows execution engine) into native Fabric gears. Functional behavior reaches full parity with the current manager; the execution engine moves from Argo Workflows to the platform serverless execution capability, and all cross-cutting concerns (auth, tenancy, secrets, outbound traffic, eventing, file storage) are delegated to platform gears.

### 1.2 Background / Problem Statement

The VHP Test Runner exists today as an independent Kubernetes-native stack: an axum-based control plane owning its own PostgreSQL schema, direct `kube` client access for creating Argo `Workflow`/`CronWorkflow` objects, a bespoke marker-scraping poller for result collection, nginx basic-auth as the only access control, and hand-rolled SMTP/JIRA integrations.

This architecture has served a single team well, but it cannot be offered as a platform capability:

- It is single-tenant with no real authentication or authorization model.
- It hard-depends on Kubernetes and Argo Workflows, so it cannot run in the single-node or multi-node deployment shapes the Fabric platform supports.
- It duplicates concerns the platform already standardizes — secret storage, outbound HTTP governance, eventing, observability, API conventions — with weaker guarantees.
- Its state model treats Argo Workflow objects as the source of truth for runs, coupling business state to the K8s control plane and requiring log-scraping to recover results.

Converting the stack into Fabric gears makes test orchestration a reusable, multi-tenant, deployment-agnostic platform capability while preserving the working test-repository contract (plan.yaml, TEST_META, environment variables) so existing test suites run unmodified.

### 1.3 Goals (Business Outcomes)

- Provide test orchestration as a composable platform capability available to any Fabric-based product, in all three deployment shapes (single-node, multi-node, Kubernetes).
- Reach functional parity with the current VHP Test Runner manager so the existing team can migrate without losing capability (parity checklist = §5 requirements at p1/p2).
- Existing test repositories run unmodified: 100% of suites that run under the current runner produce equivalent results under the new execution plane.
- Remove the Argo/Kubernetes hard dependency from the control plane: zero direct `kube` API usage in QA Platform gears.
- Multi-tenant, secure-by-default posture inherited from the platform: every entity tenant-owned, every operation policy-checked.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Test plan | A named set of test files with metadata (timeout, tags, exclusivity), defined by a YAML file in a test repository — either `plans/<anything>.yaml` or `<dir>/plan.yaml` (see `cpt-cf-qa-fr-catalog-plan-discovery`) — or composed manually (custom plan). |
| Custom plan | A user-composed, DB-persisted list of test files (possibly across repos) runnable as a unit. |
| Test repository | An external git repository containing pytest test files and plan definitions, registered in the catalog and synced periodically. |
| TEST_META | Structured metadata declared inside a test file (`title`, `tags`, `exclusive`). Parsed as text, never executed, and scanned over the whole file rather than scoped to a block (see `cpt-cf-qa-fr-catalog-test-meta`). |
| Test bundle | An ephemeral tar.gz archive packaging the test content of a repo-backed run, downloaded by the execution plane. |
| Run | One execution of a plan, custom plan, or single test against a platform, with parameters, producing a run result and per-test results. |
| Run parameters | Per-run `name=value` pairs injected into the execution environment; most specific tier of the environment chain. |
| Exclusive run | A run that holds its target platform exclusively; other runs targeting that platform queue behind it. |
| Platform (target) | A registered target environment (e.g., a Kubernetes cluster reachable via kubeconfig) that tests execute against. Distinct from "the Fabric platform". |
| Platform variables | Per-target-platform environment variables injected into every run against that platform. |
| Pipeline variables | Global environment variables injected into every run, overridable by platform variables and run parameters. |
| Schedule | A cron-defined recurring launch of a plan or custom plan, with a stored exclusivity choice (`true`/`false`/`auto`). |
| Runner | The Python pytest execution component that runs test files and emits structured progress/result events. |
| Launch (ReportPortal) | An optional external test-reporting record linked to a run. |
| Exclusivity resolution | The three-tier precedence `launch ?? plan.yaml ?? OR(TEST_META) ?? parallel`, where unset upper tiers inherit downward. |

## 2. Actors

> **Note**: Stakeholder needs are managed at project level. This section documents actors interacting with the subsystem.

### 2.1 Human Actors

#### QA Engineer

**ID**: `cpt-cf-qa-actor-engineer`

- **Role**: Authors tests in external repositories; launches, monitors, cancels, and re-runs test runs; composes custom plans; inspects results, logs, and history.
- **Needs**: Fast launch with parameter overrides, live logs, accurate per-test results, safe exclusivity semantics for destructive tests.

#### QA Administrator

**ID**: `cpt-cf-qa-actor-admin`

- **Role**: Registers test repositories, target platforms, products, and schedules; manages SSH keys, platform variables, notification and JIRA settings.
- **Needs**: Manage the catalog and environments without touching secrets in plaintext; confidence that schedules fire exactly once.

### 2.2 System Actors

#### Test Repository (git)

**ID**: `cpt-cf-qa-actor-test-repo`

- **Role**: External git repository holding test files, plan definitions, and TEST_META metadata. Read-only source synced by the catalog — outbound, from the catalog's own git adapter under the scoped exception in `cpt-cf-qa-contract-egress`, not through the platform outbound gateway.

#### Execution Plane (serverless runtime)

**ID**: `cpt-cf-qa-actor-execution-plane`

- **Role**: Platform serverless execution capability that runs the pytest workload as a workflow, streams execution events and logs back, and honors cancellation. (Interface consumer/provider details in DESIGN.)

#### Target Platform (system under test)

**ID**: `cpt-cf-qa-actor-target-platform`

- **Role**: External environment (typically a Kubernetes cluster) that tests exercise, reached by the runner using credentials referenced from the environments registry. Never called by control-plane gears directly.

#### JIRA

**ID**: `cpt-cf-qa-actor-jira`

- **Role**: External bug tracker. Outbound: bug status polling. Bugs are linked to tests in the insights-owned bug registry (`cpt-cf-qa-fr-insights-jira`), which is what links failures to known issues; resolution can trigger automatic re-runs.

#### ReportPortal

**ID**: `cpt-cf-qa-actor-reportportal`

- **Role**: Optional external test-reporting system. The runner reports launches to it; QA Platform stores and displays launch links. Not a source of truth for results.

#### CI System

**ID**: `cpt-cf-qa-actor-ci`

- **Role**: External CI pipelines that launch runs and consume run outcomes via the REST API and lifecycle events. Direction: inbound (launch) and outbound (events/webhooks via platform eventing).

#### Notification Channel (email)

**ID**: `cpt-cf-qa-actor-notification`

- **Role**: SMTP endpoint receiving run-completion notifications (via platform outbound gateway; migrates to the Notifications Service when available).

## 3. Operational Concept & Environment

Project-wide runtime, security, API, and lifecycle conventions are defined at the repository level and are not repeated here:

- [Architecture Manifest](../../../docs/ARCHITECTURE_MANIFEST.md) — three-tier hierarchy, secure-by-default data path, lifecycle, deployment shapes
- [guidelines/](../../../guidelines/README.md) — dependencies, security, GTS
- [GEARS.md](../../../docs/GEARS.md) — gear inventory and dependency rules

Gear-specific environment constraints:

- Test executions are long-running (minutes to hours) relative to typical platform requests; the execution dependency must support long-lived workloads with cancellation (see §6 and Risks).
- The runner requires network reachability to target platforms (systems under test); control-plane gears never require it.
- The subsystem must remain fully functional in single-node deployments (no Kubernetes assumed anywhere in the control plane).

## 4. Scope

### 4.1 In Scope

- Test catalog: repositories, branch cache, plan discovery, TEST_META parsing, custom plans, products and product folders (every repository belongs to a product), ephemeral test bundles, SSH key management.
- Run orchestration: launch with parameters, exclusivity resolution, per-platform FIFO queue, dispatch with crash recovery, cancellation, re-run, live log streaming, incremental result ingestion, cron schedules, lifecycle event publication.
- Environments: target platform registry with secret kubeconfig references, platform and pipeline variables, platform version polling.
- Insights: per-test history, dashboard and coverage aggregates, analytics with saved views, JIRA bug tracking with skip-known-bugs and auto-rerun, run-completion email notifications, ReportPortal link storage.
- User interface: the existing React SPA adapted to the new API and auth model, delivered as an embeddable UI gear.
- Runner migration: pytest runner repackaged for the platform execution plane with structured event emission (replacing stdout marker scraping), preserving the test-facing environment-variable contract.
- One-shot data migration from an existing VHP Test Runner PostgreSQL database.
- Deployment packaging: a reference host binary composition and container/Helm packaging for the Kubernetes shape.

### 4.2 Out of Scope

- The planned VHP Testrunner roadmap features not present in the current manager (plugin management framework, builds integration, product installation, infrastructure provisioning, CI-integration framework, existing-infrastructure runs). These remain specced in the source repository and can be layered onto this subsystem later.
- Compatibility shim for the legacy `/api/*` REST paths and the legacy server-rendered HTML pages. The adapted UI ships with the subsystem; external scripts must move to `/qa/v1/*`.
- Deployment/operation of external systems themselves (ReportPortal, JIRA, SMTP, target clusters).
- A test-authoring experience (editing test files) — tests live in external git repositories.
- Non-pytest test frameworks. The runner contract is framework-agnostic in principle, but only the pytest runner is delivered.

## 5. Functional Requirements

> **Testing strategy**: All requirements verified via automated tests (unit, integration, e2e) targeting 90%+ coverage per repository policy, unless a verification method is stated.
>
> Priorities encode dependency readiness (see §10): **p1** = implementable on today's platform; **p2** = requires the serverless execution plane; **p3** = converges onto platform gears that are not yet available (interim in-gear mechanism shipped at p1/p2).
>
> No UPSTREAM_REQS document exists for this subsystem; `Covers` fields are therefore not applicable.

### 5.1 Test Catalog

#### Test repository management

- [x] `p1` - **ID**: `cpt-cf-qa-fr-catalog-repos`

The system MUST let administrators register, update, and remove git test repositories (URL, default branch, content root, credential reference) and MUST sync repository content on demand and on a configurable interval.

- **Rationale**: Test content lives in external git; the catalog is the platform's read model of it.
- **Actors**: `cpt-cf-qa-actor-admin`, `cpt-cf-qa-actor-test-repo`

#### Branch cache

- [x] `p1` - **ID**: `cpt-cf-qa-fr-catalog-branch-cache`

The system MUST maintain a periodically refreshed cache of branches per repository, refreshed on a configurable interval and on demand, for use in launch-time branch selection.

- **Rationale**: Branch listing against remote git on every UI interaction is slow and rate-limited.
- **Actors**: `cpt-cf-qa-actor-admin`, `cpt-cf-qa-actor-engineer`

#### SSH key management

- [x] `p1` - **ID**: `cpt-cf-qa-fr-catalog-ssh-keys`

The system MUST let administrators manage named SSH keys and repository access tokens for repository sync. Key material MUST be held in the platform credential store; the catalog persists only metadata and references.

- **Rationale**: Private test repositories require credentials; plaintext secrets in the gear database are prohibited by platform security posture.
- **Actors**: `cpt-cf-qa-actor-admin`

#### Plan discovery

- [x] `p1` - **ID**: `cpt-cf-qa-fr-catalog-plan-discovery`

The system MUST discover test plans from synced repositories in **both** layouts the source system supports, and MUST expose discovered plans with their metadata (name, test files, timeout, tags, optional three-state `exclusive` flag):

1. **File-based plans** — every `*.yaml` file directly inside a `plans/` directory under the content root (flat, non-recursive). This is the primary flavor in both systems, and any filename is accepted, not only `plan.yaml`.
2. **Directory plans** — `<dir>/plan.yaml` one level under the content root, and `<dir>/<subdir>/plan.yaml` two levels under it, where **the second level is scanned only when the first-level directory has no `plan.yaml` of its own** (a depth-1 plan consumes its directory; its children are not descended into). Depth 3 is never reached.

Where two discovered plans resolve to the same identity, the file-based flavor wins; discovery output MUST be deterministically ordered and de-duplicated so that repeated reads of an unchanged working copy agree.

The `tests:` key is optional for directory plans: when omitted, the file list is derived from the plan directory's `tests/` subtree as the source system does (recursive, files named `test_*.py`, sorted; repository-sourced plans additionally require the file to contain `TEST_META`). Explicit `tests:` entries are normalized but not re-rooted at the plan directory. An absent `timeout_seconds` means 300 seconds, per the source system's plan-level default.

- **Rationale**: Plans are the primary launchable unit; their definition format is an existing contract with test authors. Both layouts and the depth rule are load-bearing: an earlier draft of this requirement described only "`plan.yaml` definitions", which understates the primary flavor and would have silently dropped every `plans/*.yaml` plan. Source: VHP Test Runner `manager/src/services/plans.rs` (`scan_root` dedup and flavor order, `scan_file_based_plans`, `scan_legacy_directory_plans` — the depth rule is the early `continue` after a depth-1 `plan.yaml`; `list_test_files` for the derived list; `default_timeout` = 300).
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-test-repo`

#### TEST_META parsing

- [x] `p1` - **ID**: `cpt-cf-qa-fr-catalog-test-meta`

The system MUST parse TEST_META metadata from test files as text (never executing them), accepting both Python and JSON boolean literals, and MUST expose per-file metadata (title, tags, exclusivity). Any occurrence of an exclusive-true declaration anywhere in the file marks the file exclusive, taking precedence over false occurrences.

Contract details, all of them existing behavior that this requirement previously left unstated:

- **Keys are scanned over the whole file, not scoped to a `TEST_META = { … }` block.** Both systems do this deliberately: a brace-delimited scope regex breaks on any nested dict value, so no reader in the source system ever scoped its keys (VHP Test Runner `manager/src/services/test_meta.rs:24-28` records the rejected alternative; mirrored in `qa-catalog/src/domain/parsing/test_meta.rs`). The accepted cost is that prose or a commented-out line mentioning an exclusive-true declaration makes the file read as exclusive — the safe direction of a wrong guess.
- **The `exclusive` match is case-insensitive on the key as well as on the literal** (`"EXCLUSIVE": TRUE` matches), and **the key must be quoted** — single or double. A bare `exclusive: True` does not match, and the surrounding quotes are what stop `"non_exclusive"` and `"exclusive_setup"` from matching as the key.
- Only the literals `true`/`false` (either case) are recognized; `1`/`yes` are not.

**Linked bugs are not part of this requirement.** An earlier draft required exposing "linked bugs" here, which was never true of the source system: TEST_META has no `bugs`/`jira`/`issues` key at all, and the complete recognized key set is `title` (with a legacy `TEST_TITLE` fallback), `component`, `tags`, `quality_vectors`, and `exclusive`, with a description taken from the module docstring. Bug↔test linkage is a database relation owned by insights — see `cpt-cf-qa-fr-insights-jira`. The gear does ship an **additive, optional `bugs` list key** as a new documented convention (it will be empty on every existing repository); it is retained because a repository-declared known-bug list is a genuine improvement over the UI-only linkage, but it is explicitly *not* a source of truth for bug tracking and consumers MUST NOT read an absent `bugs` key as "no known bugs".

- **Rationale**: Existing contract with test authors; the deliberately conservative "any occurrence wins" rule prevents a stale commented-out value from disarming a destructive-test declaration.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-test-repo`

#### Custom plans

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-catalog-custom-plans`

The system MUST let engineers create, update, delete, and launch custom plans: named, persisted selections of test files with optional tag filters and timeout.

- **Rationale**: Ad-hoc suites (triage sets, release gates) without editing repositories.
- **Actors**: `cpt-cf-qa-actor-engineer`
- **Partially delivered, amended 2026-08-13 (qa-runs plan, Task 1)**: the
  create/update/delete/list clauses shipped with `cpt-cf-qa-feature-catalog`.
  The **launch** clause is delivered by `cpt-cf-qa-feature-runs-core` — qa-catalog
  deliberately launches nothing (DESIGN §3.2, qa-catalog "Responsibility
  boundaries"; DECOMPOSITION 2.2 records the grouping contract qa-runs
  implements). This requirement stays unchecked until qa-runs ships rather than
  being split, so the remaining-work estimate cannot read it as done.

#### Products

- [x] `p1` - **ID**: `cpt-cf-qa-fr-catalog-products`

The system MUST manage products and product folders, and every test repository MUST belong to a product, so discovered plans and their runs can be attributed to a product.

- **Rationale**: Products are how the catalog, the run history, and the analytics surfaces are scoped. Repository ownership is the attribution path; the source system additionally keyed a curated product-version-to-branch table, which it deleted in VHP-319 in favour of branch selection (per-platform default plus a launch-time override).
- **Actors**: `cpt-cf-qa-actor-admin`, `cpt-cf-qa-actor-engineer`

#### Test bundles

- [x] `p1` - **ID**: `cpt-cf-qa-fr-catalog-bundles`

The system MUST package the test content of a repo-backed run into an ephemeral, integrity-checked archive stored via the platform file-storage capability, and MUST serve it to the execution plane via an authenticated download endpoint. Bundles MUST be garbage-collected after a configurable retention period.

- **Rationale**: The execution plane must receive an immutable snapshot of test content; git access from the runner is neither needed nor desirable.
- **Actors**: `cpt-cf-qa-actor-execution-plane`

### 5.2 Run Orchestration

#### Run launch

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-launch`

The system MUST launch runs of a plan, custom plan, or single test file against a selected target platform, with optional run parameters, tag include/exclude filters, an optional test-content branch (resolved explicit request → platform default → repository default, and recorded as the run's test version), and an optional explicit exclusivity choice. A launch MUST return one of exactly three outcomes: started immediately, queued, or rejected with the limit that was hit.

- **Rationale**: Core capability; the three-outcome contract is load-bearing for both UI and CI callers.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-ci`
- **Amended 2026-08-18 (qa-insights design spec, decision D2)**: alongside the
  plan / custom-plan / single-test kinds, the system MUST support a **`Collect`
  run kind** — a collect-only execution that enumerates test cases instead of
  running them. It differs from every other kind in three ways, all ported:
  it **bypasses admission** entirely (no exclusivity resolution, no queue insert,
  `exclusive` passed as `false` explicitly — `manager/src/services/argo.rs:369-373`,
  whose comment names `collect` as a genuine bypass; note that the same comment's
  claim about `jira_poller` is stale, see `cpt-cf-qa-fr-insights-auto-rerun`); it
  therefore returns *started*, never *queued*; and it carries exactly two
  environment variables and nothing else, `COLLECT_ONLY=true` and
  `VHP_COLLECT_URL=<url>` (`manager/src/services/argo.rs:50-59`, the URL pushed
  only when one was supplied). Both names are frozen under
  `cpt-cf-qa-fr-migration-runner-contract` (§5.6). A collect submission that names a branch a
  repository does not have is **skipped, not fatal** — the cycle continues with
  the repositories that do have it (`manager/src/services/collect.rs:42-48`).
  This requirement is recorded here rather than in §5.4 because the run kind is
  qa-runs' surface; qa-insights is only its caller, via
  `cpt-cf-qa-fr-insights-expected-cases`.

#### Run parameter validation

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-params`

The system MUST validate run parameters on every launch and re-run: names match `^[A-Za-z_][A-Za-z0-9_]*$`, reserved control-variable names (`APP_BUILD`, `APP_VERSION`, `E2E_K8S_NAMESPACE`, `KUBECONFIG`, `PRODUCT_KEY`, `RP_API_KEY`, `RP_PROJECT`, `SKIP_TESTS_WITH_BUGS`, `TEST_BUNDLE_URL`, `TEST_FILES`, `TEST_VERSION`) are rejected case-insensitively, names are unique, and at most 50 parameters are accepted, each parameter name at most 128 **bytes** and each value at most 8 KiB. Violations MUST fail the launch with a message naming the offending parameter **where one exists**.

**Two corrections, 2026-08-17, from the independent spec-coverage audit.** The cap is measured in bytes, faithfully to the legacy source this requirement freezes; "characters" was observably wrong for a multi-byte name. And the naming clause cannot be unconditional: an empty name and a too-many-parameters refusal structurally have no offending name to report, and an over-long name is reported as an excerpt. That is deliberate and documented in the code; the sentence was simply wrong as written.

- **Rationale**: Existing contract; reserved names protect the runner's control interface from being overridden.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-ci`
- **Amended 2026-08-13 (qa-runs plan, decision D3)**: the two size caps were
  omitted from this requirement's first draft but are enforced by the source
  system (`manager/src/routes/settings.rs:32-34`, applied at `:118-151`), so
  they are part of the ported contract rather than a new constraint. The
  reserved-name list is unchanged and complete: it matches
  `RESERVED_PIPELINE_VARIABLE_NAMES` (`routes/settings.rs:15-27`) name for
  name. Note what that list deliberately does **not** cover: the runner's
  result-callback URL (`VHP_PROGRESS_URL`, `argo.rs:438-441`) and
  `E2E_VHP_BASE_URL` are not reserved in the source system, so a launch
  parameter can replace either. Carried forward as inherited parity; if it is
  ever closed, it is a deliberate divergence and belongs here as one.

#### Environment assembly

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-env-assembly`

The system MUST assemble the run's execution environment in the precedence order: static runner variables → pipeline variables → platform variables → run parameters, where later entries override earlier ones of the same name and nothing overrides reserved control variables.

- **Rationale**: Existing, documented contract with test authors and administrators.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-admin`

#### Exclusivity resolution

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-exclusivity`

The system MUST resolve a run's exclusivity by three-tier precedence — launch choice ?? plan.yaml ?? OR(TEST_META over files that will run, after tag filtering) ?? parallel — where the upper two tiers are three-state (unset means inherit, distinct from false). An explicit false at a higher tier MUST override a lower-tier true.

- **Rationale**: Existing semantics; the three-state design is what lets a suite of exclusive-marked tests be deliberately run in parallel for one launch without editing test files.
- **Actors**: `cpt-cf-qa-actor-engineer`

#### Per-platform queue

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-queue`

The system MUST maintain a per-target-platform FIFO queue: while an exclusive run holds a platform, subsequent runs targeting it queue and start automatically when the platform frees; nothing is rejected for the platform being busy. Runs without a target platform are never queued and never block others. Queued runs MUST be listable and cancellable.

- **Rationale**: Destructive tests must not share a platform; engineers must not babysit launches.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-ci`
- **Amended 2026-08-13 (qa-runs plan, decision D1)**: "nothing is rejected for
  the platform being busy" remains exact — *busy* is never a rejection reason.
  But the queue is bounded in two other ways this requirement omitted, both
  operator settings with `0` meaning disabled: `queue_max_depth` rejects a new
  launch when a platform's queue is already full, and `queue_ttl_seconds`
  expires a row that has waited too long into a terminal `expired` state whose
  alert is mandatory, so a queued run can never disappear silently. Those two
  plus the cluster-wide `max_concurrent_runs` are exactly the limits
  `cpt-cf-qa-fr-runs-launch`'s "rejected with the limit that was hit" refers
  to. See DESIGN §3.7 for the full model and its source-system citations.

#### Dispatch and crash recovery

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-dispatch`

The system MUST persist queue and dispatch state in its database such that after a control-plane restart, held platforms, queued runs, and in-flight executions are recovered without losing or duplicating queued runs.

- **Rationale**: The queue is a promise ("it will start on its own"); a restart must not break it.
- **Actors**: `cpt-cf-qa-actor-engineer`
- **Verification Method**: Integration tests with induced restarts.

#### Run execution

- [ ] `p2` - **ID**: `cpt-cf-qa-fr-runs-execute`

The system MUST execute runs via the platform serverless execution capability: the pytest workload runs as a managed workflow receiving the assembled environment and the test source (bundle reference or repo reference), and the system MUST reflect execution state transitions in the run record. Blocking dependency: serverless-runtime Python workloads (see §10, §12).

- **Rationale**: Removes the Argo/Kubernetes hard dependency; aligns execution with the platform model.
- **Actors**: `cpt-cf-qa-actor-execution-plane`

#### Structured result ingestion

- [ ] `p2` - **ID**: `cpt-cf-qa-fr-runs-results-ingest`

The system MUST ingest typed execution events (run started, test file, test started, test result passed/failed/skipped, launch linked, run finished) and persist run and per-test results incrementally as events arrive, replacing post-hoc log parsing. The run record in the database is the sole source of truth for run state.

**Amended 2026-08-18 (qa-insights design spec, decision D1).** The per-test rows
qa-runs persists, and the `qa.test.result` event `cpt-cf-qa-interface-events`
specifies, MUST additionally carry the pytest **`nodeid`**, the skip/xfail
**`reason`** and the attributed **`ticket`**. They were omitted from the shipped
shape, which cost the analytics surface its entire per-case granularity — see
`cpt-cf-qa-fr-insights-history` in §5.4 for what becomes uncomputable without
them, and the source columns at `manager/migrations/001_initial.sql:253-263`.
The change is **additive**: three new nullable columns and three new optional
event fields. It does not mutate the published `qa.test.result` version, which
`cpt-cf-qa-interface-events` forbids — adding optional fields is not a mutation.
**Corrected 2026-09-01: the rows are what ships.** qa-runs' event publisher was
removed — event-broker has no working consumer, so it published nothing — and
the three columns carry the requirement on the per-test rows alone; the event
half remains the interface contract's, not a running data path.

- **Rationale**: Marker-scraping from logs is fragile and delays results until run completion.
- **Actors**: `cpt-cf-qa-actor-execution-plane`, `cpt-cf-qa-actor-engineer`

#### Cancellation and re-run

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-cancel-rerun`

The system MUST cancel a queued or executing run on request (propagating cancellation to the execution plane for executing runs) and MUST re-run a completed run with its original parameters, re-validating them at re-run time.

- **Rationale**: Basic operational control; re-validation guards against rules changing between launch and re-run.
- **Actors**: `cpt-cf-qa-actor-engineer`

#### Run timeout

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-timeout`

The system MUST enforce a per-run timeout (from plan metadata or launch override, with a configurable default ceiling) in the control plane, cancelling the execution and marking the run timed-out when exceeded.

- **Rationale**: Hung test runs must not hold platforms (and their queues) indefinitely; enforcement cannot rely on the execution backend alone.
- **Actors**: `cpt-cf-qa-actor-engineer`

#### Live and archived logs

- [ ] `p2` - **ID**: `cpt-cf-qa-fr-runs-logs`

The system MUST stream a run's log output live to clients while it executes and MUST retain the complete log via the platform file-storage capability after completion, retrievable for the results retention period.

- **Rationale**: Live logs are the primary debugging tool during a run; archived logs support post-hoc analysis.
- **Actors**: `cpt-cf-qa-actor-engineer`

#### Schedules

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-schedules`

The system MUST manage cron schedules for plans and custom plans, each storing an exclusivity choice (`true`/`false`/`auto`), enabled/disabled state, and launch parameters. Schedule firing MUST use the same internal run-creation path as manual launches (identical validation, naming, metadata, and queue behavior) and MUST fire exactly once per due time across all control-plane instances.

- **Rationale**: One creation path is an existing invariant that keeps scheduled and manual runs indistinguishable downstream; exactly-once firing prevents duplicate destructive runs.
- **Actors**: `cpt-cf-qa-actor-admin`
- **Verification Method**: Integration tests with multiple concurrent scheduler instances.
- **Amended 2026-08-18 (qa-insights design spec, decision D9)**: each schedule
  MUST additionally store **three notification settings** —
  `slack_notifications_enabled`, `slack_channel`, and
  `slack_notification_events` (a subset of the six scheduled-run events listed in
  `cpt-cf-qa-fr-insights-notifications`) — editable independently of the rest of
  the schedule. No document previously recorded per-schedule notification
  configuration at all, yet the source system exposes it at
  `POST /api/schedules/{name}/notifications`
  (`manager/src/routes/mod.rs:95-98`, `manager/src/routes/schedules.rs::api_update_notifications`),
  and qa-insights reads these settings when a scheduled run changes status.
  Editing them MUST leave every other field of the schedule untouched — the
  source system is explicit that a schedule pinned exclusive comes back pinned
  exclusive. They belong on the schedule because a schedule is a qa-runs
  aggregate and a notification setting on it is a field, not a separate entity;
  the source system's delete-and-recreate round-trip is *not* ported, being an
  artifact of storing configuration in CronWorkflow annotations rather than in a
  table.

  The gear exposes this as **`PUT /qa/v1/schedules/{id}/notifications`**, and
  the change of verb is deliberate. Legacy registers `axum::routing::post`
  (`manager/src/routes/mod.rs:95-98` — the design spec cites this as a `PUT` at
  `:96-97`, and is wrong on both counts). `PUT` is correct here because the
  operation is a full, idempotent replacement of a settings sub-resource, and
  because nothing freezes it to `POST`: the frozen contract is the *test-facing*
  one — environment variable names, `plan.yaml`, TEST_META, per
  `cpt-cf-qa-fr-migration-runner-contract` — and no requirement freezes an HTTP
  verb or a path. This route already moves its prefix from `/api` to `/qa/v1` and
  its key from `{name}` to `{id}`; a client that survives those survives the
  verb. See DESIGN §3.3, which records the same reasoning at length so it is not
  re-litigated a third time.

#### Lifecycle events

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-events`

The system MUST publish typed lifecycle events (run created, queued, started, finished, canceled; test result; schedule fired) to the platform event capability, with schemas registered in the type system.

- **Rationale**: Decouples insights and future CI consumers from the run orchestrator; enables integration without new endpoints.
- **Actors**: `cpt-cf-qa-actor-ci`

### 5.3 Environments

#### Target platform registry

- [x] `p1` - **ID**: `cpt-cf-qa-fr-env-platforms`

The system MUST manage target platforms: metadata (name, description, product association, availability state) in the gear database and access credentials (kubeconfig) in the platform credential store, referenced by ID. Credential material MUST never be persisted in the gear database or returned by any API.

- **Rationale**: Platforms are where tests execute; the secret-reference posture matches the current deployment (K8s secrets) and the platform security model.
- **Actors**: `cpt-cf-qa-actor-admin`

#### Platform and pipeline variables

- [x] `p1` - **ID**: `cpt-cf-qa-fr-env-variables`

The system MUST manage per-platform variables and global pipeline variables that participate in environment assembly (`cpt-cf-qa-fr-runs-env-assembly`).

- **Rationale**: Per-environment configuration (endpoints, namespaces) without editing tests or plans.
- **Actors**: `cpt-cf-qa-actor-admin`

#### Platform lease state

- [x] `p1` - **ID**: `cpt-cf-qa-fr-env-lease`

The system MUST track each platform's occupancy (free, held by run(s), held exclusively) for queue decisions, and MUST expose it for display.

- **Rationale**: The queue's correctness depends on authoritative occupancy state; engineers need to see why a run is waiting.
- **Actors**: `cpt-cf-qa-actor-engineer`

#### Platform version polling

- [ ] `p2` - **ID**: `cpt-cf-qa-fr-env-version-poll`

The system MUST periodically poll registered platforms for their deployed product version and record changes, publishing a version-changed event. p2 because probing a target platform requires the execution plane (control-plane gears never connect to targets — see DESIGN §3.5).

- **Rationale**: Version awareness drives result interpretation — the recorded version is the analytics dimension that says what was under test.
- **Actors**: `cpt-cf-qa-actor-admin`

### 5.4 Insights

#### Per-test history

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-history`

The system MUST maintain queryable per-test result history across runs at **two granularities**, both kept current with qa-runs' own run and test-result records:

**Corrected 2026-09-01**: this requirement previously said "populated by consuming run lifecycle and test-result events", naming the event-broker consumer as the mechanism. event-broker registered no client, so that consumer never ran and has since been deleted; qa-insights keeps this history current via its own reconcile sweep polling qa-runs directly. The requirement is restated by outcome rather than mechanism because the outcome — queryable, current per-test history — is what this MUST is actually about, and the previous wording named a mechanism this gear no longer has.

1. **Per test file, per run** — status, duration, the run, the target platform, the product version, the ReportPortal launch reference, and any linked JIRA key.
2. **Per test case (test function), per run** — the pytest `nodeid`, the case name, status, duration, the skip/xfail `reason`, and the `ticket` the runner attributed the case to.

**Amended 2026-08-18 (qa-insights design spec, decision D1).** This requirement
previously named a single granularity — "(status, duration, run, platform,
product version)" — and the gear architecture followed it: `qa-runs` shipped one
merged `qa_run_test_results` table with no `nodeid`, no `reason` and no `ticket`.
The source system has always stored both. `test_results` is one row per test
*file* per run (`manager/migrations/001_initial.sql:65`, with `test_file` and
`duration` added by the later `ALTER TABLE`s at `:166-167`); `test_case_results`
is one row per test *function* per run (`:253`), parsed from the runner's
`TEST_CASE` markers, and the migration comment above it states the purpose
outright: *"Lets analytics aggregate per-case, not just per-file."*

The single granularity is not a smaller version of the requirement, it is a
different one. Without the case rows,
`OverviewSummary.case_total / case_passed / case_failed / case_skipped /
case_xfail / case_xpass` (`manager/src/routes/analytics.rs:89-109`) cannot be
computed, `AnalyticsListItem.case_status` and `case_tickets` (`:112-133` — the
per-file dot colour and ticket badges the UI renders without a second fetch)
have no source, and the xfail/xpass distinction disappears from the product
entirely. Restoring both granularities is therefore parity, not scope growth.

One consequence crosses a gear boundary and is recorded in §5.2 rather than
here: `qa-runs` gains `nodeid`, `reason` and `ticket` on its own per-test rows,
**additively** — see `cpt-cf-qa-fr-runs-results-ingest`, corrected there: the
`qa.test.result` event is the interface contract's, not a running data path.

- **Rationale**: Flakiness and regression analysis need test-level granularity across runs; per-case granularity is what makes xfail/xpass visible and lets a file's ticket badges render without a second fetch.
- **Actors**: `cpt-cf-qa-actor-engineer`

#### Dashboard and coverage

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-dashboard`

The system MUST provide:

- a **dashboard** aggregate (recent runs, run and test counts, pass rates, active and queued runs) at `GET /qa/v1/dashboard`;
- a **coverage** view (which tests and plans ran against which product versions and platforms) at `GET /qa/v1/dashboard/coverage`;
- an **analytics overview** payload at `GET /qa/v1/analytics/overview` that returns, in one response, every one of these sections: `summary`, `lists` (`passed` / `failed` / `not_run`), `heatmap`, `trend`, `build_distribution`, `flaky`, `quality_vectors`, and `grouped` (by `component`, by `tag`, by `platform`), alongside the echoed query context (`product_id`, `product_key`, `version`, `scope`, `plan_id`, `branch`, `group_by`, `group_value`).

**Amended 2026-08-18 (qa-insights design spec, decision D3).** The previous
wording was "dashboard aggregates (recent runs, pass rates, active/queued runs)
and coverage views", which describes roughly a fifth of what the source system
returns and gives an implementer no way to know that heatmap, trend, build
distribution, flaky detection, quality-vector rollups and three independent
groupings are part of the same payload. The authority is
`AnalyticsOverviewResponse` (`manager/src/routes/analytics.rs:231-248`); the
grouping triple is `GroupedSummaries` (`:211-215`) and the vector rollup is
`QualityVectorSummary` (`:224-228`).

**A count correction, recorded because it propagated.**
`AnalyticsOverviewResponse` carries **eight computed sections** — the eight
enumerated above — plus eight echoed query fields. It was described during
drafting as a nine-section payload, and the qa-insights design spec of
2026-08-18 still says "nine" (§2, Finding 3). The enumerated *names* are the
same either way and nothing behavioural turns on the number, but the count is
fixed here so a reader who counts the list is not left hunting for a ninth
section that does not exist.

- **Rationale**: The at-a-glance entry point of the current tool. Shipping eight of these sections looks complete in a screenshot and is not; enumerating them is what makes the requirement testable.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-admin`

#### Analytics with saved views

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-analytics`

The system MUST expose the following analytics surface:

| Endpoint | Purpose |
|---|---|
| `GET /qa/v1/analytics/overview` | The full overview payload of `cpt-cf-qa-fr-insights-dashboard` |
| `GET /qa/v1/analytics/build-tests` | Per-build test breakdown behind the build-distribution section |
| `GET /qa/v1/analytics/export` | CSV/JSON projection of a named overview section |
| `GET/POST /qa/v1/analytics/views`, `PUT/DELETE /qa/v1/analytics/views/{id}` | Saved-view CRUD |
| `GET /qa/v1/analytics/plan/{plan_id}/tests` | Plan drill-down: tests |
| `GET /qa/v1/analytics/plan/{plan_id}/builds` | Plan drill-down: builds |
| `GET /qa/v1/analytics/plan/{plan_id}/test-history` | Plan drill-down: per-test history |

Saved views MUST be persisted per **owner**, per **scope** and per **plan**: a
global view and a plan-scoped view MAY share a name, two global views of one
owner MAY NOT, and one user's views MUST NOT appear in or collide with another's.

**Query conventions — the split (amended 2026-08-18, decisions D3 and D7).** The
previous wording required "the platform's standard collection query conventions"
(OData) across this surface, and §1.2 / `cpt-cf-qa-nfr-scale` named OData as the
mitigation for unbounded row growth. That is right for collections and wrong for
aggregates, so the requirement now splits:

- **Legacy parameters, verbatim, on the aggregates.** The overview, export,
  build-tests and the three plan drill-downs are *computed aggregates, not
  collections*. Their parameter set is fixed and frozen —
  `product_id`, `version`, `scope`, `plan_id`, `branch`, `days_heatmap`,
  `days_trend`, `group_by`, `group_value` (`AnalyticsOverviewQuery`,
  `manager/src/routes/analytics.rs:18-33`), plus `build` on build-tests. OData
  over a computed rollup is meaningless, and changing these parameters breaks
  every existing SPA call.
- **OData on the two flat collections only.** `GET /qa/v1/test-results` and
  `GET /qa/v1/test-case-results` are the unbounded row stores that
  `cpt-cf-qa-nfr-scale`'s 5 M-row target is actually about, and they carry
  OData filtering, sorting and paging with the subsystem's shared page-size clamp.

The saved-view shape is also corrected here: it is keyed on
`(owner_id, scope, plan_id, name)`, not on `name` alone
(`analytics_saved_views`, `manager/migrations/001_initial.sql:183`, unique index
at `:194` on `(owner_id, scope, COALESCE(plan_id, ''), name)` — the `COALESCE` is
what lets a global and a plan-scoped view of the same name coexist). DESIGN §3.7
carries the ported table.

- **Rationale**: Parity with the current analytics pages. Standard query conventions replace bespoke filter parameters *where the endpoint is a collection*; where it is an aggregate, the bespoke parameters are the contract the SPA already speaks.
- **Actors**: `cpt-cf-qa-actor-engineer`

#### Expected test-case counts

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-expected-cases`

The system MUST compute an "expected cases" number for the analytics universe by
**two** independent means, and MUST surface it as `summary.case_expected` on the
overview payload:

1. **Static, always available.** A per-test-file count of declared test cases,
   derived from file content without executing anything. It covers **two file
   flavours**, both of which the source system counts: pytest `def test_*`
   functions and methods (including `async def`), and Playwright specs' `test(…)`
   calls including the `.only` / `.skip` / `.fixme` / `.fail` variants but not
   `test.describe(…)` suites. It MUST NOT expand `@pytest.mark.parametrize` or
   otherwise generated cases, and is therefore an explicit lower bound.
2. **Exact, on demand and on a schedule.** A collect-only run that executes
   `pytest --collect-only` with parametrize expanded and POSTs per-file counts
   back to the control plane, keyed by `(repository, branch, test file)`. It MUST
   run on demand from the analytics UI and on a periodic cycle over a configured
   default branch (`main` in the source system,
   `manager/src/services/collect.rs:19`, with the poller interval floored at
   300 s at `:184`), and re-reporting a file MUST replace its count rather than
   accumulate. A negative reported count clamps to zero and an empty file name is
   rejected, so a runner bug cannot become a permanently wrong number.

**The runner-facing contract is preserved exactly.** A collect-only execution
receives exactly two environment variables — `COLLECT_ONLY=true` and
`VHP_COLLECT_URL=<url>`, the latter only when a URL was supplied
(`manager/src/services/argo.rs:50-59`). The URL is **built by the control plane**
(`manager/src/services/collect.rs:90`,
`format!("{}/api/collect/{}/{}", base, repo.id, branch)`), so the runner posts
wherever it is told; re-homing the report route onto the insights gear changes
nothing the runner can observe. Both variable names are frozen under
`cpt-cf-qa-fr-migration-runner-contract` (§5.6), which is amended below to name
them; its interface counterpart is `cpt-cf-qa-contract-runner` in §7.2.

*(Historical naming note, for anyone reading a draft or a branch from before
2026-08-18: this constraint was cited for a while under an id spelled
"cpt-cf-qa-nfr-test-contract" — written here without backticks because no
requirement of that id has ever existed in this document and a link checker
should not chase it. Every such citation was corrected on 2026-08-18 to
`cpt-cf-qa-fr-migration-runner-contract`.)*

**New requirement, 2026-08-18 (qa-insights design spec, decision D2).** Neither
`case_expected` nor the collect mechanism appeared in any specification document,
in either the FR list or the schema, although the source system implements both:
the static count is `count_test_functions`
(`manager/src/routes/analytics.rs:1865`, called from `parse_test_meta` at
`:1882`) reading file content off the on-disk checkout — and its doc comment at
`:1860-1864` is the authority for the two file flavours above; the qa-insights
design spec describes it as counting only `def test_*`, which would silently drop
every Playwright spec — and the exact count is
the `test_case_collect` table (`manager/migrations/001_initial.sql:272`) filled
by collect-only workflows. The omission mattered because the two halves have
**different owners** in the four-gear split, and an unrecorded requirement gets
no owner at all: the static count needs a repository checkout, which is
qa-catalog's alone (`cpt-cf-qa-adr-git-egress`), and the exact count needs an
executor, which is qa-runs'. The split is therefore: qa-catalog projects
per-file test-function counts over its SDK (it already parses those files for
TEST_META, so this is a projection of work it does anyway); qa-runs gains a
`Collect` run kind (see §5.2); qa-insights triggers it, owns the report route
and owns the counts table.

- **Rationale**: "42 of 50 expected cases ran" is the number that tells an engineer a run silently collected fewer tests than the repository declares — a class of failure no pass/fail count can show. The two sources exist because one is always available and approximate, and the other is exact but costs a run.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-execution-plane`

#### JIRA bug tracking

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-jira`

The system MUST maintain an **insights-owned bug registry** keyed on test identity (test file/name plus the plan it ran under, with optional product-version and platform qualifiers), MUST let a user link or file a bug against a specific failed test from a run view, MUST poll linked bugs' status via the platform outbound gateway, and MUST expose open-bug views. When a launch requests skip-tests-with-bugs, tests with open linked bugs MUST be skipped and reported as skipped.

The registry — not TEST_META — is the source of truth for linkage. An earlier draft sourced links "from TEST_META references", which the source system never did: linkage is a `jira_bugs` row (`JiraBug { jira_key, test_name, plan_id, app_version, platform, status, … }`, VHP Test Runner `manager/src/models.rs:687-700`, uniquely keyed on `jira_key` and indexed on `(test_name, plan_id)`), created by a UI action carrying only the test name (`JiraCreateRequest { test_name }`, `manager/src/routes/settings.rs:580-631`), and delivered to the runner as the `SKIP_TESTS_WITH_BUGS` environment variable — a comma-separated `test_name:JIRA-KEY` list (`manager/src/services/argo.rs:476-481`). The optional TEST_META `bugs` key described in `cpt-cf-qa-fr-catalog-test-meta` MAY be surfaced as an additional hint, but MUST NOT be required for this requirement to be met.

- **Rationale**: Known-failure suppression keeps signal in scheduled runs; parity with the current JIRA integration. Keying on test identity rather than on file contents is what lets a bug be filed from a failure the moment it happens, without a commit to the test repository.
- **Actors**: `cpt-cf-qa-actor-jira`, `cpt-cf-qa-actor-engineer`

#### Auto-rerun on bug resolution

- [ ] `p2` - **ID**: `cpt-cf-qa-fr-insights-auto-rerun`

The system MUST optionally re-run affected tests automatically when a linked
JIRA bug transitions to a resolved state **and a newer build is available for the
affected plan**, using the standard launch path. Both conditions are required;
either alone MUST NOT trigger a re-run.

**Amended 2026-08-18 (qa-insights design spec, decision D8).** This requirement
recorded only the resolution transition. The source system gates the re-run on
two conditions in sequence: the `auto_rerun_on_resolve` operator setting
(`manager/src/services/jira_poller.rs:61-63`) and then `check_new_build(...)`
(`:65-70`), which skips the re-run when no newer build exists for the bug's plan
and app version. Omitting the second condition would re-run the same failing
build on every poll of an already-resolved bug — a loop the source system
deliberately does not have.

*(Citation correction: the design spec cites `jira_poller.rs:62-70` for this
gate. That range opens inside the body of the `auto_rerun_on_resolve` guard; the
two conditions are at `:61-63` and `:65-70` respectively.)*

"Using the standard launch path" is confirmed rather than amended, and is worth
pinning because a stale comment contradicts it: `manager/src/services/argo.rs:369-372`
claims that `collect` and `jira_poller` are "the two paths that bypass
admission". For `jira_poller` that half is **wrong and out of date** — the module
header at `manager/src/services/jira_poller.rs:8-15` and the function comment at
`:92-97` both state the opposite, citing VHP-2618: an auto-rerun is an ordinary
launch, subject to the global cap, the platform queue and tier-resolved
exclusivity. The gear MUST NOT reproduce a bypass the source system removed.

- **Rationale**: Closes the loop on known failures without manual tracking. The new-build condition is what stops the loop from re-running a build already known to fail.
- **Actors**: `cpt-cf-qa-actor-jira`

#### Run notifications

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-notifications`

The system MUST provide a configurable run-notification surface over **two
channels**, Slack and email, with a single per-tenant configuration:

- **Slack** — both delivery forms the source system has: an incoming-webhook post
  and a post to a named channel, including Block Kit block rendering and the
  scheduled-run message templates with their placeholder and conditional
  expansion. Configuration fields are those of `NotificationsConfig`
  (`manager/src/models.rs:1338-1362`): `slack_webhook_url`, `slack_channel`,
  `manager_ui_base_url`, `slack_enabled`, `notify_on_failure`,
  `notify_on_success`, `notify_on_schedule_completion`,
  `scheduled_run_slack_enabled`, `scheduled_run_slack_templates`,
  `run_queue_queued_slack_enabled`.
- **Email** — `email_smtp_host`, `email_smtp_port`, `email_from`,
  `email_recipients`, `email_enabled`, with run status, counts and links in the
  body. See the deferral below.

It MUST cover these events:

- **Scheduled-run status**, six events: `Pending`, `InProgress`, `Succeeded`,
  `Failed`, `Error`, `Skipped` (`ScheduledRunNotificationEvent`,
  `manager/src/models.rs:952-959`). Which of them a given schedule announces is a
  per-schedule setting owned by qa-runs — see `cpt-cf-qa-fr-runs-schedules` in §5.2.
- **Run-queue lifecycle**, two events (`QueueNotificationEvent`,
  `manager/src/models.rs:1393-1399`): `Queued`, which is opt-in and off by
  default because a busy platform produces many of them; and `Expired`, which is
  **mandatory and MUST NOT be made toggleable** — the model documents it as
  *"the event that stops a run vanishing silently."*

It MUST also maintain:

- a **dedupe** record keyed on `(run identity, notification kind, event type)`,
  so one send happens per event even under concurrent attempts
  (`run_notifications`, whose composite primary key *is* the dedupe mechanism,
  `manager/migrations/001_initial.sql:197-203`);
- a **notification audit log** recording every attempt with its channel, event
  type, outcome and failure detail, readable at
  `GET /qa/v1/settings/notifications/log` (`notification_log`, `:205`);
- **test-send** and **preview** operations for both the general and the
  scheduled-run message forms.

A send failure MUST be logged and swallowed, never propagated to a caller, and
outbound requests MUST carry a bounded total-request timeout — the source system
fixes 10 s and explains why at `manager/src/services/notifications.rs:53-66`: the
mandatory `Expired` notification is sent inline from the dispatcher tick, so an
unbounded request against a black-holing webhook host would stall the whole
queue.

Slack and email egress go through the platform outbound gateway.
`cpt-cf-qa-contract-egress` admits no second exception beyond the recorded git
one (§7.2); unlike git, a webhook POST is an ordinary request the gateway can
express. Migration to a platform Notifications Service remains a p3 convergence,
non-blocking.

**Amended 2026-08-18 (qa-insights design spec, decision D5).** The previous
wording was "configurable email notifications on run completion (with status,
counts, and links)". The source system is Slack-*first*, and far wider: the
notification service alone is ~1.9 kLOC
(`manager/src/services/notifications.rs` — `send_slack:97`,
`send_slack_to_channel:102`, `send_slack_to_channel_with_blocks:113`,
`send_email:133`, `notify_run_completed:180`, `send_test_notification:358`,
`preview_scheduled_run_message:388`,
`send_scheduled_run_test_notification:407`, `notify_scheduled_run_status:431`,
`get_notification_log:622`, `notify_queue_event:657`), and two supporting tables
plus four settings routes (`manager/src/routes/mod.rs:270-286`) appeared in no
specification document. Building to the old wording would have shipped a gear
with no Slack surface at all.

*(Field-count correction: the design spec describes `NotificationsConfig` as
having sixteen fields. It has fifteen, all enumerated above.)*

**Deferred, and recorded rather than dropped — decision D10 (2026-08-18).** The
email **send** is deferred because the platform has no SMTP egress: oagw speaks
HTTP, SSE and WebSocket only (`ServiceGatewayClientV1::proxy_request`'s
protocol-mapping table, `gears/system/oagw/oagw-sdk/src/api.rs:143-156`, which
enumerates exactly those three, with gRPC marked "future use" at
`gears/system/oagw/oagw-sdk/src/models.rs:292-297`); `lettre` appears in no
`Cargo.toml` in this workspace; and `gears/system/` contains sixteen gears, none
of them a notifications gear. Everything else
about email ships and is tested: the configuration surface, the routing decisions
that select email recipients, the dedupe claim and the audit-log entry. The mail
client is a domain port whose only adapter records outcome `unsupported_egress`
in the log and returns success; when the platform grows an SMTP path the adapter
is swapped behind the port and no domain code changes. Slack ships fully working.
Rejected: a second `cpt-cf-qa-contract-egress` exception for a capability the
platform has committed to solving centrally; and dropping email outright, which
would be a functional regression against the source system, which sends SMTP
directly with `lettre` (`manager/src/services/notifications.rs:11`, `send_email`
at `:133`).

- **Rationale**: Parity; scheduled-run failures must reach people who did not launch them, and a queued run that expires must never disappear silently.
- **Actors**: `cpt-cf-qa-actor-notification`

#### ReportPortal links

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-reportportal`

The system MUST store the ReportPortal launch reference reported by the runner for a run and render launch links in run views. ReportPortal remains optional and is never a source of truth.

- **Rationale**: Parity with the current optional integration.
- **Actors**: `cpt-cf-qa-actor-reportportal`

### 5.5 User Interface

#### Embedded SPA delivery

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-ui-embedded`

The system MUST deliver the QA web UI as a gear that serves the built React SPA from embedded assets (feature-gated), with SPA-fallback routing, so any host binary composing the subsystem serves the UI without external web infrastructure.

- **Rationale**: Makes the UI a composable platform artifact; required for single-node deployments.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-admin`

#### UI functional parity

- [ ] `p2` - **ID**: `cpt-cf-qa-fr-ui-parity`

The adapted UI MUST cover the current SPA's functional surface — dashboard, runs (including queued cards and live logs), plans, custom plans, schedules, platforms, products, analytics, settings — against the new API, with platform token-based authentication and live log delivery via server-sent events.

- **Rationale**: The UI is the primary interface; parity defines "done" for the migration.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-admin`
- **Verification Method**: E2E UI test suite covering each page's primary flow.

### 5.6 Migration

#### Data migration

- [ ] `p2` - **ID**: `cpt-cf-qa-fr-migration-data`

The system MUST provide a one-shot migration tool importing an existing VHP Test Runner PostgreSQL database (products, versions, repositories, custom plans, platforms metadata, run and test results, JIRA bugs, saved views) into the subsystem's per-gear schemas, assigning imported rows to a designated tenant and reporting unmigratable records.

- **Rationale**: The existing team's history (results, analytics) is a first-class asset.
- **Actors**: `cpt-cf-qa-actor-admin`
- **Verification Method**: Migration run against a production-shaped database snapshot; row-count and spot-check reconciliation report.

#### Runner contract preservation

- [ ] `p2` - **ID**: `cpt-cf-qa-fr-migration-runner-contract`

The test-facing execution contract MUST be preserved: environment variable names consumed by tests (`KUBECONFIG`, `APP_VERSION`, `TEST_VERSION`, `PRODUCT_KEY`, `SKIP_TESTS_WITH_BUGS`, `COLLECT_ONLY`, `VHP_COLLECT_URL`, ReportPortal variables, run parameters), plan.yaml format, and TEST_META format are unchanged, so existing test repositories run unmodified.

**Amended 2026-08-18 (qa-insights design spec, decision D2)**: `COLLECT_ONLY` and
`VHP_COLLECT_URL` were missing from this list. They are as runner-facing and as
frozen as the rest — the collect-only workflow reads both
(`manager/src/services/argo.rs:50-59`) — and their omission mattered because
`cpt-cf-qa-fr-insights-expected-cases` depends on the names not moving while the
*route* they point at moves onto a different gear. The URL is supplied by the
control plane, so re-homing the route is invisible to the runner; renaming the
variable would not be.

- **Rationale**: Goal §1.3; the repositories are owned by many authors and cannot be migrated in lockstep.
- **Actors**: `cpt-cf-qa-actor-test-repo`
- **Verification Method**: A reference test repository from the current system executed on the new plane with equivalent results.

### 5.7 Packaging & Deployment

#### Deployment packaging

- [ ] `p2` - **ID**: `cpt-cf-qa-fr-deploy-packaging`

The subsystem MUST be deliverable in all three platform deployment shapes: (a) a reference host binary composition (gears + required platform gears) runnable directly for single-node use, (b) a container image of that binary, and (c) a Helm chart for the Kubernetes shape covering the control plane, its configuration/secrets, and database dependency. Manifest content is a design/packaging concern (DESIGN §3.8), not specified here.

- **Rationale**: Replaces the legacy multi-image chart (`charts/vhp-testrunner`); operators need a supported path from `cargo run` to production Kubernetes.
- **Actors**: `cpt-cf-qa-actor-admin`
- **Verification Method**: Single-node smoke run of the reference binary; Helm install of the chart on a clean cluster with the e2e suite passing against it.

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide NFRs (security, availability, observability, API conventions, error taxonomy) are defined in the [Architecture Manifest](../../../docs/ARCHITECTURE_MANIFEST.md) and [guidelines/](../../../guidelines/README.md) and apply unchanged — including tenant isolation via `SecurityContext`/`AccessScope`/SecureConn on every entity and endpoint, canonical RFC-9457 errors, and OpenAPI publication. Only deltas are listed here.

### 6.1 Gear-Specific NFRs

#### Long-running execution support

- [ ] `p2` - **ID**: `cpt-cf-qa-nfr-run-duration`

The system MUST support runs executing continuously for up to 8 hours (configurable ceiling), surviving control-plane restarts mid-run without losing run state or log continuity.

- **Threshold**: 8 h default ceiling; run state and result ingestion recover within 60 s of control-plane restart.
- **Rationale**: E2E suites against real clusters routinely run for hours — far outside typical platform request lifetimes.
- **Architecture Allocation**: See DESIGN § NFR Allocation.

#### Result ingestion latency

- [ ] `p2` - **ID**: `cpt-cf-qa-nfr-result-latency`

A test result event MUST be visible in run detail queries within 5 s of emission by the runner (p95).

- **Threshold**: ≤ 5 s p95, event emission → API visibility.
- **Rationale**: Engineers watch runs live; the current tool only shows results after completion, and this is an explicit improvement target.

#### Dispatch latency

- [ ] `p1` - **ID**: `cpt-cf-qa-nfr-dispatch-latency`

A queued run MUST start (execution requested) within 10 s of its platform becoming free (p95).

- **Threshold**: ≤ 10 s p95, platform release → execution request.
- **Rationale**: "Starts on its own" is only credible if the delay is negligible against run durations.

#### Live log latency

- [ ] `p2` - **ID**: `cpt-cf-qa-nfr-log-latency`

Log lines MUST reach connected streaming clients within 2 s of being produced by the runner (p95).

- **Threshold**: ≤ 2 s p95 end-to-end.
- **Rationale**: Live debugging of hung tests.

#### Scale envelope

- [ ] `p1` - **ID**: `cpt-cf-qa-nfr-scale`

The subsystem MUST handle: 100 registered target platforms, 50 concurrently executing runs, 5,000 test files per catalog, 10,000 per-test results per run, and 5 million retained test-result rows, with collection endpoints meeting project-default latency baselines at that volume.

- **Threshold**: As enumerated; verified by seeded load tests.
- **Rationale**: Roughly 10× the current single-team deployment, allowing multi-team platform adoption without redesign.

#### Scheduler correctness

- [ ] `p1` - **ID**: `cpt-cf-qa-nfr-scheduler-exactly-once`

Across N concurrent control-plane instances, each due schedule tick MUST produce exactly one launch (no duplicates, no misses) under instance failover.

**Unmet as specified — recorded 2026-08-17 by the independent spec-coverage audit; needs an approved deviation or closing work.** The no-duplicates half is discharged by a unique index on `(tenant_id, schedule_id, due_at)`. The **no-misses half is not satisfied under the very condition this sentence names**: an orphaned claim from a departed instance produces no run at all, and a shipped test asserts that as correct. Four further miss vectors exist in the implementation (post-claim launch failure, a full queue, per-tick fire cap starving the id-ordered tail, an undecodable row), and the adopted catch-up policy is a fifth by design — a control plane down longer than one period skips the intervening occurrences rather than back-filling them. **The tension is real rather than accidental**: a retry after a committed claim is exactly what the no-duplicates half forbids, so reconciling the two is a design decision. See DECOMPOSITION 2.4.

- **Threshold**: Zero duplicate/missed firings in failover integration tests.
- **Rationale**: Duplicate destructive runs are dangerous; missed release-gate runs are silent quality regressions.

### 6.2 NFR Exclusions

- **Internationalization**: Not applicable — engineering tool with an English-only UI, matching the current product; revisit if the platform adopts an i18n baseline.
- **Offline capability**: Not applicable — server-side system; the UI requires connectivity to the control plane by nature.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Gear SDK crates

- [ ] `p1` - **ID**: `cpt-cf-qa-interface-sdks`

- **Type**: Rust SDK crates — `qa-catalog-sdk`, `qa-runs-sdk`, `qa-environments-sdk`, `qa-insights-sdk`
- **Stability**: unstable until subsystem GA, then stable
- **Description**: Typed client interfaces, transport-agnostic models, and error types for each gear, resolved via ClientHub; the only permitted inter-gear surface.
- **Breaking Change Policy**: Per repository versioning policy; breaking changes require major version bump after GA.

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-qa-interface-rest`

- **Type**: REST under `/qa/v1/...`, registered via OperationBuilder, published in the gateway OpenAPI document; SSE for live logs.
- **Stability**: unstable until subsystem GA
- **Description**: Full control surface: catalog, runs/queue/schedules, environments, insights, UI assets. Endpoint inventory in DESIGN §3.3.
- **Breaking Change Policy**: Versioned path segment; breaking changes ship as `/qa/v2`.

#### Lifecycle event contracts

- [ ] `p1` - **ID**: `cpt-cf-qa-interface-events`

- **Type**: GTS-registered event schemas (`qa.run.*`, `qa.test.result`, `qa.schedule.fired`, `qa.platform.version_changed`) published via the platform event capability.
- **Stability**: unstable until subsystem GA
- **Description**: The integration surface for insights and external consumers (CI).
- **Breaking Change Policy**: New schema versions registered in the type system; existing versions never mutated.
- **Corrected 2026-09-01**: Not currently published. event-broker registers no client, so qa-runs' publisher was a logged no-op; the dead code has been removed. qa-insights ingests via the reconcile sweep over qa-runs' SDK instead. This entry records the contract, not a running data path.

### 7.2 External Integration Contracts

#### Runner execution contract

- [ ] `p2` - **ID**: `cpt-cf-qa-contract-runner`

- **Direction**: provided by the subsystem to the runner workload; consumed by test repositories transitively
- **Protocol/Format**: environment variables (test-facing, preserved verbatim from the current system) + typed execution events (runner → control plane, replacing stdout markers)
- **Compatibility**: test-facing env contract frozen (see `cpt-cf-qa-fr-migration-runner-contract`); event schema versioned via GTS.

#### JIRA and SMTP egress (and the scoped git exception)

- [ ] `p1` - **ID**: `cpt-cf-qa-contract-egress`

- **Direction**: required from environment (external JIRA REST API, SMTP relay), reached exclusively via the platform outbound gateway
- **Protocol/Format**: JIRA REST; SMTP
- **Compatibility**: JIRA API version pinned in gear config; no direct socket egress from gears, with **one scoped exception** (below).

**Scoped exception — git egress from qa-catalog.** Amended by `cpt-cf-qa-adr-git-egress` (2026-08-12; **widened 2026-08-27 to cover SSH**). Git clone/fetch/ls-remote against registered test repositories is performed in-process by qa-catalog's `gix` infra adapter, which opens sockets directly and therefore does not satisfy the general prohibition above. The exception was accepted because the git smart protocol is a stateful pkt-line exchange that the outbound gateway's request-centric egress contract cannot express, and the gateway has no git tunnelling capability on its roadmap; the ADR records the alternatives considered and rejected. The exception is bounded, and the boundaries are part of this contract:

- **Protocols**: git over **HTTPS/HTTP** and over **SSH** (clone, fetch, ls-remote), in both the `ssh://` and scp-like `user@host:path` forms. `file://`, `git://` and bare local paths remain refused. *(SSH was excluded until 2026-08-27 on the grounds that it "would require materializing key material on disk"; that premise was retired, not waived — see below.)*
- **Gear**: qa-catalog only, and only from `qa-catalog/src/infra/git/` behind the `RepoSyncPort` domain port.
- **Credentials**: resolved from credstore and passed to the git engine in memory; never written to disk, embedded in a URL, or logged. This holds for SSH too: the **private** key is loaded into a short-lived per-sync `ssh-agent` over **stdin**, so it never becomes a file, never enters `argv`, and never enters an environment variable. (The identity's **public** key is written to the per-sync directory, because `ssh` will only offer an agent identity that an `IdentityFile` names; a public key is not a secret — it is sent to the server in the clear during authentication — and the signature is still produced by the agent.) The general "no credential on disk" rule of this contract is therefore preserved unchanged by the widening — which is the reason the widening is admissible at all.
- **Runtime-image dependency**: the SSH path shells out to `ssh`/`ssh-agent`/`ssh-add`, so an image serving SSH remotes must ship `openssh-client`. Git itself is still never shelled out to. This cost is recorded in `cpt-cf-qa-adr-git-egress`.
- **Host key verification is disabled** on the SSH path by explicit decision of the operating organization (`StrictHostKeyChecking=no`, `UserKnownHostsFile=/dev/null`), which means no remote's identity is pinned and repository content can be attacker-chosen on a hostile network path. The risk, its containment to a single code site, and what tightening requires are recorded in `cpt-cf-qa-adr-git-egress`.
- **Everything else is unchanged**: JIRA and SMTP remain gateway-only, and no other gear or protocol inherits this exception. If the outbound gateway grows git tunnelling or a generic stream-egress capability, the exception is withdrawn and the adapter swapped behind the port.

## 8. Use Cases

#### UC-001: Launch a run that starts immediately

- [ ] `p1` - **ID**: `cpt-cf-qa-usecase-launch-immediate`

**Actor**: `cpt-cf-qa-actor-engineer`

**Preconditions**: A discovered plan; a free registered platform.

**Main Flow**:
1. Engineer launches the plan against the platform with two run parameters.
2. System validates parameters, resolves exclusivity (no launch choice, no plan flag, no TEST_META exclusive → parallel), finds the platform free.
3. System creates the run, assembles the environment, requests execution, returns "started" with the run identifier.
4. Engineer watches live logs; per-test results appear incrementally; run completes and a completion event is published.

**Postconditions**: Run record complete; per-test results persisted; notification sent if configured.

**Alternative Flows**:
- **Invalid parameter**: Launch rejected with a message naming the parameter; nothing is created.

#### UC-002: Exclusive run queues behind a busy platform

- [ ] `p1` - **ID**: `cpt-cf-qa-usecase-queue`

**Actor**: `cpt-cf-qa-actor-engineer`

**Preconditions**: A plan whose test files include one with `"exclusive": True`; a platform currently running a parallel run.

**Main Flow**:
1. Engineer launches with exclusivity "Auto (from tests)".
2. System aggregates TEST_META with OR after tag filtering → exclusive; the platform is occupied.
3. System queues the run and returns "queued" with a queue identifier.
4. The parallel run finishes; the dispatcher starts the queued run within the dispatch-latency threshold; the run holds the platform exclusively.
5. A run launched by another engineer against the same platform queues behind it.

**Postconditions**: FIFO order preserved; no run was rejected for busyness.

**Alternative Flows**:
- **Cancellation while queued**: The queued run is removed; queue order of others is unchanged.
- **Control-plane restart while queued**: After restart, queue state is recovered and dispatch proceeds (`cpt-cf-qa-fr-runs-dispatch`).

#### UC-003: Scheduled run fires exactly once

- [ ] `p1` - **ID**: `cpt-cf-qa-usecase-schedule`

**Actor**: `cpt-cf-qa-actor-admin`

**Preconditions**: An enabled cron schedule for a plan with stored exclusivity choice `auto`; two control-plane instances running.

**Main Flow**:
1. The cron time arrives; the leader instance fires the schedule.
2. The launch goes through the standard creation path with the schedule's stored choice arriving as the launch tier.
3. Exactly one run is created (started or queued per platform state); a schedule-fired event is published.

**Postconditions**: One run; schedule's last-fired state updated.

**Alternative Flows**:
- **Leader failover at fire time**: The new leader fires the missed-or-due tick once; no duplicate.

#### UC-004: Known bug suppresses a failing test, resolution re-runs it

- [ ] `p2` - **ID**: `cpt-cf-qa-usecase-jira-loop`

**Actor**: `cpt-cf-qa-actor-jira`

**Preconditions**: The bug registry links an open JIRA bug to a test; a schedule launches with skip-tests-with-bugs enabled.

**Main Flow**:
1. The scheduled run skips the linked test and reports it skipped.
2. The bug poller detects the bug transitioned to resolved.
3. The system automatically launches a run covering the affected test via the standard path.
4. Insights records the outcome against the bug's resolution.

**Postconditions**: Bug status current; re-run outcome linked.

**Alternative Flows**:
- **Auto-rerun disabled**: The transition is recorded and surfaced; no launch occurs.

## 9. Acceptance Criteria

- [ ] A reference test repository from the current VHP deployment executes on QA Platform with per-test results equivalent to the current system, without any repository modification.
- [ ] All §5 p1 requirements demonstrable in a single-node deployment with no Kubernetes present; `kube` appears in no QA Platform gear's dependency tree.
- [ ] The three-outcome launch contract, exclusivity precedence, parameter validation rules, and environment assembly order behave identically to the documented current semantics (verified by a ported semantics test suite).
- [ ] The adapted UI covers all pages listed in `cpt-cf-qa-fr-ui-parity` against the new API with platform authentication.
- [ ] The migration tool imports a production-shaped snapshot with a clean reconciliation report.
- [ ] Multi-instance scheduler failover tests show zero duplicate and zero missed firings.
- [ ] All REST endpoints appear in the gateway OpenAPI document with canonical error responses; all events registered in the type system.

## 10. Dependencies

| Dependency | Description | Readiness | Criticality |
|------------|-------------|-----------|-------------|
| ToolKit + api-gateway | Lifecycle, REST/OpenAPI, SSE, SecureORM, canonical errors | Available | p1 |
| credstore | SSH keys, repo tokens, kubeconfig material | Available | p1 |
| oagw | JIRA polling, SMTP egress (**not** git sync — see `cpt-cf-qa-contract-egress` and `cpt-cf-qa-adr-git-egress`) | Available | p1 |
| cluster (cluster-sdk) | Leader election for scheduler and pollers | Available | p1 |
| file-storage | Archived logs; bundles pending — `FileStorageClientV1` has no P1 operations yet, so qa-catalog stores bundle blobs on an interim gear-local filesystem behind its `BundleStore` port (DECOMPOSITION 2.2 convergence note) | Available (early); no P1 operations | p1 (interim in-gear store) |
| ~~event-broker~~ | ~~Lifecycle event publication/consumption~~ | **Corrected 2026-09-01: removed, not in progress.** qa-runs' publisher and qa-insights' transactional consumer were both dead code — no deployment ever registered the client the consumer needed — and both were deleted along with the `event-broker`/`event-broker-sdk` dependencies. qa-insights ingests exclusively through its reconcile sweep over the qa-runs row below, which is, and has always actually been, the only path that ran. | — |
| serverless-runtime | Python workflow execution for the runner | Specced, not implemented — **blocking for p2 execution slice** | p2 |
| Notifications Service | Replaces in-gear SMTP notification path | Not started | p3 convergence |
| Jobs Manager | Could replace in-gear scheduler triggers | Not started | p3 convergence |
| Settings Service | Replaces migrated `settings` table concerns | Not started | p3 convergence |

## 11. Assumptions

- Test repositories remain pytest-based and keep the plan.yaml/TEST_META conventions.
- Target platforms are reachable from wherever the execution plane places the runner workload; the control plane never needs that reachability.
- The existing React SPA is adaptable (API client and auth layers are replaceable without a rewrite).
- A single designated tenant is acceptable for the initial migrated deployment; multi-team tenancy is configuration, not new code.
- JIRA and SMTP endpoints and credentials are provisioned by the operating organization.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| serverless-runtime Python workloads slip | p2 execution slice blocked; parity date moves | Mock executor keeps every other slice buildable/testable; RunExecutor contract frozen early so the backend drops in; escalation path via steering committee |
| Long-running pytest workloads stress a young serverless runtime | Timeouts, lost logs, flaky executions | Timeout/cancel/stream semantics in the execution contract from day one; soak tests with multi-hour synthetic runs before cutover |
| event-broker durable backend timeline | Lifecycle events not durable; insights could miss events | **Corrected 2026-09-01**: event-broker registered no client, so the transactional consumer this row's mitigation described never ran and has been deleted along with the event-broker dependency it needed. The mitigation held anyway: insights ingestion is idempotent and rebuildable, and its reconcile sweep over qa-runs' SDK is not an event path this timeline can affect either way. |
| Exclusivity/queue semantic drift during port | Destructive tests run in parallel — real-world damage | Semantics test suite ported from the current repo before the queue is implemented (see Acceptance Criteria) |
| Data migration fidelity | Loss of historical analytics trust | Reconciliation report, dry-run mode, migration tested on a production snapshot |
| UI adaptation scope creep | p2 parity date moves | Parity page list fixed in `cpt-cf-qa-fr-ui-parity`; visual redesign explicitly not a goal |

## 13. Open Questions

- Retention policy for run logs and test results (current system keeps everything; platform may want configurable retention + purge). Owner: QA Platform team; resolve before DESIGN sign-off of the insights schema.
- Should bundle download be replaced by direct file-storage signed URLs once file-storage supports them? Owner: QA Platform team; non-blocking, revisit at p2.
- Multi-runner parallelism inside a single run (sharding a plan across workers) — out of scope for parity, but the RunExecutor contract should not preclude it. Owner: DESIGN authors.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Decomposition**: [DECOMPOSITION.md](./DECOMPOSITION.md)
- **Source system**: VHP Test Runner repository (`manager/`, `manager-ui/`, `runner/`), its `docs/prd/` roadmap (out of scope here, see §4.2), and `docs/guides/` (exclusivity and run-parameter semantics preserved by §5.2).
