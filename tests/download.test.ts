import { describe, expect, test } from 'bun:test';
import { chacha20poly1305 } from '@noble/ciphers/chacha.js';
import { x25519 } from '@noble/curves/ed25519.js';
import { createHash, randomBytes } from 'node:crypto';
import { bytesToHex, hexToBytes, sha256 } from '../src/crypto.ts';
import { deriveSegmentKey } from '../src/hax_decoder.ts';
import { downloadHotaudioBuffer } from '../src/download.ts';

const PAGE_URL = 'https://mock.test/u/a/b';
const API_BASE = 'https://mock.test';
const HAX_URL = 'https://mock.test/audio.hax';
const SEGMENTS = 10;

function concat(...parts: (string | Uint8Array)[]): Uint8Array {
  const enc = parts.map((p) => (typeof p === 'string' ? new TextEncoder().encode(p) : p));
  const out = new Uint8Array(enc.reduce((n, p) => n + p.length, 0));
  let off = 0;
  for (const p of enc) {
    out.set(p, off);
    off += p.length;
  }
  return out;
}
const encStr = (s: string) => {
  const b = new TextEncoder().encode(s);
  return concat(`${b.length}:`, b);
};
const sha256Sync = (d: Uint8Array) => new Uint8Array(createHash('sha256').update(d).digest());

interface Fixture {
  fetchFn: typeof fetch;
  listenCalls: () => number;
  expected: Uint8Array;
}

async function buildFixture(): Promise<Fixture> {
  const serverPriv = x25519.utils.randomSecretKey();
  const serverPubHex = bytesToHex(x25519.getPublicKey(serverPriv));
  const root = new Uint8Array(randomBytes(32));
  // True node keys for a partial first branch covering only segments 0-1
  // (leaves e=33,34 with treeBase=33): chain root -> 2 -> 4 -> 8 -> 16/17 -> 33/34.
  const k2 = sha256Sync(concat(root, new Uint8Array([2])));
  const k4 = sha256Sync(concat(k2, new Uint8Array([4])));
  const k8 = sha256Sync(concat(k4, new Uint8Array([8])));
  const k16 = sha256Sync(concat(k8, new Uint8Array([16])));
  const k17 = sha256Sync(concat(k8, new Uint8Array([17])));
  const k33 = sha256Sync(concat(k16, new Uint8Array([33])));
  const k34 = sha256Sync(concat(k17, new Uint8Array([34])));

  const plains: Uint8Array[] = [];
  for (let i = 0; i < SEGMENTS; i++) {
    if (i === 0) {
      plains.push(concat(new Uint8Array([0, 0, 0, 0x20]), new TextEncoder().encode('ftypM4A '), new Uint8Array(20).fill(0xab)));
    } else {
      const marker = new TextEncoder().encode(`segment-${i}`);
      const p = new Uint8Array(64).fill(0xcd);
      p.set(marker, 0);
      plains.push(p);
    }
  }
  const ciphers: Uint8Array[] = [];
  for (let i = 0; i < SEGMENTS; i++) {
    const key = await deriveSegmentKey({ 1: root }, SEGMENTS, i);
    ciphers.push(chacha20poly1305(key, new Uint8Array(12)).encrypt(plains[i]));
  }

  const table = new Uint8Array(SEGMENTS * 8);
  const encodeMeta = () =>
    concat(
      'd',
      encStr('baseKey'),
      concat('32:', new Uint8Array(32)),
      encStr('codec'),
      encStr('mp4a.40.2'),
      encStr('durationMs'),
      new TextEncoder().encode('i10000e'),
      encStr('segmentCount'),
      new TextEncoder().encode(`i${SEGMENTS}e`),
      encStr('segments'),
      concat(`${table.length}:`, table),
      'e',
    );
  const headerLength = 16 + encodeMeta().length;
  let cursor = headerLength;
  {
    const v = new DataView(table.buffer);
    for (let i = 0; i < SEGMENTS; i++) {
      v.setUint32(i * 8, cursor, true);
      v.setUint32(i * 8 + 4, i * 1000, true);
      cursor += ciphers[i].length;
    }
  }
  const fileLength = cursor;
  const haxBytes = new Uint8Array(fileLength);
  haxBytes.set(new TextEncoder().encode('HAX0'), 0);
  new DataView(haxBytes.buffer).setUint32(4, fileLength, true);
  new DataView(haxBytes.buffer).setUint32(8, headerLength, true);
  new DataView(haxBytes.buffer).setUint32(12, 0, true);
  haxBytes.set(encodeMeta(), 16);
  {
    let off = headerLength;
    for (const c of ciphers) {
      haxBytes.set(c, off);
      off += c.length;
    }
  }

  const state = {
    pid: 'mock-pid',
    tick: 'mock-tick',
    key: serverPubHex,
    tracks: { '7': { key: 'mock-track-key', title: 'Mock Track' } },
    order: [7],
  };
  const stateKey = new Uint8Array(randomBytes(32));
  const stateCt = chacha20poly1305(stateKey, new Uint8Array(12)).encrypt(
    new TextEncoder().encode(JSON.stringify(state)),
  );
  const pageHtml = `<!doctype html><html><head><script>var __ha_state = "${Buffer.from(concat(stateCt, stateKey)).toString('base64')}";</script></head></html>`;

  let listenCount = 0;
  const fetchFn = (async (input: unknown, init?: RequestInit) => {
    const url = String((input as { url?: unknown })?.url ?? input);
    if (url === PAGE_URL) {
      return new Response(pageHtml, { status: 200, headers: { 'Content-Type': 'text/html' } });
    }
    if (url === `${API_BASE}/api/v1/audio/listen`) {
      listenCount++;
      const headers = new Headers(init?.headers);
      const clientPubHex = headers.get('X-Key') ?? '';
      const sig = headers.get('X-Signature') ?? '';
      const shared = x25519.getSharedSecret(serverPriv, hexToBytes(clientPubHex));
      const Ee = await sha256(shared);
      const reqNonce = (await sha256(new TextEncoder().encode(sig))).subarray(0, 12);
      const rawBody = init?.body;
      const body =
        rawBody instanceof Uint8Array
          ? rawBody
          : new Uint8Array(await (rawBody as Blob).arrayBuffer());
      const payload = JSON.parse(
        new TextDecoder().decode(chacha20poly1305(Ee, reqNonce).decrypt(body)),
      ) as { first: number };
      const resp = {
        url: payload.first === -1 ? HAX_URL : '',
        length15s: 0,
        keys:
          payload.first === -1
            ? { '33': bytesToHex(k33), '34': bytesToHex(k34) }
            : { '1': bytesToHex(root) },
      };
      const respNonce = new Uint8Array(reqNonce);
      respNonce[0] = (respNonce[0] + 1) & 0xff;
      const enc = chacha20poly1305(Ee, respNonce).encrypt(new TextEncoder().encode(JSON.stringify(resp)));
      return new Response(enc, { status: 200 });
    }
    if (url === HAX_URL) return new Response(haxBytes, { status: 200 });
    return new Response('not found', { status: 404 });
  }) as typeof fetch;

  const expected = new Uint8Array(plains.reduce((n, p) => n + p.length, 0));
  {
    let off = 0;
    for (const p of plains) {
      expected.set(p, off);
      off += p.length;
    }
  }
  return { fetchFn, listenCalls: () => listenCount, expected };
}

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
});
