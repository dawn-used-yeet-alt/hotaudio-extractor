# Changelog

All notable changes to this project are documented here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Offline unit test suite (`bun test`) for codec helpers, bencode, HAX0 parsing, key derivation, and handshake helpers.
- `docs/` (API, CLI, architecture), `CONTRIBUTING.md`, MIT `LICENSE`, and CI typecheck/test/build workflow.
- `tsup` build emitting `dist/` with types; package `exports`/`files`/`engines` metadata.
- `AGENTS.md` agent guidance; README notes the fast-but-unstable tradeoff and points at coldvideo-downloader for slow-and-stable.
- `scripts/live-probe.ts` staged live-site diagnostic (per-stage timing, failure hints, JSON report); manual use only.
- Network resilience: `fetchWithRetry` retries transient failures (network errors, 429/5xx) with backoff across page, listen, `.hax`, and range fetches; small API calls carry a 30s per-attempt timeout so stalls degrade into bounded retries.
- Faster long-track downloads: exact-miss sequential key paging replaces the stride lookahead (fewer requests, less wall time — see `docs/ARCHITECTURE.md`).
- Committed offline integration test: full download + deterministic paging against a mocked network (`tests/download.test.ts`).
- Key cache: `SavedHotaudioKeys` envelope, `parseSavedKeys`, `downloadHaxBuffer`, CLI cached (`--keys`/`--hax-url`) and simplified offline (`--hax`) modes; `--save-keys` writes the envelope.
- Track selection: `trackId` option and CLI `--track` for multi-track pages.
- Track discovery: `fetchHotaudioTracks` / `listHotaudioTracks` and CLI `--list-tracks`; `haxUrlForTrackKey` derives the container URL without a listen call.
- Resume: `initialKeys` seeds the key map so interrupted downloads refetch only missing branches (CLI online mode accepts `--keys`).
- Player-faithful listen: `?key=` forwarding, 401 retries, plaintext error surfacing, conditional response decrypt.

### Changed

- Tightened source comments to concise API documentation; no behavior changes.

## [0.1.0] - 2026-10-06

### Added

- Initial standalone extractor: page handshake, `nozzle.js` request signing, X25519 listen key exchange, HAX0 decrypt.
- Full-track download (`downloadHotaudioBuffer`, `downloadHotaudioToFile`), offline decrypt (`decryptHaxBuffer`, `fetchHotaudioKeys`), browser MSE streaming (`extractHotaudioStream`, `extractHotaudio`).
- `hotaudio-download` CLI with online and offline (`--hax`/`--keys`) modes.
