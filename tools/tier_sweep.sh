#!/usr/bin/env bash
# tier_sweep.sh — `cargo check` + `cargo clippy -D warnings` across the feature tiers, fast.
#
# WHY THIS EXISTS. On 2026-08-27 a change under `rust/clock/src/net/**` went to CI having been
# built exactly once, with `--features espnow,cast,io`, and verified with `tools/gate.sh host`.
# CI came back red on TWELVE tiers for two independent reasons:
#
#   * `net::wire` is `espnow`-gated (`net.rs:97`) while `RelayCache` is a `wifi` item, so a
#     wifi-only build had no `wire` and three call sites failed with E0433;
#   * clippy's `manual_try_fold` refused a `fold(Some(0), |acc, b| acc?…)` under `-D warnings`.
#
# NEITHER WAS REACHABLE FROM WHAT I RAN. `gate.sh host` does not build the firmware tiers and does
# not run clippy on them — that is `gate.sh fw`, which needs a toolchain run. With the build host
# down, the fw arm had been CI-only, so the tier matrix was a gap I was carrying WITHOUT HAVING
# NAMED IT: I had explicitly written down that host-green is not proof of on-device behaviour, and
# had not noticed it was also not proof of compilation.
#
# The fix was written as a sentence in a commit message — "when the build host is unavailable, run
# a local tier sweep first". That is the artifact class this repo keeps getting burned by: the
# thing carrying the intent is not the thing anyone consults. So it is a script.
#
# WHAT IT IS NOT. Not a replacement for `gate.sh fw`, which also does the stack floor, the paint
# warrant, symbol sizes, byte-freeness and identity honesty — none of which this touches. This
# answers exactly one question, the cheap one that was missed: DOES EVERY TIER STILL COMPILE AND
# LINT? Roughly two minutes against `gate.sh fw`'s many.
#
# Usage:
#   tools/tier_sweep.sh                 # the four esp32c3 tiers, check + clippy
#   tools/tier_sweep.sh --check-only    # skip clippy (faster; catches E0433-class breaks only)
#   SWEEP_CHIP=esp32c6 tools/tier_sweep.sh
#
# Exit 0 all green · 1 a tier failed · 2 the sweep could not run.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLOCK="$ROOT/rust/clock"
CHIP="${SWEEP_CHIP:-esp32c3}"
CHECK_ONLY=0
[ "${1:-}" = "--check-only" ] && CHECK_ONLY=1

# The tiers are CUMULATIVE, which is the point: each adds one feature layer, so a break is
# attributable to the layer that introduced it rather than to "the fleet build". The names match
# the ones `gate.sh fw` prints, so a red line here and a red line there are the same word.
TIERS=(
  "hw:default (no radio)"
  "hw,wifi:wifi"
  "hw,wifi,espnow:espnow"
  "hw,wifi,espnow,cast,io:fleet"
)

command -v cargo >/dev/null 2>&1 || { echo "FATAL: cargo not on PATH (try PATH=\$HOME/.cargo/bin:\$PATH)" >&2; exit 2; }
[ -d "$CLOCK" ] || { echo "FATAL: $CLOCK not found — is this the smol repo?" >&2; exit 2; }

# board.rs / secrets.rs are git-ignored and provisioned per tree; without them EVERY tier fails
# identically and the sweep reports a dozen breaks that are really one missing file. Provision
# first, and say so if that is what failed, rather than letting it masquerade as a code error.
if [ ! -f "$CLOCK/src/board.rs" ] || [ ! -f "$CLOCK/src/secrets.rs" ]; then
  if ! "$ROOT/tools/ci_provision.sh" >/dev/null 2>&1; then
    echo "FATAL: ci_provision.sh failed — board.rs/secrets.rs are absent, so every tier would fail" >&2
    echo "       for that reason and not for yours. Fix provisioning before reading a sweep." >&2
    exit 2
  fi
fi

fails=0
run() { # <label> <phase> <cmd...>
  local label="$1" phase="$2"; shift 2
  local out rc
  out="$( (cd "$CLOCK" && "$@") 2>&1 )"; rc=$?
  if [ "$rc" -eq 0 ]; then
    printf '   \033[32mok\033[0m   %-8s %s\n' "$phase" "$label"
  else
    fails=$((fails + 1))
    printf '   \033[31mFAIL\033[0m %-8s %s\n' "$phase" "$label"
    # First error only. A tier that fails to compile emits a wall, and the first error is almost
    # always the cause while the rest are its consequences.
    printf '%s\n' "$out" | grep -E '^(error|warning: unused)' | head -2 | sed 's/^/          /'
    printf '%s\n' "$out" | grep -E '^\s+--> ' | head -1 | sed 's/^/          /'
  fi
}

echo "tier sweep — chip=$CHIP $([ "$CHECK_ONLY" = 1 ] && echo '(check only)')"
for entry in "${TIERS[@]}"; do
  feats="${entry%%:*}"; label="${entry#*:}"
  run "$label" "check" cargo check --release --no-default-features --features "$CHIP,$feats"
  [ "$CHECK_ONLY" = 1 ] && continue
  # `-D warnings` because that is what CI uses; a sweep that is more permissive than the gate it
  # stands in for would hand back a green it cannot honour.
  run "$label" "clippy" cargo clippy --release --no-default-features --features "$CHIP,$feats" -- -D warnings
done

echo
if [ "$fails" -eq 0 ]; then
  echo "tier sweep: all tiers green (chip=$CHIP)."
  echo "⚠ This is COMPILE+LINT only. It says nothing about the stack floor, symbol sizes,"
  echo "  byte-freeness, or anything on-device — run tools/gate.sh fw for those."
  exit 0
fi
echo "tier sweep: $fails failure(s) — fix before pushing; CI checks the same tiers."
exit 1
