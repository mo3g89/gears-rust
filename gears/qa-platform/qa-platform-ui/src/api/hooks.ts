// TanStack Query hooks for API calls.
//
// Every path below is relative to `/qa/v1` (`client.ts`'s `API_BASE_URL`), so a hook
// writes `/runs`, not `/qa/v1/runs`.
//
// **Every exported hook keeps its name, its parameters and its return shape**, and there
// are now no exceptions: nothing under `src/components/` or `src/pages/` is modified by
// this task at all. Every difference between what the components want and what the gears
// serve is absorbed here or in `./adapters.ts`.
//
// (`useProductCoverage` briefly lost its `productId` in the first round, because
// `GET /qa/v1/dashboard/coverage` declares no query parameters. Review round 1 found that
// the parameter is still needed — not as a server filter, but as the key for the
// client-side one that keeps one product's card from charting another product's rows. See
// its own doc.)
//
// Three shapes of work live here that did not before, all of them consequences recorded in
// `gears/qa-platform/docs/CONTRACT-DIFF.md`:
//
//  1. **Identifier resolution (X1).** The UI's own routes address runs, platforms and
//     schedules by *name*; every gear path segment is a uuid. So a hook that used to be
//     one request is now a lookup plus a request. `GET /qa/v1/runs/smoke-2` is a 400, not
//     a 404, so this is not optional.
//  2. **Plan identity (X6).** A gear plan has no id — it is `(repo_id, path)`. The pair
//     travels through the components as one opaque encoded string (`encodePlanId`), so
//     nothing downstream had to learn about pairs.
//  3. **Fan-out.** `GET /qa/v1/plans` takes a *required* `repo_id`, so a product's plans
//     are the concatenation of one request per repository.
//
// Where a field has no gear source it is left absent, never defaulted — see
// `adapters.ts`, which carries the per-field reasoning.

import { useMemo } from 'react';
import { useQuery, useQueries, useMutation, useQueryClient, keepPreviousData } from '@tanstack/react-query';
import { apiGet, apiGetBlob, apiPost, apiDelete, apiPut, apiPatch } from './client';
import { useSelectedProduct } from '@/lib/selectedProduct';
import type { components } from './generated/openapi';
import {
  analyticsOverviewFromDto,
  analyticsPlanId,
  buildTestDetailFromDto,
  coveragePointsFromDto,
  createPlatformReqFromForm,
  customPlanFromDto,
  customPlanReq,
  dashboardFromDto,
  decodePlanId,
  distinctObservedVersions,
  expandCustomPlanFiles,
  groupCasesByFile,
  jiraBugFromDto,
  jiraConfigFromDto,
  jiraConfigReq,
  launchReqFromForm,
  launchResponseFromDto,
  notificationLogEntryFromDto,
  notificationPreviewFromDto,
  notificationsConfigFromDto,
  notificationsConfigReq,
  odataEq,
  openBugsQuery,
  MAX_PAGE_LIMIT,
  parseRunLogSse,
  partitionPlatformVariables,
  planFromDto,
  platformDetailsFromDto,
  platformFromDto,
  productFromDto,
  productReqFromForm,
  queueEntryFromDto,
  recentResultsQuery,
  repoFromDto,
  repoReqFromForm,
  runDetailsFromDto,
  runFromDto,
  savedViewFromDto,
  savedViewReqFromPayload,
  scheduleFromDto,
  scheduleNotificationsReq,
  scheduleReqFromForm,
  sliceRecentResults,
  sshKeyFromDto,
  sshKeyReqFromForm,
  testAnalyticsFromDto,
  testFilesFromPlan,
  testHistoryFromDto,
  testRunResultFromDto,
  updatePlatformReqFromForm,
  unwrapPage,
  variableWritePlan,
  withPlatformNames,
  type Page,
  type PlanContext,
} from './adapters';
import {
  TestPlanInfo,
  WorkflowRun,
  TestRunResult,
  RunDetails,
  ScheduleInfo,
  CreateScheduleForm,
  UpdateScheduleNotificationsForm,
  DashboardStats,
  TestFileInfo,
  CustomPlan,
  CreateCustomPlanForm,
  PlatformDetails,
  PlatformInfo,
  CreatePlatformForm,
  UpdatePlatformForm,
  Product,
  CreateProductForm,
  ProductCoveragePoint,
  TestRepository,
  CreateTestRepositoryForm,
  TestAnalytics,
  BuildDistribution,
  TestHistory,
  AnalyticsOverview,
  AnalyticsOverviewQuery,
  AnalyticsBuildTestsQuery,
  BuildTestDetailItem,
  AnalyticsSavedView,
  AnalyticsSavedViewPayload,
  AnalyticsScope,
  JiraConfig,
  JiraBug,
  JiraCreateResponse,
  RunParameter,
  RunQueueEntry,
  PipelineVariablesConfig,
  NotificationsConfig,
  ScheduledRunNotificationPreviewRequest,
  ScheduledRunNotificationPreviewResponse,
  JiraPollerConfig,
  SshKeyInfo,
  CreateSshKeyForm,
  NotificationLogEntry,
} from './types';

type S = components['schemas'];

/**
 * A launch either started a run (200, unchanged from before the run queue) or was
 * queued behind an exclusive run on the same platform (202).
 *
 * The two members deliberately do NOT declare the other's field as
 * `?: undefined`: that would make `workflow_name` accessible on the un-narrowed
 * union and let an unguarded `data.workflow_name` compile, which is exactly the
 * bug this type exists to prevent. Narrow with `isQueued` first.
 *
 * The gears drop `state` from the queued arm (`QueuedRunDto` is `{queue_id, run_id}`), so
 * `launchResponseFromDto` re-supplies the literal rather than re-typing the union — see
 * its doc for why the narrowing is on shape and not on the status code.
 */
export type LaunchResponse =
  | { workflow_name: string }
  | { queue_id: string; state: 'queued' };

export function isQueued(
  response: LaunchResponse
): response is { queue_id: string; state: 'queued' } {
  return 'queue_id' in response;
}

// Query keys
export const queryKeys = {
  dashboard: ['dashboard'] as const,
  dashboardByDays: (days: number) => ['dashboard', days] as const,
  plans: ['plans'] as const,
  plan: (id: string) => ['plans', id] as const,
  runs: ['runs'] as const,
  run: (name: string) => ['runs', name] as const,
  runLogs: (name: string) => ['runs', name, 'logs'] as const,
  runQueue: (platform: string) => ['runQueue', platform] as const,
  schedules: ['schedules'] as const,
  tests: ['tests'] as const,
  testRepos: ['testRepos'] as const,
  testRepoBranches: (id: string) => ['testRepos', id, 'branches'] as const,
  customPlans: ['customPlans'] as const,
  customPlan: (id: string) => ['customPlans', id] as const,
  platforms: ['platforms'] as const,
  platformDetails: (name: string) => ['platforms', name, 'details'] as const,
  products: ['products'] as const,
  product: (id: string) => ['products', id] as const,
  productCoverage: (id: string) => ['products', id, 'coverage'] as const,
  analytics: (planId: string) => ['analytics', planId] as const,
  analyticsBuilds: (planId: string) => ['analytics', planId, 'builds'] as const,
  analyticsHistory: (planId: string) => ['analytics', planId, 'history'] as const,
  analyticsOverview: (queryKey: string) => ['analytics', 'overview', queryKey] as const,
  analyticsBuildTests: (queryKey: string) => ['analytics', 'build-tests', queryKey] as const,
  analyticsSavedViews: (scope: AnalyticsScope, planId?: string) =>
    ['analytics', 'views', scope, planId || ''] as const,
  jiraConfig: ['jiraConfig'] as const,
  pipelineVariables: ['pipelineVariables'] as const,
  notificationsConfig: ['notificationsConfig'] as const,
  jiraPollerConfig: ['jiraPollerConfig'] as const,
  sshKeys: ['sshKeys'] as const,
  jiraBugs: ['jiraBugs'] as const,
  notificationLog: ['notificationLog'] as const,
};

// ===========================================================================
// Shared plumbing
//
// Plain async functions rather than hooks, so a mutation can call them mid-flight and a
// query can call several of them in one `queryFn`. They are not memoised: TanStack caches
// the *hook* results, and a lookup inside a mutation has to be fresh — resolving a name
// against a stale platform list is how a rename turns into a write against the wrong row.
// ===========================================================================

async function fetchRepos(): Promise<S['TestRepositoryDto'][]> {
  return apiGet<S['TestRepositoryDto'][]>('/test-repos');
}

async function fetchPlatformDtos(): Promise<S['PlatformDto'][]> {
  return apiGet<S['PlatformDto'][]>('/platforms');
}

async function fetchProductDtos(): Promise<S['ProductDto'][]> {
  return apiGet<S['ProductDto'][]>('/products');
}

/** uuid -> display name, for the several places legacy drew a platform *name* and the
 *  gears send only the uuid (X1: rows 1, 5, 9, 14, 18). */
async function platformNameIndex(): Promise<Map<string, string>> {
  const platforms = await fetchPlatformDtos();
  return new Map(platforms.map((platform) => [platform.id, platform.name]));
}

/**
 * Resolve a platform *name* to its uuid.
 *
 * Throws rather than returning null when the name is unknown: every caller is about to
 * write to or read a path segment, and `/platforms/undefined` would be a confusing 400
 * where this is a sentence.
 */
async function resolvePlatformId(name: string): Promise<string> {
  const platforms = await fetchPlatformDtos();
  const match = platforms.find((platform) => platform.name === name);
  if (!match) {
    throw new Error(`No platform named "${name}" exists in this deployment.`);
  }
  return match.id;
}

/** A run id as the gears spell it. Used to tell an id from a name — see `resolveRunId`. */
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/**
 * Resolve a run *name* — or an id already — to its uuid (X1, §7.1).
 *
 * `name` is a filterable field on `GET /qa/v1/runs` (`odata.rs:80`); §10 listed this as
 * inferred-not-driven, and it was driven for this task:
 * `?$filter=name eq 'collect-…-2'` answers exactly that one run.
 *
 * **The uuid short-circuit is not defensive padding — without it the dashboard's
 * Recent Failures list is broken.** X1 says the UI addresses runs by name, but several
 * gear DTOs have no run name to give: `FailedTestCardDto`, `QueueEntryDto`,
 * `TestResultDto` and `NotificationLogEntryDto` all carry `run_id` and nothing else, so
 * `adapters.ts` binds that uuid to legacy's `workflow_name` field. A component that then
 * treats `workflow_name` as a name — `RecentFailuresCard.tsx:58` links to
 * `/runs/${failure.workflow_name}`, which is the `/runs/:name` route — hands this function
 * a uuid. Driven live: `?$filter=name eq '<uuid>'` answers **0 items**, so the pre-fix
 * code threw `No run named "…" exists`, and **every Recent-Failures link landed on an
 * error state**, while `GET /qa/v1/runs/<that uuid>` answers 200. A rename is only a
 * rename when the consumer treats the value as an opaque key; here it does not.
 *
 * Testing the shape rather than trying the filter first is deliberate: it is one fewer
 * request on the path that matters, and a run *name* can never collide, because qa-runs
 * builds names from a prefix plus a counter (`collect-<uuid>-3`, `smoke-7`, `login-1`) and
 * never emits a bare uuid.
 */
export async function resolveRunId(nameOrId: string): Promise<string> {
  if (UUID_PATTERN.test(nameOrId)) {
    return nameOrId;
  }
  const params = new URLSearchParams();
  params.set('$filter', odataEq('name', nameOrId));
  params.set('limit', '1');
  const page = await apiGet<Page<S['RunDto']>>(`/runs?${params.toString()}`);
  const match = unwrapPage(page)[0];
  if (!match) {
    throw new Error(`No run named "${nameOrId}" exists in this deployment.`);
  }
  return match.id;
}

