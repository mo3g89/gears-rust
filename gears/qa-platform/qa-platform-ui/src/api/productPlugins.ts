/**
 * The product-plugin catalogue: `GET /qa/v1/product-plugins`.
 *
 * One request per session, cached indefinitely. The set of registered plugins
 * is a property of the **binary**, not of the data — it changes when the
 * deployment is upgraded, not when a user does anything — so refetching it on
 * window focus would be noise. `staleTime: Infinity` says that rather than
 * leaving the default and hoping nobody notices.
 *
 * This is the single new endpoint §8 says the UI needs, and it serves both
 * halves: the environments table reads `observed_schema` (Task 21) and the
 * credential form reads `credential_schema` (Task 22).
 */
import { useQuery } from '@tanstack/react-query';

import { apiGet } from './client';
import type { ProductPlugin } from '@/lib/fieldDesc';

export const productPluginsQueryKey = ['productPlugins'] as const;

/**
 * Every product plugin this deployment registers.
 *
 * Returns `[]` rather than throwing when the catalogue cannot be read: a
 * missing plugin list must degrade the environments table to its fixed
 * columns, not blank the page. An operator sees "not observed" in every
 * descriptor column, which is the honest rendering of "this UI does not know
 * what this product's plugin declares".
 */
export function useProductPlugins() {
  return useQuery({
    queryKey: productPluginsQueryKey,
    queryFn: async (): Promise<ProductPlugin[]> => {
      // A bare JSON array, not an envelope: the operation is declared with
      // `json_array_response_with_schema`, and an empty list is a 200.
      const response = await apiGet<ProductPlugin[]>('/product-plugins');
      return response ?? [];
    },
    staleTime: Infinity,
  });
}
