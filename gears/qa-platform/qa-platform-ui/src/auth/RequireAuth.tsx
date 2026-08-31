/*
 * The route guard — and, as much as it is a guard, a throttle.
 *
 * WHY THIS IS THE STRUCTURAL HALF OF THE RATE-LIMIT FIX. Every data hook in
 * this app lives under `AppShell`, and several of them poll:
 * `src/api/hooks.ts:471` (dashboard, 15s), `:730` (5s), `:868` (schedules,
 * 15s), `:1525` (30s), `:2227` (15s), plus the Runs page's own interval.
 * React Query keeps a `refetchInterval` running through errors, so before this
 * guard existed an unauthenticated tab polled `/qa/v1` forever. Measured on
 * this stack, unauthenticated, before the guard: `/runs` issued **20 requests
 * in 25 seconds** on an unseeded deployment and **34 in 25 seconds** once
 * `smoke.sh` had seeded it — every one a 401, with no end condition. That is
 * what wedged the gateway's rate limiter for twelve hours.
 *
 * The fix is not a smarter retry. It is that an unauthenticated app must not
 * mount the queries at all: `children` — and therefore `AppShell` and every
 * hook under it — is simply not rendered until there is a token. The
 * regression assertion lives in `deploy/compose/ui-gate.js` (phase 0), which
 * counts the `/qa/v1` requests an unauthenticated browser makes and fails if
 * there are any.
 *
 * The second half of the rule is in `./tokenState`, for a token that goes bad
 * while the app is already running.
 */
import { useEffect, useRef } from 'react';
import type { ReactNode } from 'react';
import { Loader2 } from 'lucide-react';
import { useAuth } from './useAuth';
import { LoginPage } from './LoginPage';

export function RequireAuth({ children }: { children: ReactNode }) {
  const { isAuthenticated, isLoading, error, login } = useAuth();
  /** So a re-render between calling `login()` and the browser actually leaving
   *  the page cannot start a second authorization request. */
  const redirectStarted = useRef(false);

  useEffect(() => {
    if (isLoading || isAuthenticated || error) return;
    if (redirectStarted.current) return;
    redirectStarted.current = true;
    login();
  }, [isAuthenticated, isLoading, error, login]);

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-screen">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  // A session that cannot be used, and that re-authenticating automatically
  // would loop on. Show the sign-in screen in place — the URL is left alone, so
  // a successful sign-in returns to the page that was asked for.
  if (error) {
    return <LoginPage />;
  }

  if (!isAuthenticated) {
    return (
      <div className="flex items-center justify-center h-screen">
        <p className="text-sm text-muted-foreground">Redirecting to sign in…</p>
      </div>
    );
  }

  return <>{children}</>;
}
