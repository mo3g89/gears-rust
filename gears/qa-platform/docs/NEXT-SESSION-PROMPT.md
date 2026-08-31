# qa-platform — next session

**Updated 2026-08-31, evening.** The deploy landed and BOTH open problems are
fixed and verified on `10.136.20.200`. Read section 0 for what is now true.

Repo `/home/serhii/Jelastic/projects/fabric/gears-rust`, branch
`feature/qa-platform-specs`, HEAD `f97d30708`.
**58 commits unpushed** (counted against `fork/feature/qa-platform-specs`).
`./push-to-fork.sh` when ready; `origin` (constructorfabric) stays forbidden.

---

## 0. STATE OF THE DEPLOYMENT — both problems RESOLVED

Image `cf-gears-qa-platform:deploy-20260831-164728` is live (helm revision 8).
`verify-k8s.sh`: **28 PASS, 0 FAIL, 0 NOTE, exit 0**.

### 0.1 Run log persistence — WORKS, acceptance test passed

The earlier session's deploy never landed, so the feature had never run on the
remote — that, and nothing else, is why `monitoring-cms-e2e-tests-1` showed an
empty log pane. Now deployed and proven:

* Launched `monitoring-cms-e2e-tests-2` (rerun of the user's example run).
  It persisted **22,186 lines / 1,982,993 chars** into `qa_run_logs` while running.
* `GET /qa/v1/runs/{id}/logs` served **2,385,711 bytes**, then the gears
  Deployment was restarted, then the same call served **2,385,711 bytes** —
  byte-identical across a pod restart. That is the acceptance test.
* Runs that finished BEFORE this image (incl. `monitoring-cms-e2e-tests-1`)
  have no archived text and never will: their log only ever lived in a pod that
  is gone. Not a defect — the condition the feature removes going forward.

### 0.2 Analytics and the Dashboard — RESOLVED, and no event broker is needed

**Both hypotheses in the previous version of this document were wrong.**

Hypothesis A (a `26.5` vs `26.5.0` version mismatch) is disproved: every version
string in the live database is `26.5` — `qa_platforms.observed_version`,
`qa_runs.app_version`, and `ingest.rs:587` maps `product_version: run.app_version`.
The `26.5.0` on screen is version + `app_build` (`"0"`), not a stored mismatch.

The real cause was that **qa-insights had ingested zero rows**, so there was
nothing for any filter to match:

1. The `event-broker` gear is a **stub** — `gears/system/event-broker/event-broker/src/module.rs`
   registers no routes, runs no worker and registers no `EventBrokerApi` client
   (handler bodies are ticketed as #4346/#4347). Its database has no tables.
2. So qa-runs publishes nothing and qa-insights' event ingest is DISABLED,
   leaving the reconcile sweep as the only ingest path.
3. The sweep enumerates tenants with `SELECT DISTINCT tenant_id FROM qa_test_results`
   (`domain/service/tenants.rs`) — a table only the event path writes. Zero rows
   meant zero tenants meant the sweep no-opped forever. A closed loop.

**Fixed operationally, with no code change**, by the escape hatch the gear's own
startup WARN names: `POST /qa/v1/insights/rebuild` with a `{from,to}` window
(`{"from":"2026-08-29T00:00:00Z","to":"2026-09-01T00:00:00Z"}`) returned
`{"scanned":7,"replayed":7}`.

**It is self-sustaining now, and this is the important part:** the tenant has
result rows, so the reconcile ticker sees it. The next run
(`monitoring-cms-e2e-tests-2`) was ingested **automatically, 68 rows, with no
broker and no second rebuild**. That is the legacy model — legacy's
`manager/src/services/run_results_poller.rs` polls runs and persists what has no
results yet. **The event broker is not required for parity and never was.**

Live now: Dashboard `total_runs 6`, `failed_24h_count 71`, `pass_rate_24h 0.59`,
10 recent failures. Analytics `total 221 / passed 16 / failed 3`,
case-level `86 passed / 38 failed`.

### 0.3 Run detail page was laggy — FIXED (UI only)

The user reported `/runs/monitoring-cms-e2e-tests-2` lagging badly. It was a
**regression introduced by the log-persistence feature itself**: before it, a
finished run's SSE stream was empty, so `LogViewer` fell back to the one-shot
poll. Once finished runs had archived logs, the stream replayed all 22,186 lines
one at a time, and every line re-ran the whole chain —
`setMessages(prev => [...prev, line])` (array copy), `wsMessages.join('\n')`
(rebuild the whole 2.4 MB string), `logs.split('\n')`, then `buildBlocks`'
regex pass over every line. Four quadratic passes; the join alone churns ~26 GB
of string allocation for one page load.

Legacy never had this: `manager/src/routes/runs.rs`'s `api_logs` returns the
whole log as one `String` and `manager-ui` splits it once. Our block-collapsing
render is already a faithful port of legacy's (same `buildBlocks`, same
"only failed blocks open" default) — the render was never the problem.

Three changes, all in `qa-platform-ui`, no Rust and no API change:

* `LogViewer` takes `isTerminal`; a terminal run opens **no** `EventSource`
  (`useRunLogStream(null)`) and reads the single fetch instead — legacy's model.
  `RunDetailPage` passes `!activeRun`.
* `useRunLogs(name, {live})` — `refetchInterval` is `false` for a terminal run.
  It was unconditionally 5000, re-downloading and re-parsing 2.4 MB every five
  seconds forever for a log that cannot change.
* `useRunLogStream` batches commits: lines buffer in a ref and flush on a
  `setTimeout(0)` (not rAF — rAF stalls in a background tab). Live runs keep
  streaming, but the quadratic passes are bounded by tick rate, not line count.

`tsc` clean, **195/195 vitest pass**. Three `useRunLogStream` tests needed
updating because batched delivery is genuinely async now — assertions wrapped in
`waitFor` / a timer advance, not weakened.

Deployed as `deploy-20260831-172027`; **51 PASS, 0 FAIL** end to end (this run
also cleared the `provision-workflow-secret` step that failed in 0.4). Verified
in the served bundle: `push(P),v===null&&(v=setTimeout`,
`refetchInterval:t?5e3:!1`, and the `?null:` stream guard.

**Note for the next deploy:** a UI-only source change still invalidates the
Docker `COPY` layer and forces a full cargo rebuild, so it is a ~20 minute
deploy, not a fast one.

### 0.4 THE ONE THING STILL OPEN from this

The bootstrap deadlock is fixed *for this deployment's tenant only*. **A fresh
deployment, or any new tenant, hits the identical deadlock** and needs the same
manual rebuild. The durable fix is to enumerate the ticker's tenants from
qa-runs instead of from qa-insights' own result rows (legacy's model), which
needs a cross-tenant tenant/runs listing on qa-runs — its client here exposes
only tenant-scoped calls. **Not yet designed or built. Ask the user before
starting it.**

