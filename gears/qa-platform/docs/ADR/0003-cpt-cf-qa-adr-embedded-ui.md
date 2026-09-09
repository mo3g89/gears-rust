---
status: accepted
date: 2026-08-25
---
# UI delivery: the SPA ships as its own image alongside the gears

**ID**: `cpt-cf-qa-adr-embedded-ui`

## Context and Problem Statement

QA Platform has a browser interface: dashboards, run detail with a live log, plan browsing,
environment management, analytics. It has to be delivered somewhere, reach `/qa/v1` without a
cross-origin dance, and stream server-sent events for run logs.

How should the SPA be served?

## Decision Drivers

* The log view needs `EventSource`, which cannot set an `Authorization` header.
* The deployment should not require object storage or a CDN to be useful.
* The UI and the gears version together — an API change and its client change land in one commit.
* One deployable unit per concern keeps the Helm chart legible.

## Considered Options

* A dedicated nginx image, deployed beside the gears and proxying `/qa/v1` to them
* Static assets embedded in the gear binary and served from a gear route
* Static assets uploaded to object storage or a CDN

## Decision Outcome

Chosen option: **a dedicated nginx image**. `deploy/docker/qa-platform-ui.Dockerfile` builds the
SPA and bakes it into nginx; `ui-deployment.yaml` runs it; `default.conf.template` proxies `/qa/v1`
to the gears service.

nginx is also where the SSE credential problem is solved. `EventSource` cannot set headers and the
gears read a credential only from `AUTHORIZATION`, so the log-stream location — and only that
location — maps `?access_token=<jwt>` onto an `Authorization` header.

That bridge has a cost, and it is accepted with two mitigations rather than waved through:

* The header must never be added to a location the SPA calls with a real `Authorization`; the
  fallback protects exactly one URL.
* nginx's default `combined` format would write the whole request line, token included, to stdout
  — which in this deployment goes wherever the cluster ships container logs. A dedicated
  `log_format sse_no_query` redacts the query string on that location, and only that location.

### Consequences

* Good, because the SPA and the gears are same-origin, so no CORS configuration and no preflight
  on the streaming path.
* Good, because the deployment needs no object storage or CDN.
* Good, because the UI scales and restarts independently of the gears.
* Good, because nginx is the natural place for the token bridge, which nothing else in the stack
  could do as cheaply.
* Bad, because a token appears in a query string, which is a weaker position than a header even
  with the log redaction in place; a proxy in front of nginx that logs request lines would
  reintroduce the exposure.
* Bad, because it is a second image to build, version and ship.

### Confirmation

* `deploy/helm/tests/test_nginx_template.sh` asserts the rendered config matches
  `nginx.conf.baseline` byte for byte, that the SSE location redacts the token from its access
  log, and that it suppresses upstream-failure error-log entries for that location only.
