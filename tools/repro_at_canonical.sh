#!/usr/bin/env bash
# repro_at_canonical.sh — build a smol image at a FIXED absolute path, so the result is
# comparable regardless of where the tree actually lives. (#327)
#
# WHY THIS EXISTS
#
# The same commit, same flags, same toolchain, built from a different directory produces a
# DIFFERENT image. Measured on `d05a2ae`: two byte-identical trees at two paths gave
# 30f3ec33… (1,031,216 B) and 5f12b677… (1,031,200 B) — 273,648 differing byte positions.
#
# The mechanism is narrower than "cargo hashes the package path", and the narrowness is what
# makes it fixable: a STANDALONE crate is path-stable. The path enters through the `SourceId`
# of a PATH DEPENDENCY, which contains an absolute path, and propagates into every dependent's
# unit hash — including the root binary's. smol has two, both in `rust/clock/Cargo.toml`:
# `sigil-names` and `esp-wifi-sys-chip`. So the trigger is the dependency graph, not where the
# repo is checked out.
#
# That is also why `--remap-path-prefix` cannot help, and why `tools/repro_build.sh` already
# carrying remap flags is not a contradiction: remap rewrites embedded STRINGS, and metadata is
# a hash OVER the source id. There is no string to rewrite.
#
# WHY A SEPARATE SCRIPT AND NOT A FLAG ON verify_image.sh
#
# Making the canonical path work needs `mount --bind`, i.e. SUDO. Putting that inside
# `verify_image.sh` would change what running the standard verification costs, and a gate people
# avoid is the failure mode `tools/gate.sh` was written about (#338). CI cannot bind-mount at
# all, so an internal bind would be a code path that exists only where nobody runs it.
# `verify_image.sh` keeps its refusal exactly as it was and points here; this is opt-in.
#
# A SYMLINK DOES NOT WORK — do not "simplify" this to `ln -s`.
#
# Cargo canonicalizes, so it hashes the real path and the symlinked build is byte-identical to
# the unsymlinked one. Measured, both crates, both directions. It is the obvious first thing to
# try, it looks like it should work, and it fails SILENTLY — you get a plausible green with
# nothing actually pinned. A bind mount changes the path cargo resolves; a symlink does not.
#
# REQUIREMENTS
#   * passwordless `sudo mount --bind` (true on katana and familiar)
#   * working dirs under /var/tmp — DISK-backed. Never /tmp: katana's is a 16 GB tmpfs (RAM),
#     and a cold cargo target dir will happily eat it.
#
# EXIT CODES  (tools/ha_deploy.sh vocabulary: 4 = I stopped you, deliberately)
#   0  built; sha printed on stdout
#   1  something broke (build, mount, copy)
#   4  refused — a precondition that would have produced a MEANINGLESS sha
#
# USAGE
#   tools/repro_at_canonical.sh <tree> <out.bin> [--hash H] [--number N] [--sde EPOCH]
#   tools/repro_at_canonical.sh --self-test [--keep]
set -uo pipefail

EXIT_REFUSED=4
CANON="${SMOL_CANON_PATH:-/var/tmp/smol-canon}"
WORK="${SMOL_CANON_WORK:-/var/tmp/smol-repro-canon}"
# Pinned so the image is a function of the commit, not of the clock. Any fixed value works; it
# only has to be the SAME one on both sides of a comparison. Matches the #327 measurements.
SDE_DEFAULT=1750000000

die()    { echo "ERROR: $*" >&2; exit 1; }
refuse() { echo "REFUSED: $*" >&2; exit "$EXIT_REFUSED"; }
note()   { echo "  $*" >&2; }

# ── preconditions ─────────────────────────────────────────────────────────────────────────────
require_bind() {
  command -v mount >/dev/null || die "no mount(8)"
  sudo -n true 2>/dev/null || refuse \
"this tool needs passwordless 'sudo mount --bind' and sudo asked for a password.
    Without the bind mount the build happens at its real path and the image is NOT
    comparable — which is the whole point of this script, so it stops rather than
    producing a sha that looks authoritative and means nothing."
}

