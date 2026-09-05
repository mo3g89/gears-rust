import { useEffect, useRef, useState } from 'react';
import { API_BASE_URL, currentAccessToken } from '@/api/client';
import { resolveRunId } from '@/api/hooks';

interface UseRunLogStreamOptions {
  onMessage?: (line: string) => void;
}

interface UseRunLogStreamResult {
  isConnected: boolean;
  messages: string[];
  /**
   * True once the hook has exhausted its retry budget (or hit a run that genuinely
   * doesn't exist) and stopped trying. A consumer that keeps showing "Connecting..."
   * once this is true is lying about still being in progress — see `LogViewer.tsx`'s
   * `useWs = !hasGivenUp`, which switches its header to the honest "Polling" state.
   */
  hasGivenUp: boolean;
}

/**
 * A connection has to stay open at least this long before a later error counts as a
 * real drop rather than an instant failure. Comfortably under the gear's 30s keep-alive
 * (`qa-runs/.../api/rest/sse.rs:94` — so a silent-but-open stream never trips this), and
 * far above what either failure mode below takes: a finished run's stream is answered
 * with `sse_response(futures::stream::empty())` (`handlers/runs.rs:254-261`), which opens
 * and ends in the same tick; a run already at the 16-subscriber cap
 * (`infra/logs/broadcast.rs:102`) is answered with a non-2xx error response
 * (`handlers/runs.rs:263-268,296-302`), which `EventSource` can't open at all.
 *
 * This is a *duration*, not an event: classification reads `Date.now() - openedAt`
 * against this constant, never just "did `onopen` fire". An implementation that reset
 * the instant-failure budget merely because `onopen` fired would misclassify the
 * empty-stream case — which does open, then errors in the same tick — as stable, and
 * retry it forever. The same duration check gates the reconnect-gap marker below: only a
 * connection that actually carried this much time can have missed content worth flagging.
 */
const STABLE_CONNECTION_MS = 5000;

/**
 * How many consecutive *instant* failures this hook tolerates before it stops trying.
 * `EventSource` retries a closed connection on its own, by default forever — this hook
 * takes that decision away (closing and re-opening itself on every error) specifically so
 * it can give up. A finished run's empty stream, or a capped run's error response, both
 * fail exactly the same way on every retry, so there is nothing to wait out. Also used,
 * with the same reasoning, as the retry budget for a failure to *resolve* a run name to
 * an id in the first place (see `tryResolve` below) — a different failure domain, but the
 * same "bounded retries, then stop" shape.
 */
export const MAX_UNSTABLE_RETRIES = 3;

/**
 * Minimum delay, plus jitter, before reopening a stream after a connection that *was*
 * stable (open at least `STABLE_CONNECTION_MS`) drops. Without a floor here, a
 * connection that is always just barely stable before dropping — an ordinary
 * front-proxy idle timeout, commonly 10-60s — would reconnect at zero delay forever,
 * repeatedly taking and releasing one of the gear's 16 subscriber slots
 * (`broadcast.rs:102`) for no benefit. The jitter avoids every open tab reconnecting in
 * lockstep on the same schedule.
 *
 * **This is the only thing bounding the stable-reconnect path, and that is deliberate**
 * (review round 2, finding B). An earlier draft also added a `MAX_TOTAL_ATTEMPTS` ceiling
 * "independent of wasStable", reasoned against "an 8h run recycling every 60s is under
 * 500 reconnects" — but 8h at 60s is 480 reconnects, and the ceiling was 100, so the
 * comment's own arithmetic contradicted the number it was defending: the ceiling would
 * have fired on the exact legitimate case it claimed to exempt. Rather than pick a new
 * round number with the same problem, this floor is the actual fix for the hazard finding
 * 2 identified — hammering the gear's subscriber slots at zero delay — because it puts a
 * hard rate limit on reconnect attempts (at most one roughly every 1-2s, the same floor
 * `backoffMs`'s first step already enforces on the instant-failure path). A connection
 * that keeps proving itself viable for `STABLE_CONNECTION_MS` before dropping is doing
 * exactly what a live log stream should do, for as long as the run runs; capping the
 * *count* of such reconnects would only cut off a well-behaved long-lived stream at a
 * point this hook has no principled way to choose, in exchange for bounding a hazard the
 * rate limit already bounds.
 */
const STABLE_RECONNECT_BASE_DELAY_MS = 1000;
const STABLE_RECONNECT_JITTER_MS = 1000;

