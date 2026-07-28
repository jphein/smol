#!/usr/bin/env bash
# smol COMMAND-GHOST reconciler — are there retained COMMAND topics for ids that aren't live?
#
#   Usage:  ghost_reconcile.sh [--window N] [--clear] [--json] [--root R] [--strict]
#                              [--selftest] [--check-suffixes]
#   Exit:   0 = clean · 1 = ACTION NEEDED (stale and/or malformed) · 2 = COULD NOT CHECK
#
# WHY THIS EXISTS (#308). A registry-derived dashboard stops *displaying* ghosts. It does not stop a
# ghost's retained COMMAND topic from ACTING ON THE FLEET. A retained order aimed at a board that
# will never acknowledge it is never satisfied — and the symptom is SILENCE, not an error:
#
#   * `smol/<dead>/ota/install` → the crown relays, the leaf never answers, `record_leaf_ota` gets
#     `MacUnknown`/`FetchFailed` (`!reached_leaf()`), which by #134 case 2 is a PRE-RELAY transient:
#     "NEVER clear — the retained install must survive for the next attempt". Correct for a live
#     leaf; for a board that no longer exists it means the order is immortal, `leaf_ota_pending`
#     stays set, and the gateway's own self-OTA is suppressed indefinitely (`leaf_installs_
#     outstanding`, mode.rs). Observed 2026-07-28: `smol/9/ota/diag mac-unknown retry=15 src=gw`
#     while the crown sat on 906 with 907 staged.
#   * A stale TELEMETRY ghost is cosmetic. A stale COMMAND is an instruction the fleet is obeying.
#
# WHAT COUNTS AS A COMMAND — not a guess. A topic the firmware SUBSCRIBES to is an instruction;
# everything else is telemetry it publishes. Regenerate the list with:
#   grep -naE 'encode_subscribe(_qos1)?\(' rust/clock/src/net/wifi.rs \
#     | grep -oaE '"smol/[^"]+"' | sed 's/"//g' | grep '^smol/+/' | sort -u
# (Both helpers — `encode_subscribe` AND `encode_subscribe_qos1`. Grepping only the first misses
#  cmd/reset, cmd/scan and notify.)
#
# TRAPS THIS ENCODES (all learned the hard way on 2026-07-28):
#   * RETAIN FLAG IS `%r`, NOT `%R`. `%R` is not a specifier — it expands to EMPTY, so a `^0`
#     "is-this-live" test silently never matches and every message reads as a ghost.
#   * ANCHOR EVERY FIELD READ. `up=[0-9]+` also matches inside `dedup=0` (`…ded|up=0`), and
#     `ap=[0-9]+` inside `heap=42040`. Use `\|up=[0-9]+\|`. Two independent bugs came from this.
#   * grep -a EVERYWHERE. One binary byte in a payload flips grep to binary mode → prints nothing.
#   * LIVENESS MUST COME FROM THE WIRE. The HA device registry is materialised FROM retained
#     discovery configs, so it lists ghosts as devices with a sw_version — using it here would make
#     the reconciler blind to exactly what it hunts. Same for HA entity freshness, which measures
#     HA's template-recompute loop, not the fleet.
#   * "CAN'T TELL" IS NOT CLEAN. Three states, never two: live · dead · unknown. Unknown exits 2.
#   * A leaf's DIAG is CROWN-RELAYED, so "no diag" proves "not heard by the crown", not "unpowered".
#     Mitigating signal: a node hearing no crown SELF-PROMOTES (REELECT_SILENCE_MS=15000,
#     mode.rs → is_gateway=true) and then publishes its OWN diag and `PEERS|G|` directly. So a
#     powered-but-off-mesh board is not silent. A board reaching NEITHER mesh nor AP still is.
#   * id42 IS A SENTINEL, NOT A BOARD (#314). It is every C6 watch's unset-config default
#     (esp32c6-watch/src/main.rs:1130), remapped to a MAC-derived id (→122/236). It RECURS BY
#     DESIGN on any unprovisioned boot, so flagging it as a ghost trains the operator to ignore
#     this tool. Reported as SENTINEL, never cleared.
#   * CLEARING A CONFIG TOPIC DOES NOT REVERT THE BOARD. There is no empty-payload guard in the
#     config apply path (wifi.rs ~2900: `cache.set(leaf_id, CFG_KEY_LED, payload, …)` runs with an
#     empty `payload`), and the leaf's `from_wire` treats empty/garbage as invalid → KEEPS CURRENT
#     (#46 clamp). So a clear is neither a revert nor a no-op: it is a LOSS OF OBSERVABILITY — the
#     board keeps applying a value the broker no longer shows. Safe for a dead id, wrong for a live
#     one. This tool only ever clears ids it has proven dead.
#   * NEVER PLANT A TEST COMMAND ON THE LIVE ROOT. `--selftest` uses a separate topic root
#     (`smoltest`) precisely because planting `smol/199/ota/install` would pin the crown — i.e. it
#     would CAUSE the bug this tool detects. MQTT levels are exact, so `smol/#` never matches
#     `smoltest/…` and the fleet cannot see it.
#   * CREDENTIALS NEVER IN ARGV (#313). `-P <pw>` is world-readable via /proc/<pid>/cmdline. We
#     write `-u`/`-P` into a 0700 dir and point XDG_CONFIG_HOME at it (mosquitto reads
#     $XDG_CONFIG_HOME/mosquitto_{sub,pub}, "one pair of -option value per line" — WITH dashes;
#     bare `username` is rejected). Never put `-t` in that file: the man page says -t/-T in the
#     config file cannot be overridden from the command line.
set -uo pipefail