# A non-interactive shell (ssh host '...', cron, a CI step) often has no ~/.cargo/bin on PATH.
# Without it `repro_build.sh` fails deep inside sysroot resolution with "could not resolve rustc
# sysroot — cannot remap build-std paths", which reads like a toolchain bug and is really a PATH
# bug. Found by running this file's own self-test over ssh. Fix it where it is cheap, and say so.
require_toolchain() {
  if ! command -v cargo >/dev/null || ! command -v rustc >/dev/null; then
    if [ -x "$HOME/.cargo/bin/cargo" ]; then
      export PATH="$HOME/.cargo/bin:$PATH"
      note "cargo/rustc were not on PATH; using \$HOME/.cargo/bin"
    else
      refuse "cargo/rustc are not on PATH and \$HOME/.cargo/bin/cargo does not exist.
    A non-interactive shell (ssh, cron, CI) usually needs PATH set explicitly."
    fi
  fi
}

# The provisioning trap, and the reason this is a refusal rather than a convenience.
#
# `tools/ci_provision.sh` SUBSTITUTES A RANDOM KEY for the published all-zero example
# GROUP_KEY. So provisioning two trees separately gives them different `secrets.rs` — different
# SOURCE — and a comparison between them measures that, while appearing to measure the path.
# This cost a full A/B run to notice during #327. Generating silently here would rebuild the
# same trap for every future caller, so: use what the tree HAS, or say so and stop.
require_provisioned() {
  local clock="$1" provision="$2"
  local missing=()
  [ -f "$clock/src/board.rs" ]   || missing+=("board.rs")
  [ -f "$clock/src/secrets.rs" ] || missing+=("secrets.rs")
  [ ${#missing[@]} -eq 0 ] && return 0
  if [ "$provision" != 1 ]; then
    refuse \
"$clock/src is missing ${missing[*]} (git-ignored; every checkout provisions its own).
    Refusing to generate them, because ci_provision.sh substitutes a RANDOM key for the
    example GROUP_KEY — the image would build, and its sha would be unreproducible even
    from this same command. Either provision the tree yourself and re-run, or pass
    --provision to accept a random-key build (fine for a self-test, NOT for verifying a
    staged image)."
  fi
  note "--provision: generating ${missing[*]} from examples (random GROUP_KEY — sha is NOT"
  note "             comparable to a fleet image built from real provisioning)"
  # The tree's OWN provisioner, so a staged copy provisions itself by its own rules.
  local tree_root; tree_root="$(cd "$clock/../.." && pwd)"
  bash "$tree_root/tools/ci_provision.sh" "$clock" >/dev/null || die "provisioning failed"
}

# ── the actual mechanism ──────────────────────────────────────────────────────────────────────
# Copy a tree to <dst>. `.git` and `target/` are excluded on purpose: identity comes from
# --hash/--number/--sde (below), never from the copy's git state, so the copy does not need a
# repo — and carrying one would let the copy's git state influence the stamp.
stage_tree() {
  local src="$1" dst="$2"
  rm -rf "$dst"; mkdir -p "$dst"
  rsync -a --exclude '.git' --exclude 'target/' "$src/" "$dst/" || die "copy failed: $src -> $dst"
}

unmount_canon() { mountpoint -q "$CANON" 2>/dev/null && sudo umount "$CANON"; return 0; }

# Build <tree> at $CANON and write <out>. Cold target dir every time: a warm one defeats the
# comparison, and esp-bootloader-esp-idf's build script does not re-run on an epoch change
# (#326), so a reused dir can carry a previous build's stamp.
build_at_canon() {
  local tree="$1" out="$2" hash="$3" number="$4" sde="$5" tdir="$6"
  sudo mkdir -p "$CANON" || die "cannot create $CANON"
  unmount_canon
  sudo mount --bind "$tree" "$CANON" || die "bind mount failed: $tree -> $CANON"
  trap unmount_canon EXIT INT TERM

  local rc=0
  (
    cd "$CANON" || exit 1
    export CARGO_TARGET_DIR="$tdir"
    export REPRO_SDE_OVERRIDE="$sde"
    export TMPDIR="${TMPDIR:-/var/tmp/smol-canon-tmp}"
    mkdir -p "$TMPDIR"
    rm -rf "$CARGO_TARGET_DIR"
    # The COMMIT's own build recipe, read from inside the mount — not the caller's copy.
    . ./tools/repro_build.sh || exit 1
    repro_build_bin rust/clock "$out" "$hash" "$number"
  ) || rc=$?

  unmount_canon
  trap - EXIT INT TERM
  return $rc
}

# ── self-test: three paths, three images ──────────────────────────────────────────────────────
#
# The property, stated so it can fail: two DIFFERENT source directories built at the SAME
# canonical path must produce the SAME sha, and the unmounted builds of those same two trees
# must NOT. Both halves are required — a canonical build that matched everything would prove
# the rig was ignoring its input rather than that the fix works.
#
# The canonical sha should also match NEITHER unmounted build, because $CANON is a third
# distinct path. If it coincidentally equalled one, that is a reason to audit this script, not
# to celebrate.
#
# Cost: FOUR cold firmware builds. Minutes, not seconds. It is a proof, not a smoke test.
self_test() {
  local keep="$1"
  require_bind
  require_toolchain
  local base="$WORK/selftest"
  echo "── self-test: staging one provisioned tree, copying it to two paths" >&2
  rm -rf "$base"; mkdir -p "$base"

  # PROVISION ONCE, THEN COPY — the #327 confound fix, structural rather than remembered.
  stage_tree "$SRC_ROOT" "$base/staged"
  require_provisioned "$base/staged/rust/clock" 1
  stage_tree "$base/staged" "$base/A/smol"
  stage_tree "$base/staged" "$base/B/smol"

  if diff -r "$base/A/smol" "$base/B/smol" >/dev/null; then
    note "trees byte-identical at two paths — the only variable is the path"
  else
    die "self-test staging produced DIFFERING trees; the test would be meaningless"
  fi

  local h=selftest n=999
  echo "── building A and B at their REAL paths (control)" >&2
  build_at_canon_off "$base/A/smol" "$base/A.bin" "$h" "$n" "$SDE_DEFAULT" "$WORK/t-A" || die "control A failed"
  build_at_canon_off "$base/B/smol" "$base/B.bin" "$h" "$n" "$SDE_DEFAULT" "$WORK/t-B" || die "control B failed"
  echo "── building A and B at $CANON (the fix)" >&2
  build_at_canon "$base/A/smol" "$base/canon-A.bin" "$h" "$n" "$SDE_DEFAULT" "$WORK/t-cA" || die "canon A failed"
  build_at_canon "$base/B/smol" "$base/canon-B.bin" "$h" "$n" "$SDE_DEFAULT" "$WORK/t-cB" || die "canon B failed"

  local sa sb sca scb
  sa=$(sha256sum "$base/A.bin"       | cut -d' ' -f1)
  sb=$(sha256sum "$base/B.bin"       | cut -d' ' -f1)
  sca=$(sha256sum "$base/canon-A.bin" | cut -d' ' -f1)
  scb=$(sha256sum "$base/canon-B.bin" | cut -d' ' -f1)

  echo
  echo "  path A, unmounted : $sa"
  echo "  path B, unmounted : $sb"
  echo "  A at \$CANON       : $sca"
  echo "  B at \$CANON       : $scb"
  echo

  local fail=0
  if [ "$sca" = "$scb" ]; then echo "  PASS  same canonical path -> identical image"
  else echo "  FAIL  canonical builds DIFFER — the bind mount is not taking effect"; fail=1; fi
  if [ "$sa" != "$sb" ]; then echo "  PASS  different real paths -> different image (the bug is present to fix)"
  else echo "  FAIL  unmounted builds match — this toolchain is already path-stable, or the"
       echo "        control is not measuring what it claims"; fail=1; fi
  if [ "$sca" != "$sa" ] && [ "$sca" != "$sb" ]; then
       echo "  PASS  canonical sha matches neither control (\$CANON is a third distinct path)"
  else echo "  FAIL  canonical sha equals a control — audit this script before trusting it"; fail=1; fi

  [ "$keep" = 1 ] || rm -rf "$base" "$WORK/t-A" "$WORK/t-B" "$WORK/t-cA" "$WORK/t-cB"
  echo
  if [ $fail -eq 0 ]; then echo "self-test passed — a canonical-path build is path-independent"; return 0
  else echo "SELF-TEST FAILED"; return 1; fi
}

# Control arm: build in place, no mount. Same code path otherwise, so the two arms differ only
# in the bind.
build_at_canon_off() {
  local tree="$1" out="$2" hash="$3" number="$4" sde="$5" tdir="$6"
  ( cd "$tree" || exit 1
    export CARGO_TARGET_DIR="$tdir" REPRO_SDE_OVERRIDE="$sde"
    export TMPDIR="${TMPDIR:-/var/tmp/smol-canon-tmp}"; mkdir -p "$TMPDIR"
    rm -rf "$CARGO_TARGET_DIR"
    . ./tools/repro_build.sh || exit 1
    repro_build_bin rust/clock "$out" "$hash" "$number" )
}

# ── argument handling ─────────────────────────────────────────────────────────────────────────
SRC_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE=build; TREE=""; OUT=""; HASH=""; NUMBER=""; SDE="$SDE_DEFAULT"; PROVISION=0; KEEP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --self-test) MODE=selftest; shift ;;
    --keep)      KEEP=1; shift ;;
    --provision) PROVISION=1; shift ;;
    --hash)      HASH="${2:-}"; shift 2 ;;
    --number)    NUMBER="${2:-}"; shift 2 ;;
    --sde)       SDE="${2:-}"; shift 2 ;;
    -h|--help)   sed -n '2,52p' "$0"; exit 0 ;;
    -*)          die "unknown option: $1" ;;
    *)           if [ -z "$TREE" ]; then TREE="$1"; elif [ -z "$OUT" ]; then OUT="$1";
                 else die "unexpected argument: $1"; fi; shift ;;
  esac
