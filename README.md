# hotaudio-extractor

Standalone [hotaudio.net](https://hotaudio.net) audio extractor with no application dependencies. It performs the track page handshake, request signing, X25519 listen key exchange, and HAX0 container decryption. Works in browsers, Node.js, Bun, and workers.

- Full-track download to playable `.m4a` bytes (any runtime with `fetch`)
- Progressive browser playback via Media Source Extensions (MSE)
- Offline decryption of saved `.hax` containers with saved keys
- CLI plus a small typed library API
- Runtime dependencies: `@noble/ciphers`, `@noble/curves` only
- Fast and lightweight, but unstable: it tracks hotaudio.net internals and can break when the site changes. If you need slow and stable, use [coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader) instead.

> Note: this project is for interoperability and personal archiving. Download only content you have the right to access, and comply with hotaudio.net's terms of service and applicable law.

## Install

```bash
bun add hotaudio-extractor
# or
npm install hotaudio-extractor
```

Requires Node `>= 18` or Bun `>= 1.0`.

## Quick start

```ts
import { downloadHotaudioBuffer } from 'hotaudio-extractor';

const { buffer, title, duration } = await downloadHotaudioBuffer(
  'https://hotaudio.net/u/user/track-slug',
  {
    onProgress: ({ phase, loaded, total }) => console.log(phase, loaded, total),
  },
);

// Node / Bun
await Bun.write('track.m4a', buffer);
// or: await import('node:fs/promises').then((fs) => fs.writeFile('track.m4a', buffer));
```

Browser progressive playback:

```ts
import { extractHotaudio } from 'hotaudio-extractor';

const result = await extractHotaudio(pageUrl);
if (result) {
  audio.src = result.url;
  await audio.play();
}
```

Offline decrypt with previously saved keys:

```ts
import { decryptHaxBuffer } from 'hotaudio-extractor';

const { buffer } = await decryptHaxBuffer(haxBytes, savedKeys);
```

## CLI

Build first (`bun run build`), or run from source with Bun:

```bash
# Download + decrypt a track
bun ./src/cli.ts https://hotaudio.net/u/user/track-slug --out track.m4a

# Same pipeline from a saved track page (skips the page fetch)
bun ./src/cli.ts page.html --out track.m4a

# Save branch keys for later offline use
bun ./src/cli.ts https://hotaudio.net/u/user/track-slug --save-keys keys.json --out track.m4a

# Offline decrypt (no network)
bun ./src/cli.ts page.html --hax audio.hax --keys keys.json --out track.m4a
```

After `bun run build`, the `hotaudio-download` binary is available in `dist/cli.js`.

See [docs/CLI.md](docs/CLI.md) for all flags and exit codes.

## Library API

| Export | Description |
| --- | --- |
| `downloadHotaudioBuffer(pageUrl, opts?)` | Full pipeline to `{ buffer, title, duration, mime, haxUrl, keys, segmentCount }` |
| `downloadWithHandshake(handshake, opts?)` | Full pipeline from `loadHandshakeFromHtml` (local HTML, tests) |
| `downloadHotaudioToFile(pageUrl, outPath, opts?)` | Node-only convenience writer |
| `fetchHotaudioKeys(pageUrl, opts?)` | First key branch + `.hax` URL, no audio bytes |
| `decryptHaxBuffer(haxBytes, keys, onProgress?)` | Offline HAX0 decrypt, no network |
| `extractHotaudioStream(pageUrl, opts?)` | Browser MSE session (`ready` / `done` / `abort`) |
| `extractHotaudio(pageUrl, opts?)` | Browser entry point: stream-first, Blob fallback |
| `extractHotaudioDownload(pageUrl, opts?)` | Browser full-download fallback |
| `signHotaudioPayload(payload)` | Raw `X-Signature` signer |
| `deriveSegmentKey` / `parseHax0Header` / `decryptSegmentSlice` | HAX0 primitives |
| `HOTAUDIO_PATTERN` / `isHotaudioUrl` | Share-link detection |

Options: `{ userAgent?, fetchFn?, apiBase?, onProgress? }`. Pass a custom `fetchFn` for proxies or test mocks. Full signatures live in [docs/API.md](docs/API.md).

Long tracks automatically page `first:<segmentIndex>` listen handshakes and memoize intermediate tree-node keys. One X25519 session is reused per track; every request still gets a fresh signature and nonce.

Use the default bare `Mozilla/5.0` user agent. Chrome user agents receive Cloudflare 403s on the track page and listen endpoint.

## How it works

1. Fetch the track page and decrypt the embedded `__ha_state` payload.
2. Establish an ephemeral X25519 session with the server public key.
3. Sign each listen payload with the pinned `nozzle.js` routine, encrypt it under the session secret, and exchange it for branch keys plus the `.hax` URL.
4. Fetch the HAX0 container, derive per-segment keys down the key tree, and ChaCha20-Poly1305 decrypt each slice into playable MP4 fragments.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the protocol, container layout, and signer details.

## Development

```bash
bun install
bun test          # offline unit tests (no network)
bunx tsc --noEmit # typecheck
bun run build     # emit dist/ via tsup
```

Project layout:

```
src/        library + CLI
tests/      offline unit tests
docs/       API, CLI, and architecture notes
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for workflow and release notes in [CHANGELOG.md](CHANGELOG.md).

## License

MIT — see [LICENSE](LICENSE).
