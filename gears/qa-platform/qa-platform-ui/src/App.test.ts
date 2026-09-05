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
import { MemoryRouter, Route, Routes, useParams } from 'react-router-dom';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { RedirectToEnvironmentDetail, RedirectToEnvironments } from './App';

afterEach(cleanup);

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
