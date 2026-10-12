# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Browser/wasm build** (`wasm32-unknown-unknown`). The same pipeline now runs
  over the browser's `fetch` instead of blocking `ureq`, with a `wasm-bindgen`
  surface — `downloadTrack`, `downloadTrackWith`, `trackKeys`, `signPayload`,
  `listTracks`. Segments are handed to a callback as they decrypt, so a page can
  `appendBuffer` them into a `MediaSource` and play progressively. This closes the
  "browser playback" non-goal recorded in `docs/ARCHITECTURE.md`.
- `Client::with_url_rewrite`, for pointing the crate at a proxy or CDN mirror
  without forking it, and `Client::with_fetch` (wasm) for hosts whose HTTP is not
  the page's `fetch` — a Capacitor native bridge, for instance.
- `docs/BROWSER.md`: the browser build end to end — MediaSource playback, the
  proxy contract with the CORS and `User-Agent` measurements behind it, Capacitor
  integration, and troubleshooting.

### Changed

- **The pipeline is `async` at the I/O boundary.** `listen`, `range`, `download`
  and `http` entry points return futures. Native behaviour and performance are
  unchanged — the transport blocks, so a native future never suspends and
  `hotaudio::block_on` is a single poll. `download_streaming` takes an
  `on_fragment` callback instead of a `&mut Write`, which is what enables MSE.
- `http::api_agent()`/`bulk_agent()` become `http::Client::api()`/`.bulk()`.
- Tests carry both `#[test]` and `#[wasm_bindgen_test]` attributes and run on both
  targets from one body; CI gained a wasm job.

## [1.0.0] - 2026-10-07

First release of the Rust implementation, which replaces the TypeScript package
now preserved on the `legacy` branch. See [docs/MIGRATION.md](docs/MIGRATION.md).

### Added

- **Rust implementation** — page handshake, X25519 listen key exchange, HAX0
  container decrypt, and a `hotaudio-download` CLI that writes playable `.m4a`.
  Roughly 1000× faster at signing (0.2 ms vs 200–500 ms) and ~4× faster at
  decryption than the TypeScript version.
- **Signature VM** — the signer is a port of the player's 657-instruction
  register machine plus the same recovered 1314-entry bytecode, so it is exact
  by construction rather than a re-derivation of "the algorithm". Pinned to
  golden vectors captured from the reference signer, plus a differential trace of
  all 1570 VM instructions.
- **Resumable container transfer** (`http::get_resumable`): a dropped `.hax`
  download continues from the last byte received via `Range` instead of
  restarting. Falls back to a full refetch if the server ignores `Range`.
- **Streaming downloads** (`--stream-to`): peak memory of one segment instead of
  one container, and a first playable fragment after a single round trip.
- **Library API** (`hotaudio`): `download`, `listen`, `hax`, `range`, `http` and
  `signer` modules.
- A self-contained recovery toolchain (`scripts/`) with the player bundle
  vendored, so signer recovery does not depend on the TypeScript implementation.
- Docs: `ARCHITECTURE`, `PROTOCOL`, `MAINTENANCE`, `MIGRATION`.
- Prebuilt binaries for Linux (glibc and static musl), macOS, Windows (MSVC and
  MinGW), and Android/Termux, on x86-64 and ARM64, built in CI and attached to
  the GitHub release with checksums.

### Changed

- `--help` documents every flag. Flags accept both `--flag value` and
  `--flag=value`, and unknown or valueless flags are rejected rather than
  ignored.

## [0.1.0] - 2026-10-06

The TypeScript implementation, superseded by 1.0.0 and preserved on the `legacy`
branch.

### Added

- Initial standalone extractor: page handshake, `nozzle.js` request signing,
  X25519 listen key exchange, HAX0 decrypt.
- Full-track download (`downloadHotaudioBuffer`, `downloadHotaudioToFile`), offline
  decrypt (`decryptHaxBuffer`, `fetchHotaudioKeys`), browser MSE streaming
  (`extractHotaudioStream`, `extractHotaudio`).
- `hotaudio-download` CLI with online and offline (`--hax`/`--keys`) modes.
- Offline unit test suite (`bun test`) for codec helpers, bencode, HAX0 parsing,
  key derivation, and handshake helpers.
- `docs/` (API, CLI, architecture), `CONTRIBUTING.md`, MIT `LICENSE`, and the CI
  typecheck/test/build workflow.
- `tsup` build emitting `dist/` with types; package `exports`/`files`/`engines`
  metadata.
- `scripts/live-probe.ts` staged live-site diagnostic (per-stage timing, failure
  hints, JSON report); manual use only.
- Network resilience: `fetchWithRetry` retries transient failures (network
  errors, 429/5xx) with backoff across page, listen, `.hax`, and range fetches;
  small API calls carry a 30s per-attempt timeout so stalls degrade into bounded
  retries.
- Faster long-track downloads: exact-miss sequential key paging replaces the
  stride lookahead (fewer requests, less wall time).
- Committed offline integration test: full download + deterministic paging
  against a mocked network.
- Key cache: `SavedHotaudioKeys` envelope, `parseSavedKeys`, `downloadHaxBuffer`,
  CLI cached (`--keys`/`--hax-url`) and simplified offline (`--hax`) modes;
  `--save-keys` writes the envelope.
- Track selection: `trackId` option and CLI `--track` for multi-track pages.
- Track discovery: `fetchHotaudioTracks` / `listHotaudioTracks` and CLI
  `--list-tracks`; `haxUrlForTrackKey` derives the container URL without a listen
  call.
- Resume: `initialKeys` seeds the key map so interrupted downloads refetch only
  missing branches (CLI online mode accepts `--keys`).
- Player-faithful listen: `?key=` forwarding, 401 retries, plaintext error
  surfacing, conditional response decrypt.

[Unreleased]: https://github.com/dawn-used-yeet-alt/hotaudio-extractor/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/tag/v1.0.0
[0.1.0]: https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/tag/v0.1.0
