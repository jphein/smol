#!/usr/bin/env bash
# tapstone_vendor.sh — keep smol's vendored tapstone crates honest about being vendored copies.
#
# Five sources, one pin: rust/tapstone-rules (the engine), rust/tapstone-proto (MATCH frames, the
# follower, the shrine seat), rust/tapstone-progression (commander XP/slots/items),
# rust/shrine-render (the shrine's screens) and rust/tapstone-decks (the deck lists). All five
# are copied from ONE tag of the PUBLIC repo github.com/jphein/tapstone-game, never from the
# private jphein/tapstone (see "THE SOURCE" below).
#
# Sibling of tools/sigil_vendor.sh, and deliberately NOT a copy of it: the two vendor relationships
# have different shapes, and the differences are where the bugs would be. Read the header of that
# script first; this one documents only where it departs and why.
#
# ── WHAT IS AT STAKE HERE, WHICH IS MORE THAN WITH sigil-names ────────────────────────────────
# sigil-names drifting renames boards — visible, embarrassing, harmless. tapstone-rules drifting
# does something worse and quieter: Tapstone's shrines never exchange game state, only tap events
# and an 8-byte chain head (decision 0004), and each shrine recomputes the state itself. One
# differing byte in the canonical state image and two shrines compute different heads from
# identical taps. There is no compile error, no crash, no log line — just two boards that disagree
# about who won, at a table with no server to arbitrate. The same holds for tapstone-proto's frame
# codec against the arena. `src/` being byte-identical IS the protocol.
#
# ── THE SOURCE: jphein/tapstone-game, and ONLY it ─────────────────────────────────────────────
# smol is public. The game's development repo (jphein/tapstone) is private; jphein/tapstone-game is
# its scrubbed public snapshot, and tags cut there are the only thing smol may vendor. A checkout
# whose `origin` is anything else is REFUSED, not skipped — including the private repo, which has
# the same paths and would diff clean. Vendoring from it would publish whatever the scrub removed.
# (Found 2026-09-27: the first vendor, from the private repo, had put its sources in public smol.)
#
# ── THE THREE DEPARTURES FROM sigil_vendor.sh ─────────────────────────────────────────────────
#
# 1. **IT CHECKS A TAG, NOT A WORKING TREE.** Files are resolved with `git show <tag>:<path>`, never
#    from a checkout, so an in-flight edit in a sibling agent's tree cannot read as smol's drift
#    (not hypothetical: it happened during this script's first run).
#
# 2. **"upstream main has moved ahead" IS A NOTE, NOT A FAILURE.** Divergence from `main` is the
#    normal steady state of a pin; a gate that failed on it would be red the day after the tag, which
#    is how a check gets commented out. What FAILS is divergence from the RECORDED TAG.
#
# 3. **IT REPORTS THE PIN BEING BEHIND**, which its sibling has no concept of: "is a re-vendor due?"
#
# ── WHERE THE BEHAVIOURAL PROOF LIVES, WHICH IS NOT HERE ──────────────────────────────────────
# Bytes are the necessary condition, not the sufficient one. The sufficient check is the crates'
# own `cargo test` suites (rules' `tests/vendor_golden_replay.rs` replays a recorded match and
# demands its chain heads), which tools/gate.sh runs. This script is deliberately cheap.
#
# ── LAYERS, AND THEIR FAILURE PHILOSOPHY (which differs per layer, on purpose) ─────────────────
#   Layer 1  the committed fingerprint, per crate. FAILS CLOSED — a missing manifest is an error.
#            Needs no upstream checkout, so in-tree tampering is always caught.
#   Layer 1b the FILE LIST. `sha256sum -c` is blind to a file added beside the ones it names; an
#            added `src/*.rs` would compile, link and ship. The manifest IS the list.
#   Layer 2  the tag diff. FAILS. Also fails when upstream has a `src/` file smol lacks, or an
#            upstream test that is neither vendored nor declared excluded (EXCLUDED_TESTS below).
#            Without an upstream checkout it SKIPS loudly — except in CI (CI=true) or with --fetch,
#            where it fetches tapstone-game (public, so no credential) and runs for real.
#   Layer 3  "is the pin behind upstream main?" NEVER fails. Informational, for a human.
#
# MODES:
#   --check        (default) run the layers above, for every crate.
#   --sync         re-vendor every crate from tapstone-game at TAG and refresh the manifests.
#   --manifest     rewrite the manifests from what is in-tree. Only after a deliberate hand re-vendor.
#   --tag <t>      the tapstone-game TAG to vendor/verify against. Default: each manifest's `# tag:`.
#   --rev <sha>    --sync only: a DRY RUN from an untagged commit. The manifests record
#                  `tag: UNTAGGED`, which --check refuses, so a dry run can never ship green.
#   --skips <c>    print crate c's declared test-function skips, one arg per line (for tools/gate.sh).
#   --fetch        fetch tapstone-game into a temp dir when TAPSTONE_DIR is absent (implied by CI=true).
#   --tapstone-dir <p>  a tapstone-game checkout (default $TAPSTONE_DIR, else ~/Projects/tapstone-game).
#
# Exit 0 clean · 1 drift/tamper/wrong source · 2 usage or missing prerequisite.
set -euo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &> /dev/null && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
TAPSTONE_DIR="${TAPSTONE_DIR:-$HOME/Projects/tapstone-game}"
UPSTREAM_URL="https://github.com/jphein/tapstone-game"
# The one origin accepted: https or ssh, with or without `.git`. The private repo
# (jphein/tapstone) must NOT match, which is why the name is anchored at both ends.
ORIGIN_RE='github\.com[:/]jphein/tapstone-game(\.git)?/?$'
UNTAGGED="UNTAGGED"

