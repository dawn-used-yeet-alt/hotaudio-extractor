interface HotaudioTrack {
    key: string;
    title: string;
}
interface HotaudioState {
    pid: string;
    tick: string;
    /** Server static X25519 public key (hex). */
    key: string;
    tracks: Record<string, HotaudioTrack>;
    order?: number[];
}
interface HotaudioListenResponse {
    url: string;
    /** Absent on the wire in observed responses; kept for compatibility. */
    length15s?: number;
    keys: Record<string, string>;
}
interface Hax0Segment {
    offset: number;
    pts: number;
}
interface Hax0Container {
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
interface HotaudioProgress {
    phase: 'resolving' | 'fetching' | 'decrypting';
    /** Bytes (fetching) or segments (decrypting) completed. */
    loaded: number;
    /** Total bytes/segments when known, 0 when unknown. */
    total: number;
}
type HotaudioProgressCallback = (progress: HotaudioProgress) => void;
/** Framework-agnostic extraction result. */
interface HotaudioResult {
    /** Playable URL (MediaSource object URL, Blob URL, or data URL). */
    url: string;
    host: 'hotaudio';
    title?: string;
    duration?: number;
    mime?: string;
}
/** Drop-in fetch implementation for proxies and test mocks. */
type HotaudioFetch = typeof fetch;
interface HotaudioOptions {
    userAgent?: string;
    fetchFn?: HotaudioFetch;
    onProgress?: HotaudioProgressCallback;
    /** Listen API base. Defaults to `https://hotaudio.net`. */
    apiBase?: string;
}

interface HotaudioHandshake {
    state: HotaudioState;
    tid: string;
    track: HotaudioTrack;
    /** Ephemeral X25519 client public key (hex), reused for the whole extraction. */
    clientPubHex: string;
    /** Session secret (SHA-256 of the shared secret). */
    Ee: Uint8Array;
    apiBase: string;
    /** Page-URL `?key=` param, forwarded to listen requests (private/unlisted tracks). */
    listenKey?: string;
}
interface HandshakeOptions {
    userAgent?: string;
    fetchFn?: HotaudioFetch;
    apiBase?: string;
    /** Per-attempt timeout in ms for page and listen requests. Defaults to 30s. */
    timeoutMs?: number;
    /** Track id to extract from multi-track pages. Defaults to the page's primary track. */
    trackId?: string;
}
/** Extract the raw `__ha_state` value from track page HTML, or null when absent. */
declare function extractHaState(html: string): string | null;
interface HotaudioTrackInfo {
    id: string;
    key: string;
    title: string;
}
/**
 * List every track on a page in page order. Returns null when the page
 * has no decryptable state. Used for `--track` discovery.
 */
declare function listHotaudioTracks(html: string): HotaudioTrackInfo[] | null;
/**
 * Derive the `.hax` container URL from a track key, without any listen
 * call. Observed rule (stable across tracks and sessions):
 * `https://cdn.hotaudio.net/a/<key>.hax`. Informational — the downloader
 * still uses the server-issued URL; the probe warns if they ever differ.
 */
declare function haxUrlForTrackKey(trackKey: string): string;
/** Extract the page-URL `?key=` param the player forwards to listen requests. */
declare function extractListenKey(pageUrl: string): string | null;
/** Build a handshake from already-fetched page HTML. */
declare function loadHandshakeFromHtml(html: string, apiBase?: string, trackId?: string): Promise<HotaudioHandshake | null>;
/** Fetch the track page, decrypt its state, and establish the key-exchange session. */
declare function loadHotaudioHandshake(pageUrl: string, opts?: HandshakeOptions): Promise<HotaudioHandshake | null>;
/** Fetch a track page and list its tracks (for `--track` discovery). */
declare function fetchHotaudioTracks(pageUrl: string, opts?: HandshakeOptions): Promise<HotaudioTrackInfo[] | null>;
/**
 * Perform one encrypted listen request.
 *
 * Each call requires a fresh `X-Signature`. Pass `first: -1` for the initial
 * call (returns the `.hax` URL), then `first: <segmentIndex>` to page
 * additional key branches for long tracks.
 */
declare function listenRequest(handshake: HotaudioHandshake, first: number, opts?: HandshakeOptions): Promise<HotaudioListenResponse>;
/** Merge follow-up branch keys into `keysMap`. Returns the number of new keys. */
declare function mergeBranchKeys(keysMap: Record<number, Uint8Array>, keys: Record<string, string>, fromHex: (hex: string) => Uint8Array): number;

interface DownloadOptions {
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
interface HotaudioBufferResult {
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
/**
 * Decrypt a full `.hax` buffer with previously fetched keys.
 *
 * Offline path: performs no network I/O. `allKeys` must cover every
 * segment (merge several `first:<n>` responses for long tracks).
 */
declare function decryptHaxBuffer(haxBytes: Uint8Array, allKeys: Record<string, string>, onProgress?: HotaudioProgressCallback): Promise<{
    buffer: Uint8Array;
    segmentCount: number;
    duration?: number;
    mime: string;
}>;
/**
 * Saved-keys envelope written by `--save-keys`. Accepts the envelope or a
 * bare `{ index: hex }` map anywhere keys are loaded (backward compatible).
 * Branch keys are deterministic per track, and `.hax` URLs are stable, so a
 * saved envelope re-downloads with zero page/listen requests: fetch the
 * `.hax` from a CDN and decrypt. If the track's key material ever rotates,
 * decrypt fails closed (ChaCha auth error) — just re-fetch keys.
 */
interface SavedHotaudioKeys {
    version: 1;
    pageUrl: string;
    haxUrl: string;
    title?: string;
    savedAt: string;
    keys: Record<string, string>;
}
interface ParsedKeysFile {
    keys: Record<string, string>;
    haxUrl?: string;
    pageUrl?: string;
    title?: string;
}
/** Parse a `--keys` value: envelope object or bare key map, from JSON text or object. */
declare function parseSavedKeys(input: string | Record<string, unknown>): ParsedKeysFile;
/**
 * Cached download: fetch the `.hax` container and decrypt with saved keys.
 * No page fetch, no listen requests. `allKeys` must cover every segment.
 */
declare function downloadHaxBuffer(haxUrl: string, allKeys: Record<string, string>, opts?: Pick<DownloadOptions, 'fetchFn' | 'userAgent' | 'onProgress'>): Promise<{
    buffer: Uint8Array;
    segmentCount: number;
    duration?: number;
    mime: string;
}>;
/**
 * Fetch only the first key branch and the `.hax` URL, without audio.
 * Long tracks need further `listenRequest(handshake, segIdx)` calls.
 */
declare function fetchHotaudioKeys(pageUrl: string, opts?: DownloadOptions): Promise<{
    keys: Record<string, string>;
    url: string;
    title?: string;
    handshake: HotaudioHandshake;
}>;
/**
 * Full-track download: handshake, listen, `.hax` fetch, decrypt.
 * Works in browsers, Node, Bun, and workers (global fetch required).
 * Long tracks page `first:<segmentIndex>` automatically.
 */
declare function downloadHotaudioBuffer(pageUrl: string, opts?: DownloadOptions): Promise<HotaudioBufferResult>;
/** Full-track download starting from an existing handshake (local HTML, tests). */
declare function downloadWithHandshake(handshake: HotaudioHandshake, opts?: DownloadOptions): Promise<HotaudioBufferResult>;
/**
 * Node-only helper: download a track straight to a `.m4a` file.
 * Imports `node:fs` dynamically so browser bundlers never include it.
 */
declare function downloadHotaudioToFile(pageUrl: string, outPath: string, opts?: DownloadOptions): Promise<{
    outPath: string;
    title?: string;
    duration?: number;
    segmentCount: number;
}>;

/** Shared hotaudio constants. */
/**
 * Default User-Agent for page and API requests.
 *
 * Cloudflare returns HTTP 403 (`cf-mitigated: challenge`) for Chrome
 * user-agents on the track page and the listen endpoint. The bare
 * `Mozilla/5.0` token is accepted.
 */
declare const HOTAUDIO_UA = "Mozilla/5.0";
/** Base URL for the encrypted listen handshake. */
declare const HOTAUDIO_API_BASE = "https://hotaudio.net";
/** Time the stream-first path waits for the first segment before falling back to full download. */
declare const HOTAUDIO_STREAM_READY_TIMEOUT_MS = 20000;

/** Total attempts per request (initial + retries). */
declare const FETCH_RETRY_ATTEMPTS = 3;
/** Base backoff between retries; doubled per attempt with jitter. */
declare const FETCH_RETRY_BASE_DELAY_MS = 500;
/** Upper bound for a single retry wait (caps Retry-After). */
declare const FETCH_RETRY_MAX_DELAY_MS = 10000;
/** Default per-attempt timeout for small API calls (page, listen, ranges). */
declare const FETCH_API_TIMEOUT_MS = 30000;
interface FetchRetryOptions {
    attempts?: number;
    baseDelayMs?: number;
    /** Per-attempt timeout in ms. Undefined = no timeout (bulk transfers). */
    timeoutMs?: number;
}
/** True for transient HTTP statuses worth retrying (429/5xx class). */
declare function isRetryableStatus(status: number): boolean;
/**
 * Fetch with retries for transient failures: network errors, timeouts,
 * and retryable statuses (429/5xx). Other statuses return as-is.
 * Caller-aborted requests are never retried.
 */
declare function fetchWithRetry(fetchFn: HotaudioFetch, url: string, init?: RequestInit, opts?: FetchRetryOptions): Promise<Response>;

/**
 * Player version the bundled nozzle copy was captured from.
 * The signer emulates the browser environment this build expects.
 */
declare const PINNED_NOZZLE_VERSION = "1J1Db0bF";
/**
 * Compute the `X-Signature` header value for a listen request payload.
 * Pass `timestampSeconds` to pin the signing clock (tests only).
 */
declare function signHotaudioPayload(payload: string, timestampSeconds?: number): string;

/** Decode a lowercase/uppercase hex string into bytes. */
declare function hexToBytes(hex: string): Uint8Array;
/** Encode bytes as lowercase hex. */
declare function bytesToHex(bytes: Uint8Array): string;
/** Decode base64 in Node and browser runtimes. */
declare function base64ToBytes(b64: string): Uint8Array;
/** SHA-256 via WebCrypto. */
declare function sha256(data: Uint8Array): Promise<Uint8Array>;
/** Decrypt the `__ha_state` base64 payload embedded in the track page. */
declare function decryptHotaudioState(stateB64: string): HotaudioState;
interface KeyExchangeResult {
    clientPubHex: string;
    /** SHA-256 of the X25519 shared secret; encrypts listen payloads. */
    Ee: Uint8Array;
}
/** Generate an ephemeral X25519 keypair and derive the session secret. */
declare function performKeyExchange(serverPubHex: string): Promise<KeyExchangeResult>;

/** Minimal bencode decoder for HAX0 metadata dictionaries. */
declare function decodeBencode(buf: Uint8Array, offset: number): {
    value: unknown;
    nextOffset: number;
};
/** Parse the HAX0 container header and segment table. */
declare function parseHax0Header(buffer: Uint8Array): Hax0Container;
/**
 * Derive the decryption key for one segment.
 *
 * Walks from the nearest known ancestor in `keysMap` down to the leaf,
 * hashing at each tree level. `cache` memoizes intermediate node keys
 * within a single extraction; it must be cleared whenever new branch keys
 * are merged into `keysMap`.
 */
declare function deriveSegmentKey(keysMap: Record<number, Uint8Array>, segmentCount: number, segIdx: number, cache?: Map<number, Uint8Array>): Promise<Uint8Array>;
/** Decrypt one HAX0 segment slice with its derived key (zero nonce). */
declare function decryptSegmentSlice(haxSlice: Uint8Array, key: Uint8Array): Uint8Array;

interface HotaudioStreamSession {
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
/**
 * Progressive playback over Media Source Extensions.
 *
 * Reads the `.hax` header to learn the segment table, then downloads,
 * decrypts, and appends one segment at a time. Browser-only — returns
 * null when `MediaSource` is unavailable so callers can fall back to
 * `downloadHotaudioBuffer`.
 */
declare function extractHotaudioStream(pageUrl: string, opts?: DownloadOptions): Promise<HotaudioStreamSession | null>;

declare const HOTAUDIO_PATTERN: RegExp;
/** @deprecated Use {@link HOTAUDIO_PATTERN}. */
declare const HOTAUDIO_REGEX: RegExp;

/**
 * Browser-first entry point: progressive MSE streaming with a full-download
 * fallback. In runtimes without MediaSource/DOM it uses the download path
 * and returns a Blob URL (or base64 data URL where Blob URLs are unavailable).
 */
declare function extractHotaudio(url: string, opts?: DownloadOptions & {
    streamTimeoutMs?: number;
}): Promise<HotaudioResult | null>;
/**
 * Full-download fallback: fetches and decrypts the whole track before
 * returning a playable URL.
 */
declare function extractHotaudioDownload(url: string, opts?: DownloadOptions): Promise<HotaudioResult | null>;
/** Match canonical hotaudio share links. */
declare function isHotaudioUrl(url: string): boolean;

export { type DownloadOptions, FETCH_API_TIMEOUT_MS, FETCH_RETRY_ATTEMPTS, FETCH_RETRY_BASE_DELAY_MS, FETCH_RETRY_MAX_DELAY_MS, type FetchRetryOptions, HOTAUDIO_API_BASE, HOTAUDIO_PATTERN, HOTAUDIO_REGEX, HOTAUDIO_STREAM_READY_TIMEOUT_MS, HOTAUDIO_UA, type HandshakeOptions, type Hax0Container, type Hax0Segment, type HotaudioBufferResult, type HotaudioFetch, type HotaudioHandshake, type HotaudioListenResponse, type HotaudioOptions, type HotaudioProgress, type HotaudioProgressCallback, type HotaudioResult, type HotaudioState, type HotaudioStreamSession, type HotaudioTrack, type HotaudioTrackInfo, PINNED_NOZZLE_VERSION, type ParsedKeysFile, type SavedHotaudioKeys, base64ToBytes, bytesToHex, decodeBencode, decryptHaxBuffer, decryptHotaudioState, decryptSegmentSlice, deriveSegmentKey, downloadHaxBuffer, downloadHotaudioBuffer, downloadHotaudioToFile, downloadWithHandshake, extractHaState, extractHotaudio, extractHotaudioDownload, extractHotaudioStream, extractListenKey, fetchHotaudioKeys, fetchHotaudioTracks, fetchWithRetry, haxUrlForTrackKey, hexToBytes, isHotaudioUrl, isRetryableStatus, listHotaudioTracks, listenRequest, loadHandshakeFromHtml, loadHotaudioHandshake, mergeBranchKeys, parseHax0Header, parseSavedKeys, performKeyExchange, sha256, signHotaudioPayload };
