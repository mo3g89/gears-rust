# Field-level contract diff — legacy `manager-ui` against the qa-platform gears

**Measured 2026-08-26** against the running compose stack, not against the spec. This document is the
input Tasks 9, 10 and 11 implement against.

Spec [`superpowers/specs/2026-08-25-qa-platform-ui-integration-design.md`](superpowers/specs/2026-08-25-qa-platform-ui-integration-design.md)
§6.3 already holds the **path-level** diff — seven reshapes, seven dead-hook paths, two DTO-field cases,
three removals. That work is **not redone here**; rows cite it. What follows is the **field level**:
request bodies, response shapes, enum spellings, date formats, pagination envelopes, nullability — plus
five corrections where §6.3's path-level reading does not survive a field-level check (§5).

**The backend is feature-frozen.** Nothing here proposes widening a gear. A field the UI renders that the
gears do not serve is recorded in §8 for a human, never papered over with a default.

---

## 1. Method, and what "verified" means in this document

Both sides were put in a comparable form first.

```bash
# Gear side — the stack was already up (docker compose up -d skipped as a no-op;
# `docker compose ps` showed gears/postgres/git-fixture running).
curl -fsS localhost:8087/openapi.json > /tmp/qa-openapi.json
python3 tools/scripts/sort_openapi_json.py /tmp/qa-openapi.json   # 17,400 lines canonicalised

# Legacy side — the in-repo copy, byte-identical to ../vhp-testrunner/manager-ui/
gears/qa-platform/qa-platform-ui/src/api/{hooks.ts,types.ts,client.ts}   # 1419 + 897 + 96 lines

# Hook enumeration — the table below has exactly one row per line this prints (97).
grep -nE "^export (function|const) use[A-Za-z]+" src/api/hooks.ts
```

Every row's gear side was read out of the canonicalised `/openapi.json` (paths, parameters, `required`,
nullability, `format`, and the published Rust doc comments that OpenAPI carries as `description`). Where
the OpenAPI document was ambiguous or silent, the claim was settled by **calling the running gear** or by
opening the Rust source; those rows say so inline. A claim that was **not** confirmed
against either is listed in §10 — there are four, and none of them is in the table.

Legacy paths below omit the `/api` prefix `client.ts:3` adds (`API_BASE_URL = import.meta.env.VITE_API_URL || '/api'`).

---

## 2. The four field-level facts Task 5 established by driving the endpoints

These are carried here because each is a case where the plan's own endpoint list disagrees with
`/openapi.json`. All four were re-checked against the canonicalised document for this task.

