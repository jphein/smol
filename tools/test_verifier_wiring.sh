#!/usr/bin/env bash
# #367 host half — proves every arm of check_verifier_wiring.py CAN FAIL.
#
# A check nobody has watched fail is a claim, not a guard. This runs the real script against
# deliberately broken copies of the tree and asserts the exit codes.
#
# ⚠️ SAFETY: this NEVER mutates the working tree. Every case operates on a fresh `cp -r` into a
# mktemp dir, and the only writes are inside it. That is not defensive styling — this repo has
# already lost ~200 lines of uncommitted work to a self-test that ran a repo-wide `git reset
# --hard` from an EXIT trap. A test harness that edits the tree it is testing is a loaded gun
# regardless of how careful its author was.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECK="$ROOT/tools/check_verifier_wiring.py"
PASS=0; FAIL=0

# Copy only what the check reads, so a case is fast and cannot reach anything else.
mkcopy() {
  local d; d="$(mktemp -d)"
  mkdir -p "$d/rust/clock" "$d/experiments" "$d/tools"
  cp -r "$ROOT/rust/clock/src" "$d/rust/clock/src"
  cp -r "$ROOT/experiments/." "$d/experiments/"
  cp "$CHECK" "$d/tools/"
  printf '%s' "$d"
}

case_run() { # <name> <want-exit> <mutator-fn>
  local name="$1" want="$2" fn="$3" d out rc
  d="$(mkcopy)"
  "$fn" "$d"
  out="$(python3 "$d/tools/check_verifier_wiring.py" "$d" 2>&1)"; rc=$?
  rm -rf "$d"
  if [ "$rc" -eq "$want" ]; then
    printf '  \033[32mPASS\033[0m %s (exit %d)\n' "$name" "$rc"; PASS=$((PASS+1))
  else
    printf '  \033[31mFAIL\033[0m %s — wanted exit %d, got %d\n' "$name" "$want" "$rc"
    printf '%s\n' "$out" | sed 's/^/        /'; FAIL=$((FAIL+1))
  fi
}

noop() { :; }

# A new phantom: drop a module declaration that a verifier includes. This is the exact shape of
# the #185 defect, reproduced on a module that is currently wired.
drop_decl() { sed -i 's/^pub mod etx;/\/\/ removed by test/' "$1/rust/clock/src/net.rs"; }

# A stale allowlist entry: something listed as a known phantom that is in fact wired.
stale_entry() {
  python3 - "$1" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1]) / "tools" / "check_verifier_wiring.py"
t = p.read_text()
p.write_text(t.replace('KNOWN_PHANTOMS = {', 'KNOWN_PHANTOMS = {\n    "rust/clock/src/net/etx.rs": "#000 — deliberately stale test entry",', 1))
PY
}

# A verifier pointing at a file that does not exist — a rename that missed the harness.
dangling_include() { rm -f "$1/rust/clock/src/net/etx.rs"; }

echo "#367 verifier-wiring check — proving each arm can fail"
case_run "clean tree passes (crdt tracked)"        0 noop
case_run "a NEW phantom fails"                     1 drop_decl
case_run "a STALE allowlist entry fails"           1 stale_entry
case_run "a dangling #[path] target is an error"   2 dangling_include

echo
if [ "$FAIL" -eq 0 ]; then
  echo "test_verifier_wiring: OK — $PASS/$((PASS+FAIL)) arms proven able to fail"
  exit 0
fi
echo "test_verifier_wiring: $FAIL of $((PASS+FAIL)) arms did NOT behave as specified"
exit 1
