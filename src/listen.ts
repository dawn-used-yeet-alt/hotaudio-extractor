import { chacha20poly1305 } from '@noble/ciphers/chacha.js';
import { decryptHotaudioState, performKeyExchange, sha256 } from './crypto.ts';
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
}

export interface HandshakeOptions {
  userAgent?: string;
  fetchFn?: HotaudioFetch;
  apiBase?: string;
}

/** Extract the raw `__ha_state` value from track page HTML, or null when absent. */
export function extractHaState(html: string): string | null {
  const m = html.match(/var __ha_state = "([^"]+)"/);
  return m?.[1] ?? null;
}

/** Build a handshake from already-fetched page HTML. */
export async function loadHandshakeFromHtml(
  html: string,
  apiBase: string = HOTAUDIO_API_BASE,
): Promise<HotaudioHandshake | null> {
  const stateB64 = extractHaState(html);
  if (!stateB64) return null;
  let state: HotaudioState;
  try {
    state = decryptHotaudioState(stateB64);
  } catch {
    return null;
  }
  return buildHandshake(state, apiBase);
}

async function buildHandshake(
  state: HotaudioState,
  apiBase: string,
): Promise<HotaudioHandshake | null> {
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
  const pageRes = await fetchFn(pageUrl, { headers: { 'User-Agent': userAgent } });
  if (!pageRes.ok) return null;
  return loadHandshakeFromHtml(await pageRes.text(), apiBase);
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

  const listenRes = await fetchFn(`${apiBase}/api/v1/audio/listen`, {
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
  });
  if (!listenRes.ok) {
    throw new Error(`Hotaudio listen API returned ${listenRes.status} for first=${first}`);
  }

  // Responses use the same secret with the first nonce byte incremented,
  // which separates the request and response nonce domains.
  const respBuf = new Uint8Array(await listenRes.arrayBuffer());
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
