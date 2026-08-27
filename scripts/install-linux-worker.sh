#!/usr/bin/env bash
# Install beenet-worker onto PATH (Docker-style: install once, then use the binary).
#
# From a release tarball:
#   sudo ./install-linux-worker.sh
#   bworker --join-token-file ./join-token   # first run
#   bworker                                  # later runs
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${PREFIX:-/usr/local/bin}"
SRC="${1:-$DIR/beenet-worker}"

if [[ ! -x "$SRC" ]]; then
  if [[ -x "$DIR/../target/release/beenet-worker" ]]; then
    SRC="$DIR/../target/release/beenet-worker"
  else
    echo "error: beenet-worker not found next to this script" >&2
    echo "build with: make linux-worker" >&2
    exit 1
  fi
fi

install -d "$PREFIX"
install -m 0755 "$SRC" "$PREFIX/beenet-worker"
ln -sfn beenet-worker "$PREFIX/bworker"
echo "installed $PREFIX/beenet-worker"
echo "alias     $PREFIX/bworker"
echo "first run:  bworker --join-token-file ./join-token"
echo "later runs: bworker"
echo "quota:      bworker writes cgroup v2; systemd should Delegate=yes, not CPUQuota"