### 0.5 A deploy bug fixed in passing

`deploy/argo/provision-workflow-secret.sh` and
`provision-platform-kubeconfig-secret.sh` were modified locally *after* the
deploy's rsync, so the remote ran a stale copy still pointing at the deleted
`deploy/compose/` realm path; the deploy died on its last step. **`deploy-k8s.sh`
reported `exit=1` while the wrapper notification said 0** — the log is the
authority, not the notification. Re-synced by hand and re-run; the secret is
provisioned. Nothing in the driver was changed.

### 0.6 Two small things still needing the user

* **`sudo rmdir`** (needs their password) for three empty root-owned directories
  Docker left behind:
  ```bash
  cd gears/qa-platform/deploy && sudo rmdir \
    compose/.generated/k3s-kubeconfig.yaml \
    compose/.generated/qa-environments-argo.yaml \
    compose/.generated/qa-runs-argo.yaml compose/.generated compose
  ```
* **`npm run gen:api`** while the gear is live on `:8087`. One `@description` JSDoc
  line in `qa-platform-ui/src/api/generated/openapi.d.ts` was hand-edited because
  that command needs a running gear. It was verified byte-identical to the Rust
  doc and comment-only, but it should be regenerated properly.

### 0.7 Two misconfigurations the gears log flags, not yet addressed

* `collect_report_base_url` is empty — the collect job's callback URL has no
  scheme or host, so the runner cannot report exact case counts.
* `collect_report_signing_secret` is empty or too short — **every** collect
  report is refused with Forbidden (fail-closed). Set a random per-deployment
  secret of at least 16 characters.

