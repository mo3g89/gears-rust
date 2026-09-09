// @vitest-environment jsdom
//
// I-2 (Task 26 review): nothing pinned the `/platforms` -> `/environments` collection
// redirect, or that `/platforms/:name` preserves its path parameter through
// `RedirectToEnvironmentDetail` on the way to `/environments/:name`. A future cleanup
// that deleted either `<Route>` as "dead" would have been green in every other gate —
// the failure is a 404 on a bookmarked or shared link, not a compile or lint error.
//
// The parameter-preserving half (does the real page render for the right environment
// once the redirect lands) is covered in `EnvironmentDetailPage.test.ts`, which already
// has the API-mocking harness that assertion needs. This file covers the plain
// collection redirect, and re-asserts the parameter-preserving behaviour in isolation —
// Both redirects are IMPORTED from `./App`, never reimplemented here. The first
// version of this file wrote the collection redirect inline as its own
// `<Navigate>`, so it asserted against a copy: changing the real route's target
// in `App.tsx` survived all 231 tests (re-review, N-1). A test that reimplements
// what it is checking cannot fail when the real thing changes.
//
// `.test.ts`, not `.test.tsx`: `vitest.config.ts`'s `include` glob is `src/**/*.test.ts`
// only, so `createElement` stands in for JSX, as every other render test here does.
import { createElement } from 'react';
import type { ReactNode } from 'react';
import { MemoryRouter, Outlet, Route, Routes, useParams } from 'react-router-dom';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// The `/runs` and `/schedules` gate test below mounts the REAL `App` — its
// router, its route table and the real pages — so that the assertion is about
// `App.tsx`'s wiring and not about a copy of it written here (this file's header
// says why that distinction is the whole point). Only the two things that cannot
// run in jsdom are replaced: the OIDC session, and the shell chrome, which is a
// layout route whose only job in this test is to render its `Outlet`.
vi.mock('./auth', () => ({
  AuthProvider: ({ children }: { children: ReactNode }) => children,
  RequireAuth: ({ children }: { children: ReactNode }) => children,
  AuthCallbackPage: () => null,
  LoginPage: () => null,
}));
vi.mock('./components/layout/AppShell', () => ({
  AppShell: () => createElement(Outlet),
}));
vi.mock('./api/client', () => ({
  apiGet: vi.fn(),
  apiGetBlob: vi.fn(),
  apiPost: vi.fn(),
  apiDelete: vi.fn(),
  apiPut: vi.fn(),
  apiPatch: vi.fn(),
  setTokenProvider: vi.fn(),
  ApiError: class ApiError extends Error {
    status = 0;
  },
}));

import { apiGet } from './api/client';
import { queryClient } from './api/queryClient';
import { setSelectedProduct } from './lib/selectedProduct';
import App, { RedirectToEnvironmentDetail, RedirectToEnvironments } from './App';

const mockedApiGet = vi.mocked(apiGet);

// jsdom has no `matchMedia`, and `ThemeProvider` (which the real `App` mounts)
// reads it to resolve the system theme. Stubbed rather than mocking the whole
// provider away, so the tree under test stays the real one.
beforeEach(() => {
  if (!window.matchMedia) {
    Object.defineProperty(window, 'matchMedia', {
      writable: true,
      value: (query: string) => ({
        matches: false,
        media: query,
        onchange: null,
        addEventListener: () => {},
        removeEventListener: () => {},
        addListener: () => {},
        removeListener: () => {},
        dispatchEvent: () => false,
      }),
    });
  }
  queryClient.clear();
  mockedApiGet.mockReset();
  setSelectedProduct('');
});

afterEach(() => {
  cleanup();
  setSelectedProduct('');
});

