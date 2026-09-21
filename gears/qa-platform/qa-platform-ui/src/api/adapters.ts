// Pure reshape helpers between the gears' wire shapes and the shapes `src/components/`
// and `src/pages/` still consume verbatim.
//
// Every function here is a **pure** transform: no `fetch`, no React, no query client.
// That is deliberate and is what keeps `hooks.ts` a list of calls rather than a mix of
// calls and logic, and it is what makes the reshapes testable at all
// (`adapters.test.ts`). Anything that has to *talk* to a gear — a name→uuid lookup, a
// fan-out over a product's repositories, a read-modify-write — lives in `hooks.ts` and
// calls into here.
//
// The field-level justification for each transform is `gears/qa-platform/docs/DESIGN.md` §3.6,
// cited by row number. Where a legacy field has no gear source at all, the transform
// leaves it absent (`null`/`undefined`/`[]`) and says so — it never fills one with a
// plausible-looking default, because a confident wrong number is worse than a visibly
// missing one.

import type { components } from './generated/openapi';
import type {
  AnalyticsListItem,
  AnalyticsOverview,
  AnalyticsSavedView,
  AnalyticsSavedViewPayload,
  BuildTestDetailItem,
  CreateCustomPlanForm,
  CreateEnvironmentForm,
  CreateProductForm,
  CreateScheduleForm,
  CreateSshKeyForm,
  CreateTestRepositoryForm,
  CustomPlan,
  CustomPlanTest,
  DashboardStats,
  FailedTestCard,
  FlakyTestCard,
  JiraBug,
  JiraConfig,
  NotificationLogEntry,
  NotificationsConfig,
  PipelineVariable,
  EnvironmentDetails,
  EnvironmentInfo,
  Product,
  ProductCoveragePoint,
  RunDetails,
  RunParameter,
  ScheduledRunNotificationEvent,
  ScheduledRunNotificationPreviewResponse,
  ScheduleInfo,
  SshKeyInfo,
  TestAnalytics,
  TestFileInfo,
  TestHistory,
  TestPlanInfo,
  TestRepository,
  TestResult,
  TestRunResult,
  UpdateEnvironmentForm,
  UpdateScheduleNotificationsForm,
  WorkflowRun,
  RunQueueEntry,
} from './types';

type S = components['schemas'];

// ---------------------------------------------------------------------------
// Wire-type aliases.
//
// These were widenings: `generated/openapi.d.ts` predated Task 25's rename and
// the product-plugin columns, so each of these intersected a generated DTO with
// the field it actually carried. The schema is regenerated and declares those
// fields itself, so the intersections are gone and these are plain aliases —
// kept only so the consuming modules keep one import apiece. Inline them freely.
// ---------------------------------------------------------------------------
export type LaunchRunReqWithEnvironmentId = S['LaunchRunReq'];
export type NewScheduleReqWithEnvironmentId = S['NewScheduleReq'];
export type RunDtoWithEnvironmentId = S['RunDto'];
export type ScheduleDtoWithEnvironmentId = S['ScheduleDto'];
export type QueueEntryDtoWithEnvironmentId = S['QueueEntryDto'];
export type TestResultDtoWithEnvironmentId = S['TestResultDto'];
export type DashboardRunDtoWithEnvironmentId = S['DashboardRunDto'];
export type FailedTestCardDtoWithEnvironmentId = S['FailedTestCardDto'];
export type JiraBugDtoWithEnvironmentId = S['JiraBugDto'];
export type PlanTestAnalyticsDtoWithEnvironment = S['PlanTestAnalyticsDto'];
export type AnalyticsListItemDtoWithEnvironment = S['AnalyticsListItemDto'];
export type DashboardStatsDtoWithEnvironmentIds = S['DashboardStatsDto'];
/** `GroupedSummariesDto.environment[]` — an array of
 *  `EnvironmentGroupSummaryDto`, whose id and name fields are `environment_id`
 *  and `environment`. */
export type EnvironmentGroupRow = {
  environment_id: string;
  environment?: string | null;
  total: number;
  passed: number;
  failed: number;
  not_run: number;
};
/** `AnalyticsListsDto`, with its three item arrays widened for `last_environment(_id)`. */
export type AnalyticsListsDtoWithEnvironment = {
  passed: AnalyticsListItemDtoWithEnvironment[];
  failed: AnalyticsListItemDtoWithEnvironment[];
  not_run: AnalyticsListItemDtoWithEnvironment[];
};
export type AnalyticsOverviewDtoWithEnvironmentGroup = Omit<
  S['AnalyticsOverviewDto'],
  'lists' | 'grouped'
> & {
  lists: AnalyticsListsDtoWithEnvironment;
  grouped: {
    component: S['GroupSummaryDto'][];
    tag: S['GroupSummaryDto'][];
    environment?: EnvironmentGroupRow[];
  };
};

// ---------------------------------------------------------------------------
// Pagination envelope (CONTRACT-DIFF X2)
// ---------------------------------------------------------------------------

/** The gears' page envelope. `{ items, page_info }` — **not** OData's `{ value, count }`,
 *  and with no `total` and no page number anywhere in it (`generated/openapi.d.ts`
 *  `PageInfo` and its eight `Page_*` instantiations). */
export interface Page<T> {
  items: T[];
  page_info: S['PageInfo'];
}

/**
 * Unwrap a `Page<T>` into the bare array components expect.
 *
 * The pagination is **discarded** here, and that is a real loss rather than a tidy-up:
 * legacy's pager was page-N-of-M and the gears answer next/previous cursors with no
 * total (X2), so there is nothing to hand a page-number control. A real paged UI would
 * keep `page_info.next_cursor` and drive a next/previous pager from it — that belongs in
 * the components, so it is raised in the report rather than faked here. `total` is
 * emphatically **not** synthesised from `items.length` (§7.3).
 *
 * Tolerates a missing/malformed body by answering `[]`: a component that maps over
 * `undefined` crashes, where one that maps over `[]` renders its empty state.
 */
export function unwrapPage<T>(page: Page<T> | undefined | null): T[] {
  return Array.isArray(page?.items) ? page.items : [];
}

/** The cursor for the next page, or null when this is the last one. Kept separate from
 *  `unwrapPage` so a caller that genuinely wants to walk forward can, without every
 *  caller having to care. */
export function nextCursor(page: Page<unknown> | undefined | null): string | null {
  return page?.page_info?.next_cursor ?? null;
}

/** Largest page size the insights and runs collections honour — both clamp silently at
 *  500 (verified live: `?limit=501` and `?limit=10000` both answer `page_info.limit`
 *  500). Asking for more is not an error, so a caller that wanted a hard bound has to
 *  slice as well (see `recentResultsQuery`). */
export const MAX_PAGE_LIMIT = 500;

// ---------------------------------------------------------------------------
// OData ($filter) literals
// ---------------------------------------------------------------------------

/**
 * Render a value as an OData literal.
 *
 * Two kinds, because the gears' OData layer is **typed** and gets this wrong loudly:
 * a string field wants a single-quoted literal with embedded quotes doubled, and a
 * uuid-typed field wants the uuid **bare**. Driven live during this task:
 *
 *     $filter=run_id eq '1a9adedc-…'  -> 400  "Type mismatch for field run_id: expected Uuid, got string"
 *     $filter=run_id eq 1a9adedc-…    -> 200
 *
 * `CONTRACT-DIFF` §10 listed the `run_id` round trip as inferred-not-driven; this is the
 * concrete form the inference was missing.
 */
export function odataLiteral(value: string, kind: 'string' | 'uuid' = 'string'): string {
  if (kind === 'uuid') {
    return value;
  }
  return `'${value.replace(/'/g, "''")}'`;
}

/** `<field> eq <literal>`, with the literal rendered per `odataLiteral`. */
export function odataEq(field: string, value: string, kind: 'string' | 'uuid' = 'string'): string {
  return `${field} eq ${odataLiteral(value, kind)}`;
}

function clampLimit(limit: number): number {
  if (!Number.isFinite(limit) || limit < 1) {
    return 1;
  }
  return Math.min(Math.floor(limit), MAX_PAGE_LIMIT);
}

/**
 * The query string for `GET /qa/v1/test-results`.
 *
 * Two things about the bound, both load-bearing:
 *
 *  - It is spelled **`limit`**, not `$top`. `$top` is not declared on this route and is
 *    **silently ignored** — verified live in Task 6, where `?$top=1` and `?$top=0` both
 *    returned the full default page of 200. A test asserting `$top=5` would have passed
 *    while the bound had no effect at all.
 *  - `limit` itself is **undeclared in `/openapi.json`** (X2 — it is declared only on
 *    `/qa/v1/queue`). It does work, driven live, but because it is undeclared it is
 *    **not policed by `make ui-contract`**: regenerating the wire types will never tell
 *    us if it goes away.
 *
 * `$filter` on this route allows only `id, run_id, test_file, test_name,
 * run_finished_at`, so the file is the one axis legacy's `?file=` maps onto. There is no
 * chronological `$orderby` (`run_finished_at` is filterable but not sortable), so
 * "recent" here means "the default `id desc` page", which is *stable but arbitrary* — a
 * documented difference from legacy, not something this helper can fix.
 */
export function recentResultsQuery(args: { file: string; limit: number }): string {
  const params = new URLSearchParams();
  params.set('$filter', odataEq('test_file', args.file));
  params.set('limit', String(clampLimit(args.limit)));
  return params.toString();
}

// ---------------------------------------------------------------------------
// Plan identity (CONTRACT-DIFF X6)
// ---------------------------------------------------------------------------

const PLAN_ID_SEPARATOR = '~';

function base64UrlEncode(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = '';
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function base64UrlDecode(text: string): string | null {
  try {
    const padded = text.replace(/-/g, '+').replace(/_/g, '/');
    const binary = atob(padded + '='.repeat((4 - (padded.length % 4)) % 4));
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) {
      bytes[i] = binary.charCodeAt(i);
    }
    return new TextDecoder().decode(bytes);
  } catch {
    return null;
  }
}

/**
 * Pack a gear plan's identity — `(repo_id, path)` — into the single opaque string every
 * component still passes around as `plan_id`.
 *
 * X6: legacy's `plan_id` was a synthetic id; a gear plan has no id at all. Rather than
 * change ~15 component call sites to carry a pair, the pair travels encoded and the
 * adapters own the encoding. The encoding has to survive being dropped into a **URL path
 * segment** — `Link to={`/plans/${plan.id}`}` at `PlanList`, `FlakyTestsCard`,
 * `SchedulesTable` and `RunDetailPage`, read back by `useParams` — and plan paths contain
 * `/` (`plans/smoke.yaml`), so the path half is base64url'd: the result contains only
 * `A-Za-z0-9-_` plus the `~` separator, all unreserved in RFC 3986, so nothing has to be
 * percent-encoded and no proxy gets a chance to normalise a `%2F` away.
 *
 * `~` is safe as the separator because a uuid never contains one and base64url never
 * emits one.
 */
export function encodePlanId(repoId: string, path: string): string {
  return `${repoId}${PLAN_ID_SEPARATOR}${base64UrlEncode(path)}`;
}

/** The inverse of `encodePlanId`. Answers `null` for anything that is not one of ours —
 *  a custom-plan uuid, or a plan id persisted by the legacy backend — so callers can
 *  branch on it rather than guess. */
export function decodePlanId(planId: string | null | undefined): { repo_id: string; path: string } | null {
  if (!planId) {
    return null;
  }
  const cut = planId.indexOf(PLAN_ID_SEPARATOR);
  if (cut <= 0) {
    return null;
  }
  const repoId = planId.slice(0, cut);
  const encoded = planId.slice(cut + PLAN_ID_SEPARATOR.length);
  const path = base64UrlDecode(encoded);
  if (path === null) {
    return null;
  }
  return { repo_id: repoId, path };
}

/**
 * The value the **analytics** routes want in their `plan_id` query parameter.
 *
 * X6's second half: `/qa/v1/analytics/plan/{tests,builds,test-history}` keep the
 * parameter *name* `plan_id` but its *value* is the plan's **path**, "matched across
 * every repository the caller can see" — same name, different value space, and no
 * repository disambiguation. So an encoded id is unpacked to its path here; a string
 * that does not decode is passed through as a path, because a bare path is a legitimate
 * value for these routes (driven live: `?plan_id=plans/smoke.yaml` answers rows).
 */
