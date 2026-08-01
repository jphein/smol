#!/usr/bin/env bash
# test_ci_provision.sh — unit tests for the #359 symbol top-up in tools/ci_provision.sh.
#
# The bug being pinned: `ci_provision.sh` used to treat an existing secrets.rs as done. #190 grew
# `secrets.rs.example` by GROUP_KEY/GROUP_KEY_EPOCH, so every pre-#190 worktree kept a file that was
# "present, left untouched" and failed every espnow tier on `cannot find value GROUP_KEY` — while CI
# stayed green. These tests assert the top-up ADDS what the example grew, NEVER overwrites a real
# value, and that --check can actually FAIL (a gate that cannot fail is not a gate).
#
# SAFETY: every case runs in its own mktemp -d. This script runs NO git command and touches nothing
# in the repo but READS tools/ci_provision.sh and the two src/*.rs.example templates.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROV="$ROOT/tools/ci_provision.sh"
EX_SRC="$ROOT/rust/clock/src"
pass=0; fail=0

ok()   { pass=$((pass+1)); echo "  [OK] $1"; }
bad()  { fail=$((fail+1)); echo "  [NO] $1"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 ($2)"; else bad "$1: got '$2' want '$3'"; fi; }

# A throwaway clock dir carrying the REAL templates (so the tests track the live examples).
mkclock() {
  d="$(mktemp -d)"; mkdir -p "$d/src"
  cp "$EX_SRC/secrets.rs.example" "$EX_SRC/board.rs.example" "$d/src/"
  echo "$d"
}

echo "== 1. clean checkout: both files created, GROUP_KEY de-zeroed =="
d="$(mkclock)"
out="$("$PROV" "$d" 2>&1)"; rc=$?
check "exit" "$rc" "0"
case "$out" in *"secrets.rs: created"*) ok "created secrets.rs" ;; *) bad "no create line: $out" ;; esac
case "$out" in *"board.rs: created"*)   ok "created board.rs" ;;   *) bad "no create line: $out" ;; esac
if grep -qE 'GROUP_KEY: *\[u8; *32\] *= *\[0u8; *32\]' "$d/src/secrets.rs"
  then bad "GROUP_KEY still the published all-zero example"; else ok "GROUP_KEY de-zeroed"; fi
"$PROV" --check "$d" >/dev/null 2>&1; check "--check on a fresh tree" "$?" "0"
rm -rf "$d"

echo "== 2. THE #359 BUG: a pre-#190 secrets.rs (no GROUP_KEY) =="
d="$(mkclock)"
# A file exactly like the ones that broke: real values, but predating the #190 symbols.
sed '/#190 group-HMAC/,$d' "$d/src/secrets.rs.example" > "$d/src/secrets.rs"
sed -i 's/YOUR_MQTT_PASSWORD/a-real-looking-local-password/' "$d/src/secrets.rs"
cp "$d/src/board.rs.example" "$d/src/board.rs"
grep -q GROUP_KEY "$d/src/secrets.rs" && { bad "fixture setup: GROUP_KEY should be absent"; exit 1; }

# 2a. --check must FAIL, and must name the missing symbol. This is the proof the guard can fail.
out="$("$PROV" --check "$d" 2>&1)"; rc=$?
check "--check exit on a stale file" "$rc" "3"
case "$out" in *GROUP_KEY*)       ok "--check names GROUP_KEY" ;;       *) bad "no symbol named: $out" ;; esac
case "$out" in *GROUP_KEY_EPOCH*) ok "--check names GROUP_KEY_EPOCH" ;; *) bad "no symbol named: $out" ;; esac
case "$out" in *ci_provision.sh*) ok "--check states the fix" ;;        *) bad "no fix stated: $out" ;; esac
grep -q GROUP_KEY "$d/src/secrets.rs" && bad "--check MUTATED the file" || ok "--check changed nothing"

# 2b. apply tops up, loudly, and the result is complete + compilable-shaped.
out="$("$PROV" "$d" 2>&1)"; rc=$?
check "apply exit" "$rc" "0"
case "$out" in *"MISSING symbols"*GROUP_KEY*) ok "apply reports what it added" ;; *) bad "quiet top-up: $out" ;; esac
grep -q 'pub const GROUP_KEY' "$d/src/secrets.rs"       && ok "GROUP_KEY appended"       || bad "GROUP_KEY absent"
grep -q 'pub const GROUP_KEY_EPOCH' "$d/src/secrets.rs" && ok "GROUP_KEY_EPOCH appended" || bad "EPOCH absent"
grep -q '#\[cfg(feature = "espnow")\]' "$d/src/secrets.rs" && ok "cfg attribute carried" || bad "cfg lost"
if grep -qE 'GROUP_KEY: *\[u8; *32\] *= *\[0u8; *32\]' "$d/src/secrets.rs"
  then bad "appended GROUP_KEY left all-zero (would not compile)"; else ok "appended GROUP_KEY de-zeroed"; fi
grep -q 'a-real-looking-local-password' "$d/src/secrets.rs" && ok "real value NOT clobbered" || bad "real value lost"
check "appended once, not duplicated" "$(grep -c 'pub const GROUP_KEY:' "$d/src/secrets.rs")" "1"
# grep proves the text is there; rustfmt proves the appended block is valid RUST (attributes,
# brackets and the item body all survived the splice). Skipped if rustfmt is absent.
if command -v rustfmt >/dev/null 2>&1; then
  rustfmt --edition 2021 --emit stdout "$d/src/secrets.rs" >/dev/null 2>&1 \
    && ok "topped-up secrets.rs parses as Rust" || bad "topped-up secrets.rs does NOT parse"
fi

# 2c. idempotent: a second run finds nothing missing and --check now passes.
out="$("$PROV" "$d" 2>&1)"
case "$out" in *"secrets.rs: present, complete"*) ok "second run: complete" ;; *) bad "not idempotent: $out" ;; esac
check "second run leaves one GROUP_KEY" "$(grep -c 'pub const GROUP_KEY:' "$d/src/secrets.rs")" "1"
"$PROV" --check "$d" >/dev/null 2>&1; check "--check after top-up" "$?" "0"
rm -rf "$d"

echo "== 3. a complete file is byte-identical after a run =="
d="$(mkclock)"
"$PROV" "$d" >/dev/null 2>&1
before="$(cksum < "$d/src/secrets.rs") $(cksum < "$d/src/board.rs")"
"$PROV" "$d" >/dev/null 2>&1
after="$(cksum < "$d/src/secrets.rs") $(cksum < "$d/src/board.rs")"
check "no rewrite of a complete tree" "$after" "$before"
rm -rf "$d"

echo "== 4. board.rs grows too (not just secrets) =="
d="$(mkclock)"
cp "$d/src/secrets.rs.example" "$d/src/secrets.rs"
grep -v 'DEFAULT_PAGE' "$d/src/board.rs.example" > "$d/src/board.rs"
"$PROV" --check "$d" >/dev/null 2>&1; check "--check flags a stale board.rs" "$?" "3"
"$PROV" "$d" >/dev/null 2>&1
grep -q 'pub const DEFAULT_PAGE' "$d/src/board.rs" && ok "DEFAULT_PAGE appended" || bad "DEFAULT_PAGE absent"
rm -rf "$d"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
