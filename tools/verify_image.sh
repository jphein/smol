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
#   verify_image.sh [<commit>] --expect-bin <file>  # byte-compare vs a fetched image: full
#                                                   # sha AND masked sha (metadata excluded)
#   verify_image.sh [<commit>] [--node-id N] --twice # PROVE determinism: one COLD isolated
#                                                    # build vs one WARM in-tree build (the
#                                                    # mode ota_publish.sh actually uses)
#   verify_image.sh ... --build N                   # the staged build number (see below)
#   verify_image.sh ... --dev                       # dev stamp (default mirrors staging: release)
#
# <commit> defaults to HEAD — and MUST equal HEAD (#326 cause D): this tool never checks a
# commit out, so naming any other commit would stamp that identity onto TODAY'S source and
# "verify" the wrong bytes. Checking out into an isolated dir is NOT the fix: the build is
# path-dependent (cargo -C metadata hashes the crate path; measured 151,627 differing bytes
# from a different directory, zero leaked path strings), so verification only means anything
# from this repo's canonical path. To verify history: check out the commit HERE, then run.
#
# --build (#326 cause A): ota_publish.sh stages with a BROKER-ratcheted number
# (max(commit-count, staged+1)) which this offline tool cannot read; rust/clock/version.txt
# is a stale committed ratchet (345 vs a broker line in the 900s). So to match a staged
# image you MUST pass the build number from its announce (`OTA|<build>|…`). Defaulting is
# loud about this.
#
# --expect-bin masking: images staged before the #326 epoch fix carry an operator-ambient
# app-descriptor stamp that no honest rebuild reproduces. The masked comparison zeroes the
# esp_app_desc time/date fields (0x70–0x8F) and the trailing 33 B (esp-image sha digest +
# checksum, which change WITH the stamp) — everything else must match byte-for-byte. A
# masked MATCH + full MISMATCH is the expected verdict for pre-fix stages; post-fix stages
# must match in full.
#
# Read-only: NO flashing, NO MQTT, NO network — pure local build + sha256. Mirrors the
# identity contract of ota_publish.sh (same SMOL_GIT_HASH/BUILD_NUMBER/SOURCE_DATE_EPOCH
# pin, SMOL_RELEASE=1 by default to match staging, same espflash save-image), so a sha
# printed here equals the one that tool announces.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLOCK="$REPO/rust/clock"
# shellcheck source=tools/repro_build.sh
. "$(dirname "${BASH_SOURCE[0]}")/repro_build.sh"

die(){ echo "ERROR: $*" >&2; exit 1; }

COMMIT="HEAD"; NODE_ID=""; EXPECT=""; BIN=""; TWICE=0; BUILD_ARG=""; EXPECT_BIN=""; DEV=0
while [ $# -gt 0 ]; do case "$1" in
  --node-id)    NODE_ID="${2:?}"; shift 2;;
  --expect)     EXPECT="${2:?}"; shift 2;;
  --expect-bin) EXPECT_BIN="${2:?}"; shift 2;;
  --bin)        BIN="${2:?}"; shift 2;;
  --build)      BUILD_ARG="${2:?}"; shift 2;;
  --dev)        DEV=1; shift;;
  --twice)      TWICE=1; shift;;
  -h|--help)    sed -n '2,41p' "${BASH_SOURCE[0]}"; exit 0;;
  *)            COMMIT="$1"; shift;;
esac; done
[ -z "$NODE_ID" ] || case "$NODE_ID" in *[!0-9]*|'') die "--node-id must be a positive integer";; esac
[ -z "$BUILD_ARG" ] || case "$BUILD_ARG" in *[!0-9]*|'') die "--build must be a positive integer";; esac
[ -z "$EXPECT_BIN" ] || [ -f "$EXPECT_BIN" ] || die "no image at $EXPECT_BIN"

# --bin: hash an existing image, no build (parity with ota_publish.sh --bin).
if [ -n "$BIN" ]; then
  [ -f "$BIN" ] || die "no image at $BIN"
  printf 'bin=%s  size=%s  sha256=%s\n' "$BIN" "$(stat -c%s "$BIN")" "$(sha256sum "$BIN" | cut -d' ' -f1)"
  exit 0
fi

