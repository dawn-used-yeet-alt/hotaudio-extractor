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
(with a small forward lookahead, 3 in flight, stride 8), merges the new
branches, clears the node-key cache, and resumes. `keys` returned to the
caller are the union of all fetched branches.

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
process takes ~2 minutes while a bundled environment probe spins on a
browser API the shim does not provide; later signatures in the same process
are instant. Every CLI run therefore pays a one-time ~2-minute cost. The
live probe (`scripts/live-probe.ts`) flags this on the signer stage. Do not
"fix" the shim to shortcut it without a live acceptance test — the
fingerprint it computes under the timeout may be exactly what the server
expects.

## Streaming (`src/stream.ts`)

The browser path fetches only the 16-byte prefix to learn `headerLength`,
then the header to parse the segment table. It appends decrypted segments
to a `SourceBuffer` (`audio/mp4; codecs="mp4a.40.2"` preferred) as they
arrive, paging key branches on demand like the download path. `Range`
requests are used per segment; mirrors that ignore `Range` (HTTP 200 with
the full body) are sliced locally.
