#!/usr/bin/env bash
# ota_publish.sh — the smol OTA server-side publish pipeline (issue #6).
#
# Build (or take) an esp-image, host it on the LAN image server, and publish the
# RETAINED staged line every board's native HA Update entity reads as latest_version
# (Model-A #33). Matches the firmware parse contract (issue-33-modelA-design.md):
#   topic   smol/ota/staged   (retained; arms ALL boards, triggers NO fetch)
#   payload OTA|<build>|<size>|<sha256hex>|<url>        (url is LAST — contains no '|')
# Install is per-device: HA's native Update `Install` button (or `install <id>` here)
# publishes INSTALL → smol/<id>/ota/install; only that board fetches the staged image.
# The per-id announce act-path is RETIRED (Model-A #32 closure — no fleet-fetch topic).
#
# MODES (Model-A #33: stage arms every board's native Update entity; Install is per-device)
#   ota_publish.sh stage      [<commit>] [--bin <file>] [--build N]  # build+host+publish smol/ota/staged (arms all boards; NO board fetches)
#   ota_publish.sh install <id>                                      # publish INSTALL → smol/<id>/ota/install (headless per-node canary; the HA Update button is the GUI path). id42 is REFUSED (#314: C6 watch unset-config sentinel, not a node).
# <commit> defaults to HEAD. --bin <file> skips the cargo build and hosts an existing .bin.
# BUILD number (the staged-line monotonicity value the fw compares): stage RATCHETS it forward —
#   build = max(`git rev-list --count`, <retained smol/ota/staged build> + 1) — so a prior canary
#   pin (a --build N left ahead of the count) HEALS the fleet number forward automatically instead
#   of poisoning the gate (issue #128). Broker unreachable → falls back to the raw count with a
#   WARNING (no ratchet). --build N still forces an explicit override (canary an uncommitted image
#   with no throwaway commit); N is used AS-IS and, when N > count, prints a loud canary-pin
#   warning + the heal path. See choose_build() (unit-tested by tools/test_ota_ratchet.sh).
#
# SAFETY: canary is STRUCTURAL now — Install is per-device (native Update entity); there
# is no fleet-fetch topic (Model-A #32 closure). Install one board, verify its version
# advances (a graceful-fail re-shows update-available), THEN the next. NEVER script all
# three Installs at once while bootloader revert-on-boot-fail is unproven (ROADMAP D2).
#
# Broker creds: sourced from the Mosquitto/JuicePassProxy addon option — NEVER printed.
set -euo pipefail

