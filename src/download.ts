import { hexToBytes } from './crypto.ts';
import {
  decryptSegmentSlice,
  deriveSegmentKey,
  parseHax0Header,
} from './hax_decoder.ts';
import {
  listenRequest,
  loadHotaudioHandshake,
  mergeBranchKeys,
  type HotaudioHandshake,
} from './listen.ts';
import { HOTAUDIO_UA } from './constants.ts';
import type {
  HotaudioFetch,
  HotaudioProgressCallback,
} from './types.ts';

export interface DownloadOptions {
  userAgent?: string;
  fetchFn?: HotaudioFetch;
  apiBase?: string;
  onProgress?: HotaudioProgressCallback;
}

export interface HotaudioBufferResult {
  /** Concatenated decrypted MP4 fragments (playable `.m4a` bytes). */
  buffer: Uint8Array;
  title?: string;
  duration?: number;
  mime: string;
  haxUrl: string;
  /** Merged branch keys (hex), suitable for offline reuse. */
  keys: Record<string, string>;
  segmentCount: number;
}

function optsOf(o: DownloadOptions) {
  return {
    userAgent: o.userAgent ?? HOTAUDIO_UA,
    fetchFn: o.fetchFn ?? globalThis.fetch,
    apiBase: o.apiBase,
    onProgress: o.onProgress,
  };
}

function toKeysMap(keys: Record<string, string>): Record<number, Uint8Array> {
  const map: Record<number, Uint8Array> = {};
  for (const [k, v] of Object.entries(keys)) map[parseInt(k, 10)] = hexToBytes(v);
  return map;
}

/**
 * Key-branch paging lookahead.
 *
 * Each listen request costs a signature plus a round trip, so paging one
 * segment at a time dominates wall time on long tracks. On a cache miss the
 * downloader fetches the missing index plus forward strides concurrently.
 * Fanout is intentionally small to avoid stressing the API.
 */
const PREFETCH_FANOUT = 3;
const PREFETCH_STRIDE = 8;

function isMissingKey(err: unknown): boolean {
  return err instanceof Error && err.message.startsWith('Key missing in keys map');
}

/**
 * Decrypt a full `.hax` buffer with previously fetched keys.
 *
 * Offline path: performs no network I/O. `allKeys` must cover every
 * segment (merge several `first:<n>` responses for long tracks).
 */
export async function decryptHaxBuffer(
  haxBytes: Uint8Array,
  allKeys: Record<string, string>,
  onProgress?: HotaudioProgressCallback,
): Promise<{ buffer: Uint8Array; segmentCount: number; duration?: number; mime: string }> {
  const hax = parseHax0Header(haxBytes);
  const keysMap = toKeysMap(allKeys);
  const cache = new Map<number, Uint8Array>();
  const slices: Uint8Array[] = [];
  let total = 0;
  for (let i = 0; i < hax.segmentCount; i++) {
    const nextOff =
      i + 1 < hax.segmentCount ? hax.segments[i + 1].offset : hax.fileLength;
    const slice = haxBytes.subarray(hax.segments[i].offset, nextOff);
    const segKey = await deriveSegmentKey(keysMap, hax.segmentCount, i, cache);
    const plain = decryptSegmentSlice(slice, segKey);
    slices.push(plain);
    total += plain.length;
    if ((i & 15) === 15 || i === hax.segmentCount - 1) {
      onProgress?.({ phase: 'decrypting', loaded: i + 1, total: hax.segmentCount });
    }
  }
  const buffer = new Uint8Array(total);
  let off = 0;
  for (const s of slices) {
    buffer.set(s, off);
    off += s.length;
  }
  return { buffer, segmentCount: hax.segmentCount, duration: hax.durationMs / 1000, mime: 'audio/mp4' };
}

/**
 * Fetch only the first key branch and the `.hax` URL, without audio.
 * Long tracks need further `listenRequest(handshake, segIdx)` calls.
 */
export async function fetchHotaudioKeys(
  pageUrl: string,
  opts: DownloadOptions = {},
): Promise<{ keys: Record<string, string>; url: string; title?: string; handshake: HotaudioHandshake }> {
  const o = optsOf(opts);
  const handshake = await loadHotaudioHandshake(pageUrl, o);
  if (!handshake) throw new Error('Hotaudio page fetch/state decrypt failed');
  const initial = await listenRequest(handshake, -1, o);
  if (!initial.url) throw new Error('Hotaudio listen API returned no .hax url');
  return { keys: initial.keys, url: initial.url, title: handshake.track.title, handshake };
}

