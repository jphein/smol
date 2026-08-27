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
#   ota_publish.sh legacy-line <chip>                              # PREFLIGHT (#464): will an image for <chip> be published on the fleet-wide smol/ota/staged line? exit 0 = yes, 1 = skipped (non-canonical chip)
# <commit> defaults to HEAD. --bin <file> skips the cargo build and hosts an existing .bin.
# IDENTITY (#400): stage BUILDS THE WORKING TREE — it cannot build a commit's source. So a
#   <commit> that is not HEAD is REFUSED (it would stamp that commit's identity onto these bytes),
#   and DIRTY tracked build inputs under rust/clock are REFUSED unless you pass --dirty, which
#   builds a DEV-stamped image (vN+dev.<hash>) since its sha is reproducible from no commit.
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
# Staged images and any scratch land in the REPO, never /tmp (JP directive 2026-08-25) — katana's
# /tmp is a 16 GB tmpfs (RAM+swap), and a staged image is ~1.2 MB that an operator may well want to
# still exist after the shell that made it. `$REPO/tmp` is git-ignored (tmp/.gitignore) and on disk.
# NOTE: `die` is defined further down, so this uses a plain exit — the config block runs before
# the helpers exist.
SMOL_TMP="$REPO/tmp"
mkdir -p "$SMOL_TMP" || { echo "ota_publish: cannot create $SMOL_TMP" >&2; exit 1; }
export TMPDIR="$SMOL_TMP"
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
# ⚠️ The line range is a DUPLICATED FACT about the header block's extent — edit the header and it
# rots silently (help output truncates mid-sentence, and nobody reads help on the happy path).
# test_ota_publish_guards.sh pins both ends so an edit that forgets this fails instead.
usage(){ sed -n '2,28p' "${BASH_SOURCE[0]}"; exit "${1:-1}"; }   # 27→28: #464 added a MODES line

MODE="${1:-}"; [ -n "$MODE" ] || usage 1

