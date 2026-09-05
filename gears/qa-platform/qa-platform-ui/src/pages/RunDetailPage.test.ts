// @vitest-environment jsdom
//
// Task 10 review, finding 1 (Critical): `succeededWithSkips` compared
// `run.phase` against `'Succeeded'`, but `WorkflowRun.phase` is qa-runs'
// lowercase state set (`runFromDto` sets it to `dto.state` "without
// re-casing", `adapters.ts:378-383`, decision X4). The header marker and the
// Test Results card's callout were both dead code for any real run. This
// suite renders the actual page against a mocked API client with a
// lowercase-`'succeeded'`, non-zero-`skipped` fixture and asserts both are
// present, so a regression back to Title Case fails here.
//
// `.test.ts`, not `.test.tsx` — `vitest.config.ts`'s `include` glob is
// `src/**/*.test.ts` only, so `createElement` stands in for JSX exactly as
// `hooks.test.ts` and `useRunLogStream.test.ts` already do.
import { createElement } from 'react';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
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
import { RunDetailPage } from './RunDetailPage';

const mockedApiGet = vi.mocked(apiGet);

// `LogViewer` (rendered at the bottom of the page) opens a live stream via
// `useRunLogStream`, which constructs a real `EventSource` — not a jsdom
// global. Stubbed exactly as `useRunLogStream.test.ts` does, so mounting the
// page doesn't throw; nothing here asserts on the stream itself.
class FakeEventSource {
  onmessage: ((e: MessageEvent) => void) | null = null;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  close() {
    /* no-op */
  }
}

const RUN_ID = '11111111-1111-1111-1111-111111111111';

function runDetailDto(overrides: Record<string, unknown> = {}) {
  return {
    id: RUN_ID,
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
    result: { passed: 10, failed: 0, skipped: 68, in_progress: 0, total: 78 },
    test_results: [],
    ...overrides,
  };
}

function mockApiFor(detail: unknown) {
  mockedApiGet.mockImplementation(async (path: string) => {
    if (path.startsWith(`/runs/${RUN_ID}/logs`)) {
      return '' as never;
    }
    if (path.startsWith(`/runs/${RUN_ID}`)) {
      return detail as never;
    }
    if (path.startsWith('/test-case-results')) {
      return { items: [], total: 0, total_pages: 1 } as never;
    }
    if (path === '/environments') {
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
        createElement(
          MemoryRouter,
          { initialEntries: [`/runs/${RUN_ID}`] },
          createElement(Routes, null, createElement(Route, { path: '/runs/:name', element: createElement(RunDetailPage) }))
        )
      )
    )
  );
}

beforeEach(() => {
  (globalThis as unknown as { EventSource: unknown }).EventSource = FakeEventSource;
  mockedApiGet.mockReset();
});

afterEach(cleanup);

describe('RunDetailPage — the succeeded-with-skips marker', () => {
  it('shows both the header marker and the Test Results callout for a real (lowercase) succeeded run with skips', async () => {
    mockApiFor(runDetailDto());
    renderPage();

    await waitFor(() => expect(screen.queryByText('68 skipped')).not.toBeNull());
    expect(
      screen.queryByText(/This run succeeded, but 68 tests were skipped/)
    ).not.toBeNull();
  });

  it('shows neither for a succeeded run with no skips', async () => {
    mockApiFor(runDetailDto({ result: { passed: 10, failed: 0, skipped: 0, in_progress: 0, total: 10 } }));
    renderPage();

    await waitFor(() => expect(screen.queryByText('smoke-1')).not.toBeNull());
    expect(screen.queryByText(/skipped$/)).toBeNull();
    expect(screen.queryByText(/This run succeeded, but/)).toBeNull();
  });
});