# Five sources, one pin. Four crates, plus the deck lists the station's build.rs derives its tables
# from (upstream `decks/*.toml`, vendored to rust/tapstone-decks/decks/).
CRATES=(tapstone-rules tapstone-proto tapstone-progression shrine-render tapstone-decks)

# Per-crate shape. `src/` is always vendored WHOLE (derived from the tag at --sync, and a src file
# upstream has that smol lacks is drift). Tests are vendored except the ones declared here, each
# with the reason it cannot run inside smol. SMOL_OWN files are smol's, not upstream's: they are
# fingerprinted (so they cannot be edited unnoticed) but never compared with the tag. MAPPED files
# are vendored from an upstream path outside the crate (`smol/path=upstream/path`), and are
# fetched and tag-diffed like the rest.
crate_cfg() {
  EXCLUDED_TESTS=(); SMOL_OWN=(); MAPPED=(); SKIPPED_FNS=()
  # UP_PREFIX: the upstream path that maps to rust/<name>/ ("" = the repo root). VDIRS: the
  # directories vendored WHOLE, under both. Defaults fit a crate.
  UP_PREFIX="rust/$1"; VDIRS=(src tests)
  case "$1" in
    tapstone-rules)
      SMOL_OWN=(tests/vendor_golden_replay.rs tests/vendor_budget.rs)
      # The golden replay's vector, AT THE SAME TAG as src/ (tests/vendor_golden_replay.rs says
      # why it stopped being frozen: a vector must travel with the engine that recorded it).
      MAPPED=(testdata/seed-1.json=rust/tapstone-sim/golden/seed-1.json)
      # Test FUNCTIONS (not files) that read tapstone's own docs through `../../docs/`, which
      # smol does not carry: they check tapstone's documents against its code, not the engine,
      # and upstream runs them. The gate passes these as exact `--skip`s (`--skips <crate>`).
      SKIPPED_FNS=(the_documents_quote_the_codes_and_sizes_the_code_uses
                   the_protocol_draft_names_draw_by_its_code)
      ;;
    tapstone-proto)
      # follower.rs and frame.rs dev-depend on tapstone-sim and proptest, and frame.rs reads
      # docs/protocol/ from the tapstone tree. Vendoring them would drag the sim (and a
      # registry crate) into smol for tests smol cannot run as written.
      EXCLUDED_TESTS=(tests/follower.rs tests/frame.rs)
      ;;
    tapstone-progression) ;;
    # The shrine's 320x240 screens (embedded-graphics 0.8 and heapless 0.8, both already in
    # rust/clock's lock). Its four tests are self-contained.
    shrine-render) ;;
    # Data, not a crate: every file under upstream decks/, the station's deck tables' source.
    tapstone-decks) UP_PREFIX=""; VDIRS=(decks) ;;
    *) echo "internal: no config for $1" >&2; exit 2 ;;
  esac
}

