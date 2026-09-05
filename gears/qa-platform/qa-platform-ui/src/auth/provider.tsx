/*
 * The OIDC subsystem's implementation: one `UserManager`, the React state that
 * mirrors it, and the two seams the rest of the app touches it through
 * (`setTokenProvider` in `src/api/client.ts`, and `setUnauthorizedHandlers` in
 * `./tokenState`).
 *
 * FLOW: authorization code + PKCE against Keycloak, as a public client.
 * `oidc-client-ts@3.5.0` sends `code_challenge_method=S256` unless
 * `disablePKCE` is set, which defaults to `false` — read out of the library
 * rather than assumed (`dist/types/oidc-client-ts.d.ts:635-637` for the
 * default, `dist/esm/oidc-client-ts.js:1699` for the literal `"S256"`). The
 * realm *requires* S256 (`pkce.code.challenge.method` on the `qa-platform-ui`
 * client), so a silent library default change surfaces as a Keycloak error
 * rather than a quiet downgrade to `plain`.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { User, UserManager, WebStorageStateStore } from 'oidc-client-ts';
import { useQueryClient } from '@tanstack/react-query';
import { useNavigate } from 'react-router-dom';
import { setTokenProvider } from '@/api/client';
import {
  getAccessToken,
  setAccessToken,
  setUnauthorizedHandlers,
} from './tokenState';
import { AuthContext } from './useAuth';
import type { AuthContextValue } from './useAuth';

/**
 * THE ISSUER THE BROWSER GETS, and the one value here that breaks everything if
 * it is wrong. `http://localhost:8180/realms/qa-platform` is the URL a browser
 * on the host can reach AND the `iss` Keycloak stamps into the token — verified
 * against the live realm:
 *   curl -s http://localhost:8180/realms/qa-platform/.well-known/openid-configuration
 *   -> "issuer": "http://localhost:8180/realms/qa-platform"
 *
 * `https://keycloak:8443/realms/qa-platform` is the *gears'* discovery URL. It
 * resolves only inside the compose network and is pinned to a private CA, so a
 * browser cannot use it and it is never an issuer identity. If a `VITE_OIDC_*`
 * value here ever reads `keycloak:8443`, that is the bug.
 *
 * Defaulted in code, the same way `API_BASE_URL` is (`src/api/client.ts:6`),
 * because the image is built by `deploy/docker/qa-platform-ui.Dockerfile` from
 * a context that carries no `.env` — vite inlines `import.meta.env` at build
 * time, so an unset variable is an empty string in the bundle, not something
 * that can be supplied later. That Dockerfile declares both as build `ARG`s so
 * a non-localhost deployment can override them without editing this file.
 */
const OIDC_ISSUER = import.meta.env.VITE_OIDC_ISSUER || 'http://localhost:8180/realms/qa-platform';
const OIDC_CLIENT_ID = import.meta.env.VITE_OIDC_CLIENT_ID || 'qa-platform-ui';

/** Where Keycloak sends the browser back with `?code=&state=`. Covered by the
 *  realm's `http://localhost:8080/*` wildcard, and served by nginx's SPA
 *  fallback (`try_files $uri $uri/ /index.html`) — it matches neither the
 *  `location /qa/v1/` prefix nor the static-asset extension regex. */
const CALLBACK_PATH = '/auth/callback';
/** Where RP-initiated logout returns to. Also inside the realm's wildcard. */
const LOGGED_OUT_PATH = '/login';

/**
 * One manager for the whole app, at module scope rather than in a `useMemo`.
 * Two reasons, both concrete: `StrictMode` mounts effects twice in development
 * (`src/main.tsx:7`), and a second `UserManager` would start a second silent
 * renewal timer against the same session; and the callback route below needs
 * the *same* instance that issued the request, because the PKCE `code_verifier`
 * it must consume lives in that instance's state store.
 */
