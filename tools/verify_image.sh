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
#
# ⚠️ 2026-07-28: this guard was BROKEN IN BOTH HALVES — it could not detect, and what
# it did report was noise. Both are fixed below. History kept because a reader who
# trusted its silence needs to know the silence was meaningless.
#
# HALF 1 — it could not detect. It was `if strings … | grep -qE …; then`, the
# EPIPE-under-pipefail hazard: `grep -q` exits 0 the instant it matches, `strings` keeps
# writing into a closed pipe and takes EPIPE, and `set -euo pipefail` (line 17) surfaces
# the WRITER's status — so a successful detection reports non-zero and the `if` goes
# FALSE, converting "found a leak" into "nothing found".
#   MEASURED on a real 1,435,008 B image (`strings` = 86,363 B = 1.32x the 64 KB pipe
#   buffer), 20 runs each: OLD form DETECTED 0 / MISSED 20. NEW form DETECTED 20 /
#   MISSED 0. Deterministic, not flaky.
#   THE DECIDING VARIABLE IS POSITION, not size: the first match sat at line 23, so grep
#   exited with ~86 KB still to push. An earlier note here claimed this was only a
#   LATENT hazard because one test showed the old form detecting correctly — that test
#   used an image whose first match sat near the END, where grep drains almost
#   everything and no EPIPE occurs. That note said "do not cite this as evidence the
#   guard was broken"; it WAS broken, 20/20, and the instruction was wrong.
#
# HALF 2 — what it reported was noise. Two SHAPE-based alternatives matched the remap
# TARGET, not host paths: `repro_build.sh` remaps the sysroot to `/rust`, so
# `[^ ]*/rustlib/[^ ]*` hit `/rust/lib/rustlib/…` on correctly-remapped images. On that
# image 4 of 4 matches were false positives and 0 were host paths. Narrowed to host
# roots only — measured 0 matches on a correct image, and still catches a planted
# `/home/jp/Projects/…`. Deliberately unanchored: a host path can sit mid-string, so `^`
# would miss embedded ones. `.cargo/registry` needed no alternative of its own (on a dev
# box it always lives under a host root), and a rustup sysroot is under `/home/…/.rustup`
# so `/rustlib/` needs no special case.
#
# ABSENCE OF BAD IS NOT ENOUGH — assert PRESENCE OF GOOD too. An image that was never
# remapped at all also contains no host paths, and an absence-only check calls that
# clean. So we additionally require the remap's own targets to be present: on a real
# image `/registry/` appears 65x and `/rust/lib/` 4x. Absence-only crushed three states
# ("remapped", "never remapped", "no paths embedded") into two — the same defect shape as
# a two-valued `cc=`.
leaked="$(strings "$WORK/a.bin" | grep -oE '(/home/|/Users/|/root/)[^ ]*' | sort -u || true)"
remapped="$(strings "$WORK/a.bin" | grep -cE '/registry/|/rust/lib/' || true)"
if [ "${remapped:-0}" -eq 0 ]; then
  echo "WARN: no remap targets (/registry/, /rust/lib/) found in the image — the path" >&2
  echo "      remap likely never ran. An absence of host paths does NOT mean remapped." >&2
fi
if [ -n "$leaked" ]; then
  echo "WARN: absolute build paths still present in the image — remap incomplete:" >&2
  # No pipe: `printf … | head -5` is the SAME EPIPE-under-pipefail shape as the bug this
  # block exists to fix — head exits after 5 lines, printf takes EPIPE, and `set -e`
  # aborts the script IN THE WARNING PATH. Confirmed reachable synthetically at ~154 KB;
  # not reachable on a real image (measured 251 B / 4 lines), so this is prevention, not
  # a fix. Reading it in bash removes the class rather than bounding it.
  n=0
  while IFS= read -r _l; do
    [ "$n" -lt 5 ] || { echo "      … (truncated)" >&2; break; }
    printf '      %s\n' "$_l" >&2; n=$((n+1))
  done <<<"$leaked"
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
