// API client with base URL configuration and fetch wrapper

import type { components } from './generated/openapi';
import { notifyUnauthorized, resetAuthAttempts } from '@/auth/tokenState';

export const API_BASE_URL = import.meta.env.VITE_API_URL || '/qa/v1';

export class ApiError extends Error {
  constructor(
    public status: number,
    public statusText: string,
    message: string
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

// Set by src/auth in Phase C -- `AuthProvider` passes `getAccessToken` from
// `@/auth/tokenState`, which reads a module variable, so this file never sees
// browser storage and never sees React. Until it is set it stays null and no
// header is sent, which is what lets Phases A and B run with
// `auth_disabled: true`.
let tokenProvider: (() => string | null) | null = null;

export function setTokenProvider(fn: () => string | null): void {
  tokenProvider = fn;
}

/**
 * The bearer token this client would send right now, or `null` when there is
 * none -- a deployment running with auth off, or a user not yet signed in.
 *
 * Exported for exactly one caller: `src/hooks/useRunLogStream.ts`. `EventSource`
 * cannot send request headers, so the SSE log stream has to carry the credential
 * in its URL rather than through `authHeaders()` below. Reading it through this
 * function instead of importing `@/auth/tokenState` directly keeps
 * `setTokenProvider` the single seam where `api` and `auth` meet, so the hook
 * stays as ignorant of the auth stack as the rest of this file is.
 */
export function currentAccessToken(): string | null {
  return tokenProvider?.() ?? null;
}

function authHeaders(): Record<string, string> {
  const token = currentAccessToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

/** The gears' canonical error envelope, generated as `components['schemas']['Problem']`
 *  (RFC 9457 `application/problem+json`) rather than hand-typed, so a gear renaming `detail`
 *  is a compile error here instead of a silent fallback to raw-JSON messages. `Partial`
 *  because a parsed response body is never guaranteed to match its declared schema at
 *  runtime — this keeps that honesty without giving up the compile-time check. Verified live:
 *  `GET /qa/v1/runs/00000000-0000-0000-0000-000000000000` → 404,
 *  `content-type: application/problem+json`, `detail` carrying the human-readable message
 *  (`title` is the short one). */
type Problem = Partial<components['schemas']['Problem']>;

/**
 * The 401 path, which is the whole of this file's involvement in authentication
 * beyond adding the header.
 *
 * It **awaits** the rule rather than firing it off, and that is load-bearing:
 * awaiting serialises a burst of simultaneous 401s behind one refresh instead
 * of letting each race its own. It never retries the request — a 401 is not a
 * transient error, and making this retry is exactly how one dead token becomes
 * a request storm (see `@/auth/tokenState`, and the `retry` rule in
 * `src/App.tsx`). A successful refresh fixes the *next* request; this one still
 * throws.
 *
 * `notifyUnauthorized` is a no-op when no `AuthProvider` is mounted, so this
 * changes nothing for a deployment running with auth off.
 */
async function handleResponse<T>(response: Response): Promise<T> {
  if (!response.ok) {
    if (response.status === 401) {
      await notifyUnauthorized();
    }
    const text = await response.text();
    const contentType = response.headers.get('content-type');

    // Branch 1: the canonical envelope above. Unwrap `detail` into `ApiError.message` instead
    // of surfacing the raw JSON.
    if (contentType && contentType.includes('application/problem+json')) {
      try {
        const problem = JSON.parse(text) as Problem;
        if (problem && typeof problem.detail === 'string' && problem.detail) {
          throw new ApiError(response.status, response.statusText, problem.detail);
        }
      } catch (e) {
        if (e instanceof ApiError) {
          throw e;
        }
        // Malformed problem+json body — fall through to the plain-text branch below rather
        // than throw a JSON parse error.
      }
    }

    // Branch 2 (plain-text fallback): every non-envelope error body. This covers two live
    // cases, not one: axum's bare deserialization-error text, which carries no envelope and no
    // `problem+json` content-type at all — e.g. `POST /qa/v1/runs` with a malformed body
    // answers `Failed to deserialize the JSON body into the target type: missing field
    // \`target\`…` as `text/plain` — and an **empty** body, e.g. an unrouted path answering
    // `404` with `content-length: 0`. The `text ||` guard is what keeps the empty case from
    // rendering as `undefined`, `null` or `""`.
    throw new ApiError(
      response.status,
      response.statusText,
      text || `HTTP ${response.status}: ${response.statusText}`
    );
  }

  // A working session, demonstrated rather than assumed: the next 401 gets its
  // own refresh attempt instead of an immediate redirect. This is the "once a
  // request has succeeded" half of the rule in `@/auth/tokenState`.
  resetAuthAttempts();

  const contentType = response.headers.get('content-type');
  if (contentType && contentType.includes('application/json')) {
    return response.json();
  }

  return response.text() as Promise<T>;
}

type RequestHeaders = Record<string, string>;

function withHeaders(base: RequestHeaders, headers?: RequestHeaders): RequestHeaders {
  if (!headers) {
    return base;
  }
  return { ...base, ...headers };
}

export async function apiGet<T>(path: string, headers?: RequestHeaders): Promise<T> {
  const response = await fetch(`${API_BASE_URL}${path}`, {
    method: 'GET',
    headers: withHeaders({
      'Accept': 'application/json',
      ...authHeaders(),
    }, headers),
  });
  return handleResponse<T>(response);
}

export async function apiPost<T>(path: string, body?: unknown, headers?: RequestHeaders): Promise<T> {
  const response = await fetch(`${API_BASE_URL}${path}`, {
    method: 'POST',
    headers: withHeaders({
      'Content-Type': 'application/json',
      'Accept': 'application/json',
      ...authHeaders(),
    }, headers),
    body: body ? JSON.stringify(body) : undefined,
  });
  return handleResponse<T>(response);
}

export async function apiPostFormData<T>(path: string, formData: FormData, headers?: RequestHeaders): Promise<T> {
  const response = await fetch(`${API_BASE_URL}${path}`, {
    method: 'POST',
    headers: withHeaders({
      'Accept': 'application/json',
      ...authHeaders(),
    }, headers),
    body: formData,
  });
  return handleResponse<T>(response);
}

export async function apiPut<T>(path: string, body?: unknown, headers?: RequestHeaders): Promise<T> {
  const response = await fetch(`${API_BASE_URL}${path}`, {
    method: 'PUT',
    headers: withHeaders({
      'Content-Type': 'application/json',
      'Accept': 'application/json',
      ...authHeaders(),
    }, headers),
    body: body ? JSON.stringify(body) : undefined,
  });
  return handleResponse<T>(response);
}

export async function apiDelete<T>(path: string, headers?: RequestHeaders): Promise<T> {
  const response = await fetch(`${API_BASE_URL}${path}`, {
    method: 'DELETE',
    headers: withHeaders({
      'Accept': 'application/json',
      ...authHeaders(),
    }, headers),
  });
  return handleResponse<T>(response);
}

/**
 * A `GET` that answers a binary body rather than JSON — today only the analytics export,
 * which returns a CSV or JSON *file*.
 *
 * It exists so that call site stops bypassing this module. Before Task 10,
 * `exportAnalytics` used a raw `fetch('/api/analytics/export?…')`: it hardcoded the old
 * `/api` prefix, so re-basing `API_BASE_URL` onto `/qa/v1` did not reach it, and it sat
 * outside the single `Authorization` injection point above — which would have made it the
 * one request in the app that silently went out unauthenticated once Phase C turns auth
 * on.
 */
export async function apiGetBlob(path: string, headers?: RequestHeaders): Promise<Blob> {
  const response = await fetch(`${API_BASE_URL}${path}`, {
    method: 'GET',
    headers: withHeaders({ ...authHeaders() }, headers),
  });
  // Same 401 rule as `handleResponse` -- this function has its own error branch
  // because its success path returns a Blob rather than JSON, and a 401 that
  // only the JSON path noticed would be a hole in the single injection point
  // this function exists to close.
  if (!response.ok) {
    if (response.status === 401) {
      await notifyUnauthorized();
    }
    const text = await response.text();
    throw new ApiError(
      response.status,
      response.statusText,
      text || `HTTP ${response.status}: ${response.statusText}`
    );
  }
  resetAuthAttempts();
  return response.blob();
}

/** `PATCH`. Added for `PATCH /qa/v1/environments/{id}`, which is what legacy's
 *  `PUT /platforms/{name}` and its dedicated `POST .../rename` verb both became
 *  (CONTRACT-DIFF rows 53, 56) — a true partial update, so an omitted key means "leave
 *  this field alone" rather than "clear it". */
export async function apiPatch<T>(path: string, body?: unknown, headers?: RequestHeaders): Promise<T> {
  const response = await fetch(`${API_BASE_URL}${path}`, {
    method: 'PATCH',
    headers: withHeaders({
      'Content-Type': 'application/json',
      'Accept': 'application/json',
      ...authHeaders(),
    }, headers),
    body: body ? JSON.stringify(body) : undefined,
  });
  return handleResponse<T>(response);
}
