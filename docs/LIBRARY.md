# Using the crate as a library

The CLI in this repository is a thin wrapper over a Rust library, and the wasm
package and the CLI build from the same protocol code. If your program lives in
Rust, this page is the quickstart; every symbol also carries rustdoc
(`cargo doc --open`).

The crate is `hotaudio-rs` with lib name `hotaudio`. It is not published to
crates.io, so depend on a git path:

```toml
[dependencies]
hotaudio = { git = "https://github.com/dawn-used-yeet-alt/hotaudio-extractor" }
```

Rust 1.87 or newer is required (the repository's MSRV).

## Running a download

The pipeline is `async` at the I/O boundary. On a native build, drive it with
`block_on`; the future never suspends, so underneath this is a plain function
call. On `wasm32`, the host event loop runs the future instead.

```rust,ignore
use hotaudio::download::DownloadOptions;

let client = hotaudio::http::Client::api();
let mut opts = DownloadOptions::default();
let track = hotaudio::block_on(
    hotaudio::download::download_from_page(&client, "https://hotaudio.net/u/user/track-slug", &mut opts),
)?;
std::fs::write("track.m4a", &track.audio).map_err(|e| hotaudio::Error::Io(e.to_string()))?;
```

`download_from_page` is a shorthand for `Clients::single(client).download_from_page(..)`.
The `Clients` split exists because the pipeline's HTTP calls are not alike:

| Client | Used for | Timeout |
| --- | --- | --- |
| `Client::api()` | page fetch, listen, key paging | 30 s per attempt |
| `Client::bulk()` | the 15-25 MB container transfer | none |

Small API calls are latency-bound, so a stalled request should degrade into a
bounded retry. The bulk transfer legitimately takes minutes on a slow link, so
a global timeout there would only cause spurious failures. `Downloaded` carries
the audio plus everything the terminal message reports: `title`, `duration`
(seconds) and `duration_ms`, `codec` and `mime` (`audio/mp4`), `hax_url`,
`segment_count`, `pages` (how many key branches were paged), the merged `keys`
map, and `hax_bytes` when `opts.capture_hax` was set.

## Streaming

`download::download_streaming` hands segments to a callback as they decrypt:
peak memory is one segment instead of one container, and the first playable
fragment exists after one round trip. It takes an existing handshake (next
section) and a cancel token:

```rust,ignore
use std::io::Write;
use hotaudio::download::{download_streaming, Cancel, Progress};
use hotaudio::listen;

let client = hotaudio::http::Client::api();
let hs = hotaudio::block_on(listen::load_handshake(
    &client, "https://hotaudio.net/u/user/track-slug", hotaudio::HOTAUDIO_API_BASE, None,
))?;

// Cooperative cancellation between segments and key pages. Elsewhere (a
// Ctrl-C handler, a UI button), stop.store(true, Relaxed) unwinds the
// download between segments, with no partial output left behind.
let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
let flag = stop.clone();
let cancel = Cancel::new(move || flag.load(std::sync::atomic::Ordering::Relaxed));

let mut sink = std::io::BufWriter::new(std::fs::File::create("track.m4a")?);
let mut report = |p: Progress| eprintln!("{:?}: {}/{}", p.phase, p.loaded, p.total);

let summary = hotaudio::block_on(download_streaming(
    &client, &hs, None, 0,
    |fragment| {
        sink.write_all(fragment).map_err(|e| hotaudio::Error::Io(e.to_string()))
    },
    Some(&mut report),
    Some(cancel),
))?;
```

The fragment callback returns `hotaudio::Result<()>`, so local write failures
map errors into `Error::Io` as above. An `Err` aborts the download; rejecting the
first fragment is how you catch non-MP4 output before it matters. `Streamed`
reports `bytes`, `segment_count`, `duration`, `duration_ms`, `codec`,
`hax_url`, `keys` and `pages`.

## Handshake and track selection

A page can carry several tracks; the handshake is per track. Skip the page
fetch when you already have the HTML:

```rust,ignore
use hotaudio::listen;

// from a URL (fetches the page)…
let hs = hotaudio::block_on(listen::load_handshake(
    &client, url, hotaudio::HOTAUDIO_API_BASE, Some("118346"),
))?;

// …or from HTML you already hold
let hs = listen::handshake_from_html(&html, hotaudio::HOTAUDIO_API_BASE, Some("118346"))?;

// what's on this page?
let tracks = listen::list_tracks(&html); // [(id, key, title), …]
```

`hotaudio::is_hotaudio_url(url)` cheaply answers whether a URL belongs to
hotaudio at all. The returned handshake works with `download_with_handshake`
as well as `download_streaming`.

## Offline decryption

Keys never change for a given track, so a saved keys file enables replays with
no page fetch and no listen calls:

```rust,ignore
// capture: fetch keys only (no audio bytes)
let bundle = hotaudio::block_on(
    hotaudio::download::fetch_keys_only(&client, url, None, None, 0),
)?;
let json = hotaudio::download::keys_to_json(
    &bundle.keys, &bundle.hax_url, bundle.title.as_deref(),
);

// later: decrypt a container you already hold, entirely offline
let saved = hotaudio::download::parse_saved_keys(&json)?;
let (audio, segment_count, duration) =
    hotaudio::download::decrypt_offline(&hax_bytes, &saved.keys, None)?;
```

`download_cached(client, hax_url, &keys, on_progress, capture_hax, cancel)` is
the middle ground: it fetches the container from its URL and decrypts it with
saved keys, with no handshake and no listen calls.

The envelope format round-trips with the CLI's `--save-keys`: both
`parse_saved_keys` input shapes (the CLI envelope and a bare `{"node": "hex"}`
map) are accepted, so `keys.json` files are interchangeable between the two.

## Options

`DownloadOptions` controls one run:

| Field | Purpose |
| --- | --- |
| `on_progress` | `&mut dyn FnMut(Progress)`; `Progress` carries `Phase::Resolving`/`Fetching`/`Decrypting`, `loaded`, `total` |
| `cancel` | `Some(Cancel)` for cooperative cancellation, polled between segments and key pages; the transfer's progress hook doubles as the cancellation point |
| `delay_ms` | Politeness pause before each listen / key-paging request; correctness never depends on it |
| `initial_keys` | Seed the key map (resume from saved keys) |
| `api_base` | Mirror of the listen API |
| `track_id` | Track selection on multi-track pages |
| `capture_hax` | Keep raw container bytes on `Downloaded::hax_bytes` |

Client defaults are overridable per instance: `Client::with_retries` for the
retry policy (three attempts, 500 ms doubling backoff plus jitter capped at
10 s, retrying network errors and statuses `{401, 408, 425, 429, 500, 502,
503, 504}`, honouring `Retry-After` on 429), `Client::with_timeout_ms` for
per-attempt timeout, `Client::with_proxy` for an HTTP(S) CONNECT tunnel.
Standard `ALL_PROXY`/`HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY` variables are
honoured on the native backend, and the proxy setting is a documented no-op on
wasm.

## Errors

`hotaudio::Error` is the error type; `hotaudio::Result<T>` aliases its result.
Variants reachable from the library surface:

| Variant | When |
| --- | --- |
| `Auth` | Ciphertext failed authentication, or was too short to hold a tag |
| `Decode(&'static str)` | A hex or base64 string was malformed |
| `State(String)` | Page state decrypted but was the wrong shape |
| `Listen(String)` | The listen API rejected the request |
| `Protocol(String)` | A required field was missing or unusable |
| `Http(String)` | Transport failure |
| `Sign(String)` | Signing failed |
| `Cancelled` | The cancel token was set; not a failure |

The server's own failure semantics (401 retry, 403 fatal, plaintext bodies as
errors) are in [PROTOCOL.md](PROTOCOL.md#3-listen-request).

## Runnable examples

Three examples in the repository double as a usage reference. They hit the
network and need a real track page, so they are not part of `cargo test`:

```bash
cargo run --example download_track  -- "https://hotaudio.net/u/user/track-slug" track.m4a
cargo run --example stream_track    -- "https://hotaudio.net/u/user/track-slug" track.m4a
cargo run --example decrypt_offline -- audio.hax keys.json track.m4a
```

`examples/stream_track.rs` is also the working shape of the streaming section
above.