/** Resolve a schedule *name* to its uuid (X1). The list is the only lookup available —
 *  there is no `GET /qa/v1/schedules?name=`. */
async function resolveScheduleId(name: string): Promise<string> {
  const schedules = await apiGet<S['ScheduleDto'][]>('/schedules');
  const match = schedules.find((schedule) => schedule.name === name);
  if (!match) {
    throw new Error(`No schedule named "${name}" exists in this deployment.`);
  }
  return match.id;
}

/**
 * Which repositories belong to a product.
 *
 * `GET /qa/v1/test-repos` has no `product_id` filter, but `TestRepositoryDto.product_id`
 * is present and required, so this is an honest client-side filter (§7.6) rather than one
 * of §8-C7's impossible ones. Beware X8 while reading this: the legacy `?product_id=`
 * parameter answered 200 *unfiltered*, so nothing would have errored had the filter been
 * left out — it would just have shown another product's repositories.
 */
function reposForProduct(
  repos: S['TestRepositoryDto'][],
  productId: string | null | undefined
): S['TestRepositoryDto'][] {
  if (!productId) {
    return repos;
  }
  return repos.filter((repo) => repo.product_id === productId);
}

/**
 * Every plan in a product, as the concatenation of one `GET /qa/v1/plans` per repository.
 *
 * Both of that route's parameters are **required** (§2 fact 2), which is what forces the
 * fan-out and what forces a branch: when the caller pinned none, each repository is asked
 * for its own `default_branch` rather than a single guessed branch, because "the default
 * branch" is a per-repository fact.
 *
 * A repository that rejects the branch is **skipped**, not fatal. A branch legitimately
 * exists in some repositories and not others — that is the normal case for a product with
 * more than one repository — so one 400/404 must not empty the whole list. It is a
 * `Promise.allSettled` and not a swallowed `try` per repo so that the successful ones are
 * still concatenated in order.
 */
async function fetchPlanDtos(
  productId: string | null | undefined,
  branch: string
): Promise<Array<{ plan: S['PlanDto']; ctx: PlanContext }>> {
  const [repos, products] = await Promise.all([fetchRepos(), fetchProductDtos()]);
  const scoped = reposForProduct(repos, productId);
  const productById = new Map(products.map((product) => [product.id, product]));

  const settled = await Promise.allSettled(
    scoped.map(async (repo) => {
      const params = new URLSearchParams();
      params.set('repo_id', repo.id);
      params.set('branch', branch || repo.default_branch);
      const plans = await apiGet<S['PlanDto'][]>(`/plans?${params.toString()}`);
      const product = productById.get(repo.product_id);
      const ctx: PlanContext = {
        repoName: repo.name,
        productId: repo.product_id,
        productKey: product?.key ?? null,
        productName: product?.name ?? null,
      };
      return plans.map((plan) => ({ plan, ctx }));
    })
  );

  return settled.flatMap((result) => (result.status === 'fulfilled' ? result.value : []));
}

/**
 * The hard ceiling on how many runs one `fetchRunDtos` call will read, and therefore on how
 * many requests it makes: two pages of `MAX_PAGE_LIMIT`.
 *
 * It exists because legacy's caller asks for a page size no gear will serve.
 * `RunsPage.tsx:62` calls `useRuns(autoRefresh ? 5000 : undefined, 1, 10000)` — one request
 * for 10,000 runs — and `/qa/v1/runs` clamps a page at 500 no matter what is asked
 * (driven: `?limit=500`, `?limit=501` and `?limit=10000` all answer `page_info.limit` 500).
 * Reproducing legacy literally therefore means **20 sequential requests every 5 seconds**
 * on a deployment with 10,000 runs — which is not "adapting the implementation", it is
 * turning one poll into a load generator, and 20 sequential round-trips cannot even finish
 * inside the 5-second interval that triggered them.
 *
 * So the walk is bounded, and the cost is **constant**: at most two `GET /runs` per call,
 * whatever the deployment holds. The visible consequence is that the Runs page works over
 * the newest 1,000 runs rather than all of them — its filtering and paging are client-side
 * (`RunsPage.tsx:61`), so an older run cannot be found there any more. That is a real
 * behaviour change, it is bounded and statable rather than silent, and it is recorded in
 * `CONTRACT-DIFF` §11.8 rather than left in a code comment. Raising the ceiling is a
 * one-constant change; making the page complete needs a server-side filter that X2 and
 * §8-C7 say does not exist.
 */
const MAX_RUNS_WINDOW = 2 * MAX_PAGE_LIMIT;

/**
 * Walk the runs collection forward far enough to answer `skip + limit` rows, and return the
 * `limit` rows after `skip` — never reading more than `MAX_RUNS_WINDOW` rows in total.
 *
 * X2: the gears page by opaque `cursor` with **no total and no page number**. `total` is
 * deliberately **not** derived from what this returns (§7.3): `items.length` after a
 * bounded walk is a lower bound, and a pager that displayed it as a total would be
 * confidently wrong.
 */
async function fetchRunDtos(skip: number, limit: number): Promise<S['RunDto'][]> {
  const wanted = Math.min(skip + limit, MAX_RUNS_WINDOW);
  const collected: S['RunDto'][] = [];
  let cursor: string | null = null;

  while (collected.length < wanted) {
    const params = new URLSearchParams();
    params.set('limit', String(Math.min(MAX_PAGE_LIMIT, wanted - collected.length)));
    if (cursor) {
      params.set('cursor', cursor);
    }
    const page = await apiGet<Page<S['RunDto']>>(`/runs?${params.toString()}`);
    const items = unwrapPage(page);
    collected.push(...items);
    cursor = page?.page_info?.next_cursor ?? null;
    if (!cursor || items.length === 0) {
      break;
    }
  }

  return collected.slice(skip, skip + limit);
}

/** The per-function case rows for one run. `RunTestResultDto` has no `cases` field, but
 *  the rows exist on their own collection (§8-C2: `cases` is recoverable where `logs` is
 *  not). The uuid is **unquoted** in the `$filter` — a quoted one is a 400 (see
 *  `odataLiteral`). A failure here is swallowed to `[]`: the case breakdown is an
 *  enrichment of the run detail, and losing it must not fail the page. */
async function fetchRunCases(runId: string): Promise<S['TestCaseResultDto'][]> {
  try {
    const params = new URLSearchParams();
    params.set('$filter', odataEq('run_id', runId, 'uuid'));
    params.set('limit', String(MAX_PAGE_LIMIT));
    return unwrapPage(await apiGet<Page<S['TestCaseResultDto']>>(`/test-case-results?${params.toString()}`));
  } catch {
    return [];
  }
}

/** One run detail, with its platform name resolved and its case rows attached. */
async function fetchRunDetails(runId: string): Promise<RunDetails> {
  const [detail, cases, platformNames] = await Promise.all([
    apiGet<S['RunDetailDto']>(`/runs/${runId}`),
    fetchRunCases(runId),
    platformNameIndex(),
  ]);
  const details = runDetailsFromDto(detail, groupCasesByFile(cases));
  return { ...details, run: withPlatformNames([details.run], platformNames)[0] };
}

// Dashboard
/**
 * The deployment's dashboard — **not** scoped to the active product.
 *
 * `days` is the only parameter `GET /qa/v1/dashboard` takes, and it is clamped to 3–90
 * (365 answers 90 points rather than an error). The `product_id`/`product_key` this hook
 * used to send are accepted and *silently ignored* (X8), so sending them made the page
 * claim a scope it never had — and per §8-C7 no client-side substitute exists either. The
 * product is still read, but only to keep the query gated the way it was.
 */
export function useDashboard(days = 14) {
  const product = useActiveProduct();
  return useQuery({
    queryKey: queryKeys.dashboardByDays(days),
    queryFn: async () => {
      const [dto, platformNames] = await Promise.all([
        apiGet<S['DashboardStatsDto']>(`/dashboard?days=${days}`),
        platformNameIndex(),
      ]);
      const stats = dashboardFromDto(dto);
      return {
        ...stats,
        recent_runs: withPlatformNames(stats.recent_runs, platformNames),
        active_runs_list: withPlatformNames(stats.active_runs_list, platformNames),
      } satisfies DashboardStats;
    },
    enabled: !!product,
    refetchInterval: 15_000,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}

// Test Plans
/**
 * @param productIdOverride Scope to a specific product instead of the
 *   globally-active one — pass e.g. a custom plan's own `product_id` when
 *   displaying/resolving data for that specific plan, so the result stays
 *   correct even if a different product is active elsewhere in the app
 *   (distinguished from "not passed" via `!== undefined`, so `null` is a
 *   valid override meaning "no product filter").
 * @param enabled Set to `false` to defer the query entirely — e.g. while a
 *   `productIdOverride` is still being resolved (loading the plan that owns
 *   it). Without this, starting the query against the active product and
 *   then switching `productIdOverride` once it's known would briefly show
 *   the (wrong) active product's list via `keepPreviousData` below. Defaults
 *   to `true`.
 */
export function usePlans(branch?: string, productIdOverride?: string | null, enabled = true) {
  const branchKey = branch?.trim() || '';
  const hasOverride = productIdOverride !== undefined;
  const product = useActiveProduct();
  const productId = hasOverride ? productIdOverride ?? '' : product?.id ?? '';
  return useQuery({
    queryKey: [...queryKeys.plans, branchKey, productId],
    queryFn: async (): Promise<TestPlanInfo[]> => {
      const rows = await fetchPlanDtos(productId, branchKey);
      return rows.map(({ plan, ctx }) => planFromDto(plan, ctx));
    },
    enabled: (hasOverride ? true : !!product) && enabled,
    // Keep the previous branch's list on screen while the new one loads so
    // switching branches doesn't flash a spinner.
    placeholderData: keepPreviousData,
  });
}

/**
 * One plan, by the opaque id `usePlans` handed out.
 *
 * There is no single-plan read anywhere in qa-catalog's 22 operations — a plan has no id
 * to read it by (X6) — so this is §7.7's "list standing in for a single read": ask the
 * plan's own repository for its plans and match on `path`. That is one request, not a
 * fan-out, because the id already carries the repository.
 */
export function usePlan(id: string, branch?: string) {
  const branchKey = branch?.trim() || '';
  return useQuery({
    queryKey: [...queryKeys.plan(id), branchKey],
    queryFn: async (): Promise<TestPlanInfo> => {
      const decoded = decodePlanId(id);
      if (!decoded) {
        throw new Error(`"${id}" is not a plan in this deployment.`);
      }
      const [repos, products] = await Promise.all([fetchRepos(), fetchProductDtos()]);
      const repo = repos.find((candidate) => candidate.id === decoded.repo_id);
      if (!repo) {
        throw new Error(`The repository this plan belongs to no longer exists.`);
      }
      const params = new URLSearchParams();
      params.set('repo_id', repo.id);
      params.set('branch', branchKey || repo.default_branch);
      const plans = await apiGet<S['PlanDto'][]>(`/plans?${params.toString()}`);
      const match = plans.find((plan) => plan.path === decoded.path);
      if (!match) {
        throw new Error(`No plan at "${decoded.path}" on branch "${branchKey || repo.default_branch}".`);
      }
      const product = products.find((candidate) => candidate.id === repo.product_id);
      return planFromDto(match, {
        repoName: repo.name,
        productId: repo.product_id,
        productKey: product?.key ?? null,
        productName: product?.name ?? null,
      });
    },
    enabled: !!id,
  });
}

/**
 * Launch a plan. The legacy query string becomes a `LaunchRunReq` body (§7.4).
 *
 * The platform arrives as a *name* and the request needs a uuid, so this resolves it
 * first. It may still be absent: `launchReqFromForm` sends `platform_id: null` rather
 * than refusing, because a platformless run is a supported case the gear dispatches
 * inline (see that function's doc for the CONTRACT-DIFF row 3 correction). The dialogs
 * resolve their "Default cluster" option to the product's platform before calling this,
 * so in practice a name arrives — but an API caller may legitimately omit one.
 */
export function useRunPlan() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ planId, platform, branch, parameters, exclusive }: { planId: string; platform?: string; branch?: string; scheduleId?: string; parameters?: RunParameter[]; exclusive?: boolean }): Promise<LaunchResponse> => {
      // `scheduleId` is accepted and dropped: `RunDto.schedule_id` is server-set and
      // `LaunchRunReq` has no such field (row 4). A manual launch is not a scheduled one.
      const platformId = platform ? await resolvePlatformId(platform) : null;
      const body = launchReqFromForm({ planId, platformId, branch, parameters, exclusive });
      return launchResponseFromDto(await apiPost<S['RunDto'] | S['QueuedRunDto']>('/runs', body));
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.runs });
      queryClient.invalidateQueries({ queryKey: queryKeys.dashboard });
    },
  });
}

