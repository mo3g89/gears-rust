// TypeScript types matching the Rust backend models.
//
// A handful of the exports below are now generated: aliased onto a schema from the gears'
// live OpenAPI document (`./generated/openapi.d.ts`, produced by `npm run gen:api` /
// `make ui-contract`) because that schema is byte-identical to the legacy shape it replaces.
// Every other export stays hand-written, because the gears' wire shape for it has been
// renamed, reshaped, or dropped outright (see `gears/qa-platform/docs/CONTRACT-DIFF.md`), or
// because it never had a wire shape at all — a UI-only view model. Each hand-written type
// below says which. Do not force a reshaped or UI-only type through the generated namespace:
// hooks.ts and every component still construct and consume the shapes as written here, and
// Task 10 — not this file — is where the gears' actual response gets adapted onto them.
//
// The runtime helpers at the bottom of this file (`getStatusColor`, `isActiveRun`) are not
// wire types. `isActiveRun` was re-mapped onto qa-runs' lowercase state set by Task 10
// (CONTRACT-DIFF X4); `getPhaseColor` was deleted by the same task because it had no
// callers anywhere in `src`.
import type { components } from './generated/openapi';

type S = components['schemas'];

/** UI-only view model: `TestPlanInfo.plan`'s shape. No gear schema serves this granularity —
 *  `PlanDto` (see `TestPlanInfo` below) is flat, with no nested plan-manifest object. */
export interface TestPlan {
  name: string;
  tags: string[];
  /** `PlanDto` has no description. It stays required and non-nullable because
   *  `PlanList.tsx:55-56` feeds it to a `string[]` collector, and `''` is exactly what an
   *  absent description means to every reader of it (each one truthiness-checks it) — the
   *  same honest-absence spelling `TestResult.logs` uses. */
  description: string;
  /* The four fields below are optional because Task 10's adapter leaves them absent
   * rather than filling them: a `validation: false` or a `timeout_seconds: 0` would be a
   * claim about the plan manifest that nothing served (CONTRACT-DIFF row 2).
   * `timeout_seconds` and `exclusive` ARE on `PlanDto`, but both are nullable there. */
  timeout_seconds?: number;
  node_selector?: Record<string, string>;
  tolerations?: unknown[];
  validation?: boolean;
  /** Plan-level exclusivity from plan.yaml. null/undefined = inherit from tests. */
  exclusive?: boolean | null;
}

/** The shape Task 10's adapter returns to components. `PlanDto` covers `name`, `tags`,
 *  `timeout_seconds`, `exclusive` and `test_files`; everything else here (`dir_path`, `source`,
 *  `repo_name`, `product_id/key/name`, `versions`, `plan.description/node_selector/tolerations/
 *  validation`, `recent_runs`) is absent from the gear and plan identity itself changes from a
 *  synthetic `id` to `(repo_id, path)` — CONTRACT-DIFF row 2. */
export interface TestPlanInfo {
  id: string;
  dir_path: string;
  source: string;
  repo_id: string | null;
  repo_name: string | null;
  product_id: string | null;
  product_key: string | null;
  product_name: string | null;
  versions: string[];
  plan: TestPlan;
  test_files: string[];
  /** Most recent runs of this plan (pass/fail health) for the list dots. */
  recent_runs?: ScheduleRunBrief[];
}

/** The adapter's output envelope, not a wire shape — kept hand-written on purpose. The gears
 *  answer `Page<T>` = `{ items, page_info: { limit, next_cursor, prev_cursor } }`, with no
 *  `total` and no page number (CONTRACT-DIFF X2). Every hook Task 10 adapts keeps returning
 *  *this* shape so no exported hook's return type changes; the cursor-to-page bookkeeping lives
 *  in `hooks.ts`, not here. */
export interface PaginatedResponse<T> {
  runs: T[];
  pagination: {
    page: number;
    per_page: number;
    total: number;
    total_pages: number;
  };
}

/** A run's five outcome counters — `RunResultDto`'s shape, field for field. Its own name
 *  in this file rather than a reuse of `ScheduleRunBrief`'s inline fields, because
 *  `WorkflowRun.result` (below) and a schedule's recent-run strip both need it and neither
 *  should own the other's shape. */
export interface RunResultCounts {
  passed: number;
  failed: number;
  skipped: number;
  in_progress: number;
  total: number;
}

/** The shape Task 10's adapter returns to components. `RunDto` renames `phase` to `state` and
 *  changes it from capitalised Argo phases to qa-runs' lowercase set (CONTRACT-DIFF X4),
 *  restructures `plan_id`/`run_kind` into `target{}` (X6), and drops `duration`, `product_key`,
 *  `repo_name`, `source_ref(_kind)` and every `slack_*` field entirely — CONTRACT-DIFF row 5. */
export interface WorkflowRun {
  name: string;
  plan_id: string;
  run_kind?: string;
  phase: string;
  started_at: string | null;
  finished_at: string | null;
  duration: string | null;
  message: string | null;
  app_version: string | null;
  app_build: string | null;
  test_version: string | null;
  platform: string | null;
  product_key: string | null;
  /** Optional now: it is required on `RunDto`, but the dashboard's ten-field
   *  `DashboardRunDto` projection drops it (CONTRACT-DIFF row 1), and a `false` there would
   *  be a claim that a run is *not* a validation run rather than "unknown". Both readers
   *  (`RunsTable.tsx:88,101`, `RunsPage.tsx:143,165`) already truthiness-check it, and both
   *  are fed from `useRuns`, where the real value is present. */
  is_validation?: boolean;
  run_source?: string | null;
  schedule_id?: string | null;
  slack_notifications_enabled?: boolean | null;
  slack_channel?: string | null;
  slack_notification_events?: ScheduledRunNotificationEvent[];
  repo_id?: string | null;
  repo_name?: string | null;
  source_ref?: string | null;
  source_ref_kind?: string | null;
  test_file?: string | null;
  parameters?: RunParameter[];
  /** Effective platform exclusivity of this run. null = the run predates the flag. */
  exclusive?: boolean | null;
  /** The run's five outcome counters, notably `skipped` (Task 10, ratified 2026-08-28: a
   *  skipped test no longer fails a run, so the run list and detail view are what makes a
   *  `succeeded` run with a non-zero skip count visible instead — see `RunsTable.tsx` and
   *  `RunDetailPage.tsx`). Optional for the same reason `is_validation` is: `RunDto` always
   *  carries it, but the dashboard's ten-field `DashboardRunDto` projection does not, so
   *  `dashboardRunFromDto` leaves it unset. */
  result?: RunResultCounts;
}

/** A per-launch env parameter passed to the runner. Generated — `RunParameterDto` is
 *  `{ name, value }`, field-for-field identical, and (like this type before it) has no
 *  `secure` flag: nothing on this path stores a value confidentially, so a parameter must not
 *  carry a secret. */
