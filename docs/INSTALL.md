# Install

Every release is attached to the
[latest release](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest).
Archives carry no version in their names, so the URLs on this page work from
release to release.

The quickest way is the install script. It picks the build for your machine,
checks it against the release's checksum file, and installs the binary to
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

The block above (and `hotaudio-completions.tar.gz`) is generated from
`scripts/release-targets.tsv`. The `-static` builds need no libc: they run on
minimal hosts and in `scratch` containers, while the glibc builds need glibc
2.17 or newer.

## After unpacking

If the binary will not run, it is almost always one of these:

- **`cannot execute binary file`** on Linux or Termux: the build does not match
  your architecture. `uname -m` prints `x86_64` on Intel/AMD and `aarch64` (or
  `arm64`) on ARM; re-download the matching archive.
- **macOS: "cannot be opened because the developer cannot be verified"**. The
  binary is unsigned, so Gatekeeper quarantines it:
  `xattr -d com.apple.quarantine hotaudio-download`.
- **Windows SmartScreen**. More info → Run anyway.
- **`command not found`** after using `install.sh`. The install directory is
  not on your `PATH`; the script prints the exact line to add.
- **Termux: `tar: not found`**. `pkg install tar`. The Android builds need API
  24 (Android 7) or newer and link only against bionic, so no extra libraries
  are needed. On almost all devices the right build is `android-arm64`;
  `android-x86_64` is for emulators and Chromebooks.

## Checksums

Each release carries `SHA256SUMS.txt`. Check what you downloaded against it;
`--ignore-missing` skips the archives you did not download:

```bash
curl -LO https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/SHA256SUMS.txt
sha256sum --check --ignore-missing SHA256SUMS.txt
```

`install.sh` does this check before installing.

## Completions and man page

The `hotaudio-completions.tar.gz` archive holds bash, zsh, fish and PowerShell
completions plus a man page. The binary prints the same files itself with
`--completions <shell>` and `--man-page`, so you need not download the archive
to get them.

## Build from source

```bash
git clone https://github.com/dawn-used-yeet-alt/hotaudio-extractor
cd hotaudio-extractor
cargo build --release   # -> target/release/hotaudio-download
```

Requires stable Rust (edition 2024, MSRV 1.87).

## Next steps

- [CLI.md](CLI.md) is the tour of the flags.
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) covers download failures.
- Building against the crate from Rust is [LIBRARY.md](LIBRARY.md); building
  the wasm package for a page is [BROWSER.md](BROWSER.md).
