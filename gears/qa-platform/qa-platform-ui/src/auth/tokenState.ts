/*
 * The bearer token the API client sends, and the rule for what happens when the
 * gears answer 401.
 *
 * WHY THIS IS ITS OWN MODULE, separate from `provider.tsx`. It imports nothing
 * — not React, not `oidc-client-ts`, not `window`. That is what lets
 * `src/api/client.ts` depend on it without dragging the auth stack into every
 * module that makes a request, and what lets `tokenState.test.ts` run in
 * vitest's `node` environment (see `vitest.config.ts`).
 *
 * WHERE THE ACCESS TOKEN LIVES. In `currentAccessToken` below — a module
 * variable, i.e. memory, cleared by a reload. `src/api/client.ts` reads it
 * through the `setTokenProvider` seam and never touches storage itself. The
 * *session* is persisted by `oidc-client-ts`'s own store, which is
 * `window.sessionStorage` by default (verified in
 * node_modules/oidc-client-ts/dist/esm/oidc-client-ts.js:2517) — per-tab and
 * gone when the tab closes. Be precise about what that means rather than
 * over-claiming: the serialized `User` that library writes to sessionStorage
 * does include the access token, because that is how a reload restores a
 * session without a round trip. What this app deliberately never does is put a
 * token in `localStorage`, which is shared across tabs and survives the browser
 * closing. (The library's *state* store — the transient PKCE `code_verifier`
 * during a redirect — does default to localStorage, at :1073. It is single-use,
 * consumed by the callback, and is not a token.)
 *
 * THE 401 RULE, which is the part with logic worth testing:
 * **exactly one silent refresh for a run of 401s, then redirect.** A 401 is not
 * a transient error, so "retry until it works" is never the right answer — and
 * on this deployment it is actively harmful: an unauthenticated tab left on
 * `/runs` was measured issuing 20 `/qa/v1` requests in 25 seconds (34 once
 * the stack had been seeded), all 401, with no end condition — which is
 * what wedged the gateway's rate limiter for twelve hours (see the Task 15
 * report). The structural half of the fix is
 * `RequireAuth`, which stops the queries mounting at all; this counter is the
 * half that covers a token going bad *while* the app is running.
 */

/** What `handleUnauthorized` is allowed to do, injected so the rule can be
 *  tested without a browser. `refresh` is `oidc-client-ts`'s `signinSilent`;
 *  `redirect` is what sends the user back to the sign-in screen. */
export interface UnauthorizedHandlers {
  /** Attempt one silent renewal. Rejecting (or resolving to no token) means
   *  the session is gone and the user must sign in again. */
  refresh: () => Promise<unknown>;
  /** Give up on this session and put the user in front of the sign-in screen. */
  redirect: () => void;
}

/**
 * True once a refresh has been attempted for the current run of failures.
 *
 * Cleared by `resetAuthAttempts`, and — this is the whole of the rule — the
 * only thing entitled to call it is **a request that actually succeeded**.
 * `src/api/client.ts` does so from its two success paths (`handleResponse` and
 * `apiGetBlob`) and from nowhere else. So "one refresh" means one per run of
 * 401s, not one per page load.
 *
 * A TOKEN BEING ISSUED IS NOT A REQUEST BEING ACCEPTED, and conflating the two
 * is not a theoretical distinction — it was a real defect here. `AuthProvider`
 * used to reset this from inside its own `refresh` handler (`applyUser` cleared
 * it on any usable user), so every *successful* silent renewal granted itself a
 * fresh attempt. Whenever the IdP is happy but the gears are not — a role or
 * tenant removed, an audience/issuer mismatch, clock skew — that is an
 * unbounded refresh loop that never reaches `redirect` and never shows the user
 * the sign-in screen. `handleUnauthorized` therefore re-asserts this flag after
 * its attempt (see the `finally` below) rather than trusting its caller, and
 * `provider.tsx` no longer clears it at all.
 */