export type RunParameter = S['RunParameterDto'];

/** A single recent run of one test (by file): the test's status in that run,
 *  the run's version + platform (target), and a short error. The shape Task 10's adapter
 *  returns to components — `Page<TestResultDto>` renames `workflow_name`→`run_id`,
 *  `platform`→`platform_id`, `app_version`→`product_version`, `test_version`→`branch`, and has
 *  no `phase`, `short_error` or `reportportal_url` at all (CONTRACT-DIFF row 7). */
export interface TestRunResult {
  workflow_name: string;
  plan_id: string;
  /** Nullable now: `TestResultDto` carries no run phase, and there is nothing to derive
   *  one from on a per-result row (CONTRACT-DIFF row 7). */
  phase: string | null;
  status: string;
  started_at: string | null;
  duration: string | null;
  app_version: string | null;
  test_version: string | null;
  platform: string | null;
  short_error: string | null;
  reportportal_url: string | null;
}

/** The shape Task 10's adapter returns to components. `ScheduleDto` renames `schedule`→`cron`,
 *  **inverts** `suspended`→`enabled`, changes `include_tags`/`exclude_tags` from a comma-string
 *  to a real array, and turns the nullable-boolean `exclusive` into a three-valued
 *  `exclusive_choice: "true" | "false" | "auto"`; `description`, `plan_name`, `product_key`,
 *  `product_name` and `recent_runs` are absent — CONTRACT-DIFF row 18. */
export interface ScheduleInfo {
  name: string;
  schedule_id: string;
  plan_id: string;
  plan_name: string | null;
  plan_type: string;
  schedule: string;
  suspended: boolean;
  last_scheduled: string | null;
  created_at: string | null;
  /* No `description`: `ScheduleDto` has no such field — see `CustomPlan` above. */
  platform: string;
  branch: string | null;
  test_file: string | null;
  include_tags: string | null;
  exclude_tags: string | null;
  /** Schedule-level choice: null = auto (resolve from plan.yaml / TEST_META). */
  exclusive?: boolean | null;
  slack_notifications_enabled: boolean;
  slack_channel: string | null;
  slack_notification_events: ScheduledRunNotificationEvent[];
  recent_runs: ScheduleRunBrief[];
  product_key: string | null;
  product_name: string | null;
}

/** UI-only view model for the schedule list's "recent runs" strip. `ScheduleDto` carries no
 *  `recent_runs` field at all (CONTRACT-DIFF row 18); the strip is rebuilt client-side (§7) from
 *  a separate runs query, not deserialized off this shape. */
export interface ScheduleRunBrief {
  workflow_name: string;
  phase: string;
  started_at: string | null;
  finished_at: string | null;
  passed: number;
  failed: number;
  skipped: number;
  in_progress: number;
  total: number;
}

/** The shape the Create Schedule form builds and Task 10's adapter must translate.
 *  `NewScheduleReq` renames `cron_expr`→`cron`, drops `schedule_id` (server-assigned) and
 *  `description` (no such field on the DTO), requires `enabled`, and carries no `slack_*`
 *  fields at all — a create that sets them needs a follow-up `PUT .../notifications`
 *  (CONTRACT-DIFF row 19). */
export interface CreateScheduleForm {
  plan_id: string;
  schedule_id: string;
  cron_expr: string;
  platform?: string;
  branch?: string;
  test_file?: string;
  include_tags?: string;
  exclude_tags?: string;
  /** Omit or send null for auto; true/false force the platform-access mode. */
  exclusive?: boolean | null;
  slack_notifications_enabled?: boolean;
  slack_channel?: string;
  slack_notification_events?: ScheduledRunNotificationEvent[];
}

/** Dead: `grep -rn "UpdateScheduleForm" src` finds only this declaration.
 *  `useUpdateSchedule` (`hooks.ts:875`) actually takes a `CreateScheduleForm` — this type
 *  was never wired to it. Kept rather than deleted; pruning untouched legacy exports is not
 *  this task's job (same policy as `apiPostFormData` in `client.ts`) and is logged as a
 *  deferred minor for the final whole-branch review. */
export interface UpdateScheduleForm {
  cron_expr: string;
}

/** The shape Task 10's adapter must translate onto `PUT .../notifications`'s
 *  `{slack_enabled, slack_channel, slack_events}`: `enabled`→`slack_enabled`,
 *  `channel`→`slack_channel`, `events`→`slack_events`, and `slack_enabled`/`slack_events` are
 *  required on the gear side where this form's `channel`/`events` are optional
 *  (CONTRACT-DIFF row 24). */
export interface UpdateScheduleNotificationsForm {
  enabled: boolean;
  channel?: string | null;
  events?: ScheduledRunNotificationEvent[];
}

/** Generated — `TestCaseResultDto` carries `nodeid`, `name`, `status`, `duration`, `reason`
 *  and `ticket` field-for-field identical to the legacy shape (plus `id`, `run_id`, `test_file`
 *  and `created_at`, which every consumer here already ignores). Status values:
 *  PASSED | FAILED | SKIPPED | XFAIL | XPASS | ERROR. `ticket` is the key (e.g. "VHP-980")
 *  extracted from an xfail/skip reason, if any. Recoverable from
 *  `GET /qa/v1/test-case-results?$filter=run_id eq …` — CONTRACT-DIFF §8-C2.
 *  Safe to alias: `TestCase` has zero by-name importers repo-wide — it reaches components only
 *  through `TestResult.cases?: TestCase[]` (`:243` below) — and both structural readers
 *  (`TestResultsTable.tsx:108-195`, `AnalyticsDashboard.tsx:289,370-393`) are read-only
 *  (`.map`/`.reduce` over `status`/`name`/`duration`/`ticket`/`reason`), so the four extra
 *  required fields the generated type adds cost nothing at either site. */
export type TestCase = S['TestCaseResultDto'];

/** The shape Task 10's adapter returns to components — the per-run test list inside
 *  `RunDetails` below. `RunTestResultDto` renames `name`→`test_name` and adds `test_file`,
 *  `run_id`, `created_at`/`updated_at`, but carries neither `logs` nor `cases`: this gear's
 *  only source of run outcomes has no per-test log slice, a real parity gap rather than a
 *  design choice (CONTRACT-DIFF §8-C2). `cases` is separately recoverable (see `TestCase`
 *  above); `logs` is not recoverable at all. */
export interface TestResult {
  name: string;
  test_file?: string | null;
  status: string;
  duration?: string | null;
  logs: string;
  launch_id: string | null;
  jira_key: string | null;
  /** Per-function breakdown for this file (run view only). */
  cases?: TestCase[];
}

