# hotaudio-extractor

Save audio from [hotaudio.net](https://hotaudio.net) as a playable `.m4a`. Paste a track link, get a file.

> [!NOTE]
> Made for personal archiving. It hosts nothing and isn't affiliated with hotaudio.net.
> Only download what you have the right to, and follow the site's terms and your local law.

## Install

```bash
curl -fsSL https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/install.sh | sh
```

It picks the build for your platform and installs `hotaudio-download` to
`~/.local/bin` (`$PREFIX/bin` on Termux). Windows, manual downloads and
checksums are in [docs/INSTALL.md](docs/INSTALL.md).

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

- [Install](docs/INSTALL.md) — every platform, checksums, from source
- [Command line](docs/CLI.md) — sources, output, keys, network, exit codes
- [Troubleshooting](docs/TROUBLESHOOTING.md) — when it stops working
- [Architecture](docs/ARCHITECTURE.md) — how it works and why
- [Protocol](docs/PROTOCOL.md) — the wire protocol and signer
- [Browser / wasm](docs/BROWSER.md) — running it in a page
- [Maintenance](docs/MAINTENANCE.md) — fixing it after a site change
- [Migration](docs/MIGRATION.md) — moving from the TypeScript version
- [Contributing](CONTRIBUTING.md)

## License

MIT. See [LICENSE](LICENSE).

Release archives also include the notices for the permissively licensed
third-party crates they statically link.
