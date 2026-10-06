#!/usr/bin/env bun
/**
 * live-probe — staged connectivity/drift check against the real hotaudio.net.
 *
 * Walks the extraction pipeline one stage at a time with timing, byte
 * counts, and failure diagnoses, so a breakage can be pinpointed to the
 * exact stage (page markup, state crypto, signer, listen API, HAX0 layout,
 * key tree, segment decrypt). Manual use only: never add to CI or `bun test`
 * (see AGENTS.md).
 *
 *   bun ./scripts/live-probe.ts <URL> [--full] [--segments N] [--out report.json]
 */
import { downloadHotaudioBuffer } from '../src/download.ts';
import { decryptSegmentSlice, deriveSegmentKey, parseHax0Header } from '../src/hax_decoder.ts';
import {
  extractHaState,
  listenRequest,
  loadHandshakeFromHtml,
  mergeBranchKeys,
  type HotaudioHandshake,
} from '../src/listen.ts';
import { hexToBytes, bytesToHex } from '../src/crypto.ts';
import { signHotaudioPayload } from '../src/signer.ts';
import { HOTAUDIO_UA, HOTAUDIO_API_BASE } from '../src/constants.ts';
import { isHotaudioUrl } from '../src/index.ts';

interface ProbeOptions {
  url: string;
  full: boolean;
  sampleSegments: number;
  timeoutMs: number;
  userAgent: string;
  apiBase: string;
  outPath: string | null;
  json: boolean;
  noColor: boolean;
}

type StageStatus = 'pass' | 'fail' | 'skip';

interface StageResult {
  name: string;
  status: StageStatus;
  ms: number;
  detail: string;
  hint?: string;
}

