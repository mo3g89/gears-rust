// @vitest-environment jsdom
//
// Task 11: the environment detail page renders what qa-environments actually observed
// about an environment's cluster -- `observed_version`, `observed_build`, `vhp_base_url`,
// `observed_namespace`, `version_detect_error` and `version_detected_at` -- instead of
// the three "wrongs" it shipped with:
//
//   1. `UnavailableNotice` claiming nothing was ever observed (fixed for the fields that
//      now ARE observed). The cluster-health banner this note originally described --
//      node/worker/control-plane readiness, status, per-node table -- was accurate when
//      written (those fields had no source anywhere in the gears); Task 6 gave
//      qa-environments a real cluster-health reading and replaced that banner with
//      `ClusterHealthCard.tsx`. Nothing below asserts on cluster health -- these tests
//      are unaffected by that change, and are not where `ClusterHealthCard` is covered.
//   2. `platform.namespace || 'vhp-platform (default)'` against a DTO field
//      (`PlatformDto`) that never had a `namespace` at all, so the page always rendered a
//      fabricated default as if it were observed fact. It is now bound to
//      `observed_namespace`, and shows `-` (this page's convention for "nothing", used
//      throughout the same card for `build`/`vhp_base_url`) rather than a fiction when
//      `observed_namespace` is null.
//   3. "VHP Base URL" had no field behind it. It now reads `vhp_base_url`.
//
// This suite renders the actual page against a mocked API client -- exactly the pattern
// `RunDetailPage.test.ts` established for the same class of bug (a UI marker that
// compared against the wrong case, or read a field that was never wired up, and shipped
// as dead code because nothing rendered it in a test). `.test.ts`, not `.test.tsx`:
// `vitest.config.ts`'s `include` glob is `src/**/*.test.ts` only, so `createElement`
// stands in for JSX exactly as `hooks.test.ts` and `RunDetailPage.test.ts` already do.
import { createElement } from 'react';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
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

vi.mock('sonner', () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

import { apiGet, apiPost } from '@/api/client';
import { toast } from 'sonner';
import { ConfirmProvider } from '@/components/ui/confirm-dialog';
import { EnvironmentDetailPage } from './EnvironmentDetailPage';
import { RedirectToEnvironmentDetail } from '../App';

const mockedApiGet = vi.mocked(apiGet);
const mockedApiPost = vi.mocked(apiPost);
const toastSuccess = vi.mocked(toast.success);
const toastError = vi.mocked(toast.error);

const ENVIRONMENT_ID = '22222222-2222-2222-2222-222222222222';
const PRODUCT_ID = 'product-vhp';
const PLUGIN_ID = 'gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1';
const ENVIRONMENT_NAME = 'prod-cluster';

/** `EnvironmentDto` as the gear actually serves it (`dto.rs:36-69`) -- field names verified
 *  against `gears/qa-platform/qa-environments/qa-environments/src/api/rest/dto.rs` and
 *  the sample row `mapper.rs:139-145` builds for its own round-trip test. */
function environmentDto(overrides: Record<string, unknown> = {}) {
  return {
    id: ENVIRONMENT_ID,
    name: ENVIRONMENT_NAME,
    description: 'An environment',
    available: true,
    observed_version: '5.0.1',
    observed_build: '20260813',
    default_branch: null,
    product_id: PRODUCT_ID,
    // The plugin-shaped map, keyed by what VHP's `observed_schema()` declares.
    // `vhp_base_url`/`observed_namespace` were dropped by Task 19; the page
    // renders these through the product's descriptors instead.
    observed_attrs: { baseDomain: 'https://sv.jele.io', namespace: 'virtuozzo' },
    health_state: 'ok',
    health_detail: null,
    version_detect_error: null,
    version_detected_at: '2026-08-13T12:00:00Z',
    created_at: '2026-08-01T00:00:00Z',
    updated_at: '2026-08-13T12:00:00Z',
    ...overrides,
  };
}

function mockApiFor(dto: unknown) {
  mockedApiGet.mockImplementation(async (path: string) => {
    if (path === '/environments') {
      return [dto] as never;
    }
    if (path.startsWith(`/environments/${ENVIRONMENT_ID}`)) {
      return dto as never;
    }
    if (path === '/products') {
      return [
        {
          id: PRODUCT_ID,
          name: 'VHP',
          key: 'VHP',
          description: '',
          folder: null,
          plugin_instance_id: PLUGIN_ID,
          created_at: '2026-08-01T00:00:00Z',
          updated_at: '2026-08-01T00:00:00Z',
        },
      ] as never;
    }
    // The descriptor catalogue the detail rows are rendered from. VHP declares
    // `namespace` and `baseDomain` `in_detail`, which is why the page looks
    // the same as it did with those two rows hardcoded.
    if (path === '/product-plugins') {
      return [
        {
          instance_id: PLUGIN_ID,
          vendor: 'virtuozzo-vhp',
          credential_schema: [],
          observed_schema: [
            { key: 'namespace', label: 'Kubernetes Namespace', kind: 'text', required: false,
              role: 'namespace', in_table: true, in_detail: true, help: null },
            { key: 'baseDomain', label: 'VHP Base URL', kind: 'url', required: false,
              role: 'base_url', in_table: true, in_detail: true, help: null },
          ],
        },
      ] as never;
    }
    if (path.startsWith('/variables')) {
      return [] as never;
    }
    if (path.startsWith('/queue')) {
      return { items: [], page_info: { next_cursor: null } } as never;
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
          { initialEntries: [`/environments/${encodeURIComponent(ENVIRONMENT_NAME)}`] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: '/environments/:name', element: createElement(EnvironmentDetailPage) })
          )
        )
      )
    )
  );
}

