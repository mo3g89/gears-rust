// @vitest-environment jsdom
//
// This is a component-shaped hook (state, effects, a browser `EventSource`), unlike
// `src/api/adapters.test.ts`'s pure-function suite — hence the per-file jsdom override
// instead of flipping `vitest.config.ts`'s project-wide `node` default (see that file's
// comment, which stays true for every other test in this repo).
//
// `jsdom` is pinned to `^25` in `package.json`, not the `^30` `npm i -D jsdom` picks by
// default: 30's dependency chain (`html-encoding-sniffer` → `@exodus/bytes`, ESM-only)
// fails to load under this environment's Node 18.19.1 (`ERR_REQUIRE_ESM`); 25 still
// declares `node: '>=18'` and loads cleanly.
//
// The gear's payload is JSON (`RunLogLineDto { line }` — `qa-runs/.../api/rest/dto.rs:1237`,
// `handlers/runs.rs:322-327`), not a bare string, so every fake frame below is
// `JSON.stringify({ line: ... })`, and a plain `EventSource` `MessageEvent` only has to
// supply `.data` for these purposes — the cast to `MessageEvent` matches the brief's own
// test skeleton.
//
// `useRunLogStream` resolves its argument through `resolveRunId` (`src/api/hooks.ts`)
// before opening a stream, because the gear's route is `Path<Uuid>`
// (`handlers/runs.rs:244`) while `LogViewer` hands the hook a run *name*. `resolveRunId`
// short-circuits synchronously-shaped (but still Promise-wrapped) for a uuid, so every
// test below uses an already-uuid-shaped id and awaits with `waitFor` rather than
// asserting on `EventSource` construction the instant `renderHook` returns.
import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  MAX_UNSTABLE_RETRIES,
  RECONNECT_GAP_MARKER,
  useRunLogStream,
} from './useRunLogStream';
import { setTokenProvider } from '@/api/client';

const RUN_ID = '11111111-1111-1111-1111-111111111111';

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  static get last(): FakeEventSource | null {
    return FakeEventSource.instances[FakeEventSource.instances.length - 1] ?? null;
  }
  onmessage: ((e: MessageEvent) => void) | null = null;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;
  constructor(public url: string) {
    FakeEventSource.instances.push(this);
  }
  close() {
    this.closed = true;
  }
}

