#!/usr/bin/env bash
# ha_deploy.sh — keep this repo's HA packages and the live Home Assistant in sync, both ways.
#
# WHY THIS EXISTS. The HA half of smol lives in `ha/packages/*.yaml`, but HA runs from
# `/homeassistant/packages/` on the HA VM. For a long time the two drifted in BOTH directions:
# on 2026-07-27 the live `smol_mesh.yaml` was 17.5 KB AHEAD of the repo (id5 mirrors, AP-channel
# mirrors, NTP freshness, coexist health — edited live, never committed) and two whole packages
# (`smol_notify.yaml`, `smol_telemetry.yaml`) existed only on the VM. Nobody did anything wrong;
# there was simply no tool, so `cat file | ssh tee` was done by hand and only in one direction.
# This script makes the round trip cheap, so drift gets caught in seconds instead of months.
#
#   ./tools/ha_deploy.sh status        # what differs, repo vs live (READ-ONLY; the default)
#   ./tools/ha_deploy.sh diff [file]   # full unified diff for one package (or all)
#   ./tools/ha_deploy.sh pull          # live -> repo, so out-of-band edits become commits
#   ./tools/ha_deploy.sh push          # repo -> live, validated, with rollback
#   ./tools/ha_deploy.sh push --dry-run
#
# PUSH IS RECOVERABLE — and be precise about how much each layer actually buys:
#   1. BACKUPS (the real safety net): every file it overwrites is copied to
#      `<name>.bak-deploy-<stamp>` on the VM first, so any deploy can be undone by hand.
#   2. LOCAL YAML VALIDATION: a file that is not valid YAML is refused before anything is sent.
#   3. `check_config` runs BEFORE any reload, and a rejection restores the whole batch and exits
#      non-zero. BUT measure what that catches: HA's check_config validates the CORE config load,
#      it does NOT deep-validate package schemas. Probed on this instance 2026-07-27 — it happily
#      accepted `automation: "not a list"` and an `input_text` with `max: 999999`. So treat it as
#      a coarse net that catches load-breaking mistakes, NOT as proof your package is correct;
#      the backups, not check_config, are what makes a bad push undoable.
#   4. only then does it reload — `reload_all` when helpers/mqtt/template entities changed
#      (`automation.reload` does NOT register new ones — HA gotcha), else `automation.reload`.
# Unchanged files are skipped entirely, so a re-run is a no-op and cheap to do often.
#
# AUTH resolves in the order the ha skill documents: $HA_TOKEN, then ~/.cache/ha-token-tmp,
# then `bw get password ha-llat`. Never printed. SSH is the HAOS add-on (no scp subsystem —
# hence the `cat | tee` pattern rather than rsync).
set -euo pipefail

HA_SSH="${HA_SSH:-jp@10.0.6.108}"
HA_URL="${HA_URL:-https://ha.jphe.in}"
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOCAL_DIR="$REPO_DIR/ha/packages"
REMOTE_DIR="/homeassistant/packages"
SSH_OPTS=(-o ConnectTimeout=10 -o BatchMode=yes)

die() { echo "FATAL — $*" >&2; exit 1; }
note() { echo "  $*"; }

ha_token() {
  if [ -n "${HA_TOKEN:-}" ]; then printf '%s' "$HA_TOKEN"; return; fi
  if [ -s "$HOME/.cache/ha-token-tmp" ]; then tr -d '[:space:]' < "$HOME/.cache/ha-token-tmp"; return; fi
  # `bw` may trip the vault-gate hook; export BW_SESSION beforehand for unattended runs.
  bw get password ha-llat 2>/dev/null | tr -d '[:space:]' || true
}

ha_api() { # $1 method, $2 path, $3 body(optional) -> prints body, returns curl's status
  local tok; tok="$(ha_token)"
  [ -n "$tok" ] || die "no HA token (set HA_TOKEN, ~/.cache/ha-token-tmp, or bw item ha-llat)"
  curl -sS -X "$1" -H "Authorization: Bearer $tok" -H "Content-Type: application/json" \
       ${3:+-d "$3"} "$HA_URL$2"
}

packages() { # repo package basenames, sorted
  find "$LOCAL_DIR" -maxdepth 1 -name '*.yaml' -printf '%f\n' | sort
}

