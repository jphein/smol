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
live="$(mktemp -d -p "$base" ha-pkg-drift.XXXXXX)"; live2=""; stub=""
# no `set -e` here, so a failed mktemp would leave $live empty and reset() would aim rm/cp at "/" (xreview #576 r1 #3)
[ -n "$live" ] && [ -d "$live" ] || { echo "could not make a scratch dir under $base" >&2; exit 2; }
trap 'rm -rf "$live" "$live.out"; [ -n "$live2" ] && rm -rf "$live2"; [ -n "$stub" ] && rm -rf "$stub"' EXIT
pass=0; fail=0
run() { HA_SSH=local HA_REMOTE_DIR="$1" "$SH" packages >"$live.out" 2>&1; echo $?; }
runp() { PATH="$2:$PATH" HA_SSH=local HA_REMOTE_DIR="$1" "$SH" packages >"$live.out" 2>&1; echo $?; }   # with a stub dir first on PATH
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
reset; cp "$live/smol_bard.yaml" "$live/junk
smol_bard.yaml";                           expect "a live file name with a newline is refused (2)" 2 "$(run "$live")"
reset; cp "$live/smol_bard.yaml" "$live/zzzz
";                                         expect "a live name ending in a newline is refused (2)" 2 "$(run "$live")"
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
# Fault injection (xreview backfill of #580, 2026-11-16): the live-side stages must fail closed one by one. A fresh
# live copy (earlier arms leave permission/newline fixtures behind) and stubs first on PATH that fail ONLY on the live
# side: `find` when run from the live dir (the remote enumeration cd's there; the repo listing does not), `cat` on a
# live path, `cmp` always (the only cmp is the live comparison). `sh` here is /bin/sh (dash on Ubuntu: no pipefail).
live2="$(mktemp -d -p "$base" ha-pkg-fault.XXXXXX)"; cp ha/packages/*.yaml "$live2/"
stub="$(mktemp -d -p "$base" ha-pkg-stub.XXXXXX)"
# the live find prints the complete listing and THEN fails (a stat error on an entry): the count matches the listing,
# only find's status says the enumeration failed; a pipeline would have hidden it behind wc's 0
printf '#!/bin/sh\nif [ "$PWD" = "%s" ]; then /usr/bin/find "$@"; exit 1; fi\nexec /usr/bin/find "$@"\n' "$live2" > "$stub/find"; chmod 700 "$stub/find"
                                           expect "a live find that lists everything and then fails is 2, never 'in sync'"  2 "$(runp "$live2" "$stub")" "could not count"
rm -f "$stub/find"; printf '#!/bin/sh\ncase "$1" in "%s"/*) exit 1;; esac\nexec /bin/cat "$@"\n' "$live2" > "$stub/cat"; chmod 700 "$stub/cat"
                                           expect "a failing fetch (cat) of a live file is 2"             2 "$(runp "$live2" "$stub")" "could not read"
rm -f "$stub/cat"; printf '#!/bin/sh\nexit 3\n' > "$stub/cmp"; chmod 700 "$stub/cmp"
                                           expect "a comparison error (cmp exit 3) is 2, never drift or in sync" 2 "$(runp "$live2" "$stub")" "could not compare"
# a tr that fails after find succeeded (a read error on the scratch file): still 2, the stage is not hidden behind wc
rm -f "$stub/cmp"; printf '#!/bin/sh\nexit 1\n' > "$stub/tr"; chmod 700 "$stub/tr"
                                           expect "a failing tr in the live count is 2 (no pipeline hides it)"  2 "$(runp "$live2" "$stub")" "could not count"
# mktemp failing on the live side: 2, and an unrelated file named .c beside the packages survives (nothing is removed
# that was not made)
rm -f "$stub/tr"; printf '#!/bin/sh\n[ "$PWD" = "%s" ] && exit 1\nexec /usr/bin/mktemp "$@"\n' "$live2" > "$stub/mktemp"; chmod 700 "$stub/mktemp"; printf 'keep\n' > "$live2/.c"
                                           expect "a failing mktemp on the live side is 2"  2 "$(runp "$live2" "$stub")" "could not count"
[ "$(cat "$live2/.c" 2>/dev/null)" = keep ] && { pass=$((pass+1)); printf '  ok   %s\n' "a stray .c beside the packages survives a failed count"; } || { fail=$((fail+1)); printf '  FAIL %s\n' "the stray .c was removed or changed"; }
rm -f "$stub/mktemp" "$live2/.c"
# the last stage: a wc that prints a plausible count and then fails (live side only) is 2, never a count
printf '#!/bin/sh\nif [ "$PWD" = "%s" ]; then echo 4; exit 1; fi\nexec /usr/bin/wc "$@"\n' "$live2" > "$stub/wc"; chmod 700 "$stub/wc"
                                           expect "a wc that prints a count and then fails is 2"  2 "$(runp "$live2" "$stub")" "could not count"
rm -f "$stub/wc"
printf '\n%d passed, %d failed\n' "$pass" "$fail"; [ "$fail" = 0 ]
