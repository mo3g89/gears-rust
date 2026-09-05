---
status: accepted
date: 2026-08-12
---
# Subsystem decomposition: four domain gears plus a UI gear

**ID**: `cpt-cf-qa-adr-four-gear-decomposition`

## Context and Problem Statement

The source control plane is one ~30k-LOC Rust binary with 24 service modules. Converting it to Fabric gears requires choosing a decomposition: one gear, a few, or many? The cut determines contract count, schema ownership, and how independently the parts can evolve.

## Decision Drivers

* Fabric convention: gears are single-purpose with SDK-mediated boundaries; oversized gears defeat the model, over-fine gears multiply lifecycle/SDK/migration overhead.
* The source service layer already clusters into four low-chatter groups (catalog, run orchestration, environments, analytics/integrations).
* The hot run path must not share fate with analytics workloads (different write patterns, availability needs).
* Each gear owns its schema; cross-schema SQL is prohibited, so the cut must match data-access patterns.

## Considered Options

* Four domain gears (qa-catalog, qa-runs, qa-environments, qa-insights) + qa-ui
* Two gears (qa-core + qa-insights)
* Six or more fine-grained gears (scheduler and JIRA integration as separate gears)

## Decision Outcome

Chosen option: "Four domain gears + qa-ui", because it matches the seams the code already exhibits (each gear maps to a distinct actor need and a coherent table set), keeps the synchronous launch path within two SDK hops (runs→catalog, runs→environments), and isolates analytics behind asynchronous events.

### Consequences

* Four SDK contracts to design and version (`qa-catalog-sdk`, `qa-runs-sdk`, `qa-environments-sdk`, `qa-insights-sdk`); qa-ui needs no SDK.
* Table ownership fixed as in DESIGN §3.7; notably `run_results` (run-level) in qa-runs vs `test_results` (analytical) in qa-insights, connected by events, referenced by ID only.
* The scheduler stays inside qa-runs (it has no consumer other than launching runs); JIRA integration stays inside qa-insights (its only outputs are bug views, skip-lists, and rerun triggers) — both can be extracted later without re-cutting other contracts.
* One back-edge exists: qa-insights → qa-runs SDK for auto-rerun launches; it is contract-mediated and non-cyclic at the crate level (insights depends on runs-sdk, not runs).
* Directory layout `gears/qa-platform/{qa-catalog,qa-runs,qa-environments,qa-insights,qa-ui}` with subsystem docs at `gears/qa-platform/docs/`.

### Confirmation

Architecture lints (`cargo gears lint`) + dependency review: no cross-schema SQL, no non-SDK inter-gear imports, no cycles. DESIGN component boundaries reviewed against the source module inventory for coverage (every source service module has exactly one owning gear).

## Pros and Cons of the Options

### Four domain gears + qa-ui

* Good, because gear purpose statements are one line each and every source module lands unambiguously in one gear.
* Good, because the launch path stays synchronous-simple while analytics is fully decoupled.
* Good, because a later extraction toward finer gears (option 3) remains possible without breaking the four public contracts.
* Bad, because four SDKs is real design and versioning work up front.

### Two gears (qa-core + qa-insights)

* Good, because fewer contracts, fastest to spec and build.
* Bad, because qa-core would be a ~25k-LOC gear where catalog, queue, and environments concerns re-entangle — recreating the monolith inside one gear boundary.
* Bad, because environments (shared, slow-changing, admin-owned) and runs (hot, high-write) have conflicting schema and availability profiles.

### Six or more fine-grained gears

* Good, because maximal isolation and per-concern deployability.
* Bad, because scheduler-without-runs and jira-without-insights have no independent consumers — pure overhead (SDK, migrations, lifecycle, docs per gear) with no coupling reduction.

## More Information

Decomposition selected by the user from three presented approaches (2026-08-11). Naming (`qa-platform`, `qa-*` crate prefix) confirmed separately; follows the `bss-*` subsystem precedent for crate naming.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-qa-component-catalog`, `cpt-cf-qa-component-runs`, `cpt-cf-qa-component-environments`, `cpt-cf-qa-component-insights`, `cpt-cf-qa-component-ui` — the components this decision creates
* `cpt-cf-qa-principle-async-insights` — realized by the catalog/insights cut
* `cpt-cf-qa-interface-sdks` — the contract surface this decision sizes
