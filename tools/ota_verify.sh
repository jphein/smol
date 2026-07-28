#!/usr/bin/env bash
# smol OTA-roll verify harness — PASS / FAIL for one board's OTA install.
#
#   Usage:  ota_verify.sh <board_id> <target_build> [window_s]
#   e.g.    ota_verify.sh 8 907 360
#   Exit:   0 = PASS · 1 = FAIL / UNPROVEN / CONFLICT · 3 = setup error (no creds) · 4 = bad invocation
#           All three failure codes are DISTINCT deliberately, and the history is instructive:
#             3  the ENVIRONMENT is wrong (bw locked, addon unreachable) — retry later.
#             4  the CALLER is wrong (retired knob, racy threshold) — edit the command.
#             2  DELIBERATELY UNUSED: bash returns 2 for a SYNTAX ERROR, so a script that exits 2 is
#                indistinguishable from a half-written one. This guard used 3 for a revision (an
#                auditor read a caller error as a broken environment), then 2 (an auditor was
#                simultaneously seeing real bash-syntax 2s from a mid-edit file). Two conflations in
#                the same field; 4 collides with neither.
#   Test:   tools/test_ota_verify.sh   (fixture replay, no broker, no hardware)
#
# ═════════════════════════════════════════════════════════════════════════════════════════
# THE INVARIANT — the whole design, and the reason this file was RESTRUCTURED on 2026-07-28
#
#     Conclude only from TRANSITIONS OBSERVED LIVE INSIDE THIS RUN'S WINDOW.
#     Never from a state value that was merely read.
#     And never while the observed process is still making progress attempts.
#
# That third clause is the TIME axis, and it cost a false FAIL on an OTA that WORKED. A teammate
# armed a real 907 install on id8 and watched it succeed; this harness reported
# `FAIL — DEATH-POINT — offset frozen at 1179648/1440528`, having broken out of a 600 s window at
# ~60 s and never seen the PASS that landed 3.5 minutes later. The stall reading was CORRECT; the
# conclusion that a stall is TERMINAL was not. The relay path retries — that same successful run
# logged `relay-failed retry=1`, `leaf-timeout retry=2`, `relay-failed retry=3` and restarted from
# offset 0 twice. So in this system a stall is a RETRY, not a death, and `break`ing at the first one
# guarantees the verdict describes an intermediate state.
#
# Consequence, implemented below: NO failure finding ends the run. Only a completion PROOF breaks
# early. Everything else is recorded, re-evaluated every poll, and reported only if it is STILL
# STANDING when the window expires — so a later PASS overrides it, and the caller's window budget is
# actually spent on the process it was sized for. The cost is that a non-PASS run now always takes
# the full window; that is what asking for a 600 s window MEANS. Pass a shorter one for a quick read.
#
# In this system almost every operand is either a RETAINED MQTT topic or lives in rtc_fast
# persistent RAM. Both mean the same thing to a reader: **a value tells you what is true, never
# WHEN it became true.** `ota/state`, `ota/diag`, `ota/progress` and `<id>/diag` are all retained,
# so a fresh subscribe replays yesterday's truth with today's timestamp; `ota=confirmed` lives in
# `#[esp_hal::ram(rtc_fast, persistent)] OTA_OUTCOME` (ota.rs:2081-2083) and survives every
# software reset — only a power cycle clears it.
#
# Three rounds of incremental patching failed an adversarial audit three times, because every
# defect was one of two structural mistakes, not a local bug:
#
#   1. VERDICTS COMPUTED FROM VALUE SNAPSHOTS. Each poll re-read `tail -1` of a topic and compared
#      it to a constant. A snapshot cannot distinguish "this became true just now" from "this has
#      been true since a boot last week", which is exactly the question the harness exists to
#      answer. Fix: an OBSERVATION LEDGER — for every operand keep (first observation, last LIVE
#      observation) and conclude only from the DIFFERENCE between them.
#   2. A FIRST-MATCH-WINS VERDICT LADDER. Any arm placed above another masked it; the same slot
#      masked a genuine death-point FOUR separate times. Fix: EVERY check is evaluated EVERY poll
#      and EVERY finding is printed. Ordering now selects only the HEADLINE, so a wrong or
#      over-eager check can no longer delete the evidence for a right one. (Tradeoff: the output is
#      longer, and the headline is still a judgement call. But the operator always sees the full
#      set, so a misranked headline costs a glance, not a masked failure.)
#
# Corollaries, each of which was a reported defect:
#   * Every FAIL arm requires a LIVE (retain=0) message. `%r` is the retain flag. `%R` is NOT a
#     mosquitto specifier — it silently expands to the empty string, which inverted the entire
#     retained-ghost discipline for one earlier round (proved: `-F '[%R]'` → `[]`, `-F '[%r]'` → `[1]`).
#   * A MISSING OPERAND IS NEVER A VERDICT. It prints `unknown` and the check is SKIPPED. An absent
#     field must never be readable as a value (an empty baseline used to satisfy `baseline != TARGET`
#     and produced a false PASS reading `v? → v907`).
#   * A CHECK THAT CANNOT WORK SAYS SO, rather than sitting present-but-broken. See OFF-CHANNEL on a
#     leaf target below: it is inapplicable today and prints that fact.
#   * EVERY VERDICT PRINTS THE OPERANDS IT FIRED ON, so an operator can audit it without a rerun.
#
# ── SCHEMA: THERE ARE TWO PRODUCERS, WITH DIVERGENT VOCABULARIES ─────────────────────────
# Verified against live payloads and source on 2026-07-28. Do not "simplify" this into one schema.
#
#   A) smol C3 firmware (this repo) — rust/clock/src/net/mode.rs:3325 `DIAG|slot=…|rst=…|boot=…|ota=…`
#        slot=<0|1>        NUMERIC boot-slot index (← d.boot_slot). Never the string `ota_1`.
#        rst=<panic|power-on|sw|deep-sleep|brownout|wdt|usb-jtag|glitch|other|unk>
#                          reset_reason_token() (ota.rs:1761) emits NO `ota` token — an OTA reboot
#                          is a SOFTWARE reset and reads `rst=sw`.
#        boot=<n>          boot counter, increments every boot.
#        ota=<none|confirmed|rolled-back>   ota_outcome_token(); rtc_fast persistent (see above).
#        cc=<0|1|2>        #217 coexist health. THREE-VALUED since 6a62946 (2026-07-28):
#                          1 = associated AND co-channel · 0 = associated AND off-channel
#                          2 = NOT ASSOCIATED (nothing to conclude).
#                          Older DEPLOYED images are TWO-valued and fold "not associated" into 0
#                          (`unwrap_or(0)`), so a bare `cc=0` from the fleet is AMBIGUOUS. This
#                          harness therefore acts on cc=0 only with independent association proof.
#        ap=<ch>:<rssi>:<bssid>  the TARGET's OWN current association — NOT the crown's. Emitted
#                          from the same `current_ap_info()` as `cc`, but AFTER it, so truncation
#                          drops `ap=` while keeping `cc=`. CONDITIONAL: absent when unassociated.
#        cut=<bytes>       mode.rs truncation marker: this record lost <bytes> off its tail.
#
#   B) esp32c6-watch (SEPARATE REPO, READ-ONLY here — remote is `wakizashi` only; cite, never push)
#        ~/Projects/esp32c6-watch/src/main.rs:2651 emits a CONSTANT prefix:
#        `DIAG|slot=ota_0|rst=unknown|boot=0|ota=none|…`
#        So `slot=ota_0` IS published on this fleet — an earlier header asserted it never was and
#        was wrong. For a C6 target, slot/boot/ota are hardcoded and CANNOT transition, which makes
#        the OTA proof below structurally unreachable. The harness detects this and says so instead
#        of reporting a failed OTA.
#
# ── WHY THE OFF-CHANNEL CHECK IS INAPPLICABLE FOR THE CASE THIS HARNESS EXISTS FOR ───────
# A leaf's DIAG reaches the gateway inside ONE ESP-NOW frame, capped at RELAY_VALUE_MAX = 232 B
# (wifi.rs:1857). A realistic leaf record is ~307 B offered, so the tail is cut on a field boundary
# and `cc=` and `ap=` — both at the tail — are the first casualties. Measured on the live fleet:
# id5 `len=340 cc=1 ap=6` (crown, self-published over MQTT, full record) vs id8/50/51/122/236 all
# `cc=- ap=-`. So for a LEAF OTA the off-channel check CANNOT FIRE. That is printed as an explicit
# `unknown`, never silently skipped. When two-frame leaf records land `cc=` in leaf DIAGs, a
# two-valued `cc=0` would fire a false OFF-CHANNEL fleet-wide — which is precisely why the
# association requirement below must stay even after every image speaks three-valued `cc`.
#
# ── OTHER HARD-WON LESSONS (kept: each cost a real misdiagnosis) ─────────────────────────
#   * grep -a EVERYWHERE. One binary byte in a payload flips grep to binary mode and it silently
#     prints nothing; a waiter then reads "no event" while the event sits in the log.
#   * ANCHOR EVERY FIELD READ on `(^|\|)`. Unanchored `ap=[0-9]+` matches the TAIL of `heap=42040`
#     (he|ap=…), so the harness compared FREE HEAP to the mesh channel — a guaranteed mismatch that
#     fired a false OFF-CHANNEL on every run, masked every real verdict, and sent an operator to
#     re-channel a healthy AP. Same hazard: `src=` vs `tsrc=`, `ota=` vs `otah=`, `cc=` vs `cdeaf=`.
#   * A CONDITIONAL FIELD IS MORE DANGEROUS THAN AN ABSENT ONE — it buys broken code an alibi: the
#     unanchored `ap=` read was correct on the one associated crown anybody looks at and silently
#     wrong on every leaf.
#   * USB vs OTA: `installed_version` reaching the target is NOT proof. `rst=usb-jtag` is the cable
#     tell, and a USB flash is explicitly EXEMPTED from setting `ota=confirmed`.
#   * DEATH-POINT: offset frozen >STALL_AFTER with 0<done<total AND no live retry signal inside
#     RETRY_GRACE = the transfer died AT that byte. With a fresh retry signal it is a STALL, not a death.
#   * PEER-SOURCE (#237): ota/diag ` src=id<n>` = a peer HOLDER served it over ESP-NOW (vs `src=gw`).
#   * The broker password NEVER reaches argv. `-P "$PW"` published it in the process table for the
#     whole window and one agent read another's out of `ps`. It now goes in a private config file.
set -uo pipefail

