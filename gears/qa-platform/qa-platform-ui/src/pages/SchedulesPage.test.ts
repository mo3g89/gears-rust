// @vitest-environment jsdom
//
// A failed `/schedules` shows the error panel, not a spinner.
//
// The regression the product-scoping wave introduced and this pins:
// `useProductScopedRows` synthesised `isLoading` as `query.isLoading ||
// (lookupError === null && scoped === undefined)`. A failed rows query is
// `status: 'error'` in v5, so `query.isLoading` is false — but there are no
// rows to filter, so `scoped` is `undefined`, and no *lookup* failed, so the
// synthesised flag came out `true`. This page tests `isLoading` (`:129`)
// before `error` (`:137`), so the spinner won and "Failed to load schedules"
// never rendered. Measured before the fix:
//
//   schedules-failed:  isError=true error="schedules is down" isLoading=true status=error
//   SchedulesPage:     errorPanelShown=false spinnerShown=true
//
// `.test.ts`, not `.test.tsx`: `vitest.config.ts`'s `include` glob is
// `src/**/*.test.ts` only, so `createElement` stands in for JSX.
import { createElement } from 'react';
import { MemoryRouter } from 'react-router-dom';
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
import { queryClient as sharedQueryClient } from '@/api/queryClient';
import { ConfirmProvider } from '@/components/ui/confirm-dialog';
import { setSelectedProduct } from '@/lib/selectedProduct';
import { SchedulesPage } from './SchedulesPage';

const mockedApiGet = vi.mocked(apiGet);

// `useSchedules` is product-scoped and `enabled: !!product`, so the switcher
// needs a selected product for the query to run at all.
const PROD_1 = '99999999-9999-4999-8999-999999999999';

function renderPage() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(
        ConfirmProvider,
        null,
        createElement(MemoryRouter, null, createElement(SchedulesPage))
      )
    )
  );
}

beforeEach(() => {
  // `fetchEnvironmentDtos` caches in the app's SHARED client, which outlives a
  // test — see `RunsPage.test.ts` for the full reason.
  sharedQueryClient.clear();
  mockedApiGet.mockReset();
  setSelectedProduct(PROD_1);
});

afterEach(() => {
  setSelectedProduct('');
  cleanup();
});

describe('SchedulesPage — a failed /schedules', () => {
  it('renders the error panel rather than a spinner that never stops', async () => {
    mockedApiGet.mockImplementation(async (path: string) => {
      if (path.startsWith('/schedules')) throw new Error('schedules is down');
      if (path === '/environments') {
        return { items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } } as never;
      }
      if (path.startsWith('/test-repos')) {
        return [{ id: 'repo-1', name: 'repo-1', product_id: PROD_1 }] as never;
      }
      if (path.startsWith('/custom-plans')) return [] as never;
      if (path === '/products') return [{ id: PROD_1, key: 'p1', name: 'One' }] as never;
      return [] as never;
    });
    renderPage();

    await waitFor(() => expect(screen.queryByText('Failed to load schedules')).not.toBeNull());
    // The gear's own message, not a generic panel.
    expect(screen.queryByText('schedules is down')).not.toBeNull();
    // And the spinner is gone: the page returns early on `isLoading`, so a
    // still-true flag would have kept the panel unreachable.
    expect(document.querySelector('.animate-spin')).toBeNull();
  });
});
