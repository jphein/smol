#!/usr/bin/env bash
# verify_image.sh — reproducibly build a smol fleet image and print/verify its hash (#44).
#
# The release ELF is now byte-reproducible for a fixed (commit, node-id) — see
# tools/repro_build.sh. This tool turns that into an image↔commit↔board CHECK you can run
# before OR after a flash, so a wrong-image flash (the dup-NODE_ID outage, #42) is catchable:
#
#   verify_image.sh [<commit>] [--node-id N]        # build → print  build size sha256
#   verify_image.sh [<commit>] [--node-id N] --expect <sha256>   # exit 0 match / 3 mismatch
#   verify_image.sh --bin <file>                    # just hash an existing .bin (no build)
#   verify_image.sh [<commit>] [--node-id N] --twice # PROVE determinism: 2 isolated builds,
#                                                    # assert identical sha + no leaked paths
#
# <commit> defaults to HEAD. Read-only: NO flashing, NO MQTT, NO network — pure local build
# + sha256. Mirrors the identity contract of ota_publish.sh (same SMOL_GIT_HASH/BUILD_NUMBER
# pin, same espflash save-image), so a sha printed here equals the one that tool announces.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLOCK="$REPO/rust/clock"
# shellcheck source=tools/repro_build.sh
. "$(dirname "${BASH_SOURCE[0]}")/repro_build.sh"

die(){ echo "ERROR: $*" >&2; exit 1; }

COMMIT="HEAD"; NODE_ID=""; EXPECT=""; BIN=""; TWICE=0
while [ $# -gt 0 ]; do case "$1" in
  --node-id) NODE_ID="${2:?}"; shift 2;;
  --expect)  EXPECT="${2:?}"; shift 2;;
  --bin)     BIN="${2:?}"; shift 2;;
  --twice)   TWICE=1; shift;;
  -h|--help) sed -n '2,17p' "${BASH_SOURCE[0]}"; exit 0;;
  *)         COMMIT="$1"; shift;;
esac; done
[ -z "$NODE_ID" ] || case "$NODE_ID" in *[!0-9]*|'') die "--node-id must be a positive integer";; esac

# --bin: hash an existing image, no build (parity with ota_publish.sh --bin).
if [ -n "$BIN" ]; then
  [ -f "$BIN" ] || die "no image at $BIN"
  printf 'bin=%s  size=%s  sha256=%s\n' "$BIN" "$(stat -c%s "$BIN")" "$(sha256sum "$BIN" | cut -d' ' -f1)"
  exit 0
fi

cd "$REPO"
HASH="$(git rev-parse --short=7 "$COMMIT")" || die "bad commit '$COMMIT'"
BUILD="$(git rev-list --count "$COMMIT")"
LABEL="build $BUILD ($HASH)${NODE_ID:+ node $NODE_ID}"

# Build into an ISOLATED target dir so a repeat build is a true from-scratch rebuild (and so
# --twice proves target-dir/path independence, not just a warm-cache no-op). Cleaned on exit.
build_once() { # <target_dir> <out_bin>
  local tdir="$1" out="$2"
  CARGO_TARGET_DIR="$tdir" repro_build_bin "$CLOCK" "$out" "$HASH" "$BUILD" "$NODE_ID" \
    || die "reproducible build failed ($LABEL)"
}

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
echo "building reproducible espnow image — $LABEL ..." >&2
build_once "$WORK/t1" "$WORK/a.bin"
SIZE="$(stat -c%s "$WORK/a.bin")"; SHA="$(sha256sum "$WORK/a.bin" | cut -d' ' -f1)"

# Reproducibility self-check: no absolute build path may survive in the shipped image.
if strings "$WORK/a.bin" | grep -qE '/home/|/Users/|\.cargo/registry|/rustlib/'; then
  echo "WARN: absolute build paths still present in the image — remap incomplete:" >&2
  strings "$WORK/a.bin" | grep -oE '(/home/|/Users/)[^ ]*|[^ ]*\.cargo/registry[^ ]*|[^ ]*/rustlib/[^ ]*' | sort -u | head -5 >&2
fi

if [ "$TWICE" = 1 ]; then
  echo "second isolated build to prove determinism ..." >&2
  build_once "$WORK/t2" "$WORK/b.bin"
  SHA2="$(sha256sum "$WORK/b.bin" | cut -d' ' -f1)"
  if [ "$SHA" = "$SHA2" ]; then
    echo "REPRODUCIBLE ✓  two isolated builds → identical sha256  ($LABEL)"
  else
    echo "NOT REPRODUCIBLE ✗  $SHA != $SHA2  ($LABEL)" >&2
    exit 4
  fi
fi

printf 'build=%s hash=%s%s size=%s sha256=%s\n' \
  "$BUILD" "$HASH" "${NODE_ID:+ node=$NODE_ID}" "$SIZE" "$SHA"

if [ -n "$EXPECT" ]; then
  if [ "$SHA" = "$EXPECT" ]; then
    echo "MATCH ✓  image is $LABEL"
  else
    echo "MISMATCH ✗  expected $EXPECT  got $SHA  — flashed image is NOT $LABEL" >&2
    exit 3
  fi
fi
