# hotaudio-extractor

<p>
A command-line audio extractor for <a href="https://hotaudio.net">hotaudio.net</a>, written in Rust. Give it a track URL and it writes a playable <code>.m4a</code>.
</p>

> [!NOTE]
> This tool is intended for interoperability and personal archiving. It hosts no content and is not
> affiliated with hotaudio.net.
>
> Download only content you have the right to access, and comply with hotaudio.net's terms of
> service and applicable law. You are responsible for how you use it.

## Install

### 1. Download

Open the [latest release](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest) and
download the archive that matches your system. The target name is part of the file name.

| Your system | Look for |
| --- | --- |
| Linux, 64-bit Intel/AMD | `x86_64-unknown-linux-gnu` |
| Linux, ARM64 (Raspberry Pi, ARM servers) | `aarch64-unknown-linux-gnu` |
| Linux, minimal or container (static) | `x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl` |
| macOS, Apple Silicon (M1 and later) | `aarch64-apple-darwin` |
| macOS, Intel | `x86_64-apple-darwin` |
| Windows, 64-bit Intel/AMD | `x86_64-pc-windows-msvc` (or `-gnu` for MinGW) |
| Windows, ARM64 | `aarch64-pc-windows-msvc` |
| Android (Termux), most phones | `aarch64-linux-android` |
| Android (Termux), x86-64 | `x86_64-linux-android` |

Linux, macOS and Android archives are `.tar.gz`. Windows archives are `.zip`. Files are named
`hotaudio-download-v1.0.0-<target>`, followed by the extension.

Not sure which one you need? On Linux and macOS run `uname -m`: `x86_64` means Intel/AMD, `aarch64` or
`arm64` means ARM.

Each release also includes `SHA256SUMS.txt` if you want to verify the download.

### 2. Extract and run

**Linux and macOS**

```bash
tar xzf hotaudio-download-v1.0.0-<target>.tar.gz
cd hotaudio-download-v1.0.0-<target>
chmod +x hotaudio-download
./hotaudio-download "https://hotaudio.net/u/user/track-slug"
```

**Termux (Android)** uses the same commands. Install `tar` first if it is missing (`pkg install tar`).
Android builds need API 24 or newer.

**Windows**

The `.zip` contains only `hotaudio-download.exe`. Extract it (right-click → Extract All), open a terminal in that folder, then run:

```powershell
.\hotaudio-download.exe "https://hotaudio.net/u/user/track-slug"
```

### 3. (Optional) Run it from anywhere

Put the binary somewhere on your `PATH`.

```bash
# Linux and macOS
mkdir -p ~/.local/bin
cp hotaudio-download ~/.local/bin/

# Termux
cp hotaudio-download $PREFIX/bin/
```

On Windows, move `hotaudio-download.exe` to a folder of your choice and add that folder to your `PATH`
in System Settings → Environment Variables.

After that, `hotaudio-download` works from any directory.

> [!TIP]
> - Add `--out <name>.m4a` if you want to choose the output file name or location.
> - On macOS, if Gatekeeper blocks the binary, clear the quarantine flag with
>   `xattr -d com.apple.quarantine hotaudio-download`.

### Build from source

Requires stable Rust (edition 2024, MSRV 1.85).

```bash
git clone https://github.com/dawn-used-yeet-alt/hotaudio-extractor
cd hotaudio-extractor
cargo build --release   # -> target/release/hotaudio-download
```

## Usage

```bash
# Download and decrypt a track
hotaudio-download "https://hotaudio.net/u/user/track-slug"

# Choose the output file name
hotaudio-download "https://hotaudio.net/u/user/track-slug" --out track.m4a

# Stream to disk: low memory, first playable fragment after one round trip
hotaudio-download "https://hotaudio.net/u/user/track-slug" --stream-to track.m4a

# Save branch keys for later
hotaudio-download "https://hotaudio.net/u/user/track-slug" --save-keys keys.json

# Re-download from saved keys: no page fetch, no listen calls
hotaudio-download --keys keys.json --out track.m4a

# Decrypt a local .hax file offline
hotaudio-download --hax audio.hax --keys keys.json --out track.m4a

# List tracks on a page
hotaudio-download "https://hotaudio.net/u/user/track-slug" --list-tracks
```

> [!TIP]
> Branch keys are deterministic per track and `.hax` URLs are stable, so a saved keys file is enough to
> re-download a track without contacting the page or listen endpoints.

## Notes

