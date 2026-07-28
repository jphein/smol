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
#   * USB vs OTA — installed_version flipping to target is NOT proof; the discriminator is the
#     RESET REASON. A USB flash resets over the cable → `rst=usb-jtag`; an OTA reboots in
#     software → `rst=sw`. (id5 hit v346 via USB and read as an "OTA win" until this caught it.)
#     CORRECTED 2026-07-28 against live payloads: this file previously asserted a real OTA shows
#     `slot=ota_1` / `rst=ota`. Neither string is ever published — `slot=` is a NUMERIC index
#     (mode.rs:3208 ← d.boot_slot) and reset_reason_token() (ota.rs) emits no `ota` token at all.
#     The PASS arm was therefore unreachable and every genuine OTA was reported as a USB flash.
#     LESSON: this harness was written against an IMAGINED schema. A diagnostic must be validated
#     against a live payload, because a wrong one does not error — it prints a confident verdict
#     in the same format as a correct one, which is worse than a crash.
#   * SCHEMA (verified live 2026-07-28, smol/<id>/diag):
#       slot=<0|1> · rst=<panic|power-on|sw|deep-sleep|brownout|wdt|usb-jtag|glitch|other|unk>
#       ota=<none|confirmed|rolled-back>   ← `rolled-back` is the firmware's EXPLICIT revert
#       signal; prefer it over inferring a revert from a build number that moved.
#     `ap=<channel>` is CONDITIONAL — appended last and only once known (same precedent as
#     `brst=`/`io=`), so it is present on an associated crown and ABSENT on a leaf that has not
#     associated. That conditionality is what hid the bug below: `tail -1` grabbed the real
#     `ap=6` on the crown and the tail of `heap=` on every leaf, so the check was right on one
#     board and silently wrong on the rest. A field that is sometimes absent is more dangerous
#     than one that never exists — it buys the broken code an alibi.
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
# %r = retain flag (1 ghost / 0 live); tab-delimited so | : = in payloads parse cleanly.
# 2026-07-28: this was `%R`, which is NOT a mosquitto specifier — it expands to the EMPTY string
# (proved: `-F '[%R]'` → `[]`, `-F '[%r]'` → `[1]`). Every line therefore began with a tab, so
# g_live()'s `^0\t` never matched, `inst_live` was always empty, and the whole retained-ghost
# discipline was INVERTED: every message read as a ghost. That single wrong character is why PASS
# was unreachable and why the death-point gate silently disabled itself.
mosquitto_sub -h "$BROKER" -p 1883 -u "$MQTT_USER" -P "$PW" -i "ota_verify_${ID}_$$" -F '%r\t%t\t%p' \
  -t "smol/$ID/ota/progress" -t "smol/$ID/ota/diag" -t "smol/$ID/ota/state" \
  -t "smol/$ID/diag" -t "smol/mesh/channel" > "$LOG" 2>&1 &
SUB=$!; trap 'kill "$SUB" 2>/dev/null; rm -f "$LOG"' EXIT
sleep 2   # let the retained baseline land so we can tell "already-target" from a live flip

# grep helpers — always -a; g_all matches any retain flag, g_live only retain=0.
g_all()  { grep -a $'\t'"smol/$1"$'\t' "$LOG"; }
g_live() { grep -a "^0"$'\t'"smol/$1"$'\t' "$LOG"; }
ver()    { grep -oaE '"installed_version":"[0-9]+"' | grep -oaE '[0-9]+' | tail -1; }
# A WiFi channel, or it isn't one. An operand outside these bands is ABSENT, not "different" —
# without this, any junk number is silently a valid side of the off-channel comparison.
# 2.4 GHz ONLY, deliberately: both operands are 2.4-by-construction (`ap=` is this C3's own STA
# association; mesh_ch is ESP_NOW_FIXED_CHANNEL). A 5 GHz band here was dead code that WIDENED the
# gate it exists to narrow — it accepted 40 as a channel.
valid_ch() {
  case "${1:-}" in ''|*[!0-9]*) return 1;; esac
  [ "$1" -ge 1 ] && [ "$1" -le 14 ]
}

baseline="$(g_all "$ID/ota/state" | tail -1 | ver)"
# Slot at subscribe, so a PASS can show the A/B flip (0→1 or 1→0) rather than a bare index.
baseline_slot="$(g_all "$ID/diag" | tail -1 | grep -oaE '(^|\|)slot=[^|[:space:]]+' | head -1 | cut -d= -f2)"
echo "── ota_verify: id$ID → v$TARGET · window ${WINDOW}s · broker $BROKER · baseline v${baseline:-?} ──"

verdict=""; reason=""; start=$(date +%s); last_off=-1; last_off_t=$start; hwm=0; monotonic=1
total="?"; phase="none"; src="none"; saw_live_prog=0; ota_tok=""