MODE="check"; TAG=""; REV=""; FETCH=0
[[ "${CI:-}" == "true" ]] && FETCH=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --check)    MODE="check"; shift ;;
    --sync)     MODE="sync"; shift ;;
    --manifest) MODE="manifest"; shift ;;
    --tag)      TAG="${2:?--tag needs a value}"; shift 2 ;;
    --rev)      REV="${2:?--rev needs a value}"; shift 2 ;;
    --fetch)    FETCH=1; shift ;;
    --skips)    crate_cfg "${2:?--skips needs a crate}"
                for f in "${SKIPPED_FNS[@]}"; do printf -- '--skip\n%s\n' "$f"; done; exit 0 ;;
    --tapstone-dir) TAPSTONE_DIR="${2:?--tapstone-dir needs a value}"; shift 2 ;;
    -h|--help)  sed -n '2,72p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$REV" && "$MODE" != "sync" ]] && { echo "--rev is for --sync only" >&2; exit 2; }
[[ -n "$REV" && -n "$TAG" ]] && { echo "--rev and --tag are exclusive" >&2; exit 2; }

manifest_of() { echo "$REPO/rust/$1/VENDOR.sha256"; }
manifest_field() { [[ -f "$1" ]] && sed -n "s/^# $2: *//p" "$1" | head -1; }
# `sha256sum -c` cannot read comments, so the pin rides `#` lines stripped on the way in.
sums() { grep -v '^#' "$1"; }
listed_files() { sums "$1" | sed 's/^[0-9a-f]*  //'; }

have_repo() { [[ -d "$TAPSTONE_DIR" ]] && git -C "$TAPSTONE_DIR" rev-parse --git-dir >/dev/null 2>&1; }
source_ok() {
  local url
  url="$(git -C "$TAPSTONE_DIR" remote get-url origin 2>/dev/null || true)"
  [[ "$url" =~ $ORIGIN_RE ]]
}
tag_exists() { git -C "$TAPSTONE_DIR" rev-parse --verify --quiet "refs/tags/$1" >/dev/null 2>&1; }
upstream_main() {
  local r
  for r in refs/heads/main refs/remotes/origin/main; do
    git -C "$TAPSTONE_DIR" rev-parse --verify --quiet "$r" >/dev/null 2>&1 && { echo "$r"; return; }
  done
}
# The upstream path of one vendored file (relative to the crate unless MAPPED).
upjoin() { if [[ -n "$UP_PREFIX" ]]; then echo "$UP_PREFIX/$1"; else echo "$1"; fi; }
upstream_path() {
  local m
  for m in "${MAPPED[@]}"; do [[ "${m%%=*}" == "$2" ]] && { echo "${m#*=}"; return; }; done
  upjoin "$2"
}
mapped_files() { local m; for m in "${MAPPED[@]}"; do echo "${m%%=*}"; done; }
# One vendored path's bytes AT A REV, on stdout. `git show`, never `cat`: departure (1).
at_rev() { git -C "$TAPSTONE_DIR" show "$1:$(upstream_path "$2" "$3")" 2>/dev/null; }
# Every upstream file under one of VDIRS at a rev, as a path relative to rust/<name>/.
upstream_list() {
  local d pre=""
  [[ -n "$UP_PREFIX" ]] && pre="$UP_PREFIX/"
  for d in "${VDIRS[@]}"; do
    git -C "$TAPSTONE_DIR" ls-tree -r --name-only "$1" -- "$pre$d/" | sed "s|^$pre||"
  done
}
# The in-tree files of rust/<name>/ that a manifest must account for (run inside that dir).
tree_files() {
  local d
  for d in "${VDIRS[@]}" testdata; do if [[ -d $d ]]; then find "$d" -type f; fi; done | sort -u
}