Together these are why `case_expected` is the catalog's static 1262.

---

## 2. WHAT LANDED THIS SESSION

### 2.1 Run log persistence — deployed, and the acceptance test PASSED (see 0.1)

A finished run's log used to vanish: it lived only in a process-local buffer capped
at 32 runs (`MAX_RETAINED_RUNS`), so it died on eviction or a pod restart — and the
gears pod restarted 3 times on its first deploy. That is why `authentication-1`
showed nothing.

Now: a `qa_run_logs` table, a `RunLogsRepository` appending in-statement, a
`RunLogArchive` of per-run pending buffers flushed by the dispatcher tick and by
`IngestService::finish` (serialized per run by an `in_flight` set), recording of
every fanned-out line, and a read path where the terminal branch of
`GET /qa/v1/runs/{id}/logs` serves the archived text and falls back to the
in-memory tail for the ~192 runs that finished before the table existed.

* Design: `docs/superpowers/specs/2026-08-31-run-log-persistence-design.md`
* Plan: `docs/superpowers/plans/2026-08-31-run-log-persistence.md`
* **Ledger with every finding and ruling:**
  `.superpowers/sdd/2026-08-31-run-log-persistence/progress.md` — read this if you
  need to know why anything is the way it is.

Verified locally: clippy 0 on both feature sets, `cargo test -p qa-runs` 890 pass,
Postgres tier green.

**Decisions the user made, do not silently revisit:** no per-run size cap; retention
is the foreign-key cascade alone; no bound on concurrent readers of a finished run's
log (they were asked and said leave it as is).

**Known and accepted:** the terminal branch returns before
`subscribe_with_replay`, so `MAX_SUBSCRIBERS_PER_RUN` does not gate it. One caller
issuing N concurrent GETs on a large run holds N uncapped copies. Surfaced to the
user; they chose to leave it.

### 2.2 The chart is portable and the compose stack is deleted

The user's complaint: *"why here hardcoded values for my dev stand? it is a helm
repo and should be available from any k8s installation"*.

* `publicOrigin` and the driver's `--target` / `--public-origin` are now REQUIRED.
  A bare `helm install` or a bare `deploy-k8s.sh` refuses rather than silently
  deploying to one particular node.
* **`clusterDns` is discovered, not configured.** The UI image reads the first
  nameserver from its own `/etc/resolv.conf`
  (`qa-platform-ui/docker-entrypoint.d/15-resolver-from-resolv-conf.envsh`), which
  is correct under k3s, kubeadm and Docker alike. Two environment-specific defaults
  baked into the image itself went too: `ENV NGINX_RESOLVER=127.0.0.11` and
  `ENV GEARS_UPSTREAM=http://gears:8087`.
* `deploy/compose/` and its driver `deploy/remote/sync.sh` are gone.
  **`render-realm.sh` was a hard dependency of the k8s driver, not a compose
  script** — it and its realm JSON moved to `deploy/realm/`.
* `deploy/helm/tests/test_no_environment_hardcode.py` fails if a
  deployment-specific address becomes a default under `deploy/` again.
  Break-tested.

All five helm tests pass. `deploy/remote/vhp-kubeconfig.yaml` holds real
credentials, is untracked, and is covered by a `.gitignore` glob — leave it that
way.

---

## 3. A MISTAKE FROM THIS SESSION, SO IT IS NOT REPEATED

The first deploy attempt aborted because **a deploy script was edited while it was
executing.** Bash reads scripts incrementally, so changing byte offsets corrupted
its tail (`line 578: This: No such file or directory`, `IMAGE: unbound variable`)
— and it still reported **exit 0**, which was worthless. The cluster was left
untouched, so there was no damage.

**Never edit a file that a running process is reading.** And an exit code from a
run that printed errors like those means nothing.

---

## 4. Still needing a human

* **Nothing pushed. 57 commits waiting.**
* The deferred code review of the earlier k3s cutover work — postponed until the
  platform ran, and it has been running.

## Standing rules — each of these has cost a session

* **Never pipe a command whose exit code you need.** `cmd | tail` reports `tail`'s status.
  Redirect to a file and read `$?`. This has produced false green reports here more than once.
