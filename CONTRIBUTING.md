# Contributing

## Setup

```bash
cargo build
cargo test
cargo clippy --all-targets
cargo fmt --all
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

`cargo test` is an offline suite covering the signer VM against golden vectors,
the HAX0 container round-trip, key-tree derivation against keys captured from a
real track, and the resumable-transfer fallback. It requires no network access
and must stay that way — anything that would need a server goes behind the
`RangeSource` trait.

## Workflow

- `cargo clippy --all-targets` must stay at zero warnings; CI enforces it with
  `-D warnings`.
- `cargo fmt --all` must be clean; CI checks it.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` must be clean; CI checks it.
  Doc links must resolve, including from public items to private ones.
- Run `cargo test` before committing. CI also builds against the MSRV (1.85).
- The third-party notice bundled into release archives is generated at release
  time from the tag's `Cargo.lock` (`.github/workflows/release.yml`), so there
  is nothing to regenerate when dependencies change. `deny.toml` (cargo-deny in
  CI) is the license *policy* gate; the notice is the *attribution*.
- Keep comments concise: document invariants, units, failure modes, and the
  reasoning behind non-obvious choices. Avoid narrating what the code already
  says, and avoid recording bug-fix history in comments — that belongs in
  `CHANGELOG.md`.
- `src/signer/bytecode.rs` is generated. Do not hand-edit it — re-run
  `bun scripts/recapture.ts`. `bytecode.rs`, `src/signer/env.rs` and
  `scripts/shim.ts` are one pinned contract and move together; the procedure and
  the failure-symptom table are in [`docs/MAINTENANCE.md`](docs/MAINTENANCE.md).
- Do not change the user agent, add concurrency, or add a `package.json` at the
  root without reading the "Deliberate non-goals" section of
  [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) first. Those decisions are
  backed by measurements.
- The `legacy` branch holds the frozen TypeScript package. Fix bit-rot there if
  its CI breaks, but do not add features.

## When the site changes

Don't guess which stage broke — run the live acceptance suite, which names the
failing component:

```bash
cargo build --release --bin hotaudio-download
cargo run --release --manifest-path live/Cargo.toml -- \
  --cli "$PWD/target/release/hotaudio-download"
```

Then follow [`docs/MAINTENANCE.md`](docs/MAINTENANCE.md). CI also runs the suite
daily (`.github/workflows/live.yml`), so a red run already points at the
component that moved.

## Commits and releases

- Small, focused commits with imperative subjects (`Add offline decrypt test`).
- Update `CHANGELOG.md` under `Unreleased` for user-visible changes.
- Release flow: bump `version` in `Cargo.toml`, move the `Unreleased` entries to
  a new dated `CHANGELOG.md` section, and tag `vX.Y.Z`.
