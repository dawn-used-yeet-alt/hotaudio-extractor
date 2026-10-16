#!/bin/sh
# Installs hotaudio-download for this machine.
#
# It is attached to every release so it can be piped straight into a shell:
#
#   curl -fsSL https://github.com/dawn-used-yeet-alt/hotaudio-extractor/releases/latest/download/install.sh | sh
#
# Archive names carry no version, so that URL and the mapping below are stable
# across releases. `--print` resolves the archive without downloading anything.
#
# Overrides, used by CI to exercise the mapping without the matching hardware
# (and by anyone cross-installing for another machine):
#   HOTAUDIO_OS    what `uname -s` would print  (Linux, Darwin)
#   HOTAUDIO_ARCH  what `uname -m` would print  (x86_64, aarch64, arm64)
#   HOTAUDIO_DIR   install directory
#   HOTAUDIO_BASE  release download URL base
set -eu

repo=dawn-used-yeet-alt/hotaudio-extractor
base=${HOTAUDIO_BASE:-https://github.com/${repo}/releases/latest/download}
docs="https://github.com/${repo}/blob/main/docs/INSTALL.md"
os=${HOTAUDIO_OS:-$(uname -s)}
arch=${HOTAUDIO_ARCH:-$(uname -m)}

die() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

# Termux runs on Android and installs into its own prefix.
termux=false
case "${PREFIX:-}" in *com.termux*) termux=true ;; esac

case "$os" in
  Linux)
    if [ "$termux" = true ]; then
      case "$arch" in
        aarch64 | arm64) slug=android-arm64 ;;
        x86_64 | amd64) slug=android-x86_64 ;;
        *) die "no Termux build for '$arch'; see $docs" ;;
      esac
    else
      # Static: it needs no libc, so there is no reason to guess between glibc
      # and musl hosts.
      case "$arch" in
        x86_64 | amd64) slug=linux-x86_64-static ;;
        aarch64 | arm64) slug=linux-arm64-static ;;
        *) die "no Linux build for '$arch'; see $docs" ;;
      esac
    fi
    ;;
  Darwin)
    case "$arch" in
      x86_64 | amd64) slug=macos-x86_64 ;;
      aarch64 | arm64) slug=macos-arm64 ;;
      *) die "no macOS build for '$arch'; see $docs" ;;
    esac
    ;;
  *)
    die "no prebuilt binary for '$os'. Windows and manual installs are in $docs"
    ;;
esac

archive="hotaudio-download-${slug}.tar.gz"

if [ "$termux" = true ]; then
  dir=${HOTAUDIO_DIR:-$PREFIX/bin}
else
  dir=${HOTAUDIO_DIR:-$HOME/.local/bin}
fi

mode=install
while [ $# -gt 0 ]; do
  case "$1" in
    --print) mode=print ;;
    --dir)
      shift
      [ $# -gt 0 ] || die "--dir needs a value"
      dir=$1
      ;;
    --help | -h) mode=help ;;
    *) die "unknown option '$1' (try --help)" ;;
  esac
  shift
done

case "$mode" in
  print)
    printf '%s\n' "$archive"
    exit 0
    ;;
  help)
    cat <<EOF
Install hotaudio-download for this machine.

Usage: install.sh [--dir DIR] [--print]

  --dir DIR  install into DIR instead of \$HOME/.local/bin
  --print    print the archive this machine needs, and stop
  --help     print this text

Environment: HOTAUDIO_DIR, HOTAUDIO_OS, HOTAUDIO_ARCH, HOTAUDIO_BASE
Every platform, and manual downloads: $docs
EOF
    exit 0
    ;;
esac

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO "$2" "$1"; }
else
  die "curl or wget is required"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

printf 'Downloading %s\n' "$archive"
fetch "$base/$archive" "$tmp/$archive" || die "could not download $archive"

# The checksums are advisory: a missing tool or a missing SHA256SUMS.txt is a
# warning, a mismatch is fatal.
if fetch "$base/SHA256SUMS.txt" "$tmp/SHA256SUMS.txt" 2>/dev/null; then
  expected=$(grep " \./${archive}\$" "$tmp/SHA256SUMS.txt" | head -1 | cut -d' ' -f1)
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$archive" | cut -d' ' -f1)
  elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp/$archive" | cut -d' ' -f1)
  else
    actual=
  fi
  if [ -n "$expected" ] && [ -n "$actual" ]; then
    [ "$expected" = "$actual" ] || die "checksum mismatch for $archive"
    printf 'Checksum ok\n'
  else
    printf 'install.sh: could not verify the checksum\n' >&2
  fi
fi

tar -xzf "$tmp/$archive" -C "$tmp" || die "could not extract $archive"
src="$tmp/hotaudio-download-$slug/hotaudio-download"
[ -f "$src" ] || die "unexpected archive layout in $archive"

mkdir -p "$dir" || die "could not create $dir"
cp "$src" "$dir/hotaudio-download" || die "could not write $dir/hotaudio-download"
chmod 755 "$dir/hotaudio-download"

version=$("$dir/hotaudio-download" --version 2>/dev/null || true)
if [ -n "$version" ]; then
  printf 'Installed %s\n' "$version"
else
  printf 'Installed %s/hotaudio-download\n' "$dir"
fi

case ":${PATH}:" in
  *":$dir:"*) ;;
  *)
    cat <<EOF

$dir is not on your PATH. Add it, then reload your shell:

  export PATH="$dir:\$PATH"
EOF
    ;;
esac

printf '\nTry it:\n  hotaudio-download "https://hotaudio.net/u/user/track-slug"\n'
