#!/usr/bin/env bash
# status_check.sh — re-test the machine-checkable claims in issue #148, and FAIL when the doc
# has rotted. (#148)
#
# WHY THIS EXISTS
#
# #148 is the project status doc. It has been rewritten twice, and each time the previous
# snapshot had gone almost entirely stale — the 2026-08-01 revision opens by saying so about the
# 2026-07-20 one, and was itself found stale 24 days later with five falsifiable claims,
# including a Tier-B queue item instructing readers to redo settled work (`#233 merge (PR #247)`
# when #233 was closed and #247 closed-unmerged).
#
# Twice is a pattern, and the cause is structural rather than anyone's diligence: NOTHING MADE
# THE DOCUMENT FAIL. It rotted silently and was rewritten when a human happened to notice. That
# is this repo's signature defect shape (#371) at the document level, and the same shape #384
# fixed for the vendored sigil corpus — a checker nobody invokes is the same shape as prose.
#
# WHERE THE CLAIMS LIVE, AND WHY NOT HERE
#
# The assertions live in #148's BODY, as `<!-- check: … -->` annotations next to the prose they
# anchor. This script holds NO copy of them. That is deliberate and it is the whole design: a
# script carrying its own list of claims would be two statements of one fact, which is exactly
# the rot that let `mesh_elect`'s tag list and #148 itself go stale. There is one statement, in
# the doc, and this file only evaluates it.
#
# Consequence worth stating plainly: editing the prose without editing its annotation makes this
# script fail. That is the designed outcome, not an incident.
#
# WHAT IT CANNOT DO
#
# It does not keep the prose true — only the mechanically checkable half. A paragraph can be
# beautifully wrong about strategy and pass every check here. The value is narrower and real:
# the factual half is the half that sends people to redo settled work.
#
# CHECK FAMILIES  (kind + args, as they appear in the doc)
#   issue-closed <n>            an issue is closed        (and IS an issue, not a PR)
#   issue-open <n>              an issue is open          (and IS an issue, not a PR)
#   pr-merged <n>               a PR is merged
#   pr-closed-unmerged <n>      a PR is closed and was NOT merged
#   branch-absent <name>        no such branch on origin
#   file-exists <path>          path exists in the repo
#   grep-absent <pattern> <dir> pattern does not occur under dir
#
# EXIT CODES
#   0  every claim held
#   1  at least one claim FAILED — the doc is wrong somewhere
#   2  the script could not run (no gh, no network, unreadable body)
#
# USAGE
#   tools/status_check.sh                  # fetch issue #148's live body and check it
#   tools/status_check.sh --body-file F    # check a local copy (used to prove it can fail)
#   tools/status_check.sh --issue N        # check a different issue's annotations
set -uo pipefail

ISSUE="${SMOL_STATUS_ISSUE:-148}"
BODY_FILE=""
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PASS=0; FAIL=0
FAILED_CLAIMS=()

while [ $# -gt 0 ]; do
  case "$1" in
    --body-file) BODY_FILE="${2:-}"; shift 2 ;;
    --issue)     ISSUE="${2:-}"; shift 2 ;;
    -h|--help)   sed -n '2,48p' "$0"; exit 0 ;;
    *)           echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$1"; PASS=$((PASS+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAIL=$((FAIL+1)); FAILED_CLAIMS+=("$1"); }

# ── obtain the body ───────────────────────────────────────────────────────────────────────────
if [ -n "$BODY_FILE" ]; then
  [ -r "$BODY_FILE" ] || { echo "cannot read $BODY_FILE" >&2; exit 2; }
  BODY="$(cat "$BODY_FILE")"
  echo "── checking #$ISSUE annotations from LOCAL FILE $BODY_FILE"
else
  command -v gh >/dev/null || { echo "gh CLI not found" >&2; exit 2; }
  BODY="$(gh issue view "$ISSUE" --repo "${SMOL_REPO:-jphein/smol}" --json body --jq '.body' 2>/dev/null)" \
    || { echo "could not fetch issue #$ISSUE body" >&2; exit 2; }
  [ -n "$BODY" ] || { echo "issue #$ISSUE body is empty" >&2; exit 2; }
  echo "── checking #$ISSUE annotations (live body)"
fi

