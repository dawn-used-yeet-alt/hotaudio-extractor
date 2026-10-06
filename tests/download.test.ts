import { describe, expect, test } from 'bun:test';
import { downloadHotaudioBuffer } from '../src/download.ts';
import { API_BASE, PAGE_URL, SEGMENTS, buildFixture } from './fixture.ts';

describe('downloadHotaudioBuffer (mocked network)', () => {
  test('decrypts the full track and pages exactly once for missing keys', async () => {
    const { fetchFn, listenCalls, expected } = await buildFixture();
    const res = await downloadHotaudioBuffer(PAGE_URL, { fetchFn, apiBase: API_BASE });
    expect(res.buffer).toEqual(expected);
    expect(res.segmentCount).toBe(SEGMENTS);
    expect(res.title).toBe('Mock Track');
    expect(res.mime).toBe('audio/mp4');
    // Initial branch covers segments 0-1 only; segment 2 forces one page.
    expect(listenCalls()).toBe(2);
    expect(Object.keys(res.keys).sort()).toEqual(['1', '33', '34']);
  });

  test('seeds skip already-known branches', async () => {
    const { fetchFn, listenCalls, expected, rootHex } = await buildFixture();
    // Full-coverage seed: only the initial listen happens, no paging.
    const full = await downloadHotaudioBuffer(PAGE_URL, { fetchFn, apiBase: API_BASE, initialKeys: { '1': rootHex } });
    expect(full.buffer).toEqual(expected);
    expect(listenCalls()).toBe(1);
  });

  test('ignores malformed seeds and pages normally', async () => {
    const { fetchFn, listenCalls, expected } = await buildFixture();
    const res = await downloadHotaudioBuffer(PAGE_URL, {
      fetchFn,
      apiBase: API_BASE,
      initialKeys: { '1': 'zz', '2': 'abc', nope: 'ab'.repeat(32) },
    });
    expect(res.buffer).toEqual(expected);
    expect(listenCalls()).toBe(2);
  });
});
