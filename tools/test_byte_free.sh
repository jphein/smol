#!/usr/bin/env bash
# test_byte_free.sh — #351. Prove `tools/check_byte_free.py`'s source arm can FAIL.
#
# The whole point of #351 is that a claim nothing tests stops being true. A checker nothing
# tests is the same object one level up, so each case here is a miniature source tree that
# violates exactly one rule, and the suite asserts both the exit code AND the finding text.
#
# It is worth saying that this suite earned its keep before it existed: the checker's FIRST
# run reported two failures on a healthy tree, and both were the checker's fault, not the
# code's —
#   * it read `#![cfg_attr(not(espnow), allow(dead_code))]` in wifi.rs as a cfg GATE on
#     `espnow` (a cfg_attr conditions an attribute; it does not decide whether the item
#     exists), and
#   * it looked for a claim's gate only inside the claim's own file, so `net/cast.rs` — gated
#     by `#[cfg(feature = "cast")] pub mod cast;` in net.rs, another file — looked unbacked.
# Both are now cases below, so neither can come back.
#
# A case is a directory: `src/` (the miniature crate) + `EXPECT` (a substring of the required
# failure, or the literal `OK`).
#
# No cargo, no network, no ELF: this arm is pure source structure, which is exactly why it is
# the one that can run everywhere and be the proof.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CASES="$HERE/test_byte_free_cases"
CHK="$HERE/check_byte_free.py"
pass=0; fail=0
note() { printf '   \033[32mok\033[0m   %s\n' "$1"; pass=$((pass + 1)); }
oops() { printf '   \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail + 1)); }

for d in "$CASES"/*/; do
  name="$(basename "$d")"
  [ -f "$d/EXPECT" ] || { oops "$name: no EXPECT file"; continue; }
  want="$(cat "$d/EXPECT")"
  out="$("$CHK" --src "$d/src" 2>&1)"; rc=$?

  if [ "$want" = "OK" ]; then
    if [ "$rc" = 0 ]; then note "$name — passes"
    else oops "$name: expected pass, got $rc — $(printf '%s' "$out" | grep FAIL | head -1)"; fi
  else
    if [ "$rc" != 1 ]; then
      oops "$name: expected exit 1, got $rc"
    elif ! printf '%s' "$out" | grep -qF -- "$want"; then
      oops "$name: failed but not with '$want' — got: $(printf '%s' "$out" | grep FAIL | head -1)"
    else
      note "$name — caught: $want"
    fi
  fi
done

printf '\n   %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