export function analyticsPlanId(planId: string): string {
  return decodePlanId(planId)?.path ?? planId;
}

// ---------------------------------------------------------------------------
// Run target (CONTRACT-DIFF rows 4, 41, 49)
// ---------------------------------------------------------------------------

/** Fold a `RunTargetDto` back into the opaque `plan_id` + `run_kind` pair legacy carried.
 *  A `custom_plan` target has a real uuid, so that is used verbatim; every other kind is
 *  a `(repo_id, path)` pair and is encoded. */
export function planIdFromTarget(target: S['RunTargetDto'] | undefined | null): string {
  if (!target) {
    return '';
  }
  if (target.custom_plan_id) {
    return target.custom_plan_id;
  }
  if (target.repo_id) {
    return encodePlanId(target.repo_id, target.path ?? '');
  }
  return target.path ?? '';
}

/**
 * Build the `RunTargetDto` for a launch or a schedule from legacy's opaque plan id.
 *
 * `kind` follows the gear's own rule: `test_file` present means a single-test launch
 * (`RunTargetDto.test_file` is *"Required for `test`"*), a custom-plan uuid means
 * `custom_plan` (and `repo_id` alongside it is accepted and ignored, so it is not sent).
 */
export function targetFromPlanId(planId: string, testFile?: string | null): S['RunTargetDto'] {
  const decoded = decodePlanId(planId);
  if (!decoded) {
    return { kind: 'custom_plan', custom_plan_id: planId };
  }
  const file = testFile?.trim() || null;
  return {
    kind: file ? 'test' : 'plan',
    repo_id: decoded.repo_id,
    path: decoded.path,
    ...(file ? { test_file: file } : {}),
  };
}

/**
 * Build a `LaunchRunReq`.
 *
 * **A null `environment_id` is passed through, not refused.** (Renamed from
 * `platform_id` at Task 25; the measurement below predates that rename and is quoted
 * as it was taken.)
 *
 * This function used to throw on one, on the strength of CONTRACT-DIFF row 3's claim
 * that such a run "is accepted and then never queued — a silent dead end". That reading
 * was wrong, and the misreading is specifically of the word *queued*. `admission.rs`
 * does answer `Admission::Unqueued` for a platformless run, but `Unqueued`'s own doc
 * says what it means: *"Dispatch inline with no queue row at all"* — it skips the queue
 * rather than skipping execution, and `launch.rs`'s
 * `(Admission::Unqueued, _) => self.dispatch_and_report(ctx, run, None, &slot)` is where
 * it is dispatched. PRD §5.4, `cpt-cf-qa-fr-runs-queue` states the same intent: *"Runs without a target
 * platform are never queued and never block others."*
 *
 * Measured on the live stack 2026-08-31: a `POST /qa/v1/runs` with `platform_id: null`
 * was accepted, went to `running`, and ran to completion. The gear supports this case
 * deliberately, so the client must not veto it — legacy's platform was optional too.
 *
 * What a platformless run does *not* get is a target environment: the tests fall back to
 * auto-discovery against whatever cluster the runner pod lives in. That is a property of
 * the deployment topology, not something this function can decide.
 */
export function launchReqFromForm(args: {
  planId: string;
  testFile?: string | null;
  platformId: string | null;
  branch?: string | null;
  parameters?: RunParameter[];
  exclusive?: boolean | null;
}): LaunchRunReqWithEnvironmentId {
  return {
    target: targetFromPlanId(args.planId, args.testFile),
    // `''` is the "Default cluster" option's value in both dialogs; it means "no
    // platform" and has to reach the wire as `null`, not as an empty string.
    environment_id: args.platformId || null,
    branch: args.branch?.trim() || null,
    parameters: args.parameters && args.parameters.length > 0 ? args.parameters : [],
    // The tri-state passes straight through: undefined/null both mean "auto", which is
    // what an absent `exclusive` meant on the legacy query string.
    exclusive: args.exclusive ?? null,
  };
}

// ---------------------------------------------------------------------------
// Durations (CONTRACT-DIFF X3, row 5)
// ---------------------------------------------------------------------------

/**
 * The `"2m 5s"` string legacy's `WorkflowRun.duration` carried, derived from the two
 * instants the gear does send. `RunDto` has no `duration` field at all (row 5), and
 * `DashboardRunDto.duration` — which does exist — is documented as exactly this
 * rendering, and as `null` unless the run has both a start and a finish. So this is a
 * reproduction of a known format, not a new one.
 */
export function formatRunDuration(startedAt?: string | null, finishedAt?: string | null): string | null {
  if (!startedAt || !finishedAt) {
    return null;
  }
  const start = Date.parse(startedAt);
  const end = Date.parse(finishedAt);
  if (Number.isNaN(start) || Number.isNaN(end) || end < start) {
    return null;
  }
  const total = Math.round((end - start) / 1000);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  }
  if (minutes > 0) {
    return `${minutes}m ${seconds}s`;
  }
  return `${seconds}s`;
}

// ---------------------------------------------------------------------------
// Runs (CONTRACT-DIFF rows 5, 9, 10)
// ---------------------------------------------------------------------------

/**
 * `RunDto` -> `WorkflowRun`.
 *
 * `state` -> `phase` **without re-casing**: the gears' set is qa-runs' lowercase
 * `created | queued | dispatching | running | succeeded | failed | canceled | timed_out |
 * expired | error`, and `RunDto.state`'s own doc says a client ported from legacy "must
 * re-map, not merely re-case" (X4). Title-casing it here would manufacture phases legacy
 * understood out of states it never had, so `isActiveRun` in `types.ts` learns the real
 * set instead.
 *
 * Fields with no gear source are left `null`, not filled: `product_key` (§7.9 — the gear
 * sends `null` for it on the dashboard and has no field for it here at all), `repo_name`,
 * `source_ref`, `source_ref_kind` and every `slack_*`.
 *
 * `result` passes straight through (Task 10) — `RunsTable.tsx` and `RunDetailPage.tsx` are
 * what make a `succeeded` run's non-zero `skipped` visible now that a skip no longer fails
 * the run itself.
 */
export function runFromDto(dto: RunDtoWithEnvironmentId): WorkflowRun {
  return {
    name: dto.name,
    plan_id: planIdFromTarget(dto.target),
    run_kind: dto.target?.kind,
    phase: dto.state,
    started_at: dto.started_at ?? null,
    finished_at: dto.finished_at ?? null,
    duration: formatRunDuration(dto.started_at, dto.finished_at),
    message: dto.error ?? null,
    app_version: dto.app_version ?? null,
    app_build: dto.app_build ?? null,
    test_version: dto.test_version ?? null,
    // X1: legacy drew a platform *name* here. `RunDto` carries only the uuid (as
    // `environment_id`, renamed from `platform_id` at Task 25) and resolving it is a
    // qa-environments lookup; `hooks.ts` does that where it already holds the
    // environment list, and leaves the uuid otherwise.
    platform: dto.environment_id ?? null,
    product_key: null,
    is_validation: dto.is_validation,
    run_source: dto.source,
    schedule_id: dto.schedule_id ?? null,
    repo_id: dto.target?.repo_id ?? null,
    repo_name: null,
    source_ref: null,
    source_ref_kind: null,
    test_file: dto.target?.test_file ?? null,
    parameters: dto.parameters ?? [],
    exclusive: dto.resolved_exclusive,
    result: dto.result,
  };
}

/** Replace each run's `platform` uuid with the environment's name, where the name is known.
 *  X1 in the one direction the UI actually renders. A uuid with no matching environment is
 *  left as-is rather than blanked — an unresolvable id is still an identifier. */
export function withEnvironmentNames(runs: WorkflowRun[], environmentNameById: Map<string, string>): WorkflowRun[] {
  return runs.map((run) =>
    run.platform && environmentNameById.has(run.platform)
      ? { ...run, platform: environmentNameById.get(run.platform)! }
      : run
  );
}

/** `RunTestResultDto` -> `TestResult`. `logs` is `''` — the empty string is this gear's
 *  honest "no per-test log slice", which §8-C2 records as **not recoverable** (the run's
 *  own SSE stream is the whole run's output, not one test's). `TestResultsTable` already
 *  truthiness-checks it, so `''` renders no error block rather than a fabricated one. */
export function testResultFromDto(dto: S['RunTestResultDto'], cases?: S['TestCaseResultDto'][]): TestResult {
  return {
    name: dto.test_name,
    test_file: dto.test_file,
    status: dto.status,
    duration: dto.duration ?? null,
    logs: '',
    launch_id: dto.launch_id ?? null,
    jira_key: dto.jira_key ?? null,
    ...(cases ? { cases } : {}),
  };
}

/** `RunDetailDto` -> `RunDetails`. The gear's shape is `RunDto` **flattened** (which
 *  since Task 10 carries `result` itself) plus `test_results`; legacy nested the run
 *  under a `run` key, so it is re-nested here (§7.2). `reportportal_url` is gone from
 *  both sides — Task 8 removed the surface — so it is left off rather than sent as `''`.
 *
 *  `casesByFile` carries the per-function breakdown `RunTestResultDto` has no room for;
 *  it is recoverable from `GET /qa/v1/test-case-results` and `hooks.ts` fetches it. */
export function runDetailsFromDto(
  dto: S['RunDetailDto'],
  casesByFile?: Map<string, S['TestCaseResultDto'][]>
): RunDetails {
  return {
    run: runFromDto(dto),
    test_results: (dto.test_results ?? []).map((row) =>
      testResultFromDto(row, casesByFile?.get(row.test_file))
    ),
  };
}

/** Group `test-case-results` rows by their `test_file`, which is how `TestResult.cases`
 *  is scoped (§8-C2 — the case rows exist, just on their own collection). */
export function groupCasesByFile(cases: S['TestCaseResultDto'][]): Map<string, S['TestCaseResultDto'][]> {
  const out = new Map<string, S['TestCaseResultDto'][]>();
  for (const row of cases) {
    const bucket = out.get(row.test_file);
    if (bucket) {
      bucket.push(row);
    } else {
      out.set(row.test_file, [row]);
    }
  }
  return out;
}

/**
 * Flatten an SSE body into the plain log text legacy's `GET /runs/{name}/logs` returned.
 *
 * Row 12: a polled string body became a `text/event-stream` of `RunLogLineDto {line}`.
 * A finished run's stream terminates immediately (driven live: 200, `text/event-stream`,
 * zero bytes for a run with no output), so a plain GET is a complete read for the case
 * the log viewer actually opens. A *live* run's stream stays open, and `useQuery` will
 * simply not start a second request while the first is in flight — that is a real
 * behaviour difference from legacy's 5-second poll and is raised in the report rather
 * than papered over.
 *
 * A body that is not an event stream is passed through unchanged.
 */
export function parseRunLogSse(body: string): string {
  if (!body.includes('data:')) {
    return body;
  }
  const lines: string[] = [];
  for (const frame of body.split('\n')) {
    const trimmed = frame.trimStart();
    if (!trimmed.startsWith('data:')) {
      continue;
    }
    const payload = trimmed.slice('data:'.length).trim();
    if (!payload) {
      continue;
    }
    try {
      const parsed = JSON.parse(payload) as { line?: unknown };
      lines.push(typeof parsed.line === 'string' ? parsed.line : payload);
    } catch {
      lines.push(payload);
    }
  }
  return lines.join('\n');
}

// ---------------------------------------------------------------------------
// Run queue (CONTRACT-DIFF row 14)
// ---------------------------------------------------------------------------

/**
 * `QueueEntryDto` -> `RunQueueEntry`.
 *
 * `state` is passed through **unchanged**, including `cancelled` with two `l`s: X4 records
 * that the queue row's spelling differs from the run's one-`l` `canceled` deliberately,
 * and legacy's own `RunQueueEntry.state` union already used the two-`l` form. Normalising
 * them together would be inventing a single vocabulary the gears do not have. It no longer
 * needs an `as` cast (Task 20): `QueueEntryDto.state` is a schema-level enum, and
 * `RunQueueEntry['state']` is aliased onto it, so the assignment type-checks on its own and
 * a gear-side change to the seven names fails `tsc` here rather than passing through a cast.
 *
 * `target_id` is a **documented substitution**, in the sense of §7.10. `QueueEntryDto`
 * has no target of any kind, and `target_id` is the queued row's primary label at
 * `QueuedRunsCard.tsx:144` (and in both of its confirm dialogs), so leaving it `''` would
 * render an unlabelled row and a confirm reading `Cancel queued run ""?`. The run id is
 * substituted instead: it is a real identifier for the same row, it is not fabricated,
 * and it changes what the column *means* (a run uuid where legacy drew a plan id) — which
 * is why it is recorded as a finding rather than done quietly.
 */
