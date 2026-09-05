# Platform observation, base-URL propagation, and skip semantics — design

Date: 2026-08-28
Status: approved (human partner, session 8), ready for an implementation plan
Benchmark: `vhp-testrunner` `origin/master` @ `30ba077` — **the only source of truth**

## 1. Context

`https://<host>/platforms/<name>` shows nothing about the platform it names.
This is not a defect: it is unimplemented. `PlatformDto`
(`qa-environments/src/api/rest/dto.rs:36-50`) carries `observed_version` and
`observed_build`, the `qa_platforms` table carries the matching columns, and
**nothing anywhere writes them** — `grep -rl "kube::\|k8s_openapi"
gears/qa-platform/qa-environments/` returns nothing, so the gear has no way to
observe a cluster at all. `PlatformDetailPage.tsx:131` renders an honest
`UnavailableNotice` ("Cluster health is not available in this deployment").
The namespace is worse than a constant: `:225` renders
`platform.namespace || 'vhp-platform (default)'`, and `PlatformDto` has **no**
`namespace` field, so the left operand is permanently `undefined` and the page
always displays a default it presents as fact. "VHP Base URL" has no DTO field
behind it either.

Legacy implements exactly this and the work is to port it:

* `detect_platform_version` (`manager/src/services/platforms.rs:1031-1090`)
  reads the `core-install-metadata` ConfigMap and parses `data["platformVersion"]`.
* `detect_base_domain` (`:1098-1130`) reads `vp-gateway-hostnames` and derives
  the domain suffix shared by every externally-visible hostname.
* `append_vhp_base_url_env` (`manager/src/services/argo.rs:282-298`) injects
  `E2E_VHP_BASE_URL` and `VPADM_BASE_DOMAIN` into every run's environment.

The last of those is load-bearing rather than cosmetic. The session-7 hand-off
justified it by citing `tests/e2e/README.md:223` in the suite repository; **that
file is not on this disk** (the suite lives in a separate repository, present
here only as the `git-fixture` mirror), so that citation is recorded as
unverified and is not relied on. The verifiable basis is stronger anyway:
legacy's own runner consumes the variable at `runner/entrypoint.sh:485-508` and,
when it is absent, *guesses* — `http://<service-host>:8087`, or a host derived
from the kubeconfig's `server:`, or empty "for test-libs HTTPRoute
auto-discovery". None of those is the platform's real base URL. **Our runner has
no such fallback at all**: `grep -rn "E2E_VHP_BASE_URL\|VPADM_BASE_DOMAIN"` over
`deploy/runner/entrypoint.sh` returns nothing, so today the variable is simply
unset in every run we launch.

### What already exists on our side, and what is genuinely missing

Measured before designing, because it changes the size of the work:

* **`E2E_VHP_BASE_URL` is already built.** `qa-runs/src/domain/env_assembly.rs:236-294`
  implements the variable *and* legacy's "fifth position" override ordering
  (it beats a platform variable, loses to a run parameter — decision D3), with
  tests at `:445-479`. The only reason it never reaches a run is
  `dispatch_spec.rs:559` passing `platform_base_url: None`, whose own comment
  states the cause: "`qa_environments_sdk::TargetPlatform` carries no base URL."
* **`VPADM_BASE_DOMAIN` does not exist here.** It is new on master (`30ba077`).
* **The ticker pattern exists twice** — `qa-runs/src/gear.rs:504` and
  `qa-insights/src/gear.rs:925` both spawn supervised tickers through the same
  `Tickers`/`supervise` helpers — so legacy's `platform_version_poller` has a
  shape to port into rather than a mechanism to invent.

So the work is: give `qa-environments` eyes, store what it sees, and connect
the stored value to machinery that is already written and already tested.

## 2. Scope

**In:**

