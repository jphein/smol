#!/usr/bin/env bash
# ota_capture.sh — capture a REPLAYABLE live-OTA fixture from the broker.
#
#   Usage:  ota_capture.sh <board_id> <target_build> [window_s] [outfile]
#   e.g.    ota_capture.sh 51 913 560
#
# WHY THIS EXISTS. `ota_verify.sh` deletes its own raw log on exit (`trap … rm -f "$LOG"`),
# so the first real in-flight OTA anyone observed was destroyed by the harness's own cleanup.
# For a tool whose entire value is post-hoc auditability that is a defect in itself: an
# auditor is left with a summary, and a summary is precisely the thing this project spent a
# day learning not to trust. This writes the raw stream somewhere durable instead.
#
# THREE THINGS A NAIVE CAPTURE GETS WRONG — all measured on 2026-07-28:
#
#  1. NO TIMESTAMPS => THE FIXTURE UNDERSTATES ITSELF. Replayed without elapsed markers every
#     line lands at t=0, so a polling consumer only ever observes the FINAL value of each
#     topic. Measured on the same capture: raw replay gave `restarts=0 monotonic=yes` while a
#     timed replay of the identical bytes gave `restarts=1 monotonic=NO`. Four backward
#     offset moves were present and none were observable. So we emit `@<elapsed_s>` as the
#     first field — REAL seconds since capture start, not synthetic 1 s spacing, because the
#     stall/retry-grace thresholds a consumer tests are wall-clock quantities.
#
#  2. AGGREGATE READS MASQUERADE AS SEQUENCES. `grep -oE 'boot=[0-9]+' file | sort -u` over a
#     multi-topic capture yields "13, 14" and looks like an increment; it was two different
#     BOARDS. Per-field summaries must be scoped per topic. The --summary mode below does that,
#     and prints the topic beside every value for exactly this reason.
#
#  3. THE CROWN IS NOT A CONSTANT. Crown tenure was observed changing hands THREE times in a
#     nine-minute window (MC|5 → MC|50 → MC|8), which invalidated three separate measurements
#     that sampled it once at the start. `smol/mesh/channel` is therefore captured in the SAME
#     stream, so a fixture records the confounder instead of being silently ruined by it.
#
# Retain flag is `%r`. `%R` is NOT a mosquitto specifier — it expands to empty, which silently
# inverts every is-this-live test. Credentials never reach argv (#313): they go in a 0700
# XDG_CONFIG_HOME. Always `grep -a` when reading the result: one binary byte flips grep to
# binary mode and it prints nothing at all.
set -uo pipefail

ID="${1:?usage: ota_capture.sh <board_id> <target_build> [window_s] [outfile]}"
TARGET="${2:?target build, e.g. 913}"
WINDOW="${3:-560}"
OUT="${4:-$HOME/.smol-ota-fixtures/ota-$ID-$TARGET-$(date -u +%Y%m%dT%H%M%SZ).log}"

case "$ID" in ''|*[!0-9]*) echo "board_id must be numeric" >&2; exit 2;; esac

OTA_ENV="$HOME/Projects/smol/tools/ota_publish.env"; [ -f "$OTA_ENV" ] && . "$OTA_ENV"
BROKER="${BROKER:-10.0.0.1}"; MQTT_USER="${MQTT_USER:-<mqtt-user>}"; ADDON="${ADDON:-<addon-slug>}"

tok="$(timeout 40 bw get password ha-llat 2>/dev/null)" || true
PW="$(HA_TOKEN="$tok" timeout 40 python3 "$HOME/Projects/ha/tools/ha_supervisor.py" \
      GET "/addons/$ADDON/info" 2>/dev/null \
      | python3 -c "import sys,json;print(json.load(sys.stdin)['options']['mqtt_password'])" 2>/dev/null)" || true
# An empty password must FAIL LOUDLY. Silently capturing zero lines reads identically to
# "the fleet was quiet", which cost real time earlier today.
[ -n "$PW" ] || { echo "FATAL: could not source the mqtt password (bw locked? addon unreachable?)" >&2; exit 3; }

CFG="$(mktemp -d)"; chmod 700 "$CFG"
printf -- '-h %s\n-u %s\n-P %s\n' "$BROKER" "$MQTT_USER" "$PW" > "$CFG/mosquitto_sub"
chmod 600 "$CFG/mosquitto_sub"
trap 'rm -rf "$CFG"' EXIT

mkdir -p "$(dirname "$OUT")" || { echo "FATAL: cannot create $(dirname "$OUT")" >&2; exit 2; }

{ echo "# smol live-OTA fixture — id$ID → v$TARGET — $(date -u +%Y-%m-%dT%H:%M:%SZ) — window ${WINDOW}s"
  echo "# format: @<elapsed_s>\\t<retain>\\t<topic>\\t<payload>   (retain=1 lines are the pre-existing"
  echo "#         retained baseline delivered at subscribe; retain=0 lines are LIVE)"
  echo "# replay: feed to a consumer that honours @<elapsed>, or the capture understates itself"
} > "$OUT"

echo "capturing id$ID → v$TARGET for ${WINDOW}s → $OUT" >&2
START=$(date +%s)
XDG_CONFIG_HOME="$CFG" timeout "$((WINDOW + 20))" mosquitto_sub -p 1883 \
  -i "otacap_${ID}_$$" -F '%r\t%t\t%p' -W "$WINDOW" \
  -t "smol/$ID/diag" -t "smol/$ID/ota/state" -t "smol/$ID/ota/progress" \
  -t "smol/$ID/ota/diag" -t "smol/$ID/ota/armdiag" -t "smol/$ID/status" \
  -t 'smol/mesh/channel' -t 'smol/ota/staged' 2>/dev/null \
| while IFS= read -r line; do
    printf '@%d\t%s\n' "$(( $(date +%s) - START ))" "$line"
  done >> "$OUT"

n=$(grep -ac $'\t' "$OUT" || true)
live=$(grep -ac $'^@[0-9]*\t0\t' "$OUT" || true)
echo "captured ${n:-0} line(s), ${live:-0} LIVE (retain=0) → $OUT" >&2

# ── per-topic summary — scoped, because an aggregate read as a sequence is how a two-board
#    boot= pair got reported as an increment.
echo
echo "── per-topic field summary (scoped; an aggregate is NOT a sequence) ──"
for suffix in diag ota/state ota/progress ota/diag; do
  t="smol/$ID/$suffix"
  echo "  $t"
  grep -a $'\t'"$t"$'\t' "$OUT" | tail -3 | sed 's/^/      /' || echo "      (none)"
done
echo "  boot= seen ON THIS BOARD only:"
grep -a $'\t'"smol/$ID/diag"$'\t' "$OUT" | grep -oaE '\|boot=[0-9]+' | uniq | tr '\n' ' ' | sed 's/^/      /'; echo
echo "  crown tenure across the window (a variable, not a constant):"
grep -a 'smol/mesh/channel' "$OUT" | grep -oaE 'MC\|[0-9]+' | uniq | tr '\n' ' ' | sed 's/^/      /'; echo
