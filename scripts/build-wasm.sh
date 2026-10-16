#!/usr/bin/env bash
# Build the JavaScript package: `pkg/web` for a page, `pkg/nodejs` for Node.
#
# Both are the same `wasm-bindgen` output over one `cargo build`, with the
# generated `.d.ts` types included. The crate stays dependency-free — this needs
# only `cargo`, the `wasm32-unknown-unknown` target, and a `wasm-bindgen` CLI
# that matches the `wasm-bindgen` crate in `Cargo.lock`. The two are
# version-locked: the CLI refuses to generate glue for a module built by a
# different crate version, so a mismatch is checked here with a command that
# fixes it rather than surfacing as a cryptic build error.
#
# Usage: scripts/build-wasm.sh [OUT_DIR]   (default: pkg)
set -euo pipefail

cd "$(dirname "$0")/.."
out="${1:-pkg}"

want=$(awk '/^name = "wasm-bindgen"$/{getline; print; exit}' Cargo.lock \
    | sed -n 's/^version = "\(.*\)"/\1/p')
if [ -z "$want" ]; then
    echo "error: could not read the wasm-bindgen version from Cargo.lock" >&2
    exit 1
fi

if ! command -v wasm-bindgen >/dev/null 2>&1; then
    echo "error: wasm-bindgen CLI not found on PATH." >&2
    echo "  cargo install wasm-bindgen-cli --version $want" >&2
    exit 1
fi

have=$(wasm-bindgen --version | awk '{print $2}')
if [ "$have" != "$want" ]; then
    echo "error: wasm-bindgen CLI $have does not match the crate ($want)." >&2
    echo "  cargo install wasm-bindgen-cli --version $want --force" >&2
    exit 1
fi

cargo build --release --locked --target wasm32-unknown-unknown
wasm=target/wasm32-unknown-unknown/release/hotaudio.wasm

rm -rf "$out/web" "$out/nodejs"
mkdir -p "$out"
wasm-bindgen --target web --out-dir "$out/web" "$wasm"
wasm-bindgen --target nodejs --out-dir "$out/nodejs" "$wasm"

echo "wrote $out/web and $out/nodejs"