// Workflow Runs
/**
 * Every run in the deployment — **not** scoped to the active product.
 *
 * The product scoping this hook used to request was removed: a run carries no
 * product key in this deployment, and the backend ignores an unknown query
 * parameter rather than rejecting it, so `product_key=…` returned 200 with the
 * whole tenant's rows and the page silently claimed a filter it never had.
 * See CONTRACT-DIFF §8-C7. Restore the parameter only together with a product
 * field on the run itself.
 *
 * `page` and `perPage` are still honoured, but what comes back is now the **bare array**
 * rather than legacy's `PaginatedResponse<WorkflowRun>` envelope: the gears page by cursor
 * with no total and no page count (X2), so there is no `pagination` object to fill and §7.3
 * forbids synthesising one from `items.length`. That is not a return-shape break — the bare
 * array was always one arm of this hook's declared union, and `RunsPage` already handles
 * both (`Array.isArray(response)` at `:69`). But a page-N-of-M pager cannot be rebuilt
 * from this data, which is a component-level consequence for the reviewer rather than
 * something this hook can hide.
 *
 * **Cost:** two `GET /runs` plus one `GET /platforms` per fetch, constant in the size of
 * the deployment — and this hook polls every 5 seconds on the Runs page. The bound is
 * `MAX_RUNS_WINDOW` and the truncation it implies is documented there and in
 * `CONTRACT-DIFF` §11.8; `perPage` above that ceiling is silently clamped, exactly as the
 * gear clamps `limit`.
 */
export function useRuns(refetchInterval?: number, page: number = 1, perPage: number = 50) {
  return useQuery({
    queryKey: [...queryKeys.runs, page, perPage],
    queryFn: async (): Promise<WorkflowRun[]> => {
      const [dtos, platformNames] = await Promise.all([
        fetchRunDtos((Math.max(1, page) - 1) * perPage, perPage),
        platformNameIndex(),
      ]);
      return withPlatformNames(dtos.map(runFromDto), platformNames);
    },
    refetchInterval,
  });
}

// Trigger the collect-cases job. Pass a branch to collect only the tests that
// exist on that branch; omit it to collect the default branch.
export function useCollectCases() {
  return useMutation({
    mutationFn: (branch?: string) => {
      const trimmed = branch?.trim();
      const suffix = trimmed ? `?branch=${encodeURIComponent(trimmed)}` : '';
      return apiPost<S['CollectTriggerOutcomeDto']>(`/analytics/collect${suffix}`);
    },
  });
}

/**
 * Recent results for one test file.
 *
 * The bound is `limit`, not `$top` — see `recentResultsQuery` for the driven evidence and
 * for why the results are **also** sliced client-side. "Recent" is weaker here than the
 * name suggests: `/qa/v1/test-results` has no chronological `$orderby` (`run_finished_at`
 * is filterable but not sortable, because it is null mid-run and a null sort key
 * truncates pagination), so the default order is `id` descending — *stable but
 * arbitrary*. Row 7 records that; no adapter can fix it.
 */
export function useTestRecentResults(file: string, limit = 10, refetchInterval?: number) {
  return useQuery({
    queryKey: ['test-recent-results', file, limit],
    queryFn: async (): Promise<TestRunResult[]> => {
      const page = await apiGet<Page<S['TestResultDto']>>(`/test-results?${recentResultsQuery({ file, limit })}`);
      return sliceRecentResults(unwrapPage(page), limit).map(testRunResultFromDto);
    },
    enabled: !!file,
    refetchInterval,
  });
}

/**
 * A plan's runs.
 *
 * `RunFilterField` has **no `target.*` member** (`odata.rs:79-90`), so there is no server
 * filter for "runs of this plan" and this is a client-side filter over a page of runs
 * (row 8, §7.11). The window is one maximal page, so a plan whose runs are all older than
 * the newest 500 in the deployment will show none — a real limitation of the substitute,
 * not a bug in the filter.
 */
export function useRunsByPlan(planId: string, refetchInterval?: number) {
  return useQuery({
    queryKey: [...queryKeys.runs, 'plan', planId],
    queryFn: async (): Promise<WorkflowRun[]> => {
      const [dtos, platformNames] = await Promise.all([
        fetchRunDtos(0, MAX_PAGE_LIMIT),
        platformNameIndex(),
      ]);
      const runs = dtos.map(runFromDto).filter((run) => run.plan_id === planId);
      return withPlatformNames(runs, platformNames);
    },
    enabled: !!planId,
    refetchInterval,
  });
}

/** One run's detail. `name` -> uuid first (X1: `GET /qa/v1/runs/smoke-2` is a 400). */
export function useRun(name: string, refetchInterval?: number) {
  return useQuery({
    queryKey: queryKeys.run(name),
    queryFn: async () => fetchRunDetails(await resolveRunId(name)),
    enabled: !!name,
    refetchInterval,
  });
}

/**
 * Fetch several run details at once (shares the per-run cache with useRun).
 * Returns a name -> RunDetails map of whatever has loaded. Disabled when
 * `enabled` is false, so callers can gate it on e.g. a section being open.
 */
export function useRunDetailsMap(names: string[], enabled: boolean): Record<string, RunDetails> {
  const results = useQueries({
    queries: names.map((name) => ({
      queryKey: queryKeys.run(name),
      queryFn: async () => fetchRunDetails(await resolveRunId(name)),
      enabled: enabled && !!name,
      staleTime: 60_000,
    })),
  });
  const map: Record<string, RunDetails> = {};
  names.forEach((name, i) => {
    const data = results[i]?.data;
    if (data) map[name] = data;
  });
  return map;
}

/**
 * One run's log text.
 *
 * The body is a `text/event-stream` of `RunLogLineDto {line}` where legacy served a plain
 * string (row 12), so it is flattened by `parseRunLogSse`. A finished run's stream
 * terminates immediately — driven live: 200, `text/event-stream`, zero bytes — so a plain
 * GET is a complete read for the case the log viewer opens. A *live* run's stream stays
 * open and this 5-second poll will not start a second request while the first is in
 * flight, which is a behaviour difference from legacy's poll: making this incremental
 * needs `EventSource`, which is §6.4's job and carries §6.4's two consequences (no
 * headers on `EventSource`, and nginx `proxy_buffering off`).
 */
export function useRunLogs(name: string, opts: { live?: boolean } = {}) {
  // `live: false` is for a run in a terminal state, whose log can no longer change.
  // Polling one every 5s re-downloads and re-parses the whole body — 2.4 MB for a
  // 22k-line run — to produce a string identical to the one already in the cache.
  const live = opts.live ?? true;
  return useQuery({
    queryKey: queryKeys.runLogs(name),
    queryFn: async (): Promise<string> => {
      const runId = await resolveRunId(name);
      return parseRunLogSse(await apiGet<string>(`/runs/${runId}/logs`));
    },
    enabled: !!name,
    refetchInterval: live ? 5000 : false,
  });
}

/**
 * **Stops** a run. Both call sites — the Runs table's per-row square button and the
 * run detail page's Stop button — confirm with "Stop run", act only on an active
 * run and report "is stopping"; neither offers deleting a run from the history, so
 * despite the name there is one intent here, not two.
 *
 * It therefore maps to `POST /qa/v1/runs/{id}/cancel`, which serves it exactly
 * (CONTRACT-DIFF §7.12, which reclassified this row out of §8 once the *behaviour* rather
 * than the local variable name was read). Removing a run from the history has no endpoint
 * and no UI, and no delete is synthesised for one.
 */
export function useDeleteRun() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (name: string): Promise<void> => {
      const runId = await resolveRunId(name);
      await apiPost<void>(`/runs/${runId}/cancel`);
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.runs });
      queryClient.invalidateQueries({ queryKey: queryKeys.dashboard });
    },
  });
}

/**
 * The run queue, optionally for one platform. `platform` is part of the query key
 * so the Runs page (all platforms) and a platform page do not share a cache entry.
 *
 * `platform_id` is passed to the server rather than filtered client-side even though
 * `limit` would allow the latter: `queue_position`'s own doc warns it is computed over
 * *the rows that request returned*, so a truncating window understates it. Narrowing on
 * the server keeps the positions right (row 14).
 */
export function useRunQueue(platform?: string, refetchInterval?: number) {
  return useQuery({
    queryKey: queryKeys.runQueue(platform || ''),
    queryFn: async (): Promise<RunQueueEntry[]> => {
      const params = new URLSearchParams();
      if (platform) {
        params.set('platform_id', await resolvePlatformId(platform));
      }
      params.set('limit', String(MAX_PAGE_LIMIT));
      const [page, platformNames] = await Promise.all([
        apiGet<Page<S['QueueEntryDto']>>(`/queue?${params.toString()}`),
        platformNameIndex(),
      ]);
      return unwrapPage(page)
        .map(queueEntryFromDto)
        .map((entry) => ({ ...entry, platform: platformNames.get(entry.platform) ?? entry.platform }));
    },
    refetchInterval,
  });
}

/**
 * Drop a **queued row** from the queue.
 *
 * `DELETE /qa/v1/queue/{id}` and `POST /qa/v1/runs/{id}/cancel` are different operations
 * on different ids, and §5-C corrects spec §6.3 on exactly this: this hook holds a
 * `RunQueueEntry.id` — a queue-row id — so it is the DELETE. The run cancel is
 * `useDeleteRun`. Legacy answered `{id, state}`; this answers `204` with no body, and no
 * consumer read the old body.
 */
export function useCancelQueuedRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => apiDelete<void>(`/queue/${id}`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['runQueue'] });
    },
  });
}

/**
 * Force start ignores the platform's occupancy but not `max_concurrent_runs`, so
 * this can still fail with 429 — surface the error rather than assuming success.
 * (`enforce_global_cap` runs first and is cluster-wide, `admission.rs:621`.)
 */
export function useForceStartQueuedRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (id: string): Promise<{ workflow_name: string }> => {
      const started = await apiPost<S['StartedRunDto']>(`/queue/${id}/force-start`);
      // `workflow_name` -> `run_id` (uuid): the started run's name is not on this
      // response, and looking it up would be a second request for a field no caller reads.
      return { workflow_name: started.run_id };
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['runQueue'] });
      queryClient.invalidateQueries({ queryKey: queryKeys.runs });
    },
  });
}

