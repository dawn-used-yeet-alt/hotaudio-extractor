# AGENTS.md

Guidance for coding agents working in this repo. Humans: see `README.md` and `CONTRIBUTING.md`.

## What this is

Standalone hotaudio.net audio extractor. Fast and lightweight, but
unstable — it tracks site internals and can break when the site changes.
The slow-and-stable alternative is
[coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader).
No app dependencies; runtime deps are `@noble/ciphers` + `@noble/curves` only.

## Commands

```bash
bun install
bun test          # offline unit tests, no network
bunx tsc --noEmit # typecheck
bun run build     # emit dist/ via tsup
```

All three must pass before committing. Tests must stay offline.

## Layout

- `src/index.ts` — sole public export surface; `src/cli.ts` — CLI
- `src/listen.ts` — page handshake + encrypted listen API
- `src/signer.ts` — `nozzle.js` signature routine in a browser shim
- `src/crypto.ts`, `src/hax_decoder.ts` — X25519/HAX0 primitives
- `src/download.ts` — full download + offline decrypt; `src/stream.ts` — MSE playback
- `tests/offline.test.ts` — `bun:test` suite; `docs/` — API, CLI, architecture

## Rules

- Do not add network calls to tests. Mock via the `fetchFn` option.
- Keep `node:fs` imports dynamic and confined to `downloadHotaudioToFile` and `src/cli.ts` so browser bundles stay clean.
- `signer.ts`, `env_hashes.ts`, `nozzle_raw.ts`, and `PINNED_NOZZLE_VERSION` are pinned to one upstream player build — update all four together or not at all.
- `erasableSyntaxOnly` is on: no enums, namespaces, or parameter properties.
- Keep comments as concise TSDoc (invariants, units, failure modes). No narration of what the code already says.
- Default UA must stay the bare `Mozilla/5.0` — Chrome UAs get Cloudflare 403s.
- User-visible changes go under `CHANGELOG.md` → `Unreleased`.
