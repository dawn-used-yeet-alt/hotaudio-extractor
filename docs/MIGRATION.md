# Migration from the TypeScript implementation

The Rust crate replaces the TypeScript npm package, which is preserved on the
`legacy` branch (`git checkout legacy`). The network protocol, container format
and saved-keys format are unchanged; the differences are in API shape, tooling and
what no longer exists.

If you need browser/MSE playback, the `legacy` branch is still the only place it
exists — see [What is gone](#what-is-gone).

## What carries over unchanged

- Track page URLs and the `?key=` private-track parameter.
- The saved-keys envelope JSON (`version`, `haxUrl`, `title`, `savedAt`, `keys`).
  `parse_saved_keys` accepts both the envelope and a bare `{ index: hex }` map,
  exactly as before, so `keys.json` files are interchangeable.
- CLI flag spellings: `--out`, `--save-keys`, `--keys`, `--hax`, `--hax-url`,
  `--track`, `--api-base`, `--list-tracks`.
- The `Mozilla/5.0` user agent requirement, and the reason for it.

## CLI

Flags are unchanged. Two additions:

```bash
# Stream instead of buffering: low memory, first playable fragment early.
hotaudio-download <url> --stream-to track.m4a
```

Behavioural difference: when no `--out` is given the output is named from the
track title in both versions.

## Library

The TypeScript API was promise-based and browser-aware. The Rust one is
blocking and has no DOM surface.

| TypeScript | Rust |
| --- | --- |
| `downloadHotaudioBuffer(url, opts?)` | `download::download_from_page(&agent, url, &mut opts)?` |
| `downloadWithHandshake(hs, opts?)` | `download::download_with_handshake(&agent, &hs, &mut opts)?` |
| `downloadHaxBuffer(url, keys, opts?)` | `download::download_cached(&agent, url, &keys, None)?` |
| `decryptHaxBuffer(bytes, keys, onProgress?)` | `download::decrypt_offline(&bytes, &keys, None)?` |
| `loadHotaudioHandshake(url, opts?)` | `listen::load_handshake(&agent, url, api_base)?` |
| `loadHandshakeFromHtml(html, apiBase?)` | `listen::handshake_from_html(&html, api_base, track_id)?` |
| `listenRequest(hs, first, opts?)` | `listen::listen(&agent, &hs, first)?` |
| `listHotaudioTracks(html)` | `listen::list_tracks(&html)` |
| `fetchHotaudioKeys(url, opts?)` | `download::fetch_keys_only(&agent, url, api_base)?` |
| `parseSavedKeys(text)` | `download::parse_saved_keys(&text)?` |
| `signHotaudioPayload(payload, ts?)` | `signer::sign(payload, ts)` |
| `parseHax0Header(buf)` | `hax::Hax0::parse(&buf)` |
| `deriveSegmentKey(...)` | `hax::KeyTree::derive(&keys, idx, &mut cache)` |
| `HOTAUDIO_PATTERN` / `isHotaudioUrl(u)` | `is_hotaudio_url(u)` |

Options map as follows:

| TypeScript | Rust |
| --- | --- |
| `fetchFn` | inject an `ureq::Agent` instead; nothing else is configurable |
| `userAgent` | fixed at `HOTAUDIO_UA` (see below) |
| `apiBase` | `DownloadOptions::api_base` |
| `timeoutMs` | agent-level (`http::api_agent` vs `http::bulk_agent`) |
| `onProgress` | `DownloadOptions::on_progress`, a `&mut dyn FnMut(Progress)` |
| `initialKeys` | `DownloadOptions::initial_keys` |
| `trackId` | `DownloadOptions::track_id` |

## What is gone

- **Browser / MSE playback.** `extractHotaudioStream`, `extractHotaudio`,
  `extractHotaudioDownload` and the Blob/data-URL results depend on DOM APIs and
  have no server-side equivalent. The *capability* they provided — incremental
  segment delivery — is available as `range::decrypt_streaming` and
  `download::download_streaming`, and via `--stream-to`. True in-browser playback
  remains available only on the `legacy` branch (`legacy:src/stream.ts`).
- **`HotaudioPattern` regex export.** Replaced by `is_hotaudio_url`.
- **`fetchWithRetry` / `isRetryableStatus` as public API.** The behaviour is
  preserved inside `http::request`; `http::is_retryable` is public.
- **Configurable user agent.** Hard-coded to `Mozilla/5.0` on purpose. The
  TypeScript default was load-bearing (Cloudflare 403s Chrome UAs) and
  overriding it broke the extractor, so the option was misleading.

## Tooling

The `legacy` branch had `scripts/live-probe.ts`. It is replaced by:

| Script | Purpose |
| --- | --- |
| `bun scripts/verify-live.ts` | staged live diagnostic; names the failing stage |
| `bun scripts/recapture.ts` | re-capture the signer program from a new bundle |
| `bun scripts/gen-golden.ts` | regenerate signer parity vectors |
| `cargo test` | offline test suite |

`recapture.ts` and `verify-live.ts` depend only on `scripts/shim.ts` and
`vendor/nozzle.js`, both vendored into this directory. They do **not** require the
TypeScript implementation, so the Rust version can be maintained standalone.

## Building

```bash
cargo build --release      # binary at target/release/hotaudio-download
```

Requires a current stable Rust (edition 2024, MSRV 1.85). Runtime dependencies
are the RustCrypto crates plus `ureq`; there is no bundled JavaScript engine,
because the signer is native. The crate is `hotaudio-rs` and is not published to
crates.io, so build from a clone of this repository rather than installing it.