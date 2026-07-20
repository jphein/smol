#!/usr/bin/env bash
# smol OTA-roll verify harness — one-command PASS/FAIL for a board's OTA install.
#
#   Usage:  ota_verify.sh <board_id> <target_build> [window_s]
#   e.g.    ota_verify.sh 7 346 360
#   Exit:   0 = PASS, 1 = FAIL/INFO, 3 = setup error (no creds)
#
# Tails smol/<id>/ota/{progress,diag,state} + smol/<id>/diag + smol/mesh/channel and prints
# a PASS/FAIL verdict. Encodes the v346-wave (2026-07-20) hard-won lessons:
#
#   * RETAINED-GHOST discipline — a fresh subscribe redelivers retained values (MQTT retain
#     flag = 1); only a LIVE publish (retain = 0) is trustworthy. mosquitto_sub -F '%R' gives
#     the flag; a PASS requires a LIVE flip, not a persisted value. (Cost us a false
#     "fleet installing" alarm before we caught it.)
#   * grep -a EVERYWHERE — one binary byte in an MQTT payload flips grep to binary mode and it
#     silently prints nothing; a plain-grep waiter read "no event" while the event sat in the
#     log. -a (text mode) is mandatory.
#   * USB vs OTA — installed_version flipping to target is NOT proof. A real OTA boots
#     slot=ota_1 (rst=ota); a USB flash shows slot=0 / rst=usb-jtag. slot=ota_1 is the proof.
#     (id5 hit v346 via USB and read as an "OTA win" until the slot check caught it.)
#   * DEATH-POINT — offset frozen >30s with done<total = the transfer died AT that byte.
#   * OFF-CHANNEL — the coexist disease is a CHANNEL MISMATCH: crown AP ch != ESP-NOW mesh ch
#     stalls the WiFi fetch (proven: co-channel moved 48KB, off-channel moved 0).
#   * PEER-SOURCE (#237) — ota/diag ` src=id<n>` = a peer HOLDER served it over ESP-NOW
#     (vs src=gw = crown/gateway WiFi-fetch).
set -uo pipefail

ID="${1:?usage: ota_verify.sh <board_id> <target_build> [window_s]}"
TARGET="${2:?target build number, e.g. 346}"
WINDOW="${3:-360}"
STALE=30   # offset unchanged this long (0<done<total) = death-point

# ---- broker creds (mirrors tools/ota_publish.sh; password never printed) --------------
OTA_ENV="$HOME/Projects/smol/tools/ota_publish.env"; [ -f "$OTA_ENV" ] && . "$OTA_ENV"
BROKER="${BROKER:-10.0.0.1}"; MQTT_USER="${MQTT_USER:-<mqtt-user>}"; ADDON="${ADDON:-<addon-slug>}"   # placeholders; real values from tools/ota_publish.env (git-ignored)
PW="$(timeout 25 bash -c 'tok=$(bw get password ha-llat 2>/dev/null) || exit 1
  HA_TOKEN="$tok" python3 "$HOME/Projects/ha/tools/ha_supervisor.py" GET "/addons/'"$ADDON"'/info" 2>/dev/null \
  | python3 -c "import sys,json;print(json.load(sys.stdin)[\"options\"][\"mqtt_password\"])" 2>/dev/null')"
[ -n "$PW" ] || { echo "FATAL: could not source mqtt password (bw locked? addon $ADDON unreachable?)"; exit 3; }

LOG="$(mktemp "/tmp/ota_verify_${ID}_XXXX.log")"
# %R = retain flag (1 ghost / 0 live); tab-delimited so | : = in payloads parse cleanly.
mosquitto_sub -h "$BROKER" -p 1883 -u "$MQTT_USER" -P "$PW" -i "ota_verify_${ID}_$$" -F '%R\t%t\t%p' \
  -t "smol/$ID/ota/progress" -t "smol/$ID/ota/diag" -t "smol/$ID/ota/state" \
  -t "smol/$ID/diag" -t "smol/mesh/channel" > "$LOG" 2>&1 &
SUB=$!; trap 'kill "$SUB" 2>/dev/null; rm -f "$LOG"' EXIT
sleep 2   # let the retained baseline land so we can tell "already-target" from a live flip

# grep helpers — always -a; g_all matches any retain flag, g_live only retain=0.
g_all()  { grep -a $'\t'"smol/$1"$'\t' "$LOG"; }
g_live() { grep -a "^0"$'\t'"smol/$1"$'\t' "$LOG"; }
ver()    { grep -oaE '"installed_version":"[0-9]+"' | grep -oaE '[0-9]+' | tail -1; }

baseline="$(g_all "$ID/ota/state" | tail -1 | ver)"
echo "── ota_verify: id$ID → v$TARGET · window ${WINDOW}s · broker $BROKER · baseline v${baseline:-?} ──"

verdict=""; reason=""; start=$(date +%s); last_off=-1; last_off_t=$start; hwm=0; monotonic=1
total="?"; phase="none"; src="none"

