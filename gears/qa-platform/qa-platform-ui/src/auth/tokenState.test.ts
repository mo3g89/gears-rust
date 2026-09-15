/*
 * The 401 rule, which is the only piece of the auth subsystem with logic worth
 * a unit test: **exactly one silent refresh for a run of 401s, then redirect.**
 *
 * Why this and nothing else here: every other part of `src/auth` is either the
 * `oidc-client-ts` library's behaviour (testing it would test the library) or a
 * React render (exercised end-to-end against a real Keycloak rather than
 * here). This module is neither
 * — it is our own counter, and getting it wrong is how a revoked session turns
 * into an unbounded request storm. That is not hypothetical on this
 * deployment: an unauthenticated tab left on `/runs` was measured issuing 20
 * `/qa/v1` requests in 25s (34 once the stack had been seeded), all 401,
 * with no end condition (see the task report).
 *
 * No DOM here on purpose: `vitest.config.ts` runs in the `node` environment,
 * and `tokenState.ts` is kept free of `window` so it stays that way.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { handleUnauthorized, resetAuthAttempts } from './tokenState';

beforeEach(resetAuthAttempts);

describe('handleUnauthorized', () => {
  it('permits exactly one refresh for a run of 401s', async () => {
    const refresh = vi.fn().mockResolvedValue('new-token');
    const redirect = vi.fn();
    await handleUnauthorized({ refresh, redirect });
    await handleUnauthorized({ refresh, redirect });
    await handleUnauthorized({ refresh, redirect });
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(redirect).toHaveBeenCalledTimes(2);
  });

  it('redirects and does not retry when the refresh itself fails', async () => {
    const refresh = vi.fn().mockRejectedValue(new Error('revoked'));
    const redirect = vi.fn();
    await handleUnauthorized({ refresh, redirect });
    await handleUnauthorized({ refresh, redirect });
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(redirect).toHaveBeenCalledTimes(2);
  });

  it('allows a refresh again once a request has succeeded', async () => {
    const refresh = vi.fn().mockResolvedValue('t');
    const redirect = vi.fn();
    await handleUnauthorized({ refresh, redirect });
    resetAuthAttempts();                     // called on any 2xx
    await handleUnauthorized({ refresh, redirect });
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  /*
   * Not in the brief, added because the three above leave the *normal* Phase C
   * case unpinned. Several queries poll on an interval
   * (`src/api/hooks.ts:471`, `:730`, `:868`, `:1525`, `:2227`), so when an
   * access token expires the 401s arrive CONCURRENTLY, not one after another.
   * With a bare counter the first would refresh and every other in-flight
   * request would redirect to the login screen a few milliseconds later —
   * logging the user out on a token expiry that the refresh was about to fix.
   * So a 401 that finds a refresh already running joins it rather than
   * counting as a second attempt.
   */
  it('joins a refresh already in flight rather than redirecting a concurrent 401', async () => {
    let release!: (token: string) => void;
    const refresh = vi.fn(() => new Promise<string>((resolve) => { release = resolve; }));
    const redirect = vi.fn();
    const first = handleUnauthorized({ refresh, redirect });
    const second = handleUnauthorized({ refresh, redirect });
    release('new-token');
    await Promise.all([first, second]);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(redirect).not.toHaveBeenCalled();
  });

  /*
   * COMPOSITION, not the unit in isolation — this is the test whose absence let
   * a real bug through review.
   *
   * The three tests above inject a `refresh` that leaves the counter alone, so
   * they pass against an implementation whose "exactly one refresh" rule the
   * production wiring silently voids. It did: `AuthProvider`'s registered
   * `refresh` called `applyUser(renewed)`, and `applyUser` called
   * `resetAuthAttempts()` on any usable user — so every SUCCESSFUL refresh
   * cleared the counter from inside the refresh itself, and a run of 401s
   * refreshed forever instead of ever reaching `redirect`.
   *
   * That is not a hypothetical shape. It is what happens whenever the IdP is
   * happy but the gears are not: a role or tenant removed, an audience/issuer
   * mismatch after a config change, clock skew. A token being ISSUED is not
   * evidence that a request will be ACCEPTED, and only the second one may clear
   * this counter.
   *
   * So the rule now re-asserts its own bookkeeping after the attempt, and this
   * test pins that by handing it exactly the adversarial `refresh` the
   * production wiring used to be: one that resets the counter itself.
   */
  it('is not voided by a refresh that resets the counter from inside itself', async () => {
    const refresh = vi.fn(async () => {
      resetAuthAttempts();          // what applyUser() used to do at provider.tsx:129
      return 'new-token';
    });
    const redirect = vi.fn();
    await handleUnauthorized({ refresh, redirect });
    await handleUnauthorized({ refresh, redirect });
    await handleUnauthorized({ refresh, redirect });
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(redirect).toHaveBeenCalledTimes(2);
  });

  /* The same hazard on the failure path: a `refresh` that resets the counter
   * before rejecting must not buy itself a second attempt either. */
  it('is not voided by a refresh that resets the counter and then fails', async () => {
    const refresh = vi.fn(async () => {
      resetAuthAttempts();
      throw new Error('revoked');
    });
    const redirect = vi.fn();
    await handleUnauthorized({ refresh, redirect });
    await handleUnauthorized({ refresh, redirect });
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(redirect).toHaveBeenCalledTimes(2);
  });

  it('redirects every concurrent waiter when the in-flight refresh fails', async () => {
    let reject!: (e: Error) => void;
    const refresh = vi.fn(() => new Promise<string>((_resolve, r) => { reject = r; }));
    const redirect = vi.fn();
    const first = handleUnauthorized({ refresh, redirect });
    const second = handleUnauthorized({ refresh, redirect });
    reject(new Error('revoked'));
    await Promise.all([first, second]);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(redirect).toHaveBeenCalledTimes(2);
  });
});