const ANSI = {
  reset: '\x1b[0m',
  bold: '\x1b[1m',
  dim: '\x1b[2m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  cyan: '\x1b[36m',
};

function usage(): never {
  console.error(`Usage:
  live-probe <track URL> [options]

Options:
  --full            Also run a full download + decrypt (default: samples only)
  --segments N      Segments to key-check/decrypt in the sample stage (default: 3)
  --timeout MS      Per-request timeout in ms (default: 30000)
  --user-agent UA   Override the request User-Agent
  --api-base URL    Override the listen API base
  --out PATH        Write the JSON report to a file (implies structured output)
  --json            Print the JSON report to stdout instead of human logs
  --no-color        Disable ANSI colors
  --help            Show this help

Exit codes: 0 all stages passed, 1 a stage failed, 2 usage error.`);
  process.exit(2);
}

function parseArgs(argv: string[]): ProbeOptions {
  const positional: string[] = [];
  const opts: ProbeOptions = {
    url: '',
    full: false,
    sampleSegments: 3,
    timeoutMs: 30000,
    userAgent: HOTAUDIO_UA,
    apiBase: HOTAUDIO_API_BASE,
    outPath: null,
    json: false,
    noColor: false,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--help' || a === '-h') usage();
    else if (a === '--full') opts.full = true;
    else if (a === '--json') opts.json = true;
    else if (a === '--no-color') opts.noColor = true;
    else if (a === '--segments') opts.sampleSegments = num(argv[++i], '--segments');
    else if (a === '--timeout') opts.timeoutMs = num(argv[++i], '--timeout');
    else if (a === '--user-agent') opts.userAgent = str(argv[++i], '--user-agent');
    else if (a === '--api-base') opts.apiBase = str(argv[++i], '--api-base');
    else if (a === '--out') opts.outPath = str(argv[++i], '--out');
    else if (a.startsWith('-')) {
      console.error(`Unknown flag: ${a}`);
      usage();
    } else positional.push(a);
  }
  if (positional.length !== 1) {
    console.error('Expected exactly one track URL.');
    usage();
  }
  opts.url = positional[0];
  if (opts.sampleSegments < 1) {
    console.error('--segments must be >= 1.');
    process.exit(2);
  }
  return opts;
}

function num(v: string | undefined, flag: string): number {
  const n = v === undefined ? NaN : Number(v);
  if (!Number.isFinite(n)) {
    console.error(`${flag} expects a number.`);
    process.exit(2);
  }
  return n;
}

function str(v: string | undefined, flag: string): string {
  if (v === undefined) {
    console.error(`${flag} expects a value.`);
    process.exit(2);
  }
  return v;
}

async function timed<T>(fn: () => Promise<T>): Promise<{ value: T; ms: number }> {
  const t0 = performance.now();
  const value = await fn();
  return { value, ms: Math.round(performance.now() - t0) };
}

async function fetchTimed(
  url: string,
  init: RequestInit,
  timeoutMs: number,
): Promise<{ res: Response; ms: number }> {
  const { value: res, ms } = await timed(() =>
    fetch(url, { ...init, signal: AbortSignal.timeout(timeoutMs) }),
  );
  return { res, ms };
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / 1024 / 1024).toFixed(2)} MiB`;
}

export async function runProbe(opts: ProbeOptions): Promise<{ stages: StageResult[]; ok: boolean }> {
  const c = opts.noColor || !process.stderr.isTTY ? blankAnsi() : ANSI;
  const stages: StageResult[] = [];
  const log = opts.json
    ? () => {}
    : (s: string) => console.error(s);

  log(`${c.bold}live-probe${c.reset} ${opts.url}`);
  if (!isHotaudioUrl(opts.url)) {
    log(`${c.yellow}warn:${c.reset} URL does not match the canonical share pattern; continuing anyway.`);
  }

  async function stage(name: string, fn: () => Promise<{ detail: string }>): Promise<boolean> {
    log(`${c.cyan}▶${c.reset} ${name}`);
    const t0 = performance.now();
    try {
      const { detail } = await fn();
      const ms = Math.round(performance.now() - t0);
      stages.push({ name, status: 'pass', ms, detail });
      log(`${c.green}✔${c.reset} ${name} ${c.dim}(${ms}ms)${c.reset} — ${detail}`);
      return true;
    } catch (err) {
      const ms = Math.round(performance.now() - t0);
      const message = err instanceof Error ? err.message : String(err);
      const hint = hintOf(name, message);
      stages.push({ name, status: 'fail', ms, detail: message, hint });
      log(`${c.red}✘${c.reset} ${name} ${c.dim}(${ms}ms)${c.reset} — ${message}`);
      if (hint) log(`  ${c.yellow}hint:${c.reset} ${hint}`);
      return false;
    }
  }

  function skipRemaining(from: number, names: string[]): void {
    for (let i = from; i < names.length; i++) {
      stages.push({ name: names[i], status: 'skip', ms: 0, detail: 'skipped (earlier stage failed)' });
      log(`${c.dim}○ ${names[i]} — skipped (earlier stage failed)${c.reset}`);
    }
  }

  let html = '';
  let handshake: HotaudioHandshake | null = null;
  let haxUrl = '';
  let firstKeys: Record<string, string> = {};
  let headerBytes: Uint8Array | null = null;

  const plan = [
    'page fetch',
    'page state decrypt',
    'key exchange',
    'request signer',
    'listen (first=-1)',
    'hax header fetch + parse',
    `sample key derivation (${opts.sampleSegments} segs)`,
    `sample decrypt (${opts.sampleSegments} segs)`,
    ...(opts.full ? ['full download + decrypt'] : []),
  ];

  // 1. Page fetch.
  if (!(await stage(plan[0], async () => {
    const { res, ms } = await fetchTimed(
      opts.url,
      { headers: { 'User-Agent': opts.userAgent } },
      opts.timeoutMs,
    ).catch((err) => {
      throw new Error(`page request failed: ${describeFetchError(err)}`);
    });
    if (!res.ok) throw new Error(`page returned HTTP ${res.status}`);
    html = await res.text();
    const state = extractHaState(html);
    return {
      detail: `HTTP 200 in ${ms}ms, ${fmtBytes(html.length)}, __ha_state ${state ? `present (${fmtBytes(state.length)})` : 'MISSING'}`,
    };
  }))) { skipRemaining(1, plan); return finish(); }

  // 2. State decrypt + handshake (includes key exchange, staged separately below).
  if (!(await stage(plan[1], async () => {
    const raw = extractHaState(html);
    if (!raw) throw new Error('__ha_state not found in page HTML');
    handshake = await loadHandshakeFromHtml(html, opts.apiBase);
    if (!handshake) throw new Error('__ha_state present but failed to decrypt');
    const trackCount = Object.keys(handshake.state.tracks).length;
    return {
      detail: `pid=${handshake.state.pid} tick=${handshake.state.tick} tracks=${trackCount} tid=${handshake.tid} title=${JSON.stringify(handshake.track.title)} serverPub=${handshake.state.key}`,
    };
  }))) { skipRemaining(2, plan); return finish(); }

  // 3. Key exchange timing (re-run explicitly to isolate it).
  if (!(await stage(plan[2], async () => {
    const { ms } = await timed(async () => {
      const h = await loadHandshakeFromHtml(html, opts.apiBase);
      if (!h) throw new Error('handshake rebuild failed');
      handshake = h;
    });
    return { detail: `ephemeral X25519 session established in ${ms}ms, clientPub=${handshake!.clientPubHex}` };
  }))) { skipRemaining(3, plan); return finish(); }

  // 4. Signer.
  if (!(await stage(plan[3], async () => {
    const probe = JSON.stringify({ tid: 'probe', pid: 'probe', key: 'probe', tick: 'probe', first: -1 });
    const { value: sig, ms } = await timed(async () => signHotaudioPayload(probe));
    if (!sig || sig.length < 16) throw new Error(`signer returned suspicious value (length ${sig?.length ?? 0})`);
    return { detail: `signature computed in ${ms}ms, sig=${sig}` };
  }))) { skipRemaining(4, plan); return finish(); }

  // 5. Initial listen request.
  if (!(await stage(plan[4], async () => {
    const { value: initial, ms } = await timed(() =>
      listenRequest(handshake!, -1, {
        userAgent: opts.userAgent,
        apiBase: opts.apiBase,
      }).catch((err) => {
        throw new Error(`listen API failed: ${describeFetchError(err)}`);
      }),
    );
    if (!initial.url) throw new Error('listen response contained no .hax url');
    haxUrl = initial.url;
    firstKeys = initial.keys;
    const n = Object.keys(firstKeys).length;
    if (n === 0) throw new Error('listen response contained zero keys');
    return { detail: `HTTP 200 in ${ms}ms, keys=${n} ${JSON.stringify(firstKeys)}, hax=${haxUrl}` };
  }))) { skipRemaining(5, plan); return finish(); }

  // 6. HAX header fetch + parse.
  if (!(await stage(plan[5], async () => {
    const head = await fetchRange(haxUrl, 0, 15, opts).catch((err) => {
      throw new Error(`range fetch failed: ${describeFetchError(err)}`);
    });
    if (head.length < 16) throw new Error(`short header prefix (${head.length} bytes)`);
    const view = new DataView(head.buffer, head.byteOffset, head.byteLength);
    const fileLength = view.getUint32(4, true);
    const headerLength = view.getUint32(8, true);
    if (headerLength < 16 || headerLength > fileLength) {
      throw new Error(`implausible lengths (header=${headerLength}, file=${fileLength})`);
    }
    headerBytes = await fetchRange(haxUrl, 0, headerLength - 1, opts).catch((err) => {
      throw new Error(`header fetch failed: ${describeFetchError(err)}`);
    });
    const hax = parseHax0Header(headerBytes);
    return {
      detail: `file=${fmtBytes(hax.fileLength)} header=${fmtBytes(hax.headerLength)} codec=${hax.codec} duration=${(hax.durationMs / 1000).toFixed(1)}s segments=${hax.segmentCount}`,
    };
  }))) { skipRemaining(6, plan); return finish(); }

  // 7. Sample key derivation (with paging, like the real downloader).
  const keysMap: Record<number, Uint8Array> = {};
  for (const [k, v] of Object.entries(firstKeys)) keysMap[parseInt(k, 10)] = hexToBytes(v);
  const nodeCache = new Map<number, Uint8Array>();
  const hax = parseHax0Header(headerBytes!);
  const sampleN = Math.min(opts.sampleSegments, hax.segmentCount);
  let paged = 0;
  if (!(await stage(plan[6], async () => {
    const derived: string[] = [];
    for (let i = 0; i < sampleN; i++) {
      let key: Uint8Array;
      try {
        key = await deriveSegmentKey(keysMap, hax.segmentCount, i, nodeCache);
      } catch (err) {
        if (!(err instanceof Error) || !err.message.startsWith('Key missing in keys map')) throw err;
        const extra = await listenRequest(handshake!, i, { userAgent: opts.userAgent, apiBase: opts.apiBase });
        const merged = mergeBranchKeys(keysMap, extra.keys, hexToBytes);
        if (merged === 0) throw new Error(`segment ${i}: key missing and paging returned no new keys`);
        paged++;
        nodeCache.clear();
        key = await deriveSegmentKey(keysMap, hax.segmentCount, i, nodeCache);
      }
      derived.push(`${i}=${bytesToHex(key)}`);
    }
    return { detail: `${sampleN}/${hax.segmentCount} segment keys derived${paged ? ` (${paged} paging round-trips)` : ' (first branch covered all)'}: ${derived.join(' ')}` };
  }))) { skipRemaining(7, plan); return finish(); }

  // 8. Sample decrypt + ftyp check.
  if (!(await stage(plan[7], async () => {
    for (let i = 0; i < sampleN; i++) {
      const nextOff = i + 1 < hax.segmentCount ? hax.segments[i + 1].offset : hax.fileLength;
      const slice = await fetchRange(haxUrl, hax.segments[i].offset, nextOff - 1, opts);
      const key = await deriveSegmentKey(keysMap, hax.segmentCount, i, nodeCache);
      const plain = decryptSegmentSlice(slice, key);
      if (i === 0 && !isFtyp(plain)) {
        throw new Error('first decrypted segment is not an ftyp box — decrypt output looks wrong');
      }
    }
    return { detail: `${sampleN} segments decrypted, first segment starts with ftyp` };
  }))) { skipRemaining(8, plan); return finish(); }

  // 9. Full download (opt-in).
  if (opts.full) {
    await stage(plan[8], async () => {
      let lastPhase = '';
      const { value: res, ms } = await timed(() =>
        downloadHotaudioBuffer(opts.url, {
          userAgent: opts.userAgent,
          apiBase: opts.apiBase,
          onProgress: ({ phase, loaded, total }) => {
            lastPhase = `${phase} ${loaded}/${total}`;
          },
        }),
      );
      if (!isFtyp(res.buffer.subarray(0, 64))) {
        throw new Error('downloaded bytes do not start with ftyp — decrypt output looks wrong');
      }
      const speed = res.buffer.length / Math.max(ms / 1000, 0.001);
      return {
        detail: `${fmtBytes(res.buffer.length)} in ${(ms / 1000).toFixed(1)}s (${fmtBytes(speed)}/s), ${res.segmentCount} segments, last progress: ${lastPhase || 'n/a'}`,
      };
    });
  }

  return finish();

  function finish(): { stages: StageResult[]; ok: boolean } {
    const ok = stages.every((s) => s.status !== 'fail');
    if (!opts.json) {
      const passed = stages.filter((s) => s.status === 'pass').length;
      const failed = stages.filter((s) => s.status === 'fail').length;
      const skipped = stages.filter((s) => s.status === 'skip').length;
      log(`${c.bold}result:${c.reset} ${passed} passed, ${failed} failed, ${skipped} skipped`);
    }
    return { stages, ok };
  }
}

async function fetchRange(
  url: string,
  start: number,
  end: number,
  opts: ProbeOptions,
): Promise<Uint8Array> {
  const { res } = await fetchTimed(
    url,
    { headers: { 'User-Agent': opts.userAgent, Range: `bytes=${start}-${end}` } },
    opts.timeoutMs,
  );
  if (!res.ok || (res.status !== 206 && res.status !== 200)) {
    throw new Error(`range fetch returned HTTP ${res.status}`);
  }
  const bytes = new Uint8Array(await res.arrayBuffer());
  return res.status === 200 ? bytes.subarray(start, end + 1) : bytes;
}

function isFtyp(plain: Uint8Array): boolean {
  if (plain.length < 8) return false;
  return (
    plain[4] === 0x66 && plain[5] === 0x74 && plain[6] === 0x79 && plain[7] === 0x70
  );
}

function describeFetchError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

function hintOf(stage: string, message: string): string | undefined {
  if (stage === 'page fetch') {
    if (/403/.test(message)) {
      return 'Cloudflare challenge. Confirm the default bare Mozilla/5.0 UA is used; Chrome UAs are blocked. The site may also be rate-limiting this IP.';
    }
    if (/404/.test(message)) return 'Track URL not found. Check the slug, or the track was removed.';
    if (/abort|timeout|Timeout|fetch failed|ENOTFOUND|EAI_AGAIN/i.test(message)) {
      return 'Network-level failure (DNS/timeout). Check connectivity and --timeout.';
    }
  }
  if (stage === 'page state decrypt') {
    if (/__ha_state not found/.test(message)) {
      return 'Page markup changed: the `var __ha_state = "..."` embed is gone or renamed. Update extractHaState in src/listen.ts.';
    }
    return 'Page crypto changed: ChaCha20-Poly1305 state decrypt (trailing-32-byte key, zero nonce) no longer matches. See decryptHotaudioState in src/crypto.ts.';
  }
  if (stage === 'request signer') {
    return 'The pinned nozzle.js build likely drifted. Re-capture nozzle.js + env hashes and bump PINNED_NOZZLE_VERSION (see docs/ARCHITECTURE.md).';
  }
  if (stage.startsWith('listen')) {
    if (/40[13]/.test(message)) {
      return 'Listen API rejected the request: stale tick, bad signature, or blocked UA. Re-run to rule out a stale tick; then suspect signer drift.';
    }
  }
  if (stage.startsWith('sample key derivation')) {
    return 'Key-tree layout or paging protocol changed. See deriveSegmentKey in src/hax_decoder.ts and docs/ARCHITECTURE.md.';
  }
  if (stage.startsWith('sample decrypt') || stage.startsWith('full download')) {
    return 'Decrypt output is wrong but keys derived: suspect nonce/key-derivation drift in decryptSegmentSlice/deriveSegmentKey.';
  }
  return undefined;
}

function blankAnsi(): typeof ANSI {
  return { reset: '', bold: '', dim: '', red: '', green: '', yellow: '', cyan: '' };
}

async function main(): Promise<void> {
  const opts = parseArgs(process.argv.slice(2));
  const startedAt = new Date().toISOString();
  const { stages, ok } = await runProbe(opts);
  const report = {
    tool: 'hotaudio-extractor live-probe',
    url: opts.url,
    startedAt,
    options: {
      full: opts.full,
      sampleSegments: opts.sampleSegments,
      timeoutMs: opts.timeoutMs,
      apiBase: opts.apiBase,
    },
    stages,
    ok,
  };
  if (opts.json || opts.outPath) {
    const text = JSON.stringify(report, null, 2);
    if (opts.outPath) {
      const fs = await import('node:fs/promises');
      await fs.writeFile(opts.outPath, text);
      if (!opts.json) console.error(`Report written to ${opts.outPath}`);
    } else {
      console.log(text);
    }
  }
  process.exit(ok ? 0 : 1);
}

await main();
