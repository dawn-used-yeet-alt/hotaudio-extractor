#!/usr/bin/env bun
/**
 * Staged live diagnostic. Walks the pipeline one stage at a time and reports
 * per-stage timing, so when the site changes you can see precisely which stage
 * broke and why.
 *
 *   bun scripts/verify-live.ts [url]
 *   bun scripts/verify-live.ts --full        # also download and verify the audio
 *   bun scripts/verify-live.ts --keys FILE   # resume from saved keys (offline stages only)
 *
 * Stages:
 *   1. page fetch            HTTP GET the track page
 *   2. state decrypt         decrypt the embedded __ha_state payload
 *   3. key exchange          ephemeral X25519 with the server key
 *   4. signer                compute X-Signature for the listen payload
 *   5. listen API            the server validates the signature and returns keys
 *   6. container fetch       download the .hax
 *   7. HAX0 parse            bencoded header + segment table
 *   8. key derivation        derive segment keys from the branch tree
 *   9. sample decrypt        decrypt one slice
 *  10. full decrypt          (--full) decrypt everything and check it is MP4
 *
 * The server is the authority on whether the signer is correct: stage 5
 * returning 401/`bad signature` means the capture is stale — re-run
 * `scripts/recapture.ts` and see docs/MAINTENANCE.md.
 *
 * Manual use only. Never run in CI.
 */
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { sign, PINNED_NOZZLE_VERSION } from './shim.ts';

const args = process.argv.slice(2);
const FULL = args.includes('--full');
const keysIdx = args.indexOf('--keys');
const SAVED_KEYS = keysIdx >= 0 ? args[keysIdx + 1] : null;
const PAGE_URL =
  args.find((a) => a.startsWith('http')) ??
  'https://hotaudio.net/u/Lurkydip/Welcome-to-FuckBunny-FreeUse-Cruise-Lines';

const UA = 'Mozilla/5.0';
const API = 'https://hotaudio.net';

const C = {
  reset: '\x1b[0m',
  dim: '\x1b[2m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  cyan: '\x1b[36m',
  bold: '\x1b[1m',
};

/**
 * Fetch with bounded retries, mirroring `src/http.rs`: transient network
 * errors and retryable statuses get three attempts with exponential backoff.
 * The bulk container transfer legitimately takes minutes, so it gets no
 * per-attempt timeout, but it still retries dropped sockets.
 */
const RETRYABLE = new Set([401, 408, 425, 429, 500, 502, 503, 504]);

async function fetchRetry(
  url: string,
  init: RequestInit = {},
  attempts = 3,
): Promise<Response> {
  let lastErr: unknown;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    try {
      const res = await fetch(url, init);
      if (!RETRYABLE.has(res.status) || attempt === attempts) return res;
      lastErr = new Error(`HTTP ${res.status}`);
    } catch (e) {
      lastErr = e;
      if (attempt === attempts) throw e;
    }
    await new Promise((r) => setTimeout(r, 500 * 2 ** (attempt - 1)));
  }
  throw lastErr;
}

type Stage = 'pass' | 'fail' | 'skip' | 'warn';
let failed = false;

async function stage(
  n: number,
  name: string,
  fn: () => Promise<string>,
): Promise<void> {
  const t0 = Date.now();
  process.stdout.write(`${C.dim}[${String(n).padStart(2)}] ${name.padEnd(20)}${C.reset}`);
  try {
    const note = await fn();
    const ms = Date.now() - t0;
    process.stdout.write(`${C.green}ok${C.reset}   ${C.dim}${String(ms).padStart(6)}ms${C.reset}  ${C.dim}${note}${C.reset}\n`);
  } catch (err) {
    failed = true;
    const ms = Date.now() - t0;
    process.stdout.write(`${C.red}FAIL${C.reset} ${C.dim}${String(ms).padStart(6)}ms${C.reset}  ${C.red}${errText(err)}${C.reset}\n`);
  }
}

function skip(n: number, name: string, why: string): void {
  process.stdout.write(`${C.dim}[${String(n).padStart(2)}] ${name.padEnd(20)}skip          ${C.dim}${why}${C.reset}\n`);
}

function errText(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

function hint(text: string): Error {
  const e = new Error(text) as Error & { hint?: string };
  e.hint = text;
  return e;
}

// ---------------------------------------------------------------- primitives

function b64ToBytes(b64: string): Uint8Array {
  return new Uint8Array(Buffer.from(b64, 'base64'));
}

function bytesToB64(b: Uint8Array): string {
  return Buffer.from(b).toString('base64');
}

function hexToBytes(hex: string): Uint8Array {
  return new Uint8Array(Buffer.from(hex, 'hex'));
}

function bytesToHex(b: Uint8Array): string {
  return Buffer.from(b).toString('hex');
}

async function sha256(data: Uint8Array): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.digest('SHA-256', data as BufferSource));
}