# ---- source the broker password (NEVER printed) -----------------------------
# #128: memoize — the ratchet's retained-read AND the publish both need the pw; without this
# they'd each hit bw + the HA supervisor (slow + two failure points). Cached in-process only.
_MQTT_PW=""
mqtt_pw(){
  [ -n "$_MQTT_PW" ] && { printf '%s' "$_MQTT_PW"; return 0; }
  local tok pw
  # Keep bw's stderr: "Not found." (wrong server / missing item) and a locked
  # vault are DIFFERENT failures that this die used to report as one — a fresh
  # `bw login` that lands on the default cloud server (serverUrl bitwarden.com,
  # not vault.jphe.in) produces exactly this, and on 2026-08-26 it cost an hour
  # of unlock loops chasing "locked" while the truth was "wrong vault". Check
  # `bw config server` when the message says Not found.
  local _bwerr _bwmsg
  _bwerr="$(mktemp)"
  if ! tok="$(bw get password ha-llat 2>"$_bwerr")"; then
    _bwmsg="$(head -c 200 "$_bwerr" | tr '\n' ' ')"
    rm -f "$_bwerr"
    die "couldn't read ha-llat — bw said: '${_bwmsg}' ('Not found.' = wrong server or missing item, NOT locked — check \`bw config server\`)"
  fi
  rm -f "$_bwerr"
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

# ---- #400: the stamp must describe the bytes ---------------------------------
# ROOT CAUSE, stated once: the STAMP is a function of a git ref (HASH/COUNT come from <commit>);
# the BYTES are a function of the WORKING TREE (repro_build_bin builds $CLOCK). Nothing asserted
# that those two describe the same source, so the tool whose job is identity could mint a false one.
#
# Observed live (the #335 round-trip canary): `stage 1a6349e` published an image stamped
# `build 919 (1a6349e)` carrying the CURRENT tree's size (byte-identical to the HEAD build staged
# minutes earlier), the current stack region (106,480 B — that era's code reads ~75K), and a #349
# target descriptor July-28 code cannot emit. Honest-looking, false provenance.
#
# TWO refusals, because the filed instance is only the LOUD half of one defect:
#   1. a non-HEAD <commit> — the tool cannot build a commit's source, so it must not claim to.
#   2. DIRTY build inputs — `stage` with no argument, the path every caller in this repo actually
#      uses, stamped HEAD's hash onto uncommitted bytes AND force-exported SMOL_RELEASE=1, so an
#      uncommitted canary shipped stamped as a clean release. Fixing only (1) would have left (2)
#      live on the common path — a guard on one branch and absent on its sibling.

# stage_input_dirt — print the DIRTY TRACKED build inputs (empty output = clean).
# THE SCOPE IS THE CHECK. Every input whose change alters the image bytes lives under rust/clock:
# src/, build.rs, Cargo.toml, Cargo.lock, version.txt, rust-toolchain.toml, .cargo/config.toml.
# A repo-wide check would refuse an unrelated docs or tools edit, and a gate that fires on innocent
# states is a gate operators learn to route around (#338) — fatal for one guarding a publish.
# --untracked-files=no is LOAD-BEARING, not tidiness: board.rs and secrets.rs are git-ignored BY
# DESIGN (.gitignore:27,35) and are therefore permanently untracked, so counting untracked files
# would make every tree dirty forever and the guard would refuse every stage there is.
stage_input_dirt(){
  git -C "${REPO:-.}" status --porcelain --untracked-files=no -- rust/clock
}

assert_stamp_is_head(){ # <commit> — returns 0, or prints the refusal and exits 22
  local commit="$1" want have
  if ! want="$(git -C "${REPO:-.}" rev-parse --verify --quiet "${commit}^{commit}")"; then
    echo "REFUSED: '$commit' is not a commit in this repository. NOTHING WAS PUBLISHED." >&2
    exit 22
  fi
  have="$(git -C "${REPO:-.}" rev-parse --verify --quiet 'HEAD^{commit}')" || {
    echo "REFUSED: cannot resolve HEAD. NOTHING WAS PUBLISHED." >&2; exit 22; }
  [ "$want" = "$have" ] && return 0
  {
    echo "REFUSED: stage cannot build '$commit' — it builds the WORKING TREE (#400)."
    echo "  asked to stamp   $(git -C "${REPO:-.}" rev-parse --short=7 "$want")"
    echo "  tree is actually $(git -C "${REPO:-.}" rev-parse --short=7 "$have")"
    echo "  Stamping a commit's identity onto different bytes is what this refusal exists to stop:"
    echo "  it yields an honest-LOOKING image with a FALSE provenance stamp, and the only reason the"
    echo "  live instance was caught was an operator cross-checking the numbers by hand."
    echo "  NOTHING WAS PUBLISHED."
    echo "  Fix: check the commit out and stage from THERE, so the bytes match the stamp:"
    echo "    git worktree add ../smol-stage '$commit' && cd ../smol-stage && tools/ota_publish.sh stage"
    echo "  (a linked worktree has no tools/ota_publish.env of its own; the #128 resolver reads the"
    echo "  main worktree's copy, so the broker config still resolves.)"
  } >&2
  exit 22
}

assert_stampable_inputs(){ # <allow_dirty:0|1> — returns 0, or prints the refusal and exits 22
  local allow="$1" dirt
  dirt="$(stage_input_dirt)"
  [ -z "$dirt" ] && return 0
  [ "$allow" = 1 ] && return 0
  {
    echo "REFUSED: the tracked build inputs under rust/clock differ from HEAD (#400)."
    echo "$dirt" | sed 's/^/    /'
    echo "  An image built now gets HEAD's identity stamped on source that is not HEAD, so the"
    echo "  announced sha256 is reproducible from no commit and verify_image.sh cannot confirm it."
    echo "  NOTHING WAS PUBLISHED."
    echo "  Fix: commit (or stash) the change and re-stage."
    echo "  Or, to canary uncommitted work on purpose:  --dirty"
    echo "    That builds a DEV-stamped image (vN+dev.<hash>) rather than a clean release stamp,"
    echo "    because the bytes answer to no commit — the stamp then says what the image IS."
  } >&2
  exit 22
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

# ---- #464: does the FLEET-WIDE legacy line belong to this image? ---------------------------------
#
# THE COST THIS AVOIDS, measured on 2026-08-26 during the S3's first roll: staging an esp32s3 image
# wrote its build to `smol/ota/staged`, the v922 C3 crown (id50) gated that line (no target field),
# accepted it (1404 > 922) and **self-fetched the full ~1 MB S3 image**, refusing it only at the
# finalize descriptor read. Every legacy crown pays ~1 MB of fetch plus flash-write wear per
# foreign-chip stage, and the refusal is at the very end.
#
# WHY SKIPPING IS SAFE, and it rests on one fact rather than on a fleet audit: the legacy line exists
# for firmware older than #349, which knows only `smol/ota/staged` and only parses `OTA|`. **All such
# firmware is on esp32c3** — the S3 and C5 targets were created after #349 (#398 and #388 both cite
# it as prior art), so a pre-#349 non-C3 board has never existed and cannot. A foreign-chip build on
# the legacy line therefore cannot serve any board: #349-aware boards of that chip read their
# per-chip line, and non-#349-aware boards of that chip do not exist. The only thing it can do is
# cost a legacy C3 crown a megabyte.
#
# This is DELIBERATELY NARROWER than #464's own fix direction ("retire the fleet-wide line once no
# pre-per-chip firmware remains"). That retirement needs a rolled fleet and an audit; this needs
# neither, because it keeps the legacy line for exactly the images a legacy board could install.
#
# ⚠️ IT DOES NOT FIX #472 and makes its symptom deterministic rather than accidental. #472 is that a
# board's `ota/state` composer prefers the fleet line, so an S3 whose per-chip line is newer still
# advertises the fleet line's (C3) build as `latest`. After this change the fleet line reliably holds
# a canonical-chip build, which is what makes #472's consumer-side fix well-defined — but the S3's
# `latest=<c3 build>` stops being the residue of a manual re-stage and becomes the steady state.
# That trade is deliberate: #472's own body records its symptom as cosmetic (the monotonic ratchet
# refuses the lower build), while the megabyte is spent on every foreign-chip stage.
#
# Pure and exit-code-only so the decision is unit-testable without a broker, a vault or an image —
# the same reason `should_group_mac` is a pure function rather than an inline condition. Exercised by
# `ota_publish.sh legacy-line <chip>` and by tools/test_ota_publish_guards.sh.
legacy_line_wanted() { # <target-chip> <canonical-chip> → 0 = publish the legacy line, 1 = skip
  local target="$1" canon="$2"
  [ -n "$canon" ] || return 2                  # caller bug: never guess the canonical chip
  # No descriptor at all = a pre-#349 image, which by the argument above is a canonical-chip build.
  # Publishing the legacy line is the ONLY way such an image reaches anything.
  [ -n "$target" ] || return 0
  [ "$target" = "$canon" ] && return 0
  return 1
}

# ---- legacy-line mode: operator preflight for the decision above --------------
# "Will this chip's image go on the fleet-wide line?" — answerable before spending a build, a scp and
# a vault read. Also the seam the unit tests drive, which is why it prints the canonical chip it
# resolved rather than only a verdict: a wrong answer from a wrong canonical chip is the failure this
# would otherwise hide.
if [ "$MODE" = "legacy-line" ]; then
  _chip="${2-}"
  _canon="$("$REPO/tools/build_matrix.py" canonical-chip 2>/dev/null || true)"
  [ -n "$_canon" ] || die "legacy-line: could not resolve meta.canonical_chip from tools/build-matrix.toml"
  if legacy_line_wanted "$_chip" "$_canon"; then
    echo "legacy-line: YES — ${_chip:-<no descriptor / pre-#349>} is served by smol/ota/staged (canonical=$_canon)"
    exit 0
  else
    echo "legacy-line: NO — '$_chip' is not the canonical chip ($_canon); smol/ota/staged would be"
    echo "             skipped (#464: it can only cost a legacy C3 crown a ~1 MB self-fetch)."
    exit 1
  fi
fi

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
COMMIT="HEAD"; BIN=""; BUILD_OVERRIDE=""; ALLOW_DIRTY=0
while [ $# -gt 0 ]; do case "$1" in
  --bin) BIN="${2:?}"; shift 2;;
  --build) BUILD_OVERRIDE="${2:?}"; shift 2;;
  --dirty) ALLOW_DIRTY=1; shift;;
  *) COMMIT="$1"; shift;;
