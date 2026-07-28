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
#   ./tools/ha_deploy.sh dash          # dashboard-only drift check (READ-ONLY; exits 1 on drift)
#   ./tools/ha_deploy.sh pull          # live -> repo, so out-of-band edits become commits
#   ./tools/ha_deploy.sh push [file...] # repo -> live from HEAD, validated, with rollback
#   ./tools/ha_deploy.sh push --all      # every differing package (required if >1)
#   ./tools/ha_deploy.sh push --from-worktree   # push uncommitted state on purpose
#   ./tools/ha_deploy.sh push --dry-run
#
# push SENDS HEAD, NOT YOUR WORKING TREE (#318), and refuses an unscoped multi-file batch —
# see the push guard below for the incident that produced both rules. Exit 4 = refused,
# 2 = could not check.
#
# `status` and `diff` now cover the Lovelace DASHBOARD as well as the packages (#305) — see
# cmd_dash below for why that gap mattered. Only `dash` propagates the drift exit code, so a
# hook can gate on it; `status` stays exit-0 as a human report, as it always has.
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
  # status compares the WORKTREE against live; `push` sends HEAD. When those differ, say so —
  # otherwise "DIFFERS" here reads as "push will fix it" when push would send something else.
  local pdirty; pdirty="$(git -C "$REPO_DIR" status --porcelain -- ha/packages 2>/dev/null | wc -l)"
  if [ "${pdirty:-0}" -gt 0 ]; then
    echo "  NOTE: $pdirty package file(s) modified but uncommitted. The comparison above is against"
    echo "        your WORKING TREE; \`push\` sends HEAD. Commit, or push --from-worktree."
  fi
  echo
  local drc=0; cmd_dash || drc=$?
  # 1 (unreproducible) and 3 (dead rows) are both real drift; 2 means we could not tell, which
  # must not be reported as "in sync" either.
  { [ "$drc" -eq 1 ] || [ "$drc" -eq 3 ]; } && drift=1
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
  # The dashboard has no textual diff — a Lovelace view is JSON in HA's .storage, and the repo
  # side is a scaffold plus a generator. Card-level drift is the only meaningful comparison.
  [ -n "$only" ] && return 0
  echo "=== dashboard  (card-level; the view has no file to diff)"
  cmd_dash || true
}

# --- dashboard (the Lovelace VIEW, not a package) -------------------------------------------
# Everything above this line syncs `ha/packages/*.yaml`. The Control Room VIEW is not a file on
# the VM at all — it lives in HA's `.storage` and is written over the WebSocket API, so it was
# covered by NOTHING here. That is not a footnote: it is why ten hand-made cards drifted out of
# `smol-control-scaffold.yaml` unnoticed for months and were then deleted by a generator
# rebuilding from the stale scaffold (2026-07-27). Drift you cannot see is drift you keep.
#
# We shell out to the generator's own `--check` instead of reimplementing the comparison. The
# question "is this card one of ours?" is answered by `_ident()`, whose rules are subtle enough
# to have been wrong three times (span re-keying, prefix collisions, the `?v=` cache-buster). A
# second copy here would drift from that one the first time either is edited, and then this
# tool would report "in sync" about a dashboard that is not.
DASH_GEN="$REPO_DIR/ha/dashboard/build_control_room.py"

cmd_dash() { # read-only; prints the generator's report, returns 0 in sync / 1 drift / 2 error
  echo "dashboard (Lovelace view, via build_control_room.py --check):"
  [ -f "$DASH_GEN" ] || { note "generator not found at $DASH_GEN — skipped"; return 0; }
  command -v python3 >/dev/null || { note "python3 not available — skipped"; return 0; }
  local tok; tok="$(ha_token)"
  [ -n "$tok" ] || { note "no HA token — skipped"; return 0; }
  local out rc=0; out="$(mktemp)"
  # Output goes to a file rather than through a pipe: `cmd | sed` under `set -o pipefail`
  # makes the generator's exit code fiddly to recover (PIPESTATUS games), and this check is
  # worthless if its exit code is even slightly untrustworthy.
  HA_TOKEN="$tok" python3 "$DASH_GEN" --check > "$out" 2>&1 || rc=$?
  sed 's/^/  /' "$out"; rm -f "$out"
  # Distinguish the failures. Collapsing them would undo the point of having separate codes —
  # and mislabelling a real finding as "couldn't check" is the worse direction of the two.
  case "$rc" in
    0) ;;
    1) echo "  → LIVE-ONLY cards: the repo cannot reproduce the live dashboard. Back-port them"
       echo "    into ha/dashboard/smol-control-scaffold.yaml (see #305 for how)." ;;
    3) echo "  → DEAD ROWS: the dashboard is reproducible but wired to entities HA does not have."
       echo "    Repoint or gate them — the card renders 'Entity not found' as it stands." ;;
    *) echo "  → check could not run (exit $rc); the dashboard is UNVERIFIED, not proven clean." ;;
  esac
  return "$rc"
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