fetch_remote() { # $1 basename -> stdout (empty if the file does not exist there)
  ssh "${SSH_OPTS[@]}" "$HA_SSH" "cat $REMOTE_DIR/$1 2>/dev/null" || true
}

# --- status ------------------------------------------------------------------------------
cmd_status() {
  local drift=0 tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  echo "repo: $LOCAL_DIR"
  echo "live: $HA_SSH:$REMOTE_DIR"
  echo
  for f in $(packages); do
    fetch_remote "$f" > "$tmp/$f"
    if [ ! -s "$tmp/$f" ]; then
      printf '  %-24s MISSING on live (push would create it)\n' "$f"; drift=1
    elif cmp -s "$LOCAL_DIR/$f" "$tmp/$f"; then
      printf '  %-24s in sync\n' "$f"
    else
      local a b; a=$(diff "$LOCAL_DIR/$f" "$tmp/$f" | grep -c '^<' || true)
      b=$(diff "$LOCAL_DIR/$f" "$tmp/$f" | grep -c '^>' || true)
      printf '  %-24s DIFFERS  (repo-only lines: %s, live-only lines: %s)\n' "$f" "$a" "$b"; drift=1
    fi
  done
  # A package that exists ONLY on the VM is the drift that bit us before — surface it loudly.
  local extra
  extra=$(ssh "${SSH_OPTS[@]}" "$HA_SSH" "ls $REMOTE_DIR/smol*.yaml 2>/dev/null" | xargs -r -n1 basename \
          | grep -vxF "$(packages)" || true)
  if [ -n "$extra" ]; then
    echo; echo "  LIVE-ONLY packages (untracked — run 'pull' to bring them into git):"
    echo "$extra" | sed 's/^/    /'; drift=1
  fi
  echo
  [ "$drift" -eq 0 ] && echo "everything in sync." || echo "drift found (see above)."
  return 0
}

# --- diff --------------------------------------------------------------------------------
cmd_diff() {
  local only="${1:-}" tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  for f in $(packages); do
    [ -n "$only" ] && [ "$f" != "$only" ] && continue
    fetch_remote "$f" > "$tmp/$f"
    cmp -s "$LOCAL_DIR/$f" "$tmp/$f" && continue
    echo "=== $f  (< repo | > live)"
    diff -u "$LOCAL_DIR/$f" "$tmp/$f" || true
  done
}

# --- pull (live -> repo) -------------------------------------------------------------------
cmd_pull() {
  local changed=0 tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  # Everything smol* on the VM, including packages the repo does not know about yet.
  local remote_list
  remote_list=$(ssh "${SSH_OPTS[@]}" "$HA_SSH" "ls $REMOTE_DIR/smol*.yaml 2>/dev/null" | xargs -r -n1 basename)
  [ -n "$remote_list" ] || die "no smol packages found on $HA_SSH:$REMOTE_DIR"
  for f in $remote_list; do
    fetch_remote "$f" > "$tmp/$f"
    [ -s "$tmp/$f" ] || { note "$f: empty on live, skipped"; continue; }
    python3 -c "import yaml,sys; yaml.safe_load(open(sys.argv[1]))" "$tmp/$f" \
      || die "$f from live is not valid YAML — refusing to write it into the repo"
    if [ -f "$LOCAL_DIR/$f" ] && cmp -s "$LOCAL_DIR/$f" "$tmp/$f"; then
      note "$f: in sync"
    else
      cp "$tmp/$f" "$LOCAL_DIR/$f"; note "$f: PULLED into the repo"; changed=1
    fi
  done
  echo
  [ "$changed" -eq 1 ] && echo "repo updated — review with 'git diff' and COMMIT, or the drift returns." \
                       || echo "repo already matches live."
}

