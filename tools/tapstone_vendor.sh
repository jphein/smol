#!/usr/bin/env bash
# tapstone_vendor.sh — keep rust/tapstone-rules honest about being a vendored copy.
#
# Sibling of tools/sigil_vendor.sh, and deliberately NOT a copy of it: the two vendor relationships
# have different shapes, and the differences are where the bugs would be. Read the header of that
# script first; this one documents only where it departs and why.
#
# ── WHAT IS AT STAKE HERE, WHICH IS MORE THAN WITH sigil-names ────────────────────────────────
# sigil-names drifting renames boards — visible, embarrassing, harmless. tapstone-rules drifting
# does something worse and quieter: Tapstone's two shrines never exchange game state, only tap
# events and an 8-byte chain head (decision 0004), and each shrine recomputes the state itself. One
# differing byte in the 134-byte canonical state image and two shrines compute different heads from
# identical taps. There is no compile error, no crash, no log line — just two boards that disagree
# about who won, at a table with no server to arbitrate. `src/` being byte-identical IS the
# protocol.
#
# ── THE THREE DEPARTURES FROM sigil_vendor.sh ─────────────────────────────────────────────────
#
# 1. **IT CHECKS A TAG, NOT A WORKING TREE.** sigil_vendor.sh diffs against whatever is in
#    ~/Projects/realm-sigil right now, because realm-sigil has no released versions and its HEAD is
#    the only thing to mean by "upstream". tapstone DOES pin: smol carries the engine at a named tag
#    (issue 10), and tapstone's `main` is EXPECTED to move ahead of it. So this script resolves
#    files with `git show <tag>:<path>` and never reads tapstone's checkout. That also makes it
#    immune to an in-flight edit in a sibling agent's working tree, which is not hypothetical —
#    tapstone's `src/state.rs` was being modified in another lane during this script's own first
#    run (`second_player_bonus` 1 → 0, its decision 0026). Diffing a working tree would have
#    reported that as smol's drift.
#
# 2. **"tapstone main has moved ahead" IS A NOTE, NOT A FAILURE.** The direct consequence of (1).
#    For sigil-names, divergence from upstream is always a defect. Here, divergence from upstream
#    `main` is the normal steady state of a pin, and a gate that failed on it would be red from the
#    day after the tag — which is how a check gets commented out. What FAILS is divergence from the
#    RECORDED TAG. What NOTES is the tag being behind `main`, because that is a re-vendor decision
#    for a human, not a defect in this tree.
#
# 3. **IT REPORTS THE PIN BEING BEHIND, which its sibling has no concept of.** Layer 3 says whether
#    tapstone's `main` has rules-engine commits past the tag. That is the question a human needs
#    answered ("is a re-vendor due?") and it has no answer in the sigil relationship, where there is
#    only ever "same" or "wrong".
#
# ── WHERE THE BEHAVIOURAL PROOF LIVES, WHICH IS NOT HERE ──────────────────────────────────────
# Bytes are the necessary condition, not the sufficient one: two identical sources can still
# disagree through a toolchain, a feature-unification or an `sha2` that resolved differently. The
# sufficient check is `rust/tapstone-rules`'s own `tests/vendor_golden_replay.rs`, which replays a
# real recorded match and demands the recorded chain heads. That is a `cargo test`, so it belongs in
# `tools/gate.sh` next to the other suites and NOT inside a fingerprint checker that CI needs to be
# able to run in a second. This script is deliberately cheap; the gate runs both arms.
#
# ── LAYERS, AND THEIR FAILURE PHILOSOPHY (which differs per layer, on purpose) ─────────────────
#   Layer 1  the committed fingerprint. FAILS CLOSED — a missing manifest is an error, because a
#            fingerprint that can be absent is not a fingerprint. Needs no sibling checkout, so CI
#            can always run it, so in-tree tampering is always caught.
#   Layer 1b the FILE LIST, not just the listed files. `sha256sum -c` verifies the files a manifest
#            names and is blind to one added beside them; an added `src/*.rs` is drift that would
#            compile, link and ship. sigil-names has this hole; this script closes it.
#   Layer 2  the tag diff. FAILS. Skips loudly if tapstone is absent (CI), like its sibling: an
#            unsatisfiable check gets disabled, and a disabled check is no check.
#   Layer 3  "is the pin behind upstream?" NEVER fails. Informational, for a human.
#
# MODES:
#   --check     (default) run the layers above.
#   --sync      re-vendor from tapstone at TAG and refresh the manifest. Needs the sibling repo.
#   --manifest  rewrite the manifest from what is in-tree. Only after a deliberate hand re-vendor.
#   --tag <t>   the tapstone tag to vendor/verify against. Default: the `# tag:` line in the
#               manifest, so the pin lives as DATA next to the crate and not as a constant in a
#               tool that also has other jobs.
#
# Exit 0 clean · 1 drift/tamper · 2 usage or missing prerequisite.
set -euo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &> /dev/null && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
CRATE="$REPO/rust/tapstone-rules"
MANIFEST="$CRATE/VENDOR.sha256"
TAPSTONE_DIR="${TAPSTONE_DIR:-$HOME/Projects/tapstone}"

