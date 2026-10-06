import { describe, expect, test } from 'bun:test';
import { downloadHaxBuffer, parseSavedKeys } from '../src/download.ts';
import { fetchHotaudioTracks, haxUrlForTrackKey, listHotaudioTracks, loadHandshakeFromHtml } from '../src/listen.ts';
import { API_BASE, HAX_URL, PAGE_URL, buildFixture } from './fixture.ts';

describe('parseSavedKeys', () => {
  test('accepts a bare key map', () => {
    const parsed = parseSavedKeys({ '1': 'ab'.repeat(32) });
    expect(parsed.keys).toEqual({ '1': 'ab'.repeat(32) });
    expect(parsed.haxUrl).toBeUndefined();
  });

  test('accepts an envelope with metadata', () => {
    const parsed = parseSavedKeys(
      JSON.stringify({
        version: 1,
        pageUrl: 'https://hotaudio.net/u/a/b',
        haxUrl: 'https://cdn.hotaudio.net/a/x.hax',
        title: 'T',
        savedAt: new Date().toISOString(),
        keys: { '16': 'cd'.repeat(32) },
      }),
    );
    expect(parsed.keys).toEqual({ '16': 'cd'.repeat(32) });
    expect(parsed.haxUrl).toBe('https://cdn.hotaudio.net/a/x.hax');
    expect(parsed.pageUrl).toBe('https://hotaudio.net/u/a/b');
  });

  test('rejects invalid maps', () => {
    expect(() => parseSavedKeys({})).toThrow(/Invalid keys/);
    expect(() => parseSavedKeys({ '1': 'zz' })).toThrow();
    expect(() => parseSavedKeys({ '1': 'abc' })).toThrow();
    expect(() => parseSavedKeys({ keys: {} })).toThrow();
    expect(() => parseSavedKeys('not json{')).toThrow();
  });
});

describe('downloadHaxBuffer (mocked network)', () => {
  test('fetches the container and decrypts with saved keys, no handshake', async () => {
    const { fetchFn, expected, rootHex } = await buildFixture();
    const res = await downloadHaxBuffer(HAX_URL, { '1': rootHex }, { fetchFn });
    expect(res.buffer).toEqual(expected);
    expect(res.mime).toBe('audio/mp4');
  });
});

describe('loadHandshakeFromHtml track selection', () => {
  test('selects the requested track and rejects unknown ids', async () => {
    const { fetchFn } = await buildFixture();
    const pageRes = await fetchFn('https://mock.test/u/a/b');
    const html = await pageRes.text();
    const selected = await loadHandshakeFromHtml(html, 'https://mock.test', '7');
    expect(selected?.tid).toBe('7');
    expect(await loadHandshakeFromHtml(html, 'https://mock.test', 'nope')).toBeNull();
    expect((await loadHandshakeFromHtml(html, 'https://mock.test'))?.tid).toBe('7');
  });
});

describe('track listing', () => {
  test('lists tracks in page order', async () => {
    const { pageHtml } = await buildFixture();
    expect(listHotaudioTracks(pageHtml)).toEqual([{ id: '7', key: 'mock-track-key', title: 'Mock Track' }]);
    expect(listHotaudioTracks('<html></html>')).toBeNull();
  });

  test('fetches and lists tracks', async () => {
    const { fetchFn } = await buildFixture();
    const tracks = await fetchHotaudioTracks(PAGE_URL, { fetchFn, apiBase: API_BASE });
    expect(tracks?.map((t) => t.id)).toEqual(['7']);
  });

  test('derives the container URL from a track key', () => {
    expect(haxUrlForTrackKey('abc123')).toBe('https://cdn.hotaudio.net/a/abc123.hax');
  });
});
