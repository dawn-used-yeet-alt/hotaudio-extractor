# CLI

The `hotaudio-download` binary downloads and decrypts tracks. Run from source with Bun or from `dist/` after building.

```bash
bun run build
node ./dist/cli.js <URL> --out track.m4a
```

## Modes

Online (full pipeline):

```bash
hotaudio-download https://hotaudio.net/u/user/track-slug --out track.m4a
hotaudio-download page.html --out track.m4a
hotaudio-download https://hotaudio.net/u/user/track-slug --save-keys keys.json --out track.m4a
```

Cached (saved keys, no page fetch, no listen calls):

```bash
hotaudio-download --keys keys.json --out track.m4a
hotaudio-download --keys keys.json --hax-url https://cdn.hotaudio.net/a/x.hax --out track.m4a
```

Offline (local `.hax`, no network):

```bash
hotaudio-download --hax audio.hax --keys keys.json --out track.m4a
```

## Flags

| Flag | Description |
| --- | --- |
| `--out <path>` | Output `.m4a` path. Defaults to the sanitized track title. |
| `--save-keys <path>` | Write a saved-keys envelope (keys + `.hax` URL + metadata) for later cached/offline use. |
| `--keys <path \| JSON>` | Saved-keys envelope or bare `{ index: hex }` map (file or inline JSON). |
| `--hax <path>` | Local `.hax` container (offline mode). |
| `--hax-url <URL>` | `.hax` URL override for cached mode (defaults to the envelope's URL). |
| `--track <id>` | Track id for multi-track pages (defaults to the page's primary track). |
| `--api-base <URL>` | Listen API base (defaults to `https://hotaudio.net`). |
| `--help`, `-h` | Show usage. |

Branch keys are deterministic per track and `.hax` URLs are stable, so a
saved envelope re-downloads with a single CDN fetch: no page fetch, no
listen requests. If the track's key material ever rotates, decrypt fails
closed (ChaCha auth error) — just re-run online mode for fresh keys.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Usage error, missing file, undecryptable page state, network/API failure, or incomplete keys. |

## Notes

- Filenames are sanitized (`\ / * ? : " < > |` removed).
- Progress goes to stderr; the saved file path goes to stdout.
- The default bare `Mozilla/5.0` user agent is required; Chrome user agents get Cloudflare 403s.