export function useRerunRun() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (name: string): Promise<LaunchResponse> => {
      const runId = await resolveRunId(name);
      return launchResponseFromDto(await apiPost<S['RunDto'] | S['QueuedRunDto']>(`/runs/${runId}/rerun`));
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.runs });
      queryClient.invalidateQueries({ queryKey: queryKeys.dashboard });
    },
  });
}

// Schedules
/**
 * Every schedule in the deployment — **not** scoped to the active product, for
 * the reason given on `useRuns`: a schedule carries no product key and the
 * ignored `product_key=…` parameter made the list look scoped when it was not
 * (CONTRACT-DIFF §8-C7).
 */
export function useSchedules() {
  return useQuery({
    queryKey: queryKeys.schedules,
    queryFn: async (): Promise<ScheduleInfo[]> => {
      const [dtos, platformNames] = await Promise.all([
        apiGet<S['ScheduleDto'][]>('/schedules'),
        platformNameIndex(),
      ]);
      return dtos
        .map(scheduleFromDto)
        .map((schedule) => ({
          ...schedule,
          platform: platformNames.get(schedule.platform) ?? schedule.platform,
        }));
    },
    // Keep the Recent Runs strips fresh as scheduled runs come and go.
    refetchInterval: 15_000,
    refetchIntervalInBackground: false,
  });
}

/**
 * Create a schedule, then set its notifications.
 *
 * Two requests, not one, and the second is not optional: `NewScheduleReq` carries **no**
 * notification fields at all (row 19), so a create that set the form's three `slack_*`
 * fields and stopped here would drop them silently — which is the failure this comment
 * exists to prevent. The follow-up `PUT .../notifications` only runs when the form
 * actually asked for notifications, so an ordinary create is still one request.
 */
export function useCreateSchedule() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: CreateScheduleForm): Promise<ScheduleInfo> => {
      const platformId = data.platform ? await resolvePlatformId(data.platform) : null;
      const created = await apiPost<S['ScheduleDto']>(
        '/schedules',
        scheduleReqFromForm(data, { platformId, enabled: true })
      );
      const wantsNotifications =
        data.slack_notifications_enabled !== undefined ||
        data.slack_channel !== undefined ||
        data.slack_notification_events !== undefined;
      if (!wantsNotifications) {
        return scheduleFromDto(created);
      }
      const updated = await apiPut<S['ScheduleDto']>(
        `/schedules/${created.id}/notifications`,
        scheduleNotificationsReq({
          enabled: data.slack_notifications_enabled ?? false,
          channel: data.slack_channel ?? null,
          events: data.slack_notification_events,
        })
      );
      return scheduleFromDto(updated);
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.schedules });
      queryClient.invalidateQueries({ queryKey: queryKeys.dashboard });
    },
  });
}

/**
 * Replace a schedule.
 *
 * `PUT /qa/v1/schedules/{id}` is a **true replace**: an omitted defaulted field is
 * cleared, and `enabled` is required precisely so that an edit cannot silently disable or
 * re-enable a schedule (`qa-runs/.../rest/dto.rs:921-923`). So the edit form's values are sent in full, and
 * `enabled` is read from the row as it stands rather than assumed — otherwise saving an
 * unrelated field on a suspended schedule would resume it. The platform falls back to the
 * row's current one for the same reason and one more: a schedule with a null
 * `platform_id` is accepted and then never fires (§2 fact 3, which `ScheduleDto`'s own doc
 * repeats for schedules), so an edit that did not name a platform must not clear it.
 */
export function useUpdateSchedule() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ name, data }: { name: string; data: CreateScheduleForm }): Promise<ScheduleInfo> => {
      const scheduleId = await resolveScheduleId(name);
      const [current, platformId] = await Promise.all([
        apiGet<S['ScheduleDto']>(`/schedules/${scheduleId}`),
        data.platform ? resolvePlatformId(data.platform) : Promise.resolve(null),
      ]);
      const saved = await apiPut<S['ScheduleDto']>(
        `/schedules/${scheduleId}`,
        scheduleReqFromForm(data, {
          platformId: platformId ?? current.platform_id ?? null,
          enabled: current.enabled,
          name: current.name,
        })
      );
      const wantsNotifications =
        data.slack_notifications_enabled !== undefined ||
        data.slack_channel !== undefined ||
        data.slack_notification_events !== undefined;
      if (!wantsNotifications) {
        return scheduleFromDto(saved);
      }
      const updated = await apiPut<S['ScheduleDto']>(
        `/schedules/${scheduleId}/notifications`,
        scheduleNotificationsReq({
          enabled: data.slack_notifications_enabled ?? false,
          channel: data.slack_channel ?? null,
          events: data.slack_notification_events,
        })
      );
      return scheduleFromDto(updated);
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.schedules });
      queryClient.invalidateQueries({ queryKey: queryKeys.dashboard });
    },
  });
}

/**
 * Read a schedule, then PUT it back with `enabled` flipped.
 *
 * Legacy's two dedicated verbs (`POST .../suspend`, `POST .../resume`) collapse into one
 * `PUT /qa/v1/schedules/{id}` (§6.3 row 1). The read is **required**, not an
 * optimisation: the PUT replaces the whole record, so PUTting `{enabled}` alone would
 * clear the schedule's cron, target, platform, branch and tags. The other values come from
 * `GET /qa/v1/schedules/{id}` rather than from this hook's argument, because the argument
 * is only a name — a caller that wanted to change a field would use `useUpdateSchedule`.
 */
function setScheduleEnabled(enabled: boolean) {
  return async (name: string): Promise<void> => {
    const scheduleId = await resolveScheduleId(name);
    const current = await apiGet<S['ScheduleDto']>(`/schedules/${scheduleId}`);
    await apiPut<S['ScheduleDto']>(`/schedules/${scheduleId}`, {
      name: current.name,
      cron: current.cron,
      enabled,
      target: current.target,
      platform_id: current.platform_id ?? null,
      branch: current.branch ?? null,
      include_tags: current.include_tags ?? [],
      exclude_tags: current.exclude_tags ?? [],
      exclusive_choice: current.exclusive_choice,
      parameters: current.parameters ?? [],
    } satisfies S['NewScheduleReq']);
  };
}

export function useSuspendSchedule() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: setScheduleEnabled(false),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.schedules });
    },
  });
}

export function useResumeSchedule() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: setScheduleEnabled(true),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.schedules });
    },
  });
}

export function useDeleteSchedule() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (name: string): Promise<void> => {
      await apiDelete<void>(`/schedules/${await resolveScheduleId(name)}`);
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.schedules });
      queryClient.invalidateQueries({ queryKey: queryKeys.dashboard });
    },
  });
}

export function useSetScheduleNotifications() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      name,
      data,
    }: {
      name: string;
      data: UpdateScheduleNotificationsForm;
    }): Promise<ScheduleInfo> => {
      const scheduleId = await resolveScheduleId(name);
      // POST -> PUT (row 24): the gear treats a notification block as replaceable state,
      // not as an event.
      const saved = await apiPut<S['ScheduleDto']>(
        `/schedules/${scheduleId}/notifications`,
        scheduleNotificationsReq(data)
      );
      return scheduleFromDto(saved);
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.schedules });
    },
  });
}

// Tests (Catalog)
/**
 * The test-file catalog, **degraded**.
 *
 * `GET /tests` has no counterpart anywhere in the gears: qa-catalog publishes 22
 * operations and none of them is a test-file catalog (§8-C1). What can be served honestly
 * is `PlanDto.test_files` — the paths, and the plan, repository and product above them.
 * What cannot is every metadata column the catalog page renders: `title`, `component`,
 * `description`, `tags`, `quality_vectors`, `versions` and `loc` have no source in any
 * gear, so `testFilesFromPlan` leaves all of them absent rather than blank-labelling a
 * fabricated value.
 *
 * §8-C1's question — degrade `/tests` to a path list or remove the surface the way §6.5
 * removed three settings pages — is a **human's** to answer and is still open. This
 * degradation is what keeps four routed consumers (`TestCatalogPage`, `TestDetailPage`,
 * `CustomPlanEditorPage`, `CreateScheduleDialog`) working rather than 404ing while it is
 * open, and it is reported as a decision taken pending that answer.
 */
export function useTests(branch?: string) {
  const branchKey = branch?.trim() || '';
  const product = useActiveProduct();
  const productId = product?.id ?? '';
  return useQuery({
    queryKey: [...queryKeys.tests, branchKey, productId],
    queryFn: async (): Promise<TestFileInfo[]> => {
      const rows = await fetchPlanDtos(productId, branchKey);
      return rows.flatMap(({ plan, ctx }) => testFilesFromPlan(plan, ctx));
    },
    enabled: !!product,
    placeholderData: keepPreviousData,
  });
}

/**
 * Warm the plans cache before the user commits to a branch, e.g. on dropdown-option hover.
 *
 * It no longer triggers a backend branch sync, and that is deliberate rather than an
 * omission: the gears sync via `POST /qa/v1/test-repos/{id}/sync`, which is a **mutation**
 * (row 26). A prefetch that fired a mutation on hover would fetch git for every option the
 * pointer crossed. `useRefreshBranch` is the explicit control for that.
 */
export function usePlansPrefetch() {
  const queryClient = useQueryClient();
  const product = useActiveProduct();
  const productId = product?.id ?? '';
  return (branch?: string) => {
    const branchKey = branch?.trim() || '';
    queryClient.prefetchQuery({
      queryKey: [...queryKeys.plans, branchKey, productId],
      queryFn: async (): Promise<TestPlanInfo[]> => {
        const rows = await fetchPlanDtos(productId, branchKey);
        return rows.map(({ plan, ctx }) => planFromDto(plan, ctx));
      },
    });
  };
}

export function useTestRepositories(productId?: string | null) {
  const hasProductFilter = productId !== undefined && productId !== null;
  return useQuery({
    queryKey: hasProductFilter ? [...queryKeys.testRepos, productId] : queryKeys.testRepos,
    queryFn: async (): Promise<TestRepository[]> => {
      // Client-side (§7.6): `GET /qa/v1/test-repos` takes no `product_id`, but
      // `TestRepositoryDto.product_id` is required, so the key is on the row.
      const repos = await fetchRepos();
      return reposForProduct(repos, hasProductFilter ? productId : null).map(repoFromDto);
    },
  });
}

/**
 * Force a fresh git fetch of a branch (bypassing the backend sync TTL) and
 * then refresh the plan/test listings so new commits show up immediately.
 *
 * The response is the **repository row**, not legacy's `{status, plans, tests}` counts
 * (row 30): success or failure now reads from `sync_error`/`last_synced_at`. No consumer
 * read the counts, so nothing is invented to replace them.
 */
export function useRefreshBranch() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async ({ repoId, branch }: { repoId: string; branch?: string }): Promise<TestRepository> => {
      const qs = branch?.trim() ? `?branch=${encodeURIComponent(branch.trim())}` : '';
      return repoFromDto(await apiPost<S['TestRepositoryDto']>(`/test-repos/${repoId}/sync${qs}`));
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.plans });
      queryClient.invalidateQueries({ queryKey: queryKeys.tests });
      queryClient.invalidateQueries({ queryKey: queryKeys.testRepos });
    },
  });
}

export function useTestRepoBranches(repoId: string) {
  return useQuery({
    queryKey: queryKeys.testRepoBranches(repoId),
    // `{branches: string[]}` -> `string[]` (§7.2, row 31).
    queryFn: async (): Promise<string[]> =>
      (await apiGet<S['BranchListDto']>(`/test-repos/${repoId}/branches`)).branches ?? [],
    enabled: !!repoId,
  });
}

