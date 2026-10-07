A standalone [hotaudio.net](https://hotaudio.net) audio extractor: it performs the
track-page handshake, request signing, X25519 listen key exchange and HAX0
container decryption, and writes a playable `.m4a`.

## Install

Download the archive for your platform, extract it, and run `hotaudio-download`:

| Platform | Architecture | Archive |
| --- | --- | --- |
| Linux | x86-64 | `hotaudio-download-<tag>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux | ARM64 | `hotaudio-download-<tag>-aarch64-unknown-linux-gnu.tar.gz` |
| macOS | Intel | `hotaudio-download-<tag>-x86_64-apple-darwin.tar.gz` |
| macOS | Apple Silicon | `hotaudio-download-<tag>-aarch64-apple-darwin.tar.gz` |
| Windows | x86-64 | `hotaudio-download-<tag>-x86_64-pc-windows-msvc.zip` |
| Windows | ARM64 | `hotaudio-download-<tag>-aarch64-pc-windows-msvc.zip` |

On Unix, `chmod +x hotaudio-download` after extraction.

Verify a download against the attached `SHA256SUMS.txt`:

```bash
sha256sum --check SHA256SUMS.txt
```

Building from source instead:

```bash
git clone https://github.com/dawn-used-yeet-alt/hotaudio-extractor
cd hotaudio-extractor
cargo build --release   # target/release/hotaudio-download
```

Requires stable Rust (edition 2024, MSRV 1.85).

## Usage

```bash
# Download + decrypt a track
hotaudio-download https://hotaudio.net/u/user/track-slug --out track.m4a

# Stream: low memory, first playable fragment after one round trip
hotaudio-download https://hotaudio.net/u/user/track-slug --stream-to track.m4a

# Save branch keys for later offline use
hotaudio-download https://hotaudio.net/u/user/track-slug --save-keys keys.json

# Cached re-download: no page fetch, no listen calls
hotaudio-download --keys keys.json --out track.m4a

# Fully offline decrypt of a local .hax
hotaudio-download --hax audio.hax --keys keys.json --out track.m4a

# List tracks on a page
hotaudio-download https://hotaudio.net/u/user/track-slug --list-tracks
```

Run `hotaudio-download --help` for every flag.

> **Please respect creators.** This is an interoperability and personal-archiving
> tool. Download only content you have the right to access, and comply with
> hotaudio.net's terms of service and applicable law.

See the [full README](https://github.com/dawn-used-yeet-alt/hotaudio-extractor)
for the library API and design notes. MIT licensed.
