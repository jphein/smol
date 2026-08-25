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
#   ./tools/ha_deploy.sh assets        # theme/www file pairs (#321) (READ-ONLY; exits 1 on drift)
#   ./tools/ha_deploy.sh pull          # live -> repo, so out-of-band edits become commits
#   ./tools/ha_deploy.sh push [file...] # repo -> live from HEAD, validated, with rollback
#   ./tools/ha_deploy.sh push --all      # every differing package (required if >1)
#   ./tools/ha_deploy.sh push --from-worktree   # push uncommitted state on purpose
#   ./tools/ha_deploy.sh push --dry-run
#
# `push` RE-CHECKS THE DASHBOARD AFTERWARDS (#333). A push adds and removes entities; the Lovelace
# view wires entities by id, so a removal turns its card into a dead row — and every pre-flight gate
# is green at that moment because the entity still exists. It warns, never fails: the push already
# succeeded, and the actionable output is the entity list, not the exit status.
#   ./tools/ha_deploy.sh push --assets [path...]  # the theme/www pairs — a SEPARATE batch from
#                                        packages, so the two classes never ride together
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

# --- #321: the SECOND class of managed path — explicit file PAIRS, not a directory sync ------
# Everything this repo installs into HA that is not a package and not the Lovelace view. Before
# this, none of it was compared by anything: `ha/www/luna-fonts/ATTRIBUTION.md` existed in the
# repo and had NEVER been deployed, and eighteen days of both sides looking identical on every
# file someone would spot-check never revealed it. That file is the SIL OFL 1.1 notice for the
# two fonts HA serves to browsers — i.e. the miss landed on the one file with an external
# obligation, so the consequence did not scale with the plausibility. That is the reason for a
# manifest: you cannot tell which unwatched file matters until you look.
#
#   <repo-relative path>:<absolute live path>:<reload action>
#
# The reload action is DECLARED, never inferred — guessing is how the existing reload-choice bug
# shipped. A changed theme needs `frontend.reload_themes`; fonts need nothing, because the
# browser refetches. `none` is a real answer here, not a placeholder.
#
# Enumerated rather than synced wholesale ON PURPOSE: `/config/www/` is served as `/local/…`
# WITHOUT authentication, so anything landing there is public to anyone who can reach the
# instance. That is correct for OFL fonts and a licence notice, and it is exactly why a new
# file should have to be named here rather than swept up by a directory glob.
ASSETS=(
  "ha/themes/smol.yaml:/config/themes/smol.yaml:reload_themes"
  "ha/www/luna-fonts/vt323.woff2:/config/www/luna-fonts/vt323.woff2:none"
  "ha/www/luna-fonts/ibmplexmono.woff2:/config/www/luna-fonts/ibmplexmono.woff2:none"
  "ha/www/luna-fonts/ATTRIBUTION.md:/config/www/luna-fonts/ATTRIBUTION.md:none"
)

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

# --- assets (#321) --------------------------------------------------------------------------
# Compared by MD5, never by `diff`: two of the four are `.woff2`, and a comparison that silently
# degrades on binary input is the same class of defect as the gap this closes.
asset_fields() { # $1 manifest entry -> sets REPO_P / LIVE_P / RELOAD_A in the caller
  IFS=: read -r REPO_P LIVE_P RELOAD_A <<<"$1"    # no path here contains ':'
}

assets_live_md5() { # -> "<md5>  <live path>" for those that EXIST live (absent = no line)
  local -a lp=(); local e
  for e in "${ASSETS[@]}"; do asset_fields "$e"; lp+=("$LIVE_P"); done
  # One round trip for all of them. A missing file simply produces no line, which is precisely
  # the MISSING signal — `md5sum` on an absent path is an error, not an empty hash.
  ssh "${SSH_OPTS[@]}" "$HA_SSH" "md5sum ${lp[*]} 2>/dev/null" || true
}

