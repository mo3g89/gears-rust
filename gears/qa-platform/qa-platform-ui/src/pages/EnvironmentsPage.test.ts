// @vitest-environment jsdom
//
// The Environments page says its product filter runs in the browser.
//
// `GET /environments` has no `product_id` parameter, so `useEnvironments`
// filters the rows the page already holds. ADR-0010 requires a browser-scoped
// surface to say so in the UI, as the Runs, Plans and Schedules pages do.
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
import { EnvironmentsPage } from './EnvironmentsPage';

const mockedApiGet = vi.mocked(apiGet);
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
        createElement(MemoryRouter, null, createElement(EnvironmentsPage))
      )
    )
  );
}

beforeEach(() => {
  sharedQueryClient.clear();
  mockedApiGet.mockReset();
  setSelectedProduct(PROD_1);
  mockedApiGet.mockImplementation(async (path: string) => {
    if (path === '/environments') {
      return { items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } } as never;
    }
    if (path === '/products') return [{ id: PROD_1, key: 'p1', name: 'One' }] as never;
    return [] as never;
  });
});

afterEach(() => {
  setSelectedProduct('');
  cleanup();
});

describe('EnvironmentsPage — product scope', () => {
  it('says the product filter runs in the browser', async () => {
    renderPage();
    await waitFor(() => expect(screen.queryByText('Configured Environments')).not.toBeNull());
    expect(screen.queryByText(/filtered in your browser/)).not.toBeNull();
    // `product_id` is NOT NULL in qa-environments, so no environment names no
    // product and the caption must not describe one.
    expect(screen.queryByText(/names no product/)).toBeNull();
  });
});