/** The shape Task 10's adapter returns to components. `DashboardStatsDto` has no
 *  `total_plans`, `total_schedules` or `platforms_summary` (CONTRACT-DIFF §8-C3), and its
 *  `recent_runs`/`active_runs_list` come back as a ten-field `DashboardRunDto[]` projection
 *  losing `plan_id`, `run_kind`, `finished_at`, `message`, `app_build`, `test_version`,
 *  `is_validation`, `exclusive` and every `slack_*` — row 1. */
export interface DashboardStats {
  /** Optional: `DashboardStatsDto` has no such field, and a `0` beside a "Total plans"
   *  label is a confident false claim rather than an empty state (CONTRACT-DIFF §8-C3).
   *  Nothing renders it — Task 8a's KPI strip never bound it. */
  total_plans?: number;
  total_runs: number;
  active_runs: number;
  /** Optional for the same reason as `total_plans` above. */
  total_schedules?: number;
  recent_runs: WorkflowRun[];
  recent_run_test_trend: DashboardRunTestTrendPoint[];
  daily_test_status_trend: DashboardDailyStatusPoint[];
  active_runs_list: WorkflowRun[];
  failed_recent: FailedTestCard[];
  failed_24h_count: number;
  failed_prev_24h_count: number;
  pass_rate_24h: number | null;
  pass_rate_prev_24h: number | null;
  flaky_tests: FlakyTestCard[];
  /** Optional: qa-environments observes nothing about the cluster behind an environment, so
   *  there is no healthy/degraded/unhealthy/unreachable count to serve and no defensible
   *  default for one (CONTRACT-DIFF §8-C3). Task 8a replaced the strip that rendered it
   *  with a labelled-unavailable card, so nothing reads it. */
  platforms_summary?: PlatformsSummary;
  quality_vectors_pass_rate: QualityVectorPassRate[];
}

/** Generated — `QualityVectorPassRateDto` is `{vector, passed, failed, total, tests}`,
 *  field-for-field identical (all required, no renames). */
export type QualityVectorPassRate = S['QualityVectorPassRateDto'];

/** The shape Task 10's adapter returns to components. `FailedTestCardDto` renames
 *  `workflow_name`→`run_id` (uuid), `plan_id`→`repo_id`+`plan_path`, `platform`→`platform_id`,
 *  and makes `test_file`/`platform_id` optional where this type requires them
 *  (CONTRACT-DIFF row 1). */
export interface FailedTestCard {
  test_name: string;
  test_file: string | null;
  workflow_name: string;
  plan_id: string;
  platform: string | null;
  finished_at: string | null;
  jira_key: string | null;
  launch_id: string | null;
}

/** The shape Task 10's adapter returns to components. `FlakyTestCardDto` renames
 *  `plan_id`→`repo_id`+`plan_path` and makes them optional where this type requires
 *  `plan_id` (CONTRACT-DIFF row 1). */
export interface FlakyTestCard {
  test_name: string;
  test_file: string | null;
  plan_id: string;
  passed: number;
  failed: number;
  total: number;
}

/** UI-only view model. `DashboardStatsDto` has no environment-health rollup at all — qa-environments
 *  models an environment's availability as a single `available: boolean` and observes nothing about
 *  the cluster behind it (CONTRACT-DIFF §8-C3). */
export interface PlatformsSummary {
  total: number;
  healthy: number;
  degraded: number;
  unhealthy: number;
  unreachable: number;
  items: PlatformBrief[];
}

/** UI-only view model — see `PlatformsSummary` above (§8-C3). */
export interface PlatformBrief {
  name: string;
  status: string;
  version: string | null;
  build: string | null;
}

/** UI-only view model. `RunTestTrendPointDto` adds a required `run_id` and makes
 *  `started_at` optional as well as nullable, so it is not a byte-identical match; kept
 *  hand-written rather than partially aliased. */
export interface DashboardRunTestTrendPoint {
  run_name: string;
  started_at: string | null;
  tests_total: number;
  passed: number;
  failed: number;
  skipped: number;
}

/** Generated — `DailyStatusPointDto` is `{day, passed, failed}`, field-for-field identical. */
export type DashboardDailyStatusPoint = S['DailyStatusPointDto'];

/** The shape Task 10's adapter returns to components. `RunDetailDto` is `RunDto` flattened
 *  (no `run` wrapper) plus a `result` summary and `test_results: RunTestResultDto[]`; there is
 *  no `reportportal_url` at all (Task 8 stripped that surface) — CONTRACT-DIFF row 9. */
export interface RunDetails {
  run: WorkflowRun;
  test_results: TestResult[];
  /** Optional: the gear has no such field and Task 8 removed the surface that rendered
   *  it, so the adapter omits it rather than sending `''` (CONTRACT-DIFF row 9). */
  reportportal_url?: string;
}

/** UI-only view model. The gears publish no test-file catalog anywhere: `PlanDto.test_files`
 *  gives paths only, and `title`, `component`, `description`, `tags`, `quality_vectors`,
 *  `versions` and `loc` have no source in any gear (CONTRACT-DIFF §8-C1). */
export interface TestFileInfo {
  plan_id: string;
  plan_name: string;
  source: string;
  source_path?: string | null;
  repo_id: string | null;
  repo_name: string | null;
  product_id: string | null;
  product_key: string | null;
  product_name: string | null;
  test_file: string;
  title?: string | null;
  component?: string | null;
  description?: string | null;
  tags: string[];
  quality_vectors?: string[];
  versions?: string[];
  loc?: number;
}

/** The shape Task 10's adapter returns to components. `UpsertCustomPlanReq`/`CustomPlanDto`
 *  restructure `{plan_id, test_file}[]` into `files: {repo_id, plan_path, path}[]`
 *  (CONTRACT-DIFF row 42, X6). */
export interface CustomPlanTest {
  plan_id: string;
  test_file: string;
}

/** UI-only. No gear type carries a `run_if` or any other DAG concept — CONTRACT-DIFF §8-C6. */
export type RunIf = 'succeeded' | 'always';

/** UI-only view model — the custom-plan dependency graph. `CustomPlanDto` is a flat list of
 *  files; no gear type carries a node graph, a dependency edge, or a `run_if`
 *  (CONTRACT-DIFF §8-C6). */
export interface PlanNode {
  id: string;
  /** Include a whole plan's tests (mutually exclusive with `tests`). */
  plan?: string;
  /** Explicit tests (mutually exclusive with `plan`). */
  tests?: CustomPlanTest[];
  depends_on?: string[];
  run_if?: RunIf;
}

/** The shape Task 10's adapter returns to components. `CustomPlanDto` renames `tests`→`files`
 *  (row 42) and has no `included_plans`, `nodes` or `parallelism` (§8-C6) and no `product_id`
 *  or `description` (§8-C7) — everything a custom plan's product scoping and DAG features
 *  need is simply absent from the gear. */
