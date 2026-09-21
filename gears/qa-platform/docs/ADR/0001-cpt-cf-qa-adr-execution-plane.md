---
status: accepted
date: 2026-08-12
---
# Execution plane: a `RunExecutor` port with swappable adapters

**ID**: `cpt-cf-qa-adr-execution-plane`

## Context and Problem Statement

A test run is a long-lived workload: it can run for hours, it must be cancellable, it streams log
output while it runs, and it must survive a restart of the control plane that launched it. Someone
has to actually run it — a workflow engine, a serverless runtime, a container scheduler.

QA Platform's gears are control-plane components and must deploy in every shape the platform
supports, including ones with no Kubernetes at all. How should test workloads be executed without
that requirement dictating where the control plane can run?

## Decision Drivers

* Control-plane gears must be infrastructure-agnostic; only an integration layer may touch an
  external system.
* Long-running workloads with cancellation and log streaming are mandatory.
* Run state must be recoverable after a control-plane restart, without reconstructing it from an
  execution engine's objects.
* A backend that does not exist yet must not block everything that depends on execution.

## Considered Options

* An internal `RunExecutor` port with adapters, selected by configuration
* A direct dependency on one execution engine
* A public execution-backend plugin interface, resolved at runtime like a product plugin

## Decision Outcome

Chosen option: **an internal `RunExecutor` port with adapters**, because it is the only option that
keeps a Kubernetes dependency out of a default build while still shipping a backend that really
runs tests, and because it lets every other part of the subsystem be built and tested against a
deterministic adapter.

The port is four operations and is frozen:

```rust
async fn start(&self, spec: RunSpec) -> Result<ExecutionRef, DomainError>;
async fn watch(&self, execution_ref: &ExecutionRef, resume: LogResume)
    -> Result<ExecutionStream, DomainError>;
async fn cancel(&self, execution_ref: &ExecutionRef) -> Result<(), DomainError>;
async fn list_active(&self) -> Result<BTreeSet<ExecutionRef>, DomainError>;
```

`watch` takes a resume position so a restarted control plane re-attaches to a live execution;
`list_active` is what the boot path sweeps to find executions to re-attach to. Together they are
what makes `cpt-cf-qa-nfr-run-duration` reachable.

Two adapters ship:

| Adapter | Gate | Behaviour |
|---------|------|-----------|
| `MockRunExecutor` | default | deterministic, in-memory; every run reports one fabricated passing test |
| `ArgoRunExecutor` | non-default `argo` cargo feature | submits an Argo `Workflow`, polls status, follows pod logs, deletes on cancel |

The feature gate is the mechanism that satisfies `cpt-cf-qa-constraint-no-kube`: `kube` and
`k8s-openapi` enter `qa-runs`' tree only when `argo` is enabled, so a default build of every
qa-platform **gear** has no Kubernetes dependency. (Not every *crate*: see this document's
Amendments section for `qa-connector-k8s`, a non-gear crate that carries `kube` unconditionally.)

The port is deliberately **internal**. It is not a public plugin interface, because choosing an
execution backend is a deployment decision made once in configuration, not a per-product decision
that has to be resolved at runtime. Contrast `QaProductPluginV1`
([ADR-0006](./0006-cpt-cf-qa-adr-product-plugins.md)), which is public precisely because it *is*
per-product.

### Consequences

* Good, because a default build is infrastructure-free and provably so.
* Good, because every service above the port is testable without an execution engine.
* Good, because run state is database-first: the executor reports into `qa_runs`, and nothing
  reconstructs business state from workflow objects or log text.
* Good, because a further backend is an adapter, not a redesign.
* Bad, because the default adapter fabricates a passing test, so a deployment that forgets to set
  `executor: argo` reports green runs that ran nothing. The configuration is explicit and the
  adapter's doc says so, but the failure mode is quiet.
* Bad, because the port has to be the narrow intersection of every backend's capabilities; a
  backend feature that does not fit the four operations is not reachable.

### Confirmation

* `cargo tree -p qa-runs -i kube -e normal` prints nothing for a default build, and shows the tree
  under `--features argo`.
* `qa-runs/src/domain/ports/run_executor.rs` declares the trait; no service above it names an
  adapter type.
* The `MockRunExecutor` suite covers every service that depends on execution.

## Pros and Cons of the Options

### Internal `RunExecutor` port with adapters

* Good, because the Kubernetes dependency is confined to one feature-gated module.
* Good, because the mock adapter makes the whole subsystem testable in-process.
* Neutral, because backend selection is startup configuration, not runtime resolution.
* Bad, because the port constrains adapters to its four operations.

### Direct dependency on one execution engine

* Good, because it is the least code and the engine's full capability is available.
* Bad, because every deployment shape then requires that engine.
* Bad, because run state tends to drift toward being read back out of engine objects, which is
  exactly what `cpt-cf-qa-principle-db-first-state` forbids.

### Public execution-backend plugin interface

* Good, because a third party could supply a backend.
* Bad, because it prices in runtime resolution, GTS identity and a stability guarantee for a
  decision that is made once per deployment.
* Bad, because a public interface is much harder to change than an internal trait, and this one is
  expected to change as backends are added.

## Amendments

**`qa-environments`' runner-`Secret` writer.** After this ADR's original acceptance,
`qa-environments` gained a second Kubernetes-touching adapter: `KubeRunnerSecretWriter::ensure_runner_secret`
(decision D4) writes each runner's credential `Secret` into the Argo cluster, which may differ from
any environment's own. It needs a Kubernetes client for that write alone. This is gated behind
`qa-environments`' own non-default `runner-secret` cargo feature — a second, independently-switched
gate beside `qa-runs`' `argo`, not a relaxation of this ADR's Confirmation criterion: a default
build of `qa-environments` still carries no Kubernetes dependency
(`qa-environments/Cargo.toml`'s `[features]` block and `containment_tests.rs` are the authority).

**`qa-connector-k8s` is unconditional, not feature-gated, and that is consistent with this ADR.**
The Kubernetes transport library the VHP product plugin links (`connectors/qa-connector-k8s`) names
`kube` and `k8s-openapi` without a feature flag of its own. It needs none: it is not a gear, nothing
resolves it at runtime, and the only edge to it in any gear's dependency graph is a product plugin
that already requires a Kubernetes-shaped environment. `cargo tree -p qa-runs -i kube -e normal`
still prints nothing for a default build; a deployment that never registers the VHP plugin never
builds this crate's client either. The containment this ADR requires is structural here rather than
a cargo feature, and is exactly as strong.

Several code comments across `qa-runs`, `qa-environments` and `qa-connector-k8s` referred to the
two facts above as a "waiver" or an "amendment" dated 2026-08-27, 2026-08-28 or 2026-09-04. No such
waiver or amendment existed in this document before this section — those dates never appeared above,
and this ADR was not revised between its acceptance and today. This section is what those comments
meant to cite. It is added now, dated to when the gap was found rather than backdated to an event
this document has no record of.

*(Added 2026-09-18, QA Platform review remediation §5.6.2.)*
