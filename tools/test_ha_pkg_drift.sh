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
# Arms added after the post-merge review of #576: each was a way to return 0 without having checked.
reset; echo "x: 1" > "$live/other.yaml"
                                           expect "a live-only package not named smol_*"  1 "$(run "$live")" "LIVE-ONLY other.yaml"
reset; : > "$live/smol_bard.yaml"; empty="$(mktemp -d -p "$base" ha-pkg-empty.XXXXXX)"; cp "$live"/*.yaml "$empty/"
       : > "$empty/smol_bard.yaml"
                                           expect "two identical empty files are in sync (not MISSING)" 0 "$(HA_LOCAL_DIR=$empty run "$live")"
rm -rf "$empty"
nopk="$(mktemp -d -p "$base" ha-pkg-none.XXXXXX)"
reset;                                     expect "an empty repo package list is 2, never 'in sync'" 2 "$(HA_LOCAL_DIR=$nopk run "$live")"
rm -rf "$nopk"
reset; echo "x: 1" > "$live/.hidden.yaml";  expect "a hidden live-only package is drift"   1 "$(run "$live")" "LIVE-ONLY .hidden.yaml"
reset;                                     expect "a nonexistent repo package dir is 2"  2 "$(HA_LOCAL_DIR=$live/nowhere run "$live")"
sp="$(mktemp -d -p "$base" "ha pkg sp.XXXXXX")"; cp ha/packages/*.yaml "$sp/"
                                           expect "a live dir path with a space is in sync" 0 "$(run "$sp")"
rm -rf "$sp"
ap="$(mktemp -d -p "$base" "ha-pkg-it's.XXXXXX")"; cp ha/packages/*.yaml "$ap/"
                                           expect "a live dir path with an apostrophe is in sync" 0 "$(run "$ap")"
rm -rf "$ap"
nl="$(mktemp -d -p "$base" ha-pkg-nl.XXXXXX)"; cp ha/packages/*.yaml "$nl/"; cp "$nl/smol_bard.yaml" "$nl/a.yaml
smol_bard.yaml"
reset;                                     expect "a package name with a newline is refused (2)" 2 "$(HA_LOCAL_DIR=$nl run "$live")"
rm -rf "$nl"
reset;                                     expect "a failed scratch mktemp is 2"         2 "$(TMPDIR=$live/no-such-tmp run "$live")"
if [ "$(id -u)" != 0 ]; then
  reset; chmod 000 "$live/smol_mesh.yaml";  expect "an unreadable live file is 2, not drift"  2 "$(run "$live")"
  chmod 644 "$live/smol_mesh.yaml"
  reset; chmod 111 "$live";                 expect "a live dir that exists but cannot be listed is 2" 2 "$(run "$live")"
  chmod 755 "$live"
  lo="$(mktemp -d -p "$base" ha-pkg-lo.XXXXXX)"; cp ha/packages/*.yaml "$lo/"; chmod 000 "$lo/smol_bard.yaml"
  reset;                                   expect "an unreadable repo file is 2, not drift"  2 "$(HA_LOCAL_DIR=$lo run "$live")"
  chmod 644 "$lo/smol_bard.yaml"; rm -rf "$lo"
else
  echo "  skip the three permission arms (running as root)"
fi
rm -f "$live.out"
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