const userManager = new UserManager({
  authority: OIDC_ISSUER,
  client_id: OIDC_CLIENT_ID,
  redirect_uri: window.location.origin + CALLBACK_PATH,
  post_logout_redirect_uri: window.location.origin + LOGGED_OUT_PATH,
  response_type: 'code',
  scope: 'openid profile email',
  // Renew before expiry rather than waiting for a 401. The realm's
  // `accessTokenLifespan` is 300s, so without this every session would take the
  // `handleUnauthorized` path every five minutes.
  automaticSilentRenew: true,
  // The default is already sessionStorage; stated explicitly because "which
  // store holds the session" is a security property of this file and should not
  // depend on a library default staying put. sessionStorage is per-tab and
  // cleared when the tab closes; localStorage — which would survive the browser
  // closing and be shared across tabs — is deliberately not used.
  userStore: new WebStorageStateStore({ store: window.sessionStorage }),
});

/**
 * The `?code=` exchange must happen exactly once per page load. `StrictMode`
 * mounts every effect twice, and `signinRedirectCallback` consumes the stored
 * PKCE state, so a second call fails with "No matching state found in
 * storage" — which would render a spurious error on a login that in fact
 * succeeded. A module-level flag is the right scope: the callback page is
 * reached by a full navigation, so a fresh page load gets a fresh module.
 */
let callbackExchangeStarted = false;

/** What `signinRedirect` round-trips through the IdP so the user comes back to
 *  the page they asked for rather than to `/`. */
interface SigninState {
  returnTo?: string;
}

