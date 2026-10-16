# Architecture

How the extractor works, and why it is built the way it is.

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

Design rules:

- **`signer` has no I/O and no dependencies** beyond `std`. It is a pure
  function of `(payload, timestamp)`, which is what makes it trivially testable
  and fuzzable against the reference.
- **Two HTTP clients, deliberately.** `Client::api()` has a 30 s per-attempt
  timeout because page and listen calls are latency-bound and one stalled
  request should degrade into a bounded retry. `Client::bulk()` has no global
  timeout because a 15 MB container over a slow link legitimately takes
  minutes. Mixing these up produces either spurious failures or hangs.
  `download::Clients` carries the pair through the pipeline so callers run one
  implementation instead of re-wiring the two clients themselves.
- **Async at the I/O boundary only.** `crypto`, `hax` and `signer` are
  synchronous on both targets, so the CPU-bound half of the pipeline is
  identical everywhere and the wasm build is not a second implementation of
  the protocol.
- **Miss-driven key paging.** On a key miss the downloader asks for exactly the
  missing segment index. Measurements showed the server serialises per track,
  so every request is provably needed and no lookahead can help.

## The signature VM

The single most unusual part. See [PROTOCOL.md](PROTOCOL.md#the-signature-vm)
for the protocol-level description; architecturally the point is:

Upstream, the signer is a 106 KB obfuscated JavaScript bundle evaluated in a
fake-browser sandbox. Deobfuscating it revealed the payload is **not a hash
call** — it is a register machine running a 1314-entry program. This crate
implements that machine and embeds that program.

That choice matters:

- **Exact.** It is the same program, not a re-derivation of "the algorithm", so
  there is no risk of matching the shape but not the behaviour.
- **Fast.** A signature costs ~0.2 ms versus 200–500 ms for the bundle. The
  upstream docs note the first signature in a process occasionally took 123 s;
  that failure mode is now impossible.
- **Auditable.** `vm.rs` is 700 lines of ordinary Rust with named opcodes, and
  `tests/data/signer_golden.json` pins it against the reference.

The VM has no notion of "the environment" beyond a small static model
(`global_member` / `member_lookup` in `vm.rs`): the pinned build reads
`Date.stack` (absent, which routes through the bundle's `__FAB` error hook),
`MediaSource`, `SourceBuffer`, `SourceBuffer.prototype` and `appendBuffer`, and
probes the global for markers that must be *absent*. That surface is small
because the fingerprint table pins everything else.

## Data flow and memory

Three download strategies, in increasing order of streaming:

1. **Buffered** (default). Fetch the whole `.hax`, decrypt into one `Vec`.
   Simplest; peak memory is roughly the container size.
2. **Streamed** (`--stream-to`). Read the 16-byte prefix over `Range` to learn
   `headerLength`, read the header, then fetch and decrypt one segment at a time
   straight into a buffered writer. Peak memory is one segment; the first
   playable fragment exists after a single round trip.
3. **Offline** (`--hax`). Decrypt a local container with saved keys.

`range.rs` makes (2) possible behind a `RangeSource` trait, which is also how
the tests exercise the "mirror ignores `Range`" fallback without a server.

## Reliability

`http::request` retries up to three attempts with exponential backoff (500 ms
base, doubling, jitter, 10 s cap) for network errors and for statuses
`{401, 408, 425, 429, 500, 502, 503, 504}`, honouring `Retry-After` on 429.
Other statuses return immediately so the caller can surface the error body.
These are the defaults of `http::Retry`; a caller can replace them with
`Client::with_retries`, and the CLI exposes `--retries`/`--timeout`.

`401` being retryable matters: the player refreshes once on 401 before giving
up, so a transient signature rejection is recoverable.

The container transfer uses `http::get_resumable` instead, because it is the
one request large enough for a retry policy to matter. Measured on the live CDN,
per-connection throughput varies roughly 0.7–2.3 MB/s by which Cloudflare edge
answers, and connections do drop mid-body. Retrying a dropped body from byte
zero re-spends everything already received, so a 25 MB track degrades into
several full-length transfers. `get_resumable` keeps the bytes already read and
asks only for the remainder via `Range`, which the CDN honours with `206`. A
server that ignores `Range` and replies `200` causes the partial buffer to be
discarded and the request to restart, so correctness never depends on range
support. The reassembled body is also checked against the announced
`Content-Length` — a body cut short is an error, not a silently corrupt
container — and the decrypted result is checked for an MP4 `ftyp` box before it
is returned or handed to a caller. `--verify` goes further: it walks the box
tree and checks the output's duration against the container's `durationMs` (see
`mp4`). Covered by `tests/http_resume.rs`.

### What does *not* help

Measured against the live CDN, so as to avoid re-measuring:

- **Parallel range requests.** At a fixed 8 MiB total, one connection managed
  6.3 MB/s, four connections 3.65, eight 2.69, sixteen 2.38. Splitting a
  transfer multiplies handshakes and slow-start, and loses. The single large
  GET is the right shape.
- **Connection reuse across the pipeline.** Minor next to the transfer.

## Testing strategy

- **Parity, not similarity.** The signer is checked against vectors produced by
  the real JavaScript, not against hand-written expectations. The corpus covers
  what the VM is sensitive to: SHA-256 block-padding boundaries, astral-plane
  characters where JS `.length` counts UTF-16 code units, and JSON escaping. If
  parity ever breaks, diff the two implementations instruction by instruction
  (see [MAINTENANCE.md](MAINTENANCE.md#if-the-vm-opcode-semantics-changed)).
- **Fixtures from the real world, not invented.** `tests/key_tree.rs` uses 150
  branch keys captured from a live 899-segment track, so tree-geometry drift
  fails immediately.
- **One test body, two targets.** Tests carry `#[test]` and
  `#[wasm_bindgen_test]` attributes and are run by both CI jobs, so a divergence
  between browser and CLI behaviour fails a build rather than being discovered
  in production. `tests/transport_fetch.rs` drives the real `fetch` backend
  against a stub, which is how the transport is tested without a browser.
- **Performance is asserted, not assumed.** `tests/perf_guard.rs` measures bulk
  decrypt throughput and fails on an order-of-magnitude regression. The
  throughput half is `#[cfg(not(debug_assertions))]` rather than `#[ignore]`: it
  is a build profile where the assertion does not apply, not a test a human has
  to remember to ask for. Its correctness half runs in every profile and on both
  targets.
- **Offline.** `cargo test` needs no network. Live checks are explicit,
  manual, and separate: `scripts/verify-live.ts`.

## The browser transport

`wasm32-unknown-unknown` has no sockets, no blocking call and no threads, so the
browser build borrows the platform's HTTP stack instead. The split is drawn at
the narrowest possible seam:

| Shared, both targets | Native only | `wasm32` only |
| --- | --- | --- |
| retry and backoff policy | `ureq` + rustls | `fetch` |
| resume-on-drop logic | `thread::sleep` | `setTimeout` promise |
| URL rewriting | `Instant` / `SystemTime` | `performance.now()` / `Date.now()` |
| the entire protocol pipeline | | `web-sys` streaming reads |

`src/clock.rs` exists because `SystemTime::now()` and `Instant::now()` both
**panic** on `wasm32-unknown-unknown`. Every clock read in the crate goes through
it; nothing else may name either type. The values agree — `Date.now()` is Unix
milliseconds — so the signer sees the same clock it always did, which the golden
vectors confirm.

The pipeline's I/O entry points are `async` so that one implementation serves
both transports. On native nothing ever suspends, because the transport blocks,
so `hotaudio::block_on` is a single poll of a ready future and costs nothing; if a
future ever *does* suspend it panics rather than hanging, which turns a latent
bug into a loud one. `tests/perf_guard.rs` asserts the decrypt path has not
acquired a per-segment copy or await, which is the property this rests on.

Streaming matters more in a browser than on the CLI: it is what lets a page
`appendBuffer` each fragment into a `MediaSource` and start playing after one
round trip, with peak memory of one segment instead of one container. It is also
what keeps a fronting proxy from having to buffer the whole container — see
below.

### Why a browser build needs a proxy

Measured against the live site: hotaudio.net and cdn.hotaudio.net send no
`Access-Control-Allow-Origin` and refuse the cross-origin preflight, and
`User-Agent` is a forbidden header a page cannot set — so Cloudflare 403s the
browser's own. A page therefore cannot reach the site directly, however correct
this crate is.

A proxy is the embedder's infrastructure, and this crate deliberately does not
ship one: egress costs and abuse liability are not something an interoperability
tool should be running. What it does instead is make one trivial to point at —
`Client::with_url_rewrite`, or `Client::with_fetch` for a host whose HTTP is not
the page's `fetch` — and document what a compliant proxy must do, since a
range-preserving streaming relay is what keeps the cost to bandwidth rather than
memory. [docs/BROWSER.md](BROWSER.md) has the checklist and worked examples.

## Deliberate non-goals

- **Shipping a proxy.** The browser build needs a same-origin front, for CORS and
  user-agent reasons; pointing the crate at one is supported and documented, but
  running one is the embedder's call, not this project's. See
  [BROWSER.md](BROWSER.md).
- **Concurrency.** Deliberately sequential; see the paging note above.
- **Stealth.** The user agent is the bare `Mozilla/5.0` the site requires. This
  is an interoperability tool, not a scraper that hides itself. (On `wasm32` the
  browser forbids setting it at all, so the header is dropped rather than
  spoofed — see the browser transport section.)