ID="${1:?usage: ota_verify.sh <board_id> <target_build> [window_s]}"
TARGET="${2:?target build number, e.g. 907}"
# 600 s default, not 360: a real leaf OTA RETRIES. The measured 907→id8 run stalled, logged
# `relay-failed retry=1` / `leaf-timeout retry=2` / `relay-failed retry=3`, RESTARTED FROM OFFSET 0
# twice, and completed ~3.5 minutes after the first stall. A window shorter than that budget reports
# an intermediate state as an outcome.
WINDOW="${3:-600}"
# ── TWO thresholds, deliberately TWO knobs ─────────────────────────────────────────────────────
# These both default from the same measured number today and they currently agree. They are split
# anyway, because ONE KNOB WITH TWO SEMANTICS is the exact conflation pattern behind most of this
# file's defects: `cc=0` meaning off-channel OR unassociated; `ota=confirmed` meaning "a build was
# confirmed" OR "THIS build was". Each was harmless until the two meanings diverged, and then it was
# a silent wrong answer. Someone must be able to move one of these without moving the other WITHOUT
# NOTICING THEY DID.
#
# They measure different planes, and would diverge for different reasons:
#
#   STALL_AFTER — the DATA plane, measured by US. How long the offset may sit unchanged (with
#     0<off<total) before we call the transfer stalled. Derived FROM THE DATA PLANE: an active
#     transfer republishes progress every 5 s (`Duration::from_secs(5)`, wifi.rs:5368), so 30 s is
#     SIX consecutive missed publishes.
#     It was 150 s for one revision — the retry interval's number wearing a data-plane name, i.e. the
#     mechanism was split but the CALIBRATION was still conflated (caught by oracle-verify). Deriving
#     it from the publish cadence instead is what makes a genuine death detectable INSIDE a 600 s
#     window rather than at its very end. Tightening is nearly free here for two independent reasons:
#     DEATH-POINT is non-terminal, so a premature one is overridden by a later completion; and
#     RETRY_GRACE forgives any stall the board is announcing retries through. That is the split
#     paying off — neither protection existed when 30 s was previously wrong.
#
#   RETRY_GRACE — the CONTROL plane, stated by the BOARD. How long a `retry=`/`leaf-timeout`/
#     `relay-failed` on ota/diag keeps a stall non-terminal. Derivation: it must exceed the longest
#     gap between the board's retry ANNOUNCEMENTS, which is a different quantity from the gap between
#     progress publishes — a relay can announce once per failed attempt while publishing progress
#     every few seconds during one. Same 58 s observation, different reason for depending on it.
#
# If the relay's cadence changes, re-derive them SEPARATELY.
STALL_AFTER="${OTA_VERIFY_STALL_AFTER:-30}"
RETRY_GRACE="${OTA_VERIFY_RETRY_GRACE:-150}"
# BARREN_STALLS — how many CONSECUTIVE stall episodes with NO high-water advance between them mean
# the board is thrashing rather than progressing. Derivation: the observed successful run stalled and
# restarted from 0 TWICE and its high-water still advanced (0 → 1440528), so "no advance across two
# consecutive stalls" is outside anything healthy that has been observed. This is a COUNT of repeated
# non-progress, not a timer — deliberately, because the time axis is what the retry grace already
# covers and a thrashing board can retry forever without a timer ever expiring.
BARREN_STALLS="${OTA_VERIFY_BARREN_STALLS:-2}"
# A threshold at or below the poll interval is decided by SCHEDULING JITTER, not by the system under
# observation. `retry_fresh` asks "was a retry seen within RETRY_GRACE" — with RETRY_GRACE <= POLL
# that question is answered by which side of a sleep the loop happens to land on, and it made this
# file's own independence test 60% non-deterministic (oracle-verify measured 3/5 wrong). Note the
# asymmetry, because it generalises: PRESENCE-of-signal survives coarse polling (the signal is still
# there next poll) while ABSENCE-of-signal does not. So refuse the racy configuration outright rather
# than widen a margin and hope — the same reasoning that deleted the mesh_ch arm instead of tuning it.