export function queueEntryFromDto(dto: QueueEntryDtoWithEnvironmentId): RunQueueEntry {
  return {
    id: dto.id,
    platform: dto.environment_id,
    run_kind: dto.run_kind,
    target_id: dto.run_id,
    source: dto.source,
    exclusive: dto.exclusive,
    state: dto.state,
    workflow_name: dto.run_id,
    error: dto.error ?? null,
    enqueued_at: dto.enqueued_at,
    dispatched_at: dto.dispatched_at ?? null,
    finished_at: dto.finished_at ?? null,
    queue_position: dto.queue_position ?? null,
    ttl_expires_at: dto.ttl_expires_at ?? null,
    blocked_by: dto.blocked_by ?? null,
  };
}

// ---------------------------------------------------------------------------
// Plans (CONTRACT-DIFF rows 2, 3)
// ---------------------------------------------------------------------------

/** What `hooks.ts` knows about a plan's repository and product, gathered from the lists it
 *  already fetches. Every member is optional because every one of them is a field the
 *  gear's `PlanDto` does not carry. */
export interface PlanContext {
  repoName?: string | null;
  productId?: string | null;
  productKey?: string | null;
  productName?: string | null;
}

function dirNameOf(path: string): string {
  const cut = path.lastIndexOf('/');
  return cut < 0 ? '' : path.slice(0, cut);
}

/**
 * `PlanDto` -> `TestPlanInfo`.
 *
 * `dir_path` is derived from `path` (the same string, minus its last segment) rather than
 * read from a field, which is a rearrangement of data the gear did send. Everything the
 * gear does **not** send is left absent: `versions` is `[]`, `plan.node_selector`,
 * `plan.tolerations`, `plan.validation` and a null `plan.timeout_seconds` are omitted
 * entirely (they are optional on `TestPlan` for exactly this reason), and
 * `plan.description` is `''` — required there because a component collects it into a
 * `string[]`, and `''` is what "no description" has always meant to every reader of it.
 *
 * `source` is `'git'` because in this deployment it can only be git: a repository is
 * creatable from a git URL and nothing else, and the archive-upload route does not exist
 * (§8-C4). That is a fact about the deployment, not a default standing in for a field.
 */
export function planFromDto(dto: S['PlanDto'], ctx: PlanContext = {}): TestPlanInfo {
  return {
    id: encodePlanId(dto.repo_id, dto.path),
    dir_path: dirNameOf(dto.path),
    source: 'git',
    repo_id: dto.repo_id,
    repo_name: ctx.repoName ?? null,
    product_id: ctx.productId ?? null,
    product_key: ctx.productKey ?? null,
    product_name: ctx.productName ?? null,
    versions: [],
    plan: {
      name: dto.name,
      description: '',
      tags: dto.tags ?? [],
      // The key is dropped rather than defaulted when the gear sends null.
      // `PlanDto.timeout_seconds` is `number | null` (`generated/openapi.d.ts:3818-3819`),
      // and a `0` here would read as "this plan times out immediately" to any future
      // consumer — a measurement where the truth is "the manifest did not say".
      // `types.ts`' own comment on `TestPlan.timeout_seconds` names this exact value as
      // the thing not to write, so writing it was a contradiction as well as a defect.
      ...(dto.timeout_seconds != null ? { timeout_seconds: dto.timeout_seconds } : {}),
      exclusive: dto.exclusive ?? null,
    },
    test_files: dto.test_files ?? [],
  };
}

/**
 * `PlanDto` -> one `TestFileInfo` per test file.
 *
 * This is the degraded stand-in for `GET /tests`, which has **no backend at all**
 * (§8-C1): the gears publish 22 qa-catalog operations and none of them is a test-file
 * catalog. What is real here is the path, the plan it belongs to, and the repository and
 * product above it — all of it read off `PlanDto`. What is *not* real is every metadata
 * column the catalog page renders (`title`, `component`, `description`, `quality_vectors`,
 * `versions`, `loc`), so every one of them is left absent and `tags` is `[]` — the plan's
 * own tags are the *plan's*, not this file's, and attributing them here would be the
 * fabrication §8-C1 warns about.
 *
 * §8-C1's actual question — degrade the `/tests` surface to a path list, or remove it the
 * way §6.5 removed three settings pages — is still a human's to answer. This keeps four
 * routed pages working in the meantime instead of 404ing them, and is reported as such.
 */
export function testFilesFromPlan(dto: S['PlanDto'], ctx: PlanContext = {}): TestFileInfo[] {
  const planId = encodePlanId(dto.repo_id, dto.path);
  return (dto.test_files ?? []).map((testFile) => ({
    plan_id: planId,
    plan_name: dto.name,
    source: 'git',
    source_path: null,
    repo_id: dto.repo_id,
    repo_name: ctx.repoName ?? null,
    product_id: ctx.productId ?? null,
    product_key: ctx.productKey ?? null,
    product_name: ctx.productName ?? null,
    test_file: testFile,
    tags: [],
  }));
}

// ---------------------------------------------------------------------------
// Dashboard (CONTRACT-DIFF row 1)
// ---------------------------------------------------------------------------

/** `DashboardRunDto` -> `WorkflowRun`. A ten-field projection, so most of `WorkflowRun`
 *  is absent by construction (row 1 lists what the projection drops) — including
 *  `is_validation`, which is left off rather than sent as `false`: "not a validation run"
 *  and "we were not told" are different claims. `duration` is the gear's own rendered
 *  string here, not derived. `product_key` is bound to what the gear sends, which its own
 *  doc says is *"always `null` today"* (§7.9). */
export function dashboardRunFromDto(dto: DashboardRunDtoWithEnvironmentId): WorkflowRun {
  return {
    name: dto.name,
    plan_id: dto.repo_id ? encodePlanId(dto.repo_id, dto.plan_path ?? '') : (dto.plan_path ?? ''),
    phase: dto.phase,
    started_at: dto.started_at ?? null,
    finished_at: null,
    duration: dto.duration ?? null,
    message: null,
    app_version: dto.app_version ?? null,
    app_build: null,
    test_version: null,
    platform: dto.environment_id ?? null,
    product_key: dto.product_key ?? null,
    repo_id: dto.repo_id ?? null,
  };
}

function failedCardFromDto(dto: FailedTestCardDtoWithEnvironmentId): FailedTestCard {
  return {
    test_name: dto.test_name,
    test_file: dto.test_file ?? null,
    workflow_name: dto.run_id,
    plan_id: dto.repo_id ? encodePlanId(dto.repo_id, dto.plan_path ?? '') : (dto.plan_path ?? ''),
    platform: dto.environment_id ?? null,
    finished_at: dto.finished_at ?? null,
    jira_key: dto.jira_key ?? null,
    launch_id: dto.launch_id ?? null,
  };
}

function flakyCardFromDto(dto: S['FlakyTestCardDto']): FlakyTestCard {
  return {
    test_name: dto.test_name,
    test_file: dto.test_file ?? null,
    plan_id: dto.repo_id ? encodePlanId(dto.repo_id, dto.plan_path ?? '') : (dto.plan_path ?? ''),
    passed: dto.passed,
    failed: dto.failed,
    total: dto.total,
  };
}

/**
 * `DashboardStatsDto` -> `DashboardStats`.
 *
 * `total_plans`, `total_schedules` and `platforms_summary` are **omitted**, not zeroed.
 * `DashboardStatsDto` has no such fields (§8-C3), a `0` next to a "Total plans" label is
 * a confident false claim rather than an empty state, and nothing renders them any more:
 * Task 8a replaced the platform strip with a labelled-unavailable card and the KPI strip
 * never bound the other two. They are optional on `DashboardStats` for this reason.
 *
 * `queued_runs`, which the gear adds, has no legacy field and no consumer, so it is not
 * carried.
 */
export function dashboardFromDto(dto: DashboardStatsDtoWithEnvironmentIds): DashboardStats {
  return {
    total_runs: dto.total_runs,
    active_runs: dto.active_runs,
    recent_runs: (dto.recent_runs ?? []).map(dashboardRunFromDto),
    recent_run_test_trend: (dto.recent_run_test_trend ?? []).map((point) => ({
      run_name: point.run_name,
      started_at: point.started_at ?? null,
      tests_total: point.tests_total,
      passed: point.passed,
      failed: point.failed,
      skipped: point.skipped,
    })),
    daily_test_status_trend: dto.daily_test_status_trend ?? [],
    active_runs_list: (dto.active_runs_list ?? []).map(dashboardRunFromDto),
    failed_recent: (dto.failed_recent ?? []).map(failedCardFromDto),
    failed_24h_count: dto.failed_24h_count,
    failed_prev_24h_count: dto.failed_prev_24h_count,
    pass_rate_24h: dto.pass_rate_24h ?? null,
    pass_rate_prev_24h: dto.pass_rate_prev_24h ?? null,
    flaky_tests: (dto.flaky_tests ?? []).map(flakyCardFromDto),
    quality_vectors_pass_rate: dto.quality_vectors_pass_rate ?? [],
    unattributable_runs: dto.unattributable_runs ?? 0,
  };
}

// ---------------------------------------------------------------------------
// Test results (CONTRACT-DIFF row 7)
// ---------------------------------------------------------------------------

/** `TestResultDto` -> `TestRunResult`. `phase`, `short_error` and `reportportal_url` have
 *  no gear source at all and stay null. Note `started_at` is fed from `run_finished_at`,
 *  which is *a different instant* — row 7 records the swap; there is no run-start on this
 *  DTO. */
export function testRunResultFromDto(dto: TestResultDtoWithEnvironmentId): TestRunResult {
  return {
    workflow_name: dto.run_id,
    plan_id: dto.repo_id ? encodePlanId(dto.repo_id, dto.plan_path ?? '') : (dto.plan_path ?? ''),
    phase: null,
    status: dto.status,
    started_at: dto.run_finished_at ?? null,
    duration: dto.duration ?? null,
    app_version: dto.product_version ?? null,
    test_version: dto.branch ?? null,
    platform: dto.environment_id ?? null,
    short_error: null,
    reportportal_url: null,
  };
}

// ---------------------------------------------------------------------------
// Schedules (CONTRACT-DIFF rows 18-24)
// ---------------------------------------------------------------------------

function tagsToCommaString(tags: string[] | undefined): string | null {
  const joined = (tags ?? []).filter((tag) => tag.trim()).join(',');
  return joined || null;
}

function commaStringToTags(value: string | null | undefined): string[] {
  return (value ?? '')
    .split(',')
    .map((tag) => tag.trim())
    .filter(Boolean);
}

/**
 * `ScheduleDto` -> `ScheduleInfo`.
 *
 * Three inversions in one shape, all of them from row 18:
 *  - `enabled` -> `suspended`, **negated**. Getting this backwards would show every
 *    running schedule as paused and vice versa.
 *  - `include_tags`/`exclude_tags` array -> the comma string the edit form still edits.
 *    An empty array becomes `null`, which is how legacy spelled "no tags" (`''` would
 *    make the form think the user had cleared a value).
 *  - `exclusive_choice: "true" | "false" | "auto"` -> legacy's nullable boolean, where
 *    `null` *is* "auto".
 *
 * `schedule_id` carries the gear's uuid, because every write addresses the row by it
 * while the UI addresses schedules by name (X1). `plan_name`, `product_key`,
 * `product_name` and `description` have no gear field, and `recent_runs` is `[]` — the
 * strip is rebuilt from a separate runs query, not deserialised off this shape.
 */