esac; done

# ---- identity (matches build.rs deploy contract; archive builds have no .git) -
cd "$REPO"
# #400: refuse BEFORE the broker read and before the build — a stamp this tool cannot honour must
# cost nothing to reject. This applies to the --bin path too: the <commit> argument's ONLY effect is
# provenance, and with a prebuilt image we can verify that claim even less, so `stage <old> --bin
# <current>` is the same mislabelling by a shorter route.
assert_stamp_is_head "$COMMIT"
# The dirty refusal is a claim about BYTES THIS TOOL IS ABOUT TO PRODUCE, so it is conditioned on
# the build path: on --bin we neither build nor stamp, and refusing an operator's own prebuilt image
# over the state of a tree it did not come from would be a gate firing on an innocent state.
# Conditioned HERE rather than called inside the build branch so that BOTH refusals land before the
# broker read and before any credential is sourced — a stamp this tool cannot honour must cost
# nothing to reject, which is the same placement property #314's id42 refusal is tested for.
[ -n "$BIN" ] || assert_stampable_inputs "$ALLOW_DIRTY"
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
  # #400: the dirty refusal already fired up at the identity block (before the broker read).
  echo "building reproducible espnow release @ $HASH (build $BUILD) ..."
  BIN="$SMOL_TMP/smol-${BUILD}.bin"
  # #326: staging IS the release act, so stamp it as one HERE rather than hoping the
  # operator remembered `export SMOL_RELEASE=1`. Before this line the release-vs-dev stamp
  # of a STAGED image depended on the operator's shell: repro_build.sh's comment said "the
  # caller sets SMOL_RELEASE=1" and no caller in the repo ever did — 913/915 shipped
  # release-stamped only because operators exported it by hand.
  #
  # #400 CORRECTION — this comment used to end: "A canary of an uncommitted image still goes
  # through --bin, which skips this build path entirely, so dev images cannot masquerade: this
  # export never touches them." That was FOLKLORE. Nothing routed uncommitted work to --bin, and
  # nothing checked the tree, so `stage` on a dirty tree reached this line and force-stamped a
  # CLEAN RELEASE onto uncommitted bytes — the precise masquerade the sentence denied, in the block
  # whose job is honest release stamping. It is true now because assert_stampable_inputs makes it
  # true: reaching here means either the inputs match HEAD, or --dirty was named and the stamp
  # below is deliberately withheld.
  if [ "$ALLOW_DIRTY" = 1 ] && [ -n "$(stage_input_dirt)" ]; then
    # NO SMOL_RELEASE: build.rs then stamps `vN+dev.<hash>` (#218's honest-identity path), so the
    # image reports on-glass and over DIAG that it is not a reproducible release. The stamp is the
    # only part of this that survives into an incident three weeks from now.
    echo "⚠️  #400: --dirty — building from UNCOMMITTED inputs; stamping DEV (vN+dev.$HASH), not a release."
    echo "    The announced sha256 is reproducible from NO commit; verify_image.sh cannot confirm it."
    repro_build_bin "$CLOCK" "$BIN" "$HASH" "$BUILD" || die "reproducible build failed"
  else
    SMOL_RELEASE=1 repro_build_bin "$CLOCK" "$BIN" "$HASH" "$BUILD" || die "reproducible build failed"
  fi
