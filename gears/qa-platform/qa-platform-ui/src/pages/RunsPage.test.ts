// @vitest-environment jsdom
//
// UI casing fix: `STATUS_CHIPS` compared `run.phase` against Title-Case
// strings ('Succeeded', 'Failed', ...), but `WorkflowRun.phase` is qa-runs'
// own lowercase `RunState` set — `runFromDto` sets it to `dto.state` "without
// re-casing" (`adapters.ts`, decision X4; `RunState::as_str`,
// qa-runs-sdk/src/models.rs:271-284). This was the worst of the casing bugs:
// selecting any status chip filtered the run list to zero rows and every chip
// count read zero. This suite renders the actual page against a mocked API
// client with real lowercase-phase run fixtures and asserts that picking the
// "Succeeded" chip narrows the table to the succeeded run (and away from the
// failed one), so a regression back to Title Case fails here rather than
// only in a manual check.
//
// `.test.ts`, not `.test.tsx` — `vitest.config.ts`'s `include` glob is
// `src/**/*.test.ts` only, so `createElement` stands in for JSX exactly as
// `RunsTable.test.ts` and `RunDetailPage.test.ts` already do.
import { createElement } from 'react';
import { MemoryRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { queryClient as sharedQueryClient } from '@/api/queryClient';

vi.mock('@/api/client', () => ({
  apiGet: vi.fn(),
  apiGetBlob: vi.fn(),
  apiPost: vi.fn(),
  apiDelete: vi.fn(),
  apiPut: vi.fn(),
  apiPatch: vi.fn(),
  setTokenProvider: vi.fn(),
}));

import { apiGet } from '@/api/client';
import { ConfirmProvider } from '@/components/ui/confirm-dialog';
import { setSelectedProduct } from '@/lib/selectedProduct';
import { RunsPage } from './RunsPage';

const mockedApiGet = vi.mocked(apiGet);

// `useRuns` is now product-scoped (`hooks.ts`): every run fixture below hangs off
// `repo-1`, so the switcher needs exactly one product bound to that repo, selected,
// for the query to be enabled at all and for its rows to resolve to that product.
const PROD_1 = '99999999-9999-4999-8999-999999999999';

function runDto(overrides: Record<string, unknown> = {}) {
  return {
    id: overrides.id ?? '11111111-1111-1111-1111-111111111111',
    name: 'smoke-1',
    target: { kind: 'plan', repo_id: 'repo-1', path: 'plans/smoke.yaml', test_file: null, custom_plan_id: null },
    environment_id: null,
    test_version: 'main',
    app_version: null,
    app_build: null,
    state: 'succeeded',
    resolved_exclusive: false,
    exclusive_tier: 'default',
    is_validation: false,
    parameters: [],
    include_tags: [],
    exclude_tags: [],
    source: 'manual',
    schedule_id: null,
    bundle_ids: [],
    execution_ref: null,
    log_storage_ref: null,
    timeout_at: null,
    started_at: '2026-08-28T10:00:00Z',
    finished_at: '2026-08-28T10:02:00Z',
    error: null,
    created_at: '2026-08-28T09:59:00Z',
    updated_at: '2026-08-28T10:02:00Z',
    result: { passed: 10, failed: 0, skipped: 0, in_progress: 0, xfail: 0, xpass: 0, total: 10 },
    ...overrides,
  };
}

function mockApiFor(runs: unknown[]) {
  mockedApiGet.mockImplementation(async (path: string) => {
    if (path.startsWith('/runs')) {
      return { items: runs, page_info: { next_cursor: null } } as never;
    }
    if (path.startsWith('/queue')) {
      return { items: [], page_info: { next_cursor: null } } as never;
    }
    if (path === '/environments') {
      // A page since review finding #55, empty here.
      return { items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } } as never;
    }
    if (path.startsWith('/test-repos')) {
      return [{ id: 'repo-1', name: 'repo-1', product_id: PROD_1 }] as never;
    }
    if (path.startsWith('/custom-plans')) {
      return [] as never;
    }
    if (path === '/products') {
      return [{ id: PROD_1, key: 'p1', name: 'One' }] as never;
    }
    throw new Error(`unexpected apiGet(${path}) in this test`);
  });
}

function renderPage() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(
        ConfirmProvider,
        null,
        createElement(MemoryRouter, null, createElement(RunsPage))
      )
    )
  );
}

