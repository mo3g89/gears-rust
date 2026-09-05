# qa-platform UI Integration and Deployment — Design

**Date:** 2026-08-25
**Status:** approved, ready for an implementation plan
**Scope:** port the legacy `manager-ui` onto the four qa-platform gears, add real OIDC
authentication, and deliver both a docker-compose stack for verification and a Helm chart for
deployment.

**Preceding work:** the four gears (`qa-environments`, `qa-catalog`, `qa-runs`, `qa-insights`) are
code-complete on `feature/qa-platform-specs`. This design consumes them; it does not change their
domain logic.

---

## 1. Goal and governing principle

Two outcomes, in this order:

1. **A stack anyone can bring up and click through**, exercising launch -> dispatch -> results
   ingest -> dashboard -> analytics -> notifications end to end.
2. **The legacy UI serving that stack**, at behavioural parity with what it does today against the
   monolith.

The governing principle is the one that governed the backend port: **preserve legacy behaviour;
adapt only the implementation.** For the UI this has a sharp, testable meaning — `src/components/`
(~18k LOC, 114 files) is copied verbatim and must stay that way. Every backend difference is
absorbed inside `src/api/`.

There is exactly one deliberate departure, stated up front: **authentication is new code, not a
port.** Legacy has no application-level auth at all (optional nginx-ingress basic auth on the UI
ingress; the manager itself is unauthenticated behind a ClusterIP). Real OIDC login was chosen
knowingly, and it is the one subsystem here with no legacy counterpart.

---

## 2. What already exists, measured

Numbers were measured on 2026-08-25 and are load-bearing for the plan's task sizing.

### Legacy UI (`../vhp-testrunner/manager-ui`)

| Fact | Value |
|---|---|
| Source files | 114 `.ts`/`.tsx`, **20,498 lines** |
| Wire surface | **3 files, 2,412 lines**: `src/api/client.ts`, `hooks.ts` (1,419), `types.ts` (897) |
| Distinct API paths called | **~59**, all under one `API_BASE_URL` (`import.meta.env.VITE_API_URL` or `/api`) |
| Stack | React 19, Vite 6, TypeScript 5.7, Tailwind 3, radix/shadcn, TanStack Query 5, react-router 7, apexcharts |
| Log streaming | **WebSocket** — `src/hooks/useWebSocket.ts`, sole consumer `src/components/runs/LogViewer.tsx` |
| Serving | nginx serving `dist`, `location /api/` proxying to `vhp-test-manager...:8080`, resolver `kube-dns` |
| Auth | none in the application |

### The gears

| Fact | Value |
|---|---|
| REST routes across the four gears | **52**, all under `/qa/v1/...` |
| Run logs | **SSE** (`GET /qa/v1/runs/{id}/logs`, `qa-runs/src/api/rest/routes/runs.rs:163`) |
| Server | one binary, `cf-gears-example-server`, `--features qa-platform,...` |
| Dev config | `config/qa-platform.yaml` — SQLite, `auth_disabled: true`, single-tenant |
| OpenAPI | served at `/openapi.json` when `enable_docs: true` |
| Existing container build | `testing/docker/cyberware.Dockerfile` — multi-stage, `ARG CARGO_FEATURES`, builds `cf-gears-server`. **Note:** `testing/docker/docker-compose.yml` references a `cf-gears.Dockerfile` that **does not exist** in the tree; only `cyberware`, `http-mock` and the compose file are there. |
| Frontend precedent | **none** — no `package.json` anywhere in the repo |

The route counts are close (52 vs ~59) because the gears were ported from this exact surface. That
is the reason this project is an adaptation and not a rewrite.

### Legacy deployment

Helm chart `charts/vhp-testrunner`: `manager` Deployment + PVC + RBAC, `manager-ui` Deployment +
ConfigMap(nginx) + Service + Ingress (optional htpasswd basic auth), `postgres` StatefulSet, and
`argo-workflows` as a subchart. Three Dockerfiles. **No docker-compose exists in legacy** — the
compose stack in this design is new, and "prod the same way as legacy" means the Helm chart.

