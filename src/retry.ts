import type { HotaudioFetch } from './types.ts';

/** Total attempts per request (initial + retries). */
export const FETCH_RETRY_ATTEMPTS = 3;
/** Base backoff between retries; doubled per attempt with jitter. */
export const FETCH_RETRY_BASE_DELAY_MS = 500;
/** Upper bound for a single retry wait (caps Retry-After). */
export const FETCH_RETRY_MAX_DELAY_MS = 10000;

const RETRYABLE_STATUS = new Set([408, 425, 429, 500, 502, 503, 504]);

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
 * Fetch with retries for transient failures: network errors and
 * retryable statuses (429/5xx). Other statuses return as-is. Aborted
 * requests (caller `signal`) are never retried.
 */
export async function fetchWithRetry(
  fetchFn: HotaudioFetch,
  url: string,
  init: RequestInit = {},
  attempts: number = FETCH_RETRY_ATTEMPTS,
  baseDelayMs: number = FETCH_RETRY_BASE_DELAY_MS,
): Promise<Response> {
  let lastErr: unknown = null;
  for (let attempt = 1; attempt <= Math.max(1, attempts); attempt++) {
    let res: Response;
    try {
      res = await fetchFn(url, init);
    } catch (err) {
      // Never retry a caller-aborted request.
      if (init.signal?.aborted || attempt >= attempts) throw err;
      lastErr = err;
      await sleep(backoffMs(baseDelayMs, attempt));
      continue;
    }
    if (res.ok || !isRetryableStatus(res.status)) return res;
    try {
      await res.arrayBuffer();
    } catch {
      // Body drain is best-effort (keeps pooled connections reusable).
    }
    lastErr = new Error(`Request failed with HTTP ${res.status}`);
    if (attempt >= attempts) return res;
    await sleep(backoffMs(baseDelayMs, attempt, res));
  }
  throw lastErr instanceof Error ? lastErr : new Error('Request failed');
}