> [!WARNING]
> - **The signer is pinned to one build of the upstream player.** If hotaudio.net changes its player,
>   downloads can fail until this project is updated. Check the releases page for a newer version, or
>   open an issue.
>
> - This tool depends on site internals, so it can break when they change.
>   [coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader) is slower but more stable.
>
> - **The user agent is fixed** at `Mozilla/5.0`, because Cloudflare returns 403 for Chrome user agents
>   on the track page and listen endpoint. It is deliberately not configurable. In a browser the header
>   is dropped entirely, since a page cannot set it — see [Browser / wasm](#browser--wasm).

The other operational decisions are deliberate too, and the measurements behind
them are in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#reliability) so they do
not get re-litigated. Requests are sequential by design: the server serialises
per track, and parallel `Range` requests were measured and lost. Page, listen and
range reads retry 3× with backoff; the bulk `.hax` transfer resumes via `Range`
rather than restarting, and has no global timeout because large containers
legitimately take minutes. No proxy is bundled for the browser build — that is
the embedder's infrastructure, not this project's.

## Library

The crate is `hotaudio-rs` with the library name `hotaudio`. It is not published to crates.io; release
binaries are the distribution channel.

```rust
use hotaudio::{block_on, download, http};

block_on(async {
    let client = http::Client::api();
    let mut opts = download::DownloadOptions::default();
    let track = download::download_from_page(&client, url, &mut opts).await?;
    std::fs::write("track.m4a", track.audio)?;
    Ok::<_, hotaudio::Error>(())
})
```

| Module | Responsibility |
| --- | --- |
| [`signer`](src/signer) | Signature VM (`bytecode.rs` is generated, `env.rs` is the environment fingerprint, `vm.rs` is the machine) |
| [`crypto`](src/crypto.rs) | SHA-256, X25519, ChaCha20-Poly1305, hex/base64 |
| [`hax`](src/hax.rs) | Bencode, HAX0 header, segment key tree |
| [`range`](src/range.rs) | HTTP `Range` reads, header probe, streaming decrypt |
| [`listen`](src/listen.rs) | Page handshake and the encrypted listen API |
| [`http`](src/http/mod.rs) | `Client`, retry policy, per-target transport |
| [`download`](src/download.rs) | Pipeline, key paging, saved-keys envelope |
| [`clock`](src/clock.rs) | Clocks and sleeping, per target |
| [`wasm`](src/wasm.rs) | The `wasm-bindgen` surface (`wasm32` only) |

The pipeline is `async` at the I/O boundary so one implementation serves both
transports. On a native build nothing ever suspends, so `block_on` is a single
poll and costs nothing.

## Browser / wasm

The crate builds for `wasm32-unknown-unknown` and exposes a `wasm-bindgen` API.
Segments are handed to a callback as they decrypt, so a page can append them to a
`MediaSource` and start playing after one round trip.

```bash
cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release/hotaudio.wasm
```

```js
import init, { downloadTrack } from "./pkg/hotaudio.js";
await init();

const media = new MediaSource();
video.src = URL.createObjectURL(media);
// ...on sourceopen, append each fragment to a SourceBuffer...
const info = await downloadTrack(
  "https://hotaudio.net/u/user/track",
  (fragment) => sourceBuffer.appendBuffer(fragment),
  (phase, loaded, total) => console.log(phase, `${loaded}/${total}`),
);
```

Also exported: `downloadTrackWith` (proxy/custom-`fetch`), `trackKeys`,
`signPayload` and `listTracks`.

**A browser needs a proxy in front, and this crate does not ship one.** Measured
against the live site: hotaudio.net and cdn.hotaudio.net send no
`Access-Control-Allow-Origin` and refuse the cross-origin preflight, and
`User-Agent` is a forbidden header a page cannot set — so Cloudflare 403s the
browser's own. Put your own origin in front and pass a URL rewrite, or pass a
replacement `fetch` if your host is not a page (a Capacitor native bridge, say):

```js
await downloadTrackWith(url, onFragment, onProgress, null,
  (u) => u.replace("hotaudio.net", "proxy.example"));
```

A compliant proxy must rewrite the user agent, add CORS headers and answer the
preflight, forward `Range` with real `206`s, and stream rather than buffer.
**[docs/BROWSER.md](docs/BROWSER.md)** has the checklist, a MediaSource playback
example, Capacitor integration and troubleshooting.

## Development

```bash
cargo test                    # offline, no network required
bun scripts/verify-live.ts    # manual live check; names the failing stage
```

The same test bodies run on both the native and browser targets, so a divergence
between them fails CI rather than surfacing later. The browser tests need a runner,
because `wasm32` binaries cannot be exec'd:

```bash
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
cargo test --target wasm32-unknown-unknown
```

Suites: `signer_parity` (golden vectors from the reference signer), `hax_roundtrip`,
`key_tree` (150 branch keys from a real 899-segment track), `http_resume` and
`transport_fetch` (the browser transport against a stub `fetch`), and `perf_guard`
(bulk decrypt throughput, so an async or copy regression on the hot path fails).
`cargo test --release --test perf_guard` for the timing half; its correctness half
runs everywhere.

> [!NOTE]
> After a site change, `bytecode.rs`, `env.rs` and the mirrored values in `scripts/shim.ts` must be
> updated together. The full procedure, a symptom-to-cause table and the opcode-semantics pitfalls are in
> [docs/MAINTENANCE.md](docs/MAINTENANCE.md).

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how it fits together
- [docs/BROWSER.md](docs/BROWSER.md): running in a browser, and proxying
- [docs/PROTOCOL.md](docs/PROTOCOL.md): the wire protocol and the signature VM
- [docs/MAINTENANCE.md](docs/MAINTENANCE.md): diagnosing and re-capturing
- [docs/MIGRATION.md](docs/MIGRATION.md): moving from the TypeScript version

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Agent guidance is in [AGENTS.md](AGENTS.md). User-visible
changes go under `CHANGELOG.md` → `Unreleased`.

## License

MIT. See [LICENSE](LICENSE).
