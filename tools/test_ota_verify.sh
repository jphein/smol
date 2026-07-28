#!/usr/bin/env bash
# test_ota_verify.sh — regression suite for tools/ota_verify.sh.
#
# NO broker, NO hardware, NO cargo. Each case replays a canned MQTT log through the REAL script via
# its OTA_VERIFY_FIXTURE seam, then asserts the verdict AND the specific finding text.
#
# Two properties this suite exists to lock, because both were broken in production:
#
#   1. THE INVARIANT — a verdict may come only from a transition observed LIVE (retain=0) inside the
#      window. `retained_only_ghosts` is the pure form of this: a complete, perfectly consistent OTA
#      story told ENTIRELY by retained messages must NEVER be a PASS.
#   2. NO MASKING — every check is evaluated every poll, so a finding can never delete another. The
#      `n4_cc0_masks_deathpoint` / `n5_retained_atslot_ghost` cases assert that a wrong-or-ambiguous
#      check still prints while the RIGHT verdict is the headline. That slot masked a genuine
#      death-point four separate times before the restructure.
#
# The fixture format is the harness's own log format, `<retain>\t<topic>\t<payload>`, optionally
# prefixed `@<seconds>\t` to be delivered that many seconds in. Unprefixed lines are the
# retained-at-subscribe batch. Timed lines are what make a TRANSITION expressible.
#
# TWO SHARP EDGES FOR FIXTURE AUTHORS:
#
#   1. Replay is SEQUENTIAL, so timestamps must be NON-DECREASING. An unprefixed line placed AFTER an
#      `@5` line lands at t=5, not t=0 — the replayer is already past t=5 and never goes back. Put the
#      whole retained batch first. (Raised by oracle-verify while auditing this seam.)
#   2. Leave GENEROUS margins around anything time-derived. A stall episode is only counted if a poll
#      lands inside the frozen span, and under the suite's own load a poll iteration can stretch well
#      past POLL seconds. `retry_restart_then_pass` originally had a 4 s frozen span with
#      STALL_AFTER=2 and passed on a loaded box while FAILING in a pristine checkout — a flaky
#      assertion, which is worse than no assertion because it REDs a future audit at random. The span
#      is now 9 s. If an assertion depends on a poll landing in an interval, make the interval wide.
#
# Run:  tools/test_ota_verify.sh        (exit 0 = all green)
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$HERE/ota_verify.sh"
CASES="$HERE/test_ota_verify_cases"
[ -x "$SCRIPT" ] || [ -f "$SCRIPT" ] || { echo "missing $SCRIPT" >&2; exit 2; }
[ -d "$CASES" ] || { echo "missing $CASES" >&2; exit 2; }

pass=0; fail=0; OUT=""; RC=0
# Compressed thresholds so a stall case takes ~3 s instead of ~153 s. These are the only knobs the
# tests move; every code path under test is the shipped one. STALL_AFTER (data plane) and RETRY_GRACE
# (control plane) are separate on purpose — see the header of ota_verify.sh — and the
# `knobs_are_independent` case below proves moving one does not move the other.
export OTA_VERIFY_STALL_AFTER=2 OTA_VERIFY_RETRY_GRACE=2 OTA_VERIFY_POLL=1 OTA_VERIFY_SETTLE=1