/**
 * Full-track download: handshake, listen, `.hax` fetch, decrypt.
 * Works in browsers, Node, Bun, and workers (global fetch required).
 * Long tracks page `first:<segmentIndex>` automatically.
 */
export async function downloadHotaudioBuffer(
  pageUrl: string,
  opts: DownloadOptions = {},
): Promise<HotaudioBufferResult> {
  const o = optsOf(opts);
  o.onProgress?.({ phase: 'resolving', loaded: 0, total: 0 });
  const handshake = await loadHotaudioHandshake(pageUrl, o);
  if (!handshake) throw new Error('Hotaudio page fetch/state decrypt failed');
  return downloadWithHandshake(handshake, opts);
}

/** Full-track download starting from an existing handshake (local HTML, tests). */
export async function downloadWithHandshake(
  handshake: HotaudioHandshake,
  opts: DownloadOptions = {},
): Promise<HotaudioBufferResult> {
  const o = optsOf(opts);
  const initial = await listenRequest(handshake, -1, o);
  if (!initial.url) throw new Error('Hotaudio listen API returned no .hax url');

  const haxRes = await o.fetchFn(initial.url, { headers: { 'User-Agent': o.userAgent } });
  if (!haxRes.ok) throw new Error(`Hotaudio .hax fetch returned ${haxRes.status}`);
  const haxBytes = new Uint8Array(await haxRes.arrayBuffer());
  o.onProgress?.({ phase: 'fetching', loaded: haxBytes.length, total: haxBytes.length });

  const hax = parseHax0Header(haxBytes);
  const keysMap = toKeysMap(initial.keys);
  const cache = new Map<number, Uint8Array>();
  const allKeys: Record<string, string> = { ...initial.keys };

  const slices: Uint8Array[] = [];
  let total = 0;
  for (let i = 0; i < hax.segmentCount; i++) {
    const nextOff =
      i + 1 < hax.segmentCount ? hax.segments[i + 1].offset : hax.fileLength;
    const slice = haxBytes.subarray(hax.segments[i].offset, nextOff);
    let segKey: Uint8Array;
    try {
      segKey = await deriveSegmentKey(keysMap, hax.segmentCount, i, cache);
    } catch (err) {
      if (!isMissingKey(err)) throw err;
      const wanted = [i];
      for (let k = 1; k < PREFETCH_FANOUT; k++) {
        const j = i + k * PREFETCH_STRIDE;
        if (j < hax.segmentCount) wanted.push(j);
      }
      const branches = await Promise.all(wanted.map((j) => listenRequest(handshake, j, o)));
      let merged = 0;
      for (const extra of branches) {
        merged += mergeBranchKeys(keysMap, extra.keys, hexToBytes);
        Object.assign(allKeys, extra.keys);
      }
      if (merged === 0) throw err;
      cache.clear();
      segKey = await deriveSegmentKey(keysMap, hax.segmentCount, i, cache);
    }
    const plain = decryptSegmentSlice(slice, segKey);
    slices.push(plain);
    total += plain.length;
    if ((i & 15) === 15 || i === hax.segmentCount - 1) {
      o.onProgress?.({ phase: 'decrypting', loaded: i + 1, total: hax.segmentCount });
    }
  }

  const buffer = new Uint8Array(total);
  let off = 0;
  for (const s of slices) {
    buffer.set(s, off);
    off += s.length;
  }
  return {
    buffer,
    title: handshake.track.title,
    duration: hax.durationMs / 1000,
    mime: 'audio/mp4',
    haxUrl: initial.url,
    keys: allKeys,
    segmentCount: hax.segmentCount,
  };
}

/**
 * Node-only helper: download a track straight to a `.m4a` file.
 * Imports `node:fs` dynamically so browser bundlers never include it.
 */
export async function downloadHotaudioToFile(
  pageUrl: string,
  outPath: string,
  opts: DownloadOptions = {},
): Promise<{ outPath: string; title?: string; duration?: number; segmentCount: number }> {
  const res = await downloadHotaudioBuffer(pageUrl, opts);
  const fs = await import('node:fs/promises');
  await fs.writeFile(outPath, res.buffer);
  return { outPath, title: res.title, duration: res.duration, segmentCount: res.segmentCount };
}