done

if [ "$MODE" = selftest ]; then
  self_test "$KEEP"; exit $?
fi

[ -n "$TREE" ] && [ -n "$OUT" ] || { sed -n '2,52p' "$0"; exit 2; }
TREE="$(cd "$TREE" 2>/dev/null && pwd)" || die "no such tree: $TREE"
[ -f "$TREE/tools/repro_build.sh" ] || die "$TREE does not look like a smol checkout"
require_bind
require_toolchain
require_provisioned "$TREE/rust/clock" "$PROVISION"

# Identity defaults: the tree's own git HEAD when it has one, else explicit flags are required —
# an image labelled with a hash it was not built from is #326 cause D with extra steps.
if [ -z "$HASH" ]; then
  HASH="$(git -C "$TREE" rev-parse --short=7 HEAD 2>/dev/null || true)"
  [ -n "$HASH" ] || refuse "no --hash given and $TREE has no git HEAD to take one from."
fi
[ -n "$NUMBER" ] || NUMBER="$(tr -d '[:space:]' < "$TREE/rust/clock/version.txt" 2>/dev/null || echo 0)"

# Stage a COPY and build that, never the caller's tree: the bind mount would otherwise shadow
# the user's own directory for the duration, and a build interrupted mid-flight could leave it
# mounted over.
mkdir -p "$WORK"
STAGE="$WORK/tree"
stage_tree "$TREE" "$STAGE"

echo "── building $HASH (build $NUMBER) at $CANON" >&2
build_at_canon "$STAGE" "$OUT" "$HASH" "$NUMBER" "$SDE" "$WORK/target" || die "build failed"
sha256sum "$OUT"
