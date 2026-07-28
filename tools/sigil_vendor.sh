#!/usr/bin/env bash
# sigil_vendor.sh — keep rust/sigil-names/src/ honest about being a vendored copy.
#
# smol is a PUBLIC repo, so it cannot `path`-depend on ~/Projects/realm-sigil (builds on one
# machine) and cannot `git`-depend until that repo is pushed (JP's call). So the Rust sigil binding
# is VENDORED — and a vendored copy is exactly what caused the problem this whole exercise fixed:
# smol hand-copied the word corpus into two crates and they drifted from upstream for three months
# with nothing to notice. A copy without a checker is a future bug with a delay fuse.
#
# Hence two layers, because they catch different things:
#
#   --check       (default) Verify the committed sha256 manifest. Detects IN-TREE tampering and
#                 needs no sibling checkout, so CI can always run it. Then, IF realm-sigil is
#                 present, also diff against it to detect UPSTREAM drift.
#   --sync        Re-vendor from realm-sigil and refresh the manifest. Requires the sibling repo.
#   --manifest    Rewrite the manifest from what is currently in-tree. Use only when you have
#                 deliberately re-vendored by hand.
#
# Exit 0 clean · 1 drift/tamper · 2 usage or missing prerequisite.
#
# NOTE the failure philosophy, which differs per layer on purpose: the manifest check FAILS CLOSED
# (a missing manifest is an error — that is the whole point of a fingerprint). The upstream diff
# SKIPS with a loud notice when realm-sigil is absent, because failing a public CI run for not
# having a private sibling checkout would be a check nobody could satisfy, and an unsatisfiable
# check gets disabled, which is how you end up with no check at all.
set -euo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &> /dev/null && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
VENDOR="$REPO/rust/sigil-names/src"
MANIFEST="$REPO/rust/sigil-names/VENDOR.sha256"
SIGIL_DIR="${SIGIL_DIR:-$HOME/Projects/realm-sigil}"
FILES=(lib.rs realms.rs reserved.rs)
MODE="check"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check)    MODE="check"; shift ;;
    --sync)     MODE="sync"; shift ;;
    --manifest) MODE="manifest"; shift ;;
    --sigil-dir) SIGIL_DIR="$2"; shift 2 ;;
    -h|--help)  sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

write_manifest() {
  ( cd "$VENDOR" && sha256sum "${FILES[@]}" ) > "$MANIFEST"
  echo "  wrote $(basename "$MANIFEST")"
}

case "$MODE" in
  manifest)
    write_manifest
    ;;

  sync)
    [[ -d "$SIGIL_DIR/rust/src" ]] || {
      echo "FATAL: $SIGIL_DIR/rust/src not found — cannot re-vendor." >&2
      echo "       Set SIGIL_DIR or pass --sigil-dir <path>." >&2
      exit 2
    }
    for f in "${FILES[@]}"; do
      cp "$SIGIL_DIR/rust/src/$f" "$VENDOR/$f"
      echo "  vendored $f"
    done
    write_manifest
    echo "Re-vendored from $SIGIL_DIR. Review the diff before committing — a corpus change RENAMES"
    echo "every board, and the compile-time assertions in lib.rs are what stop a broken one landing."
    ;;

  check)
    rc=0

    # Layer 1 — the fingerprint. Always runnable; fails closed.
    if [[ ! -f "$MANIFEST" ]]; then
      echo "FATAL: $MANIFEST is missing. A vendored copy with no fingerprint cannot be checked at" >&2
      echo "       all, which is the exact condition that let smol's corpora drift for three" >&2
      echo "       months. Regenerate with: tools/sigil_vendor.sh --manifest" >&2
      exit 1
    fi
    if ( cd "$VENDOR" && sha256sum --quiet -c "$MANIFEST" ); then
      echo "ok: vendored sources match the committed fingerprint"
    else
      echo "DRIFT: rust/sigil-names/src/ has been EDITED IN TREE." >&2
      echo "       That directory is a verbatim copy of realm-sigil's rust/src/. Make the change" >&2
      echo "       upstream, regenerate there (./sync-words.sh --only rust), then re-vendor with" >&2
      echo "       tools/sigil_vendor.sh --sync." >&2
      rc=1
    fi

    # Layer 2 — upstream drift. Skips loudly when the sibling repo is absent.
    if [[ -d "$SIGIL_DIR/rust/src" ]]; then
      drift=0
      for f in "${FILES[@]}"; do
        if ! diff -q "$SIGIL_DIR/rust/src/$f" "$VENDOR/$f" >/dev/null 2>&1; then
          echo "DRIFT: $f differs from $SIGIL_DIR/rust/src/$f" >&2
          diff -u "$SIGIL_DIR/rust/src/$f" "$VENDOR/$f" | head -40 >&2
          drift=1
        fi
      done
      if [[ $drift -eq 0 ]]; then
        echo "ok: vendored sources match $SIGIL_DIR"
      else
        echo "       Re-vendor with: tools/sigil_vendor.sh --sync" >&2
        echo "       ⚠️ A corpus change renames EVERY board. Read the diff before syncing." >&2
        rc=1
      fi
    else
      echo "note: $SIGIL_DIR not present — upstream-drift check SKIPPED (fingerprint still verified)."
      echo "      This is expected in CI. It means in-tree tampering is caught here, but a change"
      echo "      made upstream is only caught on a machine that has realm-sigil checked out."
    fi

    exit "$rc"
    ;;
esac