describe('the retired /platforms routes redirect rather than 404 (I-2)', () => {
  it('/platforms redirects to /environments', () => {
    render(
      createElement(
        MemoryRouter,
        { initialEntries: ['/platforms'] },
        createElement(
          Routes,
          null,
          createElement(Route, { path: '/environments', element: createElement('div', null, 'Environments page') }),
          createElement(Route, { path: '/platforms', element: createElement(RedirectToEnvironments) })
        )
      )
    );
    expect(screen.queryByText('Environments page')).not.toBeNull();
  });

  it('/platforms/:name redirects to /environments/:name, preserving the name', () => {
    function EnvironmentNameProbe() {
      const { name } = useParams<{ name: string }>();
      return createElement('div', null, `landed on ${name}`);
    }

    render(
      createElement(
        MemoryRouter,
        { initialEntries: ['/platforms/sv-test'] },
        createElement(
          Routes,
          null,
          createElement(Route, { path: '/environments/:name', element: createElement(EnvironmentNameProbe) }),
          createElement(Route, { path: '/platforms/:name', element: createElement(RedirectToEnvironmentDetail) })
        )
      )
    );
    // Not just "some redirect happened" — the exact name must survive it. A redirect
    // that dropped `:name` (e.g. `<Navigate to="/environments" replace />` reused by
    // mistake) would land on no route here at all, and a redirect that mis-encoded it
    // would land on the wrong probe text.
    expect(screen.queryByText('landed on sv-test')).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// C2 — `/runs` and `/schedules` are behind `RequireProduct`, like every other
// product-scoped surface.
//
// Both hooks gained `enabled: !!product` when the lists became product-scoped,
// and a disabled React Query v5 query reports `isLoading: false` with
// `data: undefined`. Without the gate each page therefore walked straight past
// its `if (isLoading)` branch into its empty state: a false "No test runs
// found" on **every** visit before `/products` resolved, and a permanent one on
// a deployment that has no products at all — where every other page shows the
// "Нет продуктов" panel instead.
//
// The route table is exercised through the real `App`, so removing either
// wrapper fails here. Asserting the negative ("the empty state is not shown")
// is the load-bearing half: a page that rendered nothing at all would pass a
// "the gate is visible" check on the no-products case and still be wrong on the
// still-loading one.
// ---------------------------------------------------------------------------
describe('C2 — /runs and /schedules are gated on a resolved product', () => {
  /** Answer everything except `/products`, which never settles: the state every
   *  visit passes through before the switcher has anything to select. */
  function mockProductsStillLoading() {
    mockedApiGet.mockImplementation((path: string) => {
      if (path.startsWith('/products')) return new Promise(() => {});
      if (path.startsWith('/environments')) {
        return Promise.resolve({ items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } });
      }
      if (path.startsWith('/runs') || path.startsWith('/queue')) {
        return Promise.resolve({ items: [], page_info: { next_cursor: null } });
      }
      return Promise.resolve([]);
    });
  }

  /** A deployment with no products at all — the permanent case. */
  function mockNoProducts() {
    mockedApiGet.mockImplementation((path: string) => {
      if (path.startsWith('/environments')) {
        return Promise.resolve({ items: [], page_info: { limit: 200, next_cursor: null, prev_cursor: null } });
      }
      if (path.startsWith('/runs') || path.startsWith('/queue')) {
        return Promise.resolve({ items: [], page_info: { next_cursor: null } });
      }
      return Promise.resolve([]);
    });
  }

  function renderAppAt(path: string) {
    window.history.pushState({}, '', path);
    render(createElement(App));
  }

  it('/runs shows no "No test runs found" while the products list is still loading', async () => {
    mockProductsStillLoading();
    renderAppAt('/runs');

    // Give the runs query every chance to resolve and render its empty table.
    await waitFor(() => expect(mockedApiGet).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(screen.queryByText('No test runs found')).toBeNull();
  });

  it('/runs shows the no-products panel, not an empty run list, on a deployment with no products', async () => {
    mockNoProducts();
    renderAppAt('/runs');

    await waitFor(() => expect(screen.queryByText('Нет продуктов')).not.toBeNull());
    expect(screen.queryByText('No test runs found')).toBeNull();
  });

  it('/schedules shows no "No schedules configured" while the products list is still loading', async () => {
    mockProductsStillLoading();
    renderAppAt('/schedules');

    await waitFor(() => expect(mockedApiGet).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(screen.queryByText('No schedules configured')).toBeNull();
  });

  it('/schedules shows the no-products panel on a deployment with no products', async () => {
    mockNoProducts();
    renderAppAt('/schedules');

    await waitFor(() => expect(screen.queryByText('Нет продуктов')).not.toBeNull());
    expect(screen.queryByText('No schedules configured')).toBeNull();
  });
});
