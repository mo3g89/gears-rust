# E2E Scenarios — QA Platform

End-to-end scenarios for the subsystem. Each names its preconditions, the steps a caller takes, and
what must be observable afterwards. They are written against the public surface — REST, the UI, and
the Helm deployment — not against internals.

<!-- toc -->

- [S1. Onboard a product and register an environment](#s1-onboard-a-product-and-register-an-environment)
- [S2. Register and sync a test repository](#s2-register-and-sync-a-test-repository)
- [S3. Launch a plan against a free environment](#s3-launch-a-plan-against-a-free-environment)
- [S4. Launch against a busy environment](#s4-launch-against-a-busy-environment)
- [S5. Cancel a running run](#s5-cancel-a-running-run)
- [S6. Survive a control-plane restart mid-run](#s6-survive-a-control-plane-restart-mid-run)
- [S7. Collect case counts](#s7-collect-case-counts)
- [S8. Schedule a nightly run](#s8-schedule-a-nightly-run)
- [S9. Reconcile into analytics](#s9-reconcile-into-analytics)
- [S10. Correlate a failure with JIRA](#s10-correlate-a-failure-with-jira)
- [S11. Notify once, and audit it](#s11-notify-once-and-audit-it)
- [S12. Run against a VHI environment over SSH](#s12-run-against-a-vhi-environment-over-ssh)
- [S13. Scope every list to a product](#s13-scope-every-list-to-a-product)
- [S14. Install the stack](#s14-install-the-stack)

<!-- /toc -->

## S1. Onboard a product and register an environment

**Preconditions**: the deployment is up; a product plugin is registered.

1. `GET /qa/v1/product-plugins` — the plugin's instance id, credential schema and observed schema
   are returned.
2. `POST /qa/v1/products` with that instance id.
3. `GET /qa/v1/product-plugins` from the UI renders the credential form from `credential_schema`
   with no product-specific UI code.
4. `POST /qa/v1/environments` with the filled form.
5. `GET /qa/v1/environments/{id}`.

**Must hold**

* The environment exists and is bound to the product.
* Secret fields are **not** returned by the read, and are **not** present in the row: the row holds
  credstore references.
* After observation, `observed_version`, `observed_build` and `health_state` are populated.
* `POST /qa/v1/environments/{id}/refresh` re-observes on demand.
* An environment that has never been observed reads back successfully with null observed fields —
  that is a valid state, not an error.

## S2. Register and sync a test repository

**Preconditions**: a product exists; an SSH key is registered if the remote needs one.

1. `POST /qa/v1/ssh-keys` — a name and key material.
2. `POST /qa/v1/test-repos` referencing the product and, optionally, the key.
3. `POST /qa/v1/test-repos/{id}/sync`.
4. `GET /qa/v1/test-repos/{id}/branches`, then `GET /qa/v1/plans?repo_id=…`.

**Must hold**

* `GET /qa/v1/ssh-keys/{id}` returns a fingerprint and never the private key.
* After sync, `last_synced_at` is set and the branch list is populated.
* Plans are discovered from the work tree and identified by `(repo_id, path)`.
* A sync failure sets `sync_error` on the repository, readable through the API.

## S3. Launch a plan against a free environment

**Preconditions**: S1 and S2 complete; the environment holds no lease.

1. `POST /qa/v1/runs` with a plan target and the environment.
2. Open `GET /qa/v1/runs/{id}/logs` as an `EventSource`.
3. Poll `GET /qa/v1/runs/{id}`.

**Must hold**

* The run never enters `queued`: it goes `created → dispatching → running`.
* `GET /qa/v1/environments/{id}/lease` shows the run as a holder.
* Log lines arrive on the stream while the run is live.
* Tallies (`passed`, `failed`, `skipped`, `total`) advance **during** the run, not only at the end.
* On completion the state is terminal, `finished_at` is set, and the lease is released.
* `GET /qa/v1/runs/{id}/logs` after completion serves the archived copy.

## S4. Launch against a busy environment

**Preconditions**: an exclusive run holds the environment.

1. `POST /qa/v1/runs` targeting the same environment.
2. `GET /qa/v1/queue?environment=…`.
3. Let the first run finish.

**Must hold**

* The second run is `queued`, not rejected — a busy environment never fails a launch.
* It appears in the queue listing and can be cancelled from there.
* When the first run finishes and the lease releases, the queued run starts automatically within
  the dispatcher interval.
* `POST /qa/v1/queue/{id}/force-start` bypasses the FIFO for one entry.
* A run with **no** environment launched during all of this is never queued and never blocked.

## S5. Cancel a running run

1. Launch a run and wait for `running`.
2. `POST /qa/v1/runs/{id}/cancel`.

**Must hold**

* The run reaches `canceled` (one `l`); the queue row reaches `cancelled` (two).
* The execution is stopped at the backend, not merely marked cancelled in the database.
* The environment lease is released.
* A second cancel is rejected by the state-machine guard rather than rewriting `finished_at`.

## S6. Survive a control-plane restart mid-run

**Preconditions**: a long-running run is `running` with log output flowing.

1. Restart the gears process.
2. Reconnect to `GET /qa/v1/runs/{id}/logs`.
3. Wait for the run to finish.

**Must hold**

* The run is still `running` after the restart — state came from the database, not from the
  execution backend's objects.
* The control plane re-attaches to the live execution rather than abandoning it.
* Results reported during the restart window are not lost.
* The run reaches its correct terminal state and the lease is released.

## S7. Collect case counts

1. `POST /qa/v1/collect/{repo_id}` for a branch.
2. Wait for the collect run to finish.
3. Query the recorded counts.

**Must hold**

* The collect run carries **no environment**, is never queued, and never takes a lease.
* An exact case count is recorded per `(repo, branch, test_file)`.
* The counts come from case enumeration, not from parsing run log output.
* The collect run is attributed to a product through its repository, so it appears in that
  product's lists.

## S8. Schedule a nightly run

1. `POST /qa/v1/schedules` with a cron expression and a target.
2. `PUT /qa/v1/schedules/{id}/notifications`.
3. Let at least one due instant pass.

**Must hold**

* Exactly one run is launched per due instant, even across dispatcher sweeps.
* The launched run carries `schedule_id` and `source`.
* A disabled schedule fires nothing.
* Per-schedule notification settings apply to the runs it launches.

## S9. Reconcile into analytics

**Preconditions**: at least one run has finished.

1. Wait for the reconcile sweep.
2. `GET /qa/v1/test-results` and `GET /qa/v1/test-case-results`.
3. `GET /qa/v1/dashboard?product_id=…`.

**Must hold**

* Per-file and per-case rows exist for the finished run, the latter keyed by pytest `nodeid`.
* The dashboard reflects the run.
* Stopping qa-insights, finishing more runs, and restarting it ingests the missed runs from the
  watermark, with no duplicates.
* **Nothing on the launch path ever calls qa-insights**: with qa-insights stopped, launching and
  completing a run works unchanged.

## S10. Correlate a failure with JIRA

1. `PUT /qa/v1/settings/jira` and `PUT /qa/v1/settings/jira-poller`.
2. Produce a failing test correlated to an issue.
3. `GET /qa/v1/jira/open-bugs`.
4. Resolve the issue upstream and let the poller run.

**Must hold**

* The failing test correlates to its issue and appears in the open-bug listing.
* The API token is stored as a credstore reference and never returned by the settings read.
* JIRA egress goes through the platform gateway.
* With auto-rerun enabled, resolving the issue re-launches the corresponding run.

## S11. Notify once, and audit it

1. `PUT /qa/v1/settings/notifications`.
2. `POST /qa/v1/settings/notifications/preview`, then `/test`.
3. Let a run fail.
4. `GET /qa/v1/settings/notifications/log`.

**Must hold**

* Preview renders without sending; test sends without a run.
* A failure notifies once. Re-running the reconcile sweep does **not** notify again.
* Every attempt — success or failure — appears in the audit log with channel, event type, outcome
  and detail.
* The Slack webhook is a credstore reference, never returned by the settings read.

## S12. Run against a VHI environment over SSH

**Preconditions**: a VHI product bound to the VHI plugin; a reachable management node.

1. Register an environment with the VHI credential form.
2. Observe it.
3. Launch a suite against it.

**Must hold**

* Observation reports the product version and build read from the node, and its node topology.
* The private key never appears as a file, in `argv`, or in an environment variable — it reaches a
  short-lived agent through a pipe.
* A secret passed to a remote command goes over **stdin**.
* No credential material appears in any error message, log line or API response, including on the
  failure paths: a wrong password yields a failure whose detail is a fixed string.
* The run reaches the node and reports results like any other run.

## S13. Scope every list to a product

**Preconditions**: two products, each with repositories, environments and finished runs.

1. Select product A in the UI.
2. Visit dashboard, runs, schedules, plans, custom plans, environments.
3. Switch to product B.

**Must hold**

* Every list shows only the selected product's rows.
* A run is attributed through its **target**, not its environment — including collect runs, which
  have no environment at all and must still appear under their product.
* The dashboard is scoped by the server via `product_id`; the other surfaces filter in the browser.
* Where filtering happens in the browser, the page says so rather than presenting itself as
  server-scoped.

## S14. Install the stack

1. `helm install` the chart onto a clean cluster.
2. Wait for the jobs and deployments.
3. Open the UI and sign in.

**Must hold**

* Migrations run before the gears start.
* The designated tenant is seeded.
* TLS material is generated for Keycloak and the UI.
* The RBAC the execution backend needs is created.
* The UI is served, reaches `/qa/v1` same-origin, and the log stream works — including the
  token bridge on the SSE location.
* The access log for the SSE location does **not** contain the token; other locations are
  unaffected.
