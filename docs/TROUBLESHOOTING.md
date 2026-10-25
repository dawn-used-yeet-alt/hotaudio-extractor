# Troubleshooting

## It suddenly stopped working

The extractor tracks how hotaudio.net works today, so a site update can break it.
Check the [releases page](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases)
for a newer version, or open an issue with what you saw.

[coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader) is the
slower-but-stable alternative: it does not track the site's internals.

## It says “site challenge” / “Just a moment…”

hotaudio.net is behind Cloudflare, and Cloudflare has decided this network
looks automated. The extractor cannot answer an interactive challenge, but you
can pass it in a browser and reuse the resulting `cf_clearance` cookie. The
cookie only works with the browser's exact User-Agent on the same network.

The two-minute guide, with per-browser steps for copying both values, is in
[CLOUDFLARE.md](CLOUDFLARE.md).

## A download fails or the audio looks truncated

Transfers resume themselves after dropped connections, so many failures are
temporary. Give the requests more room on a flaky link:

```bash
hotaudio-download "$URL" --retries 5 --timeout 60
```

Downloads to `--stream-to` land at a sibling `.part` file and are renamed into
place only after the whole download succeeds, so an interrupted run leaves no
half-written `.m4a` behind. The manual construction is shown in
[CLI.md](CLI.md#streaming-output), and leftover `.part` files are safe to delete.

To be certain the decrypted bytes match the container:

```bash
hotaudio-download "$URL" --verify
```

`--verify` walks the MP4 box tree and fails loudly when the output's duration
does not match the container.

## The binary will not run

The fixes for architecture mismatches, Gatekeeper, SmartScreen, `PATH` and
Termux live in [INSTALL.md](INSTALL.md#after-unpacking), next to the download
instructions they belong with.

## Reading the output

Failures separate into these classes by exit code:

- `3` transport or Cloudflare-challenge error
- `4` protocol error

plus `2` for bad input, `5` for the local filesystem, and `130` for Ctrl-C.
The full table is in [CLI.md](CLI.md#exit-codes). `--json` prints a
machine-readable result, and `--quiet` / `--verbose` change how much is
printed. A 3 or 4 after a working install is a site-side change; see the
section above.

## Debugging deeper

`--verbose` logs request-level detail. If you can read the protocol, the
failure-symptom table in [MAINTENANCE.md](MAINTENANCE.md#diagnosis-first) maps
each symptom to the component that owns it: most failures name their own fix.
