# Architecture

How the extractor works, and why it is built the way it is. The measurements
backing the non-obvious decisions live here, so a re-measurement is the only
reason to change them.

```
track page HTML
  └─ __ha_state  (base64 → ChaCha20-Poly1305, trailing 32-byte key, zero nonce)
       └─ HotaudioState { pid, tick, key (server X25519 pubkey), tracks, order }
            ├─ X25519 ephemeral key exchange → Ee = SHA-256(shared secret)
            └─ listenRequest(first = -1)
                 payload = {"tid","pid","key","tick","first"}   (JSON, fixed order)
                 X-Signature = VM(payload, now_seconds)          ← the signer
                 nonce       = SHA-256(signature)[0..12]
                 body        = ChaCha20-Poly1305(Ee, nonce, payload)
                 ← { url: ".hax", keys: { node: hex } }
                 ← response decrypted under SHA-256(signature)[0..12] with byte 0 + 1
            └─ GET url → .hax container
                 ├─ bencoded header: codec, durationMs, segmentCount, segments, baseKey
                 └─ per segment: derive key from the branch tree, decrypt (zero nonce)
            └─ concatenate fragments → playable .m4a
```

## Crate layout

| Module | Responsibility |
| --- | --- |
| `signer` | The signature VM. `bytecode.rs` (generated), `env.rs` (fingerprint table), `vm.rs` (the machine). |
| `crypto` | SHA-256, X25519, ChaCha20-Poly1305, hex/base64. Thin wrappers over RustCrypto. |
| `hax` | Bencode reader, HAX0 header, and the segment key tree. |
| `range` | HTTP `Range` reading: header probe, per-segment fetch, streaming decrypt. |
| `listen` | Page handshake, `__ha_state` decrypt, the encrypted listen API. |
| `http` | `Client`, retry policy, resume logic. Transport backends per target. |
| `download` | The pipeline, key paging, saved-keys envelope. |
| `clock` | Wall clock, monotonic clock and sleeping, per target. |
| `wasm` | The `wasm-bindgen` surface. `wasm32` only. |
| `bin/download` | CLI. Native only. |

Design rules the layout rests on:

- **`signer` has no I/O and no dependencies** beyond `std`. It is a pure
  function of `(payload, timestamp)`, which makes it trivially testable and
  fuzzable against the reference.
- **Two HTTP clients.** `Client::api()` has a 30 s per-attempt timeout,
  because page and listen calls are latency-bound and one stalled request
  should degrade into a bounded retry. `Client::bulk()` has no global
  timeout, because a 15 MB container over a slow link legitimately takes
  minutes. Mixing up the two produces spurious failures (a slow transfer
  against the API timeout) or hangs (an API call without one).
  `download::Clients` carries the pair through the pipeline so callers run
  one implementation instead of re-wiring it.
- **Async at the I/O boundary only.** `crypto`, `hax` and `signer` are
  synchronous on both targets, so the CPU-bound half of the pipeline is
  identical everywhere and the wasm build is not a second implementation of
  the protocol.