---

## 3. The three obstacles, and what each costs

These were identified before design, and each one shapes a deliverable.

### 3.1 Every background task is switched off, so the product renders empty

`config/qa-platform.yaml` sets `dispatcher_enabled: false`, `scheduler_enabled: false`, and
qa-insights' `enable_tickers` is left at its default with the tickers unstarted. The file's own
comments give the reason: each task runs under its gear's system actor with a **nil tenant**, and
`static-authz-plugin` denies a nil tenant outright (`src/domain/service.rs`, the
`tid == Uuid::default()` branch, whose comment says it denies "rather than grant unrestricted
access"). Its config is `{vendor, priority}` — there is no grant to express. `tr-authz-plugin` is
the same: `{vendor, priority}` and nothing else.

With the dispatcher off, **no run's results are ingested at all** — no per-test rows, no verdict, no
live logs — so the dashboard, coverage and all eight analytics sections render empty. A stack that
cannot show data cannot verify a UI.

`config/qa-platform.yaml` already names the remedy in prose: *"until a policy engine that can grant
the system actor DELETE on qa.bundle replaces the static plugin"*. Section 4 builds it.

### 3.2 There is no real test executor

`qa-runs/src/infra/executor/` contains **only `mock.rs`**, hard-wired at `gear.rs:283`:

```rust
let executor: Arc<dyn RunExecutor> = Arc::new(MockRunExecutor::new());
```

No Argo adapter, no config knob. The mock's header explains why: the real adapter waits on the
`serverless-runtime` gear, *"a docs-only gear with zero `.rs` files"*.

**Consequence, accepted deliberately:** everything except execution is real — launch, admission,
queueing, dispatch, ingest, history, dashboard, analytics, JIRA, notifications. Every run's
*execution* is a deterministic in-memory fake. This is ideal for compose. It means the Helm chart
deploys a **staging/demo** system, and the chart's `NOTES.txt` must say so in those words. When the
runtime gear lands, the change is one executor binding plus one values key.

### 3.3 Neither shipped authz plugin can express a per-user role

Both are `{vendor, priority}`. Authorization is therefore **tenant-scoped**: an authenticated user
may do everything within their tenant and nothing across tenants. This matches legacy, which has no
per-user permissions at all, but it is not what "real authz" might be taken to mean.

**Explicitly deferred, not dropped:** a claims-to-grants policy plugin (mapping OIDC groups/roles to
per-resource, per-action grants, with the UI hiding what the user cannot do) is a separate subsystem
needing its own spec. It is out of scope here.

---

## 4. `system_grants` — a default-off cross-tenant grant

### 4.1 Why read-only is the default, and why it is safe

`qa-runs/src/domain/system_actor.rs` states the danger precisely:

> *"In a deployment where the PDP grants `qa_runs.system` a covering constraint set, a nil-tenant
> **write** context compiles to a platform-wide write scope."*

and names the gap:

> *"Nothing in the type system distinguishes 'this factory is for enumeration' from 'this factory is
> for writes'."*

So the shape of this feature is the difference between a safe stack and a platform-wide write scope.
What makes a safe shape possible is that the gears already separate the two, and the separation is
machine-enforced. qa-runs declares twelve system-actor factories in a symmetric 6 + 6 split, pinned
by `every_factory_in_this_module_is_classified`:

| Platform-scoped — nil tenant, **reads** | Tenant-bound — **acts, writes** |
|---|---|
| `for_dispatch_enumeration` | `for_dispatch` |
| `for_claim_reconciliation` | `for_claim_release` |
| `for_ttl_sweep` | `for_ttl_expiry` |
| `for_timeout_sweep` | `for_timeout_enforcement` |
| `for_watch_scan` | `for_result_ingest` |
| `for_schedule_tick` | `for_schedule_fire` |

The pattern is: enumerate across tenants (read), then act under a tenant-bound context built from
the `tenant_id` decoded off each row. qa-insights follows it exactly (`tenants_for` -> per-tenant
contexts, with `TenantBound::new(nil)` returning `None` so a nil-tenant write context does not
compile). qa-catalog follows it for branch refresh (`for_branch_refresh_enumeration` ->
`for_branch_refresh(tenant_id)`).

**There is exactly one exception, and it is documented as one:** `for_bundle_gc` is platform-scoped
and performs a cross-tenant DELETE on `qa.bundle`, justified in its own doc as *"expiry is a platform
hygiene concern, and the delete is keyed strictly on `expires_at`."*

So: eight of nine platform-scoped factories need **read** authority only. The design makes that the
default and makes the ninth look exceptional in the config file.

### 4.2 Configuration

Added to `static-authz-plugin`. **Absent config must be byte-for-byte today's behaviour** — no
existing stack changes.

```yaml
static-authz-plugin:
  config:
    vendor: constructorfabric
    priority: 100
    system_grants:                        # OPTIONAL. Absent => nil tenant denied, as today.
      - subject_type: qa_runs.system
        resources: [qa.queue_entry, qa.run, qa.schedule]
        actions:   [get, list]
      - subject_type: qa_insights.system
        resources: [qa.test_result]
        actions:   [get, list]
      - subject_type: qa_catalog.system
        resources: [qa.test_repo]            # the (repository, tenant) target listing, nothing more
        actions:   [get, list]
      - subject_type: qa_catalog.system   # the one write grant in the product
        resources: [qa.bundle]
        actions:   [delete]
        allow_write: true                 # REQUIRED for any action outside {get, list}
```

**Validation rules, enforced at config load:**

1. An action outside `{get, list}` without `allow_write: true` is a **config load failure**, naming
   the subject, resource and action. A cross-tenant write cannot be reached by a typo or a copied
   block.
2. `allow_write: true` with only read actions is also a failure — it means the author misunderstood
   the flag.
3. An unknown `subject_type`, resource or action is a failure, not a silent no-match. A grant that
   matches nothing is indistinguishable from a missing grant at runtime, and that is how a stack
   ships with a task silently denied.

### 4.3 Behaviour

Inside the existing nil-tenant branch, before the current deny: if a grant matches the request's
subject type **and** resource **and** action, emit a constraint set spanning tenants; otherwise fall
through to the deny that exists today. The subject type comes from the security context the gear
stamps (`qa_runs.system`, `qa_catalog.system`, `qa_insights.system` — verified present in the three
gears).

### 4.4 Tests

- absent `system_grants` produces a decision identical to today's for a nil-tenant request
- a write action without `allow_write` fails config load with a message naming subject/resource/action
- `allow_write` with only read actions fails config load
- an unknown subject type / resource / action fails config load
- a matched read grant compiles to a scope spanning more than one tenant
- a request whose subject matches but whose resource does not still gets today's deny
- a request whose subject and resource match but whose action does not still gets today's deny

---

## 5. Repository layout

```
gears-rust/
  gears/qa-platform/qa-platform-ui/          # this repo's first frontend
    package.json  vite.config.ts  tsconfig.json  tailwind.config.ts
    nginx.conf                               # retargeted from legacy's
    Dockerfile
    src/
      api/client.ts                          # ADAPTED
      api/hooks.ts                           # ADAPTED
      api/generated/openapi.d.ts             # GENERATED, committed
      api/types.ts                           # thin re-export over generated types
      auth/                                  # NEW: PKCE, token store, guard, login
      hooks/useRunLogStream.ts               # NEW, replaces useWebSocket.ts
      components/ pages/ lib/                # COPIED VERBATIM
  gears/system/authz-resolver/plugins/static-authz-plugin/   # + system_grants
  config/qa-platform-stack.yaml              # NEW: postgres, OIDC, rg-tr, tasks ON
  deploy/
    docker/ui.Dockerfile
    compose/docker-compose.yml
    compose/keycloak/realm-qa-platform.json
    charts/qa-platform/{Chart.yaml,values.yaml,templates/,NOTES.txt}
```

`src/api/` is the seam, and the whole design rests on it: legacy already routed its entire wire
surface through three files behind one base URL, so the backend swap is 2,412 lines of edits and
nothing else. Components keep importing the same hook names with the same shapes.

`src/auth/` is a sibling of `api/`, not threaded through it, so the login subsystem is reviewable and
testable as one unit. `client.ts` touches it through a single `getAccessToken()` call.

---

## 6. UI adaptation

### 6.1 `client.ts`

- `API_BASE_URL` -> `/qa/v1` (still overridable by `VITE_API_URL`).
- One `Authorization: Bearer <token>` injection point, calling `auth.getAccessToken()`.
- `ApiError` keeps its `{status, statusText, message}` shape so component error handling is
  untouched, and learns to unwrap the gears' canonical error envelope into `message`.
- A 401 raises a typed error the auth layer intercepts (section 7).

### 6.2 `types.ts` -> generated

Replaced by types generated from the running gateway's `/openapi.json` via `openapi-typescript`,
committed to the repo, with `types.ts` reduced to a thin re-export so component imports do not
change.

**This is the design's one deliberate departure from "copy verbatim", and the reason is drift:**
hand-maintaining 897 lines of wire types against 52 endpoints is how a UI silently diverges from its
backend. Generation turns divergence into a build failure (section 10).

### 6.3 `hooks.ts`

The ~59 legacy paths remapped onto the 52 gear routes, **keeping every exported hook name and
signature** so no component changes.

#### The path-level gap analysis, done during planning

The path-level diff was **completed while planning this work** rather than deferred, because its
outcome decides Phase B's task list. Measured 2026-08-25: **64 distinct path templates** extracted
from legacy's `hooks.ts`/`client.ts`, against **74 operations over 52 distinct paths** in the gears
(qa-environments 9, qa-catalog 22, qa-runs 15, qa-insights 28). Every legacy path falls into one of
four categories.

**Category 1 — reshaped; adapter work inside `hooks.ts`.**

| Legacy | Gear | Nature of the change |
|---|---|---|
| `/schedules/{id}/suspend`, `/schedules/{id}/resume` | `PUT /qa/v1/schedules/{id}` with required `enabled` | **A deliberate improvement, not a gap.** `qa-runs/src/api/rest/dto.rs:915` records that legacy edits a schedule by deleting and recreating the `CronWorkflow` then re-suspending it by hand, and that requiring `enabled` makes that "unrepresentable here rather than merely unlikely". Two UI buttons become one PUT. |
| `/run-queue/{id}/cancel` | `POST /qa/v1/runs/{id}/cancel` | cancel moved from the queue to the run |
| `/analytics/plan/{id}/{builds,tests,test-history}` | `GET /qa/v1/analytics/plan/{builds,tests,test-history}` | plan identity moved from path segment to query — qa-insights Task 27 |
| `/products/{id}/coverage` | `GET /qa/v1/dashboard/coverage` | coverage is a dashboard concern, not a product sub-resource |
| `/tests/recent-results?file=&limit=` | `GET /qa/v1/test-results` + OData | ad-hoc filters become OData `$filter`/`$top` |
| `/settings/variables`, `/platforms/{id}/variables` | `GET/PUT/DELETE /qa/v1/variables` | one variables surface instead of two |
| `/runs/{id}/jira` | `POST /qa/v1/jira/bugs`, `GET /qa/v1/jira/open-bugs` | filing and listing split, and the GET takes `(repo_id, plan_path)` not `plan_id` |

**Category 2 — the data exists, but as fields rather than an endpoint.** `/products/{id}/observed-branches`
and `/products/{id}/observed-versions` have no gear route; `observed_version` and `observed_build` are
fields on the platform DTO (`qa-environments/src/api/rest/dto.rs:27-28`), so the UI derives them from
`GET /qa/v1/platforms`.

**Category 3 — absent, and nothing calls them.** These legacy paths appear **only in `hooks.ts`**, with
no component consumer anywhere in the 114 files: `/tests/source`, `/platforms/{id}/refresh-version`,
`/platforms/{id}/details`, `/plans/{id}/run-test`, `/plans/{id}/runs`, `/git-plans`, `/runs/{id}/dag`.
They are dead hooks in legacy and are simply dropped. Zero UI impact.

**Category 4 — absent, and user-visible.** See section 6.5.

**What remains for Phase B is the field-level diff, not the path-level one.** Response and request
*shapes* have not been compared field by field, and that is Phase B's first task. Known field-level
differences already: the new `Collect` run kind, and the HMAC-signed collect callback over
`(repo_id, branch, tenant_id)`.

Any mismatch resolved inside `hooks.ts`. A field the gears do not serve is recorded as a gap for a
human, not papered over with a default. **The backend is feature-frozen for this project.**

### 6.5 Removals — three settings surfaces with no backend

Three legacy settings pages have no gear endpoint and are **removed**, because a settings page that
404s is worse than an absent one. Each removal is recorded here so re-adding it is mechanical rather
than archaeological:

| Removed | Needed endpoint (absent) | Files touched |
|---|---|---|
| Repo Poller settings | `/settings/repo-poller` | `pages/settings/SettingsRepoPollerPage.tsx`, `components/layout/Sidebar.tsx` nav entry, `App.tsx` route, its hooks |
| ReportPortal settings | `/settings/reportportal` | `pages/settings/SettingsReportPortalPage.tsx`, Sidebar nav entry, `App.tsx` route, its hooks, its `types.ts` entries |
| Runner Defaults settings | `/settings/runner-defaults` | `pages/settings/SettingsRunnerDefaultsPage.tsx`, Sidebar nav entry, `App.tsx` route, its hooks |
| ReportPortal links in run/test detail | `/settings/reportportal` | `pages/RunDetailPage.tsx` (2 references), `pages/TestDetailPage.tsx` (1 reference) |

The five remaining settings pages map cleanly: JIRA, JIRA poller, SSH keys, Variables, and the
settings layout.

**This is the only place `src/components/` and `src/pages/` are modified at all.** Three deletions and
three reference strips, against ~18k LOC otherwise copied verbatim — which is what keeps the
"components untouched" property meaningful rather than approximate.

### 6.4 WebSocket -> SSE

`useWebSocket.ts` is replaced by `useRunLogStream.ts` built on `EventSource`, with `LogViewer.tsx`
keeping its props and its connection-state indicator. This is a required rewrite, not a proxy
setting: the gear serves SSE and the legacy UI speaks WebSocket.

Two consequences to handle rather than discover:

- `EventSource` cannot send headers, so the bearer token cannot ride one. The stream is authorized by
  a short-lived query parameter or a cookie set at login — **decide in Phase C, and record the
  decision with its reasoning**, because a token in a URL lands in access logs.
- nginx must set `proxy_buffering off` for that location or events arrive in batches. Legacy's
  WebSocket config did not need this.

---

## 7. Authentication (`src/auth/`)

Authorization-code flow with PKCE against Keycloak, via `oidc-client-ts` rather than hand-rolled
crypto.

- **Access token in memory only**; refresh token in the library's session store.
- **Route guard** wrapping the existing router; unauthenticated users reach a login screen that is
  the only new page in the application.
- **401 handling:** one silent refresh attempt, then redirect to login. Exactly one refresh, so a
  revoked session cannot loop.
- **Logout** clears local state and calls the end-session endpoint.
- **Tenant** comes from token claims via `rg-tr-plugin`. The UI never sends a tenant header — if it
  could, a client could choose its own tenant.

Backend side is configuration, not construction: `oidc-authn-plugin` (11,874 LOC, released, with JWT
validation, JWKS caching, discovery and S2S exchange) replaces `static-authn-plugin`, and
`rg-tr-plugin` + `resource-group` replace `single-tenant-tr-plugin`. **No config in this repo uses
the OIDC plugin today** — this will be the first, so its configuration is a genuine unknown and
Phase C's first task is standing it up against Keycloak before any UI code is written.

---

## 8. Deployment

### 8.1 Compose (`deploy/compose/docker-compose.yml`)

| Service | Purpose |
|---|---|
| `postgres` | the gears' database; replaces SQLite so compose matches the chart |
| `keycloak` | realm, client (public + PKCE) and two users imported from a committed realm JSON |
| `cf-gears` | all four gears + api-gateway, `config/qa-platform-stack.yaml`, tasks ON |
| `qa-platform-ui` | nginx serving `dist`, proxying `/qa/v1` and the SSE stream |

Keycloak is a compose service specifically so the **real** OIDC path — discovery, JWKS, PKCE,
refresh, logout — is exercised on every bring-up. The chart takes issuer and client id from values,
so production points at the corporate IdP with no code difference. The alternative considered and
rejected was static-authn locally with OIDC only in production, on the grounds that the auth path you
ship would never be the auth path you verified.

The gears image is modelled on `testing/docker/cyberware.Dockerfile` — the same multi-stage shape and
`ARG CARGO_FEATURES` — with the binary changed to `cf-gears-example-server` and features
`qa-platform,oidc-authn,static-authz,rg-tr,static-credstore`. It is a **new** file: the
`cf-gears.Dockerfile` that `testing/docker/docker-compose.yml` names is absent from the tree, so
there is nothing to reuse directly.

The UI image is legacy's Dockerfile (node build -> nginx) with `nginx.conf` retargeted from the
kube-dns resolver to the compose service name, plus `proxy_buffering off` on the log-stream
location.

### 8.2 Helm chart (`deploy/charts/qa-platform/`)

Mirrors `charts/vhp-testrunner` template-for-template so the two are diffable by anyone who knows the
legacy deployment:

| Legacy | Here |
|---|---|
| `manager-deployment.yaml` + PVC + RBAC + SA | `gears-deployment.yaml` (one binary, all four gears) |
| `manager-ui-{deployment,configmap,service,ingress}.yaml` | same four, `ui-*` |
| `manager-ui-ingress-auth-secret.yaml` | kept — optional basic auth is defence in depth alongside OIDC |
| `postgres-{statefulset,service,secret}.yaml` | same |
| `argo-workflows` subchart | **omitted** — no executor to drive (section 3.2) |

`values.yaml` carries `oidc.{issuer,clientId}`, image tags, replica counts and ingress host, in
legacy's value-naming style. **`NOTES.txt` must state that runs are simulated** and that real
execution awaits the `serverless-runtime` gear.

---

## 9. Phasing

Each phase ends at a gate that is a command someone runs, not a judgement.

| Phase | Delivers | Gate |
|---|---|---|
| **A** | `system_grants` + tests; `qa-platform-stack.yaml` (postgres, tasks ON, `auth_disabled: true` still); gears Dockerfile; compose with postgres + gears, no UI and no Keycloak yet | `curl` a run from launch -> dispatch -> results -> dashboard -> analytics with every response non-empty |
| **B** | UI copied in; path/field mismatch table; `src/api/` adapted; generated types; SSE log stream; UI added to compose | every page renders real data, still with auth off — no login, no token |
| **C** | Keycloak added to compose with its realm import; OIDC plugin + `rg-tr-plugin` configured; `src/auth/`; `auth_disabled: false`; the SSE-authorization decision | log in via Keycloak, session survives reload, logout clears, a 401 refreshes once then redirects |
| **D** | Helm chart, UI nginx ConfigMap, ingress, `NOTES.txt` | `helm template` clean; deploys to kind; Phase A's flow passes through the ingress |
| **E** | contract check, Makefile targets, CI Node job, docs | contract check **fails** on a deliberately renamed route |

**Authentication arrives last on purpose, in Phase C.** Phases A and B run with
`auth_disabled: true`, exactly as `config/qa-platform.yaml` does today, so that a failure in those
phases is a data or contract failure and never an auth failure. Keycloak is not in the compose file
until Phase C. The cost is that Phase B's UI is briefly unauthenticated in local compose only; the
benefit is that the two hardest debugging surfaces are never live at the same time.

**Phase A carries no UI on purpose.** It proves the backend can produce the data every later phase
displays, and it is where `system_grants` either works or does not — the riskiest unknown, so it goes
first. Phase E's gate is a deliberate break, because a verification step that has never failed has
not been shown to work.

---

## 10. Verification strategy

Three layers, in increasing durability:

1. **Per phase:** the gates above.
2. **The contract check (Phase E, the one that matters):** CI regenerates types from the running
   gateway's `/openapi.json` and fails if they differ from the committed ones. This is what makes
   "verify all changes" hold over time instead of at one moment — a renamed route or a changed field
   breaks the build rather than a page at runtime.
3. **UI unit tests** for the three units with real logic: the mismatch adapters in `hooks.ts`, the SSE
   stream hook, and the auth token/refresh state machine. `src/components/` is copied verbatim and
   gets no new tests; testing untouched ported code would be testing legacy, not this work.

**Node tooling is not quite new, and the contract check has machinery to reuse.** The Makefile already
depends on `npx` for the `slides` target, so Node is an assumed tool here — but there is no
`package.json` anywhere, so a *built* frontend still is a first. Three things already exist and should
be reused rather than reinvented:

- `make openapi` — builds the example server, starts it via the `start_server_and_wait` macro, and
  curls `$(OPENAPI_URL)` to `$(OPENAPI_OUT)`. The contract check is this target pointed at
  `config/qa-platform-stack.yaml`.
- `tools/scripts/sort_openapi_json.py` — canonical ordering, which is what makes a spec diff
  meaningful rather than noisy.
- the `E2E_ARGS` / feature-flag conventions for choosing which gears are compiled in.

New targets: `ui-install`, `ui-lint`, `ui-build`, `ui-contract`, plus a CI Node job.

---

## 11. What is explicitly out of scope

- **A real test executor.** Runs are simulated (section 3.2).
- **Per-user roles / claims-based authz.** Needs its own spec (section 3.3).
- **Any change to the four gears' domain logic.** They are feature-frozen for this project. A
  contract gap found in Phase B is reported, not fixed by widening a gear.
- **The qa-insights release-gate items** (Slack webhook credential path, the skip list's missing
  producer, `notify_run_completed`'s missing producer, the ticker/poller race). Those are open
  decisions from the backend work and are not resolved here — though note that **Phase A turns the
  tickers on**, which is precisely the deployment grant that makes the JIRA poller's duplicate-launch
  race live under `NoopLeaderElector`. That must be decided before Phase A ships to any multi-replica
  environment.
- **Deleting or modifying the legacy `manager-ui`.** It stays where it is; this is a copy. The three
  removals in section 6.5 are made in the *copy*, never in `../vhp-testrunner`.

---

## 12. Risks

| Risk | Why it matters | Mitigation |
|---|---|---|
| Phase B's field-level mismatch table is larger than expected | The path-level diff is done (section 6.3) and came out clean — 7 reshapes, 7 dead hooks, 3 removals — but request/response *shapes* are still uncompared, and `hooks.ts` could grow past a clean adaptation | The field diff is Phase B's *first* task and its output is a table reviewed before code; a gap the gears cannot serve is escalated, not defaulted. The path-level result lowers this risk but does not retire it |
| First OIDC configuration in this repo | No in-repo example; the plugin is released but unexercised here | Phase C stands the plugin up against Keycloak **before** any UI auth code |
| SSE authorization has no clean answer | `EventSource` cannot send headers; a token in a URL reaches access logs | Decided explicitly in Phase C with the reasoning recorded, not chosen by default |
| `system_grants` is a security-relevant change to a shipped system gear | A mistake here is a cross-tenant scope | Default-off; read-only unless `allow_write` names the resource; seven tests; the one write grant is visibly exceptional in config |
| A built frontend in a Rust repo | No `package.json` precedent; CI and Makefile conventions are Rust-shaped (though `npx` is already used by the `slides` target) | Confined to one directory with its own targets; the contract check reuses the existing `make openapi` + `sort_openapi_json.py` machinery rather than inventing a second path |
| The chart deploys a system that cannot run tests | Someone could mistake staging for production | `NOTES.txt` says so; `values.yaml` has no executor option to imply otherwise |