/**
 * I-2: nothing pinned the `/platforms/:name` redirect or that it carries the `:name`
 * segment through. This mounts the SAME real page behind the SAME real redirect
 * component App.tsx registers (`RedirectToEnvironmentDetail`, imported from `../App`,
 * not reimplemented here), entering at the OLD bookmarked path. If the redirect ever
 * dropped or mis-encoded the name, the mocked API would answer for the wrong id (or none)
 * and the page would render its error state instead of the observed fields below.
 */
function renderPageViaOldPlatformsRoute() {
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
          { initialEntries: [`/platforms/${encodeURIComponent(ENVIRONMENT_NAME)}`] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: '/environments/:name', element: createElement(EnvironmentDetailPage) }),
            createElement(Route, { path: '/platforms/:name', element: createElement(RedirectToEnvironmentDetail) })
          )
        )
      )
    )
  );
}

beforeEach(() => {
  mockedApiGet.mockReset();
  mockedApiPost.mockReset();
  toastSuccess.mockReset();
  toastError.mockReset();
});

afterEach(cleanup);

describe('EnvironmentDetailPage — observed environment fields', () => {
  it('renders observed version, build, namespace and base URL', async () => {
    mockApiFor(environmentDto());
    renderPage();

    await waitFor(() => expect(screen.queryByText('5.0.1')).not.toBeNull());
    expect(screen.queryByText('20260813')).not.toBeNull();
    expect(screen.queryByText('virtuozzo')).not.toBeNull();
    expect(screen.queryByText('https://sv.jele.io')).not.toBeNull();
    // Wrong #2's fabricated default must never appear now that a real value is served.
    expect(screen.queryByText(/vhp-platform \(default\)/)).toBeNull();
  });

  /* The brief asked for this and the page does render it
   * (`EnvironmentDetailPage.tsx`'s "Last Detected" row), but nothing asserted it
   * until the 2026-08-28 final review's triage said so. The row is CONDITIONAL
   * on `version_detected_at || version_detect_error`, so "the timestamp
   * arrives and the row does not appear" is a real, silent failure mode; the
   * label assertion is what catches it, and the second half pins that the row
   * shows a formatted timestamp rather than the '-' its else-branch renders. */
  it('renders the Last Detected timestamp, not just the version it detected', async () => {
    mockApiFor(environmentDto());
    renderPage();

    await waitFor(() => expect(screen.queryByText('Last Detected')).not.toBeNull());
    const row = screen.getByText('Last Detected').closest('div');
    expect(row?.textContent).not.toBe('Last Detected-');
    expect(row?.textContent).toContain('2026');
  });

  it('renders a detection error message rather than swallowing it', async () => {
    mockApiFor(
      environmentDto({
        observed_version: null,
        observed_namespace: null,
        version_detect_error: 'namespaces "virtuozzo" not found',
      })
    );
    renderPage();

    await waitFor(() =>
      expect(screen.queryByText('namespaces "virtuozzo" not found')).not.toBeNull()
    );
  });

  /* An attribute the plugin DECLARES but no observation has produced. The row
   * still appears -- the plugin says this product has a namespace -- and the
   * cell reads as "not observed" rather than as `undefined` or a blank. The
   * declaration is the plugin's, the value is the observation's, and the two
   * are allowed to disagree. */
  it('shows no fiction for a declared attribute no observation produced', async () => {
    mockApiFor(environmentDto({ observed_attrs: { baseDomain: 'https://sv.jele.io' } }));
    renderPage();

    await waitFor(() => expect(screen.queryByText('5.0.1')).not.toBeNull());
    expect(screen.queryByText(/vhp-platform \(default\)/)).toBeNull();

    const label = screen.getByText('Kubernetes Namespace');
    const row = label.closest('div');
    expect(row?.textContent).toContain('Kubernetes Namespace—');
    expect(row?.textContent).not.toContain('undefined');
  });

  it('does not report success when the refresh response carries a detection error (the "lying toast" defect, 89b55eb22)', async () => {
    mockApiFor(environmentDto());
    mockedApiPost.mockResolvedValue(
      environmentDto({
        observed_version: null,
        version_detect_error: 'namespaces "virtuozzo" not found',
      }) as never
    );
    renderPage();

    await waitFor(() => expect(screen.queryByText('5.0.1')).not.toBeNull());

    const refreshButton = screen.getByRole('button', { name: 'Run detection again' });
    fireEvent.click(refreshButton);

    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(toastSuccess).not.toHaveBeenCalled();
    expect(toastError.mock.calls[0][0]).toBe('Detection failed');
  });
});

describe('EnvironmentDetailPage — reached via the retired /platforms/:name redirect (I-2)', () => {
  it('still renders this environment when entered at its old bookmarked URL', async () => {
    mockApiFor(environmentDto());
    renderPageViaOldPlatformsRoute();

    // Proves both halves at once: the redirect fired (nothing here mounts
    // `EnvironmentDetailPage` on `/platforms/:name` directly), and it preserved the
    // `:name` segment rather than dropping it — the mocked API only answers for
    // `ENVIRONMENT_ID`, keyed off `ENVIRONMENT_NAME`, so a lost or mangled name would
    // 404 and this would never appear.
    await waitFor(() => expect(screen.queryByText('5.0.1')).not.toBeNull());
  });
});