/**
 * ChaCha20-Poly1305, used by the state-decrypt and sample-decrypt stages.
 *
 * The crate itself has no JavaScript dependencies, so this diagnostic needs
 * `@noble/ciphers` installed separately (`bun add -d @noble/ciphers`).
 */
let chacha: any;
async function loadChacha() {
  if (chacha) return chacha;
  try {
    chacha = await import('@noble/ciphers/chacha.js');
    return chacha;
  } catch {}
  throw hint(
    'ChaCha20-Poly1305 unavailable. Run `bun add -d @noble/ciphers`. ' +
      'Stages 2/9/10 need it; stage 1 does not.',
  );
}

async function chachaOpen(key: Uint8Array, nonce: Uint8Array, ct: Uint8Array): Promise<Uint8Array> {
  const { chacha20poly1305 } = await loadChacha();
  return new Uint8Array(chacha20poly1305(key, nonce).decrypt(ct));
}

async function chachaSeal(key: Uint8Array, nonce: Uint8Array, pt: Uint8Array): Promise<Uint8Array> {
  const { chacha20poly1305 } = await loadChacha();
  return new Uint8Array(chacha20poly1305(key, nonce).encrypt(pt));
}

/** Minimal bencode reader for the HAX0 metadata dict. */
function decodeBencode(buf: Uint8Array, offset = 0): { value: unknown; next: number } {
  const b = buf[offset];
  if (b === 0x69) {
    let end = offset + 1;
    while (buf[end] !== 0x65 && end < buf.length) end++;
    return { value: parseInt(Buffer.from(buf.slice(offset + 1, end)).toString(), 10), next: end + 1 };
  }
  if (b === 0x64) {
    let cur = offset + 1;
    const dict: Record<string, unknown> = {};
    while (buf[cur] !== 0x65 && cur < buf.length) {
      const k = decodeBencode(buf, cur);
      const key = Buffer.from(k.value as Uint8Array).toString();
      cur = k.next;
      const v = decodeBencode(buf, cur);
      dict[key] = v.value;
      cur = v.next;
    }
    return { value: dict, next: cur + 1 };
  }
  if (b === 0x6c) {
    let cur = offset + 1;
    const items: unknown[] = [];
    while (buf[cur] !== 0x65 && cur < buf.length) {
      const it = decodeBencode(buf, cur);
      items.push(it.value);
      cur = it.next;
    }
    return { value: items, next: cur + 1 };
  }
  let colon = offset;
  while (colon < buf.length && buf[colon] >= 0x30 && buf[colon] <= 0x39) colon++;
  const len = parseInt(Buffer.from(buf.slice(offset, colon)).toString(), 10);
  const start = colon + 1;
  return { value: buf.slice(start, start + len), next: start + len };
}

const bitLen = (n: number) => 32 - Math.clz32(n >>> 0);

/** Derive a segment key by walking the key tree from the nearest known ancestor. */
async function deriveSegmentKey(
  keys: Map<number, Uint8Array>,
  segmentCount: number,
  segIdx: number,
  cache: Map<number, Uint8Array> = new Map(),
): Promise<Uint8Array> {
  const depth = bitLen(segmentCount - 1) + 1;
  const treeBase = 1 + (1 << depth);
  const leaf = treeBase + segIdx;

  let level = -1;
  let cur: Uint8Array | null = null;
  for (let a = 0; a <= depth; a++) {
    const node = leaf >>> (depth - a);
    const hit = keys.get(node);
    if (hit) {
      level = a;
      cur = hit;
      break;
    }
  }
  if (!cur) throw new Error(`Key missing in keys map for segment index ${segIdx}`);

  for (let a = level + 1; a <= depth; a++) {
    const node = leaf >>> (depth - a);
    const cached = cache.get(node);
    if (cached) {
      cur = cached;
      continue;
    }
    const merged = new Uint8Array(cur.length + 1);
    merged.set(cur, 0);
    merged[cur.length] = node & 0xff;
    const next = await sha256(merged);
    cache.set(node, next);
    cur = next;
  }
  return cur;
}

// ---------------------------------------------------------------------- stages

