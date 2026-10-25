# hotaudio-extractor

Save audio from [hotaudio.net](https://hotaudio.net) as a playable `.m4a`. Paste a track link, get a file.

> [!CAUTION]
> Made for personal archiving. It hosts nothing and isn't affiliated with hotaudio.net.
> Only download what you have the right to, and follow the site's terms and your local law.

## Install

Linux, macOS and Termux:

```bash
curl -fsSL https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/install.sh | sh
```

It picks the build for your platform and installs `hotaudio-download` to
`~/.local/bin` (`$PREFIX/bin` on Termux).

Windows, in PowerShell (x86-64 shown; use `windows-arm64` on ARM):

```powershell
Invoke-WebRequest https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/hotaudio-download-windows-x86_64.zip -OutFile hotaudio-download-windows-x86_64.zip
Expand-Archive hotaudio-download-windows-x86_64.zip
.\hotaudio-download-windows-x86_64\hotaudio-download.exe "https://hotaudio.net/u/user/track-slug"
```

SmartScreen may warn about an unsigned binary: More info → Run anyway. Every
other platform, checksums and building from source are in
[docs/INSTALL.md](docs/INSTALL.md).

## Use it

```bash
hotaudio-download "https://hotaudio.net/u/user/track-slug"                  # saves the track here
hotaudio-download "$URL" --out ~/Music/track.m4a                           # pick the name and place
hotaudio-download "$URL" --list-tracks                                     # what is on this page?
hotaudio-download "$URL" --all-tracks --output-dir ~/Music                 # every track, to files
```

If `hotaudio-download` is not on your `PATH`, run it from where it was installed.

Streaming, keys, offline decryption, proxies, retries, templated file names and a
config file all exist; [docs/CLI.md](docs/CLI.md) covers them, and
`hotaudio-download --help` lists every flag.

## Docs

Docs live in [`docs/`](docs/README.md), split by reader:

- Install and use: [Install](docs/INSTALL.md), [Command line](docs/CLI.md),
  [Troubleshooting](docs/TROUBLESHOOTING.md),
  [Cloudflare challenges](docs/CLOUDFLARE.md)
- Build on the crate: [Library (Rust)](docs/LIBRARY.md),
  [Bindings (Kotlin, Swift, Python, Ruby)](docs/BINDINGS.md),
  [Browser / wasm](docs/BROWSER.md)
- Keep it working: [Architecture](docs/ARCHITECTURE.md),
  [Protocol](docs/PROTOCOL.md), [Maintenance](docs/MAINTENANCE.md)
- Historical: [Migration from TypeScript](docs/MIGRATION.md)
- [Contributing](CONTRIBUTING.md), and the full
  [documentation index](docs/README.md)

## Plug
If your goal is to just stream audios from GWA(or adjacant subreddits) check out [Dawngasm](https://github.com/dawn-used-yeet-alt/Dawngasm).
It's a TUI app to do just that. It supports Whyp, Hotaudio, Audiochan and Soundgasm.  
It runs on my database off all the posts ever posted on these subreddits.
It lags behind ~1 day though.

## License

MIT. See [LICENSE](LICENSE).

Release archives also include the notices for the permissively licensed
third-party crates they statically link.