export function scheduleFromDto(dto: ScheduleDtoWithEnvironmentId): ScheduleInfo {
  return {
    name: dto.name,
    schedule_id: dto.id,
    plan_id: planIdFromTarget(dto.target),
    repo_id: dto.target?.repo_id ?? null,
    plan_name: null,
    plan_type: dto.target?.kind ?? '',
    schedule: dto.cron,
    suspended: !dto.enabled,
    last_scheduled: dto.last_fired_tick ?? null,
    created_at: dto.created_at ?? null,
    platform: dto.environment_id ?? '',
    branch: dto.branch ?? null,
    test_file: dto.target?.test_file ?? null,
    include_tags: tagsToCommaString(dto.include_tags),
    exclude_tags: tagsToCommaString(dto.exclude_tags),
    exclusive: dto.exclusive_choice === 'auto' ? null : dto.exclusive_choice === 'true',
    slack_notifications_enabled: dto.slack_notifications_enabled,
    slack_channel: dto.slack_channel ?? null,
    slack_notification_events: (dto.slack_notification_events ??
      []) as ScheduleInfo['slack_notification_events'],
    recent_runs: [],
    product_key: null,
    product_name: null,
  };
}

/**
 * `CreateScheduleForm` -> `NewScheduleReq`, which `POST /qa/v1/schedules` and
 * `PUT /qa/v1/schedules/{id}` both take.
 *
 * `enabled` is **required and explicit** on every call. `qa-runs/qa-runs/src/api/rest/dto.rs:935-937` explains why an
 * `#[serde(default)]` was refused there: an omitted field would turn every edit that did
 * not show the user the toggle into a silent disable, and a defaulted one into a silent
 * re-enable. So the caller must state it, and `useSuspendSchedule`/`useResumeSchedule`
 * read the schedule first and re-send everything else unchanged (row 21) rather than
 * PUT a partial record onto a route that replaces.
 *
 * `name` comes from `opts.name` when the caller is round-tripping an existing schedule,
 * and falls back to the form's `schedule_id` for a create — legacy's form has no `name`
 * field, and its generated `schedule_id` slug is what the gear's required `name` means.
 *
 * The form's three `slack_*` fields are **not** here: `NewScheduleReq` carries no
 * notification fields at all, so a create that sets them needs a follow-up
 * `PUT .../notifications` (row 19). The caller makes that second call; silently dropping
 * them is the bug row 19 warns about.
 *
 * A **null `environment_id` is passed through** (renamed from `platform_id` at Task 25),
 * for the same reason `launchReqFromForm`
 * stopped refusing one: the claim that such a schedule "saves and then never fires" was
 * a misreading of `Admission::Unqueued`. `qa-runs`' `schedules.rs` states the opposite
 * outright — *"a schedule with no `platform_id` is admitted `Unqueued` before the depth
 * check is reached at all [...] Either kind can exceed twenty in a pass and succeed
 * every time."* A platformless schedule fires, and its run dispatches inline.
 * `useSuspendSchedule`/`useResumeSchedule` do **not** route through this function.
 */
export function scheduleReqFromForm(
  form: CreateScheduleForm,
  opts: { platformId: string | null; enabled: boolean; name?: string }
): NewScheduleReqWithEnvironmentId {
  return {
    name: opts.name ?? form.schedule_id,
    cron: form.cron_expr,
    enabled: opts.enabled,
    target: targetFromPlanId(form.plan_id, form.test_file),
    environment_id: opts.platformId || null,
    branch: form.branch?.trim() || null,
    include_tags: commaStringToTags(form.include_tags),
    exclude_tags: commaStringToTags(form.exclude_tags),
    exclusive_choice:
      form.exclusive === null || form.exclusive === undefined ? 'auto' : String(form.exclusive),
    parameters: [],
  };
}

/** `UpdateScheduleNotificationsForm` -> `UpdateScheduleNotificationsReq`. `slack_enabled`
 *  and `slack_events` are required on the gear where legacy's `channel`/`events` were
 *  optional (row 24), so an absent `events` becomes `[]` — "notify on nothing" — rather
 *  than an omitted key the gear would reject. */
export function scheduleNotificationsReq(
  form: UpdateScheduleNotificationsForm
): S['UpdateScheduleNotificationsReq'] {
  return {
    slack_enabled: form.enabled,
    slack_channel: form.channel ?? null,
    slack_events: form.events ?? [],
  };
}

// ---------------------------------------------------------------------------
// Test repositories (CONTRACT-DIFF rows 29, 30, 33, 35, 37)
// ---------------------------------------------------------------------------

/** `TestRepositoryDto` -> `TestRepository`. `content_root` -> `tests_root`.
 *
 *  **`ssh_key_id` is no longer derived from the DTO; `has_token` now comes from
 *  `has_credential` instead of `credential_ref`.** The DTO used to publish the raw
 *  `credential_ref` and this adapter forwarded it into `ssh_key_id` — the gear's read
 *  DTO no longer does that (review finding #2: a LIST/GET caller could redeem the
 *  credstore reference for the repository's git credentials), so deriving `ssh_key_id`
 *  from it here would just re-open the leak the gear closed. `ssh_key_id` is hardcoded
 *  `null` because nothing reads it: no component prefills an edit form from it. The
 *  gear replaced the reference with a boolean, `has_credential` (a fact, not a
 *  redeemable reference), specifically so `has_token` — which does have a live
 *  consumer, `ProductDetailPage`'s "Auth" column — keeps reporting real state instead
 *  of always reading "Public/none". `source_type` is `'git'` for the reason given on
 *  `planFromDto`. */
export function repoFromDto(dto: S['TestRepositoryDto']): TestRepository {
  return {
    id: dto.id,
    name: dto.name,
    source_type: 'git',
    url: dto.url,
    archive_file_name: null,
    product_id: dto.product_id,
    default_branch: dto.default_branch,
    tests_root: dto.content_root,
    ssh_key_id: null,
    ssh_key_name: null,
    has_token: dto.has_credential,
    last_synced_at: dto.last_synced_at ?? null,
    head_commit: dto.head_commit ?? null,
    sync_error: dto.sync_error ?? null,
    created_at: dto.created_at,
    updated_at: dto.updated_at,
  };
}

/**
 * `CreateTestRepositoryForm` -> `CreateTestRepoReq` (identical to `UpdateTestRepoReq`).
 *
 * **X7 is the one place a rename adapter is wrong**, and this is that place: the form's
 * `token` is a *pasted secret* and the gear's `credential_ref` is a *credstore
 * reference*. Forwarding the paste as the reference would store a credential in a field
 * that is read back to every caller. So a pasted token is refused here, and only an
 * already-referenced credential (`ssh_key_id`) is sent. Wiring the credstore write is not
 * in this task's scope — the refusal is what keeps the gap visible instead of silently
 * leaking.
 *
 * `default_branch` is required on the gear where the form's is optional, so an empty one
 * is refused rather than defaulted to a branch name nobody chose.
 */
export function repoReqFromForm(form: CreateTestRepositoryForm): S['CreateTestRepoReq'] {
  if (form.token?.trim()) {
    throw new Error(
      'Pasted access tokens are not supported yet: this deployment stores a credstore reference, not a secret. Register the credential first and select it, or use a public URL / SSH key.'
    );
  }
  const defaultBranch = form.default_branch?.trim();
  if (!defaultBranch) {
    throw new Error('A default branch is required for a test repository in this deployment.');
  }
  return {
    name: form.name,
    url: form.url,
    product_id: form.product_id,
    default_branch: defaultBranch,
    content_root: form.tests_root ?? '',
    credential_ref: form.ssh_key_id?.trim() || null,
  };
}

// ---------------------------------------------------------------------------
// SSH keys (CONTRACT-DIFF rows 38, 39)
// ---------------------------------------------------------------------------

/**
 * `SshKeyDto` -> `SshKeyInfo`.
 *
 * `updated_at` is set to `created_at`, and that is a **derivation from a route census
 * rather than a copied instant** — review round 1 asked for the field to be dropped
 * instead, and dropping it is wrong here for two independent reasons.
 *
 * First, it is not unknown: qa-environments publishes exactly three ssh-key operations —
 * `GET /qa/v1/ssh-keys`, `POST /qa/v1/ssh-keys`, `DELETE /qa/v1/ssh-keys/{id}` (re-checked
 * against the live document for this round; there is no `PUT` and no `PATCH`) — so a key
 * **cannot** be updated, and `updated_at == created_at` is necessarily true of every key
 * that can exist. That is the same kind of claim as `source: 'git'` above: a fact about
 * what this deployment can do, not a default standing in for a field. Contrast
 * `planFromDto`'s `timeout_seconds`, where `0` is a *different* value from "not stated".
 *
 * Second, the field is **rendered**: `pages/settings/SettingsSshKeysPage.tsx:108` draws
 * `new Date(key.updated_at).toLocaleString()` as the key's only timestamp. Leaving it
 * absent renders the string **"Invalid Date"** on a live page — which is the exact defect
 * class that round's other findings are about, introduced to avoid a value that is true.
 * Making it optional therefore needs a component edit this task may not make.
 */
export function sshKeyFromDto(dto: S['SshKeyDto']): SshKeyInfo {
  return {
    id: dto.id,
    name: dto.name,
    created_at: dto.created_at,
    updated_at: dto.created_at,
  };
}

/** `CreateSshKeyForm` -> `CreateSshKeyReq`. The rename is `private_key` ->
 *  `private_key_pem`, and X7 notes the name is load-bearing: PEM specifically. */
export function sshKeyReqFromForm(form: CreateSshKeyForm): S['CreateSshKeyReq'] {
  return { name: form.name, private_key_pem: form.private_key };
}

// ---------------------------------------------------------------------------
// Custom plans (CONTRACT-DIFF rows 42, 46, 47; Step 1a.1)
// ---------------------------------------------------------------------------

/** `CustomPlanDto` -> `CustomPlan`. `files: {repo_id, plan_path, path}[]` folds back into
 *  legacy's `{plan_id, test_file}[]` (X6). `included_plans`, `nodes`, `parallelism`,
 *  `product_id` and `description` have no gear field (§8-C6, §8-C7) and are left absent —
 *  note this means a saved plan does **not** round-trip its "whole plans" selection: it
 *  comes back as the expanded file list, which is what the gear stores. */
export function customPlanFromDto(dto: S['CustomPlanDto']): CustomPlan {
  return {
    id: dto.id,
    name: dto.name,
    tests: (dto.files ?? []).map((file) => ({
      plan_id: encodePlanId(file.repo_id, file.plan_path ?? ''),
      test_file: file.path,
    })),
    product_id: null,
    created_at: dto.created_at,
  };
}

/** What `expandCustomPlanFiles` needs to look up, supplied by `hooks.ts` because these are
 *  network reads. Kept as an interface so the expansion itself stays pure and testable. */
export interface CustomPlanFileSource {
  /** A standard plan's repository, path and test files, or null when the id names no
   *  standard plan. */
  standardPlan(planId: string): Promise<{ repo_id: string; plan_path: string; test_files: string[] } | null>;
  /** An already-saved custom plan's flat file list, or null when the id names no custom
   *  plan. */
  customPlanFiles(planId: string): Promise<S['CustomPlanFileDto'][] | null>;
  /** An unsaved custom plan's own `included_plans`, when the caller can see them. Used
   *  only to terminate a self-reference; a saved plan has no such field. */
  customPlanIncludes?(planId: string): Promise<string[] | null>;
}

/** The subset of the editor's form `expandCustomPlanFiles` reads. */
export interface CustomPlanFilesInput {
  id?: string;
  tests: CustomPlanTest[];
  included_plans?: string[];
}

/**
 * Expand a custom-plan form into the `files` the gears actually store.
 *
 * **This is not an optimisation.** `UpsertCustomPlanReq` has no `included_plans` field at
 * all, so without this expansion the editor's "Whole plans" tab saves cleanly, the gear
 * stores a plan with an empty file list, and the plan then **runs nothing** — a control
 * that appears to work and does not. Task 8a kept that tab precisely because, unlike a
 * node graph, an included plan *is* expressible in `files`; expanding it is the price.
 *
 * The walk mirrors `src/lib/customPlanTests.ts`'s existing client-side resolution: a
 * standard plan contributes each of its `test_files`, a custom plan contributes its own
 * already-flat `files`, and `expanded` is threaded through so a cycle (A includes B
 * includes A) terminates and a diamond does not double-count. The plan's own id seeds the
 * set, so a self-reference is caught by the same guard.
 *
 * An include that resolves to **nothing** throws. That is the deliberate choice: the
 * failure mode this function exists to prevent is a save that succeeds and runs nothing,
 * and swallowing an unresolvable include would reintroduce exactly that.
 */