cmd_assets() { # read-only; 0 in sync · 1 drift · 2 could not check
  local live rc=0
  if ! live="$(assets_live_md5)"; then return 2; fi
  echo "assets (theme / www — file pairs, md5-compared):"
  local e lmd5 rmd5 drift=0 missing_repo=0
  for e in "${ASSETS[@]}"; do
    asset_fields "$e"
    if [ ! -f "$REPO_DIR/$REPO_P" ]; then
      printf '  %-38s MANIFEST ERROR — not in the repo\n' "$REPO_P"; missing_repo=1; continue
    fi
    lmd5="$(md5sum "$REPO_DIR/$REPO_P" | cut -d' ' -f1)"
    # Match on the live PATH, anchored, so one asset's hash can never be read for another.
    rmd5="$(printf '%s\n' "$live" | awk -v p="$LIVE_P" '$2==p{print $1; exit}')"
    if [ -z "$rmd5" ]; then
      printf '  %-38s MISSING on live (never deployed)\n' "$REPO_P"; drift=1
    elif [ "$lmd5" = "$rmd5" ]; then
      printf '  %-38s in sync\n' "$REPO_P"
    else
      printf '  %-38s DIFFERS  (repo %s | live %s)\n' "$REPO_P" "${lmd5:0:8}" "${rmd5:0:8}"; drift=1
    fi
  done
  [ "$missing_repo" -eq 1 ] && rc=2
  [ "$drift" -eq 1 ] && [ "$rc" -eq 0 ] && rc=1
  case "$rc" in
    1) echo "  → drift: \`push --assets\` deploys them (reload action is per-asset, declared)." ;;
    2) echo "  → the manifest names a file the repo does not have; assets are UNVERIFIED." ;;
  esac
  return "$rc"
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
  # #321 — same rule as the dashboard above: 1 is drift, and 2 ("could not check") must never
  # read as in sync. A clean answer about a subset is what let ATTRIBUTION.md go undeployed.
  local arc=0; cmd_assets || arc=$?
  [ "$arc" -ne 0 ] && drift=1
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
  # #321 — text assets get a real diff; binary ones get their md5 pair and say so, because a
  # `diff` that quietly reports "Binary files differ" is not a comparison, it is a shrug.
  echo "=== assets  (text: unified diff · binary: md5 pair)"
  local e; for e in "${ASSETS[@]}"; do
    asset_fields "$e"
    [ -f "$REPO_DIR/$REPO_P" ] || { echo "--- $REPO_P: not in the repo (manifest error)"; continue; }
    if grep -Iq . "$REPO_DIR/$REPO_P" 2>/dev/null; then     # -I: binary counts as no-match
      ssh "${SSH_OPTS[@]}" "$HA_SSH" "cat $LIVE_P 2>/dev/null" > "$tmp/asset.live" || true
      if cmp -s "$REPO_DIR/$REPO_P" "$tmp/asset.live"; then echo "--- $REPO_P: in sync"; continue; fi
      echo "--- $REPO_P  (< repo | > live)"
      diff -u "$REPO_DIR/$REPO_P" "$tmp/asset.live" || true
    else
      local lm rm; lm="$(md5sum "$REPO_DIR/$REPO_P" | cut -d' ' -f1)"
      rm="$(ssh "${SSH_OPTS[@]}" "$HA_SSH" "md5sum $LIVE_P 2>/dev/null" | cut -d' ' -f1)"
      if [ "$lm" = "${rm:-}" ]; then echo "--- $REPO_P: in sync (binary, md5 ${lm:0:8})"
      else echo "--- $REPO_P: DIFFERS (binary) repo=$lm live=${rm:-MISSING}"; fi
    fi
  done
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
    # #356 — the inverse of 3: not a card pointing at nothing, but a live board no card mentions.
    # Given its own arm for the reason the comment above insists on: the remediation is in a
    # different file (usually a view this repo does not build), so folding it into 1 or 3 would
    # send someone to edit a scaffold that has never contained the card.
    5) echo "  → UNCOVERED NODES: a live board appears on NO card. The dashboard is not wrong,"
       echo "    it is incomplete — a view is hardcoding node ids instead of enumerating the"
       echo "    fleet (see #356). The list is above." ;;
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