function currentLocation(): string {
  return window.location.pathname + window.location.search;
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<User | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const queryClient = useQueryClient();

  /** The single point where `api` and `auth` meet. `getAccessToken` reads a
   *  module variable in `./tokenState`, so the API client never sees storage
   *  and never sees React. */
  useEffect(() => {
    setTokenProvider(getAccessToken);
  }, []);

  /**
   * Mirror the manager's user into React state and into the token the API
   * client reads. That is ALL it does.
   *
   * It deliberately does **not** call `resetAuthAttempts()`, and that omission
   * is the fix for a real defect rather than an oversight. It used to, on any
   * usable user — which meant the `refresh` handler below cleared the 401
   * counter from inside the refresh itself, so "exactly one silent refresh"
   * became "refresh forever". `automaticSilentRenew` reached the same reset by
   * a second path, through the `userLoaded` subscription. Whenever the IdP
   * renews happily but the gears still answer 401 — a role or tenant removed,
   * an audience/issuer mismatch after a config change, clock skew — that is an
   * unbounded loop of request-plus-refresh with no end condition, and the
   * loop-breaker at `redirect` below was unreachable.
   *
   * The invariant, stated once: **only a request that actually succeeded may
   * clear that counter**, because only that demonstrates the gears accept this
   * token. `src/api/client.ts` does it from its two success paths and nothing
   * else does it at all.
   */
  const applyUser = useCallback((next: User | null) => {
    const usable = next && !next.expired ? next : null;
    setUser(usable);
    setAccessToken(usable ? usable.access_token : null);
  }, []);

  // Restore an existing session, and follow the manager thereafter.
  useEffect(() => {
    let cancelled = false;
    userManager
      .getUser()
      .then((restored) => {
        if (cancelled) return;
        applyUser(restored);
      })
      .catch(() => {
        if (!cancelled) applyUser(null);
      })
      .finally(() => {
        if (!cancelled) setIsLoading(false);
      });

    const unsubscribers = [
      userManager.events.addUserLoaded((loaded) => applyUser(loaded)),
      userManager.events.addUserUnloaded(() => applyUser(null)),
      userManager.events.addAccessTokenExpired(() => applyUser(null)),
      // A failed silent renewal is a dead session. Recording it as an `error`
      // rather than just clearing the user is what makes `RequireAuth` show the
      // sign-in screen instead of bouncing straight back to the IdP — see
      // `AuthContextValue.error`.
      userManager.events.addSilentRenewError((e) => {
        applyUser(null);
        setError(`Your session could not be renewed (${e.message}). Please sign in again.`);
      }),
    ];
    return () => {
      cancelled = true;
      unsubscribers.forEach((off) => off());
    };
  }, [applyUser]);

  const login = useCallback(() => {
    setError(null);
    const state: SigninState = { returnTo: currentLocation() };
    void userManager.signinRedirect({ state }).catch((e: unknown) => {
      setError(`Could not reach the sign-in service: ${e instanceof Error ? e.message : String(e)}`);
    });
  }, []);

  const logout = useCallback(() => {
    setError(null);
    applyUser(null);
    // Anything cached was fetched with the outgoing user's token and their
    // tenant scope. Dropping it is what stops the next sign-in painting the
    // previous user's rows for a frame.
    queryClient.clear();
    void userManager.signoutRedirect().catch(() => {
      // The IdP is unreachable, but this browser is signed out either way —
      // `applyUser(null)` above already cleared the token. Fall back to a local
      // sign-out so the user is not stranded on a page they cannot use.
      void userManager.removeUser();
      window.location.assign(LOGGED_OUT_PATH);
    });
  }, [applyUser, queryClient]);

  /*
   * The 401 rule's two effects, handed to `./tokenState` so `src/api/client.ts`
   * can trigger them without importing any of this.
   *
   * `redirect` deliberately does NOT start a new sign-in redirect. If the IdP
   * would hand back a token the gears still refuse — a tenant that does not
   * resolve, say — an automatic retry is an infinite redirect loop, which is
   * the same failure as the request storm with a slower period. Setting `error`
   * puts the user in front of a sign-in button instead: one deliberate click,
   * not a loop.
   */
  useEffect(() => {
    setUnauthorizedHandlers({
      refresh: async () => {
        const renewed = await userManager.signinSilent();
        if (!renewed) {
          throw new Error('the identity provider returned no session');
        }
        // Publishes the new token so the NEXT request carries it. It does not,
        // and must not, tell `tokenState` that the session is working — see
        // `applyUser`. Whether the gears accept this token is answered by the
        // next response, not by the IdP having minted it.
        applyUser(renewed);
        return renewed.access_token;
      },
      redirect: () => {
        applyUser(null);
        void userManager.removeUser();
        setError('Your session has expired or was revoked. Please sign in again.');
      },
    });
    return () => setUnauthorizedHandlers(null);
  }, [applyUser]);

  const value = useMemo<AuthContextValue>(
    () => ({
      user,
      isAuthenticated: !!user,
      isLoading,
      error,
      login,
      logout,
    }),
    [user, isLoading, error, login, logout]
  );

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

/**
 * `/auth/callback`. Exchanges `?code=` for tokens, then replaces itself with
 * the route the user originally asked for, so the code and state never stay in
 * history.
 *
 * It renders no data-fetching component at any point, which matters: this route
 * sits OUTSIDE `RequireAuth`, so anything it mounted would be mounted
 * unauthenticated.
 */
export function AuthCallbackPage() {
  const navigate = useNavigate();
  const [failure, setFailure] = useState<string | null>(null);
  const done = useRef(false);

  useEffect(() => {
    if (callbackExchangeStarted || done.current) return;
    callbackExchangeStarted = true;
    done.current = true;
    userManager
      .signinRedirectCallback()
      .then((signedIn) => {
        const state = signedIn.state as SigninState | undefined;
        const target = state?.returnTo && state.returnTo !== CALLBACK_PATH ? state.returnTo : '/';
        navigate(target, { replace: true });
      })
      .catch((e: unknown) => {
        setFailure(e instanceof Error ? e.message : String(e));
      });
  }, [navigate]);

  return (
    <main className="flex min-h-screen items-center justify-center p-4">
      {failure ? (
        <div className="text-center py-8">
          <p className="text-destructive">Sign-in failed: {failure}</p>
          <a className="text-sm text-primary underline-offset-4 hover:underline" href={LOGGED_OUT_PATH}>
            Back to sign in
          </a>
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">Completing sign-in…</p>
      )}
    </main>
  );
}