export async function expandCustomPlanFiles(
  input: CustomPlanFilesInput,
  source: CustomPlanFileSource
): Promise<S['CustomPlanFileDto'][]> {
  const out: S['CustomPlanFileDto'][] = [];
  const seen = new Set<string>();
  const expanded = new Set<string>(input.id ? [input.id] : []);

  const push = (file: S['CustomPlanFileDto']) => {
    // `\u0000` as the delimiter, spelled as an escape rather than written literally: it is
    // the one character a repository id, a plan path and a file path cannot contain, and a
    // raw NUL byte in the source makes this whole file binary to `grep` (which then
    // silently matches nothing in it). Same convention as `lib/customPlanTests.ts:104`.
    const key = `${file.repo_id}\u0000${file.plan_path ?? ''}\u0000${file.path}`;
    if (!seen.has(key)) {
      seen.add(key);
      out.push(file);
    }
  };

  for (const test of input.tests ?? []) {
    const decoded = decodePlanId(test.plan_id);
    if (!decoded) {
      throw new Error(
        `Cannot save this custom plan: the selected test "${test.test_file}" belongs to plan "${test.plan_id}", which does not resolve to a repository and path in this deployment.`
      );
    }
    push({ repo_id: decoded.repo_id, plan_path: decoded.path, path: test.test_file });
  }

  const visit = async (planId: string): Promise<void> => {
    if (expanded.has(planId)) {
      return;
    }
    expanded.add(planId);

    const standard = await source.standardPlan(planId);
    if (standard) {
      for (const testFile of standard.test_files) {
        push({ repo_id: standard.repo_id, plan_path: standard.plan_path, path: testFile });
      }
      return;
    }

    const customFiles = await source.customPlanFiles(planId);
    if (customFiles) {
      for (const file of customFiles) {
        push(file);
      }
      return;
    }

    const includes = (await source.customPlanIncludes?.(planId)) ?? null;
    if (includes) {
      for (const nested of includes) {
        await visit(nested);
      }
      return;
    }

    throw new Error(
      `Cannot save this custom plan: included plan "${planId}" could not be resolved to any test files, and saving it would produce a plan that runs nothing.`
    );
  };

  for (const includedId of input.included_plans ?? []) {
    await visit(includedId);
  }

  return out;
}

/**
 * `CreateCustomPlanForm` + the expanded files -> `UpsertCustomPlanReq`.
 *
 * `description`, `product_id`, `included_plans`, `nodes` and `parallelism` have nowhere to
 * go (row 46); the first two are already gone from the UI (Task 8a) and the DAG three are
 * §8-C6.
 *
 * **`PUT /qa/v1/custom-plans/{id}` is a replace, not a merge**, and `UpsertCustomPlanReq`
 * declares `tags` and `timeout_seconds` optional (`generated/openapi.d.ts:5264-5270`), so
 * omitting them on an edit **clears** whatever the row held. The UI has never had a field
 * for either, so nothing the user typed can be lost — but something set outside this UI
 * can, which is the same hazard `useUpdateSchedule` and `useUpdateNotificationsConfig`
 * answer by reading first. `existing` is that read: pass the current row's values on an
 * update and they are carried forward untouched; omit it on a create and neither key is
 * sent, so the gear applies its own defaults rather than this adapter inventing an empty
 * tag list or a timeout.
 */
export function customPlanReq(
  form: CreateCustomPlanForm,
  files: S['CustomPlanFileDto'][],
  existing?: Pick<S['CustomPlanDto'], 'tags' | 'timeout_seconds'>
): S['UpsertCustomPlanReq'] {
  return {
    name: form.name,
    files,
    ...(existing?.tags ? { tags: existing.tags } : {}),
    ...(existing?.timeout_seconds != null ? { timeout_seconds: existing.timeout_seconds } : {}),
  };
}

// ---------------------------------------------------------------------------
// Environments (CONTRACT-DIFF rows 50-56)
// ---------------------------------------------------------------------------


/** `EnvironmentDto` -> `EnvironmentInfo`. `observed_version`/`observed_build` are the renames
 *  §6.3 category 2 is about. The observation half is now the plugin's own
 *  `observed_attrs` map plus `health_state`/`health_detail`; `observed_namespace`,
 *  `vhp_base_url` and the five `cluster_*` columns it used to read were dropped by
 *  Task 19, and `ClusterHealth` went with them (user decision U4).
 *  `version_detected_at` and `version_detect_error` are still served directly. `available` IS carried
 *  since Task 21 (a fixed leading column in the environments table);
 *  `kubeconfig_credstore_ref` is not, and Task 19 dropped it anyway. */
type EnvironmentDtoWithPluginFields = S['EnvironmentDto'];

export function environmentFromDto(dto: EnvironmentDtoWithPluginFields): EnvironmentInfo {
  return {
    id: dto.id,
    name: dto.name,
    created_at: dto.created_at,
    description: dto.description ?? '',
    product_id: dto.product_id ?? null,
    // Defaults to true: an environment the gear answers without the field is
    // one from a build predating the column, and "usable" is the reading that
    // does not silently take a working environment out of every run dialog.
    available: dto.available ?? true,
    version: dto.observed_version ?? null,
    build: dto.observed_build ?? null,
    // The plugin's own map, replacing `observed_namespace`/`vhp_base_url`,
    // which Task 19 dropped. `{}` for an environment no cycle has observed --
    // an empty map renders as "not observed" in every descriptor column, which
    // is the honest reading.
    observed_attrs: dto.observed_attrs ?? {},
    health_state: dto.health_state ?? 'unknown',
    health_detail: dto.health_detail ?? null,
    version_detected_at: dto.version_detected_at ?? null,
    version_detect_error: dto.version_detect_error ?? null,
    default_branch: dto.default_branch ?? null,
    // Defaults to false rather than propagating undefined: a gear that predates the
    // column answers without the field, and "not the default" is the correct reading
    // of its absence.
    is_default: dto.is_default ?? false,
  };
}

/**
 * `EnvironmentDto` -> `EnvironmentDetails`.
 *
 * Was, before Task 5, a genuine partial substitute (§7.11): the cluster half had no source
 * anywhere in the gears, so it was left off entirely rather than filled with a `0` or an
 * `"unknown"` (§8-C3). It has no source again, for the opposite reason -- Task 19 dropped
 * the columns and U4 declined to replace the node inventory. What qa-environments observes
 * now is the plugin's own attributes, served on
 * `EnvironmentDto`'s plugin-shaped observation fields, and `environmentFromDto` maps them,
 * so this is a straight reuse rather than a degraded one.
 */
export function environmentDetailsFromDto(dto: EnvironmentDtoWithPluginFields): EnvironmentDetails {
  return environmentFromDto(dto);
}

// The bound a credstore reference used to inherit from its own column:
// `qa_environments.kubeconfig_credstore_ref` was `varchar(1024)` (gears migration
// `m20260814_000006_platform_default_branch.rs`, predating the table's rename to
// `qa_environments`). Task 19's `m20260903_000012` dropped that column -- references
// now live inside the `credentials` JSON array, which imposes no width of its own --
// so this is no longer a schema fact. It is kept as a sanity bound on what this
// adapter will treat as a reference rather than as a pasted document, and 1024 is
// retained so the predicate accepts exactly what the gear used to be able to store.
// Every reference this system actually mints is far shorter (see
// `looksLikeCredstoreReference`'s charset note below).
const MAX_CREDSTORE_REF_LENGTH = 1024;

/** `atob` the value if -- and only if -- it is shaped like base64; `null` otherwise.
 *
 *  m-4 (Task 26 review): this used to cite `CreateEnvironmentDialog.tsx:47` calling
 *  `btoa(kubeconfig.trim())`, but that dialog has carried no `kubeconfig` textarea and no
 *  `btoa` call since Task 22 replaced it with descriptor-driven `CredentialFields` — the
 *  citation was stale even before this file's own rename repointed its path without
 *  re-reading the claim. No current dialog base64-encodes a kubeconfig; the caller this
 *  decode actually guards is `updateEnvironmentReqFromForm`'s `kubeconfig` field, which the
 *  gear's PATCH still accepts as a raw document OR a base64-wrapped one from any API
 *  caller, UI or otherwise -- decoding first is how this layer recovers the document a
 *  caller actually meant when it arrives wrapped.
 *
 *  Deliberately conservative about what it even tries to decode: the value must look like
 *  base64 (alphabet + `=` padding, length a multiple of 4) before `atob` runs. That is a
 *  *shape* test and nothing more -- it does NOT establish that the value was encoded.
 *  Plenty of real credstore references are spelled entirely in base64's alphabet, because
 *  credstore's own charset is `[a-zA-Z0-9_-]` (`credstore_sdk::SecretRef::new`,
 *  `credstore-sdk/src/models.rs`): a bare `Uuid::simple()` reference such as
 *  `0123456789abcdef0123456789abcdef` is 32 base64-alphabet characters with a length
 *  divisible by four and decodes happily to garbage. (An earlier version of this comment
 *  claimed *"every credstore reference in this system contains a character outside that
 *  alphabet"*. It is false, and the code that trusted it corrupted such references --
 *  measured: `teamaprodcluster` was sent as `"\u00b5\u00e6\u00a6j\u00e8u\u00c9n\u00b2\u00d7\u00ab"`.
 *  Whether a decode is believed is decided by `classifyKubeconfigValue`, not here.)
 *
 *  No `TextDecoder` step: `btoa` throws on any code point above U+00FF, so the dialog
 *  cannot produce base64 of non-Latin1 text in the first place (it catches and reports
 *  "Failed to encode kubeconfig"). There is nothing multi-byte to re-decode. */
function decodeIfBase64(value: string): string | null {
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(value) || value.length % 4 !== 0) return null;
  try {
    return atob(value);
  } catch {
    return null;
  }
}

/** True when `text` reads as a pasted kubeconfig: a multi-line YAML mapping.
 *
 *  Both halves earn their place. The **newline** is what separates a document from a
 *  reference (every credstore reference is one line). The **`key:` line** is what
 *  separates a document from an arbitrary multi-line blob -- base64 of a tarball decodes
 *  to multi-line bytes too, and sending that as `kubeconfig` would store nonsense under a
 *  field name that promises YAML.
 *
 *  It does NOT try to decide whether the YAML is a *valid* kubeconfig. That is the
 *  cluster's answer to give, not this adapter's to guess, and a wrong guess here would
 *  refuse a document that works. */
function looksLikeKubeconfigDocument(text: string): boolean {
  return text.includes('\n') && /^[ \t-]*[\w./-]+[ \t]*:/m.test(text);
}

/** True when `text` could be a credstore reference the gear can actually store: one line,
 *  non-empty, inside `MAX_CREDSTORE_REF_LENGTH`, and printable ASCII.
 *
 *  The printable-ASCII half is not decoration. Without it this predicate accepted decoded
 *  binary -- one line of high bytes is still "one line" -- and the adapter would send
 *  `"\u00b5\u00e6\u00a6j\u00e8u\u00c9n\u00b2\u00d7\u00ab"` to the gear as a reference, with no error, for the input
 *  `teamaprodcluster`. Every reference this system actually uses is ASCII: credstore's own
 *  charset is `[a-zA-Z0-9_-]` (`credstore_sdk::SecretRef::new`) and this repo's other
 *  spelling is `credstore://team-a/prod`. */
function looksLikeCredstoreReference(text: string): boolean {
  return (
    !text.includes('\n') &&
    text.length > 0 &&
    text.length <= MAX_CREDSTORE_REF_LENGTH &&
    isPrintableAscii(text)
  );
}

/** True when `text` is plausibly a text document rather than decoded binary: no C0 control
 *  characters beyond tab, newline and carriage return, none of which occur in YAML. */
function isPlausibleText(text: string): boolean {
  // eslint-disable-next-line no-control-regex
  return !/[\x00-\x08\x0b\x0c\x0e-\x1f]/.test(text);
}

/** True when `text` is printable ASCII end to end -- no control bytes, no high bytes, and
 *  no newline either.
 *
 *  This is what decides whether a base64 *decode* is believed. Both values this adapter can
 *  send are ASCII: a kubeconfig is YAML text, and a credstore reference is
 *  `[a-zA-Z0-9_-]` or a `credstore://…` spelling. Base64 of anything else decodes to bytes
 *  spread over the whole 0-255 range, so the chance a short accidental decode is printable
 *  ASCII throughout is roughly `(95/256)^n` -- about five in a million for twelve bytes.
 *  A decode that is not printable ASCII is therefore strong evidence the input was never
 *  encoded at all and merely happens to be spelled in base64's alphabet. */