ROOT="smol"; WINDOW=90; DO_CLEAR=0; JSON=0; SELFTEST=0; LIVE_OVERRIDE=""; CHECK_SUFFIXES=0; STRICT=0
while [ $# -gt 0 ]; do
  case "$1" in
    --window) WINDOW="${2:?}"; shift 2;;
    --clear)  DO_CLEAR=1; shift;;
    --json)   JSON=1; shift;;
    --root)   ROOT="${2:?}"; shift 2;;
    --selftest) SELFTEST=1; shift;;
    --check-suffixes) CHECK_SUFFIXES=1; shift;;
    --strict) STRICT=1; shift;;
    --live-override) LIVE_OVERRIDE="${2:?}"; shift 2;;   # TEST ONLY — stubs wire liveness
    -h|--help) sed -n '2,7p' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

# ── per-id COMMAND topic suffixes (see header for the regenerating grep) ────────────────
CMD_SUFFIXES=(
  ota/install cmd/reset cmd/scan notify io/set cast
  config/broker config/custom config/default_screen config/delivery
  config/io config/led config/net config/ota_host config/plugins config/tale
)
SENTINEL_IDS=(42)   # #314 — C6 watch unset-config default; recurs by design

# ── --check-suffixes: fail loudly if the firmware grew a subscription this list doesn't know ──
# A hand-maintained list that silently drifts is the same disease #308 is about, one level up: this
# tool would keep reporting "clean" while a whole new command family went unwatched. Cheap to check,
# so check it — needs no broker, so it runs before credentials.
if [ "$CHECK_SUFFIXES" = 1 ]; then
  WIFI="$HOME/Projects/smol/rust/clock/src/net/wifi.rs"
  [ -r "$WIFI" ] || { echo "COULD NOT CHECK: $WIFI unreadable" >&2; exit 2; }
  fw="$(grep -naE 'encode_subscribe(_qos1)?\(' "$WIFI" | grep -oaE '"smol/[^"]+"' | sed 's/"//g' \
        | grep -a '^smol/+/' | sed 's|^smol/+/||' | sort -u)"
  tool="$(printf '%s\n' "${CMD_SUFFIXES[@]}" | sort -u)"
  missing="$(comm -23 <(printf '%s\n' "$fw") <(printf '%s\n' "$tool"))"
  extra="$(  comm -13 <(printf '%s\n' "$fw") <(printf '%s\n' "$tool"))"
  echo "── suffix drift check vs $WIFI ──"
  if [ -n "$missing" ]; then
    echo "  ✗ firmware subscribes to these, tool does NOT watch them:"; printf '      %s\n' $missing
    echo "  → add them to CMD_SUFFIXES, or this tool reports clean while they go unwatched."; exit 1
  fi
  # `cast` is legitimately absent from the smol/+/ list: it is subscribed per-node as
  # smol/<node_id>/cast (feature-gated), so no wildcard form exists in the source.
  for e in $extra; do
    [ "$e" = "cast" ] && continue
    echo "  ✗ tool watches '$e' but the firmware does not subscribe to it — stale entry?"; exit 1
  done
  echo "  ✅ in sync (${extra:+extra '$extra' justified: per-node subscription, not a wildcard})"
  exit 0
fi

# ── credentials: sourced, then written to a private mosquitto config (never argv) ───────
OTA_ENV="$HOME/Projects/smol/tools/ota_publish.env"; [ -f "$OTA_ENV" ] && . "$OTA_ENV"
# Placeholders only — the real broker/user/addon come from tools/ota_publish.env (git-ignored),
# matching ota_verify.sh. Do not bake LAN topology into a tracked file.
BROKER="${BROKER:-<broker-ip>}"; MQTT_USER="${MQTT_USER:-<mqtt-user>}"; ADDON="${ADDON:-<addon-slug>}"
case "$BROKER" in '<'*) echo "COULD NOT CHECK: no BROKER — source tools/ota_publish.env first" >&2; exit 2;; esac
WORK="$(mktemp -d "${TMPDIR:-/tmp}/ghostrec.XXXXXX")"