fi
[ -f "$BIN" ] || die "no image at $BIN"

# ---- metadata + HARD slot-fit gate ------------------------------------------
SIZE="$(stat -c%s "$BIN")"
SHA="$(sha256sum "$BIN" | cut -d' ' -f1)"
[ "$SIZE" -le "$SLOT_MAX" ] || die "image $SIZE B > slot $SLOT_MAX B (0x1F0000) — WILL NOT FIT, aborting"

# ---- #349: read the TARGET DESCRIPTOR out of the image we are about to publish ----------
# The descriptor is a 16-byte record (magic "SMLT" + fields + FNV-1a/32 checksum) that the
# firmware embeds in itself; see rust/clock/src/net/target.rs and docs/protocol.md.
#
# It is EXTRACTED FROM THE BINARY, never recomputed from the build flags. That is the whole
# point: the manifest's target and the image's target are then the same 16 bytes by
# construction, so they cannot disagree. Deriving it a second way here would be exactly WLED's
# bug — their descriptor is instantiated with a literal where the constant belongs, and the two
# silently drifted apart.
#
# A real image contains TWO "SMLT" occurrences and only ONE that checksums (the other is the
# firmware's own scanner constant), so the checksum is what selects the record — matching the
# device-side scanner byte for byte.
read_target_desc() { # <bin> → "<hexdesc> <chipname>" on stdout, non-zero if absent/ambiguous
  python3 - "$1" <<'PY'
import re, sys
# #413: esp32c5 (id 4) WAS MISSING, and the consequence was not a crash. `CHIPS.get(4)` returned
# None, this script exited 1, the caller's `if _desc_out=$(read_target_desc ...)` took the
# pre-#349 branch, and a C5 image with a PERFECTLY VALID descriptor was reported as
# "no target descriptor in this image" and staged LEGACY-ONLY — invisible to boards routing on
# smol/ota/staged/<chip>. Fail-closed against publishing a wrong name, but MISATTRIBUTED: it sends
# whoever debugs it into the firmware's descriptor emission when the bug is this dict.
#
# ⚠️ THIS IS THE FOURTH COPY OF THE CHIP ROSTER in the tree (tools/build-matrix.toml,
# rust/clock/src/budget.rs, rust/clock/src/net/target.rs, here) and it was the one nothing
# compared. `net/target.rs` is AUTHORITATIVE because the id is the #349 WIRE FORMAT, and
# `build_matrix.py check` now asserts this map against it in BOTH directions — so this list going
# short again is a gate failure rather than a mystery at staging time.
CHIPS = {1: "esp32c3", 2: "esp32c6", 3: "esp32s3", 4: "esp32c5"}
data = open(sys.argv[1], "rb").read()
def fnv(b):
    h = 0x811c9dc5
    for x in b:
        h ^= x
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h
found = []
for m in re.finditer(re.escape(b"SMLT"), data):
    rec = data[m.start():m.start() + 16]
    if len(rec) == 16 and int.from_bytes(rec[12:16], "little") == fnv(rec[:12]):
        found.append(rec)
uniq = {r.hex() for r in found}
if not uniq:
    sys.stderr.write("no valid #349 target descriptor in the image\n"); sys.exit(1)
if len(uniq) > 1:
    sys.stderr.write("MULTIPLE conflicting target descriptors in the image: %s\n" % sorted(uniq)); sys.exit(1)
rec = found[0]
chip = CHIPS.get(rec[5])
if chip is None:
    sys.stderr.write("image declares unknown chip id %d\n" % rec[5]); sys.exit(1)
print("%s %s" % (rec.hex(), chip))
PY
}
if _desc_out="$(read_target_desc "$BIN")"; then
  TARGET_HEX="${_desc_out%% *}"; TARGET_CHIP="${_desc_out##* }"
  echo "target  ${TARGET_CHIP}  desc ${TARGET_HEX}"
