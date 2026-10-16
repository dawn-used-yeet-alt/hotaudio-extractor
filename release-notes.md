A standalone [hotaudio.net](https://hotaudio.net) audio extractor: it performs the
track-page handshake, request signing, X25519 listen key exchange and HAX0
container decryption, and writes a playable `.m4a`.

## Install

Linux, macOS and Termux, one line:

```bash
curl -fsSL https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/install.sh | sh
```

Windows, every other platform, checksums and building from source are in
[docs/INSTALL.md](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/blob/main/docs/INSTALL.md).

## Use

```bash
hotaudio-download "https://hotaudio.net/u/user/track-slug" --out track.m4a
```

`--stream-to`, `--all-tracks`, `--keys`/`--hax` for offline decryption, proxies,
a config file and per-platform exit codes are in
[docs/CLI.md](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/blob/main/docs/CLI.md);
`hotaudio-download --help` lists every flag. The browser build is attached as
`hotaudio-wasm.tar.gz`
(see [docs/BROWSER.md](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/blob/main/docs/BROWSER.md)).

> **Please respect creators.** This is an interoperability and personal-archiving
> tool. Download only content you have the right to access, and comply with
> hotaudio.net's terms of service and applicable law.

MIT licensed. The [README](https://github.com/dawn-used-yeet-alt/hotaudio-extractor)
has the full tour.
