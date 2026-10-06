# Contributing

## Setup

```bash
bun install
bun test
bunx tsc --noEmit
bun run build
```

Tests are offline unit tests (`bun test`) covering hex/codec helpers,
bencode, synthetic HAX0 headers, key derivation, and handshake helpers.
They require no network access and must stay that way — mock `fetchFn`
for anything touching the network.

## Workflow

- Keep the public API in `src/index.ts` as the single export surface.
- Keep Node-only imports (`node:fs`) dynamic and isolated to `downloadHotaudioToFile` and `src/cli.ts` so browser bundles stay clean.
- Keep comments concise: document invariants, units, and failure modes. Avoid narration of what the code already says.
- The signer shims (`src/signer.ts`, `src/env_hashes.ts`, `src/nozzle_raw.ts`) are pinned to one upstream player build. Update all three together and bump `PINNED_NOZZLE_VERSION`.

## Commits and releases

- Small, focused commits with imperative subjects (`Add offline decrypt test`).
- Update `CHANGELOG.md` under `Unreleased` for user-visible changes.
- Release flow: bump `version` in `package.json`, move entries to a new `CHANGELOG.md` section, `bun run build`, tag `vX.Y.Z`.
