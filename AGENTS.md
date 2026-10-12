# AGENTS.md

Guidance for coding agents working in this repo. Humans: see `README.md` and
`CONTRIBUTING.md`.

## What this is

Standalone hotaudio.net audio extractor. The Rust crate at the repo root is the
supported implementation. Fast and lightweight, but unstable — it tracks site
internals and can break when the site changes. The slow-and-stable alternative is
[coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader).

The TypeScript npm package is on the `legacy` branch, not in this branch. It is
frozen. To work on it: `git checkout legacy`. Do not port work in that direction;
new features go here. Its browser/MSE path now has an equivalent in
`src/wasm.rs`.

## Targets

One implementation, two transports. The pipeline is `async` at the I/O boundary
so the same code drives blocking `ureq` natively and `fetch` on
`wasm32-unknown-unknown`. Both configurations must build, pass clippy and pass
their tests.

## Commands

```bash
cargo test                  # offline test suite, no network
cargo clippy --all-targets  # must stay at zero warnings
cargo fmt --all
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# Browser build. wasm32 binaries cannot be exec'd; this runner executes them
# through wasm-bindgen, which is what a page does.
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
cargo test --target wasm32-unknown-unknown
cargo clippy --target wasm32-unknown-unknown --all-targets

# Throughput guard for the decrypt path. The timing half only exists in
# optimised builds (`#[cfg(not(debug_assertions))]`), so no flag is needed.
cargo test --release --test perf_guard
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
- `src/http/` — `Client` + shared policy, with one transport per target
- `src/clock.rs` — clocks and sleeping, per target
- `src/wasm.rs` — the `wasm-bindgen` surface, `wasm32` only
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
  which `verify-live.ts` needs and the crate does not. The `wasm32`-gated
  `wasm-bindgen`/`web-sys` deps are the one exception: they are Rust crates that
  only build for that target, and they are what the browser transport is built
  on.
- Keep `crypto`, `hax` and `signer` synchronous and free of `cfg(target_arch)`.
  They are the CPU-bound half of the pipeline and are identical on both targets;
  putting an `async` or a per-target branch in them would make the browser a
  second implementation of the protocol rather than a second transport.
- Never name `SystemTime` or `Instant` outside `src/clock.rs`: both **panic** on
  `wasm32-unknown-unknown`. Go through `clock::unix_secs` / `monotonic_ms`.
- Tests carry both `#[test]` and `#[wasm_bindgen_test]` attributes, or use
  `async_test!`, and must pass on both targets. Use `#![cfg(target_arch = ...)]`
  only where the code genuinely cannot exist on one of them.
- Do not ship a proxy for the browser build. It is the embedder's infrastructure;
  the crate provides `with_url_rewrite` / `with_fetch` and documents what a proxy
  must do (`docs/BROWSER.md`).
- Do not use `#[ignore]` to gate a test on a build profile; use
  `#[cfg(debug_assertions)]`/`#[cfg(not(debug_assertions))]` so it runs wherever it
  applies and nobody has to remember to pass an extra flag.
- User-visible changes go under `CHANGELOG.md` → `Unreleased`.
