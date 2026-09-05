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

The honest statement of what enforces this today, in decreasing order of strength —
**superseded 2026-09-04**, see the amendment below; every item here is about
`qa-environments`, and the boundary has since stopped being only about that crate:

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

### Waiver amendment, 2026-09-04 (Phase C of the product-plugin plan): the containment moves from a feature gate to a crate boundary

The two amendments above are both about `qa-environments`, and so is every item in
the enforcement list they end with. Phase C changed the boundary underneath that
list and touched none of it, which made this the least accurate description of the
containment in the repo while still being the authoritative one. This paragraph
corrects that; it records a strengthening and a weakening, because both happened.

**What Phase C did**, in three moves, each already recorded locally but none of them
here:

* **Task 8** copied the Kubernetes mechanics out of `qa-environments/infra/observer/`
  into a new library crate, `gears/qa-platform/plugins/qa-plugin-k8s`. A copy, not a
  move: the original stays in place and stays active behind `platform-observation`
  until Task 19, because Phase C must not change `qa-environments`' behaviour. The new
  crate names `kube`/`k8s-openapi` **unconditionally** — there is deliberately no cargo
  feature on it to turn Kubernetes off, because a build that does not want Kubernetes
  simply does not depend on the crate. Its own manifest and `lib.rs` argue that at
  length.
* **Tasks 9-10** built `qa-vhp-product-plugin` on it. That plugin is now the carrier:
  it is the only crate that depends on `qa-plugin-k8s`.
* **Task 11** put the plugin into `cf-gears-example-server`'s `qa-platform` feature
  (`qa-platform = [..., "dep:qa-vhp-product-plugin"]`), because a `qa-platform` build
  without it can store environments and do nothing with them.

**The measured consequence**, re-run against `07727d5ac` and quoted rather than
described:

```
$ cargo tree -p cf-gears-example-server -i kube
error: package ID specification `kube` did not match any packages

$ cargo tree -p cf-gears-example-server --features qa-platform -i kube
kube v3.1.0
└── qa-plugin-k8s v0.1.0
    └── qa-vhp-product-plugin v0.1.0
        └── cf-gears-example-server v0.6.1
```

(Local crate paths elided from each line; nothing else is.)

The 2026-08-27 waiver's condition 1 — "reachable only through a cargo feature that is
off by default" — therefore still holds: `apps/cf-gears-example-server/Cargo.toml`
declares `default = []`, so a default build of the binary carries no `kube`. What
changed is which feature the Kubernetes edge arrives with. It used to be
`platform-observation` or `qa-runs-argo`, both narrow and both explicitly named by an
operator. It is now `qa-platform` itself.

**Two corrections to how this containment has been described elsewhere**, because
both statements are in the tree today and both read wider than the tree supports:

* "`qa-plugin-k8s` is the only crate in the workspace that names Kubernetes types"
  (`PRODUCT-PLUGINS-DESIGN.md` §4.1 reading 1, and, until 2026-09-04, repeated in two
  manifests and in `qa-plugin-k8s/src/lib.rs`) **is not true and will not become true.**
  All four are corrected as of 2026-09-04; §4.1 carries a dated *Correction* subsection
  that keeps its original sentence visible beside the true one, which is the source fix
  the other three had propagated from. Workspace-wide, `libs/toolkit-k8s-auth` names them
  unconditionally and `chat-engine`/`mini-chat` name them behind their own `k8s`
  features; none of those is in scope for this ADR, but "the only crate in the
  workspace" is the wrong phrase for what was meant. Inside qa-platform, three crates
  name them right now — `qa-plugin-k8s` unconditionally, `qa-environments` behind
  `platform-observation`, `qa-runs` behind `argo`. After Task 19 deletes
  `platform-observation` there will still be **two**, not one: nothing in the plan
  removes `qa-runs`' `argo` adapter (`qa-runs/qa-runs/Cargo.toml:89`), which is the
  subject of the 2026-08-27 waiver and stays until the serverless runtime exists.
  §4.1's claim was therefore not just premature, it was wrong about the end state too —
  which is why the correction there is dated rather than a quiet edit.
  The claim that is true, and is the one this amendment makes: **`qa-plugin-k8s` is
  the only qa-platform crate that names `kube`/`k8s-openapi` unconditionally, and the
  only edge into it is a product plugin whose target is a cluster.**
