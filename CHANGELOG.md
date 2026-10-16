# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **`download::Clients`**, a split-client entry point (`Clients { api, bulk }`)
  whose `download_from_page` / `download_with_handshake` methods run the full
  pipeline with small API calls on one client and the bulk container transfer on
  another. `src/bin/download.rs` now uses it instead of reimplementing the
  pipeline (`download_from_page_split` / `download_with_agents` are gone), which
  is what let `--track` and the resumable transfer drift out of sync. The
  single-client `download::download_from_page` and
  `download::download_with_handshake` remain as `Clients::single` wrappers.

- **The HAX0 extra metadata region is surfaced.** `Hax0` keeps the
  still-encrypted `extraLength` region as `Hax0::extra`, and `KeyTree::extra_node`
  / `KeyTree::derive_node` derive the node key (`tree_base - 1`, "segment" `-1`)
  that decrypts it. The pinned player uses this blob for `ebur128` loudness
  visualisation; it is not audio, so the pipeline still skips it.
  `Hax0::base_key` is now documented as parsed-but-unused: the player only logs
  it and never derives from it.

- **Input URLs are checked early.** `is_hotaudio_url` is now used by the CLI and
  the `downloadTrack` / `trackKeys` wasm exports to warn when a page URL does not
  look like a canonical share link, so a typo surfaces before the pipeline fails
  deep inside. The matcher also tolerates a trailing slash. It stays a hint, not
  a gate: proxies and mirrors are supported, so the request still goes ahead.

- **The CDN `.hax`-URL tripwire now fires in normal use.** After the handshake,
  `download` compares the server-issued `.hax` URL with the one derivable from
  the track key (`listen::hax_url_for_key`) and warns (stderr natively,
  `console.warn` on `wasm32`) when they disagree, which means the CDN layout
  moved. It keeps fetching the server-issued URL.

- **`--version` / `-V`.** The binary prints `hotaudio-download <version>` from
  `CARGO_PKG_VERSION`, and `--help` lists the flag.

- **`--quiet` / `-q` and `--verbose` / `-v`.** `--quiet` silences the stderr
  progress reporter; `--verbose` adds a stderr line with the segment count, key
  page count and elapsed time (the two are mutually exclusive). To feed that,
  `download::Downloaded` now exposes `pages`, the number of key branches that
  had to be paged.

- **`--json` machine-readable output.** `--json` prints a result object on
  stdout matching the wasm `downloadTrack` shape (`title`, `durationSecs`,
  `codec`, `mime`, `bytes`, `segmentCount`, `keyPages`, `haxUrl`, `keysJson`)
  plus `savedTo`, the path written. `--list-tracks --json` prints
  `[{ id, key, title }]`, and `--progress=json` streams NDJSON progress events
  on stderr. `download::Downloaded` gained `codec`, and
  `download::download_cached` now returns `Downloaded` so every mode reports the
  same result shape.

- **`--out -` and `--stream-to -` write to stdout.** The decrypted audio can be
  piped (`hotaudio-download <url> --out - | ffplay -`). Progress already went to
  stderr, and the result line moves there too when stdout carries the audio, so
  the stream is never corrupted.

- **`--save-hax PATH`.** Writes the raw `.hax` container alongside the decrypted
  audio so it can be decrypted offline later (with `--save-keys`). Available in
  every buffered mode; rejected with `--stream-to`, which never buffers the
  container. `DownloadOptions` gained `capture_hax`, `Downloaded` gained
  `hax_bytes`, and `download_cached` takes a `capture_hax` flag to carry it.

- **`--keys-only`.** With `--save-keys`, it does the handshake and first listen
  and writes the envelope without fetching the container or decrypting, so keys
  can be archived without a full download. `--track` selects a track. It reuses
  `download::fetch_keys_only`, which gained a `track_id` argument. It needs
  `--save-keys` and conflicts with `--hax`, `--keys` and `--stream-to`.