# A retired knob that silently does nothing is the same trap as an absent field read as a value: the
# caller believes they set a threshold and gets the default. Fail loudly instead of ignoring it.
if [ -n "${OTA_VERIFY_STALE:-}" ]; then
  echo "FATAL: OTA_VERIFY_STALE is retired — it conflated two thresholds. Set OTA_VERIFY_STALL_AFTER" >&2
  echo "       (offset-frozen threshold, data plane) and/or OTA_VERIFY_RETRY_GRACE (how long the" >&2
  echo "       board's own retry signal keeps a stall non-terminal, control plane). See the header." >&2
  exit 4   # NOT 3 (environment) and NOT 2 (bash's own syntax-error code).
fi
POLL="${OTA_VERIFY_POLL:-3}"       # re-evaluate every N s
SETTLE="${OTA_VERIFY_SETTLE:-2}"   # let the retained baseline land before the first evaluation
FIXTURE="${OTA_VERIFY_FIXTURE:-}"  # test seam: replay a canned log instead of subscribing

# Spelled out, not looped through `eval` — indirect assignment hides the name from shellcheck and
# from a reader, and this file has already paid for that once.
require_above_poll() { # require_above_poll <knob-name> <value>
  [ "$2" -gt "$POLL" ] && return 0
  echo "FATAL: $1=$2 must be GREATER than POLL=$POLL — at or below the poll interval this" >&2
  echo "       threshold is decided by scheduling jitter rather than by the board." >&2
  echo "       Raise $1 or lower OTA_VERIFY_POLL." >&2
  exit 4
}
require_above_poll STALL_AFTER "$STALL_AFTER"
require_above_poll RETRY_GRACE "$RETRY_GRACE"

# ═══ message source ══════════════════════════════════════════════════════════════════════
# Log format is `<retain>\t<topic>\t<payload>`, one message per line, in ARRIVAL ORDER. The
# retain flag is load-bearing: it is the only thing separating "the broker replayed history" from
# "this board just said something".
if [ -n "$FIXTURE" ]; then
  # Fixture replay. A line may be prefixed `@<seconds>\t` to be emitted that many seconds after
  # replay starts; unprefixed lines land at t=0 (i.e. in the retained-at-subscribe batch). This is
  # what makes TRANSITIONS testable — the previous test copy read a static file, so "first
  # observation" and "last observation" were always the same line and no transition could ever be
  # exercised. A harness whose tests cannot express its central concept is not tested.
  [ -f "$FIXTURE" ] || { echo "FATAL: fixture not found: $FIXTURE" >&2; exit 4; }
  BROKER="fixture($(basename "$FIXTURE"))"
  LOG="$(mktemp "/tmp/ota_verify_fix_${ID}_XXXX.log")"
  (
    t0=$(date +%s)
    while IFS= read -r line; do
      case "$line" in
        @*) at="${line%%$'\t'*}"; at="${at#@}"; rest="${line#*$'\t'}" ;;
        *)  at=0; rest="$line" ;;
      esac
      while [ $(( $(date +%s) - t0 )) -lt "$at" ]; do sleep 0.2; done
      printf '%s\n' "$rest" >> "$LOG"
    done < "$FIXTURE"
  ) &
  SUB=$!
  trap 'kill "$SUB" 2>/dev/null; rm -f "$LOG"' EXIT
else
  # ---- broker creds (mirrors tools/ota_publish.sh) --------------------------------------
  OTA_ENV="$HOME/Projects/smol/tools/ota_publish.env"
  # shellcheck source=/dev/null
  [ -f "$OTA_ENV" ] && . "$OTA_ENV"
  BROKER="${BROKER:-10.0.0.1}"; MQTT_USER="${MQTT_USER:-<mqtt-user>}"; ADDON="${ADDON:-<addon-slug>}"
  PW="$(timeout 25 bash -c 'tok=$(bw get password ha-llat 2>/dev/null) || exit 1
    HA_TOKEN="$tok" python3 "$HOME/Projects/ha/tools/ha_supervisor.py" GET "/addons/'"$ADDON"'/info" 2>/dev/null \
    | python3 -c "import sys,json;print(json.load(sys.stdin)[\"options\"][\"mqtt_password\"])" 2>/dev/null')"
  [ -n "$PW" ] || { echo "FATAL: could not source mqtt password (bw locked? addon $ADDON unreachable?)" >&2; exit 3; }

  # The password goes in a PRIVATE config file, never in argv. mosquitto_sub reads
  # `$XDG_CONFIG_HOME/mosquitto_sub` (one `-option value` per line) as if those options preceded the
  # command line — verified against mosquitto_sub 2.0.18 by planting an unknown option there and
  # watching it error. We point XDG_CONFIG_HOME at a private 0700 tmpdir rather than writing
  # $HOME/.config/mosquitto_sub, because that file is SHARED user-global state: clobbering it would
  # break other tooling and race every concurrent agent on this host. PW is never exported (so it
  # stays out of /proc/*/environ) and never printed.
  umask 077
  CFGDIR="$(mktemp -d "/tmp/ota_verify_cfg_${ID}_XXXX")"
  printf -- '-P %s\n' "$PW" > "$CFGDIR/mosquitto_sub"
  LOG="$(mktemp "/tmp/ota_verify_${ID}_XXXX.log")"
  XDG_CONFIG_HOME="$CFGDIR" mosquitto_sub -h "$BROKER" -p 1883 -u "$MQTT_USER" \
    -i "ota_verify_${ID}_$$" -F '%r\t%t\t%p' \
    -t "smol/$ID/ota/progress" -t "smol/$ID/ota/diag" -t "smol/$ID/ota/state" \
    -t "smol/$ID/diag" -t "smol/mesh/channel" > "$LOG" 2>&1 &
  SUB=$!
  trap 'kill "$SUB" 2>/dev/null; rm -f "$LOG"; rm -rf "$CFGDIR"' EXIT
  # The trap is not enough. `mosquitto_sub` reads its config ONCE at startup and keeps the credential
  # in memory, so the file only needs to exist for that instant — and leaving it for the whole window
  # means a SIGKILL (this host has a documented OOM-scope-kill history that has taken whole agent trees)
  # skips the EXIT trap and strands a plaintext password in /tmp. So delete it as soon as it has been
  # read, and keep the settle wait AFTER that. The floor of 2 s is the read guard: below it we would be
  # racing the child's own startup, which is why SETTLE is clamped here rather than trusted.
  [ "$SETTLE" -lt 2 ] && SETTLE=2
  sleep 2
  rm -f "$CFGDIR/mosquitto_sub"
fi
sleep "$SETTLE"