run() { # run <case-file> <id> <target> <window>
  # `.mqtt`, not `.log`: the repo's .gitignore has a blanket `*.log`, so fixtures named .log are
  # silently NOT committed — the suite would pass here and be missing its cases for everyone else.
  OUT="$(OTA_VERIFY_FIXTURE="$CASES/$1.mqtt" bash "$SCRIPT" "$2" "$3" "$4" 2>&1)"; RC=$?
  printf '\n── %s (id%s → v%s)\n' "$1" "$2" "$3"
}
ok()   { pass=$((pass+1)); printf '   ok   - %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '   FAIL - %s\n' "$1"; }
# ── assertions use PURE BASH, no pipelines, and that is a correctness requirement ──────────────
# `printf '%s' "$OUT" | grep -qaF "$pat"` under `pipefail` returns non-zero ~0.2% of the time EVEN
# WHEN THE PATTERN MATCHES (grep -q exits on first match without draining → writer takes EPIPE →
# pipefail surfaces the writer's status). Measured: 3 spurious non-matches in 1500 runs on real
# output, position-independent; `case` was 0 in 1500. At ~124 assertions per run that is a ~25%
# chance of at least one PHANTOM FAILURE per suite run — and it is exactly what produced the
# wandering counts (100/2, 119/4, 122/2, 124/0) across otherwise identical runs, including two
# failures I could not reproduce and briefly suspected in the code under test. A flaky assertion is
# worse than no assertion: it REDs an audit at random and teaches everyone to re-run until green.
verdict_is() { # verdict_is <expected>
  local got="${OUT#*VERDICT: }"; got="${got%%[! A-Z]*}"; got="${got%% *}"
  [ "$got" = "$1" ] && ok "verdict $1" || bad "verdict: want $1, got ${got:-<none>}"
}
rc_is()  { [ "$RC" = "$1" ] && ok "exit $1" || bad "exit: want $1, got $RC"; }
has()    { case "$OUT" in *"$2"*) ok "$1";; *) bad "$1 (missing: $2)";; esac; }
hasnt()  { case "$OUT" in *"$2"*) bad "$1 (unexpected: $2)";; *) ok "$1";; esac; }
# A proof conjunct's state, read off the proof block: proof <A|B|C|D|E|F> <yes|NO>
# y() renders 'yes' or 'NO ' (3 chars, padded), so match the padded form exactly.
proof()  { local p="$2"; [ "$p" = NO ] && p="NO "
  case "$OUT" in *"[$p] $1 "*) ok "proof $1=$2";; *) bad "proof $1: want $2";; esac; }

echo "═══ PASS must be REACHABLE (it was not, for a long time) ═══"
run pass_peer_sourced 8 907 12
verdict_is PASS; rc_is 0
proof A yes; proof B yes; proof C yes; proof D yes; proof E yes
has "reports the live flip"      "state 906→907"
has "reports the slot flip"      "slot 0→1"
has "credits the peer holder"    "PEER-SOURCED"

echo
echo "═══ N1 · an EMPTY baseline must not satisfy 'baseline != TARGET' ═══"
run n1_no_baseline_state 8 907 4
verdict_is UNPROVEN; rc_is 1
proof A NO
hasnt "no false 'real OTA' claim" "over the air"
# The first thing ever observed IS the live 907, so there is no prior version to flip FROM. The
# harness must say that, not invent a baseline — the old code read the empty baseline as
# "!= TARGET" and printed `v? → v907 … real OTA-over-WiFi`.
has   "no baseline to flip from"  "already on v907 at our FIRST observation"
# ...and when the operand truly never arrives, it must print `unknown`, never a value.
run n1b_state_never_arrives 8 907 4
verdict_is UNPROVEN; rc_is 1
proof A NO
has   "absent operand prints unknown" "A state flip   unknown → unknown"
hasnt "and is never a verdict"        "over the air"

echo
echo "═══ N2 · a PERSISTENT ota=confirmed must not read as this run's proof ═══"
echo "     (rtc_fast survives sw/wdt/panic/brownout resets AND a usb-jtag reflash)"
run n2_persistent_confirmed 9 907 4
verdict_is UNPROVEN; rc_is 1
proof B NO; proof D NO
has "no slot flip is the catch"  "B slot flip    1 → 1"
has "token never transitioned"   "confirmed → confirmed"
run n2b_usb_reflash 8 907 4
verdict_is UNPROVEN; rc_is 1
proof D NO; proof E NO
has "usb-jtag disqualifies"      "rst=usb-jtag"

echo
echo "═══ N4 · cc=0 is not proof of off-channel, and must mask nothing ═══"
run n4_cc0_masks_deathpoint 8 907 8
verdict_is FAIL; rc_is 1
has "DEATH-POINT is the headline" "VERDICT: FAIL"
has "death-point fired"           "offset frozen at 720000/1440528"
has "cc=0 reported, not acted on" "AMBIGUOUS, so no verdict"
hasnt "no off-channel verdict"    "[FAIL   ] OFF-CHANNEL"
run n4b_cc0_associated_offchannel 8 907 4
verdict_is FAIL; rc_is 1
has "fires when association IS proven" "cc=0 AND ap=1"
has "names whose association it is"    "OWN association"
run n4c_cc2_not_associated 8 907 4
has "cc=2 is not a verdict"       "NOT ASSOCIATED"
hasnt "and never a FAIL"          "[FAIL"
run n4d_heap_not_a_channel 8 907 4
hasnt "heap= is never read as ap=" "[FAIL"
has  "off-channel stays inapplicable" "INAPPLICABLE"

