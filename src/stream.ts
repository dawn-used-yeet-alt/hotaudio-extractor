import { hexToBytes } from './crypto.ts';
import { parseHax0Header, deriveSegmentKey, decryptSegmentSlice } from './hax_decoder.ts';
import {
  loadHotaudioHandshake,
  listenRequest,
  mergeBranchKeys,
  type HotaudioHandshake,
} from './listen.ts';
import type { HotaudioListenResponse, HotaudioProgressCallback } from './types.ts';
import { HOTAUDIO_UA } from './constants.ts';
import { fetchWithRetry, FETCH_API_TIMEOUT_MS } from './retry.ts';
import type { DownloadOptions } from './download.ts';

export interface HotaudioStreamSession {
  /** MediaSource object URL. Assign to `audio.src` once `ready` resolves. */
  url: string;
  /** Resolves when the first segment is appended. */
  ready: Promise<void>;
  /** Resolves when all segments are appended or the session aborts. */
  done: Promise<void>;
  title?: string;
  /** Track duration in seconds. */
  duration?: number;
  /** Stop background work and release the object URL. */
  abort(): void;
}

async function fetchRange(
  fetchFn: typeof fetch,
  url: string,
  start: number,
  end: number,
  signal: AbortSignal,
  timeoutMs: number = FETCH_API_TIMEOUT_MS,
): Promise<Uint8Array> {
  const res = await fetchWithRetry(fetchFn, url, {
    headers: { 'User-Agent': HOTAUDIO_UA, Range: `bytes=${start}-${end}` },
    signal,
  }, { timeoutMs });
  // Some mirrors ignore Range and return 200 with the full file.
  if (!res.ok || (res.status !== 206 && res.status !== 200)) {
    throw new Error(`Hotaudio range fetch returned ${res.status}`);
  }
  const bytes = new Uint8Array(await res.arrayBuffer());
  return res.status === 200 ? bytes.subarray(start, end + 1) : bytes;
}

function appendBuffer(sb: SourceBuffer, data: Uint8Array): Promise<void> {
  return new Promise((resolve, reject) => {
    const onDone = () => {
      sb.removeEventListener('updateend', onDone);
      sb.removeEventListener('error', onFail);
      resolve();
    };
    const onFail = () => {
      sb.removeEventListener('updateend', onDone);
      sb.removeEventListener('error', onFail);
      reject(new Error('MediaSource append failed'));
    };
    sb.addEventListener('updateend', onDone);
    sb.addEventListener('error', onFail);
    const copy = new Uint8Array(data.length);
    copy.set(data);
    sb.appendBuffer(copy.buffer as ArrayBuffer);
  });
}

/**
 * Progressive playback over Media Source Extensions.
 *
 * Reads the `.hax` header to learn the segment table, then downloads,
 * decrypts, and appends one segment at a time. Browser-only — returns
 * null when `MediaSource` is unavailable so callers can fall back to
 * `downloadHotaudioBuffer`.
 */