beforeEach(() => {
  // `fetchEnvironmentDtos` caches the environment name index in the app's SHARED
  // query client (`api/queryClient.ts`) rather than in this file's per-test one,
  // because it is a plain function with no provider to read a client from. That
  // cache outlives a test, so it is cleared here -- without this, one test's
  // environments answer the next test's lookup.
  sharedQueryClient.clear();
  mockedApiGet.mockReset();
  setSelectedProduct(PROD_1);
});

afterEach(() => {
  setSelectedProduct('');
  cleanup();
});

describe('RunsPage — status quick-filter chips against real (lowercase) phases', () => {
  it('narrows the table to the succeeded run when the "Succeeded" chip is toggled on', async () => {
    mockApiFor([
      runDto({ id: '11111111-1111-1111-1111-111111111111', name: 'smoke-succeeded', state: 'succeeded' }),
      runDto({ id: '22222222-2222-2222-2222-222222222222', name: 'smoke-failed', state: 'failed', result: { passed: 5, failed: 1, skipped: 0, in_progress: 0, xfail: 0, xpass: 0, total: 6 } }),
    ]);
    renderPage();

    await waitFor(() => expect(screen.queryByText('smoke-succeeded')).not.toBeNull());
    expect(screen.queryByText('smoke-failed')).not.toBeNull();

    // The chip count must be non-zero for the real lowercase phase.
    const succeededChip = await screen.findByText('Succeeded 1');
    fireEvent.click(succeededChip);

    await waitFor(() => expect(screen.queryByText('smoke-failed')).toBeNull());
    expect(screen.queryByText('smoke-succeeded')).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Collect runs are not test runs (legacy parity)
//
// The source system creates a `run_results` row for every collect workflow and
// then keeps it out of the UI: `list_runs_with_history` — the one read path
// behind every run listing and every plan's recent-runs view — drops them,
// *"Collect-only workflows aren't real test runs"*
// (`manager/src/services/run_history.rs:382-386`, `run_source != "collect"`).
// This port had no equivalent, so 24 hourly collect runs a day sat in the runs
// list alongside real test runs. Hidden HERE rather than in `GET /qa/v1/runs`
// on purpose: a listing endpoint that silently omits rows is a trap for every
// other API consumer, and "not a real test run" is a presentation judgement.
// ---------------------------------------------------------------------------
describe('RunsPage — collect runs', () => {
  it('keeps a collect run out of the table while still listing the test run', async () => {
    mockApiFor([
      runDto({ id: '11111111-1111-1111-1111-111111111111', name: 'smoke-1' }),
      runDto({
        id: '33333333-3333-3333-3333-333333333333',
        name: 'collect-repo-58',
        target: { kind: 'collect', repo_id: 'repo-1', path: null, test_file: null, custom_plan_id: null },
      }),
    ]);
    renderPage();

    await waitFor(() => expect(screen.queryByText('smoke-1')).not.toBeNull());
    expect(screen.queryByText('collect-repo-58')).toBeNull();
  });

  it('lists them once the Collect chip is toggled on, and says how many are hidden', async () => {
    mockApiFor([
      runDto({ id: '11111111-1111-1111-1111-111111111111', name: 'smoke-1' }),
      runDto({
        id: '33333333-3333-3333-3333-333333333333',
        name: 'collect-repo-58',
        target: { kind: 'collect', repo_id: 'repo-1', path: null, test_file: null, custom_plan_id: null },
      }),
    ]);
    renderPage();

    const chip = await screen.findByText('Collect 1');
    fireEvent.click(chip);

    await waitFor(() => expect(screen.queryByText('collect-repo-58')).not.toBeNull());
    expect(screen.queryByText('smoke-1')).not.toBeNull();
  });

  it('shows no Collect chip at all when no collect run came back', async () => {
    mockApiFor([runDto({ id: '11111111-1111-1111-1111-111111111111', name: 'smoke-1' })]);
    renderPage();

    await waitFor(() => expect(screen.queryByText('smoke-1')).not.toBeNull());
    expect(screen.queryByText(/^Collect \d+$/)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Ruling G-5: `platform` is a deprecated FQL alias for `environment`, not an
// error. Unlike the OData `$filter` field (G-3), this keyword is typed by an
// operator and persisted in `localStorage['qa:fql:saved:runs']`, so silently
// rejecting the old spelling would turn a saved filter into a silent
// zero-match rather than a visible one. Both spellings must narrow the same
// way, on the same field (`run.platform`).
// ---------------------------------------------------------------------------
describe('RunsPage — FQL filter accepts both `environment` and its deprecated `platform` alias', () => {
  function fixtures() {
    return [
      runDto({
        id: '11111111-1111-1111-1111-111111111111',
        name: 'smoke-staging',
        environment_id: 'env-staging',
      }),
      runDto({
        id: '22222222-2222-2222-2222-222222222222',
        name: 'smoke-prod',
        environment_id: 'env-prod',
      }),
    ];
  }

  it('narrows to the matching run when queried with the canonical `environment` field', async () => {
    mockApiFor(fixtures());
    renderPage();
    await waitFor(() => expect(screen.queryByText('smoke-staging')).not.toBeNull());
    expect(screen.queryByText('smoke-prod')).not.toBeNull();

    const input = await screen.findByPlaceholderText(/status = Failed/);
    fireEvent.change(input, { target: { value: 'environment = env-staging' } });

    await waitFor(() => expect(screen.queryByText('smoke-prod')).toBeNull());
    expect(screen.queryByText('smoke-staging')).not.toBeNull();
  });

  it('narrows the same way when queried with the deprecated `platform` alias', async () => {
    // This is the exact case C-2 found broken: before the alias, `platform` fell to
    // `compileFql`'s unhandled `default:` arm, the accessor returned `undefined`, and
    // `.some(...)` over an empty list matched **zero** runs — not an error, a silent
    // wrong answer indistinguishable from "nothing matched".
    mockApiFor(fixtures());
    renderPage();
    await waitFor(() => expect(screen.queryByText('smoke-staging')).not.toBeNull());

    const input = await screen.findByPlaceholderText(/status = Failed/);
    fireEvent.change(input, { target: { value: 'platform = env-staging' } });

    await waitFor(() => expect(screen.queryByText('smoke-prod')).toBeNull());
    expect(screen.queryByText('smoke-staging')).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// A failed `/runs` shows the error panel, not a spinner.
//
// The regression the product-scoping wave introduced and this pins:
// `useProductScopedRows` synthesised `isLoading` as `query.isLoading ||
// (lookupError === null && scoped === undefined)`. A failed rows query is
// `status: 'error'` in v5, so `query.isLoading` is false — but there are no
// rows to filter, so `scoped` is `undefined`, and no *lookup* failed, so the
// synthesised flag came out `true`. This page tests `isLoading` (`:326`)
// before `error` (`:334`), so the spinner won and "Failed to load test runs"
// never rendered. Measured before the fix:
//
//   runs-failed:  isError=true error="runs is down" isLoading=true status=error
//   RunsPage:     errorPanelShown=false spinnerShown=true
//
// The page assertion is what the user sees; the hook-level pair lives in
// `api/hooks.test.ts`.
// ---------------------------------------------------------------------------
describe('RunsPage — a failed /runs', () => {
  it('renders the error panel rather than a spinner that never stops', async () => {
    mockedApiGet.mockImplementation(async (path: string) => {
      if (path.startsWith('/runs')) throw new Error('runs is down');
      if (path.startsWith('/queue')) return { items: [], page_info: { next_cursor: null } } as never;
      if (path === '/environments') {
        return { items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } } as never;
      }
      if (path.startsWith('/test-repos')) {
        return [{ id: 'repo-1', name: 'repo-1', product_id: PROD_1 }] as never;
      }
      if (path.startsWith('/custom-plans')) return [] as never;
      if (path === '/products') return [{ id: PROD_1, key: 'p1', name: 'One' }] as never;
      throw new Error(`unexpected apiGet(${path}) in this test`);
    });
    renderPage();

    await waitFor(() => expect(screen.queryByText('Failed to load test runs')).not.toBeNull());
    // The message the gear gave, not a generic one — a panel that renders
    // without it would pass the line above.
    expect(screen.queryByText('runs is down')).not.toBeNull();
    // And the spinner is gone: `RunsPage` returns early on `isLoading`, so a
    // still-true flag would have kept the panel unreachable.
    expect(document.querySelector('.animate-spin')).toBeNull();
  });
});
