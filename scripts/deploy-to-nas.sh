#!/usr/bin/env bash
# Copy the source to the NAS, build there, and leave a static binary in place.
#
#   scripts/deploy-to-nas.sh root@tower /mnt/user/appdata/photo-cleanup
set -euo pipefail

HOST="${1:?usage: deploy-to-nas.sh <user@host> [remote-dir]}"
DIR="${2:-/mnt/user/appdata/photo-cleanup}"
SRC="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> копирую исходники в $HOST:$DIR/src"
rsync -az --delete \
  --exclude target --exclude .git --exclude dist --exclude '*.db*' \
  "$SRC/" "$HOST:$DIR/src/"

echo "==> собираю на NAS (нативно, amd64)"
ssh "$HOST" "cd '$DIR/src' && docker build --target export --output type=local,dest='$DIR' ."

echo "==> готово: $DIR/photo-cleanup"
ssh "$HOST" "'$DIR/photo-cleanup' --version && file '$DIR/photo-cleanup' 2>/dev/null || true"
