# API reference

All symbols are exported from the package root (`hotaudio-extractor`).

## Download

### `downloadHotaudioBuffer(pageUrl, opts?): Promise<HotaudioBufferResult>`

Runs the full pipeline: page handshake, initial listen request, `.hax`
fetch, key paging, decrypt. Works anywhere `fetch` exists.

```ts
const res = await downloadHotaudioBuffer(url, {
  userAgent: 'Mozilla/5.0', // default
  fetchFn: fetch, // custom fetch / proxy / mock
  apiBase: 'https://hotaudio.net', // override listen host
  onProgress: ({ phase, loaded, total }) => {},
});
// res: { buffer, title, duration, mime, haxUrl, keys, segmentCount }
```

`res.keys` contains the merged hex branch keys and can be persisted for
offline use. `res.buffer` is concatenated playable MP4 (`.m4a`) bytes.

### `downloadWithHandshake(handshake, opts?)`

Same as above but starts from an existing handshake. Use with
`loadHandshakeFromHtml` for saved HTML files or tests.

### `downloadHotaudioToFile(pageUrl, outPath, opts?)`

Node-only helper that downloads and writes the file. Returns
`{ outPath, title, duration, segmentCount }`.

### `fetchHotaudioKeys(pageUrl, opts?)`

Head-only key fetch: returns `{ keys, url, title, handshake }` without
downloading audio. Long tracks need follow-up
`listenRequest(handshake, segmentIndex)` calls for remaining branches.

### `decryptHaxBuffer(haxBytes, allKeys, onProgress?)`

Pure offline decrypt. `allKeys` must cover every segment. Returns
`{ buffer, segmentCount, duration, mime }`.

### `downloadHaxBuffer(haxUrl, allKeys, opts?)`

Cached download: fetch the `.hax` container and decrypt with saved keys.
No page fetch, no listen requests.

### Resuming interrupted downloads

Pass previously saved keys as `initialKeys` — they seed the key map, so
only still-missing branches are fetched:

```ts
const saved = parseSavedKeys(await readFile('keys.json', 'utf8'));
const res = await downloadHotaudioBuffer(pageUrl, { initialKeys: saved.keys });
// or via CLI: hotaudio-download <URL> --keys keys.json --save-keys keys.json
```

Branch keys are deterministic per track, so seeds from older sessions stay
valid. Malformed seed entries are skipped (paged fresh); well-formed but
stale seeds fail closed at decrypt time (ChaCha auth error) — drop `--keys`
and re-run for fresh keys.

### `parseSavedKeys(input)`

Parse a `--keys` value: saved-keys envelope or bare `{ index: hex }` map,
from JSON text or an object. Returns `{ keys, haxUrl?, pageUrl?, title? }`.

## Browser playback

### `extractHotaudio(url, opts?): Promise<HotaudioResult | null>`

Stream-first entry point. Waits for the MSE session to become ready
(`streamTimeoutMs`, default 20s), then falls back to a full-download
Blob URL (or base64 data URL outside DOM runtimes). Never throws for
playback setup failures; returns `null` only when extraction fails.

### `extractHotaudioStream(url, opts?): Promise<HotaudioStreamSession | null>`

Low-level MSE session. Returns `null` when `MediaSource` is unavailable
or the selected MIME is unsupported.

```ts
const session = await extractHotaudioStream(url);
if (!session) return fallback();
audio.src = session.url;
await session.ready;
await audio.play();
await session.done; // or session.abort() to cancel
```

### `extractHotaudioDownload(url, opts?): Promise<HotaudioResult | null>`

Full-download fallback used by `extractHotaudio`.

## Handshake and protocol

- `loadHotaudioHandshake(pageUrl, opts?)` — page fetch + state decrypt + key exchange.
- `loadHandshakeFromHtml(html, apiBase?)` — same without the page fetch.
- `fetchHotaudioTracks(pageUrl, opts?)` / `listHotaudioTracks(html)` — every track on a page, in page order.
- `haxUrlForTrackKey(trackKey)` — derive the `.hax` container URL without a listen call (`https://cdn.hotaudio.net/a/<key>.hax`).
- `extractHaState(html)` — raw `__ha_state` string or `null`.
- `listenRequest(handshake, first, opts?)` — one encrypted listen call. `first: -1` for the initial call, `first: <segmentIndex>` for paging.
- `extractListenKey(pageUrl)` — page-URL `?key=` param forwarded to listen requests (private/unlisted tracks).
- `mergeBranchKeys(keysMap, keys, fromHex)` — merge a follow-up response; returns new-key count.
- `signHotaudioPayload(payload, timestampSeconds?)` — raw `X-Signature` value
- `fetchWithRetry` / `isRetryableStatus` — transient-failure retries used by all network reads.
- `performKeyExchange(serverPubHex)` — ephemeral X25519 session.
- `decryptHotaudioState(stateB64)` — page state decrypt.

## HAX0 primitives

- `parseHax0Header(buffer)` — magic, lengths, bencoded metadata, segment table.
- `deriveSegmentKey(keysMap, segmentCount, segIdx, cache?)` — tree descent with memoization. Clear `cache` after merging new branches.
- `decryptSegmentSlice(slice, key)` — ChaCha20-Poly1305 decrypt (zero nonce).
- `decodeBencode(buf, offset)` — minimal decoder for HAX0 metadata.

## Types and constants

- `DownloadOptions` / `HotaudioBufferResult` / `HotaudioStreamSession`
- `HotaudioTrack` / `HotaudioState` / `HotaudioListenResponse`
- `Hax0Container` / `Hax0Segment`
- `HotaudioProgress` (`resolving` | `fetching` | `decrypting`) and `HotaudioProgressCallback`
- `HotaudioResult`, `HotaudioFetch`, `HotaudioOptions`
- `HOTAUDIO_UA`, `HOTAUDIO_API_BASE`, `HOTAUDIO_STREAM_READY_TIMEOUT_MS`
- `HOTAUDIO_PATTERN` (alias `HOTAUDIO_REGEX`), `isHotaudioUrl(url)`
