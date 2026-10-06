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
    const res = await fetchWithRetry(fetchFn, 'http://x/', {}, { attempts: 3, baseDelayMs: 1 });
    expect(res.ok).toBe(true);
    expect(calls()).toBe(3);
  });

  test('does not retry 403', async () => {
    const { fetchFn, calls } = responder([403]);
    const res = await fetchWithRetry(fetchFn, 'http://x/', {}, { attempts: 3, baseDelayMs: 1 });
    expect(res.status).toBe(403);
    expect(calls()).toBe(1);
  });

  test('retries network errors then succeeds', async () => {
    const { fetchFn, calls } = responder([new TypeError('down'), new TypeError('down'), 200]);
    const res = await fetchWithRetry(fetchFn, 'http://x/', {}, { attempts: 3, baseDelayMs: 1 });
    expect(res.ok).toBe(true);
    expect(calls()).toBe(3);
  });

  test('returns the last retryable status after exhausting attempts', async () => {
    const { fetchFn, calls } = responder([503]);
    const res = await fetchWithRetry(fetchFn, 'http://x/', {}, { attempts: 3, baseDelayMs: 1 });
    expect(res.status).toBe(503);
    expect(calls()).toBe(3);
  });

  test('does not retry aborted requests', async () => {
    const { fetchFn, calls } = responder([new DOMException('aborted', 'AbortError')]);
    const controller = new AbortController();
    controller.abort();
    await expect(
      fetchWithRetry(fetchFn, 'http://x/', { signal: controller.signal }, { attempts: 3, baseDelayMs: 1 }),
    ).rejects.toThrow();
    expect(calls()).toBe(1);
  });

  test('times out a hanging request and retries', async () => {
    let n = 0;
    const fetchFn = ((_url: string, init?: RequestInit) => {
      n++;
      if (n === 1) {
        return new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener('abort', () => reject(new DOMException('timed out', 'TimeoutError')));
        });
      }
      return Promise.resolve(new Response('ok', { status: 200 }));
    }) as typeof fetch;
    const res = await fetchWithRetry(fetchFn, 'http://x/', {}, { attempts: 2, baseDelayMs: 1, timeoutMs: 10 });
    expect(res.ok).toBe(true);
    expect(n).toBe(2);
  });

  test('caller abort during a hanging request is not retried', async () => {
    let n = 0;
    const controller = new AbortController();
    const fetchFn = ((_url: string, init?: RequestInit) => {
      n++;
      return new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
      });
    }) as typeof fetch;
    setTimeout(() => controller.abort(), 5);
    await expect(
      fetchWithRetry(fetchFn, 'http://x/', { signal: controller.signal }, { attempts: 3, baseDelayMs: 1, timeoutMs: 5000 }),
    ).rejects.toThrow();
    expect(n).toBe(1);
  });

  test('classifies statuses', () => {
    expect(isRetryableStatus(401)).toBe(true);
    expect(isRetryableStatus(429)).toBe(true);
    expect(isRetryableStatus(503)).toBe(true);
    expect(isRetryableStatus(403)).toBe(false);
    expect(isRetryableStatus(404)).toBe(false);
  });
});
