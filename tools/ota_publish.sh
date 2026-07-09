#!/usr/bin/env bash
# ota_publish.sh — the smol OTA server-side publish pipeline (issue #6).
#
# Build (or take) an esp-image, host it on the LAN image server, and publish the
# RETAINED announce the boards fetch from. Matches the firmware parse contract
# (ota-firmware-spec.md, LOCKED):
#   topic   smol/ota/announce/<id>   (per-id canary)   ·  smol/ota/announce/all (fleet)
#   payload OTA|<build>|<size>|<sha256hex>|<url>        (url is LAST — contains no '|')
# Plus a NON-acted staging topic  smol/ota/staged  that the HA OTA panel mirrors, so
# the GUI can do per-id canary targeting WITHOUT rebuilding/re-hashing.
#
# MODES
#   ota_publish.sh stage    [<commit>] [--bin <file>] [--build N]           # build+host+publish smol/ota/staged (no board acts)
#   ota_publish.sh push <id> [<commit>] [--bin <file>] [--build N]           # stage, then publish smol/ota/announce/<id> (CANARY — frictionless)
#   ota_publish.sh push all  [<commit>] [--bin <file>] [--build N] [--force] # FLEET — SEATBELTED (see below)
#   ota_publish.sh clear <id|all>                                            # retain-delete the announce (R-P1 lifecycle)
# <commit> defaults to HEAD. --bin <file> skips the cargo build and hosts an existing .bin.
# --build N overrides the git-derived BUILD number in the announce — canary an uncommitted
#   image without a throwaway commit to bump the count (default: `git rev-list --count`).
# --force applies ONLY to `push all`: it skips the interactive typed confirmation (for
# scripted/non-interactive use). The per-id canary `push <id>` is NEVER gated. Without
# --force, `push all` requires typing the exact staged build number to confirm, and
# refuses outright if stdin is not a TTY. (Mass-brick seatbelt — ROADMAP D2.)
#
# SAFETY: canary-first. Prefer `push <id>` (one board), verify it comes back healthy
# (its running build advances — HA reads smol/<id>/status), THEN `push` the rest or use
# the HA "roll out to rest" button. NEVER `push all` blind while bootloader
# revert-on-boot-fail is unproven (ota-firmware-spec §4 / ROADMAP D2).
#
# Broker creds: sourced from the Mosquitto/JuicePassProxy addon option — NEVER printed.
set -euo pipefail

# ---- config (matches the deployed image host + broker legs) -----------------
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLOCK="$REPO/rust/clock"
ESPFLASH="${ESPFLASH:-$HOME/.cargo/bin/espflash}"
# ⚙️ INFRA CONFIG — the defaults below are non-real PLACEHOLDERS (this repo is public).
# Put YOUR real infra in a git-ignored `tools/ota_publish.env` (copy the tracked
# `tools/ota_publish.env.example` → `tools/ota_publish.env`, edit) — it's sourced here if
# present (dotenv-style) and its values fill in the placeholders below, so operators don't
# retype env overrides. Precedence: env file > a var the file leaves unset (pre-set env) >
# placeholder default. Nothing real ever lives in this committed script.
_OTA_ENV="$(dirname "${BASH_SOURCE[0]}")/ota_publish.env"
[ -f "$_OTA_ENV" ] && . "$_OTA_ENV"
OTA_HOST_SSH="${OTA_HOST_SSH:-<ssh-host>}"      # scp target (ssh alias for the image host)
OTA_HOST_IP="${OTA_HOST_IP:-10.0.0.0}"          # image host on the boards' VLAN (same subnet as boards)
OTA_PORT="${OTA_PORT:-8087}"                    # smol-ota static HTTP server port
OTA_REMOTE_DIR="${OTA_REMOTE_DIR:-}"            # absolute; resolved from the remote $HOME if empty
SLOT_MAX=$((0x1F0000))                          # 2,031,616 B — hard ceiling per slot
BROKER="${BROKER:-10.0.0.1}"                    # Mosquitto broker leg reachable from where you run this
MQTT_USER="${MQTT_USER:-<mqtt-user>}"           # broker username (password sourced from the addon, never here)
ADDON="${ADDON:-<addon-slug>}"                  # supervisor addon slug carrying mqtt_password

die(){ echo "ERROR: $*" >&2; exit 1; }
usage(){ sed -n '2,20p' "${BASH_SOURCE[0]}"; exit "${1:-1}"; }

MODE="${1:-}"; [ -n "$MODE" ] || usage 1

# ---- source the broker password (NEVER printed) -----------------------------
mqtt_pw(){
  local tok pw
  tok="$(bw get password ha-llat 2>/dev/null)" || die "bw locked? couldn't read ha-llat"
  pw="$(HA_TOKEN="$tok" python3 "$HOME/Projects/ha/tools/ha_supervisor.py" GET "/addons/$ADDON/info" \
        | python3 -c "import sys,json;print(json.load(sys.stdin)['options']['mqtt_password'])")" \
     || die "couldn't source mqtt_password from addon $ADDON"
  [ -n "$pw" ] || die "empty mqtt_password"
  printf '%s' "$pw"
}