- **Miss-driven key paging.** On a key miss the downloader asks for exactly
  the missing segment index. The server serialises per track
  ([PROTOCOL.md](PROTOCOL.md#5-segment-key-tree)), so every request is
  provably needed and no lookahead can help.

## The signature VM

See [PROTOCOL.md](PROTOCOL.md#6-the-signature-vm) for the protocol-level
description. Architecturally, the choice that matters is embedding the
program instead of re-deriving "the algorithm":

- It **matches behaviour**, because it is the same machine running the same
  1,314-byte program; there is no risk of matching the shape but not the
  behaviour.
- It is **fast**: a signature costs about 0.2 ms against 200-500 ms for the
  eval'd bundle. Upstream docs noted the first signature in a process
  occasionally taking 123 s; that failure mode no longer exists.
- It stays **auditable**: `vm.rs` is 700 lines of ordinary Rust with named
  opcodes, and `tests/data/signer_golden.json` pins it against the reference.

The VM has no environment beyond a small static model (`global_member` /
`member_lookup` in `vm.rs`): the pinned build reads `Date.stack` (absent,
which routes through the bundle's `__FAB` error hook), `MediaSource`,
`SourceBuffer`, `SourceBuffer.prototype` and `appendBuffer`, and probes the
global for markers that must be absent. Everything else is pinned by the
fingerprint table, which is why the probe stays small.

## Data flow and memory

Three download strategies, from most buffered to most streamed:

1. **Buffered** (default). Fetch the whole `.hax`, decrypt into one `Vec`.
   Simplest; peak memory is roughly the container size.
2. **Streamed** (`--stream-to`). Read the 16-byte prefix over `Range` to
   learn `headerLength`, read the header, then fetch and decrypt one segment
   at a time straight into a buffered writer. Peak memory is one segment, and
   the first playable fragment exists after a single round trip.
3. **Offline** (`--hax`). Decrypt a local container with saved keys.

`range.rs` makes strategy 2 possible behind a `RangeSource` trait, which is
also how the tests exercise the "mirror ignores `Range`" fallback with no
server involved.

## Reliability

`http::request` retries up to three attempts with exponential backoff (500 ms
base, doubling, jitter, 10 s cap) for network errors and for statuses
`{401, 408, 425, 429, 500, 502, 503, 504}`, honouring `Retry-After` on 429.
Other statuses return immediately so the caller can surface the error body.
These are the defaults of `http::Retry`; a caller can replace them with
`Client::with_retries`, and the CLI exposes `--retries`/`--timeout`.

`401` being retryable matters. The player refreshes once on 401 before giving
up, so a transient signature rejection is recoverable.

The container transfer uses `http::get_resumable` instead, because it is the
one request large enough for a retry policy to matter. Measured on the live
CDN: per-connection throughput varies roughly 0.7-2.3 MB/s depending on which
Cloudflare edge answers, and connections do drop mid-body. Retrying a dropped
body from byte zero re-spends everything already received, so a 25 MB track
degrades into several full-length transfers. `get_resumable` keeps the bytes
already read and asks only for the remainder via `Range`, which the CDN
honours with `206`. A server that ignores `Range` and replies `200` causes
the partial buffer to be discarded and the request to restart, so correctness
never depends on range support.

After reassembly, the body is checked against the announced `Content-Length`
(a body cut short is an error, not a silently corrupt container), and the
decrypted result is checked for an MP4 `ftyp` box before it is returned to
anyone. `--verify` goes further: it walks the box tree and checks the output
duration against the container's `durationMs` (the `mp4` module).

The progress hook doubles as the cancellation point. An error returned from
it aborts the transfer and is never retried, because it is the caller's own
decision rather than a transport failure. That is what puts a Ctrl-C at the
next chunk instead of after the whole container. `tests/http_resume.rs`
covers this path.

### What does not help

Measured against the live CDN, so the numbers need not be re-measured:

- **Parallel range requests.** At a fixed 8 MiB total, one connection managed
  6.3 MB/s, four connections 3.65, eight 2.69, sixteen 2.38. Splitting a
  transfer multiplies handshakes and slow-start, and loses. The single large
  GET is the right shape.
- **Connection reuse across the pipeline.** Minor next to the transfer.

## Testing strategy

- **Parity against the real thing.** The signer is checked against vectors
  produced by the actual JavaScript, not hand-written expectations. The
  corpus covers what the VM is sensitive to: SHA-256 block-padding
  boundaries, astral-plane characters (JS `.length` counts UTF-16 code
  units), and JSON escaping. If parity ever breaks, diff the two
  implementations instruction by instruction
  ([MAINTENANCE.md](MAINTENANCE.md#if-the-vm-opcode-semantics-changed)).
- **Fixtures from the real world.** `tests/key_tree.rs` uses 150 branch keys
  captured from a live 899-segment track, so tree-geometry drift fails
  immediately.
- **One test body, two targets.** Tests carry `#[test]` and
  `#[wasm_bindgen_test]` attributes and are run by both CI jobs, so a
  divergence between browser and CLI fails a build instead of showing up in
  production. `tests/transport_fetch.rs` drives the real `fetch` backend
  against a stub, which is how the transport is tested without a browser.
- **Performance asserted, not assumed.** `tests/perf_guard.rs` measures bulk
  decrypt throughput and fails on an order-of-magnitude regression. Its
  throughput half runs only where the assertion can hold, under
  `#[cfg(not(debug_assertions))]`; this is a build-profile question rather
  than a test a human must remember to ask for. Its correctness half runs in
  every profile and on both targets.
- **Offline.** `cargo test` needs no network. The live checks are a separate
  workspace, `live/`, which the test build never sees.

## The browser transport

`wasm32-unknown-unknown` has no sockets, no blocking call and no threads, so
the browser build borrows the platform's HTTP stack. The seam is drawn at the
narrowest point that could carry it:

| Shared, both targets | Native only | `wasm32` only |
| --- | --- | --- |
| retry and backoff policy | `ureq` + rustls | `fetch` |
| resume-on-drop logic | `thread::sleep` | `setTimeout` promise |
| URL rewriting | `Instant` / `SystemTime` | `performance.now()` / `Date.now()` |
| the entire protocol pipeline | | `web-sys` streaming reads |

`src/clock.rs` exists because `SystemTime::now()` and `Instant::now()` both
**panic** on `wasm32-unknown-unknown`. Every clock read in the crate goes
through it, and nothing else may name either type. The values agree
(`Date.now()` is Unix milliseconds), so the signer sees the same clock as it
always did; the golden vectors confirm this.

The pipeline's I/O entry points are `async` so one implementation serves both
transports. On native, nothing ever suspends, because the transport blocks;
`hotaudio::block_on` is a single poll of a ready future and costs nothing. If
a future ever does suspend, it panics rather than hanging, which turns a
latent bug into a loud one. `tests/perf_guard.rs` asserts the decrypt path
has not acquired a per-segment copy or await: the property this rests on.

Streaming matters more in a browser than on the CLI. It is what lets a page
`appendBuffer` each fragment into a `MediaSource` and start playing after one
round trip, with peak memory of one segment instead of one container. It is
also what keeps a fronting proxy from having to buffer the whole container
(see below, and [BROWSER.md](BROWSER.md#what-a-compliant-proxy-must-do)).

### The browser needs a same-origin front

Exceptions get their own page, because the requirements are a checklist an
embedder has to pass: no `Access-Control-Allow-Origin`, refused preflight,
and a forbidden-request header. A summary with the checklist lives in
[BROWSER.md](BROWSER.md#a-proxy-is-required-and-this-crate-does-not-ship-one);
the design position is in the non-goals below.

## Deliberate non-goals

Do not "improve" the following without re-measuring; the measurements above
or in `docs/PROTOCOL.md` are the point of naming them.

- **Shipping a proxy.** The browser build needs a same-origin front, for CORS
  and user-agent reasons. Pointing the crate at one is supported and
  documented ([BROWSER.md](BROWSER.md)); running one is the embedder's call.
  Egress cost and abuse liability belong to whoever serves the traffic.
- **Concurrency.** Sequential by design. The server serialises per track
  (paging note above), and the parallel-range measurement says adding
  connections does not increase throughput.
- **Stealth.** The default user agent is the bare `Mozilla/5.0`; `--user-agent`
  exists only to match the browser that earned a `cf_clearance` cookie. There
  is no fingerprint rotation, challenge solving or proxy farming. This is an
  interoperability tool, not a scraper that hides itself. On `wasm32` the
  browser forbids setting the header at all, so the crate drops it rather than
  spoofing.
