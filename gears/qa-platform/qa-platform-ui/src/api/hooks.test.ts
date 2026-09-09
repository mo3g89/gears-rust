// @vitest-environment jsdom
//
// Bug: the Analytics "Refresh" button calls `refetch()` directly
// (`AnalyticsDashboard.tsx:1078`), and React Query's `refetch()` deliberately ignores a
// query's `enabled` flag. `AnalyticsDashboard` disables `useAnalyticsOverview` by gating
// its `enabled` argument on `selectedProductId && selectedVersion`, but that gate is a
// react-query concern, not an HTTP one — `refetch()` walks straight past it and the hook
// falls back to `{ product_id: '', version: '', ... }` (`AnalyticsDashboard.tsx:697-704`),
// which used to reach `apiGet` and produce
// `GET /qa/v1/analytics/overview?product_id=&version=&scope=all` -> 400.
//
// The fix has to live in the hook itself (`src/api/hooks.ts`), not in the caller's
// `enabled` wiring, because `enabled` is exactly what `refetch()` bypasses. So this suite
// drives the hooks directly with `refetch()` — never toggling `enabled` — and asserts the
// mocked `apiGet` was never called for an incomplete query.
import { createElement } from 'react';
import type { ReactNode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('./client', () => ({
  apiGet: vi.fn(),
  apiGetBlob: vi.fn(),
  apiPost: vi.fn(),
  apiDelete: vi.fn(),
  apiPut: vi.fn(),
  apiPatch: vi.fn(),
}));

import { apiGet } from './client';
import { queryClient as sharedQueryClient } from './queryClient';
import { setSelectedProduct } from '@/lib/selectedProduct';
import {
  queryKeys,
  useAnalyticsOverview,
  useAnalyticsBuildTests,
  useDashboard,
  useEnvironmentDetails,
  usePipelineVariables,
  useRuns,
  useSchedules,
} from './hooks';
import type { AnalyticsOverviewQuery, AnalyticsBuildTestsQuery } from './types';

const mockedApiGet = vi.mocked(apiGet);

function makeWrapper() {
  // One fresh client per test, retries off -- a retry would just repeat whatever
  // the first call already proved.
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return function Wrapper({ children }: { children: ReactNode }) {
    return createElement(QueryClientProvider, { client }, children);
  };
}

const EMPTY_OVERVIEW_QUERY: AnalyticsOverviewQuery = {
  product_id: '',
  version: '',
  scope: 'all',
};

const FULL_OVERVIEW_QUERY: AnalyticsOverviewQuery = {
  product_id: 'prod-1',
  version: '1.2.3',
  scope: 'all',
};

const EMPTY_BUILD_TESTS_QUERY: AnalyticsBuildTestsQuery = {
  product_id: '',
  version: '',
  scope: 'all',
  build: 'build-9',
};

const FULL_BUILD_TESTS_QUERY: AnalyticsBuildTestsQuery = {
  product_id: 'prod-1',
  version: '1.2.3',
  scope: 'all',
  build: 'build-9',
};

beforeEach(() => {
  // `fetchEnvironmentDtos` caches in the app's SHARED client (it is not a hook
  // and has no provider to read one from), so unlike `makeWrapper`'s per-test
  // client that cache outlives a test. Cleared here so one test's environments
  // cannot answer the next one's request.
  sharedQueryClient.clear();
  mockedApiGet.mockReset();
  // `useAnalyticsOverview`'s adapter wants an object it can pick fields off of;
  // `useAnalyticsBuildTests`'s calls `.map` straight on the response, so it needs an
  // array. Branching on the path keeps one mock honest for both "still fetches
  // normally" cases without asserting anything about response *content*, which these
  // tests are not about.
  mockedApiGet.mockImplementation(async (path: string) =>
    path.includes('/analytics/build-tests') ? [] : ({} as never)
  );
});

afterEach(() => {
  // `selectedProduct`'s module-level store outlives any one test's wrapper/client,
  // so a test that selects a product must clear it again or the next test inherits it.
  setSelectedProduct('');
  vi.restoreAllMocks();
});

describe('useAnalyticsOverview', () => {
  it('never sends an HTTP request when refetch() is called on an empty product_id/version query, even though the caller left `enabled` true', async () => {
    // Mirrors AnalyticsDashboard.tsx:697-704's fallback object exactly: `enabled` is
    // passed as `true` here on purpose, the same way a stale closure or a caller that
    // forgets to gate `enabled` would leave it. The hook itself must still refuse.
    const { result } = renderHook(() => useAnalyticsOverview(EMPTY_OVERVIEW_QUERY, true), {
      wrapper: makeWrapper(),
    });

    result.current.refetch();
    result.current.refetch();

    await waitFor(() => expect(result.current.isError || result.current.isFetched).toBe(true));

    expect(mockedApiGet).not.toHaveBeenCalled();
  });

  it('still fetches normally once product_id and version are both present', async () => {
    const { result } = renderHook(() => useAnalyticsOverview(FULL_OVERVIEW_QUERY, true), {
      wrapper: makeWrapper(),
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(mockedApiGet).toHaveBeenCalledTimes(1);
    const [path] = mockedApiGet.mock.calls[0] as [string];
    expect(path).toContain('product_id=prod-1');
    expect(path).toContain('version=1.2.3');
  });

  it('reproduces the exact production shape -- enabled:false (as AnalyticsDashboard passes when the gate is unmet) plus a direct refetch() call, the way the Refresh button drives it -- and still sends nothing', async () => {
    const { result } = renderHook(() => useAnalyticsOverview(EMPTY_OVERVIEW_QUERY, false), {
      wrapper: makeWrapper(),
    });

    // The button's handler is exactly `() => refetch()`; React Query's refetch()
    // ignores `enabled` by design, which is the entire bug.
    result.current.refetch();

    await waitFor(() => expect(result.current.isError || result.current.isFetched).toBe(true));

    expect(mockedApiGet).not.toHaveBeenCalled();
  });
});

describe('useAnalyticsBuildTests', () => {
  it('never sends an HTTP request when refetch() is called on an empty product_id/version query', async () => {
    const { result } = renderHook(() => useAnalyticsBuildTests(EMPTY_BUILD_TESTS_QUERY, true), {
      wrapper: makeWrapper(),
    });

    result.current.refetch();

    await waitFor(() => expect(result.current.isError || result.current.isFetched).toBe(true));

    expect(mockedApiGet).not.toHaveBeenCalled();
  });

  it('still fetches normally once product_id and version are both present', async () => {
    const { result } = renderHook(() => useAnalyticsBuildTests(FULL_BUILD_TESTS_QUERY, true), {
      wrapper: makeWrapper(),
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(mockedApiGet).toHaveBeenCalledTimes(1);
  });
});

describe('fetchEnvironmentDtos (the environment name index)', () => {
  /** `GET /qa/v1/environments` as it answers since review finding #55: a page. */
  const ENVIRONMENTS_PAGE = {
    items: [
      { id: 'env-uuid-1', name: 'alpha' },
      { id: 'env-uuid-2', name: 'beta' },
    ],
    page_info: { limit: 200, next_cursor: null, prev_cursor: null },
  };

  function mockEnvironments() {
    mockedApiGet.mockImplementation(async (path: string) =>
      path === '/environments' ? (ENVIRONMENTS_PAGE as never) : ({ id: 'env-uuid-1', name: 'alpha' } as never)
    );
  }

  /** How many times `/environments` itself was requested. */
  function environmentListCalls() {
    return mockedApiGet.mock.calls.filter(([path]) => path === '/environments').length;
  }

  it('reads the page shape rather than a bare array, so a name still resolves to an id', async () => {
    mockEnvironments();
    const { result } = renderHook(() => useEnvironmentDetails('alpha'), { wrapper: makeWrapper() });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(mockedApiGet.mock.calls.map(([path]) => path)).toContain('/environments/env-uuid-1');
  });

  it('does not repeat the fetch within the stale window -- the regression finding #55 measured', async () => {
    // `useRun(name, 5000)` reaches this through `fetchRunDetails` every 5 s per
    // open Run Detail page, and `useDashboard` every 15 s. Two consumers here
    // stand in for two polls: before the cache, each was its own request for the
    // whole fleet's state.
    mockEnvironments();

    const first = renderHook(() => useEnvironmentDetails('alpha'), { wrapper: makeWrapper() });
    await waitFor(() => expect(first.result.current.isSuccess).toBe(true));

    const second = renderHook(() => useEnvironmentDetails('beta'), { wrapper: makeWrapper() });
    await waitFor(() => expect(second.result.current.isSuccess).toBe(true));

    expect(environmentListCalls()).toBe(1);
  });

  it('does fetch again once an environment mutation has invalidated it', async () => {
    // Every environment mutation calls
    // `invalidateQueries({ queryKey: queryKeys.environments })`, and
    // `queryKeys.environmentDtos` is a child of that key, so the prefix match
    // drops this cache too. That is what keeps a rename or a create from being
    // hidden behind the staleTime -- assert the prefix key, not the exact one.
    mockEnvironments();

    const first = renderHook(() => useEnvironmentDetails('alpha'), { wrapper: makeWrapper() });
    await waitFor(() => expect(first.result.current.isSuccess).toBe(true));
    expect(environmentListCalls()).toBe(1);

    await sharedQueryClient.invalidateQueries({ queryKey: queryKeys.environments });

    const second = renderHook(() => useEnvironmentDetails('beta'), { wrapper: makeWrapper() });
    await waitFor(() => expect(second.result.current.isSuccess).toBe(true));

    expect(environmentListCalls()).toBe(2);
  });
});

describe('fetchVariableDtos (the Settings -> Variables editor)', () => {
  /**
   * Two pages of pipeline variables, the way the gear serves them without an
   * `environment_id`: a single table, so `next_cursor` is real.
   *
   * Task 24 review finding 3: the first version of `fetchVariableDtos`
   * discarded that cursor, so the editor rendered a first page as if it were
   * the whole variable set. `variableWritePlan` derives deletes only from the
   * `previous` array it is handed, so nothing was destroyed by it -- but the
   * operator was editing a list that claimed to be the set and was not.
   */
  function mockTwoPagesOfVariables() {
    mockedApiGet.mockImplementation(async (path: string) => {
      if (path === '/variables') {
        return {
          items: [{ id: 'v1', name: 'ALPHA', value: 'a', environment_id: null }],
          page_info: { limit: 200, next_cursor: 'page-2-token', prev_cursor: null },
        } as never;
      }
      if (path.startsWith('/variables?cursor=')) {
        return {
          items: [{ id: 'v2', name: 'BETA', value: 'b', environment_id: null }],
          page_info: { limit: 200, next_cursor: null, prev_cursor: null },
        } as never;
      }
      throw new Error(`unexpected apiGet(${path})`);
    });
  }

  it('follows next_cursor, so the editor sees the whole set rather than the first page', async () => {
    mockTwoPagesOfVariables();

    const { result } = renderHook(() => usePipelineVariables(), { wrapper: makeWrapper() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(result.current.data?.variables?.map((row) => row.name)).toEqual(['ALPHA', 'BETA']);
    expect(mockedApiGet).toHaveBeenCalledTimes(2);
  });

  it('sends the cursor as a bare `cursor` parameter, which is what the gear extractor reads', async () => {
    // Not `$skiptoken`: `ODataParams` renames only `$filter`/`$orderby`/`$select`
    // and takes `limit` and `cursor` bare. A wrong spelling here is not an error
    // -- it is an ignored parameter and an infinite first page.
    mockTwoPagesOfVariables();

    const { result } = renderHook(() => usePipelineVariables(), { wrapper: makeWrapper() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(mockedApiGet.mock.calls.map(([path]) => path)).toEqual([
      '/variables',
      '/variables?cursor=page-2-token',
    ]);
  });

  it('stops after one request when the gear returns no cursor, which is the union case', async () => {
    // With `environment_id` the response is the union of two tables and the gear
    // returns `next_cursor: null` -- and refuses a `cursor` sent alongside it.
    // So this path must not invent a second request.
    mockedApiGet.mockImplementation(async () =>
      ({
        items: [{ id: 'v1', name: 'ALPHA', value: 'a', environment_id: null }],
        page_info: { limit: 500, next_cursor: null, prev_cursor: null },
      }) as never
    );

    const { result } = renderHook(() => usePipelineVariables(), { wrapper: makeWrapper() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(mockedApiGet).toHaveBeenCalledTimes(1);
  });
});

// Two products worth of fixtures, shared by `useRuns` and `useSchedules`'s scoping
// suites below: a run and a schedule both resolve their product through a repository,
// never through an environment (`productScope.ts`), so both hooks' tests need the same
// two repos and the same "only one product in the switcher" setup.
const PROD_1 = '11111111-1111-4111-8111-111111111111';
const PROD_2 = '22222222-2222-4222-8222-222222222222';

function runDto(name: string, repoId: string) {
  return {
    id: `run-${name}`,
    name,
    target: { kind: 'plan', repo_id: repoId, path: 'plan.yaml' },
    state: 'succeeded',
    environment_id: null,
    parameters: [],
    resolved_exclusive: false,
  };
}

/** A schedule targeting a repository directly -- carries `target.repo_id`, the field
 *  `scheduleFromDto` copies onto `ScheduleInfo.repo_id` so it resolves the same way
 *  `runDto` above does, without a custom-plan detour. */
function scheduleDto(name: string, repoId: string) {
  return {
    id: `sched-${name}`,
    name,
    target: { kind: 'plan', repo_id: repoId, path: 'plan.yaml' },
    cron: '0 0 * * *',
    enabled: true,
    environment_id: null,
    slack_notifications_enabled: false,
  };
}

/** A run targeting a custom plan by id -- no `repo_id` of its own, unlike `runDto`
 *  above, which is exactly why `productIdOfRow` has to walk through the custom
 *  plan's own tests instead. */
function customPlanRunDto(name: string, customPlanId: string) {
  return {
    id: `run-${name}`,
    name,
    target: { kind: 'custom_plan', custom_plan_id: customPlanId },
    state: 'succeeded',
    environment_id: null,
    parameters: [],
    resolved_exclusive: false,
  };
}

/** The raw `CustomPlanDto` shape (`files`, not `tests` -- `customPlanFromDto` renames
 *  it): one file per given repo id, so a plan "belongs to" whichever product(s) those
 *  repos belong to once resolved. */
function customPlanDto(id: string, repoIds: string[]) {
  return {
    id,
    name: id,
    files: repoIds.map((repoId, i) => ({ path: `t${i}.py`, plan_path: `plan-${i}.yaml`, repo_id: repoId })),
    tags: [],
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  };
}

/**
 * Wires `/test-repos`, `/custom-plans` and `/products` the same way for every
 * product-scoped list hook's test (`useRuns` and `useSchedules` below):
 * two repositories split across `PROD_1`/`PROD_2` and a products list that names
 * only `PROD_1` -- the fixture a caller pairs with `setSelectedProduct(PROD_1)`
 * to reproduce "the switcher has auto-selected the deployment's one product".
 * `rows` supplies the one endpoint each hook actually differs on (`/runs`'s
 * paginated envelope vs. `/schedules`'s bare array), plus `/environments`.
 */
function mockTwoProductsWorthOfRows(rows: Record<string, unknown>) {
  mockedApiGet.mockImplementation((path: string) => {
    for (const prefix of Object.keys(rows)) {
      if (path.startsWith(prefix)) return Promise.resolve(rows[prefix]);
    }
    if (path.startsWith('/test-repos')) {
      return Promise.resolve([
        { id: 'repo-a', name: 'a', product_id: PROD_1 },
        { id: 'repo-b', name: 'b', product_id: PROD_2 },
      ]);
    }
    if (path.startsWith('/custom-plans')) return Promise.resolve([]);
    if (path.startsWith('/products')) {
      return Promise.resolve([{ id: PROD_1, key: 'p1', name: 'One' }]);
    }
    return Promise.resolve([]);
  });
}

describe('useRuns (product scoping)', () => {
  function mockTwoProductsWorthOfRuns() {
    mockTwoProductsWorthOfRows({
      '/runs': {
        items: [runDto('mine', 'repo-a'), runDto('theirs', 'repo-b')],
        page_info: { next_cursor: null },
      },
      // `fetchEnvironmentDtos` reads `.items` off this (Page_EnvironmentDto, not a
      // bare array) -- see the `ENVIRONMENTS_PAGE` fixture above.
      '/environments': { items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } },
    });
  }

  it('useRuns returns only the active product’s runs', async () => {
    setSelectedProduct(PROD_1);
    mockTwoProductsWorthOfRuns();

    const { result } = renderHook(() => useRuns(), { wrapper: makeWrapper() });
    await waitFor(() => expect(result.current.data).toBeDefined());

    const names = result.current.data!.map((r) => r.name);
    expect(names).toContain('mine');
    // The load-bearing half: a filter that does nothing passes the line above
    // and fails this one.
    expect(names).not.toContain('theirs');
  });

  // Round 2 review finding: `CustomPlan.product_id` is always `null` in this gear (no
  // such field exists), so a custom-plan run resolves through its tests' repositories
  // instead (`productScope.ts`). These two are the vacuity proof for that fix: the same
  // run construction, unanimous for the foreign product vs. unanimous for the selected
  // one.
  it('excludes a custom-plan run whose tests all belong to a foreign product', async () => {
    setSelectedProduct(PROD_1);
    mockTwoProductsWorthOfRows({
      '/runs': {
        items: [customPlanRunDto('foreign-plan-run', 'cp-foreign')],
        page_info: { next_cursor: null },
      },
      '/custom-plans': [customPlanDto('cp-foreign', ['repo-b', 'repo-b'])],
      '/environments': { items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } },
    });

    const { result } = renderHook(() => useRuns(), { wrapper: makeWrapper() });
    await waitFor(() => expect(result.current.data).toBeDefined());

    expect(result.current.data!.map((r) => r.name)).not.toContain('foreign-plan-run');
  });

  it('includes a custom-plan run whose tests unanimously belong to the selected product', async () => {
    setSelectedProduct(PROD_1);
    mockTwoProductsWorthOfRows({
      '/runs': {
        items: [customPlanRunDto('mine-plan-run', 'cp-mine')],
        page_info: { next_cursor: null },
      },
      '/custom-plans': [customPlanDto('cp-mine', ['repo-a', 'repo-a'])],
      '/environments': { items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } },
    });

    const { result } = renderHook(() => useRuns(), { wrapper: makeWrapper() });
    await waitFor(() => expect(result.current.data).toBeDefined());

    expect(result.current.data!.map((r) => r.name)).toContain('mine-plan-run');
  });
});

/**
 * A promise whose settlement this test controls, standing in for a request
 * still in flight. `setTimeout` would make the race a matter of timer ordering;
 * this makes it a matter of the test's own sequencing, which cannot flake.
 */
function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

const REPOS_TWO_PRODUCTS = [
  { id: 'repo-a', name: 'a', product_id: PROD_1 },
  { id: 'repo-b', name: 'b', product_id: PROD_2 },
];

const ENVIRONMENTS_EMPTY_PAGE = {
  items: [],
  page_info: { limit: 200, next_cursor: null, prev_cursor: null },
};

/**
 * The C1 race, made deterministic: every endpoint answers at once except
 * `/test-repos`, which answers only when the test says so.
 *
 * This is the ordering the reviewer reproduced against the shipped code with a
 * plain 80 ms delay — `GET /runs` resolving while `GET /test-repos` is still in
 * flight, which is the ordinary case on a cold page load, since `/runs` is one
 * request and the repository listing races it.
 */
function mockRunsWinningTheRaceAgainstTestRepos(rows: Record<string, unknown>) {
  const repos = deferred<unknown>();
  const settled = { rows: false };
  mockedApiGet.mockImplementation((path: string) => {
    for (const prefix of Object.keys(rows)) {
      if (path.startsWith(prefix)) {
        // Recorded rather than inferred: "the rows response has settled" is the
        // precondition of the race, and it is the one thing the composed result
        // deliberately does not expose (`ProductScopedQueryResult` carries no
        // `isFetched`, because passing query fields through is what produced the
        // flag inconsistency this shape exists to prevent).
        return Promise.resolve(rows[prefix]).then((value) => {
          settled.rows = true;
          return value;
        });
      }
    }
    if (path.startsWith('/test-repos')) return repos.promise;
    if (path.startsWith('/custom-plans')) return Promise.resolve([]);
    if (path.startsWith('/products')) return Promise.resolve([{ id: PROD_1, key: 'p1', name: 'One' }]);
    if (path.startsWith('/environments')) return Promise.resolve(ENVIRONMENTS_EMPTY_PAGE);
    return Promise.resolve([]);
  });
  return { repos, settled };
}

/** Drain every already-resolved promise and let React commit what they produced.
 *  A macrotask boundary is what makes this total: the remaining work after a
 *  response settles is a bounded chain of microtasks. */
async function flushSettledWork() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

// ---------------------------------------------------------------------------
// C1 — the filter must not be able to cache an empty list.
//
// The scope used to be computed INSIDE `queryFn`, against `repos`,
// `customPlans` and `standardPlans` captured from sibling queries — none of
// which was in the query key. When `/runs` resolved first, every row resolved
// to no product, the empty array was *cached*, and React Query re-rendered on
// the lookups arriving without re-running `queryFn`. The page then showed "No
// test runs found" with `isLoading: false` and no error, recovering only on the
// next poll — which `RunsPage` lets the user switch off, and which `staleTime`
// re-served across navigation in the meantime.
//
// Both tests below fail against that implementation: the first because the list
// never recovers, the second because "still resolving" was reported as an empty
// answer rather than as loading.
// ---------------------------------------------------------------------------
describe('useRuns / useSchedules — the scope is a view over the rows, not part of the fetch', () => {
  it('useRuns still lists the product’s runs when /test-repos resolves after /runs', async () => {
    setSelectedProduct(PROD_1);
    const { repos, settled } = mockRunsWinningTheRaceAgainstTestRepos({
      '/runs': {
        items: [runDto('mine', 'repo-a'), runDto('theirs', 'repo-b')],
        page_info: { next_cursor: null },
      },
    });

    const { result } = renderHook(() => useRuns(), { wrapper: makeWrapper() });
    // `/runs` has come back and the repositories have not: the exact ordering
    // that used to cache `[]`.
    await waitFor(() => expect(settled.rows).toBe(true));
    await flushSettledWork();

    repos.resolve(REPOS_TWO_PRODUCTS);

    await waitFor(() => expect(result.current.data?.length).toBe(1));
    const names = result.current.data!.map((r) => r.name);
    expect(names).toContain('mine');
    // The load-bearing half, unchanged from the non-racing test above: a filter
    // that does nothing passes the line before this one and fails this one.
    expect(names).not.toContain('theirs');
  });

  it('useRuns reports the lookups as loading rather than answering with an empty list', async () => {
    setSelectedProduct(PROD_1);
    const { settled } = mockRunsWinningTheRaceAgainstTestRepos({
      '/runs': {
        items: [runDto('mine', 'repo-a'), runDto('theirs', 'repo-b')],
        page_info: { next_cursor: null },
      },
    });

    const { result } = renderHook(() => useRuns(), { wrapper: makeWrapper() });
    await waitFor(() => expect(settled.rows).toBe(true));
    await flushSettledWork();

    // `[]` here is not a smaller answer, it is a wrong one: `RunsPage` falls
    // through its `if (isLoading)` guard and renders "No test runs found".
    expect(result.current.data).toBeUndefined();
    expect(result.current.isLoading).toBe(true);
    // And every flag says the same thing, because they are one state.
    expect(result.current.status).toBe('pending');
    expect(result.current.isSuccess).toBe(false);
    expect(result.current.isError).toBe(false);
  });

  it('useSchedules recovers the same way', async () => {
    setSelectedProduct(PROD_1);
    const { repos, settled } = mockRunsWinningTheRaceAgainstTestRepos({
      '/schedules': [scheduleDto('mine', 'repo-a'), scheduleDto('theirs', 'repo-b')],
    });

    const { result } = renderHook(() => useSchedules(), { wrapper: makeWrapper() });
    await waitFor(() => expect(settled.rows).toBe(true));
    await flushSettledWork();

    repos.resolve(REPOS_TWO_PRODUCTS);

    await waitFor(() => expect(result.current.data?.length).toBe(1));
    expect(result.current.data!.map((s) => s.name)).toEqual(['mine']);
  });

  // ---------------------------------------------------------------------
  // The regression this shape's first version introduced. `isLoading` was
  // synthesised as `query.isLoading || (lookupError === null && scoped ===
  // undefined)`. A failed ROWS query is `status: 'error'` in v5, so
  // `query.isLoading` is false — but `scoped` is undefined (no rows to filter)
  // and no lookup failed, so the synthesised flag came out true, and both pages
  // test `isLoading` before `error`. The panel never rendered.
  //
  // The existing lookup-failure test below could not catch it: it fails
  // `/test-repos`, which sets `lookupError` and short-circuits the very term
  // that was wrong.
  // ---------------------------------------------------------------------
  it('useRuns reports a failed /runs as an error, not as still loading', async () => {
    setSelectedProduct(PROD_1);
    mockedApiGet.mockImplementation((path: string) => {
      if (path.startsWith('/runs')) return Promise.reject(new Error('runs is down'));
      if (path.startsWith('/test-repos')) return Promise.resolve(REPOS_TWO_PRODUCTS);
      if (path.startsWith('/custom-plans')) return Promise.resolve([]);
      if (path.startsWith('/products')) return Promise.resolve([{ id: PROD_1, key: 'p1', name: 'One' }]);
      if (path.startsWith('/environments')) return Promise.resolve(ENVIRONMENTS_EMPTY_PAGE);
      return Promise.resolve([]);
    });

    const { result } = renderHook(() => useRuns(), { wrapper: makeWrapper() });

    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error?.message).toBe('runs is down');
    // The load-bearing assertion: `RunsPage` checks this before `error`.
    expect(result.current.isLoading).toBe(false);
    expect(result.current.status).toBe('error');
    expect(result.current.isPending).toBe(false);
    expect(result.current.isSuccess).toBe(false);
  });

  it('useSchedules reports a failed /schedules as an error, not as still loading', async () => {
    setSelectedProduct(PROD_1);
    mockedApiGet.mockImplementation((path: string) => {
      if (path.startsWith('/schedules')) return Promise.reject(new Error('schedules is down'));
      if (path.startsWith('/test-repos')) return Promise.resolve(REPOS_TWO_PRODUCTS);
      if (path.startsWith('/custom-plans')) return Promise.resolve([]);
      if (path.startsWith('/products')) return Promise.resolve([{ id: PROD_1, key: 'p1', name: 'One' }]);
      if (path.startsWith('/environments')) return Promise.resolve(ENVIRONMENTS_EMPTY_PAGE);
      return Promise.resolve([]);
    });

    const { result } = renderHook(() => useSchedules(), { wrapper: makeWrapper() });

    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error?.message).toBe('schedules is down');
    expect(result.current.isLoading).toBe(false);
    expect(result.current.status).toBe('error');
  });

  // The trap the flag rewrite closes, stated as its own assertion: no
  // combination of these five may claim data that is not there. `isSuccess`
  // used to be passed through from the rows query, so it read `true` while
  // `data` was `undefined` and `isLoading` was `true` — the ordinary v5 idiom
  // `if (isSuccess) data.map(...)` would have thrown.
  it('never reports success while the scope is still resolving', async () => {
    setSelectedProduct(PROD_1);
    const { settled } = mockRunsWinningTheRaceAgainstTestRepos({
      '/runs': { items: [runDto('mine', 'repo-a')], page_info: { next_cursor: null } },
    });

    const { result } = renderHook(() => useRuns(), { wrapper: makeWrapper() });
    await waitFor(() => expect(settled.rows).toBe(true));
    await flushSettledWork();

    expect(result.current.isSuccess).toBe(result.current.data !== undefined);
    expect(result.current.status).toBe('pending');
    expect(result.current.isPending).toBe(result.current.isLoading);
  });

  it('surfaces a failed /test-repos as an error rather than spinning forever', async () => {
    setSelectedProduct(PROD_1);
    mockedApiGet.mockImplementation((path: string) => {
      if (path.startsWith('/runs')) {
        return Promise.resolve({ items: [runDto('mine', 'repo-a')], page_info: { next_cursor: null } });
      }
      if (path.startsWith('/test-repos')) return Promise.reject(new Error('test-repos is down'));
      if (path.startsWith('/custom-plans')) return Promise.resolve([]);
      if (path.startsWith('/products')) return Promise.resolve([{ id: PROD_1, key: 'p1', name: 'One' }]);
      if (path.startsWith('/environments')) return Promise.resolve(ENVIRONMENTS_EMPTY_PAGE);
      return Promise.resolve([]);
    });

    const { result } = renderHook(() => useRuns(), { wrapper: makeWrapper() });

    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error?.message).toBe('test-repos is down');
    expect(result.current.isLoading).toBe(false);
  });
});

describe('useSchedules (product scoping)', () => {
  function mockTwoProductsWorthOfSchedules() {
    mockTwoProductsWorthOfRows({
      // Bare array, not a paginated envelope -- `useSchedules` calls
      // `apiGet<ScheduleDtoWithEnvironmentId[]>('/schedules')` directly.
      '/schedules': [scheduleDto('mine', 'repo-a'), scheduleDto('theirs', 'repo-b')],
      '/environments': { items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } },
    });
  }

  it('useSchedules returns only the active product’s schedules', async () => {
    setSelectedProduct(PROD_1);
    mockTwoProductsWorthOfSchedules();

    const { result } = renderHook(() => useSchedules(), { wrapper: makeWrapper() });
    await waitFor(() => expect(result.current.data).toBeDefined());

    const names = result.current.data!.map((s) => s.name);
    expect(names).toContain('mine');
    // The load-bearing half: a filter that does nothing passes the line above
    // and fails this one.
    expect(names).not.toContain('theirs');
  });
});

describe('useDashboard (product scoping)', () => {
  // This file's own header names the shape an empty `product_id` used to take:
  // `?product_id=` reached the gear and 400'd. `useDashboard` builds its query
  // string with `URLSearchParams` and only calls `.set('product_id', ...)` when
  // one is actually selected, so these two assertions are the guard that bug
  // fix needs and did not have.
  function mockDashboardRows() {
    mockedApiGet.mockImplementation((path: string) => {
      if (path.startsWith('/dashboard')) return Promise.resolve({});
      if (path.startsWith('/environments')) {
        return Promise.resolve({ items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } });
      }
      if (path.startsWith('/products')) {
        return Promise.resolve([{ id: PROD_1, key: 'p1', name: 'One' }]);
      }
      return Promise.resolve([]);
    });
  }

  function dashboardCallUrl() {
    const call = mockedApiGet.mock.calls.find(([path]) => (path as string).startsWith('/dashboard'));
    return call?.[0];
  }

  it('omits product_id from the query string when no product is selected', async () => {
    mockDashboardRows();

    // `enabled: !!product` is false with no active product, so this drives the
    // query the way this file's header says every product-scoping test here
    // must: through `refetch()`, which bypasses `enabled` exactly as the
    // Analytics "Refresh" button does.
    const { result } = renderHook(() => useDashboard(), { wrapper: makeWrapper() });
    await result.current.refetch();

    await waitFor(() => expect(dashboardCallUrl()).toBeDefined());
    expect(dashboardCallUrl()).toBe('/dashboard?days=14');
  });

  it('includes product_id once a product is selected', async () => {
    setSelectedProduct(PROD_1);
    mockDashboardRows();

    const { result } = renderHook(() => useDashboard(), { wrapper: makeWrapper() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(dashboardCallUrl()).toBe(`/dashboard?days=14&product_id=${PROD_1}`);
  });
});
