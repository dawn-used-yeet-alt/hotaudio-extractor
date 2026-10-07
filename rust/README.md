# hotaudio-rs

A standalone [hotaudio.net](https://hotaudio.net) audio extractor in Rust: page
handshake, request signing, X25519 listen key exchange, HAX0 container
decryption, and a CLI that writes playable `.m4a`.

This replaces the TypeScript `hotaudio-extractor`; see
[docs/MIGRATION.md](docs/MIGRATION.md).

> For interoperability and personal archiving only. Download only content you
> have the right to access, and comply with hotaudio.net's terms of service and
> applicable law.

## Why this port is interesting

Upstream, the `X-Signature` header comes from evaluating a ~106 KB obfuscated
JavaScript bundle inside a fake-browser sandbox. That bundle is obfuscated to
hide its payload, but the payload is **not a hash call** — it is a **register
machine**:

- a **1314-entry program** (657 two-byte instructions), recovered from the bundle
- an **80-slot register file**; opcodes below 80 are `MOV`, 80 and above dispatch
- an instruction set that is SHA-256 compression, string building and branches —
  plus a probe that walks stubbed globals to prove the environment looks like a
  browser
- output: `"9:" + big-endian u32 seconds + 12 bytes`

This crate re-implements **the machine and the same program** rather than
guessing at "the algorithm". That makes it exact by construction, and much
faster:

| | TypeScript (bun) | Rust |
| --- | --- | --- |
| one signature | 200–500 ms (occasionally 123 s) | **~0.2 ms** |
| decrypt a 14.8 MB `.hax` | 0.80 s | **0.19 s** |

Correctness is pinned against the real thing: 98 golden vectors captured from
the reference signer, plus a differential trace that compared all 1570 VM
instructions register-for-register. Validated end-to-end against the live API
(899-segment track → valid 14.8 MB AAC/MP4).

## Install

```bash
cargo build --release          # -> target/release/hotaudio-download
```

## Usage

```bash
# Download + decrypt a track
hotaudio-download https://hotaudio.net/u/user/track-slug --out track.m4a

# Stream: low memory, first playable fragment after one round trip
hotaudio-download https://hotaudio.net/u/user/track-slug --stream-to track.m4a

# Save branch keys for later offline use
hotaudio-download https://hotaudio.net/u/user/track-slug --save-keys keys.json

# Cached re-download: no page fetch, no listen calls
hotaudio-download --keys keys.json --out track.m4a

# Fully offline decrypt of a local .hax
hotaudio-download --hax audio.hax --keys keys.json --out track.m4a

# List tracks on a page
hotaudio-download https://hotaudio.net/u/user/track-slug --list-tracks
```

Branch keys are deterministic per track and `.hax` URLs are stable, so a saved
envelope re-downloads a track with zero page/listen requests.

## Library

```rust
use hotaudio::{download, http};

let agent = http::api_agent();
let mut opts = download::DownloadOptions::default();
let result = download::download_from_page(&agent, url, &mut opts)?;
std::fs::write("track.m4a", result.audio)?;
```

| Module | Responsibility |
| --- | --- |
| `signer` | the signature VM (`bytecode.rs` generated, `env.rs` fingerprint, `vm.rs` machine) |
| `crypto` | SHA-256, X25519, ChaCha20-Poly1305, hex/base64 |
| `hax` | bencode, HAX0 header, segment key tree |
| `range` | HTTP `Range` reads, header probe, streaming decrypt |
| `listen` | handshake and the encrypted listen API |
| `http` | agents and retry policy |
| `download` | pipeline, key paging, saved-keys envelope |

## Tests

```bash
cargo test
```

Offline; no network required.

- `tests/signer_parity.rs` — 98 golden vectors from the reference signer
- `tests/hax_roundtrip.rs` — container round-trip, corruption, wrong keys
- `tests/key_tree.rs` — derivation against 150 branch keys captured from a real
  899-segment track

## When the site changes

The signer is pinned to one upstream player build. Diagnose and re-capture:

```bash
bun scripts/verify-live.ts      # names the failing stage
bun scripts/recapture.ts        # regenerate src/signer/bytecode.rs
$EDITOR src/signer/env.rs       # PINNED_NOZZLE_VERSION + ENV_HASHES
$EDITOR scripts/shim.ts         # the same two values, for the reference sandbox
bun scripts/verify-live.ts      # the server is the authority
bun scripts/gen-golden.ts       # then regenerate parity vectors
cargo test
```

[docs/MAINTENANCE.md](docs/MAINTENANCE.md) is the full runbook, including a
symptom-to-cause table and the opcode-semantics gotchas that cost the most
debugging time during the original port.

The tooling is self-contained: it needs only `vendor/nozzle.js` and
`scripts/shim.ts`, both vendored here. It does not require the TypeScript
implementation.

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — how it fits together
- [docs/PROTOCOL.md](docs/PROTOCOL.md) — the wire protocol and the signature VM
- [docs/MAINTENANCE.md](docs/MAINTENANCE.md) — diagnosing and re-capturing
- [docs/MIGRATION.md](docs/MIGRATION.md) — from the TypeScript version

## Notes

- **User agent** must stay the bare `Mozilla/5.0`; Cloudflare returns 403 for
  Chrome user agents on the track page and listen endpoint. It is deliberately
  not configurable.
- **Concurrency** is intentionally sequential: the server serialises per track,
  so concurrency adds load without reducing wall time.
- **Retries**: page, listen and range reads retry 3× with backoff; the bulk
  `.hax` transfer has no global timeout because large containers legitimately
  take minutes.
- **Stability**: fast and dependency-light, but it tracks site internals. For a
  slower, more stable alternative see
  [coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader).

## License

MIT — see [LICENSE](../LICENSE).