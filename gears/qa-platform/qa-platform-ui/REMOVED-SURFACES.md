# Removed surfaces

A record of UI surfaces removed because the qa-platform gears serve no backend for them. Each
section below is a re-add checklist: what was removed, the exact legacy endpoint it needed, the
files touched, and the commit that removed it. Written before the removal, so it is a record and
not a reconstruction.

This file is created by Task 8 and appended to by Task 8a (the ten surfaces the gears do not
serve, per `gears/qa-platform/docs/CONTRACT-DIFF.md` §8). Each task's sections are grouped under
its own heading below.

---

## Task 8: the three settings surfaces with no backend at all

### Repository Poller settings

- **What was removed:** the whole "Repository Poller" settings page — branch-cache poll interval
  and enable/disable toggle.
- **Legacy endpoints it needed:** `GET /settings/repo-poller`, `PUT /settings/repo-poller`.
- **Files touched:**
  - Deleted `src/pages/settings/SettingsRepoPollerPage.tsx`.
  - `src/components/layout/Sidebar.tsx` — removed the "Repository Poller" nav entry.
  - `src/App.tsx` — removed the `/settings/repo-poller` route and its import.
  - `src/api/hooks.ts` — removed `useRepoPollerConfig`, `useUpdateRepoPollerConfig`, the
    `repoPollerConfig` query key, and the `RepoPollerConfig` type import.
  - `src/api/types.ts` — removed the `RepoPollerConfig` interface.
- **Commit:** a8a7b90646ea1f94c975f045bae0a16740d61fb2

### ReportPortal (settings page, and every deep link fed from it)

- **What was removed — most-wanted-back item first:** the **Dashboard "Recent Failures" per-row
  ReportPortal deep link** (`src/components/dashboard/RecentFailuresCard.tsx`, fed by
  `src/pages/DashboardPage.tsx`) — this is the one a human is most likely to ask to restore, since
  it's the only ReportPortal affordance that was reachable from the page most people land on
  first. Also removed: the "ReportPortal Integration" settings page itself (URL, project, API key
  secret name, enable toggle); the "ReportPortal" link button and the per-row link on the run
  detail page; the per-row "Open in ReportPortal" icon link on the test detail page's Recent Runs
  table; and the per-row link inside `TestResultsTable`.
- **Legacy endpoints it needed:** `GET /settings/reportportal`, `PUT /settings/reportportal`. To
  restore any of the above, both must exist again — the deep links all depend on the `url` field
  the `GET` returns.
- **Done in two passes** (see commits below):
  - **Pass 1** removed the settings page and its mutation (`useUpdateReportPortalConfig`) and the
    two affordances then known to Step 3 (`RunDetailPage.tsx`, `TestDetailPage.tsx`), but
    **kept** the `useReportPortalConfig` query hook and `ReportPortalConfig` type alive. Reason:
    a grep of every identifier before deletion (required by Step 4) found `useReportPortalConfig`
    still imported and called by `src/pages/DashboardPage.tsx` — a file outside that task's
    permitted edit scope — which fed `reportPortalUrl` into `RecentFailuresCard` and, separately,
    the still-passed prop into `TestResultsTable`. Deleting the hook then would have broken those
    files' compile. This contradicted the task brief's own claim that removal was "safe by
    construction," which the brief's author confirmed was written without grepping.
  - **Pass 2**, ruled by the controller after reviewing that finding, authorized touching the
    three additional files and removed the rest: the `RecentFailuresCard` prop/link, the
    `DashboardPage` hook call and prop pass-through, the (by-then dead) `TestResultsTable` prop
    and link, and finally `useReportPortalConfig` / `ReportPortalConfig` themselves. Both passes'
    affordances were guarded (`reportPortalUrl && failure.launch_id` / `result.launch_id &&
    reportPortalUrl`) the whole time, so at no point did any of them render a false value — the
    query simply had no backend to resolve against, in either pass.
