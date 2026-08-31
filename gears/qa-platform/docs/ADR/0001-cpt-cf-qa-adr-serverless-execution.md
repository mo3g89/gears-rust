---
status: accepted
date: 2026-08-12
---
# Execution plane: replace Argo Workflows with the platform serverless runtime behind a RunExecutor port

**ID**: `cpt-cf-qa-adr-serverless-execution`

## Context and Problem Statement

The source system creates test runs as Argo `Workflow` objects via a direct `kube::Client`, treats those objects as the source of truth for run state, and recovers results by scraping pod logs. QA Platform gears must run in all three Fabric deployment shapes (single-node, multi-node, Kubernetes), which a hard Argo/Kubernetes dependency makes impossible. How should test workloads be executed?

## Decision Drivers

* Platform rule: control-plane gears must be infrastructure-agnostic; only integration layers touch external systems.
* Existing investment: the pytest runner and its test-facing contract work and are owned by many test authors.
* serverless-runtime (the platform's execution capability) is specced but not implemented; its Python workload support is p2 on its own roadmap.
* Long-running workloads (multi-hour suites) with cancellation and log streaming are mandatory.
* The team explicitly chose full migration over keeping Argo (decision made during design review, 2026-08-11).

## Considered Options

* Migrate to the platform serverless runtime behind an internal `RunExecutor` port
* Keep Argo Workflows behind a public execution-backend plugin interface
* Pluggable contract with Argo first and serverless later

## Decision Outcome

Chosen option: "Migrate to the platform serverless runtime behind an internal `RunExecutor` port", because it is the only option that removes the Kubernetes dependency from the product entirely (a k.o. criterion for platform adoption), and the port isolates the schedule risk of the unbuilt runtime: everything except the adapter builds and tests against a mock.

### Consequences

* qa-runs domain code depends only on `RunExecutor { start(RunSpec) → execution_ref; cancel(execution_ref); watch(execution_ref) → ExecutionEvent stream; list_active() → set of execution_ref }`; the serverless adapter and the p1 mock live in infra.

  **Amended 2026-08-13 (qa-runs Task 11): four operations, not three.** `list_active` was absent from this ADR's first draft and is required by the dispatcher's per-tick claim reconciliation, which must answer "is this execution still alive?" for every outstanding claim at once. The source system does exactly that, building a membership set from one list call per tick (`manager/src/services/run_dispatcher.rs:371-375`) and treating an unreadable answer as *busy* (`:237-252`). The alternative within three operations — one `watch` per claim per tick — is strictly worse: it opens a stream per claim to ask a membership question, and it cannot express the fail-safe direction, because a stream that fails to open is indistinguishable from an execution that has ended. The set it returns is **not tenant-scoped** and is safe only as a membership test; it must never be enumerated into a response.
* Run state must become database-first (no engine objects to reconcile from); this forces the structured-events decision (`cpt-cf-qa-adr-structured-events`).
* Scheduling can no longer ride on CronWorkflows; qa-runs must provide a leader-elected cron task (cluster-sdk).
* The p2 execution slice is blocked on serverless-runtime Python workloads; DECOMPOSITION orders all other slices first and the RunExecutor contract is frozen early.
* The serverless runtime must support long-lived (hours) workloads with cancellation and event/log streaming — these requirements must be fed into its spec now (upstream requirement on serverless-runtime).
* The port stays internal (not a public plugin) until a second real backend exists; promoting it later is additive.

### Waiver, 2026-08-27 (human decision): a feature-gated Argo adapter is permitted

**This ADR is not reversed.** The serverless runtime remains the intended execution plane and this
decision's reasoning still stands: `gears/serverless-runtime/` is docs-only (zero `.rs` files), so the
execution slice this ADR ordered last is not merely blocked, it is unstarted, and meanwhile every run in
every deployment reports one fabricated passing test (`test_mock_default`, `infra/executor/mock.rs:55`).

The product owner decided on 2026-08-27 to accept an Argo-backed `RunExecutor` adapter **inside qa-runs,
behind a non-default cargo feature**, in order to execute real tests before the runtime exists. Stated to
them before the decision, and on the record: this breaches this ADR's Confirmation criterion and the `p1`
constraint `cpt-cf-qa-constraint-no-kube` (`DESIGN.md:180-182`) in letter, and it re-introduces the
Kubernetes dependency whose removal this ADR calls a k.o. criterion for platform adoption. The
ADR-compliant alternative was offered and declined on cost: put the Argo code in a crate outside
qa-platform behind the same frozen port, which `DESIGN.md:182` already implies by passing kubeconfigs
"by reference to the execution plane", and which this ADR's own "promoting it later is additive" permits.

Scope of the waiver, deliberately narrow:

* The `kube`/`k8s-openapi` dependency is **optional** and reachable only through a cargo feature that is
  **off by default**, so a default build of any qa-platform crate still satisfies the constraint.
* No change to the `RunExecutor` port, which stays frozen. The adapter is an additional `infra` impl.
* Nothing in `domain/` learns that Kubernetes exists. `RunSpec` continues to carry secret **references**,
  never material (`run_executor.rs:85-86`).
* The mock stays the default executor, so no deployment gains a Kubernetes dependency by upgrading.

Note for whoever audits this: the Confirmation criterion below says a `cargo-deny`/dependency check
enforces "no `kube`/`k8s-openapi` in any qa-platform crate". **That check was never implemented** —
`deny.toml` exists and contains no such rule — so this breach would have been silent. It is recorded here
instead, which is the only reason it is visible.

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

The unimplemented `cargo-deny` rule named below is still unimplemented, and — corrected
2026-08-28, after the final whole-branch review — **so is the CI check this paragraph
used to claim.** An earlier revision said "`cargo tree -p <crate> -i kube` in CI is the
only enforcement, and this plan adds it for `qa-environments`". No such CI job was added:
`.github/workflows/` contains no qa-platform job at all, and the only artefact the plan
produced is the commented-out recipe in `qa-environments/Cargo.toml` (under the
`platform-observation` feature) that an auditor has to run by hand.

The honest statement of what enforces this today, in decreasing order of strength:

* `cargo test -p qa-environments` with **no** features passes (142 tests as of
  2026-08-28), which is only possible because `domain/` compiles with no `kube` in the
  tree — the waiver's central condition, proven by a command anyone can run.
* `infra/observer/mod.rs` carries `#![cfg(feature = "platform-observation")]` *and* is
  reached through a `#[cfg]`-gated `pub mod observer` in `infra/mod.rs`, so the gate
  cannot be lost by editing one line.
* `cargo tree -p qa-environments -i kube` errors in a default build and resolves under
  `--features platform-observation`. **This is a manual check.** Making it automatic
  means adding qa-platform to CI, which this subsystem has never had and which is a
  larger decision than a waiver amendment should take on its own; it is recorded here as
  an open item rather than asserted as done.

### Confirmation

`cargo-deny`/dependency check: no `kube`/`k8s-openapi` in any qa-platform crate. Integration suite runs the full launch→ingest path against the mock adapter; the same suite runs against the serverless adapter when it lands (contract test parity).

## Pros and Cons of the Options

### Migrate to serverless runtime behind an internal RunExecutor port

* Good, because the control plane becomes deployment-shape-agnostic — the platform's core promise.
* Good, because execution consolidates onto one platform capability instead of a parallel engine.
* Good, because the mock adapter decouples the subsystem's schedule from the runtime's.
* Bad, because the runtime does not exist yet; execution parity date is hostage to another team's roadmap.
* Bad, because a young runtime must immediately handle its hardest workload class (multi-hour, cancellable, streaming).

### Keep Argo behind a public plugin interface

* Good, because it preserves a proven execution engine and the current runner unchanged.
* Good, because p2 would not be blocked.
* Bad, because the Kubernetes dependency remains — single-node/multi-node shapes stay impossible, failing the primary conversion goal.
* Bad, because a public plugin interface designed against one backend ossifies that backend's assumptions.

### Pluggable contract, Argo first, serverless later

* Good, because it de-risks the timeline while promising eventual convergence.
* Bad, because it requires building and maintaining two execution adapters plus the K8s dependency for an interim period, roughly doubling execution-plane work for a system with one deployment today.
* Bad, because "later" migrations off a working interim backend historically do not happen.

## More Information

Decision made with the user during brainstorming on 2026-08-11 (execution-engine question, option "Migrate to Fabric serverless-runtime / Jobs" selected explicitly over both Argo-keeping options).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-qa-fr-runs-execute` — defines how execution is delegated
* `cpt-cf-qa-nfr-run-duration` — constrains the executor contract (long-lived, re-attachable)
* `cpt-cf-qa-constraint-no-kube` — realized by this decision
* `cpt-cf-qa-principle-executor-port`, `cpt-cf-qa-principle-db-first-state` — direct consequences