* **Never edit a file a running process is reading** — see section 3. Bash reads scripts
  incrementally, so editing a running script corrupts it mid-execution, and the exit code
  afterwards is worthless.
* **No kubeconfig-derived value is ever formatted** — not into a message, a log line, or a DTO
  field. Everything goes through `qa-environments/src/infra/observer/errors.rs`. A measured
  leak once put a pasted **private key** on the platform page.
* **`kubeconfig_credstore_ref` must never reach `PlatformDto`.** Under `SharingMode::Tenant`
  the reference *is* a read path to the material.
* **ADR-0001:** no `kube` / `k8s-openapi` outside `qa-environments/src/infra/observer/`, and
  never in `domain/`. Gated behind the `platform-observation` cargo feature, whose list lives
  in **two** places now that compose is gone: `apps/cf-gears-example-server/Cargo.toml` and
  `deploy/docker/qa-platform.Dockerfile`. The canonical argo list is
  `deploy/cargo-features.argo`, guarded by `deploy/helm/tests/test_features.py`.
* **Do not run the user's test suite against the VHP cluster** without asking.
* Toolchain: `export PATH="$HOME/.cargo/bin:$PATH"`. Never `cargo test --all-targets` at
  workspace level. Clippy is `-D warnings`.
* `npm run lint` is broken repo-wide (no `eslint.config.js`; ESLint 9 dropped `.eslintrc`) and
  `cargo fmt --check` is red on many untouched files. Both pre-existing — not findings.
* The remote needs the **VPN**. If `10.136.20.200` times out, that is the tunnel, not the code.

---

---

## 5. Backlog and history (everything below predates this session)

### Cluster health — done, deployed and verified

**The `sync.sh --argo` run named here no longer exists** — that driver was deleted
with the compose stack (section 2.2). The result stands as a historical record.
Exit 0, 42 PASS, zero FAIL.
Its new check 10 passed on its first real run, and the spec's predicted values matched
field-for-field on `sv-test`: `Healthy`, one node `sv-vhp-jele-io` (control-plane, ready,
`v1.33.4+k3s1`, Ubuntu 24.04.3 LTS), counts `1/1/1/1/0/0`, `namespace_count` 14. The other
seven platforms report `cluster: null` — never checked, which is item 1 below.

* Plan and full task history: `docs/superpowers/plans/2026-08-28-cluster-health.md`
* Design authority: `docs/superpowers/specs/2026-08-28-cluster-health-design.md`
* Predecessor (version + base-domain observation):
  `docs/superpowers/specs/2026-08-28-platform-observation-design.md`
* Execution ledger with every ruling: `.superpowers/sdd/2026-08-28-cluster-health/progress.md`

Repo `/home/serhii/Jelastic/projects/fabric/gears-rust`, branch `feature/qa-platform-specs`.
Pushed to the **personal fork** (`github.com/mo3g89/gears-rust`) with `./push-to-fork.sh` at
the repo root. **Nothing goes to `origin` (constructorfabric).**

Legacy reference: `/home/serhii/Jelastic/projects/fabric/vhp-testrunner` (Rust manager +
React manager-ui + Argo). It is the behavioural benchmark; when the two disagree, legacy is
the specification unless a recorded decision says otherwise.

---

### Surface a kubeconfig that cannot be resolved — READY, small

**The gap.** Seven of the eight registered platforms fail at credstore resolution *before*
`observe()` is ever called: `platform ...'s kubeconfig secret (argo-proof-kubeconfig) was not
found in credstore`. Because the failure happens upstream of the observer, **no observation
column is written at all** — so the UI shows them as "not yet observed" forever while the real
reason repeats in the log every five minutes.

Raised in the cluster-health spec section 8 and deliberately left undecided there. Verified
still true on the live deployment 2026-08-29: all seven report `cluster: null` and a null
`version_detect_error`.

**Shape of the fix.** In `qa-environments/src/domain/service/platforms.rs`, the
`observe_cluster` path resolves the kubeconfig via `fetch_kubeconfig_material` and propagates
a `DomainError` upward, which `run_observation_cycle` logs and swallows. Record that failure
as an `ObservationOutcome::Failed` instead, so `record_observation` writes
`version_detect_error` and `version_detected_at` and an operator can read it on the page.

