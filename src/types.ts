export interface HotaudioTrack {
  key: string;
  title: string;
}

export interface HotaudioState {
  pid: string;
  tick: string;
  /** Server static X25519 public key (hex). */
  key: string;
  tracks: Record<string, HotaudioTrack>;
  order?: number[];
}

export interface HotaudioListenResponse {
  url: string;
  length15s: number;
  keys: Record<string, string>;
}

export interface Hax0Segment {
  offset: number;
  pts: number;
}

export interface Hax0Container {
  fileLength: number;
  headerLength: number;
  extraLength: number;
  baseKey: Uint8Array;
  codec: string;
  durationMs: number;
  segmentCount: number;
  segments: Hax0Segment[];
}

/** Progress report emitted during long extractions. */
export interface HotaudioProgress {
  phase: 'resolving' | 'fetching' | 'decrypting';
  /** Bytes (fetching) or segments (decrypting) completed. */
  loaded: number;
  /** Total bytes/segments when known, 0 when unknown. */
  total: number;
}

export type HotaudioProgressCallback = (progress: HotaudioProgress) => void;

/** Framework-agnostic extraction result. */
export interface HotaudioResult {
  /** Playable URL (MediaSource object URL, Blob URL, or data URL). */
  url: string;
  host: 'hotaudio';
  title?: string;
  duration?: number;
  mime?: string;
}

/** Drop-in fetch implementation for proxies and test mocks. */
export type HotaudioFetch = typeof fetch;

export interface HotaudioOptions {
  userAgent?: string;
  fetchFn?: HotaudioFetch;
  onProgress?: HotaudioProgressCallback;
  /** Listen API base. Defaults to `https://hotaudio.net`. */
  apiBase?: string;
}