else
  # Pre-#349 image (or --bin of an old artifact): stage LEGACY-ONLY. Refusing here would block
  # staging a rollback build, and the device-side descriptor check already fails such an image
  # closed if it ever reaches a board that expects one.
  TARGET_HEX=""; TARGET_CHIP=""
  echo "WARNING: #349 — no target descriptor in this image; staging the LEGACY line only." >&2
  echo "         Boards route by chip on smol/ota/staged/<chip>; this build will only be seen" >&2
  echo "         on the fleet-wide smol/ota/staged topic." >&2
fi

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
bw get notes "$SMOL_OTA_SIGNING_KEY_ITEM" > "$_keyf" 2>/dev/null \
  || { shred -u "$_msgf" "$_keyf" 2>/dev/null; die "bw: couldn't read signing key '$SMOL_OTA_SIGNING_KEY_ITEM' — locked vault and 'Not found.' (wrong server/missing item) are DIFFERENT failures; check \`bw status\` + \`bw config server\` (2026-08-26: a default-cloud login wore this message for an hour)"; }
# #349: the key is now used for up to TWO signatures (legacy M and OTA2 M), so it stays in
# /dev/shm across both and is shredded once, immediately after. The trap above still covers an
# interrupt in that (slightly longer, still sub-second) window.
sign_msg() { # <message> → 128-hex ed25519 signature on stdout; dies on any failure
  local _m="$1" _s
  printf '%s' "$_m" > "$_msgf"   # printf, NOT echo: M is exact wire bytes, no trailing newline
  _s="$(openssl pkeyutl -sign -rawin -inkey "$_keyf" -in "$_msgf" | xxd -p -c 64)"
  case "$_s" in *[!0-9a-f]*|"") die "ed25519 signing failed (empty/non-hex sig — openssl >=3.0 + valid key?)";; esac
  [ "${#_s}" -eq 128 ] || die "ed25519 sig wrong length ${#_s} (want 128 hex)"
  printf '%s' "$_s"
}
SIG="$(sign_msg "${BUILD}|${SIZE}|${SHA}")"
# #349 OTA2 M puts the target INSIDE the signed bytes — an unauthenticated target field could be
# stripped or rewritten by anyone with broker write access, and a suitability check on
# unauthenticated data is theatre.
SIG2=""
[ -n "$TARGET_HEX" ] && SIG2="$(sign_msg "${BUILD}|${SIZE}|${SHA}|${TARGET_HEX}")"
shred -u "$_msgf" "$_keyf" 2>/dev/null