# ═══ readers ═════════════════════════════════════════════════════════════════════════════
# g_all = every observation of a topic (any retain flag) · g_live = LIVE ones only (retain=0).
g_all()  { grep -a  $'\t'"smol/$1"$'\t' "$LOG"; }
g_live() { grep -a "^0"$'\t'"smol/$1"$'\t' "$LOG"; }
pay()    { cut -f3-; }   # payload column (cut's default delimiter is TAB; -f3- keeps any tabs within)
ver()    { grep -oaE '"installed_version":"[0-9]+"' | grep -oaE '[0-9]+' | tail -1; }
# Field read out of one DIAG line. ANCHORED on `(^|\|)` — see the header; unanchored reads have
# produced two separate false verdicts in this file's history.
fld()    { printf '%s' "${2:-}" | grep -oaE "(^|\|)$1=[^|[:space:]]+" | head -1 | cut -d= -f2; }
# First / last-LIVE observation of a DIAG line that actually CARRIES the field. Selecting on the
# field (not just on the topic) means a record that lost the field to truncation cannot blank an
# operand we already observed — absence and change stay distinguishable.
d_first() { g_all  "$ID/diag" | grep -aE "(^|\|)$1=" | head -1; }
d_live()  { g_live "$ID/diag" | grep -aE "(^|\|)$1=" | tail -1; }
# A WiFi channel, or it isn't one. An operand outside 1-14 is ABSENT, not "different" — otherwise
# any junk number is silently a valid side of the off-channel comparison. 2.4 GHz only, deliberately:
# both operands are 2.4-by-construction (`ap=` is this C3's own STA association, `mesh_ch` is
# ESP_NOW_FIXED_CHANNEL). A 5 GHz branch here was dead code that WIDENED the gate it exists to
# narrow — it accepted 40 as a channel.
valid_ch() {
  case "${1:-}" in ''|*[!0-9]*) return 1;; esac
  [ "$1" -ge 1 ] && [ "$1" -le 14 ]
}
is_num() { case "${1:-}" in ''|*[!0-9]*) return 1;; esac; }
# `at=slot` at start-of-payload or after a space — the anchored equivalent of the old
# `grep -qaE '(^| )at=slot'`, but WITHOUT A PIPELINE, and that is a correctness fix, not tidying.
#
# `printf '%s' "$x" | grep -q PATTERN` under `set -o pipefail` returns NON-ZERO roughly 0.2% of the
# time EVEN WHEN THE PATTERN MATCHES: `grep -q` exits on first match without draining, the writer
# takes EPIPE, and pipefail surfaces the WRITER's failure as the pipeline's. Measured 3 spurious
# non-matches in 1500 runs on real output, position-independent; `case` was 0 in 1500. Anywhere a
# `grep -q` STATUS is a verdict decision, that is a ~0.2%-per-poll chance of a check silently NOT
# FIRING — exactly the failure mode this whole file exists to eliminate, arriving through the shell
# rather than the data. Every other grep here feeds a command substitution where the OUTPUT is what
# matters and no reader exits early, so this hazard is specific to `-q`.
has_at_slot() { case "${1:-}" in "at=slot"*|*" at=slot"*) return 0;; esac; return 1; }

echo "── ota_verify: id$ID → v$TARGET · window ${WINDOW}s · stall-after ${STALL_AFTER}s · retry-grace ${RETRY_GRACE}s · broker $BROKER ──"

start=$(date +%s); last_off=-1; last_off_t=$start; hwm=""; monotonic=1; saw_live_prog=0
findings=(); pass_ok=0; fail_n=0; headline=""; verdict=""
# Retry-signal freshness. `retry_t` is when the board LAST TOLD US it is still trying; epoch 0 means
# never, so `now - retry_t` is never "recent" until a retry is actually observed. Counted by LINES,
# not by payload change: a board republishing the identical `relay-failed retry=1` is emitting a
# fresh signal, and comparing payload strings would discard it.
retry_t=0; retry_n=0
# Per-image progress state. A CHANGED `total` IS A DIFFERENT IMAGE, so all offset-derived state must
# reset with it — otherwise a high-water mark accumulates ACROSS images and the report pairs one
# image's offset with another's total (observed: hwm 1277952 from a 906 whose total was 1435280,
# printed against 907's total of 1440528).
prev_total=""; images=0; restarts=0; stalls=0; stall_at=""; stall_latched=0
# High-water at the previous stall episode, and how many consecutive episodes have shown no advance
# since. These were computed and PRINTED but consulted by nothing, so a board stuck in a retry loop
# reported "still in progress, re-run with a longer window" forever and the operator's only exit was
# judgement. A distinguishable failure with no check behind it is not a reported failure.
hwm_at_stall=""; barren=0

# add <severity> <rank> <code> <text>   — rank orders the HEADLINE only; every finding prints.
add() { findings+=("$1"$'\t'"$2"$'\t'"$3"$'\t'"$4"); }

