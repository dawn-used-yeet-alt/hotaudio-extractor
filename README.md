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

## Status

This is the Rust implementation and the current `main` branch. The original TypeScript npm package is
frozen on the `legacy` branch, kept only for its browser/MSE playback path, which has no Rust
equivalent. See [docs/MIGRATION.md](docs/MIGRATION.md) for how the two map to each other.

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

## Library

The crate is `hotaudio-rs` with the library name `hotaudio`. It is not published to crates.io; release
binaries are the distribution channel.

```rust
use hotaudio::{download, http};

let agent = http::api_agent();
let mut opts = download::DownloadOptions::default();
let result = download::download_from_page(&agent, url, &mut opts)?;
std::fs::write("track.m4a", result.audio)?;
```

| Module | Responsibility |
| --- | --- |
| [`signer`](src/signer) | Signature VM (`bytecode.rs` is generated, `env.rs` is the environment fingerprint, `vm.rs` is the machine) |
| [`crypto`](src/crypto.rs) | SHA-256, X25519, ChaCha20-Poly1305, hex/base64 |
| [`hax`](src/hax.rs) | Bencode, HAX0 header, segment key tree |
| [`range`](src/range.rs) | HTTP `Range` reads, header probe, streaming decrypt |
| [`listen`](src/listen.rs) | Page handshake and the encrypted listen API |
| [`http`](src/http.rs) | Agents and retry policy |
| [`download`](src/download.rs) | Pipeline, key paging, saved-keys envelope |

## Development

```bash
cargo test                    # offline, no network required
bun scripts/verify-live.ts    # manual live check; names the failing stage
```

> [!NOTE]
> After a site change, `bytecode.rs`, `env.rs` and the mirrored values in `scripts/shim.ts` must be
> updated together. The full procedure, a symptom-to-cause table and the opcode-semantics pitfalls are in
> [docs/MAINTENANCE.md](docs/MAINTENANCE.md).

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how it fits together
- [docs/PROTOCOL.md](docs/PROTOCOL.md): the wire protocol and the signature VM
- [docs/MAINTENANCE.md](docs/MAINTENANCE.md): diagnosing and re-capturing
- [docs/MIGRATION.md](docs/MIGRATION.md): moving from the TypeScript version

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Agent guidance is in [AGENTS.md](AGENTS.md). User-visible
changes go under `CHANGELOG.md` → `Unreleased`.

## License

MIT. See [LICENSE](LICENSE).
