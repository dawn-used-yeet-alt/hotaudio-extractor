# Command line

```
hotaudio-download [OPTIONS] [URL|HTML]...
```

`hotaudio-download --help` lists every flag; this page is the tour, with the
reasoning that `--help` has no room for.

## Sources

A page URL is the usual input. Several can be given in one run, or read from a
file:

```bash
hotaudio-download "$URL" "$OTHER_URL"
hotaudio-download --input urls.txt        # one URL per line; - reads stdin
```

`--all-tracks` downloads every track on a page, each to its own file;
`--track ID` picks one (see `--list-tracks` for the ids). A page with one track
is unaffected by both.

## Output

By default the track lands in the current directory, named from its title. To
say otherwise:

```bash
--out track.m4a                 # exact path (single source only)
--output-dir ~/Music            # directory to write title-derived names into
--filename-template '{index} - {title}'   # {title}, {index}, {id}
--no-clobber                    # fail instead of overwriting
```

`--stream-to <file>` writes straight to disk as it decrypts: lower memory, and
the first playable fragment is on disk after one round trip. The output goes to a
sibling `<file>.part` and is renamed into place only once the whole download has
succeeded, so an interrupted run never leaves a half-written `.m4a`. A leftover
`.part` is safe to delete.

`--out -` writes the track to stdout, for piping into another tool. When stdout
carries audio, the human-readable result (and `--json`) moves to stderr.

## Keys, and working offline

Keys never change for a given track, so a saved keys file is all you need to
download it again.

```bash
hotaudio-download "$URL" --save-keys keys.json    # while downloading
hotaudio-download --keys keys.json --out track.m4a  # later: no page fetch, no listen calls
```

`--save-keys` writes a small JSON envelope (`version`, `haxUrl`, `title`,
`savedAt`, `keys`); `--keys` accepts that envelope or a bare
`{ "node": "hex" }` map from an older tool. `--keys-only` saves just the keys,
without downloading the track.

`--save-hax` also archives the raw container, which makes a later decrypt
completely offline:

```bash
hotaudio-download "$URL" --save-hax audio.hax --save-keys keys.json --out track.m4a
hotaudio-download --hax audio.hax --keys keys.json --out again.m4a
```

## Checking the output

`--verify` walks the MP4 box tree and checks the audio duration against the
container's, failing loudly on a mismatch. With `--stream-to`, the check runs on
the `.part` file before it is renamed into place, so a bad decrypt never lands at
the destination.

## Network

```bash
--proxy http://127.0.0.1:8080      # HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY are honoured too
--retries 5 --timeout 60           # more retries, longer per-attempt timeout
--delay 500                        # ms between requests; politeness, not correctness
--api-base https://mirror.example  # talk to a mirror, or override the listen API
```

Transfers resume themselves across dropped connections. `--delay` spaces listen
and key-paging requests when bulk-downloading; key paging is sequential on
purpose, because the server serialises per track and concurrency would add load
without reducing wall time.

## Configuration file

Defaults can live in `~/.config/hotaudio/config.toml` (or a path given with
`--config`, or `$HOTAUDIO_CONFIG`). Command-line flags still win.

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
