---
status: accepted
date: 2026-08-12
---
# Runner reporting: typed execution events

**ID**: `cpt-cf-qa-adr-structured-events`

## Context and Problem Statement

A runner produces two kinds of output: text a human reads, and facts the control plane must record
— this test started, this test passed in 1.2 s, this run finished. The second kind has to reach
`qa_runs` and `qa_run_test_results` reliably enough that `cpt-cf-qa-nfr-result-latency` (a result
visible within 5 s p95) is meetable.

How should a runner report those facts?

## Decision Drivers

* A result must be persistable incrementally, as it happens, not only at the end of a run.
* The reporting contract is consumed by test authors who do not own the control plane, so it must
  be explicit and versionable.
* The control plane must be able to distinguish "the run reported nothing" from "the run reported
  a failure".
* Log text must stay free-form: it is for humans and must not become a parsing surface.

## Considered Options

* Typed execution events over the `RunExecutor::watch` stream
* Markers embedded in the runner's stdout, parsed by the control plane
* A JSON report file written at the end of the run

## Decision Outcome

Chosen option: **typed execution events**. `RunExecutor::watch` yields a stream of a closed enum:

| Event | Carries |
|-------|---------|
| `Started` | — |
| `TestResult(TestObservation)` | file, name, `nodeid`, status, duration, reason, ticket |
| `Log { node, line }` | one line of runner output |
| `Finished { outcome }` | terminal outcome and node health |

Log text travels as its own event variant, which is what keeps it free-form: it is carried, never
parsed. Results travel as a struct with a schema, so adding a field is a typed change and not a
new regular expression.

The ingest path folds each event into the database under `SERIALIZABLE` with a bounded retry, so
the per-test tally and the terminal write cannot interleave into a phantom read.

### Consequences

* Good, because results are incremental: the tally on `qa_runs` is correct while a run is still
  going.
* Good, because a schema change is a compile error rather than a silently mismatched pattern.
* Good, because log output stays a human artifact, so a test author cannot break ingestion by
  printing something that looks like a marker.
* Good, because `Finished` carries node health separately from test outcome, so a run whose tests
  passed on a node that then died does not report success.
* Bad, because the runner and the control plane now share a typed contract that must be versioned
  together.
* Bad, because an adapter for a backend that only produces text has to synthesize the events, and
  the fidelity of that synthesis is the adapter's problem.

### Confirmation

* `ExecutionEvent` is a closed enum in `qa-runs/src/domain/ports/run_executor.rs`.
* `domain::service::ingest_races_pg_tests` falsifies the interleaving races against real Postgres.
* No module under `qa-runs/src/domain/` parses log text for results.

## Pros and Cons of the Options

### Typed execution events

* Good, because incremental, schema'd and testable.
* Bad, because it is a contract two components must agree on.

### Stdout markers

* Good, because a runner needs no transport beyond printing.
* Bad, because log text becomes a parsing surface, so an ordinary `print` can corrupt state.
* Bad, because the parser is untyped and its failures are silent.

### End-of-run JSON report

* Good, because it is simple and atomic.
* Bad, because nothing is visible until the run ends, which cannot meet a 5 s result latency.
* Bad, because a run that dies mid-way reports nothing at all.
