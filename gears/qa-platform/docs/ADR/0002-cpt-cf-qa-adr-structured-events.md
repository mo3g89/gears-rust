---
status: accepted
date: 2026-08-12
---
# Runner reporting: replace stdout markers with typed execution events

**ID**: `cpt-cf-qa-adr-structured-events`

## Context and Problem Statement

Today the Python runner prints magic marker lines (`=== TEST_RESULT: ... ===`) to stdout; a manager-side poller scrapes completed workflow logs to reconstruct results after the fact. With Argo gone (`cpt-cf-qa-adr-serverless-execution`) and run state database-first, how should the runner report progress and results?

## Decision Drivers

* Post-hoc log parsing delays results until run completion and is fragile against log formatting drift.
* PRD requires incremental result visibility (`cpt-cf-qa-nfr-result-latency`: ≤ 5 s) and live logs.
* The test-facing contract (env vars, plan.yaml, TEST_META) must stay frozen so repositories run unmodified (`cpt-cf-qa-fr-migration-runner-contract`).
* The serverless execution channel already carries an event/stream concept in its spec; a parallel reporting path would duplicate it.

## Considered Options

* Typed execution events emitted by the runner over the execution-plane event channel
* Keep stdout markers, parse the live log stream instead of completed logs
* Runner POSTs results directly to a qa-runs callback API

## Decision Outcome

Chosen option: "Typed execution events over the execution-plane event channel", because it makes the result contract a versioned schema instead of a log-format convention, gives incremental ingestion for free via the same `watch` stream the executor port already requires, and keeps the runner free of control-plane addressing/auth concerns.

### Consequences

* Event vocabulary: `run.started`, `test.file`, `test.started`, `test.result{passed|failed|skipped}`, `launch.linked` (ReportPortal), `run.finished` — GTS-registered schemas, versioned, never mutated.
* The runner's marker-printing layer is replaced by an event emitter; everything test-facing (env vars, pytest invocation, ReportPortal reporting) is untouched.
* Raw stdout/stderr remains the run log (streamed live via SSE, archived to file-storage); it is no longer parsed for data.
* qa-runs ingestion is a single consumer of the `watch` stream: upsert result rows, fan out SSE, republish to event-broker.
* The source system's run-results poller disappears; restart recovery is `watch` re-attach, so the serverless channel must support resuming a stream (upstream requirement on serverless-runtime).

### Confirmation

Contract tests: a recorded event stream replayed into ingestion produces exactly the expected run/test rows. E2E: reference repository run shows results incrementally with the log free of magic markers. Schema presence verified in Types Registry.

## Pros and Cons of the Options

### Typed events over the execution channel

* Good, because results become schema-validated data with latency bounded by the stream, not the run duration.
* Good, because runner needs no control-plane URL, credentials, or retry logic — the execution plane carries the events.
* Good, because log content and result data are fully decoupled.
* Bad, because the runner emitter and ingestion must version-negotiate via GTS as the vocabulary evolves.
* Bad, because it adds a hard requirement (resumable event stream) on the not-yet-built runtime.

### Keep markers, parse the live stream

* Good, because the runner is unchanged.
* Bad, because the fragile text contract survives and now must be parsed under streaming conditions (partial lines, interleaving).
* Bad, because markers pollute logs and any test printing a marker-shaped line corrupts results.

### Direct callback API from runner to qa-runs

* Good, because it works over plain HTTP with no execution-plane features needed.
* Bad, because the runner must discover, authenticate to, and retry against the control plane from inside arbitrary execution networks — exactly the coupling the platform's egress/ingress model avoids.
* Bad, because run-scoped credentials for the callback would need issuing and revoking per run.

## More Information

The `TEST_LAUNCH_ID` marker's role is covered by `launch.linked`. Marker semantics being replaced are documented in the source repository's README (runtime model section).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-qa-fr-runs-results-ingest` — defines the ingestion input
* `cpt-cf-qa-fr-migration-runner-contract` — constrains what may change in the runner
* `cpt-cf-qa-nfr-result-latency`, `cpt-cf-qa-nfr-log-latency` — enabled by stream-based reporting
* `cpt-cf-qa-contract-runner` — this is that contract's rationale
