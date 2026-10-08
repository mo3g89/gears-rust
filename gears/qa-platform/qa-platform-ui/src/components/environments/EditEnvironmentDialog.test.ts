// @vitest-environment jsdom
//
// The Edit Environment dialog's product picker offers only real products.
//
// `qa_environments.product_id` is `NOT NULL` and the gear refuses an empty
// `product_id` with 400, so a "No product" option (which sent `product_id: ''`)
// could only ever end in an error toast. The dialog also used to say version and
// build are "not observed in this deployment"; the gear writes both
// (`observed_version`, `observed_build`), so that caption was false.
//
// `.test.ts`, not `.test.tsx`: `vitest.config.ts`'s `include` glob is
// `src/**/*.test.ts` only, so `createElement` stands in for JSX.
import { createElement } from 'react';
import { MemoryRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
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

vi.mock('sonner', () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

import { apiGet } from '@/api/client';
import { queryClient as sharedQueryClient } from '@/api/queryClient';
import { EnvironmentInfo } from '@/api/types';
import { EditEnvironmentDialog } from './EditEnvironmentDialog';

const mockedApiGet = vi.mocked(apiGet);
const PROD_1 = '11111111-1111-4111-8111-111111111111';
const PROD_2 = '22222222-2222-4222-8222-222222222222';

function product(id: string, key: string, name: string) {
  return {
    id,
    key,
    name,
    description: '',
    folder: null,
    plugin_instance_id: null,
    created_at: '2026-08-01T00:00:00Z',
    updated_at: '2026-08-01T00:00:00Z',
  };
}

const ENVIRONMENT: EnvironmentInfo = {
  id: '33333333-3333-4333-8333-333333333333',
  name: 'prod-cluster',
  created_at: '2026-08-01T00:00:00Z',
  description: '',
  product_id: PROD_1,
  version: '8.4.1',
  build: '17',
  available: true,
  observed_attrs: {},
  health_state: 'ok',
  health_detail: null,
  version_detected_at: null,
  version_detect_error: null,
  is_default: false,
  default_branch: null,
};

function renderDialog() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(MemoryRouter, null, createElement(EditEnvironmentDialog, { environment: ENVIRONMENT }))
    )
  );
}

// jsdom has no `ResizeObserver`; Radix's `Switch` measures itself with one.
class NoopResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}

beforeEach(() => {
  vi.stubGlobal('ResizeObserver', NoopResizeObserver);
  sharedQueryClient.clear();
  mockedApiGet.mockReset();
  mockedApiGet.mockImplementation(async (path: string) => {
    if (path === '/products') {
      return [product(PROD_1, 'ONE', 'One'), product(PROD_2, 'TWO', 'Two')] as never;
    }
    return [] as never;
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('EditEnvironmentDialog', () => {
  it('offers exactly the products, with no "No product" option', async () => {
    renderDialog();
    fireEvent.click(screen.getByRole('button', { name: /edit/i }));

    // The trigger (named by its "Product" label) shows the environment's current
    // product once the product list loads.
    await screen.findByText('One (ONE)');
    fireEvent.click(screen.getByRole('button', { name: 'Product' }));

    await screen.findByRole('button', { name: 'Two (TWO)' });
    const optionRows = screen
      .getAllByRole('button')
      .filter((button) => button.getAttribute('id') !== 'product' && button.closest('[role="dialog"]'))
      .map((button) => button.textContent?.trim() ?? '')
      .filter((label) => label !== '' && !/^(cancel|save changes|close)$/i.test(label));
    expect(optionRows).toEqual(['One (ONE)', 'Two (TWO)']);
    expect(screen.queryByText(/no product/i)).toBeNull();
  });

  it('does not claim version and build go unobserved, and never disables the default switch for a missing product', async () => {
    renderDialog();
    fireEvent.click(screen.getByRole('button', { name: /edit/i }));
    await screen.findByText('One (ONE)');

    expect(screen.queryByText(/not observed/i)).toBeNull();
    expect(screen.queryByText(/cannot be a product/i)).toBeNull();
    expect((screen.getByRole('switch') as HTMLButtonElement).disabled).toBe(false);
  });
});
