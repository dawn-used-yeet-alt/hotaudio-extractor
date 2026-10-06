#!/usr/bin/env bun
/**
 * hotaudio-download — download and decrypt a hotaudio track.
 *
 *   hotaudio-download <URL | HTML file> [--out file.m4a] [--save-keys keys.json] [--track id] [--api-base URL] [--keys resume.json]
 *   hotaudio-download <URL | HTML file> --list-tracks
 *   hotaudio-download --keys keys.json [--hax-url URL] [--out file.m4a]
 *   hotaudio-download --hax audio.hax --keys keys.json [--out file.m4a]
 *
 * Online mode runs the full pipeline and writes playable `.m4a`.
 * Cached mode reuses a saved-keys envelope (no page fetch, no listen calls).
 * Offline mode decrypts a local `.hax` with saved keys (no network).
 */
import {
  decryptHaxBuffer,
  downloadHaxBuffer,
  downloadHotaudioBuffer,
  downloadWithHandshake,
  parseSavedKeys,
  type SavedHotaudioKeys,
} from './download.ts';
import { loadHandshakeFromHtml, fetchHotaudioTracks, listHotaudioTracks } from './listen.ts';
import { HOTAUDIO_API_BASE } from './constants.ts';
import type { HotaudioProgress } from './types.ts';

function usage(exitCode: number = 1): never {
  console.error(`Usage:
  hotaudio-download <URL | HTML file> [--out file.m4a] [--save-keys keys.json] [--track id] [--api-base URL] [--keys resume.json]
  hotaudio-download <URL | HTML file> --list-tracks
  hotaudio-download --keys keys.json [--hax-url URL] [--out file.m4a]
  hotaudio-download --hax audio.hax --keys keys.json [--out file.m4a]`);
  process.exit(exitCode);
}

if (process.argv.includes('--help') || process.argv.includes('-h')) usage(0);

function arg(flag: string): string | null {
  const i = process.argv.indexOf(flag);
  return i >= 0 && i + 1 < process.argv.length ? process.argv[i + 1] : null;
}