cd "$REPO"
HASH="$(git rev-parse --short=7 "$COMMIT")" || die "bad commit '$COMMIT'"
# #326 cause D: this tool NEVER checks out <commit> — it pins <commit>'s identity onto the
# source that is present. Verifying any commit other than HEAD therefore built the wrong
# bytes while printing the right label. Refuse instead: to verify history, check the commit
# out here (canonical path — see the path-dependence note in the header) and re-run.
if [ "$(git rev-parse "$COMMIT")" != "$(git rev-parse HEAD)" ]; then
  die "commit '$COMMIT' is not checked out (HEAD is $(git rev-parse --short=7 HEAD)) — \
this tool builds the WORKING TREE and cannot verify a commit that isn't checked out. \
git checkout $HASH here, re-run, then return to your branch."
fi
# Warn (don't die) on a dirty crate: the build would include uncommitted edits under the
# named commit's label. A deliberate local experiment is legitimate; an unnoticed one lies.
if ! git diff --quiet HEAD -- rust/clock 2>/dev/null; then
  echo "WARN: rust/clock has uncommitted changes — the built sha will NOT be $HASH's" >&2
fi
# #326 cause A: the staged build number comes from ota_publish.sh's BROKER ratchet, which
# this offline tool cannot read. Precedence: --build (from the staged announce) > the
# committed version.txt ratchet (stale: 345 while the broker line is in the 900s) > the raw
# commit count. Loud when defaulting, because a wrong number here silently forks the sha.
if [ -n "$BUILD_ARG" ]; then
  BUILD="$BUILD_ARG"
else
  BUILD="$(tr -d '[:space:]' < "$CLOCK/version.txt" 2>/dev/null || true)"
  [ -n "$BUILD" ] || BUILD="$(git rev-list --count "$COMMIT")"
  echo "note: no --build given — using $BUILD (version.txt/commit-count). Staged images use" >&2
  echo "      the broker ratchet; to match one, pass --build <N> from its OTA| announce." >&2
fi
# #326 cause B: staged images are release-stamped (ota_publish.sh exports SMOL_RELEASE=1),
# so the verifier must match or every comparison fails on the version-stamp string. --dev
# opts out for locally-flashed dev images.
DEVTAG=""
if [ "$DEV" = 0 ]; then export SMOL_RELEASE=1; else unset SMOL_RELEASE 2>/dev/null || true; DEVTAG=" [dev]"; fi
LABEL="build $BUILD ($HASH)${NODE_ID:+ node $NODE_ID}${DEVTAG}"