/**
 * Fetch cached branch lists for several repositories in parallel and merge
 * them into a single deduped, sorted list. Used by branch pickers so that a
 * product with more than one git repository offers branches from all of
 * them, not just one.
 *
 * Memoized on the underlying query data (not just recomputed every render):
 * several callers put this hook's return value straight into a `useEffect`
 * dependency array to pick a default branch, so a fresh array identity on
 * every render — regardless of whether the branch set actually changed —
 * would make that effect re-run on every unrelated re-render too. The
 * envelope unwrapping happens inside each `queryFn`, so the memoised value is
 * still the plain `string[]` it always was (row 32).
 */
export function useTestRepoBranchesForRepos(repoIds: string[]): string[] {
  const ids = [...new Set(repoIds.filter(Boolean))];
  const results = useQueries({
    queries: ids.map((repoId) => ({
      queryKey: queryKeys.testRepoBranches(repoId),
      queryFn: async (): Promise<string[]> =>
        (await apiGet<S['BranchListDto']>(`/test-repos/${repoId}/branches`)).branches ?? [],
      enabled: !!repoId,
    })),
  });
  // A single fixed-length dependency key covering which repos are in play
  // and their fetched branch data — `useMemo`'s dependency array must stay
  // the same length across renders, which a variable-length `results`/`ids`
  // array (repo count changes as callers' inputs change) would violate.
  // `JSON.stringify` (rather than a manually delimited string) avoids any
  // ambiguity from a repo id or branch name containing the delimiter itself.
  const signature = JSON.stringify(ids.map((id, i) => [id, results[i]?.data ?? []]));
  return useMemo(() => {
    const branches = new Set<string>();
    for (const result of results) {
      if (Array.isArray(result.data)) {
        for (const branch of result.data) branches.add(branch);
      }
    }
    return [...branches].sort((a, b) => a.localeCompare(b));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [signature]);
}

export function useCreateTestRepository() {
  const queryClient = useQueryClient();

  return useMutation({
    // `repoReqFromForm` refuses a pasted token rather than forwarding it as a credstore
    // reference — X7 is the one place a rename adapter is wrong.
    mutationFn: async (data: CreateTestRepositoryForm): Promise<TestRepository> =>
      repoFromDto(await apiPost<S['TestRepositoryDto']>('/test-repos', repoReqFromForm(data))),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.testRepos });
      queryClient.invalidateQueries({ queryKey: queryKeys.tests });
      queryClient.invalidateQueries({ queryKey: queryKeys.plans });
    },
  });
}

export function useUpdateTestRepository() {
  const queryClient = useQueryClient();

  return useMutation({
    // A replace, not a merge (row 35) — `UpdateTestRepoReq` is field-identical to the
    // create request, so every field the form did not show is re-sent by the caller.
    mutationFn: async ({ id, data }: { id: string; data: CreateTestRepositoryForm }): Promise<TestRepository> =>
      repoFromDto(await apiPut<S['TestRepositoryDto']>(`/test-repos/${id}`, repoReqFromForm(data))),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.testRepos });
      queryClient.invalidateQueries({ queryKey: queryKeys.tests });
      queryClient.invalidateQueries({ queryKey: queryKeys.plans });
    },
  });
}

export function useDeleteTestRepository() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => apiDelete<void>(`/test-repos/${id}`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.testRepos });
      queryClient.invalidateQueries({ queryKey: queryKeys.tests });
      queryClient.invalidateQueries({ queryKey: queryKeys.plans });
    },
  });
}

export function useSyncTestRepository() {
  const queryClient = useQueryClient();

  return useMutation({
    // As `useRefreshBranch`: the response is the repository row, not a count pair.
    mutationFn: async (id: string): Promise<TestRepository> =>
      repoFromDto(await apiPost<S['TestRepositoryDto']>(`/test-repos/${id}/sync`)),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.testRepos });
      queryClient.invalidateQueries({ queryKey: queryKeys.tests });
      queryClient.invalidateQueries({ queryKey: queryKeys.plans });
    },
  });
}

export function useSshKeys() {
  return useQuery({
    queryKey: queryKeys.sshKeys,
    queryFn: async (): Promise<SshKeyInfo[]> =>
      (await apiGet<S['SshKeyDto'][]>('/ssh-keys')).map(sshKeyFromDto),
  });
}

export function useCreateSshKey() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (data: CreateSshKeyForm): Promise<SshKeyInfo> =>
      sshKeyFromDto(await apiPost<S['SshKeyDto']>('/ssh-keys', sshKeyReqFromForm(data))),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.sshKeys });
    },
  });
}

export function useDeleteSshKey() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => apiDelete<void>(`/ssh-keys/${id}`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.sshKeys });
      queryClient.invalidateQueries({ queryKey: queryKeys.testRepos });
    },
  });
}

/**
 * Launch a single test file from a plan.
 *
 * §5-D corrects spec §6.3, which listed `/plans/{id}/run-test` among the dead paths: it is
 * neither dead (two consumers) nor absent. `RunTargetDto.kind` is *"`plan`, `test`,
 * `custom_plan` or `collect`"* and `test_file` is *"Required for `test`"*, so a
 * single-test launch is served exactly by `POST /qa/v1/runs` with a `test` target.
 */
export function useRunSingleTest() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ planId, testFile, platform, branch, parameters, exclusive }: { planId: string; testFile: string; platform?: string; branch?: string; scheduleId?: string; parameters?: RunParameter[]; exclusive?: boolean }): Promise<LaunchResponse> => {
      const platformId = platform ? await resolvePlatformId(platform) : null;
      const body = launchReqFromForm({ planId, testFile, platformId, branch, parameters, exclusive });
      return launchResponseFromDto(await apiPost<S['RunDto'] | S['QueuedRunDto']>('/runs', body));
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.runs });
      queryClient.invalidateQueries({ queryKey: queryKeys.dashboard });
    },
  });
}

// Custom Plans
/**
 * Every custom plan in the deployment — **not** scoped to a product, for the
 * reason given on `useRuns`: a custom plan carries no product key (and, unlike a
 * run, no repository either, so there is nothing to walk to one), while the
 * ignored `product_id=…` parameter made the list look scoped (CONTRACT-DIFF
 * §8-C7).
 *
 * @param enabled See `usePlans`'s `enabled` param — same rationale.
 */
export function useCustomPlans(enabled = true) {
  return useQuery({
    queryKey: queryKeys.customPlans,
    queryFn: async (): Promise<CustomPlan[]> =>
      (await apiGet<S['CustomPlanDto'][]>('/custom-plans')).map(customPlanFromDto),
    enabled,
  });
}

export function useCustomPlan(id: string) {
  return useQuery({
    queryKey: queryKeys.customPlan(id),
    queryFn: async (): Promise<CustomPlan> =>
      customPlanFromDto(await apiGet<S['CustomPlanDto']>(`/custom-plans/${id}`)),
    enabled: !!id,
  });
}

/**
 * The lookups `expandCustomPlanFiles` needs, as network reads.
 *
 * A standard plan's `test_files` are only readable per `(repo_id, branch)`, so this asks
 * the plan's own repository on its own default branch. That is the same branch the editor
 * would have resolved the plan on, and using the repository's default rather than the
 * currently-selected branch matters: `useAttributedRepoIds`' doc makes the same point in
 * reverse — which repository a plan lives in does not change per branch, and scoping the
 * attribution to a selected branch silently drops a plan that does not exist on it.
 */
function customPlanFileSource() {
  return {
    async standardPlan(planId: string) {
      const decoded = decodePlanId(planId);
      if (!decoded) {
        return null;
      }
      const repos = await fetchRepos();
      const repo = repos.find((candidate) => candidate.id === decoded.repo_id);
      if (!repo) {
        return null;
      }
      const params = new URLSearchParams();
      params.set('repo_id', repo.id);
      params.set('branch', repo.default_branch);
      const plans = await apiGet<S['PlanDto'][]>(`/plans?${params.toString()}`);
      const match = plans.find((plan) => plan.path === decoded.path);
      if (!match) {
        return null;
      }
      return { repo_id: match.repo_id, plan_path: match.path, test_files: match.test_files ?? [] };
    },
    async customPlanFiles(planId: string) {
      try {
        const plan = await apiGet<S['CustomPlanDto']>(`/custom-plans/${planId}`);
        return plan.files ?? [];
      } catch {
        return null;
      }
    },
  };
}

/**
 * Create a custom plan.
 *
 * `included_plans` is **expanded into `files`** before the write — see
 * `expandCustomPlanFiles`. Without that expansion the editor's "Whole plans" tab saves
 * cleanly and the plan then runs nothing, because `UpsertCustomPlanReq` has no
 * `included_plans` field to store it in. `description`, `product_id`, `nodes` and
 * `parallelism` have nowhere to go either (row 46); the first two are already gone from
 * the UI and the DAG pair is §8-C6.
 */
export function useCreateCustomPlan() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: CreateCustomPlanForm): Promise<CustomPlan> => {
      const files = await expandCustomPlanFiles(data, customPlanFileSource());
      return customPlanFromDto(
        await apiPost<S['CustomPlanDto']>('/custom-plans', customPlanReq(data, files))
      );
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.customPlans });
    },
  });
}

/**
 * Replace a custom plan. As `useCreateCustomPlan`, plus two things:
 *
 *  - the plan's own id seeds the cycle guard, so an `included_plans` entry pointing at the
 *    plan being saved terminates instead of recursing;
 *  - the current row is **read first**, so the replace does not clear `tags` and
 *    `timeout_seconds`. The PUT is a replace and both fields are optional on the request,
 *    so omitting them wipes them; the UI has no field for either, so anything set outside
 *    this UI would have disappeared on the next unrelated edit. Same read-then-replace this
 *    file already does for `useUpdateSchedule` and `useUpdateNotificationsConfig`, for the
 *    same reason.
 */
export function useUpdateCustomPlan() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ id, data }: { id: string; data: CreateCustomPlanForm }): Promise<CustomPlan> => {
      const [files, current] = await Promise.all([
        expandCustomPlanFiles({ ...data, id }, customPlanFileSource()),
        apiGet<S['CustomPlanDto']>(`/custom-plans/${id}`),
      ]);
      return customPlanFromDto(
        await apiPut<S['CustomPlanDto']>(`/custom-plans/${id}`, customPlanReq(data, files, current))
      );
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.customPlans });
    },
  });
}

export function useDeleteCustomPlan() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => apiDelete<void>(`/custom-plans/${id}`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.customPlans });
    },
  });
}

/** Launch a custom plan. `RunTargetDto.custom_plan_id` is *"Required for `custom_plan`"*,
 *  and a `repo_id` alongside it is accepted and ignored, so `targetFromPlanId` sends only
 *  the id (row 49). */
export function useRunCustomPlan() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ id, platform, branch, parameters, exclusive }: { id: string; platform?: string; branch?: string; scheduleId?: string; parameters?: RunParameter[]; exclusive?: boolean }): Promise<LaunchResponse> => {
      const platformId = platform ? await resolvePlatformId(platform) : null;
      const body = launchReqFromForm({ planId: id, platformId, branch, parameters, exclusive });
      return launchResponseFromDto(await apiPost<S['RunDto'] | S['QueuedRunDto']>('/runs', body));
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.runs });
      queryClient.invalidateQueries({ queryKey: queryKeys.dashboard });
    },
  });
}

