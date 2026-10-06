#!/usr/bin/env bun
import {
  HOTAUDIO_API_BASE,
  decryptHaxBuffer,
  downloadHaxBuffer,
  downloadHotaudioBuffer,
  downloadWithHandshake,
  fetchHotaudioTracks,
  listHotaudioTracks,
  loadHandshakeFromHtml,
  parseSavedKeys
} from "./chunk-GJ54U5UL.js";

// src/cli.ts
function usage(exitCode = 1) {
  console.error(`Usage:
  hotaudio-download <URL | HTML file> [--out file.m4a] [--save-keys keys.json] [--track id] [--api-base URL] [--keys resume.json]
  hotaudio-download <URL | HTML file> --list-tracks
  hotaudio-download --keys keys.json [--hax-url URL] [--out file.m4a]
  hotaudio-download --hax audio.hax --keys keys.json [--out file.m4a]`);
  process.exit(exitCode);
}
if (process.argv.includes("--help") || process.argv.includes("-h")) usage(0);
function arg(flag) {
  const i = process.argv.indexOf(flag);
  return i >= 0 && i + 1 < process.argv.length ? process.argv[i + 1] : null;
}
function sanitize(name) {
  return name.replace(/[\\/*?:"<>|]/g, "").trim() || "hotaudio-track";
}
var rawSource = process.argv[2];
var source = rawSource && !rawSource.startsWith("-") ? rawSource : null;
var outArg = arg("--out");
var saveKeysPath = arg("--save-keys");
var haxPath = arg("--hax");
var haxUrlArg = arg("--hax-url");
var keysArg = arg("--keys");
var trackId = arg("--track");
var apiBaseArg = arg("--api-base");
var apiBase = apiBaseArg ?? void 0;
var listTracks = process.argv.includes("--list-tracks");
var fs = await import("fs/promises");
var path = await import("path");
function onProgress({ phase, loaded, total }) {
  if (total <= 0) return;
  if (phase === "fetching") {
    process.stderr.write(`\r${phase}: ${(loaded / total * 100).toFixed(1)}%`);
  } else if (phase === "decrypting") {
    process.stderr.write(`\r${phase}: ${loaded}/${total} segments`);
  }
  if (loaded === total) process.stderr.write("\n");
}
async function loadKeys() {
  if (!keysArg) {
    console.error("Missing --keys <file | JSON>");
    usage();
  }
  let text;
  try {
    text = keysArg.trim().startsWith("{") ? keysArg : await fs.readFile(keysArg, "utf8");
  } catch {
    console.error("--keys must be a JSON file path or inline JSON object");
    process.exit(1);
  }
  try {
    return parseSavedKeys(text);
  } catch (err) {
    console.error(err instanceof Error ? err.message : err);
    process.exit(1);
  }
}
async function saveKeys(pageUrl, haxUrl2, title2, keys2) {
  if (!saveKeysPath) return;
  const envelope = {
    version: 1,
    pageUrl: pageUrl ?? "",
    haxUrl: haxUrl2,
    title: title2,
    savedAt: (/* @__PURE__ */ new Date()).toISOString(),
    keys: keys2
  };
  await fs.writeFile(saveKeysPath, JSON.stringify(envelope, null, 2));
  console.error(`Keys saved to ${saveKeysPath}`);
}
if (listTracks) {
  if (!source) usage();
  const tracks = /^https?:\/\//i.test(source) ? await fetchHotaudioTracks(source, { apiBase }) : await fs.readFile(source, "utf8").then((html) => listHotaudioTracks(html)).catch(() => null);
  if (!tracks || tracks.length === 0) {
    console.error("No tracks found (missing or undecryptable page state)");
    process.exit(1);
  }
  for (const t of tracks) console.log(`${t.id}	${t.title}`);
  process.exit(0);
}
if (haxPath) {
  const { keys: keys2, title: title2 } = await loadKeys();
  let haxBytes;
  try {
    haxBytes = new Uint8Array(await fs.readFile(haxPath));
  } catch {
    console.error(`HAX file not found: ${haxPath}`);
    process.exit(1);
  }
  const { buffer: buffer2 } = await decryptHaxBuffer(haxBytes, keys2, onProgress).catch((err) => {
    console.error(`Decrypt failed: ${err instanceof Error ? err.message : err}`);
    process.exit(1);
  });
  await saveKeys(void 0, "", title2, keys2);
  const out2 = outArg ?? `${sanitize(title2 ?? path.basename(haxPath, path.extname(haxPath)))}.m4a`;
  await fs.writeFile(out2, buffer2);
  console.log(`Saved ${out2}`);
  process.exit(0);
}
if (keysArg && !source) {
  const { keys: keys2, haxUrl: haxUrl2, title: title2 } = await loadKeys();
  const url = haxUrlArg ?? haxUrl2;
  if (!url) {
    console.error("Keys file has no .hax URL \u2014 pass --hax-url URL or use online mode");
    process.exit(1);
  }
  const { buffer: buffer2 } = await downloadHaxBuffer(url, keys2, { onProgress }).catch((err) => {
    console.error(`Cached download failed: ${err instanceof Error ? err.message : err}`);
    process.exit(1);
  });
  await saveKeys(void 0, url, title2, keys2);
  const out2 = outArg ?? `${sanitize(title2 ?? "hotaudio-track")}.m4a`;
  await fs.writeFile(out2, buffer2);
  console.log(`Saved ${path.resolve(out2)}`);
  process.exit(0);
}
if (!source) usage();
var title = "hotaudio-track";
var buffer;
var keys;
var haxUrl = "";
if (/^https?:\/\//i.test(source)) {
  const seed = keysArg ? (await loadKeys()).keys : void 0;
  const res = await downloadHotaudioBuffer(source, { onProgress, trackId: trackId ?? void 0, apiBase, initialKeys: seed }).catch((err) => {
    console.error(`Download failed: ${err instanceof Error ? err.message : err}`);
    process.exit(1);
  });
  buffer = res.buffer;
  keys = res.keys;
  haxUrl = res.haxUrl;
  if (res.title) title = res.title;
} else {
  const html = await fs.readFile(source, "utf8").catch(() => null);
  if (!html) {
    console.error(`HTML file not found: ${source}`);
    process.exit(1);
  }
  const handshake = await loadHandshakeFromHtml(html, apiBase ?? HOTAUDIO_API_BASE, trackId ?? void 0);
  if (!handshake) {
    console.error(trackId ? `Track not found in HTML file: ${trackId}` : "Could not decrypt __ha_state from HTML file");
    process.exit(1);
  }
  const res = await downloadWithHandshake(handshake, { onProgress, initialKeys: keysArg ? (await loadKeys()).keys : void 0 }).catch((err) => {
    console.error(`Download failed: ${err instanceof Error ? err.message : err}`);
    process.exit(1);
  });
  buffer = res.buffer;
  keys = res.keys;
  haxUrl = res.haxUrl;
  if (res.title) title = res.title;
}
await saveKeys(/^https?:\/\//i.test(source) ? source : void 0, haxUrl, title, keys);
var out = outArg ?? `${sanitize(title)}.m4a`;
await fs.writeFile(out, buffer);
console.log(`Saved ${path.resolve(out)}`);