# A child invocation (--selftest re-runs this script) INHERITS the parent's credential dir via
# GREC_CRED rather than calling `bw` again: three vault hits in quick succession is a needless
# failure surface, and the vault-gate unlock window can close between them (it did, once, mid-test).
if [ -n "${GREC_CRED:-}" ] && [ -r "${GREC_CRED}/mosquitto_sub" ]; then
  CRED="$GREC_CRED"; OWN_CRED=0
else
  PW="$(timeout 40 bash -c 'tok=$(bw get password ha-llat 2>/dev/null) || exit 1
    HA_TOKEN="$tok" python3 "$HOME/Projects/ha/tools/ha_supervisor.py" GET "/addons/'"$ADDON"'/info" 2>/dev/null \
    | python3 -c "import sys,json;print(json.load(sys.stdin)[\"options\"][\"mqtt_password\"])" 2>/dev/null')"
  [ -n "$PW" ] || { rm -rf "$WORK"; echo "COULD NOT CHECK: no mqtt password (bw locked? addon unreachable?)" >&2; exit 2; }
  CRED="$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/smol-mqtt.XXXXXX")"; chmod 700 "$CRED"
  umask 077
  printf -- '-u %s\n-P %s\n' "$MQTT_USER" "$PW" > "$CRED/mosquitto_sub"
  cp "$CRED/mosquitto_sub" "$CRED/mosquitto_pub"
  unset PW
  OWN_CRED=1
fi
# Only the creator removes the credential dir, or a child would yank it from under its parent.
trap '[ "${OWN_CRED:-0}" = 1 ] && rm -rf "$CRED"; rm -rf "$WORK"' EXIT
msub() { XDG_CONFIG_HOME="$CRED" mosquitto_sub -h "$BROKER" -p 1883 "$@"; }
mpub() { XDG_CONFIG_HOME="$CRED" mosquitto_pub -h "$BROKER" -p 1883 "$@"; }

