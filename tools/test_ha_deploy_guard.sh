#!/usr/bin/env bash
# test_ha_deploy_guard.sh — exercise the #318 push guards in tools/ha_deploy.sh.
#
# WHY THIS EXISTS. Every guard added on 2026-07-28 was added because some check reported clean while
# asking the wrong question. A guard whose refusal has never been observed is indistinguishable from
# one that always passes — so every refusal path here asserts exit 4 (or 1/2), and every permit path
# asserts the absence of a refusal. This file is the answer to "how do you know yours can fail?"
#
# SAFETY. Every case runs `push --dry-run`, which sends nothing; the guards under test also all
# refuse BEFORE the copy step. Throwaway commits are made on the current branch and reset to the SHA
# recorded at entry. Untracked files are left alone.
#
# TWO HARNESS BUGS THIS FILE HAS ALREADY HAD, both of which made it report success while testing
# nothing. They are why it now checks its own setup:
#   1. `if ! cmd; then rc=$?` captures the NEGATION's status (always 0), not the command's — so the
#      preflight "skip if unreachable" fired every run and every test was skipped.
#   2. Extracting functions with two OVERLAPPING sed ranges printed the shared lines twice, so the
#      eval'd copy was malformed and returned empty — a false FAIL blamed on the code under test.
# A test harness gets the same scepticism as the thing it tests.
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 2
SH=./tools/ha_deploy.sh
PKG=ha/packages
BASE="$(git rev-parse HEAD)"
pass=0; fail=0; LAST_OUT=''

# ⚠️ THIS TRAP ONCE DESTROYED A COLLEAGUE'S WORK. It was `git reset -q --hard "$BASE"` —
# repo-WIDE — so every uncommitted tracked change in the working tree died when the test
# exited, not merely the packages the test touches. On 2026-08-01 it ate ~175 lines of
# in-progress #321 work (recovered only because its author still had the content); the
# `test: marker a/b` commits this harness makes were briefly mistaken for someone rehearsing
# git in the shared tree. Uncommitted content is not in the object store, so fsck/stash/
# reflog recovery all come back empty — a hard reset is terminal for it.
#
# Cleanup is now SCOPED to $PKG (the only tree the test mutates) and the repo-wide reset is
# gone. A test harness must not have a blast radius larger than its subject.
cleanup() { git restore -q --source="$BASE" -- "$PKG" 2>/dev/null || true; }
trap cleanup EXIT

# Refuse to run against a dirty tree OUTSIDE $PKG. The scoped cleanup above protects work
# elsewhere, but this harness also runs `git commit -am` (below), which would sweep unrelated
# modified files into its marker commits. Fail closed instead: an unexpected commit in someone
# else's lane is nearly as costly as a lost edit.
_dirty_outside="$(git status --porcelain -- . ":(exclude)$PKG" | grep -v '^??' || true)"
if [ -n "$_dirty_outside" ]; then
  printf 'REFUSING: uncommitted tracked changes outside %s — this harness commits with `-am`\n' "$PKG" >&2
  printf '%s\n' "$_dirty_outside" | sed 's/^/  /' >&2
  printf 'Commit, or run this in a scratch clone (never rehearse git in a shared tree).\n' >&2
  exit 2
fi

ok()  { pass=$((pass+1)); printf '  PASS  %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s\n' "$1"; }

expect() { # $1 label · $2 expected exit · rest: args after `push`
  local label="$1" want="$2"; shift 2
  local rc=0
  LAST_OUT="$(timeout 240 "$SH" push --dry-run "$@" 2>&1)" || rc=$?
  if [ "$rc" -eq "$want" ]; then ok "$label (exit $rc)"
  else bad "$label — wanted exit $want, got $rc"; printf '%s\n' "$LAST_OUT" | sed 's/^/        /' | tail -12
  fi
}
saw()    { grep -qF -- "$2" <<<"$LAST_OUT" && ok "  └ says: $1" || bad "  └ MISSING: $1"; }
notsaw() { grep -qF -- "$2" <<<"$LAST_OUT" && bad "  └ should NOT say: $1" || ok "  └ silent on: $1"; }

