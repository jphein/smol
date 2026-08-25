#!/usr/bin/env bash
# Build the s3-cyd spike on familiar (24 cores), pull the ELF back so cargo's
# local `runner` contract (./flash.sh) still works.
#
# Shape copied from cyd-c5/spike/build-remote.sh. Xtensa-specific facts:
#   * familiar got espup 2026-08-24, toolchain PINNED to 1.95.0.0 to byte-match
#     katana (rustc 95e5bda86 / GCC esp-15.2.0_20250920 / clang esp-20.1.1_20250829).
#     CONDITION ON THE PIN: upgrade BOTH hosts in one motion or not at all — two
#     hosts building one crate with different compilers is how irreproducible
#     binaries happen. (JP directive 2026-08-24: xtensa builds move to familiar;
#     this retired PORT-SCOPING's "katana-only" standing exception.)
#   * rsync goes to ~/builds/<name>, NEVER the syncthing-mirrored ~/Projects tree
#     — that keeps target/ (GBs) out of the sync. Do not "simplify" the two into one.
#   * familiar's /tmp is a 512 MB tmpfs (it filled during the espup install and
#     disguised itself as a compile error, same class as smol#363's gate lesson).
#     TMPDIR stays /var/tmp for anything staged remotely.
set -euo pipefail

REMOTE=familiar
RDIR="builds/s3-cyd-spike"
HERE="$(cd "$(dirname "$0")" && pwd)"
TRIPLE=xtensa-esp32s3-none-elf

rsync -a --delete --exclude target/ "$HERE/" "$REMOTE:$RDIR/"

# Both PATH halves, remote edition — same two-disguise trap as local builds:
# missing first half = `cargo: command not found`; missing second = a
# "linker xtensa-esp32s3-elf-gcc not found" that impersonates a broken toolchain.
ssh "$REMOTE" "export PATH=\"\$HOME/.cargo/bin:\$PATH\" && . \$HOME/export-esp.sh && \
  cd $RDIR && TMPDIR=/var/tmp cargo build --release ${*:-}"

mkdir -p "$HERE/target/$TRIPLE/release"
rsync -a "$REMOTE:$RDIR/target/$TRIPLE/release/s3-cyd-spike" \
  "$HERE/target/$TRIPLE/release/s3-cyd-spike"
echo "ELF pulled: target/$TRIPLE/release/s3-cyd-spike (flash via ./flash.sh or cargo run --release)"