**Watch for:** the resolution error carries a credstore *reference*, not material — but it is
still an error string reaching a published column, so classify it the way `errors.rs`
classifies everything else rather than interpolating a `DomainError`'s `Display`.
`cluster_status` must stay NULL (nothing looked at a cluster), which is exactly what
`HealthOutcome::NotAttempted` already expresses.

**DECIDED 2026-08-29 by the user:** a resolution failure gets **its own state**, not the red
"detection failed" dot. A missing credstore secret is an operator configuration error, not a
cluster problem, and the page must distinguish "we could not even try" from "we tried and
detection failed". That means a new dot colour/label and the DTO field to carry it, on top of
the `ObservationOutcome::Failed` write described above. `cluster_status` still stays NULL.

---

### Deploy qa-platform into the remote k3s — DONE

Delivered before this session (the chart, `deploy-k8s.sh`, `verify-k8s.sh`) and made
portable in this one (section 2.2). Kept only as a pointer: the design is
`docs/superpowers/specs/2026-08-29-in-cluster-deployment-design.md`.

### Code coverage — NEEDS DESIGN, and legacy's approach will not port

`GET /qa/v1/dashboard/coverage` returns `[]` from a deliberate `Ok(Vec::new())`
(`qa-insights/src/domain/service/dashboard.rs:810-817`). Three upstreams are missing, all
measured rather than assumed:

* Legacy parses `=== COVERAGE_SUMMARY: line branch function ===` out of **finished workflow
  logs**. **THIS BULLET IS NOW OUT OF DATE and it is the one that changes the plan:** a
  finished run's log IS archived, in `qa_run_logs` (section 2.1), so the text to parse exists.
  `log_storage_ref` is still deliberately never written (design D-RLP-6) — read the log through
  `GET /qa/v1/runs/{id}/logs` or the table, not that column. One of this item's three blockers
  is therefore gone.
* The marker comes from legacy's `runner/coverage.sh`, which runs against an *instrumented*
  build (its `coverage-install` / `coverage-collect` plans, profraw to Nexus). Our pytest
  runner has no coverage step at all.
* `qa_runs_sdk::Run` carries **no product key** — VHP-319 deleted legacy's product-version
  model and qa-catalog owns products now.

So this is a three-gear change and copying legacy is not an option. Likely shape: the runner
emits coverage as structured output, qa-runs persists it on the run, qa-insights serves it.
The product-key question must be answered as part of that design.

---

### Smaller items, none blocking

* **Seven stale platform fixtures** on the remote (`argo-proof-platform-*`,
  `smoke-platform-*`, `late-pod-proof`, `probe-platform-2`) now dominate the dashboard's
  Platforms card and log a warning every ticker cycle. Deleting them is destructive and needs
  the user's go-ahead. Offered twice, never decided.
* **`PlatformsStrip` and `PlatformsTable` have no render tests** — flagged by the final
  review. `ClusterHealthCard.test.ts` is the pattern to copy.
* **`RunsPage` still offers `Pending` and `Skipped` filter chips** that this gear's `RunState`
  can never produce, so they always return zero rows. Removing a UI affordance is a product
  decision.
* **`canceled` / `timed_out` / `expired` have no badge styling** and fall through to default.
* **Nothing deletes the old Argo Secret** on kubeconfig rotation or platform delete.
* **`qa-environments` has no leader election**, unlike its sibling gears. Assessed as wasteful
  rather than harmful under multi-replica: every write is idempotent and every cluster read is
  a GET.
* **The compose stack is deleted** (section 2.2), so the old note here about restoring the
  local argo plane no longer applies. Three empty root-owned directories still need
  `sudo rmdir` — the command is in section 0.3.

---

## How this work has been run, and it has been working

Design, then spec, then plan, then subagent-driven execution with a task review after every
task and a whole-branch review at the end. **Match the process to the artefact** — a dev
script does not get a feature's full review chain; that mistake once produced 702 lines of
process for about 15 lines of work, and the human called it out.

Two habits worth carrying:

* **Break-test every guard you add.** Mutate the code so the new test *should* fail, and
  confirm it does. A guard that cannot fail is worse than no guard, and this project has
  shipped one before.
* **The whole-branch review earns its keep.** The last one found a defect no per-task review
  could see: a feature-off build fabricating an `Unreachable` status through the interaction
  of three separately-correct tasks.
