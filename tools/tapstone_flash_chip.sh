#!/usr/bin/env bash
# tapstone_flash_chip.sh <chip> — the vendored tapstone crates' 64 KB flash budget on ONE chip.
#
# tools/gate.sh fw measures it on the canonical chip (the C3) against the ELF its stack arm already
# built. The S3 is where the shrine runs, and it runs heavier: its `opt_level = 2` workaround
# (#398/#409) costs ~32% more .text. On 2026-09-27 at rules-v0.2.2 that was +51,832 B of 65,536 B,
# 13.7 KB of headroom, and only a human on familiar measured it. `tools/gate.sh hand` (CI job
# `hand builds`, which has the Xtensa toolchain) runs this for the S3, so the tighter chip is gated.
#
# It builds TWO images from `build_matrix.py chip-recipe <chip>`, not from a copy of the chip's
# knobs:
#   fleet  the chip's canonical-tier image
#   probe  the same image plus `tapstone,tapstone-probe` (src/tapstone.rs flash_probe)
# Each goes into its own target dir, so neither can reuse or overwrite the other's ELF (the paint
# arm's reason). Then tools/check_tapstone_flash.py compares them: red over the budget, and red
# under its 4 KB floor, where LTO has stripped the probe and the instrument sees nothing.
#
# A build that exits 0 but leaves no fresh ELF fails: a stale ELF would compare two old images and
# pass (build_hand.sh's rule).
#
# Env: CARGO (default cargo), CARGO_TARGET_DIR (the base; default rust/clock/target), and
#      TAPSTONE_FLASH_BUDGET (default: the checker's 65,536). The override exists for the red control.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATE="$ROOT/rust/clock"
CARGO="${CARGO:-cargo}"
chip="${1:?usage: tools/tapstone_flash_chip.sh <chip>}"

rec="$("$ROOT/tools/build_matrix.py" chip-recipe "$chip")" || exit 2
IFS=$'\t' read -r _chip target toolchain build_std opt_level features <<< "$rec"
envs=()
[ "$build_std" != "-" ] && envs+=("CARGO_UNSTABLE_BUILD_STD=$build_std")
[ "$opt_level" != "-" ] && envs+=("CARGO_PROFILE_RELEASE_OPT_LEVEL=$opt_level")
tc=(); [ "$toolchain" != "-" ] && tc=("+$toolchain")
base="${CARGO_TARGET_DIR:-$CRATE/target}"

# rust-toolchain.toml resolves by DIRECTORY: build from the crate (tools/check_chips.sh).
cd "$CRATE" || exit 2
if [ ${#tc[@]} -gt 0 ] && ! "$CARGO" "${tc[@]}" --version >/dev/null 2>&1; then
  echo "tapstone_flash: toolchain ${tc[*]} is not installed (espup; . ~/export-esp.sh)" >&2; exit 2
fi

build() {  # <name> <features> -> prints the ELF path
  local name="$1" feats="$2" dir="$base/tapstone-flash-$chip-$1" stamp elf
  stamp="$(mktemp "${TMPDIR:-/var/tmp}/tapstone_flash.XXXXXX")"
  echo "── $chip $name: ${envs[*]} cargo ${tc[*]} build --release --features $feats" >&2
  if ! env "${envs[@]}" CARGO_TARGET_DIR="$dir" "$CARGO" "${tc[@]}" build --release --bin clock \
         --no-default-features --features "$feats" --target "$target" >&2; then
    rm -f "$stamp"; echo "tapstone_flash: $chip $name: cargo failed" >&2; return 1
  fi
  elf="$dir/$target/release/clock"
  if [ ! -f "$elf" ] || [ ! "$elf" -nt "$stamp" ]; then
    rm -f "$stamp"; echo "tapstone_flash: $chip $name: no ELF newer than this build's start" >&2
    return 1
  fi
  rm -f "$stamp"; echo "$elf"
}

fleet="$(build fleet "$features")" || exit 1
probe="$(build probe "tapstone,tapstone-probe,$features")" || exit 1
args=(--fleet-elf "$fleet" --tapstone-elf "$probe")
[ -n "${TAPSTONE_FLASH_BUDGET:-}" ] && args+=(--budget "$TAPSTONE_FLASH_BUDGET")
echo "── $chip: tools/check_tapstone_flash.py" >&2
"$ROOT/tools/check_tapstone_flash.py" "${args[@]}"