1. Kubernetes observation in `qa-environments`, behind a cargo feature.
2. Version, build, namespace and base-domain detection, ported from master.
3. Persistence, including a detection *error* that is shown rather than swallowed.
4. A supervised refresh ticker plus an on-demand refresh endpoint.
5. Decision **D4**: materialise a platform's kubeconfig into the Kubernetes
   `Secret` the runner pod mounts, ending the manual operator step.
6. `E2E_VHP_BASE_URL`, `VPADM_BASE_DOMAIN` and `E2E_K8S_NAMESPACE` reaching runs.
7. Skip semantics: a skipped test no longer fails an entire run.

**Out, by explicit human decision this session:**

* **Anything from `origin/VHP-1655-YZ`.** Ruled out by name. It is also
  divergent rather than newer: `git merge-base --is-ancestor origin/master
  origin/VHP-1655-YZ` exits **1** — it forked at `04ca966`, before both
  `1c622f9` and `30ba077`, while rewriting the same files those commits touch.
  Its `kind`/external-platform concept is therefore **not** part of this design.
* **Slack bot-token parity** (`1c622f9`). Our Slack transport lives in a
  different gear (`qa-insights/src/infra/notify/slack_oagw.rs`, incoming-webhook
  based) and nothing here depends on it. Deferred with its own brainstorm.

## 3. Decisions ratified in this session

* **D-OBS-1 — detection runs on a ticker, plus an on-demand endpoint.**
  Legacy's shape. Rejected: detection at `GET /platforms/{id}`, which puts an
  unreliable cross-cluster call in the page-load path and would fan out to N
  clusters on the list page; and a refresh endpoint alone, which leaves a run
  dispatched by anyone who did not click with no base URL — the one moment the
  value must exist.
* **D-OBS-2 — one cargo feature, `platform-observation`, and an ADR amendment.**
  The ledger records "the local stack stays kube-free" as a *verified* property
  (`cargo tree -i kube` finds nothing in the committed image feature set), and
  **ADR-0001's Confirmation criterion forbids `kube`/`k8s-openapi` in *any*
  qa-platform crate**. The waiver of 2026-08-27 is deliberately narrow — it
  permits the dependency "inside qa-runs, behind a non-default cargo feature"
  and states "nothing in `domain/` learns that Kubernetes exists" — so
  `qa-environments` is outside it. The human partner extended the waiver on
  2026-08-28 on the same terms; the amendment is Task 1 and records the breach
  plainly, as the first waiver does.
* **D-OBS-2b — `domain/` stays kube-free, behind a port.** Honouring the
  waiver's second clause: a `PlatformObserver` port in `domain/ports/` carries
  no Kubernetes types, the parsing rules are pure functions over plain data
  (`BTreeMap<String, String>` and a namespace, not a `ConfigMap`), and only the
  `infra/` adapter constructs a client. This mirrors how the frozen
  `RunExecutor` port kept Argo out of `qa-runs`' domain, and it has the side
  benefit that every parsing rule is testable in a **default, kube-free build**.
* **D-OBS-3 — master's merge rules are ported verbatim.** See §4.3. Master's
  own comment warns that a manually-set `vhp_base_url` "won't stick past the
  next detection cycle". **That trap is unreachable for us** because we expose
  no manual base-URL field, so fidelity costs nothing and no divergence is
  needed. If a manual override is ever added, this decision must be revisited
  in the same commit.
* **D-OBS-4 — D4 closes here.** `provision-platform-kubeconfig-secret.sh`'s own
  header asked for it: the automatic reconciliation "belongs in qa-environments
  and would give that gear a Kubernetes dependency of its own", which §4.1 adds.
  Its second objection — "IT DOES NOT READ CREDSTORE, and it cannot: the only
  backend is static-credstore-plugin" — went stale when session 7 shipped the
  Postgres-backed plugin (`230f41f5c`, `164c2125d`).
