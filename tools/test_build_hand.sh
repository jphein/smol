#!/usr/bin/env bash
# test_build_hand.sh — prove `tools/build_hand.sh` (the CI arm that COMPILES every [hand_build] row)
# can fail, one way at a time, with no cargo, no toolchain and no hardware. Part of smol#548.
#
# ── WHY ────────────────────────────────────────────────────────────────────────────────────────
# `test_build_matrix.sh` pins what `hand-build s3-tapstone-gw` DERIVES to the bench-verified recipe.
# Nothing compiled that recipe: the S3 is `builds = false`, so the gateway on the arena box was a
# build that only a human on familiar ever ran. build_hand.sh is the job that runs it on CI. A job
# like that can go blind four ways, and each case below plants one:
#   1. the list of rows it builds is typed rather than read from the manifest — it would miss a row;
#   2. the command it runs is not the derived recipe (a dropped env word is the classic, see
#      targets/s3-tapstone-gw/build.sh's header: a build without build-std or opt=2 is a different
#      image, or none);
#   3. the manifest names no rows and the loop builds nothing and exits 0 (a skip guard with no floor,
#      tapstone docs/verification.md);
#   4. cargo "succeeds" and leaves no ELF, or the toolchain is missing and that reads as a pass.
#
# Exit 0 all cases behaved; 1 otherwise.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
BM="$HERE/build_matrix.py"
BH="$HERE/build_hand.sh"
CASES="$HERE/test_build_matrix_cases"
pass=0; fail=0
note() { printf '   \033[32mok\033[0m   %s\n' "$1"; pass=$((pass + 1)); }
oops() { printf '   \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail + 1)); }

scratch="$(mktemp -d "${TMPDIR:-/var/tmp}/test_build_hand.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

# ── 1. the row list is the manifest's, counted from its own headers ─────────────────────────────
want_rows="$(sed -n 's/^\[hand_build\.\([^]]*\)\]$/\1/p' "$HERE/build-matrix.toml" | sort)"
got_rows="$("$BM" hand-builds 2>&1 | sort)"
if [ -z "$want_rows" ]; then
  oops "hand-builds: the manifest declares no [hand_build.*] row — this suite has nothing to hold"
elif [ "$got_rows" = "$want_rows" ]; then
  note "hand-builds lists every [hand_build.*] header ($(echo $want_rows))"
else
  oops "hand-builds: got $(printf '%q' "$got_rows"), the manifest's headers say $(printf '%q' "$want_rows")"
fi
# A fixture with TWO rows, so a `head -1` in the lister is visible.
two="$scratch/two.toml"
{ cat "$CASES/hand_build_ok.toml"; printf '[hand_build.s3-gw-two]\nchip = "esp32s3"\ntier = "gw"\n'; } > "$two"
got_two="$("$BM" hand-builds --manifest "$two" | sort | paste -sd' ')"
[ "$got_two" = "s3-gw s3-gw-two" ] && note "hand-builds lists both rows of a two-row manifest" \
  || oops "hand-builds on a two-row manifest: got '$got_two'"

# ── 2. the command is exactly the bench recipe (targets/s3-tapstone-gw/README.md, "The recipe") ──
want_cmd="CARGO_UNSTABLE_BUILD_STD=core,alloc CARGO_PROFILE_RELEASE_OPT_LEVEL=2 ESP_LOG=info cargo +esp build --release --no-default-features --features esp32s3,hw,tapstone-gw,espnow,cast,io --target xtensa-esp32s3-none-elf"
got_cmd="$("$BH" --print s3-tapstone-gw 2>&1)"
if [ "$got_cmd" = "s3-tapstone-gw: $want_cmd" ]; then
  note "--print s3-tapstone-gw is the bench recipe, word for word"
else
  oops "--print s3-tapstone-gw: got $(printf '%q' "$got_cmd")"
fi
# With no name it builds EVERY row: one line per row of the real manifest.
n_all="$("$BH" --print 2>/dev/null | grep -c ': .*cargo ')"
n_want="$(printf '%s\n' "$want_rows" | grep -c .)"
[ "$n_all" = "$n_want" ] && note "--print with no name covers all $n_want row(s)" \
  || oops "--print with no name printed $n_all command(s), the manifest has $n_want row(s)"

# ...and the BUILDER reads that list: on the two-row fixture it must emit two commands. With one real
# row, a list typed into build_hand.sh would pass the count above; it cannot pass this.
n_two="$(BUILD_MATRIX_MANIFEST="$two" "$BH" --print 2>&1 | grep -c ': .*cargo ')"
[ "$n_two" = 2 ] && note "--print on the two-row fixture emits 2 commands (the builder reads the manifest)" \
  || oops "--print on the two-row fixture emitted $n_two command(s), want 2"

# ── 3. the floor: nothing to build is a failure, not a pass ─────────────────────────────────────
if out="$(BUILD_MATRIX_MANIFEST="$CASES/three_chips_one_axis.toml" "$BH" --print 2>&1)"; then
  oops "a manifest with no [hand_build] rows exited 0 — built nothing and passed: $out"
else
  case "$out" in *"no [hand_build]"*) note "a manifest with no rows is refused (the floor)" ;;
    *) oops "no-rows refusal, wrong reason: $out" ;; esac
fi
if "$BH" --print no-such-target >/dev/null 2>&1; then
  oops "an unknown name exited 0"
else
  note "an unknown name is refused"
fi

# ── 4. a missing toolchain and a missing ELF are failures ───────────────────────────────────────
# Fake cargos, so the real build path runs end to end without a compiler.
mk() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$scratch/$1"; chmod +x "$scratch/$1"; }
mk cargo-none 'exit 1'                                                     # no `+esp` toolchain
mk cargo-noelf 'exit 0'                                                    # "builds", writes nothing
mk cargo-elf 'for a; do [ "$p" = --target ] && t=$a; p=$a; done
[ "$1" = "+esp" ] && [ "$2" = "--version" ] && exit 0
mkdir -p "$CARGO_TARGET_DIR/$t/release"; : > "$CARGO_TARGET_DIR/$t/release/clock"'
export CARGO_TARGET_DIR="$scratch/target"
if out="$(CARGO="$scratch/cargo-none" "$BH" s3-tapstone-gw 2>&1)"; then
  oops "a missing +esp toolchain exited 0: $out"
else
  case "$out" in *toolchain*) note "a missing toolchain is a failure, named as one" ;;
    *) oops "missing toolchain, wrong reason: $out" ;; esac
fi
# A STALE ELF is planted first: an image left by an earlier build is not this build's output.
stale="$CARGO_TARGET_DIR/xtensa-esp32s3-none-elf/release/clock"
mkdir -p "$(dirname "$stale")"; : > "$stale"; touch -d '1 hour ago' "$stale"
if out="$(CARGO="$scratch/cargo-noelf" "$BH" s3-tapstone-gw 2>&1)"; then
  oops "cargo exit 0 with no ELF exited 0: $out"
else
  case "$out" in *"no ELF"*) note "a build that leaves no fresh ELF is a failure (a stale one does not count)" ;;
    *) oops "no-ELF, wrong reason: $out" ;; esac
fi
# The positive control for case 4: the same script with a cargo that leaves an ELF must PASS, or
# the two refusals above prove only that the script always fails.
if out="$(CARGO="$scratch/cargo-elf" "$BH" s3-tapstone-gw 2>&1)"; then
  case "$out" in *"1 hand build(s) built"*) note "positive control: a build that leaves an ELF passes" ;;
    *) oops "positive control passed without the count line: $out" ;; esac
else
  oops "positive control failed: $out"
fi

printf '\n   %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
