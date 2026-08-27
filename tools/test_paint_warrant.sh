#!/usr/bin/env bash
# test_paint_warrant.sh — proves tools/check_paint_warrant.py can FAIL (#438/#335).
#
# The arm it tests exists because a PROSE warrant rotted silently: #438 asserted the paint tier's
# `.stack` was byte-identical to the fleet tier's, that stopped being true under STEP T, and nothing
# noticed because nothing was checking. A replacement check that only ever passes would be the same
# failure wearing a green badge — so every arm below drives the checker into a specific wrong state
# and asserts it goes red FOR THE RIGHT REASON, not merely red.
#
# SAFETY: operates only on synthetic `readelf -sW` fixtures under mktemp, via the checker's
# documented --*-syms test seam. It never reads a real ELF and never writes inside the repo.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHK="$ROOT/tools/check_paint_warrant.py"
pass=0; fail=0
ok(){ pass=$((pass+1)); echo "ok   - $1"; }
no(){ fail=$((fail+1)); echo "FAIL - $1"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A minimal `readelf -sW`-shaped fixture. Columns: Num: Value Size Type Bind Vis Ndx Name
# `_stack_start` and `_stack_end` are what the checker reads; the decoys exist so a sloppy
# substring match would pick the wrong line and this suite would catch it.
syms() { # syms <file> <start-hex> <end-hex>
  cat > "$1" <<EOF
Symbol table '.symtab' contains 4 entries:
   Num:    Value  Size Type    Bind   Vis      Ndx Name
     1: $2     0 NOTYPE  GLOBAL DEFAULT  ABS $START_NAME
     2: $3     0 NOTYPE  GLOBAL DEFAULT  ABS $END_NAME
     3: 3fc00000  1024 OBJECT  LOCAL  DEFAULT    7 _stack_start_decoy_object
     4: 40000c58     0 NOTYPE  GLOBAL DEFAULT  ABS r_co_list_pool_init
EOF
}
START_NAME=_stack_start
END_NAME=_stack_end

run() { "$CHK" --fleet-syms "$work/fleet.txt" --paint-syms "$work/paint.txt" 2>&1; }

echo "== baseline: the REAL post-T pair must pass (paint tighter = conservative) =="
# fleet 73,112 = 0x3fcce400-0x3fcbc668 · paint 71,976 = 0x3fcce400-0x3fcbcad8 — measured 2026-08-27
syms "$work/fleet.txt" 3fcce400 3fcbc668
syms "$work/paint.txt" 3fcce400 3fcbcad8
out="$(run)"; rc=$?
if [ "$rc" = 0 ]; then
  case "$out" in *"paint is 1136 B tighter"*) ok "post-T pair passes with the right delta" ;;
    *) no "post-T pair passed but the delta is wrong — $out" ;; esac
else no "post-T pair FAILED (rc=$rc) — $out"; fi

# ⚠️ THIS ARM IS INVERTED from the original suite, and the inversion is the lesson.
# Byte-identity was #438's WARRANT and is now a FAILURE: a paint build costing zero region is the
# signature of one built without ESP_LOG, i.e. with the report line compiled out. On 2026-08-27 two
# such images shipped to a bench handoff and passed md5, region, symbol-count and seed checks.
echo "== INVERTED: a byte-identical pair is now RED (an instrument that costs nothing is absent) =="
syms "$work/fleet.txt" 3fcce400 3fcb9348
syms "$work/paint.txt" 3fcce400 3fcb9348
out="$(run)"; rc=$?
if [ "$rc" != 1 ]; then no "byte-identical: rc $rc, want 1 — $out"
else case "$out" in *"costs NOTHING"*) ok "byte-identical pair is caught (rc=1)" ;;
  *) no "byte-identical: rc right, WRONG REASON — $out" ;; esac; fi

