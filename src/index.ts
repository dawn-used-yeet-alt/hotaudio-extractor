/**
 * hotaudio-extractor — standalone hotaudio.net audio extractor.
 *
 * Browser: stream-first MSE playback (`extractHotaudio`,
 * `extractHotaudioStream`) with a full-download Blob fallback.
 * Node / Bun / workers: `downloadHotaudioBuffer` and
 * `downloadHotaudioToFile` return raw playable `.m4a` bytes.
 */
import { HOTAUDIO_STREAM_READY_TIMEOUT_MS } from './constants.ts';
import { downloadHotaudioBuffer, type DownloadOptions } from './download.ts';
import { extractHotaudioStream, type HotaudioStreamSession } from './stream.ts';
import type { HotaudioResult } from './types.ts';

export const HOTAUDIO_PATTERN =
  /https?:\/\/(?:www\.)?hotaudio\.net\/u\/[a-zA-Z0-9_%~.-]+\/[a-zA-Z0-9_%~.-]+(?:\?[^"'\s<>)\]]*)?/i;

/** @deprecated Use {@link HOTAUDIO_PATTERN}. */
export const HOTAUDIO_REGEX = HOTAUDIO_PATTERN;

export { HOTAUDIO_UA, HOTAUDIO_API_BASE, HOTAUDIO_STREAM_READY_TIMEOUT_MS } from './constants.ts';
export {
  fetchWithRetry,
  isRetryableStatus,
  FETCH_RETRY_ATTEMPTS,
  FETCH_RETRY_BASE_DELAY_MS,
  FETCH_RETRY_MAX_DELAY_MS,
  FETCH_API_TIMEOUT_MS,
  type FetchRetryOptions,
} from './retry.ts';
export { PINNED_NOZZLE_VERSION, signHotaudioPayload } from './signer.ts';
export {
  hexToBytes,
  bytesToHex,
  base64ToBytes,
  sha256,
  decryptHotaudioState,
  performKeyExchange,
} from './crypto.ts';
export {
  decodeBencode,
  parseHax0Header,
  deriveSegmentKey,
  decryptSegmentSlice,
} from './hax_decoder.ts';
export {
  extractHaState,
  loadHandshakeFromHtml,
  loadHotaudioHandshake,
  listenRequest,
  mergeBranchKeys,
  type HotaudioHandshake,
  type HandshakeOptions,
} from './listen.ts';
export {
  decryptHaxBuffer,
  fetchHotaudioKeys,
  downloadHotaudioBuffer,
  downloadWithHandshake,
  downloadHotaudioToFile,
  downloadHaxBuffer,
  parseSavedKeys,
  type DownloadOptions,
  type HotaudioBufferResult,
  type SavedHotaudioKeys,
  type ParsedKeysFile,
} from './download.ts';
export { extractHotaudioStream, type HotaudioStreamSession } from './stream.ts';
export type {
  HotaudioTrack,
  HotaudioState,
  HotaudioListenResponse,
  Hax0Segment,
  Hax0Container,
  HotaudioProgress,
  HotaudioProgressCallback,
  HotaudioResult,
  HotaudioFetch,
  HotaudioOptions,
} from './types.ts';

/**
 * Browser-first entry point: progressive MSE streaming with a full-download
 * fallback. In runtimes without MediaSource/DOM it uses the download path
 * and returns a Blob URL (or base64 data URL where Blob URLs are unavailable).
 */
export async function extractHotaudio(
  url: string,
  opts: DownloadOptions & { streamTimeoutMs?: number } = {},
): Promise<HotaudioResult | null> {
  try {
    const session: HotaudioStreamSession | null = await extractHotaudioStream(url, opts).catch(
      () => null,
    );
    if (session) {
      try {
        await Promise.race([
          session.ready,
          new Promise((_, reject) =>
            setTimeout(
              () => reject(new Error('Hotaudio stream timeout')),
              opts.streamTimeoutMs ?? HOTAUDIO_STREAM_READY_TIMEOUT_MS,
            ),
          ),
        ]);
        return {
          url: session.url,
          host: 'hotaudio',
          title: session.title,
          duration: session.duration,
        };
      } catch {
        session.abort();
      }
    }
  } catch {
    // Fall through to the full-download path.
  }
  return extractHotaudioDownload(url, opts);
}

/**
 * Full-download fallback: fetches and decrypts the whole track before
 * returning a playable URL.
 */
export async function extractHotaudioDownload(
  url: string,
  opts: DownloadOptions = {},
): Promise<HotaudioResult | null> {
  try {
    const res = await downloadHotaudioBuffer(url, opts);
    if (typeof Blob !== 'undefined' && typeof URL !== 'undefined' && 'createObjectURL' in URL) {
      const blob = new Blob([res.buffer as unknown as ArrayBuffer], { type: res.mime });
      return { url: URL.createObjectURL(blob), host: 'hotaudio', title: res.title, duration: res.duration, mime: res.mime };
    }
    let b64: string;
    if (typeof Buffer !== 'undefined') {
      b64 = Buffer.from(res.buffer).toString('base64');
    } else {
      let bin = '';
      const CHUNK = 0x8000;
      for (let i = 0; i < res.buffer.length; i += CHUNK) {
        bin += String.fromCharCode(...res.buffer.subarray(i, i + CHUNK));
      }
      b64 = btoa(bin);
    }
    return {
      url: `data:${res.mime};base64,${b64}`,
      host: 'hotaudio',
      title: res.title,
      duration: res.duration,
      mime: res.mime,
    };
  } catch (err) {
    console.error('Hotaudio extraction failed:', err);
    return null;
  }
}

/** Match canonical hotaudio share links. */
export function isHotaudioUrl(url: string): boolean {
  return HOTAUDIO_PATTERN.test(url);
}
