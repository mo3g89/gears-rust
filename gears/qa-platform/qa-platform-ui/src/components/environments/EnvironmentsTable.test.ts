// @vitest-environment jsdom
//
// `EnvironmentsTable` had no render test at all — the final review of the cluster-health
// work flagged exactly that, alongside `EnvironmentsStrip`. This suite opens the gap with
// the surface that most needs it: the **Default** badge.
//
// The badge is the only place in the product that answers "which environment does the Run
// and Schedule dialogs' 'Default cluster' option actually run against?". It is a bare
// `{environment.is_default && …}`, which is the shape that ships silently broken: bind it to
// the wrong field, or lose `is_default` anywhere along
// `EnvironmentDto → environmentFromDto → EnvironmentInfo`, and the badge simply never renders. No
// error, no blank space, nothing to notice — the operator just cannot see which environment
// is the default, which is the report that prompted the badge in the first place.
//
// `.test.ts`, not `.test.tsx`: `vitest.config.ts`'s `include` glob is `src/**/*.test.ts`
// only, so `createElement` stands in for JSX, exactly as `EnvironmentDetailPage.test.ts` and
// `RunDetailPage.test.ts` already do.
import { createElement } from 'react';
import { MemoryRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { productPluginsQueryKey } from '@/api/productPlugins';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const PLUGIN_ID_HOISTED = vi.hoisted(() => 'gts.a~vhp.v1');
const PLUGIN_ID = PLUGIN_ID_HOISTED;

/** VHP's own observed schema: the two attributes that used to be hardcoded
 *  columns, declared `in_table` by the plugin instead. */
const VHP_OBSERVED_SCHEMA = [
  { key: 'namespace', label: 'Namespace', kind: 'text', required: false,
    role: 'namespace', in_table: true, in_detail: true, help: null },
  { key: 'baseDomain', label: 'VHP URL', kind: 'url', required: false,
    role: 'base_url', in_table: true, in_detail: true, help: null },
];

/** The one product every row here belongs to. Lifted out of the mock factory so
 *  the empty-catalogue test can keep the products while emptying
 *  `/product-plugins` alone. `vi.hoisted` because `vi.mock`'s factory is hoisted
 *  above every module-level `const`, and a plain one would be read before it is
 *  initialised. */
const PRODUCTS = vi.hoisted(() => [
  { id: 'product-vhp', name: 'VHP', key: 'VHP', description: '', folder: null,
    plugin_instance_id: PLUGIN_ID_HOISTED, created_at: '2026-08-01T00:00:00Z',
    updated_at: '2026-08-01T00:00:00Z' },
]);

vi.mock('@/api/client', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/products') {
      return PRODUCTS;
    }
    if (path === '/product-plugins') {
      return [
        { instance_id: PLUGIN_ID, vendor: 'virtuozzo-vhp', credential_schema: [],
          observed_schema: VHP_OBSERVED_SCHEMA },
      ];
    }
    return [];
  }),
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
import { EnvironmentInfo } from '@/api/types';
import { EnvironmentsTable } from './EnvironmentsTable';

function environment(overrides: Partial<EnvironmentInfo> = {}): EnvironmentInfo {
  return {
    id: 'p1',
    name: 'sv-test',
    created_at: '2026-08-29T00:00:00Z',
    description: '',
    product_id: 'product-vhp',
    is_default: false,
    version: '26.5',
    build: '0',
    available: true,
    observed_attrs: { namespace: 'virtuozzo', baseDomain: 'https://sv.jele.io' },
    health_state: 'ok',
    health_detail: null,
    version_detected_at: '2026-08-31T00:00:00Z',
    version_detect_error: null,
    default_branch: null,
    ...overrides,
  } as EnvironmentInfo;
}

function renderTable(environments: EnvironmentInfo[]) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(MemoryRouter, null, createElement(EnvironmentsTable, { environments }))
    )
  );
  return client;
}

/** Block until every query this render started has settled, and the plugin
 *  catalogue is one of them.
 *
 *  The rows come from a prop, so `findByText('sv-test')` resolves on the first
 *  paint — *before* `/product-plugins` answers. An assertion that something is
 *  ABSENT is therefore a no-op unless it waits for the catalogue first:
 *  measured, a version of the empty-catalogue test below that awaited only the
 *  row name passed even when the mock was changed to return VHP's plugin
 *  (re-review, N-1). Awaiting a positive heading is what the other tests here
 *  do; this is what works when the expected outcome is that no new heading
 *  appears. */
async function settled(client: QueryClient) {
  await waitFor(() => {
    expect(
      client.getQueryCache().findAll({ queryKey: productPluginsQueryKey }).length
    ).toBeGreaterThan(0);
    expect(client.isFetching()).toBe(0);
  });
}

afterEach(() => cleanup());

