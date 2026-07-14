#!/usr/bin/env bash
# test_ota_ratchet.sh — unit tests for the #128 build-number ratchet in ota_publish.sh.
#
# Pure-logic: NO broker, NO cargo build, NO publish. It extracts just the two functions under
# test (choose_build — the ratchet/override decision; read_staged_build — the retained parse +
# reachable/unreachable disambiguation) and stubs mqtt_pw / mosquitto_sub. The full end-to-end
# (staging against the LIVE broker) is the real acceptance test; this locks the decision logic.
#
# Run:  tools/test_ota_ratchet.sh      (exit 0 = all green)
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$HERE/ota_publish.sh"
[ -f "$SCRIPT" ] || { echo "missing $SCRIPT" >&2; exit 2; }

# Pull in ONLY the functions under test — no side effects, and none of the parent's `set -e`.
eval "$(awk '/^choose_build\(\)\{/,/^\}/' "$SCRIPT")"
eval "$(awk '/^read_staged_build\(\)\{/,/^\}/' "$SCRIPT")"

# read_staged_build references these + calls mqtt_pw/mosquitto_sub — stub them all.
# (export: they're read inside the eval'd read_staged_build, invisible to shellcheck here.)
export BROKER="test.invalid" MQTT_USER="tester"
mqtt_pw(){ printf 'stub-pw'; }

pass=0; fail=0
eq(){ # name  expected  actual
  if [ "$2" = "$3" ]; then pass=$((pass+1)); printf 'ok   - %s\n' "$1"
  else fail=$((fail+1)); printf 'FAIL - %s\n        want [%s]\n        got  [%s]\n' "$1" "$2" "$3"; fi
}

echo "== choose_build  (count, staged, override) -> build =="
eq "no stage, no override -> count"                254 "$(choose_build 254 ''  ''  2>/dev/null)"
eq "staged behind count -> count"                  254 "$(choose_build 254 100 ''  2>/dev/null)"
# staged == count: max(count, staged+1) = count+1 — a re-stage at the same count is forced to
# SUPERSEDE the retained record (guarantees boards accept it); the +1 self-corrects at the next
# commit. This is the literal max(count, staged+1) spec, not a bug.
eq "staged == count -> staged+1 (force re-stage supersede)" 255 "$(choose_build 254 254 ''  2>/dev/null)"
eq "staged == count-1 (normal advance) -> count"   254 "$(choose_build 254 253 ''  2>/dev/null)"
eq "staged AHEAD (canary pin) -> ratchet staged+1" 331 "$(choose_build 254 330 ''  2>/dev/null)"
eq "override below count -> override as-is"         200 "$(choose_build 254 ''  200 2>/dev/null)"
eq "override above count -> override as-is"         330 "$(choose_build 254 ''  330 2>/dev/null)"
eq "override IGNORES staged (no ratchet)"          200 "$(choose_build 254 999 200 2>/dev/null)"
eq "override>count emits canary warning"             1 "$(choose_build 254 '' 330 2>&1 >/dev/null | grep -c 'AHEAD of the honest')"
eq "override<=count emits NO warning"                0 "$(choose_build 254 '' 200 2>&1 >/dev/null | grep -c 'AHEAD of the honest')"
eq "ratchet emits heal note"                         1 "$(choose_build 254 330 ''  2>&1 >/dev/null | grep -c 'ratchet')"
eq "no-ratchet emits NO note"                        0 "$(choose_build 254 100 ''  2>&1 >/dev/null | grep -c 'ratchet')"

echo "== read_staged_build  (stubbed mosquitto_sub) -> build:rc =="
mosquitto_sub(){ printf 'OTA|321|2048|deadbeef|sig|http://h/o.bin\n'; return 0; }
eq "reachable + retained record -> 321:0"  "321:0" "$(printf '%s:%s' "$(read_staged_build)" "$?")"
mosquitto_sub(){ printf 'garbage\n'; return 0; }
eq "reachable + non-OTA payload -> :0"       ":0"   "$(printf '%s:%s' "$(read_staged_build)" "$?")"
mosquitto_sub(){ echo "Timed out" >&2; return 27; }
eq "reachable + empty topic -> :0"           ":0"   "$(printf '%s:%s' "$(read_staged_build)" "$?")"
mosquitto_sub(){ echo "Error: Connection refused" >&2; return 1; }
eq "broker unreachable -> :1"                ":1"   "$(printf '%s:%s' "$(read_staged_build)" "$?")"

echo "----"
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