// Platforms
/**
 * The product's platforms.
 *
 * `GET /qa/v1/platforms` has no `product_id` filter, but `PlatformDto.product_id` is on
 * the row, so this is §7.6's honest client-side filter — with the one decision §7.6 flags:
 * **what to do with `null`**. A platform with no product is included. It belongs to no
 * product rather than to another one, so including it leaks nothing; and excluding it
 * would empty every platform picker in a deployment where `product_id` is null on every
 * platform (which it is on this stack), turning "run a plan" into a control with no
 * options and no error. That is a judgment call, not a fact the gear states, and it is
 * flagged as one.
 */
export function usePlatforms() {
  const product = useActiveProduct();
  const productId = product?.id ?? '';
  return useQuery({
    queryKey: [...queryKeys.platforms, productId],
    queryFn: async (): Promise<PlatformInfo[]> => {
      const platforms = await fetchPlatformDtos();
      const scoped = productId
        ? platforms.filter((platform) => !platform.product_id || platform.product_id === productId)
        : platforms;
      return scoped.map(platformFromDto);
    },
    enabled: !!product,
  });
}

/**
 * One platform's detail page.
 *
 * `GET /qa/v1/platforms/{id}` serves the `PlatformInfo` half exactly. The **cluster**
 * half was a partial substitute (§7.11) before Task 5 — status, six node counts,
 * `namespace_count`, `checked_at`, `nodes[]` had no source anywhere in the gears (§8-C3),
 * so it was left absent rather than zero-filled, and Task 8a removed the panels that
 * would have rendered it.
 *
 * qa-environments now observes the cluster and serves it as `PlatformDto.cluster`
 * (`dto.rs:75`), and `platformDetailsFromDto` maps it through `platformFromDto` /
 * `clusterHealthFromDto` like any other platform read. `cluster` on the result is
 * `null` when no observation cycle has reached this platform yet, and a populated
 * `ClusterHealth` — including the `status: "Unreachable"` case, where a cycle DID reach
 * the platform and could not read the cluster — once one has. See `ClusterHealth`'s own
 * doc in `types.ts` for what each shape means; `ClusterHealthCard.tsx` is what renders
 * the distinction.
 *
 * X1 on the name.
 */
export function usePlatformDetails(name: string) {
  return useQuery({
    queryKey: queryKeys.platformDetails(name),
    queryFn: async (): Promise<PlatformDetails> => {
      const platformId = await resolvePlatformId(name);
      return platformDetailsFromDto(await apiGet<S['PlatformDto']>(`/platforms/${platformId}`));
    },
    enabled: !!name,
    refetchInterval: 30000,
  });
}

export function useCreatePlatform() {
  const queryClient = useQueryClient();

  return useMutation({
    // A pasted kubeconfig is forwarded, not refused: `createPlatformReqFromForm` decodes
    // the dialog's base64 and sends the YAML as `kubeconfig`, which the gear writes to
    // credstore before any row exists, storing (and returning) only the reference. A
    // caller who already holds a reference still gets `kubeconfig_credstore_ref`.
    mutationFn: async (data: CreatePlatformForm): Promise<void> => {
      await apiPost<S['PlatformDto']>('/platforms', createPlatformReqFromForm(data));
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.platforms });
    },
  });
}

/** PUT -> PATCH (row 53): `UpdatePlatformReq` is all-optional, a true partial, so only the
 *  keys the form carries are sent and an omitted one is left alone. */
export function useUpdatePlatform() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ name, data }: { name: string; data: UpdatePlatformForm }): Promise<PlatformInfo> => {
      const platformId = await resolvePlatformId(name);
      return platformFromDto(
        await apiPatch<S['PlatformDto']>(`/platforms/${platformId}`, updatePlatformReqFromForm(data))
      );
    },
    onSuccess: (_result, variables) => {
      queryClient.invalidateQueries({ queryKey: queryKeys.platforms });
      queryClient.invalidateQueries({ queryKey: queryKeys.platformDetails(variables.name) });
    },
  });
}

export function useDeletePlatform() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (name: string): Promise<void> => {
      await apiDelete<void>(`/platforms/${await resolvePlatformId(name)}`);
    },
    onSuccess: (_result, name) => {
      queryClient.invalidateQueries({ queryKey: queryKeys.platforms });
      queryClient.removeQueries({ queryKey: queryKeys.platformDetails(name) });
    },
  });
}

/** The dedicated rename verb collapses into the partial update: `new_name` -> `name` on
 *  `PATCH /qa/v1/platforms/{id}` (row 56, a reshape spec §6.3 does not list). */
export function useRenamePlatform() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ name, newName }: { name: string; newName: string }): Promise<PlatformInfo> => {
      const platformId = await resolvePlatformId(name);
      return platformFromDto(await apiPatch<S['PlatformDto']>(`/platforms/${platformId}`, { name: newName }));
    },
    onSuccess: (_result, variables) => {
      queryClient.invalidateQueries({ queryKey: queryKeys.platforms });
      queryClient.invalidateQueries({ queryKey: queryKeys.platformDetails(variables.name) });
    },
  });
}

/**
 * Run one on-demand detection cycle against a platform's own cluster and return the
 * refreshed row.
 *
 * `POST /qa/v1/platforms/{id}/refresh` answers HTTP 200 even when detection itself
 * failed — the gear records the failure in `version_detect_error` on the returned row
 * rather than failing the request (`dto.rs:56-60`: "reported as HTTP 200 with
 * version_detect_error populated, never as an error response"). This mutation therefore
 * always "succeeds" from React Query's point of view; the returned row is what tells a
 * real detection success from a failure. Same discipline `89b55eb22` established for
 * `POST /test-repos/{id}/sync`'s `sync_error` — the caller must read the row, not just the
 * status code, before announcing anything to the user.
 */
export function useRefreshPlatform() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (name: string): Promise<PlatformDetails> => {
      const platformId = await resolvePlatformId(name);
      return platformDetailsFromDto(await apiPost<S['PlatformDto']>(`/platforms/${platformId}/refresh`));
    },
    onSuccess: (result, name) => {
      queryClient.setQueryData(queryKeys.platformDetails(name), result);
      queryClient.invalidateQueries({ queryKey: queryKeys.platforms });
    },
  });
}

// Product Folders (top-level dirs containing test plans)
export function useProductFolders() {
  return useQuery({
    queryKey: ['productFolders'] as const,
    // `{folders: string[]}` -> `string[]` (§7.2, row 57).
    queryFn: async (): Promise<string[]> =>
      (await apiGet<S['ProductFolderListDto']>('/product-folders')).folders ?? [],
  });
}

// Products
export function useProducts() {
  return useQuery({
    queryKey: queryKeys.products,
    queryFn: async (): Promise<Product[]> =>
      (await apiGet<S['ProductDto'][]>('/products')).map(productFromDto),
  });
}

/** qa-catalog serves `POST`/`PUT`/`DELETE /qa/v1/products{/id}` but no single-product
 *  `GET`, so this is §7.7's list-as-single-read — over the list `useProducts` already
 *  caches. */
export function useProduct(id: string) {
  return useQuery({
    queryKey: queryKeys.product(id),
    queryFn: async (): Promise<Product> => {
      const products = await fetchProductDtos();
      const match = products.find((product) => product.id === id);
      if (!match) {
        throw new Error(`No product with id "${id}" exists in this deployment.`);
      }
      return productFromDto(match);
    },
    enabled: !!id,
  });
}

/**
 * Coverage points for one product.
 *
 * `GET /qa/v1/dashboard/coverage` declares **zero** query parameters (verified against the
 * live document) and answers one point per product for the whole deployment. So `productId`
 * survives, but its meaning changes: it is no longer a *server* filter, it is the key for a
 * **client-side** one (§7.6, the same treatment `useTestRepositories` and `usePlatforms`
 * get). The narrowing is not optional — the only consumer is a per-product card captioned
 * *"Coverage belongs to this product…"* (`ProductCoverageCard.tsx:96`), so an unnarrowed
 * list would put another product's numbers in this product's chart under a caption saying
 * it could not happen.
 *
 * The gear sends `product_key`, not `product_id`, so the id is resolved to a key against
 * the product list first.
 *
 * The array is **empty in every deployment today** — nothing in this system measures a
 * coverage point (§8-C8) — and the live consumer already renders an honest empty state, so
 * no zero is displayed. That is a deployment fact rather than a code guarantee, which is
 * why the filter is real rather than resting on the emptiness.
 */
export function useProductCoverage(productId: string) {
  return useQuery({
    queryKey: queryKeys.productCoverage(productId),
    queryFn: async (): Promise<ProductCoveragePoint[]> => {
      const [rows, products] = await Promise.all([
        apiGet<S['CoverageBuildDto'][]>('/dashboard/coverage'),
        fetchProductDtos(),
      ]);
      const product = products.find((candidate) => candidate.id === productId);
      return coveragePointsFromDto(rows, product);
    },
    enabled: !!productId,
  });
}

// Resolve the active product (from the selectedProduct store) to its full
// Product object so call sites get both `id` and `key`. Returns undefined when
// nothing is selected or the products list hasn't loaded yet.
export function useActiveProduct(): Product | undefined {
  const [selectedId] = useSelectedProduct();
  const { data: products } = useProducts();
  if (!selectedId || !products) return undefined;
  return products.find((p) => p.id === selectedId);
}

export function useCreateProduct() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: CreateProductForm): Promise<Product> =>
      productFromDto(await apiPost<S['ProductDto']>('/products', productReqFromForm(data))),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.products });
    },
  });
}

export function useUpdateProduct() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ id, data }: { id: string; data: CreateProductForm }): Promise<Product> =>
      productFromDto(await apiPut<S['ProductDto']>(`/products/${id}`, productReqFromForm(data))),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.products });
    },
  });
}

export function useDeleteProduct() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => apiDelete<void>(`/products/${id}`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.products });
    },
  });
}

/**
 * Versions observed for a product, from `PlatformDto.observed_version`.
 *
 * **The semantics change** (row 65). Legacy returned the distinct `app_version` values
 * seen *in run results*; this is the version currently observed *on a platform*, one per
 * platform. It is `null` on every platform in this deployment, because nothing here
 * observes a platform version at all — so this list is empty and the Analytics version
 * dropdown is empty.
 *
 * **And nothing on screen explains why.** An earlier revision of this comment said
 * "§9's banner is what tells the reader why"; that is false, and Task 12's gate is what
 * disproved it. The banner is gated on `noExecutionDataCount !== null`
 * (`components/analytics/AnalyticsDashboard.tsx:968`), which is assigned only inside
 * `if (!overviewLoading && !overviewError && overview)` (`:950`) — and `overview` is
 * never populated here, because `overviewQuery` is `null` whenever no version is
 * selected (`:669-670`) and that `null` is passed straight through as
 * `enabled: !!overviewQuery` (`:704`), reaching the `enabled` that `useAnalyticsOverview`
 * (below in THIS file) hands to `useQuery` — cited by symbol rather than by line on
 * purpose: a line number into the file the comment itself lives in is invalidated by any
 * edit to the comment, and this one has now gone stale twice for exactly that reason.
 * A disabled query has no `data`. So with an empty dropdown the
 * overview request is **never issued at all** and the banner **cannot render**: the page
 * paints its filter controls and nothing else. Driven, not read — the gate recorded
 * `/analytics` issuing four `/qa/v1` requests (`products`, `platforms`, `test-repos` and one
 * `test-repos/{id}/branches`), **none of them `analytics/overview`**, and rendering no banner
 * text. `/analytics/plan/:planId` records the identical four.
 *
 * That is §9.3's own chosen remedy failing to fire in exactly the case its copy
 * describes. The decision is §9's and is a human's; the fix is a component edit no task
 * on this plan is authorised to make, so it is stated here rather than worked around.
 * **Do not hardcode a fallback version**: the smoke script passes `version=unknown` as a
 * probe, and shipping that as a default would present a fabricated filter as a real one.
 */
