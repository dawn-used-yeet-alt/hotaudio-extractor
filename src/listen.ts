import { chacha20poly1305 } from '@noble/ciphers/chacha.js';
import { decryptHotaudioState, performKeyExchange, sha256 } from './crypto.ts';
import { fetchWithRetry, FETCH_API_TIMEOUT_MS } from './retry.ts';
import { signHotaudioPayload } from './signer.ts';
import { HOTAUDIO_API_BASE, HOTAUDIO_UA } from './constants.ts';
import type {
  HotaudioFetch,
  HotaudioListenResponse,
  HotaudioState,
  HotaudioTrack,
} from './types.ts';

const UTF8_ENC = new TextEncoder();
const UTF8_DEC = new TextDecoder();

export interface HotaudioHandshake {
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

export interface HandshakeOptions {
  userAgent?: string;
  fetchFn?: HotaudioFetch;
  apiBase?: string;
  /** Per-attempt timeout in ms for page and listen requests. Defaults to 30s. */
  timeoutMs?: number;
  /** Track id to extract from multi-track pages. Defaults to the page's primary track. */
  trackId?: string;
}

/** Extract the raw `__ha_state` value from track page HTML, or null when absent. */
export function extractHaState(html: string): string | null {
  const m = html.match(/var __ha_state = "([^"]+)"/);
  return m?.[1] ?? null;
}

export interface HotaudioTrackInfo {
  id: string;
  key: string;
  title: string;
}

/**
 * List every track on a page in page order. Returns null when the page
 * has no decryptable state. Used for `--track` discovery.
 */
export function listHotaudioTracks(html: string): HotaudioTrackInfo[] | null {
  const stateB64 = extractHaState(html);
  if (!stateB64) return null;
  let state: HotaudioState;
  try {
    state = decryptHotaudioState(stateB64);
  } catch {
    return null;
  }
  const ordered = Array.isArray(state.order) ? state.order.map(String) : [];
  const ids = [...ordered.filter((id) => state.tracks[id]), ...Object.keys(state.tracks).filter((id) => !ordered.includes(id))];
  return ids.map((id) => ({ id, key: state.tracks[id].key, title: state.tracks[id].title }));
}

/**
 * Derive the `.hax` container URL from a track key, without any listen
 * call. Observed rule (stable across tracks and sessions):
 * `https://cdn.hotaudio.net/a/<key>.hax`. Informational — the downloader
 * still uses the server-issued URL; the probe warns if they ever differ.
 */
export function haxUrlForTrackKey(trackKey: string): string {
  return `https://cdn.hotaudio.net/a/${trackKey}.hax`;
}

/** Extract the page-URL `?key=` param the player forwards to listen requests. */
export function extractListenKey(pageUrl: string): string | null {
  try {
    return new URL(pageUrl).searchParams.get('key');
  } catch {
    return null;
  }
}

/** Build a handshake from already-fetched page HTML. */
export async function loadHandshakeFromHtml(
  html: string,
  apiBase: string = HOTAUDIO_API_BASE,
  trackId?: string,
): Promise<HotaudioHandshake | null> {
  const stateB64 = extractHaState(html);
  if (!stateB64) return null;
  let state: HotaudioState;
  try {
    state = decryptHotaudioState(stateB64);
  } catch {
    return null;
  }
  return buildHandshake(state, apiBase, trackId);
}

async function buildHandshake(
  state: HotaudioState,
  apiBase: string,
  trackId?: string,
): Promise<HotaudioHandshake | null> {
  if (trackId) {
    if (!state.tracks[trackId]) return null;
    const session = await performKeyExchange(state.key);
    return {
      state,
      tid: trackId,
      track: state.tracks[trackId],
      clientPubHex: session.clientPubHex,
      Ee: session.Ee,
      apiBase,
    };
  }
  // Integer-like track ids sort numerically under Object.keys(), which can
  // hide the page's primary track. Honor the page-provided order first.
  const orderedIds = Array.isArray(state.order)
    ? state.order.map((n: number) => String(n)).filter((id: string) => state.tracks[id])
    : [];
  const tid = orderedIds[0] ?? Object.keys(state.tracks)[0];
  if (!tid || !state.tracks[tid]) return null;

  // One session keypair per extraction. Each request still uses a fresh
  // signature-derived nonce, so nonces never repeat under the shared secret.
  const session = await performKeyExchange(state.key);
  return {
    state,
    tid,
    track: state.tracks[tid],
    clientPubHex: session.clientPubHex,
    Ee: session.Ee,
    apiBase,
  };
}

