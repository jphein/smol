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

# --- live-baseline attribution (morpheus-yaml's `live_is_ahead`, taken verbatim from b002ab4) --
# Kept as THEIR code rather than reimplemented: one maintained version beats two that agree today.
# It fixes three things my first attempt got wrong, and the first one produced a false alarm I
# reported as actionable — I told the team a live file had been hand-edited on the VM when it was
# byte-identical to commit 3565997, which my branch simply did not contain.
#   1. `--all` in the history walk. "Live matches no commit I CAN SEE" and "live matches no commit
#      that EXISTS" are different facts, and the first is usually just being behind your own repo.
#      Conflating them sends someone hunting a person who did nothing wrong.
#   2. BEHIND vs UNKNOWN. Once the baseline is found, ask whether HEAD contains it; if not, live is
#      NEWER than you and pushing REVERTS deployed work. Invisible in a batch listing, because
#      every line looks normal. Its own flag (--allow-revert), because "ship it forward" is not
#      consent to undo — the same reasoning that made refusal exit 4 rather than 1.
#   3. Trailing-whitespace tolerance as a SECOND pass, exact match first. Live's bard was once one
#      final newline short of its commit, and an exact-only test cried "hand-edited" over a byte
#      no YAML parser can see. A guard that raises false alarms gets switched off.
LIVE_BASELINE_DEPTH=60

_norm_hash() { # $1 file -> blob hash with trailing newlines normalised to exactly one
  local t; t="$(mktemp)"; printf '%s\n' "$(cat "$1")" > "$t"
  git -C "$REPO_DIR" hash-object "$t"; rm -f "$t"
}

live_is_ahead() { # $1 basename, $2 path-to-live-copy
  local f="$1" live="$2" livehash livenorm sha blob base='' ws='' bt
  [ -s "$live" ] || return 0                       # not on live yet: nothing to overwrite
  livehash="$(git -C "$REPO_DIR" hash-object "$live" 2>/dev/null)" || return 0
  local -a hist=()
  readarray -t hist < <(git -C "$REPO_DIR" log --all -n "$LIVE_BASELINE_DEPTH" --format='%H' \
                          -- "ha/packages/$f" 2>/dev/null || true)
  for sha in "${hist[@]}"; do
    [ -n "$sha" ] || continue
    blob="$(git -C "$REPO_DIR" rev-parse "$sha:ha/packages/$f" 2>/dev/null || true)"
    [ "$blob" = "$livehash" ] && { base="$sha"; break; }
  done
  # Trailing-whitespace-tolerant second pass. The very first live run reported "hand-edited on the
  # VM" when the whole difference from the deploying commit was a missing final newline — a false
  # alarm over a byte no YAML parser can see, and a guard that cries wolf gets switched off.
  if [ -z "$base" ]; then
    livenorm="$(_norm_hash "$live")"
    for sha in "${hist[@]}"; do
      [ -n "$sha" ] || continue
      bt="$(mktemp)"
      if git -C "$REPO_DIR" cat-file blob "$sha:ha/packages/$f" > "$bt" 2>/dev/null \
         && [ "$(_norm_hash "$bt")" = "$livenorm" ]; then
        base="$sha"; ws=' (differs only in trailing whitespace)'; rm -f "$bt"; break
      fi
      rm -f "$bt"
    done
  fi
  [ -n "$base" ] || { echo 'UNKNOWN'; return 0; }
  git -C "$REPO_DIR" merge-base --is-ancestor "$base" HEAD 2>/dev/null && return 0
  echo "BEHIND $(git -C "$REPO_DIR" log -1 --format='%h %s' "$base")$ws"
}