export function useObservedProductVersions(productId: string) {
  return useQuery({
    queryKey: ['products', productId, 'observed-versions'],
    queryFn: async (): Promise<string[]> => {
      const platforms = await fetchPlatformDtos();
      const scoped = platforms.filter(
        (platform) => !platform.product_id || platform.product_id === productId
      );
      return distinctObservedVersions(scoped);
    },
    enabled: !!productId,
  });
}

/**
 * Branches for a product's Analytics filter — a **documented substitution** (§7.10), not
 * an equivalent.
 *
 * §5-E: spec §6.3 category 2 mapped this onto `PlatformDto.observed_build`, and a build is
 * not a branch. Nothing in the gears aggregates the distinct branches *seen in results*
 * (`TestResultDto.branch` exists but is not in `TestResultsField`, so it can be neither
 * filtered nor grouped). The nearest real source is each repository's **git** branches,
 * which is a **superset**: the filter will offer branches with no data, and selecting one
 * yields an empty view rather than a wrong number. Nothing is fabricated, but the control
 * means something slightly different, which is why it is recorded rather than done
 * quietly.
 */
export function useObservedProductBranches(productId: string) {
  return useQuery({
    queryKey: ['products', productId, 'observed-branches'],
    queryFn: async (): Promise<string[]> => {
      const repos = reposForProduct(await fetchRepos(), productId);
      const settled = await Promise.allSettled(
        repos.map((repo) => apiGet<S['BranchListDto']>(`/test-repos/${repo.id}/branches`))
      );
      const branches = new Set<string>();
      for (const result of settled) {
        if (result.status === 'fulfilled') {
          for (const branch of result.value.branches ?? []) branches.add(branch);
        }
      }
      return [...branches].sort((a, b) => a.localeCompare(b));
    },
    enabled: !!productId,
  });
}

// Analytics
/**
 * The three plan drill-downs move plan identity from the path into the query
 * (§6.3 row 3), and the parameter is spelled `plan_id` but carries the plan's **path**
 * (X6) — same name, different value space, and no repository disambiguation, so a path
 * that exists in two repositories matches both. `plan_id` is **required** on all three;
 * a value naming nothing answers an empty array, not a 404.
 */
export function useTestAnalytics(planId: string) {
  return useQuery({
    queryKey: queryKeys.analytics(planId),
    queryFn: async (): Promise<TestAnalytics[]> => {
      const qs = `plan_id=${encodeURIComponent(analyticsPlanId(planId))}`;
      return (await apiGet<S['PlanTestAnalyticsDto'][]>(`/analytics/plan/tests?${qs}`)).map(
        testAnalyticsFromDto
      );
    },
    enabled: !!planId,
  });
}

export function useBuildDistribution(planId: string) {
  return useQuery({
    queryKey: queryKeys.analyticsBuilds(planId),
    queryFn: async (): Promise<BuildDistribution[]> => {
      const qs = `plan_id=${encodeURIComponent(analyticsPlanId(planId))}`;
      // Field-identical (`build, total, passed, failed, skipped`), so `BuildDistribution`
      // is an alias of the generated DTO and no reshape is needed (row 68).
      return apiGet<BuildDistribution[]>(`/analytics/plan/builds?${qs}`);
    },
    enabled: !!planId,
  });
}

export function useTestHistory(planId: string) {
  return useQuery({
    queryKey: queryKeys.analyticsHistory(planId),
    queryFn: async (): Promise<TestHistory[]> => {
      const qs = `plan_id=${encodeURIComponent(analyticsPlanId(planId))}`;
      return (await apiGet<S['PlanTestHistoryDto'][]>(`/analytics/plan/test-history?${qs}`)).map(
        testHistoryFromDto
      );
    },
    enabled: !!planId,
  });
}

/** Every parameter *name* survives from legacy (§2 fact 4), but `plan_id`'s **value** is
 *  the plan's path (X6), so the opaque id is unpacked on the way out. `scope`/`group_by`
 *  are matched case-insensitively; `days_heatmap` clamps to 1–30 and `days_trend` to
 *  7–365, both silently. */
function buildAnalyticsQueryParams(query: AnalyticsOverviewQuery): URLSearchParams {
  const params = new URLSearchParams();
  params.set('product_id', query.product_id);
  params.set('version', query.version);
  params.set('scope', query.scope);
  if (query.plan_id) params.set('plan_id', analyticsPlanId(query.plan_id));
  if (query.branch) params.set('branch', query.branch);
  if (query.days_heatmap) params.set('days_heatmap', String(query.days_heatmap));
  if (query.days_trend) params.set('days_trend', String(query.days_trend));
  if (query.group_by) params.set('group_by', query.group_by);
  if (query.group_value) params.set('group_value', query.group_value);
  return params;
}

export function analyticsQueryToKey(query: AnalyticsOverviewQuery): string {
  return buildAnalyticsQueryParams(query).toString();
}

/** `product_id` and `version` are both required by the gear (`buildAnalyticsQueryParams`
 *  sends them unconditionally), so a query missing either can only 400. `enabled` alone
 *  cannot guard that: it only gates react-query's own scheduling (mount, window refocus,
 *  `refetchInterval`) and React Query's `refetch()` — which is exactly what the
 *  Analytics page's Refresh button calls (`AnalyticsDashboard.tsx`) — deliberately
 *  bypasses `enabled` and runs `queryFn` anyway. So the refusal has to live inside
 *  `queryFn`, before `apiGet` is reached, to hold no matter who calls `refetch()` or
 *  with what `enabled` value. */
function assertAnalyticsQueryComplete(query: { product_id: string; version: string }): void {
  if (!query.product_id || !query.version) {
    throw new Error(
      'Analytics overview needs both a product and a version selected; pick one of each before refreshing.'
    );
  }
}

export function useAnalyticsOverview(query: AnalyticsOverviewQuery, enabled = true) {
  const queryKey = analyticsQueryToKey(query);
  return useQuery({
    queryKey: queryKeys.analyticsOverview(queryKey),
    queryFn: async (): Promise<AnalyticsOverview> => {
      assertAnalyticsQueryComplete(query);
      return analyticsOverviewFromDto(await apiGet<S['AnalyticsOverviewDto']>(`/analytics/overview?${queryKey}`), {
        plan_id: query.plan_id,
      });
    },
    enabled,
  });
}

function buildAnalyticsBuildTestsParams(query: AnalyticsBuildTestsQuery): URLSearchParams {
  const params = new URLSearchParams();
  params.set('product_id', query.product_id);
  params.set('version', query.version);
  params.set('scope', query.scope);
  params.set('build', query.build);
  if (query.plan_id) params.set('plan_id', analyticsPlanId(query.plan_id));
  if (query.branch) params.set('branch', query.branch);
  if (query.group_by) params.set('group_by', query.group_by);
  if (query.group_value) params.set('group_value', query.group_value);
  return params;
}

export function useAnalyticsBuildTests(query: AnalyticsBuildTestsQuery, enabled = true) {
  const queryKey = buildAnalyticsBuildTestsParams(query).toString();
  return useQuery({
    queryKey: queryKeys.analyticsBuildTests(queryKey),
    queryFn: async (): Promise<BuildTestDetailItem[]> => {
      // Same defect and same fix as `useAnalyticsOverview` above: `buildAnalyticsBuildTestsParams`
      // sends `product_id`/`version` unconditionally, and `enabled` cannot stop a caller's
      // `refetch()`.
      assertAnalyticsQueryComplete(query);
      return (await apiGet<S['BuildTestDetailDto'][]>(`/analytics/build-tests?${queryKey}`)).map(
        buildTestDetailFromDto
      );
    },
    enabled,
  });
}

/**
 * Download an analytics export.
 *
 * It now goes through `client.ts` (`apiGetBlob`). Before this task it used a raw
 * `fetch('/api/analytics/export?…')`, which hardcoded the pre-migration prefix — so
 * re-basing `API_BASE_URL` onto `/qa/v1` did not reach it — and sat outside the single
 * `Authorization` injection point, which would have made it the one request in the app
 * that went out unauthenticated once Phase C turns auth on (§4a).
 *
 * Behaviour worth knowing before trusting the file: an unrecognised `format` answers JSON
 * rather than an error; an unrecognised `section` is a 400 on the JSON branch and a **200
 * with an empty body** on the CSV branch; `build_distribution`, `quality_vectors` and
 * `grouped` are reachable only via `section=all`; and the CSV `summary` block carries only
 * `total/passed/failed/not_run` and their percentages — i.e. exactly the fields that are 0
 * by construction here, and **not** `case_expected` — so a CSV export from this deployment
 * is all zeros with no banner attached to it (§9).
 */
export async function exportAnalytics(
  query: AnalyticsOverviewQuery,
  format: 'csv' | 'json',
  section: 'summary' | 'lists' | 'heatmap' | 'trend' | 'flaky' | 'all' = 'all'
) {
  const params = buildAnalyticsQueryParams(query);
  params.set('format', format);
  params.set('section', section);
  return apiGetBlob(`/analytics/export?${params.toString()}`);
}

/**
 * A caller's saved analytics views.
 *
 * `plan_id` becomes `(repo_id, plan_path)`, and both halves are **required together** when
 * `scope=plan` (X6), so an id that does not decode sends neither.
 *
 * `ownerId` is now **inert**. `handlers/saved_views.rs:60-73` takes the owner from the
 * `SecurityContext` — the bearer token's subject — and the endpoint's own doc says *"A view
 * is visible only to the caller who created it"*, so the `X-Analytics-Owner` header is not
 * read and legacy's ability to read another owner's views is gone. The parameter is kept
 * because it still gates the query the way it did (and removing it would change every call
 * site), but nothing is sent for it.
 */
export function useAnalyticsSavedViews(
  ownerId: string,
  scope: AnalyticsScope,
  planId?: string,
  enabled = true
) {
  return useQuery({
    queryKey: queryKeys.analyticsSavedViews(scope, planId),
    queryFn: async (): Promise<AnalyticsSavedView[]> => {
      const params = new URLSearchParams();
      params.set('scope', scope);
      const decoded = planId ? decodePlanId(planId) : null;
      if (decoded) {
        params.set('repo_id', decoded.repo_id);
        params.set('plan_path', decoded.path);
      }
      return (await apiGet<S['SavedViewDto'][]>(`/analytics/views?${params.toString()}`)).map(
        savedViewFromDto
      );
    },
    enabled: enabled && !!ownerId,
  });
}

/** `409` on a duplicate `(owner, scope, plan, name)`. A plan supplied with `scope=all` is
 *  *"accepted but not stored"* rather than rejected. `query_json` stays opaque — this gear
 *  never inspects it — so a legacy `plan_id` inside one is **not** reconciled with the new
 *  vocabulary (row 73). */
export function useCreateAnalyticsSavedView(ownerId: string) {
  const queryClient = useQueryClient();
  // Inert — see `useAnalyticsSavedViews`. Kept in the signature so no call site changes,
  // and referenced here so `noUnusedParameters` stays on for the rest of the file.
  void ownerId;
  return useMutation({
    mutationFn: async (payload: AnalyticsSavedViewPayload): Promise<AnalyticsSavedView> =>
      savedViewFromDto(await apiPost<S['SavedViewDto']>('/analytics/views', savedViewReqFromPayload(payload))),
    onSuccess: (data) => {
      queryClient.invalidateQueries({
        queryKey: queryKeys.analyticsSavedViews(data.scope, data.plan_id || undefined),
      });
    },
  });
}

