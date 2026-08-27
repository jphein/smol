#!/usr/bin/env bash
# check_lock_fresh.sh — #460. Is the committed Cargo.lock actually consulted?
#
# THE DEFECT THIS EXISTS FOR: `rust/clock/Cargo.toml` pins `mipidsi = "=0.10.0"` — the strictest pin
# cargo has, placed deliberately because s3_oled's ORIENTATION reasoning is tied to 0.10's MADCTL
# semantics. On 2026-08-26 the committed lock did not contain `mipidsi` at all, and no build path in
# the repo passed `--locked`, so every build silently REWROTE the lock and no gate noticed. An exact
# pin enforced only by whoever last read the comment above it.
#
# WHY A GATE ARM RATHER THAN `--locked` ON THE BUILD PATHS (#460's option 2, deliberately): adding
# `--locked` to `repro_build.sh`/`gate.sh`/`fw-gate.yml` changes how everyone builds and collides
# with whichever lane holds those files. This arm catches the same staleness without touching a
# single build invocation. If `--locked` is later adopted on the build paths, this arm becomes
# redundant and should be deleted rather than kept as a second statement of one fact.
#
# ── THE INSTRUMENT, AND WHY IT IS SHAPED LIKE THIS ────────────────────────────────────────────────
#
# `cargo metadata --locked` resolves the FULL dependency graph (optional deps included — which is
# the whole point, since #460's two missing entries were both optional) and refuses to rewrite the
# lock. Fresh lock ⇒ exit 0. Stale lock ⇒ exit 101.
#
# ⚠️ `--offline` is NOT used, and that is measured, not stylistic. All six combinations were run:
#
#   lock      registry cache   flags                 rc    verdict
#   healthy   warm             --locked --offline     0    fresh
#   STALE     warm             --locked --offline   101    stale
#   healthy   COLD             --locked --offline   101    *** FALSE STALE ***
#   healthy   COLD             --locked               0    fresh
#   STALE     COLD             --locked             101    stale
#   (cargo absent)                                   127   cannot check
#
# A cold registry cache plus `--offline` fails on a PERFECTLY FRESH lock ("no matching package named
# `ed25519-compact` found … offline mode can sometimes cause surprising resolution failures"). On a
# fresh CI runner that is the normal state, so an `--offline` arm would have been red for a reason
# that has nothing to do with the lock — a check that fails for the wrong reason, which is worse
# than no check because it teaches people to ignore it (#338).
#
# ⚠️ Therefore exit 101 ALONE IS NOT THE FINDING. This script requires cargo's own discriminating
# sentence, `because --locked was passed`, before it will say STALE. That sentence was verified
# present in BOTH stale cases above (warm and cold cache) and absent in the false-stale case. Same
# discipline as the #420 gate arm, which refuses to read an unrelated build failure as proof the
# guard fired.
#
# THREE OUTCOMES, not two:
#   0  the lock is fresh — cargo resolved the graph without needing to change it
#   1  STALE LOCK — the finding
#   2  COULD NOT CHECK — cargo absent, network unreachable, cold cache under --offline, bad cwd.
#      NEVER reported as a pass and never reported as staleness. (#280's manifest-gap precedent.)
#
# SCOPE: exit-code-bearing for `rust/clock` only. Every other Cargo.lock in the repo is DISCOVERED
# (not hardcoded — a hand-kept list is what #328 watched rot) and reported informationally. Reason
# for the split, so it reads as a decision: `targets/*` are other lanes' actively-moving trees, and
# nothing in-repo states their locks must be fresh. Failing on them would be inventing another
# lane's requirements (#338). Reporting them means the limit of this gate is VISIBLE rather than
# something a reader has to infer from its absence (#430: make the drop visible).
#
# Usage:
#   tools/check_lock_fresh.sh              # check rust/clock (exit 0/1/2) + report other locks
#   tools/check_lock_fresh.sh --self-test  # prove this checker can fail, and cannot falsely fail

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CANON_REL="rust/clock"
DISCRIMINATOR="because --locked was passed"

