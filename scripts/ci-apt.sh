#!/usr/bin/env bash
# Install Debian packages on a GitHub runner without the Azure Ubuntu mirror.
#
# The ubuntu-24.04 images point apt at azure.archive.ubuntu.com, which has been
# observed to crawl (a 100 kB package taking a minute) and to hold the dpkg lock
# long enough for an install to look hung. Switch to the upstream archive, bound
# every network operation, and stop unattended-upgrades from competing for the
# lock.
#
# Usage: sudo scripts/ci-apt.sh PACKAGE...
set -euo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: $0 PACKAGE..." >&2
    exit 2
fi

# Rewrite both deb822 sources and legacy one-line lists, whichever the image has.
for file in /etc/apt/sources.list \
    /etc/apt/sources.list.d/*.sources \
    /etc/apt/sources.list.d/*.list; do
    [ -e "$file" ] || continue
    sed -i 's|azure\.archive\.ubuntu\.com|archive.ubuntu.com|g' "$file"
done

# The runner image starts this; it can hold the dpkg lock through a whole job.
systemctl stop unattended-upgrades.service 2>/dev/null || true

acquire=(
    -o Acquire::Retries=5
    -o Acquire::http::Timeout=30
    -o Acquire::https::Timeout=30
    -o Acquire::ForceIPv4=true
)

timeout 600 apt-get "${acquire[@]}" update -qq
DEBIAN_FRONTEND=noninteractive timeout 900 \
    apt-get "${acquire[@]}" install -y --no-install-recommends "$@"