# 6-field SIGNED announce (was 4-field unsigned): url stays LAST (may contain no '|').
LINE="OTA|${BUILD}|${SIZE}|${SHA}|${SIG}|${URL}"

# ---- publish: DUAL-STAGE (#349) ---------------------------------------------
# The legacy fleet-wide line is published UNCHANGED, and it is what makes this a safe
# migration rather than a flag day:
#
#   * Firmware older than #349 only knows `smol/ota/staged` and only parses `OTA|` — it keeps
#     working, untouched. (It cannot parse `OTA2|` at all: its `strip_prefix("OTA|")` fails on
#     "OTA2|", so it ignores the new line cleanly rather than mis-slicing it.)
#   * Firmware with #349 subscribes BOTH topics and prefers whichever build is newer, so it
#     picks up the per-chip line automatically.
#
# Retiring `smol/ota/staged` is therefore the ONE step that needs a ROLLED fleet — not a merged
# main. Do not remove it here until every board reports a build carrying the OTA2 parser.
#
# ⚠️ #464: "UNCHANGED" above was chip-BLIND, and that is the defect. The paragraph's reasoning is
# entirely about pre-#349 *firmware*, all of which is on the canonical chip — so the line is now
# published only for images a legacy board could actually install. See `legacy_line_wanted`.
CANON_CHIP="$("$REPO/tools/build_matrix.py" canonical-chip 2>/dev/null || true)"
[ -n "$CANON_CHIP" ] || die "#464: could not resolve meta.canonical_chip from tools/build-matrix.toml"
if legacy_line_wanted "$TARGET_CHIP" "$CANON_CHIP"; then
  pub_retained "smol/ota/staged" "$LINE"
  echo "staged  smol/ota/staged  <-  build $BUILD ($HASH) ${SIZE}B sha ${SHA:0:12}… sig ${SIG:0:12}… @ $URL"
else
  # SKIPPED, and said so — a silent omission here would look exactly like a broker failure, which is
  # the failure mode the install path's 2026-07-28 note calls the worst this tool can have.
  echo "skipped smol/ota/staged  —  #464: this is an ${TARGET_CHIP} image and the fleet-wide line"
  echo "        only serves pre-#349 firmware, all of which is ${CANON_CHIP}. Publishing it here"
  echo "        would make every legacy ${CANON_CHIP} crown self-fetch ~${SIZE}B and refuse it at"
  echo "        the finalize descriptor read (observed 2026-08-26, id50). ${TARGET_CHIP} boards are"
  echo "        armed by smol/ota/staged/${TARGET_CHIP} below."
  echo "        The retained fleet-wide line keeps its previous (${CANON_CHIP}) value — that is"
  echo "        #472's cosmetic 'latest' wart on non-${CANON_CHIP} boards, tracked separately."
fi
if [ -n "$TARGET_HEX" ]; then
  LINE2="OTA2|${BUILD}|${SIZE}|${SHA}|${TARGET_HEX}|${SIG2}|${URL}"
  pub_retained "smol/ota/staged/${TARGET_CHIP}" "$LINE2"
  echo "staged  smol/ota/staged/${TARGET_CHIP}  <-  OTA2 build $BUILD target ${TARGET_HEX}"
  echo "done. ${TARGET_CHIP} boards see build $BUILD on their per-chip topic; every board still"
  echo "      sees it fleet-wide. Boards on other silicon will not be armed by the per-chip line."
else
  echo "done. Every board's native HA Update entity now shows build $BUILD as available."
fi
echo "      Install per-node from HA (the Update entity's Install button) or: ota_publish.sh install <id>"