export interface CustomPlan {
  id: string;
  name: string;
  /* No `description`: `CustomPlanDto` has no such field, so anything typed into one
   * was dropped without an error. Removed rather than defaulted — see
   * `REMOVED-SURFACES.md` (Task 8a, C7). */
  tests: CustomPlanTest[];
  included_plans?: string[];
  nodes?: PlanNode[];
  parallelism?: number | null;
  product_id: string | null;
  created_at: string;
}

/** The shape the Create/Edit Custom Plan form builds. `UpsertCustomPlanReq` has no
 *  `description`, `product_id`, `included_plans`, `nodes` or `parallelism` field — a create
 *  that sets them loses them silently (CONTRACT-DIFF row 46). */
export interface CreateCustomPlanForm {
  name: string;
  tests: CustomPlanTest[];
  included_plans?: string[];
  nodes?: PlanNode[];
  parallelism?: number | null;
  product_id?: string;
}

/** Dead: `grep -rn "RunTestForm" src` finds only this declaration. `useRunSingleTest`
 *  (`hooks.ts:1252`) takes its arguments as an inline destructured object, not this type.
 *  Kept rather than deleted — same deferred-cleanup policy as `UpdateScheduleForm` above and
 *  `apiPostFormData` in `client.ts`. */
export interface RunTestForm {
  test_file: string;
  platform?: string;
  branch?: string;
}

// `NodeSummary`, `NodeCounts` and `ClusterHealth` were deleted at branch close.
//
// They were hand-written mirrors of `ClusterHealthDto`/`NodeSummaryDto`/
// `NodeCountsDto`, reachable only from `ClusterHealthCard.tsx` and
// `clusterHealthFromDto` — both deleted by Task 21 under user decision U4,
// along with the five `cluster_*` columns Task 19 dropped. `EnvironmentDto`
// has no `cluster` field any more; a product's own facts arrive through
// `observed_attrs` and its verdict through `health_state`. The whole-branch
// review found the three types still here with no consumer (m-8).

/** The shape Task 10's adapter returns to components. `EnvironmentDto` renames
 *  `version`→`observed_version`, `build`→`observed_build` (CONTRACT-DIFF row 50);
 *  `version_detected_at` and `version_detect_error` are served directly (Task 7/9).
 *  The product-specific fields that used to sit here — `observed_namespace`,
 *  `vhp_base_url` and the `cluster` object — are gone: Task 19 dropped their
 *  columns, and what a product observes now arrives in `observed_attrs`, keyed
 *  by its plugin's own `observed_schema()`. */
export interface EnvironmentInfo {
  /** Stable UUID assigned at environment creation (Phase A). Use `name` for
   *  display / URLs; internal references will move to `id` in later phases. */
  id: string;
  name: string;
  created_at: string;
  description: string;
  product_id: string | null;
  version: string | null;
  build: string | null;
  /** The operator-set availability toggle: an unavailable environment accepts
   *  no new leases. Carried since Task 21, which made it one of the four fixed
   *  leading columns -- it is a fact every product has, unlike the two it
   *  replaced. `environmentFromDto`'s own doc used to say `available` "has no
   *  legacy field and is not carried"; that was true of the legacy UI, not of
   *  what the table needs. */
  available: boolean;
  /** The plugin's observed attributes, keyed by `FieldDesc.key` and bounded by
   *  `retain_declared` — every key here was declared in the product plugin's
   *  `observed_schema()`. The environments table renders one column per
   *  `in_table` descriptor from this map (Task 21), which is what replaced the
   *  hardcoded "Namespace" and "VHP URL" columns. */
  observed_attrs: Record<string, string>;
  /** The plugin's health verdict: `ok`, `degraded`, `down` or `unknown`. It
   *  replaced the five `cluster_*` columns, which Task 19 dropped. */
  health_state: string;
  /** Classified text explaining the verdict, never a formatted error (D12). */
  health_detail: string | null;
  version_detected_at: string | null;
  version_detect_error: string | null;
  /** Whether this environment is its product's **default** — what the Run and
   *  Schedule dialogs' "Default cluster" option resolves to. At most one
   *  environment per product has it set; see `defaultEnvironmentForProduct`. */
  is_default: boolean;
  /** Per-environment default branch used by Run/Schedule dialogs and the
   *  Environment Details page when listing plans/tests. Falls back to the
   *  repository's default branch when empty. */
  default_branch: string | null;
}

/** The shape Task 10's adapter returns to components for the environment detail page.
 *  Was, until Task 6, a hand-duplicated copy of `EnvironmentInfo` plus a set of dead optional
 *  cluster-shaped fields (`status?`, `node_count?`, …) that qa-environments had no source
 *  for and nothing here ever populated (CONTRACT-DIFF row 51, §8-C3). Those stand-ins are
 *  gone; what an environment observes reaches this shape through the same
 *  `observed_attrs`/`health_state` pair `EnvironmentInfo` carries. */
export interface EnvironmentDetails {
  id: string;
  name: string;
  created_at: string;
  description: string;
  product_id: string | null;
  version: string | null;
  build: string | null;
  /** The operator-set availability toggle: an unavailable environment accepts
   *  no new leases. Carried since Task 21, which made it one of the four fixed
   *  leading columns -- it is a fact every product has, unlike the two it
   *  replaced. `environmentFromDto`'s own doc used to say `available` "has no
   *  legacy field and is not carried"; that was true of the legacy UI, not of
   *  what the table needs. */
  available: boolean;
  /** The plugin's observed attributes, keyed by `FieldDesc.key` and bounded by
   *  `retain_declared` — every key here was declared in the product plugin's
   *  `observed_schema()`. The environments table renders one column per
   *  `in_table` descriptor from this map (Task 21), which is what replaced the
   *  hardcoded "Namespace" and "VHP URL" columns. */
  observed_attrs: Record<string, string>;
  /** The plugin's health verdict: `ok`, `degraded`, `down` or `unknown`. It
   *  replaced the five `cluster_*` columns, which Task 19 dropped. */
  health_state: string;
  /** Classified text explaining the verdict, never a formatted error (D12). */
  health_detail: string | null;
  version_detected_at: string | null;
  version_detect_error: string | null;
  default_branch: string | null;
  /** Whether this environment is its product's default; see `EnvironmentInfo.is_default`.
   *  Carried here so the Environment Details page can hand it to the edit dialog. */
  is_default: boolean;
}

/** The shape the Create Environment form builds. `CreateEnvironmentReq` now takes **either** a
 *  raw `kubeconfig` or a `kubeconfig_credstore_ref` (X7 — the gear does the credstore
 *  write itself), and has no `namespace` or `vhp_base_url` (CONTRACT-DIFF row 52). */
