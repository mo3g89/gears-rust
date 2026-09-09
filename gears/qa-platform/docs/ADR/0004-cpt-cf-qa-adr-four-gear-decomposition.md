---
status: accepted
date: 2026-08-12
---
# Subsystem decomposition: four domain gears

**ID**: `cpt-cf-qa-adr-four-gear-decomposition`

## Context and Problem Statement

QA Platform covers test catalogues, target environments, run orchestration and analytics. Those
are related but not identical concerns, with different write rates, different consistency needs and
different failure tolerances. How should the subsystem be split into gears?

## Decision Drivers

* A failure in analytics must not stop a run from launching.
* Run orchestration is the only latency-sensitive part; analytics is throughput-sensitive.
* Cross-gear calls cost more than in-process calls and are harder to reason about, so the split
  should minimise them.
* Each gear should be small enough to hold in one reader's head.

## Considered Options

* Four domain gears: environments, catalog, runs, insights
* One gear for the whole subsystem
* Two gears: a control plane and a read model

## Decision Outcome

Chosen option: **four domain gears**, because the seams fall naturally along four different
questions — *where does a test run*, *what is there to run*, *run it*, *what happened* — and
because cross-gear chatter turns out to be low by construction.

The call graph is deliberately thin:

* `qa-runs` reads `qa-catalog` and `qa-environments` synchronously, once, at launch time.
* `qa-insights` reads `qa-runs` through its SDK on its own reconcile sweep, never on the run path
  ([ADR-0009](./0009-cpt-cf-qa-adr-insights-reconcile.md)).
* `qa-catalog` and `qa-environments` call nothing inside the subsystem.

There is no shared database. Each gear owns its schema; cross-gear reads go through SDK clients.

### Consequences

* Good, because analytics can be down, slow or restarting with no effect on a launch.
* Good, because each gear's schema, migrations and tests stay independently comprehensible.
* Good, because the latency-sensitive gear is small and has few dependencies.
* Bad, because a launch does pay for two cross-gear reads.
* Bad, because facts that span gears — a run, its plan and its environment — have to be joined by
  a caller instead of by the database, which the UI does in the browser.
* Bad, because four gears is four sets of migrations, configuration and boilerplate.

### Confirmation

* No gear's `infra/storage` names another gear's table.
* `qa-insights` has no caller on the `qa-runs` launch path.