export function useUpdateAnalyticsSavedView(ownerId: string) {
  const queryClient = useQueryClient();
  // Inert — see `useAnalyticsSavedViews`.
  void ownerId;
  return useMutation({
    mutationFn: async ({ id, payload }: { id: string; payload: AnalyticsSavedViewPayload }): Promise<AnalyticsSavedView> =>
      savedViewFromDto(
        await apiPut<S['SavedViewDto']>(`/analytics/views/${id}`, savedViewReqFromPayload(payload))
      ),
    onSuccess: (data) => {
      queryClient.invalidateQueries({
        queryKey: queryKeys.analyticsSavedViews(data.scope, data.plan_id || undefined),
      });
    },
  });
}

export function useDeleteAnalyticsSavedView(ownerId: string, scope: AnalyticsScope, planId?: string) {
  const queryClient = useQueryClient();
  // Inert — see `useAnalyticsSavedViews`.
  void ownerId;
  return useMutation({
    mutationFn: (id: string) => apiDelete<void>(`/analytics/views/${id}`),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: queryKeys.analyticsSavedViews(scope, planId),
      });
    },
  });
}

// JIRA Settings
export function useJiraConfig() {
  return useQuery({
    queryKey: queryKeys.jiraConfig,
    // `api_token` is now `api_token_credstore_ref` (X7): what this form shows and saves is
    // a *reference*, never a token.
    queryFn: async (): Promise<JiraConfig> =>
      jiraConfigFromDto(await apiGet<S['JiraSettingsDto']>('/settings/jira')),
  });
}

export function useUpdateJiraConfig() {
  const queryClient = useQueryClient();

  return useMutation({
    // `envelope: void -> the saved config` (row 77). The return is discarded to keep the
    // hook's shape, and the invalidation below is what refreshes the form.
    mutationFn: async (data: JiraConfig): Promise<void> => {
      await apiPut<S['JiraSettingsDto']>('/settings/jira', jiraConfigReq(data));
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.jiraConfig });
    },
  });
}

/** Global pipeline variables. `{variables: […]}` -> a bare `VariableDto[]` on the wire,
 *  re-enveloped here (row 82). Asking without `platform_id` returns exactly the global
 *  rows, driven live. `secure` is gone from both sides — §8-C10, closed by Task 8a by
 *  removing the affordance rather than by defaulting the field. */
export function usePipelineVariables() {
  return useQuery({
    queryKey: queryKeys.pipelineVariables,
    queryFn: async (): Promise<PipelineVariablesConfig> => ({
      variables: partitionPlatformVariables(await apiGet<S['VariableDto'][]>('/variables'), null),
    }),
  });
}

/**
 * Save the global variables set.
 *
 * The granularity **inverts** (row 83): the editor hands over the whole list, and the gear
 * upserts one row at a time by natural key plus a separate delete. So this re-reads the
 * current set, diffs it, and issues one PUT per changed row and one DELETE per removed
 * row — see `variableWritePlan`.
 *
 * **Not atomic.** A failure part-way leaves the set half-written, which legacy's whole-set
 * PUT could not do. There is no rollback to offer, so the error is surfaced as-is; the
 * `onSuccess` invalidation re-reads whatever actually landed. Deletes run after the
 * upserts so that an interruption leaves extra rows rather than missing ones.
 */
function saveVariables(platformId: string | null) {
  return async (data: PipelineVariablesConfig): Promise<void> => {
    const query = platformId ? `?platform_id=${encodeURIComponent(platformId)}` : '';
    const current = await apiGet<S['VariableDto'][]>(`/variables${query}`);
    const scoped = current.filter((row) => (platformId ? row.platform_id === platformId : !row.platform_id));
    const plan = variableWritePlan(scoped, data.variables ?? [], platformId);
    for (const upsert of plan.upserts) {
      await apiPut<S['VariableDto']>('/variables', upsert);
    }
    for (const id of plan.deletes) {
      await apiDelete<void>(`/variables/${id}`);
    }
  };
}

export function useUpdatePipelineVariables() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: saveVariables(null),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.pipelineVariables });
    },
  });
}

/**
 * One platform's variables.
 *
 * `platform_id` is a real, honoured parameter — driven live: a bogus uuid is a 404 and a
 * non-uuid is a 400, so it is read rather than silently ignored. But its semantics are
 * **additive**: *"List global pipeline variables, **plus** a platform's variables when
 * `platform_id` is given"*, confirmed live (three seeded rows: no parameter answered the
 * global row only; `?platform_id=p1` answered global + p1; `?platform_id=p2` answered
 * global + p2). Legacy's `/platforms/{name}/variables` returned the platform's **own**
 * set, so the server filter alone is a superset and §7.6's client-side partition is still
 * needed on top of it (row 84).
 */
export function usePlatformVariables(platformName: string) {
  return useQuery({
    queryKey: ['platforms', platformName, 'variables'] as const,
    queryFn: async (): Promise<PipelineVariablesConfig> => {
      const platformId = await resolvePlatformId(platformName);
      const rows = await apiGet<S['VariableDto'][]>(
        `/variables?platform_id=${encodeURIComponent(platformId)}`
      );
      return { variables: partitionPlatformVariables(rows, platformId) };
    },
    enabled: !!platformName,
  });
}

/** As `useUpdatePipelineVariables` and `usePlatformVariables` combined (row 85). */
export function useUpdatePlatformVariables(platformName: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (data: PipelineVariablesConfig): Promise<PipelineVariablesConfig> => {
      const platformId = await resolvePlatformId(platformName);
      await saveVariables(platformId)(data);
      const rows = await apiGet<S['VariableDto'][]>(
        `/variables?platform_id=${encodeURIComponent(platformId)}`
      );
      return { variables: partitionPlatformVariables(rows, platformId) };
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: ['platforms', platformName, 'variables'],
      });
    },
  });
}

export function useNotificationsConfig() {
  return useQuery({
    queryKey: queryKeys.notificationsConfig,
    // `slack_webhook_url` -> `slack_webhook_credstore_ref` (X7), so the input holds a
    // reference. The gear's extra required `run_queue_queued_slack_enabled` has no UI
    // field; it is round-tripped on write rather than dropped (row 86).
    queryFn: async (): Promise<NotificationsConfig> =>
      notificationsConfigFromDto(await apiGet<S['NotificationConfigDto']>('/settings/notifications')),
  });
}

export function useUpdateNotificationsConfig() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (data: NotificationsConfig): Promise<void> => {
      // Re-read before writing: `run_queue_queued_slack_enabled` is required on the DTO
      // and absent from this form, so writing without the current value would silently
      // clear a notification the operator turned on elsewhere.
      const current = await apiGet<S['NotificationConfigDto']>('/settings/notifications');
      await apiPut<S['NotificationConfigDto']>(
        '/settings/notifications',
        notificationsConfigReq(data, current)
      );
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.notificationsConfig });
    },
  });
}

/**
 * Send a test notification.
 *
 * A **bodyless POST gains a required body** (row 88): the gear has one test endpoint where
 * legacy had two shapes on the same path, and this hook is the bodyless one, so it cannot
 * be called as written. It is absorbed by sending the currently-saved config plus an
 * event. The event has to be *some* value — `NotificationTestReq.event` is required — and
 * `succeeded` is used because a test message describing a success is the least alarming
 * thing to deliver to a real Slack channel. That choice is this hook's, not the gear's;
 * `useTestScheduledRunNotification` is the variant that lets the caller pick.
 */
export function useTestNotification() {
  return useMutation({
    mutationFn: async (): Promise<void> => {
      const config = await apiGet<S['NotificationConfigDto']>('/settings/notifications');
      await apiPost<S['NotificationTestOutcomeDto']>('/settings/notifications/test', {
        config,
        event: 'succeeded',
      } satisfies S['NotificationTestReq']);
    },
  });
}

export function usePreviewScheduledRunNotification() {
  return useMutation({
    mutationFn: async (
      data: ScheduledRunNotificationPreviewRequest
    ): Promise<ScheduledRunNotificationPreviewResponse> => {
      // The request is identical modulo `config`'s own shape difference (row 89), so the
      // saved config is re-read and the form's edits are laid over it — the same
      // round-trip `useUpdateNotificationsConfig` needs, for the same field.
      const current = await apiGet<S['NotificationConfigDto']>('/settings/notifications');
      const preview = await apiPost<S['NotificationPreviewDto']>('/settings/notifications/preview', {
        config: notificationsConfigReq(data.config, current),
        event: data.event,
      } satisfies S['NotificationPreviewReq']);
      return notificationPreviewFromDto(preview);
    },
  });
}

export function useTestScheduledRunNotification() {
  return useMutation({
    mutationFn: async (data: ScheduledRunNotificationPreviewRequest): Promise<void> => {
      const current = await apiGet<S['NotificationConfigDto']>('/settings/notifications');
      await apiPost<S['NotificationTestOutcomeDto']>('/settings/notifications/test', {
        config: notificationsConfigReq(data.config, current),
        event: data.event,
      } satisfies S['NotificationTestReq']);
    },
  });
}

export function useNotificationLog(limit = 100) {
  return useQuery({
    queryKey: [...queryKeys.notificationLog, limit],
    queryFn: async (): Promise<NotificationLogEntry[]> =>
      (await apiGet<S['NotificationLogEntryDto'][]>(`/settings/notifications/log?limit=${limit}`)).map(
        notificationLogEntryFromDto
      ),
    refetchInterval: 15000,
  });
}

export function useJiraPollerConfig() {
  return useQuery({
    queryKey: queryKeys.jiraPollerConfig,
    // Field-identical both sides (row 92), so `JiraPollerConfig` is an alias of the
    // generated DTO and nothing is reshaped.
    queryFn: () => apiGet<JiraPollerConfig>('/settings/jira-poller'),
  });
}

export function useUpdateJiraPollerConfig() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (data: JiraPollerConfig): Promise<void> => {
      await apiPut<JiraPollerConfig>('/settings/jira-poller', data);
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.jiraPollerConfig });
    },
  });
}

/** File JIRA bugs for a run. Filing and listing split (§6.3 row 7): this is the POST, and
 *  the run moves from a *path segment* to a `run_id` **uuid in the body**, so the name has
 *  to be resolved first (X1). The response element is unchanged — `{jira_key, created}`. */
export function useCreateJiraTicket() {
  return useMutation({
    mutationFn: async ({ runName, testName }: { runName: string; testName?: string }): Promise<JiraCreateResponse[]> => {
      const runId = await resolveRunId(runName);
      return apiPost<JiraCreateResponse[]>('/jira/bugs', {
        run_id: runId,
        test_name: testName ?? null,
      } satisfies S['FileJiraBugsReq']);
    },
  });
}

/**
 * Open JIRA bugs, for one plan or for all of them.
 *
 * The listing half of §6.3 row 7, keyed on **`(repo_id, plan_path)`, not `plan_id`**. The
 * two are a co-requirement: `/openapi.json` marks neither individually required, and
 * driven live, omitting both is a 200 while supplying one alone is a 400 —
 * *"repo_id and plan_path must be supplied together, or not at all"*. So the "omit for all
 * bugs" branch omits **both**, which is what `openBugsQuery` returns for an id it cannot
 * decode. Unlike the analytics drill-downs' path-only match, this one is exact.
 */
export function useOpenBugs(planId?: string) {
  return useQuery({
    queryKey: [...queryKeys.jiraBugs, planId],
    queryFn: async (): Promise<JiraBug[]> => {
      const qs = openBugsQuery(planId);
      const path = qs ? `/jira/open-bugs?${qs}` : '/jira/open-bugs';
      return (await apiGet<S['JiraBugDto'][]>(path)).map(jiraBugFromDto);
    },
  });
}
