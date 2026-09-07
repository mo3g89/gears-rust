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
import { renderHook, waitFor } from '@testing-library/react';
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
import {
  queryKeys,
  useAnalyticsOverview,
  useAnalyticsBuildTests,
  useEnvironmentDetails,
  usePipelineVariables,
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