# ── the one measurement ───────────────────────────────────────────────────────────────────────────
# Echoes a verdict word on stdout and returns 0/1/2. `err` receives cargo's stderr.
probe_lock() { # <crate-dir>
  local dir="$1" err rc
  [ -d "$dir" ]            || { echo "CANNOT-CHECK no such directory: $dir"; return 2; }
  [ -f "$dir/Cargo.toml" ] || { echo "CANNOT-CHECK no Cargo.toml in $dir"; return 2; }
  [ -f "$dir/Cargo.lock" ] || { echo "CANNOT-CHECK no Cargo.lock in $dir (nothing to enforce)"; return 2; }
  command -v cargo >/dev/null 2>&1 || { echo "CANNOT-CHECK cargo is not on PATH"; return 2; }

  err="$(cd "$dir" && cargo metadata --locked --format-version 1 2>&1 >/dev/null)"; rc=$?
  if [ "$rc" -eq 0 ]; then echo "FRESH"; return 0; fi
  # Non-zero: only cargo's own sentence licenses the STALE verdict.
  case "$err" in
    *"$DISCRIMINATOR"*) echo "STALE"; printf '%s\n' "$err" | sed 's/^/      /' >&2; return 1 ;;
    *) echo "CANNOT-CHECK cargo metadata failed for an unrelated reason (rc=$rc)"
       printf '%s\n' "$err" | sed 's/^/      /' >&2; return 2 ;;
  esac
}