echo "== the sanity band's UPPER bound: an implausibly large delta is RED =="
syms "$work/fleet.txt" 3fcce400 3fcb9348
syms "$work/paint.txt" 3fcce400 3fcc9348   # region 20,664 = 65,536 B TIGHTER (end RAISED, not lowered)
out="$(run)"; rc=$?
if [ "$rc" != 1 ]; then no "huge delta: rc $rc, want 1 — $out"
else case "$out" in *"far more region than the instrument explains"*) ok "over-band delta is caught (rc=1)" ;;
  *) no "huge delta: rc right, WRONG REASON — $out" ;; esac; fi

echo "== THE ARM: paint with MORE room than the shipped image must be RED =="
# This is the whole point. If the instrument is roomier than the fleet image, a soak can pass a
# depth the shipped image cannot hold — a green read off easier-than-shipping conditions.
syms "$work/fleet.txt" 3fcce400 3fcbcad8   # fleet 71,976
syms "$work/paint.txt" 3fcce400 3fcbc668   # paint 73,112  <- roomier
out="$(run)"; rc=$?
if [ "$rc" != 1 ]; then no "roomier paint: rc $rc, want 1 — $out"
else case "$out" in *"MORE stack room"*) ok "roomier paint is caught (rc=1)" ;;
  *) no "roomier paint: rc right, WRONG REASON — $out" ;; esac; fi

echo "== THE ARM: even ONE byte of extra room is caught (no silent tolerance) =="
syms "$work/fleet.txt" 3fcce400 3fcbcad8
syms "$work/paint.txt" 3fcce400 3fcbcad7   # exactly 1 B roomier
out="$(run)"; rc=$?
if [ "$rc" = 1 ]; then ok "1 B of extra room is caught (rc=1)"
else no "1 B roomier: rc $rc, want 1 — a silent tolerance is how the prose warrant rotted"; fi

echo "== fail-closed: a renamed/absent boundary symbol must NOT read as success =="
syms "$work/fleet.txt" 3fcce400 3fcbc668
START_NAME=_stack_beginning   # the rename this arm must survive by failing, not by passing
syms "$work/paint.txt" 3fcce400 3fcbcad8
START_NAME=_stack_start
out="$(run)"; rc=$?
if [ "$rc" != 2 ]; then no "missing symbol: rc $rc, want 2 — $out"
else case "$out" in *"is ABSENT from the symbol table"*) ok "absent boundary symbol fails CLOSED (rc=2)" ;;
  *) no "missing symbol: rc right, WRONG REASON — $out" ;; esac; fi

echo "== fail-closed: a nonsense region (symbols swapped) must NOT read as success =="
syms "$work/fleet.txt" 3fcbc668 3fcce400   # start below end -> negative region
syms "$work/paint.txt" 3fcce400 3fcbcad8
out="$(run)"; rc=$?
if [ "$rc" != 2 ]; then no "swapped symbols: rc $rc, want 2 — $out"
else case "$out" in *"plausibility floor"*) ok "swapped symbols fail CLOSED (rc=2)" ;;
  *) no "swapped symbols: rc right, WRONG REASON — $out" ;; esac; fi

# An unreadable input must exit 2 (BLIND), never 1 (VIOLATED). Asserting the exact code, not merely
# "non-zero": the first draft of the checker let open() raise, which exited 1 — so a wrong path
# would have been reported by CI as "the paint image is roomier than the shipped image", a real and
# alarming finding that never happened. Sharing an exit code IS sharing a name.
echo "== fail-closed: an unreadable input is BLIND (rc=2), not VIOLATED (rc=1) =="
syms "$work/paint.txt" 3fcce400 3fcbcad8
out="$("$CHK" --fleet-syms "$work/does-not-exist.txt" --paint-syms "$work/paint.txt" 2>&1)"; rc=$?
if [ "$rc" != 2 ]; then no "missing file: rc $rc, want 2 (1 would impersonate a violation) — $out"
else case "$out" in *"cannot read"*) ok "unreadable input fails CLOSED as BLIND (rc=2)" ;;
  *) no "missing file: rc right, WRONG REASON — $out" ;; esac; fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ] || exit 1