# --- push guard (#318) ----------------------------------------------------------------------
# `push` had NO SCOPE ARGUMENT. `diff` takes a file; `push` did not — it iterated every package
# and pushed each one that differed. So "push my package" was inexpressible, and on 2026-07-28
# one operator's routine push silently carried another author's committed id7/id9 retire (1439
# lines, 172 entities) in the same batch. The lead had reserved that push for its author; the
# tool routed around the reservation without anyone deciding to. That missing argument is the
# root cause, so it is fixed here rather than papered over with a warning.
#
# Milder than the dashboard generator's guard, deliberately. `push` converges toward COMMITTED
# state, so whatever it carries has at least been reviewed by someone; the generator reads the
# working tree and can publish work nobody finished. Same vocabulary, different strictness:
#   exit 4 = REFUSED   · exit 2 = could not check   · 0 = pushed / nothing to do
#
#   push                      every differing package — REFUSES if that is more than one
#   push smol_mesh.yaml …     scope it; this is the way to push your own work
#   push --all                "yes, the whole batch, I have read the list"
#   push --from-worktree      deploy uncommitted state on purpose (named first)
#   push --dry-run            as before; combines with the above
PUSH_SCRIPT_REL="tools/ha_deploy.sh"

git_or_die() { # $@ git args -> stdout; sets GIT_ERR and returns 1 on failure
  GIT_ERR=""
  local out; out="$(git -C "$REPO_DIR" "$@" 2>&1)" || { GIT_ERR="$out"; return 1; }
  printf '%s' "$out"
}

push_dirty() { # -> newline list of dirty repo-relative paths among the packages + this script
  git_or_die status --porcelain -- "ha/packages" "$PUSH_SCRIPT_REL" \
    | sed -n 's/^.\{3\}//p'
}

push_provenance() { # $1 basename -> "abc1234 3 hours ago · subject"
  git_or_die log -1 --format='%h %ar · %s' -- "ha/packages/$1" || echo "(no commit found)"
}