export interface CreateEnvironmentForm {
  name: string;
  /** The credentials the product's plugin declares, keyed by `FieldDesc.key`.
   *
   *  **Replaced the single `kubeconfig` string at Task 22.** That field was
   *  VHP's one credential with its name in a form every product shares; the
   *  gear has taken a keyed map since Task 18b, and this is its first UI
   *  caller. `createEnvironmentReqFromForm` sends it as `credentials` and no
   *  longer sends the pre-plugin `kubeconfig`/`kubeconfig_credstore_ref` pair
   *  at all. */
  credentials: Record<string, { material: string }>;
  description?: string;
  product_id?: string;
  default_branch?: string;
  /** Create this environment as its product's default, demoting the previous holder.
   *  Absent means `false`. */
  is_default?: boolean;
}

/** The shape the Edit Environment form builds. The gear's `UpdateEnvironmentReq` is a `PATCH`
 *  (legacy was `PUT`). The form's own `namespace`/`vhp_base_url` fields are gone at
 *  Task 22: they never had a gear field to send to, and Task 19 dropped the columns they
 *  were named after. */
export interface UpdateEnvironmentForm {
  /** A replacement kubeconfig document, or a credstore reference to one. Absent leaves the
   *  stored kubeconfig alone. No component sets this today (the Edit Environment dialog has
   *  no kubeconfig field); the gear's PATCH accepts it. */
  kubeconfig?: string;
  description?: string;
  product_id?: string;
  default_branch?: string;
  /** Make this environment its product's default, or clear the flag. Absent leaves it
   *  unchanged — see `defaultEnvironmentForProduct` for what the flag drives. */
  is_default?: boolean;
}

// Product types
/** The shape Task 10's adapter returns to components. `ProductDto` renames
 *  `tests_folder`→`folder` **and flips its nullability**: required on this type, nullable on
 *  the gear's (CONTRACT-DIFF row 58). */
export interface Product {
  id: string;
  name: string;
  key: string;
  description: string;
  /** The GTS instance id of the product plugin that owns this product's
   *  behaviour. Required on the gear since Task 20a; `null` here only if a
   *  deployment predating that answers without the field.
   *
   *  This is what resolves a product to the `observed_schema` the environments
   *  table renders its columns from (Task 21). */
  plugin_instance_id: string | null;
  tests_folder: string;
  created_at: string;
  updated_at: string;
}

/** The shape the Create/Edit Product form builds. `CreateProductReq` renames
 *  `tests_folder`→`folder` (nullable, optional) and, unlike this form, requires `description`
 *  (CONTRACT-DIFF row 62).
 *
 *  `plugin_instance_id` is a plain `string`, not `string | null` like `Product`'s: it is the
 *  Combobox's current selection, and a controlled input needs a string even when nothing is
 *  picked (`''`). That is a fact about the *form field*, not about what gets sent — an edit
 *  form can and does hold `''` (a legacy or deregistered binding, `openEditDialog`), and on
 *  create Step 3's guard is what keeps it non-empty by submit time, not this type. Whether
 *  the value is sent, and to what, is `productReqFromForm`'s `currentBinding` parameter
 *  (ruling **G-2**) — see it for why an edit that never touches this field must not resend
 *  it either. */
export interface CreateProductForm {
  name: string;
  key: string;
  description?: string;
  tests_folder: string;
  plugin_instance_id: string;
}

/** The shape Task 10's adapter returns to components. `TestRepositoryDto` renames
 *  `tests_root`→`content_root`, replaces `ssh_key_id`+`has_token` with a single nullable
 *  `credential_ref` (X7), and has no `source_type`, `archive_file_name` or `ssh_key_name`
 *  (CONTRACT-DIFF row 29). */
export interface TestRepository {
  id: string;
  name: string;
  source_type: 'git' | 'archive' | string;
  url: string;
  archive_file_name: string | null;
  product_id: string;
  default_branch: string;
  tests_root: string;
  ssh_key_id: string | null;
  ssh_key_name: string | null;
  has_token: boolean;
  /** Set by a successful sync, left untouched by a failed one — so this can still hold a
   *  past timestamp while `sync_error` is populated (synced once, failing now). */
  last_synced_at: string | null;
  /** Sanitized error text of the last failed sync attempt; cleared back to `null` the next
   *  time a sync succeeds. A populated value here must take precedence over any
   *  `last_synced_at` when rendering sync state. */
  sync_error: string | null;
  created_at: string;
  updated_at: string;
}

/** The shape the Create/Edit Test Repository form builds. `CreateTestRepoReq` replaces
 *  `token`/`ssh_key_id` with a single `credential_ref` (X7 — a pasted token becomes a
 *  credstore reference), renames `tests_root`→`content_root`, and requires `default_branch`
 *  where this form's is optional (CONTRACT-DIFF row 33). */
export interface CreateTestRepositoryForm {
  name: string;
  url: string;
  token?: string;
  product_id: string;
  default_branch?: string;
  tests_root?: string;
  ssh_key_id?: string;
}

/** The shape Task 10's adapter returns to components. `SshKeyDto` has no `updated_at` and
 *  adds a `fingerprint` this type doesn't carry (CONTRACT-DIFF row 38). */
export interface SshKeyInfo {
  id: string;
  name: string;
  created_at: string;
  updated_at: string;
}

/** The shape the Create SSH Key form builds. `CreateSshKeyReq` renames
 *  `private_key`→`private_key_pem` (X7 — the name is load-bearing: PEM specifically)
 *  (CONTRACT-DIFF row 39). */
export interface CreateSshKeyForm {
  name: string;
  private_key: string;
}

// Analytics types
/** The shape Task 10's adapter returns to components. `PlanTestAnalyticsDto` renames
 *  `last_run_name`→`last_run_id` (uuid) and adds `last_platform_id` beside `last_platform`;
 *  the read is also bounded to the trailing 90 days, where this type had no window
 *  (CONTRACT-DIFF row 67). */
export interface TestAnalytics {
  test_name: string;
  last_platform: string | null;
  last_version: string | null;
  last_status: string;
  last_run_name: string;
  jira_key: string | null;
  total_runs: number;
  pass_count: number;
  fail_count: number;
}

/** Generated — `PlanBuildDistributionDto` is `{build, total, passed, failed, skipped}`,
 *  field-for-field identical, all `int64` (CONTRACT-DIFF row 68). */
export type BuildDistribution = S['PlanBuildDistributionDto'];

/** The shape Task 10's adapter returns to components (nested in `TestHistory` below).
 *  `PlanTestHistoryEntryDto` renames `run_name`→`run_id` (uuid); `status` is unchanged, and
 *  `build` flips from required (`string | null` here) to optional-and-nullable
 *  (`build?: string | null` there) — CONTRACT-DIFF row 69. */