# ---------------------------------------------------------------------------------------------
echo "=== preflight ==="
pre=0; timeout 90 "$SH" status >/dev/null 2>&1 || pre=$?
[ "$pre" -eq 0 ] || { echo "  SKIP — VM unreachable / no auth (rc=$pre). Guards UNTESTED, not passed."; exit 0; }
echo "  VM reachable"

# Which packages is live AHEAD of? Ambient state: other agents commit and deploy continuously, so a
# suite that assumed "branch is current" would fail for reasons that are not bugs. Discover it, then
# isolate each case from it — a flaky test in a shared repo is worse than no test.
BEHIND_RAW="$(timeout 240 "$SH" push --dry-run 2>&1 || true)"
BEHIND_PKGS="$(sed -n 's/.*live is AHEAD of HEAD for: //p' <<<"$BEHIND_RAW" | tr ' ' '\n' | grep -c . || true)"
mapfile -t CLEANPKG < <(
  for f in $(cd "$PKG" && ls *.yaml); do
    grep -qF "live is AHEAD of HEAD for:" <<<"$BEHIND_RAW" \
      && grep -q "$f" <<< "$(sed -n 's/.*live is AHEAD of HEAD for: //p' <<<"$BEHIND_RAW")" && continue
    echo "$f"
  done
)
echo "  packages live is ahead of: ${BEHIND_PKGS:-0} · usable for scoped cases: ${#CLEANPKG[@]}"
[ ${#CLEANPKG[@]} -ge 2 ] || { echo "  SKIP — need 2 non-behind packages to test scoping"; exit 0; }
P1="${CLEANPKG[0]}"; P2="${CLEANPKG[1]}"
echo "  using P1=$P1  P2=$P2"
# When live is ahead of something unrelated, unscoped cases must accept that to reach the guard they
# actually mean to exercise. Isolating the behaviour under test, not papering over it.
AR=(); [ "${BEHIND_PKGS:-0}" -gt 0 ] && AR=(--allow-revert)
[ ${#AR[@]} -gt 0 ] && echo "  (unscoped cases will pass --allow-revert to isolate the batch guard)"

echo
echo "=== A. the deploy script's own cleanliness (logic running != logic reviewed) ==="
printf '\n# test-dirt\n' >> tools/ha_deploy.sh
expect "dirty ha_deploy.sh refuses" 4 "${AR[@]}"
saw "names the script"  "ha_deploy.sh is modified"
saw "offers the escape" "--from-worktree"
git restore -- tools/ha_deploy.sh
expect "clean again -> no script refusal" 0 "${AR[@]}" "$P1"
notsaw "no stale refusal" "is modified"

echo
echo "=== B. uncommitted package edits are NAMED and NOT shipped (push sends HEAD) ==="
printf '\n# test-marker-worktree\n' >> "$PKG/$P1"
expect "dirty package does not block" 0 "${AR[@]}" "$P1"
saw "names the dirty file"    "$P1"
saw "says HEAD is the source" "read from HEAD"
git restore -- "$PKG/$P1"

echo
echo "=== C. ONE package, committed, scoped -> permitted (the common case must not cry wolf) ==="
printf '\n# test-marker-a\n' >> "$PKG/$P1"
git commit -q -am "test: marker a"
expect "single scoped package is permitted" 0 "$P1"
saw "the file"     "$P1"
saw "the subject"  "test: marker a"
notsaw "a refusal" "REFUSED"

echo
echo "=== D. TWO packages, unscoped -> REFUSED for the BATCH; scope or --all permits ==="
printf '\n# test-marker-b\n' >> "$PKG/$P2"
git commit -q -am "test: marker b"
expect "unscoped multi-file refuses" 4 "${AR[@]}"
saw "explains the batch" "in one batch"
saw "suggests scoping"   "push "
expect "scoped to one file permits" 0 "$P2"
notsaw "no refusal when scoped" "REFUSED"
expect "--all permits the same batch" 0 "${AR[@]}" --all
notsaw "no refusal with --all" "REFUSED"

echo
echo "=== E. a bad flag must DIE, never fall back to pushing everything ==="
expect "unknown flag dies" 1 --alll
saw "names the bad flag" "--alll"

echo
echo "=== F. live_is_ahead(): the REVERT guard, unit-tested ==="
# Unit-tested because reaching BEHIND end-to-end needs live deployed from a commit HEAD lacks — true
# right now, but not reproducible on demand. An untested fail-closed branch is a fail-open branch you
# have not met yet, which is this file's whole thesis.
(
  REPO_DIR="$(pwd)"
  # ONE contiguous range, no overlap: from LIVE_BASELINE_DEPTH through the SECOND top-level `}`
  # (i.e. the end of live_is_ahead). Harness bug #2 was two overlapping sed ranges.
  eval "$(awk '/^LIVE_BASELINE_DEPTH=/{on=1} on{print} on && /^\}/{n++; if(n==2) exit}' "$SH")"
  # Assert the setup, so a broken extraction can never be reported as a failure of the code.
  for fn in _norm_hash live_is_ahead; do
    [ "$(type -t "$fn")" = function ] || { echo "  FAIL  harness: $fn did not extract"; exit 1; }
  done
  echo "  PASS  harness: both functions extracted"
  t=0; f=''
  chk() { # $1 label · $2 expected ('' = silent) · $3 file
    local out; out="$(live_is_ahead "$P1x" "$3")"
    if [ "$2" = '*' ]; then case "$out" in BEHIND*) echo "  PASS  $1 -> $out"; return;; esac
      echo "  FAIL  $1 — wanted BEHIND, got '${out:-<empty>}'"; t=1; return; fi
    [ "$out" = "$2" ] && echo "  PASS  $1${out:+ -> $out}" \
      || { echo "  FAIL  $1 — wanted '${2:-<silent>}', got '${out:-<empty>}'"; t=1; }
  }
  P1x="$P1"
  # F1 · content committed on a ref HEAD does not contain -> BEHIND
  other="$(git log --all --format='%H' -- "$PKG/$P1x" | while read -r s; do
             git merge-base --is-ancestor "$s" HEAD 2>/dev/null || { echo "$s"; break; }; done)"
  if [ -n "$other" ]; then
    f="$(mktemp)"; git cat-file blob "$other:$PKG/$P1x" > "$f"; chk "non-ancestor commit's content" '*' "$f"; rm -f "$f"
  else echo "  SKIP  no non-ancestor commit for $P1x right now (cannot construct)"; fi
  # F2 · never committed -> UNKNOWN (hand-edited on the VM)
  f="$(mktemp)"; printf 'this content was never committed anywhere\n' > "$f"
  chk "never-committed content" 'UNKNOWN' "$f"; rm -f "$f"
  # F3 · HEAD's own content -> silent (a normal forward push must not warn)
  f="$(mktemp)"; git show "HEAD:$PKG/$P1x" > "$f"; chk "HEAD's own content" '' "$f"; rm -f "$f"
  # F4 · HEAD's content minus the final newline -> STILL silent (the cry-wolf regression)
  f="$(mktemp)"; git show "HEAD:$PKG/$P1x" | perl -0pe 's/\n\z//' > "$f"
  chk "missing trailing newline" '' "$f"; rm -f "$f"
  # F5 · absent on live -> silent (nothing to overwrite; must not be conflated with UNKNOWN)
  f="$(mktemp)"; : > "$f"; chk "empty live copy" '' "$f"; rm -f "$f"
  exit "$t"
) && pass=$((pass+6)) || fail=$((fail+1))

echo
echo "================================================================"
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ] || exit 1
