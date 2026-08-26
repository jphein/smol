#!/usr/bin/env bash
# test_board_consts.sh — #419: prove tools/check_board_consts.py catches the trap it exists for,
# clears the fixed version of it, and does not cry wolf on the benign remainder.
#
# THE ARM WORTH READING IS 3. It asserts that a NAIVE BARE-IDENTIFIER GREP PASSES ON THE SAME BYTES
# that the checker fails. That is not a stylistic preference: the first version of this checker
# counted bare occurrences, and when it was pointed at the real armed trap it printed "0 SHADOWED"
# and exit 0 — a clean bill of health on the collision — because `ui/slint_shell.rs` uses the bare
# name and that reference resolves to its OWN file-local const, not to `board::HOLD_SLOP_PX`. Same
# identifier, two bindings. Arm 3 is what stops that being "simplified" back in.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$HERE/check_board_consts.py"
[ -f "$CHECK" ] || { echo "missing $CHECK" >&2; exit 2; }

pass=0; fail=0
ok(){ pass=$((pass+1)); echo "ok   - $1"; }
no(){ fail=$((fail+1)); echo "FAIL - $1"; }
eq(){ if [ "$2" = "$3" ]; then ok "$1 ($2)"; else no "$1: want [$2] got [$3]"; fi; }

