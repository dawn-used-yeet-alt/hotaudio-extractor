#!/usr/bin/env bun
/**
 * hotaudio-download — download and decrypt a hotaudio track.
 *
 *   hotaudio-download <URL | HTML file> [--out file.m4a] [--save-keys keys.json]
 *   hotaudio-download <HTML file> --hax audio.hax --keys keys.json [--out file.m4a]
 *
 * Online mode fetches keys plus the `.hax` container and writes playable `.m4a`.
 * Offline mode decrypts a local `.hax` with saved keys without network access.
 */
import { decryptHaxBuffer, downloadHotaudioBuffer, downloadWithHandshake } from './download.ts';
import { extractHaState, loadHandshakeFromHtml } from './listen.ts';
import type { HotaudioProgress } from './types.ts';

function usage(): never {
  console.error(`Usage:
  hotaudio-download <URL | HTML file> [--out file.m4a] [--save-keys keys.json]
  hotaudio-download <HTML file> --hax audio.hax --keys keys.json [--out file.m4a]`);
  process.exit(1);
}

function arg(flag: string): string | null {
  const i = process.argv.indexOf(flag);
  return i >= 0 && i + 1 < process.argv.length ? process.argv[i + 1] : null;
}

function sanitize(name: string): string {
  return name.replace(/[\\/*?:"<>|]/g, '').trim() || 'hotaudio-track';
}

const source = process.argv[2];
if (!source || source.startsWith('-')) usage();

const outArg = arg('--out');
const saveKeysPath = arg('--save-keys');
const haxPath = arg('--hax');
const keysArg = arg('--keys');

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

if (haxPath && keysArg) {
  let html: string;
  try {
    html = await fs.readFile(source, 'utf8');
  } catch {
    console.error(`HTML file not found: ${source}`);
    process.exit(1);
  }
  if (!extractHaState(html)) {
    console.error('Could not find __ha_state in HTML file');
    process.exit(1);
  }
  let keys: Record<string, string>;
  try {
    keys = JSON.parse(
      keysArg.trim().startsWith('{') ? keysArg : await fs.readFile(keysArg, 'utf8'),
    );
  } catch {
    console.error('--keys must be a JSON file path or inline JSON object');
    process.exit(1);
  }
  const haxBytes = new Uint8Array(await fs.readFile(haxPath));
  const { buffer } = await decryptHaxBuffer(haxBytes, keys, onProgress);
  const out = outArg ?? `${sanitize(path.basename(haxPath, path.extname(haxPath)))}.m4a`;
  await fs.writeFile(out, buffer);
  console.log(`Saved ${out}`);
  process.exit(0);
}

let title = 'hotaudio-track';
let buffer: Uint8Array;
let keys: Record<string, string>;

if (/^https?:\/\//i.test(source)) {
  const res = await downloadHotaudioBuffer(source, { onProgress });
  buffer = res.buffer;
  keys = res.keys;
  if (res.title) title = res.title;
} else {
  const html = await fs.readFile(source, 'utf8').catch(() => null);
  if (!html) {
    console.error(`HTML file not found: ${source}`);
    process.exit(1);
  }
  const handshake = await loadHandshakeFromHtml(html);
  if (!handshake) {
    console.error('Could not decrypt __ha_state from HTML file');
    process.exit(1);
  }
  const res = await downloadWithHandshake(handshake, { onProgress });
  buffer = res.buffer;
  keys = res.keys;
  if (res.title) title = res.title;
}

if (saveKeysPath) {
  await fs.writeFile(saveKeysPath, JSON.stringify(keys!, null, 2));
  console.error(`Keys saved to ${saveKeysPath}`);
}

const out = outArg ?? `${sanitize(title)}.m4a`;
await fs.writeFile(out, buffer!);
console.log(`Saved ${path.resolve(out)}`);
