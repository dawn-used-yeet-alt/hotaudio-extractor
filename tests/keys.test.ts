import { describe, expect, test } from 'bun:test';
import { downloadHaxBuffer, parseSavedKeys } from '../src/download.ts';
import { loadHandshakeFromHtml } from '../src/listen.ts';
import { HAX_URL, buildFixture } from './fixture.ts';

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