while :; do
  now=$(date +%s)
  findings=(); pass_ok=0; fail_n=0

  # ═══ observation ledger ════════════════════════════════════════════════════════════════
  # Baselines are the FIRST observation (retained is fine — that is legitimately "the state when we
  # arrived"). Conclusions come only from the LAST LIVE observation differing from it.
  st_first="$(g_all  "$ID/ota/state" | head -1 | ver)"
  st_live="$( g_live "$ID/ota/state" | tail -1 | ver)"
  # Spelled out rather than looped through `printf -v`: indirect assignment hides these names from
  # both shellcheck and a reader, and a typo in the loop list would silently leave an operand empty —
  # which in this file means "check skipped", the one failure mode that must never be accidental.
  slot_first="$(fld slot "$(d_first slot)")"; slot_live="$(fld slot "$(d_live slot)")"
  boot_first="$(fld boot "$(d_first boot)")"; boot_live="$(fld boot "$(d_live boot)")"
  ota_first="$( fld ota  "$(d_first ota)")";  ota_live="$( fld ota  "$(d_live ota)")"
  cut_first="$( fld cut  "$(d_first cut)")";  cut_live="$( fld cut  "$(d_live cut)")"
  rst_live="$(  fld rst  "$(d_live rst)")"
  up_live="$(   fld up   "$(d_live up)")"
  cc_live="$(   fld cc   "$(d_live cc)")"
  # `ap=` must come from the SAME live record as `cc=`: association is only evidence for the
  # co-channel verdict if both were true at the same instant on the same board.
  cc_line="$(d_live cc)"
  ap_cc="$(printf '%s' "$(fld ap "$cc_line")" | cut -d: -f1)"; valid_ch "$ap_cc" || ap_cc=""
  ap_live="$(printf '%s' "$(fld ap "$(d_live ap)")" | cut -d: -f1)"; valid_ch "$ap_live" || ap_live=""
  # mesh channel: a compile-time fleet constant (ESP_NOW_FIXED_CHANNEL), so a retained value is
  # epistemically fine here — it cannot have "become" something else per boot. Stated openly
  # because it is the ONE operand below not required to be live, and any FAIL that leans on it says so.
  mesh_ch="$(g_all mesh/channel | tail -1 | pay | awk -F'|' '{print $3}')"; valid_ch "$mesh_ch" || mesh_ch=""

  dg_live="$(g_live "$ID/ota/diag" | tail -1 | pay)"
  dg_any="$( g_all  "$ID/ota/diag" | tail -1 | pay)"
  # anchored: ota/diag carries `tsrc=`, one prefix away from silently matching `src=`.
  src="$(printf '%s' "$dg_any" | grep -oaE '(^| )src=(gw|id[0-9]+)' | tr -d ' ' | tail -1)"; src="${src#src=}"

  # The freeze clock must run on LIVE progress only. Reading the offset from `tail -1` of ALL
  # progress observations while merely checking that SOME live publish existed would let a retained
  # ghost — which never changes, and so satisfies "frozen" for free — supply the offset that a
  # verdict fires on. `pl_any` is kept for the evidence line and for saying "retained only", never
  # for the death-point arm.
  pl_any="$( g_all  "$ID/ota/progress" | tail -1 | pay)"
  pl_live="$(g_live "$ID/ota/progress" | tail -1 | pay)"
  [ -n "$pl_live" ] && saw_live_prog=1
  off="$( printf '%s' "$pl_live" | cut -d'|' -f1)"; total="$(printf '%s' "$pl_live" | cut -d'|' -f2)"
  phase="$(printf '%s' "$pl_live" | cut -d'|' -f3)"; is_num "$off" || off=""
  off_any="$(  printf '%s' "$pl_any" | cut -d'|' -f1)"; is_num "$off_any" || off_any=""
  total_any="$(printf '%s' "$pl_any" | cut -d'|' -f2)"

  # ── the board's own "I have not given up" signal, preferred over any timer of ours ──────
  # ota/diag carries `retry=<n>` / `leaf-timeout` / `relay-failed` LIVE while the relay is retrying.
  rn="$(g_live "$ID/ota/diag" | grep -acE 'retry=[0-9]+|leaf-timeout|relay-failed')"
  is_num "$rn" || rn=0
  if [ "$rn" -gt "$retry_n" ]; then retry_n="$rn"; retry_t="$now"; fi
  # Control plane: how long the board's own "still trying" statement remains fresh.
  retry_fresh=0; [ "$retry_t" != 0 ] && [ $((now-retry_t)) -le "$RETRY_GRACE" ] && retry_fresh=1

  # ── a changed `total` is a different image: reset every offset-derived statistic ────────
  if [ -n "$total" ] && [ "$total" != "$prev_total" ]; then
    if [ -n "$prev_total" ]; then
      images=$((images+1))
      hwm=""; last_off=-1; last_off_t="$now"; monotonic=1; restarts=0; stalls=0; stall_at=""; stall_latched=0
      hwm_at_stall=""; barren=0
    fi
    prev_total="$total"
  fi

  if [ -n "$off" ]; then
    { [ -z "$hwm" ] || [ "$off" -gt "$hwm" ]; } && hwm="$off"
    # A HIGH-WATER ADVANCE CLEARS THE BARREN STREAK, IMMEDIATELY — not at the next stall episode.
    # `barren` is history; the RETRY-LOOP verdict is a claim about NOW. Consulting an accumulated
    # counter as if it were a current condition is the same mistake as reading a retained value as a
    # current one, which is this file's founding defect — and it bit here: a transfer that stalled
    # barrenly and then SUCCEEDED still had barren=2 at the final poll, so a completed OTA reported
    # `CONFLICT` with exit 1 and the self-refuting text "HWM stuck at 1440528/1440528".
    if [ -n "$hwm_at_stall" ] && [ -n "$hwm" ] && [ "$hwm" -gt "$hwm_at_stall" ]; then
      barren=0; hwm_at_stall="$hwm"
    fi
    if [ "$off" != "$last_off" ]; then
      if [ "$last_off" != -1 ] && [ "$off" -lt "$last_off" ]; then
        # A RESTART is not corruption. The relay legitimately re-fetches from 0 after a failed
        # attempt, and #267 resume can re-enter at a checkpoint > 0 — so "went backwards" is only a
        # REGRESSION when the board is NOT telling us it retried. Using the board's signal to
        # classify our own measurement is the same discipline as `cc=0` needing `ap=`.
        if [ "$off" = 0 ] || [ "$retry_fresh" = 1 ]; then restarts=$((restarts+1)); else monotonic=0; fi
      fi
      last_off="$off"; last_off_t="$now"; stall_latched=0
    fi
    # Count stall EPISODES (latched), so the evidence can say "stalled twice and recovered" instead
    # of implying one endless freeze.
    if [ "$stall_latched" = 0 ] && [ "$off" -gt 0 ] && is_num "$total" && [ "$off" -lt "$total" ] \
       && [ $((now-last_off_t)) -ge "$STALL_AFTER" ]; then
      stalls=$((stalls+1)); stall_at="${stall_at:+$stall_at, }$off"; stall_latched=1
      barren=$((barren+1)); hwm_at_stall="$hwm"
    fi
  fi

  # ═══ CHECK 1 · AT-SLOT — local otadata write failure (#226) ═════════════════════════════
  if has_at_slot "$dg_live"; then
    # Rank 5, ABOVE death-point (10): a death-point CAUSED BY a failed otadata write must not headline
    # as a generic death-point, because the instruction differs (USB-flash it vs investigate the path).
    add FAIL 5 AT-SLOT "a LIVE ota/diag reports at=slot — the slot/otadata write itself failed (#226); needs a USB flash, OTA cannot proceed. payload: '$dg_live'"
  elif has_at_slot "$dg_any"; then
    add unknown 0 AT-SLOT "a RETAINED ota/diag carries at=slot but NO live one does — ota/diag is published RETAINED (wifi.rs:3299), so this is a ghost of an earlier attempt and is NOT a verdict. (This ghost once condemned a board whose OTA had just succeeded, and told the operator to USB-flash it.) payload: '$dg_any'"
  else
    add ok 0 AT-SLOT "no live ota/diag at=slot."
  fi

  # ═══ CHECK 2 · OFF-CHANNEL ═════════════════════════════════════════════════════════════
  # `cc=0` alone is NOT off-channel. Pre-6a62946 images fold "not associated" into 0, and even a
  # three-valued image can have `ap=` truncated off an associated leaf's record. So: act only with
  # association proof from the same live record. This slot has masked a genuine death-point four
  # times; it can no longer mask anything, but it can still MISLEAD, so it stays conservative.
  if [ "$cc_live" = "0" ] && [ -n "$ap_cc" ]; then
    add FAIL 30 OFF-CHANNEL "LIVE diag: cc=0 AND ap=$ap_cc in the SAME record → id$ID IS associated and its own AP (ch$ap_cc) is off the ESP-NOW mesh (ch${mesh_ch:-unknown}). Proven OTA blocker: co-channel moved 48 KB, off-channel moved 0. NOTE this is id$ID's OWN association, NOT the crown's — do not re-channel the crown on this evidence alone."
  elif [ "$cc_live" = "0" ]; then
    add unknown 0 OFF-CHANNEL "cc=0 but NO ap= in the same LIVE record → AMBIGUOUS, so no verdict. Deployed two-valued images fold 'not associated' into cc=0 (mode.rs unwrap_or(0)), and a truncated leaf record drops ap= before cc=. Off-channel and unassociated demand OPPOSITE responses (re-channel the AP / do nothing), so this is skipped rather than guessed."
  elif [ "$cc_live" = "2" ]; then
    add ok 0 OFF-CHANNEL "cc=2 = NOT ASSOCIATED (three-valued cc, 6a62946) — nothing to conclude; check N/A."
  elif [ "$cc_live" = "1" ]; then
    add ok 0 OFF-CHANNEL "cc=1 — live diag reports co-channel with the mesh."
  # The `ap=` vs `smol/mesh/channel` fallback arm that used to sit here is DELETED (2026-07-28), and
  # this comment is its headstone so nobody reinvents it. I had justified reading a retained
  # `mesh/channel` on the grounds that it is a compile-time fleet constant. That is FALSE on the wire:
  # wifi.rs:3864 publishes `MC|<id>|<pub_ch>|<seq>` where
  #     pub_ch = if co_channel && mesh_channel != 0 { mesh_channel } else { my_channel }
  # and `my_channel` is the crown's own LEARNED channel (mode.rs:2439/4983, "advisory 0 until a frame's
  # rx_control is learned"). So field 3 is the mesh channel ONLY WHILE THE CROWN IS CO-CHANNEL —
  # meaning the operand goes wrong precisely in the disease state the check existed to detect. An
  # off-channel crown that has learned ch1 publishes MC|5|1|…, and a target associated on ch1 would then
  # read as CO-channel: a false NEGATIVE, the worst direction for this file. `valid_ch` rejecting 0 only
  # covered the unlearned case. The honest operand is the firmware constant ESP_NOW_FIXED_CHANNEL
  # (mode.rs:67, =6; =1 under `coexist-soak`) which the shell cannot see, and the arm bought nothing:
  # the primary cc+ap path never used it, and every board that can be OTA'd today truncates cc=/ap=
  # off its record anyway.
  elif [ -n "$cut_live$cut_first" ]; then
    add unknown 0 OFF-CHANNEL "INAPPLICABLE: no cc= or ap= survived, and the record says why — cut=${cut_live:-$cut_first} bytes were truncated off its tail (mode.rs |cut=). The operands were sent and LOST, not absent."
  else
    add unknown 0 OFF-CHANNEL "INAPPLICABLE: no cc=/ap= in any live diag. Expected for a LEAF target: the leaf record is capped at 232 B (RELAY_VALUE_MAX, wifi.rs:1857) and cc=/ap= sit at the tail, so they are cut first (measured: crown id5 cc=1 ap=6; leaves id8/50/51/122/236 all cc=- ap=-). This check therefore CANNOT FIRE for the case this harness exists for — printed, not silently skipped."
  fi

  # ═══ CHECK 3 · DEATH-POINT ═════════════════════════════════════════════════════════════
  if [ "$saw_live_prog" = 1 ] && [ -n "$off" ] && is_num "$total" && [ "$off" -gt 0 ] \
     && [ "$off" -lt "$total" ] && [ $((now-last_off_t)) -ge "$STALL_AFTER" ] && [ "$retry_fresh" = 1 ]; then
    # Stalled, but the board says it is retrying. NOT a death and NOT terminal: report it as a
    # standing suspicion and keep watching. This is the exact case that produced a FAIL on a
    # successful OTA.
    add SUSPECT 45 STALLED "offset frozen at $off/$total for $((now-last_off_t))s, BUT a live ota/diag reports a retry $((now-retry_t))s ago, inside the ${RETRY_GRACE}s retry-grace (retry signals seen: $retry_n) — the board has NOT given up, so this is a retry, not a death. Still watching; a completion will override this. last live ota/diag: '${dg_live:-none}'"
  elif [ "$saw_live_prog" = 1 ] && [ -n "$off" ] && is_num "$total" && [ "$off" -gt 0 ] \
     && [ "$off" -lt "$total" ] && [ $((now-last_off_t)) -ge "$STALL_AFTER" ]; then
    add FAIL 10 DEATH-POINT "offset frozen at $off/$total for $((now-last_off_t))s (>= ${STALL_AFTER}s stall-after) and NO retry signal inside the ${RETRY_GRACE}s grace (last retry signal: $([ "$retry_t" = 0 ] && printf none || printf %ss $((now-retry_t))) ago; $retry_n seen this run) — the transfer died AT that byte (phase='${phase:-none}', monotonic=$([ $monotonic = 1 ] && echo yes || echo NO), restarts=$restarts, src=${src:-none}). NOT terminal to this run: still watching, and a completion would override this."
  elif [ "$saw_live_prog" = 0 ] && [ -n "$off_any" ]; then
    add unknown 0 DEATH-POINT "progress seen RETAINED ONLY (${off_any}/${total_any:-?}) — no live publish this run, so this offset is a ghost of an earlier attempt, possibly of a different image (compare total against the staged size). A retained value never changes, so it would satisfy 'frozen' for free; not a verdict."
  elif [ -z "$off" ] && [ -z "$off_any" ]; then
    add unknown 0 DEATH-POINT "no ota/progress observed at all — nothing to freeze; check skipped."
  else
    add ok 0 DEATH-POINT "live progress advancing (or complete): ${off:-unknown}/${total:-?}."
  fi

  # ═══ CHECK 3b · RETRY-LOOP — trying forever, arriving nowhere ═══════════════════════════
  # DEATH-POINT catches "stopped trying". STALLED forgives "still trying". Neither catches "trying and
  # never getting anywhere", which is a real, distinguishable failure: repeated stall episodes whose
  # high-water mark never advances. Without this arm an endlessly-retrying board can NEVER fail — it
  # reports UNPROVEN/extend-the-window forever, and extending the window is exactly the wrong advice.
  if [ "$barren" -ge "$BARREN_STALLS" ]; then
    add FAIL 15 RETRY-LOOP "$barren consecutive stall episodes with NO high-water advance (stalls at $stall_at; HWM stuck at ${hwm:-none}/${total:-?}; restarts=$restarts; retry signals=$retry_n). The board keeps retrying and keeps arriving nowhere. Extending the window has NOT helped across those stalls, so suspect the source path (src=${src:-none}) or the crown rather than the timeout — but note this is a claim about the run SO FAR: a later advance clears it, and this finding disappears if one arrives."
  elif [ "$stalls" -gt 0 ]; then
    add ok 0 RETRY-LOOP "$stalls stall episode(s), but the high-water advanced between them (HWM ${hwm:-none}) — retrying AND progressing."
  fi

  # ═══ CHECK 4 · ROLLED BACK ═════════════════════════════════════════════════════════════
  # A live `rolled-back` proves the firmware STILL HOLDS that outcome, which is not the same as it
  # having happened during this window: the token is rtc_fast persistent (ota.rs:2081). So the two
  # cases are reported DIFFERENTLY rather than collapsed.
  if [ "$ota_live" = "rolled-back" ] && [ -n "$ota_first" ] && [ "$ota_first" != "rolled-back" ]; then
    add FAIL 20 ROLLED-BACK "TRANSITION observed live in-window: ota=$ota_first → rolled-back. The board booted the new image, failed its self-test, and boot_confirm reverted it. App-side rollback worked as designed — the IMAGE is the problem, not the network."
  elif [ "$ota_live" = "rolled-back" ]; then
    add SUSPECT 50 ROLLED-BACK "a live diag reports ota=rolled-back, but it ALREADY read rolled-back at our first observation (${ota_first:-unknown}) — the token is rtc_fast persistent and only a power cycle clears it (ota.rs:2081), so this may predate this run entirely. Actionable, but NOT proof that this attempt rolled back."
  else
    add ok 0 ROLLED-BACK "no live ota=rolled-back."
  fi

  # ═══ CHECK 5 · PRODUCER CAPABILITY ═════════════════════════════════════════════════════
  producer="smol-c3"
  if [ -n "$slot_first" ] && ! is_num "$slot_first"; then producer="esp32c6-watch"
  elif [ "$boot_first" = "0" ] && [ "$boot_live" = "0" ] && [ "$rst_live" = "unknown" ]; then producer="esp32c6-watch"
  fi
  if [ "$producer" = "esp32c6-watch" ]; then
    add unknown 0 PRODUCER "this target speaks the esp32c6-watch DIAG vocabulary (slot=${slot_first:-?} rst=${rst_live:-?} boot=${boot_live:-?}), which is a CONSTANT string (esp32c6-watch/src/main.rs:2651). slot/boot/ota cannot transition on this producer, so the OTA proof below is STRUCTURALLY UNREACHABLE — a non-PASS here says nothing about whether the install worked. Verify a C6 install another way."
  fi
  if [ -n "$cut_live$cut_first" ]; then
    add unknown 0 TRUNCATION "this target's DIAG is truncated: cut=${cut_live:-$cut_first} bytes lost off the tail (mode.rs). Any field reported 'unknown' below may have been SENT and LOST rather than never published."
  fi

  # ═══ THE OTA PROOF · four live in-window transitions, all required ══════════════════════
  # Individually each conjunct is forgeable; the CONJUNCTION is what is hard to forge:
  #   A state flip   — a live ota/state flip to TARGET from a KNOWN, different baseline. Requiring a
  #                    non-empty baseline is what kills the old false PASS (`v? → v907`): an EMPTY
  #                    baseline used to satisfy `baseline != TARGET`.
  #   B slot flip    — the A/B partition actually changed. THIS is what defeats a persistent
  #                    `ota=confirmed`: a `confirmed` set by a real 905→906 OTA survives a later
  #                    usb-jtag reflash to 907 and every non-power-cycle reboot (wdt/panic/sw/
  #                    brownout — id8 runs rst=brownout live), so the old `rst=usb-jtag` arm caught
  #                    only the immediate post-flash boot and a stale `confirmed` read as a PASS.
  #                    No slot flip, no PASS.
  #   C boot inc     — the board actually rebooted inside our window.
  #   D ota token    — none|rolled-back → confirmed, observed LIVE HERE. boot_confirm sets it only
  #                    when the image was New/PendingVerify AND ota_was_activated_for(BUILD) AND the
  #                    self-test passed; a USB flash takes an explicit exemption and never marks it.
  #                    Requiring the TRANSITION (not the value) is what makes the persistent marker
  #                    unusable as a forgery.
  #   E not-usb      — a live rst=usb-jtag disqualifies outright.
  # Residual, stated rather than hidden: if we never observe a baseline (no retained diag lands), the
  # operands are `unknown` and PASS is REFUSED. That is the fail-closed direction — this harness
  # would rather under-report a real OTA than certify one it did not watch happen.
  A=0; B=0; C=0; D=0; E=0
  [ -n "$st_first" ] && [ "$st_first" != "$TARGET" ] && [ "$st_live" = "$TARGET" ] && A=1
  [ -n "$slot_first" ] && [ -n "$slot_live" ] && [ "$slot_first" != "$slot_live" ] && B=1
  # EXACTLY ONE boot, not merely an increase. `-gt` was defeated by a real attack: the gateway stops
  # republishing a leaf's diag once its cache entry ages past DIAG_FRESH_MS, so the RETAINED topic
  # holds the pre-OTA record INDEFINITELY (not for 150 s — that bound was wrong). An OTA then completes
  # unobserved, and ANY later unrelated reboot — wdt, panic, brownout — resets `up=` while
  # `ota=confirmed` survives in rtc_fast. That reboot makes F pass again, so F alone bounds nothing.
  # The tell was already being printed and ignored: `boot 492→495`, a delta of THREE. One in-window OTA
  # reboot is a delta of ONE.
  # Fail-closed cost, stated: a genuine in-window OTA is REFUSED if any extra reboot lands in the same
  # window, or if our baseline diag is more than one boot stale. That is the direction this file chooses
  # everywhere, and the rollback path cannot slip through either — two boots there end at
  # `rolled-back`, and D demands `confirmed`.
  boot_delta=""
  if is_num "$boot_first" && is_num "$boot_live"; then
    boot_delta=$((boot_live - boot_first))
    [ "$boot_delta" = 1 ] && C=1
  fi
  { [ "$ota_first" = "none" ] || [ "$ota_first" = "rolled-back" ]; } && [ "$ota_live" = "confirmed" ] && D=1
  # E requires an OBSERVED live rst — an ABSENT rst must not read as "not a USB flash". `rst=` is an
  # early positional field, so truncation (which eats the tail) never removes it; demanding it is
  # safe as well as fail-closed.
  [ -n "$rst_live" ] && [ "$rst_live" != "usb-jtag" ] && E=1
  # F — THE BOOT ITSELF HAPPENED INSIDE OUR WINDOW. A→E prove the values transitioned as we watched;
  # for a LEAF target that is not quite the same thing, because the operands reach us through TWO
  # INDEPENDENT GATEWAY CACHES with DIFFERENT freshness gates: `ota/state` is republished under STAT's
  # 45 s gate while `<id>/diag` uses its own DIAG_FRESH_MS = 150 s (wifi.rs:1716, deliberately longer or
  # "a live node's record would flicker stale between broadcasts"). So a DIAG lagging a completed OTA can
  # hand us a stale `ota=none` followed by a fresh `confirmed`, which LOOKS like an in-window transition
  # for an install that finished before we subscribed. The install claim would still be true; "I watched
  # it happen" would not — and that is the one place the stated invariant leaks.
  # `up=` (mode.rs:3280, seconds since boot, stamped when the record is generated) narrows it: if the
  # CURRENT boot were older than this run, `up` would exceed our elapsed time. Absent `up=` refuses
  # PASS, like every other missing operand.
  #
  # F IS NOT SUFFICIENT ON ITS OWN, and the first version of this comment claimed otherwise. F asks
  # whether the CURRENT boot is ours; any later unrelated reboot resets `up=` while `ota=confirmed`
  # survives in rtc_fast, so a fresh `up` can belong to a reboot that has nothing to do with the OTA.
  # C's exactly-one-boot rule is what closes that, and the two are only strong together. The earlier
  # claim that this bounded the hole to ~30 s was doubly wrong: the retained record can be
  # ARBITRARILY stale, because the gateway stops republishing once the cache entry ages out.
  F=0
  if is_num "$up_live"; then
    [ "$up_live" -le $(( (now-start) + SETTLE + 2*POLL + 30 )) ] && F=1
  fi
  [ $((A+B+C+D)) = 4 ] && [ "$E" = 1 ] && [ "$F" = 1 ] && pass_ok=1

  for x in "${findings[@]}"; do
    case "$x" in FAIL*) fail_n=$((fail_n+1));; esac
  done

  # ═══ decide ════════════════════════════════════════════════════════════════════════════
  # ONLY A COMPLETION PROOF ENDS THE RUN EARLY. Every failure finding is non-terminal: it is
  # recomputed each poll, so it stands only while it is still TRUE, and it is reported only if the
  # window expires with it standing. This is the fix for reporting FAIL on an OTA that worked — the
  # old code `break`ed on the first DEATH-POINT at ~60 s of a 600 s window and never saw the PASS
  # that arrived 3.5 minutes later. It also removes the last place where one check could pre-empt
  # another: not by ordering (already fixed) but by ENDING THE OBSERVATION.
  if [ "$pass_ok" = 1 ] && [ "$fail_n" = 0 ]; then verdict=PASS; break; fi
  if [ "$pass_ok" = 1 ] && [ "$fail_n" -gt 0 ]; then verdict=CONFLICT; break; fi
  if [ $((now-start)) -ge "$WINDOW" ]; then
    [ "$fail_n" -gt 0 ] && verdict=FAIL || verdict=UNPROVEN
    break
  fi
  sleep "$POLL"
