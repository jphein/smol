#!/usr/bin/env bash
# Build spike-scry on familiar (xtensa builds live there — JP directive
# 2026-08-24; toolchain pinned to match katana). Shape copied from
# ../spike/build-remote.sh; no wifi/radio tiers, so no vault access ever.
#   * rsync to ~/builds/<name>, NEVER the syncthing-mirrored ~/Projects tree
#   * familiar /tmp is a 512MB tmpfs — TMPDIR stays /var/tmp remotely
set -euo pipefail

REMOTE=familiar
RDIR="builds/spike-scry"
HERE="$(cd "$(dirname "$0")" && pwd)"
TRIPLE=xtensa-esp32s3-none-elf

rsync -a --delete --exclude target/ "$HERE/" "$REMOTE:$RDIR/"

# ⚠️ ${*:-} IS THE EXPERIMENT — forward the caller's feature flags. Omitting it
# silently rebuilds the DEFAULT tier and "proves" a tier that never compiled
# (../spike/build-remote.sh documents this exact trap; it bit here 2026-09-01,
# tell = a 3.7 s build where a cold feature build was expected).
ssh "$REMOTE" "cd $RDIR && export PATH=\"\$HOME/.cargo/bin:\$PATH\" && source ~/export-esp.sh && TMPDIR=/var/tmp cargo build --release ${*:-} 2>&1 | tail -30"

mkdir -p "$HERE/target/$TRIPLE/release"
rsync -a "$REMOTE:$RDIR/target/$TRIPLE/release/spike-scry" "$HERE/target/$TRIPLE/release/spike-scry"
echo "ELF pulled: target/$TRIPLE/release/spike-scry"