function isPrintableAscii(text: string): boolean {
  // No `no-control-regex` disable needed here: the class is printable ASCII only. The
  // newline exclusion is deliberate -- a reference is one line, and a document is caught by
  // `looksLikeKubeconfigDocument` before this predicate ever runs.
  return /^[\x20-\x7e]*$/.test(text);
}

/** Classify one candidate value as the document or the reference, or `null` when it is
 *  neither.
 *
 *  Order matters: the document test runs first because a multi-line YAML mapping is the one
 *  thing no credstore reference can be. */
function classifyKubeconfigValue(
  text: string
): { kubeconfig: string } | { kubeconfig_credstore_ref: string } | null {
  if (isPlausibleText(text) && looksLikeKubeconfigDocument(text)) return { kubeconfig: text };
  if (looksLikeCredstoreReference(text)) return { kubeconfig_credstore_ref: text };
  return null;
}

/** The one kubeconfig field the gear should receive, from whatever the form holds.
 *
 *  X7 used to be a **refusal** here: the gear's column held a credstore reference, the
 *  form collected a document, and the adapter threw rather than store a secret in a field
 *  read back to every caller. That refusal fixed the 500 and removed the capability, which
 *  is not what was wanted. The gear now takes a raw `kubeconfig` -- writing it to credstore
 *  first and persisting only the generated reference (`EnvironmentsService::create_environment`,
 *  mirroring `qa-catalog`'s `create_ssh_key`) -- so the document can simply be forwarded,
 *  and the response still carries only the reference.
 *
 *  Both inputs stay supported, because both really occur: a pasted document, and a
 *  reference from a caller who already registered one. The dialog base64-encodes either,
 *  so the decode has to happen before the two can be told apart.
 *
 *  # Why the decode is *tried* and not *assumed*
 *
 *  `decodeIfBase64`'s guard is a shape test, and a short credstore reference can pass it by
 *  coincidence -- credstore's charset (`[a-zA-Z0-9_-]`) is almost a subset of base64's. An
 *  earlier version of this function did `typed = decoded ?? raw` and classified `typed`,
 *  which corrupted exactly those references. Measured against the predicates:
 *  `teamaprodcluster` was sent as `"\u00b5\u00e6\u00a6j\u00e8u\u00c9n\u00b2\u00d7\u00ab"` with no error;
 *  `0123456789abcdef0123456789abcdef`, a bare `Uuid::simple()` reference, likewise;
 *  `platformProd0001`, `prodcluster12345` and `secretRef00001AA` were *thrown* as "decodes
 *  to binary".
 *
 *  So the decode is now one *candidate* among two, and the candidate is only used if it
 *  classifies (`classifyKubeconfigValue`): a decode to bytes that are neither a YAML
 *  document nor printable-ASCII reference text is treated as the coincidence it almost
 *  certainly is, and the value **as typed** is classified instead.
 *
 *  One case is genuinely undecidable and is worth naming rather than hiding: a short
 *  single-line base64 string whose decode is binary is indistinguishable from a reference
 *  that happens to be spelled in base64's alphabet, because that is precisely what both
 *  are. It is forwarded as a reference. The alternative -- throwing -- rejects real
 *  references outright, which is the worse of the two failures: a wrong reference is a
 *  visibly broken platform, while a rejection is a capability the caller cannot use at all.
 *  A base64 blob too long to be a reference still throws, which is the shape that produced
 *  the original 500.
 *
 *  @throws if the value is empty, or is neither a plausible document nor a storable
 *          reference under either reading. */
function kubeconfigFieldsFromFormValue(
  value: string | undefined
): { kubeconfig: string } | { kubeconfig_credstore_ref: string } {
  const raw = value?.trim();
  if (!raw) {
    throw new Error(
      'Paste a kubeconfig, or enter the credstore reference of one already registered.'
    );
  }

  const decoded = decodeIfBase64(raw);
  const fromDecoded = decoded === null ? null : classifyKubeconfigValue(decoded);
  if (fromDecoded) return fromDecoded;

  const asTyped = classifyKubeconfigValue(raw);
  if (asTyped) return asTyped;

  throw new Error(
    'That is neither a kubeconfig document nor a credstore reference the gear can store. ' +
      'Paste the YAML text of the kubeconfig file, or enter a single-line credstore reference of at most ' +
      `${MAX_CREDSTORE_REF_LENGTH} characters.`
  );
}

/** `CreateEnvironmentForm` -> `CreateEnvironmentReq`. The X7 rename is a **forward** now, not a
 *  refusal: see `kubeconfigFieldsFromFormValue`. `namespace`/`vhp_base_url` have no gear
 *  field and are dropped (CONTRACT-DIFF row 52). */
/** `CreateEnvironmentReq.credentials` -- the plugin-shaped map Task 18b Step 2
 *  added, and absent from the stale generated schema. See
 *  `EnvironmentDtoWithPluginFields` for why this is declared rather than cast. */
type CreateEnvironmentReqWithCredentials = S['CreateEnvironmentReq'] & {
  credentials?: Record<string, { material: string }>;
};

export function createEnvironmentReqFromForm(
  form: CreateEnvironmentForm,
): CreateEnvironmentReqWithCredentials {
  return {
    name: form.name,
    // **The plugin-shaped map, and nothing else.** Task 18b left the
    // pre-plugin `kubeconfig`/`kubeconfig_credstore_ref` pair accepted
    // precisely so this task could be the change that stops sending it, and
    // this is that change. The gear still folds the pair for any other client
    // until a follow-up deletes it from the DTOs.
    credentials: form.credentials,
    description: form.description ?? null,
    product_id: form.product_id ?? null,
    default_branch: form.default_branch ?? null,
    is_default: form.is_default ?? false,
  };
}

/** `UpdateEnvironmentForm` -> `UpdateEnvironmentReq`. A true partial (`PATCH`), so only the keys
 *  the form actually carries are sent — an omitted key leaves the field alone, unlike the
 *  schedules PUT. `namespace`/`vhp_base_url` have no gear field and are dropped. */
export function updateEnvironmentReqFromForm(form: UpdateEnvironmentForm): S['UpdateEnvironmentReq'] {
  const req: S['UpdateEnvironmentReq'] = {};
  if (form.description !== undefined) req.description = form.description;
  if (form.product_id !== undefined) req.product_id = form.product_id;
  if (form.default_branch !== undefined) req.default_branch = form.default_branch;
  // Absent leaves the flag alone; the gear reads `Some(true)` as "promote, demoting the
  // product's previous default" and `Some(false)` as "clear it on this environment".
  if (form.is_default !== undefined) req.is_default = form.is_default;
  // A patch that does not mention the kubeconfig leaves the stored one alone, so this key
  // is set only when the form carries it. No component supplies it today -- the Edit
  // Environment dialog has no kubeconfig field -- but the gear's PATCH accepts a pasted
  // replacement, and an adapter that could not express it would put the capability out of
  // this layer's reach entirely.
  //
  // A blank value is "unchanged", not an error. An edit form binds a textarea to a string,
  // so an untouched kubeconfig field arrives as `''`; on create that is the required-field
  // refusal `kubeconfigFieldsFromFormValue` throws, but on a PATCH there is nothing to
  // require -- and no way to spell "clear the kubeconfig" either, since the gear's column is
  // `NOT NULL`. Refusing the whole update because one field the caller never touched is
  // empty would be a false rejection.
  if (form.kubeconfig !== undefined && form.kubeconfig.trim() !== '') {
    Object.assign(req, kubeconfigFieldsFromFormValue(form.kubeconfig));
  }
  return req;
}

// ---------------------------------------------------------------------------
// Observed versions (CONTRACT-DIFF rows 65, 66; §5-E, §7.10)
// ---------------------------------------------------------------------------

/** One environment's observed version, with its build alongside. */
export interface ObservedEnvironmentVersion {
  environment_id: string;
  version: string;
  build: string | null;
}

/**
 * Derive observed versions from the environment DTO's `observed_version`/`observed_build`.
 *
 * An environment with a null `observed_version` is **dropped**, not rendered as `"null"` or
 * `""` — an empty version dropdown is a true statement about a deployment that observes
 * no versions, and §9 already decided the Analytics page labels that rather than
 * inventing a value. Do **not** hardcode a fallback such as `unknown`: the smoke script
 * passes `version=unknown` as a probe, and shipping it as a default would present a
 * fabricated filter as a real one.
 *
 * The `build` is carried but is **never** used as a branch. §5-E: §6.3 category 2 sent
 * both `observed-versions` and `observed-branches` to these two fields, and a build is
 * not a branch. `useObservedProductBranches` is fed from the repository's git branches
 * instead (§7.10).
 */
export function observedFromEnvironments(
  environments: Array<{ id: string; observed_version?: string | null; observed_build?: string | null }>
): ObservedEnvironmentVersion[] {
  const out: ObservedEnvironmentVersion[] = [];
  for (const environment of environments) {
    const version = environment.observed_version?.trim();
    if (!version) {
      continue;
    }
    out.push({ environment_id: environment.id, version, build: environment.observed_build ?? null });
  }
  return out;
}

/** Distinct observed versions, sorted, for the Analytics version dropdown. Empty in every
 *  deployment this plan produces — see §9.1 — and honestly so. */
export function distinctObservedVersions(
  environments: Array<{ id: string; observed_version?: string | null; observed_build?: string | null }>
): string[] {
  return [...new Set(observedFromEnvironments(environments).map((entry) => entry.version))].sort((a, b) =>
    a.localeCompare(b)
  );
}

// ---------------------------------------------------------------------------
// Products and coverage (CONTRACT-DIFF rows 58, 60, 62)
// ---------------------------------------------------------------------------

/** `ProductDto` -> `Product`. `folder` -> `tests_folder`, and the nullability flips: the
 *  gear's is nullable where legacy's was required, so a null folder becomes `''` — the
 *  same "no folder" the required field always spelled. */
/** `ProductDto.plugin_instance_id` -- required on the gear since Task 20a, and
 *  absent from the stale generated schema. See `EnvironmentDtoWithPluginFields`. */
type ProductDtoWithPlugin = S['ProductDto'] & { plugin_instance_id?: string | null };

export function productFromDto(dto: ProductDtoWithPlugin): Product {
  return {
    id: dto.id,
    name: dto.name,
    key: dto.key,
    description: dto.description,
    plugin_instance_id: dto.plugin_instance_id ?? null,
    tests_folder: dto.folder ?? '',
    created_at: dto.created_at,
    updated_at: dto.updated_at,
  };
}

/** `CreateProductReq.plugin_instance_id` -- absent from the stale generated
 *  schema (same F-21 reasoning as `ProductDtoWithPlugin` above, the read
 *  path's mirror) and **optional here**, deliberately unlike `ProductDto`'s:
 *  see `productReqFromForm`'s `currentBinding` parameter (ruling G-2) for why
 *  omitting the key, not sending an empty or repeated one, is what this type
 *  has to allow. */
type CreateProductReqWithPlugin = S['CreateProductReq'] & { plugin_instance_id?: string };

/** `CreateProductForm` -> `CreateProductReq` (identical to `UpdateProductReq`).
 *  `description` is required on the gear where the form's is optional, so an absent one
 *  becomes `''` rather than an omitted key the gear would reject.
 *
 *  `plugin_instance_id` is **not** just passed through -- ruling **G-2**. `qa-catalog`
 *  re-validates *any* `Some(id)` it receives against the live per-process plugin registry
 *  (`domain/service/products.rs:165-167`); it does not compare the incoming id to what is
 *  already stored. So resending a product's own current binding on every save is not a
 *  no-op -- it is a rebind request that must re-resolve, and it 400s for any product bound
 *  to a plugin this deployment does not run (every product `m20260903_000003` backfilled to
 *  VHP, on a non-VHP deployment, for instance). Ruling **D-18** made `None` mean "leave the
 *  binding alone" for exactly this reason; the key has to be omitted, not sent as `''` or
 *  `null` -- an empty string is not "no selection" to the gear, it is a value, and
 *  `validation.rs:83-100` rejects it explicitly, which is a worse failure than the one
 *  D-18 exists to avoid.
 *
 *  `currentBinding` is the product's *stored* `plugin_instance_id` (omitted entirely on
 *  create, where there is no prior binding to compare against -- Step 3's guard already
 *  guarantees `form.plugin_instance_id` is non-empty there, so it is always sent). The key
 *  is included only when the form is naming a plugin genuinely different from what is
 *  already stored -- a deliberate rebind -- or omitted otherwise, which covers both "the
 *  form has no selection" and "the form still names the same plugin".
 */