# ── selftest: plant on a SEPARATE root so the fleet cannot see it (see header) ──────────
if [ "$SELFTEST" = 1 ]; then
  echo "── selftest: planting a retained command on root 'smoltest' (fleet-invisible) ──"
  mpub -i "grec_st_$$" -r -t "smoltest/199/ota/install" -m "INSTALL" || { echo "plant failed"; exit 2; }
  sleep 1
  out="$(GREC_CRED="$CRED" "$0" --root smoltest --window 8 --live-override 5 2>&1)"; rc=$?
  echo "$out" | sed 's/^/  | /'
  echo "  selftest exit=$rc (want 1 = stale found)"
  mpub -i "grec_st2_$$" -r -n -t "smoltest/199/ota/install"
  sleep 1
  out2="$(GREC_CRED="$CRED" "$0" --root smoltest --window 8 --live-override 5 2>&1)"; rc2=$?
  echo "$out2" | sed 's/^/  | /'
  echo "  after-clear exit=$rc2 (want 0 = clean)"
  if [ "$rc" = 1 ] && [ "$rc2" = 0 ]; then echo "  ✅ SELFTEST PASS — goes red on a planted ghost, green once cleared"; exit 0
  else echo "  ✗ SELFTEST FAIL (red=$rc green=$rc2)"; exit 2; fi
fi

# ── 1. observe the wire ────────────────────────────────────────────────────────────────
LOG="$WORK/scan.log"
# `-W <secs>` makes mosquitto_sub self-terminate, so there is no background pid to kill.
# The earlier `msub … & ; sleep ; kill $!` LEAKED A SUBSCRIBER PER RUN: `msub` is a shell function,
# so `&` forks a subshell and `$!` is the SUBSHELL's pid — killing it left the real mosquitto_sub
# grandchild connected forever (found three orphans from three runs). Self-timeout has no such gap.
# The window must span >=2 roster/diag samples (~15 s cadence) for the "advancing" tests to work.
msub -i "grec_$$" -F '%r %t %p' -t "$ROOT/#" -W "$WINDOW" > "$LOG" 2>&1 || true
# An empty log on the REAL root means we never saw the fleet → cannot conclude anything (exit 2).
# Under --live-override (test) liveness is stubbed, so an empty root legitimately means "no
# command topics", i.e. clean — bailing there would make the post-clear selftest unfalsifiable.
if [ ! -s "$LOG" ] && [ -z "$LIVE_OVERRIDE" ]; then
  echo "COULD NOT CHECK: no MQTT data from $BROKER on root '$ROOT'" >&2; exit 2
fi

# ── 2. liveness, from the wire only ────────────────────────────────────────────────────
# crown = MC|<owner>|<ch>|<seq>; the seq MUST ADVANCE or the MC record is itself a retained ghost.
mc_first="$(grep -a " $ROOT/mesh/channel " "$LOG" | head -1 | awk '{print $3}')"
mc_last="$( grep -a " $ROOT/mesh/channel " "$LOG" | tail -1 | awk '{print $3}')"
crown="$(printf '%s' "$mc_last" | cut -d'|' -f2)"
seq_first="$(printf '%s' "$mc_first" | cut -d'|' -f4)"; seq_last="$(printf '%s' "$mc_last" | cut -d'|' -f4)"
crown_live=0
if [ -n "${seq_first:-}" ] && [ -n "${seq_last:-}" ] && [ "$seq_last" != "$seq_first" ]; then crown_live=1; fi

# roster: PEERS|G|<ch>|id,rssi,age,ch,flags;…  — take ids seen with a fresh age.
roster=""
if [ -n "${crown:-}" ]; then
  roster="$(grep -a " $ROOT/$crown/peers " "$LOG" | tail -1 | awk '{print $3}' \
    | cut -d'|' -f4- | tr ';' '\n' | awk -F, '$3!="" && $3+0 <= 120 {print $1}' | sort -un | tr '\n' ' ')"
fi