async function main() {
  process.stdout.write(`${C.bold}hotaudio live probe${C.reset}  ${C.dim}${C.cyan}${PAGE_URL}${C.reset}\n`);
  process.stdout.write(`${C.dim}signer pinned to player build ${PINNED_NOZZLE_VERSION}${C.reset}\n\n`);

  let html = '';
  let state: any = null;
  let haxBytes: Uint8Array | null = null;
  let haxUrl = '';
  let keys = new Map<number, Uint8Array>();
  let secret: Uint8Array | null = null;
  let clientPubHex = '';
  let title: string | undefined;

  /** Ranged GET; falls back to slicing a full 200 response. */
async function fetchRange(url: string, start: number, end: number): Promise<Uint8Array> {
  const res = await fetchRetry(url, {
    headers: { 'User-Agent': UA, Range: `bytes=${start}-${end}` },
  });
  if (res.status === 206) return new Uint8Array(await res.arrayBuffer());
  if (res.status === 200) {
    // Mirror ignored the Range header: slice locally.
    const all = new Uint8Array(await res.arrayBuffer());
    return all.slice(start, end + 1);
  }
  throw new Error(`range fetch returned ${res.status}`);
}

if (SAVED_KEYS) {
    process.stdout.write(`${C.dim}using saved keys: ${SAVED_KEYS}${C.reset}\n`);
    const env = JSON.parse(fs.readFileSync(SAVED_KEYS, 'utf8'));
    haxUrl = env.haxUrl ?? '';
    for (const [k, v] of Object.entries(env.keys)) keys.set(parseInt(k, 10), hexToBytes(v as string));
    title = env.title;
    skip(1, 'page fetch', '--keys: offline mode');
    skip(2, 'state decrypt', '--keys: offline mode');
    skip(3, 'key exchange', '--keys: offline mode');
    skip(4, 'signer', '--keys: offline mode');
    skip(5, 'listen API', '--keys: offline mode');
  } else {
    await stage(1, 'page fetch', async () => {
      const res = await fetch(PAGE_URL, { headers: { 'User-Agent': UA } });
      if (!res.ok) throw new Error(`HTTP ${res.status} — Cloudflare 403 usually means the UA was changed`);
      html = await res.text();
      return `${(html.length / 1024).toFixed(1)} KiB`;
    });

    await stage(2, 'state decrypt', async () => {
      const m = html.match(/var __ha_state = "([^"]+)"/);
      if (!m) throw new Error('no __ha_state in page HTML — the page layout changed');
      const raw = b64ToBytes(m[1]);
      if (raw.length < 48) throw new Error('__ha_state too short');
      const ct = raw.slice(0, raw.length - 32);
      const key = raw.slice(raw.length - 32);
      const plain = await chachaOpen(key, new Uint8Array(12), ct);
      state = JSON.parse(Buffer.from(plain).toString());
      if (!state.key || !state.tracks) throw new Error('state missing key/tracks');
      return `pid=${state.pid} tracks=${Object.keys(state.tracks).length}`;
    });

    await stage(3, 'key exchange', async () => {
      const { x25519 } = await import('@noble/curves/ed25519.js');
      const priv = x25519.utils.randomSecretKey();
      const pub = x25519.getPublicKey(priv);
      clientPubHex = bytesToHex(pub);
      secret = await sha256(x25519.getSharedSecret(priv, hexToBytes(state.key)));
      return `client pub ${clientPubHex.slice(0, 16)}…`;
    });

    await stage(4, 'signer', async () => {
      const tid = state.order?.length ? String(state.order[0]) : Object.keys(state.tracks)[0];
      title = state.tracks[tid]?.title;
      const payload = JSON.stringify({
        tid,
        pid: state.pid,
        key: state.tracks[tid].key,
        tick: state.tick,
        first: -1,
      });
      const sig = sign(payload, Math.floor(Date.now() / 1000));
      if (!/^9:[0-9a-f]{32}$/.test(sig)) throw new Error(`unexpected signature shape: ${sig}`);
      (globalThis as any).__payload = payload;
      (globalThis as any).__sig = sig;
      return sig;
    });

    await stage(5, 'listen API', async () => {
      const payload = (globalThis as any).__payload as string;
      const sig = (globalThis as any).__sig as string;
      const reqNonce = (await sha256(new TextEncoder().encode(sig))).slice(0, 12);
      const body = await chachaSeal(secret!, reqNonce, new TextEncoder().encode(payload));

      const listenKey = new URL(PAGE_URL).searchParams.get('key');
      const url =
        `${API}/api/v1/audio/listen` + (listenKey ? `?key=${encodeURIComponent(listenKey)}` : '');
      const res = await fetch(url, {
        method: 'POST',
        headers: {
          'X-Signature': sig,
          'X-Key': clientPubHex,
          'Content-Type': 'application/vnd.hotaudio.crypt+json',
          'User-Agent': UA,
          Origin: 'https://hotaudio.net',
          Referer: 'https://hotaudio.net/',
        },
        body,
      });

      if (res.status === 401) {
        throw hint(
          '401 bad signature — the signer capture is stale. ' +
            'Re-run `bun scripts/recapture.ts`; see docs/MAINTENANCE.md.',
        );
      }
      if (res.status === 403) throw new Error('403 — the User-Agent was probably changed');
      if (!res.ok) throw new Error(`HTTP ${res.status}: ${(await res.text()).slice(0, 200)}`);

      const ctype = res.headers.get('content-type') ?? '';
      if (!ctype.includes('hotaudio.crypt')) {
        throw new Error(`non-crypt body (${ctype}): ${(await res.text()).slice(0, 200)}`);
      }
      const respNonce = reqNonce.slice();
      respNonce[0] = (respNonce[0] + 1) & 0xff;
      const plain = await chachaOpen(secret!, respNonce, new Uint8Array(await res.arrayBuffer()));
      const json = JSON.parse(Buffer.from(plain).toString());
      haxUrl = json.url ?? '';
      if (!haxUrl) throw new Error('listen response carried no .hax url');
      for (const [k, v] of Object.entries(json.keys as Record<string, string>)) {
        keys.set(parseInt(k, 10), hexToBytes(v));
      }
      return `${Object.keys(json.keys).length} branch keys, ${haxUrl.split('/').pop()}`;
    });

  }

  if (!haxUrl) {
    skip(6, 'container probe', 'no .hax URL');
    skip(7, 'HAX0 parse', 'no .hax URL');
    skip(8, 'key derivation', 'no .hax URL');
    skip(9, 'sample decrypt', 'no .hax URL');
    skip(10, 'full decrypt', 'no .hax URL');
  } else {
    let segmentCount = 0;
    let fileLength = 0;
    let headerLength = 0;
    let offsets: number[] = [];

    // Read only what is needed: the 16-byte prefix yields headerLength, then
    // the header. Whole-container reads are slow and can drop mid-transfer.
    await stage(6, 'container probe', async () => {
      const head = await fetchRange(haxUrl, 0, 15);
      if (head.length < 16) throw new Error('short range response');
      if (Buffer.from(head.slice(0, 4)).toString() !== 'HAX0') throw new Error('bad HAX0 magic');
      const dv = new DataView(head.buffer, head.byteOffset, head.byteLength);
      fileLength = dv.getUint32(4, true);
      headerLength = dv.getUint32(8, true);
      if (headerLength < 16 || headerLength > fileLength) {
        throw new Error(`implausible headerLength ${headerLength} vs fileLength ${fileLength}`);
      }
      return `${fileLength.toLocaleString()} bytes, header ${headerLength} bytes`;
    });

    await stage(7, 'HAX0 parse', async () => {
      const header = await fetchRange(haxUrl, 0, headerLength - 1);
      haxBytes = header;
      const meta = decodeBencode(header, 16).value as Record<string, unknown>;
      segmentCount = meta.segmentCount as number;
      const table = meta.segments as Uint8Array;
      if (table.length < segmentCount * 8) {
        throw new Error('segment table shorter than segmentCount');
      }
      const tv = new DataView(table.buffer, table.byteOffset, table.byteLength);
      for (let i = 0; i < segmentCount; i++) offsets.push(tv.getUint32(i * 8, true));
      const codec = Buffer.from(meta.codec as Uint8Array).toString();
      return `${segmentCount} segments, codec=${codec}, durationMs=${meta.durationMs}`;
    });

    await stage(8, 'key derivation', async () => {
      const cache = new Map<number, Uint8Array>();
      let derived = 0;
      for (let i = 0; i < segmentCount; i++) {
        try {
          await deriveSegmentKey(keys, segmentCount, i, cache);
          derived++;
        } catch {
          break;
        }
      }
      if (derived === 0) throw new Error('no segment key derivable — branch keys look wrong');
      return derived < segmentCount
        ? `${derived}/${segmentCount} from the first branch (rest needs paging)`
        : `all ${derived} derivable from the first branch`;
    });

    await stage(9, 'sample decrypt', async () => {
      const cache = new Map<number, Uint8Array>();
      const key = await deriveSegmentKey(keys, segmentCount, 0, cache);
      const start = offsets[0];
      const end = segmentCount > 1 ? offsets[1] : fileLength;
      const slice = await fetchRange(haxUrl, start, end - 1);
      const plain = await chachaOpen(key, new Uint8Array(12), slice);
      if (plain.length < 8 || Buffer.from(plain.slice(4, 8)).toString() !== 'ftyp') {
        throw new Error('first slice did not decrypt to an ftyp box');
      }
      return `${plain.length} bytes, ftyp ok`;
    });

    if (FULL) {
      await stage(10, 'full decrypt', async () => {
        const cache = new Map<number, Uint8Array>();
        const out = path.join(os.tmpdir(), `hotaudio-probe-${Date.now()}.m4a`);
        const chunks: Buffer[] = [];
        let total = 0;
        let paged = 0;

        // Long tracks hold more segments than one branch covers, so page
        // `first:<segment>` for exactly the missing index — the same
        // miss-driven policy the downloader uses.
        const listenAt = async (first: number): Promise<Record<string, string>> => {
          if (!secret) throw new Error('need an online session to page keys');
          const base = JSON.parse((globalThis as any).__payload as string);
          const payload = JSON.stringify({ ...base, first });
          const sig = sign(payload, Math.floor(Date.now() / 1000));
          const reqNonce = (await sha256(new TextEncoder().encode(sig))).slice(0, 12);
          const body = await chachaSeal(secret, reqNonce, new TextEncoder().encode(payload));
          const listenKey = new URL(PAGE_URL).searchParams.get('key');
          const res = await fetchRetry(
            `${API}/api/v1/audio/listen` + (listenKey ? `?key=${encodeURIComponent(listenKey)}` : ''),
            {
              method: 'POST',
              headers: {
                'X-Signature': sig,
                'X-Key': clientPubHex,
                'Content-Type': 'application/vnd.hotaudio.crypt+json',
                'User-Agent': UA,
                Origin: 'https://hotaudio.net',
                Referer: 'https://hotaudio.net/',
              },
              body,
            },
          );
          if (res.status === 401) throw new Error('401 during key paging — stale capture');
          if (!res.ok) throw new Error(`key paging HTTP ${res.status}`);
          const respNonce = reqNonce.slice();
          respNonce[0] = (respNonce[0] + 1) & 0xff;
          const plain = await chachaOpen(
            secret,
            respNonce,
            new Uint8Array(await res.arrayBuffer()),
          );
          return JSON.parse(Buffer.from(plain).toString()).keys ?? {};
        };

        const res = await fetchRetry(haxUrl, { headers: { 'User-Agent': UA } });
        if (!res.ok) throw new Error(`container fetch HTTP ${res.status}`);
        const whole = new Uint8Array(await res.arrayBuffer());

        for (let i = 0; i < segmentCount; i++) {
          let key: Uint8Array;
          try {
            key = await deriveSegmentKey(keys, segmentCount, i, cache);
          } catch {
            const extra = await listenAt(i);
            const before = keys.size;
            for (const [k, v] of Object.entries(extra)) {
              keys.set(parseInt(k, 10), hexToBytes(v));
            }
            if (keys.size === before) throw new Error(`no key branch covered segment ${i}`);
            cache.clear();
            paged++;
            key = await deriveSegmentKey(keys, segmentCount, i, cache);
          }
          const start = offsets[i];
          const end = i + 1 < segmentCount ? offsets[i + 1] : fileLength;
          const plain = await chachaOpen(key, new Uint8Array(12), whole.slice(start, end));
          chunks.push(Buffer.from(plain));
          total += plain.length;
        }
        const all = Buffer.concat(chunks);
        fs.writeFileSync(out, all);
        const isMp4 = all.length > 12 && all.subarray(4, 8).toString() === 'ftyp';
        process.stdout.write(
          `     ${C.dim}wrote ${C.reset}${out}${C.dim} (${(total / 1048576).toFixed(1)} MiB, ${isMp4 ? 'valid MP4' : 'NOT MP4'})${C.reset}\n`,
        );
        if (!isMp4) throw new Error('output is not an MP4');
        return `${(total / 1048576).toFixed(1)} MiB, ${paged} page(s)`;
      });
    } else {
      skip(10, 'full decrypt', 'needs --full');
    }
  }

  console.log('');
  if (failed) {
    console.log(`${C.red}${C.bold}probe failed${C.reset}`);
    if (!SAVED_KEYS && !FULL) {
      console.log(`${C.dim}rerun with --full to exercise the container and decrypt stages${C.reset}`);
    }
    process.exit(1);
  }
  console.log(`${C.green}${C.bold}all stages ok${C.reset}`);
}

await main().catch((err) => {
  console.error(`${C.red}fatal${C.reset} ${errText(err)}`);
  process.exit(1);
});