export interface TestHistoryEntry {
  build: string | null;
  status: string;
  run_name: string;
}

/** The shape Task 10's adapter returns to components — see `TestHistoryEntry` above. */
export interface TestHistory {
  test_name: string;
  results: TestHistoryEntry[];
}

/** UI-only: the analytics `scope`/`group_by` query parameters are plain `string` on the
 *  wire (`AnalyticsOverviewDto.scope`/`.group_by`), not a schema-level enum, so there is no
 *  generated type to alias onto — this is the literal union client code narrows requests to.
 *
 *  **The saved-view scope is a different field and now *is* generated** (Task 20):
 *  `SavedViewDto.scope` publishes `"all" | "plan"` as a schema-level enum, and
 *  `savedViewFromDto` assigns it here without a cast. The two are kept as separate types
 *  because they are separate wire contracts: the analytics query parameters accept their
 *  value trimmed and case-insensitively (`domain::analytics::query::parse_scope`), which a
 *  schema enum would not describe. */
export type AnalyticsScope = 'all' | 'plan';
export type AnalyticsGroupBy = 'none' | 'component' | 'tag' | 'environment';

/** UI-only: a client-side query-parameter builder, not a response body — OpenAPI does not
 *  publish path/query parameters as reusable named schemas, so there is nothing to alias onto.
 *  Parameter names and the three required ones (`product_id`, `version`, `scope`) survive; see
 *  CONTRACT-DIFF §2 fact 4 and row 70 for the `plan_id` value-space change (X6). */
export interface AnalyticsOverviewQuery {
  product_id: string;
  version: string;
  scope: AnalyticsScope;
  plan_id?: string;
  /** Optional git branch filter; omit for "all branches". */
  branch?: string;
  days_heatmap?: number;
  days_trend?: number;
  group_by?: AnalyticsGroupBy;
  group_value?: string;
}

/** UI-only query-parameter builder — see `AnalyticsOverviewQuery` above. Same eight parameters
 *  plus the required `build` (CONTRACT-DIFF row 71). */
export interface AnalyticsBuildTestsQuery {
  product_id: string;
  version: string;
  scope: AnalyticsScope;
  plan_id?: string;
  branch?: string;
  group_by?: AnalyticsGroupBy;
  group_value?: string;
  build: string;
}

/** The shape Task 10's adapter returns to components. `BuildTestDetailDto` renames
 *  `run_name`→`run_id` (uuid) and makes `run_finished_at` required and non-null, unlike
 *  `TestResultDto`'s (CONTRACT-DIFF row 71). */
export interface BuildTestDetailItem {
  test_file: string;
  test_name: string;
  status: string;
  run_name: string;
  run_finished_at: string;
  component: string | null;
  tags: string[];
}

/** The shape Task 10's adapter returns to components. `AnalyticsListItemDto` restructures
 *  `plan_id`→`repo_id`+`plan_path` (X6), `last_run_name`→`last_run_id` (uuid), adds
 *  `last_platform_id`, and makes `case_status`/`case_tickets` required (`case_tickets`
 *  non-null) where this type has them optional (CONTRACT-DIFF row 70). */
export interface AnalyticsListItem {
  test_file: string;
  test_name: string;
  component: string | null;
  tags: string[];
  plan_id: string;
  plan_name: string;
  versions: string[];
  last_status: string;
  last_platform: string | null;
  last_run_name: string | null;
  last_build: string | null;
  last_run_finished_at: string | null;
  pass_count: number;
  fail_count: number;
  skipped_count: number;
  total_runs: number;
  /** Effective per-case status of the latest run (e.g. XFAIL) for dot coloring. */
  case_status?: string | null;
  /** Distinct tickets from the latest run's cases. */
  case_tickets?: string[];
}

/** The shape Task 10's adapter returns to components. `AnalyticsOverviewDto` has no
 *  `product_key`; renames `build_distribution[].latest_run_name`→`latest_run_id`,
 *  `lists[].plan_id`→`repo_id`+`plan_path`, `lists[].last_run_name`→`last_run_id`; adds
 *  `lists[].last_platform_id`; and reshapes `grouped.platform[]` to carry `platform`+
 *  `platform_id` where `grouped.component`/`.tag` keep a bare `value`. `summary`, `heatmap`,
 *  `trend`, `flaky` and `quality_vectors` are otherwise field-identical (CONTRACT-DIFF row
 *  70). See `pages/AnalyticsPage.tsx`'s "no execution data" banner (CONTRACT-DIFF §9) for why
 *  the values, not just the shape, need a human decision Task 11 owns. */
export interface AnalyticsOverview {
  product_id: string;
  product_key: string;
  version: string;
  scope: AnalyticsScope;
  plan_id: string | null;
  branch: string | null;
  group_by: AnalyticsGroupBy;
  group_value: string | null;
  summary: {
    total: number;
    passed: number;
    failed: number;
    not_run: number;
    passed_pct: number;
    failed_pct: number;
    not_run_pct: number;
    case_total: number;
    case_passed: number;
    case_failed: number;
    case_skipped: number;
    case_xfail: number;
    case_xpass: number;
    case_expected: number;
  };
  lists: {
    passed: AnalyticsListItem[];
    failed: AnalyticsListItem[];
    not_run: AnalyticsListItem[];
  };
  heatmap: {
    days: string[];
    rows: Array<{
      test_file: string;
      test_name: string;
      values: string[];
    }>;
  };
  trend: {
    points: Array<{
      day: string;
      passed: number;
      failed: number;
      not_run: number;
    }>;
  };
  build_distribution: Array<{
    build: string;
    latest_run_name: string | null;
    passed: number;
    failed: number;
    executed_total: number;
  }>;
  flaky: Array<{
    test_file: string;
    test_name: string;
    component: string | null;
    tags: string[];
    pass_rate: number;
    executions: number;
    pass_count: number;
    fail_count: number;
    skipped_count: number;
  }>;
  quality_vectors: {
    items: Array<{
      vector: string;
      tests: number;
    }>;
    unclassified_tests: number;
    total_tests: number;
  };
  grouped: {
    component: Array<{
      value: string;
      total: number;
      passed: number;
      failed: number;
      not_run: number;
    }>;
    tag: Array<{
      value: string;
      total: number;
      passed: number;
      failed: number;
      not_run: number;
    }>;
    platform: Array<{
      value: string;
      total: number;
      passed: number;
      failed: number;
      not_run: number;
    }>;
  };
}

/** The shape Task 10's adapter returns to components. `SavedViewDto` restructures
 *  `plan_id`→`plan_path`+`repo_id` (X6); `query_json` stays opaque and unreconciled with the
 *  new vocabulary (CONTRACT-DIFF row 72). */