TMPROOT="${TMPDIR:-/var/tmp}"; case "$TMPROOT" in /tmp|/tmp/*) TMPROOT=/var/tmp ;; esac
W="$(mktemp -d "$TMPROOT/boardconst-XXXXXX")"
trap 'rm -rf "$W"' EXIT INT TERM

# ── fixture: the minimum tree shaped like targets/<t>/src/{board,ui} ───────────────────────────
mk(){ # <consumer-body>  — rebuilds the fixture with a given consumer file
  rm -rf "$W/t"; mkdir -p "$W/t/targets/watch/src/board" "$W/t/targets/watch/src/ui"
  cat > "$W/t/targets/watch/src/board/mod.rs" <<'EOF'
#[cfg(feature = "board-a")]
mod board_a;
#[cfg(feature = "board-a")]
pub use board_a::*;
EOF
  cat > "$W/t/targets/watch/src/board/board_a.rs" <<'EOF'
pub const LCD_WIDTH: u16 = 320;
pub const HOLD_SLOP_PX: u16 = 18;
pub const WS2812_GPIO: u8 = 4;
EOF
  printf '%s\n' "$1" > "$W/t/targets/watch/src/ui/shell.rs"
}
run(){ python3 "$CHECK" "$W/t" 2>&1; }

echo "== 1. wired via board:: — clean =="
mk 'use crate::board;
const OTHER: u16 = 1;
fn f() { let _ = board::LCD_WIDTH; let _ = board::HOLD_SLOP_PX; let _ = board::WS2812_GPIO; }'
out="$(run)"; rc=$?
eq "all consts read via board:: exits 0" "0" "$rc"
case "$out" in *"0 SHADOWED"*) ok "reports 0 shadowed" ;; *) no "unexpected: $out" ;; esac

echo "== 2. the armed trap: board declares it, consumer shadows it with a file-local =="
mk 'use crate::board;
const HOLD_SLOP_PX: u16 = 24;
fn f() { let _ = board::LCD_WIDTH; let _ = board::WS2812_GPIO; if 5 > HOLD_SLOP_PX {} }'
out="$(run)"; rc=$?
eq "a shadowed board constant exits 1" "1" "$rc"
case "$out" in *"1 SHADOWED"*)     ok "counts exactly one shadowed constant" ;; *) no "count wrong: $out" ;; esac
case "$out" in *HOLD_SLOP_PX*)     ok "names the constant" ;;                  *) no "unnamed: $out" ;; esac
case "$out" in *board_a.rs*)       ok "names where it is declared" ;;          *) no "no declaration site" ;; esac
case "$out" in *shell.rs*)         ok "names the file that shadows it" ;;      *) no "no shadow site" ;; esac
case "$out" in *"one bundle"*)     ok "warns against wiring a subset" ;;       *) no "no bundle warning" ;; esac

echo "== 3. THE KEEPER — a naive bare grep PASSES on the same bytes =="
# If this ever fails, the checker has been reduced to a bare-identifier search and has stopped
# being able to see the trap at all. Exactly the shape #432 arm 4 pins for the config guard.
if grep -qE '\bHOLD_SLOP_PX\b' "$W/t/targets/watch/src/ui/shell.rs"; then
  ok "bare-identifier grep DOES find the name in the shadowing file (so it cannot be the test)"
else
  no "fixture no longer reproduces the trap — the shadowing file must reference the bare name"
fi
# and the qualified form, which is the only one that proves board:: is read, is absent for it
if grep -qE 'board::HOLD_SLOP_PX' "$W/t/targets/watch/src/ui/shell.rs"; then
  no "fixture reads board::HOLD_SLOP_PX — that is the FIXED case, not the trap"
else
  ok "no qualified read of the board constant (the distinguishing fact)"
fi

echo "== 4. qualified read WINS even when a file-local of the same name exists =="
# A file may legitimately declare its own const AND read the board's. `board::NAME` resolves
# unambiguously, so it must count as a reader and the constant is wired, not shadowed.
mk 'use crate::board;
const HOLD_SLOP_PX: u16 = 24;
fn f() { let _ = board::LCD_WIDTH; let _ = board::WS2812_GPIO;
         let _ = board::HOLD_SLOP_PX; if 5 > HOLD_SLOP_PX {} }'
out="$(run)"; rc=$?
eq "a qualified read clears the shadow" "0" "$rc"
case "$out" in *"0 SHADOWED"*) ok "not reported as shadowed" ;; *) no "false positive: $out" ;; esac

echo "== 5. comments are not readers (#426 class) =="
# A constant mentioned ONLY in prose must not read as wired — prose about a constant nobody
# applies is precisely what is being hunted, so counting it inverts the result.
mk 'use crate::board;
const HOLD_SLOP_PX: u16 = 24;
/// drifting past [`board::HOLD_SLOP_PX`] disarms it
// let _ = board::HOLD_SLOP_PX;
fn f() { let _ = board::LCD_WIDTH; let _ = board::WS2812_GPIO; if 5 > HOLD_SLOP_PX {} }'
out="$(run)"; rc=$?
eq "a comment-only board:: mention is NOT a reader" "1" "$rc"
case "$out" in *HOLD_SLOP_PX*) ok "still reported as shadowed despite the doc mention" ;; *) no "comment counted as a reader: $out" ;; esac

echo "== 6. UNREAD is reported, never failed =="
# A pin whose value is hardcoded at its use site is not a contradiction — no competing
# declaration exists. Failing these would fire on a dozen innocent constants on day one and the
# arm would be routed around (#338), taking the SHADOWED finding with it.
mk 'use crate::board;
fn f() { let _ = board::LCD_WIDTH; }'
out="$(run)"; rc=$?
eq "unread-but-uncontradicted constants exit 0" "0" "$rc"
case "$out" in *"UNREAD"*)        ok "unread constants are still REPORTED" ;;  *) no "silent: $out" ;; esac
case "$out" in *WS2812_GPIO*)     ok "names the unread constant" ;;            *) no "unnamed unread" ;; esac
case "$out" in *"0 SHADOWED"*)    ok "and not counted as shadowed" ;;          *) no "miscounted: $out" ;; esac

echo "== 7. vacuous-pass guards: cannot-check is 2, never 0 =="
mkdir -p "$W/empty"
out="$(python3 "$CHECK" "$W/empty" 2>&1)"; rc=$?
eq "no board modules at all exits 2" "2" "$rc"
case "$out" in *"did not run"*) ok "explains that it did not run" ;; *) no "weak message: $out" ;; esac
# a board module with nothing else to search would score every constant as unread = noise
rm -rf "$W/t2"; mkdir -p "$W/t2/targets/watch/src/board"
cp "$W/t/targets/watch/src/board/board_a.rs" "$W/t2/targets/watch/src/board/"
out="$(python3 "$CHECK" "$W/t2" 2>&1)"; rc=$?
eq "a board module with no consumers exits 2" "2" "$rc"

echo "== 8. the REAL tree — proves the glob still matches this repo's layout =="
# The fixture arms prove the LOGIC; this proves the checker still finds the real board seam. A
# layout move (targets/*/src/board/) would keep every arm above green and only fail here.
out="$(python3 "$CHECK" "$HERE/.." 2>&1)"; rc=$?
eq "real repo exits 0 (no live shadowed constant)" "0" "$rc"
n=$(sed -n 's/.*seam: \([0-9]*\) board module.*/\1/p' <<<"$out")
if [ "${n:-0}" -ge 2 ]; then ok "found $n real board modules (glob matches the layout)"; else
  no "found ${n:-0} board modules — the glob has stopped matching targets/*/src/board/*.rs"; fi
w=$(sed -n 's/.*, \([0-9]*\) constants with code readers.*/\1/p' <<<"$out")
if [ "${w:-0}" -ge 30 ]; then ok "$w real constants have code readers"; else
  no "only ${w:-0} constants scored readers — reference counting looks broken on the real tree"; fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
