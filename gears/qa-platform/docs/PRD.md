# PRD — QA Platform

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Problem Statement](#12-problem-statement)
  - [1.3 Goals](#13-goals)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
- [3. Operational Concept](#3-operational-concept)
- [4. Scope](#4-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Products and Product Plugins](#51-products-and-product-plugins)
  - [5.2 Test Catalog](#52-test-catalog)
  - [5.3 Environments](#53-environments)
  - [5.4 Run Orchestration](#54-run-orchestration)
  - [5.5 Insights](#55-insights)
  - [5.6 User Interface](#56-user-interface)
  - [5.7 Packaging and Deployment](#57-packaging-and-deployment)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
- [7. Public Interfaces](#7-public-interfaces)
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

QA Platform is the test-management control plane for Virtuozzo products. It lets an engineer
register a running product instance, point the platform at a repository of tests, launch those
tests against that instance on demand or on a schedule, watch them run, and read the history back
as analytics.

### 1.2 Problem Statement

Virtuozzo ships several products, and they are not variations on one another: one is a Kubernetes
distribution, one is a hyperconverged infrastructure appliance reached over SSH, others expose
their own control-plane APIs. Each has its own idea of what an "environment" is, its own
credentials, and its own way of telling you what version it is running.

A test-management tool that models any one of them directly can only ever serve that one. QA
Platform's purpose is to be the tool that serves all of them: a common control plane for
catalogues, environments, runs and results, with everything product-specific confined behind a
single extension point.

### 1.3 Goals

| Goal | Measure |
|------|---------|
| One control plane for every Virtuozzo product | A second product is onboarded by adding a crate, with no edit to any gear |
| Unattended test execution | An engineer launches a run and does not babysit it; destructive tests never share an environment |
| Results are data, not log text | Every result is a typed row queryable by file, case, build and environment |
| Failures are traceable to a bug | A failing test correlates to a JIRA issue, and its resolution can re-trigger the run |
| Deployable as a unit | One Helm install brings up gears, UI, database, identity and execution wiring |

### 1.4 Glossary

| Term | Meaning |
|------|---------|
| **Product** | A Virtuozzo product. Carries no behaviour itself; names the plugin that supplies it |
| **Product plugin** | The implementation of `QaProductPluginV1` for one product |
| **Connector** | A library crate implementing a transport (Kubernetes, SSH) that a plugin links |
| **Environment** | A running instance of a product that tests can be aimed at |
| **Observation** | A plugin's report of an environment's detected attributes and health |
| **Lease** | The claim a run holds on an environment, shared or exclusive |
| **Test repository** | A git remote holding pytest suites and their plan definitions |
| **Plan** | A named set of tests discovered in a repository, identified by `(repo_id, path)` |
| **Custom plan** | An operator-assembled list of test files with tags and an optional timeout |
| **Run** | One execution of a target against an environment |
| **Collect run** | A run that enumerates test cases without executing them; it has no environment |
| **Bundle** | A checksummed, expiring snapshot of a work tree that a runner fetches |
| **Execution event** | A typed fact reported by a runner: started, test result, log line, finished |
| **Exclusivity** | Whether a run needs sole possession of its environment |

## 2. Actors

| Actor | ID | Interest |
|-------|-----|----------|
| QA engineer | `cpt-cf-qa-actor-engineer` | Launches runs, reads results, investigates failures |
| QA administrator | `cpt-cf-qa-actor-admin` | Registers environments and repositories, manages products, JIRA and notifications |
| CI system | `cpt-cf-qa-actor-ci` | Launches runs programmatically and reads their outcome |
| Platform operator | `cpt-cf-qa-actor-operator` | Installs and configures the deployment |
| Product plugin | `cpt-cf-qa-actor-plugin` | Supplies product-specific behaviour |
| Runner workload | `cpt-cf-qa-actor-runner` | Executes tests and reports execution events |

## 3. Operational Concept

An administrator registers a **product** and the platform resolves its **plugin**. They register an
**environment** for that product, filling in the credential form the plugin declares; the platform
stores secret fields in credstore and keeps only references, then **observes** the environment to
learn its version, build and health.

They register a **test repository**, which the platform clones and from which it discovers
**plans**.

An engineer launches a **run**: a plan, a single test file, a custom plan, or a collect. The
platform resolves the target, merges pipeline and environment variables, asks the plugin what the
run needs in order to reach the environment, and submits it — immediately if the environment is
free, otherwise onto that environment's queue. While it runs, log lines stream to the browser and
typed results are folded into the run's tally.

When it finishes, the run's lease is released. Independently, **qa-insights** sweeps finished runs
into the historical model, correlates failures with JIRA, and sends notifications.

## 4. Scope

### 4.1 In Scope

* Products, product plugins and their registration.
* Test repositories, branch caching, plan discovery, custom plans, SSH keys, test bundles.
* Environments: registration, credentials, observation, health, leases, variables.
* Runs: launch, per-environment queue, dispatch, cancel, rerun, schedules, timeouts.
* Live log streaming and durable log archiving.
* Typed result ingestion at file and case granularity.
* Analytics: dashboard, overview, per-build and per-test breakdowns, export, saved views.
* JIRA correlation and polling; Slack and email notifications.
* A browser UI covering all of the above.
* A Helm chart deploying the subsystem with Postgres, Keycloak and Argo wiring.

### 4.2 Out of Scope

* Authoring tests. The platform runs pytest suites; it does not help write them.
* Provisioning environments. The platform registers and observes environments; creating them is the
  product's own tooling.
* Being a general-purpose CI system. Runs are test runs, not arbitrary pipelines.
* Multi-team tenancy beyond a single designated tenant per deployment (see
  `cpt-cf-qa-constraint-single-tenant-deployment`).
* Runtime installation of plugins. A new product requires a rebuild.

## 5. Functional Requirements

### 5.1 Products and Product Plugins

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-product-plugins`

The system MUST resolve every product-specific behaviour through `QaProductPluginV1`, resolved per
product from `ClientHub` by `qa_products.plugin_instance_id`. No gear may branch on a product. A
plugin MUST declare the credential fields an operator supplies and the fields an observation can
yield, and MUST provide credential validation, observation and run-access preparation.

- **Rationale**: Onboarding a product must be additive.
- **Actors**: `cpt-cf-qa-actor-admin`, `cpt-cf-qa-actor-plugin`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-catalog-products`

The system MUST support product CRUD, where a product carries a name, key, description, optional
folder, and the plugin instance that governs it.

- **Actors**: `cpt-cf-qa-actor-admin`

### 5.2 Test Catalog

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-catalog-repos`

The system MUST register test repositories per product with a URL, default branch and content root,
optionally authenticated by a credstore-held credential; MUST clone and fetch them on demand; and
MUST cache their branch list. A sync failure MUST be visible on the repository rather than only in
logs.

- **Actors**: `cpt-cf-qa-actor-admin`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-catalog-plan-discovery`

The system MUST discover test plans in a synced work tree following the `plan.yaml` / `TEST_META`
conventions, and MUST identify a plan by `(repository, path)`.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-catalog-test-meta`

The system MUST read per-test metadata from the repository's `TEST_META` declarations, including the
tags that drive admission and the exclusivity a test declares for itself.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runner-contract`

The contract between the platform and a test repository is **frozen**: the `plan.yaml` format, the
`TEST_META` declarations, the environment variable names a test may rely on, and the marker format
by which a test declares its JIRA issue. A change to any of them is a change to every existing test
repository, so none may be made incidentally.

- **Rationale**: Test repositories are owned by many authors and are not versioned with the
  platform.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-runner`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-catalog-custom-plans`

The system MUST support operator-assembled plans: a list of test files, tags, and an optional
timeout, with full CRUD.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-catalog-ssh-keys`

The system MUST let an administrator register named SSH keys for repository access, storing only a
credstore reference and a fingerprint. The private key MUST NOT be readable back through any API.

- **Actors**: `cpt-cf-qa-actor-admin`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-catalog-bundles`

The system MUST produce a checksummed, expiring snapshot of a work tree for a runner to fetch.

- **Actors**: `cpt-cf-qa-actor-runner`

### 5.3 Environments

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-environments-registry`

The system MUST register environments per product, capturing the credential form the product's
plugin declares. Secret fields MUST be written to credstore and only references persisted. At most
one environment per product MAY be marked default.

- **Actors**: `cpt-cf-qa-actor-admin`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-environments-observation`

The system MUST observe an environment through its plugin, obtaining detected attributes and health
in a single call, and MUST persist the observed version, build, base URL, plugin-declared
attributes, health state and the time of observation. Observation MUST be available both on a
background cycle and on demand. An unobserved environment MUST be a valid state, not an error.

- **Actors**: `cpt-cf-qa-actor-admin`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-env-lease`

The system MUST record which runs hold an environment and in which mode (shared or exclusive), and
MUST make the lease inspectable. Lease updates MUST be safe under concurrency.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-environments-variables`

The system MUST support variables at two scopes — per environment and subsystem-wide — and MUST
merge them into a run's environment at dispatch, with the narrower scope winning. Variable names
consumed by tests MUST be preserved verbatim into the runner's environment.

- **Actors**: `cpt-cf-qa-actor-admin`

### 5.4 Run Orchestration

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-launch`

The system MUST launch a run against one of four target shapes — a plan, a single test file, a
custom plan, or a collect URL — resolving the target, the environment, the plugin's access
requirements and the merged variables into an execution specification.

- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-ci`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-queue`

The system MUST maintain a per-environment FIFO queue: while an exclusive run holds an environment,
subsequent runs targeting it queue and start automatically when it frees; nothing is rejected for
the environment being busy. Runs without a target environment are never queued and never block
others. Queued runs MUST be listable and cancellable.

- **Rationale**: Destructive tests must not share an environment; engineers must not babysit
  launches.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-ci`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-exclusivity`

The system MUST resolve each run's exclusivity and record where the answer came from: the launch
request, the plan definition, test metadata, or the default. Enforcement MUST NOT rely on the
execution backend alone.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-dispatch`

The system MUST drain the queue on an interval, claiming the environment before submitting, so that
a claim exists for the whole window in which the repository sync and bundle build run.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-timeout`

The system MUST bound a run's execution with a configurable deadline and a cap, and MUST bound a
run's wait in the queue separately. The two MUST be distinguishable in the run's terminal state.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-cancel-rerun`

The system MUST cancel a live run, propagating the cancellation to the execution backend and
releasing the environment lease, and MUST re-launch a previous run's resolved target.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-params`

The system MUST accept named run parameters and MUST validate them on every launch endpoint, and
re-validate them when a run is re-run. Validation covers the permitted character set, a reserved-name
list, rejection of duplicates, and caps on the number of parameters and on the length of a name and
of a value. Parameters are stored in plain text and MUST NOT be used for tokens or passwords.

- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-ci`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-env-assembly`

The system MUST assemble a run's environment from four sources in a fixed order — static runner
variables, subsystem-wide pipeline variables, environment variables, then run parameters — where a
later entry overrides an earlier one of the same name, and MUST hand the result to the runner with
no duplicate entries left to resolve.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-schedules`

The system MUST launch runs on a cron schedule, MUST record each due instant as a claim so that a
schedule fires once per instant, and MUST support per-schedule notification settings.

- **Actors**: `cpt-cf-qa-actor-admin`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-results-ingest`

The system MUST ingest typed execution events — run started, test result, log line, run finished —
and persist run tallies and per-test results incrementally as events arrive. The run record in the
database is the sole source of truth for run state. Ingestion MUST be safe against concurrent
producers.

- **Actors**: `cpt-cf-qa-actor-runner`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-runs-logs`

The system MUST stream a running run's log lines to a viewer as server-sent events, and MUST
persist a durable copy readable after the run finishes.

- **Actors**: `cpt-cf-qa-actor-engineer`

### 5.5 Insights

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-history`

The system MUST persist historical results at two granularities — per test file and per test case,
the latter keyed by pytest `nodeid` — and MUST expose both for query by run, file, case, status,
build and environment.

- **Rationale**: Flakiness and regression analysis need test-level granularity across runs;
  per-case granularity is what makes xfail/xpass visible and lets a file's ticket badges render
  without a second fetch.
- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-ingest`

The system MUST ingest finished runs into the historical model without the run path depending on
it, and MUST resume from a durable watermark after a restart.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-dashboard`

The system MUST provide an at-a-glance dashboard showing recent runs, active and queued runs, pass
rates, and a coverage view (which tests and plans ran against which product versions and
environments), scoped to the selected product.

- **Rationale**: The entry point of the tool. Shipping a dashboard that looks complete in a
  screenshot and is not is worse than shipping fewer sections; enumerating them is what makes the
  requirement testable.
- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-admin`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-analytics`

The system MUST provide an overview of exactly eight sections, a per-build test breakdown, per-plan
build and test listings, a single test's history across builds, and an export of the current query.
The count is fixed here so that a reader who counts the list is not left hunting for a ninth.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-saved-views`

The system MUST persist analytics queries per owner, scope and plan, and list them back.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-collect`

The system MUST record the exact number of test cases a file contains for a `(repository, branch,
file)` triple, obtained by enumerating cases without executing them.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-jira`

The system MUST correlate a failing test with a JIRA issue, poll issue status on a configurable
interval, and optionally re-launch the corresponding run when an issue is resolved. JIRA
credentials MUST come from credstore and egress MUST go through the platform gateway.

- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-admin`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-insights-notifications`

The system MUST notify on run outcomes over Slack and email, per tenant, with per-event toggles, a
template preview, a test send, and an audit record of every attempt. A run MUST NOT be notified
twice for the same event.

- **Actors**: `cpt-cf-qa-actor-admin`

### 5.6 User Interface

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-ui`

The system MUST provide a browser interface covering dashboard, runs and run detail with a live
log, plans, custom plans, products, environments, schedules, test results, analytics and settings.

- **Actors**: `cpt-cf-qa-actor-engineer`, `cpt-cf-qa-actor-admin`

- [ ] `p2` - **ID**: `cpt-cf-qa-fr-ui-surfaces`

Where a gear serves no backend for a surface the interface would otherwise offer, the surface MUST
be absent rather than present and empty. A page that renders a confident but unsourced value is
worse than a page that does not exist.

- **Actors**: `cpt-cf-qa-actor-engineer`

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-ui-product-scoping`

Every list MUST be scoped to the selected product, attributing a row through its target rather than
its environment. Where scoping happens in the browser rather than on the server, the interface MUST
say so rather than presenting an unscoped list as scoped.

- **Actors**: `cpt-cf-qa-actor-engineer`

### 5.7 Packaging and Deployment

- [ ] `p1` - **ID**: `cpt-cf-qa-fr-packaging`

The system MUST ship a Helm chart deploying the gears, the UI, Postgres with per-gear databases,
Keycloak with its realm, TLS material, database migrations, tenant seeding, and the RBAC the
execution backend requires.

- **Actors**: `cpt-cf-qa-actor-operator`

## 6. Non-Functional Requirements

| ID | Requirement |
|----|-------------|
| `cpt-cf-qa-nfr-run-duration` | A run of up to 8 hours MUST survive a control-plane restart: state is recovered and the live execution re-attached |
| `cpt-cf-qa-nfr-result-latency` | A reported result MUST be visible within 5 s at p95 |
| `cpt-cf-qa-nfr-dispatch-latency` | A queued run MUST start within 10 s at p95 of its environment becoming free |
| `cpt-cf-qa-nfr-log-latency` | A log line MUST reach a connected viewer within 2 s at p95 |
| `cpt-cf-qa-nfr-ingest-recovery` | Historical ingestion MUST resume within 60 s of a control-plane restart, with no duplicate rows |
| `cpt-cf-qa-nfr-tenant-isolation` | No row MUST ever be readable across a tenant boundary |
| `cpt-cf-qa-nfr-credential-containment` | No value derived from credential material MUST reach any published surface |
| `cpt-cf-qa-nfr-infra-agnostic` | A default build MUST have no Kubernetes dependency in its tree |
| `cpt-cf-qa-nfr-observability` | Every background loop and every external call MUST be measured |
| `cpt-cf-qa-nfr-scheduler-exactly-once` | A schedule MUST launch exactly one run per due instant, however many replicas are running |
| `cpt-cf-qa-nfr-scale` | A deployment MUST support 100 environments, 100 test repositories and 10,000 retained runs per tenant. **It states no latency bound**: it is a sizing requirement, and no metric measures it |

## 7. Public Interfaces

### 7.1 REST

All endpoints live under `/qa/v1`, are described by the generated OpenAPI document, and return
problem-detail errors. Collection endpoints support OData query. The surface is enumerated per gear
in [DESIGN.md](./DESIGN.md) §3.2–§3.5.

### 7.2 SDK crates

`qa-environments-sdk`, `qa-catalog-sdk`, `qa-runs-sdk` and `qa-insights-sdk` carry each gear's
client trait and models, and are the only way another gear may read its data.

**No SDK method returns run log text.** A run's log is reachable only as the SSE stream.

### 7.3 `qa-product-sdk`

`QaProductPluginV1` is the contract a product plugin implements, and is a versioned public
interface. See [ADR-0006](./ADR/0006-cpt-cf-qa-adr-product-plugins.md) and
[features/product-plugins.md](./features/product-plugins.md).

### 7.4 Egress

- [ ] `p1` - **ID**: `cpt-cf-qa-contract-egress`

All outbound HTTP MUST go through the platform's Outbound API Gateway, which injects credentials and
controls egress. The contract permits **exactly one** direct-HTTP exception, and it belongs to
qa-catalog's git transport ([ADR-0005](./ADR/0005-cpt-cf-qa-adr-git-egress.md)). JIRA, Slack and SMTP
all go through the gateway; a `reqwest` dependency in qa-insights would be a violation of this
contract.

### 7.5 Execution events

The typed events a runner reports are the contract between a runner and the control plane. See
[ADR-0002](./ADR/0002-cpt-cf-qa-adr-structured-events.md).

## 8. Use Cases

| # | Use case | Actor | Outcome |
|---|----------|-------|---------|
| UC-1 | Onboard a product | admin | A product exists, bound to its plugin |
| UC-2 | Register an environment | admin | Credentials stored as references; version, build and health observed |
| UC-3 | Register a test repository | admin | Repository synced, branches cached, plans discoverable |
| UC-4 | Launch a plan against an environment | engineer | Run starts or queues; results and log stream live |
| UC-5 | Launch while the environment is busy | engineer | Run queues, then starts automatically when the environment frees |
| UC-6 | Cancel a run | engineer | Execution stops and the lease is released |
| UC-7 | Schedule a nightly run | admin | The schedule fires once per due instant |
| UC-8 | Collect case counts | engineer | Expected case counts recorded per file, with no environment involved |
| UC-9 | Investigate a failure | engineer | Per-case result, its history, and its correlated JIRA issue |
| UC-10 | Be told about a failure | admin | Slack or email notification, sent once, audited |
| UC-11 | Install the platform | operator | One Helm install brings the stack up |

## 9. Acceptance Criteria

* A second product is onboarded by adding a crate and a row, with no edit to any gear.
* A run launched against a busy environment queues and starts automatically when it frees.
* A run's results are queryable per case, not only per file.
* A control plane restarted mid-run re-attaches and the run completes.
* Planted credential material never appears on a published surface; the check fails the build.
* `cargo tree -p qa-runs -i kube -e normal` prints nothing for a default build.
* A Helm install on a clean cluster brings up the stack and serves the UI.

## 10. Dependencies

| Dependency | Used for |
|------------|----------|
| credstore | All credential material |
| ClientHub | Plugin and SDK client resolution |
| types-registry / GTS | Plugin instance identity |
| AuthZ resolver | Authorization decisions |
| OAGW | JIRA and SMTP egress |
| Postgres | Per-gear storage |
| Keycloak | Identity in the shipped deployment |
| Argo Workflows | The shipped execution backend, behind the `argo` feature |

## 11. Assumptions

* Test repositories are pytest-based and keep the `plan.yaml` / `TEST_META` conventions.
* Environments are reachable from wherever the execution plane places the runner workload; the
  control plane itself never needs that reachability.
* A single designated tenant is acceptable per deployment; multi-team tenancy is configuration, not
  new code.
* JIRA and SMTP endpoints and credentials are provisioned by the operating organisation.
* The platform's event bus has no durable backend in this deployment, so nothing may rely on an
  event delivered while a consumer is down.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Host-key verification is disabled for git and SSH | An on-path attacker can substitute a repository or capture a session credential | Recorded in [ADR-0005](./ADR/0005-cpt-cf-qa-adr-git-egress.md) with the tightening path |
| The default executor fabricates a passing test | A misconfigured deployment reports green runs that ran nothing | Explicit configuration; documented in [ADR-0001](./ADR/0001-cpt-cf-qa-adr-execution-plane.md) |
| Live log fan-out is per replica | A second replica splits the live stream | Single-replica deployment; the durable copy is unaffected |
| A plugin shares the gear process | A misbehaving plugin affects the gear | Plugins are first-party and reviewed |
| Dispatch latency is measured by a proxy | An alert can miss a violation | Stated in DESIGN §3.11; closing it needs a lease-release instant |

## 13. Open Questions

* **Coverage semantics.** `cpt-cf-qa-fr-insights-dashboard` phrases coverage as *execution*
  coverage; the shipped endpoint answers *code* coverage and returns an empty array because nothing
  computes it. Which reading the requirement should have is unsettled. See DESIGN §3.5.
* **Custom roles for QA resources.** QA resource types are plain strings rather than GTS type ids,
  so no custom role can target them. Renaming is precluded by existing policies. See DESIGN §3.10.
* **Host-key pinning.** Whether to pin, and how keys would be provisioned for operator-registered
  remotes and nodes.
* **Leader election.** The shipped elector makes every replica a dispatcher; a real elector is
  needed before a multi-replica deployment.

## 14. Traceability

| Requirement | Design | ADR |
|-------------|--------|-----|
| `cpt-cf-qa-fr-product-plugins` | §3.7 | ADR-0006, ADR-0007 |
| `cpt-cf-qa-fr-catalog-repos` | §3.3 | ADR-0005 |
| `cpt-cf-qa-fr-environments-observation` | §3.2 | ADR-0006 |
| `cpt-cf-qa-fr-runs-queue` | §3.4 | ADR-0004 |
| `cpt-cf-qa-fr-runs-results-ingest` | §3.4 | ADR-0002 |
| `cpt-cf-qa-fr-runs-logs` | §3.4, §3.6 | ADR-0003 |
| `cpt-cf-qa-fr-insights-ingest` | §3.5 | ADR-0009 |
| `cpt-cf-qa-fr-ui-product-scoping` | §3.6 | ADR-0010 |
| `cpt-cf-qa-nfr-credential-containment` | §3.7 | ADR-0008 |
| `cpt-cf-qa-nfr-infra-agnostic` | §2.2 | ADR-0001 |