- **Files touched:**
  - Deleted `src/pages/settings/SettingsReportPortalPage.tsx`.
  - `src/components/layout/Sidebar.tsx` — removed the "ReportPortal" nav entry.
  - `src/App.tsx` — removed the `/settings/reportportal` route and its import.
  - `src/pages/RunDetailPage.tsx` — removed the "ReportPortal" link button and the
    `reportPortalUrl` prop passed to `TestResultsTable` (2 affordances).
  - `src/pages/TestDetailPage.tsx` — removed the per-row "Open in ReportPortal" icon link on the
    Recent Runs table (1 affordance).
  - `src/components/dashboard/RecentFailuresCard.tsx` — removed the `reportPortalUrl` prop, the
    `launchUrl` construction, and the per-row "Open in ReportPortal" icon link.
  - `src/pages/DashboardPage.tsx` — removed the `useReportPortalConfig()` call and the
    `reportPortalUrl` prop passed to `RecentFailuresCard`.
  - `src/components/runs/TestResultsTable.tsx` — removed the `reportPortalUrl` prop and the
    per-row "Open in ReportPortal" icon link (this one was already dead by Pass 1, since
    `RunDetailPage.tsx` no longer passed the prop; Pass 2 deleted the dead code).
  - `src/api/hooks.ts` — Pass 1 removed `useUpdateReportPortalConfig`; Pass 2 removed
    `useReportPortalConfig`, the `reportPortalConfig` query key, and the `ReportPortalConfig`
    type import.
  - `src/api/types.ts` — Pass 2 removed the `ReportPortalConfig` interface.
- **Left alone on purpose — pointer to Task 9:** the dead DTO *data fields* `reportportal_url`
  (`src/api/types.ts:91,263`) and `launch_id` (`src/api/types.ts:176,217`) are **not** removed.
  Task 9 replaces `types.ts` wholesale with types generated from the gears' OpenAPI document,
  which do not have these fields — they disappear there on their own. Hand-deleting them now
  would edit shapes Task 9 regenerates anyway.
- **Commits:** Pass 1 `a8a7b90646ea1f94c975f045bae0a16740d61fb2`; Pass 2
  `682a34ff827f6106d2545a8ad1afee015ff8cc16`. (This SHA is necessarily one generation behind the
  commit that results from amending it to add this line — amending a commit to record its own
  hash always produces a new hash. The task report for this fix names the true final commit.)

### Runner Defaults settings

- **What was removed:** the whole "Test Runner Defaults" settings page — default timeout, max
  concurrent runs, queue TTL, queue max depth.
- **Legacy endpoints it needed:** `GET /settings/runner-defaults`, `PUT /settings/runner-defaults`.
- **Files touched:**
  - Deleted `src/pages/settings/SettingsRunnerDefaultsPage.tsx`.
  - `src/components/layout/Sidebar.tsx` — removed the "Runner Defaults" nav entry.
  - `src/App.tsx` — removed the `/settings/runner-defaults` route and its import; changed the
    `/settings` index redirect target from `runner-defaults` to `variables` (the first remaining
    settings sub-page).
  - `src/api/hooks.ts` — removed `useRunnerDefaults`, `useUpdateRunnerDefaults`, the
    `runnerDefaults` query key, and the `RunnerDefaultsConfig` type import.
  - `src/api/types.ts` — removed the `RunnerDefaultsConfig` interface.
- **Commit:** a8a7b90646ea1f94c975f045bae0a16740d61fb2

---

## Task 8a: the ten surfaces the gears do not serve (CONTRACT-DIFF §8, C1–C10)

Ten fields the UI renders that the gears serve nowhere. Six of the ten cost a surface
(C1, C3, C4, C6, C7, C10); four are labelled rather than removed (C2, C5, C8, C9) and
are recorded here only where they also deleted content. Every commit SHA in this Task
8a block is `2a48e667e29e188186cc6a663de3614e64207dcf` — one commit for all ten, with
this record added in the commit immediately after it (a commit cannot contain its own
hash).

### C1 — The test catalog (`/tests` and `/tests/view`)

- **What was removed:** the whole Tests section — the catalog table (title, component,
  description, tags, quality vectors, versions, LOC, per-test source view with syntax
  highlighting, "Run test" and "Schedule test" from the detail page) and the "Tests"
  sidebar entry. `PlanDto.test_files` supplies bare paths, and **every** rendered
  metadata column and the file's source text have no source anywhere in the gears, so
  the page would have been a table of paths with every column blank.