/** Fetch the track page, decrypt its state, and establish the key-exchange session. */
export async function loadHotaudioHandshake(
  pageUrl: string,
  opts: HandshakeOptions = {},
): Promise<HotaudioHandshake | null> {
  const userAgent = opts.userAgent ?? HOTAUDIO_UA;
  const fetchFn = opts.fetchFn ?? globalThis.fetch;
  const apiBase = opts.apiBase ?? HOTAUDIO_API_BASE;
  const pageRes = await fetchWithRetry(fetchFn, pageUrl, { headers: { 'User-Agent': userAgent } }, {
    timeoutMs: opts.timeoutMs ?? FETCH_API_TIMEOUT_MS,
  });
  if (!pageRes.ok) return null;
  const handshake = await loadHandshakeFromHtml(await pageRes.text(), apiBase, opts.trackId);
  if (handshake) handshake.listenKey = extractListenKey(pageUrl) ?? undefined;
  return handshake;
}

/** Fetch a track page and list its tracks (for `--track` discovery). */
export async function fetchHotaudioTracks(
  pageUrl: string,
  opts: HandshakeOptions = {},
): Promise<HotaudioTrackInfo[] | null> {
  const userAgent = opts.userAgent ?? HOTAUDIO_UA;
  const fetchFn = opts.fetchFn ?? globalThis.fetch;
  const pageRes = await fetchWithRetry(fetchFn, pageUrl, { headers: { 'User-Agent': userAgent } }, {
    timeoutMs: opts.timeoutMs ?? FETCH_API_TIMEOUT_MS,
  });
  if (!pageRes.ok) return null;
  return listHotaudioTracks(await pageRes.text());
}

/**
 * Perform one encrypted listen request.
 *
 * Each call requires a fresh `X-Signature`. Pass `first: -1` for the initial
 * call (returns the `.hax` URL), then `first: <segmentIndex>` to page
 * additional key branches for long tracks.
 */
export async function listenRequest(
  handshake: HotaudioHandshake,
  first: number,
  opts: HandshakeOptions = {},
): Promise<HotaudioListenResponse> {
  const { state, tid, track, clientPubHex, Ee } = handshake;
  const userAgent = opts.userAgent ?? HOTAUDIO_UA;
  const fetchFn = opts.fetchFn ?? globalThis.fetch;
  const apiBase = opts.apiBase ?? handshake.apiBase ?? HOTAUDIO_API_BASE;
  const payloadStr = JSON.stringify({ tid, pid: state.pid, key: track.key, tick: state.tick, first });

  const sig = signHotaudioPayload(payloadStr);
  const reqNonce = (await sha256(UTF8_ENC.encode(sig))).subarray(0, 12);
  const encBody = chacha20poly1305(Ee, reqNonce).encrypt(UTF8_ENC.encode(payloadStr));

  const listenUrl = `${apiBase}/api/v1/audio/listen${handshake.listenKey ? `?key=${encodeURIComponent(handshake.listenKey)}` : ''}`;
  const listenRes = await fetchWithRetry(fetchFn, listenUrl, {
    method: 'POST',
    headers: {
      'X-Signature': sig,
      'X-Key': clientPubHex,
      'Content-Type': 'application/vnd.hotaudio.crypt+json',
      'User-Agent': userAgent,
      Origin: 'https://hotaudio.net',
      Referer: 'https://hotaudio.net/',
    },
    body: encBody,
  }, {
    timeoutMs: opts.timeoutMs ?? FETCH_API_TIMEOUT_MS,
  });
  if (!listenRes.ok) {
    const snippet = await listenRes.text().then((t) => t.slice(0, 300)).catch(() => '');
    throw new Error(
      `Hotaudio listen API returned ${listenRes.status} for first=${first}${snippet ? `: ${snippet}` : ''}`,
    );
  }

  // Success bodies are encrypted (`application/vnd.hotaudio.crypt+json`);
  // anything else is a plaintext error — surface it instead of decrypting garbage.
  // A missing content-type (mocks) still takes the decrypt path.
  const contentType = listenRes.headers.get('content-type') ?? '';
  const respBuf = new Uint8Array(await listenRes.arrayBuffer());
  if (contentType && !contentType.includes('hotaudio.crypt')) {
    throw new Error(
      `Hotaudio listen API returned a non-crypt body for first=${first}: ${UTF8_DEC.decode(respBuf).slice(0, 300)}`,
    );
  }
  // Responses use the same secret with the first nonce byte incremented,
  // which separates the request and response nonce domains.
  const respNonce = new Uint8Array(reqNonce);
  respNonce[0] = (respNonce[0] + 1) & 0xff;
  const decoded = chacha20poly1305(Ee, respNonce).decrypt(respBuf);
  return JSON.parse(UTF8_DEC.decode(decoded)) as HotaudioListenResponse;
}

/** Merge follow-up branch keys into `keysMap`. Returns the number of new keys. */
export function mergeBranchKeys(
  keysMap: Record<number, Uint8Array>,
  keys: Record<string, string>,
  fromHex: (hex: string) => Uint8Array,
): number {
  let merged = 0;
  for (const [k, v] of Object.entries(keys)) {
    const n = parseInt(k, 10);
    if (!keysMap[n]) merged++;
    keysMap[n] = fromHex(v);
  }
  return merged;
}
