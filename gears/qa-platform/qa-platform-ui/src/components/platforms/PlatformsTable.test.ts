// @vitest-environment jsdom
//
// `PlatformsTable` had no render test at all — the final review of the cluster-health
// work flagged exactly that, alongside `PlatformsStrip`. This suite opens the gap with
// the surface that most needs it: the **Default** badge.
//
// The badge is the only place in the product that answers "which platform does the Run
// and Schedule dialogs' 'Default cluster' option actually run against?". It is a bare
// `{platform.is_default && …}`, which is the shape that ships silently broken: bind it to
// the wrong field, or lose `is_default` anywhere along
// `PlatformDto → platformFromDto → PlatformInfo`, and the badge simply never renders. No
// error, no blank space, nothing to notice — the operator just cannot see which platform
// is the default, which is the report that prompted the badge in the first place.
//
// `.test.ts`, not `.test.tsx`: `vitest.config.ts`'s `include` glob is `src/**/*.test.ts`
// only, so `createElement` stands in for JSX, exactly as `PlatformDetailPage.test.ts` and
// `RunDetailPage.test.ts` already do.
import { createElement } from 'react';
import { MemoryRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/api/client', () => ({
  apiGet: vi.fn().mockResolvedValue([]),
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

import { PlatformInfo } from '@/api/types';
import { PlatformsTable } from './PlatformsTable';

function platform(overrides: Partial<PlatformInfo> = {}): PlatformInfo {
  return {
    id: 'p1',
    name: 'sv-test',
    created_at: '2026-08-29T00:00:00Z',
    description: '',
    product_id: 'product-vhp',
    is_default: false,
    version: '26.5',
    build: '0',
    namespace: 'virtuozzo',
    vhp_base_url: 'https://sv.jele.io',
    version_detected_at: '2026-08-31T00:00:00Z',
    version_detect_error: null,
    default_branch: null,
    cluster: null,
    ...overrides,
  } as PlatformInfo;
}

function renderTable(platforms: PlatformInfo[]) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(MemoryRouter, null, createElement(PlatformsTable, { platforms }))
    )
  );
}

afterEach(() => cleanup());

describe('PlatformsTable — the Default badge', () => {
  it('marks the platform its product defaults to', async () => {
    renderTable([platform({ is_default: true })]);

    expect(await screen.findByText('sv-test')).toBeTruthy();
    expect(screen.queryByText('Default')).toBeTruthy();
  });

  it('shows no badge when the platform is not the default', async () => {
    // The negative case is the one that catches a badge hardcoded to render, which a
    // positive-only test would pass with `is_default` never read at all.
    renderTable([platform({ is_default: false })]);

    expect(await screen.findByText('sv-test')).toBeTruthy();
    expect(screen.queryByText('Default')).toBeNull();
  });

  it('marks only the default row when a product has several platforms', async () => {
    renderTable([
      platform({ id: 'p1', name: 'sv-test', is_default: false }),
      platform({ id: 'p2', name: 'sv-stage', is_default: true }),
    ]);

    expect(await screen.findByText('sv-stage')).toBeTruthy();
    // Exactly one badge, not one per row: this is the case the flag exists for, and a
    // badge rendered on every row would be worse than none — it would name every
    // platform the default.
    expect(screen.queryAllByText('Default')).toHaveLength(1);
  });
});
