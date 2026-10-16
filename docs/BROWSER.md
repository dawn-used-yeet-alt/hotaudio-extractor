# Running in a browser

How to use this crate from `wasm32-unknown-unknown`: a page, a Capacitor app, or
any other JS host.

The crate builds for `wasm32-unknown-unknown` and exposes a `wasm-bindgen` API.
The protocol pipeline is the same one the CLI runs — the signer, the X25519
exchange and the HAX0 decrypt are shared code, not a second implementation.
Only the transport differs. See
[ARCHITECTURE.md](ARCHITECTURE.md#the-browser-transport) for why.

## Build

`scripts/build-wasm.sh` produces the JavaScript package: `pkg/web` for a page,
`pkg/nodejs` for Node, each with `wasm-bindgen`'s generated `.d.ts` types.

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <the version in Cargo.lock>
./scripts/build-wasm.sh
```

The `wasm-bindgen` CLI and the `wasm-bindgen` crate in `Cargo.lock` are
version-locked, so the script checks that they match and prints the exact
`cargo install` line otherwise. This is the same path CI and the release take,
so a broken package fails before a tag rather than during one. Release archives
carry the prebuilt package as `hotaudio-wasm-<tag>.tar.gz`.

By hand — the same two steps, if you would rather not use the script:

```bash
cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir pkg/web target/wasm32-unknown-unknown/release/hotaudio.wasm
wasm-bindgen --target nodejs --out-dir pkg/nodejs target/wasm32-unknown-unknown/release/hotaudio.wasm
```

## Play

Segments are handed to a callback as they decrypt, so playback starts after one
round trip instead of after the whole container, and neither side ever holds more
than one segment.

```js
import init, { downloadTrack } from "./pkg/web/hotaudio.js";
await init();

const media = new MediaSource();
const video = document.querySelector("video");
video.src = URL.createObjectURL(media);

media.addEventListener("sourceopen", async () => {
  const mime = "audio/mp4; codecs=mp4a.40.2";
  const sourceBuffer = media.addSourceBuffer(mime);
  const queue = [];
  let done = false;

  const pump = () => {
    if (queue.length && !sourceBuffer.updating) {
      sourceBuffer.appendBuffer(queue.shift());
    } else if (done && !queue.length) {
      media.endOfStream();
    }
  };
  sourceBuffer.addEventListener("updateend", pump);

  try {
    const info = await downloadTrack(
      "https://hotaudio.net/u/user/track",
      // Each call is one decrypted fragment; `SourceBuffer.appendBuffer` copies
      // it, so buffering the whole track here is safe if you prefer.
      (fragment) => new Promise((resolve) => {
        queue.push(fragment);
        pump();
        resolve();
      }),
      (phase, loaded, total) => report(phase, loaded, total),
    );
    done = true;
    pump();
  } catch (err) {
    console.error(err);
  }
});
```

The plain version, collecting a Blob instead of streaming:

```js
const parts = [];
const info = await downloadTrack(url, (fragment) => parts.push(fragment));
const blob = new Blob(parts, { type: info.mime });
```

`downloadTrack` resolves with:

| Field | Meaning |
| --- | --- |
| `title` | from the page state, or `null` |
| `durationSecs` | container duration |
| `codec` | e.g. `mp4a.40.2` |
| `mime` | always `audio/mp4` |
| `bytes` | plaintext audio handed to the callback |
| `segmentCount`, `keyPages` | segments decrypted, key branches paged |
| `haxUrl` | the server-issued container URL |
| `keysJson` | saved-keys envelope, for a later offline fetch |

## Other exports

- `downloadTrackWith(url, onFragment, onProgress, fetchImpl, rewrite, trackId, signal)` —
  a replacement `fetch` and/or a URL rewrite. This is the integration seam; see
  below.
- `trackKeys(url, trackId)` — handshake and first key branch without downloading
  audio.
- `trackKeysWith(url, fetchImpl, rewrite, trackId)` — the same key-only save
  through the replacement `fetch` / rewrite seam, so it works behind a proxy or
  a native bridge too.
- `decryptHax(haxBytes, keysJson)` — decrypt a `.hax` you already hold with a
  saved-keys envelope, entirely offline. Returns the MP4 as a `Uint8Array`.
- `downloadCached(haxUrl, keysJson, onFragment, onProgress, signal)` — fetch a
  `.hax` by URL and decrypt it with saved keys, with no page handshake and no
  listen calls. The offline decrypt is buffered, so `onFragment` fires once with
  the whole MP4.
- `signPayload(payload)` — the `X-Signature` for a payload right now, so a host
  can diff against the reference signer in the browser.
- `signPayloadAt(payload, timestampSecs)` — the same signature at a pinned Unix
  time, for reproducing a golden vector deterministically.
- `listTracks(htmlOrUrlHtml)` — `[{ id, key, title }, …]`. Takes page HTML, so a
  host that already fetched the page need not fetch it twice.

### Selecting a track

`downloadTrack`, `downloadTrackWith` and `trackKeys` take an optional `trackId`
(the last argument for `trackKeys`; followed only by `signal` elsewhere). Omit it
(or pass `null`/`undefined`) for the page's default track.

```js
const tracks = listTracks(pageHtml);
const info = await downloadTrack(url, onFragment, onProgress, tracks[1].id);
```

An id the page does not carry is rejected with `track <id> not on page` rather
than silently falling back to the first track.

### Replaying a saved track offline

`trackKeys` — or `downloadTrack`'s `keysJson` — saves everything a later replay
needs. With that envelope, replaying skips the page and the listen API:

```js
const envelope = JSON.parse(storedKeysJson);
await downloadCached(envelope.haxUrl, storedKeysJson, onFragment, onProgress);
```

If the host already has the `.hax` bytes — because it saved them along the way —
`decryptHax` skips the fetch as well:

```js
const mp4 = decryptHax(cachedHaxBytes, storedKeysJson); // Uint8Array
```

### Cancelling a download

`downloadTrack`, `downloadTrackWith` and `downloadCached` take a trailing
`AbortSignal`. Aborting it stops the download at the next segment or key-page
boundary — no fragment is torn mid-write — and rejects with an `AbortError`,
which is what `signal`-aware callers already treat as a deliberate cancel:

```js
const controller = new AbortController();
const info = await downloadTrack(
  url, onFragment, onProgress, trackId, controller.signal,
).catch((err) => {
  if (err.name === "AbortError") return null; // the user navigated away
  throw err;
});

// elsewhere, e.g. on route change:
controller.abort();
```

## A proxy is required — this crate does not ship one

Measured against the live site:

- `hotaudio.net` and `cdn.hotaudio.net` send **no** `Access-Control-Allow-Origin`.
  A cross-origin preflight is refused: `OPTIONS` returns 404 or 405.
- `User-Agent` is a *forbidden header* — `fetch` will not let a page set it — and
  the site answers Chrome user agents with 403. This is why the native build
  pins `Mozilla/5.0`.

So a page cannot talk to hotaudio directly, however correct the crate is. Put your
own origin in front. This project deliberately does not ship one: a proxy is
infrastructure with egress costs and abuse liability, which is not something an
interoperability tool should be running.

```js
await downloadTrackWith(
  "https://hotaudio.net/u/user/track",
  onFragment,
  onProgress,
  null,                                              // keep the global fetch
  (url) => url.replace("hotaudio.net", "proxy.example"),
);
```

### What a compliant proxy must do

1. **Rewrite `User-Agent` to `Mozilla/5.0`.** The browser cannot, and Cloudflare
   403s Chrome user agents. This is the single most common way a hand-rolled
   proxy breaks.
2. **Add CORS headers and answer the preflight.** Echo
   `Access-Control-Allow-Origin`, and answer `OPTIONS` with `x-signature`,
   `x-key` and `content-type` allow-listed, or every listen call fails before it
   reaches the crate.
3. **Forward `Range` and return real `206`s.** Both the resumable bulk transfer
   and the segment-by-segment reads depend on it.
4. **Stream; do not buffer.** Relay the bytes. Calling `arrayBuffer()` on the
   container defeats the whole design.

Points 3 and 4 are what keep a proxy cheap. A byte relay is bandwidth, not CPU,
and because ranges pass straight through, the proxy never holds a 15 MB
container and the browser never holds more than a segment. A 15 MB track is
over 99% of the traffic, so this is the whole cost model: one extra network hop,
a handful of extra round trips on the small API calls, and no added CPU.

### Native `--proxy` is not the browser's proxy

The CLI's `--proxy` and the `ALL_PROXY`/`HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY`
environment variables configure an HTTP(S) CONNECT tunnel for the *native*
transport. A page cannot choose its proxy, and this crate does not try:
`Client::with_proxy` is a documented no-op on `wasm32`. Proxying in a browser is
the embedder's job — the origin in front above, or a replacement `fetch`.

## Hosts that are not a page

Capacitor, React Native and Electron shells usually have a native HTTP bridge
that bypasses CORS entirely. Rather than proxying, pass that bridge as a
replacement `fetch`:

```js
const info = await downloadTrackWith(
  url,
  onFragment,
  onProgress,
  (url, init) => CapacitorHttp.request({ url, ...init, responseType: "arraybuffer" }),
);
```

The replacement must take `(url, init)` and resolve to a `Response`. The native
bridge in most shells can be adapted in a few lines. `trackKeysWith` takes the
same pair when only the keys are wanted.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| 403 from Cloudflare, or a challenge page | `User-Agent` not rewritten (rule 1) |
| CORS error in the console, nothing reaches the crate | preflight not answered (rule 2) |
| Every segment 416s or re-downloads | `Range` dropped or answered `200` (rule 3) |
| Proxy memory spikes to track size | body buffered rather than streamed (rule 4) |
| `fetch failed` immediately, no status | the proxy is unreachable or its TLS is invalid |

`signPayload` is the quickest way to check the rest of the pipeline is sound: if
it returns a `9:`-prefixed signature, the signer works in that environment and
the problem is network-side.

## Testing

```bash
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
cargo test --target wasm32-unknown-unknown
```

The same test bodies run as on native, plus `tests/transport_fetch.rs`, which
drives the real `fetch` transport against a stub — header construction,
forbidden-header dropping, streaming reads — without needing a browser.