done

# ═══ report ══════════════════════════════════════════════════════════════════════════════
# Headline selection ONLY. Nothing is dropped: every finding prints below, which is what makes
# masking structurally impossible rather than repeatedly re-fixed.
if [ "$verdict" = "FAIL" ] || [ "$verdict" = "CONFLICT" ]; then
  best=99
  for x in "${findings[@]}"; do
    IFS=$'\t' read -r s r c t <<<"$x"
    case "$s" in FAIL|SUSPECT) [ "$r" -lt "$best" ] && { best="$r"; headline="$c — $t"; };; esac
  done
  [ "$verdict" = CONFLICT ] && headline="CONFLICT — the OTA proof is COMPLETE and a failure signal also fired. Both are printed below; trust neither until you have read the operands. Leading failure: $headline"
elif [ "$verdict" = PASS ]; then
  headline="v$st_first → v$TARGET over the air. All four transitions observed LIVE in-window: state $st_first→$st_live · slot $slot_first→$slot_live · boot $boot_first→$boot_live · ota $ota_first→$ota_live · rst=$rst_live${src:+ · source: $src}"
  case "$src" in id*) headline="$headline  ← PEER-SOURCED (#237)";; esac
else
  # UNPROVEN is the honest default, and it is NOT the same as FAIL. Say which operand is missing.
  # A standing SUSPECT outranks the generic text: "the window ended while the board was still
  # retrying" is a different instruction to the operator (extend the window) than "nothing happened".
  susp=""; sbest=99
  for x in "${findings[@]}"; do
    IFS=$'\t' read -r s r c t <<<"$x"
    [ "$s" = SUSPECT ] && [ "$r" -lt "$sbest" ] && { sbest="$r"; susp="$c — $t"; }
  done
  if [ -n "$susp" ]; then
    headline="window ${WINDOW}s expired with the transfer STILL IN PROGRESS — not a failure. Re-run with a longer window. $susp"
  elif [ "$producer" = "esp32c6-watch" ]; then
    headline="UNPROVABLE BY THIS HARNESS, not failed — the target publishes the esp32c6-watch constant DIAG, whose slot/boot/ota fields cannot transition (see PRODUCER below). state ${st_first:-unknown}→${st_live:-unknown} is all that is observable here."
  elif [ -n "$st_first" ] && [ "$st_first" = "$TARGET" ]; then
    headline="already on v$TARGET at our FIRST observation — no flip was observable, so nothing here proves an OTA either way. Run this BEFORE arming."
  elif [ "$pass_ok" = 0 ] && [ $((A+B+C+D)) -gt 0 ]; then
    headline="window ${WINDOW}s elapsed with the OTA proof INCOMPLETE ($((A+B+C+D))/4 transitions, see below) — no failure was observed either. This is 'not proven', not 'failed'."
  else
    headline="window ${WINDOW}s elapsed, no OTA transition observed at all — HWM ${hwm:-none}/${total:-?}, last phase '${phase:-none}', last ota/diag '${dg_any:-none}'."
  fi