# ---- config (matches the deployed image host + broker legs) -----------------
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLOCK="$REPO/rust/clock"
ESPFLASH="${ESPFLASH:-$HOME/.cargo/bin/espflash}"
# #44: reproducible-build helpers — the release build below goes through repro_build_bin so
# the announced sha256 is a stable, verifiable (commit) identity (see tools/verify_image.sh).
# shellcheck source=tools/repro_build.sh
. "$(dirname "${BASH_SOURCE[0]}")/repro_build.sh"
# ⚙️ INFRA CONFIG — the defaults below are non-real PLACEHOLDERS (this repo is public).
# Put YOUR real infra in a git-ignored `tools/ota_publish.env` (copy the tracked
# `tools/ota_publish.env.example` → `tools/ota_publish.env`, edit) — it's sourced here if
# present (dotenv-style) and its values fill in the placeholders below, so operators don't
# retype env overrides. Precedence: env file > a var the file leaves unset (pre-set env) >
# placeholder default. Nothing real ever lives in this committed script.
_OTA_SELF_DIR="$(dirname "${BASH_SOURCE[0]}")"
_OTA_ENV="$_OTA_SELF_DIR/ota_publish.env"
# #128: the infra env is git-ignored, so it lives ONLY in the operator's MAIN checkout — a linked
# git worktree (or a fresh clone) has no copy, and the tool then silently fell back to the
# PLACEHOLDER broker (10.0.0.1). Resolve worktree-robustly: prefer a local tools/ota_publish.env,
# else the MAIN worktree's copy via git-common-dir (its dir strips the trailing /.git). This makes
# install AND the new ratchet-read reach the REAL broker from any worktree, resolved up-front (once,
# before any mode runs) so every code path — install and stage — reads the identical $BROKER.
if [ ! -f "$_OTA_ENV" ]; then
  _OTA_COMMON="$(git -C "$_OTA_SELF_DIR" rev-parse --git-common-dir 2>/dev/null)" || _OTA_COMMON=""
  case "$_OTA_COMMON" in
    /*) _OTA_MAIN_ENV="${_OTA_COMMON%/.git}/tools/ota_publish.env"
        [ -f "$_OTA_MAIN_ENV" ] && _OTA_ENV="$_OTA_MAIN_ENV" ;;
    *)  : ;;  # empty (not a git dir) or relative (already IN the main tree) → nothing to fall back to
  esac
fi
# shellcheck source=/dev/null  # operator-supplied, git-ignored, path known only at runtime
[ -f "$_OTA_ENV" ] && . "$_OTA_ENV"
OTA_HOST_SSH="${OTA_HOST_SSH:-<ssh-host>}"      # scp target (ssh alias for the image host)
OTA_HOST_IP="${OTA_HOST_IP:-10.0.0.0}"          # image host on the boards' VLAN (same subnet as boards)
OTA_PORT="${OTA_PORT:-8087}"                    # smol-ota static HTTP server port
OTA_REMOTE_DIR="${OTA_REMOTE_DIR:-}"            # absolute; resolved from the remote $HOME if empty
SLOT_MAX=$((0x1F0000))                          # 2,031,616 B — hard ceiling per slot
BROKER="${BROKER:-10.0.0.1}"                    # Mosquitto broker leg reachable from where you run this
MQTT_USER="${MQTT_USER:-<mqtt-user>}"           # broker username (password sourced from the addon, never here)
ADDON="${ADDON:-<addon-slug>}"                  # supervisor addon slug carrying mqtt_password
SMOL_OTA_SIGNING_KEY_ITEM="${SMOL_OTA_SIGNING_KEY_ITEM:-smol-ota-signing-ed25519}"  # Vaultwarden secureNote holding the ed25519 signing PEM (#32)

die(){ echo "ERROR: $*" >&2; exit 1; }
usage(){ sed -n '2,23p' "${BASH_SOURCE[0]}"; exit "${1:-1}"; }

MODE="${1:-}"; [ -n "$MODE" ] || usage 1

# ---- source the broker password (NEVER printed) -----------------------------
# #128: memoize — the ratchet's retained-read AND the publish both need the pw; without this
# they'd each hit bw + the HA supervisor (slow + two failure points). Cached in-process only.
_MQTT_PW=""
mqtt_pw(){
  [ -n "$_MQTT_PW" ] && { printf '%s' "$_MQTT_PW"; return 0; }
  local tok pw
  tok="$(bw get password ha-llat 2>/dev/null)" || die "bw locked? couldn't read ha-llat"
  pw="$(HA_TOKEN="$tok" python3 "$HOME/Projects/ha/tools/ha_supervisor.py" GET "/addons/$ADDON/info" \
        | python3 -c "import sys,json;print(json.load(sys.stdin)['options']['mqtt_password'])")" \
     || die "couldn't source mqtt_password from addon $ADDON"
  [ -n "$pw" ] || die "empty mqtt_password"
  _MQTT_PW="$pw"
  printf '%s' "$pw"
}

# ---- #313: credentials reach mosquitto via a private config dir, NEVER argv --
# `-P <pw>` is world-readable through /proc/<pid>/cmdline for the life of the process — any
# local process running `ps -o args` reads it (that is how #313 was found, verbatim, off
# another agent's subscriber). mosquitto_{pub,sub} read default options from
# $XDG_CONFIG_HOME/mosquitto_{pub,sub}, one flag per line, so the password never enters argv.
# Same shape as tools/ota_capture.sh, which already solved this for the capture path.
#
# CALL THIS AS A STATEMENT, never `$(mqtt_cfg)`. #128's memoization was silently defeated by
# exactly that: every `$(mqtt_pw)` ran in a SUBSHELL, so the `_MQTT_PW` it set died with the
# subshell and each call re-hit `bw` + the HA supervisor (measured: 3 sourcings per stage, not
# the 1 the comment intends). Setting the global in THIS shell is what makes the password —
# and this directory — genuinely once-per-run.
_MQTT_CFG=""
_mqtt_cfg_cleanup(){ [ -n "$_MQTT_CFG" ] && rm -rf "$_MQTT_CFG"; return 0; }
mqtt_cfg(){
  [ -n "$_MQTT_CFG" ] && return 0
  local pw d f; pw="$(mqtt_pw)"
  d="$(mktemp -d)"; chmod 700 "$d"
  for f in mosquitto_pub mosquitto_sub; do
    printf -- '-h %s\n-u %s\n-P %s\n' "$BROKER" "$MQTT_USER" "$pw" > "$d/$f"
    chmod 600 "$d/$f"
  done
  _MQTT_CFG="$d"
  trap _mqtt_cfg_cleanup EXIT INT TERM
}

# ---- #314: reserved ids that are NEVER an OTA target ------------------------
# 42 is not a node. It is the C6 watch's unset-config sentinel: every watch boots with
# `watch_cfg.node_id == 42` and esp32c6-watch #34 remaps it to a MAC-derived id, so 42 is a
# transient alias that TWO DIFFERENT WATCHES can publish under, at different times. An install
# aimed at 42 is aimed at an unknown board — and the firmware comment records that this very
# collision has already broken MQTT windows in the field. It also RECURS BY DESIGN (any watch
# booting unprovisioned republishes it), so this cannot be closed by clearing the ghost once.
#
# REFUSE, never skip-with-a-warning: the same discipline as the never-flash MAC allowlist. A
# warning that is followed by a publish is not a guard, and the operator would have no way to
# tell an armed-the-wrong-board from an armed-nothing afterwards.
#
# Called BEFORE any credential sourcing or publish, so a refusal is guaranteed to have published
# nothing. Exit 22 = client error (the request itself is invalid), distinct from the 5 an actual
# failed arm returns. Defense-in-depth only — #349's image target descriptor is what makes a
# watch refuse a C3 image it somehow receives; this stops the aim, not the shot.
assert_ota_targetable(){ # <id> — returns 0, or prints the refusal and exits 22
  case "$1" in
    42)
      {
        echo "REFUSED: id42 is NOT a node — it is the C6 watch's unset-config sentinel (#314)."
        echo "  Every watch boots with node_id 42 until #34 remaps it to its MAC-derived id, so 42"
        echo "  is a transient alias two different watches can publish under at different times."
        echo "  An install aimed at 42 is aimed at an unknown board. NOTHING WAS PUBLISHED."
        echo "  Fix: find the watch's REAL id (its MAC-derived id — read it off smol/<id>/diag or"
        echo "  the crown roster) and install that. A device still publishing as 42 is UNPROVISIONED:"
        echo "  provision it, do not OTA it."
      } >&2
      exit 22 ;;
  esac
  return 0
}

pub_retained(){ # topic, payload  (payload may be empty = retain-delete)
  local topic="$1" payload="$2"
  mqtt_cfg
  if [ -z "$payload" ]; then
    XDG_CONFIG_HOME="$_MQTT_CFG" mosquitto_pub -p 1883 -r -n -t "$topic"
  else
    XDG_CONFIG_HOME="$_MQTT_CFG" mosquitto_pub -p 1883 -r -t "$topic" -m "$payload"
  fi
}

# ---- #128: read the retained staged BUILD (for the ratchet) ------------------
# Prints the current retained `smol/ota/staged` build number (field 2 of OTA|<build>|…) with
# NO trailing newline, or nothing if the topic is empty / carries a non-OTA payload. Returns 0
# when the broker was reachable (record found OR topic empty), 1 when the broker is UNREACHABLE
# (so the caller can WARN + fall back to the raw count). A retained message arrives immediately
# on subscribe, so -C 1 returns in ms; -W 3 bounds an empty topic. The reachable-but-empty case
# and the unreachable case both exit non-zero, so we disambiguate on the stderr text (a real
# connect failure always prints one of these; a bare -W timeout on an empty topic does not).
read_staged_build(){
  local msg rc err
  mqtt_cfg
  err="$(mktemp)"
  msg="$(XDG_CONFIG_HOME="$_MQTT_CFG" mosquitto_sub -p 1883 -C 1 -W 3 \
        -t "smol/ota/staged" 2>"$err")" && rc=0 || rc=$?
  if [ "$rc" -ne 0 ]; then
    if grep -qiE 'connection refused|connection error|unable to connect|getaddrinfo|unknown host|name or service|network is unreachable|no route to host|not authorised|error: connect' "$err"; then
      rm -f "$err"; return 1   # broker unreachable / auth-failed → caller falls back to count
    fi
    rm -f "$err"; return 0      # connected, empty topic (no prior stage) → no build, reachable
  fi
  rm -f "$err"
  case "$msg" in
    OTA\|*) printf '%s' "$msg" | cut -d'|' -f2 | tr -d '\n' ;;  # field 2 = build
    *) : ;;                                                     # non-OTA payload → treat as none
  esac
  return 0
}

# ---- #128: choose the staged BUILD number (pure decision — unit-tested) ------
# Args: <commit-count> <retained-staged-build|""> <override|"">  → echoes the BUILD to stage;
# warnings/notes go to STDERR only. Kept side-effect-free so tools/test_ota_ratchet.sh can
# exercise every branch without a broker, a build, or a publish.
#
# INCIDENT (2026-07-14, issue #128): canary staging with --build 300/320/330 left id8 pinned
# NUMERICALLY AHEAD of the honest commit count (254); honest-numbered stages then read as
# NotNewer and #120's cleanup (correctly) cleared their orders → id8 silently refused real
# updates until a 331 re-pin. The ratchet below (build = max(count, staged+1)) makes the fleet
# number heal FORWARD automatically instead of poisoning the monotonicity gate.
choose_build(){
  local count="$1" staged="$2" override="$3" build
  if [ -n "$override" ]; then
    # Explicit operator override (canary an uncommitted image without a throwaway commit).
    # Used AS-IS — but if it out-runs the honest count it re-creates the #128 incident, so warn.
    build="$override"
    if [ "$override" -gt "$count" ]; then
      cat >&2 <<WARN
⚠️  #128: --build $override is AHEAD of the honest commit count ($count). This PINS the fleet's
    monotonicity gate above main — honest-numbered stages will read as NotNewer (and #120 cleanup
    clears their orders) until main's count passes $override or the board is USB-reflashed.
    HEAL PATH: stage ONE more pinned build (> the pinned board's current) to converge it, then
    numbering self-heals at the next USB access / once the commit count overtakes the pin.
WARN
    fi
  else
    # Ratchet: never regress below the retained record — heal forward past any prior canary pin.
    build="$count"
    if [ -n "$staged" ] && [ "$((staged + 1))" -gt "$build" ]; then
      build="$((staged + 1))"
      echo "note: #128 ratchet — retained staged build ($staged) is ahead of the commit count ($count);" \
           "staging $build to heal the fleet number forward past a prior canary pin." >&2
    fi
  fi
  printf '%s' "$build"
}

# ---- install mode (Model-A per-node canary; parity with the HA Update button) --
if [ "$MODE" = "install" ]; then
  ID="${2:?usage: ota_publish.sh install <id>}"
  case "$ID" in ''|*[!0-9]*) die "install <id>: id must be a positive integer (got '$ID')";; esac
  assert_ota_targetable "$ID"   # #314 — refuses the id42 watch sentinel before anything is published
  # RETAINED (-r): the fw does a retained-read on subscribe (wifi.rs:1126); a non-retained INSTALL
  # is missed by id7's bursty subscribe window (lucid A/B: retained→fetch 6s; non-retained→miss).
  # Idempotent: fw gate is staged.build > running, so a retained re-fire won't re-install same build.
  # 2026-07-28: the publish result MUST be checked. This block used to run mosquitto_pub,
  # print the success line unconditionally, and `exit 0` — so a failed arm announced itself
  # as an arm. Hit twice in one rollout ("Error: The connection was refused", transient
  # broker pressure): the operator sees an error on stderr amid other output, any caller
  # reading $? sees success, and the board simply never updates. An arm that silently
  # doesn't arm is the worst failure this tool can have, because the symptom is a board
  # that stays on the old build with nothing reporting why.
  mqtt_cfg
  if ! XDG_CONFIG_HOME="$_MQTT_CFG" mosquitto_pub -p 1883 \
        -r -t "smol/${ID}/ota/install" -m "INSTALL"; then
    echo "FAILED to arm id${ID}: the INSTALL publish did not succeed — id${ID} is NOT armed." >&2
    echo "  retry; if it persists check broker reachability (transient refusals seen under load)." >&2
    exit 5
  fi
  echo "install  smol/${ID}/ota/install  <-  INSTALL (RETAINED — id${ID} reliably catches it; fetches STAGED if staged.build>running)"
  exit 0
fi

[ "$MODE" = "stage" ] || usage 1
shift 1
COMMIT="HEAD"; BIN=""; BUILD_OVERRIDE=""
while [ $# -gt 0 ]; do case "$1" in
  --bin) BIN="${2:?}"; shift 2;;
  --build) BUILD_OVERRIDE="${2:?}"; shift 2;;
  *) COMMIT="$1"; shift;;
esac; done

# ---- identity (matches build.rs deploy contract; archive builds have no .git) -
cd "$REPO"
HASH="$(git rev-parse --short=7 "$COMMIT")"
COUNT="$(git rev-list --count "$COMMIT")"
# #128: --build N stays an explicit operator override (canary an UNCOMMITTED image with no
# throwaway commit to bump the count). Must be a positive integer.
if [ -n "$BUILD_OVERRIDE" ]; then
  case "$BUILD_OVERRIDE" in ''|*[!0-9]*) die "--build must be a positive integer (got '$BUILD_OVERRIDE')";; esac
fi
# #128 RATCHET: with no override, stage build = max(commit count, retained staged build + 1) so
# a prior canary pin heals the fleet number FORWARD instead of poisoning the monotonicity gate
# (see choose_build + the incident note there). Only the ratchet path needs the broker read; an
# explicit override skips it. Broker unreachable → WARN and fall back to the raw count.
STAGED=""
if [ -z "$BUILD_OVERRIDE" ]; then
  if STAGED="$(read_staged_build)"; then :; else
    echo "WARNING: #128 — broker $BROKER unreachable; can't read retained smol/ota/staged." >&2
    echo "         Falling back to the raw commit count ($COUNT) with NO ratchet — if a prior" >&2
    echo "         canary pin is live, this stage may re-collide with it (read fw DIAG to confirm)." >&2
    STAGED=""
  fi
fi
BUILD="$(choose_build "$COUNT" "$STAGED" "$BUILD_OVERRIDE")"

# ---- build (or take a prebuilt .bin) ----------------------------------------
# #40 IDENTITY — the staged image is FLEET-SHARED BY DESIGN: it is built with NO
# SMOL_NODE_ID, so it bakes the board.rs default id (7). That default is ONLY a factory
# seed — every radio node reads its TRUE id from the `nvs` partition at runtime
# (ota.rs::resolve_node_id, seeded on the first USB boot after an erase-flash). OTA never
# touches `nvs`, so a single image installs onto id7/id8/id9/... and each KEEPS its own
# identity. DO NOT add SMOL_NODE_ID here (that would re-fragment one image per node); and
# do NOT USB-flash this staged .bin as a factory image without SMOL_NODE_ID=<n>, or a
# fresh (erased) board would seed NVS to the default id 7.
# #44 REPRODUCIBLE — repro_build_bin pins the version stamp (as before) AND remaps absolute
# build paths + pins SOURCE_DATE_EPOCH, so the same commit built anywhere yields the same
# bytes → the SHA below is a stable identity an operator can pre/post-flash verify with
# `verify_image.sh <commit>`. No node-id here is consistent with the fleet-shared design
# above: ONE reproducible image, one sha per commit for the whole fleet.
if [ -z "$BIN" ]; then
  echo "building reproducible espnow release @ $HASH (build $BUILD) ..."
  BIN="/tmp/smol-${BUILD}.bin"
  # #326: staging IS the release act, so stamp it as one HERE rather than hoping the
  # operator remembered `export SMOL_RELEASE=1`. Before this line the release-vs-dev stamp
  # of a STAGED image depended on the operator's shell: repro_build.sh's comment said "the
  # caller sets SMOL_RELEASE=1" and no caller in the repo ever did — 913/915 shipped
  # release-stamped only because operators exported it by hand. A canary of an uncommitted
  # image still goes through --bin, which skips this build path entirely, so dev images
  # cannot masquerade: this export never touches them.
  SMOL_RELEASE=1 repro_build_bin "$CLOCK" "$BIN" "$HASH" "$BUILD" || die "reproducible build failed"
fi
[ -f "$BIN" ] || die "no image at $BIN"

# ---- metadata + HARD slot-fit gate ------------------------------------------
SIZE="$(stat -c%s "$BIN")"
SHA="$(sha256sum "$BIN" | cut -d' ' -f1)"
[ "$SIZE" -le "$SLOT_MAX" ] || die "image $SIZE B > slot $SLOT_MAX B (0x1F0000) — WILL NOT FIT, aborting"

# ---- host on the LAN image server (VLAN11, same subnet as boards) ------------
# Resolve the remote dir absolutely — scp's SFTP protocol does NOT expand remote $HOME.
[ -n "$OTA_REMOTE_DIR" ] || OTA_REMOTE_DIR="$(ssh "$OTA_HOST_SSH" 'printf %s "$HOME/smol-ota/ota"')"
REMOTE="smol-${BUILD}.bin"
ssh "$OTA_HOST_SSH" "mkdir -p '$OTA_REMOTE_DIR'"
scp -q "$BIN" "$OTA_HOST_SSH:$OTA_REMOTE_DIR/$REMOTE"
URL="http://${OTA_HOST_IP}:${OTA_PORT}/ota/${REMOTE}"

# ---- #32: ed25519-sign M = "build|size|sha256" (the fw verifies this EXACT string) ----------
# openssl Ed25519 is ONESHOT → SEEKABLE FILES only (stdin/process-sub fail: "unable to determine
# file size for oneshot operation"). Key from Vault → temp file in RAM (/dev/shm), shredded right
# after signing (never echoed). printf (NOT echo): M must be the exact wire bytes, no newline.
_msgf="$(mktemp)"; _keyf="$(mktemp -p /dev/shm 2>/dev/null || mktemp)"
# Shred the key/msg temps even on interrupt (SIGINT/TERM) in the window before the
# inline shred below — else a Ctrl-C mid-sign could leave the key in /dev/shm.
# Carries `_mqtt_cfg_cleanup` too (#313): bash traps are REPLACED, not stacked, so setting an
# EXIT trap here would otherwise silently drop the one mqtt_cfg installed and leave the
# credential dir behind on every stage.
trap 'shred -u "$_msgf" "$_keyf" 2>/dev/null; _mqtt_cfg_cleanup' EXIT INT TERM
printf '%s' "${BUILD}|${SIZE}|${SHA}" > "$_msgf"
bw get notes "$SMOL_OTA_SIGNING_KEY_ITEM" > "$_keyf" 2>/dev/null \
  || { shred -u "$_msgf" "$_keyf" 2>/dev/null; die "bw: couldn't read signing key '$SMOL_OTA_SIGNING_KEY_ITEM' (locked?)"; }
SIG="$(openssl pkeyutl -sign -rawin -inkey "$_keyf" -in "$_msgf" | xxd -p -c 64)"
shred -u "$_msgf" "$_keyf" 2>/dev/null
case "$SIG" in *[!0-9a-f]*|"") die "ed25519 signing failed (empty/non-hex sig — openssl >=3.0 + valid key?)";; esac
[ "${#SIG}" -eq 128 ] || die "ed25519 sig wrong length ${#SIG} (want 128 hex)"

# 6-field SIGNED announce (was 4-field unsigned): url stays LAST (may contain no '|').
LINE="OTA|${BUILD}|${SIZE}|${SHA}|${SIG}|${URL}"

# ---- publish: stage the retained line (arms every board's native Update) -----
pub_retained "smol/ota/staged" "$LINE"
echo "staged  smol/ota/staged  <-  build $BUILD ($HASH) ${SIZE}B sha ${SHA:0:12}… sig ${SIG:0:12}… @ $URL"
echo "done. Every board's native HA Update entity now shows build $BUILD as available."
echo "      Install per-node from HA (the Update entity's Install button) or: ota_publish.sh install <id>"