export interface AnalyticsSavedView {
  id: string;
  owner_id: string;
  scope: AnalyticsScope;
  plan_id: string | null;
  name: string;
  query_json: Record<string, unknown>;
  created_at: string;
  updated_at: string;
}

/** The shape the Create/Edit Saved View form builds — see `AnalyticsSavedView` above
 *  (CONTRACT-DIFF row 73). */
export interface AnalyticsSavedViewPayload {
  name: string;
  scope: AnalyticsScope;
  plan_id?: string;
  query_json: Record<string, unknown>;
}

// JIRA types
/** The shape Task 10's adapter returns to components. `JiraSettingsDto` renames
 *  `api_token`→`api_token_credstore_ref` (X7 — the form field that accepts a pasted API token
 *  becomes a credstore write); `issue_type` is optional both sides but nullable only on the
 *  gear (`issue_type?: string | null` there vs `issue_type?: string` here) — CONTRACT-DIFF
 *  row 76. */
export interface JiraConfig {
  url: string;
  project_key: string;
  email: string;
  api_token: string;
  issue_type?: string;
  enabled: boolean;
}

/** The shape Task 10's adapter returns to components. `JiraBugDto` changes `id` from a
 *  number to a uuid string and restructures `plan_id`→`repo_id`+`plan_path` (X6),
 *  `platform`→`platform_id`; the rest is unchanged (CONTRACT-DIFF row 97). */
export interface JiraBug {
  /** A uuid string, not a number: `JiraBugDto.id` is `string(uuid)` (CONTRACT-DIFF
   *  row 97). */
  id: string;
  jira_key: string;
  test_name: string;
  plan_id: string;
  app_version: string | null;
  platform: string | null;
  status: string;
  summary: string;
  created_at: string;
  resolved_at: string | null;
}

/** Generated — `JiraBugFilingDto` is `{jira_key, created}`, field-for-field identical
 *  (CONTRACT-DIFF row 96). */
export type JiraCreateResponse = S['JiraBugFilingDto'];

/** The shape Task 10's adapter returns to components. `QueueEntryDto` renames
 *  `platform`→`platform_id`, `workflow_name`→`run_id` (uuid), and has no `target_id`
 *  (CONTRACT-DIFF row 14). `ttl_expires_at`/`blocked_by` are already field-identical. */
export interface RunQueueEntry {
  id: string;
  platform: string;
  run_kind: string;
  target_id: string;
  source: string;
  exclusive: boolean;
  /** Generated (Task 20): `QueueEntryDto.state` publishes the seven frozen queue-state
   *  names as a schema-level enum now, so this is aliased onto it rather than re-typed by
   *  hand. The union was previously copied here literally and reached through an `as`
   *  cast in `queueEntryFromDto`; the alias is what makes a gear-side change to the set
   *  a `tsc` failure here instead of a silent divergence. Note `cancelled`, two `l`s —
   *  the *run* state's spelling is `canceled` and X4 records that as deliberate. */
  state: S['QueueStateDto'];
  workflow_name: string | null;
  error: string | null;
  enqueued_at: string;
  dispatched_at: string | null;
  finished_at: string | null;
  /** 1-based FIFO position among this platform's `queued` rows. Null otherwise. */
  queue_position: number | null;
  /** When the TTL sweep will expire this row. Null when not queued, or when expiry is off. */
  ttl_expires_at: string | null;
  /** Server-rendered explanation of why the row is still waiting. Null when not queued. */
  blocked_by: string | null;
}

/** The shape `VariablesEditor.tsx` still renders — kept hand-written, not aliased. `VariableDto`
 *  is `{id, environment_id, name, value}` and carries no `secure` field, and neither does the model
 *  behind it: values are stored and returned in cleartext, with no masking, no credstore
 *  reference and no encrypted column on this surface (unlike every other secret X7 covers).
 *  Task 8a already closed the confidentiality hazard that gap implied — the editor's old
 *  "Secured" checkbox, password input and padlock are gone, replaced by an `UnavailableNotice`
 *  telling the user not to put a credential in a variable (`VariablesEditor.tsx:10-14`, §8-C10). */
export interface PipelineVariable {
  name: string;
  value: string;
  /* No `secure` flag: nothing behind this type stores, masks or encrypts one, so a
   * variable is held and returned in cleartext. The field was removed rather than
   * defaulted — see `REMOVED-SURFACES.md` (Task 8a, C10). */
}

/** The shape Task 10's adapter returns to components. The envelope itself differs completely:
 *  `PUT /qa/v1/variables` upserts one row (plus `DELETE .../{id}`) where this type's `PUT`
 *  replaced the whole set — CONTRACT-DIFF row 83. See `PipelineVariable` above for the `secure`
 *  gap (§8-C10). */
export interface PipelineVariablesConfig {
  variables: PipelineVariable[];
}

/** UI-only: the six event tokens are unchanged on the gear side (CONTRACT-DIFF row 24), but
 *  they appear only as inline `string` properties on generated DTOs, not as a standalone
 *  schema — there is nothing to alias onto. This is the literal union client code narrows to. */
export type ScheduledRunNotificationEvent =
  | 'pending'
  | 'in_progress'
  | 'succeeded'
  | 'failed'
  | 'error'
  | 'skipped';

/** Generated — `ScheduledRunSlackTemplateDto` is `{enabled, status_icon?, header?, summary?,
 *  results?, body?, footer?}`, field-for-field identical (CONTRACT-DIFF row 86). */
export type ScheduledRunSlackTemplate = S['ScheduledRunSlackTemplateDto'];

/** Generated — `ScheduledRunSlackTemplatesDto` is the same six event keys, field-for-field
 *  identical (CONTRACT-DIFF row 86). */
export type ScheduledRunSlackTemplatesConfig = S['ScheduledRunSlackTemplatesDto'];

/** The shape Task 10's adapter returns to components. `NotificationConfigDto` renames
 *  `slack_webhook_url`→`slack_webhook_credstore_ref` (X7) and adds a required
 *  `run_queue_queued_slack_enabled`; every other field, including the whole
 *  `scheduled_run_slack_templates` tree, is field-identical (CONTRACT-DIFF row 86). */
export interface NotificationsConfig {
  slack_webhook_url: string;
  slack_channel: string;
  manager_ui_base_url: string;
  slack_enabled: boolean;
  notify_on_failure: boolean;
  notify_on_success: boolean;
  notify_on_schedule_completion: boolean;
  scheduled_run_slack_enabled: boolean;
  scheduled_run_slack_templates: ScheduledRunSlackTemplatesConfig;
  email_smtp_host: string;
  email_smtp_port: number;
  email_from: string;
  email_recipients: string;
  email_enabled: boolean;
}