fi

printf '\n════════════════════════════════════════════════════════════════════════\n'
printf '  VERDICT: %s — id%s → v%s\n  %s\n' "$verdict" "$ID" "$TARGET" "$headline"
printf '  ── findings · every check evaluated; none can mask another ──\n'
for x in "${findings[@]}"; do
  IFS=$'\t' read -r s r c t <<<"$x"
  printf '  [%-7s] %-12s %s\n' "$s" "$c" "$t"
done
printf '  ── OTA proof · all four transitions required, LIVE, in-window ──\n'
y() { [ "$1" = 1 ] && printf 'yes' || printf 'NO '; }
printf '    [%s] A state flip   %s → %s   (target v%s)\n'  "$(y $A)" "${st_first:-unknown}"   "${st_live:-unknown}"   "$TARGET"
printf '    [%s] B slot flip    %s → %s\n'                 "$(y $B)" "${slot_first:-unknown}" "${slot_live:-unknown}"
printf '    [%s] C boot incr    %s → %s   (delta %s, want EXACTLY 1 — more boots than the one OTA we claim to have watched)\n' \
  "$(y $C)" "${boot_first:-unknown}" "${boot_live:-unknown}" "${boot_delta:-unknown}"
printf '    [%s] D ota token    %s → %s   (want none|rolled-back → confirmed)\n' "$(y $D)" "${ota_first:-unknown}" "${ota_live:-unknown}"
printf '    [%s] E not usb      rst=%s\n'                  "$(y $E)" "${rst_live:-unknown}"
printf '    [%s] F boot in-window  up=%ss (elapsed %ss + %ss slack) — defeats a cache-lagged ota= flip\n' \
  "$(y $F)" "${up_live:-unknown}" "$((now-start))" "$((SETTLE + 2*POLL + 30))"
