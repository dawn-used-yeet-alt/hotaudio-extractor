# hotaudio-extractor

A standalone [hotaudio.net](https://hotaudio.net) audio extractor. It performs
the track-page handshake, request signing, X25519 listen key exchange, and HAX0
container decryption, and writes a playable `.m4a`.

This is the **Rust** implementation, and the current `main` branch.

The original TypeScript npm package is preserved on the **`legacy`** branch. It is
frozen, and exists only for its browser/MSE playback path, which has no Rust
equivalent. See [docs/MIGRATION.md](docs/MIGRATION.md) for the mapping between the
two, and `git checkout legacy` to work on it.

> **Please respect creators.** This is an interoperability and personal-archiving
> tool. Download only content you have the right to access, and comply with
> hotaudio.net's terms of service and applicable law.

## Why the Rust implementation is interesting

Upstream, the `X-Signature` header comes from evaluating a ~106 KB obfuscated
JavaScript bundle inside a fake-browser sandbox. That bundle is obfuscated to
hide its payload, but the payload is **not a hash call** — it is a **register
machine**:

- a **1314-entry program** (657 two-byte instructions), recovered from the bundle
- an **80-slot register file**; opcodes below 80 are `MOV`, 80 and above dispatch
- an instruction set of SHA-256 compression, string building and branches — plus
  a probe that walks stubbed globals to prove the environment looks like a
  browser
- output: `"9:" + big-endian u32 seconds + 12 bytes`

This crate re-implements **the machine and the same program** rather than
guessing at "the algorithm". That makes it exact by construction, and much
faster:

| | TypeScript (bun) | Rust |
| --- | --- | --- |
| one signature | 200–500 ms (occasionally 123 s) | **~0.2 ms** |
| decrypt a 14.8 MB `.hax` | 0.80 s | **0.19 s** |

Correctness is pinned against the real thing: golden vectors captured from the
reference signer over a corpus covering SHA-256 padding boundaries, non-ASCII
input and JSON escaping, plus a differential trace of all 1570 VM instructions.
Validated end-to-end against the live API (899-segment track → valid 14.8 MB
AAC/MP4).

## Install

```bash
git clone https://github.com/dawn-used-yeet-alt/hotaudio-extractor
cd hotaudio-extractor
cargo build --release        # -> target/release/hotaudio-download
```

Requires stable Rust (edition 2024, MSRV 1.85 — CI checks against it). Runtime
dependencies are the RustCrypto crates plus `ureq`; there is no bundled
JavaScript engine, because the signer is native.

The crate is `hotaudio-rs`, with its library importable as `hotaudio`. It is not
published to crates.io (`publish = false`); build from source with the command
above.

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
| [`signer`](src/signer) | the signature VM — [`bytecode.rs`](src/signer/bytecode.rs) is generated, [`env.rs`](src/signer/env.rs) is the environment fingerprint, [`vm.rs`](src/signer/vm.rs) is the machine |
| [`crypto`](src/crypto.rs) | SHA-256, X25519, ChaCha20-Poly1305, hex/base64 |
| [`hax`](src/hax.rs) | bencode, HAX0 header, segment key tree |
| [`range`](src/range.rs) | HTTP `Range` reads, header probe, streaming decrypt |
| [`listen`](src/listen.rs) | page handshake and the encrypted listen API |
| [`http`](src/http.rs) | agents and retry policy |
| [`download`](src/download.rs) | pipeline, key paging, saved-keys envelope |

## Tests

```bash
cargo test
```

Offline; no network required.

| Suite | Covers |
| --- | --- |
| [`tests/signer_parity.rs`](tests/signer_parity.rs) | golden vectors from the reference signer, plus signature shape and clock semantics |
| [`tests/hax_roundtrip.rs`](tests/hax_roundtrip.rs) | container round-trip, corruption, wrong keys |
| [`tests/key_tree.rs`](tests/key_tree.rs) | derivation against 150 branch keys from a real 899-segment track |
| [`tests/http_resume.rs`](tests/http_resume.rs) | resumable transfer, and the "mirror ignores `Range`" fallback |
| unit tests in [`src/`](src) | bencode, URL parsing, tree geometry, escaping |

Live checks are explicit, manual, and never run in CI:

```bash
bun scripts/verify-live.ts          # names the failing stage
```

## When the site changes

The signer is pinned to one upstream player build. `bytecode.rs`, `env.rs` and
the mirrored values in `scripts/shim.ts` move together:

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

The tooling is self-contained: it needs only [`vendor/nozzle.js`](vendor) and
[`scripts/shim.ts`](scripts/shim.ts), both vendored here. It does not require the
TypeScript implementation.

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — how it fits together
- [docs/PROTOCOL.md](docs/PROTOCOL.md) — the wire protocol and the signature VM
- [docs/MAINTENANCE.md](docs/MAINTENANCE.md) — diagnosing and re-capturing
- [docs/MIGRATION.md](docs/MIGRATION.md) — moving from the TypeScript version
- `legacy` branch — the TypeScript npm package and its `docs/`

## Operational notes

These are deliberate decisions, not oversights. The measurements behind them are
recorded in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#reliability) so they do
not get re-litigated.

- **User agent** stays the bare `Mozilla/5.0`; Cloudflare returns 403 for Chrome
  user agents on the track page and listen endpoint. It is deliberately not
  configurable — a configurable one would break the extractor, so the option was
  misleading.
- **Concurrency** is intentionally sequential: the server serialises per track,
  so concurrency adds load without reducing wall time. Parallel `Range` requests
  were measured and lost (6.3 MB/s on one connection vs 2.38 MB/s on sixteen).
- **Retries**: page, listen and range reads retry 3× with backoff. The bulk
  `.hax` transfer resumes via `Range` instead of restarting, and has no global
  timeout because large containers legitimately take minutes.
- **Stability**: fast and dependency-light, but it tracks site internals. For a
  slower, more stable alternative see
  [coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Agent guidance is in [AGENTS.md](AGENTS.md);
user-visible changes go under `CHANGELOG.md` → `Unreleased`.

## License

MIT — see [LICENSE](LICENSE).