# --- push assets (#321) ----------------------------------------------------------------------
# A SEPARATE batch from packages, deliberately: #318's incident was one operator's push silently
# carrying another author's work, and mixing two classes of managed path into one envelope is
# the same mistake with a new surface. `--assets` pushes assets and nothing else.
push_assets() { # $1 dry, $2 all, $3 from_worktree, rest: scoped repo paths
  local dry="$1" all="$2" from_worktree="$3"; shift 3
  local -a scope=("$@") changed=() reloads=()
  local stamp; stamp="$(date +%Y%m%d-%H%M%S)"
  local tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  local live; live="$(assets_live_md5)" || { echo "cannot reach live to compare assets" >&2; return 2; }

  local e lmd5 rmd5 n=0
  for e in "${ASSETS[@]}"; do
    asset_fields "$e"
    if [ ${#scope[@]} -gt 0 ]; then
      case " ${scope[*]} " in *" $REPO_P "*) ;; *) continue ;; esac
    fi
    [ -f "$REPO_DIR/$REPO_P" ] || { note "$REPO_P: not in the repo — skipped"; continue; }
    # Source is HEAD, exactly as packages are (#318): the bytes pushed must be the bytes
    # reviewed. `git show` to a FILE, never `$( )` — command substitution strips trailing
    # newlines and would corrupt every binary asset.
    n=$((n+1)); local src="$tmp/a$n"
    if [ "$from_worktree" -eq 1 ]; then cp "$REPO_DIR/$REPO_P" "$src"
    elif ! git -C "$REPO_DIR" show "HEAD:$REPO_P" > "$src" 2>/dev/null; then
      note "$REPO_P: not in HEAD (new/uncommitted) — skipped; --from-worktree to push it anyway"
      continue
    fi
    lmd5="$(md5sum "$src" | cut -d' ' -f1)"
    rmd5="$(printf '%s\n' "$live" | awk -v p="$LIVE_P" '$2==p{print $1; exit}')"
    [ "$lmd5" = "${rmd5:-}" ] && continue
    changed+=("$REPO_P:$LIVE_P:$RELOAD_A:$src")
    printf '    %-38s %s\n' "$REPO_P" "${rmd5:+repo ${lmd5:0:8} -> live ${rmd5:0:8}}${rmd5:-NEW on live}"
  done

  [ ${#changed[@]} -eq 0 ] && { echo "  assets: nothing to push."; return 0; }
  # Same refusal shape as the package path: never send a batch nobody named.
  if [ ${#scope[@]} -eq 0 ] && [ ${#changed[@]} -gt 1 ] && [ "$all" -eq 0 ]; then
    echo
    echo "  REFUSED · an unscoped push would send all ${#changed[@]} assets above in one batch." >&2
    echo "  Name what you mean:  ./tools/ha_deploy.sh push --assets ${changed[0]%%:*}" >&2
    echo "  or:                  ./tools/ha_deploy.sh push --assets --all" >&2
    return 4
  fi
  [ "$dry" -eq 1 ] && { echo "  (dry run — nothing sent)"; return 0; }

  local c rp lp ra sf
  for c in "${changed[@]}"; do
    IFS=: read -r rp lp ra sf <<<"$c"
    ssh "${SSH_OPTS[@]}" "$HA_SSH" \
      "sudo mkdir -p $(dirname "$lp"); [ -f $lp ] && sudo cp $lp $lp.bak-deploy-$stamp || true"
    # base64 over the wire: two of these are .woff2, and a byte-mangled font fails silently in
    # a browser rather than here. `tee` alone is fine for text and not worth trusting for binary.
    base64 < "$sf" | ssh "${SSH_OPTS[@]}" "$HA_SSH" "base64 -d | sudo tee $lp > /dev/null" \
      || die "copy of $rp failed"
    # VERIFY AFTER WRITE — a copy that reports success and lands wrong is the failure this whole
    # issue is about. Re-read the live hash rather than trusting an exit status.
    local want got; want="$(md5sum "$sf" | cut -d' ' -f1)"
    got="$(ssh "${SSH_OPTS[@]}" "$HA_SSH" "md5sum $lp 2>/dev/null" | cut -d' ' -f1)"
    [ "$want" = "${got:-}" ] || die "$rp: post-write md5 mismatch (want $want, live ${got:-none}) — restore from $lp.bak-deploy-$stamp"
    note "$rp -> $lp: copied + verified ($want)"
    [ "$ra" = "none" ] || case " ${reloads[*]:-} " in *" $ra "*) ;; *) reloads+=("$ra") ;; esac
  done
  # Declared, per-asset, deduped. Fonts reload NOTHING: the browser refetches, and inventing a
  # reload for them would be the same guessing that produced the reload-choice bug.
  local r
  for r in "${reloads[@]:-}"; do
    [ -n "$r" ] || continue
    ha_api POST "/api/services/frontend/$r" '{}' >/dev/null && note "frontend.$r"
  done
  [ ${#reloads[@]} -eq 0 ] && note "no reload needed (browser refetches static assets)"
  echo
  echo "pushed ${#changed[@]} asset(s). Backups on the VM: *.bak-deploy-$stamp"
}

# --- push (repo -> live) -------------------------------------------------------------------
cmd_push() {
  local dry=0 from_worktree=0 all=0 allow_revert=0; local -a scope=()
  # #321: decide the CLASS before validating any name, so `--assets` may appear anywhere in the
  # argument list rather than only first. Validating an asset path against `packages()` would
  # otherwise die with a confusing "no such package".
  local assets_mode=0 _a
  for _a in "$@"; do [ "$_a" = "--assets" ] && assets_mode=1; done
  while [ $# -gt 0 ]; do
    case "$1" in
      --dry-run) dry=1 ;;
      --from-worktree) from_worktree=1 ;;
      --all) all=1 ;;
      --allow-revert) allow_revert=1 ;;
      --assets) ;;   # class selector, already scanned above
      -*) die "unknown push flag '$1' (--dry-run | --from-worktree | --all | --allow-revert | --assets)" ;;
      "") ;;
      # 2026-07-28: was `packages | grep -qxF "$1" || die …`. Under `pipefail`, deciding on
      # the status of `cmd | grep -q` is unreliable AT ANY SIZE — grep -q exits on first
      # match, the writer takes EPIPE, and pipefail surfaces the WRITER's status. Measured
      # in this repo: 3/1500 spurious non-matches on a ~3.9 KB input, and identical
      # behaviour from GNU grep 3.11 and ugrep 7.5.0, so it is not tool-specific and it
      # generalises to CI. `grep -q` reading a FILE is 0/2000 — the pipeline is the defect,
      # not grep. I first triaged this site as "small input, low risk, leave it" on the
      # since-refuted theory that only large inputs or early matches were affected; a late
      # match at 266 KB still failed 2-3.5%. So the rule is absolute rather than sized, and
      # this is now a pipe-free membership test that cannot fail for pipeline reasons.
      *) if [ "$assets_mode" -eq 1 ]; then
           _known=""; for _a in "${ASSETS[@]}"; do _known="$_known${_a%%:*} "; done
           case " $_known" in
             *" $1 "*) ;;
             *) die "no such asset '$1' (have: $_known)" ;;
           esac
         else
           _pkgs="$(packages)"
           case $'\n'"$_pkgs"$'\n' in
             *$'\n'"$1"$'\n'*) ;;
             *) die "no such package '$1' (have: $(printf '%s' "$_pkgs" | tr '\n' ' '))" ;;
           esac
         fi
         scope+=("$1") ;;
    esac; shift
  done
  if [ "$assets_mode" -eq 1 ]; then
    echo "  would push asset(s):"
    push_assets "$dry" "$all" "$from_worktree" "${scope[@]+"${scope[@]}"}"
    return $?
  fi
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

  # --- #333 GAP 2: re-check the DASHBOARD *after* the push, not only before -------------------
  # A package push adds and REMOVES entities (the push report above prints exactly which). The
  # Lovelace view wires entities by id, so removing one silently turns its card into a dead row —
  # and every pre-flight gate is green at that moment BECAUSE THE ENTITY STILL EXISTS. The deploy
  # is what creates the breakage, so a check that only ever runs beforehand is structurally unable
  # to see it.
  #
  # That is not hypothetical. 2026-08-01: #320 removed `sensor.smol_8_{delivery,tale}`, and
  # `vertical-stack|node8` was still wired to both. `--check` had passed cleanly minutes earlier.
  # Two "Entity not found" rows shipped to JP's dashboard and were found only because someone
  # happened to re-run the check afterwards.
  #
  # Run automatically rather than documented as a step, because "remember to re-check after
  # deploying" is precisely the discipline that failed the first time. WARNS, never fails: the push
  # already succeeded and rolling HA back over a dashboard row would be wildly disproportionate —
  # the actionable output is the entity list, not the exit status. Skipped when the generator or a
  # token is unavailable, and its own failure is reported rather than swallowed.
  local dash_rc=0
  if [ -z "${HA_TOKEN:-}" ] && command -v bw >/dev/null 2>&1; then
    HA_TOKEN="$(timeout 40 bw get password ha-llat 2>/dev/null)" || true
  fi
  if [ ! -f "$REPO_DIR/ha/dashboard/build_control_room.py" ]; then
    note "post-deploy dashboard check SKIPPED (generator not found)"
  elif [ -z "${HA_TOKEN:-}" ]; then
    note "post-deploy dashboard check SKIPPED (no HA_TOKEN; run: HA_TOKEN=… $0 push …)"
  else
    echo
    echo "  post-deploy dashboard check (#333) — did this push kill any card's rows?"
    # ONE invocation. Running it twice (once for text, once for status) would spend a second
    # websocket round trip and, worse, could report a verdict from a different read than the lines
    # printed above it — a check disagreeing with its own output is not a check.
    local dash_out=""
    dash_out="$(HA_TOKEN="$HA_TOKEN" timeout 180 python3 \
      "$REPO_DIR/ha/dashboard/build_control_room.py" --check 2>&1)" || dash_rc=$?
    printf '%s\n' "$dash_out" | sed -n '/^LIVE-ONLY/p;/^DEAD ROWS/p;/^UNCOVERED/p;/^  ⚠/p;/^    - /p' | sed 's/^/    /'
    # #340: that count is DASHBOARD-WIDE — every view, not just `smol-control` — so this warning
    # now covers rows on views this repo does not generate (a push that deletes an entity can kill
    # a card on any of them). The generator deliberately keeps it as ONE `DEAD ROWS · N` line so
    # this regex needs no change; a per-view section would have re-created #340 right here.
    #
    # DEAD ROWS is read from the OUTPUT, not from the exit code, and that distinction matters:
    # `report_check` returns `1 if extras else (3 if dead else 0)`, so ANY live-only drift MASKS the
    # dead-rows status. Keying this warning off `$dash_rc == 3` alone would mean that on an instance
    # with pre-existing LIVE-ONLY drift — which is the normal state whenever a back-port is pending —
    # a push that killed a card's rows would print "pre-existing drift" and say nothing about the
    # rows it just broke. The exit code answers "what should I fix FIRST", which is a narrower
    # question than "did this push break anything", and confusing the two is the whole of #333.
    local dead_line
    dead_line="$(printf '%s\n' "$dash_out" | sed -n 's/^DEAD ROWS · \([0-9][0-9]*\).*/\1/p' | head -1)"
    if [ "${dead_line:-0}" -gt 0 ] 2>/dev/null; then
      echo "  ⚠ DEAD ROWS ($dead_line) — an entity a dashboard card wires is missing from HA." >&2
      echo "    A push that REMOVES entities is the usual cause; the list is above. Fix the card or" >&2
      echo "    the generator, then re-run: python3 ha/dashboard/build_control_room.py --check" >&2
    elif [ "$dash_rc" -eq 0 ]; then
      note "dashboard still clean (LIVE-ONLY 0 · DEAD ROWS 0)"
    fi
    case "$dash_rc" in
      0|3) : ;;   # 3 is already covered by the DEAD ROWS branch above
      1) note "dashboard also reports LIVE-ONLY drift (see #305 — back-port vs retire is a decision)" ;;
      # #356: a real finding, not a failure to run. Deliberately a `note` and not a ⚠ — an
      # uncovered node is a pre-existing coverage gap, not damage this push just did, and the
      # post-deploy question here is "did this push break anything".
      5) note "dashboard also reports UNCOVERED live node(s) with no card at all (#356)" ;;
      *) echo "  ⚠ post-deploy dashboard check could not run (exit $dash_rc) — run it by hand." >&2 ;;
    esac
  fi
}

case "${1:-status}" in
  status) cmd_status ;;
  diff)   cmd_diff "${2:-}" ;;
  dash)   cmd_dash ;;   # exit code propagates: 0 in sync · 1 live-only drift · 2 could not check
  assets) cmd_assets ;; # #321; same contract as dash: 0 in sync · 1 drift · 2 could not check
  pull)   cmd_pull ;;
  push)   shift; cmd_push "$@" ;;
  -h|--help|help) sed -n '2,34p' "${BASH_SOURCE[0]}" ;;
  *) die "unknown mode '${1}' (status | diff | dash | assets | pull | push)" ;;
esac
