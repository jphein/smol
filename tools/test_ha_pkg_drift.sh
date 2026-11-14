#!/usr/bin/env bash
# test_ha_pkg_drift.sh — prove `ha_deploy.sh packages` (#575) can fail, in every arm, without the HA VM.
#
# WHY. #575: smol_mesh.yaml and smol_telemetry.yaml sat committed-but-undeployed for weeks because the
# only drift report (`status`) always exits 0. `packages` is the exit-coded check; a check whose red
# has never been seen is indistinguishable from one that is always green, so each arm is observed.
#
# HOW. HA_SSH=local + HA_REMOTE_DIR point the script's remote calls at a scratch copy of the repo's
# packages (READ-ONLY on the repo; nothing is sent anywhere). Scratch lives under $TMPDIR / ~/.cache,
# never /tmp (this repo's hookify rule), and is removed on exit.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 2
SH=./tools/ha_deploy.sh
base="${RUNNER_TEMP:-${TMPDIR:-$HOME/.cache}}"; mkdir -p "$base"
live="$(mktemp -d -p "$base" ha-pkg-drift.XXXXXX)"; trap 'rm -rf "$live"' EXIT
pass=0; fail=0
run() { HA_SSH=local HA_REMOTE_DIR="$1" "$SH" packages >"$live.out" 2>&1; echo $?; }
expect() { # $1 name, $2 want rc, $3 got rc, $4 optional grep the output must contain
  if [ "$3" = "$2" ] && { [ -z "${4:-}" ] || grep -q -- "$4" "$live.out"; }; then
    pass=$((pass+1)); printf '  ok   %s\n' "$1"
  else
    fail=$((fail+1)); printf '  FAIL %s (rc %s, want %s)\n' "$1" "$3" "$2"; sed 's/^/       /' "$live.out"
  fi
}
reset() { rm -f "$live"/*.yaml; cp ha/packages/*.yaml "$live/"; }

reset;                                     expect "identical copy is in sync"         0 "$(run "$live")"
reset; echo "# live edit" >> "$live/smol_mesh.yaml"
                                           expect "a live-only line is drift"         1 "$(run "$live")" "smol_mesh.yaml *DIFFERS"
reset; sed -i '1d' "$live/smol_telemetry.yaml"
                                           expect "a repo-only line is drift"         1 "$(run "$live")" "smol_telemetry.yaml *DIFFERS"
reset; rm "$live/smol_bard.yaml";          expect "a package missing on live"         1 "$(run "$live")" "smol_bard.yaml *MISSING"
reset; echo "x: 1" > "$live/smol_extra.yaml"
                                           expect "a live-only package"               1 "$(run "$live")" "LIVE-ONLY smol_extra.yaml"
                                           expect "an unreachable live dir is 2, never 'in sync'" 2 "$(run "$live/nope")"
rm -f "$live.out"
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
