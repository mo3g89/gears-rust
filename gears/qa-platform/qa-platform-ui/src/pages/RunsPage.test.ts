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
import { RunsPage } from './RunsPage';

const mockedApiGet = vi.mocked(apiGet);

function runDto(overrides: Record<string, unknown> = {}) {
  return {
    id: overrides.id ?? '11111111-1111-1111-1111-111111111111',
    name: 'smoke-1',
    target: { kind: 'plan', repo_id: 'repo-1', path: 'plans/smoke.yaml', test_file: null, custom_plan_id: null },
    platform_id: null,
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
    result: { passed: 10, failed: 0, skipped: 0, in_progress: 0, total: 10 },
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
    if (path === '/platforms') {
      return [] as never;
    }
    if (path === '/products') {
      return [] as never;
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
  mockedApiGet.mockReset();
});

afterEach(cleanup);

describe('RunsPage — status quick-filter chips against real (lowercase) phases', () => {
  it('narrows the table to the succeeded run when the "Succeeded" chip is toggled on', async () => {
    mockApiFor([
      runDto({ id: '11111111-1111-1111-1111-111111111111', name: 'smoke-succeeded', state: 'succeeded' }),
      runDto({ id: '22222222-2222-2222-2222-222222222222', name: 'smoke-failed', state: 'failed', result: { passed: 5, failed: 1, skipped: 0, in_progress: 0, total: 6 } }),
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
