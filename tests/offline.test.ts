import { describe, expect, test } from 'bun:test';
import { chacha20poly1305 } from '@noble/ciphers/chacha.js';
import {
  base64ToBytes,
  bytesToHex,
  hexToBytes,
} from '../src/crypto.ts';
import {
  decodeBencode,
  decryptSegmentSlice,
  deriveSegmentKey,
  parseHax0Header,
} from '../src/hax_decoder.ts';
import { extractHaState, mergeBranchKeys } from '../src/listen.ts';
import { HOTAUDIO_PATTERN, isHotaudioUrl } from '../src/index.ts';

function bencodeStr(s: string): Uint8Array {
  const b = new TextEncoder().encode(s);
  return concat(`${b.length}:`, b);
}

function bencodeInt(n: number): Uint8Array {
  return new TextEncoder().encode(`i${n}e`);
}

function bencodeBytes(b: Uint8Array): Uint8Array {
  return concat(`${b.length}:`, b);
}

function concat(...parts: (string | Uint8Array)[]): Uint8Array {
  const enc = parts.map((p) => (typeof p === 'string' ? new TextEncoder().encode(p) : p));
  const total = enc.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of enc) {
    out.set(p, off);
    off += p.length;
  }
  return out;
}

function buildHax0(segmentCount: number): Uint8Array {
  const segments = new Uint8Array(segmentCount * 8);
  const view = new DataView(segments.buffer);
  for (let i = 0; i < segmentCount; i++) {
    view.setUint32(i * 8, 16 + 64 + i * 100, true);
    view.setUint32(i * 8 + 4, i * 1000, true);
  }
  const meta = concat(
    'd',
    bencodeStr('baseKey'),
    bencodeBytes(new Uint8Array(32).fill(7)),
    bencodeStr('codec'),
    bencodeStr('mp4a.40.2'),
    bencodeStr('durationMs'),
    bencodeInt(90000),
    bencodeStr('segmentCount'),
    bencodeInt(segmentCount),
    bencodeStr('segments'),
    bencodeBytes(segments),
    'e',
  );
  const headerLength = 16 + meta.length;
  const fileLength = headerLength + segmentCount * 100;
  const out = new Uint8Array(fileLength);
  out.set(new TextEncoder().encode('HAX0'), 0);
  new DataView(out.buffer).setUint32(4, fileLength, true);
  new DataView(out.buffer).setUint32(8, headerLength, true);
  new DataView(out.buffer).setUint32(12, 0, true);
  out.set(meta, 16);
  return out;
}

describe('hex utils', () => {
  test('hex round-trips', () => {
    const bytes = new Uint8Array([0, 1, 254, 255]);
    expect(hexToBytes(bytesToHex(bytes))).toEqual(bytes);
  });

  test('base64 round-trips', () => {
    const original = new Uint8Array([1, 2, 3, 250]);
    const b64 = Buffer.from(original).toString('base64');
    expect(base64ToBytes(b64)).toEqual(original);
  });
});

describe('bencode', () => {
  test('decodes integers', () => {
    expect(decodeBencode(new TextEncoder().encode('i42e'), 0).value).toBe(42);
  });

  test('decodes byte strings', () => {
    const { value } = decodeBencode(new TextEncoder().encode('3:abc'), 0);
    expect(new TextDecoder().decode(value as Uint8Array)).toBe('abc');
  });

  test('decodes dicts', () => {
    const buf = concat('d', bencodeStr('a'), bencodeInt(1), 'e');
    expect(decodeBencode(buf, 0).value).toEqual({ a: 1 });
  });

  test('rejects unknown tokens', () => {
    expect(() => decodeBencode(new Uint8Array([0x6c]), 0)).toThrow();
  });
});

describe('hax0', () => {
  test('parses a synthetic header', () => {
    const hax = parseHax0Header(buildHax0(3));
    expect(hax.segmentCount).toBe(3);
    expect(hax.codec).toBe('mp4a.40.2');
    expect(hax.durationMs).toBe(90000);
    expect(hax.segments).toHaveLength(3);
  });

  test('rejects bad magic', () => {
    const bad = buildHax0(1);
    bad.set(new TextEncoder().encode('XXXX'), 0);
    expect(() => parseHax0Header(bad)).toThrow(/magic/);
  });

  test('segment slice decrypt inverts chacha encrypt', () => {
    const key = new Uint8Array(32).fill(9);
    const plain = new TextEncoder().encode('hello-audio');
    const ct = chacha20poly1305(key, new Uint8Array(12)).encrypt(plain);
    expect(decryptSegmentSlice(ct, key)).toEqual(plain);
  });

  test('deriveSegmentKey throws on missing keys and derives from root', async () => {
    await expect(deriveSegmentKey({}, 4, 0)).rejects.toThrow(/Key missing/);
    const root = new Uint8Array(32).fill(3);
    const k0 = await deriveSegmentKey({ 1: root }, 4, 0, new Map());
    const k0b = await deriveSegmentKey({ 1: root }, 4, 0, new Map());
    expect(k0).toEqual(k0b);
    expect(k0).toHaveLength(32);
  });
});

describe('handshake helpers', () => {
  test('extractHaState finds the embedded payload', () => {
    expect(extractHaState('<script>var __ha_state = "abc123"</script>')).toBe('abc123');
    expect(extractHaState('<html></html>')).toBeNull();
  });

  test('mergeBranchKeys counts new keys', () => {
    const map: Record<number, Uint8Array> = { 1: new Uint8Array([1]) };
    const added = mergeBranchKeys(map, { 1: 'aa', 2: 'bb' }, hexToBytes);
    expect(added).toBe(1);
    expect(map[2]).toEqual(hexToBytes('bb'));
  });
});

describe('url detection', () => {
  test('matches share links', () => {
    expect(isHotaudioUrl('https://hotaudio.net/u/someuser/some-track')).toBe(true);
    expect(HOTAUDIO_PATTERN.test('https://hotaudio.net/u/a/b?x=1')).toBe(true);
  });

  test('rejects other hosts', () => {
    expect(isHotaudioUrl('https://example.com/u/a/b')).toBe(false);
  });
});
