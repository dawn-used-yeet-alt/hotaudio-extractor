#!/usr/bin/env bash
# Build the FFI library and generate Kotlin, Swift, Python and Ruby bindings.
#
# The generator is pinned by `bindings/Cargo.lock` and built from source, so the
# emitted bindings always match the runtime the library was compiled against.
# One build produces all four languages; the per-language staging scripts
# (`build-python.sh`, and the Android/iOS recipes in `docs/BINDINGS.md`) start
# from this output.
#
# Usage: scripts/build-bindings.sh [OUT_DIR] [--release]
#   OUT_DIR defaults to bindings/out.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

out="bindings/out"
profile=debug
for arg in "$@"; do
    case "$arg" in
        --release) profile=release ;;
        --*) echo "error: unknown flag $arg" >&2; exit 2 ;;
        *) out="$arg" ;;
    esac
done

if [ ! -f bindings/Cargo.lock ]; then
    echo "error: bindings/Cargo.lock is missing; run cargo generate-lockfile in bindings/" >&2
    exit 1
fi

if [ "$profile" = release ]; then
    cargo build --manifest-path bindings/Cargo.toml --locked --release -p hotaudio-ffi
else
    cargo build --manifest-path bindings/Cargo.toml --locked -p hotaudio-ffi
fi

case "$(uname -s)" in
    Darwin) lib="libhotaudio_ffi.dylib" ;;
    MINGW* | MSYS* | CYGWIN*) lib="hotaudio_ffi.dll" ;;
    *) lib="libhotaudio_ffi.so" ;;
esac
source_lib="bindings/target/$profile/$lib"
if [ ! -f "$source_lib" ]; then
    echo "error: expected $source_lib after the build" >&2
    exit 1
fi

rm -rf "$out"
mkdir -p "$out"
out_abs=$(cd "$out" && pwd)

# Run from the bindings workspace: library mode shells out to `cargo metadata`,
# and only that directory resolves `hotaudio-ffi` and its `uniffi.toml`. From
# the repository root the metadata names the core crate instead, and the
# generator would silently fall back to default package/module names.
(
    cd bindings
    cargo run --manifest-path Cargo.toml --locked -p uniffi-bindgen -- generate \
        -l kotlin -l swift -l python -l ruby \
        --out-dir "$out_abs" "$root/$source_lib"
)

echo "wrote bindings to $out"
