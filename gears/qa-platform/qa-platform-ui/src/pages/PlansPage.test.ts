// @vitest-environment jsdom
//
// The Plans page's two tabs must agree about what the global product switcher
// means. Both are scoped in the browser: the Standard tab lists plans per
// repository after `reposForProduct` narrows the repository list
// (`GET /qa/v1/test-repos` has no `product_id`), and the Custom tab attributes
// each custom plan through the repositories its tests name — `productScope.ts`'s
// resolver, the same one the Runs and Schedules lists use — with the
// hide-unattributable rule applied unchanged. Each says so in the UI (ADR-0010).
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
import { ConfirmProvider } from '@/components/ui/confirm-dialog';
import { queryClient as sharedQueryClient } from '@/api/queryClient';
import { setSelectedProduct } from '@/lib/selectedProduct';
import { PlansPage } from './PlansPage';

const mockedApiGet = vi.mocked(apiGet);

const PROD_1 = '11111111-1111-4111-8111-111111111111';
const PROD_2 = '22222222-2222-4222-8222-222222222222';

/** `customPlanFromDto` reads `files`, and encodes each one's `(repo_id, plan_path)`
 *  into the `plan_id` the resolver decodes back to a repository. */
function customPlanDto(id: string, name: string, repoIds: string[]) {
  return {
    id,
    name,
    files: repoIds.map((repoId, i) => ({ path: `t${i}.py`, plan_path: `plan-${i}.yaml`, repo_id: repoId })),
    tags: [],
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  };
}

function mockApi(customPlans: unknown[]) {
  mockedApiGet.mockImplementation(async (path: string) => {
    if (path.startsWith('/custom-plans')) return customPlans as never;
    if (path.startsWith('/test-repos')) {
      return [
        { id: 'repo-a', name: 'a', product_id: PROD_1, url: '', default_branch: 'main' },
        { id: 'repo-b', name: 'b', product_id: PROD_2, url: '', default_branch: 'main' },
      ] as never;
    }
    if (path.startsWith('/products')) return [{ id: PROD_1, key: 'p1', name: 'One' }] as never;
    if (path.startsWith('/environments')) {
      // `fetchEnvironmentDtos` reads `.items` off a page, not a bare array.
      return { items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } } as never;
    }
    return [] as never;
  });
}

function renderPage(tab: 'custom' | 'standard' = 'custom') {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(
        ConfirmProvider,
        null,
        createElement(MemoryRouter, { initialEntries: [`/plans?tab=${tab}`] }, createElement(PlansPage))
      )
    )
  );
}

beforeEach(() => {
  sharedQueryClient.clear();
  mockedApiGet.mockReset();
  setSelectedProduct(PROD_1);
});

afterEach(() => {
  setSelectedProduct('');
  cleanup();
});

describe('PlansPage — the Custom Plans tab is scoped like the Standard Plans tab beside it', () => {
  it('lists the selected product’s custom plan and not the other product’s', async () => {
    mockApi([
      customPlanDto('cp-mine', 'mine-plan', ['repo-a', 'repo-a']),
      customPlanDto('cp-theirs', 'theirs-plan', ['repo-b']),
    ]);
    renderPage();

    await waitFor(() => expect(screen.queryByText('mine-plan')).not.toBeNull());
    // The load-bearing half: a filter that does nothing passes the line above
    // and fails this one.
    expect(screen.queryByText('theirs-plan')).toBeNull();
  });

  it('hides a plan whose tests span two products (hidden, extended to ambiguous)', async () => {
    mockApi([
      customPlanDto('cp-mine', 'mine-plan', ['repo-a']),
      customPlanDto('cp-spanning', 'spanning-plan', ['repo-a', 'repo-b']),
    ]);
    renderPage();

    await waitFor(() => expect(screen.queryByText('mine-plan')).not.toBeNull());
    expect(screen.queryByText('spanning-plan')).toBeNull();
  });

  it('hides a plan with no resolvable tests at all', async () => {
    mockApi([
      customPlanDto('cp-mine', 'mine-plan', ['repo-a']),
      customPlanDto('cp-empty', 'empty-plan', []),
      customPlanDto('cp-gone', 'deleted-repo-plan', ['repo-deleted']),
    ]);
    renderPage();

    await waitFor(() => expect(screen.queryByText('mine-plan')).not.toBeNull());
    expect(screen.queryByText('empty-plan')).toBeNull();
    expect(screen.queryByText('deleted-repo-plan')).toBeNull();
  });
});

// ADR-0010: a surface that scopes in the browser says so in the UI.
describe('PlansPage — product scope', () => {
  it('says the Custom Plans tab is filtered in the browser', async () => {
    mockApi([]);
    renderPage();
    await waitFor(() => expect(screen.queryByText(/filtered in your browser/)).not.toBeNull());
  });

  // The Standard tab's repositories are filtered in the browser too
  // (`reposForProduct`: `GET /qa/v1/test-repos` has no `product_id`), so it
  // carries its own notice rather than relying on the Custom tab's.
  it('says the Standard Plans tab is filtered in the browser', async () => {
    mockApi([]);
    renderPage('standard');
    await waitFor(() => expect(screen.queryByText(/filtered in your browser/)).not.toBeNull());
    expect(screen.queryByText(/Scoped to the selected product, filtered in your browser\. Plans are listed/)).not.toBeNull();
  });
});
