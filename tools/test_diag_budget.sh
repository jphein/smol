#!/usr/bin/env bash
# test_diag_budget.sh — proves tools/check_diag_budget.py can FAIL (#306).
#
# The bug that motivated the checker was a bound that drifted without anything noticing: the
# positional term read 220 against a type-provable 228, and three different margins (51, 23, the
# real one) were in prose in one file. A checker that only ever passes would have been the same
# failure wearing a green badge — so every arm below MUTATES a copy of mode.rs and asserts the
# checker goes red for the right reason, plus a clean baseline that must stay green.
#
# SAFETY: every arm works on a copy in mktemp. No git command, no write inside the repo.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHK="$ROOT/tools/check_diag_budget.py"
SRC="$ROOT/rust/clock/src/net/mode.rs"
pass=0; fail=0
ok(){ pass=$((pass+1)); echo "ok   - $1"; }
no(){ fail=$((fail+1)); echo "FAIL - $1"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# arm <name> <want-rc> <want-substring> <sed-script...>
arm() {
  local name="$1" want_rc="$2" want="$3"; shift 3
  cp "$SRC" "$work/m.rs"
  for s in "$@"; do python3 - "$work/m.rs" "$s" <<'PY'
import sys
path, spec = sys.argv[1], sys.argv[2]
old, new = spec.split("\t")
s = open(path).read()
if old not in s:
    sys.exit(f"fixture setup failed: {old!r} not in mode.rs — this test needs updating")
open(path, "w").write(s.replace(old, new, 1))
PY
    [ $? -eq 0 ] || { no "$name (fixture setup)"; return; }
  done
  local out rc
  out="$("$CHK" "$work/m.rs" 2>&1)"; rc=$?
  if [ "$rc" != "$want_rc" ]; then no "$name: rc $rc, want $want_rc — $out"; return; fi
  case "$out" in *"$want"*) ok "$name (rc=$rc)" ;; *) no "$name: rc right, wrong reason — $out" ;; esac
}

echo "== baseline: the real file must be consistent =="
out="$("$CHK" "$SRC" 2>&1)"; rc=$?
if [ "$rc" = 0 ]; then ok "unmodified mode.rs: $out"; else no "unmodified mode.rs FAILS: $out"; fi

echo "== the mutations it must catch =="
# The motivating regression, exactly: a positional field added and not declared.
arm "undeclared new positional field" 1 "UNDECLARED: zzz" \
    '|etx={}"	|etx={}|zzz={}"'
# The 2026-08-01 drift itself: a width silently short of its type maximum.
arm "a width short of its type max" 1 "positional value widths sum to" \
    'rst=10	rst=8'
# The declaration losing a field the record still has.
arm "declared field dropped" 1 "UNDECLARED: etx" \
    ' etx=3	 '
# Same fields, wrong order — order matters because the checker pairs by position.
arm "declaration reordered" 1 "different ORDER" \
    'slot=3 rst=10	rst=10 slot=3'
# A placeholder-count mismatch (led={}:{} declared as one width).
arm "placeholder count mismatch" 1 "placeholder" \
    'led=6:3	led=6'
# The literal term left stale after the record's keys change length.
arm "stale literal term" 1 "format-string literal is" \
    'const DIAG_CORE_MAX: usize = 167	const DIAG_CORE_MAX: usize = 168'
# Over the cliff: the whole point of the constant.
# Both halves move together so the ONLY thing that can fail is the budget: +300 on a declared width
# and +300 on the term it sums to. The literal `167 + 237` is the live constant's own text — when it
# changes, this arm reports "fixture setup failed" rather than silently testing nothing. It has
# already done that once, on the #323 bump that took the term 228 -> 237.
arm "core over budget" 1 "does NOT fit" \
    'up=10 heap=10	up=310 heap=10' \
    'usize = 167 + 237	usize = 167 + 537'
# A checker that cannot find its subject must NOT report success.
arm "checker blinded (no format string)" 2 "has gone blind" \
    'let mut rec = alloc::format!	let mut rec = notformat!'

echo "== the DERIVED read-out (#382) =="
# Arms 1-9 above all prove a declared INPUT against the source, and that is precisely why they did
# not stop the drift they were written to stop: `fef377d` (#323) satisfied every one of them and
# still left the read-out stale for three weeks. These arms cover the numbers a HUMAN reads.
#
# The overstatement direction is the dangerous one and gets its own assertion. An understated margin
# makes a legitimate field look unaffordable (annoying, and it nearly sank #323). An OVERstated one
# hands a designer room that does not exist — #471's 30 B three-way split "fits" in the advertised
# 31 and puts the core 8 B past the cliff, silencing a healthy fleet. So the checker must not merely
# go red; it must say WHICH WAY it is wrong.
arm "read-out overstates the margin" 1 "OVERSTATES the margin by 92 B" \
    'budget=495 margin=7	budget=495 margin=99'
# The understatement direction must also be caught — it is safe for the cliff, not free for design.
arm "read-out understates the margin" 1 "margin: doc says 1, derived" \
    'budget=495 margin=7	budget=495 margin=1'
# Deleting the line must be FATAL, not a silent pass. A check you can switch off by removing its
# input is not a check; it is a suggestion. (Tag mangled rather than the line removed, so the arm
# tests the checker's handling and not sed's.)
arm "read-out declaration removed" 2 "no DIAG-DERIVED" \
    'DIAG-DERIVED:	DIAG-DERIVEDX:'
# A PARTIAL read-out is the realistic sloppy edit: update the terms you touched, leave the rest.
# That is how half a stale declaration survives an otherwise honest correction.
arm "read-out missing a term" 2 "is missing margin" \
    ' margin=7	 '
# Garbage must not parse as agreement.
arm "read-out term malformed" 2 "malformed or unknown term" \
    'margin=7	margin=twenty-two'

echo "== the PROTECTED tail (unconditional push_str) =="
# The documented hazard, verbatim: "a field appended with a bare push_str and NOT counted here
# defeats this whole mechanism". Adding one must now be impossible to do quietly.
arm "undeclared protected append" 1 "UNDECLARED: zzz" \
    'rec.push_str(&alloc::format!("|apch={}"	rec.push_str("|zzz=x");
        rec.push_str(&alloc::format!("|apch={}"'
# The over-count direction — safe for the cliff, but it is how 16 B went missing and nearly made
# #323 look unaffordable. An over-count must be as loud as an under-count.
arm "tail term over-counted" 1 "protected-tail widths sum to" \
    'DIAG-TAIL: mo=43	DIAG-TAIL: mo=59'
# A protected field deleted from the record but left in the declaration.
arm "declared tail field vanished" 1 "no longer appended: brst" \
    'rec.push_str(&alloc::format!(
                "|brst={}:{}:{}{}"	rec.push_str(&alloc::format!(
                "|xbrst={}:{}:{}{}"'
# Blind in the other direction: the tail region delimiters gone.
arm "checker blinded (no tail region)" 2 "could not delimit" \
    'PROTECTED tail: appended unconditionally	PROTECTED tail: appended conditionally'

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