pub_retained(){ # topic, payload  (payload may be empty = retain-delete)
  local topic="$1" payload="$2" pw; pw="$(mqtt_pw)"
  if [ -z "$payload" ]; then
    mosquitto_pub -h "$BROKER" -p 1883 -u "$MQTT_USER" -P "$pw" -r -n -t "$topic"
  else
    mosquitto_pub -h "$BROKER" -p 1883 -u "$MQTT_USER" -P "$pw" -r -t "$topic" -m "$payload"
  fi
}

# ---- clear mode -------------------------------------------------------------
if [ "$MODE" = "clear" ]; then
  TGT="${2:?usage: ota_publish.sh clear <id|all>}"
  pub_retained "smol/ota/announce/$TGT" ""
  echo "cleared retained smol/ota/announce/$TGT"
  exit 0
fi

[ "$MODE" = "stage" ] || [ "$MODE" = "push" ] || usage 1
if [ "$MODE" = "push" ]; then TARGET="${2:?usage: ota_publish.sh push <id|all> [<commit>]}"; shift 2; else shift 1; fi
COMMIT="HEAD"; BIN=""; FORCE=0; BUILD_OVERRIDE=""
while [ $# -gt 0 ]; do case "$1" in
  --bin) BIN="${2:?}"; shift 2;;
  --build) BUILD_OVERRIDE="${2:?}"; shift 2;;
  --force) FORCE=1; shift;;
  *) COMMIT="$1"; shift;;
esac; done

# ---- identity (matches build.rs deploy contract; archive builds have no .git) -
cd "$REPO"
HASH="$(git rev-parse --short=7 "$COMMIT")"
BUILD="$(git rev-list --count "$COMMIT")"
# --build N overrides the git-derived count — lets an operator canary an UNCOMMITTED image
# without a throwaway commit to bump the number (collision-risky on a busy repo). It becomes
# the announce BUILD the firmware compares (monotonicity), so it must be a positive integer.
if [ -n "$BUILD_OVERRIDE" ]; then
  case "$BUILD_OVERRIDE" in ''|*[!0-9]*) die "--build must be a positive integer (got '$BUILD_OVERRIDE')";; esac
  BUILD="$BUILD_OVERRIDE"
fi

# ---- build (or take a prebuilt .bin) ----------------------------------------
if [ -z "$BIN" ]; then
  echo "building espnow release @ $HASH (build $BUILD) ..."
  ( cd "$CLOCK" && SMOL_GIT_HASH="$HASH" SMOL_BUILD_NUMBER="$BUILD" \
      cargo build --release --features espnow )
  ELF="$CLOCK/target/riscv32imc-unknown-none-elf/release/clock"
  BIN="/tmp/smol-${BUILD}.bin"
  "$ESPFLASH" save-image --chip esp32c3 "$ELF" "$BIN" >/dev/null
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

LINE="OTA|${BUILD}|${SIZE}|${SHA}|${URL}"

# ---- publish: always stage; push targets the acted announce topic -----------
pub_retained "smol/ota/staged" "$LINE"
echo "staged  smol/ota/staged  <-  build $BUILD ($HASH) ${SIZE}B sha ${SHA:0:12}… @ $URL"
if [ "$MODE" = "push" ]; then
  # SEATBELT — fleet path ONLY. The per-id canary (`push <id>`) never enters this block.
  if [ "$TARGET" = "all" ] && [ "$FORCE" != "1" ]; then
    if [ -t 0 ]; then
      echo "⚠️  FLEET push of build $BUILD to ALL boards — mass-brick risk (bootloader"
      echo "    revert-on-boot-fail is UNPROVEN on hardware; ROADMAP D2). Prefer canary."
      printf "    Type the build number (%s) to confirm, anything else aborts: " "$BUILD"
      read -r ans
      [ "$ans" = "$BUILD" ] || die "fleet push aborted — confirmation ('$ans') != build $BUILD"
    else
      die "fleet push to ALL refused without --force (stdin not a TTY). Prefer canary: ota_publish.sh push <id>. Use --force only if you truly mean a fleet push."
    fi
  fi
  pub_retained "smol/ota/announce/$TARGET" "$LINE"
  echo "PUSHED  smol/ota/announce/$TARGET  (boards on that id act on next burst)"
  [ "$TARGET" = "all" ] && echo "⚠️  FLEET push done — verify EACH board comes back healthy (its running build advances)."
fi
echo "done. HA OTA panel mirrors smol/ota/staged; use its per-node canary buttons, or: ota_publish.sh push <id> --bin $BIN"