reload_signature() { # $1 yaml file -> sorted set of REGISTERABLE entity keys; non-zero if unparseable
  # What `automation.reload` cannot create: helpers (`input_*` children) and anything carrying a
  # `unique_id` (mqtt/template entities). Comparing this set between the outgoing file and the
  # live one answers the only question that matters — "does this change ADD or REMOVE an entity?"
  # — structurally, from the parsed documents, rather than by pattern-matching a diff.
  python3 - "$1" <<'PY'
import sys, yaml
def walk(o, out):
    if isinstance(o, dict):
        u = o.get("unique_id")
        if isinstance(u, str): out.add("uid:" + u)
        for v in o.values(): walk(v, out)
    elif isinstance(o, list):
        for v in o: walk(v, out)
d = yaml.safe_load(open(sys.argv[1])) or {}
out = set()
if isinstance(d, dict):
    for dom, body in d.items():
        if isinstance(dom, str) and dom.startswith("input_") and isinstance(body, dict):
            out |= {f"{dom}.{k}" for k in body}
walk(d, out)
print("\n".join(sorted(out)))
PY
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
PUSH_UNATTRIBUTABLE=0   # set by push_unshipped when live matches no committed blob

GIT_ERR=""   # global so `set -u` can never trip on it — see the subshell trap below

git_or_die() { # $@ git args -> stdout; returns 1 on failure
  # NB: a caller that runs this inside $( ) gets a SUBSHELL, so anything this assigns is lost to
  # the parent. That bit exactly once, in the worst place: the "cannot determine cleanliness"
  # branch referenced $GIT_ERR set inside such a subshell, so under `set -u` the guard died with
  # exit 1 — the code that means "your push was refused for a real reason" — instead of the 2 it
  # promises for "I could not check". A guard whose own failure path is broken is not a guard.
  # Callers that need the error TEXT must capture it themselves, as push_dirty now does.
  local out; out="$(git -C "$REPO_DIR" "$@" 2>&1)" || { GIT_ERR="$out"; return 1; }
  printf '%s' "$out"
}

push_unshipped_line() { # $1 basename, $2 live copy -> one-line summary of what is unshipped
  # Only reached once live_is_ahead() has confirmed the baseline IS an ancestor of HEAD, so this
  # is purely cosmetic: name the commits between that baseline and here. All the judgement lives
  # in live_is_ahead; this just reads the list out.
  local f="$1" live="$2" livehash n
  livehash="$(git -C "$REPO_DIR" hash-object "$live" 2>/dev/null)" || { echo "(unknown)"; return; }
  local base=""
  while read -r sha; do
    [ -n "$sha" ] || continue
    [ "$(git -C "$REPO_DIR" rev-parse "$sha:ha/packages/$f" 2>/dev/null || true)" = "$livehash" ] \
      && { base="$sha"; break; }
  done < <(git -C "$REPO_DIR" log --all -n "$LIVE_BASELINE_DEPTH" --format=%H -- "ha/packages/$f" 2>/dev/null || true)
  [ -n "$base" ] || { echo "(live is at an unrecognised baseline)"; return; }
  n="$(git -C "$REPO_DIR" log --format=%H "$base..HEAD" -- "ha/packages/$f" 2>/dev/null | grep -c . || true)"
  [ "${n:-0}" -eq 0 ] && { echo "(live is at HEAD for this file)"; return; }
  echo "$n unshipped: $(git -C "$REPO_DIR" log -1 --format='%h %s' HEAD -- "ha/packages/$f")"
}

# --- push (repo -> live) -------------------------------------------------------------------
cmd_push() {
  local dry=0 from_worktree=0 all=0 allow_revert=0; local -a scope=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --dry-run) dry=1 ;;
      --from-worktree) from_worktree=1 ;;
      --all) all=1 ;;
      --allow-revert) allow_revert=1 ;;
      -*) die "unknown push flag '$1' (--dry-run | --from-worktree | --all | --allow-revert)" ;;
      "") ;;
      *) packages | grep -qxF "$1" || die "no such package '$1' (have: $(packages | tr '\n' ' '))"
         scope+=("$1") ;;
    esac; shift
  done
  local stamp; stamp="$(date +%Y%m%d-%H%M%S)"
  local tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  local -a changed=() ; local needs_full=0

  # ---- GUARD, before the network and long before any mutation ----------------------------
  # One substitution captures git's output AND its stderr AND its status, all in this shell —
  # deliberately not via a helper, so the error text survives to be printed.
  local raw dirty
  if ! raw="$(git -C "$REPO_DIR" status --porcelain -- ha/packages "$PUSH_SCRIPT_REL" 2>&1)"; then
    echo "!! cannot determine whether the working tree is clean — ${raw:-git failed}" >&2
    echo "   Refusing to push: unverified is not the same as clean." >&2
    return 2
  fi
  dirty="$(printf '%s' "$raw" | sed -n 's/^.\{3\}//p')"
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
    # Redirect git DIRECTLY to the file. Routing it through git_or_die means a command
    # substitution, and `$( )` STRIPS TRAILING NEWLINES — so every package arrived one byte short
    # of its committed self. Effect: every file always looked changed, and a push would have
    # written a file with no final newline to the VM on every run. Caught because push_unshipped
    # (which hashes the real blob) said "live is at HEAD" while `cmp` insisted it differed; two
    # components disagreeing is what made a silent byte-level bug visible.
    if ! git -C "$REPO_DIR" show "HEAD:ha/packages/$f" > "$SRC/$f" 2>/dev/null; then
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
    # THE ENTITY-SET TEST comes first, and it is structural. The grep below could not answer this
    # and its author thought it could: the alternative meant to catch a new child key,
    # `^[<>]  [a-z0-9_]+:`, sits after `.*` in the same pattern, and in ERE a `^` there can never
    # match. It was DEAD from the day it was written, under a comment describing the case it
    # fails to catch. So adding four `input_*` helpers under sections that already existed chose
    # `automation.reload`, check_config passed, the deploy reported SUCCESS — and none of the
    # four helpers existed (luna, 2026-07-28). A reload that cannot create what was asked for,
    # reporting success, is the worst failure this script can have.
    #
    # Comparing the parsed ENTITY SETS cannot have that bug, because it never looks at text.
    local sig_new sig_live
    if ! sig_new="$(reload_signature "$SRC/$f" 2>/dev/null)" \
       || ! sig_live="$(reload_signature "$tmp/$f" 2>/dev/null)"; then
      note "$f: cannot parse one side's entity set — choosing reload_all (fail safe)"
      needs_full=1
    elif [ "$sig_new" != "$sig_live" ]; then
      # Say WHICH, because a silent choice is how this went unnoticed for a whole deploy.
      local added removed
      added="$(comm -13 <(printf '%s\n' "$sig_live") <(printf '%s\n' "$sig_new") | tr '\n' ' ')"
      removed="$(comm -23 <(printf '%s\n' "$sig_live") <(printf '%s\n' "$sig_new") | tr '\n' ' ')"
      note "$f: entity set changes → reload_all${added:+ · adds: $added}${removed:+ · removes: $removed}"
      needs_full=1
    elif grep -qE '^[<>]' "$tmp/$f.diff" \
       && grep -qE '^[<>].*(alias:|trigger:|action:|service:|condition:|mode: (single|parallel|queued|restart))' "$tmp/$f.diff"; then
      : # same entities, automation bodies changed — automation.reload is genuinely sufficient
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
  local -a behind=() unknown=(); local verdict
  for f in "${changed[@]}"; do
    verdict="$(live_is_ahead "$f" "$tmp/$f")"
    case "$verdict" in
      BEHIND*)  behind+=("$f");  printf '    %-22s %s\n' "$f" "live is NEWER — ${verdict#BEHIND }" ;;
      UNKNOWN)  unknown+=("$f"); printf '    %-22s %s\n' "$f" "cannot attribute — live matches no commit anywhere" ;;
      *)        printf '    %-22s %s\n' "$f" "$(push_unshipped_line "$f" "$tmp/$f")" ;;
    esac
  done
  # BEHIND gets its OWN refusal and its own flag. It is not a wider batch, it is the opposite
  # hazard: pushing would UNDO work already deployed, and nothing in a batch listing looks wrong
  # while it happens — one file, one plausible subject. "Ship it forward" is not consent to undo.
  if [ ${#behind[@]} -gt 0 ] && [ "$allow_revert" -eq 0 ]; then
    echo
    echo "  REFUSED · live is NEWER than your HEAD for: ${behind[*]}" >&2
    echo "  Pushing would REVERT work that is already deployed. Update your branch first:" >&2
    echo "      git pull   (or rebase onto the branch that carries it)" >&2
    echo "      ./tools/ha_deploy.sh push --allow-revert    (if undoing it is the intent)" >&2
    return 4
  fi
  [ ${#behind[@]} -gt 0 ] && note "--allow-revert: OVERWRITING live's newer copy of ${behind[*]}"
  if [ ${#unknown[@]} -gt 0 ] && [ "$all" -eq 0 ]; then
    echo
    echo "  REFUSED · cannot attribute ${unknown[*]}: live matches no committed version anywhere," >&2
    echo "  so it was edited on the VM and a push would silently overwrite those edits." >&2
    echo "  Run \`pull\` to capture them, or --all if you have decided to discard them." >&2
    return 4
  fi
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