function backoffMs(attempt: number): number {
  return Math.min(1000 * 2 ** attempt, 8000);
}

function stableReconnectDelayMs(): number {
  return STABLE_RECONNECT_BASE_DELAY_MS + Math.random() * STABLE_RECONNECT_JITTER_MS;
}

/**
 * Synthesized client-side, not sent by the gear — this hook's own resubscribe leaves a
 * gap the gear has no way to fill in. `broadcast.rs:286`: "The subscription only ever
 * carries lines published **after** it was created", so a fresh `EventSource` opened
 * after a reconnect cannot see whatever was published during the gap. The gear's own
 * `gap_marker` (`broadcast.rs:122`) covers a different case — a receiver that fell behind
 * *within* one live subscription — and knows exactly how many lines it skipped; this
 * hook doesn't, because it isn't the same subscription. Mirrors that function's
 * `[qa-runs] log gap: ...` prefix convention and its stated reasoning
 * (`broadcast.rs:117-119`): "a caller that swallowed it would hand an operator a log
 * with an invisible hole in the middle, which is worse than a visibly truncated one".
 *
 * **Fires only when replacing a connection that was actually `STABLE_CONNECTION_MS` old**
 * (review round 2, finding A). The first draft keyed this on "has this hook ever
 * connected before", which fires on *every* reconnect including the instant-failure
 * retries a finished run's empty stream produces — three or four false gap markers,
 * with no content ever missed, shown in place of the real (already-available) polled log
 * for the several seconds before the hook gives up. Gating on the same stable-duration
 * check the retry budget already uses means the marker only appears when a connection
 * that actually carried content is the one being replaced.
 */
export const RECONNECT_GAP_MARKER =
  '[qa-runs] log gap: reconnected — lines published while disconnected are not shown';

/**
 * The exact prefix `resolveRunId` (`src/api/hooks.ts`) throws when a name matches no run
 * — the only rejection from it that means "stop trying" rather than "retry". Matched by
 * prefix against a plain `Error` because `resolveRunId` has no dedicated not-found error
 * type; if its message ever changes, the failure mode is graceful (this hook starts
 * retrying a genuinely-absent run too, rather than silently mis-classifying something
 * else as absent), not silent.
 */
const NOT_FOUND_MESSAGE_PREFIX = 'No run named';

/**
 * The query parameter that carries the bearer token on this one stream — the only
 * place in this app that puts a credential in a URL.
 *
 * WHY IT IS NECESSARY. `EventSource` cannot send request headers; its constructor has
 * no `headers` option. So the token every other request carries through
 * `authHeaders()` (`src/api/client.ts`) cannot ride this one, and the gears offer no
 * alternative: `extract_bearer_http`
 * (`libs/toolkit-http-middleware/src/security.rs:47`) takes `&HeaderMap` and reads only
 * `AUTHORIZATION`, and `security_context_middleware`
 * (`libs/toolkit-http-middleware/src/auth.rs:112`) calls it with `request.headers()`.
 * That function cannot see the request URI at all, so authenticating by query
 * parameter is not a gear setting somebody forgot to enable — it is unreachable — and
 * there is no cookie branch either (`security.rs:24-35`: three error variants, all
 * about that one header).
 *
 * WHAT ACTUALLY READS IT. Not the gear — **nginx**, which sits in the browser's path
 * to this route. `gears/qa-platform/qa-platform-ui/default.conf.template`
 * (renamed from nginx.conf when it became an envsubst template) translates this
 * parameter into the `Authorization: Bearer …` the toolkit does read, for this route
 * and no other. Its `$sse_authorization` map carries the rest of the reasoning: why it
 * is a map rather than a bare `proxy_set_header` (an unconditional one would overwrite
 * the real header the polled `useRunLogs` fallback sends to this same URL), how it
 * fails closed, and the costs of a token in a URL. Those costs are recorded for a
 * human in `gears/qa-platform/docs/CONTRACT-DIFF.md` §12.
 *
 * Spelled `access_token` after RFC 6750 §2.3 — the standard name for a bearer token in
 * a URI query — rather than a house convention this file and `nginx.conf` would have
 * to agree on twice with nothing to check it.
 */
const ACCESS_TOKEN_PARAM = 'access_token';

