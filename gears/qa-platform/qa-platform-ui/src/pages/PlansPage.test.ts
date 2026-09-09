// @vitest-environment jsdom
//
// I1 (final review): the Plans page's two tabs disagreed about what the global
// product switcher means. "Standard Plans" is product-scoped on the server
// (`usePlans` sends `product_id`); "Custom Plans" sat beside it, on the same
// page, inside the same `RequireProduct`, listing every custom plan in the
// deployment. Spec §4 states the invariant over *every* list surface — §1's
// inventory was built from `hooks.ts` and missed this one, because the scoping
// decision is made in the page rather than in a hook, and §7 does not record it
// as out of scope.
//
// A custom plan carries no product key of its own, so it is attributed through
// the repositories its tests name — `productScope.ts`'s resolver, the same one
// the Runs and Schedules lists use — and D4's hide applies unchanged.
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

function renderPage() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(
        ConfirmProvider,
        null,
        createElement(MemoryRouter, { initialEntries: ['/plans?tab=custom'] }, createElement(PlansPage))
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

  it('hides a plan whose tests span two products (spec D4, extended to ambiguous)', async () => {
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