# up=-advancing, ANCHORED (\|up=…\| — bare up= matches inside dedup=). Independent of the roster.
advancing=""
for id in $(grep -oaE " $ROOT/[0-9]+/diag " "$LOG" | tr -d ' ' | sed -E "s|$ROOT/||;s|/diag||" | sort -un); do
  a="$(grep -a " $ROOT/$id/diag " "$LOG" | head -1 | grep -oaE '\|up=[0-9]+\|' | head -1 | tr -d '|' | cut -d= -f2)"
  b="$(grep -a " $ROOT/$id/diag " "$LOG" | tail -1 | grep -oaE '\|up=[0-9]+\|' | head -1 | tr -d '|' | cut -d= -f2)"
  [ -n "$a" ] && [ -n "$b" ] && [ "$b" != "$a" ] && advancing="$advancing $id"
done

LIVE=" $crown $roster $advancing "
if [ -n "$LIVE_OVERRIDE" ]; then LIVE=" ${LIVE_OVERRIDE//,/ } "; crown_live=1; fi
# Ids with a live publish on a topic the BOARD (or the crown relaying it) produces.
# CRITICAL: this must EXCLUDE command topics. A live publish on `smol/7/config/custom` is Home
# Assistant writing an instruction — not the board answering. Observed 2026-07-28: HA publishes
# `smol/{7,9}/config/custom = "unavailable"` (retain=0) for boards that have not existed since
# 07-22, so counting any-live-publish would mark both ids "alive but off-roster" forever. Attributing
# a publish to the wrong producer is the same mistake as trusting the registry: the topic tree is
# shared, so only the SUFFIX tells you who wrote it.
livepub="$(grep -a '^0 ' "$LOG" | awk '{print $2}' | grep -oaE "^$ROOT/[0-9]+/.*" | while read -r t; do
    sfx="$(printf '%s' "$t" | cut -d/ -f3-)"; isc=0
    for s in "${CMD_SUFFIXES[@]}"; do [ "$sfx" = "$s" ] && isc=1; done
    [ "$isc" = 0 ] && printf '%s\n' "$t" | cut -d/ -f2
  done | sort -un | tr '\n' ' ')"