* Under the feature list an actual deployment builds
  (`gears/qa-platform/deploy/cargo-features.argo`), the same query today reports three
  reverse dependencies, not one:

  ```
  $ cargo tree -p cf-gears-example-server --features "$(cat deploy/cargo-features.argo)" -i kube
  kube v3.1.0
  ├── qa-environments v0.1.0      # via platform-observation
  ├── qa-plugin-k8s v0.1.0        # via qa-vhp-product-plugin, unconditional
  └── qa-runs v0.1.0              # via argo
  ```

  (Top-level reverse dependencies only; the real output expands each into its own
  dependents, and `qa-environments`' subtree reaches the binary through `qa-runs` and
  `qa-insights` as well as directly.)

  The structural containment is a statement about the *default* graph. It says nothing
  about the graph that is actually shipped, and Task 19 removes only the first of those
  three edges.

**The honest weakening, which is the part an amendment that only recorded wins would
leave out.** `PRODUCT-PLUGINS-DESIGN.md` §4.1 calls the crate boundary "strictly
stronger than the feature gate". On one axis it is: a reader can now find every
Kubernetes type in qa-platform by looking at one crate's manifest, and no `#[cfg]`
can be lost by editing one line. On the axis this ADR's Decision Outcome actually
names — *"removes the Kubernetes dependency from the product entirely (a k.o.
criterion for platform adoption)"* — it is **weaker**:

* Before Phase C, `--features qa-platform` produced a complete, compiling, testing
  QA Platform binary with no `kube` anywhere in its tree. That is not an inference:
  at `07727d5ac~1` the feature read
  `qa-platform = ["dep:qa-environments", "dep:qa-catalog", "dep:qa-runs",
  "dep:qa-insights"]`, and the only two edges to `kube` from any of those four were
  `platform-observation` and `argo`, both off. The build was degraded — no
  observation, the mock executor — but it existed, and its existence is what the
  feature gate bought.
* As of Task 11 it does not exist. `qa-plugin-k8s` has no gate and the plugin is
  unconditional in the `qa-platform` feature, so there is no switch that produces a
  kube-free build of the product feature.
* After Task 19 that becomes permanent rather than incidental: `platform-observation`
  is deleted and observation runs only through the plugin, so the kube-free
  observation path is gone as well as the kube-free build.

A single-node or multi-node Fabric deployment therefore cannot build the QA Platform
feature at all without pulling in a Kubernetes client, which is exactly the shape this
ADR's Decision Outcome rejected. It is accepted here for the same reason the first
waiver was accepted — the serverless runtime does not exist — and it is written down
here because this ADR's only real value is that it records breaches against itself.

**The honest statement of what enforces this today**, replacing the 2026-08-28 list
above rather than sitting beside it, in decreasing order of strength:

* `cargo tree -p cf-gears-example-server -i kube` with default features errors with
  "did not match any packages". This is the strongest single fact in the file: it is
  the whole binary, not one crate, and it is the waiver's central condition proven by
  a command anyone can run.
* `cargo test -p qa-environments` with **no** features passes (177 tests as of
  2026-09-04; the 2026-08-28 entry above said 142, and the difference is cluster-health
  and rename work, not a change to this property), which is only possible because
  `domain/` compiles with no `kube` in the tree. Unchanged by Phase C, and expires at
  Task 19 along with the feature it is about.
* `qa-plugin-k8s` names `kube`/`k8s-openapi` with no feature gate, and
  `qa-vhp-product-plugin` is its only dependent. A reader checks the whole qa-platform
  containment by reading two manifests. **Nothing enforces it**: a second crate adding
  a `kube` dependency would be caught by review or by nothing.
* `qa-environments`' `infra/observer/mod.rs` carries `#![cfg(feature =
  "platform-observation")]` *and* is reached through a `#[cfg]`-gated `pub mod
  observer`, so that gate cannot be lost by editing one line. Also unchanged, also
  expires at Task 19.
* `qa-runs`' `argo` feature is off by default and unaffected by any of this.
* **Nothing here is automatic.** There is still no qa-platform job in
  `.github/workflows/`, and the `cargo-deny`/dependency check this ADR's Confirmation
  criterion calls for **is still unimplemented** — `deny.toml` contains no such rule.
  It was unimplemented for the 2026-08-27 waiver, unimplemented for the 2026-08-28
  amendment, and is unimplemented for this one. Three widenings have now been recorded
  against a criterion that has never been enforced by anything but prose, and the rule
  is the single change that would convert every bullet above from a command someone
  has to remember to run into a build failure.

**Cross-reference.** `gears/qa-platform/docs/PRODUCT-PLUGINS-DESIGN.md` declares
`**Amends:** ADR-0001` in its own header; until this paragraph the link ran in one
direction only. §4.1 is the component view that argues the crate boundary, §9 is the
secret-containment argument across it, and §10's rollout table is where Task 19 —
the one-way door referenced above — is sequenced. Read §4.1's "strictly stronger"
sentence together with the weakening recorded here; neither is complete alone.

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
