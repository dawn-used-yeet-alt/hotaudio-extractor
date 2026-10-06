import {
  FETCH_API_TIMEOUT_MS,
  FETCH_RETRY_ATTEMPTS,
  FETCH_RETRY_BASE_DELAY_MS,
  FETCH_RETRY_MAX_DELAY_MS,
  HOTAUDIO_API_BASE,
  HOTAUDIO_STREAM_READY_TIMEOUT_MS,
  HOTAUDIO_UA,
  PINNED_NOZZLE_VERSION,
  base64ToBytes,
  bytesToHex,
  decodeBencode,
  decryptHaxBuffer,
  decryptHotaudioState,
  decryptSegmentSlice,
  deriveSegmentKey,
  downloadHaxBuffer,
  downloadHotaudioBuffer,
  downloadHotaudioToFile,
  downloadWithHandshake,
  extractHaState,
  extractListenKey,
  fetchHotaudioKeys,
  fetchHotaudioTracks,
  fetchWithRetry,
  haxUrlForTrackKey,
  hexToBytes,
  isRetryableStatus,
  listHotaudioTracks,
  listenRequest,
  loadHandshakeFromHtml,
  loadHotaudioHandshake,
  mergeBranchKeys,
  parseHax0Header,
  parseSavedKeys,
  performKeyExchange,
  sha256,
  signHotaudioPayload
} from "./chunk-GJ54U5UL.js";

// src/stream.ts
async function fetchRange(fetchFn, url, start, end, signal, timeoutMs = FETCH_API_TIMEOUT_MS) {
  const res = await fetchWithRetry(fetchFn, url, {
    headers: { "User-Agent": HOTAUDIO_UA, Range: `bytes=${start}-${end}` },
    signal
  }, { timeoutMs });
  if (!res.ok || res.status !== 206 && res.status !== 200) {
    throw new Error(`Hotaudio range fetch returned ${res.status}`);
  }
  const bytes = new Uint8Array(await res.arrayBuffer());
  return res.status === 200 ? bytes.subarray(start, end + 1) : bytes;
}
function appendBuffer(sb, data) {
  return new Promise((resolve, reject) => {
    const onDone = () => {
      sb.removeEventListener("updateend", onDone);
      sb.removeEventListener("error", onFail);
      resolve();
    };
    const onFail = () => {
      sb.removeEventListener("updateend", onDone);
      sb.removeEventListener("error", onFail);
      reject(new Error("MediaSource append failed"));
    };
    sb.addEventListener("updateend", onDone);
    sb.addEventListener("error", onFail);
    const copy = new Uint8Array(data.length);
    copy.set(data);
    sb.appendBuffer(copy.buffer);
  });
}
async function extractHotaudioStream(pageUrl, opts = {}) {
  const onProgress = opts.onProgress;
  const fetchFn = opts.fetchFn ?? globalThis.fetch;
  if (typeof MediaSource === "undefined") return null;
  const mimeCandidates = ['audio/mp4; codecs="mp4a.40.2"', "audio/mp4"];
  const mime = mimeCandidates.find((m) => {
    try {
      return MediaSource.isTypeSupported(m);
    } catch {
      return false;
    }
  });
  if (!mime) return null;
  onProgress?.({ phase: "resolving", loaded: 0, total: 0 });
  const handshake = await loadHotaudioHandshake(pageUrl, opts).catch(
    () => null
  );
  if (!handshake) return null;
  const controller = new AbortController();
  const signal = controller.signal;
  let initial;
  try {
    initial = await listenRequest(handshake, -1, opts);
  } catch (err) {
    console.error(err instanceof Error ? err.message : err);
    return null;
  }
  if (!initial.url) return null;
  const haxUrl = initial.url;
  const head = await fetchRange(fetchFn, haxUrl, 0, 15, signal, opts.timeoutMs).catch(() => null);
  if (!head || head.length < 16) return null;
  const headCopy = new Uint8Array(16);
  headCopy.set(head.subarray(0, 16));
  const view = new DataView(headCopy.buffer);
  const headerLength = view.getUint32(8, true);
  const fileLength = view.getUint32(4, true);
  if (headerLength < 16 || headerLength > fileLength) return null;
  const headerBytes = await fetchRange(fetchFn, haxUrl, 0, headerLength - 1, signal, opts.timeoutMs).catch(
    () => null
  );
  if (!headerBytes) return null;
  const hax = parseHax0Header(headerBytes);
  const keysMap = {};
  for (const [k, v] of Object.entries(initial.keys)) {
    keysMap[parseInt(k, 10)] = hexToBytes(v);
  }
  const nodeKeyCache = /* @__PURE__ */ new Map();
  const mediaSource = new MediaSource();
  const url = URL.createObjectURL(mediaSource);
  let resolveReady;
  const ready = new Promise((res) => {
    resolveReady = res;
  });
  let aborted = false;
  const pump = async () => {
    await new Promise((resolve, reject) => {
      if (mediaSource.readyState === "open") return resolve();
      const onOpen = () => {
        mediaSource.removeEventListener("sourceopen", onOpen);
        resolve();
      };
      mediaSource.addEventListener("sourceopen", onOpen, { once: true });
      setTimeout(() => reject(new Error("MediaSource never opened")), 3e4);
    });
    const sb = mediaSource.addSourceBuffer(mime);
    let appendedBytes = 0;
    const totalBytes = fileLength - hax.segments[0].offset;
    for (let i = 0; i < hax.segmentCount; i++) {
      if (signal.aborted || aborted) break;
      const seg = hax.segments[i];
      const nextOff = i + 1 < hax.segmentCount ? hax.segments[i + 1].offset : fileLength;
      const slice = await fetchRange(fetchFn, haxUrl, seg.offset, nextOff - 1, signal, opts.timeoutMs);
      let segKey;
      try {
        segKey = await deriveSegmentKey(keysMap, hax.segmentCount, i, nodeKeyCache);
      } catch (err) {
        if (!(err instanceof Error) || !err.message.startsWith("Key missing in keys map")) throw err;
        const extra = await listenRequest(handshake, i, opts);
        if (mergeBranchKeys(keysMap, extra.keys, hexToBytes) === 0) throw err;
        nodeKeyCache.clear();
        segKey = await deriveSegmentKey(keysMap, hax.segmentCount, i, nodeKeyCache);
      }
      const plain = decryptSegmentSlice(slice, segKey);
      await appendBuffer(sb, plain);
      appendedBytes += plain.length;
      if (i === 0) resolveReady();
      onProgress?.({ phase: "fetching", loaded: appendedBytes, total: totalBytes });
    }
    try {
      if (!signal.aborted && !aborted && mediaSource.readyState === "open") {
        mediaSource.endOfStream();
      }
    } catch {
    }
  };
  const done = pump().catch((err) => {
    if (signal.aborted || aborted) return;
    console.error("Hotaudio stream failed:", err);
  });
  return {
    url,
    ready,
    done,
    title: handshake.track.title,
    duration: hax.durationMs / 1e3,
    abort() {
      aborted = true;
      try {
        controller.abort();
      } catch {
      }
      try {
        URL.revokeObjectURL(url);
      } catch {
      }
    }
  };
}

