# Install

Every release is attached to the
[latest release](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest).
Archives carry no version and are named by platform, so the URLs on this page do
not change from release to release.

The quickest way — it picks the build for your machine and installs it to
`~/.local/bin` (`$PREFIX/bin` on Termux):

```bash
curl -fsSL https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/install.sh | sh
```

Prefer to do it by hand? Copy the block for your system.

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

The `-static` builds need no libc, so they run on minimal hosts and inside
`scratch` containers; the glibc builds need glibc 2.17 or newer.

Not sure whether you need Intel/AMD or ARM64? On Linux or macOS, `uname -m`
prints `x86_64` for Intel/AMD and `aarch64` or `arm64` for ARM.

## Checksums

Each release carries `SHA256SUMS.txt`. After downloading, check the file you
have — `--ignore-missing` skips the archives you did not download:

```bash
curl -LO https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/SHA256SUMS.txt
sha256sum --check --ignore-missing SHA256SUMS.txt
```

`install.sh` checks the archive against the same file before installing it.

## Termux (Android)

The `*-linux-android` builds run directly in Termux, on API 24 (Android 7) or
newer, and link only against bionic, so no extra libraries are needed. Pick
`android-arm64` on almost all devices; use `android-x86_64` only on emulators and
Chromebooks. `cannot execute binary file` means the build does not match the
device's architecture.

## Windows

`install.sh` is a POSIX shell script, so on Windows
use the PowerShell block above, or [the zip for your architecture](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest).
SmartScreen may warn about an unsigned binary: More info → Run anyway.

On macOS, if Gatekeeper quarantines the binary:
`xattr -d com.apple.quarantine hotaudio-download`.

## Completions and man page

The `hotaudio-completions.tar.gz` archive holds bash, zsh, fish and PowerShell
completions plus a man page. The binary prints the same files itself with
`--completions <shell>` and `--man-page`, so you never have to download the
archive to get them.

## Build from source

```bash
git clone https://github.com/dawn-used-yeet-alt/hotaudio-extractor
cd hotaudio-extractor
cargo build --release   # -> target/release/hotaudio-download
```

Requires stable Rust (edition 2024, MSRV 1.85).

## Browser / wasm

Running it in a page instead? Each release carries the `wasm-bindgen` package as
`hotaudio-wasm.tar.gz`; see [BROWSER.md](BROWSER.md) for the package layout and
the same-origin proxy a browser needs.