# The vendored set, as PATHS RELATIVE TO THE CRATE ROOT — the one shape difference from
# sigil-names' manifest, which holds bare basenames because everything it vendors is in one
# directory. Here there are three (src/, tests/, testdata/), so the manifest is at the crate root
# and `sha256sum -c` runs from there. Same pattern, one level out.
#
# `SMOL_OWN_TESTS` is deliberately absent from the vendored set: those are smol's own tests, not
# upstream's, so they must NOT be required to match tapstone — but layer 1b still has to know they
# are expected, or every one of them would read as an unvouched-for added file.
SRC_FILES=(src/lib.rs src/cards.rs src/event.rs src/hash.rs src/rules.rs src/state.rs)
TEST_FILES=(tests/cards.rs tests/determinism.rs tests/event.rs tests/lobby.rs tests/rules.rs tests/state.rs)
SMOL_OWN_TESTS=(tests/vendor_golden_replay.rs tests/vendor_budget.rs)
# The frozen test vector. See the long note in tests/vendor_golden_replay.rs for why this one is
# pinned to a COMMIT rather than to the tag, and why it is not chased as tapstone's goldens move.
VECTOR="testdata/seed-1.json"
VECTOR_UPSTREAM="rust/tapstone-sim/golden/seed-1.json"

VENDORED=("${SRC_FILES[@]}" "${TEST_FILES[@]}" "$VECTOR")
MODE="check"
TAG=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check)    MODE="check"; shift ;;
    --sync)     MODE="sync"; shift ;;
    --manifest) MODE="manifest"; shift ;;
    --tag)      TAG="${2:?--tag needs a value}"; shift 2 ;;
    --tapstone-dir) TAPSTONE_DIR="${2:?--tapstone-dir needs a value}"; shift 2 ;;
    -h|--help)  sed -n '2,60p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

manifest_tag() {
  [[ -f "$MANIFEST" ]] || return 1
  sed -n 's/^# tag: *//p' "$MANIFEST" | head -1
}

# `sha256sum -c` cannot read a file with comments in it, so the pin rides a `#` line that is
# stripped on the way in. One file that carries both the fingerprint and what it is a fingerprint
# OF beats two files that can disagree.
sums() { grep -v '^#' "$MANIFEST"; }

have_tapstone() {
  [[ -d "$TAPSTONE_DIR/.git" ]] && git -C "$TAPSTONE_DIR" rev-parse --git-dir >/dev/null 2>&1
}

tag_exists() {
  git -C "$TAPSTONE_DIR" rev-parse --verify --quiet "refs/tags/$1" >/dev/null 2>&1
}

# Resolve one vendored path to its bytes AT THE TAG, on stdout. `git show` and not `cat`: see
# departure (1).
at_tag() {
  local tag="$1" rel="$2" upstream
  case "$rel" in
    "$VECTOR") upstream="$VECTOR_UPSTREAM" ;;
    *)         upstream="rust/tapstone-rules/$rel" ;;
  esac
  git -C "$TAPSTONE_DIR" show "$tag:$upstream" 2>/dev/null
}

