# Architecture

## Pipeline

```
track page HTML
  -> __ha_state (base64, ChaCha20-Poly1305, trailing 32-byte key, zero nonce)
  -> HotaudioState { pid, tick, key (server X25519 pub), tracks, order }
  -> X25519 ephemeral key exchange -> session secret Ee = SHA-256(shared)
  -> listenRequest(first=-1) -> { url (.hax), keys (first branch) }
  -> fetch .hax container
  -> derive per-segment keys -> decrypt slices -> concat MP4 fragments
```

Long tracks hold more segments than one branch covers. The downloader
catches the `Key missing in keys map` error, pages `first:<segmentIndex>`
for exactly the missing index, merges the new branch, clears the node-key
cache, and resumes. `keys` returned to the caller are the union of all
fetched branches.

Measured against the live API, each `first:<n>` response unlocks a small
window starting at segment `n` (typically `[n..n+7]`), and concurrent
requests show no latency benefit over sequential ones — the server
effectively serializes per track. Exact-miss sequential paging is therefore
optimal: every request is provably needed, minimizing both wall time and
server load. (An earlier 3-in-flight stride-8 lookahead was removed after
measurement showed it only added redundant requests.)

## Listen encryption

- Request payload: `{ tid, pid, key, tick, first }` as JSON.
- `X-Signature`: output of the pinned `nozzle.js` routine over the payload string.
- Request nonce: first 12 bytes of `SHA-256(signature)`.
- Request body: payload encrypted with ChaCha20-Poly1305 under `Ee`.
- Response nonce: request nonce with the first byte incremented by one.
- Headers include `X-Key` (client ephemeral pubkey), the vendor content type, `Origin`/`Referer`, and the default UA.

## HAX0 container

```
offset 0:  magic "HAX0" (4 bytes)
offset 4:  fileLength   (u32 LE)
offset 8:  headerLength (u32 LE)
offset 12: extraLength  (u32 LE)
offset 16: bencoded metadata dict, through headerLength
```

Metadata keys: `codec`, `durationMs`, `segmentCount`, `segments`
(byte string of `segmentCount` × `{ offset u32 LE, pts u32 LE }`), `baseKey`.
Each segment slice (from its offset to the next, or `fileLength`) is an
independent ChaCha20-Poly1305 ciphertext under a zero nonce.

## Segment key tree

Keys form a binary tree above the leaf segments. Given `segmentCount`:

```
bitLen   = bits(segmentCount - 1)
treeBase = 1 + (1 << (bitLen + 1))
e        = treeBase + segIdx
t        = bits(e) - 1
```

Derivation starts at the nearest known ancestor in `keysMap` and hashes
down: `child = SHA-256(parent || branchByte)` where `branchByte` is the
low byte of the node index at that level. Consecutive segments share most
of their path, so a per-extraction `Map` cache makes this effectively free.

## Signer (`src/signer.ts`)

`signHotaudioPayload` evaluates a pinned `nozzle.js` build
(`PINNED_NOZZLE_VERSION`) inside a minimal browser shim: stubbed
`MediaSource`/`SourceBuffer` with native-code `toString` behavior,
`navigator.vendor`, frozen `performance` values, a hookable `Date`,
V8-style error stacks rooted at the pinned player URL, and a static
environment hash table (`src/env_hashes.ts`).

When the upstream player build changes, signatures break. The fix is to
re-capture `nozzle.js` and `env_hashes.ts` for the new version and bump
`PINNED_NOZZLE_VERSION` together — they must stay in sync.

Known quirk: in non-browser runtimes (Node/Bun) the *first* signature per
process has been observed to take ~123s (later signatures are instant), but
on other runs the first signature takes ~0.5s. The cause is not yet
identified — CPU profiling a slow run is the next step. The live probe
(`scripts/live-probe.ts`) flags any non-download stage over 30s, so a
recurrence is visible immediately. Do not "fix" the shim blindly: the
fingerprint it computes may be exactly what the server expects, and any
shim change needs a live acceptance test.

Side effect to know about: the sandbox replaces the *global* `performance`
object (frozen `now()`/`timeOrigin`) and `Date` subclass — `Date.now()` stays
real, but `performance.now()` stops advancing in the host process after the
signer initializes. The probe therefore times stages with `Date.now()`.

## Reliability (`src/retry.ts`)

All network reads — page fetch, listen requests, `.hax` fetch, streaming
range requests — go through `fetchWithRetry`: up to 3 attempts with
exponential backoff (500ms base, 10s cap) for network errors and
transient statuses (408/425/429/5xx), honoring `Retry-After` on 429.
Other statuses return immediately, and caller-aborted requests
(streaming `AbortSignal`) are never retried. A single transient failure
therefore no longer aborts a multi-minute, multi-hundred-request track
download. Small API calls (page, listen, ranges) additionally carry a
per-attempt timeout (`timeoutMs`, default 30s) so one stalled request —
observed live at 66s — degrades into a bounded retry instead of an
unbounded stall; the bulk `.hax` transfer is intentionally exempt.

## Streaming (`src/stream.ts`)
The browser path fetches only the 16-byte prefix to learn `headerLength`,
then the header to parse the segment table. It appends decrypted segments
to a `SourceBuffer` (`audio/mp4; codecs="mp4a.40.2"` preferred) as they
arrive, paging key branches on demand like the download path. `Range`
requests are used per segment; mirrors that ignore `Range` (HTTP 200 with
the full body) are sliced locally.