fetch_upstream() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  # Bare + blobless: refs and tags only, blobs on demand for the handful of files diffed.
  if git clone --quiet --bare --filter=blob:none "$UPSTREAM_URL" "$tmp/tapstone-game.git" 2>/dev/null; then
    git -C "$tmp/tapstone-game.git" remote set-url origin "$UPSTREAM_URL"
    TAPSTONE_DIR="$tmp/tapstone-game.git"
    echo "note: fetched $UPSTREAM_URL for the tag diff"
  else
    echo "note: could not fetch $UPSTREAM_URL — tag diff will be SKIPPED" >&2
  fi
}

refuse_source() {
  echo "FATAL: $TAPSTONE_DIR is not a jphein/tapstone-game checkout" >&2
  echo "       (origin: $(git -C "$TAPSTONE_DIR" remote get-url origin 2>/dev/null || echo none))." >&2
  echo "       smol is PUBLIC and may vendor only the public snapshot. The private jphein/tapstone" >&2
  echo "       has the same paths and would diff clean — that is why this refuses, not skips." >&2
}

write_manifest() {
  local crate="$1" tag="$2" commit="$3"; shift 3
  local m; m="$(manifest_of "$crate")"
  {
    echo "# Per-file SHA-256 of the VENDORED $crate. Paths are relative to rust/$crate/."
    echo "# Generated and verified by tools/tapstone_vendor.sh; do not hand-edit, and do not edit"
    echo "# the files it names — see that script's header. This list IS the vendored file set."
    echo "# source: $UPSTREAM_URL"
    echo "# tag: $tag"
    echo "# commit: $commit"
    ( cd "$REPO/rust/$crate" && sha256sum "$@" )
  } > "$m"
  echo "  wrote rust/$crate/VENDOR.sha256 (tag $tag, commit ${commit:0:7})"
}