echo
echo "═══ N5 · a retained at=slot ghost must not condemn a successful OTA ═══"
run n5_retained_atslot_ghost 8 907 8
verdict_is PASS; rc_is 0
has "ghost printed, not obeyed"   "RETAINED ota/diag carries at=slot"
run n5b_live_atslot 8 907 4
verdict_is FAIL; rc_is 1
has "a LIVE at=slot still fires"  "a LIVE ota/diag reports at=slot"

echo
echo "═══ N6 · a check that cannot work must SAY so ═══"
run n6_truncated_cut 50 907 4
has "truncation is explicit"      "cut=75 bytes"
has "operands were sent and lost" "SENT and LOST"
run pass_peer_sourced 8 907 12
has "leaf inapplicability is printed" "CANNOT FIRE"

echo
echo "═══ N7 · the second producer (esp32c6-watch constant DIAG) ═══"
run n7_c6_constant_diag 236 907 4
verdict_is UNPROVEN; rc_is 1
has "not reported as a failure"   "UNPROVABLE BY THIS HARNESS, not failed"
has "names the producer"          "esp32c6-watch"
has "explains unreachability"     "STRUCTURALLY UNREACHABLE"

echo
echo "═══ ROLLED BACK · in-window transition vs a persistent marker ═══"
run rollback_inwindow 8 907 4
verdict_is FAIL; rc_is 1
has "reports it as a transition"  "TRANSITION observed live in-window: ota=none → rolled-back"
run rollback_stale_marker 8 907 4
has "stale marker is not proof"   "may predate this run"
hasnt "not claimed as in-window"  "TRANSITION observed live in-window"

echo
echo "═══ THE INVARIANT · retained-only evidence can never be a verdict ═══"
run retained_only_ghosts 8 907 4
verdict_is UNPROVEN; rc_is 1
proof A NO; proof B NO; proof C NO; proof D NO; proof E NO
hasnt "no PASS from ghosts alone" "over the air"
run deathpoint_retained_ghost 8 907 6
hasnt "a retained frozen offset is not a death-point" "[FAIL"
has   "and says why"              "ghost of an earlier attempt"

echo
echo "═══ TIME AXIS · a stall is a RETRY, not a death ═══"
echo "     A real 907 install on id8 SUCCEEDED and this harness reported FAIL DEATH-POINT: it broke"
echo "     out of a 600 s window at ~60 s and never saw the PASS that landed 3.5 min later. The stall"
echo "     reading was right; treating a stall as TERMINAL was wrong. No failure ends the run now."
run retry_restart_then_pass 8 907 14
verdict_is PASS; rc_is 0
has  "a stall then a restart still PASSes" "over the air"
has  "restart from 0 is not corruption"    "monotonic=yes"
has  "the restart is counted"              "restarts from a lower offset=1"
has  "the stall episode is on the record"  "stall episodes=1"
run stall_no_retry_is_death 8 907 8
verdict_is FAIL; rc_is 1
has  "a stall with NO retry signal is a death" "NO retry signal inside the"
has  "and names the grace it measured against" "grace (last retry signal: none"
has  "and says it was not terminal to the run" "NOT terminal to this run"
run stall_with_retry_not_death 8 907 8
verdict_is UNPROVEN; rc_is 1
hasnt "a retrying transfer is NOT a death-point" "] DEATH-POINT  offset frozen"
has   "prefers the board's own signal"           "the board has NOT given up"
has   "tells the operator to extend the window"  "Re-run with a longer window"
run total_change_resets_hwm 8 907 6
has   "a changed total resets the HWM"  "live progress HWM 100000/1440528"
has   "and is counted as a new image"   "images seen=2"
hasnt "no HWM carried across images"    "HWM 1277952"
run midstream_regression 8 907 6
has   "a regression with no retry is still flagged" "monotonic=NO"
hasnt "and is not excused as a restart"             "restarts from a lower offset=1"