/**
 * The URL one `EventSource` opens against.
 *
 * Called from inside `openStream`, i.e. **on every open including every reconnect**,
 * so a stream that drops after a silent renewal reopens with the new token rather than
 * the one captured when the hook first mounted. The token is deliberately *not* an
 * effect dependency: making it one would tear down and rebuild the stream on every
 * renewal, which is exactly the reconnect churn `STABLE_RECONNECT_BASE_DELAY_MS`
 * exists to bound.
 */
function streamUrl(id: string): string {
  const base = `${API_BASE_URL}/runs/${id}/logs`;
  const token = currentAccessToken();
  // No token — auth off, or signed out — means *no parameter*, not an empty one. The
  // nginx map falls back to the inbound `Authorization` header when the parameter is
  // absent, so this adds nothing to a request that had no credential to begin with:
  // it gets the same 401 it got before.
  if (!token) {
    return base;
  }
  // Encoded even though a JWT's alphabet (base64url plus `.`) never needs it. nginx
  // does NOT percent-decode `$arg_…`, so a token that genuinely required encoding
  // would reach the gear still encoded and be rejected — a 401, which is the safe
  // direction, rather than a mangled URL or a credential that decodes to something
  // else.
  return `${base}?${ACCESS_TOKEN_PARAM}=${encodeURIComponent(token)}`;
}

/**
 * Pull the log line out of one SSE frame. The route is `.sse_json::<RunLogLineDto>`
 * (`routes/runs.rs:184`) and `RunLogLineDto` is `{ line: String }` (`dto.rs:1237`), so
 * `event.data` is `{"line":"…"}` — mirrors `parseRunLogSse` in `src/api/adapters.ts`,
 * which does the same extraction for the polled body. A frame that isn't that shape is
 * passed through as-is rather than thrown away, so one malformed frame degrades instead
 * of breaking the handler.
 */
function extractLine(raw: string): string {
  try {
    const parsed = JSON.parse(raw) as { line?: unknown };
    if (parsed && typeof parsed.line === 'string') {
      return parsed.line;
    }
  } catch {
    // Not JSON — fall through to the raw frame.
  }
  return raw;
}

/**
 * Streams one run's logs over SSE (`GET /runs/{id}/logs`, unnamed `message` events —
 * `qa-runs/.../api/rest/routes/runs.rs:184`, no `.event(...)` call anywhere in the
 * handler or `sse.rs`).
 *
 * `runIdOrName` accepts either, resolved via `resolveRunId` (`src/api/hooks.ts`) before
 * the stream opens — its only caller, `LogViewer`, only has the run's name
 * (`LogViewer.tsx:173,180`), while the gear's route is `Path<Uuid>` (`handlers/runs.rs:244`).
 *
 * Returns the same shape the legacy WebSocket hook it replaces did (`isConnected`,
 * `messages`), plus `hasGivenUp` — see that field's own doc for why a third field was
 * necessary rather than a scope violation.
 */