export function productReqFromForm(
  form: CreateProductForm,
  currentBinding?: string | null
): CreateProductReqWithPlugin {
  const req: CreateProductReqWithPlugin = {
    name: form.name,
    key: form.key,
    description: form.description ?? '',
    folder: form.tests_folder?.trim() || null,
  };
  if (form.plugin_instance_id && form.plugin_instance_id !== currentBinding) {
    req.plugin_instance_id = form.plugin_instance_id;
  }
  return req;
}

/**
 * `CoverageBuildDto[]` -> `ProductCoveragePoint[]`, **narrowed to one product**.
 *
 * `GET /qa/v1/dashboard/coverage` takes no parameters and answers one point per product for
 * the whole deployment, so the narrowing has to happen here — and it has to happen at all,
 * because the only consumer is a **per-product** card whose own caption reads *"Coverage
 * belongs to this product and is calculated per reported product version."*
 * (`ProductCoverageCard.tsx:96`). Handing that card every product's rows would make the
 * caption false and put another product's numbers in this product's chart. This is §7.6's
 * pattern — a client-side filter where a server filter never existed — and it is safe for
 * the same reason rows 29 and 50 are: the key is on the row. The gear sends `product_key`,
 * not `product_id`, so the caller resolves the id to a key first.
 *
 * The array is **empty in every deployment today** — the endpoint's own published doc says
 * nothing in this system measures a coverage point and no number is folded out of ingested
 * results to fill the gap (§8-C8) — and the live call site already renders an honest empty
 * state, so no zero is ever displayed. But that is a *deployment* fact, not a code
 * guarantee, which is why the filter is here rather than resting on the emptiness.
 *
 * `run_name` and `collected_at` are absent from the DTO and stay `''` — emphatically not
 * back-filled with `Date.now()`, which would fabricate a measurement time. `collected_at`
 * is both sorted on and rendered as a date (`ProductCoverageCard.tsx:18` and `:114`), so on
 * a deployment that ever measures coverage that column reads "Invalid Date" and the sort
 * comparator returns `NaN`. That needs a component edit this task may not make; it is
 * recorded as a §8-C8 consequence in `CONTRACT-DIFF` §11.9. `product_id` is the product
 * asked for — the caller's own input, echoed back, not a value invented for the row.
 */
export function coveragePointsFromDto(
  rows: S['CoverageBuildDto'][],
  product: { id: string; key: string } | undefined
): ProductCoveragePoint[] {
  if (!product) {
    return [];
  }
  return rows
    .filter((row) => row.product_key === product.key)
    .map((row) => ({
      product_id: product.id,
      product_key: row.product_key,
      version: row.version,
      run_name: '',
      collected_at: '',
      coverage: row.coverage,
    }));
}

// ---------------------------------------------------------------------------
// Analytics (CONTRACT-DIFF rows 67-75)
// ---------------------------------------------------------------------------

/** `PlanTestAnalyticsDto` -> `TestAnalytics`. `last_run_name` is now the run's uuid; the
 *  gear keeps `last_environment` as a name *and* adds `last_environment_id` (renamed from
 *  `last_platform`/`last_platform_id` at Task 25), so the name is used and the uuid
 *  dropped. The read is bounded to the trailing 90 days where legacy had no window (row
 *  67) — a semantic difference no adapter can undo. */
export function testAnalyticsFromDto(dto: PlanTestAnalyticsDtoWithEnvironment): TestAnalytics {
  return {
    test_name: dto.test_name,
    last_platform: dto.last_environment ?? null,
    last_version: dto.last_version ?? null,
    last_status: dto.last_status,
    last_run_name: dto.last_run_id,
    jira_key: dto.jira_key ?? null,
    total_runs: dto.total_runs,
    pass_count: dto.pass_count,
    fail_count: dto.fail_count,
  };
}

/** `PlanTestHistoryDto` -> `TestHistory`. `run_name` is now a uuid (row 69). */
export function testHistoryFromDto(dto: S['PlanTestHistoryDto']): TestHistory {
  return {
    test_name: dto.test_name,
    results: (dto.results ?? []).map((entry) => ({
      build: entry.build ?? null,
      status: entry.status,
      run_name: entry.run_id,
    })),
  };
}

/** `BuildTestDetailDto` -> `BuildTestDetailItem`. `run_name` -> the run uuid (row 71). */
export function buildTestDetailFromDto(dto: S['BuildTestDetailDto']): BuildTestDetailItem {
  return {
    test_file: dto.test_file,
    test_name: dto.test_name,
    status: dto.status,
    run_name: dto.run_id,
    run_finished_at: dto.run_finished_at,
    component: dto.component ?? null,
    tags: dto.tags ?? [],
  };
}

function analyticsListItemFromDto(dto: AnalyticsListItemDtoWithEnvironment): AnalyticsListItem {
  return {
    test_file: dto.test_file,
    test_name: dto.test_name,
    component: dto.component ?? null,
    tags: dto.tags ?? [],
    plan_id: encodePlanId(dto.repo_id, dto.plan_path),
    plan_name: dto.plan_name,
    versions: dto.versions ?? [],
    last_status: dto.last_status,
    last_platform: dto.last_environment ?? null,
    last_run_name: dto.last_run_id ?? null,
    last_build: dto.last_build ?? null,
    last_run_finished_at: dto.last_run_finished_at ?? null,
    pass_count: dto.pass_count,
    fail_count: dto.fail_count,
    skipped_count: dto.skipped_count,
    total_runs: dto.total_runs,
    case_status: dto.case_status ?? null,
    case_tickets: dto.case_tickets ?? [],
  };
}

/**
 * `AnalyticsOverviewDto` -> `AnalyticsOverview`.
 *
 * Mostly field-identical; three reshapes:
 *  - `product_key` is absent from the DTO, so it is `''` (the page renders it as a label
 *    beside a product it already knows).
 *  - `build_distribution[].latest_run_name` is now `latest_run_id`.
 *  - `grouped.environment[]` (renamed from `grouped.platform[]` at Task 25) carries
 *    `environment` (a nullable *name*) plus `environment_id`, where `grouped.component`/
 *    `.tag` keep a bare `value`. The three are one chart in the UI, so the environment
 *    arm is projected onto `value` using the name, falling back to the uuid when the name
 *    is null — an unresolved environment is still identifiable, and blanking it would
 *    silently merge every unnamed environment into one bar.
 *
 * `scope` no longer needs an `as AnalyticsScope` cast (Task 20 fix round): the gear
 * publishes the echoed scope as a two-value schema enum, and `AnalyticsScope` is aliased
 * onto it, so a gear-side change to the pair fails `tsc` here instead of passing through
 * a cast. `group_by` still needs its cast — `AnalyticsOverviewDto.group_by` is still a
 * `String` on the wire.
 *
 * §9 is about the *values* on this shape, not the shape: every execution-scoped counter
 * is 0 by construction in this deployment, and the decision recorded there is a banner in
 * `pages/AnalyticsPage.tsx` (Task 11's), not a number invented here.
 */
export function analyticsOverviewFromDto(
  dto: AnalyticsOverviewDtoWithEnvironmentGroup,
  requested: { plan_id?: string }
): AnalyticsOverview {
  return {
    product_id: dto.product_id,
    product_key: '',
    version: dto.version,
    scope: dto.scope,
    // The DTO echoes back the *path* it was given (X6); the components hand this straight
    // back into `plan_id`-shaped props, so the caller's own opaque id is preserved when
    // there is one.
    plan_id: requested.plan_id ?? dto.plan_id ?? null,
    branch: dto.branch ?? null,
    group_by: dto.group_by as AnalyticsOverview['group_by'],
    group_value: dto.group_value ?? null,
    summary: dto.summary,
    lists: {
      passed: (dto.lists?.passed ?? []).map(analyticsListItemFromDto),
      failed: (dto.lists?.failed ?? []).map(analyticsListItemFromDto),
      not_run: (dto.lists?.not_run ?? []).map(analyticsListItemFromDto),
    },
    heatmap: dto.heatmap,
    trend: dto.trend,
    build_distribution: (dto.build_distribution ?? []).map((row) => ({
      build: row.build,
      latest_run_name: row.latest_run_id ?? null,
      passed: row.passed,
      failed: row.failed,
      executed_total: row.executed_total,
    })),
    flaky: (dto.flaky ?? []).map((row) => ({
      test_file: row.test_file,
      test_name: row.test_name,
      component: row.component ?? null,
      tags: row.tags ?? [],
      pass_rate: row.pass_rate,
      executions: row.executions,
      pass_count: row.pass_count,
      fail_count: row.fail_count,
      skipped_count: row.skipped_count,
    })),
    quality_vectors: dto.quality_vectors,
    grouped: {
      component: dto.grouped?.component ?? [],
      tag: dto.grouped?.tag ?? [],
      platform: (dto.grouped?.environment ?? []).map((row) => ({
        value: row.environment ?? row.environment_id,
        total: row.total,
        passed: row.passed,
        failed: row.failed,
        not_run: row.not_run,
      })),
    },
  };
}

/** `SavedViewDto` -> `AnalyticsSavedView`. `(repo_id, plan_path)` folds back into the
 *  opaque `plan_id` (X6). `query_json` is passed through untouched — the gear never
 *  inspects it, so a legacy `plan_id` buried inside one is **not** reconciled with the new
 *  vocabulary (row 73), which is a real and reported limitation rather than something to
 *  rewrite blindly.
 *
 *  `scope` no longer needs an `as AnalyticsScope` cast (Task 20): the gear publishes it as
 *  a two-value schema enum (`SavedViewScopeDto`), so `"all" | "plan"` is what the generated
 *  type already says. It is a *different* schema from `AnalyticsScopeDto`, which is what
 *  `AnalyticsScope` is aliased onto — same value space, separate contracts — and the two
 *  assign into each other because they are structurally identical. */
export function savedViewFromDto(dto: S['SavedViewDto']): AnalyticsSavedView {
  return {
    id: dto.id,
    owner_id: dto.owner_id,
    scope: dto.scope,
    plan_id: dto.repo_id && dto.plan_path ? encodePlanId(dto.repo_id, dto.plan_path) : null,
    name: dto.name,
    query_json: (dto.query_json ?? {}) as Record<string, unknown>,
    created_at: dto.created_at,
    updated_at: dto.updated_at,
  };
}

/** `AnalyticsSavedViewPayload` -> `NewSavedViewReq`. `(repo_id, plan_path)` must be
 *  supplied **together** (X6), so an id that does not decode sends neither half rather
 *  than one and a 400. */
export function savedViewReqFromPayload(payload: AnalyticsSavedViewPayload): S['NewSavedViewReq'] {
  const decoded = payload.plan_id ? decodePlanId(payload.plan_id) : null;
  return {
    name: payload.name,
    scope: payload.scope,
    query_json: payload.query_json,
    repo_id: decoded?.repo_id ?? null,
    plan_path: decoded?.path ?? null,
  };
}

// ---------------------------------------------------------------------------
// JIRA (CONTRACT-DIFF rows 76, 77, 96, 97)
// ---------------------------------------------------------------------------

/** `JiraSettingsDto` -> `JiraConfig`. X7: `api_token` is now
 *  `api_token_credstore_ref`, so what the form shows is a *reference*, never a token. */
export function jiraConfigFromDto(dto: S['JiraSettingsDto']): JiraConfig {
  return {
    url: dto.url,
    project_key: dto.project_key,
    email: dto.email,
    api_token: dto.api_token_credstore_ref,
    issue_type: dto.issue_type ?? undefined,
    enabled: dto.enabled,
  };
}

