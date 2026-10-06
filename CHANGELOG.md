# Changelog

All notable changes to this project are documented here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Offline unit test suite (`bun test`) for codec helpers, bencode, HAX0 parsing, key derivation, and handshake helpers.
- `docs/` (API, CLI, architecture), `CONTRIBUTING.md`, MIT `LICENSE`, and CI typecheck/test/build workflow.
- `tsup` build emitting `dist/` with types; package `exports`/`files`/`engines` metadata.
- `AGENTS.md` agent guidance; README notes the fast-but-unstable tradeoff and points at coldvideo-downloader for slow-and-stable.
- `scripts/live-probe.ts` staged live-site diagnostic (per-stage timing, failure hints, JSON report); manual use only.

### Changed

- Tightened source comments to concise API documentation; no behavior changes.

## [0.1.0] - 2026-10-06

### Added

- Initial standalone extractor: page handshake, `nozzle.js` request signing, X25519 listen key exchange, HAX0 decrypt.
- Full-track download (`downloadHotaudioBuffer`, `downloadHotaudioToFile`), offline decrypt (`decryptHaxBuffer`, `fetchHotaudioKeys`), browser MSE streaming (`extractHotaudioStream`, `extractHotaudio`).
- `hotaudio-download` CLI with online and offline (`--hax`/`--keys`) modes.