# Build into an ISOLATED target dir so a repeat build is a true from-scratch rebuild (and so
# --twice proves target-dir/path independence, not just a warm-cache no-op). Cleaned on exit.
build_once() { # <target_dir|""> <out_bin>   ("" = in-tree warm target/, the publish mode)
  local tdir="$1" out="$2"
  if [ -n "$tdir" ]; then
    CARGO_TARGET_DIR="$tdir" repro_build_bin "$CLOCK" "$out" "$HASH" "$BUILD" "$NODE_ID" \
      || die "reproducible build failed ($LABEL)"
  else
    # Drop any ambient CARGO_TARGET_DIR (subshell, since repro_build_bin is a function and
    # `env -u` can't invoke one): the warm half must build where ota_publish.sh builds —
    # the in-tree target/ — or the parity proof tests the wrong mode.
    ( unset CARGO_TARGET_DIR; repro_build_bin "$CLOCK" "$out" "$HASH" "$BUILD" "$NODE_ID" ) \
      || die "reproducible build failed ($LABEL)"
  fi
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
#   THE VARIABLE IS BYTES REMAINING AFTER THE MATCH — not size, and not position. Two
#   earlier models (a flat ~0.2% flake; "safe below 64 KB or if the match is late") were
#   both refuted; this is the third and the only one that predicts every datapoint. EPIPE
#   occurs iff the writer still has unwritten bytes when grep -q exits, the rate scales
#   with the residual, and it becomes certain past the pipe buffer. Measured at 263 KB,
#   300 iterations each: residual 0 bytes -> 0/300 both tools; residual SIXTEEN BYTES ->
#   ugrep 1/300, GNU grep 3.11 9/300. Here the first match sat at line 23 with ~86 KB
#   still to push, hence 20/20.
#   So "the match is at the end" is the most dangerous-looking exemption: almost true, and
#   false the moment anything appends. And GNU flaked 9x more than ugrep at identical
#   residual — existence is implementation-independent, RATE is not, so a site that
#   measures clean here can be worse on a CI runner. `grep -q` reading a FILE is 0/2000:
#   the pipeline is the defect, not grep.
#   SO THE RULE IS ABSOLUTE, NOT SIZED: never decide on `cmd | grep -q` status under
#   pipefail, at any size or match position. An earlier note here called this merely a
#   LATENT hazard because one test showed the old form detecting correctly — that image's
#   first match sat near the end. It said "do not cite this as evidence the guard was
#   broken"; it WAS broken, 20/20, and the instruction was wrong.
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
  # #326: the second build is WARM and IN-TREE — the mode ota_publish.sh actually uses —
  # not a second cold isolated dir. The old cold+cold form reported REPRODUCIBLE ✓ while
  # being STRUCTURALLY INCAPABLE of seeing the warm-cache stamp freeze that made every
  # staged sha unreproducible (same defect shape as a gate that cannot fail: it proved
  # determinism only in a mode the publish path never used). Warm-vs-cold parity is the
  # claim that matters, so it is the claim this tests. Mutates the in-tree target/ cache;
  # that cache is a build artifact, and exercising it is the point.
  echo "second build (WARM, in-tree — the ota_publish.sh mode) to prove parity ..." >&2
  build_once "" "$WORK/b.bin"
  SHA2="$(sha256sum "$WORK/b.bin" | cut -d' ' -f1)"
  if [ "$SHA" = "$SHA2" ]; then
    echo "REPRODUCIBLE ✓  cold isolated build == warm in-tree build  ($LABEL)"
  else
    echo "NOT REPRODUCIBLE ✗  cold $SHA != warm $SHA2  ($LABEL)" >&2
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

# --expect-bin: byte-compare the rebuild against a fetched image, full AND masked. The mask
# covers exactly the bytes an operator-ambient SOURCE_DATE_EPOCH could move on a pre-fix
# stage: esp_app_desc time[16]+date[16] at 0x70–0x8F, and the trailing 33 B (espflash's
# appended sha256 digest + checksum byte, which change WITH the stamp). Verdicts:
#   full ✓             — post-#326 stage, fully reproducible
#   full ✗ + masked ✓  — pre-#326 stage: code identical, stamp was ambient (expected for 915)
#   masked ✗           — genuinely different code: exit 3
if [ -n "$EXPECT_BIN" ]; then
  ESIZE="$(stat -c%s "$EXPECT_BIN")"
  if [ "$ESIZE" != "$SIZE" ]; then
    echo "MISMATCH ✗  size $ESIZE != $SIZE — different image, masking cannot apply" >&2
    exit 3
  fi
  if cmp -s "$WORK/a.bin" "$EXPECT_BIN"; then
    echo "FULL MATCH ✓  byte-identical to $EXPECT_BIN  ($LABEL)"
  else
    MSHA_A="$(python3 - "$WORK/a.bin" <<'EOF'
import hashlib, sys
b = bytearray(open(sys.argv[1], 'rb').read())
b[0x70:0x90] = bytes(0x20)   # esp_app_desc time[16] + date[16]
b[-33:] = bytes(33)          # espflash trailing checksum + sha256 digest
print(hashlib.sha256(bytes(b)).hexdigest())
EOF
)"
    MSHA_E="$(python3 - "$EXPECT_BIN" <<'EOF'
import hashlib, sys
b = bytearray(open(sys.argv[1], 'rb').read())
b[0x70:0x90] = bytes(0x20)
b[-33:] = bytes(33)
print(hashlib.sha256(bytes(b)).hexdigest())
EOF
)"
    if [ "$MSHA_A" = "$MSHA_E" ]; then
      echo "MASKED MATCH ✓ (full ✗)  code identical; only the app-desc stamp + digest differ"
      echo "                         — the pre-#326-fix signature. masked=$MSHA_A"
    else
      echo "MISMATCH ✗  masked shas differ — genuinely different code, not a stamp artefact" >&2
      echo "            built  masked=$MSHA_A" >&2
      echo "            target masked=$MSHA_E" >&2
      exit 3
    fi
  fi
fi
