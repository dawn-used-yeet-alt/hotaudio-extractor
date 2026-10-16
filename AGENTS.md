# AGENTS.md

Agent guidance for this repo. Humans: see `README.md` and `CONTRIBUTING.md`.

## What this is

A standalone hotaudio.net audio extractor. The Rust crate at the repo root is the
supported implementation: fast and lightweight, but unstable, because it tracks
site internals and breaks when the site moves. The slow-and-stable alternative is
[coldvideo-downloader](https://github.com/rebelonion/coldvideo-downloader).

The TypeScript npm package lives on the `legacy` branch and is frozen. Never port
work from it; new features go here. Its browser/MSE path is now `src/wasm.rs`.

## Targets

One implementation, two transports. The pipeline is `async` only at the I/O
boundary, so the same code drives blocking `ureq` natively and `fetch` on
`wasm32-unknown-unknown`. Both must build, pass clippy, and pass their tests.

## Commands

```bash
cargo test                  # offline test suite; must never touch the network
cargo clippy --all-targets  # zero warnings
cargo fmt --all
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# Perf guard for the decrypt path; its timing half only compiles in release.
cargo test --release --test perf_guard

# Browser build. wasm32 binaries cannot be exec'd, so this runner runs them
# through wasm-bindgen, the way a page does.
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
cargo test --target wasm32-unknown-unknown
cargo clippy --target wasm32-unknown-unknown --all-targets
```

The live acceptance suite is `live/` — a separate workspace, never built by
`cargo test`, run daily by `.github/workflows/live.yml`. By hand:

```bash
cargo build --release --bin hotaudio-download
cargo run --release --manifest-path live/Cargo.toml -- \
  --cli "$PWD/target/release/hotaudio-download" \
  --url https://hotaudio.net/u/user/track-slug
```

## Layout

- `src/lib.rs` — crate root; `src/signer/` — the signature VM
- `src/listen.rs` — page handshake + encrypted listen API
- `src/hax.rs` — bencode, HAX0 header, segment key tree; `src/range.rs` — `Range` reads
- `src/download.rs` — pipeline, key paging, saved-keys envelope; `src/bin/download.rs` — CLI
- `src/http/` — `Client` + shared policy, one transport per target
- `src/clock.rs` — clocks and sleeping, per target
- `src/wasm.rs` — the `wasm-bindgen` surface, `wasm32` only
- `scripts/` — signer-recovery tooling and `build-wasm.sh`, the wasm packager;
  `vendor/nozzle.js` — the pinned player bundle
- `live/` — the live acceptance suite (its own workspace)
- `docs/` — architecture, protocol, maintenance, migration, browser

## Rules

**Offline by default.**

- `cargo test` stays offline. Anything that would hit the network takes a
  `RangeSource`, and the only code allowed to touch the real site is `live/` and
  `scripts/recapture.ts`; never import either from `src/` or `tests/`.
- `live/` is its own workspace, and it is deliberately unlocked (no committed
  `Cargo.lock`) so the suite always resolves against the crate's current
  dependencies.
- No JavaScript dependencies. `scripts/*.ts` are self-contained Bun scripts, the
  `scripts/build-wasm.sh` packager is a shell script, and there is no
  `package.json` at the root (the one in the generated `pkg/` is `wasm-bindgen`'s
  own output). The only JS-facing code is the `wasm32`-gated
  `wasm-bindgen`/`web-sys` binding layer, which is Rust.

**One implementation, two targets.**

- Keep `crypto`, `hax` and `signer` synchronous and free of `cfg(target_arch)`.
  They are the CPU-bound, target-identical half of the pipeline; an `async` or a
  per-target branch there would make the browser a second implementation of the
  protocol rather than a second transport.
- Never name `SystemTime` or `Instant` outside `src/clock.rs` — both panic on
  `wasm32-unknown-unknown`. Use `clock::unix_secs` / `clock::monotonic_ms`.
- Tests carry both `#[test]` and `#[wasm_bindgen_test]` (or use `async_test!`) and
  must pass on both targets. `#![cfg(target_arch = ...)]` is only for code that
  genuinely cannot exist on one of them.
- Do not gate a test on a build profile with `#[ignore]`; use
  `#[cfg(debug_assertions)]` / `#[cfg(not(debug_assertions))]` so it runs wherever
  it applies.

**Generated and pinned.**

- `src/signer/bytecode.rs` is generated — never hand-edit it; re-run
  `scripts/recapture.ts`.
- `bytecode.rs`, `src/signer/env.rs` (`PINNED_NOZZLE_VERSION` + `ENV_HASHES`) and
  `scripts/shim.ts` are one pinned contract. Move them together, in the order
  `docs/MAINTENANCE.md` gives.

**Deliberate non-goals.** Do not "improve" these without re-measuring; the
measurements live in `docs/ARCHITECTURE.md`.

- No concurrency, no stealth, and `Mozilla/5.0` stays fixed.
- No proxy in the browser build: it is the embedder's infrastructure. The crate
  exposes `with_url_rewrite` / `with_fetch` and documents the contract in
  `docs/BROWSER.md`.

**Docs.**

- Comments are concise rustdoc: invariants, units, failure modes, and *why* a
  non-obvious choice was made. Never narrate what the code already says, and keep
  bug-fix history in `CHANGELOG.md`, not comments.
- Intra-doc links must resolve, including from public items to private ones.
  Ambiguous links fail under `-D warnings`; disambiguate with `mod@name`.
- User-visible changes go under `CHANGELOG.md` → `Unreleased`.
