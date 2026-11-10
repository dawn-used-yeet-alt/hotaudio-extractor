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
hotaudio-download --input urls.txt --remove-downloaded  # consume the list as it goes
```

`--input` is the bulk mode: sources are downloaded one at a time, in file order.
With `--remove-downloaded` (alias `--prune-input`) each line is removed from the
file as soon as that source succeeds, so an interrupted run can be restarted and
only picks up what is left. Failures stay in the file and the run exits non-zero;
blank lines and other lines are preserved. `--list-tracks` never consumes the
list.

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
--cookie 'cf_clearance=...'        # reuse a browser's Cloudflare clearance
--user-agent 'Mozilla/5.0 (...)'   # the exact UA that earned that clearance
--webview                          # open a browser window now and do both for you
--no-webview                       # never open it automatically; use cache only
```

Transfers resume themselves across dropped connections, and a dropped body is
retried from the last byte rather than the beginning. `--delay` spaces listen
and key-paging requests when bulk-downloading.

### Cloudflare challenges

If the site answers with `cf-mitigated: challenge`, the CLI exits 3 and prints a
link to [CLOUDFLARE.md](CLOUDFLARE.md). Passing the challenge in a browser gives
you a `cf_clearance` cookie, but Cloudflare binds it to the network and the
browser's exact User-Agent. Pass **both**:

```bash
hotaudio-download "$URL" \
  --cookie 'cf_clearance=...' \
  --user-agent 'Mozilla/5.0 (...) ...'
```

`HOTAUDIO_COOKIE` and `HOTAUDIO_USER_AGENT` set the same values per run; flags
win over the environment, which wins over the config file.

`--webview` is the shortcut: the bundled `hotaudio-webview` helper opens the
track page in a real window, waits for the challenge (optionally bounded by
`--webview-timeout SECS`, default 0 for no timeout), and returns that browser's cookie,
User-Agent, page state and detected IP family. If the helper or a webview toolkit is unavailable,
the run warns and continues rather than aborting. The extractor never solves or
bypasses a challenge itself; the helper is an interactive browser the user
drives.

Without `--webview`, the downloader probes the track page once and opens the
helper only if the request is actually challenged. The clearance it returns is
cached in `~/.cache/hotaudio/clearance.json` (override with
`HOTAUDIO_CLEARANCE`; written `0600` on Unix) and reused by later runs, so the
window normally appears only when the site stops trusting the old cookie. Pass
`--no-webview` to disable the automatic pass and fail with the challenge
instead; an explicitly supplied `--cookie` or a cached clearance is still used.

Dual-stack networks: Cloudflare binds `cf_clearance` to the IP family (IPv4 or IPv6)
used during the challenge. The webview automatically detects this and configures the
downloader to match. You can also explicitly set `--ip-family` (`any`, `ipv4`, or `ipv6`)
or `HOTAUDIO_IP_FAMILY`. When unset, challenged requests on dual-stack automatically try
the alternate IP family before falling back.

Separately, a download that hits the site's per-tick key-window limit
re-handshakes automatically: it re-reads the page for a fresh tick and resumes
at the segment that failed, keeping every key it already decrypted. That applies
even without `--webview`; if Cloudflare challenges the refresh as well, the
helper renews the clearance (unless `--no-webview` is set).

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
cookie = "cf_clearance=..."
user_agent = "Mozilla/5.0 (...) ..."
```

The cookie and user agent are a pair; see [CLOUDFLARE.md](CLOUDFLARE.md).

## Exit codes

For scripts. `--json` prints a machine-readable result line.

| Code | Meaning |
| --- | --- |
| 0 | success |
| 2 | bad input (unknown flag, conflicting options, bad config) |
| 3 | network, transport or Cloudflare-challenge error |
| 4 | protocol error (handshake, decryption, signer) |
| 5 | local filesystem error |
| 130 | cancelled with Ctrl-C |