| # | Fact | Re-verification |
|---|---|---|
| 1 | **`product_id` is required on repo creation**, and the plan's Task 5 step list omits the product step entirely — a product must exist first. A whole entity the plan never mentions. | `CreateTestRepoReq.required` = `[default_branch, name, product_id, url]`; `product_id` is `string(uuid)`, not nullable. |
| 2 | **`GET /qa/v1/plans` takes required `repo_id` + `branch` query parameters**, not a filter. Legacy's `usePlans(branch, productId)` will not map onto it unchanged. | Both marked `required: true`. Live: `GET /qa/v1/plans` → `400 Failed to deserialize query string: missing field 'repo_id'`. |
| 3 | ~~**`platform_id` must be non-null on launch**~~ **CORRECTED 2026-08-31: this row was wrong and the UI guard built on it has been removed.** `Unqueued` means *"dispatch inline with no queue row at all"* (`launch.rs`'s `Admission::Unqueued` doc and its `dispatch_and_report` arm), not "never runs"; `schedules.rs` says a platformless schedule can *"succeed every time"*, and `PRD.md:366` calls platformless runs a supported case. Measured live: `POST /qa/v1/runs` with `platform_id: null` went to `running` and ran to completion. A platformless run does lack a target environment, which is a topology matter, not a launch validation. | `LaunchRunReq.platform_id` is `string\|null(uuid)` and **not** in `required`. `qa-runs/qa-runs/src/domain/service/admission.rs:624-633`: *"A run with no platform is never queued and never occupancy [...] — no row, no lease, no lock"*, returning `Admission::Unqueued`. `ScheduleDto.platform_id`'s own doc says the same for schedules. |
| 4 | **`analytics/overview` requires `product_id`, `version` and `scope`.** | All three `required: true`; `plan_id`, `branch`, `days_heatmap`, `days_trend`, `group_by`, `group_value` optional. Same nine on `/qa/v1/analytics/export`; `/qa/v1/analytics/build-tests` adds a required `build`. |

And the contract gap that is a §8 item rather than an adapter problem: **`analytics/overview`'s
`passed`/`failed` are permanently 0 in every deployment this plan produces**, because `app_version` is
never set and version observation is a separate feature nobody is building here. See §9 for the
show / hide / label-unavailable decision and its reasoning, and
`gears/qa-platform/deploy/compose/smoke.sh:388-417` for the most precise statement of the gap that exists
(including which fields its substitute assertion does **not** cover).

**Two gates check this deployment, not one.** `smoke.sh` (above) drives the gears' API and is Phase A's.
`gears/qa-platform/deploy/compose/ui-gate.js` is Phase B's: it opens every route `src/App.tsx` declares in a
real headless browser and records, per route, console errors, failed requests, which `/qa/v1` paths the page
actually requested, and the text it painted. It is the only check here that can see a component crash, an
empty panel, or a page that renders "Failed to load …" off a 200 — every SPA route answers the same
`index.html` to `curl`. Several rows in this document are claims about what a page *renders*, and that script
is how they are testable rather than argued; §9's consequences in particular are visible in its
`QA-REQUESTS` lines. It needs a browser driver installed outside this repo on purpose — its own header says
why and how — and it **writes** to the deployment it measures (one custom plan, one run per invocation).

---

## 3. Cross-cutting rules

Referenced by tag from the table so each row states only what is specific to it.

**X1 — every gear path segment is a UUID; legacy addresses things by name.** Legacy routes runs by
`name` (`App.tsx:59` `/runs/:name`), platforms by `name` (`:69` `/platforms/:name`) and schedules by
`name`. Verified live: `GET /qa/v1/runs/smoke-2` → `400 Cannot parse 'id' with value 'smoke-2': UUID
parsing failed`. Resolution is available without a new endpoint — `name` is a filterable field on
`GET /qa/v1/runs` (`qa-runs/qa-runs/src/infra/storage/odata.rs:86`), and `EnvironmentDto`/`ScheduleDto`
both carry `id` alongside `name` — so this is a lookup in `hooks.ts`, not a component change.

**X2 — pagination envelope.** Legacy `PaginatedResponse<T>` is `{ runs: T[], pagination: { page,
per_page, total, total_pages } }` (`types.ts:31-39`). The gears answer `Page<T>` = `{ items: T[],
page_info: { limit, next_cursor, prev_cursor } }`. **There is no total and no page number**, so legacy's
page-N/of-M pager cannot be reproduced; only next/previous. Page size and cursor travel as `limit` and
`cursor`, which are **undeclared in `/openapi.json`** for `/qa/v1/runs`, `/qa/v1/test-results` and
`/qa/v1/test-case-results` (declared only on `/qa/v1/queue`). Verified live: `?limit=1` returns one item
and a `next_cursor`; `?cursor=<that>` returns the next. **`$top` is silently ignored** — `?$top=1` and
`?$top=0` both returned the full default page. Insights collections default to 200 and clamp at 500
(`qa-insights/qa-insights/src/api/rest/routes/collections.rs:60-61`).

**X3 — dates and durations.** Every gear instant is RFC3339 UTC with fractional seconds and a `Z`
(`"2026-08-26T10:31:43.083383Z"`, `format: date-time`). Legacy types these as bare `string`, so nothing
breaks at the type level, but anything that string-compared or slice-parsed a legacy timestamp must be
re-checked. `duration` stays free text on both sides and is **not** a number: `TestResultDto.duration`'s
doc says *"the runner's own duration text, verbatim — e.g. `85.06s (0:01:25)`. Not a number and not
normalised"*; `DashboardRunDto.duration` is legacy's rendered `"2m 5s"` and is `null` unless the run has
both a start and a finish.

**X4 — run state enum.** Legacy renders **capitalised Argo phases**: `types.ts:845-861` `getPhaseColor`
switches on `Running | Succeeded | Skipped | Failed | Error | Pending`, and `types.ts:885`
`isActiveRun` tests `phase === "Running" || phase === "Pending"`. The gears use qa-runs' **lowercase**
set: `created | queued | dispatching | running | succeeded | failed | canceled | timed_out | expired |
error` (`DashboardRunDto.phase`'s published doc, citing `qa-runs-sdk/src/models.rs:266-280`), and that
doc says outright *"a client ported from it must re-map, not merely re-case"* — legacy has no
`created`, `queued`, `dispatching`, `canceled`, `timed_out` or `expired`, and the gears have no
`Skipped` at the run level. The queue row's equivalent is spelled **`cancelled`, two `l`s**, against the
run's **`canceled`, one `l`** — `RunDto.state`'s doc says the difference is deliberate. Legacy's
`RunQueueEntry.state` union (`types.ts:728`) is `queued | dispatching | running | done | failed |
cancelled | expired`, which matches the queue side already.

**X5 — test status enum is unchanged, and is matched case-sensitively.** Both sides use the runner's
uppercase spellings. `types.ts:863-883` `getStatusColor` switches on eight, not six: `PASSED`,
`RUNNING`, `PENDING`, `FAILED`, `ERROR`, `SKIPPED`, `XFAIL`, `XPASS` (`:865-879`) — the two extra are
*run*-phase words appearing in a test-status helper, they have no counterpart in the gear's set, and
nothing here depends on them. `types.ts:162` (`TestCase.status`) documents the six that matter as
`PASSED | FAILED | SKIPPED | XFAIL | XPASS | ERROR`, and those are the ones `TestCaseResultDto.status`
carries. It is an **open set** on the gear side and nothing normalises it, so
`$filter=status eq 'XFAIL'` does not find a row holding `xfail` and answers an empty page rather than an
error.

**X6 — plan identity is `(repo_id, plan_path)`, not `plan_id`.** Legacy's `plan_id` is a synthetic id;
the gears identify a plan by its repository plus its path within that repository's content root
(`PlanDto` has `repo_id` + `path`, no `id`). Two consequences that are **not** the same rule:
- On the **analytics drill-downs and the overview**, the query parameter is still spelled `plan_id`, but
  its *value* is the plan's **path** — *"The plan's path within its repository, matched across every
  repository the caller can see"* (`/qa/v1/analytics/plan/tests`, `.../builds`, `.../test-history`), and
  *"Required when `scope=plan`, ignored otherwise"* on the overview. Same field name, different value
  space, and no repository disambiguation.
- On **`/qa/v1/jira/open-bugs`** and **`/qa/v1/analytics/views`** the pair is explicit and both halves
  are required together — one without the other is a 400.

**X7 — secret material never crosses the wire.** Legacy posts raw secrets; the gears take a credstore
reference: `kubeconfig` → `kubeconfig_credstore_ref`, `api_token` → `api_token_credstore_ref`,
`slack_webhook_url` → `slack_webhook_credstore_ref`, `private_key` → `private_key_pem`, and a repo's
`token`/`ssh_key_id` → `credential_ref`. Every form that today takes a pasted secret needs a credstore
write first. **This is the one place a "just rename the field" adapter is wrong.**

**Amended 2026-08-27 for the kubeconfig, which is no longer one of the five.** "Needs a credstore write
first" describes *what has to happen*, not *who has to do it*, and qa-environments now does it itself:
`POST /qa/v1/environments` and `PATCH /qa/v1/environments/{id}` accept a raw **`kubeconfig`** as the
alternative to `kubeconfig_credstore_ref`, write the document to credstore before any row exists, and
persist only the generated reference (`qa-environments-kubeconfig-<uuid>`). This is exactly
what `qa-catalog`'s `create_ssh_key` has always done with a pasted private key
(`qa-catalog/src/domain/service/ssh_keys.rs`); qa-environments simply lacked the `credstore` gear
dependency to do it. Secret material still never *reaches the database* and is never returned by any
read path — which is the property X7 exists to state. **And since 2026-08-27 the read paths no longer
return the *reference* either** (`EnvironmentDto`), because under `SharingMode::Tenant` a reference is
itself a read path to the material. See §11.7.

**X8 — unknown query parameters are accepted and silently ignored.** Verified live:
`GET /qa/v1/test-repos?product_id=abc` → 200 (unfiltered), `GET /qa/v1/dashboard?days=14&product_id=abc`
→ 200 (unscoped). So every legacy product-scoping parameter this document marks absent will *appear* to
work and quietly return the whole tenant's rows. **No 400 will warn Task 10 about a missed filter.**

---

## 4. The table — one row per exported hook

97 rows, one for each line of `grep -nE "^export (function|const) use[A-Za-z]+" src/api/hooks.ts`, in
source order. The `#` column makes completeness checkable against that command's output. Two exported
non-hooks (`exportAnalytics`, `analyticsQueryToKey`) that also touch the wire are in §4a.

`Difference` uses the brief's vocabulary: `identical`, `reshaped:`, `field:`, `enum:`, `envelope:`,
`absent`. A row may carry more than one.

| # | Hook (`hooks.ts:`) | Legacy | Gear | Difference |
|---|---|---|---|---|
| 1 | `useDashboard` :121 | `GET /dashboard?days&product_id&product_key` | `GET /qa/v1/dashboard?days` | `reshaped:` **no product scoping** — `days` is the only parameter, and it is clamped to 3–90 (365 answers 90 points, not an error). Per X8 the two product params are accepted and ignored. `field: total_plans, total_schedules, platforms_summary -> absent` (see §8-C3); `field: recent_runs/active_runs_list: WorkflowRun[] -> DashboardRunDto[]` — a ten-field projection (`qa-insights/qa-insights/src/api/rest/dto.rs:363-405`), losing `plan_id`, `run_kind`, `finished_at`, `message`, `app_build`, `test_version`, `is_validation`, `exclusive` and every `slack_*`; `field: platform (name) -> platform_id (uuid)` (its doc: *"Legacy draws a platform name here; resolving the name is a qa-environments lookup this gear does not make yet"*); `field: product_key -> **always null today**` (its own doc says so); `field: + queued_runs`; `enum:` X4 on `phase`. `failed_recent`/`flaky_tests`: `plan_id -> repo_id + plan_path`, `workflow_name -> run_id (uuid)`, `platform -> platform_id`. |
| 2 | `usePlans` :156 | `GET /plans?branch&product_id` | `GET /qa/v1/plans?repo_id*&branch*` | `reshaped:` §2 fact 2 — **both parameters required**, and the filter axis changes from product to repository. Task 10 must fan out over the product's repos (`GET /qa/v1/test-repos`, filter `product_id` client-side) and concatenate. `envelope:` still a bare array. `field: TestPlanInfo -> PlanDto` — `id -> (repo_id, path)` (X6), `plan.name -> name`, `plan.tags -> tags`, `plan.timeout_seconds -> timeout_seconds`, `plan.exclusive -> exclusive`, `test_files -> test_files`; **absent**: `dir_path`, `source`, `repo_name`, `product_id`, `product_key`, `product_name`, `versions`, `plan.description`, `plan.node_selector`, `plan.tolerations`, `plan.validation`, `recent_runs`. `+ branch` echoed back. |
| 3 | `usePlan` :177 | `GET /plans/{id}?branch` | — | `absent` — **new, and a finding** (§6). qa-catalog publishes no single-plan read. Absorbable: `GET /qa/v1/plans?repo_id&branch` and match on `path`. Same field diff as row 2. |
| 4 | `useRunPlan` :189 | `POST /plans/{planId}/run?platform&branch&schedule_id&parameters&exclusive` | `POST /qa/v1/runs` | `reshaped:` **query string becomes a JSON body** (`LaunchRunReq`). `field: planId (path) -> target{kind:"plan", repo_id, path}` (X6); `field: platform (name) -> platform_id (uuid)` (X1) and §2 fact 3 — **null is accepted and the run then never queues**; `field: parameters (JSON-encoded query string) -> parameters: RunParameterDto[]` (real array); `field: exclusive?: boolean -> exclusive: boolean\|null`; `field: schedule_id -> absent on the request` (`RunDto.schedule_id` is server-set); `field: + include_tags/exclude_tags/timeout_seconds`. `envelope: LaunchResponse -> 200 RunDto \| 202 QueuedRunDto` — `{workflow_name}` becomes the whole run, and `{queue_id, state:'queued'}` becomes `{queue_id, run_id}`: **`state` is gone**. `isQueued` (:73, narrowing on `'queue_id' in response`) still works; any read of `.state` does not. |
| 5 | `useRuns` :214 | `GET /runs?page&per_page&product_key` | `GET /qa/v1/runs?limit&cursor&$filter&$orderby` | `envelope:` X2 — **cursor pagination, no `total`, no page number**. `reshaped:` **no product scoping and no way to add one** — `RunFilterField` is `id, name, state, run_kind, platform_id, source, schedule_id, resolved_exclusive, is_validation, created_at, started_at, finished_at` (`qa-runs/qa-runs/src/infra/storage/odata.rs:85-96`) and `RunDto` carries no product key; scoping means resolving the product's repos and filtering client-side, which is wrong across pages. `field: WorkflowRun -> RunDto`: `phase -> state` (X4), `plan_id -> target{}` (X6), `run_kind -> target.kind`, `platform -> platform_id`, `message -> error`, `run_source -> source`, `exclusive -> resolved_exclusive` (+ `exclusive_tier`), `test_file -> target.test_file`; **absent**: `duration` (derive from `started_at`/`finished_at`), `product_key`, `repo_name`, `source_ref`, `source_ref_kind`, and all four `slack_*`; `+ bundle_ids`, `execution_ref`, `log_storage_ref`, `timeout_at`, `created_at`, `updated_at`. |
| 6 | `useCollectCases` :234 | `POST /analytics/collect?branch` | `POST /qa/v1/analytics/collect?branch` | `identical` — same optional `branch`, and `CollectTriggerOutcomeDto {branch, launched}` matches the legacy inline type exactly. **The only fully identical mutation in the table.** |
| 7 | `useTestRecentResults` :244 | `GET /tests/recent-results?file&limit` | `GET /qa/v1/test-results?$filter=test_file eq '…'&limit=N` | `reshaped:` §6.3 row 5 — but see §5-B: the limit is **`limit`, not `$top`**, and `$top` is ignored (X2). `$filter` allows only `id, run_id, test_file, test_name, run_finished_at`; `$orderby` allows the first four (`run_finished_at` is filterable but not sortable, because it is null mid-run and a null sort key truncates pagination). **There is no chronological sort** — default order is `id` descending, *"stable but arbitrary"*; recency is expressed as `$filter=run_finished_at ge …`. `envelope: TestRunResult[] -> Page<TestResultDto>`. `field:` `workflow_name -> run_id (uuid)`, `plan_id -> repo_id + plan_path`, `platform -> platform_id`, `app_version -> product_version`, `test_version -> branch`, `started_at -> run_finished_at` (a different instant); **absent**: `phase`, `short_error`, `reportportal_url`. |
| 8 | `useRunsByPlan` :254 | `GET /plans/{id}/runs` | — | `absent` per §6.3 category 3 — **but §5-A: it is not dead.** `pages/PlanDetailPage.tsx:71` calls it. Absorbable only in part: `GET /qa/v1/runs` cannot filter by plan (no `target.*` in `RunFilterField`), so this becomes a client-side filter over a page of runs. |
| 9 | `useRun` :263 | `GET /runs/{name}` | `GET /qa/v1/runs/{id}` | `reshaped:` X1 — 400 on a name. `envelope: RunDetails {run, test_results, reportportal_url} -> RunDetailDto` = `RunDto` **flattened** (no `run` wrapper) `+ result: RunResultDto {total, passed, failed, skipped, in_progress}` `+ test_results: RunTestResultDto[]`. `field:` all of row 5's renames; `reportportal_url -> absent` (Task 8 strips it); `TestResult.name -> test_name`, `+ test_file`, `+ run_id`, `+ created_at/updated_at`; **`TestResult.logs` and `TestResult.cases` are absent** — see §8-C2. |
| 10 | `useRunDetailsMap` :277 | `GET /runs/{name}` ×N | `GET /qa/v1/runs/{id}` ×N | As row 9. Shares the per-run cache; no consumer in `src/**/*.tsx` today. |
| 11 | `useRunDag` :300 | `GET /runs/{name}/dag` | — | `absent` per §6.3 category 3 — **but §5-A: `pages/RunDetailPage.tsx:32` calls it.** Nothing in the gears models a DAG: `RunDto` has no node graph and `CustomPlanDto` has no `nodes`. §8-C6. |
| 12 | `useRunLogs` :309 | `GET /runs/{name}/logs` → `string`, polled every 5 s | `GET /qa/v1/runs/{id}/logs` → `text/event-stream` of `RunLogLineDto {line}` | `envelope:` **a polled string body becomes an SSE stream**. This is a *second* SSE surface beyond §6.4's `useWebSocket` replacement, and §6.4's two consequences (no headers on `EventSource`; nginx `proxy_buffering off`) apply to it identically. X1 on the id. |
| 13 | `useDeleteRun` :318 | `DELETE /runs/{name}` | — | `absent` — **new, and a finding** (§6). qa-runs registers exactly three run mutations: `POST /qa/v1/runs`, `.../{id}/cancel`, `.../{id}/rerun` (`qa-runs/qa-runs/src/api/rest/routes/runs.rs:14,106,131`). Two live consumers, and — **corrected during Task 8a** — *both* mean **stop**: `pages/RunDetailPage.tsx:33` binds it as `stopRun`, and `pages/RunsPage.tsx:64` binds it as `deleteRun` but uses it only in `handleStop`, which confirms with "Stop run", is rendered (via `RunsTable.tsx:165-181`) only for `isActiveRun(run)`, and reports "is stopping". The original reading of this row was taken from the local variable *name*. One intent, and it maps to `POST /qa/v1/runs/{id}/cancel`: **absorbable, §7.12**. |
| 14 | `useRunQueue` :334 | `GET /run-queue?platform` | `GET /qa/v1/queue?platform_id&limit&$filter&$orderby` | `reshaped:` platform **name → uuid** (X1); `limit` is declared here (unlike X2's other collections). `envelope: RunQueueEntry[] -> Page<QueueEntryDto>`. `field: platform -> platform_id`, `workflow_name -> run_id (uuid)`, `target_id -> absent`; `+ ttl_expires_at`, `+ blocked_by` both already in legacy's type. `queue_position`'s doc warns it is computed over *the rows that request returned*, so a truncating `limit` understates it — pass `platform_id` rather than a bigger window. `enum:` `state` matches legacy's seven-value union (X4). |
| 15 | `useCancelQueuedRun` :345 | `POST /run-queue/{id}/cancel` → `{id, state}` | `DELETE /qa/v1/queue/{id}` → `204` | `reshaped:` §5-C corrects §6.3 row 2 — the queue-row cancel **did** survive, as a DELETE (`qa_runs.cancel_queued_row`); `POST /qa/v1/runs/{id}/cancel` is the *run* cancel and takes a run id, not a queue id. `envelope: {id, state} -> no body`. |
| 16 | `useForceStartQueuedRun` :359 | `POST /run-queue/{id}/force-start` → `{workflow_name}` | `POST /qa/v1/queue/{id}/force-start` → `StartedRunDto {run_id}` | `field: workflow_name -> run_id (uuid)`. The hook's doc-comment warning that this can still 429 on `max_concurrent_runs` remains true (`admission.rs:621` `enforce_global_cap` runs first, cluster-wide). |
| 17 | `useRerunRun` :371 | `POST /runs/{name}/rerun` | `POST /qa/v1/runs/{id}/rerun` | X1; `envelope:` as row 4 — `200 RunDto \| 202 QueuedRunDto`, and the queued arm has **no `state` field**. |
| 18 | `useSchedules` :384 | `GET /schedules?product_key` | `GET /qa/v1/schedules` | `reshaped:` **no parameters at all** — no product scoping, and none reachable client-side without walking `target.repo_id` → `GET /qa/v1/test-repos` → `product_id`. `field: ScheduleInfo -> ScheduleDto`: `schedule -> cron`, **`suspended: boolean -> enabled: boolean` (inverted)**, `last_scheduled -> last_fired_tick`, `platform (name) -> platform_id (uuid)`, `plan_id -> target{}` (X6), `plan_type -> target.kind`, `test_file -> target.test_file`; `field: include_tags/exclude_tags: string -> string[]` (a comma-string becomes a real array); `enum: exclusive?: boolean\|null -> exclusive_choice: "true" \| "false" \| "auto"` — **a nullable boolean becomes a three-valued string**; **absent**: `description`, `plan_name`, `product_key`, `product_name`, `recent_runs` (§7 covers how the Recent Runs strip is rebuilt). |
| 19 | `useCreateSchedule` :400 | `POST /schedules` (`CreateScheduleForm`) | `POST /qa/v1/schedules` (`NewScheduleReq`) | `field: cron_expr -> cron`, `schedule_id -> absent` (server-assigned `id`), `plan_id -> target{}`, `platform -> platform_id`, `include_tags/exclude_tags: string -> string[]`, `description -> absent`; `enum: exclusive -> exclusive_choice` (row 18). **`enabled` is required** on the request. **The three `slack_*` form fields are silently dropped**: `NewScheduleReq` carries no notification fields at all, so a create that sets them must be followed by a `PUT .../notifications` (`ScheduleDto.slack_notifications_enabled`'s doc: *"Read-only on this type's sibling request"*). |
| 20 | `useUpdateSchedule` :412 | `PUT /schedules/{name}` | `PUT /qa/v1/schedules/{id}` | X1 + row 19's field set. `reshaped:` **a true replace** — an omitted defaulted field is *cleared*, and `enabled` is required so an edit cannot silently disable or re-enable a schedule (`qa-runs/qa-runs/src/api/rest/dto.rs:935-937` and the paragraph under it: *"an `#[serde(default)]` here would turn every edit that omits the field into a silent disable"*). §6.3 row 1 calls this a deliberate improvement; the field-level consequence is that **the edit form must round-trip every field it did not show the user**. |
| 21 | `useSuspendSchedule` :425 | `POST /schedules/{name}/suspend` → `void` | `PUT /qa/v1/schedules/{id}` with `enabled: false` | `reshaped:` §6.3 row 1 — two buttons become one PUT, and the PUT is a **whole-record replace**, so the adapter must read the schedule first (`GET /qa/v1/schedules/{id}`) and re-send it with `enabled` flipped. `envelope: void -> ScheduleDto`. |
| 22 | `useResumeSchedule` :436 | `POST /schedules/{name}/resume` → `void` | `PUT /qa/v1/schedules/{id}` with `enabled: true` | As row 21. |
| 23 | `useDeleteSchedule` :447 | `DELETE /schedules/{name}` | `DELETE /qa/v1/schedules/{id}` | X1 only; `204` both sides. |
| 24 | `useSetScheduleNotifications` :459 | `POST /schedules/{name}/notifications` `{enabled, channel, events}` | `PUT /qa/v1/schedules/{id}/notifications` `{slack_enabled, slack_channel, slack_events}` | `reshaped:` POST → PUT, X1. `field: enabled -> slack_enabled`, `channel -> slack_channel`, `events -> slack_events`; `slack_enabled` and `slack_events` are **required** where legacy's `channel`/`events` were optional. `envelope: ScheduleInfo -> ScheduleDto` (row 18). `enum:` the six event tokens are unchanged — `pending, in_progress, succeeded, failed, error, skipped`. |
| 25 | `useTests` :477 | `GET /tests?branch&product_id` | — | `absent` — **new, and the largest finding** (§6). There is no test-file catalog endpoint anywhere in the 22 qa-catalog operations. `PlanDto.test_files: string[]` gives the *paths* and nothing else; `TestFileInfo`'s `title`, `component`, `description`, `tags`, `quality_vectors`, `versions` and `loc` have no gear source. Four live consumers. §8-C1. |
| 26 | `usePlansPrefetch` :497 | `GET /plans?branch&product_id` | as row 2 | As row 2. Returns a prefetch callback, so it also carries the hook's stated side effect of *triggering the backend branch sync* — the gears sync via `POST /qa/v1/test-repos/{id}/sync`, which is a mutation, so **prefetch must not call it**. |
| 27 | `useTestsPrefetch` :516 | `GET /tests?branch&product_id` | — | `absent`, as row 25. |
| 28 | `useTestSource` :535 | `GET /tests/source?test_file&source&repo_id&branch` | — | `absent` per §6.3 category 3 — **but §5-A: `pages/TestDetailPage.tsx:71` calls it.** Nothing serves file content; `GET /qa/v1/test-bundles/{id}` returns a whole `application/gzip` bundle, not one file. §8-C1. |
| 29 | `useTestRepositories` :551 | `GET /test-repos?product_id` | `GET /qa/v1/test-repos` | `reshaped:` **no `product_id` filter** — but `TestRepositoryDto.product_id` is present and required, so this is an honest client-side filter. Beware X8: the legacy parameter returns 200 unfiltered. `field: tests_root -> content_root`, `ssh_key_id + has_token -> credential_ref` (X7, one nullable string replacing two fields); **absent**: `source_type`, `archive_file_name`, `ssh_key_name`; `+ last_synced_at`, `+ sync_error`. |
| 30 | `useRefreshBranch` :566 | `POST /test-repos/{id}/sync?branch` → `{status, plans, tests}` | `POST /qa/v1/test-repos/{id}/sync?branch` → `TestRepositoryDto` | `envelope:` **the counts are gone** — the response is the repository row. Success/failure now reads from `sync_error`/`last_synced_at` rather than `status`. |
| 31 | `useTestRepoBranches` :582 | `GET /test-repos/{id}/branches` → `string[]` | `GET /qa/v1/test-repos/{id}/branches` → `BranchListDto {branches}` | `envelope: string[] -> {branches: string[]}`. |
| 32 | `useTestRepoBranchesForRepos` :602 | same ×N | same ×N | As row 31. Four consumers; its memoisation contract (a stable array identity, because callers put the result in `useEffect` deps) must survive the unwrapping. |
| 33 | `useCreateTestRepository` :630 | `POST /test-repos` | `POST /qa/v1/test-repos` | `field: token -> credential_ref` (X7 — a pasted token becomes a credstore reference), `tests_root -> content_root`, `ssh_key_id -> credential_ref`; **`default_branch` is required** where legacy's was optional; `product_id` required (§2 fact 1). |
| 34 | `useCreateArchiveTestRepository` :643 | `POST /test-repos/archive` (**multipart**, a `File`) | — | `absent` — **new, and a finding** (§6). No multipart upload route exists; the only bundle route is `GET /qa/v1/test-bundles/{id}`, a download. Live consumer `pages/ProductDetailPage.tsx:87`. This also strands `client.ts:65` `apiPostFormData`, whose only caller this is. §8-C4. |
| 35 | `useUpdateTestRepository` :665 | `PUT /test-repos/{id}` | `PUT /qa/v1/test-repos/{id}` | As row 33 (`UpdateTestRepoReq` is field-identical to `CreateTestRepoReq`). A replace, not a merge. |
| 36 | `useDeleteTestRepository` :679 | `DELETE /test-repos/{id}` | `DELETE /qa/v1/test-repos/{id}` | `identical` — same path shape, no body, `204`. |
| 37 | `useSyncTestRepository` :692 | `POST /test-repos/{id}/sync` → `{status, path}` | `POST /qa/v1/test-repos/{id}/sync` → `TestRepositoryDto` | As row 30. |
| 38 | `useSshKeys` :705 | `GET /ssh-keys` | `GET /qa/v1/ssh-keys` | `envelope:` bare array both sides. `field: updated_at -> absent`, `+ fingerprint`. |
| 39 | `useCreateSshKey` :712 | `POST /ssh-keys` `{name, private_key}` | `POST /qa/v1/ssh-keys` `{name, private_key_pem}` | `field: private_key -> private_key_pem` (X7 — and the name is load-bearing: PEM specifically). Response as row 38. |
| 40 | `useDeleteSshKey` :722 | `DELETE /ssh-keys/{id}` | `DELETE /qa/v1/ssh-keys/{id}` | `identical`. |
| 41 | `useRunSingleTest` :733 | `POST /plans/{planId}/run-test` `{test_file, platform, branch, schedule_id, parameters, exclusive}` | `POST /qa/v1/runs` with `target{kind:"test", repo_id, path, test_file}` | **§5-D corrects §6.3 category 3, which lists `/plans/{id}/run-test` as dead: it is neither dead (two consumers — `components/tests/RunTestDialog.tsx:43`, `pages/SchedulesPage.tsx:33`) nor absent.** `RunTargetDto.kind` is *"`plan`, `test`, `custom_plan` or `collect`"* and `test_file` is *"Required for `test`"*, so a single-test launch is served exactly. Field diff as row 4. |
| 42 | `useCustomPlans` :759 | `GET /custom-plans?product_id` | `GET /qa/v1/custom-plans` | `reshaped:` **no product filter, and none possible** — `CustomPlanDto` has no `product_id` at all (X8 makes the legacy parameter look like it works). `field: tests: {plan_id, test_file}[] -> files: {repo_id, plan_path, path}[]` (X6); **absent**: `description`, `product_id`, `included_plans`, `nodes`, `parallelism`; `+ tags`, `+ updated_at`. §8-C6, §8-C7. |
| 43 | `useCustomPlan` :773 | `GET /custom-plans/{id}` | `GET /qa/v1/custom-plans/{id}` | Path identical; field diff as row 42. |
| 44 | `useGitPlans` :782 | `GET /git-plans?product_id&branch` | — | `absent` per §6.3 category 3 — **but §5-A: `components/custom-plans/GitPlansList.tsx:14` calls it, and `pages/PlansPage.tsx:109` mounts it.** §8-C6. |
| 45 | `useRunGitPlan` :799 | `POST /git-plans/{repoId}/{planId}/run` | — | `absent` — same `/git-plans` family as row 44 (§6.3 names the collection path only). Live consumer `components/custom-plans/RunGitPlanDialog.tsx:39`, mounted from `GitPlansList.tsx:65`. §8-C6. |
| 46 | `useCreateCustomPlan` :835 | `POST /custom-plans` | `POST /qa/v1/custom-plans` (`UpsertCustomPlanReq`) | `field: tests -> files` (row 42), `description/product_id/included_plans/nodes/parallelism -> absent on the request`, `+ tags`, `+ timeout_seconds`. A create that sets the absent fields loses them silently. |
| 47 | `useUpdateCustomPlan` :846 | `PUT /custom-plans/{id}` | `PUT /qa/v1/custom-plans/{id}` | As row 46; a replace. |
| 48 | `useDeleteCustomPlan` :858 | `DELETE /custom-plans/{id}` | `DELETE /qa/v1/custom-plans/{id}` | `identical`. |
| 49 | `useRunCustomPlan` :869 | `POST /custom-plans/{id}/run?platform&branch&schedule_id&parameters&exclusive` | `POST /qa/v1/runs` with `target{kind:"custom_plan", custom_plan_id}` | `reshaped:` as row 4. `RunTargetDto.custom_plan_id` is *"Required for `custom_plan`"*; `repo_id` alongside it is *accepted and ignored* (that doc records the correction explicitly). |
| 50 | `usePlatforms` :894 | `GET /platforms?product_id` | `GET /qa/v1/environments` | `reshaped:` **no product filter**; `EnvironmentDto.product_id` is present (nullable), so a client-side filter works — but must decide what to do with `null`. `field: version -> observed_version`, `build -> observed_build` (§6.3 category 2, `qa-environments/qa-environments/src/api/rest/dto.rs:42-43`); **absent**: `namespace`, `vhp_base_url`, `version_detected_at`, `version_detect_error`; `+ available`, `+ updated_at`. `kubeconfig_credstore_ref` is **not** returned (removed from `EnvironmentDto` 2026-08-27; §11.7), so it is neither an addition nor available to a client. |
| 51 | `usePlatformDetails` :907 | `GET /platforms/{name}/details` | — | `absent` per §6.3 category 3 — **but §5-A: `pages/PlatformDetailPage.tsx:81` calls it, on the routed `/platforms/:name` page.** Partly reconstructible: `GET /qa/v1/environments/{id}` gives the `PlatformInfo` half and `GET /qa/v1/environments/{id}/lease` gives a three-state `LeaseDto` (`free` / `held_parallel{holders}` / `held_exclusive{holder}`). The cluster half — `status`, `status_message`, `node_count`, `ready_node_count`, `worker_count`, `ready_worker_count`, `control_plane_count`, `ready_control_plane_count`, `namespace_count`, `checked_at`, `nodes[]` — has no source. §8-C3. |
| 52 | `useCreatePlatform` :916 | `POST /platforms` `{name, kubeconfig, …}` | `POST /qa/v1/environments` (`CreateEnvironmentReq`) | `field: kubeconfig (raw YAML) -> kubeconfig \| kubeconfig_credstore_ref` — **exactly one of the two**, and `kubeconfig` is the legacy field unchanged: raw YAML, which the gear writes to credstore itself (X7 as amended in §2; **updated 2026-08-27**, this row previously said `kubeconfig_credstore_ref` was **required**). Both supplied is a 400 naming both fields; neither is the same `kubeconfig_credstore_ref must not be empty` 400 as before. An **empty** `kubeconfig_credstore_ref` counts as not supplied on both verbs (fixed 2026-08-27; PATCH used to read it as supplied and reject an empty-plus-paste as "both"). The response carries **neither** kubeconfig field — `EnvironmentDto` withholds the reference (§11.7). **absent**: `namespace`, `vhp_base_url`. `envelope: void -> 201 EnvironmentDto`. |
| 53 | `useUpdatePlatform` :927 | `PUT /platforms/{name}` | `PATCH /qa/v1/environments/{id}` | `reshaped:` **PUT → PATCH** and X1. `UpdateEnvironmentReq` is all-optional (a true partial): `available, default_branch, description, kubeconfig, kubeconfig_credstore_ref, name, product_id`. `kubeconfig` (added 2026-08-27) replaces the stored kubeconfig by pasting a new document — same exactly-one rule as row 52, and absent on both leaves the stored reference alone. Replacing by **either** spelling removes the superseded secret when this gear minted it (fixed 2026-08-27; a replacement spelled as a reference used to orphan it). **absent**: `namespace`, `vhp_base_url`. |
| 54 | `useDeletePlatform` :940 | `DELETE /platforms/{name}` | `DELETE /qa/v1/environments/{id}` | X1 only. (`/openapi.json` declares `204` with a JSON object schema — an artifact of the builder; treat as no body.) |
| 55 | `useRefreshPlatformVersion` :952 | `POST /platforms/{name}/refresh-version` | — | `absent` per §6.3 category 3 — **but §5-A: `pages/PlatformDetailPage.tsx:83` calls it.** Nothing in this plan observes an environment version at all (`smoke.sh:391-394`: *"the version-observation poller is a separate, out-of-scope feature"*), so this is the button that would populate the field §9 is about. §8-C3. |
| 56 | `useRenamePlatform` :965 | `POST /platforms/{name}/rename` `{new_name}` | `PATCH /qa/v1/environments/{id}` `{name}` | `reshaped:` **a new §6.3 row** — the dedicated rename verb collapses into the partial update, `new_name -> name`. Not absent, and not currently in §6.3's seven. `envelope: PlatformInfo -> EnvironmentDto` (row 50). |
| 57 | `useProductFolders` :981 | `GET /product-folders` → `string[]` | `GET /qa/v1/product-folders` → `ProductFolderListDto {folders}` | `envelope: string[] -> {folders: string[]}`. Verified live: `{"folders":[]}`. |
| 58 | `useProducts` :989 | `GET /products` | `GET /qa/v1/products` | `field: tests_folder -> folder` — **and the nullability flips**: legacy `tests_folder: string` (required), gear `folder: string\|null`. `description` is required and non-nullable on the gear side. Bare array both sides. |
| 59 | `useProduct` :996 | `GET /products/{id}` | — | `absent` — **new, and a finding** (§6), though a small one: qa-catalog serves `POST/PUT/DELETE /qa/v1/products{/id}` but no single-product `GET`. Absorbable by finding the id in the `GET /qa/v1/products` list `useProducts` already caches. |
| 60 | `useProductCoverage` :1004 | `GET /products/{id}/coverage` → `ProductCoveragePoint[]` | `GET /qa/v1/dashboard/coverage` → `CoverageBuildDto[]` | `reshaped:` §6.3 row 4 — and the field-level answer to "does it still take a product filter" is **no: it takes no parameters at all**, one point per product. `field:` **absent**: `product_id`, `run_name`, `collected_at` — and `collected_at` is what `components/products/ProductCoverageCard.tsx:18` sorts by; `+ build` (*"the two joined by a slash"*). `coverage{line_pct, branch_pct, function_pct}` is unchanged. **The array is empty in every deployment today** — its own published doc: *"nothing in this system measures a coverage point yet, and no number is folded out of the ingested test results to fill the gap"*. §8-C8. |
| 61 | `useActiveProduct` :1015 | — (no request of its own) | — | `identical` — reads `useProducts` (row 58) plus the `selectedProduct` store. Its `Product` return type carries `tests_folder`, so row 58's rename reaches every one of its ~15 call sites. |
| 62 | `useCreateProduct` :1022 | `POST /products` | `POST /qa/v1/products` | `field: tests_folder -> folder` (nullable, optional); **`description` becomes required** where legacy's was optional. |
| 63 | `useUpdateProduct` :1033 | `PUT /products/{id}` | `PUT /qa/v1/products/{id}` | As row 62 (`UpdateProductReq` ≡ `CreateProductReq`). |
| 64 | `useDeleteProduct` :1045 | `DELETE /products/{id}` | `DELETE /qa/v1/products/{id}` | `identical`. |
| 65 | `useObservedProductVersions` :1058 | `GET /products/{id}/observed-versions` → `string[]` | — (derive from `GET /qa/v1/environments`) | `reshaped:` §6.3 category 2 — `EnvironmentDto.observed_version` (`qa-environments/qa-environments/src/api/rest/dto.rs:42`). **The semantics change**: legacy returns distinct `app_version` values *observed in run results*; the gear field is the version currently observed *on an environment*, one per environment. In this deployment it is `null` on every environment (§9), so the Analytics version dropdown is empty. §8-C9. |
| 66 | `useObservedProductBranches` :1068 | `GET /products/{id}/observed-branches` → `string[]` | — (nearest: `GET /qa/v1/test-repos/{id}/branches`) | `absent` — **§5-E corrects §6.3 category 2**, which maps this onto `observed_build`. A build is not a branch. Legacy returns distinct branches *seen in run results*; the gears' only branch list is the repository's **git** branches (`BranchListDto`), a superset that includes branches with no results. `TestResultDto.branch` exists but is not in `TestResultsField`, so it cannot be filtered or aggregated. §7 records the substitution; it is a superset, not a fabricated value. |
| 67 | `useTestAnalytics` :1077 | `GET /analytics/plan/{planId}/tests` | `GET /qa/v1/analytics/plan/tests?plan_id=*` | `reshaped:` §6.3 row 3 — and the field-level answer to *which query parameter carries plan identity*: it is spelled **`plan_id`** and its **value is the plan's path** (X6), *"matched across every repository the caller can see — there is no `product_id` here to fix one repository"*. `field: TestAnalytics -> PlanTestAnalyticsDto`: `last_run_name -> last_run_id (uuid)`, `last_platform` kept **plus** `last_platform_id`; `last_version` and `jira_key` unchanged. Counts are `int64`. Note the semantics: *"`pass_count` and `fail_count` match the runner's literal PASSED/FAILED and nothing else, so an ERROR execution counts toward the total but toward neither"*, and the read is **bounded to the trailing 90 days** where legacy had no window. A `plan_id` naming nothing is an empty array, not a 404. |
| 68 | `useBuildDistribution` :1085 | `GET /analytics/plan/{planId}/builds` | `GET /qa/v1/analytics/plan/builds?plan_id=*` | As row 67. `field: BuildDistribution -> PlanBuildDistributionDto` — identical field set (`build, total, passed, failed, skipped`), all `int64`. |
| 69 | `useTestHistory` :1093 | `GET /analytics/plan/{planId}/test-history` | `GET /qa/v1/analytics/plan/test-history?plan_id=*` | As row 67. `field: TestHistoryEntry.run_name -> run_id (uuid)`; `build` and `status` unchanged. |
| 70 | `useAnalyticsOverview` :1119 | `GET /analytics/overview?…` | `GET /qa/v1/analytics/overview?…` | Path and all nine parameter **names** survive (§2 fact 4 for the three required ones), but `plan_id`'s value space changes (X6) and `scope`/`group_by` are matched case-insensitively. `field: AnalyticsOverview -> AnalyticsOverviewDto`: **`product_key` absent**; `build_distribution[].latest_run_name -> latest_run_id (uuid)`; `grouped.platform[].value -> platform (nullable name) + platform_id (uuid)` — a different shape from `grouped.component`/`grouped.tag`, which keep `value`; `lists[].plan_id -> repo_id + plan_path`, `lists[].last_run_name -> last_run_id (uuid)`, `+ lists[].last_platform_id`, and `case_status`/`case_tickets` become **required** (`case_tickets` non-null). `summary`, `heatmap`, `trend`, `flaky` and `quality_vectors` are field-identical. **§9 is about the values, not the shape.** `days_heatmap` clamps to 1–30 and `days_trend` to 7–365, silently. |
| 71 | `useAnalyticsBuildTests` :1141 | `GET /analytics/build-tests?…&build` | `GET /qa/v1/analytics/build-tests?…&build*` | Same eight parameters plus required `build`. `field: BuildTestDetailItem.run_name -> run_id (uuid)`; `run_finished_at` is required and non-null here (unlike on `TestResultDto`); `component`, `tags`, `status`, `test_file`, `test_name` unchanged. |
| 72 | `useAnalyticsSavedViews` :1168 | `GET /analytics/views?scope&plan_id` + `X-Analytics-Owner: <ownerId>` | `GET /qa/v1/analytics/views?scope*&repo_id&plan_path` | `reshaped:` `plan_id -> (repo_id, plan_path)`, **both required together when `scope=plan`** (X6). `field:` **the `X-Analytics-Owner` header is not read** — `handlers/saved_views.rs:60-73` takes the owner from the `SecurityContext`, i.e. the bearer token's subject, and the endpoint doc says *"A view is visible only to the caller who created it"*. The hook's `ownerId` argument becomes inert; legacy's ability to read another owner's views is gone. `field: SavedViewDto.plan_id -> plan_path + repo_id`. Ordered most-recently-updated first. |
| 73 | `useCreateAnalyticsSavedView` :1188 | `POST /analytics/views` + owner header | `POST /qa/v1/analytics/views` (`NewSavedViewReq`) | As row 72. `field: plan_id -> plan_path + repo_id`. `409` on a duplicate `(owner, scope, plan, name)`. A plan supplied with `scope=all` is *"accepted but not stored"*, not rejected. `query_json` stays opaque — *"this gear never inspects"* it — so a `plan_id` inside it is **not** reconciled with the new vocabulary and will still hold a legacy plan id. |
| 74 | `useUpdateAnalyticsSavedView` :1203 | `PUT /analytics/views/{id}` + owner header | `PUT /qa/v1/analytics/views/{id}` | As row 73. `onSuccess` re-keys on `data.plan_id`, which is now `data.plan_path`. |
| 75 | `useDeleteAnalyticsSavedView` :1218 | `DELETE /analytics/views/{id}` + owner header | `DELETE /qa/v1/analytics/views/{id}` | Path identical; header inert (row 72). `204`. |
| 76 | `useJiraConfig` :1234 | `GET /settings/jira` | `GET /qa/v1/settings/jira` | `field: api_token -> api_token_credstore_ref` (X7). `issue_type` nullable both sides; `url`, `project_key`, `email`, `enabled` unchanged. |
| 77 | `useUpdateJiraConfig` :1241 | `PUT /settings/jira` → `void` | `PUT /qa/v1/settings/jira` → `JiraSettingsDto` | As row 76. `envelope: void -> the saved config`. **The form field that today accepts a pasted API token must write to credstore first.** |
| 78 | `useReportPortalConfig` :1252 | `GET /settings/reportportal` | — | `absent` — **expected**: spec §6.5 removes the ReportPortal settings page. Not a finding. |
| 79 | `useUpdateReportPortalConfig` :1259 | `PUT /settings/reportportal` | — | `absent` — expected, §6.5. |
| 80 | `useRunnerDefaults` :1269 | `GET /settings/runner-defaults` | — | `absent` — expected, §6.5. Note `App.tsx:80` makes `runner-defaults` the **`/settings` index redirect target**, so Task 8 must re-point it as well as delete the page. |
| 81 | `useUpdateRunnerDefaults` :1276 | `PUT /settings/runner-defaults` | — | `absent` — expected, §6.5. |
| 82 | `usePipelineVariables` :1286 | `GET /settings/variables` → `{variables:[…]}` | `GET /qa/v1/variables` → `VariableDto[]` | `reshaped:` §6.3 row 6 — one variables surface. `envelope: {variables: PipelineVariable[]} -> VariableDto[]` (bare array). `field: + id (uuid)`, `+ environment_id`; **`secure` absent** — and it has no substitute: `VariableDto` carries `{id, environment_id, name, value}` and nothing else. The UI's secure checkbox, password input, `********` masking and padlock are all promises the gear cannot keep. **§8-C10**, which is the only C-item with a confidentiality consequence. |
| 83 | `useUpdatePipelineVariables` :1293 | `PUT /settings/variables` — **replaces the whole set** | `PUT /qa/v1/variables` — **upserts one** (+ `DELETE /qa/v1/variables/{id}`) | `reshaped:` the granularity inverts. The editor hands `onSave` the full list (`components/settings/VariablesEditor.tsx:30-33`), so the adapter must diff it against the last read and issue one PUT per changed row plus one DELETE per removed row. **Not atomic**: a partial failure leaves the set half-written, which the whole-set PUT could not do. `field:` `UpsertVariableReq {name, environment_id, value}` — keyed on the natural key, no `secure`. |
| 84 | `usePlatformVariables` :1303 | `GET /platforms/{name}/variables` | `GET /qa/v1/variables?environment_id=<uuid>` | `reshaped:` §6.3 row 6 — and the field-level answer to *whether `/qa/v1/variables` offers a platform filter*: **yes, an optional `environment_id`**, and its semantics are **additive**: *"List global pipeline variables, **plus** a platform's variables when `environment_id` is given"*. Legacy's per-platform endpoint returned the platform's own set. So the adapter must partition the result on `environment_id != null` rather than pass it through. X1 on the name→uuid. |
| 85 | `useUpdatePlatformVariables` :1312 | `PUT /platforms/{name}/variables` — whole set | `PUT /qa/v1/variables` with `environment_id` — one at a time | As rows 83 and 84 combined. |
| 86 | `useNotificationsConfig` :1328 | `GET /settings/notifications` | `GET /qa/v1/settings/notifications` | `field: slack_webhook_url -> slack_webhook_credstore_ref` (X7); `+ run_queue_queued_slack_enabled` (new, required). Every other field, including the whole six-event `scheduled_run_slack_templates` tree, is field-identical. |
| 87 | `useUpdateNotificationsConfig` :1335 | `PUT /settings/notifications` → `void` | `PUT /qa/v1/settings/notifications` → `NotificationConfigDto` | As row 86. `envelope: void -> the saved config`. The webhook-URL input becomes a credstore write. |
| 88 | `useTestNotification` :1345 | `POST /settings/notifications/test` — **no body** | `POST /qa/v1/settings/notifications/test` — `NotificationTestReq {config*, event*}` | `reshaped:` **a bodyless POST gains a required body.** The gear has one test endpoint where legacy had two shapes on the same path; this hook is the bodyless one and cannot be called as written. Absorbable by sending the currently-loaded config plus a chosen `event`. `envelope: void -> NotificationTestOutcomeDto {status}`. |
| 89 | `usePreviewScheduledRunNotification` :1351 | `POST /settings/notifications/preview` `{config, event}` | `POST /qa/v1/settings/notifications/preview` (`NotificationPreviewReq`) | `identical` request; response `NotificationPreviewDto {event, event_label, rendered_message, fallback_text, blocks}` matches `ScheduledRunNotificationPreviewResponse` field for field. Modulo row 86's `config` shape, **the closest thing to identical in the settings area.** |
| 90 | `useTestScheduledRunNotification` :1358 | `POST /settings/notifications/test` `{config, event}` → `void` | `POST /qa/v1/settings/notifications/test` | Request matches `NotificationTestReq` exactly (modulo row 86's `config`). `envelope: void -> NotificationTestOutcomeDto {status}`. |
| 91 | `useNotificationLog` :1365 | `GET /settings/notifications/log?limit` | `GET /qa/v1/settings/notifications/log?limit` | Path and parameter identical; bare array both sides. `field: id: number -> id: string(uuid)`, `workflow_name -> run_id (uuid\|null)` — **the log line loses its human-readable run name** and needs an X1 lookup to render one. `channel`, `event_type`, `outcome`, `detail`, `created_at` unchanged. |
| 92 | `useJiraPollerConfig` :1373 | `GET /settings/jira-poller` | `GET /qa/v1/settings/jira-poller` | `identical` — `{poll_interval_seconds, auto_rerun_on_resolve}` both sides, both required. |
| 93 | `useUpdateJiraPollerConfig` :1380 | `PUT /settings/jira-poller` → `void` | `PUT /qa/v1/settings/jira-poller` → `JiraPollerConfigDto` | `envelope: void -> the saved config`; request identical. |
| 94 | `useRepoPollerConfig` :1390 | `GET /settings/repo-poller` | — | `absent` — expected, §6.5. |
| 95 | `useUpdateRepoPollerConfig` :1397 | `PUT /settings/repo-poller` | — | `absent` — expected, §6.5. |
| 96 | `useCreateJiraTicket` :1407 | `POST /runs/{runName}/jira` `{test_name}` | `POST /qa/v1/jira/bugs` `{run_id*, test_name}` | `reshaped:` §6.3 row 7 — filing and listing split. `field: runName (path segment) -> run_id (uuid, in the body)` (X1). `envelope: JiraCreateResponse[] -> JiraBugFilingDto[]` — `{jira_key, created}` both sides, so the array element is unchanged. |
| 97 | `useOpenBugs` :1414 | `GET /jira/open-bugs?plan_id` | `GET /qa/v1/jira/open-bugs?repo_id&plan_path` | `reshaped:` §6.3 row 7 — *"the GET takes `(repo_id, plan_path)` not `plan_id`"*, and the field-level detail is that **the two must be supplied together; one without the other is a 400**, so the hook's "omit for all bugs" branch must omit both. `field: JiraBug.id: number -> id: string(uuid)`, `plan_id -> repo_id + plan_path`, `platform (name) -> platform_id (uuid)`; `jira_key`, `test_name`, `app_version`, `status`, `summary`, `created_at`, `resolved_at` unchanged. Note the match is *exact* here, unlike the analytics drill-downs' path-only match (row 67). |

### 4a. Two exported non-hooks that also touch the wire

| Symbol | Legacy | Gear | Difference |
|---|---|---|---|
| `exportAnalytics` `hooks.ts:1150` | `GET /api/analytics/export?…&format&section` via a **raw `fetch`**, returning a `Blob` | `GET /qa/v1/analytics/export` | `reshaped:` it hardcodes `/api/...` and so **bypasses `client.ts` entirely** — no `API_BASE_URL`, and Task 9's single `Authorization` injection point will not cover it. Parameters survive (the overview's nine plus `format` and `section`). Behaviour worth pinning: an unrecognised `format` answers **JSON, not an error**; an unrecognised `section` is a 400 on the JSON branch and a **200 with an empty body** on the CSV branch; `build_distribution`, `quality_vectors` and `grouped` are reachable only via `section=all`; and the CSV `summary` block carries only `total/passed/failed/not_run` and their percentages — the six per-case counters and `case_expected` are JSON-only, which matters for §9. |
| `analyticsQueryToKey` `hooks.ts:1115` | pure — serialises `AnalyticsOverviewQuery` | — | `identical` in shape; its `plan_id` value space changes with X6. |

### 4b. One component bypasses `hooks.ts`

`components/analytics/CoverageChart.tsx:20` calls `apiGet<CoverageBuild[]>('/dashboard/coverage')`
directly. It is the **only** such call outside `src/api/` (`grep -rn "apiGet\|apiPost\|apiPut\|apiDelete\|fetch("
--include=*.tsx --include=*.ts src | grep -v "^src/api/"`), and a `hooks.ts`-only adapter will miss it.
It already renders an honest empty state when the array is empty (`:36-41`). **Corrected during Task
8a:** this component is imported by nothing — `grep -rn "CoverageChart" src` returns only its own file —
so it is mounted on no page and no route, and the gap it describes is latent rather than live. §8-C8's
only reachable consumer is `components/products/ProductCoverageCard.tsx`.

---

## 5. Five corrections to spec §6.3, found at the field level

Recorded because Tasks 8, 10 and 11 are planned against §6.3 as written.

**A. Category 3's "nothing calls them / Zero UI impact" is false for all seven.** §6.3 says these paths
*"appear only in `hooks.ts`, with no component consumer anywhere in the 114 files"*. Re-measured over the
same 114 files (`find src -name '*.tsx' -o -name '*.ts' | wc -l` = 114), every one has a live consumer on
a **routed** page:

| §6.3 category-3 path | Hook | Consumer | Route |
|---|---|---|---|
| `/tests/source` | `useTestSource` | `pages/TestDetailPage.tsx:71` | `App.tsx:62` `/tests/view` |
| `/platforms/{id}/refresh-version` | `useRefreshPlatformVersion` | `pages/PlatformDetailPage.tsx:83` | `App.tsx:69` `/platforms/:name` |
| `/platforms/{id}/details` | `usePlatformDetails` | `pages/PlatformDetailPage.tsx:81` | `App.tsx:69` |
| `/plans/{id}/run-test` | `useRunSingleTest` | `components/tests/RunTestDialog.tsx:43`, `pages/SchedulesPage.tsx:33` | mounted from `components/tests/TestCatalogTable.tsx:7` and `components/analytics/AnalyticsDashboard.tsx:512` |
| `/plans/{id}/runs` | `useRunsByPlan` | `pages/PlanDetailPage.tsx:71` | `App.tsx:57` `/plans/:id` |
| `/git-plans` | `useGitPlans`, `useRunGitPlan` | `components/custom-plans/GitPlansList.tsx:14`, `RunGitPlanDialog.tsx:39` | mounted at `pages/PlansPage.tsx:109` |
| `/runs/{id}/dag` | `useRunDag` | `pages/RunDetailPage.tsx:32` | `App.tsx:59` `/runs/:name` |

Dropping these hooks is therefore **not** zero-impact: it breaks six routed pages, and Phase B's browser
gate will catch it. One of the seven (`run-test`) needs no work at all — see D. The other six are §8-C1,
C3 and C6.

**B. `$top` is not the pagination knob.** §6.3 row 5 says ad-hoc filters become *"OData `$filter`/`$top`"*.
`$top` is **silently ignored** on `/qa/v1/test-results` (verified: `?$top=1` and `?$top=0` both returned
the default page of 200). Page size is an **undeclared `limit` query parameter** and paging is by
`cursor`. See X2.

**C. The queue cancel did not move.** §6.3 row 2 maps `/run-queue/{id}/cancel` onto
`POST /qa/v1/runs/{id}/cancel`. Both exist and they are different operations:
`DELETE /qa/v1/queue/{id}` (`qa_runs.cancel_queued_row`) takes a **queue-row** id, and
`POST /qa/v1/runs/{id}/cancel` takes a **run** id. `useCancelQueuedRun` holds a queue-row id
(`RunQueueEntry.id`), so it maps to the DELETE.

**D. `/plans/{id}/run-test` is served, not absent.** `POST /qa/v1/runs` with `target.kind = "test"` and
`target.test_file` is exactly a single-test launch (`RunTargetDto.kind`: *"`plan`, `test`, `custom_plan`
or `collect`"*; `test_file`: *"Required for `test`"*). It belongs in category 1, not category 3.

**E. `observed-branches` does not map to `observed_build`.** §6.3 category 2 sends both
`/products/{id}/observed-versions` and `/products/{id}/observed-branches` to
`qa-environments/qa-environments/src/api/rest/dto.rs:42-43`. `observed_version` is a fair match for the first; `observed_build` is
a **build**, not a branch, and is not a match for the second. See row 66 and §7.

---

## 6. New `absent` rows — the findings

The table carries 20 `absent` rows. Thirteen are **expected**: §6.3 category 3's seven paths
(rows 8, 11, 28, 44, 45, 51, 55 — but see §5-A, they are not dead; §5-D moves the seventh member of
that category, `/plans/{id}/run-test`, out of it entirely, which is why only seven paths leave six
`absent` rows here plus row 8) and §6.5's three settings pages (rows 78, 79, 80, 81, 94, 95, two hooks each). Per the brief,
every *other* `absent` is a finding. The remaining **seven rows** are these six findings:

| Hook(s) | Legacy path | Why it is absent | Live consumers |
|---|---|---|---|
| `useTests` (25), `useTestsPrefetch` (27) | `GET /tests` | qa-catalog publishes 22 operations and none of them is a test-file catalog. `PlanDto.test_files` gives paths only. | `pages/TestCatalogPage.tsx:9`, `pages/TestDetailPage.tsx:3`, `pages/CustomPlanEditorPage.tsx:32`, `components/schedules/CreateScheduleDialog.tsx:226` |
| `useDeleteRun` (13) | `DELETE /runs/{name}` | qa-runs registers only `POST /qa/v1/runs`, `.../{id}/cancel`, `.../{id}/rerun`. Both consumers mean *stop*, which maps to cancel (corrected during Task 8a — see §7.12). No consumer deletes a run. | `pages/RunDetailPage.tsx:33` (as `stopRun`), `pages/RunsPage.tsx:64` (as `deleteRun`, used only by `handleStop`) |
| `useCreateArchiveTestRepository` (34) | `POST /test-repos/archive` (multipart) | No multipart route anywhere in the gears; the only bundle route is a download. | `pages/ProductDetailPage.tsx:87` |
| `usePlan` (3) | `GET /plans/{id}` | No single-plan read; a plan has no id (X6). **Absorbable** — list and match on `path`. | `pages/PlanDetailPage.tsx:3` |
| `useProduct` (59) | `GET /products/{id}` | No single-product read, though POST/PUT/DELETE by id all exist. **Absorbable** — the list is already cached. | `pages/ProductDetailPage.tsx:80` |
| `useObservedProductBranches` (66) | `GET /products/{id}/observed-branches` | §6.3 category 2 claims this maps to a platform DTO field; §5-E shows it does not. Nothing aggregates the distinct branches seen in results. **Absorbable as a documented superset** (§7.10), not as an equivalent. | `components/analytics/AnalyticsDashboard.tsx:616` (the branch filter) |

Rows 3, 59 and 66 are absorbable and are covered in §7 — as is row 13 (§7.12, reclassified during
Task 8a). Rows 25, 27 and 34 are in §8.

---

## 7. Absorbable in `hooks.ts` — the adapter work Task 10 does

Everything here can be made to work behind the existing hook name and signature, with no component
change. Grouped by the kind of adapter, because each kind is written once.

**7.1 Identifier resolution (X1).** Runs, platforms and schedules are addressed by name in the UI's own
routes and by uuid in the gears. `hooks.ts` resolves name → id: for runs via
`GET /qa/v1/runs?$filter=name eq '…'` (`name` is a filterable field), for platforms and schedules from
the list each hook already fetches. Affects rows 9–13, 17, 51, 53, 54, 56, 84, 85, 91, 96.

**7.2 Envelope unwrapping.** `{folders}` → `string[]` (57), `{branches}` → `string[]` (31, 32),
`{variables}` → array and back (82), `RunDetailDto` re-nested into `{run, test_results}` (9). Mechanical.

**7.3 Cursor pagination behind a page-number API (X2).** `useRuns(refetchInterval, page, perPage)` keeps
its signature; `hooks.ts` keeps a cursor per `(query, page)` and walks forward. **`total` and
`total_pages` cannot be supplied** — the pager must degrade to next/previous, which is a *component*
consequence to raise with the reviewer rather than something `hooks.ts` can hide. Do **not** synthesise a
total from `items.length`.

**7.4 Query-string → request-body launches.** Rows 4, 41, 49: one `LaunchRunReq` builder, one
`RunTargetDto` per kind, one `platform` name → `platform_id` lookup, and the tri-state `exclusive`
passed through. **Refuse to launch with a null `platform_id`** rather than sending one (§2 fact 3): a
silent never-queued run is worse than a client-side error. `LaunchResponse` narrows on `queue_id` as
before; the `state: 'queued'` literal must be synthesised by the adapter or the union re-typed.

**7.5 Whole-set writes → per-item upserts.** Rows 83, 85 (variables) and 21, 22 (schedule
suspend/resume, which need a read-modify-write because the PUT is a replace). Each needs a documented
non-atomicity note where the legacy call was atomic. The variables rows are absorbable **only in their
plumbing** — the `secure` flag they carry is §8-C10 and is not resolved by this adapter.

**7.6 Client-side filtering where a server filter disappeared.** Rows 29 (`test-repos` by
`product_id`), 50 (`platforms` by `product_id`), 84 (variables partitioned on `environment_id != null`).
Safe because the filter key is on the DTO. **Not** safe for rows 5, 18 and 42, where the key is absent
entirely — those are §8-C7.

**7.7 Lists standing in for single reads.** Rows 3 (`usePlan`) and 59 (`useProduct`).

**7.8 Field renames and enum re-mapping.** The full list is in the table; the two that touch shared
helpers are X4 (`getPhaseColor` / `isActiveRun` in `types.ts:845-861` and `:885` must learn the lowercase set, and
`canceled` vs `cancelled` must not be normalised away) and X6 (`plan_id` → `(repo_id, plan_path)`
everywhere, including the analytics parameter that keeps the *name* `plan_id` but changes its *value*).

**7.9 One constant that is a reproduction, not an invention.** `DashboardRunDto.product_key` → `null`,
which is what the gear already sends, with its own doc saying so — *"Always `null` today"*
(`qa-insights/qa-insights/src/api/rest/dto.rs:392-396`). Bind the field; do not fill it.

`PipelineVariable.secure` was in this list in the first draft of this document, justified as the same
kind of reproduction. **It is not one, and the justification cited the wrong type.** It is §8-C10.

**7.10 One documented semantic substitution.** `useObservedProductBranches` (66) is fed from
`GET /qa/v1/test-repos/{id}/branches` — the repository's git branches. This is a **superset** of legacy's
"branches seen in results": the Analytics branch filter will offer branches that have no data, and
selecting one yields an empty view rather than a wrong number. That is a legitimate substitution under
"never invent a default" (nothing is fabricated), but it **changes what the control means** and must be
stated in the UI's own copy or raised with the reviewer. It is *not* `observed_build` (§5-E).

**7.11 Two `absent` hooks with partial substitutes.** `useRunsByPlan` (8) and
`usePlatformDetails` (51) each have a partial substitute (a client-side filter over `GET /qa/v1/runs`;
`GET /qa/v1/environments/{id}` plus `/lease`). Their *remaining* fields are in §8.

**7.12 One row reclassified out of §8 (`useDeleteRun`, row 13, was §8-C5).** Added during Task 8a, from
the implementation. `POST /qa/v1/runs/{id}/cancel` serves this hook completely, because **both**
consumers mean *stop*: `pages/RunsPage.tsx:64` names the binding `deleteRun`, but its only use is
`handleStop` — confirm "Stop run", "The running workflow will be terminated.", toast "is stopping" — and
`components/runs/RunsTable.tsx:165-181` renders that action only when `isActiveRun(run)`.
`pages/RunDetailPage.tsx` is the same intent under the name `stopRun`. There is **no delete-a-run
affordance anywhere in the UI**, and `RunsPage.tsx` is byte-identical to the untouched legacy copy, so
this is legacy's own wiring rather than something the port introduced. The original §8-C5 row was
derived from the local variable *name* rather than from the behaviour, which is why it read as two
intents. Adapter action: map to cancel, and do not synthesise a delete.

---

## 8. Cannot be absorbed — report to a human

**Nine items: C1–C4 and C6–C10.** Each is a field the UI renders that the gears do not serve. Per the
Global Constraints none is fixed by widening a gear, and none is filled with a default. (C5 was
reclassified as **absorbable** during Task 8a and moved to §7.12; its identifier is retired rather than
reused, so C6–C10 keep the names every other document already cites.) **C10 is the only one
with a confidentiality consequence** rather than a display one.

**C1 — The test catalog has no backend.** `useTests` and `useTestSource` (rows 25, 27, 28). Four routed
consumers, including the whole `/tests` page and `/tests/view`. `PlanDto.test_files` supplies paths; the
rendered `title`, `component`, `description`, `tags`, `quality_vectors`, `versions`, `loc` and the file's
source text have **no source anywhere in the gears**. A catalog page listing bare paths with every
metadata column blank is a different product, not a degraded one. *Decision needed from a human:*
degrade `/tests` to a path list, or remove the surface the way §6.5 removes the three settings pages.

**C2 — Per-test logs on the run detail.** `RunDetails.test_results[].logs` (`types.ts:175`, row 9). The
gear's own DTO doc states the gap rather than leaving it implicit: *"`logs` is not here and cannot be,
which is a parity gap rather than a design choice: legacy's `test_results` has a `logs TEXT` column that
its run detail view renders, and this gear's only source of outcomes is qa-runs, whose `RunTestResult`
carries no per-test log slice."* `TestResult.cases[]` (the per-function breakdown) is likewise absent
from `RunTestResultDto`; the case rows exist at `GET /qa/v1/test-case-results` filtered by `run_id`, so
**`cases` is recoverable and `logs` is not.** The run-level stream (`GET /qa/v1/runs/{id}/logs`) is not a
substitute — it is the whole run's output, not one test's slice.

**C3 — Environment health.** Three surfaces, one root cause: qa-environments models an environment's
availability as a single `available: boolean` and observes nothing about the cluster behind it.
- `DashboardStats.platforms_summary` (`pages/DashboardPage.tsx:55-56,78`) needs
  `healthy/degraded/unhealthy/unreachable` counts and `PlatformBrief.status/version/build`.
  `DashboardStatsDto` has no such field.
- `PlatformDetails` (row 51) needs `status`, `status_message`, six node counts, `namespace_count`,
  `checked_at` and `nodes[]` — none exists.
- `useRefreshPlatformVersion` (row 55) is the button that would populate `observed_version`; nothing in
  this plan observes a version at all.

**C4 — Archive test repositories.** `useCreateArchiveTestRepository` (row 34). A repository can only be
created from a git URL. The archive-upload form on `pages/ProductDetailPage.tsx:87` has no endpoint, and
`CreateArchiveTestRepositoryForm.archive: File` has nowhere to go.

**C5 — Withdrawn: it is absorbable, and is now §7.12.** *Stopping* a run is served by
`POST /qa/v1/runs/{id}/cancel`, and Task 8a's implementation established that **both** consumers of
`useDeleteRun` mean stop — `pages/RunsPage.tsx:64` binds it under the name `deleteRun` but uses it only
in `handleStop`, and the button `RunsTable.tsx:165-181` draws for it says "Stop run" and is rendered
only for an active run. No decision is needed, and no UI was changed for it. The entry is kept rather
than deleted so that the identifier C5 is not silently reused by a later item.

**C6 — Everything DAG.** `useRunDag` (11), `useGitPlans` (44), `useRunGitPlan` (45), and
`CustomPlan.nodes`/`parallelism`/`included_plans` (42, 46, 47). `CustomPlanDto` is a flat list of files;
no gear type carries a node graph, a dependency edge, a `run_if`, or a parallelism cap. This strands
`components/custom-plans/PlanDagView.tsx`, `PlanPipelineView.tsx`, `GitPlansList.tsx` and
`RunGitPlanDialog.tsx`, and the DAG panel on `pages/RunDetailPage.tsx:32`.

**C7 — Product scoping of runs, schedules and custom plans.** Rows 5, 18, 42. `RunDto`, `ScheduleDto` and
`CustomPlanDto` carry no product key and no `product_id`, and `RunFilterField` has no target field, so
there is no server filter **and no client-side filter either**. Worse, X8 means the legacy parameters
return 200 with the whole tenant's rows: `RequireProduct` will render a product-scoped page showing
another product's runs, and nothing will error. This is the highest-risk row in the document, because it
fails **silently and plausibly**. The partial workaround (walk `target.repo_id` → `GET /qa/v1/test-repos`
→ `product_id`) is wrong across pages and impossible for custom plans, which have no repo either.
- Also here: `CustomPlan.description` and `ScheduleInfo.description`, both absent from their DTOs.

**C8 — Code coverage.** Rows 60 and 4b. `GET /qa/v1/dashboard/coverage` is documented as empty in every
deployment: *"nothing in this system measures a coverage point yet, and no number is folded out of the
ingested test results to fill the gap."* Both call sites already render an honest empty state
(`CoverageChart.tsx:37`, `ProductCoverageCard.tsx:36`), so **no zero is displayed** — and, corrected
during Task 8a, only one of the two is reachable at all: `CoverageChart` is imported by nothing (§4b), so
`ProductCoverageCard`, mounted at `pages/ProductDetailPage.tsx:562`, is the sole live consumer
— but a "Code Coverage" card that is permanently empty is a surface a human should decide to keep or
drop. `collected_at`, which `ProductCoverageCard.tsx:16-20` sorts by, is also absent from `CoverageBuildDto`.

**C9 — The analytics execution-scoped counters, and the version filter that feeds them.** See §9, which
is this item stated at length because it needs a decision rather than a report.

**C10 — `PipelineVariable.secure`, and the confidentiality it promises.** This is the only item in this
list with a **security** dimension rather than a display one, so it is stated at length.

*What the UI renders.* `PipelineVariable` (`types.ts:751-755`) carries `secure: boolean`, and
`components/settings/VariablesEditor.tsx` — the shared editor behind both `/settings/variables` and the
per-platform variables panel — makes it a **user-settable promise**, not a passive field:
- a "Secured" checkbox the user ticks when adding a variable (`:136-146`);
- the value input switches to `type="password"` with `autoComplete="new-password"` while it is ticked
  (`:125`, `:129`);
- a saved secure variable's value is replaced with `SECRET_PLACEHOLDER = '********'` before it is shown
  (`:10`, `:20-25`) and rendered as `********` in the list (`:171-175`);
- each row draws a `Lock` icon when secure and an `Unlock` icon when not (`:179`).

The component's own prop doc states the contract it expects from the backend: *"Resolves with the
server's view (secure values masked) so the editor can show what the server actually stored"*
(`:30-33`).

*What the gears serve.* Nothing. `VariableDto`
(`qa-environments/qa-environments/src/api/rest/dto.rs:151-156`) is `{id, environment_id, name, value}` and
`UpsertVariableReq` (`:175-179`) is `{environment_id, name, value}` — **neither has a `secure` field**, and
neither does the model behind them (`qa-environments/qa-environments-sdk/src/models.rs:165-170`, `value:
String`). There is no masking, no credstore reference (contrast X7, where every other secret on this
surface became one) and no encrypted column: `GET /qa/v1/variables` returns `value` verbatim to any
caller holding the read grant.

*What breaks, stated plainly.* A user ticks "Secured", types a credential, and saves. The adapter drops
the flag on the way out because there is nowhere to put it. The gear stores and returns the credential
in cleartext. The flag comes back `false` — or, if the adapter hardcodes it, comes back `true` over a
value that was never protected. Either way Task 11's verbatim editor draws the row: unmasked with an
open padlock, or masked with a closed padlock over a secret the server is serving in the clear to
everyone with `qa.variable` list access. **The second is worse than the first**, because the padlock is
the UI telling the user a protection exists.

*Why this is C-list and not a §7 adapter constant.* The first draft of this document put it in §7.9 as a
constant `false`, justified by the doc comment at `types.ts:71-72`. That comment — *"Never secret, so no
`secure` flag (the backend forces it off)"* — is on **`RunParameter`** (`types.ts:73-76`), a different
interface, and its whole point is that `RunParameter` has **no** `secure` field. `PipelineVariable` has
one, with no such comment. So the constant had no source: it was an invented default of exactly the kind
the Global Constraints forbid, and it would have been invented on the one field where being wrong has a
confidentiality consequence.

*Decision needed from a human*, and the options are not equivalent: strip the secure affordance from the
editor so the UI stops promising what the backend cannot do (a component change beyond §6.5's list, and
the only option that is honest today); or keep the surface and accept that "Secured" means nothing; or
treat this as the one place the feature freeze should be revisited. **This document does not choose** —
it is a backend capability question, and the Global Constraints put that with a human.

---

## 9. The decision: what the Analytics page does about permanently-zero counters

### 9.1 The gap, stated precisely

`analytics/overview` binds its required `version` parameter verbatim against each result row's stored
`app_version`. Every run's `app_version` is `null` in every deployment this plan produces: it is
snapshotted at launch from the environment's `observed_version`, which nothing here ever sets, because the
version-observation poller is a separate and out-of-scope feature. **So no value of `version` can match
an execution row, and the execution-scoped fields are 0 by construction rather than because ingest
failed.** The full argument, with its source citations, is at
`gears/qa-platform/deploy/compose/smoke.sh:388-403`.

Phase A's gate handles this by asserting on `summary.case_expected` instead — a static count derived from
the plan universe qa-catalog resolves, which is non-zero even where nothing has run — and
`smoke.sh:408-417` records in the script itself which fields that substitution does **not** cover:
`summary.passed`, `summary.failed`, `summary.not_run` and the case counters below `case_expected`.

### 9.2 It is not one panel

Reading `components/analytics/AnalyticsDashboard.tsx` rather than assuming: the page never binds
`summary.passed` / `summary.failed` directly (`grep -rn "summary\." src/components/analytics src/pages`
returns nothing for either). What it renders instead is **also** execution-scoped:

| Panel | Source | In this deployment |
|---|---|---|
| Test-case stat row | `summary.case_passed/case_failed/case_skipped/case_xfail/case_xpass`, `:869-890` | every count 0 |
| — its `total` | `max(case_expected, case_total, executed)`, `:880` | **non-zero** |
| — its `not_run` | `total - executed`, `:888` | equals the whole universe |
| Test-file stat row | bucketed from `lists.passed/failed/not_run`, `:894-902` | everything in `not_run` |
| Trend chart | `trend.points[].passed/failed/not_run`, `:861-863` | flat zero |
| Build distribution | `build_distribution[].passed/failed/executed_total`, `:824-836` | empty array |
| Heatmap, flaky, grouped, quality vectors | overview sub-objects | empty |

So the page renders **"Total N — Passed 0, Failed 0, Not run N"** on a working stack. That is exactly the
plausible-looking zero the Global Constraints call worse than a missing panel: it is not an empty state, it
is a *confident* claim that N tests exist and none of them passed.

### 9.3 Decision: **label unavailable** — one page-level banner, panels left in place

Not "show" and not "hide".

**Not show.** `Passed 0 / Failed 0 / Not run N` over a non-zero total is a false statement about the
product's quality, rendered in the one place a reader goes to ask about it. The distinction between
"nothing passed" and "nothing was measured" is the entire information content of the page, and the
rendering collapses it.

**Not hide.** Hiding the panels would delete six charts to work around one absent field, in a UI whose
governing property is that components are copied verbatim (§6.5: three deletions against ~18k LOC). It
also destroys the evidence: a deployment that later gains version observation would show nothing, with no
trace of why. And a hidden panel cannot say *why* it is hidden, so the next person re-derives §9.1.

**Label unavailable.** A single banner at the top of `pages/AnalyticsPage.tsx`. Task 11 implements
this verbatim, so both halves are given exactly.

**The condition** — evaluated only on a settled query (`!isLoading && !isError && overview`), so a
loading page shows nothing:

```ts
const s = overview.summary;
const executed = s.case_passed + s.case_failed + s.case_skipped + s.case_xfail + s.case_xpass;
const showNoExecutionData = s.case_expected > 0 && executed === 0;
```

**The copy** — heading, then body, with `{n}` substituted from `summary.case_expected`:

> **No execution data for this selection**
>
> The {n} tests below are the plan universe for this product and branch, and that count is accurate.
> Every outcome count is 0 because no execution rows matched the selected version — not because those
> tests failed or were skipped. Check the version filter; if no version ever matches, this deployment is
> not recording a product version on its runs.

**The copy states the observation, not the cause, and that is deliberate.** The condition above is
satisfied by more than one situation: the structural gap of §9.1, *and* a product that has simply never
been run, *and* a version picked from a dropdown with nothing behind it — the last two on a deployment
where version recording works perfectly. A banner that asserted "runs in this deployment do not record a
product version" would state a false cause in two of the three. So it reports what is true in all three
(nothing matched, and the universe count is still good) and hands the reader the diagnostic — *if no
version ever matches* — as a check to run rather than a conclusion. The panels stay, and stop being read
as claims.

Why this is the right shape of change and not a fourth exception to "components untouched":
- It is **additive** — one banner component and one line in one page — where hiding is subtractive
  across six panels.
- The condition is **computed from the response**, not from configuration, so nothing has to be
  un-done later: the banner clears itself as soon as any execution row matches the current selection.
  Note that this is *not* the same event as version recording starting to work — it is narrower, and
  the copy is written to be true under either reading.
- It **invents no value.** Every number on the page keeps coming from the gear. The banner adds the one
  fact the gear cannot put in a field: that zero here means unmatched, not failed.

Consequences to carry forward:
- §6.5's "the only place `src/components/` and `src/pages/` are modified" becomes three deletions, three
  reference strips **and this one addition**. Task 8 or Task 11 should amend that sentence rather than
  leave it approximate.
- The Analytics **version dropdown** is fed by `useObservedProductVersions` (row 65, `components/analytics/AnalyticsDashboard.tsx:604`), which is empty for
  the same reason. An empty dropdown over a required parameter means the page cannot be driven at all
  unless the UI offers a free-text version or a literal fallback. **Do not hardcode a fallback value** —
  Phase A's smoke script passes `version=unknown` as a probe, and a UI that shipped `unknown` as a
  default would be presenting a fabricated filter as a real one. This needs the same human decision as
  C1 and is recorded here rather than solved.
- `exportAnalytics`'s CSV summary block carries only `total/passed/failed/not_run` and their percentages
  (§4a) — i.e. **exactly the zero fields, and not `case_expected`**. A CSV export from this deployment is
  all zeros with no banner attached to it. Task 11 should either suppress the CSV branch or note it.

---

## 10. Confidence

Stated per the brief rather than presented uniformly.

**Verified against `/openapi.json`, the running gear, or Rust source I opened** — all field names, all
`required` and nullability claims, all path and parameter names, every quoted doc comment, every
`file:line` citation in this document, X1–X8, §5-A through §5-E, §6, §9.1 and §9.2.

**Inferred, and marked as such:** three claims rest on reading rather than execution and should be
re-checked by whoever implements them.
1. That `GET /qa/v1/runs?$filter=name eq '…'` resolves a run name (§7.1). `name` is in `RunFilterField`
   (`odata.rs:80`); the query itself was not run.
2. That `useRunsByPlan` can be served by client-side filtering a page of runs (row 8). No `target.*`
   field is filterable, which is verified; that a *single page* suffices for the Plan Detail page is not.
3. That `TestResult.cases[]` is recoverable from `GET /qa/v1/test-case-results?$filter=run_id eq …`
   (§8-C2). The field set matches; the round trip was not driven (that collection is empty in the
   current stack — `{"items":[],…}`). **Partly closed in Task 10** — the filter is now driven and the
   uuid must be *unquoted* (§11.1); the collection is still empty, so no row has round-tripped.
4. *(Withdrawn on review — it was never inferred, only unread.)* `ProductCoverageCard`'s empty-state
   body **is** checkable and is an honest empty state: `if (!coveragePoints.length)` at `:36` returns a
   card built from `UnavailableNotice` titled *"Code coverage is not available in this deployment"*
   (`:44-49`). §8-C8 is right to rely on it. Three inferred claims remain, not four.
   *(Copy re-checked in Task 10: it is Task 8a's `UnavailableNotice` — the sentence quoted here before,
   "No product-version coverage reported yet.", is the pre-8a wording and no longer exists. Line numbers
   are against the file as Task 10 leaves it, which is byte-identical to Task 8a's.)*

**Least confident rows**, in order: **18/19/20** (schedules — the `exclusive` tri-state, the
comma-string→array change and the read-modify-write PUT compound, and the notification split means a
create silently drops three form fields); **83/85** (variables — the whole-set→per-item inversion is the
only place a legacy atomic write becomes several; the `platform_id`-is-additive semantics were read
from the endpoint description here and have since been **driven** in §11.2, which confirms them); **5** (the runs pager — X2 is verified, but whether the Runs
page can live without `total` is a component question this document only raises).

**One thing this document does not do:** it does not compare the two sides' *error* bodies beyond
`client.ts`'s `ApiError` contract, which §6.1 already owns. The gears answer a canonical envelope
(`{type, title, status, detail, instance, trace_id, context.field_violations[]}` — observed on the 400
from `$filter=status eq 'FAILED'`), and §6.1's "learns to unwrap the gears' canonical error envelope into
`message`" is the right place for that, not here.

---

## 11. Findings from Task 10's implementation (driven, session 4)

Added while remapping `hooks.ts`. Everything here was **driven against the running stack**,
not read; each entry says what was run. Per the Global Constraints none of it proposes
widening a gear.

### 11.1 A uuid in an OData `$filter` must be **unquoted** — a quoted one is a 400

New, and it changes how three filters have to be written. The gears' OData layer is typed:

```
GET /qa/v1/test-case-results?$filter=run_id eq '1a9adedc-83b2-4a63-b353-8cd4b3eb1c9e'
  -> 400 {"detail":"Request validation failed",
          "context":{"field_violations":[{"field":"$filter",
            "description":"invalid $filter: Type mismatch for field run_id: expected Uuid, got string"}]}}
GET /qa/v1/test-case-results?$filter=run_id eq 1a9adedc-83b2-4a63-b353-8cd4b3eb1c9e
  -> 200 {"items":[],"page_info":{...}}
```

Same on `/qa/v1/test-results` (quoted → 400, unquoted → 200 with rows). A *string* field is
the opposite: `$filter=name eq 'collect-…-2'` on `/qa/v1/runs` answers exactly that run,
and an unquoted string would not parse. §10's inferred claim 3 (that `TestCase.cases[]` is
recoverable from `?$filter=run_id eq …`) is now **driven as far as it can be** — the filter
is accepted and correctly typed; the collection is still empty on this stack, so no row has
been round-tripped through it. `adapters.ts`'s `odataLiteral(value, 'uuid' | 'string')`
exists for this and carries the transcript.

### 11.2 `GET /qa/v1/variables?environment_id` **is** honoured, and **is** additive

Row 84 was right on both counts, and §7.6's client-side partition is **not** dead work —
it is needed *on top of* the server filter, not instead of it. Driven with three seeded
rows (one global, one on each of two platforms):

```
GET /qa/v1/variables                       -> [T10_GLOBAL]
GET /qa/v1/variables?environment_id=<p1>      -> [T10_GLOBAL, T10_P1]
GET /qa/v1/variables?environment_id=<p2>      -> [T10_GLOBAL, T10_P2]
GET /qa/v1/variables?environment_id=<unknown uuid> -> 404 "Environment … was not found"
GET /qa/v1/variables?environment_id=not-a-uuid     -> 400 (UUID parsing failed)
GET /qa/v1/variables?environment_id=              -> 400 (invalid length: found 0)
```

This transcript predates qa-environments' Environment rename: the parameter was
`platform_id` at the time it was driven, and the 404 body read "Platform … was not found"
(`PlatformResourceError::not_found`, `api/rest/error.rs`) rather than the "Environment …"
text shown above. Both the parameter and the message wording changed with the rename;
neither changed the behaviour being recorded, so the transcript is updated to read as a
rerun would today rather than left to quote a string the gear no longer emits.

The 404 and the 400 are what distinguish this from X8: the parameter is **read**, not
silently ignored, so this is not a C7-shaped trap. But its semantics are additive exactly
as its own doc says, and legacy's `/platforms/{name}/variables` returned the platform's
**own** set — so `usePlatformVariables` passes `environment_id` *and* partitions on
`environment_id !== null`. **No new gap.**

### 11.3 `GET /qa/v1/jira/open-bugs` — document and X6 are both right

`/openapi.json` marks neither `repo_id` nor `plan_path` `required`, and X6 says the pair is
required together. Driven, both are true: neither is required *individually*, and the pair
is a **co-requirement**.

```
GET /qa/v1/jira/open-bugs                          -> 200 []
GET /qa/v1/jira/open-bugs?repo_id=<uuid>           -> 400 field_violations[0] = {field:"plan_path",
      description:"repo_id and plan_path must be supplied together, or not at all"}
GET /qa/v1/jira/open-bugs?plan_path=x              -> 400 (same violation)
GET /qa/v1/jira/open-bugs?repo_id=<uuid>&plan_path=x -> 200 []
```

So the hook's "omit for all bugs" branch omits **both**, and a plan id that does not decode
into a pair sends neither rather than half of one.

### 11.4 **New finding:** `RunQueueEntry.target_id` is rendered, and has no gear source

Row 14 records `target_id -> absent` but not that it is **what the queued row is labelled
with**: `components/runs/QueuedRunsCard.tsx:144` draws it as the row's primary cell, and
both confirm dialogs quote it (`:66` *"Cancel queued run \"{target_id}\"?"*, `:87` *"Force
start \"{target_id}\" on …?"*). `QueueEntryDto` carries no target of any kind — no
`plan_id`, no `target{}`, nothing but `run_id` and `run_kind`.

Leaving it `''` renders an unlabelled row and a confirm reading `Cancel queued run ""?`.
The adapter therefore **substitutes `run_id`**, in the sense of §7.10: a real identifier for
the same row, nothing fabricated, but it **changes what the column means** — a run uuid
where legacy drew a plan id. Recorded here rather than done quietly. A human may prefer the
blank; the substitution is one line in `queueEntryFromDto`.

### 11.5 Corrections to facts this document and its briefs carried

- **`isActiveRun` has five call sites, not four.** §7.8 and Task 9's hand-off named
  `pages/RunsPage.tsx:145,166,279` and `components/runs/RunsTable.tsx:165`.
  `pages/RunDetailPage.tsx:62` (`const activeRun = isActiveRun(run)`) is a fifth.
- **`getPhaseColor` confirmed dead and deleted.** `grep -rn "getPhaseColor" src` finds its
  own declaration and one file-header comment, nothing else. §7.8 told Task 10 to teach it
  the lowercase set; teaching a function with no callers is dead work, so it was removed
  and only `isActiveRun` was re-mapped. Third independent confirmation.
- **`/qa/v1/runs` clamps `limit` at 500 too**, not only the insights collections X2 names:
  `?limit=501` and `?limit=10000` both answer `page_info.limit` 500 (8 rows on this stack).
  `useRuns(…, page, perPage)` therefore walks cursors to satisfy a `perPage` above 500.
- **`POST /qa/v1/runs` answers `200 RunDto` for a launch that started**, and `client.ts`
  returns a parsed body rather than a `Response`, so 200 and 202 are indistinguishable by
  the time a hook sees them. `launchResponseFromDto` narrows on `'queue_id' in body`
  instead — which is what `isQueued` already did, so the discriminator survives.
- **`useCancelQueuedRun`'s return envelope becomes `void`.** Row 15 records
  `{id, state} -> no body`; no consumer read the old body (`QueuedRunsCard`'s `onSuccess`
  toast reads the entry it already had).

### 11.6 Two decisions Task 10 took that are a human's to confirm

Neither invents a value; both change what a control *means*, so both are surfaced rather
than buried.

1. **`useTests` is degraded, not removed.** §8-C1 asks a human to choose between degrading
   `/tests` to a path list and removing the surface the way §6.5 removed three settings
   pages. **That question is still open.** Task 10 implemented the degradation —
   `PlanDto.test_files` supplies the paths and the plan/repository/product above them; the
   rendered `title`, `component`, `description`, `tags`, `quality_vectors`, `versions` and
   `loc` are left absent, and `tags` is `[]` rather than borrowing the *plan's* tags —
   because the alternative while the question is open is four routed pages 404ing
   (`TestCatalogPage`, `TestDetailPage`, `CustomPlanEditorPage`, `CreateScheduleDialog`).
2. **A platform with `product_id: null` is included in every product's platform list.**
   §7.6 flags this as the decision the client-side filter forces and does not make it. A
   null product means the platform belongs to *no* product rather than to another one, so
   including it leaks nothing — and excluding it empties every platform picker on this
   deployment, where `product_id` is null on all three platforms, turning "run a plan" into
   a control with no options and no error. That is a judgment call, not a fact the gear
   states.

### 11.7 X7's remaining edge: two form fields whose **label** now lies

Task 10 could refuse two of X7's five renames and could not refuse the other three, and the
difference is whether the pasted secret is distinguishable from a reference. **One of the two
refusals has since been replaced by the capability it was standing in for** — see the entry
under the table.

| Form field | Gear field | Behaviour |
|---|---|---|
| Test repo `token` | `credential_ref` | **Refused** — `repoReqFromForm` throws with a message naming the alternative. A pasted token and a reference are both opaque, but the form has a *separate* `ssh_key_id` field that is already a reference, so refusing steers the user to it rather than dead-ending. |
| Platform `kubeconfig` | `kubeconfig` **or** `kubeconfig_credstore_ref` | **Forwarded** since 2026-08-27, and the document really is stored — see below. Was "Refused". The response returns neither field. |
| JIRA `api_token` | `api_token_credstore_ref` | **Forwarded.** No signal to refuse on — both are opaque single-line strings. |
| Notifications `slack_webhook_url` | `slack_webhook_credstore_ref` | **Forwarded.** Same. |
| SSH key `private_key` | `private_key_pem` | **Forwarded, and correctly so** — this one is *not* a credstore reference and its label does not lie. The rename is PEM-specific and the field really does take key material. |

#### The kubeconfig row, and why it changed twice

**First change (found via a production 500, not review).** The row originally read: *"a pasted
kubeconfig is always several lines and a reference always one, so the test cannot
false-positive."* That claim is true about false-positives and false about
false-**negatives**, and the false-negative rate on the only path that reaches the guard
turned out to be 100%. `CreatePlatformDialog.tsx` base64-encodes the pasted kubeconfig
(`btoa(kubeconfig.trim())`) *before* calling `createPlatformReqFromForm` — a fact the original
reasoning missed entirely. Base64 output contains no newlines by construction, so every real
submission handed the multi-line test a single-line string and it passed every time. A
~4000-character encoded blob then reached `qa_environments.kubeconfig_credstore_ref`
(`varchar(1024)`) and the gear answered `500`
(`error=Database("... value too long for type character varying(1024)")`, `content_length=4097`)
instead of the guard's own client-side refusal. The fix at that point was a stricter refusal.

**Second change (2026-08-27): the refusal is gone, because the capability exists now.** A
refusal fixed the symptom and removed the feature; pasting a kubeconfig is what the form is
for. Two things changed together:

1. **The gear takes the document.** `qa-environments` gained the `credstore` gear dependency
   `qa-catalog` already had (`qa-environments/src/gear.rs`, `deps = [authz_resolver, credstore]`),
   and `EnvironmentsService` now accepts a raw `kubeconfig` on create and update. The document goes
   to credstore **first**, under a generated `qa-environments-kubeconfig-<uuid>` reference, and
   only that reference reaches the database — the ordering and the compensating cleanup are
   `create_ssh_key`'s (`qa-catalog/src/domain/service/ssh_keys.rs`). A failed credstore write
   creates no row; a failed row write deletes the secret it had just written; replacing a pasted
   kubeconfig removes the superseded secret, and so does deleting the environment — but **only** for
   references this gear minted, because a caller-supplied one may be shared with other systems.
2. **The adapter decodes rather than refuses.** `createPlatformReqFromForm` undoes the dialog's
   `btoa` and sends the YAML as `kubeconfig`. It sends the *decoded* text, never the base64:
   storing an encoded blob under a field named `kubeconfig` would be a latent trap for whatever
   reads it later. `CreatePlatformDialog.tsx` is unchanged — it is copied verbatim, and the
   decode lives in the adapter for exactly that reason.

A single-line value still goes through as `kubeconfig_credstore_ref`, so a caller who already
holds a reference is unaffected; because the dialog base64-encodes that too, the decode is what
makes the reference path reachable from the UI at all. What is refused now is only the
genuinely unusable: empty input on **create**, and any value that is neither a plausible YAML
document nor a single-line reference within `varchar(1024)` under either reading — encoded or as
typed. Tests are in `src/api/adapters.test.ts`
(`describe('createPlatformReqFromForm', ...)` and `describe('updatePlatformReqFromForm', ...)`).

**Amended after review round 1 (fix round 1, 2026-08-27): the decode is a candidate, not an
assumption.** The first version did `typed = decoded ?? raw` and classified the *decoded* bytes
unconditionally. Credstore's charset is `[a-zA-Z0-9_-]`, which is almost a subset of base64's, so
any reference of a length divisible by four was decoded to garbage and either sent as that garbage
or thrown. Measured: `teamaprodcluster` was sent as `"µæ¦jèuÉn²×«"` with no error;
`0123456789abcdef0123456789abcdef`, a bare `Uuid::simple()` reference, likewise; and
`platformProd0001`, `prodcluster12345` and `secretRef00001AA` were *thrown* as "decodes to binary".
The decode is now used only if it classifies as a document or as printable-ASCII reference text;
otherwise the value **as typed** is classified. The doc comment that justified the old code claimed
*"every credstore reference in this system contains a character outside that alphabet"* — false, and
corrected at its site.

One case stays undecidable and is named rather than hidden: a short single-line base64 string whose
decode is binary is indistinguishable from a reference spelled in base64's alphabet. It is forwarded
as a reference, because rejecting a real reference outright is the worse failure — a wrong reference
is a visibly broken platform, a rejection is a capability the caller cannot use. A base64 blob too
long to be a reference still throws, which is the shape that produced the original 500. A blank
`kubeconfig` on a **PATCH** now means "unchanged" rather than an error, since an edit form binds a
textarea to a string and an untouched field arrives as `''`.

**The document is secret and is treated as such.** A kubeconfig's `client-key-data` is a client
private key. It is wrapped in `qa_environments_sdk::KubeconfigMaterial`, whose hand-written
`Debug` prints `[REDACTED]` (so every enclosing type's derived `Debug` is redacted too); the REST
request DTOs redact it the way `qa-catalog`'s `CreateSshKeyReq` does; every `#[instrument]` on the
paths that carry it skips the argument; and `EnvironmentDto` carries no `kubeconfig` field at all.
`domain::service::environments_kubeconfig_tests` asserts the absence against the **real** emitted
`tracing` output, not a stand-in string.

**And, by a human decision taken after review round 1, `EnvironmentDto` no longer returns
`kubeconfig_credstore_ref` either.** Under `SharingMode::Tenant` the reference *is* a read path:
any tenant member holding it can fetch the document through credstore's own
`GET /credstore/v1/secrets/{ref}`. `EnvironmentDto` is returned on every environment read and write path,
so publishing the reference handed every `qa.platform` GET/LIST-authorized caller a working read
path to a pasted private key. That was tolerable while the column held an operator-configured
placeholder and is not now. This is exactly `SshKeyDto`'s convention, which has always withheld its
own `credstore_ref` for the same reason. The reference stays on the SDK model
(`qa_environments_sdk::Environment`) and on the column — `qa-runs` reads it in-process when it
builds a dispatch spec — so only the REST projection changed. **No UI consumer read it**: the
legacy `PlatformInfo` never carried it (`platformFromDto` drops it), and `tsc` passes.

The two remaining forwarded rows are **not a leak**: a pasted secret is stored as a reference that
resolves to nothing, so the integration fails rather than the secret being served back. The
remaining gap is the **label** — a field captioned "API token" or "Slack webhook URL" that in fact
stores a credstore reference — which is copy in `SettingsJiraPage` and the notifications
pages, not something an adapter can fix. Recorded here as an X7 follow-up for Task 11 or a
human; it is a usability-and-honesty issue, not a confidentiality one (contrast §8-C10,
which was).

**One thing this did not fix, stated so it is not mistaken for fixed.** A caller-supplied
`kubeconfig_credstore_ref` is still stored with no shape validation beyond "not empty", and
`credstore_sdk::SecretRef::new` accepts only `[a-zA-Z0-9_-]{1,255}`. So the `credstore://…`
spelling used by this document's own examples and by several test fixtures is **not a
resolvable reference** — nothing can read it back out of credstore. That predates this change
(the gear held the column without ever resolving it) and nothing here made it worse, but a
consumer that starts resolving these references will meet it.

### 11.8 The Runs page now works over the newest 1,000 runs, not all of them

Found in review round 1, and it is a **behaviour change a user can see**, so it is here
rather than only in a code comment.

`RunsPage.tsx:61-62` fetches every run in one request and filters and pages **client-side**:
`useRuns(autoRefresh ? 5000 : undefined, 1, 10000)`. No gear will serve that page:
`/qa/v1/runs` clamps `limit` at 500 whatever is asked (driven twice — `?limit=500`,
`?limit=501` and `?limit=10000` all answer `page_info.limit` 500). Reproducing legacy
literally means walking cursors, which on a deployment with 10,000 runs is **20 sequential
requests every 5 seconds** — and 20 sequential round-trips cannot finish inside the interval
that triggered them.

So `hooks.ts`' `MAX_RUNS_WINDOW` bounds the walk at two pages. The cost is now constant —
two `GET /runs` plus one `GET /environments` per poll, whatever the deployment holds — and the
consequence is that the Runs page's search, filter and pager operate over the newest 1,000
runs. An older run cannot be found there.

**Why this cannot be fixed in `hooks.ts`.** Client-side filtering over a truncated window is
only correct when the window is the whole collection. Making the page complete needs a
*server-side* filter for whatever the user typed, and `RunFilterField` is
`id, name, state, run_kind, platform_id, source, schedule_id, resolved_exclusive,
is_validation, created_at, started_at, finished_at` — no plan, no product, no test file
(§8-C7). So the Runs page's search box cannot become a server query without new gear
capability. Raising the ceiling is a one-constant change and trades the same request budget
back; the honest fix is a component that pages against the cursor instead of filtering a
window, which is the same §7.3 conversation as the missing `total`.

### 11.8b Collect runs are hidden from the Runs page, as legacy hides them — but in the UI, not the API

Found 2026-09-03, while asking why a legacy instance shows no collect runs at all. It is a
**behaviour parity gap this table's row 5 did not name**: that row records the field rename
`run_source -> source` and not the *filter* legacy applies to that column.

Legacy creates a run row for every collect workflow — the collect submission goes through
`submit_workflow`, which calls `persist_submitted_run` -> `upsert_run_metadata` ->
`INSERT INTO run_results` (`manager/src/services/argo.rs:403-410, 697`) with
`run_source = "collect"` — and then keeps every one of them out of sight. The single read
path behind every run listing and every plan's recent-runs view drops them
(`manager/src/services/run_history.rs:382-386`):

```rust
// Collect-only workflows aren't real test runs — keep them out of every
// run listing / plan recent-runs view.
.filter(|run| run.run_source != "collect")
```

The poller that produces them is **on by default** there — `env_bool("COLLECT_POLLER_ENABLED",
true)`, `COLLECT_POLLER_INTERVAL_SECONDS` defaulting to 3600 (`manager/src/main.rs:273-278`),
and nothing in legacy's own charts or environment sets either — so a stock legacy install has
been collecting hourly and showing nothing, which is why an operator there has never seen one.

This port had no equivalent filter: `GET /qa/v1/runs` lists collect runs like any other, and
`RunsPage.tsx` queried it unfiltered, so qa-insights' hourly cycle contributed 24 rows a day —
for **one** repository — to the table an operator opens to see test runs.

**Closed in the UI, deliberately not in the gear.** `RunsPage`'s `listedRuns` excludes
`isCollectRun` rows, and a `Collect <n>` chip beside the status chips carries their count and
includes them in one click; `kind` is also a filterable FQL field now (`kind:collect`,
`-kind:collect`). The gear's listing is untouched: an endpoint that silently omits rows is a
trap for every other consumer — legacy has exactly that trap, and a client that asked for
`$filter=run_kind eq 'collect'` and got nothing would have no way to tell why — whereas "not
a real test run" is a presentation judgement, which is where legacy's decision effectively
lives too. A collect run's own page still opens by URL with its log intact.

Pinned by `RunsPage.test.ts`' three collect cases: hidden by default, listed with the chip on,
and no chip rendered at all when a deployment launches none.

### 11.9 Coverage's `collected_at` is rendered as a date, and there is none

A §8-C8 consequence, sharpened in review round 1. `CoverageBuildDto` has no `collected_at`,
and `ProductCoverageCard` both **sorts** on it (`:18`) and **renders** it as a date in a
column headed `Collected` (`:114`). The adapter leaves it `''` rather than back-filling
`Date.now()`, so on any deployment that ever measures a coverage point that column reads
**"Invalid Date"** on every row and the sort comparator returns `NaN`.

It is latent today only because the collection is empty everywhere (§8-C8) — a deployment
fact, not a code guarantee. Fixing it needs a component edit (drop the column, or label it
unavailable), which is the same class of change as §8-C1's blank metadata columns and is
recorded here for the same human.

The *other* half of the same finding **was** fixed rather than recorded: the coverage route
answers one point per product for the whole deployment, so `useProductCoverage` now filters
on `product_key` (§7.6) against the product it was asked for. Without that, one product's
card charted every product's rows under a caption reading *"Coverage belongs to this
product…"*. `useProductCoverage` therefore **keeps** its `productId` parameter — no longer
as a server filter, which the route does not offer, but as the key for the client-side one.
That retires the signature change this task briefly carried, and with it the task's only
component edit: **Task 10 modifies nothing under `src/components/` or `src/pages/`.**

### 11.10 One value in the UI that no gear supplied: `useTestNotification`'s event

Recorded here because it is the single invented value left in this task's diff and it
belongs on a human's list, not only in a hook comment.

`POST /qa/v1/settings/notifications/test` requires a body — `NotificationTestReq {config*,
event*}` — where legacy's `POST /settings/notifications/test` took **none** (row 88). The
config is real: it is re-read from `GET /qa/v1/settings/notifications`. The `event` is not.
Something has to be sent, the hook's signature takes no argument, and there is no "any
event" token, so `hooks.ts` sends **`succeeded`** — chosen because a test message describing
a success is the least alarming thing to deliver to a real Slack channel.

It has **zero consumers** today (`grep -rn useTestNotification src --include=*.tsx` → none),
so nothing sends it. `useTestScheduledRunNotification` is the variant that lets the caller
choose, and it is the one the notifications pages use. If the bodyless hook ever gains a
consumer, the event should come from the UI rather than from this default.

## 12. SSE authorization — how the log stream carries a bearer token

Task 16's decision, written before it was implemented and corrected afterwards where the
measurements disagreed with the plan. This is the one place in the UI where a credential
travels in a URL, so the reasoning and the cost both belong on a human's list.

### 12.1 The constraint, established from source rather than assumed

`EventSource` cannot send request headers — its constructor takes no `headers` option — so
the bearer token that every other request carries through `authHeaders()`
(`qa-platform-ui/src/api/client.ts`) cannot ride `GET /qa/v1/runs/{id}/logs`.

**The gears accept neither a cookie nor a query parameter, and that is structural, not a
configuration gap.** `security_context_middleware`
(`libs/toolkit-http-middleware/src/auth.rs:112`) calls
`extract_bearer_http(request.headers())`, and `extract_bearer_http`
(`libs/toolkit-http-middleware/src/security.rs:47`) has the signature
`fn(&HeaderMap) -> Result<SecretString, SecurityContextHttpError>`. It is handed the header
map and nothing else, so it **cannot see the request URI at all**: authenticating by query
parameter is not unsupported, it is unreachable. There is no cookie branch either — the
error enum (`security.rs:24-35`) has exactly three variants, all about the `Authorization`
header: `MissingAuthHeader`, `InvalidAuthHeader`, `EmptyToken`.

### 12.2 The decision: nginx translates a query parameter into the header, for this route only

The task brief framed the choice as *query parameter or cookie, else BLOCKED — the
remaining options being a gateway change or an unauthenticated log viewer*. **That binary is
incomplete**, because it treats the gear as the only place a token can be read. nginx is in
the browser's path to this route, and `qa-platform-ui/nginx.conf` already carried a location
scoped to exactly this one URI (Task 11 created it for `proxy_buffering off`). What ships is:

```
map $arg_access_token $sse_authorization {
    ''      $http_authorization;
    default "Bearer $arg_access_token";
}
```

referenced by `proxy_set_header Authorization $sse_authorization;` inside the SSE location,
and `useRunLogStream.ts` appending `?access_token=<token>` to the URL it opens. No gear
change and no gateway change, so nothing here is out of scope by the Global Constraints, and
the backend stays feature-frozen.

`access_token` is RFC 6750 §2.3's name for a bearer token in a URI query.

### 12.3 Why a `map` rather than a bare `proxy_set_header` — a defect avoided, not a preference

`proxy_set_header Authorization "Bearer $arg_access_token";` **replaces** any inbound
`Authorization`, and the pre-flight reasoning that this is harmless ("`EventSource` sends
none") is **wrong**, because `EventSource` is not the only client of this URL.
`LogViewer.tsx:173,180` mounts *two* hooks side by side: `useRunLogStream` (the stream) and
`useRunLogs` (`src/api/hooks.ts:722-732`), which polls **the same URL** every 5s with a plain
`apiGet` that does carry a real header. That is also the mechanism behind the interleaved
`401 problem+json` / `200 text/event-stream` at a 5s cadence Task 15 observed and offered as
an unverified hypothesis: it is confirmed — one code path was authorized and the other was
not.

Measured on the live stack before this block existed:

```
$ curl -s -o /dev/null -w '%{http_code}\n' -H "Authorization: Bearer <real>" \
       'http://localhost:8080/qa/v1/runs/<id>/logs'
200        # Content-Type: text/event-stream
```

The unconditional form would have overwritten that header with the literal `Bearer ` and
turned the one path that already worked into a 401. The map uses the query parameter only
when there is one and otherwise passes the inbound header through untouched.

**Precedence, so it is written down:** when both are present the query parameter wins, in **both**
directions, and both were measured on this route:

| inbound header | `?access_token=` | result |
| --- | --- | --- |
| `Bearer <real>` | *(absent)* | 200 |
| `Bearer <real>` | `garbage` | **401** — the parameter downgrades a valid header request |
| `Bearer garbage` | *(absent)* | 401 |
| `Bearer garbage` | `<real>` | **200** — the parameter upgrades an invalid header request |

An earlier draft of this section said the parameter *"can only deny, never elevate"*. **That is false
as written and the last row refutes it in one command** — it is recorded rather than quietly deleted,
because the sentence was a security claim and this document is where such a claim would be trusted.

The accurate statement is narrower: the query parameter is **authoritative** on this route, and it
**cannot grant privilege beyond whatever the token it carries already grants**. Nothing is bypassed by
supplying it — nginx only decides *where the gear looks for a credential*, and the gear then validates
that credential in full (probe 2's `AUTHN_FAILED`, `auth.rs:118`, is the proof that an unacceptable
parameter is rejected rather than waved through). So the 200 in the last row is exactly the access the
real token was always entitled to; what it is not is a promise that the parameter can only ever make an
outcome worse.

### 12.4 The rejected alternatives

- **Cookie set at login** — rejected on a fact, not a preference: the toolkit has no cookie
  branch (§12.1). Adopting it would require a gear change, plus `SameSite`/`Secure`
  decisions and a CSRF story for every non-GET that would then also be cookie-authenticated.
- **A bare query parameter read by the gear** — unreachable for the same reason.
- **Shipping the log viewer unauthenticated** — never on the table once a third option
  existed.
- **BLOCKED** — declined. The brief's dichotomy was incomplete and its own Files line
  (`nginx.conf`) already pointed at the option it omitted.

### 12.5 The costs, honestly

- **The token lands in the nginx access log**, verbatim, and therefore in
  `docker compose logs ui`. Measured:
  `"GET /qa/v1/runs/<id>/logs?access_token=<token> HTTP/1.1" 401` appears in full.
- **Nothing here can shorten the token's lifetime.** Mitigating a URL-borne credential by
  making it short-lived needs a token-exchange endpoint this deployment does not have. The
  realm's `accessTokenLifespan` is 300s, which is the only bound.
- **Correction to the brief's third cost.** It also names *browser history*. That is
  overstated for this design: `EventSource` issues a **subresource** request, not a
  navigation, so the URL does not become a browsing-history entry the way a clicked or typed
  URL does, and the UI never renders it — `LogViewer`'s "download" builds a `Blob` object URL
  from text it already holds (`LogViewer.tsx:241-245`), not a link to the stream. It remains
  visible in devtools and in whatever the browser keeps internally. The access-log exposure
  above is the real one.
- **What bounds the blast radius:** the translation exists in one regex location matching
  `^/qa/v1/runs/[^/]+/logs$` and nowhere else. The prefix `location /qa/v1/` sets no such
  header, so no other API path becomes query-authenticatable — measured: the same real token
  as `?access_token=` on `/qa/v1/products`, `/qa/v1/environments` and `/qa/v1/runs` all answer
  **401**, while the same token in a header on `/qa/v1/products` answers 200. A leaked URL
  is one read-only log stream, not the API.
- **The standing hazard this creates:** because `proxy_set_header Authorization` overwrites,
  this header must **never** be added to a location the SPA calls with a real
  `Authorization`. The `$http_authorization` fallback protects the one URL it covers, not any
  other.

### 12.6 How it fails closed — measured, all through nginx on `:8080`

| request | result |
| --- | --- |
| no token, no parameter | **401** `MISSING_BEARER` — nginx omits an empty `proxy_set_header`, so the gear sees no header (`auth.rs:123`) |
| `?access_token=` (empty) | **401** `MISSING_BEARER` — the map's `''` branch, same as above |
| `?access_token=not-a-real-token` | **401** `AUTHN_FAILED` — extracted and *rejected* (`auth.rs:118`); the parameter is validated, not merely accepted |
| `?access_token=<real>` | **200** `text/event-stream` |
| `Authorization: Bearer <real>`, no parameter | **200** `text/event-stream` — the polled fallback, unbroken |

### 12.7 The one thing this deployment cannot demonstrate: an actual log line

**No run on this compose stack ever emits a log line**, and that is a property of the mock
executor, not of this change. Log lines reach the SSE bus only through
`IngestService::fan_out_log` (`qa-runs/src/domain/service/ingest.rs:732`), which fires on an
`ExecutionEvent::Log`; `MockRunExecutor`'s production `default_script`
(`qa-runs/src/infra/executor/mock.rs:123-157`) emits `Started`, one `TestResult` and
`Finished`, and **no `Log` event at all** — the only `ExecutionEvent::Log` in that file is
inside `#[cfg(test)]`. There is also no HTTP ingest route to inject one (`/openapi.json` has
five `/qa/v1/runs*` paths, none of them a log POST).

So "confirm lines stream in" was verified with the strongest thing this deployment can
produce: a **queued** run's stream held open through nginx, authorized by the query parameter
alone with no `Authorization` header, delivered the gear's SSE keep-alive comment live —

```
10:32:52.042 | HTTP/1.1 200 OK ... Content-Type: text/event-stream
10:33:22.047 | :
```

— 30.005s after the headers, matching `KEEP_ALIVE_INTERVAL = 30s` (`qa-runs/src/api/rest/sse.rs:94`).
That proves the frame path end to end: authorized, open, and unbatched through
`proxy_buffering off`. It does not prove a *log line* renders, and on this deployment nothing
can, which is the same "runs are simulated" honesty Phase D is required to keep.
