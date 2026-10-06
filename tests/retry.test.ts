import { describe, expect, test } from 'bun:test';
import { fetchWithRetry, isRetryableStatus } from '../src/retry.ts';

function responder(statuses: (number | Error)[]): { fetchFn: typeof fetch; calls: () => number } {
  let n = 0;
  const fetchFn = (async (_url: string, _init?: RequestInit) => {
    const next = statuses[Math.min(n, statuses.length - 1)];
    n++;
    if (next instanceof Error) throw next;
    return new Response('body', { status: next });
  }) as typeof fetch;
  return { fetchFn, calls: () => n };
}

describe('fetchWithRetry', () => {
  test('retries 500s then succeeds', async () => {
    const { fetchFn, calls } = responder([500, 500, 200]);
    const res = await fetchWithRetry(fetchFn, 'http://x/', {}, 3, 1);
    expect(res.ok).toBe(true);
    expect(calls()).toBe(3);
  });

  test('does not retry 403', async () => {
    const { fetchFn, calls } = responder([403]);
    const res = await fetchWithRetry(fetchFn, 'http://x/', {}, 3, 1);
    expect(res.status).toBe(403);
    expect(calls()).toBe(1);
  });

  test('retries network errors then succeeds', async () => {
    const { fetchFn, calls } = responder([new TypeError('down'), new TypeError('down'), 200]);
    const res = await fetchWithRetry(fetchFn, 'http://x/', {}, 3, 1);
    expect(res.ok).toBe(true);
    expect(calls()).toBe(3);
  });

  test('returns the last retryable status after exhausting attempts', async () => {
    const { fetchFn, calls } = responder([503]);
    const res = await fetchWithRetry(fetchFn, 'http://x/', {}, 3, 1);
    expect(res.status).toBe(503);
    expect(calls()).toBe(3);
  });

  test('does not retry aborted requests', async () => {
    const { fetchFn, calls } = responder([new DOMException('aborted', 'AbortError')]);
    const controller = new AbortController();
    controller.abort();
    await expect(
      fetchWithRetry(fetchFn, 'http://x/', { signal: controller.signal }, 3, 1),
    ).rejects.toThrow();
    expect(calls()).toBe(1);
  });

  test('classifies statuses', () => {
    expect(isRetryableStatus(429)).toBe(true);
    expect(isRetryableStatus(503)).toBe(true);
    expect(isRetryableStatus(403)).toBe(false);
    expect(isRetryableStatus(404)).toBe(false);
  });
});
