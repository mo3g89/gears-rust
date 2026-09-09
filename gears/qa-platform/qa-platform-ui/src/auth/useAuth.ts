/*
 * The auth context and the hook that reads it.
 *
 * The context lives here rather than in `provider.tsx` on purpose: this is a
 * `.ts` file with no components in it, so `provider.tsx` is left exporting
 * components and nothing else. That is what keeps `react-refresh` HMR working
 * for the provider (a module that exports both a component and a non-component
 * loses fast refresh), and it is also the file a reader looking for "what does
 * `useAuth()` give me" opens first.
 */
import { createContext, useContext } from 'react';
import type { User } from 'oidc-client-ts';

export interface AuthContextValue {
  /** The signed-in user, or `null`. `user.profile` carries the OIDC claims —
   *  `preferred_username` is what the sidebar shows. */
  user: User | null;
  /** True once a non-expired user is loaded and `getAccessToken()` has a token. */
  isAuthenticated: boolean;
  /** True until the initial "is there a stored session?" check has finished.
   *  Rendering guarded content before this clears would flash the sign-in
   *  screen on every reload. */
  isLoading: boolean;
  /**
   * Set when this session cannot be used and re-authenticating automatically
   * would loop: a silent refresh that failed, a callback that could not be
   * exchanged, or a 401 that survived a refresh. `RequireAuth` shows the login
   * screen instead of bouncing to the IdP when this is set — which is the
   * thing that stops "redirect to Keycloak, come back, get 401, redirect
   * again" from becoming an unbounded redirect loop. Cleared by `login()`.
   */
  error: string | null;
  /** Start the authorization-code + PKCE redirect to the IdP. */
  login: () => void;
  /** RP-initiated logout: ends the Keycloak session as well as this one. */
  logout: () => void;
}

export const AuthContext = createContext<AuthContextValue | null>(null);

export function useAuth(): AuthContextValue {
  const ctx = useContext(AuthContext);
  if (!ctx) {
    throw new Error('useAuth() must be used inside <AuthProvider>');
  }
  return ctx;
}