/** `JiraConfig` -> `JiraSettingsDto`. The `api_token` field carries a credstore
 *  **reference** in this deployment, and whatever the form holds is forwarded as one.
 *
 *  Unlike the kubeconfig case below, nothing is refused here, and that is a limit rather
 *  than a choice: a credstore reference and an API token are both opaque single-line
 *  strings, so there is no signal to refuse on. The gap is therefore in the **label** —
 *  a field captioned "API token" that in fact stores a reference — which is a copy change
 *  in `SettingsJiraPage`, not something an adapter can fix. Reported as an X7 follow-up.
 *  Note the failure is not a leak: a pasted token would be stored as a reference that
 *  resolves to nothing, so JIRA calls fail rather than the token being served back as a
 *  secret. */
export function jiraConfigReq(config: JiraConfig): S['JiraSettingsDto'] {
  return {
    url: config.url,
    project_key: config.project_key,
    email: config.email,
    api_token_credstore_ref: config.api_token,
    issue_type: config.issue_type ?? null,
    enabled: config.enabled,
  };
}

/** `JiraBugDto` -> `JiraBug`. `id` changes from a number to a uuid, so `JiraBug.id` is a
 *  string now; `(repo_id, plan_path)` folds into `plan_id` and `environment_id` (renamed
 *  from `platform_id` at Task 25) into `platform` (row 97). */
export function jiraBugFromDto(dto: JiraBugDtoWithEnvironmentId): JiraBug {
  return {
    id: dto.id,
    jira_key: dto.jira_key,
    test_name: dto.test_name,
    plan_id: encodePlanId(dto.repo_id, dto.plan_path),
    app_version: dto.app_version ?? null,
    platform: dto.environment_id ?? null,
    status: dto.status,
    summary: dto.summary,
    created_at: dto.created_at,
    resolved_at: dto.resolved_at ?? null,
  };
}

/**
 * The query string for `GET /qa/v1/jira/open-bugs`.
 *
 * `repo_id` and `plan_path` are a **co-requirement**: `/openapi.json` marks neither
 * individually required, and both readings are right. Driven live during this task:
 *
 *     (neither)                -> 200 []
 *     ?repo_id=<uuid>          -> 400 "repo_id and plan_path must be supplied together, or not at all"
 *     ?plan_path=x             -> 400 (same violation)
 *     ?repo_id=<uuid>&plan_path=x -> 200
 *
 * So the hook's "omit for all bugs" branch has to omit **both**, and a plan id that does
 * not decode into a pair sends neither rather than half of one and a guaranteed 400.
 */
export function openBugsQuery(planId?: string | null): string {
  const decoded = planId ? decodePlanId(planId) : null;
  if (!decoded) {
    return '';
  }
  const params = new URLSearchParams();
  params.set('repo_id', decoded.repo_id);
  params.set('plan_path', decoded.path);
  return params.toString();
}

// ---------------------------------------------------------------------------
// Variables (CONTRACT-DIFF rows 82-85, §8-C10)
// ---------------------------------------------------------------------------

/**
 * Keep only the rows that belong to the scope the caller asked for.
 *
 * `GET /qa/v1/variables?environment_id=<uuid>` is **additive**, not a filter: its own doc
 * says "List global pipeline variables, **plus** an environment's variables when
 * `environment_id` is given", and that is what it does. Driven live during this task with
 * three seeded rows (field named `platform_id` at the time; qa-environments' Environment
 * rename later carried this query parameter and `VariableDto`/`UpsertVariableReq` field
 * to `environment_id`, with no behaviour change — the 404 body's wording changed with it,
 * from "Platform … was not found" to "Environment … was not found"
 * (`api/rest/error.rs`'s `PlatformResourceError::not_found`), quoted below as it reads now):
 *
 *     (no environment_id)   -> [T10_GLOBAL]
 *     ?environment_id=<p1>  -> [T10_GLOBAL, T10_P1]
 *     ?environment_id=<p2>  -> [T10_GLOBAL, T10_P2]
 *     ?environment_id=<unknown uuid> -> 404 "Environment … was not found"
 *     ?environment_id=not-a-uuid     -> 400
 *
 * So the parameter is genuinely honoured (a 404 and a 400 prove it is read, not ignored),
 * *and* §7.6's client-side partition is still needed — legacy's
 * `/platforms/{name}/variables` returned the platform's **own** set, and the server
 * filter alone answers a superset of it.
 *
 * There is no `secure` flag on either side any more: §8-C10, closed by Task 8a by
 * removing the affordance rather than by defaulting the field.
 */
export function partitionEnvironmentVariables(
  rows: S['VariableDto'][],
  environmentId: string | null
): PipelineVariable[] {
  return rows
    .filter((row) => (environmentId ? row.environment_id === environmentId : !row.environment_id))
    .map((row) => ({ name: row.name, value: row.value }));
}

/** One upsert-and-delete plan for the variables editor. */
export interface VariableWritePlan {
  upserts: S['UpsertVariableReq'][];
  deletes: string[];
}

/**
 * Turn the whole-set save the editor performs into the per-item writes the gear takes.
 *
 * The granularity **inverts** here (row 83): legacy's `PUT /settings/variables` replaced
 * the entire set atomically, and the gear upserts one row at a time by natural key with a
 * separate `DELETE /qa/v1/variables/{id}`. So the editor's full list is diffed against
 * the last read and turned into one PUT per changed row plus one DELETE per removed row.
 *
 * **This is not atomic, and cannot be made atomic.** A failure part-way leaves the set
 * half-written, which the whole-set PUT could not do. The caller surfaces the error; there
 * is no rollback to offer.
 *
 * The key is the **name**, which is what the gear upserts on, so a renamed row is a delete
 * plus an insert rather than an update — the same thing the gear would do. A blank name is
 * skipped rather than written, because an unnamed variable is a row nothing can ever
 * reference.
 */
export function variableWritePlan(
  previous: S['VariableDto'][],
  next: PipelineVariable[],
  environmentId: string | null
): VariableWritePlan {
  const previousByName = new Map<string, S['VariableDto']>();
  for (const row of previous) {
    previousByName.set(row.name, row);
  }

  const upserts: S['UpsertVariableReq'][] = [];
  const keptNames = new Set<string>();

  for (const variable of next) {
    const name = variable.name.trim();
    if (!name) {
      continue;
    }
    keptNames.add(name);
    const existing = previousByName.get(name);
    if (!existing || existing.value !== variable.value) {
      upserts.push({ name, value: variable.value, environment_id: environmentId });
    }
  }

  const deletes: string[] = [];
  for (const row of previous) {
    if (!keptNames.has(row.name)) {
      deletes.push(row.id);
    }
  }

  return { upserts, deletes };
}

// ---------------------------------------------------------------------------
// Notifications (CONTRACT-DIFF rows 86-91)
// ---------------------------------------------------------------------------

/** `NotificationConfigDto` -> `NotificationsConfig`. X7: `slack_webhook_url` is now
 *  `slack_webhook_credstore_ref`, so the input holds a reference. The gear's extra
 *  required `run_queue_queued_slack_enabled` has no legacy field and is not carried into
 *  the UI shape — but it **is** round-tripped on write (see `notificationsConfigReq`), so
 *  saving the form does not silently clear it. */
export function notificationsConfigFromDto(dto: S['NotificationConfigDto']): NotificationsConfig {
  return {
    slack_webhook_url: dto.slack_webhook_credstore_ref,
    slack_channel: dto.slack_channel,
    manager_ui_base_url: dto.manager_ui_base_url,
    slack_enabled: dto.slack_enabled,
    notify_on_failure: dto.notify_on_failure,
    notify_on_success: dto.notify_on_success,
    notify_on_schedule_completion: dto.notify_on_schedule_completion,
    scheduled_run_slack_enabled: dto.scheduled_run_slack_enabled,
    scheduled_run_slack_templates: dto.scheduled_run_slack_templates,
    email_smtp_host: dto.email_smtp_host,
    email_smtp_port: dto.email_smtp_port,
    email_smtp_username: dto.email_smtp_username,
    email_smtp_credstore_ref: dto.email_smtp_credstore_ref,
    email_from: dto.email_from,
    email_recipients: dto.email_recipients,
    email_enabled: dto.email_enabled,
  };
}

/** `NotificationsConfig` -> `NotificationConfigDto`. `run_queue_queued_slack_enabled` is
 *  required on the gear and has no UI field, so it is carried through from the last read
 *  rather than defaulted — a `false` here would silently switch off a notification the
 *  operator turned on outside this form. When there is no last read, the write is refused
 *  rather than guessed. */
export function notificationsConfigReq(
  config: NotificationsConfig,
  previous: S['NotificationConfigDto'] | undefined
): S['NotificationConfigDto'] {
  if (!previous) {
    throw new Error(
      'Load the notification settings before saving them: this deployment stores a queue-notification flag this form does not show, and saving without it would clear it.'
    );
  }
  return {
    ...previous,
    slack_webhook_credstore_ref: config.slack_webhook_url,
    slack_channel: config.slack_channel,
    manager_ui_base_url: config.manager_ui_base_url,
    slack_enabled: config.slack_enabled,
    notify_on_failure: config.notify_on_failure,
    notify_on_success: config.notify_on_success,
    notify_on_schedule_completion: config.notify_on_schedule_completion,
    scheduled_run_slack_enabled: config.scheduled_run_slack_enabled,
    scheduled_run_slack_templates: config.scheduled_run_slack_templates,
    email_smtp_host: config.email_smtp_host,
    email_smtp_port: config.email_smtp_port,
    email_smtp_username: config.email_smtp_username,
    email_smtp_credstore_ref: config.email_smtp_credstore_ref,
    email_from: config.email_from,
    email_recipients: config.email_recipients,
    email_enabled: config.email_enabled,
  };
}

/** `NotificationLogEntryDto` -> `NotificationLogEntry`. `id` becomes a uuid string and
 *  `workflow_name` becomes the run's uuid — row 91: the log line loses its
 *  human-readable run name, and rendering one would need an X1 lookup per line. A null
 *  `run_id` becomes `null`, not the string `"null"`. */
export function notificationLogEntryFromDto(dto: S['NotificationLogEntryDto']): NotificationLogEntry {
  return {
    id: dto.id,
    created_at: dto.created_at,
    workflow_name: dto.run_id ?? null,
    channel: dto.channel,
    event_type: dto.event_type,
    outcome: dto.outcome,
    detail: dto.detail,
  };
}

/**
 * `RunDto | QueuedRunDto` -> `LaunchResponse`.
 *
 * The gear answers `200 RunDto` when the run started and `202 QueuedRunDto` when it was
 * queued behind an exclusive run. Legacy's union was `{workflow_name}` vs
 * `{queue_id, state: 'queued'}`, and **`state` is gone** from the queued arm (rows 4, 17).
 * `isQueued` narrows on `'queue_id' in response`, which still works, so the discriminator
 * survives; the `state` literal is re-supplied here rather than left off, because
 * re-typing the union would be a change every `isQueued` call site can see.
 *
 * Narrowing on the *shape* rather than the status code is deliberate: `client.ts` returns
 * a parsed body and not a `Response`, so 200 and 202 are indistinguishable by the time
 * this runs — but `queue_id` appears on exactly one of the two shapes.
 */
export function launchResponseFromDto(
  body: S['RunDto'] | S['QueuedRunDto']
): { workflow_name: string } | { queue_id: string; state: 'queued' } {
  if (body && typeof body === 'object' && 'queue_id' in body) {
    return { queue_id: (body as S['QueuedRunDto']).queue_id, state: 'queued' };
  }
  return { workflow_name: (body as S['RunDto']).name };
}

/** `NotificationPreviewDto` -> `ScheduledRunNotificationPreviewResponse`. Field-for-field
 *  the same names (row 89) but not the same types: `blocks: unknown[]` there is not
 *  assignable to `SlackMessagePreview`'s `Array<Record<string, unknown>>` prop, and
 *  `event` is a plain `string` there rather than the UI's literal union. That mismatch is
 *  why `types.ts` keeps this shape hand-written, and this is where the narrowing happens —
 *  once, rather than at the component. */
export function notificationPreviewFromDto(
  dto: S['NotificationPreviewDto']
): ScheduledRunNotificationPreviewResponse {
  return {
    event: dto.event as ScheduledRunNotificationEvent,
    event_label: dto.event_label,
    rendered_message: dto.rendered_message,
    fallback_text: dto.fallback_text,
    blocks: (dto.blocks ?? []) as Array<Record<string, unknown>>,
  };
}
