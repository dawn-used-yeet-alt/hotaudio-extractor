import type { HotaudioFetch } from './types.ts';

/** Total attempts per request (initial + retries). */
export const FETCH_RETRY_ATTEMPTS = 3;
/** Base backoff between retries; doubled per attempt with jitter. */
export const FETCH_RETRY_BASE_DELAY_MS = 500;
/** Upper bound for a single retry wait (caps Retry-After). */
export const FETCH_RETRY_MAX_DELAY_MS = 10000;
/** Default per-attempt timeout for small API calls (page, listen, ranges). */
export const FETCH_API_TIMEOUT_MS = 30000;

const RETRYABLE_STATUS = new Set([401, 408, 425, 429, 500, 502, 503, 504]);

export interface FetchRetryOptions {
  attempts?: number;
  baseDelayMs?: number;
  /** Per-attempt timeout in ms. Undefined = no timeout (bulk transfers). */
  timeoutMs?: number;
}

/** True for transient HTTP statuses worth retrying (429/5xx class). */
export function isRetryableStatus(status: number): boolean {
  return RETRYABLE_STATUS.has(status);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function backoffMs(baseDelayMs: number, attempt: number, res?: Response): number {
  if (res?.status === 429) {
    const after = res.headers.get('Retry-After');
    if (after !== null) {
      const secs = Number(after);
      const ms = Number.isFinite(secs)
        ? secs * 1000
        : Math.max(0, Date.parse(after) - Date.now());
      if (Number.isFinite(ms) && ms >= 0) return Math.min(ms, FETCH_RETRY_MAX_DELAY_MS);
    }
  }
  return Math.min(baseDelayMs * 2 ** (attempt - 1) + Math.random() * 100, FETCH_RETRY_MAX_DELAY_MS);
}

/**
 * Combine a caller signal with a timeout into one signal. Caller aborts
 * and timeouts both abort the combined signal; cleanup clears the timer.
 */
function withTimeout(
  callerSignal: AbortSignal | null | undefined,
  timeoutMs: number | undefined,
): { signal: AbortSignal | undefined; cleanup: () => void } {
  if (timeoutMs === undefined) return { signal: callerSignal ?? undefined, cleanup: () => {} };
  const ctrl = new AbortController();
  let onAbort: (() => void) | undefined;
  if (callerSignal) {
    if (callerSignal.aborted) {
      ctrl.abort(callerSignal.reason);
    } else {
      onAbort = () => ctrl.abort(callerSignal.reason);
      callerSignal.addEventListener('abort', onAbort, { once: true });
    }
  }
  const timer = setTimeout(() => {
    ctrl.abort(new DOMException('Request timed out', 'TimeoutError'));
  }, timeoutMs);
  (timer as unknown as { unref?: () => void }).unref?.();
  return {
    signal: ctrl.signal,
    cleanup: () => {
      clearTimeout(timer);
      if (callerSignal && onAbort) callerSignal.removeEventListener('abort', onAbort);
    },
  };
}

/**
 * Fetch with retries for transient failures: network errors, timeouts,
 * and retryable statuses (429/5xx). Other statuses return as-is.
 * Caller-aborted requests are never retried.
 */
export async function fetchWithRetry(
  fetchFn: HotaudioFetch,
  url: string,
  init: RequestInit = {},
  opts: FetchRetryOptions = {},
): Promise<Response> {
  const attempts = Math.max(1, opts.attempts ?? FETCH_RETRY_ATTEMPTS);
  const baseDelayMs = opts.baseDelayMs ?? FETCH_RETRY_BASE_DELAY_MS;
  const callerSignal = init.signal;
  let lastErr: unknown = null;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    const { signal, cleanup } = withTimeout(callerSignal, opts.timeoutMs);
    try {
      const res = await fetchFn(url, signal ? { ...init, signal } : init);
      cleanup();
      if (res.ok || !isRetryableStatus(res.status)) return res;
      lastErr = new Error(`Request failed with HTTP ${res.status}`);
      if (attempt >= attempts) return res;
      try {
        // Release the body without consuming it (only when retrying, so
        // callers can still read error payloads from the final response).
        await res.body?.cancel();
      } catch {
        // Best-effort (keeps pooled connections reusable).
      }
      await sleep(backoffMs(baseDelayMs, attempt, res));
    } catch (err) {
      cleanup();
      if (callerSignal?.aborted || attempt >= attempts) throw err;
      lastErr = err;
      await sleep(backoffMs(baseDelayMs, attempt));
    }
  }
  throw lastErr instanceof Error ? lastErr : new Error('Request failed');
}
