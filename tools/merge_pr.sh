#!/usr/bin/env bash
# merge_pr.sh — merge a PR only if its head is still the branch's tip.
#
# ── WHY ───────────────────────────────────────────────────────────────────────
# On 2026-08-01, PR #365 was merged while two commits were in flight to its branch. The merge
# EVENT fired correctly; the CONTENT did not all land. One of the orphaned commits was the
# refusal that stops `[tier_exclusive]` — the load-bearing arm of that very PR's gate — from
# being switched off by deleting its table. So the gate shipped with its kill switch unguarded,
# and nothing said so for hours. Four other PRs merged the same night were checked afterwards
# and were clean; this one was not.
#
# The fix everyone reaches for is "check the branch tip before merging". That is a REFLEX, and a
# reflex that lives in someone's memory has already stopped running — the lesson this repo has
# now learned in eight other places (#338 prose gate, #339 shed order, #350 tier coverage, #351
# byte-free claims). So the check is not written down as a habit. It IS the merge path.
#
# ── WHAT IT CHECKS ────────────────────────────────────────────────────────────
# GitHub's view of the PR head vs the LIVE remote branch tip, both fetched at merge time. That
# pair catches both ways this goes wrong: a `gh pr view` that has not caught up, and commits
# that arrive between looking and merging.
#
# FAILS CLOSED. Cannot read a SHA, cannot resolve the branch, PR not OPEN → refuse. A merge
# helper that proceeds when it cannot verify is worse than no helper, because it looks like one.
#
# Usage:  tools/merge_pr.sh <pr-number> [extra args passed to `gh pr merge`]
#         tools/merge_pr.sh 365 --squash --delete-branch
#         SELFTEST=1 tools/merge_pr.sh          # prove the refusal fires; no network, no merge
set -uo pipefail

die() { printf '\033[31mREFUSING\033[0m %s\n' "$1" >&2; exit 1; }

# The whole decision, isolated so the self-test can drive it without a network or a live PR.
# Returns 0 to proceed, 1 to refuse (and prints why).
verify_tip() {                                   # verify_tip <branch> <pr-head> <remote-tip>
  local branch="$1" head="$2" tip="$3"
  [ -n "$head" ] && [ "$head" != "null" ] || { echo "cannot read the PR head SHA"; return 1; }
  [ -n "$tip" ] || { echo "cannot read the remote tip of '$branch'"; return 1; }
  [ "$head" = "$tip" ] && return 0
  echo "PR head is NOT the branch tip — commits would be LEFT BEHIND by this merge
    branch      $branch
    PR head     $head
    remote tip  $tip
  Merging now lands the PR head and orphans everything after it. Either wait for CI on the
  new tip, or confirm the extra commits are unwanted. This is the #365 failure exactly."
  return 1
}

# ── self-test: watch it go red before believing it ────────────────────────────
# Not optional decoration. A guard nobody has seen refuse is the thing this repo keeps
# rediscovering; a merge guard that silently always passes would be the most expensive possible
# instance of it, because it would be trusted at exactly the moment work is destroyed.
if [ -n "${SELFTEST:-}" ]; then
  fails=0
  t() { # t <name> <want-rc> <branch> <head> <tip>
    local out; out="$(verify_tip "$3" "$4" "$5" 2>&1)"; local rc=$?
    if [ "$rc" = "$2" ]; then printf '   \033[32mok\033[0m   %s\n' "$1"
    else printf '   \033[31mFAIL\033[0m %s (rc=%s, want %s) %s\n' "$1" "$rc" "$2" "$out"; fails=1; fi
  }
  echo "  merge_pr self-test"
  t "head == tip proceeds"                0 feat/x aaaa111 aaaa111
  t "head behind tip REFUSES"             1 feat/x aaaa111 bbbb222
  t "unreadable PR head refuses"          1 feat/x ""      bbbb222
  t "null PR head refuses"                1 feat/x null    bbbb222
  t "unresolvable branch refuses"         1 feat/x aaaa111 ""
  # The message has to name the orphaned SHA, or a reader cannot act on the refusal.
  out="$(verify_tip feat/x aaaa111 bbbb222 2>&1)"
  case "$out" in *bbbb222*) printf '   \033[32mok\033[0m   %s\n' "the refusal names the tip it saw" ;;
                 *) printf '   \033[31mFAIL\033[0m the refusal does not name the tip\n'; fails=1 ;; esac
  [ "$fails" = 0 ] && { echo "  self-test passed"; exit 0; } || { echo "  SELF-TEST FAILED"; exit 1; }
fi

# ── the merge path ────────────────────────────────────────────────────────────
PR="${1:-}"
case "$PR" in ''|*[!0-9]*) echo "usage: tools/merge_pr.sh <pr-number> [gh pr merge args]" >&2; exit 2 ;; esac
shift

command -v gh >/dev/null || die "gh is not installed"
meta="$(gh pr view "$PR" --json state,headRefName,headRefOid 2>/dev/null)" \
  || die "cannot read PR #$PR"
state="$(printf '%s' "$meta"  | jq -r .state)"
branch="$(printf '%s' "$meta" | jq -r .headRefName)"
head="$(printf '%s' "$meta"   | jq -r .headRefOid)"
[ "$state" = "OPEN" ] || die "PR #$PR is $state, not OPEN"

# `git ls-remote` rather than another `gh` field: it asks the git server for the ref RIGHT NOW,
# which is the number that matters. The API's PR object is the thing that can be stale.
tip="$(git ls-remote origin "refs/heads/$branch" 2>/dev/null | cut -f1)"

if ! out="$(verify_tip "$branch" "$head" "$tip")"; then
  printf '%s\n' "$out" >&2
  # Name the commits, not just the SHAs — "two commits behind" is not actionable, a subject line is.
  if [ -n "$tip" ] && [ -n "$head" ] && [ "$head" != "null" ]; then
    echo "  Orphaned by this merge:" >&2
    gh api "repos/{owner}/{repo}/compare/$head...$tip" --jq \
      '.commits[] | "    \(.sha[0:7])  \(.commit.message | split("\n")[0])"' 2>/dev/null \
      || echo "    (could not list them — compare $head...$tip by hand)" >&2
  fi
  exit 1
fi

echo "PR #$PR: head $head IS the tip of $branch — merging."
exec gh pr merge "$PR" "$@"
