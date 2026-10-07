# AGENTS.md

Guidance for coding agents working in this repo. Humans: see `README.md` and
`CONTRIBUTING.md`.

## What this is

Standalone hotaudio.net audio extractor. The Rust crate at the repo root is the
supported implementation. Fast and lightweight, but unstable — it tracks site
internals and can break when the site changes. The slow-and-stable alternative is
[coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader).

The TypeScript npm package is on the `legacy` branch, not in this branch. It is
frozen and exists only for its browser/MSE playback path. To work on it:
`git checkout legacy`. Do not port work in that direction; new features go here.

## Commands

```bash
cargo test                  # offline test suite, no network
cargo clippy --all-targets  # must stay at zero warnings
cargo fmt --all
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

Live checks against the real site are manual-only and never in CI:

```bash
bun scripts/verify-live.ts https://hotaudio.net/u/user/track-slug
```

## Layout

- `src/lib.rs` — crate root; `src/signer/` — the signature VM
- `src/listen.rs` — page handshake + encrypted listen API
- `src/hax.rs` — bencode, HAX0 header, segment key tree; `src/range.rs` — `Range` reads
- `src/download.rs` — pipeline, key paging, saved-keys envelope; `src/bin/download.rs` — CLI
- `scripts/` — recovery tooling (see below); `vendor/nozzle.js` — the pinned player bundle
- `docs/` — architecture, protocol, maintenance, migration

## Rules

- Do not add network calls to tests. `cargo test` must stay offline; use the
  `RangeSource` trait for anything that would otherwise hit the network.
- `scripts/verify-live.ts` and `scripts/recapture.ts` are the only files allowed
  to touch the real site. Never import them from `src/` or `tests/`.
- `src/signer/bytecode.rs` is **generated** — never hand-edit it. Re-run
  `scripts/recapture.ts`.
- `bytecode.rs`, `env.rs` (`PINNED_NOZZLE_VERSION` + `ENV_HASHES`) and the
  mirrored values in `scripts/shim.ts` are one pinned contract. Update all of
  them together or not at all, in the order `docs/MAINTENANCE.md` gives.
- Keep comments as concise rustdoc: invariants, units, failure modes, and *why*
  a non-obvious choice was made. No narration of what the code already says, and
  no bug-fix history in comments — that belongs in `CHANGELOG.md`.
- Intra-doc links must resolve, including from public items to private ones.
  Ambiguous links are errors under `-D warnings`; disambiguate with `mod@name`.
- Do not "improve" these deliberate non-goals without re-measuring — they are
  documented in `docs/ARCHITECTURE.md`: concurrency, stealth, and the fixed
  `Mozilla/5.0` user agent.
- Do not add JavaScript dependencies to the crate. `scripts/` are Bun scripts for
  the recovery toolchain and are the only thing that may use `@noble/ciphers`,
  which `verify-live.ts` needs and the crate does not.
- User-visible changes go under `CHANGELOG.md` → `Unreleased`.