# ── parse ─────────────────────────────────────────────────────────────────────────────────────
#
# INLINE CODE SPANS ARE STRIPPED FIRST, and this is load-bearing rather than tidiness. #148's own
# opening paragraph explains the convention to the reader by quoting it:
#
#     every factual claim that CAN be machine-checked carries a `<!-- check: … -->` annotation
#
# That is a backticked EXAMPLE, not a claim. A parser that does not strip code spans either fails
# the doc on an unparseable kind ("…") or, worse, counts it as a check that trivially passes —
# and the document would then be teaching its own convention in a way that breaks the tool that
# reads it. Markdown already means "this is an example, not a directive" with backticks; honour it.
#
# After stripping, an unknown kind is a HARD FAIL, not a skip: a typo'd annotation that silently
# passes is precisely the failure this file exists to prevent.
LINE_NO=0
while IFS= read -r line; do
  LINE_NO=$((LINE_NO+1))
  stripped="$(printf '%s' "$line" | sed 's/`[^`]*`//g')"
  case "$stripped" in *'<!-- check:'*) ;; *) continue ;; esac

  # The prose this line anchors — trimmed, for the report. Readers need to know WHICH sentence
  # is wrong, not merely that check #7 failed.
  anchor="$(printf '%s' "$stripped" | sed 's/<!-- check:[^>]*-->//g; s/^[[:space:]-]*//; s/[[:space:]]*$//' | cut -c1-72)"

  while IFS= read -r ann; do
    [ -n "$ann" ] || continue
    kind="$(printf '%s' "$ann" | awk '{print $1}')"
    a1="$(printf '%s' "$ann" | awk '{print $2}')"
    a2="$(printf '%s' "$ann" | awk '{print $3}')"
    label="L$LINE_NO $kind ${a1:-}${a2:+ $a2}  — $anchor"

    case "$kind" in
      issue-closed|issue-open)
        json="$(gh api "repos/${SMOL_REPO:-jphein/smol}/issues/$a1" --jq '{s:.state,p:(.pull_request!=null)}' 2>/dev/null)"
        if [ -z "$json" ]; then bad "$label  [could not read issue $a1]"; continue; fi
        state="$(printf '%s' "$json" | sed -n 's/.*"s":"\([a-z]*\)".*/\1/p')"
        ispr="$(printf '%s' "$json" | grep -q '"p":true' && echo yes || echo no)"
        want="${kind#issue-}"
        if [ "$ispr" = yes ]; then bad "$label  [#$a1 is a PR, not an issue]"
        elif [ "$state" = "$want" ]; then ok "$label"
        else bad "$label  [is '$state', expected '$want']"; fi ;;
      pr-merged|pr-closed-unmerged)
        json="$(gh api "repos/${SMOL_REPO:-jphein/smol}/pulls/$a1" --jq '{s:.state,m:.merged}' 2>/dev/null)"
        if [ -z "$json" ]; then bad "$label  [could not read PR $a1]"; continue; fi
        state="$(printf '%s' "$json" | sed -n 's/.*"s":"\([a-z]*\)".*/\1/p')"
        merged="$(printf '%s' "$json" | grep -q '"m":true' && echo yes || echo no)"
        if [ "$kind" = pr-merged ]; then
          [ "$merged" = yes ] && ok "$label" || bad "$label  [not merged; state '$state']"
        else
          { [ "$state" = closed ] && [ "$merged" = no ]; } && ok "$label" \
            || bad "$label  [state '$state', merged=$merged]"
        fi ;;
      branch-absent)
        if [ -n "$(git -C "$REPO_ROOT" ls-remote --heads origin "$a1" 2>/dev/null)" ]; then
          bad "$label  [branch still exists on origin]"
        else ok "$label"; fi ;;
      file-exists)
        [ -e "$REPO_ROOT/$a1" ] && ok "$label" || bad "$label  [missing from the tree]" ;;
      grep-absent)
        if [ ! -d "$REPO_ROOT/$a2" ] && [ ! -f "$REPO_ROOT/$a2" ]; then
          bad "$label  [search path '$a2' does not exist — a vacuous pass is not a pass]"
        elif grep -rq -- "$a1" "$REPO_ROOT/$a2" 2>/dev/null; then
          bad "$label  [still present under $a2]"
        else ok "$label"; fi ;;
      *)
        bad "$label  [UNKNOWN check kind '$kind' — a check nobody can evaluate is not a check]" ;;
    esac
  done < <(printf '%s' "$stripped" | grep -o '<!-- check:[^>]*-->' \
             | sed 's/<!-- check:[[:space:]]*//; s/[[:space:]]*-->//')
done <<< "$BODY"

# ── verdict ───────────────────────────────────────────────────────────────────────────────────
echo
TOTAL=$((PASS+FAIL))
if [ "$TOTAL" -eq 0 ]; then
  echo "NO CHECKS FOUND in #$ISSUE — the annotations were removed, or the parser is broken." >&2
  echo "Either way this tool is no longer guarding anything, so that is a failure, not a pass." >&2
  exit 1
fi
echo "$PASS/$TOTAL claims hold."
if [ "$FAIL" -gt 0 ]; then
  echo
  echo "THE DOC IS WRONG in $FAIL place(s):"
  for c in "${FAILED_CLAIMS[@]}"; do echo "  • $c"; done
  echo
  echo "Fix the prose AND its annotation together — they are one statement. If a claim has"
  echo "simply moved on, that is the doc rotting exactly as designed to be caught."
  exit 1
fi
echo "#$ISSUE's machine-checkable half is intact."