- **Several sources and `--input FILE`.** `hotaudio-download` now takes multiple
  positional URLs (or HTML files) and processes them in order, and `--input FILE`
  reads one source per line (`-` is stdin, blank lines skipped). It continues
  past a failing source and exits non-zero if any failed. Flags that name a
  single path (`--out`, `--stream-to`, `--save-keys`, `--save-hax`, `--keys-only`,
  `--list-tracks`) are rejected when more than one source is given. Two titles
  that sanitise to the same filename make the later source fail instead of
  silently overwriting the earlier one.

- **`--all-tracks`, download every track on one page.** Track ids come from
  `listen::list_tracks` and are fetched in page order, each with its own
  handshake and its own title-derived file. It continues past a failing track
  and exits non-zero if any failed. It takes exactly one source and conflicts
  with `--track`; like several sources, it rejects the single-path flags
  (`--out`, `--stream-to`, `--save-keys`, `--save-hax`, `--keys`, `--hax`,
  `--keys-only`, `--list-tracks`), since each track needs its own path.

- **`--no-clobber` and `--force`.** An existing output file is still replaced by
  default; `--no-clobber` makes it an error instead, for the decrypted audio, the
  saved-keys envelope and the `.hax` container alike, and `--force` is the
  explicit spelling of the default (the two are mutually exclusive). Missing
  parent directories are now created, so `--out a/b/track.m4a` works when `a/b`
  does not exist yet.

- **Hardened filenames and `--filename-template`.** Title-derived names now drop
  control characters and `:`, strip leading and trailing dots and spaces, escape
  Windows reserved device names (`CON`, `NUL`, `COM1`…`LPT9`, `_`-prefixed), and
  cap the stem at 200 bytes with a deterministic `~<hash>` suffix instead of
  truncating. `--filename-template` shapes title-derived outputs with `{title}`,
  `{index}` (1-based) and `{ext}`; `/` separates directories (created as needed),
  a title can never inject one, and `--out` still wins. Unknown or unclosed
  placeholders are rejected before any network work.

- **`--proxy` and proxy environment variables.** The native transport honours
  `ALL_PROXY`/`HTTPS_PROXY`/`HTTP_PROXY` (and `NO_PROXY`) by default, and
  `--proxy URL` — or `http::Client::with_proxy` — overrides them with an explicit
  HTTP(S) CONNECT proxy. A malformed URL, or a scheme this build cannot speak
  (SOCKS needs a `ureq` feature that is off), is an error. On `wasm32` the
  browser's `fetch` owns proxying, so `Client::with_proxy` is a documented no-op.

- **Configurable retry and timeout policy.** `http::Retry` (attempts, backoff
  base and cap) is the policy as one value, applied with
  `Client::with_retries`; the old `ATTEMPTS`/`BASE_DELAY_MS`/`MAX_DELAY_MS` are
  now just its defaults. `hotaudio-download` gains `--retries N` (retries after
  the first attempt, default 2) and `--timeout SECS` (per-attempt API timeout,
  default 30). The bulk container transfer keeps its deliberate no-timeout;
  `--retries` still applies to it.

- **Politeness delay.** `--delay MS` (or `download::DownloadOptions::delay_ms`)
  pauses before each listen/key-paging request, defaulting to 0. It only spaces
  load for bulk/batch runs; correctness never depends on it.

### Fixed

- **`--stream-to` writes atomically.** Streamed output goes to a sibling
  `<dest>.part` and is renamed onto the destination only once the download
  succeeds; a failed or interrupted stream no longer leaves a truncated `.m4a`
  in its place. `-` (stdout) is unchanged, since a pipe cannot be made atomic.

- **A literal `-` is accepted as a flag value.** Validation rejected any value
  starting with `-` as a missing value, so `--out -` (stdout, added for piping)
  never worked; `-` is now allowed, while a longer dash-word in the value
  position is still an error.

- **Truncated transfers and non-MP4 output are rejected.** `http::get_resumable`
  used `Content-Length` only for progress, so a body that ended cleanly but
  short of the announced length was returned as success; it now compares the
  final length and resumes for the remainder within the existing retry budget,
  erroring if it never completes. The decrypted audio is also checked for an MP4
  `ftyp` box at offset 4 before it is returned or streamed, mirroring
  `scripts/verify-live.ts`.

