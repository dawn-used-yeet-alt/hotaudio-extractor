# hotaudio-extractor

Save audio from [hotaudio.net](https://hotaudio.net) as a playable `.m4a`. Paste a track link, get a file.

> [!NOTE]
> Made for personal archiving. It hosts nothing and isn't affiliated with hotaudio.net.
> Only download what you have the right to, and follow the site's terms and your local law.

## Install

Grab the file for your system from the [latest release](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest). The target name is part of the file name.

| System | Look for |
| --- | --- |
| Windows, 64-bit Intel/AMD | `x86_64-pc-windows-msvc` (or `-gnu` for MinGW) |
| Windows, ARM64 | `aarch64-pc-windows-msvc` |
| macOS, Apple Silicon (M1 or newer) | `aarch64-apple-darwin` |
| macOS, Intel | `x86_64-apple-darwin` |
| Linux, 64-bit Intel/AMD | `x86_64-unknown-linux-gnu` |
| Linux, ARM64 (Raspberry Pi, ARM servers) | `aarch64-unknown-linux-gnu` |
| Linux, minimal or container (static) | `x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl` |
| Android (Termux), most phones | `aarch64-linux-android` |
| Android (Termux), x86-64 | `x86_64-linux-android` |

Windows downloads are `.zip`; everything else is `.tar.gz`. Not sure if you're on Intel or ARM? On Linux or macOS, run `uname -m`: `x86_64` is Intel/AMD, `aarch64` or `arm64` is ARM.

Each release also has a `SHA256SUMS.txt` if you want to verify your download.

## Use it

**Windows:** unzip it (right-click → Extract All), open a terminal in that folder, then run:

```powershell
.\hotaudio-download.exe "https://hotaudio.net/u/user/track-slug"
```

**Linux, macOS, Termux:** extract and run:

```bash
tar xzf hotaudio-download-v1.0.0-<target>.tar.gz
cd hotaudio-download-v1.0.0-<target>
chmod +x hotaudio-download
./hotaudio-download "https://hotaudio.net/u/user/track-slug"
```

The track is saved in the current folder. To pick the name or location:

```bash
hotaudio-download "https://hotaudio.net/u/user/track-slug" --out track.m4a
```

To see every track on a page:

```bash
hotaudio-download "https://hotaudio.net/u/user/track-slug" --list-tracks
```

To download every track on a page, each to its own file:

```bash
hotaudio-download "https://hotaudio.net/u/user/album" --all-tracks
```

### Run it from anywhere

Put the binary on your `PATH` and you can call `hotaudio-download` from any folder.

```bash
# Linux and macOS
mkdir -p ~/.local/bin
cp hotaudio-download ~/.local/bin/

# Termux
cp hotaudio-download $PREFIX/bin/
```

On Windows, move `hotaudio-download.exe` somewhere permanent and add that folder to `PATH` under System Settings → Environment Variables.

> [!TIP]
> - macOS blocking it? Run `xattr -d com.apple.quarantine hotaudio-download`.
> - Termux missing `tar`? Run `pkg install tar`. Android builds need API 24 or newer.

## Advanced

```bash
# Write straight to disk: low memory, starts playing sooner
hotaudio-download "https://hotaudio.net/u/user/track-slug" --stream-to track.m4a

# Save the track's keys for later
hotaudio-download "https://hotaudio.net/u/user/track-slug" --save-keys keys.json

# Never overwrite an existing file (fails instead)
hotaudio-download "https://hotaudio.net/u/user/track-slug" --out track.m4a --no-clobber

# Name every track on a page by its position
hotaudio-download "https://hotaudio.net/u/user/album" --all-tracks --filename-template '{index} - {title}'

# Through a proxy (HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY are honored too)
hotaudio-download "https://hotaudio.net/u/user/track-slug" --proxy http://127.0.0.1:8080

# Re-download from saved keys (no need to contact the page again)
hotaudio-download --keys keys.json --out track.m4a

# Decrypt a local .hax file offline
hotaudio-download --hax audio.hax --keys keys.json --out track.m4a
```

Keys never change for a given track, so a saved keys file is all you need to download it again.

## If it stops working

This tool depends on how hotaudio.net works today, so a site update can break it. If downloads suddenly fail, check the [releases page](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases) for a newer version, or open an issue.
[coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader) is slower but more stable.

## For developers

Build from source (needs Rust 1.85+):

```bash
git clone https://github.com/dawn-used-yeet-alt/hotaudio-extractor
cd hotaudio-extractor
cargo build --release   # -> target/release/hotaudio-download
```

Everything else lives in the docs:

- [Architecture](docs/ARCHITECTURE.md): how it works and why
- [Protocol](docs/PROTOCOL.md): the wire protocol and signer
- [Browser / wasm](docs/BROWSER.md): running it in a page, and the proxy you'll need
- [Maintenance](docs/MAINTENANCE.md): fixing it after a site change
- [Migration](docs/MIGRATION.md): moving from the TypeScript version
- [Contributing](CONTRIBUTING.md)

## License

MIT. See [LICENSE](LICENSE).
