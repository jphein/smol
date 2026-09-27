#!/usr/bin/env bash
# build_hand.sh — COMPILE every `[hand_build]` row of tools/build-matrix.toml, from the recipe the
# manifest derives. The CI job `hand builds` in .github/workflows/fw-gate.yml runs it; so does
# `tools/gate.sh hand` on a box with the espup toolchain. Part of smol#548.
#
#   tools/build_hand.sh [--print] [name ...]     # default: every row
#
# ── WHY ────────────────────────────────────────────────────────────────────────────────────────
# A hand build is a (chip, tier) pair off the CI matrix's one axis: s3-tapstone-gw is the Tapstone
# gateway on an S3, and the S3 is `builds = false`. test_build_matrix.sh pins what the recipe
# DERIVES to the bench-verified invocation, but until this script nothing compiled it, so the image
# on the arena box was checked only when someone on familiar happened to build it. This is the
# compile, with the same derivation targets/<name>/build.sh uses: no second copy of the recipe.
#
# ── WHAT A GREEN RUN MEANS ─────────────────────────────────────────────────────────────────────
# Every named row compiled and linked a fresh ELF. It does NOT mean the image works (that is the
# bench, targets/c3-tapstone-gw/bench.py), and on CI it is built with ci_provision.sh's throwaway
# GROUP_KEY, so it is never flashable onto the fleet. Four ways to go green having checked nothing
# are refused, each held by tools/test_build_hand.sh: a typed row list (the rows come from
# `build_matrix.py hand-builds`); a manifest with no rows (the floor: building nothing is a FAIL);
# a missing toolchain; and a cargo that exits 0 but leaves no ELF newer than the build's start.
#
# Env: CARGO (default cargo), CARGO_TARGET_DIR (default rust/clock/target),
#      BUILD_MATRIX_MANIFEST (a fixture manifest, for the suite). SMOL_NODE_ID and friends are
#      passed through untouched; CI sets none, so board.rs's fallback id is used. ESP_LOG defaults
#      to `info`, as in targets/*/build.sh.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATE="$ROOT/rust/clock"
BM=("$ROOT/tools/build_matrix.py")
[ -n "${BUILD_MATRIX_MANIFEST:-}" ] && BM+=(--manifest "$BUILD_MATRIX_MANIFEST")
CARGO="${CARGO:-cargo}"

print=0
[ "${1:-}" = "--print" ] && { print=1; shift; }

names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  mapfile -t names < <("${BM[@]}" hand-builds)
fi
if [ ${#names[@]} -eq 0 ]; then
  echo "build_hand: the manifest names no [hand_build] rows — refusing to pass having built nothing" >&2
  exit 1
fi

tdir="${CARGO_TARGET_DIR:-$CRATE/target}"
built=0
for name in "${names[@]}"; do
  if ! rec="$("${BM[@]}" hand-build "$name")"; then
    echo "build_hand: $name: no recipe (see the error above)" >&2; exit 1
  fi
  # Tab-separated with `-` for empty (build_matrix.py's sentinel rule; a tab is IFS whitespace).
  IFS=$'\t' read -r _chip target toolchain build_std opt_level features <<< "$rec"
  envs=()
  [ "$build_std" != "-" ] && envs+=("CARGO_UNSTABLE_BUILD_STD=$build_std")
  [ "$opt_level" != "-" ] && envs+=("CARGO_PROFILE_RELEASE_OPT_LEVEL=$opt_level")
  # ESP_LOG is baked in at compile time, and a hand build is the image that gets FLASHED:
  # targets/*/build.sh default it to `info` (the gateway's ordinary logs stay on the port), and the
  # bench passed on that. Without it the image is ~210 KB smaller (measured, s3-tapstone-gw: 1,463,684
  # vs 1,678,364 B), i.e. CI would compile a build nobody runs.
  envs+=("ESP_LOG=${ESP_LOG:-info}")
  tc=(); [ "$toolchain" != "-" ] && tc=("+$toolchain")
  args=(build --release --no-default-features --features "$features" --target "$target")

  if [ "$print" = 1 ]; then
    echo "$name: ${envs[*]} cargo ${tc[*]} ${args[*]}" | tr -s ' '
    continue
  fi

  # rust-toolchain.toml resolves by DIRECTORY: an xtensa build from the repo root silently gets
  # `stable` (tools/check_chips.sh). So every cargo here runs in the crate.
  cd "$CRATE" || exit 1
  if [ ${#tc[@]} -gt 0 ] && ! "$CARGO" "${tc[@]}" --version >/dev/null 2>&1; then
    echo "build_hand: $name: toolchain ${tc[*]} is not installed (espup; . ~/export-esp.sh)" >&2; exit 1
  fi
  stamp="$(mktemp "${TMPDIR:-/var/tmp}/build_hand.XXXXXX")"
  echo "── build_hand: $name — ${envs[*]} cargo ${tc[*]} ${args[*]}"
  if ! env "${envs[@]}" "$CARGO" "${tc[@]}" "${args[@]}"; then
    rm -f "$stamp"; echo "build_hand: $name: cargo failed" >&2; exit 1
  fi
  elf="$tdir/$target/release/clock"
  if [ ! -f "$elf" ] || [ ! "$elf" -nt "$stamp" ]; then
    rm -f "$stamp"; echo "build_hand: $name: no ELF at $elf newer than this build's start" >&2; exit 1
  fi
  rm -f "$stamp"
  echo "   $name: $(stat -c%s "$elf") B  $elf"
  built=$((built + 1))
done

[ "$print" = 1 ] && exit 0
echo "build_hand: $built hand build(s) built"
[ "$built" -eq ${#names[@]} ]
