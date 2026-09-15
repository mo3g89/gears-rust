import { QueryClient } from '@tanstack/react-query';
import { ApiError } from './client';

/**
 * The app's single React Query client.
 *
 * **It lives here rather than in `App.tsx` because `hooks.ts` needs to reach it
 * without a React context.** Most of this file's callers are hooks, which would
 * get the client from `QueryClientProvider`; `fetchEnvironmentDtos` is not a
 * hook — it is a plain async function called from inside other queries'
 * `queryFn`s and from mutations — and `queryClient.fetchQuery` is what gives it
 * a cache and a `staleTime`. Importing `App.tsx` from `hooks.ts` to get the
 * client would be a cycle, so the client moved down instead.
 *
 * `App.tsx` still owns the `QueryClientProvider`; it imports this.
 */
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: false,
      /**
       * One retry for a transient failure, and **none for a 401**.
       *
       * A 401 is a statement about the session, not about this request, so
       * retrying it cannot succeed — it only doubles the request volume of a
       * broken session. Measured on this stack before this rule and
       * `RequireAuth` existed, by counting an unauthenticated headless browser's
       * requests on `/runs`: **20 `/qa/v1` requests in 25 seconds** on an
       * unseeded deployment and **34 in 25 seconds** once the stack had been seeded
       * it — every one a 401, with no end condition, because `refetchInterval`
       * keeps polling through errors and `retry: 1` doubled each cycle. The
       * gateway's rate limiter answered 429 with a twelve-hour `retry_after` to
       * *every* client on the host as a result.
       *
       * `RequireAuth` is the structural half of the fix (an unauthenticated app
       * mounts no queries at all) and `@/auth/tokenState` handles a token that
       * dies mid-session; this is the cheap third guard for anything that gets
       * past both.
       */
      retry: (failureCount, error) => {
        if (error instanceof ApiError && error.status === 401) return false;
        return failureCount < 1;
      },
      staleTime: 30000, // 30 seconds
    },
  },
});