echo
echo "═══ THE TWO THRESHOLDS ARE SEPARATE KNOBS ═══"
echo "     One knob with two semantics is the conflation pattern behind most of this file's defects"
echo "     (cc=0 = off-channel OR unassociated; ota=confirmed = a build OR THIS build). Each was"
echo "     harmless until the two meanings diverged. These must move independently."
# Same fixture, same STALL_AFTER: a WIDE retry-grace forgives the stall (still retrying),
# a NARROW one calls it dead. Only RETRY_GRACE moved, so only the control-plane arm may change.
OUT="$(OTA_VERIFY_RETRY_GRACE=30 OTA_VERIFY_FIXTURE="$CASES/stall_with_retry_not_death.mqtt" \
       bash "$SCRIPT" 8 907 12 2>&1)"; RC=$?
printf '\n── stall_with_retry_not_death · RETRY_GRACE=30 (wide)\n'
verdict_is UNPROVEN
has "a wide retry-grace forgives the stall" "the board has NOT given up"
OUT="$(OTA_VERIFY_RETRY_GRACE=1 OTA_VERIFY_FIXTURE="$CASES/stall_with_retry_not_death.mqtt" \
       bash "$SCRIPT" 8 907 12 2>&1)"; RC=$?
printf '── stall_with_retry_not_death · RETRY_GRACE=1 (narrow), STALL_AFTER unchanged\n'
verdict_is FAIL
has "a narrow retry-grace calls the same stall dead" "NO retry signal inside the 1s grace"
has "and the stall threshold did NOT move with it"   ">= 2s stall-after"
# The retired single knob must fail LOUDLY, not be silently ignored — a caller who sets it believes
# they changed a threshold, and getting the default instead is exactly the class of silent-wrong-answer
# this file exists to prevent.
OUT="$(OTA_VERIFY_STALE=2 OTA_VERIFY_FIXTURE="$CASES/pass_peer_sourced.mqtt" \
       bash "$SCRIPT" 8 907 4 2>&1)"; RC=$?
printf '── the retired OTA_VERIFY_STALE knob\n'
# 2, not 3: a renamed knob is the CALLER's mistake. Sharing code 3 with "could not source mqtt
# password" made an audit read a TEST failure as an ENVIRONMENT failure.
rc_is 2
has "refuses to run rather than ignore it" "OTA_VERIFY_STALE is retired"
has "and names both replacements"          "OTA_VERIFY_RETRY_GRACE"

echo
echo "═══ AUDIT ROUND 2 · findings from oracle-verify ═══"
# O6 — at=slot is the more specific root cause and carries a DIFFERENT instruction (USB-flash it), so
# a death-point CAUSED BY a failed otadata write must not headline as a generic death-point.
run atslot_outranks_deathpoint 8 907 8
verdict_is FAIL; rc_is 1
has   "at=slot headlines over the stall"  "VERDICT: FAIL"
has   "and names the otadata cause first" "AT-SLOT"
has   "the stall still prints underneath" "720000/1440528"
# O4 — trying forever, arriving nowhere. Neither DEATH-POINT (stopped trying) nor STALLED (still
# trying) catches it, and without an arm the board reports extend-the-window forever.
OUT="$(OTA_VERIFY_RETRY_GRACE=30 OTA_VERIFY_FIXTURE="$CASES/retry_loop_no_progress.mqtt" \
       bash "$SCRIPT" 8 907 20 2>&1)"; RC=$?
printf '\n── retry_loop_no_progress · RETRY_GRACE=30 (retries stay fresh)\n'
verdict_is FAIL; rc_is 1
has   "a barren retry loop FAILS"            "RETRY-LOOP"
has   "and says a longer window won't help"  "LONGER WINDOW WILL NOT HELP"
has   "high-water never advanced"            "NO high-water advance"
hasnt "not left as extend-the-window"        "Re-run with a longer window"
# O5 residual — two gateway caches with different freshness gates (STAT 45 s vs DIAG_FRESH_MS 150 s)
# can hand us a stale ota=none then a fresh confirmed, which LOOKS like an in-window transition for an
# OTA that finished before we subscribed. up= proves whether the BOOT was ours.
run stale_cache_confirmed 8 907 6
verdict_is UNPROVEN; rc_is 1
proof A yes; proof B yes; proof C yes; proof D yes; proof E yes
proof F NO
has   "A-E can all hold and it still is not a PASS" "F boot in-window"
hasnt "no over-claim of having watched it"          "over the air"

printf '\n════════════════════════════════════════════\n'
printf '  %d passed · %d failed\n' "$pass" "$fail"
printf '════════════════════════════════════════════\n'
[ "$fail" = 0 ] || exit 1