* **D-OBS-5 — a skipped test no longer fails a run, diverging from the
  benchmark deliberately.** Legacy is explicit and considered
  (`argo.rs:2197`: "a skipped test means the run didn't fully execute, so it
  must never read as passing either"), and our `state_machine.rs:395` is a
  faithful port of it. The human partner's suite is full of gated skips, so the
  rule marks nearly every real run red and destroys the signal it exists to
  give. **Consequence designed for, not accepted silently:** trading a false red
  for a false green is no improvement, so the skip count must become visible
  enough that "succeeded, 68 skipped" is legible without opening the run.

## 4. Architecture

### 4.1 The Kubernetes dependency, and its gate

`qa-environments` gains `kube` and `k8s-openapi` as **optional** dependencies
behind a `platform-observation` feature, mirroring `qa-runs`' `argo` feature
(`qa-runs/Cargo.toml:89`), whose Cargo.toml already documents this exact
"no `kube`/`k8s-openapi` in the product" criterion.

**Ruling 23 applies and is the trap to avoid.** The effective feature list lives
in more than one place: the Dockerfile `ARG CARGO_FEATURES` default **and**
`docker-compose.argo.yml`'s own hard-coded copy, which is the copy that governs
every `--argo` deploy. Flipping one and not the other produces a remote build
silently missing the feature. Both are edited, and `sync.sh` gains a check that
the feature is *live* — in the presence-and-absence style of check 8 — rather
than merely requested.

With the feature off, the gear compiles exactly as today: no ticker, no
detection, no Secret materialisation, columns stay null.

### 4.2 Detection

Ported from `platforms.rs:1031-1130`, structured so the decisions are pure
functions and only the I/O touches a cluster.

**Version and build.**

1. Resolve the namespace: the platform's `VPADM_NAMESPACE` variable when set
   and non-empty, else `virtuozzo` (`pick_vpadm_namespace`, `:1312`).
2. `get_opt("core-install-metadata")` in that namespace. Present and parseable
   wins.
3. Otherwise scan all namespaces with label
   `app.kubernetes.io/managed-by=vpadm` and field
   `metadata.name=core-install-metadata`; more than one logs a warning and takes
   the first.
4. `parse_platform_version` (`:1349`) splits the last dotted segment into a
   build **only** when the head still contains a `.` and the tail is all ASCII
   digits — so `26.5.0` → `("26.5", Some("0"))` but `1.0` stays `("1.0", None)`.

**Base domain.** `vp-gateway-hostnames` in the namespace where the metadata was
found. Each entry is JSON; the `internal-gateway-clusterip` key is skipped by
name (its value is a bare IP and would fail to parse), and only
`visibility == "external"` entries contribute. `compute_base_domain` (`:1268`)
then takes the longest common label suffix across the **distinct** hosts,
returning `None` unless at least two hosts agree on at least two labels.

The two-host requirement is not defensiveness: vpadm never exposes a component
at the bare base domain, so every entry carries a subdomain and the domain can
only be recovered by comparison. `None` means **inconclusive**, never an error,
and callers must not clobber a stored value with it.

**Verified against the real cluster before any code was written** (§7).

### 4.3 Persistence and the three merge rules

New columns on `qa_platforms`, one migration:
`vhp_base_url`, `observed_namespace`, `version_detect_error`,
`version_detected_at`. `observed_version` and `observed_build` already exist and
finally acquire a writer.

Legacy's UPSERT (`platforms.rs:1189-1205`) applies three *different* rules in one
statement, all deliberate, all ported:

| column | rule | why |
|---|---|---|
| `observed_version`, `observed_build` | overwrite on success | the cluster is authoritative |
| `observed_namespace` | keep a set value, else take detected | never overwrite an operator's namespace |
| `vhp_base_url` | successful detection wins; inconclusive preserves | self-heals if the domain changes, but an inconclusive read must not erase a known-good value |

On **failure**, `version_detect_error` and `version_detected_at` are written and
version/build are left intact — stale-but-known beats blank — and the error is
rendered in the UI rather than swallowed.

### 4.4 Building a client, and an invariant that is narrowed on purpose

`PlatformsService` already holds `Arc<dyn CredStoreClientV1>`
(`domain/service/platforms.rs:101`), so the material is reachable:
credstore → `Kubeconfig::from_yaml` → `Config::from_custom_kubeconfig` →
`Client::try_from`.

**This narrows a stated invariant and must be recorded as such.** That module's
header says the kubeconfig "goes to credstore **first**, only the generated
reference reaches `qa_platforms.kubeconfig_credstore_ref`, and nothing on any
read path ever returns it." After this change the material is still never
*returned* — no DTO, no API, no log — but it is now *read in-process*. The
precise invariant becomes: **the material never leaves the gear.** The three
mechanisms that enforce it (redacted `Debug`, `#[instrument]` skips, references
at the repository boundary) are unaffected and are extended to the new code
path. The header comment is updated in the same commit, because a doc comment
that no longer describes the code is a defect by this project's standing rule.

### 4.5 The ticker and the refresh endpoint

A `platform_observation` ticker spawned through the existing `Tickers`/
`supervise` helpers, configurable interval with legacy's 60s floor, enabled by
config, skipped entirely when the feature is off. Per cycle: list platforms,
detect, persist the outcome — a failure for one platform is logged and never
aborts the cycle for the others.

`POST /qa/v1/platforms/{id}/refresh` runs one platform's detection on demand
and returns the updated `PlatformDto`, backing a Refresh button on the detail
page. Reserves 5xx for genuine faults: a *detection* failure is a 200 with
`version_detect_error` populated, matching legacy (`routes/platforms.rs:363`).

### 4.6 D4 — materialising the Secret

On create, on update, and as a self-heal each ticker cycle, `qa-environments`
upserts the `Secret` the runner mounts: namespace `argo`, name
`{prefix}{sanitised-ref}` with prefix `qa-platform-`, key `value` — matching
both the script and `qa-runs`' `secret_name` (`argo/naming.rs:112-116`), whose
tests already act as a parity oracle against the script (`naming.rs:196-210`).
That oracle is extended to cover this writer, so three implementations of one
name cannot drift.

**The second set of credentials, resolved.** This Secret goes into the **Argo**
cluster, not the platform's. `qa-environments` takes the same config-driven
shape `qa-runs`' executor already uses (`argo/mod.rs:219-230`): a
`kubeconfig_path` that, when empty, falls back to `Config::infer()`. Under
docker-compose it names a mounted kubeconfig; running inside the cluster it is
left empty and resolves to the pod's ServiceAccount. **This is deliberately
deployment-shape-agnostic**, because the human partner has asked for
qa-platform to be installed into the remote k3s the way legacy is (a separate
piece of work, brainstormed on its own after this one) — and one code path that
is already correct in-cluster means that move reworks nothing here.

This removes the failure the ledger calls the worst-behaved thing in the system:
a UI-created platform sitting `Pending` ~5 minutes and then failing on
`FailedMount` with no explanation.

### 4.7 Reaching a run

* `dispatch_spec.rs:559` reads the platform's stored `vhp_base_url` instead of
  passing `None`, lighting up machinery that already exists and is already
  tested.
* `VPADM_BASE_DOMAIN` is added beside it in the same tier, with the same dedupe
  and the same override ordering, derived from the URL's host exactly as
  `base_domain_from_url` (`argo.rs:2476`) does.
* `E2E_K8S_NAMESPACE` is populated from `observed_namespace`. This is the
  column's stated purpose in legacy's own UPSERT comment — auto-filled "so runs
  get a correct `E2E_K8S_NAMESPACE`, used by tests' Keycloak/credstore
  auto-discovery" — and `dispatch_spec.rs:484` lists it among the things this
  gear "cannot yet" set for want of a source. Detection is that source.

### 4.8 Skip semantics

`state_machine.rs:395`'s `counts.failed > 0 || counts.skipped > 0` becomes
`counts.failed > 0`. Per D-OBS-5 the skip count is surfaced in the run list and
run detail so a green run with skips cannot be mistaken for a clean one. The
existing tests that pin the old behaviour are updated with the reason recorded,
not deleted.

## 5. Error handling

* A cluster that cannot be reached is a **stored, displayed error**, never an
  exception that hides the platform.
* An inconclusive base domain is `None` and preserves the stored value.
* One platform's detection failure never aborts a ticker cycle.
* Credstore failures on the observation path are logged without the material and
  surface as a detection error.
* A run dispatched while `vhp_base_url` is still null behaves exactly as today —
  the variable is absent, which `env_assembly` already treats as legitimate
  (`a_blank_platform_base_url_is_treated_as_absent`, `:471`).

## 6. Testing

* **Pure functions** — `parse_platform_version`, `compute_base_domain`,
  `detected_from_configmap`, the merge rules — unit-tested against fixtures
  captured verbatim from the real cluster in §7, including the
  `internal-gateway-clusterip` bare-IP entry and the `internal-only` entry that
  must be excluded.
* **Merge rules** tested per rule, each asserting the *difference* from its
  neighbours, since all three live in one statement.
* **Secret naming** joined to the existing parity oracle so script, executor and
  the new writer are one behaviour.
* **Feature-off build** asserted: `cargo tree -i kube` finds nothing in
  `qa-environments` without `platform-observation`.
* No test may pass by being skipped. Per the standing rule, any test gated on an
  environment variable prints its skip loudly and CI sets the variable.

## 7. Verification plan

Against the platform kubeconfig supplied by the human partner
(`deploy/remote/vhp-kubeconfig.yaml`, server `185.231.240.170:6443`), which was
probed read-only **before** this design was written. Both of legacy's lookup
paths already succeed against it, so these are predictions, not hopes:

| observable | expected | basis |
|---|---|---|
| `observed_version` | `26.5` | `platformVersion: "26.5.0"` through `parse_platform_version` |
| `observed_build` | `0` | same |
| `observed_namespace` | `virtuozzo` | ConfigMap's own namespace |
| `vhp_base_url` | `https://sv.jele.io` | common suffix of `adapter-s3`/`app`/`auth`/`monitoring`/`api`.`sv.jele.io` |
| `VPADM_BASE_DOMAIN` in a run's env | `sv.jele.io` | `base_domain_from_url` |

`sv.jele.io` was independently stated by the human partner before the cluster
was read; deriving the same value from the ConfigMap confirms the algorithm
rather than the expectation.

D4 is verified by creating a platform through the UI and observing the pod reach
`Running` **without** anyone running the provisioning script.

**Every claim reports the method beside it, per item, at the moment it is
written.** This project's documented failure mode is a query written from an
assumed schema whose empty answer is read as a finding: `$filter` with a bare
uuid rather than `?run_id=`, `qa_ssh_keys` rather than `ssh_keys`, counts nested
under `result`. A `0` or a `null` is evidence only once the query is known
right. Exit codes are captured directly and never through a pipe.

**Not authorised yet:** launching the human partner's suite against
`185.231.240.170`. Registering the platform and reading from it is approved;
executing tests against a cluster that matters requires a separate go-ahead and
will be asked for when the environment variables actually land.

## 8. Risks and open items

* **Kubernetes deployment of qa-platform is a queued follow-on**, requested
  2026-08-28 and explicitly *not* folded into this plan: the two fail
  independently, and a half-built deployment must not block a finished feature
  from review. §4.6's config shape is chosen so it lands unchanged.
* **The dev k3s box still does not host VHP**, so the observation path is proven
  against the real cluster while runs still execute on k3s. These are two
  different clusters and no claim may conflate them.
* **Run logs still do not survive a gears restart** — out of scope here, still
  open, still needs the `log_storage_ref` path that a comment claimed existed.
* **Secrets remain plaintext at rest** by ratified decision; this design adds a
  second reader of a kubeconfig, not a new storage property.
* **`deploy/remote/vhp-kubeconfig.yaml` is untracked and not gitignored**, so
  one `git add -A` would commit a client private key. A `.gitignore` entry lands
  with this work.