function sanitize(name: string): string {
  return name.replace(/[\\/*?:"<>|]/g, '').trim() || 'hotaudio-track';
}

const rawSource = process.argv[2];
const source = rawSource && !rawSource.startsWith('-') ? rawSource : null;

const outArg = arg('--out');
const saveKeysPath = arg('--save-keys');
const haxPath = arg('--hax');
const haxUrlArg = arg('--hax-url');
const keysArg = arg('--keys');
const trackId = arg('--track');
const apiBaseArg = arg('--api-base');
const apiBase = apiBaseArg ?? undefined;
const listTracks = process.argv.includes('--list-tracks');

const fs = await import('node:fs/promises');
const path = await import('node:path');

function onProgress({ phase, loaded, total }: HotaudioProgress): void {
  if (total <= 0) return;
  if (phase === 'fetching') {
    process.stderr.write(`\r${phase}: ${((loaded / total) * 100).toFixed(1)}%`);
  } else if (phase === 'decrypting') {
    process.stderr.write(`\r${phase}: ${loaded}/${total} segments`);
  }
  if (loaded === total) process.stderr.write('\n');
}

async function loadKeys(): Promise<{ keys: Record<string, string>; haxUrl?: string; title?: string }> {
  if (!keysArg) {
    console.error('Missing --keys <file | JSON>');
    usage();
  }
  let text: string;
  try {
    text = keysArg.trim().startsWith('{') ? keysArg : await fs.readFile(keysArg, 'utf8');
  } catch {
    console.error('--keys must be a JSON file path or inline JSON object');
    process.exit(1);
  }
  try {
    return parseSavedKeys(text);
  } catch (err) {
    console.error(err instanceof Error ? err.message : err);
    process.exit(1);
  }
}

async function saveKeys(  pageUrl: string | undefined,
  haxUrl: string,
  title: string | undefined,
  keys: Record<string, string>,
): Promise<void> {
  if (!saveKeysPath) return;
  const envelope: SavedHotaudioKeys = {
    version: 1,
    pageUrl: pageUrl ?? '',
    haxUrl,
    title,
    savedAt: new Date().toISOString(),
    keys,
  };
  await fs.writeFile(saveKeysPath, JSON.stringify(envelope, null, 2));
  console.error(`Keys saved to ${saveKeysPath}`);
}

// ---- Track listing: every track on the page, in page order ----
if (listTracks) {
  if (!source) usage();
  const tracks = /^https?:\/\//i.test(source)
    ? await fetchHotaudioTracks(source, { apiBase })
    : await fs
      .readFile(source, 'utf8')
      .then((html) => listHotaudioTracks(html))
      .catch(() => null);
  if (!tracks || tracks.length === 0) {
    console.error('No tracks found (missing or undecryptable page state)');
    process.exit(1);
  }
  for (const t of tracks) console.log(`${t.id}\t${t.title}`);
  process.exit(0);
}

// ---- Offline mode: local .hax + saved keys, no network ----
if (haxPath) {
  const { keys, title } = await loadKeys();
  let haxBytes: Uint8Array;
  try {
    haxBytes = new Uint8Array(await fs.readFile(haxPath));
  } catch {
    console.error(`HAX file not found: ${haxPath}`);
    process.exit(1);
  }
  const { buffer } = await decryptHaxBuffer(haxBytes, keys, onProgress).catch((err: unknown) => {
    console.error(`Decrypt failed: ${err instanceof Error ? err.message : err}`);
    process.exit(1);
  });
  await saveKeys(undefined, '', title, keys);
  const out = outArg ?? `${sanitize(title ?? path.basename(haxPath, path.extname(haxPath)))}.m4a`;
  await fs.writeFile(out, buffer!);
  console.log(`Saved ${out}`);
  process.exit(0);
}

// ---- Cached mode: saved keys (+ envelope hax URL), no page/listen ----
if (keysArg && !source) {
  const { keys, haxUrl, title } = await loadKeys();
  const url = haxUrlArg ?? haxUrl;
  if (!url) {
    console.error('Keys file has no .hax URL — pass --hax-url URL or use online mode');
    process.exit(1);
  }
  const { buffer } = await downloadHaxBuffer(url, keys, { onProgress }).catch((err: unknown) => {
    console.error(`Cached download failed: ${err instanceof Error ? err.message : err}`);
    process.exit(1);
  });
  await saveKeys(undefined, url, title, keys);
  const out = outArg ?? `${sanitize(title ?? 'hotaudio-track')}.m4a`;
  await fs.writeFile(out, buffer!);
  console.log(`Saved ${path.resolve(out)}`);
  process.exit(0);
}

// ---- Online mode ----
if (!source) usage();

let title = 'hotaudio-track';
let buffer: Uint8Array;
let keys: Record<string, string>;
let haxUrl = '';

if (/^https?:\/\//i.test(source)) {
  const seed = keysArg ? (await loadKeys()).keys : undefined;
  const res = await downloadHotaudioBuffer(source, { onProgress, trackId: trackId ?? undefined, apiBase, initialKeys: seed });
  buffer = res.buffer;
  keys = res.keys;
  haxUrl = res.haxUrl;
  if (res.title) title = res.title;
} else {
  const html = await fs.readFile(source, 'utf8').catch(() => null);
  if (!html) {
    console.error(`HTML file not found: ${source}`);
    process.exit(1);
  }
  const handshake = await loadHandshakeFromHtml(html, apiBase ?? HOTAUDIO_API_BASE, trackId ?? undefined);
  if (!handshake) {
    console.error(trackId ? `Track not found in HTML file: ${trackId}` : 'Could not decrypt __ha_state from HTML file');
    process.exit(1);
  }
  const res = await downloadWithHandshake(handshake, { onProgress, initialKeys: keysArg ? (await loadKeys()).keys : undefined });
  buffer = res.buffer;
  keys = res.keys;
  haxUrl = res.haxUrl;
  if (res.title) title = res.title;
}

await saveKeys(/^https?:\/\//i.test(source) ? source : undefined, haxUrl, title, keys!);

const out = outArg ?? `${sanitize(title)}.m4a`;
await fs.writeFile(out, buffer!);
console.log(`Saved ${path.resolve(out)}`);