while :; do
  now=$(date +%s)
  mesh_ch="$(g_all mesh/channel | tail -1 | awk -F'\t' '{print $3}' | awk -F'|' '{print $3}')"
  d="$(g_all "$ID/diag" | tail -1)"
  d_live="$(g_live "$ID/diag" | tail -1)"   # DIAG is published RETAINED (wifi.rs:2639)
  # 2026-07-28: ANCHORED + range-checked. `ap=[0-9]+` unanchored matches the TAIL of
  # `heap=42040` (he|ap=…), so this read FREE HEAP and compared it to the mesh channel — a
  # guaranteed mismatch that fired a false OFF-CHANNEL on every run and, being first in the
  # ladder, MASKED every other verdict (incl. the correct DEATH-POINT). It also told the
  # operator to re-channel a live AP. `ap=` is real but CONDITIONAL (appended only once known),
  # so the old read worked on an associated crown and silently read heap on every leaf.
  # NOTE `ap=<ch>:<rssi>:<bssid>` is the TARGET's OWN association, not the crown's — so for a
  # LEAF target (the case this harness exists for) it is absent and this check cannot fire.
  # `cc=` below is the deployed, always-present boolean that actually answers the question.
  ap_ch="$(printf '%s' "$d" | grep -oaE '(^|\|)ap=[0-9]+' | grep -oaE '[0-9]+' | tail -1)"
  valid_ch "$ap_ch"   || ap_ch=""
  valid_ch "$mesh_ch" || mesh_ch=""
  # `cc=` (mode.rs) — firmware's own "crown is co-channel" verdict: 1 = co-channel, 0 = NOT.
  # Prefer a field that states the conclusion over two fields we compare ourselves.
  cc="$(printf '%s' "$d" | grep -oaE '(^|\|)cc=[0-9]+' | grep -oaE '[0-9]+' | tail -1)"
  slot="$(printf '%s' "$d" | grep -oaE '(^|\|)slot=[^|[:space:]]+' | head -1 | cut -d= -f2)"
  rst="$(printf '%s' "$d" | grep -oaE '(^|\|)rst=[^|[:space:]]+' | head -1 | cut -d= -f2)"
  # `ota=` ∈ {none, confirmed, rolled-back} (ota.rs ota_outcome_token). `confirmed` is the REAL
  # OTA proof: boot_confirm() sets it only when the image was New/PendingVerify AND
  # ota_was_activated_for(BUILD_NUMBER) AND self-test passed — and a USB flash takes an explicit
  # exemption and returns WITHOUT marking. That is precisely "this image arrived over our OTA
  # path and passed health check", which `rst` cannot express at all.
  ota_tok="$(printf '%s' "$d" | grep -oaE '(^|\|)ota=[^|[:space:]]+' | head -1 | cut -d= -f2)"
  # …and the same token from a LIVE line only, so a stale `rolled-back` cannot condemn this run.
  # OTA_OUTCOME lives in rtc_fast persistent RAM: only a POWER CYCLE clears it, so it survives
  # every later software reset and keeps being republished retained.
  ota_tok_live="$(printf '%s' "$d_live" | grep -oaE '(^|\|)ota=[^|[:space:]]+' | head -1 | cut -d= -f2)"
  inst_live="$(g_live "$ID/ota/state" | tail -1 | ver)"
  inst_any="$(g_all "$ID/ota/state" | tail -1 | ver)"
  dg="$(g_all "$ID/ota/diag" | tail -1 | awk -F'\t' '{print $3}')"
  # anchored: DIAG carries `tsrc=`, which differs from `src=` only by a prefix. It doesn't collide
  # today solely because its values aren't gw/idN — one rename away from silently matching.
  src="$(printf '%s' "$dg" | grep -oaE '(^| )src=(gw|id[0-9]+)' | tr -d ' ' | tail -1)"; src="${src:-none}"
  pl="$(g_all "$ID/ota/progress" | tail -1 | awk -F'\t' '{print $3}')"
  # progress is RETAINED and published on a ~5 s cadence (wifi.rs:4890), so a subscribe replays
  # the last offset of whatever attempt died LAST — possibly for a different image entirely
  # (compare `total` against the staged size). A retained value never changes, so it satisfies
  # "frozen for STALE seconds" for free and the death-point arm below would condemn a ghost.
  # Require at least one LIVE (retain=0) progress publish first: a genuinely dying transfer
  # publishes live every 5 s before it stops, so this keeps real death-points and drops ghosts.
  [ -n "$(g_live "$ID/ota/progress" | tail -1)" ] && saw_live_prog=1
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
  # ORDER MATTERS AND IS LOAD-BEARING. Anything placed above a check MASKS it, which is exactly
  # how the free-heap OFF-CHANNEL bug hid every real DEATH-POINT. So: hard local blockers first,
  # then a RELIABLE cause, then the observed symptom, and only then inferences from persistent
  # state. `ROLLED BACK` sits BELOW death-point deliberately — its token survives power-cycles in
  # rtc_fast RAM, so it is the most likely thing here to be stale.
  if printf '%s' "$dg" | grep -qaE '(^| )at=slot'; then
    verdict=FAIL; reason="at=slot — local otadata problem (#226); needs a USB flash, OTA can't proceed."; break
  elif [ "$cc" = "0" ]; then
    verdict=FAIL; reason="OFF-CHANNEL — firmware reports cc=0: the crown is NOT co-channel with the ESP-NOW mesh (ch$mesh_ch). Proven OTA blocker — co-channel moved 48KB, off-channel moved 0."; break
  elif [ -z "$cc" ] && [ -n "$mesh_ch" ] && [ -n "$ap_ch" ] && [ "$ap_ch" != "$mesh_ch" ]; then
    # fallback only when cc= is absent (pre-#217 image). `ap=` is the TARGET's own association.
    verdict=FAIL; reason="OFF-CHANNEL — id$ID's own AP ch=$ap_ch != ESP-NOW mesh ch=$mesh_ch (coexist disease; WiFi fetch will stall)."; break
  elif [ "$saw_live_prog" = 1 ] && [ -n "$off" ] && [ -n "$total" ] && [ "$total" != "?" ] && [ "$off" -gt 0 ] && [ "$off" -lt "$total" ] && [ $((now-last_off_t)) -ge "$STALE" ]; then
    verdict=FAIL; reason="DEATH-POINT — offset frozen at $off/$total for ${STALE}s+ (transfer died mid-flight)."; break
  elif [ "$ota_tok_live" = "rolled-back" ]; then
    verdict=FAIL; reason="ROLLED BACK — a LIVE diag reports ota=rolled-back: the board booted a new image, failed its health check, and boot_confirm reverted it. App-side rollback worked as designed; the IMAGE is the problem, not the network."; break
  elif [ "$inst_live" = "$TARGET" ]; then
    # 2026-07-28: the old proof tested `slot = "ota_1"`. Firmware publishes `slot=<0|1>` as a
    # NUMERIC index (mode.rs:3208 ← d.boot_slot) and reset_reason_token() has no `ota` token —
    # an OTA reboot is a software reset, so it reads `rst=sw`. So the PASS arm was unreachable
    # and EVERY genuine OTA fell through to "USB flash, NOT an OTA": the harness's core proof,
    # inverted. Discriminate on what the wire actually carries — `rst=usb-jtag` is the USB
    # tell (a cable reset), and its absence with the target running is over-the-air.
    if [ "$rst" = "usb-jtag" ]; then
      verdict=INFO; reason="reports v$TARGET but rst=usb-jtag → USB flash, NOT an OTA (no over-the-air proof)."; break
    elif [ "$baseline" = "$TARGET" ]; then
      verdict=INFO; reason="already on v$TARGET when we subscribed — no transition observed, so nothing here proves an OTA. Run BEFORE arming to verify a fresh flip."; break
    elif [ "$ota_tok" = "confirmed" ]; then
      verdict=PASS; reason="v${baseline:-?} → v$TARGET · ota=confirmed (boot_confirm: activated by OUR OTA + self-test passed; a USB flash is explicitly exempted and never marks it) · rst=$rst · slot=$slot${baseline_slot:+ (was $baseline_slot)} — real OTA-over-WiFi. source: $src"
      [ "${src#src=id}" != "$src" ] && reason="$reason  ← PEER-SOURCED (#237)"; break
    elif [ -n "$ota_tok" ]; then
      verdict=INFO; reason="reached v$TARGET but ota=$ota_tok, not 'confirmed' — no OTA proof. Note a POWER CYCLE clears the marker (rtc_fast persistent RAM), so a genuine OTA also looks like this after one."; break
    fi   # ota token not yet known — keep waiting for a diag
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
printf '  installed v%s (target v%s) · slot=%s%s · rst=%s · ota=%s\n' "${inst_any:-?}" "$TARGET" "${slot:-?}" "$([ -n "${baseline_slot:-}" ] && [ "${baseline_slot:-}" != "${slot:-}" ] && printf ' (was %s)' "$baseline_slot")" "${rst:-?}" "${ota_tok:-?}"
printf '  offset HWM %s/%s · monotonic=%s · phase=%s%s\n' "$hwm" "$total" "$([ $monotonic = 1 ] && echo yes || echo NO)" "${phase:-none}" "$([ "$saw_live_prog" = 0 ] && printf '  [RETAINED ONLY — no live progress publish this run; offset is a ghost of an earlier attempt, compare total vs the staged size]')"
# "unknown" not "?" — the off-channel check is DISABLED when the AP channel is unpublished, and
# the evidence line must say so rather than imply a value was read and compared.
printf '  crown AP ch=%s · mesh ch=%s · %s\n' "${ap_ch:-unknown (no ap= field in DIAG)}" "${mesh_ch:-?}" "${src:-none}"
printf '  last ota/diag: %s\n' "${dg:-none}"
printf '════════════════════════════════════════════════════════════\n'
[ "$verdict" = "PASS" ] && exit 0 || exit 1
