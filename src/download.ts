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
import { fetchWithRetry } from './retry.ts';
import type {
  HotaudioFetch,
  HotaudioProgressCallback,
} from './types.ts';

export interface DownloadOptions {
  userAgent?: string;
  fetchFn?: HotaudioFetch;
  apiBase?: string;
  onProgress?: HotaudioProgressCallback;
  /** Per-attempt timeout in ms for page/listen/range requests. Defaults to 30s. Not applied to the bulk `.hax` fetch. */
  timeoutMs?: number;
  /** Track id to extract from multi-track pages. Defaults to the page's primary track. */
  trackId?: string;
  /**
   * Previously saved branch keys to seed the key map (resume/extend).
   * Merged with freshly fetched branches; branch keys are deterministic
   * per track, so keys from older sessions remain valid.
   */
  initialKeys?: Record<string, string>;
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
    timeoutMs: o.timeoutMs,
    trackId: o.trackId,
    initialKeys: o.initialKeys,
  };
}

function toKeysMap(keys: Record<string, string>): Record<number, Uint8Array> {
  const map: Record<number, Uint8Array> = {};
  for (const [k, v] of Object.entries(keys)) map[parseInt(k, 10)] = hexToBytes(v);
  return map;
}

/**
 * Key-branch paging.
 *
 * Measured against the live API: each `first:<n>` response unlocks a small
 * window starting at segment `n` (typically `[n..n+7]`), and concurrent
 * requests show no latency benefit over sequential ones (the server
 * effectively serializes per track). So on a cache miss the downloader
 * requests exactly the missing index — no lookahead, no fanout. Every
 * request is provably needed, which minimizes both wall time and server
 * load regardless of how the windows vary.
 */

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
 * Saved-keys envelope written by `--save-keys`. Accepts the envelope or a
 * bare `{ index: hex }` map anywhere keys are loaded (backward compatible).
 * Branch keys are deterministic per track, and `.hax` URLs are stable, so a
 * saved envelope re-downloads with zero page/listen requests: fetch the
 * `.hax` from a CDN and decrypt. If the track's key material ever rotates,
 * decrypt fails closed (ChaCha auth error) — just re-fetch keys.
 */
export interface SavedHotaudioKeys {
  version: 1;
  pageUrl: string;
  haxUrl: string;
  title?: string;
  savedAt: string;
  keys: Record<string, string>;
}

export interface ParsedKeysFile {
  keys: Record<string, string>;
  haxUrl?: string;
  pageUrl?: string;
  title?: string;
}

function isHexMap(value: unknown): value is Record<string, string> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  return Object.entries(value).every(
    ([k, v]) => /^\d+$/.test(k) && typeof v === 'string' && /^[0-9a-fA-F]+$/.test(v) && v.length % 2 === 0,
  );
}

/** Parse a `--keys` value: envelope object or bare key map, from JSON text or object. */
export function parseSavedKeys(input: string | Record<string, unknown>): ParsedKeysFile {
  const obj: Record<string, unknown> =
    typeof input === 'string' ? (JSON.parse(input) as Record<string, unknown>) : input;
  if (obj && typeof obj['keys'] === 'object' && obj['keys'] !== null) {
    const keys = obj['keys'] as Record<string, string>;
    if (!isHexMap(keys) || Object.keys(keys).length === 0) throw new Error('Invalid keys envelope: keys must be a non-empty { index: hex } map');
    const out: ParsedKeysFile = { keys };
    if (typeof obj['haxUrl'] === 'string') out.haxUrl = obj['haxUrl'];
    if (typeof obj['pageUrl'] === 'string') out.pageUrl = obj['pageUrl'];
    if (typeof obj['title'] === 'string') out.title = obj['title'];
    return out;
  }
  if (!isHexMap(obj) || Object.keys(obj).length === 0) {
    throw new Error('Invalid keys file: expected a { index: hex } map or a saved-keys envelope');
  }
  return { keys: obj };
}

/**
 * Cached download: fetch the `.hax` container and decrypt with saved keys.
 * No page fetch, no listen requests. `allKeys` must cover every segment.
 */
export async function downloadHaxBuffer(
  haxUrl: string,
  allKeys: Record<string, string>,
  opts: Pick<DownloadOptions, 'fetchFn' | 'userAgent' | 'onProgress'> = {},
): Promise<{ buffer: Uint8Array; segmentCount: number; duration?: number; mime: string }> {
  const fetchFn = opts.fetchFn ?? globalThis.fetch;
  const userAgent = opts.userAgent ?? HOTAUDIO_UA;
  const haxRes = await fetchWithRetry(fetchFn, haxUrl, { headers: { 'User-Agent': userAgent } });
  if (!haxRes.ok) throw new Error(`Hotaudio .hax fetch returned ${haxRes.status}`);
  const haxBytes = new Uint8Array(await haxRes.arrayBuffer());
  opts.onProgress?.({ phase: 'fetching', loaded: haxBytes.length, total: haxBytes.length });
  return decryptHaxBuffer(haxBytes, allKeys, opts.onProgress);
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

  const haxRes = await fetchWithRetry(o.fetchFn, initial.url, { headers: { 'User-Agent': o.userAgent } });
  if (!haxRes.ok) throw new Error(`Hotaudio .hax fetch returned ${haxRes.status}`);
  const haxBytes = new Uint8Array(await haxRes.arrayBuffer());
  o.onProgress?.({ phase: 'fetching', loaded: haxBytes.length, total: haxBytes.length });

  const hax = parseHax0Header(haxBytes);
  const keysMap = toKeysMap(initial.keys);
  const cache = new Map<number, Uint8Array>();
  const allKeys: Record<string, string> = { ...initial.keys };
  if (o.initialKeys) {
    // Seeds must be well-formed hex: a corrupt seed would derive a wrong
    // (but valid-shaped) key and fail closed at decrypt time instead of
    // paging. Invalid entries are skipped so paging refetches them.
    for (const [k, v] of Object.entries(o.initialKeys)) {
      if (!/^\d+$/.test(k) || typeof v !== 'string' || v.length === 0 || v.length % 2 !== 0 || !/^[0-9a-fA-F]+$/.test(v)) continue;
      allKeys[k] = v;
      keysMap[parseInt(k, 10)] = hexToBytes(v);
    }
  }

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
      const extra = await listenRequest(handshake, i, o);
      const merged = mergeBranchKeys(keysMap, extra.keys, hexToBytes);
      Object.assign(allKeys, extra.keys);
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