function line(text: string): MessageEvent {
  return { data: JSON.stringify({ line: text }) } as MessageEvent;
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

beforeEach(() => {
  FakeEventSource.instances = [];
  (globalThis as unknown as { EventSource: unknown }).EventSource = FakeEventSource;
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('useRunLogStream', () => {
  it('opens no stream until a run id is given', () => {
    renderHook(() => useRunLogStream(null));
    expect(FakeEventSource.last).toBeNull();
  });

  it('opens a stream against the SSE route once a run id resolves, and reports connection state', async () => {
    const { result } = renderHook(() => useRunLogStream(RUN_ID));
    await waitFor(() => expect(FakeEventSource.last).not.toBeNull());
    const es = FakeEventSource.last!;
    expect(es.url).toContain(`/qa/v1/runs/${RUN_ID}/logs`);

    expect(result.current.isConnected).toBe(false);
    act(() => {
      es.onopen?.();
    });
    expect(result.current.isConnected).toBe(true);
  });

  // The credential, which for this one request lives in the URL because `EventSource`
  // cannot send headers. Both branches are asserted, because the *absence* of the
  // parameter is load-bearing too: `nginx.conf`'s `$sse_authorization` map falls back to
  // the inbound `Authorization` header when there is no parameter, and an always-present
  // (possibly empty) parameter would defeat that fallback and 401 the polled
  // `useRunLogs` request that shares this exact URL.
  it('carries the bearer token as an access_token query parameter when one is set', async () => {
    setTokenProvider(() => 'tok-abc.123');
    try {
      renderHook(() => useRunLogStream(RUN_ID));
      await waitFor(() => expect(FakeEventSource.last).not.toBeNull());
      expect(FakeEventSource.last!.url).toBe(
        `/qa/v1/runs/${RUN_ID}/logs?access_token=tok-abc.123`
      );
    } finally {
      setTokenProvider(() => null);
    }
  });

  it('omits the query parameter entirely when there is no token', async () => {
    setTokenProvider(() => null);
    renderHook(() => useRunLogStream(RUN_ID));
    await waitFor(() => expect(FakeEventSource.last).not.toBeNull());
    expect(FakeEventSource.last!.url).toBe(`/qa/v1/runs/${RUN_ID}/logs`);
  });

  // nginx does not percent-decode `$arg_access_token`, so encoding here is what keeps an
  // unusual token a 401 rather than a mangled URL -- assert the encoding actually happens
  // rather than trusting the template literal.
  it('percent-encodes a token containing characters that would break the query string', async () => {
    setTokenProvider(() => 'a&b=c d');
    try {
      renderHook(() => useRunLogStream(RUN_ID));
      await waitFor(() => expect(FakeEventSource.last).not.toBeNull());
      expect(FakeEventSource.last!.url).toBe(
        `/qa/v1/runs/${RUN_ID}/logs?access_token=a%26b%3Dc%20d`
      );
    } finally {
      setTokenProvider(() => null);
    }
  });

  it('accumulates lines parsed out of the JSON SSE payload, in order', async () => {
    const { result } = renderHook(() => useRunLogStream(RUN_ID));
    await waitFor(() => expect(FakeEventSource.last).not.toBeNull());
    const es = FakeEventSource.last!;

    act(() => {
      es.onmessage?.(line('first'));
    });
    act(() => {
      es.onmessage?.(line('second'));
    });
    // `waitFor`, not a bare assertion: lines are committed in a batched flush (see the
    // hook's `enqueue`), so they land on the next macrotask rather than synchronously
    // inside `act`.
    await waitFor(() => expect(result.current.messages).toEqual(['first', 'second']));
  });

  it('falls back to the raw frame when a message is not the expected JSON shape', async () => {
    const { result } = renderHook(() => useRunLogStream(RUN_ID));
    await waitFor(() => expect(FakeEventSource.last).not.toBeNull());
    const es = FakeEventSource.last!;

    act(() => {
      es.onmessage?.({ data: 'plain text, not json' } as MessageEvent);
    });
    act(() => {
      es.onmessage?.({ data: '{"unexpected":"shape"}' } as MessageEvent);
    });
    await waitFor(() =>
      expect(result.current.messages).toEqual(['plain text, not json', '{"unexpected":"shape"}'])
    );
  });

  // Finding 6 (review round 1): the null-transition used to leave a stale
  // `isConnected: true` behind, because the early-return guard skipped the reset. Assert
  // the state, not just that the underlying `EventSource` closed.
  it('closes the stream and reports itself disconnected when the run id goes away', async () => {
    const { result, rerender } = renderHook(({ id }) => useRunLogStream(id), {
      initialProps: { id: RUN_ID as string | null },
    });
    await waitFor(() => expect(FakeEventSource.last).not.toBeNull());
    const es = FakeEventSource.last!;
    act(() => {
      es.onopen?.();
    });
    expect(result.current.isConnected).toBe(true);

    rerender({ id: null });
    expect(es.closed).toBe(true);
    expect(result.current.isConnected).toBe(false);
  });

  it('closes the stream on unmount rather than leaking it', async () => {
    const { unmount } = renderHook(() => useRunLogStream(RUN_ID));
    await waitFor(() => expect(FakeEventSource.last).not.toBeNull());
    const es = FakeEventSource.last!;

    unmount();
    expect(es.closed).toBe(true);
  });

  // This is the load-bearing case (pre-flight fact #3): a finished run answers an
  // immediately-empty SSE stream (`handlers/runs.rs:254-261`), which a browser
  // `EventSource` — left to its own devices — treats as a closed connection to retry,
  // by default forever. The hook must bound that instead: every attempt below opens
  // (or fails to) and errors right away, never reaching the "stable" threshold, so the
  // hook has to give up rather than loop.
  it('gives up after repeated instant failures instead of reconnecting forever', async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useRunLogStream(RUN_ID));

    // Let resolveRunId's microtask resolve and the first EventSource get created.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(FakeEventSource.instances.length).toBe(1);

    // Fail the stream over and over, immediately each time (no stable connection ever
    // reached), advancing past every backoff delay the hook schedules.
    for (let i = 0; i < 10; i++) {
      const es = FakeEventSource.last!;
      act(() => {
        es.onerror?.();
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(30_000);
      });
    }

    // Finding 7 (review round 1): an exact count, not just an upper bound — pins both
    // edges (it retried at all, and it stopped) rather than only the "not more than"
    // edge, which a hook that never retried at all would also satisfy.
    expect(FakeEventSource.instances.length).toBe(1 + MAX_UNSTABLE_RETRIES);
    expect(result.current.hasGivenUp).toBe(true);
    expect(result.current.isConnected).toBe(false);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });
    expect(FakeEventSource.instances.length).toBe(1 + MAX_UNSTABLE_RETRIES);
  });

  // Finding 4 (review round 1): the give-up test above fires `onerror` without ever
  // firing `onopen`, but the actual empty-stream shape is "`onopen` fires, then `onerror`
  // fires almost immediately" (headers arrive with 200 before the zero-byte body ends the
  // response). An implementation that reset the instant-failure budget merely because
  // `onopen` fired — rather than requiring `STABLE_CONNECTION_MS` to actually elapse —
  // would pass the test above and still reconnect forever against every finished run.
  it('gives up even when the stream opens before erroring in the same tick, the real empty-stream shape', async () => {
    vi.useFakeTimers();
    renderHook(() => useRunLogStream(RUN_ID));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(FakeEventSource.instances.length).toBe(1);

    for (let i = 0; i < 10; i++) {
      const es = FakeEventSource.last!;
      act(() => {
        es.onopen?.();
        es.onerror?.();
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(30_000);
      });
    }

    expect(FakeEventSource.instances.length).toBe(1 + MAX_UNSTABLE_RETRIES);
  });

  // Contrast case: a connection that stayed open a while (past the 8-hour cutoff in
  // `sse.rs:86`, or a genuine network blip) is not the same failure as an instant one,
  // and is worth retrying rather than counting toward the same give-up budget. The
  // reconnect is paced (`STABLE_RECONNECT_BASE_DELAY_MS` + jitter), not instant, so this
  // advances a generous 3s — comfortably past the 1-2s the delay can land on — rather
  // than the 0ms the pre-fix zero-delay policy would have needed.
  it('retries a connection that had been open for a while before it dropped', async () => {
    vi.useFakeTimers();
    renderHook(() => useRunLogStream(RUN_ID));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(FakeEventSource.instances.length).toBe(1);

    const first = FakeEventSource.last!;
    act(() => {
      first.onopen?.();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000); // well past the stability threshold
    });
    act(() => {
      first.onerror?.();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3_000); // past the jittered stable-reconnect delay
    });

    expect(FakeEventSource.instances.length).toBe(2);
    expect(FakeEventSource.instances[1]).not.toBe(first);
  });

  // Finding 2 (review round 1): a stable-then-dropped connection used to reset the
  // failure budget *and* reconnect at zero delay, so a connection that is always just
  // barely stable before dropping (an ordinary front-proxy idle timeout — 10-60s is
  // common) would hammer the gear's subscriber slots forever. Proves the reconnect is
  // paced (no new `EventSource` appears immediately after the error, only after the
  // delay) across many stable-drop cycles in a row.
  //
  // Review round 2, finding B: this used to also assert a `MAX_TOTAL_ATTEMPTS` ceiling
  // fired at a fixed count. That ceiling's own justifying comment did the arithmetic
  // wrong (100 against an "8h at 60s" example that is actually 480), and this test's own
  // purely-stable cycles proved the ceiling firing squarely inside the case the comment
  // claimed to exempt. The fix removed the ceiling rather than pick a new number with the
  // same shape of problem — see `STABLE_RECONNECT_BASE_DELAY_MS`'s doc for why the pacing
  // alone already bounds the hazard. So this test now proves the *opposite* of a ceiling:
  // reconnecting stays paced, not instant, indefinitely — many cycles in, it is still
  // creating exactly one new `EventSource` per cycle, never more, never zero.
  it('paces reconnects after a stable drop instead of retrying at zero delay, indefinitely', async () => {
    vi.useFakeTimers();
    renderHook(() => useRunLogStream(RUN_ID));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    const CYCLES = 30;
    for (let i = 0; i < CYCLES; i++) {
      const es = FakeEventSource.last!;
      act(() => {
        es.onopen?.();
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(6_000); // past the stability threshold
      });
      const countBeforeError = FakeEventSource.instances.length;
      act(() => {
        es.onerror?.();
      });
      // Immediately after the error there is no new EventSource yet — the reconnect is
      // scheduled, not instant.
      expect(FakeEventSource.instances.length).toBe(countBeforeError);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3_000); // past the max jittered delay (2s)
      });
      // Exactly one new EventSource per cycle — not zero (it kept going) and not more
      // than one (nothing is racing ahead of the pacing).
      expect(FakeEventSource.instances.length).toBe(countBeforeError + 1);
    }

    // No ceiling: `CYCLES` stable drops in a row produced `CYCLES + 1` total attempts
    // (the initial connection plus one per cycle), still going, still paced.
    expect(FakeEventSource.instances.length).toBe(CYCLES + 1);
  }, 20_000);

  // Finding 3 (review round 1): the gear's own subscription "only ever carries lines
  // published after it was created" (`broadcast.rs:286`), so a reconnect leaves a gap the
  // new subscription cannot see. Left unmarked, that gap lands invisibly in the middle of
  // the rendered log — exactly what `broadcast.rs:117-119` says its own `gap_marker`
  // exists to avoid for the in-subscription lag case.
  it('marks a gap in the log when reconnecting, but not on the first connection', async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useRunLogStream(RUN_ID));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    const first = FakeEventSource.last!;
    act(() => {
      first.onopen?.();
    });
    expect(result.current.messages).toEqual([]); // no marker on the very first open

    act(() => {
      first.onmessage?.(line('during-first-connection'));
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(6_000); // past the stability threshold
    });
    act(() => {
      first.onerror?.();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3_000); // past the jittered stable-reconnect delay
    });

    const second = FakeEventSource.last!;
    expect(second).not.toBe(first);
    act(() => {
      second.onopen?.();
    });
    // Drain the batched flush the gap marker is enqueued into (the hook's `enqueue`).
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(result.current.messages).toEqual(['during-first-connection', RECONNECT_GAP_MARKER]);
  });

  // Review round 2, finding A: the marker used to fire on *any* reconnect after the
  // first open, keyed on "has this hook ever connected before" rather than on whether
  // the connection being replaced was actually stable. That fires on every instant-retry
  // attempt too — three or four false gap markers in a row for a finished run's empty
  // stream (open, error almost instantly, repeat), even though nothing was ever streamed
  // and so nothing was missed. A marker test that only checks presence cannot catch a
  // marker that fires too often, so this asserts absence specifically after an instant
  // (never-stable) reconnect.
  it('does not mark a gap when reconnecting after an instant failure, since nothing was ever streamed', async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useRunLogStream(RUN_ID));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    const first = FakeEventSource.last!;
    act(() => {
      // Opens and errors in the same tick — the real empty-stream shape, and never
      // reaches STABLE_CONNECTION_MS.
      first.onopen?.();
      first.onerror?.();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_000); // past the instant-failure backoff
    });

    const second = FakeEventSource.last!;
    expect(second).not.toBe(first);
    act(() => {
      second.onopen?.();
    });

    expect(result.current.messages).toEqual([]); // no false gap marker
  });

  it('resolves a plain run name to an id via resolveRunId before opening the stream', async () => {
    const fetchMock = vi.fn(async () => jsonResponse({ items: [{ id: RUN_ID, name: 'collect-abc-1' }] }));
    vi.stubGlobal('fetch', fetchMock);

    renderHook(() => useRunLogStream('collect-abc-1'));
    await waitFor(() => expect(FakeEventSource.last).not.toBeNull());

    expect(fetchMock).toHaveBeenCalledWith(
      expect.stringContaining('/qa/v1/runs?'),
      expect.anything()
    );
    expect(FakeEventSource.last!.url).toContain(`/qa/v1/runs/${RUN_ID}/logs`);
  });

  // Finding 5 (review round 1): the resolve `.catch()` used to swallow every rejection —
  // including a transient failure on the id lookup itself, not just a genuine "no such
  // run" — permanently yielding no stream and (per finding 1) a permanent spinner. Only
  // the genuine not-found case should be terminal; everything else should retry.
  it('retries resolving the run id after a transient failure, rather than giving up like a genuine "no such run"', async () => {
    vi.useFakeTimers();
    let calls = 0;
    const fetchMock = vi.fn(async () => {
      calls += 1;
      if (calls === 1) {
        throw new TypeError('network error');
      }
      return jsonResponse({ items: [{ id: RUN_ID, name: 'collect-abc-1' }] });
    });
    vi.stubGlobal('fetch', fetchMock);

    const { result } = renderHook(() => useRunLogStream('collect-abc-1'));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(FakeEventSource.instances.length).toBe(0); // first lookup failed; nothing opened yet
    expect(result.current.hasGivenUp).toBe(false); // transient, not terminal

    await act(async () => {
      // Past the resolve retry's backoff. `advanceTimersByTimeAsync` flushes the
      // microtasks in between too, so the second `fetch` call and its `.then` chain
      // (through to `openStream`) complete within this one advance — no `waitFor`
      // needed (and `waitFor`'s own internal polling uses real timers, which never
      // advance while fake timers are installed, so it would hang here instead).
      await vi.advanceTimersByTimeAsync(2_000);
    });
    expect(FakeEventSource.last).not.toBeNull();
    expect(FakeEventSource.last!.url).toContain(`/qa/v1/runs/${RUN_ID}/logs`);
    expect(calls).toBe(2);
  });

  it('gives up immediately on a genuine "no such run", without retrying the lookup', async () => {
    vi.useFakeTimers();
    const fetchMock = vi.fn(async () => jsonResponse({ items: [] }));
    vi.stubGlobal('fetch', fetchMock);

    const { result } = renderHook(() => useRunLogStream('no-such-run'));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(result.current.hasGivenUp).toBe(true);
    expect(FakeEventSource.instances.length).toBe(0);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000);
    });
    expect(fetchMock).toHaveBeenCalledTimes(1); // never retried — this run does not exist
    expect(FakeEventSource.instances.length).toBe(0);
  });
});
