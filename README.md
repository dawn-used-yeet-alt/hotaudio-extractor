# hotaudio-extractor

Save audio from [hotaudio.net](https://hotaudio.net) as a playable `.m4a`. Paste a track link, get a file.

> [!NOTE]
> Made for personal archiving. It hosts nothing and isn't affiliated with hotaudio.net.
> Only download what you have the right to, and follow the site's terms and your local law.

## Install

Grab your platform's archive from the [latest release](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest), or copy a command below. The file name says the platform, so there is nothing to look up.

<!-- install:start -->

**Linux, Intel/AMD**

```bash
curl -LO https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-linux-x86_64.tar.gz
tar xzf hotaudio-download-linux-x86_64.tar.gz
./hotaudio-download-linux-x86_64/hotaudio-download "https://hotaudio.net/u/user/track-slug"
```

**Linux, ARM64**

```bash
curl -LO https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-linux-arm64.tar.gz
tar xzf hotaudio-download-linux-arm64.tar.gz
./hotaudio-download-linux-arm64/hotaudio-download "https://hotaudio.net/u/user/track-slug"
```

**macOS, Intel**

```bash
curl -LO https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-macos-x86_64.tar.gz
tar xzf hotaudio-download-macos-x86_64.tar.gz
./hotaudio-download-macos-x86_64/hotaudio-download "https://hotaudio.net/u/user/track-slug"
```

**macOS, Apple Silicon**

```bash
curl -LO https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-macos-arm64.tar.gz
tar xzf hotaudio-download-macos-arm64.tar.gz
./hotaudio-download-macos-arm64/hotaudio-download "https://hotaudio.net/u/user/track-slug"
```

**Windows, Intel/AMD**

```powershell
Invoke-WebRequest https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-windows-x86_64.zip -OutFile hotaudio-download-windows-x86_64.zip
Expand-Archive hotaudio-download-windows-x86_64.zip
.\hotaudio-download-windows-x86_64\hotaudio-download.exe "https://hotaudio.net/u/user/track-slug"
```

**Android/Termux, most phones**

```bash
curl -LO https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-android-arm64.tar.gz
tar xzf hotaudio-download-android-arm64.tar.gz
./hotaudio-download-android-arm64/hotaudio-download "https://hotaudio.net/u/user/track-slug"
```

| Your system | Archive |
| --- | --- |
| Linux, Intel/AMD | [`hotaudio-download-linux-x86_64.tar.gz`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-linux-x86_64.tar.gz) |
| Linux, ARM64 | [`hotaudio-download-linux-arm64.tar.gz`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-linux-arm64.tar.gz) |
| Linux, static, Intel/AMD | [`hotaudio-download-linux-x86_64-static.tar.gz`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-linux-x86_64-static.tar.gz) |
| Linux, static, ARM64 | [`hotaudio-download-linux-arm64-static.tar.gz`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-linux-arm64-static.tar.gz) |
| macOS, Intel | [`hotaudio-download-macos-x86_64.tar.gz`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-macos-x86_64.tar.gz) |
| macOS, Apple Silicon | [`hotaudio-download-macos-arm64.tar.gz`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-macos-arm64.tar.gz) |
| Windows, Intel/AMD | [`hotaudio-download-windows-x86_64.zip`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-windows-x86_64.zip) |
| Windows, ARM64 | [`hotaudio-download-windows-arm64.zip`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-windows-arm64.zip) |
| Windows, MinGW | [`hotaudio-download-windows-x86_64-gnu.zip`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-windows-x86_64-gnu.zip) |
| Android/Termux, most phones | [`hotaudio-download-android-arm64.tar.gz`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-android-arm64.tar.gz) |
| Android/Termux, emulators | [`hotaudio-download-android-x86_64.tar.gz`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-android-x86_64.tar.gz) |
| Browser / Node (wasm) | [`hotaudio-wasm.tar.gz`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-wasm.tar.gz) |
| Shell completions + man page | [`hotaudio-completions.tar.gz`](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-completions.tar.gz) |

<!-- install:end -->

Each release also carries `SHA256SUMS.txt`; after downloading, check the file you have:

```bash
curl -LO https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/SHA256SUMS.txt
sha256sum --check --ignore-missing SHA256SUMS.txt
```

Not sure if you need Intel/AMD or ARM64? On Linux or macOS, `uname -m` prints `x86_64` for Intel/AMD and `aarch64` or `arm64` for ARM. The `-static` builds need no glibc, so they are the ones for containers and minimal systems.

Running it in a browser instead? Each release also carries the wasm package (`hotaudio-wasm.tar.gz`); see [docs/BROWSER.md](docs/BROWSER.md).

## Use it

Run it with a track link (this is the last line of your install command above):

```bash
hotaudio-download "https://hotaudio.net/u/user/track-slug"
```

If it is not on your `PATH` yet, prefix it with the folder it was extracted to — `./hotaudio-download-linux-x86_64/hotaudio-download` on Linux and macOS, or `.\hotaudio-download-windows-x86_64\hotaudio-download.exe` on Windows.

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

# Pick one track from a multi-track page (see --list-tracks for the ids)
hotaudio-download "https://hotaudio.net/u/user/album" --track 2 --out track-2.m4a

# Talk to a mirror, or override the listen API base
hotaudio-download "https://mirror.example/u/user/track-slug" --api-base https://mirror.example

# Through a proxy (HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY are honored too)
hotaudio-download "https://hotaudio.net/u/user/track-slug" --proxy http://127.0.0.1:8080

# Retry harder and give each API call a longer per-attempt timeout
hotaudio-download "https://hotaudio.net/u/user/track-slug" --retries 5 --timeout 60

# Space listen/key-paging requests when bulk-downloading (politeness, not correctness)
hotaudio-download "https://hotaudio.net/u/user/album" --all-tracks --delay 500

# Re-download from saved keys (no need to contact the page again)
hotaudio-download --keys keys.json --out track.m4a

# Decrypt a local .hax file offline
hotaudio-download --hax audio.hax --keys keys.json --out track.m4a

# Send title-derived outputs to a directory
hotaudio-download "https://hotaudio.net/u/user/album" --all-tracks --output-dir ~/Music
```

Defaults can live in `~/.config/hotaudio/config.toml` (or a path given with `--config`); command-line flags still win:

```toml
output_dir = "/home/you/Music"
retries = 5
timeout = 60
```

Keys never change for a given track, so a saved keys file is all you need to download it again. `--save-keys` writes a small JSON envelope (`version`, `haxUrl`, `title`, `savedAt`, `keys`); `--keys` accepts that envelope or a bare `{ "node": "hex" }` map from an older tool. Pair it with `--save-hax` to archive the raw container and decrypt it later without touching the network at all:

```bash
# Archive the container and its keys once
hotaudio-download "$URL" --save-hax audio.hax --save-keys keys.json --out track.m4a

# Decrypt the archive offline, as many times as you like
hotaudio-download --hax audio.hax --keys keys.json --out again.m4a
```

For scripts, the exit code says what went wrong: `0` success, `2` bad input, `3` network, `4` protocol or decryption, `5` local file error, `130` cancelled (Ctrl-C).

## If it stops working

This tool depends on how hotaudio.net works today, so a site update can break it. If downloads suddenly fail, check the [releases page](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases) for a newer version, or open an issue.
[coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader) is slower but more stable.

### A download failed or the audio looks truncated

Transfers resume themselves across dropped connections. With `--stream-to <file>`, the output goes to a sibling `<file>.part` and is renamed into place only once the whole download has succeeded, so a failed or interrupted run never leaves a half-written `.m4a`; a leftover `.part` is safe to delete, and a re-run starts clean. If the network is flaky, give the request more room:

```bash
hotaudio-download "$URL" --retries 5 --timeout 60
```

To be sure the decrypted bytes are what the container says they are, `--verify`
walks the MP4 box tree and checks the audio's duration against the container's,
failing loudly on a mismatch. With `--stream-to`, the check runs on the `.part`
file before it is renamed into place, so a bad decrypt never lands at the
destination:

```bash
hotaudio-download "$URL" --verify
```

## For developers

Build from source (needs Rust 1.85+):

```bash
git clone https://github.com/dawn-used-yeet-alt/hotaudio-extractor
cd hotaudio-extractor
cargo build --release   # -> target/release/hotaudio-download
```

Runnable examples show the library API; each takes a page URL (or local files) as arguments:

```bash
cargo run --example download_track  -- "https://hotaudio.net/u/user/track-slug" track.m4a
cargo run --example stream_track    -- "https://hotaudio.net/u/user/track-slug" track.m4a
cargo run --example decrypt_offline -- audio.hax keys.json track.m4a
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

Release archives also include the notices for the permissively licensed
third-party crates they statically link.