printf '  ── operands (first observed → last LIVE; "unknown" = never observed, check skipped) ──\n'
# Every value on these lines is LIVE (verdict-time) unless explicitly tagged (retained) — the old
# evidence block printed `tail -1` of ALL observations, so it could contradict its own verdict
# (observed: `installed v345 · rst=brownout · ota=none` printed for a board that was on 907 by then).
# `crown-adv ch` is NOT necessarily the mesh channel — see the headstone above. Labelled as what it
# literally is (whatever the crown advertised) so nobody reads a verdict into it.
printf '    producer=%s · cc=%s · ap=%s · crown-adv ch=%s (retained; mesh ch only while co-channel) · cut=%s\n' \
  "$producer" "${cc_live:-unknown}" "${ap_live:-unknown}" "${mesh_ch:-unknown}" "${cut_live:-${cut_first:-none}}"
printf '    retry signals=%s · stall episodes=%s%s · restarts from a lower offset=%s · images seen=%s\n' \
  "$retry_n" "$stalls" "${stall_at:+ (at $stall_at)}" "$restarts" "$((images+1))"
# HWM is the high-water mark of LIVE offsets only — a retained ghost cannot inflate it. `none` (not
# `0`) when no live progress was ever seen, because 0 is a legitimate offset and would read as one.
printf '    live progress HWM %s/%s · monotonic=%s · phase=%s · live progress seen=%s · last seen (any) %s/%s\n' \
  "${hwm:-none}" "${total:-unknown}" "$([ $monotonic = 1 ] && echo yes || echo NO)" "${phase:-none}" \
  "$([ "$saw_live_prog" = 1 ] && echo yes || echo NO)" "${off_any:-none}" "${total_any:-unknown}"
printf '    last ota/diag (LIVE):     %s\n' "${dg_live:-none}"
printf '    last ota/diag (any, may be retained): %s\n' "${dg_any:-none}"
printf '════════════════════════════════════════════════════════════════════════\n'
[ "$verdict" = "PASS" ] && exit 0 || exit 1
