# CLI

The `hotaudio-download` binary downloads and decrypts tracks. Run from source with Bun or from `dist/` after building.

```bash
bun run build
node ./dist/cli.js <URL | HTML file> --out track.m4a
```

## Online mode

```bash
# Download + decrypt
hotaudio-download https://hotaudio.net/u/user/track-slug --out track.m4a

# Same pipeline from a saved track page (no page fetch)
hotaudio-download page.html --out track.m4a

# Also persist branch keys for later offline use
hotaudio-download https://hotaudio.net/u/user/track-slug --save-keys keys.json --out track.m4a
```

Flags:

| Flag | Description |
| --- | --- |
| `--out <path>` | Output `.m4a` path. Defaults to the sanitized track title. |
| `--save-keys <path>` | Write merged hex branch keys as JSON. |

Progress (`resolving` / `fetching` / `decrypting`) goes to stderr. The saved file path goes to stdout.

## Offline mode

```bash
hotaudio-download page.html --hax audio.hax --keys keys.json --out track.m4a
```

| Flag | Description |
| --- | --- |
| `--hax <path>` | Local `.hax` container. |
| `--keys <path \| JSON>` | Saved keys file or inline JSON object. |

Offline mode performs no network I/O. The HTML file is only used to
confirm the page state exists; keys must already cover every segment
(merge several `first:<n>` responses for long tracks, or reuse a
`--save-keys` file from a completed download).

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Usage error, missing file, undecryptable page state, network/API failure, or incomplete keys. |

## Notes

- Filenames are sanitized (`\ / * ? : " < > |` removed).
- The default bare `Mozilla/5.0` user agent is required; Chrome user agents get Cloudflare 403s.