while :; do
  now=$(date +%s)
  mesh_ch="$(g_all mesh/channel | tail -1 | awk -F'\t' '{print $3}' | awk -F'|' '{print $3}')"
  d="$(g_all "$ID/diag" | tail -1)"
  ap_ch="$(printf '%s' "$d" | grep -oaE 'ap=[0-9]+' | grep -oaE '[0-9]+' | tail -1)"
  slot="$(printf '%s' "$d" | grep -oaE 'slot=[^|[:space:]]+' | head -1 | cut -d= -f2)"
  rst="$(printf '%s' "$d" | grep -oaE 'rst=[^|[:space:]]+' | head -1 | cut -d= -f2)"
  inst_live="$(g_live "$ID/ota/state" | tail -1 | ver)"
  inst_any="$(g_all "$ID/ota/state" | tail -1 | ver)"
  dg="$(g_all "$ID/ota/diag" | tail -1 | awk -F'\t' '{print $3}')"
  src="$(printf '%s' "$dg" | grep -oaE 'src=(gw|id[0-9]+)' | tail -1)"; src="${src:-none}"
  pl="$(g_all "$ID/ota/progress" | tail -1 | awk -F'\t' '{print $3}')"
  off="$(printf '%s' "$pl" | cut -d'|' -f1)"; total="$(printf '%s' "$pl" | cut -d'|' -f2)"
  phase="$(printf '%s' "$pl" | cut -d'|' -f3)"; [[ "$off" =~ ^[0-9]+$ ]] || off=""
  if [ -n "$off" ]; then
    [ "$off" -gt "$hwm" ] && hwm="$off"
    if [ "$off" != "$last_off" ]; then
      [ "$last_off" != -1 ] && [ "$off" -lt "$last_off" ] && monotonic=0
      last_off="$off"; last_off_t="$now"
    fi
  fi

  # ---- verdict ladder (first match wins) ----
  if [ -n "$mesh_ch" ] && [ -n "$ap_ch" ] && [ "$ap_ch" != "$mesh_ch" ]; then
    verdict=FAIL; reason="OFF-CHANNEL — crown AP ch=$ap_ch != ESP-NOW mesh ch=$mesh_ch (coexist disease; WiFi fetch will stall). Move the crown to ch$mesh_ch."; break
  elif printf '%s' "$dg" | grep -qa 'at=slot'; then
    verdict=FAIL; reason="at=slot — local otadata problem (#226); needs a USB flash, OTA can't proceed."; break
  elif [ -n "$off" ] && [ "$total" != "?" ] && [ "$off" -gt 0 ] && [ "$off" -lt "$total" ] && [ $((now-last_off_t)) -ge "$STALE" ]; then
    verdict=FAIL; reason="DEATH-POINT — offset frozen at $off/$total for ${STALE}s+ (transfer died mid-flight)."; break
  elif [ "$inst_live" = "$TARGET" ]; then
    if [ "$slot" = "ota_1" ]; then
      verdict=PASS; reason="installed_version=$TARGET on slot=ota_1 — real OTA-over-WiFi. source: $src"
      [ "${src#src=id}" != "$src" ] && reason="$reason  ← PEER-SOURCED (#237)"; break
    elif [ -n "$slot" ]; then
      verdict=INFO; reason="reports v$TARGET but slot=$slot / rst=$rst → USB flash, NOT an OTA (no over-the-air proof)."; break
    fi   # slot not yet known — keep waiting for a diag
  fi

  if [ $((now-start)) -ge "$WINDOW" ]; then
    verdict=FAIL
    if [ "$inst_any" = "$TARGET" ] && [ "$baseline" = "$TARGET" ]; then
      reason="already on v$TARGET at subscribe (retained baseline) — no LIVE OTA observed. Run BEFORE arming to verify a fresh flip."
    else
      reason="window ${WINDOW}s elapsed, no completion — HWM ${hwm}/${total} (monotonic=$([ $monotonic = 1 ] && echo yes || echo NO)), last phase '${phase:-none}', last diag '${dg:-none}'."
    fi
    break
  fi
  sleep 3
done

printf '\n════════════════════════════════════════════════════════════\n'
printf '  VERDICT: %s — id%s → v%s\n  %s\n' "$verdict" "$ID" "$TARGET" "$reason"
printf '  ── evidence ──\n'
printf '  installed v%s (target v%s) · slot=%s · rst=%s\n' "${inst_any:-?}" "$TARGET" "${slot:-?}" "${rst:-?}"
printf '  offset HWM %s/%s · monotonic=%s · phase=%s\n' "$hwm" "$total" "$([ $monotonic = 1 ] && echo yes || echo NO)" "${phase:-none}"
printf '  crown AP ch=%s · mesh ch=%s · %s\n' "${ap_ch:-?}" "${mesh_ch:-?}" "${src:-none}"
printf '  last ota/diag: %s\n' "${dg:-none}"
printf '════════════════════════════════════════════════════════════\n'
[ "$verdict" = "PASS" ] && exit 0 || exit 1
