import { useEffect, useRef, useState } from 'react';

/**
 * Which product the Analytics page starts on.
 *
 * The URL wins. `?product_id=` is a deep link to one report, and a link that
 * silently retargeted itself to whatever the switcher happened to hold would
 * be worse than one that ignores the switcher entirely.
 */
export function initialAnalyticsProductId(
  urlProductId: string | null,
  activeProductId: string | null | undefined
): string {
  return urlProductId || activeProductId || '';
}

/**
 * The Analytics page's product selection: seeded from the URL or the switcher,
 * and — spec D3 — **kept in sync with the switcher** while the URL names none.
 *
 * Seeding alone was not the property D3 states, and it is not the one the human
 * partner chose. `useState`'s initializer runs once, and `ProductSwitcher`
 * mutates a module store without navigating or remounting, so switching product
 * while already on `/analytics` left the report on the previous product — the
 * one page in the app where the switcher visibly did nothing.
 *
 * Three details, each load-bearing:
 *
 * * **The deep link is captured at mount, not read live.** `AnalyticsDashboard`
 *   writes its own selection back into the query string on every change, so
 *   after the first render `?product_id=` is there whether or not anyone linked
 *   it. Only the value the page was *entered* with distinguishes a deep link
 *   from the page's own echo, so that is what is remembered. A deep link keeps
 *   winning for the whole visit: a link that retargeted itself would show a
 *   different report to every recipient.
 * * **The user's own choice in the page's dropdown is not overwritten.** The
 *   sync fires on a *change* of the active product, tracked in a ref, rather
 *   than on "the two disagree" — otherwise picking another product in the
 *   Analytics selector would snap straight back to the switcher's.
 * * **An absent active product is not a selection.** Until `useProducts`
 *   resolves there is nothing to sync to, and clearing the selection then would
 *   drop the seed.
 */
export function useAnalyticsProductSelection(
  urlProductId: string | null,
  activeProductId: string | null | undefined
): [string, (productId: string) => void] {
  const deepLinkedProductId = useRef(urlProductId || '').current;
  const [selectedProductId, setSelectedProductId] = useState(() =>
    initialAnalyticsProductId(urlProductId, activeProductId)
  );
  // Seeded with what the initial selection already reflects, so a mount does
  // not count as a switch. `null` under a deep link: the effect never runs
  // there, and leaving it null keeps that unambiguous.
  const lastSyncedProductId = useRef<string | null>(
    deepLinkedProductId ? null : (activeProductId ?? null)
  );

  useEffect(() => {
    if (deepLinkedProductId) return;
    if (!activeProductId) return;
    if (lastSyncedProductId.current === activeProductId) return;
    lastSyncedProductId.current = activeProductId;
    setSelectedProductId(activeProductId);
  }, [activeProductId, deepLinkedProductId]);

  return [selectedProductId, setSelectedProductId];
}