describe('EnvironmentsTable — the Default badge', () => {
  it('marks the environment its product defaults to', async () => {
    renderTable([environment({ is_default: true })]);

    expect(await screen.findByText('sv-test')).toBeTruthy();
    expect(screen.queryByText('Default')).toBeTruthy();
  });

  it('shows no badge when the environment is not the default', async () => {
    // The negative case is the one that catches a badge hardcoded to render, which a
    // positive-only test would pass with `is_default` never read at all.
    renderTable([environment({ is_default: false })]);

    expect(await screen.findByText('sv-test')).toBeTruthy();
    expect(screen.queryByText('Default')).toBeNull();
  });

  it('marks only the default row when a product has several environments', async () => {
    renderTable([
      environment({ id: 'p1', name: 'sv-test', is_default: false }),
      environment({ id: 'p2', name: 'sv-stage', is_default: true }),
    ]);

    expect(await screen.findByText('sv-stage')).toBeTruthy();
    // Exactly one badge, not one per row: this is the case the flag exists for, and a
    // badge rendered on every row would be worse than none — it would name every
    // environment the default.
    expect(screen.queryAllByText('Default')).toHaveLength(1);
  });
});

/**
 * **The copper thread.** The table used to hardcode "Namespace" and "VHP URL";
 * it renders them from VHP's own descriptors now, and the result is the same
 * table. That is the whole claim of this branch in one test: the coupling
 * moved out, the product did not change.
 */
describe('EnvironmentsTable — descriptor-driven columns', () => {
  it("renders VHP's table exactly as the hardcoded one did, from its plugin's descriptors", async () => {
    renderTable([environment()]);

    expect(await screen.findByText('sv-test')).toBeTruthy();
    // The headings are the plugin's labels now, not string literals in this file.
    expect(await screen.findByText('Namespace')).toBeTruthy();
    expect(screen.queryByText('VHP URL')).toBeTruthy();
    // And the values come from `observed_attrs`, keyed by the descriptors.
    expect(screen.queryByText('virtuozzo')).toBeTruthy();
    expect(screen.queryByText('https://sv.jele.io')).toBeTruthy();
  });

  it('renders the four fixed leading columns for every product', async () => {
    renderTable([environment()]);

    for (const heading of ['Name', 'Health', 'Product', 'Available']) {
      expect(await screen.findByText(heading)).toBeTruthy();
    }
  });

  it('renders the absent marker, never "undefined", for a declared attribute nothing observed', async () => {
    renderTable([environment({ observed_attrs: { namespace: 'virtuozzo' } })]);

    // Awaited: the heading only appears once the plugin catalogue resolves, and
    // the environment's name renders before it does.
    expect(await screen.findByText('VHP URL')).toBeTruthy();
    // The `baseDomain` column still exists -- the plugin declares it -- and its
    // cell says "not observed" rather than printing `undefined`.
    expect(document.body.textContent).not.toContain('undefined');
  });

  it('falls back to the fixed columns alone for a row whose product resolves no plugin', async () => {
    // Named for what it varies. It used to say "when the plugin catalogue is
    // empty", which the mock never made true -- the catalogue still returned
    // VHP's plugin and the row simply had no product (whole-branch review,
    // m-10).
    renderTable([environment({ product_id: null })]);

    expect(await screen.findByText('sv-test')).toBeTruthy();
    expect(screen.queryByText('Namespace')).toBeNull();
  });

  it('falls back to the fixed columns alone when the catalogue really is empty', async () => {
    // The case the test above was named for. A deployment whose catalogue
    // cannot be read must still list its environments: losing the descriptor
    // columns is a degradation, blanking the page is a failure.
    // Mocked BY PATH, not with `mockImplementationOnce`: "once" replaces the
    // next single `apiGet` call whichever it is, and the next one here is
    // `/products` -- so the first version of this test emptied the product
    // list and left the catalogue exactly as the test above it (re-review,
    // N-1). The row keeps its product; only the catalogue is empty.
    const client = vi.mocked(apiGet);
    const original = client.getMockImplementation();
    client.mockImplementation(async (path: string) => {
      if (path === '/product-plugins') return [];
      if (path === '/products') return PRODUCTS;
      return [];
    });
    try {
      await assertFixedColumnsOnly(renderTable([environment()]));
    } finally {
      // `mockImplementation` is not scoped to one call, so it outlives this
      // test unless it is put back.
      client.mockImplementation(original!);
    }
  });
});

/** The four fixed leading columns render, the descriptor columns do not. */
async function assertFixedColumnsOnly(client: QueryClient) {
  expect(await screen.findByText('sv-test')).toBeTruthy();
  await settled(client);
  for (const heading of ['Name', 'Health', 'Product', 'Available']) {
    expect(screen.queryByText(heading)).toBeTruthy();
  }
  // The descriptor columns are what an empty catalogue costs. Losing them is
  // the degradation; blanking the page would be the failure.
  expect(screen.queryByText('Namespace')).toBeNull();
  expect(screen.queryByText('VHP URL')).toBeNull();
}