# --- push (repo -> live) -------------------------------------------------------------------
cmd_push() {
  local dry=0 from_worktree=0 all=0; local -a scope=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --dry-run) dry=1 ;;
      --from-worktree) from_worktree=1 ;;
      --all) all=1 ;;
      -*) die "unknown push flag '$1' (--dry-run | --from-worktree | --all)" ;;
      "") ;;
      *) packages | grep -qxF "$1" || die "no such package '$1' (have: $(packages | tr '\n' ' '))"
         scope+=("$1") ;;
    esac; shift
  done
  local stamp; stamp="$(date +%Y%m%d-%H%M%S)"
  local tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  local -a changed=() ; local needs_full=0

  # ---- GUARD, before the network and long before any mutation ----------------------------
  local dirty; if ! dirty="$(push_dirty)"; then
    echo "!! cannot determine whether the working tree is clean — $GIT_ERR" >&2
    echo "   Refusing to push: unverified is not the same as clean." >&2
    return 2
  fi
  if [ -n "$dirty" ]; then
    echo "  ── PUSH GUARD ───────────────────────────────────────────────"
    echo "  Modified and NOT committed:"; echo "$dirty" | sed 's/^/    /'
    if grep -qxF "$PUSH_SCRIPT_REL" <<<"$dirty" && [ "$from_worktree" -eq 0 ]; then
      # The strict case, exactly as in the generator: package CONTENT can be read from HEAD,
      # but the deploy LOGIC running is whatever is on disk. A modified deploy script means the
      # thing executing is not the thing reviewed, and no amount of reading HEAD fixes that.
      echo "  REFUSED · $PUSH_SCRIPT_REL is modified, so the deploy logic about to run is not"
      echo "  the reviewed logic. Commit it, or re-run with --from-worktree." >&2
      return 4
    fi
    [ "$from_worktree" -eq 0 ] \
      && echo "  Packages are read from HEAD, so the above is NOT pushed (--from-worktree to)." \
      || echo "  --from-worktree: pushing the above ON PURPOSE."
  fi

  # ---- materialise the push SOURCE: HEAD by default, worktree only on request ------------
  local SRC="$tmp/src"; mkdir -p "$SRC"
  local -a candidates=(); [ ${#scope[@]} -gt 0 ] && candidates=("${scope[@]}") || readarray -t candidates < <(packages)
  for f in "${candidates[@]}"; do
    if [ "$from_worktree" -eq 1 ]; then cp "$LOCAL_DIR/$f" "$SRC/$f"; continue; fi
    if ! git_or_die show "HEAD:ha/packages/$f" > "$SRC/$f"; then
      note "$f: not in HEAD (new/uncommitted) — skipped; --from-worktree to push it anyway"
      rm -f "$SRC/$f"
    fi
  done

  for f in "${candidates[@]}"; do
    [ -f "$SRC/$f" ] || continue
    python3 -c "import yaml,sys; yaml.safe_load(open(sys.argv[1]))" "$SRC/$f" \
      || die "$f is not valid YAML locally — fix it before pushing"
    fetch_remote "$f" > "$tmp/$f"
    if cmp -s "$SRC/$f" "$tmp/$f"; then note "$f: in sync, skipping"; continue; fi
    changed+=("$f")
    # Adding/removing helpers or mqtt/template entities needs reload_all; automations alone do not.
    # NB: the diff goes to a FILE before grepping, deliberately. `diff | grep -q` looks obvious but
    # is wrong under `set -o pipefail`: diff exits 1 for "files differ", so the pipeline reports
    # failure even when grep matched, and the `&& needs_full=1` never fires. That bug shipped for
    # exactly one test run here — it picked automation.reload for a new input_text, which is the
    # silent failure where a new helper never registers and nobody can see why.
    diff "$SRC/$f" "$tmp/$f" > "$tmp/$f.diff" || true
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

  # ---- NAME THE BATCH, then require intent for a multi-file one --------------------------
  # This is the part that was missing when one operator's push carried another author's retire:
  # nothing ever said what else was in the envelope. Provenance is the last commit per file —
  # every agent here commits as the same git user, so authorship cannot identify anyone; the
  # commit SUBJECT can, because you recognise your own work. Say what is knowable, not what
  # would merely look authoritative.
  echo
  echo "  would push ${#changed[@]} package(s):"
  for f in "${changed[@]}"; do printf '    %-22s %s\n' "$f" "$(push_provenance "$f")"; done
  if [ ${#scope[@]} -eq 0 ] && [ ${#changed[@]} -gt 1 ] && [ "$all" -eq 0 ]; then
    echo
    echo "  REFUSED · an unscoped push would send all ${#changed[@]} of the above in one batch," >&2
    echo "  and at least one of them is probably not yours. Name what you mean:" >&2
    echo "      ./tools/ha_deploy.sh push ${changed[0]}" >&2
    echo "  or say you have read the list and want all of it:" >&2
    echo "      ./tools/ha_deploy.sh push --all" >&2
    return 4
  fi
  [ "$dry" -eq 1 ] && { echo "  (dry run — nothing sent)"; return 0; }

  # 1. back up every file we are about to overwrite, so step 3 can undo the whole batch.
  for f in "${changed[@]}"; do
    ssh "${SSH_OPTS[@]}" "$HA_SSH" \
      "[ -f $REMOTE_DIR/$f ] && sudo cp $REMOTE_DIR/$f $REMOTE_DIR/$f.bak-deploy-$stamp || true"
  done
  # 2. copy (HAOS ssh has no scp subsystem — tee is the documented pattern).
  for f in "${changed[@]}"; do
    < "$SRC/$f" ssh "${SSH_OPTS[@]}" "$HA_SSH" "sudo tee $REMOTE_DIR/$f > /dev/null" \
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
  dash)   cmd_dash ;;   # exit code propagates: 0 in sync · 1 live-only drift · 2 could not check
  pull)   cmd_pull ;;
  push)   shift; cmd_push "$@" ;;
  -h|--help|help) sed -n '2,30p' "${BASH_SOURCE[0]}" ;;
  *) die "unknown mode '${1}' (status | diff | dash | pull | push)" ;;
esac
