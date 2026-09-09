---
status: accepted
date: 2026-08-18
---
# qa-insights ingests by sweeping qa-runs, not by being called

**ID**: `cpt-cf-qa-adr-insights-reconcile`

## Context and Problem Statement

`qa-insights` holds the historical read model: per-file and per-case results, dashboards,
analytics, JIRA correlation, notifications. Those rows derive from finished runs, which `qa-runs`
owns.

How should data get from one gear to the other?

## Decision Drivers

* A launch must never fail, slow down or block because analytics is unavailable.
* Ingestion must recover after a control-plane restart without an operator replaying anything.
* Notifications must not fire twice for the same run.
* The platform's event bus has no durable backend in this deployment, so an event delivered while
  a consumer is down is simply gone.

## Considered Options

* A reconcile sweep: qa-insights polls qa-runs' SDK for runs finished since a watermark
* A synchronous call from the qa-runs ingest path into qa-insights
* Publish-subscribe over the platform event bus

## Decision Outcome

Chosen option: **a reconcile sweep**. `qa-insights` walks runs that finished after
`qa_ingest_watermarks.last_reconciled_finished_at`, writes their result rows, and advances the
watermark. The qa-runs launch and ingest paths make no call into qa-insights at all.

Recovery is therefore a property of a table rather than of a retry policy: whatever happened while
qa-insights was down, the watermark says where to resume, which is what makes
`cpt-cf-qa-nfr-ingest-recovery` meetable.

`qa_leader_claims` gates the sweep per tenant, so only one replica reconciles, polls JIRA or sends
a notification for a given tenant. `qa_run_notifications` records what has already been sent, so a
re-swept run does not re-notify.

Pub/sub was rejected on a concrete fact rather than a preference: the event bus available here has
no durable backend, so an event delivered while qa-insights is restarting is lost, and the design
would need the watermark anyway as a backstop. Given that, the watermark alone is simpler.

### Consequences

* Good, because analytics can be down, slow or restarting with no effect on launching runs.
* Good, because recovery needs no operator action and no replay tooling.
* Good, because the sweep is idempotent, so re-running it is always safe.
* Bad, because results reach analytics on a sweep interval rather than immediately, so the
  dashboard trails the run list.
* Bad, because the sweep reads runs qa-insights has already seen when the watermark is coarse,
  which costs work proportional to interval rather than to new data.
* Bad, because `qa-insights` depends on `qa-runs`' SDK shape, so a change there is a change here.

### Confirmation

* No caller on the `qa-runs` launch or ingest path references `qa-insights`.
* A restart test asserts the watermark advances and no run is ingested twice.