export function useRunLogStream(
  runIdOrName: string | null,
  opts: UseRunLogStreamOptions = {}
): UseRunLogStreamResult {
  const [isConnected, setIsConnected] = useState(false);
  const [messages, setMessages] = useState<string[]>([]);
  const [hasGivenUp, setHasGivenUp] = useState(false);
  const onMessageRef = useRef(opts.onMessage);
  onMessageRef.current = opts.onMessage;

  useEffect(() => {
    if (!runIdOrName) {
      setIsConnected(false);
      return;
    }

    setMessages([]);
    setIsConnected(false);
    setHasGivenUp(false);

    // Narrowed once, outside the closures below: TS does not carry the `if (!runIdOrName)`
    // guard's narrowing into a nested function declaration's body.
    const targetRunIdOrName: string = runIdOrName;

    let cancelled = false;
    let es: EventSource | null = null;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;
    // Incoming lines are buffered and flushed once per animation frame rather than
    // committed one `setMessages` at a time.
    //
    // A per-line `setMessages(prev => [...prev, line])` is O(n) per line, and every
    // consumer downstream of it is too: `LogViewer` re-joins the whole array into one
    // string, re-splits it, and re-runs `buildBlocks`' regex classification over every
    // line on each commit. That is four quadratic passes, and it only became visible
    // once finished runs started replaying their archived log — a 22k-line run replayed
    // 22k times through that chain, which is roughly 26 GB of string churn for a 2.4 MB
    // log. Legacy never had it because it fetched one text body and parsed it once
    // (`manager-ui/src/components/runs/LogViewer.tsx` + `manager/src/routes/runs.rs`'s
    // `api_logs`).
    //
    // Batching keeps the streaming path (which legacy had no equivalent of) while
    // bounding the number of those passes to the frame rate instead of the line count.
    // Order is preserved because the gap marker goes through this same buffer.
    let pending: string[] = [];
    let flushHandle: ReturnType<typeof setTimeout> | null = null;

    function flushPending() {
      flushHandle = null;
      if (cancelled || pending.length === 0) return;
      const batch = pending;
      pending = [];
      setMessages((prev) => prev.concat(batch));
    }

    function enqueue(line: string) {
      pending.push(line);
      // `setTimeout(0)` rather than `requestAnimationFrame`: rAF does not fire in a
      // background tab, which would let a long replay accumulate unboundedly and then
      // land in one burst when the tab is focused.
      if (flushHandle === null) flushHandle = setTimeout(flushPending, 0);
    }
    let unstableRetries = 0;
    let resolveRetries = 0;
    let openedAt = 0;
    // Set only when scheduling a reconnect after a *stable* drop (see the `wasStable`
    // branch below), and consumed by the next `onopen` — so the gap marker fires exactly
    // when a connection that carried real time is the one being replaced, never on the
    // first connection and never after a run of instant failures.
    let markGapOnNextOpen = false;

    function giveUp() {
      setIsConnected(false);
      setHasGivenUp(true);
    }

    function openStream(id: string) {
      es = new EventSource(streamUrl(id));

      es.onopen = () => {
        if (cancelled) return;
        openedAt = Date.now();
        setIsConnected(true);
        if (markGapOnNextOpen) {
          // Replacing a connection that was actually stable — the new subscription
          // cannot see whatever was published during the gap (`broadcast.rs:286`). Say
          // so rather than splicing the two halves together silently.
          enqueue(RECONNECT_GAP_MARKER);
          markGapOnNextOpen = false;
        }
      };

      es.onmessage = (event) => {
        if (cancelled) return;
        const messageLine = extractLine((event as MessageEvent).data);
        enqueue(messageLine);
        onMessageRef.current?.(messageLine);
      };

      es.onerror = () => {
        if (cancelled) return;
        es?.close();
        setIsConnected(false);

        const wasStable = openedAt > 0 && Date.now() - openedAt >= STABLE_CONNECTION_MS;
        openedAt = 0;

        if (wasStable) {
          // A connection that proved itself viable is worth another attempt — reset the
          // instant-failure budget — but not at zero delay (see
          // `STABLE_RECONNECT_BASE_DELAY_MS`, whose own doc covers why no further total
          // ceiling is needed on top of it).
          unstableRetries = 0;
          markGapOnNextOpen = true;
          retryTimer = setTimeout(() => openStream(id), stableReconnectDelayMs());
          return;
        }

        unstableRetries += 1;
        if (unstableRetries > MAX_UNSTABLE_RETRIES) {
          // A run that fails instantly keeps failing instantly — a finished run's empty
          // stream, or a subscriber-capped run's error response. Stop rather than loop.
          giveUp();
          return;
        }

        retryTimer = setTimeout(() => openStream(id), backoffMs(unstableRetries - 1));
      };
    }

    function tryResolve() {
      resolveRunId(targetRunIdOrName)
        .then((id) => {
          if (!cancelled) {
            openStream(id);
          }
        })
        .catch((error: unknown) => {
          if (cancelled) return;

          const message = error instanceof Error ? error.message : '';
          if (message.startsWith(NOT_FOUND_MESSAGE_PREFIX)) {
            // Genuinely no such run — never invent a connection for one that doesn't
            // exist. This is the only resolve rejection that's terminal by itself.
            giveUp();
            return;
          }

          // Every other rejection (a transient 5xx or a network failure on the
          // `GET /runs?$filter=...` lookup `resolveRunId` performs for a non-uuid name)
          // is retried, bounded the same way a stream failure is — one hiccup on this
          // lookup should not permanently disable the stream.
          resolveRetries += 1;
          if (resolveRetries > MAX_UNSTABLE_RETRIES) {
            giveUp();
            return;
          }
          retryTimer = setTimeout(tryResolve, backoffMs(resolveRetries - 1));
        });
    }

    tryResolve();

    return () => {
      cancelled = true;
      if (retryTimer) clearTimeout(retryTimer);
      if (flushHandle !== null) clearTimeout(flushHandle);
      pending = [];
      es?.close();
    };
  }, [runIdOrName]);

  return { isConnected, messages, hasGivenUp };
}