# ── self-test ─────────────────────────────────────────────────────────────────────────────────────
# Built from the REAL 2026-08-26 drift (the two optional deps #460 measured absent), reconstructed
# from a copy of the real lock — not a hand-written miniature, which would drift from the thing being
# guarded and reproduce this issue's own defect.
self_test() {
  local pass=0 fail=0
  # WORK is deliberately GLOBAL: an EXIT trap fires after this function returns, so a `local` here
  # gives "work: unbound variable" at cleanup time — a real bug this self-test hit on first run.
  WORK="$(mktemp -d /var/tmp/lockfresh-selftest.XXXXXX)"   # never /tmp: 16 GB tmpfs on this host
  trap 'rm -rf "${WORK:-}"' EXIT INT TERM
  local work="$WORK"

  # ⚠️ Copy the whole `rust/` tree, not just `rust/clock`. `Cargo.toml` carries two PATH deps —
  # `sigil-names` (:87) and `esp-wifi-sys-chip` (:220) — and a lone clock/ copy makes cargo fail at
  # manifest load with its own exit 101. First run of this self-test did exactly that, and the probe
  # correctly answered CANNOT-CHECK rather than STALE: an unrelated 101 must never be read as the
  # finding. Recorded because that near-miss is the arm's whole justification. (Those same two path
  # deps are #327's cause, which is why they are worth knowing about here.)
  # `target/` is excluded DURING the copy, not deleted after. A `cp -r` followed by `rm -rf target`
  # gives the same end state and copies the build output first: on a fresh worktree rust/ is 4 MB and
  # nobody notices, but on a warm CI runner or a developer's tree target/ is gigabytes, so the arm
  # would cost minutes and disk for a 0.1 s measurement. Sequence matters even when the result does
  # not. (tar rather than rsync: no extra dependency.)
  mkdir -p "$work/rust"
  tar -cf - --exclude=target -C "$ROOT/rust" . | tar -xf - -C "$work/rust"
  local C="$work/rust/clock"
  cp "$C/Cargo.lock" "$work/lock.healthy"

  t() { # <name> <expected-rc> <actual-rc> <verdict-word>
    if [ "$2" = "$3" ]; then printf '   PASS %-52s rc=%s %s\n' "$1" "$3" "$4"; pass=$((pass+1))
    else printf '   FAIL %-52s want rc=%s got rc=%s %s\n' "$1" "$2" "$3" "$4"; fail=$((fail+1)); fi
  }

  local v rc
  # 1. HEALTHY must pass. A checker that refuses everything "can fail" and is useless.
  v="$(probe_lock "$C" 2>/dev/null)"; rc=$?
  t "healthy lock -> FRESH" 0 "$rc" "$v"

  # 2. THE REAL DRIFT: remove exactly the two entries #460 found absent.
  python3 - "$C/Cargo.lock" <<'PY'
import re, sys, pathlib
p = pathlib.Path(sys.argv[1]); blocks = p.read_text().split('\n[[package]]\n')
keep = [blocks[0]]
for b in blocks[1:]:
    m = re.match(r'name = "([^"]+)"', b)
    if m and m.group(1) in ('mipidsi', 'embedded-hal-bus'):
        continue
    keep.append(b)
p.write_text('\n[[package]]\n'.join(keep))
PY
  v="$(probe_lock "$C" 2>/dev/null)"; rc=$?
  t "the real #460 drift -> STALE" 1 "$rc" "$v"

  # 3. Restored. Proves arm 2 measured the edit and not a broken rig.
  cp "$work/lock.healthy" "$C/Cargo.lock"
  v="$(probe_lock "$C" 2>/dev/null)"; rc=$?
  t "restored -> FRESH again" 0 "$rc" "$v"

  # 4. THE KEEPER. A healthy lock under --offline with a COLD registry cache exits 101 — the arm
  #    that would have made this gate permanently red on fresh CI runners had --offline been used.
  #    Asserted as a property of the FLAGS, so nobody "simplifies" --locked into --locked --offline:
  #    it would not fail loudly, it would fail on every clean checkout for the wrong reason.
  local empty="$work/empty-cargo-home"; mkdir -p "$empty"
  ( cd "$C" && CARGO_HOME="$empty" cargo metadata --locked --offline --format-version 1 ) >/dev/null 2>"$work/offline.err"; rc=$?
  if [ "$rc" -ne 0 ] && ! grep -qF -- "$DISCRIMINATOR" "$work/offline.err"; then
    printf '   PASS %-52s rc=%s (no discriminator -> our probe would say CANNOT-CHECK, not STALE)\n' \
      "--offline + cold cache is a FALSE stale" "$rc"; pass=$((pass+1))
  elif [ "$rc" -eq 0 ]; then
    printf '   SKIP %-52s cold-cache arm inconclusive: CARGO_HOME override did not cool the cache\n' \
      "--offline + cold cache is a FALSE stale"
  else
    printf '   FAIL %-52s the discriminator appeared on a HEALTHY lock — it cannot discriminate\n' \
      "--offline + cold cache is a FALSE stale"; fail=$((fail+1))
  fi

  # 4b. THE DISCRIMINATOR ITSELF, exercised THROUGH probe_lock. A crate copied WITHOUT its sibling
  #     path deps makes `cargo metadata` exit 101 with no `--locked` sentence — a healthy lock and an
  #     unrelated failure. Must be CANNOT-CHECK (2), never STALE (1).
  #
  #     This arm exists because sabotaging the `case` to read ANY non-zero as STALE was caught by
  #     NOTHING ELSE in this suite: arms 1/3 are exit-0 paths and arms 5-8 return before cargo runs,
  #     so the single most important design decision in this file was untested. The fixture is the
  #     near-miss from this self-test's own first run, kept rather than discarded.
  mkdir -p "$work/orphan"
  cp -r "$C" "$work/orphan/clock"
  rm -rf "$work/orphan/clock/target"
  v="$(probe_lock "$work/orphan/clock" 2>/dev/null)"; rc=$?
  t "unrelated cargo 101 -> CANNOT-CHECK not STALE" 2 "$rc" "$v"

  # 5. cargo absent must be CANNOT-CHECK (2), never a pass and never STALE.
  v="$(PATH=/nonexistent-dir probe_lock "$C" 2>/dev/null)"; rc=$?
  t "cargo absent -> CANNOT-CHECK" 2 "$rc" "$v"

  # 6-8. Vacuous-pass guards: each of these would otherwise report green while guarding nothing.
  v="$(probe_lock "$work/does-not-exist" 2>/dev/null)"; rc=$?
  t "missing directory -> CANNOT-CHECK" 2 "$rc" "$v"
  mkdir -p "$work/nolock"; cp "$C/Cargo.toml" "$work/nolock/"
  v="$(probe_lock "$work/nolock" 2>/dev/null)"; rc=$?
  t "Cargo.toml but no Cargo.lock -> CANNOT-CHECK" 2 "$rc" "$v"
  mkdir -p "$work/notacrate"
  v="$(probe_lock "$work/notacrate" 2>/dev/null)"; rc=$?
  t "not a crate at all -> CANNOT-CHECK" 2 "$rc" "$v"

  # 9. The discovery must SCAN, not carry a list — a hand-kept list is what rots (#328).
  local found; found="$(discover_locks | wc -l)"
  if [ "$found" -ge 2 ]; then
    printf '   PASS %-52s %s locks discovered, source=%s\n' "lock discovery is a scan, not a list" "$found" "$(lock_source)"; pass=$((pass+1))
  else
    printf '   FAIL %-52s found %s (a scan that finds ~nothing passes vacuously)\n' \
      "lock discovery is a scan, not a list" "$found"; fail=$((fail+1))
  fi

  # 10. The two discovery paths must be DISTINGUISHED, because they give different answers: a build
  #     mirror has no .git, the `find` fallback then runs, and it cannot tell a tracked lock from a
  #     gitignored generated one (`experiments/*/Cargo.lock`). Caught on familiar, where the report
  #     silently listed three build products as repo locks.
  #
  #     ⚠️ Asserted against TWO CONSTRUCTED FIXTURES, not against the ambient tree. The first version
  #     asserted "this repo reports tracked", which is a fact about the ENVIRONMENT rather than about
  #     this code — and it duly failed on familiar, where the rsync'd mirror legitimately has no .git.
  #     That is precisely the failure mode this file's header is about (a check that goes red for a
  #     reason unrelated to what it measures), committed one level inside the fix for it. The property
  #     wanted is "git present => tracked, git absent => scan", and fixtures can state that anywhere.
  local ft="$work/fixture-tracked" fs="$work/fixture-scan" got_t got_s
  mkdir -p "$ft" "$fs"
  : > "$ft/Cargo.lock"; : > "$fs/Cargo.lock"
  ( cd "$ft" && git init -q . && git add Cargo.lock ) >/dev/null 2>&1
  got_t="$( ROOT="$ft"; lock_source )"
  got_s="$( ROOT="$fs"; lock_source )"
  if [ "$got_t" = tracked ] && [ "$got_s" = scan ]; then
    printf '   PASS %-52s git-present=tracked, git-absent=scan\n' "discovery labels which path it took"; pass=$((pass+1))
  else
    printf '   FAIL %-52s git-present=%s git-absent=%s (an unlabelled fallback reports build output as repo locks)\n' \
      "discovery labels which path it took" "$got_t" "$got_s"; fail=$((fail+1))
  fi

  printf '\n   %s ok, %s failed\n' "$pass" "$fail"
  [ "$fail" -eq 0 ]
}