- **Legacy endpoints it needed:** `GET /tests` (the catalog rows with their metadata)
  and `GET /tests/source?test_file=…&source=…&repo_id=…&branch=…` (the file's text).
  `GET /tests` still exists and is still used — `useTests` has three surviving
  consumers (the schedule dialog, the plan detail page and the custom-plan editor),
  which need only `plan_id` / `test_file`. Only the *catalog surface* went.
- **Files touched:**
  - Deleted `src/pages/TestCatalogPage.tsx`, `src/pages/TestDetailPage.tsx`,
    `src/components/tests/TestCatalogTable.tsx`.
  - `src/App.tsx` — removed the `/tests` and `/tests/view` routes and their imports.
  - `src/components/layout/Sidebar.tsx` — removed the "Tests" nav entry and its icon.
  - `src/components/runs/TestResultsTable.tsx` — a test name is now plain text
    instead of a link to `/tests/view`; the `repoId` and `source` props, which only
    fed that link, are gone.
  - `src/pages/RunDetailPage.tsx` — stopped passing those two props.
  - `src/components/dashboard/FlakyTestsCard.tsx` — a flaky test now links to its
    plan instead of `/tests/view` (the row keeps a working destination).
  - `src/api/hooks.ts` — removed `useTestSource`, `useTestsPrefetch` and the
    `testSource` query key. **`useTests` was kept**: a grep found three live
    consumers outside the removed pages.
- **Alternative not taken** (per the decision rule's step 5): degrade `/tests` to a
  bare path list. One human edit reverses this — restore the three deleted files and
  the two routes, then strip the metadata columns from the table.
- **Commit:** 2a48e667e29e188186cc6a663de3614e64207dcf

### C3 — Platform health (three affordances)

Labelled unavailable rather than deleted outright, but the labelled state replaced
real content, so it is recorded here.

- **What was removed, and what stands in its place:**
  - Dashboard: the per-platform health strip (status dot, observed version, and the
    healthy/degraded/unhealthy/unreachable counts) is now one labelled card, and the
    "Platforms healthy/total" KPI tile is gone (the KPI strip is five tiles → four).
  - Platform detail page: the header status badge, the status-message card, the four
    count cards (Nodes / Workers / Control Plane / Namespaces), the "Cluster Health
    Snapshot" card, the "Last Checked" tile and the per-node table are replaced by
    one labelled notice at the top of the page.
  - Platform detail page: the "refresh platform version" button is left visible but
    **disabled**, with the shared "not available in this deployment" tooltip.
- **Legacy fields/endpoints it needed:** `DashboardStats.platforms_summary`
  (`GET /dashboard`) with `PlatformBrief.status/version/build`; `GET
  /platforms/{name}/details` returning `status`, `status_message`, `node_count`,
  `ready_node_count`, `worker_count`, `ready_worker_count`, `control_plane_count`,
  `ready_control_plane_count`, `namespace_count`, `checked_at` and `nodes[]`; and
  `POST /platforms/{name}/refresh-version`. **This was true when written and is no
  longer true — see "Restored" below.** At the time, the gears modelled a platform as
  `available: boolean` and observed nothing about the cluster behind it.
- **Files touched:**
  - `src/components/dashboard/PlatformsStrip.tsx` — now takes no props and renders
    the shared notice inside its card, with a link to the platform list.
  - `src/components/dashboard/KpiStrip.tsx` — removed the `platformsHealthy` /
    `platformsTotal` props and the Platforms tile; grid 5 → 4 columns.
  - `src/pages/DashboardPage.tsx` — stopped reading `data.platforms_summary` (which
    would have thrown, not merely rendered a zero) and changed the page subtitle from
    "Live view of runs, failures, and platform health" to "Live view of runs and
    failures". *(The subtitle change was claimed here before it had actually been made —
    a `str.replace` with the wrong indentation silently did nothing. Fixed in
    `c9d672d931d0749489c29803cfe3413391f58c95`.)*
  - `src/pages/PlatformDetailPage.tsx` — removed the health surfaces listed above,
    added the notice, disabled the refresh button and dropped the now-dead
    `statusClasses` helper and `toast` import.
  - `src/api/hooks.ts` — removed `useRefreshPlatformVersion` (sole consumer was the
    disabled button).
- **A fourth surface, found on review** (`c9d672d931d0749489c29803cfe3413391f58c95`): the
  platforms **table's "Status" column**, derived by `derivePlatformHealth` from
  `version_detect_error` / `version_detected_at`. Nothing in this deployment sets either,
  so every platform read a permanent "Unknown" under the tooltip *"No detection cycle has
  run yet for this platform"* — a "never" told as a "not yet". Removed the column, its
  header, the `derivePlatformHealth` helper and the `PlatformHealth`/`PlatformHealthInfo`
  types (`src/components/platforms/PlatformsTable.tsx`, 8 columns → 7); added the shared
  notice once above the table (`src/pages/PlatformsPage.tsx`).
  In the same pass: the platform detail page's Platform Version row now reads *"Not
  observed in this deployment"* instead of *"Not detected"*, its two branches on
  `version_detected_at` / `version_detect_error` (which can never fire) are gone, and
  `src/components/platforms/EditPlatformDialog.tsx` no longer tells the user that "version
  and build are auto-detected from the vpadm install metadata" — nothing here detects them.
- **Legacy fields the fourth surface needed:** `PlatformInfo.version_detect_error` and
  `version_detected_at`, i.e. a version-detection cycle. `EnvironmentDto` (then `PlatformDto`) had
  `observed_version` / `observed_build` and nothing wrote them. **Also no longer true**
  — the platform-observation design gave both a source.
- **Commit:** 2a48e667e29e188186cc6a663de3614e64207dcf, extended by
  c9d672d931d0749489c29803cfe3413391f58c95

#### RESTORED (branch `feature/qa-platform-specs`)

C3 was recorded as a re-add checklist, and the checklist has now been worked. Two designs
gave these surfaces a source in the gears themselves rather than by re-adding a legacy
endpoint, so nothing here reads `GET /platforms/{name}/details` or
`DashboardStats.platforms_summary`:

- [2026-08-28 platform observation](../docs/superpowers/specs/2026-08-28-platform-observation-design.md)
  — an observation ticker reads each platform's version, build, namespace and base domain
  and writes `observed_version`, `observed_build`, `version_detect_error` and
  `version_detected_at`.
- [2026-08-28 cluster health](../docs/superpowers/specs/2026-08-28-cluster-health-design.md)
  — the same `observe` call now also lists the cluster's nodes and namespaces and persists
  `cluster_status`, `cluster_status_message`, `cluster_nodes`, `cluster_namespace_count`
  and `cluster_checked_at`. `EnvironmentDto` carries them as one optional `cluster` object.

**Restored, and where:**

| C3 affordance | state now |
|---|---|
| Dashboard per-platform health strip (dot, version, healthy/degraded/unhealthy/unreachable counts) | restored — `components/dashboard/PlatformsStrip.tsx` renders the real strip and legacy's count line, plus a "not yet checked" bucket legacy has no equivalent for |
| Platform detail: status badge, status message, the four count cards, the node table, last-checked | restored — `components/platforms/ClusterHealthCard.tsx` (one card rather than legacy's several), covered by `ClusterHealthCard.test.ts` |
| Platform detail: the refresh button | restored and **enabled** — `useRefreshPlatform` calls `POST /qa/v1/environments/{id}/refresh`, which re-observes live |
| Platforms table "Status" column (the fourth surface) | restored — `components/platforms/PlatformsTable.tsx`, driven by cluster status with a reachability fallback (D-CH-6), so it no longer reads a permanent "Unknown" |
| The `UnavailableNotice` above the platforms table | removed — the claim it made ("nothing reads the cluster's nodes") is what this branch falsified |
| Platform detail: the `UnavailableNotice` at the top of the page | removed — replaced by `ClusterHealthCard` |

**Deliberately NOT restored:**

- The **KPI strip's "Platforms healthy/total" tile.** The data exists now, but
  `PlatformsStrip` renders the full breakdown a few pixels below, so a tile would restate
  one slice of it. Left out on redundancy grounds, not for want of a source; the grid stays
  4 columns. Noted in `KpiStrip.tsx` itself.

**Two facts C3 turned on that survive, and must keep surviving:**

- "never checked" and "checked, and unreachable" are *different states*, and a platform
  that has never been polled must not render as a status. That is what the fourth
  surface's permanent "Unknown" got wrong, and D-CH-6's fallback is what keeps it right.
- A build without the `platform-observation` cargo feature must fall back to the
  reachability dot, not manufacture a status. `HealthOutcome::NotAttempted` is what makes
  that hold: `NoopObserver` returns it, and `record_observation` then writes none of the
  five cluster columns, so `cluster` stays `null`.

### C4 — Archive test repositories (the upload form)

- **What was removed:** the "Source Type" selector on the product page's Add Test
  Source dialog and its "Archive upload" branch (the file input, the accepted-format
  hint and the "Upload Archive" submit label). Creating a repository from a git URL
  is untouched; the dialog is now titled "Add Git Repository".
- **Legacy endpoint it needed:** `POST /test-repos/archive` (multipart: `name`,
  `product_id`, `tests_root`, `archive`).
- **Files touched:**
  - `src/pages/ProductDetailPage.tsx` — removed `source_type` / `archive_file` from
    the form state, the archive branch of `handleCreateRepo`, the archive fields, and
    the copy that offered archives.
  - `src/api/hooks.ts` — removed `useCreateArchiveTestRepository` and the
    `apiPostFormData` import (`apiPostFormData` itself is left in `client.ts`).
  - `src/api/types.ts` — removed `CreateArchiveTestRepositoryForm`.
- **Left alone on purpose:** the repository *table* still renders an "Archive" badge
  and the archive-specific columns for a row whose `source_type` says `archive`. No
  such row can be created any more and the gears never return one, so nothing false
  is displayed; those branches disappear with `types.ts` in Task 9.
- **Commit:** 2a48e667e29e188186cc6a663de3614e64207dcf

### C6 — The DAG: pipeline views, the git-plans list, the node editor

- **What was removed:**
  - the run detail page's "Pipeline" card (live per-node status);
  - the custom plan detail page's "Pipeline" card (the plan's own node graph);
  - the "Dependencies" tab of the custom plan editor — the node editor, the
    "Max parallel nodes" input and the "a DAG plan can't also use individual tests"
    guard that only existed to police it;
  - the git-defined DAG plans list under Test Plans → Custom Plans, and its
    "Run git plan" dialog.
- **Legacy endpoints/fields it needed:** `GET /runs/{name}/dag` (`{nodes[],
  parallelism, statuses}`), `GET /git-plans?product_id=…&branch=…`, `POST
  /git-plans/{repo_id}/{plan_id}/run`, and `CustomPlan.nodes` / `parallelism` on
  `GET|POST|PUT /custom-plans`. `CustomPlanDto` is a flat list of files: no gear type
  carries a node graph, a dependency edge, a `run_if` or a parallelism cap.
- **Files touched:**
  - Deleted `src/components/custom-plans/PlanDagView.tsx` (already unreferenced),
    `PlanPipelineView.tsx`, `GitPlansList.tsx`, `RunGitPlanDialog.tsx`,
    `NodeEditor.tsx`.
  - `src/pages/RunDetailPage.tsx`, `src/pages/CustomPlanDetailPage.tsx`,
    `src/pages/CustomPlanEditorPage.tsx`, `src/pages/PlansPage.tsx` — removed the
    panels, the tab and the list.
  - `src/api/hooks.ts` — removed `useRunDag`, `RunDagResponse`, `useGitPlans`,
    `useRunGitPlan`.
  - `src/api/types.ts` — removed `GitDagPlan`. **`PlanNode` was kept**: a grep found
    `src/lib/customPlanTests.ts` and two `types.ts` fields still using it.
- **Kept deliberately:** the custom plan editor's "Whole plans" tab
  (`included_plans`). CONTRACT-DIFF lists `included_plans` under C6, but the task
  brief's decision names only the DAG view and the git-plans list, and unlike a node
  graph an included plan is expandable into the `files` a `CustomPlanDto` does carry.
  Flagged in the task report as an open item rather than removed here.
- **Commit:** 2a48e667e29e188186cc6a663de3614e64207dcf

### C7 — Product scoping of runs, schedules and custom plans

The one item that failed **silently**: `RunDto`, `ScheduleDto` and `CustomPlanDto`
carry no product key of any kind (verified against `/openapi.json` on the running
gears, not only the document), and the gears ignore an unknown query parameter rather
than rejecting it. A product-scoped request therefore returned HTTP 200 with the whole
tenant's rows and nothing errored. A client-side filter is impossible — there is no
field to filter on — so the controls were **removed rather than labelled**.

- **What was removed:**
  - the `product_key` / `product_id` query parameters (and the per-product query keys
    and `enabled: !!product` gates) on `useRuns`, `useSchedules` and `useCustomPlans`;
  - the Runs page's FQL `product` / `productKey` fields — the accessor, the value
    suggestions, the advertised field list and the example query;
  - the Custom Plans list's **Product column** and its FQL `product` / `productId` /
    `productName` fields (its `productLabels` prop is gone, and `PlansPage` no longer
    builds one);
  - the custom plan detail page's "Product:" row, which said "Not selected" for every
    plan in this deployment;
  - the per-run product label on the runs table and the dashboard's active-runs card;
  - the `RequireProduct` gate on `/runs`, `/runs/:name` and `/schedules`: those lists
    are tenant-wide, so gating them on a product selection they do not honour was the
    same false claim in another form.
- **Added instead:** a plain statement of the true scope on each of the three surfaces
  ("every run in this deployment, not scoped to the selected product", and the same
  for schedules and custom plans). This is a statement about the *list*, not a label
  on a control — no control claiming to filter by product survives.
- **Legacy fields it needed:** a product key on the run, schedule and custom plan
  themselves (legacy `WorkflowRun.product_key`, `ScheduleInfo.product_key`,
  `CustomPlan.product_id`), or a server filter accepting `product_key` / `product_id`
  on `GET /runs`, `GET /schedules`, `GET /custom-plans`.
- **Files touched:** `src/api/hooks.ts`, `src/App.tsx`, `src/pages/RunsPage.tsx`,
  `src/pages/SchedulesPage.tsx`, `src/pages/PlansPage.tsx`,
  `src/pages/CustomPlanDetailPage.tsx`, `src/components/custom-plans/CustomPlansList.tsx`,
  `src/components/custom-plans/RunCustomPlanDialog.tsx`,
  `src/components/schedules/CreateScheduleDialog.tsx`,
  `src/components/runs/RunsTable.tsx`, `src/components/dashboard/ActiveRunsCard.tsx`.
- **Residual risk, stated plainly:** the **global product switcher** in the sidebar is
  untouched, because plans, tests, platforms, the dashboard and analytics are all
  genuinely product-scoped. Switching product therefore still leaves the runs,
  schedules and custom-plan lists unchanged. That is now *true and stated on the page*
  rather than silently false, but it is the residual oddity a human should know about.
- **To re-add:** give the three DTOs a product key, then restore the query parameters
  (server-side) or filter on the new field (client-side) — one place each, in the
  three hooks named above.
- **Commit:** 2a48e667e29e188186cc6a663de3614e64207dcf

#### C7's second half — `CustomPlan.description` and `ScheduleInfo.description`

CONTRACT-DIFF §8-C7 ends with *"Also here: `CustomPlan.description` and
`ScheduleInfo.description`, both absent from their DTOs."* That sentence was dropped when the
brief's decision table narrowed C7 to "product filter on runs/schedules/custom plans", so the
first pass missed it. Both were live text inputs whose content the gears accept and discard
with no error — the C10 shape without the confidentiality — which is decision-rule item 4, *a
gap that yields silently wrong data must not ship in any form*.

- **What was removed:**
  - the custom plan editor's "Description" input, its state and the `description` it submitted
    (`src/pages/CustomPlanEditorPage.tsx`); the Name field, previously the left half of a
    two-column grid, is now a single full-width field rather than a half-width one beside a gap;
  - the schedule dialog's "Description (Optional)" input, its state, its reset and the
    `description` on the created/updated payload (`src/components/schedules/CreateScheduleDialog.tsx`);
  - the custom plans list's FQL `description` field — advertised field, accessor, free-text
    fallback term — and the row link's `name — description` tooltip, now just the name
    (`src/components/custom-plans/CustomPlansList.tsx`);
  - the custom plan detail page's description line under the title
    (`src/pages/CustomPlanDetailPage.tsx`), which rendered an empty paragraph;
  - the fields themselves: `CustomPlan.description`, `CreateCustomPlanForm.description`,
    `ScheduleInfo.description`, `CreateScheduleForm.description` (`src/api/types.ts`).
- **Legacy fields it needed:** `description` on `GET|POST|PUT /custom-plans` and on
  `GET|POST|PUT /schedules`. `CustomPlanDto` is `{id, name, files, tags, timeout_seconds}` and
  `ScheduleDto` has no description field of any kind.
- **Commit:** c9d672d931d0749489c29803cfe3413391f58c95

### C10 — The "Secured" variable affordance

The only item with a **confidentiality** consequence. `VariableDto` and
`UpsertVariableReq` have no `secure` field, nothing behind them masks or encrypts a
value, and `GET /variables` returns the value verbatim. The editor's padlock was the
UI telling the user a protection existed. Labelling it would have kept the
misrepresentation while still accepting the secret, so the affordance was removed.

- **What was removed — every part of it:** the "Secured" checkbox on the add-row; the
  value input's `type="password"` / `autoComplete="new-password"` switch; the
  `SECRET_PLACEHOLDER = '********'` constant, the `savedVariableForUi` substitution
  that applied it to a freshly-saved variable, and the `********` rendering in the
  list; and the per-row `Lock` / `Unlock` padlock icon. The add-row grid is now three
  columns instead of four.
- **Legacy field it needed:** `PipelineVariable.secure` on `GET|PUT
  /settings/variables` and `GET|PUT /platforms/{name}/variables`, plus an actual
  masking/credstore implementation behind it — the flag alone would not be enough,
  since a `secure` that does not encrypt is the same misrepresentation.
- **What was removed on review** (`c9d672d931d0749489c29803cfe3413391f58c95`) — the same
  promise, in prose, on the routed page that mounts the editor: *"Secure values stay stored
  in the database, return masked in the UI, and keep their saved value unless you replace
  them."* and *"If you want the variable stored encrypted and shown masked in logs and UI,
  enable Secure."* Both are gone. The second was worse than the padlock: it asserted
  **encryption at rest**, which nothing here does, and instructed the user to enable a control
  that no longer exists. In their place, `VariablesEditor` itself now states that a variable is
  stored and returned in cleartext and must not hold a credential — inside the shared component
  rather than on one page, so both mounts carry it and a third could not miss it.
  The first pass missed this because its verification grep used the identifier `Secured`; the
  page said `Secure`. The re-run swept the promise's vocabulary instead — `encrypt|masked|
  masking|secret|secure|hidden|confidential|obfuscat|redact|plaintext|cleartext|padlock`.
- **Files touched:**
  - `src/components/settings/VariablesEditor.tsx` — the shared editor behind both
    `/settings/variables` and the per-platform variables card.
  - `src/pages/settings/SettingsVariablesPage.tsx` — the two passages above.
  - `src/api/types.ts` — removed `PipelineVariable.secure`, replaced by a comment
    saying why. (Unlike the dead DTO fields Task 8 left for Task 9, this one had to
    go: it was a **required** field, so every object literal in the editor would have
    had to keep supplying a value the backend cannot honour.)
- **Commits:** 2a48e667e29e188186cc6a663de3614e64207dcf, then
  c9d672d931d0749489c29803cfe3413391f58c95 for the prose.

### Not removed — the labelled-unavailable set (C2, C5, C8, C9)

Recorded for completeness; nothing here needs a re-add checklist because nothing was
deleted. All four share one presentational treatment,
`src/components/ui/unavailable.tsx` (`UnavailableNotice`, plus `unavailableTitle` for
the tooltip on a disabled control), so the five labelled surfaces read as one decision
rather than five bugs.

- **C2 — per-test logs.** The run detail page's Test Results card carries a notice
  saying a failed test shows no error detail of its own and pointing at the run-level
  log viewer below. `RunTestResultDto` has no `logs`. The per-case breakdown
  (`TestResult.cases`) is **left rendering as-is**: those rows exist at `GET
  /qa/v1/test-case-results?$filter=run_id eq …` and are the adapter's to recover.
- **C5 — deleting a run.** Nothing was labelled, because a grep found no
  delete-a-run affordance to label: both consumers of `useDeleteRun` confirm with
  "Stop run", act only on an active run and report "is stopping". CONTRACT-DIFF
  §8-C5's claim that the two bind the hook to different intents does not hold; the
  finding is recorded on the hook itself, which the adapter must map to `POST
  /qa/v1/runs/{id}/cancel`. On review this was carried back into
  `gears/qa-platform/docs/CONTRACT-DIFF.md`: C5 is withdrawn from §8 (now **nine** items) and
  reclassified as absorbable at §7.12, where Task 10 reads it.
- **C8 — code coverage.** Both coverage cards' empty states now say coverage is not
  measured in this deployment rather than implying it has not been reported *yet*.
  The chart branch is untouched and returns the moment real data arrives.
  (`src/components/analytics/CoverageChart.tsx` has no consumer — it is mounted
  nowhere — and was labelled anyway for the day someone mounts it.)
- **C9 — analytics execution counters.** One page-level banner at the top of the
  analytics dashboard, with CONTRACT-DIFF §9.3's condition and copy verbatim. The six
  panels are deliberately left in place.