case "$MODE" in
  manifest)
    for crate in "${CRATES[@]}"; do
      crate_cfg "$crate"
      m="$(manifest_of "$crate")"
      t="${TAG:-$(manifest_field "$m" tag || true)}"
      c="$(manifest_field "$m" commit || true)"
      [[ -n "$t" ]] || { echo "FATAL: no tag given and none recorded in $m" >&2; exit 2; }
      mapfile -t files < <(cd "$REPO/rust/$crate" && tree_files)
      keep=()
      for f in "${files[@]}"; do [[ " ${SMOL_OWN[*]} " == *" $f "* ]] || keep+=("$f"); done
      write_manifest "$crate" "$t" "${c:-unknown}" "${keep[@]}" "${SMOL_OWN[@]}"
    done
    ;;

  sync)
    have_repo || [[ $FETCH -eq 0 ]] || fetch_upstream
    have_repo || { echo "FATAL: $TAPSTONE_DIR is not a git checkout — cannot re-vendor (try --fetch)." >&2; exit 2; }
    source_ok || { refuse_source; exit 2; }
    if [[ -n "$REV" ]]; then
      git -C "$TAPSTONE_DIR" rev-parse --verify --quiet "$REV^{commit}" >/dev/null \
        || { echo "FATAL: $REV is not a commit in $TAPSTONE_DIR" >&2; exit 2; }
      rev="$REV"; tag="$UNTAGGED"
    else
      tag="${TAG:-$(manifest_field "$(manifest_of tapstone-rules)" tag || true)}"
      [[ -n "$tag" && "$tag" != "$UNTAGGED" ]] || { echo "FATAL: pass --tag <tag>." >&2; exit 2; }
      tag_exists "$tag" || { echo "FATAL: tag '$tag' does not exist in $TAPSTONE_DIR." >&2; exit 2; }
      rev="$tag"
    fi
    commit="$(git -C "$TAPSTONE_DIR" rev-parse "$rev^{commit}")"
    for crate in "${CRATES[@]}"; do
      crate_cfg "$crate"
      dir="$REPO/rust/$crate"
      mkdir -p "$dir"
      # Remove the old vendored set first (per the OLD manifest), so a file upstream deleted
      # does not linger. SMOL_OWN files are kept.
      if [[ -f "$(manifest_of "$crate")" ]]; then
        while read -r f; do
          [[ " ${SMOL_OWN[*]} " == *" $f "* ]] || rm -f "$dir/$f"
        done < <(listed_files "$(manifest_of "$crate")")
      fi
      vendored=()
      while read -r f; do
        [[ -n "$f" ]] || continue
        [[ " ${EXCLUDED_TESTS[*]} " == *" $f "* ]] && continue
        mkdir -p "$dir/$(dirname "$f")"
        at_rev "$rev" "$crate" "$f" > "$dir/$f"
        vendored+=("$f")
      done < <(upstream_list "$rev"; mapped_files)
      [[ ${#vendored[@]} -gt 0 ]] || { echo "FATAL: rust/$crate has no files at $rev" >&2; exit 2; }
      echo "  $crate: vendored ${#vendored[@]} file(s); excluded tests: ${EXCLUDED_TESTS[*]:-none}"
      write_manifest "$crate" "$tag" "$commit" "${vendored[@]}" "${SMOL_OWN[@]}"
      if at_rev "$rev" "$crate" Cargo.toml >/dev/null; then
        echo "  $crate: upstream [dependencies] at $rev (compare with rust/$crate/Cargo.toml, smol's own):"
        at_rev "$rev" "$crate" Cargo.toml | sed -n '/^\[dependencies\]/,/^\[/p' | grep -vE '^\[|^$' | sed 's/^/      /'
      fi
    done
    echo
    if [[ "$tag" == "$UNTAGGED" ]]; then
      echo "DRY RUN from untagged ${commit:0:7}: --check will FAIL until this is re-run with --tag."
    fi
    echo "READ THE DIFF before committing: a rules change alters what every recorded match hashes to."
    echo "Then run each crate's own cargo test (tools/gate.sh does)."
    ;;

  check)
    rc=0
    for crate in "${CRATES[@]}"; do
      crate_cfg "$crate"
      m="$(manifest_of "$crate")"; dir="$REPO/rust/$crate"
      # ── Layer 1: the fingerprint. Always runnable; fails closed. ────────────────────────────
      if [[ ! -f "$m" ]]; then
        echo "FATAL: rust/$crate/VENDOR.sha256 is missing: a vendored copy with no fingerprint cannot" >&2
        echo "       be checked at all. Re-vendor: tools/tapstone_vendor.sh --sync --tag <tag>" >&2
        rc=1; continue
      fi
      t="${TAG:-$(manifest_field "$m" tag || true)}"
      if [[ -z "$t" || "$t" == "$UNTAGGED" ]]; then
        echo "FATAL: rust/$crate is pinned to '${t:-nothing}': a dry run or no tag at all." >&2
        echo "       Re-vendor from a real tapstone-game tag: --sync --tag <tag>." >&2
        rc=1; continue
      fi
      [[ -z "${PIN:-}" || "$PIN" == "$t" ]] || {
        echo "DRIFT: rust/$crate pins $t but another vendored crate pins $PIN — one engine, one tag." >&2
        rc=1; }
      PIN="$t"
      if ( cd "$dir" && sums "$m" | sha256sum --quiet -c - ); then
        echo "ok: $crate matches its committed fingerprint (tag $t)"
      else
        echo "DRIFT: rust/$crate has been EDITED IN TREE. Its vendored files are verbatim copies of" >&2
        echo "       tapstone-game at $t: change them THERE, tag, then --sync --tag <t>." >&2
        rc=1
      fi
      # ── Layer 1b: the file LIST. ────────────────────────────────────────────────────────────
      listed=$(listed_files "$m" | sort)
      actual=$(cd "$dir" && tree_files)
      if [[ "$listed" != "$actual" ]]; then
        echo "DRIFT: the file LIST under rust/$crate/ is not what its manifest declares." >&2
        diff <(echo "$listed") <(echo "$actual") | sed 's/^/       /' >&2
        echo "       '>' is a file nothing vouches for (invisible to sha256sum -c); '<' vanished." >&2
        rc=1
      fi
    done
    TAG="${PIN:-}"

    # ── Layer 2: the tag diff. ──────────────────────────────────────────────────────────────────
    have_repo || [[ $FETCH -eq 0 || -z "$TAG" ]] || fetch_upstream
    if [[ -z "$TAG" ]]; then
      :
    elif ! have_repo; then
      echo "note: no tapstone-game checkout at $TAPSTONE_DIR — tag diff SKIPPED (fingerprints still"
      echo "      verified). CI fetches it (CI=true); locally pass --fetch or clone it there."
    elif ! source_ok; then
      refuse_source; rc=1
    elif ! tag_exists "$TAG"; then
      # A FAIL, not a note: an absent checkout is an unavailable instrument, but a PRESENT one that
      # lacks the pinned tag means the pin names nothing and layer 2 can never run again.
      echo "FATAL: tag '$TAG' is pinned but does not exist in $TAPSTONE_DIR. The pin names nothing." >&2
      rc=1
    else
      for crate in "${CRATES[@]}"; do
        crate_cfg "$crate"
        m="$(manifest_of "$crate")"; dir="$REPO/rust/$crate"
        drift=0
        mapfile -t vend < <(listed_files "$m")
        for f in "${vend[@]}"; do
          [[ " ${SMOL_OWN[*]} " == *" $f "* ]] && continue
          if ! at_rev "$TAG" "$crate" "$f" | diff -q - "$dir/$f" >/dev/null 2>&1; then
            echo "DRIFT: $crate/$f differs from tapstone-game $TAG" >&2
            at_rev "$TAG" "$crate" "$f" | diff -u - "$dir/$f" | head -40 >&2
            drift=1
          fi
        done
        while read -r f; do
          [[ -n "$f" ]] || continue
          [[ " ${vend[*]} ${EXCLUDED_TESTS[*]} " == *" $f "* ]] && continue
          echo "DRIFT: $crate/$f exists upstream at $TAG but is neither vendored nor declared excluded." >&2
          drift=1
        done < <(upstream_list "$TAG")
        if [[ $drift -eq 0 ]]; then echo "ok: $crate matches tapstone-game at $TAG"; else rc=1; fi
      done
      # ── Layer 3: has upstream moved past the pin? A NOTE. ────────────────────────────────────
      main_ref="$(upstream_main || true)"
      if [[ -n "$main_ref" ]]; then
        paths=()
        for crate in "${CRATES[@]}"; do
          crate_cfg "$crate"; for d in "${VDIRS[@]}"; do paths+=("$(upjoin "$d")"); done
        done
        behind=$(git -C "$TAPSTONE_DIR" rev-list --count "$TAG..$main_ref" -- "${paths[@]}" 2>/dev/null || echo 0)
        if [[ "$behind" -gt 0 ]]; then
          echo "note: tapstone-game main is $behind commit(s) ahead of $TAG in the vendored crates' src."
          echo "      NOT a failure — a re-vendor DECISION for a human: read the diff, tag, --sync."
        else
          echo "ok: tapstone-game main has no vendored-crate commits past $TAG"
        fi
      fi
    fi
    exit "$rc"
    ;;
esac