# ── 3. retained COMMAND topics, grouped by id ──────────────────────────────────────────
: > "$WORK/cmds"
while read -r r topic payload; do
  [ "$r" = "1" ] || continue
  case "$topic" in "$ROOT"/*) ;; *) continue;; esac
  id="$(printf '%s' "$topic" | cut -d/ -f2)"; suffix="$(printf '%s' "$topic" | cut -d/ -f3-)"
  case "$id" in ''|*[!0-9]*) continue;; esac            # non-numeric segment → not a per-id topic
  for s in "${CMD_SUFFIXES[@]}"; do
    [ "$suffix" = "$s" ] && printf '%s\t%s\t%s\n' "$id" "$topic" "$payload" >> "$WORK/cmds"
  done
done < <(grep -a '^1 ' "$LOG")

# ── 4. classify ────────────────────────────────────────────────────────────────────────
stale=0; unknown=0; sentinel=0; live_ok=0; duty=0; malformed=0
: > "$WORK/report"; : > "$WORK/malformed"

# PRODUCER CLASS, read off the wire — this is what stops a sleeping watch reading as a dead board.
# Two DIAG producers exist: the C3 `rust/clock` fleet emits a NUMERIC `slot=<0|1>`; the C6 Embassy
# watches emit the string `slot=ota_0` (esp32c6-watch/src/main.rs). The C3 nodes are mains-powered
# and crown-relayed every ~15 s, so absence across the window is meaningful. **The watches are
# duty-cycled** — they sleep and reboot, drop out of the roster, and reappear. On the first real run
# of this tool a 100 s window put live id236 in DEAD, which under --clear would have wiped a real
# device's config. MQTT exposes no publish timestamp for a retained value, so there is no "last
# seen" to fall back on; the producer signature is the only honest discriminator.
prodclass() { # $1=id → c3 | c6 | none
  local d; d="$(grep -a " $ROOT/$1/diag " "$LOG" | tail -1 | awk '{ $1=""; $2=""; print }')"
  [ -z "$d" ] && { echo none; return; }
  local slot; slot="$(printf '%s' "$d" | grep -oaE '\|?slot=[^|[:space:]]*' | head -1 | cut -d= -f2)"
  case "${slot:-}" in ''|*[!0-9]*) echo c6;; *) echo c3;; esac
}

for id in $(cut -f1 "$WORK/cmds" 2>/dev/null | sort -un); do
  state=DEAD; why="no roster entry, no advancing up=, no live publish in ${WINDOW}s"
  for s in "${SENTINEL_IDS[@]}"; do [ "$id" = "$s" ] && { state=SENTINEL; why="#314 C6 watch unset-config sentinel — recurs by design, never a board"; }; done
  if [ "$state" != SENTINEL ]; then
    case "$LIVE" in *" $id "*) state=LIVE; why="crown/roster/up=-advancing";; esac
    if [ "$state" = DEAD ]; then
      case " $livepub " in *" $id "*) state=UNKNOWN; why="published live telemetry this window but is NOT in the crown roster — alive but off-mesh?";; esac
    fi
    # A duty-cycled producer absent for one window is asleep, not dead. Its command topics are
    # LEGITIMATE config for a real device, so this is a benign state by default — flagging it every
    # run would train the operator to ignore the tool (the #314 lesson). --strict escalates it.
    if [ "$state" = DEAD ] && [ "$(prodclass "$id")" = c6 ]; then
      if [ "${STRICT:-0}" = 1 ]; then
        state=UNKNOWN; why="C6/Embassy producer (slot=ota_*), duty-cycled — absent this window; --strict declines to call it dead"
      else
        state=DUTY-CYC; why="C6/Embassy producer (slot=ota_*) — duty-cycled device, sleeps between windows; command topics are legitimate"
      fi
    fi
    # 2026-07-28 (audit G2): NO PRODUCER EVIDENCE AT ALL IS NOT PROOF OF DEATH.
    # prodclass() returns `none` when no DIAG line exists for the id, and the duty-cycle
    # exemption above is gated on `= c6` — so `none` used to fall straight through to DEAD,
    # i.e. into the one state --clear mutates. Every OTHER guard here keys off some OBSERVED
    # signal (roster entry, advancing up=, live publish, producer class); an id with retained
    # command topics and zero producer evidence satisfied none of them and was clearable.
    # Reachable in a normal flow: a board configured in HA before its first boot, or one whose
    # retained DIAG was cleared. That contradicted this tool's own doctrine — it prints
    # "'Can't tell' is not clean" and exits 2 for UNKNOWN, then treated total absence of
    # evidence as certainty. Structurally present but unexercised when found (every id in that
    # run had at least a retained DIAG), which is exactly when it is cheapest to fix.
    if [ "$state" = DEAD ] && [ "$(prodclass "$id")" = none ]; then
      state=UNKNOWN; why="NO producer evidence at all — no DIAG line for this id, so its silence is unexplained rather than explained. Absence of evidence is not evidence of death; refusing to call it dead."
    fi
    # No trustworthy fleet view at all ⇒ we cannot call anything dead.
    if [ "$crown_live" = 0 ] && [ "$state" = DEAD ]; then
      state=UNKNOWN; why="no LIVE crown observed (mesh/channel seq did not advance) — fleet view untrustworthy"
    fi
  fi

  # MALFORMED COMMANDS — independent of liveness, and worse on a LIVE device. Home Assistant
  # renders its own placeholder strings ("unknown"/"unavailable"/"None") into templates; when such a
  # template feeds a retained COMMAND topic, that garbage becomes a standing instruction. Observed
  # 2026-07-28: `smol/122/config/delivery = 160:inf:unknown` on a LIVE watch, and
  # `smol/{7,9}/config/custom = unavailable`. The firmware's from_wire rejects it and keeps current
  # (#46 clamp), so the failure is SILENT — the board obeys a value the topic no longer describes.
  while IFS=$'\t' read -r cid ctopic cpayload; do
    [ "$cid" = "$id" ] || continue
    case "$cpayload" in
      *unknown*|*unavailable*|*None*|*nan*)
        printf '%s\t%s\t%s\t%s\n' "$id" "$state" "$ctopic" "$cpayload" >> "$WORK/malformed"
        malformed=$((malformed+1));;
    esac
  done < "$WORK/cmds"
  n="$(awk -F'\t' -v i="$id" '$1==i' "$WORK/cmds" | wc -l)"
  printf '%s\t%s\t%s\t%s\n' "$id" "$state" "$n" "$why" >> "$WORK/report"
  case "$state" in
    DEAD) stale=$((stale+n));; UNKNOWN) unknown=$((unknown+n));;
    SENTINEL) sentinel=$((sentinel+n));; LIVE) live_ok=$((live_ok+n));;
    DUTY-CYC) duty=$((duty+n));;
  esac
done

# ── 5. output ──────────────────────────────────────────────────────────────────────────
if [ "$JSON" = 1 ]; then
  printf '{"root":"%s","crown":"%s","crown_live":%s,"roster":"%s","stale":%s,"unknown":%s,"sentinel":%s,"live":%s,"ids":[' \
    "$ROOT" "${crown:-}" "$crown_live" "$(echo $roster)" "$stale" "$unknown" "$sentinel" "$live_ok"
  first=1; while IFS=$'\t' read -r id st n why; do
    [ $first = 1 ] || printf ','; first=0
    printf '{"id":%s,"state":"%s","cmd_topics":%s,"why":"%s"}' "$id" "$st" "$n" "$why"
  done < "$WORK/report"; printf ']}\n'
else
  printf '── ghost_reconcile: root %s · window %ss · broker %s ──\n' "$ROOT" "$WINDOW" "$BROKER"
  printf '   crown %s (live=%s) · roster [%s]\n' "${crown:-none}" "$([ "$crown_live" = 1 ] && echo yes || echo NO)" "$(echo $roster)"
  printf '   up=-advancing: [%s]\n\n' "$(echo $advancing)"
  if [ -s "$WORK/report" ]; then
    printf '   %-5s %-9s %-5s %s\n' ID STATE CMDS WHY
    while IFS=$'\t' read -r id st n why; do printf '   %-5s %-9s %-5s %s\n' "$id" "$st" "$n" "$why"; done < "$WORK/report"
    echo
    awk -F'\t' '$2=="DEAD"||$2=="UNKNOWN"{print $1}' "$WORK/report" | while read -r id; do
      printf '   ▸ id%s retained command topics:\n' "$id"
      awk -F'\t' -v i="$id" '$1==i {printf "       %s = [%s]\n", $2, $3}' "$WORK/cmds"
    done
    if [ -s "$WORK/malformed" ]; then
      echo
      echo "   ⚠ MALFORMED COMMAND PAYLOADS — a controller wrote its own placeholder into a standing"
      echo "     instruction. On a LIVE device the board rejects it and KEEPS ITS CURRENT value"
      echo "     (from_wire + #46 clamp), so the broker and the board silently disagree."
      while IFS=$'\t' read -r id st topic payload; do
        printf '       [%s] id%-4s %s = [%s]\n' "$st" "$id" "$topic" "$payload"
      done < "$WORK/malformed"
    fi
  else
    printf '   no retained per-id command topics found at all\n'
  fi
fi

# ── 6. optional clear — backup first, DEAD only ────────────────────────────────────────
if [ "$DO_CLEAR" = 1 ] && [ "$stale" -gt 0 ]; then
  # 2026-07-28 (audit G3): the backup lives OUTSIDE agent scratch. It used to be written to
  # ~/.claude/projects/…/scratch/nexus-345/, which is an agent TASK dir — and this project's
  # housekeeping policy says to delete stale task dirs at conversation start. The sole restore
  # path for destroyed fleet state was parked somewhere policy marks disposable, under one
  # agent's task name where nobody would think to look.
  BK="$HOME/.smol-ghost-backups/cmdghost-backup-$(date -u +%Y%m%dT%H%M%SZ).txt"
  mkdir -p "$(dirname "$BK")" || { echo "FATAL: cannot create backup dir $(dirname "$BK") — refusing to clear" >&2; exit 2; }
  { echo "# smol COMMAND-ghost backup — $(date -u +%Y-%m-%dT%H:%M:%SZ) — root $ROOT"
    echo "# restore: mosquitto_pub -r -t <topic> -m <payload>"; } > "$BK"
  awk -F'\t' 'NR==FNR{if($2=="DEAD")d[$1];next} ($1 in d){printf "%s\t%s\n",$2,$3}' "$WORK/report" "$WORK/cmds" >> "$BK"
  # 2026-07-28 (audit G1): VERIFY the backup before destroying what it protects. This used to
  # print a reassuring "backup → <path>" and clear the fleet regardless — so a failed write
  # (full disk, bad permissions, unwritable parent) produced a confident path and no rollback.
  # CLAUDE.md's rule is snapshot AND VERIFY before a destructive infra op; this snapshotted and
  # hoped. Checks non-empty AND that it contains at least one topic line, since the header alone
  # would satisfy -s while protecting nothing.
  if [ ! -s "$BK" ] || ! grep -q "^$ROOT/" "$BK"; then
    echo "FATAL: backup at $BK is empty or contains no topic lines — refusing to clear." >&2
    echo "       (nothing was published; the fleet is untouched)" >&2
    exit 2
  fi
  echo; echo "   backup → $BK  ($(grep -c "^$ROOT/" "$BK") topic(s), verified non-empty)"
  echo "   ⚠ config/* clears do NOT revert a board (empty payload is cached + relayed; the leaf's"
  echo "     from_wire keeps its CURRENT value, #46 clamp). Safe here only because these ids are dead."
  # 2026-07-28 (audit G4): a FAILED clear used to be silent — `mpub … && echo cleared` printed
  # nothing on failure, the loop continued, and the exit code was unaffected, so a partial clear
  # looked like a full one minus a line the operator would have to count.
  cfail=0
  awk -F'\t' 'NR==FNR{if($2=="DEAD")d[$1];next} ($1 in d){print $2}' "$WORK/report" "$WORK/cmds" \
  | while read -r t; do
      case "$t" in
        "$ROOT"/*)
          if mpub -i "grec_c_$$_$RANDOM" -r -n -t "$t"; then echo "   cleared $t"
          else echo "   ⚠ FAILED to clear $t — still retained on the broker" >&2; cfail=$((cfail+1)); fi ;;
      esac
    done
  echo "   re-run without --clear to verify."
fi

summary="stale=$stale malformed=$malformed unknown=$unknown duty-cycled=$duty live=$live_ok sentinel=$sentinel"
if [ "$unknown" -gt 0 ]; then
  echo; echo "   VERDICT: COULD NOT CHECK — $unknown command topic(s) on ids of UNKNOWN state (see WHY)."
  echo "            'Can't tell' is not clean. $summary"; exit 2
elif [ "$stale" -gt 0 ] || [ "$malformed" -gt 0 ]; then
  echo; echo "   VERDICT: ACTION NEEDED — $stale stale (aimed at dead ids, can still act on the fleet)," \
            "$malformed malformed."
  echo "            $summary"; exit 1
else
  # `${sentinel:+…}` would fire on the string "0" (non-empty), so test the number.
  extra=""; [ "$sentinel" -gt 0 ] && extra=" (+$sentinel sentinel topic(s), expected — #314)"
  echo; echo "   VERDICT: clean — every retained command topic belongs to a live member${extra}."; exit 0
fi
