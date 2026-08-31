// @vitest-environment jsdom
//
// Task 11: the platform detail page renders what qa-environments actually observed
// about a platform's cluster -- `observed_version`, `observed_build`, `vhp_base_url`,
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
import { PlatformDetailPage } from './PlatformDetailPage';

const mockedApiGet = vi.mocked(apiGet);
const mockedApiPost = vi.mocked(apiPost);
const toastSuccess = vi.mocked(toast.success);
const toastError = vi.mocked(toast.error);

const PLATFORM_ID = '22222222-2222-2222-2222-222222222222';
const PLATFORM_NAME = 'prod-cluster';

/** `PlatformDto` as the gear actually serves it (`dto.rs:36-69`) -- field names verified
 *  against `gears/qa-platform/qa-environments/qa-environments/src/api/rest/dto.rs` and
 *  the sample row `mapper.rs:139-145` builds for its own round-trip test. */
function platformDto(overrides: Record<string, unknown> = {}) {
  return {
    id: PLATFORM_ID,
    name: PLATFORM_NAME,
    product_id: null,
    description: 'A platform',
    available: true,
    observed_version: '5.0.1',
    observed_build: '20260813',
    default_branch: null,
    vhp_base_url: 'https://sv.jele.io',
    observed_namespace: 'virtuozzo',
    version_detect_error: null,
    version_detected_at: '2026-08-13T12:00:00Z',
    created_at: '2026-08-01T00:00:00Z',
    updated_at: '2026-08-13T12:00:00Z',
    ...overrides,
  };
}

function mockApiFor(dto: unknown) {
  mockedApiGet.mockImplementation(async (path: string) => {
    if (path === '/platforms') {
      return [dto] as never;
    }
    if (path.startsWith(`/platforms/${PLATFORM_ID}`)) {
      return dto as never;
    }
    if (path === '/products') {
      return [] as never;
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
          { initialEntries: [`/platforms/${encodeURIComponent(PLATFORM_NAME)}`] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: '/platforms/:name', element: createElement(PlatformDetailPage) })
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

describe('PlatformDetailPage — observed platform fields', () => {
  it('renders observed version, build, namespace and base URL', async () => {
    mockApiFor(platformDto());
    renderPage();

    await waitFor(() => expect(screen.queryByText('5.0.1')).not.toBeNull());
    expect(screen.queryByText('20260813')).not.toBeNull();
    expect(screen.queryByText('virtuozzo')).not.toBeNull();
    expect(screen.queryByText('https://sv.jele.io')).not.toBeNull();
    // Wrong #2's fabricated default must never appear now that a real value is served.
    expect(screen.queryByText(/vhp-platform \(default\)/)).toBeNull();
  });

  /* The brief asked for this and the page does render it
   * (`PlatformDetailPage.tsx`'s "Last Detected" row), but nothing asserted it
   * until the 2026-08-28 final review's triage said so. The row is CONDITIONAL
   * on `version_detected_at || version_detect_error`, so "the timestamp
   * arrives and the row does not appear" is a real, silent failure mode; the
   * label assertion is what catches it, and the second half pins that the row
   * shows a formatted timestamp rather than the '-' its else-branch renders. */
  it('renders the Last Detected timestamp, not just the version it detected', async () => {
    mockApiFor(platformDto());
    renderPage();

    await waitFor(() => expect(screen.queryByText('Last Detected')).not.toBeNull());
    const row = screen.getByText('Last Detected').closest('div');
    expect(row?.textContent).not.toBe('Last Detected-');
    expect(row?.textContent).toContain('2026');
  });

  it('renders a detection error message rather than swallowing it', async () => {
    mockApiFor(
      platformDto({
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

  it('shows no fiction for a null observed_namespace', async () => {
    mockApiFor(platformDto({ observed_namespace: null }));
    renderPage();

    await waitFor(() => expect(screen.queryByText('5.0.1')).not.toBeNull());
    expect(screen.queryByText(/vhp-platform \(default\)/)).toBeNull();

    const label = screen.getByText('Kubernetes Namespace');
    const row = label.closest('div');
    expect(row?.textContent).toContain('Kubernetes Namespace-');
  });

  it('does not report success when the refresh response carries a detection error (the "lying toast" defect, 89b55eb22)', async () => {
    mockApiFor(platformDto());
    mockedApiPost.mockResolvedValue(
      platformDto({
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