export async function extractHotaudioStream(
  pageUrl: string,
  opts: DownloadOptions = {},
): Promise<HotaudioStreamSession | null> {
  const onProgress: HotaudioProgressCallback | undefined = opts.onProgress;
  const fetchFn = opts.fetchFn ?? globalThis.fetch;
  if (typeof MediaSource === 'undefined') return null;
  const mimeCandidates = ['audio/mp4; codecs="mp4a.40.2"', 'audio/mp4'];
  const mime = mimeCandidates.find((m) => {
    try {
      return MediaSource.isTypeSupported(m);
    } catch {
      return false;
    }
  });
  if (!mime) return null;

  onProgress?.({ phase: 'resolving', loaded: 0, total: 0 });
  const handshake: HotaudioHandshake | null = await loadHotaudioHandshake(pageUrl, opts).catch(
    () => null,
  );
  if (!handshake) return null;

  const controller = new AbortController();
  const signal = controller.signal;
  let initial: HotaudioListenResponse;
  try {
    initial = await listenRequest(handshake, -1, opts);
  } catch (err) {
    console.error(err instanceof Error ? err.message : err);
    return null;
  }
  if (!initial.url) return null;
  const haxUrl = initial.url;

  const head = await fetchRange(fetchFn, haxUrl, 0, 15, signal, opts.timeoutMs).catch(() => null);
  if (!head || head.length < 16) return null;
  const headCopy = new Uint8Array(16);
  headCopy.set(head.subarray(0, 16));
  const view = new DataView(headCopy.buffer);
  const headerLength = view.getUint32(8, true);
  const fileLength = view.getUint32(4, true);
  if (headerLength < 16 || headerLength > fileLength) return null;
  const headerBytes = await fetchRange(fetchFn, haxUrl, 0, headerLength - 1, signal, opts.timeoutMs).catch(
    () => null,
  );
  if (!headerBytes) return null;
  const hax = parseHax0Header(headerBytes);

  const keysMap: Record<number, Uint8Array> = {};
  for (const [k, v] of Object.entries(initial.keys)) {
    keysMap[parseInt(k, 10)] = hexToBytes(v);
  }
  const nodeKeyCache = new Map<number, Uint8Array>();

  const mediaSource = new MediaSource();
  const url = URL.createObjectURL(mediaSource);
  let resolveReady!: () => void;
  const ready = new Promise<void>((res) => {
    resolveReady = res;
  });
  let aborted = false;

  const pump = async (): Promise<void> => {
    await new Promise<void>((resolve, reject) => {
      if (mediaSource.readyState === 'open') return resolve();
      const onOpen = () => {
        mediaSource.removeEventListener('sourceopen', onOpen);
        resolve();
      };
      mediaSource.addEventListener('sourceopen', onOpen, { once: true });
      setTimeout(() => reject(new Error('MediaSource never opened')), 30000);
    });
    const sb = mediaSource.addSourceBuffer(mime);
    let appendedBytes = 0;
    const totalBytes = fileLength - hax.segments[0].offset;

    for (let i = 0; i < hax.segmentCount; i++) {
      if (signal.aborted || aborted) break;
      const seg = hax.segments[i];
      const nextOff = i + 1 < hax.segmentCount ? hax.segments[i + 1].offset : fileLength;
      const slice = await fetchRange(fetchFn, haxUrl, seg.offset, nextOff - 1, signal, opts.timeoutMs);
      let segKey: Uint8Array;
      try {
        segKey = await deriveSegmentKey(keysMap, hax.segmentCount, i, nodeKeyCache);
      } catch (err) {
        if (!(err instanceof Error) || !err.message.startsWith('Key missing in keys map')) throw err;
        const extra = await listenRequest(handshake, i, opts);
        if (mergeBranchKeys(keysMap, extra.keys, hexToBytes) === 0) throw err;
        nodeKeyCache.clear();
        segKey = await deriveSegmentKey(keysMap, hax.segmentCount, i, nodeKeyCache);
      }
      const plain = decryptSegmentSlice(slice, segKey);
      await appendBuffer(sb, plain);
      appendedBytes += plain.length;
      if (i === 0) resolveReady();
      onProgress?.({ phase: 'fetching', loaded: appendedBytes, total: totalBytes });
    }

    try {
      if (!signal.aborted && !aborted && mediaSource.readyState === 'open') {
        mediaSource.endOfStream();
      }
    } catch {
      // Already closed by abort().
    }
  };

  const done = pump().catch((err: unknown) => {
    if (signal.aborted || aborted) return;
    console.error('Hotaudio stream failed:', err);
  });

  return {
    url,
    ready,
    done,
    title: handshake.track.title,
    duration: hax.durationMs / 1000,
    abort() {
      aborted = true;
      try {
        controller.abort();
      } catch {
        // Already settled.
      }
      try {
        URL.revokeObjectURL(url);
      } catch {
        // Already revoked.
      }
    },
  };
}