let refreshAttempted = false;

/** The refresh currently running, if any. Present so that the 401s which
 *  arrive *together* — the normal shape of an expiry here, because several
 *  queries poll on an interval — join one refresh instead of the first
 *  refreshing while the rest bounce the user to the login screen. Always
 *  cleared by the `finally` below, so it can never wedge. */
let refreshInFlight: Promise<unknown> | null = null;

/** Called on any 2xx, and ONLY on a 2xx — see `refreshAttempted`. A request
 *  came back, so the session is demonstrably working and the next 401 gets a
 *  fresh refresh attempt rather than an immediate redirect. Calling this
 *  because the IdP issued a token would defeat the rule; calling it from
 *  inside a `refresh` handler cannot, because `handleUnauthorized` re-asserts
 *  the flag afterwards. */
export function resetAuthAttempts(): void {
  refreshAttempted = false;
}

/**
 * Apply the 401 rule once.
 *
 * Never retries the failed request — the caller has already failed it, and
 * making this function retry is precisely how one 401 becomes a request storm.
 * A successful refresh fixes the *next* request, not this one.
 */
export async function handleUnauthorized({ refresh, redirect }: UnauthorizedHandlers): Promise<void> {
  if (refreshInFlight) {
    try {
      await refreshInFlight;
    } catch {
      redirect();
    }
    return;
  }
  if (refreshAttempted) {
    redirect();
    return;
  }
  refreshAttempted = true;
  // Wrapped in an async IIFE so a `refresh` that throws synchronously becomes a
  // rejected promise like any other, and so `refreshInFlight` is assigned
  // before the first `await` — which is what makes the join above see it.
  const attempt = (async () => refresh())();
  refreshInFlight = attempt;
  try {
    await attempt;
  } catch {
    redirect();
  } finally {
    if (refreshInFlight === attempt) {
      refreshInFlight = null;
    }
    // Re-asserted, not merely set once above. A `refresh` implementation that
    // clears the counter from inside itself — which the production wiring did,
    // by way of `applyUser` — would otherwise grant itself an unlimited supply
    // of attempts and this rule would silently not exist. Keeping the
    // bookkeeping inside the module that asserts the invariant is what makes it
    // true of every caller rather than of well-behaved callers.
    //
    // Note what this deliberately does NOT do: it does not reach past a
    // legitimate reset. `resetAuthAttempts()` called *between* two
    // `handleUnauthorized` calls — which is what a 2xx in `src/api/client.ts`
    // does — still permits the next refresh, because by then this `finally` has
    // already run. Only a reset racing the attempt is overruled.
    refreshAttempted = true;
  }
}

/* ------------------------------------------------------------------------- */
/* The access token itself, and the wiring `src/api/client.ts` reaches for.    */
/* ------------------------------------------------------------------------- */

let currentAccessToken: string | null = null;

/** Set by `AuthProvider` whenever the signed-in user changes; `null` signs out. */
export function setAccessToken(token: string | null): void {
  currentAccessToken = token;
}

/** The provider passes this to `setTokenProvider` — the single seam where `api`
 *  and `auth` meet (`src/api/client.ts`). */
export function getAccessToken(): string | null {
  return currentAccessToken;
}

let handlers: UnauthorizedHandlers | null = null;

/** Registered by `AuthProvider` on mount, cleared on unmount. Until it is set —
 *  Phases A and B, or any deployment running with `auth_disabled: true` — a 401
 *  does nothing here beyond becoming an `ApiError`, which is the pre-Phase-C
 *  behaviour. */
export function setUnauthorizedHandlers(next: UnauthorizedHandlers | null): void {
  handlers = next;
}

/** The 401 entry point for `src/api/client.ts`, which must not know about
 *  `oidc-client-ts` or React. A no-op when no provider is mounted. */
export async function notifyUnauthorized(): Promise<void> {
  if (!handlers) return;
  await handleUnauthorized(handlers);
}