// src/index.ts
var HOTAUDIO_PATTERN = /https?:\/\/(?:www\.)?hotaudio\.net\/u\/[a-zA-Z0-9_%~.-]+\/[a-zA-Z0-9_%~.-]+(?:\?[^"'\s<>)\]]*)?/i;
var HOTAUDIO_REGEX = HOTAUDIO_PATTERN;
async function extractHotaudio(url, opts = {}) {
  try {
    const session = await extractHotaudioStream(url, opts).catch(
      () => null
    );
    if (session) {
      try {
        await Promise.race([
          session.ready,
          new Promise(
            (_, reject) => setTimeout(
              () => reject(new Error("Hotaudio stream timeout")),
              opts.streamTimeoutMs ?? HOTAUDIO_STREAM_READY_TIMEOUT_MS
            )
          )
        ]);
        return {
          url: session.url,
          host: "hotaudio",
          title: session.title,
          duration: session.duration
        };
      } catch {
        session.abort();
      }
    }
  } catch {
  }
  return extractHotaudioDownload(url, opts);
}
async function extractHotaudioDownload(url, opts = {}) {
  try {
    const res = await downloadHotaudioBuffer(url, opts);
    if (typeof Blob !== "undefined" && typeof URL !== "undefined" && "createObjectURL" in URL) {
      const blob = new Blob([res.buffer], { type: res.mime });
      return { url: URL.createObjectURL(blob), host: "hotaudio", title: res.title, duration: res.duration, mime: res.mime };
    }
    let b64;
    if (typeof Buffer !== "undefined") {
      b64 = Buffer.from(res.buffer).toString("base64");
    } else {
      let bin = "";
      const CHUNK = 32768;
      for (let i = 0; i < res.buffer.length; i += CHUNK) {
        bin += String.fromCharCode(...res.buffer.subarray(i, i + CHUNK));
      }
      b64 = btoa(bin);
    }
    return {
      url: `data:${res.mime};base64,${b64}`,
      host: "hotaudio",
      title: res.title,
      duration: res.duration,
      mime: res.mime
    };
  } catch (err) {
    console.error("Hotaudio extraction failed:", err);
    return null;
  }
}
function isHotaudioUrl(url) {
  return HOTAUDIO_PATTERN.test(url);
}
export {
  FETCH_API_TIMEOUT_MS,
  FETCH_RETRY_ATTEMPTS,
  FETCH_RETRY_BASE_DELAY_MS,
  FETCH_RETRY_MAX_DELAY_MS,
  HOTAUDIO_API_BASE,
  HOTAUDIO_PATTERN,
  HOTAUDIO_REGEX,
  HOTAUDIO_STREAM_READY_TIMEOUT_MS,
  HOTAUDIO_UA,
  PINNED_NOZZLE_VERSION,
  base64ToBytes,
  bytesToHex,
  decodeBencode,
  decryptHaxBuffer,
  decryptHotaudioState,
  decryptSegmentSlice,
  deriveSegmentKey,
  downloadHaxBuffer,
  downloadHotaudioBuffer,
  downloadHotaudioToFile,
  downloadWithHandshake,
  extractHaState,
  extractHotaudio,
  extractHotaudioDownload,
  extractHotaudioStream,
  extractListenKey,
  fetchHotaudioKeys,
  fetchHotaudioTracks,
  fetchWithRetry,
  haxUrlForTrackKey,
  hexToBytes,
  isHotaudioUrl,
  isRetryableStatus,
  listHotaudioTracks,
  listenRequest,
  loadHandshakeFromHtml,
  loadHotaudioHandshake,
  mergeBranchKeys,
  parseHax0Header,
  parseSavedKeys,
  performKeyExchange,
  sha256,
  signHotaudioPayload
};