- **Key paging no longer gives up after one draw.** The branch window returned
  for an index rotates in width (8/32/128), so a single draw can miss the leaves
  that need it; `decrypt_container` and `download_streaming` treated that first
  miss as terminal. Sequential downloads usually recovered by luck on a later
  segment, but random access (seeking) into an arbitrary segment failed
  intermittently with `no key branch covered segment N`. Both paths now redraw
  the same index through one shared helper (`download::derive_or_page`, bounded
  by `KEY_PAGE_ATTEMPTS`), so the policy lives in the crate instead of in each
  caller. A transport error still propagates unchanged rather than being retried
  and reported as an uncovered segment.

- **`--track` now works for URL-based online downloads.** `listen::load_handshake`
  built the handshake with no track id, so both the buffered and streamed online
  paths ignored `--track`/`DownloadOptions::track_id` and always picked the
  page's first track; only HTML-file input honoured it. The requested id is now
  threaded through `load_handshake` and the online pipeline, and
  `download::download_from_page` selects the track instead of merely rejecting a
  mismatched id. A requested track the page does not carry is still an error
  (`track <id> not on page`).

- **The CLI's buffered download is resumable again.** `download_with_agents` in
  `src/bin/download.rs` fetched the `.hax` container with a plain bulk `GET`, so
  a dropped transfer restarted from byte zero while the library path
  (`download::fetch_container` → `get_resumable`) continued from the last byte.
  The CLI now goes through `download::fetch_container`, so both share one
  transfer path and the CLI cannot drift out of sync with it.

- **Saved-keys envelopes carry a real timestamp and validate their version.**
  `download::keys_to_json` wrote `"savedAt": ""`; it now writes an ISO-8601 UTC
  timestamp (`clock::iso8601_utc`, pure integer arithmetic so it is identical on
  both targets). `download::parse_saved_keys` now rejects an envelope whose
  `version` is not `1` instead of silently misreading it; a bare
  `{ index: hex }` map with no `version` is still accepted.

## [1.1.0] - 2026-10-12

The browser build. One implementation, two transports: the pipeline is `async`
at the I/O boundary, so the same code drives blocking `ureq` natively and
`fetch` on `wasm32-unknown-unknown`. `crypto`/`hax`/`signer` stay synchronous and
cfg-free, which keeps the browser a second *transport* rather than a second
implementation of the protocol, and keeps the CPU-bound half identical on both.

Native behaviour and performance are unchanged, and verified so: an 899-segment
download produces the same valid 14.7 MB MP4, and a cached re-download is
byte-identical to it.

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
- A CI job that builds, clippy-checks and *runs* the browser test suite, and a
  throughput guard for the decrypt path.

### Changed

- **The pipeline is `async` at the I/O boundary.** `listen`, `range`, `download`
  and `http` entry points return futures. On native nothing ever suspends, so
  `hotaudio::block_on` is a single poll and costs nothing; it panics rather than
  hangs if a future ever does suspend. `download_streaming` takes an
  `on_fragment` callback instead of a `&mut Write`, which is what enables MSE.
- `http::api_agent()`/`bulk_agent()` become `http::Client::api()`/`.bulk()`.
- Tests carry both `#[test]` and `#[wasm_bindgen_test]` attributes and run on both
  targets from one body, so a browser/CLI divergence fails CI rather than
  surfacing later. 52 tests run on wasm, 59 native.

### Notes

- A browser needs a same-origin proxy in front: hotaudio.net sends no
  `Access-Control-Allow-Origin`, refuses the cross-origin preflight, and returns
  403 for the user agent a browser is forced to send. This project does not ship
  a proxy — see `docs/BROWSER.md`.

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

[Unreleased]: https://github.com/dawn-used-yeet-alt/hotaudio-extractor/compare/v1.1.0...HEAD
[1.1.0]: https://github.com/dawn-used-yeet-alt/hotaudio-extractor/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/tag/v1.0.0
[0.1.0]: https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/tag/v0.1.0