write_manifest() {
  local tag="$1" commit="unknown"
  if have_tapstone && tag_exists "$tag"; then
    commit="$(git -C "$TAPSTONE_DIR" rev-parse "$tag^{commit}")"
  fi
  {
    echo "# Per-file SHA-256 of the VENDORED tapstone rules engine. Paths are relative to"
    echo "# rust/tapstone-rules/. Generated and verified by tools/tapstone_vendor.sh; do not"
    echo "# hand-edit, and do not edit the files it names — see that script's header."
    echo "# tag: $tag"
    echo "# commit: $commit"
    ( cd "$CRATE" && sha256sum "${VENDORED[@]}" )
  } > "$MANIFEST"
  echo "  wrote $(basename "$MANIFEST") (tag $tag, commit ${commit:0:7})"
}

case "$MODE" in
  manifest)
    TAG="${TAG:-$(manifest_tag || true)}"
    [[ -n "$TAG" ]] || { echo "FATAL: no tag given and none recorded in $MANIFEST" >&2; exit 2; }
    write_manifest "$TAG"
    ;;

  sync)
    have_tapstone || {
      echo "FATAL: $TAPSTONE_DIR is not a git checkout — cannot re-vendor." >&2
      echo "       Set TAPSTONE_DIR or pass --tapstone-dir <path>." >&2
      exit 2
    }
    TAG="${TAG:-$(manifest_tag || true)}"
    [[ -n "$TAG" ]] || { echo "FATAL: no tag given and none recorded — pass --tag <tag>." >&2; exit 2; }
    tag_exists "$TAG" || { echo "FATAL: tag '$TAG' does not exist in $TAPSTONE_DIR." >&2; exit 2; }
    for f in "${SRC_FILES[@]}" "${TEST_FILES[@]}"; do
      mkdir -p "$CRATE/$(dirname "$f")"
      at_tag "$TAG" "$f" > "$CRATE/$f" || { echo "FATAL: $f not found at $TAG" >&2; exit 2; }
      echo "  vendored $f"
    done
    echo "  NOT re-fetched: $VECTOR — it is a frozen test vector, not a mirror. Replace it only"
    echo "  deliberately (see tests/vendor_golden_replay.rs), then re-run --manifest."
    write_manifest "$TAG"
    echo
    echo "Re-vendored from $TAPSTONE_DIR at $TAG. READ THE DIFF before committing: a rules change"
    echo "alters what every recorded match hashes to, so a stale shrine and a fresh one will"
    echo "disagree silently rather than refuse to pair. Then run the vendored crate's own tests —"
    echo "  cd rust/tapstone-rules && cargo test"
    echo "— because the frozen vector was produced by the OLD engine and is exactly the thing that"
    echo "should go red if the new one is not equivalent."
    ;;

  check)
    rc=0

    # ── Layer 1: the fingerprint. Always runnable; fails closed. ──────────────────────────────
    if [[ ! -f "$MANIFEST" ]]; then
      echo "FATAL: $MANIFEST is missing. A vendored copy with no fingerprint cannot be checked at" >&2
      echo "       all — the condition that let smol's sigil corpora drift for three months." >&2
      echo "       Regenerate with: tools/tapstone_vendor.sh --manifest --tag <tag>" >&2
      exit 1
    fi
    TAG="${TAG:-$(manifest_tag || true)}"
    if [[ -z "$TAG" ]]; then
      echo "FATAL: $MANIFEST records no '# tag:' line, so there is nothing to verify AGAINST." >&2
      exit 1
    fi
    if ( cd "$CRATE" && sums | sha256sum --quiet -c - ); then
      echo "ok: vendored sources match the committed fingerprint (tag $TAG)"
    else
      echo "DRIFT: rust/tapstone-rules has been EDITED IN TREE." >&2
      echo "       src/ and tests/ are verbatim copies of tapstone at $TAG. Make the change" >&2
      echo "       THERE, tag it there, then re-vendor: tools/tapstone_vendor.sh --sync --tag <t>." >&2
      echo "       A fix applied here is a fix the arena service and the sim never get, and the" >&2
      echo "       hash chain is only a protocol while both engines are the same engine." >&2
      rc=1
    fi

    # ── Layer 1b: the file LIST. Closes the added-file hole `sha256sum -c` leaves open. ───────
    listed=$(printf '%s\n' "${VENDORED[@]}" "${SMOL_OWN_TESTS[@]}" | sort)
    actual=$(cd "$CRATE" && find src tests testdata -type f | sort)
    if [[ "$listed" == "$actual" ]]; then
      echo "ok: no unlisted files beside the vendored set"
    else
      echo "DRIFT: the file LIST under rust/tapstone-rules/ is not what the vendor declares." >&2
      diff <(echo "$listed") <(echo "$actual") | sed 's/^/       /' >&2
      echo "       '>' is a file in the tree that nothing vouches for; '<' is one that vanished." >&2
      echo "       A file ADDED next to the manifest's entries is invisible to sha256sum -c and" >&2
      echo "       would compile, link and ship — hence this arm." >&2
      rc=1
    fi

    # ── Layer 2: the tag diff. Fails; skips loudly when tapstone is absent. ──────────────────
    if have_tapstone && tag_exists "$TAG"; then
      drift=0
      for f in "${SRC_FILES[@]}" "${TEST_FILES[@]}"; do
        if ! at_tag "$TAG" "$f" | diff -q - "$CRATE/$f" >/dev/null 2>&1; then
          echo "DRIFT: $f differs from tapstone $TAG" >&2
          at_tag "$TAG" "$f" | diff -u - "$CRATE/$f" | head -40 >&2
          drift=1
        fi
      done
      if [[ $drift -eq 0 ]]; then
        echo "ok: vendored sources match tapstone at $TAG"
      else
        echo "       Re-vendor with: tools/tapstone_vendor.sh --sync --tag $TAG" >&2
        rc=1
      fi

      # ── Layer 3: has upstream moved past the pin? A NOTE. See departures (2) and (3). ──────
      tagged=$(git -C "$TAPSTONE_DIR" rev-parse "$TAG^{commit}")
      if git -C "$TAPSTONE_DIR" rev-parse --verify --quiet main >/dev/null 2>&1; then
        behind=$(git -C "$TAPSTONE_DIR" rev-list --count "$tagged..main" -- rust/tapstone-rules/src 2>/dev/null || echo 0)
        if [[ "$behind" -gt 0 ]]; then
          echo "note: tapstone main is $behind commit(s) ahead of $TAG under tapstone-rules/src."
          echo "      NOT a failure — smol pins a tag on purpose and upstream is meant to move."
          echo "      It is a re-vendor DECISION for a human: read the diff, tag tapstone, --sync."
        else
          echo "ok: tapstone main has no rules-engine commits past $TAG"
        fi
      fi
    else
      if ! have_tapstone; then
        echo "note: $TAPSTONE_DIR absent — tag diff SKIPPED (fingerprint still verified)."
        echo "      Expected in CI. In-tree tampering is caught above; a change made upstream is"
        echo "      only caught on a machine with tapstone checked out."
      else
        # A FAIL, not a note — and the distinction is the same one sigil_vendor.sh's #280 arm
        # makes between "the toolchain is absent" and "the config is stale". tapstone being
        # absent is an unavailable instrument: not this tree's fault, so it skips. tapstone
        # being PRESENT while the tag this manifest pins does not exist in it is a DEFECT: the
        # pin names nothing, so layer 2 can never run again and the loudest guarantee in this
        # script quietly becomes prose. Found by running the script's own controls — with this
        # as a note, a garbage tag produced exit 0 and three cheerful `ok:` lines.
        echo "FATAL: tag '$TAG' is recorded in $(basename "$MANIFEST") but does not exist in" >&2
        echo "       $TAPSTONE_DIR. The pin names nothing, so the tag diff can never run and" >&2
        echo "       the byte-identical guarantee is unenforceable. Either the tag was never" >&2
        echo "       created, or it was renamed/deleted upstream." >&2
        echo "       Fix the pin: tools/tapstone_vendor.sh --sync --tag <a tag that exists>" >&2
        rc=1
      fi
    fi

    exit "$rc"
    ;;
esac
