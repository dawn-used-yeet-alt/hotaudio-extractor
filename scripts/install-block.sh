#!/usr/bin/env bash
# Renders the install block in docs/INSTALL.md from scripts/release-targets.tsv.
#
# The release archives are named by a friendly slug (`linux-x86_64`) rather than
# a Rust target triple, and carry no version, so
# `releases/latest/download/<archive>` is a permanent URL: the commands here are
# valid release after release, and the copy of this file shipped inside each
# archive is never stale. That also makes the block a generated artifact rather
# than prose someone has to remember to bump, which is why CI runs `--check` on
# every push and release.yml runs `--write` after publishing.
#
# The README is deliberately not generated: it carries one `install.sh` line and
# a link here, so there is nothing in it that a release can invalidate.
#
# Usage:
#   install-block.sh --write   rewrite docs/INSTALL.md in place
#   install-block.sh --check   fail if docs/INSTALL.md does not match the table
#   install-block.sh           print the block to stdout
set -euo pipefail

repo=dawn-used-yeet-alt/hotaudio-extractor
base="https://github.com/${repo}/releases/latest/download"
track_url="https://hotaudio.net/u/user/track-slug"

here=$(cd "$(dirname "$0")" && pwd)
table="$here/release-targets.tsv"
target=${TARGET_FILE:-docs/INSTALL.md}
start='<!-- install:start -->'
end='<!-- install:end -->'

# Field order is documented in the table itself.
render() {
  local target slug runner native label featured archive winpath
  printf '\n'
  while IFS=$'\t' read -r target slug runner native label featured || [ -n "$target" ]; do
    case "$target" in '' | '#'*) continue ;; esac

    if [ "$featured" != true ]; then continue; fi

    printf '**%s**\n\n' "$label"
    if [ "$runner" = windows-latest ]; then
      archive="hotaudio-download-${slug}.zip"
      winpath=".\\hotaudio-download-${slug}\\hotaudio-download.exe"
      printf '```powershell\n'
      printf 'Invoke-WebRequest %s/%s -OutFile %s\n' "$base" "$archive" "$archive"
      printf 'Expand-Archive %s\n' "$archive"
      printf '%s "%s"\n' "$winpath" "$track_url"
      printf '```\n\n'
    else
      archive="hotaudio-download-${slug}.tar.gz"
      printf '```bash\n'
      printf 'curl -LO %s/%s\n' "$base" "$archive"
      printf 'tar xzf %s\n' "$archive"
      printf './hotaudio-download-%s/hotaudio-download "%s"\n' "$slug" "$track_url"
      printf '```\n\n'
    fi
  done <"$table"

  printf '| Your system | Archive |\n'
  printf '| --- | --- |\n'
  while IFS=$'\t' read -r target slug runner native label featured || [ -n "$target" ]; do
    case "$target" in '' | '#'*) continue ;; esac
    if [ "$runner" = windows-latest ]; then
      archive="hotaudio-download-${slug}.zip"
    else
      archive="hotaudio-download-${slug}.tar.gz"
    fi
    printf '| %s | [`%s`](%s/%s) |\n' "$label" "$archive" "$base" "$archive"
  done <"$table"
  printf '| Browser / Node (wasm) | [`hotaudio-wasm.tar.gz`](%s/hotaudio-wasm.tar.gz) |\n' "$base"
  printf '| Shell completions + man page | [`hotaudio-completions.tar.gz`](%s/hotaudio-completions.tar.gz) |\n' "$base"
  printf '\n'
}

# Replaces everything between the markers in $1 with the generated block ($2).
replace_block() {
  local file=$1 block=$2 tmp
  tmp=$(mktemp)
  awk -v start="$start" -v end="$end" -v block="$block" '
    $0 == start {
      print
      while ((getline line < block) > 0) print line
      close(block)
      inside = 1
      next
    }
    $0 == end { inside = 0 }
    inside != 1 { print }
  ' "$file" >"$tmp"
  mv "$tmp" "$file"
}

block=$(mktemp)
current=$(mktemp)
trap 'rm -f "$block" "$current"' EXIT
render >"$block"

has_markers() {
  grep -qxF "$start" "$target" && grep -qxF "$end" "$target"
}

case "${1:-}" in
  --write)
    has_markers || {
      echo "::error::$target has no $start / $end markers" >&2
      exit 1
    }
    replace_block "$target" "$block"
    ;;
  --check)
    has_markers || {
      echo "::error::$target has no $start / $end markers" >&2
      exit 1
    }
    cp "$target" "$current"
    replace_block "$current" "$block"
    if ! diff -u --label "$target" --label "$target (generated)" "$target" "$current"; then
      echo "::error::$target install block is stale; run scripts/install-block.sh --write" >&2
      exit 1
    fi
    echo "$target install block is current"
    ;;
  '')
    cat "$block"
    ;;
  *)
    echo "usage: install-block.sh [--write|--check]" >&2
    exit 2
    ;;
esac
