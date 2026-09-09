// @vitest-environment jsdom
import { act, renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { initialAnalyticsProductId, useAnalyticsProductSelection } from './analyticsProductSeed';

describe('initialAnalyticsProductId', () => {
  it('seeds from the active product when the URL names none', () => {
    expect(initialAnalyticsProductId(null, 'prod-1')).toBe('prod-1');
  });

  it('lets the URL override the active product, because it is a deep link', () => {
    expect(initialAnalyticsProductId('prod-2', 'prod-1')).toBe('prod-2');
  });

  it('falls back to empty when neither is known', () => {
    expect(initialAnalyticsProductId(null, undefined)).toBe('');
  });

  it('treats an empty URL parameter as absent rather than as a selection', () => {
    expect(initialAnalyticsProductId('', 'prod-1')).toBe('prod-1');
  });
});

// ---------------------------------------------------------------------------
// I4 (final review): spec D3 says Analytics *is wired to* the switcher, and
// that is the property the human partner chose. It was only ever seeded from
// it: `useState`'s initializer runs once, and `ProductSwitcher` mutates a
// module store without navigating or remounting, so switching product while on
// `/analytics` left the report on the previous one.
//
// The first test is the one that failed against the seed-only implementation.
// The other three are the constraints the fix must not break while satisfying
// it — and the deep-link one is the reason this cannot simply be "follow the
// switcher": a link that retargeted itself would show a different report to
// every recipient.
// ---------------------------------------------------------------------------
describe('useAnalyticsProductSelection', () => {
  it('follows the switcher when the product changes while the page is already open', () => {
    const { result, rerender } = renderHook(
      ({ active }: { active: string }) => useAnalyticsProductSelection(null, active),
      { initialProps: { active: 'prod-1' } }
    );
    expect(result.current[0]).toBe('prod-1');

    rerender({ active: 'prod-2' });

    expect(result.current[0]).toBe('prod-2');
  });

  it('lets a deep link keep winning for the whole visit, not just at mount', () => {
    const { result, rerender } = renderHook(
      ({ active }: { active: string }) => useAnalyticsProductSelection('prod-linked', active),
      { initialProps: { active: 'prod-1' } }
    );
    expect(result.current[0]).toBe('prod-linked');

    rerender({ active: 'prod-2' });

    expect(result.current[0]).toBe('prod-linked');
  });

  it('does not snap back a product picked in the page’s own selector', () => {
    const { result, rerender } = renderHook(
      ({ active }: { active: string }) => useAnalyticsProductSelection(null, active),
      { initialProps: { active: 'prod-1' } }
    );

    act(() => result.current[1]('prod-9'));
    expect(result.current[0]).toBe('prod-9');

    // A re-render with the switcher unchanged must leave the choice alone; only
    // an actual switch overrides it.
    rerender({ active: 'prod-1' });
    expect(result.current[0]).toBe('prod-9');

    rerender({ active: 'prod-3' });
    expect(result.current[0]).toBe('prod-3');
  });

  it('waits for the products list rather than clearing the selection', () => {
    const { result, rerender } = renderHook(
      ({ active }: { active: string | undefined }) => useAnalyticsProductSelection('prod-linked', active),
      { initialProps: { active: undefined as string | undefined } }
    );
    expect(result.current[0]).toBe('prod-linked');

    rerender({ active: undefined });
    expect(result.current[0]).toBe('prod-linked');
  });
});
