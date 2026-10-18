# Command line

```
hotaudio-download [OPTIONS] [URL|HTML]...
```

`--help` lists every flag. This page is the same ground at walking pace, with
the reasoning that has no room in a help screen.

## Sources

A page URL is the usual input. Several can go in one run, or a list from a file:

```bash
hotaudio-download "$URL" "$OTHER_URL"
hotaudio-download --input urls.txt        # one URL per line; - reads stdin
```

Pages can carry several tracks. `--track ID` picks one (`--list-tracks` prints
the ids), `--all-tracks` downloads every track on the page, each to its own
file. A single-track page is unaffected by either flag.

## Output

The track lands in the working directory, named from its title. To say
otherwise:

```bash
--out track.m4a                 # exact path (single source only)
--output-dir ~/Music            # directory to write title-derived names into
--filename-template '{index} - {title}'   # {title}, {index}, {id}
--no-clobber                    # fail instead of overwriting
```

### Streaming output

`--stream-to <file>` writes to disk while decrypting: lower memory, and the
first playable fragment is on disk after one round trip. The output goes to a
sibling `<file>.part` and is renamed into place only once the whole download
has succeeded, so an interrupted run never leaves a half-written `.m4a`. A
leftover `.part` is safe to delete, and a re-run starts clean.

`--out -` writes the track to stdout for piping into another tool. When stdout
carries audio, the human-readable result (and `--json`) moves to stderr.

## Keys, and working offline

Keys never change for a given track, so a saved keys file is all a re-download
needs.

```bash
hotaudio-download "$URL" --save-keys keys.json    # while downloading
hotaudio-download --keys keys.json --out track.m4a  # later: no page fetch, no listen calls
```

`--save-keys` writes a small JSON envelope (`version`, `haxUrl`, `title`,
`savedAt`, `keys`); `--keys` accepts that envelope or a bare `{"node": "hex"}`
map from an older tool. `--keys-only` saves keys without downloading audio.

`--save-hax` archives the raw container too, which makes a later decrypt
completely offline:

```bash
hotaudio-download "$URL" --save-hax audio.hax --save-keys keys.json --out track.m4a
hotaudio-download --hax audio.hax --keys keys.json --out again.m4a
```

The same saved-keys envelope works with the library and the browser build; see
[LIBRARY.md](LIBRARY.md#offline-decryption) and [BROWSER.md](BROWSER.md#replaying-a-saved-track-offline).

## Checking the output

`--verify` walks the MP4 box tree and checks the audio duration against the
container's, failing loudly on a mismatch. With `--stream-to`, the check runs
on the `.part` file before the rename, so a bad decrypt never reaches the
destination.

## Network

```bash
--proxy http://127.0.0.1:8080      # HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY are honoured too
--retries 5 --timeout 60           # more retries, longer per-attempt timeout
--delay 500                        # ms between requests; politeness, not correctness
--api-base https://mirror.example  # talk to a mirror, or override the listen API
```

Transfers resume themselves across dropped connections, and a dropped body is
retried from the last byte rather than the beginning. `--delay` spaces listen
and key-paging requests when bulk-downloading.

The flags are native-transport concerns. The wasm build has no flag of this
kind: a page cannot choose its proxy, so proxying there belongs to the host.
See [BROWSER.md](BROWSER.md#a-proxy-is-required-and-this-crate-does-not-ship-one).
The retry policy behind `--retries` and `--timeout` is described in
[ARCHITECTURE.md](ARCHITECTURE.md#reliability).

## Configuration file

Defaults can live in `~/.config/hotaudio/config.toml`, a path given with
`--config`, or `$HOTAUDIO_CONFIG`. Command-line flags still win.

```toml
output_dir = "/home/you/Music"
retries = 5
timeout = 60
```

## Exit codes

For scripts. `--json` prints a machine-readable result line.

| Code | Meaning |
| --- | --- |
| 0 | success |
| 2 | bad input (unknown flag, conflicting options, bad config) |
| 3 | network or transport error |
| 4 | protocol error (handshake, decryption, signer) |
| 5 | local filesystem error |
| 130 | cancelled with Ctrl-C |