# --- push (repo -> live) -------------------------------------------------------------------
cmd_push() {
  local dry=0; [ "${1:-}" = "--dry-run" ] && dry=1
  local stamp; stamp="$(date +%Y%m%d-%H%M%S)"
  local tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  local -a changed=() ; local needs_full=0

  for f in $(packages); do
    python3 -c "import yaml,sys; yaml.safe_load(open(sys.argv[1]))" "$LOCAL_DIR/$f" \
      || die "$f is not valid YAML locally — fix it before pushing"
    fetch_remote "$f" > "$tmp/$f"
    if cmp -s "$LOCAL_DIR/$f" "$tmp/$f"; then note "$f: in sync, skipping"; continue; fi
    changed+=("$f")
    # Adding/removing helpers or mqtt/template entities needs reload_all; automations alone do not.
    # NB: the diff goes to a FILE before grepping, deliberately. `diff | grep -q` looks obvious but
    # is wrong under `set -o pipefail`: diff exits 1 for "files differ", so the pipeline reports
    # failure even when grep matched, and the `&& needs_full=1` never fires. That bug shipped for
    # exactly one test run here — it picked automation.reload for a new input_text, which is the
    # silent failure where a new helper never registers and nobody can see why.
    diff "$LOCAL_DIR/$f" "$tmp/$f" > "$tmp/$f.diff" || true
    # Which reload? Default to the SAFE one and narrow only when it is provably enough.
    # The first version matched `input_[a-z]+:` — i.e. a DOMAIN being added — and so missed the
    # far commoner case: a new helper added under a domain that already existed. Adding
    # `smol_8_bard_font:` under an existing `input_select:` picked automation.reload, which does
    # not register new entities, and the control silently did not appear (caught by JP asking
    # where it was). So: reload_all unless EVERY changed line is inside the automation block.
    if grep -qE '^[<>]' "$tmp/$f.diff" \
       && ! grep -qE '^[<>].*(input_[a-z]+:|unique_id:|state_topic:|mqtt:|template:|^[<>]  [a-z0-9_]+:)' "$tmp/$f.diff" \
       && grep -qE '^[<>].*(alias:|trigger:|action:|service:|condition:|mode: (single|parallel|queued|restart))' "$tmp/$f.diff"; then
      : # automations only — automation.reload is sufficient
    else
      needs_full=1
    fi
  done

  [ ${#changed[@]} -eq 0 ] && { echo "nothing to push — live already matches the repo."; return 0; }
  echo "would push: ${changed[*]}"
  [ "$dry" -eq 1 ] && { echo "(dry run — nothing sent)"; return 0; }

  # 1. back up every file we are about to overwrite, so step 3 can undo the whole batch.
  for f in "${changed[@]}"; do
    ssh "${SSH_OPTS[@]}" "$HA_SSH" \
      "[ -f $REMOTE_DIR/$f ] && sudo cp $REMOTE_DIR/$f $REMOTE_DIR/$f.bak-deploy-$stamp || true"
  done
  # 2. copy (HAOS ssh has no scp subsystem — tee is the documented pattern).
  for f in "${changed[@]}"; do
    < "$LOCAL_DIR/$f" ssh "${SSH_OPTS[@]}" "$HA_SSH" "sudo tee $REMOTE_DIR/$f > /dev/null" \
      || die "copy of $f failed"
    note "$f: copied"
  done
  # 3. HA validates BEFORE any reload; roll the whole batch back if it complains.
  local check; check="$(ha_api POST /api/config/core/check_config || true)"
  if ! grep -q '"result": *"valid"' <<<"$check"; then
    echo "check_config REJECTED the new config — rolling back:" >&2
    echo "$check" | head -c 500 >&2; echo >&2
    for f in "${changed[@]}"; do
      ssh "${SSH_OPTS[@]}" "$HA_SSH" \
        "[ -f $REMOTE_DIR/$f.bak-deploy-$stamp ] && sudo mv $REMOTE_DIR/$f.bak-deploy-$stamp $REMOTE_DIR/$f || sudo rm -f $REMOTE_DIR/$f"
      note "$f: restored"
    done
    die "rolled back; live is unchanged"
  fi
  note "check_config: valid"
  # 4. minimal reload that actually registers what changed.
  if [ "$needs_full" -eq 1 ]; then
    ha_api POST /api/services/homeassistant/reload_all '{}' >/dev/null
    note "reload_all (helpers/mqtt/template entities changed)"
  else
    ha_api POST /api/services/automation/reload '{}' >/dev/null
    note "automation.reload (automations only)"
  fi
  echo
  echo "pushed ${#changed[@]} package(s). Backups on the VM: *.bak-deploy-$stamp"
}

case "${1:-status}" in
  status) cmd_status ;;
  diff)   cmd_diff "${2:-}" ;;
  pull)   cmd_pull ;;
  push)   cmd_push "${2:-}" ;;
  -h|--help|help) sed -n '2,30p' "${BASH_SOURCE[0]}" ;;
  *) die "unknown mode '${1}' (status | diff | pull | push)" ;;
esac
