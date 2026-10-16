# Troubleshooting

## It suddenly stopped working

This tool depends on how hotaudio.net works today, so a site update can break it.
If downloads fail where they used to work, check the
[releases page](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases)
for a newer version, or open an issue.

[coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader) is
slower but more stable, because it does not track the site's internals.

## A download failed, or the audio looks truncated

Transfers resume themselves across dropped connections, so most failures are
temporary. With `--stream-to <file>` the output goes to a sibling `<file>.part`
and is renamed into place only once the whole download has succeeded, so a failed
or interrupted run never leaves a half-written `.m4a`. A leftover `.part` is safe
to delete, and a re-run starts clean.

If the network is flaky, give the request more room:

```bash
hotaudio-download "$URL" --retries 5 --timeout 60
```

To be sure the decrypted bytes are what the container says they are, `--verify`
walks the MP4 box tree and checks the audio's duration against the container's,
failing loudly on a mismatch:

```bash
hotaudio-download "$URL" --verify
```

## The binary will not run

- **`cannot execute binary file`** (Termux or Linux): the build does not match
  your architecture. `uname -m` prints `x86_64` for Intel/AMD and `aarch64` for
  ARM; re-download the matching archive from
  [the latest release](https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest).
- **macOS "cannot be opened because the developer cannot be verified"**:
  `xattr -d com.apple.quarantine hotaudio-download`.
- **Windows SmartScreen**: More info → Run anyway.
- **`hotaudio-download: command not found`** after using `install.sh`: the
  install directory is not on your `PATH`. The script prints the line to add.
- **Termux is missing `tar`**: `pkg install tar`. Android builds need API 24 or
  newer.

## Reading the output

Failures are distinguished by exit code (`2` bad input, `3` transport, `4`
protocol, `5` filesystem, `130` cancelled); the full table is in
[CLI.md](CLI.md#exit-codes). `--json` prints a machine-readable result, and
`--quiet` / `--verbose` change how much is printed.