# Every Cargo.lock the repo TRACKS, discovered rather than listed (a hand-kept list is what #328
# watched rot). `git ls-files` is the authority; DISCOVER_SOURCE records which path was taken.
#
# ⚠️ The two paths give DIFFERENT ANSWERS and the fallback's is not a subset — found the hard way.
# In a rsync'd build mirror there is no `.git`, so the fallback ran, and it reported three extra
# entries (`experiments/mac_verify`, `ota_http_verify`, `etx_verify`) that the git path never shows.
# Those are **gitignored generated artifacts** (`.gitignore:76 experiments/*/Cargo.lock`) written by a
# previous host-verifier run; they do not exist in a clean checkout at all. A bare `find` cannot tell
# a tracked lock from a build product, so the fallback's list is labelled rather than presented as
# equivalent. The gated check itself is a fixed path and is unaffected either way — only this
# informational list could mislead, and an informational list that quietly counts build output is
# still the wrong number in front of a reader.
# ⚠️ The source is reported by its OWN function, not by a variable discover_locks sets. Every caller
# uses `discover_locks | …` or `$(discover_locks)`, and a pipeline or command substitution runs the
# function in a SUBSHELL — so an assignment inside it never reaches the caller. The first draft did
# exactly that: `DISCOVER_SOURCE` was always empty at the call site, so the main path would have
# printed the "filesystem scan, may include generated locks" warning on EVERY run including in a
# normal git checkout. Caught because the self-test prints the value. Same family as the
# pipelines-launder-exit-codes trap: a pipeline launders variable assignments too.
lock_source() {
  if ( cd "$ROOT" && git ls-files '*Cargo.lock' 2>/dev/null | grep -q . ); then
    echo tracked
  else
    echo scan
  fi
}

discover_locks() {
  if [ "$(lock_source)" = tracked ]; then
    ( cd "$ROOT" && git ls-files '*Cargo.lock' | sed 's#/Cargo.lock$##' )
  else
    ( cd "$ROOT" && find . -name Cargo.lock -not -path '*/target/*' -printf '%h\n' | sed 's#^\./##' )
  fi
}

# ── main ──────────────────────────────────────────────────────────────────────────────────────────
if [ "${1:-}" = "--self-test" ]; then
  echo "check_lock_fresh --self-test (proving both directions, and that it cannot falsely fail)"
  self_test; exit $?
fi

verdict="$(probe_lock "$ROOT/$CANON_REL")"; rc=$?
case "$rc" in
  0) echo "ok  $CANON_REL/Cargo.lock is FRESH — cargo resolved the graph without rewriting it" ;;
  1) echo "STALE LOCK: $CANON_REL/Cargo.lock does not match Cargo.toml (#460)."
     echo "  A build would silently rewrite it, so any exact '=' pin in Cargo.toml is unenforced"
     echo "  and two hosts can resolve different graphs from the same commit (#44/#326)."
     echo "  Fix: cd $CANON_REL && cargo metadata >/dev/null   # resolve, then COMMIT the lock" ;;
  2) echo "COULD NOT CHECK: $verdict"
     echo "  Deliberately not reported as a pass and not as staleness — see this file's header." ;;
esac

# Informational: the locks this arm does NOT gate, named so the limit is visible.
others="$(discover_locks | grep -v "^$CANON_REL\$" || true)"
if [ -n "$others" ]; then
  case "$(lock_source)" in
    tracked) echo "  not gated by this arm (other lanes' trees; reported so the scope is visible):" ;;
    *)       echo "  not gated by this arm — FILESYSTEM SCAN (no .git here, e.g. a build mirror), so"
             echo "  this list may include gitignored generated locks such as experiments/*:" ;;
  esac
  while IFS= read -r d; do [ -n "$d" ] && echo "    $d"; done <<<"$others"
fi
exit "$rc"