/** The shape the notification preview/test forms build. Request shape matches
 *  `NotificationPreviewReq`/`NotificationTestReq` modulo `config`'s own difference — see
 *  `NotificationsConfig` above (CONTRACT-DIFF rows 89, 90). */
export interface ScheduledRunNotificationPreviewRequest {
  config: NotificationsConfig;
  event: ScheduledRunNotificationEvent;
}

/** The shape `SlackMessagePreview.tsx` (`blocks?: Array<Record<string, unknown>> | null`)
 *  needs — kept hand-written rather than aliased to `NotificationPreviewDto`, which is
 *  field-for-field the same names (CONTRACT-DIFF row 89) but **not** the same types:
 *  `blocks: unknown[]` there is not assignable to that component's prop type, and `event` is a
 *  plain `string` there rather than this file's literal union. Aliasing would fail `tsc` at
 *  that component without touching it, which this task may not do. */
export interface ScheduledRunNotificationPreviewResponse {
  event: ScheduledRunNotificationEvent;
  event_label: string;
  rendered_message: string;
  fallback_text: string;
  blocks: Array<Record<string, unknown>>;
}

/** Generated — `JiraPollerConfigDto` is `{poll_interval_seconds, auto_rerun_on_resolve}`,
 *  field-for-field identical, both required (CONTRACT-DIFF row 92). */
export type JiraPollerConfig = S['JiraPollerConfigDto'];

// Coverage types
/** Generated — `CoverageSummaryDto` is `{line_pct, branch_pct, function_pct}`, field-for-field
 *  identical (CONTRACT-DIFF row 60). */
export type CoverageSummary = S['CoverageSummaryDto'];

/** The shape Task 10's adapter returns to components. `CoverageBuildDto` has no `product_id`,
 *  `run_name` or `collected_at` (the last is what `ProductCoverageCard.tsx:18` sorts by) and
 *  adds `build` (the product key and version joined by a slash); `coverage` itself is
 *  unchanged (CONTRACT-DIFF row 60, §8-C8). The array is empty in every deployment today —
 *  nothing in this system measures a coverage point yet. */
export interface ProductCoveragePoint {
  product_id: string;
  product_key: string;
  version: string;
  run_name: string;
  collected_at: string;
  coverage: CoverageSummary;
}

// Helper functions for status colors (matching Rust impl)
//
// `getPhaseColor` used to live here, switching on legacy's capitalised Argo phases.
// It is gone rather than re-taught: `grep -rn "getPhaseColor" src` found its own
// declaration and one comment and nothing else, so teaching it the gears' state set
// would have been teaching a function with no callers. Its live counterpart is
// `isActiveRun` below, and the per-page phase-to-colour maps the pages hold themselves
// (e.g. `PlanDetailPage`'s `runPhaseDot`) — those are Task 11's, not this file's.

export function getStatusColor(status: string): string {
  switch (status) {
    case "PASSED":
      return "bg-green-100 text-green-800";
    case "RUNNING":
      return "bg-blue-100 text-blue-800";
    case "PENDING":
      return "bg-slate-100 text-slate-800";
    case "FAILED":
    case "ERROR":
      return "bg-red-100 text-red-800";
    case "SKIPPED":
      return "bg-yellow-100 text-yellow-800";
    case "XFAIL":
      return "bg-purple-100 text-purple-800";
    case "XPASS":
      return "bg-blue-600 text-white";
    default:
      return "bg-gray-100 text-gray-800";
  }
}

/**
 * Is this run still going to change state on its own?
 *
 * Re-mapped, not re-cased (CONTRACT-DIFF X4). Legacy tested `phase === "Running" ||
 * phase === "Pending"` against Argo's capitalised phases; qa-runs' set is lowercase and
 * is a genuinely different set — `RunDto.state`'s own doc says a client ported from
 * legacy *"must re-map, not merely re-case"*. Legacy had no `created`, `queued`,
 * `dispatching`, `canceled`, `timed_out` or `expired` at all, and the five that mean "not
 * finished yet" are the ones enumerated here.
 *
 * `canceled` (one `l`) is deliberately absent: a cancelled run is finished. It is spelled
 * with one `l` on a run and with two (`cancelled`) on a queue row, and X4 records that
 * difference as intentional — do not normalise the two together.
 *
 * The full run set, for reference: `created | queued | dispatching | running | succeeded |
 * failed | canceled | timed_out | expired | error`.
 *
 * Callers: `pages/RunsPage.tsx` (three), `components/runs/RunsTable.tsx`,
 * `pages/RunDetailPage.tsx`.
 */
const ACTIVE_RUN_STATES = new Set(['created', 'queued', 'dispatching', 'running']);

export function isActiveRun(run: WorkflowRun): boolean {
  return ACTIVE_RUN_STATES.has(run.phase);
}

/**
 * Is this a collect-only enumeration run rather than a test run?
 *
 * `run_kind` is `RunTargetDto.kind` as `runFromDto` carried it over
 * (`adapters.ts`, CONTRACT-DIFF row 5's `run_kind -> target.kind`), so the one
 * value that matters here is qa-runs' own `"collect"` spelling
 * (`RunKind::as_str`, qa-runs-sdk/src/models.rs).
 *
 * WHY THE UI ASKS AT ALL. A collect run runs `pytest --collect-only`, executes
 * no test and records no result — it reports per-file case counts to
 * qa-insights instead. The source system keeps them out of every run listing
 * for that reason (*"Collect-only workflows aren't real test runs"*,
 * `manager/src/services/run_history.rs:382-386`), and one repository's hourly
 * cycle is 24 rows a day that would otherwise crowd out real runs.
 *
 * Callers: `pages/RunsPage.tsx`.
 */
export function isCollectRun(run: WorkflowRun): boolean {
  return run.run_kind === 'collect';
}

/** The shape Task 10's adapter returns to components. `NotificationLogEntryDto` changes `id`
 *  from a number to a uuid string and renames `workflow_name`→`run_id` (a nullable uuid) — the
 *  log line loses its human-readable run name and needs a separate lookup to render one
 *  (CONTRACT-DIFF row 91). `channel`, `event_type`, `outcome`, `detail`, `created_at` unchanged. */
export interface NotificationLogEntry {
  /** A uuid string, not a number (CONTRACT-DIFF row 91). */
  id: string;
  created_at: string;
  /** The run's uuid, or null: `NotificationLogEntryDto` carries `run_id` and no run name
   *  at all, so the log line loses its human-readable label and rendering one would need
   *  a per-line lookup (CONTRACT-DIFF row 91). */
  workflow_name: string | null;
  channel: string;
  event_type: string;
  outcome: string;
  detail: string;
}
